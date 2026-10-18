// ABOUTME: Relay transport contract tests against a mock relay (plan T4 + T5).
// ABOUTME: Control socket: online/offline, sync reconcile, backoff, stale,
// ABOUTME: ready timeout, frame bounds. Data sockets behind the E2EE
// ABOUTME: handshake: dial contract, encrypted echo round-trips,
// ABOUTME: slow-consumer eviction, inbound length bounds.

//! Every assertion runs on injected seams, never wall-clock luck (plan T4
//! risk: "all timing asserts must run on injected clocks/delay"): reconnect
//! backoff durations are *recorded* by the sleep seam and asserted as the
//! spec §2.5 sequence; the ping/stale/ready timers run on injected short
//! durations with generous await timeouts.

#[path = "support/e2ee_client.rs"]
mod e2ee_client;
#[path = "support/mock_relay.rs"]
mod mock_relay;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mock_relay::{MockClientSocket, MockRelay, SilentMode};
use picot_host::relay::e2ee::DaemonIdentity;
use picot_host::relay::transport::{
    ConnectionState, EncryptedChannelHandle, RelayConfig, RelaySessionSink, RelayTransport,
    SleepFn, TransportSeams, TransportTimings,
};
use picot_host::transport_limits::{RELAY_DATA_FRAME_TEXT_BYTES, WS_PHYSICAL_FRAME_BYTES};
use tokio::sync::mpsc;

/// Records every state transition; tests await them with generous timeouts.
#[derive(Clone)]
struct RecordingSink {
    states: mpsc::UnboundedSender<ConnectionState>,
    opens: mpsc::UnboundedSender<String>,
    closes: mpsc::UnboundedSender<String>,
}

#[async_trait::async_trait]
impl RelaySessionSink for RecordingSink {
    async fn on_connection_state(&self, state: ConnectionState) {
        let _ = self.states.send(state);
    }
    async fn open_session(&self, connection_id: String, _channel: EncryptedChannelHandle) {
        let _ = self.opens.send(connection_id);
    }
    async fn data_frame(&self, _connection_id: &str, _payload: String) {}
    async fn close_session(&self, connection_id: &str) {
        let _ = self.closes.send(connection_id.to_string());
    }
}

/// The data-side sink for the integration tests: echoes every inbound frame
/// back through the handle it was handed — the same shape the slice-b
/// session bridge will have (the sink owns the outbound path).
struct EchoSink {
    states: mpsc::UnboundedSender<ConnectionState>,
    opens: mpsc::UnboundedSender<String>,
    closes: mpsc::UnboundedSender<String>,
    handles: Mutex<HashMap<String, EncryptedChannelHandle>>,
}

#[async_trait::async_trait]
impl RelaySessionSink for EchoSink {
    async fn on_connection_state(&self, state: ConnectionState) {
        let _ = self.states.send(state);
    }
    async fn open_session(&self, connection_id: String, channel: EncryptedChannelHandle) {
        let _ = self.opens.send(connection_id.clone());
        self.handles.lock().unwrap().insert(connection_id, channel);
    }
    async fn data_frame(&self, connection_id: &str, payload: String) {
        if let Some(channel) = self.handles.lock().unwrap().get(connection_id) {
            channel.send(payload);
        }
    }
    async fn close_session(&self, connection_id: &str) {
        let _ = self.closes.send(connection_id.to_string());
        self.handles.lock().unwrap().remove(connection_id);
    }
}

/// EchoSink plus its three record channels.
fn echo_sink() -> (
    Arc<EchoSink>,
    mpsc::UnboundedReceiver<ConnectionState>,
    mpsc::UnboundedReceiver<String>,
    mpsc::UnboundedReceiver<String>,
) {
    let (state_tx, states) = mpsc::unbounded_channel();
    let (open_tx, opens) = mpsc::unbounded_channel();
    let (close_tx, closes) = mpsc::unbounded_channel();
    let sink = Arc::new(EchoSink {
        states: state_tx,
        opens: open_tx,
        closes: close_tx,
        handles: Mutex::new(HashMap::new()),
    });
    (sink, states, opens, closes)
}

/// Fake clock for the backoff path: records the requested sleep instead of
/// waiting, so the reconnect sequence asserts exact durations instantly.
#[derive(Clone, Default)]
struct RecordingSleep(Arc<Mutex<Vec<Duration>>>);

