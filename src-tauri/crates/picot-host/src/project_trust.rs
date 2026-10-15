// ABOUTME: Pi-compatible project trust store: writes and nearest-entry
// ABOUTME: lookups over `<agent root>/trust.json` using Pi's lock protocol.

use crate::pi_launch;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

const LOCK_STALE_MS: u64 = 10_000;
const LOCK_RETRY_DELAY_MS: u64 = 20;
const LOCK_MAX_ATTEMPTS: usize = 750; // ~15s ceiling

/// Trust file inside the Pi agent root (Pi: `ProjectTrustStore.trustPath`).
fn trust_file(agent_root: &Path) -> PathBuf {
    agent_root.join("trust.json")
}

/// Pi's `normalizeCwd`: `realpathSync` with a graceful fallback to the input
/// path, plus the verbatim-prefix strip Picot applies before handing paths to
/// Pi so keys match byte-for-byte on Windows too.
fn canonical_trust_key(cwd: &Path) -> String {
    let resolved = cwd
        .canonicalize()
        .map(|canonical| canonical.to_string_lossy().into_owned())
        .unwrap_or_else(|_| cwd.to_string_lossy().into_owned());
    pi_launch::strip_verbatim_prefix(&resolved)
}

/// Read and validate the trust store. Values must be booleans or null, like
/// Pi's `readTrustFile`. An empty file is treated as an empty store (heals a
/// zero-byte file left by a crashed non-atomic Pi write) instead of erroring.
fn read_trust_map(agent_root: &Path) -> Result<Map<String, Value>, String> {
    let path = trust_file(agent_root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Map::new());
        }
        Err(error) => {
            return Err(format!("failed to read {}: {error}", path.display()));
        }
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let parsed: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|error| format!("invalid trust store {}: {error}", path.display()))?;
    let map = parsed
        .as_object()
        .cloned()
        .ok_or_else(|| format!("invalid trust store {}: expected an object", path.display()))?;
    for (key, value) in &map {
        if !value.is_boolean() && !value.is_null() {
            return Err(format!(
                "invalid trust store {}: value for {key:?} must be true, false, or null",
                path.display()
            ));
        }
    }
    Ok(map)
}

/// Write the store in Pi's exact format: keys sorted, two-space JSON, one
/// trailing newline. Serialized through a BTreeMap so key order is sorted
/// regardless of serde_json Map features. Atomic tmp+rename so concurrent
/// readers never observe a torn file.
fn write_trust_map(agent_root: &Path, map: &Map<String, Value>) -> Result<(), String> {
    let path = trust_file(agent_root);
    let sorted: BTreeMap<&str, &Value> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let mut text = serde_json::to_string_pretty(&sorted)
        .map_err(|error| format!("failed to serialize trust store: {error}"))?;
    text.push('\n');
    let tmp = agent_root.join(format!(
        "trust.json.picot-tmp-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    if let Err(error) = fs::write(&tmp, &text) {
        return Err(format!("failed to write {}: {error}", tmp.display()));
    }
    match fs::rename(&tmp, &path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(format!("failed to replace {}: {error}", path.display()))
        }
    }
}

