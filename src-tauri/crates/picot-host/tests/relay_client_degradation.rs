// ABOUTME: Client degradation contracts: unreachable add, relay restart, half-open bridge close, TLS.
// ABOUTME: Real loopback sockets and production JS WebSocketClient; all async work has deadlines.

#[path = "support/mock_relay.rs"]
mod mock_relay;

use std::net::{Ipv4Addr, SocketAddr};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use mock_relay::MockRelay;
use picot_host::host_server::HostServer;
use picot_host::metadata_store::MetadataStore;
use picot_host::native_pi_manager::NativePiManager;
use picot_host::relay::client_role::{ClientRoleConfig, ClientRoleSeams};
use picot_host::relay::e2ee::DaemonIdentity;
use picot_host::relay::host_bridge::{
    start_host_bridge_with_seams, BridgeSeams, Credential, GrantRegistry, HostBridgeHandle,
    HostBridgeStatus,
};
use picot_host::relay::host_store::DeviceToken;
use picot_host::relay::transport::{RelayConfig, RelayTransport, TransportSeams};
use picot_host::remote_auth::RemoteAuth;
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::window_owner::{TemporaryKind, WindowOwnerRegistry};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const BUDGET: Duration = Duration::from_secs(60);
const BASE: Duration = Duration::from_millis(20);
const ORIGIN: &str = "http://127.0.0.1:54321";
type Socket = picot_host::relay::client_role::ClientStream;
type Backoff = (Duration, oneshot::Sender<()>);

struct Grants;
impl GrantRegistry for Grants {
    fn validate(&self, token: &str, server: &str, origin: &str) -> Option<String> {
        (token == "grant" && server == "host" && origin == ORIGIN).then(|| "window".into())
    }
}

// Each recorded sleep proves one failed attempt; permits keep Offline observable
// without polling races or waiting through the production 30-second cap.
// Dropping a permit parks that attempt until bridge.stop() cancels it.
fn client_seams() -> (ClientRoleSeams, mpsc::UnboundedReceiver<Backoff>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut seams = ClientRoleSeams::default();
    seams.timings.pre_hello_delay = Duration::from_millis(10);
    seams.timings.hello_resend_after = Duration::from_millis(50);
    seams.timings.dial_timeout = Duration::from_secs(2);
    seams.timings.handshake_timeout = Duration::from_secs(2);
    seams.timings.backoff_base = BASE;
    seams.timings.backoff_cap = BASE * 3;
    seams.sleep = Arc::new(move |duration| {
        let (permit, wait) = oneshot::channel();
        let tx = tx.clone();
        Box::pin(async move {
            tx.send((duration, permit)).unwrap();
            if wait.await.is_err() {
                std::future::pending::<()>().await;
            }
        })
    });
    (seams, rx)
}

async fn bridge(
    addr: SocketAddr,
    tls: bool,
    public_key: String,
    token: &str,
    client: ClientRoleSeams,
) -> HostBridgeHandle {
    let bridge = start_host_bridge_with_seams(
        ClientRoleConfig {
            endpoint: addr.to_string(),
            use_tls: tls,
            server_id: "host".into(),
            peer_public_key_b64: public_key,
        },
        Credential::Device(DeviceToken::new(token)),
        Arc::new(Grants),
        None,
        BridgeSeams {
            client,
            ..BridgeSeams::default()
        },
    )
    .await
    .unwrap();
    assert!(bridge.acquire("window"));
    bridge
}

async fn dial(origin: &str, hello: Value) -> Socket {
    let mut request = (origin.replacen("http://", "ws://", 1) + "/v2/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Origin", ORIGIN.parse().unwrap());
    let mut socket = tokio_tungstenite::connect_async(request).await.unwrap().0;
    socket.send(Message::Text(hello.to_string())).await.unwrap();
    socket
}
fn hello() -> Value {
    json!({"type":"hello","protocolVersion":2,"clientType":"desktop","clientId":"window","bridgeGrant":"grant"})
}
async fn recv_type(socket: &mut Socket, expected: &str) -> Value {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                let frame: Value = serde_json::from_str(&text).unwrap();
                if frame["type"] == "owner_bootstrap" {
                    continue;
                }
                assert_eq!(frame["type"], expected, "{frame}");
                return frame;
            }
            Some(Ok(Message::Ping(_))) => {}
            frame => panic!("expected {expected}, got {frame:?}"),
        }
    }
}
async fn closed(socket: &mut Socket) {
    loop {
        match socket.next().await {
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
            Some(Ok(Message::Ping(_))) => {}
            frame => panic!("expected proxy close, got {frame:?}"),
        }
    }
}
async fn status(receiver: &mut watch::Receiver<HostBridgeStatus>, expected: HostBridgeStatus) {
    while *receiver.borrow_and_update() != expected {
        receiver.changed().await.unwrap();
    }
}