impl RecordingSleep {
    fn durations(&self) -> Vec<Duration> {
        self.0.lock().unwrap().clone()
    }
    fn seam(&self) -> SleepFn {
        let recorded = Arc::clone(&self.0);
        Arc::new(move |duration| {
            recorded.lock().unwrap().push(duration);
            Box::pin(std::future::ready(()))
        })
    }
}

fn test_timings() -> TransportTimings {
    TransportTimings {
        // Backoff magnitudes are the spec values; the recording sleep makes
        // them observable without waiting.
        backoff_base: Duration::from_secs(1),
        backoff_cap: Duration::from_secs(30),
        ping_interval: Duration::from_millis(50),
        stale_timeout: Duration::from_millis(400),
        ready_timeout: Duration::from_millis(400),
        handshake_timeout: Duration::from_secs(2),
        data_open_timeout: Duration::from_secs(2),
        e2ee_handshake_timeout: Duration::from_secs(2),
    }
}

fn test_identity() -> DaemonIdentity {
    DaemonIdentity::for_tests()
}

fn start_transport(
    relay: &MockRelay,
    identity: DaemonIdentity,
    sink: Arc<dyn RelaySessionSink>,
    timings: TransportTimings,
    sleep: SleepFn,
) -> picot_host::relay::transport::RelayTransportHandle {
    RelayTransport::start_with_seams(
        RelayConfig {
            endpoint: relay.addr().to_string(),
            use_tls: false,
            server_id: "test-server-id".into(),
        },
        identity,
        sink,
        TransportSeams {
            timings,
            sleep,
            connector: None,
        },
    )
}

/// Transport with a fresh identity whose public half the test keeps for the
/// client-side key agreement.
fn transport_with_fresh_identity(
    relay: &MockRelay,
    sink: Arc<dyn RelaySessionSink>,
    timings: TransportTimings,
) -> (
    picot_host::relay::transport::RelayTransportHandle,
    DaemonIdentity,
) {
    let identity = test_identity();
    (
        start_transport(relay, identity.clone(), sink, timings, default_sleep()),
        identity,
    )
}

/// Connect a client, wait for the daemon's data dial, complete the E2EE
/// handshake, and return the client with its per-connection shared key.
/// Frames sent before the dial are dropped by the mock's pipe routing, so
/// every encrypted test must start here.
async fn encrypted_client(
    relay: &MockRelay,
    daemon_public_key_b64: &str,
) -> (MockClientSocket, [u8; 32]) {
    let before = relay.data_dials().len();
    let mut client = relay.connect_client().await;
    for _ in 0..400 {
        if relay.data_dials().len() > before {
            let shared = e2ee_client::handshake(&mut client, daemon_public_key_b64).await;
            return (client, shared);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("daemon never dialed the data socket");
}

async fn recv_state(rx: &mut mpsc::UnboundedReceiver<ConnectionState>) -> ConnectionState {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("state within timeout")
        .expect("transport task alive")
}

async fn recv_open(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("open within timeout")
        .expect("transport task alive")
}

async fn recv_close(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("close within timeout")
        .expect("transport task alive")
}

async fn until(what: &str, check: impl Fn() -> bool) {
    for _ in 0..400 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not met within timeout: {what}");
}

#[tokio::test]
async fn control_connects_reports_online_and_no_sessions_without_events() {
    let relay = MockRelay::start().await;
    let (state_tx, mut states) = mpsc::unbounded_channel();
    let (open_tx, mut opens) = mpsc::unbounded_channel();
    let (close_tx, _closes) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink {
        states: state_tx,
        opens: open_tx,
        closes: close_tx,
    });
    let transport = start_transport(
        &relay,
        test_identity(),
        sink,
        test_timings(),
        default_sleep(),
    );

    assert_eq!(recv_state(&mut states).await, ConnectionState::Connecting);
    assert_eq!(recv_state(&mut states).await, ConnectionState::Online);
    assert_eq!(relay.control_upgrades(), 1);

    // A control connection with no relay events dispatches nothing to the sink.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        opens.try_recv().is_err(),
        "no open_session before relay events"
    );

    let health = reqwest::get(format!("http://{}/health", relay.addr()))
        .await
        .expect("health");
    assert!(health.status().is_success());

    transport.stop().await;
    assert_eq!(relay.control_upgrades(), 1, "stop does not reconnect");
}

