// ABOUTME: Relay client-role transport (plan T2, spec 2026-10-19 §5.2 contract 3).
// ABOUTME: role=client data socket: E2EE handshake, bounded plaintext pipe,
// ABOUTME: half-open detection, backoff redial. No app hello, no pairing.

//! The desktop client's outbound relay transport for one remote host:
//! `ws(s)://<endpoint>/ws?serverId=<id>&role=client&v=2`, the E2EE client
//! handshake (ephemeral keypair → `e2ee_hello` → `e2ee_ready`), a bounded
//! plaintext pipe, half-open detection, and backoff redial
//! (`min(30s, 1s × attempt)`, the [`crate::relay::transport`] curve).
//!
//! Layering (spec §5.2 contract 3, R2-05): this module is **pure transport**.
//! It never sends an application-level `hello`, never touches a pairing token
//! or device token, and owns no credential state — the host-level pairing
//! transaction belongs to `host_client`, and per-proxy sessions send their own
//! rewritten hello through the bridge. One relay session = one `hello`.
//!
//! Resend cadence is Paseo's client: `e2ee_hello` repeats every
//! `HELLO_RESEND_AFTER` up to `HELLO_MAX_SENDS` times, and the whole exchange
//! gives up at `CLIENT_HANDSHAKE_TIMEOUT` (the relay drops client frames sent
//! before the remote daemon dials its data socket, so silence is expected).
//! Resource bounds ride [`crate::transport_limits`]: the wire cap sits on the
//! WebSocket read config and the explicit check, the post-decrypt cap catches
//! the b64 quartet boundary, and truncated/tampered ciphertext closes that
//! connection only — the redial loop then re-establishes it.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::time::{timeout_at, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::relay::e2ee;
use crate::relay::transport::{
    bounded_writer, control_backoff, decrypt_frame, try_enqueue, ConnectionState, SleepFn,
    CONTROL_BACKOFF_BASE, CONTROL_BACKOFF_CAP,
};
use crate::transport_limits::{OUTBOUND_QUEUE_CAPACITY, RELAY_DATA_FRAME_TEXT_BYTES};

/// `e2ee_hello` resend cadence on silence: 1s (Paseo client parity).
pub const HELLO_RESEND_AFTER: Duration = Duration::from_secs(1);
/// At most this many `e2ee_hello` sends per connection.
pub const HELLO_MAX_SENDS: usize = 3;
/// Whole handshake budget (pre-hello delay included): 15s.
pub const CLIENT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Half-open detection: no inbound frame for this long closes the socket: 30s.
pub const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// WS ping cadence: 10s (the pong is what keeps a quiet session alive).
pub const CLIENT_PING_INTERVAL: Duration = Duration::from_secs(10);
/// Dial (TCP + WS upgrade) deadline: 10s. A cold dial against the company relay
/// can exceed this (TLS + the daemon's data-dial handoff measured ~10-15s), so a
/// timeout is an ordinary retry, not a failure: the bridge keeps the proxy
/// waiting across attempts instead of closing it.
pub const CLIENT_DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// Head start for the remote daemon's data dial before the first `e2ee_hello`.
pub const CLIENT_PRE_HELLO_DELAY: Duration = Duration::from_millis(250);

/// One client-role data socket, before the socket halves are taken.
pub type ClientStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Injectable timings (defaults are the spec values; tests shrink them).
#[derive(Debug, Clone, Copy)]
pub struct ClientRoleTimings {
    pub dial_timeout: Duration,
    pub pre_hello_delay: Duration,
    pub hello_resend_after: Duration,
    pub hello_max_sends: usize,
    pub handshake_timeout: Duration,
    pub idle_timeout: Duration,
    pub ping_interval: Duration,
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
}

impl Default for ClientRoleTimings {
    fn default() -> Self {
        Self {
            dial_timeout: CLIENT_DIAL_TIMEOUT,
            pre_hello_delay: CLIENT_PRE_HELLO_DELAY,
            hello_resend_after: HELLO_RESEND_AFTER,
            hello_max_sends: HELLO_MAX_SENDS,
            handshake_timeout: CLIENT_HANDSHAKE_TIMEOUT,
            idle_timeout: CLIENT_IDLE_TIMEOUT,
            ping_interval: CLIENT_PING_INTERVAL,
            backoff_base: CONTROL_BACKOFF_BASE,
            backoff_cap: CONTROL_BACKOFF_CAP,
        }
    }
}

