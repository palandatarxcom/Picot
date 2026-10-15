#![cfg_attr(not(test), allow(dead_code))]
// ABOUTME: Port of Picot's Settings > Skills inventory pipeline: skill
// ABOUTME: discovery (SKILL.md frontmatter), settings.json enable rules with
// ABOUTME: `!`/`+`/`-` override semantics, shadowing/ambiguity resolution,
// ABOUTME: the group tree rendered by the UI, and package skill candidates.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const CONFIG_DIR_NAME: &str = ".pi";
const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;

// ── Discovery ─────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct RawSkill {
    pub canonical_path: String,
    pub file_path: String,
    pub skill_dir: String,
    pub name: String,
    pub description: String,
    pub is_configured_file: bool,
}

#[derive(Clone, Debug)]
pub struct DiscoveredRoot {
    pub dir: String,
    /// "pi" native skill dir vs "agents" shared dir.
    pub mode: &'static str,
    pub base_dir: String,
    pub scope: &'static str,
    pub source: &'static str,
}

/// Minimal frontmatter reader: `key: value` lines inside a `---` block,
/// with YAML block-scalar support for `description` (`>`, `|`, and `-`/`+`
/// chomping indicators) so multi-line skill descriptions unfold correctly.
pub(crate) fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>, bool) {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return (None, None, false);
    }
    let Some(end) = normalized[3..].find("\n---") else {
        return (None, None, false);
    };
    let block = &normalized[4..3 + end];
    let mut name = None;
    let mut description = None;
    let mut disable_model_invocation = false;
    let mut lines = block.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let trimmed = value.trim();
        // A block scalar indicator (or an empty value) means the content is
        // the following, more-indented lines. `|` keeps line breaks; `>`
        // folds them into spaces; trailing `-` strips the final newline.
        let is_block_indicator = trimmed.is_empty()
            || trimmed
                .chars()
                .all(|c| c == '>' || c == '|' || c == '-' || c == '+');
        let value = if is_block_indicator && !trimmed.starts_with('"') {
            let folded = trimmed.starts_with('>');
            let chomp_strip = trimmed.contains('-');
            let base_indent = line.chars().take_while(|c| c.is_whitespace()).count();
            let mut content_lines: Vec<String> = Vec::new();
            while let Some(next) = lines.peek() {
                let is_blank = next.trim().is_empty();
                let indent = next.chars().take_while(|c| c.is_whitespace()).count();
                if !is_blank && indent <= base_indent {
                    break;
                }
                content_lines.push(next.to_string());
                lines.next();
            }
            let mut text = if folded {
                // YAML folding: single newlines become spaces; blank lines
                // survive as paragraph breaks.
                let mut out = String::new();
                let mut pending_breaks = 0usize;
                for content_line in &content_lines {
                    if content_line.trim().is_empty() {
                        pending_breaks += 1;
                        continue;
                    }
                    if !out.is_empty() {
                        if pending_breaks == 0 {
                            // A single fold break joins lines with a space.
                            out.push(' ');
                        } else {
                            // Blank lines survive as paragraph breaks.
                            for _ in 0..pending_breaks {
                                out.push('\n');
                            }
                        }
                    }
                    pending_breaks = 0;
                    out.push_str(content_line.trim());
                }
                out
            } else {
                content_lines
                    .iter()
                    .map(|l| l.trim_start().to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if !chomp_strip {
                text.push('\n');
            }
            text.trim().to_string()
        } else {
            trimmed.trim_matches('"').trim_matches('\'').to_string()
        };
        match key {
            "name" => name = Some(value),
            "description" => description = Some(value),
            "disable-model-invocation" if value == "true" => disable_model_invocation = true,
            _ => {}
        }
    }
    (name, description, disable_model_invocation)
}

fn validate_skill_name(name: &str, errors: &mut Vec<String>) {
    if name.len() > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({})",
            name.len()
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        errors
            .push("name contains invalid characters (must be lowercase a-z, 0-9, hyphens)".into());
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".into());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".into());
    }
}

