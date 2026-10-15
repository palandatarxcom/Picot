// ABOUTME: Host-owned JSON configuration contract with validation and atomic replacement.
// ABOUTME: Preserves bounded, redacted errors and prevents partial or insecure writes.

use serde_json::json;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const MAX_CONFIG_BYTES: usize = 512 * 1024;
const LOCK_STALE_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    InvalidJson,
    TooLarge,
    NotObject,
    Busy,
    RevisionConflict,
    Io(String),
}

impl ConfigError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_config",
            Self::TooLarge => "config_too_large",
            Self::NotObject => "config_object_required",
            Self::Busy => "config_busy",
            Self::RevisionConflict => "revision_conflict",
            Self::Io(_) => "config_access_failed",
        }
    }
}

pub fn read_json(path: &Path) -> Result<Value, ConfigError> {
    let file = fs::File::open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(ConfigError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    // The stat above can race a concurrent writer; take() keeps the read
    // bounded even when the file grows between the check and the read.
    file.take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| ConfigError::InvalidJson)?;
    if !value.is_object() {
        return Err(ConfigError::NotObject);
    }
    Ok(value)
}

/// Revision sentinel for a settings file that does not exist yet.
pub const MISSING_REVISION: &str = "absent";

/// SHA-256 of the exact file bytes, or `MISSING_REVISION`. This is the
/// transaction revision compared under the lock: byte-exact, so even an
/// external whitespace-only rewrite conflicts instead of being overwritten.
pub fn revision_of(path: &Path) -> String {
    match read_bounded(path) {
        Ok(Some(bytes)) => format!("{:x}", Sha256::digest(&bytes)),
        _ => MISSING_REVISION.to_string(),
    }
}

fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, ConfigError> {
    let meta = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
        Ok(meta) => meta,
    };
    if meta.file_type().is_symlink() {
        return Err(ConfigError::Io("config path is a symlink".to_owned()));
    }
    if !meta.is_file() {
        return Err(ConfigError::Io(
            "config path is not a regular file".to_owned(),
        ));
    }
    if meta.len() > MAX_CONFIG_BYTES as u64 {
        return Err(ConfigError::TooLarge);
    }
    let bytes = fs::read(path).map_err(io_error)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }
    Ok(Some(bytes))
}

// Test seams for the external-writer race and rename-failure paths.
// Production builds never observe them.
#[cfg(test)]
pub(crate) static TEST_HOOK_AFTER_EDIT: std::sync::Mutex<
    Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
> = std::sync::Mutex::new(None);
#[cfg(test)]
pub(crate) static TEST_HOOK_FAIL_RENAME_PATH: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);

