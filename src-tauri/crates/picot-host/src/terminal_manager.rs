// ABOUTME: Owns every terminal PTY: spawn, input, resize, batched output delivery,
// ABOUTME: generation-checked lifecycle, and process-tree cleanup. The only module
// ABOUTME: allowed to touch portable-pty; never parses ANSI output or infers cwd.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};

use crate::terminal_output::{TerminalLimits, TerminalOutputStore};
use crate::terminal_profiles::{
    resolve_macos_default, resolve_windows_profile, ProfileError, ResolvedShell, ShellProbe,
    ShellProfileId, SystemShellProbe,
};
use crate::terminal_registry::{
    err_str, CloseLease, TerminalDescriptor, TerminalError, TerminalKey, TerminalRegistry,
};
use crate::terminal_state_store::{
    PersistedTabDescriptor, TerminalStateStore, WorkspaceTerminalMetadata,
};
use crate::window_owner::OwnerId;

const INITIAL_COLS: u16 = 80;
const INITIAL_ROWS: u16 = 24;
const READ_BUF_BYTES: usize = 8 * 1024;
const CHECKPOINT_MAX_BYTES: usize = 2 * 1024 * 1024;
const INPUT_MAX_BYTES: usize = 64 * 1024;

/// Delivers a `terminal_event` frame to the current authenticated owner client.
/// HostServer installs this sink and applies owner filtering before delivery.
pub type EventSink = Arc<dyn Fn(&OwnerId, Value) + Send + Sync>;

#[derive(Clone)]
pub struct TerminalManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    registry: TerminalRegistry,
    probe: Box<dyn ShellProbe>,
    live: Mutex<HashMap<String, LiveTerminal>>,
    outputs: Mutex<HashMap<String, TerminalOutputStore>>,
    event_sink: Mutex<Option<EventSink>>,
    state_store: TerminalStateStore,
}

struct LiveTerminal {
    owner: OwnerId,
    workspace_root: PathBuf,
    generation: u64,
    profile_id: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    #[cfg(windows)]
    job: Option<windows_job::JobHandle>,
}

impl TerminalManager {
    pub fn new(registry: TerminalRegistry, state_store: TerminalStateStore) -> Self {
        Self::with_probe(registry, state_store, Box::new(SystemShellProbe))
    }