/// Injection point for timers, the backoff sleep, and the TLS connector —
/// the same shape as [`crate::relay::transport::TransportSeams`]. Production
/// uses [`Default`]; the connector is `None` (native-tls defaults).
#[derive(Clone)]
pub struct ClientRoleSeams {
    pub timings: ClientRoleTimings,
    pub sleep: SleepFn,
    pub connector: Option<Connector>,
}

impl Default for ClientRoleSeams {
    fn default() -> Self {
        Self {
            timings: ClientRoleTimings::default(),
            sleep: Arc::new(|duration| Box::pin(tokio::time::sleep(duration))),
            connector: None,
        }
    }
}

/// Where this session dials, which serverId it announces, and the daemon
/// public key from the host's offer (the E2EE trust anchor).
#[derive(Debug, Clone)]
pub struct ClientRoleConfig {
    pub endpoint: String,
    pub use_tls: bool,
    pub server_id: String,
    pub peer_public_key_b64: String,
}

/// Write path for the live session: plaintext in, base64 ciphertext out on
/// the bounded queue. Handed to the sink on `Online`.
#[derive(Clone)]
pub struct ClientChannel {
    shared: Arc<[u8; 32]>,
    out: mpsc::Sender<Message>,
    close: watch::Sender<bool>,
}

impl ClientChannel {
    /// Queue one outbound text frame. `false`: the queue is full or the
    /// writer is gone — this connection is closing and the frame is dropped
    /// (a slow consumer closes only its own connection, spec §5.2 R2-10).
    pub fn send(&self, plaintext: String) -> bool {
        let wire = BASE64_STANDARD.encode(e2ee::seal(&self.shared, plaintext.as_bytes()));
        if try_enqueue(&self.out, Message::Text(wire)) {
            true
        } else {
            let _ = self.close.send(true);
            false
        }
    }
}

/// Consumer surface: the bridge/proxy layer owns the session meaning.
#[async_trait]
pub trait ClientRoleSink: Send + Sync {
    async fn on_state(&self, state: ConnectionState);
    /// E2EE handshake done — the session is usable for frames.
    async fn on_online(&self, channel: ClientChannel);
    /// One decrypted plaintext frame. Nothing arrives before `on_online`.
    async fn on_frame(&self, payload: String);
}

/// Owns the redial loop. Dropping the handle leaves the loop running;
/// [`stop`](Self::stop) cancels an in-flight dial or handshake and joins.
pub struct ClientRoleHandle {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ClientRoleHandle {
    /// Stop the session: cancels dial/handshake/backoff locally, never waits
    /// on the network. Idempotent in effect — a second call finds it done.
    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
    }
}

pub struct ClientRole;

impl ClientRole {
    /// Start with production parameters.
    pub fn start(config: ClientRoleConfig, sink: Arc<dyn ClientRoleSink>) -> ClientRoleHandle {
        Self::start_with_seams(config, sink, ClientRoleSeams::default())
    }

    /// Start with injected seams — the entry the mock-relay tests use.
    pub fn start_with_seams(
        config: ClientRoleConfig,
        sink: Arc<dyn ClientRoleSink>,
        seams: ClientRoleSeams,
    ) -> ClientRoleHandle {
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_client_loop(config, sink, seams, receiver));
        ClientRoleHandle { shutdown, task }
    }
}

async fn run_client_loop(
    config: ClientRoleConfig,
    sink: Arc<dyn ClientRoleSink>,
    seams: ClientRoleSeams,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut failed_attempts: u32 = 0;
    while !*shutdown.borrow() {
        sink.on_state(ConnectionState::Connecting).await;
        match run_session(&config, &sink, &seams, &mut shutdown).await {
            SessionOutcome::Stopped => return,
            SessionOutcome::Dropped { reached_online } => {
                // A session that reached Online resets the curve: the next
                // failure starts over at base × 1.
                if reached_online {
                    failed_attempts = 0;
                }
                failed_attempts = failed_attempts.saturating_add(1);
                sink.on_state(ConnectionState::Offline).await;
                let backoff = control_backoff(
                    failed_attempts,
                    seams.timings.backoff_base,
                    seams.timings.backoff_cap,
                );
                tokio::select! {
                    _ = (seams.sleep)(backoff) => {}
                    _ = shutdown.changed() => return,
                }
            }
        }
    }
}