fn push_skill(
    file_path: &Path,
    _root: &DiscoveredRoot,
    diagnostics: &mut Vec<Value>,
    out: &mut Vec<RawSkill>,
    is_configured_file: bool,
) {
    let canonical = std::fs::canonicalize(file_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file_path.to_string_lossy().into_owned());
    let Ok(raw) = std::fs::read_to_string(file_path) else {
        diagnostics.push(json!({
            "path": file_path.to_string_lossy(),
            "message": "failed to read skill file",
        }));
        return;
    };
    // The disable-model-invocation flag is consumed by the installer's scan
    // (skill_install.rs); discovery here ignores it.
    let (name_from_fm, description_from_fm, _disable_model_invocation) = parse_frontmatter(&raw);
    let skill_dir = file_path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent_name = Path::new(&skill_dir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let description = description_from_fm.unwrap_or_default().trim().to_string();
    let name = name_from_fm.unwrap_or(parent_name);
    if description.is_empty() {
        diagnostics.push(json!({
            "path": file_path.to_string_lossy(),
            "message": "description is required",
        }));
        return;
    }
    if description.len() > MAX_DESCRIPTION_LENGTH {
        diagnostics.push(json!({
            "path": file_path.to_string_lossy(),
            "message": format!("description exceeds {MAX_DESCRIPTION_LENGTH} characters"),
        }));
    }
    let mut name_errors = Vec::new();
    validate_skill_name(&name, &mut name_errors);
    for message in name_errors {
        diagnostics.push(json!({ "path": file_path.to_string_lossy(), "message": message }));
    }
    out.push(RawSkill {
        canonical_path: canonical,
        file_path: file_path.to_string_lossy().into_owned(),
        skill_dir,
        name,
        description,
        is_configured_file,
    });
}

/// Recursive walk mirroring Pi: a directory containing SKILL.md is one skill
/// and recursion stops under it; hidden dirs and node_modules are skipped; in
/// `pi` mode loose root-level Markdown files are discovered.
fn discover_from_root(root: &DiscoveredRoot, diagnostics: &mut Vec<Value>) -> Vec<RawSkill> {
    let mut out = Vec::new();
    let root_dir = Path::new(&root.dir);
    let Ok(root_meta) = std::fs::metadata(root_dir) else {
        return out;
    };
    if root_meta.is_file() {
        if root.dir.ends_with(".md") {
            push_skill(root_dir, root, diagnostics, &mut out, true);
        } else {
            diagnostics.push(json!({
                "path": root.dir,
                "message": "configured skill file must be Markdown",
            }));
        }
        return out;
    }
    fn collect(
        dir: &Path,
        root: &DiscoveredRoot,
        diagnostics: &mut Vec<Value>,
        out: &mut Vec<RawSkill>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in &entries {
            if path.file_name().map(|n| n == "SKILL.md").unwrap_or(false) && path.is_file() {
                push_skill(path, root, diagnostics, out, false);
                return;
            }
        }
        for path in &entries {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name == "SKILL.md" || name.starts_with('.') || name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                collect(path, root, diagnostics, out);
            } else if root.mode == "pi"
                && dir == Path::new(&root.dir)
                && name.ends_with(".md")
                && path.is_file()
            {
                push_skill(path, root, diagnostics, out, false);
            }
        }
    }
    collect(root_dir, root, diagnostics, &mut out);
    out.dedup_by(|a, b| a.canonical_path == b.canonical_path);
    out
}

// ── Settings + rules ──────────────────────────────────────────────────

fn read_settings_skills(settings_path: &Path) -> (Vec<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(settings_path) else {
        return (Vec::new(), None);
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return (Vec::new(), Some("invalid JSON".into()));
    };
    let Some(skills) = parsed.get("skills").and_then(Value::as_array) else {
        return (Vec::new(), None);
    };
    (
        skills
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .collect(),
        None,
    )
}

fn is_override(entry: &str) -> bool {
    entry.starts_with('!') || entry.starts_with('+') || entry.starts_with('-')
}

fn is_plain_glob(entry: &str) -> bool {
    !is_override(entry) && (entry.contains('*') || entry.contains('?'))
}

/// Minimal glob matching mirroring minimatch for the patterns Picot writes:
/// `*` (no `/`), `**` (any depth), `?` (single non-`/` char).
fn glob_match(pattern: &str, value: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), value.as_bytes())
}

fn glob_match_inner(pattern: &[u8], value: &[u8]) -> bool {
    if pattern.is_empty() {
        return value.is_empty();
    }
    if pattern.starts_with(b"**") {
        let rest = if pattern.len() > 2 {
            let after = &pattern[2..];
            if after.first() == Some(&b'/') {
                &after[1..]
            } else {
                after
            }
        } else {
            &pattern[2..]
        };
        if rest.is_empty() {
            return true;
        }
        for (index, _) in value.iter().enumerate() {
            if glob_match_inner(rest, &value[index..]) {
                return true;
            }
        }
        return glob_match_inner(rest, b"");
    }
    match pattern.first() {
        Some(b'*') => {
            for index in 0..=value.len() {
                if value[..index].contains(&b'/') {
                    break;
                }
                if glob_match_inner(&pattern[1..], &value[index..]) {
                    return true;
                }
            }
            false
        }
        Some(b'?') => {
            !value.is_empty() && value[0] != b'/' && glob_match_inner(&pattern[1..], &value[1..])
        }
        Some(first) => {
            !value.is_empty() && value[0] == *first && glob_match_inner(&pattern[1..], &value[1..])
        }
        None => false,
    }
}

fn to_posix(path: &str) -> String {
    path.replace('\\', "/")
}

fn normalize_exact_pattern(pattern: &str) -> String {
    let p = pattern
        .strip_prefix("./")
        .unwrap_or(pattern)
        .replace('\\', "/");
    p
}

struct MatchContext {
    rel: String,
    abs: String,
    name: String,
    parent_rel: String,
    parent_abs: String,
    parent_name: String,
}

fn build_match_context(file_path: &str, base_dir: &str) -> MatchContext {
    let rel = to_posix(
        Path::new(file_path)
            .strip_prefix(base_dir)
            .unwrap_or(Path::new(file_path))
            .to_string_lossy()
            .as_ref(),
    );
    let parent = Path::new(file_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    MatchContext {
        name: Path::new(file_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        parent_rel: to_posix(
            Path::new(&parent)
                .strip_prefix(base_dir)
                .unwrap_or(Path::new(&parent))
                .to_string_lossy()
                .as_ref(),
        ),
        parent_abs: to_posix(&parent),
        abs: to_posix(file_path),
        rel,
        parent_name: Path::new(&parent)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

fn matches_any_pattern(ctx: &MatchContext, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        let p = to_posix(pattern);
        glob_match(&p, &ctx.rel)
            || glob_match(&p, &ctx.name)
            || glob_match(&p, &ctx.abs)
            || glob_match(&p, &ctx.parent_rel)
            || glob_match(&p, &ctx.parent_name)
            || glob_match(&p, &ctx.parent_abs)
    })
}

fn matches_any_exact(ctx: &MatchContext, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        let n = normalize_exact_pattern(pattern);
        n == ctx.rel || n == ctx.abs || n == ctx.parent_rel || n == ctx.parent_abs
    })
}

