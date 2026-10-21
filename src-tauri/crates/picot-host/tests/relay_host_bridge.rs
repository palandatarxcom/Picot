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
    start_host_bridge_with_seams, BridgeSeams, Credential, HostBridgeHandle, HostBridgeStatus,
    PairingSaveError, PairingSink,
};
use picot_host::relay::host_store::DeviceToken;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

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
    Rig {
        relay,
        secret,
        bridge,
    }
}
fn device() -> Credential {
    Credential::Device(DeviceToken::new("rust-secret"))
}
fn hello(client_id: &str) -> String {
    json!({"type":"hello", "protocolVersion":2, "clientId":client_id, "clientType":"desktop", "desktopCapability":"local-secret", "bridgeGrant":"local-window"}).to_string()
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

#[tokio::test]
async fn hello_preserves_raw_values_and_business_frames_and_two_sessions_are_independent() {
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let mut seen = Vec::new();
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        let original = r#"{"type":"hello","bridgeGrant":"w1","clientType":"desktop","desktopCapability":"local-secret","clientId":"w1","unknown":{ "x":1e+2, "s":"\u0041" },"canonicalRoute":"a\/b"}"#;
        send(&mut a, original).await;
        let mut p = peer(&r, &mut seen).await;
        let rewritten = p.recv().await;
        let raw = |s: &str| serde_json::from_str::<std::collections::BTreeMap<String, Box<serde_json::value::RawValue>>>(s).unwrap();
        let before = raw(original); let after = raw(&rewritten);
        for (key, value) in &before { if !["clientType", "desktopCapability", "bridgeGrant"].contains(&key.as_str()) { assert_eq!(after[key].get(), value.get(), "raw {key}"); } }
        let v: Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(v["clientType"], "remote"); assert_eq!(v["deviceToken"], "rust-secret");
        assert!(v.get("desktopCapability").is_none()); assert!(v.get("bridgeGrant").is_none());
        p.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut a).await, "{\"type\":\"hello_ack\"}");
        let frame = "{ \"type\": \"business\", \"n\": 1e+2 }";
        send(&mut a, frame).await; assert_eq!(p.recv().await, frame);
        p.send(frame).await; assert_eq!(recv(&mut a).await, frame);
        let mut b = dial(&r.bridge, Some(ORIGIN)).await; send(&mut b, &hello("w2")).await;
        let mut q = peer(&r, &mut seen).await; assert_eq!(serde_json::from_str::<Value>(&q.recv().await).unwrap()["clientId"], "w2");
        assert_eq!(seen.len(), 2);
        r.bridge.stop().await; closed(&mut a).await; closed(&mut b).await;
    }).await.expect("rewrite test deadline");
}

#[tokio::test]
async fn pairing_single_flight_sanitizes_ack_and_failed_save_is_terminal() {
    tokio::time::timeout(BUDGET, async {
        for fail in [false, true] {
            let saved = Arc::new(Saved { tokens: Mutex::new(vec![]), fail });
            let r = rig(Credential::Pairing { pairing_token: DeviceToken::new("offer-secret"), device_id: "stable-id".into() }, Some(saved.clone())).await;
            let mut seen = vec![];
            let mut a = dial(&r.bridge, Some(ORIGIN)).await; send(&mut a, &hello("w1")).await;
            let mut p = peer(&r, &mut seen).await;
            let v: Value = serde_json::from_str(&p.recv().await).unwrap();
            assert_eq!(v["pairingToken"], "offer-secret"); assert_eq!(v["deviceId"], "stable-id"); assert!(v.get("deviceToken").is_none());
            // Chosen policy: reject second proxy during exchange, never consume offer twice.
            let mut b = dial(&r.bridge, Some(ORIGIN)).await; send(&mut b, &hello("w2")).await; closed(&mut b).await;
            assert_eq!(r.relay.client_upgrades(), 1);
            p.send(r#"{"type":"hello_ack","deviceToken":"minted-secret","capabilities":{"native":true}}"#).await;
            if fail {
                closed(&mut a).await;
                assert_eq!(*r.bridge.status.borrow(), HostBridgeStatus::PairingFailed);
            } else {
                let ack = recv(&mut a).await; assert!(!ack.contains("deviceToken")); assert!(!ack.contains("minted-secret"));
                assert_eq!(serde_json::from_str::<Value>(&ack).unwrap()["capabilities"]["native"], true);
                let mut b = dial(&r.bridge, Some(ORIGIN)).await; send(&mut b, &hello("w2")).await;
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
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let mut seen = vec![];
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut a, &hello("w1")).await;
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
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let mut seen = vec![];
        assert!(r.bridge.acquire("w1"));
        assert!(r.bridge.acquire("w1"));
        assert!(r.bridge.acquire("w2"));
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut a, &hello("w1")).await;
        let mut p = peer(&r, &mut seen).await;
        p.recv().await;
        let mut b = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut b, &hello("w2")).await;
        let mut q = peer(&r, &mut seen).await;
        q.recv().await;
        r.bridge.release("w1");
        q.send("{\"type\":\"hello_ack\"}").await;
        assert_eq!(recv(&mut b).await, "{\"type\":\"hello_ack\"}");
        r.bridge.release("w2");
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
    tokio::time::timeout(BUDGET, async {
        let r = rig(device(), None).await;
        let mut a = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut a, &hello("w1")).await;
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
        let mut a = dial(&bridge, Some(ORIGIN)).await; send(&mut a, &hello("w1")).await;
        let token_free = |text: &str| {
            assert!(!text.contains("deviceToken"), "no credential field reaches WebView");
            for token in saved.tokens.lock().unwrap().iter() { assert!(!text.contains(token.expose()), "no credential value reaches WebView"); }
            serde_json::from_str::<Value>(text).unwrap()
        };
        let ack = token_free(&recv(&mut a).await); assert_eq!(ack["type"], "hello_ack");
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
        let mut b = dial(&bridge, Some(ORIGIN)).await; send(&mut b, &hello("w1")).await; closed(&mut b).await;
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
        send(&mut a, &hello("w1")).await;
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
    tokio::time::timeout(BUDGET, async {
        for _ in 0..3 {
            let r = rig(device(), None).await;
            let mut a = dial(&r.bridge, Some(ORIGIN)).await;
            send(&mut a, &hello("w1")).await;
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
        send(&mut a, &hello("w1")).await;
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
        let mut b = dial(&r.bridge, Some(ORIGIN)).await;
        send(&mut b, &hello("w2")).await;
        // Session is retained across proxy sessions, so the retry rides the SAME
        // relay session: no second data dial, the hello arrives on this peer.
        assert_eq!(
            serde_json::from_str::<Value>(&p.recv().await).unwrap()["pairingToken"],
            "offer-secret"
        );
        p.send(r#"{"type":"hello_ack","deviceToken":"minted-secret"}"#)
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
        send(&mut a, &hello("w1")).await;
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
        send(&mut b, &hello("w2")).await;
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