/// Read–compare–edit–backup–write under ONE proper-lockfile hold. The plain
/// `read_json` + `write_json` pair locks only the write, so an external edit
/// between the two would be silently overwritten; here the bounded read, the
/// revision compare, the edit, the backup, the temp write, a final external-
/// byte recheck, and the rename all happen inside one critical section.
///
/// `root` bounds the write: every path component between `root` and the file
/// must be a real directory (symlinks reject) and the canonical parent must
/// stay under the canonical root, so a swapped symlink cannot escape it.
///
/// Lock interoperability: this is the same `<file>.lock` directory protocol
/// Pi and the legacy settings editor already use (see `acquire_lock`), so an
/// independent Pi writer holding the lock blocks here and vice versa.
///
/// Remaining TOCTOU limit (documented, same as Pi's own lock protocol): an
/// external writer that ignores the lock entirely can race the window between
/// the final byte recheck and the rename; the unconditional rename cannot
/// promise zero lost writes against writers that never take the lock.
pub fn update_json_locked(
    path: &Path,
    root: &Path,
    expected_revision: &str,
    edit: impl FnOnce(&mut Value) -> Result<(), ConfigError>,
) -> Result<String, ConfigError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::Io("config path has no parent".into()))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let lock = PathBuf::from(format!("{}.lock", path.display()));
    acquire_lock_retrying(&lock, LOCK_STALE_AFTER, Duration::from_secs(2))?;
    let _guard = LockGuard(&lock);
    let result = (|| {
        let canonical_root = root.canonicalize().map_err(io_error)?;
        reject_symlink_ancestry(path, &canonical_root)?;
        let bytes = read_bounded(path)?;
        let initial_revision = revision_of_bytes(&bytes);
        if initial_revision != expected_revision {
            return Err(ConfigError::RevisionConflict);
        }
        let mut value = match &bytes {
            Some(bytes) => {
                let parsed: Value =
                    serde_json::from_slice(bytes).map_err(|_| ConfigError::InvalidJson)?;
                if !parsed.is_object() {
                    return Err(ConfigError::NotObject);
                }
                parsed
            }
            None => json!({}),
        };
        edit(&mut value)?;
        if !value.is_object() {
            return Err(ConfigError::NotObject);
        }
        let encoded = serde_json::to_vec_pretty(&value)
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        if encoded.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::TooLarge);
        }
        #[cfg(test)]
        if TEST_HOOK_AFTER_EDIT.lock().is_ok() {
            let hook = TEST_HOOK_AFTER_EDIT.lock().unwrap().clone();
            if let Some(hook) = hook.as_ref() {
                hook();
            }
        }
        // Final external-byte recheck immediately before publication.
        if revision_of_bytes(&read_bounded(path)?) != initial_revision {
            return Err(ConfigError::RevisionConflict);
        }
        // Private, restorable backup of the prior bytes before any mutation.
        if let Some(bytes) = &bytes {
            let backup = backup_path(path);
            fs::write(&backup, bytes).map_err(|error| {
                ConfigError::Io(format!(
                    "backup to {} failed ({error}); prior settings left intact",
                    backup.display()
                ))
            })?;
            restrict_permissions(&backup)?;
        }
        let temporary = path.with_file_name(format!(
            ".{}.picot-tmp-{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            uuid::Uuid::new_v4()
        ));
        let write_result = (|| {
            fs::write(&temporary, &encoded).map_err(io_error)?;
            restrict_permissions(&temporary)?;
            #[cfg(test)]
            if TEST_HOOK_FAIL_RENAME_PATH
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .is_some_and(|target| target == path)
            {
                return Err(ConfigError::Io("injected rename failure".to_owned()));
            }
            fs::rename(&temporary, path).map_err(io_error)
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(format!("{:x}", Sha256::digest(&encoded)))
    })();
    result
}

/// RAII release of the proper-lockfile directory: a panic inside the
/// critical section must not strand the lock (a stranded dir would still be
/// reclaimed by the 10 s stale takeover, but an unwind must not need it).
struct LockGuard<'a>(&'a Path);

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_dir(self.0);
    }
}

fn revision_of_bytes(bytes: &Option<Vec<u8>>) -> String {
    match bytes {
        Some(bytes) => format!("{:x}", Sha256::digest(bytes)),
        None => MISSING_REVISION.to_string(),
    }
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.bak",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

/// Components between `root` and `path` must all be real directories; the
/// file itself must not be a symlink, and its canonical parent must remain
/// under the canonical root.
pub(crate) fn reject_symlink_ancestry(
    path: &Path,
    canonical_root: &Path,
) -> Result<(), ConfigError> {
    if path.file_name().is_some() {
        if let Ok(meta) = fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return Err(ConfigError::Io("config path is a symlink".to_owned()));
            }
        }
    }
    let mut current = path.to_path_buf();
    while let Some(parent) = current.parent() {
        if let Ok(meta) = fs::symlink_metadata(parent) {
            if meta.file_type().is_symlink() {
                return Err(ConfigError::Io("config parent is a symlink".to_owned()));
            }
        }
        // Compare canonically: the caller's root may itself be reached
        // through a symlinked prefix (e.g. /var -> /private/var on macOS).
        if parent
            .canonicalize()
            .is_ok_and(|parent| parent == canonical_root)
        {
            break;
        }
        current = parent.to_path_buf();
    }
    let canonical_parent = path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .ok_or_else(|| ConfigError::Io("config parent unavailable".to_owned()))?;
    if !canonical_parent.starts_with(canonical_root) {
        return Err(ConfigError::Io("config path escaped its root".to_owned()));
    }
    Ok(())
}

