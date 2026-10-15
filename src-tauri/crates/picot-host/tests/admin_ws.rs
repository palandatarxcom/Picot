// ABOUTME: Task 3 protocol tests: a real loopback AdminServer against real client sockets.
// ABOUTME: Manager lease (4401/4409), attachment epochs, mirror acks, drain order, id routing.

//! Plan Task 3 Step 1 (red first). Every test here connects a **real** client
//! socket (`tokio-tungstenite`) to a **real** loopback `AdminServer` served by
//! `axum` — there is no mock-vs-mock pair anywhere: the daemon side is the
//! production server and the client side is a real WS client speaking the
//! literal protocol frames.
//!
//! The frames are written as the plan's protocol tables spell them
//! (engineering-lesson #1): fixtures are typed by hand from the table, never
//! generated from the Rust types.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use picot_host::daemon::admin::{
    AdminConfig, AdminServer, ADMIN_CLOSE_BAD_TOKEN, ADMIN_CLOSE_LEASE_HELD, ADMIN_WS_PATH,
    MIRROR_ACK_TIMEOUT,
};
use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::{DaemonConfig, DaemonState, DrainObserver, DrainStep};
use picot_host::window_owner::OwnerId;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A surface that fails every call: these tests never exercise the app surface,
/// so any call is a bug and must not be silently plausible.
#[derive(Default)]
struct NeverCalledSurface;

#[async_trait::async_trait]
impl AppSurface for NeverCalledSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &picot_host::runtime_coordinator::RuntimeTarget,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Err("unused in admin tests".into())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused in admin tests".into())
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in admin tests".into())
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in admin tests".into())
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        Err("unused in admin tests".into())
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused in admin tests".into())
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        Err("unused in admin tests".into())
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused in admin tests".into())
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        Err("unused in admin tests".into())
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        Err("unused in admin tests".into())
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused in admin tests".into())
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        Err("unused in admin tests".into())
    }
}

/// Records the drain steps so the plan's fixed order is assertable.
#[derive(Default)]
struct StepLog(Mutex<Vec<DrainStep>>);

impl DrainObserver for StepLog {
    fn step(&self, step: DrainStep) {
        self.0.lock().unwrap().push(step);
    }
}

impl StepLog {
    fn steps(&self) -> Vec<DrainStep> {
        self.0.lock().unwrap().clone()
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: Arc<DaemonState>,
    admin: AdminServer,
    token: String,
}

impl Fixture {
    async fn boot(mirror_ack_timeout: Duration) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).expect("static dir");
        std::fs::write(static_dir.join("index.html"), "<h1>admin ws</h1>").expect("index");
        let state = Arc::new(
            DaemonState::start(
                DaemonConfig {
                    data_dir: temp.path().join("data"),
                    config_dir: temp.path().join("config"),
                    static_dir,
                    bundle_identifier: "com.palandata.picot.test".to_string(),
                },
                Arc::new(NeverCalledSurface),
            )
            .await
            .expect("the daemon host stack boots"),
        );
        let token = "test-admin-token-0123456789abcdef".to_string();
        let admin = AdminServer::start(
            AdminConfig::new(token.clone()).with_mirror_ack_timeout(mirror_ack_timeout),
        )
        .await
        .expect("the admin listener binds");
        state.install_admin(admin.clone());
        Self {
            _temp: temp,
            state,
            admin,
            token,
        }
    }

    fn admin_ws_url(&self) -> String {
        format!(
            "ws://{}{ADMIN_WS_PATH}",
            self.admin.origin().trim_start_matches("http://")
        )
    }

    /// Connect and complete the manager handshake; returns the socket and the
    /// `admin_welcome` frame.
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
                "pid": 4242,
            }),
        )
        .await;
        let welcome = match next_frame(&mut socket).await {
            Frame::Json(value) => value,
            other => panic!("expected admin_welcome, got {other:?}"),
        };
        assert_eq!(welcome["type"], "admin_welcome");
        (socket, welcome)
    }
}

#[derive(Debug)]
enum Frame {
    Json(Value),
    Closed(u16, String),
}

