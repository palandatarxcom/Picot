// ABOUTME: Per-host bridge contracts: byte-preserving hello, pairing and lifecycle.
// ABOUTME: Real loopback WS against mock relay and in-process daemon; every test has deadline.

#[path = "support/mock_relay.rs"]
mod mock_relay;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use mock_relay::{MockClientSocket, MockRelay};
use picot_host::relay::client_role::{ClientRoleConfig, ClientRoleSeams};
use picot_host::relay::e2ee;
use picot_host::relay::host_bridge::{
    start_host_bridge_with_seams, BindingRecord, BridgeCounters, BridgeSeams, Credential,
    HostBridgeHandle, HostBridgeStatus, PairingSaveError, PairingSink,
};
use picot_host::relay::host_store::DeviceToken;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

struct LogCapture(Arc<Mutex<Vec<String>>>);
impl log::Log for LogCapture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        self.0.lock().unwrap().push(format!(
            "[{}] {} {}",
            record.target(),
            record.level(),
            record.args()
        ));
    }
    fn flush(&self) {}
}
fn captured_logs() -> Arc<Mutex<Vec<String>>> {
    static LOGS: std::sync::OnceLock<Arc<Mutex<Vec<String>>>> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs: Arc<Mutex<Vec<String>>> = Arc::default();
        let _ = log::set_boxed_logger(Box::new(LogCapture(logs.clone())));
        // Trace on purpose: the assertion below also covers DEBUG/TRACE sinks,
        // not just the shipped Info level.
        log::set_max_level(log::LevelFilter::Trace);
        logs
    })
    .clone()
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
const ORIGIN: &str = "http://127.0.0.1:54321";
const BUDGET: Duration = Duration::from_secs(15);

