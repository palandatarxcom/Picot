// ABOUTME: Owner-scoped admission and wire dispatch for the four subagents host controls.
// ABOUTME: Project identity comes only from the live owner registry; caller paths are rejected.
use crate::subagents_inventory::{self, Inventory};
use crate::window_owner::OwnerWorkspaceSnapshot;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentsError {
    UnauthorizedTarget(&'static str),
    NotRegistered,
    ProjectUntrusted,
    ProjectRootMismatch,
    StaleGeneration,
    CandidateStale,
    ShadowedCandidate,
    ExternalRunner,
    RevisionConflict,
    InventoryRevisionConflict,
    WriteEligibilityUnknown,
    InvalidDefinition(String),
    ConfigBusy,
    WriteFailed(String),
    ConfigUnavailable(String),
    UnknownOperation,
}

impl SubagentsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnauthorizedTarget(_) => "unauthorized_target",
            Self::NotRegistered => "not_registered",
            Self::ProjectUntrusted => "project_untrusted",
            Self::ProjectRootMismatch => "project_root_mismatch",
            Self::StaleGeneration => "stale_generation",
            Self::CandidateStale => "candidate_stale",
            Self::ShadowedCandidate => "shadowed_candidate",
            Self::ExternalRunner => "external_runner",
            Self::RevisionConflict => "revision_conflict",
            Self::InventoryRevisionConflict => "inventory_revision_conflict",
            Self::WriteEligibilityUnknown => "write_eligibility_unknown",
            Self::InvalidDefinition(_) => "invalid_definition",
            Self::ConfigBusy => "config_busy",
            Self::WriteFailed(_) => "write_failed",
            Self::ConfigUnavailable(_) => "config_unavailable",
            Self::UnknownOperation => "unknown_operation",
        }
    }

    /// Redacted: stable text only, never filesystem paths or definition content.
    pub fn message(&self) -> String {
        match self {
            Self::UnauthorizedTarget(reason) => {
                format!("Caller-supplied identity rejected: {reason}")
            }
            Self::NotRegistered => "Registered desktop workspace required".into(),
            Self::ProjectUntrusted => "Project is not trusted for this owner".into(),
            Self::ProjectRootMismatch => {
                "Extension project root differs from the workspace; project writes disabled".into()
            }
            Self::StaleGeneration => "Workspace binding changed; refresh and retry".into(),
            Self::CandidateStale => "Candidate no longer matches the current scan".into(),
            Self::ShadowedCandidate => {
                "Candidate is shadowed by another definition; overrides are read-only".into()
            }
            Self::ExternalRunner => "External runner definitions cannot be overridden here".into(),
            Self::RevisionConflict => "Settings changed on disk; refresh and retry".into(),
            Self::InventoryRevisionConflict => {
                "Agent inventory changed; refresh, reconfirm, and retry".into()
            }
            Self::WriteEligibilityUnknown => {
                "In-scope discovery incomplete or candidate disabled; write refused".into()
            }
            Self::InvalidDefinition(reason) => format!("Invalid override payload: {reason}"),
            Self::ConfigBusy => "Settings are locked by another writer; retry shortly".into(),
            Self::WriteFailed(reason) => format!("Settings write failed: {reason}"),
            Self::ConfigUnavailable(reason) => {
                format!("Subagent configuration unavailable: {reason}")
            }
            Self::UnknownOperation => "Unknown subagents operation".into(),
        }
    }
}

/// The one scope the host authorizes for a request. Identity is host-derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedScope {
    pub project: bool,
    pub workspace: Option<WorkspaceBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceBinding {
    pub wid: String,
    pub generation: u64,
    pub root: PathBuf,
}

const CALLER_PATH_KEYS: [&str; 6] = [
    "agentRoot",
    "workspaceRoot",
    "projectRoot",
    "path",
    "filePath",
    "root",
];

/// Admission shared by inventory, detail, create and override. The frame may
/// carry scope labels and revision tokens, never roots or owner identity.
pub fn authorize_scope(
    args: &Value,
    snapshot: &OwnerWorkspaceSnapshot,
    agent_root: &Path,
) -> Result<AuthorizedScope, SubagentsError> {
    for key in CALLER_PATH_KEYS {
        if args.get(key).is_some_and(|value| !value.is_null()) {
            return Err(SubagentsError::UnauthorizedTarget("root or path argument"));
        }
    }
    match args.get("scope").and_then(Value::as_str) {
        Some("global") => {
            if args
                .get("workspaceId")
                .is_some_and(|value| !value.is_null())
                || args
                    .get("workspaceGeneration")
                    .is_some_and(|value| !value.is_null())
            {
                return Err(SubagentsError::UnauthorizedTarget(
                    "project identity on global scope",
                ));
            }
            Ok(AuthorizedScope {
                project: false,
                workspace: None,
            })
        }
        Some("project") => {
            let OwnerWorkspaceSnapshot::Registered {
                wid,
                root,
                generation,
            } = snapshot
            else {
                return Err(SubagentsError::NotRegistered);
            };
            match args.get("workspaceId").and_then(Value::as_str) {
                Some(requested) if requested == wid => {}
                Some(_) => {
                    return Err(SubagentsError::UnauthorizedTarget(
                        "workspaceId does not match the owner binding",
                    ))
                }
                None => {
                    return Err(SubagentsError::UnauthorizedTarget(
                        "workspaceId is required for project scope",
                    ))
                }
            }
            match args.get("workspaceGeneration").and_then(Value::as_u64) {
                Some(requested) if requested == *generation => {}
                _ => return Err(SubagentsError::StaleGeneration),
            }
            if !crate::project_trust::is_project_trusted(agent_root, root) {
                return Err(SubagentsError::ProjectUntrusted);
            }
            Ok(AuthorizedScope {
                project: true,
                workspace: Some(WorkspaceBinding {
                    wid: wid.clone(),
                    generation: *generation,
                    root: root.clone(),
                }),
            })
        }
        _ => Err(SubagentsError::UnauthorizedTarget("scope is required")),
    }
}

/// Parity evidence from the checked-in spike (scripts/subagents-parity-spike.mjs).
/// Current verdict: disk-candidates-only — no winner assertion; scoped name writes allowed.
fn parity_evidence() -> subagents_inventory::ParityEvidence {
    subagents_inventory::ParityEvidence::default()
}

pub fn handle_inventory(
    args: &Value,
    snapshot: &OwnerWorkspaceSnapshot,
    agent_root: &Path,
) -> Result<Inventory, SubagentsError> {
    let scope = authorize_scope(args, snapshot, agent_root)?;
    let workspace = scope
        .workspace
        .as_ref()
        .map(|binding| binding.root.as_path());
    subagents_inventory::inventory(agent_root, workspace, parity_evidence())
        .map_err(SubagentsError::ConfigUnavailable)
}

pub fn handle_detail(
    args: &Value,
    snapshot: &OwnerWorkspaceSnapshot,
    agent_root: &Path,
) -> Result<Value, SubagentsError> {
    let scope = authorize_scope(args, snapshot, agent_root)?;
    let workspace = scope
        .workspace
        .as_ref()
        .map(|binding| binding.root.as_path());
    let fresh = subagents_inventory::inventory(agent_root, workspace, parity_evidence())
        .map_err(SubagentsError::ConfigUnavailable)?;
    let candidate_id = args
        .get("candidateId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(SubagentsError::CandidateStale)?;
    let raw = subagents_inventory::detail(&fresh, candidate_id).map_err(|code| {
        if code == "candidate_stale" {
            SubagentsError::CandidateStale
        } else {
            SubagentsError::ConfigUnavailable(format!("detail failed: {code}"))
        }
    })?;
    Ok(json!({
        "candidateId": candidate_id,
        "rawDefinition": raw,
        "diagnostic": Value::Null,
    }))
}

const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

fn op_kind(action: &Value) -> Result<&str, String> {
    action
        .get("op")
        .and_then(Value::as_str)
        .filter(|op| matches!(*op, "set" | "clear" | "keep"))
        .ok_or_else(|| "op must be set, clear, or keep".to_string())
}

fn validate_model_value(value: &Value) -> Result<(), String> {
    // No live model catalog is available to the host; apply the strict
    // nonempty provider/model-id shape and let the client surface the
    // runtime-confirmation notice instead of presenting it as validated.
    value
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && !id.contains(char::is_whitespace)
                && !id.chars().any(|c| c.is_control())
                && id.contains('/')
        })
        .ok_or_else(|| "model value must be a nonempty provider/model id".to_string())?;
    Ok(())
}

fn validate_thinking_value(value: &Value) -> Result<(), String> {
    match value {
        Value::Bool(false) => Ok(()),
        Value::String(level) if THINKING_LEVELS.contains(&level.as_str()) => Ok(()),
        _ => Err(format!(
            "thinking value must be false or one of {}",
            THINKING_LEVELS.join("/")
        )),
    }
}