enum SessionOutcome {
    /// Shutdown observed — exit without reporting Offline.
    Stopped,
    /// The session ended (error, close, stale, frame violation) — report
    /// Offline and redial per backoff.
    Dropped { reached_online: bool },
}

async fn run_session(
    config: &ClientRoleConfig,
    sink: &Arc<dyn ClientRoleSink>,
    seams: &ClientRoleSeams,
    shutdown: &mut watch::Receiver<bool>,
) -> SessionOutcome {
    // Dial under the shutdown flag: stop() must not wait out an upgrade.
    let dial = dial_client(config, seams);
    let mut stream = tokio::select! {
        _ = shutdown.changed() => return SessionOutcome::Stopped,
        stream = dial => match stream {
            Ok(stream) => stream,
            Err(()) => return SessionOutcome::Dropped { reached_online: false },
        },
    };

    let handshake = e2ee_client_handshake(&mut stream, &config.peer_public_key_b64, &seams.timings);
    let shared_key = tokio::select! {
        _ = shutdown.changed() => return SessionOutcome::Stopped,
        outcome = handshake => match outcome {
            Ok(shared_key) => shared_key,
            Err(_) => return SessionOutcome::Dropped { reached_online: false },
        },
    };

    let shared_key = Arc::new(shared_key);
    let (socket_sink, mut inbound) = stream.split();
    let (out_tx, out_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
    let writer = tokio::spawn(bounded_writer(socket_sink, out_rx));
    let (close_tx, mut close_rx) = watch::channel(false);
    sink.on_online(ClientChannel {
        shared: Arc::clone(&shared_key),
        out: out_tx.clone(),
        close: close_tx,
    })
    .await;
    sink.on_state(ConnectionState::Online).await;

    let timings = seams.timings;
    let mut last_seen = Instant::now();
    let mut ping = tokio::time::interval_at(
        Instant::now() + timings.ping_interval,
        timings.ping_interval,
    );
    let outcome = loop {
        tokio::select! {
            _ = shutdown.changed() => break SessionOutcome::Stopped,
            _ = close_rx.changed() => {
                // Local overflow close: the queue signalled, redial.
                break SessionOutcome::Dropped { reached_online: true };
            }
            _ = ping.tick() => {
                if last_seen + timings.idle_timeout <= Instant::now() {
                    // Half-open: the peer stopped answering our pings.
                    break SessionOutcome::Dropped { reached_online: true };
                }
                if !try_enqueue(&out_tx, Message::Ping(Vec::new())) {
                    break SessionOutcome::Dropped { reached_online: true };
                }
            }
            frame = inbound.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    last_seen = Instant::now();
                    // Wire cap, then post-decrypt cap (spec §2.7 layers);
                    // bad base64 and failed opens are the same violation
                    // class. Any of them closes this connection only.
                    if text.len() > RELAY_DATA_FRAME_TEXT_BYTES {
                        break SessionOutcome::Dropped { reached_online: true };
                    }
                    let Some(plaintext) = decrypt_frame(&shared_key, &text) else {
                        break SessionOutcome::Dropped { reached_online: true };
                    };
                    sink.on_frame(plaintext).await;
                }
                // The data protocol is text-only; binary is a violation.
                Some(Ok(Message::Binary(_))) => {
                    break SessionOutcome::Dropped { reached_online: true };
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    break SessionOutcome::Dropped { reached_online: true };
                }
                // ping/pong: connection liveness only.
                Some(Ok(_)) => last_seen = Instant::now(),
            },
        }
    };
    // Local-only teardown: aborting the writer drops the socket halves.
    writer.abort();
    outcome
}