async fn connect(url: &str) -> Client {
    let (socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("a real admin client connects");
    socket
}

async fn send_json(socket: &mut Client, frame: Value) {
    socket
        .send(Message::Text(frame.to_string()))
        .await
        .expect("frame sent");
}

async fn send_raw(socket: &mut Client, frame: &str) {
    socket
        .send(Message::Text(frame.to_string()))
        .await
        .expect("frame sent");
}

/// Read the next frame, ignoring pings/pongs. Panics on a read timeout so a
/// stuck server cannot hang the suite.
async fn next_frame(socket: &mut Client) -> Frame {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(15), socket.next())
            .await
            .expect("a frame within the deadline")
            .expect("the socket is open")
            .expect("a valid frame");
        match message {
            Message::Text(text) => {
                return Frame::Json(serde_json::from_str(&text).expect("a JSON frame"));
            }
            Message::Close(Some(frame)) => {
                return Frame::Closed(u16::from(frame.code), frame.reason.to_string());
            }
            Message::Close(None) => return Frame::Closed(0, String::new()),
            _ => continue,
        }
    }
}

async fn next_json(socket: &mut Client) -> Value {
    match next_frame(socket).await {
        Frame::Json(value) => value,
        other => panic!("expected a JSON frame, got {other:?}"),
    }
}

/// One request/response round trip; asserts the response id matches, and also
/// returns the mirror frames the op published before answering.
///
/// `owner.create` publishes the new owner's row (Task 5) before its reply, so a
/// request helper that dropped those frames would silently eat an assertion's
/// evidence. The caller gets both.
async fn request_with_events(
    socket: &mut Client,
    id: &str,
    op: &str,
    args: Value,
) -> (Value, Vec<Value>) {
    send_json(
        socket,
        json!({"type": "admin_request", "id": id, "op": op, "args": args}),
    )
    .await;
    let mut events = Vec::new();
    let response = loop {
        let frame = next_json(socket).await;
        if frame["type"] == "admin_result" {
            break frame;
        }
        events.push(frame);
    };
    assert_eq!(
        response["id"], id,
        "responses are matched by id: {response}"
    );
    (response, events)
}

/// One request/response round trip; asserts the response id matches.
async fn request(socket: &mut Client, id: &str, op: &str, args: Value) -> Value {
    let (response, _events) = request_with_events(socket, id, op, args).await;
    response
}

/// JSON object keys, sorted: `serde_json` maps are unordered, so a table
/// comparison is a key-set comparison, not a key-order one.
fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn owner_args(label: &str) -> Value {
    json!({
        "label": label,
        "cwd": "/workspace",
        "port": 3001,
        "origin": "http://127.0.0.1:3001",
    })
}

/// Poll until `predicate` holds, or fail after `deadline`.
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