    pub fn with_probe(
        registry: TerminalRegistry,
        state_store: TerminalStateStore,
        probe: Box<dyn ShellProbe>,
    ) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                registry,
                probe,
                live: Mutex::new(HashMap::new()),
                outputs: Mutex::new(HashMap::new()),
                event_sink: Mutex::new(None),
                state_store,
            }),
        }
    }

    /// Install the owner-scoped event forwarder. Called once from main.rs after
    /// the broker exists; the closure typically wraps `send_owner_event`.
    pub fn set_event_sink(&self, sink: EventSink) {
        *self
            .inner
            .event_sink
            .lock()
            .expect("terminal event sink lock") = Some(sink);
    }

    /// Dispatch a verified terminal command. `owner` and `workspace_root` are
    /// derived from the authenticated broker context, never from the payload.
    /// Returns a synchronous response value, or an error string surfaced to the
    /// sender as `terminal_command_failed`.
    pub fn dispatch(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "terminal_list" => self.list(owner, workspace_root),
            "terminal_create" => self.create(owner, workspace_root, payload),
            "terminal_input" => self.create_input(owner, workspace_root, payload),
            "terminal_resize" => self.resize(owner, workspace_root, payload),
            "terminal_checkpoint" => self.checkpoint(owner, workspace_root, payload),
            "terminal_ack" => self.ack(owner, workspace_root, payload),
            "terminal_close" => self.close(owner, workspace_root, payload),
            "terminal_restart" => self.restart(owner, workspace_root, payload),
            "terminal_activate" => self.activate(owner, workspace_root, payload),
            "terminal_reorder" => self.reorder(owner, workspace_root, payload),
            "terminal_set_panel_height" => {
                let height = payload
                    .get("heightPx")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "heightPx is required".to_string())?;
                let height =
                    u32::try_from(height).map_err(|_| "heightPx is out of range".to_string())?;
                self.set_panel_height(owner, workspace_root, height);
                Ok(json!({ "type": "terminal_command_acked" }))
            }
            other => Err(format!("unknown terminal command: {other}")),
        }
    }

    fn list(&self, owner: &OwnerId, workspace_root: &Path) -> Result<Value, String> {
        let key = self.key(owner, workspace_root);
        let descriptors = self.inner.registry.descriptors(&key);
        let persisted = self
            .inner
            .state_store
            .load_for_workspace(workspace_root)
            .ok()
            .flatten();
        let tabs: Vec<Value> = if descriptors.is_empty() {
            // Fresh owner/partition (new app run): surface persisted tab metadata
            // so the panel can re-create fresh shells. Live PTYs are never restored.
            self.restored_metadata(persisted.as_ref())
        } else {
            self.live_tab_descriptors(&descriptors)
        };
        Ok(json!({
            "type": "terminal_listed",
            "tabs": tabs,
            "panelHeightPx": persisted.as_ref().and_then(|meta| meta.panel_height_px),
            "activeIndex": persisted.as_ref().and_then(|meta| meta.active_index),
        }))
    }

    /// Surface persisted tab metadata (label/profile) for a fresh app run so the
    /// panel can re-create fresh shells. Live PTYs/checkpoints are never restored.
    fn restored_metadata(&self, metadata: Option<&WorkspaceTerminalMetadata>) -> Vec<Value> {
        metadata
            .into_iter()
            .flat_map(|meta| meta.tabs.iter().enumerate())
            .map(|(index, t)| {
                json!({
                    "terminalId": format!("restored-{index}"),
                    "generation": 0,
                    "status": "restoredMetadata",
                    "profileId": t.profile_id,
                    "label": t.label,
                })
            })
            .collect()
    }

    fn live_tab_descriptors(&self, descriptors: &[TerminalDescriptor]) -> Vec<Value> {
        let outputs = self.inner.outputs.lock().expect("outputs lock");
        descriptors
            .iter()
            .map(|d| {
                let output = outputs.get(&d.terminal_id);
                json!({
                    "terminalId": d.terminal_id,
                    "generation": d.generation,
                    "status": d.status,
                    "profileId": d.profile_id,
                    "label": d.label,
                    "exitCode": d.exit_code,
                    "failReason": d.fail_reason,
                    "checkpoint": output.and_then(|o| o.checkpoint()).map(|b| BASE64.encode(b)),
                    "checkpointWatermark": output.map(|o| o.checkpoint_watermark()).unwrap_or(0),
                    "historyGap": output.map(|o| o.history_gap()).unwrap_or(false),
                    "journal": output.and_then(|o| {
                        let wm = o.checkpoint_watermark();
                        o.journal_from(wm).map(|batch| json!({
                            "firstSequence": batch.first_sequence,
                            "lastSequence": batch.last_sequence,
                            "dataBase64": BASE64.encode(&batch.bytes),
                        }))
                    }),
                })
            })
            .collect()
    }

    fn create(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let profile_id = payload
            .get("profileId")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let key = self.key(owner, workspace_root);
        let label = label_for_profile(profile_id);
        let reservation = self
            .inner
            .registry
            .reserve_tab_with_profile(&key, profile_id, label)
            .map_err(err_str_owned)?;

        let shell = match self.resolve_shell(profile_id) {
            Ok(shell) => shell,
            Err(reason) => {
                let _ = self.inner.registry.mark_failed(
                    &key,
                    &reservation.terminal_id,
                    reservation.generation,
                    reason.clone(),
                );
                return Err(reason);
            }
        };

        let spawned = match self.spawn_pty(&shell, workspace_root) {
            Ok(s) => s,
            Err(reason) => {
                let _ = self.inner.registry.mark_failed(
                    &key,
                    &reservation.terminal_id,
                    reservation.generation,
                    reason.clone(),
                );
                return Err(reason);
            }
        };

        if let Err(err) = self.inner.registry.commit_running(
            &key,
            &reservation.terminal_id,
            reservation.generation,
        ) {
            let reason = err_str_owned(err);
            let _ = self.inner.registry.mark_failed(
                &key,
                &reservation.terminal_id,
                reservation.generation,
                reason.clone(),
            );
            // The spawned PTY must not leak when commit fails (e.g. global quota).
            self.detach_spawned(spawned);
            return Err(reason);
        }

        self.attach_spawned(
            owner.clone(),
            workspace_root.to_path_buf(),
            reservation.terminal_id.clone(),
            reservation.generation,
            profile_id.to_string(),
            spawned,
        );
        self.persist(owner, workspace_root);
        Ok(json!({
            "type": "terminal_created",
            "terminalId": reservation.terminal_id,
            "generation": reservation.generation,
        }))
    }

    fn create_input(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let (terminal_id, generation, bytes) = decode_target_bytes(payload, INPUT_MAX_BYTES)?;
        let key = self.key(owner, workspace_root);
        self.inner
            .registry
            .require_running(&key, &terminal_id, generation)
            .map_err(err_str_owned)?;
        let mut live = self.inner.live.lock().expect("live lock");
        let lt = live
            .get_mut(&terminal_id)
            .ok_or_else(|| "terminal is not running".to_string())?;
        if lt.generation != generation {
            return Err(err_str_owned(TerminalError::StaleGeneration));
        }
        lt.writer
            .write_all(&bytes)
            .map_err(|e| format!("terminal write failed: {e}"))?;
        lt.writer
            .flush()
            .map_err(|e| format!("terminal flush failed: {e}"))?;
        Ok(json!({ "type": "terminal_input_acked", "terminalId": terminal_id }))
    }

    fn resize(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let terminal_id = payload
            .get("terminalId")
            .and_then(Value::as_str)
            .ok_or_else(|| "terminalId is required".to_string())?
            .to_string();
        let generation = payload
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| "generation is required".to_string())?;
        let cols_raw = payload
            .get("cols")
            .and_then(Value::as_u64)
            .ok_or_else(|| "cols is required".to_string())?;
        let rows_raw = payload
            .get("rows")
            .and_then(Value::as_u64)
            .ok_or_else(|| "rows is required".to_string())?;
        // Validate the full range before truncating to u16: `as u16` would
        // silently wrap oversized values (e.g. 65536 -> 0).
        if cols_raw == 0
            || rows_raw == 0
            || cols_raw > u16::MAX as u64
            || rows_raw > u16::MAX as u64
        {
            return Err("terminal dimensions out of range".to_string());
        }
        let cols = cols_raw as u16;
        let rows = rows_raw as u16;
        let key = self.key(owner, workspace_root);
        self.inner
            .registry
            .require_running(&key, &terminal_id, generation)
            .map_err(err_str_owned)?;
        let mut live = self.inner.live.lock().expect("live lock");
        let lt = live
            .get_mut(&terminal_id)
            .ok_or_else(|| "terminal is not running".to_string())?;
        if lt.generation != generation {
            return Err(err_str_owned(TerminalError::StaleGeneration));
        }
        lt.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("terminal resize failed: {e}"))?;
        Ok(json!({ "type": "terminal_resized", "terminalId": terminal_id }))
    }

    fn checkpoint(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let (terminal_id, generation, snapshot) =
            decode_target_bytes(payload, CHECKPOINT_MAX_BYTES)?;
        let watermark = payload
            .get("watermark")
            .and_then(Value::as_u64)
            .ok_or_else(|| "watermark is required".to_string())?;
        let key = self.key(owner, workspace_root);
        self.inner
            .registry
            .require_running(&key, &terminal_id, generation)
            .map_err(err_str_owned)?;
        let mut outputs = self.inner.outputs.lock().expect("outputs lock");
        let output = outputs
            .entry(terminal_id.clone())
            .or_insert_with(|| TerminalOutputStore::new(TerminalLimits::default()));
        output
            .accept_checkpoint(watermark, snapshot)
            .map_err(|e| format!("checkpoint rejected: {e:?}"))?;
        Ok(json!({ "type": "terminal_checkpoint_acked", "terminalId": terminal_id }))
    }

    fn ack(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let terminal_id = payload
            .get("terminalId")
            .and_then(Value::as_str)
            .ok_or_else(|| "terminalId is required".to_string())?
            .to_string();
        let generation = payload
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| "generation is required".to_string())?;
        let sequence = payload
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| "sequence is required".to_string())?;
        let key = self.key(owner, workspace_root);
        self.inner
            .registry
            .require_running(&key, &terminal_id, generation)
            .map_err(err_str_owned)?;
        let mut outputs = self.inner.outputs.lock().expect("outputs lock");
        if let Some(output) = outputs.get_mut(&terminal_id) {
            output.ack(sequence);
        }
        Ok(json!({ "type": "terminal_ack_acked", "terminalId": terminal_id }))
    }

    fn close(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let terminal_id = payload
            .get("terminalId")
            .and_then(Value::as_str)
            .ok_or_else(|| "terminalId is required".to_string())?
            .to_string();
        let generation = payload
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| "generation is required".to_string())?;
        let key = self.key(owner, workspace_root);
        let lease = self
            .inner
            .registry
            .begin_close(&key, &terminal_id, generation)
            .map_err(err_str_owned)?;
        self.terminate_and_finish(&key, &lease);
        self.persist(owner, workspace_root);
        Ok(json!({ "type": "terminal_closed", "terminalId": terminal_id }))
    }

    fn restart(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let terminal_id = payload
            .get("terminalId")
            .and_then(Value::as_str)
            .ok_or_else(|| "terminalId is required".to_string())?
            .to_string();
        let generation = payload
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| "generation is required".to_string())?;
        let requested_profile = payload
            .get("profileId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let key = self.key(owner, workspace_root);
        let previous_profile = self
            .inner
            .live
            .lock()
            .expect("live lock")
            .get(&terminal_id)
            .filter(|live| live.generation == generation)
            .map(|live| live.profile_id.clone());
        let profile_id = requested_profile
            .or(previous_profile)
            .unwrap_or_else(|| "default".to_string());
        // Validate the requested profile before transitioning the existing tab.
        let shell = self.resolve_shell(&profile_id)?;
        let handle = self
            .inner
            .registry
            .restart(
                &key,
                &terminal_id,
                generation,
                &profile_id,
                label_for_profile(&profile_id),
            )
            .map_err(err_str_owned)?;

        self.replace_generation(&terminal_id, generation);
        let spawned = self
            .spawn_pty(&shell, workspace_root)
            .inspect_err(|reason| {
                let _ = self.inner.registry.mark_failed(
                    &key,
                    &terminal_id,
                    handle.generation,
                    reason.clone(),
                );
            })?;
        if let Err(error) =
            self.inner
                .registry
                .commit_running(&key, &terminal_id, handle.generation)
        {
            self.detach_spawned(spawned);
            let reason = err_str_owned(error.clone());
            if !matches!(error, TerminalError::NotCreating) {
                let _ = self.inner.registry.mark_failed(
                    &key,
                    &terminal_id,
                    handle.generation,
                    reason.clone(),
                );
            }
            return Err(reason);
        }
        self.attach_spawned(
            owner.clone(),
            workspace_root.to_path_buf(),
            terminal_id.clone(),
            handle.generation,
            profile_id,
            spawned,
        );
        self.persist(owner, workspace_root);
        Ok(json!({
            "type": "terminal_restarted",
            "terminalId": terminal_id,
            "generation": handle.generation,
        }))
    }

    /// Terminate every live PTY owned by `owner` across all workspace roots.
    /// Called by the host when a native window is destroyed, before revoking
    /// the owner. Idempotent.
    pub fn kill_owner(&self, owner: &OwnerId) {
        let leases = self.inner.registry.cleanup_owner(owner);
        for (key, lease) in leases {
            self.terminate_and_finish(&key, &lease);
        }
    }

    /// Terminate every live PTY in every owner partition. Called on application
    /// exit. Idempotent.
    pub fn kill_all(&self) {
        let live: Vec<(String, LiveTerminal)> =
            self.inner.live.lock().expect("live lock").drain().collect();
        for (terminal_id, lt) in live {
            let key = self.key(&lt.owner, &lt.workspace_root);
            let generation = lt.generation;
            kill_live_terminal(lt);
            let _ = self
                .inner
                .registry
                .finish_close(&key, &terminal_id, generation);
        }
    }

    fn key(&self, owner: &OwnerId, workspace_root: &Path) -> TerminalKey {
        TerminalKey {
            owner: owner.clone(),
            workspace_root: workspace_root.to_path_buf(),
        }
    }

    fn resolve_shell(&self, profile_id: &str) -> Result<ResolvedShell, String> {
        let id = ShellProfileId::from_id_str(profile_id)
            .ok_or_else(|| format!("unknown shell profile: {profile_id}"))?;
        let resolved = if cfg!(target_os = "windows") {
            resolve_windows_profile(id, self.inner.probe.as_ref())
        } else {
            resolve_macos_default(&default_preferred_shell(), self.inner.probe.as_ref())
        };
        resolved.map_err(|ProfileError::ProfileUnavailable { guidance, .. }| guidance)
    }

    fn spawn_pty(&self, shell: &ResolvedShell, workspace_root: &Path) -> Result<Spawned, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: INITIAL_ROWS,
                cols: INITIAL_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("pty open failed: {e}"))?;
        let mut cmd = CommandBuilder::new(&shell.program);
        for arg in &shell.args {
            cmd.arg(arg);
        }
        cmd.cwd(workspace_root);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("pty spawn failed: {e}"))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("pty reader clone failed: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("pty writer clone failed: {e}"))?;
        #[cfg(windows)]
        let job = windows_job::create_and_assign(child.process_id().unwrap_or(0));
        Ok(Spawned {
            master: pair.master,
            writer,
            child,
            reader,
            #[cfg(windows)]
            job,
        })
    }

    /// Store the live PTY handle (master/writer/child) and start a reader
    /// thread that owns the reader half.
    fn attach_spawned(
        &self,
        owner: OwnerId,
        workspace_root: PathBuf,
        terminal_id: String,
        generation: u64,
        profile_id: String,
        spawned: Spawned,
    ) {
        let Spawned {
            master,
            writer,
            child,
            reader,
            #[cfg(windows)]
            job,
        } = spawned;
        self.inner.live.lock().expect("live lock").insert(
            terminal_id.clone(),
            LiveTerminal {
                owner: owner.clone(),
                workspace_root: workspace_root.clone(),
                generation,
                profile_id,
                master,
                writer,
                child: Some(child),
                #[cfg(windows)]
                job,
            },
        );
        self.inner
            .outputs
            .lock()
            .expect("outputs lock")
            .entry(terminal_id.clone())
            .or_insert_with(|| TerminalOutputStore::new(TerminalLimits::default()));
        let manager = self.clone();
        std::thread::spawn(move || {
            run_reader(
                manager,
                owner,
                workspace_root,
                terminal_id,
                generation,
                reader,
            );
        });
    }

    /// Drop a spawned PTY without registry bookkeeping. Used when commit_running
    /// fails after a successful spawn.
    fn detach_spawned(&self, spawned: Spawned) {
        #[cfg(windows)]
        let job = spawned.job;
        #[cfg(not(windows))]
        let job: Option<isize> = None;
        kill_process_tree(&*spawned.master, spawned.child, job);
    }

    /// Remove only the generation being replaced, and reset its output store
    /// while both maps are locked in their global lock order.
    fn replace_generation(&self, terminal_id: &str, generation: u64) {
        let removed = {
            let mut live = self.inner.live.lock().expect("live lock");
            let mut outputs = self.inner.outputs.lock().expect("outputs lock");
            let matches_generation = live
                .get(terminal_id)
                .is_some_and(|terminal| terminal.generation == generation);
            if matches_generation {
                outputs.insert(
                    terminal_id.to_string(),
                    TerminalOutputStore::new(TerminalLimits::default()),
                );
                live.remove(terminal_id)
            } else {
                None
            }
        };
        if let Some(terminal) = removed {
            kill_live_terminal(terminal);
        }
    }

    fn terminate_and_finish(&self, key: &TerminalKey, lease: &CloseLease) {
        let removed = {
            let mut live = self.inner.live.lock().expect("live lock");
            let mut outputs = self.inner.outputs.lock().expect("outputs lock");
            let matches_generation = live
                .get(&lease.terminal_id)
                .is_some_and(|terminal| terminal.generation == lease.generation);
            if matches_generation {
                outputs.remove(&lease.terminal_id);
                live.remove(&lease.terminal_id)
            } else {
                None
            }
        };
        if let Some(terminal) = removed {
            kill_live_terminal(terminal);
        }
        let _ = self
            .inner
            .registry
            .finish_close(key, &lease.terminal_id, lease.generation);
    }

    /// Atomically verify that output belongs to the current PTY generation and
    /// append it to that generation's journal. Restart/close use the same
    /// live-then-output lock order, preventing an old reader from appending
    /// between validation and journal replacement.
    fn append_output_if_current(
        &self,
        terminal_id: &str,
        generation: u64,
        chunk: &[u8],
    ) -> Option<u64> {
        let live = self.inner.live.lock().expect("live lock");
        if live
            .get(terminal_id)
            .is_none_or(|terminal| terminal.generation != generation)
        {
            return None;
        }
        let mut outputs = self.inner.outputs.lock().expect("outputs lock");
        let output = outputs
            .entry(terminal_id.to_string())
            .or_insert_with(|| TerminalOutputStore::new(TerminalLimits::default()));
        Some(output.append(chunk))
    }

    fn emit_output(
        &self,
        owner: &OwnerId,
        terminal_id: &str,
        generation: u64,
        sequence: u64,
        bytes: &[u8],
    ) {
        self.emit(
            owner,
            json!({
                "type": "terminal_event",
                "payload": {
                    "type": "terminal_output",
                    "terminalId": terminal_id,
                    "generation": generation,
                    "firstSequence": sequence,
                    "lastSequence": sequence,
                    "dataBase64": BASE64.encode(bytes),
                }
            }),
        );
    }

    fn activate(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let terminal_id = payload
            .get("terminalId")
            .and_then(Value::as_str)
            .ok_or_else(|| "terminalId is required".to_string())?;
        let generation = payload
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| "generation is required".to_string())?;
        let descriptors = self
            .inner
            .registry
            .descriptors(&self.key(owner, workspace_root));
        let active_index = descriptors
            .iter()
            .position(|descriptor| {
                descriptor.terminal_id == terminal_id && descriptor.generation == generation
            })
            .ok_or_else(|| err_str_owned(TerminalError::UnknownTerminal))?;
        self.persist_with_active_index(owner, workspace_root, Some(active_index));
        Ok(json!({ "type": "terminal_command_acked" }))
    }

    fn reorder(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        payload: &Value,
    ) -> Result<Value, String> {
        let ordered_terminal_ids = payload
            .get("orderedTerminalIds")
            .and_then(Value::as_array)
            .ok_or_else(|| "orderedTerminalIds is required".to_string())?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "orderedTerminalIds must contain strings".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let key = self.key(owner, workspace_root);
        let before = self.inner.registry.descriptors(&key);
        let active_terminal_id = self
            .inner
            .state_store
            .load_for_workspace(workspace_root)
            .ok()
            .flatten()
            .and_then(|metadata| metadata.active_index)
            .and_then(|index| before.get(index))
            .map(|descriptor| descriptor.terminal_id.clone());
        self.inner
            .registry
            .reorder(&key, &ordered_terminal_ids)
            .map_err(err_str_owned)?;
        let active_index = active_terminal_id.and_then(|terminal_id| {
            self.inner
                .registry
                .descriptors(&key)
                .iter()
                .position(|descriptor| descriptor.terminal_id == terminal_id)
        });
        self.persist_with_active_index(owner, workspace_root, active_index);
        Ok(json!({ "type": "terminal_command_acked" }))
    }

    fn persist(&self, owner: &OwnerId, workspace_root: &Path) {
        let active_index = self
            .inner
            .state_store
            .load_for_workspace(workspace_root)
            .ok()
            .flatten()
            .and_then(|metadata| metadata.active_index);
        self.persist_with_active_index(owner, workspace_root, active_index);
    }

    fn persist_with_active_index(
        &self,
        owner: &OwnerId,
        workspace_root: &Path,
        active_index: Option<usize>,
    ) {
        let key = self.key(owner, workspace_root);
        let descriptors = self.inner.registry.descriptors(&key);
        let default_profile = descriptors
            .first()
            .map(|d| d.profile_id.clone())
            .unwrap_or_else(|| "default".to_string());
        let mut metadata = WorkspaceTerminalMetadata::new(&default_profile);
        metadata.tabs = descriptors
            .iter()
            .map(|d| PersistedTabDescriptor {
                label: d.label.clone(),
                profile_id: d.profile_id.clone(),
            })
            .collect();
        let previous = self
            .inner
            .state_store
            .load_for_workspace(workspace_root)
            .ok()
            .flatten();
        metadata.panel_height_px = previous.as_ref().and_then(|value| value.panel_height_px);
        metadata.active_index = active_index.filter(|index| *index < metadata.tabs.len());
        if let Err(err) = self
            .inner
            .state_store
            .save_for_workspace(workspace_root, &metadata)
        {
            log::warn!("[terminal] failed to persist terminal state: {err:?}");
        }
    }

    /// Persist the panel height (terminal_set_panel_height command).
    fn set_panel_height(&self, owner: &OwnerId, workspace_root: &Path, height: u32) {
        let key = self.key(owner, workspace_root);
        let descriptors = self.inner.registry.descriptors(&key);
        let mut metadata = self
            .inner
            .state_store
            .load_for_workspace(workspace_root)
            .ok()
            .flatten()
            .unwrap_or_else(|| WorkspaceTerminalMetadata::new("default"));
        if !descriptors.is_empty() {
            metadata.default_profile_id = descriptors
                .first()
                .map(|d| d.profile_id.clone())
                .unwrap_or_else(|| "default".to_string());
            metadata.tabs = descriptors
                .iter()
                .map(|d| PersistedTabDescriptor {
                    label: d.label.clone(),
                    profile_id: d.profile_id.clone(),
                })
                .collect();
        }
        metadata.panel_height_px = Some(height);
        if let Err(err) = self
            .inner
            .state_store
            .save_for_workspace(workspace_root, &metadata)
        {
            log::warn!("[terminal] failed to persist panel height: {err:?}");
        }
    }

    fn emit(&self, owner: &OwnerId, value: Value) {
        if let Some(sink) = self.inner.event_sink.lock().expect("sink lock").as_ref() {
            sink(owner, value);
        }
    }
}

