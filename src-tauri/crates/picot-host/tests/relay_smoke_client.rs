// ABOUTME: Mock-relay self-verification of the T11 smoke client (plan T11).
// ABOUTME: Drives picot_host::relay::smoke_client::run_smoke end to end
// ABOUTME: against the full production stack (HostServer + transport + mock
// ABOUTME: relay): one-scan pairing, probe round-trip, redaction, one-time
// ABOUTME: pairing token.

#[path = "support/mock_relay.rs"]
mod mock_relay;

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mock_relay::MockRelay;
use picot_host::host_server::HostServer;
use picot_host::metadata_store::{MetadataStore, SharedMetadataStore};
use picot_host::native_pi_manager::NativePiManager;
use picot_host::relay::e2ee::DaemonIdentity;
use picot_host::relay::pairing::{build_pairing_offer, PairingOffer};
use picot_host::relay::smoke_client::{self, OfferSource, SmokeOptions};
use picot_host::relay::transport::{
    RelayConfig, RelaySessionSink, RelayTransport, RelayTransportHandle, TransportSeams,
    TransportTimings,
};
use picot_host::remote_auth::RemoteAuth;
use serde_json::json;

/// The same in-memory owner runtime the Gate B e2e suite seeds: one live
/// workspace + a responder answering every runtime request, so the smoke
/// probe's `runtime_subscribe` can land on a live target.
fn seed_owner_runtime(
    temp: &std::path::Path,
    runtimes: &NativePiManager,
) -> (Arc<picot_host::window_owner::WindowOwnerRegistry>, String) {
    let registry = Arc::new(picot_host::window_owner::WindowOwnerRegistry::default());
    fs::create_dir_all(temp.join("ws")).unwrap();
    let (owner, _capability) = registry
        .create_owner_with_workspace(
            "relay-smoke-window".into(),
            temp.join("ws").canonicalize().unwrap(),
            0,
            "http://127.0.0.1:1".into(),
            Some("relay-smoke-workspace".into()),
            picot_host::window_owner::TemporaryKind::DefaultStartup,
        )
        .unwrap();
    let target = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
        "relay-smoke-workspace",
        "relay-smoke-session",
        "relay-smoke-instance",
        owner.as_str(),
        0,
    );
    let mut fake = runtimes.register_in_memory(target).unwrap();
    tokio::spawn(async move {
        while let Some(request) = fake.read_request().await {
            let _ = fake
                .write_frame(json!({
                    "id": request["id"],
                    "type": "response",
                    "success": true,
                    "data": { "sessionId": "relay-smoke-session" },
                }))
                .await;
        }
    });
    (registry, owner.as_str().to_owned())
}

struct SmokeStack {
    relay: MockRelay,
    host: HostServer,
    transport: RelayTransportHandle,
    identity: DaemonIdentity,
    /// The daemon's live RemoteAuth handle — pairing tokens mint here are
    /// the ones the smoke client's hello exchanges.
    auth: Arc<Mutex<RemoteAuth>>,
    temp: PathBuf,
}

async fn stack(label: &str) -> SmokeStack {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "picot-relay-smoke-cli-{label}-{}-{nonce}",
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
    let (registry, _owner) = seed_owner_runtime(&temp, &runtimes);
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
            server_id: "smoke-server".into(),
        },
        identity.clone(),
        sink,
        TransportSeams {
            timings: TransportTimings {
                backoff_base: Duration::from_secs(1),
                backoff_cap: Duration::from_secs(30),
                ping_interval: Duration::from_millis(50),
                stale_timeout: Duration::from_millis(400),
                ready_timeout: Duration::from_millis(400),
                handshake_timeout: Duration::from_secs(2),
                data_open_timeout: Duration::from_secs(2),
                e2ee_handshake_timeout: Duration::from_secs(5),
            },
            sleep: Arc::new(|duration: Duration| Box::pin(tokio::time::sleep(duration))),
            connector: None,
        },
    );
    // The daemon's control socket must exist before any client dial.
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
    SmokeStack {
        relay,
        host,
        transport,
        identity,
        auth,
        temp,
    }
}

/// The same mint point `relay.offer` fronts: a fresh one-scan pairing token
/// bound to the live owner, rendered into the offer URL the CLI consumes.
fn offer_for(stack: &SmokeStack) -> (PairingOffer, String) {
    // Real-clock mint: the daemon's hello-time exchange runs against
    // `now_seconds()`, so a synthetic `now` would be born expired.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let pairing = stack
        .auth
        .lock()
        .unwrap()
        .mint_fresh_pairing_token(now, Some("relay-smoke-owner"));
    let offer = build_pairing_offer(
        &stack.identity,
        &stack.relay.addr().to_string(),
        false,
        &pairing,
    )
    .unwrap();
    (offer, pairing.token.clone())
}

fn smoke_options(url: &str) -> SmokeOptions {
    SmokeOptions {
        offer_source: OfferSource::Argv(url.to_string()),
        endpoint_override: None,
        device_id: "smoke-1".into(),
        pairing_token_env: "PICOT_SMOKE_TOKEN".into(),
        target: Some(
            json!({
                "workspaceId": "relay-smoke-workspace",
                "sessionId": "relay-smoke-session",
                "instanceId": "relay-smoke-instance"
            })
            .to_string(),
        ),
    }
}

async fn teardown(stack: SmokeStack) {
    stack.transport.stop().await;
    stack.host.stop();
    fs::remove_dir_all(stack.temp).unwrap();
}

/// The CLI contract against the full stack: one-scan pairing exchange over
/// the E2EE relay channel, a positive data-plane probe, and output that
/// never contains either secret in full.
#[tokio::test]
async fn smoke_client_pairs_and_probes_the_data_plane_over_the_mock_relay() {
    let stack = stack("full-run").await;
    let (offer, pairing_token) = offer_for(&stack);
    let report = smoke_client::run_smoke(&smoke_options(&offer.url))
        .await
        .expect("smoke run succeeds");
    assert_eq!(report.probe_frame_type, "runtime_subscribed");
    assert!(report.ok());
    let device_token = report
        .device_token()
        .expect("one-scan pairing mints a device token")
        .to_owned();
    assert!(device_token.starts_with("picot_device_"));

    // Redaction: the rendered report and both live secrets stay disjoint
    // (the pairing token rode the offer URL, so it is the likeliest echo).
    let rendered = report.render();
    assert!(rendered.contains("SMOKE OK"), "{rendered}");
    assert!(!rendered.contains(&pairing_token), "{rendered}");
    assert!(!rendered.contains(&device_token), "{rendered}");

    teardown(stack).await;
}

#[tokio::test]
async fn smoke_client_rejects_a_reused_pairing_token() {
    let stack = stack("reused").await;
    let (offer, pairing_token) = offer_for(&stack);
    smoke_client::run_smoke(&smoke_options(&offer.url))
        .await
        .expect("first one-scan run succeeds");
    let second = smoke_client::run_smoke(&smoke_options(&offer.url))
        .await
        .expect_err("pairing tokens are one-time");
    assert!(second.contains("unauthorized_device"), "{second}");
    // The refusal never echoes the secret it rejected.
    assert!(!second.contains(&pairing_token), "{second}");
    teardown(stack).await;
}
