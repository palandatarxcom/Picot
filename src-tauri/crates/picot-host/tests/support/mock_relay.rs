// ABOUTME: Mock relay server for protocol v2 contract tests (plan T4/T5).
// ABOUTME: /ws role routing (control + client + server&connectionId data
// ABOUTME: pipe), GET /health; data pipes forward bytes bidirectionally.

//! A real axum server standing in for the company relay, driven from the
//! integration tests (`tests/relay_control_transport.rs`). Control frames are
//! the literal JSON shapes from the plan's protocol tables — never generated
//! by round-tripping the daemon's types (engineering lesson #1).
//!
//! Data pipes: a client-role socket is assigned a `c-N` connectionId and
//! reported via `connected`; when the daemon dials
//! `role=server&connectionId=…`, the two sockets are spliced through bounded
//! channels, so frames flow client ↔ daemon through the mock exactly as
//! through the real relay. Bounded pipes are what make the slow-consumer
//! test honest: a client that stops reading backpressures its channel, which
//! stalls the mock's data-socket read loop, which fills the daemon's outbound
//! queue.
//!
//! Test knobs the daemon's reconnect/stale paths need:
//!
//! - `set_gate(false)`: control upgrades are closed immediately, so the daemon
//!   cycles connect → drop → backoff without any server shutdown;
//! - `set_silent`: `BeforeReady` parks the socket without the initial `sync`
//!   (drives the 8s control-ready timeout), `AfterSync` sends `sync` then
//!   parks without reading (no WS pong → drives the 30s stale force-close);
//! - `send_control` / `drop_control`: inject raw control frames or sever the
//!   control socket from the test side.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};

// ── TLS contract fixtures (plan T4 step 1) ─────────────────────────────────
//
// rcgen mints the test CA and the server certificate in-process, so no PEM
// fixture ever lands in the repo. The CA signs the server cert exactly as a
// company relay's would, and the daemon's connector trusts only the CA.

/// The certificate pair the TLS contract cases need.
#[allow(dead_code)] // shared support: per-target subset usage
pub struct TestCertificate {
    /// CA certificate PEM — the only root the fixture connector trusts.
    pub ca_pem: String,
    /// Server certificate PEM for the listener's identity.
    pub server_pem: String,
    /// Server key PEM for the listener's identity.
    pub server_key_pem: String,
}

#[allow(dead_code)] // shared support: per-target subset usage
pub fn test_certificate() -> TestCertificate {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose,
    };

    let ca_key = KeyPair::generate().expect("CA key");
    let mut ca_params = CertificateParams::default();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Picot relay test CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = ca_params.self_signed(&ca_key).expect("CA certificate");

    // The server key is RSA: SecurityFramework (native-tls's macOS backend)
    // rejects PKCS#8 EC keys at identity import (`errSecUnknownFormat`) while
    // PKCS#8 RSA imports cleanly — native-tls's own from_pkcs8 tests rely on
    // the same combination. rcgen's ring backend cannot generate RSA keys, so
    // the pure-Rust `rsa` crate mints it and rcgen only loads it.
    use rsa::pkcs8::EncodePrivateKey;

    let mut rng = rand::thread_rng();
    let rsa_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("server RSA key generation");
    let server_key = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
        &rsa_key
            .to_pkcs8_der()
            .expect("server key PKCS#8 export")
            .as_bytes()
            .into(),
        &rcgen::PKCS_RSA_SHA256,
    )
    .expect("server key load");
    // SANs cover both dial forms the transport can produce for loopback.
    let mut server_params =
        CertificateParams::new(["localhost".to_string(), "127.0.0.1".to_string()])
            .expect("server params");
    // The key usages a TLS server certificate is validated against.
    server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let issuer = Issuer::new(ca_params, ca_key);
    let server = server_params
        .signed_by(&server_key, &issuer)
        .expect("server certificate");

    TestCertificate {
        ca_pem: ca.pem(),
        server_pem: server.pem(),
        server_key_pem: server_key.serialize_pem(),
    }
}

