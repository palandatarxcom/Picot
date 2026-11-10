// ABOUTME: Client degradation contracts: unreachable add, relay restart, half-open bridge close, TLS.
// ABOUTME: Remote-chain diagnosis: stage timings, early-hello drop, handshake-permit queueing.

#[path = "support/e2ee_client.rs"]
mod e2ee_client;
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
    start_host_bridge_with_seams, BridgeSeams, Credential, HostBridgeHandle, HostBridgeStatus,
};
use picot_host::relay::host_store::DeviceToken;
use picot_host::relay::transport::{
    RelayConfig, RelayTransport, RelayTransportHandle, TransportSeams, TransportTimings,
    E2EE_HANDSHAKE_TIMEOUT,
};
use picot_host::remote_auth::RemoteAuth;
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::transport_limits::E2EE_HANDSHAKE_PERMITS;
use picot_host::window_owner::{TemporaryKind, WindowOwnerRegistry};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
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
    json!({"type":"hello","protocolVersion":2,"clientType":"desktop","clientId":"window"})
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
    auth: Arc<Mutex<RemoteAuth>>,
    owner: String,
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
            Arc::clone(&auth),
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
            auth,
            owner: owner.as_str().to_owned(),
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
    /// A device token for one more synthetic remote device (distinct identities
    /// matter: one relay session per device).
    fn device_token(&self, device: &str) -> String {
        let offer = self
            .auth
            .lock()
            .unwrap()
            .create_pairing(1000, Some(self.owner.as_str()));
        self.auth
            .lock()
            .unwrap()
            .exchange(&offer.token, device, 1001)
            .unwrap()
    }

    /// Lane one: the production server transport behind the mock relay, ready
    /// for production bridges to dial in. Waits for the control socket first —
    /// before it exists the mock's `connected` notify is lost, and a lost
    /// notify means no data dial (an accident, not the window under test).
    async fn chain(
        &self,
        e2ee_handshake_timeout: Duration,
    ) -> (mock_relay::MockRelay, RelayTransportHandle, DaemonIdentity) {
        let relay = mock_relay::MockRelay::start().await;
        let identity = DaemonIdentity::for_tests();
        let transport = RelayTransport::start_with_seams(
            RelayConfig {
                endpoint: relay.addr().to_string(),
                use_tls: false,
                server_id: "host".into(),
            },
            identity.clone(),
            self.host.relay_session_sink(),
            TransportSeams {
                timings: TransportTimings {
                    e2ee_handshake_timeout,
                    ..TransportTimings::default()
                },
                ..TransportSeams::default()
            },
        );
        for _ in 0..400 {
            if relay.control_upgrades() >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            relay.control_upgrades() >= 1,
            "the daemon's control socket never reached the mock relay"
        );
        (relay, transport, identity)
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

// ── Task 2: production-chain connection diagnosis ─────────────────────────
//
// Lane one only: the production `ClientRole`/`HostBridge` behind its loopback
// proxy, a mock relay, and the production server transport into a real
// `HostServer`. That is local production-transport/bridge diagnosis, not GUI
// or public-network acceptance. Timings stay at the production defaults
// (250 ms pre-hello, 1 s resend, at most three E2EE hellos, 10 s server
// handshake budget, four handshake permits) unless a case says otherwise.

/// The outcome of waiting for one frame type on the proxy socket.
#[derive(Debug, Clone, Copy)]
enum Wait {
    Arrived(Duration),
    Closed(Duration),
    TimedOut,
}

impl Wait {
    fn elapsed_micros(self) -> Option<u128> {
        match self {
            Wait::Arrived(duration) | Wait::Closed(duration) => Some(duration.as_micros()),
            Wait::TimedOut => None,
        }
    }
}

async fn wait_kind(socket: &mut Socket, expected: &str, budget: Duration) -> Wait {
    let start = Instant::now();
    let seen = tokio::time::timeout(budget, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    if frame["type"] == expected {
                        return true;
                    }
                }
                Some(Ok(_)) => {}
                _ => return false,
            }
        }
    })
    .await;
    match seen {
        Ok(true) => Wait::Arrived(start.elapsed()),
        Ok(false) => Wait::Closed(start.elapsed()),
        Err(_) => Wait::TimedOut,
    }
}

/// One production connect: client start → proxy dial → application `hello_ack`,
/// then a subscription so "transport ready" and "application useful" are
/// reported separately.
struct Sample {
    ack: Wait,
    start_to_ack: Duration,
    subscribe: Wait,
}

