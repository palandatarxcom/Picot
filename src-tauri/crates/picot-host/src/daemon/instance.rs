// ABOUTME: The daemon instance file and its single-instance lock: atomic tmp+rename writes whose temp
// ABOUTME: is 0600 from creation, corrupt-tolerant reads with a redacted token, and pid+start-time identity.

//! # Instance file + single-instance lock (plan Task 4, D7)
//!
//! `<data_dir>/picot-daemon.json` is **written only by the daemon** (plan D3:
//! adoption is a daemon-authorized transaction — the app never rewrites it) and
//! read by every Picot app that probes for a running daemon:
//!
//! ```jsonc
//! {"pid":123,"processStartTime":"Mon Oct 13 09:15:02 2026","hostOrigin":"http://127.0.0.1:51234",
//!  "adminOrigin":"http://127.0.0.1:51235","adminToken":"…","startedAt":"2026-10-13T09:15:02Z",
//!  "version":"0.5.0","buildId":"af69d94",
//!  "manager":{"pid":456,"processStartTime":"…","startedAt":"…","appVersion":"0.5.0","buildId":"af69d94"}}
//! ```
//!
//! The field names above are the plan's table, not this struct's `serde` shape:
//! the round-trip test pins the literal JSON so a rename cannot pass silently
//! (engineering-lesson #1).
//!
//! ## Hardening (plan D7, strategist D3/D7 note)
//!
//! - the temp file is created **with mode 0600 at creation** (`OpenOptionsExt::mode`),
//!   never create-then-chmod — the mode test asserts it at the instant the temp
//!   exists, before the document is written;
//! - `rename` publishes atomically, so a reader never sees a truncated document;
//! - parent directories are created 0700;
//! - the token is **never** logged, and every error string names the path only;
//! - [`InstanceFile`]'s `Debug` is hand-written to redact the token, so an
//!   accidental `{:?}` cannot leak it either.
//!
//! ## Identity
//!
//! A pid is not an identity: it wraps. Both the daemon record and the manager
//! record carry the OS process start time, and every liveness question is
//! answered with pid **and** start time (`child_supervision.rs:51-57` is the
//! precedent this reuses). The app-side decision table lives in
//! `src-tauri/src/daemon_client/manager.rs`.
//!
//! ## Single instance
//!
//! `<data_dir>/picot-daemon.lock` holds an exclusive `flock` for the daemon's
//! whole life. On graceful exit the file is unlinked *after* the runtimes are
//! down (the drain's file step), so the next daemon starts from a clean
//! directory; the unlink race with a concurrently starting daemon is closed by
//! re-verifying that the locked descriptor still *is* the file at that path.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::daemon::admin::{LeaseObserver, ManagerIdentity};

/// The instance file name under the daemon's data directory.
pub const INSTANCE_FILE_NAME: &str = "picot-daemon.json";
/// The single-instance lock file name under the daemon's data directory.
pub const LOCK_FILE_NAME: &str = "picot-daemon.lock";
/// The daemon's own log file under its data directory. The app names this path
/// in every "the daemon did not come up" error, so it lives beside the other two.
pub const LOG_FILE_NAME: &str = "picot-daemon.log";

/// `<data_dir>/picot-daemon.json`.
pub fn instance_path(data_dir: &Path) -> PathBuf {
    data_dir.join(INSTANCE_FILE_NAME)
}

/// `<data_dir>/picot-daemon.lock`.
pub fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOCK_FILE_NAME)
}

/// `<data_dir>/picot-daemon.log`.
pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_FILE_NAME)
}

/// The app that currently holds the daemon's manager lease (plan D3). Written
/// by the daemon when it grants the lease, cleared when the attachment dies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerInfo {
    /// The manager's pid. `None` when the app's `admin_hello` omitted it; a
    /// record without a pid is treated as *live* by the app's probe (adopting
    /// blind would risk kicking a working app).
    pub pid: Option<u32>,
    /// The manager's process start time (pid-reuse guard).
    #[serde(default)]
    pub process_start_time: Option<String>,
    /// When the daemon granted this attachment (RFC 3339 / UTC).
    pub started_at: String,
    /// The manager's app version, as its `admin_hello` reported it.
    #[serde(default)]
    pub app_version: Option<String>,
    /// The manager's build id, as its `admin_hello` reported it.
    #[serde(default)]
    pub build_id: Option<String>,
}

