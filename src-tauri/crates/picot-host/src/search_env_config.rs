// ABOUTME: Host control ops for the global search-extension .env file
// ABOUTME: (~/.pi/agent/.env) shared by the datarx-essential brave-search and
// ABOUTME: tavily search extensions. Uses a comment-preserving upsert,
// ABOUTME: whole-line delete and masked reads mirroring the extension's
// ABOUTME: env-utils semantics; the API key is never returned in clear text.
// ABOUTME: Every env var name and count bound comes from a SearchEnvSpec so the
// ABOUTME: two extensions share one implementation.
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::host_config;
use crate::pi_launch;

/// Per-extension env contract: the variable names the extension reads (never
/// inlined in the logic) plus the accepted result-count range.
pub struct SearchEnvSpec {
    pub key_env: &'static str,
    pub count_env: &'static str,
    pub count_min: i64,
    pub count_max: i64,
}

/// datarx-essential brave-search extension.
pub const BRAVE_SPEC: SearchEnvSpec = SearchEnvSpec {
    key_env: "BRAVE_SEARCH_API_KEY",
    count_env: "BRAVE_SEARCH_RESULT_COUNT",
    count_min: 1,
    count_max: 20,
};

/// datarx-essential tavily-search extension (brave's twin; same bounds).
pub const TAVILY_SPEC: SearchEnvSpec = SearchEnvSpec {
    key_env: "TAVILY_API_KEY",
    count_env: "TAVILY_RESULT_COUNT",
    count_min: 1,
    count_max: 20,
};

/// The global tier lives next to the Pi agent state (PI_CODING_AGENT_DIR aware).
fn agent_root_env_path() -> Result<PathBuf, String> {
    Ok(pi_launch::resolve_pi_agent_root()?.join(".env"))
}

/// Public get for the brave-search extension.
pub fn get_brave_config(payload: &Value) -> Result<Value, String> {
    get_config(payload, &BRAVE_SPEC)
}

/// Public set for the brave-search extension.
pub fn set_brave_config(payload: &Value) -> Result<Value, String> {
    set_config(payload, &BRAVE_SPEC)
}

/// Public get for the tavily-search extension.
pub fn get_tavily_config(payload: &Value) -> Result<Value, String> {
    get_config(payload, &TAVILY_SPEC)
}

/// Public set for the tavily-search extension.
pub fn set_tavily_config(payload: &Value) -> Result<Value, String> {
    set_config(payload, &TAVILY_SPEC)
}

/// Resolve the real global path, then render. The payload is accepted for a
/// uniform dispatch signature but carries nothing (the setting is global-only).
fn get_config(_payload: &Value, spec: &SearchEnvSpec) -> Result<Value, String> {
    let global = agent_root_env_path()?;
    Ok(get_config_at(&global, spec))
}

/// Resolve the real global path, apply the payload, then re-read.
fn set_config(payload: &Value, spec: &SearchEnvSpec) -> Result<Value, String> {
    let global = agent_root_env_path()?;
    set_config_at(&global, payload, spec)
}

/// Render the shared get payload from an explicit path (test-injectable).
fn get_config_at(global: &Path, spec: &SearchEnvSpec) -> Value {
    let global_value = read_key(global, spec.key_env);
    let default_count =
        read_key(global, spec.count_env).and_then(|raw| parse_result_count(&raw, spec));
    json!({
        "ok": true,
        "globalPath": global.to_string_lossy(),
        "globalKeyMasked": masked(&global_value),
        "defaultCount": default_count,
    })
}