#[tokio::test]
async fn sync_connected_disconnected_route_to_sink() {
    let relay = MockRelay::start().await;
    let (state_tx, mut states) = mpsc::unbounded_channel();
    let (open_tx, mut opens) = mpsc::unbounded_channel();
    let (close_tx, mut closes) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink {
        states: state_tx,
        opens: open_tx,
        closes: close_tx,
    });
    let (transport, identity) = transport_with_fresh_identity(&relay, sink, test_timings());
    let _ = recv_state(&mut states).await;
    // Two real clients: the mock reports `connected` per client, the daemon
    // dials + E2EE-handshakes each data socket, and open_session fires only
    // once the channel is ready (so both clients complete the handshake).
    let (client_one, _shared_one) = encrypted_client(&relay, &identity.public_key_b64).await;
    let first = recv_open(&mut opens).await;
    let (client_two, _shared_two) = encrypted_client(&relay, &identity.public_key_b64).await;
    let second = recv_open(&mut opens).await;

    // sync reconcile against the live set: already-known ids open nothing,
    // ids the relay drops close (spec §2.5).
    relay.send_control(&format!(
        "{{\"type\":\"sync\",\"connectionIds\":[\"{first}\"]}}"
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(opens.try_recv().is_err(), "known ids open nothing on sync");
    assert_eq!(recv_close(&mut closes).await, second);

    // connected/disconnected deltas; duplicate connected is idempotent.
    relay.send_control(&format!(
        "{{\"type\":\"connected\",\"connectionId\":\"{first}\"}}"
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        opens.try_recv().is_err(),
        "duplicate connected opens nothing"
    );
    client_one.close().await;
    assert_eq!(recv_close(&mut closes).await, first);
    drop(client_two);

    transport.stop().await;
}

#[tokio::test]
async fn oversized_control_frame_drops_connection_and_reconnects() {
    let relay = MockRelay::start().await;
    let (state_tx, mut states) = mpsc::unbounded_channel();
    let (open_tx, _opens) = mpsc::unbounded_channel();
    let (close_tx, _closes) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink {
        states: state_tx,
        opens: open_tx,
        closes: close_tx,
    });
    let transport = start_transport(
        &relay,
        test_identity(),
        sink,
        test_timings(),
        default_sleep(),
    );
    let _ = recv_state(&mut states).await;
    let _ = recv_state(&mut states).await; // Online

    // 64 KiB + 1 of text: over RELAY_CONTROL_FRAME_BYTES → drop control.
    let oversized = format!(
        "{}{}",
        "{\"type\":\"sync\",\"connectionIds\":[\"",
        "x".repeat(64 * 1024)
    );
    relay.send_control(&oversized);
    until("reconnect after oversized frame", || {
        relay.control_upgrades() >= 2
    })
    .await;
    assert_eq!(recv_state(&mut states).await, ConnectionState::Offline);

    transport.stop().await;
}

#[tokio::test]
async fn oversized_sync_list_drops_control() {
    let relay = MockRelay::start().await;
    let (state_tx, mut states) = mpsc::unbounded_channel();
    let (open_tx, _opens) = mpsc::unbounded_channel();
    let (close_tx, _closes) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink {
        states: state_tx,
        opens: open_tx,
        closes: close_tx,
    });
    let transport = start_transport(
        &relay,
        test_identity(),
        sink,
        test_timings(),
        default_sleep(),
    );
    let _ = recv_state(&mut states).await;
    let _ = recv_state(&mut states).await; // Online

    // MAX_CLIENTS + 1 ids: protocol anomaly → drop control (spec §2.7).
    let ids: Vec<String> = (0..257).map(|i| format!("\"x{i}\"")).collect();
    relay.send_control(&format!(
        "{{\"type\":\"sync\",\"connectionIds\":[{}]}}",
        ids.join(",")
    ));
    until("reconnect after oversized sync", || {
        relay.control_upgrades() >= 2
    })
    .await;

    transport.stop().await;
}