struct Local {
    host: HostServer,
    capability: String,
    token: String,
    target: Value,
    _runtime: picot_host::pi_rpc_bridge::InMemoryPiProcess,
    _temp: tempfile::TempDir,
}
impl Local {
    async fn start() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let static_dir = temp.path().join("public");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(static_dir.join("index.html"), "local remains available").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.path().join("metadata.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(metadata.clone())));
        let registry = Arc::new(WindowOwnerRegistry::default());
        let (owner, capability) = registry
            .create_owner_with_workspace(
                "local-window".into(),
                workspace.canonicalize().unwrap(),
                0,
                ORIGIN.into(),
                Some("workspace".into()),
                TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let target =
            RuntimeTarget::with_owner("workspace", "session", "instance", owner.as_str(), 0);
        let runtimes = NativePiManager::new(1);
        let runtime = runtimes.register_in_memory(target).unwrap();
        let offer = auth
            .lock()
            .unwrap()
            .create_pairing(1000, Some(owner.as_str()));
        let token = auth
            .lock()
            .unwrap()
            .exchange(&offer.token, "device", 1001)
            .unwrap();
        let host = HostServer::start_with_session_root(
            static_dir,
            runtimes,
            auth,
            metadata,
            Some(temp.path().join("sessions")),
        )
        .await
        .unwrap();
        host.set_owner_registry(registry);
        Self {
            host,
            capability,
            token,
            target: json!({"workspaceId":"workspace","sessionId":"session","instanceId":"instance"}),
            _runtime: runtime,
            _temp: temp,
        }
    }
    async fn connect(&self) -> Socket {
        let mut socket = dial(
            self.host.origin(),
            json!({
                "type":"hello","protocolVersion":2,"clientType":"desktop",
                "clientId":"local-window","desktopCapability":self.capability,
            }),
        )
        .await;
        recv_type(&mut socket, "hello_ack").await;
        socket
    }
    async fn subscribe(&self, socket: &mut Socket, request: &str) {
        socket
            .send(Message::Text(
                json!({
                    "type":"runtime_subscribe","requestId":request,"target":self.target,
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            recv_type(socket, "runtime_subscribed").await["requestId"],
            request
        );
    }
}
impl Drop for Local {
    fn drop(&mut self) {
        self.host.stop();
    }
}

#[tokio::test]
async fn unreachable_at_add_is_offline_retries_on_curve_and_local_still_works() {
    tokio::time::timeout(BUDGET, async {
        let local = Local::start().await;
        let mut desktop = local.connect().await;
        local.subscribe(&mut desktop, "before-add").await;
        let unused = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = unused.local_addr().unwrap();
        drop(unused);
        let identity = DaemonIdentity::for_tests();
        let (seams, mut attempts) = client_seams();
        let bridge = bridge(addr, false, identity.public_key_b64, &local.token, seams).await;
        let mut proxy = dial(&bridge.origin, hello()).await;
        for (index, n) in [1, 2, 3, 3].into_iter().enumerate() {
            let (delay, permit) = attempts.recv().await.unwrap();
            assert_eq!(delay, BASE * n, "one recorded sleep per failed dial");
            assert_eq!(*bridge.status.borrow(), HostBridgeStatus::Offline);
            local
                .subscribe(&mut desktop, &format!("while-offline-{n}"))
                .await;
            if index < 3 {
                permit.send(()).unwrap();
            }
        }
        closed(&mut proxy).await;
        bridge.stop().await;
        local.subscribe(&mut desktop, "after-stop").await;
    })
    .await
    .expect("unreachable-add deadline");
}

// Bun supplies real network WebSocket + browser EventTarget; only Origin is
// injected because a CLI has no document origin. Reconnect/hello/subscribe are production JS.
const REAL_CLIENT: &str = r#"
import assert from 'node:assert/strict';
import { WebSocketClient } from './public/app/websocket-client.js';
const NativeWebSocket = globalThis.WebSocket;
globalThis.WebSocket = class extends NativeWebSocket {
  constructor(url) { super(url, { headers: { Origin: process.env.T10_ORIGIN } }); }
};
const deadline = setTimeout(() => { console.error('client deadline'); process.exit(1); }, 12000);
const client = new WebSocketClient(process.env.T10_URL, { identity: 'bridge', grant: 'grant' });
client.reconnectDelay = 50;
client.maxReconnectDelay = 100;
let connections = 0;
let drops = 0;
client.addEventListener('disconnected', () => {
  assert.equal(connections, 1, 'authenticated proxy must close on transport Offline');
  drops++;
  console.log('T10:closed');
});
client.addEventListener('connected', () => {
  assert.equal(client.authenticated, true);
  connections++;
  if (connections === 1) { console.log('T10:online'); return; }
  assert.equal(connections, 2);
  assert.equal(drops, 1);
  client.ws.addEventListener('message', ({ data }) => {
    const frame = JSON.parse(data);
    if (frame.type !== 'runtime_subscribed') return;
    assert.match(frame.requestId, /^sub-/);
    assert.equal(client.authenticated, true, 'fresh hello_ack precedes subscription');
    console.log('T10:subscribed');
    clearTimeout(deadline);
    client.disconnect();
    process.exit(0);
  });
  client.subscribeRuntimeTarget(JSON.parse(process.env.T10_TARGET));
});
client.connect();
"#;

#[tokio::test]
async fn relay_dies_returns_and_real_websocket_client_rehellos_and_subscribes() {
    tokio::time::timeout(BUDGET, async {
        let local = Local::start().await;
        let relay = MockRelay::start().await;
        let addr = relay.addr();
        let identity = DaemonIdentity::for_tests();
        let mut transport_seams = TransportSeams::default();
        transport_seams.timings.backoff_base = BASE;
        let transport = RelayTransport::start_with_seams(
            RelayConfig {
                endpoint: addr.to_string(),
                use_tls: false,
                server_id: "host".into(),
            },
            identity.clone(),
            local.host.relay_session_sink(),
            transport_seams,
        );
        while relay.control_upgrades() == 0 {
            tokio::time::sleep(BASE).await;
        }
        let (seams, mut attempts) = client_seams();
        let bridge = bridge(addr, false, identity.public_key_b64, &local.token, seams).await;
        let mut states = bridge.status.clone();
        let mut child = tokio::process::Command::new("bun")
            .args(["--eval", REAL_CLIENT])
            .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .env("T10_ORIGIN", ORIGIN)
            .env(
                "T10_URL",
                bridge.origin.replacen("http://", "ws://", 1) + "/v2/ws",
            )
            .env("T10_TARGET", local.target.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("Bun required for production WebSocketClient e2e");
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        async fn marker(
            lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
            expected: &str,
        ) {
            while let Some(line) = lines.next_line().await.unwrap() {
                if line.starts_with("T10:") {
                    assert_eq!(line, expected);
                    return;
                }
            }
            panic!("client exited before {expected}");
        }
        marker(&mut lines, "T10:online").await;
        status(&mut states, HostBridgeStatus::Online).await;
        relay.stop().await;
        let (delay, permit) = attempts.recv().await.unwrap();
        assert_eq!(delay, BASE, "Online resets backoff");
        status(&mut states, HostBridgeStatus::Offline).await;
        marker(&mut lines, "T10:closed").await;
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "listener really stopped"
        );
        permit.send(()).unwrap();
        let (delay, permit) = attempts.recv().await.unwrap();
        assert_eq!(
            delay,
            BASE * 2,
            "failed dial while relay down advances curve"
        );
        let relay = MockRelay::start_at(addr).await;
        while relay.control_upgrades() == 0 {
            tokio::time::sleep(BASE).await;
        }
        permit.send(()).unwrap();
        marker(&mut lines, "T10:subscribed").await;
        assert_eq!(*bridge.status.borrow(), HostBridgeStatus::Online);
        let exit = child.wait().await.unwrap();
        assert!(exit.success(), "production JS client: {exit}");
        assert_eq!(
            relay.client_upgrades(),
            1,
            "idle role reused by reconnected WebView"
        );
        bridge.stop().await;
        transport.stop().await;
        relay.stop().await;
    })
    .await
    .expect("relay restart e2e deadline");
}

#[tokio::test]
async fn client_role_half_open_offline_closes_bridge_proxy() {
    tokio::time::timeout(BUDGET, async {
        let relay = MockRelay::start().await;
        let secret = crypto_box::SecretKey::generate(&mut rand::rngs::OsRng);
        let (mut seams, mut attempts) = client_seams();
        seams.timings.idle_timeout = Duration::from_millis(150);
        seams.timings.ping_interval = Duration::from_millis(50);
        let bridge = bridge(
            relay.addr(),
            false,
            B64.encode(secret.public_key().as_bytes()),
            "token",
            seams,
        )
        .await;
        let mut proxy = dial(&bridge.origin, hello()).await;
        let id = loop {
            if let Some(id) = relay.client_ids().first() {
                break id.clone();
            }
            tokio::time::sleep(BASE).await;
        };
        let mut peer = relay.connect_data(&id).await;
        let hello: Value = serde_json::from_str(&peer.recv_text().await).unwrap();
        let key = picot_host::relay::e2ee::derive_shared_key(
            &secret.to_bytes(),
            hello["key"].as_str().unwrap(),
        )
        .unwrap();
        peer.send_text("{\"type\":\"e2ee_ready\"}").await;
        peer.recv_text().await;
        peer.send_text(&B64.encode(picot_host::relay::e2ee::seal(
            &key,
            b"{\"type\":\"hello_ack\"}",
        )))
        .await;
        recv_type(&mut proxy, "hello_ack").await;
        assert_eq!(*bridge.status.borrow(), HostBridgeStatus::Online);
        relay.set_client_park(true);
        let (delay, _permit) = attempts.recv().await.unwrap();
        assert_eq!(delay, BASE);
        assert_eq!(*bridge.status.borrow(), HostBridgeStatus::Offline);
        closed(&mut proxy).await;
        bridge.stop().await;
        relay.stop().await;
    })
    .await
    .expect("half-open bridge close deadline");
}

#[tokio::test]
async fn client_tls_trusts_fixture_ca_but_production_default_rejects_unknown_ca() {
    tokio::time::timeout(BUDGET, async {
        let cert = tokio::task::spawn_blocking(mock_relay::test_certificate)
            .await
            .unwrap();
        let relay = mock_relay::TlsMockRelay::start_client(&cert).await;
        let tls = native_tls::TlsConnector::builder()
            .add_root_certificate(
                native_tls::Certificate::from_pem(cert.ca_pem.as_bytes()).unwrap(),
            )
            .build()
            .unwrap();
        let identity = DaemonIdentity::for_tests();
        let (mut seams, _attempts) = client_seams();
        seams.connector = Some(tokio_tungstenite::Connector::NativeTls(tls));
        let trusted = bridge(
            relay.addr(),
            true,
            identity.public_key_b64.clone(),
            "token",
            seams,
        )
        .await;
        let mut proxy = dial(&trusted.origin, hello()).await;
        status(&mut trusted.status.clone(), HostBridgeStatus::Online).await;
        assert_eq!(relay.control_upgrades(), 1);
        trusted.stop().await;
        closed(&mut proxy).await;

        let (seams, mut attempts) = client_seams();
        assert!(
            seams.connector.is_none(),
            "production connector: system roots"
        );
        let untrusted = bridge(relay.addr(), true, identity.public_key_b64, "token", seams).await;
        let mut proxy = dial(&untrusted.origin, hello()).await;
        for n in 1..=3 {
            let (delay, permit) = attempts.recv().await.unwrap();
            assert_eq!(delay, BASE * n);
            assert_eq!(*untrusted.status.borrow(), HostBridgeStatus::Offline);
            assert_eq!(
                relay.control_upgrades(),
                1,
                "unknown CA never upgrades, no insecure fallback"
            );
            assert_eq!(
                relay.plaintext_dials(),
                0,
                "never retry wss as plaintext HTTP"
            );
            if n < 3 {
                permit.send(()).unwrap();
            }
        }
        closed(&mut proxy).await;
        untrusted.stop().await;
        // Prove the downgrade counter is live, not an always-zero assertion.
        assert!(
            tokio_tungstenite::connect_async(format!("ws://{}/ws?role=client", relay.addr()))
                .await
                .is_err()
        );
        assert_eq!(relay.plaintext_dials(), 1);
    })
    .await
    .expect("client TLS contract deadline");
}