/// Apply only the fields present in `payload` to the global file, then re-read.
/// Absent fields and JSON `null` mean "leave unchanged"; an empty apiKey string
/// clears the line.
fn set_config_at(global: &Path, payload: &Value, spec: &SearchEnvSpec) -> Result<Value, String> {
    // A missing file is the empty document; any other read failure (non-UTF-8,
    // EACCES, IO) must abort rather than collapse to "" and truncate the file.
    let original = match std::fs::read_to_string(global) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(format!(
                "config_access_failed: cannot read {}: {error}",
                global.display()
            ))
        }
    };
    let mut content = original.clone();

    match payload.get("apiKey") {
        None | Some(Value::Null) => {}
        Some(Value::String(raw)) => {
            content = if raw.is_empty() {
                delete_key(&content, spec.key_env)
            } else {
                upsert_key(&content, spec.key_env, raw)
            };
        }
        Some(_) => return Err("apiKey must be a string or null".to_string()),
    }

    match payload.get("defaultCount") {
        None => {}
        Some(Value::Null) => {
            content = delete_key(&content, spec.count_env);
        }
        Some(Value::Number(number)) => {
            let count = number
                .as_i64()
                .filter(|n| (spec.count_min..=spec.count_max).contains(n))
                .ok_or_else(|| count_error(spec))?;
            content = upsert_key(&content, spec.count_env, &count.to_string());
        }
        Some(_) => return Err(count_error(spec)),
    }

    // Never create a file for a no-op delete of an absent key.
    if content != original {
        write_env(global, &content)?;
    }
    Ok(get_config_at(global, spec))
}

fn count_error(spec: &SearchEnvSpec) -> String {
    format!(
        "defaultCount must be an integer between {} and {}",
        spec.count_min, spec.count_max
    )
}

fn write_env(path: &Path, content: &str) -> Result<(), String> {
    host_config::write_text(path, content).map_err(config_error_string)
}

fn config_error_string(error: host_config::ConfigError) -> String {
    match &error {
        host_config::ConfigError::Io(detail) => format!("{}: {detail}", error.code()),
        other => other.code().to_string(),
    }
}

fn read_key(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| line_value(line, key))
}

fn parse_result_count(raw: &str, spec: &SearchEnvSpec) -> Option<i64> {
    raw.trim()
        .parse::<i64>()
        .ok()
        .filter(|count| (spec.count_min..=spec.count_max).contains(count))
}

/// `""`/short values can't leak; longer values keep only the last four chars.
fn mask_secret(value: &str) -> String {
    let count = value.chars().count();
    if count <= 4 {
        return "••••".to_string();
    }
    let tail: String = value.chars().skip(count - 4).collect();
    format!("••••{tail}")
}

fn masked(value: &Option<String>) -> Value {
    match value {
        Some(raw) if !raw.is_empty() => json!(mask_secret(raw)),
        _ => Value::Null,
    }
}

/// JSON-quoted value: `A="x"` with `"`/`\` escaped, matching env-utils.
fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

/// Replace the first matching line in place (keeping its ending), drop any
/// later duplicate of the same key, else append at EOF.
fn upsert_key(text: &str, key: &str, value: &str) -> String {
    let assignment = format!("{key}={}", json_string(value));
    let mut out = String::new();
    let mut replaced = false;
    for piece in text.split_inclusive('\n') {
        let has_newline = piece.ends_with('\n');
        let without_newline = if has_newline {
            &piece[..piece.len() - 1]
        } else {
            piece
        };
        let (stripped, had_cr) = match without_newline.strip_suffix('\r') {
            Some(body) => (body, true),
            None => (without_newline, false),
        };
        if line_matches_key(stripped, key) {
            if replaced {
                // The extension's loader is last-match-wins, so a surviving
                // duplicate would shadow the new value: drop it.
                continue;
            }
            out.push_str(&assignment);
            if had_cr {
                out.push('\r');
            }
            replaced = true;
        } else {
            out.push_str(without_newline);
        }
        if has_newline {
            out.push('\n');
        }
    }
    if !replaced {
        if !text.is_empty() && !text.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&assignment);
    }
    out
}

/// Remove every whole line for `key` (matching + trailing newline).
fn delete_key(text: &str, key: &str) -> String {
    let mut out = String::new();
    for piece in text.split_inclusive('\n') {
        let without_newline = piece.strip_suffix('\n').unwrap_or(piece);
        let stripped = without_newline
            .strip_suffix('\r')
            .unwrap_or(without_newline);
        if line_matches_key(stripped, key) {
            continue;
        }
        out.push_str(piece);
    }
    out
}