#[tokio::test]
async fn reconnect_backoff_is_linear_capped_and_resets_on_success() {
    let relay = MockRelay::start().await;
    let (state_tx, mut states) = mpsc::unbounded_channel();
    let (open_tx, _opens) = mpsc::unbounded_channel();
    let (close_tx, _closes) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink {
        states: state_tx,
        opens: open_tx,
        closes: close_tx,
    });
    let recorder = RecordingSleep::default();
    relay.set_gate(false);
    let transport = start_transport(
        &relay,
        test_identity(),
        sink,
        test_timings(),
        recorder.seam(),
    );

    until("35 recorded backoffs", || recorder.durations().len() >= 35).await;
    let durations = recorder.durations();
    // Spec §2.5: min(30s, 1s × attempt) — linear, capped.
    for (index, duration) in durations.iter().take(30).enumerate() {
        assert_eq!(
            *duration,
            Duration::from_secs(index as u64 + 1),
            "attempt {}",
            index + 1
        );
    }
    assert!(
        durations
            .iter()
            .skip(30)
            .all(|d| *d == Duration::from_secs(30)),
        "backoff caps at 30s"
    );

    // Recovery: gate opens → next dial succeeds and reports Online.
    let upgrades_before_recovery = relay.control_upgrades();
    relay.set_gate(true);
    until("online after gate opens", || {
        relay.control_upgrades() > upgrades_before_recovery
    })
    .await;
    loop {
        if recv_state(&mut states).await == ConnectionState::Online {
            break;
        }
    }
    let before = recorder.durations().len();
    relay.drop_control();
    until("one more backoff after drop", || {
        recorder.durations().len() > before
    })
    .await;
    assert_eq!(
        recorder.durations()[before],
        Duration::from_secs(1),
        "attempt counter resets on success"
    );

    transport.stop().await;
}

#[tokio::test]
async fn silent_relay_after_sync_is_stale_force_closed() {
    let relay = MockRelay::start().await;
    let (state_tx, mut states) = mpsc::unbounded_channel();
    let (open_tx, _opens) = mpsc::unbounded_channel();
    let (close_tx, _closes) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink {
        states: state_tx,
        opens: open_tx,
        closes: close_tx,
    });
    relay.set_silent(Some(SilentMode::AfterSync));
    let transport = start_transport(
        &relay,
        test_identity(),
        sink,
        test_timings(),
        default_sleep(),
    );

    assert_eq!(recv_state(&mut states).await, ConnectionState::Connecting);
    assert_eq!(recv_state(&mut states).await, ConnectionState::Online);
    // Mock parks without reading → no pong → stale timeout force-closes.
    until("stale force-close reconnects", || {
        relay.control_upgrades() >= 2
    })
    .await;
    assert_eq!(recv_state(&mut states).await, ConnectionState::Offline);

    transport.stop().await;
}

#[tokio::test]
async fn relay_silent_before_first_frame_hits_ready_timeout() {
    let relay = MockRelay::start().await;
    let (state_tx, mut states) = mpsc::unbounded_channel();
    let (open_tx, _opens) = mpsc::unbounded_channel();
    let (close_tx, _closes) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink {
        states: state_tx,
        opens: open_tx,
        closes: close_tx,
    });
    relay.set_silent(Some(SilentMode::BeforeReady));
    let transport = start_transport(
        &relay,
        test_identity(),
        sink,
        test_timings(),
        default_sleep(),
    );

    assert_eq!(recv_state(&mut states).await, ConnectionState::Connecting);
    // No first frame → ready timeout closes the socket → reconnect loop.
    until("ready-timeout reconnects", || relay.control_upgrades() >= 2).await;
    assert_eq!(recv_state(&mut states).await, ConnectionState::Offline);
    assert_eq!(recv_state(&mut states).await, ConnectionState::Connecting);
    assert_ne!(
        recv_state(&mut states).await,
        ConnectionState::Online,
        "never online without sync"
    );

    transport.stop().await;
}

#[tokio::test]
async fn stop_closes_locally_without_reconnect() {
    let relay = MockRelay::start().await;
    let (state_tx, mut states) = mpsc::unbounded_channel();
    let (open_tx, _opens) = mpsc::unbounded_channel();
    let (close_tx, _closes) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink {
        states: state_tx,
        opens: open_tx,
        closes: close_tx,
    });
    let transport = start_transport(
        &relay,
        test_identity(),
        sink,
        test_timings(),
        default_sleep(),
    );
    let _ = recv_state(&mut states).await;
    let _ = recv_state(&mut states).await; // Online

    let started = std::time::Instant::now();
    transport.stop().await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "stop is local-only"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(relay.control_upgrades(), 1, "no reconnect after stop");
}

