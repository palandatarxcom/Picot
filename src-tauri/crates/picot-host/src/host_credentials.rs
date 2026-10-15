// ABOUTME: Landing zero-credential detection for the "configure a model
// ABOUTME: first" card — file-based truth only, no Pi process needed.

use std::path::Path;

use serde_json::Value;

/// Env vars that upstream pi names as explicit constants (packages/ai
/// env-api-keys.ts) plus the two most common hosted providers. Conservative
/// by design: a miss only means the card shows while the models page itself
/// resolves more env keys via the live catalog.
// ponytail: static list, not a port of upstream's dynamic per-provider env
// resolution — extend only if the card misfires for a named provider.
const COMMON_CREDENTIAL_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "OPENROUTER_API_KEY",
];

/// What one credential file tells us. Only proof of emptiness suppresses the
/// zero-credential card: an unreadable file may well hold credentials, and
/// telling a configured user to "set up a model first" is the worse error.
#[derive(Debug, Eq, PartialEq)]
enum Presence {
    Present,
    Absent,
    Unreadable,
}

enum ReadOutcome {
    Parsed(Value),
    Missing,
    Unreadable,
}

/// True when any usable credential source exists: a non-empty auth.json, a
/// models.json provider with a stored apiKey, or a hit on the common env
/// vars. The host env is what the embedded Pi inherits (fff precedent).
pub fn has_any_credentials(agent_root: &Path) -> bool {
    has_credential_source(agent_root, |key| std::env::var(key).ok())
}

/// Env lookup is a parameter so the probe is testable on a machine that has
/// credentials exported.
fn has_credential_source(agent_root: &Path, env: impl Fn(&str) -> Option<String>) -> bool {
    if auth_json_presence(agent_root) != Presence::Absent {
        return true;
    }
    if models_json_presence(agent_root) != Presence::Absent {
        return true;
    }
    COMMON_CREDENTIAL_ENV_KEYS
        .iter()
        .any(|key| env(key).is_some_and(|value| !value.trim().is_empty()))
}

fn read_json(path: &Path) -> ReadOutcome {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) => ReadOutcome::Parsed(value),
            Err(_) => ReadOutcome::Unreadable,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ReadOutcome::Missing,
        Err(_) => ReadOutcome::Unreadable,
    }
}

/// auth.json: an object with at least one provider entry that is itself a
/// non-empty object (the credential record).
fn auth_json_presence(agent_root: &Path) -> Presence {
    match read_json(&agent_root.join("auth.json")) {
        ReadOutcome::Missing => Presence::Absent,
        ReadOutcome::Unreadable => Presence::Unreadable,
        ReadOutcome::Parsed(value) => match value.as_object() {
            Some(store)
                if store
                    .values()
                    .any(|entry| entry.as_object().is_some_and(|fields| !fields.is_empty())) =>
            {
                Presence::Present
            }
            _ => Presence::Absent,
        },
    }
}

/// models.json: any provider under "providers" with a non-empty apiKey.
fn models_json_presence(agent_root: &Path) -> Presence {
    match read_json(&agent_root.join("models.json")) {
        ReadOutcome::Missing => Presence::Absent,
        ReadOutcome::Unreadable => Presence::Unreadable,
        ReadOutcome::Parsed(value) => {
            let keyed = value
                .get("providers")
                .and_then(Value::as_object)
                .is_some_and(|providers| {
                    providers.values().any(|provider| {
                        provider
                            .get("apiKey")
                            .and_then(Value::as_str)
                            .is_some_and(|key| !key.trim().is_empty())
                    })
                });
            if keyed {
                Presence::Present
            } else {
                Presence::Absent
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// No env hit: the aggregate stays deterministic on machines that export
    /// provider keys.
    fn no_env(_key: &str) -> Option<String> {
        None
    }

    fn temp_agent_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("picot-cred-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_root_reports_no_credentials() {
        let dir = temp_agent_root("empty");
        assert!(!has_credential_source(&dir, no_env));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auth_json_with_entry_counts() {
        let dir = temp_agent_root("auth");
        fs::write(
            dir.join("auth.json"),
            r#"{"anthropic":{"type":"oauth","token":"t"}}"#,
        )
        .unwrap();
        assert!(has_credential_source(&dir, no_env));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auth_json_empty_object_does_not_count() {
        let dir = temp_agent_root("auth-empty");
        fs::write(dir.join("auth.json"), "{}").unwrap();
        assert!(!has_credential_source(&dir, no_env));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn models_json_keyed_provider_counts() {
        let dir = temp_agent_root("models");
        fs::write(
            dir.join("models.json"),
            r#"{"providers":{"openai":{"apiKey":"sk-x"}}}"#,
        )
        .unwrap();
        assert!(has_credential_source(&dir, no_env));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn models_json_blank_key_does_not_count() {
        let dir = temp_agent_root("models-blank");
        fs::write(
            dir.join("models.json"),
            r#"{"providers":{"openai":{"apiKey":"  "}}}"#,
        )
        .unwrap();
        assert!(!has_credential_source(&dir, no_env));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_files_never_prove_absence() {
        let dir = temp_agent_root("unreadable");
        fs::write(dir.join("auth.json"), "{not json").unwrap();
        assert!(has_credential_source(&dir, no_env));
        // A directory where the file belongs is just as unreadable.
        fs::remove_file(dir.join("auth.json")).unwrap();
        fs::create_dir(dir.join("auth.json")).unwrap();
        assert!(has_credential_source(&dir, no_env));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn models_json_unreadable_never_proves_absence() {
        let dir = temp_agent_root("models-unreadable");
        fs::write(dir.join("models.json"), "[").unwrap();
        assert!(has_credential_source(&dir, no_env));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_leg_reads_through_the_injected_lookup() {
        let dir = temp_agent_root("env");
        assert!(!has_credential_source(&dir, no_env));
        assert!(!has_credential_source(&dir, |_| Some("   ".to_string())));
        assert!(has_credential_source(&dir, |key| {
            (key == "ANTHROPIC_API_KEY").then(|| " token ".to_string())
        }));
        let _ = fs::remove_dir_all(&dir);
    }
}
