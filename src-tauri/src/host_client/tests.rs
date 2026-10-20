// ABOUTME: Command-core contracts for TOFU, staged persistence, cancellation and grants.
// ABOUTME: Isolated temporary host stores only; all async cases have bounded execution.

use super::*;
use picot_host::relay::host_bridge::{GrantRegistry, PairingSink};
use picot_host::relay::pairing::{offer_url, ConnectionOfferV2, RelayEndpoint};
use serde_json::Value;
use std::time::Duration;

const ORIGIN: &str = "http://127.0.0.1:54321";
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
    let pending = client.add("w1", ORIGIN, &offer(key)).await.unwrap();
    assert_eq!(pending["status"], "tofuPending");
    client
        .confirm("w1", ORIGIN, pending["questionId"].as_u64().unwrap(), true)
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
        let auto = c.add("w1", ORIGIN, &offer(KEY)).await.unwrap();
        assert_ne!(auto["status"], "tofuPending");
        let changed = c.add("w1", ORIGIN, &offer(KEY2)).await.unwrap();
        let stale = changed["questionId"].as_u64().unwrap();
        let latest = c.add("w1", ORIGIN, &offer(KEY2)).await.unwrap();
        assert!(c.confirm("w1", ORIGIN, stale, true).await.is_err());
        assert!(c
            .confirm("w2", ORIGIN, latest["questionId"].as_u64().unwrap(), true)
            .await
            .is_err());
        c.confirm("w1", ORIGIN, latest["questionId"].as_u64().unwrap(), false)
            .await
            .unwrap();
        assert_eq!(c.store.host("host").unwrap(), saved);
        for response in [
            started,
            auto,
            changed,
            latest,
            c.list().await.unwrap(),
            c.status("host").await.unwrap(),
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
async fn host_client_retry_save_commits_before_bridge_rebuild_and_preserves_refs() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (dir, c) = client();
        begin(&c, KEY).await;
        sink(&c)
            .await
            .persist(DeviceToken::new("old-device"))
            .await
            .unwrap();
        begin(&c, KEY2).await;
        let transaction = sink(&c).await;
        let file = dir.path().join("hosts.json");
        std::fs::rename(&file, dir.path().join("backup")).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(transaction
            .persist(DeviceToken::new("new-device"))
            .await
            .is_err());
        assert_eq!(
            c.store.host("host").unwrap().profile.daemon_public_key_b64,
            KEY
        );
        let failed = c.status("host").await.unwrap();
        assert_eq!(failed["status"], "saveFailed");
        assert_eq!(failed["recovery"]["onProcessExit"], "newOfferRequired");
        let old_url = failed["wsUrl"].clone();
        std::fs::remove_dir(&file).unwrap();
        std::fs::rename(dir.path().join("backup"), &file).unwrap();
        let retried = c.retry_save("host").await.unwrap();
        assert_ne!(retried["wsUrl"], old_url);
        assert_eq!(
            c.store.host("host").unwrap().device_token.unwrap().expose(),
            "new-device"
        );
        let core = c.core.lock().await;
        assert!(core.hosts["host"]
            .active
            .as_ref()
            .unwrap()
            .windows
            .contains("w1"));
        assert!(core.hosts["host"].candidate.is_none());
        drop(core);
        c.shutdown().await;
        // R2-06 death branch: durable file still has old token when candidate dies.
        let restarted = HostClient::load(file).unwrap();
        assert_eq!(
            restarted
                .store
                .host("host")
                .unwrap()
                .device_token
                .unwrap()
                .expose(),
            "new-device"
        );
        restarted.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn host_client_remove_cancels_late_ack_and_grants_reject_forgery() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (_dir, c) = client();
        begin(&c, KEY).await;
        let transaction = sink(&c).await;
        assert!(check_window_id("w1", Some("w2")).is_err());
        let grant = c.grant("w1", ORIGIN, "host").await.unwrap();
        let token = grant["grant"].as_str().unwrap();
        assert_eq!(
            c.grants.validate(token, "host", ORIGIN).as_deref(),
            Some("w1")
        );
        assert!(c.grants.validate(token, "other", ORIGIN).is_none());
        assert!(c
            .grants
            .validate(token, "host", "http://evil.test")
            .is_none());
        assert!(c.grants.validate("forged", "host", ORIGIN).is_none());
        c.remove("host").await.unwrap();
        assert!(c.grants.validate(token, "host", ORIGIN).is_none());
        assert!(transaction
            .persist(DeviceToken::new("late-token"))
            .await
            .is_err());
        assert!(c.store.host("host").is_none());
        assert!(c.core.lock().await.hosts.is_empty());
        c.shutdown().await;
        assert!(c.add("w1", ORIGIN, &offer(KEY)).await.is_err());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn host_client_release_and_destroy_stop_last_ref_but_keep_other_window() {
    tokio::time::timeout(Duration::from_secs(5), async {
        for destroyed in [false, true] {
            let (_dir, c) = client();
            begin(&c, KEY).await;
            sink(&c)
                .await
                .persist(DeviceToken::new("saved-device"))
                .await
                .unwrap();
            let first = c.grant("w1", ORIGIN, "host").await.unwrap();
            c.grant("w2", ORIGIN, "host").await.unwrap();
            let mut stopped = c.core.lock().await.hosts["host"]
                .active
                .as_ref()
                .unwrap()
                .handle
                .status
                .clone();
            if destroyed {
                c.revoke_window_now("w1");
            }
            c.close_window("w1").await;
            assert!(c
                .grants
                .validate(first["grant"].as_str().unwrap(), "host", ORIGIN)
                .is_none());
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
        }
    })
    .await
    .unwrap();
}