/// A TLS-listening relay for the transport's TLS contract cases (spec §2.5,
/// D10): the control-socket behavior of the plaintext mock (upgrade → initial
/// `sync`) over a native-tls listener holding the test certificate. Data
/// sockets stay out of scope — TLS sits below the WS layer the plaintext
/// mock's data paths already cover.
pub struct TlsMockRelay {
    addr: std::net::SocketAddr,
    upgrades: Arc<AtomicU32>,
    plaintext_dials: Arc<AtomicU32>,
}

#[allow(dead_code)] // shared support: per-target subset usage
impl TlsMockRelay {
    /// Bind an ephemeral loopback TLS port serving the control contract.
    pub async fn start(cert: &TestCertificate) -> TlsMockRelay {
        Self::start_role(cert, false).await
    }

    /// Same TLS listener, answering the client-role E2EE handshake instead of sync.
    pub async fn start_client(cert: &TestCertificate) -> TlsMockRelay {
        Self::start_role(cert, true).await
    }

    async fn start_role(cert: &TestCertificate, client_role: bool) -> TlsMockRelay {
        let identity = native_tls::Identity::from_pkcs8(
            cert.server_pem.as_bytes(),
            cert.server_key_pem.as_bytes(),
        )
        .expect("server identity");
        let acceptor = tokio_native_tls::TlsAcceptor::from(
            native_tls::TlsAcceptor::new(identity).expect("TLS acceptor"),
        );
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind TLS mock relay");
        let addr = listener.local_addr().expect("TLS mock relay port");
        let upgrades = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&upgrades);
        let plaintext_dials = Arc::new(AtomicU32::new(0));
        let plaintext = plaintext_dials.clone();
        tokio::spawn(async move {
            loop {
                // Serial by design: the daemon holds one control socket, and
                // a rejected handshake (the default-connector case) returns
                // the loop to accept at once.
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                // A WS downgrade starts with GET, not a TLS handshake record.
                let mut first = [0];
                if matches!(
                    tokio::time::timeout(Duration::from_secs(2), tcp.peek(&mut first)).await,
                    Ok(Ok(1))
                ) && first[0] == b'G'
                {
                    plaintext.fetch_add(1, Ordering::SeqCst);
                    continue;
                }
                let Ok(tls) = acceptor.accept(tcp).await else {
                    continue;
                };
                let Ok(mut socket) = tokio_tungstenite::accept_async(tls).await else {
                    continue;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let initial = if client_role {
                    let Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(hello)))) =
                        tokio::time::timeout(Duration::from_secs(2), socket.next()).await
                    else {
                        continue;
                    };
                    let hello: serde_json::Value =
                        serde_json::from_str(&hello).expect("client E2EE hello");
                    assert_eq!(hello["type"], "e2ee_hello");
                    "{\"type\":\"e2ee_ready\"}"
                } else {
                    "{\"type\":\"sync\",\"connectionIds\":[]}"
                };
                if socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        initial.to_string(),
                    ))
                    .await
                    .is_err()
                {
                    continue;
                }
                // Pump reads so the daemon's WS pings get answered while
                // the control socket lives.
                while let Some(Ok(_)) = socket.next().await {}
            }
        });
        TlsMockRelay {
            addr,
            upgrades,
            plaintext_dials,
        }
    }

    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    /// Completed WS upgrades over TLS — a rejected TLS handshake never counts.
    pub fn control_upgrades(&self) -> u32 {
        self.upgrades.load(Ordering::SeqCst)
    }

    /// Plaintext WS upgrade attempts made against this TLS-only endpoint.
    pub fn plaintext_dials(&self) -> u32 {
        self.plaintext_dials.load(Ordering::SeqCst)
    }
}
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use picot_host::transport_limits::RELAY_DATA_FRAME_TEXT_BYTES;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Forwarding channels are bounded so a stalled peer backpressures the source
/// socket instead of growing mock memory without end.
const PIPE_CAPACITY: usize = 8;