// ---------------------------------------------------------------------------
// Step 1 assertions
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn bad_admin_token_is_closed_with_4401() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let mut socket = connect(&fixture.admin_ws_url()).await;
    send_json(
        &mut socket,
        json!({
            "type": "admin_hello",
            "adminToken": "not-the-token",
            "role": "manager",
        }),
    )
    .await;
    match next_frame(&mut socket).await {
        Frame::Closed(code, reason) => {
            assert_eq!(code, ADMIN_CLOSE_BAD_TOKEN, "close reason: {reason}");
        }
        other => panic!("expected a 4401 close, got {other:?}"),
    }
    assert!(!fixture.admin.has_manager(), "no lease may be granted");
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn second_manager_is_rejected_with_4409_and_the_incumbent_still_works() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut incumbent, welcome) = fixture.attach().await;
    let first_epoch = welcome["epoch"].as_u64().expect("epoch");

    let mut challenger = connect(&fixture.admin_ws_url()).await;
    send_json(
        &mut challenger,
        json!({
            "type": "admin_hello",
            "adminToken": fixture.token,
            "role": "manager",
            "appVersion": "0.5.0",
            "buildId": "test-build",
            "pid": 9999,
        }),
    )
    .await;
    match next_frame(&mut challenger).await {
        Frame::Closed(code, reason) => {
            assert_eq!(code, ADMIN_CLOSE_LEASE_HELD, "close reason: {reason}");
        }
        other => panic!("expected a 4409 close, got {other:?}"),
    }

    // The incumbent is undisturbed: same epoch, and its ops still work.
    assert_eq!(
        fixture.admin.epoch(),
        first_epoch,
        "a rejected hello must not increment the epoch"
    );
    let status = request(&mut incumbent, "r1", "daemon.status", json!({})).await;
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(status["result"]["epoch"], first_epoch);
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_reports_version_build_id_and_pid() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, _) = fixture.attach().await;
    let response = request(&mut socket, "r1", "daemon.status", json!({})).await;

    // Protocol table: version + buildId + pid (plus the daemon origins/epoch).
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(response["result"]["buildId"], picot_host::BUILD_ID);
    assert_eq!(
        response["result"]["pid"].as_u64(),
        Some(u64::from(std::process::id())),
        "{response}"
    );
    assert_eq!(response["result"]["hostOrigin"], fixture.state.origin());
    assert_eq!(response["result"]["adminOrigin"], fixture.admin.origin());
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_create_mints_a_capability_that_authenticates() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, welcome) = fixture.attach().await;
    let response = request(&mut socket, "r1", "owner.create", owner_args("w1")).await;
    assert_eq!(response["ok"], true, "{response}");

    let owner = OwnerId::from_string(response["result"]["owner"].as_str().unwrap().to_string());
    let capability = response["result"]["capability"].as_str().unwrap();
    assert_eq!(
        fixture.state.owner_registry.authenticate(capability),
        Some(owner.clone()),
        "the minted capability must pass owner-registry authentication"
    );
    assert_eq!(
        fixture
            .state
            .owner_registry
            .label_for_owner(&owner)
            .as_deref(),
        Some("w1")
    );
    assert_eq!(
        fixture.state.owner_registry.attachment_epoch(),
        welcome["epoch"].as_u64().unwrap(),
        "the owner is stamped with the granting attachment epoch"
    );
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_welcome_carries_the_full_mirror_snapshot() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    // Owners that exist before the attachment (the reattach case) must appear
    // in the snapshot the welcome frame carries.
    let (owner, _capability) = fixture
        .state
        .owner_registry
        .create_owner_with_workspace(
            "w1".to_string(),
            std::path::PathBuf::from("/workspace"),
            3001,
            "http://127.0.0.1:3001".to_string(),
            None,
            picot_host::window_owner::TemporaryKind::DefaultStartup,
        )
        .expect("owner");
    let (generation, permit_id) = fixture
        .state
        .owner_registry
        .prepare_navigation_with_permit(
            &owner,
            3002,
            std::path::PathBuf::from("/workspace"),
            "http://127.0.0.1:3002/workspaces/w1/sessions/s1".to_string(),
            Duration::from_secs(30),
        )
        .expect("permit");
    assert!(!permit_id.is_empty());
    fixture
        .state
        .owner_registry
        .commit_workspace_transition_with_workspace(
            &owner,
            generation,
            "http://127.0.0.1:3002/workspaces/w1/sessions/s1".to_string(),
            Some("w1".to_string()),
            picot_host::window_owner::TemporaryKind::DefaultStartup,
        )
        .expect("commit");

    let (_socket, welcome) = fixture.attach().await;
    let owners = welcome["mirror"]["owners"]
        .as_array()
        .expect("owners array");
    assert_eq!(owners.len(), 1, "{welcome}");
    let row = &owners[0];
    assert_eq!(
        keys(row),
        vec![
            "currentOrigin",
            "generation",
            "label",
            "owner",
            "sessionRoute",
            "workspaceId"
        ],
        "mirror row must match the protocol table exactly: {row}"
    );
    assert_eq!(row["owner"], owner.as_str());
    assert_eq!(row["label"], "w1");
    assert_eq!(row["currentOrigin"], "http://127.0.0.1:3002");
    assert_eq!(row["workspaceId"], "w1");
    assert_eq!(row["sessionRoute"], "/workspaces/w1/sessions/s1");
    assert_eq!(row["generation"], generation);
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_stop_drains_in_order_and_clears_bookkeeping() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let steps = Arc::new(StepLog::default());
    fixture.state.set_drain_observer(steps.clone());
    let (mut socket, _) = fixture.attach().await;
    assert!(fixture.admin.has_manager(), "the lease is held");

    let response = request(&mut socket, "r1", "daemon.stop", json!({})).await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["result"],
        json!({ "accepted": true }),
        "{response}"
    );

    // The admin socket is closed as part of the drain (step 2).
    match next_frame(&mut socket).await {
        Frame::Closed(code, reason) => {
            assert_eq!(code, 1001, "close reason: {reason}");
        }
        other => panic!("expected the admin WS to close, got {other:?}"),
    }

    // Steps 3-5 run *after* the close frame above, so this has to wait for the
    // drain to finish rather than assert instantaneously: the reply is
    // deliberately sent before the drain starts ("accepted" ≠ "completed",
    // plan D12), and asserting immediately races a loaded machine.
    wait_until(
        Duration::from_secs(10),
        || steps.steps().len() >= 5,
        "the drain to reach its file and exit steps",
    )
    .await;
    assert_eq!(
        steps.steps(),
        vec![
            DrainStep::StopAcceptingAdminFrames,
            DrainStep::CloseAdminWs,
            DrainStep::DrainRuntimes,
            DrainStep::DrainFiles,
            DrainStep::Exit,
        ],
        "the drain order is fixed by the plan"
    );
    assert!(
        !fixture.admin.manager_lease_held(),
        "the manager bookkeeping is cleared"
    );
    assert_eq!(fixture.admin.connection_count(), 0);

    // "accepted" ≠ "completed": the listener really stops accepting.
    let address = fixture
        .admin
        .origin()
        .trim_start_matches("http://")
        .to_string();
    let mut refused = false;
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        match tokio::net::TcpStream::connect(&address).await {
            Ok(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            Err(_) => {
                refused = true;
                break;
            }
        }
    }
    assert!(
        refused,
        "the admin listener must stop accepting connections"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mirror_ack_timeout_fails_the_triggering_op() {
    let fixture = Fixture::boot(Duration::from_millis(300)).await;
    let (mut socket, welcome) = fixture.attach().await;
    let epoch = welcome["epoch"].as_u64().unwrap();
    let (created, events) =
        request_with_events(&mut socket, "r1", "owner.create", owner_args("w1")).await;
    let owner = OwnerId::from_string(created["result"]["owner"].as_str().unwrap().to_string());
    // The creation also publishes the owner's mirror row (Task 5): the app's
    // navigation delegate is fed by these rows, so they must precede the
    // window's first load.
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["type"], "owner_binding_changed", "{}", events[0]);
    assert_eq!(events[0]["owner"], owner.as_str());
    assert_eq!(events[0]["currentOrigin"], "http://127.0.0.1:3001");

    let state = Arc::clone(&fixture.state);
    let op_owner = owner.clone();
    let op = tokio::spawn(async move {
        state
            .prepare_navigation_with_mirror_ack(
                &op_owner,
                3002,
                std::path::PathBuf::from("/workspace"),
                "http://127.0.0.1:3002/workspaces/w1/sessions/s1".to_string(),
                Duration::from_secs(30),
            )
            .await
    });

    // The permit is pushed, and it matches the table.
    let permit = next_json(&mut socket).await;
    assert_eq!(permit["type"], "navigation_permit", "{permit}");
    assert_eq!(permit["epoch"], epoch);
    assert_eq!(permit["owner"], owner.as_str());
    assert_eq!(permit["origin"], "http://127.0.0.1:3002");
    assert!(permit["permitId"].is_string(), "{permit}");
    assert!(permit["expiresAtMs"].as_u64().unwrap() > 0, "{permit}");
    assert!(permit["generation"].as_u64().is_some(), "{permit}");

    // Deliberately no mirror_ack: the granting op must fail (fail-safe).
    let outcome = tokio::time::timeout(Duration::from_secs(10), op)
        .await
        .expect("the op settles within the deadline")
        .expect("the op task does not panic");
    let error = outcome.expect_err("an unacknowledged permit must fail the op");
    assert!(
        error.contains("acknowledge"),
        "the error must name the missing ack: {error}"
    );
    assert_eq!(
        fixture.state.owner_registry.pending_target_origin(&owner),
        None,
        "the permit is revoked so the WebView can never reach it"
    );
    let revoked = next_json(&mut socket).await;
    assert_eq!(revoked["type"], "permit_revoked", "{revoked}");
    assert_eq!(revoked["epoch"], epoch);

    // Positive path: with an ack the same op succeeds.
    let state = Arc::clone(&fixture.state);
    let op_owner = owner.clone();
    let op = tokio::spawn(async move {
        state
            .prepare_navigation_with_mirror_ack(
                &op_owner,
                3002,
                std::path::PathBuf::from("/workspace"),
                "http://127.0.0.1:3002/workspaces/w1/sessions/s1".to_string(),
                Duration::from_secs(30),
            )
            .await
    });
    let permit = next_json(&mut socket).await;
    assert_eq!(permit["type"], "navigation_permit", "{permit}");
    send_json(
        &mut socket,
        json!({
            "type": "mirror_ack",
            "permitId": permit["permitId"],
            "epoch": permit["epoch"],
        }),
    )
    .await;
    let generation = tokio::time::timeout(Duration::from_secs(10), op)
        .await
        .expect("the acked op settles")
        .expect("no panic")
        .expect("an acknowledged permit completes the op");
    assert_eq!(generation, permit["generation"].as_u64().unwrap());
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn responses_are_matched_by_id_when_completions_arrive_out_of_order() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    fixture
        .admin
        .set_op_delay_for_tests("owner.create", Duration::from_millis(400));
    let (mut socket, _) = fixture.attach().await;

    // r1 is slow, r2 fast; unrelated frames are interleaved between them.
    send_json(
        &mut socket,
        json!({"type": "admin_request", "id": "r1", "op": "owner.create", "args": owner_args("w1")}),
    )
    .await;
    send_json(
        &mut socket,
        json!({"type": "admin_request", "id": "r2", "op": "daemon.status", "args": {}}),
    )
    .await;
    send_json(
        &mut socket,
        json!({"type": "mirror_ack", "permitId": "unknown", "epoch": 1}),
    )
    .await;
    send_json(
        &mut socket,
        json!({"type": "surface_response", "id": "s1", "ok": true, "result": {}}),
    )
    .await;

    let first = next_json(&mut socket).await;
    let row = next_json(&mut socket).await;
    let second = next_json(&mut socket).await;
    assert_eq!(first["id"], "r2", "the fast op answers first: {first}");
    assert_eq!(
        first["result"]["pid"].as_u64(),
        Some(u64::from(std::process::id()))
    );
    // r1 (the delayed op) publishes its owner row, then its own answer.
    assert_eq!(row["type"], "owner_binding_changed", "{row}");
    assert!(row["id"].is_null(), "a mirror row is not a reply: {row}");
    assert_eq!(second["id"], "r1", "the slow op still answers its own id");
    assert!(
        second["result"]["capability"].is_string(),
        "r1's payload is r1's, not r2's: {second}"
    );

    // A control plane that worked while the slow op was in flight: the op
    // delay must not stall the reader loop.
    let status = request(&mut socket, "r3", "daemon.status", json!({})).await;
    assert_eq!(status["ok"], true, "{status}");
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_attachment_revokes_its_epoch_and_frees_the_label() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, welcome) = fixture.attach().await;
    let epoch = welcome["epoch"].as_u64().unwrap();
    let created = request(&mut socket, "r1", "owner.create", owner_args("w1")).await;
    let owner = OwnerId::from_string(created["result"]["owner"].as_str().unwrap().to_string());
    let capability = created["result"]["capability"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        fixture.state.owner_registry.authenticate(&capability),
        Some(owner.clone())
    );

    // The app dies (socket gone). After the grace window the attachment is
    // dead: capabilities revoked, labels freed (plan D11).
    socket.close(None).await.expect("close");
    wait_until(
        Duration::from_secs(6),
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
        "the owner record is gone with its epoch"
    );
    assert!(
        !fixture.admin.has_manager(),
        "the dead attachment no longer holds the lease"
    );
    assert!(epoch > 0);

    // The label is free again: a later attachment can recreate it.
    assert!(fixture
        .state
        .owner_registry
        .create_owner(
            "w1".to_string(),
            std::path::PathBuf::from("/workspace"),
            3001,
            "http://127.0.0.1:3001".to_string(),
        )
        .is_ok());
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mirror_event_frames_match_the_protocol_table() {
    let fixture = Fixture::boot(Duration::from_millis(300)).await;
    let (mut socket, welcome) = fixture.attach().await;
    let epoch = welcome["epoch"].as_u64().unwrap();
    let (created, events) =
        request_with_events(&mut socket, "r1", "owner.create", owner_args("w1")).await;
    let owner = OwnerId::from_string(created["result"]["owner"].as_str().unwrap().to_string());
    // The owner-creation row (Task 5), then the permit.
    assert_eq!(events.len(), 1, "{events:?}");
    let created_row = &events[0];
    assert_eq!(
        created_row["type"], "owner_binding_changed",
        "{created_row}"
    );
    assert_eq!(
        keys(created_row),
        vec![
            "currentOrigin",
            "epoch",
            "generation",
            "hasWorkspace",
            "label",
            "owner",
            "sessionRoute",
            "type",
            "workspaceId"
        ],
        "the creation row must match the protocol table exactly: {created_row}"
    );
    assert_eq!(created_row["label"], "w1");
    assert_eq!(created_row["hasWorkspace"], false);
    assert_eq!(created_row["currentOrigin"], "http://127.0.0.1:3001");
    assert_eq!(created_row["generation"], 0);

    // navigation_permit, pushed by the registry itself.
    let (generation, permit_id) = fixture
        .state
        .owner_registry
        .prepare_navigation_with_permit(
            &owner,
            3002,
            std::path::PathBuf::from("/workspace"),
            "http://127.0.0.1:3002/workspaces/w1/sessions/s1".to_string(),
            Duration::from_secs(30),
        )
        .expect("permit prepared");
    let permit = next_json(&mut socket).await;
    assert_eq!(
        keys(&permit),
        vec![
            "epoch",
            "expiresAtMs",
            "generation",
            "origin",
            "owner",
            "permitId",
            "type"
        ],
        "{permit}"
    );
    assert_eq!(permit["type"], "navigation_permit");
    assert_eq!(permit["epoch"], epoch);
    assert_eq!(permit["permitId"], permit_id);
    assert_eq!(permit["owner"], owner.as_str());
    assert_eq!(permit["origin"], "http://127.0.0.1:3002");
    assert_eq!(permit["generation"], generation);

    // Cancel pushes permit_revoked.
    fixture
        .state
        .owner_registry
        .cancel_workspace_transition(&owner, generation)
        .expect("cancel");
    let revoked = next_json(&mut socket).await;
    assert_eq!(
        keys(&revoked),
        vec!["epoch", "permitId", "type"],
        "{revoked}"
    );
    assert_eq!(revoked["type"], "permit_revoked");
    assert_eq!(revoked["permitId"], permit_id);

    // Commit pushes owner_binding_changed.
    let (generation, permit_id) = fixture
        .state
        .owner_registry
        .prepare_navigation_with_permit(
            &owner,
            3002,
            std::path::PathBuf::from("/workspace"),
            "http://127.0.0.1:3002/workspaces/w1/sessions/s1".to_string(),
            Duration::from_secs(30),
        )
        .expect("permit prepared");
    assert!(!permit_id.is_empty());
    let _ = next_json(&mut socket).await;
    fixture
        .state
        .owner_registry
        .commit_workspace_transition_with_workspace(
            &owner,
            generation,
            "http://127.0.0.1:3002/workspaces/w1/sessions/s1".to_string(),
            Some("w1".to_string()),
            picot_host::window_owner::TemporaryKind::DefaultStartup,
        )
        .expect("commit");
    let binding = next_json(&mut socket).await;
    assert_eq!(
        keys(&binding),
        vec![
            "currentOrigin",
            "epoch",
            "generation",
            "hasWorkspace",
            "label",
            "owner",
            "sessionRoute",
            "type",
            "workspaceId"
        ],
        "{binding}"
    );
    assert_eq!(binding["label"], "w1");
    assert_eq!(
        binding["sessionRoute"], "/workspaces/w1/sessions/s1",
        "{binding}"
    );
    assert_eq!(binding["workspaceId"], "w1");
    assert_eq!(binding["type"], "owner_binding_changed");
    assert_eq!(binding["epoch"], epoch);
    assert_eq!(binding["owner"], owner.as_str());
    assert_eq!(binding["hasWorkspace"], true);
    assert_eq!(binding["generation"], generation);
    assert_eq!(
        binding["currentOrigin"], "http://127.0.0.1:3002",
        "the committed origin is what the app's mirror authorizes: {binding}"
    );

    // The mirror snapshot now carries the committed route.
    let snapshot = fixture.state.owner_registry.mirror_snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].workspace_id.as_deref(), Some("w1"));
    assert_eq!(
        snapshot[0].session_route.as_deref(),
        Some("/workspaces/w1/sessions/s1")
    );
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_pre_hello_frames_close_with_4410() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let mut socket = connect(&fixture.admin_ws_url()).await;
    send_raw(
        &mut socket,
        r#"{"type":"admin_request","id":"r1","op":"daemon.status"}"#,
    )
    .await;
    match next_frame(&mut socket).await {
        Frame::Closed(code, reason) => {
            assert_eq!(
                code,
                picot_host::daemon::admin::ADMIN_CLOSE_MALFORMED,
                "close reason: {reason}"
            );
        }
        other => panic!("expected a 4410 close, got {other:?}"),
    }
    fixture.state.shutdown_graceful("test").await;
}

