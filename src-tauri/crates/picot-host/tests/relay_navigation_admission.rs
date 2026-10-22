// ABOUTME: Relay phase-2.5 Task 5: the D2 action matrix over real handlers —
// ABOUTME: device navigation/git/terminal, the restricted `workspace.bootstrap`
// ABOUTME: op, and the `hello_ack.capabilities` sender (contract 1/2).

//! Semantics written down before the implementation (contract 1, D2; R2-02/03),
//! asserted through the production handlers rather than through `admits()`:
//!
//! - **Navigation face** = `workspace.list`, `workspace_target_prepare`,
//!   `workspace_transition_commit`, `workspace_transition_cancel`, and `git_*`.
//!   A paired device is admitted by the router's direct check, the host_request
//!   gate, and the dispatcher's device branch; each acts on the device's own
//!   persisted view and never on the window-owner registry.
//! - **Management face** (workspace add/remove/close, preferences, settings,
//!   admin) stays desktop-only for a device, unchanged for a desktop.
//! - **terminal** gets a device branch rooted at the device view's workspace.
//! - `workspace.bootstrap` is the one restricted WS op born from the T1
//!   inventory: fixed schema (workspaceId + sessionId), the `RuntimeTarget`
//!   shape `/v2/bootstrap` returns, and the same owner admission.
//! - `hello_ack.capabilities` is the additive sender; the rest of the ack is
//!   byte-identical, so an old client that reads the ack by key is unaffected.
//!
//! Every await is timeout-guarded: a hang is a failure, not a slow test.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::{DaemonConfig, DaemonState};
use picot_host::host_control::{ClientClass, VerifiedClientContext};
use picot_host::host_control_dispatch::host_control_dispatch;
use picot_host::host_server::HostServer;
use picot_host::metadata_store::{MetadataStore, SharedMetadataStore};
use picot_host::native_pi_manager::NativePiManager;
use picot_host::remote_auth::{DeviceView, RemoteAuth};
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::terminal_manager::TerminalManager;
use picot_host::terminal_registry::TerminalRegistry;
use picot_host::terminal_state_store::TerminalStateStore;
use picot_host::window_owner::{
    OwnerId, OwnerWorkspaceSnapshot, TemporaryKind, WindowOwnerRegistry,
};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const STEP: Duration = Duration::from_secs(20);
const DEVICE: &str = "phone-a";

fn temp_dir(tag: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "picot-navigation-{tag}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(temp.join("public")).unwrap();
    fs::write(temp.join("public").join("index.html"), "Picot").unwrap();
    fs::create_dir_all(temp.join("ws")).unwrap();
    temp
}

fn open_store(temp: &std::path::Path) -> SharedMetadataStore {
    Arc::new(Mutex::new(
        MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
    ))
}

fn register_workspace(metadata: &SharedMetadataStore, dir: &std::path::Path) -> String {
    metadata
        .lock()
        .unwrap()
        .add_workspace(dir)
        .expect("the workspace registers")
        .0
        .workspace_id
}

fn seed_window(
    registry: &Arc<WindowOwnerRegistry>,
    temp: &std::path::Path,
    workspace_id: &str,
) -> (OwnerId, String) {
    registry
        .create_owner_with_workspace(
            "navigation-window".into(),
            temp.join("ws").canonicalize().unwrap(),
            0,
            "http://127.0.0.1:1".into(),
            Some(workspace_id.into()),
            TemporaryKind::DefaultStartup,
        )
        .unwrap()
}

fn pair_device(auth: &Arc<Mutex<RemoteAuth>>, owner: &OwnerId) -> String {
    let pairing = auth.lock().unwrap().create_pairing(1, Some(owner.as_str()));
    auth.lock()
        .unwrap()
        .exchange(&pairing.token, DEVICE, 2)
        .expect("owner-stamped exchange")
}

