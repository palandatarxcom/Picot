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
const ORIGIN: &str = "http://127.0.0.1:54321";
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
            c.status("w0", ORIGIN, "host", None).await.unwrap(),
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
        c.status("w1", ORIGIN, "host", c.capture("w1"))
            .await
            .unwrap();
        c.remove("host").await.unwrap();
        // A late ack for a removed host is ignored, not a save failure: the
        // assertion below is that nothing is resurrected.
        assert!(transaction
            .persist(DeviceToken::new("late-token"))
            .await
            .is_ok());
        assert!(c.store.host("host").is_none());
        assert!(c.core.lock().await.hosts.is_empty());
        c.shutdown().await;
        assert!(c.add("w1", ORIGIN, &offer(KEY)).await.is_err());
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
        c.status("w1", ORIGIN, "host", c.capture("w1"))
            .await
            .unwrap();
        c.status("w2", ORIGIN, "host", c.capture("w2"))
            .await
            .unwrap();
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
        c.status("w1", ORIGIN, "host", c.capture("w1"))
            .await
            .unwrap();
        c.status("w2", ORIGIN, "host", c.capture("w2"))
            .await
            .unwrap();
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
            c.status("w1", ORIGIN, "host", None).await.unwrap_err(),
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
        assert_eq!(
            c.status("w0", ORIGIN, "host", None).await.unwrap()["status"],
            "offline"
        );
        assert!(c.store.host("host").is_none());
        // A refused offer does not block the next one behind "pairing in progress".
        let pending = c.add("w1", ORIGIN, &offer(KEY2)).await.unwrap();
        assert_eq!(pending["status"], "tofuPending");
        c.confirm("w1", ORIGIN, pending["questionId"].as_u64().unwrap(), true)
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

#[tokio::test]
async fn cancelling_a_pairing_leaves_nothing_behind_to_deadlock_the_next_one() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (_dir, c) = client();
        begin(&c, KEY).await;
        let pending = c.add("w1", ORIGIN, &offer(KEY2)).await.unwrap();
        c.confirm("w1", ORIGIN, pending["questionId"].as_u64().unwrap(), false)
            .await
            .unwrap();
        {
            let core = c.core.lock().await;
            assert!(core.pending.is_none());
            // A candidate never reaches `host_list`, so nothing may survive a cancel.
            assert!(core
                .hosts
                .values()
                .all(|runtime| runtime.candidate.is_none()));
        }
        // The host is re-pairable straight away: no "pairing in progress" wall.
        let again = c.add("w1", ORIGIN, &offer(KEY)).await.unwrap();
        assert_eq!(again["status"], "tofuPending");
        c.confirm("w1", ORIGIN, again["questionId"].as_u64().unwrap(), true)
            .await
            .unwrap();
        sink(&c)
            .await
            .persist(DeviceToken::new("fresh-device"))
            .await
            .unwrap();
        assert!(c.store.host("host").is_some());
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_superseded_pairing_ack_is_dropped_instead_of_failing_the_new_one() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (_dir, c) = client();
        begin(&c, KEY).await;
        let stale = sink(&c).await;
        begin(&c, KEY).await;
        // A late ack for the replaced candidate is not a storage failure: reporting
        // one would paint the newest pairing as `saveFailed`.
        stale
            .persist(DeviceToken::new("late-device"))
            .await
            .unwrap();
        assert!(c.store.host("host").is_none());
        sink(&c)
            .await
            .persist(DeviceToken::new("fresh-device"))
            .await
            .unwrap();
        assert_eq!(
            c.store.host("host").unwrap().device_token.unwrap().expose(),
            "fresh-device"
        );
        c.shutdown().await;
    })
    .await
    .unwrap();
}

const GRANT: &str = "Host grant unavailable";
const STALE: &str = "Host window changed; reload the window";

