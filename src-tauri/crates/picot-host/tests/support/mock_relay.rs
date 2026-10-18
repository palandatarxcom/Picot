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
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use picot_host::transport_limits::RELAY_DATA_FRAME_TEXT_BYTES;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Forwarding channels are bounded so a stalled peer backpressures the source
/// socket instead of growing mock memory without end.
const PIPE_CAPACITY: usize = 8;

/// How a control socket stays quiet, per test.
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
}

struct Inner {
    gate_open: Mutex<bool>,
    silent: Mutex<Option<SilentMode>>,
    /// Writer half of the current interactive control socket.
    control: Mutex<Option<mpsc::UnboundedSender<Message>>>,
    control_upgrades: AtomicU32,
    next_id: AtomicU32,
    /// connectionId → writer channel of the client-role socket.
    clients: Mutex<HashMap<String, mpsc::Sender<Message>>>,
    /// connectionId → writer channel of the daemon's data socket.
    data_pipes: Mutex<HashMap<String, mpsc::Sender<Message>>>,
    /// (serverId, connectionId) of every server+connectionId dial.
    data_dials: Mutex<Vec<(String, String)>>,
}

impl MockRelay {
    /// Bind an ephemeral loopback port and serve the mock relay.
    pub async fn start() -> MockRelay {
        let inner = Arc::new(Inner {
            gate_open: Mutex::new(true),
            silent: Mutex::new(None),
            control: Mutex::new(None),
            control_upgrades: AtomicU32::new(0),
            next_id: AtomicU32::new(0),
            clients: Mutex::new(HashMap::new()),
            data_pipes: Mutex::new(HashMap::new()),
            data_dials: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/health", get(health))
            .route("/ws", get(ws_upgrade))
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind mock relay");
        let addr = listener.local_addr().expect("mock relay port");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock relay serve");
        });
        MockRelay { inner, addr }
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

    /// `false`: accept control upgrades, then close them at once.
    pub fn set_gate(&self, open: bool) {
        *self.inner.gate_open.lock().unwrap() = open;
    }

    pub fn set_silent(&self, mode: Option<SilentMode>) {
        *self.inner.silent.lock().unwrap() = mode;
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
        // Same read cap as the server side: at-limit data frames must reach
        // the test client's reads.
        let cap = RELAY_DATA_FRAME_TEXT_BYTES.saturating_mul(2);
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
            max_message_size: Some(cap),
            max_frame_size: Some(cap),
            ..tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        };
        let request = url.into_client_request().expect("client url");
        let (stream, _response) = tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(config),
            false,
            None::<tokio_tungstenite::Connector>,
        )
        .await
        .expect("mock relay client dial");
        MockClientSocket { stream }
    }
}

/// A client-role socket held by the test. Dropping the write side (close) is
/// what triggers the mock's `disconnected` notification.
pub struct MockClientSocket {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

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
        park((sink, stream)).await;
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
    while let Some(Ok(_)) = read.next().await {}
    writer.abort();
    inner.control.lock().unwrap().take();
}

async fn client_socket(inner: Arc<Inner>, socket: WebSocket) {
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
    while let Some(Ok(message)) = stream.next().await {
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
    while let Some(Ok(message)) = stream.next().await {
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
