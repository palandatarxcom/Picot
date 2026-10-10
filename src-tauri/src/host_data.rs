// ABOUTME: Host data-plane reads for registry-authorized workspaces.
// ABOUTME: Resolves workspace roots through shared MetadataStore authority.
use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMentionCandidate {
    pub value: String,
    pub label: String,
    pub description: String,
    pub is_directory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMentionSearchResult {
    pub items: Vec<FileMentionCandidate>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub relative_path: String,
    pub kind: FileKind,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub timestamp: String,
    pub name: Option<String>,
    pub first_message: Option<String>,
    pub workspace_id: String,
    pub file_name: String,
    pub modified_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeSnapshot {
    pub entries: Vec<serde_json::Value>,
    pub leaf_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchMatch {
    pub role: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchResult {
    pub session_id: String,
    pub session_name: Option<String>,
    pub session_timestamp: String,
    pub first_message: Option<String>,
    pub file_name: String,
    pub matches: Vec<SessionSearchMatch>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SessionMetrics {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) cwd_canonical: Option<PathBuf>,
    pub(crate) model: String,
    pub(crate) timestamp: String,
    pub(crate) last_active: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) total_cost: f64,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cache_read: u64,
    pub(crate) cache_write: u64,
    pub(crate) assistant_messages: u64,
    pub(crate) tool_calls: u64,
    pub(crate) user_messages: u64,
    pub(crate) tool_cost_by_name: HashMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDataError {
    UnknownWorkspace,
    InvalidRelativePath,
    OutsideWorkspace,
    NotDirectory,
    /// Upstream mention-query guard: bad token shape (no `@` prefix, NUL,
    /// mid-word traversal, Windows bare `@/`, root declaration mismatch).
    InvalidMentionQuery,
    /// Wide-root search root does not exist or is unreachable (missing drive,
    /// UNC timeout) — contract E distinguishes this from "no matches".
    MentionRootUnavailable(String),
    /// The workspace root, or the requested directory under it, was reachable
    /// before and is not now: unmounted volume, offline share, stale network
    /// handle. Never a deletion signal — the WebView keeps its cache.
    TemporarilyUnavailable,
    Io(String),
}

#[derive(Clone)]
pub struct HostDataPlane {
    metadata: crate::metadata_store::SharedMetadataStore,
    session_root: Option<PathBuf>,
    /// Parsed-metrics cache for the cost dashboard scan; shared with the
    /// startup prewarm thread, so the first Usage open is already warm.
    cost_cache: std::sync::Arc<std::sync::Mutex<crate::cost_compat::SessionMetricsCache>>,
}

impl HostDataPlane {
    /// Codex reset-credit ledger passthrough (spec 2026-09-22): the ledger
    /// lives in the shared metadata store; callers get the raw String error.
    pub fn reset_credit_open(&self) -> Result<String, String> {
        self.metadata
            .lock()
            .map_err(|_| "Picot metadata lock poisoned".to_owned())?
            .reset_credit_open()
    }

    pub fn reset_credit_settle(&self, operation_id: &str, status: &str) -> Result<(), String> {
        self.metadata
            .lock()
            .map_err(|_| "Picot metadata lock poisoned".to_owned())?
            .reset_credit_settle(operation_id, status)
    }

    pub fn new(metadata: crate::metadata_store::SharedMetadataStore) -> Self {
        Self::with_metadata(metadata)
    }

    /// A boolean user preference, or `None` when unset. One accessor so every
    /// caller reads the same DB-backed value the Settings toggles write.
    pub fn preference_bool(&self, key: &str) -> Option<bool> {
        self.metadata
            .lock()
            .ok()
            .and_then(|store| store.pref_get(key).ok())
            .flatten()
            .and_then(|value| value.as_bool())
    }

    fn with_metadata(metadata: crate::metadata_store::SharedMetadataStore) -> Self {
        Self {
            metadata,
            session_root: None,
            cost_cache: std::sync::Arc::new(std::sync::Mutex::new(
                crate::cost_compat::SessionMetricsCache::default(),
            )),
        }
    }

    pub fn workspace_root(&self, workspace_id: &str) -> Result<PathBuf, HostDataError> {
        self.metadata
            .lock()
            .map_err(|_| HostDataError::UnknownWorkspace)?
            .canonical_root_for_workspace_id(workspace_id)
            .map_err(|_| HostDataError::UnknownWorkspace)
    }

    pub fn with_session_root(mut self, session_root: PathBuf) -> Self {
        self.session_root = Some(session_root);
        self
    }

    /// Shared handle for the startup prewarm thread.
    pub fn cost_metrics_cache(
        &self,
    ) -> std::sync::Arc<std::sync::Mutex<crate::cost_compat::SessionMetricsCache>> {
        std::sync::Arc::clone(&self.cost_cache)
    }

    /// Persist the bucket directory reported by Pi's `sessionFile` RPC field.
    /// The host never derives a bucket from workspace paths or scans sessions to
    /// discover one.
    pub fn record_pi_session_bucket(
        &self,
        workspace_id: &str,
        session_file: &str,
    ) -> Result<bool, String> {
        let bucket = Path::new(session_file)
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or_else(|| "Pi returned an invalid sessionFile path".to_string())?;
        let mut metadata = self
            .metadata
            .lock()
            .map_err(|_| "Picot metadata lock poisoned".to_string())?;
        metadata.set_workspace_session_bucket_from_pi(workspace_id, bucket)
    }

    /// Resolve the single persisted bucket for a workspace. The registry owns
    /// this identity; a missing or invalid value has no fallback scan.
    pub fn session_bucket_for_workspace(&self, workspace_id: &str) -> Option<PathBuf> {
        let session_root = self.session_root.as_ref()?;
        let row = self
            .metadata
            .lock()
            .ok()?
            .get_workspace(workspace_id)
            .ok()??;
        let bucket = row.session_bucket?;
        crate::metadata_store::MetadataStore::is_valid_session_bucket_name(&bucket)
            .then(|| session_root.join(bucket))
    }

    /// Read one persisted workspace bucket once. Count-only only enumerates
    /// JSONL directory entries and never opens their content.
    pub fn read_workspace_session_bucket(
        &self,
        workspace_id: &str,
        count_only: bool,
    ) -> WorkspaceSessionBucketResult {
        let Some(bucket) = self.session_bucket_for_workspace(workspace_id) else {
            return (None, Vec::new(), Some(0), Some(0));
        };
        let workspace = match self.workspace_root(workspace_id) {
            Ok(workspace) => workspace,
            Err(_) => return (None, Vec::new(), None, None),
        };
        let bucket_name = bucket
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        if !bucket.exists() {
            return (bucket_name, Vec::new(), Some(0), Some(0));
        }
        let files = match std::fs::read_dir(&bucket) {
            Ok(files) => files.filter_map(Result::ok).collect::<Vec<_>>(),
            Err(_) => return (bucket_name, Vec::new(), None, None),
        };
        let jsonl_files = files
            .into_iter()
            .map(|file| file.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
            .collect::<Vec<_>>();
        if count_only {
            // Count mode is deliberately independent of the classification cache:
            // cold-start badges must be exact without opening any JSONL content.
            return (bucket_name, Vec::new(), Some(jsonl_files.len()), None);
        }
        if jsonl_files.is_empty() {
            return (bucket_name, Vec::new(), Some(0), Some(0));
        }

        let workers = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
            .clamp(1, 10)
            .min(jsonl_files.len());
        let chunk_size = jsonl_files.len().div_ceil(workers).max(1);
        let classification_chunks = std::thread::scope(|scope| {
            let handles = jsonl_files
                .chunks(chunk_size)
                .map(|chunk| {
                    scope.spawn(|| {
                        chunk
                            .iter()
                            .map(|path| {
                                (path.clone(), self.classify_sidebar_file(path, &workspace))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join())
                .collect::<Result<Vec<_>, _>>()
        });

        let mut sessions = Vec::new();
        let mut visible_count = 0;
        let hidden_count = 0;
        let mut all_resolved = true;
        let Ok(classification_chunks) = classification_chunks else {
            return (bucket_name, sessions, None, None);
        };
        for chunk in classification_chunks {
            for (path, classification) in chunk {
                let Some(classification) = classification else {
                    all_resolved = false;
                    continue;
                };
                if !classification.resolved {
                    all_resolved = false;
                    continue;
                }
                if !classification.is_sidebar_visible {
                    continue;
                }
                let Some(header) = classification.header else {
                    all_resolved = false;
                    continue;
                };
                visible_count += 1;
                sessions.push(session_summary_value(&path, &header));
            }
        }
        sessions.sort_by(|left, right| {
            session_activity_time(right).total_cmp(&session_activity_time(left))
        });
        if all_resolved {
            (
                bucket_name,
                sessions,
                Some(visible_count),
                Some(hidden_count),
            )
        } else {
            (bucket_name, sessions, None, None)
        }
    }

    fn classify_sidebar_file(
        &self,
        path: &Path,
        workspace: &Path,
    ) -> Option<SidebarFileClassification> {
        let before = sidebar_file_metadata(path)?;
        let path = cache_path(path);

        let scan = scan_session_sidebar_visibility(&path).ok()?;
        let after = sidebar_file_metadata(&path)?;
        let header = scan.header;
        let is_sidebar_visible = header
            .as_ref()
            .is_some_and(|header| session_header_matches_workspace(header, workspace));
        if !sidebar_file_metadata_matches(before, after) {
            return Some(SidebarFileClassification {
                is_sidebar_visible,
                resolved: false,
                header: is_sidebar_visible.then_some(header).flatten(),
            });
        }
        Some(SidebarFileClassification {
            is_sidebar_visible,
            resolved: true,
            header: is_sidebar_visible.then_some(header).flatten(),
        })
    }

    /// Session file path for a workspace session in Pi's persisted bucket.
    pub fn session_file_path(&self, workspace_id: &str, session_id: &str) -> Option<PathBuf> {
        let bucket = self.session_bucket_for_workspace(workspace_id)?;
        self.session_file_path_in_bucket(&bucket, session_id)
    }

    /// Active-branch transcript messages for the main chat (upstream
    /// data-plane contract). Reads the session JSONL line-by-line, walks from
    /// the last message entry (the same tip rule as `read_session_tree`) back
    /// through parentId links, and returns the chain's user/assistant
    /// messages with `entryId` attached — the stable anchor the frontend
    /// stamps on transcript rows.
    ///
    /// `session_path` is an optional caller-supplied hint (the sidebar's
    /// scanned `filePath`). It stays untrusted: it is used only after
    /// `session_file_path_by_path` canonicalizes it inside this workspace's
    /// bucket and matches its header id to `session_id`; any stale or invalid
    /// hint falls back to the ordinary id lookup. Authorization is unchanged.
    pub fn read_session_messages(
        &self,
        workspace_id: &str,
        session_id: &str,
        session_path: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, HostDataError> {
        let path = self
            .resolve_session_file(workspace_id, session_id, session_path)
            .ok_or_else(|| HostDataError::Io(format!("session {session_id} not found")))?;
        let file = std::fs::File::open(&path).map_err(|e| HostDataError::Io(e.to_string()))?;
        let lines = parse_session_lines(file);
        let Some((chain, _)) = active_chain(&lines) else {
            return Ok(vec![]);
        };
        Ok(chain
            .iter()
            .filter(|&&i| lines[i].is_message)
            .filter_map(|&i| {
                lines[i]
                    .entry
                    .get("message")
                    .cloned()
                    .map(|m| message_with_entry_id(m, &lines[i].id))
            })
            .collect())
    }

    /// Flat session-tree snapshot for the Info panel (upstream data-plane
    /// contract). Reads the session JSONL line-by-line — each line is a
    /// shallow entry — so a deeply-branched session never crosses the
    /// runtime bridge's nested-JSON parser. The leaf follows the same tip
    /// rule as the transcript: the last message entry in the file heads the
    /// active branch.
    pub fn read_session_tree(
        &self,
        workspace_id: &str,
        session_id: &str,
    ) -> Result<SessionTreeSnapshot, HostDataError> {
        let path = self
            .session_file_path(workspace_id, session_id)
            .ok_or_else(|| HostDataError::Io(format!("session {session_id} not found")))?;
        let file = std::fs::File::open(&path).map_err(|e| HostDataError::Io(e.to_string()))?;
        let lines = parse_session_lines(file);
        let entries: Vec<serde_json::Value> = lines.iter().map(|l| l.entry.clone()).collect();
        // The leaf follows the same tip rule as the transcript; a cyclic
        // parent chain leaves the leaf unset (entries still return).
        let leaf_id = active_chain(&lines).and_then(|(chain, intact)| {
            intact
                .then(|| chain.last().map(|&tip| lines[tip].id.clone()))
                .flatten()
        });
        Ok(SessionTreeSnapshot { entries, leaf_id })
    }

    /// Resolve a sidebar-scanned JSONL path. The path stays untrusted: it must
    /// canonicalize inside this workspace's persisted bucket and its session
    /// header must match the separately supplied stable ID.
    pub fn session_file_path_by_path(
        &self,
        workspace_id: &str,
        session_id: &str,
        session_path: &str,
    ) -> Option<PathBuf> {
        if session_id.is_empty() || session_path.is_empty() {
            return None;
        }
        let bucket = self.session_bucket_for_workspace(workspace_id)?;
        let session_root = self.session_root.as_ref()?;
        let canonical_root = session_root.canonicalize().ok()?;
        let canonical_bucket = bucket.canonicalize().ok()?;
        if canonical_bucket.strip_prefix(&canonical_root).is_err() {
            return None;
        }
        let resolved = Path::new(session_path).canonicalize().ok()?;
        if resolved.strip_prefix(&canonical_bucket).is_err()
            || !resolved.is_file()
            || resolved
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("jsonl")
        {
            return None;
        }
        let header = parse_session_header(&resolved)?;
        (header.id == session_id).then_some(resolved)
    }

    /// Resolve a session file, preferring a validated caller-supplied hint
    /// (the sidebar's scanned `filePath`) over a bucket rescan. `session_path`
    /// is never trusted on its own: it must pass `session_file_path_by_path`
    /// (canonical containment under the workspace's bucket + matching header
    /// id). A missing, stale, or invalid hint falls through to the ordinary
    /// id lookup, so correctness and authorization are unchanged.
    pub fn resolve_session_file(
        &self,
        workspace_id: &str,
        session_id: &str,
        session_path: Option<&str>,
    ) -> Option<PathBuf> {
        session_path
            .and_then(|hint| self.session_file_path_by_path(workspace_id, session_id, hint))
            .or_else(|| self.session_file_path(workspace_id, session_id))
    }

    /// Canonicalize a Pi-reported `sessionFile` path for retention on a running
    /// runtime. The input is untrusted: it must resolve inside the configured
    /// session root and be a regular `.jsonl`. The header id is not required —
    /// a temporary runtime's host id never matches its persisted header, and
    /// the retained file only feeds display summaries, never authorization.
    pub fn canonical_session_file(&self, session_path: &str) -> Option<PathBuf> {
        let session_root = self.session_root.as_ref()?;
        let canonical_root = session_root.canonicalize().ok()?;
        let resolved = Path::new(session_path).canonicalize().ok()?;
        (resolved.strip_prefix(&canonical_root).is_ok()
            && resolved.is_file()
            && resolved
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("jsonl"))
        .then_some(resolved)
    }

    fn session_file_path_in_bucket(&self, bucket: &Path, session_id: &str) -> Option<PathBuf> {
        if session_id.is_empty()
            || session_id.contains('/')
            || session_id.contains('\\')
            || session_id.contains("..")
            || session_id.contains('\0')
        {
            return None;
        }
        let session_root = self.session_root.as_ref()?;
        let canonical_root = session_root.canonicalize().ok()?;
        let canonical_bucket = bucket.canonicalize().ok()?;
        if canonical_bucket.strip_prefix(&canonical_root).is_err() {
            return None;
        }
        // Pi may prefix the JSONL filename with a timestamp. The session
        // header is therefore the stable identity, not the filename stem.
        std::fs::read_dir(&canonical_bucket)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find_map(|path| {
                (path.extension().and_then(|extension| extension.to_str()) == Some("jsonl"))
                    .then(|| path.canonicalize().ok())
                    .flatten()
                    .filter(|resolved| {
                        resolved.strip_prefix(&canonical_bucket).is_ok() && resolved.is_file()
                    })
                    .filter(|resolved| {
                        parse_session_header(resolved).is_some_and(|header| header.id == session_id)
                    })
            })
    }

    /// Append a `session_info` name record to a session file (rename).
    ///
    /// The record is a full tree entry — id, parentId linking to the file's
    /// current tip, timestamp. Pi resolves the leaf from the newest entry, so a
    /// bare `{type,name}` record (this function's shape before 2026-09-24)
    /// leaves the tip id-less: Pi's next resume then appends a fresh root
    /// instead of continuing the branch, and every earlier entry falls off the
    /// active chain — the transcript silently drops the whole history
    /// (incident: 2026-09-24, `准备一开集团的PPT`).
    pub fn append_session_info_name(
        &self,
        session_file: &Path,
        name: &str,
    ) -> Result<(), HostDataError> {
        use std::io::Write;
        let parent_id = session_tip_id(session_file);
        let record = serde_json::json!({
            "type": "session_info",
            "id": new_session_entry_id(),
            "parentId": parent_id,
            "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "name": name,
        });
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(session_file)
            .map_err(|error| HostDataError::Io(error.to_string()))?;
        writeln!(file, "{record}").map_err(|error| HostDataError::Io(error.to_string()))?;
        Ok(())
    }

    /// P4 delete-batch: trash-first removal (move into a sibling staging
    /// trash, falling back to permanent unlink), running sessions are
    /// protected, and per-path results follow the legacy contract
    /// `{deleted, errors, running}`.
    #[allow(dead_code)]
    pub fn delete_session_batch(
        &self,
        file_paths: &[String],
        running_session_files: &[String],
    ) -> Result<serde_json::Value, HostDataError> {
        let Some(session_root) = &self.session_root else {
            return Ok(serde_json::json!({ "deleted": 0, "errors": [], "running": [] }));
        };
        let resolved_root = session_root
            .canonicalize()
            .unwrap_or_else(|_| session_root.clone());
        let trash_dir = session_root
            .parent()
            .map(|parent| parent.join(".picot-session-trash"))
            .unwrap_or_else(|| session_root.join(".picot-session-trash"));
        let mut result = serde_json::json!({ "deleted": 0, "errors": [], "running": [] });
        for path in file_paths {
            let path: &str = path;
            let canonical = std::path::PathBuf::from(path)
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(path));
            if !path.ends_with(".jsonl") || canonical.strip_prefix(&resolved_root).is_err() {
                if let Some(errors) = result["errors"].as_array_mut() {
                    errors.push(serde_json::Value::String(path.to_owned()));
                }
                continue;
            }
            let running_hit = running_session_files.iter().any(|running| {
                let running_canonical = std::path::PathBuf::from(running)
                    .canonicalize()
                    .unwrap_or_else(|_| std::path::PathBuf::from(running));
                running_canonical == canonical
            });
            if running_hit {
                if let Some(running) = result["running"].as_array_mut() {
                    running.push(serde_json::Value::String(path.to_owned()));
                }
                continue;
            }
            std::fs::create_dir_all(&trash_dir)
                .map_err(|error| HostDataError::Io(error.to_string()))?;
            let target = trash_dir.join(format!(
                "{}.{}",
                canonical
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                uuid::Uuid::new_v4().simple()
            ));
            let removed = std::fs::rename(&canonical, &target)
                .or_else(|_| std::fs::remove_file(&canonical))
                .is_ok();
            if removed {
                result["deleted"] =
                    serde_json::Value::from(result["deleted"].as_u64().unwrap_or(0) + 1);
            } else if let Some(errors) = result["errors"].as_array_mut() {
                errors.push(serde_json::Value::String(path.to_owned()));
            }
        }
        Ok(result)
    }

    /// Legacy `/api/cost-dashboard` payload (P4 parity): full aggregation
    /// with range/granularity/scope/models parameters over the shared
    /// session tree.
    pub fn cost_dashboard_compat(
        &self,
        workspace_id: &str,
        params: &crate::cost_compat::CostRangeParams,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<serde_json::Value, HostDataError> {
        // The landing page requests the dashboard without a workspace target;
        // `current_root` only feeds the scope=current filter, so an empty root
        // degrades that filter to no matches instead of failing scope=all.
        let workspace = self.workspace_root(workspace_id).unwrap_or_default();
        match &self.session_root {
            Some(session_root) => crate::cost_compat::scan_compat_cost_dashboard(
                session_root,
                &workspace,
                params,
                now,
                Some(&self.cost_cache),
            )
            .map_err(HostDataError::Io),
            None => Ok(crate::cost_compat::empty_payload(params)),
        }
    }

    /// Mention search (2026-09-19 spec): step 1 upstream parity plus step 2
    /// wide roots (`@../`, `@~/`, `@/`, `@C:/`, `@//server/share/`). The host
    /// is the sole root authority: the frame's declared root must equal what
    /// the query itself resolves to, else `InvalidMentionQuery`.
    pub fn search_file_mentions(
        &self,
        workspace_id: &str,
        query: &str,
        declared_root: Option<(&str, &str)>,
    ) -> Result<FileMentionSearchResult, HostDataError> {
        let root = self.workspace_root(workspace_id)?;
        if !query.starts_with('@') || query.contains('\0') {
            return Err(HostDataError::InvalidMentionQuery);
        }
        let raw = query.strip_prefix('@').unwrap_or_default();
        let (is_quoted, body) = if let Some(rest) = raw.strip_prefix('"') {
            (true, rest.strip_suffix('"').unwrap_or(rest))
        } else {
            (false, raw)
        };
        let normalized = body.replace('\\', "/");
        let plan = classify_mention_body(&normalized, &root)?;
        if let Some((declared_kind, declared_value)) = declared_root {
            let (kind, value) = plan.declaration();
            if kind != declared_kind || value != declared_value {
                return Err(HostDataError::InvalidMentionQuery);
            }
        }
        let root = match plan.resolve_search_root()? {
            Some(resolved) => resolved,
            None => root, // workspace kind: the registered root IS the root
        };
        // v3 special case (spec contract B): bare `@` lists the workspace
        // root's first level immediately — dot entries skipped, name-sorted.
        // (An empty body always classifies as the workspace root.)
        if normalized.is_empty() {
            let mut root_entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&root)
                .map(|dir| {
                    dir.flatten()
                        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                        .collect()
                })
                .unwrap_or_default();
            root_entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
            // `truncated` is the only signal the contract keeps for a cut list,
            // so report it here instead of claiming a complete listing.
            let truncated = root_entries.len() > MAX_MENTION_RESULTS;
            let items = root_entries
                .into_iter()
                .take(MAX_MENTION_RESULTS)
                .map(|entry| {
                    let is_directory = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    let display = entry.file_name().to_string_lossy().into_owned();
                    build_file_mention_candidate(
                        &display,
                        is_directory,
                        is_quoted_display(is_quoted, &display),
                    )
                })
                .collect();
            return Ok(FileMentionSearchResult { items, truncated });
        }
        // The filesystem join uses the BARE scope base (relative to the
        // resolved search root); the display form keeps the user's prefix.
        let (join_base, display_base, fuzzy) = match plan.scope.rsplit_once('/') {
            Some((base, fuzzy)) => (
                base.to_string(),
                format!("{}{base}/", plan.display_prefix),
                fuzzy.to_owned(),
            ),
            None => (
                String::new(),
                plan.display_prefix.clone(),
                plan.scope.clone(),
            ),
        };
        if plan
            .scope
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .any(is_ignored_mention_dir)
        {
            return Ok(FileMentionSearchResult {
                items: Vec::new(),
                truncated: false,
            });
        }
        let base_dir = match safe_join(&root, &join_base) {
            Ok(path) => path,
            Err(HostDataError::Io(_)) | Err(HostDataError::NotDirectory) => {
                return Ok(FileMentionSearchResult {
                    items: Vec::new(),
                    truncated: false,
                });
            }
            Err(error) => return Err(error),
        };
        // Containment is per-search-root now (wide roots walk outside the
        // workspace by design; the walk still never leaves its own root).
        let mut walk = FileMentionWalk::new(root.as_path(), fuzzy.to_lowercase(), is_quoted);
        walk.collect(&base_dir, &display_base, 1)?;
        walk.collected.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.description.cmp(&right.1.description))
        });
        Ok(FileMentionSearchResult {
            items: walk
                .collected
                .into_iter()
                .take(MAX_MENTION_RESULTS)
                .map(|(_, item)| item)
                .collect(),
            truncated: walk.truncated,
        })
    }

    /// Session root directory for compat handlers that validate absolute
    /// session file paths.
    pub fn session_root_path(&self) -> Option<PathBuf> {
        self.session_root.clone()
    }

    /// Reverse lookup: registered workspace id whose root canonicalizes to
    /// the given path (owner-only system-open validation).
    pub fn workspace_root_for_path(&self, path: &Path) -> Result<String, HostDataError> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.metadata
            .lock()
            .map_err(|_| HostDataError::UnknownWorkspace)?
            .workspace_id_for_canonical_root(&canonical)
            .map_err(|_| HostDataError::UnknownWorkspace)
    }

    pub fn list_files(
        &self,
        workspace_id: &str,
        relative_path: &str,
    ) -> Result<Vec<FileEntry>, HostDataError> {
        let root = self.workspace_root(workspace_id)?;
        let requested = resolve_for_listing(&root, relative_path)?;
        if !requested.is_dir() {
            return Err(HostDataError::NotDirectory);
        }
        let mut entries = std::fs::read_dir(&requested)
            .map_err(classify_listing_io)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                let kind = if file_type.is_dir() {
                    FileKind::Directory
                } else if file_type.is_file() {
                    FileKind::File
                } else {
                    return None;
                };
                let path = entry.path();
                let relative = path.strip_prefix(&root).ok()?;
                Some(FileEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    relative_path: relative.to_string_lossy().replace('\\', "/"),
                    kind,
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            let left_directory = left.kind == FileKind::Directory;
            let right_directory = right.kind == FileKind::Directory;
            right_directory
                .cmp(&left_directory)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        Ok(entries)
    }

    pub fn list_sessions(&self, workspace_id: &str) -> Result<Vec<SessionSummary>, HostDataError> {
        let workspace = self.workspace_root(workspace_id)?;
        let Some(bucket) = self.session_bucket_for_workspace(workspace_id) else {
            return Ok(Vec::new());
        };
        if !bucket.is_dir() {
            return Ok(Vec::new());
        }
        let files = std::fs::read_dir(&bucket)
            .map_err(|error| HostDataError::Io(error.to_string()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
            .collect::<Vec<_>>();
        let mut sessions = Vec::new();
        for path in files {
            let Some(classification) = self.classify_sidebar_file(&path, &workspace) else {
                continue;
            };
            if !should_project_sidebar_file(&classification) {
                continue;
            }
            if let Some(summary) = parse_session_summary(&path, workspace_id, &workspace)? {
                sessions.push(summary);
            }
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.modified_at_ms));
        Ok(sessions)
    }

    pub fn search_sessions(
        &self,
        workspace_id: &str,
        query: &str,
    ) -> Result<Vec<SessionSearchResult>, HostDataError> {
        const MAX_RESULTS: usize = 30;
        let workspace = self.workspace_root(workspace_id)?;
        let Some(bucket) = self.session_bucket_for_workspace(workspace_id) else {
            return Ok(Vec::new());
        };
        let query = query.trim().to_lowercase();
        if query.len() < 2 || !bucket.is_dir() {
            return Ok(Vec::new());
        }
        let files = std::fs::read_dir(&bucket)
            .map_err(|error| HostDataError::Io(error.to_string()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
            .collect::<Vec<_>>();
        let mut results = Vec::new();
        for path in files {
            if results.len() >= MAX_RESULTS {
                return Ok(results);
            }
            let Some(classification) = self.classify_sidebar_file(&path, &workspace) else {
                continue;
            };
            if !should_project_sidebar_file(&classification) {
                continue;
            }
            if let Some(result) = search_session_file(&path, &workspace, &query)? {
                results.push(result);
            }
        }
        Ok(results)
    }
}

fn find_chars(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn search_session_file(
    path: &Path,
    workspace: &Path,
    query: &str,
) -> Result<Option<SessionSearchResult>, HostDataError> {
    const MAX_MATCHES_PER_SESSION: usize = 3;
    let file = std::fs::File::open(path).map_err(|error| HostDataError::Io(error.to_string()))?;
    let mut session_id = None;
    let mut session_timestamp = String::new();
    let mut session_name = None;
    let mut first_message = None;
    let mut cwd = None;
    let mut matches = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("session") => {
                session_id = entry
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                session_timestamp = entry
                    .get("timestamp")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                cwd = entry
                    .get("cwd")
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from);
            }
            Some("session_info") => {
                session_name = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
            }
            Some("message") => {
                let role = entry
                    .pointer("/message/role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                let Some(text) = message_text(entry.pointer("/message/content")) else {
                    continue;
                };
                if role == "user" && first_message.is_none() {
                    first_message = Some(text.chars().take(120).collect::<String>());
                }
                if matches.len() >= MAX_MATCHES_PER_SESSION {
                    continue;
                }
                let lower: Vec<char> = text.to_lowercase().chars().collect();
                let needle: Vec<char> = query.chars().collect();
                if let Some(index) = find_chars(&lower, &needle) {
                    let original: Vec<char> = text.chars().collect();
                    let start = index.saturating_sub(60);
                    let end = (index + needle.len() + 60).min(original.len());
                    let snippet: String = original[start..end].iter().collect();
                    let snippet = format!(
                        "{}{}{}",
                        if start > 0 { "…" } else { "" },
                        snippet.replace('\n', " "),
                        if end < original.len() { "…" } else { "" }
                    );
                    matches.push(SessionSearchMatch { role, snippet });
                }
            }
            _ => {}
        }
    }
    let Some(session_id) = session_id else {
        return Ok(None);
    };
    let Some(cwd) = cwd.and_then(|cwd| cwd.canonicalize().ok()) else {
        return Ok(None);
    };
    if cwd != workspace || matches.is_empty() {
        return Ok(None);
    }
    Ok(Some(SessionSearchResult {
        session_id,
        session_name,
        session_timestamp,
        first_message,
        file_name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        matches,
    }))
}

/// One-shot, TTL'd, owner-scoped export tokens backing the P4
/// `session_export` control and the streaming
/// `GET /v2/session-export/{token}` route.
///
/// Host control issues grants after validating session identity and
/// containment; this registry owns token lifecycle and redemption.
#[allow(dead_code)]
pub struct SessionExportRegistry {
    tokens: std::sync::Mutex<HashMap<String, ExportGrant>>,
    max_per_owner: usize,
    ttl: std::time::Duration,
}

/// One outstanding export grant: bound to the issuing owner, the workspace
/// generation at issue time, the validated session file, and the expiry.
pub struct ExportGrant {
    pub file_path: std::path::PathBuf,
    pub owner: String,
    pub generation: u64,
    pub expires: std::time::Instant,
}

#[allow(dead_code)]
impl SessionExportRegistry {
    pub fn new(max_per_owner: usize, ttl: std::time::Duration) -> Self {
        Self {
            tokens: std::sync::Mutex::new(HashMap::new()),
            max_per_owner,
            ttl,
        }
    }

    pub fn ttl_secs(&self) -> u64 {
        self.ttl.as_secs()
    }

    /// Mint a one-shot token bound to an owner and a validated session file.
    pub fn issue(
        &self,
        owner: &str,
        generation: u64,
        file_path: std::path::PathBuf,
    ) -> Result<(String, std::time::Instant), String> {
        let mut tokens = self
            .tokens
            .lock()
            .map_err(|_| "export registry lock poisoned".to_string())?;
        // Expire stale entries for the owner first so the quota reflects
        // only live grants.
        tokens.retain(|_, grant| grant.expires > std::time::Instant::now());
        let outstanding = tokens.values().filter(|grant| grant.owner == owner).count();
        if outstanding >= self.max_per_owner {
            return Err("export token quota reached".to_string());
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let expires = std::time::Instant::now() + self.ttl;
        tokens.insert(
            token.clone(),
            ExportGrant {
                file_path,
                owner: owner.to_string(),
                generation,
                expires,
            },
        );
        Ok((token, expires))
    }

    /// Redeem a token: one-shot. Expired or unknown tokens are rejected and
    /// removed.
    pub fn redeem(
        &self,
        token: &str,
        owner: &str,
        current_generation: u64,
    ) -> Result<std::path::PathBuf, String> {
        self.redeem_checked(token, |grant_owner, grant_generation| {
            grant_owner == owner && grant_generation == current_generation
        })
    }

    /// Atomically redeem a one-shot token; the grant's owner and generation
    /// must still be current per the caller-supplied authority check.
    pub fn redeem_checked(
        &self,
        token: &str,
        is_current: impl Fn(&str, u64) -> bool,
    ) -> Result<std::path::PathBuf, String> {
        let (_, grant) = self
            .tokens
            .lock()
            .map_err(|_| "export registry lock poisoned".to_string())?
            .remove_entry(token)
            .ok_or_else(|| "export token is invalid or expired".to_string())?;
        if grant.expires <= std::time::Instant::now() || !is_current(&grant.owner, grant.generation)
        {
            return Err("export token is invalid or expired".to_string());
        }
        Ok(grant.file_path)
    }

    /// Owner destruction hygiene: drop every outstanding grant.
    pub fn revoke_owner(&self, owner: &str) {
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.retain(|_, grant| grant.owner != owner);
        }
    }

    #[cfg(test)]
    pub fn outstanding_for(&self, owner: &str) -> usize {
        self.tokens
            .lock()
            .map(|tokens| tokens.values().filter(|grant| grant.owner == owner).count())
            .unwrap_or(0)
    }
}

/// Bounded session header (legacy `parseSessionFile`): at most 50 lines,
/// early exit once name and first message are both known.
pub(crate) struct SessionHeader {
    pub id: String,
    pub timestamp: String,
    pub name: Option<String>,
    pub first_message: Option<String>,
    pub cwd: Option<String>,
    pub parent_session: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct SidebarFileMetadata {
    modified_at_ms: i64,
    size_bytes: i64,
}

struct SidebarFileClassification {
    is_sidebar_visible: bool,
    resolved: bool,
    header: Option<SessionHeader>,
}

struct SidebarScan {
    header: Option<SessionHeader>,
}

/// `(bucket_name, sessions, visible_count, hidden_subagent_count)` returned
/// by workspace session discovery. The fourth value is deprecated and always
/// zero; it remains only for frontend wire compatibility.
pub(crate) type WorkspaceSessionBucketResult = (
    Option<String>,
    Vec<serde_json::Value>,
    Option<usize>,
    Option<usize>,
);

fn cache_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn sidebar_file_metadata_matches(left: SidebarFileMetadata, right: SidebarFileMetadata) -> bool {
    left.modified_at_ms == right.modified_at_ms && left.size_bytes == right.size_bytes
}

fn sidebar_file_metadata(path: &Path) -> Option<SidebarFileMetadata> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified_at_ms = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()?;
    let size_bytes = metadata.len().try_into().ok()?;
    Some(SidebarFileMetadata {
        modified_at_ms,
        size_bytes,
    })
}

fn session_header_matches_workspace(header: &SessionHeader, workspace: &Path) -> bool {
    header
        .cwd
        .as_deref()
        .and_then(|cwd| Path::new(cwd).canonicalize().ok())
        .is_some_and(|cwd| cwd == workspace)
}

fn should_project_sidebar_file(classification: &SidebarFileClassification) -> bool {
    classification.resolved && classification.is_sidebar_visible
}

fn session_summary_value(path: &Path, header: &SessionHeader) -> serde_json::Value {
    let metadata = std::fs::metadata(path).ok();
    let size_bytes = metadata.as_ref().map(|value| value.len());
    let modified = metadata.as_ref().and_then(|value| {
        value
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis())
    });
    let created = metadata.as_ref().and_then(|value| {
        value
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis())
    });
    serde_json::json!({
        "id": header.id,
        "timestamp": header.timestamp,
        "name": header.name,
        "firstMessage": header.first_message,
        "cwd": header.cwd,
        "parentSession": header.parent_session,
        "file": path.file_name().map(|name| name.to_string_lossy().into_owned()),
        "filePath": path.to_string_lossy(),
        "mtime": modified,
        "ctime": created,
        "sizeBytes": size_bytes,
    })
}

fn scan_session_sidebar_visibility(path: &Path) -> Result<SidebarScan, HostDataError> {
    // ponytail: full linear scan per file; if sidebar refresh latency hurts
    // on huge buckets, re-land a head+tail window (see reverted 4c73f52) —
    // the head window must not truncate before the first user message.
    let file = std::fs::File::open(path).map_err(|error| HostDataError::Io(error.to_string()))?;
    let mut header: Option<(String, String, Option<String>, Option<String>)> = None;
    let mut name: Option<String> = None;
    let mut first_message: Option<String> = None;
    let mut user_messages = 0usize;
    let mut line_count = 0usize;
    for line in BufReader::new(file).lines() {
        // A corrupt tail (invalid UTF-8, IO error) ends the scan with the
        // fields collected so far instead of failing the whole file.
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        line_count += 1;
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("session") => {
                header = Some((
                    entry
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    entry
                        .get("timestamp")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    entry
                        .get("cwd")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    entry
                        .get("parentSession")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                ));
                if let Some(session_name) = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    name = Some(session_name.to_owned());
                }
            }
            Some("session_info") => {
                // Pi naming semantics (buildSessionInfo): every session_info
                // entry restates the name — the latest wins, empty clears.
                // Renames append at the file tip, so a scan can never stop
                // early on a settled name.
                name = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned);
            }
            Some("message")
                if entry
                    .pointer("/message/role")
                    .and_then(serde_json::Value::as_str)
                    == Some("user") =>
            {
                user_messages += 1;
                if first_message.is_none() {
                    first_message =
                        entry
                            .pointer("/message/content")
                            .and_then(|content| match content {
                                serde_json::Value::String(text) => {
                                    Some(text.chars().take(120).collect::<String>())
                                }
                                serde_json::Value::Array(blocks) => blocks
                                    .iter()
                                    .find(|block| {
                                        block.get("type").and_then(serde_json::Value::as_str)
                                            == Some("text")
                                    })
                                    .and_then(|block| block.get("text"))
                                    .and_then(serde_json::Value::as_str)
                                    .map(|text| text.chars().take(120).collect::<String>()),
                                _ => None,
                            });
                }
            }
            _ => {}
        }
    }
    let header = header.and_then(|(id, timestamp, cwd, parent_session)| {
        if id.is_empty() || (user_messages == 0 && line_count <= 4) {
            return None;
        }
        Some(SessionHeader {
            id,
            timestamp,
            name,
            first_message,
            cwd,
            parent_session,
        })
    });
    Ok(SidebarScan { header })
}

pub(crate) fn parse_session_header(path: &Path) -> Option<SessionHeader> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut header: Option<(String, String, Option<String>, Option<String>)> = None;
    let mut name: Option<String> = None;
    let mut first_message: Option<String> = None;
    let mut user_messages = 0usize;
    let mut line_count = 0usize;
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        line_count += 1;
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("session") => {
                header = Some((
                    entry
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    entry
                        .get("timestamp")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    entry
                        .get("cwd")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    entry
                        .get("parentSession")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                ));
                if let Some(session_name) = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    name = Some(session_name.to_owned());
                }
            }
            Some("session_info") => {
                // Same latest-wins semantics as scan_session_sidebar_visibility.
                name = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned);
            }
            Some("message") => {
                let role = entry
                    .pointer("/message/role")
                    .and_then(serde_json::Value::as_str);
                if role == Some("user") {
                    user_messages += 1;
                    if first_message.is_none() {
                        first_message =
                            entry
                                .pointer("/message/content")
                                .and_then(|content| match content {
                                    serde_json::Value::String(text) => {
                                        Some(text.chars().take(120).collect::<String>())
                                    }
                                    serde_json::Value::Array(blocks) => blocks
                                        .iter()
                                        .find(|block| {
                                            block.get("type").and_then(serde_json::Value::as_str)
                                                == Some("text")
                                        })
                                        .and_then(|block| block.get("text"))
                                        .and_then(serde_json::Value::as_str)
                                        .map(|text| text.chars().take(120).collect::<String>()),
                                    _ => None,
                                });
                    }
                }
            }
            _ => {}
        }
    }
    let (id, timestamp, cwd, parent_session) = header?;
    if id.is_empty() {
        return None;
    }
    // Suppress trivially-short one-shot sessions (no user input at all).
    if user_messages == 0 && line_count <= 4 {
        return None;
    }
    Some(SessionHeader {
        id,
        timestamp,
        name,
        first_message,
        cwd,
        parent_session,
    })
}

