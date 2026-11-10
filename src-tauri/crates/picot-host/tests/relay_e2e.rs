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
use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::{DaemonConfig, DaemonState};
use picot_host::host_server::HostServer;
use picot_host::metadata_store::{MetadataStore, SharedMetadataStore};
use picot_host::native_pi_manager::NativePiManager;
use picot_host::relay::e2ee::DaemonIdentity;
use picot_host::relay::transport::{
    RelayConfig, RelaySessionSink, RelayTransport, RelayTransportHandle, TransportSeams,
    TransportTimings,
};
use picot_host::remote_auth::{DeviceView, RemoteAuth};
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::terminal_manager::TerminalManager;
use picot_host::transport_limits::WS_PHYSICAL_FRAME_BYTES;
use serde_json::{json, Value};

struct E2e {
    relay: MockRelay,
    host: HostServer,
    transport: RelayTransportHandle,
    identity: DaemonIdentity,
    device_token: String,
    device_token_b: String,
    temp: PathBuf,
}

/// The paired owner's live workspace + runtime the remote data plane rides
/// (Gate B): one in-memory runtime owned by a registered desktop owner, and
/// a responder task answering every runtime request the session dispatches.
fn seed_owner_runtime(
    temp: &std::path::Path,
    runtimes: &picot_host::native_pi_manager::NativePiManager,
) -> (Arc<picot_host::window_owner::WindowOwnerRegistry>, String) {
    let registry = Arc::new(picot_host::window_owner::WindowOwnerRegistry::default());
    fs::create_dir_all(temp.join("ws")).unwrap();
    let (owner, _capability) = registry
        .create_owner_with_workspace(
            "relay-e2e-window".into(),
            temp.join("ws").canonicalize().unwrap(),
            0,
            "http://127.0.0.1:1".into(),
            Some("relay-e2e-workspace".into()),
            picot_host::window_owner::TemporaryKind::DefaultStartup,
        )
        .unwrap();
    let target = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
        "relay-e2e-workspace",
        "relay-e2e-session",
        "relay-e2e-instance",
        owner.as_str(),
        0,
    );
    let mut fake = runtimes.register_in_memory(target).unwrap();
    tokio::spawn(async move {
        while let Some(request) = fake.read_request().await {
            let _ = fake
                .write_frame(serde_json::json!({
                    "id": request["id"],
                    "type": "response",
                    "success": true,
                    "data": { "sessionId": "relay-e2e-session" },
                }))
                .await;
        }
    });
    (registry, owner.as_str().to_owned())
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