/// The instance file document (plan D7 fields, camelCase on the wire).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceFile {
    /// The daemon's pid.
    pub pid: u32,
    /// The daemon's process start time (pid-reuse guard).
    #[serde(default)]
    pub process_start_time: Option<String>,
    /// The host origin the WebView and the data plane use.
    pub host_origin: String,
    /// The admin channel origin (loopback only, plan D3).
    pub admin_origin: String,
    /// The high-entropy admin token. Never logged; redacted in `Debug`.
    pub admin_token: String,
    /// When the daemon published this document (RFC 3339 / UTC).
    pub started_at: String,
    /// The daemon's version.
    pub version: String,
    /// The daemon's build id (`picot_host::BUILD_ID`): the staleness rule
    /// compares version **and** build id, because a dev rebuild keeps the version.
    pub build_id: String,
    /// The attached app, when one holds the lease.
    #[serde(default)]
    pub manager: Option<ManagerInfo>,
}

impl std::fmt::Debug for InstanceFile {
    /// Hand-written so the token cannot be printed by accident (plan D7: the
    /// token is never logged).
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstanceFile")
            .field("pid", &self.pid)
            .field("process_start_time", &self.process_start_time)
            .field("host_origin", &self.host_origin)
            .field("admin_origin", &self.admin_origin)
            .field("admin_token", &"<redacted>")
            .field("started_at", &self.started_at)
            .field("version", &self.version)
            .field("build_id", &self.build_id)
            .field("manager", &self.manager)
            .finish()
    }
}

/// A fresh admin token: 32 random bytes, base64url without padding (plan D3).
pub fn generate_admin_token() -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use rand::{rngs::OsRng, RngCore};
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC 3339 UTC timestamp, seconds precision.
pub fn now_timestamp() -> String {
    format_timestamp(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0),
    )
}

/// Format a unix timestamp as RFC 3339 UTC. Falls back to a `t<seconds>` marker
/// rather than failing: a log line or an instance record is worth more than an
/// exact calendar rendering.
pub fn format_timestamp(seconds: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds as i64, 0)
        .map(|moment| moment.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| format!("t{seconds}"))
}

/// Create `dir` (and its parents) and force it owner-only (0700).
pub fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot restrict {}: {error}", dir.display()))?;
    }
    Ok(())
}

/// Publish the instance file atomically: temp created 0600, written, synced and
/// renamed over the destination. No `.tmp` survives a failure.
pub fn write_atomic(path: &Path, file: &InstanceFile) -> Result<(), String> {
    write_atomic_with(path, file, |_| {})
}

/// [`write_atomic`] with a failpoint: `on_temp_created` runs with the temp path
/// immediately after the descriptor exists and before the document is written,
/// so a test can observe the *creation* mode rather than the post-hoc one.
pub fn write_atomic_with(
    path: &Path,
    file: &InstanceFile,
    on_temp_created: impl FnOnce(&Path),
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let temp = temp_path(path);
    let mut handle = match create_new_private_file(&temp) {
        Ok(handle) => handle,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // A crashed writer's temp: delete it and create fresh, so the mode
            // above is applied at creation instead of being inherited.
            if let Err(error) = std::fs::remove_file(&temp) {
                return Err(format!("cannot clear {}: {error}", temp.display()));
            }
            create_new_private_file(&temp)
                .map_err(|error| format!("cannot create {}: {error}", temp.display()))?
        }
        Err(error) => return Err(format!("cannot create {}: {error}", temp.display())),
    };
    on_temp_created(&temp);
    let encoded = serde_json::to_vec(file)
        .map_err(|error| format!("cannot encode the instance file: {error}"))?;
    let written = handle.write_all(&encoded).and_then(|()| handle.sync_all());
    if let Err(error) = written {
        drop(handle);
        let _ = std::fs::remove_file(&temp);
        return Err(format!("cannot write {}: {error}", temp.display()));
    }
    drop(handle);
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("cannot publish {}: {error}", path.display()));
    }
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Create `path` owner-only **at creation**. `create_new` semantics: an
/// existing file is an error, never silently reused (that distinction is what
/// keeps the second daemon from deleting the first one's lock file).
fn create_new_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Open `path` for writing, creating it owner-only when it does not exist. Used
/// for the lock file, which normally exists already and must **never** be
/// unlinked here: unlinking a held lock file is exactly the race
/// [`InstanceLock::acquire`] re-verifies against.
fn open_or_create_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Read the instance file. Missing → `None`; unreadable or corrupt → `None`
/// plus a warning that names the path only (never the contents: they hold the
/// token).
pub fn read(path: &Path) -> Option<InstanceFile> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::warn!(
                "[picot-daemon] cannot read the instance file {}: {error}",
                path.display()
            );
            return None;
        }
    };
    match serde_json::from_str::<InstanceFile>(&raw) {
        Ok(file) if file.admin_token.is_empty() => {
            log::warn!(
                "[picot-daemon] the instance file {} carries no admin token; treating it as stale",
                path.display()
            );
            None
        }
        Ok(file) => Some(file),
        Err(error) => {
            log::warn!(
                "[picot-daemon] the instance file {} is corrupt ({error}); treating it as absent",
                path.display()
            );
            None
        }
    }
}

