// ABOUTME: Mock relay server for protocol v2 contract tests (plan T4).
// ABOUTME: Minimal face: /ws role routing (control + client), GET /health; the
// ABOUTME: server+connectionId data pipe lands with T5's data sockets.

//! A real axum server standing in for the company relay, driven from the
//! integration tests (`tests/relay_control_transport.rs`). Control frames are
//! the literal JSON shapes from the plan's protocol tables — never generated
//! by round-tripping the daemon's types (engineering lesson #1).
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

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

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

    /// Dial a client-role socket (the remote device side of the tests).
    pub async fn connect_client(&self) -> MockClientSocket {
        let url = format!("ws://{}/ws?role=client&v=2", self.addr);
        let (stream, _response) = tokio_tungstenite::connect_async(url)
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
    let has_connection_id = query.contains_key("connectionId");
    match query.get("role").map(String::as_str) {
        Some("server") if !has_connection_id => {
            Ok(ws.on_upgrade(move |socket| control_socket(inner, socket)))
        }
        Some("server") => {
            // server+connectionId is the data pipe; nothing dials it until T5.
            // Park it so an accidental dial cannot masquerade as a control.
            Ok(ws.on_upgrade(|socket| async move {
                park(socket).await;
            }))
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
    notify_control(
        &inner,
        &format!("{{\"type\":\"connected\",\"connectionId\":\"{id}\"}}"),
    );
    let (_sink, mut stream) = socket.split();
    // Frame relay (the T5 data pipe) is not exercised in this slice; holding
    // the socket open is all the connected/disconnected lifecycle needs.
    while let Some(Ok(_)) = stream.next().await {}
    notify_control(
        &inner,
        &format!("{{\"type\":\"disconnected\",\"connectionId\":\"{id}\"}}"),
    );
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