/// Lock acquisition with a bounded wait: two host mutations on the same file
/// serialize instead of the loser failing instantly. Stale takeover still
/// follows the shared 10 s threshold.
fn acquire_lock_retrying(
    lock: &Path,
    stale_after: Duration,
    wait: Duration,
) -> Result<(), ConfigError> {
    let start = SystemTime::now();
    loop {
        match acquire_lock(lock, stale_after) {
            Ok(()) => return Ok(()),
            Err(ConfigError::Busy) => {
                if SystemTime::now()
                    .duration_since(start)
                    .unwrap_or(Duration::ZERO)
                    >= wait
                {
                    return Err(ConfigError::Busy);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(other) => return Err(other),
        }
    }
}

pub fn write_text(path: &Path, content: &str) -> Result<(), ConfigError> {
    write_bytes(path, content.as_bytes(), LOCK_STALE_AFTER)
}

pub fn write_json(path: &Path, value: &Value) -> Result<(), ConfigError> {
    if !value.is_object() {
        return Err(ConfigError::NotObject);
    }
    let encoded =
        serde_json::to_vec_pretty(value).map_err(|error| ConfigError::Io(error.to_string()))?;
    write_bytes(path, &encoded, LOCK_STALE_AFTER)
}

fn write_bytes(path: &Path, encoded: &[u8], stale_after: Duration) -> Result<(), ConfigError> {
    if encoded.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::Io("config path has no parent".into()))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let lock = PathBuf::from(format!("{}.lock", path.display()));
    acquire_lock(&lock, stale_after)?;
    let temporary = path.with_file_name(format!(
        ".{}.picot-tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        fs::write(&temporary, encoded).map_err(io_error)?;
        restrict_permissions(&temporary)?;
        fs::rename(&temporary, path).map_err(io_error)
    })();
    let _ = fs::remove_dir(&lock);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Proper-lockfile parity: the lock must be *created* (not `create_dir_all`,
/// which succeeds on an existing directory and would provide no mutual
/// exclusion at all). A pre-existing lock is honored unless its directory
/// mtime is older than `stale_after`, matching the documented 10 s stale
/// takeover used by the legacy settings editor and the Pi process itself.
fn acquire_lock(lock: &Path, stale_after: Duration) -> Result<(), ConfigError> {
    match fs::create_dir(lock) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let stale = fs::symlink_metadata(lock)
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|held| SystemTime::now().duration_since(held).ok())
                .is_some_and(|age| age >= stale_after);
            if !stale {
                return Err(ConfigError::Busy);
            }
            let _ = fs::remove_dir(lock);
            fs::create_dir(lock).map_err(|_| ConfigError::Busy)
        }
        Err(error) => Err(io_error(error)),
    }
}

fn io_error(error: std::io::Error) -> ConfigError {
    ConfigError::Io(error.to_string())
}
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io_error)
}
#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_non_object_and_writes_private_json_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(
            write_json(&path, &Value::String("secret".into())),
            Err(ConfigError::NotObject)
        );
        write_json(&path, &serde_json::json!({"enabled": true})).unwrap();
        assert_eq!(read_json(&path).unwrap()["enabled"], true);
        assert!(!path.with_file_name("settings.json.lock").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn external_lock_is_honored_and_stale_locks_are_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::create_dir_all(dir.path().join("settings.json.lock")).unwrap();
        // A fresh external lock (another writer holds it) must block, not
        // silently proceed like create_dir_all would.
        assert_eq!(
            write_json(&path, &serde_json::json!({"a": 1})),
            Err(ConfigError::Busy)
        );
        // stale_after = 0 makes the held lock immediately reclaimable, which
        // is the same takeover path production uses after 10 s.
        write_bytes(&path, br#"{"a": 1}"#, Duration::ZERO).unwrap();
        assert_eq!(read_json(&path).unwrap()["a"], 1);
        assert!(!path.with_file_name("settings.json.lock").exists());
    }

    #[test]
    fn read_rejects_oversized_files_before_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "0".repeat(MAX_CONFIG_BYTES + 1)).unwrap();
        assert_eq!(read_json(&path), Err(ConfigError::TooLarge));
    }
}