async fn connect_once(
    endpoint: SocketAddr,
    public_key: String,
    token: String,
    target: Value,
) -> Sample {
    let start = Instant::now();
    let bridge = start_host_bridge_with_seams(
        ClientRoleConfig {
            endpoint: endpoint.to_string(),
            use_tls: false,
            server_id: "host".into(),
            peer_public_key_b64: public_key,
        },
        Credential::Device(DeviceToken::new(&token)),
        None,
        BridgeSeams::default(),
    )
    .await
    .unwrap();
    assert!(bridge.acquire("window"));
    let mut socket = dial(&bridge.origin, hello()).await;
    let ack = wait_kind(&mut socket, "hello_ack", Duration::from_secs(30)).await;
    let start_to_ack = start.elapsed();
    let subscribe = match ack {
        Wait::Arrived(_) => {
            socket
                .send(Message::Text(
                    json!({"type":"runtime_subscribe","requestId":"perf-sub","target":target})
                        .to_string(),
                ))
                .await
                .unwrap();
            wait_kind(&mut socket, "runtime_subscribed", Duration::from_secs(10)).await
        }
        _ => Wait::TimedOut,
    };
    bridge.stop().await;
    Sample {
        ack,
        start_to_ack,
        subscribe,
    }
}

/// Nearest-rank percentile over sorted microsecond samples.
fn percentile(sorted: &[u128], percent: usize) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1] as f64 / 1000.0
}

const WARMUPS: usize = 5;
const SAMPLES: usize = 20;
const RUNS: usize = 2;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "diagnostic measurement: run with --ignored --nocapture, not as a CI gate"]
async fn production_chain_connect_stage_timings_one_and_five_clients() {
    tokio::time::timeout(Duration::from_secs(300), async {
        let local = Local::start().await;
        let (relay, transport, identity) = local.chain(E2EE_HANDSHAKE_TIMEOUT).await;
        for run in 1..=RUNS {
            for clients in [1usize, 5] {
                let mut ack_micros = Vec::new();
                let mut subscribe_micros = Vec::new();
                let mut failures = 0usize;
                for sample in 0..(WARMUPS + SAMPLES) {
                    let warmup = sample < WARMUPS;
                    let mut tasks = Vec::new();
                    for client in 0..clients {
                        let token = local
                            .device_token(&format!("perf-{run}-{clients}-{sample}-{client}"));
                        tasks.push(tokio::spawn(connect_once(
                            relay.addr(),
                            identity.public_key_b64.clone(),
                            token,
                            local.target.clone(),
                        )));
                    }
                    for task in tasks {
                        let sample_result = task.await.unwrap();
                        println!(
                            "rust-perf: run={run} clients={clients} sample={sample} warmup={warmup} \
                             start_to_ack_ms={:.1} ack={:?} subscribe={:?}",
                            sample_result.start_to_ack.as_secs_f64() * 1000.0,
                            sample_result.ack,
                            sample_result.subscribe,
                        );
                        if warmup {
                            continue;
                        }
                        match sample_result.ack.elapsed_micros() {
                            Some(micros) => ack_micros.push(micros),
                            None => failures += 1,
                        }
                        if let Some(micros) = sample_result.subscribe.elapsed_micros() {
                            subscribe_micros.push(micros);
                        }
                    }
                }
                ack_micros.sort_unstable();
                subscribe_micros.sort_unstable();
                println!(
                    "rust-perf: summary run={run} clients={clients} samples={} failures={failures} \
                     ack_p50_ms={:.1} ack_p95_ms={:.1} ack_max_ms={:.1} \
                     subscribe_p50_ms={:.1} control_upgrades={} client_upgrades={} \
                     data_dials={} e2ee_hellos={} dropped_client_frames={}",
                    ack_micros.len(),
                    percentile(&ack_micros, 50),
                    percentile(&ack_micros, 95),
                    percentile(&ack_micros, 100),
                    percentile(&subscribe_micros, 50),
                    relay.control_upgrades(),
                    relay.client_upgrades(),
                    relay.data_dials().len(),
                    relay.e2ee_hellos(),
                    relay.dropped_client_frames(),
                );
            }
        }
        transport.stop().await;
    })
    .await
    .expect("connect-stage measurement deadline");
}

/// The window the old direct-client helper removes by waiting for the data
/// dial: the production client's first `e2ee_hello` arrives before the daemon
/// has a data pipe, so the relay drops it. A later resend must recover.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_hello_before_the_data_dial_is_dropped_and_a_resend_recovers() {
    tokio::time::timeout(BUDGET, async {
        let local = Local::start().await;
        let (relay, transport, identity) = local.chain(E2EE_HANDSHAKE_TIMEOUT).await;
        relay.set_attach_hold(true);
        let token = local.device_token("early-hello");
        let bridge = bridge(
            relay.addr(),
            false,
            identity.public_key_b64.clone(),
            &token,
            ClientRoleSeams::default(),
        )
        .await;
        let mut proxy = dial(&bridge.origin, hello()).await;
        for _ in 0..600 {
            if relay.dropped_client_frames() >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let dropped = relay.dropped_client_frames();
        let forwarded = relay.forwarded_client_frames();
        assert!(
            dropped >= 1,
            "the production client never sent a pre-dial e2ee_hello"
        );
        assert_eq!(
            forwarded, 0,
            "nothing may reach the daemon before its data dial"
        );
        assert_eq!(
            relay.data_dials().len(),
            1,
            "the daemon dialed, and its pipe is what the hold keeps unattached"
        );
        relay.set_attach_hold(false);
        let ack = wait_kind(&mut proxy, "hello_ack", Duration::from_secs(20)).await;
        println!(
            "rust-diagnosis: early-hello dropped_before_attach={dropped} \
             forwarded_before_attach={forwarded} hellos_total={} data_dials={} ack={ack:?}",
            relay.e2ee_hellos(),
            relay.data_dials().len(),
        );
        assert!(
            matches!(ack, Wait::Arrived(_)),
            "a later hello must recover the connection: {ack:?}"
        );
        bridge.stop().await;
        transport.stop().await;
    })
    .await
    .expect("early-hello deadline");
}

