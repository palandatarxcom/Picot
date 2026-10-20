// ABOUTME: R5 protocol tests: the app's `owner.close_request` admin op, the daemon's
// ABOUTME: data-plane `window_close_request` push, and its approve/cancel/timeout settlement.

//! Plan R5 (follow-up): the window-close confirmation flow lives in the
//! WebView's coordinator, and the WebView holds its data-plane connection to
//! the **daemon** — so the app's close delegate can only reach it through the
//! admin channel. These tests pin the whole daemon half of that chain against a
//! real loopback `AdminServer` and a real client socket:
//!
//! 1. the push: `owner.close_request` produces the literal
//!    `{"type":"window_close_request","requestId":"close-7"}` frame on the data
//!    plane, for that owner only;
//! 2. the result: the data-plane `window_close_approve`/`window_close_cancel`
//!    settlement resolves the long-poll with `approved`/`cancelled`;
//! 3. the degradation: an owner with no WebView answers
//!    `{delivered:false, outcome:"undeliverable"}`, and a silent one answers
//!    `{delivered:true, outcome:"timeout"}` — both are the app's native-dialog
//!    fallback, which is why they must stay reachable.
//!
//! Frames are written from the plan's protocol tables by hand (engineering
//! lesson #1), never generated from the Rust types.
//!
//! `register_desktop_owner_for_tests` stands in for the WebView's `/v2/ws`
//! connection: it is the same router registration the handshake performs, and
//! the frame the test reads back is the production frame the WS handler would
//! have forwarded.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use picot_host::daemon::admin::{AdminConfig, AdminServer, ADMIN_WS_PATH, MIRROR_ACK_TIMEOUT};
use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::owner_close::{
    CloseOutcome, OwnerCloseDelivery, CLOSE_REQUEST_SETTLE_TIMEOUT,
};
use picot_host::daemon::{DaemonConfig, DaemonState};
use picot_host::host_control::{ClientClass, OwnerEvent, VerifiedClientContext};
use picot_host::window_owner::OwnerId;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The app surface these tests never exercise: any call is a bug, and the
/// settlement hook runs *after* the surface call in production, so a stub that
/// silently succeeded would hide a missing surface call.
///
/// The close ops *are* exercised (the approve/cancel tests drive the dispatcher
/// directly), so the two records are read back by those tests.
#[derive(Default)]
struct RecordingSurface {
    closed: Mutex<Vec<String>>,
    cancelled: Mutex<Vec<String>>,
}

impl RecordingSurface {
    fn closed(&self) -> Vec<String> {
        self.closed.lock().unwrap().clone()
    }

    fn cancelled(&self) -> Vec<String> {
        self.cancelled.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl AppSurface for RecordingSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &picot_host::runtime_coordinator::RuntimeTarget,
        _label: &str,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Err("unused".into())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused".into())
    }

    async fn focus_window(&self, _label: &str) -> Result<(), String> {
        Err("unused".into())
    }

    async fn close_window(&self, label: &str, _request_id: &str) -> Result<(), String> {
        self.closed.lock().unwrap().push(label.to_string());
        Ok(())
    }

    async fn cancel_window_close(&self, _label: &str, request_id: &str) -> Result<(), String> {
        self.cancelled.lock().unwrap().push(request_id.to_string());
        Ok(())
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

struct Fixture {
    _temp: tempfile::TempDir,
    state: Arc<DaemonState>,
    admin: AdminServer,
    token: String,
    surface: Arc<RecordingSurface>,
}

impl Fixture {
    async fn boot(close_request_timeout: Duration) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).expect("static dir");
        std::fs::write(static_dir.join("index.html"), "<h1>owner close</h1>").expect("index");
        let surface = Arc::new(RecordingSurface::default());
        let state = Arc::new(
            DaemonState::start(
                DaemonConfig {
                    data_dir: temp.path().join("data"),
                    config_dir: temp.path().join("config"),
                    static_dir,
                    bundle_identifier: "com.palandata.picot.test".to_string(),
                },
                surface.clone(),
            )
            .await
            .expect("the daemon host stack boots"),
        );
        let token = "test-admin-token-0123456789abcdef".to_string();
        let admin = AdminServer::start(
            AdminConfig::new(token.clone())
                .with_mirror_ack_timeout(MIRROR_ACK_TIMEOUT)
                .with_close_request_timeout(close_request_timeout),
        )
        .await
        .expect("the admin listener binds");
        state.install_admin(admin.clone());
        Self {
            _temp: temp,
            state,
            admin,
            token,
            surface,
        }
    }

