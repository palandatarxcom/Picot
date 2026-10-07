// ABOUTME: Bounded both-scope subagent disk candidates with metadata-only list and restricted detail.
// ABOUTME: Name-level writes qualify for unique precedence winners in complete disk snapshots.
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = 512 * 1024;
const MAX_FILES: usize = 2_000;
const MAX_DEPTH: usize = 12;

#[derive(Clone, Copy, Debug, Default)]
pub struct ParityEvidence {
    pub verified: bool,
    pub runtime_names_bounded: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub source: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub id: String,
    pub runtime_name: String,
    pub local_name: String,
    pub source: String,
    pub source_scope: String,
    pub package_identity: Option<String>,
    pub file_path: Option<PathBuf>,
    pub parsed_fields: Value,
    pub status: String,
    pub winner_id: Option<String>,
    pub read_only: bool,
    pub native_override_supported: bool,
    pub write_qualified: bool,
    pub write_diagnostic: Option<Diagnostic>,
    pub saved_override: Value,
    pub inferred_value: Option<Value>,
    pub settings_revision: String,
    #[serde(skip)]
    pub(crate) aliases: Vec<String>,
    #[serde(skip)]
    detail_root: Option<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub additional_scopes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionContext {
    pub mode: &'static str,
    pub reason: Option<String>,
    pub project_writes_allowed: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub agent_root: PathBuf,
    pub workspace_root: Option<PathBuf>,
    pub project_root: Option<PathBuf>,
    pub resolution_context: ResolutionContext,
    pub entries: Vec<Candidate>,
    pub diagnostics: Vec<Diagnostic>,
    pub inventory_revision: String,
    pub settings_revisions: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct RootResolution {
    pub project_root: Option<PathBuf>,
    pub project_writes_allowed: bool,
}

pub fn project_root_for_extension(workspace: &Path, settings: &Value) -> RootResolution {
    let workspace = match workspace.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            return RootResolution {
                project_root: None,
                project_writes_allowed: false,
            }
        }
    };
    let mut candidates = vec![];
    for path in workspace.ancestors() {
        if path.join(".pi").is_dir() || path.join(".agents").is_dir() {
            candidates.push(path.to_path_buf());
        }
    }
    let mut selected = candidates.first().cloned();
    if settings
        .pointer("/subagents/projectRootResolution")
        .and_then(Value::as_str)
        == Some("git-root")
    {
        if let Some(git) = workspace
            .ancestors()
            .find(|path| path.join(".git").exists())
        {
            if candidates.iter().any(|candidate| candidate == git) {
                selected = Some(git.to_path_buf());
            }
        }
    }
    RootResolution {
        project_writes_allowed: selected.as_deref() == Some(workspace.as_path()),
        project_root: selected,
    }
}

fn diagnostic(source: &str, message: &str) -> Diagnostic {
    Diagnostic {
        source: source.to_string(),
        message: message.to_string(),
    }
}

fn bounded(path: &Path) -> Result<Vec<u8>, &'static str> {
    let meta = fs::symlink_metadata(path).map_err(|_| "unreadable")?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return Err("non-regular or oversized file");
    }
    let bytes = fs::read(path).map_err(|_| "unreadable")?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("oversized file");
    }
    Ok(bytes)
}

fn settings(path: &Path, diagnostics: &mut Vec<Diagnostic>) -> Value {
    if fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) {
        return json!({});
    }
    match bounded(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
    {
        Some(value) => value,
        None => {
            diagnostics.push(diagnostic(
                "settings",
                "invalid settings; discovery incomplete",
            ));
            json!({})
        }
    }
}

// Pi >=0.76.0 (upstream #2681) serializes a multi-line description as a YAML block
// scalar (`description: |-` plus indented lines), so the marker is not the value and
// indented continuation lines are folded into the description. The parser is still
// permissive rather than full YAML: reject other ambiguous values instead of
// manufacturing a valid winner from a malformed definition.
fn frontmatter(raw: &str) -> Result<(String, String, String, Vec<String>, String), &'static str> {
    let mut lines = raw.lines();
    if lines.next() != Some("---") {
        return Err("missing frontmatter");
    }
    let mut fields = BTreeMap::new();
    let mut closed = false;
    let mut last = String::new();
    let mut block_description = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if !line.starts_with(char::is_whitespace) {
                if !key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err("unsupported frontmatter");
                }
                if matches!(
                    key,
                    "name" | "description" | "package" | "runner" | "alias" | "aliases"
                ) && (value.trim().starts_with('[') || value.trim().starts_with('{'))
                {
                    return Err("unsupported frontmatter");
                }
                last = key.to_string();
                block_description = key == "description"
                    && matches!(value.trim(), "|" | "|-" | "|+" | ">" | ">-" | ">+");
                fields.insert(
                    last.clone(),
                    if block_description {
                        String::new()
                    } else {
                        value
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string()
                    },
                );
                continue;
            }
        }
        if line.trim_start().starts_with("- ") && (last == "aliases" || last == "alias") {
            fields.entry(last.clone()).and_modify(|value: &mut String| {
                value.push(',');
                value.push_str(line.trim_start().trim_start_matches("- "));
            });
        } else if last == "runner"
            && line.starts_with(char::is_whitespace)
            && !line.trim().is_empty()
            && !line.trim_start().starts_with('#')
        {
            let Some((key, value)) = line.trim_start().split_once(':') else {
                return Err("unsupported frontmatter");
            };
            if key.is_empty()
                || !key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err("unsupported frontmatter");
            }
            if key == "type" {
                fields.insert("runner".to_string(), value.trim().to_string());
            }
        } else if block_description
            && line.starts_with(char::is_whitespace)
            && !line.trim().is_empty()
        {
            // ponytail: folds block scalar lines with a single space and drops blank
            // lines; upstream foldBlock preserves those newline breaks, which the
            // single-line inventory display does not need. Upgrade if a caller wants
            // the exact multi-line description back.
            let text = line.trim();
            fields
                .entry("description".to_string())
                .and_modify(|value: &mut String| {
                    if !value.is_empty() {
                        value.push(' ');
                    }
                    value.push_str(text);
                });
        } else if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            return Err("unsupported frontmatter");
        }
    }
    if !closed {
        return Err("unterminated frontmatter");
    }
    let required = |key| {
        fields
            .get(key)
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .ok_or("missing name or description")
    };
    let name = required("name")?;
    let description = required("description")?;
    let package = fields
        .get("package")
        .filter(|v| !v.is_empty() && *v != "false")
        .map(|v| {
            v.to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-")
                .chars()
                .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '.')
                .collect::<String>()
        });
    if package.as_ref().is_some_and(|v| v.is_empty()) {
        return Err("invalid package name");
    }
    let runtime = package.map_or_else(|| name.clone(), |prefix| format!("{prefix}.{name}"));
    let aliases = fields
        .get("aliases")
        .or_else(|| fields.get("alias"))
        .map_or_else(Vec::new, |v| {
            v.split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .collect()
        });
    let runner = fields
        .get("runner")
        .cloned()
        .unwrap_or_else(|| "native".to_string());
    Ok((runtime, name, description, aliases, runner))
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>, diagnostics: &mut Vec<Diagnostic>) {
    fn visit(
        path: &Path,
        depth: usize,
        files: &mut Vec<PathBuf>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        if depth > MAX_DEPTH || files.len() >= MAX_FILES {
            diagnostics.push(diagnostic("scan", "discovery budget exhausted"));
            return;
        }
        let Ok(entries) = fs::read_dir(path) else {
            if path.exists() {
                diagnostics.push(diagnostic("scan", "unreadable source"));
            }
            return;
        };
        let mut entries: Vec<_> = entries
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry),
                Err(_) => {
                    diagnostics.push(diagnostic("scan", "unreadable entry"));
                    None
                }
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if files.len() >= MAX_FILES {
                diagnostics.push(diagnostic("scan", "discovery budget exhausted"));
                break;
            }
            let file = entry.path();
            let Ok(kind) = entry.file_type() else {
                diagnostics.push(diagnostic("scan", "unreadable entry"));
                continue;
            };
            if kind.is_dir() {
                visit(&file, depth + 1, files, diagnostics);
            } else if kind.is_file()
                && file.extension().is_some_and(|ext| ext == "md")
                && !file.to_string_lossy().ends_with(".chain.md")
            {
                files.push(file);
            } else if kind.is_symlink() {
                diagnostics.push(diagnostic("scan", "symlink source omitted"));
            }
        }
    }
    visit(dir, 0, files, diagnostics);
}