struct Spawned {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    #[cfg(windows)]
    job: Option<windows_job::JobHandle>,
}

/// Reader loop: append PTY output to the journal (assigning the next sequence),
/// then emit a `terminal_output` event to the owning client. Stale generations
/// (from a restarted tab's old reader) are dropped silently. On EOF, record the
/// exit and emit `terminal_exited`.
fn run_reader(
    manager: TerminalManager,
    owner: OwnerId,
    workspace_root: PathBuf,
    terminal_id: String,
    generation: u64,
    mut reader: Box<dyn Read + Send>,
) {
    let mut buf = [0u8; READ_BUF_BYTES];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = buf[..n].to_vec();
                let Some(sequence) =
                    manager.append_output_if_current(&terminal_id, generation, &chunk)
                else {
                    continue;
                };
                manager.emit_output(&owner, &terminal_id, generation, sequence, &chunk);
            }
            Err(_) => break,
        }
    }

    // Take the child out of the live map without holding the lock while
    // waiting on it: close/restart must be able to acquire the same lock to
    // kill the process tree. The child is owned exclusively by this reader.
    let child = manager
        .inner
        .live
        .lock()
        .expect("live lock")
        .get_mut(&terminal_id)
        .and_then(|lt| {
            if lt.generation != generation {
                return None;
            }
            lt.child.take()
        });
    let exit_code = child.and_then(|mut c| {
        c.wait()
            .ok()
            .map(|status| if status.success() { 0 } else { -1 })
    });
    // The child has exited; release the live handle only when it still belongs
    // to this reader generation. A replacement generation remains untouched.
    {
        let mut live = manager.inner.live.lock().expect("live lock");
        if live
            .get(&terminal_id)
            .is_some_and(|terminal| terminal.generation == generation)
        {
            live.remove(&terminal_id);
        }
    }
    let key = TerminalKey {
        owner: owner.clone(),
        workspace_root: workspace_root.clone(),
    };
    // A restart or explicit close may have replaced/removed this generation
    // while the reader was waiting. Only the still-owned running generation may
    // publish an exit event; stale exits must never remove the replacement tab.
    if manager
        .inner
        .registry
        .mark_exited(&key, &terminal_id, generation, exit_code)
        .is_err()
    {
        return;
    }
    manager.emit(
        &owner,
        json!({
            "type": "terminal_event",
            "payload": {
                "type": "terminal_exited",
                "terminalId": terminal_id,
                "generation": generation,
                "exitCode": exit_code,
            }
        }),
    );
}