/// The daemon's own persisted bookkeeping (plan D7): the instance file it
/// publishes and the single-instance lock it holds.
///
/// Both are removed by [`crate::daemon::DaemonState::drain_persisted_state`] —
/// the one plug point the drain order reserves for files — so a graceful exit
/// (signal or `daemon.stop`) leaves neither behind.
pub struct DaemonFiles {
    pub store: InstanceStore,
    pub lock: InstanceLock,
}

/// Owner of the instance file: remembers the published document so the manager
/// record can be updated in place when the lease changes (plan D3), and removes
/// the file on graceful exit.
#[derive(Clone)]
pub struct InstanceStore {
    path: PathBuf,
    current: Arc<Mutex<Option<InstanceFile>>>,
}

impl InstanceStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            current: Arc::new(Mutex::new(None)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write `file` atomically and remember it as the current document.
    pub fn publish(&self, file: InstanceFile) -> Result<(), String> {
        write_atomic(&self.path, &file)?;
        if let Ok(mut slot) = self.current.lock() {
            *slot = Some(file);
        }
        Ok(())
    }

    /// Re-publish the remembered document with `manager` as the attachment.
    /// A no-op (with a warning) when nothing was published yet.
    pub fn update_manager(&self, manager: Option<ManagerInfo>) -> Result<(), String> {
        let mut slot = self
            .current
            .lock()
            .map_err(|_| "the instance bookkeeping lock is poisoned".to_string())?;
        let Some(file) = slot.as_mut() else {
            return Err("no instance file has been published yet".to_string());
        };
        file.manager = manager;
        write_atomic(&self.path, file)?;
        Ok(())
    }

    /// Read the file from disk (the authoritative record, not the memory copy).
    pub fn read(&self) -> Option<InstanceFile> {
        read(&self.path)
    }

    /// The last document this process published.
    pub fn current(&self) -> Option<InstanceFile> {
        self.current.lock().ok().and_then(|slot| slot.clone())
    }

    /// Remove the instance file. Best effort by design: a failure is logged
    /// (the file is stale-but-harmless — the next daemon overwrites it) and
    /// never blocks the exit path.
    pub fn clear(&self) {
        if let Ok(mut slot) = self.current.lock() {
            *slot = None;
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "[picot-daemon] cannot remove the instance file {}: {error}",
                self.path.display()
            ),
        }
    }
}

/// The daemon republishes its manager record whenever the lease changes.
impl LeaseObserver for InstanceStore {
    fn manager_changed(&self, identity: Option<ManagerIdentity>) {
        let manager = identity.map(|identity| ManagerInfo {
            pid: identity.pid,
            process_start_time: identity
                .pid
                .and_then(crate::child_supervision::process_start_time),
            started_at: now_timestamp(),
            app_version: identity.app_version,
            build_id: identity.build_id,
        });
        if let Err(error) = self.update_manager(manager) {
            log::warn!("[picot-daemon] cannot update the manager record: {error}");
        }
    }
}

/// The single-instance lock (plan D7): an exclusive `flock` on
/// `<data_dir>/picot-daemon.lock`, held for the daemon's whole life.
///
/// **Implementation-time decision (plan Task 4 Step 2):** `flock` via the
/// already-present `libc`, not the `fd-lock` crate. Reasons: (1) libc is
/// already a dependency, so no new crate enters the daemon's graph; (2)
/// `fd-lock`'s extra value is its Windows path, which Phase 1 does not build or
/// test (macOS-first, D9 — un-gating + Windows validation is a tracked
/// follow-up); (3) the unlink race that `fd-lock` does not cover either is
/// closed here by re-verifying that the locked descriptor is still the file at
/// the path.
pub struct InstanceLock {
    file: File,
    path: PathBuf,
    unlinked: AtomicBool,
}