fn package_roots(
    dir: &Path,
    settings: &Value,
    roots: &mut BTreeMap<PathBuf, BTreeSet<String>>,
    scope: &str,
) {
    let add = |path: PathBuf, roots: &mut BTreeMap<PathBuf, BTreeSet<String>>| {
        if fs::symlink_metadata(&path)
            .is_ok_and(|meta| meta.file_type().is_dir() || meta.file_type().is_symlink())
        {
            if let Ok(real) = path.canonicalize() {
                if real.is_dir() {
                    roots.entry(real).or_default().insert(scope.to_string());
                }
            }
        }
    };
    add(dir.to_path_buf(), roots); // project root package.json
    let npm = dir.join("npm/node_modules");
    if let Ok(items) = fs::read_dir(npm) {
        for item in items.flatten() {
            if item.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = item.path();
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('@'))
            {
                if let Ok(scoped) = fs::read_dir(path) {
                    for nested in scoped.flatten() {
                        if !nested.file_name().to_string_lossy().starts_with('.') {
                            add(nested.path(), roots);
                        }
                    }
                }
            } else {
                add(path, roots);
            }
        }
    }
    if let Some(packages) = settings.get("packages").and_then(Value::as_array) {
        for item in packages {
            let Some(source) = item
                .as_str()
                .or_else(|| item.get("source").and_then(Value::as_str))
            else {
                continue;
            };
            let source = source.trim();
            let path = if let Some(spec) = source.strip_prefix("npm:") {
                let spec = spec.trim();
                let name = if spec.starts_with('@') {
                    spec.rsplit_once('@')
                        .filter(|(name, _)| name.contains('/'))
                        .map_or(spec, |(name, _)| name)
                } else {
                    spec.split('@').next().unwrap_or("")
                };
                if !safe_package_path(name) || name.starts_with('@') && name.split('/').count() != 2
                {
                    continue;
                }
                dir.join("npm/node_modules").join(name)
            } else if let Some(spec) = source.strip_prefix("git:").or_else(|| {
                // Host extension: pi-subagents 0.74 accepts bare HTTP(S), but not bare ssh://.
                (source.starts_with("ssh://")
                    || source.starts_with("http://")
                    || source.starts_with("https://")
                    || source.starts_with("git@")
                    || source.contains('/')
                        && !source.starts_with("file:")
                        && !source.starts_with("./")
                        && !source.starts_with("../")
                        && !source.starts_with("~/")
                        && !Path::new(source).is_absolute())
                .then_some(source)
            }) {
                let Some(relative) = git_package_path(spec) else {
                    continue;
                };
                dir.join("git").join(relative)
            } else {
                let Some(path) = local_package_path(source, dir) else {
                    continue;
                };
                path
            }; // undeclared system npm/git-cache contents stay opaque (diagnostic below)
            add(path, roots);
        }
    }
}

fn local_package_path(source: &str, dir: &Path) -> Option<PathBuf> {
    let path = source.strip_prefix("file:").unwrap_or(source);
    if path == "~" || path.starts_with("~/") {
        return Some(dirs::home_dir()?.join(path.strip_prefix("~/").unwrap_or("")));
    }
    if Path::new(path).is_absolute()
        || path == "."
        || path == ".."
        || path.starts_with("./")
        || path.starts_with("../")
    {
        return Some(dir.join(path));
    }
    None
}

// Match pi-subagents 0.74 isSafePackagePath for declared package components.
fn safe_package_path(value: &str) -> bool {
    !value.is_empty()
        && !Path::new(value).is_absolute()
        && value
            .split(['/', '\\'])
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

// 0.74 parseBuiltinOverrideEntry ignores entries containing only unknown keys.
fn builtin_override<'a>(settings: &'a Value, name: &str) -> Option<&'a Value> {
    let entry = settings.pointer("/subagents/agentOverrides")?.get(name)?;
    entry
        .as_object()?
        .iter()
        .any(|(key, value)| match key.as_str() {
            "disabled" => value.is_boolean(),
            "model" | "thinking" => value.is_string() || value == &json!(false),
            "description"
            | "defaultProvider"
            | "fast"
            | "tools"
            | "skills"
            | "systemPrompt"
            | "systemPromptMode"
            | "outputMode"
            | "toolBudget"
            | "machine"
            | "defaultReads"
            | "inheritProjectContext"
            | "inheritGlobalContext"
            | "inheritSkills"
            | "defaultContext"
            | "acceptanceRole"
            | "excludeTools"
            | "allowNestedSubagents"
            | "allowedAgents"
            | "extensions"
            | "subagentOnlyExtensions"
            | "mutationTools" => !value.is_null(),
            _ => false,
        })
        .then_some(entry)
}