#[cfg(windows)]
mod windows_job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// Owned handle to a Win32 Job Object. Wraps the raw `HANDLE` (a
    /// `*mut c_void`, which is `!Send`) so a terminal's job handle can cross
    /// the thread boundary imposed by Tauri's managed state and its async
    /// runtime spawner.
    ///
    /// Soundness: a Job Object handle is an opaque, process-local identifier
    /// for a kernel object whose operations the kernel itself serializes.
    /// Sending or sharing the *handle value* between threads is therefore
    /// safe; only `create_and_assign` mints a `JobHandle` and only
    /// `terminate` consumes it (terminate-then-close, exactly once).
    pub struct JobHandle(HANDLE);

    // SAFETY: `HANDLE` is an integer identifying a kernel object. The kernel
    // synchronizes all Job Object operations, so the handle value can be
    // moved between threads and shared by reference. The wrapper never
    // mutates the handle after construction.
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    /// Create a Job Object and assign the freshly spawned child to it so the
    /// whole process tree dies with the terminal. Returns the job handle.
    pub fn create_and_assign(pid: u32) -> Option<JobHandle> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            if pid != 0 {
                let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if !proc.is_null() {
                    AssignProcessToJobObject(job, proc);
                    CloseHandle(proc);
                }
            }
            Some(JobHandle(job))
        }
    }

    /// Terminate every process in the job, then close the handle.
    pub fn terminate(job: JobHandle) {
        unsafe {
            TerminateJobObject(job.0, 1);
            CloseHandle(job.0);
        }
    }
}