/// Pi's proper-lockfile protocol, replicated: acquire by `create_dir` on an
/// empty `trust.json.lock` directory (atomic EEXIST = held), staleness by
/// directory mtime against a 10s threshold, release by `remove_dir`. No
/// mtime refresh: the critical section is a sub-second read→mutate→write.
fn with_trust_lock<T>(
    agent_root: &Path,
    critical: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    fs::create_dir_all(agent_root)
        .map_err(|error| format!("failed to create {}: {error}", agent_root.display()))?;
    let lock_dir = agent_root.join("trust.json.lock");
    for _ in 0..LOCK_MAX_ATTEMPTS {
        // create_dir (NOT create_dir_all): the single mkdir must fail with
        // AlreadyExists when another process holds the lock.
        match fs::create_dir(&lock_dir) {
            Ok(()) => {
                let result = critical();
                let _ = fs::remove_dir(&lock_dir);
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match fs::metadata(&lock_dir) {
                    Ok(metadata) => {
                        let stale = metadata
                            .modified()
                            .ok()
                            .map(|mtime| {
                                SystemTime::now()
                                    .duration_since(mtime)
                                    .unwrap_or(Duration::ZERO)
                                    >= Duration::from_millis(LOCK_STALE_MS)
                            })
                            .unwrap_or(true);
                        if stale {
                            // A crashed prior holder: clear and retry.
                            let _ = fs::remove_dir(&lock_dir);
                            continue;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        continue; // released meanwhile; retry the mkdir
                    }
                    Err(error) => {
                        return Err(format!(
                            "failed to inspect trust lock {}: {error}",
                            lock_dir.display()
                        ));
                    }
                }
                thread::sleep(Duration::from_millis(LOCK_RETRY_DELAY_MS));
            }
            Err(error) => {
                return Err(format!(
                    "failed to acquire trust lock {}: {error}",
                    lock_dir.display()
                ));
            }
        }
    }
    Err(format!(
        "timed out waiting for trust lock: {}",
        lock_dir.display()
    ))
}

/// Record `trusted: true` for the workspace under its canonical key,
/// preserving every other entry. Idempotent: an entry already `true` skips
/// the write entirely.
pub fn trust_project(agent_root: &Path, cwd: &Path) -> Result<(), String> {
    let key = canonical_trust_key(cwd);
    with_trust_lock(agent_root, || {
        let mut map = read_trust_map(agent_root)?;
        if map.get(&key).and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        map.insert(key, Value::Bool(true));
        write_trust_map(agent_root, &map)
    })
}

/// Pi's `findNearestTrustEntry` semantics: walk from the canonical project
/// root upward; the nearest `true`/`false` entry decides (a nearer explicit
/// `false` overrides a trusted parent); missing/null entries keep walking;
/// no entry anywhere means untrusted.
pub fn is_project_trusted(agent_root: &Path, cwd: &Path) -> bool {
    let Ok(map) = read_trust_map(agent_root) else {
        return false;
    };
    let mut current = PathBuf::from(canonical_trust_key(cwd));
    loop {
        let key = current.to_string_lossy().into_owned();
        if let Some(Value::Bool(trusted)) = map.get(&key) {
            return *trusted;
        }
        match current.parent() {
            Some(parent) if parent != current.as_path() => current = parent.to_path_buf(),
            _ => return false,
        }
    }
}

/// Best-effort trust write for a registered workspace. Registering or
/// opening a workspace through Picot is the explicit trust gesture; a write
/// failure only degrades to Pi's untrusted behavior, so it logs instead of
/// failing the workspace operation or launch.
pub fn trust_registered_workspace(cwd: &str) {
    let agent_root = match pi_launch::resolve_pi_agent_root() {
        Ok(root) => root,
        Err(error) => {
            log::warn!(
                "[picot-native] cannot resolve Pi agent root to record project trust: {error}"
            );
            return;
        }
    };
    if let Err(error) = trust_project(&agent_root, Path::new(cwd)) {
        log::warn!("[picot-native] failed to record project trust for {cwd}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    fn agent_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("agent root temp dir")
    }

    fn workspace_under(parent: &Path, name: &str) -> PathBuf {
        let dir = parent.join(name);
        fs::create_dir_all(&dir).expect("workspace dir");
        dir
    }

    fn key_of(path: &Path) -> String {
        canonical_trust_key(path)
    }

    fn backdate_mtime(path: &Path, age: Duration) {
        let file = fs::File::open(path).expect("open for set_times");
        let past = SystemTime::now()
            .checked_sub(age)
            .expect("backdate age within range");
        file.set_times(fs::FileTimes::new().set_modified(past))
            .expect("backdate mtime");
    }

    #[test]
    fn trust_project_writes_pi_format_entry() {
        let root = agent_root();
        let ws = workspace_under(root.path(), "proj");
        trust_project(root.path(), &ws).expect("trust write");
        let text = fs::read_to_string(trust_file(root.path())).expect("read trust.json");
        assert_eq!(text, format!("{{\n  \"{}\": true\n}}\n", key_of(&ws)));
        // The lock directory must be released after the write.
        assert!(!root.path().join("trust.json.lock").exists());
    }

    #[test]
    fn trust_project_is_idempotent_and_preserves_entries_sorted() {
        let root = agent_root();
        let ws = workspace_under(root.path(), "proj");
        let other = workspace_under(root.path(), "other");
        // Pre-seed like Pi would have written it: sorted, 2-space, newline.
        fs::write(
            trust_file(root.path()),
            "{\n  \"zz\": false,\n  \"aa\": true\n}\n",
        )
        .expect("seed trust.json");
        trust_project(root.path(), &ws).expect("first write");
        trust_project(root.path(), &ws).expect("second (idempotent) write");
        // Trusting an unrelated second project keeps both plus the seeds.
        trust_project(root.path(), &other).expect("third write");
        let text = fs::read_to_string(trust_file(root.path())).expect("read trust.json");
        // Key order matches Pi's sorted write: `/` (0x2F) sorts before
        // letters, so path keys precede the seeded `aa`/`zz` keys — the same
        // order JS `Object.keys().sort()` produces.
        assert_eq!(
            text,
            format!(
                "{{\n  \"{}\": true,\n  \"{}\": true,\n  \"aa\": true,\n  \"zz\": false\n}}\n",
                key_of(&other),
                key_of(&ws),
            )
        );
        assert!(!root.path().join("trust.json.lock").exists());
    }

    #[test]
    fn trust_project_recovers_from_stale_lock_and_empty_file() {
        let root = agent_root();
        let ws = workspace_under(root.path(), "proj");
        // A crashed holder left an 11s-old lock dir and a zero-byte store.
        let lock_dir = root.path().join("trust.json.lock");
        fs::create_dir(&lock_dir).expect("seed stale lock");
        backdate_mtime(&lock_dir, Duration::from_millis(LOCK_STALE_MS + 1000));
        fs::write(trust_file(root.path()), "").expect("seed empty trust.json");
        trust_project(root.path(), &ws).expect("write past stale lock");
        let text = fs::read_to_string(trust_file(root.path())).expect("read trust.json");
        assert_eq!(text, format!("{{\n  \"{}\": true\n}}\n", key_of(&ws)));
        assert!(!lock_dir.exists());
    }

    #[test]
    fn is_project_trusted_walks_parents_with_nearest_decision() {
        let root = agent_root();
        let parent = workspace_under(root.path(), "parent");
        let child = workspace_under(&parent, "child");
        // Trusted parent → child inherits trust.
        trust_project(root.path(), &parent).expect("trust parent");
        assert!(is_project_trusted(root.path(), &child));
        assert!(is_project_trusted(root.path(), &parent));
        // A nearer explicit false overrides the trusted parent...
        fs::write(
            trust_file(root.path()),
            format!(
                "{{\n  \"{}\": true,\n  \"{}\": false\n}}\n",
                key_of(&parent),
                key_of(&child)
            ),
        )
        .expect("seed override");
        assert!(!is_project_trusted(root.path(), &child));
        // ...and null entries are skipped, so the parent decision applies.
        fs::write(
            trust_file(root.path()),
            format!(
                "{{\n  \"{}\": true,\n  \"{}\": null\n}}\n",
                key_of(&parent),
                key_of(&child)
            ),
        )
        .expect("seed null");
        assert!(is_project_trusted(root.path(), &child));
        // No entries at all → untrusted.
        fs::remove_file(trust_file(root.path())).expect("remove trust.json");
        assert!(!is_project_trusted(root.path(), &child));
    }

    #[test]
    fn canonical_trust_key_strips_windows_verbatim_prefix_on_fallback() {
        // canonicalize fails (path does not exist here), so the fallback must
        // still strip the verbatim prefix exactly like the launch path does.
        assert_eq!(
            canonical_trust_key(Path::new(r"\\?\C:\workspace")),
            r"C:\workspace".to_string()
        );
    }
}