/// Timings for the long real-PTY fixture. It runs a real shell plus the whole
/// daemon stack on the test's single-thread runtime, so a loaded machine (other
/// tests of this file in parallel, or the workspace test run) can miss the
/// deliberately tight 400ms liveness window used by the short-lived relay tests.
/// That window is a *test speed* knob, not a behavior under test here, and a
/// genuine hang still fails on the mock's own 5s frame deadline.
fn pty_timings() -> TransportTimings {
    TransportTimings {
        ping_interval: Duration::from_millis(500),
        stale_timeout: Duration::from_secs(10),
        ready_timeout: Duration::from_secs(10),
        handshake_timeout: Duration::from_secs(10),
        data_open_timeout: Duration::from_secs(10),
        e2ee_handshake_timeout: Duration::from_secs(10),
        ..e2e_timings()
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
    let runtimes = NativePiManager::new(4);
    // The paired owner's live runtime (Gate B data plane) plus an
    // owner-stamped device token.
    let (registry, owner) = seed_owner_runtime(&temp, &runtimes);
    let pairing = auth
        .lock()
        .unwrap()
        .create_pairing(1_000, Some(owner.as_str()));
    let device_token = auth
        .lock()
        .unwrap()
        .exchange(&pairing.token, "relay-e2e-device", 1_001)
        .expect("seeded device token");
    let pairing_b = auth
        .lock()
        .unwrap()
        .create_pairing(1_500, Some(owner.as_str()));
    let device_token_b = auth
        .lock()
        .unwrap()
        .exchange(&pairing_b.token, "relay-e2e-device-b", 1_501)
        .expect("seeded second device token");

    let host = HostServer::start_with_session_root(
        public,
        runtimes,
        Arc::clone(&auth),
        Arc::clone(&metadata),
        Some(temp.join("sessions")),
    )
    .await
    .expect("host server starts");
    host.set_owner_registry(registry);

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
        device_token_b,
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

/// Poll until the daemon reports `want` live sessions of `device`.
async fn device_sessions_reach(e2e: &E2e, device: &str, want: usize, what: &str) {
    for _ in 0..400 {
        if e2e.host.remote_device_sessions_for_tests(device) == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{what}: session count never reached {want}");
}

/// T9-C lifecycle check (plan T5(d)): the remote session's own teardown —
/// the client disconnects, the transport tears the daemon-side data socket
/// down, and the session task observes the ended stream and unregisters. No
/// session survives its connection.
#[tokio::test]
async fn client_disconnect_unregisters_the_relay_session() {
    let _serial = serial_guard().await;
    let e2e = e2e_setup("disconnect-cleanup", Duration::from_secs(5)).await;
    let (mut client, _id) = connect_client(&e2e.relay).await;
    let shared = e2ee_client::handshake(&mut client, &e2e.identity.public_key_b64).await;
    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &remote_hello("cleanup-1", &e2e.device_token),
    )
    .await;
    assert_eq!(recv_json(&mut client, &shared).await["type"], "hello_ack");
    device_sessions_reach(&e2e, "relay-e2e-device", 1, "the session registered").await;

    // Client gone: the mock severs the daemon's data socket, the inbound
    // stream ends, run_session returns, and the lease drop unregisters.
    client.close().await;
    device_sessions_reach(
        &e2e,
        "relay-e2e-device",
        0,
        "the session unregistered after the disconnect",
    )
    .await;

    teardown(e2e).await;
}

/// T10 (spec §2.9): revoking a paired device while it is connected tears
/// its live relay session down (the E2EE data connection closes mid-session),
/// refuses a reconnect with the same device token at hello, and leaves a
/// second paired device connected.
#[tokio::test]
async fn revoking_a_device_kicks_its_live_relay_sessions() {
    let _serial = serial_guard().await;
    let e2e = e2e_setup("revoke-kick", Duration::from_secs(5)).await;
    let (mut revoked, _id_a) = connect_client(&e2e.relay).await;
    let revoked_shared = e2ee_client::handshake(&mut revoked, &e2e.identity.public_key_b64).await;
    e2ee_client::send_sealed(
        &mut revoked,
        &revoked_shared,
        &remote_hello("revoke-a", &e2e.device_token),
    )
    .await;
    assert_eq!(
        recv_json(&mut revoked, &revoked_shared).await["type"],
        "hello_ack"
    );
    let (mut survivor, _id_b) = connect_client(&e2e.relay).await;
    let survivor_shared = e2ee_client::handshake(&mut survivor, &e2e.identity.public_key_b64).await;
    e2ee_client::send_sealed(
        &mut survivor,
        &survivor_shared,
        &remote_hello("revoke-b", &e2e.device_token_b),
    )
    .await;
    assert_eq!(
        recv_json(&mut survivor, &survivor_shared).await["type"],
        "hello_ack"
    );
    device_sessions_reach(&e2e, "relay-e2e-device", 1, "device a registered").await;
    device_sessions_reach(&e2e, "relay-e2e-device-b", 1, "device b registered").await;

    e2e.host
        .revoke_remote_device("relay-e2e-device")
        .expect("revoke succeeds");

    // The revoked device's data connection tears down mid-session; a
    // follow-up read/write on it is impossible (socket closed).
    revoked.expect_close().await;
    device_sessions_reach(
        &e2e,
        "relay-e2e-device",
        0,
        "the kicked session unregistered",
    )
    .await;

    // The second paired device still round-trips on its own channel.
    e2ee_client::send_sealed(
        &mut survivor,
        &survivor_shared,
        &json!({
            "type": "host_request", "requestId": "surv-1", "operation": "preference_get"
        })
        .to_string(),
    )
    .await;
    let response = recv_json(&mut survivor, &survivor_shared).await;
    assert_eq!(response["error"]["code"], "forbidden_class", "{response}");

    // A reconnect with the revoked device token is refused at hello.
    let (mut again, _id_c) = connect_client(&e2e.relay).await;
    let again_shared = e2ee_client::handshake(&mut again, &e2e.identity.public_key_b64).await;
    e2ee_client::send_sealed(
        &mut again,
        &again_shared,
        &remote_hello("revoke-a", &e2e.device_token),
    )
    .await;
    let rejected = recv_json(&mut again, &again_shared).await;
    assert_eq!(
        rejected["error"]["code"], "unauthorized_device",
        "{rejected}"
    );
    again.expect_close().await;
    device_sessions_reach(&e2e, "relay-e2e-device-b", 1, "device b still live").await;

    teardown(e2e).await;
}

/// (a) Full session: handshake → remote hello → hello_ack → one routed
/// request round-trip over the encrypted channel. The desktop-only
/// management plane rejecting the remote class is today's contract (Gate B
/// admission is plan T9); the round-trip proves the whole pipe.
#[tokio::test]
async fn handshake_then_full_session_round_trip_over_e2ee() {
    let _serial = serial_guard().await;
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

/// (a2) Gate B positive business round-trip (plan T9 step 1): a paired
/// device rides its owner's data plane end to end — subscribe, runtime
/// request, snapshot, workspace-bound data op — over the E2EE relay channel.
#[tokio::test]
async fn paired_device_rides_the_owner_data_plane_over_the_relay() {
    let _serial = serial_guard().await;
    let e2e = e2e_setup("gate-b", Duration::from_secs(5)).await;
    let (mut client, _id) = connect_client(&e2e.relay).await;
    let shared = e2ee_client::handshake(&mut client, &e2e.identity.public_key_b64).await;

    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &remote_hello("gate-b-phone", &e2e.device_token),
    )
    .await;
    let ack = recv_json(&mut client, &shared).await;
    assert_eq!(ack["type"], "hello_ack");

    let wire_target = json!({
        "workspaceId": "relay-e2e-workspace",
        "sessionId": "relay-e2e-session",
        "instanceId": "relay-e2e-instance"
    });
    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &json!({
            "type": "runtime_subscribe", "requestId": "sub", "target": wire_target
        })
        .to_string(),
    )
    .await;
    let subscribed = recv_json(&mut client, &shared).await;
    assert_eq!(subscribed["type"], "runtime_subscribed", "{subscribed}");

    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &json!({
            "type": "runtime_request", "requestId": "cmd-1", "target": wire_target,
            "idempotencyKey": "cmd-1-key", "command": { "type": "get_state" }
        })
        .to_string(),
    )
    .await;
    let reply = recv_json(&mut client, &shared).await;
    assert_eq!(reply["type"], "runtime_response", "{reply}");

    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &json!({
            "type": "runtime_snapshot_request", "requestId": "snap-1", "target": wire_target
        })
        .to_string(),
    )
    .await;
    let snapshot = recv_json(&mut client, &shared).await;
    assert_eq!(snapshot["type"], "runtime_snapshot", "{snapshot}");

    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &json!({
            "type": "data_request", "requestId": "data-1",
            "operation": "workspace_info", "workspaceId": "relay-e2e-workspace"
        })
        .to_string(),
    )
    .await;
    let data = recv_json(&mut client, &shared).await;
    assert_eq!(data["requestId"], "data-1", "{data}");
    assert_ne!(data["error"]["code"], "unauthorized_target");

    // Reverse: the management plane still rejects the paired device, and a
    // token without the owner stamp loses the whole data plane.
    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &json!({
            "type": "host_request", "requestId": "manage-1",
            "operation": "preference_get"
        })
        .to_string(),
    )
    .await;
    let refusal = recv_json(&mut client, &shared).await;
    assert_eq!(refusal["error"]["code"], "forbidden_class", "{refusal}");

    // Reverse: a remote hello without a valid device token never gains a
    // session at all — no data plane exists to refuse (spec §2.8).
    let (mut rogue, _id) = connect_client(&e2e.relay).await;
    let rogue_shared = e2ee_client::handshake(&mut rogue, &e2e.identity.public_key_b64).await;
    e2ee_client::send_sealed(
        &mut rogue,
        &rogue_shared,
        &remote_hello("rogue-phone", "not-a-device-token"),
    )
    .await;
    let rejected = recv_json(&mut rogue, &rogue_shared).await;
    assert_eq!(
        rejected["error"]["code"], "unauthorized_device",
        "{rejected}"
    );
    rogue.expect_close().await;

    teardown(e2e).await;
}

