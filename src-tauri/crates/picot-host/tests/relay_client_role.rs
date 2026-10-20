// ABOUTME: Mock-relay contract tests for the client-role transport (plan T2).
// ABOUTME: Bytes, resend cadence, frame bounds, half-open, backoff, stop/join
// ABOUTME: — the wire shapes are the literal spec frames, never round-trips.

//! `relay/client_role.rs` is pure transport (spec §5.2 contract 3): an E2EE
//! client handshake, a bounded plaintext pipe, half-open detection, and
//! backoff redial — no application-level hello, no pairing, no credentials.
//! The peer here is a scripted remote daemon: it dials the
//! `role=server&connectionId=` side of the mock's splice and speaks the
//! literal `e2ee_ready` frame.

#[path = "support/mock_relay.rs"]
mod mock_relay;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use mock_relay::{MockClientSocket, MockRelay};
use picot_host::relay::client_role::{
    ClientChannel, ClientRole, ClientRoleConfig, ClientRoleHandle, ClientRoleSeams, ClientRoleSink,
    ClientRoleTimings,
};
use picot_host::relay::e2ee;
use picot_host::relay::transport::{ConnectionState, SleepFn};
use picot_host::transport_limits::{RELAY_DATA_FRAME_TEXT_BYTES, WS_PHYSICAL_FRAME_BYTES};
use tokio::sync::mpsc;

struct RecordingSink {
    states: mpsc::UnboundedSender<ConnectionState>,
    frames: mpsc::UnboundedSender<String>,
    online: mpsc::UnboundedSender<ClientChannel>,
}

#[async_trait]
impl ClientRoleSink for RecordingSink {
    async fn on_state(&self, state: ConnectionState) {
        let _ = self.states.send(state);
    }
    async fn on_online(&self, channel: ClientChannel) {
        let _ = self.online.send(channel);
    }
    async fn on_frame(&self, payload: String) {
        let _ = self.frames.send(payload);
    }
}

struct Client {
    handle: ClientRoleHandle,
    states: mpsc::UnboundedReceiver<ConnectionState>,
    frames: mpsc::UnboundedReceiver<String>,
    online: mpsc::UnboundedReceiver<ClientChannel>,
}

fn fast_timings() -> ClientRoleTimings {
    ClientRoleTimings {
        dial_timeout: Duration::from_secs(2),
        pre_hello_delay: Duration::from_millis(10),
        hello_resend_after: Duration::from_millis(50),
        hello_max_sends: 3,
        handshake_timeout: Duration::from_millis(400),
        idle_timeout: Duration::from_secs(2),
        ping_interval: Duration::from_secs(1),
        backoff_base: Duration::from_secs(1),
        backoff_cap: Duration::from_secs(30),
    }
}

fn start_client(relay: &MockRelay, daemon_public_b64: &str, seams: ClientRoleSeams) -> Client {
    let (state_tx, states) = mpsc::unbounded_channel();
    let (frame_tx, frames) = mpsc::unbounded_channel();
    let (online_tx, online) = mpsc::unbounded_channel();
    let sink = Arc::new(RecordingSink {
        states: state_tx,
        frames: frame_tx,
        online: online_tx,
    });
    let handle = ClientRole::start_with_seams(
        ClientRoleConfig {
            endpoint: relay.addr().to_string(),
            use_tls: false,
            server_id: "client-role-server".into(),
            peer_public_key_b64: daemon_public_b64.to_string(),
        },
        sink,
        seams,
    );
    Client {
        handle,
        states,
        frames,
        online,
    }
}

fn daemon_keys() -> (crypto_box::SecretKey, String) {
    let secret = crypto_box::SecretKey::generate(&mut rand::rngs::OsRng);
    let public_b64 = BASE64_STANDARD.encode(secret.public_key().as_bytes());
    (secret, public_b64)
}

fn seams(timings: ClientRoleTimings) -> ClientRoleSeams {
    ClientRoleSeams {
        timings,
        ..ClientRoleSeams::default()
    }
}

/// The remote-daemon side of one relay session: dials the paired server-side
/// socket, answers the hello, keeps the shared key.
struct Peer {
    socket: MockClientSocket,
    shared: [u8; 32],
}

