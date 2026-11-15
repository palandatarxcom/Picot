// ABOUTME: Reproduces the sidebar's remove → re-add → prepare sequence.
// ABOUTME: A workspace's live runtime outlives registry removal, so a re-add
// ABOUTME: mints a fresh uuid while the old runtime still holds the old one.

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

/// Minimal recording surface, mirroring open_workspace_reuse.rs: focus is
/// recorded, everything else these flows never reach is an explicit error.
#[derive(Default)]
struct RecordingSurface {
    focused: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AppSurface for RecordingSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &RuntimeTarget,
        _label: &str,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused in workspace-readd tests".into())
    }

    async fn focus_window(&self, label: &str) -> Result<(), String> {
        self.focused
            .lock()
            .expect("focused lock")
            .push(label.to_string());
        Ok(())
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in workspace-readd tests".into())
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in workspace-readd tests".into())
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        Ok(())
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused in workspace-readd tests".into())
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        Err("unused in workspace-readd tests".into())
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused in workspace-readd tests".into())
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        Err("unused in workspace-readd tests".into())
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        Err("unused in workspace-readd tests".into())
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused in workspace-readd tests".into())
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        Err("unused in workspace-readd tests".into())
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: Arc<DaemonState>,
    static_dir: PathBuf,
    workspace_dir: PathBuf,
    workspace_id: String,
}

impl Fixture {
    async fn boot() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).expect("static dir");
        std::fs::write(static_dir.join("index.html"), "<h1>readd</h1>").expect("index");
        // Stub Pi: stays alive until killed, matching production liveness.
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

        let surface = Arc::new(RecordingSurface::default());
        let state = Arc::new(
            DaemonState::start(
                DaemonConfig {
                    data_dir: temp.path().join("data"),
                    config_dir: temp.path().join("config"),
                    static_dir: static_dir.clone(),
                    bundle_identifier: "com.palandata.picot.test".to_string(),
                },
                surface,
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
            state,
            static_dir,
            workspace_dir,
            workspace_id: row.workspace_id,
        }
    }

    fn native_ctx(&self, owner: &OwnerId) -> VerifiedClientContext {
        VerifiedClientContext {
            client_id: 1,
            class: ClientClass::Native,
            owner_id: Some(owner.clone()),
            device_id: None,
        }
    }

    async fn dispatch_as(
        &self,
        owner: &OwnerId,
        command: &str,
        args: Value,
    ) -> Result<Value, String> {
        let handler = host_control_dispatch(Arc::clone(&self.state));
        let canonical = json!({
            "type": "runtime_request",
            "command": { "type": command },
            "args": args,
        });
        let progress: ProgressSink = Arc::new(|_| {});
        handler(self.native_ctx(owner), canonical, progress).await
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

    /// An owner whose window sits on the fixture workspace.
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
            .expect("the owner mints");
        owner
    }
}

/// The sidebar sequence that regressed: the re-added workspace must prepare a
/// fresh runtime even though its predecessor (on the deleted uuid) is still
/// alive and owned by the same owner.
#[tokio::test(flavor = "multi_thread")]
async fn prepare_serves_a_readded_workspace_whose_old_runtime_survives() {
    let fixture = Fixture::boot().await;

    // A second workspace (the one the user removes and re-adds).
    let readd_raw = fixture._temp.path().join("readded");
    std::fs::create_dir_all(&readd_raw).expect("re-add dir");
    let readd_dir = readd_raw.canonicalize().expect("canonical re-add dir");

    let (old_row, added) = fixture
        .state
        .metadata
        .lock()
        .expect("metadata lock")
        .add_workspace(&readd_dir)
        .expect("second workspace registers");
    assert!(added);

    // The window's owner sits on workspace A.
    let owner = fixture.live_owner("native-workspace-a");

    // The removed workspace keeps a live runtime bound to its OLD uuid —
    // cross-workspace semantics never stop it, and neither does removal.
    let orphan = RuntimeTarget::with_owner(
        old_row.workspace_id.clone(),
        "session-old".to_string(),
        "instance-old".to_string(),
        owner.as_str(),
        0,
    );
    fixture
        .state
        .runtimes
        .spawn(orphan, fixture.stub_spec(&readd_dir))
        .expect("the orphan runtime spawns");

    // Remove, then re-add: the registry mints a fresh uuid.
    fixture
        .dispatch_as(
            &owner,
            "workspace.remove",
            json!({ "workspaceId": old_row.workspace_id }),
        )
        .await
        .expect("remove succeeds");
    let readd = fixture
        .dispatch_as(
            &owner,
            "workspace.add",
            json!({ "path": readd_dir.to_string_lossy() }),
        )
        .await
        .expect("re-add succeeds");
    let new_id = readd["workspace"]["workspaceId"]
        .as_str()
        .expect("the re-added workspace id")
        .to_string();
    assert_ne!(new_id, old_row.workspace_id, "a re-add mints a fresh uuid");

    // The failing step in the field: prepare for the re-added workspace.
    let prepared = fixture
        .dispatch_as(
            &owner,
            "workspace_target_prepare",
            json!({
                "targetCwd": readd_dir.to_string_lossy(),
                "forceNewSession": true,
                "reuseExisting": false,
            }),
        )
        .await
        .expect("prepare must serve the re-added workspace");

    assert_eq!(prepared["targetWorkspaceId"], json!(new_id));
    assert_eq!(
        prepared["classification"], "cross",
        "the owner still sits on workspace A: {prepared}"
    );
    let generation = prepared["transitionGeneration"]
        .as_u64()
        .expect("generation");
    fixture
        .dispatch_as(
            &owner,
            "workspace_transition_commit",
            json!({ "transitionGeneration": generation }),
        )
        .await
        .expect("commit succeeds");

    // The orphan keeps running on the deleted uuid; the new runtime serves
    // the re-added one. Both are live facts.
    let running = fixture.state.runtimes.running_targets();
    assert!(
        running.iter().any(|t| t.workspace_id == new_id),
        "the prepared runtime is registered: {running:?}"
    );
    assert!(
        running.iter().any(|t| t.session_id == "session-old"),
        "the orphan survived the transition: {running:?}"
    );
}