#[test]
fn locked_update_creates_missing_file_and_returns_byte_revision() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let revision = update_json_locked(&path, dir.path(), MISSING_REVISION, |value| {
        value
            .as_object_mut()
            .unwrap()
            .insert("subagents".into(), json!({"agentOverrides": {}}));
        Ok(())
    })
    .unwrap();
    assert_eq!(revision, revision_of(&path));
    assert_eq!(
        read_json(&path).unwrap()["subagents"]["agentOverrides"],
        json!({})
    );
    assert_eq!(
        update_json_locked(&path, dir.path(), "wrong", |value| {
            value.as_object_mut().unwrap().insert("x".into(), json!(1));
            Ok(())
        })
        .unwrap_err(),
        ConfigError::RevisionConflict
    );
}

#[test]
fn locked_update_rejects_bad_shapes_and_leaves_original_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, "{\"a\": 1}").unwrap();
    // Malformed and non-object payloads reject unchanged.
    fs::write(&path, "not json").unwrap();
    let malformed = revision_of(&path);
    assert_eq!(
        update_json_locked(&path, dir.path(), &malformed, |value| {
            value.as_object_mut().unwrap().insert("x".into(), json!(1));
            Ok(())
        })
        .unwrap_err(),
        ConfigError::InvalidJson
    );
    fs::write(&path, "[1,2]").unwrap();
    let array = revision_of(&path);
    assert_eq!(
        update_json_locked(&path, dir.path(), &array, |value| {
            value.as_object_mut().unwrap().insert("x".into(), json!(1));
            Ok(())
        })
        .unwrap_err(),
        ConfigError::NotObject
    );
    // Oversized file rejects before any parse.
    fs::write(
        &path,
        format!("{{\"a\": \"{}\"}}", "x".repeat(MAX_CONFIG_BYTES)),
    )
    .unwrap();
    let oversized = revision_of(&path);
    assert_eq!(
        update_json_locked(&path, dir.path(), &oversized, |_| Ok(())).unwrap_err(),
        ConfigError::TooLarge
    );
    // Edit rejection leaves the original bytes untouched.
    fs::write(&path, "{\"a\": 1}").unwrap();
    let again = revision_of(&path);
    assert_eq!(
        update_json_locked(&path, dir.path(), &again, |_| {
            Err(ConfigError::Io("edit rejected".into()))
        })
        .unwrap_err()
        .code(),
        "config_access_failed"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\": 1}");
}

#[test]
fn locked_update_rejects_symlink_file_and_parent() {
    let dir = tempfile::tempdir().unwrap();
    let target_dir = dir.path().join("real");
    fs::create_dir_all(&target_dir).unwrap();
    let real = target_dir.join("settings.json");
    fs::write(&real, "{\"a\": 1}").unwrap();
    let via_parent_symlink = dir.path().join("linked");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target_dir, &via_parent_symlink).unwrap();
    #[cfg(unix)]
    {
        let path = via_parent_symlink.join("settings.json");
        let expected = format!("{:x}", Sha256::digest(fs::read(&real).unwrap()));
        assert_eq!(
            update_json_locked(&path, dir.path(), &expected, |_| Ok(()))
                .unwrap_err()
                .code(),
            "config_access_failed"
        );
    }
    #[cfg(unix)]
    {
        let link = dir.path().join("settings-link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let expected = format!("{:x}", Sha256::digest(fs::read(&real).unwrap()));
        assert_eq!(
            update_json_locked(&link, dir.path(), &expected, |_| Ok(()))
                .unwrap_err()
                .code(),
            "config_access_failed"
        );
    }
}

