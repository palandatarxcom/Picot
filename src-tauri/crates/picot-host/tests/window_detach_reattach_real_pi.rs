// ABOUTME: Task 7 Step 5 real-Pi chain smoke: the real embedded Pi survives a window
// ABOUTME: close, is reattached by a reopen, and dies when the sidebar stops it.

//! The manual matrix of plan Task 7 Step 5, made repeatable: this runs the
//! **production** chain against the **real** embedded Pi binary
//! (`src-tauri/resources/pi/pi`, the same one `pi_launch` resolves in dev and in
//! the packaged app) — real `DaemonState`, real loopback `AdminServer` with a
//! real WS client, real `host_control_dispatch` prepare path.
//!
//! What it proves, in process-liveness terms (never bookkeeping):
//! 1. open workspace → a real Pi child exists (`ps` shows the Pi binary);
//! 2. close window → `owner.detach` over the admin channel → **the Pi child is
//!    still alive** (window close detaches, plan D6);
//! 3. reopen the window → prepare again → **the same Pi process** is reattached
//!    (same instance id, same pid — no respawn, plan D11);
//! 4. sidebar "Stop Running" → `workspace_close` → the Pi child is gone **and
//!    reaped** (plan R5).
//!
//! Ignored by default so `bun run check:rust` stays fast and hermetic; run it
//! with `cargo test -p picot-host --test window_detach_reattach_real_pi --
//! --ignored --nocapture`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use picot_host::child_supervision;
use picot_host::daemon::admin::{AdminConfig, AdminServer, ADMIN_WS_PATH};
use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::{DaemonConfig, DaemonState};
use picot_host::host_control::{ClientClass, ProgressSink, VerifiedClientContext};
use picot_host::host_control_dispatch::host_control_dispatch;
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::window_owner::OwnerId;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// None of these steps opens a window or a dialog: the surface is the app's,
/// and a call from here would mean the chain under test is not the chain the
/// WebView drives.
#[derive(Default)]
struct NeverCalledSurface;

#[async_trait::async_trait]
impl AppSurface for NeverCalledSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &RuntimeTarget,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Err("unused in the real-Pi chain smoke".into())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused".into())
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused".into())
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused".into())
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        Err("unused".into())
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused".into())
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        Err("unused".into())
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused".into())
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        Err("unused".into())
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        Err("unused".into())
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused".into())
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        Err("unused".into())
    }
}

/// The command line of `pid`, as `ps` reports it — the evidence that the live
/// process is the real Pi binary and not a test double.
fn ps_line(pid: u32) -> String {
    let output = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// True when this process has already reaped `pid` (`waitpid` answers ECHILD):
/// the difference between "signalled" and "the stop path completed".
#[cfg(unix)]
fn child_was_reaped(pid: u32) -> bool {
    let mut status = 0;
    let result = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
    result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
}

async fn send_json(socket: &mut Client, frame: Value) {
    socket
        .send(Message::Text(frame.to_string()))
        .await
        .expect("frame sent");
}

async fn next_json(socket: &mut Client) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(60), socket.next())
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

async fn request(socket: &mut Client, id: &str, op: &str, args: Value) -> Value {
    send_json(
        socket,
        json!({ "type": "admin_request", "id": id, "op": op, "args": args }),
    )
    .await;
    loop {
        let value = next_json(socket).await;
        if value["type"] != "admin_result" {
            continue;
        }
        assert_eq!(value["id"], id, "responses are matched by id: {value}");
        return value;
    }
}

struct Chain {
    _temp: tempfile::TempDir,
    state: Arc<DaemonState>,
    token: String,
    admin: AdminServer,
    workspace_id: String,
    workspace_dir: PathBuf,
    session_file: PathBuf,
}