async fn next_frame(socket: &mut Client) -> Value {
    let message = tokio::time::timeout(STEP, socket.next())
        .await
        .expect("frame timed out")
        .expect("socket closed")
        .unwrap();
    serde_json::from_str(&message.into_text().unwrap()).unwrap()
}

async fn send_frame(socket: &mut Client, frame: Value) -> Value {
    socket.send(Message::Text(frame.to_string())).await.unwrap();
    next_frame(socket).await
}

async fn dial(host: &HostServer) -> Client {
    let (socket, _) = tokio::time::timeout(
        STEP,
        tokio_tungstenite::connect_async(host.origin().replacen("http://", "ws://", 1) + "/v2/ws"),
    )
    .await
    .expect("dial timed out")
    .unwrap();
    socket
}

async fn connect_device(host: &HostServer, client_id: &str, token: &str) -> (Client, Value) {
    let mut socket = dial(host).await;
    socket
        .send(Message::Text(
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "remote",
                "clientId": client_id, "deviceToken": token
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let ack = next_frame(&mut socket).await;
    (socket, ack)
}

/// A host with one registered workspace, a paired device seeded with the same
/// workspace in its view, and the terminal manager attached (the D2 terminal
/// row needs it). No control handler is installed: the frame path is what
/// these tests assert.
struct Wave {
    temp: PathBuf,
    host: HostServer,
    token: String,
    capability: String,
}

async fn wave(tag: &str) -> Wave {
    let temp = temp_dir(tag);
    let workspace_dir = temp.join("workspace");
    fs::create_dir_all(&workspace_dir).unwrap();
    let workspace_dir = workspace_dir.canonicalize().unwrap();
    let metadata = open_store(&temp);
    let workspace_id = register_workspace(&metadata, &workspace_dir);
    let registry = Arc::new(WindowOwnerRegistry::default());
    let (owner, capability) = seed_window(&registry, &temp, &workspace_id);
    let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
    let host = tokio::time::timeout(
        STEP,
        HostServer::start(
            temp.join("public"),
            NativePiManager::new(8),
            Arc::clone(&auth),
            Arc::clone(&metadata),
        ),
    )
    .await
    .expect("host start timed out")
    .unwrap();
    host.set_owner_registry(Arc::clone(&registry));
    host.set_terminal_manager(Arc::new(TerminalManager::new(
        TerminalRegistry::new(15),
        TerminalStateStore::new(temp.join("terminal")),
    )));
    let token = pair_device(&auth, &owner);
    // The pairing-time seed the hello performs (asserted by the T4 suite); this
    // suite drives frames directly, so the view is written here.
    auth.lock()
        .unwrap()
        .store_device_view(
            DEVICE,
            &DeviceView {
                workspace_id: workspace_id.clone(),
                session_id: None,
                generation: 0,
            },
        )
        .unwrap();
    Wave {
        temp,
        host,
        token,
        capability,
    }
}

impl Wave {
    fn drop(self) {
        self.host.stop();
        let _ = fs::remove_dir_all(self.temp);
    }
}

fn close_quietly(socket: Client) {
    tokio::spawn(async move {
        let mut socket = socket;
        let _ = socket.close(None).await;
    });
}

/// `hello_ack.capabilities` (contract 1 sender, wire whitelist §12): a paired
/// device advertises navigation/git/terminal and no native/management face, a
/// desktop advertises all of them, and the rest of the ack keeps the legacy
/// shape an old client already reads by key.
#[tokio::test]
async fn capabilities_are_advertised_per_face_and_the_ack_stays_additive() {
    let wave = wave("capabilities").await;
    let (device, ack) = connect_device(&wave.host, "device-a", &wave.token).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");
    let caps = &ack["capabilities"];
    assert_eq!(caps["navigation"], json!(true), "{ack}");
    assert_eq!(caps["git"], json!(true), "{ack}");
    assert_eq!(caps["terminal"], json!(true), "{ack}");
    // `native` describes the host, not the caller: a paired device drives the
    // same workspaces, so hiding the registry would empty its sidebar.
    assert_eq!(caps["native"], json!(true), "{ack}");
    assert_eq!(caps["management"], json!(false), "{ack}");
    // Additive only: dropping the new field leaves exactly the legacy ack.
    let mut legacy = ack.clone();
    legacy.as_object_mut().unwrap().remove("capabilities");
    assert_eq!(legacy, json!({ "type": "hello_ack", "protocolVersion": 2 }));
    close_quietly(device);

    // A desktop owner keeps every face.
    let mut desktop = dial(&wave.host).await;
    desktop
        .send(Message::Text(
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                "clientId": "desktop-a", "desktopCapability": wave.capability
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let desktop_ack = next_frame(&mut desktop).await;
    let caps = &desktop_ack["capabilities"];
    for face in ["native", "navigation", "git", "terminal", "management"] {
        assert_eq!(caps[face], json!(true), "desktop {face}: {desktop_ack}");
    }
    close_quietly(desktop);
    wave.drop();
}

/// The routing layer's direct check (and the host_request gate behind it): a
/// device reaches exactly the navigation ops. Management ops are refused with
/// the same permanent `forbidden_class` as before this task.
#[tokio::test]
async fn device_reaches_the_navigation_face_and_never_the_management_face() {
    let wave = wave("routing").await;
    let (mut device, ack) = connect_device(&wave.host, "device-a", &wave.token).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");

    // Navigation ops pass the class check and reach the control plane (no
    // handler installed here, so the answer is `control_unavailable` — the
    // point is that it is no longer `forbidden_class`).
    for operation in [
        "workspace.list",
        "runtime_instances",
        "get_app_version",
        "is_dev",
        "pi_path_status",
        "git_status",
        "workspace_target_prepare",
    ] {
        let reply = send_frame(
            &mut device,
            json!({
                "type": "host_request", "requestId": format!("nav-{operation}"),
                "operation": operation, "args": {}
            }),
        )
        .await;
        assert_ne!(
            reply["error"]["code"], "forbidden_class",
            "{operation}: {reply}"
        );
    }

    // Management ops stay closed, unchanged from the pre-T5 behavior.
    for operation in ["preference_get", "workspace.add", "host_health"] {
        let reply = send_frame(
            &mut device,
            json!({
                "type": "host_request", "requestId": format!("mgmt-{operation}"),
                "operation": operation, "args": {}
            }),
        )
        .await;
        assert_eq!(
            reply["error"]["code"], "forbidden_class",
            "{operation}: {reply}"
        );
    }

    // Desktop is untouched: the same management op is admitted.
    let mut desktop = dial(&wave.host).await;
    desktop
        .send(Message::Text(
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                "clientId": "desktop-a", "desktopCapability": wave.capability
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let _ = next_frame(&mut desktop).await;
    let reply = send_frame(
        &mut desktop,
        json!({
            "type": "host_request", "requestId": "desktop-pref",
            "operation": "preference_get", "args": {}
        }),
    )
    .await;
    assert_ne!(reply["error"]["code"], "forbidden_class", "{reply}");

    close_quietly(device);
    close_quietly(desktop);
    wave.drop();
}

/// `workspace.bootstrap` (contract 2, T1 row `/v2/bootstrap`): the fixed
/// schema, the same RuntimeTarget reply, and refusal of anything wider.
#[tokio::test]
async fn workspace_bootstrap_is_a_fixed_schema_op() {
    let temp = temp_dir("bootstrap");
    let workspace_dir = temp.join("workspace");
    fs::create_dir_all(&workspace_dir).unwrap();
    let metadata = open_store(&temp);
    let workspace_id = register_workspace(&metadata, &workspace_dir);
    let registry = Arc::new(WindowOwnerRegistry::default());
    let (owner, _capability) = seed_window(&registry, &temp, &workspace_id);
    let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
    let runtimes = NativePiManager::new(8);
    let target = RuntimeTarget::with_owner(
        workspace_id.clone(),
        "session-one",
        "instance-one",
        owner.as_str(),
        0,
    );
    // Keep the bridge open: dropping the fake process signals runtime exit.
    let _process = runtimes.register_in_memory(target.clone()).unwrap();
    assert_eq!(
        runtimes
            .target_for_session(&workspace_id, "session-one")
            .map(|target| target.instance_id),
        Some("instance-one".to_string()),
        "the seeded runtime is resident before the host starts"
    );
    let host = tokio::time::timeout(
        STEP,
        HostServer::start(
            temp.join("public"),
            runtimes,
            Arc::clone(&auth),
            Arc::clone(&metadata),
        ),
    )
    .await
    .expect("host start timed out")
    .unwrap();
    host.set_owner_registry(Arc::clone(&registry));
    let token = pair_device(&auth, &owner);
    auth.lock()
        .unwrap()
        .store_device_view(
            DEVICE,
            &DeviceView {
                workspace_id: workspace_id.clone(),
                session_id: None,
                generation: 0,
            },
        )
        .unwrap();
    let (mut device, ack) = connect_device(&host, "device-a", &token).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");

    let reply = send_frame(
        &mut device,
        json!({
            "type": "data_request", "requestId": "boot-1",
            "operation": "workspace.bootstrap",
            "workspaceId": workspace_id, "sessionId": "session-one"
        }),
    )
    .await;
    assert_eq!(reply["type"], "data_response", "{reply}");
    assert_eq!(reply["operation"], "workspace.bootstrap", "{reply}");
    // Same shape as the HTTP handler's RuntimeTarget.
    assert_eq!(
        reply["target"]["workspaceId"],
        json!(workspace_id),
        "{reply}"
    );
    assert_eq!(
        reply["target"]["sessionId"],
        json!("session-one"),
        "{reply}"
    );
    assert_eq!(
        reply["target"]["instanceId"],
        json!("instance-one"),
        "{reply}"
    );
    assert_eq!(reply["target"]["ownerId"], json!(owner.as_str()), "{reply}");

    // Fixed schema: no field rides along, nothing is proxied.
    let extra = send_frame(
        &mut device,
        json!({
            "type": "data_request", "requestId": "boot-2",
            "operation": "workspace.bootstrap",
            "workspaceId": workspace_id, "sessionId": "session-one",
            "url": "http://example.invalid/v2/bootstrap"
        }),
    )
    .await;
    assert_eq!(
        extra["error"]["code"], "invalid_bootstrap_request",
        "{extra}"
    );

    // A device rides its owner's data plane (option A), so bootstrap is no
    // longer pinned to the device view: an id with no live runtime fails at
    // resolution instead of at admission.
    let outside = send_frame(
        &mut device,
        json!({
            "type": "data_request", "requestId": "boot-3",
            "operation": "workspace.bootstrap",
            "workspaceId": "ws-other", "sessionId": "session-one"
        }),
    )
    .await;
    assert_eq!(outside["error"]["code"], "runtime_not_found", "{outside}");

    // A session with no live runtime: the HTTP handler's `runtime_not_found`.
    let missing = send_frame(
        &mut device,
        json!({
            "type": "data_request", "requestId": "boot-4",
            "operation": "workspace.bootstrap",
            "workspaceId": workspace_id, "sessionId": "session-nope"
        }),
    )
    .await;
    assert_eq!(missing["error"]["code"], "runtime_not_found", "{missing}");

    close_quietly(device);
    host.stop();
    let _ = fs::remove_dir_all(temp);
}

/// The terminal row of the matrix: a device's `terminal_command` runs against
/// its own view's workspace root instead of the pairing window's registry
/// binding (the pre-T5 answer was "requires a desktop owner").
#[tokio::test]
async fn device_terminal_commands_run_against_the_device_view() {
    let wave = wave("terminal").await;
    let (mut device, ack) = connect_device(&wave.host, "device-a", &wave.token).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");

    let reply = send_frame(
        &mut device,
        json!({
            "type": "terminal_command", "requestId": "term-1",
            "payload": { "type": "terminal_list" }
        }),
    )
    .await;
    assert_eq!(reply["type"], "terminal_listed", "{reply}");
    assert_eq!(reply["requestId"], "term-1", "{reply}");

    // An unknown payload still answers as a failed terminal command (the
    // device branch is the terminal path, not a silent drop).
    let bad = send_frame(
        &mut device,
        json!({
            "type": "terminal_command", "requestId": "term-2",
            "payload": { "type": "terminal_bogus" }
        }),
    )
    .await;
    assert_eq!(bad["type"], "terminal_command_failed", "{bad}");

    close_quietly(device);
    wave.drop();
}

// ---------------------------------------------------------------------------
// Dispatcher level: the real control handler (`host_control_dispatch`) driven
// with a Remote principal, so the device-view execution is asserted on the
// production handler, not on its classifier.
// ---------------------------------------------------------------------------

/// Every method refuses: this target never exercises the app surface.
struct NoSurface;

#[async_trait::async_trait]
impl AppSurface for NoSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &RuntimeTarget,
        _label: &str,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Err("unused in the navigation suite".into())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused in the navigation suite".into())
    }

    async fn focus_window(&self, _label: &str) -> Result<(), String> {
        Err("unused in the navigation suite".into())
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in the navigation suite".into())
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in the navigation suite".into())
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        Ok(())
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused in the navigation suite".into())
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        Err("unused in the navigation suite".into())
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused in the navigation suite".into())
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        Err("unused in the navigation suite".into())
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        Err("unused in the navigation suite".into())
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused in the navigation suite".into())
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        Err("unused in the navigation suite".into())
    }
}

struct Daemon {
    _temp: tempfile::TempDir,
    state: Arc<DaemonState>,
    workspace_id: String,
    workspace_dir: PathBuf,
    session_file: PathBuf,
    owner: OwnerId,
}

impl Daemon {
    /// A daemon with one registered workspace, one live in-memory runtime for
    /// the persisted session `saved`, one desktop window, and one paired device
    /// whose view starts on that workspace.
    async fn boot() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(static_dir.join("index.html"), "<h1>navigation</h1>").unwrap();
        let workspace_dir = temp.path().join("workspace");
        fs::create_dir_all(&workspace_dir).unwrap();
        let workspace_dir = workspace_dir.canonicalize().unwrap();
        let session_dir = temp.path().join("sessions");
        fs::create_dir_all(&session_dir).unwrap();
        let session_file = session_dir.join("2026-10-20T00-00-00-000Z_saved.jsonl");
        fs::write(
            &session_file,
            format!(
                "{}\n{}\n",
                json!({
                    "type": "session",
                    "id": "saved",
                    "cwd": workspace_dir.to_string_lossy(),
                }),
                // One user turn: `parse_session_header` suppresses trivially
                // short one-shot sessions, and this fixture is a real session.
                json!({ "type": "message", "message": { "role": "user", "content": "hello" } }),
            ),
        )
        .unwrap();
        let state = Arc::new(
            tokio::time::timeout(
                STEP,
                DaemonState::start(
                    DaemonConfig {
                        data_dir: temp.path().join("data"),
                        config_dir: temp.path().join("config"),
                        static_dir,
                        bundle_identifier: "com.palandata.picot.test".into(),
                    },
                    Arc::new(NoSurface),
                ),
            )
            .await
            .expect("daemon start timed out")
            .expect("the daemon host stack boots"),
        );
        let workspace_id = register_workspace(&state.metadata, &workspace_dir);
        let (owner, _capability) = state
            .owner_registry
            .create_owner_with_workspace(
                "navigation-window".into(),
                workspace_dir.clone(),
                0,
                "http://127.0.0.1:1".into(),
                Some(workspace_id.clone()),
                TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let target = RuntimeTarget::with_owner(
            workspace_id.clone(),
            "saved",
            "instance-saved",
            owner.as_str(),
            0,
        );
        state.runtimes.register_in_memory(target).unwrap();
        pair_device(&state.auth, &owner);
        state
            .auth
            .lock()
            .unwrap()
            .store_device_view(
                DEVICE,
                &DeviceView {
                    workspace_id: workspace_id.clone(),
                    session_id: None,
                    generation: 0,
                },
            )
            .unwrap();
        Self {
            _temp: temp,
            state,
            workspace_id,
            workspace_dir,
            session_file,
            owner,
        }
    }

    fn device_ctx(&self) -> VerifiedClientContext {
        VerifiedClientContext {
            client_id: 7,
            class: ClientClass::Remote,
            owner_id: Some(self.owner.clone()),
            device_id: Some(DEVICE.into()),
        }
    }

    fn desktop_ctx(&self) -> VerifiedClientContext {
        VerifiedClientContext {
            client_id: 1,
            class: ClientClass::Native,
            owner_id: Some(self.owner.clone()),
            device_id: None,
        }
    }

    async fn dispatch_as(
        &self,
        ctx: VerifiedClientContext,
        command: &str,
        args: Value,
    ) -> Result<Value, String> {
        let handler = host_control_dispatch(Arc::clone(&self.state));
        let canonical = json!({
            "type": "host_request",
            "requestId": format!("{command}-1"),
            "operation": command,
            "args": args,
        });
        tokio::time::timeout(STEP, handler(ctx, canonical, Arc::new(|_| {})))
            .await
            .expect("dispatch timed out")
    }

    fn view(&self) -> Option<DeviceView> {
        self.state.auth.lock().unwrap().device_view(DEVICE).unwrap()
    }

    fn window_snapshot(&self) -> OwnerWorkspaceSnapshot {
        self.state
            .owner_registry
            .owner_current_workspace(&self.owner)
    }
}

/// The navigation rows of the matrix, end to end on the real dispatcher: list,
/// prepare, commit move **only** the device view; the window binding is
/// untouched; a stale generation is refused.
#[tokio::test]
async fn device_navigation_moves_only_the_device_view() {
    let daemon = Daemon::boot().await;
    let before = daemon.window_snapshot();

    let listed = daemon
        .dispatch_as(daemon.device_ctx(), "workspace.list", json!({}))
        .await
        .expect("a device may list workspaces");
    let workspaces = listed["workspaces"].as_array().expect("workspaces array");
    assert!(
        workspaces
            .iter()
            .any(|row| row["workspaceId"] == json!(daemon.workspace_id)),
        "{listed}"
    );

    let prepared = daemon
        .dispatch_as(
            daemon.device_ctx(),
            "workspace_target_prepare",
            json!({
                "targetCwd": daemon.workspace_dir.to_string_lossy(),
                "sessionPath": daemon.session_file.to_string_lossy(),
            }),
        )
        .await
        .expect("prepare resolves the device target");
    assert_eq!(prepared["targetSessionId"], json!("saved"), "{prepared}");
    assert_eq!(prepared["classification"], json!("same"), "{prepared}");
    let generation = prepared["transitionGeneration"]
        .as_u64()
        .expect("a transition generation");
    // Prepare alone never moves the view.
    assert_eq!(
        daemon.view().unwrap().session_id,
        None,
        "prepare must not commit the binding"
    );

    let committed = daemon
        .dispatch_as(
            daemon.device_ctx(),
            "workspace_transition_commit",
            json!({ "transitionGeneration": generation }),
        )
        .await
        .expect("commit lands on the device view");
    assert_eq!(
        committed["workspaceGeneration"],
        json!(generation),
        "{committed}"
    );
    assert_eq!(
        daemon.view(),
        Some(DeviceView {
            workspace_id: daemon.workspace_id.clone(),
            session_id: Some("saved".into()),
            generation,
        })
    );

    // A second commit for the same generation has nothing left to commit.
    assert!(daemon
        .dispatch_as(
            daemon.device_ctx(),
            "workspace_transition_commit",
            json!({ "transitionGeneration": generation }),
        )
        .await
        .is_err());

    // Contract 1: the window-owner registry never moved.
    assert_eq!(
        daemon.window_snapshot(),
        before,
        "device navigation must not touch the window binding"
    );

    // A stale generation is refused rather than applied.
    daemon
        .dispatch_as(
            daemon.device_ctx(),
            "workspace_target_prepare",
            json!({
                "targetCwd": daemon.workspace_dir.to_string_lossy(),
                "sessionPath": daemon.session_file.to_string_lossy(),
            }),
        )
        .await
        .expect("prepare again");
    let stale = daemon
        .dispatch_as(
            daemon.device_ctx(),
            "workspace_transition_commit",
            json!({ "transitionGeneration": generation + 7 }),
        )
        .await;
    assert!(
        stale.is_err(),
        "stale generation must be refused: {stale:?}"
    );
    assert!(
        stale.unwrap_err().contains("stale transition generation"),
        "the refusal names the stale generation"
    );

    // Cancel drops the pending navigation and keeps the committed view.
    let prepared = daemon
        .dispatch_as(
            daemon.device_ctx(),
            "workspace_target_prepare",
            json!({
                "targetCwd": daemon.workspace_dir.to_string_lossy(),
                "sessionPath": daemon.session_file.to_string_lossy(),
            }),
        )
        .await
        .expect("prepare for cancel");
    let cancel_generation = prepared["transitionGeneration"].as_u64().unwrap();
    daemon
        .dispatch_as(
            daemon.device_ctx(),
            "workspace_transition_cancel",
            json!({ "transitionGeneration": cancel_generation }),
        )
        .await
        .expect("cancel");
    assert_eq!(
        daemon.view(),
        Some(DeviceView {
            workspace_id: daemon.workspace_id.clone(),
            session_id: Some("saved".into()),
            generation,
        }),
        "cancel leaves the committed binding alone"
    );
    assert_eq!(daemon.window_snapshot(), before);
}

/// Management stays closed for a device (and open for a desktop): the matrix's
/// unchanged rows, asserted on the same dispatcher.
#[tokio::test]
async fn device_navigation_keeps_management_and_host_control_closed() {
    let daemon = Daemon::boot().await;
    for command in [
        "workspace.add",
        "workspace.remove",
        "workspace_close",
        "preference_get",
        "host_health",
    ] {
        let reply = daemon
            .dispatch_as(daemon.device_ctx(), command, json!({ "path": "/tmp" }))
            .await;
        assert!(reply.is_err(), "{command} must be refused for a device");
    }
    // The device reached none of them, so nothing was created or removed.
    let listed = daemon
        .dispatch_as(daemon.desktop_ctx(), "workspace.list", json!({}))
        .await
        .expect("desktop list");
    assert_eq!(
        listed["workspaces"].as_array().unwrap().len(),
        1,
        "{listed}"
    );

    // Desktop keeps its own path: the same navigation op still runs through the
    // window registry and is byte-identical in shape.
    let desktop_prepare = daemon
        .dispatch_as(
            daemon.desktop_ctx(),
            "workspace_target_prepare",
            json!({
                "targetCwd": daemon.workspace_dir.to_string_lossy(),
                "sessionPath": daemon.session_file.to_string_lossy(),
            }),
        )
        .await
        .expect("desktop prepare");
    assert_eq!(
        desktop_prepare["targetSessionId"],
        json!("saved"),
        "{desktop_prepare}"
    );
    // …and the desktop prepare moves the window registry, not the device view.
    assert!(matches!(
        daemon.window_snapshot(),
        OwnerWorkspaceSnapshot::Registered { .. }
    ));
    assert_eq!(daemon.view().unwrap().session_id, None);
}
