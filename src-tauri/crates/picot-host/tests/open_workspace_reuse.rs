// ABOUTME: Multi-window T1 slice 1: open_workspace resolves the persisted session identity
// ABOUTME: and reuses/focuses an existing runtime instead of always spawning (spec §6.2, MW-1).

//! Red tests for the `open_workspace` identity-resolution slice. The op today
//! mints a random `session-<uuid>` and spawns unconditionally; these tests pin
//! the spec §6.2 behavior: a named session that is already running is never
//! given a second runtime — a live holder yields the focus semantics
//! (`focusedExisting`), a detached holder's runtime is reattached, a foreign
//! session file fails closed, and a request without `sessionPath` keeps the
//! fresh-spawn behavior.
//!
//! Runtimes are real stub child processes (`sleep`), so "no second runtime" is
//! a manager-bookkeeping fact mirroring the process table (the same fixture
//! shape as `window_detach_reattach.rs`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::{DaemonConfig, DaemonState};
use picot_host::host_control::{ClientClass, ProgressSink, VerifiedClientContext};
use picot_host::host_control_dispatch::host_control_dispatch;
use picot_host::native_pi_manager::{
    NativeCleanupResources, NativeLaunchSpec, NativeRuntimeType, ReadinessPolicy,
};
use picot_host::pi_launch;
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::window_owner::{OwnerId, TemporaryKind};
use serde_json::{json, Value};

/// Records every workspace-window open the daemon requests; anything else is
/// unused by these tests and must not look plausible.
#[derive(Default)]
struct RecordingSurface {
    opened: Mutex<Vec<(RuntimeTarget, String)>>,
}

impl RecordingSurface {
    fn opened(&self) -> Vec<(RuntimeTarget, String)> {
        self.opened.lock().expect("surface lock").clone()
    }
}

#[async_trait::async_trait]
impl AppSurface for RecordingSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        target: &RuntimeTarget,
        owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        self.opened
            .lock()
            .expect("surface lock")
            .push((target.clone(), owner.to_string()));
        Ok(())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused in open-workspace-reuse tests".into())
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        Err("unused in open-workspace-reuse tests".into())
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    surface: Arc<RecordingSurface>,
    state: Arc<DaemonState>,
    static_dir: PathBuf,
    workspace_dir: PathBuf,
    workspace_id: String,
    session_file: PathBuf,
}