/// How a control socket stays quiet, per test.
// Shared support module: each test target uses a subset of these knobs, so
// per-target dead_code analysis cannot see the whole picture.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilentMode {
    /// Upgrade but never send the initial `sync` — control-ready timeout path.
    BeforeReady,
    /// Send the initial `sync`, then park without reading — stale path (no pong).
    AfterSync,
}

pub struct MockRelay {
    inner: Arc<Inner>,
    addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

struct Inner {
    shutdown: watch::Sender<bool>,
    gate_open: Mutex<bool>,
    silent: Mutex<Option<SilentMode>>,
    /// Park client-role sockets after registration: never reading them means
    /// no automatic WS pong, so the client's liveness probes go unanswered.
    client_park: watch::Sender<bool>,
    /// Writer half of the current interactive control socket.
    control: Mutex<Option<mpsc::UnboundedSender<Message>>>,
    control_upgrades: AtomicU32,
    client_upgrades: AtomicU32,
    next_id: AtomicU32,
    /// connectionId → writer channel of the client-role socket.
    clients: Mutex<HashMap<String, mpsc::Sender<Message>>>,
    /// connectionId → writer channel of the daemon's data socket.
    data_pipes: Mutex<HashMap<String, mpsc::Sender<Message>>>,
    /// (serverId, connectionId) of every server+connectionId dial.
    data_dials: Mutex<Vec<(String, String)>>,
}

#[allow(dead_code)] // shared support: per-target subset usage
impl MockRelay {
    /// Bind an ephemeral loopback port and serve the mock relay.
    pub async fn start() -> MockRelay {
        Self::start_at((std::net::Ipv4Addr::LOCALHOST, 0).into()).await
    }

    /// Restart tests rebind the exact endpoint clients already know.
    pub async fn start_at(addr: std::net::SocketAddr) -> MockRelay {
        let inner = Arc::new(Inner {
            shutdown: watch::channel(false).0,
            gate_open: Mutex::new(true),
            silent: Mutex::new(None),
            client_park: watch::channel(false).0,
            control: Mutex::new(None),
            control_upgrades: AtomicU32::new(0),
            client_upgrades: AtomicU32::new(0),
            next_id: AtomicU32::new(0),
            clients: Mutex::new(HashMap::new()),
            data_pipes: Mutex::new(HashMap::new()),
            data_dials: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/health", get(health))
            .route("/ws", get(ws_upgrade))
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("bind mock relay");
        let addr = listener.local_addr().expect("mock relay port");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock relay serve");
        });
        MockRelay { inner, addr, task }
    }

    /// Drop the listener and sever established pipes, not merely control.
    pub async fn stop(self) {
        self.inner.shutdown.send_replace(true);
        self.task.abort();
        let _ = self.task.await;
    }

    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    /// Number of control sockets the daemon has opened (reconnects included).
    pub fn control_upgrades(&self) -> u32 {
        self.inner.control_upgrades.load(Ordering::SeqCst)
    }

    /// Every server+connectionId dial as its (serverId, connectionId) pair.
    pub fn data_dials(&self) -> Vec<(String, String)> {
        self.inner.data_dials.lock().unwrap().clone()
    }

    /// Number of client-role sockets dialed (the client transport's redials).
    pub fn client_upgrades(&self) -> u32 {
        self.inner.client_upgrades.load(Ordering::SeqCst)
    }

