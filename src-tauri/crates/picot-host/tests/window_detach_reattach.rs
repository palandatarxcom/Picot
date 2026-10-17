// ABOUTME: Task 7 tests: window close detaches without stopping resident runtimes, and
// ABOUTME: detached/dead-attachment owners reattach to the same runtime instance.

//! Plan Task 7 Step 1 (red first): the window-destroy semantics matrix a/b/b2/c/d.
//!
//! Everything here runs against a **real** `DaemonState` (the production host
//! stack), a **real** loopback `AdminServer` and a real WS client. The Pi
//! runtimes are real child processes backed by a `sleep` stub that *is* the
//! binary `pi_launch` resolves (written into the test static dir's sibling
//! `pi/pi`), so "the runtime is still alive" / "the process is gone and reaped"
//! are process-liveness facts, never bookkeeping assertions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use picot_host::child_supervision;
use picot_host::daemon::admin::{AdminConfig, AdminServer, ADMIN_WS_PATH};
use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::{DaemonConfig, DaemonState};
use picot_host::ephemeral_registry::EphemeralKind;
use picot_host::host_control::{ClientClass, ProgressSink, VerifiedClientContext};
use picot_host::host_control_dispatch::{host_control_dispatch, plan_prepare_reuse, PrepareReuse};
use picot_host::native_pi_manager::{
    NativeCleanupResources, NativeLaunchSpec, NativeRuntimeType, ReadinessPolicy,
};
use picot_host::pi_launch;
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::window_owner::OwnerId;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A surface that fails every call: none of these tests exercise the app
/// surface, so a call is a bug and must not look plausible.
#[derive(Default)]
struct NeverCalledSurface;