fn overrides_of(skills: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut excl = Vec::new();
    let mut finc = Vec::new();
    let mut fexc = Vec::new();
    for entry in skills {
        if let Some(body) = entry.strip_prefix('!') {
            excl.push(body.to_string());
        } else if let Some(body) = entry.strip_prefix('+') {
            finc.push(body.to_string());
        } else if let Some(body) = entry.strip_prefix('-') {
            fexc.push(body.to_string());
        }
    }
    (excl, finc, fexc)
}

fn is_enabled_by_overrides(ctx: &MatchContext, skills: &[String]) -> (bool, Vec<String>) {
    let (excl, finc, fexc) = overrides_of(skills);
    let mut matched = Vec::new();
    let mut enabled = true;
    if !excl.is_empty() {
        let hits: Vec<String> = excl
            .iter()
            .filter(|p| matches_any_pattern(ctx, std::slice::from_ref(*p)))
            .cloned()
            .collect();
        if !hits.is_empty() {
            enabled = false;
            for hit in hits {
                matched.push(format!("!{hit}"));
            }
        }
    }
    if !finc.is_empty() {
        let hits: Vec<String> = finc
            .iter()
            .filter(|p| matches_any_exact(ctx, std::slice::from_ref(*p)))
            .cloned()
            .collect();
        if !hits.is_empty() {
            enabled = true;
            for hit in hits {
                matched.push(format!("+{hit}"));
            }
        }
    }
    if !fexc.is_empty() {
        let hits: Vec<String> = fexc
            .iter()
            .filter(|p| matches_any_exact(ctx, std::slice::from_ref(*p)))
            .cloned()
            .collect();
        if !hits.is_empty() {
            enabled = false;
            for hit in hits {
                matched.push(format!("-{hit}"));
            }
        }
    }
    (enabled, matched)
}

// ── Inventory ─────────────────────────────────────────────────────────

fn precedence_rank(scope: &str, source: &str) -> u8 {
    let scope_base = if scope == "project" { 0u8 } else { 2 };
    scope_base + if source == "local" { 0 } else { 1 }
}

fn resolve_local(input: &str, base_dir: &Path) -> String {
    let expanded = if input == "~" {
        std::env::temp_dir()
            .join("__home__")
            .to_string_lossy()
            .into_owned()
    } else {
        input.to_string()
    };
    let path = Path::new(&expanded);
    if path.is_absolute() {
        path.to_string_lossy().into_owned()
    } else {
        base_dir.join(path).to_string_lossy().into_owned()
    }
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(current) = dir {
        if current.join(".git").exists() {
            return Some(current);
        }
        dir = current.parent().map(Path::to_path_buf);
    }
    None
}

fn collect_ancestor_agents_skill_dirs(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let git_root = find_git_root(cwd);
    let mut dir = Some(cwd.to_path_buf());
    while let Some(current) = dir {
        dirs.push(current.join(".agents").join("skills"));
        if let Some(git) = &git_root {
            if &current == git {
                break;
            }
        }
        dir = current.parent().map(Path::to_path_buf);
    }
    let _ = home;
    dirs
}

pub struct SkillInventoryOptions<'a> {
    pub scope: &'a str,
    pub cwd: &'a Path,
    pub agent_dir: &'a Path,
    pub home_dir: &'a Path,
    pub project_trusted: bool,
}