/// (b) Any frame before `e2ee_ready` closes the connection and nothing
/// reaches the session pipeline (spec §2.5: zero processing pre-handshake).
#[tokio::test]
async fn pre_ready_frame_closes_the_data_connection() {
    let _serial = serial_guard().await;
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
    let _serial = serial_guard().await;
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
    let _serial = serial_guard().await;
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
    let _serial = serial_guard().await;
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
    let _serial = serial_guard().await;
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
    let _serial = serial_guard().await;
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
    let _serial = serial_guard().await;
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
    let _serial = serial_guard().await;
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

// ---------------------------------------------------------------------------
// Remote terminal over the production relay path with a REAL PTY (plan
// 2026-11-03 §7 Task 5). This is **relay integration**: real encrypted relay
// transport + real HostServer/TerminalManager/PTY, not a deployed two-host
// session. The device view points at a registered workspace that differs from
// the pairing window's workspace, so "the PTY ran in the page's workspace" is a
// real assertion rather than a restatement of the fixture.
// ---------------------------------------------------------------------------

const PTY_STEP: Duration = Duration::from_secs(20);

fn pty_b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn pty_unb64(value: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .expect("terminal payload is base64")
}

/// Serializes the process-wide environment this PTY fixture needs. The guard is
/// applied only around the spawn calls (the child inherits its environment at
/// spawn time), so a parallel test never sees a mutated HOME for long.
static PTY_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Serializes this file's tests. The real-PTY case runs a shell, the whole
/// daemon stack and the test client in one runtime; on a busy machine the other
/// relay fixtures running beside it starve that runtime past the mock's frame
/// deadline, which failed the case with "socket closed"/frame timeouts instead
/// of a terminal defect. Serializing removes the contention, not the coverage:
/// every assertion, deadline and the multi-second PTY script still run, and a
/// genuine stall still fails (mock `recv_text` deadline, `PTY_STEP`).
static RELAY_SERIAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Async on purpose: the guard is held for the whole test, so it must be a
/// runtime-aware lock (a `std` guard held across an await is not allowed).
async fn serial_guard() -> tokio::sync::MutexGuard<'static, ()> {
    RELAY_SERIAL_LOCK.lock().await
}

/// The app surface this suite never exercises: every call refuses.
struct NoSurface;

