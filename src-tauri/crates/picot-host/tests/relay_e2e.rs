// ABOUTME: Relay E2EE + session-bridge integration (plan T5 slice b).
// ABOUTME: Real mock relay + real HostServer through the production bridge:
// ABOUTME: handshake gating, tamper eviction, per-connection keys, internal-id
// ABOUTME: registration, remote-only relay admission, handshake semaphore.

//! Every test drives the production stack end to end: the relay transport
//! (control + data sockets + E2EE termination), the `RelaySessionSink` bridge
//! out of `HostServer`, and `run_session` under `SessionTransport::Relay`.
//! The test client is a real `MockClientSocket` speaking the literal wire
//! frames from the spec tables. Device tokens are seeded straight through
//! `RemoteAuth`'s exchange machinery; the hello-embedded pairing path is
//! plan T7's deliverable.

#[path = "support/e2ee_client.rs"]
mod e2ee_client;
#[path = "support/mock_relay.rs"]
mod mock_relay;

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mock_relay::{MockClientSocket, MockRelay};
use picot_host::host_server::HostServer;
use picot_host::metadata_store::{MetadataStore, SharedMetadataStore};
use picot_host::native_pi_manager::NativePiManager;
use picot_host::relay::e2ee::DaemonIdentity;
use picot_host::relay::transport::{
    RelayConfig, RelaySessionSink, RelayTransport, RelayTransportHandle, TransportSeams,
    TransportTimings,
};
use picot_host::remote_auth::RemoteAuth;
use picot_host::transport_limits::WS_PHYSICAL_FRAME_BYTES;
use serde_json::{json, Value};

struct E2e {
    relay: MockRelay,
    host: HostServer,
    transport: RelayTransportHandle,
    identity: DaemonIdentity,
    device_token: String,
    temp: PathBuf,
}

fn e2e_timings() -> TransportTimings {
    TransportTimings {
        backoff_base: Duration::from_secs(1),
        backoff_cap: Duration::from_secs(30),
        ping_interval: Duration::from_millis(50),
        stale_timeout: Duration::from_millis(400),
        ready_timeout: Duration::from_millis(400),
        handshake_timeout: Duration::from_secs(2),
        data_open_timeout: Duration::from_secs(2),
        e2ee_handshake_timeout: Duration::from_secs(5),
    }
}

async fn e2e_setup(label: &str, e2ee_handshake_timeout: Duration) -> E2e {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "picot-relay-e2e2e-{label}-{}-{nonce}",
        std::process::id()
    ));
    let public = temp.join("public");
    fs::create_dir_all(&public).unwrap();
    fs::write(public.join("index.html"), "Picot").unwrap();
    let metadata: SharedMetadataStore = Arc::new(Mutex::new(
        MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
    ));
    let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
    // Seed the device token straight through the existing exchange machinery
    // (plan T5 step 1a: "do NOT use the pairing path — that is T7").
    let pairing = auth.lock().unwrap().create_pairing(1_000, None);
    let device_token = auth
        .lock()
        .unwrap()
        .exchange(&pairing.token, "relay-e2e-device", 1_001)
        .expect("seeded device token");

    let host = HostServer::start_with_session_root(
        public,
        NativePiManager::new(4),
        Arc::clone(&auth),
        Arc::clone(&metadata),
        Some(temp.join("sessions")),
    )
    .await
    .expect("host server starts");

    let relay = MockRelay::start().await;
    let identity = DaemonIdentity::for_tests();
    let sink: Arc<dyn RelaySessionSink> = host.relay_session_sink();
    let transport = RelayTransport::start_with_seams(
        RelayConfig {
            endpoint: relay.addr().to_string(),
            use_tls: false,
            server_id: "e2e-server".into(),
        },
        identity.clone(),
        sink,
        TransportSeams {
            timings: TransportTimings {
                e2ee_handshake_timeout,
                ..e2e_timings()
            },
            sleep: Arc::new(|duration: Duration| Box::pin(tokio::time::sleep(duration))),
            connector: None,
        },
    );
    E2e {
        relay,
        host,
        transport,
        identity,
        device_token,
        temp,
    }
}