/// Plan Task 4 Step 1: "concurrent adopt — two clients race hello on a
/// managerless daemon → exactly one lease, one 4409".
///
/// The two hellos are sent *before* either frame is read, so the daemon sees
/// them interleaved on two connections: the lease must be granted exactly once,
/// and the loser must not disturb the winner.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_manager_hellos_grant_exactly_one_lease() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let mut first = connect(&fixture.admin_ws_url()).await;
    let mut second = connect(&fixture.admin_ws_url()).await;
    let hello = json!({
        "type": "admin_hello",
        "adminToken": fixture.token,
        "role": "manager",
        "appVersion": "0.5.0",
        "buildId": "test-build",
        "pid": 11,
    });
    send_json(&mut first, hello.clone()).await;
    send_json(&mut second, hello).await;

    let mut granted = Vec::new();
    let mut rejected = 0;
    for (index, socket) in [&mut first, &mut second].into_iter().enumerate() {
        match next_frame(socket).await {
            Frame::Json(value) => {
                assert_eq!(value["type"], "admin_welcome", "client {index}: {value}");
                assert_eq!(
                    value["epoch"].as_u64(),
                    Some(1),
                    "exactly one epoch was dolled out"
                );
                granted.push(index);
            }
            Frame::Closed(code, _reason) => {
                assert_eq!(code, ADMIN_CLOSE_LEASE_HELD, "client {index}");
                rejected += 1;
            }
        }
    }
    assert_eq!(granted.len(), 1, "one lease, not two: {granted:?}");
    assert_eq!(rejected, 1, "the other client is closed 4409");
    assert_eq!(fixture.admin.epoch(), 1, "a rejected hello adds no epoch");
    assert_eq!(
        fixture.admin.connection_count(),
        1,
        "the rejected socket is gone; only the winner is attached"
    );

    // The winner is undisturbed and still holds the lease.
    let winner = if granted[0] == 0 {
        &mut first
    } else {
        &mut second
    };
    let status = request(winner, "r1", "daemon.status", json!({})).await;
    assert_eq!(status["ok"], true, "{status}");
    fixture.state.shutdown_graceful("test").await;
}

/// Plan Task 4 Step 1's last row: a stop without the lease is refused **by the
/// daemon**, whatever the caller believes. The manager's own version of this
/// check (`BlockedByOtherApp` / `StopWithoutLease`) is only the app-side half.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_without_the_lease_is_refused_by_the_daemon() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, _welcome) = fixture.attach().await;
    assert!(fixture.admin.has_manager(), "the lease is held");

    // Drop the lease while the connection stays open: the strongest form of
    // "this caller does not hold the lease" a live socket can be in (the
    // production path is a superseded attachment; `clear_bookkeeping` is the
    // seam that reaches it without a reconnect).
    fixture.admin.clear_bookkeeping();
    assert!(!fixture.admin.manager_lease_held(), "the lease is gone");

    for (id, op) in [
        ("r1", "daemon.stop"),
        ("r2", "owner.create"),
        ("r3", "owner.detach"),
    ] {
        let response = request(&mut socket, id, op, owner_args("w-leaseless")).await;
        assert_eq!(response["ok"], false, "{op} must be refused: {response}");
        assert!(
            response["error"]
                .as_str()
                .unwrap_or_default()
                .contains("manager lease"),
            "{op} must say why: {response}"
        );
    }
    fixture.state.shutdown_graceful("test").await;
}