impl Chain {
    async fn boot() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        // The real static dir: `pi_launch::resolve_bundled_pi` finds the shipped
        // Pi here, exactly as the dev and packaged app do.
        let static_dir = PathBuf::from(env!("PICOT_SRC_TAURI_DIR")).join("resources");
        let pi = picot_host::pi_launch::resolve_bundled_pi(&static_dir)
            .expect("the shipped Pi binary resolves");
        assert!(pi.is_file(), "the shipped Pi binary exists at {pi:?}");

        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let workspace_dir = std::fs::canonicalize(&workspace_dir).expect("canonical workspace");
        let session_file = temp.path().join("2026-10-15T00-00-00-000Z_saved.jsonl");
        std::fs::write(
            &session_file,
            format!(
                "{}\n{}\n",
                json!({
                    "type": "session",
                    "id": "saved",
                    "cwd": workspace_dir.to_string_lossy(),
                }),
                // One user message: `parse_session_header` suppresses a header-only
                // file as a trivially short one-shot session.
                json!({ "type": "message", "message": { "role": "user", "content": "smoke" } })
            ),
        )
        .expect("session fixture");

        let state = Arc::new(
            DaemonState::start(
                DaemonConfig {
                    data_dir: temp.path().join("data"),
                    config_dir: temp.path().join("config"),
                    static_dir,
                    bundle_identifier: "com.palandata.picot.smoke".to_string(),
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
        let token = "real-pi-chain-smoke-token-0123456789".to_string();
        let admin = AdminServer::start(AdminConfig::new(token.clone()))
            .await
            .expect("the admin listener binds");
        state.install_admin(admin.clone());
        Self {
            _temp: temp,
            state,
            token,
            admin,
            workspace_id: row.workspace_id,
            workspace_dir,
            session_file,
        }
    }

    async fn attach(&self) -> Client {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "ws://{}{ADMIN_WS_PATH}",
            self.admin.origin().trim_start_matches("http://")
        ))
        .await
        .expect("a real admin client connects");
        send_json(
            &mut socket,
            json!({
                "type": "admin_hello",
                "adminToken": self.token,
                "role": "manager",
                "appVersion": "0.5.0",
                "buildId": "smoke",
                "pid": std::process::id(),
            }),
        )
        .await;
        loop {
            if next_json(&mut socket).await["type"] == "admin_welcome" {
                return socket;
            }
        }
    }

    /// `owner.create` for a workspace window (the app's open-window step).
    async fn create_owner(&self, socket: &mut Client, id: &str) -> OwnerId {
        let response = request(
            socket,
            id,
            "owner.create",
            json!({
                "label": format!("native-workspace-{}", self.workspace_id),
                "cwd": self.workspace_dir.to_string_lossy(),
                "port": 0,
                "origin": "http://127.0.0.1:3001",
                "workspaceId": self.workspace_id,
            }),
        )
        .await;
        assert_eq!(response["ok"], true, "owner.create: {response}");
        OwnerId::from_string(
            response["result"]["owner"]
                .as_str()
                .expect("owner id")
                .to_string(),
        )
    }

    /// The WebView's session-click path: `workspace_target_prepare` through the
    /// production dispatcher.
    async fn prepare(&self, owner: &OwnerId) -> Value {
        let handler = host_control_dispatch(Arc::clone(&self.state));
        let ctx = VerifiedClientContext {
            client_id: 1,
            class: ClientClass::Native,
            owner_id: Some(owner.clone()),
        };
        let progress: ProgressSink = Arc::new(|_| {});
        handler(
            ctx,
            json!({
                "type": "runtime_request",
                "command": { "type": "workspace_target_prepare" },
                "args": {
                    "targetCwd": self.workspace_dir.to_string_lossy(),
                    "sessionPath": self.session_file.to_string_lossy(),
                },
            }),
            progress,
        )
        .await
        .expect("the prepare succeeds")
    }