/// Connect a client and wait until the daemon has dialed its data socket:
/// frames sent before the dial are dropped by the mock's pipe routing, so
/// every handshake must start after this point.
async fn connect_client(relay: &MockRelay) -> (MockClientSocket, String) {
    // The control socket must exist first: the mock's `connected` notify has
    // no control to deliver to before then, and a lost notify means no dial.
    for _ in 0..400 {
        if relay.control_upgrades() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        relay.control_upgrades() >= 1,
        "daemon control socket never established"
    );
    let before = relay.data_dials().len();
    let client = relay.connect_client().await;
    for _ in 0..400 {
        let dials = relay.data_dials();
        if dials.len() > before {
            let connection_id = dials.last().expect("dial recorded").1.clone();
            return (client, connection_id);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("daemon never dialed the data socket");
}

fn remote_hello(client_id: &str, device_token: &str) -> String {
    json!({
        "type": "hello",
        "protocolVersion": 2,
        "clientType": "remote",
        "clientId": client_id,
        "deviceToken": device_token,
    })
    .to_string()
}

async fn recv_json(client: &mut MockClientSocket, shared: &[u8; 32]) -> Value {
    let text = e2ee_client::recv_opened(client, shared).await;
    serde_json::from_str(&text).expect("session frames are JSON")
}

async fn teardown(e2e: E2e) {
    e2e.transport.stop().await;
    e2e.host.stop();
    fs::remove_dir_all(e2e.temp).unwrap();
}

/// (a) Full session: handshake → remote hello → hello_ack → one routed
/// request round-trip over the encrypted channel. The desktop-only
/// management plane rejecting the remote class is today's contract (Gate B
/// admission is plan T9); the round-trip proves the whole pipe.
#[tokio::test]
async fn handshake_then_full_session_round_trip_over_e2ee() {
    let e2e = e2e_setup("round-trip", Duration::from_secs(5)).await;
    let (mut client, _id) = connect_client(&e2e.relay).await;
    let shared = e2ee_client::handshake(&mut client, &e2e.identity.public_key_b64).await;

    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &remote_hello("phone-1", &e2e.device_token),
    )
    .await;
    let ack = recv_json(&mut client, &shared).await;
    assert_eq!(ack["type"], "hello_ack");
    assert_eq!(ack["protocolVersion"], 2);

    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &json!({
            "type": "host_request",
            "requestId": "r1",
            "operation": "preference_get",
        })
        .to_string(),
    )
    .await;
    let response = recv_json(&mut client, &shared).await;
    assert_eq!(response["requestId"], "r1", "response correlates");
    assert_eq!(response["error"]["code"], "forbidden_class");

    teardown(e2e).await;
}

/// (b) Any frame before `e2ee_ready` closes the connection and nothing
/// reaches the session pipeline (spec §2.5: zero processing pre-handshake).
#[tokio::test]
async fn pre_ready_frame_closes_the_data_connection() {
    let e2e = e2e_setup("pre-ready", Duration::from_secs(5)).await;
    let (mut client, _id) = connect_client(&e2e.relay).await;
    // A well-formed session hello sent before the E2EE handshake: the only
    // legal first frame is e2ee_hello.
    client.send_text(&remote_hello("early", "any-token")).await;
    client.expect_close().await;
    teardown(e2e).await;
}

/// (c) A client that never sends `e2ee_hello` is closed by the handshake
/// timeout (spec §2.5: 10s in production, 300ms here through the seam).
#[tokio::test]
async fn handshake_timeout_closes_the_connection() {
    let started = std::time::Instant::now();
    let e2e = e2e_setup("handshake-timeout", Duration::from_millis(300)).await;
    let (mut client, _id) = connect_client(&e2e.relay).await;
    client.expect_close().await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "close came from the handshake timeout, not a slower path"
    );
    teardown(e2e).await;
}

