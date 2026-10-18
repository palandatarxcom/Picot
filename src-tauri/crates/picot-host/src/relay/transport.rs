// ABOUTME: Relay transport: control-socket state machine (spec §2.5) plus
// ABOUTME: per-client data sockets with bounded-queue eviction (spec §2.7).

//! The daemon holds exactly one outbound control WebSocket to the relay:
//! `ws(s)://<endpoint>/ws?serverId=<id>&role=server&v=2`. The relay pushes
//! `sync` (full connectionId list) on connect plus `connected`/`disconnected`
//! deltas; the transport reconciles them against its live set and reports
//! additions/removals to the [`ConnectionLifecycle`] driver, which dials one
//! data socket per connectionId (`…&connectionId=<id>`, 15s open timeout),
//! terminates its E2EE handshake (`e2ee_hello` → shared key → `e2ee_ready`,
//! ≤4 concurrent, 10s budget), and hands the write path to the
//! [`RelaySessionSink`] via [`EncryptedChannelHandle`] — every frame in both
//! directions is base64 NaCl-box ciphertext from there on. Data sockets
//! survive control reconnects; `sync` reconcile closes the ones the relay
//! no longer lists.
//!
//! Parameters are Paseo's, copied verbatim (spec §2.5 / plan T4):
//! reconnect backoff `min(30s, 1s × attempt)`, WS ping every 10s, 30s stale
//! force-close, 8s control-ready timeout, 10s WS handshake timeout. Every
//! timer runs through [`TransportSeams`] so tests observe the exact backoff
//! sequence on a recording sleep instead of wall-clock luck. The handshake
//! request never negotiates `permessage-deflate` (tungstenite sends no
//! extension offer by default).
//!
//! Capacity bounds (spec §2.7): inbound control frames > `RELAY_CONTROL_FRAME_BYTES`
//! and `sync.connectionIds` longer than `MAX_CLIENTS` are protocol violations
//! that drop the control connection (reconnect per backoff). Outbound frames
//! go through a bounded queue of `OUTBOUND_QUEUE_CAPACITY`; a full queue
//! closes the offending connection — for the control socket that means drop
//! and reconnect, for a data socket eviction of that connection only. The
//! same caps ride the WebSocket read config (max message and frame size) so
//! oversized input dies at the protocol layer, with the explicit checks as
//! the documented second layer — including the post-decrypt plaintext cap,
//! which the wire cap alone cannot express at the b64 quartet boundary.

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch, Semaphore};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::host_router::MAX_CLIENTS;
use crate::relay::e2ee::{self, DaemonIdentity};
use crate::transport_limits::{
    E2EE_HANDSHAKE_PERMITS, OUTBOUND_QUEUE_CAPACITY, RELAY_CONTROL_FRAME_BYTES,
    RELAY_DATA_FRAME_TEXT_BYTES, WS_PHYSICAL_FRAME_BYTES,
};