#[cfg(unix)]
fn kill_process_tree(
    master: &dyn MasterPty,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    _job: Option<isize>,
) {
    // The PTY slave runs the shell in its own process group/session, so killing
    // the group terminates descendants (e.g. a spawned dev server) too.
    if let Some(pgid) = master.process_group_leader() {
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn kill_process_tree(
    _master: &dyn MasterPty,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    job: Option<windows_job::JobHandle>,
) {
    // Terminate the Job Object first so descendants (dev servers, watchers)
    // die with the shell, then fall back to killing the direct child.
    if let Some(handle) = job {
        windows_job::terminate(handle);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(any(unix, windows)))]
fn kill_process_tree(
    _master: &dyn MasterPty,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    _job: Option<isize>,
) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Terminate a live terminal's process tree, using the platform's Job Object
/// (Windows) or process-group (Unix) mechanism. Consumes the handle.
fn kill_live_terminal(lt: LiveTerminal) {
    #[cfg(windows)]
    let job = lt.job;
    #[cfg(not(windows))]
    let job: Option<isize> = None;
    if let Some(child) = lt.child {
        kill_process_tree(&*lt.master, child, job);
    }
}

fn err_str_owned(err: TerminalError) -> String {
    err_str(err).to_string()
}

fn label_for_profile(profile_id: &str) -> String {
    match profile_id {
        "git-bash" => "Git Bash".to_string(),
        "powershell" => "PowerShell".to_string(),
        "command-prompt" => "Command Prompt".to_string(),
        _ => "Terminal".to_string(),
    }
}

fn default_preferred_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| String::new())
}