async fn peer_accept(
    relay: &MockRelay,
    connection_id: &str,
    daemon_secret: &crypto_box::SecretKey,
) -> Peer {
    let mut socket = relay.connect_data(connection_id).await;
    // The relay drops client frames sent before this dial exists, so the
    // client resends; the first hello that lands is the one asserted.
    let hello = socket.recv_text().await;
    let key = hello_key(&hello);
    let shared = e2ee::derive_shared_key(&daemon_secret.to_bytes(), &key)
        .expect("daemon derives the shared key");
    socket.send_text("{\"type\":\"e2ee_ready\"}").await;
    Peer { socket, shared }
}

/// Assert the hello is byte-exactly `{"type":"e2ee_hello","key":…}` — no
/// capabilities field, base64-only v1 (spec §2.4/§12) — and return the key.
fn hello_key(hello: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(hello).expect("hello is JSON");
    let key = value["key"]
        .as_str()
        .expect("hello carries a key")
        .to_string();
    assert_eq!(
        hello,
        format!("{{\"type\":\"e2ee_hello\",\"key\":\"{key}\"}}"),
        "e2ee_hello must be byte-exact with no extra fields"
    );
    assert_eq!(
        BASE64_STANDARD.decode(&key).expect("key is base64").len(),
        32,
        "an X25519 public key is 32 bytes"
    );
    key
}

fn seal(shared: &[u8; 32], plaintext: &str) -> String {
    BASE64_STANDARD.encode(e2ee::seal(shared, plaintext.as_bytes()))
}

fn open(shared: &[u8; 32], wire: &str) -> String {
    let bundle = BASE64_STANDARD.decode(wire).expect("wire is base64");
    let plaintext = e2ee::open(shared, &bundle).expect("frame decrypts");
    String::from_utf8(plaintext).expect("plaintext is UTF-8")
}