#[async_trait::async_trait]
impl AppSurface for NoSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &RuntimeTarget,
        _label: &str,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn focus_window(&self, _label: &str) -> Result<(), String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        Ok(())
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused in the relay PTY suite".into())
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        Err("unused in the relay PTY suite".into())
    }
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn apply(vars: [(&'static str, String); 4]) -> Self {
        let mut saved = Vec::new();
        for (key, value) in vars {
            saved.push((key, std::env::var(key).ok()));
            std::env::set_var(key, value);
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct PtyE2e {
    /// The production composition root: host, manager, runtimes, auth, registry.
    state: Arc<DaemonState>,
    /// Taken by `teardown` for its explicit `MockRelay::stop`.
    relay: Option<MockRelay>,
    host: Arc<HostServer>,
    transport: Option<RelayTransportHandle>,
    identity: DaemonIdentity,
    device_token: String,
    owner: String,
    temp: PathBuf,
    /// Registered workspace the device view points at (its PTY cwd must be here).
    workspace_id: String,
    workspace_root: PathBuf,
    /// The pairing window's workspace — deliberately a different directory.
    pairing_root: PathBuf,
    /// That workspace's registry id, the navigation target for the second
    /// authenticated connection.
    pairing_workspace_id: String,
    session_id: String,
    manager: Arc<TerminalManager>,
    /// Set once the fixture is torn down, so the Drop safety net is a no-op.
    closed: std::sync::atomic::AtomicBool,
    /// Isolates the shell launcher's HOME for as long as this fixture lives.
    _env: EnvGuard,
    /// Serializes the process-wide launcher environment with any other test.
    _env_lock: std::sync::MutexGuard<'static, ()>,
    /// Kept alive: dropping it ends the in-memory Pi bridge and the manager
    /// would report the fixture runtime as crashed.
    _pi: picot_host::pi_rpc_bridge::InMemoryPiProcess,
}

impl Drop for PtyE2e {
    fn drop(&mut self) {
        // Final safety net: an assertion panic still kills every PTY, stops the
        // runtimes and the host, and removes the fixture roots.
        self.cleanup();
    }
}

impl PtyE2e {
    fn frame(&self, request_id: &str, payload: Value) -> Value {
        json!({
            "type": "terminal_command",
            "requestId": request_id,
            "workspaceId": self.workspace_id,
            "workspaceGeneration": 0,
            "sessionId": self.session_id,
            "instanceId": "pty-instance",
            "payload": payload,
        })
    }

    /// A `host_request` for the production navigation handler.
    fn host_frame(&self, request_id: &str, operation: &str, args: Value) -> Value {
        json!({
            "type": "host_request",
            "protocolVersion": 2,
            "requestId": request_id,
            "operation": operation,
            "args": args,
        })
    }

    /// Stop every fixture-owned resource. Idempotent, so normal teardown and the
    /// panic path can both call it.
    fn cleanup(&self) {
        if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.manager.kill_all();
        self.state.runtimes.stop_all();
        self.host.stop();
        let _ = fs::remove_dir_all(&self.temp);
    }

    fn relay(&self) -> &MockRelay {
        self.relay.as_ref().expect("the fixture relay is live")
    }

    /// Explicit bounded teardown, in dependency order: the transport joins
    /// first (it owns the relay session), then the mock relay's listener task is
    /// stopped, and only then are the fixture roots removed. The `Drop` safety
    /// net stays for the panic path and is a no-op after this.
    async fn teardown(mut self) {
        let transport = self.transport.take();
        if let Some(transport) = transport {
            tokio::time::timeout(PTY_STEP, transport.stop())
                .await
                .expect("relay transport stop timed out");
        }
        if let Some(relay) = self.relay.take() {
            tokio::time::timeout(PTY_STEP, relay.stop())
                .await
                .expect("mock relay stop timed out");
        }
        self.cleanup();
    }
}

async fn pty_setup(label: &str) -> PtyE2e {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "picot-relay-pty-{label}-{}-{nonce}",
        std::process::id()
    ));
    let static_dir = temp.join("public");
    let workspace_root = temp.join("device-workspace");
    let pairing_root = temp.join("pairing-window");
    let home = temp.join("home");
    for dir in [&static_dir, &workspace_root, &pairing_root, &home] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(static_dir.join("index.html"), "Picot").unwrap();
    fs::write(workspace_root.join("marker.txt"), "device workspace").unwrap();
    let workspace_root = workspace_root.canonicalize().unwrap();
    let pairing_root = pairing_root.canonicalize().unwrap();

    // Production composition root (daemon/mod.rs): host, terminal manager with
    // its owner event sink, owner registry and device authority all wired the
    // way the shipped daemon wires them.
    let state = Arc::new(
        tokio::time::timeout(
            PTY_STEP,
            DaemonState::start(
                DaemonConfig {
                    data_dir: temp.join("data"),
                    config_dir: temp.join("config"),
                    static_dir,
                    bundle_identifier: "com.palandata.picot.test".into(),
                },
                Arc::new(NoSurface),
            ),
        )
        .await
        .expect("daemon start timed out")
        .expect("the daemon host stack boots"),
    );
    // The real navigation dispatcher, exactly as the daemon installs it.
    state.install_control_handler();
    let host = Arc::clone(&state.host);
    let manager = Arc::clone(&state.terminals);

    let workspace_id = state
        .metadata
        .lock()
        .unwrap()
        .add_workspace(&workspace_root)
        .expect("the device workspace registers")
        .0
        .workspace_id;
    let pairing_workspace_id = state
        .metadata
        .lock()
        .unwrap()
        .add_workspace(&pairing_root)
        .expect("the pairing workspace registers")
        .0
        .workspace_id;
    let (owner, _capability) = state
        .owner_registry
        .create_owner_with_workspace(
            "relay-pty-window".into(),
            pairing_root.clone(),
            0,
            "http://127.0.0.1:1".into(),
            Some(pairing_workspace_id.clone()),
            picot_host::window_owner::TemporaryKind::DefaultStartup,
        )
        .unwrap();
    let session_id = "pty-session".to_string();
    let target = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
        workspace_id.clone(),
        session_id.clone(),
        "pty-instance",
        owner.as_str(),
        0,
    );
    let pi = state.runtimes.register_in_memory(target).unwrap();

    // One paired device; both authenticated connections in the test carry the
    // same device token, so they share one view (contract 1).
    let pairing = state
        .auth
        .lock()
        .unwrap()
        .create_pairing(2_000, Some(owner.as_str()));
    let device_token = state
        .auth
        .lock()
        .unwrap()
        .exchange(&pairing.token, "relay-pty-device", 2_001)
        .expect("seeded device token");
    state
        .auth
        .lock()
        .unwrap()
        .store_device_view(
            "relay-pty-device",
            &DeviceView {
                workspace_id: workspace_id.clone(),
                session_id: Some(session_id.clone()),
                generation: 0,
            },
        )
        .unwrap();

    let relay = MockRelay::start().await;
    let identity = DaemonIdentity::for_tests();
    let sink: Arc<dyn RelaySessionSink> = host.relay_session_sink();
    let transport = RelayTransport::start_with_seams(
        RelayConfig {
            endpoint: relay.addr().to_string(),
            use_tls: false,
            server_id: "pty-server".into(),
        },
        identity.clone(),
        sink,
        TransportSeams {
            // The PTY fixture keeps a stable control link; see `pty_timings`.
            timings: pty_timings(),
            sleep: Arc::new(|duration: Duration| Box::pin(tokio::time::sleep(duration))),
            connector: None,
        },
    );

    // A controlled launcher: bash with an isolated HOME and no rc files, so no
    // user shell configuration can change the fixture's output.
    let env_lock = PTY_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let env_guard = EnvGuard::apply([
        ("SHELL", "/bin/bash".to_string()),
        ("HOME", home.to_string_lossy().to_string()),
        ("ZDOTDIR", home.to_string_lossy().to_string()),
        ("BASH_ENV", String::new()),
    ]);

    PtyE2e {
        state,
        relay: Some(relay),
        host,
        transport: Some(transport),
        identity,
        device_token,
        owner: owner.as_str().to_owned(),
        temp,
        workspace_id,
        workspace_root,
        pairing_root,
        pairing_workspace_id,
        session_id,
        manager,
        closed: std::sync::atomic::AtomicBool::new(false),
        _env: env_guard,
        _env_lock: env_lock,
        _pi: pi,
    }
}

async fn pty_hello(e2e: &PtyE2e) -> (MockClientSocket, [u8; 32], Value) {
    let (mut client, _id) = connect_client(e2e.relay()).await;
    let shared = e2ee_client::handshake(&mut client, &e2e.identity.public_key_b64).await;
    e2ee_client::send_sealed(
        &mut client,
        &shared,
        &remote_hello("pty-ui", &e2e.device_token),
    )
    .await;
    let ack = recv_json(&mut client, &shared).await;
    (client, shared, ack)
}

/// One overall deadline for a whole expectation (plan Task 5): a slow peer must
/// fail the test with a diagnostic, never hang it.
struct Deadline {
    label: &'static str,
    ends: tokio::time::Instant,
    last: String,
}

impl Deadline {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            ends: tokio::time::Instant::now() + PTY_STEP,
            last: String::from("(no frame yet)"),
        }
    }

    fn record(&mut self, frame: &Value) {
        // No credentials land in these frames; the tail is the diagnostic.
        let text = frame.to_string();
        self.last = text.chars().take(600).collect();
    }

    async fn frame(&mut self, client: &mut MockClientSocket, shared: &[u8; 32]) -> Value {
        let remaining = self
            .ends
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "{} exceeded {:?}; last frame: {}",
                self.label, PTY_STEP, self.last
            );
        }
        let frame = tokio::time::timeout(remaining, recv_json(client, shared))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{} exceeded {:?} (last frame: {})",
                    self.label, PTY_STEP, self.last
                )
            });
        self.record(&frame);
        frame
    }
}

/// Acknowledges PTY output batches on the connection they arrived on: the
/// retained journal only advances for batches the device confirms, so a batch
/// read here is acked here — never recorded for a later, skippable ack.
struct OutputAcks<'a> {
    /// Envelope template for the acks this helper sends: the caller's binding,
    /// so a test that moved to a replacement runtime acks on that identity.
    template: Value,
    /// Request-id prefix for the acks this helper sends.
    tag: &'a str,
    /// Whether batches are acknowledged (`false` keeps output for a journal
    /// assertion). Reply checking and output collection always run.
    ack: bool,
    sent: usize,
    acknowledged: usize,
    /// Request id of the one ack in flight. Acks are paced: the host's device
    /// session drops inbound frames once its bounded channel fills, so a client
    /// that fires an ack per batch starves its own input path.
    pending: Option<String>,
    /// Newest watermark seen while an ack was in flight.
    latest: Option<(String, u64, u64)>,
}

