#![cfg_attr(not(test), allow(dead_code))]
// ABOUTME: Picot application metadata database (workspaces registry,
// paired devices, global preferences). Single-writer store shared by
// RemoteAuth and broker controls behind one Arc<Mutex<MetadataStore>>.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

// HISTORY: v1 introduced workspaces/paired_devices/preferences. Other
// branches may carry a v2 with extra tables; v3 adds sidebar-registry
// columns to workspaces. v4–v6 add Picot Corp tables (company account
// profile, GitLab bindings, company install ledger) owned by Corp builds:
// this build accepts those databases read-compatibly and never creates or
// alters Corp tables. Migrations stay strictly additive (see
// ARCHITECTURE.md persistence).
/// Highest metadata schema this build can open. Databases stamped above this
/// reject startup (fail-closed N-1 rule); v4–v6 Corp databases open normally.
const SCHEMA_VERSION: i64 = 6;
/// Schema level this build's own migration completes and stamps. Never above
/// the shape it actually creates, so a Corp binary never mistakes a
/// public-created database for a completed Corp schema.
const WRITTEN_SCHEMA_VERSION: i64 = 3;

/// A registered workspace row as exposed to the frontend (camelCase).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRow {
    pub workspace_id: String,
    pub canonical_path: String,
    pub display_name: Option<String>,
    pub pinned: bool,
    pub last_opened_at: Option<i64>,
    pub session_bucket: Option<String>,
}

/// A registry row removed by automatic prune because its directory vanished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedWorkspace {
    pub workspace_id: String,
    pub canonical_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AddWorkspaceError {
    PathNotFound(String),
    NotADirectory(String),
    Db(String),
}

impl AddWorkspaceError {
    /// Stable machine-readable code surfaced over broker controls.
    pub fn code(&self) -> &'static str {
        match self {
            AddWorkspaceError::PathNotFound(_) => "path_not_found",
            AddWorkspaceError::NotADirectory(_) => "not_a_directory",
            AddWorkspaceError::Db(_) => "db_error",
        }
    }
}

impl std::fmt::Display for AddWorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddWorkspaceError::PathNotFound(detail) => write!(f, "path_not_found: {detail}"),
            AddWorkspaceError::NotADirectory(detail) => write!(f, "not_a_directory: {detail}"),
            AddWorkspaceError::Db(detail) => write!(f, "{detail}"),
        }
    }
}

/// Failure from a read-only workspace registry lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceLookupError {
    NotRegistered,
    Db(String),
}

impl WorkspaceLookupError {
    /// Stable machine-readable code for authority callers.
    pub fn code(&self) -> &'static str {
        match self {
            WorkspaceLookupError::NotRegistered => "not_registered",
            WorkspaceLookupError::Db(_) => "db_error",
        }
    }
}

impl std::fmt::Display for WorkspaceLookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceLookupError::NotRegistered => write!(f, "not_registered"),
            WorkspaceLookupError::Db(detail) => write!(f, "{detail}"),
        }
    }
}

/// Capability held only by host-internal rollout code. Public preference
/// controls never receive this marker.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RuntimePreferenceAuthorization(());

/// Mint the capability used by the rollout-authorized host path.
pub(crate) fn rollout_authorization() -> RuntimePreferenceAuthorization {
    RuntimePreferenceAuthorization(())
}

pub struct MetadataStore {
    connection: Connection,
    path: PathBuf,
    runtime_audit_events: Vec<String>,
}

/// App-level single store handle. One SQLite connection per process; every
/// consumer (RemoteAuth pairing, broker workspace/preference controls,
/// lifecycle touch) shares this exact instance.
pub type SharedMetadataStore = std::sync::Arc<std::sync::Mutex<MetadataStore>>;