struct Saved {
    tokens: Mutex<Vec<DeviceToken>>,
    fail: bool,
}
#[async_trait]
impl PairingSink for Saved {
    async fn persist(&self, token: DeviceToken) -> Result<(), PairingSaveError> {
        self.tokens.lock().unwrap().push(token);
        if self.fail {
            Err(PairingSaveError)
        } else {
            Ok(())
        }
    }
}
struct Rig {
    relay: MockRelay,
    secret: crypto_box::SecretKey,
    bridge: HostBridgeHandle,
    /// The first window's authorization record, bound to this bridge instance.
    binding: Arc<BindingRecord>,
}
impl Rig {
    /// A fresh secret for the rig's window, as `host_grant` would mint it.
    fn grant(&self) -> String {
        self.bridge.mint_grant(&self.binding).expect("mint")
    }
    fn counters(&self) -> Arc<BridgeCounters> {
        self.bridge.counters_for_tests()
    }
    fn new_binding(&self) -> Arc<BindingRecord> {
        let binding = BindingRecord::new("host", ORIGIN).expect("origin");
        assert!(self.bridge.bind(&binding));
        binding
    }
    async fn authorized(&self, window: &str) -> Socket {
        let grant = self.grant();
        self.authorized_with(&grant, window).await
    }
    async fn authorized_with(&self, grant: &str, window: &str) -> Socket {
        let mut socket = dial(&self.bridge, Some(ORIGIN)).await;
        send(&mut socket, &hello_with_grant(window, grant)).await;
        socket
    }
}
async fn rig(credential: Credential, sink: Option<Arc<dyn PairingSink>>) -> Rig {
    let relay = MockRelay::start().await;
    let secret = crypto_box::SecretKey::generate(&mut rand::rngs::OsRng);
    let mut client = ClientRoleSeams::default();
    client.timings.pre_hello_delay = Duration::from_millis(20);
    client.timings.hello_resend_after = Duration::from_millis(50);
    client.timings.backoff_base = Duration::from_millis(80);
    let bridge = start_host_bridge_with_seams(
        ClientRoleConfig {
            endpoint: relay.addr().to_string(),
            use_tls: false,
            server_id: "host".into(),
            peer_public_key_b64: B64.encode(secret.public_key().as_bytes()),
        },
        credential,
        sink,
        BridgeSeams {
            first_frame_timeout: Duration::from_millis(150),
            client,
        },
    )
    .await
    .unwrap();
    let binding = BindingRecord::new("host", ORIGIN).expect("origin");
    assert!(bridge.bind(&binding));
    Rig {
        relay,
        secret,
        bridge,
        binding,
    }
}
fn device() -> Credential {
    Credential::Device(DeviceToken::new("rust-secret"))
}
/// A raw hello WITHOUT a grant: every use of this helper is a negative case.
fn hello(client_id: &str) -> String {
    json!({"type":"hello", "protocolVersion":2, "clientId":client_id, "clientType":"desktop", "desktopCapability":"local-secret"}).to_string()
}
fn hello_with_grant(client_id: &str, grant: &str) -> String {
    json!({"type":"hello", "protocolVersion":2, "clientId":client_id, "clientType":"desktop", "desktopCapability":"local-secret", "bridgeGrant":grant}).to_string()
}
/// The authorized hello a window behind the app really sends.
fn granted(rig: &Rig, client_id: &str) -> String {
    hello_with_grant(client_id, &rig.grant())
}
/// Binds a fresh record on an ad-hoc bridge and mints its grant.
fn authorize(bridge: &HostBridgeHandle) -> String {
    let binding = BindingRecord::new("host", ORIGIN).expect("origin");
    assert!(bridge.bind(&binding));
    bridge.mint_grant(&binding).expect("mint")
}
/// Dials with an arbitrary Origin header list: duplicates and malformed values
/// are shapes a native client can produce and a browser cannot.
async fn dial_with_origins(bridge: &HostBridgeHandle, origins: &[&str]) -> Socket {
    let mut request = (bridge.origin.replacen("http://", "ws://", 1) + "/v2/ws")
        .into_client_request()
        .unwrap();
    for origin in origins {
        request
            .headers_mut()
            .append("Origin", origin.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request).await.unwrap().0
}
async fn dial(bridge: &HostBridgeHandle, origin: Option<&str>) -> Socket {
    let mut req = (bridge.origin.replacen("http://", "ws://", 1) + "/v2/ws")
        .into_client_request()
        .unwrap();
    if let Some(origin) = origin {
        req.headers_mut().insert("Origin", origin.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.unwrap().0
}
async fn send(socket: &mut Socket, text: &str) {
    socket.send(Message::Text(text.into())).await.unwrap();
}
async fn recv(socket: &mut Socket) -> String {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(s))) => return s,
            Some(Ok(Message::Ping(_))) => {}
            x => panic!("expected text, got {x:?}"),
        }
    }
}
async fn closed(socket: &mut Socket) {
    loop {
        match socket.next().await {
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
            Some(Ok(Message::Ping(_))) => {}
            x => panic!("unexpected forwarded frame: {x:?}"),
        }
    }
}
struct Peer {
    socket: MockClientSocket,
    shared: [u8; 32],
}
impl Peer {
    async fn recv(&mut self) -> String {
        let wire = self.socket.recv_text().await;
        String::from_utf8(e2ee::open(&self.shared, &B64.decode(wire).unwrap()).unwrap()).unwrap()
    }
    async fn send(&mut self, s: &str) {
        self.socket
            .send_text(&B64.encode(e2ee::seal(&self.shared, s.as_bytes())))
            .await;
    }
}
async fn peer(rig: &Rig, seen: &mut Vec<String>) -> Peer {
    let id = loop {
        if let Some(id) = rig
            .relay
            .client_ids()
            .into_iter()
            .find(|id| !seen.contains(id))
        {
            break id;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    seen.push(id.clone());
    let mut socket = rig.relay.connect_data(&id).await;
    let hello: Value = serde_json::from_str(&socket.recv_text().await).unwrap();
    let shared =
        e2ee::derive_shared_key(&rig.secret.to_bytes(), hello["key"].as_str().unwrap()).unwrap();
    socket.send_text("{\"type\":\"e2ee_ready\"}").await;
    Peer { socket, shared }
}

/// File-internal serial guard: every test here spins a bridge + E2EE peer rig;
/// running them in parallel makes the mock relay drop handshakes under load
/// (same fixture class as relay_e2e). Serial execution only removes
/// test-vs-test contention; every assertion and deadline stays unchanged.
static BRIDGE_SERIAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn serial_guard() -> tokio::sync::MutexGuard<'static, ()> {
    BRIDGE_SERIAL_LOCK.lock().await
}

#[tokio::test]
async fn hello_preserves_raw_values_and_business_frames_and_two_sessions_are_independent() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let mut seen = Vec::new();
        let grant = r.grant();
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        // Forged credential fields and a real grant: the grant is read, then
        // stripped with every caller-supplied credential before the rewrite.
        let original = format!(r#"{{"type":"hello","bridgeGrant":"{grant}","deviceToken":"forged-device","pairingToken":"forged-pairing","deviceId":"forged-id","clientType":"desktop","desktopCapability":"local-secret","clientId":"w1","unknown":{{ "x":1e+2, "s":"\u0041" }},"canonicalRoute":"a\/b"}}"#);
        send(&mut a, &original).await;
        let mut p = peer(&r, &mut seen).await;
        let rewritten = p.recv().await;
        let raw = |s: &str| serde_json::from_str::<std::collections::BTreeMap<String, Box<serde_json::value::RawValue>>>(s).unwrap();
        let before = raw(&original); let after = raw(&rewritten);
        for (key, value) in &before { if !["clientType", "desktopCapability", "bridgeGrant", "deviceToken", "pairingToken", "deviceId"].contains(&key.as_str()) { assert_eq!(after[key].get(), value.get(), "raw {key}"); } }
        let v: Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["clientType"], "remote"); assert_eq!(v["deviceToken"], "rust-secret");
        assert!(v.get("desktopCapability").is_none()); assert!(v.get("bridgeGrant").is_none());
        p.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut a).await, "{\"type\":\"hello_ack\"}");
        let frame = "{ \"type\": \"business\", \"n\": 1e+2 }";
        send(&mut a, frame).await; assert_eq!(p.recv().await, frame);
        p.send(frame).await; assert_eq!(recv(&mut a).await, frame);
        let mut b = dial(&r.bridge, Some(ORIGIN)).await; send(&mut b, &granted(&r, "w2")).await;
        let mut q = peer(&r, &mut seen).await; assert_eq!(serde_json::from_str::<Value>(&q.recv().await).unwrap()["clientId"], "w2");
        assert_eq!(seen.len(), 2);
        r.bridge.stop().await; closed(&mut a).await; closed(&mut b).await;
    }).await.expect("rewrite test deadline");
}