    /// The WebView's data-plane connection, as far as the daemon can tell: one
    /// desktop router registration (what `/v2/ws`'s handshake installs) plus one
    /// event subscriber (what that connection's frame pump installs). **Both**
    /// must hold for a push to be `delivered`, so the pair *is* "this owner has
    /// a live WebView" — the returned receiver is where the test reads the
    /// frame the WebView would have gotten.
    fn attach_webview(&self, owner: &OwnerId) -> broadcast::Receiver<OwnerEvent> {
        assert!(
            self.state
                .host
                .register_desktop_owner_for_tests("ws-1", owner),
            "the WebView's data-plane connection registers as a desktop owner"
        );
        self.state.host_events.subscribe_for_tests()
    }

    async fn attach(&self) -> Client {
        let url = format!(
            "ws://{}{ADMIN_WS_PATH}",
            self.admin.origin().trim_start_matches("http://")
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(url)
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
                "pid": 4242,
            }),
        )
        .await;
        let welcome = next_json(&mut socket).await;
        assert_eq!(welcome["type"], "admin_welcome");
        socket
    }
}

async fn send_json(socket: &mut Client, frame: Value) {
    socket
        .send(Message::Text(frame.to_string()))
        .await
        .expect("frame sent");
}

/// Read frames until the `admin_result` for `id` arrives (the owner-mirror
/// events of `owner.create` travel first).
async fn await_result(socket: &mut Client, id: &str) -> Value {
    loop {
        let text = tokio::time::timeout(Duration::from_secs(15), socket.next())
            .await
            .expect("a frame within the deadline")
            .expect("the socket is open")
            .expect("a valid frame")
            .into_text()
            .expect("a text frame");
        let value: Value = serde_json::from_str(&text).expect("a JSON frame");
        if value["type"] == "admin_result" && value["id"] == id {
            return value;
        }
    }
}

async fn next_json(socket: &mut Client) -> Value {
    let text = tokio::time::timeout(Duration::from_secs(15), socket.next())
        .await
        .expect("a frame within the deadline")
        .expect("the socket is open")
        .expect("a valid frame")
        .into_text()
        .expect("a text frame");
    serde_json::from_str(&text).expect("a JSON frame")
}

async fn request(socket: &mut Client, id: &str, op: &str, args: Value) -> Value {
    send_json(
        socket,
        json!({"type": "admin_request", "id": id, "op": op, "args": args}),
    )
    .await;
    await_result(socket, id).await
}