/// Reconnect backoff: `min(30s, 1s × attempt)` (relay-transport.ts:338).
pub const CONTROL_BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Backoff ceiling: 30s.
pub const CONTROL_BACKOFF_CAP: Duration = Duration::from_secs(30);
/// WS ping interval on the control socket: 10s.
pub const CONTROL_PING_INTERVAL: Duration = Duration::from_secs(10);
/// No inbound frame for this long → force-close the control socket: 30s.
pub const CONTROL_STALE_TIMEOUT: Duration = Duration::from_secs(30);
/// First relay frame must arrive within this window: 8s.
pub const CONTROL_READY_TIMEOUT: Duration = Duration::from_secs(8);
/// WS handshake (dial → upgrade) deadline: 10s.
pub const CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-client data-socket open timeout: 15s (relay-transport.ts:361).
/// Per-client data-socket open timeout: 15s (relay-transport.ts:361).
pub const DATA_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// E2EE handshake budget per data socket, semaphore wait included
/// (spec §2.5/§2.7): hello → shared key → ready inside this window or the
/// connection closes.
pub const E2EE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// `min(cap, base × attempt)` — linear, per the copied parameter.
pub fn control_backoff(attempt: u32, base: Duration, cap: Duration) -> Duration {
    cap.min(base.saturating_mul(attempt.max(1)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Online,
    Offline,
}

/// Where the daemon dials and which serverId it announces.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// `host:port` of the relay endpoint (no scheme, no path).
    pub endpoint: String,
    /// `true` → `wss://` with the native-tls connector; `false` → `ws://`.
    pub use_tls: bool,
    /// The daemon identity's serverId (T6 wires them together).
    pub server_id: String,
}

/// Write path for one relay data connection after its E2EE handshake,
/// handed to the sink on `open_session`: plaintext in, base64 NaCl-box
/// ciphertext out on the same bounded socket queue.
#[derive(Clone)]
pub struct EncryptedChannelHandle {
    connection_id: String,
    shared: Arc<[u8; 32]>,
    out: mpsc::Sender<Message>,
    close: watch::Sender<bool>,
}

impl EncryptedChannelHandle {
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    /// Queue one outbound text frame on the bounded queue
    /// (`OUTBOUND_QUEUE_CAPACITY`). `false`: the queue is full or the writer
    /// is gone — the connection is closing and the frame is dropped
    /// (spec §2.7: a full queue closes that data connection only).
    pub fn send(&self, plaintext: String) -> bool {
        let wire = BASE64_STANDARD.encode(e2ee::seal(&self.shared, plaintext.as_bytes()));
        match self.out.try_send(Message::Text(wire)) {
            Ok(()) => true,
            Err(_) => {
                self.close();
                false
            }
        }
    }

    /// Close this data connection (idempotent).
    pub fn close(&self) {
        let _ = self.close.send(true);
    }
}

/// Consumer-facing surface of the transport.
#[async_trait]
pub trait RelaySessionSink: Send + Sync {
    async fn on_connection_state(&self, state: ConnectionState);
    /// A relay connectionId is live and its data socket is open.
    async fn open_session(&self, connection_id: String, channel: EncryptedChannelHandle);
    /// One decrypted plaintext frame off that connection's E2EE channel.
    /// Handshake-gated: nothing arrives before `e2ee_ready`.
    async fn data_frame(&self, connection_id: &str, payload: String);
    /// The connection went away (`disconnected` delta, `sync` reconcile
    /// removal, socket death, or bounded-queue eviction). May fire more than
    /// once per id (relay report racing socket death); sinks must be
    /// idempotent.
    async fn close_session(&self, connection_id: &str);
}

/// Control-side driver for per-connection resources: the control loop calls
/// open/close as the relay's sync and deltas move connectionIds. The
/// production implementation dials and tears down data sockets; unit tests
/// record the calls.
#[async_trait]
trait ConnectionLifecycle: Send + Sync {
    async fn open(&self, connection_id: String);
    async fn close(&self, connection_id: &str);
}

/// Injectable timers (defaults are the spec §2.5 values; tests shrink them).
#[derive(Debug, Clone, Copy)]
pub struct TransportTimings {
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
    pub ping_interval: Duration,
    pub stale_timeout: Duration,
    pub ready_timeout: Duration,
    pub handshake_timeout: Duration,
    pub data_open_timeout: Duration,
    pub e2ee_handshake_timeout: Duration,
}

impl Default for TransportTimings {
    fn default() -> Self {
        Self {
            backoff_base: CONTROL_BACKOFF_BASE,
            backoff_cap: CONTROL_BACKOFF_CAP,
            ping_interval: CONTROL_PING_INTERVAL,
            stale_timeout: CONTROL_STALE_TIMEOUT,
            ready_timeout: CONTROL_READY_TIMEOUT,
            handshake_timeout: CONTROL_HANDSHAKE_TIMEOUT,
            data_open_timeout: DATA_OPEN_TIMEOUT,
            e2ee_handshake_timeout: E2EE_HANDSHAKE_TIMEOUT,
        }
    }
}

/// Sleep seam: production sleeps for real; tests record the requested
/// duration and resolve at once (fake clock for the backoff sequence).
pub type SleepFn =
    Arc<dyn Fn(Duration) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// Test/production injection point for timers, sleep, and the TLS connector.
/// Production uses [`Default`]; the connector is `None` (native-tls defaults:
/// system roots, chain + SNI validation). Tests inject a fixture-CA-only
/// connector for the TLS contract cases (spec §2.5).
#[derive(Clone)]
pub struct TransportSeams {
    pub timings: TransportTimings,
    pub sleep: SleepFn,
    pub connector: Option<Connector>,
}

impl Default for TransportSeams {
    fn default() -> Self {
        Self {
            timings: TransportTimings::default(),
            sleep: Arc::new(|duration| Box::pin(tokio::time::sleep(duration))),
            connector: None,
        }
    }
}

/// Owns the outbound control connection family. Dropping the handle without
/// `stop` leaves the task running; `stop` is the local-only shutdown.
pub struct RelayTransportHandle {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl RelayTransportHandle {
    /// Stop the transport: closes the control socket locally, never waits on
    /// the network. Idempotent in effect — the second call finds the task done.
    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
    }
}

pub struct RelayTransport;

impl RelayTransport {
    /// Start the control transport with production parameters.
    pub fn start(
        config: RelayConfig,
        identity: DaemonIdentity,
        sink: Arc<dyn RelaySessionSink>,
    ) -> RelayTransportHandle {
        Self::start_with_seams(config, identity, sink, TransportSeams::default())
    }

    /// Start with injected seams — the entry the mock-relay contract tests use.
    pub fn start_with_seams(
        config: RelayConfig,
        identity: DaemonIdentity,
        sink: Arc<dyn RelaySessionSink>,
        seams: TransportSeams,
    ) -> RelayTransportHandle {
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_control_loop(config, identity, sink, seams, receiver));
        RelayTransportHandle { shutdown, task }
    }
}