#[tokio::test]
async fn pairing_single_flight_sanitizes_ack_and_failed_save_is_terminal() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        for fail in [false, true] {
            let saved = Arc::new(Saved { tokens: Mutex::new(vec![]), fail });
            let r = rig(Credential::Pairing { pairing_token: DeviceToken::new("offer-secret"), device_id: "stable-id".into() }, Some(saved.clone())).await;
            let mut seen = vec![];
            let mut a = dial(&r.bridge, Some(ORIGIN)).await; send(&mut a, &granted(&r, "w1")).await;
            let mut p = peer(&r, &mut seen).await;
            let v: Value = serde_json::from_str(&p.recv().await).unwrap();
            assert_eq!(v["pairingToken"], "offer-secret"); assert_eq!(v["deviceId"], "stable-id"); assert!(v.get("deviceToken").is_none());
            // Chosen policy: reject second proxy during exchange, never consume offer twice.
            let mut b = dial(&r.bridge, Some(ORIGIN)).await; send(&mut b, &granted(&r, "w2")).await; closed(&mut b).await;
            assert_eq!(r.relay.client_upgrades(), 1);
            p.send(r#"{"type":"hello_ack","deviceToken":"minted-secret","capabilities":{"native":true}}"#).await;
            if fail {
                closed(&mut a).await;
                assert_eq!(*r.bridge.status.borrow(), HostBridgeStatus::PairingFailed);
            } else {
                let ack = recv(&mut a).await; assert!(!ack.contains("deviceToken")); assert!(!ack.contains("minted-secret"));
                assert_eq!(serde_json::from_str::<Value>(&ack).unwrap()["capabilities"]["native"], true);
                let mut b = dial(&r.bridge, Some(ORIGIN)).await; send(&mut b, &granted(&r, "w2")).await;
                let mut q = peer(&r, &mut seen).await;
                let v: Value = serde_json::from_str(&q.recv().await).unwrap(); assert_eq!(v["deviceToken"], "minted-secret"); assert!(v.get("pairingToken").is_none());
                // Defense in depth: even an unexpected token on an ordinary ack is stripped.
                q.send("{\"type\":\"hello_ack\",\"deviceToken\":\"unexpected-secret\"}").await;
                let ack = recv(&mut b).await; assert!(!ack.contains("deviceToken")); assert!(!ack.contains("unexpected-secret"));
                assert_eq!(saved.tokens.lock().unwrap().len(), 1);
            }
            assert_eq!(saved.tokens.lock().unwrap()[0].expose(), "minted-secret");
            r.bridge.stop().await;
        }
    }).await.expect("pairing test deadline");
}

#[tokio::test]
async fn relay_loss_keeps_proxy_redials_without_duplicate_hello_and_reconnects() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let mut seen = vec![];
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut a, &granted(&r, "w1")).await;
        let mut p = peer(&r, &mut seen).await;
        p.recv().await;
        p.send("{\"type\":\"hello_ack\"}").await;
        recv(&mut a).await;
        p.socket.close().await;
        // Transport loss no longer closes the proxy: it waits for the next
        // session, and the client's next hello rides it.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), a.next())
                .await
                .is_err(),
            "proxy stays open across a relay drop"
        );
        // The bridge never replays the application hello on the fresh relay
        // session; the next window's hello is what rides it. The mock relay does
        // not re-dial a data socket for us, so the reconnect itself is covered by
        // the real-relay e2e (scripts/smoke-relay-phase25.sh).
        assert_eq!(seen.len(), 1, "no replayed app hello on the redial");
        r.bridge.stop().await;
    })
    .await
    .expect("reconnect test deadline");
}

#[tokio::test]
async fn window_refs_are_deduplicated_and_last_release_stops() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let mut seen = vec![];
        assert!(r.bridge.acquire("w1"));
        assert!(r.bridge.acquire("w1"));
        assert!(r.bridge.acquire("w2"));
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut a, &granted(&r, "w1")).await;
        let mut p = peer(&r, &mut seen).await;
        p.recv().await;
        let mut b = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut b, &granted(&r, "w2")).await;
        let mut q = peer(&r, &mut seen).await;
        q.recv().await;
        // One reference leaves: the other window keeps the bridge and its session.
        r.bridge.release("w1");
        q.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut b).await, "{\"type\":\"hello_ack\"}");
        // The last reference leaves: the bridge stops and closes every proxy.
        r.bridge.release("w2");
        closed(&mut a).await;
        closed(&mut b).await;
        let mut status = r.bridge.status.clone();
        while *status.borrow_and_update() != HostBridgeStatus::Stopped {
            status.changed().await.unwrap();
        }
        assert!(!r.bridge.acquire("w3"));
        r.bridge.stop().await;
    })
    .await
    .expect("refs test deadline");
}

#[tokio::test]
async fn stop_cancels_pending_transport_handshake() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut a, &granted(&r, "w1")).await;
        while r.relay.client_upgrades() == 0 {
            tokio::task::yield_now().await;
        }
        r.bridge.stop().await;
        closed(&mut a).await;
    })
    .await
    .expect("stop test deadline");
}

use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::{DaemonConfig, DaemonState};
use picot_host::relay::e2ee::DaemonIdentity;
use picot_host::relay::transport::{RelayConfig, RelayTransport};
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::window_owner::TemporaryKind;

struct NoSurface;
#[async_trait]
impl AppSurface for NoSurface {
    async fn open_workspace_window(
        &self,
        _: &str,
        _: &RuntimeTarget,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(), String> {
        Err("unused".into())
    }
    async fn open_devtools(&self, _: &str) -> Result<(), String> {
        Err("unused".into())
    }
    async fn focus_window(&self, _: &str) -> Result<(), String> {
        Err("unused".into())
    }
    async fn close_window(&self, _: &str, _: &str) -> Result<(), String> {
        Err("unused".into())
    }
    async fn cancel_window_close(&self, _: &str, _: &str) -> Result<(), String> {
        Err("unused".into())
    }
    async fn menu_state_changed(&self) -> Result<(), String> {
        Ok(())
    }
    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused".into())
    }
    async fn pane(&self, _: PaneOp) -> Result<Value, String> {
        Err("unused".into())
    }
    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused".into())
    }
    async fn pick_image_files(&self, _: Option<String>) -> Result<Value, String> {
        Err("unused".into())
    }
    async fn save_session_export(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Option<String>,
    ) -> Result<Value, String> {
        Err("unused".into())
    }
    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused".into())
    }
    async fn download_and_install_update(&self, _: SurfaceProgress) -> Result<Value, String> {
        Err("unused".into())
    }
}