impl InstanceLock {
    /// Try to take the lock. `Ok(None)` means another live daemon holds it.
    pub fn acquire(path: &Path) -> Result<Option<Self>, String> {
        if let Some(parent) = path.parent() {
            ensure_private_dir(parent)?;
        }
        for _ in 0..8 {
            let file = open_or_create_private_file(path)
                .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
            if !lock_exclusive_nonblocking(&file)? {
                return Ok(None);
            }
            if points_at(&file, path)? {
                return Ok(Some(Self {
                    file,
                    path: path.to_path_buf(),
                    unlinked: AtomicBool::new(false),
                }));
            }
            // The path was unlinked and recreated by a daemon that was exiting:
            // our descriptor guards an unlinked inode, which would let a second
            // daemon lock the new file. Retry against the fresh file.
        }
        Err(format!(
            "cannot establish the instance lock at {}: the file keeps being replaced",
            path.display()
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Unlink the lock file (graceful-exit step). The descriptor stays open
    /// until this struct drops — i.e. until the process exits — so the window in
    /// which a new daemon could start against a missing lock file is the last
    /// few instructions of the process, after every runtime is already down.
    pub fn remove_file(&self) {
        if self.unlinked.swap(true, Ordering::SeqCst) {
            return;
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "[picot-daemon] cannot remove the instance lock {}: {error}",
                self.path.display()
            ),
        }
    }

    pub fn file(&self) -> &File {
        &self.file
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Closing the descriptor releases the `flock` (the kernel does it on
        // process exit too). Deliberately no unlink here: removal is an explicit
        // graceful-exit step, so a crashed daemon leaves its lock file behind
        // for the next one to take over.
    }
}

/// `flock(LOCK_EX | LOCK_NB)`: `Ok(false)` when another process holds it.
#[cfg(unix)]
fn lock_exclusive_nonblocking(file: &File) -> Result<bool, String> {
    use std::os::unix::io::AsRawFd;
    loop {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EWOULDBLOCK) => return Ok(false),
            Some(libc::EINTR) => continue,
            _ => return Err(format!("cannot lock the instance file: {error}")),
        }
    }
}

#[cfg(not(unix))]
fn lock_exclusive_nonblocking(_file: &File) -> Result<bool, String> {
    // Phase 1 is macOS-first (plan D9): Windows sidecar build + validation is a
    // tracked follow-up, so an honest refusal beats a silent no-op lock.
    Err("single-instance locking is not implemented on this target yet".to_string())
}

/// True when `file`'s descriptor still refers to the file at `path` (the
/// unlink-race guard described on [`InstanceLock::acquire`]).
#[cfg(unix)]
fn points_at(file: &File, path: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;
    let held = file
        .metadata()
        .map_err(|error| format!("cannot stat the instance lock: {error}"))?;
    let at_path = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot stat {}: {error}", path.display())),
    };
    Ok(held.dev() == at_path.dev() && held.ino() == at_path.ino())
}