async fn run_control_loop(
    config: RelayConfig,
    identity: DaemonIdentity,
    sink: Arc<dyn RelaySessionSink>,
    seams: TransportSeams,
    mut shutdown: watch::Receiver<bool>,
) {
    let timings = seams.timings;
    let mut live: HashSet<String> = HashSet::new();
    // Data sockets outlive any single control cycle (reconnects reconcile
    // drift via `sync`), so their registry sits outside the cycle loop.
    let manager = DataSocketManager {
        config: config.clone(),
        seams: seams.clone(),
        identity,
        sink: Arc::clone(&sink),
        registry: Arc::new(DataConnections::default()),
        handshake_permits: Arc::new(Semaphore::new(E2EE_HANDSHAKE_PERMITS)),
        shutdown: shutdown.clone(),
    };
    let mut failed_attempts: u32 = 0;
    while !*shutdown.borrow() {
        sink.on_connection_state(ConnectionState::Connecting).await;
        let outcome =
            run_control_cycle(&config, &sink, &seams, &mut shutdown, &mut live, &manager).await;
        match outcome {
            CycleOutcome::Stopped => return,
            CycleOutcome::Dropped { reached_online } => {
                // A connection that reached Online resets the attempt
                // counter: the next failure starts over at base × 1.
                if reached_online {
                    failed_attempts = 0;
                }
                if failed_attempts == u32::MAX {
                    // The counter saturates; backoff is already at cap anyway.
                    continue;
                }
                failed_attempts += 1;
                sink.on_connection_state(ConnectionState::Offline).await;
                let backoff =
                    control_backoff(failed_attempts, timings.backoff_base, timings.backoff_cap);
                tokio::select! {
                    _ = (seams.sleep)(backoff) => {}
                    _ = shutdown.changed() => return,
                }
            }
        }
    }
}

enum CycleOutcome {
    /// Shutdown observed — exit without reporting Offline.
    Stopped,
    /// Connection dropped (error, close, stale, violation) — report Offline
    /// and reconnect per backoff.
    Dropped { reached_online: bool },
}