/// Create one workspace owner and return its id.
async fn create_owner(socket: &mut Client) -> OwnerId {
    let response = request(
        socket,
        "r0",
        "owner.create",
        json!({
            "label": "w1",
            "cwd": "/workspace",
            "port": 3001,
            "origin": "http://127.0.0.1:3001",
        }),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");
    OwnerId::from_string(response["result"]["owner"].as_str().unwrap().to_string())
}

fn close_args(owner: &OwnerId, request_id: &str) -> Value {
    json!({"owner": owner.as_str(), "requestId": request_id})
}

/// The happy path: the push reaches the owner's WebView connection, the WebView's
/// data-plane `window_close_approve` settles the app surface, and the app's
/// long-poll is answered `approved`.
#[tokio::test(flavor = "multi_thread")]
async fn close_request_pushes_the_frame_and_the_approve_settles_it() {
    let fixture = Fixture::boot(CLOSE_REQUEST_SETTLE_TIMEOUT).await;
    let mut socket = fixture.attach().await;
    let owner = create_owner(&mut socket).await;
    let mut events = fixture.attach_webview(&owner);

    send_json(
        &mut socket,
        json!({
            "type": "admin_request",
            "id": "r1",
            "op": "owner.close_request",
            "args": close_args(&owner, "close-7"),
        }),
    )
    .await;

    // The pushed frame is the protocol table's literal.
    let pushed = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("the close request is pushed")
        .expect("the sink is live");
    assert_eq!(pushed.owner, Some(owner.clone()));
    assert_eq!(
        pushed.value,
        json!({"type": "window_close_request", "requestId": "close-7"}),
        "the data-plane frame the WebView coordinator consumes"
    );

    // The WebView approves over the data plane: the control dispatcher settles
    // the app surface (`close_window`) and then the waiting op.
    let result = drive_control(&fixture, &owner, "window_close_approve", json!("close-7")).await;
    assert!(
        result.is_null(),
        "the approve op answers Null, got {result}"
    );
    assert_eq!(
        fixture.surface.closed(),
        vec!["w1".to_string()],
        "the approve reached the app surface with the owner's label"
    );

    let response = await_result(&mut socket, "r1").await;
    assert_eq!(response["ok"], true, "{response}");
    let delivery = OwnerCloseDelivery::from_value(&response["result"]).expect("the table's reply");
    assert_eq!(
        delivery,
        OwnerCloseDelivery {
            request_id: "close-7".to_string(),
            delivered: true,
            outcome: CloseOutcome::Approved,
        }
    );
    assert!(!delivery.requires_native_fallback());
    fixture.state.shutdown_graceful("test").await;
}

/// The cancel path: same push, `cancelled` outcome — the app-surface cancel
/// runs, the app drops its pending close, and the window stays open with no
/// native dialog.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_settles_the_op_as_cancelled() {
    let fixture = Fixture::boot(CLOSE_REQUEST_SETTLE_TIMEOUT).await;
    let mut socket = fixture.attach().await;
    let owner = create_owner(&mut socket).await;
    let _webview = fixture.attach_webview(&owner);

    send_json(
        &mut socket,
        json!({
            "type": "admin_request",
            "id": "r1",
            "op": "owner.close_request",
            "args": close_args(&owner, "close-8"),
        }),
    )
    .await;
    // The op must be waiting before its settlement arrives, or the settlement
    // would have no waiter to resolve.
    wait_pending(&fixture, "close-8").await;

    drive_control(&fixture, &owner, "window_close_cancel", json!("close-8")).await;
    assert_eq!(
        fixture.surface.cancelled(),
        vec!["close-8".to_string()],
        "the cancel reached the app surface with the request id"
    );
    assert!(fixture.surface.closed().is_empty(), "cancel never closes");

    let response = await_result(&mut socket, "r1").await;
    assert_eq!(
        response["result"],
        json!({"requestId": "close-8", "delivered": true, "outcome": "cancelled"}),
        "{response}"
    );
    fixture.state.shutdown_graceful("test").await;
}