#[async_trait::async_trait]
impl AppSurface for NeverCalledSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &RuntimeTarget,
        _label: &str,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Err("unused in window-detach tests".into())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused in window-detach tests".into())
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in window-detach tests".into())
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in window-detach tests".into())
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        Err("unused in window-detach tests".into())
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused in window-detach tests".into())
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        Err("unused in window-detach tests".into())
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused in window-detach tests".into())
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        Err("unused in window-detach tests".into())
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        Err("unused in window-detach tests".into())
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused in window-detach tests".into())
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        Err("unused in window-detach tests".into())
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: Arc<DaemonState>,
    admin: AdminServer,
    token: String,
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
        std::fs::write(static_dir.join("index.html"), "<h1>window detach</h1>").expect("index");
        // The stub Pi: `resolve_bundled_pi` looks for `<static_dir>/../pi/pi`,
        // so this script IS what every launch resolution in these tests finds.
        // It ignores Pi's argv and stays alive until it is killed — which is
        // what makes process liveness the evidence, not a call log.
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
        let session_file = session_dir.join("2026-10-15T00-00-00-000Z_saved.jsonl");
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

        let state = Arc::new(
            DaemonState::start(
                DaemonConfig {
                    data_dir: temp.path().join("data"),
                    config_dir: temp.path().join("config"),
                    static_dir: static_dir.clone(),
                    bundle_identifier: "com.palandata.picot.test".to_string(),
                },
                Arc::new(NeverCalledSurface),
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
        let token = "test-admin-token-0123456789abcdef".to_string();
        let admin = AdminServer::start(AdminConfig::new(token.clone()))
            .await
            .expect("the admin listener binds");
        state.install_admin(admin.clone());
        Self {
            _temp: temp,
            state,
            admin,
            token,
            static_dir,
            workspace_dir,
            workspace_id: row.workspace_id,
            session_file,
        }
    }

    fn admin_ws_url(&self) -> String {
        format!(
            "ws://{}{ADMIN_WS_PATH}",
            self.admin.origin().trim_start_matches("http://")
        )
    }

    fn native_ctx(&self, owner: &OwnerId) -> VerifiedClientContext {
        VerifiedClientContext {
            client_id: 1,
            class: ClientClass::Native,
            owner_id: Some(owner.clone()),
        }
    }

    /// Drive one control op through the production dispatcher (no app, no
    /// WebSocket): the same code path the WebView's `runtime_request` reaches.
    async fn dispatch(
        &self,
        ctx: &VerifiedClientContext,
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
        handler(ctx.clone(), canonical, progress).await
    }

    fn stub_spec(
        &self,
        cwd: &Path,
        runtime_type: NativeRuntimeType,
        cleanup: NativeCleanupResources,
    ) -> NativeLaunchSpec {
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
            runtime_type,
            no_tools: false,
            readiness: ReadinessPolicy {
                probe_required: false,
                timeout: Duration::from_secs(5),
            },
            cleanup,
            mcp_project_root: None,
        }
    }

    /// Spawn a real resident runtime (a live stub process) for a workspace and
    /// session, exactly as the session-prepare path would have.
    fn spawn_stub_runtime(
        &self,
        workspace_id: &str,
        cwd: &Path,
        session_id: &str,
        owner: &OwnerId,
    ) -> RuntimeTarget {
        let target = RuntimeTarget::with_owner(
            workspace_id,
            session_id,
            format!("instance-{session_id}"),
            owner.as_str(),
            0,
        );
        let spec = self.stub_spec(cwd, NativeRuntimeType::Primary, Default::default());
        self.state
            .runtimes
            .spawn(target.clone(), spec)
            .expect("the stub runtime spawns");
        target
    }

    /// Spawn an owner-scoped ephemeral runtime (Quick Chat) through the
    /// production ephemeral path, carrying a real temp directory so the
    /// detach-side cleanup has something to remove.
    fn spawn_stub_ephemeral(&self, owner: &OwnerId) -> (RuntimeTarget, PathBuf) {
        let token = format!("token-{}", std::process::id());
        // Mirrors `temp_resources::QUICK_CHAT_TEMP_PREFIX` (a private const):
        // the cleanup path only deletes a directory carrying that prefix plus
        // the ownership token, so the fixture must build the same name.
        let temp_dir = picot_host::temp_resources::canonical_temp_root()
            .join(format!("picot-quick-chat-{token}"));
        std::fs::create_dir_all(&temp_dir).expect("ephemeral temp dir");
        let cleanup = NativeCleanupResources {
            temporary_directory: Some((temp_dir.clone(), token)),
            oauth_generation_supported: false,
        };
        let spec = self.stub_spec(&self.workspace_dir, NativeRuntimeType::QuickChat, cleanup);
        let target = self
            .state
            .runtimes
            .spawn_ephemeral(
                &self.workspace_id,
                "ephemeral-quick-1",
                spec,
                owner,
                EphemeralKind::QuickChat,
                0,
            )
            .expect("the ephemeral runtime spawns");
        (target, temp_dir)
    }

    /// Connect and complete the manager handshake.
    async fn attach(&self) -> (Client, Value) {
        let (mut socket, _) = tokio_tungstenite::connect_async(self.admin_ws_url())
            .await
            .expect("a real admin client connects");
        send_json(
            &mut socket,
            json!({
                "type": "admin_hello",
                "adminToken": self.token,
                "role": "manager",
                "appVersion": "0.5.0",
                "buildId": "test-build",
                "pid": std::process::id(),
            }),
        )
        .await;
        let welcome = loop {
            let value = next_json(&mut socket).await;
            if value["type"] == "admin_welcome" {
                break value;
            }
        };
        (socket, welcome)
    }

    /// `owner.create` with the workspace binding (the window a workspace opens
    /// with) or without it (a landing-shaped owner).
    async fn create_owner(
        &self,
        socket: &mut Client,
        id: &str,
        label: &str,
        workspace: bool,
    ) -> (OwnerId, String) {
        let mut args = json!({
            "label": label,
            "cwd": self.workspace_dir.to_string_lossy(),
            "port": 0,
            "origin": "http://127.0.0.1:3001",
        });
        if workspace {
            args["workspaceId"] = Value::String(self.workspace_id.clone());
        }
        let response = self.request(socket, id, "owner.create", args).await;
        assert_eq!(response["ok"], true, "owner.create: {response}");
        (
            OwnerId::from_string(
                response["result"]["owner"]
                    .as_str()
                    .expect("owner id")
                    .to_string(),
            ),
            response["result"]["capability"]
                .as_str()
                .expect("capability")
                .to_string(),
        )
    }

    async fn request(&self, socket: &mut Client, id: &str, op: &str, args: Value) -> Value {
        send_json(
            socket,
            json!({ "type": "admin_request", "id": id, "op": op, "args": args }),
        )
        .await;
        loop {
            let value = next_json(socket).await;
            if value["type"] != "admin_result" {
                // Mirror events travel on the same socket (plan Task 3/5).
                continue;
            }
            assert_eq!(value["id"], id, "responses are matched by id: {value}");
            return value;
        }
    }

    async fn teardown(&self) {
        self.state.runtimes.stop_all();
        self.state.shutdown_graceful("test").await;
    }
}