    /// Live client-role connectionIds, ascending — the ids the remote daemon
    /// side dials as `role=server&connectionId=`.
    pub fn client_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.clients.lock().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Dial the remote-daemon side of a live client-role connection — the
    /// `role=server&connectionId=` socket the real daemon opens on
    /// `connected`. The two sockets are then spliced by the same code path
    /// the production pairs use.
    pub async fn connect_data(&self, connection_id: &str) -> MockClientSocket {
        let url = format!(
            "ws://{}/ws?serverId=mock&role=server&v=2&connectionId={connection_id}",
            self.addr
        );
        dial_socket(&url).await
    }

    /// `false`: accept control upgrades, then close them at once.
    pub fn set_gate(&self, open: bool) {
        *self.inner.gate_open.lock().unwrap() = open;
    }

    pub fn set_silent(&self, mode: Option<SilentMode>) {
        *self.inner.silent.lock().unwrap() = mode;
    }

    /// `true`: existing and new client-role sockets stop being read (no pong).
    pub fn set_client_park(&self, park: bool) {
        self.inner.client_park.send_replace(park);
    }

    /// Push a raw text frame onto the current control socket.
    pub fn send_control(&self, text: &str) {
        let guard = self.inner.control.lock().unwrap();
        if let Some(sender) = guard.as_ref() {
            let _ = sender.send(Message::Text(text.to_string().into()));
        }
    }

    /// Sever the current control socket (Close frame + deregister).
    pub fn drop_control(&self) {
        let mut guard = self.inner.control.lock().unwrap();
        if let Some(sender) = guard.take() {
            let _ = sender.send(Message::Close(None));
        }
    }

    /// Dial a client-role socket (the remote device side of the tests). The
    /// mock assigns its connectionId and reports `connected` on control; the
    /// test correlates via the daemon-side `open_session` callback.
    pub async fn connect_client(&self) -> MockClientSocket {
        let url = format!("ws://{}/ws?role=client&v=2", self.addr);
        dial_socket(&url).await
    }
}

/// Dial a mock-relay socket with the shared read cap: at-limit data frames
/// must reach the test's reads.
async fn dial_socket(url: &str) -> MockClientSocket {
    let cap = RELAY_DATA_FRAME_TEXT_BYTES.saturating_mul(2);
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_message_size: Some(cap),
        max_frame_size: Some(cap),
        ..tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
    };
    let request = url.into_client_request().expect("mock relay url");
    let (stream, _response) = tokio_tungstenite::connect_async_tls_with_config(
        request,
        Some(config),
        false,
        None::<tokio_tungstenite::Connector>,
    )
    .await
    .expect("mock relay dial");
    MockClientSocket { stream }
}

/// A client-role socket held by the test. Dropping the write side (close) is
/// what triggers the mock's `disconnected` notification.
pub struct MockClientSocket {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

#[allow(dead_code)] // shared support: per-target subset usage
impl MockClientSocket {
    pub async fn send_text(&mut self, text: &str) {
        self.stream
            .send(tokio_tungstenite::tungstenite::Message::Text(
                text.to_string(),
            ))
            .await
            .expect("client send");
    }

    /// Next text frame, failing the test on close/error instead.
    pub async fn recv_text(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match self.stream.next().await {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => return text,
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                        panic!("socket closed before text frame")
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => panic!("socket error before text frame: {e}"),
                    None => panic!("socket ended before text frame"),
                }
            }
        })
        .await
        .expect("text frame within timeout")
    }

    /// Next text frame within `duration`, or `None` when the window elapses
    /// (a late frame stays queued for the next read). Close/error still
    /// panic — this is only for "nothing arrived yet" assertions.
    pub async fn maybe_recv_text(&mut self, duration: Duration) -> Option<String> {
        tokio::time::timeout(duration, async {
            loop {
                match self.stream.next().await {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        return text.to_string()
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                        panic!("socket closed before text frame")
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => panic!("socket error before text frame: {e}"),
                    None => panic!("socket ended before text frame"),
                }
            }
        })
        .await
        .ok()
    }

    /// Next text frame within `duration`, or `None` when the window elapses
    /// **or the socket ends** — the "collect what arrives, then stop" shape
    /// a close-observing test needs (no panic on close).
    pub async fn next_text_or_close(&mut self, duration: Duration) -> Option<String> {
        tokio::time::timeout(duration, async {
            loop {
                match self.stream.next().await {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        return Some(text.to_string())
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(_)) | None => return None,
                }
            }
        })
        .await
        .unwrap_or(None)
    }

    /// Await the socket's end (Close frame, EOF, or error).
    pub async fn expect_close(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match self.stream.next().await {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))
                    | None
                    | Some(Err(_)) => return,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await
        .expect("close within timeout")
    }

    /// Close the socket; the mock reports `disconnected` for its id.
    pub async fn close(mut self) {
        let _ = futures_util::SinkExt::close(&mut self.stream).await;
    }
}