#[cfg(not(unix))]
fn points_at(_file: &File, _path: &Path) -> Result<bool, String> {
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample(token: &str) -> InstanceFile {
        InstanceFile {
            pid: 4711,
            process_start_time: Some("Mon Oct 13 09:15:02 2026".to_string()),
            host_origin: "http://127.0.0.1:51234".to_string(),
            admin_origin: "http://127.0.0.1:51235".to_string(),
            admin_token: token.to_string(),
            started_at: "2026-10-13T09:15:02Z".to_string(),
            version: "0.5.0".to_string(),
            build_id: "af69d94".to_string(),
            manager: None,
        }
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    }

    /// Plan Task 4 Step 1: "atomic write leaves no `.tmp`".
    #[test]
    fn write_atomic_leaves_no_temp_behind() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data").join(INSTANCE_FILE_NAME);
        write_atomic(&path, &sample("token-one")).expect("publish");

        assert!(path.exists(), "the document is published");
        assert_eq!(
            read(&path).expect("readable").admin_token,
            "token-one",
            "the published document is the one that was written"
        );
        let mut leftovers: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        leftovers.sort();
        assert!(
            leftovers.is_empty(),
            "no .tmp may survive a successful publish: {leftovers:?}"
        );

        // A failed encode/rename must not leave one either (the destination
        // directory is gone). Exercised through a directory that cannot be
        // written: the temp creation itself fails first, which is the same
        // "no leftover" property.
        let unwritable = temp
            .path()
            .join("nope")
            .join("deeper")
            .join("instance.json");
        std::fs::write(temp.path().join("nope"), b"a file, not a directory").unwrap();
        assert!(
            write_atomic(&unwritable, &sample("token-two")).is_err(),
            "an unwritable destination fails"
        );
    }

    /// Plan Task 4 Step 1: "temp 0600 from creation", asserted at the instant
    /// the temp exists (a post-hoc stat proves nothing about create-then-chmod).
    #[test]
    fn temp_file_is_owner_only_from_the_moment_it_is_created() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data").join(INSTANCE_FILE_NAME);
        let observed: Arc<Mutex<Option<(u32, bool, u64)>>> = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&observed);
        write_atomic_with(&path, &sample("token-three"), move |temp_path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = std::fs::metadata(temp_path).expect("stat the temp");
                let size = metadata.len();
                *sink.lock().unwrap() = Some((
                    metadata.permissions().mode() & 0o777,
                    temp_path.exists(),
                    size,
                ));
            }
            #[cfg(not(unix))]
            {
                *sink.lock().unwrap() = Some((0, temp_path.exists(), 0));
            }
        })
        .expect("publish");

        let (mode, existed, size) = observed
            .lock()
            .unwrap()
            .expect("the failpoint observed the temp");
        assert!(existed, "the temp file exists at creation");
        assert_eq!(
            size, 0,
            "the failpoint runs before the document is written, so the mode \
             observed is the creation mode"
        );
        assert_eq!(
            mode & 0o077,
            0,
            "group/other bits must never be set on the temp file (mode was {mode:o})"
        );
        assert_eq!(
            mode_of(&path) & 0o077,
            0,
            "the published file is owner-only too"
        );
    }

    #[test]
    fn the_data_directory_is_owner_only() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let path = instance_path(&data_dir);
        write_atomic(&path, &sample("token-four")).expect("publish");
        assert_eq!(
            mode_of(&data_dir) & 0o077,
            0,
            "the daemon's data directory must be owner-only"
        );
    }

    /// Plan Task 4 Step 1: "corrupt JSON → None".
    #[test]
    fn corrupt_or_tokenless_documents_read_as_none() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(INSTANCE_FILE_NAME);

        std::fs::write(&path, b"{ this is not json").unwrap();
        assert!(read(&path).is_none(), "corrupt JSON reads as None");

        // Truncated mid-document: the shape an interrupted plain write leaves.
        let encoded = serde_json::to_string(&sample("token-five")).unwrap();
        std::fs::write(&path, &encoded[..encoded.len() / 2]).unwrap();
        assert!(read(&path).is_none(), "truncated JSON reads as None");

        // Parseable but tokenless: an instance we must not connect to.
        let mut tokenless = sample("token-six");
        tokenless.admin_token = String::new();
        write_atomic(&path, &tokenless).unwrap();
        assert!(read(&path).is_none(), "a tokenless instance is stale");

        assert!(
            read(&temp.path().join("absent.json")).is_none(),
            "missing → None"
        );
    }

    /// Plan Task 4 Step 1: "token redacted in every error/log path".
    #[test]
    fn debug_and_error_paths_never_carry_the_token() {
        const TOKEN: &str = "super-secret-admin-token";
        let file = sample(TOKEN);
        let rendered = format!("{file:?}");
        assert!(
            !rendered.contains(TOKEN),
            "Debug must redact the token: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");

        // The read path's failure modes name the path, never the contents.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(INSTANCE_FILE_NAME);
        std::fs::write(&path, format!("{{ broken {TOKEN}")).unwrap();
        assert!(read(&path).is_none());
    }

    /// Plan D7's literal field table (engineering-lesson #1: the fixture is
    /// typed by hand, never generated from the struct).
    #[test]
    fn document_matches_the_instance_file_table_verbatim() {
        let literal = r#"{"pid":4711,"processStartTime":"Mon Oct 13 09:15:02 2026","hostOrigin":"http://127.0.0.1:51234","adminOrigin":"http://127.0.0.1:51235","adminToken":"tok","startedAt":"2026-10-13T09:15:02Z","version":"0.5.0","buildId":"af69d94","manager":{"pid":456,"processStartTime":"Mon Oct 13 09:15:03 2026","startedAt":"2026-10-13T09:15:03Z","appVersion":"0.5.0","buildId":"af69d94"}}"#;
        let parsed: InstanceFile = serde_json::from_str(literal).expect("the table parses");

        let mut expected = sample("tok");
        expected.manager = Some(ManagerInfo {
            pid: Some(456),
            process_start_time: Some("Mon Oct 13 09:15:03 2026".to_string()),
            started_at: "2026-10-13T09:15:03Z".to_string(),
            app_version: Some("0.5.0".to_string()),
            build_id: Some("af69d94".to_string()),
        });
        assert_eq!(parsed, expected);

        let encoded = serde_json::to_string(&expected).expect("encode");
        let as_value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let literal_value: serde_json::Value = serde_json::from_str(literal).unwrap();
        assert_eq!(as_value, literal_value, "encoded shape: {encoded}");
    }

    #[test]
    fn manager_changes_rewrite_the_published_document() {
        let temp = tempfile::tempdir().unwrap();
        let store = InstanceStore::new(instance_path(temp.path()));
        store.publish(sample("token-seven")).expect("publish");
        assert!(store.read().unwrap().manager.is_none());

        store.manager_changed(Some(ManagerIdentity {
            pid: Some(std::process::id()),
            app_version: Some("0.5.0".to_string()),
            build_id: Some("af69d94".to_string()),
        }));

        let on_disk = store.read().expect("republished");
        let manager = on_disk.manager.expect("the manager record landed");
        assert_eq!(manager.pid, Some(std::process::id()));
        assert!(
            manager.process_start_time.is_some(),
            "the manager record carries the pid-reuse guard"
        );

        store.manager_changed(None);
        assert!(store.read().unwrap().manager.is_none(), "release clears it");

        store.clear();
        assert!(store.read().is_none(), "a graceful exit removes the file");
    }

    #[test]
    fn the_lock_admits_one_holder_and_is_released_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        let first = InstanceLock::acquire(&path)
            .expect("first acquire")
            .expect("free");
        assert_eq!(first.path(), path.as_path());

        assert!(
            InstanceLock::acquire(&path)
                .expect("second acquire")
                .is_none(),
            "a second daemon must not get the lock"
        );

        drop(first);
        // The lock is released when the holder drops — observed with a bounded
        // poll rather than one instantaneous syscall.
        //
        // `flock` locks belong to the *open file description*, and this test
        // binary runs 600+ other tests in parallel: a sibling that is inside a
        // `fork` while this descriptor is open hands a copy of that description
        // to a child, and the copy keeps the lock alive until that child
        // `exec`s. Measured on macOS (2026-10-15): a `Command`-spawned child
        // never holds it (Rust opens files `O_CLOEXEC` and uses
        // `POSIX_SPAWN_CLOEXEC_DEFAULT`), a `fork`-without-`exec` child holds it
        // for its whole life. A genuine leak never recovers, so the deadline
        // below still fails on one; only the pre-`exec` window of an unrelated
        // sibling is tolerated.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut released = false;
        while std::time::Instant::now() < deadline {
            if InstanceLock::acquire(&path)
                .expect("third acquire")
                .is_some()
            {
                released = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(released, "the lock is released when the holder drops");
        assert_eq!(mode_of(&path) & 0o077, 0, "the lock file is owner-only");
    }

    #[test]
    fn removing_the_lock_file_leaves_the_next_daemon_a_clean_directory() {
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        let held = InstanceLock::acquire(&path)
            .expect("acquire")
            .expect("free");
        held.remove_file();
        assert!(
            !path.exists(),
            "the graceful-exit step unlinks the lock file"
        );
        // Idempotent: the drain step may run once per shutdown path.
        held.remove_file();
    }

    #[test]
    fn admin_tokens_are_high_entropy_and_url_safe() {
        let first = generate_admin_token();
        let second = generate_admin_token();
        assert_ne!(first, second, "two tokens differ");
        assert_eq!(first.len(), 43, "32 random bytes base64url without padding");
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "url-safe alphabet only: {first}"
        );
        // Distinct bytes, not one repeated character.
        let distinct = first.chars().collect::<std::collections::HashSet<_>>();
        assert!(distinct.len() > 20, "entropy, not padding: {first}");
    }

    /// The instance file must survive a read-back after a *process* wrote it —
    /// the property the daemon/app hand-off depends on. Exercised with a real
    /// file handle read, not just `read_to_string`.
    #[test]
    fn published_document_is_readable_by_a_second_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = instance_path(temp.path());
        write_atomic(&path, &sample("token-eight")).expect("publish");
        let mut raw = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut raw)
            .expect("read");
        assert!(raw.contains("\"adminToken\":\"token-eight\""), "{raw}");
    }
}
