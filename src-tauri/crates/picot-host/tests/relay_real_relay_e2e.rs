// ABOUTME: Real-relay e2e for the two remaining phase-2.5 acceptance items, against an isolated daemon.
// ABOUTME: Env-gated by PICOT_REAL_RELAY=1 + PICOT_REAL_RELAY_ISO; no GUI, every step has a deadline.
//
//! Zero mocks on the wire: the relay is the company relay
//! (`paseo.021521.xyz:443`, TLS), the host side is a **real external
//! `picot-daemon` process** started on an isolation root by
//! `scripts/smoke-relay-phase25.sh`, and this test only owns the pieces a GUI
//! would own: the admin channel (loopback WS) and the per-host bridge proxy
//! (loopback WS + pairing sink).
//!
//! Frame shapes follow `tests/admin_ws.rs` / `tests/relay_host_bridge.rs`. Two
//! spellings differ from the task prose and follow the code:
//! `owner.detach` takes `{"owner": …}` (not `ownerId`), and a
//! `host_request{operation:"workspace.list"}` answers with
//! `host_response` (`data_response` belongs to `data_request` frames).
//!
//! The two items, each with its own deadline and a message carrying the actual
//! observation:
//!
//! 1. bridge session round trip (pairing → hello_ack without `deviceToken` →
//!    `workspace.list` navigation admitted for the device);
//! 2. D1: after `owner.detach` the same connection still operates.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use picot_host::relay::client_role::{ClientRoleConfig, ClientRoleSeams};
use picot_host::relay::host_bridge::{
    start_host_bridge_with_seams, BridgeSeams, Credential, HostBridgeHandle, HostBridgeStatus,
    PairingSaveError, PairingSink,
};
use picot_host::relay::host_store::DeviceToken;
use picot_host::relay::pairing::{parse_connection_offer, ConnectionOfferV2};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The WebView's loopback origin, as its page would present it.
/// The app's own page origin: a bridge only serves loopback documents.
const ORIGIN: &str = "http://127.0.0.1:54321";
const DEVICE_1: &str = "e2e-device-1";
/// Whole-test backstop: one pairing over the public relay.
const BUDGET: Duration = Duration::from_secs(120);

fn iso_root() -> Option<PathBuf> {
    let enabled = std::env::var("PICOT_REAL_RELAY").ok();
    if enabled.as_deref() != Some("1") {
        eprintln!(
            "SKIP real-relay e2e: PICOT_REAL_RELAY={enabled:?} (set 1 and PICOT_REAL_RELAY_ISO)"
        );
        return None;
    }
    match std::env::var("PICOT_REAL_RELAY_ISO") {
        Ok(value) if !value.trim().is_empty() => Some(PathBuf::from(value.trim())),
        other => {
            eprintln!("SKIP real-relay e2e: PICOT_REAL_RELAY_ISO={other:?}");
            None
        }
    }
}

/// The instance file the daemon publishes (data dir, plan D3/D7).
struct Instance {
    pid: u32,
    admin_origin: String,
    admin_token: String,
}

fn read_instance(iso: &Path) -> Option<Instance> {
    let raw = std::fs::read_to_string(iso.join("data").join("picot-daemon.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    Some(Instance {
        pid: value["pid"].as_u64()? as u32,
        admin_origin: value["adminOrigin"].as_str()?.to_owned(),
        admin_token: value["adminToken"].as_str()?.to_owned(),
    })
}