async fn run_control_cycle(
    config: &RelayConfig,
    sink: &Arc<dyn RelaySessionSink>,
    seams: &TransportSeams,
    shutdown: &mut watch::Receiver<bool>,
    live: &mut HashSet<String>,
    manager: &DataSocketManager,
) -> CycleOutcome {
    // Dial under the shutdown flag: stop() must not wait out a handshake.
    let dial = dial_control(config, seams);
    let stream = tokio::select! {
        _ = shutdown.changed() => return CycleOutcome::Stopped,
        stream = dial => match stream {
            Ok(stream) => stream,
            Err(_) => return CycleOutcome::Dropped { reached_online: false },
        },
    };

    let (sink_half, stream_half) = stream.split();
    // Bounded outbound queue: overflow closes this connection (spec §2.7).
    let (out_tx, out_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
    let writer = tokio::spawn(bounded_writer(sink_half, out_rx));

    let timings = seams.timings;
    let mut last_seen = tokio::time::Instant::now();
    let ready_deadline = last_seen + timings.ready_timeout;
    let mut online_reported = false;
    let mut ping = tokio::time::interval_at(
        tokio::time::Instant::now() + timings.ping_interval,
        timings.ping_interval,
    );
    let mut inbound = stream_half;

    let outcome = loop {
        tokio::select! {
            _ = shutdown.changed() => break CycleOutcome::Stopped,
            _ = ping.tick() => {
                if last_seen + timings.stale_timeout <= tokio::time::Instant::now() {
                    // Stale: no inbound frame for the whole window → force-close.
                    break CycleOutcome::Dropped { reached_online: online_reported };
                }
                if !try_enqueue(&out_tx, Message::Ping(Vec::new())) {
                    // Queue full: close the offending connection (control
                    // overflow = drop + reconnect, spec §2.7).
                    break CycleOutcome::Dropped { reached_online: online_reported };
                }
            }
            _ = tokio::time::sleep_until(ready_deadline), if !online_reported => {
                // No first frame within the ready window → drop and reconnect.
                break CycleOutcome::Dropped { reached_online: online_reported };
            }
            frame = inbound.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    last_seen = tokio::time::Instant::now();
                    match handle_control_frame(&text, live, manager).await {
                        Ok(()) => {
                            if !online_reported {
                                online_reported = true;
                                sink.on_connection_state(ConnectionState::Online).await;
                            }
                        }
                        Err(()) => break CycleOutcome::Dropped { reached_online: online_reported },
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    break CycleOutcome::Dropped { reached_online: online_reported }
                }
                // Any inbound frame (ping/pong) refreshes liveness.
                Some(Ok(_)) => last_seen = tokio::time::Instant::now(),
                Some(Err(_)) => break CycleOutcome::Dropped { reached_online: online_reported },
            },
        }
    };
    // Local-only teardown: aborting the writer drops the socket halves.
    writer.abort();
    outcome
}

async fn bounded_writer(
    mut sink_half: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    mut out_rx: mpsc::Receiver<Message>,
) {
    while let Some(message) = out_rx.recv().await {
        if sink_half.send(message).await.is_err() {
            break;
        }
    }
}

/// Enqueue on the bounded outbound queue; `false` means full (or writer gone)
/// — the caller closes the connection.
fn try_enqueue(out_tx: &mpsc::Sender<Message>, message: Message) -> bool {
    out_tx.try_send(message).is_ok()
}

type ControlStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// `ws(s)://<endpoint>/ws?serverId=&role=server&v=2` (the control socket).
fn control_url(config: &RelayConfig) -> String {
    let scheme = if config.use_tls { "wss" } else { "ws" };
    let server_id = utf8_percent_encode(&config.server_id, NON_ALPHANUMERIC);
    format!(
        "{scheme}://{}/ws?serverId={server_id}&role=server&v=2",
        config.endpoint
    )
}

/// The per-connection data-socket dial:
/// `ws(s)://<endpoint>/ws?serverId=&role=server&v=2&connectionId=`.
fn data_url(config: &RelayConfig, connection_id: &str) -> String {
    let scheme = if config.use_tls { "wss" } else { "ws" };
    let server_id = utf8_percent_encode(&config.server_id, NON_ALPHANUMERIC);
    let connection_id = utf8_percent_encode(connection_id, NON_ALPHANUMERIC);
    format!(
        "{scheme}://{}/ws?serverId={server_id}&role=server&v=2&connectionId={connection_id}",
        config.endpoint
    )
}

/// Dial one relay WebSocket under an open timeout and a read cap: oversized
/// inbound input dies at the protocol layer (the explicit per-kind checks
/// are the second, documented layer).
async fn dial_ws(
    url: String,
    open_timeout: Duration,
    max_message_bytes: usize,
    connector: Option<Connector>,
) -> Result<ControlStream, ()> {
    let request = url.into_client_request().map_err(|_| ())?;
    let config = WebSocketConfig {
        max_message_size: Some(max_message_bytes),
        max_frame_size: Some(max_message_bytes),
        ..WebSocketConfig::default()
    };
    let dial =
        tokio_tungstenite::connect_async_tls_with_config(request, Some(config), false, connector);
    match tokio::time::timeout(open_timeout, dial).await {
        Ok(Ok((stream, _response))) => Ok(stream),
        _ => Err(()),
    }
}