/// Attach the stable entry id to a user/assistant message so the frontend
/// can anchor transcript rows (`data-entry-id`) without carrying the whole
/// session-file entry shape over the wire.
fn message_with_entry_id(mut message: serde_json::Value, entry_id: &str) -> serde_json::Value {
    let role = message.get("role").and_then(serde_json::Value::as_str);
    if role != Some("user") && role != Some("assistant") {
        return message;
    }
    if let Some(object) = message.as_object_mut() {
        object.insert(
            "entryId".to_owned(),
            serde_json::Value::String(entry_id.to_owned()),
        );
    }
    message
}

/// Legacy `sessionActivityTime`: mtime, else parsed timestamp, else ctime.
fn session_activity_time(entry: &serde_json::Value) -> f64 {
    if let Some(modified) = entry.get("mtime").and_then(serde_json::Value::as_f64) {
        return modified;
    }
    if let Some(timestamp) = entry
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::cost_compat::parse_js_date)
    {
        return timestamp.timestamp_millis() as f64;
    }
    entry
        .get("ctime")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
}

pub(crate) fn parse_session_metrics(path: &Path) -> Result<Option<SessionMetrics>, HostDataError> {
    let file = std::fs::File::open(path).map_err(|error| HostDataError::Io(error.to_string()))?;
    let mut metrics = SessionMetrics {
        model: "unknown".to_owned(),
        ..SessionMetrics::default()
    };
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(timestamp) = entry.get("timestamp").and_then(serde_json::Value::as_str) {
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp) {
                metrics.last_active = Some(parsed.with_timezone(&chrono::Utc));
            }
        }
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("session") => {
                metrics.id = entry
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                metrics.timestamp = entry
                    .get("timestamp")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                metrics.cwd = entry
                    .get("cwd")
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from);
            }
            Some("session_info") => {
                if let Some(name) = entry.get("name").and_then(serde_json::Value::as_str) {
                    metrics.title = name.to_owned();
                }
            }
            Some("model_change") => {
                if let Some(model) = entry.get("model").and_then(serde_json::Value::as_str) {
                    metrics.model = model.to_owned();
                }
            }
            Some("message") => {
                let Some(role) = entry
                    .pointer("/message/role")
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                if role == "user" {
                    metrics.user_messages += 1;
                    continue;
                }
                if role != "assistant" {
                    continue;
                }
                if let Some(model) = entry
                    .pointer("/message/model")
                    .and_then(serde_json::Value::as_str)
                {
                    metrics.model = model.to_owned();
                }
                let usage = entry.pointer("/message/usage");
                let cost = usage
                    .and_then(|usage| usage.pointer("/cost/total"))
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                metrics.total_cost += cost;
                metrics.input_tokens += usage
                    .and_then(|usage| usage.get("input"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                metrics.output_tokens += usage
                    .and_then(|usage| usage.get("output"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                metrics.cache_read += usage
                    .and_then(|usage| usage.get("cacheRead"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let tool_calls: Vec<&str> = entry
                    .pointer("/message/content")
                    .and_then(serde_json::Value::as_array)
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|block| {
                                block.get("type").and_then(serde_json::Value::as_str)
                                    == Some("toolCall")
                            })
                            .filter_map(|block| {
                                block.get("name").and_then(serde_json::Value::as_str)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !tool_calls.is_empty() && cost > 0.0 {
                    let per_tool_cost = cost / tool_calls.len() as f64;
                    for tool_name in &tool_calls {
                        *metrics
                            .tool_cost_by_name
                            .entry((*tool_name).to_owned())
                            .or_insert(0.0) += per_tool_cost;
                    }
                }
                metrics.tool_calls += tool_calls.len() as u64;
                metrics.assistant_messages += 1;
                metrics.cache_write += usage
                    .and_then(|usage| usage.get("cacheWrite"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
            }
            _ => {}
        }
    }
    if metrics.id.is_empty() {
        return Ok(None);
    }
    // Keep the recorded cwd verbatim (the legacy payload echoes it) and add
    // a canonicalized copy so scope filters compare realpath-to-realpath.
    metrics.cwd_canonical = metrics
        .cwd
        .as_ref()
        .and_then(|cwd| cwd.canonicalize().ok())
        .or_else(|| metrics.cwd.clone());
    if metrics.title.is_empty() {
        metrics.title = "Untitled".to_owned();
    }
    Ok(Some(metrics))
}

fn parse_session_summary(
    path: &Path,
    workspace_id: &str,
    workspace: &Path,
) -> Result<Option<SessionSummary>, HostDataError> {
    let file = std::fs::File::open(path).map_err(|error| HostDataError::Io(error.to_string()))?;
    let mut id = None;
    let mut timestamp = String::new();
    let mut cwd = None;
    let mut name = None;
    let mut first_message = None;
    let mut user_message_count = 0;
    let mut line_count = 0;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        line_count += 1;
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("session") => {
                id = entry
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                timestamp = entry
                    .get("timestamp")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                cwd = entry
                    .get("cwd")
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from);
            }
            Some("session_info") => {
                // Latest session_info wins (Pi buildSessionInfo semantics);
                // renames append at the file tip, so the whole file streams.
                name = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned);
            }
            Some("message")
                if entry
                    .pointer("/message/role")
                    .and_then(serde_json::Value::as_str)
                    == Some("user") =>
            {
                user_message_count += 1;
                if first_message.is_none() {
                    first_message = message_text(entry.pointer("/message/content"))
                        .map(|text| text.chars().take(120).collect());
                }
            }
            _ => {}
        }
    }
    let Some(id) = id else { return Ok(None) };
    if user_message_count == 0 && line_count <= 4 {
        return Ok(None);
    }
    let Some(cwd) = cwd.and_then(|cwd| cwd.canonicalize().ok()) else {
        return Ok(None);
    };
    if cwd != workspace {
        return Ok(None);
    }
    let metadata = std::fs::metadata(path).map_err(|error| HostDataError::Io(error.to_string()))?;
    let modified_at_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis());
    Ok(Some(SessionSummary {
        id,
        timestamp,
        name,
        first_message,
        workspace_id: workspace_id.to_owned(),
        file_name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        modified_at_ms,
    }))
}

/// One JSONL session line reduced to its structural identity. Malformed or
/// id-less lines are skipped: the file is append-only user data, not a
/// trusted contract.
struct SessionLine {
    entry: serde_json::Value,
    id: String,
    parent_id: Option<String>,
    is_message: bool,
}

fn parse_session_lines(file: std::fs::File) -> Vec<SessionLine> {
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_owned();
            let parent_id = entry
                .get("parentId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let is_message =
                entry.get("type").and_then(serde_json::Value::as_str) == Some("message");
            Some(SessionLine {
                entry,
                id,
                parent_id,
                is_message,
            })
        })
        .collect()
}