#[tokio::test]
async fn real_daemon_pairing_ack_subscription_request_navigation_and_revoke_are_token_free() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let temp = tempfile::tempdir().unwrap();
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "Picot").unwrap();
        let workspace = temp.path().join("ws"); std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let state = Arc::new(DaemonState::start(DaemonConfig {
            data_dir: temp.path().join("data"), config_dir: temp.path().join("config"), static_dir,
            bundle_identifier: "com.palandata.picot.test".into(),
        }, Arc::new(NoSurface)).await.unwrap());
        state.install_control_handler();
        let ws_id = state.metadata.lock().unwrap().add_workspace(&workspace).unwrap().0.workspace_id;
        let (owner, _) = state.owner_registry.create_owner_with_workspace(
            "bridge-window".into(), workspace, 0, ORIGIN.into(), Some(ws_id.clone()), TemporaryKind::DefaultStartup,
        ).unwrap();
        let target = RuntimeTarget::with_owner(ws_id.clone(), "session", "instance", owner.as_str(), 0);
        let mut fake = state.runtimes.register_in_memory(target).unwrap();
        let responder = tokio::spawn(async move {
            while let Some(request) = fake.read_request().await {
                fake.write_frame(json!({"id":request["id"], "type":"response", "success":true, "data":{"sessionId":"session"}})).await.unwrap();
            }
        });
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let offer = state.auth.lock().unwrap().create_pairing(now, Some(owner.as_str()));
        let relay = MockRelay::start().await;
        let identity = DaemonIdentity::for_tests();
        let transport = RelayTransport::start(RelayConfig { endpoint: relay.addr().to_string(), use_tls:false, server_id:"host".into() }, identity.clone(), state.host.relay_session_sink());
        while relay.control_upgrades() == 0 { tokio::time::sleep(Duration::from_millis(5)).await; }
        let saved = Arc::new(Saved { tokens:Mutex::new(vec![]), fail:false });
        let bridge = start_host_bridge_with_seams(ClientRoleConfig {
            endpoint: relay.addr().to_string(), use_tls:false, server_id:"host".into(), peer_public_key_b64: identity.public_key_b64,
        }, Credential::Pairing { pairing_token:DeviceToken::new(offer.token), device_id:"bridge-device".into() }, Some(saved.clone()), BridgeSeams::default()).await.unwrap();
        let grant = authorize(&bridge);
        let mut a = dial(&bridge, Some(ORIGIN)).await; send(&mut a, &hello_with_grant("w1", &grant)).await;
        let token_free = |text: &str| {
            assert!(!text.contains("deviceToken"), "no credential field reaches WebView");
            for token in saved.tokens.lock().unwrap().iter() { assert!(!text.contains(token.expose()), "no credential value reaches WebView"); }
            serde_json::from_str::<Value>(text).unwrap()
        };
        let ack = token_free(&recv(&mut a).await); assert_eq!(ack["type"], "hello_ack");
        // The additive Remote terminal revision survives the bridge's ack
        // sanitization: a device client can only learn it enforces the binding
        // contract from the authenticated ack the bridge forwards (plan §1).
        assert_eq!(
            ack["capabilities"]["terminalBindingVersion"], json!(1),
            "the bridge must preserve the additive capability: {ack}"
        );
        assert_eq!(saved.tokens.lock().unwrap().len(), 1);
        let wire_target = json!({"workspaceId":ws_id,"sessionId":"session","instanceId":"instance"});
        send(&mut a, &json!({"type":"runtime_subscribe","requestId":"sub","target":wire_target}).to_string()).await;
        assert_eq!(token_free(&recv(&mut a).await)["type"], "runtime_subscribed");
        send(&mut a, &json!({"type":"runtime_request","requestId":"req","target":wire_target,"idempotencyKey":"req-key","command":{"type":"get_state"}}).to_string()).await;
        assert_eq!(token_free(&recv(&mut a).await)["type"], "runtime_response");
        send(&mut a, &json!({"type":"host_request","requestId":"nav","operation":"workspace.list","args":{}}).to_string()).await;
        let navigation = token_free(&recv(&mut a).await);
        assert_eq!(navigation["type"], "host_response", "{navigation}");
        assert_eq!(navigation["response"]["workspaces"][0]["workspaceId"], ws_id);
        state.host.revoke_remote_device("bridge-device").unwrap();
        // The revoked session drops; the proxy stays. The next hello is what the
        // daemon refuses, and that explicit rejection is the terminal status.
        let mut b = dial(&bridge, Some(ORIGIN)).await; send(&mut b, &hello_with_grant("w1", &grant)).await; closed(&mut b).await;
        assert_eq!(*bridge.status.borrow(), HostBridgeStatus::CredentialInvalid);
        bridge.stop().await; closed(&mut a).await; transport.stop().await; responder.abort();
        state.host.stop();
    }).await.expect("daemon round trip deadline");
}

struct BlockingSave {
    entered: tokio::sync::Notify,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}