fn is_builtin_disabled(
    name: &str,
    user: &Value,
    project: &Value,
    bulk_disabled: bool,
    has_project: bool,
) -> bool {
    if let Some(entry) = has_project
        .then(|| builtin_override(project, name))
        .flatten()
    {
        return entry
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    }
    if has_project && project.pointer("/subagents/disableBuiltins") == Some(&json!(true)) {
        return true;
    }
    if let Some(entry) = builtin_override(user, name) {
        return entry
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    }
    bulk_disabled
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Collision {
    Alias,
    SameLevel,
    Shadowed,
}

fn source_rank(source: &str) -> u8 {
    match source {
        "builtin" => 0,
        "package" => 1,
        "user" => 2,
        "project" => 3,
        _ => 0,
    }
}

// Shared by inventory qualification and fresh-scan write gate. Alias crosses
// remain ambiguous even if a runtime-name winner has higher precedence.
pub(crate) fn collision(entries: &[Candidate], candidate: &Candidate) -> Option<Collision> {
    let mut highest = source_rank(&candidate.source);
    let mut highest_count = 1;
    for other in entries.iter().filter(|other| other.id != candidate.id) {
        let alias_cross = other.runtime_name != candidate.runtime_name
            && (candidate
                .aliases
                .iter()
                .any(|alias| alias == &other.runtime_name || other.aliases.contains(alias))
                || other.aliases.contains(&candidate.runtime_name));
        if alias_cross {
            return Some(Collision::Alias);
        }
        if other.runtime_name == candidate.runtime_name {
            let rank = source_rank(&other.source);
            if rank > highest {
                highest = rank;
                highest_count = 1;
            } else if rank == highest {
                highest_count += 1;
            }
        }
    }
    if highest > source_rank(&candidate.source) {
        Some(Collision::Shadowed)
    } else if highest_count > 1 {
        Some(Collision::SameLevel)
    } else {
        None
    }
}

// Mirrors pi-subagents 0.74 parseGitPackagePath; rejects raw and decoded traversal
// before Url parsing can normalize away dot segments.
fn git_package_path(spec: &str) -> Option<PathBuf> {
    let spec = spec.trim();
    let (host, repo) = if let Some(rest) = spec.strip_prefix("git@") {
        let (host, repo) = rest.split_once(':')?;
        (host.to_string(), repo.to_string())
    } else if spec.contains("://") {
        let raw = spec.split_once("://")?.1.split_once('/')?.1;
        if !safe_package_path(raw)
            || !safe_package_path(
                &percent_encoding::percent_decode_str(raw)
                    .decode_utf8()
                    .ok()?,
            )
        {
            return None;
        }
        let url = reqwest::Url::parse(spec).ok()?;
        (
            url.host_str()?.to_string(),
            url.path().trim_start_matches('/').to_string(),
        )
    } else {
        let (host, repo) = spec.split_once('/')?;
        (host.to_string(), repo.to_string())
    };
    if !safe_package_path(&host)
        || host.contains([':', '@', '%'])
        || !safe_package_path(&repo)
        || !safe_package_path(
            &percent_encoding::percent_decode_str(&repo)
                .decode_utf8()
                .ok()?,
        )
    {
        return None;
    }
    let repo = repo.split(['@', '#']).next()?.trim_end_matches(".git");
    if !safe_package_path(repo) || repo.split('/').count() < 2 {
        return None;
    }
    Some(PathBuf::from(host).join(repo))
}

pub fn inventory(
    agent_root: &Path,
    workspace: Option<&Path>,
    parity: ParityEvidence,
) -> Result<Inventory, String> {
    let agent_root = agent_root
        .canonicalize()
        .map_err(|_| "agent root unavailable".to_string())?;
    let workspace_root = workspace
        .map(Path::canonicalize)
        .transpose()
        .map_err(|_| "workspace root unavailable".to_string())?;
    let mut diagnostics = Vec::new();
    let user = settings(&agent_root.join("settings.json"), &mut diagnostics);
    let root = workspace_root
        .as_ref()
        .map(|cwd| project_root_for_extension(cwd, &json!({})));
    let project_root = root.as_ref().and_then(|r| r.project_root.clone());
    let project = project_root
        .as_ref()
        .map(|root| settings(&root.join(".pi/settings.json"), &mut diagnostics))
        .unwrap_or_else(|| json!({}));
    let root = workspace_root
        .as_ref()
        .map(|cwd| project_root_for_extension(cwd, &project));
    let project_root = root.as_ref().and_then(|r| r.project_root.clone());
    let writes_allowed = root.as_ref().is_none_or(|r| r.project_writes_allowed);
    if !writes_allowed {
        diagnostics.push(diagnostic(
            "project",
            "extension project root differs from workspace root; project writes disabled",
        ));
    }
    let mut settings_revisions = BTreeMap::new();
    settings_revisions.insert(
        "global".into(),
        crate::host_config::revision_of(&agent_root.join("settings.json")),
    );
    if let Some(root) = &workspace_root {
        settings_revisions.insert(
            "project".into(),
            crate::host_config::revision_of(&root.join(".pi/settings.json")),
        );
    }
    let mut roots = BTreeMap::<PathBuf, BTreeSet<String>>::new();
    package_roots(&agent_root, &user, &mut roots, "user");
    if let Some(project_root) = &project_root {
        package_roots(&project_root.join(".pi"), &project, &mut roots, "project");
        // Project-root manifest only: settings sources resolve exclusively from .pi.
        roots
            .entry(project_root.clone())
            .or_default()
            .insert("root".into());
    }
    // Out-of-scope sources remain opaque, even if enumerable: never open their .md files.
    for source in [
        ".agents/",
        "agentScanDirs",
        "PI_SUBAGENT_EXTRA_AGENT_DIRS",
        "runtime registrations",
        "system npm / git cache",
    ] {
        diagnostics.push(diagnostic(
            source,
            "out-of-scope occupancy not verified; no definition body read",
        ));
    }
    let mut entries = Vec::new();
    let mut seen_files = BTreeMap::<PathBuf, usize>::new();
    struct SourceDir {
        directory: PathBuf,
        source: &'static str,
        source_scope: &'static str,
        package_identity: Option<String>,
        builtin: bool,
        extra_scopes: Vec<String>,
    }
    impl SourceDir {
        fn new(directory: PathBuf, source: &'static str, source_scope: &'static str) -> Self {
            Self {
                directory,
                source,
                source_scope,
                package_identity: None,
                builtin: false,
                extra_scopes: vec![],
            }
        }
    }
    let mut sources: Vec<SourceDir> =
        vec![SourceDir::new(agent_root.join("agents"), "user", "global")];
    if let Some(root) = &project_root {
        sources.push(SourceDir::new(
            root.join(".pi/agents"),
            "project",
            "project",
        ));
    }
    // Builtins are bundled with the installed extension; if unavailable they are not invented.
    // Installed metadata is scanned even under bulk disable; editing model must not revive it.
    let builtin_disabled = match project_root
        .is_some()
        .then(|| {
            project
                .pointer("/subagents/disableBuiltins")
                .and_then(Value::as_bool)
        })
        .flatten()
    {
        Some(flag) => flag,
        None => user
            .pointer("/subagents/disableBuiltins")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    let builtin_dir = agent_root.join("npm/node_modules/pi-subagents/agents");
    {
        let mut builtin_source = SourceDir::new(builtin_dir.clone(), "builtin", "global");
        builtin_source.builtin = true;
        sources.push(builtin_source);
        if !builtin_dir.is_dir() {
            diagnostics.push(diagnostic(
                "builtin",
                "installed extension builtins unavailable; names unknown",
            ));
        }
    }
    for (root, scopes) in roots {
        let path_label = || {
            let name = root.file_name().unwrap_or_default().to_string_lossy();
            let parent = root
                .parent()
                .and_then(Path::file_name)
                .unwrap_or_default()
                .to_string_lossy();
            format!("{parent}/{name}")
        };
        let manifest = root.join("package.json");
        let Ok(bytes) = bounded(&manifest) else {
            if !fs::symlink_metadata(&manifest)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                diagnostics.push(diagnostic(
                    "package",
                    &format!("unreadable or oversized manifest; package {}", path_label()),
                ));
            }
            continue;
        };
        let Ok(pkg) = serde_json::from_slice::<Value>(&bytes) else {
            diagnostics.push(diagnostic(
                "package",
                &format!("invalid manifest; package {}", path_label()),
            ));
            continue;
        };
        let identity = pkg
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unnamed")
            .to_string();
        for key in [pkg.get("pi-subagents"), pkg.pointer("/pi/subagents")]
            .into_iter()
            .flatten()
        {
            if let Some(paths) = key.get("agents").and_then(Value::as_array) {
                for relative in paths.iter().filter_map(Value::as_str) {
                    let directory = root.join(relative);
                    if directory
                        .canonicalize()
                        .ok()
                        .is_some_and(|path| path.starts_with(&root))
                    {
                        let scope = if scopes.contains("project") || scopes.contains("root") {
                            "project"
                        } else {
                            "global"
                        };
                        // One physical root shared by user and project gets one identity
                        // with dual-source annotation instead of duplicate entries.
                        let extra = if scope == "project" && scopes.contains("user") {
                            vec!["global".to_string()]
                        } else {
                            vec![]
                        };
                        let mut package_source = SourceDir::new(directory, "package", scope);
                        package_source.package_identity = Some(identity.clone());
                        package_source.extra_scopes = extra;
                        sources.push(package_source);
                    } else {
                        diagnostics.push(diagnostic(
                            "package",
                            &format!(
                                "manifest path outside package root; package {}",
                                pkg.get("name")
                                    .and_then(Value::as_str)
                                    .filter(|name| !name.is_empty())
                                    .map_or_else(path_label, str::to_owned)
                            ),
                        ));
                    }
                }
            }
        }
    }
    for item in sources {
        let SourceDir {
            directory,
            source,
            source_scope,
            package_identity,
            builtin,
            extra_scopes,
        } = item;
        let directory = match directory.canonicalize() {
            Ok(path) => path,
            Err(_) => {
                if !fs::symlink_metadata(&directory)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    diagnostics.push(diagnostic("scan", "unreadable source"));
                }
                continue;
            }
        };
        let mut files = vec![];
        walk(&directory, &mut files, &mut diagnostics);
        for file in files {
            let Ok(canonical) = file.canonicalize() else {
                diagnostics.push(diagnostic("scan", "unreadable entry"));
                continue;
            };
            if !canonical.starts_with(directory.canonicalize().unwrap_or_default()) {
                diagnostics.push(diagnostic("scan", "entry escaped source"));
                continue;
            }
            if let Some(index) = seen_files.get(&canonical) {
                let entry: &mut Candidate = &mut entries[*index];
                if !entry
                    .additional_scopes
                    .iter()
                    .any(|scope| scope == source_scope)
                    && entry.source_scope != source_scope
                {
                    entry.additional_scopes.push(source_scope.to_string());
                }
                continue;
            }
            let raw = match bounded(&canonical)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
            {
                Some(raw) => raw,
                None => {
                    diagnostics.push(diagnostic(
                        source,
                        "unreadable, oversized or non-UTF-8 definition",
                    ));
                    continue;
                }
            };
            let (runtime_name, local_name, description, aliases, runner) = match frontmatter(&raw) {
                Ok(fields) => fields,
                Err(reason) => {
                    diagnostics.push(diagnostic(source, reason));
                    continue;
                }
            };
            let mut hasher = Sha256::new();
            hasher.update(canonical.to_string_lossy().as_bytes());
            hasher.update(runtime_name.as_bytes());
            hasher.update(local_name.as_bytes());
            hasher.update(description.as_bytes());
            for alias in &aliases {
                hasher.update(alias.as_bytes());
            }
            hasher.update(runner.as_bytes());
            let id = format!("{:x}", hasher.finalize());
            let layer = if workspace_root.is_some() {
                &project
            } else {
                &user
            };
            let saved_override = layer
                .pointer("/subagents/agentOverrides")
                .and_then(|v| v.get(&runtime_name))
                .map(|v| {
                    json!({
                        "model": v.get("model"),
                        "thinking": v.get("thinking"),
                        "advertise": v.get("advertise"),
                        "disabled": v.get("disabled"),
                    })
                })
                .unwrap_or_else(|| json!({}));
            // All rows and the inventory token use the same write-layer revision.
            let settings_revision = settings_revisions
                .get(if workspace_root.is_some() {
                    "project"
                } else {
                    "global"
                })
                .cloned()
                .expect("write layer revision was computed");
            seen_files.insert(canonical.clone(), entries.len());
            let disabled = builtin
                && is_builtin_disabled(
                    &runtime_name,
                    &user,
                    &project,
                    builtin_disabled,
                    project_root.is_some(),
                );
            if disabled {
                diagnostics.push(diagnostic(
                    "builtin",
                    "builtin disabled by settings; edit disabled state separately",
                ));
            }
            entries.push(Candidate { id, runtime_name, local_name: local_name.clone(), source: source.to_string(), source_scope: source_scope.to_string(), package_identity: package_identity.clone(), file_path: (!builtin).then_some(canonical), parsed_fields: json!({ "name": local_name, "description": description, "runner": runner }), status: if disabled { "disabled" } else { "candidate" }.into(), winner_id: None, read_only: true, native_override_supported: runner == "native", write_qualified: false, write_diagnostic: disabled.then(|| diagnostic("builtin", "builtin disabled by settings; edit disabled state separately")), saved_override, inferred_value: None, settings_revision, aliases, detail_root: (!builtin).then_some(directory.clone()), additional_scopes: extra_scopes.clone() });
        }
    }
    entries.sort_by(|a, b| {
        a.runtime_name
            .cmp(&b.runtime_name)
            .then(source_rank(&a.source).cmp(&source_rank(&b.source)))
            .then(a.id.cmp(&b.id))
    });
    let collisions: Vec<Option<Collision>> = entries
        .iter()
        .map(|entry| collision(&entries, entry))
        .collect();
    if collisions.iter().any(Option::is_some) {
        diagnostics.push(diagnostic(
            "collision",
            "duplicate runtime name or alias; runtime winner unverified",
        ));
    }
    let mut hash = Sha256::new();
    for entry in &entries {
        hash.update(entry.id.as_bytes());
        hash.update(entry.source_scope.as_bytes());
        for scope in &entry.additional_scopes {
            hash.update(scope.as_bytes());
        }
        for alias in &entry.aliases {
            hash.update(alias.as_bytes());
        }
    }
    for item in &diagnostics {
        hash.update(item.source.as_bytes());
        hash.update(item.message.as_bytes());
    }
    // A boolean alone cannot supply a live snapshot or prove runtime registrations.
    if parity.verified || parity.runtime_names_bounded {
        diagnostics.push(diagnostic(
            "parity",
            "live snapshot not supplied; disk candidates only",
        ));
    }
    // Any in-scope unreadable/invalid source could hide a competing name.
    let incomplete = diagnostics.iter().any(|d| {
        !((d.source == "package"
            && [
                "unreadable or oversized manifest;",
                "invalid manifest;",
                "manifest path outside package root;",
            ]
            .iter()
            .any(|message| d.message.starts_with(message)))
            || matches!(
                d.source.as_str(),
                ".agents/"
                    | "agentScanDirs"
                    | "PI_SUBAGENT_EXTRA_AGENT_DIRS"
                    | "runtime registrations"
                    | "system npm / git cache"
                    | "collision"
                    | "project"
                    | "parity"
                    | "builtin"
            ))
    });
    for (entry, collision) in entries.iter_mut().zip(collisions) {
        if entry.write_diagnostic.is_some() {
            continue;
        }
        entry.write_diagnostic = if !entry.native_override_supported {
            Some(diagnostic("runner", "external or unknown runner"))
        } else if let Some(collision) = collision {
            Some(diagnostic(
                "collision",
                match collision {
                    Collision::Alias => "name or alias occupied by another candidate",
                    Collision::SameLevel => {
                        "same-level duplicate name; scan-order winner unverifiable"
                    }
                    Collision::Shadowed => "shadowed by a higher-precedence definition",
                },
            ))
        } else if incomplete {
            Some(diagnostic("scan", "in-scope discovery incomplete"))
        } else {
            None
        };
        entry.write_qualified = entry.write_diagnostic.is_none();
    }
    let mode = "disk-candidates-only";
    Ok(Inventory {
        agent_root,
        workspace_root,
        project_root,
        resolution_context: ResolutionContext {
            mode,
            reason: Some("live /run winner and runtime registrations not observable".into()),
            project_writes_allowed: writes_allowed,
        },
        entries,
        diagnostics,
        inventory_revision: format!("{:x}", hash.finalize()),
        settings_revisions,
    })
}