/// Pi session entry ids are 8 lowercase hex characters (for example
/// `42233c7f`). Host-written records must use the same shape or Pi's loader
/// cannot address them.
fn new_session_entry_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The id of the file's newest entry — the parent a freshly appended record
/// must link to. `None` when the file holds no entry yet: the header line
/// carries an id but sits outside the tree, so it is never a parent.
fn session_tip_id(session_file: &Path) -> Option<String> {
    let file = std::fs::File::open(session_file).ok()?;
    parse_session_lines(file)
        .into_iter()
        .rfind(|line| line.entry.get("type").and_then(serde_json::Value::as_str) != Some("session"))
        .map(|line| line.id)
}

/// The active branch: indices from root to the LAST message entry (the tip)
/// via parentId links, in file order. `None` when the file has no message;
/// the bool is false when the parent chain cycles (callers keep the partial
/// chain but treat the leaf as unknown).
fn active_chain(lines: &[SessionLine]) -> Option<(Vec<usize>, bool)> {
    let id_to_idx: HashMap<&str, usize> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| (line.id.as_str(), i))
        .collect();
    let tip = lines.iter().rposition(|line| line.is_message)?;
    let mut walked = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut current = tip;
    let mut intact = true;
    loop {
        if !visited.insert(current) {
            intact = false; // cycle guard
            break;
        }
        walked.push(current);
        match lines[current].parent_id.as_deref() {
            None => break,
            Some(pid) => match id_to_idx.get(pid) {
                Some(&idx) => current = idx,
                None => break,
            },
        }
    }
    walked.reverse();
    Some((walked, intact))
}