impl<'a> OutputAcks<'a> {
    fn new(e2e: &PtyE2e, tag: &'a str, ack: bool) -> Self {
        Self::with_template(e2e.frame("ack", Value::Null), tag, ack)
    }

    fn with_template(template: Value, tag: &'a str, ack: bool) -> Self {
        Self {
            template,
            tag,
            ack,
            sent: 0,
            acknowledged: 0,
            pending: None,
            latest: None,
        }
    }

    /// Feed one inbound frame: collect its output, send the ack, and check the
    /// reply of any ack this helper sent. Returns true when the frame was an ack
    /// reply rather than something the caller still has to interpret.
    async fn observe(
        &mut self,
        client: &mut MockClientSocket,
        shared: &[u8; 32],
        frame: &Value,
        output: &mut Vec<u8>,
    ) -> bool {
        if self
            .pending
            .as_deref()
            .is_some_and(|pending| frame["requestId"].as_str() == Some(pending))
        {
            // An ack reply: a refusal is a failure here, not a skip.
            assert_eq!(
                frame["type"],
                json!("terminal_ack_acked"),
                "the host refused a batch ack: {frame}"
            );
            self.acknowledged += 1;
            self.pending = None;
            if let Some((id, generation, sequence)) = self.latest.take() {
                self.send(client, shared, id, generation, sequence).await;
            }
            return true;
        }
        if frame["requestId"]
            .as_str()
            .is_some_and(|request_id| request_id.starts_with(self.tag))
        {
            // A reply to an ack that is no longer the one in flight: nothing to
            // assert beyond the type, but a refusal is still a failure.
            assert_eq!(
                frame["type"],
                json!("terminal_ack_acked"),
                "the host refused a batch ack: {frame}"
            );
            return true;
        }
        if frame["type"] != json!("terminal_event") {
            return false;
        }
        let payload = &frame["payload"];
        if payload["type"] != json!("terminal_output") {
            return false;
        }
        if let Some(value) = payload["dataBase64"].as_str() {
            output.extend_from_slice(&pty_unb64(value));
        }
        if !self.ack {
            return false;
        }
        let (Some(id), Some(last)) = (
            payload["terminalId"].as_str(),
            payload["lastSequence"].as_u64(),
        ) else {
            return false;
        };
        let generation = payload["generation"].as_u64().unwrap_or(1);
        let (id, last) = (id.to_string(), last);
        if self.pending.is_some() {
            // One ack in flight at a time; the newest watermark is sent as soon
            // as its reply lands.
            self.latest = Some((id, generation, last));
        } else {
            self.send(client, shared, id, generation, last).await;
        }
        false
    }

    async fn send(
        &mut self,
        client: &mut MockClientSocket,
        shared: &[u8; 32],
        terminal_id: String,
        generation: u64,
        sequence: u64,
    ) {
        self.sent += 1;
        let request_id = format!("{}-{}", self.tag, self.sent);
        let mut ack = self.template.clone();
        ack["requestId"] = json!(request_id.clone());
        ack["payload"] = json!({
            "type": "terminal_ack",
            "terminalId": terminal_id,
            "generation": generation,
            "sequence": sequence,
        });
        e2ee_client::send_sealed(client, shared, &ack.to_string()).await;
        self.pending = Some(request_id);
    }
}

/// Send one terminal command and return the correlated response, accumulating
/// decoded output until it arrives. Bounded by one deadline.
async fn pty_command(
    client: &mut MockClientSocket,
    shared: &[u8; 32],
    frame: Value,
    output: &mut Vec<u8>,
) -> Value {
    pty_command_seen_with(client, shared, frame, output, None)
        .await
        .0
}

/// The same command, with every output batch acknowledged as it arrives: the
/// bytes a command reply carries are acked on the same connection, so a later
/// expectation never has to ack on their behalf.
async fn pty_command_acked(
    client: &mut MockClientSocket,
    shared: &[u8; 32],
    frame: Value,
    output: &mut Vec<u8>,
    acks: &mut OutputAcks<'_>,
) -> Value {
    pty_command_seen_with(client, shared, frame, output, Some(acks))
        .await
        .0
}

/// Same correlation, but reports the frame types observed while waiting: the
/// delayed-exit assertion needs the frames that *did* arrive, not silence.
async fn pty_command_seen(
    client: &mut MockClientSocket,
    shared: &[u8; 32],
    frame: Value,
    output: &mut Vec<u8>,
) -> (Value, Vec<String>) {
    pty_command_seen_with(client, shared, frame, output, None).await
}

async fn pty_command_seen_with(
    client: &mut MockClientSocket,
    shared: &[u8; 32],
    frame: Value,
    output: &mut Vec<u8>,
    mut acks: Option<&mut OutputAcks<'_>>,
) -> (Value, Vec<String>) {
    let mut seen = Vec::new();
    let request_id = frame["requestId"].as_str().unwrap().to_string();
    e2ee_client::send_sealed(client, shared, &frame.to_string()).await;
    let mut deadline = Deadline::new("terminal command reply");
    loop {
        let frame = deadline.frame(client, shared).await;
        seen.push(
            frame["event"]["type"]
                .as_str()
                .or_else(|| frame["type"].as_str())
                .unwrap_or("?")
                .to_string(),
        );
        match acks.as_deref_mut() {
            Some(acks) => {
                acks.observe(client, shared, &frame, output).await;
            }
            None => {
                if frame["type"] == json!("terminal_event") {
                    let payload = &frame["payload"];
                    if payload["type"] == json!("terminal_output") {
                        if let Some(value) = payload["dataBase64"].as_str() {
                            output.extend_from_slice(&pty_unb64(value));
                        }
                    }
                    continue;
                }
            }
        }
        if frame["requestId"] == json!(request_id) {
            return (frame, seen);
        }
    }
}

/// Poll for output that contains `needle`, acking every batch with a real
/// `terminal_ack` command so the retained journal advances instead of stalling
/// the PTY (bounded high output). Returns the number of accepted acks; reaching
/// the marker without one is a failure, not a silent pass.
async fn pty_expect_output(
    client: &mut MockClientSocket,
    shared: &[u8; 32],
    output: &mut Vec<u8>,
    needle: &str,
    acks: &mut OutputAcks<'_>,
) -> usize {
    pty_expect_output_with(client, shared, output, needle, acks, true).await
}

/// The same poll with every batch deliberately left unacknowledged: the
/// reconnect journal assertion needs output this device has not consumed.
async fn pty_expect_unacked_output(
    client: &mut MockClientSocket,
    shared: &[u8; 32],
    output: &mut Vec<u8>,
    needle: &str,
    acks: &mut OutputAcks<'_>,
) -> usize {
    pty_expect_output_with(client, shared, output, needle, acks, false).await
}