impl Fixture {
    async fn boot() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).expect("static dir");
        std::fs::write(static_dir.join("index.html"), "<h1>reuse</h1>").expect("index");
        // The stub Pi every launch in these tests resolves: stays alive until
        // killed, so runtime liveness is a process fact, not a call log.
        let pi_dir = temp.path().join("pi");
        std::fs::create_dir_all(&pi_dir).expect("pi dir");
        let stub = pi_dir.join("pi");
        std::fs::write(&stub, "#!/bin/sh\nexec sleep 300\n").expect("stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
                .expect("stub is executable");
        }

        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let workspace_dir = std::fs::canonicalize(&workspace_dir).expect("canonical workspace");

        let session_dir = temp.path().join("sessions");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        let session_file = session_dir.join("2026-10-17T00-00-00-000Z_saved.jsonl");
        std::fs::write(
            &session_file,
            format!(
                "{}\n{}\n",
                json!({
                    "type": "session",
                    "id": "saved",
                    "cwd": workspace_dir.to_string_lossy(),
                }),
                json!({ "type": "message", "message": { "role": "user", "content": "hello" } }),
            ),
        )
        .expect("session fixture");

        let surface = Arc::new(RecordingSurface::default());
        let state = Arc::new(
            DaemonState::start(
                DaemonConfig {
                    data_dir: temp.path().join("data"),
                    config_dir: temp.path().join("config"),
                    static_dir: static_dir.clone(),
                    bundle_identifier: "com.palandata.picot.test".to_string(),
                },
                surface.clone(),
            )
            .await
            .expect("the daemon host stack boots"),
        );
        let (row, added) = state
            .metadata
            .lock()
            .expect("metadata lock")
            .add_workspace(&workspace_dir)
            .expect("the workspace registers");
        assert!(added);
        Self {
            _temp: temp,
            surface,
            state,
            static_dir,
            workspace_dir,
            workspace_id: row.workspace_id,
            session_file,
        }
    }

    fn ctx(&self) -> VerifiedClientContext {
        VerifiedClientContext {
            client_id: 1,
            class: ClientClass::Native,
            owner_id: None,
        }
    }

    /// Drive one control op through the production dispatcher.
    async fn dispatch(&self, command: &str, args: Value) -> Result<Value, String> {
        let handler = host_control_dispatch(Arc::clone(&self.state));
        let canonical = json!({
            "type": "runtime_request",
            "command": { "type": command },
            "args": args,
        });
        let progress: ProgressSink = Arc::new(|_| {});
        handler(self.ctx(), canonical, progress).await
    }

    fn open_args(&self) -> Value {
        json!({
            "cwd": self.workspace_dir.to_string_lossy(),
        })
    }

    fn stub_spec(&self, cwd: &Path) -> NativeLaunchSpec {
        NativeLaunchSpec {
            binary: pi_launch::resolve_bundled_pi(&self.static_dir).expect("the stub pi resolves"),
            cwd: cwd.to_path_buf(),
            session_path: None,
            extensions: Vec::new(),
            pi_version: "test".to_string(),
            path_env: std::env::var("PATH").unwrap_or_default(),
            agent_root: None,
            static_dir: None,
            install_secret: None,
            runtime_type: NativeRuntimeType::Primary,
            no_tools: false,
            readiness: ReadinessPolicy {
                probe_required: false,
                timeout: std::time::Duration::from_secs(5),
            },
            cleanup: NativeCleanupResources::default(),
            mcp_project_root: None,
        }
    }

    /// A live window owner holding a label distinct from
    /// `native-workspace-<wid>` — the shape open_workspace's own mint expects
    /// to find free (a landing-entered owner, e.g.).
    fn live_owner(&self, label: &str) -> OwnerId {
        let (owner, _capability) = self
            .state
            .owner_registry
            .create_owner_with_workspace(
                label.to_string(),
                self.workspace_dir.clone(),
                0,
                "http://127.0.0.1:3001".to_string(),
                Some(self.workspace_id.clone()),
                TemporaryKind::DefaultStartup,
            )
            .expect("the holder owner mints");
        owner
    }

    /// Spawn a real resident runtime for the fixture session, exactly as the
    /// session-prepare path would have.
    fn spawn_stub_runtime(&self, session_id: &str, owner: &OwnerId) -> RuntimeTarget {
        let target = RuntimeTarget::with_owner(
            self.workspace_id.clone(),
            session_id.to_string(),
            format!("instance-{session_id}"),
            owner.as_str(),
            0,
        );
        let spec = self.stub_spec(&self.workspace_dir);
        self.state
            .runtimes
            .spawn(target.clone(), spec)
            .expect("the stub runtime spawns");
        target
    }
}

/// A live holder's session is never given a second runtime: the op returns the
/// focus semantics and spawns nothing (spec §6.2 Blocked branch).
#[tokio::test(flavor = "multi_thread")]
async fn open_workspace_reports_focus_semantics_for_a_live_holder_without_spawning() {
    let fixture = Fixture::boot().await;
    let holder = fixture.live_owner("native-landing");
    fixture.spawn_stub_runtime("saved", &holder);

    let mut args = fixture.open_args();
    args["sessionPath"] = json!(fixture.session_file.to_string_lossy());
    let result = fixture
        .dispatch("open_workspace", args)
        .await
        .expect("the op succeeds without spawning");

    assert_eq!(result["focusedExisting"], json!(true));
    assert_eq!(result["sessionId"], json!("saved"));
    let running = fixture.state.runtimes.running_targets();
    assert_eq!(running.len(), 1, "no second runtime for a live holder");
    assert_eq!(running[0].session_id, "saved", "identity did not drift");
    assert!(
        fixture.surface.opened().is_empty(),
        "Blocked opens no window"
    );
}

