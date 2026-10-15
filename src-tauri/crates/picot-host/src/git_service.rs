// ABOUTME: Executes owner-scoped Git status and diff reads against host-derived workspaces.
// ABOUTME: Snapshot records bind raw paths to a generation so browser data cannot authorize Git access.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const MAX_STATUS_ENTRIES: usize = 1_200;
static NEXT_SNAPSHOT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
const SNAPSHOT_TTL: Duration = Duration::from_secs(300);
const MAX_SNAPSHOTS_PER_OWNER: usize = 8;
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const COMMIT_DEADLINE: Duration = Duration::from_secs(300);
// Push crosses the network, so it needs far more headroom than the 30s write
// deadline — but still a hard ceiling, because a credential prompt this
// process cannot answer would otherwise stall the write slot forever.
const PUSH_DEADLINE: Duration = Duration::from_secs(120);
const MAX_PUSH_OUTPUT_BYTES: usize = 4 * 1024;
pub const PUSH_DETACHED_HEAD: &str = "push_detached_head";
pub const PUSH_NO_REMOTE: &str = "push_no_remote";
const OUTCOME_TTL: Duration = Duration::from_secs(600);
#[allow(dead_code)]
const MAX_STDOUT_BYTES: usize = 4 * 1024 * 1024;
#[allow(dead_code)]
const MAX_STDERR_BYTES: usize = 64 * 1024;
#[allow(dead_code)]
const GIT_READ_DEADLINE: Duration = Duration::from_secs(10);
#[allow(dead_code)]
const GIT_WRITE_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusSnapshot {
    pub snapshot_id: String,
    pub head_state: String,
    pub head_oid: Option<String>,
    pub index_tree_oid: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub change_stats: GitChangeStats,
    pub counts: GitCounts,
    pub entries: Vec<GitStatusEntry>,
    pub returned_entry_count: usize,
    pub total_entry_count: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitCounts {
    pub staged: usize,
    pub changes: usize,
    pub untracked: usize,
    pub conflicted: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitChangeStats {
    pub basis: String,
    pub additions: u64,
    pub deletions: u64,
    pub untracked_excluded_count: usize,
    pub binary_file_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusEntry {
    pub record_type: String,
    pub xy: Option<String>,
    pub entry_kind: String,
    pub display_path: String,
    pub path_bytes_base64: String,
    pub original_display_path: Option<String>,
    pub original_path_bytes_base64: Option<String>,
    pub submodule: Option<String>,
    /// Stage 1/2/3 modes for an unmerged porcelain-v2 `u` record.
    pub unmerged_modes: Option<Vec<String>>,
    /// Stage 1/2/3 object IDs for an unmerged porcelain-v2 `u` record.
    pub unmerged_object_ids: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffResponse {
    pub snapshot_id: String,
    pub comparison: String,
    pub path_bytes_base64: String,
    pub raw_patch: String,
    pub truncated: bool,
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitPushOutcome {
    pub remote: String,
    pub branch: String,
    pub set_upstream: bool,
    /// Bounded stderr transcript; git push writes its human-readable result
    /// there even on success.
    pub output: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitDiffResponse {
    pub comparison: String,
    pub path_bytes_base64: String,
    pub raw_patch: String,
    pub truncated: bool,
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitNameStatusFile {
    pub status: String,
    pub path: String,
    pub path_bytes: Vec<u8>,
    pub original_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitLogEntry {
    pub oid: String,
    pub subject: String,
    pub author_name: String,
    pub author_time: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitLogResponse {
    pub commits: Vec<GitLogEntry>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitLogDetailFile {
    pub status: String,
    pub path: String,
    pub path_bytes_base64: String,
    pub original_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitLogDetailResponse {
    pub oid: String,
    pub author_name: String,
    pub author_time: i64,
    pub full_message: String,
    pub message_truncated: bool,
    pub files_truncated: bool,
    pub files: Vec<GitLogDetailFile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitKind {
    Root,
    Linear,
    Merge,
}

pub const MAX_LOG_LIMIT: usize = 200;
pub const MAX_LOG_FILES: usize = 2_000;
pub const MAX_LOG_MESSAGE_BYTES: usize = 32 * 1024;
pub const MAX_LOG_FILES_BYTES: usize = 256 * 1024;
const LOG_LIST_FMT: &str = "%H%x00%h%x00%s%x00%an%x00%at";
const LOG_DETAIL_FMT: &str = "%H%x00%an%x00%at%x00%B";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitAiSnapshot {
    pub snapshot_id: String,
    pub head_state: String,
    pub head_oid: Option<String>,
    pub index_tree_oid: Option<String>,
    pub staged_diff: String,
    pub staged_diff_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedStatus {
    pub entries: Vec<GitStatusEntry>,
    pub counts: GitCounts,
    pub total_entry_count: usize,
    pub truncated: bool,
    pub head_state: String,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPathIdentity {
    pub group: String,
    pub path_bytes: Vec<u8>,
    pub original_path_bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
struct SnapshotRecord {
    owner: String,
    root: PathBuf,
    generation: u64,
    created: Instant,
    entries: Vec<GitStatusEntry>,
    head_state: String,
    head_oid: Option<String>,
    index_tree_oid: Option<String>,
    partial_stage_token: Option<String>,
    partial_stage_token_created: Option<Instant>,
}

#[derive(Clone, Default)]
pub struct GitService {
    snapshots: Arc<Mutex<HashMap<String, SnapshotRecord>>>,
    /// Per-canonical-root write locks. Each canonical workspace gets its own
    /// lock so writes to different workspaces run in parallel; writes to the
    /// same workspace are serialized. Idle slots are pruned when no snapshot,
    /// outcome, or running commit references the root.
    write_slots: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>,
    outcomes: Arc<Mutex<HashMap<String, PendingGitOutcome>>>,
    /// Owners whose workspace identity has been revoked (window destroy /
    /// owner revoke). record_outcome refuses to store outcomes for revoked
    /// owners so a detached commit that completes after revoke cannot leave a
    /// residue no client will ever consume.
    revoked_owners: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl GitService {
    /// Acquire the write lock for a canonical root. Returns the `Arc` to the
    /// per-root mutex; the caller must `lock()` it and hold both the `Arc` and
    /// the guard in the same scope. The registry is bounded: when it exceeds
    /// MAX_WRITE_SLOTS the entry whose `Arc` has the fewest strong references
    /// (i.e. no active write holds it) is evicted.
    fn lock_for_write(&self, root: &Path) -> Arc<Mutex<()>> {
        const MAX_WRITE_SLOTS: usize = 64;
        let mut slots = self.write_slots.lock().unwrap();
        if slots.len() > MAX_WRITE_SLOTS {
            // Evict the idle slot with the fewest strong references. A slot
            // with >1 strong ref is currently held by a running write/commit
            // and must not be evicted.
            if let Some(victim) = slots
                .iter()
                .filter(|(_, arc)| Arc::strong_count(arc) <= 1)
                .min_by_key(|(path, _)| path.to_string_lossy().len())
                .map(|(path, _)| path.clone())
            {
                slots.remove(&victim);
            }
        }
        slots
            .entry(root.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

/// Keyed by owner so outcomes survive the originating client disconnecting.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingGitOutcome {
    pub owner: String,
    pub root: String,
    pub generation: u64,
    pub request_id: String,
    pub status: String,
    pub commit_oid: Option<String>,
    pub hook_changed_tree: bool,
    pub error: Option<String>,
    #[serde(skip)]
    pub created: Option<Instant>,
}

fn record_value<'a>(record: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    record.strip_prefix(prefix)
}
fn display_path(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}
fn path_field(record: &[u8]) -> &[u8] {
    record.rsplit(|b| *b == b' ').next().unwrap_or_default()
}
fn header_fields(record: &[u8]) -> Vec<&[u8]> {
    record.splitn(10, |b| *b == b' ').collect()
}
fn unmerged_fields(record: &[u8]) -> Vec<&[u8]> {
    // `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`.
    // Keep the path separate from h3; `header_fields` deliberately leaves the
    // final path attached for ordinary and rename records.
    record.splitn(11, |b| *b == b' ').collect()
}
fn submodule_field(record: &[u8]) -> Option<String> {
    // In porcelain v2, a modified submodule (gitlink) has `S` as the
    // normalized mode field (field index 2) instead of `N`.
    header_fields(record)
        .get(2)
        .filter(|value| value.starts_with(b"S"))
        .map(|value| display_path(value))
}
fn rename_kind(record: &[u8]) -> &'static str {
    match header_fields(record).get(8).and_then(|value| value.first()) {
        Some(b'C') => "copy",
        _ => "rename",
    }
}
fn xy(record: &[u8]) -> Option<String> {
    record
        .get(2..4)
        .map(|v| String::from_utf8_lossy(v).to_string())
}

/// Resolve the canonical git top-level for `root` and verify it matches the
/// host-derived workspace root. Git's repository discovery walks up from the
/// current directory, so when the workspace is a subdirectory of a parent repo
/// (`repo/sub`), every git subprocess would otherwise operate on the parent —
/// surfacing the parent's staged files and even committing them on behalf of a
/// workspace that never authorized that scope. Canonicalize both sides before
/// comparing so a trailing slash or symlink difference cannot mask a mismatch.
fn assert_workspace_root(root: &Path) -> Result<(), String> {
    let output = git(root, &["rev-parse", "--show-toplevel"])?;
    let toplevel_str = String::from_utf8_lossy(&output).trim().to_string();
    if toplevel_str.is_empty() {
        return Err("not a git repository".into());
    }
    let host = fs::canonicalize(root).map_err(|e| format!("invalid workspace root: {e}"))?;
    let resolved =
        fs::canonicalize(&toplevel_str).map_err(|e| format!("invalid git toplevel: {e}"))?;
    if host != resolved {
        return Err(format!(
            "workspace root is nested inside another git repository ({} != {})",
            host.display(),
            resolved.display()
        ));
    }
    Ok(())
}
fn changed(value: Option<&String>, index: usize) -> bool {
    value
        .and_then(|v| v.as_bytes().get(index))
        .is_some_and(|b| *b != b'.')
}
fn numstat(bytes: &[u8]) -> (u64, u64, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    let mut binary = 0;
    for line in bytes.split(|b| *b == b'\n') {
        let fields = line.splitn(3, |b| *b == b'\t').collect::<Vec<_>>();
        if fields.len() < 3 {
            continue;
        }
        if fields[0] == b"-" || fields[1] == b"-" {
            binary += 1;
            continue;
        }
        additions += std::str::from_utf8(fields[0])
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        deletions += std::str::from_utf8(fields[1])
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
    }
    (additions, deletions, binary)
}
fn bounded_ai_diff(bytes: &[u8]) -> (String, bool) {
    const MAX_CHARS: usize = 24_000;
    const MAX_FILES: usize = 80;
    const MAX_LINES_PER_FILE: usize = 80;
    let source = String::from_utf8_lossy(bytes);
    let mut output = String::new();
    let mut files = 0;
    let mut lines_in_file = 0;
    let mut truncated = false;
    for line in source.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            files += 1;
            lines_in_file = 0;
        }
        if files > MAX_FILES || lines_in_file >= MAX_LINES_PER_FILE {
            truncated = true;
            continue;
        }
        if output.chars().count() + line.chars().count() > MAX_CHARS {
            truncated = true;
            break;
        }
        output.push_str(line);
        lines_in_file += 1;
    }
    (output, truncated)
}
fn change_stats(root: &Path, head_state: &str, untracked: usize) -> GitChangeStats {
    let args = if head_state == "unborn" {
        vec!["--literal-pathspecs", "diff", "--cached", "--numstat", "--"]
    } else {
        vec!["--literal-pathspecs", "diff", "HEAD", "--numstat", "--"]
    };
    let (additions, deletions, binary_file_count) = git(root, &args)
        .map(|output| numstat(&output))
        .unwrap_or_default();
    GitChangeStats {
        basis: if head_state == "unborn" {
            "empty-tree-to-worktree"
        } else {
            "head-to-worktree"
        }
        .into(),
        additions,
        deletions,
        untracked_excluded_count: untracked,
        binary_file_count,
    }
}

/// Parses `git diff-tree --name-status -r -z` and `git diff --name-status -r -z`.
/// Empty NUL fields are separators between records; path bytes remain opaque.
pub fn parse_diff_tree_name_status_z(bytes: &[u8]) -> Vec<GitNameStatusFile> {
    let tokens: Vec<&[u8]> = bytes
        .split(|b| *b == 0)
        .filter(|token| !token.is_empty())
        .collect();
    let mut files = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        let is_status = matches!(token, b"M" | b"A" | b"D" | b"T" | b"U" | b"X" | b"B")
            || matches!(token.first(), Some(b'R' | b'C'))
                && token.len() > 1
                && token[1..].iter().all(u8::is_ascii_digit);
        if !is_status {
            index += 1;
            continue;
        }
        let is_rename_or_copy = matches!(token.first(), Some(b'R' | b'C'));
        let path_count = if is_rename_or_copy { 2 } else { 1 };
        if index + path_count >= tokens.len() {
            index += 1;
            continue;
        }
        let original = if is_rename_or_copy {
            Some(tokens[index + 1].to_vec())
        } else {
            None
        };
        let path = tokens[index + path_count].to_vec();
        files.push(GitNameStatusFile {
            status: String::from_utf8_lossy(token).into_owned(),
            path: String::from_utf8_lossy(&path).into_owned(),
            path_bytes: path,
            original_path: original
                .as_deref()
                .map(|value| String::from_utf8_lossy(value).into_owned()),
        });
        index += path_count + 1;
    }
    files
}

/// Parses `git status --porcelain=v2 -z`; NUL, not whitespace or newline, delimits records.
pub fn parse_porcelain_v2_z(bytes: &[u8]) -> ParsedStatus {
    let records: Vec<&[u8]> = bytes.split(|b| *b == 0).filter(|r| !r.is_empty()).collect();
    let mut entries = Vec::new();
    let mut counts = GitCounts::default();
    let mut total_entry_count = 0;
    let mut index = 0;
    let mut head_state = "unborn".to_string();
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = None;
    let mut behind = None;
    while index < records.len() {
        if let Some(value) = record_value(records[index], b"# branch.oid ") {
            head_state = if value == b"(initial)" {
                "unborn"
            } else {
                "attached"
            }
            .into();
            index += 1;
            continue;
        }
        if let Some(value) = record_value(records[index], b"# branch.head ") {
            if value == b"(detached)" {
                head_state = "detached".into();
            } else if !value.is_empty() {
                branch = Some(display_path(value));
            }
            index += 1;
            continue;
        }
        if let Some(value) = record_value(records[index], b"# branch.upstream ") {
            upstream = Some(display_path(value));
            index += 1;
            continue;
        }
        if let Some(value) = record_value(records[index], b"# branch.ab ") {
            let fields = value.split(|b| *b == b' ').collect::<Vec<_>>();
            ahead = fields
                .first()
                .and_then(|v| v.strip_prefix(b"+"))
                .and_then(|v| std::str::from_utf8(v).ok())
                .and_then(|v| v.parse().ok());
            behind = fields
                .get(1)
                .and_then(|v| v.strip_prefix(b"-"))
                .and_then(|v| std::str::from_utf8(v).ok())
                .and_then(|v| v.parse().ok());
            index += 1;
            continue;
        }
        let record = records[index];
        let kind = record.first().copied().unwrap_or_default();
        let (entry, staged, worktree) = match kind {
            b'?' | b'!' => {
                let path = record.get(2..).unwrap_or_default();
                (
                    GitStatusEntry {
                        record_type: (kind as char).to_string(),
                        xy: None,
                        entry_kind: if kind == b'?' { "untracked" } else { "ignored" }.into(),
                        display_path: display_path(path),
                        path_bytes_base64: BASE64.encode(path),
                        original_display_path: None,
                        original_path_bytes_base64: None,
                        submodule: None,
                        unmerged_modes: None,
                        unmerged_object_ids: None,
                    },
                    false,
                    false,
                )
            }
            b'1' => {
                let state = xy(record);
                let path = path_field(record);
                (
                    GitStatusEntry {
                        record_type: "1".into(),
                        xy: state.clone(),
                        entry_kind: "ordinary".into(),
                        display_path: display_path(path),
                        path_bytes_base64: BASE64.encode(path),
                        original_display_path: None,
                        original_path_bytes_base64: None,
                        submodule: submodule_field(record),
                        unmerged_modes: None,
                        unmerged_object_ids: None,
                    },
                    changed(state.as_ref(), 0),
                    changed(state.as_ref(), 1),
                )
            }
            b'2' => {
                let state = xy(record);
                let path = path_field(record);
                let original = records.get(index + 1).copied().unwrap_or_default();
                index += 1;
                (
                    GitStatusEntry {
                        record_type: "2".into(),
                        xy: state,
                        entry_kind: rename_kind(record).into(),
                        display_path: display_path(path),
                        path_bytes_base64: BASE64.encode(path),
                        original_display_path: Some(display_path(original)),
                        original_path_bytes_base64: Some(BASE64.encode(original)),
                        submodule: submodule_field(record),
                        unmerged_modes: None,
                        unmerged_object_ids: None,
                    },
                    true,
                    true,
                )
            }
            b'u' => {
                let fields = unmerged_fields(record);
                let path = fields.get(10).copied().unwrap_or_default();
                (
                    GitStatusEntry {
                        record_type: "u".into(),
                        xy: xy(record),
                        entry_kind: "unmerged".into(),
                        display_path: display_path(path),
                        path_bytes_base64: BASE64.encode(path),
                        original_display_path: None,
                        original_path_bytes_base64: None,
                        submodule: submodule_field(record),
                        unmerged_modes: Some(
                            fields
                                .iter()
                                .skip(3)
                                .take(3)
                                .map(|value| display_path(value))
                                .collect(),
                        ),
                        unmerged_object_ids: Some(
                            fields
                                .iter()
                                .skip(7)
                                .take(3)
                                .map(|value| display_path(value))
                                .collect(),
                        ),
                    },
                    false,
                    false,
                )
            }
            _ => {
                index += 1;
                continue;
            }
        };
        total_entry_count += 1;
        counts.staged += staged as usize;
        counts.changes += worktree as usize;
        counts.untracked += (kind == b'?') as usize;
        counts.conflicted += (kind == b'u') as usize;
        if entries.len() < MAX_STATUS_ENTRIES {
            entries.push(entry);
        }
        index += 1;
    }
    ParsedStatus {
        total_entry_count,
        truncated: total_entry_count > MAX_STATUS_ENTRIES,
        entries,
        counts,
        head_state,
        branch,
        upstream,
        ahead,
        behind,
    }
}

impl GitService {
    pub fn new() -> Self {
        Self {
            snapshots: Arc::new(Mutex::new(HashMap::new())),
            write_slots: Arc::new(Mutex::new(HashMap::new())),
            outcomes: Arc::new(Mutex::new(HashMap::new())),
            revoked_owners: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }
    pub fn log(
        &self,
        _owner: &str,
        root: &Path,
        _generation: u64,
        limit: usize,
        before: Option<&str>,
    ) -> Result<GitLogResponse, String> {
        assert_workspace_root(root)?;
        let limit = limit.clamp(1, MAX_LOG_LIMIT);
        if let Some(before) = before {
            validate_oid(before)?;
        }
        if git(root, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).is_err() {
            return Ok(GitLogResponse {
                commits: Vec::new(),
                has_more: false,
            });
        }
        let mut args: Vec<OsString> = vec![
            "--no-pager".into(),
            "log".into(),
            "--first-parent".into(),
            format!("--max-count={limit}").into(),
            format!("--format={LOG_LIST_FMT}").into(),
            "-z".into(),
        ];
        if let Some(before) = before {
            let verify = format!("{before}^{{commit}}");
            if git(root, &["rev-parse", "--verify", "--quiet", &verify]).is_err() {
                return Err("unknown commit".into());
            }
            let parent = format!("{before}~1^{{commit}}");
            if git(root, &["rev-parse", "--verify", "--quiet", &parent]).is_err() {
                return Ok(GitLogResponse {
                    commits: Vec::new(),
                    has_more: false,
                });
            }
            args.push(format!("{before}~1").into());
        }
        let output = git_os(root, &args).map_err(|_| "log failed".to_string())?;
        let fields = output.split(|byte| *byte == 0).collect::<Vec<_>>();
        let mut commits = Vec::new();
        for group in fields.as_chunks::<5>().0 {
            commits.push(GitLogEntry {
                oid: String::from_utf8_lossy(group[0]).into_owned(),
                subject: String::from_utf8_lossy(group[2]).into_owned(),
                author_name: String::from_utf8_lossy(group[3]).into_owned(),
                author_time: String::from_utf8_lossy(group[4]).parse().unwrap_or(0),
            });
        }
        Ok(GitLogResponse {
            has_more: commits.len() == limit,
            commits,
        })
    }

    pub fn log_detail(
        &self,
        _owner: &str,
        root: &Path,
        _generation: u64,
        oid: &str,
    ) -> Result<GitLogDetailResponse, String> {
        assert_workspace_root(root)?;
        validate_oid(oid)?;
        let kind = commit_kind(root, oid)?;
        let args = vec![
            OsString::from("--no-pager"),
            OsString::from("log"),
            OsString::from("-1"),
            OsString::from(format!("--format={LOG_DETAIL_FMT}")),
            OsString::from("-z"),
            OsString::from(oid),
        ];
        let output = git_os(root, &args).map_err(|_| "log detail failed".to_string())?;
        let fields = output.split(|byte| *byte == 0).collect::<Vec<_>>();
        if fields.len() < 4 {
            return Err("malformed log detail".into());
        }
        let mut full_message = String::from_utf8_lossy(fields[3]).into_owned();
        while full_message.ends_with(['\r', '\n']) {
            full_message.pop();
        }
        let mut message_truncated = false;
        if full_message.len() > MAX_LOG_MESSAGE_BYTES {
            message_truncated = true;
            let mut end = MAX_LOG_MESSAGE_BYTES;
            while end > 0 && !full_message.is_char_boundary(end) {
                end -= 1;
            }
            full_message.truncate(end);
            full_message.push('…');
        }
        let mut files = Vec::new();
        let mut files_truncated = false;
        let mut files_bytes = 0;
        for file in name_status(root, &kind, oid)? {
            let encoded_path = BASE64.encode(&file.path_bytes);
            let file_bytes = file.status.len()
                + file.path.len()
                + encoded_path.len()
                + file.original_path.as_deref().map_or(0, str::len);
            if files.len() >= MAX_LOG_FILES || files_bytes + file_bytes > MAX_LOG_FILES_BYTES {
                files_truncated = true;
                break;
            }
            files_bytes += file_bytes;
            files.push(GitLogDetailFile {
                status: file.status,
                path: file.path,
                path_bytes_base64: encoded_path,
                original_path: file.original_path,
            });
        }
        Ok(GitLogDetailResponse {
            oid: String::from_utf8_lossy(fields[0]).into_owned(),
            author_name: String::from_utf8_lossy(fields[1]).into_owned(),
            author_time: String::from_utf8_lossy(fields[2]).parse().unwrap_or(0),
            full_message,
            message_truncated,
            files_truncated,
            files,
        })
    }

    pub fn commit_diff(
        &self,
        _owner: &str,
        root: &Path,
        _generation: u64,
        oid: &str,
        path_bytes: &[u8],
    ) -> Result<GitCommitDiffResponse, String> {
        assert_workspace_root(root)?;
        validate_oid(oid)?;
        let kind = commit_kind(root, oid)?;
        let file = name_status(root, &kind, oid)?
            .into_iter()
            .find(|file| file.path_bytes == path_bytes)
            .ok_or("unauthorized commit path")?;
        let path = path_arg(path_bytes);
        let args = match kind {
            CommitKind::Root => vec![
                "--literal-pathspecs".into(),
                "--no-pager".into(),
                "diff-tree".into(),
                "-p".into(),
                "--root".into(),
                oid.into(),
                "--".into(),
                path,
            ],
            CommitKind::Linear => vec![
                "--literal-pathspecs".into(),
                "--no-pager".into(),
                "diff-tree".into(),
                "-p".into(),
                oid.into(),
                "--".into(),
                path,
            ],
            CommitKind::Merge => {
                let parent = format!("{oid}^1");
                vec![
                    "--literal-pathspecs".into(),
                    "--no-pager".into(),
                    "diff".into(),
                    parent.into(),
                    oid.into(),
                    "--".into(),
                    path,
                ]
            }
        };
        let mut patch = git_os(root, &args)?;
        let truncated = patch.len() > MAX_DIFF_BYTES;
        patch.truncate(MAX_DIFF_BYTES);
        let patch_text = String::from_utf8_lossy(&patch);
        let fallback_reason = if patch_text.contains("Binary files ")
            || patch_text.contains("GIT binary patch")
            || patch.contains(&0)
        {
            Some("binary".to_string())
        } else if file.status.starts_with('R') {
            Some("rename".to_string())
        } else if file.status.starts_with('C') {
            Some("copy".to_string())
        } else {
            None
        };
        Ok(GitCommitDiffResponse {
            comparison: "commit".into(),
            path_bytes_base64: BASE64.encode(path_bytes),
            raw_patch: String::from_utf8_lossy(&patch).into_owned(),
            truncated,
            fallback_reason,
        })
    }

    pub fn status(
        &self,
        owner: &str,
        root: &Path,
        generation: u64,
    ) -> Result<GitStatusSnapshot, String> {
        assert_workspace_root(root)?;
        let output = git(root, &["status", "--porcelain=v2", "-z", "--branch"])?;
        let parsed = parse_porcelain_v2_z(&output);
        let id = format!(
            "git-{}",
            NEXT_SNAPSHOT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.prune(owner);
        self.snapshots.lock().unwrap().insert(
            id.clone(),
            SnapshotRecord {
                owner: owner.into(),
                root: root.to_path_buf(),
                generation,
                created: Instant::now(),
                entries: parsed.entries.clone(),
                head_state: parsed.head_state.clone(),
                head_oid: None,
                index_tree_oid: None,
                partial_stage_token: None,
                partial_stage_token_created: None,
            },
        );
        self.enforce_limit(owner);
        let head_oid = git(root, &["rev-parse", "--verify", "HEAD"])
            .ok()
            .map(|bytes| display_path(&bytes).trim().to_string())
            .filter(|value| !value.is_empty());
        let index_tree_oid = git(root, &["write-tree"])
            .ok()
            .map(|bytes| display_path(&bytes).trim().to_string())
            .filter(|value| !value.is_empty());
        let stats = change_stats(root, &parsed.head_state, parsed.counts.untracked);
        if let Some(record) = self.snapshots.lock().unwrap().get_mut(&id) {
            record.head_state = parsed.head_state.clone();
            record.head_oid = head_oid.clone();
            record.index_tree_oid = index_tree_oid.clone();
        }
        Ok(GitStatusSnapshot {
            snapshot_id: id,
            head_state: parsed.head_state,
            head_oid,
            index_tree_oid,
            branch: parsed.branch,
            upstream: parsed.upstream,
            ahead: parsed.ahead,
            behind: parsed.behind,
            change_stats: stats,
            counts: parsed.counts,
            returned_entry_count: parsed.entries.len(),
            total_entry_count: parsed.total_entry_count,
            truncated: parsed.truncated,
            entries: parsed.entries,
        })
    }
    fn snapshot_for(
        &self,
        snapshot_id: &str,
        owner: &str,
        root: &Path,
        generation: u64,
    ) -> Result<SnapshotRecord, String> {
        let store = self.snapshots.lock().unwrap();
        let record = store.get(snapshot_id).ok_or("stale snapshot")?;
        if record.owner != owner
            || record.root != root
            || record.generation != generation
            || record.created.elapsed() > SNAPSHOT_TTL
        {
            return Err("stale snapshot".into());
        }
        Ok(record.clone())
    }
    fn belongs_to_group(&self, entry: &GitStatusEntry, group: &str) -> bool {
        match group {
            "conflicted" => entry.entry_kind == "unmerged",
            "untracked" => entry.entry_kind == "untracked",
            "staged" => entry
                .xy
                .as_deref()
                .is_some_and(|xy| xy.as_bytes().first().is_some_and(|value| *value != b'.')),
            "changes" => entry
                .xy
                .as_deref()
                .is_some_and(|xy| xy.as_bytes().get(1).is_some_and(|value| *value != b'.')),
            _ => false,
        }
    }

    pub fn validate_path(
        &self,
        snapshot_id: &str,
        owner: &str,
        root: &Path,
        generation: u64,
        path_bytes: &[u8],
    ) -> Result<(), String> {
        let mut store = self.snapshots.lock().unwrap();
        let record = store.get(snapshot_id).ok_or("stale snapshot")?;
        if record.owner != owner
            || record.root != root
            || record.generation != generation
            || record.created.elapsed() > SNAPSHOT_TTL
        {
            store.remove(snapshot_id);
            return Err("stale snapshot".into());
        }
        if path_bytes.starts_with(b":(")
            || !record.entries.iter().any(|entry| {
                BASE64.decode(&entry.path_bytes_base64).ok().as_deref() == Some(path_bytes)
            })
        {
            return Err("unauthorized path".into());
        }
        Ok(())
    }
    pub fn write(
        &self,
        snapshot_id: &str,
        owner: &str,
        root: &Path,
        generation: u64,
        entries: &[GitPathIdentity],
        operation: &str,
    ) -> Result<(), String> {
        if entries.is_empty() || entries.len() > MAX_STATUS_ENTRIES {
            return Err("invalid path batch".into());
        }
        assert_workspace_root(root)?;
        // Per-canonical-root single-writer: hold both the Arc (to keep the
        // slot alive) and the try-lock guard for the duration of the write.
        let _write_slot = self.lock_for_write(root);
        let _slot = match _write_slot.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::WouldBlock) => return Err("busy".into()),
            Err(TryLockError::Poisoned(_)) => return Err("write slot unavailable".into()),
        };
        let snapshot = self.snapshot_for(snapshot_id, owner, root, generation)?;
        let fresh = parse_porcelain_v2_z(&git(root, &["status", "--porcelain=v2", "-z"])?).entries;
        for entry in entries {
            let old = snapshot
                .entries
                .iter()
                .find(|item| {
                    BASE64.decode(&item.path_bytes_base64).ok().as_deref()
                        == Some(entry.path_bytes.as_slice())
                })
                .ok_or("stale path")?;
            let current = fresh
                .iter()
                .find(|item| {
                    BASE64.decode(&item.path_bytes_base64).ok().as_deref()
                        == Some(entry.path_bytes.as_slice())
                })
                .ok_or("stale path")?;
            let allowed = matches!(
                (operation, entry.group.as_str()),
                ("stage", "changes" | "untracked" | "conflicted")
                    | ("unstage", "staged")
                    | ("discard", "changes")
            );
            let conflict_resolution_stage = operation == "stage"
                && entry.group == "conflicted"
                && self.belongs_to_group(old, "conflicted")
                // Editing a conflicted file can turn the live porcelain record
                // into an ordinary modified record before the user clicks
                // Stage. Accept that transition, but never accept an
                // untracked/ignored replacement for the old conflicted path.
                && matches!(current.entry_kind.as_str(), "unmerged" | "ordinary")
                && !matches!(current.entry_kind.as_str(), "untracked" | "ignored");
            if !allowed
                || !self.belongs_to_group(old, &entry.group)
                || (!conflict_resolution_stage && !self.belongs_to_group(current, &entry.group))
                || (!conflict_resolution_stage && old.xy != current.xy)
                || (!conflict_resolution_stage && old.entry_kind != current.entry_kind)
                || (!conflict_resolution_stage && old.submodule != current.submodule)
                || old
                    .original_path_bytes_base64
                    .as_deref()
                    .and_then(|v| BASE64.decode(v).ok())
                    != entry.original_path_bytes
                || current
                    .original_path_bytes_base64
                    .as_deref()
                    .and_then(|v| BASE64.decode(v).ok())
                    != entry.original_path_bytes
            {
                return Err("stale path".into());
            }
        }
        let paths = entries
            .iter()
            .flat_map(|entry| {
                std::iter::once(entry.path_bytes.clone()).chain(entry.original_path_bytes.clone())
            })
            .collect::<Vec<_>>();
        let mut args = vec![OsString::from("--literal-pathspecs")];
        match operation {
            "stage" => args.extend([OsString::from("add"), OsString::from("-A")]),
            "unstage" => args.push(OsString::from("reset")),
            "discard" => args.extend([OsString::from("checkout")]),
            _ => return Err("unsupported Git write".into()),
        }
        args.push(OsString::from("--"));
        args.extend(paths.iter().map(|path| path_arg(path)));
        let _ = git_os(root, &args)?;
        Ok(())
    }

    /// Push the current branch to its upstream, or to the default remote with
    /// `--set-upstream` when no upstream is configured yet.
    ///
    /// Authentication is strictly non-interactive: the Tauri child has no TTY,
    /// so any credential or passphrase prompt would hang until `PUSH_DEADLINE`
    /// kills the process group. `git_command` already sets
    /// GIT_TERMINAL_PROMPT=0 and a null stdin; this adds askpass suppression
    /// and SSH BatchMode so a missing credential fails immediately with a
    /// message the user can act on instead of stalling the panel.
    pub fn push(&self, root: &Path) -> Result<GitPushOutcome, String> {
        assert_workspace_root(root)?;
        // Share the per-root write slot with stage/unstage/discard/commit so a
        // push never races an index mutation on the same workspace.
        let write_slot = self.lock_for_write(root);
        let _slot = match write_slot.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::WouldBlock) => return Err("busy".into()),
            Err(TryLockError::Poisoned(_)) => return Err("write slot unavailable".into()),
        };
        let status =
            parse_porcelain_v2_z(&git(root, &["status", "--porcelain=v2", "-z", "--branch"])?);
        let branch = match (status.head_state.as_str(), status.branch.clone()) {
            ("attached", Some(branch)) if !branch.is_empty() => branch,
            _ => return Err(PUSH_DETACHED_HEAD.into()),
        };
        // The configured upstream owns both the remote and the remote branch.
        // Deriving the refspec from the local branch name instead would publish
        // a same-named branch on the remote whenever the two names differ,
        // leaving the real upstream behind.
        let (remote, upstream_branch, set_upstream) = match status
            .upstream
            .as_deref()
            .and_then(|upstream| upstream.split_once('/'))
            .filter(|(remote, branch)| !remote.is_empty() && !branch.is_empty())
        {
            Some((remote, upstream_branch)) => {
                (remote.to_owned(), upstream_branch.to_owned(), false)
            }
            None => {
                let remotes = String::from_utf8_lossy(&git(root, &["remote"])?)
                    .lines()
                    .map(|line| line.trim().to_owned())
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>();
                (
                    remotes.first().cloned().ok_or(PUSH_NO_REMOTE.to_string())?,
                    String::new(),
                    true,
                )
            }
        };
        let mut args = vec![OsString::from("push")];
        if set_upstream {
            args.push(OsString::from("--set-upstream"));
        }
        args.push(OsString::from(remote.clone()));
        // `--` ends option parsing: a branch whose name starts with `-` is a
        // legal ref, but git would otherwise read the refspec as a switch.
        args.push(OsString::from("--"));
        if set_upstream {
            // Publish under the branch's own name; HEAD keeps that name out of
            // argv and lets git derive it.
            args.push(OsString::from("HEAD"));
        } else {
            args.push(OsString::from(format!("HEAD:{upstream_branch}")));
        }
        let mut command = git_command(root, args);
        // Absent a TTY, these keep git from blocking on a prompt it can never
        // answer — it fails with an actionable message instead.
        command.env("GIT_ASKPASS", "").env("SSH_ASKPASS", "");
        // Impose BatchMode only when the user has no SSH command of their own.
        // Overriding a configured core.sshCommand would break bespoke setups
        // (custom keys, jump hosts, ProxyCommand).
        let user_ssh_command = std::env::var_os("GIT_SSH_COMMAND").is_some()
            || std::env::var_os("GIT_SSH").is_some()
            || git(root, &["config", "--get", "core.sshCommand"]).is_ok();
        if !user_ssh_command {
            command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        // A configured credential helper (macOS keychain, GCM) is deliberately
        // left in place — removing it would break HTTPS pushes that rely on a
        // stored token. Only GCM's GUI prompt is switched off: it is the one
        // prompt that would block, unseen, for the whole deadline while the
        // per-root write slot is held. Other helpers fail without a TTY.
        command.env("GCM_INTERACTIVE", "never");
        let (stdout, stderr, success) = run_git(command, PUSH_DEADLINE)?;
        let transcript = if stderr.is_empty() { stdout } else { stderr };
        let output = String::from_utf8_lossy(&transcript)
            .trim()
            .chars()
            .take(MAX_PUSH_OUTPUT_BYTES)
            .collect::<String>();
        if !success {
            return Err(if output.is_empty() {
                "git push failed".into()
            } else {
                output
            });
        }
        Ok(GitPushOutcome {
            remote,
            branch,
            set_upstream,
            output,
        })
    }

    pub fn prepare_ai_snapshot(
        &self,
        owner: &str,
        root: &Path,
        generation: u64,
    ) -> Result<GitAiSnapshot, String> {
        // Read-consistency protocol: capture the index tree before and after
        // reading porcelain + staged diff. If the tree changed between the two
        // reads (because an external process mutated the index), retry up to
        // 3 times; if it never stabilizes, refuse to create a snapshot whose
        // staged diff does not match its bound index tree.
        const MAX_RETRIES: usize = 3;
        for _attempt in 0..MAX_RETRIES {
            let tree_before = git(root, &["write-tree"])
                .ok()
                .map(|v| display_path(&v).trim().to_owned());
            let status = self.status(owner, root, generation)?;
            if status.counts.staged == 0 {
                return Err("no staged changes".into());
            }
            if status.counts.conflicted > 0 {
                return Err("conflicted changes".into());
            }
            let diff = git(
                root,
                &[
                    "--literal-pathspecs",
                    "diff",
                    "--cached",
                    "--binary",
                    "--no-color",
                    "--",
                ],
            )?;
            let tree_after = git(root, &["write-tree"])
                .ok()
                .map(|v| display_path(&v).trim().to_owned());
            // Three-way consistency: the index tree captured before reading
            // status, the tree status() bound to the snapshot, and the tree
            // captured after reading the staged diff must all be equal. If any
            // differ, an external process mutated the index between reads and
            // the staged diff may not correspond to the bound tree — retry.
            if tree_before != tree_after || status.index_tree_oid != tree_before {
                continue;
            }
            let (staged_diff, staged_diff_truncated) = bounded_ai_diff(&diff);
            return Ok(GitAiSnapshot {
                snapshot_id: status.snapshot_id,
                head_state: status.head_state,
                head_oid: status.head_oid,
                index_tree_oid: status.index_tree_oid,
                staged_diff,
                staged_diff_truncated,
            });
        }
        Err("staged diff did not stabilize".into())
    }

    /// Synchronous pre-commit validation. Returns `confirmationRequired:<token>`
    /// when a partial-stage file needs explicit user confirmation; otherwise
    /// returns `Ok(())` and the caller may spawn the detached commit.
    pub fn prepare_commit(
        &self,
        snapshot_id: &str,
        owner: &str,
        root: &Path,
        generation: u64,
        message: &str,
        confirmation_token: Option<&str>,
    ) -> Result<(), String> {
        if message.trim().is_empty() || message.len() > 64 * 1024 {
            return Err("invalid commit message".into());
        }
        assert_workspace_root(root)?;
        let snapshot = self.snapshot_for(snapshot_id, owner, root, generation)?;
        let current_head = git(root, &["rev-parse", "--verify", "HEAD"])
            .ok()
            .map(|v| display_path(&v).trim().to_owned());
        let current_tree = git(root, &["write-tree"])
            .ok()
            .map(|v| display_path(&v).trim().to_owned());
        if current_head != snapshot.head_oid || current_tree != snapshot.index_tree_oid {
            return Err("stale snapshot".into());
        }
        let partial = snapshot.entries.iter().any(|entry| {
            entry
                .xy
                .as_deref()
                .is_some_and(|xy| xy.as_bytes().first().is_some_and(|v| *v != b'.'))
                && entry
                    .xy
                    .as_deref()
                    .is_some_and(|xy| xy.as_bytes().get(1).is_some_and(|v| *v != b'.'))
        });
        if partial {
            let mut store = self.snapshots.lock().unwrap();
            let record = store.get_mut(snapshot_id).ok_or("stale snapshot")?;
            if confirmation_token.is_none() {
                let token = Uuid::new_v4().simple().to_string();
                record.partial_stage_token = Some(token.clone());
                record.partial_stage_token_created = Some(Instant::now());
                return Err(format!("confirmationRequired:{token}"));
            }
            if record.partial_stage_token.as_deref() != confirmation_token
                || record
                    .partial_stage_token_created
                    .is_none_or(|created| created.elapsed() > Duration::from_secs(60))
            {
                return Err("invalid confirmation token".into());
            }
            record.partial_stage_token = None;
            record.partial_stage_token_created = None;
        }
        Ok(())
    }

    /// Spawn a detached, host-owned commit. Once spawned the commit is not
    /// cancelled by client disconnect or workspace transition: the host runs
    /// it to completion under a 5-minute hard deadline, records initial HEAD
    /// for reconciliation, stores the outcome bound to owner/root/generation
    /// for 10 minutes, and removes the snapshot. The caller is notified via
    /// `record_outcome`; reconnecting clients recover the result through
    /// `take_pending_outcome`.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_detached(
        &self,
        snapshot_id: String,
        owner: String,
        root: PathBuf,
        generation: u64,
        request_id: String,
        message: String,
        notify: Option<Box<dyn FnOnce(String) + Send>>,
    ) {
        // Capture the authorized snapshot before the detached task can wait
        // for the per-workspace write slot. A workspace transition is allowed
        // to clear interactive snapshots after spawn, but must not turn this
        // host-owned commit into a stale request while it is queued.
        let captured_snapshot = self
            .snapshot_for(&snapshot_id, &owner, &root, generation)
            .ok();
        let snapshots = self.snapshots.clone();
        let outcomes = self.outcomes.clone();
        let revoked_owners = self.revoked_owners.clone();
        let write_slot = self.lock_for_write(&root);
        thread::spawn(move || {
            let _slot = write_slot.lock().unwrap();
            // Re-validate HEAD/index against the snapshot captured at spawn
            // while holding the write slot. A concurrent index mutation is
            // rejected, but transition cleanup cannot revoke this job's
            // already-authorized snapshot.
            let snapshot = match captured_snapshot {
                Some(record) => record,
                None => {
                    let frame = record_outcome(
                        &outcomes,
                        &revoked_owners,
                        &owner,
                        &root,
                        generation,
                        &request_id,
                        "failed",
                        None,
                        false,
                        Some("stale snapshot".into()),
                    );
                    if let Some(notify) = notify {
                        notify(frame);
                    }
                    return;
                }
            };
            let current_head = git(&root, &["rev-parse", "--verify", "HEAD"])
                .ok()
                .map(|v| display_path(&v).trim().to_owned());
            let current_tree = git(&root, &["write-tree"])
                .ok()
                .map(|v| display_path(&v).trim().to_owned());
            if current_head != snapshot.head_oid || current_tree != snapshot.index_tree_oid {
                let frame = record_outcome(
                    &outcomes,
                    &revoked_owners,
                    &owner,
                    &root,
                    generation,
                    &request_id,
                    "failed",
                    None,
                    false,
                    Some("stale snapshot".into()),
                );
                if let Some(notify) = notify {
                    notify(frame);
                }
                snapshots.lock().unwrap().remove(&snapshot_id);
                return;
            }
            // Copy the expected tree into the detached job before executing
            // Git. Workspace transition cleanup may remove authorization
            // snapshots while this host-owned commit is still running.
            let expected_tree = snapshot.index_tree_oid.clone();
            let initial_head = current_head;
            let temp = match tempfile::Builder::new()
                .prefix("picot-git-message-")
                .tempfile()
            {
                Ok(temp) => temp,
                Err(error) => {
                    let frame = record_outcome(
                        &outcomes,
                        &revoked_owners,
                        &owner,
                        &root,
                        generation,
                        &request_id,
                        "failed",
                        None,
                        false,
                        Some(error.to_string()),
                    );
                    if let Some(notify) = notify {
                        notify(frame);
                    }
                    snapshots.lock().unwrap().remove(&snapshot_id);
                    return;
                }
            };
            if let Err(error) = fs::write(temp.path(), message.as_bytes()) {
                let frame = record_outcome(
                    &outcomes,
                    &revoked_owners,
                    &owner,
                    &root,
                    generation,
                    &request_id,
                    "failed",
                    None,
                    false,
                    Some(error.to_string()),
                );
                if let Some(notify) = notify {
                    notify(frame);
                }
                snapshots.lock().unwrap().remove(&snapshot_id);
                return;
            }
            let args = [
                OsString::from("commit"),
                OsString::from("-F"),
                temp.path().as_os_str().to_owned(),
            ];
            let started = Instant::now();
            // Commit uses a dedicated 5-minute deadline, not the 30s write
            // deadline, because pre-commit hooks may legitimately run long.
            // run_commit also puts git in its own process group so hook
            // descendants are killed as a tree on timeout.
            let commit_result = run_git(git_command(&root, args.iter().cloned()), COMMIT_DEADLINE);
            let _elapsed = started.elapsed();
            // run_git returns (stdout, stderr, success) or Err on timeout/overflow.
            let (commit_ok, stderr_text) = match commit_result {
                Ok((_stdout, stderr, success)) => {
                    (success, String::from_utf8_lossy(&stderr).to_string())
                }
                Err(error) => {
                    // timeout or output overflow — no stderr available.
                    (false, error)
                }
            };
            let timed_out = stderr_text.contains("timed out");
            // Reconciliation: compare initial and current HEAD to classify
            // succeeded / failed / outcomeUnknown. Unborn repos go from
            // None -> Some(oid) on first commit.
            let current_head = git(&root, &["rev-parse", "--verify", "HEAD"])
                .ok()
                .map(|v| display_path(&v).trim().to_owned());
            let actual_tree = git(&root, &["rev-parse", "HEAD^{tree}"])
                .ok()
                .map(|v| display_path(&v).trim().to_owned());
            let hook_changed_tree = actual_tree.is_some() && actual_tree != expected_tree;
            // Success means HEAD changed (including None -> Some for unborn).
            // Failure means HEAD stayed the same. outcomeUnknown covers timeout
            // or ambiguous HEAD state. Preserve bounded stderr so the user can
            // see why a hook rejected the commit.
            let (status, commit_oid, error) =
                if commit_ok && current_head.is_some() && current_head != initial_head {
                    ("succeeded", current_head.clone(), None)
                } else if !commit_ok && !timed_out && current_head == initial_head {
                    // Hook rejection or commit failure — include stderr so the
                    // user can diagnose and retry.
                    let trimmed = stderr_text.trim();
                    (
                        "failed",
                        None,
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        },
                    )
                } else {
                    // timeout or ambiguous HEAD state
                    (
                        "outcomeUnknown",
                        None,
                        if stderr_text.trim().is_empty() {
                            None
                        } else {
                            Some(stderr_text.trim().to_string())
                        },
                    )
                };
            let frame = record_outcome(
                &outcomes,
                &revoked_owners,
                &owner,
                &root,
                generation,
                &request_id,
                status,
                commit_oid,
                hook_changed_tree,
                error,
            );
            if let Some(notify) = notify {
                notify(frame);
            }
            snapshots.lock().unwrap().remove(&snapshot_id);
        });
    }

    /// Take (consume) all pending outcomes that match owner + root + generation.
    /// Returns an empty Vec when no pending outcomes exist for these three
    /// values, so a client that has switched to another workspace/generation
    /// never receives an old workspace's commit results. Multiple detached
    /// commits that completed while the client was disconnected are all
    /// recovered here, sorted by creation time so the UI sees them in the
    /// order they finished.
    #[allow(dead_code)]
    pub fn take_pending_outcomes(
        &self,
        owner: &str,
        root: &Path,
        generation: u64,
    ) -> Vec<PendingGitOutcome> {
        let root_string = root.to_string_lossy().to_string();
        let mut store = self.outcomes.lock().unwrap();
        // Prune expired outcomes on read too, so a reconnect long after a
        // commit never receives a stale result past its 10-minute TTL.
        store.retain(|_, existing| {
            existing
                .created
                .is_some_and(|created| created.elapsed() <= OUTCOME_TTL)
        });
        let matching_keys: Vec<String> = store
            .iter()
            .filter(|(_, outcome)| {
                outcome.owner == owner
                    && outcome.root == root_string
                    && outcome.generation == generation
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut results = Vec::new();
        for key in matching_keys {
            if let Some(outcome) = store.remove(&key) {
                results.push(outcome);
            }
        }
        // Sort by creation time so outcomes are delivered in completion order.
        results.sort_by_key(|a| a.created);
        results
    }

    #[allow(clippy::too_many_arguments)]
    pub fn diff(
        &self,
        snapshot_id: &str,
        owner: &str,
        root: &Path,
        generation: u64,
        group: &str,
        path_bytes: &[u8],
        comparison: &str,
    ) -> Result<GitDiffResponse, String> {
        self.validate_path(snapshot_id, owner, root, generation, path_bytes)?;
        let snapshot = self.snapshot_for(snapshot_id, owner, root, generation)?;
        let entry = snapshot
            .entries
            .iter()
            .find(|item| BASE64.decode(&item.path_bytes_base64).ok().as_deref() == Some(path_bytes))
            .ok_or("stale path")?;
        let expected_comparison = match group {
            "staged" => "staged",
            "changes" => "changes",
            "untracked" => "untracked",
            _ => return Err("unsupported diff group".into()),
        };
        if !self.belongs_to_group(entry, group) || comparison != expected_comparison {
            return Err("unauthorized diff".into());
        }
        let path = path_arg(path_bytes);
        let args: Vec<OsString> = match comparison {
            "staged" => vec![
                "--literal-pathspecs".into(),
                "diff".into(),
                "--cached".into(),
                "--".into(),
                path,
            ],
            "changes" => vec![
                "--literal-pathspecs".into(),
                "diff".into(),
                "--".into(),
                path,
            ],
            "untracked" => vec![
                "--literal-pathspecs".into(),
                "diff".into(),
                "--no-index".into(),
                "/dev/null".into(),
                path,
            ],
            _ => return Err("unsupported comparison".into()),
        };
        let mut patch = git_os(root, &args)?;
        let truncated = patch.len() > MAX_DIFF_BYTES;
        patch.truncate(MAX_DIFF_BYTES);
        // Detect non-textual diff cases that must render as raw patch with an
        // explicit reason rather than a side-by-side alignment.
        let patch_str = String::from_utf8_lossy(&patch);
        let is_binary_marker = patch_str.contains("Binary files ")
            || patch_str.contains("GIT binary patch")
            || patch_str.contains("Binary files a/")
            || patch.contains(&0);
        let fallback_reason = if entry.submodule.is_some() {
            Some("submodule".to_string())
        } else if entry.entry_kind == "unmerged" {
            Some("conflict".to_string())
        } else if is_binary_marker {
            Some("binary".to_string())
        } else if entry.entry_kind == "rename" || entry.entry_kind == "copy" {
            // Rename/copy diffs are textual but may include a mode-only or
            // similarity block that cannot be aligned; fall back to raw patch.
            Some(entry.entry_kind.clone())
        } else {
            None
        };
        Ok(GitDiffResponse {
            snapshot_id: snapshot_id.into(),
            comparison: comparison.into(),
            path_bytes_base64: BASE64.encode(path_bytes),
            raw_patch: String::from_utf8_lossy(&patch).to_string(),
            truncated,
            fallback_reason,
        })
    }
    /// Clear snapshots for an owner. Used on client disconnect: pending
    /// status/diff requests are cancelled, but detached commit outcomes are
    /// preserved so a reconnecting client can still receive the result.
    pub fn clear_owner_snapshots(&self, owner: &str) {
        self.snapshots
            .lock()
            .unwrap()
            .retain(|_, record| record.owner != owner);
    }
    /// Clear status/diff authorization snapshots during a workspace
    /// transition. Detached outcomes remain bound to their origin root and
    /// generation until expiry; deleting them here would violate their
    /// host-owned lifecycle before the transition settles.
    pub fn clear_workspace_state(&self, owner: &str) {
        self.clear_owner_snapshots(owner);
    }
    /// Clear all Git state and revoke the owner after window destruction.
    /// A detached commit that completes after revoke must not record an
    /// outcome no client will ever consume.
    pub fn clear_owner(&self, owner: &str) {
        self.clear_workspace_state(owner);
        self.outcomes
            .lock()
            .unwrap()
            .retain(|_, outcome| outcome.owner != owner);
        self.revoked_owners
            .lock()
            .unwrap()
            .insert(owner.to_string());
    }
    /// Un-mark an owner as revoked (e.g. when a new window reopens the same
    /// workspace). Outcomes recorded before the reopen are already gone; new
    /// commits after this point may store outcomes normally.
    #[allow(dead_code)]
    pub fn revive_owner(&self, owner: &str) {
        self.revoked_owners.lock().unwrap().remove(owner);
    }
    fn prune(&self, owner: &str) {
        self.snapshots
            .lock()
            .unwrap()
            .retain(|_, record| record.owner != owner || record.created.elapsed() <= SNAPSHOT_TTL);
    }
    fn enforce_limit(&self, owner: &str) {
        let mut store = self.snapshots.lock().unwrap();
        while store
            .values()
            .filter(|record| record.owner == owner)
            .count()
            > MAX_SNAPSHOTS_PER_OWNER
        {
            if let Some(id) = store
                .iter()
                .filter(|(_, record)| record.owner == owner)
                .min_by_key(|(_, record)| record.created)
                .map(|(id, _)| id.clone())
            {
                store.remove(&id);
            }
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn record_outcome(
    outcomes: &Arc<Mutex<HashMap<String, PendingGitOutcome>>>,
    revoked_owners: &Arc<Mutex<std::collections::HashSet<String>>>,
    owner: &str,
    root: &Path,
    generation: u64,
    request_id: &str,
    status: &str,
    commit_oid: Option<String>,
    hook_changed_tree: bool,
    error: Option<String>,
) -> String {
    let outcome = PendingGitOutcome {
        owner: owner.to_string(),
        root: root.to_string_lossy().to_string(),
        generation,
        request_id: request_id.to_string(),
        status: status.to_string(),
        commit_oid: commit_oid.clone(),
        hook_changed_tree,
        error: error.clone(),
        created: Some(Instant::now()),
    };
    // Use a host-synthesized key so different owners with the same
    // browser-generated requestId (e.g. both start at "git-1") cannot
    // overwrite each other's outcomes.
    let key = format!(
        "{}\0{}\0{}\0{}",
        owner,
        root.to_string_lossy(),
        generation,
        request_id
    );
    let mut store = outcomes.lock().unwrap();
    // A revoked owner's detached commit completed after the workspace was
    // destroyed/replaced — drop the outcome so it cannot linger as state no
    // client will ever consume.
    if revoked_owners.lock().unwrap().contains(owner) {
        return serde_json::to_string(&serde_json::json!({
            "type": "git_commit_result",
            "requestId": request_id,
            "workspaceGeneration": generation,
            "status": status,
            "commitOid": commit_oid.clone(),
            "hookChangedTree": hook_changed_tree,
            "error": error.clone()
        }))
        .unwrap_or_else(|_| {
            "{\"type\":\"git_commit_result\",\"status\":\"outcomeUnknown\"}".into()
        });
    }
    // Drop expired outcomes (10-minute TTL) before inserting the new one.
    store.retain(|_, existing| {
        existing
            .created
            .is_some_and(|created| created.elapsed() <= OUTCOME_TTL)
    });
    store.insert(key, outcome.clone());
    let frame = serde_json::json!({
        "type": "git_commit_result",
        "requestId": request_id,
        "workspaceGeneration": generation,
        "status": status,
        "commitOid": commit_oid.clone(),
        "hookChangedTree": hook_changed_tree,
        "error": error.clone()
    });
    serde_json::to_string(&frame)
        .unwrap_or_else(|_| "{\"type\":\"git_commit_result\",\"status\":\"outcomeUnknown\"}".into())
}

pub fn validate_oid(oid: &str) -> Result<(), String> {
    if (oid.len() != 40 && oid.len() != 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid oid".into());
    }
    Ok(())
}

pub fn commit_kind(root: &Path, oid: &str) -> Result<CommitKind, String> {
    validate_oid(oid)?;
    let verify = format!("{oid}^{{commit}}");
    if git(root, &["rev-parse", "--verify", "--quiet", &verify]).is_err() {
        return Err("unknown commit".into());
    }
    let parents = git(root, &["log", "-1", "--format=%P", oid])?;
    Ok(
        match String::from_utf8_lossy(&parents).split_whitespace().count() {
            0 => CommitKind::Root,
            1 => CommitKind::Linear,
            _ => CommitKind::Merge,
        },
    )
}

fn name_status(
    root: &Path,
    kind: &CommitKind,
    oid: &str,
) -> Result<Vec<GitNameStatusFile>, String> {
    let output = match kind {
        CommitKind::Root => git_os(
            root,
            &[
                "--literal-pathspecs".into(),
                "--no-pager".into(),
                "diff-tree".into(),
                "--no-commit-id".into(),
                "--name-status".into(),
                "-M".into(),
                "-C".into(),
                "-r".into(),
                "-z".into(),
                "--root".into(),
                oid.into(),
            ],
        )?,
        CommitKind::Linear => git_os(
            root,
            &[
                "--literal-pathspecs".into(),
                "--no-pager".into(),
                "diff-tree".into(),
                "--no-commit-id".into(),
                "--name-status".into(),
                "-M".into(),
                "-C".into(),
                "-r".into(),
                "-z".into(),
                oid.into(),
            ],
        )?,
        CommitKind::Merge => {
            let parent = format!("{oid}^1");
            git_os(
                root,
                &[
                    "--literal-pathspecs".into(),
                    "--no-pager".into(),
                    "diff".into(),
                    "--name-status".into(),
                    "-M".into(),
                    "-C".into(),
                    "-r".into(),
                    "-z".into(),
                    parent.into(),
                    oid.into(),
                ],
            )?
        }
    };
    Ok(parse_diff_tree_name_status_z(&output))
}

fn path_arg(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        OsString::from_vec(bytes.to_vec())
    }
    #[cfg(windows)]
    {
        OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

const GIT_NOT_FOUND: &str = "git_not_found";

fn git_spawn_error(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        GIT_NOT_FOUND.to_string()
    } else {
        error.to_string()
    }
}

fn run_git(mut command: Command, deadline: Duration) -> Result<(Vec<u8>, Vec<u8>, bool), String> {
    let mut child = command.spawn().map_err(|error| git_spawn_error(&error))?;
    let stdout = child.stdout.take().ok_or("git stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("git stderr unavailable")?;
    let out_thread = thread::spawn(move || read_limited(stdout, MAX_STDOUT_BYTES));
    let err_thread = thread::spawn(move || read_limited(stderr, MAX_STDERR_BYTES));
    let deadline_at = Instant::now() + deadline;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            let (out, out_overflow) = out_thread
                .join()
                .map_err(|_| "git stdout reader failed".to_string())??;
            let (err, _err_overflow) = err_thread
                .join()
                .map_err(|_| "git stderr reader failed".to_string())??;
            if out_overflow {
                // Truncated status output is unsafe to parse as porcelain.
                terminate_git(&mut child);
                return Err("Git output exceeded limit".into());
            }
            return Ok((out, err, status.success()));
        }
        if Instant::now() >= deadline_at {
            terminate_git(&mut child);
            let _ = out_thread.join();
            let _ = err_thread.join();
            return Err("Git command timed out".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
fn read_limited<R: std::io::Read>(mut reader: R, cap: usize) -> Result<(Vec<u8>, bool), String> {
    let mut result = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut overflow = false;
    loop {
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if result.len() >= cap {
            // Continue draining so the child does not block on a full pipe,
            // but stop accumulating and flag overflow so the caller can reject
            // the truncated output.
            overflow = true;
            continue;
        }
        let allowed = count.min(cap - result.len());
        result.extend_from_slice(&buffer[..allowed]);
        if result.len() >= cap {
            overflow = true;
        }
    }
    Ok((result, overflow))
}
fn terminate_git(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}
fn git_command(root: &Path, args: impl IntoIterator<Item = OsString>) -> Command {
    let mut command = Command::new("git");
    crate::windows_child::hide_console(&mut command);
    command
        .current_dir(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Defense in depth against parent-repo discovery: cap repository
    // discovery at the workspace root so a stray git invocation whose cwd
    // is deeper than the authorized top-level cannot walk up into a parent
    // repository even if some other guard is bypassed. `--show-toplevel`
    // enforcement in `assert_workspace_root` is the authoritative check.
    if let Ok(canonical) = fs::canonicalize(root) {
        if let Some(path) = canonical.parent() {
            command.env("GIT_CEILING_DIRECTORIES", path);
        }
    }
    // Make git a process-group leader so hook descendants can be killed as a
    // tree. Without this, killpg(-pid) is a no-op because git is not its own
    // process-group leader.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}
fn git_os(root: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
    let (stdout, stderr, success) =
        run_git(git_command(root, args.iter().cloned()), GIT_WRITE_DEADLINE)?;
    if success || args.iter().any(|arg| arg == "diff") && args.iter().any(|arg| arg == "--no-index")
    {
        Ok(stdout)
    } else {
        Err(String::from_utf8_lossy(&stderr).to_string())
    }
}
// ─── Turn files card stats (2026-09-19 spec) ─────────────────────────────────

/// Spec bounds for one `git_turn_stats` call.
const TURN_STATS_MAX_PATHS: usize = 200;
const TURN_STATS_MAX_PATH_CHARS: usize = 4096;
/// Untracked line counting stops at the first of these (spec: 256 KiB / 5 万行).
const TURN_STATS_MAX_UNTRACKED_BYTES: usize = 256 * 1024;
const TURN_STATS_MAX_UNTRACKED_LINES: u64 = 50_000;
const TURN_STATS_READ_CHUNK: usize = 8 * 1024;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TurnFileStat {
    /// Path exactly as the caller supplied it (the card keys rows by it).
    path: String,
    /// "added" | "modified" | "untracked" | "deleted" | "clean" | "unavailable".
    /// `deleted` and `unavailable` rows carry no stats; the card renders every
    /// status except `deleted`.
    status: String,
    additions: u64,
    deletions: u64,
    /// True when untracked counting stopped before EOF, so `additions` is a
    /// lower bound and the card renders `+≥N`.
    additions_capped: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TurnStats {
    pub files: Vec<TurnFileStat>,
    /// Caller-supplied paths whose containment could not be decided (outside
    /// the workspace, or containing `..`). They are still listed with
    /// `status = "unavailable"`; this count exists so tests and diagnostics can
    /// tell a deliberate rejection from a silent pathspec miss.
    pub dropped: usize,
}

/// Map one caller-supplied path to the workspace-relative pathspec for it, or
/// `None` when containment cannot be decided (root escape, `..`, root itself).
///
/// Deliberately lexical, unlike `assert_workspace_root`'s `fs::canonicalize`:
/// canonicalization fails for paths that no longer exist and resolves
/// in-workspace symlinks that point outside, so it would misclassify legitimate
/// writes. Containment is decided per component, never by string prefix
/// (`docs/engineering-lessons.md` #3).
fn workspace_relative_spec(
    root: &Path,
    canonical_root: Option<&Path>,
    input: &str,
) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = Path::new(trimmed);
    let relative = if path.is_absolute() {
        // The same workspace reaches us both canonicalized (`/private/var/…` on
        // macOS) and as the caller spelled it (`/var/…`); accept either spelling
        // so a canonical write path is not misread as an escape. The canonical
        // root is resolved once per call, not once per path.
        path.strip_prefix(root)
            .ok()
            .or_else(|| canonical_root.and_then(|canonical| path.strip_prefix(canonical).ok()))?
    } else {
        path
    };
    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            // `ParentDir` (and any root/prefix component in a relative path) is
            // not decidable inside the workspace.
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Count lines in the head of an untracked file, bounded by the spec caps and
/// by `deadline`. Returns `(lines, capped)`; a NUL byte means binary, which is
/// never counted. The final line counts when the file has no trailing newline.
///
/// Recovery here is deliberately local: the path was untracked when git listed
/// it, so it can be gone by now, or be a link this walk refuses to follow. The
/// row still renders, only without numbers.
fn count_untracked_lines(path: &Path, deadline: Instant) -> (u64, bool) {
    // `symlink_metadata` on purpose: counting lines is not a reason to open a
    // link that may point outside the workspace.
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return (0, false);
    };
    if !metadata.is_file() {
        return (0, false);
    }
    let expected_len = metadata.len();
    let Ok(mut file) = fs::File::open(path) else {
        return (0, false);
    };
    let mut buffer = [0u8; TURN_STATS_READ_CHUNK];
    let mut lines = 0u64;
    let mut bytes = 0usize;
    let mut last_byte = None;
    let mut reached_eof = false;
    loop {
        let count = match file.read(&mut buffer) {
            Ok(0) => {
                reached_eof = true;
                break;
            }
            Ok(count) => count,
            Err(_) => break,
        };
        let chunk = &buffer[..count];
        if chunk.contains(&0) {
            return (0, false);
        }
        lines += chunk.iter().filter(|byte| **byte == b'\n').count() as u64;
        last_byte = Some(chunk[count - 1]);
        bytes += count;
        if bytes >= TURN_STATS_MAX_UNTRACKED_BYTES
            || lines >= TURN_STATS_MAX_UNTRACKED_LINES
            || Instant::now() >= deadline
        {
            break;
        }
    }
    let trailing = u64::from(matches!(last_byte, Some(byte) if byte != b'\n'));
    if reached_eof || bytes as u64 >= expected_len {
        // EOF, or the cap landed exactly on the end of the file: exact count.
        return (lines + trailing, false);
    }
    // A cap (or the shared deadline) stopped us mid-file: report a lower bound.
    (lines, true)
}

/// Working-tree stats for the files a turn wrote (cumulative vs HEAD, frozen at
/// turn end per the 2026-09-19 spec): porcelain v1 XY classification plus
/// numstat (vs HEAD; `--cached` when HEAD is unborn, mirroring `change_stats`).
/// Untracked files count their head line count as additions.
pub fn turn_stats(root: &Path, paths: &[String]) -> Result<TurnStats, String> {
    assert_workspace_root(root)?;
    if paths.len() > TURN_STATS_MAX_PATHS {
        return Err(format!(
            "too many paths ({} > {TURN_STATS_MAX_PATHS})",
            paths.len()
        ));
    }
    if paths
        .iter()
        .any(|path| path.chars().count() > TURN_STATS_MAX_PATH_CHARS)
    {
        return Err(format!(
            "path longer than {TURN_STATS_MAX_PATH_CHARS} chars"
        ));
    }
    if paths.is_empty() {
        return Ok(TurnStats {
            files: Vec::new(),
            dropped: 0,
        });
    }
    // One wall-clock budget for the whole call: the per-file byte cap bounds IO,
    // this bounds it further on slow volumes (network shares, dead mounts).
    let deadline = Instant::now() + GIT_READ_DEADLINE;
    let canonical_root = fs::canonicalize(root).ok();

    // Containment gate first: an undecidable path never reaches git as a
    // pathspec, and is reported as `unavailable` so the row still renders.
    let specs: Vec<Option<String>> = paths
        .iter()
        .map(|input| workspace_relative_spec(root, canonical_root.as_deref(), input))
        .collect();
    let dropped = specs.iter().filter(|spec| spec.is_none()).count();
    let accepted: Vec<&str> = specs.iter().flatten().map(String::as_str).collect();

    // XY classification from `status --porcelain -z --` (records are NUL
    // separated: "XY path"; renames carry a second, header-less record).
    let mut classes: HashMap<String, char> = HashMap::new();
    if !accepted.is_empty() {
        let mut status_args: Vec<&str> =
            vec!["--literal-pathspecs", "status", "--porcelain", "-z", "--"];
        status_args.extend(accepted.iter().copied());
        let status_out = git(root, &status_args)?;
        let mut skip_rename_tail = false;
        for record in status_out.split(|byte| *byte == 0) {
            if record.is_empty() {
                continue;
            }
            if skip_rename_tail {
                skip_rename_tail = false;
                // `-z` lists the rename source after the target record: it no
                // longer exists in the worktree, so it is a deleted row.
                let source = String::from_utf8_lossy(record).to_string();
                if !source.is_empty() {
                    classes.insert(source, 'D');
                }
                continue;
            }
            let text = String::from_utf8_lossy(record).to_string();
            if text.len() < 4 {
                continue;
            }
            let xy = &text[..2];
            let path = text[3..].to_string();
            if xy.starts_with('R') || xy.starts_with('C') {
                skip_rename_tail = true;
            }
            // `D` wins over `A`: `AD`/`MD` mean the worktree file is gone.
            let class = if xy == "??" {
                'U'
            } else if xy.contains('D') {
                'D'
            } else if xy.contains('A') {
                'A'
            } else {
                'M'
            };
            classes.insert(path, class);
        }
    }

    // numstat for tracked changes (added/modified); untracked is absent there.
    let tracked: Vec<&str> = specs
        .iter()
        .flatten()
        .filter(|rel| matches!(classes.get(rel.as_str()), Some('A') | Some('M')))
        .map(String::as_str)
        .collect();
    let mut numstat: HashMap<String, (u64, u64)> = HashMap::new();
    if !tracked.is_empty() {
        let mut diff_args: Vec<&str> =
            vec!["--literal-pathspecs", "diff", "HEAD", "--numstat", "--"];
        diff_args.extend(tracked.iter().copied());
        let out = match git(root, &diff_args) {
            Ok(out) => out,
            Err(_) => {
                // Unborn HEAD: the index-to-be basis mirrors change_stats.
                let mut cached: Vec<&str> =
                    vec!["--literal-pathspecs", "diff", "--cached", "--numstat", "--"];
                cached.extend(tracked.iter().copied());
                git(root, &cached)?
            }
        };
        for line in String::from_utf8_lossy(&out).lines() {
            let mut parts = line.split('\t');
            let (Some(a), Some(d), Some(p)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            // Binary numstat columns are `-`, which parses to 0.
            numstat.insert(
                p.to_string(),
                (a.parse().unwrap_or(0), d.parse().unwrap_or(0)),
            );
        }
    }

    let mut files = Vec::with_capacity(paths.len());
    for (input, spec) in paths.iter().zip(specs.iter()) {
        let Some(rel) = spec else {
            files.push(TurnFileStat {
                path: input.clone(),
                status: "unavailable".into(),
                additions: 0,
                deletions: 0,
                additions_capped: false,
            });
            continue;
        };
        match classes.get(rel.as_str()) {
            Some('D') => files.push(TurnFileStat {
                path: input.clone(),
                status: "deleted".into(),
                additions: 0,
                deletions: 0,
                additions_capped: false,
            }),
            Some('U') => {
                let (additions, additions_capped) =
                    count_untracked_lines(&root.join(rel), deadline);
                files.push(TurnFileStat {
                    path: input.clone(),
                    status: "untracked".into(),
                    additions,
                    deletions: 0,
                    additions_capped,
                });
            }
            Some('A') | Some('M') => {
                let (additions, deletions) = numstat.get(rel.as_str()).copied().unwrap_or((0, 0));
                files.push(TurnFileStat {
                    path: input.clone(),
                    status: if classes.get(rel.as_str()) == Some(&'A') {
                        "added".into()
                    } else {
                        "modified".into()
                    },
                    additions,
                    deletions,
                    additions_capped: false,
                });
            }
            _ => files.push(TurnFileStat {
                // Clean (committed since the turn) or unmatched by git.
                path: input.clone(),
                status: "clean".into(),
                additions: 0,
                deletions: 0,
                additions_capped: false,
            }),
        }
    }

    Ok(TurnStats { files, dropped })
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let owned = args.iter().map(OsString::from).collect::<Vec<_>>();
    let (stdout, stderr, success) = run_git(git_command(root, owned), GIT_READ_DEADLINE)?;
    if success || args.contains(&"diff") && args.contains(&"--no-index") {
        Ok(stdout)
    } else {
        Err(String::from_utf8_lossy(&stderr).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn test_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let run = |args: &[&str]| {
            let output = StdCommand::new("git")
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.com"]);
        (dir, root)
    }

    fn test_commit(root: &Path, path: &str, content: &str, message: &str) -> String {
        std::fs::write(root.join(path), content).unwrap();
        let run = |args: &[&str]| {
            let output = StdCommand::new("git")
                .current_dir(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", message]);
        let output = StdCommand::new("git")
            .current_dir(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn maps_missing_git_to_stable_error_code() {
        let error = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(git_spawn_error(&error), GIT_NOT_FOUND);
    }

    #[test]
    fn preserves_non_missing_git_spawn_errors() {
        let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(git_spawn_error(&error), error.to_string());
        assert_ne!(git_spawn_error(&error), GIT_NOT_FOUND);
    }

    #[test]
    fn parses_diff_tree_name_status_z_flat_and_rename() {
        let files = parse_diff_tree_name_status_z(
            b"M\0a.txt\0A\0b.txt\0R100\0old.txt\0new.txt\0C75\0source\nname\0copy\"name\0\0",
        );
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].status, "M");
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[2].status, "R100");
        assert_eq!(files[2].path, "new.txt");
        assert_eq!(files[2].original_path.as_deref(), Some("old.txt"));
        assert_eq!(files[3].status, "C75");
        assert_eq!(files[3].original_path.as_deref(), Some("source\nname"));
        assert_eq!(files[3].path, "copy\"name");
    }

    #[test]
    fn parses_diff_tree_name_status_z_empty_fields_and_missing_paths() {
        let files = parse_diff_tree_name_status_z(b"M\0A\0R50\0only-one\0\0");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, "M");
        assert_eq!(files[0].path, "A");
    }

    #[test]
    fn validate_oid_rejects_injection_and_bad_shape() {
        assert!(validate_oid(&"0".repeat(40)).is_ok());
        assert!(validate_oid(&"0".repeat(64)).is_ok());
        assert!(validate_oid("; rm -rf /").is_err());
        assert!(validate_oid("abc").is_err());
        assert!(validate_oid(&"g".repeat(40)).is_err());
    }

    #[test]
    fn log_detail_and_commit_diff_preserve_empty_message_fields() {
        let (_dir, root) = test_repo();
        std::fs::write(root.join("line\nname.txt"), "one\n").unwrap();
        let output = StdCommand::new("git")
            .current_dir(&root)
            .args(["add", "-A"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = StdCommand::new("git")
            .current_dir(&root)
            .args(["commit", "-q", "--allow-empty-message", "-m", ""])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let root_oid = String::from_utf8(
            StdCommand::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let service = GitService::new();
        let page = service.log("owner", &root, 1, 50, None).unwrap();
        assert_eq!(page.commits.len(), 1);
        assert_eq!(page.commits[0].subject, "");
        let detail = service.log_detail("owner", &root, 1, &root_oid).unwrap();
        assert_eq!(detail.full_message, "");
        assert_eq!(detail.files[0].path, "line\nname.txt");
        let diff = service
            .commit_diff("owner", &root, 1, &root_oid, b"line\nname.txt")
            .unwrap();
        assert_eq!(diff.comparison, "commit");
        assert!(diff.raw_patch.contains("+one"));
        let response = serde_json::to_value(&diff).unwrap();
        assert!(response.get("snapshotId").is_none());
        assert!(service
            .commit_diff("owner", &root, 1, &root_oid, b"missing")
            .is_err());
    }

    #[test]
    fn log_detail_trims_git_format_newline_but_preserves_message_body() {
        let (_dir, root) = test_repo();
        let message_path = root.join("message.txt");
        std::fs::write(&message_path, "subject\n\nbody\n").unwrap();
        let output = StdCommand::new("git")
            .current_dir(&root)
            .args(["commit", "--allow-empty", "-q", "-F", "message.txt"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let oid = String::from_utf8(
            StdCommand::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let detail = GitService::new()
            .log_detail("owner", &root, 1, &oid)
            .unwrap();
        assert_eq!(detail.full_message, "subject\n\nbody");
        assert!(!detail.full_message.ends_with(['\r', '\n']));
    }

    #[test]
    fn log_detail_marks_oversized_message_as_truncated() {
        let (_dir, root) = test_repo();
        let message_path = root.join("message.txt");
        std::fs::write(
            &message_path,
            format!("{}\n", "x".repeat(MAX_LOG_MESSAGE_BYTES + 1)),
        )
        .unwrap();
        let output = StdCommand::new("git")
            .current_dir(&root)
            .args(["commit", "--allow-empty", "-q", "-F", "message.txt"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let oid = String::from_utf8(
            StdCommand::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let detail = GitService::new()
            .log_detail("owner", &root, 1, &oid)
            .unwrap();
        assert!(detail.message_truncated);
        assert!(detail.full_message.ends_with('…'));
    }

    #[test]
    fn log_detail_and_commit_diff_detect_real_rename() {
        let (_dir, root) = test_repo();
        test_commit(&root, "old.txt", "content\n", "add old");
        let output = StdCommand::new("git")
            .current_dir(&root)
            .args(["mv", "old.txt", "new.txt"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = StdCommand::new("git")
            .current_dir(&root)
            .args(["commit", "-q", "-m", "rename"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let oid = String::from_utf8(
            StdCommand::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let service = GitService::new();
        let detail = service.log_detail("owner", &root, 1, &oid).unwrap();
        assert_eq!(detail.files.len(), 1);
        assert!(detail.files[0].status.starts_with('R'));
        assert_eq!(detail.files[0].original_path.as_deref(), Some("old.txt"));
        assert_eq!(detail.files[0].path, "new.txt");
        let diff = service
            .commit_diff("owner", &root, 1, &oid, b"new.txt")
            .unwrap();
        assert_eq!(diff.fallback_reason.as_deref(), Some("rename"));
    }

    #[test]
    fn log_paginates_first_parent_and_stops_at_root() {
        let (_dir, root) = test_repo();
        let root_oid = test_commit(&root, "root.txt", "root\n", "root");
        let second_oid = test_commit(&root, "second.txt", "second\n", "second");
        let service = GitService::new();
        let first = service.log("owner", &root, 1, 1, None).unwrap();
        assert_eq!(first.commits[0].oid, second_oid);
        assert!(first.has_more);
        let second = service
            .log("owner", &root, 1, 1, Some(&second_oid))
            .unwrap();
        assert_eq!(second.commits[0].oid, root_oid);
        let end = service.log("owner", &root, 1, 1, Some(&root_oid)).unwrap();
        assert!(end.commits.is_empty());
        assert!(!end.has_more);
        let unknown = "0".repeat(40);
        assert!(service.log("owner", &root, 1, 1, Some(&unknown)).is_err());
    }

    #[test]
    fn log_detail_and_commit_diff_use_first_parent_for_merges() {
        let (_dir, root) = test_repo();
        test_commit(&root, "base.txt", "base\n", "base");
        let run = |args: &[&str]| {
            let output = StdCommand::new("git")
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["checkout", "-q", "-b", "side"]);
        test_commit(&root, "side.txt", "side\n", "side");
        run(&["checkout", "-q", "main"]);
        test_commit(&root, "main.txt", "main\n", "main");
        run(&["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        let merge_oid = String::from_utf8(
            StdCommand::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let service = GitService::new();
        let detail = service.log_detail("owner", &root, 1, &merge_oid).unwrap();
        let paths = detail
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&"side.txt"));
        assert!(!paths.contains(&"main.txt"));
        let diff = service
            .commit_diff("owner", &root, 1, &merge_oid, b"side.txt")
            .unwrap();
        assert!(diff.raw_patch.contains("+side"));
        assert!(service
            .commit_diff("owner", &root, 1, &merge_oid, b"main.txt")
            .is_err());
    }

    #[test]
    fn parses_paths_and_renames() {
        let p = parse_porcelain_v2_z(
            b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\x002 R. N... 100644 100644 100644 a a R100 new\0old\0? - weird\npath\0",
        );
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].original_display_path.as_deref(), Some("old"));
        assert_eq!(p.head_state, "attached");
        assert_eq!(p.branch.as_deref(), Some("main"));
        assert_eq!(p.upstream.as_deref(), Some("origin/main"));
        assert_eq!(p.ahead, Some(2));
        assert_eq!(p.behind, Some(1));
    }
    #[test]
    fn parses_unborn_branch() {
        let p = parse_porcelain_v2_z(b"# branch.oid (initial)\0# branch.head main\0");
        assert_eq!(p.head_state, "unborn");
        assert_eq!(p.branch.as_deref(), Some("main"));
    }

    #[test]
    fn parses_unmerged_stage_modes_and_object_ids() {
        let p = parse_porcelain_v2_z(
            b"u UU N... 100644 100755 100644 100644 deadbeef1111 deadbeef2222 deadbeef3333 conflict.txt\0",
        );
        let entry = &p.entries[0];
        assert_eq!(
            entry.unmerged_modes.as_deref(),
            Some(&["100644".into(), "100755".into(), "100644".into()][..])
        );
        assert_eq!(
            entry.unmerged_object_ids.as_deref(),
            Some(
                &[
                    "deadbeef1111".into(),
                    "deadbeef2222".into(),
                    "deadbeef3333".into()
                ][..]
            )
        );
    }
    #[test]
    fn stages_resolved_conflict_from_conflicted_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        std::fs::write(root.path().join("file.txt"), "base\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        assert!(run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "base",
        ])
        .status
        .success());
        assert!(run(&["checkout", "-b", "feature"]).status.success());
        std::fs::write(root.path().join("file.txt"), "feature\n").unwrap();
        assert!(run(&["commit", "-am", "feature"]).status.success());
        assert!(run(&["checkout", "-"]).status.success());
        std::fs::write(root.path().join("file.txt"), "main\n").unwrap();
        assert!(run(&["commit", "-am", "main"]).status.success());
        assert!(!run(&["merge", "feature"]).status.success());

        let service = GitService::new();
        let snapshot = service.status("owner", root.path(), 1).unwrap();
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.entry_kind == "unmerged")
            .unwrap();
        // Resolve the file in the editor, but keep the snapshot captured while
        // it was conflicted. Git Panel Stage must accept this transition.
        std::fs::write(root.path().join("file.txt"), "resolved\n").unwrap();
        service
            .write(
                &snapshot.snapshot_id,
                "owner",
                root.path(),
                1,
                &[GitPathIdentity {
                    group: "conflicted".into(),
                    path_bytes: BASE64.decode(&entry.path_bytes_base64).unwrap(),
                    original_path_bytes: None,
                }],
                "stage",
            )
            .unwrap();
        let after = service.status("owner", root.path(), 1).unwrap();
        assert_eq!(after.counts.conflicted, 0);
        assert_eq!(after.counts.staged, 1);
    }

    #[test]
    fn nested_workspace_is_rejected_for_subdirectory_root() {
        // The host-derived workspace root is `parent/sub`, but a git subprocess
        // whose cwd is `sub` would discover the parent repository. Git Panel
        // must refuse to operate so the user never stages or commits files
        // from a repository scope they did not authorize.
        let parent = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(parent.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        std::fs::write(parent.path().join("parent.txt"), "p\n").unwrap();
        let sub = parent.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("sub.txt"), "s\n").unwrap();
        // Stage files in the parent repo from the parent root so a naive
        // `git commit` from `sub` would sweep them in.
        assert!(run(&["add", "parent.txt"]).status.success());
        assert!(run(&["add", "sub/sub.txt"]).status.success());

        let service = GitService::new();
        let err = service
            .status("owner", &sub, 1)
            .expect_err("nested workspace status must be refused");
        assert!(
            err.contains("nested") || err.contains("not a git repository"),
            "expected nested-repository rejection, got: {err}"
        );
        let err = service
            .prepare_ai_snapshot("owner", &sub, 1)
            .expect_err("nested workspace AI snapshot must be refused");
        assert!(
            err.contains("nested") || err.contains("not a git repository"),
            "expected nested-repository rejection, got: {err}"
        );
        let err = service
            .prepare_commit("snap", "owner", &sub, 1, "m", None)
            .expect_err("nested workspace commit must be refused");
        assert!(
            err.contains("nested") || err.contains("not a git repository"),
            "expected nested-repository rejection, got: {err}"
        );
    }

    #[test]
    fn real_repo_stage_stale_and_discard_preserves_staged() {
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        std::fs::write(root.path().join("file.txt"), "one\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        assert!(run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial"
        ])
        .status
        .success());
        std::fs::write(root.path().join("file.txt"), "two\n").unwrap();
        let service = GitService::new();
        let before_stage = service.status("owner", root.path(), 1).unwrap();
        let entry = before_stage
            .entries
            .iter()
            .find(|entry| entry.display_path == "file.txt")
            .unwrap();
        let identity = GitPathIdentity {
            group: "changes".into(),
            path_bytes: b"file.txt".to_vec(),
            original_path_bytes: None,
        };
        service
            .write(
                &before_stage.snapshot_id,
                "owner",
                root.path(),
                1,
                &[identity],
                "stage",
            )
            .unwrap();
        std::fs::write(root.path().join("file.txt"), "three\n").unwrap();
        assert!(service
            .write(
                &before_stage.snapshot_id,
                "owner",
                root.path(),
                1,
                &[GitPathIdentity {
                    group: "changes".into(),
                    path_bytes: b"file.txt".to_vec(),
                    original_path_bytes: None
                }],
                "stage"
            )
            .is_err());
        let after_stage = service.status("owner", root.path(), 1).unwrap();
        let staged = after_stage
            .entries
            .iter()
            .find(|entry| entry.display_path == "file.txt")
            .unwrap();
        assert!(service.belongs_to_group(staged, "staged"));
        service
            .write(
                &after_stage.snapshot_id,
                "owner",
                root.path(),
                1,
                &[GitPathIdentity {
                    group: "changes".into(),
                    path_bytes: b"file.txt".to_vec(),
                    original_path_bytes: None,
                }],
                "discard",
            )
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("file.txt")).unwrap(),
            "two\n"
        );
        let _ = entry;
    }
    #[test]
    fn bounds_ai_diff_by_chars_files_and_lines() {
        let one_file = "diff --git a/a b/a\n".repeat(81);
        let (lines, lines_truncated) = bounded_ai_diff(one_file.as_bytes());
        assert!(lines_truncated);
        assert!(lines.lines().count() <= 80);

        let many_files = (0..81)
            .map(|index| format!("diff --git a/{index} b/{index}\n"))
            .collect::<String>();
        let (_, files_truncated) = bounded_ai_diff(many_files.as_bytes());
        assert!(files_truncated);

        let oversized = "x".repeat(24_001);
        let (chars, chars_truncated) = bounded_ai_diff(oversized.as_bytes());
        assert!(chars_truncated);
        assert!(chars.chars().count() <= 24_000);
    }
    #[test]
    fn partial_stage_commit_requires_confirmation_token() {
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        std::fs::write(root.path().join("file.txt"), "one\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        assert!(run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial"
        ])
        .status
        .success());
        std::fs::write(root.path().join("file.txt"), "two\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        std::fs::write(root.path().join("file.txt"), "three\n").unwrap();
        let service = GitService::new();
        let snapshot = service
            .prepare_ai_snapshot("owner", root.path(), 1)
            .unwrap();
        let error = service
            .prepare_commit(
                &snapshot.snapshot_id,
                "owner",
                root.path(),
                1,
                "test: partial",
                None,
            )
            .unwrap_err();
        let token = error
            .strip_prefix("confirmationRequired:")
            .unwrap()
            .to_string();
        assert!(service
            .prepare_commit(
                &snapshot.snapshot_id,
                "owner",
                root.path(),
                1,
                "test: partial",
                Some("wrong"),
            )
            .is_err());
        service
            .prepare_commit(
                &snapshot.snapshot_id,
                "owner",
                root.path(),
                1,
                "test: partial",
                Some(&token),
            )
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        service.commit_detached(
            snapshot.snapshot_id.clone(),
            "owner".into(),
            root.path().to_path_buf(),
            1,
            "req".into(),
            "test: partial".into(),
            Some(Box::new(move |frame| {
                let _ = tx.send(frame);
            })),
        );
        let frame = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            frame.contains("\"status\":\"succeeded\""),
            "frame was: {frame}"
        );
        let outcomes = service.take_pending_outcomes("owner", root.path(), 1);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, "succeeded");
        assert!(!outcomes[0].commit_oid.as_ref().unwrap().is_empty());
    }
    #[test]
    fn binary_diff_detected_as_fallback_reason() {
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        // Write a file with NUL bytes so Git treats it as binary.
        let binary_content = vec![b'G', b'I', b'F', 0u8, 1u8, 2u8, 3u8];
        std::fs::write(root.path().join("blob.bin"), &binary_content).unwrap();
        assert!(run(&["add", "blob.bin"]).status.success());
        assert!(run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial"
        ])
        .status
        .success());
        let modified = vec![b'G', b'I', b'F', 0u8, 9u8, 9u8, 9u8];
        std::fs::write(root.path().join("blob.bin"), &modified).unwrap();
        let service = GitService::new();
        let snapshot = service.status("owner", root.path(), 1).unwrap();
        let entry = snapshot
            .entries
            .iter()
            .find(|e| e.display_path == "blob.bin")
            .unwrap();
        let path_bytes = BASE64.decode(&entry.path_bytes_base64).unwrap();
        let diff = service
            .diff(
                &snapshot.snapshot_id,
                "owner",
                root.path(),
                1,
                "changes",
                &path_bytes,
                "changes",
            )
            .unwrap();
        assert_eq!(diff.fallback_reason.as_deref(), Some("binary"));
    }
    #[test]
    fn rejects_magic_paths() {
        let service = GitService::new();
        let root = tempfile::tempdir().unwrap();
        assert!(service
            .validate_path("missing", "owner", root.path(), 1, b":(glob)*")
            .is_err());
    }
    #[test]
    fn missing_snapshots_are_rejected_before_path_use() {
        let service = GitService::new();
        let root = tempfile::tempdir().unwrap();
        assert!(service
            .validate_path("missing", "owner", root.path(), 7, b"file")
            .is_err());
    }
    #[test]
    fn rejects_empty_write_batches() {
        let service = GitService::new();
        let root = tempfile::tempdir().unwrap();
        assert!(service
            .write("missing", "owner", root.path(), 1, &[], "stage")
            .is_err());
    }

    #[test]
    fn clearing_workspace_state_does_not_revoke_owner() {
        let service = GitService::new();
        service.clear_workspace_state("owner");
        assert!(
            !service.revoked_owners.lock().unwrap().contains("owner"),
            "a workspace transition must not revoke its still-live owner"
        );
    }

    #[test]
    fn detached_commit_captures_snapshot_before_waiting_for_write_slot() {
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        run(&["config", "core.hooksPath", ".git/hooks"]);
        std::fs::write(root.path().join("file.txt"), "one\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        assert!(run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial"
        ])
        .status
        .success());
        std::fs::write(root.path().join("file.txt"), "two\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        let service = GitService::new();
        let snapshot = service
            .prepare_ai_snapshot("owner", root.path(), 1)
            .unwrap();
        service
            .prepare_commit(&snapshot.snapshot_id, "owner", root.path(), 1, "test", None)
            .unwrap();
        let slot = service.lock_for_write(root.path());
        let guard = slot.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        service.commit_detached(
            snapshot.snapshot_id,
            "owner".into(),
            root.path().to_path_buf(),
            1,
            "req".into(),
            "test".into(),
            Some(Box::new(move |frame| {
                let _ = tx.send(frame);
            })),
        );
        std::thread::sleep(Duration::from_millis(20));
        service.clear_workspace_state("owner");
        drop(guard);
        let frame = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            frame.contains("\"status\":\"succeeded\""),
            "frame was: {frame}"
        );
    }

    #[test]
    fn detached_commit_keeps_expected_tree_after_workspace_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        run(&["config", "core.hooksPath", ".git/hooks"]);
        std::fs::write(root.path().join("file.txt"), "one\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        assert!(run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial"
        ])
        .status
        .success());
        let hook = root.path().join(".git/hooks/pre-commit");
        std::fs::create_dir_all(root.path().join(".git/hooks")).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nsleep 0.2\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(root.path().join("file.txt"), "two\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        let service = GitService::new();
        let snapshot = service
            .prepare_ai_snapshot("owner", root.path(), 1)
            .unwrap();
        service
            .prepare_commit(&snapshot.snapshot_id, "owner", root.path(), 1, "test", None)
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        service.commit_detached(
            snapshot.snapshot_id,
            "owner".into(),
            root.path().to_path_buf(),
            1,
            "req".into(),
            "test".into(),
            Some(Box::new(move |frame| {
                let _ = tx.send(frame);
            })),
        );
        std::thread::sleep(Duration::from_millis(50));
        service.clear_workspace_state("owner");
        let frame = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            frame.contains("\"status\":\"succeeded\""),
            "frame was: {frame}"
        );
        assert!(
            frame.contains("\"hookChangedTree\":false"),
            "workspace cleanup must not erase the detached commit expected tree: {frame}"
        );
    }

    #[test]
    fn revoked_owner_commit_outcome_is_dropped() {
        // A detached commit completing after owner revoke must not leave an
        // outcome in the registry.
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        std::fs::write(root.path().join("file.txt"), "one\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        let service = GitService::new();
        let snapshot = service
            .prepare_ai_snapshot("owner", root.path(), 1)
            .unwrap();
        service
            .prepare_commit(&snapshot.snapshot_id, "owner", root.path(), 1, "test", None)
            .unwrap();
        // Revoke the owner before the detached commit completes.
        service.clear_owner("owner");
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        service.commit_detached(
            snapshot.snapshot_id.clone(),
            "owner".into(),
            root.path().to_path_buf(),
            1,
            "req".into(),
            "test".into(),
            Some(Box::new(move |frame| {
                let _ = tx.send(frame);
            })),
        );
        let _frame = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // The outcome must NOT be recoverable — it was dropped because the
        // owner is revoked.
        let outcomes = service.take_pending_outcomes("owner", root.path(), 1);
        assert!(outcomes.is_empty(), "revoked owner outcome must be dropped");
    }

    #[test]
    fn commit_failure_preserves_hook_stderr() {
        // A pre-commit hook that rejects the commit must surface its stderr
        // in the failed outcome so the user can diagnose and retry.
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        // Ensure the repo-local hooks dir is used (the test host may have a
        // global core.hooksPath that would bypass .git/hooks).
        run(&["config", "core.hooksPath", ".git/hooks"]);
        std::fs::write(root.path().join("file.txt"), "one\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        // Make the initial commit before installing the failing hook.
        assert!(run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial"
        ])
        .status
        .success());
        // Install a failing pre-commit hook for the second commit.
        let hook = root.path().join(".git/hooks/pre-commit");
        std::fs::create_dir_all(root.path().join(".git/hooks")).unwrap();
        std::fs::write(&hook, "#!/bin/sh\necho 'hook says no' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Now stage a change the failing pre-commit hook will reject.
        std::fs::write(root.path().join("file.txt"), "two\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        let service = GitService::new();
        let snapshot = service
            .prepare_ai_snapshot("owner", root.path(), 1)
            .unwrap();
        service
            .prepare_commit(&snapshot.snapshot_id, "owner", root.path(), 1, "test", None)
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        service.commit_detached(
            snapshot.snapshot_id.clone(),
            "owner".into(),
            root.path().to_path_buf(),
            1,
            "req".into(),
            "test".into(),
            Some(Box::new(move |frame| {
                let _ = tx.send(frame);
            })),
        );
        let frame = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            frame.contains("\"status\":\"failed\""),
            "expected failed, got: {frame}"
        );
        assert!(
            frame.contains("hook says no"),
            "expected hook stderr preserved, got: {frame}"
        );
    }

    #[test]
    fn per_root_write_locks_do_not_block_other_workspaces() {
        // Different canonical roots get independent write locks, so a write
        // to workspace A must not make a concurrent write to workspace B
        // return "busy".
        let service = GitService::new();
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let slot_a = service.lock_for_write(root_a.path());
        let _guard_a = slot_a.lock().unwrap();
        // Acquiring the lock for root B must succeed even though root A is held.
        let slot_b = service.lock_for_write(root_b.path());
        let guard_b = slot_b.try_lock();
        assert!(
            guard_b.is_ok(),
            "different workspace root should not be blocked"
        );
    }

    #[test]
    fn commit_detached_rejects_stale_index_after_prepare() {
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        std::fs::write(root.path().join("file.txt"), "one\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        assert!(run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial"
        ])
        .status
        .success());
        std::fs::write(root.path().join("file.txt"), "two\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        let service = GitService::new();
        let snapshot = service
            .prepare_ai_snapshot("owner", root.path(), 1)
            .unwrap();
        service
            .prepare_commit(
                &snapshot.snapshot_id,
                "owner",
                root.path(),
                1,
                "test: commit",
                None,
            )
            .unwrap();
        // Mutate the index after prepare_commit but before commit_detached runs,
        // simulating a concurrent stage that changes the index tree.
        std::fs::write(root.path().join("other.txt"), "x\n").unwrap();
        assert!(run(&["add", "other.txt"]).status.success());
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        service.commit_detached(
            snapshot.snapshot_id.clone(),
            "owner".into(),
            root.path().to_path_buf(),
            1,
            "req".into(),
            "test: commit".into(),
            Some(Box::new(move |frame| {
                let _ = tx.send(frame);
            })),
        );
        let frame = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            frame.contains("\"status\":\"failed\""),
            "expected stale rejection, got: {frame}"
        );
    }

    #[test]
    fn commit_detached_succeeds_for_unborn_repository() {
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(root.path())
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&["init"]).status.success());
        std::fs::write(root.path().join("file.txt"), "one\n").unwrap();
        assert!(run(&["add", "file.txt"]).status.success());
        let service = GitService::new();
        let snapshot = service
            .prepare_ai_snapshot("owner", root.path(), 1)
            .unwrap();
        service
            .prepare_commit(
                &snapshot.snapshot_id,
                "owner",
                root.path(),
                1,
                "test: unborn",
                None,
            )
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        service.commit_detached(
            snapshot.snapshot_id.clone(),
            "owner".into(),
            root.path().to_path_buf(),
            1,
            "req".into(),
            "test: unborn".into(),
            Some(Box::new(move |frame| {
                let _ = tx.send(frame);
            })),
        );
        let frame = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            frame.contains("\"status\":\"succeeded\""),
            "expected succeeded for unborn first commit, got: {frame}"
        );
        // The unborn repo should now have a HEAD.
        assert!(root.path().join(".git/HEAD").exists());
    }

    // ── turn_stats (2026-09-19 turn files card spec) ──

    fn git_ok(root: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn stats_of(root: &Path, paths: &[String]) -> TurnStats {
        turn_stats(root, paths).unwrap()
    }

    fn stat_for<'a>(stats: &'a TurnStats, path: &str) -> &'a TurnFileStat {
        stats
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("no stat for {path}"))
    }

    fn abs(root: &Path, relative: &str) -> String {
        root.join(relative).to_string_lossy().into_owned()
    }

    #[test]
    fn turn_stats_classifies_porcelain_states() {
        let (_dir, root) = test_repo();
        test_commit(&root, "kept.ts", "let a = 1;\n", "init");
        test_commit(&root, "gone.ts", "remove me\n", "second");
        std::fs::write(root.join("kept.ts"), "let a = 2;\nlet b = 3;\n").unwrap();
        std::fs::write(root.join("added.ts"), "new file\n").unwrap();
        git_ok(&root, &["add", "added.ts"]);
        std::fs::write(root.join("untracked.ts"), "one\ntwo\n").unwrap();
        std::fs::remove_file(root.join("gone.ts")).unwrap();
        // Narrow pathspec on purpose: `git add -A` would stage untracked.ts too.
        git_ok(&root, &["add", "gone.ts"]);

        let stats = stats_of(
            &root,
            &[
                abs(&root, "kept.ts"),
                abs(&root, "added.ts"),
                abs(&root, "untracked.ts"),
                abs(&root, "gone.ts"),
            ],
        );

        assert_eq!(stat_for(&stats, &abs(&root, "kept.ts")).status, "modified");
        assert!(stat_for(&stats, &abs(&root, "kept.ts")).additions >= 2);
        assert_eq!(stat_for(&stats, &abs(&root, "added.ts")).status, "added");
        let untracked = stat_for(&stats, &abs(&root, "untracked.ts"));
        assert_eq!(untracked.status, "untracked");
        assert_eq!(untracked.additions, 2);
        assert_eq!(stat_for(&stats, &abs(&root, "gone.ts")).status, "deleted");
        assert_eq!(stats.files.len(), 4);
        assert_eq!(stats.dropped, 0);
    }

    #[test]
    fn turn_stats_reports_rename_as_modified() {
        let (_dir, root) = test_repo();
        test_commit(&root, "before.ts", "line\n", "init");
        git_ok(&root, &["mv", "before.ts", "after.ts"]);

        // Both paths are requested because git only pairs a rename when the
        // pathspec covers the pair; the target is modified, the source gone.
        let stats = stats_of(&root, &[abs(&root, "after.ts"), abs(&root, "before.ts")]);
        assert_eq!(stat_for(&stats, &abs(&root, "after.ts")).status, "modified");
        assert_eq!(stat_for(&stats, &abs(&root, "before.ts")).status, "deleted");
        assert_eq!(stats.files.len(), 2);
    }

    #[test]
    fn turn_stats_prefers_deleted_over_added() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        std::fs::write(root.join("fresh.ts"), "temp\n").unwrap();
        git_ok(&root, &["add", "fresh.ts"]);
        std::fs::remove_file(root.join("fresh.ts")).unwrap();

        let stats = stats_of(&root, &[abs(&root, "fresh.ts")]);
        assert_eq!(stat_for(&stats, &abs(&root, "fresh.ts")).status, "deleted");
    }

    #[test]
    fn turn_stats_matches_paths_with_spaces_and_cjk() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        let relative = "my docs/报告 1.md";
        std::fs::create_dir_all(root.join("my docs")).unwrap();
        std::fs::write(root.join(relative), "one\ntwo\nthree\n").unwrap();

        let stats = stats_of(&root, &[abs(&root, relative)]);
        let stat = stat_for(&stats, &abs(&root, relative));
        assert_eq!(stat.status, "untracked");
        assert_eq!(stat.additions, 3);
    }

    #[test]
    fn turn_stats_falls_back_to_cached_numstat_on_unborn_head() {
        let (_dir, root) = test_repo();
        std::fs::write(root.join("first.ts"), "a\nb\nc\n").unwrap();
        git_ok(&root, &["add", "first.ts"]);

        let stats = stats_of(&root, &[abs(&root, "first.ts")]);
        let stat = stat_for(&stats, &abs(&root, "first.ts"));
        assert_eq!(stat.status, "added");
        assert_eq!(stat.additions, 3);
    }

    #[test]
    fn turn_stats_reports_escape_paths_as_unavailable() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        let sibling = format!("{}-evil/other.ts", root.to_string_lossy());
        let parent = root
            .join("..")
            .join("outside.ts")
            .to_string_lossy()
            .into_owned();

        let stats = stats_of(&root, &[sibling.clone(), parent.clone()]);
        assert_eq!(stat_for(&stats, &sibling).status, "unavailable");
        assert_eq!(stat_for(&stats, &parent).status, "unavailable");
        assert_eq!(stats.files.len(), 2);
        assert_eq!(stats.dropped, 2);
    }

    #[test]
    fn turn_stats_accepts_relative_paths_and_rejects_parent_dir() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        std::fs::write(root.join("loose.ts"), "x\n").unwrap();

        let stats = stats_of(&root, &["loose.ts".to_string(), "../escape.ts".to_string()]);
        assert_eq!(stat_for(&stats, "loose.ts").status, "untracked");
        assert_eq!(stat_for(&stats, "../escape.ts").status, "unavailable");
        assert_eq!(stats.dropped, 1);
    }

    #[test]
    fn turn_stats_accepts_canonical_root_spelling() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        std::fs::write(root.join("edited.ts"), "x\n").unwrap();
        git_ok(&root, &["add", "edited.ts"]);
        let canonical = std::fs::canonicalize(&root).unwrap();
        let spelled = canonical.join("edited.ts").to_string_lossy().into_owned();

        let stats = stats_of(&root, std::slice::from_ref(&spelled));
        assert_eq!(stat_for(&stats, &spelled).status, "added");
        assert_eq!(stats.dropped, 0);
    }

    #[test]
    fn turn_stats_skips_binary_and_counts_unterminated_lines() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        std::fs::write(root.join("blob.bin"), b"a\0b\nc\n").unwrap();
        std::fs::write(root.join("no-newline.txt"), "one\ntwo").unwrap();

        let stats = stats_of(
            &root,
            &[abs(&root, "blob.bin"), abs(&root, "no-newline.txt")],
        );
        let blob = stat_for(&stats, &abs(&root, "blob.bin"));
        assert_eq!(blob.status, "untracked");
        assert_eq!(blob.additions, 0);
        assert!(!blob.additions_capped);
        let text = stat_for(&stats, &abs(&root, "no-newline.txt"));
        assert_eq!(text.additions, 2);
        assert!(!text.additions_capped);
    }

    #[test]
    fn turn_stats_caps_large_untracked_files() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        let mut contents = String::with_capacity(600_000);
        for _ in 0..300_000 {
            contents.push_str("x\n");
        }
        std::fs::write(root.join("big.txt"), contents).unwrap();

        let stats = stats_of(&root, &[abs(&root, "big.txt")]);
        let stat = stat_for(&stats, &abs(&root, "big.txt"));
        assert!(stat.additions_capped, "expected capped: {stat:?}");
        assert!(stat.additions >= TURN_STATS_MAX_UNTRACKED_LINES);
    }

    #[test]
    fn turn_stats_rejects_non_git_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let error = turn_stats(dir.path(), &["a.ts".to_string()]).unwrap_err();
        assert!(error.contains("git"), "unexpected error: {error}");
    }

    #[test]
    fn turn_stats_rejects_oversized_input() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        let many: Vec<String> = (0..=TURN_STATS_MAX_PATHS)
            .map(|index| format!("path-{index}.ts"))
            .collect();
        assert!(turn_stats(&root, &many).is_err());
        let long = "a".repeat(TURN_STATS_MAX_PATH_CHARS + 1);
        assert!(turn_stats(&root, &[long]).is_err());
    }

    #[test]
    fn turn_stats_preserves_input_order_and_length() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        std::fs::write(root.join("loose.ts"), "x\n").unwrap();
        let first = abs(&root, "seed.ts");
        let second = "../outside.ts".to_string();
        let third = abs(&root, "loose.ts");

        let stats = stats_of(&root, &[first.clone(), second.clone(), third.clone()]);
        assert_eq!(stats.files.len(), 3);
        assert_eq!(stats.files[0].path, first);
        assert_eq!(stats.files[1].path, second);
        assert_eq!(stats.files[2].path, third);
        assert_eq!(stats.dropped, 1);
    }

    #[test]
    fn turn_stats_does_not_cap_a_file_that_ends_at_the_read_limit() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        // One padded line filling exactly the byte cap: the count is exact.
        let mut contents = "x".repeat(TURN_STATS_MAX_UNTRACKED_BYTES - 1);
        contents.push('\n');
        std::fs::write(root.join("exact.txt"), &contents).unwrap();
        assert_eq!(contents.len(), TURN_STATS_MAX_UNTRACKED_BYTES);

        let stats = stats_of(&root, &[abs(&root, "exact.txt")]);
        let stat = stat_for(&stats, &abs(&root, "exact.txt"));
        assert!(!stat.additions_capped, "unexpected cap: {stat:?}");
        assert_eq!(stat.additions, 1);
    }

    #[test]
    fn turn_stats_caps_one_byte_past_the_read_limit() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        let mut contents = "x".repeat(TURN_STATS_MAX_UNTRACKED_BYTES - 1);
        contents.push('\n');
        contents.push('y');
        std::fs::write(root.join("over.txt"), &contents).unwrap();

        let stats = stats_of(&root, &[abs(&root, "over.txt")]);
        let stat = stat_for(&stats, &abs(&root, "over.txt"));
        assert!(stat.additions_capped, "expected a cap: {stat:?}");
        assert_eq!(stat.additions, 1);
    }

    #[cfg(unix)]
    #[test]
    fn turn_stats_does_not_follow_untracked_symlinks() {
        let (_dir, root) = test_repo();
        test_commit(&root, "seed.ts", "seed\n", "init");
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("target.txt"), "a\nb\nc\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("target.txt"), root.join("link.txt"))
            .unwrap();

        let stats = stats_of(&root, &[abs(&root, "link.txt")]);
        let stat = stat_for(&stats, &abs(&root, "link.txt"));
        assert_eq!(stat.status, "untracked");
        assert_eq!(stat.additions, 0);
        assert!(!stat.additions_capped);
    }

    #[test]
    fn push_rejects_a_detached_head() {
        let (_dir, root) = test_repo();
        test_commit(&root, "file.txt", "one\n", "init");
        let service = GitService::new();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap()
        };
        let head = String::from_utf8(run(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        assert!(run(&["checkout", "-q", &head]).status.success());
        assert_eq!(
            service.push(&root).unwrap_err(),
            PUSH_DETACHED_HEAD.to_string()
        );
    }

    #[test]
    fn push_reports_a_workspace_without_a_remote() {
        let (_dir, root) = test_repo();
        test_commit(&root, "file.txt", "one\n", "init");
        let service = GitService::new();
        assert_eq!(service.push(&root).unwrap_err(), PUSH_NO_REMOTE.to_string());
    }

    #[test]
    fn push_publishes_the_branch_then_reuses_the_upstream() {
        let (_dir, root) = test_repo();
        test_commit(&root, "file.txt", "one\n", "init");
        let remote = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap()
        };
        assert!(Command::new("git")
            .current_dir(remote.path())
            .args(["init", "-q", "--bare", "-b", "main"])
            .output()
            .unwrap()
            .status
            .success());
        assert!(
            run(&["remote", "add", "origin", remote.path().to_str().unwrap()])
                .status
                .success()
        );

        let service = GitService::new();
        // No upstream yet: the first push publishes and records one.
        let first = service.push(&root).unwrap();
        assert_eq!(first.remote, "origin");
        assert_eq!(first.branch, "main");
        assert!(first.set_upstream);

        // The recorded upstream is reused, so nothing is re-published.
        let second = service.push(&root).unwrap();
        assert_eq!(second.remote, "origin");
        assert!(!second.set_upstream);
    }

    #[test]
    fn push_refuses_to_run_while_the_write_slot_is_held() {
        let (_dir, root) = test_repo();
        test_commit(&root, "file.txt", "one\n", "init");
        let service = GitService::new();
        let slot = service.lock_for_write(&root);
        let _guard = slot.try_lock().unwrap();
        assert_eq!(service.push(&root).unwrap_err(), "busy".to_string());
    }
    #[test]
    fn push_updates_the_tracked_upstream_branch_when_names_differ() {
        let (_dir, root) = test_repo();
        test_commit(&root, "file.txt", "one\n", "init");
        let remote = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap()
        };
        assert!(Command::new("git")
            .current_dir(remote.path())
            .args(["init", "-q", "--bare", "-b", "main"])
            .output()
            .unwrap()
            .status
            .success());
        let remote_path = remote.path().to_str().unwrap();
        assert!(run(&["remote", "add", "origin", remote_path])
            .status
            .success());
        // The remote branch is named differently from the local one.
        assert!(run(&["push", "origin", "HEAD:refs/heads/other"])
            .status
            .success());
        assert!(run(&["fetch", "-q", "origin"]).status.success());
        assert!(run(&["branch", "-q", "-u", "origin/other", "main"])
            .status
            .success());
        test_commit(&root, "file.txt", "two\n", "second");

        let service = GitService::new();
        let outcome = service.push(&root).unwrap();
        assert_eq!(outcome.remote, "origin");
        assert_eq!(outcome.branch, "main");
        assert!(!outcome.set_upstream);

        let heads =
            String::from_utf8(run(&["ls-remote", "origin", "refs/heads/*"]).stdout).unwrap();
        // A same-named branch must not appear: that would mean the local name
        // was published and the real upstream left behind.
        assert!(!heads.contains("refs/heads/main"), "{heads}");
        let local = String::from_utf8(run(&["rev-parse", "HEAD"]).stdout).unwrap();
        let upstream = String::from_utf8(run(&["rev-parse", "origin/other"]).stdout).unwrap();
        assert_eq!(local.trim(), upstream.trim());
    }

    #[test]
    fn push_handles_a_branch_name_that_looks_like_a_git_switch() {
        let (_dir, root) = test_repo();
        test_commit(&root, "file.txt", "one\n", "init");
        let remote = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap()
        };
        assert!(Command::new("git")
            .current_dir(remote.path())
            .args(["init", "-q", "--bare", "-b", "main"])
            .output()
            .unwrap()
            .status
            .success());
        let remote_path = remote.path().to_str().unwrap();
        assert!(run(&["remote", "add", "origin", remote_path])
            .status
            .success());
        // `-evil` is a legal ref name; a refspec built from it would be read as
        // a git switch without the `--` guard.
        assert!(run(&["update-ref", "refs/heads/-evil", "HEAD"])
            .status
            .success());
        assert!(run(&["symbolic-ref", "HEAD", "refs/heads/-evil"])
            .status
            .success());

        let service = GitService::new();
        let outcome = service.push(&root).unwrap();
        assert!(outcome.set_upstream);

        let heads =
            String::from_utf8(run(&["ls-remote", "origin", "refs/heads/*"]).stdout).unwrap();
        assert!(heads.contains("refs/heads/-evil"), "{heads}");
    }
}