async fn dial_control(config: &RelayConfig, seams: &TransportSeams) -> Result<ControlStream, ()> {
    dial_ws(
        control_url(config),
        seams.timings.handshake_timeout,
        RELAY_CONTROL_FRAME_BYTES,
        seams.connector.clone(),
    )
    .await
}

async fn dial_data(
    config: &RelayConfig,
    seams: &TransportSeams,
    connection_id: &str,
) -> Result<ControlStream, ()> {
    dial_ws(
        data_url(config, connection_id),
        seams.timings.data_open_timeout,
        RELAY_DATA_FRAME_TEXT_BYTES,
        seams.connector.clone(),
    )
    .await
}

/// Handle one relay→daemon control frame. `Err(())` is a protocol violation:
/// the control connection is dropped and reconnected per backoff.
async fn handle_control_frame(
    text: &str,
    live: &mut HashSet<String>,
    driver: &dyn ConnectionLifecycle,
) -> Result<(), ()> {
    if text.len() > RELAY_CONTROL_FRAME_BYTES {
        return Err(());
    }
    let value: Value = serde_json::from_str(text).map_err(|_| ())?;
    match value.get("type").and_then(Value::as_str) {
        Some("sync") => {
            let ids = value
                .get("connectionIds")
                .and_then(Value::as_array)
                .ok_or(())?;
            if ids.len() > MAX_CLIENTS {
                // A sync longer than the client ceiling is a protocol anomaly.
                return Err(());
            }
            let mut desired: HashSet<String> = HashSet::new();
            let mut declared_order: Vec<String> = Vec::with_capacity(ids.len());
            for id in ids {
                let id = id.as_str().ok_or(())?.to_string();
                if desired.insert(id.clone()) {
                    declared_order.push(id);
                }
            }
            // Opens follow the relay-declared order; removals sort for
            // deterministic reporting.
            for id in declared_order {
                if !live.contains(&id) {
                    driver.open(id.clone()).await;
                }
            }
            let mut removed: Vec<String> = live.difference(&desired).cloned().collect();
            removed.sort();
            for id in removed {
                driver.close(&id).await;
            }
            *live = desired;
            Ok(())
        }
        Some("connected") => {
            let id = value
                .get("connectionId")
                .and_then(Value::as_str)
                .ok_or(())?;
            if live.insert(id.to_string()) {
                driver.open(id.to_string()).await;
            }
            Ok(())
        }
        Some("disconnected") => {
            let id = value
                .get("connectionId")
                .and_then(Value::as_str)
                .ok_or(())?;
            if live.remove(id) {
                driver.close(id).await;
            }
            Ok(())
        }
        _ => Err(()),
    }
}

/// Live data-connection registry: connectionId → its close switch. Control
/// removals take the entry out (the task then stays silent — control already
/// delivered `close_session`); self-initiated teardowns (eviction, frame
/// violations, peer death) remove their own entry and notify.
#[derive(Default)]
struct DataConnections {
    entries: std::sync::Mutex<HashMap<String, watch::Sender<bool>>>,
}

impl DataConnections {
    fn insert(&self, connection_id: String, close: watch::Sender<bool>) {
        self.entries.lock().unwrap().insert(connection_id, close);
    }

    /// Signal the connection task to close and drop the registry entry.
    fn close(&self, connection_id: &str) {
        if let Some(close) = self.entries.lock().unwrap().remove(connection_id) {
            let _ = close.send(true);
        }
    }

    /// Remove only if this entry still owns the id (no replacement happened):
    /// tells the caller whether it owes the sink a `close_session`.
    fn remove_if_owner(&self, connection_id: &str, close: &watch::Sender<bool>) -> bool {
        let mut entries = self.entries.lock().unwrap();
        if entries
            .get(connection_id)
            .is_some_and(|current| current.same_channel(close))
        {
            entries.remove(connection_id);
            true
        } else {
            false
        }
    }
}

/// Production [`ConnectionLifecycle`]: one data socket per connectionId.
struct DataSocketManager {
    config: RelayConfig,
    seams: TransportSeams,
    identity: DaemonIdentity,
    sink: Arc<dyn RelaySessionSink>,
    registry: Arc<DataConnections>,
    handshake_permits: Arc<Semaphore>,
    shutdown: watch::Receiver<bool>,
}