#[test]
fn locked_update_writes_private_backup_and_survives_rename_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, "{\"old\": true}").unwrap();
    let expected = revision_of(&path);
    *TEST_HOOK_FAIL_RENAME_PATH.lock().unwrap() = Some(path.clone());
    let failed = update_json_locked(&path, dir.path(), &expected, |value| {
        value
            .as_object_mut()
            .unwrap()
            .insert("new".into(), json!(2));
        Ok(())
    });
    *TEST_HOOK_FAIL_RENAME_PATH.lock().unwrap() = None;
    assert_eq!(failed.unwrap_err().code(), "config_access_failed");
    // Original intact, no stray temp, backup restorable and private.
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"old\": true}");
    assert!(fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .all(|entry| !entry.file_name().to_string_lossy().contains("picot-tmp")));
    let revision = update_json_locked(&path, dir.path(), &expected, |value| {
        value
            .as_object_mut()
            .unwrap()
            .insert("new".into(), json!(2));
        Ok(())
    })
    .unwrap();
    let backup = dir.path().join("settings.json.bak");
    assert_eq!(fs::read_to_string(&backup).unwrap(), "{\"old\": true}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(revision, revision_of(&path));
}

#[test]
fn locked_update_concurrent_mutations_serialize() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, "{\"count\": 0}").unwrap();
    // Host mutations recompute the expected revision and retry on conflict:
    // the lock serializes the critical sections, and an interleaved external
    // write surfaces as a conflict, never a lost update.
    let bump = |path: PathBuf| {
        std::thread::spawn(move || {
            for _ in 0..10 {
                let mut attempts = 0;
                loop {
                    let expected = revision_of(&path);
                    match update_json_locked(&path, path.parent().unwrap(), &expected, |value| {
                        let count = value["count"].as_u64().unwrap_or(0) + 1;
                        value
                            .as_object_mut()
                            .unwrap()
                            .insert("count".into(), json!(count));
                        Ok(())
                    }) {
                        Ok(_) => break,
                        Err(ConfigError::RevisionConflict) if attempts < 1000 => {
                            attempts += 1;
                            continue;
                        }
                        Err(other) => panic!("unexpected error: {other:?}"),
                    }
                }
            }
        })
    };
    let a = bump(path.clone());
    let b = bump(path.clone());
    a.join().unwrap();
    b.join().unwrap();
    // Both writers serialized: every increment landed, none lost.
    assert_eq!(read_json(&path).unwrap()["count"], json!(20));
}

#[test]
fn external_writer_conflicts_instead_of_being_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, "{\"v\": 1}").unwrap();
    // External edit BEFORE the lock: caller's expected revision is stale.
    let stale = revision_of(&path);
    fs::write(&path, "{\"v\": 2}").unwrap();
    assert_eq!(
        update_json_locked(&path, dir.path(), &stale, |value| {
            value.as_object_mut().unwrap().insert("x".into(), json!(1));
            Ok(())
        })
        .unwrap_err(),
        ConfigError::RevisionConflict
    );
    // External edit BETWEEN the edit and the final rename (an unlocked
    // writer): the recheck fails closed, the external bytes survive.
    let current = revision_of(&path);
    let hook_path = path.clone();
    *TEST_HOOK_AFTER_EDIT.lock().unwrap() = Some(std::sync::Arc::new(move || {
        fs::write(&hook_path, "{\"v\": 3}").unwrap();
    }));
    let raced = update_json_locked(&path, dir.path(), &current, |value| {
        value.as_object_mut().unwrap().insert("x".into(), json!(1));
        Ok(())
    });
    *TEST_HOOK_AFTER_EDIT.lock().unwrap() = None;
    assert_eq!(raced.unwrap_err(), ConfigError::RevisionConflict);
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"v\": 3}");
}