struct CancelMark(Arc<std::sync::atomic::AtomicBool>);
impl Drop for CancelMark {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
#[async_trait]
impl PairingSink for BlockingSave {
    async fn persist(&self, _: DeviceToken) -> Result<(), PairingSaveError> {
        let _mark = CancelMark(self.cancelled.clone());
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn stop_drops_inflight_persistence_callback_without_ack_or_late_commit() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let sink = Arc::new(BlockingSave {
            entered: tokio::sync::Notify::new(),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let r = rig(
            Credential::Pairing {
                pairing_token: DeviceToken::new("offer"),
                device_id: "stable".into(),
            },
            Some(sink.clone()),
        )
        .await;
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut a, &granted(&r, "w1")).await;
        let mut p = peer(&r, &mut vec![]).await;
        p.recv().await;
        p.send("{\"type\":\"hello_ack\",\"deviceToken\":\"minted\"}")
            .await;
        sink.entered.notified().await;
        r.bridge.stop().await;
        closed(&mut a).await;
        assert!(sink.cancelled.load(std::sync::atomic::Ordering::SeqCst));
    })
    .await
    .expect("callback cancellation deadline");
}

#[tokio::test]
async fn explicit_auth_rejection_before_transport_close_wins_over_generic_offline() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        for _ in 0..3 {
            let r = rig(device(), None).await;
            let mut a = dial(&r.bridge, Some(ORIGIN)).await;
            send(&mut a, &granted(&r, "w1")).await;
            let mut p = peer(&r, &mut vec![]).await;
            p.recv().await;
            p.send("{\"type\":\"error\",\"error\":{\"code\":\"unauthorized_device\",\"message\":\"Device token rejected\"}}").await;
            p.socket.close().await;
            closed(&mut a).await;
            assert_eq!(*r.bridge.status.borrow(), HostBridgeStatus::CredentialInvalid);
            r.bridge.stop().await;
        }
    }).await.expect("terminal auth frame deadline");
}

#[tokio::test]
async fn pairing_ack_deadline_abandons_the_exchange_and_frees_the_pairing_slot() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let saved = Arc::new(Saved {
            tokens: Mutex::new(vec![]),
            fail: false,
        });
        let r = rig(
            Credential::Pairing {
                pairing_token: DeviceToken::new("offer-secret"),
                device_id: "stable-id".into(),
            },
            Some(saved.clone()),
        )
        .await;
        let mut seen = vec![];
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut a, &granted(&r, "w1")).await;
        let mut p = peer(&r, &mut seen).await;
        let exchanged: Value = serde_json::from_str(&p.recv().await).unwrap();
        assert_eq!(exchanged["pairingToken"], "offer-secret");
        // Peer swallows hello: no ack, no error, no close. The bridge must give up on
        // its own budget (BridgeSeams::first_frame_timeout) and close the proxy socket.
        let started = tokio::time::Instant::now();
        closed(&mut a).await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "ack wait is bounded by first_frame_timeout"
        );
        // pairing_busy released: the next window is an ordinary attempt, not "busy".
        // The ended proxy's relay session died with it (F-02: a daemon session
        // answers one hello), so the retry rides a FRESH session — a second
        // dial whose new peer receives the pairing hello. This mock-relay peer
        // acks whatever hello it sees; the real refusal of a second hello on a
        // live session is covered by proxy_reconnect_after_webview_drop below.
        let mut b = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut b, &granted(&r, "w2")).await;
        let mut q = peer(&r, &mut seen).await;
        assert_eq!(
            serde_json::from_str::<Value>(&q.recv().await).unwrap()["pairingToken"],
            "offer-secret"
        );
        q.send(r#"{"type":"hello_ack","deviceToken":"minted-secret"}"#)
            .await;
        assert!(!recv(&mut b).await.contains("minted-secret"));
        assert_eq!(saved.tokens.lock().unwrap().len(), 1);
        assert_eq!(*r.bridge.status.borrow(), HostBridgeStatus::Online);
        r.bridge.stop().await;
    })
    .await
    .expect("pairing ack deadline test deadline");
}

#[tokio::test]
async fn pairing_rejection_is_terminal_and_never_replays_the_spent_token() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let saved = Arc::new(Saved {
            tokens: Mutex::new(vec![]),
            fail: false,
        });
        let r = rig(
            Credential::Pairing {
                pairing_token: DeviceToken::new("offer-secret"),
                device_id: "stable-id".into(),
            },
            Some(saved.clone()),
        )
        .await;
        let mut seen = vec![];
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut a, &granted(&r, "w1")).await;
        let mut p = peer(&r, &mut seen).await;
        assert_eq!(
            serde_json::from_str::<Value>(&p.recv().await).unwrap()["pairingToken"],
            "offer-secret"
        );
        p.send("{\"type\":\"error\",\"error\":{\"code\":\"unauthorized_device\",\"message\":\"Pairing offer expired or used\"}}").await;
        closed(&mut a).await;
        assert_eq!(*r.bridge.status.borrow(), HostBridgeStatus::PairingFailed);
        assert!(saved.tokens.lock().unwrap().is_empty());
        // Terminal for this bridge: no hello replay on the relay session, no redial and
        // no new exchange for the next window that dials the same proxy origin.
        assert_eq!(p.socket.next_text_or_close(Duration::from_millis(200)).await, None);
        let upgrades = r.relay.client_upgrades();
        let mut b = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut b, &granted(&r, "w2")).await;
        closed(&mut b).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            r.relay.client_upgrades(),
            upgrades,
            "a spent pairing token never dials the relay again"
        );
        assert_eq!(*r.bridge.status.borrow(), HostBridgeStatus::PairingFailed);
        r.bridge.stop().await;
    })
    .await
    .expect("terminal pairing rejection test deadline");
}