/// (d) A flipped ciphertext byte fails the MAC and closes the connection.
#[tokio::test]
async fn tampered_ciphertext_closes_the_connection() {
    let e2e = e2e_setup("tampered", Duration::from_secs(5)).await;
    let (mut client, _id) = connect_client(&e2e.relay).await;
    let shared = e2ee_client::handshake(&mut client, &e2e.identity.public_key_b64).await;

    // Valid bundle, then flip one MAC byte (offset 24) before base64.
    let bundle = picot_host::relay::e2ee::seal(&shared, b"{\"type\":\"host_request\"}");
    let mut tampered = bundle;
    tampered[24] ^= 0x01;
    use base64::Engine as _;
    let wire = base64::engine::general_purpose::STANDARD.encode(tampered);
    client.send_text(&wire).await;
    client.expect_close().await;

    teardown(e2e).await;
}

/// (e) Two clients hold independent channels: different shared keys, both
/// sessions live at once, and a frame sealed with the other's key cannot
/// open — the wrong-key frame evicts only the offending connection.
#[tokio::test]
async fn two_clients_get_independent_channels() {
    let e2e = e2e_setup("two-keys", Duration::from_secs(5)).await;
    let (mut a, _id_a) = connect_client(&e2e.relay).await;
    let shared_a = e2ee_client::handshake(&mut a, &e2e.identity.public_key_b64).await;
    let (mut b, _id_b) = connect_client(&e2e.relay).await;
    let shared_b = e2ee_client::handshake(&mut b, &e2e.identity.public_key_b64).await;
    assert_ne!(shared_a, shared_b, "per-connection keys never repeat");

    e2ee_client::send_sealed(&mut a, &shared_a, &remote_hello("a", &e2e.device_token)).await;
    assert_eq!(recv_json(&mut a, &shared_a).await["type"], "hello_ack");
    e2ee_client::send_sealed(&mut b, &shared_b, &remote_hello("b", &e2e.device_token)).await;
    assert_eq!(recv_json(&mut b, &shared_b).await["type"], "hello_ack");

    // Sealed with B's key on A's channel: A's connection closes, B lives on.
    e2ee_client::send_sealed(&mut a, &shared_b, &remote_hello("a", &e2e.device_token)).await;
    a.expect_close().await;
    e2ee_client::send_sealed(
        &mut b,
        &shared_b,
        &json!({"type": "host_request", "requestId": "r-b", "operation": "preference_get"})
            .to_string(),
    )
    .await;
    let response = recv_json(&mut b, &shared_b).await;
    assert_eq!(response["error"]["code"], "forbidden_class");

    teardown(e2e).await;
}

/// Plan T5(f), the three-ID model (spec §2.2): two sequential sessions
/// declaring the same `clientId` must not clobber each other's registration,
/// and the stale one's disconnect must not unregister the live one.
#[tokio::test]
async fn same_declared_client_id_sessions_do_not_clobber_each_other() {
    let e2e = e2e_setup("id-separation", Duration::from_secs(5)).await;
    let (mut first, _id) = connect_client(&e2e.relay).await;
    let shared_first = e2ee_client::handshake(&mut first, &e2e.identity.public_key_b64).await;
    e2ee_client::send_sealed(
        &mut first,
        &shared_first,
        &remote_hello("c1", &e2e.device_token),
    )
    .await;
    assert_eq!(
        recv_json(&mut first, &shared_first).await["type"],
        "hello_ack"
    );

    // Same declared id over a fresh channel: registers beside, not over.
    let (mut second, _id2) = connect_client(&e2e.relay).await;
    let shared_second = e2ee_client::handshake(&mut second, &e2e.identity.public_key_b64).await;
    e2ee_client::send_sealed(
        &mut second,
        &shared_second,
        &remote_hello("c1", &e2e.device_token),
    )
    .await;
    assert_eq!(
        recv_json(&mut second, &shared_second).await["type"],
        "hello_ack"
    );

    // The first session still routes (a clobbered registration would answer
    // unauthorized_client instead of the class rejection).
    e2ee_client::send_sealed(
        &mut first,
        &shared_first,
        &json!({"type": "host_request", "requestId": "r-first", "operation": "preference_get"})
            .to_string(),
    )
    .await;
    let response = recv_json(&mut first, &shared_first).await;
    assert_eq!(response["error"]["code"], "forbidden_class");

    // Stale disconnect removes only its own internal-id registration.
    second.close().await;
    e2ee_client::send_sealed(
        &mut first,
        &shared_first,
        &json!({"type": "host_request", "requestId": "r-after", "operation": "preference_get"})
            .to_string(),
    )
    .await;
    let response = recv_json(&mut first, &shared_first).await;
    assert_eq!(
        response["error"]["code"], "forbidden_class",
        "live session survives the stale disconnect"
    );

    teardown(e2e).await;
}

