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
#[path = "support/mock_relay.rs"]
mod mock_relay;

use mock_relay::MockRelay;
use picot_host::relay::controller::RelayStartPlan;
use picot_host::relay::pairing::{parse_connection_offer, PairingOffer, DEFAULT_RELAY_ENDPOINT};
use picot_host::relay::transport::{TransportSeams, TransportTimings};
use picot_host::remote_auth::PairingError;
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
        _label: &str,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Err("unused in admin tests".into())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused in admin tests".into())
    }

    async fn focus_window(&self, _label: &str) -> Result<(), String> {
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
    temp: tempfile::TempDir,
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
            temp,
            state,
            admin,
            token,
        }
    }

    /// The fixture's own root: the test can create a workspace under it and
    /// register it with the daemon's metadata store.
    fn temp(&self) -> &std::path::Path {
        self.temp.path()
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
            .expect("the socket is open");
        // A kicked session ends in an abrupt TCP reset (revocation drops the
        // socket without a close handshake): that is a close, not a protocol
        // violation the harness should panic on.
        let message = match message {
            Ok(message) => message,
            Err(tokio_tungstenite::tungstenite::Error::Protocol(
                tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
            )) => return Frame::Closed(1006, "reset without closing handshake".into()),
            Err(error) => panic!("a valid frame: {error}"),
        };
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

/// Fast transport timings so relay state transitions are observable in
/// seconds, not the production tens-of-seconds (same shape as the relay e2e
/// suite).
fn relay_op_seams() -> TransportSeams {
    TransportSeams {
        timings: TransportTimings {
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(2),
            ping_interval: Duration::from_millis(50),
            stale_timeout: Duration::from_millis(400),
            ready_timeout: Duration::from_millis(400),
            handshake_timeout: Duration::from_secs(2),
            data_open_timeout: Duration::from_secs(2),
            e2ee_handshake_timeout: Duration::from_secs(5),
        },
        sleep: Arc::new(|duration: Duration| Box::pin(tokio::time::sleep(duration))),
        connector: None,
    }
}

/// Point the daemon's relay controller at `relay` over plain WS (the mock has
/// no TLS), then enable through the admin op the way the UI will.
async fn enable_relay_against_mock(
    fixture: &Fixture,
    socket: &mut Client,
    relay: &MockRelay,
) -> Value {
    fixture
        .state
        .relay
        .set_start_plan_for_tests(RelayStartPlan {
            endpoint: relay.addr().to_string(),
            use_tls: false,
            seams: relay_op_seams(),
        });
    let response = request(
        socket,
        "r-enable",
        "relay.set_enabled",
        json!({ "enabled": true }),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"], json!({ "enabled": true }), "{response}");
    response
}

/// Poll `relay.status` through the admin op until its state equals `want`.
async fn relay_status_reaches(socket: &mut Client, want: &str, what: &str) -> Value {
    let started = std::time::Instant::now();
    loop {
        let status = request(socket, "r-status", "relay.status", json!({})).await;
        assert_eq!(status["ok"], true, "{status}");
        if status["result"]["state"] == want {
            return status["result"].clone();
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timed out waiting for relay state {what} (last: {status})"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// One `relay.offer` round trip minted for `owner`, asserted ok.
async fn relay_offer(socket: &mut Client, id: &str, owner: &str) -> PairingOffer {
    let response = request(socket, id, "relay.offer", json!({ "ownerId": owner })).await;
    assert_eq!(response["ok"], true, "{response}");
    let url = response["result"]["url"]
        .as_str()
        .expect("offer url")
        .to_owned();
    let expires_at = response["result"]["expiresAt"].as_u64().expect("expiresAt");
    PairingOffer { url, expires_at }
}

/// Create one owner through the admin op and return its id — the identity a
/// pairing offer stamps (spec §2.8 device principal).
async fn create_owner(socket: &mut Client, label: &str) -> String {
    let response = request(socket, "r-owner", "owner.create", owner_args(label)).await;
    assert_eq!(response["ok"], true, "{response}");
    response["result"]["owner"]
        .as_str()
        .expect("owner id")
        .to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_status_defaults_to_disabled_and_offline() {
    // The default endpoint comes from the production plan, which reads
    // PICOT_RELAY_ENDPOINT; pin the env so the assertion cannot drift with
    // the developer's shell.
    let saved = std::env::var("PICOT_RELAY_ENDPOINT").ok();
    std::env::remove_var("PICOT_RELAY_ENDPOINT");
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, _) = fixture.attach().await;
    let response = request(&mut socket, "r1", "relay.status", json!({})).await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["result"],
        json!({
            "enabled": false,
            "state": "offline",
            "endpoint": DEFAULT_RELAY_ENDPOINT,
            "serverId": Value::Null,
        }),
        "{response}"
    );
    drop(fixture);
    if let Some(saved) = saved {
        std::env::set_var("PICOT_RELAY_ENDPOINT", saved);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_set_enabled_starts_the_transport_and_reaches_online() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let relay = MockRelay::start().await;
    let (mut socket, _) = fixture.attach().await;
    enable_relay_against_mock(&fixture, &mut socket, &relay).await;

    let status = relay_status_reaches(&mut socket, "online", "online against the mock").await;
    assert_eq!(status["enabled"], true, "{status}");
    let server_id = status["serverId"]
        .as_str()
        .expect("identity minted on enable");
    assert!(!server_id.is_empty());
    // The enabled flag is the persisted preference (status re-reads the
    // store), and the identity is stable across reads.
    let again = relay_status_reaches(&mut socket, "online", "a second status read").await;
    assert_eq!(again["serverId"], server_id, "{again}");

    // The flip back to offline is the state machine's third state.
    let response = request(
        &mut socket,
        "r2",
        "relay.set_enabled",
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");
    let status = relay_status_reaches(&mut socket, "offline", "offline after disable").await;
    assert_eq!(status["enabled"], false, "{status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_disable_revokes_pending_tokens_but_keeps_paired_devices() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let relay = MockRelay::start().await;
    let (mut socket, _) = fixture.attach().await;
    enable_relay_against_mock(&fixture, &mut socket, &relay).await;
    relay_status_reaches(&mut socket, "online", "online against the mock").await;

    // A paired device token (the thing that must survive the flip) and a
    // pending pairing token (the thing that must not).
    let auth = fixture.state.relay.remote_auth_for_tests();
    let pairing = auth.lock().unwrap().create_pairing(1_000, None);
    let device_token = auth
        .lock()
        .unwrap()
        .exchange(&pairing.token, "relay-op-device", 1_001)
        .expect("seeded device token");
    let owner = create_owner(&mut socket, "relay-offer-owner").await;
    let pending = relay_offer(&mut socket, "r-offer-1", &owner).await;

    let response = request(
        &mut socket,
        "r3",
        "relay.set_enabled",
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");

    let offer = parse_connection_offer(&pending.url).expect("offer parses");
    let token = offer.pairing_token.expect("token embedded");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let rejected = auth.lock().unwrap().exchange(&token, "late-device", now);
    assert_eq!(
        rejected.err(),
        Some(PairingError::InvalidOrUsed),
        "disable clears unconsumed pairing tokens (spec §2.10)"
    );
    assert!(
        auth.lock().unwrap().authorize(&device_token).unwrap(),
        "paired device tokens survive a relay disable (spec §2.10)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_offer_is_refused_while_relay_is_disabled() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, _) = fixture.attach().await;
    let owner = create_owner(&mut socket, "relay-off-owner").await;
    let response = request(
        &mut socket,
        "r1",
        "relay.offer",
        json!({ "ownerId": owner }),
    )
    .await;
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"].as_str().unwrap().contains("not enabled"),
        "the refusal must say relay is not enabled: {response}"
    );
    // No implicit enable: status stays disabled and no identity was minted.
    let status = request(&mut socket, "r2", "relay.status", json!({})).await;
    assert_eq!(status["result"]["enabled"], false, "{status}");
    assert_eq!(status["result"]["serverId"], Value::Null, "{status}");
}
/// T10 (spec §2.9): `relay.revoke_device` revokes the device's tokens and
/// kicks its **live loopback remote session** — the connection closes, the
/// same device token cannot reconnect, and revoking an already-gone device
/// is a no-op `{ok: true}`. No relay transport needed: revocation is
/// DB + sessions.
#[tokio::test(flavor = "multi_thread")]
async fn relay_revoke_device_kicks_the_live_loopback_session() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, _welcome) = fixture.attach().await;
    let owner = create_owner(&mut socket, "revoke-owner").await;

    let auth = fixture.state.relay.remote_auth_for_tests();
    let pairing = auth.lock().unwrap().create_pairing(1_000, Some(&owner));
    let device_token = auth
        .lock()
        .unwrap()
        .exchange(&pairing.token, "revoke-phone", 1_001)
        .expect("seeded device token");

    // The paired device holds a live loopback remote session.
    let host_ws = fixture.state.host.origin().replacen("http://", "ws://", 1) + "/v2/ws";
    let (mut remote, _) = tokio_tungstenite::connect_async(&host_ws).await.unwrap();
    remote
        .send(Message::Text(
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "remote",
                "clientId": "revoke-phone-session", "deviceToken": device_token
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let ack = next_json(&mut remote).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");
    for _ in 0..400 {
        if fixture
            .state
            .host
            .remote_device_sessions_for_tests("revoke-phone")
            == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        fixture
            .state
            .host
            .remote_device_sessions_for_tests("revoke-phone"),
        1,
        "the loopback remote session registered as kickable"
    );

    // Revoke: the record dies first, then the live session is kicked.
    let response = request(
        &mut socket,
        "r-revoke",
        "relay.revoke_device",
        json!({ "deviceId": "revoke-phone" }),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"], json!({ "ok": true }), "{response}");
    match next_frame(&mut remote).await {
        Frame::Closed(..) => {}
        other => panic!("the kicked session's socket must close, got {other:?}"),
    }

    // The revoked token cannot reconnect (hello-time DB check).
    let (mut again, _) = tokio_tungstenite::connect_async(&host_ws).await.unwrap();
    again
        .send(Message::Text(
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "remote",
                "clientId": "revoke-phone-retry", "deviceToken": device_token
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let rejected = next_json(&mut again).await;
    assert_eq!(
        rejected["error"]["code"], "unauthorized_device",
        "{rejected}"
    );

    // Revoking an already-gone device is a no-op success.
    let response = request(
        &mut socket,
        "r-revoke-again",
        "relay.revoke_device",
        json!({ "deviceId": "revoke-phone" }),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");

    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_pairing_offer_round_trips_and_supersedes_the_prior_token() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let relay = MockRelay::start().await;
    let (mut socket, _) = fixture.attach().await;
    enable_relay_against_mock(&fixture, &mut socket, &relay).await;
    let status = relay_status_reaches(&mut socket, "online", "online against the mock").await;
    let owner = create_owner(&mut socket, "relay-pair-owner").await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let first = relay_offer(&mut socket, "r-offer-1", &owner).await;
    assert!(
        first.url.starts_with("https://picot.021521.xyz/#offer="),
        "{}",
        first.url
    );
    assert!(
        first.expires_at > now && first.expires_at <= now + 300,
        "5-minute TTL: {} at {}",
        first.expires_at,
        now
    );
    let offer = parse_connection_offer(&first.url).expect("offer url round-trips");
    assert_eq!(offer.v, 2);
    assert_eq!(offer.server_id, status["serverId"].as_str().unwrap());
    assert_eq!(offer.relay.endpoint, relay.addr().to_string());
    assert_eq!(offer.pairing_token_expires_at, Some(first.expires_at));

    // The second mint kills the first token (spec §2.10 `superseded`).
    let second = relay_offer(&mut socket, "r-offer-2", &owner).await;
    let second_token = parse_connection_offer(&second.url)
        .unwrap()
        .pairing_token
        .unwrap();
    assert_ne!(second_token, offer.pairing_token.clone().unwrap());
    let auth = fixture.state.relay.remote_auth_for_tests();
    assert_eq!(
        auth.lock()
            .unwrap()
            .exchange(offer.pairing_token.as_deref().unwrap(), "sup", now)
            .err(),
        Some(PairingError::InvalidOrUsed),
        "the first token dies the moment the second offer is minted"
    );
    let device_token = auth
        .lock()
        .unwrap()
        .exchange(&second_token, "sup", now)
        .expect("the fresh token exchanges");
    // The relay offer stamps the requesting owner (spec §2.8): the exchanged
    // device token carries that owner as its principal binding.
    assert_eq!(
        auth.lock().unwrap().authenticate(&device_token).unwrap(),
        Some(picot_host::remote_auth::DevicePrincipal {
            device_id: "sup".into(),
            owner_id: Some(owner),
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_offer_with_an_unknown_owner_is_refused_without_minting() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let relay = MockRelay::start().await;
    let (mut socket, _) = fixture.attach().await;
    enable_relay_against_mock(&fixture, &mut socket, &relay).await;
    let response = request(
        &mut socket,
        "r-ghost",
        "relay.offer",
        json!({ "ownerId": "no-such-owner" }),
    )
    .await;
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("unknown owner"),
        "the refusal must name the unknown owner: {response}"
    );
    // Nothing was minted: a live seeded token must still exchange (a real
    // mint would have superseded it, plan D12).
    let auth = fixture.state.relay.remote_auth_for_tests();
    let seeded = auth.lock().unwrap().create_pairing(1_000, None);
    // A missing owner id is the same refusal, never an unbound mint.
    let missing = request(&mut socket, "r-none", "relay.offer", json!({})).await;
    assert_eq!(missing["ok"], false, "{missing}");
    let refused = request(
        &mut socket,
        "r-ghost",
        "relay.offer",
        json!({ "ownerId": "no-such-owner" }),
    )
    .await;
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        auth.lock()
            .unwrap()
            .exchange(&seeded.token, "probe", 1_001)
            .is_ok(),
        "a refused offer must not mint or supersede a live token"
    );
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
    // T10 amendment: the reply also lists the running agents a restart would
    // stop. A freshly booted daemon has none — an empty list, never a missing
    // field (the settings confirm renders it directly).
    assert_eq!(response["result"]["agents"], json!([]), "{response}");
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

/// T9 amendment (plan D10 step 4): the restart transaction rebuilds one owner
/// per captured window, and the app does **not** know a workspace window's
/// canonical root — only its `workspaceId` (the daemon minted that owner). The
/// daemon is the authority for its own registry data (D4), so `owner.create`
/// accepts a request without `cwd` and resolves the registered root itself.
#[tokio::test(flavor = "multi_thread")]
async fn owner_create_without_cwd_resolves_the_registered_workspace_root() {
    use picot_host::window_owner::OwnerWorkspaceSnapshot;

    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let root = fixture.temp().join("project");
    std::fs::create_dir_all(&root).expect("workspace dir");
    let (row, _added) = fixture
        .state
        .metadata
        .lock()
        .expect("metadata lock")
        .add_workspace(&root)
        .expect("register the workspace");
    let wid = row.workspace_id.clone();
    let canonical = std::path::PathBuf::from(row.canonical_path.clone());

    let (mut socket, _welcome) = fixture.attach().await;
    let response = request(
        &mut socket,
        "r1",
        "owner.create",
        json!({
            "label": format!("native-workspace-{wid}"),
            "workspaceId": wid,
            "port": 0,
            "origin": "http://127.0.0.1:3001",
        }),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["workspaceId"], wid, "{response}");

    let owner = OwnerId::from_string(response["result"]["owner"].as_str().unwrap().to_string());
    match fixture.state.owner_registry.owner_current_workspace(&owner) {
        OwnerWorkspaceSnapshot::Registered { root, .. } => assert_eq!(
            root, canonical,
            "the daemon resolved the registered root, not a caller-supplied path"
        ),
        other => panic!("expected the registered workspace, got {other:?}"),
    }

    fixture.state.shutdown_graceful("test").await;
}

/// The other half of the T9 amendment: with neither `cwd` nor a resolvable
/// `workspaceId` the op is refused (fail closed — never invent a home), and a
/// `workspaceId` the registry does not know is refused rather than recorded.
#[tokio::test(flavor = "multi_thread")]
async fn owner_create_without_cwd_fails_closed() {
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, _welcome) = fixture.attach().await;

    let no_cwd_no_workspace = request(
        &mut socket,
        "r1",
        "owner.create",
        json!({
            "label": "native-landing",
            "port": 0,
            "origin": "http://127.0.0.1:3001",
        }),
    )
    .await;
    assert_eq!(no_cwd_no_workspace["ok"], false, "{no_cwd_no_workspace}");
    assert!(
        no_cwd_no_workspace["error"]
            .as_str()
            .unwrap_or_default()
            .contains("cwd"),
        "the refusal names the missing argument: {no_cwd_no_workspace}"
    );

    let unknown_workspace = request(
        &mut socket,
        "r2",
        "owner.create",
        json!({
            "label": "native-workspace-ghost",
            "workspaceId": "ghost",
            "port": 0,
            "origin": "http://127.0.0.1:3001",
        }),
    )
    .await;
    assert_eq!(unknown_workspace["ok"], false, "{unknown_workspace}");
    assert!(
        unknown_workspace["error"]
            .as_str()
            .unwrap_or_default()
            .contains("ghost"),
        "the refusal names the unknown workspace: {unknown_workspace}"
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
        || steps.steps().len() >= 6,
        "the drain to reach its file and exit steps",
    )
    .await;
    assert_eq!(
        steps.steps(),
        vec![
            DrainStep::StopAcceptingAdminFrames,
            DrainStep::CloseAdminWs,
            DrainStep::StopRelayTransport,
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
    // Every inbound frame type requires the hello. The table is exhaustive on
    // purpose (R1): `mirror_ack` used to be the one frame the connection loop
    // processed *before* `admin_hello`, which let an unauthenticated peer poke
    // a live permit slot by name.
    let frames = [
        r#"{"type":"admin_request","id":"r1","op":"daemon.status"}"#,
        r#"{"type":"surface_response","id":"s1","ok":true,"result":{}}"#,
        r#"{"type":"surface_progress","ackId":"s1","event":{}}"#,
        r#"{"type":"mirror_ack","permitId":"p1","epoch":1}"#,
    ];
    for frame in frames {
        let mut socket = connect(&fixture.admin_ws_url()).await;
        send_raw(&mut socket, frame).await;
        match next_frame(&mut socket).await {
            Frame::Closed(code, reason) => {
                assert_eq!(
                    code,
                    picot_host::daemon::admin::ADMIN_CLOSE_MALFORMED,
                    "{frame} → close reason: {reason}"
                );
            }
            other => panic!("{frame} → expected a 4410 close, got {other:?}"),
        }
    }
    assert!(
        !fixture.admin.manager_lease_held(),
        "no pre-hello frame may acquire the lease"
    );
    fixture.state.shutdown_graceful("test").await;
}

/// B2 regression (the review's PoC, made deterministic). Ten times the review's
/// 300-round race: two clients hello concurrently while the scheduler
/// interleaving seam is engaged, round after round. With the old four-fragment
/// `grant_lease` both racers read "no live incumbent" across the released guard
/// and both were welcomed; with the single critical section at most one welcome
/// is ever issued, the loser reads 4409, and the epoch advances exactly once per
/// round.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_hellos_never_double_grant_the_lease_under_interleaving() {
    const ROUNDS: usize = 200;
    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    // The interleaving point sits *before* the grant's critical section, so
    // engaging it cannot make the refactored grant racy — that is the property
    // under test. Against the old fragment-per-guard grant the same point was
    // between the liveness check and the install (see the review's B2 PoC).
    fixture.admin.set_lease_gap_yield_for_tests(true);
    let hello = json!({
        "type": "admin_hello",
        "adminToken": fixture.token,
        "role": "manager",
        "appVersion": "0.5.0",
        "buildId": "test-build",
        "pid": 11,
    });

    for round in 0..ROUNDS {
        let mut first = connect(&fixture.admin_ws_url()).await;
        let mut second = connect(&fixture.admin_ws_url()).await;
        send_json(&mut first, hello.clone()).await;
        send_json(&mut second, hello.clone()).await;

        let mut granted = 0;
        let mut rejected = 0;
        for (index, socket) in [&mut first, &mut second].into_iter().enumerate() {
            match next_frame(socket).await {
                Frame::Json(value) => {
                    assert_eq!(
                        value["type"], "admin_welcome",
                        "round {round} client {index}: {value}"
                    );
                    granted += 1;
                }
                Frame::Closed(code, reason) => {
                    assert_eq!(
                        code, ADMIN_CLOSE_LEASE_HELD,
                        "round {round} client {index}: {reason}"
                    );
                    rejected += 1;
                }
            }
        }
        assert_eq!(
            granted, 1,
            "round {round}: exactly one welcome, got {granted}"
        );
        assert_eq!(rejected, 1, "round {round}: the loser closed 4409");
        assert_eq!(
            fixture.admin.epoch(),
            (round + 1) as u64,
            "round {round}: a lost race adds no epoch"
        );

        // Drop both sockets and wait until the daemon has unregistered them:
        // the next round must start from a lease whose connection is gone (the
        // supersede path), which is where the old grant was non-atomic.
        drop(first);
        drop(second);
        wait_until(
            Duration::from_secs(5),
            || fixture.admin.connection_count() == 0,
            "both round sockets to be unregistered",
        )
        .await;
    }
    assert_eq!(fixture.admin.epoch(), ROUNDS as u64);
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
        // R5: the close-request bridge is lease-gated too — a leaseless caller
        // must not be able to make the app interrupt its own user.
        ("r4", "owner.close_request"),
        // B1: stopping a single runtime is a lifecycle op like any other.
        ("r5", "agent.stop"),
        // T10: device revocation is a mutating relay op — lease-gated too.
        ("r6", "relay.revoke_device"),
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

// ---------------------------------------------------------------------------
// B1 — `agent.stop`: one exact runtime, by its full identity
// ---------------------------------------------------------------------------

/// The triple is matched **exactly**: a request that names no live runtime is
/// refused by name, and the refusal touches nothing. `daemon.status` is the
/// second witness — the row the settings panel renders still lists the runtime
/// that the refused stop named.
#[tokio::test(flavor = "multi_thread")]
async fn agent_stop_refuses_an_unknown_triple_and_touches_no_runtime() {
    use picot_host::runtime_coordinator::RuntimeTarget;

    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, _) = fixture.attach().await;
    let running = RuntimeTarget::with_owner("w1", "s1", "i1", "o1", 1);
    // Held, not dropped: the in-memory process's channels are what keep the
    // runtime addressable (a dropped seam would make the pump see EOF).
    let _process = fixture
        .state
        .runtimes
        .register_in_memory(running.clone())
        .expect("an in-memory runtime stands in for a spawned Pi");
    // One field wrong in each direction: no wildcard, no partial match.
    for (id, wrong) in [
        (
            "r1",
            json!({"workspaceId": "w1", "sessionId": "s1", "instanceId": "i-other"}),
        ),
        (
            "r2",
            json!({"workspaceId": "w1", "sessionId": "s-other", "instanceId": "i1"}),
        ),
        (
            "r3",
            json!({"workspaceId": "w-other", "sessionId": "s1", "instanceId": "i1"}),
        ),
    ] {
        let response = request(&mut socket, id, "agent.stop", wrong).await;
        assert_eq!(response["ok"], false, "{response}");
        assert!(
            response["error"]
                .as_str()
                .unwrap_or_default()
                .contains("unknown agent"),
            "the refusal names the triple it could not find: {response}"
        );
    }
    // A workspace-only stop is what this op deliberately does not have: the
    // missing field is an argument error, never a workspace-level batch.
    let batch = request(
        &mut socket,
        "r4",
        "agent.stop",
        json!({"workspaceId": "w1"}),
    )
    .await;
    assert_eq!(batch["ok"], false, "{batch}");
    assert!(
        batch["error"]
            .as_str()
            .unwrap_or_default()
            .contains("requires a string sessionId"),
        "{batch}"
    );

    let live = fixture.state.runtimes.running_targets();
    assert_eq!(live.len(), 1, "a refused stop stops nothing: {live:?}");
    assert_eq!(live[0].instance_id, "i1");

    let status = request(&mut socket, "r5", "daemon.status", json!({})).await;
    let agents = status["result"]["agents"]
        .as_array()
        .expect("an agents list");
    assert_eq!(agents.len(), 1, "{status}");
    assert_eq!(agents[0]["instanceId"], "i1");
    assert_eq!(agents[0]["sessionId"], "s1");
    fixture.state.shutdown_graceful("test").await;
}

/// The exact match stops **one** runtime: a sibling in the same workspace is
/// what the triple exists for, and it survives — in the manager's own registry
/// and in the `daemon.status` reply alike.
#[tokio::test(flavor = "multi_thread")]
async fn agent_stop_stops_exactly_the_matching_runtime() {
    use picot_host::runtime_coordinator::RuntimeTarget;

    let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
    let (mut socket, _) = fixture.attach().await;
    let victim = RuntimeTarget::with_owner("w1", "s1", "i1", "o1", 1);
    let sibling = RuntimeTarget::with_owner("w1", "s2", "i2", "o1", 1);
    let _victim_process = fixture
        .state
        .runtimes
        .register_in_memory(victim.clone())
        .expect("an in-memory runtime stands in for a spawned Pi");
    let _sibling_process = fixture
        .state
        .runtimes
        .register_in_memory(sibling.clone())
        .expect("a second in-memory runtime in the same workspace");
    let response = request(
        &mut socket,
        "r1",
        "agent.stop",
        json!({
            "workspaceId": victim.workspace_id,
            "sessionId": victim.session_id,
            "instanceId": victim.instance_id,
        }),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["result"]["stopped"],
        json!({"workspaceId": "w1", "sessionId": "s1", "instanceId": "i1"}),
        "the confirmation names the exact triple: {response}"
    );
    let live = fixture.state.runtimes.running_targets();
    assert_eq!(live.len(), 1, "one runtime stopped, one survived: {live:?}");
    assert_eq!(live[0].instance_id, "i2");
    assert_eq!(
        fixture.state.runtimes.state_of(&victim),
        None,
        "the coordinator no longer tracks the stopped runtime"
    );
    let status = request(&mut socket, "r2", "daemon.status", json!({})).await;
    let agents = status["result"]["agents"]
        .as_array()
        .expect("an agents list");
    assert_eq!(agents.len(), 1, "{status}");
    assert_eq!(agents[0]["instanceId"], "i2");
    fixture.state.shutdown_graceful("test").await;
}

#[tokio::test]
async fn relay_device_list_exposes_only_safe_fields_and_omits_revoked() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let fixture = Fixture::boot(MIRROR_ACK_TIMEOUT).await;
        let (mut socket, _) = fixture.attach().await;
        {
            let mut metadata = fixture.state.metadata.lock().unwrap();
            metadata
                .store_device_token("device-live", "never-return-this-token", Some("owner-safe"))
                .unwrap();
            metadata
                .store_device_token("device-revoked", "revoked-secret", None)
                .unwrap();
            metadata.revoke_device("device-revoked").unwrap();
        }
        let response = request(&mut socket, "device-list", "relay.device_list", json!({})).await;
        assert_eq!(response["ok"], true, "{response}");
        let devices = response["result"]["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        let row = devices[0].as_object().unwrap();
        assert_eq!(
            row.keys().map(String::as_str).collect::<Vec<_>>(),
            ["deviceId", "owner", "pairedAt"]
        );
        assert_eq!(row["deviceId"], "device-live");
        assert_eq!(row["owner"], "owner-safe");
        assert!(row["pairedAt"].as_i64().unwrap() > 0);
        assert!(!response.to_string().contains("token"));
        fixture.state.shutdown_graceful("device list test").await;
    })
    .await
    .expect("device list deadline");
}
