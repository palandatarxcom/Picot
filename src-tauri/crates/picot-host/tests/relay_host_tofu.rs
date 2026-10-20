// ABOUTME: Contract tests for TOFU decisions and the pairing transaction (plan T3).
// ABOUTME: Exact matches auto-approve, every key/endpoint/TLS difference asks one
// ABOUTME: question at a time, and a staged change only lands after remote auth.

use std::fs;

use picot_host::relay::host_store::{
    hosts_path, DeviceToken, HostProfile, HostStore, StagedPairing, StagedState, StoreError,
};
use picot_host::relay::host_tofu::{
    decide, format_fingerprint, Answer, ConfirmKind, ConfirmationQueue, ConnectionChange,
    TofuDecision,
};
use picot_host::relay::pairing::{ConnectionOfferV2, RelayEndpoint};

const KEY_B64: &str = "3H1QXjHfKZWDqjXxrIhCNBQCGi5PZaXTVO06FSTBRHU=";

fn offer(server_id: &str, endpoint: &str, use_tls: bool, key_b64: &str) -> ConnectionOfferV2 {
    ConnectionOfferV2 {
        v: 2,
        server_id: server_id.to_string(),
        daemon_public_key_b64: key_b64.to_string(),
        relay: RelayEndpoint {
            endpoint: endpoint.to_string(),
            use_tls,
        },
        pairing_token: Some("picot_pair_0123456789abcdef0123456789abcdef".to_string()),
        pairing_token_expires_at: Some(1_771_000_000),
    }
}

fn offer_default() -> ConnectionOfferV2 {
    offer("server-a", "relay.example.com:443", true, KEY_B64)
}

fn saved(
    server_id: &str,
    endpoint: &str,
    use_tls: bool,
    key_b64: &str,
    token: Option<&str>,
) -> picot_host::relay::host_store::StoredHost {
    picot_host::relay::host_store::StoredHost {
        profile: HostProfile {
            server_id: server_id.to_string(),
            label: "relay.example.com".to_string(),
            relay: RelayEndpoint {
                endpoint: endpoint.to_string(),
                use_tls,
            },
            daemon_public_key_b64: key_b64.to_string(),
            added_at: 1_771_000_000,
        },
        device_token: token.map(DeviceToken::new),
        generation: 1,
    }
}

fn question(decision: TofuDecision) -> picot_host::relay::host_tofu::ConfirmQuestion {
    match decision {
        TofuDecision::Confirm(question) => question,
        TofuDecision::AutoApprove => panic!("expected a question"),
    }
}

#[test]
fn an_unknown_server_id_asks_for_a_new_host() {
    let asked = question(decide(&offer_default(), None));
    assert_eq!(asked.kind, ConfirmKind::NewHost);
    assert!(asked.changes.is_empty(), "a new host has nothing to diff");
    assert_eq!(asked.server_id, "server-a");
    assert_eq!(asked.relay_endpoint, "relay.example.com:443");
    assert_eq!(asked.key_fingerprint, format_fingerprint(KEY_B64));
}

#[test]
fn an_exact_match_with_a_held_token_auto_approves() {
    let saved = saved(
        "server-a",
        "relay.example.com:443",
        true,
        KEY_B64,
        Some("dev-token"),
    );
    assert_eq!(
        decide(&offer_default(), Some(&saved)),
        TofuDecision::AutoApprove
    );
}

#[test]
fn an_exact_match_without_a_token_still_asks() {
    // Spec §2.3: auto-approval needs the exact connection *and* a token on hand;
    // a revoked device re-pairs and sees its key once more.
    let saved = saved("server-a", "relay.example.com:443", true, KEY_B64, None);
    let asked = question(decide(&offer_default(), Some(&saved)));
    assert_eq!(asked.kind, ConfirmKind::ChangedConnection);
    assert!(
        asked.changes.is_empty(),
        "nothing changed except the missing token"
    );
}

#[test]
fn a_changed_daemon_key_reports_only_that_diff() {
    const OTHER_KEY: &str = "q83vEjRWeJq83vEjRWeJq83vEjRWeJq83vEjRWeJq80=";
    let saved = saved(
        "server-a",
        "relay.example.com:443",
        true,
        KEY_B64,
        Some("dev-token"),
    );
    let asked = question(decide(
        &offer("server-a", "relay.example.com:443", true, OTHER_KEY),
        Some(&saved),
    ));
    assert_eq!(asked.kind, ConfirmKind::ChangedConnection);
    assert_eq!(
        asked.changes,
        vec![ConnectionChange::DaemonKey {
            saved: format_fingerprint(KEY_B64),
            offered: format_fingerprint(OTHER_KEY),
        }]
    );
    assert_eq!(
        asked.key_fingerprint,
        format_fingerprint(OTHER_KEY),
        "the question shows the offered key"
    );
}