#[test]
fn locked_update_preserves_unrelated_json_and_prunes_empty_layers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(
        &path,
        r#"{
  "model": "anthropic/claude",
  "otherAgent": {"model": "x/y", "disabled": true, "tools": ["a"], "description": "d"},
  "subagents": {
    "disableBuiltins": false,
    "agentOverrides": {
      "existing.agent": {"model": "p/m", "thinking": false, "unknownField": 7}
    }
  }
}"#,
    )
    .unwrap();
    let expected = revision_of(&path);
    update_json_locked(&path, dir.path(), &expected, |value| {
        crate::subagents_settings::apply_override_edit(
            value,
            "target.agent",
            &json!({"op": "set", "value": "provider/model-id"}),
            &json!({"op": "keep"}),
        )
        .map_err(|code| ConfigError::Io(code.to_string()))
    })
    .unwrap();
    let after = read_json(&path).unwrap();
    assert_eq!(after["model"], "anthropic/claude");
    assert_eq!(after["otherAgent"]["disabled"], true);
    assert_eq!(after["otherAgent"]["tools"], json!(["a"]));
    assert_eq!(
        after["subagents"]["agentOverrides"]["existing.agent"],
        json!({"model": "p/m", "thinking": false, "unknownField": 7})
    );
    assert_eq!(
        after["subagents"]["agentOverrides"]["target.agent"],
        json!({"model": "provider/model-id"})
    );
    assert_eq!(after["subagents"]["disableBuiltins"], false);
    // Clearing every field prunes the empty entry and overrides layer —
    // but never unrelated keys at any level.
    let expected = revision_of(&path);
    update_json_locked(&path, dir.path(), &expected, |value| {
        crate::subagents_settings::apply_override_edit(
            value,
            "target.agent",
            &json!({"op": "clear"}),
            &json!({"op": "clear"}),
        )
        .map_err(|code| ConfigError::Io(code.to_string()))
    })
    .unwrap();
    let pruned = read_json(&path).unwrap();
    assert!(pruned["subagents"]["agentOverrides"]["target.agent"].is_null());
    assert!(pruned["subagents"]["agentOverrides"]["existing.agent"].is_object());
    assert_eq!(pruned["subagents"]["disableBuiltins"], false);
    assert_eq!(pruned["model"], "anthropic/claude");
    let expected = revision_of(&path);
    update_json_locked(&path, dir.path(), &expected, |value| {
        crate::subagents_settings::apply_override_edit(
            value,
            "existing.agent",
            &json!({"op": "clear"}),
            &json!({"op": "clear"}),
        )
        .map_err(|code| ConfigError::Io(code.to_string()))
    })
    .unwrap();
    let kept = read_json(&path).unwrap();
    // disableBuiltins survives; the entry keeps its unknown field, so it
    // is not empty and must not be pruned.
    assert!(kept["subagents"].is_object());
    assert_eq!(
        kept["subagents"]["agentOverrides"]["existing.agent"],
        json!({"unknownField": 7})
    );
    assert_eq!(kept["model"], "anthropic/claude");
}

#[test]
fn locked_update_busy_when_lock_held_and_invalid_layer_shapes_reject() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, "{\"a\": 1}").unwrap();
    let expected = revision_of(&path);
    // A held external lock (Pi or the legacy editor) blocks the whole
    // transaction after the bounded retry window — never a lock-free write.
    fs::create_dir_all(dir.path().join("settings.json.lock")).unwrap();
    let started = SystemTime::now();
    assert_eq!(
        update_json_locked(&path, dir.path(), &expected, |_| Ok(())).unwrap_err(),
        ConfigError::Busy
    );
    assert!(started.elapsed().unwrap() >= Duration::from_secs(1));
    fs::remove_dir_all(dir.path().join("settings.json.lock")).unwrap();
    // Invalid subagents / agentOverrides shapes reject unchanged.
    for bad in [
        r#"{"subagents": 5}"#,
        r#"{"subagents": {"agentOverrides": []}}"#,
        r#"{"subagents": {"agentOverrides": {"a.b": "str"}}}"#,
    ] {
        fs::write(&path, bad).unwrap();
        let revision = revision_of(&path);
        let error = update_json_locked(&path, dir.path(), &revision, |value| {
            crate::subagents_settings::apply_override_edit(
                value,
                "a.b",
                &json!({"op": "set", "value": "p/m"}),
                &json!({"op": "keep"}),
            )
            .map_err(ConfigError::Io)
        })
        .unwrap_err();
        assert_eq!(error.code(), "config_access_failed", "shape {bad}");
        assert_eq!(fs::read_to_string(&path).unwrap(), bad);
    }
}
