// ABOUTME: Contract tests for the desktop host-profile store (plan T3, spec §2.6).
// ABOUTME: Owner-only creation, atomic publish, write failure preserving the old
// ABOUTME: document, corruption as a single-track blocked state, generation guard.

use std::fs;
use std::path::Path;

use picot_host::relay::host_store::{hosts_path, DeviceToken, HostProfile, HostStore, StoreError};
use picot_host::relay::pairing::RelayEndpoint;

fn profile(server_id: &str) -> HostProfile {
    HostProfile {
        server_id: server_id.to_string(),
        label: "relay.example.com".to_string(),
        relay: RelayEndpoint {
            endpoint: "relay.example.com:443".to_string(),
            use_tls: true,
        },
        daemon_public_key_b64: "3H1QXjHfKZWDqjXxrIhCNBQCGi5PZaXTVO06FSTBRHU=".to_string(),
        added_at: 1_771_000_000,
    }
}

fn token() -> DeviceToken {
    DeviceToken::new("picot_dev_0123456789abcdef0123456789abcdef")
}

fn open(dir: &Path) -> HostStore {
    HostStore::load(hosts_path(dir)).expect("store loads")
}

fn commit_new(store: &HostStore, server_id: &str) {
    let mut staged = picot_host::relay::host_store::StagedPairing::begin(profile(server_id), None);
    staged.remote_authenticated(token()).expect("remote auth");
    staged.commit(store).expect("commit");
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_missing_file_creates_an_empty_store_with_a_stable_install_id() {
    let temp = tempfile::tempdir().unwrap();
    let path = hosts_path(temp.path());
    assert!(!path.exists(), "precondition: no store yet");

    let first = open(temp.path());
    assert!(first.hosts().is_empty());
    let install_id = first.app_device_id();
    assert!(
        !install_id.is_empty(),
        "the install id is minted on first load"
    );
    assert!(path.exists(), "the install id is persisted immediately");

    let second = open(temp.path());
    assert_eq!(
        second.app_device_id(),
        install_id,
        "a reload keeps the install-unique app device id"
    );
}

#[test]
fn a_committed_host_round_trips_through_a_fresh_load() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    let mut staged = picot_host::relay::host_store::StagedPairing::begin(profile("server-a"), None);
    staged.remote_authenticated(token()).unwrap();
    let committed = staged.commit(&store).unwrap();
    assert_eq!(
        committed.generation, 1,
        "a new host commits at generation 1"
    );

    let reloaded = open(temp.path());
    let hosts = reloaded.hosts();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].profile, profile("server-a"));
    assert_eq!(
        hosts[0].device_token.as_ref().map(DeviceToken::expose),
        Some(token().expose())
    );
    assert_eq!(hosts[0].generation, 1);
    assert_eq!(reloaded.app_device_id(), store.app_device_id());
}

#[cfg(unix)]
#[test]
fn the_store_file_and_its_directory_are_owner_only() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("app-config");
    let store = HostStore::load(hosts_path(&dir)).unwrap();
    commit_new(&store, "server-a");

    assert_eq!(mode_of(&hosts_path(&dir)), 0o600, "the document is 0600");
    assert_eq!(mode_of(&dir), 0o700, "the config directory is 0700");
}

#[test]
fn publishing_leaves_no_temp_file_and_keeps_json_readable() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    commit_new(&store, "server-a");

    let names: Vec<String> = fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["picot-hosts.json".to_string()],
        "no temp leftovers"
    );

    let raw = fs::read_to_string(hosts_path(temp.path())).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(
        parsed.get("appDeviceId").is_some(),
        "camelCase document: {raw}"
    );
    assert_eq!(parsed["hosts"][0]["profile"]["relay"]["useTls"], true);
}

/// Injection: `chflags uchg` makes the directory immutable for its owner, so the
/// publish fails *before* the temp file exists and the old document must survive
/// verbatim. Only macOS can express this failure without root.
#[cfg(target_os = "macos")]
#[test]
fn a_failed_write_keeps_the_old_file_and_the_old_profile() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    commit_new(&store, "server-a");
    let before = fs::read_to_string(hosts_path(temp.path())).unwrap();

    let flag = |value: &str| {
        let status = std::process::Command::new("chflags")
            .arg(value)
            .arg(temp.path())
            .status()
            .expect("chflags runs");
        assert!(status.success(), "chflags {value} failed");
    };
    flag("uchg");
    let mut staged = picot_host::relay::host_store::StagedPairing::begin(profile("server-b"), None);
    staged.remote_authenticated(token()).unwrap();
    let error = staged
        .commit(&store)
        .expect_err("an immutable directory rejects the write");
    assert!(matches!(error, StoreError::Io { .. }), "got {error:?}");
    flag("nouchg");

    assert_eq!(
        fs::read_to_string(hosts_path(temp.path())).unwrap(),
        before,
        "the old document is untouched"
    );
    assert!(
        store.host("server-b").is_none(),
        "memory never outran a failed write"
    );
    assert!(store.host("server-a").is_some());
}