fn message_text(content: Option<&serde_json::Value>) -> Option<String> {
    match content? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .find(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
            .and_then(|block| block.get("text"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

// ─── @ mention search machinery (upstream parity + spec additions) ────────────

const MAX_MENTION_RESULTS: usize = 20;
const MAX_MENTION_VISITED: usize = 10_000;
const MAX_MENTION_COLLECTED: usize = 200;
const MAX_MENTION_DEPTH: usize = 4;
const MAX_MENTION_MILLIS: u128 = 500;

const IGNORED_MENTION_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    "target",
    ".next",
    ".nuxt",
    ".cache",
    "coverage",
    ".venv",
    "venv",
    "__pycache__",
];

fn is_ignored_mention_dir(name: &str) -> bool {
    IGNORED_MENTION_DIRS.contains(&name)
}

/// Parsed mention query: which root kind the user's prefix selects, the
/// remaining scope under it, and the display prefix that inserted values keep.
pub struct MentionQueryPlan {
    kind: &'static str,
    /// Declaration value mirrored by the frontend classifier (audit field).
    declared_value: String,
    /// Scope base + fuzzy remainder under the resolved root ("" allowed).
    scope: String,
    /// Inserted tokens keep this prefix ("../", "~/", "/", "C:/", "//s/share/",
    /// or "./" for the explicit-dot workspace form).
    display_prefix: String,
    /// None = the registered workspace root (no separate path to resolve).
    resolved_root: Option<PathBuf>,
}

impl MentionQueryPlan {
    /// The wire declaration both sides must agree on (contract D).
    pub fn declaration(&self) -> (&'static str, &str) {
        (self.kind, self.declared_value.as_str())
    }

    /// Resolve the search root to a canonical directory, probing wide roots
    /// with a bounded timeout so dead drives/UNC shares fail fast. Returns
    /// Ok(None) for the workspace kind — its root is the registered one.
    fn resolve_search_root(&self) -> Result<Option<PathBuf>, HostDataError> {
        let Some(path) = self.resolved_root.as_ref() else {
            return Ok(None);
        };
        if self.kind == "drive" || self.kind == "unc" {
            let reachable = std::sync::mpsc::channel();
            let (tx, rx) = reachable;
            let target = path.clone();
            std::thread::spawn(move || {
                let ok = std::fs::metadata(&target)
                    .map(|meta| meta.is_dir())
                    .unwrap_or(false);
                let _ = tx.send(ok);
            });
            return match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(true) => path
                    .canonicalize()
                    .map(Some)
                    .map_err(|error| HostDataError::MentionRootUnavailable(error.to_string())),
                Ok(false) => Err(HostDataError::MentionRootUnavailable(
                    "search root is not a reachable directory".into(),
                )),
                Err(_) => Err(HostDataError::MentionRootUnavailable(
                    "search root probe timed out".into(),
                )),
            };
        }
        path.canonicalize()
            .map(Some)
            .map_err(|error| HostDataError::MentionRootUnavailable(error.to_string()))
    }
}

/// Pure classification of the normalized mention body (no filesystem access):
/// selects the search root kind, strips its prefix, and guards mid-word
/// traversal. Mirrored by the frontend `classifyMentionRoot` — the wire
/// declaration must match this exactly.
fn classify_mention_body(
    normalized: &str,
    workspace_root: &Path,
) -> Result<MentionQueryPlan, HostDataError> {
    classify_mention_body_for_platform(normalized, workspace_root, cfg!(target_os = "windows"))
}

/// Windows drive prefix (`C:/…` / `c:/…`), mirroring the frontend
/// `classifyMentionRoot` drive branch: the letter is upper-cased for the wire
/// declaration, so both letter cases name the same root. Returns the letter and
/// the scope after `X:/` (empty for a bare `C:`).
///
/// Platform-independent on purpose: the branch it serves is Windows-only, but a
/// `#[cfg(windows)]` call site is invisible to a macOS `cargo check`, which is
/// how this function shipped undefined and broke the Windows build.
fn drive_prefix(normalized: &str) -> Option<(String, &str)> {
    let mut chars = normalized.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next() != Some(':') {
        return None;
    }
    let rest = normalized.get(2..)?;
    let scope = match rest.strip_prefix('/') {
        Some(scope) => scope,
        None if rest.is_empty() => "",
        None => return None,
    };
    Some((letter.to_ascii_uppercase().to_string(), scope))
}

/// `classify_mention_body` with the platform as a runtime value, so both the
/// Windows and the POSIX branches compile and run under test on any host.
fn classify_mention_body_for_platform(
    normalized: &str,
    workspace_root: &Path,
    windows: bool,
) -> Result<MentionQueryPlan, HostDataError> {
    // Home: `~` or `~/…` — expanded by the host only, never the browser.
    if normalized == "~" || normalized.starts_with("~/") {
        let scope = normalized.strip_prefix("~/").unwrap_or("").to_string();
        if scope.split('/').any(|part| part == "..") {
            return Err(HostDataError::InvalidMentionQuery);
        }
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or_else(|| {
                HostDataError::MentionRootUnavailable("home directory unavailable".into())
            })?;
        return Ok(MentionQueryPlan {
            kind: "home",
            declared_value: "~".into(),
            scope,
            display_prefix: "~/".into(),
            resolved_root: Some(home),
        });
    }
    // Windows named drive: `C:/…` (bare `@/` stays illegal on Windows — no
    // single filesystem root). POSIX falls through to the absolute branch.
    if windows {
        if let Some((letter, scope)) = drive_prefix(normalized) {
            if scope.split('/').any(|part| part == "..") {
                return Err(HostDataError::InvalidMentionQuery);
            }
            let root = format!("{letter}:/");
            return Ok(MentionQueryPlan {
                kind: "drive",
                declared_value: root.clone(),
                scope: scope.to_string(),
                display_prefix: root.clone(),
                resolved_root: Some(PathBuf::from(root)),
            });
        }
    }
    if windows && normalized.starts_with('/') {
        // UNC `//server/share/…` is the only legal `@/` form on Windows.
        if let Some(rest) = normalized.strip_prefix("//") {
            let mut parts = rest.splitn(3, '/');
            let server = parts.next().unwrap_or_default();
            let share = parts.next().unwrap_or_default();
            let scope = parts.next().unwrap_or_default();
            if scope.split('/').any(|part| part == "..") {
                return Err(HostDataError::InvalidMentionQuery);
            }
            if server.is_empty() || share.is_empty() {
                return Err(HostDataError::InvalidMentionQuery);
            }
            let root = format!("//{server}/{share}");
            return Ok(MentionQueryPlan {
                kind: "unc",
                declared_value: root.clone(),
                scope: scope.to_string(),
                display_prefix: format!("{root}/"),
                resolved_root: Some(PathBuf::from(root)),
            });
        }
        return Err(HostDataError::InvalidMentionQuery);
    }
    // POSIX absolute: `@/…` searches from the filesystem root.
    if !windows {
        if let Some(scope) = normalized.strip_prefix('/') {
            if scope.split('/').any(|part| part == "..") {
                return Err(HostDataError::InvalidMentionQuery);
            }
            return Ok(MentionQueryPlan {
                kind: "absolute",
                declared_value: "/".into(),
                scope: scope.to_string(),
                display_prefix: "/".into(),
                resolved_root: Some(PathBuf::from("/")),
            });
        }
    }
    // Parent chain: one leading `../` per level (multi-level allowed; the
    // climb floors at the filesystem/drive root). Mid-word `..` stays illegal.
    let mut levels = 0usize;
    let mut rest = normalized;
    while let Some(tail) = rest.strip_prefix("../") {
        levels += 1;
        rest = tail;
    }
    if rest == ".." {
        levels += 1;
        rest = "";
    }
    if levels > 0 {
        if rest.split('/').any(|part| part == "..") {
            return Err(HostDataError::InvalidMentionQuery);
        }
        let workspace_str = workspace_root.to_string_lossy().replace('\\', "/");
        let mut components: Vec<&str> =
            workspace_str.split('/').filter(|c| !c.is_empty()).collect();
        let floor = if windows { 1 } else { 0 };
        while components.len() > floor && levels > 0 {
            components.pop();
            levels -= 1;
        }
        let mut climbed = String::new();
        if windows && components.len() == 1 && components[0].ends_with(':') {
            climbed.push_str(components[0]);
            climbed.push('/');
        } else {
            for component in &components {
                climbed.push('/');
                climbed.push_str(component);
            }
        }
        if climbed.is_empty() {
            climbed = if windows {
                // Single remaining component = the drive root itself.
                format!("/{}", components.first().copied().unwrap_or_default())
            } else {
                "/".to_string()
            };
        }
        let mut display_prefix = String::new();
        for _ in 0..count_parent_levels(normalized) {
            display_prefix.push_str("../");
        }
        return Ok(MentionQueryPlan {
            kind: "absolute",
            declared_value: climbed.clone(),
            scope: rest.to_string(),
            display_prefix,
            resolved_root: Some(PathBuf::from(climbed)),
        });
    }
    // Workspace form, optionally with an explicit `./` prefix.
    let (scope, display_prefix) = if let Some(rest) = normalized.strip_prefix("./") {
        (rest.to_string(), "./".to_string())
    } else {
        (normalized.to_string(), String::new())
    };
    if scope.split('/').any(|part| part == "..") {
        return Err(HostDataError::InvalidMentionQuery);
    }
    Ok(MentionQueryPlan {
        kind: "workspace",
        declared_value: String::new(),
        scope,
        display_prefix,
        resolved_root: None,
    })
}

/// Number of leading `../` segments in the normalized body (display prefix).
fn count_parent_levels(normalized: &str) -> usize {
    let mut count = 0usize;
    let mut rest = normalized;
    while let Some(tail) = rest.strip_prefix("../") {
        count += 1;
        rest = tail;
    }
    if rest == ".." {
        count += 1;
    }
    count
}

struct FileMentionWalk<'a> {
    root: &'a Path,
    fuzzy: String,
    is_quoted: bool,
    started: std::time::Instant,
    visited: usize,
    collected: Vec<(u16, FileMentionCandidate)>,
    truncated: bool,
}

impl<'a> FileMentionWalk<'a> {
    fn new(root: &'a Path, fuzzy: String, is_quoted: bool) -> Self {
        Self {
            root,
            fuzzy,
            is_quoted,
            started: std::time::Instant::now(),
            visited: 0,
            collected: Vec::new(),
            truncated: false,
        }
    }

    fn budget_hit(&self) -> bool {
        self.visited >= MAX_MENTION_VISITED
            || self.collected.len() >= MAX_MENTION_COLLECTED
            || self.started.elapsed().as_millis() >= MAX_MENTION_MILLIS
    }

    fn collect(
        &mut self,
        dir: &Path,
        display_base: &str,
        depth: usize,
    ) -> Result<(), HostDataError> {
        if self.budget_hit() {
            self.truncated = true;
            return Ok(());
        }
        // Wide roots routinely contain unreadable directories (`@/` from the
        // filesystem root, protected system dirs); skip them instead of
        // aborting the whole search. Workspace-relative walks are unaffected.
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries.filter_map(Result::ok) {
            if self.budget_hit() {
                self.truncated = true;
                return Ok(());
            }
            self.visited += 1;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let is_directory = file_type.is_dir();
            if !is_directory && !file_type.is_file() {
                continue;
            }
            // Spec contract C: hidden entries (dot-prefixed) are skipped in
            // addition to the upstream IGNORED directory table.
            if name.starts_with('.') {
                continue;
            }
            if is_directory && is_ignored_mention_dir(&name) {
                continue;
            }
            let display_path = format!("{display_base}{name}");
            let score = score_mention(&display_path, &name, &self.fuzzy, is_directory);
            if score > 0 {
                self.collected.push((
                    score,
                    build_file_mention_candidate(
                        &display_path,
                        is_directory,
                        is_quoted_display(self.is_quoted, &display_path),
                    ),
                ));
            }
            if is_directory && depth < MAX_MENTION_DEPTH {
                let path = entry.path();
                if path.starts_with(self.root) {
                    self.collect(&path, &format!("{display_path}/"), depth + 1)?;
                }
            }
        }
        Ok(())
    }
}

fn score_mention(display_path: &str, name: &str, fuzzy: &str, is_directory: bool) -> u16 {
    let base = if fuzzy.is_empty() {
        if is_directory {
            11
        } else {
            1
        }
    } else {
        let name = name.to_lowercase();
        if name == fuzzy {
            100
        } else if name.starts_with(fuzzy) {
            80
        } else if name.contains(fuzzy) {
            50
        } else if display_path.to_lowercase().contains(fuzzy) {
            30
        } else {
            0
        }
    };
    if base > 0 && is_directory {
        base + 10
    } else {
        base
    }
}

fn is_quoted_display(was_quoted: bool, display_path: &str) -> bool {
    was_quoted || display_path.contains(' ')
}

fn build_file_mention_candidate(
    display_path: &str,
    is_directory: bool,
    needs_quotes: bool,
) -> FileMentionCandidate {
    let value_path = if is_directory {
        format!("{display_path}/")
    } else {
        display_path.to_owned()
    };
    let value = if needs_quotes {
        format!("@\"{value_path}\"")
    } else {
        format!("@{value_path}")
    };
    let label = Path::new(display_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(display_path);
    FileMentionCandidate {
        value,
        label: format!("{label}{}", if is_directory { "/" } else { "" }),
        description: display_path.to_owned(),
        is_directory,
    }
}

/// Resolve a listing target under `root`, keeping "unreachable right now"
/// distinguishable from "this path is wrong".
///
/// `safe_join` collapses every canonicalize failure into `Io`, which is fine
/// for a one-shot read but wrong for a tree: an ejected volume or a dropped
/// network share must not read to the WebView as "the user deleted this
/// directory", because the frontend invalidates its cache on a real deletion
/// and keeps it on a transient one.
fn resolve_for_listing(root: &Path, relative_path: &str) -> Result<PathBuf, HostDataError> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(HostDataError::InvalidRelativePath);
    }
    let root = root.canonicalize().map_err(classify_listing_io)?;
    let joined = root
        .join(relative)
        .canonicalize()
        .map_err(classify_listing_io)?;
    if !joined.starts_with(&root) {
        return Err(HostDataError::OutsideWorkspace);
    }
    Ok(joined)
}

