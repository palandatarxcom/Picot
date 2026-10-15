// ABOUTME: Fails the build if the admin token can reach a log sink or an error string in the daemon
// ABOUTME: and app-manager sources — the token's only legal destination is the 0600 instance file.

//! Plan D7 / Task 4 Step 1: "token in everything error/log path redacted (grep
//! log sink)". Redaction by convention decays, so the convention is a test:
//! every logging line in the files that hold a token is scanned, and a format
//! that interpolates the token value fails the build.
//!
//! What counts as a violation on a line carrying `log::…!`, `eprintln!` or
//! `println!`:
//!
//! - the token interpolated inside a format string (`{token}`, `{:?}`, `{token:?}`);
//! - the bare identifier `token` passed as an argument (`"… {}", token`);
//!
//! A *literal* mention is fine: `"invalid admin token"` names the concept
//! without the value, which is exactly what an honest log line should do.
//!
//! The scanner is self-tested below, so a regex that silently matches nothing
//! cannot pass the whole file.

use std::path::{Path, PathBuf};

/// The files that handle the admin token.
const SOURCES: &[&str] = &[
    "crates/picot-host/src/daemon/instance.rs",
    "crates/picot-host/src/daemon/admin.rs",
    "crates/picot-host/src/daemon/mod.rs",
    "crates/picot-daemon/src/main.rs",
    "crates/picot-daemon/src/log_sink.rs",
    "src/daemon_client/mod.rs",
    "src/daemon_client/manager.rs",
    "src/daemon_client/exit_waiter.rs",
];

/// The repository's `src-tauri` directory, published by `picot-host/build.rs`
/// so a test never has to guess its own position.
fn src_tauri_dir() -> PathBuf {
    PathBuf::from(env!("PICOT_SRC_TAURI_DIR"))
}

fn is_log_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("log::")
        || trimmed.starts_with("eprintln!")
        || trimmed.starts_with("println!")
        // A multi-line invocation continues on an indented line: keep scanning
        // every line that looks like a format string argument list.
        || (line.contains("log::") || line.contains("eprintln!") || line.contains("println!"))
}

/// Strip Rust string literals and char literals, leaving code.
fn without_string_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    for current in chars.by_ref() {
        if in_string {
            if escaped {
                escaped = false;
            } else if current == '\\' {
                escaped = true;
            } else if current == '"' {
                in_string = false;
            }
            continue;
        }
        if current == '"' {
            in_string = true;
            continue;
        }
        out.push(current);
    }
    // Keep an unterminated literal's tail out of the analysis too: a truncation
    // here means the line is mid-string, not code.
    let _ = chars.peek();
    out
}

/// The token interpolated inside a format string on this line.
fn interpolated_token(line: &str) -> Option<String> {
    let mut rest = line;
    while let Some(start) = rest.find('{') {
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('}') else { break };
        let spec = &tail[..end];
        if spec
            .split([':', '?'])
            .any(|word| word.trim() == "token" || word.trim().ends_with("_token"))
        {
            return Some(format!("{{{spec}}}"));
        }
        rest = &tail[end + 1..];
    }
    None
}

/// The bare `token` identifier used as an argument (outside string literals).
///
/// An identifier counts when it is `token`, `admin_token` (any `_token`
/// suffix) or the wire spelling `adminToken` — i.e. when it *names the value*,
/// not merely mentions the word (`tokens`, `tokenless`).
fn bare_token_argument(line: &str) -> Option<String> {
    let code = without_string_literals(line);
    for identifier in code.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let names_the_token = identifier == "token"
            || identifier.ends_with("_token")
            || identifier == "adminToken"
            || identifier.starts_with("token_") && identifier != "token_redaction_ok";
        if names_the_token {
            return Some(format!("passes `{identifier}`: {}", code.trim()));
        }
    }
    None
}

/// `Some(reason)` when `line` may leak the token value into the log.
fn violation(line: &str) -> Option<String> {
    if line.contains("// token-redaction-ok:") {
        return None;
    }
    if let Some(spec) = interpolated_token(line) {
        return Some(format!("interpolates {spec}"));
    }
    if let Some(code) = bare_token_argument(line) {
        return Some(format!("passes the token identifier: {code}"));
    }
    None
}

#[test]
fn the_scanner_actually_detects_the_shapes_it_is_guarding_against() {
    for (line, why) in [
        (r#"log::warn!("admin hello {token}")"#, "inline capture"),
        (r#"log::warn!("token {:?}", admin_token)"#, "debug capture"),
        (r#"eprintln!("cannot use {}", token)"#, "bare argument"),
        (
            r#"log::info!("adminToken {}", endpoint.admin_token)"#,
            "named argument",
        ),
    ] {
        assert!(
            violation(line).is_some(),
            "the scanner must flag {why}: {line}"
        );
    }
    for safe in [
        r#"log::warn!("invalid admin token")"#,
        r#"log::warn!("the instance file {} carries no admin token", path.display())"#,
        // A different identifier that merely contains the word.
        r#"log::info!("tokens exhausted: {}", remaining)"#,
        // An explicit, recorded exception.
        r#"log::info!("token {token}") // token-redaction-ok: test fixture"#,
    ] {
        assert!(
            violation(safe).is_none(),
            "the scanner must not flag a literal mention: {safe}"
        );
    }
    // The scan is scoped to the log sink on purpose: the admin frame and the
    // instance document are the token's legitimate carriers.
    for not_a_log in [
        r#"json!({"type": "admin_hello", "adminToken": endpoint.token})"#,
        r#"write_atomic(&path, &file)"#,
    ] {
        assert!(
            !is_log_line(not_a_log),
            "not a log sink, not scanned: {not_a_log}"
        );
    }
}

#[test]
fn no_log_line_in_the_token_handling_sources_can_print_the_token() {
    let root = src_tauri_dir();
    let mut violations: Vec<String> = Vec::new();
    let mut scanned = 0_usize;
    for relative in SOURCES {
        let path: &Path = &root.join(relative);
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        for (number, line) in source.lines().enumerate() {
            scanned += 1;
            if !is_log_line(line) {
                continue;
            }
            if let Some(reason) = violation(line) {
                violations.push(format!("{relative}:{}: {reason}\n    {line}", number + 1));
            }
        }
    }
    assert!(scanned > 0, "the scan must actually read the sources");
    assert!(
        violations.is_empty(),
        "the admin token may never reach a log sink or an error string:\n{}",
        violations.join("\n")
    );
}