/// A prepare that fails after `begin_workspace_transition` (here: the Pi
/// binary cannot execute) must not strand the transition — the sidebar's
/// pattern is "click again on failure", and a stranded transition turns
/// every retry into "a workspace transition is already in progress" until
/// something else clears it. Field symptom: the 3rd/4th workspace needing
/// two clicks.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_prepare_does_not_strand_the_transition() {
    let fixture = Fixture::boot().await;

    let readd_raw = fixture._temp.path().join("readd");
    std::fs::create_dir_all(&readd_raw).expect("re-add dir");
    let readd_dir = readd_raw.canonicalize().expect("canonical re-add dir");
    fixture
        .state
        .metadata
        .lock()
        .expect("metadata lock")
        .add_workspace(&readd_dir)
        .expect("second workspace registers");

    let owner = fixture.live_owner("native-workspace-a");

    // Break the stub Pi so `runtimes.spawn` fails AFTER the transition began.
    let stub = fixture._temp.path().join("pi").join("pi");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o000))
            .expect("stub breaks");
    }

    let first = fixture
        .dispatch_as(
            &owner,
            "workspace_target_prepare",
            json!({
                "targetCwd": readd_dir.to_string_lossy(),
                "forceNewSession": true,
                "reuseExisting": false,
            }),
        )
        .await;
    assert!(first.is_err(), "the broken stub fails the spawn: {first:?}");

    // Restore the stub: the immediate retry must succeed. With a stranded
    // transition it would answer "already in progress" instead.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("stub restored");
    }
    let second = fixture
        .dispatch_as(
            &owner,
            "workspace_target_prepare",
            json!({
                "targetCwd": readd_dir.to_string_lossy(),
                "forceNewSession": true,
                "reuseExisting": false,
            }),
        )
        .await
        .expect("the retry prepares instead of hitting a stranded transition");
    let generation = second["transitionGeneration"].as_u64().expect("generation");
    fixture
        .dispatch_as(
            &owner,
            "workspace_transition_commit",
            json!({ "transitionGeneration": generation }),
        )
        .await
        .expect("commit succeeds after the compensated failure");
}

/// The field regression sequence: adding several FRESH workspaces back to
/// back (add → prepare → commit each). Every launch must succeed first-try;
/// a transition left behind by any path turns every later add into two
/// clicks ("a workspace transition is already in progress").
#[tokio::test(flavor = "multi_thread")]
async fn adding_several_fresh_workspaces_succeeds_first_try_each_time() {
    let fixture = Fixture::boot().await;
    let owner = fixture.live_owner("native-workspace-a");

    for index in 1..=3 {
        let dir_raw = fixture._temp.path().join(format!("fresh-{index}"));
        std::fs::create_dir_all(&dir_raw).expect("fresh dir");
        let dir = dir_raw.canonicalize().expect("canonical fresh dir");

        fixture
            .dispatch_as(
                &owner,
                "workspace.add",
                json!({ "path": dir.to_string_lossy() }),
            )
            .await
            .expect("add succeeds");
        let prepared = fixture
            .dispatch_as(
                &owner,
                "workspace_target_prepare",
                json!({
                    "targetCwd": dir.to_string_lossy(),
                    "forceNewSession": true,
                    "reuseExisting": false,
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("prepare #{index} failed: {error}"));
        let generation = prepared["transitionGeneration"]
            .as_u64()
            .expect("generation");
        fixture
            .dispatch_as(
                &owner,
                "workspace_transition_commit",
                json!({ "transitionGeneration": generation }),
            )
            .await
            .unwrap_or_else(|error| panic!("commit #{index} failed: {error}"));
    }
}