#[async_trait]
impl ConnectionLifecycle for DataSocketManager {
    async fn open(&self, connection_id: String) {
        let (close_tx, _close_rx) = watch::channel(false);
        // The entry exists from dial start so a same-id close can cancel an
        // in-flight dial.
        self.registry
            .insert(connection_id.clone(), close_tx.clone());
        tokio::spawn(run_data_connection(DataConnection {
            config: self.config.clone(),
            seams: self.seams.clone(),
            identity: self.identity.clone(),
            sink: Arc::clone(&self.sink),
            registry: Arc::clone(&self.registry),
            handshake_permits: Arc::clone(&self.handshake_permits),
            connection_id,
            close: close_tx,
            shutdown: self.shutdown.clone(),
        }));
    }

    async fn close(&self, connection_id: &str) {
        // Registry entry first: the task's teardown sees a missing entry and
        // stays silent (this call owns the sink notification).
        self.registry.close(connection_id);
        self.sink.close_session(connection_id).await;
    }
}

struct DataConnection {
    config: RelayConfig,
    seams: TransportSeams,
    identity: DaemonIdentity,
    sink: Arc<dyn RelaySessionSink>,
    registry: Arc<DataConnections>,
    handshake_permits: Arc<Semaphore>,
    connection_id: String,
    close: watch::Sender<bool>,
    shutdown: watch::Receiver<bool>,
}

/// One per-client data socket: dial, hand the write path to the sink, pump
/// inbound frames, enforce the data-side bounds, tear down locally.
async fn run_data_connection(conn: DataConnection) {
    let mut close_rx = conn.close.subscribe();
    let mut shutdown = conn.shutdown.clone();
    if *shutdown.borrow() || *close_rx.borrow() {
        conn.registry
            .remove_if_owner(&conn.connection_id, &conn.close);
        return;
    }
    let dial = dial_data(&conn.config, &conn.seams, &conn.connection_id);
    let mut stream = tokio::select! {
        _ = shutdown.changed() => {
            conn.registry.remove_if_owner(&conn.connection_id, &conn.close);
            return;
        }
        _ = close_rx.changed() => {
            conn.registry.remove_if_owner(&conn.connection_id, &conn.close);
            return;
        }
        stream = dial => match stream {
            Ok(stream) => stream,
            Err(()) => {
                // Dial failure: nothing opened, nothing to report. The relay
                // still lists the client; its own `disconnected` heals state.
                conn.registry.remove_if_owner(&conn.connection_id, &conn.close);
                return;
            }
        },
    };
    // Cancellation may have landed while the dial resolved.
    if *shutdown.borrow() || *close_rx.borrow() {
        conn.registry
            .remove_if_owner(&conn.connection_id, &conn.close);
        return;
    }

    // E2EE handshake gates the channel (spec §2.4/§2.5): no frame reaches
    // the sink before e2ee_ready, any pre-ready frame closes the socket,
    // and the whole exchange (semaphore wait included) fits the handshake
    // budget. A failed or stalled handshake never opens a session, so its
    // teardown stays silent (the relay's `disconnected` heals the state).
    let secret = match conn.identity.secret_key_bytes() {
        Ok(secret) => secret,
        Err(_) => {
            conn.registry
                .remove_if_owner(&conn.connection_id, &conn.close);
            return;
        }
    };
    let handshake = e2ee_server_handshake(&mut stream, &secret, &conn.handshake_permits);
    let shared_key = tokio::select! {
        _ = shutdown.changed() => {
            conn.registry.remove_if_owner(&conn.connection_id, &conn.close);
            return;
        }
        _ = close_rx.changed() => {
            conn.registry.remove_if_owner(&conn.connection_id, &conn.close);
            return;
        }
        outcome = tokio::time::timeout(
            conn.seams.timings.e2ee_handshake_timeout,
            handshake,
        ) => match outcome {
            Ok(Some(shared_key)) => shared_key,
            Ok(None) | Err(_) => {
                conn.registry
                    .remove_if_owner(&conn.connection_id, &conn.close);
                return;
            }
        },
    };

    let shared_key = Arc::new(shared_key);
    let (socket_sink, socket_stream) = stream.split();
    let (out_tx, out_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
    let writer = tokio::spawn(bounded_writer(socket_sink, out_rx));
    let handle = EncryptedChannelHandle {
        connection_id: conn.connection_id.clone(),
        shared: Arc::clone(&shared_key),
        out: out_tx,
        close: conn.close.clone(),
    };
    conn.sink
        .open_session(conn.connection_id.clone(), handle)
        .await;

    let mut inbound = socket_stream;
    let mut notify_close = false;
    loop {
        tokio::select! {
            _ = shutdown.changed() => break, // stopping: no close_session
            _ = close_rx.changed() => {
                notify_close = true;
                break;
            }
            frame = inbound.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    // Spec §2.7: ciphertext text beyond the wire cap, or
                    // (post-decrypt) plaintext beyond the physical frame
                    // cap — both close this connection only. Bad base64
                    // and failed opens are the same class of violation.
                    if text.len() > RELAY_DATA_FRAME_TEXT_BYTES {
                        notify_close = true;
                        break;
                    }
                    let Some(plaintext) = decrypt_frame(&shared_key, &text) else {
                        notify_close = true;
                        break;
                    };
                    conn.sink.data_frame(&conn.connection_id, plaintext).await;
                }
                // The data protocol is text-only; binary is a violation.
                Some(Ok(Message::Binary(_))) => {
                    notify_close = true;
                    break;
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    notify_close = true;
                    break;
                }
                // ping/pong: connection liveness only.
                Some(Ok(_)) => {}
            },
        }
    }
    writer.abort();
    // Self-initiated teardown notifies; a control-directed close already did
    // (its registry entry is gone, so ownership check fails).
    if notify_close
        && conn
            .registry
            .remove_if_owner(&conn.connection_id, &conn.close)
    {
        conn.sink.close_session(&conn.connection_id).await;
    }
}