async fn pty_expect_output_with(
    client: &mut MockClientSocket,
    shared: &[u8; 32],
    output: &mut Vec<u8>,
    needle: &str,
    acks: &mut OutputAcks<'_>,
    require_ack: bool,
) -> usize {
    let mut deadline = Deadline::new("terminal output");
    loop {
        if String::from_utf8_lossy(output).contains(needle) {
            if require_ack {
                assert!(
                    acks.sent > 0,
                    "the output for {needle:?} arrived without a single batch ack"
                );
                // Drain until the in-flight ack has been answered (and any
                // deferred watermark it carried): a refusal must fail the test
                // instead of being left unread.
                while acks.pending.is_some() || acks.latest.is_some() {
                    let frame = deadline.frame(client, shared).await;
                    acks.observe(client, shared, &frame, output).await;
                }
            }
            return acks.acknowledged;
        }
        let frame = deadline.frame(client, shared).await;
        acks.observe(client, shared, &frame, output).await;
    }
}

/// B1 over the E2EE relay: admission identity is the page's verified binding
/// plus the server-derived owner, so a same-instance rebind of the workspace
/// generation stops matching and the command is refused *before* any terminal
/// state changes. The pre-fix code inherited the live generation into
/// `expected`, which compared the live value with itself and admitted the stale
/// page — this is the relay-level counterpart of the authenticated-WebSocket
/// fixture in `relay_navigation_admission.rs`.
#[tokio::test]
async fn a_same_instance_generation_rebind_stops_admitting_the_page_binding() {
    let _serial = serial_guard().await;
    let e2e = pty_setup("rebind").await;
    let (mut client, shared, ack) = pty_hello(&e2e).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");
    let mut output = Vec::new();

    // The captured binding (workspace generation 0) is admitted as usual.
    let listed = pty_command(
        &mut client,
        &shared,
        e2e.frame("rebind-list-1", json!({ "type": "terminal_list" })),
        &mut output,
    )
    .await;
    assert_eq!(listed["type"], "terminal_listed", "{listed}");
    let created = pty_command(
        &mut client,
        &shared,
        e2e.frame("rebind-create-1", json!({ "type": "terminal_create" })),
        &mut output,
    )
    .await;
    assert_eq!(created["type"], "terminal_created", "{created}");
    let terminal_id = created["terminalId"].as_str().unwrap().to_string();

    // Same instance, same owner, same session: only the workspace generation
    // moves, which is exactly the drift the page's binding cannot follow.
    assert!(e2e
        .state
        .runtimes
        .rebind_owner_generation(&e2e.owner, &e2e.session_id, 1));

    let refused = pty_command(
        &mut client,
        &shared,
        e2e.frame("rebind-create-2", json!({ "type": "terminal_create" })),
        &mut output,
    )
    .await;
    assert_eq!(refused["type"], "terminal_command_failed", "{refused}");
    assert_eq!(refused["code"], "terminal_runtime_unavailable", "{refused}");

    // Rebind back only to observe the refusal's effect: still exactly the one
    // terminal created before the rebind, so the refused command mutated nothing.
    assert!(e2e
        .state
        .runtimes
        .rebind_owner_generation(&e2e.owner, &e2e.session_id, 0));
    let listed = pty_command(
        &mut client,
        &shared,
        e2e.frame("rebind-list-2", json!({ "type": "terminal_list" })),
        &mut output,
    )
    .await;
    assert_eq!(listed["type"], "terminal_listed", "{listed}");
    let tabs = listed["tabs"].as_array().unwrap();
    assert_eq!(
        tabs.len(),
        1,
        "the refused create made no terminal: {listed}"
    );
    assert_eq!(
        tabs[0]["terminalId"],
        json!(terminal_id.as_str()),
        "{listed}"
    );

    client.close().await;
    e2e.teardown().await;
}