#[test]
fn corruption_is_a_single_track_blocked_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = hosts_path(temp.path());
    let corrupt = b"{\"appDeviceId\": \"x\", \"hosts\": [ truncated";
    fs::write(&path, corrupt).unwrap();

    for attempt in 0..2 {
        let error = HostStore::load(path.clone()).expect_err("corrupt store never loads");
        assert!(
            matches!(error, StoreError::Corrupt { ref detail, .. } if !detail.is_empty()),
            "attempt {attempt}: got {error:?}"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            corrupt,
            "a corrupt store is preserved, never silently replaced"
        );
    }

    // Explicit recovery is the caller's act: deleting the bytes re-initialises.
    fs::remove_file(&path).unwrap();
    assert!(open(temp.path()).hosts().is_empty());
}

#[test]
fn staged_commits_are_generation_guarded() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    commit_new(&store, "server-a");
    assert_eq!(store.host("server-a").unwrap().generation, 1);

    let mut wrong =
        picot_host::relay::host_store::StagedPairing::begin(profile("server-a"), Some(0));
    wrong.remote_authenticated(token()).unwrap();
    let error = wrong
        .commit(&store)
        .expect_err("a stale generation must lose");
    assert!(
        matches!(
            error,
            StoreError::GenerationMismatch {
                expected: Some(0),
                actual: Some(1),
                ..
            }
        ),
        "got {error:?}"
    );
    assert_eq!(
        store.host("server-a").unwrap().generation,
        1,
        "no side effect"
    );

    let mut correct =
        picot_host::relay::host_store::StagedPairing::begin(profile("server-a"), Some(1));
    correct
        .remote_authenticated(DeviceToken::new("picot_dev_replacement"))
        .unwrap();
    assert_eq!(correct.commit(&store).unwrap().generation, 2);

    let mut duplicate =
        picot_host::relay::host_store::StagedPairing::begin(profile("server-a"), None);
    duplicate.remote_authenticated(token()).unwrap();
    let error = duplicate
        .commit(&store)
        .expect_err("an existing host needs its generation");
    assert!(
        matches!(
            error,
            StoreError::GenerationMismatch {
                expected: None,
                actual: Some(2),
                ..
            }
        ),
        "got {error:?}"
    );
}

#[test]
fn delete_removes_the_host_and_persists() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    commit_new(&store, "server-a");

    assert!(store.delete("server-a").unwrap());
    assert!(store.host("server-a").is_none());
    assert!(
        !store.delete("server-a").unwrap(),
        "a repeat delete is a no-op"
    );
    assert!(
        open(temp.path()).hosts().is_empty(),
        "the removal is durable"
    );
}

#[test]
fn debug_output_never_contains_the_token() {
    let temp = tempfile::tempdir().unwrap();
    let store = open(temp.path());
    commit_new(&store, "server-a");
    let host = store.host("server-a").unwrap();

    let rendered = format!("{host:?}");
    assert!(
        !rendered.contains(token().expose()),
        "a staged host token must not print: {rendered}"
    );
    assert!(
        rendered.contains("<redacted>"),
        "the redaction is visible: {rendered}"
    );
    assert!(!format!("{:?}", host.device_token.as_ref().unwrap()).contains(token().expose()));
    assert!(!format!("{:?}", token()).contains(token().expose()));
}

#[test]
fn deleting_a_directory_path_reports_io_not_corruption() {
    // A path that cannot be read is an I/O failure, not a corrupt document: the
    // caller must distinguish "restore the file" from "fix the filesystem".
    let temp = tempfile::tempdir().unwrap();
    let path = hosts_path(temp.path());
    fs::create_dir(&path).unwrap();
    let error = HostStore::load(path).expect_err("a directory is not a document");
    assert!(matches!(error, StoreError::Io { .. }), "got {error:?}");
}