/// The daemon side of the E2EE handshake (spec §2.4/§2.5): wait for the
/// client's `e2ee_hello`, derive the per-connection shared key, answer
/// `e2ee_ready`. Runs under one of the ≤`E2EE_HANDSHAKE_PERMITS` permits;
/// `None` is any connection-closing violation — a non-handshake frame before
/// ready, malformed JSON, an unusable peer key, a dead socket.
async fn e2ee_server_handshake(
    stream: &mut ControlStream,
    secret: &[u8; 32],
    permits: &Arc<Semaphore>,
) -> Option<[u8; 32]> {
    // Held for the handshake only: a completed channel does not occupy a
    // permit, so four slow handshakes queue a fifth instead of crowding out
    // the CPU on key agreements.
    let _permit = permits.acquire().await.ok()?;
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                let key = e2ee::parse_e2ee_hello(&text)?;
                let shared_key = e2ee::derive_shared_key(secret, &key).ok()?;
                stream
                    .send(Message::Text(e2ee::e2ee_ready_frame().to_string()))
                    .await
                    .ok()?;
                return Some(shared_key);
            }
            // ping/pong is liveness, not content.
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => return None,
        }
    }
}

/// Decode + open one inbound wire frame and re-check the decrypted length
/// against the physical frame cap (spec §2.7 second layer: at the b64
/// quartet boundary a plaintext one byte over still fits the wire cap).
/// `None` is any of: bad base64, failed decryption, oversized or non-UTF-8
/// plaintext — all connection-closing violations.
fn decrypt_frame(shared: &[u8; 32], wire: &str) -> Option<String> {
    let bundle = BASE64_STANDARD.decode(wire).ok()?;
    let plaintext = e2ee::open(shared, &bundle).ok()?;
    if plaintext.len() > WS_PHYSICAL_FRAME_BYTES {
        return None;
    }
    String::from_utf8(plaintext).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_linear_and_capped() {
        let base = Duration::from_secs(1);
        let cap = Duration::from_secs(30);
        assert_eq!(control_backoff(1, base, cap), Duration::from_secs(1));
        assert_eq!(control_backoff(2, base, cap), Duration::from_secs(2));
        assert_eq!(control_backoff(29, base, cap), Duration::from_secs(29));
        assert_eq!(control_backoff(30, base, cap), Duration::from_secs(30));
        assert_eq!(control_backoff(31, base, cap), Duration::from_secs(30));
        assert_eq!(control_backoff(1000, base, cap), Duration::from_secs(30));
    }

    #[test]
    fn outbound_queue_overflow_reports_close() {
        let (out_tx, _out_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
        for _ in 0..OUTBOUND_QUEUE_CAPACITY {
            assert!(try_enqueue(&out_tx, Message::Ping(Vec::new())));
        }
        assert!(!try_enqueue(&out_tx, Message::Ping(Vec::new())));
    }

    #[tokio::test]
    async fn sync_reconcile_and_delta_semantics() {
        #[derive(Default)]
        struct RecordingDriver {
            opens: std::sync::Mutex<Vec<String>>,
            closes: std::sync::Mutex<Vec<String>>,
        }
        #[async_trait]
        impl ConnectionLifecycle for RecordingDriver {
            async fn open(&self, connection_id: String) {
                self.opens.lock().unwrap().push(connection_id);
            }
            async fn close(&self, connection_id: &str) {
                self.closes.lock().unwrap().push(connection_id.to_string());
            }
        }
        let driver = Arc::new(RecordingDriver::default());
        let mut live = HashSet::new();

        handle_control_frame(
            "{\"type\":\"sync\",\"connectionIds\":[\"c1\",\"c2\"]}",
            &mut live,
            driver.as_ref(),
        )
        .await
        .unwrap();
        handle_control_frame(
            "{\"type\":\"connected\",\"connectionId\":\"c3\"}",
            &mut live,
            driver.as_ref(),
        )
        .await
        .unwrap();
        handle_control_frame(
            "{\"type\":\"connected\",\"connectionId\":\"c3\"}",
            &mut live,
            driver.as_ref(),
        )
        .await
        .unwrap();
        handle_control_frame(
            "{\"type\":\"disconnected\",\"connectionId\":\"c1\"}",
            &mut live,
            driver.as_ref(),
        )
        .await
        .unwrap();
        handle_control_frame(
            "{\"type\":\"sync\",\"connectionIds\":[\"c2\"]}",
            &mut live,
            driver.as_ref(),
        )
        .await
        .unwrap();

        assert_eq!(*driver.opens.lock().unwrap(), vec!["c1", "c2", "c3"]);
        assert_eq!(*driver.closes.lock().unwrap(), vec!["c1", "c3"]);
        assert_eq!(live, HashSet::from(["c2".to_string()]));
    }

    #[tokio::test]
    async fn protocol_violations_reject_the_frame() {
        #[derive(Default)]
        struct NoopDriver;
        #[async_trait]
        impl ConnectionLifecycle for NoopDriver {
            async fn open(&self, _connection_id: String) {}
            async fn close(&self, _connection_id: &str) {}
        }
        let driver = Arc::new(NoopDriver);
        let mut live = HashSet::new();
        // Unknown type, malformed JSON, non-string id, sync without list.
        assert!(
            handle_control_frame("{\"type\":\"mystery\"}", &mut live, driver.as_ref())
                .await
                .is_err()
        );
        assert!(handle_control_frame("not json", &mut live, driver.as_ref())
            .await
            .is_err());
        assert!(handle_control_frame(
            "{\"type\":\"connected\",\"connectionId\":42}",
            &mut live,
            driver.as_ref()
        )
        .await
        .is_err());
        assert!(
            handle_control_frame("{\"type\":\"sync\"}", &mut live, driver.as_ref())
                .await
                .is_err()
        );
    }
}

#[test]
fn relay_urls_carry_the_v2_query_contract() {
    let config = RelayConfig {
        endpoint: "relay.example:443".into(),
        use_tls: true,
        server_id: "srv id".into(),
    };
    assert_eq!(
        control_url(&config),
        "wss://relay.example:443/ws?serverId=srv%20id&role=server&v=2"
    );
    assert_eq!(
        data_url(&config, "c 1"),
        "wss://relay.example:443/ws?serverId=srv%20id&role=server&v=2&connectionId=c%201"
    );
}

#[tokio::test]
async fn encrypted_channel_send_signals_close_on_full_queue() {
    let (close_tx, close_rx) = watch::channel(false);
    let (out_tx, _out_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
    let handle = EncryptedChannelHandle {
        connection_id: "c1".into(),
        shared: Arc::new([7u8; 32]),
        out: out_tx,
        close: close_tx,
    };
    assert_eq!(handle.connection_id(), "c1");
    for _ in 0..OUTBOUND_QUEUE_CAPACITY {
        assert!(handle.send("x".into()));
    }
    assert!(!handle.send("overflow".into()));
    assert!(*close_rx.borrow(), "full queue closes the connection");
}
