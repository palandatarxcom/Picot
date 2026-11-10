// ABOUTME: Owns native Pi runtime admission, transport, and bounded lifecycle cleanup.
// ABOUTME: Enforces exact runtime identity across requests, faults, and stop/replacement races.

#![allow(dead_code)]

use crate::ephemeral_registry::EphemeralRegistry;
use crate::mutation_types::is_mutation;
use crate::operation_registry::{OperationRegistry, OperationScope};
#[cfg(any(test, feature = "test-seams"))]
use crate::pi_rpc_bridge::InMemoryPiProcess;
use crate::pi_rpc_bridge::{BridgeFrame, PiRpcBridge, PiRpcProcess, PiRpcProcessObserver};
use crate::runtime_coordinator::{
    MutationAcceptance, RuntimeCoordinator, RuntimeSnapshot, RuntimeState, RuntimeTarget,
};
use crate::temp_resources::{canonical_temp_root, cleanup_quick_chat_dir};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCleanupStage {
    AdmissionClosed,
    OperationsSettled,
    ProcessTreeTerminated,
    ProcessReaped,
    TemporaryResourcesCleaned,
    RuntimeUnregistered,
}

const MAX_RPC_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeRuntimeType {
    Primary,
    Dedicated,
    SideChat,
    QuickChat,
    Standby,
    SuperAgent,
    /// Landing-only bridge-service runtime: sessionless, toolless, cwd
    /// `~/.pi/tmp`; hosts picot-bridge so ConfigGateway ops work with no
    /// workspace open. Never rendered as a session (landing-bridge-runtime
    /// spec v2).
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessPolicy {
    pub probe_required: bool,
    pub timeout: Duration,
}

impl Default for ReadinessPolicy {
    fn default() -> Self {
        Self {
            probe_required: true,
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NativeCleanupResources {
    pub temporary_directory: Option<(PathBuf, String)>,
    pub oauth_generation_supported: bool,
}

pub struct NativeLaunchSpec {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub session_path: Option<PathBuf>,
    pub extensions: Vec<PathBuf>,
    pub pi_version: String,
    pub path_env: String,
    pub agent_root: Option<PathBuf>,
    pub static_dir: Option<PathBuf>,
    pub install_secret: Option<String>,
    pub runtime_type: NativeRuntimeType,
    pub no_tools: bool,
    pub readiness: ReadinessPolicy,
    pub cleanup: NativeCleanupResources,
    /// Canonical project root of a registry-verified launch. Only the primary
    /// launch wrapper sets it; Picot's config bridge compares it against the
    /// request cwd before any project MCP read or write. It is never derived
    /// from browser input or inherited from Picot's own environment.
    pub mcp_project_root: Option<PathBuf>,
}

/// Host-issued project-root marker for project-scoped MCP operations.
pub(crate) const MCP_PROJECT_ROOT_ENV: &str = "PI_STUDIO_MCP_PROJECT_ROOT";

/// Every spawned Pi starts from a clean marker: an inherited value (a Picot
/// process that was itself launched inside a workspace) must not look like a
/// host-verified project root for Config/Quick/Side/Standby runtimes.
fn apply_launch_environment<'a>(
    command: &'a mut Command,
    launch: &LaunchDescription,
) -> &'a mut Command {
    command
        .env_remove(MCP_PROJECT_ROOT_ENV)
        .envs(&launch.environment)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchDescription {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub safe_environment: BTreeMap<String, String>,
    pub runtime_type: NativeRuntimeType,
    pub readiness: ReadinessPolicy,
}

impl NativeLaunchSpec {
    pub fn command_description(&self) -> LaunchDescription {
        let mut args = Vec::new();
        for extension in &self.extensions {
            args.push("--extension".into());
            args.push(extension.to_string_lossy().into_owned());
        }
        args.extend(["--mode".into(), "rpc".into()]);
        // pi-subagents gates `subagent` behind `subagents_enable` on Pi >= 0.86.1;
        // excluding the loader keeps `subagent` and its advertised catalog active
        // from process start, since every Picot runtime is operator-driven.
        args.extend(["--exclude-tools".into(), "subagents_enable".into()]);
        if let Some(session_path) = &self.session_path {
            args.push("--session".into());
            args.push(session_path.to_string_lossy().into_owned());
        } else if matches!(
            self.runtime_type,
            NativeRuntimeType::SideChat
                | NativeRuntimeType::QuickChat
                | NativeRuntimeType::Standby
                | NativeRuntimeType::Config
        ) {
            // Sessionless runtime types per the Gate C launch contract: the
            // host never resumes or persists a session for these runtimes.
            args.push("--no-session".into());
        }
        if self.no_tools {
            args.push("--no-tools".into());
        }
        let mut environment: BTreeMap<String, String> = BTreeMap::from([
            ("PATH".into(), self.path_env.clone()),
            ("PI_STUDIO_PI_VERSION".into(), self.pi_version.clone()),
        ]);
        if let Some(agent_root) = &self.agent_root {
            environment.insert(
                "PI_CODING_AGENT_DIR".into(),
                agent_root.to_string_lossy().into_owned(),
            );
        }
        if let Some(static_dir) = &self.static_dir {
            environment.insert(
                "PI_STUDIO_STATIC_DIR".into(),
                static_dir.to_string_lossy().into_owned(),
            );
        }
        if let Some(secret) = &self.install_secret {
            environment.insert("PI_STUDIO_SKILL_INSTALL_SECRET".into(), secret.clone());
        }
        if let Some(project_root) = &self.mcp_project_root {
            environment.insert(
                MCP_PROJECT_ROOT_ENV.into(),
                project_root.to_string_lossy().into_owned(),
            );
        }
        let safe_environment = environment
            .iter()
            .map(|(key, value)| {
                let value = if key == "PI_STUDIO_SKILL_INSTALL_SECRET" {
                    "<redacted>".into()
                } else {
                    value.clone()
                };
                (key.clone(), value)
            })
            .collect();
        LaunchDescription {
            program: self.binary.clone(),
            args,
            environment,
            safe_environment,
            runtime_type: self.runtime_type,
            readiness: self.readiness,
        }
    }
}

struct ManagedRuntime {
    target: Arc<Mutex<RuntimeTarget>>,
    bridge: PiRpcBridge,
    process: Option<PiRpcProcess>,
    cleanup: NativeCleanupResources,
    /// Host-validated Pi `sessionFile` last reported by `get_state` for this
    /// instance. Retained so `runtime_instance_summaries` reads it in O(1)
    /// instead of rescanning the workspace bucket per runtime. Cleared whenever
    /// the instance's session identity is rebound (nothing derived may outlive
    /// its identity).
    session_file: Arc<Mutex<Option<PathBuf>>>,
}

/// Test barrier invoked from `stop` after its tombstone (see `stop_barrier`).
#[cfg(test)]
type StopBarrier = Arc<dyn Fn(&RuntimeTarget) + Send + Sync>;

struct NativePiManagerInner {
    coordinator: Mutex<RuntimeCoordinator>,
    /// P1 adapter boundary: native callers currently provide RuntimeTarget only.
    /// Owner/workspace generation must come from Gate R before production admission;
    /// no browser-root or synthetic owner fallback is allowed here.
    operations: Mutex<OperationRegistry>,
    /// Most recent accepted operation per runtime instance. Sole consumer is
    /// the bare-abort authorization: a stop may only forward to pi when the
    /// requester's scope matches the operation that started the running turn.
    turn_operations: Mutex<HashMap<String, (String, OperationScope)>>,
    runtimes: Mutex<HashMap<String, ManagedRuntime>>,
    events: broadcast::Sender<NativeRuntimeEvent>,
    pending_ui: Mutex<HashMap<String, Vec<NativeRuntimeEvent>>>,
    /// Exact targets currently closing or already stopped. Retaining exact
    /// identity makes repeated stop a no-op without allowing stale stops to
    /// affect a replacement using the same instance id.
    closing: Mutex<HashMap<String, RuntimeTarget>>,
    /// Linearizes terminal admission against runtime loss (stop, crash,
    /// removal). Lock order: this guard first, then `closing`, `runtimes`,
    /// `coordinator` — no path acquires it while holding one of those, and
    /// `with_live_terminal_target` never holds two of them at once.
    admission: Mutex<()>,
    /// Test barrier that runs after the stop tombstone and before process
    /// cleanup: the only window in which a runtime is closing *and* still
    /// registered, which is exactly the shape admission must reject.
    #[cfg(test)]
    stop_barrier: Mutex<Option<StopBarrier>>,
    /// Test barrier that runs in `crash_and_remove` after it captured the
    /// current identity, so a test can prove capture, crash and removal are one
    /// admission: nothing can rebind the identity while it is parked there.
    #[cfg(test)]
    crash_barrier: Mutex<Option<CrashBarrier>>,
    cleanup_trace: Mutex<Vec<(String, NativeCleanupStage)>>,
    revoked_owners: Mutex<HashMap<String, u64>>,
    ephemeral_registry: Mutex<Option<Arc<EphemeralRegistry>>>,
    /// Owner lifecycle authority for reattach decisions (plan D11/T7). Set at
    /// boot like the ephemeral registry; a rebind without it fails closed.
    owner_registry: Mutex<Option<Arc<crate::window_owner::WindowOwnerRegistry>>>,
    owners: Mutex<std::collections::HashSet<String>>,
    secret_generations: Mutex<HashMap<String, u64>>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRuntimeEvent {
    pub target: RuntimeTarget,
    pub sequence: u64,
    pub event: Value,
}

#[derive(Clone)]
pub struct NativePiManager {
    inner: Arc<NativePiManagerInner>,
}

/// The registry entry's current identity for an instance, if any.
fn live_target(inner: &NativePiManagerInner, instance_id: &str) -> Option<RuntimeTarget> {
    inner
        .runtimes
        .lock()
        .ok()?
        .get(instance_id)
        .and_then(|runtime| runtime.target.lock().ok().map(|target| target.clone()))
}

/// Remove an exiting runtime, under the caller's admission and only for the
/// exact identity it observed (plan §5, B2).
///
/// The identity check matters because a rebind may have moved generation, owner
/// or session between the exit and this removal: deleting by instance key alone
/// could remove a runtime some page still legitimately holds. Anything else
/// means the entry is already gone (another path removed it) or its identity
/// changed, and either way this removal has nothing to do.
fn remove_closed_runtime(inner: &NativePiManagerInner, expected: &RuntimeTarget) {
    if live_target(inner, &expected.instance_id).as_ref() != Some(expected) {
        return;
    }
    let runtime = inner
        .runtimes
        .lock()
        .ok()
        .and_then(|mut runtimes| runtimes.remove(&expected.instance_id));
    let Some(mut runtime) = runtime else {
        return;
    };
    if let Some(process) = &mut runtime.process {
        let signalled = process.kill().unwrap_or(false);
        if let Some(pid) = process.pid() {
            if signalled {
                crate::child_supervision::forget_runtime(pid);
            } else {
                log::warn!(
                    "[picot-native] runtime process group {pid} was not signalled; retaining registry entry"
                );
            }
        }
    }
    let target = runtime.target.lock().ok().map(|target| target.clone());
    if let Some(target) = target {
        if let Ok(mut coordinator) = inner.coordinator.lock() {
            let _ = coordinator.unregister(&target);
        }
    }
    if let Ok(mut pending_ui) = inner.pending_ui.lock() {
        pending_ui.remove(&expected.instance_id);
    }
}

/// Attribute a process exit to the runtime's *current* identity and remove it.
///
/// The event pump captures a target when it starts; a rebind (generation, owner,
/// session) makes that capture stale while the process — and the registry entry
/// — stay the same. Identity capture, crash attribution and removal therefore
/// share one admission (plan §5, B2): a rebind either lands before the capture,
/// so this exit belongs to the rebound identity, or after the removal, and then
/// it finds nothing to revive. Split across admissions, a rebind landing between
/// them makes the crash and the removal both refuse, and a dead runtime stays
/// registered and admitted.
fn crash_and_remove(inner: &Arc<NativePiManagerInner>, instance_id: &str) {
    let Ok(_admission) = inner.admission.lock() else {
        return;
    };
    let Some(current) = live_target(inner, instance_id) else {
        return;
    };
    #[cfg(test)]
    hold_crash_barrier(inner, &current);
    mark_crashed(inner, &current, "runtime_crashed");
    remove_closed_runtime(inner, &current);
}

/// The crash half of [`crash_and_remove`], under the caller's admission: taking
/// that guard before the state change means no terminal dispatch is admitted
/// after the crash was observed, even though the instance stays in the registry
/// until removal.
fn mark_crashed(
    inner: &Arc<NativePiManagerInner>,
    expected_target: &RuntimeTarget,
    reason: impl Into<String>,
) {
    let reason = reason.into();
    if inner
        .closing
        .lock()
        .ok()
        .and_then(|closing| closing.get(&expected_target.instance_id).cloned())
        .is_some_and(|closing_target| closing_target == *expected_target)
    {
        return;
    }
    if let Ok(mut turn_operations) = inner.turn_operations.lock() {
        turn_operations.remove(&expected_target.instance_id);
    }
    let Ok(runtimes) = inner.runtimes.lock() else {
        return;
    };
    let Some(managed) = runtimes.get(expected_target.instance_id.as_str()) else {
        return;
    };
    let Ok(current_target) = managed.target.lock() else {
        return;
    };
    if *current_target != *expected_target {
        return;
    }
    let target = current_target.clone();
    drop(current_target);
    drop(runtimes);
    let Ok(mut coordinator) = inner.coordinator.lock() else {
        return;
    };
    let Ok(snapshot) = coordinator.snapshot(&target) else {
        return;
    };
    if snapshot.state == RuntimeState::Crashed || snapshot.state == RuntimeState::Stopped {
        return;
    }
    if coordinator
        .set_state(&target, RuntimeState::Crashed)
        .is_err()
    {
        return;
    }
    if let Ok(mut operations) = inner.operations.lock() {
        operations.instance_replaced(&target.instance_id, "runtime_crashed");
    }
    for event in [
        serde_json::json!({ "type": "runtime_crashed", "reason": reason }),
        serde_json::json!({ "type": "snapshot_required", "reason": "runtime_crashed" }),
    ] {
        let Ok(sequenced) = coordinator.emit_event(&target, event) else {
            return;
        };
        let _ = inner.events.send(NativeRuntimeEvent {
            target: sequenced.target,
            sequence: sequenced.sequence,
            event: sequenced.event,
        });
    }
}

/// Crash an exactly-identified runtime without removing it, under its own
/// admission. Only the crate's tests call this; the production exit path is
/// [`crash_and_remove`], which never splits capture from removal.
#[cfg(test)]
fn emit_runtime_crashed(
    inner: &Arc<NativePiManagerInner>,
    expected_target: &RuntimeTarget,
    reason: impl Into<String>,
) {
    let Ok(_admission) = inner.admission.lock() else {
        return;
    };
    mark_crashed(inner, expected_target, reason);
}

/// Test barrier invoked from `crash_and_remove` once it captured the current
/// identity, still inside the admission (see `crash_barrier`).
#[cfg(test)]
type CrashBarrier = Arc<dyn Fn(&RuntimeTarget) + Send + Sync>;

#[cfg(test)]
fn hold_crash_barrier(inner: &NativePiManagerInner, target: &RuntimeTarget) {
    let barrier = inner
        .crash_barrier
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    if let Some(barrier) = barrier {
        barrier(target);
    }
}

impl NativePiManager {
    pub fn new(idempotency_capacity: usize) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(NativePiManagerInner {
                coordinator: Mutex::new(RuntimeCoordinator::new(idempotency_capacity)),
                operations: Mutex::new(OperationRegistry::new(
                    idempotency_capacity,
                    Duration::from_secs(300),
                )),
                runtimes: Mutex::new(HashMap::new()),
                turn_operations: Mutex::new(HashMap::new()),
                events,
                pending_ui: Mutex::new(HashMap::new()),
                closing: Mutex::new(HashMap::new()),
                admission: Mutex::new(()),
                #[cfg(test)]
                stop_barrier: Mutex::new(None),
                #[cfg(test)]
                crash_barrier: Mutex::new(None),
                cleanup_trace: Mutex::new(Vec::new()),
                revoked_owners: Mutex::new(HashMap::new()),
                ephemeral_registry: Mutex::new(None),
                owner_registry: Mutex::new(None),
                owners: Mutex::new(std::collections::HashSet::new()),
                secret_generations: Mutex::new(HashMap::new()),
            }),
        }
    }

    #[cfg(test)]
    fn in_memory(idempotency_capacity: usize) -> Self {
        Self::new(idempotency_capacity)
    }

    pub fn spawn(&self, target: RuntimeTarget, spec: NativeLaunchSpec) -> Result<(), String> {
        if let Some(owner_id) = target.owner_id.as_deref() {
            if self
                .inner
                .revoked_owners
                .lock()
                .map_err(|_| "Native owner revocation lock poisoned".to_string())?
                .get(owner_id)
                .is_some_and(|generation| *generation >= target.workspace_generation)
            {
                return Err("Native runtime owner generation has been revoked".into());
            }
            self.inner
                .secret_generations
                .lock()
                .map_err(|_| "Native secret registry lock poisoned".to_string())?
                .insert(owner_id.to_string(), target.workspace_generation);
        }
        let launch = spec.command_description();
        let mut command = Command::new(&launch.program);
        configure_child_process(&mut command);
        apply_launch_environment(&mut command, &launch)
            .args(&launch.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command
            .spawn()
            .map_err(|error| format!("Cannot start embedded Pi native RPC process: {error}"))?;
        let (bridge, mut process) = PiRpcBridge::attach(child, MAX_RPC_FRAME_BYTES)?;
        // Leave a record of this runtime: a Picot that is killed outright runs
        // no teardown, and a wedged pi never notices the stdin EOF that would
        // otherwise stop it. The next launch sweeps what this leaves behind.
        // Recorded after attach: the identity probe shells out to `ps`, and a
        // child that exits immediately (a stub command in tests, a broken pi)
        // must not be reported as a group-identity mismatch before the bridge
        // has even taken ownership of it.
        if let Some(pid) = process.pid() {
            crate::child_supervision::record_runtime(pid);
        }
        let observer = process.observer();
        if let Err(error) = self
            .inner
            .coordinator
            .lock()
            .map_err(|_| "Runtime coordinator lock poisoned".to_string())?
            .register(target.clone(), RuntimeState::Starting)
        {
            let _ = process.kill();
            if let Some(pid) = process.pid() {
                crate::child_supervision::forget_runtime(pid);
            }
            return Err(format!("Cannot register Pi runtime: {error:?}"));
        }
        if let Some(owner_id) = target.owner_id.as_ref() {
            self.inner
                .owners
                .lock()
                .map_err(|_| "Native owner registry lock poisoned".to_string())?
                .insert(owner_id.clone());
        }
        self.inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?
            .insert(
                target.instance_id.clone(),
                ManagedRuntime {
                    target: Arc::new(Mutex::new(target.clone())),
                    bridge: bridge.clone(),
                    process: Some(process),
                    cleanup: spec.cleanup,
                    session_file: Arc::new(Mutex::new(None)),
                },
            );
        self.start_event_pump(target, bridge, Some(observer));
        Ok(())
    }

    /// Whether this runtime is mid-turn (`agent_start` … `agent_end`): the
    /// authoritative streaming flag surfaced in `runtime_instances`.
    pub fn is_working(&self, target: &crate::runtime_coordinator::RuntimeTarget) -> bool {
        self.state_of(target) == Some(crate::runtime_coordinator::RuntimeState::Working)
    }

    /// The runtime's coordinator state, or `None` when the coordinator no longer
    /// tracks it: the one read-only probe behind `is_working` and every
    /// `daemon.status` agent row (`None` travels to the wire as a null state).
    pub fn state_of(
        &self,
        target: &crate::runtime_coordinator::RuntimeTarget,
    ) -> Option<crate::runtime_coordinator::RuntimeState> {
        self.inner
            .coordinator
            .lock()
            .ok()
            .and_then(|coordinator| coordinator.state_of(target))
    }

    /// Test seam: register a runtime backed by an in-memory bridge instead of a
    /// spawned Pi child. Compiled for this crate's tests and for consuming
    /// packages that enable `test-seams` (the app package owns the launch
    /// orchestration those tests assert on) — same posture as
    /// `host_server::set_agent_root_for_tests`.
    #[cfg(any(test, feature = "test-seams"))]
    pub fn register_in_memory(&self, target: RuntimeTarget) -> Result<InMemoryPiProcess, String> {
        // Production runtimes allow 16MB frames (snapshot-scale get_tree
        // replies); tests that push big payloads must see the same ceiling,
        // not a 1MB test-only cap.
        let (bridge, process) = PiRpcBridge::in_memory(MAX_RPC_FRAME_BYTES);
        self.inner
            .coordinator
            .lock()
            .map_err(|_| "Runtime coordinator lock poisoned".to_string())?
            .register(target.clone(), RuntimeState::Ready)
            .map_err(|error| format!("Cannot register test runtime: {error:?}"))?;
        self.inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?
            .insert(
                target.instance_id.clone(),
                ManagedRuntime {
                    target: Arc::new(Mutex::new(target.clone())),
                    bridge: bridge.clone(),
                    process: None,
                    cleanup: NativeCleanupResources::default(),
                    session_file: Arc::new(Mutex::new(None)),
                },
            );
        self.start_event_pump(target, bridge, None);
        Ok(process)
    }

    fn start_event_pump(
        &self,
        target: RuntimeTarget,
        bridge: PiRpcBridge,
        mut observer: Option<PiRpcProcessObserver>,
    ) {
        let inner = Arc::clone(&self.inner);
        // Called synchronously from the Tauri setup thread during native
        // startup; spawn on the process-wide host runtime instead of the
        // ambient tokio context (which does not exist on that thread).
        crate::runtime::spawn(async move {
            let mut poll = tokio::time::interval(Duration::from_millis(50));
            loop {
                let frame = if observer.is_some() {
                    tokio::select! {
                        frame = bridge.next_frame() => frame,
                        _ = poll.tick() => {
                            if observer.as_mut().and_then(|process| process.try_wait().ok()).flatten().is_some() {
                                crash_and_remove(&inner, &target.instance_id);
                                return;
                            }
                            continue;
                        }
                    }
                } else {
                    bridge.next_frame().await
                };
                let Some(frame) = frame else {
                    crash_and_remove(&inner, &target.instance_id);
                    return;
                };
                let current_target = inner.runtimes.lock().ok().and_then(|runtimes| {
                    runtimes
                        .get(&target.instance_id)?
                        .target
                        .lock()
                        .ok()
                        .map(|target| target.clone())
                });
                let Some(current_target) = current_target else {
                    return;
                };
                let event = match frame {
                    BridgeFrame::Event(event) | BridgeFrame::ExtensionUi(event) => event,
                    // A garbled line (e.g. pi 0.84.2 statusText with raw
                    // newlines) is a dropped frame, not a dead runtime: surface
                    // it as a sequenced event and keep the pump running.
                    BridgeFrame::ProtocolError(message) => serde_json::json!({
                        "type": "protocol_error",
                        "message": message
                    }),
                    BridgeFrame::TransportError(_) => {
                        crash_and_remove(&inner, &target.instance_id);
                        return;
                    }
                };
                let sequenced = {
                    let Ok(mut coordinator) = inner.coordinator.lock() else {
                        return;
                    };
                    match event.get("type").and_then(Value::as_str) {
                        Some("agent_start") => {
                            let _ = coordinator.set_state(&current_target, RuntimeState::Working);
                        }
                        Some("agent_settled") | Some("agent_end") => {
                            let _ = coordinator.set_state(&current_target, RuntimeState::Idle);
                        }
                        _ => {}
                    }
                    coordinator.emit_event(&current_target, event)
                };
                let Ok(sequenced) = sequenced else {
                    return;
                };
                let runtime_event = NativeRuntimeEvent {
                    target: sequenced.target,
                    sequence: sequenced.sequence,
                    event: sequenced.event,
                };
                if runtime_event.event.get("type").and_then(Value::as_str)
                    == Some("extension_ui_request")
                {
                    if let Ok(mut pending) = inner.pending_ui.lock() {
                        pending
                            .entry(runtime_event.target.instance_id.clone())
                            .or_default()
                            .push(runtime_event.clone());
                    }
                }
                let _ = inner.events.send(runtime_event);
            }
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<NativeRuntimeEvent> {
        self.inner.events.subscribe()
    }

    /// Returns operation state only for exact host-derived logical scope.
    pub fn operation_status(
        &self,
        operation_id: &str,
        scope: &OperationScope,
    ) -> Result<crate::operation_registry::OperationRecord, String> {
        self.inner
            .operations
            .lock()
            .map_err(|_| "Operation registry lock poisoned".to_string())?
            .get_scoped(operation_id, scope)
            .cloned()
            .map_err(|error| format!("Operation lookup rejected: {error:?}"))
    }

    pub fn operation_status_for_context(
        &self,
        operation_id: &str,
        owner_id: &str,
        workspace_id: &str,
        generation: u64,
    ) -> Result<crate::operation_registry::OperationRecord, String> {
        self.inner
            .operations
            .lock()
            .map_err(|_| "Operation registry lock poisoned".to_string())?
            .get_owner_scoped(operation_id, owner_id, workspace_id, generation)
            .cloned()
            .map_err(|error| format!("Operation lookup rejected: {error:?}"))
    }

    /// Host restart invalidates unresolved logical operations but retains terminal records.
    pub fn mark_host_restart(&self, reason: impl Into<String>) -> Result<(), String> {
        self.inner
            .operations
            .lock()
            .map_err(|_| "Operation registry lock poisoned".to_string())?
            .host_restart(reason);
        Ok(())
    }

    /// Instance replacement cannot silently inherit unresolved work.
    pub fn mark_instance_replaced(
        &self,
        instance_id: &str,
        reason: impl Into<String>,
    ) -> Result<(), String> {
        if let Ok(mut turn_operations) = self.inner.turn_operations.lock() {
            turn_operations.remove(instance_id);
        }
        self.inner
            .operations
            .lock()
            .map_err(|_| "Operation registry lock poisoned".to_string())?
            .instance_replaced(instance_id, reason);
        Ok(())
    }

    pub fn pending_extension_ui(
        &self,
        target: &RuntimeTarget,
    ) -> Result<Vec<NativeRuntimeEvent>, String> {
        self.inner
            .coordinator
            .lock()
            .map_err(|_| "Runtime coordinator lock poisoned".to_string())?
            .validate(target)
            .map_err(|error| format!("Extension UI lookup rejected: {error:?}"))?;
        Ok(self
            .inner
            .pending_ui
            .lock()
            .map_err(|_| "Pending extension UI lock poisoned".to_string())?
            .get(&target.instance_id)
            .cloned()
            .unwrap_or_default())
    }

    /// Sends a mutation through the logical operation authority.
    ///
    /// Scope is deliberately explicit: until host owner/generation snapshots are
    /// wired, callers cannot use the legacy `request` adapter for operation status.
    pub async fn request_scoped(
        &self,
        target: &RuntimeTarget,
        scope: OperationScope,
        command: Value,
        idempotency_key: &str,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.request_scoped_receipt(target, scope, command, idempotency_key, timeout)
            .await
            .map(|(_, _, response)| response.unwrap_or(Value::Null))
    }

    /// Run a host-side mutation through the Operation Registry (plan §14:
    /// writes walk the mutation registry): accept with the caller's
    /// idempotency key, execute, then complete with the terminal result or
    /// mark indeterminate on failure. Duplicate-completed replays return the
    /// recorded terminal result; a duplicate-pending key is reported back as
    /// `OperationAcceptance::DuplicatePending` so the caller can answer with
    /// `duplicate_pending` instead of a generic failure.
    ///
    /// Errors are `(code, message)` pairs so a caller can propagate the
    /// filesystem-level code straight out of `dispatch` without re-wrapping it.
    pub fn host_mutation<F>(
        &self,
        scope: crate::operation_registry::OperationScope,
        idempotency_key: &str,
        command_type: &str,
        execution_instance_id: &str,
        operation: F,
    ) -> Result<
        (
            String,
            crate::operation_registry::OperationAcceptance,
            Option<Value>,
        ),
        (&'static str, String),
    >
    where
        F: FnOnce() -> Result<Value, (&'static str, String)>,
    {
        use crate::operation_registry::OperationAcceptance;
        let (operation_id, acceptance) = self
            .inner
            .operations
            .lock()
            .map_err(|_| {
                (
                    "operation_registry_failed",
                    "Operation registry lock poisoned".to_string(),
                )
            })?
            .accept(
                scope,
                idempotency_key,
                command_type,
                execution_instance_id.to_string(),
            )
            .map_err(|error| {
                (
                    "operation_rejected",
                    format!("Operation rejected: {error:?}"),
                )
            })?;
        match acceptance {
            OperationAcceptance::DuplicateCompleted => {
                let record = self
                    .inner
                    .operations
                    .lock()
                    .map_err(|_| {
                        (
                            "operation_registry_failed",
                            "Operation registry lock poisoned".to_string(),
                        )
                    })?
                    .get(&operation_id)
                    .map_err(|error| {
                        (
                            "operation_rejected",
                            format!("Operation lookup rejected: {error:?}"),
                        )
                    })?
                    .clone();
                let terminal = record.terminal_response;
                Ok((operation_id, acceptance, terminal))
            }
            OperationAcceptance::DuplicatePending => Ok((operation_id, acceptance, None)),
            OperationAcceptance::Accepted => match operation() {
                Ok(result) => {
                    let terminal = result.clone();
                    self.inner
                        .operations
                        .lock()
                        .map_err(|_| {
                            (
                                "operation_registry_failed",
                                "Operation registry lock poisoned".to_string(),
                            )
                        })?
                        .complete(&operation_id, terminal)
                        .map_err(|error| {
                            (
                                "operation_registry_failed",
                                format!("Cannot complete operation: {error:?}"),
                            )
                        })?;
                    Ok((operation_id, acceptance, Some(result)))
                }
                Err(error) => {
                    if let Ok(mut registry) = self.inner.operations.lock() {
                        let _ = registry.mark_indeterminate(&operation_id, error.1.clone());
                    }
                    Err(error)
                }
            },
        }
    }

    pub async fn request_scoped_receipt(
        &self,
        target: &RuntimeTarget,
        scope: OperationScope,
        command: Value,
        idempotency_key: &str,
        timeout: Duration,
    ) -> Result<
        (
            String,
            crate::operation_registry::OperationAcceptance,
            Option<Value>,
        ),
        String,
    > {
        if scope.workspace_id != target.workspace_id || scope.session_id != target.session_id {
            return Err("Operation scope does not match runtime target".into());
        }
        let command_type = command
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "Runtime command type is missing".to_string())?;
        if self.is_closing(target)? {
            return Err("Native runtime is stopping".into());
        }
        let (operation_id, acceptance) = self
            .inner
            .operations
            .lock()
            .map_err(|_| "Operation registry lock poisoned".to_string())?
            .accept(
                scope.clone(),
                idempotency_key,
                command_type,
                target.instance_id.clone(),
            )
            .map_err(|error| format!("Operation rejected: {error:?}"))?;
        match acceptance {
            crate::operation_registry::OperationAcceptance::DuplicateCompleted => {
                let response = self
                    .operation_status(&operation_id, &scope)
                    .map(|record| record.terminal_response.clone().unwrap_or(Value::Null))?;
                return Ok((operation_id, acceptance, Some(response)));
            }
            crate::operation_registry::OperationAcceptance::DuplicatePending => {
                return Ok((operation_id, acceptance, None));
            }
            crate::operation_registry::OperationAcceptance::Accepted => {}
        }
        // Track the latest accepted operation for this instance so the event
        // pump can bind turn events (which arrive while the turn is running)
        // to the operation that started the turn.
        if let Ok(mut turn_operations) = self.inner.turn_operations.lock() {
            turn_operations.insert(
                target.instance_id.clone(),
                (operation_id.clone(), scope.clone()),
            );
        }
        let bridge = self
            .inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?
            .get(&target.instance_id)
            .map(|runtime| runtime.bridge.clone())
            .ok_or_else(|| "Native runtime instance is not running".to_string())?;
        let response = bridge.request(command, timeout).await.map_err(|error| {
            if let Ok(mut registry) = self.inner.operations.lock() {
                let _ = registry.mark_indeterminate(&operation_id, format!("rpc_{error:?}"));
            }
            self.rpc_error_with_diagnostics(&target.instance_id, error)
        })?;
        self.inner
            .operations
            .lock()
            .map_err(|_| "Operation registry lock poisoned".to_string())?
            .complete(&operation_id, response.clone())
            .map_err(|error| format!("Cannot complete operation: {error:?}"))?;
        Ok((operation_id, acceptance, Some(response)))
    }

    /// Aborts the current run with pi's native bare abort command. Requires
    /// the runtime to be Working and the requester's scope to match the most
    /// recent accepted operation for the instance; anything less settles as
    /// `stale_turn`. Deliberately bypasses both idempotency caches.
    pub async fn abort_turn(
        &self,
        target: &RuntimeTarget,
        scope: &OperationScope,
        timeout: Duration,
    ) -> Result<Value, String> {
        if scope.workspace_id != target.workspace_id || scope.session_id != target.session_id {
            return Ok(serde_json::json!({ "disposition": "stale_turn" }));
        }
        let working = {
            let coordinator = self
                .inner
                .coordinator
                .lock()
                .map_err(|_| "Runtime coordinator lock poisoned".to_string())?;
            coordinator
                .validate_identity(target)
                .map_err(|error| format!("Abort target rejected: {error:?}"))?;
            coordinator.state_of(target) == Some(RuntimeState::Working)
        };
        if !working {
            return Ok(serde_json::json!({ "disposition": "stale_turn" }));
        }
        let binding = self
            .inner
            .turn_operations
            .lock()
            .map_err(|_| "Turn operation lock poisoned".to_string())?
            .get(&target.instance_id)
            .cloned();
        let Some((operation_id, operation_scope)) = binding else {
            return Ok(serde_json::json!({ "disposition": "stale_turn" }));
        };
        if operation_scope != *scope
            || self
                .inner
                .operations
                .lock()
                .map_err(|_| "Operation registry lock poisoned".to_string())?
                .get_scoped(&operation_id, scope)
                .is_err()
        {
            return Ok(serde_json::json!({ "disposition": "stale_turn" }));
        }
        let bridge = self
            .inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?
            .get(&target.instance_id)
            .map(|runtime| runtime.bridge.clone())
            .ok_or_else(|| "Native runtime instance is not running".to_string())?;
        bridge
            .request(serde_json::json!({ "type": "abort" }), timeout)
            .await
            .map_err(|error| format!("Pi abort failed: {error:?}"))
    }

    pub async fn request(
        &self,
        target: &RuntimeTarget,
        command: Value,
        idempotency_key: Option<&str>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let mut mutation_key = None;
        {
            let mut coordinator = self
                .inner
                .coordinator
                .lock()
                .map_err(|_| "Runtime coordinator lock poisoned".to_string())?;
            coordinator
                .validate_command(target, &command)
                .map_err(|error| format!("Runtime request rejected: {error:?}"))?;
            if self.is_closing(target)? {
                return Err("Native runtime is stopping".into());
            }
            if is_mutation(
                command
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ) {
                let key = idempotency_key
                    .ok_or_else(|| "Runtime mutation requires an idempotency key".to_string())?;
                // Legacy request API has no owner/workspace generation snapshot.
                // Preserve its coordinator cache; never fabricate OperationScope authority.
                let acceptance = coordinator
                    .accept_mutation(target, key)
                    .map_err(|error| format!("Runtime mutation rejected: {error:?}"))?;
                if acceptance == MutationAcceptance::Duplicate {
                    return coordinator
                        .mutation_result(target, key)
                        .map_err(|error| format!("Cannot read mutation result: {error:?}"))?
                        .ok_or_else(|| {
                            "Runtime mutation was accepted and is still pending".into()
                        });
                }
                mutation_key = Some(key.to_owned());
            }
        }
        let bridge = self
            .inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?
            .get(&target.instance_id)
            .map(|runtime| runtime.bridge.clone())
            .ok_or_else(|| "Native runtime instance is not running".to_string())?;
        let response = bridge
            .request(command, timeout)
            .await
            .map_err(|error| self.rpc_error_with_diagnostics(&target.instance_id, error))?;
        if let Some(key) = mutation_key {
            self.inner
                .coordinator
                .lock()
                .map_err(|_| "Runtime coordinator lock poisoned".to_string())?
                .complete_mutation(target, &key, response.clone())
                .map_err(|error| format!("Cannot cache mutation result: {error:?}"))?;
        }
        Ok(response)
    }

    /// Preserve Pi's stderr tail with its transport failure; otherwise a
    /// closed child leaves callers no evidence about its exit.
    fn rpc_error_with_diagnostics(
        &self,
        instance_id: &str,
        error: crate::pi_rpc_bridge::BridgeError,
    ) -> String {
        let mut message = format!("Pi RPC request failed: {error:?}");
        let stderr = self.inner.runtimes.lock().ok().and_then(|runtimes| {
            runtimes
                .get(instance_id)?
                .process
                .as_ref()?
                .drain_diagnostics()
        });
        if let Some(stderr) = stderr {
            message.push_str(&format!("\nPi stderr:\n{stderr}"));
        }
        message
    }

    fn record_cleanup(&self, target: &RuntimeTarget, stage: NativeCleanupStage) {
        if let Ok(mut trace) = self.inner.cleanup_trace.lock() {
            trace.push((target.instance_id.clone(), stage));
        }
    }

    /// Stops one exact runtime. All lifecycle callers use this ordered path.
    pub fn stop(&self, target: &RuntimeTarget) -> Result<(), String> {
        {
            let coordinator = self
                .inner
                .coordinator
                .lock()
                .map_err(|_| "Runtime coordinator lock poisoned".to_string())?;
            if let Err(error) = coordinator.validate(target) {
                if self
                    .inner
                    .closing
                    .lock()
                    .map_err(|_| "Runtime stop registry lock poisoned".to_string())?
                    .get(&target.instance_id)
                    == Some(target)
                {
                    return Ok(());
                }
                return Err(format!("Runtime stop rejected: {error:?}"));
            }
        }

        // Close admission before touching operations or the child. The same
        // guard serializes this with terminal admission, so a dispatch is either
        // already inside (and finishes) or rejected from here on.
        let already_closing = {
            let _admission = self
                .inner
                .admission
                .lock()
                .map_err(|_| "Runtime admission lock poisoned".to_string())?;
            // Re-verify the exact identity inside the guard (plan §5, B2): a
            // rebind may have landed between the validation above and this
            // tombstone, and closing admission for an identity that no longer
            // exists would stop a runtime nobody asked to stop.
            if live_target(&self.inner, &target.instance_id)
                .is_some_and(|current| current != *target)
            {
                return Err("Runtime stop rejected: identity changed".to_string());
            }
            let mut closing = self
                .inner
                .closing
                .lock()
                .map_err(|_| "Runtime stop registry lock poisoned".to_string())?;
            // A valid replacement supersedes a stopped tombstone with same
            // instance key; stale callers still fail coordinator validation.
            if closing
                .get(&target.instance_id)
                .is_some_and(|current| current != target)
            {
                closing.remove(&target.instance_id);
            }
            closing.insert(target.instance_id.clone(), target.clone()) == Some(target.clone())
        };
        if already_closing {
            return Ok(());
        }
        self.record_cleanup(target, NativeCleanupStage::AdmissionClosed);
        #[cfg(test)]
        self.hold_stop_after_admission(target);

        // Preserve operation records, but make unresolved work explicitly
        // indeterminate so no completed response can be replayed as success.
        self.inner
            .operations
            .lock()
            .map_err(|_| "Operation registry lock poisoned".to_string())?
            .instance_replaced(&target.instance_id, "runtime_stopped");
        self.record_cleanup(target, NativeCleanupStage::OperationsSettled);

        let mut runtime = self
            .inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?
            .remove(&target.instance_id)
            .ok_or_else(|| "Native runtime instance is not running".to_string())?;

        let mut cleanup_error = None;
        if let Some(process) = &mut runtime.process {
            let pid = process.pid();
            let signalled = match process.kill() {
                Ok(signalled) => signalled,
                Err(error) => {
                    cleanup_error = Some(error);
                    false
                }
            };
            if let Err(error) = process.wait() {
                cleanup_error = Some(error);
            }
            // Keep the record unless the process group was signalled and
            // reaping completed. A direct child may already be gone while a
            // descendant remains alive; the next startup sweep needs this pid.
            if signalled && cleanup_error.is_none() {
                if let Some(pid) = pid {
                    crate::child_supervision::forget_runtime(pid);
                }
            } else if let Some(pid) = pid {
                log::warn!(
                    "[picot-native] runtime process group {pid} cleanup incomplete; retaining registry entry"
                );
            }
        }
        self.record_cleanup(target, NativeCleanupStage::ProcessTreeTerminated);
        self.record_cleanup(target, NativeCleanupStage::ProcessReaped);
        if let Some((path, token)) = runtime.cleanup.temporary_directory.take() {
            let root = canonical_temp_root();
            if let Err(error) = cleanup_quick_chat_dir(&root, &path, &token) {
                cleanup_error.get_or_insert(error);
            }
        }
        // OAuth is unsupported by native runtime. The false capability is a
        // fail-closed state; no token or generation cleanup is attempted.
        self.record_cleanup(target, NativeCleanupStage::TemporaryResourcesCleaned);
        // Pending UI belongs to exact instance and is no longer addressable.
        self.inner
            .pending_ui
            .lock()
            .map_err(|_| "Pending extension UI lock poisoned".to_string())?
            .remove(&target.instance_id);

        let terminal = {
            let mut coordinator = self
                .inner
                .coordinator
                .lock()
                .map_err(|_| "Runtime coordinator lock poisoned".to_string())?;
            coordinator
                .set_state(target, RuntimeState::Stopped)
                .map_err(|error| format!("Cannot stop runtime: {error:?}"))?;
            coordinator
                .emit_event(target, serde_json::json!({ "type": "runtime_stopped" }))
                .map_err(|error| format!("Cannot emit stopped runtime: {error:?}"))?
        };
        let _ = self.inner.events.send(NativeRuntimeEvent {
            target: terminal.target,
            sequence: terminal.sequence,
            event: terminal.event,
        });
        self.inner
            .coordinator
            .lock()
            .map_err(|_| "Runtime coordinator lock poisoned".to_string())?
            .unregister(target)
            .map_err(|error| format!("Cannot unregister stopped runtime: {error:?}"))?;
        self.record_cleanup(target, NativeCleanupStage::RuntimeUnregistered);
        cleanup_error.map_or(Ok(()), Err)
    }

    fn is_closing(&self, target: &RuntimeTarget) -> Result<bool, String> {
        Ok(self
            .inner
            .closing
            .lock()
            .map_err(|_| "Runtime stop registry lock poisoned".to_string())?
            .get(&target.instance_id)
            .is_some_and(|current| current == target))
    }

    /// Run one synchronous terminal dispatch only while `expected` is the exact
    /// live runtime and its admission is open (plan 2026-11-03 §5).
    ///
    /// The guard is the same lock `stop` takes to record its closing tombstone,
    /// so exactly one order is possible: either the dispatch admits first and
    /// its synchronous mutation completes before stop closes admission, or
    /// closing/removal is already recorded and the command is rejected before it
    /// touches terminal state. A check-then-dispatch caller would leave a window
    /// for the runtime to stop between the two.
    ///
    /// Limits (approved): the guarantee starts at host-observed closing, not at
    /// physical process death; admitted bytes are not revoked; the full PTY
    /// process execution is not serialized. The closure must be synchronous —
    /// no await may run while the guard is held.
    pub fn with_live_terminal_target<T>(
        &self,
        expected: &RuntimeTarget,
        dispatch: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let _admission = self
            .inner
            .admission
            .lock()
            .map_err(|_| "Runtime admission lock poisoned".to_string())?;
        if self.is_closing(expected)? {
            return Err("the page runtime is stopping".to_string());
        }
        let live = self
            .inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?
            .get(&expected.instance_id)
            .and_then(|runtime| runtime.target.lock().ok().map(|target| target.clone()));
        if live.as_ref() != Some(expected) {
            return Err("the page runtime is not running".to_string());
        }
        let (registered, state) = {
            let coordinator = self
                .inner
                .coordinator
                .lock()
                .map_err(|_| "Runtime coordinator lock poisoned".to_string())?;
            // Identity first: `state_of` keys on the instance id alone, so a
            // rebind would otherwise report the replacement's state as this
            // target's liveness.
            (
                coordinator.validate(expected).is_ok(),
                coordinator.state_of(expected),
            )
        };
        if !registered {
            return Err("the page runtime is not registered".to_string());
        }
        if !matches!(
            state,
            Some(
                RuntimeState::Starting
                    | RuntimeState::Trusting
                    | RuntimeState::Ready
                    | RuntimeState::Working
                    | RuntimeState::Idle
                    | RuntimeState::Suspended
            )
        ) {
            return Err("the page runtime is not live".to_string());
        }
        Ok(dispatch())
    }

    /// Test seam: hold one stop inside the window where its tombstone is
    /// recorded but the runtime is still registered (plan §5 closing-but-
    /// registered evidence). `stop` calls the barrier synchronously, so a test
    /// blocks stop at exactly that boundary without timing sleeps.
    #[cfg(test)]
    fn hold_stop_after_admission(&self, target: &RuntimeTarget) {
        let barrier = self
            .inner
            .stop_barrier
            .lock()
            .ok()
            .and_then(|slot| slot.clone());
        if let Some(barrier) = barrier {
            barrier(target);
        }
    }

    #[cfg(test)]
    fn set_stop_barrier(&self, barrier: Option<StopBarrier>) {
        if let Ok(mut slot) = self.inner.stop_barrier.lock() {
            *slot = barrier;
        }
    }
    #[cfg(test)]
    fn set_crash_barrier(&self, barrier: Option<CrashBarrier>) {
        if let Ok(mut slot) = self.inner.crash_barrier.lock() {
            *slot = barrier;
        }
    }

    pub fn cleanup_trace(&self) -> Vec<(String, NativeCleanupStage)> {
        self.inner
            .cleanup_trace
            .lock()
            .map(|trace| trace.clone())
            .unwrap_or_default()
    }

    pub fn set_ephemeral_registry(&self, registry: Arc<EphemeralRegistry>) {
        if let Ok(mut current) = self.inner.ephemeral_registry.lock() {
            *current = Some(registry);
        }
    }

    /// The owner registry this manager asks whether a runtime's current owner
    /// may be reattached (plan D11/T7). Installed once at boot, like the
    /// ephemeral registry: "is this owner still live?" is the registry's
    /// question, and a second copy of the answer here is exactly the drift the
    /// plan forbids.
    pub fn set_owner_registry(&self, registry: Arc<crate::window_owner::WindowOwnerRegistry>) {
        if let Ok(mut current) = self.inner.owner_registry.lock() {
            *current = Some(registry);
        }
    }

    /// Create a native ephemeral chat (Side/Quick) through the shared
    /// `EphemeralRegistry` — the single lifecycle authority (decision:
    /// reuse, Dr. Lin 2026-08-30). The registry mints instance identity and
    /// enforces per-owner quotas; commit happens only after a successful
    /// spawn, and the existing owner/transition/process-exit cleanup paths
    /// become effective for natively created chats. A failed spawn rolls
    /// the reservation back so no orphan record survives.
    pub fn spawn_ephemeral(
        &self,
        workspace_id: &str,
        session_id: &str,
        spec: NativeLaunchSpec,
        owner: &crate::window_owner::OwnerId,
        kind: crate::ephemeral_registry::EphemeralKind,
        transition_generation: u64,
    ) -> Result<RuntimeTarget, String> {
        let kind_matches = match kind {
            crate::ephemeral_registry::EphemeralKind::SideChat => {
                spec.runtime_type == NativeRuntimeType::SideChat
            }
            crate::ephemeral_registry::EphemeralKind::QuickChat => {
                spec.runtime_type == NativeRuntimeType::QuickChat
            }
            crate::ephemeral_registry::EphemeralKind::Config => {
                spec.runtime_type == NativeRuntimeType::Config
            }
        };
        if !kind_matches {
            return Err("Ephemeral kind must match the launch spec runtime type".into());
        }
        let registry = self
            .inner
            .ephemeral_registry
            .lock()
            .map_err(|_| "Native ephemeral registry lock poisoned".to_string())?
            .clone()
            .ok_or_else(|| {
                "Native ephemeral creation requires the shared ephemeral registry".to_string()
            })?;
        let reservation = registry
            .reserve_create(owner, kind)
            .map_err(|error| format!("Ephemeral reservation rejected: {error}"))?;
        self.spawn_ephemeral_committed(
            reservation,
            workspace_id,
            session_id,
            spec,
            transition_generation,
        )
    }

    /// Spawn + commit an existing reservation. Split from [`Self::spawn_ephemeral`]
    /// so replacement flows can reserve a candidate while the old instance stays
    /// live (transactional Quick Chat replacement): the registry's candidate-cancel
    /// path restores the old record when spawn/commit fails here.
    pub fn spawn_ephemeral_committed(
        &self,
        reservation: crate::ephemeral_registry::CreateReservation,
        workspace_id: &str,
        session_id: &str,
        spec: NativeLaunchSpec,
        transition_generation: u64,
    ) -> Result<RuntimeTarget, String> {
        let registry = self
            .inner
            .ephemeral_registry
            .lock()
            .map_err(|_| "Native ephemeral registry lock poisoned".to_string())?
            .clone()
            .ok_or_else(|| {
                "Native ephemeral creation requires the shared ephemeral registry".to_string()
            })?;
        let canonical_cwd = spec.cwd.clone();
        let cleanup_resources = spec.cleanup.clone();
        let target = RuntimeTarget::with_owner(
            workspace_id,
            session_id,
            reservation.instance_id.clone(),
            reservation.owner_id.as_str(),
            transition_generation,
        );
        if let Err(error) = self.spawn(target.clone(), spec) {
            if let Ok(Some(lease)) = registry.begin_close(
                &reservation.owner_id,
                &reservation.instance_id,
                reservation.generation,
            ) {
                registry.finish_cleanup(&lease);
            }
            return Err(error);
        }
        let pid = {
            let runtimes = self
                .inner
                .runtimes
                .lock()
                .map_err(|_| "Native runtime registry lock poisoned".to_string())?;
            let Some(runtime) = runtimes.get(&target.instance_id) else {
                return Err("Native runtime vanished before ephemeral commit".into());
            };
            runtime
                .process
                .as_ref()
                .and_then(|process| process.pid())
                .unwrap_or_default()
        };
        registry
            .commit_ready(
                &reservation,
                crate::ephemeral_registry::OwnedProcess {
                    // Native runtimes speak RPC over stdio; there is no HTTP
                    // port. Zero marks the field unused for native records.
                    port: 0,
                    pid,
                    child_identity: pid as u64,
                    canonical_cwd,
                    transition_generation,
                    temporary_directory: cleanup_resources.temporary_directory,
                },
            )
            .map_err(|error| {
                // Commit rejected (e.g. stale reservation): do not leave a
                // half-owned runtime behind.
                let _ = self.stop(&target);
                format!("Ephemeral commit rejected: {error}")
            })?;
        Ok(target)
    }

    /// Owner binding is host-derived and carried by RuntimeTarget.
    /// Revokes owner generation before stopping processes. This also blocks
    /// stale secret-bearing launch specs from being admitted for that owner.
    pub fn stop_for_owner(&self, owner_id: &str) {
        let targets = self
            .inner
            .runtimes
            .lock()
            .map(|runtimes| {
                runtimes
                    .values()
                    .filter_map(|runtime| runtime.target.lock().ok().map(|target| target.clone()))
                    .filter(|target| target.owner_id.as_deref() == Some(owner_id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let generation = targets
            .iter()
            .map(|target| target.workspace_generation)
            .max()
            .unwrap_or_default();
        for target in targets {
            let _ = self.stop(&target);
        }
        if let Ok(mut revoked) = self.inner.revoked_owners.lock() {
            revoked.insert(owner_id.to_string(), generation);
        }
        if let Ok(mut secrets) = self.inner.secret_generations.lock() {
            secrets.remove(owner_id);
        }
        self.stop_owner_ephemeral(owner_id);
    }

    /// Rebind one resident runtime to a new owner and generation (plan D11/B1).
    ///
    /// This is the reattach counterpart of [`Self::rebind_owner_generation`]:
    /// the app reclaimed the runtime after its previous owner was relinquished
    /// (a destroyed window, or a dead attachment), so both the owner stamp and
    /// the generation move — atomically, with the coordinator record as the
    /// truth and the runtimes map mirrored from it.
    ///
    /// Refuses unless the runtime is currently owned by `from_owner` **and**
    /// that owner is reattachable per the owner registry: a live attachment's
    /// runtime is never stolen. The registry decides, so this check cannot
    /// drift from the capability/label truth; with no registry installed the
    /// refusal is total (fail closed).
    pub fn rebind_runtime_owner(
        &self,
        from_owner: &str,
        to_owner: &str,
        workspace_id: &str,
        session_id: &str,
        generation: u64,
    ) -> bool {
        let Some(registry) = self
            .inner
            .owner_registry
            .lock()
            .ok()
            .and_then(|registry| registry.clone())
        else {
            return false;
        };
        if !registry.owner_is_reattachable(&crate::window_owner::OwnerId::from_string(
            from_owner.to_string(),
        )) {
            return false;
        }
        // Identity mutation shares the terminal admission guard (plan §5, B2):
        // a dispatch admitted under the old identity must finish before the
        // owner/generation the page captured can change, and a stop that won the
        // guard must not be revived by this rebind.
        let Ok(_admission) = self.inner.admission.lock() else {
            return false;
        };
        let current = {
            let Ok(runtimes) = self.inner.runtimes.lock() else {
                return false;
            };
            let Some(found) = runtimes.values().find(|runtime| {
                let Ok(target) = runtime.target.lock() else {
                    return false;
                };
                target.workspace_id == workspace_id
                    && target.session_id == session_id
                    && target.owner_id.as_deref() == Some(from_owner)
            }) else {
                return false;
            };
            let Ok(target) = found.target.lock() else {
                return false;
            };
            target.clone()
        };
        if self.is_closing(&current).unwrap_or(true) {
            return false;
        }
        // Coordinator record first (identity-validated), then mirror into the
        // runtimes map — the same dual bookkeeping as bind_session_id.
        let formal = {
            let Ok(mut coordinator) = self.inner.coordinator.lock() else {
                return false;
            };
            match coordinator.rebind_owner(&current, to_owner, generation) {
                Ok(formal) => formal,
                Err(_) => return false,
            }
        };
        let Ok(mut runtimes) = self.inner.runtimes.lock() else {
            return false;
        };
        if let Some(runtime) = runtimes.get_mut(&current.instance_id) {
            if let Ok(mut target) = runtime.target.lock() {
                *target = formal.clone();
            }
        }
        drop(runtimes);
        // The reattaching owner now holds this runtime's secret generation,
        // exactly as a spawn would have recorded it.
        if let Ok(mut secrets) = self.inner.secret_generations.lock() {
            secrets.insert(to_owner.to_string(), generation);
        }
        if let Ok(mut owners) = self.inner.owners.lock() {
            owners.insert(to_owner.to_string());
        }
        true
    }

    /// Owner-scoped ephemeral teardown (plan T7/R5): every ephemeral runtime
    /// this owner still holds is **stopped for real** (process group killed and
    /// reaped) before its lease is finalized and its temporary directory is
    /// cleaned up.
    ///
    /// Resident workspace/session runtimes are deliberately left alone: this is
    /// the process half of `detach_owner`, not a window-destroy sweep. Leases
    /// that name no live runtime (already stopped, or a request-only record)
    /// still get their lease and temp dir finalized, which is why the stop
    /// outcome is not a precondition for the cleanup below.
    pub fn stop_owner_ephemeral(&self, owner_id: &str) {
        let Some(registry) = self
            .inner
            .ephemeral_registry
            .lock()
            .ok()
            .and_then(|registry| registry.clone())
        else {
            return;
        };
        let owner = crate::window_owner::OwnerId::from_string(owner_id.to_string());
        let leases = registry.owner_cleanup(&owner);
        for lease in leases {
            if let Some(target) = self.target_for_instance_id(&lease.instance_id) {
                let _ = self.stop(&target);
            }
            if let Some((path, token)) = lease.temporary_directory.as_ref() {
                let _ = cleanup_quick_chat_dir(&canonical_temp_root(), path, token);
            }
            registry.finish_cleanup(&lease);
        }
    }

    pub fn stop_for_workspace_transition(&self, workspace_id: &str) {
        self.stop_workspace(workspace_id);
    }

    /// Stop old owner-bound runtimes and revoke all old-generation ephemeral
    /// leases. Caller must pass generation from WindowOwnerRegistry; workspace
    /// IDs alone are not an authorization boundary during navigation.
    /// Re-stamp one runtime's workspace generation (owner-bound). Used by
    /// workspace transitions that REUSE a live runtime: the commit sweep
    /// stops every runtime with `generation < transition`, so the reused
    /// target must carry the post-transition generation to survive it.
    pub fn rebind_owner_generation(
        &self,
        owner_id: &str,
        session_id: &str,
        generation: u64,
    ) -> bool {
        // Identity mutation shares the terminal admission guard (plan §5, B2).
        // Segment order under it: closing → release, runtimes → release,
        // coordinator → release, runtimes mirror → release. Never nested.
        let Ok(_admission) = self.inner.admission.lock() else {
            return false;
        };
        // Read the live identity first (runtimes map and coordinator are in
        // sync here; validate below must see the pre-rebind generation).
        let current = {
            let Ok(runtimes) = self.inner.runtimes.lock() else {
                return false;
            };
            let Some(found) = runtimes.values().find(|runtime| {
                let Ok(target) = runtime.target.lock() else {
                    return false;
                };
                target.session_id == session_id && target.owner_id.as_deref() == Some(owner_id)
            }) else {
                return false;
            };
            let Ok(target) = found.target.lock() else {
                return false;
            };
            target.clone()
        };
        // A stop that already closed admission is final: rebinding the identity
        // it closed would hand the stopped targets' pages a live-looking runtime.
        if self.is_closing(&current).unwrap_or(true) {
            return false;
        }
        // Coordinator record first (identity-validated), then mirror into the
        // runtimes map — same dual bookkeeping as bind_session_id.
        let formal = {
            let Ok(mut coordinator) = self.inner.coordinator.lock() else {
                return false;
            };
            match coordinator.rebind_generation(&current, generation) {
                Ok(formal) => formal,
                Err(_) => return false,
            }
        };
        let Ok(mut runtimes) = self.inner.runtimes.lock() else {
            return false;
        };
        if let Some(runtime) = runtimes.get_mut(&current.instance_id) {
            if let Ok(mut target) = runtime.target.lock() {
                *target = formal.clone();
            }
        }
        true
    }

    pub fn stop_for_owner_transition(&self, owner_id: &str, transition_generation: u64) {
        let targets = self
            .inner
            .runtimes
            .lock()
            .map(|runtimes| {
                runtimes
                    .values()
                    .filter_map(|runtime| runtime.target.lock().ok().map(|target| target.clone()))
                    .filter(|target| {
                        target.owner_id.as_deref() == Some(owner_id)
                            && target.workspace_generation < transition_generation
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for target in targets {
            let _ = self.stop(&target);
        }
        // Old-generation admission is gone; its secret generation must die
        // with it instead of lingering until the next owner spawn.
        if let Ok(mut secrets) = self.inner.secret_generations.lock() {
            secrets.remove(owner_id);
        }
        if let Some(registry) = self
            .inner
            .ephemeral_registry
            .lock()
            .ok()
            .and_then(|registry| registry.clone())
        {
            let owner = crate::window_owner::OwnerId::from_string(owner_id.to_string());
            for lease in registry.cleanup_for_transition(&owner, transition_generation) {
                if let Some((path, token)) = lease.temporary_directory.as_ref() {
                    let _ = cleanup_quick_chat_dir(&canonical_temp_root(), path, token);
                }
                registry.finish_cleanup(&lease);
            }
        }
    }
    pub fn stop_for_window_destroy(&self, workspace_id: &str) {
        self.stop_workspace(workspace_id);
    }

    pub fn stop_for_app_exit(&self) {
        self.stop_all();
        let owners = self
            .inner
            .owners
            .lock()
            .map(|owners| owners.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for owner in owners {
            self.stop_owner_ephemeral(&owner);
        }
    }

    pub fn stop_workspace(&self, workspace_id: &str) {
        let targets = self
            .inner
            .runtimes
            .lock()
            .map(|runtimes| {
                runtimes
                    .values()
                    .filter_map(|runtime| runtime.target.lock().ok().map(|target| target.clone()))
                    .filter(|target| target.workspace_id == workspace_id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for target in targets {
            let _ = self.stop(&target);
        }
    }

    pub fn stop_all(&self) {
        let targets = self
            .inner
            .runtimes
            .lock()
            .map(|runtimes| {
                runtimes
                    .values()
                    .filter_map(|runtime| runtime.target.lock().ok().map(|target| target.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for target in targets {
            let _ = self.stop(&target);
        }
    }

    pub fn target_for_session(
        &self,
        workspace_id: &str,
        session_id: &str,
    ) -> Option<RuntimeTarget> {
        self.inner
            .runtimes
            .lock()
            .ok()?
            .values()
            .find(|runtime| {
                runtime.target.lock().is_ok_and(|target| {
                    target.workspace_id == workspace_id && target.session_id == session_id
                })
            })
            .and_then(|runtime| runtime.target.lock().ok().map(|target| target.clone()))
    }

    pub fn target_for_session_id(&self, session_id: &str) -> Option<RuntimeTarget> {
        self.inner
            .runtimes
            .lock()
            .ok()?
            .values()
            .find(|runtime| {
                runtime
                    .target
                    .lock()
                    .is_ok_and(|target| target.session_id == session_id)
            })
            .and_then(|runtime| runtime.target.lock().ok().map(|target| target.clone()))
    }

    /// The runtime registered under one instance id (ephemeral leases carry the
    /// instance id, not the session id).
    pub fn target_for_instance_id(&self, instance_id: &str) -> Option<RuntimeTarget> {
        self.inner
            .runtimes
            .lock()
            .ok()?
            .get(instance_id)
            .and_then(|runtime| runtime.target.lock().ok().map(|target| target.clone()))
    }

    pub fn bind_session_id(
        &self,
        temporary: &RuntimeTarget,
        session_id: &str,
    ) -> Result<RuntimeTarget, String> {
        if !temporary.session_id.starts_with("temporary-") {
            return Ok(temporary.clone());
        }
        // Session identity is an admission identity component (plan §5, B2):
        // the rebind runs in the same guard as terminal dispatch.
        let _admission = self
            .inner
            .admission
            .lock()
            .map_err(|_| "Runtime admission lock poisoned".to_string())?;
        if self
            .is_closing(temporary)
            .map_err(|_| "Runtime stop registry lock poisoned".to_string())?
        {
            return Err("Native runtime is stopping".to_string());
        }
        let mut coordinator = self
            .inner
            .coordinator
            .lock()
            .map_err(|_| "Runtime coordinator lock poisoned".to_string())?;
        let binding_event = coordinator
            .emit_event(
                temporary,
                serde_json::json!({
                    "type": "session_bound",
                    "sessionId": session_id,
                }),
            )
            .map_err(|error| format!("Cannot sequence session binding: {error:?}"))?;
        let formal = coordinator
            .bind_session_id(temporary, session_id)
            .map_err(|error| format!("Cannot bind formal session: {error:?}"))?;
        drop(coordinator);
        let runtime = self
            .inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?;
        let managed = runtime
            .get(&temporary.instance_id)
            .ok_or_else(|| "Native runtime instance is not running".to_string())?;
        *managed
            .target
            .lock()
            .map_err(|_| "Native runtime target lock poisoned".to_string())? = formal.clone();
        // The session identity just changed (temporary -> formal); any retained
        // session file belongs to the old identity and must not answer for the
        // new one. The next get_state re-populates it.
        if let Ok(mut session_file) = managed.session_file.lock() {
            *session_file = None;
        }
        drop(runtime);
        let _ = self.inner.events.send(NativeRuntimeEvent {
            target: binding_event.target,
            sequence: binding_event.sequence,
            event: binding_event.event,
        });
        Ok(formal)
    }

    /// Targets of all running native runtimes (host compat surface).
    pub fn running_targets(&self) -> Vec<RuntimeTarget> {
        self.inner
            .runtimes
            .lock()
            .map(|runtimes| {
                runtimes
                    .values()
                    .filter_map(|runtime| runtime.target.lock().ok().map(|target| target.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Retain the host-validated Pi `sessionFile` for a running instance so
    /// `runtime_instance_summaries` can read it without a bucket rescan. The
    /// caller owns validation (canonical, inside the session root, `.jsonl`).
    ///
    /// Identity guard: an `instance_id` can be reused after the entry it named
    /// is stopped and rebuilt (restart, re-adoption, a rebuilt entry). A
    /// detached probe captured its target *before* that replacement, so writing
    /// by `instance_id` alone could stamp the previous session's file onto the
    /// new entry. Only write when the live entry still names the same session
    /// identity (`workspace_id` + `session_id`). The workspace generation is
    /// deliberately not compared: a normal transition bumps it while the entry
    /// — and the file it retained — stay valid.
    pub fn note_session_file(
        &self,
        target: &RuntimeTarget,
        session_file: PathBuf,
    ) -> Result<(), String> {
        let runtimes = self
            .inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?;
        let managed = runtimes
            .get(&target.instance_id)
            .ok_or_else(|| "Native runtime instance is not running".to_string())?;
        {
            let current = managed
                .target
                .lock()
                .map_err(|_| "Native runtime target lock poisoned".to_string())?;
            if current.workspace_id != target.workspace_id
                || current.session_id != target.session_id
            {
                return Err("Native runtime session identity has changed".to_string());
            }
        }
        *managed
            .session_file
            .lock()
            .map_err(|_| "Native session file lock poisoned".to_string())? = Some(session_file);
        Ok(())
    }

    /// The retained Pi `sessionFile` for a running instance, when one is known
    /// and still present on disk. `None` means the caller must fall back to the
    /// ordinary bucket lookup.
    pub fn session_file_for(&self, target: &RuntimeTarget) -> Option<PathBuf> {
        let runtimes = self.inner.runtimes.lock().ok()?;
        let managed = runtimes.get(&target.instance_id)?;
        let retained = managed.session_file.lock().ok()?.clone()?;
        retained.is_file().then_some(retained)
    }

    /// Process id of a running native runtime, when present.
    pub fn pid_for(&self, target: &RuntimeTarget) -> Option<u32> {
        let runtimes = self.inner.runtimes.lock().ok()?;
        let runtime = runtimes.get(&target.instance_id)?;
        let process = runtime.process.as_ref()?;
        process.pid()
    }

    pub fn snapshot(&self, target: &RuntimeTarget) -> Result<RuntimeSnapshot, String> {
        self.inner
            .coordinator
            .lock()
            .map_err(|_| "Runtime coordinator lock poisoned".to_string())?
            .snapshot(target)
            .map_err(|error| format!("Runtime snapshot rejected: {error:?}"))
    }

    pub async fn respond_extension_ui(
        &self,
        target: &RuntimeTarget,
        response: Value,
    ) -> Result<(), String> {
        self.inner
            .coordinator
            .lock()
            .map_err(|_| "Runtime coordinator lock poisoned".to_string())?
            .validate(target)
            .map_err(|error| format!("Extension UI response rejected: {error:?}"))?;
        if response.get("type").and_then(Value::as_str) != Some("extension_ui_response") {
            return Err("Expected extension_ui_response".into());
        }
        let bridge = self
            .inner
            .runtimes
            .lock()
            .map_err(|_| "Native runtime registry lock poisoned".to_string())?
            .get(&target.instance_id)
            .map(|runtime| runtime.bridge.clone())
            .ok_or_else(|| "Native runtime instance is not running".to_string())?;
        let response_id = response
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        bridge
            .send_frame(response)
            .await
            .map_err(|error| format!("Cannot send extension UI response: {error:?}"))?;
        if let Some(response_id) = response_id {
            let mut pending = self
                .inner
                .pending_ui
                .lock()
                .map_err(|_| "Pending extension UI lock poisoned".to_string())?;
            if let Some(events) = pending.get_mut(&target.instance_id) {
                events.retain(|event| {
                    event.event.get("id").and_then(Value::as_str) != Some(response_id.as_str())
                });
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn configure_child_process(command: &mut Command) {
    crate::windows_child::hide_console(command);
}

#[cfg(unix)]
fn configure_child_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
fn configure_child_process(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::{
        apply_launch_environment, LaunchDescription, NativeLaunchSpec, NativePiManager,
        NativeRuntimeType, ReadinessPolicy, MCP_PROJECT_ROOT_ENV,
    };
    use crate::operation_registry::{OperationScope, OperationState};
    use crate::runtime_coordinator::RuntimeTarget;
    use crate::temp_resources::{canonical_temp_root, cleanup_quick_chat_dir};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Duration;

    // ── P1.11 real-Pi lifecycle smoke ────────────────────────────────
    // These spawn the real embedded Pi binary through the native manager —
    // no in-memory bridge. Ignored by default so `cargo test` stays fast
    // and hermetic; run them via `scripts/smoke-native-lifecycle.mjs` or
    // `cargo test --ignored native_smoke_ -- --nocapture`.

    async fn native_smoke_lifecycle(
        name: &str,
        runtime_type: NativeRuntimeType,
        session_path: Option<PathBuf>,
        no_tools: bool,
    ) {
        let static_dir = PathBuf::from(env!("PICOT_SRC_TAURI_DIR")).join("resources");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cwd = std::env::temp_dir().join(format!("picot-native-smoke-{name}-{nonce}"));
        std::fs::create_dir_all(&cwd).unwrap();
        let mut spec = crate::pi_launch::native_launch_spec_for(
            &static_dir,
            runtime_type,
            &cwd,
            session_path.as_deref(),
        )
        .expect("native launch spec for smoke");
        spec.no_tools = no_tools;

        let manager = NativePiManager::new(32);
        let mut events = manager.subscribe();
        let target = RuntimeTarget::new(
            format!("smoke-{name}-workspace"),
            format!("smoke-{name}-session"),
            format!("smoke-{name}-instance"),
        );
        manager
            .spawn(target.clone(), spec)
            .expect("real Pi spawn must succeed");

        let first = tokio::time::timeout(Duration::from_secs(60), events.recv())
            .await
            .expect("real Pi must emit a runtime event within 60s")
            .expect("event stream must stay open");
        assert_eq!(first.target.instance_id, target.instance_id);

        // A read-only RPC round trip proves the assembled args/environment
        // produce a working Pi, not just a process that started.
        let state = manager
            .request(
                &target,
                json!({ "type": "get_state" }),
                None,
                Duration::from_secs(15),
            )
            .await
            .expect("get_state round trip over the real bridge");
        assert!(state.is_object(), "unexpected get_state response: {state}");

        manager.stop(&target).expect("stop must succeed");
        // stop() is synchronous: Ok means the child was killed and reaped,
        // the coordinator recorded Stopped, and the instance was
        // unregistered. A stopped runtime is no longer addressable.
        assert!(
            manager.snapshot(&target).is_err(),
            "stopped runtime must no longer be addressable"
        );

        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[tokio::test]
    #[ignore = "real-Pi lifecycle smoke; run via scripts/smoke-native-lifecycle.mjs"]
    async fn native_smoke_primary() {
        native_smoke_lifecycle("primary", NativeRuntimeType::Primary, None, false).await;
    }

    #[tokio::test]
    #[ignore = "real-Pi lifecycle smoke; run via scripts/smoke-native-lifecycle.mjs"]
    async fn native_smoke_dedicated() {
        let session = std::env::temp_dir().join(format!(
            "picot-native-smoke-dedicated-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(&session, b"").unwrap();
        native_smoke_lifecycle(
            "dedicated",
            NativeRuntimeType::Dedicated,
            Some(session),
            false,
        )
        .await;
    }

    #[tokio::test]
    #[ignore = "real-Pi lifecycle smoke; run via scripts/smoke-native-lifecycle.mjs"]
    async fn native_smoke_side_chat() {
        native_smoke_lifecycle("side_chat", NativeRuntimeType::SideChat, None, false).await;
    }

    #[tokio::test]
    #[ignore = "real-Pi lifecycle smoke; run via scripts/smoke-native-lifecycle.mjs"]
    async fn native_smoke_quick_chat() {
        native_smoke_lifecycle("quick_chat", NativeRuntimeType::QuickChat, None, true).await;
    }

    #[tokio::test]
    #[ignore = "real-Pi lifecycle smoke; run via scripts/smoke-native-lifecycle.mjs"]
    async fn native_smoke_standby() {
        native_smoke_lifecycle("standby", NativeRuntimeType::Standby, None, false).await;
    }

    #[test]
    fn spawned_pi_starts_from_a_clean_project_root_marker() {
        let launch = LaunchDescription {
            program: PathBuf::from("/embedded/pi"),
            args: Vec::new(),
            environment: BTreeMap::from([("PATH".into(), "/usr/bin".into())]),
            safe_environment: BTreeMap::new(),
            runtime_type: NativeRuntimeType::Config,
            readiness: ReadinessPolicy::default(),
        };
        let mut command = Command::new(&launch.program);
        apply_launch_environment(&mut command, &launch);
        let envs: Vec<_> = command.get_envs().collect();
        assert!(
            envs.contains(&(std::ffi::OsStr::new(MCP_PROJECT_ROOT_ENV), None)),
            "an inherited marker must be removed before the owned environment is applied"
        );
        assert!(envs.contains(&(
            std::ffi::OsStr::new("PATH"),
            Some(std::ffi::OsStr::new("/usr/bin"))
        )));
    }

    #[test]
    fn launch_spec_has_no_tcp_port_and_resumes_only_at_process_start() {
        let spec = NativeLaunchSpec {
            binary: PathBuf::from("/embedded/pi"),
            cwd: PathBuf::from("/workspace"),
            session_path: Some(PathBuf::from("/sessions/a.jsonl")),
            extensions: vec![PathBuf::from("/extensions/picot-bridge.mjs")],
            pi_version: "0.80.10".into(),
            path_env: "/usr/bin".into(),
            agent_root: None,
            static_dir: None,
            install_secret: None,
            runtime_type: super::NativeRuntimeType::Primary,
            no_tools: false,
            readiness: super::ReadinessPolicy::default(),
            cleanup: super::NativeCleanupResources::default(),
            mcp_project_root: None,
        };
        let launch = spec.command_description();
        assert_eq!(launch.program, PathBuf::from("/embedded/pi"));
        assert!(launch.args.windows(2).any(|pair| pair == ["--mode", "rpc"]));
        assert!(launch
            .args
            .windows(2)
            .any(|pair| pair == ["--session", "/sessions/a.jsonl"]));
        assert!(!launch.environment.contains_key("PI_STUDIO_PORT"));
        assert!(!launch
            .args
            .iter()
            .any(|argument| argument.parse::<u16>().is_ok()));
    }

    #[test]
    fn launch_spec_keeps_subagent_tool_active_from_start() {
        let spec = NativeLaunchSpec {
            binary: PathBuf::from("/embedded/pi"),
            cwd: PathBuf::from("/workspace"),
            session_path: None,
            extensions: vec![PathBuf::from("/extensions/picot-bridge.mjs")],
            pi_version: "0.86.1".into(),
            path_env: "/usr/bin".into(),
            agent_root: None,
            static_dir: None,
            install_secret: None,
            runtime_type: super::NativeRuntimeType::Primary,
            no_tools: false,
            readiness: super::ReadinessPolicy::default(),
            cleanup: super::NativeCleanupResources::default(),
            mcp_project_root: None,
        };
        let launch = spec.command_description();
        assert!(launch
            .args
            .windows(2)
            .any(|pair| pair == ["--exclude-tools", "subagents_enable"]));
    }

    #[tokio::test]
    async fn eof_crashes_exact_instance_and_marks_pending_operation_indeterminate() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-a", "session-a", "instance-a");
        let scope = OperationScope::new("owner-a", "workspace-a", "session-a", 1);
        let mut events = manager.subscribe();
        let mut fake = manager.register_in_memory(target.clone()).unwrap();
        let request = tokio::spawn({
            let manager = manager.clone();
            let target = target.clone();
            let scope = scope.clone();
            async move {
                manager
                    .request_scoped(
                        &target,
                        scope,
                        json!({ "type": "prompt" }),
                        "crash-intent",
                        Duration::from_secs(5),
                    )
                    .await
            }
        });
        fake.read_request().await.unwrap();
        fake.close().await;
        assert_eq!(
            events.recv().await.unwrap().event["type"],
            "runtime_crashed"
        );
        assert_eq!(
            events.recv().await.unwrap().event["type"],
            "snapshot_required"
        );
        assert!(request.await.unwrap().is_err());
        let states = manager
            .inner
            .operations
            .lock()
            .unwrap()
            .states_for_instance("instance-a");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].1, OperationState::Indeterminate);
    }

    #[tokio::test]
    async fn child_exit_crashes_runtime_and_emits_snapshot_required() {
        let cwd =
            std::env::temp_dir().join(format!("picot-native-child-exit-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let spec = NativeLaunchSpec {
            binary: PathBuf::from("/bin/sh"),
            cwd: cwd.clone(),
            session_path: None,
            extensions: Vec::new(),
            pi_version: "test".into(),
            path_env: "/usr/bin:/bin".into(),
            agent_root: None,
            static_dir: None,
            install_secret: None,
            runtime_type: NativeRuntimeType::Primary,
            no_tools: false,
            readiness: super::ReadinessPolicy::default(),
            cleanup: super::NativeCleanupResources::default(),
            mcp_project_root: None,
        };
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-exit", "session-exit", "instance-exit");
        let mut events = manager.subscribe();
        // The shell exits immediately because native launch flags are invalid
        // for it; this exercises observer child-exit handling without mocks.
        manager.spawn(target, spec).unwrap();
        let crash = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = events.recv().await.unwrap();
                if event.event["type"] == "runtime_crashed" {
                    break event;
                }
            }
        })
        .await
        .expect("child exit crash event");
        assert_eq!(crash.event["reason"], "runtime_crashed");
        assert_eq!(
            events.recv().await.unwrap().event["type"],
            "snapshot_required"
        );
        assert!(
            manager.target_for_session_id("session-exit").is_none(),
            "a crashed child must not remain reusable"
        );
        let _ = std::fs::remove_dir_all(cwd);
    }

    // ── Terminal admission guard (plan 2026-11-03 §5) ───────────────
    // Barriers, not sleeps: every ordering below is decided by a channel
    // handshake, so a passing test proves the lock order, not the timing.

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_admission_rejects_an_unregistered_target() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let ran = std::sync::atomic::AtomicBool::new(false);
        let outcome = manager.with_live_terminal_target(&target, || {
            ran.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(
            outcome.is_err(),
            "an unregistered runtime is never admitted"
        );
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_admission_rejects_a_foreign_owner() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        let expected = RuntimeTarget::with_owner("ws", "s", "instance-a", "someone-else", 1);
        assert!(
            manager.with_live_terminal_target(&expected, || ()).is_err(),
            "the owner is server-derived and part of the identity"
        );
        // The exact target is admitted, so the rejection above is not a blanket
        // refusal.
        assert!(manager.with_live_terminal_target(&target, || ()).is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_admission_rejects_a_stopped_or_closing_target() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        manager.stop(&target).unwrap();
        assert!(
            manager.with_live_terminal_target(&target, || ()).is_err(),
            "a stopped runtime is not live"
        );
        // `stop_first_closing_but_registered_rejects_admission_and_rebind` covers
        // the closing-but-registered window through the real stop path; a
        // re-registration here would prove tombstone retention only.
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_admission_rejects_a_crashed_target_that_is_still_registered() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        super::emit_runtime_crashed(&manager.inner, &target, "runtime_crashed");
        // Crash keeps the registry entry; only the coordinator state changes.
        assert!(manager
            .inner
            .runtimes
            .lock()
            .unwrap()
            .contains_key(&target.instance_id));
        assert!(manager.with_live_terminal_target(&target, || ()).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_admission_rejects_a_replacement_instance_in_the_same_session() {
        let manager = NativePiManager::in_memory(8);
        let first = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(first.clone()).unwrap();
        manager.stop(&first).unwrap();
        let second = RuntimeTarget::with_owner("ws", "s", "instance-b", "owner", 1);
        let _replacement = manager.register_in_memory(second.clone()).unwrap();
        assert!(manager.with_live_terminal_target(&first, || ()).is_err());
        assert!(manager.with_live_terminal_target(&second, || ()).is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_dispatch_wins_the_guard_then_stop_closes_admission() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
        let (inside_tx, inside_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();

        let dispatching = std::thread::spawn({
            let manager = manager.clone();
            let target = target.clone();
            let log = std::sync::Arc::clone(&log);
            move || {
                manager
                    .with_live_terminal_target(&target, || {
                        log.lock().unwrap().push("dispatch");
                        inside_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    })
                    .unwrap();
            }
        });
        inside_rx.recv().unwrap();
        // Stop cannot close admission while the dispatch is inside the guard.
        let stopping = std::thread::spawn({
            let manager = manager.clone();
            let target = target.clone();
            let log = std::sync::Arc::clone(&log);
            move || {
                manager.stop(&target).unwrap();
                log.lock().unwrap().push("stop");
            }
        });
        // One admitted command, then the tombstone: the order is a property of
        // the guard, not of the scheduling above.
        release_tx.send(()).unwrap();
        dispatching.join().unwrap();
        stopping.join().unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["dispatch", "stop"]);
        assert!(manager.with_live_terminal_target(&target, || ()).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_wins_the_guard_and_the_mutation_never_runs() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
        let (inside_tx, inside_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let first = std::thread::spawn({
            let manager = manager.clone();
            let target = target.clone();
            let log = std::sync::Arc::clone(&log);
            move || {
                manager
                    .with_live_terminal_target(&target, || {
                        log.lock().unwrap().push("first");
                        inside_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    })
                    .unwrap();
            }
        });
        inside_rx.recv().unwrap();
        let stopping = std::thread::spawn({
            let manager = manager.clone();
            let target = target.clone();
            move || {
                manager.stop(&target).unwrap();
            }
        });
        release_tx.send(()).unwrap();
        first.join().unwrap();
        stopping.join().unwrap();
        assert!(manager
            .with_live_terminal_target(&target, || log.lock().unwrap().push("second"))
            .is_err());
        assert_eq!(*log.lock().unwrap(), vec!["first"]);
    }

    /// B2 (event pump identity): a rebind that lands between spawn and exit must
    /// be attributed to the identity the host currently publishes, and the
    /// removal must be for that same identity — not for the target captured when
    /// the pump started. Stale-capture case only; the narrower window inside
    /// `crash_and_remove` is covered by
    /// `crash_and_remove_holds_admission_across_identity_capture_and_removal`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_exit_after_a_rebind_is_attributed_to_the_live_identity() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let mut events = manager.subscribe();
        let mut fake = manager.register_in_memory(target.clone()).unwrap();
        // The resident runtime is rebound while its process is still alive.
        assert!(manager.rebind_owner_generation("owner", "s", 2));
        let rebound = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 2);

        fake.close().await;
        let crashed = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("the pump reports the exit")
            .expect("event channel alive");
        assert_eq!(crashed.event["type"], "runtime_crashed");
        assert_eq!(
            crashed.target, rebound,
            "the exit belongs to the identity the host publishes"
        );
        // Removal ran for that identity: the registry entry is gone, and a
        // stale stop of the pre-rebind target cannot revive or re-close it.
        assert!(manager
            .inner
            .runtimes
            .lock()
            .unwrap()
            .get(&target.instance_id)
            .is_none());
        // The rebound identity is not admitted either, and nothing runs for it.
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(manager
            .with_live_terminal_target(&rebound, || ran
                .store(true, std::sync::atomic::Ordering::SeqCst))
            .is_err());
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "a dead runtime mutates nothing after the rebind"
        );
    }

    /// B2 barrier: the crash path captures the identity and then removes it in
    /// one admission, so a rebind offered while it is parked in between cannot
    /// land — that window used to make the crash *and* the removal both refuse,
    /// leaving a dead runtime registered and admitted under the new generation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn crash_and_remove_holds_admission_across_identity_capture_and_removal() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let mut events = manager.subscribe();
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<RuntimeTarget>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = std::sync::Mutex::new(release_rx);
        manager.set_crash_barrier(Some(Arc::new(move |captured| {
            entered_tx.send(captured.clone()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        })));
        let exiting = std::thread::spawn({
            let manager = manager.clone();
            let instance_id = target.instance_id.clone();
            move || super::crash_and_remove(&manager.inner, &instance_id)
        });
        let captured = entered_rx.recv().unwrap();
        assert_eq!(captured, target, "the captured identity is the live one");
        // Parked between capture and removal, the guard is still held: no rebind
        // can slip in, which is exactly what the pre-fix code allowed.
        assert!(
            manager.inner.admission.try_lock().is_err(),
            "capture, crash and removal are one admission"
        );
        release_tx.send(()).unwrap();
        exiting.join().unwrap();
        manager.set_crash_barrier(None);
        let crashed = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("the crash is reported")
            .expect("event channel alive");
        assert_eq!(crashed.event["type"], "runtime_crashed");
        assert_eq!(crashed.target, captured);
        assert!(
            manager
                .inner
                .runtimes
                .lock()
                .unwrap()
                .get(&target.instance_id)
                .is_none(),
            "the crash path removed the runtime it captured"
        );
    }

    /// Genuine stop-first ordering (plan §5): stop is parked in the window where
    /// its tombstone is recorded but the runtime is still registered. No
    /// dispatch is inside the guard before it, so admission — not scheduling —
    /// is what rejects, and identity rebinding cannot revive the closed target.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_first_closing_but_registered_rejects_admission_and_rebind() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = std::sync::Mutex::new(release_rx);
        manager.set_stop_barrier(Some(Arc::new(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        })));
        let stopping = std::thread::spawn({
            let manager = manager.clone();
            let target = target.clone();
            move || manager.stop(&target)
        });
        entered_rx.recv().unwrap();
        // Closing *and* still registered: the registry entry is present, so a
        // registry-presence check alone would admit this command.
        assert!(manager
            .inner
            .runtimes
            .lock()
            .unwrap()
            .contains_key(&target.instance_id));
        let outcome = manager.with_live_terminal_target(&target, || {
            ran.store(true, std::sync::atomic::Ordering::SeqCst)
        });
        assert!(outcome.is_err(), "closing-but-registered must reject");
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        // A generation rebind in this window would hand the stopped page a
        // live-looking identity, so it must refuse.
        assert!(!manager.rebind_owner_generation("owner", "s", 9));
        release_tx.send(()).unwrap();
        stopping.join().unwrap().unwrap();
        assert!(manager.with_live_terminal_target(&target, || ()).is_err());
    }

    /// Dispatch-first ordering across an identity rebind: the admitted mutation
    /// finished before the rebuilt identity became visible, and the stale
    /// target is refused afterwards.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dispatch_first_then_rebind_waits_for_the_admitted_mutation() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        let log = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
        let (inside_tx, inside_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let dispatching = std::thread::spawn({
            let manager = manager.clone();
            let target = target.clone();
            let log = Arc::clone(&log);
            move || {
                manager
                    .with_live_terminal_target(&target, || {
                        log.lock().unwrap().push("dispatch");
                        inside_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        log.lock().unwrap().push("mutated");
                    })
                    .unwrap();
            }
        });
        inside_rx.recv().unwrap();
        let rebinding = std::thread::spawn({
            let manager = manager.clone();
            let log = Arc::clone(&log);
            move || {
                let rebound = manager.rebind_owner_generation("owner", "s", 2);
                log.lock().unwrap().push("rebind");
                rebound
            }
        });
        // The rebind cannot enter while the dispatch holds admission: probe the
        // guard itself instead of waiting and hoping the rebind was slow.
        assert!(
            manager.inner.admission.try_lock().is_err(),
            "the admitted mutation still holds admission"
        );
        assert_eq!(*log.lock().unwrap(), vec!["dispatch"]);
        release_tx.send(()).unwrap();
        dispatching.join().unwrap();
        assert!(rebinding.join().unwrap(), "the rebind applies after it");
        assert_eq!(*log.lock().unwrap(), vec!["dispatch", "mutated", "rebind"]);
        let rebound = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 2);
        assert!(
            manager
                .with_live_terminal_target(&target, || log.lock().unwrap().push("stale"))
                .is_err(),
            "the page's old generation is no longer the live identity"
        );
        assert!(manager.with_live_terminal_target(&rebound, || ()).is_ok());
    }

    /// Rebind-first ordering: the rebuilt identity exists before the stale
    /// command arrives, so the command rejects with zero mutation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rebind_first_rejects_the_stale_dispatch_without_mutation() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        assert!(manager.rebind_owner_generation("owner", "s", 2));
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(manager
            .with_live_terminal_target(&target, || ran
                .store(true, std::sync::atomic::Ordering::SeqCst))
            .is_err());
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        assert!(manager
            .with_live_terminal_target(
                &RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 2),
                || ()
            )
            .is_ok());
    }

    /// A stop that won the guard is final: the exact tombstone keeps admission
    /// closed even if the identity is rebound afterwards.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stopped_target_cannot_be_revived_by_a_rebind() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        manager.stop(&target).unwrap();
        // The runtime is gone from the registry, so nothing is there to rebind.
        assert!(!manager.rebind_owner_generation("owner", "s", 2));
        assert!(manager.with_live_terminal_target(&target, || ()).is_err());
    }

    /// R5: a wedged PTY write returns to its caller inside the bounded input
    /// deadline. Sequential on purpose — it proves the *return*, not that a
    /// concurrent stop gets in; `a_wedged_pty_write_releases_admission_so_a_concurrent_stop_completes`
    /// proves the concurrency.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_wedged_pty_write_returns_inside_the_input_deadline() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        let (terminals, owner, root, terminal_id, generation) =
            crate::terminal_manager::wedged_input_fixture();
        let payload = serde_json::json!({
            "type": "terminal_input",
            "terminalId": terminal_id,
            "generation": generation,
            "dataBase64": "cHJpbnRmIGhpCg==",
        });
        let started = std::time::Instant::now();
        let dispatched = manager
            .with_live_terminal_target(&target, || terminals.dispatch(&owner, &root, &payload))
            .expect("the target is live, so dispatch runs");
        assert!(dispatched.is_err(), "the wedged write fails explicitly");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "the guard was held for {:?}",
            started.elapsed()
        );
        // The guard is available again: stop closes admission and completes.
        manager.stop(&target).unwrap();
        assert!(manager.with_live_terminal_target(&target, || ()).is_err());
    }

    /// R5 concurrency, no ordering assumption: the writer reports entering its
    /// blocking write, the stop starts from that instant, and one deadline
    /// covers both — the input times out and gives admission up, and the stop
    /// completes, both while the writer is still stuck. Only then is it
    /// released.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_wedged_pty_write_releases_admission_so_a_concurrent_stop_completes() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::with_owner("ws", "s", "instance-a", "owner", 1);
        let _fake = manager.register_in_memory(target.clone()).unwrap();
        let ((terminals, owner, root, terminal_id, generation), wedged) =
            crate::terminal_manager::wedged_input_fixture_with_barrier();
        let payload = serde_json::json!({
            "type": "terminal_input",
            "terminalId": terminal_id,
            "generation": generation,
            "dataBase64": "cHJpbnRmIGhpCg==",
        });
        let (done_tx, done) = std::sync::mpsc::channel::<(&'static str, bool)>();
        let dispatching = std::thread::spawn({
            let manager = manager.clone();
            let target = target.clone();
            let done_tx = done_tx.clone();
            move || {
                let admitted = manager.with_live_terminal_target(&target, || {
                    terminals.dispatch(&owner, &root, &payload)
                });
                // The wedged write must surface as an explicit failure, not a
                // silent success that would look like delivered keystrokes.
                let _ = done_tx.send(("dispatch", matches!(admitted, Ok(Err(_)))));
            }
        });
        assert!(
            wedged.wait_entered(),
            "the writer never entered its blocking write"
        );
        let stopping = std::thread::spawn({
            let manager = manager.clone();
            let target = target.clone();
            move || {
                let stopped = manager.stop(&target).is_ok();
                let _ = done_tx.send(("stop", stopped));
            }
        });
        let deadline = std::time::Duration::from_secs(5);
        let started = std::time::Instant::now();
        let mut finished: Vec<&'static str> = Vec::new();
        while finished.len() < 2 {
            match done.recv_timeout(deadline.saturating_sub(started.elapsed())) {
                Ok((what, ok)) => {
                    assert!(ok, "{what} did not complete as expected");
                    finished.push(what);
                }
                Err(_) => panic!("only {finished:?} completed within {deadline:?}"),
            }
        }
        dispatching.join().unwrap();
        stopping.join().unwrap();
        // Both halves finished while the writer was still wedged: the input gave
        // admission up on its own deadline instead of the stop waiting on it.
        wedged.release();
        assert!(
            manager.with_live_terminal_target(&target, || ()).is_err(),
            "the stopped runtime is no longer admitted"
        );
    }

    #[tokio::test]
    async fn stop_cleans_owned_temporary_directory_idempotently() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-temp", "session-temp", "instance-temp");
        let fake = manager.register_in_memory(target.clone()).unwrap();
        let root = canonical_temp_root();
        let (path, token) = crate::temp_resources::create_quick_chat_temp_dir().unwrap();
        manager
            .inner
            .runtimes
            .lock()
            .unwrap()
            .get_mut(&target.instance_id)
            .unwrap()
            .cleanup
            .temporary_directory = Some((path.clone(), token));
        manager.stop(&target).unwrap();
        assert!(!path.exists());
        // The ownership guard makes a second cleanup attempt a harmless no-op;
        // stop itself is idempotent for the exact target.
        assert!(cleanup_quick_chat_dir(&root, &path, "unused").is_err());
        assert!(manager.stop(&target).is_ok());
        drop(fake);
    }

    #[tokio::test]
    async fn writer_fail_crashes_and_marks_in_flight_indeterminate() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-a", "session-a", "instance-a");
        let scope = OperationScope::new("owner-a", "workspace-a", "session-a", 1);
        let mut events = manager.subscribe();
        let mut fake = manager.register_in_memory(target.clone()).unwrap();
        // Kill only the host→child request pipe BEFORE any request flows: the
        // next host write fails immediately — the writer-fault class — while
        // the child→host direction stays open, so this is distinct from EOF.
        fake.fail_outbound();
        let result = manager
            .request_scoped(
                &target,
                scope,
                json!({ "type": "prompt" }),
                "crash-intent",
                Duration::from_secs(5),
            )
            .await;
        assert!(result.is_err());
        let crash = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("crash event within 5s")
            .expect("event stream open");
        assert_eq!(crash.event["type"], "runtime_crashed");
        let snapshot = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("snapshot_required within 5s")
            .expect("event stream open");
        assert_eq!(snapshot.event["type"], "snapshot_required");
        let states = manager
            .inner
            .operations
            .lock()
            .unwrap()
            .states_for_instance("instance-a");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].1, OperationState::Indeterminate);
    }

    #[tokio::test]
    async fn invalid_frames_surface_protocol_error_without_crashing() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-a", "session-a", "instance-a");
        let mut events = manager.subscribe();
        let mut fake = manager.register_in_memory(target.clone()).unwrap();
        // Pi 0.84.2 status frames can carry raw newlines, splitting a frame
        // into invalid halves. Each half is reported as a sequenced
        // protocol_error and the runtime keeps serving traffic — a garbled
        // status line must never read as a dead runtime.
        fake.write_raw("not-json\n".into()).await.unwrap();
        assert_eq!(events.recv().await.unwrap().event["type"], "protocol_error");
        assert!(
            manager.target_for_session_id("session-a").is_some(),
            "a garbled line must not unregister the runtime"
        );
        let _ = fake.write_raw("also-not-json\n".into()).await;
        assert_eq!(events.recv().await.unwrap().event["type"], "protocol_error");
    }

    #[tokio::test]
    async fn routes_native_requests_by_opaque_target_and_rejects_session_replacement() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-a", "session-a", "instance-a");
        let mut events = manager.subscribe();
        let mut fake = manager.register_in_memory(target.clone()).unwrap();

        fake.write_frame(json!({ "type": "agent_start" }))
            .await
            .unwrap();
        let event = events.recv().await.unwrap();
        assert_eq!(event.target, target);
        assert_eq!(event.sequence, 1);
        assert_eq!(event.event["type"], "agent_start");

        let request = tokio::spawn({
            let manager = manager.clone();
            let target = target.clone();
            async move {
                manager
                    .request(
                        &target,
                        json!({ "type": "get_state" }),
                        None,
                        Duration::from_secs(1),
                    )
                    .await
            }
        });
        let outbound = fake.read_request().await.unwrap();
        let id = outbound["id"].as_str().unwrap();
        fake.write_frame(json!({
            "id": id,
            "type": "response",
            "command": "get_state",
            "success": true
        }))
        .await
        .unwrap();
        assert!(request.await.unwrap().unwrap()["success"]
            .as_bool()
            .unwrap());

        let first_prompt = tokio::spawn({
            let manager = manager.clone();
            let target = target.clone();
            async move {
                manager
                    .request(
                        &target,
                        json!({ "type": "prompt", "message": "once" }),
                        Some("prompt-intent"),
                        Duration::from_secs(1),
                    )
                    .await
            }
        });
        let outbound = fake.read_request().await.unwrap();
        let id = outbound["id"].as_str().unwrap();
        fake.write_frame(json!({
            "id": id,
            "type": "response",
            "command": "prompt",
            "success": true
        }))
        .await
        .unwrap();
        let accepted = first_prompt.await.unwrap().unwrap();
        let duplicate = manager
            .request(
                &target,
                json!({ "type": "prompt", "message": "once" }),
                Some("prompt-intent"),
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(duplicate, accepted);
        assert!(fake.try_read_request().is_none());

        assert!(manager
            .request(
                &target,
                json!({ "type": "switch_session", "sessionPath": "/other.jsonl" }),
                Some("intent-1"),
                Duration::from_secs(1),
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn abort_live_prompt_with_real_pi_events_without_turn_id() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-a", "session-a", "instance-a");
        let scope = OperationScope::new("owner-a", "workspace-a", "session-a", 1);
        let mut fake = manager.register_in_memory(target.clone()).unwrap();
        let mut events = manager.subscribe();
        let prompt = tokio::spawn({
            let manager = manager.clone();
            let target = target.clone();
            let scope = scope.clone();
            async move {
                manager
                    .request_scoped(
                        &target,
                        scope,
                        json!({"type": "prompt", "message": "turn"}),
                        "intent",
                        Duration::from_secs(1),
                    )
                    .await
            }
        });
        let request = fake.read_request().await.unwrap();
        fake.write_frame(json!({"type": "agent_start"}))
            .await
            .unwrap();
        fake.write_frame(json!({"type": "turn_start"}))
            .await
            .unwrap();
        while events.recv().await.unwrap().event["type"] != "turn_start" {}
        fake.write_frame(
            json!({"id": request["id"], "type": "response", "command": "prompt", "success": true}),
        )
        .await
        .unwrap();
        prompt.await.unwrap().unwrap();

        let wrong_scope = OperationScope::new("other-owner", "workspace-a", "session-a", 1);
        assert_eq!(
            manager
                .abort_turn(&target, &wrong_scope, Duration::from_secs(1))
                .await
                .unwrap()["disposition"],
            "stale_turn"
        );
        assert!(fake.try_read_request().is_none());

        let abort = tokio::spawn({
            let manager = manager.clone();
            let target = target.clone();
            let scope = scope.clone();
            async move {
                manager
                    .abort_turn(&target, &scope, Duration::from_secs(1))
                    .await
            }
        });
        let outbound = tokio::time::timeout(Duration::from_secs(2), fake.read_request())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outbound["type"], "abort");
        assert!(outbound.get("turnId").is_none());
        fake.write_frame(
            json!({"id": outbound["id"], "type": "response", "command": "abort", "success": true}),
        )
        .await
        .unwrap();
        assert_eq!(abort.await.unwrap().unwrap()["success"], true);
    }

    #[tokio::test]
    async fn bare_abort_via_generic_request_path_reaches_pi() {
        // Quick/Side Chat forward_command routes commands through request(),
        // whose validate_command once rejected a turnId-less abort (the
        // fabricated-contract regression): the stop never left the host.
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-a", "session-a", "instance-a");
        let mut fake = manager.register_in_memory(target.clone()).unwrap();
        fake.write_frame(json!({ "type": "agent_start" }))
            .await
            .unwrap();

        let abort = tokio::spawn({
            let manager = manager.clone();
            let target = target.clone();
            async move {
                manager
                    .request(
                        &target,
                        json!({ "type": "abort" }),
                        Some("ep-1-quick-chat"),
                        Duration::from_secs(1),
                    )
                    .await
            }
        });
        let outbound = fake.read_request().await.unwrap();
        assert_eq!(outbound["type"], "abort");
        assert!(outbound.get("turnId").is_none());
        fake.write_frame(
            json!({"id": outbound["id"], "type": "response", "command": "abort", "success": true}),
        )
        .await
        .unwrap();
        assert_eq!(abort.await.unwrap().unwrap()["success"], true);
    }

    #[tokio::test]
    async fn stop_is_ordered_idempotent_and_rejects_stale_identity() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-a", "session-a", "instance-a");
        let stale = RuntimeTarget::new("workspace-b", "session-b", "instance-a");
        let mut events = manager.subscribe();
        let mut fake = manager.register_in_memory(target.clone()).unwrap();
        fake.write_frame(json!({ "type": "extension_ui_request", "id": "ui-1" }))
            .await
            .unwrap();
        assert_eq!(
            events.recv().await.unwrap().event["type"],
            "extension_ui_request"
        );

        manager.stop(&target).unwrap();
        let stopped = events.recv().await.unwrap();
        assert_eq!(stopped.target, target);
        assert_eq!(stopped.event["type"], "runtime_stopped");
        assert_eq!(stopped.sequence, 2);
        assert!(manager.stop(&target).is_ok());
        assert!(manager.stop(&stale).is_err());
        assert!(manager.pending_extension_ui(&target).is_err());
        assert_eq!(
            manager
                .cleanup_trace()
                .into_iter()
                .map(|(_, stage)| stage)
                .collect::<Vec<_>>(),
            vec![
                super::NativeCleanupStage::AdmissionClosed,
                super::NativeCleanupStage::OperationsSettled,
                super::NativeCleanupStage::ProcessTreeTerminated,
                super::NativeCleanupStage::ProcessReaped,
                super::NativeCleanupStage::TemporaryResourcesCleaned,
                super::NativeCleanupStage::RuntimeUnregistered,
            ]
        );
    }

    #[tokio::test]
    async fn owner_stop_cleans_registry_leases_and_is_idempotent() {
        let manager = NativePiManager::in_memory(8);
        let registry = std::sync::Arc::new(crate::ephemeral_registry::EphemeralRegistry::default());
        manager.set_ephemeral_registry(registry.clone());
        let owner = crate::window_owner::OwnerId::from_string("owner-stop".to_string());
        let reservation = registry
            .reserve_create(&owner, crate::ephemeral_registry::EphemeralKind::SideChat)
            .unwrap();
        registry
            .commit_ready(
                &reservation,
                crate::ephemeral_registry::OwnedProcess {
                    port: 6100,
                    pid: 6100,
                    child_identity: 6100,
                    canonical_cwd: PathBuf::from("/workspace"),
                    transition_generation: 1,
                    temporary_directory: None,
                },
            )
            .unwrap();
        let target =
            RuntimeTarget::with_owner("workspace-a", "session-a", "instance-a", owner.as_str(), 1);
        manager.register_in_memory(target).unwrap();
        manager.stop_for_owner(owner.as_str());
        manager.stop_for_owner(owner.as_str());
        assert!(registry.descriptors(&owner).is_empty());
    }

    #[test]
    fn transition_cleanup_rejects_stale_generation_at_owner_boundary() {
        let manager = NativePiManager::in_memory(8);
        let registry = std::sync::Arc::new(crate::ephemeral_registry::EphemeralRegistry::default());
        manager.set_ephemeral_registry(registry.clone());
        let owner = crate::window_owner::OwnerId::from_string("owner-transition".to_string());
        let reservation = registry
            .reserve_create(&owner, crate::ephemeral_registry::EphemeralKind::SideChat)
            .unwrap();
        registry
            .commit_ready(
                &reservation,
                crate::ephemeral_registry::OwnedProcess {
                    port: 6200,
                    pid: 6200,
                    child_identity: 6200,
                    canonical_cwd: PathBuf::from("/workspace"),
                    transition_generation: 2,
                    temporary_directory: None,
                },
            )
            .unwrap();
        manager.stop_for_owner_transition(owner.as_str(), 2);
        assert_eq!(registry.descriptors(&owner).len(), 1);
        manager.stop_for_owner_transition(owner.as_str(), 3);
        assert!(registry.descriptors(&owner).is_empty());
    }

    fn cat_ephemeral_spec(
        runtime_type: NativeRuntimeType,
        secret: Option<&str>,
    ) -> NativeLaunchSpec {
        NativeLaunchSpec {
            binary: PathBuf::from("/bin/cat"),
            cwd: std::env::temp_dir(),
            session_path: None,
            extensions: Vec::new(),
            pi_version: "test".to_string(),
            path_env: std::env::var("PATH").unwrap_or_default(),
            agent_root: None,
            static_dir: None,
            install_secret: secret.map(str::to_string),
            runtime_type,
            no_tools: runtime_type == NativeRuntimeType::QuickChat,
            readiness: super::ReadinessPolicy::default(),
            cleanup: super::NativeCleanupResources::default(),
            mcp_project_root: None,
        }
    }

    #[tokio::test]
    async fn spawn_ephemeral_rejects_kind_mismatch() {
        let manager = NativePiManager::in_memory(8);
        let owner = crate::window_owner::OwnerId::from_string("owner-a".into());
        let error = manager
            .spawn_ephemeral(
                "workspace-a",
                "session-a",
                cat_ephemeral_spec(NativeRuntimeType::SideChat, None),
                &owner,
                crate::ephemeral_registry::EphemeralKind::QuickChat,
                1,
            )
            .unwrap_err();
        assert!(error.contains("must match"));
    }

    #[tokio::test]
    async fn spawn_ephemeral_requires_shared_registry() {
        let manager = NativePiManager::in_memory(8);
        let owner = crate::window_owner::OwnerId::from_string("owner-a".into());
        let error = manager
            .spawn_ephemeral(
                "workspace-a",
                "session-a",
                cat_ephemeral_spec(NativeRuntimeType::SideChat, None),
                &owner,
                crate::ephemeral_registry::EphemeralKind::SideChat,
                1,
            )
            .unwrap_err();
        assert!(error.contains("shared ephemeral registry"));
    }

    #[tokio::test]
    async fn spawn_ephemeral_populates_shared_registry_and_cleans_on_transition() {
        let manager = NativePiManager::in_memory(8);
        let registry = std::sync::Arc::new(crate::ephemeral_registry::EphemeralRegistry::default());
        manager.set_ephemeral_registry(std::sync::Arc::clone(&registry));
        let owner = crate::window_owner::OwnerId::from_string("owner-a".into());
        let target = manager
            .spawn_ephemeral(
                "workspace-a",
                "session-a",
                cat_ephemeral_spec(NativeRuntimeType::SideChat, None),
                &owner,
                crate::ephemeral_registry::EphemeralKind::SideChat,
                2,
            )
            .unwrap();
        assert_eq!(target.owner_id.as_deref(), Some("owner-a"));
        assert_eq!(target.workspace_generation, 2);
        let descriptors = registry.descriptors(&owner);
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].instance_id, target.instance_id);

        // Transition to a newer generation stops the runtime and clears the
        // natively-created record through the shared authority.
        manager.stop_for_owner_transition("owner-a", 3);
        assert!(registry.descriptors(&owner).is_empty());
        assert!(manager.snapshot(&target).is_err());
    }

    #[tokio::test]
    async fn rebind_owner_generation_keeps_runtime_through_transition_sweep() {
        let manager = NativePiManager::in_memory(8);
        let registry = std::sync::Arc::new(crate::ephemeral_registry::EphemeralRegistry::default());
        manager.set_ephemeral_registry(std::sync::Arc::clone(&registry));
        let owner = crate::window_owner::OwnerId::from_string("owner-gen".into());
        let target = manager
            .spawn_ephemeral(
                "workspace-a",
                "session-gen",
                cat_ephemeral_spec(NativeRuntimeType::SideChat, Some("secret")),
                &owner,
                crate::ephemeral_registry::EphemeralKind::SideChat,
                1,
            )
            .unwrap();
        assert_eq!(target.workspace_generation, 1);

        // A transition to generation 3 sweeps everything below it; rebinding
        // the runtime to 3 first must keep it alive through the sweep. The
        // rebound identity (generation 3) is what callers must use from now
        // on — stale pre-rebind targets fail identity validation.
        assert!(manager.rebind_owner_generation("owner-gen", "session-gen", 3));
        manager.stop_for_owner_transition("owner-gen", 3);
        let rebound = manager
            .target_for_session_id("session-gen")
            .expect("rebound runtime survives the sweep");
        assert_eq!(rebound.workspace_generation, 3);
        assert!(
            manager.snapshot(&rebound).is_ok(),
            "rebound runtime is snapshot-able"
        );

        // A later transition (generation 5) still sweeps the rebound runtime.
        manager.stop_for_owner_transition("owner-gen", 5);
        assert!(
            manager.target_for_session_id("session-gen").is_none(),
            "stale runtime is swept later"
        );

        // Unknown session ids never rebind anything.
        assert!(!manager.rebind_owner_generation("owner-gen", "session-missing", 9));
        // Cross-owner rebind attempts are rejected.
        manager
            .spawn_ephemeral(
                "workspace-a",
                "session-other",
                cat_ephemeral_spec(NativeRuntimeType::SideChat, Some("secret")),
                &owner,
                crate::ephemeral_registry::EphemeralKind::SideChat,
                1,
            )
            .unwrap();
        assert!(!manager.rebind_owner_generation("owner-b", "session-other", 4));
    }

    #[tokio::test]
    async fn quick_chat_replacement_restores_old_when_candidate_spawn_fails() {
        let manager = NativePiManager::in_memory(8);
        let registry = std::sync::Arc::new(crate::ephemeral_registry::EphemeralRegistry::default());
        manager.set_ephemeral_registry(std::sync::Arc::clone(&registry));
        let owner = crate::window_owner::OwnerId::from_string("owner-a".into());
        let original = manager
            .spawn_ephemeral(
                "workspace-a",
                "session-1",
                cat_ephemeral_spec(NativeRuntimeType::QuickChat, Some("secret")),
                &owner,
                crate::ephemeral_registry::EphemeralKind::QuickChat,
                1,
            )
            .unwrap();

        // Reserve the replacement: old goes Replacing, candidate goes Creating.
        let replacement = registry.reserve_quick_replacement(&owner).unwrap();
        assert!(replacement.old_instance.is_some());

        // Candidate spawn fails (nonexistent binary); the committed path must
        // cancel the candidate reservation, restoring the old record so the
        // user's conversation survives the failure.
        let mut bad_spec = cat_ephemeral_spec(NativeRuntimeType::QuickChat, Some("secret"));
        bad_spec.binary = PathBuf::from("/nonexistent/picot-test-binary");
        let error = manager
            .spawn_ephemeral_committed(
                replacement.candidate.clone(),
                "workspace-a",
                "session-2",
                bad_spec,
                1,
            )
            .unwrap_err();
        assert!(!error.is_empty());

        let descriptors = registry.descriptors(&owner);
        assert_eq!(descriptors.len(), 1, "candidate must not linger");
        assert_eq!(descriptors[0].instance_id, original.instance_id);
        assert_eq!(
            descriptors[0].state,
            crate::ephemeral_registry::EphemeralState::Ready
        );

        // Happy path: a fresh replacement reservation joins as the second
        // record; closing the old identity then leaves exactly the
        // replacement. (The cancelled candidate reservation above stays
        // permanently stale — by design, a retry must re-reserve.)
        let retry = registry.reserve_quick_replacement(&owner).unwrap();
        assert_ne!(
            retry.candidate.instance_id,
            replacement.candidate.instance_id
        );
        let candidate = manager
            .spawn_ephemeral_committed(
                retry.candidate.clone(),
                "workspace-a",
                "session-3",
                cat_ephemeral_spec(NativeRuntimeType::QuickChat, Some("secret")),
                1,
            )
            .unwrap();
        assert_ne!(candidate.instance_id, original.instance_id);
        if let Some((old_id, old_generation)) = retry.old_instance.clone() {
            if let Ok(Some(lease)) = registry.begin_close(&owner, &old_id, old_generation) {
                let _ = manager.stop(&original);
                registry.finish_cleanup(&lease);
            }
        }
        let descriptors = registry.descriptors(&owner);
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].instance_id, candidate.instance_id);
    }

    #[tokio::test]
    async fn spawn_ephemeral_enforces_owner_quota() {
        let manager = NativePiManager::in_memory(8);
        let registry = std::sync::Arc::new(crate::ephemeral_registry::EphemeralRegistry::default());
        manager.set_ephemeral_registry(std::sync::Arc::clone(&registry));
        let owner = crate::window_owner::OwnerId::from_string("owner-a".into());
        manager
            .spawn_ephemeral(
                "workspace-a",
                "session-a",
                cat_ephemeral_spec(NativeRuntimeType::SideChat, None),
                &owner,
                crate::ephemeral_registry::EphemeralKind::SideChat,
                2,
            )
            .unwrap();
        // SIDE_CHAT_QUOTA is one per owner.
        let error = manager
            .spawn_ephemeral(
                "workspace-a",
                "session-b",
                cat_ephemeral_spec(NativeRuntimeType::SideChat, None),
                &owner,
                crate::ephemeral_registry::EphemeralKind::SideChat,
                2,
            )
            .unwrap_err();
        assert!(error.contains("quota"));
        assert_eq!(registry.descriptors(&owner).len(), 1);
    }

    #[tokio::test]
    async fn spawn_ephemeral_failure_rolls_back_reservation() {
        let manager = NativePiManager::in_memory(8);
        let registry = std::sync::Arc::new(crate::ephemeral_registry::EphemeralRegistry::default());
        manager.set_ephemeral_registry(std::sync::Arc::clone(&registry));
        let owner = crate::window_owner::OwnerId::from_string("owner-a".into());
        let mut spec = cat_ephemeral_spec(NativeRuntimeType::SideChat, None);
        spec.binary = PathBuf::from("/nonexistent/picot-p16-smoke");
        let error = manager
            .spawn_ephemeral(
                "workspace-a",
                "session-a",
                spec,
                &owner,
                crate::ephemeral_registry::EphemeralKind::SideChat,
                1,
            )
            .unwrap_err();
        assert!(error.contains("Cannot start"));
        assert!(registry.descriptors(&owner).is_empty());
    }

    #[tokio::test]
    async fn owner_transition_revokes_secret_generation() {
        let manager = NativePiManager::in_memory(8);
        let registry = std::sync::Arc::new(crate::ephemeral_registry::EphemeralRegistry::default());
        manager.set_ephemeral_registry(std::sync::Arc::clone(&registry));
        let owner = crate::window_owner::OwnerId::from_string("owner-a".into());
        manager
            .spawn_ephemeral(
                "workspace-a",
                "session-a",
                cat_ephemeral_spec(NativeRuntimeType::QuickChat, Some("secret-a")),
                &owner,
                crate::ephemeral_registry::EphemeralKind::QuickChat,
                2,
            )
            .unwrap();
        assert!(manager
            .inner
            .secret_generations
            .lock()
            .unwrap()
            .contains_key("owner-a"));
        manager.stop_for_owner_transition("owner-a", 3);
        assert!(!manager
            .inner
            .secret_generations
            .lock()
            .unwrap()
            .contains_key("owner-a"));
    }

    #[tokio::test]
    async fn mutation_replay_covers_all_three_acceptance_states() {
        let manager = NativePiManager::in_memory(8);
        let target = RuntimeTarget::new("workspace-a", "session-a", "instance-a");
        let scope = OperationScope::new("owner-a", "workspace-a", "session-a", 1);
        let mut fake = manager.register_in_memory(target.clone()).unwrap();

        // accepted_pending: the first send is accepted; completion delivers
        // its terminal response.
        let first = tokio::spawn({
            let manager = manager.clone();
            let target = target.clone();
            let scope = scope.clone();
            async move {
                manager
                    .request_scoped_receipt(
                        &target,
                        scope,
                        json!({ "type": "prompt", "message": "m" }),
                        "intent-replay",
                        Duration::from_secs(5),
                    )
                    .await
            }
        });
        let outbound = fake.read_request().await.unwrap();
        let id = outbound["id"].as_str().unwrap();

        // duplicate_pending: the same idempotency key while the first request
        // is still in flight reports the in-band pending acceptance — no
        // error, no second execution.
        let (_, pending_acceptance, pending_response) = manager
            .request_scoped_receipt(
                &target.clone(),
                scope.clone(),
                json!({ "type": "prompt", "message": "m" }),
                "intent-replay",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert!(matches!(
            pending_acceptance,
            crate::operation_registry::OperationAcceptance::DuplicatePending
        ));
        assert!(pending_response.is_none());

        fake.write_frame(json!({
            "id": id,
            "type": "response",
            "command": "prompt",
            "success": true
        }))
        .await
        .unwrap();
        let (first_id, first_acceptance, first_response) = first.await.unwrap().unwrap();
        assert!(matches!(
            first_acceptance,
            crate::operation_registry::OperationAcceptance::Accepted
        ));
        let first_response = first_response.expect("first send returns its terminal response");
        assert_eq!(first_response["success"], true);

        // duplicate_completed: after completion the same key replays the
        // recorded terminal response instead of executing again.
        let (third_id, third_acceptance, third_response) = manager
            .request_scoped_receipt(
                &target.clone(),
                scope.clone(),
                json!({ "type": "prompt", "message": "m" }),
                "intent-replay",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(third_id, first_id);
        assert!(matches!(
            third_acceptance,
            crate::operation_registry::OperationAcceptance::DuplicateCompleted
        ));
        assert_eq!(
            third_response.expect("completed duplicate replays terminal response"),
            first_response
        );
    }

    #[tokio::test]
    async fn binds_a_temporary_session_once_and_routes_future_events_to_the_formal_target() {
        let manager = NativePiManager::in_memory(8);
        let temporary = RuntimeTarget::new("workspace-a", "temporary-a", "instance-a");
        let mut events = manager.subscribe();
        let mut fake = manager.register_in_memory(temporary.clone()).unwrap();

        let formal = manager.bind_session_id(&temporary, "session-a").unwrap();
        let binding = events.recv().await.unwrap();
        assert_eq!(binding.target, temporary);
        assert_eq!(binding.event["type"], "session_bound");
        assert_eq!(binding.event["sessionId"], "session-a");
        assert_eq!(formal.instance_id, "instance-a");

        fake.write_frame(json!({ "type": "agent_start" }))
            .await
            .unwrap();
        let event = events.recv().await.unwrap();
        assert_eq!(event.target, formal);
        assert_eq!(manager.target_for_session_id("session-a"), Some(formal));
    }

    #[tokio::test]
    async fn retained_session_file_reads_back_and_clears_on_session_bind() {
        let manager = NativePiManager::in_memory(8);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file = std::env::temp_dir().join(format!("picot-retained-{nonce}.jsonl"));
        std::fs::write(&file, "{\"type\":\"session\",\"id\":\"a\"}\n").unwrap();
        let canonical = file.canonicalize().unwrap();

        let temporary = RuntimeTarget::new("workspace-a", "temporary-a", "instance-a");
        let _fake = manager.register_in_memory(temporary.clone()).unwrap();
        assert_eq!(manager.session_file_for(&temporary), None);

        manager
            .note_session_file(&temporary, canonical.clone())
            .unwrap();
        assert_eq!(
            manager.session_file_for(&temporary),
            Some(canonical.clone())
        );

        // A vanished file drops the retained value so summaries fall back to
        // the bucket lookup instead of reporting a deleted path.
        std::fs::remove_file(&file).unwrap();
        assert_eq!(manager.session_file_for(&temporary), None);

        // A rebound session identity must not inherit the previous file.
        std::fs::write(&file, "{\"type\":\"session\",\"id\":\"b\"}\n").unwrap();
        manager
            .note_session_file(&temporary, canonical.clone())
            .unwrap();
        let formal = manager.bind_session_id(&temporary, "session-a").unwrap();
        assert_eq!(manager.session_file_for(&formal), None);
        let _ = std::fs::remove_file(&file);
    }

    /// A detached probe captures its target before the entry it names is
    /// stopped and rebuilt under the same instance id. Its late write-back must
    /// not stamp the previous session's file onto the replacement entry.
    #[tokio::test]
    async fn retained_session_file_guard_rejects_a_stale_probe_after_replacement() {
        let manager = NativePiManager::in_memory(8);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let old_file = std::env::temp_dir().join(format!("picot-retained-stale-{nonce}.jsonl"));
        std::fs::write(&old_file, "{\"type\":\"session\",\"id\":\"old\"}\n").unwrap();
        let canonical_old = old_file.canonicalize().unwrap();

        let old =
            RuntimeTarget::with_owner("workspace-a", "session-old", "instance-a", "owner-a", 0);
        manager.register_in_memory(old.clone()).unwrap();
        manager
            .note_session_file(&old, canonical_old.clone())
            .unwrap();
        assert_eq!(manager.session_file_for(&old), Some(canonical_old.clone()));

        // The entry is rebuilt under the same instance id but a new session
        // identity, exactly what a restart/re-adoption produces.
        manager.stop(&old).unwrap();
        let replacement =
            RuntimeTarget::with_owner("workspace-a", "session-new", "instance-a", "owner-a", 0);
        manager.register_in_memory(replacement.clone()).unwrap();

        // The stale probe resolves late and tries to write its file back.
        assert!(
            manager
                .note_session_file(&old, canonical_old.clone())
                .is_err(),
            "a stale probe must be rejected"
        );
        assert_eq!(
            manager.session_file_for(&replacement),
            None,
            "the replacement entry must not inherit the previous session's file"
        );
        let _ = std::fs::remove_file(&old_file);
    }
}