async fn wait_for_client(relay: &MockRelay) -> String {
    for _ in 0..250 {
        if let Some(id) = relay.client_ids().into_iter().next() {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the client transport never dialed the relay");
}

/// The next client-role connection that is not in `seen` — a redial.
async fn wait_next_client(relay: &MockRelay, seen: &[String]) -> String {
    for _ in 0..250 {
        if let Some(id) = relay.client_ids().into_iter().find(|id| !seen.contains(id)) {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no redial after {seen:?}");
}

async fn wait_for_redial(relay: &MockRelay, at_least: u32) {
    for _ in 0..250 {
        if relay.client_upgrades() >= at_least {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "expected {at_least} client dials, saw {}",
        relay.client_upgrades()
    );
}

/// Pure transport: the handshake is byte-exact, no application-level hello
/// follows it, and plaintext frames flow both ways.
#[tokio::test]
async fn client_role_handshake_is_byte_exact_and_pipes_plaintext_frames() {
    let relay = MockRelay::start().await;
    let (daemon_secret, daemon_public) = daemon_keys();
    let mut client = start_client(&relay, &daemon_public, seams(fast_timings()));

    assert_eq!(
        client.states.recv().await,
        Some(ConnectionState::Connecting),
        "the transport starts Connecting"
    );
    let id = wait_for_client(&relay).await;
    let mut peer = peer_accept(&relay, &id, &daemon_secret).await;
    assert_eq!(client.states.recv().await, Some(ConnectionState::Online));
    let channel = client.online.recv().await.expect("online channel");

    // Contract 3: after the handshake a pure transport sends nothing — the
    // application-level hello belongs to host_client / the bridge.
    assert!(
        peer.socket
            .maybe_recv_text(Duration::from_millis(200))
            .await
            .is_none(),
        "no application-level hello on a pure transport session"
    );

    // Remote daemon → client.
    peer.socket
        .send_text(&seal(&peer.shared, "{\"type\":\"data_frame\"}"))
        .await;
    assert_eq!(
        client.frames.recv().await.as_deref(),
        Some("{\"type\":\"data_frame\"}")
    );

    // Client → remote daemon.
    assert!(channel.send("{\"type\":\"request_frame\"}".into()));
    let wire = peer.socket.recv_text().await;
    assert_eq!(open(&peer.shared, &wire), "{\"type\":\"request_frame\"}");

    client.handle.stop().await;
}

/// Resend contract: `e2ee_hello` repeats on 1s-cadence silence up to three
/// sends, then the 15s deadline gives up and the transport redials.
#[tokio::test]
async fn client_role_resends_the_hello_three_times_then_gives_up_at_the_deadline() {
    let relay = MockRelay::start().await;
    let (_, daemon_public) = daemon_keys();
    let timings = ClientRoleTimings {
        // Shrunk curve: base is injectable, so the redial is observable fast.
        backoff_base: Duration::from_millis(20),
        ..fast_timings()
    };
    let mut client = start_client(&relay, &daemon_public, seams(timings));

    let id = wait_for_client(&relay).await;
    // The peer answers nothing: every hello stays unanswered.
    let mut peer = relay.connect_data(&id).await;
    assert_eq!(
        client.states.recv().await,
        Some(ConnectionState::Connecting)
    );
    // Frames sent before this dial exists are dropped by the relay, so the
    // peer observes two or three of the three sends — never a fourth.
    let mut hellos = Vec::new();
    while let Some(hello) = peer.next_text_or_close(Duration::from_millis(600)).await {
        hello_key(&hello);
        hellos.push(hello);
    }
    assert!(
        (2..=3).contains(&hellos.len()),
        "resend on silence, capped at three sends: got {}",
        hellos.len()
    );
    assert!(
        hellos.windows(2).all(|pair| pair[0] == pair[1]),
        "resends repeat the same ephemeral key"
    );
    // Deadline elapsed → the connection closed (read above) and it redialed.
    assert_eq!(client.states.recv().await, Some(ConnectionState::Offline));
    wait_for_redial(&relay, 2).await;

    client.handle.stop().await;
}

/// Bounds (spec §5.2 R2-10): a wire-cap overflow, a post-decrypt overflow,
/// truncated and tampered ciphertext each close only that connection.
#[tokio::test]
async fn client_role_bounds_frames_and_closes_only_the_violating_connection() {
    let relay = MockRelay::start().await;
    let (daemon_secret, daemon_public) = daemon_keys();
    let timings = ClientRoleTimings {
        backoff_base: Duration::from_millis(20),
        ..fast_timings()
    };
    let mut client = start_client(&relay, &daemon_public, seams(timings));

    let at_cap_plaintext = vec![b'x'; WS_PHYSICAL_FRAME_BYTES + 1];
    let build_over_wire = |_: &[u8; 32]| "x".repeat(RELAY_DATA_FRAME_TEXT_BYTES + 1);
    let build_over_plaintext =
        move |shared: &[u8; 32]| BASE64_STANDARD.encode(e2ee::seal(shared, &at_cap_plaintext));
    let build_truncated = |shared: &[u8; 32]| {
        let bundle = e2ee::seal(shared, b"{\"type\":\"tiny\"}");
        BASE64_STANDARD.encode(&bundle[..20])
    };
    let build_tampered = |shared: &[u8; 32]| {
        let mut bundle = e2ee::seal(shared, b"{\"type\":\"tiny\"}");
        let last = bundle.len() - 1;
        bundle[last] ^= 0xff;
        BASE64_STANDARD.encode(bundle)
    };
    type FrameBuilder = Box<dyn Fn(&[u8; 32]) -> String>;
    let cases: Vec<(&str, FrameBuilder)> = vec![
        ("post-decrypt over cap", Box::new(build_over_plaintext)),
        ("truncated ciphertext", Box::new(build_truncated)),
        ("tampered ciphertext", Box::new(build_tampered)),
    ];

    let mut seen = Vec::new();
    for (label, build) in cases {
        let id = if seen.is_empty() {
            wait_for_client(&relay).await
        } else {
            wait_next_client(&relay, &seen).await
        };
        seen.push(id.clone());
        let mut peer = peer_accept(&relay, &id, &daemon_secret).await;
        peer.socket.send_text(&build(&peer.shared)).await;
        peer.socket.expect_close().await;
        assert!(
            client.frames.try_recv().is_err(),
            "{label}: no plaintext reaches the sink"
        );
    }

    // Wire-level over-cap: the socket's own read cap rejects it before any
    // ciphertext parsing (the explicit check is the documented second layer).
    let id = wait_next_client(&relay, &seen).await;
    seen.push(id.clone());
    let mut peer = peer_accept(&relay, &id, &daemon_secret).await;
    peer.socket.send_text(&build_over_wire(&peer.shared)).await;
    peer.socket.expect_close().await;
    assert!(client.frames.try_recv().is_err());

    client.handle.stop().await;
}

/// Half-open (spec §5.2 R2-10): a peer that stops answering (no WS pong) is
/// closed after the idle window and redialed.
#[tokio::test]
async fn client_role_detects_half_open_silence_and_redials() {
    let relay = MockRelay::start().await;
    let (daemon_secret, daemon_public) = daemon_keys();
    let timings = ClientRoleTimings {
        idle_timeout: Duration::from_millis(200),
        ping_interval: Duration::from_millis(50),
        backoff_base: Duration::from_millis(20),
        ..fast_timings()
    };
    let mut client = start_client(&relay, &daemon_public, seams(timings));
    assert_eq!(
        client.states.recv().await,
        Some(ConnectionState::Connecting)
    );

    let id = wait_for_client(&relay).await;
    let _peer = peer_accept(&relay, &id, &daemon_secret).await;
    assert_eq!(client.states.recv().await, Some(ConnectionState::Online));
    // A quiet but responsive relay keeps the session alive via WS pongs.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        client.states.try_recv().is_err(),
        "responsive relay stays Online"
    );
    // Stop reading the established client socket, not the paired daemon
    // socket: the relay itself automatically answers the client's WS pings.
    relay.set_client_park(true);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), client.states.recv())
            .await
            .expect("silent session must go Offline within the idle budget"),
        Some(ConnectionState::Offline)
    );
    wait_for_redial(&relay, 2).await;
    assert!(
        client.frames.try_recv().is_err(),
        "a silent peer delivers no frames"
    );

    client.handle.stop().await;
}

/// Relay drop → redial on the `control_backoff` curve (`min(30s, 1s × n)`),
/// with the sleep injected so the curve is asserted, not waited out.
#[tokio::test]
async fn client_role_redials_on_the_control_backoff_curve() {
    let relay = MockRelay::start().await;
    let (_, daemon_public) = daemon_keys();
    let (sleep_tx, mut sleeps) = mpsc::unbounded_channel();
    let sleep: SleepFn = Arc::new(move |duration: Duration| {
        let sleep_tx = sleep_tx.clone();
        Box::pin(async move {
            let _ = sleep_tx.send(duration);
        })
    });
    let seams = ClientRoleSeams {
        timings: ClientRoleTimings {
            // Shrunk handshake budget: no peer ever answers here.
            handshake_timeout: Duration::from_millis(120),
            ..fast_timings()
        },
        sleep,
        connector: None,
    };
    let mut client = start_client(&relay, &daemon_public, seams);

    assert_eq!(
        client.states.recv().await,
        Some(ConnectionState::Connecting)
    );
    assert_eq!(
        sleeps.recv().await,
        Some(Duration::from_secs(1)),
        "attempt 1 → base"
    );
    assert_eq!(
        sleeps.recv().await,
        Some(Duration::from_secs(2)),
        "attempt 2 → 2 × base"
    );
    assert_eq!(
        sleeps.recv().await,
        Some(Duration::from_secs(3)),
        "attempt 3 → 3 × base"
    );
    assert_eq!(client.states.recv().await, Some(ConnectionState::Offline));
    wait_for_redial(&relay, 2).await;

    client.handle.stop().await;
}

/// Handle contract (spec §5.2 R2-05): `stop` cancels an in-flight handshake
/// and joins without waiting out the handshake budget, and nothing redials.
#[tokio::test]
async fn client_role_stop_cancels_an_in_flight_handshake() {
    let relay = MockRelay::start().await;
    let (_, daemon_public) = daemon_keys();
    let timings = ClientRoleTimings {
        handshake_timeout: Duration::from_secs(30),
        ..fast_timings()
    };
    let client = start_client(&relay, &daemon_public, seams(timings));

    let id = wait_for_client(&relay).await;
    // The peer holds the socket open and never answers: the handshake sits
    // in flight for the whole (30s) budget.
    let mut peer = relay.connect_data(&id).await;
    let _hello = peer.recv_text().await;

    let handle = client.handle;
    tokio::time::timeout(Duration::from_secs(2), handle.stop())
        .await
        .expect("stop must cancel the in-flight handshake, not wait it out");
    let dials = relay.client_upgrades();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        relay.client_upgrades(),
        dials,
        "a stopped transport never redials"
    );
}