fn validate_override_actions(actions: [(&str, &Value); 4]) -> Result<(), String> {
    for (field, action) in actions {
        if op_kind(action)? == "set" {
            let selected = action.get("value").unwrap_or(&Value::Null);
            match field {
                "model" => validate_model_value(selected)?,
                "thinking" => validate_thinking_value(selected)?,
                _ if !selected.is_boolean() => {
                    return Err(format!("{field} value must be a boolean"))
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Pure settings-tree edit for one runtime name's four supported overrides.
/// `keep` never touches the field (preserving `false`, `"inherit"`, and
/// unknown fields); fully-empty entries and layers are pruned, everything
/// else survives untouched.
pub fn apply_override_edit_four(
    value: &mut Value,
    runtime_name: &str,
    model: &Value,
    thinking: &Value,
    advertise: &Value,
    disabled: &Value,
) -> Result<(), String> {
    if runtime_name.is_empty() || runtime_name.contains('/') || runtime_name.contains('\\') {
        return Err("runtimeName must be a single path-free segment".into());
    }
    let actions = [
        ("model", model),
        ("thinking", thinking),
        ("advertise", advertise),
        ("disabled", disabled),
    ];
    validate_override_actions(actions)?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| "settings must be an object".to_string())?;
    let subagents = root.entry("subagents").or_insert_with(|| json!({}));
    let subagents_map = subagents
        .as_object_mut()
        .ok_or_else(|| "subagents must be an object".to_string())?;
    let overrides = subagents_map
        .entry("agentOverrides")
        .or_insert_with(|| json!({}));
    let overrides_map = overrides
        .as_object_mut()
        .ok_or_else(|| "agentOverrides must be an object".to_string())?;
    let entry = overrides_map
        .entry(runtime_name.to_string())
        .or_insert_with(|| json!({}));
    let entry_map = entry
        .as_object_mut()
        .ok_or_else(|| "existing override entry is not an object".to_string())?;
    for (field, action) in actions {
        match action["op"].as_str() {
            Some("set") => {
                entry_map.insert(field.into(), action["value"].clone());
            }
            Some("clear") => {
                entry_map.remove(field);
            }
            _ => {}
        }
    }
    if entry_map.is_empty() {
        overrides_map.remove(runtime_name);
    }
    if overrides_map.is_empty() {
        subagents_map.remove("agentOverrides");
    }
    if subagents_map.is_empty() {
        root.remove("subagents");
    }
    Ok(())
}

#[cfg(test)]
pub fn apply_override_edit(
    value: &mut Value,
    runtime_name: &str,
    model: &Value,
    thinking: &Value,
) -> Result<(), String> {
    let keep = json!({"op": "keep"});
    apply_override_edit_four(value, runtime_name, model, thinking, &keep, &keep)
}

fn map_config_error(error: crate::host_config::ConfigError) -> SubagentsError {
    match error {
        crate::host_config::ConfigError::Busy => SubagentsError::ConfigBusy,
        crate::host_config::ConfigError::RevisionConflict => SubagentsError::RevisionConflict,
        other => SubagentsError::WriteFailed(other.code().to_string()),
    }
}

fn target_settings(scope: &AuthorizedScope, agent_root: &Path) -> (PathBuf, PathBuf) {
    match &scope.workspace {
        Some(binding) => (binding.root.join(".pi/settings.json"), binding.root.clone()),
        None => (agent_root.join("settings.json"), agent_root.to_path_buf()),
    }
}

fn fresh_inventory(
    scope: &AuthorizedScope,
    agent_root: &Path,
) -> Result<Inventory, SubagentsError> {
    let workspace = scope
        .workspace
        .as_ref()
        .map(|binding| binding.root.as_path());
    subagents_inventory::inventory(agent_root, workspace, parity_evidence())
        .map_err(SubagentsError::ConfigUnavailable)
}

/// Recheck current disk-snapshot qualification; never trust a caller-supplied flag.
fn write_gate(
    fresh: &Inventory,
    candidate: &crate::subagents_inventory::Candidate,
) -> Result<(), SubagentsError> {
    if candidate
        .parsed_fields
        .get("runner")
        .and_then(Value::as_str)
        != Some("native")
    {
        return Err(SubagentsError::ExternalRunner);
    }
    if subagents_inventory::collision(&fresh.entries, candidate).is_some() {
        return Err(SubagentsError::ShadowedCandidate);
    }
    if !candidate.write_qualified {
        return Err(SubagentsError::WriteEligibilityUnknown);
    }
    Ok(())
}

pub fn set_override(
    args: &Value,
    snapshot: &OwnerWorkspaceSnapshot,
    agent_root: &Path,
) -> Result<Value, SubagentsError> {
    let scope = authorize_scope(args, snapshot, agent_root)?;
    let fresh = fresh_inventory(&scope, agent_root)?;
    if scope.project && !fresh.resolution_context.project_writes_allowed {
        return Err(SubagentsError::ProjectRootMismatch);
    }
    let candidate_id = args
        .get("candidateId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(SubagentsError::CandidateStale)?;
    let candidate = subagents_inventory::resolve_candidate(&fresh, candidate_id)
        .ok_or(SubagentsError::CandidateStale)?
        .clone();
    write_gate(&fresh, &candidate)?;
    // Transaction: fixed destination, one lock, byte-exact expected revision.
    let (settings_path, lock_root) = target_settings(&scope, agent_root);
    let expected_revision = args
        .get("expectedRevision")
        .and_then(Value::as_str)
        .ok_or(SubagentsError::RevisionConflict)?
        .to_string();
    let model = args
        .get("model")
        .cloned()
        .ok_or_else(|| SubagentsError::InvalidDefinition("model action is required".into()))?;
    let thinking = args
        .get("thinking")
        .cloned()
        .ok_or_else(|| SubagentsError::InvalidDefinition("thinking action is required".into()))?;
    let advertise = args
        .get("advertise")
        .cloned()
        .ok_or_else(|| SubagentsError::InvalidDefinition("advertise action is required".into()))?;
    let disabled = args
        .get("disabled")
        .cloned()
        .ok_or_else(|| SubagentsError::InvalidDefinition("disabled action is required".into()))?;
    validate_override_actions([
        ("model", &model),
        ("thinking", &thinking),
        ("advertise", &advertise),
        ("disabled", &disabled),
    ])
    .map_err(SubagentsError::InvalidDefinition)?;
    let runtime_name = candidate.runtime_name.clone();
    let revision = crate::host_config::update_json_locked(
        &settings_path,
        &lock_root,
        &expected_revision,
        |value| {
            apply_override_edit_four(
                value,
                &runtime_name,
                &model,
                &thinking,
                &advertise,
                &disabled,
            )
            .map_err(crate::host_config::ConfigError::Io)
        },
    )
    .map_err(map_config_error)?;
    let after = fresh_inventory(&scope, agent_root)?;
    serde_json::to_value(&after)
        .map(|inventory| {
            json!({
                "revision": revision,
                "inventory": inventory,
                "runtimeRestartRequired": true,
            })
        })
        .map_err(|error| SubagentsError::ConfigUnavailable(error.to_string()))
}

const MAX_NAME_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_PROMPT_BYTES: usize = 64 * 1024;

/// Restricted single-segment agent name: path-free, dot-free at the start,
/// no Windows-reserved characters, no control characters. The exact `.`,
/// `..` (and any segment containing separators) reject.
pub fn validate_definition(name: &str, description: &str, prompt: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name is required".into());
    }
    if name.len() > MAX_NAME_BYTES {
        return Err("name exceeds the length bound".into());
    }
    if matches!(name, "." | "..")
        || name.starts_with('.')
        || name.ends_with('.')
        || name.ends_with(".chain")
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        // Trailing dots break Windows names; `.chain` names produce files
        // discovery deliberately excludes (.chain.md).
        return Err("name must be one path-free segment".into());
    }
    let description = description.trim();
    if description.is_empty() {
        return Err("description is required".into());
    }
    if description.len() > MAX_DESCRIPTION_BYTES
        || description.chars().any(|c| c.is_control() && c != '\t')
    {
        return Err("description is invalid or oversized".into());
    }
    if prompt.trim().is_empty() {
        return Err("prompt must not be blank".into());
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err("prompt exceeds the length bound".into());
    }
    Ok(())
}

/// Restricted frontmatter: escaped single-quoted YAML scalars so hostile
/// description text can never break out of the scalar or the document.
pub fn serialize_definition(name: &str, description: &str, prompt: &str) -> String {
    let scalar = |value: &str| format!("'{}'", value.replace('\'', "''"));
    format!(
        "---\nname: {}\ndescription: {}\n---\n{}",
        scalar(name.trim()),
        scalar(description.trim()),
        prompt
    )
}

fn collision_ids_for_name(fresh: &Inventory, name: &str) -> Vec<String> {
    let mut ids: Vec<String> = fresh
        .entries
        .iter()
        .filter(|entry| {
            entry.runtime_name == name
                || entry.local_name == name
                || entry.aliases.iter().any(|alias| alias == name)
        })
        .map(|entry| entry.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Confirmation gate shared by the dialog validation and the pre-publication
/// recheck: caller's inventory revision and shadow set must match a freshly
/// computed both-scope snapshot exactly. Drift means reconfirm, never write.
fn confirm_create_snapshot(
    expected_inventory_revision: &str,
    confirm_shadowed_ids: &[String],
    fresh: &Inventory,
    name: &str,
) -> Result<(), SubagentsError> {
    if fresh.inventory_revision != expected_inventory_revision {
        return Err(SubagentsError::InventoryRevisionConflict);
    }
    let mut confirmed = confirm_shadowed_ids.to_vec();
    confirmed.sort();
    confirmed.dedup();
    if confirmed != collision_ids_for_name(fresh, name) {
        return Err(SubagentsError::InventoryRevisionConflict);
    }
    Ok(())
}

/// Exclusive, symlink-safe publication of one definition file: a private
/// temp plus same-directory `hard_link` (fails if the target exists), so a
/// concurrent creator can never overwrite an existing file. Filesystems
/// without hard links refuse instead of falling back to a direct write.
fn publish_definition_exclusive(
    final_path: &Path,
    root: &Path,
    content: &[u8],
) -> Result<(), String> {
    let parent = final_path
        .parent()
        .ok_or_else(|| "definition path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    crate::host_config::reject_symlink_ancestry(final_path, &canonical_root)
        .map_err(config_error_reason)?;
    if fs::symlink_metadata(final_path).is_ok() {
        return Err(format!(
            "destination already exists: {}",
            final_path.display()
        ));
    }
    let temporary = final_path.with_file_name(format!(
        ".{}.picot-new-{}",
        final_path.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let attempt = (|| -> Result<(), std::io::Error> {
        fs::write(&temporary, content)?;
        restrict_private(&temporary)?;
        #[cfg(test)]
        if TEST_HOOK_FAIL_HARDLINK
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .is_some_and(|target| target == final_path)
        {
            return Err(std::io::Error::other("injected hard-link failure"));
        }
        // hard_link publishes without replacement: link(2) fails with EEXIST
        // if the final name appeared concurrently, so a racing creator can
        // never overwrite an existing definition.
        // ponytail: residual parent-symlink swap race between the ancestry
        // check above and this link; linkat(dirfd) closes it if it matters.
        fs::hard_link(&temporary, final_path)?;
        fs::remove_file(&temporary)
    })();
    if let Err(error) = attempt {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

// Test seam mirroring host_config's rename-failure hook: the publish path
// must clean its temp file when the final hard link fails.
#[cfg(test)]
static TEST_HOOK_FAIL_HARDLINK: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

fn config_error_reason(error: crate::host_config::ConfigError) -> String {
    match error {
        crate::host_config::ConfigError::Io(reason) => reason,
        other => other.code().to_string(),
    }
}

#[cfg(unix)]
fn restrict_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_private(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn target_agents_dir(scope: &AuthorizedScope, agent_root: &Path) -> (PathBuf, PathBuf) {
    match &scope.workspace {
        Some(binding) => (binding.root.join(".pi/agents"), binding.root.clone()),
        None => (agent_root.join("agents"), agent_root.to_path_buf()),
    }
}

/// Post-gate transaction: exclusive publication of already-validated bytes.
/// Revalidates the inventory revision and the fixed destination, publishes,
/// then re-reads and parses the final file — post-publish uncertainty is a
/// repair-needed report, never a rollback that might delete a replaced file.
pub(crate) fn create_transaction(
    scope: &AuthorizedScope,
    agent_root: &Path,
    confirmed: &Inventory,
    name: &str,
    content: &[u8],
    expected_inventory_revision: &str,
) -> Result<Value, SubagentsError> {
    let (agents_dir, lock_root) = target_agents_dir(scope, agent_root);
    let final_path = agents_dir.join(format!("{name}.md"));
    if fs::symlink_metadata(&final_path).is_ok() {
        return Err(SubagentsError::WriteFailed(
            "destination already exists".into(),
        ));
    }
    // Immediately before publication: fresh scan must still match the
    // confirmation snapshot exactly.
    let workspace = scope
        .workspace
        .as_ref()
        .map(|binding| binding.root.as_path());
    let preflight = subagents_inventory::inventory(agent_root, workspace, parity_evidence())
        .map_err(SubagentsError::ConfigUnavailable)?;
    confirm_create_snapshot(
        expected_inventory_revision,
        &collision_ids_for_name(confirmed, name),
        &preflight,
        name,
    )?;
    publish_definition_exclusive(&final_path, &lock_root, content)
        .map_err(SubagentsError::WriteFailed)?;
    // Post-publish verification: parse what actually landed; uncertainty is
    // a manual-check report, never a rollback of a possibly-replaced file.
    let after = subagents_inventory::inventory(agent_root, workspace, parity_evidence()).map_err(
        |reason| {
            SubagentsError::WriteFailed(format!(
                "definition published but rescan failed ({reason}); check the file manually"
            ))
        },
    )?;
    if !after.entries.iter().any(|entry| entry.runtime_name == name) {
        return Err(SubagentsError::WriteFailed(
            "definition published but did not rescan; check the file manually".into(),
        ));
    }
    let revision = crate::host_config::revision_of(&target_settings(scope, agent_root).0);
    serde_json::to_value(&after)
        .map(|inventory| {
            json!({
                "revision": revision,
                "inventory": inventory,
                "runtimeRestartRequired": true,
            })
        })
        .map_err(|error| SubagentsError::ConfigUnavailable(error.to_string()))
}

pub fn create(
    args: &Value,
    snapshot: &OwnerWorkspaceSnapshot,
    agent_root: &Path,
) -> Result<Value, SubagentsError> {
    let scope = authorize_scope(args, snapshot, agent_root)?;
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| SubagentsError::InvalidDefinition("name is required".into()))?;
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .ok_or_else(|| SubagentsError::InvalidDefinition("description is required".into()))?;
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| SubagentsError::InvalidDefinition("prompt is required".into()))?;
    validate_definition(name, description, prompt).map_err(SubagentsError::InvalidDefinition)?;
    let confirm_shadowed_ids: Vec<String> = args
        .get("confirmShadowedIds")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let expected_inventory_revision = args
        .get("expectedInventoryRevision")
        .and_then(Value::as_str)
        .ok_or(SubagentsError::InventoryRevisionConflict)?
        .to_string();
    let fresh = fresh_inventory(&scope, agent_root)?;
    if scope.project && !fresh.resolution_context.project_writes_allowed {
        return Err(SubagentsError::ProjectRootMismatch);
    }
    // Fixed destination existence is refused independently of everything else.
    let (agents_dir, _root) = target_agents_dir(&scope, agent_root);
    let final_path = agents_dir.join(format!("{}.md", name.trim()));
    if fs::symlink_metadata(&final_path).is_ok() {
        return Err(SubagentsError::WriteFailed(
            "destination already exists".into(),
        ));
    }
    // Write-qualification gate: the parity spike proved only
    // disk-candidates-only discovery, so out-of-scope occupancy of any
    // prospective name cannot be bounded and every create refuses here —
    // even one whose shadow set was confirmed exactly.
    // ponytail: single refusal point; relax together with ParityEvidence
    // when the spike demonstrates bounded runtime occupancy.
    let parity = parity_evidence();
    if !(parity.verified || parity.runtime_names_bounded) {
        return Err(SubagentsError::WriteEligibilityUnknown);
    }
    // Confirmation validation against the fresh snapshot.
    confirm_create_snapshot(
        &expected_inventory_revision,
        &confirm_shadowed_ids,
        &fresh,
        name.trim(),
    )?;
    // Target settings revision is validated independently.
    let (settings_path, _lock_root) = target_settings(&scope, agent_root);
    let expected_revision = args
        .get("expectedRevision")
        .and_then(Value::as_str)
        .ok_or(SubagentsError::RevisionConflict)?;
    if crate::host_config::revision_of(&settings_path) != expected_revision {
        return Err(SubagentsError::RevisionConflict);
    }
    // Pre-publish discovery-parse confirmation: the serialized definition
    // must round-trip through the same parser discovery uses.
    let content = serialize_definition(name, description, prompt);
    if !name_matches_serialized(&content, name.trim(), description.trim()) {
        return Err(SubagentsError::InvalidDefinition(
            "serialized definition does not round-trip".into(),
        ));
    }
    create_transaction(
        &scope,
        agent_root,
        &fresh,
        name.trim(),
        content.as_bytes(),
        &expected_inventory_revision,
    )
}

fn name_matches_serialized(content: &str, name: &str, description: &str) -> bool {
    crate::subagents_inventory::parse_definition(content).is_ok_and(
        |(parsed_name, parsed_description)| {
            parsed_name == name && parsed_description == description
        },
    )
}

/// Entry point behind the `RoutedAction::Host` gate. The caller supplies the
/// live owner snapshot and the resolved agent root; blocking work runs under
/// `spawn_blocking` in the WS layer, which re-reads the registry afterwards.
pub fn handle(
    op: &str,
    args: &Value,
    snapshot: &OwnerWorkspaceSnapshot,
    agent_root: &Path,
) -> Result<Value, SubagentsError> {
    match op {
        "subagents_inventory" => {
            handle_inventory(args, snapshot, agent_root).and_then(|inventory| {
                serde_json::to_value(&inventory)
                    .map_err(|error| SubagentsError::ConfigUnavailable(error.to_string()))
            })
        }
        "subagents_get_detail" => handle_detail(args, snapshot, agent_root),
        "subagents_set_override" => set_override(args, snapshot, agent_root),
        "subagents_create" => create(args, snapshot, agent_root),
        _ => Err(SubagentsError::UnknownOperation),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_owner::{OwnerId, TemporaryKind};
    use std::fs;

    struct ScopeDirs {
        root: PathBuf,
        agent_root: PathBuf,
        workspace: PathBuf,
    }

    fn make_fixture() -> ScopeDirs {
        let root = tempfile::tempdir().unwrap();
        let agent_root = root.path().join("agent");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(agent_root.join("agents")).unwrap();
        fs::create_dir_all(workspace.join(".pi/agents")).unwrap();
        fs::write(
            agent_root.join("agents/global-agent.md"),
            "---\nname: global-agent\ndescription: Global fixture\n---\nRAW_PROMPT  ",
        )
        .unwrap();
        fs::write(
            workspace.join(".pi/agents/project-agent.md"),
            "---\nname: project-agent\ndescription: Project fixture\n---\nPROJECT_PROMPT",
        )
        .unwrap();
        // Out-of-scope source with a secret sentinel: list must stay opaque.
        fs::create_dir_all(workspace.join(".agents")).unwrap();
        fs::write(workspace.join(".agents/hidden.md"), "OUT_OF_SCOPE_SENTINEL").unwrap();
        fs::write(
            agent_root.join("trust.json"),
            format!(
                "{{\n  \"{}\": true\n}}\n",
                workspace.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        ScopeDirs {
            root: root.keep(),
            agent_root,
            workspace,
        }
    }

    fn registered(fx: &ScopeDirs, wid: &str, generation: u64) -> OwnerWorkspaceSnapshot {
        OwnerWorkspaceSnapshot::Registered {
            wid: wid.to_string(),
            root: fx.workspace.canonicalize().unwrap(),
            generation,
        }
    }

    fn landing() -> OwnerWorkspaceSnapshot {
        OwnerWorkspaceSnapshot::Temporary {
            root: PathBuf::from("/nonexistent-home"),
            generation: 1,
            temporary_kind: TemporaryKind::Landing,
        }
    }

    #[test]
    fn global_scope_admits_landing_and_rejects_project_identity() {
        let fx = make_fixture();
        let args = json!({"scope": "global"});
        let inventory =
            handle_inventory(&args, &landing(), &fx.agent_root).expect("global admitted");
        assert!(inventory.workspace_root.is_none());
        assert!(inventory.project_root.is_none());
        assert_eq!(inventory.entries.len(), 1);
        assert_eq!(inventory.entries[0].runtime_name, "global-agent");
        let wire = handle("subagents_inventory", &args, &landing(), &fx.agent_root).unwrap();
        let text = wire.to_string();
        assert!(text.contains("global-agent"));
        assert!(!text.contains("PROJECT_PROMPT"));
        assert!(!text.contains("OUT_OF_SCOPE_SENTINEL"));
        // Project identity supplied on global scope is refused, not ignored:
        // a landing client must never smuggle a project binding this way.
        let forged = json!({"scope": "global", "workspaceId": "w1"});
        assert_eq!(
            handle("subagents_inventory", &forged, &landing(), &fx.agent_root)
                .unwrap_err()
                .code(),
            "unauthorized_target"
        );
        let with_generation = json!({"scope": "global", "workspaceGeneration": 2});
        assert_eq!(
            handle(
                "subagents_inventory",
                &with_generation,
                &landing(),
                &fx.agent_root
            )
            .unwrap_err()
            .code(),
            "unauthorized_target"
        );
    }

    #[test]
    fn project_scope_requires_registered_trusted_current_binding() {
        let fx = make_fixture();
        let ok = json!({"scope": "project", "workspaceId": "w1", "workspaceGeneration": 3});
        let inventory = handle_inventory(&ok, &registered(&fx, "w1", 3), &fx.agent_root)
            .expect("project admitted");
        assert_eq!(inventory.entries.len(), 2);
        assert_eq!(
            inventory.project_root.as_deref(),
            Some(fx.workspace.canonicalize().unwrap().as_path())
        );
        assert!(inventory.resolution_context.project_writes_allowed);
        // Forged wid.
        let forged = json!({"scope": "project", "workspaceId": "other", "workspaceGeneration": 3});
        assert_eq!(
            handle_inventory(&forged, &registered(&fx, "w1", 3), &fx.agent_root)
                .unwrap_err()
                .code(),
            "unauthorized_target"
        );
        // Stale generation (rebind happened between render and request).
        let stale = json!({"scope": "project", "workspaceId": "w1", "workspaceGeneration": 2});
        assert_eq!(
            handle_inventory(&stale, &registered(&fx, "w1", 3), &fx.agent_root)
                .unwrap_err()
                .code(),
            "stale_generation"
        );
        // Temporary owner (landing) cannot request project scope.
        assert_eq!(
            handle_inventory(&ok, &landing(), &fx.agent_root)
                .unwrap_err()
                .code(),
            "not_registered"
        );
        // No workspace binding at all.
        assert_eq!(
            handle_inventory(&ok, &OwnerWorkspaceSnapshot::NoWorkspace, &fx.agent_root)
                .unwrap_err()
                .code(),
            "not_registered"
        );
        // Untrusted workspace.
        let untrusted = make_fixture();
        fs::write(
            untrusted.agent_root.join("trust.json"),
            format!(
                "{{\n  \"{}\": false\n}}\n",
                untrusted.workspace.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        assert_eq!(
            handle_inventory(&ok, &registered(&untrusted, "w1", 3), &untrusted.agent_root)
                .unwrap_err()
                .code(),
            "project_untrusted"
        );
        let _ = fx.root;
    }

    #[test]
    fn caller_supplied_roots_are_rejected() {
        let fx = make_fixture();
        for key in ["agentRoot", "workspaceRoot", "projectRoot", "path", "root"] {
            let args = json!({"scope": "global", key: "/etc"});
            assert_eq!(
                handle("subagents_inventory", &args, &landing(), &fx.agent_root)
                    .unwrap_err()
                    .code(),
                "unauthorized_target",
                "key {key} must be rejected"
            );
        }
    }

    #[test]
    fn detail_rejects_forged_stale_and_out_of_scope_ids() {
        let fx = make_fixture();
        let snapshot = registered(&fx, "w1", 4);
        let inventory = handle_inventory(
            &json!({"scope": "project", "workspaceId": "w1", "workspaceGeneration": 4}),
            &snapshot,
            &fx.agent_root,
        )
        .unwrap();
        let candidate = inventory
            .entries
            .iter()
            .find(|entry| entry.runtime_name == "global-agent")
            .unwrap();
        let ok = json!({
            "scope": "project", "workspaceId": "w1", "workspaceGeneration": 4,
            "candidateId": candidate.id,
        });
        let detail = handle("subagents_get_detail", &ok, &snapshot, &fx.agent_root).unwrap();
        assert_eq!(detail["candidateId"], candidate.id);
        assert_eq!(
            detail["rawDefinition"].as_str().unwrap(),
            "---\nname: global-agent\ndescription: Global fixture\n---\nRAW_PROMPT  "
        );
        for forged in [
            json!({"scope": "global", "candidateId": "0".repeat(64)}),
            json!({"scope": "global", "candidateId": "../../workspace/.agents/hidden.md"}),
            json!({"scope": "global", "candidateId": ""}),
            json!({"scope": "global"}),
        ] {
            let error =
                handle("subagents_get_detail", &forged, &snapshot, &fx.agent_root).unwrap_err();
            assert_eq!(error.code(), "candidate_stale", "forged: {forged}");
        }
    }

    #[test]
    fn set_override_refusals_and_package_bytes_unchanged() {
        let fx = make_fixture();
        // Shadowed: same runtime name in global and project scopes.
        fs::write(
            fx.agent_root.join("agents/twin.md"),
            "---\nname: project-agent\ndescription: shadowing twin\n---\nTWIN",
        )
        .unwrap();
        // External runner definition.
        fs::create_dir_all(fx.agent_root.join("agents")).unwrap();
        fs::write(
            fx.agent_root.join("agents/external.md"),
            "---\nname: external-agent\ndescription: mcp backed\nrunner: mcp\n---\nEXT",
        )
        .unwrap();
        let snapshot = registered(&fx, "w1", 5);
        let args = |candidate_id: &str| {
            json!({
                "scope": "project", "workspaceId": "w1", "workspaceGeneration": 5,
                "candidateId": candidate_id,
                "model": {"op": "set", "value": "provider/model"},
                "thinking": {"op": "keep"},
                "advertise": {"op": "keep"}, "disabled": {"op": "keep"},
                "expectedRevision": "any",
            })
        };
        let inventory = handle_inventory(
            &json!({"scope": "project", "workspaceId": "w1", "workspaceGeneration": 5}),
            &snapshot,
            &fx.agent_root,
        )
        .unwrap();
        let find = |name: &str| {
            inventory
                .entries
                .iter()
                .find(|entry| entry.runtime_name == name)
                .unwrap()
                .id
                .clone()
        };
        let loser = inventory
            .entries
            .iter()
            .find(|entry| entry.runtime_name == "project-agent" && entry.source == "user")
            .unwrap();
        assert_eq!(
            set_override(&args(&loser.id), &snapshot, &fx.agent_root)
                .unwrap_err()
                .code(),
            "shadowed_candidate"
        );
        assert_eq!(
            set_override(
                &args(
                    &inventory
                        .entries
                        .iter()
                        .find(|entry| entry.runtime_name == "project-agent"
                            && entry.source == "project")
                        .unwrap()
                        .id
                ),
                &snapshot,
                &fx.agent_root
            )
            .unwrap_err()
            .code(),
            "revision_conflict"
        );
        assert_eq!(
            set_override(&args(&find("external-agent")), &snapshot, &fx.agent_root)
                .unwrap_err()
                .code(),
            "external_runner"
        );
        // Sole occupant, native runner: stale revision still refuses.
        fs::remove_file(fx.agent_root.join("agents/twin.md")).unwrap();
        assert_eq!(
            set_override(&args(&find("global-agent")), &snapshot, &fx.agent_root)
                .unwrap_err()
                .code(),
            "revision_conflict"
        );
        // Stale candidate ID.
        assert_eq!(
            set_override(&args("0".repeat(64).as_str()), &snapshot, &fx.agent_root)
                .unwrap_err()
                .code(),
            "candidate_stale"
        );
        // Root mismatch: extension project root resolves above the workspace.
        let nested = fx.workspace.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let mismatch_args = json!({
            "scope": "project", "workspaceId": "w2", "workspaceGeneration": 5,
            "candidateId": find("global-agent"),
            "model": {"op": "keep"}, "thinking": {"op": "keep"},
            "advertise": {"op": "keep"}, "disabled": {"op": "keep"},
            "expectedRevision": "any",
        });
        // Register the nested dir as the workspace: .pi lives at the parent,
        // so the extension root and the workspace root disagree.
        let mismatched_snapshot = OwnerWorkspaceSnapshot::Registered {
            wid: "w2".to_string(),
            root: nested,
            generation: 5,
        };
        let error = set_override(&mismatch_args, &mismatched_snapshot, &fx.agent_root).unwrap_err();
        assert_eq!(error.code(), "project_root_mismatch");
        // Refused writes never touched any definition bytes.
        assert_eq!(
            fs::read_to_string(fx.workspace.join(".pi/agents/project-agent.md")).unwrap(),
            "---\nname: project-agent\ndescription: Project fixture\n---\nPROJECT_PROMPT"
        );
        assert_eq!(
            fs::read_to_string(fx.agent_root.join("agents/global-agent.md")).unwrap(),
            "---\nname: global-agent\ndescription: Global fixture\n---\nRAW_PROMPT  "
        );
        // And no settings file materialized from refused transactions.
        assert!(!fx.agent_root.join("settings.json").exists());
        assert!(!fx.workspace.join(".pi/settings.json").exists());
    }

    #[test]
    fn override_precedence_winner_passes_fresh_write_gate() {
        let fx = make_fixture();
        fs::write(
            fx.agent_root.join("agents/duplicate.md"),
            "---\nname: project-agent\ndescription: User\n---\nbody",
        )
        .unwrap();
        let owner = registered(&fx, "w1", 3);
        let args = json!({"scope":"project","workspaceId":"w1","workspaceGeneration":3});
        let inv = handle_inventory(&args, &owner, &fx.agent_root).unwrap();
        let winner = inv
            .entries
            .iter()
            .find(|e| e.runtime_name == "project-agent" && e.source == "project")
            .unwrap();
        let loser = inv
            .entries
            .iter()
            .find(|e| e.runtime_name == "project-agent" && e.source == "user")
            .unwrap();
        assert_eq!(
            write_gate(&inv, loser),
            Err(SubagentsError::ShadowedCandidate)
        );
        assert_eq!(write_gate(&inv, winner), Ok(()));
        let response = set_override(
            &json!({
                "scope":"project", "workspaceId":"w1", "workspaceGeneration":3,
                "candidateId":winner.id, "expectedRevision":winner.settings_revision,
                "model":{"op":"set","value":"provider/winner"}, "thinking":{"op":"keep"},
                "advertise":{"op":"set","value":false}, "disabled":{"op":"set","value":false}
            }),
            &owner,
            &fx.agent_root,
        )
        .unwrap();
        assert_eq!(
            response["inventory"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["id"] == winner.id)
                .unwrap()["savedOverride"]["model"],
            "provider/winner"
        );
        let saved = &response["inventory"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == winner.id)
            .unwrap()["savedOverride"];
        assert_eq!(saved["advertise"], false);
        assert_eq!(saved["disabled"], false);
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(fx.workspace.join(".pi/settings.json")).unwrap()
            )
            .unwrap()["subagents"]["agentOverrides"]["project-agent"]["model"],
            "provider/winner"
        );
    }

    #[test]
    fn apply_override_edit_validates_ops_and_values() {
        let mut settings = json!({});
        let keep = json!({"op": "keep"});
        // Bad op.
        assert!(apply_override_edit_four(
            &mut settings,
            "a.b",
            &json!({"op": "nonsense"}),
            &keep,
            &keep,
            &keep,
        )
        .is_err());
        // Bad model ids: empty, whitespace, missing provider segment.
        for bad in ["", "has space/x", "noprovider", "p/m\u{0000}"] {
            assert!(
                apply_override_edit_four(
                    &mut settings,
                    "a.b",
                    &json!({"op": "set", "value": bad}),
                    &keep,
                    &keep,
                    &keep,
                )
                .is_err(),
                "model {bad:?} must reject"
            );
        }
        // Bad thinking values.
        for bad in [json!(true), json!("sometimes"), json!(5)] {
            assert!(
                apply_override_edit_four(
                    &mut settings,
                    "a.b",
                    &keep,
                    &json!({"op": "set", "value": bad}),
                    &keep,
                    &keep,
                )
                .is_err(),
                "thinking {bad} must reject"
            );
        }
        // Full accepted range, including false and xhigh/max extremes.
        for good in [
            json!(false),
            json!("off"),
            json!("minimal"),
            json!("low"),
            json!("medium"),
            json!("high"),
            json!("xhigh"),
            json!("max"),
        ] {
            apply_override_edit_four(
                &mut settings,
                "a.b",
                &keep,
                &json!({"op": "set", "value": good}),
                &keep,
                &keep,
            )
            .unwrap();
        }
        assert_eq!(
            settings["subagents"]["agentOverrides"]["a.b"]["thinking"],
            json!("max")
        );
        // runtimeName must be path-free.
        assert!(
            apply_override_edit_four(&mut settings, "a/../b", &keep, &keep, &keep, &keep,).is_err()
        );
    }

    #[test]
    fn override_four_fields_set_keep_clear_and_preserve_unknown() {
        let mut settings = json!({"other": 7, "subagents": {"agentOverrides": {
            "agent": {"other": "untouched"}
        }}});
        let keep = json!({"op": "keep"});
        let fields = [
            ("model", json!("provider/model")),
            ("thinking", json!(false)),
            ("advertise", json!(false)),
            ("disabled", json!(true)),
        ];
        for (index, (field, expected)) in fields.iter().enumerate() {
            let mut actions = [keep.clone(), keep.clone(), keep.clone(), keep.clone()];
            actions[index] = json!({"op": "set", "value": expected});
            apply_override_edit_four(
                &mut settings,
                "agent",
                &actions[0],
                &actions[1],
                &actions[2],
                &actions[3],
            )
            .unwrap();
            assert_eq!(
                settings["subagents"]["agentOverrides"]["agent"][field], *expected,
                "set {field}"
            );
            actions[index] = keep.clone();
            apply_override_edit_four(
                &mut settings,
                "agent",
                &actions[0],
                &actions[1],
                &actions[2],
                &actions[3],
            )
            .unwrap();
            assert_eq!(
                settings["subagents"]["agentOverrides"]["agent"][field], *expected,
                "keep {field}"
            );
            actions[index] = json!({"op": "clear"});
            apply_override_edit_four(
                &mut settings,
                "agent",
                &actions[0],
                &actions[1],
                &actions[2],
                &actions[3],
            )
            .unwrap();
            assert!(
                settings["subagents"]["agentOverrides"]["agent"]
                    .get(field)
                    .is_none(),
                "clear {field}"
            );
        }
        assert_eq!(
            settings,
            json!({"other": 7, "subagents": {"agentOverrides": {
                "agent": {"other": "untouched"}
            }}})
        );
        let mut empty = json!({});
        apply_override_edit_four(
            &mut empty,
            "agent",
            &json!({"op":"set","value":"provider/model"}),
            &json!({"op":"set","value":"high"}),
            &json!({"op":"set","value":true}),
            &json!({"op":"set","value":false}),
        )
        .unwrap();
        assert_eq!(
            empty["subagents"]["agentOverrides"]["agent"],
            json!({"model":"provider/model","thinking":"high","advertise":true,"disabled":false})
        );
        let clear = json!({"op":"clear"});
        apply_override_edit_four(&mut empty, "agent", &clear, &clear, &clear, &clear).unwrap();
        assert_eq!(
            empty,
            json!({}),
            "all four fields clear prunes empty layers"
        );
    }

    #[test]
    fn override_boolean_actions_reject_non_boolean_without_writing() {
        let fx = make_fixture();
        let inv = handle_inventory(&json!({"scope":"global"}), &landing(), &fx.agent_root).unwrap();
        let candidate = &inv.entries[0];
        let settings_path = fx.agent_root.join("settings.json");
        let valid = json!({
            "scope":"global", "candidateId":candidate.id, "expectedRevision":candidate.settings_revision,
            "model":{"op":"keep"}, "thinking":{"op":"keep"},
            "advertise":{"op":"keep"}, "disabled":{"op":"keep"}
        });
        for field in ["advertise", "disabled"] {
            for bad in [json!("true"), json!(1), Value::Null] {
                let mut args = valid.clone();
                args[field] = json!({"op":"set", "value":bad});
                assert_eq!(
                    set_override(&args, &landing(), &fx.agent_root)
                        .unwrap_err()
                        .code(),
                    "invalid_definition",
                    "{field}={bad}"
                );
                assert!(!settings_path.exists());
            }
            let mut missing_value = valid.clone();
            missing_value[field] = json!({"op":"set"});
            assert_eq!(
                set_override(&missing_value, &landing(), &fx.agent_root)
                    .unwrap_err()
                    .code(),
                "invalid_definition",
                "{field} missing value"
            );
            let mut missing_action = valid.clone();
            missing_action.as_object_mut().unwrap().remove(field);
            assert_eq!(
                set_override(&missing_action, &landing(), &fx.agent_root)
                    .unwrap_err()
                    .code(),
                "invalid_definition",
                "{field} missing action"
            );
            assert!(!settings_path.exists());
        }
    }

    #[test]
    fn owner_rebind_after_admission_fails_recheck() {
        let fx = make_fixture();
        let args = json!({"scope": "project", "workspaceId": "w1", "workspaceGeneration": 3});
        let before = registered(&fx, "w1", 3);
        let scope = authorize_scope(&args, &before, &fx.agent_root).unwrap();
        assert!(scope.project);
        // The WS layer re-reads the registry after blocking work and compares
        // snapshots; a rebind to another workspace (or generation) must void
        // the admission that produced this scan.
        let rebind = OwnerWorkspaceSnapshot::Registered {
            wid: "w2".to_string(),
            root: fx.workspace.canonicalize().unwrap(),
            generation: 4,
        };
        assert_ne!(before, rebind);
        assert_eq!(
            authorize_scope(&args, &rebind, &fx.agent_root)
                .unwrap_err()
                .code(),
            "unauthorized_target"
        );
        let mut revoked = before.clone();
        if let OwnerWorkspaceSnapshot::Registered { generation, .. } = &mut revoked {
            *generation += 1;
        }
        assert_eq!(
            authorize_scope(&args, &revoked, &fx.agent_root)
                .unwrap_err()
                .code(),
            "stale_generation"
        );
    }

    #[test]
    fn unavailable_agent_root_is_a_config_error() {
        let fx = make_fixture();
        let missing = fx.root.join("nope");
        assert_eq!(
            handle(
                "subagents_inventory",
                &json!({"scope": "global"}),
                &landing(),
                &missing
            )
            .unwrap_err()
            .code(),
            "config_unavailable"
        );
    }

    #[test]
    fn write_controls_are_dispatched_but_gate_refused() {
        let fx = make_fixture();
        let snapshot = registered(&fx, "w1", 3);
        let inventory = handle_inventory(
            &json!({"scope": "project", "workspaceId": "w1", "workspaceGeneration": 3}),
            &snapshot,
            &fx.agent_root,
        )
        .unwrap();
        let candidate_id = inventory
            .entries
            .iter()
            .find(|entry| entry.runtime_name == "global-agent")
            .unwrap()
            .id
            .clone();
        let override_args = json!({
            "scope": "project", "workspaceId": "w1", "workspaceGeneration": 3,
            "candidateId": candidate_id, "runtimeName": "global-agent",
            "model": {"op": "set", "value": "provider/model"},
            "thinking": {"op": "keep"},
            "advertise": {"op": "keep"}, "disabled": {"op": "keep"},
        });
        assert_eq!(
            handle(
                "subagents_set_override",
                &override_args,
                &snapshot,
                &fx.agent_root
            )
            .unwrap_err()
            .code(),
            "revision_conflict"
        );
        let create_args = json!({
            "scope": "global", "name": "x", "description": "d", "prompt": "p",
            "expectedRevision": "r", "expectedInventoryRevision": "i",
            "confirmShadowedIds": [],
        });
        assert_eq!(
            handle("subagents_create", &create_args, &snapshot, &fx.agent_root)
                .unwrap_err()
                .code(),
            "write_eligibility_unknown"
        );
        // Admission still runs first: untrusted project writes report trust,
        // not eligibility, so the client can explain the refusal.
        let denied = make_fixture();
        fs::write(
            denied.agent_root.join("trust.json"),
            format!(
                "{{\n  \"{}\": false\n}}\n",
                denied.workspace.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        assert_eq!(
            handle(
                "subagents_set_override",
                &json!({"scope": "project", "workspaceId": "w9", "workspaceGeneration": 9}),
                &registered(&denied, "w9", 9),
                &denied.agent_root
            )
            .unwrap_err()
            .code(),
            "project_untrusted"
        );
        assert_eq!(
            handle(
                "subagents_no_such_op",
                &json!({}),
                &snapshot,
                &fx.agent_root
            )
            .unwrap_err()
            .code(),
            "unknown_operation"
        );
        let _ = OwnerId::from_string("unused".into());
    }

    #[test]
    fn override_cross_layer_and_package_preserves_definition_bytes() {
        let fx = make_fixture();
        let shared = fx.root.join("shared");
        fs::create_dir_all(shared.join("agents")).unwrap();
        fs::write(
            shared.join("package.json"),
            r#"{"name":"shared","pi-subagents":{"agents":["agents"]}}"#,
        )
        .unwrap();
        let package_file = shared.join("agents/worker.md");
        fs::write(
            &package_file,
            "---\nname: worker\ndescription: D\nmodel: frontmatter/model\n---\nPROMPT",
        )
        .unwrap();
        let global_file = fx.agent_root.join("agents/global-agent.md");
        let original_global_file = fs::read(&global_file).unwrap();
        let original_package_file = fs::read(&package_file).unwrap();
        let user_settings = json!({"packages":["file:../shared"], "subagents":{"agentOverrides":{
            "worker":{"model":"user/model","thinking":"low","other":"keep"},
            "global-agent":{"model":"user/old"}
        }}, "unrelated": 7});
        fs::write(
            fx.agent_root.join("settings.json"),
            user_settings.to_string(),
        )
        .unwrap();
        let global_bytes = fs::read(fx.agent_root.join("settings.json")).unwrap();
        fs::write(fx.workspace.join(".pi/settings.json"), json!({"packages":["file:../../shared"], "subagents":{"agentOverrides":{"worker":{"thinking":"high","other":"project"}}}}).to_string()).unwrap();
        let snapshot = registered(&fx, "w1", 3);
        let project_args = json!({"scope":"project","workspaceId":"w1","workspaceGeneration":3});
        assert!(
            handle_inventory(&project_args, &snapshot, &fx.agent_root)
                .unwrap()
                .entries
                .iter()
                .find(|e| e.runtime_name == "project-agent")
                .unwrap()
                .write_qualified
        );
        for name in ["global-agent", "worker"] {
            let inventory = handle_inventory(&project_args, &snapshot, &fx.agent_root).unwrap();
            let entry = inventory
                .entries
                .iter()
                .find(|e| e.runtime_name == name)
                .unwrap();
            assert!(
                entry.write_qualified,
                "{name}: {:?}",
                entry.write_diagnostic
            );
            assert_eq!(
                entry.settings_revision,
                inventory.settings_revisions["project"]
            );
            if name == "worker" {
                assert_eq!(entry.source, "package");
                assert_eq!(entry.saved_override["thinking"], "high");
                assert_eq!(entry.saved_override["model"], Value::Null);
            } else {
                assert_eq!(entry.source_scope, "global");
                assert_eq!(entry.saved_override, json!({}));
            }
            let output = set_override(
                &json!({
                    "scope":"project", "workspaceId":"w1", "workspaceGeneration":3,
                    "candidateId":entry.id, "expectedRevision":entry.settings_revision,
                    "model":{"op":"set","value":"project/new"}, "thinking":{"op":"keep"},
                    "advertise":{"op":"keep"}, "disabled":{"op":"keep"}
                }),
                &snapshot,
                &fx.agent_root,
            )
            .unwrap();
            assert_eq!(
                output["revision"],
                output["inventory"]["settingsRevisions"]["project"]
            );
            let echoed = output["inventory"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["runtimeName"] == name)
                .unwrap();
            assert_eq!(echoed["savedOverride"]["model"], "project/new");
        }
        let project: Value =
            serde_json::from_slice(&fs::read(fx.workspace.join(".pi/settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            project["subagents"]["agentOverrides"]["worker"],
            json!({"model":"project/new","thinking":"high","other":"project"})
        );
        assert_eq!(
            project["subagents"]["agentOverrides"]["global-agent"]["model"],
            "project/new"
        );
        assert_eq!(
            fs::read(fx.agent_root.join("settings.json")).unwrap(),
            global_bytes
        );
        assert_eq!(fs::read(&global_file).unwrap(), original_global_file);
        assert_eq!(fs::read(&package_file).unwrap(), original_package_file);
        assert!(String::from_utf8(original_package_file.clone())
            .unwrap()
            .contains("model: frontmatter/model"));
        // Global projection of same package sees user layer instead.
        let global =
            handle_inventory(&json!({"scope":"global"}), &landing(), &fx.agent_root).unwrap();
        let worker = global
            .entries
            .iter()
            .find(|e| e.runtime_name == "worker")
            .unwrap();
        assert_eq!(worker.saved_override["model"], "user/model");
        assert_eq!(
            worker.settings_revision,
            global.settings_revisions["global"]
        );
        assert!(worker.write_qualified);
        // Same package also permits writes to global layer, without affecting project bytes.
        let project_bytes = fs::read(fx.workspace.join(".pi/settings.json")).unwrap();
        let response = set_override(&json!({
            "scope":"global", "candidateId":worker.id, "expectedRevision":worker.settings_revision,
            "model":{"op":"set","value":"user/new"}, "thinking":{"op":"keep"},
            "advertise":{"op":"keep"}, "disabled":{"op":"keep"}
        }), &landing(), &fx.agent_root).unwrap();
        assert_eq!(
            response["inventory"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["runtimeName"] == "worker")
                .unwrap()["savedOverride"]["model"],
            "user/new"
        );
        assert_eq!(
            fs::read(fx.workspace.join(".pi/settings.json")).unwrap(),
            project_bytes
        );
        assert_eq!(fs::read(&global_file).unwrap(), original_global_file);
        assert_eq!(fs::read(&package_file).unwrap(), original_package_file);
    }

    #[test]
    fn override_refuses_alias_collisions_and_incomplete_scan_but_tolerates_bad_manifests() {
        let fx = make_fixture();
        let global = json!({"scope":"global"});
        let path = fx.agent_root.join("agents/other.md");
        for content in [
            "---\nname: global-agent\ndescription: D\n---\nX",
            "---\nname: other\ndescription: D\nalias: global-agent\n---\nX",
        ] {
            fs::write(&path, content).unwrap();
            let inv = handle_inventory(&global, &landing(), &fx.agent_root).unwrap();
            let entry = inv
                .entries
                .iter()
                .find(|e| e.runtime_name == "global-agent")
                .unwrap();
            assert!(!entry.write_qualified);
            assert_eq!(
                write_gate(&inv, entry),
                Err(SubagentsError::ShadowedCandidate)
            );
        }
        fs::write(
            fx.agent_root.join("agents/global-agent.md"),
            "---\nname: global-agent\ndescription: D\nalias: same\n---\nX",
        )
        .unwrap();
        fs::write(
            &path,
            "---\nname: other\ndescription: D\nalias: same\n---\nX",
        )
        .unwrap();
        let inv = handle_inventory(&global, &landing(), &fx.agent_root).unwrap();
        assert!(inv.entries.iter().all(|e| !e.write_qualified));
        fs::remove_file(&path).unwrap();
        // Repeated self-alias does not create a second occupant.
        fs::write(
            fx.agent_root.join("agents/global-agent.md"),
            "---\nname: global-agent\ndescription: D\naliases: same, same\n---\nX",
        )
        .unwrap();
        assert!(
            handle_inventory(&global, &landing(), &fx.agent_root)
                .unwrap()
                .entries[0]
                .write_qualified
        );
        fs::write(fx.agent_root.join("agents/bad.md"), "invalid").unwrap();
        let inv = handle_inventory(&global, &landing(), &fx.agent_root).unwrap();
        let entry = &inv.entries[0];
        assert!(!entry.write_qualified);
        assert_eq!(
            write_gate(&inv, entry),
            Err(SubagentsError::WriteEligibilityUnknown)
        );
        fs::remove_file(fx.agent_root.join("agents/bad.md")).unwrap();
        fs::write(fx.agent_root.join("package.json"), "{").unwrap();
        let inv = handle_inventory(&global, &landing(), &fx.agent_root).unwrap();
        assert!(inv.entries[0].write_qualified);
        assert!(inv
            .diagnostics
            .iter()
            .any(|d| d.source == "package" && d.message.contains("invalid manifest")));
        fs::remove_file(fx.agent_root.join("package.json")).unwrap();
        fs::write(fx.agent_root.join("settings.json"), "{").unwrap();
        assert!(
            !handle_inventory(&global, &landing(), &fx.agent_root)
                .unwrap()
                .entries[0]
                .write_qualified
        );
    }

    fn no_stray_temp(dir: &Path) -> bool {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().contains("picot-new"))
    }

    fn create_args(name: &str, description: &str, prompt: &str) -> Value {
        json!({
            "scope": "global", "name": name, "description": description, "prompt": prompt,
            "expectedRevision": "absent", "expectedInventoryRevision": "unused",
            "confirmShadowedIds": [],
        })
    }

    #[test]
    fn create_rejects_invalid_definitions() {
        let fx = make_fixture();
        let bad = [
            ("", "d", "p"),
            ("   ", "d", "p"),
            (".", "d", "p"),
            ("..", "d", "p"),
            (".hidden", "d", "p"),
            ("a/b", "d", "p"),
            ("a\\b", "d", "p"),
            ("a:b", "d", "p"),
            ("a\u{0007}b", "d", "p"),
            ("x.chain", "d", "p"),
            ("x.", "d", "p"),
            ("x", "", "p"),
            ("x", "   ", "p"),
            ("x", "d\u{0000}", "p"),
            ("x", "d", ""),
            ("x", "d", "   \n\t "),
        ];
        for (name, description, prompt) in bad {
            assert_eq!(
                create(
                    &create_args(name, description, prompt),
                    &landing(),
                    &fx.agent_root
                )
                .unwrap_err()
                .code(),
                "invalid_definition",
                "case name={name:?} must reject"
            );
        }
        // Oversized fields: name > 64 bytes, description > 8 KiB, prompt > 64 KiB.
        for (name, description, prompt) in [
            ("x".repeat(MAX_NAME_BYTES + 1), "d".into(), "p".into()),
            (
                "x".into(),
                "d".repeat(MAX_DESCRIPTION_BYTES + 1),
                "p".into(),
            ),
            ("x".into(), "d".into(), "p".repeat(MAX_PROMPT_BYTES + 1)),
        ] {
            assert_eq!(
                create(
                    &create_args(&name, &description, &prompt),
                    &landing(),
                    &fx.agent_root
                )
                .unwrap_err()
                .code(),
                "invalid_definition",
                "oversized case must reject"
            );
        }
        // Missing fields outright.
        for missing in [
            json!({"scope": "global", "description": "d", "prompt": "p",
                   "expectedInventoryRevision": "x"}),
            json!({"scope": "global", "name": "n", "prompt": "p",
                   "expectedInventoryRevision": "x"}),
            json!({"scope": "global", "name": "n", "description": "d",
                   "expectedInventoryRevision": "x"}),
        ] {
            assert_eq!(
                create(&missing, &landing(), &fx.agent_root)
                    .unwrap_err()
                    .code(),
                "invalid_definition"
            );
        }
        // Missing inventory revision token is its own conflict, after validation.
        let no_revision = json!({
            "scope": "global", "name": "n", "description": "d", "prompt": "p",
            "expectedRevision": "absent", "confirmShadowedIds": [],
        });
        assert_eq!(
            create(&no_revision, &landing(), &fx.agent_root)
                .unwrap_err()
                .code(),
            "inventory_revision_conflict"
        );
        assert!(!fx.agent_root.join("agents/x.md").exists());
        assert!(no_stray_temp(&fx.agent_root.join("agents")));
    }

    #[test]
    fn serialized_frontmatter_escapes_and_round_trips() {
        // Hostile punctuation stays one quoted scalar line: the discovery
        // parser reads back exactly what was validated.
        let description = "desc: colon, [brackets], --- dashes, # hash";
        let content = serialize_definition("plain", description, "PROMPT");
        assert!(content.starts_with(
            "---\nname: 'plain'\ndescription: 'desc: colon, [brackets], --- dashes, # hash'\n---\nPROMPT"
        ));
        assert!(name_matches_serialized(&content, "plain", description));
        // Embedded quotes cannot round-trip through the bounded frontmatter
        // parser (''-escaping is not unescaped there); creation rejects
        // rather than publishing misrepresented fields.
        assert!(!name_matches_serialized(
            &serialize_definition("it's", "d", "p"),
            "it's",
            "d"
        ));
        assert!(!name_matches_serialized(
            &serialize_definition("x", "it's", "p"),
            "x",
            "it's"
        ));
    }

    #[test]
    fn create_refuses_destination_and_project_gates() {
        let fx = make_fixture();
        // Target-path collision refused independently of everything else.
        assert_eq!(
            create(
                &create_args("global-agent", "d", "p"),
                &landing(),
                &fx.agent_root
            )
            .unwrap_err()
            .code(),
            "write_failed"
        );
        // A symlink at the destination is refused without following it.
        #[cfg(unix)]
        {
            let outside = fx.root.join("outside.md");
            fs::write(&outside, "SENTINEL").unwrap();
            let link = fx.agent_root.join("agents/link-target.md");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert_eq!(
                create(
                    &create_args("link-target", "d", "p"),
                    &landing(),
                    &fx.agent_root
                )
                .unwrap_err()
                .code(),
                "write_failed"
            );
            assert_eq!(fs::read_to_string(&outside).unwrap(), "SENTINEL");
            assert!(fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
        }
        // Basename differs from YAML name: the exact target filename is
        // never overwritten even though its content registers another name.
        fs::write(
            fx.agent_root.join("agents/weird.md"),
            "---\nname: target\ndescription: other name\n---\nW",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(fx.agent_root.join("agents/weird.md")).unwrap(),
            "---\nname: target\ndescription: other name\n---\nW"
        );
        assert_eq!(
            create(&create_args("weird", "d", "p"), &landing(), &fx.agent_root)
                .unwrap_err()
                .code(),
            "write_failed"
        );
        assert_eq!(
            fs::read_to_string(fx.agent_root.join("agents/weird.md")).unwrap(),
            "---\nname: target\ndescription: other name\n---\nW"
        );
        // An uncontested new name with a correct revision still refuses:
        // disk-candidates-only parity leaves occupancy unbounded.
        let fresh =
            handle_inventory(&json!({"scope": "global"}), &landing(), &fx.agent_root).unwrap();
        let qualified = json!({
            "scope": "global", "name": "brand-new", "description": "d", "prompt": "p",
            "expectedRevision": "absent",
            "expectedInventoryRevision": fresh.inventory_revision,
            "confirmShadowedIds": [],
        });
        assert_eq!(
            create(&qualified, &landing(), &fx.agent_root)
                .unwrap_err()
                .code(),
            "write_eligibility_unknown"
        );
        assert!(!fx.agent_root.join("agents/brand-new.md").exists());
        // Project-scope gates: not registered, stale generation, untrusted,
        // root mismatch.
        let project = |wid: &str, generation: u64| {
            json!({
                "scope": "project", "workspaceId": wid, "workspaceGeneration": generation,
                "name": "n", "description": "d", "prompt": "p",
                "expectedRevision": "absent", "expectedInventoryRevision": "x",
                "confirmShadowedIds": [],
            })
        };
        assert_eq!(
            create(&project("w1", 3), &landing(), &fx.agent_root)
                .unwrap_err()
                .code(),
            "not_registered"
        );
        assert_eq!(
            create(&project("w1", 2), &registered(&fx, "w1", 3), &fx.agent_root)
                .unwrap_err()
                .code(),
            "stale_generation"
        );
        let denied = make_fixture();
        fs::write(
            denied.agent_root.join("trust.json"),
            format!(
                "{{\n  \"{}\": false\n}}\n",
                denied.workspace.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        assert_eq!(
            create(
                &project("w1", 3),
                &registered(&denied, "w1", 3),
                &denied.agent_root
            )
            .unwrap_err()
            .code(),
            "project_untrusted"
        );
        // Nested dir under the trusted workspace: .pi lives at the parent,
        // so the extension root and the registered root disagree.
        let nested = fx.workspace.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            fx.agent_root.join("trust.json"),
            format!(
                "{{\n  \"{}\": true,\n  \"{}\": true\n}}\n",
                fx.workspace.canonicalize().unwrap().display(),
                nested.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        let mismatched = OwnerWorkspaceSnapshot::Registered {
            wid: "w2".to_string(),
            root: nested.canonicalize().unwrap(),
            generation: 3,
        };
        assert_eq!(
            create(&project("w2", 3), &mismatched, &fx.agent_root)
                .unwrap_err()
                .code(),
            "project_root_mismatch"
        );
    }

    #[test]
    fn create_confirmation_requires_exact_shadow_snapshot() {
        let fx = make_fixture();
        // Same runtime name in global and project, builtin plus user, a
        // basename/YAML-name divergence, and an alias: all occupy names.
        fs::write(
            fx.agent_root.join("agents/twin.md"),
            "---\nname: project-agent\ndescription: twin\n---\nTWIN",
        )
        .unwrap();
        let builtin = fx.agent_root.join("npm/node_modules/pi-subagents/agents");
        fs::create_dir_all(&builtin).unwrap();
        fs::write(
            fx.agent_root
                .join("npm/node_modules/pi-subagents/package.json"),
            r#"{"name":"pi-subagents"}"#,
        )
        .unwrap();
        fs::write(
            builtin.join("b.md"),
            "---\nname: dup\ndescription: Builtin\n---\nB",
        )
        .unwrap();
        fs::write(
            fx.agent_root.join("agents/dup.md"),
            "---\nname: dup\ndescription: User\n---\nD",
        )
        .unwrap();
        fs::write(
            fx.agent_root.join("agents/weird.md"),
            "---\nname: target\ndescription: weird basename\n---\nW",
        )
        .unwrap();
        fs::write(
            fx.agent_root.join("agents/aliased.md"),
            "---\nname: real-name\naliases: alt-name\ndescription: a\n---\nA",
        )
        .unwrap();
        let snapshot = registered(&fx, "w1", 3);
        let fresh = handle_inventory(
            &json!({"scope": "project", "workspaceId": "w1", "workspaceGeneration": 3}),
            &snapshot,
            &fx.agent_root,
        )
        .unwrap();
        let revision = fresh.inventory_revision.clone();
        let conflict = Err(SubagentsError::InventoryRevisionConflict);
        // Same name in global and project: the exact pair is required.
        let twins = collision_ids_for_name(&fresh, "project-agent");
        assert_eq!(
            twins.len(),
            2,
            "global twin and project original both occupy"
        );
        assert_eq!(
            confirm_create_snapshot(&revision, &[], &fresh, "project-agent"),
            conflict
        );
        assert_eq!(
            confirm_create_snapshot(&revision, &twins[..1], &fresh, "project-agent"),
            conflict,
            "a partial shadow set conflicts"
        );
        assert_eq!(
            confirm_create_snapshot(&revision, &twins, &fresh, "project-agent"),
            Ok(())
        );
        // Order-insensitive: caller may list ids in any order.
        assert_eq!(
            confirm_create_snapshot(
                &revision,
                &[twins[1].clone(), twins[0].clone()],
                &fresh,
                "project-agent"
            ),
            Ok(())
        );
        // Shadow ids unchanged but the inventory revision drifted.
        assert_eq!(
            confirm_create_snapshot(&"0".repeat(64), &twins, &fresh, "project-agent"),
            conflict
        );
        // Builtin + user with the same name: both sources in the set.
        let dups = collision_ids_for_name(&fresh, "dup");
        assert_eq!(dups.len(), 2);
        assert_eq!(
            confirm_create_snapshot(&revision, &dups[..1], &fresh, "dup"),
            conflict
        );
        assert_eq!(
            confirm_create_snapshot(&revision, &dups, &fresh, "dup"),
            Ok(())
        );
        // Content-level lookup: basename "weird" registers runtime name
        // "target", so creating "target" collides via parsed content.
        let targets = collision_ids_for_name(&fresh, "target");
        assert_eq!(targets.len(), 1);
        assert_eq!(
            confirm_create_snapshot(&revision, &[], &fresh, "target"),
            conflict
        );
        assert_eq!(
            confirm_create_snapshot(&revision, &targets, &fresh, "target"),
            Ok(())
        );
        // Alias occupancy blocks the aliased name too.
        let aliased = collision_ids_for_name(&fresh, "alt-name");
        assert_eq!(aliased.len(), 1);
        assert_eq!(
            confirm_create_snapshot(&revision, &aliased, &fresh, "alt-name"),
            Ok(())
        );
    }

    #[test]
    fn create_transaction_publishes_exclusively_without_stray_temp() {
        let fx = make_fixture();
        let scope =
            authorize_scope(&json!({"scope": "global"}), &landing(), &fx.agent_root).unwrap();
        let confirmed = fresh_inventory(&scope, &fx.agent_root).unwrap();
        let content = serialize_definition("fresh-agent", "brand new", "PROMPT BODY");
        let result = create_transaction(
            &scope,
            &fx.agent_root,
            &confirmed,
            "fresh-agent",
            content.as_bytes(),
            &confirmed.inventory_revision,
        )
        .unwrap();
        let file = fx.agent_root.join("agents/fresh-agent.md");
        assert_eq!(fs::read_to_string(&file).unwrap(), content);
        assert_eq!(result["runtimeRestartRequired"], json!(true));
        assert_eq!(
            result["revision"],
            json!(crate::host_config::revision_of(
                &fx.agent_root.join("settings.json")
            ))
        );
        assert!(result["inventory"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["runtimeName"] == json!("fresh-agent")));
        assert!(no_stray_temp(&fx.agent_root.join("agents")));
        // A second publish to the same destination never replaces the file.
        assert_eq!(
            create_transaction(
                &scope,
                &fx.agent_root,
                &confirmed,
                "fresh-agent",
                b"replacement",
                &confirmed.inventory_revision
            )
            .unwrap_err()
            .code(),
            "write_failed"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), content);
        // Injected hard-link failure: no half-file, temp cleaned up.
        let confirmed_after = fresh_inventory(&scope, &fx.agent_root).unwrap();
        *TEST_HOOK_FAIL_HARDLINK.lock().unwrap() =
            Some(fx.agent_root.join("agents/failed-agent.md"));
        let failed = create_transaction(
            &scope,
            &fx.agent_root,
            &confirmed_after,
            "failed-agent",
            content.as_bytes(),
            &confirmed_after.inventory_revision,
        );
        *TEST_HOOK_FAIL_HARDLINK.lock().unwrap() = None;
        assert_eq!(failed.unwrap_err().code(), "write_failed");
        assert!(!fx.agent_root.join("agents/failed-agent.md").exists());
        assert!(no_stray_temp(&fx.agent_root.join("agents")));
    }

    #[test]
    fn create_transaction_conflicts_on_prepublication_drift() {
        let fx = make_fixture();
        let scope =
            authorize_scope(&json!({"scope": "global"}), &landing(), &fx.agent_root).unwrap();
        let confirmed = fresh_inventory(&scope, &fx.agent_root).unwrap();
        // The shadow set for the new name is empty and stays empty; only the
        // inventory revision drifts (an unrelated agent appeared between
        // confirmation and publication).
        fs::write(
            fx.agent_root.join("agents/unrelated.md"),
            "---\nname: unrelated\ndescription: u\n---\nU",
        )
        .unwrap();
        assert_eq!(
            create_transaction(
                &scope,
                &fx.agent_root,
                &confirmed,
                "fresh-agent",
                b"content",
                &confirmed.inventory_revision
            )
            .unwrap_err()
            .code(),
            "inventory_revision_conflict"
        );
        assert!(!fx.agent_root.join("agents/fresh-agent.md").exists());
        assert!(no_stray_temp(&fx.agent_root.join("agents")));
    }

    #[test]
    fn create_transaction_reports_manual_check_after_parse_failure() {
        let fx = make_fixture();
        let scope =
            authorize_scope(&json!({"scope": "global"}), &landing(), &fx.agent_root).unwrap();
        let confirmed = fresh_inventory(&scope, &fx.agent_root).unwrap();
        let error = create_transaction(
            &scope,
            &fx.agent_root,
            &confirmed,
            "broken",
            b"no frontmatter here",
            &confirmed.inventory_revision,
        )
        .unwrap_err();
        assert_eq!(error.code(), "write_failed");
        assert!(error.message().contains("check the file manually"));
        // No false rollback: the published file stays for manual inspection.
        assert_eq!(
            fs::read_to_string(fx.agent_root.join("agents/broken.md")).unwrap(),
            "no frontmatter here"
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_transaction_refuses_symlinked_parent_directory() {
        let fx = make_fixture();
        let elsewhere = fx.root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::remove_dir_all(fx.agent_root.join("agents")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, fx.agent_root.join("agents")).unwrap();
        let scope =
            authorize_scope(&json!({"scope": "global"}), &landing(), &fx.agent_root).unwrap();
        let confirmed = fresh_inventory(&scope, &fx.agent_root).unwrap();
        assert_eq!(
            create_transaction(
                &scope,
                &fx.agent_root,
                &confirmed,
                "esc",
                b"content",
                &confirmed.inventory_revision
            )
            .unwrap_err()
            .code(),
            "write_failed"
        );
        assert!(!elsewhere.join("esc.md").exists());
        assert!(no_stray_temp(&elsewhere));
    }
}