async fn health() -> impl IntoResponse {
    StatusCode::OK
}

async fn ws_upgrade(
    Query(query): Query<HashMap<String, String>>,
    State(inner): State<Arc<Inner>>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    // The daemon's data-side length check has to see at-limit frames, so the
    // mock's read limits sit above the wire cap (the daemon rejects what it
    // rejects; the mock never should).
    let ws = ws
        .max_message_size(RELAY_DATA_FRAME_TEXT_BYTES.saturating_mul(2))
        .max_frame_size(RELAY_DATA_FRAME_TEXT_BYTES.saturating_mul(2));
    let has_connection_id = query.contains_key("connectionId");
    match query.get("role").map(String::as_str) {
        Some("server") if !has_connection_id => {
            Ok(ws.on_upgrade(move |socket| control_socket(inner, socket)))
        }
        Some("server") => {
            let server_id = query.get("serverId").cloned().unwrap_or_default();
            let connection_id = query.get("connectionId").cloned().unwrap_or_default();
            inner
                .data_dials
                .lock()
                .unwrap()
                .push((server_id, connection_id.clone()));
            Ok(ws.on_upgrade(move |socket| data_socket(inner, connection_id, socket)))
        }
        Some("client") => Ok(ws.on_upgrade(move |socket| client_socket(inner, socket))),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

async fn control_socket(inner: Arc<Inner>, socket: WebSocket) {
    let mut stopped = inner.shutdown.subscribe();
    inner.control_upgrades.fetch_add(1, Ordering::SeqCst);
    if !*inner.gate_open.lock().unwrap() {
        // Close at once: dial success + immediate drop is the reconnect-cycle
        // shape under test (waiting for the daemon's ready timeout would make
        // the backoff-sequence test crawl).
        let (mut sink, _stream) = socket.split();
        let _ = sink.send(Message::Close(None)).await;
        return;
    }
    let silent = *inner.silent.lock().unwrap();
    let (mut sink, stream) = socket.split();
    if silent != Some(SilentMode::BeforeReady) {
        let sync = "{\"type\":\"sync\",\"connectionIds\":[]}";
        if sink
            .send(Message::Text(sync.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    if silent.is_some() {
        tokio::select! {
            _ = park((sink, stream)) => {},
            _ = stopped.wait_for(|stop| *stop) => {},
        }
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    *inner.control.lock().unwrap() = Some(tx);
    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });
    // Pump reads: axum answers the daemon's WS pings while the socket is polled.
    let mut read = stream;
    while let Some(Ok(_)) = tokio::select! {
        _ = stopped.wait_for(|stop| *stop) => None,
        frame = read.next() => frame,
    } {}
    writer.abort();
    inner.control.lock().unwrap().take();
}

async fn client_socket(inner: Arc<Inner>, socket: WebSocket) {
    let mut stopped = inner.shutdown.subscribe();
    let mut parked = inner.client_park.subscribe();
    inner.client_upgrades.fetch_add(1, Ordering::SeqCst);
    let id = format!("c-{}", inner.next_id.fetch_add(1, Ordering::SeqCst) + 1);
    let (tx, rx) = mpsc::channel::<Message>(PIPE_CAPACITY);
    inner.clients.lock().unwrap().insert(id.clone(), tx);
    notify_control(
        &inner,
        &format!("{{\"type\":\"connected\",\"connectionId\":\"{id}\"}}"),
    );
    let (sink, mut stream) = socket.split();
    let writer = tokio::spawn(pump(rx, sink));
    // client → daemon: forward through the data pipe (bounded, so a slow
    // daemon-side writer backpressures the client socket).
    loop {
        // Stop polling before tungstenite can queue an automatic pong.
        let message = tokio::select! {
            _ = stopped.wait_for(|stop| *stop) => break,
            _ = parked.changed() => continue,
            message = stream.next(), if !*parked.borrow() => message,
        };
        let Some(Ok(message)) = message else { break };
        let pipe = inner.data_pipes.lock().unwrap().get(&id).cloned();
        if let Some(pipe) = pipe {
            let _ = pipe.send(message).await;
        }
    }
    // Client gone: sever the daemon side and report disconnected.
    if let Some(pipe) = inner.data_pipes.lock().unwrap().remove(&id) {
        let _ = pipe.try_send(Message::Close(None));
    }
    inner.clients.lock().unwrap().remove(&id);
    writer.abort();
    notify_control(
        &inner,
        &format!("{{\"type\":\"disconnected\",\"connectionId\":\"{id}\"}}"),
    );
}

async fn data_socket(inner: Arc<Inner>, connection_id: String, socket: WebSocket) {
    let mut stopped = inner.shutdown.subscribe();
    let client = inner.clients.lock().unwrap().get(&connection_id).cloned();
    let Some(client) = client else {
        // The client vanished between `connected` and this dial.
        let (mut sink, _stream) = socket.split();
        let _ = sink.send(Message::Close(None)).await;
        return;
    };
    let (tx, rx) = mpsc::channel::<Message>(PIPE_CAPACITY);
    inner
        .data_pipes
        .lock()
        .unwrap()
        .insert(connection_id.clone(), tx);
    let (sink, mut stream) = socket.split();
    let writer = tokio::spawn(pump(rx, sink));
    // daemon → client: a stalled client fills the bounded channel, this loop
    // stops reading the daemon's socket, and the daemon's own outbound queue
    // fills — the slow-consumer eviction path.
    // shortcut: a loop blocked on a stalled peer cannot observe the daemon
    // closing the socket; eviction tests assert daemon-side close, not mock
    // cleanup (the stuck task dies with the test runtime).
    while let Some(Ok(message)) = tokio::select! {
        _ = stopped.wait_for(|stop| *stop) => None,
        frame = stream.next() => frame,
    } {
        if client.send(message).await.is_err() {
            break;
        }
    }
    // Daemon side gone: nudge the client closed and report disconnected.
    let _ = client.try_send(Message::Close(None));
    inner.data_pipes.lock().unwrap().remove(&connection_id);
    writer.abort();
    notify_control(
        &inner,
        &format!("{{\"type\":\"disconnected\",\"connectionId\":\"{connection_id}\"}}"),
    );
}

async fn pump(mut rx: mpsc::Receiver<Message>, mut sink: SplitSink<WebSocket, Message>) {
    while let Some(message) = rx.recv().await {
        if sink.send(message).await.is_err() {
            break;
        }
    }
}

fn notify_control(inner: &Arc<Inner>, text: &str) {
    let guard = inner.control.lock().unwrap();
    if let Some(sender) = guard.as_ref() {
        let _ = sender.send(Message::Text(text.to_string().into()));
    }
}

/// Hold a socket (or its halves) open without ever polling it: no reads means
/// no WS pong, which is exactly the silence the stale/ready-timeout tests need.
async fn park<S>(_socket: S) -> ! {
    futures_util::future::pending::<()>().await;
    unreachable!("pending never resolves")
}
