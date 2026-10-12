// ABOUTME: Inspects configured Pi packages and preserves their global/project scope.
// ABOUTME: Owns typed package records and atomic disabled-state mutations for the native host.

#![allow(dead_code)]

use futures_util::stream::{self, StreamExt};
use semver::{Version, VersionReq};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tempfile::NamedTempFile;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

const MAX_RESOURCE_FILES: usize = 10_000;
const UPDATE_CHECK_CONCURRENCY: usize = 4;
const UPDATE_CHECK_TIMEOUT_SECS: u64 = 10;

const RESOURCE_KINDS: [(&str, &str); 4] = [
    ("extensions", "extension"),
    ("skills", "skill"),
    ("prompts", "prompt"),
    ("themes", "theme"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PiPackageResourceKind {
    Extension,
    Skill,
    Prompt,
    Theme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PiPackageScope {
    Global,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PiPackageResource {
    pub kind: PiPackageResourceKind,
    pub name: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PiPackageCounts {
    pub extensions: usize,
    pub skills: usize,
    pub prompts: usize,
    pub themes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PiPackageRecord {
    pub source: String,
    pub scope: PiPackageScope,
    pub installed_path: Option<String>,
    pub package_name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub disabled: bool,
    pub counts: PiPackageCounts,
    pub resources: Vec<PiPackageResource>,
}

#[derive(Debug, Clone)]
pub(crate) struct PackageLocations {
    pub global_settings: PathBuf,
    pub project_settings: Option<PathBuf>,
    pub global_agent_dir: PathBuf,
    pub project_root: Option<PathBuf>,
}

pub(crate) fn locations_for_workspace(
    workspace: Option<&Path>,
) -> Result<PackageLocations, String> {
    let global_agent_dir = match std::env::var("PI_CODING_AGENT_DIR") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => dirs::home_dir()
            .ok_or_else(|| "cannot resolve Pi home directory".to_string())?
            .join(".pi")
            .join("agent"),
    };
    let project_root = workspace.map(Path::to_path_buf);
    Ok(PackageLocations {
        global_settings: global_agent_dir.join("settings.json"),
        project_settings: project_root
            .as_ref()
            .map(|root| root.join(".pi/settings.json")),
        global_agent_dir,
        project_root,
    })
}

#[derive(Debug, Clone)]
struct ConfiguredPackage {
    source: String,
    disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedSource {
    Npm {
        name: String,
        spec: String,
        version: Option<String>,
    },
    Git {
        host: String,
        path: String,
        reference: Option<String>,
    },
    Local {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PiPackageUpdate {
    pub source: String,
    pub scope: PiPackageScope,
    pub available: bool,
}

/// Check each installed package against its configured upstream.
pub(crate) async fn check_available_updates(
    records: &[PiPackageRecord],
    locations: &PackageLocations,
) -> Vec<PiPackageUpdate> {
    let records = records.to_vec();
    let locations = locations.clone();
    // No aggregate timeout: one slow source (e.g. intranet git remotes) used to
    // discard every finished result via an all-or-nothing batch timeout. Each
    // check is already bounded by UPDATE_CHECK_TIMEOUT_SECS with kill_on_drop.
    stream::iter(records.into_iter().map(move |record| {
        let locations = locations.clone();
        async move {
            let available = check_package_update(&record, &locations).await;
            PiPackageUpdate {
                source: record.source,
                scope: record.scope,
                available,
            }
        }
    }))
    .buffer_unordered(UPDATE_CHECK_CONCURRENCY)
    .filter(|update| std::future::ready(update.available))
    .collect()
    .await
}

async fn check_package_update(record: &PiPackageRecord, locations: &PackageLocations) -> bool {
    let Some(installed_path) = record.installed_path.as_deref() else {
        return false;
    };
    let Ok(source) = parse_source(&record.source) else {
        return false;
    };
    match source {
        ParsedSource::Npm {
            name,
            spec,
            version,
        } => {
            if version
                .as_deref()
                .and_then(|value| Version::parse(value).ok())
                .is_some()
            {
                return false;
            }
            let Some(installed_version) = record.version.as_deref() else {
                return false;
            };
            let package_spec = if version.is_some() {
                spec
            } else {
                name.clone()
            };
            let range = version
                .as_deref()
                .and_then(|value| VersionReq::parse(value).ok());
            let command =
                configured_npm_command(locations).unwrap_or_else(|| vec!["npm".to_string()]);
            let mut args = command.iter().skip(1).cloned().collect::<Vec<_>>();
            let executable = command
                .first()
                .cloned()
                .unwrap_or_else(|| "npm".to_string());
            args.extend([
                "view".to_string(),
                package_spec,
                "version".to_string(),
                "--json".to_string(),
            ]);
            // `npm view` needs only a valid working directory; landing (no
            // workspace binding) has no project root, so fall back to the
            // global agent dir instead of skipping the check entirely.
            let cwd = locations
                .project_root
                .as_deref()
                .unwrap_or(locations.global_agent_dir.as_path());
            let Ok(output) = run_update_command(&executable, &args, cwd).await else {
                return false;
            };
            let Some(target_version) = latest_npm_version(&output, range.as_ref()) else {
                return false;
            };
            target_version != installed_version
        }
        ParsedSource::Git {
            reference: Some(_), ..
        }
        | ParsedSource::Local { .. } => false,
        ParsedSource::Git {
            reference: None, ..
        } => {
            let Ok(local_head) = run_update_command(
                "git",
                &["rev-parse".to_string(), "HEAD".to_string()],
                Path::new(installed_path),
            )
            .await
            else {
                return false;
            };
            let Ok(remote_head) = remote_git_head(installed_path).await else {
                return false;
            };
            local_head.trim() != remote_head
        }
    }
}

fn configured_npm_command(locations: &PackageLocations) -> Option<Vec<String>> {
    let project_command = locations
        .project_settings
        .as_deref()
        .and_then(|path| read_settings_object(path).ok())
        .and_then(|settings| npm_command_from_settings(&settings));
    project_command.or_else(|| {
        read_settings_object(&locations.global_settings)
            .ok()
            .and_then(|settings| npm_command_from_settings(&settings))
    })
}

fn npm_command_from_settings(settings: &Map<String, Value>) -> Option<Vec<String>> {
    let command = settings.get("npmCommand")?.as_array()?;
    let command = command
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    (!command.is_empty()).then(|| command.into_iter().map(ToOwned::to_owned).collect())
}

async fn run_update_command(command: &str, args: &[String], cwd: &Path) -> Result<String, String> {
    if std::env::var("PI_OFFLINE")
        .ok()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
    {
        return Err("offline mode".to_string());
    }
    let mut child_command = Command::new(command);
    crate::windows_child::hide_console_tokio(&mut child_command);
    let output = timeout(
        Duration::from_secs(UPDATE_CHECK_TIMEOUT_SECS),
        child_command
            .args(args)
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A per-subprocess timeout cancels the in-flight output() future, and a
            // cancelled child that stays attached would keep running detached; always
            // reap it on drop so stalled npm/git checks cannot leak orphan processes.
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "package update check timed out".to_string())?
    .map_err(|error| format!("package update check failed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn latest_npm_version(output: &str, range: Option<&VersionReq>) -> Option<String> {
    let value: Value = serde_json::from_str(output.trim()).ok()?;
    let mut versions = match value {
        Value::String(version) => vec![version],
        Value::Array(values) => values
            .into_iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect(),
        _ => return None,
    };
    versions.retain(|version| Version::parse(version).is_ok());
    if let Some(range) = range {
        versions
            .into_iter()
            .filter_map(|version| {
                let parsed = Version::parse(&version).ok()?;
                range.matches(&parsed).then_some((parsed, version))
            })
            .max_by(|left, right| left.0.cmp(&right.0))
            .map(|(_, version)| version)
    } else {
        versions
            .into_iter()
            .filter_map(|version| Some((Version::parse(&version).ok()?, version)))
            .max_by(|left, right| left.0.cmp(&right.0))
            .map(|(_, version)| version)
    }
}

async fn remote_git_head(installed_path: &str) -> Result<String, String> {
    let upstream = run_update_command(
        "git",
        &[
            "rev-parse".to_string(),
            "--abbrev-ref".to_string(),
            "@{upstream}".to_string(),
        ],
        Path::new(installed_path),
    )
    .await
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| value.starts_with("origin/") && value.len() > "origin/".len());
    let output = if let Some(upstream) = upstream {
        let branch = &upstream["origin/".len()..];
        run_update_command(
            "git",
            &[
                "ls-remote".to_string(),
                "origin".to_string(),
                format!("refs/heads/{branch}"),
            ],
            Path::new(installed_path),
        )
        .await?
    } else {
        run_update_command(
            "git",
            &[
                "ls-remote".to_string(),
                "origin".to_string(),
                "HEAD".to_string(),
            ],
            Path::new(installed_path),
        )
        .await?
    };
    output
        .lines()
        .find_map(parse_ls_remote_reference)
        .ok_or_else(|| "failed to determine remote git HEAD".to_string())
}

/// Parse a `git ls-remote <ref>` line into its resolved 40-hex-dir commit SHA.
/// Returns None for advert entries that are not the named ref we care about.
fn parse_ls_remote_reference(line: &str) -> Option<String> {
    let mut fields = line.split_whitespace();
    let head = fields.next()?;
    let reference = fields.next()?;
    (head.len() == 40
        && head.bytes().all(|byte| byte.is_ascii_hexdigit())
        && (reference == "HEAD" || reference.starts_with("refs/heads/")))
    .then(|| head.to_string())
}

pub(crate) fn inspect_configured_packages(
    locations: &PackageLocations,
) -> Result<Vec<PiPackageRecord>, String> {
    let mut records = Vec::new();
    for package in read_configured_packages(&locations.global_settings)? {
        records.push(build_record(locations, package, "global"));
    }
    if let Some(project_settings) = &locations.project_settings {
        for package in read_configured_packages(project_settings)? {
            records.push(build_record(locations, package, "project"));
        }
    }
    Ok(records)
}

/// Parse the embedded Pi CLI's `pi list` output. Pi is authoritative for the
/// configured sources, scope, and resolved install path; this module only
/// enriches those host-resolved records with metadata and resources.
pub(crate) fn inspect_pi_list_output(
    output: &str,
    locations: &PackageLocations,
) -> Result<Vec<PiPackageRecord>, String> {
    let global_packages = read_configured_packages(&locations.global_settings)?;
    let project_packages = locations
        .project_settings
        .as_ref()
        .map(|path| read_configured_packages(path))
        .transpose()?
        .unwrap_or_default();
    let mut records: Vec<PiPackageRecord> = Vec::new();
    let mut scope = "global";

    for line in output.lines() {
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("No packages installed.") {
            continue;
        }
        if trimmed.ends_with(':') {
            let lower = trimmed.to_ascii_lowercase();
            if lower.starts_with("project") {
                scope = "project";
            } else if lower.starts_with("user") || lower.starts_with("global") {
                scope = "global";
            }
            continue;
        }
        if leading >= 4 {
            let path = trimmed.strip_prefix('-').map(str::trim).unwrap_or(trimmed);
            if !path.is_empty() {
                if let Some(record) = records.last_mut() {
                    if record.installed_path.is_none() {
                        record.installed_path = Some(path.to_string());
                        enrich_record(record);
                    }
                }
            }
            continue;
        }

        let source = trimmed.strip_prefix('-').map(str::trim).unwrap_or(trimmed);
        let source = source
            .strip_suffix(" (filtered)")
            .map(str::trim)
            .unwrap_or(source);
        if source.is_empty() {
            continue;
        }
        let disabled = configured_package_disabled(
            if scope == "project" {
                &project_packages
            } else {
                &global_packages
            },
            source,
        );
        records.push(record_from_pi_list(source, scope, disabled));
    }

    Ok(records)
}

fn record_from_pi_list(source: &str, scope: &str, disabled: bool) -> PiPackageRecord {
    PiPackageRecord {
        source: source.to_string(),
        scope: match scope {
            "global" => PiPackageScope::Global,
            "project" => PiPackageScope::Project,
            _ => unreachable!("Pi list parser only emits known scopes"),
        },
        installed_path: None,
        package_name: None,
        version: None,
        description: None,
        disabled,
        counts: PiPackageCounts {
            extensions: 0,
            skills: 0,
            prompts: 0,
            themes: 0,
        },
        resources: Vec::new(),
    }
}

fn configured_package_disabled(packages: &[ConfiguredPackage], source: &str) -> bool {
    packages
        .iter()
        .find(|package| package.source == source)
        .is_some_and(|package| package.disabled)
}

fn enrich_record(record: &mut PiPackageRecord) {
    let Some(path) = record.installed_path.as_deref() else {
        return;
    };
    let metadata = read_package_metadata(path);
    record.package_name = metadata.as_ref().and_then(|value| value.name.clone());
    record.version = metadata.as_ref().and_then(|value| value.version.clone());
    record.description = metadata.and_then(|value| value.description);
    record.resources = match read_manifest_resources(Path::new(path)) {
        Ok(Some(resources)) => resources,
        Ok(None) => read_conventional_resources(Path::new(path)).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    record.counts = counts_for(&record.resources);
}

pub(crate) fn set_package_disabled(
    locations: &PackageLocations,
    scope: &str,
    source: &str,
    disabled: bool,
) -> Result<bool, String> {
    let settings_path = match scope {
        "global" => &locations.global_settings,
        "project" => locations
            .project_settings
            .as_ref()
            .ok_or_else(|| "project settings are unavailable".to_string())?,
        _ => return Err("scope must be global or project".to_string()),
    };
    let source = source.trim();
    if source.is_empty() {
        return Err("package source cannot be empty".to_string());
    }
    let mut settings = read_settings_object(settings_path)?;
    let packages = settings
        .entry("packages".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let entries = packages
        .as_array_mut()
        .ok_or_else(|| "Pi settings packages must be an array".to_string())?;
    let index = entries
        .iter()
        .position(|entry| package_source(entry).as_deref() == Some(source))
        .ok_or_else(|| format!("configured package not found: {source}"))?;
    let current_disabled = package_disabled(&entries[index]);
    if current_disabled == disabled {
        return Ok(false);
    }
    entries[index] = if disabled {
        disabled_package_entry(source)
    } else {
        Value::String(source.to_string())
    };
    atomic_write_settings(settings_path, &settings)?;
    Ok(true)
}

fn disabled_package_entry(source: &str) -> Value {
    serde_json::json!({
        "source": source,
        "extensions": [],
        "skills": [],
        "prompts": [],
        "themes": []
    })
}

fn read_configured_packages(path: &Path) -> Result<Vec<ConfiguredPackage>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let settings = read_settings_object(path)?;
    let Some(entries) = settings.get("packages").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(entries
        .iter()
        .filter_map(|entry| {
            Some(ConfiguredPackage {
                source: package_source(entry)?,
                disabled: package_disabled(entry),
            })
        })
        .collect())
}

fn read_settings_object(path: &Path) -> Result<Map<String, Value>, String> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("Pi settings must be a JSON object: {}", path.display()))
}

fn package_source(entry: &Value) -> Option<String> {
    match entry {
        Value::String(source) if !source.trim().is_empty() => Some(source.trim().to_string()),
        Value::Object(object) => object
            .get("source")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn package_disabled(entry: &Value) -> bool {
    let Some(object) = entry.as_object() else {
        return false;
    };
    ["extensions", "skills", "prompts", "themes"]
        .iter()
        .all(|key| {
            object
                .get(*key)
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        })
}

fn build_record(
    locations: &PackageLocations,
    package: ConfiguredPackage,
    scope: &str,
) -> PiPackageRecord {
    let root = installed_root(locations, &package.source, scope)
        .ok()
        .flatten();
    let metadata = root.as_deref().and_then(read_package_metadata);
    let resources = root
        .as_deref()
        .and_then(|path| match read_manifest_resources(Path::new(path)) {
            Ok(Some(resources)) => Some(resources),
            Ok(None) => read_conventional_resources(Path::new(path)).ok(),
            Err(_) => None,
        })
        .unwrap_or_default();
    let counts = counts_for(&resources);
    PiPackageRecord {
        source: package.source,
        scope: match scope {
            "global" => PiPackageScope::Global,
            "project" => PiPackageScope::Project,
            _ => unreachable!("package scope is validated by the caller"),
        },
        installed_path: root,
        package_name: metadata.as_ref().and_then(|m| m.name.clone()),
        version: metadata.as_ref().and_then(|m| m.version.clone()),
        description: metadata.and_then(|m| m.description),
        disabled: package.disabled,
        counts,
        resources,
    }
}

#[derive(Debug)]
struct PackageMetadata {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
}

fn read_package_metadata(root: &str) -> Option<PackageMetadata> {
    let value: Value =
        serde_json::from_slice(&fs::read(Path::new(root).join("package.json")).ok()?).ok()?;
    let object = value.as_object()?;
    Some(PackageMetadata {
        name: object
            .get("name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        version: object
            .get("version")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        description: object
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    })
}

fn installed_root(
    locations: &PackageLocations,
    source: &str,
    scope: &str,
) -> Result<Option<String>, String> {
    let parsed = parse_source(source)?;
    let base = if scope == "global" {
        locations.global_agent_dir.clone()
    } else {
        locations
            .project_root
            .as_ref()
            .ok_or_else(|| "project root is unavailable".to_string())?
            .join(".pi")
    };
    let root = match parsed {
        ParsedSource::Npm { name, .. } => base.join("npm").join("node_modules").join(name),
        ParsedSource::Git { host, path, .. } => {
            resolve_managed_path(&base.join("git"), &[host, path])?
        }
        ParsedSource::Local { path } => {
            let expanded = expand_local_path(&path);
            let local = PathBuf::from(&expanded);
            if local.is_absolute() {
                local
            } else {
                base.join(local)
            }
        }
    };
    Ok(root.is_dir().then(|| root.to_string_lossy().into_owned()))
}

fn parse_source(source: &str) -> Result<ParsedSource, String> {
    let source = source.trim();
    if let Some(spec) = source.strip_prefix("npm:") {
        let (name, version) = npm_package_spec(spec)?;
        return Ok(ParsedSource::Npm {
            name,
            spec: spec.to_string(),
            version,
        });
    }
    let (git_candidate, explicit_git) = if let Some(value) = source.strip_prefix("git:") {
        (value.trim(), true)
    } else if let Some(value) = source.strip_prefix("github:") {
        (value.trim(), true)
    } else {
        (source, false)
    };
    if explicit_git
        || git_candidate.starts_with("http://")
        || git_candidate.starts_with("https://")
        || git_candidate.starts_with("ssh://")
        || git_candidate.starts_with("git://")
    {
        return parse_git_source(git_candidate);
    }
    Ok(ParsedSource::Local {
        path: source.to_string(),
    })
}

fn npm_package_spec(spec: &str) -> Result<(String, Option<String>), String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("npm package name cannot be empty".to_string());
    }
    let name = if let Some(rest) = spec.strip_prefix('@') {
        let slash = rest
            .find('/')
            .ok_or_else(|| format!("invalid scoped npm package: {spec}"))?;
        format!("@{}", &rest[..slash + 1]) + rest[slash + 1..].split('@').next().unwrap_or_default()
    } else {
        spec.split('@').next().unwrap_or_default().to_string()
    };
    if name == "@" || name.ends_with('/') || name.contains('/') && name.split('/').count() != 2 {
        return Err(format!("invalid npm package name: {spec}"));
    }
    let version = if name.starts_with('@') {
        spec.strip_prefix(&name)
            .and_then(|value| value.strip_prefix('@'))
    } else {
        spec.strip_prefix(&name)
            .and_then(|value| value.strip_prefix('@'))
    }
    .map(ToOwned::to_owned);
    Ok((name, version))
}

fn parse_git_source(value: &str) -> Result<ParsedSource, String> {
    let (without_ref, reference) = split_git_ref(value);
    let (host, path) = if let Some(rest) = without_ref.strip_prefix("git@") {
        rest.split_once(':')
            .ok_or_else(|| format!("invalid git source: {value}"))?
    } else if let Some(rest) = without_ref.strip_prefix("https://") {
        split_host_path(rest)?
    } else if let Some(rest) = without_ref.strip_prefix("http://") {
        split_host_path(rest)?
    } else if let Some(rest) = without_ref.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map_or(rest, |(_, rest)| rest);
        split_host_path(rest)?
    } else if let Some(rest) = without_ref.strip_prefix("git://") {
        split_host_path(rest)?
    } else {
        let rest = without_ref.strip_prefix("github:").unwrap_or(without_ref);
        if rest.split('/').count() < 2 {
            return Err(format!("invalid git source: {value}"));
        }
        ("github.com", rest)
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    validate_git_part(host, false)?;
    validate_git_part(path, true)?;
    if path.split('/').count() < 2 {
        return Err(format!("invalid git repository path: {value}"));
    }
    Ok(ParsedSource::Git {
        host: host.to_string(),
        path: path.to_string(),
        reference: reference.map(ToOwned::to_owned),
    })
}

fn split_git_ref(value: &str) -> (&str, Option<&str>) {
    if let Some((repo, reference)) = value.split_once('#') {
        return (repo, Some(reference));
    }
    if let Some((prefix, _)) = value.split_once("://") {
        let scheme_len = prefix.len() + 3;
        let path_start = value[scheme_len..]
            .find('/')
            .map_or(value.len(), |index| scheme_len + index + 1);
        if let Some(at) = value[path_start..].find('@') {
            let at = path_start + at;
            return (&value[..at], Some(&value[at + 1..]));
        }
    } else if let Some(slash) = value.find('/') {
        if let Some(at) = value[slash + 1..].find('@') {
            let at = slash + 1 + at;
            return (&value[..at], Some(&value[at + 1..]));
        }
    }
    (value, None)
}

fn split_host_path(value: &str) -> Result<(&str, &str), String> {
    value
        .split_once('/')
        .ok_or_else(|| "git source is missing repository path".to_string())
}

fn validate_git_part(value: &str, allow_slash: bool) -> Result<(), String> {
    // Mirrors package-skill-inventory.ts hasUnsafeGitInstallPart, including encoded traversal.
    let decoded = percent_encoding::percent_decode_str(value).decode_utf8_lossy();
    for candidate in [value, decoded.as_ref()] {
        if candidate.is_empty()
            || candidate.contains('\0')
            || candidate.contains('\\')
            || candidate.starts_with('/')
            || (!allow_slash && candidate.contains('/'))
            || candidate
                .split('/')
                .any(|part| part == ".." || part.is_empty())
        {
            return Err(format!("unsafe git install path component: {value}"));
        }
    }
    Ok(())
}

fn resolve_managed_path(root: &Path, parts: &[String]) -> Result<PathBuf, String> {
    let root = absolute_path(root)?;
    let candidate = parts
        .iter()
        .fold(root.clone(), |path, part| path.join(part));
    let candidate = absolute_path(&candidate)?;
    if candidate != root && !candidate.starts_with(&root) {
        return Err(format!(
            "refusing path outside install root: {}",
            candidate.display()
        ));
    }
    Ok(candidate)
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| format!("cannot resolve path: {error}"))
    }
}

fn expand_local_path(path: &str) -> String {
    if path == "~" {
        return dirs::home_dir()
            .map(|home| home.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

fn read_manifest_resources(root: &Path) -> Result<Option<Vec<PiPackageResource>>, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve package root: {error}"))?;
    let bytes = match fs::read(canonical_root.join("package.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read package manifest: {error}")),
    };
    let package: Value =
        serde_json::from_slice(&bytes).map_err(|_| "invalid package manifest".to_string())?;
    let Some(manifest) = package.get("pi").and_then(Value::as_object) else {
        return Ok(None);
    };
    let mut resources = Vec::new();
    for (directory, kind) in RESOURCE_KINDS {
        let Some(entries) = manifest.get(directory).and_then(Value::as_array) else {
            continue;
        };
        let start = resources.len();
        let mut exclusions = Vec::new();
        let mut force_includes = Vec::new();
        let mut force_excludes = Vec::new();
        for entry in entries {
            let Some(raw) = entry.as_str() else {
                continue;
            };
            let (mode, pattern) = match raw.as_bytes().first() {
                Some(b'!') => ("exclude", &raw[1..]),
                Some(b'+') => ("force-include", &raw[1..]),
                Some(b'-') => ("force-exclude", &raw[1..]),
                _ => ("include", raw),
            };
            match mode {
                "exclude" => exclusions.push(pattern),
                "force-include" => force_includes.push(pattern),
                "force-exclude" => force_excludes.push(pattern),
                _ => collect_manifest_pattern(&canonical_root, pattern, kind, &mut resources)?,
            }
        }
        let all = resources.split_off(start);
        let mut selected = all.clone();
        selected.retain(|resource| {
            !exclusions
                .iter()
                .any(|pattern| glob_matches(pattern, &resource.relative_path))
        });
        for resource in all {
            if force_includes
                .iter()
                .any(|pattern| glob_matches(pattern, &resource.relative_path))
                && !selected
                    .iter()
                    .any(|selected| selected.relative_path == resource.relative_path)
            {
                selected.push(resource);
            }
        }
        selected.retain(|resource| {
            !force_excludes
                .iter()
                .any(|pattern| glob_matches(pattern, &resource.relative_path))
        });
        let mut seen = BTreeSet::new();
        selected.retain(|resource| seen.insert(resource.relative_path.clone()));
        resources.extend(selected);
    }
    resources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(Some(resources))
}

fn read_conventional_resources(root: &Path) -> Result<Vec<PiPackageResource>, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve package root: {error}"))?;
    let mut resources = Vec::new();
    for (directory, kind) in RESOURCE_KINDS {
        // Conventional discovery follows Pi package rules; a missing or
        // escaping directory simply contributes nothing.
        let Ok(conventional_dir) = canonical_root.join(directory).canonicalize() else {
            continue;
        };
        if !conventional_dir.starts_with(&canonical_root) {
            continue;
        }
        if kind == "skill" {
            let mut visited = BTreeSet::new();
            collect_conventional_skills(
                &canonical_root,
                &conventional_dir,
                true,
                &mut visited,
                &mut resources,
            )?;
        } else {
            for entry in fs::read_dir(&conventional_dir)
                .map_err(|error| format!("cannot read resource directory: {error}"))?
            {
                let entry =
                    entry.map_err(|error| format!("cannot inspect resource entry: {error}"))?;
                add_resource(&canonical_root, &entry.path(), kind, None, &mut resources)?;
            }
        }
    }
    resources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(resources)
}

// Pi loads skills from SKILL.md folders anywhere under skills/ plus top-level
// loose .md files; nested loose .md files are not skills.
fn collect_conventional_skills(
    root: &Path,
    directory: &Path,
    top_level: bool,
    visited: &mut BTreeSet<PathBuf>,
    resources: &mut Vec<PiPackageResource>,
) -> Result<(), String> {
    if resources.len() >= MAX_RESOURCE_FILES {
        return Ok(());
    }
    let canonical = directory
        .canonicalize()
        .map_err(|error| format!("cannot resolve resource directory: {error}"))?;
    if !canonical.starts_with(root) || !visited.insert(canonical.clone()) {
        return Ok(());
    }
    for entry in fs::read_dir(&canonical)
        .map_err(|error| format!("cannot read resource directory: {error}"))?
    {
        let entry = entry.map_err(|error| format!("cannot inspect resource entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            if path.join("SKILL.md").is_file() {
                add_resource(
                    root,
                    &path.join("SKILL.md"),
                    "skill",
                    path.file_name(),
                    resources,
                )?;
            } else {
                collect_conventional_skills(root, &path, false, visited, resources)?;
            }
        } else if top_level {
            add_resource(root, &path, "skill", None, resources)?;
        }
    }
    Ok(())
}

fn has_glob(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

fn collect_manifest_pattern(
    root: &Path,
    pattern: &str,
    kind: &str,
    resources: &mut Vec<PiPackageResource>,
) -> Result<(), String> {
    if has_glob(pattern) {
        let mut matches = Vec::new();
        collect_files(root, root, &mut matches)?;
        for path in matches {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("cannot relativize resource path: {error}"))?
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if glob_matches(pattern, &relative) {
                add_resource(root, &path, kind, None, resources)?;
            }
        }
        return Ok(());
    }
    let Ok(path) = root.join(pattern).canonicalize() else {
        return Ok(());
    };
    if path.starts_with(root) {
        collect_resource_path(root, &path, kind, resources)?;
    }
    Ok(())
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern
        .trim_start_matches("./")
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let value_parts: Vec<&str> = value.split('/').filter(|part| !part.is_empty()).collect();
    let mut states = vec![vec![false; value_parts.len() + 1]; pattern_parts.len() + 1];
    states[0][0] = true;
    for (pattern_index, pattern_part) in pattern_parts.iter().enumerate() {
        for value_index in 0..=value_parts.len() {
            if !states[pattern_index][value_index] {
                continue;
            }
            if *pattern_part == "**" {
                states[pattern_index + 1][value_index] = true;
                if value_index < value_parts.len() {
                    states[pattern_index][value_index + 1] = true;
                }
            } else if value_index < value_parts.len()
                && segment_matches(pattern_part, value_parts[value_index])
            {
                states[pattern_index + 1][value_index + 1] = true;
            }
        }
    }
    states[pattern_parts.len()][value_parts.len()]
}

fn segment_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut states = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    states[0][0] = true;
    for index in 0..pattern.len() {
        for value_index in 0..=value.len() {
            if !states[index][value_index] {
                continue;
            }
            match pattern[index] {
                b'*' => {
                    states[index + 1][value_index] = true;
                    if value_index < value.len() {
                        states[index][value_index + 1] = true;
                    }
                }
                b'?' if value_index < value.len() => {
                    states[index + 1][value_index + 1] = true;
                }
                byte if value_index < value.len() && byte == value[value_index] => {
                    states[index + 1][value_index + 1] = true;
                }
                _ => {}
            }
        }
    }
    states[pattern.len()][value.len()]
}

fn collect_files(root: &Path, path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut visited = BTreeSet::new();
    collect_files_inner(root, path, files, &mut visited)
}

fn collect_files_inner(
    root: &Path,
    path: &Path,
    files: &mut Vec<PathBuf>,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    if files.len() >= MAX_RESOURCE_FILES {
        return Ok(());
    }
    if path.is_file() {
        files.push(path.to_path_buf());
        return Ok(());
    }
    if !path.is_dir() {
        return Ok(());
    }
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("cannot resolve resource directory: {error}"))?;
    if !canonical_path.starts_with(root) || !visited.insert(canonical_path.clone()) {
        return Ok(());
    }
    for entry in fs::read_dir(&canonical_path)
        .map_err(|error| format!("cannot read resource directory: {error}"))?
    {
        let entry = entry.map_err(|error| format!("cannot inspect resource entry: {error}"))?;
        if entry
            .path()
            .symlink_metadata()
            .map_err(|error| format!("cannot inspect resource link: {error}"))?
            .file_type()
            .is_symlink()
        {
            continue;
        }
        let child = entry.path().canonicalize().map_err(|error| {
            format!(
                "cannot resolve resource entry under {}: {error}",
                root.display()
            )
        })?;
        if child.starts_with(root) {
            collect_files_inner(root, &child, files, visited)?;
        }
    }
    Ok(())
}

fn collect_resource_path(
    root: &Path,
    path: &Path,
    kind: &str,
    resources: &mut Vec<PiPackageResource>,
) -> Result<(), String> {
    // Visited canonical directories keep in-root symlink cycles from recursing
    // forever while expanding manifest entries.
    let mut visited = BTreeSet::new();
    collect_resource_path_inner(root, path, kind, resources, &mut visited)
}

fn collect_resource_path_inner(
    root: &Path,
    path: &Path,
    kind: &str,
    resources: &mut Vec<PiPackageResource>,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    if resources.len() >= MAX_RESOURCE_FILES {
        return Ok(());
    }
    let path = path
        .canonicalize()
        .map_err(|error| format!("cannot resolve manifest resource: {error}"))?;
    if !path.starts_with(root) {
        return Ok(());
    }
    if path.is_dir() {
        if !visited.insert(path.clone()) {
            return Ok(());
        }
        if kind == "skill" && path.join("SKILL.md").is_file() {
            add_resource(
                root,
                &path.join("SKILL.md"),
                kind,
                path.file_name(),
                resources,
            )?;
            return Ok(());
        }
        for entry in fs::read_dir(&path)
            .map_err(|error| format!("cannot read manifest resource directory: {error}"))?
        {
            let entry = entry.map_err(|error| format!("cannot inspect resource entry: {error}"))?;
            collect_resource_path_inner(root, &entry.path(), kind, resources, visited)?;
        }
        return Ok(());
    }
    add_resource(root, &path, kind, None, resources)
}

fn add_resource(
    root: &Path,
    path: &Path,
    kind: &str,
    skill_name: Option<&std::ffi::OsStr>,
    resources: &mut Vec<PiPackageResource>,
) -> Result<(), String> {
    let allowed = match kind {
        "extension" => matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("js" | "ts")
        ),
        "skill" | "prompt" => path.extension().and_then(|value| value.to_str()) == Some("md"),
        "theme" => path.extension().and_then(|value| value.to_str()) == Some("json"),
        _ => false,
    };
    if !allowed || !path.is_file() || !path.starts_with(root) {
        return Ok(());
    }
    let relative_path = path
        .strip_prefix(root)
        .map_err(|error| format!("cannot relativize resource path: {error}"))?
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let name = skill_name
        .and_then(|name| name.to_str())
        .or_else(|| {
            (kind == "skill" && path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md"))
                .then(|| path.parent()?.file_name()?.to_str())
                .flatten()
        })
        .or_else(|| path.file_stem().and_then(|name| name.to_str()))
        .unwrap_or_default()
        .to_string();
    resources.push(PiPackageResource {
        kind: resource_kind(kind),
        name,
        relative_path,
    });
    Ok(())
}

fn resource_kind(kind: &str) -> PiPackageResourceKind {
    match kind {
        "extension" => PiPackageResourceKind::Extension,
        "skill" => PiPackageResourceKind::Skill,
        "prompt" => PiPackageResourceKind::Prompt,
        "theme" => PiPackageResourceKind::Theme,
        _ => unreachable!("resource kind comes from RESOURCE_KINDS"),
    }
}

fn counts_for(resources: &[PiPackageResource]) -> PiPackageCounts {
    let mut counts = PiPackageCounts {
        extensions: 0,
        skills: 0,
        prompts: 0,
        themes: 0,
    };
    for resource in resources {
        match resource.kind {
            PiPackageResourceKind::Extension => counts.extensions += 1,
            PiPackageResourceKind::Skill => counts.skills += 1,
            PiPackageResourceKind::Prompt => counts.prompts += 1,
            PiPackageResourceKind::Theme => counts.themes += 1,
        }
    }
    counts
}

fn atomic_write_settings(path: &Path, settings: &Map<String, Value>) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("settings path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create settings directory: {error}"))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| format!("cannot create temporary settings file: {error}"))?;
    let encoded = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    temporary
        .write_all(&encoded)
        .and_then(|_| temporary.write_all(b"\n"))
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| format!("cannot write settings: {error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("cannot atomically replace settings: {}", error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        check_available_updates, check_package_update, inspect_configured_packages,
        inspect_pi_list_output, latest_npm_version, npm_package_spec, parse_ls_remote_reference,
        set_package_disabled, PackageLocations, PiPackageCounts, PiPackageRecord,
        PiPackageResourceKind, PiPackageScope,
    };
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn locations(root: &Path) -> PackageLocations {
        PackageLocations {
            global_settings: root.join("agent/settings.json"),
            project_settings: Some(root.join("workspace/.pi/settings.json")),
            global_agent_dir: root.join("agent"),
            project_root: Some(root.join("workspace")),
        }
    }

    fn write_json(path: &Path, value: serde_json::Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    fn package_root(locations: &PackageLocations, scope: &str, name: &str) -> PathBuf {
        let base = if scope == "global" {
            locations.global_agent_dir.clone()
        } else {
            locations.project_root.clone().unwrap().join(".pi")
        };
        base.join("npm/node_modules").join(name)
    }

    fn manifest_package(root: &Path, name: &str, resources: serde_json::Value) {
        fs::create_dir_all(root).unwrap();
        write_json(
            &root.join("package.json"),
            json!({"name": name, "version": "1.0.0", "description": "test package", "pi": resources}),
        );
    }

    #[test]
    fn parses_pi_list_scopes_and_resolved_paths() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(
            &paths.global_settings,
            json!({"packages": ["npm:global-package"]}),
        );
        write_json(
            paths.project_settings.as_ref().unwrap(),
            json!({"packages": ["npm:project-package"]}),
        );
        let global = package_root(&paths, "global", "global-package");
        manifest_package(
            &global,
            "global-package",
            json!({"extensions": ["extensions/index.js"]}),
        );
        fs::create_dir_all(global.join("extensions")).unwrap();
        fs::write(global.join("extensions/index.js"), "export default {};").unwrap();
        let project = package_root(&paths, "project", "project-package");
        manifest_package(
            &project,
            "project-package",
            json!({"skills": ["skills/demo"]}),
        );
        fs::create_dir_all(project.join("skills/demo")).unwrap();
        fs::write(project.join("skills/demo/SKILL.md"), "# demo").unwrap();

        let output = format!(
            "User packages:\n  npm:global-package\n    {}\nProject packages:\n  npm:project-package\n    {}\n",
            global.display(),
            project.display()
        );
        let records = inspect_pi_list_output(&output, &paths).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].scope, PiPackageScope::Global);
        assert_eq!(records[1].scope, PiPackageScope::Project);
        assert_eq!(
            records[0].installed_path.as_deref(),
            Some(global.to_str().unwrap())
        );
        assert_eq!(records[0].counts.extensions, 1);
        assert_eq!(records[1].counts.skills, 1);
    }

    #[test]
    fn parses_filtered_pi_list_sources_and_disabled_state() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        let disabled_source = "git:github.com/jonjonrankin/pi-caveman";
        let active_source = "git:github.com/example/active-package";
        write_json(
            &paths.global_settings,
            json!({
                "packages": [
                    {
                        "source": disabled_source,
                        "extensions": [],
                        "skills": [],
                        "prompts": [],
                        "themes": []
                    },
                    {
                        "source": active_source,
                        "extensions": ["extensions/main.ts"],
                        "skills": [],
                        "prompts": [],
                        "themes": []
                    }
                ]
            }),
        );
        let output = format!(
            "User packages:\n  {disabled_source} (filtered)\n  {active_source} (filtered)\n"
        );

        let records = inspect_pi_list_output(&output, &paths).unwrap();
        assert_eq!(records[0].source, disabled_source);
        assert!(records[0].disabled);
        assert_eq!(records[1].source, active_source);
        assert!(!records[1].disabled);
    }

    #[test]
    fn keeps_global_and_project_packages_with_distinct_scopes() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(
            &paths.global_settings,
            json!({"packages": ["npm:global-package"]}),
        );
        write_json(
            paths.project_settings.as_ref().unwrap(),
            json!({"packages": ["npm:project-package"]}),
        );
        for (scope, name) in [("global", "global-package"), ("project", "project-package")] {
            let package = package_root(&paths, scope, name);
            manifest_package(
                &package,
                name,
                json!({"extensions": ["extensions/index.js"]}),
            );
            fs::create_dir_all(package.join("extensions")).unwrap();
            fs::write(package.join("extensions/index.js"), "export default {};").unwrap();
        }
        let records = inspect_configured_packages(&paths).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].scope, PiPackageScope::Global);
        assert_eq!(records[1].scope, PiPackageScope::Project);
        assert_eq!(records[0].counts.extensions, 1);
    }

    #[test]
    fn reports_disabled_configured_package() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(
            &paths.global_settings,
            json!({"packages": [{"source": "npm:disabled", "extensions": [], "skills": [], "prompts": [], "themes": []}]}),
        );
        let records = inspect_configured_packages(&paths).unwrap();
        assert!(records.iter().any(|record| record.disabled));
    }

    #[test]
    fn autoload_false_delta_is_not_reported_as_disabled() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(
            &paths.global_settings,
            json!({"packages": [{"source": "npm:delta", "autoload": false}]}),
        );
        let records = inspect_configured_packages(&paths).unwrap();
        assert!(!records[0].disabled);
    }

    #[test]
    fn keeps_configured_package_when_install_path_is_missing() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(&paths.global_settings, json!({"packages": ["npm:missing"]}));
        let records = inspect_configured_packages(&paths).unwrap();
        let missing = records
            .iter()
            .find(|record| record.source == "npm:missing")
            .unwrap();
        assert_eq!(missing.installed_path, None);
        assert_eq!(missing.resources.len(), 0);
    }

    #[test]
    fn malformed_manifest_with_resource_directory_has_zero_resources() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(
            &paths.global_settings,
            json!({"packages": ["npm:bad", "npm:healthy"]}),
        );
        let package = package_root(&paths, "global", "bad");
        fs::create_dir_all(package.join("extensions")).unwrap();
        fs::write(package.join("package.json"), "{").unwrap();
        fs::write(package.join("extensions/index.js"), "export default {};").unwrap();
        let healthy = package_root(&paths, "global", "healthy");
        manifest_package(
            &healthy,
            "healthy",
            json!({"extensions": ["extensions/index.js"]}),
        );
        fs::create_dir_all(healthy.join("extensions")).unwrap();
        fs::write(healthy.join("extensions/index.js"), "export default {};").unwrap();
        let records = inspect_configured_packages(&paths).unwrap();
        let bad = records
            .iter()
            .find(|record| record.source == "npm:bad")
            .unwrap();
        let good = records
            .iter()
            .find(|record| record.source == "npm:healthy")
            .unwrap();
        assert_eq!(bad.package_name, None);
        assert_eq!(bad.counts.extensions, 0);
        assert!(bad.resources.is_empty());
        assert_eq!(good.counts.extensions, 1);
    }

    #[test]
    fn missing_manifest_falls_back_to_conventional_directories() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(&paths.global_settings, json!({"packages": ["npm:empty"]}));
        let package = package_root(&paths, "global", "empty");
        for file in [
            "extensions/main.js",
            "skills/demo/SKILL.md",
            "prompts/review.md",
            "themes/dark.json",
        ] {
            let path = package.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "resource").unwrap();
        }
        let record = &inspect_configured_packages(&paths).unwrap()[0];
        assert!(record.installed_path.is_some());
        assert_eq!(record.package_name, None);
        assert_eq!(record.version, None);
        assert_eq!(record.counts.extensions, 1);
        assert_eq!(record.counts.skills, 1);
        assert_eq!(record.counts.prompts, 1);
        assert_eq!(record.counts.themes, 1);
        assert!(record.resources.iter().any(|resource| resource.kind
            == PiPackageResourceKind::Skill
            && resource.name == "demo"));
    }

    #[test]
    fn conventional_fallback_follows_pi_directory_semantics() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(
            &paths.global_settings,
            json!({"packages": ["npm:conventional", "npm:partial", "npm:nopi"]}),
        );
        let conventional = package_root(&paths, "global", "conventional");
        for file in [
            "extensions/main.js",
            "extensions/nested/skip.ts",
            "skills/demo/SKILL.md",
            "skills/nested/deep/SKILL.md",
            "skills/notes.md",
            "skills/nested/extra.md",
            "prompts/review.md",
            "prompts/nested/skip.md",
            "themes/dark.json",
            "themes/nested/skip.json",
        ] {
            let path = conventional.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "resource").unwrap();
        }
        let partial = package_root(&paths, "global", "partial");
        fs::create_dir_all(partial.join("extensions")).unwrap();
        fs::write(partial.join("extensions/partial.js"), "resource").unwrap();
        let nopi = package_root(&paths, "global", "nopi");
        write_json(
            &nopi.join("package.json"),
            json!({"name": "nopi", "version": "0.2.0"}),
        );
        fs::create_dir_all(nopi.join("extensions")).unwrap();
        fs::write(nopi.join("extensions/nopi.js"), "resource").unwrap();
        let records = inspect_configured_packages(&paths).unwrap();
        let conventional = records
            .iter()
            .find(|record| record.source == "npm:conventional")
            .unwrap();
        // Pi conventional rules: top-level files for extensions/prompts/themes,
        // recursive SKILL.md folders plus top-level loose .md files for skills.
        assert_eq!(conventional.counts.extensions, 1);
        assert_eq!(conventional.counts.skills, 3);
        assert_eq!(conventional.counts.prompts, 1);
        assert_eq!(conventional.counts.themes, 1);
        let skill_paths: Vec<&str> = conventional
            .resources
            .iter()
            .filter(|resource| resource.kind == PiPackageResourceKind::Skill)
            .map(|resource| resource.relative_path.as_str())
            .collect();
        assert_eq!(
            skill_paths,
            vec![
                "skills/demo/SKILL.md",
                "skills/nested/deep/SKILL.md",
                "skills/notes.md"
            ]
        );
        let partial = records
            .iter()
            .find(|record| record.source == "npm:partial")
            .unwrap();
        assert_eq!(partial.counts.extensions, 1);
        let nopi = records
            .iter()
            .find(|record| record.source == "npm:nopi")
            .unwrap();
        assert_eq!(nopi.counts.extensions, 1);
        assert_eq!(nopi.package_name.as_deref(), Some("nopi"));
    }

    #[test]
    #[cfg(unix)]
    fn resource_traversal_terminates_on_symlink_cycles() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(
            &paths.global_settings,
            json!({"packages": ["npm:glob-cycle", "npm:dir-cycle", "npm:skills-cycle"]}),
        );
        let glob_package = package_root(&paths, "global", "glob-cycle");
        manifest_package(
            &glob_package,
            "glob-cycle",
            json!({"extensions": ["extensions/**/*.js"]}),
        );
        let dir_package = package_root(&paths, "global", "dir-cycle");
        manifest_package(
            &dir_package,
            "dir-cycle",
            json!({"extensions": ["./extensions"]}),
        );
        for package in [&glob_package, &dir_package] {
            fs::create_dir_all(package.join("extensions/nested")).unwrap();
            fs::write(package.join("extensions/main.js"), "resource").unwrap();
            fs::write(package.join("extensions/nested/deep.js"), "resource").unwrap();
            // In-root cycle: canonicalizes back to the package root.
            std::os::unix::fs::symlink("..", package.join("extensions/loop")).unwrap();
        }
        let skills_package = package_root(&paths, "global", "skills-cycle");
        fs::create_dir_all(skills_package.join("skills/demo")).unwrap();
        fs::write(skills_package.join("skills/demo/SKILL.md"), "resource").unwrap();
        std::os::unix::fs::symlink(".", skills_package.join("skills/loop")).unwrap();
        let records = inspect_configured_packages(&paths).unwrap();
        let glob_cycle = records
            .iter()
            .find(|record| record.source == "npm:glob-cycle")
            .unwrap();
        assert_eq!(glob_cycle.counts.extensions, 2);
        let dir_cycle = records
            .iter()
            .find(|record| record.source == "npm:dir-cycle")
            .unwrap();
        assert_eq!(dir_cycle.counts.extensions, 2);
        let skills_cycle = records
            .iter()
            .find(|record| record.source == "npm:skills-cycle")
            .unwrap();
        assert_eq!(skills_cycle.counts.skills, 1);
    }

    #[test]
    fn maps_manifest_resources_and_camel_case_fields() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(&paths.global_settings, json!({"packages": ["npm:rich"]}));
        let package = package_root(&paths, "global", "rich");
        manifest_package(
            &package,
            "rich",
            json!({
                "extensions": ["extensions/main.js"],
                "skills": ["skills/demo/SKILL.md"],
                "prompts": ["prompts/demo.md"],
                "themes": ["themes/demo.json"]
            }),
        );
        for file in [
            "extensions/main.js",
            "skills/demo/SKILL.md",
            "prompts/demo.md",
            "themes/demo.json",
        ] {
            let path = package.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "resource").unwrap();
        }
        let record = &inspect_configured_packages(&paths).unwrap()[0];
        assert_eq!(record.counts.extensions, 1);
        assert_eq!(record.counts.skills, 1);
        assert_eq!(record.counts.prompts, 1);
        assert_eq!(record.counts.themes, 1);
        assert!(record.resources.iter().any(|resource| resource.kind
            == PiPackageResourceKind::Skill
            && resource.name == "demo"));
        let json = serde_json::to_value(record).unwrap();
        assert_eq!(json["scope"], "global");
        assert!(json["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|resource| resource["kind"] == "skill"));
        assert!(json.get("installedPath").is_some());
        assert!(json["resources"][0].get("relativePath").is_some());
        assert!(json["counts"].get("extensions").is_some());
    }

    #[test]
    fn expands_manifest_directories_into_supported_resources() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(
            &paths.global_settings,
            json!({"packages": ["npm:directories"]}),
        );
        let package = package_root(&paths, "global", "directories");
        manifest_package(
            &package,
            "directories",
            json!({
                "extensions": ["./extensions"],
                "skills": ["./skills"],
                "prompts": ["./prompts"],
                "themes": ["./themes"]
            }),
        );
        for file in [
            "extensions/main.ts",
            "skills/demo/SKILL.md",
            "prompts/demo.md",
            "themes/demo.json",
        ] {
            let path = package.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "resource").unwrap();
        }
        let record = &inspect_configured_packages(&paths).unwrap()[0];
        assert_eq!(record.counts.extensions, 1);
        assert_eq!(record.counts.skills, 1);
        assert_eq!(record.counts.prompts, 1);
        assert_eq!(record.counts.themes, 1);
        assert!(record.resources.iter().any(|resource| {
            resource.kind == PiPackageResourceKind::Skill && resource.name == "demo"
        }));
    }

    #[test]
    fn expands_manifest_globs_into_supported_resources() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(&paths.global_settings, json!({"packages": ["npm:globs"]}));
        let package = package_root(&paths, "global", "globs");
        manifest_package(
            &package,
            "globs",
            json!({"extensions": ["extensions/**/*.js", "!extensions/nested/ignored.js"]}),
        );
        fs::create_dir_all(package.join("extensions/nested")).unwrap();
        fs::write(package.join("extensions/main.js"), "resource").unwrap();
        fs::write(package.join("extensions/ignored.ts"), "resource").unwrap();
        fs::write(package.join("extensions/nested/deep.js"), "resource").unwrap();
        fs::write(package.join("extensions/nested/ignored.js"), "resource").unwrap();
        let record = &inspect_configured_packages(&paths).unwrap()[0];
        assert_eq!(record.counts.extensions, 2);
        assert_eq!(record.resources[0].relative_path, "extensions/main.js");
        assert_eq!(
            record.resources[1].relative_path,
            "extensions/nested/deep.js"
        );
    }

    #[test]
    fn disabling_and_enabling_uses_pi_entry_format_and_is_idempotent() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(
            &paths.global_settings,
            json!({"packages": ["npm:known"], "custom": true}),
        );
        assert!(set_package_disabled(&paths, "global", "npm:known", true).unwrap());
        let disabled: serde_json::Value =
            serde_json::from_slice(&fs::read(&paths.global_settings).unwrap()).unwrap();
        assert_eq!(disabled["packages"][0]["extensions"], json!([]));
        assert_eq!(disabled["packages"][0]["skills"], json!([]));
        assert_eq!(disabled["packages"][0]["prompts"], json!([]));
        assert_eq!(disabled["packages"][0]["themes"], json!([]));
        assert!(disabled["packages"][0].get("autoload").is_none());
        assert_eq!(disabled["custom"], json!(true));
        assert!(set_package_disabled(&paths, "global", "npm:known", false).unwrap());
        assert!(!set_package_disabled(&paths, "global", "npm:known", false).unwrap());
        let enabled: serde_json::Value =
            serde_json::from_slice(&fs::read(&paths.global_settings).unwrap()).unwrap();
        assert_eq!(enabled["packages"][0], json!("npm:known"));
    }

    #[test]
    fn git_and_local_sources_resolve_without_escape() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        for source in [
            "git:user/repo",
            "git:https://github.com/owner/repo",
            "git:https://github.com/owner/repo@main",
            "git:https://github.com/owner/repo#main",
            "git:git@github.com:owner/repo",
            "git:git@github.com:owner/repo@main",
            "git:ssh://git@github.com/owner/repo@main",
        ] {
            write_json(&paths.global_settings, json!({"packages": [source]}));
            let git_root =
                paths
                    .global_agent_dir
                    .join("git/github.com")
                    .join(if source.contains("owner") {
                        "owner/repo"
                    } else {
                        "user/repo"
                    });
            fs::create_dir_all(&git_root).unwrap();
            let records = inspect_configured_packages(&paths).unwrap();
            assert!(records[0].installed_path.is_some(), "{source}");
            fs::remove_dir_all(paths.global_agent_dir.join("git")).unwrap();
        }
        write_json(
            &paths.global_settings,
            json!({"packages": ["npm:@scope/name@1.2.3"]}),
        );
        let npm_root = paths.global_agent_dir.join("npm/node_modules/@scope/name");
        fs::create_dir_all(&npm_root).unwrap();
        assert!(inspect_configured_packages(&paths).unwrap()[0]
            .installed_path
            .is_some());

        let local_source = "local:foo/bar";
        write_json(&paths.global_settings, json!({"packages": [local_source]}));
        let local_root = paths.global_agent_dir.join(local_source);
        fs::create_dir_all(&local_root).unwrap();
        assert!(inspect_configured_packages(&paths).unwrap()[0]
            .installed_path
            .is_some());

        let scp_source = "git@github.com:owner/repo";
        write_json(&paths.global_settings, json!({"packages": [scp_source]}));
        let scp_root = paths.global_agent_dir.join(scp_source);
        fs::create_dir_all(&scp_root).unwrap();
        assert!(inspect_configured_packages(&paths).unwrap()[0]
            .installed_path
            .is_some());
    }

    #[test]
    fn manifest_entries_cannot_escape_package_root() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(&paths.global_settings, json!({"packages": ["npm:bounded"]}));
        let package = package_root(&paths, "global", "bounded");
        manifest_package(
            &package,
            "bounded",
            json!({"extensions": ["../outside.js", "extensions/inside.js"]}),
        );
        fs::create_dir_all(package.join("extensions")).unwrap();
        fs::write(package.join("extensions/inside.js"), "export default {};").unwrap();
        fs::write(package.parent().unwrap().join("outside.js"), "outside").unwrap();
        let record = &inspect_configured_packages(&paths).unwrap()[0];
        assert_eq!(record.counts.extensions, 1);
        assert_eq!(record.resources[0].relative_path, "extensions/inside.js");
    }

    #[test]
    fn disabling_project_package_preserves_unrelated_settings() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        let settings = json!({"packages": ["npm:project", "npm:other"], "thinkingLevel": "high", "custom": {"keep": true}});
        write_json(paths.project_settings.as_ref().unwrap(), settings.clone());
        assert!(set_package_disabled(&paths, "project", "npm:project", true).unwrap());
        let after: serde_json::Value =
            serde_json::from_slice(&fs::read(paths.project_settings.as_ref().unwrap()).unwrap())
                .unwrap();
        assert_eq!(after["thinkingLevel"], settings["thinkingLevel"]);
        assert_eq!(after["custom"], settings["custom"]);
        assert_eq!(after["packages"][1], settings["packages"][1]);
    }

    #[test]
    fn disabling_unknown_source_returns_error_without_writing() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(&paths.global_settings, json!({"packages": ["npm:known"]}));
        let before = fs::read(&paths.global_settings).unwrap();
        assert!(set_package_disabled(&paths, "global", "npm:unknown", true).is_err());
        assert_eq!(fs::read(&paths.global_settings).unwrap(), before);
    }

    #[test]
    fn rejects_invalid_scope_before_any_settings_write() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        write_json(&paths.global_settings, json!({"packages": ["npm:known"]}));
        let before = fs::read(&paths.global_settings).unwrap();
        assert!(set_package_disabled(&paths, "other", "npm:known", true).is_err());
        assert_eq!(fs::read(&paths.global_settings).unwrap(), before);
    }

    fn npm_record(
        source: &str,
        installed_path: Option<&str>,
        installed_version: Option<&str>,
    ) -> PiPackageRecord {
        PiPackageRecord {
            source: source.to_string(),
            scope: PiPackageScope::Global,
            installed_path: installed_path.map(ToOwned::to_owned),
            package_name: None,
            version: installed_version.map(ToOwned::to_owned),
            description: None,
            disabled: false,
            counts: PiPackageCounts {
                extensions: 0,
                skills: 0,
                prompts: 0,
                themes: 0,
            },
            resources: Vec::new(),
        }
    }

    #[test]
    fn latest_npm_version_handles_string_and_array_shapes() {
        assert_eq!(
            latest_npm_version("\"1.2.3\"", None).as_deref(),
            Some("1.2.3")
        );
        assert_eq!(
            latest_npm_version("[\"1.5.0\", \"2.0.0\", \"1.0.0\"]", None).as_deref(),
            Some("2.0.0")
        );
        assert_eq!(latest_npm_version("not json", None), None);
        assert_eq!(latest_npm_version("null", None), None);
    }

    #[test]
    fn latest_npm_version_filters_invalid_versions_and_applies_range() {
        assert_eq!(
            latest_npm_version("[\"latest\", \"1.0.0\", \"not-semver\"]", None).as_deref(),
            Some("1.0.0")
        );
        let range = semver::VersionReq::parse("^1.0.0").unwrap();
        assert_eq!(
            latest_npm_version("[\"3.0.0\", \"2.0.0\", \"1.5.0\", \"1.0.0\"]", Some(&range))
                .as_deref(),
            Some("1.5.0")
        );
    }

    #[test]
    fn npm_package_spec_parses_scoped_pinned_and_range_specs() {
        assert_eq!(npm_package_spec("foo").unwrap(), ("foo".to_string(), None));
        assert_eq!(
            npm_package_spec("foo@1.2.3").unwrap(),
            ("foo".to_string(), Some("1.2.3".to_string()))
        );
        assert_eq!(
            npm_package_spec("foo@^1.0.0").unwrap(),
            ("foo".to_string(), Some("^1.0.0".to_string()))
        );
        assert_eq!(
            npm_package_spec("@scope/name").unwrap(),
            ("@scope/name".to_string(), None)
        );
        assert_eq!(
            npm_package_spec("@scope/name@2.0.0").unwrap(),
            ("@scope/name".to_string(), Some("2.0.0".to_string()))
        );
        assert!(npm_package_spec("").is_err());
        assert!(npm_package_spec("@missing-slash").is_err());
    }

    #[test]
    fn check_package_update_short_circuits_on_exact_pin_without_network() {
        let root = tempdir().unwrap();
        let paths = locations(root.path());
        // Exact-pin version is Parseable, so the check must return false without
        // ever invoking npm; a real npm call here would fail the test via PI_OFFLINE.
        let record = npm_record("npm:foo@1.2.3", Some("/tmp/installed"), Some("1.0.0"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(!runtime.block_on(check_package_update(&record, &paths)));
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git should launch");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn check_available_updates_reports_git_package_behind_upstream() {
        let root = tempdir().unwrap();
        let seed = root.path().join("seed");
        let bare = root.path().join("origin.git");
        let installed = root.path().join("installed");

        run_git(
            root.path(),
            &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        run_git(root.path(), &["init", "-b", "main", seed.to_str().unwrap()]);
        fs::write(seed.join("README.md"), "one").unwrap();
        run_git(&seed, &["add", "."]);
        run_git(
            &seed,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "one",
            ],
        );
        run_git(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
        run_git(&seed, &["push", "origin", "main"]);
        run_git(
            root.path(),
            &["clone", bare.to_str().unwrap(), installed.to_str().unwrap()],
        );

        // A second upstream commit leaves the installed clone behind.
        fs::write(seed.join("README.md"), "two").unwrap();
        run_git(&seed, &["add", "."]);
        run_git(
            &seed,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "two",
            ],
        );
        run_git(&seed, &["push", "origin", "main"]);

        let paths = locations(root.path());
        let record = npm_record(
            "git:ssh://git@example.test/palan/test-repo.git",
            Some(installed.to_str().unwrap()),
            None,
        );
        let updates = check_available_updates(&[record], &paths).await;
        assert_eq!(updates.len(), 1);
        assert!(updates[0].available);
    }

    #[test]
    fn parse_ls_remote_selects_only_matching_reference_heads() {
        let sha = "0123456789abcdef0123456789abcdef01234567"; // 40 hex chars
        assert_eq!(
            parse_ls_remote_reference(&format!("{sha}\tHEAD")).as_deref(),
            Some(sha)
        );
        assert_eq!(
            parse_ls_remote_reference(&format!("{sha}\trefs/heads/main")).as_deref(),
            Some(sha)
        );
        assert_eq!(
            parse_ls_remote_reference(&format!("{sha}\trefs/tags/v1.0.0")),
            None
        );
        assert_eq!(parse_ls_remote_reference("deadbeef\tHEAD"), None);
        assert_eq!(
            parse_ls_remote_reference(&format!("{sha}\tHEAD\textra")),
            Some(sha.to_string())
        );
        assert_eq!(parse_ls_remote_reference(""), None);
    }

    // Landing has no workspace binding, so locations carry no project root;
    // the npm update check must still run against the global agent dir.
    // Probes the real registry; skipped when the suite runs PI_OFFLINE.
    #[tokio::test]
    async fn check_available_updates_checks_npm_without_project_root() {
        if std::env::var("PI_OFFLINE").is_ok() {
            return;
        }
        let root = tempdir().unwrap();
        let paths = PackageLocations {
            global_settings: root.path().join("agent/settings.json"),
            project_settings: None,
            global_agent_dir: root.path().join("agent"),
            project_root: None,
        };
        fs::create_dir_all(&paths.global_agent_dir).unwrap();
        // installed_path is only consulted by the git branch; the npm branch
        // compares record.version against the registry.
        let record = npm_record("npm:pi-lens", Some("/tmp"), Some("0.0.1"));
        let available = check_package_update(&record, &paths).await;
        assert!(
            available,
            "npm update check must run without a project root"
        );
    }
}