async fn wait_for_instance(iso: &Path, deadline: Duration) -> Instance {
    let started = Instant::now();
    loop {
        if let Some(instance) = read_instance(iso) {
            return instance;
        }
        assert!(
            started.elapsed() < deadline,
            "no instance file at {} after {deadline:?} (is picot-daemon running?)",
            iso.join("data").join("picot-daemon.json").display()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// One frame with a deadline; a close before the frame is a failure with the
/// observed close reason.
async fn next_json(socket: &mut Socket, deadline: Duration) -> Value {
    loop {
        let message = tokio::time::timeout(deadline, socket.next())
            .await
            .expect("a frame within the deadline")
            .expect("the socket is open");
        match message {
            Ok(Message::Text(text)) => return serde_json::from_str(&text).expect("a JSON frame"),
            Ok(Message::Close(frame)) => panic!("socket closed before a frame: {frame:?}"),
            Ok(_) => continue,
            Err(error) => panic!("websocket error: {error}"),
        }
    }
}

/// The admin channel as the app holds it: one manager lease, id-matched replies,
/// mirror events skipped.
struct Admin {
    socket: Socket,
    counter: u64,
}

impl Admin {
    async fn attach(instance: &Instance) -> Admin {
        let url = format!(
            "{}/admin",
            instance
                .admin_origin
                .replacen("http://", "ws://", 1)
                .trim_end_matches('/')
        );
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio_tungstenite::connect_async(url.as_str()).await
        })
        .await
        .expect("the admin listener accepts within 10s")
        .expect("a real admin client connects");
        let hello = json!({
            "type": "admin_hello",
            "adminToken": instance.admin_token,
            "role": "manager",
            "appVersion": "0.5.0",
            "buildId": "e2e",
            "pid": std::process::id(),
        });
        socket.send(Message::Text(hello.to_string())).await.unwrap();
        let welcome = next_json(&mut socket, Duration::from_secs(10)).await;
        assert_eq!(welcome["type"], "admin_welcome", "{welcome}");
        Admin { socket, counter: 0 }
    }

    async fn result(&mut self, op: &str, args: Value) -> Value {
        self.counter += 1;
        let id = format!("e2e-{}", self.counter);
        let frame = json!({"type": "admin_request", "id": id, "op": op, "args": args});
        self.socket
            .send(Message::Text(frame.to_string()))
            .await
            .unwrap();
        loop {
            let reply = next_json(&mut self.socket, Duration::from_secs(30)).await;
            if reply["type"] == "admin_result" && reply["id"] == json!(id) {
                return reply;
            }
        }
    }

    async fn op(&mut self, op: &str, args: Value) -> Value {
        let reply = self.result(op, args).await;
        assert_eq!(reply["ok"], true, "{op} failed: {reply}");
        reply["result"].clone()
    }
}

async fn wait_relay_online(admin: &mut Admin, deadline: Duration) -> Value {
    let started = Instant::now();
    loop {
        let status = admin.op("relay.status", json!({})).await;
        if status["state"] == "online" {
            return status;
        }
        assert!(
            started.elapsed() < deadline,
            "relay.state = {} after {deadline:?}: {status}",
            status["state"]
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The app's pairing sink: the one device token a scan mints.
struct Saved {
    tokens: Mutex<Vec<DeviceToken>>,
}
impl Saved {
    fn new() -> Arc<Saved> {
        Arc::new(Saved {
            tokens: Mutex::new(Vec::new()),
        })
    }
    fn count(&self) -> usize {
        self.tokens.lock().unwrap().len()
    }
    fn value(&self, index: usize) -> String {
        self.tokens.lock().unwrap()[index].expose().to_owned()
    }
}
#[async_trait]
impl PairingSink for Saved {
    async fn persist(&self, token: DeviceToken) -> Result<(), PairingSaveError> {
        self.tokens.lock().unwrap().push(token);
        Ok(())
    }
}

/// Every status the bridge ever published, so a flap is caught, not just the
/// final value.
#[derive(Clone)]
struct StatusLog(Arc<Mutex<Vec<HostBridgeStatus>>>);
impl StatusLog {
    fn record(handle: &HostBridgeHandle) -> StatusLog {
        let log = StatusLog(Arc::new(Mutex::new(vec![*handle.status.borrow()])));
        let mut status = handle.status.clone();
        let sink = log.clone();
        tokio::spawn(async move {
            while status.changed().await.is_ok() {
                let value = *status.borrow_and_update();
                println!("bridge status -> {value:?}");
                sink.0.lock().unwrap().push(value);
            }
        });
        log
    }
    fn states(&self) -> Vec<HostBridgeStatus> {
        self.0.lock().unwrap().clone()
    }
    fn describe(&self) -> String {
        format!("{:?}", self.states())
    }
}

async fn wait_status(
    handle: &HostBridgeHandle,
    log: &StatusLog,
    want: HostBridgeStatus,
    deadline: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let current = *handle.status.borrow();
        if current == want {
            return Ok(());
        }
        if started.elapsed() >= deadline {
            return Err(format!(
                "bridge status stayed {current:?}, wanted {want:?} within {deadline:?}; \
                 observed {}",
                log.describe()
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn start_bridge(
    offer: &ConnectionOfferV2,
    device_id: &str,
    saved: Arc<Saved>,
) -> HostBridgeHandle {
    let pairing_token = offer
        .pairing_token
        .clone()
        .expect("the offer carries a one-scan pairingToken");
    start_host_bridge_with_seams(
        ClientRoleConfig {
            endpoint: offer.relay.endpoint.clone(),
            use_tls: offer.relay.use_tls,
            server_id: offer.server_id.clone(),
            peer_public_key_b64: offer.daemon_public_key_b64.clone(),
        },
        Credential::Pairing {
            pairing_token: DeviceToken::new(pairing_token),
            device_id: device_id.to_owned(),
        },
        Some(saved),
        BridgeSeams {
            first_frame_timeout: Duration::from_secs(20),
            client: ClientRoleSeams::default(),
        },
    )
    .await
    .expect("the loopback bridge starts")
}

/// The WebView's socket: loopback + the Origin the app's page carries.
async fn dial(bridge: &HostBridgeHandle) -> Socket {
    let mut request = (bridge.origin.replacen("http://", "ws://", 1) + "/v2/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Origin", ORIGIN.parse().unwrap());
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio_tungstenite::connect_async(request).await
    })
    .await
    .expect("the bridge accepts within 10s")
    .expect("the bridge loopback upgrades")
    .0
}

async fn send(socket: &mut Socket, text: &str) {
    socket.send(Message::Text(text.into())).await.unwrap();
}

/// The first business frame, with the deadline and a message carrying what did
/// arrive instead.
async fn recv_json(socket: &mut Socket, deadline: Duration, what: &str) -> Value {
    loop {
        let message = tokio::time::timeout(deadline, socket.next())
            .await
            .unwrap_or_else(|_| panic!("no frame for {what} within {deadline:?}"))
            .unwrap_or_else(|| panic!("the socket closed while waiting for {what}"));
        match message {
            Ok(Message::Text(text)) => return serde_json::from_str(&text).expect("a JSON frame"),
            Ok(Message::Close(frame)) => {
                panic!("socket closed while waiting for {what}: {frame:?}")
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            Ok(other) => panic!("unexpected frame while waiting for {what}: {other:?}"),
            Err(error) => panic!("websocket error while waiting for {what}: {error}"),
        }
    }
}

/// Same read with the failure returned instead of panicked, so the caller can
/// add the bridge's status timeline (the proxy closes without a handshake on a
/// terminal pairing, and that is the evidence worth printing).
async fn try_recv_json(socket: &mut Socket, deadline: Duration) -> Result<Value, String> {
    loop {
        match tokio::time::timeout(deadline, socket.next()).await {
            Err(_) => return Err(format!("no frame within {deadline:?}")),
            Ok(None) => return Err("the socket closed".into()),
            Ok(Some(Ok(Message::Text(text)))) => {
                return serde_json::from_str(&text).map_err(|error| format!("bad JSON: {error}"))
            }
            Ok(Some(Ok(Message::Close(frame)))) => return Err(format!("closed: {frame:?}")),
            Ok(Some(Ok(Message::Ping(_)))) | Ok(Some(Ok(Message::Pong(_)))) => continue,
            Ok(Some(Ok(other))) => return Err(format!("unexpected frame: {other:?}")),
            Ok(Some(Err(error))) => return Err(format!("websocket error: {error}")),
        }
    }
}

fn hello() -> String {
    json!({
        "type": "hello",
        "protocolVersion": 2,
        "clientType": "desktop",
        "clientId": "e2e-1",
    })
    .to_string()
}

/// The D2 navigation request a paired device sends after `hello_ack`.
fn workspace_list(request_id: &str) -> String {
    json!({
        "type": "host_request",
        "requestId": request_id,
        "operation": "workspace.list",
        "args": {},
    })
    .to_string()
}

/// One pairing: admin offer → bridge → proxy hello → `hello_ack` (token-free).
async fn paired(
    admin: &mut Admin,
    owner: &str,
    device_id: &str,
) -> (HostBridgeHandle, StatusLog, Arc<Saved>, Socket, Value) {
    let offer = admin.op("relay.offer", json!({ "ownerId": owner })).await;
    let url = offer["url"].as_str().expect("offer url").to_owned();
    let parsed = parse_connection_offer(&url).expect("the daemon's offer parses");
    assert!(parsed.relay.use_tls, "the company relay is TLS-only");
    let saved = Saved::new();
    let bridge = start_bridge(&parsed, device_id, saved.clone()).await;
    let log = StatusLog::record(&bridge);
    let mut socket = dial(&bridge).await;
    send(&mut socket, &hello()).await;
    let ack = match try_recv_json(&mut socket, Duration::from_secs(30)).await {
        Ok(ack) => ack,
        Err(error) => panic!(
            "no hello_ack for {device_id}: {error}; bridge states={}; tokens saved={}",
            log.describe(),
            saved.count()
        ),
    };
    (bridge, log, saved, socket, ack)
}

async fn create_owner(admin: &mut Admin, label: &str, iso: &Path) -> String {
    let workspace = iso.join("workspace");
    let created = admin
        .op(
            "owner.create",
            json!({
                "label": label,
                "origin": "http://127.0.0.1:3999",
                "port": 3999,
                "cwd": workspace.display().to_string(),
            }),
        )
        .await;
    created["owner"].as_str().expect("owner id").to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn two_acceptance_items_over_the_real_relay() {
    let Some(iso) = iso_root() else { return };
    tokio::time::timeout(BUDGET, run(&iso))
        .await
        .expect("the whole suite stays inside its 120s budget");
}

async fn run(iso: &Path) {
    let instance = wait_for_instance(iso, Duration::from_secs(30)).await;
    println!(
        "instance: pid={} admin={}",
        instance.pid, instance.admin_origin
    );
    let mut admin = Admin::attach(&instance).await;
    let owner = create_owner(&mut admin, "e2e-window-1", iso).await;
    let enabled = admin
        .op("relay.set_enabled", json!({ "enabled": true }))
        .await;
    assert_eq!(enabled["enabled"], true, "{enabled}");
    let online = wait_relay_online(&mut admin, Duration::from_secs(20)).await;
    println!("relay online: {online}");

    // ---- item 1: bridge session round trip over the real relay ------------
    let (bridge1, log1, saved1, mut socket1, ack) = paired(&mut admin, &owner, DEVICE_1).await;
    assert_eq!(
        ack["type"], "hello_ack",
        "item 1: the pairing exchange must end in hello_ack: {ack}"
    );
    assert!(
        ack.get("deviceToken").is_none(),
        "item 1: the minted deviceToken leaked to the WebView: {ack}"
    );
    assert_eq!(saved1.count(), 1, "item 1: exactly one token persisted");
    let token = saved1.value(0);
    assert!(
        !ack.to_string().contains(&token),
        "item 1: the persisted token value leaked to the WebView: {ack}"
    );
    wait_status(
        &bridge1,
        &log1,
        HostBridgeStatus::Online,
        Duration::from_secs(10),
    )
    .await
    .expect("item 1: the bridge is Online after the exchange");
    send(&mut socket1, &workspace_list("nav-1")).await;
    let navigation = recv_json(&mut socket1, Duration::from_secs(30), "workspace.list").await;
    assert_eq!(
        navigation["type"], "host_response",
        "item 1: a device navigation request must be answered, not refused: {navigation}"
    );
    assert!(
        navigation.get("error").is_none(),
        "item 1: workspace.list refused: {navigation}"
    );
    assert!(
        navigation["response"].is_object(),
        "item 1: workspace.list response is not an object: {navigation}"
    );
    println!(
        "item 1 PASS: hello_ack token-free, 1 token saved, workspace.list -> {}",
        navigation["response"]
    );

    // ---- item 2 (D1): the device keeps working after the host window dies --
    let detached = admin.op("owner.detach", json!({ "owner": owner })).await;
    assert_eq!(detached["detached"], json!(owner), "{detached}");
    send(&mut socket1, &workspace_list("nav-2")).await;
    let after_detach = recv_json(
        &mut socket1,
        Duration::from_secs(30),
        "workspace.list after detach",
    )
    .await;
    assert_eq!(
        after_detach["type"], "host_response",
        "item 2: detach (host window gone) must not end device navigation: {after_detach}"
    );
    assert!(
        after_detach.get("error").is_none(),
        "item 2: workspace.list after detach refused: {after_detach}"
    );
    println!(
        "item 2 PASS: workspace.list after owner.detach -> {}",
        after_detach["response"]
    );

    // ---- teardown: the daemon drains through `daemon.stop` -----------------
    let stop = admin.op("daemon.stop", json!({ "reason": "e2e" })).await;
    assert_eq!(stop["accepted"], true, "{stop}");
    let started = Instant::now();
    loop {
        let gone = read_instance(iso).is_none();
        let exited = unsafe { libc::kill(instance.pid as i32, 0) } != 0;
        if gone && exited {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "daemon.stop: instance file gone={gone}, pid {} exited={exited} after 20s",
            instance.pid
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!(
        "teardown PASS: instance file removed, pid {} exited",
        instance.pid
    );
    bridge1.stop().await;
}