/// Plan T5(g): a desktop-typed hello over relay is rejected regardless of
/// any capability fields — the local owner path is loopback only (D2).
#[tokio::test]
async fn forged_desktop_hello_over_relay_is_rejected() {
    let e2e = e2e_setup("forged-desktop", Duration::from_secs(5)).await;
    let (mut client, _id) = connect_client(&e2e.relay).await;
    let shared = e2ee_client::handshake(&mut client, &e2e.identity.public_key_b64).await;

    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &json!({
            "type": "hello",
            "protocolVersion": 2,
            "clientType": "desktop",
            "clientId": "c1",
            "desktopCapability": "forged-capability",
        })
        .to_string(),
    )
    .await;
    let error = recv_json(&mut client, &shared).await;
    assert_eq!(error["error"]["code"], "invalid_client_type");
    client.expect_close().await;

    teardown(e2e).await;
}

/// Plan T5(i): at most 4 concurrent E2EE handshakes (spec §2.7). Four
/// clients park inside the handshake (dialed, no hello sent) holding all
/// permits; the fifth's hello cannot be answered until one holder leaves.
#[tokio::test]
async fn fifth_handshake_waits_for_a_free_permit() {
    let e2e = e2e_setup("permits", Duration::from_secs(8)).await;
    let mut holders: Vec<MockClientSocket> = Vec::new();
    for _ in 0..4 {
        let (client, _id) = connect_client(&e2e.relay).await;
        holders.push(client);
    }
    let (mut fifth, _id) = connect_client(&e2e.relay).await;
    let fifth_keys = e2ee_client::client_keys();
    fifth
        .send_text(&e2ee_client::hello_frame(&fifth_keys))
        .await;
    assert!(
        fifth
            .maybe_recv_text(Duration::from_millis(600))
            .await
            .is_none(),
        "no fifth concurrent handshake while four hold the permits"
    );

    // One holder leaves → its data connection tears down → the permit frees
    // → the queued fifth completes.
    holders.pop().expect("four holders").close().await;
    let ready = fifth.recv_text().await;
    assert_eq!(ready, "{\"type\":\"e2ee_ready\"}");

    // The channel is fully usable afterwards.
    let shared = e2ee_client::shared_key(&fifth_keys, &e2e.identity.public_key_b64);
    e2ee_client::send_sealed(
        &mut fifth,
        &shared,
        &remote_hello("fifth", &e2e.device_token),
    )
    .await;
    assert_eq!(recv_json(&mut fifth, &shared).await["type"], "hello_ack");

    teardown(e2e).await;
}

/// Decrypted plaintext beyond `WS_PHYSICAL_FRAME_BYTES` closes the
/// connection even when the wire text sits exactly at the b64 cap (the
/// explicit second layer from spec §2.7 / plan T5).
#[tokio::test]
async fn plaintext_over_the_physical_cap_closes_the_connection() {
    let e2e = e2e_setup("oversize-plaintext", Duration::from_secs(5)).await;
    let (mut client, _id) = connect_client(&e2e.relay).await;
    let shared = e2ee_client::handshake(&mut client, &e2e.identity.public_key_b64).await;

    // Plaintext exactly at the cap passes (both wire and plaintext checks
    // use <=); one byte over sits at the *same* wire length (b64 quartet
    // boundary) and must be rejected by the post-decrypt check.
    let at_cap = "x".repeat(WS_PHYSICAL_FRAME_BYTES);
    let wire_at_cap = e2ee_client::seal_text(&shared, &at_cap);
    client.send_text(&wire_at_cap).await;
    let over_cap = format!("{}y", at_cap);
    let wire_over = e2ee_client::seal_text(&shared, &over_cap);
    assert_eq!(
        wire_at_cap.len(),
        wire_over.len(),
        "the boundary pair differs only after decryption"
    );
    client.send_text(&wire_over).await;
    client.expect_close().await;

    teardown(e2e).await;
}