fn line_matches_key(line: &str, key: &str) -> bool {
    matches!(split_assignment(line), Some((name, _)) if name == key)
}

/// `NAME=value` after trimming leading space and an optional `export `.
fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let mut rest = line.trim_start();
    if let Some(after) = rest.strip_prefix("export") {
        if after.starts_with(|c: char| c.is_whitespace()) {
            rest = after.trim_start();
        }
    }
    let eq = rest.find('=')?;
    let name = rest[..eq].trim_end();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    Some((name, &rest[eq + 1..]))
}

fn line_value(line: &str, key: &str) -> Option<String> {
    let (name, raw) = split_assignment(line)?;
    (name == key).then(|| parse_env_value(raw))
}

/// Optional paired quotes, else unquoted with ` #` inline-comment truncation.
/// Double quotes are JSON-unescaped (upsert writes `serde_json` output);
/// single quotes are literal.
fn parse_env_value(raw: &str) -> String {
    let trimmed = raw.trim_start();
    match trimmed.as_bytes().first() {
        Some(b'"') => {
            read_double_quoted(&trimmed[1..]).unwrap_or_else(|| strip_inline_comment(trimmed))
        }
        Some(b'\'') => match trimmed[1..].find('\'') {
            Some(end) => trimmed[1..1 + end].to_string(),
            None => strip_inline_comment(trimmed),
        },
        _ => strip_inline_comment(trimmed),
    }
}

/// Content up to the first unescaped `"`, with JSON escapes resolved. `None`
/// when the closing quote is missing (caller falls back to the raw line).
fn read_double_quoted(inner: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            other => out.push(other),
        }
    }
    None
}