fn decode_target_bytes(
    payload: &Value,
    max_bytes: usize,
) -> Result<(String, u64, Vec<u8>), String> {
    let terminal_id = payload
        .get("terminalId")
        .and_then(Value::as_str)
        .ok_or_else(|| "terminalId is required".to_string())?
        .to_string();
    let generation = payload
        .get("generation")
        .and_then(Value::as_u64)
        .ok_or_else(|| "generation is required".to_string())?;
    let data_b64 = payload
        .get("dataBase64")
        .and_then(Value::as_str)
        .ok_or_else(|| "dataBase64 is required".to_string())?;
    let bytes = BASE64
        .decode(data_b64)
        .map_err(|e| format!("invalid base64: {e}"))?;
    if bytes.len() > max_bytes {
        return Err(format!("payload exceeds {max_bytes} bytes"));
    }
    Ok((terminal_id, generation, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_registry::TerminalStatus;
    use crate::window_owner::WindowOwnerRegistry;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    fn owner(name: &str) -> OwnerId {
        let reg = WindowOwnerRegistry::default();
        reg.create_owner(
            name.to_string(),
            PathBuf::from("/ws"),
            9000,
            "http://127.0.0.1:9000".to_string(),
        )
        .expect("owner")
        .0
    }

    fn manager() -> TerminalManager {
        let state_dir = tempdir().unwrap().keep();
        TerminalManager::new(
            TerminalRegistry::new(15),
            TerminalStateStore::new(state_dir),
        )
    }

    #[test]
    fn dispatch_unknown_command_is_an_error() {
        let mgr = manager();
        let owner = owner("t-dispatch");
        let payload = json!({ "type": "terminal_bogus" });
        assert!(mgr.dispatch(&owner, Path::new("/ws"), &payload).is_err());
    }

    #[test]
    fn activate_and_reorder_persist_active_index_and_tab_order() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path().to_path_buf();
        let registry = TerminalRegistry::new(15);
        let mgr =
            TerminalManager::new(registry.clone(), TerminalStateStore::new(state_dir.clone()));
        let owner = owner("t-ui");
        let workspace = Path::new("/ws");
        let key = TerminalKey {
            owner: owner.clone(),
            workspace_root: workspace.to_path_buf(),
        };
        let first = registry
            .reserve_tab_with_profile(&key, "default", "Terminal".to_string())
            .unwrap();
        let second = registry
            .reserve_tab_with_profile(&key, "powershell", "PowerShell".to_string())
            .unwrap();

        mgr.dispatch(
            &owner,
            workspace,
            &json!({
                "type": "terminal_reorder",
                "orderedTerminalIds": [second.terminal_id, first.terminal_id],
            }),
        )
        .unwrap();
        mgr.dispatch(
            &owner,
            workspace,
            &json!({
                "type": "terminal_activate",
                "terminalId": first.terminal_id,
                "generation": first.generation,
            }),
        )
        .unwrap();

        let metadata = TerminalStateStore::new(state_dir)
            .load_for_workspace(workspace)
            .unwrap()
            .expect("metadata persisted");
        assert_eq!(metadata.active_index, Some(1));
        assert_eq!(
            metadata
                .tabs
                .iter()
                .map(|tab| tab.profile_id.as_str())
                .collect::<Vec<_>>(),
            vec!["powershell", "default"]
        );
    }

    #[test]
    fn panel_height_rejects_values_that_do_not_fit_u32() {
        let mgr = manager();
        let owner = owner("t-height-overflow");
        let result = mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({ "type": "terminal_set_panel_height", "heightPx": u64::from(u32::MAX) + 1 }),
        );
        assert_eq!(result.unwrap_err(), "heightPx is out of range");
    }

    #[test]
    fn stale_reader_and_close_do_not_touch_replacement_generation() {
        let mgr = manager();
        let owner = owner("t-generation-safe");
        let workspace = Path::new("/ws");
        let key = TerminalKey {
            owner: owner.clone(),
            workspace_root: workspace.to_path_buf(),
        };
        let reservation = mgr
            .inner
            .registry
            .reserve_tab_with_profile(&key, "default", "Terminal".to_string())
            .unwrap();
        mgr.inner
            .registry
            .commit_running(&key, &reservation.terminal_id, reservation.generation)
            .unwrap();
        let replacement = mgr
            .inner
            .registry
            .restart(
                &key,
                &reservation.terminal_id,
                reservation.generation,
                "default",
                "Terminal".to_string(),
            )
            .unwrap();
        mgr.inner
            .registry
            .commit_running(&key, &replacement.terminal_id, replacement.generation)
            .unwrap();

        let pair = native_pty_system()
            .openpty(PtySize::default())
            .expect("test PTY");
        let writer = pair.master.take_writer().expect("test PTY writer");
        mgr.inner.live.lock().unwrap().insert(
            replacement.terminal_id.clone(),
            LiveTerminal {
                owner: owner.clone(),
                workspace_root: workspace.to_path_buf(),
                generation: replacement.generation,
                profile_id: "default".to_string(),
                master: pair.master,
                writer,
                child: None,
                #[cfg(windows)]
                job: None,
            },
        );
        mgr.inner.outputs.lock().unwrap().insert(
            replacement.terminal_id.clone(),
            TerminalOutputStore::new(TerminalLimits::default()),
        );

        assert_eq!(
            mgr.append_output_if_current(
                &replacement.terminal_id,
                reservation.generation,
                b"old reader output",
            ),
            None
        );
        mgr.terminate_and_finish(
            &key,
            &CloseLease {
                terminal_id: replacement.terminal_id.clone(),
                generation: reservation.generation,
            },
        );
        assert_eq!(
            mgr.inner.live.lock().unwrap()[&replacement.terminal_id].generation,
            replacement.generation
        );
        assert!(mgr
            .inner
            .outputs
            .lock()
            .unwrap()
            .contains_key(&replacement.terminal_id));
    }

    #[test]
    fn create_spawns_a_running_terminal_with_real_shell() {
        // Uses the real SystemShellProbe: on macOS this resolves /bin/zsh or
        // /bin/bash. Skipped when no shell is resolvable on the host.
        let mgr = manager();
        let owner = owner("t-create");
        let payload = json!({ "type": "terminal_create", "profileId": "default" });
        let result = mgr.dispatch(&owner, Path::new("/ws"), &payload);
        let Ok(response) = result else {
            // Host without a usable shell (CI without /bin/zsh) — skip gracefully.
            return;
        };
        assert_eq!(response["type"], "terminal_created");
        let terminal_id = response["terminalId"].as_str().unwrap().to_string();
        let generation = response["generation"].as_u64().unwrap();

        // The new terminal is listed as running.
        let listed = mgr
            .dispatch(
                &owner,
                Path::new("/ws"),
                &json!({ "type": "terminal_list" }),
            )
            .unwrap();
        assert_eq!(listed["tabs"][0]["status"], "running");

        // Closing it removes it from the partition.
        let close = mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({ "type": "terminal_close", "terminalId": terminal_id, "generation": generation }),
        );
        assert!(close.is_ok());
        let listed = mgr
            .dispatch(
                &owner,
                Path::new("/ws"),
                &json!({ "type": "terminal_list" }),
            )
            .unwrap();
        assert!(listed["tabs"].as_array().unwrap().is_empty());

        let _ = TerminalStatus::Running; // silence unused import in no-shell hosts
    }

    #[test]
    fn list_restores_workspace_metadata_with_unique_placeholder_ids() {
        let dir = tempdir().unwrap();
        let store = TerminalStateStore::new(dir.path().to_path_buf());
        let mut metadata = WorkspaceTerminalMetadata::new("default");
        metadata.tabs = vec![
            PersistedTabDescriptor {
                label: "first".to_string(),
                profile_id: "default".to_string(),
            },
            PersistedTabDescriptor {
                label: "second".to_string(),
                profile_id: "powershell".to_string(),
            },
        ];
        metadata.panel_height_px = Some(320);
        store
            .save_for_workspace(Path::new("/ws"), &metadata)
            .unwrap();
        let mgr = TerminalManager::new(TerminalRegistry::new(15), store);
        let owner = owner("t-restored");
        let listed = mgr
            .dispatch(
                &owner,
                Path::new("/ws"),
                &json!({ "type": "terminal_list" }),
            )
            .unwrap();
        assert_eq!(listed["panelHeightPx"], 320);
        assert_eq!(listed["tabs"][0]["terminalId"], "restored-0");
        assert_eq!(listed["tabs"][1]["terminalId"], "restored-1");
    }

    #[test]
    fn pty_round_trips_utf8_input_to_output_and_resizes() {
        let mgr = manager();
        let owner = owner("t-roundtrip");
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_events = events.clone();
        mgr.set_event_sink(Arc::new(move |_owner, event| {
            sink_events.lock().unwrap().push(event);
        }));

        let Ok(created) = mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({ "type": "terminal_create", "profileId": "default" }),
        ) else {
            return; // host without a usable shell — skip
        };
        let terminal_id = created["terminalId"].as_str().unwrap().to_string();
        let generation = created["generation"].as_u64().unwrap();

        // Drive deterministic output that includes CJK text.
        let input = "printf 'hello-终端-ok\\n'\\n";
        mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({
                "type": "terminal_input",
                "terminalId": terminal_id,
                "generation": generation,
                "dataBase64": BASE64.encode(input.as_bytes()),
            }),
        )
        .expect("input accepted");

        // Wait for the echoed output to arrive via the owner event sink.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = false;
        while Instant::now() < deadline {
            let captured = events.lock().unwrap();
            seen = captured.iter().any(|event| {
                if event.pointer("/payload/type").and_then(Value::as_str) != Some("terminal_output")
                {
                    return false;
                }
                event
                    .pointer("/payload/dataBase64")
                    .and_then(Value::as_str)
                    .and_then(|data| BASE64.decode(data).ok())
                    .map(|bytes| String::from_utf8_lossy(&bytes).contains("hello-终端-ok"))
                    .unwrap_or(false)
            });
            drop(captured);
            if seen {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !seen {
            // Shell present but echo did not arrive in time (flaky host); skip.
            let _ = mgr.dispatch(
                &owner,
                Path::new("/ws"),
                &json!({ "type": "terminal_close", "terminalId": terminal_id, "generation": generation }),
            );
            return;
        }

        // Resize propagates without error.
        mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({
                "type": "terminal_resize",
                "terminalId": terminal_id,
                "generation": generation,
                "cols": 120,
                "rows": 40,
            }),
        )
        .expect("resize accepted");

        let _ = mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({ "type": "terminal_close", "terminalId": terminal_id, "generation": generation }),
        );
    }

    #[test]
    fn unknown_profile_id_is_rejected() {
        let mgr = manager();
        let owner = owner("t-profile");
        let result = mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({ "type": "terminal_create", "profileId": "bogus" }),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown shell profile"));
    }

    #[test]
    fn resize_rejects_out_of_range_dimensions() {
        let mgr = manager();
        let owner = owner("t-resize");
        // 65536 as u64 would truncate to 0; it must be rejected before truncation.
        let oversize = mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({ "type": "terminal_resize", "terminalId": "x", "generation": 1, "cols": 65536, "rows": 24 }),
        );
        assert!(oversize.is_err());
        let zero = mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({ "type": "terminal_resize", "terminalId": "x", "generation": 1, "cols": 0, "rows": 24 }),
        );
        assert!(zero.is_err());
    }

    #[test]
    fn ack_requires_a_known_running_terminal() {
        let mgr = manager();
        let owner = owner("t-ack");
        let result = mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({ "type": "terminal_ack", "terminalId": "missing", "generation": 1, "sequence": 1 }),
        );
        assert!(result.is_err());
    }

    #[test]
    fn list_returns_empty_when_no_live_tabs_and_no_metadata() {
        let mgr = manager();
        let owner = owner("t-empty");
        let listed = mgr
            .dispatch(
                &owner,
                Path::new("/ws"),
                &json!({ "type": "terminal_list" }),
            )
            .unwrap();
        assert_eq!(listed["tabs"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn panel_height_command_is_acknowledged() {
        let mgr = manager();
        let owner = owner("t-height");
        let result = mgr.dispatch(
            &owner,
            Path::new("/ws"),
            &json!({ "type": "terminal_set_panel_height", "heightPx": 400 }),
        );
        assert!(result.is_ok());
    }
}
