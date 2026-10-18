// ABOUTME: Relay control transport: outbound control-socket state machine (spec §2.5).
// ABOUTME: Control half only; per-client data sockets + E2EE land in plan T5.

//! The daemon holds exactly one outbound control WebSocket to the relay:
//! `ws(s)://<endpoint>/ws?serverId=<id>&role=server&v=2`. The relay pushes
//! `sync` (full connectionId list) on connect plus `connected`/`disconnected`
//! deltas; the transport reconciles them against its live set and reports
//! additions/removals to the [`RelaySessionSink`] (T5 turns those into
//! data-socket dials and E2EE channels).
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
//! and reconnect.

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::host_router::MAX_CLIENTS;
use crate::relay::e2ee::DaemonIdentity;
use crate::transport_limits::{OUTBOUND_QUEUE_CAPACITY, RELAY_CONTROL_FRAME_BYTES};

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

/// Consumer-facing surface of the transport. T5 replaces the bare
/// `open_session` with the full data-socket + E2EE channel handoff.
#[async_trait]
pub trait RelaySessionSink: Send + Sync {
    async fn on_connection_state(&self, state: ConnectionState);
    /// The relay reports a live connectionId the transport has no socket for
    /// (`sync` reconcile gap or a `connected` delta).
    async fn open_session(&self, connection_id: String);
    /// The relay reports a connectionId going away (`disconnected` delta or
    /// `sync` reconcile removal).
    async fn close_session(&self, connection_id: &str);
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
        // The identity rides along for T5's E2EE handshake; the control socket
        // itself only announces config.server_id.
        let _ = &identity;
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_control_loop(config, sink, seams, receiver));
        RelayTransportHandle { shutdown, task }
    }
}

async fn run_control_loop(
    config: RelayConfig,
    sink: Arc<dyn RelaySessionSink>,
    seams: TransportSeams,
    mut shutdown: watch::Receiver<bool>,
) {
    let timings = seams.timings;
    let mut live: HashSet<String> = HashSet::new();
    let mut failed_attempts: u32 = 0;
    while !*shutdown.borrow() {
        sink.on_connection_state(ConnectionState::Connecting).await;
        let outcome = run_control_cycle(&config, &sink, &seams, &mut shutdown, &mut live).await;
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
                    match handle_control_frame(&text, live, sink.as_ref()).await {
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

async fn dial_control(config: &RelayConfig, seams: &TransportSeams) -> Result<ControlStream, ()> {
    let scheme = if config.use_tls { "wss" } else { "ws" };
    let server_id = utf8_percent_encode(&config.server_id, NON_ALPHANUMERIC);
    let url = format!(
        "{scheme}://{}/ws?serverId={server_id}&role=server&v=2",
        config.endpoint
    );
    let request = url.into_client_request().map_err(|_| ())?;
    let dial = tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        seams.connector.clone(),
    );
    match tokio::time::timeout(seams.timings.handshake_timeout, dial).await {
        Ok(Ok((stream, _response))) => Ok(stream),
        _ => Err(()),
    }
}

/// Handle one relay→daemon control frame. `Err(())` is a protocol violation:
/// the control connection is dropped and reconnected per backoff.
async fn handle_control_frame(
    text: &str,
    live: &mut HashSet<String>,
    sink: &dyn RelaySessionSink,
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
                    sink.open_session(id.clone()).await;
                }
            }
            let mut removed: Vec<String> = live.difference(&desired).cloned().collect();
            removed.sort();
            for id in removed {
                sink.close_session(&id).await;
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
                sink.open_session(id.to_string()).await;
            }
            Ok(())
        }
        Some("disconnected") => {
            let id = value
                .get("connectionId")
                .and_then(Value::as_str)
                .ok_or(())?;
            if live.remove(id) {
                sink.close_session(id).await;
            }
            Ok(())
        }
        _ => Err(()),
    }
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
        struct Recorder {
            opens: std::sync::Mutex<Vec<String>>,
            closes: std::sync::Mutex<Vec<String>>,
        }
        #[async_trait]
        impl RelaySessionSink for Recorder {
            async fn on_connection_state(&self, _state: ConnectionState) {}
            async fn open_session(&self, connection_id: String) {
                self.opens.lock().unwrap().push(connection_id);
            }
            async fn close_session(&self, connection_id: &str) {
                self.closes.lock().unwrap().push(connection_id.to_string());
            }
        }
        let sink = Arc::new(Recorder::default());
        let mut live = HashSet::new();

        handle_control_frame(
            "{\"type\":\"sync\",\"connectionIds\":[\"c1\",\"c2\"]}",
            &mut live,
            sink.as_ref(),
        )
        .await
        .unwrap();
        handle_control_frame(
            "{\"type\":\"connected\",\"connectionId\":\"c3\"}",
            &mut live,
            sink.as_ref(),
        )
        .await
        .unwrap();
        handle_control_frame(
            "{\"type\":\"connected\",\"connectionId\":\"c3\"}",
            &mut live,
            sink.as_ref(),
        )
        .await
        .unwrap();
        handle_control_frame(
            "{\"type\":\"disconnected\",\"connectionId\":\"c1\"}",
            &mut live,
            sink.as_ref(),
        )
        .await
        .unwrap();
        handle_control_frame(
            "{\"type\":\"sync\",\"connectionIds\":[\"c2\"]}",
            &mut live,
            sink.as_ref(),
        )
        .await
        .unwrap();

        assert_eq!(*sink.opens.lock().unwrap(), vec!["c1", "c2", "c3"]);
        assert_eq!(*sink.closes.lock().unwrap(), vec!["c1", "c3"]);
        assert_eq!(live, HashSet::from(["c2".to_string()]));
    }

    #[tokio::test]
    async fn protocol_violations_reject_the_frame() {
        #[derive(Default)]
        struct Noop;
        #[async_trait]
        impl RelaySessionSink for Noop {
            async fn on_connection_state(&self, _state: ConnectionState) {}
            async fn open_session(&self, _connection_id: String) {}
            async fn close_session(&self, _connection_id: &str) {}
        }
        let sink = Arc::new(Noop);
        let mut live = HashSet::new();
        // Unknown type, malformed JSON, non-string id, sync without list.
        assert!(
            handle_control_frame("{\"type\":\"mystery\"}", &mut live, sink.as_ref())
                .await
                .is_err()
        );
        assert!(handle_control_frame("not json", &mut live, sink.as_ref())
            .await
            .is_err());
        assert!(handle_control_frame(
            "{\"type\":\"connected\",\"connectionId\":42}",
            &mut live,
            sink.as_ref()
        )
        .await
        .is_err());
        assert!(
            handle_control_frame("{\"type\":\"sync\"}", &mut live, sink.as_ref())
                .await
                .is_err()
        );
    }
}