#[test]
fn a_changed_endpoint_and_tls_are_separate_diffs() {
    let saved = saved(
        "server-a",
        "relay.example.com:443",
        true,
        KEY_B64,
        Some("dev-token"),
    );
    let asked = question(decide(
        &offer("server-a", "other.example.com:8443", false, KEY_B64),
        Some(&saved),
    ));
    assert_eq!(
        asked.changes,
        vec![
            ConnectionChange::RelayEndpoint {
                saved: "relay.example.com:443".to_string(),
                offered: "other.example.com:8443".to_string(),
            },
            ConnectionChange::UseTls {
                saved: true,
                offered: false,
            },
        ]
    );

    let tls_only = question(decide(
        &offer("server-a", "relay.example.com:443", false, KEY_B64),
        Some(&saved),
    ));
    assert_eq!(
        tls_only.changes,
        vec![ConnectionChange::UseTls {
            saved: true,
            offered: false,
        }]
    );
}

#[test]
fn the_fingerprint_matches_paseo() {
    // Paseo `formatDaemonKeyFingerprint`: base64 punctuation dropped, groups of
    // four, first and last eight when longer than sixteen.
    assert_eq!(format_fingerprint(KEY_B64), "3H1Q XjHf … 6FST BRHU");
    assert_eq!(format_fingerprint("abcdefghijklmno"), "abcd efgh ijkl mno");
    assert_eq!(
        format_fingerprint("abcdefghijklmnop"),
        "abcd efgh ijkl mnop"
    );
    assert_eq!(
        format_fingerprint("abcdefghijklmnopq"),
        "abcd efgh … jklm nopq"
    );
    assert_eq!(format_fingerprint("sk+foo/bar=="), "skfo obar");
}

#[test]
fn one_question_at_a_time_supersedes_the_unanswered_one() {
    let mut queue = ConfirmationQueue::new();
    let first = question(decide(&offer_default(), None));
    assert_eq!(
        queue.ask(first),
        None,
        "the first question supersedes nothing"
    );
    let first_id = queue.pending().unwrap().id;

    let second_question = question(decide(
        &offer("server-b", "relay.example.com:443", true, KEY_B64),
        None,
    ));
    let superseded = queue.ask(second_question);
    assert_eq!(
        superseded,
        Some(first_id),
        "the newer question cancels the older"
    );
    assert_eq!(queue.pending().unwrap().question.server_id, "server-b");

    // The cancelled question can no longer be approved (unanswered = false).
    assert_eq!(
        queue.answer(first_id, true),
        None,
        "a stale answer is ignored"
    );
    let live_id = queue.pending().unwrap().id;
    assert_eq!(queue.answer(live_id, false), Some(Answer::Denied));

    assert_eq!(
        queue.ask(question(decide(&offer_default(), None))),
        None,
        "the queue is empty again"
    );
    let third_id = queue.pending().unwrap().id;
    assert_eq!(queue.answer(third_id, true), Some(Answer::Approved));
    assert!(
        queue.pending().is_none(),
        "an answered question leaves the queue"
    );
    assert_eq!(
        queue.ask(question(decide(&offer_default(), None))),
        None,
        "nothing was pending to supersede"
    );
}

fn open(dir: &std::path::Path) -> HostStore {
    HostStore::load(hosts_path(dir)).expect("store loads")
}

fn commit_original(store: &HostStore) {
    let mut staged = StagedPairing::begin(
        HostProfile {
            server_id: "server-a".to_string(),
            label: "relay.example.com".to_string(),
            relay: RelayEndpoint {
                endpoint: "relay.example.com:443".to_string(),
                use_tls: true,
            },
            daemon_public_key_b64: KEY_B64.to_string(),
            added_at: 1_771_000_000,
        },
        None,
    );
    staged
        .remote_authenticated(DeviceToken::new("dev-token-original"))
        .unwrap();
    staged.commit(store).unwrap();
}

fn candidate() -> HostProfile {
    HostProfile {
        server_id: "server-a".to_string(),
        label: "other.example.com".to_string(),
        relay: RelayEndpoint {
            endpoint: "other.example.com:8443".to_string(),
            use_tls: false,
        },
        daemon_public_key_b64: KEY_B64.to_string(),
        added_at: 1_771_000_100,
    }
}