#[tokio::test]
async fn only_a_loopback_hello_starts_a_session_and_the_deadline_closes_the_rest() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        for (origin, frame) in [
            // A page that is not one of the app's own loopback documents.
            (Some("https://evil.test"), hello("w1")),
            (Some("http://evil.test:4321"), hello("w1")),
            (None, hello("w1")),
            // Any first frame that is not a hello, whatever else it carries.
            (
                Some(ORIGIN),
                "{\"type\":\"request\",\"clientId\":\"w1\"}".into(),
            ),
            (Some(ORIGIN), "not json".into()),
        ] {
            let mut s = dial(&r.bridge, origin).await;
            send(&mut s, &frame).await;
            closed(&mut s).await;
        }
        // A socket that never sends its first frame is closed by the deadline.
        let mut s = dial(&r.bridge, Some(ORIGIN)).await;
        closed(&mut s).await;
        assert_eq!(
            r.relay.client_upgrades(),
            0,
            "no session is dialed for a refused handshake"
        );
        r.bridge.stop().await;
    })
    .await
    .expect("handshake gate test deadline");
}

#[tokio::test]
async fn proxy_reconnect_after_webview_drop_gets_a_fresh_hello_and_snapshot() {
    let _serial = serial_guard().await;
    // F-02 against real host session semantics: `run_session` answers exactly
    // one hello, so a proxy that reconnects onto a kept-alive relay session
    // draws an invalid_frame error instead of the fresh snapshot the frontend
    // needs. The mock-relay peers above cannot see this (they hand-ack every
    // hello); only a real daemon session can refuse the second one.
    tokio::time::timeout(Duration::from_secs(30), async {
        let temp = tempfile::tempdir().unwrap();
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "Picot").unwrap();
        let workspace = temp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let state = Arc::new(
            DaemonState::start(
                DaemonConfig {
                    data_dir: temp.path().join("data"),
                    config_dir: temp.path().join("config"),
                    static_dir,
                    bundle_identifier: "com.palandata.picot.test".into(),
                },
                Arc::new(NoSurface),
            )
            .await
            .unwrap(),
        );
        state.install_control_handler();
        let ws_id = state
            .metadata
            .lock()
            .unwrap()
            .add_workspace(&workspace)
            .unwrap()
            .0
            .workspace_id;
        let (owner, _) = state
            .owner_registry
            .create_owner_with_workspace(
                "bridge-window".into(),
                workspace,
                0,
                ORIGIN.into(),
                Some(ws_id.clone()),
                TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let target = RuntimeTarget::with_owner(
            ws_id.clone(),
            "session",
            "instance",
            owner.as_str(),
            0,
        );
        let mut fake = state.runtimes.register_in_memory(target).unwrap();
        let responder = tokio::spawn(async move {
            while let Some(request) = fake.read_request().await {
                fake.write_frame(
                    json!({"id":request["id"], "type":"response", "success":true, "data":{"sessionId":"session"}}),
                )
                .await
                .unwrap();
            }
        });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let offer = state
            .auth
            .lock()
            .unwrap()
            .create_pairing(now, Some(owner.as_str()));
        let relay = MockRelay::start().await;
        let identity = DaemonIdentity::for_tests();
        let transport = RelayTransport::start(
            RelayConfig {
                endpoint: relay.addr().to_string(),
                use_tls: false,
                server_id: "host".into(),
            },
            identity.clone(),
            state.host.relay_session_sink(),
        );
        while relay.control_upgrades() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let saved = Arc::new(Saved {
            tokens: Mutex::new(vec![]),
            fail: false,
        });
        let bridge = start_host_bridge_with_seams(
            ClientRoleConfig {
                endpoint: relay.addr().to_string(),
                use_tls: false,
                server_id: "host".into(),
                peer_public_key_b64: identity.public_key_b64,
            },
            Credential::Pairing {
                pairing_token: DeviceToken::new(offer.token),
                device_id: "bridge-device".into(),
            },
            Some(saved.clone()),
            BridgeSeams::default(),
        )
        .await
        .unwrap();
        let wire_target = json!({"workspaceId":ws_id, "sessionId":"session", "instanceId":"instance"});
        let grant = authorize(&bridge);
        let mut a = dial(&bridge, Some(ORIGIN)).await;
        send(&mut a, &hello_with_grant("w1", &grant)).await;
        let ack: Value = serde_json::from_str(&recv(&mut a).await).unwrap();
        assert_eq!(ack["type"], "hello_ack");
        assert_eq!(saved.tokens.lock().unwrap().len(), 1);
        assert_eq!(relay.client_ids().len(), 1, "one live bridge relay session");
        let old_session = relay.client_ids();
        send(
            &mut a,
            &json!({"type":"runtime_subscribe","requestId":"sub","target":wire_target}).to_string(),
        )
        .await;
        assert_eq!(
            serde_json::from_str::<Value>(&recv(&mut a).await).unwrap()["type"],
            "runtime_subscribed"
        );
        // The WebView drops (reload/crash/WS blip) while the bridge<->relay
        // socket stays perfectly healthy.
        drop(a);
        tokio::time::sleep(Duration::from_millis(250)).await;
        let mut b = dial(&bridge, Some(ORIGIN)).await;
        send(&mut b, &hello_with_grant("w1", &grant)).await;
        let ack: Value = serde_json::from_str(&recv(&mut b).await).unwrap();
        assert_eq!(
            ack["type"], "hello_ack",
            "reconnect must ride a fresh session, not an already-helloed one: {ack}"
        );
        send(
            &mut b,
            &json!({"type":"runtime_subscribe","requestId":"sub2","target":wire_target}).to_string(),
        )
        .await;
        assert_eq!(
            serde_json::from_str::<Value>(&recv(&mut b).await).unwrap()["type"],
            "runtime_subscribed",
            "the fresh session restores the snapshot subscription"
        );
        assert!(
            !relay.client_ids().iter().any(|id| old_session.contains(id)),
            "the ended proxy's relay session must not outlive it"
        );
        bridge.stop().await;
        transport.stop().await;
        responder.abort();
        state.host.stop();
    })
    .await
    .expect("reconnect deadline");
}

