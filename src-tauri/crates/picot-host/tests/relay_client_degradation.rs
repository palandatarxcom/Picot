// ABOUTME: Client degradation contracts: unreachable add, relay restart, half-open bridge close, TLS.
// ABOUTME: Real loopback sockets and production JS WebSocketClient; all async work has deadlines.

#[path = "support/mock_relay.rs"]
mod mock_relay;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
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
use picot_host::remote_auth::RemoteAuth;
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::window_owner::{TemporaryKind, WindowOwnerRegistry};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const BUDGET: Duration = Duration::from_secs(60);
const BASE: Duration = Duration::from_millis(20);
const ORIGIN: &str = "http://127.0.0.1:54321";
type Socket = picot_host::relay::client_role::ClientStream;
type Backoff = (Duration, oneshot::Sender<()>);

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
        Arc::new(WindowGrants),
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
    json!({"type":"hello","protocolVersion":2,"clientType":"desktop","clientId":"window","bridgeGrant":"window"})
}
/// The one live window this suite grants: token `window` on ORIGIN for host `host`.
struct WindowGrants;
impl GrantRegistry for WindowGrants {
    fn validate(&self, token: &str, server: &str, origin: &str) -> Option<String> {
        (token == "window" && server == "host" && origin == ORIGIN).then(|| token.to_owned())
    }
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
        // Transport loss no longer closes the proxy: it waits for the next
        // session. Stopping the bridge is what closes it.
        bridge.stop().await;
        closed(&mut proxy).await;
        local.subscribe(&mut desktop, "after-stop").await;
    })
    .await
    .expect("unreachable-add deadline");
}

// Bun supplies real network WebSocket + browser EventTarget; only Origin is
// injected because a CLI has no document origin. Reconnect/hello/subscribe are production JS.
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
        // A failed dial leaves the proxy waiting; stopping the bridge closes it.
        untrusted.stop().await;
        closed(&mut proxy).await;
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