pub fn build_skill_inventory(opts: &SkillInventoryOptions) -> Value {
    let mut diagnostics: Vec<Value> = Vec::new();
    let global_settings_path = opts.agent_dir.join("settings.json");
    let project_settings_path = opts.cwd.join(CONFIG_DIR_NAME).join("settings.json");
    let (global_skills, global_error) = read_settings_skills(&global_settings_path);
    if let Some(message) = global_error {
        diagnostics.push(json!({
            "path": global_settings_path.to_string_lossy(),
            "message": message,
        }));
    }
    let (project_skills, project_error) = if opts.project_trusted {
        read_settings_skills(&project_settings_path)
    } else {
        (Vec::new(), None)
    };
    if let Some(message) = project_error {
        diagnostics.push(json!({
            "path": project_settings_path.to_string_lossy(),
            "message": message,
        }));
    }
    let scope_skills: &[String] = if opts.scope == "global" {
        &global_skills
    } else {
        &project_skills
    };
    let custom_rules: Vec<String> = scope_skills
        .iter()
        .filter(|entry| is_plain_glob(entry))
        .map(|entry| to_posix(entry))
        .collect();

    // Roots, in Pi's discovery order.
    let mut roots: Vec<DiscoveredRoot> = vec![
        DiscoveredRoot {
            dir: opts.agent_dir.join("skills").to_string_lossy().into_owned(),
            mode: "pi",
            base_dir: opts.agent_dir.to_string_lossy().into_owned(),
            scope: "user",
            source: "auto",
        },
        DiscoveredRoot {
            dir: opts
                .home_dir
                .join(".agents")
                .join("skills")
                .to_string_lossy()
                .into_owned(),
            mode: "agents",
            base_dir: opts.home_dir.join(".agents").to_string_lossy().into_owned(),
            scope: "user",
            source: "auto",
        },
    ];
    for entry in &global_skills {
        if is_override(entry) || is_plain_glob(entry) {
            continue;
        }
        roots.push(DiscoveredRoot {
            dir: resolve_local(entry, opts.agent_dir),
            mode: "pi",
            base_dir: opts.agent_dir.to_string_lossy().into_owned(),
            scope: "user",
            source: "local",
        });
    }
    if opts.project_trusted {
        let project_base = opts.cwd.join(CONFIG_DIR_NAME);
        roots.push(DiscoveredRoot {
            dir: project_base.join("skills").to_string_lossy().into_owned(),
            mode: "pi",
            base_dir: project_base.to_string_lossy().into_owned(),
            scope: "project",
            source: "auto",
        });
        for agents_dir in collect_ancestor_agents_skill_dirs(opts.cwd, opts.home_dir) {
            if agents_dir == opts.home_dir.join(".agents").join("skills") {
                continue;
            }
            roots.push(DiscoveredRoot {
                dir: agents_dir.to_string_lossy().into_owned(),
                mode: "agents",
                base_dir: agents_dir
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                scope: "project",
                source: "auto",
            });
        }
        for entry in &project_skills {
            if is_override(entry) || is_plain_glob(entry) {
                continue;
            }
            roots.push(DiscoveredRoot {
                dir: resolve_local(entry, &project_base),
                mode: "pi",
                base_dir: project_base.to_string_lossy().into_owned(),
                scope: "project",
                source: "local",
            });
        }
    }

    struct Resolved {
        raw: RawSkill,
        root: DiscoveredRoot,
        enabled: bool,
        matching: Vec<String>,
    }
    let mut resolved: Vec<Resolved> = Vec::new();
    for root in &roots {
        for raw in discover_from_root(root, &mut diagnostics) {
            let ctx = build_match_context(&raw.file_path, &root.base_dir);
            let scope_skills: &[String] = if root.scope == "user" {
                &global_skills
            } else {
                &project_skills
            };
            let (enabled, matching) = is_enabled_by_overrides(&ctx, scope_skills);
            resolved.push(Resolved {
                raw,
                root: root.clone(),
                enabled,
                matching,
            });
        }
    }
    resolved.sort_by_key(|entry| precedence_rank(entry.root.scope, entry.root.source));

    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut items: Vec<Value> = Vec::new();
    let mut name_winner: HashMap<String, usize> = HashMap::new();
    for entry in &resolved {
        if !seen_paths.insert(entry.raw.canonical_path.clone()) {
            continue;
        }
        let skill_dir_string = raw_skill_dir(&entry.raw);
        let skill_dir = Path::new(&skill_dir_string);
        // A configured single-file skill points its rule at the file itself;
        // directory skills point at the skill dir (matches the TS pipeline).
        let rule_target_path = if entry.raw.is_configured_file {
            Path::new(&entry.raw.file_path)
        } else {
            skill_dir
        };
        let rule_relative_dir = to_posix(
            rule_target_path
                .strip_prefix(&entry.root.base_dir)
                .unwrap_or(rule_target_path)
                .to_string_lossy()
                .as_ref(),
        );
        // treePath is relative to the sourceRoot (root.dir), not Pi's base —
        // it decides the skill's place in the rendered tree.
        let root_dir_path = Path::new(&entry.root.dir);
        let tree_path = to_posix(
            skill_dir
                .strip_prefix(root_dir_path)
                .unwrap_or(skill_dir)
                .to_string_lossy()
                .as_ref(),
        );
        let item = json!({
            "kind": "skill",
            "id": entry.raw.canonical_path,
            "canonicalPath": entry.raw.canonical_path,
            "name": entry.raw.name,
            "description": entry.raw.description,
            "enabled": entry.enabled,
            "status": if entry.enabled { "enabled" } else { "disabled" },
            "ruleBaseDir": entry.root.base_dir,
            "ruleRelativeDir": rule_relative_dir,
            "treePath": tree_path,
            "sourceRoot": entry.root.dir,
            "scope": entry.root.scope,
            "source": entry.root.source,
            "matchingRules": entry.matching,
            "ambiguous": false,
        });
        items.push(item);
        if entry.enabled {
            name_winner
                .entry(entry.raw.name.clone())
                .or_insert(items.len() - 1);
        }
    }
    // Mark enabled losers shadowed by their higher-precedence winner.
    for index in 0..items.len() {
        if items[index]["status"] != "enabled" {
            continue;
        }
        let name = items[index]["name"].as_str().unwrap_or("").to_string();
        if let Some(&winner_index) = name_winner.get(&name) {
            if winner_index != index {
                items[index]["status"] = Value::String("shadowed".into());
                items[index]["shadowedBy"] = items[winner_index].clone();
            }
        }
    }
    // Cross-root ambiguity per (scope, ruleRelativeDir).
    let mut dir_to_roots: HashMap<String, HashSet<String>> = HashMap::new();
    for item in &items {
        let key = format!(
            "{}::{}",
            item["scope"].as_str().unwrap_or(""),
            item["ruleRelativeDir"].as_str().unwrap_or("")
        );
        dir_to_roots
            .entry(key)
            .or_default()
            .insert(item["sourceRoot"].as_str().unwrap_or("").to_string());
    }
    for item in &mut items {
        let key = format!(
            "{}::{}",
            item["scope"].as_str().unwrap_or(""),
            item["ruleRelativeDir"].as_str().unwrap_or("")
        );
        if dir_to_roots.get(&key).map(|set| set.len()).unwrap_or(0) > 1 {
            item["ambiguous"] = Value::Bool(true);
        }
    }

    // One tree per sourceRoot; intermediate treePath segments become groups.
    let mut items_by_root: HashMap<String, Vec<Value>> = HashMap::new();
    for item in &items {
        items_by_root
            .entry(item["sourceRoot"].as_str().unwrap_or("").to_string())
            .or_default()
            .push(item.clone());
    }
    let mut roots_out: Vec<Value> = Vec::new();
    for (source_root, root_items) in items_by_root {
        let root_dir = Path::new(&source_root);
        let base_dir = root_items
            .first()
            .and_then(|item| item["ruleBaseDir"].as_str())
            .unwrap_or("")
            .to_string();
        let scope = root_items
            .first()
            .and_then(|item| item["scope"].as_str())
            .unwrap_or("user");
        let source = root_items
            .first()
            .and_then(|item| item["source"].as_str())
            .unwrap_or("auto");
        let mut children: Vec<Value> = Vec::new();
        for item in &root_items {
            let tree_path = item["treePath"].as_str().unwrap_or("");
            let segments: Vec<&str> = tree_path
                .split('/')
                .filter(|segment| !segment.is_empty())
                .collect();
            insert_into_tree(
                &mut children,
                item,
                &segments,
                root_dir,
                &base_dir,
                scope,
                source,
            );
        }
        annotate_tree(&mut children);
        sort_tree(&mut children);
        roots_out.push(json!({
            "sourceRoot": source_root,
            "ruleBaseDir": base_dir,
            "scope": scope,
            "source": source,
            "rootKind": "pi",
            "children": children,
        }));
    }

    json!({
        "scope": opts.scope,
        "settingsPath": if opts.scope == "global" {
            global_settings_path.to_string_lossy()
        } else {
            project_settings_path.to_string_lossy()
        },
        "trusted": opts.project_trusted,
        "roots": roots_out,
        "customRules": custom_rules,
        "discoveredRoots": roots.iter().map(|r| r.dir.clone()).collect::<Vec<_>>(),
        "diagnostics": diagnostics,
    })
}