/// `ws(s)://<endpoint>/ws?serverId=&role=client&v=2` with the wire cap on the
/// read config, so oversized input dies at the protocol layer too.
async fn dial_client(
    config: &ClientRoleConfig,
    seams: &ClientRoleSeams,
) -> Result<ClientStream, ()> {
    let scheme = if config.use_tls { "wss" } else { "ws" };
    let server_id = utf8_percent_encode(&config.server_id, NON_ALPHANUMERIC);
    let url = format!(
        "{scheme}://{}/ws?serverId={server_id}&role=client&v=2",
        config.endpoint
    );
    let request = url.into_client_request().map_err(|_| ())?;
    let ws_config = WebSocketConfig {
        max_message_size: Some(RELAY_DATA_FRAME_TEXT_BYTES),
        max_frame_size: Some(RELAY_DATA_FRAME_TEXT_BYTES),
        ..WebSocketConfig::default()
    };
    let dial = tokio_tungstenite::connect_async_tls_with_config(
        request,
        Some(ws_config),
        false,
        seams.connector.clone(),
    );
    match tokio::time::timeout(seams.timings.dial_timeout, dial).await {
        Ok(Ok((stream, _response))) => Ok(stream),
        _ => Err(()),
    }
}

/// The client side of the E2EE handshake: fresh ephemeral keypair, then
/// `e2ee_hello` → shared key on the daemon's `e2ee_ready`. Frames sent before
/// the remote daemon dials its data socket are dropped by the relay, so the
/// hello repeats on silence up to [`HELLO_MAX_SENDS`] sends, and the whole
/// exchange gives up at `pre_hello_delay + handshake_timeout`. Any other
/// pre-ready frame is a violation.
pub async fn e2ee_client_handshake(
    stream: &mut ClientStream,
    peer_public_key_b64: &str,
    timings: &ClientRoleTimings,
) -> Result<[u8; 32], String> {
    let secret = crypto_box::SecretKey::generate(&mut rand::rngs::OsRng);
    let public_key_b64 = BASE64_STANDARD.encode(secret.public_key().as_bytes());
    let shared = e2ee::derive_shared_key(&secret.to_bytes(), peer_public_key_b64)?;
    let hello = format!("{{\"type\":\"e2ee_hello\",\"key\":\"{public_key_b64}\"}}");

    let deadline = Instant::now() + timings.pre_hello_delay + timings.handshake_timeout;
    tokio::time::sleep(timings.pre_hello_delay).await;
    send_text(stream, &hello).await?;
    let mut sent = 1;
    loop {
        let resend_at = Instant::now() + timings.hello_resend_after;
        let wait_until = if sent < timings.hello_max_sends && resend_at < deadline {
            resend_at
        } else {
            deadline
        };
        match timeout_at(wait_until, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if text != e2ee::e2ee_ready_frame() {
                    return Err("unexpected frame before e2ee_ready".to_string());
                }
                return Ok(shared);
            }
            // ping/pong is liveness, not content.
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(Some(Ok(_))) => {
                return Err("unexpected non-text frame before e2ee_ready".to_string())
            }
            Ok(Some(Err(error))) => {
                return Err(format!("socket read failed before e2ee_ready: {error}"))
            }
            Ok(None) => return Err("relay closed before e2ee_ready".to_string()),
            Err(_) if Instant::now() < deadline => {
                send_text(stream, &hello).await?;
                sent += 1;
            }
            Err(_) => return Err("timed out waiting for e2ee_ready".to_string()),
        }
    }
}

async fn send_text(stream: &mut ClientStream, text: &str) -> Result<(), String> {
    stream
        .send(Message::Text(text.to_string()))
        .await
        .map_err(|error| format!("socket send failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec §5.2 / §2.1 numbers, pinned as constants rather than prose.
    #[test]
    fn default_timings_carry_the_contract_values() {
        let timings = ClientRoleTimings::default();
        assert_eq!(timings.hello_resend_after, Duration::from_secs(1));
        assert_eq!(timings.hello_max_sends, 3);
        assert_eq!(timings.handshake_timeout, Duration::from_secs(15));
        assert_eq!(timings.idle_timeout, Duration::from_secs(30));
        assert_eq!(timings.backoff_base, Duration::from_secs(1));
        assert_eq!(timings.backoff_cap, Duration::from_secs(30));
    }

    #[test]
    fn channel_send_closes_the_session_when_the_queue_fills() {
        let (out_tx, _out_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
        let (close_tx, close_rx) = watch::channel(false);
        let channel = ClientChannel {
            shared: Arc::new([9u8; 32]),
            out: out_tx,
            close: close_tx,
        };
        for _ in 0..OUTBOUND_QUEUE_CAPACITY {
            assert!(channel.send("x".into()));
        }
        assert!(!channel.send("overflow".into()));
        assert!(
            *close_rx.borrow(),
            "a full queue closes this connection only"
        );
    }
}