/// Four handshakes hold every server permit while pausing before a valid
/// hello; the fifth production client's hellos queue on its data pipe. One
/// holder is released inside the server budget, so the fifth completes the
/// E2EE handshake and the server then meets its own queued plaintext hellos.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn four_held_handshakes_queue_the_fifth_production_client() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let local = Local::start().await;
        let (relay, transport, identity) = local.chain(E2EE_HANDSHAKE_TIMEOUT).await;
        let mut holders = Vec::new();
        for index in 0..E2EE_HANDSHAKE_PERMITS {
            holders.push(relay.connect_client().await);
            for _ in 0..600 {
                if relay.data_dials().len() == index + 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(
                relay.data_dials().len(),
                index + 1,
                "the daemon dialed a data socket for each held handshake"
            );
        }
        let token = local.device_token("permit-fifth");
        let bridge = bridge(
            relay.addr(),
            false,
            identity.public_key_b64.clone(),
            &token,
            ClientRoleSeams::default(),
        )
        .await;
        let mut proxy = dial(&bridge.origin, hello()).await;
        for _ in 0..800 {
            if relay.e2ee_hellos() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let hellos_while_blocked = relay.e2ee_hellos();
        assert!(
            hellos_while_blocked >= 2,
            "the fifth client's e2ee_hello must repeat while it waits for a permit \
             (observed {hellos_while_blocked})"
        );
        assert_eq!(
            relay.data_dials().len(),
            E2EE_HANDSHAKE_PERMITS + 1,
            "the fifth connection dialed its own data socket"
        );
        e2ee_client::handshake(&mut holders[0], &identity.public_key_b64).await;
        let ack = wait_kind(&mut proxy, "hello_ack", Duration::from_secs(30)).await;
        let redials = relay.client_upgrades();
        println!(
            "rust-diagnosis: permit-contention hellos_while_blocked={hellos_while_blocked} \
             hellos_total={} redials={redials} data_dials={} ack={ack:?} bridge_status={:?}",
            relay.e2ee_hellos(),
            relay.data_dials().len(),
            *bridge.status.borrow(),
        );
        assert!(
            matches!(ack, Wait::Arrived(_)) || redials >= 2,
            "the fifth client must reach hello_ack or visibly redial: {ack:?}"
        );
        bridge.stop().await;
        transport.stop().await;
    })
    .await
    .expect("permit-contention deadline");
}

/// The other branch: every permit stays held past the server handshake budget.
/// The fifth attempt must time out with the wait inside its budget, open no
/// session, and release the permit so a later client still connects.
///
/// Timing difference from production: the server handshake budget is shrunk to
/// 1.5 s so the case is deterministic; the production value is 10 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fifth_client_times_out_inside_the_permit_wait_and_the_next_one_connects() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let local = Local::start().await;
        let (relay, transport, identity) = local.chain(Duration::from_millis(1500)).await;
        let mut holders = Vec::new();
        for index in 0..E2EE_HANDSHAKE_PERMITS {
            holders.push(relay.connect_client().await);
            for _ in 0..600 {
                if relay.data_dials().len() == index + 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(relay.data_dials().len(), index + 1);
        }
        let token = local.device_token("permit-timeout");
        let starved = bridge(
            relay.addr(),
            false,
            identity.public_key_b64.clone(),
            &token,
            ClientRoleSeams::default(),
        )
        .await;
        let _proxy = dial(&starved.origin, hello()).await;
        for _ in 0..800 {
            if relay.e2ee_hellos() >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The daemon's data socket for the fifth attempt is closed by its own
        // handshake timeout, not by anything the client did.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(
            local
                .host
                .remote_device_sessions_for_tests("permit-timeout"),
            0,
            "a permit-starved handshake never opens a session"
        );
        let blocked_hellos = relay.e2ee_hellos();
        println!(
            "rust-diagnosis: permit-timeout hellos={blocked_hellos} data_dials={} \
             sessions=0 ack_expected=none",
            relay.data_dials().len(),
        );
        starved.stop().await;
        // Permits are free again: a fresh device connects on the same chain.
        let token = local.device_token("after-timeout");
        let fresh = bridge(
            relay.addr(),
            false,
            identity.public_key_b64.clone(),
            &token,
            ClientRoleSeams::default(),
        )
        .await;
        let mut proxy = dial(&fresh.origin, hello()).await;
        let ack = wait_kind(&mut proxy, "hello_ack", Duration::from_secs(20)).await;
        assert!(
            matches!(ack, Wait::Arrived(_)),
            "the timed-out permit must not leak: {ack:?}"
        );
        fresh.stop().await;
        transport.stop().await;
    })
    .await
    .expect("permit-timeout deadline");
}