fn strip_inline_comment(value: &str) -> String {
    let mut result = String::new();
    let mut preceded_by_space = false;
    for c in value.chars() {
        if c == '#' && preceded_by_space {
            break;
        }
        result.push(c);
        preceded_by_space = c.is_whitespace();
    }
    result.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spec-dependent test runs once per extension so the two share one
    /// implementation and drift is caught immediately.
    const SPECS: [(&str, &SearchEnvSpec); 2] = [("brave", &BRAVE_SPEC), ("tavily", &TAVILY_SPEC)];

    /// A fresh temp global `.env` path (parent dir created).
    fn unique_env(tag: &str) -> (PathBuf, PathBuf) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("picot-search-{tag}-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let global = dir.join(".env");
        (dir, global)
    }

    #[test]
    fn upsert_appends_to_empty_and_existing_files() {
        assert_eq!(upsert_key("", "A", "1"), "A=\"1\"");
        assert_eq!(upsert_key("X=1\n", "A", "1"), "X=1\nA=\"1\"");
        assert_eq!(upsert_key("X=1", "A", "1"), "X=1\nA=\"1\"");
    }

    #[test]
    fn upsert_replaces_in_place_and_preserves_comments_and_other_lines() {
        let text = "# header\nA=\"old\"\nB=2 # keep\n";
        assert_eq!(
            upsert_key(text, "A", "new"),
            "# header\nA=\"new\"\nB=2 # keep\n"
        );
        // export-prefixed source line is replaced wholesale by the plain form.
        assert_eq!(upsert_key("export A=1\n", "A", "2"), "A=\"2\"\n");
    }

    #[test]
    fn upsert_json_escapes_quotes_and_backslashes() {
        assert_eq!(upsert_key("", "A", "a\"b\\c"), "A=\"a\\\"b\\\\c\"");
        assert_eq!(
            line_value("A=\"a\\\"b\\\\c\"", "A").as_deref(),
            Some("a\"b\\c")
        );
    }

    #[test]
    fn repeated_upserts_converge_to_a_single_line() {
        let once = upsert_key("", "A", "1");
        let twice = upsert_key(&once, "A", "2");
        assert_eq!(twice, "A=\"2\"");
        assert_eq!(twice.matches("A=").count(), 1);
    }

    #[test]
    fn reads_export_prefix_quotes_and_inline_comments() {
        assert_eq!(
            line_value("export A=\"hello world\"", "A").as_deref(),
            Some("hello world")
        );
        assert_eq!(line_value("A='single'", "A").as_deref(), Some("single"));
        assert_eq!(line_value("  A = bare ", "A").as_deref(), Some("bare"));
        assert_eq!(line_value("A=bare # note", "A").as_deref(), Some("bare"));
        assert_eq!(
            line_value("A=bare#notacomment", "A").as_deref(),
            Some("bare#notacomment")
        );
        assert_eq!(line_value("# A=x", "A"), None);
        assert_eq!(line_value("B=1", "A"), None);
    }

    #[test]
    fn delete_removes_whole_lines_and_is_silent_when_absent() {
        assert_eq!(delete_key("X=1\nA=2\nB=3\n", "A"), "X=1\nB=3\n");
        assert_eq!(delete_key("X=1\n", "A"), "X=1\n");
        assert_eq!(delete_key("A=2", "A"), "");
        assert_eq!(delete_key("export A=2\nB=1\n", "A"), "B=1\n");
    }

    #[cfg(unix)]
    #[test]
    fn writes_env_files_with_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = unique_env("perm");
        write_env(&path, "A=\"1\"\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn masks_secrets_and_never_leaks_clear_text() {
        assert_eq!(mask_secret(""), "••••");
        assert_eq!(mask_secret("ab"), "••••");
        assert_eq!(mask_secret("abcd"), "••••");
        assert_eq!(mask_secret("abcde"), "••••bcde");
        assert_eq!(mask_secret("0123456789"), "••••6789");
        assert_eq!(masked(&Some(String::new())), Value::Null);
        assert_eq!(masked(&None), Value::Null);
        assert_eq!(masked(&Some("secretvalue".into())), json!("••••alue"));
    }

    #[test]
    fn get_reports_the_global_path_and_masks_the_key() {
        for (tag, spec) in SPECS {
            let (dir, global) = unique_env(&format!("get-{tag}"));
            std::fs::write(
                &global,
                format!("{}=global-key-1234\n{}=7\n", spec.key_env, spec.count_env),
            )
            .unwrap();

            let value = get_config_at(&global, spec);
            assert_eq!(value["ok"], json!(true), "[{tag}]");
            assert_eq!(
                value["globalPath"],
                json!(global.to_string_lossy()),
                "[{tag}]"
            );
            assert_eq!(value["globalKeyMasked"], json!("••••1234"), "[{tag}]");
            assert_eq!(value["defaultCount"], json!(7), "[{tag}]");
            // The plaintext key never appears anywhere in the payload.
            assert!(!value.to_string().contains("global-key"), "[{tag}]");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn get_reports_null_when_unset_and_ignores_an_invalid_count() {
        for (tag, spec) in SPECS {
            let (dir, global) = unique_env(&format!("empty-{tag}"));
            // No file at all: nothing configured.
            let value = get_config_at(&global, spec);
            assert_eq!(value["globalKeyMasked"], Value::Null, "[{tag}]");
            assert_eq!(value["defaultCount"], Value::Null, "[{tag}]");

            // Out-of-range / non-numeric counts read as null.
            std::fs::write(&global, format!("{}=99\n", spec.count_env)).unwrap();
            assert_eq!(
                get_config_at(&global, spec)["defaultCount"],
                Value::Null,
                "[{tag}]"
            );
            std::fs::write(&global, format!("{}=abc\n", spec.count_env)).unwrap();
            assert_eq!(
                get_config_at(&global, spec)["defaultCount"],
                Value::Null,
                "[{tag}]"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn set_writes_and_clears_the_key_and_the_count() {
        for (tag, spec) in SPECS {
            let (dir, global) = unique_env(&format!("set-{tag}"));
            let written = set_config_at(
                &global,
                &json!({"apiKey": "abc12345", "defaultCount": 1}),
                spec,
            )
            .unwrap();
            assert_eq!(written["globalKeyMasked"], json!("••••2345"), "[{tag}]");
            assert_eq!(written["defaultCount"], json!(1), "[{tag}]");
            let text = std::fs::read_to_string(&global).unwrap();
            assert!(text.contains(spec.key_env), "[{tag}]");
            assert!(text.contains(spec.count_env), "[{tag}]");

            // boundary 20 accepted.
            set_config_at(&global, &json!({"defaultCount": 20}), spec).unwrap();
            assert_eq!(
                get_config_at(&global, spec)["defaultCount"],
                json!(20),
                "[{tag}]"
            );

            // Some("") clears the key line, leaves the count line intact.
            set_config_at(&global, &json!({"apiKey": ""}), spec).unwrap();
            assert_eq!(
                get_config_at(&global, spec)["globalKeyMasked"],
                Value::Null,
                "[{tag}]"
            );
            let text = std::fs::read_to_string(&global).unwrap();
            assert!(!text.contains(spec.key_env), "[{tag}]");
            assert!(text.contains(spec.count_env), "[{tag}]");

            // defaultCount null deletes the line.
            set_config_at(&global, &json!({"defaultCount": null}), spec).unwrap();
            assert!(
                !std::fs::read_to_string(&global)
                    .unwrap()
                    .contains(spec.count_env),
                "[{tag}]"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn set_validates_count_boundaries() {
        for (tag, spec) in SPECS {
            let (dir, global) = unique_env(&format!("count-bounds-{tag}"));
            for bad in [json!(0), json!(21), json!(5.5), json!("abc")] {
                assert!(
                    set_config_at(&global, &json!({"defaultCount": bad}), spec).is_err(),
                    "[{tag}] count {bad} must be rejected"
                );
            }
            // A rejected write leaves no file behind.
            assert!(!global.exists(), "[{tag}]");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn set_absent_or_null_fields_leave_the_file_untouched() {
        for (tag, spec) in SPECS {
            let (dir, global) = unique_env(&format!("noop-{tag}"));
            set_config_at(&global, &json!({"apiKey": "keep-me-123456"}), spec).unwrap();
            // Only defaultCount present: apiKey untouched.
            set_config_at(&global, &json!({"defaultCount": 3}), spec).unwrap();
            assert!(
                std::fs::read_to_string(&global)
                    .unwrap()
                    .contains("keep-me-123456"),
                "[{tag}]"
            );
            // apiKey null: still untouched.
            set_config_at(&global, &json!({"apiKey": null}), spec).unwrap();
            assert!(
                std::fs::read_to_string(&global)
                    .unwrap()
                    .contains("keep-me-123456"),
                "[{tag}]"
            );
            // Clearing an absent count on a missing file must not create it.
            let fresh = dir.join("never.env");
            set_config_at(&fresh, &json!({"defaultCount": null}), spec).unwrap();
            assert!(!fresh.exists(), "[{tag}]");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn count_parse_bounds() {
        for (tag, spec) in SPECS {
            assert_eq!(parse_result_count("1", spec), Some(1), "[{tag}]");
            assert_eq!(parse_result_count("20", spec), Some(20), "[{tag}]");
            assert_eq!(parse_result_count(" 5 ", spec), Some(5), "[{tag}]");
            assert_eq!(parse_result_count("0", spec), None, "[{tag}]");
            assert_eq!(parse_result_count("21", spec), None, "[{tag}]");
            assert_eq!(parse_result_count("abc", spec), None, "[{tag}]");
            assert_eq!(parse_result_count("", spec), None, "[{tag}]");
        }
    }

    /// A file that exists but cannot be decoded (non-UTF-8, EACCES, IO) must
    /// abort the set instead of collapsing to "" and truncating the file.
    #[test]
    fn set_aborts_when_the_existing_file_cannot_be_read() {
        for (tag, spec) in SPECS {
            let (dir, global) = unique_env(&format!("undecodable-{tag}"));
            let bytes = b"GOOD=1\nBAD=\xff\n".to_vec();
            std::fs::write(&global, &bytes).unwrap();
            let error = set_config_at(&global, &json!({"defaultCount": 3}), spec).unwrap_err();
            assert!(error.starts_with("config_access_failed"), "[{tag}] {error}");
            assert_eq!(std::fs::read(&global).unwrap(), bytes, "[{tag}]");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A genuinely missing file is the empty document: the first set creates it.
    #[test]
    fn set_creates_a_missing_file() {
        for (tag, spec) in SPECS {
            let (dir, global) = unique_env(&format!("create-{tag}"));
            assert!(!global.exists(), "[{tag}]");
            set_config_at(&global, &json!({"defaultCount": 3}), spec).unwrap();
            assert!(global.exists(), "[{tag}]");
            assert_eq!(
                get_config_at(&global, spec)["defaultCount"],
                json!(3),
                "[{tag}]"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A hand-written duplicate line would win under the extension's
    /// last-match-wins loader, so the upsert must leave exactly one line.
    #[test]
    fn upsert_collapses_duplicate_lines_keeping_order() {
        let text = "A=\"first\"\nB=2\nA=\"second\"\nC=3\n";
        let out = upsert_key(text, "A", "new");
        assert_eq!(out, "A=\"new\"\nB=2\nC=3\n");
        assert_eq!(out.matches("A=").count(), 1);
        // A trailing duplicate (no newline) is dropped too.
        assert_eq!(upsert_key("A=1\nB=2\nA=3", "A", "new"), "A=\"new\"\nB=2\n");
    }

    /// A replaced line keeps its original CRLF ending; untouched CRLF lines and
    /// LF lines keep theirs.
    #[test]
    fn upsert_preserves_the_replaced_line_ending() {
        assert_eq!(
            upsert_key("A=\"old\"\r\nB=2\r\n", "A", "new"),
            "A=\"new\"\r\nB=2\r\n"
        );
        assert_eq!(upsert_key("A=old\r\n", "A", "new"), "A=\"new\"\r\n");
        assert_eq!(upsert_key("A=old\n", "A", "new"), "A=\"new\"\n");
    }

    /// The two extensions never cross-contaminate: each writes and reads back
    /// its own variable names, and clearing one leaves the other's line alone.
    #[test]
    fn brave_and_tavily_write_and_read_their_own_env_names() {
        let (dir, global) = unique_env("twins");
        set_config_at(
            &global,
            &json!({"apiKey": "brave-key-1111", "defaultCount": 4}),
            &BRAVE_SPEC,
        )
        .unwrap();
        set_config_at(
            &global,
            &json!({"apiKey": "tavily-key-2222", "defaultCount": 8}),
            &TAVILY_SPEC,
        )
        .unwrap();

        let text = std::fs::read_to_string(&global).unwrap();
        assert!(text.contains("BRAVE_SEARCH_API_KEY"), "{text}");
        assert!(text.contains("TAVILY_API_KEY"), "{text}");
        assert!(text.contains("BRAVE_SEARCH_RESULT_COUNT"), "{text}");
        assert!(text.contains("TAVILY_RESULT_COUNT"), "{text}");

        let brave = get_config_at(&global, &BRAVE_SPEC);
        let tavily = get_config_at(&global, &TAVILY_SPEC);
        assert_eq!(brave["globalKeyMasked"], json!("••••1111"));
        assert_eq!(brave["defaultCount"], json!(4));
        assert_eq!(tavily["globalKeyMasked"], json!("••••2222"));
        assert_eq!(tavily["defaultCount"], json!(8));

        // Clearing the tavily key leaves the brave line intact.
        set_config_at(&global, &json!({"apiKey": ""}), &TAVILY_SPEC).unwrap();
        let text = std::fs::read_to_string(&global).unwrap();
        assert!(!text.contains("TAVILY_API_KEY"), "{text}");
        assert!(text.contains("BRAVE_SEARCH_API_KEY"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