/// Transient-vs-permanent split for listing I/O. The errno set covers the
/// network-filesystem answers (`ESTALE`, `ENOTCONN`, `EIO`, host-down); the
/// `ErrorKind` arms cover everything the portable layer already names.
fn classify_listing_io(error: std::io::Error) -> HostDataError {
    let transient = match error.kind() {
        std::io::ErrorKind::NotFound
        | std::io::ErrorKind::NotConnected
        | std::io::ErrorKind::TimedOut
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::ConnectionReset => true,
        _ => is_transient_listing_errno(&error),
    };
    if transient {
        HostDataError::TemporarilyUnavailable
    } else {
        HostDataError::Io(error.to_string())
    }
}

/// Unix-only: Windows reports these conditions through `ErrorKind`.
#[cfg(unix)]
fn is_transient_listing_errno(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ESTALE)
            | Some(libc::EIO)
            | Some(libc::ENXIO)
            | Some(libc::ENOTCONN)
            | Some(libc::EHOSTDOWN)
            | Some(libc::EHOSTUNREACH)
            | Some(libc::ETIMEDOUT)
    )
}

#[cfg(not(unix))]
fn is_transient_listing_errno(_error: &std::io::Error) -> bool {
    false
}

fn safe_join(root: &Path, relative_path: &str) -> Result<PathBuf, HostDataError> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(HostDataError::InvalidRelativePath);
    }
    let joined = root.join(relative);
    let canonical = joined
        .canonicalize()
        .map_err(|error| HostDataError::Io(error.to_string()))?;
    if !canonical.starts_with(root) {
        return Err(HostDataError::OutsideWorkspace);
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::{
        classify_mention_body_for_platform, should_project_sidebar_file, FileKind, HostDataError,
        HostDataPlane, SidebarFileClassification,
    };
    use crate::metadata_store::MetadataStore;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn subagent_marker() -> String {
        r#"{"type":"custom","customType":"pi-subagents_launch_metadata","data":{"version":1,"name":"worker","sessionMode":"lineage-only"}}"#.to_owned()
    }

    fn session_fixture(id: &str, cwd: &str, parent_session: Option<&str>, message: &str) -> String {
        let mut header = serde_json::json!({
            "type": "session",
            "id": id,
            "timestamp": "2026-09-12T00:00:00.000Z",
            "cwd": cwd,
        });
        if let Some(parent_session) = parent_session {
            header["parentSession"] = serde_json::Value::String(parent_session.to_owned());
        }
        [
            header,
            serde_json::json!({ "type": "session_info", "name": id }),
            serde_json::json!({
                "type": "message",
                "message": { "role": "user", "content": message },
            }),
        ]
        .into_iter()
        .map(|entry| entry.to_string())
        .collect::<Vec<_>>()
        .join("\n")
            + "\n"
    }

    fn write_session_fixture(bucket: &std::path::Path, filename: &str, content: impl AsRef<[u8]>) {
        fs::write(bucket.join(filename), content).unwrap();
    }

    fn test_data(workspace: &std::path::Path) -> (HostDataPlane, String) {
        let db = std::env::temp_dir().join(format!("picot-host-data-db-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(Mutex::new(MetadataStore::open(&db).unwrap()));
        let (row, _) = store.lock().unwrap().add_workspace(workspace).unwrap();
        store
            .lock()
            .unwrap()
            .set_workspace_session_bucket_from_pi(&row.workspace_id, "--test-bucket--")
            .unwrap();
        (HostDataPlane::new(store), row.workspace_id)
    }

    #[test]
    fn file_mentions_bare_query_lists_workspace_root_first_level() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-mention-bare-{nonce}"));
        let workspace = temp.join("workspace");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("package.json"), "{}").unwrap();
        fs::write(workspace.join("README.md"), "x").unwrap();
        fs::write(workspace.join(".hidden"), "x").unwrap();
        let (data, workspace_id) = test_data(&workspace);

        // Bare `@` must list files immediately (pi TUI affordance): the root's
        // first level, dot entries skipped, deterministic name order. Values
        // are RELATIVE candidates now (value/label/description, not FileEntry).
        let result = data.search_file_mentions(&workspace_id, "@", None).unwrap();
        let labels: Vec<String> = result.items.iter().map(|item| item.label.clone()).collect();
        assert!(
            labels.contains(&"package.json".to_string()),
            "got {labels:?}"
        );
        assert!(labels.contains(&"README.md".to_string()), "got {labels:?}");
        assert!(labels.contains(&"src/".to_string()), "got {labels:?}");
        assert!(!labels.iter().any(|label| label.starts_with('.')));
        let src = result
            .items
            .iter()
            .find(|item| item.label == "src/")
            .unwrap();
        assert!(src.is_directory);
        assert_eq!(src.value, "@src/");
        assert!(!result.truncated);

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn file_mentions_upstream_alignment_relative_scope_and_guards() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-mention-align-{nonce}"));
        let workspace = temp.join("workspace");
        fs::create_dir_all(workspace.join("src/deep")).unwrap();
        fs::write(workspace.join("package.json"), "{}").unwrap();
        fs::write(workspace.join("src/note.md"), "x").unwrap();
        fs::write(workspace.join("src/my file.txt"), "x").unwrap();
        fs::write(workspace.join(".env"), "x").unwrap();
        fs::create_dir_all(workspace.join("node_modules/pkg")).unwrap();
        fs::write(workspace.join("node_modules/pkg/index.js"), "x").unwrap();
        let (data, workspace_id) = test_data(&workspace);

        // Guards (upstream parity): no @ prefix, absolute prefix, traversal,
        // and NUL are invalid mention queries.
        assert!(matches!(
            data.search_file_mentions(&workspace_id, "pack", None),
            Err(HostDataError::InvalidMentionQuery)
        ));
        assert!(matches!(
            data.search_file_mentions(&workspace_id, "@/etc/passwd/../../x", None),
            Err(HostDataError::InvalidMentionQuery)
        ));
        assert!(matches!(
            data.search_file_mentions(&workspace_id, "@a/../b", None),
            Err(HostDataError::InvalidMentionQuery)
        ));
        assert!(matches!(
            data.search_file_mentions(&workspace_id, "@a\u{0}b", None),
            Err(HostDataError::InvalidMentionQuery)
        ));

        // Fuzzy name match yields a RELATIVE value; the description is the
        // visible (workspace-relative) form.
        let result = data
            .search_file_mentions(&workspace_id, "@pack", None)
            .unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].value, "@package.json");
        assert_eq!(result.items[0].description, "package.json");
        assert!(!result.items[0].is_directory);

        // Scope split: "@src/no" only walks src/, yielding src-prefixed paths.
        let result = data
            .search_file_mentions(&workspace_id, "@src/no", None)
            .unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].value, "@src/note.md");
        assert_eq!(result.items[0].description, "src/note.md");

        // Space in path (found via scoped search) quotes the inserted token.
        let result = data
            .search_file_mentions(&workspace_id, "@src/my fi", None)
            .unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].value, "@\"src/my file.txt\"");
        assert_eq!(result.items[0].description, "src/my file.txt");

        // Unterminated opening quote is tolerated mid-typing (upstream).
        let result = data
            .search_file_mentions(&workspace_id, "@\"src/my fi", None)
            .unwrap();
        assert_eq!(result.items.len(), 1);

        // Hidden entries and IGNORED directories never surface.
        let result = data
            .search_file_mentions(&workspace_id, "@env", None)
            .unwrap();
        assert!(result.items.is_empty());
        let result = data
            .search_file_mentions(&workspace_id, "@index", None)
            .unwrap();
        assert!(result.items.is_empty(), "node_modules must be skipped");

        fs::remove_dir_all(temp).unwrap();
    }

    /// The Windows branches used to be `#[cfg(target_os = "windows")]`-only, so
    /// a macOS `cargo check` never compiled them — which is how a call to the
    /// undefined `drive_prefix` shipped and broke the Windows build. These run
    /// the Windows logic on any host.
    #[test]
    fn mention_parent_traversal_reaches_windows_drive_root() {
        let workspace = Path::new("C:/work/project");
        let plan = classify_mention_body_for_platform("../../foo", workspace, true).unwrap();
        assert_eq!(plan.declaration(), ("absolute", "C:/"));
        assert_eq!(plan.scope, "foo");
        assert_eq!(plan.resolved_root.as_deref(), Some(Path::new("C:/")));
    }

    #[test]
    fn mention_drive_form_mirrors_the_frontend_declaration() {
        let workspace = Path::new("/workspace");
        // Lowercase input must declare an upper-cased letter, exactly like
        // `body[0].toUpperCase()` on the browser side; otherwise every `@c:/x`
        // declaration mismatches and the query is rejected.
        let plan = classify_mention_body_for_platform("c:/Users/dev/src", workspace, true).unwrap();
        assert_eq!(plan.declaration(), ("drive", "C:/"));
        assert_eq!(plan.scope, "Users/dev/src");
        assert_eq!(plan.display_prefix, "C:/");

        // `C:` alone is the drive root with an empty scope.
        let plan = classify_mention_body_for_platform("C:", workspace, true).unwrap();
        assert_eq!(plan.declaration(), ("drive", "C:/"));
        assert_eq!(plan.scope, "");

        assert!(matches!(
            classify_mention_body_for_platform("c:/a/../b", workspace, true),
            Err(HostDataError::InvalidMentionQuery)
        ));

        // POSIX must keep `c:/x` workspace-relative — the drive form is
        // Windows-only, mirroring the frontend's `windows` test.
        let plan = classify_mention_body_for_platform("c:/x", workspace, false).unwrap();
        assert_eq!(plan.declaration(), ("workspace", ""));
        assert_eq!(plan.scope, "c:/x");
    }

    #[test]
    fn mention_windows_root_forms_differ_from_posix() {
        let workspace = Path::new("/workspace");
        // Windows has no single filesystem root: bare `@/etc` is illegal, UNC
        // is the only legal `@/` form.
        assert!(matches!(
            classify_mention_body_for_platform("/etc", workspace, true),
            Err(HostDataError::InvalidMentionQuery)
        ));
        let plan =
            classify_mention_body_for_platform("//server/share/dir", workspace, true).unwrap();
        assert_eq!(plan.declaration(), ("unc", "//server/share"));
        assert_eq!(plan.scope, "dir");

        // POSIX `@/etc` searches the filesystem root.
        let plan = classify_mention_body_for_platform("/etc", workspace, false).unwrap();
        assert_eq!(plan.declaration(), ("absolute", "/"));
        assert_eq!(plan.scope, "etc");
    }

    #[test]
    fn file_mentions_wide_roots_parent_absolute_and_declaration() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-mention-wide-{nonce}"));
        let workspace = temp.join("workspace");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("inside.ts"), "x").unwrap();
        fs::write(temp.join("outside.ts"), "x").unwrap();
        let (data, workspace_id) = test_data(&workspace);

        // `@../out` searches the workspace PARENT; the inserted token keeps
        // the ../ display prefix.
        let result = data
            .search_file_mentions(&workspace_id, "@../out", None)
            .unwrap();
        assert_eq!(result.items.len(), 1, "got {:?}", result.items);
        assert_eq!(result.items[0].value, "@../outside.ts");
        assert_eq!(result.items[0].description, "../outside.ts");

        // Declaration must mirror the host's own parse (contract D): a
        // mismatched value is rejected, the matching one passes.
        assert!(matches!(
            data.search_file_mentions(&workspace_id, "@../out", Some(("absolute", "/nope"))),
            Err(HostDataError::InvalidMentionQuery)
        ));
        // Declaration mirrors the host parse: climb from the CANONICAL
        // registered root (macOS: /var → /private/var), forward slashes.
        let canonical_workspace = workspace
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let parent_declared = canonical_workspace
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_string())
            .unwrap_or_else(|| "/".to_string());
        assert!(data
            .search_file_mentions(
                &workspace_id,
                "@../out",
                Some(("absolute", parent_declared.as_str()))
            )
            .is_ok());

        // A missing declaration is fine for direct calls (host_server
        // enforces presence), but POSIX `@/` now resolves the fs root and
        // mid-word traversal inside the scope stays illegal.
        #[cfg(not(target_os = "windows"))]
        {
            let result = data
                .search_file_mentions(&workspace_id, "@/etc/host", None)
                .unwrap();
            assert!(
                result.items.iter().any(|item| item.value.contains("hosts")),
                "got {:?}",
                result.items
            );
        }

        // `@../..` multi-level is legal; mid-word `..` after the prefix is not.
        assert!(data
            .search_file_mentions(&workspace_id, "@../../x", None)
            .is_ok());
        assert!(matches!(
            data.search_file_mentions(&workspace_id, "@../ok/../bad", None),
            Err(HostDataError::InvalidMentionQuery)
        ));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn read_session_messages_returns_active_chain_with_entry_ids() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-msgs-{nonce}"));
        let workspace = temp.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let data = data.with_session_root(temp.join("sessions"));
        let bucket_dir = data
            .session_bucket_for_workspace(&workspace_id)
            .expect("persisted session bucket");
        fs::create_dir_all(&bucket_dir).unwrap();
        // Same branching fixture as the tree test: the file's last message
        // (a3) is the tip, so the transcript chain is u1→a1→u3→a3 — the u2
        // sibling branch is inactive and must not render in the transcript.
        let cwd = serde_json::to_string(&workspace.to_string_lossy()).unwrap();
        let lines: Vec<String> = vec![
            format!("{{\"type\":\"session\",\"id\":\"session-a\",\"timestamp\":\"2026-01-01\",\"cwd\":{cwd}}}"),
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"q1\"}}".into(),
            "{\"type\":\"message\",\"id\":\"tr1\",\"parentId\":\"u1\",\"message\":{\"role\":\"toolResult\",\"content\":[]}}".into(),
            "{\"type\":\"compaction\",\"id\":\"c1\",\"parentId\":\"tr1\"}".into(),
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"c1\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"ans\"}]}}".into(),
            "{\"type\":\"message\",\"id\":\"u2\",\"parentId\":\"a1\",\"message\":{\"role\":\"user\",\"content\":\"other branch\"}}".into(),
            "{\"type\":\"message\",\"id\":\"u3\",\"parentId\":\"a1\",\"message\":{\"role\":\"user\",\"content\":\"side\"}}".into(),
            "{\"type\":\"message\",\"id\":\"a3\",\"parentId\":\"u3\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"side-ans\"}]}}".into(),
        ];
        fs::write(
            bucket_dir.join("2026-01-01T00-00-00-000Z_session-a.jsonl"),
            lines.join("\n") + "\n",
        )
        .unwrap();

        let messages = data
            .read_session_messages(&workspace_id, "session-a", None)
            .unwrap();

        // Full tip chain including the toolResult (rendered as a tool card);
        // u2 (inactive sibling) and non-message entries (c1) never reach the
        // transcript.
        let roles: Vec<&str> = messages
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            vec!["user", "toolResult", "assistant", "user", "assistant"]
        );
        // Only user/assistant messages carry the stable entry id anchor.
        let ids: Vec<&str> = messages
            .iter()
            .filter(|message| message["entryId"].is_string())
            .map(|message| message["entryId"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["u1", "a1", "u3", "a3"]);

        fs::remove_dir_all(temp).unwrap();
    }

    /// Real-data probe: run the HOST read_session_tree against a real session
    /// file and report the derived leaf. Env-gated diagnostic.
    #[test]
    #[ignore = "real registry+session probe; set PICOT_SMOKE_DB/PICOT_SMOKE_WS/PICOT_SMOKE_SESSION"]
    fn read_session_tree_real_leaf_probe() {
        let (db, ws, session) = match (
            std::env::var("PICOT_SMOKE_DB"),
            std::env::var("PICOT_SMOKE_WS"),
            std::env::var("PICOT_SMOKE_SESSION"),
        ) {
            (Ok(db), Ok(ws), Ok(session)) => (db, ws, session),
            _ => return,
        };
        let store = Arc::new(Mutex::new(
            MetadataStore::open(std::path::Path::new(&db)).unwrap(),
        ));
        let data = HostDataPlane::new(store).with_session_root(std::path::PathBuf::from(
            std::env::var("HOME").unwrap() + "/.pi/agent/sessions",
        ));
        let tree = data.read_session_tree(&ws, &session).expect("tree");
        println!(
            "[tree-probe] entries={} messages={} leaf={:?} tree_bytes={}",
            tree.entries.len(),
            tree.entries
                .iter()
                .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("message"))
                .count(),
            tree.leaf_id,
            serde_json::to_string(&tree).unwrap_or_default().len()
        );
        let messages = data
            .read_session_messages(&ws, &session, None)
            .expect("messages");
        println!(
            "[tree-probe] transcript messages={} bytes={}",
            messages.len(),
            serde_json::to_string(&messages).unwrap_or_default().len()
        );
    }

    #[test]
    fn read_session_tree_returns_full_entries_and_derived_leaf() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-tree-{nonce}"));
        let workspace = temp.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let data = data.with_session_root(temp.join("sessions"));
        // The bucket is registry-persisted state: read it back from the DB
        // exactly as production does — never re-derive it from the path.
        let bucket_dir = data
            .session_bucket_for_workspace(&workspace_id)
            .expect("persisted session bucket");
        fs::create_dir_all(&bucket_dir).unwrap();
        // Branching session: active path u1→a1→u2; inactive sibling u3→a3;
        // hidden toolResult + compaction entries stay in the snapshot.
        let cwd = serde_json::to_string(&workspace.to_string_lossy()).unwrap();
        let lines: Vec<String> = vec![
            format!("{{\"type\":\"session\",\"id\":\"session-a\",\"timestamp\":\"2026-01-01\",\"cwd\":{cwd}}}"),
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"q1\"}}".into(),
            "{\"type\":\"message\",\"id\":\"tr1\",\"parentId\":\"u1\",\"message\":{\"role\":\"toolResult\",\"content\":[]}}".into(),
            "{\"type\":\"compaction\",\"id\":\"c1\",\"parentId\":\"tr1\"}".into(),
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"c1\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"ans\"}]}}".into(),
            "{\"type\":\"message\",\"id\":\"u2\",\"parentId\":\"a1\",\"message\":{\"role\":\"user\",\"content\":\"q2\"}}".into(),
            "{\"type\":\"message\",\"id\":\"u3\",\"parentId\":\"a1\",\"message\":{\"role\":\"user\",\"content\":\"side\"}}".into(),
            "{\"type\":\"message\",\"id\":\"a3\",\"parentId\":\"u3\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"side-ans\"}]}}".into(),
        ];
        fs::write(
            bucket_dir.join("2026-01-01T00-00-00-000Z_session-a.jsonl"),
            lines.join("\n") + "\n",
        )
        .unwrap();

        let tree = data.read_session_tree(&workspace_id, "session-a").unwrap();

        // Every id-carrying entry survives verbatim (hidden ones included).
        let ids: Vec<&str> = tree
            .entries
            .iter()
            .map(|entry| entry["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec!["session-a", "u1", "tr1", "c1", "a1", "u2", "u3", "a3"]
        );
        // Active leaf follows the last-message tip rule (same as the transcript):
        // the final message entry in the file heads the active branch.
        assert_eq!(tree.leaf_id.as_deref(), Some("a3"));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn delete_session_batch_trashes_protects_running_and_rejects_outside_paths() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-p4-delete-{nonce}"));
        let workspace = temp.join("workspace");
        let sessions = temp.join("sessions");
        let project = sessions.join("--workspace-project--");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&project).unwrap();
        let free = project.join("free.jsonl");
        let running = project.join("running.jsonl");
        fs::write(&free, "{\"type\":\"session\",\"id\":\"free\"}\n").unwrap();
        fs::write(&running, "{\"type\":\"session\",\"id\":\"running\"}\n").unwrap();
        let outside = temp.join("outside.jsonl");
        fs::write(&outside, "{}").unwrap();

        let (data, _workspace_id) = test_data(&workspace);
        let data = data.with_session_root(sessions.clone());

        let result = data
            .delete_session_batch(
                &[
                    free.to_string_lossy().into_owned(),
                    running.to_string_lossy().into_owned(),
                    outside.to_string_lossy().into_owned(),
                ],
                &[running.to_string_lossy().into_owned()],
            )
            .unwrap();

        assert_eq!(result["deleted"], 1);
        assert_eq!(result["running"].as_array().unwrap().len(), 1);
        assert_eq!(result["errors"].as_array().unwrap().len(), 1);
        assert!(!free.exists(), "free session must be removed from the tree");
        assert!(running.exists(), "running session must be protected");
        // Trash-first: the free file survives in the staging trash directory.
        let trash = sessions.parent().unwrap().join(".picot-session-trash");
        assert_eq!(fs::read_dir(&trash).unwrap().count(), 1);
    }

    #[test]
    fn session_export_registry_is_one_shot_quota_bound_and_revocable() {
        use super::SessionExportRegistry;
        use std::time::{Duration, Instant};

        let registry = SessionExportRegistry::new(1, Duration::from_secs(60));
        assert_eq!(registry.ttl_secs(), 60);
        let file = std::path::PathBuf::from("/tmp/session.jsonl");
        let (token, expires) = registry
            .issue("owner", 1, file.clone())
            .expect("first grant is issued");
        assert!(expires > Instant::now());
        assert_eq!(registry.outstanding_for("owner"), 1);
        // Quota is per-owner and counts only live grants.
        assert!(registry.issue("owner", 1, file.clone()).is_err());
        assert!(registry.issue("other", 1, file.clone()).is_ok());
        // One-shot: a redeemed token cannot be redeemed twice.
        assert_eq!(registry.redeem(&token, "owner", 1), Ok(file));
        assert!(registry.redeem(&token, "owner", 1).is_err());
        registry.revoke_owner("other");
        assert_eq!(registry.outstanding_for("other"), 0);

        let expired = SessionExportRegistry::new(4, Duration::ZERO);
        let (stale, _) = expired
            .issue("owner", 1, std::path::PathBuf::from("/tmp/session.jsonl"))
            .expect("grant is issued");
        assert!(
            expired.redeem(&stale, "owner", 1).is_err(),
            "zero-ttl grant is expired"
        );
    }

    #[test]
    fn persisted_bucket_reads_parent_and_subagent_sessions_as_tree_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let other_workspace = temp.path().join("other-workspace");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&other_workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let data = data.with_session_root(sessions.clone());
        let bucket = data
            .session_bucket_for_workspace(&workspace_id)
            .expect("persisted session bucket");
        fs::create_dir_all(&bucket).unwrap();
        let cwd = workspace.to_string_lossy();

        write_session_fixture(
            &bucket,
            "main.jsonl",
            session_fixture("main", &cwd, None, "main session"),
        );
        write_session_fixture(
            &bucket,
            "other-workspace.jsonl",
            session_fixture(
                "other-workspace",
                &other_workspace.to_string_lossy(),
                None,
                "must not leak",
            ),
        );
        write_session_fixture(
            &bucket,
            "human-fork.jsonl",
            session_fixture(
                "human-fork",
                &cwd,
                Some("/sessions/parent.jsonl"),
                "need refactor",
            ),
        );
        write_session_fixture(
            &bucket,
            "lineage-child.jsonl",
            session_fixture(
                "lineage-child",
                &cwd,
                Some("/sessions/parent.jsonl"),
                "need refactor",
            ) + &subagent_marker(),
        );
        let mut long_child = session_fixture(
            "fork-child",
            &cwd,
            Some("/sessions/parent.jsonl"),
            "need refactor",
        );
        for index in 0..200 {
            long_child.push_str(
                &serde_json::json!({
                    "type": "message",
                    "id": format!("copied-{index}"),
                    "message": { "role": "assistant", "content": "copied transcript" },
                })
                .to_string(),
            );
            long_child.push('\n');
        }
        long_child.push_str(&subagent_marker());
        long_child.push('\n');
        write_session_fixture(&bucket, "fork-child.jsonl", long_child);
        write_session_fixture(
            &bucket,
            "standalone-child.jsonl",
            session_fixture("standalone-child", &cwd, None, "need refactor") + &subagent_marker(),
        );

        let (_, cold_sessions, cold_count, cold_hidden) =
            data.read_workspace_session_bucket(&workspace_id, true);
        assert!(cold_sessions.is_empty());
        assert_eq!(cold_count, Some(6));
        assert_eq!(cold_hidden, None);

        let (_, sessions, visible_count, hidden_count) =
            data.read_workspace_session_bucket(&workspace_id, false);
        let mut ids = sessions
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![
                "fork-child",
                "human-fork",
                "lineage-child",
                "main",
                "standalone-child",
            ]
        );
        assert_eq!(visible_count, Some(5));
        assert_eq!(hidden_count, Some(0));
        let lineage_child = sessions
            .iter()
            .find(|row| row["id"] == "lineage-child")
            .expect("subagent session is projected");
        assert_eq!(lineage_child["parentSession"], "/sessions/parent.jsonl");

        let (_, cached_sessions, cached_count, cached_hidden) =
            data.read_workspace_session_bucket(&workspace_id, true);
        assert!(cached_sessions.is_empty());
        // Count-only reads always return the raw .jsonl entry count, warm or
        // cold: no session-count cache, no visibility classification
        // (ARCHITECTURE.md, sidebar session discovery).
        assert_eq!(cached_count, Some(6));
        assert_eq!(cached_hidden, None);

        let search_results = data.search_sessions(&workspace_id, "refactor").unwrap();
        assert_eq!(search_results.len(), 4);
        assert!(search_results
            .iter()
            .any(|result| result.session_id == "lineage-child"));

        let mut listed_ids = data
            .list_sessions(&workspace_id)
            .unwrap()
            .into_iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        listed_ids.sort();
        assert_eq!(
            listed_ids,
            vec![
                "fork-child",
                "human-fork",
                "lineage-child",
                "main",
                "standalone-child",
            ]
        );
    }

    #[test]
    fn appended_subagent_marker_does_not_hide_a_session() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let data = data.with_session_root(temp.path().join("sessions"));
        let bucket = data
            .session_bucket_for_workspace(&workspace_id)
            .expect("persisted session bucket");
        fs::create_dir_all(&bucket).unwrap();
        let file = bucket.join("grown.jsonl");
        write_session_fixture(
            &bucket,
            "grown.jsonl",
            session_fixture("grown", &workspace.to_string_lossy(), None, "main"),
        );

        let (_, _, first_count, first_hidden) =
            data.read_workspace_session_bucket(&workspace_id, false);
        assert_eq!(first_count, Some(1));
        assert_eq!(first_hidden, Some(0));
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(format!("{}\n", subagent_marker()).as_bytes())
            .unwrap();

        let (_, sessions, second_count, second_hidden) =
            data.read_workspace_session_bucket(&workspace_id, false);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["id"], "grown");
        assert_eq!(second_count, Some(1));
        assert_eq!(second_hidden, Some(0));
    }

    #[test]
    fn unresolved_sidebar_classifications_are_not_projected() {
        let classification = SidebarFileClassification {
            is_sidebar_visible: true,
            resolved: false,
            header: None,
        };
        assert!(!should_project_sidebar_file(&classification));
    }

    #[test]
    fn session_header_name_is_used_and_session_info_can_override_it() {
        let temp = tempfile::tempdir().unwrap();
        let header_path = temp.path().join("header-name.jsonl");
        let header = serde_json::json!({
            "type": "session",
            "id": "header-name",
            "timestamp": "2026-09-12T00:00:00.000Z",
            "cwd": "/workspace",
            "name": "[worker] Implement questionnaire renderer",
        });
        let message = serde_json::json!({
            "type": "message",
            "message": { "role": "user", "content": "hello" },
        });
        fs::write(&header_path, format!("{header}\n{message}\n")).unwrap();

        let scan = super::scan_session_sidebar_visibility(&header_path).unwrap();
        assert_eq!(
            scan.header.as_ref().and_then(|value| value.name.as_deref()),
            Some("[worker] Implement questionnaire renderer")
        );
        assert_eq!(
            super::parse_session_header(&header_path).and_then(|value| value.name),
            Some("[worker] Implement questionnaire renderer".to_string())
        );

        let renamed_path = temp.path().join("renamed.jsonl");
        fs::write(
            &renamed_path,
            format!("{header}\n{message}\n{{\"type\":\"session_info\",\"name\":\"Renamed\"}}\n"),
        )
        .unwrap();
        assert_eq!(
            super::parse_session_header(&renamed_path).and_then(|value| value.name),
            Some("Renamed".to_string())
        );
    }

    #[test]
    fn sidebar_scan_survives_trailing_invalid_utf8() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("early-stop.jsonl");
        let mut content = session_fixture("early-stop", "/workspace", None, "hello").into_bytes();
        content.extend_from_slice(
            b"{\"type\":\"message\",\"message\":{\"role\":\"assistant\"}}\n\xff\n",
        );
        fs::write(&path, content).unwrap();

        let scan = super::scan_session_sidebar_visibility(&path).unwrap();
        assert_eq!(
            scan.header.as_ref().map(|header| header.id.as_str()),
            Some("early-stop")
        );
    }

    #[test]
    fn persisted_bucket_reads_once_and_missing_directory_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let data = data.with_session_root(sessions.clone());
        let bucket_name = data
            .session_bucket_for_workspace(&workspace_id)
            .and_then(|bucket| {
                bucket
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap();
        assert_eq!(
            data.read_workspace_session_bucket(&workspace_id, true),
            (Some(bucket_name.clone()), Vec::new(), Some(0), Some(0)),
        );

        let bucket = sessions.join(&bucket_name);
        fs::create_dir_all(&bucket).unwrap();
        fs::write(
            bucket.join("saved.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"saved\",\"cwd\":{}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n",
                serde_json::to_string(&workspace.canonicalize().unwrap().to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        let (dir_name, sessions, count, hidden_count) =
            data.read_workspace_session_bucket(&workspace_id, false);
        assert_eq!(dir_name, Some(bucket_name));
        assert_eq!(count, Some(1));
        assert_eq!(hidden_count, Some(0));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["id"], "saved");
    }

    #[test]
    fn session_file_path_accepts_verified_pi_filename() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let root = workspace.canonicalize().unwrap();
        let bucket_name = "--test-bucket--";
        let bucket = sessions.join(bucket_name);
        fs::create_dir_all(&bucket).unwrap();
        let file = bucket.join("2026-09-03T12-00-00-000Z_saved.jsonl");
        fs::write(
            &file,
            format!(
                "{{\"type\":\"session\",\"id\":\"saved\",\"cwd\":{}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n",
                serde_json::to_string(&root.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        let data = data.with_session_root(sessions);

        assert_eq!(
            data.session_file_path_by_path(&workspace_id, "saved", &file.to_string_lossy()),
            file.canonicalize().ok(),
        );
    }

    #[test]
    fn session_file_path_finds_pi_filename_by_header_id() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let root = workspace.canonicalize().unwrap();
        let bucket_name = "--test-bucket--";
        let bucket = sessions.join(bucket_name);
        fs::create_dir_all(&bucket).unwrap();
        let file = bucket.join("2026-09-03T12-00-00-000Z_saved.jsonl");
        fs::write(
            &file,
            format!(
                "{{\"type\":\"session\",\"id\":\"saved\",\"cwd\":{}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n",
                serde_json::to_string(&root.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        let data = data.with_session_root(sessions);

        assert_eq!(
            data.session_file_path(&workspace_id, "saved"),
            file.canonicalize().ok(),
        );
    }

    #[test]
    fn session_file_path_rejects_traversal_and_outside_bucket() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let root = workspace.canonicalize().unwrap();
        let bucket_name = "--test-bucket--";
        let bucket = sessions.join(bucket_name);
        fs::create_dir_all(&bucket).unwrap();
        fs::write(
            bucket.join("safe.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"safe\",\"cwd\":{}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n",
                serde_json::to_string(&root.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        let data = data.with_session_root(sessions);
        let resolved = data.session_file_path(&workspace_id, "safe");
        assert!(
            resolved.is_some(),
            "root={root:?} bucket={bucket:?} resolved={resolved:?}"
        );
        for invalid in ["../outside", "nested/name", r"nested\\name", "..secret"] {
            assert!(
                data.session_file_path(&workspace_id, invalid).is_none(),
                "{invalid}"
            );
        }
    }

    /// History reads must consume a caller-supplied path hint (the sidebar's
    /// scanned `filePath`) instead of rescanning the bucket by session id.
    /// The hint is placed one level below the bucket, which the id scan never
    /// reads (it is non-recursive): a successful read therefore proves the
    /// validated hint — not the scan — resolved the file.
    #[test]
    fn read_session_messages_prefers_verified_path_hint_over_bucket_rescan() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let root = workspace.canonicalize().unwrap();
        let bucket = sessions.join("--test-bucket--");
        let nested = bucket.join("nested").join("saved.jsonl");
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        fs::write(
            &nested,
            format!(
                "{{\"type\":\"session\",\"id\":\"saved\",\"cwd\":{}}}\n{{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n",
                serde_json::to_string(&root.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        let data = data.with_session_root(sessions);

        assert!(
            data.session_file_path(&workspace_id, "saved").is_none(),
            "the id rescan must not discover the nested hint file"
        );
        let messages = data
            .read_session_messages(&workspace_id, "saved", Some(&nested.to_string_lossy()))
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "hello");
    }

    /// An untrusted hint (wrong header id, or a path outside the workspace's
    /// bucket) must never fail the read: the host ignores it and falls back to
    /// its own id lookup, so authorization and correctness are unchanged.
    #[test]
    fn read_session_messages_ignores_untrusted_path_hint() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&workspace).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let root = workspace.canonicalize().unwrap();
        let bucket = sessions.join("--test-bucket--");
        fs::create_dir_all(&bucket).unwrap();
        let cwd = serde_json::to_string(&root.to_string_lossy()).unwrap();
        fs::write(
            bucket.join("saved.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"saved\",\"cwd\":{cwd}}}\n{{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{{\"role\":\"user\",\"content\":\"real\"}}}}\n"
            ),
        )
        .unwrap();
        // A file with the wrong header id, inside the bucket.
        fs::write(
            bucket.join("other.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"other\",\"cwd\":{cwd}}}\n{{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{{\"role\":\"user\",\"content\":\"decoy\"}}}}\n"
            ),
        )
        .unwrap();
        // A file outside the bucket that does carry the requested id.
        let outside = temp.path().join("outside.jsonl");
        fs::write(
            &outside,
            format!(
                "{{\"type\":\"session\",\"id\":\"saved\",\"cwd\":{cwd}}}\n{{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{{\"role\":\"user\",\"content\":\"outside\"}}}}\n"
            ),
        )
        .unwrap();
        let data = data.with_session_root(sessions);

        for hint in [
            bucket.join("other.jsonl").to_string_lossy().into_owned(),
            outside.to_string_lossy().into_owned(),
            "/etc/passwd".to_string(),
        ] {
            let messages = data
                .read_session_messages(&workspace_id, "saved", Some(&hint))
                .unwrap_or_else(|error| panic!("hint {hint} must fall back: {error:?}"));
            assert_eq!(messages.len(), 1, "hint {hint}");
            assert_eq!(messages[0]["content"], "real", "hint {hint}");
        }
    }

    /// A retained runtime session file only needs root containment and a
    /// `.jsonl` shape: temporary runtimes never match a header id, and the
    /// value feeds display summaries, never authorization.
    #[test]
    fn canonical_session_file_requires_root_containment_and_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&workspace).unwrap();
        let (data, _) = test_data(&workspace);
        let data = data.with_session_root(sessions.clone());
        fs::create_dir_all(&sessions).unwrap();
        let good = sessions.join("ok.jsonl");
        fs::write(&good, "{\"type\":\"session\",\"id\":\"a\"}\n").unwrap();
        let not_jsonl = sessions.join("notes.txt");
        fs::write(&not_jsonl, "x").unwrap();
        let outside = temp.path().join("outside.jsonl");
        fs::write(&outside, "{\"type\":\"session\",\"id\":\"a\"}\n").unwrap();

        assert_eq!(
            data.canonical_session_file(&good.to_string_lossy()),
            good.canonicalize().ok()
        );
        assert!(data
            .canonical_session_file(&not_jsonl.to_string_lossy())
            .is_none());
        assert!(data
            .canonical_session_file(&outside.to_string_lossy())
            .is_none());
        assert!(data.canonical_session_file("/etc/passwd").is_none());
        assert!(data.canonical_session_file("").is_none());
    }

    #[test]
    fn record_pi_session_bucket_uses_pi_authority() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let db = temp.path().join("metadata.sqlite3");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let metadata = Arc::new(Mutex::new(MetadataStore::open(&db).unwrap()));
        let (row, _) = metadata.lock().unwrap().add_workspace(&workspace).unwrap();
        let data = HostDataPlane::new(metadata).with_session_root(sessions);

        assert!(data
            .record_pi_session_bucket(
                &row.workspace_id,
                "/home/user/.pi/agent/sessions/--pi-returned-bucket--/chat.jsonl",
            )
            .unwrap());
        assert!(data
            .record_pi_session_bucket(
                &row.workspace_id,
                "/home/user/.pi/agent/sessions/--another-bucket--/chat.jsonl",
            )
            .unwrap());
        assert_eq!(
            data.session_bucket_for_workspace(&row.workspace_id)
                .and_then(|path| path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())),
            Some("--another-bucket--".to_string())
        );
    }

    #[test]
    fn append_session_info_name_appends_a_valid_record() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let session_file = std::env::temp_dir().join(format!("picot-p4-rename-{nonce}.jsonl"));
        fs::write(&session_file, "{\"type\":\"session\",\"id\":\"a\"}\n").unwrap();

        let (data, workspace_id) = test_data(std::path::Path::new("/"));
        let _ = workspace_id;
        data.append_session_info_name(&session_file, "Renamed")
            .unwrap();

        let content = fs::read_to_string(&session_file).unwrap();
        let last = content.lines().last().unwrap();
        let record: serde_json::Value = serde_json::from_str(last).unwrap();
        assert_eq!(record["type"], "session_info");
        assert_eq!(record["name"], "Renamed");
        let _ = fs::remove_file(&session_file);
    }

    /// A rename record must be a real tree entry: Pi resolves the leaf from the
    /// file's newest entry, so a bare `{type,name}` record (the pre-2026-09-24
    /// shape) leaves the tip id-less and Pi's next resume appends a fresh root
    /// instead — orphaning the whole prior lineage from the transcript.
    #[test]
    fn append_session_info_name_links_the_new_record_to_the_current_tip() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let session_file = std::env::temp_dir().join(format!("picot-rename-tip-{nonce}.jsonl"));
        fs::write(
            &session_file,
            concat!(
                "{\"type\":\"session\",\"id\":\"hdr\"}\n",
                "{\"type\":\"model_change\",\"id\":\"aaaa1111\",\"parentId\":null,\"timestamp\":\"2026-09-11T09:11:33.397Z\"}\n",
                "{\"type\":\"message\",\"id\":\"bbbb2222\",\"parentId\":\"aaaa1111\",\"timestamp\":\"2026-09-11T09:14:48.686Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            ),
        )
        .unwrap();

        let (data, _) = test_data(std::path::Path::new("/"));
        data.append_session_info_name(&session_file, "Renamed")
            .unwrap();

        let content = fs::read_to_string(&session_file).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(content.lines().last().unwrap()).unwrap();
        assert_eq!(record["type"], "session_info");
        assert_eq!(record["name"], "Renamed");
        let id = record["id"].as_str().expect("entry id");
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(record["parentId"], "bbbb2222");
        assert!(record["timestamp"]
            .as_str()
            .is_some_and(|timestamp| timestamp.ends_with('Z')));
        let _ = fs::remove_file(&session_file);
    }

    /// A session file whose entries carry no ids (header only) has no tip to
    /// link to: the record is still well-formed, rooted like Pi's first entry.
    #[test]
    fn append_session_info_name_roots_a_record_when_no_tip_exists() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let session_file = std::env::temp_dir().join(format!("picot-rename-root-{nonce}.jsonl"));
        fs::write(&session_file, "{\"type\":\"session\",\"id\":\"hdr\"}\n").unwrap();

        let (data, _) = test_data(std::path::Path::new("/"));
        data.append_session_info_name(&session_file, "Renamed")
            .unwrap();

        let content = fs::read_to_string(&session_file).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(content.lines().last().unwrap()).unwrap();
        assert!(record["parentId"].is_null());
        assert!(record["id"].as_str().is_some());
        let _ = fs::remove_file(&session_file);
    }

    #[test]
    fn lists_registered_workspace_files_and_rejects_escape_paths() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-data-{nonce}"));
        let workspace = temp.join("workspace");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("README.md"), "read me").unwrap();
        fs::write(temp.join("secret.txt"), "secret").unwrap();
        let (data, workspace_id) = test_data(&workspace);

        let entries = data.list_files(&workspace_id, "").unwrap();
        assert_eq!(entries[0].name, "src");
        assert_eq!(entries[0].kind, FileKind::Directory);
        assert_eq!(entries[1].relative_path, "README.md");
        assert_eq!(
            data.list_files(&workspace_id, "../"),
            Err(HostDataError::InvalidRelativePath)
        );
        assert_eq!(
            data.list_files("missing", ""),
            Err(HostDataError::UnknownWorkspace)
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_resolve_outside_the_workspace() {
        use std::os::unix::fs::symlink;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-data-link-{nonce}"));
        let workspace = temp.join("workspace");
        let outside = temp.join("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, workspace.join("escape")).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        assert_eq!(
            data.list_files(&workspace_id, "escape"),
            Err(HostDataError::OutsideWorkspace)
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn rename_appended_beyond_the_summary_window_is_visible_in_both_scans() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-rename-{nonce}"));
        let workspace = temp.join("workspace");
        let sessions = temp.join("sessions/--test-bucket--");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        let mut content = format!(
            "{{\"type\":\"session\",\"id\":\"renamed\",\"timestamp\":\"2026-01-01\",\"cwd\":{}}}\n",
            serde_json::to_string(&workspace.to_string_lossy()).unwrap()
        );
        content.push_str(
            "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"first user message\"}}\n",
        );
        for index in 0..300 {
            content.push_str(&format!(
                "{{\"type\":\"message\",\"id\":\"m{index}\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"padding {index}\"}}]}}}}\n"
            ));
        }
        content.push_str(
            "{\"type\":\"session_info\",\"id\":\"rr1\",\"parentId\":\"m299\",\"name\":\"Renamed late\"}\n",
        );
        fs::write(sessions.join("renamed.jsonl"), content).unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let data = data.with_session_root(temp.join("sessions"));

        let (bucket_name, bucket_sessions, _, _) =
            data.read_workspace_session_bucket(&workspace_id, false);
        assert_eq!(
            bucket_sessions
                .first()
                .and_then(|value| value.get("name"))
                .and_then(serde_json::Value::as_str),
            Some("Renamed late"),
            "sidebar bucket scan must see the end-of-file rename (bucket={bucket_name:?})"
        );

        let listed = data.list_sessions(&workspace_id).unwrap();
        assert_eq!(
            listed.first().and_then(|summary| summary.name.as_deref()),
            Some("Renamed late"),
            "list_sessions summary must see the end-of-file rename"
        );
    }

    #[test]
    fn repeated_renames_resolve_to_the_latest_session_info() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("multi-rename.jsonl");
        let mut content = String::from(
            "{\"type\":\"session\",\"id\":\"multi\",\"timestamp\":\"2026-01-01\",\"cwd\":\"/workspace\"}\n\
             {\"type\":\"session_info\",\"name\":\"first name\"}\n\
             {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        );
        content.push_str(&"{}\n".repeat(200));
        content.push_str("{\"type\":\"session_info\",\"name\":\"latest name\"}\n");
        fs::write(&path, content).unwrap();

        assert_eq!(
            super::scan_session_sidebar_visibility(&path)
                .unwrap()
                .header
                .and_then(|header| header.name),
            Some("latest name".to_owned())
        );
        assert_eq!(
            super::parse_session_header(&path).and_then(|header| header.name),
            Some("latest name".to_owned())
        );

        // Pi semantics: the latest session_info with an empty name clears it.
        fs::write(
            &path,
            fs::read_to_string(&path).unwrap() + "{\"type\":\"session_info\",\"name\":\"   \"}\n",
        )
        .unwrap();
        assert_eq!(
            super::scan_session_sidebar_visibility(&path)
                .unwrap()
                .header
                .and_then(|header| header.name),
            None
        );
    }

    #[test]
    fn lists_only_sessions_owned_by_the_registered_workspace_and_skips_unknown_entries() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-sessions-{nonce}"));
        let workspace = temp.join("workspace");
        let other = temp.join("other");
        let sessions = temp.join("sessions/--test-bucket--");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("included.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"session-a\",\"timestamp\":\"2026-01-01\",\"cwd\":{}}}\n{{\"type\":\"future_entry\",\"payload\":true}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hello from session\"}}}}\n",
                serde_json::to_string(&workspace.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        fs::write(
            sessions.join("excluded.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"session-b\",\"cwd\":{}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"private\"}}}}\n",
                serde_json::to_string(&other.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let data = data.with_session_root(temp.join("sessions"));

        let listed = data.list_sessions(&workspace_id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "session-a");
        assert_eq!(
            listed[0].first_message.as_deref(),
            Some("hello from session")
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn searches_only_the_registered_workspace_and_returns_snippets() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-search-{nonce}"));
        let workspace = temp.join("workspace");
        let other = temp.join("other");
        let sessions = temp.join("sessions/--test-bucket--");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("included.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"session-a\",\"timestamp\":\"2026-01-01\",\"cwd\":{}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"please refactor the widget factory\"}}}}\n",
                serde_json::to_string(&workspace.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        fs::write(
            sessions.join("excluded.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"session-b\",\"cwd\":{}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"refactor this too\"}}}}\n",
                serde_json::to_string(&other.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        let (data, workspace_id) = test_data(&workspace);
        let data = data.with_session_root(temp.join("sessions"));

        let results = data.search_sessions(&workspace_id, "widget").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "session-a");
        assert!(results[0].matches[0].snippet.contains("widget"));

        assert!(
            data.search_sessions(&workspace_id, "refactor")
                .unwrap()
                .len()
                == 1
        );
        assert!(data.search_sessions("missing", "widget").is_err());
        fs::remove_dir_all(temp).unwrap();
    }
}