/// A detached holder's runtime is reattached to a fresh owner and the window
/// opens on the existing process: instance count and identity unchanged
/// (spec §6.2 Rebind branch).
#[tokio::test(flavor = "multi_thread")]
async fn open_workspace_reattaches_a_detached_holders_runtime() {
    let fixture = Fixture::boot().await;
    let holder = fixture.live_owner("native-landing");
    let existing = fixture.spawn_stub_runtime("saved", &holder);
    fixture.state.owner_registry.detach_owner(&holder);

    let mut args = fixture.open_args();
    args["sessionPath"] = json!(fixture.session_file.to_string_lossy());
    let result = fixture
        .dispatch("open_workspace", args)
        .await
        .expect("the op succeeds by reattaching");

    assert_eq!(result["sessionId"], json!("saved"));
    let running = fixture.state.runtimes.running_targets();
    assert_eq!(running.len(), 1, "the runtime is reused, not duplicated");
    assert_eq!(running[0].session_id, "saved", "identity did not drift");
    assert_eq!(
        running[0].instance_id, existing.instance_id,
        "the same instance is reused"
    );
    let opened = fixture.surface.opened();
    assert_eq!(opened.len(), 1, "the window opens on the reused runtime");
    assert_eq!(opened[0].0.session_id, "saved");
    assert_eq!(opened[0].0.instance_id, existing.instance_id);
    assert_ne!(
        opened[0].1,
        existing.owner_id.clone().unwrap_or_default(),
        "the window belongs to the freshly minted owner"
    );
}

/// A session file whose header workspace is not the target workspace fails
/// closed — no spawn (spec §6.2 step 1).
#[tokio::test(flavor = "multi_thread")]
async fn open_workspace_fails_closed_on_a_foreign_session_path() {
    let fixture = Fixture::boot().await;
    let foreign_dir = fixture._temp.path().join("other-workspace");
    std::fs::create_dir_all(&foreign_dir).expect("foreign dir");
    let foreign_dir = foreign_dir.canonicalize().expect("canonical foreign dir");
    let foreign_session = fixture._temp.path().join("foreign.jsonl");
    std::fs::write(
        &foreign_session,
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "id": "foreign",
                "cwd": foreign_dir.to_string_lossy(),
            }),
            json!({ "type": "message", "message": { "role": "user", "content": "elsewhere" } }),
        ),
    )
    .expect("foreign session fixture");

    let mut args = fixture.open_args();
    args["sessionPath"] = json!(foreign_session.to_string_lossy());
    let error = fixture
        .dispatch("open_workspace", args)
        .await
        .expect_err("a misowned session path is refused");

    assert_eq!(error, "Selected session is outside the target workspace");
    assert!(
        fixture.state.runtimes.running_targets().is_empty(),
        "nothing spawned for a refused path"
    );
}

/// Without a sessionPath the op keeps its fresh-spawn behavior: one new
/// runtime, a minted owner, a window open (the "no identity to resolve" arm).
#[tokio::test(flavor = "multi_thread")]
async fn open_workspace_without_a_session_still_spawns_fresh() {
    let fixture = Fixture::boot().await;

    let result = fixture
        .dispatch("open_workspace", fixture.open_args())
        .await
        .expect("the fresh open succeeds");

    let running = fixture.state.runtimes.running_targets();
    assert_eq!(running.len(), 1, "a fresh runtime spawned");
    assert_eq!(
        result["sessionId"],
        json!(running[0].session_id),
        "the response reports the spawned session"
    );
    assert_eq!(fixture.surface.opened().len(), 1, "the window opened");
}