#[tokio::test]
async fn data_socket_dials_with_v2_contract_and_round_trips_frames() {
    let relay = MockRelay::start().await;
    let (sink, mut states, mut opens, _closes) = echo_sink();
    let (transport, identity) = transport_with_fresh_identity(&relay, sink, test_timings());
    let _ = recv_state(&mut states).await;
    let _ = recv_state(&mut states).await; // Online

    let (mut client, shared) = encrypted_client(&relay, &identity.public_key_b64).await;
    let id = recv_open(&mut opens).await;
    // The data dial carries the full v2 query contract for its connection.
    until("data dial recorded with contract query", || {
        relay
            .data_dials()
            .iter()
            .any(|(server_id, connection_id)| server_id == "test-server-id" && connection_id == &id)
    })
    .await;

    // Every post-handshake frame is ciphertext (spec §2.4): the echo comes
    // back sealed under the same per-connection key.
    e2ee_client::send_sealed(&mut client, &shared, "ping-1").await;
    assert_eq!(
        e2ee_client::recv_opened(&mut client, &shared).await,
        "ping-1"
    );

    // Each client is an independent data socket.
    let (mut second, second_shared) = encrypted_client(&relay, &identity.public_key_b64).await;
    let second_id = recv_open(&mut opens).await;
    assert_ne!(id, second_id);
    e2ee_client::send_sealed(&mut second, &second_shared, "ping-2").await;
    assert_eq!(
        e2ee_client::recv_opened(&mut second, &second_shared).await,
        "ping-2"
    );
    e2ee_client::send_sealed(&mut client, &shared, "ping-3").await;
    assert_eq!(
        e2ee_client::recv_opened(&mut client, &shared).await,
        "ping-3"
    );

    transport.stop().await;
}

#[tokio::test]
async fn client_disconnect_closes_the_data_session() {
    let relay = MockRelay::start().await;
    let (sink, mut states, _opens, mut closes) = echo_sink();
    let transport = start_transport(
        &relay,
        test_identity(),
        sink,
        test_timings(),
        default_sleep(),
    );
    let _ = recv_state(&mut states).await;
    let _ = recv_state(&mut states).await; // Online

    // The client leaves before any e2ee_hello: open_session never fires
    // (handshake-gated), and the relay's `disconnected` still routes a
    // close_session for its id (first client on this relay is c-1).
    let client = relay.connect_client().await;
    client.close().await;
    assert_eq!(recv_close(&mut closes).await, "c-1");

    transport.stop().await;
}

#[tokio::test]
async fn sync_removal_closes_only_that_data_socket() {
    let relay = MockRelay::start().await;
    let (sink, mut states, mut opens, mut closes) = echo_sink();
    let (transport, identity) = transport_with_fresh_identity(&relay, sink, test_timings());
    let _ = recv_state(&mut states).await;
    let _ = recv_state(&mut states).await; // Online

    let (mut first, _first_shared) = encrypted_client(&relay, &identity.public_key_b64).await;
    let first_id = recv_open(&mut opens).await;
    let (mut second, second_shared) = encrypted_client(&relay, &identity.public_key_b64).await;
    let second_id = recv_open(&mut opens).await;

    // A sync that drops first_id severs only that data connection.
    relay.send_control(&format!(
        "{{\"type\":\"sync\",\"connectionIds\":[\"{second_id}\"]}}"
    ));
    assert_eq!(recv_close(&mut closes).await, first_id);
    first.expect_close().await;

    e2ee_client::send_sealed(&mut second, &second_shared, "still-here").await;
    assert_eq!(
        e2ee_client::recv_opened(&mut second, &second_shared).await,
        "still-here"
    );

    transport.stop().await;
}