fn raw_skill_dir(raw: &RawSkill) -> String {
    raw.skill_dir.clone()
}

#[allow(clippy::too_many_arguments)]
fn insert_into_tree(
    children: &mut Vec<Value>,
    item: &Value,
    segments: &[&str],
    root_dir: &Path,
    base_dir: &str,
    scope: &str,
    source: &str,
) {
    if segments.is_empty() {
        children.push(item.clone());
        return;
    }
    let mut node_children = children;
    for (index, segment) in segments.iter().enumerate() {
        if index == segments.len() - 1 {
            node_children.push(item.clone());
            return;
        }
        let existing_index = node_children
            .iter()
            .position(|child| child["kind"] == "group" && child["name"] == *segment);
        if let Some(existing_index) = existing_index {
            node_children = match node_children[existing_index]["children"].as_array_mut() {
                Some(array) => array,
                None => return,
            };
        } else {
            let group_tree_path = segments[..=index].join("/");
            let group_dir =
                root_dir.join(group_tree_path.replace('/', std::path::MAIN_SEPARATOR_STR));
            let rule_base_relative = to_posix(
                group_dir
                    .strip_prefix(base_dir)
                    .unwrap_or(&group_dir)
                    .to_string_lossy()
                    .as_ref(),
            );
            let group = json!({
                "kind": "group",
                "id": format!("{}::{group_tree_path}", root_dir.to_string_lossy()),
                "sourceRoot": root_dir.to_string_lossy(),
                "ruleBaseDir": base_dir,
                "ruleBaseRelativePath": rule_base_relative,
                "name": segment,
                "scope": scope,
                "source": source,
                "state": "all-off",
                "ambiguous": false,
                "children": [],
            });
            node_children.push(group);
            let last = node_children.last_mut().expect("just pushed");
            node_children = match last["children"].as_array_mut() {
                Some(array) => array,
                None => return,
            };
        }
    }
}

fn collect_leaves(node: &Value, out: &mut Vec<Value>) {
    for child in node["children"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if child["kind"] == "skill" {
            out.push(child.clone());
        } else {
            collect_leaves(child, out);
        }
    }
}

fn annotate_tree(children: &mut [Value]) {
    for child in children.iter_mut() {
        if child["kind"] == "group" {
            annotate_tree(child["children"].as_array_mut().unwrap_or(&mut Vec::new()));
            let mut leaves = Vec::new();
            collect_leaves(child, &mut leaves);
            let enabled = leaves
                .iter()
                .filter(|leaf| leaf["status"] == "enabled")
                .count();
            child["state"] = if leaves.is_empty() || enabled == 0 {
                Value::String("all-off".into())
            } else if enabled == leaves.len() {
                Value::String("all-on".into())
            } else {
                Value::String("mixed".into())
            };
        }
    }
}