/// Attaches `window` to the saved host and returns the grant the page receives.
async fn attach(c: &Arc<HostClient>, window: &str) -> Value {
    let captured = c.capture(window);
    c.status(window, ORIGIN, "host", captured).await.unwrap();
    c.grant(window, ORIGIN, "host", c.capture(window))
        .await
        .unwrap()
}
fn secret(payload: &Value) -> String {
    assert_eq!(payload["bridgeGrantVersion"], 1);
    let grant = payload["bridgeGrant"].as_str().unwrap().to_string();
    assert_eq!(grant.len(), 47, "bg1_ plus 43 base64url characters");
    assert!(grant.starts_with("bg1_"));
    assert!(payload["wsUrl"].as_str().unwrap().ends_with("/v2/ws"));
    assert!(!payload.to_string().contains("deviceToken"));
    grant
}
async fn paired() -> (tempfile::TempDir, Arc<HostClient>) {
    let (dir, c) = client();
    begin(&c, KEY).await;
    sink(&c)
        .await
        .persist(DeviceToken::new("saved-device"))
        .await
        .unwrap();
    (dir, c)
}

#[tokio::test]
async fn grants_are_per_window_secrets_for_the_current_host_only() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = paired().await;
        let a = secret(&attach(&c, "w1").await);
        let b = secret(&attach(&c, "w2").await);
        assert_ne!(a, b, "two windows on one host never share a secret");
        // Status polling is not issuance: the stored secret must not rotate.
        c.status("w1", ORIGIN, "host", c.capture("w1"))
            .await
            .unwrap();
        assert_eq!(
            c.capture("w1").unwrap().grant_for_tests(),
            Some(a.clone()),
            "host_status must not rotate the grant"
        );
        // A reconnect reissues: the new secret replaces the previous one.
        let reissued = secret(&attach(&c, "w1").await);
        assert_ne!(reissued, a);
        assert_eq!(c.capture("w1").unwrap().grant_for_tests(), Some(reissued));
        // Wrong host, wrong origin, unknown window and absent capture all refuse
        // with one fixed nonsecret error.
        assert_eq!(
            c.grant("w1", ORIGIN, "other", c.capture("w1"))
                .await
                .unwrap_err(),
            GRANT
        );
        assert_eq!(
            c.grant("w1", "http://127.0.0.1:1", "host", c.capture("w1"))
                .await
                .unwrap_err(),
            GRANT,
            "the captured page origin is part of the binding"
        );
        assert_eq!(
            c.grant("w3", ORIGIN, "host", c.capture("w3"))
                .await
                .unwrap_err(),
            GRANT
        );
        assert_eq!(
            c.grant("w1", ORIGIN, "host", None).await.unwrap_err(),
            GRANT
        );
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_revoked_binding_can_never_mint_again_but_its_neighbour_can() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = paired().await;
        secret(&attach(&c, "w1").await);
        secret(&attach(&c, "w2").await);
        let revoked = c.capture("w1").unwrap();
        // Invalidation without release: the record keeps its references and
        // loses its authority.
        assert!(c.revoke_binding_for_tests("w1"));
        assert!(!revoked.is_issuable());
        assert_eq!(revoked.grant_for_tests(), None, "the secret is dropped");
        assert_eq!(
            c.grant("w1", ORIGIN, "host", Some(revoked.clone()))
                .await
                .unwrap_err(),
            GRANT
        );
        // The bridge agrees, while the other window's binding keeps working.
        {
            let core = c.core.lock().await;
            let bridge = core.hosts["host"].active.as_ref().unwrap();
            assert!(bridge.handle.mint_grant(&revoked).is_err());
            assert!(bridge.handle.mint_grant(&c.capture("w2").unwrap()).is_ok());
        }
        assert_eq!(
            c.grant("w2", ORIGIN, "host", c.capture("w2"))
                .await
                .unwrap()["bridgeGrantVersion"],
            json!(1)
        );
        // A revoked binding is never re-authorized through status: the stale
        // capture is rejected, and a `None` capture cannot resurrect it either
        // because the revoked record still sits in the window's index slot.
        assert_eq!(
            c.status("w1", ORIGIN, "host", Some(revoked.clone()))
                .await
                .unwrap_err(),
            STALE_WINDOW
        );
        assert_eq!(
            c.status("w1", ORIGIN, "host", None).await.unwrap_err(),
            STALE_WINDOW
        );
        assert!(c
            .capture("w1")
            .is_some_and(|current| Arc::ptr_eq(&current, &revoked)));
        assert!(revoked.grant_for_tests().is_none());
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_delayed_grant_cannot_reauthorize_a_closed_or_switched_window() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = paired().await;
        secret(&attach(&c, "w1").await);
        secret(&attach(&c, "w2").await);
        let old = c.capture("w1").unwrap();
        let (entered, resume) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        // The command validated its caller and captured the binding, then waited.
        let task = {
            let c = c.clone();
            let (entered, resume) = (entered.clone(), resume.clone());
            let captured = Some(old.clone());
            tokio::spawn(async move {
                c.grant_paused("w1", ORIGIN, "host", captured, &entered, &resume)
                    .await
            })
        };
        entered.notified().await;
        c.close_window("w1").await;
        resume.notify_one();
        // Issuance reports its one fixed refusal; the paused record is gone, so
        // nothing is minted for it.
        assert_eq!(task.await.unwrap().unwrap_err(), GRANT);
        assert!(c.capture("w1").is_none(), "no grant resurrected a binding");
        assert!(old.grant_for_tests().is_none());
        // The binding-mutating status path obeys the same rule: it neither
        // reissues nor reacquires the old authorization.
        let (entered, resume) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        let task = {
            let c = c.clone();
            let (entered, resume) = (entered.clone(), resume.clone());
            let captured = Some(old.clone());
            tokio::spawn(async move {
                c.status_paused("w1", ORIGIN, "host", captured, &entered, &resume)
                    .await
            })
        };
        entered.notified().await;
        c.close_window("w2").await;
        resume.notify_one();
        assert_eq!(task.await.unwrap().unwrap_err(), STALE);
        assert!(c.capture("w1").is_none());
        let core = c.core.lock().await;
        // Both windows left: the last release stopped the shared bridge, and the
        // delayed status did not take a reference back.
        assert!(
            core.hosts["host"].active.is_none(),
            "the delayed status reacquired nothing"
        );
        assert!(core.hosts["host"].candidate.is_none());
        drop(core);
        assert!(c.core.lock().await.hosts["host"].active.is_none());
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn delayed_cleanup_cannot_remove_a_reopened_binding() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = paired().await;
        secret(&attach(&c, "w1").await);
        secret(&attach(&c, "w2").await);
        let old = c.capture("w1").unwrap();
        // The synchronous half of a close: authority withdrawn, refs released.
        let release = c.plan_close_for_tests("w1").await;
        // The user reopens the same label before the old teardown finishes.
        c.status("w1", ORIGIN, "host", c.capture("w1"))
            .await
            .unwrap();
        let replacement = c.capture("w1").unwrap();
        assert!(!Arc::ptr_eq(&old, &replacement));
        c.finish_release_for_tests(release).await;
        let after = c.capture("w1").expect("the reopened binding survives");
        assert!(
            Arc::ptr_eq(&after, &replacement),
            "late cleanup took the replacement"
        );
        assert!(after.is_issuable());
        assert!(!old.is_issuable());
        secret(&attach(&c, "w1").await);
        // The surviving neighbour window kept the shared bridge alive.
        let core = c.core.lock().await;
        let bridge = core.hosts["host"].active.as_ref().unwrap();
        assert_eq!(bridge.windows.len(), 2);
        assert_eq!(*bridge.handle.status.borrow(), HostBridgeStatus::Offline);
        drop(core);
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn synchronous_invalidation_kills_authorization_before_async_cleanup() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = paired().await;
        secret(&attach(&c, "w1").await);
        secret(&attach(&c, "w2").await);
        let revoked = c.invalidate_window("w1").expect("binding captured");
        // The synchronous boundary already ran: no issuance, no admission, no
        // secret — even though the async cleanup has not started.
        assert!(!revoked.is_issuable());
        assert!(revoked.grant_for_tests().is_none());
        assert!(
            c.capture("w1").is_none(),
            "the index entry left synchronously"
        );
        {
            let core = c.core.lock().await;
            let bridge = core.hosts["host"].active.as_ref().unwrap();
            assert!(bridge.handle.mint_grant(&revoked).is_err());
        }
        // A first attach for the same label is only legal AFTER the cleanup
        // released the references; this is exactly the Destroyed ordering.
        c.close_window_with("w1", Some(revoked.clone())).await;
        secret(&attach(&c, "w1").await);
        let replacement = c.capture("w1").unwrap();
        assert!(!Arc::ptr_eq(&revoked, &replacement));
        // The neighbour never noticed.
        assert!(c.grant("w2", ORIGIN, "host", c.capture("w2")).await.is_ok());
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn navigation_keeps_a_reconnect_and_invalidates_a_real_switch() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = paired().await;
        secret(&attach(&c, "w1").await);
        let record = c.capture("w1").unwrap();
        // Same origin, same host: the designed reconnect keeps the grant.
        assert!(c
            .invalidate_window_on_navigation("w1", ORIGIN, Some("host"))
            .is_none());
        assert!(c
            .capture("w1")
            .is_some_and(|current| Arc::ptr_eq(&current, &record)));
        // Same origin, different ?host=: a host switch.
        let switched = c
            .invalidate_window_on_navigation("w1", ORIGIN, Some("other"))
            .expect("host switch captured");
        assert!(Arc::ptr_eq(&switched, &record));
        assert!(!switched.is_issuable());
        assert!(c.capture("w1").is_none());
        // Production ordering: the navigation callback spawns this cleanup.
        c.close_window_with("w1", Some(switched.clone())).await;
        // A fresh attach is the new page's first command.
        secret(&attach(&c, "w1").await);
        let record = c.capture("w1").unwrap();
        // A different origin invalidates even without a host parameter.
        let migrated = c
            .invalidate_window_on_navigation("w1", "http://127.0.0.1:9", Some("host"))
            .expect("origin migration captured");
        assert!(Arc::ptr_eq(&migrated, &record));
        assert!(c.capture("w1").is_none());
        c.close_window_with("w1", Some(migrated.clone())).await;
        // A window with no binding has nothing to invalidate.
        assert!(c
            .invalidate_window_on_navigation("w9", ORIGIN, Some("host"))
            .is_none());
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_window_closed_before_its_first_attach_cannot_authorize_later() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = paired().await;
        // "w9" never attached: a first command captures "no binding yet".
        let captured = c.capture("w9");
        assert!(captured.is_none());
        // The window closes before that command resumes.
        assert!(c.invalidate_window("w9").is_none());
        assert_eq!(
            c.status("w9", ORIGIN, "host", captured).await.unwrap_err(),
            STALE_WINDOW
        );
        assert!(c.capture("w9").is_none(), "nothing was published");
        // Once the cleanup retires the tombstone, a genuinely new page may attach.
        c.close_window_with("w9", None).await;
        secret(&attach(&c, "w9").await);
        c.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn old_cleanup_cannot_release_a_replacement_that_reused_the_label() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, c) = paired().await;
        secret(&attach(&c, "w1").await);
        secret(&attach(&c, "w2").await);
        let old = c.invalidate_window("w1").expect("binding captured");
        // Before the old cleanup runs, a same-label attach is refused: the
        // tombstone is the authority boundary, not the async task's timing.
        assert_eq!(
            c.status("w1", ORIGIN, "host", None).await.unwrap_err(),
            STALE_WINDOW
        );
        c.close_window_with("w1", Some(old.clone())).await;
        // The replacement attaches and works, and the old record stays dead.
        let payload = attach(&c, "w1").await;
        secret(&payload);
        let replacement = c.capture("w1").unwrap();
        assert!(!Arc::ptr_eq(&old, &replacement));
        assert!(!old.is_issuable());
        // The neighbour window never lost its reference on the shared bridge.
        assert!(c.grant("w2", ORIGIN, "host", c.capture("w2")).await.is_ok());
        let core = c.core.lock().await;
        assert_eq!(core.hosts["host"].active.as_ref().unwrap().windows.len(), 2);
        drop(core);
        c.shutdown().await;
    })
    .await
    .unwrap();
}