#[test]
fn a_staged_change_needs_the_remote_auth_success_first() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    commit_original(&store);

    let mut staged = StagedPairing::begin(candidate(), Some(1));
    assert_eq!(*staged.state(), StagedState::AwaitingRemoteAuth);
    let error = staged.commit(&store).expect_err("no token, no commit");
    assert!(
        matches!(error, StoreError::InvalidStagedState { .. }),
        "got {error:?}"
    );
    assert_eq!(
        store.host("server-a").unwrap().profile.relay.endpoint,
        "relay.example.com:443"
    );

    staged
        .remote_authenticated(DeviceToken::new("dev-token-new"))
        .unwrap();
    assert_eq!(*staged.state(), StagedState::ReadyToCommit);
    let committed = staged.commit(&store).unwrap();
    assert_eq!(committed.generation, 2);
    assert_eq!(*staged.state(), StagedState::Committed { generation: 2 });
    assert_eq!(store.host("server-a").unwrap().profile, candidate());

    let error = staged
        .commit(&store)
        .expect_err("a committed transaction is closed");
    assert!(
        matches!(error, StoreError::InvalidStagedState { .. }),
        "got {error:?}"
    );
}

#[test]
fn cancel_and_a_stale_answer_leave_the_original_alone() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    commit_original(&store);
    let before = fs::read_to_string(hosts_path(temp.path())).unwrap();

    let mut staged = StagedPairing::begin(candidate(), Some(1));
    staged
        .remote_authenticated(DeviceToken::new("dev-token-new"))
        .unwrap();
    staged.cancel();
    assert_eq!(*staged.state(), StagedState::Abandoned);
    assert!(staged.token().is_none(), "the candidate token is dropped");
    let error = staged
        .commit(&store)
        .expect_err("a cancelled transaction cannot land");
    assert!(
        matches!(error, StoreError::InvalidStagedState { .. }),
        "got {error:?}"
    );

    assert_eq!(
        store.host("server-a").unwrap().profile.relay.endpoint,
        "relay.example.com:443",
        "the original profile stays usable"
    );
    assert_eq!(
        fs::read_to_string(hosts_path(temp.path())).unwrap(),
        before,
        "a cancel touches nothing on disk"
    );
}

/// The "exchange succeeded, save failed" recovery state (spec §5.2 contract 3,
/// R2-06): the remote token already exists, so only the local write can be
/// retried. Process death drops the candidate — the device then re-pairs.
#[test]
fn a_save_failure_keeps_the_candidate_for_a_retry_save() {
    let temp = tempfile::tempdir().unwrap();
    let store_dir = temp.path().join("store");
    let store = HostStore::load(hosts_path(&store_dir)).unwrap();
    commit_original(&store);

    // Inject the write failure: the store's directory becomes a regular file, so
    // the atomic publish cannot even create its temp.
    fs::remove_dir_all(&store_dir).unwrap();
    fs::write(&store_dir, b"not a directory").unwrap();

    let mut staged = StagedPairing::begin(candidate(), Some(1));
    staged
        .remote_authenticated(DeviceToken::new("dev-token-new"))
        .unwrap();
    let error = staged.commit(&store).expect_err("the write must fail");
    assert!(matches!(error, StoreError::Io { .. }), "got {error:?}");
    assert!(
        matches!(staged.state(), StagedState::SaveFailed { .. }),
        "state: {:?}",
        staged.state()
    );
    assert_eq!(
        staged.token().unwrap().expose(),
        "dev-token-new",
        "the remote token is kept for the retry"
    );
    assert_eq!(
        store.host("server-a").unwrap().profile.relay.endpoint,
        "relay.example.com:443",
        "the original profile is still in effect"
    );
    assert_eq!(
        store.host("server-a").unwrap().generation,
        1,
        "no generation bump"
    );
    assert!(
        fs::read(&store_dir).is_ok(),
        "nothing was published while the write failed"
    );

    // The retry is the same commit call once the filesystem is healthy again.
    fs::remove_file(&store_dir).unwrap();
    fs::create_dir(&store_dir).unwrap();
    let committed = staged.commit(&store).expect("the retry save lands");
    assert_eq!(committed.generation, 2);
    assert_eq!(store.host("server-a").unwrap().profile, candidate());
    assert_eq!(
        open(&store_dir).host("server-a").unwrap().profile,
        candidate(),
        "the retried save is durable"
    );
}