/// Discovery-parser confirmation for a serialized definition: returns the
/// runtime name and description the bounded frontmatter parser would read
/// back. Malformed or ambiguous input errors; never invents fields.
pub(crate) fn parse_definition(raw: &str) -> Result<(String, String), &'static str> {
    frontmatter(raw).map(|(runtime, _, description, _, _)| (runtime, description))
}

pub fn resolve_candidate<'a>(inventory: &'a Inventory, id: &str) -> Option<&'a Candidate> {
    inventory.entries.iter().find(|entry| entry.id == id)
}

/// Caller must first authorize scope and rescan with inventory(); IDs are not paths.
pub fn detail(inventory: &Inventory, id: &str) -> Result<String, &'static str> {
    let candidate = resolve_candidate(inventory, id).ok_or("candidate_stale")?;
    let file = candidate.file_path.as_ref().ok_or("candidate_stale")?;
    let scope_root = candidate.detail_root.as_ref().ok_or("candidate_stale")?;
    if !file.starts_with(scope_root) || file.canonicalize().map_err(|_| "candidate_stale")? != *file
    {
        return Err("candidate_stale");
    }
    let raw = String::from_utf8(bounded(file).map_err(|_| "candidate_stale")?)
        .map_err(|_| "candidate_stale")?;
    let (runtime, name, description, aliases, runner) =
        frontmatter(&raw).map_err(|_| "candidate_stale")?;
    if runtime != candidate.runtime_name
        || name != candidate.local_name
        || aliases != candidate.aliases
        || candidate.parsed_fields
            != json!({"name": name, "description": description, "runner": runner})
    {
        return Err("candidate_stale");
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subagents_inventory_both_scope_and_metadata_detail() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(agent.join("agents/nested")).unwrap();
        fs::create_dir_all(workspace.join(".pi/agents")).unwrap();
        let raw =
            "---\nname: different\ndescription: actual\naliases: shortcut\n---\nsecret prompt  \n";
        fs::write(agent.join("agents/nested/filename.md"), raw).unwrap();
        fs::write(agent.join("agents/nested/no.chain.md"), raw).unwrap();
        fs::write(
            workspace.join(".pi/agents/other.md"),
            "---\nname: different\ndescription: project\n---\nbody",
        )
        .unwrap();
        let result = inventory(&agent, Some(&workspace), ParityEvidence::default()).unwrap();
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.resolution_context.mode, "disk-candidates-only");
        assert!(result
            .entries
            .iter()
            .all(|e| e.winner_id.is_none() && e.status == "candidate"));
        let project = result
            .entries
            .iter()
            .find(|e| e.source == "project")
            .unwrap();
        let user = result.entries.iter().find(|e| e.source == "user").unwrap();
        assert!(project.write_qualified);
        assert!(!user.write_qualified);
        assert_eq!(
            user.write_diagnostic.as_ref().unwrap().message,
            "shadowed by a higher-precedence definition"
        );
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("secret prompt"));
        assert_eq!(
            detail(&result, &result.entries[0].id).unwrap_or_else(|_| detail(
                &result,
                &result.entries[1].id
            )
            .unwrap()),
            raw
        );
        assert!(detail(&result, "../../etc/passwd").is_err());
    }
    #[test]
    fn subagents_inventory_precedence_alias_and_package_root_filter() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let project = root.path().join("project");
        let npm = agent.join("npm/node_modules");
        fs::create_dir_all(agent.join("agents")).unwrap();
        fs::create_dir_all(project.join(".pi/agents")).unwrap();
        fs::create_dir_all(npm.join("pi-subagents/agents")).unwrap();
        fs::create_dir_all(npm.join("pkg/agents")).unwrap();
        fs::create_dir_all(npm.join(".bin")).unwrap();
        fs::write(npm.join(".package-lock.json"), "{}").unwrap();
        fs::write(npm.join("ordinary-file"), "{}").unwrap();
        fs::write(npm.join(".bin/package.json"), "invalid").unwrap();
        fs::write(
            npm.join("pkg/package.json"),
            r#"{"name":"pkg","pi-subagents":{"agents":["agents"]}}"#,
        )
        .unwrap();
        let definition = |name: &str, alias: &str| {
            format!("---\nname: {name}\ndescription: D\nalias: {alias}\n---\nbody")
        };
        fs::write(
            npm.join("pi-subagents/agents/b.md"),
            definition("builtin-user", ""),
        )
        .unwrap();
        fs::write(
            npm.join("pi-subagents/agents/c.md"),
            definition("builtin-package", ""),
        )
        .unwrap();
        fs::write(
            npm.join("pkg/agents/c.md"),
            definition("builtin-package", ""),
        )
        .unwrap();
        fs::write(
            npm.join("pkg/agents/u.md"),
            definition("user-package", "shared-name-alias"),
        )
        .unwrap();
        fs::write(agent.join("agents/b.md"), definition("builtin-user", "")).unwrap();
        fs::write(
            agent.join("agents/u.md"),
            definition("user-package", "shared-name-alias"),
        )
        .unwrap();
        fs::write(agent.join("agents/p.md"), definition("project-user", "")).unwrap();
        fs::write(
            project.join(".pi/agents/p.md"),
            definition("project-user", ""),
        )
        .unwrap();
        let result = inventory(&agent, Some(&project), ParityEvidence::default()).unwrap();
        for (name, winner) in [
            ("builtin-user", "user"),
            ("builtin-package", "package"),
            ("user-package", "user"),
            ("project-user", "project"),
        ] {
            let matches: Vec<_> = result
                .entries
                .iter()
                .filter(|e| e.runtime_name == name)
                .collect();
            assert_eq!(matches.len(), 2, "{name}");
            for entry in matches {
                assert_eq!(
                    entry.write_qualified,
                    entry.source == winner,
                    "{name}: {:?}",
                    entry.write_diagnostic
                );
                if entry.source != winner {
                    assert_eq!(
                        entry.write_diagnostic.as_ref().unwrap().message,
                        "shadowed by a higher-precedence definition"
                    );
                }
            }
        }
        assert!(!result.diagnostics.iter().any(|d| d.source == "package"
            && (d.message.contains(".package-lock.json")
                || d.message.contains(".bin")
                || d.message.contains("ordinary-file"))));
        fs::write(
            agent.join("agents/duplicate.md"),
            definition("user-package", ""),
        )
        .unwrap();
        let duplicate = inventory(&agent, Some(&project), ParityEvidence::default()).unwrap();
        for entry in duplicate
            .entries
            .iter()
            .filter(|e| e.runtime_name == "user-package" && e.source == "user")
        {
            assert!(!entry.write_qualified);
            assert_eq!(
                entry.write_diagnostic.as_ref().unwrap().message,
                "same-level duplicate name; scan-order winner unverifiable"
            );
        }
        fs::write(
            agent.join("agents/alias.md"),
            definition("alias-other", "project-user"),
        )
        .unwrap();
        let ambiguous = inventory(&agent, Some(&project), ParityEvidence::default()).unwrap();
        for entry in ambiguous
            .entries
            .iter()
            .filter(|e| e.runtime_name == "project-user" || e.runtime_name == "alias-other")
        {
            assert!(!entry.write_qualified);
            assert_eq!(
                entry.write_diagnostic.as_ref().unwrap().message,
                "name or alias occupied by another candidate"
            );
        }
        fs::write(
            agent.join("agents/alias.md"),
            definition("alias-other", "shared"),
        )
        .unwrap();
        fs::write(
            project.join(".pi/agents/alias.md"),
            definition("alias-project", "shared"),
        )
        .unwrap();
        let ambiguous = inventory(&agent, Some(&project), ParityEvidence::default()).unwrap();
        for entry in ambiguous
            .entries
            .iter()
            .filter(|e| e.runtime_name == "alias-other" || e.runtime_name == "alias-project")
        {
            assert!(!entry.write_qualified);
            assert_eq!(
                entry.write_diagnostic.as_ref().unwrap().message,
                "name or alias occupied by another candidate"
            );
        }
    }

    #[test]
    fn subagents_inventory_extra_sources_never_read_body() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        fs::create_dir_all(agent.join(".agents")).unwrap();
        fs::write(agent.join(".agents/hidden.md"), "PRIVATE_SENTINEL").unwrap();
        fs::write(
            agent.join("settings.json"),
            r#"{"subagents":{"agentScanDirs":[".agents"]}}"#,
        )
        .unwrap();
        let snapshot = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert!(snapshot.entries.is_empty());
        assert!(!serde_json::to_string(&snapshot)
            .unwrap()
            .contains("PRIVATE_SENTINEL"));
        assert!(snapshot
            .diagnostics
            .iter()
            .any(|d| d.source == "runtime registrations"));
    }
    #[test]
    fn subagents_inventory_manifests_and_root_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let workspace = root.path().join("workspace");
        let nested = workspace.join("nested");
        fs::create_dir_all(agent.join("npm/node_modules/one/agents")).unwrap();
        fs::create_dir_all(workspace.join(".pi")).unwrap();
        fs::create_dir_all(nested.join(".pi")).unwrap();
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::write(
            nested.join(".pi/settings.json"),
            r#"{"subagents":{"projectRootResolution":"git-root"}}"#,
        )
        .unwrap();
        fs::write(
            agent.join("npm/node_modules/one/package.json"),
            r#"{"name":"one","pi-subagents":{"agents":["agents"]}}"#,
        )
        .unwrap();
        fs::write(
            agent.join("npm/node_modules/one/agents/a.md"),
            "---\nname: a\ndescription: A\npackage: one\n---\nbody",
        )
        .unwrap();
        let result = inventory(&agent, Some(&nested), ParityEvidence::default()).unwrap();
        assert_eq!(result.entries[0].runtime_name, "one.a");
        assert!(!result.resolution_context.project_writes_allowed);
    }
    #[test]
    fn subagents_inventory_empty_roots_and_invalid_definitions() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        fs::create_dir_all(&agent).unwrap();
        let empty = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert!(empty.entries.is_empty());
        assert_eq!(
            empty.settings_revisions.get("global"),
            Some(&crate::host_config::MISSING_REVISION.to_string())
        );
        assert!(empty
            .diagnostics
            .iter()
            .any(|d| d.source == "builtin" && d.message.contains("unavailable")));
        fs::create_dir_all(agent.join("agents")).unwrap();
        fs::write(
            agent.join("agents/missing-desc.md"),
            "---\nname: x\n---\nbody",
        )
        .unwrap();
        fs::write(
            agent.join("agents/missing-name.md"),
            "---\ndescription: d\n---\nbody",
        )
        .unwrap();
        fs::write(
            agent.join("agents/bad-key.md"),
            "---\nbad key: v\nname: y\ndescription: d\n---\nbody",
        )
        .unwrap();
        fs::write(
            agent.join("agents/nonutf8.md"),
            b"---\nname: \xff\xfe\ndescription: d\n---\nbody",
        )
        .unwrap();
        let mut oversized = String::from("---\nname: big\ndescription: d\n---\n");
        oversized.push_str(&"x".repeat(600 * 1024));
        fs::write(agent.join("agents/oversize.md"), oversized).unwrap();
        let snapshot = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert!(snapshot.entries.is_empty());
        for needle in [
            "missing name or description",
            "unsupported frontmatter",
            "unreadable, oversized or non-UTF-8 definition",
        ] {
            assert!(
                snapshot.diagnostics.iter().any(|d| d.message == needle),
                "missing diagnostic: {needle}"
            );
        }
    }
    #[test]
    fn subagents_inventory_manifest_keys_git_file_and_project_root_packages() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(agent.join("npm/node_modules/npm-pkg/agents")).unwrap();
        fs::write(
            agent.join("npm/node_modules/npm-pkg/package.json"),
            r#"{"name":"npm-pkg","pi":{"subagents":{"agents":["agents"]}}}"#,
        )
        .unwrap();
        fs::write(
            agent.join("npm/node_modules/npm-pkg/agents/n.md"),
            "---\nname: n\ndescription: N\n---\nbody",
        )
        .unwrap();
        fs::create_dir_all(root.path().join("file-pkg/agents")).unwrap();
        fs::write(
            root.path().join("file-pkg/package.json"),
            r#"{"name":"file-pkg","pi-subagents":{"agents":["agents"]}}"#,
        )
        .unwrap();
        fs::write(
            root.path().join("file-pkg/agents/f.md"),
            "---\nname: f\ndescription: F\n---\nbody",
        )
        .unwrap();
        fs::create_dir_all(agent.join("git/github.com/org/git-pkg/agents")).unwrap();
        fs::write(
            agent.join("git/github.com/org/git-pkg/package.json"),
            r#"{"name":"git-pkg","pi-subagents":{"agents":["agents"]}}"#,
        )
        .unwrap();
        fs::write(
            agent.join("git/github.com/org/git-pkg/agents/g.md"),
            "---\nname: g\ndescription: G\n---\nbody",
        )
        .unwrap();
        fs::write(
            agent.join("settings.json"),
            r#"{"packages":["file:../file-pkg","git:github.com/org/git-pkg"]}"#,
        )
        .unwrap();
        fs::create_dir_all(workspace.join(".pi/agents")).unwrap();
        fs::create_dir_all(workspace.join("rootpkg/agents")).unwrap();
        fs::write(
            workspace.join("package.json"),
            r#"{"name":"rootpkg","pi-subagents":{"agents":["rootpkg/agents"]}}"#,
        )
        .unwrap();
        fs::write(
            workspace.join("rootpkg/agents/r.md"),
            "---\nname: r\ndescription: R\n---\nbody",
        )
        .unwrap();
        let result = inventory(&agent, Some(&workspace), ParityEvidence::default()).unwrap();
        let names: Vec<&str> = result
            .entries
            .iter()
            .map(|e| e.runtime_name.as_str())
            .collect();
        for expected in ["n", "f", "g", "r"] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        let package_entries: Vec<_> = result
            .entries
            .iter()
            .filter(|e| e.source == "package")
            .collect();
        assert!(package_entries.len() >= 4);
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.source.contains("system npm")));
    }
    #[test]
    fn subagents_inventory_declared_sources_matrix() {
        let temp = tempfile::tempdir().unwrap();
        let agent = temp.path().join("agent");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(workspace.join(".pi")).unwrap();
        let cases = [
            (
                "ssh://git@host.example:2224/org/bare.git",
                "git/host.example/org/bare",
            ),
            // Host extension: pi-subagents 0.74 rejects bare ssh://; host accepts it.
            (
                "ssh://git@host.example:2224/org/plain",
                "git/host.example/org/plain",
            ),
            (
                "git:ssh://git@host.example:2224/org/explicit.git#main",
                "git/host.example/org/explicit",
            ),
            (
                "git@host.example:org/scp.git@branch",
                "git/host.example/org/scp",
            ),
            ("host.example/org/short.git", "git/host.example/org/short"),
            (
                "git:host.example/org/prefixed.git",
                "git/host.example/org/prefixed",
            ),
            (
                "https://git@host.example:443/org/web.git#v1",
                "git/host.example/org/web",
            ),
            (
                "git:http://host.example:80/org/insecure.git@v1",
                "git/host.example/org/insecure",
            ),
            ("npm:@org/scoped@1.2.3", "npm/node_modules/@org/scoped"),
        ];
        for (index, (source, relative)) in cases.iter().enumerate() {
            let base = if index % 2 == 0 {
                &agent
            } else {
                &workspace.join(".pi")
            };
            let package = base.join(relative);
            fs::create_dir_all(package.join("agents")).unwrap();
            fs::write(
                package.join("package.json"),
                r#"{"name":"fixture","pi-subagents":{"agents":["./agents"]}}"#,
            )
            .unwrap();
            fs::write(
                package.join("agents/a.md"),
                format!("---\nname: case{index}\ndescription: D\n---\nbody"),
            )
            .unwrap();
            let setting = if index % 2 == 0 {
                agent.join("settings.json")
            } else {
                workspace.join(".pi/settings.json")
            };
            let declaration = if index % 2 == 0 {
                json!([source])
            } else {
                json!([{"source": format!("  {source}  ")}])
            };
            fs::write(&setting, json!({"packages": declaration}).to_string()).unwrap();
            let result = inventory(&agent, Some(&workspace), ParityEvidence::default()).unwrap();
            assert!(
                result
                    .entries
                    .iter()
                    .any(|e| e.runtime_name == format!("case{index}")),
                "missing {source}"
            );
        }
        let home = dirs::home_dir().unwrap();
        // Test ~ expansion without touching user home: test parser path directly.
        assert_eq!(
            local_package_path("~/some-package", &agent),
            Some(home.join("some-package"))
        );
        assert_eq!(
            local_package_path("file:~/some-package", &agent),
            Some(home.join("some-package"))
        );
        assert_eq!(
            local_package_path("file:../shared", &agent),
            Some(agent.join("../shared"))
        );
        for (base, source, relative, name) in [
            (&agent, "file:../outside", "../outside", "local-user"),
            (&workspace.join(".pi"), "./local", "local", "local-project"),
        ] {
            let package = base.join(relative);
            fs::create_dir_all(package.join("agents")).unwrap();
            fs::write(
                package.join("package.json"),
                r#"{"name":"local","pi-subagents":{"agents":["agents"]}}"#,
            )
            .unwrap();
            fs::write(
                package.join("agents/local.md"),
                format!("---\nname: {name}\ndescription: D\n---\nbody"),
            )
            .unwrap();
            let setting = if base == &agent {
                agent.join("settings.json")
            } else {
                workspace.join(".pi/settings.json")
            };
            fs::write(setting, json!({"packages":[{"source":source}]}).to_string()).unwrap();
            assert!(
                inventory(&agent, Some(&workspace), ParityEvidence::default())
                    .unwrap()
                    .entries
                    .iter()
                    .any(|e| e.runtime_name == name)
            );
        }
    }

    #[test]
    fn subagents_inventory_real_ssh_fixture_and_negative_cases() {
        let temp = tempfile::tempdir().unwrap();
        let agent = temp.path().join("agent");
        let package = agent.join("git/project.palandata.com/palan/picot4rx/datarx-agents-team");
        fs::create_dir_all(package.join("agents")).unwrap();
        fs::write(agent.join("settings.json"), json!({"packages": ["ssh://git@project.palandata.com:2224/palan/picot4rx/datarx-agents-team.git"]}).to_string()).unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"name":"datarx-agents-team","pi-subagents":{"agents":["./agents"]}}"#,
        )
        .unwrap();
        for (file, name, alias) in [
            ("drudge", "苦力", "drudge"),
            ("runner", "跑者", "runner"),
            ("artifacts-analyzer", "产物分析师", "artifacts-analyzer"),
        ] {
            fs::write(package.join(format!("agents/{file}.md")), format!("---\nname: {name}\ndescription: fixture\nalias: {alias}\n---\nSECRET_PROMPT_{file}")).unwrap();
        }
        let hidden = agent.join("git/host.example/org/hidden");
        fs::create_dir_all(hidden.join("agents")).unwrap();
        fs::write(
            hidden.join("package.json"),
            r#"{"name":"hidden","pi-subagents":{"agents":["agents"]}}"#,
        )
        .unwrap();
        fs::write(
            hidden.join("agents/hidden.md"),
            "---\nname: hidden\ndescription: hidden\n---\nbody",
        )
        .unwrap();
        let result = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert_eq!(result.entries.len(), 3);
        for (file, name, alias) in [
            ("drudge", "苦力", "drudge"),
            ("runner", "跑者", "runner"),
            ("artifacts-analyzer", "产物分析师", "artifacts-analyzer"),
        ] {
            let entry = result
                .entries
                .iter()
                .find(|e| e.runtime_name == name)
                .unwrap();
            assert_eq!(entry.aliases, [alias]);
            assert_eq!(
                entry.package_identity.as_deref(),
                Some("datarx-agents-team")
            );
            assert_eq!(entry.status, "candidate");
            assert!(entry.write_qualified);
            assert!(detail(&result, &entry.id)
                .unwrap()
                .contains(&format!("SECRET_PROMPT_{file}")));
        }
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("SECRET_PROMPT"));
        for source in [
            "git:host.example/org/../evil",
            "git:ssh://git@host.example/org/%2e%2e/evil",
            "ssh://git@host.example:2224/org/../evil",
            "https://host.example/org/%2e%2e/evil",
            "git:host.example/one",
            "git:../org/evil",
            "git:host.example/org\\..\\evil",
        ] {
            assert!(
                git_package_path(source.strip_prefix("git:").unwrap_or(source)).is_none(),
                "accepted {source}"
            );
        }
    }

    #[test]
    fn subagents_inventory_project_settings_not_resolved_from_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let agent = temp.path().join("agent");
        let project = temp.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        fs::create_dir_all(project.join("shared/agents")).unwrap();
        fs::write(
            project.join("shared/package.json"),
            r#"{"name":"wrong-base","pi-subagents":{"agents":["agents"]}}"#,
        )
        .unwrap();
        fs::write(
            project.join("shared/agents/w.md"),
            "---\nname: wrong-base\ndescription: D\n---\nbody",
        )
        .unwrap();
        fs::write(
            project.join(".pi/settings.json"),
            r#"{"packages":["file:./shared"]}"#,
        )
        .unwrap();
        assert!(inventory(&agent, Some(&project), ParityEvidence::default())
            .unwrap()
            .entries
            .is_empty());
    }
    #[test]
    fn subagents_inventory_projects_four_saved_fields_without_changing_qualification() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        fs::create_dir_all(agent.join("agents")).unwrap();
        fs::write(
            agent.join("agents/a.md"),
            "---\nname: a\ndescription: A\n---\nbody",
        )
        .unwrap();
        fs::write(
            agent.join("agents/b.md"),
            "---\nname: a\ndescription: B\n---\nbody",
        )
        .unwrap();
        let baseline = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert!(baseline.entries.iter().all(|entry| !entry.write_qualified));
        fs::write(
            agent.join("settings.json"),
            json!({"subagents":{"agentOverrides":{"a":{
                "model":"provider/model", "thinking":"high", "advertise":false,
                "disabled":false, "unknown":"preserve"
            }}}})
            .to_string(),
        )
        .unwrap();
        let with_override = inventory(&agent, None, ParityEvidence::default()).unwrap();
        for entry in &with_override.entries {
            assert!(!entry.write_qualified);
            assert!(matches!(
                collision(&with_override.entries, entry),
                Some(Collision::SameLevel)
            ));
            assert_eq!(
                entry.saved_override,
                json!({
                    "model":"provider/model", "thinking":"high", "advertise":false,
                    "disabled":false
                })
            );
            assert_eq!(
                serde_json::to_value(entry).unwrap()["savedOverride"],
                entry.saved_override
            );
            assert!(entry.saved_override.get("unknown").is_none());
        }
    }

    #[test]
    fn subagents_inventory_builtin_disabled_and_conflict() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let builtin = agent.join("npm/node_modules/pi-subagents/agents");
        fs::create_dir_all(&builtin).unwrap();
        fs::create_dir_all(agent.join("agents")).unwrap();
        fs::write(
            agent.join("npm/node_modules/pi-subagents/package.json"),
            r#"{"name":"pi-subagents"}"#,
        )
        .unwrap();
        fs::write(
            builtin.join("b.md"),
            "---\nname: common\ndescription: Builtin\n---\nbody",
        )
        .unwrap();
        fs::write(
            agent.join("agents/u.md"),
            "---\nname: common\ndescription: User\n---\nbody",
        )
        .unwrap();
        let result = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert_eq!(result.entries.len(), 2);
        assert!(result.entries.iter().any(|e| e.source == "builtin"));
        assert!(result.diagnostics.iter().any(|d| d.source == "collision"));
        fs::write(
            agent.join("settings.json"),
            r#"{"subagents":{"disableBuiltins":true}}"#,
        )
        .unwrap();
        let disabled = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert!(disabled
            .entries
            .iter()
            .any(|e| e.source == "builtin" && e.status == "disabled" && !e.write_qualified));
        assert!(disabled
            .diagnostics
            .iter()
            .any(|d| d.source == "builtin" && d.message.contains("disabled")));
    }
    #[test]
    fn subagents_inventory_builtin_roles_and_thinking_flags() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let project = root.path().join("project");
        let builtin = agent.join("npm/node_modules/pi-subagents/agents");
        fs::create_dir_all(&builtin).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        fs::write(
            builtin.join("b.md"),
            "---\nname: b\ndescription: D\n---\nBODY",
        )
        .unwrap();
        let user = agent.join("settings.json");
        let project_settings = project.join(".pi/settings.json");
        let scan = || inventory(&agent, Some(&project), ParityEvidence::default()).unwrap();
        let builtin_entry = |snapshot: Inventory| {
            snapshot
                .entries
                .into_iter()
                .find(|e| e.source == "builtin")
                .unwrap()
        };
        fs::write(&user, r#"{"subagents":{"disableBuiltins":true,"disableThinking":true,"agentOverrides":{"b":{"thinking":"high","disabled":false}}}}"#).unwrap();
        let restored = builtin_entry(scan());
        assert!(restored.write_qualified);
        assert_eq!(restored.saved_override, json!({})); // project write layer, not user
        fs::write(
            &project_settings,
            r#"{"subagents":{"agentOverrides":{"b":{"unknown":"only"}},"disableThinking":false}}"#,
        )
        .unwrap();
        assert!(builtin_entry(scan()).write_qualified); // unknown field cannot override user role
        fs::write(&project_settings, r#"{"subagents":{"disableBuiltins":true,"disableThinking":true,"agentOverrides":{"b":{"thinking":"medium"}}}}"#).unwrap();
        let restored = builtin_entry(scan());
        assert!(restored.write_qualified); // explicit project entry supersedes bulk and user
        assert_eq!(restored.saved_override["thinking"], "medium");
        fs::write(&project_settings, r#"{"subagents":{"disableBuiltins":true,"agentOverrides":{"b":{"disabled":true,"model":"provider/model"}}}}"#).unwrap();
        let disabled = builtin_entry(scan());
        assert_eq!(disabled.status, "disabled");
        assert!(!disabled.write_qualified); // model edit cannot silently restore disabled role
        fs::write(
            &project_settings,
            r#"{"subagents":{"disableBuiltins":false,"agentOverrides":{"b":{"disabled":true}}}}"#,
        )
        .unwrap();
        assert!(!builtin_entry(scan()).write_qualified);
        fs::write(
            &project_settings,
            r#"{"subagents":{"disableBuiltins":true,"agentOverrides":{"b":{"unknown":"only"}}}}"#,
        )
        .unwrap();
        assert!(!builtin_entry(scan()).write_qualified);
        fs::write(
            &project_settings,
            r#"{"subagents":{"disableBuiltins":false,"agentOverrides":{"b":{"thinking":false}}}}"#,
        )
        .unwrap();
        assert_eq!(builtin_entry(scan()).saved_override["thinking"], false);
    }

    #[test]
    fn subagents_inventory_bad_manifests_do_not_hide_valid_package() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let npm = agent.join("npm/node_modules");
        for name in ["valid", "broken", "large", "escaped"] {
            fs::create_dir_all(npm.join(name)).unwrap();
        }
        fs::create_dir_all(npm.join("valid/agents")).unwrap();
        fs::write(
            npm.join("valid/package.json"),
            r#"{"name":"valid","pi-subagents":{"agents":["agents"]}}"#,
        )
        .unwrap();
        fs::write(
            npm.join("valid/agents/valid.md"),
            "---\nname: valid\ndescription: D\n---\nbody",
        )
        .unwrap();
        fs::write(npm.join("broken/package.json"), "not json").unwrap();
        fs::write(
            npm.join("large/package.json"),
            "x".repeat(MAX_FILE as usize + 1),
        )
        .unwrap();
        fs::write(
            npm.join("escaped/package.json"),
            r#"{"name":"escaped-pkg","pi-subagents":{"agents":["../valid/agents"]}}"#,
        )
        .unwrap();
        let result = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert!(result.entries[0].write_qualified);
        for (name, message) in [
            ("broken", "invalid manifest"),
            ("large", "unreadable or oversized manifest"),
            ("escaped-pkg", "manifest path outside package root"),
        ] {
            assert!(
                result.diagnostics.iter().any(|d| d.source == "package"
                    && d.message.contains(name)
                    && d.message.contains(message)),
                "missing diagnostic: {name} {message}"
            );
        }
    }

    #[test]
    fn subagents_inventory_external_cli_builtin_runner_block() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let builtin = agent.join("npm/node_modules/pi-subagents/agents");
        fs::create_dir_all(&builtin).unwrap();
        // Matches pi-subagents 0.74 builtins: runner has type, adapter, command,
        // promptDelivery; top-level async/inheritance fields follow the runner block.
        for (name, adapter, command) in [
            ("claude-code", "claude-code", "claude"),
            ("claude-code-writer", "claude-code", "claude"),
            ("codex-exec", "codex-exec", "codex"),
            ("codex-exec-writer", "codex-exec", "codex"),
            ("cursor-agent", "cursor-agent", "cursor-agent"),
            ("cursor-agent-writer", "cursor-agent", "cursor-agent"),
        ] {
            fs::write(builtin.join(format!("{name}.md")), format!("---\nname: {name}\ndescription: external CLI\nrunner:\n  type: external-cli\n  adapter: {adapter}\n  command: {command}\n  promptDelivery: stdin\nasync: true\nsystemPromptMode: replace\ninheritProjectContext: true\ninheritSkills: false\n---\nbody")).unwrap();
        }
        let result = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert_eq!(result.entries.len(), 6);
        for entry in &result.entries {
            assert_eq!(entry.source, "builtin");
            assert_eq!(entry.parsed_fields["runner"], "external-cli");
            assert!(!entry.native_override_supported);
            assert!(!entry.write_qualified);
            assert_eq!(
                entry.write_diagnostic.as_ref().unwrap().message,
                "external or unknown runner"
            );
        }
        assert!(!result
            .diagnostics
            .iter()
            .any(|d| d.source == "builtin" && d.message == "unsupported frontmatter"));
    }

    #[test]
    fn subagents_inventory_agents_symlink_scan_still_denies_writes() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        fs::create_dir_all(agent.join("agents")).unwrap();
        fs::write(
            agent.join("agents/valid.md"),
            "---\nname: valid\ndescription: D\n---\nbody",
        )
        .unwrap();
        std::os::unix::fs::symlink("loop", agent.join("agents/loop")).unwrap();
        let result = inventory(&agent, None, ParityEvidence::default()).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.source == "scan" && d.message == "symlink source omitted"));
        assert!(!result.entries[0].write_qualified);
        assert_eq!(
            result.entries[0].write_diagnostic.as_ref().unwrap().message,
            "in-scope discovery incomplete"
        );
    }
    #[test]
    fn subagents_inventory_dual_scope_package_and_write_gate() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let workspace = root.path().join("workspace");
        let shared = root.path().join("shared");
        fs::create_dir_all(shared.join("agents")).unwrap();
        fs::write(
            shared.join("package.json"),
            r#"{"name":"shared","pi-subagents":{"agents":["agents"]}}"#,
        )
        .unwrap();
        fs::write(
            shared.join("agents/s.md"),
            "---\nname: s\ndescription: S\n---\nbody",
        )
        .unwrap();
        fs::create_dir_all(agent.join("agents")).unwrap();
        fs::write(
            agent.join("agents/user.md"),
            "---\nname: user1\ndescription: U\n---\nbody",
        )
        .unwrap();
        fs::create_dir_all(workspace.join(".pi")).unwrap();
        fs::create_dir_all(workspace.join(".agents")).unwrap();
        fs::write(workspace.join(".agents/extra.md"), "OUT_OF_SCOPE_SENTINEL").unwrap();
        fs::write(
            agent.join("settings.json"),
            r#"{"packages":["file:../shared"]}"#,
        )
        .unwrap();
        fs::write(
            workspace.join(".pi/settings.json"),
            r#"{"packages":["file:../../shared"]}"#,
        )
        .unwrap();
        let result = inventory(&agent, Some(&workspace), ParityEvidence::default()).unwrap();
        let shared_entry = result
            .entries
            .iter()
            .find(|e| e.runtime_name == "s")
            .expect("shared package entry");
        assert_eq!(
            result
                .entries
                .iter()
                .filter(|e| e.runtime_name == "s")
                .count(),
            1
        );
        assert_eq!(shared_entry.source_scope, "project");
        assert!(
            shared_entry
                .additional_scopes
                .contains(&"global".to_string()),
            "dual-source annotation missing"
        );
        assert!(result
            .entries
            .iter()
            .all(|e| e.write_qualified && e.write_diagnostic.is_none()));
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("OUT_OF_SCOPE_SENTINEL"));
        assert!(result.diagnostics.iter().any(|d| d.source == ".agents/"));
    }
    #[test]
    fn subagents_inventory_block_scalar_description_folds_continuation_lines() {
        let (_, _, description, _, _) =
            frontmatter("---\nname: x\ndescription: |-\n  first line\n  second line\n---\nbody")
                .unwrap();
        assert_eq!(description, "first line second line");
        let (_, _, description, _, _) =
            frontmatter("---\nname: x\ndescription: |\n  first line\n\n  second line\n---\nbody")
                .unwrap();
        assert_eq!(description, "first line second line");
        for marker in ["|", "|-", "|+", ">", ">-", ">+"] {
            let raw = format!("---\nname: x\ndescription: {marker}\n  one\n  two\n---\nbody");
            let (_, _, description, _, _) = frontmatter(&raw).unwrap();
            assert_eq!(description, "one two", "marker {marker}");
        }
    }
    #[test]
    fn subagents_inventory_block_scalar_keeps_neighbouring_fields() {
        let (runtime, name, description, aliases, runner) = frontmatter(
            "---\nname: x\ndescription: >-\n  first\n  second\naliases: a, b\nrunner:\n  type: cli\n---\nbody",
        )
        .unwrap();
        assert_eq!((runtime.as_str(), name.as_str()), ("x", "x"));
        assert_eq!(description, "first second");
        assert_eq!(aliases, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(runner, "cli");
        // Shape emitted by pi-subagents >=0.76.0 serializeAgent: the block scalar is
        // followed by advertise/tools/model keys and an indented runner block.
        let (runtime, name, description, aliases, runner) = frontmatter(
            "---\nname: reviewer\npackage: core\ndescription: |-\n  Review diffs for\n  correctness and scope.\nadvertise: true\naliases: rvw, review\ntools: read, grep\nmodel: sonnet\nrunner:\n  type: cli\n  command: claude\n---\nbody",
        )
        .unwrap();
        assert_eq!(
            (runtime.as_str(), name.as_str()),
            ("core.reviewer", "reviewer")
        );
        assert_eq!(description, "Review diffs for correctness and scope.");
        assert_eq!(aliases, vec!["rvw".to_string(), "review".to_string()]);
        assert_eq!(runner, "cli");
    }
    #[test]
    fn subagents_inventory_block_scalar_hash_lines_are_content() {
        assert_eq!(
            frontmatter("---\nname: x\ndescription: |-\n---\nbody"),
            Err("missing name or description")
        );
        assert_eq!(
            frontmatter("---\nname: x\ndescription: |\n\n  # only a heading\n---\nbody")
                .unwrap()
                .2,
            "# only a heading"
        );
    }
    #[test]
    fn subagents_inventory_non_block_scalar_values_keep_strictness() {
        assert_eq!(
            frontmatter("---\nname: x\ndescription: d\n  stray\n---\nbody"),
            Err("unsupported frontmatter")
        );
        let (_, _, description, _, _) =
            frontmatter("---\nname: x\ndescription: \"|\"\n---\nbody").unwrap();
        assert_eq!(description, "|");
    }
    #[test]
    fn subagents_inventory_block_scalar_description_lists_entry() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        fs::create_dir_all(agent.join("agents")).unwrap();
        fs::write(
            agent.join("agents/multi.md"),
            "---\nname: multi\ndescription: |-\n  first line\n  second line\n---\nbody",
        )
        .unwrap();
        let snapshot = inventory(&agent, None, ParityEvidence::default()).unwrap();
        let entry = snapshot
            .entries
            .iter()
            .find(|e| e.runtime_name == "multi")
            .expect("block scalar definition is listed");
        assert_eq!(entry.parsed_fields["description"], "first line second line");
        assert!(snapshot
            .diagnostics
            .iter()
            .all(|d| d.message != "unsupported frontmatter"));
    }
}