// Multi-threaded on purpose: this fixture drives a real shell, the whole daemon
// stack and the test client from one test, and on a single-thread runtime a busy
// machine starves one of them into a missed frame deadline. Wider scheduling
// removes that starvation without weakening any assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_terminal_runs_real_pty_in_the_device_workspace() {
    let _serial = serial_guard().await;
    let e2e = pty_setup("main").await;
    let (mut client, shared, ack) = pty_hello(&e2e).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");
    // The approved revision survives the bridge/relay ack path.
    assert_eq!(
        ack["capabilities"]["terminalBindingVersion"],
        json!(1),
        "{ack}"
    );

    // list → create → input a printed marker (echo of the typed command cannot
    // satisfy this: only the expanded output contains the marker) → cwd.
    let mut output = Vec::new();
    let listed = pty_command(
        &mut client,
        &shared,
        e2e.frame("pty-list-1", json!({ "type": "terminal_list" })),
        &mut output,
    )
    .await;
    assert_eq!(listed["type"], "terminal_listed", "{listed}");
    assert_eq!(listed["tabs"].as_array().unwrap().len(), 0, "{listed}");

    let created = pty_command(
        &mut client,
        &shared,
        e2e.frame("pty-create-1", json!({ "type": "terminal_create" })),
        &mut output,
    )
    .await;
    assert_eq!(created["type"], "terminal_created", "{created}");
    let terminal_id = created["terminalId"].as_str().unwrap().to_string();
    let generation = created["generation"].as_u64().unwrap();

    // Every batch is acknowledged inline and each ack reply is checked, so the
    // retained journal advances instead of replaying (no skippable branch).
    let mut marker_acks = OutputAcks::new(&e2e, "pty-ack-mark", true);
    pty_command_acked(
        &mut client,
        &shared,
        e2e.frame(
            "pty-input-marker",
            json!({
                "type": "terminal_input",
                "terminalId": terminal_id,
                "generation": generation,
                "dataBase64": pty_b64(b"printf 'PICOT_MARK_%s\\n' 2026\n"),
            }),
        ),
        &mut output,
        &mut marker_acks,
    )
    .await;
    pty_expect_output(
        &mut client,
        &shared,
        &mut output,
        "PICOT_MARK_2026",
        &mut marker_acks,
    )
    .await;
    assert!(
        marker_acks.acknowledged > 0,
        "the marker output was acknowledged"
    );

    output.clear();
    let mut pwd_acks = OutputAcks::new(&e2e, "pty-ack-pwd", true);
    pty_command_acked(
        &mut client,
        &shared,
        e2e.frame(
            "pty-input-pwd",
            json!({
                "type": "terminal_input",
                "terminalId": terminal_id,
                "generation": generation,
                "dataBase64": pty_b64(b"pwd\n"),
            }),
        ),
        &mut output,
        &mut pwd_acks,
    )
    .await;
    let device_root = e2e.workspace_root.to_string_lossy().to_string();
    pty_expect_output(
        &mut client,
        &shared,
        &mut output,
        &device_root,
        &mut pwd_acks,
    )
    .await;
    let printed = String::from_utf8_lossy(&output).to_string();
    let pairing_root = e2e.pairing_root.to_string_lossy().to_string();
    assert!(
        !printed.contains(&pairing_root),
        "the PTY ran in the pairing window's workspace: {printed}"
    );

    // resize
    let resized = pty_command(
        &mut client,
        &shared,
        e2e.frame(
            "pty-resize",
            json!({
                "type": "terminal_resize",
                "terminalId": terminal_id,
                "generation": generation,
                "cols": 120,
                "rows": 40,
            }),
        ),
        &mut output,
    )
    .await;
    assert_eq!(resized["type"], "terminal_resized", "{resized}");

    // Bounded high output: one ~5KB burst plus a trailing marker command. The
    // marker is awaited through `pty_expect_output` (the same helper as every
    // other expectation), so the evidence is the burst's *content*, not just a
    // command reply: every batch is acked as it lands and the acked output must
    // contain 100 numbered lines and the marker.
    output.clear();
    let mut high_acks = OutputAcks::new(&e2e, "pty-ack-high", true);
    pty_command_acked(
        &mut client,
        &shared,
        e2e.frame(
            "pty-input-high-1",
            json!({
                "type": "terminal_input",
                "terminalId": terminal_id,
                "generation": generation,
                "dataBase64": pty_b64(
                    b"for i in $(seq 1 100); do printf 'line-%s-0123456789012345678901234567890123456789\\n' \"$i\"; done\n"
                ),
            }),
        ),
        &mut output,
        &mut high_acks,
    )
    .await;
    pty_command_acked(
        &mut client,
        &shared,
        e2e.frame(
            "pty-input-high-done",
            json!({
                "type": "terminal_input",
                "terminalId": terminal_id,
                "generation": generation,
                "dataBase64": pty_b64(b"printf 'PICOT_HIGH_%s_DONE\\n' 2026\n"),
            }),
        ),
        &mut output,
        &mut high_acks,
    )
    .await;
    pty_expect_output(
        &mut client,
        &shared,
        &mut output,
        "PICOT_HIGH_2026_DONE",
        &mut high_acks,
    )
    .await;
    // The acked bytes are the burst itself: 100 numbered lines, in order, and
    // the marker line after them.
    let high_text = String::from_utf8_lossy(&output).to_string();
    let high_lines: Vec<&str> = high_text
        .lines()
        .filter(|line| line.starts_with("line-"))
        .collect();
    assert_eq!(
        high_lines.len(),
        100,
        "the burst delivered {} numbered line(s): {:?}",
        high_lines.len(),
        &high_text.chars().take(300).collect::<String>()
    );
    for index in 1..=100usize {
        assert!(
            high_text.contains(&format!("line-{index}-0123456789")),
            "line {index} is missing from the acknowledged output"
        );
    }
    // `pty_expect_output` drains the in-flight ack reply before returning, so a
    // refused ack fails there and the accepted count is final here.
    assert!(
        high_acks.acknowledged > 0,
        "no batch ack of the high-output burst was accepted ({} sent)",
        high_acks.sent
    );

    // Unacknowledged output stays in the retained journal, so a reconnect can
    // be checked for content instead of a terminal id alone.
    output.clear();
    pty_command(
        &mut client,
        &shared,
        e2e.frame(
            "pty-input-resume",
            json!({
                "type": "terminal_input",
                "terminalId": terminal_id,
                "generation": generation,
                "dataBase64": pty_b64(b"printf 'PICOT_RESUME_%s\\n' 2026\n"),
            }),
        ),
        &mut output,
    )
    .await;
    // Deliberately *not* acknowledged: the reconnect assertion below reads this
    // output out of the retained journal.
    let mut resume_acks = OutputAcks::new(&e2e, "pty-resume", false);
    pty_expect_unacked_output(
        &mut client,
        &shared,
        &mut output,
        "PICOT_RESUME_2026",
        &mut resume_acks,
    )
    .await;

    // A literal old-client frame (only the legacy generation token) is refused
    // before dispatch — the PTY list below proves nothing was created for it.
    let legacy = pty_command(
        &mut client,
        &shared,
        json!({
            "type": "terminal_command",
            "requestId": "pty-legacy",
            "workspaceGeneration": 0,
            "payload": { "type": "terminal_create" },
        }),
        &mut output,
    )
    .await;
    assert_eq!(legacy["type"], "terminal_command_failed", "{legacy}");
    assert_eq!(legacy["code"], "terminal_protocol_unsupported", "{legacy}");

    // The exact page runtime stops while the PTY keeps running: the next
    // command is refused server-side, and the retained PTY is still there once
    // a replacement runtime is attached.
    let target = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
        e2e.workspace_id.clone(),
        e2e.session_id.clone(),
        "pty-instance",
        e2e.owner.clone(),
        0,
    );
    e2e.state.runtimes.stop(&target).unwrap();
    let (stopped, seen) = pty_command_seen(
        &mut client,
        &shared,
        e2e.frame("pty-after-stop", json!({ "type": "terminal_list" })),
        &mut output,
    )
    .await;
    assert_eq!(stopped["code"], "terminal_runtime_unavailable", "{stopped}");
    // Explicit delayed-exit evidence, not silence: the frames this client
    // received before the rejection were inspected and none of them was a
    // runtime exit. The server refused on its own closing tombstone.
    assert!(
        !seen.iter().any(|kind| kind == "runtime_stopped"
            || kind == "runtime_crashed"
            || kind == "snapshot_required"),
        "the rejection must not depend on a delivered exit event: {seen:?}"
    );

    // A late exit notification must not be required: the server already
    // refuses, before any runtime_stopped frame is delivered to this client.
    let replacement = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
        e2e.workspace_id.clone(),
        e2e.session_id.clone(),
        "pty-instance-2",
        e2e.owner.clone(),
        0,
    );
    let _pi = e2e.state.runtimes.register_in_memory(replacement).unwrap();
    let mut replacement_frame = e2e.frame("pty-list-2", json!({ "type": "terminal_list" }));
    replacement_frame["instanceId"] = json!("pty-instance-2");
    let listed = pty_command(&mut client, &shared, replacement_frame.clone(), &mut output).await;
    assert_eq!(listed["type"], "terminal_listed", "{listed}");
    let tabs = listed["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1, "the PTY survived the runtime stop: {listed}");
    assert_eq!(
        tabs[0]["terminalId"],
        json!(terminal_id.as_str()),
        "{listed}"
    );

    // Disconnect without closing: a new authenticated connection reattaches and
    // recovers the same PTY (no duplicate shell).
    client.close().await;
    let (mut second, shared, _ack) = pty_hello(&e2e).await;
    let mut replacement_frame = replacement_frame.clone();
    replacement_frame["requestId"] = json!("pty-list-3");
    let recovered = pty_command(&mut second, &shared, replacement_frame.clone(), &mut output).await;
    assert_eq!(recovered["type"], "terminal_listed", "{recovered}");
    let recovered_tabs = recovered["tabs"].as_array().unwrap();
    assert_eq!(recovered_tabs.len(), 1, "no duplicate PTY: {recovered}");
    assert_eq!(recovered_tabs[0]["terminalId"], json!(terminal_id.as_str()));
    // Journal content, not just the id: the unacked output is still retained
    // for the reattaching projection to replay.
    let journal = recovered_tabs[0]["journal"]["dataBase64"]
        .as_str()
        .map(pty_unb64)
        .unwrap_or_default();
    let replayed = String::from_utf8_lossy(&journal).to_string();
    assert!(
        replayed.contains("PICOT_RESUME_2026"),
        "the retained journal lost the unacked output: {replayed:?}"
    );
    assert_eq!(
        recovered_tabs[0]["generation"],
        json!(generation),
        "{recovered}"
    );

    // Close, then reopen on the same reattached connection: the closed PTY ends
    // and takes its journal with it, and a fresh create gets its own shell.
    let mut close_frame = replacement_frame.clone();
    close_frame["requestId"] = json!("pty-close-1");
    close_frame["payload"] = json!({
        "type": "terminal_close",
        "terminalId": terminal_id,
        "generation": generation,
    });
    let closed = pty_command(&mut second, &shared, close_frame, &mut output).await;
    assert_eq!(closed["type"], "terminal_closed", "{closed}");
    let mut reopen_frame = replacement_frame.clone();
    reopen_frame["requestId"] = json!("pty-reopen-1");
    reopen_frame["payload"] = json!({ "type": "terminal_create" });
    let reopened = pty_command(&mut second, &shared, reopen_frame, &mut output).await;
    assert_eq!(reopened["type"], "terminal_created", "{reopened}");
    let reopened_id = reopened["terminalId"].as_str().unwrap().to_string();
    assert_ne!(reopened_id, terminal_id, "a reopen is a new terminal");
    let reopened_generation = reopened["generation"].as_u64().unwrap();
    output.clear();
    let mut reopen_input = replacement_frame.clone();
    reopen_input["requestId"] = json!("pty-reopen-input");
    reopen_input["payload"] = json!({
        "type": "terminal_input",
        "terminalId": reopened_id,
        "generation": reopened_generation,
        "dataBase64": pty_b64(b"printf 'PICOT_REOPEN_%s\\n' 2026\n"),
    });
    // The reattached page binds the replacement runtime, so its acks carry that
    // instance id rather than the stopped one.
    let mut reopen_acks =
        OutputAcks::with_template(replacement_frame.clone(), "pty-ack-reopen", true);
    pty_command_acked(
        &mut second,
        &shared,
        reopen_input,
        &mut output,
        &mut reopen_acks,
    )
    .await;
    pty_expect_output(
        &mut second,
        &shared,
        &mut output,
        "PICOT_REOPEN_2026",
        &mut reopen_acks,
    )
    .await;
    assert!(
        reopen_acks.acknowledged > 0,
        "the reopened PTY produces output"
    );

    // A second authenticated connection for the same device drives the real
    // navigation handler (prepare + commit) over the relay, which moves the
    // shared view. The old page's binding is then stale and must be refused
    // without touching terminal state.
    let (mut mover, shared_mover, _ack) = pty_hello(&e2e).await;
    let pairing_target = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
        e2e.pairing_workspace_id.clone(),
        "pairing-session",
        "pairing-instance",
        e2e.owner.clone(),
        0,
    );
    let _pairing_pi = e2e
        .state
        .runtimes
        .register_in_memory(pairing_target)
        .unwrap();
    let prepared = pty_command(
        &mut mover,
        &shared_mover,
        e2e.host_frame(
            "pty-nav-prepare",
            "workspace_target_prepare",
            json!({
                "targetCwd": e2e.pairing_root.to_string_lossy(),
                "reuseExisting": true,
            }),
        ),
        &mut output,
    )
    .await;
    assert_eq!(prepared["type"], "host_response", "{prepared}");
    assert_eq!(
        prepared["response"]["targetWorkspaceId"],
        json!(e2e.pairing_workspace_id),
        "{prepared}"
    );
    let transition_generation = prepared["response"]["transitionGeneration"]
        .as_u64()
        .unwrap_or_else(|| panic!("prepare returns a transition generation: {prepared}"));
    let committed = pty_command(
        &mut mover,
        &shared_mover,
        e2e.host_frame(
            "pty-nav-commit",
            "workspace_transition_commit",
            json!({ "transitionGeneration": transition_generation }),
        ),
        &mut output,
    )
    .await;
    assert_eq!(committed["type"], "host_response", "{committed}");
    assert!(
        committed.get("error").is_none(),
        "the production handler committed: {committed}"
    );
    assert_eq!(
        e2e.state
            .auth
            .lock()
            .unwrap()
            .device_view("relay-pty-device")
            .unwrap()
            .map(|view| view.workspace_id),
        Some(e2e.pairing_workspace_id.clone()),
        "the production navigation handler moved the shared view"
    );
    let stale = pty_command(
        &mut mover,
        &shared_mover,
        e2e.frame("pty-stale", json!({ "type": "terminal_list" })),
        &mut output,
    )
    .await;
    assert_eq!(stale["code"], "terminal_binding_mismatch", "{stale}");

    // Close every connection this test still owns before the bounded teardown:
    // nothing is left half-open for the transport/mock-relay stop to clean up.
    // `client` was closed at the reattach step above; the Drop safety net stays
    // for the panic path.
    second.close().await;
    mover.close().await;

    e2e.teardown().await;
}
