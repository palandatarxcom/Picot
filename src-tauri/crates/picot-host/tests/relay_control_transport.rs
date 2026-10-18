// ABOUTME: Relay control-transport contract tests against a mock relay (plan T4 slice 1).
// ABOUTME: Control socket only: online/offline, sync reconcile, backoff, stale,
// ABOUTME: ready timeout, frame bounds. Data sockets + E2EE are T5.

//! Every assertion runs on injected seams, never wall-clock luck (plan T4
//! risk: "all timing asserts must run on injected clocks/delay"): reconnect
//! backoff durations are *recorded* by the sleep seam and asserted as the
//! spec §2.5 sequence; the ping/stale/ready timers run on injected short
//! durations with generous await timeouts.

#[path = "support/mock_relay.rs"]
mod mock_relay;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mock_relay::{MockRelay, SilentMode};
use picot_host::relay::e2ee::DaemonIdentity;
use picot_host::relay::transport::{
    ConnectionState, RelayConfig, RelaySessionSink, RelayTransport, SleepFn, TransportSeams,
    TransportTimings,
};
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
    async fn open_session(&self, connection_id: String) {
        let _ = self.opens.send(connection_id);
    }
    async fn close_session(&self, connection_id: &str) {
        let _ = self.closes.send(connection_id.to_string());
    }
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
    }
}

fn test_identity() -> DaemonIdentity {
    DaemonIdentity::for_tests()
}

fn start_transport(
    relay: &MockRelay,
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
        test_identity(),
        sink,
        TransportSeams {
            timings,
            sleep,
            connector: None,
        },
    )
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
    let transport = start_transport(&relay, sink, test_timings(), default_sleep());

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
    let transport = start_transport(&relay, sink, test_timings(), default_sleep());
    let _ = recv_state(&mut states).await;
    let _ = recv_state(&mut states).await; // Online

    // sync reconcile: unknown ids open, missing ids close (spec §2.5).
    relay.send_control("{\"type\":\"sync\",\"connectionIds\":[\"c1\",\"c2\"]}");
    assert_eq!(recv_open(&mut opens).await, "c1");
    assert_eq!(recv_open(&mut opens).await, "c2");
    relay.send_control("{\"type\":\"sync\",\"connectionIds\":[\"c1\"]}");
    assert_eq!(recv_close(&mut closes).await, "c2");

    // connected/disconnected deltas; duplicate connected is idempotent.
    let client = relay.connect_client().await;
    let id = recv_open(&mut opens).await;
    relay.send_control(&format!(
        "{{\"type\":\"connected\",\"connectionId\":\"{id}\"}}"
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        opens.try_recv().is_err(),
        "duplicate connected opens nothing"
    );
    client.close().await;
    assert_eq!(recv_close(&mut closes).await, id);

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
    let transport = start_transport(&relay, sink, test_timings(), default_sleep());
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
    let transport = start_transport(&relay, sink, test_timings(), default_sleep());
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
    let transport = start_transport(&relay, sink, test_timings(), recorder.seam());

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
    let transport = start_transport(&relay, sink, test_timings(), default_sleep());

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
    let transport = start_transport(&relay, sink, test_timings(), default_sleep());

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
    let transport = start_transport(&relay, sink, test_timings(), default_sleep());
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

fn default_sleep() -> SleepFn {
    Arc::new(|duration: Duration| Box::pin(tokio::time::sleep(duration)))
}