    async fn workspace_close(&self, owner: &OwnerId) -> Value {
        let handler = host_control_dispatch(Arc::clone(&self.state));
        let ctx = VerifiedClientContext {
            client_id: 1,
            class: ClientClass::Native,
            owner_id: Some(owner.clone()),
        };
        let progress: ProgressSink = Arc::new(|_| {});
        handler(
            ctx,
            json!({
                "type": "runtime_request",
                "command": { "type": "workspace_close" },
                "args": { "workspaceId": self.workspace_id },
            }),
            progress,
        )
        .await
        .expect("workspace_close succeeds")
    }

    fn pid(&self, session_id: &str) -> u32 {
        let target = self
            .state
            .runtimes
            .target_for_session_id(session_id)
            .expect("the session runtime is registered");
        self.state
            .runtimes
            .pid_for(&target)
            .expect("the runtime has a live child")
    }

    fn target(&self, session_id: &str) -> RuntimeTarget {
        self.state
            .runtimes
            .target_for_session_id(session_id)
            .expect("the session runtime is registered")
    }

    async fn teardown(&self) {
        self.state.runtimes.stop_all();
        self.state.shutdown_graceful("smoke").await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "real-Pi chain smoke; run explicitly with --ignored --nocapture"]
async fn real_pi_survives_window_close_reattaches_and_dies_on_sidebar_stop() {
    let chain = Chain::boot().await;
    let mut socket = chain.attach().await;
    let owner = chain.create_owner(&mut socket, "r1").await;

    // 1. Open the workspace: a real Pi child, named by `ps`.
    let prepared = chain.prepare(&owner).await;
    let session_id = prepared["targetSessionId"]
        .as_str()
        .expect("targetSessionId")
        .to_string();
    let instance_before = chain.target(&session_id).instance_id;
    let pid = chain.pid(&session_id);
    let command = ps_line(pid);
    assert!(
        child_supervision::pid_is_alive(pid),
        "the prepared runtime is a live process"
    );
    assert!(
        command.contains("pi"),
        "ps names the shipped Pi binary for pid {pid}: {command}"
    );
    println!("[smoke] open   → pid {pid}: {command}");

    // 2. Close the window: the app sends exactly this admin op.
    let detached = request(
        &mut socket,
        "r2",
        "owner.detach",
        json!({ "owner": owner.as_str() }),
    )
    .await;
    assert_eq!(detached["ok"], true, "{detached}");
    assert!(
        child_supervision::pid_is_alive(pid),
        "closing the window must leave the Pi process running"
    );
    println!("[smoke] close  → pid {pid} still alive: {}", ps_line(pid));

    // 3. Reopen the window and the same session: same process, no respawn.
    let reopened = chain.create_owner(&mut socket, "r3").await;
    let reprepared = chain.prepare(&reopened).await;
    assert_eq!(reprepared["targetSessionId"], session_id, "{reprepared}");
    let target_after = chain.target(&session_id);
    assert_eq!(
        target_after.instance_id, instance_before,
        "the reopen reattaches the same runtime instance"
    );
    assert_eq!(
        chain.pid(&session_id),
        pid,
        "the reopen reattaches the same Pi process"
    );
    assert_eq!(
        target_after.owner_id.as_deref(),
        Some(reopened.as_str()),
        "the runtime is rebound to the reopened window's owner"
    );
    println!("[smoke] reopen → pid {pid} reattached: {}", ps_line(pid));

    // 4. Sidebar "Stop Running": the workspace's runtime is really gone.
    let stopped = chain.workspace_close(&reopened).await;
    assert_eq!(stopped["stopped"], true, "{stopped}");
    assert!(
        !child_supervision::pid_is_alive(pid),
        "workspace_close must terminate the workspace's Pi process"
    );
    assert!(
        child_was_reaped(pid),
        "the stopped Pi child must be reaped, not merely signalled"
    );
    assert!(
        chain
            .state
            .runtimes
            .target_for_session_id(&session_id)
            .is_none(),
        "the stopped runtime is no longer addressable"
    );
    println!("[smoke] stop   → pid {pid} gone (ps: '{}')", ps_line(pid));

    chain.teardown().await;
}