/// Degradation 1: no WebView holds the owner (closed/crashed renderer), so
/// there was nothing to ask — the app must fall back to its native dialog.
#[tokio::test(flavor = "multi_thread")]
async fn a_close_request_without_a_webview_is_undeliverable() {
    let fixture = Fixture::boot(CLOSE_REQUEST_SETTLE_TIMEOUT).await;
    let mut socket = fixture.attach().await;
    let owner = create_owner(&mut socket).await;

    let response = request(
        &mut socket,
        "r1",
        "owner.close_request",
        close_args(&owner, "close-9"),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");
    let delivery = OwnerCloseDelivery::from_value(&response["result"]).expect("the table's reply");
    assert_eq!(
        delivery,
        OwnerCloseDelivery {
            request_id: "close-9".to_string(),
            delivered: false,
            outcome: CloseOutcome::Undeliverable,
        }
    );
    assert!(
        delivery.requires_native_fallback(),
        "an undelivered request is the native-dialog path"
    );
    assert!(
        !fixture.state.close_requests.is_pending("close-9"),
        "an undelivered request leaves no waiter behind"
    );
    fixture.state.shutdown_graceful("test").await;
}

/// Degradation 2: the WebView connection is live but its coordinator never
/// answers. The op answers `timeout` on its deadline, and the waiter is gone
/// (a late settlement must not resurrect it).
#[tokio::test(flavor = "multi_thread")]
async fn a_silent_webview_times_out_and_leaves_no_waiter() {
    let fixture = Fixture::boot(Duration::from_millis(150)).await;
    let mut socket = fixture.attach().await;
    let owner = create_owner(&mut socket).await;
    let _webview = fixture.attach_webview(&owner);

    let response = request(
        &mut socket,
        "r1",
        "owner.close_request",
        close_args(&owner, "close-10"),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");
    let delivery = OwnerCloseDelivery::from_value(&response["result"]).expect("the table's reply");
    assert!(delivery.delivered, "the frame did reach the WebView");
    assert_eq!(delivery.outcome, CloseOutcome::Timeout);
    assert!(delivery.requires_native_fallback());
    assert!(
        !fixture.state.close_requests.is_pending("close-10"),
        "the deadline drops the waiter"
    );
    assert!(
        !fixture
            .state
            .resolve_owner_close_settlement("close-10", CloseOutcome::Approved),
        "a settlement after the deadline is not a live request"
    );
    fixture.state.shutdown_graceful("test").await;
}

/// The op is manager-gated like every other owner-lifecycle op; the shared
/// `a_stop_without_the_lease_is_refused_by_the_daemon` row in `admin_ws.rs`
/// covers it (it lists every lease-gated op).
///
/// An owner the daemon does not know is a refusal, never a silent no-op: the
/// app then runs its native dialog (and cannot mistake the answer for consent).
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_owner_is_refused() {
    let fixture = Fixture::boot(CLOSE_REQUEST_SETTLE_TIMEOUT).await;
    let mut socket = fixture.attach().await;
    let owner = OwnerId::from_string("not-an-owner".to_string());

    let response = request(
        &mut socket,
        "r1",
        "owner.close_request",
        close_args(&owner, "close-11"),
    )
    .await;
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown owner"),
        "{response}"
    );
    assert!(!fixture.state.close_requests.is_pending("close-11"));
    fixture.state.shutdown_graceful("test").await;
}

/// Drive one **canonical** control frame through the production dispatcher
/// ([`host_control_dispatch`]) as one owner's native client, and return the op
/// result as its wire value.
///
/// This is the half of the chain no WebSocket test can reach in this crate
/// (there is no `/v2/ws` desktop harness): the WebView's approve/cancel frames
/// arrive on the data plane, are normalized to `host_request` by `HostServer`,
/// and land in this handler — which is the code under test here, verbatim.
async fn drive_control(
    fixture: &Fixture,
    owner: &OwnerId,
    operation: &str,
    request_id: Value,
) -> Value {
    let handler = picot_host::host_control_dispatch::host_control_dispatch(fixture.state.clone());
    let ctx = VerifiedClientContext {
        client_id: 1,
        class: ClientClass::Native,
        owner_id: Some(owner.clone()),
        device_id: None,
    };
    handler(
        ctx,
        json!({"type": "host_request", "operation": operation, "args": {"requestId": request_id}}),
        Arc::new(|_| {}),
    )
    .await
    .unwrap_or_else(|error| panic!("{operation} failed in the dispatcher: {error}"))
}

/// Poll until the op has registered its waiter, so a settlement cannot race it.
async fn wait_pending(fixture: &Fixture, request_id: &str) {
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if fixture.state.close_requests.is_pending(request_id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("close request {request_id} never registered its waiter");
}