async fn send_json(socket: &mut Client, frame: Value) {
    socket
        .send(Message::Text(frame.to_string()))
        .await
        .expect("frame sent");
}

/// The next JSON frame. A close is a failure here: every test in this file
/// keeps its admin channel open until its own teardown, so a server-initiated
/// close is a bug worth surfacing with its code and reason.
async fn next_json(socket: &mut Client) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(15), socket.next())
            .await
            .expect("a frame within the deadline")
            .expect("the socket is open")
            .expect("a valid frame");
        match message {
            Message::Text(text) => return serde_json::from_str(&text).expect("a JSON frame"),
            Message::Close(Some(frame)) => panic!(
                "the admin socket closed with {} {}",
                u16::from(frame.code),
                frame.reason
            ),
            Message::Close(None) => panic!("the admin socket closed"),
            _ => continue,
        }
    }
}

async fn wait_until(deadline: Duration, mut predicate: impl FnMut() -> bool, what: &str) {
    let started = std::time::Instant::now();
    while started.elapsed() < deadline {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

/// True when this process has already reaped `pid` (no zombie, no child):
/// `waitpid` answers ECHILD. That is the difference between "signalled" and
/// "the stop path completed" (plan R5's stop-and-reap evidence).
#[cfg(unix)]
fn child_was_reaped(pid: u32) -> bool {
    let mut status = 0;
    let result = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
    result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
}

#[cfg(not(unix))]
fn child_was_reaped(_pid: u32) -> bool {
    true
}

// ---------------------------------------------------------------------------
// (a) detach: resident runtime survives, capability dies, ephemeral process dies
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn detach_keeps_the_resident_runtime_and_terminates_the_ephemeral_process() {
    let fixture = Fixture::boot().await;
    let (mut socket, _welcome) = fixture.attach().await;
    let label = format!("native-workspace-{}", fixture.workspace_id);
    let (owner, capability) = fixture.create_owner(&mut socket, "r1", &label, true).await;

    let resident = fixture.spawn_stub_runtime(
        &fixture.workspace_id.clone(),
        &fixture.workspace_dir.clone(),
        "saved",
        &owner,
    );
    let (ephemeral, temp_dir) = fixture.spawn_stub_ephemeral(&owner);
    let resident_pid = fixture.state.runtimes.pid_for(&resident).expect("pid");
    let ephemeral_pid = fixture.state.runtimes.pid_for(&ephemeral).expect("pid");
    assert!(
        child_supervision::pid_is_alive(resident_pid),
        "the resident session runtime is a live process"
    );
    assert!(
        child_supervision::pid_is_alive(ephemeral_pid),
        "the ephemeral runtime is a live process"
    );
    assert!(
        temp_dir.is_dir(),
        "the ephemeral temp dir exists before detach"
    );

    // The window is destroyed: the app sends one admin op (plan D6/T7).
    let detached = fixture
        .request(
            &mut socket,
            "r2",
            "owner.detach",
            json!({ "owner": owner.as_str() }),
        )
        .await;
    assert_eq!(detached["ok"], true, "{detached}");

    // Capability + label are revoked.
    assert_eq!(fixture.state.owner_registry.authenticate(&capability), None);
    assert_eq!(fixture.state.owner_registry.label_for_owner(&owner), None);

    // The resident workspace/session runtime is untouched: same process, same
    // instance, still running.
    assert!(
        child_supervision::pid_is_alive(resident_pid),
        "detach must not stop the resident session runtime"
    );
    assert!(fixture
        .state
        .runtimes
        .running_targets()
        .iter()
        .any(|target| target.instance_id == resident.instance_id));

    // The owner-scoped ephemeral process is really gone — exited *and* reaped.
    assert!(
        !child_supervision::pid_is_alive(ephemeral_pid),
        "the owner-scoped ephemeral process must be terminated by detach"
    );
    assert!(
        child_was_reaped(ephemeral_pid),
        "the ephemeral child must be reaped by the detach path, not merely signalled"
    );
    assert!(
        !fixture
            .state
            .runtimes
            .running_targets()
            .iter()
            .any(|target| target.instance_id == ephemeral.instance_id),
        "the stopped ephemeral runtime is no longer addressable"
    );
    assert!(
        fixture
            .state
            .ephemeral_registry
            .descriptors(&owner)
            .is_empty(),
        "the ephemeral lease is finalized after the process is gone"
    );
    assert!(!temp_dir.is_dir(), "the ephemeral temp dir is cleaned up");
    fixture.teardown().await;
}

// ---------------------------------------------------------------------------
// (b) SIGKILL-app simulation: dead attachment, then reattach
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn dead_attachment_reattach_rebinds_the_same_runtime_instance() {
    let fixture = Fixture::boot().await;
    let (mut socket, _welcome) = fixture.attach().await;
    let label = format!("native-workspace-{}", fixture.workspace_id);
    let (owner, capability) = fixture.create_owner(&mut socket, "r1", &label, true).await;
    let resident = fixture.spawn_stub_runtime(
        &fixture.workspace_id.clone(),
        &fixture.workspace_dir.clone(),
        "saved",
        &owner,
    );
    let pid_before = fixture.state.runtimes.pid_for(&resident).expect("pid");
    assert!(child_supervision::pid_is_alive(pid_before));

    // The app is SIGKILLed: the socket goes away with no detach.
    socket.close(None).await.expect("close");
    wait_until(
        Duration::from_secs(8),
        || {
            fixture
                .state
                .owner_registry
                .authenticate(&capability)
                .is_none()
        },
        "the dead attachment's capability to be revoked",
    )
    .await;
    assert_eq!(fixture.state.owner_registry.authenticate(&capability), None);
    assert_eq!(
        fixture.state.owner_registry.label_for_owner(&owner),
        None,
        "the dead attachment's labels are freed"
    );
    assert!(
        child_supervision::pid_is_alive(pid_before),
        "the resident runtime keeps running through a dead attachment (plan D11)"
    );

    // A new app attaches: a fresh attachment epoch, a fresh owner, and the
    // reclaimed label is reusable.
    let (mut socket, welcome) = fixture.attach().await;
    assert!(
        welcome["epoch"].as_u64().unwrap_or_default() > 1,
        "reattach is a new attachment epoch: {welcome}"
    );
    let (new_owner, new_capability) = fixture.create_owner(&mut socket, "r2", &label, false).await;
    assert_eq!(
        fixture.state.owner_registry.authenticate(&new_capability),
        Some(new_owner.clone()),
        "the new capability authenticates"
    );
    assert_eq!(
        fixture.state.owner_registry.authenticate(&capability),
        None,
        "the dead attachment's capability stays rejected"
    );

    // Session-prepare reattaches the resident runtime to the new owner.
    let prepared = fixture
        .dispatch(
            &fixture.native_ctx(&new_owner),
            "workspace_target_prepare",
            json!({
                "targetCwd": fixture.workspace_dir.to_string_lossy(),
                "sessionPath": fixture.session_file.to_string_lossy(),
            }),
        )
        .await
        .expect("the reattach prepare succeeds");
    let generation = prepared["transitionGeneration"]
        .as_u64()
        .expect("generation");
    assert!(generation > 0, "{prepared}");
    assert_eq!(prepared["targetSessionId"], "saved", "{prepared}");
    assert_eq!(
        prepared["targetWorkspaceId"], fixture.workspace_id,
        "{prepared}"
    );

    let rebound = fixture
        .state
        .runtimes
        .target_for_session_id("saved")
        .expect("the session runtime is still registered");
    assert_eq!(
        rebound.instance_id, resident.instance_id,
        "reattach reuses the exact runtime instance: no respawn"
    );
    assert_eq!(
        rebound.owner_id.as_deref(),
        Some(new_owner.as_str()),
        "the runtime is rebound to the reattaching owner"
    );
    assert_eq!(rebound.workspace_generation, generation);
    assert_eq!(
        fixture.state.runtimes.pid_for(&rebound),
        Some(pid_before),
        "the same Pi process is still the session's runtime"
    );
    assert!(child_supervision::pid_is_alive(pid_before));
    fixture.teardown().await;
}

// ---------------------------------------------------------------------------
// (b2) same live app: close window → detach → reopen → reattach
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn detach_and_reopen_in_one_live_app_rebinds_the_same_runtime_instance() {
    let fixture = Fixture::boot().await;
    let (mut socket, welcome) = fixture.attach().await;
    let epoch = welcome["epoch"].as_u64().expect("epoch");
    let label = format!("native-workspace-{}", fixture.workspace_id);
    let (owner, capability) = fixture.create_owner(&mut socket, "r1", &label, true).await;
    let resident = fixture.spawn_stub_runtime(
        &fixture.workspace_id.clone(),
        &fixture.workspace_dir.clone(),
        "saved",
        &owner,
    );
    let pid_before = fixture.state.runtimes.pid_for(&resident).expect("pid");

    // Close the window (same attachment, no dead attachment in this path).
    let detached = fixture
        .request(
            &mut socket,
            "r2",
            "owner.detach",
            json!({ "owner": owner.as_str() }),
        )
        .await;
    assert_eq!(detached["ok"], true, "{detached}");
    assert_eq!(fixture.state.owner_registry.authenticate(&capability), None);
    assert_eq!(
        fixture.admin.epoch(),
        epoch,
        "a detach is not an attachment change"
    );

    // Reopen: the same live attachment mints the label again.
    let (new_owner, _new_capability) = fixture.create_owner(&mut socket, "r3", &label, false).await;
    assert_ne!(new_owner, owner);

    let prepared = fixture
        .dispatch(
            &fixture.native_ctx(&new_owner),
            "workspace_target_prepare",
            json!({
                "targetCwd": fixture.workspace_dir.to_string_lossy(),
                "sessionPath": fixture.session_file.to_string_lossy(),
            }),
        )
        .await
        .expect("the reopen prepare succeeds");
    let generation = prepared["transitionGeneration"]
        .as_u64()
        .expect("generation");
    let rebound = fixture
        .state
        .runtimes
        .target_for_session_id("saved")
        .expect("the session runtime survives the window close");
    assert_eq!(rebound.instance_id, resident.instance_id);
    assert_eq!(rebound.owner_id.as_deref(), Some(new_owner.as_str()));
    assert_eq!(rebound.workspace_generation, generation);
    assert_eq!(fixture.state.runtimes.pid_for(&rebound), Some(pid_before));
    assert!(
        child_supervision::pid_is_alive(pid_before),
        "closing a window no longer kills the Pi subprocess"
    );
    fixture.teardown().await;
}

// ---------------------------------------------------------------------------
// (c) a live owner's session is never stolen
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_second_live_owner_cannot_claim_a_running_session() {
    let fixture = Fixture::boot().await;
    let (mut socket, _welcome) = fixture.attach().await;
    let label = format!("native-workspace-{}", fixture.workspace_id);
    let (owner, _capability) = fixture.create_owner(&mut socket, "r1", &label, true).await;
    let resident = fixture.spawn_stub_runtime(
        &fixture.workspace_id.clone(),
        &fixture.workspace_dir.clone(),
        "saved",
        &owner,
    );

    // A second live window for the same workspace.
    let (other_owner, _other_capability) = fixture
        .create_owner(&mut socket, "r2", "native-workspace-other", true)
        .await;
    assert_ne!(other_owner, owner);

    // The reuse decision refuses: a live owner's runtime is not reattachable.
    assert_eq!(
        plan_prepare_reuse(
            &fixture.state.runtimes,
            &fixture.state.owner_registry,
            &fixture.workspace_id,
            other_owner.as_str(),
            Some("saved"),
            false,
        ),
        PrepareReuse::Blocked(resident.clone())
    );
    assert!(
        !fixture.state.runtimes.rebind_runtime_owner(
            owner.as_str(),
            other_owner.as_str(),
            &fixture.workspace_id,
            "saved",
            7,
        ),
        "a live owner's runtime is never rebound away"
    );
    assert_eq!(
        fixture
            .state
            .runtimes
            .target_for_session_id("saved")
            .and_then(|target| target.owner_id),
        Some(owner.as_str().to_string()),
        "the original owner keeps the single writable binding"
    );

    // And the op refuses instead of creating a second binding for the session.
    let denied = fixture
        .dispatch(
            &fixture.native_ctx(&other_owner),
            "workspace_target_prepare",
            json!({
                "targetCwd": fixture.workspace_dir.to_string_lossy(),
                "sessionPath": fixture.session_file.to_string_lossy(),
            }),
        )
        .await
        .expect_err("preparing a live owner's session must be refused");
    assert!(
        denied.contains("another window"),
        "the refusal names the existing window path: {denied}"
    );
    assert_eq!(
        fixture
            .state
            .runtimes
            .running_targets()
            .iter()
            .filter(|target| target.session_id == "saved")
            .count(),
        1,
        "exactly one writable binding exists for the session"
    );
    fixture.teardown().await;
}

// ---------------------------------------------------------------------------
// (d) workspace_close stops one workspace; workspace.remove stays list-only
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn workspace_close_stops_the_workspace_runtime_while_remove_only_deletes_the_row() {
    let fixture = Fixture::boot().await;
    let (mut socket, _welcome) = fixture.attach().await;
    let label = format!("native-workspace-{}", fixture.workspace_id);
    let (owner, _capability) = fixture.create_owner(&mut socket, "r1", &label, true).await;

    // A second registered workspace with its own live runtime: stopping (or
    // un-registering) one must never touch the other.
    let other_root = fixture
        .workspace_dir
        .parent()
        .expect("temp root")
        .join("other");
    std::fs::create_dir_all(&other_root).expect("other workspace");
    let other_root = std::fs::canonicalize(&other_root).expect("canonical other");
    let (other_row, _added) = fixture
        .state
        .metadata
        .lock()
        .expect("metadata lock")
        .add_workspace(&other_root)
        .expect("register other");
    let (other_owner, _capability) = fixture
        .create_owner(&mut socket, "r2", "native-workspace-other", false)
        .await;
    let other_target = fixture.spawn_stub_runtime(
        &other_row.workspace_id.clone(),
        &other_root,
        "saved-other",
        &other_owner,
    );

    let resident = fixture.spawn_stub_runtime(
        &fixture.workspace_id.clone(),
        &fixture.workspace_dir.clone(),
        "saved",
        &owner,
    );
    let resident_pid = fixture.state.runtimes.pid_for(&resident).expect("pid");
    let other_pid = fixture.state.runtimes.pid_for(&other_target).expect("pid");
    let mut events = fixture.state.runtimes.subscribe();

    // `workspace.remove` keeps its list-removal semantics: the row is gone and
    // the agent keeps running.
    let removed = fixture
        .dispatch(
            &fixture.native_ctx(&owner),
            "workspace.remove",
            json!({ "workspaceId": fixture.workspace_id }),
        )
        .await
        .expect("workspace.remove succeeds");
    assert_eq!(removed["removed"], true, "{removed}");
    assert!(fixture
        .state
        .metadata
        .lock()
        .expect("metadata lock")
        .get_workspace(&fixture.workspace_id)
        .expect("row lookup")
        .is_none());
    assert!(
        child_supervision::pid_is_alive(resident_pid),
        "removing a row must not stop the workspace's runtime"
    );

    // `workspace_close` stops exactly that workspace's runtime.
    let stopped = fixture
        .dispatch(
            &fixture.native_ctx(&owner),
            "workspace_close",
            json!({ "workspaceId": other_row.workspace_id }),
        )
        .await
        .expect("workspace_close succeeds");
    assert_eq!(stopped["stopped"], true, "{stopped}");
    assert!(
        !child_supervision::pid_is_alive(other_pid),
        "workspace_close stops the workspace's Pi process"
    );
    assert!(
        fixture
            .state
            .metadata
            .lock()
            .expect("metadata lock")
            .get_workspace(&other_row.workspace_id)
            .expect("row lookup")
            .is_some(),
        "workspace_close does not un-register the workspace"
    );
    assert!(
        child_supervision::pid_is_alive(resident_pid),
        "workspace_close stops one workspace only"
    );

    // The window showing that workspace degrades to the existing stopped-session
    // state: the stop publishes `runtime_stopped` for the session it rendered.
    let mut saw_stopped = false;
    for _ in 0..64 {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
            Ok(Ok(event)) => {
                if event.event["type"] == "runtime_stopped"
                    && event.target.instance_id == other_target.instance_id
                {
                    saw_stopped = true;
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert!(
        saw_stopped,
        "the stopped workspace's runtime publishes runtime_stopped"
    );
    fixture.teardown().await;
}