fn sort_tree(children: &mut [Value]) {
    children.sort_by(|a, b| {
        let a_skill = a["kind"] == "skill";
        let b_skill = b["kind"] == "skill";
        if a_skill != b_skill {
            return if a_skill {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        let a_name = a["name"].as_str().unwrap_or("");
        let b_name = b["name"].as_str().unwrap_or("");
        a_name.cmp(b_name)
    });
    for child in children.iter_mut() {
        if child["kind"] == "group" {
            if let Some(array) = child["children"].as_array_mut() {
                sort_tree(array);
            }
        }
    }
}

// ── Mutation: `set_skill_enabled` ─────────────────────────────────────

/// Apply one enable/disable mutation to the scope's settings.json, returning
/// the refreshed inventory. Mirrors the minimal-mutation policy: managed exact
/// `+`/`-` rules and group `!` rules are added/removed idempotently.
pub fn set_skill_enabled(
    opts: &SkillInventoryOptions,
    target_kind: &str,
    target_id: &str,
    enabled: bool,
) -> Result<Value, String> {
    if opts.scope == "project" && !opts.project_trusted {
        return Err("Project is not trusted; cannot mutate project skills".into());
    }
    let settings_path = if opts.scope == "global" {
        opts.agent_dir.join("settings.json")
    } else {
        opts.cwd.join(CONFIG_DIR_NAME).join("settings.json")
    };
    let inventory = build_skill_inventory(opts);
    let mut skills = {
        let text = std::fs::read_to_string(&settings_path).unwrap_or_default();
        let parsed: Value = serde_json::from_str(&text).unwrap_or(json!({}));
        parsed
            .get("skills")
            .and_then(Value::as_array)
            .map(|array| {
                array
                    .iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    let find_skill =
        |roots: &Value, id: &str| -> Option<Value> { find_kind_in_roots(roots, id, "skill") };
    let find_group =
        |roots: &Value, id: &str| -> Option<Value> { find_kind_in_roots(roots, id, "group") };

    if target_kind == "skill" {
        let item = find_skill(&inventory["roots"], target_id)
            .ok_or_else(|| "Unknown skill target".to_string())?;
        if item["scope"] != expected_scope(opts.scope) {
            return Err("Skill does not belong to the selected scope".into());
        }
        let rule = item["ruleRelativeDir"].as_str().unwrap_or("").to_string();
        if !enabled {
            ensure_override_present(&mut skills, "-", &rule);
        } else {
            remove_exact_prefix(&mut skills, &rule, &["-"]);
            let base_dir = item["ruleBaseDir"].as_str().unwrap_or("");
            let ctx = build_match_context_from_rule(
                item["ruleRelativeDir"].as_str().unwrap_or(""),
                base_dir,
            );
            let (excl, _, _) = overrides_of(&skills);
            if excl
                .iter()
                .any(|pattern| matches_any_pattern(&ctx, std::slice::from_ref(pattern)))
            {
                ensure_override_present(&mut skills, "+", &rule);
            }
        }
    } else {
        let group = find_group(&inventory["roots"], target_id)
            .ok_or_else(|| "Unknown group target".to_string())?;
        if group["scope"] != expected_scope(opts.scope) {
            return Err("Group does not belong to the selected scope".into());
        }
        let group_rule = group["ruleBaseRelativePath"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let mut members = Vec::new();
        collect_leaves(&group, &mut members);
        let member_rules: HashSet<String> = members
            .iter()
            .map(|leaf| leaf["ruleRelativeDir"].as_str().unwrap_or("").to_string())
            .collect();
        if !enabled {
            skills.retain(|entry| {
                !(entry.starts_with('+')
                    && member_rules.contains(&normalize_exact_pattern(&entry[1..])))
            });
            ensure_override_present(&mut skills, "!", &format!("{group_rule}/**"));
        } else {
            skills.retain(|entry| {
                !(entry.starts_with('!')
                    && normalize_exact_pattern(&entry[1..]) == format!("{group_rule}/**"))
            });
            let (excl, fexc, _) = overrides_of(&skills);
            for member in &members {
                let base_dir = member["ruleBaseDir"].as_str().unwrap_or("");
                let ctx = build_match_context_from_rule(
                    member["ruleRelativeDir"].as_str().unwrap_or(""),
                    base_dir,
                );
                if matches_any_exact(&ctx, &fexc) {
                    continue;
                }
                if excl
                    .iter()
                    .any(|pattern| matches_any_pattern(&ctx, std::slice::from_ref(pattern)))
                {
                    let rule = member["ruleRelativeDir"].as_str().unwrap_or("").to_string();
                    ensure_override_present(&mut skills, "+", &rule);
                }
            }
        }
    }

    write_settings_atomically(&settings_path, &skills)?;
    let refreshed = build_skill_inventory(opts);
    Ok(json!({ "inventory": refreshed, "runtimeRestartRequired": true }))
}

fn expected_scope(scope: &str) -> &'static str {
    if scope == "global" {
        "user"
    } else {
        "project"
    }
}

fn find_kind_in_roots(roots: &Value, id: &str, kind: &str) -> Option<Value> {
    for root in roots.as_array()? {
        if let Some(found) = find_kind_in_node(root, id, kind) {
            return Some(found);
        }
    }
    None
}

fn find_kind_in_node(node: &Value, id: &str, kind: &str) -> Option<Value> {
    for child in node["children"].as_array()?.iter() {
        if child["kind"] == kind && child["id"] == id {
            return Some(child.clone());
        }
        if child["kind"] == "group" {
            if let Some(found) = find_kind_in_node(child, id, kind) {
                return Some(found);
            }
        }
    }
    None
}

fn build_match_context_from_rule(rule_relative_dir: &str, base_dir: &str) -> MatchContext {
    let rel = format!("{rule_relative_dir}/SKILL.md");
    let abs = to_posix(&format!("{base_dir}/{rel}"));
    let parent_rel = rule_relative_dir.to_string();
    let parent_abs = to_posix(&format!("{base_dir}/{rule_relative_dir}"));
    MatchContext {
        parent_name: Path::new(rule_relative_dir)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        name: "SKILL.md".into(),
        rel,
        abs,
        parent_rel,
        parent_abs,
    }
}

fn ensure_override_present(arr: &mut Vec<String>, prefix: &str, body: &str) {
    let entry = format!("{prefix}{body}");
    if !arr.contains(&entry) {
        arr.push(entry);
    }
}

fn remove_exact_prefix(arr: &mut Vec<String>, rule: &str, prefixes: &[&str]) {
    arr.retain(|entry| {
        !prefixes.iter().any(|prefix| {
            entry.starts_with(prefix) && normalize_exact_pattern(&entry[prefix.len()..]) == rule
        })
    });
}

fn write_settings_atomically(settings_path: &Path, skills: &[String]) -> Result<(), String> {
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create settings dir: {error}"))?;
    }
    let mut parsed: Map<String, Value> = std::fs::read_to_string(settings_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    parsed.insert(
        "skills".into(),
        Value::Array(
            skills
                .iter()
                .map(|skill| Value::String(skill.clone()))
                .collect(),
        ),
    );
    let tmp = settings_path.with_extension("picot-tmp");
    {
        let mut file = std::fs::File::create(&tmp)
            .map_err(|error| format!("Cannot write settings: {error}"))?;
        file.write_all(
            serde_json::to_string_pretty(&json!(parsed))
                .unwrap_or_default()
                .as_bytes(),
        )
        .map_err(|error| format!("Cannot write settings: {error}"))?;
    }
    std::fs::rename(&tmp, settings_path)
        .map_err(|error| format!("Cannot replace settings: {error}"))
}

// ── Package skill inventory (candidates only; resolution is Pi-side) ──

/// Scan installed Pi packages for bundled skill directories. Each installed
/// package under `<agentDir>/npm/node_modules/<package>` with a `skills/`
/// subtree becomes one card listing its skill candidates.
pub fn build_package_skill_inventory(opts: &SkillInventoryOptions) -> Value {
    let mut diagnostics: Vec<Value> = Vec::new();
    let mut packages: Vec<Value> = Vec::new();
    let node_modules = opts.agent_dir.join("npm").join("node_modules");
    let Ok(scopes) = std::fs::read_dir(&node_modules) else {
        return json!({ "scope": opts.scope, "trusted": opts.project_trusted, "packages": [], "diagnostics": [] });
    };
    for scope_entry in scopes.filter_map(|entry| entry.ok()) {
        let scope_path = scope_entry.path();
        let Ok(entries) = std::fs::read_dir(&scope_path) else {
            // Unscoped package: the scope dir itself may be the package.
            if scope_path.join("skills").is_dir() {
                packages.push(package_card(
                    &scope_entry.file_name().to_string_lossy(),
                    &scope_path,
                    opts,
                    &mut diagnostics,
                ));
            }
            continue;
        };
        for package_entry in entries.filter_map(|entry| entry.ok()) {
            let package_path = package_entry.path();
            if !package_path.is_dir() {
                continue;
            }
            let package_name = format!(
                "{}/{}",
                scope_entry.file_name().to_string_lossy(),
                package_entry.file_name().to_string_lossy()
            );
            packages.push(package_card(
                &package_name,
                &package_path,
                opts,
                &mut diagnostics,
            ));
        }
    }
    json!({
        "scope": opts.scope,
        "trusted": opts.project_trusted,
        "packages": packages,
        "diagnostics": diagnostics,
    })
}

fn package_card(
    package_name: &str,
    package_path: &Path,
    opts: &SkillInventoryOptions,
    diagnostics: &mut Vec<Value>,
) -> Value {
    let mut candidates: Vec<Value> = Vec::new();
    let skills_root = package_path.join("skills");
    if skills_root.is_dir() {
        let root = DiscoveredRoot {
            dir: skills_root.to_string_lossy().into_owned(),
            mode: "pi",
            base_dir: package_path.to_string_lossy().into_owned(),
            scope: "user",
            source: "auto",
        };
        for raw in discover_from_root(&root, diagnostics) {
            candidates.push(json!({
                "id": format!("{package_name}/{}", raw.name),
                "canonicalPath": raw.canonical_path,
                "relativePath": to_posix(
                    Path::new(&raw.file_path)
                        .strip_prefix(package_path)
                        .unwrap_or(Path::new(&raw.file_path))
                        .to_string_lossy()
                        .as_ref(),
                ),
                "name": raw.name,
                "description": raw.description,
            }));
        }
    }
    json!({
        "id": format!("{package_name}/skills"),
        "source": package_name,
        "identity": package_name,
        "scope": opts.scope,
        "effectivePackageRoot": package_path.to_string_lossy(),
        "candidates": candidates,
        "diagnostics": [],
    })
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_unfolds_block_scalar_descriptions() {
        // Folded (>) with strip chomping: lines join with spaces.
        let folded = parse_frontmatter(
            "---\nname: tty7\ndescription: >-\n  Drive the tty7 terminal\n  workbench from the shell.\n---\nbody",
        );
        assert_eq!(folded.0.as_deref(), Some("tty7"));
        assert_eq!(
            folded.1.as_deref(),
            Some("Drive the tty7 terminal workbench from the shell."),
        );

        // Literal (|): line breaks are kept.
        let literal = parse_frontmatter(
            "---\nname: humanizer-zh\ndescription: |\n  去除 AI 痕迹。\n  综合指南。\n---\nbody",
        );
        assert_eq!(literal.1.as_deref(), Some("去除 AI 痕迹。\n综合指南。"),);

        // Flat one-line value still works (quotes stripped as before).
        let flat = parse_frontmatter("---\nname: x\ndescription: \"one line\"\n---\nbody");
        assert_eq!(flat.1.as_deref(), Some("one line"));

        // A following non-indented key stops the block scalar.
        let bounded = parse_frontmatter(
            "---\nname: y\ndescription: >\n  folded text\nallowed-tools: Read\n---\nbody",
        );
        assert_eq!(bounded.1.as_deref(), Some("folded text"));
    }

    fn write_skill(dir: &Path, name: &str, description: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\nbody"),
        )
        .unwrap();
    }

    fn opts<'a>(
        scope: &'a str,
        cwd: &'a Path,
        agent_dir: &'a Path,
        home_dir: &'a Path,
        trusted: bool,
    ) -> SkillInventoryOptions<'a> {
        SkillInventoryOptions {
            scope,
            cwd,
            agent_dir,
            home_dir,
            project_trusted: trusted,
        }
    }

    #[test]
    fn discovers_user_skills_and_reports_enabled_by_default() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path().join("agent");
        let home = temp.path().join("home");
        write_skill(&agent_dir.join("skills").join("alpha"), "alpha", "First");
        let inventory =
            build_skill_inventory(&opts("global", temp.path(), &agent_dir, &home, false));
        let roots = inventory["roots"].as_array().unwrap();
        let skills: Vec<&Value> = roots
            .iter()
            .flat_map(|root| root["children"].as_array().unwrap())
            .filter(|child| child["kind"] == "skill")
            .collect();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0]["name"], "alpha");
        assert_eq!(skills[0]["status"], "enabled");
    }

    #[test]
    fn dash_rule_disables_and_plus_rule_re_enables() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path().join("agent");
        let home = temp.path().join("home");
        write_skill(&agent_dir.join("skills").join("bravo"), "bravo", "Second");
        let settings = agent_dir.join("settings.json");
        std::fs::write(&settings, r#"{"skills": ["-skills/bravo"]}"#).unwrap();
        let context = opts("global", temp.path(), &agent_dir, &home, false);
        let inventory = build_skill_inventory(&context);
        let skills: Vec<Value> = inventory["roots"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|root| root["children"].as_array().unwrap().clone())
            .filter(|child| child["kind"] == "skill")
            .collect();
        assert_eq!(skills[0]["status"], "disabled");

        // Re-enable via set_skill_enabled: removes the managed `-` rule.
        let result =
            set_skill_enabled(&context, "skill", skills[0]["id"].as_str().unwrap(), true).unwrap();
        let refreshed = result["inventory"].as_object().unwrap();
        assert!(!refreshed.is_empty());
        let skills: Vec<&Value> = refreshed["roots"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|root| root["children"].as_array().unwrap())
            .filter(|child| child["kind"] == "skill")
            .collect();
        assert_eq!(skills[0]["status"], "enabled");
    }

    #[test]
    fn nested_dirs_become_groups_with_state() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path().join("agent");
        let home = temp.path().join("home");
        write_skill(
            &agent_dir.join("skills").join("group-a").join("one"),
            "one",
            "A",
        );
        write_skill(
            &agent_dir.join("skills").join("group-a").join("two"),
            "two",
            "B",
        );
        let inventory =
            build_skill_inventory(&opts("global", temp.path(), &agent_dir, &home, false));
        let roots = inventory["roots"].as_array().unwrap();
        let groups: Vec<&Value> = roots
            .iter()
            .flat_map(|root| root["children"].as_array().unwrap())
            .filter(|child| child["kind"] == "group")
            .collect();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["name"], "group-a");
        assert_eq!(groups[0]["state"], "all-on");
        assert_eq!(groups[0]["children"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn project_skills_require_trust() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path().join("agent");
        let home = temp.path().join("home");
        let cwd = temp.path().join("project");
        write_skill(
            &cwd.join(".pi").join("skills").join("proj"),
            "proj",
            "Project",
        );
        let untrusted = build_skill_inventory(&opts("project", &cwd, &agent_dir, &home, false));
        assert_eq!(untrusted["trusted"], false);
        let trusted = build_skill_inventory(&opts("project", &cwd, &agent_dir, &home, true));
        let skills: Vec<&Value> = trusted["roots"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|root| root["children"].as_array().unwrap())
            .filter(|child| child["kind"] == "skill")
            .collect();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0]["scope"], "project");
    }

    #[test]
    fn glob_match_supports_doublestar_and_question() {
        assert!(glob_match("skills/**", "skills/alpha/SKILL.md"));
        assert!(glob_match("skills/*", "skills/alpha"));
        assert!(!glob_match("skills/*", "skills/alpha/SKILL.md"));
        assert!(glob_match("sk?lls", "skills"));
    }

    #[test]
    fn package_inventory_lists_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path().join("agent");
        let home = temp.path().join("home");
        write_skill(
            &agent_dir
                .join("npm")
                .join("node_modules")
                .join("@scope")
                .join("pkg")
                .join("skills")
                .join("demo"),
            "demo",
            "Packaged",
        );
        let inventory =
            build_package_skill_inventory(&opts("global", temp.path(), &agent_dir, &home, false));
        let packages = inventory["packages"].as_array().unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0]["source"], "@scope/pkg");
        let candidates = packages[0]["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["name"], "demo");
    }
}