impl MetadataStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        match Self::open_without_recovery(path) {
            Ok(store) => Ok(store),
            Err(first) => {
                // R4.11 graceful degradation: only corruption-class failures
                // (unreadable header / integrity check) quarantine the file
                // and retry once. Newer-schema rejection, busy/locked,
                // permission and IO errors keep their original semantics —
                // a healthy database is never quarantined.
                if !Self::file_reports_corruption(path) {
                    return Err(first);
                }
                let Some(quarantine_name) = Self::quarantine_database_file(path) else {
                    return Err(first);
                };
                log::warn!(
                    "[picot] metadata database quarantined as {quarantine_name} and recreated; \
                     registry/preferences rebuild on demand (sessions are unaffected)"
                );
                Self::open_without_recovery(path).map_err(|retry| {
                    format!("Picot metadata recovery failed after quarantine: {retry}")
                })
            }
        }
    }

    fn open_without_recovery(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Cannot create Picot metadata directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        let connection = Connection::open(path).map_err(|error| {
            format!(
                "Cannot open Picot metadata database {}: {error}",
                path.display()
            )
        })?;
        let mut store = Self {
            connection,
            path: path.to_path_buf(),
            runtime_audit_events: Vec::new(),
        };
        store.migrate()?;
        store.restrict_permissions()?;
        Ok(store)
    }

    /// True when the file at `path` fails SQLite's own integrity probe with a
    /// corruption-class error. Used only on the failure path of `open` so
    /// busy/locked, permission, IO and newer-schema rejections are never
    /// misclassified as corruption.
    fn file_reports_corruption(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        let Ok(connection) = Connection::open(path) else {
            return false;
        };
        match connection.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0)) {
            Ok(_) => false,
            Err(rusqlite::Error::SqliteFailure(failure, _)) => {
                // SQLITE_CORRUPT (11) and SQLITE_NOTADB (26) are stable
                // primary codes; both families signal a damaged file.
                matches!(failure.extended_code, 11 | 26)
            }
            Err(_) => false,
        }
    }

    /// Renames a corrupt database next to itself (bytes retained) and returns
    /// the quarantine file name for a redacted log line. Returns `None` when
    /// the rename fails — user data is then left untouched and the original
    /// error propagates.
    fn quarantine_database_file(path: &Path) -> Option<String> {
        let file_name = path.file_name()?.to_string_lossy().into_owned();
        let parent = path.parent()?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let quarantine = parent.join(format!("{file_name}.corrupt-{nanos}"));
        std::fs::rename(path, &quarantine).ok()?;
        quarantine
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }

    fn migrate(&mut self) -> Result<(), String> {
        let current: i64 = self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| format!("Cannot read Picot metadata schema version: {error}"))?;
        if current > SCHEMA_VERSION {
            return Err(format!(
                "Picot metadata schema {current} is newer than supported schema {SCHEMA_VERSION}"
            ));
        }
        // Corp v4–v6 databases keep their version: Corp tables are owned by
        // Corp builds. Public-owned workspace columns remain additive and must
        // exist at every accepted schema level.
        if current >= WRITTEN_SCHEMA_VERSION {
            self.ensure_workspace_session_bucket_column()?;
            self.ensure_reset_credit_operations_table()?;
            self.sweep_abandoned_reset_credits()?;
            self.drop_session_sidebar_visibility_table()?;
            return Ok(());
        }

        // Version staircase runs entirely inside one transaction; user_version
        // advances only on commit, so a mid-staircase failure leaves the DB
        // untouched and retryable.
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| format!("Cannot start Picot metadata migration: {error}"))?;

        // Step 0 → v1: baseline tables. IF NOT EXISTS keeps v1/v2 data and any
        // v2-era extra tables completely untouched (strictly additive rule).
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS workspaces (
                    workspace_id TEXT PRIMARY KEY,
                    canonical_path TEXT NOT NULL UNIQUE,
                    created_at INTEGER NOT NULL DEFAULT (unixepoch())
                );
                CREATE TABLE IF NOT EXISTS paired_devices (
                    device_id TEXT PRIMARY KEY,
                    token_hash BLOB NOT NULL UNIQUE,
                    paired_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    revoked_at INTEGER
                );
                CREATE TABLE IF NOT EXISTS preferences (
                    key TEXT PRIMARY KEY,
                    value_json TEXT NOT NULL
                );",
            )
            .map_err(|error| format!("Cannot migrate Picot metadata schema: {error}"))?;

        // Step v1|v2 → v3: add registry columns individually because SQLite has
        // no ADD COLUMN IF NOT EXISTS. Reaching here on a fresh v0 DB means the
        // baseline CREATE above guaranteed the workspaces table exists first.
        let existing_columns =
            {
                let mut statement = transaction
                    .prepare("PRAGMA table_info(workspaces)")
                    .map_err(|error| format!("Cannot inspect Picot workspaces schema: {error}"))?;
                let mapped = statement
                    .query_map([], |row| row.get::<_, String>(1))
                    .map_err(|error| format!("Cannot inspect Picot workspaces schema: {error}"))?;
                let mut names = Vec::new();
                for name in mapped {
                    names.push(name.map_err(|error| {
                        format!("Cannot inspect Picot workspaces schema: {error}")
                    })?);
                }
                names
            };
        let additions: [(&str, &str); 4] = [
            ("display_name", "TEXT"),
            ("pinned", "INTEGER NOT NULL DEFAULT 0"),
            ("last_opened_at", "INTEGER"),
            ("session_bucket", "TEXT"),
        ];
        for (column, declaration) in additions {
            if !existing_columns
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(column))
            {
                transaction
                    .execute(
                        &format!("ALTER TABLE workspaces ADD COLUMN {column} {declaration}"),
                        [],
                    )
                    .map_err(|error| {
                        format!("Cannot extend Picot workspaces schema ({column}): {error}")
                    })?;
            }
        }

        transaction
            .execute_batch(&format!("PRAGMA user_version = {WRITTEN_SCHEMA_VERSION};"))
            .map_err(|error| format!("Cannot finalize Picot metadata schema version: {error}"))?;

        transaction
            .commit()
            .map_err(|error| format!("Cannot commit Picot metadata migration: {error}"))?;

        // Public-owned additions apply to every accepted schema level, so a
        // freshly migrated database needs the same existence-based pass.
        self.ensure_workspace_session_bucket_column()?;
        self.ensure_reset_credit_operations_table()?;
        self.sweep_abandoned_reset_credits()?;
        self.drop_session_sidebar_visibility_table()
    }

    fn ensure_workspace_session_bucket_column(&self) -> Result<(), String> {
        let mut statement = self
            .connection
            .prepare("PRAGMA table_info(workspaces)")
            .map_err(|error| format!("Cannot inspect Picot workspaces schema: {error}"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| format!("Cannot inspect Picot workspaces schema: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Cannot inspect Picot workspaces schema: {error}"))?;
        if columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case("session_bucket"))
        {
            return Ok(());
        }
        self.connection
            .execute("ALTER TABLE workspaces ADD COLUMN session_bucket TEXT", [])
            .map_err(|error| {
                format!("Cannot extend Picot workspaces schema (session_bucket): {error}")
            })?;
        Ok(())
    }

    /// Codex reset-credit ledger (spec 2026-09-22): public-owned table
    /// created existence-based, never via the version staircase — Corp
    /// builds own schema v4–v6, public stamps v3 (session_bucket precedent).
    /// Rows carry only id/timestamps/status; no credentials, no upstream
    /// payloads. The id doubles as the upstream redeem_request_id idempotency
    /// key, so an unresolved (pending|ambiguous) row must be REUSED, not
    /// replaced — a fresh uuid would double-spend the credit.
    fn ensure_reset_credit_operations_table(&self) -> Result<(), String> {
        self.connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS reset_credit_operations (
                    id TEXT PRIMARY KEY,
                    opened_at INTEGER NOT NULL,
                    settled_at INTEGER,
                    status TEXT NOT NULL
                );",
            )
            .map_err(|error| format!("Cannot create Picot reset-credit ledger: {error}"))?;
        Ok(())
    }

    fn now_secs() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Open (or reuse) the one unresolved reset-credit operation.
    pub fn reset_credit_open(&self) -> Result<String, String> {
        // One conditional INSERT instead of read-then-insert: two concurrent
        // opens must never leave two unresolved rows behind (an orphan row
        // would let a later session replay a credit this one already used).
        // SQLite serializes writers, so the second statement observes the
        // first row and inserts nothing.
        let candidate = uuid::Uuid::new_v4().to_string();
        let inserted = self
            .connection
            .execute(
                "INSERT INTO reset_credit_operations (id, opened_at, settled_at, status)
                 SELECT ?1, ?2, NULL, 'pending'
                 WHERE NOT EXISTS (
                     SELECT 1 FROM reset_credit_operations
                     WHERE status IN ('pending', 'ambiguous')
                 )",
                rusqlite::params![candidate, Self::now_secs()],
            )
            .map_err(|error| format!("Cannot write Picot reset-credit ledger: {error}"))?;
        if inserted == 1 {
            return Ok(candidate);
        }
        // An unresolved row already exists: reuse it (the id doubles as the
        // upstream idempotency key, so a fresh uuid would double-spend).
        self.connection
            .query_row(
                "SELECT id FROM reset_credit_operations
                 WHERE status IN ('pending', 'ambiguous')
                 ORDER BY opened_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("Cannot read Picot reset-credit ledger: {error}"))
    }

    /// Settle an operation as `settled` or mark it `ambiguous` (response
    /// lost mid-flight; recovery re-checks before replaying the same id).
    pub fn reset_credit_settle(&self, operation_id: &str, status: &str) -> Result<(), String> {
        if !matches!(status, "settled" | "ambiguous") {
            return Err(format!("Invalid reset-credit settle status: {status}"));
        }
        let settled_at = if status == "settled" {
            Self::now_secs()
        } else {
            0
        };
        let changed = self
            .connection
            .execute(
                "UPDATE reset_credit_operations SET status = ?2, settled_at = ?3 WHERE id = ?1",
                rusqlite::params![operation_id, status, settled_at],
            )
            .map_err(|error| format!("Cannot update Picot reset-credit ledger: {error}"))?;
        if changed == 0 {
            return Err("Unknown reset-credit operation".to_owned());
        }
        Ok(())
    }

    /// Startup sweep: a pending row whose opener process is long gone can
    /// never be settled — mark it abandoned so the next open proceeds.
    pub fn sweep_abandoned_reset_credits(&self) -> Result<u64, String> {
        let swept = self
            .connection
            .execute(
                "UPDATE reset_credit_operations SET status = 'abandoned'
                 WHERE status = 'pending' AND opened_at < ?1",
                rusqlite::params![Self::now_secs() - 60],
            )
            .map_err(|error| format!("Cannot sweep Picot reset-credit ledger: {error}"))?;
        Ok(swept as u64)
    }

    /// The session sidebar visibility cache table is deprecated (Dr. Lin,
    /// 2026-09-17): sidebar classification is a live scan of the session
    /// files, so the historical cache is dropped once at every open —
    /// idempotent, and no code path recreates it.
    fn drop_session_sidebar_visibility_table(&self) -> Result<(), String> {
        self.connection
            .execute_batch("DROP TABLE IF EXISTS session_sidebar_visibility;")
            .map_err(|error| format!("Cannot drop session sidebar visibility cache: {error}"))
    }

    #[cfg(unix)]
    fn restrict_permissions(&self) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                format!(
                    "Cannot restrict metadata permissions {}: {error}",
                    self.path.display()
                )
            },
        )
    }

    #[cfg(not(unix))]
    fn restrict_permissions(&self) -> Result<(), String> {
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64, String> {
        self.connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| format!("Cannot read Picot metadata schema version: {error}"))
    }

    /// All workspaces ordered for the sidebar: pinned first, then most
    /// recently opened. Rows whose directory disappeared are deleted (DB row
    /// only, never filesystem content) and reported back for user feedback.
    pub fn list_workspaces_and_prune(
        &mut self,
    ) -> Result<(Vec<WorkspaceRow>, Vec<RemovedWorkspace>), String> {
        let rows = self.list_workspace_rows()?;
        let mut removed = Vec::new();
        for row in &rows {
            let vanished = std::fs::metadata(Path::new(&row.canonical_path))
                .map(|metadata| !metadata.is_dir())
                .unwrap_or(true);
            if vanished {
                let deleted = self
                    .connection
                    .execute(
                        "DELETE FROM workspaces WHERE workspace_id = ?1",
                        [&row.workspace_id],
                    )
                    .map_err(|error| format!("Cannot prune Picot workspace row: {error}"))?;
                if deleted > 0 {
                    removed.push(RemovedWorkspace {
                        workspace_id: row.workspace_id.clone(),
                        canonical_path: row.canonical_path.clone(),
                    });
                }
            }
        }
        if removed.is_empty() {
            return Ok((rows, removed));
        }
        Ok((self.list_workspace_rows()?, removed))
    }

    fn list_workspace_rows(&self) -> Result<Vec<WorkspaceRow>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT workspace_id, canonical_path, display_name, pinned, last_opened_at, session_bucket
                 FROM workspaces
                 ORDER BY pinned DESC, last_opened_at DESC",
            )
            .map_err(|error| format!("Cannot list Picot workspaces: {error}"))?;
        let mapped = statement
            .query_map([], |row| {
                Ok(WorkspaceRow {
                    workspace_id: row.get(0)?,
                    canonical_path: row.get(1)?,
                    display_name: row.get(2)?,
                    pinned: row.get::<_, i64>(3)? != 0,
                    last_opened_at: row.get(4)?,
                    session_bucket: row.get(5)?,
                })
            })
            .map_err(|error| format!("Cannot list Picot workspaces: {error}"))?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row.map_err(|error| format!("Cannot list Picot workspaces: {error}"))?);
        }
        Ok(rows)
    }

    /// Register a workspace directory. Identity is the host-canonicalized
    /// absolute path; re-registering an existing path returns its row with
    /// `added = false`. Never writes outside the database.
    pub fn add_workspace(&self, path: &Path) -> Result<(WorkspaceRow, bool), AddWorkspaceError> {
        let canonical = match path.canonicalize() {
            Ok(canonical) => canonical,
            Err(error) => {
                return Err(AddWorkspaceError::PathNotFound(format!(
                    "{}: {error}",
                    path.display()
                )))
            }
        };
        if !canonical.is_dir() {
            return Err(AddWorkspaceError::NotADirectory(format!(
                "{}",
                canonical.display()
            )));
        }
        let canonical_text = canonical.to_string_lossy().to_string();
        if let Some(existing) = self
            .workspace_row_by_path(&canonical_text)
            .map_err(AddWorkspaceError::Db)?
        {
            return Ok((existing, false));
        }
        let id = Uuid::new_v4().to_string();
        let display_name = canonical
            .file_name()
            .map(|name| name.to_string_lossy().to_string());
        self.connection
            .execute(
                "INSERT INTO workspaces (workspace_id, canonical_path, display_name)
                 VALUES (?1, ?2, ?3)",
                params![id, canonical_text.as_str(), display_name],
            )
            .map_err(|error| AddWorkspaceError::Db(format!("Cannot store workspace: {error}")))?;
        let row = self
            .workspace_row_by_id(&id)
            .map_err(AddWorkspaceError::Db)?
            .ok_or_else(|| {
                AddWorkspaceError::Db("Stored workspace row immediately vanished".to_string())
            })?;
        Ok((row, true))
    }

    pub fn is_valid_session_bucket_name(name: &str) -> bool {
        name.starts_with("--")
            && name.ends_with("--")
            && name.len() > 4
            && !name.contains(['/', '\\', '\0', ':'])
    }

    /// Store the bucket returned by Pi. Pi is authoritative, including when
    /// replacing a bucket written by an older Picot version.
    pub fn set_workspace_session_bucket_from_pi(
        &mut self,
        workspace_id: &str,
        session_bucket: &str,
    ) -> Result<bool, String> {
        if !Self::is_valid_session_bucket_name(session_bucket) {
            return Err("invalid_session_bucket".to_string());
        }
        let changed = self
            .connection
            .execute(
                "UPDATE workspaces
                 SET session_bucket = ?2
                 WHERE workspace_id = ?1 AND (session_bucket IS NULL OR session_bucket != ?2)",
                params![workspace_id, session_bucket],
            )
            .map_err(|error| format!("Cannot update Picot workspace session bucket: {error}"))?;
        Ok(changed == 1)
    }

    /// Remove a registry row. Returns whether a row was deleted. Sessions and
    /// directories are intentionally left untouched.
    pub fn remove_workspace(&mut self, workspace_id: &str) -> Result<bool, String> {
        let deleted = self
            .connection
            .execute(
                "DELETE FROM workspaces WHERE workspace_id = ?1",
                [workspace_id],
            )
            .map_err(|error| format!("Cannot remove Picot workspace row: {error}"))?;
        Ok(deleted > 0)
    }

    /// Update the pinned flag. Unknown ids yield `Ok(None)`; callers surface
    /// a stable not-found error rather than creating phantom rows.
    pub fn set_workspace_pinned(
        &mut self,
        workspace_id: &str,
        pinned: bool,
    ) -> Result<Option<WorkspaceRow>, String> {
        self.connection
            .execute(
                "UPDATE workspaces SET pinned = ?2 WHERE workspace_id = ?1",
                params![workspace_id, pinned],
            )
            .map_err(|error| format!("Cannot update Picot workspace pin: {error}"))?;
        self.workspace_row_by_id(workspace_id)
    }

    /// Record an open/switch of an already-registered workspace. Takes a
    /// host-verified canonical path; unregistered paths return false and
    /// never create rows, so browser-supplied ids cannot grow the registry.
    pub fn touch_registered_path(&mut self, canonical_path: &Path) -> Result<bool, String> {
        let Some(row) = self
            .workspace_row_by_path(&canonical_path.to_string_lossy())
            .map_err(|error| format!("Cannot look up Picot workspace: {error}"))?
        else {
            return Ok(false);
        };
        self.connection
            .execute(
                "UPDATE workspaces SET last_opened_at = unixepoch() WHERE workspace_id = ?1",
                [&row.workspace_id],
            )
            .map_err(|error| format!("Cannot touch Picot workspace: {error}"))?;
        Ok(true)
    }

    pub fn get_workspace(&mut self, workspace_id: &str) -> Result<Option<WorkspaceRow>, String> {
        self.workspace_row_by_id(workspace_id)
    }

    fn workspace_row_by_id(&self, workspace_id: &str) -> Result<Option<WorkspaceRow>, String> {
        self.connection
            .query_row(
                "SELECT workspace_id, canonical_path, display_name, pinned, last_opened_at, session_bucket
                 FROM workspaces WHERE workspace_id = ?1",
                [workspace_id],
                |row| {
                    Ok(WorkspaceRow {
                        workspace_id: row.get(0)?,
                        canonical_path: row.get(1)?,
                        display_name: row.get(2)?,
                        pinned: row.get::<_, i64>(3)? != 0,
                        last_opened_at: row.get(4)?,
                        session_bucket: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("Cannot load Picot workspace row: {error}"))
    }

    fn workspace_row_by_path(&self, canonical_path: &str) -> Result<Option<WorkspaceRow>, String> {
        self.connection
            .query_row(
                "SELECT workspace_id, canonical_path, display_name, pinned, last_opened_at, session_bucket
                 FROM workspaces WHERE canonical_path = ?1",
                [canonical_path],
                |row| {
                    Ok(WorkspaceRow {
                        workspace_id: row.get(0)?,
                        canonical_path: row.get(1)?,
                        display_name: row.get(2)?,
                        pinned: row.get::<_, i64>(3)? != 0,
                        last_opened_at: row.get(4)?,
                        session_bucket: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("Cannot load Picot workspace row: {error}"))
    }

    /// Return whether registry contains at least one workspace without
    /// canonicalizing, pruning, or mutating the database. Startup uses this
    /// gate before selecting a Registered native target.
    pub fn has_registered_workspaces(&self) -> Result<bool, String> {
        self.connection
            .query_row("SELECT EXISTS(SELECT 1 FROM workspaces)", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|value| value != 0)
            .map_err(|error| format!("Cannot inspect Picot workspace registry: {error}"))
    }

    pub fn workspace_id_for_path(&mut self, workspace: &Path) -> Result<String, String> {
        self.add_workspace(workspace)
            .map(|(row, _added)| row.workspace_id)
            .map_err(|error| error.to_string())
    }

    /// Look up a registered workspace without canonicalizing, creating, or
    /// touching a registry row. Callers must provide the host-canonical root.
    pub fn workspace_id_for_canonical_root(
        &self,
        root: &Path,
    ) -> Result<String, WorkspaceLookupError> {
        self.workspace_row_by_path(&root.to_string_lossy())
            .map_err(WorkspaceLookupError::Db)?
            .map(|row| row.workspace_id)
            .ok_or(WorkspaceLookupError::NotRegistered)
    }

    /// Resolve a registered workspace id to its canonical root. This is a
    /// read-only authority lookup and deliberately does not check filesystem
    /// existence or prune the registry row.
    pub fn canonical_root_for_workspace_id(
        &self,
        workspace_id: &str,
    ) -> Result<PathBuf, WorkspaceLookupError> {
        self.workspace_row_by_id(workspace_id)
            .map_err(WorkspaceLookupError::Db)?
            .map(|row| PathBuf::from(row.canonical_path))
            .ok_or(WorkspaceLookupError::NotRegistered)
    }

    pub fn store_device_token(&mut self, device_id: &str, token: &str) -> Result<(), String> {
        let token_hash = token_hash(token);
        self.connection
            .execute(
                "INSERT INTO paired_devices (device_id, token_hash, revoked_at)
                 VALUES (?1, ?2, NULL)
                 ON CONFLICT(device_id) DO UPDATE SET
                   token_hash = excluded.token_hash,
                   paired_at = unixepoch(),
                   revoked_at = NULL",
                params![device_id, token_hash],
            )
            .map_err(|error| format!("Cannot store paired device: {error}"))?;
        Ok(())
    }

    pub fn verify_device_token(&self, token: &str) -> Result<bool, String> {
        let token_hash = token_hash(token);
        self.connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM paired_devices WHERE token_hash = ?1 AND revoked_at IS NULL
                )",
                [token_hash],
                |row| row.get(0),
            )
            .map_err(|error| format!("Cannot verify paired device: {error}"))
    }

    pub fn revoke_device(&mut self, device_id: &str) -> Result<(), String> {
        self.connection
            .execute(
                "UPDATE paired_devices SET revoked_at = unixepoch() WHERE device_id = ?1",
                [device_id],
            )
            .map_err(|error| format!("Cannot revoke paired device: {error}"))?;
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), String> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| format!("Cannot start metadata reset: {error}"))?;
        transaction
            .execute_batch(
                "DELETE FROM workspaces; DELETE FROM paired_devices; DELETE FROM preferences;",
            )
            .map_err(|error| format!("Cannot reset Picot metadata: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Cannot commit Picot metadata reset: {error}"))
    }

    /// Read rollout state for the host launch path. Any authorization,
    /// schema, storage, JSON, or value-shape failure maps to `None`, which
    /// means legacy remains selected.
    pub(crate) fn runtime_pref_get(
        &self,
        authorization: Option<&RuntimePreferenceAuthorization>,
    ) -> Option<bool> {
        authorization?;
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT value_json FROM preferences WHERE key = 'runtime.native_origin'",
                [],
                |row| row.get(0),
            )
            .optional()
            .ok()?;
        let json = json?;
        serde_json::from_str::<serde_json::Value>(&json)
            .ok()?
            .as_bool()
    }

    /// Write rollout state through the host-internal path only. Audit output
    /// intentionally contains operation status, never preference key/value.
    pub(crate) fn runtime_pref_set(
        &mut self,
        authorization: Option<&RuntimePreferenceAuthorization>,
        value: bool,
    ) -> Result<(), String> {
        if authorization.is_none() {
            return Err("rollout authorization required".to_string());
        }
        self.connection
            .execute(
                "INSERT INTO preferences (key, value_json) VALUES ('runtime.native_origin', ?1)
                 ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
                [serde_json::to_string(&value).map_err(|error| {
                    format!("Cannot serialize runtime rollout preference: {error}")
                })?],
            )
            .map_err(|error| format!("Cannot write runtime rollout preference: {error}"))?;
        let event = "runtime_preference_write status=success".to_string();
        log::info!("[rollout-audit] {event}");
        self.runtime_audit_events.push(event);
        Ok(())
    }

    #[cfg(test)]
    fn runtime_audit_events(&self) -> &[String] {
        &self.runtime_audit_events
    }

    fn validate_public_preference_key(key: &str) -> Result<(), String> {
        if key.starts_with("runtime.") {
            return Err("reserved preference namespace".to_string());
        }
        Ok(())
    }

    pub fn pref_get(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        Self::validate_public_preference_key(key)?;
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT value_json FROM preferences WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("Cannot read Picot preference: {error}"))?;
        match json {
            None => Ok(None),
            Some(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|error| format!("Picot preference {key} holds invalid JSON: {error}")),
        }
    }

    pub fn pref_set(&self, key: &str, value: &serde_json::Value) -> Result<(), String> {
        Self::validate_public_preference_key(key)?;
        let json = serde_json::to_string(value)
            .map_err(|error| format!("Cannot serialize Picot preference {key}: {error}"))?;
        self.connection
            .execute(
                "INSERT INTO preferences (key, value_json) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
                params![key, json],
            )
            .map_err(|error| format!("Cannot write Picot preference: {error}"))?;
        Ok(())
    }

    pub fn pref_delete(&self, key: &str) -> Result<bool, String> {
        Self::validate_public_preference_key(key)?;
        let deleted = self
            .connection
            .execute("DELETE FROM preferences WHERE key = ?1", [key])
            .map_err(|error| format!("Cannot delete Picot preference: {error}"))?;
        Ok(deleted > 0)
    }

    pub fn pref_list(&self, prefix: &str) -> Result<BTreeMap<String, serde_json::Value>, String> {
        if prefix.starts_with("runtime.") {
            return Err("reserved preference namespace".to_string());
        }
        // Escape LIKE metacharacters so caller-supplied prefixes match
        // literally instead of acting as wildcards.
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("{escaped}%");
        let mut statement = self
            .connection
            .prepare("SELECT key, value_json FROM preferences WHERE key LIKE ?1 ESCAPE '\\' ORDER BY key")
            .map_err(|error| format!("Cannot list Picot preferences: {error}"))?;
        let mapped = statement
            .query_map([pattern], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("Cannot list Picot preferences: {error}"))?;
        let mut entries = BTreeMap::new();
        for entry in mapped {
            let (key, json) =
                entry.map_err(|error| format!("Cannot list Picot preferences: {error}"))?;
            if key.starts_with("runtime.") {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(&json)
                .map_err(|error| format!("Picot preference {key} holds invalid JSON: {error}"))?;
            entries.insert(key, value);
        }
        Ok(entries)
    }
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    use super::{
        AddWorkspaceError, MetadataStore, RemovedWorkspace, WorkspaceLookupError, SCHEMA_VERSION,
        WRITTEN_SCHEMA_VERSION,
    };
    use rusqlite::Connection;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        // Parallel tests may sample identical nanosecond timestamps; the
        // monotonic per-call sequence guarantees a unique directory.
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "picot-metadata-{}-{sequence}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn fresh_store(name: &str) -> (tempdir_guard::TempDirGuard, MetadataStore) {
        let temp = temp_dir();
        let store = MetadataStore::open(&temp.join(name)).unwrap();
        (tempdir_guard::TempDirGuard(temp), store)
    }

    #[test]
    fn reset_credit_ledger_opens_reuses_and_settles() {
        let (_guard, store) = fresh_store("rc-ledger.sqlite3");
        let first = store.reset_credit_open().expect("first open");
        assert!(!first.is_empty());
        // A pending row blocks new operations: the same id comes back.
        assert_eq!(store.reset_credit_open().unwrap(), first);
        store
            .reset_credit_settle(&first, "settled")
            .expect("settle");
        // Settled: the next open creates a fresh operation.
        let second = store.reset_credit_open().unwrap();
        assert_ne!(second, first);
    }

    #[test]
    fn reset_credit_ledger_reuses_ambiguous_rows_for_recovery() {
        let (_guard, store) = fresh_store("rc-ambiguous.sqlite3");
        let id = store.reset_credit_open().unwrap();
        store.reset_credit_settle(&id, "ambiguous").unwrap();
        // Recovery reuses the SAME operationId so the upstream idempotency
        // key answers already_redeemed instead of double-spending.
        assert_eq!(store.reset_credit_open().unwrap(), id);
        store.reset_credit_settle(&id, "settled").unwrap();
        assert_ne!(store.reset_credit_open().unwrap(), id);
    }

    #[test]
    fn reset_credit_sweep_abandons_only_stale_pending_rows() {
        let (_guard, store) = fresh_store("rc-sweep.sqlite3");
        let stale = store.reset_credit_open().unwrap();
        store.reset_credit_settle(&stale, "settled").unwrap();
        // Re-open as pending, then backdate it past the sweep horizon.
        let backdated = store.reset_credit_open().unwrap();
        store
            .connection
            .execute(
                "UPDATE reset_credit_operations SET opened_at = ? WHERE id = ?",
                rusqlite::params![
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64
                        - 120,
                    backdated
                ],
            )
            .unwrap();
        let recent = store.reset_credit_open().unwrap();
        assert_eq!(
            recent, backdated,
            "the backdated pending row is reused, not swept"
        );

        let swept = store.sweep_abandoned_reset_credits().unwrap();
        assert_eq!(swept, 1);
        // After the sweep the stale row is abandoned: a new open proceeds.
        let fresh = store.reset_credit_open().unwrap();
        assert_ne!(fresh, backdated);
        // The settled row was never touched.
        let status = store
            .connection
            .query_row(
                "SELECT status FROM reset_credit_operations WHERE id = ?",
                rusqlite::params![stale],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(status, "settled");
    }

    mod tempdir_guard {
        use super::*;
        pub struct TempDirGuard(pub PathBuf);
        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn workspace_columns(store: &MetadataStore) -> Vec<String> {
        table_columns(store, "workspaces")
    }

    fn table_columns(store: &MetadataStore, table: &str) -> Vec<String> {
        let connection: &Connection = &store.connection;
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let mapped = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap();
        mapped.map(|entry| entry.unwrap()).collect()
    }

    fn table_exists(store: &MetadataStore, table: &str) -> bool {
        let connection: &Connection = &store.connection;
        connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |_| Ok(()),
            )
            .is_ok()
    }

    #[test]
    fn fresh_empty_database_reaches_public_schema_v3_without_corp_tables() {
        let (_guard, mut store) = fresh_store("picot.sqlite3");
        assert_eq!(store.schema_version().unwrap(), WRITTEN_SCHEMA_VERSION);
        let columns = workspace_columns(&store);
        for expected in [
            "workspace_id",
            "canonical_path",
            "created_at",
            "display_name",
            "pinned",
            "last_opened_at",
            "session_bucket",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "missing column {expected}"
            );
        }
        assert!(store.list_workspaces_and_prune().unwrap().0.is_empty());
        assert!(!table_exists(&store, "session_sidebar_visibility"));
        // Corp tables belong to Corp builds; a public-created database must
        // never fabricate them or claim a Corp-complete schema version.
        for corp_table in [
            "company_account_profiles",
            "gitlab_bindings",
            "company_install_ledger",
        ] {
            assert!(
                !table_exists(&store, corp_table),
                "public migration must not create {corp_table}"
            );
        }
    }

    #[test]
    fn deprecated_visibility_table_is_dropped_at_open_and_stays_gone() {
        let (_guard, store) = fresh_store("picot.sqlite3");
        assert_eq!(store.schema_version().unwrap(), WRITTEN_SCHEMA_VERSION);
        assert!(!table_exists(&store, "session_sidebar_visibility"));

        // A database left over from a deprecated build still carries the
        // table; re-opening must drop it and never recreate it.
        store
            .connection
            .execute(
                "CREATE TABLE session_sidebar_visibility (
                    session_file TEXT PRIMARY KEY,
                    bucket_name TEXT NOT NULL,
                    modified_at_ms INTEGER NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    is_subagent INTEGER NOT NULL,
                    is_sidebar_visible INTEGER NOT NULL
                )",
                [],
            )
            .unwrap();
        let database = store.path.clone();
        drop(store);

        let reopened = MetadataStore::open(&database).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), WRITTEN_SCHEMA_VERSION);
        assert!(!table_exists(&reopened, "session_sidebar_visibility"));
    }

    #[test]
    fn v2_style_database_keeps_unknown_tables_and_migrates_current() {
        let temp = temp_dir();
        let _guard = tempdir_guard::TempDirGuard(temp.clone());
        let database = temp.join("picot.sqlite3");
        {
            let connection = Connection::open(&database).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE workspaces (
                        workspace_id TEXT PRIMARY KEY,
                        canonical_path TEXT NOT NULL UNIQUE,
                        created_at INTEGER NOT NULL DEFAULT (unixepoch())
                    );
                    CREATE TABLE paired_devices (
                        device_id TEXT PRIMARY KEY,
                        token_hash BLOB NOT NULL UNIQUE,
                        paired_at INTEGER NOT NULL DEFAULT (unixepoch()),
                        revoked_at INTEGER
                    );
                    CREATE TABLE preferences (
                        key TEXT PRIMARY KEY,
                        value_json TEXT NOT NULL
                    );
                    CREATE TABLE legacy_experiments (
                        profile_key TEXT PRIMARY KEY,
                        payload TEXT NOT NULL
                    );
                    INSERT INTO legacy_experiments (profile_key, payload) VALUES ('a', '{}');
                    INSERT INTO workspaces (workspace_id, canonical_path) VALUES ('legacy', '/tmp/legacy');
                    PRAGMA user_version = 2;",
                )
                .unwrap();
        }
        let store = MetadataStore::open(&database).unwrap();
        assert_eq!(store.schema_version().unwrap(), WRITTEN_SCHEMA_VERSION);

        // Verify the v2-era registry row survived migration through a direct
        // count: the placeholder path is not a real directory, so the pruning
        // list API would legitimately remove it.
        let legacy_rows: i64 = {
            let connection: &Connection = &store.connection;
            connection
                .query_row(
                    "SELECT count(*) FROM workspaces WHERE workspace_id = 'legacy'",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(legacy_rows, 1, "v2 row survived migration");

        // Reopen to prove unknown-table data plus v3 columns persist.
        drop(store);
        let reopened = MetadataStore::open(&database).unwrap();
        let profiles: i64 = {
            let connection: &Connection = &reopened.connection;
            connection
                .query_row("SELECT count(*) FROM legacy_experiments", [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert_eq!(profiles, 1, "unknown v2 table survived");
        assert_eq!(workspace_columns(&reopened).len(), 7);
    }

    #[test]
    fn corp_v6_database_opens_unchanged_with_workspace_rows() {
        let temp = temp_dir();
        let _guard = tempdir_guard::TempDirGuard(temp.clone());
        let database = temp.join("picot.sqlite3");
        let project = temp.join("corp-project");
        fs::create_dir_all(&project).unwrap();
        let project_path = project.to_string_lossy().into_owned();
        {
            let connection = Connection::open(&database).unwrap();
            connection
                .execute_batch(&format!(
                    "CREATE TABLE workspaces (
                    workspace_id TEXT PRIMARY KEY,
                    canonical_path TEXT NOT NULL UNIQUE,
                    created_at INTEGER NOT NULL DEFAULT (unixepoch())
                , display_name TEXT, pinned INTEGER NOT NULL DEFAULT 0, last_opened_at INTEGER);
                CREATE TABLE paired_devices (
                    device_id TEXT PRIMARY KEY,
                    token_hash BLOB NOT NULL UNIQUE,
                    paired_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    revoked_at INTEGER
                );
                CREATE TABLE preferences (
                    key TEXT PRIMARY KEY,
                    value_json TEXT NOT NULL
                );
                CREATE TABLE company_account_profiles (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    sub TEXT NOT NULL,
                    preferred_username TEXT,
                    name TEXT,
                    email TEXT,
                    picture TEXT,
                    last_validated_at INTEGER
                );
                CREATE TABLE gitlab_bindings (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    gitlab_user_id INTEGER NOT NULL,
                    gitlab_username TEXT NOT NULL,
                    company_account_sub TEXT NOT NULL,
                    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                , gitlab_name TEXT, gitlab_avatar_url TEXT);
                CREATE TABLE company_install_ledger (
                    id INTEGER PRIMARY KEY,
                    source TEXT NOT NULL,
                    scope TEXT NOT NULL CHECK (scope IN ('global', 'project')),
                    workspace_id TEXT,
                    catalog TEXT CHECK (catalog = 'datarx-enterprise'),
                    source_kind TEXT NOT NULL CHECK (source_kind IN ('catalog', 'company-git')),
                    installed_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    last_seen_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    last_attempt_at INTEGER,
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    last_error_code TEXT,
                    UNIQUE(source, scope, workspace_id),
                    CHECK (
                        (scope = 'global' AND workspace_id IS NULL)
                        OR
                        (scope = 'project' AND workspace_id IS NOT NULL)
                    ),
                    CHECK (
                        (source_kind = 'catalog' AND catalog IS NOT NULL)
                        OR
                        (source_kind = 'company-git' AND catalog IS NULL)
                    )
                );
                INSERT INTO company_account_profiles
                    (singleton, sub, preferred_username) VALUES (1, 'corp-sub', 'lin');
                INSERT INTO workspaces (workspace_id, canonical_path, display_name)
                    VALUES ('corp-ws', '{project_path}', 'Corp');
                PRAGMA user_version = 6;",
                ))
                .unwrap();
        }
        let mut store = MetadataStore::open(&database).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let rows = store.list_workspaces_and_prune().unwrap().0;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].workspace_id, "corp-ws");
        let corp_profile: i64 = {
            let connection: &Connection = &store.connection;
            connection
                .query_row("SELECT count(*) FROM company_account_profiles", [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert_eq!(corp_profile, 1, "corp data untouched");
    }

    #[test]
    fn corp_v4_partial_database_opens_without_creating_corp_tables() {
        let temp = temp_dir();
        let _guard = tempdir_guard::TempDirGuard(temp.clone());
        let database = temp.join("picot.sqlite3");
        let project = temp.join("corp-partial");
        fs::create_dir_all(&project).unwrap();
        {
            let connection = Connection::open(&database).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE workspaces (
                        workspace_id TEXT PRIMARY KEY,
                        canonical_path TEXT NOT NULL UNIQUE,
                        created_at INTEGER NOT NULL DEFAULT (unixepoch())
                    , display_name TEXT, pinned INTEGER NOT NULL DEFAULT 0, last_opened_at INTEGER);
                    CREATE TABLE paired_devices (
                        device_id TEXT PRIMARY KEY,
                        token_hash BLOB NOT NULL UNIQUE,
                        paired_at INTEGER NOT NULL DEFAULT (unixepoch()),
                        revoked_at INTEGER
                    );
                    CREATE TABLE preferences (
                        key TEXT PRIMARY KEY,
                        value_json TEXT NOT NULL
                    );
                    CREATE TABLE company_account_profiles (
                        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                        sub TEXT NOT NULL,
                        preferred_username TEXT,
                        name TEXT,
                        email TEXT,
                        picture TEXT,
                        last_validated_at INTEGER
                    );
                    PRAGMA user_version = 4;",
                )
                .unwrap();
        }
        let store = MetadataStore::open(&database).unwrap();
        // A Corp-partial database keeps its version untouched: the Corp
        // staircase owns v4–v6 completion, and this build must neither stamp
        // a Corp-complete version nor fabricate Corp tables it does not use.
        assert_eq!(store.schema_version().unwrap(), 4);
        assert!(!table_exists(&store, "session_sidebar_visibility"));
        assert!(table_exists(&store, "company_account_profiles"));
        assert!(
            !table_exists(&store, "gitlab_bindings"),
            "public open must not create Corp tables"
        );
        assert!(!table_exists(&store, "company_install_ledger"));
        // The v1–v3 surface stays fully usable.
        let (_row, added) = store.add_workspace(&project).unwrap();
        assert!(added);
        assert_eq!(store.schema_version().unwrap(), 4);
        drop(store);
        let reopened = MetadataStore::open(&database).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), 4);
    }

    #[test]
    fn reopening_migrated_database_is_idempotent() {
        let temp = temp_dir();
        let _guard = tempdir_guard::TempDirGuard(temp.clone());
        let database = temp.join("picot.sqlite3");

        {
            let store = MetadataStore::open(&database).unwrap();
            assert_eq!(store.schema_version().unwrap(), WRITTEN_SCHEMA_VERSION);
        }
        assert!(database.exists());

        // Reopen the exact same database; migration must be a no-op.
        let mut reopened = MetadataStore::open(&database).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), WRITTEN_SCHEMA_VERSION);
        assert!(reopened.list_workspaces_and_prune().unwrap().0.is_empty());
    }

    #[test]
    fn newer_schema_than_supported_rejects_open() {
        let temp = temp_dir();
        let _guard = tempdir_guard::TempDirGuard(temp.clone());
        let database = temp.join("picot.sqlite3");
        {
            let connection = Connection::open(&database).unwrap();
            connection
                .execute_batch("PRAGMA user_version = 99;")
                .unwrap();
        }
        let error = match MetadataStore::open(&database) {
            Err(error) => error,
            Ok(_) => panic!("expected open to reject newer schema"),
        };
        assert!(error.contains("newer than supported"), "{error}");
    }

    #[test]
    fn corrupt_database_is_quarantined_and_recreated() {
        let temp = temp_dir();
        let _guard = tempdir_guard::TempDirGuard(temp.clone());
        let database = temp.join("picot.sqlite3");
        fs::write(&database, b"this is definitely not a sqlite database").unwrap();

        // R4.11 graceful degradation: a corrupt database is quarantined
        // (renamed, bytes retained) and a fresh store is created, so the app
        // keeps starting without user intervention.
        let store = MetadataStore::open(&database).unwrap();
        assert_eq!(store.schema_version().unwrap(), WRITTEN_SCHEMA_VERSION);

        let quarantined = quarantine_files(&temp);
        assert_eq!(quarantined.len(), 1, "exactly one quarantine file");
        assert_eq!(
            fs::read(temp.join(&quarantined[0])).unwrap(),
            b"this is definitely not a sqlite database",
            "quarantine retains the original bytes"
        );

        // The recreated store is fully functional.
        let project = temp.join("project");
        fs::create_dir_all(&project).unwrap();
        let (row, added) = store.add_workspace(&project).unwrap();
        assert!(added);
        assert_eq!(row.display_name.as_deref(), Some("project"));
    }

    #[test]
    fn fresh_and_valid_reopens_create_no_quarantine_files() {
        let temp = temp_dir();
        let _guard = tempdir_guard::TempDirGuard(temp.clone());
        let database = temp.join("picot.sqlite3");
        {
            let store = MetadataStore::open(&database).unwrap();
            assert_eq!(store.schema_version().unwrap(), WRITTEN_SCHEMA_VERSION);
        }

        // Reopen a healthy database with data; no quarantine may appear.
        let reopened = MetadataStore::open(&database).unwrap();
        let project = temp.join("project");
        fs::create_dir_all(&project).unwrap();
        let (row, added) = reopened.add_workspace(&project).unwrap();
        assert!(added);
        assert_eq!(row.display_name.as_deref(), Some("project"));
        assert!(database.exists());
        assert!(
            quarantine_files(&temp).is_empty(),
            "unexpected quarantine files"
        );
    }

    fn quarantine_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".corrupt-"))
            .collect()
    }

    #[test]
    fn add_workspace_is_canonical_idempotent_and_validated() {
        let (_guard, store) = fresh_store("picot.sqlite3");
        let dir = _guard.0.join("project");
        fs::create_dir_all(&dir).unwrap();

        let (row, added) = store.add_workspace(&dir).unwrap();
        assert!(added);
        assert_eq!(row.display_name.as_deref(), Some("project"));
        assert!(!row.pinned);
        assert!(row.last_opened_at.is_none());

        // Same logical directory via a different spelling stays one row.
        fs::create_dir_all(_guard.0.join("sub")).unwrap();
        let detoured = _guard.0.join("sub").join("..").join("project");
        let (_same_row, same_added) = store.add_workspace(&detoured).unwrap();
        assert!(
            !same_added,
            "detoured spelling must hit UNIQUE canonical_path"
        );

        let (again, added_again) = store.add_workspace(&dir).unwrap();
        assert!(!added_again);
        assert_eq!(again.workspace_id, row.workspace_id);

        let missing = store.add_workspace(&_guard.0.join("missing"));
        assert!(matches!(missing, Err(AddWorkspaceError::PathNotFound(_))));
        assert_eq!(missing.unwrap_err().code(), "path_not_found");

        let file = _guard.0.join("plain.txt");
        fs::write(&file, "x").unwrap();
        let not_dir = store.add_workspace(&file);
        assert!(matches!(not_dir, Err(AddWorkspaceError::NotADirectory(_))));
        assert_eq!(not_dir.unwrap_err().code(), "not_a_directory");
    }

    #[test]
    fn list_prune_reports_missing_directories_but_never_touches_files() {
        let (_guard, mut store) = fresh_store("picot.sqlite3");
        let alive = _guard.0.join("alive");
        let dying = _guard.0.join("dying");
        fs::create_dir_all(&alive).unwrap();
        fs::create_dir_all(&dying).unwrap();
        let session_note = dying.join("session.jsonl");
        fs::write(&session_note, "{\"type\":\"session\"}\n").unwrap();

        let (_, _) = store.add_workspace(&alive).unwrap();
        let (dying_row, _) = store.add_workspace(&dying).unwrap();
        fs::remove_dir_all(&dying).unwrap();

        let expected_alive = fs::canonicalize(&alive)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let (rows, removed): (Vec<_>, Vec<RemovedWorkspace>) =
            store.list_workspaces_and_prune().unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].workspace_id, dying_row.workspace_id);
        assert_eq!(removed[0].canonical_path, dying_row.canonical_path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].canonical_path, expected_alive);
        assert!(!session_note.exists(), "prune must not touch filesystem");

        // Second listing is clean and stable.
        let (rows2, removed2) = store.list_workspaces_and_prune().unwrap();
        assert!(removed2.is_empty());
        assert_eq!(rows2.len(), 1);
    }

    #[test]
    fn ordering_puts_pinned_then_recently_opened_first() {
        let (_guard, mut store) = fresh_store("picot.sqlite3");
        let mut ids = Vec::new();
        for name in ["alpha", "beta", "gamma"] {
            let dir = _guard.0.join(name);
            fs::create_dir_all(&dir).unwrap();
            ids.push(store.add_workspace(&dir).unwrap().0);
        }
        store
            .set_workspace_pinned(&ids[2].workspace_id, true)
            .unwrap();
        store
            .touch_registered_path(Path::new(&ids[1].canonical_path))
            .unwrap();

        let rows = store.list_workspaces_and_prune().unwrap().0;
        assert_eq!(
            rows[0].canonical_path, ids[2].canonical_path,
            "pinned first"
        );
        assert_eq!(
            rows[1].canonical_path, ids[1].canonical_path,
            "touched beats untitled"
        );
        assert_eq!(rows[2].last_opened_at, None);
    }

    #[test]
    fn pin_touch_and_remove_behave_as_documented() {
        let (_guard, mut store) = fresh_store("picot.sqlite3");
        let dir = _guard.0.join("main");
        fs::create_dir_all(&dir).unwrap();
        let row = store.add_workspace(&dir).unwrap().0;

        let pinned = store
            .set_workspace_pinned(&row.workspace_id, true)
            .unwrap()
            .unwrap();
        assert!(pinned.pinned);

        // Unregistered path must neither touch nor create a registry row.
        let total_before: i64 = {
            let connection: &Connection = &store.connection;
            connection
                .query_row("SELECT count(*) FROM workspaces", [], |r| r.get(0))
                .unwrap()
        };
        let unregistered = store
            .touch_registered_path(&_guard.0.join("ghost"))
            .unwrap();
        assert!(!unregistered, "unregistered path must not touch or create");
        let total_after: i64 = {
            let connection: &Connection = &store.connection;
            connection
                .query_row("SELECT count(*) FROM workspaces", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(total_before, total_after);

        let touched = store
            .touch_registered_path(Path::new(&row.canonical_path))
            .unwrap();
        assert!(touched);
        let updated = store.get_workspace(&row.workspace_id).unwrap().unwrap();
        assert!(updated.last_opened_at.is_some());

        assert!(store.remove_workspace(&row.workspace_id).unwrap());
        assert!(!store.remove_workspace(&row.workspace_id).unwrap());
        assert!(store.get_workspace(&row.workspace_id).unwrap().is_none());

        let unknown_pin = store.set_workspace_pinned("no-such-id", false).unwrap();
        assert!(unknown_pin.is_none());
    }

    #[test]
    fn preferences_round_trip_upsert_delete_and_prefix_list() {
        let (_guard, store) = fresh_store("picot.sqlite3");

        assert_eq!(store.pref_get("ui.theme").unwrap(), None);

        store
            .pref_set("ui.theme", &serde_json::json!("dark"))
            .unwrap();
        store
            .pref_set("ui.locale", &serde_json::json!("zh"))
            .unwrap();
        store
            .pref_set("sidebar.sizes.lane", &serde_json::json!({ "px": 320 }))
            .unwrap();

        assert_eq!(
            store.pref_get("ui.theme").unwrap(),
            Some(serde_json::json!("dark"))
        );
        // Upsert overwrites.
        store
            .pref_set("ui.theme", &serde_json::json!("light"))
            .unwrap();
        assert_eq!(
            store.pref_get("ui.theme").unwrap(),
            Some(serde_json::json!("light"))
        );

        let ui_prefs = store.pref_list("ui.").unwrap();
        assert_eq!(ui_prefs.len(), 2);
        assert_eq!(ui_prefs["ui.locale"], serde_json::json!("zh"));

        assert!(store.pref_delete("ui.theme").unwrap());
        assert!(!store.pref_delete("ui.theme").unwrap());
        assert_eq!(store.pref_list("").unwrap().len(), 2, "prefix '' lists all");

        // Nested JSON survives storage.
        assert_eq!(
            store.pref_list("sidebar.").unwrap()["sidebar.sizes.lane"],
            serde_json::json!({ "px": 320 })
        );

        // LIKE metacharacters never act as wildcards. `_`/`%` in a prefix are
        // taken literally: the pairs below would MATCH under wildcard
        // semantics (`_`→any char, `%`→any run) but must stay empty now.
        store.pref_set("weird.x-b", &serde_json::json!(1)).unwrap();
        store.pref_set("weird.pxg", &serde_json::json!(2)).unwrap();
        assert!(store.pref_list("weird.x_b").unwrap().is_empty());
        assert!(store.pref_list("weird.p%g").unwrap().is_empty());
        // Literal self-match keeps working for meta-free prefixes.
        assert_eq!(
            store.pref_list("weird.x-b").unwrap()["weird.x-b"],
            serde_json::json!(1)
        );
    }

    #[test]
    fn runtime_preference_requires_rollout_authorization() {
        let (_guard, mut store) = fresh_store("picot.sqlite3");
        assert_eq!(store.runtime_pref_get(None), None);
        assert_eq!(
            store.runtime_pref_set(None, true).unwrap_err(),
            "rollout authorization required"
        );

        let authorization = super::rollout_authorization();
        store.runtime_pref_set(Some(&authorization), false).unwrap();
        assert_eq!(store.runtime_pref_get(Some(&authorization)), Some(false));
        store.runtime_pref_set(Some(&authorization), true).unwrap();
        assert_eq!(store.runtime_pref_get(Some(&authorization)), Some(true));
        assert_eq!(
            store.pref_get("runtime.native_origin").unwrap_err(),
            "reserved preference namespace"
        );
        assert_eq!(
            store
                .pref_set("runtime.native_origin", &serde_json::json!(false))
                .unwrap_err(),
            "reserved preference namespace"
        );
        assert_eq!(
            store.pref_delete("runtime.native_origin").unwrap_err(),
            "reserved preference namespace"
        );
        assert_eq!(
            store.pref_list("runtime.").unwrap_err(),
            "reserved preference namespace"
        );
        assert!(store.pref_list("").unwrap().is_empty());
    }

    #[test]
    fn runtime_preference_reader_fails_closed_for_invalid_json_and_value() {
        let (_guard, store) = fresh_store("picot.sqlite3");
        let authorization = super::rollout_authorization();
        store
            .connection
            .execute(
                "INSERT INTO preferences (key, value_json) VALUES ('runtime.native_origin', 'not-json')",
                [],
            )
            .unwrap();
        assert_eq!(store.runtime_pref_get(Some(&authorization)), None);

        store
            .connection
            .execute(
                "UPDATE preferences SET value_json = '42' WHERE key = 'runtime.native_origin'",
                [],
            )
            .unwrap();
        assert_eq!(store.runtime_pref_get(Some(&authorization)), None);
    }

    #[test]
    fn runtime_preference_write_audit_is_redacted() {
        let (_guard, mut store) = fresh_store("picot.sqlite3");
        let authorization = super::rollout_authorization();
        store.runtime_pref_set(Some(&authorization), true).unwrap();
        let events = store.runtime_audit_events();
        assert_eq!(events, &["runtime_preference_write status=success"]);
        assert!(events
            .iter()
            .all(|event| { !event.contains("runtime.native_origin") && !event.contains("true") }));
    }

    #[test]
    fn readonly_workspace_lookups_are_registry_authority_without_writes() {
        let (_guard, mut store) = fresh_store("picot.sqlite3");
        let workspace = _guard.0.join("workspace");
        let missing = _guard.0.join("missing");
        fs::create_dir(&workspace).unwrap();

        let count = |store: &MetadataStore| -> i64 {
            store
                .connection
                .query_row("SELECT count(*) FROM workspaces", [], |row| row.get(0))
                .unwrap()
        };
        let before = count(&store);
        let error = store.workspace_id_for_canonical_root(&missing).unwrap_err();
        assert_eq!(error, WorkspaceLookupError::NotRegistered);
        assert_eq!(error.code(), "not_registered");
        assert_eq!(
            count(&store),
            before,
            "read-only lookup must not register path"
        );

        let row = store.add_workspace(&workspace).unwrap().0;
        let canonical = Path::new(&row.canonical_path);
        assert_eq!(
            store.workspace_id_for_canonical_root(canonical).unwrap(),
            row.workspace_id
        );
        assert_eq!(
            store
                .canonical_root_for_workspace_id(&row.workspace_id)
                .unwrap(),
            PathBuf::from(&row.canonical_path)
        );
        assert_eq!(
            store.workspace_id_for_canonical_root(canonical).unwrap(),
            store.workspace_id_for_path(&workspace).unwrap()
        );

        assert!(store.remove_workspace(&row.workspace_id).unwrap());
        assert_eq!(
            store
                .canonical_root_for_workspace_id(&row.workspace_id)
                .unwrap_err(),
            WorkspaceLookupError::NotRegistered
        );
        assert_eq!(
            store
                .workspace_id_for_canonical_root(canonical)
                .unwrap_err(),
            WorkspaceLookupError::NotRegistered
        );
    }

    #[test]
    fn has_registered_workspaces_is_read_only() {
        let (_guard, store) = fresh_store("picot.sqlite3");
        assert!(!store.has_registered_workspaces().unwrap());
        let workspace = _guard.0.join("workspace");
        fs::create_dir(&workspace).unwrap();
        store.add_workspace(&workspace).unwrap();
        assert!(store.has_registered_workspaces().unwrap());
    }

    #[test]
    fn assigns_stable_workspace_ids_and_stores_only_device_token_hashes() {
        let temp = temp_dir();
        let _guard = tempdir_guard::TempDirGuard(temp.clone());
        let database = temp.join("picot.sqlite3");
        let workspace = temp.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut store = MetadataStore::open(&database).unwrap();

        let first = store.workspace_id_for_path(&workspace).unwrap();
        let second = store.workspace_id_for_path(&workspace).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.schema_version().unwrap(), WRITTEN_SCHEMA_VERSION);

        store
            .store_device_token("phone", "plain-device-token")
            .unwrap();
        assert!(store.verify_device_token("plain-device-token").unwrap());
        assert!(!store.verify_device_token("wrong-token").unwrap());
        let bytes = fs::read(&database).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("plain-device-token"));
        store.revoke_device("phone").unwrap();
        assert!(!store.verify_device_token("plain-device-token").unwrap());
    }

    #[test]
    fn reset_cannot_modify_pi_sessions_or_workspace_files_or_unknown_tables() {
        let temp = temp_dir();
        let _guard = tempdir_guard::TempDirGuard(temp.clone());
        let database = temp.join("picot.sqlite3");
        let workspace = temp.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let session = workspace.join("session.jsonl");
        fs::write(&session, "{\"type\":\"session\"}\n").unwrap();
        let mut store = MetadataStore::open(&database).unwrap();
        store.workspace_id_for_path(&workspace).unwrap();
        store
            .pref_set("ui.theme", &serde_json::json!("dark"))
            .unwrap();
        {
            let connection: &Connection = &store.connection;
            connection
                .execute_batch("CREATE TABLE foreign_branch_table (id INTEGER PRIMARY KEY); INSERT INTO foreign_branch_table VALUES (7);")
                .unwrap();
        }

        store.reset().unwrap();

        assert_eq!(
            fs::read_to_string(session).unwrap(),
            "{\"type\":\"session\"}\n"
        );
        assert!(store.list_workspaces_and_prune().unwrap().0.is_empty());
        let foreign: i64 = {
            let connection: &Connection = &store.connection;
            connection
                .query_row("SELECT count(*) FROM foreign_branch_table", [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert_eq!(foreign, 1, "reset must not touch unknown tables");
    }
}