#[tokio::test]
async fn the_origin_only_bypass_is_closed_and_denied_admissions_have_no_side_effects() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let counters = r.counters();
        // One live authorized session: a zero delta below means *this* admission
        // did nothing, not that the bridge was idle.
        let grant = r.grant();
        let mut a = r.authorized_with(&grant, "w1").await;
        let mut seen = vec![];
        let mut p = peer(&r, &mut seen).await;
        p.recv().await;
        p.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut a).await, "{\"type\":\"hello_ack\"}");
        let upgrades = r.relay.client_upgrades();
        let before = counters.snapshot();
        assert_eq!(before.0, 1, "the live session read one credential");
        let forge = format!("bg1_{}", "A".repeat(43));
        let duplicate =
            format!(r#"{{"type":"hello","bridgeGrant":"{grant}","bridgeGrant":"{forge}"}}"#);
        let cases: Vec<(Vec<&str>, String)> = vec![
            // The reported bypass: a plausible loopback Origin with no grant.
            (vec![ORIGIN], hello("w1")),
            (vec![ORIGIN], hello_with_grant("w1", "")),
            (vec![ORIGIN], hello_with_grant("w1", "bg1_short")),
            (vec![ORIGIN], hello_with_grant("w1", &forge)),
            (vec![ORIGIN], hello_with_grant("w1", "not-a-grant")),
            // Duplicate authorization fields are refused, never last-value-wins.
            (vec![ORIGIN], duplicate),
            // Correct grant, wrong scheme / port / host / loopback alias.
            (
                vec!["https://127.0.0.1:54321"],
                hello_with_grant("w1", &grant),
            ),
            (vec!["http://127.0.0.1:1"], hello_with_grant("w1", &grant)),
            (
                vec!["http://127.0.0.2:54321"],
                hello_with_grant("w1", &grant),
            ),
            (
                vec!["http://localhost:54321"],
                hello_with_grant("w1", &grant),
            ),
            // Absent, null, duplicate and unparsable Origin headers.
            (vec![], hello_with_grant("w1", &grant)),
            (vec!["null"], hello_with_grant("w1", &grant)),
            (vec![ORIGIN, ORIGIN], hello_with_grant("w1", &grant)),
            (vec!["not an origin"], hello_with_grant("w1", &grant)),
        ];
        for (origins, frame) in cases {
            let mut refused = dial_with_origins(&r.bridge, &origins).await;
            send(&mut refused, &frame).await;
            closed(&mut refused).await;
        }
        assert_eq!(
            counters.snapshot(),
            before,
            "a denied admission must not read credentials, rewrite a hello or dial the relay"
        );
        assert_eq!(
            r.relay.client_upgrades(),
            upgrades,
            "a refused hello never reaches the relay"
        );
        // The authorized neighbour is untouched by the refusals.
        p.send("{\"type\":\"business\",\"n\":1}").await;
        assert_eq!(recv(&mut a).await, "{\"type\":\"business\",\"n\":1}");
        r.bridge.stop().await;
    })
    .await
    .expect("bypass matrix deadline");
}

#[tokio::test]
async fn a_grant_belongs_to_its_own_bridge_instance_and_host() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let other = rig(device(), None).await;
        // Same server id, different bridge instance: exactly the "restarted
        // bridge" and "host-A grant at host-B" shapes.
        let foreign = other.grant();
        let counters = r.counters();
        let before = counters.snapshot();
        let mut s = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut s, &hello_with_grant("w1", &foreign)).await;
        closed(&mut s).await;
        assert_eq!(counters.snapshot(), before);
        assert!(
            other.bridge.mint_grant(&r.binding).is_err(),
            "no cross-bridge mint"
        );
        // The bridge's own grant does authorize.
        let mut own = r.authorized("w1").await;
        let mut seen = vec![];
        let mut p = peer(&r, &mut seen).await;
        assert_eq!(
            serde_json::from_str::<Value>(&p.recv().await).unwrap()["clientId"],
            "w1"
        );
        assert_eq!(
            counters.snapshot().2,
            1,
            "exactly one dial for the admitted proxy"
        );
        p.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut own).await, "{\"type\":\"hello_ack\"}");
        r.bridge.stop().await;
        other.bridge.stop().await;
    })
    .await
    .expect("grant binding deadline");
}