#[tokio::test]
async fn slow_consumer_is_evicted_without_touching_control_or_peers() {
    let relay = MockRelay::start().await;
    let (sink, mut states, mut opens, mut closes) = echo_sink();
    let (transport, identity) = transport_with_fresh_identity(&relay, sink, test_timings());
    let _ = recv_state(&mut states).await;
    let _ = recv_state(&mut states).await; // Online

    let (mut stalled, stalled_shared) = encrypted_client(&relay, &identity.public_key_b64).await;
    let stalled_id = recv_open(&mut opens).await;
    let (mut healthy, healthy_shared) = encrypted_client(&relay, &identity.public_key_b64).await;
    let healthy_id = recv_open(&mut opens).await;
    assert_ne!(stalled_id, healthy_id);

    // The stalled client never reads; the daemon's sealed echoes fill its
    // bounded outbound queue (OUTBOUND_QUEUE_CAPACITY) through the mock's
    // bounded pipes → eviction closes that data connection only (spec §2.7).
    let payload = "x".repeat(64 * 1024);
    let sender = tokio::spawn(async move {
        for _ in 0..800 {
            e2ee_client::send_sealed(&mut stalled, &stalled_shared, &payload).await;
        }
    });

    let evicted = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let id = closes.recv().await.expect("transport task alive");
            if id == stalled_id {
                return id;
            }
        }
    })
    .await
    .expect("slow consumer evicted within timeout");
    assert_eq!(evicted, stalled_id);

    // Control is untouched: no reconnect, no state transition.
    assert_eq!(relay.control_upgrades(), 1, "control socket survives");
    assert!(
        states.try_recv().is_err(),
        "no Offline/Connecting transition from data eviction"
    );

    // The reading peer keeps working.
    e2ee_client::send_sealed(&mut healthy, &healthy_shared, "still-here").await;
    assert_eq!(
        e2ee_client::recv_opened(&mut healthy, &healthy_shared).await,
        "still-here"
    );

    sender.abort();
    transport.stop().await;
}

#[tokio::test]
async fn inbound_plaintext_length_bound_closes_connection_beyond_limit() {
    let relay = MockRelay::start().await;
    let (sink, mut states, mut opens, mut closes) = echo_sink();
    // Production-scale ping/stale on the control socket: this test moves two
    // ~22 MB frames through one current-thread runtime, and the crypto while
    // sealing the echo can block the thread far past the usual 400 ms test
    // stale window — a control reconnect here is unrelated to the boundary
    // under test and would reconcile the data socket away.
    let timings = TransportTimings {
        ping_interval: Duration::from_secs(10),
        stale_timeout: Duration::from_secs(30),
        ..test_timings()
    };
    let (transport, identity) = transport_with_fresh_identity(&relay, sink, timings);
    let _ = recv_state(&mut states).await;
    let _ = recv_state(&mut states).await; // Online

    let (mut client, shared) = encrypted_client(&relay, &identity.public_key_b64).await;
    let id = recv_open(&mut opens).await;

    // Boundary pair on the plaintext axis: the wire cap is the b64 of the
    // plaintext cap (derivation in transport_limits), and at the quartet
    // boundary both WS_PHYSICAL_FRAME_BYTES and one byte over encode to the
    // *same* wire length — so only the post-decrypt check can reject the
    // over-cap frame (spec §2.7 second layer).
    let at_cap = "x".repeat(WS_PHYSICAL_FRAME_BYTES);
    let wire_at_cap = e2ee_client::seal_text(&shared, &at_cap);
    let wire_over = e2ee_client::seal_text(&shared, &format!("{at_cap}y"));
    assert_eq!(wire_at_cap.len(), wire_over.len());
    assert_eq!(wire_at_cap.len(), RELAY_DATA_FRAME_TEXT_BYTES);

    // At the cap exactly: decrypts, passes, echoes back sealed.
    client.send_text(&wire_at_cap).await;
    let echoed = client.recv_text().await;
    assert_eq!(echoed.len(), RELAY_DATA_FRAME_TEXT_BYTES);

    // One plaintext byte over: that data connection closes; control stays up.
    client.send_text(&wire_over).await;
    client.expect_close().await;
    assert_eq!(recv_close(&mut closes).await, id);
    assert_eq!(relay.control_upgrades(), 1, "control socket survives");

    transport.stop().await;
}

fn default_sleep() -> SleepFn {
    Arc::new(|duration: Duration| Box::pin(tokio::time::sleep(duration)))
}
