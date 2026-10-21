// ABOUTME: Command-core contracts for TOFU, pairing persistence and cancellation.
// ABOUTME: Isolated temporary host stores only; all async cases have bounded execution.

use super::*;
use picot_host::relay::host_bridge::PairingSink;
use picot_host::relay::pairing::{offer_url, ConnectionOfferV2, RelayEndpoint};
use serde_json::Value;
use std::time::Duration;

fn offer(key: &str) -> String {
    offer_url(
        &ConnectionOfferV2 {
            v: 2,
            server_id: "host".into(),
            daemon_public_key_b64: key.into(),
            relay: RelayEndpoint {
                endpoint: "127.0.0.1:9".into(),
                use_tls: false,
            },
            pairing_token: Some("private-pairing".into()),
            pairing_token_expires_at: None,
        },
        "https://example.test",
    )
    .unwrap()
}
const KEY: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
const KEY2: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";
fn client() -> (tempfile::TempDir, Arc<HostClient>) {
    let dir = tempfile::tempdir().unwrap();
    let client = HostClient::load(dir.path().join("hosts.json")).unwrap();
    (dir, client)
}
async fn begin(client: &Arc<HostClient>, key: &str) -> Value {
    let pending = client.add("w1", &offer(key)).await.unwrap();
    assert_eq!(pending["status"], "tofuPending");
    client
        .confirm("w1", pending["questionId"].as_u64().unwrap(), true)
        .await
        .unwrap()
}
async fn sink(client: &Arc<HostClient>) -> PairingTransaction {
    let core = client.core.lock().await;
    PairingTransaction {
        client: Arc::downgrade(client),
        server_id: "host".into(),
        id: core.hosts["host"].candidate.as_ref().unwrap().id.clone(),
    }
}

#[tokio::test]
async fn host_client_tofu_stages_and_redacts_every_response() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (_dir, c) = client();
        let started = begin(&c, KEY).await;
        assert!(c.store.host("host").is_none());
        sink(&c)
            .await
            .persist(DeviceToken::new("private-device"))
            .await
            .unwrap();
        let saved = c.store.host("host").unwrap();
        assert_eq!(saved.profile.daemon_public_key_b64, KEY);
        let auto = c.add("w1", &offer(KEY)).await.unwrap();
        assert_ne!(auto["status"], "tofuPending");
        let changed = c.add("w1", &offer(KEY2)).await.unwrap();
        let stale = changed["questionId"].as_u64().unwrap();
        let latest = c.add("w1", &offer(KEY2)).await.unwrap();
        assert!(c.confirm("w1", stale, true).await.is_err());
        assert!(c
            .confirm("w2", latest["questionId"].as_u64().unwrap(), true)
            .await
            .is_err());
        c.confirm("w1", latest["questionId"].as_u64().unwrap(), false)
            .await
            .unwrap();
        assert_eq!(c.store.host("host").unwrap(), saved);
        for response in [
            started,
            auto,
            changed,
            latest,
            c.list().await.unwrap(),
            c.status("w0", "host").await.unwrap(),
        ] {
            let text = response.to_string();
            for secret in [
                "private-device",
                "private-pairing",
                "deviceToken",
                "pairingToken",
            ] {
                assert!(!text.contains(secret), "leaked {secret}");
            }
        }
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn host_client_remove_cancels_late_ack() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (_dir, c) = client();
        begin(&c, KEY).await;
        let transaction = sink(&c).await;
        c.status("w1", "host").await.unwrap();
        c.remove("host").await.unwrap();
        assert!(transaction
            .persist(DeviceToken::new("late-token"))
            .await
            .is_err());
        assert!(c.store.host("host").is_none());
        assert!(c.core.lock().await.hosts.is_empty());
        c.shutdown().await;
        assert!(c.add("w1", &offer(KEY)).await.is_err());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn host_client_release_stops_the_last_ref_but_keeps_the_other_window() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (_dir, c) = client();
        begin(&c, KEY).await;
        sink(&c)
            .await
            .persist(DeviceToken::new("saved-device"))
            .await
            .unwrap();
        c.status("w1", "host").await.unwrap();
        c.status("w2", "host").await.unwrap();
        let mut stopped = c.core.lock().await.hosts["host"]
            .active
            .as_ref()
            .unwrap()
            .handle
            .status
            .clone();
        c.close_window("w1").await;
        assert_ne!(*stopped.borrow(), HostBridgeStatus::Stopped);
        assert_eq!(
            c.core.lock().await.hosts["host"]
                .active
                .as_ref()
                .unwrap()
                .windows
                .len(),
            1
        );
        c.close_window("w2").await;
        while *stopped.borrow_and_update() != HostBridgeStatus::Stopped {
            stopped.changed().await.unwrap();
        }
        assert!(c.core.lock().await.hosts["host"].active.is_none());
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn host_client_credential_invalid_clears_token_and_stops_the_host_bridge() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = client();
        begin(&c, KEY).await;
        sink(&c)
            .await
            .persist(DeviceToken::new("private-device"))
            .await
            .unwrap();
        c.status("w1", "host").await.unwrap();
        c.status("w2", "host").await.unwrap();
        let mut stopped = {
            let core = c.core.lock().await;
            let bridge = core.hosts["host"].active.as_ref().unwrap();
            assert_eq!(bridge.windows.len(), 2);
            // picot-host owns how CredentialInvalid is reached; this case owns the reaction.
            bridge
                .handle
                .force_status_for_tests(HostBridgeStatus::CredentialInvalid);
            bridge.handle.status.clone()
        };
        // The row settles on offline + needs-pairing, never on a dead bridge.
        let listed = c.list().await.unwrap();
        assert_eq!(listed[0]["status"], "offline");
        assert_eq!(listed[0]["needsPairing"], true);
        assert!(!c.store.host("host").unwrap().has_token());
        while *stopped.borrow_and_update() != HostBridgeStatus::Stopped {
            stopped.changed().await.unwrap();
        }
        assert!(c.core.lock().await.hosts["host"].active.is_none());
        // No dead bridge is reused: the window is told to pair again.
        assert_eq!(
            c.status("w1", "host").await.unwrap_err(),
            "Host credential invalid; a fresh pairing offer is required"
        );
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn host_client_pairing_failure_is_reported_and_replaced_by_a_fresh_offer() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = client();
        begin(&c, KEY).await;
        let (mut stopped, spent) = {
            let core = c.core.lock().await;
            let candidate = core.hosts["host"].candidate.as_ref().unwrap();
            candidate
                .bridge
                .handle
                .force_status_for_tests(HostBridgeStatus::PairingFailed);
            (candidate.bridge.handle.status.clone(), candidate.id.clone())
        };
        assert_eq!(c.status("w0", "host").await.unwrap()["status"], "offline");
        assert!(c.store.host("host").is_none());
        // A refused offer does not block the next one behind "pairing in progress".
        let pending = c.add("w1", &offer(KEY2)).await.unwrap();
        assert_eq!(pending["status"], "tofuPending");
        c.confirm("w1", pending["questionId"].as_u64().unwrap(), true)
            .await
            .unwrap();
        while *stopped.borrow_and_update() != HostBridgeStatus::Stopped {
            stopped.changed().await.unwrap();
        }
        let core = c.core.lock().await;
        let candidate = core.hosts["host"].candidate.as_ref().unwrap();
        assert_ne!(candidate.id, spent);
        assert_ne!(
            *candidate.bridge.handle.status.borrow(),
            HostBridgeStatus::PairingFailed
        );
        drop(core);
        assert!(c.store.host("host").is_none());
        c.shutdown().await;
    })
    .await
    .unwrap();
}