#[tokio::test]
async fn reissue_invalidates_the_previous_secret_without_replacing_live_sessions() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let first = r.grant();
        let mut live = r.authorized_with(&first, "w1").await;
        let mut seen = vec![];
        let mut p = peer(&r, &mut seen).await;
        p.recv().await;
        p.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut live).await, "{\"type\":\"hello_ack\"}");
        // Reconnect: the command mints a fresh secret atomically.
        let second = r.grant();
        assert_ne!(
            first, second,
            "a reissue must not repeat the previous secret"
        );
        let counters = r.counters();
        let before = counters.snapshot();
        let mut stale = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut stale, &hello_with_grant("w1", &first)).await;
        closed(&mut stale).await;
        assert_eq!(
            counters.snapshot(),
            before,
            "the replaced secret no longer admits, with no side effects"
        );
        // The already authorized session is not replaced by the reissue.
        p.send("{\"type\":\"business\",\"n\":2}").await;
        assert_eq!(recv(&mut live).await, "{\"type\":\"business\",\"n\":2}");
        // The new secret admits on the same binding.
        let mut fresh = r.authorized_with(&second, "w1").await;
        let mut q = peer(&r, &mut seen).await;
        q.recv().await;
        assert_eq!(seen.len(), 2);
        // The new secret's proxy is a normal authorized session.
        q.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut fresh).await, "{\"type\":\"hello_ack\"}");
        r.bridge.stop().await;
    })
    .await
    .expect("reissue deadline");
}

#[tokio::test]
async fn revocation_stops_every_proxy_owned_by_that_binding_and_leaves_the_other_usable() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let a_record = r.binding.clone();
        let a_grant = r.grant();
        let mut a1 = r.authorized_with(&a_grant, "a1").await;
        let mut seen = vec![];
        let mut pa1 = peer(&r, &mut seen).await;
        pa1.recv().await;
        pa1.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut a1).await, "{\"type\":\"hello_ack\"}");
        // A second connection for the SAME binding (the window reconnected).
        let mut a2 = r.authorized_with(&a_grant, "a2").await;
        let mut pa2 = peer(&r, &mut seen).await;
        pa2.recv().await;
        // Another window on the same bridge and host, own record.
        let b_record = r.new_binding();
        let b_grant = r.bridge.mint_grant(&b_record).unwrap();
        let mut b = r.authorized_with(&b_grant, "b").await;
        let mut pb = peer(&r, &mut seen).await;
        pb.recv().await;
        pb.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut b).await, "{\"type\":\"hello_ack\"}");

        // Revocation is synchronous authority plus owned-proxy cancellation.
        r.bridge.revoke(&a_record);
        closed(&mut a1).await;
        closed(&mut a2).await;
        assert!(
            r.bridge.mint_grant(&a_record).is_err(),
            "revoked record mints nothing"
        );
        let counters = r.counters();
        let before = counters.snapshot();
        let mut refused = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut refused, &hello_with_grant("a3", &a_grant)).await;
        closed(&mut refused).await;
        assert_eq!(counters.snapshot(), before);
        // B's session and B's grant are unaffected.
        pb.send("{\"type\":\"business\",\"n\":3}").await;
        assert_eq!(recv(&mut b).await, "{\"type\":\"business\",\"n\":3}");
        let mut b2 = r.authorized_with(&b_grant, "b2").await;
        let mut pb2 = peer(&r, &mut seen).await;
        assert_eq!(
            serde_json::from_str::<Value>(&pb2.recv().await).unwrap()["clientId"],
            "b2"
        );
        pb2.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut b2).await, "{\"type\":\"hello_ack\"}");
        r.bridge.stop().await;
    })
    .await
    .expect("revocation deadline");
}

#[tokio::test]
async fn bridge_logs_never_carry_the_grant() {
    let _serial = serial_guard().await;
    tokio::time::timeout(BUDGET, async {
        let logs = captured_logs();
        let r = rig(device(), None).await;
        let grant = r.grant();
        let mut a = r.authorized_with(&grant, "w1").await;
        let mut seen = vec![];
        let mut p = peer(&r, &mut seen).await;
        p.recv().await;
        p.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut a).await, "{\"type\":\"hello_ack\"}");
        // A denial logs a refusal; neither line may quote the secret.
        let mut refused = dial(&r.bridge, Some("http://evil.test")).await;
        send(&mut refused, &hello_with_grant("w1", &grant)).await;
        closed(&mut refused).await;
        let mut malformed = dial(&r.bridge, Some(ORIGIN)).await;
        send(
            &mut malformed,
            &hello_with_grant("w1", &format!("bg1_{}", "B".repeat(43))),
        )
        .await;
        closed(&mut malformed).await;
        let lines = logs.lock().unwrap().clone();
        // The test's own tungstenite client echoes every wire frame it sends at
        // TRACE, which is the WebView's frame, not an app log. Production pins
        // those transport targets to Warn (`main.rs` plugin_log); the app's own
        // targets must never log the secret at any level.
        let transport_echo = |line: &str| {
            [
                "[tokio_tungstenite",
                "[tungstenite",
                "[hyper",
                "[h2",
                "[rustls",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        };
        let app_hits: Vec<&String> = lines
            .iter()
            .filter(|line| line.contains(&grant) && !transport_echo(line))
            .collect();
        let shipped_hits: Vec<&String> = lines
            .iter()
            .filter(|line| {
                line.contains(&grant)
                    && !transport_echo(line)
                    && !line.contains(" TRACE ")
                    && !line.contains(" DEBUG ")
            })
            .collect();
        assert!(
            app_hits.is_empty(),
            "the bridge grant must never reach the app's own log sink: {app_hits:?}"
        );
        assert!(
            shipped_hits.is_empty(),
            "the bridge grant must never reach a log line the shipped Info level keeps: {shipped_hits:?}"
        );
        assert!(
            lines.iter().any(|line| line.starts_with("[picot_host")),
            "the capture must actually see the bridge's log records"
        );
        r.bridge.stop().await;
    })
    .await
    .expect("log sentinel deadline");
}
