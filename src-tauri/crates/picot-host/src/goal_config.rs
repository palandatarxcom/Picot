// ABOUTME: Host control ops for @narumitw/pi-goal (~/.pi/agent/pi-goal.json).
// ABOUTME: Mirrors the package's real settings rules (pi-goal 0.54.8
// ABOUTME: src/settings.ts + docs/settings.md): only `rpc.enabled` and
// ABOUTME: `continuationLimits.*` are typed; unknown/retired keys are ignored
// ABOUTME: and preserved on save, and `experimental.goals === true` is a
// ABOUTME: legacy warning, not an error.
use std::path::PathBuf;

use serde_json::{json, Map, Value};

const DEFAULTS: &str =
    r#"{"rpc":{"enabled":false},"continuationLimits":{"automaticTurns":25,"noProgressTurns":3}}"#;

fn config_file() -> Result<PathBuf, String> {
    Ok(crate::pi_launch::resolve_pi_agent_root()?.join("pi-goal.json"))
}

/// Mirror of the package's `normalizeGoalSettings`: only the two known
/// subtrees are typed, unknown keys anywhere are ignored (the package
/// preserves them verbatim on save). Returns the effective settings or an
/// invalid reason for a shape the package itself would refuse.
fn effective_settings(doc: &Value) -> Result<Value, String> {
    let defaults: Value = serde_json::from_str(DEFAULTS).expect("static defaults");
    let Some(object) = doc.as_object() else {
        return Err("config root must be an object".to_string());
    };
    let mut effective = Map::new();
    let rpc = object.get("rpc").unwrap_or(&defaults["rpc"]);
    let Some(rpc) = rpc.as_object() else {
        return Err("rpc must be an object".to_string());
    };
    let enabled = rpc
        .get("enabled")
        .cloned()
        .unwrap_or_else(|| defaults["rpc"]["enabled"].clone());
    if !enabled.is_boolean() {
        return Err("rpc.enabled must be a boolean".to_string());
    }
    effective.insert("rpc".into(), json!({ "enabled": enabled }));

    let limits = object
        .get("continuationLimits")
        .unwrap_or(&defaults["continuationLimits"]);
    let Some(limits) = limits.as_object() else {
        return Err("continuationLimits must be an object".to_string());
    };
    let mut effective_limits = Map::new();
    for field in ["automaticTurns", "noProgressTurns"] {
        let value = limits
            .get(field)
            .cloned()
            .unwrap_or_else(|| defaults["continuationLimits"][field].clone());
        match &value {
            Value::Null => {}
            Value::Number(number) => {
                let turns = number
                    .as_u64()
                    .filter(|turns| *turns > 0)
                    .ok_or_else(|| format!("{field} must be a safe integer > 0 or null"))?;
                effective_limits.insert(field.to_string(), json!(turns));
                continue;
            }
            _ => return Err(format!("{field} must be a safe integer > 0 or null")),
        }
        effective_limits.insert(field.to_string(), Value::Null);
    }
    effective.insert("continuationLimits".into(), Value::Object(effective_limits));
    Ok(Value::Object(effective))
}

fn read_doc() -> Result<Option<Value>, String> {
    let path = config_file()?;
    match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => {
            Ok(Some(serde_json::from_str(&text).map_err(|error| {
                format!("config file is not valid JSON: {error}")
            })?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read config file: {error}")),
    }
}

/// The package treats `experimental.goals: true` as a removed legacy setting:
/// the file stays valid, and the user sees a warning (docs/settings.md).
fn legacy_experimental_goals(doc: &Value) -> bool {
    doc.get("experimental")
        .and_then(Value::as_object)
        .and_then(|experimental| experimental.get("goals"))
        .and_then(Value::as_bool)
        == Some(true)
}

fn write_doc(value: &Value) -> Result<(), String> {
    let path = config_file()?;
    crate::host_config::write_json(&path, value).map_err(|error| match &error {
        crate::host_config::ConfigError::Io(detail) => format!("{}: {detail}", error.code()),
        other => other.code().to_string(),
    })
}

pub fn get_config() -> Result<Value, String> {
    match read_doc()? {
        None => Ok(json!({
            "settings": effective_settings(&json!({}))?,
            "invalid": null,
            "legacyExperimentalGoals": false,
        })),
        Some(doc) => match effective_settings(&doc) {
            Ok(settings) => Ok(json!({
                "settings": settings,
                "invalid": null,
                "legacyExperimentalGoals": legacy_experimental_goals(&doc),
            })),
            // An invalid file reports no legacy hint, exactly like the
            // package's load result (`kind !== "invalid" && ...`).
            Err(reason) => Ok(json!({
                "settings": Value::Null,
                "invalid": { "reason": reason },
                "legacyExperimentalGoals": false,
            })),
        },
    }
}

pub fn set_config(payload: &Value) -> Result<Value, String> {
    if payload.get("reset").and_then(Value::as_bool) == Some(true) {
        let defaults: Value = serde_json::from_str(DEFAULTS).expect("static defaults");
        write_doc(&defaults)?;
        return get_config();
    }
    // Merge the single key onto the RAW document, then validate the whole
    // merged file — the package falls back wholesale on any invalid key, so
    // a partial write must never persist an invalid document.
    let raw = read_doc()?.unwrap_or_else(|| json!({}));
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key is required")?;
    let value = payload.get("value").cloned().unwrap_or(Value::Null);
    let mut merged = raw.as_object().cloned().unwrap_or_default();
    match key {
        "rpc.enabled" => {
            if !value.is_boolean() {
                return Err("rpc.enabled must be a boolean".to_string());
            }
            let rpc = merged.entry("rpc").or_insert_with(|| json!({}));
            let rpc = rpc.as_object_mut().ok_or("rpc must be an object")?;
            rpc.insert("enabled".into(), value);
        }
        "continuationLimits.automaticTurns" | "continuationLimits.noProgressTurns" => {
            let field = key.rsplit('.').next().expect("dotted key has a field");
            match &value {
                Value::Null => {}
                Value::Number(_) => {
                    let turns = value
                        .as_u64()
                        .filter(|turns| *turns > 0)
                        .ok_or_else(|| format!("{field} must be a safe integer > 0 or null"))?;
                    merged_entry(&mut merged, field, json!(turns))?;
                    // skip the tail insert below for the numeric path
                    validate_and_write(&mut merged)?;
                    return get_config();
                }
                _ => return Err(format!("{field} must be a safe integer > 0 or null")),
            }
            merged_entry(&mut merged, field, Value::Null)?;
        }
        _ => return Err(format!("unknown pi-goal config key: {key}")),
    }
    validate_and_write(&mut merged)?;
    get_config()
}

fn merged_entry(merged: &mut Map<String, Value>, field: &str, value: Value) -> Result<(), String> {
    let limits = merged
        .entry("continuationLimits")
        .or_insert_with(|| json!({}));
    let limits = limits
        .as_object_mut()
        .ok_or("continuationLimits must be an object")?;
    limits.insert(field.to_string(), value);
    Ok(())
}

fn validate_and_write(merged: &mut Map<String, Value>) -> Result<(), String> {
    let doc = Value::Object(merged.clone());
    effective_settings(&doc)?;
    write_doc(&doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_reject_and_unknown_keys_are_tolerated() {
        // Type errors: the package's normalizeGoalSettings returns undefined.
        assert!(effective_settings(&json!({"rpc": {"enabled": "yes"}})).is_err());
        assert!(effective_settings(&json!({"rpc": []})).is_err());
        assert!(effective_settings(&json!({"continuationLimits": {"automaticTurns": 0}})).is_err());
        assert!(effective_settings(&json!({"continuationLimits": 3})).is_err());
        assert!(
            effective_settings(&json!({"continuationLimits": {"noProgressTurns": -1}})).is_err()
        );
        // Unknown / retired keys are ignored, never fatal.
        assert_eq!(
            effective_settings(&json!({
                "toolVisibility": "always",
                "experimental": {"goals": false},
                "rpc": {"enabled": true},
                "continuationLimits": {"automaticTurns": 7},
            }))
            .unwrap(),
            json!({"rpc": {"enabled": true}, "continuationLimits": {"automaticTurns": 7, "noProgressTurns": 3}})
        );
        assert!(effective_settings(&json!({"rpc": {"enabled": true}})).is_ok());
        assert_eq!(
            effective_settings(&json!({})).unwrap()["continuationLimits"]["automaticTurns"],
            json!(25)
        );
        assert_eq!(
            effective_settings(&json!({"continuationLimits": {"automaticTurns": null}})).unwrap()
                ["continuationLimits"]["automaticTurns"],
            Value::Null
        );
    }

    #[test]
    fn legacy_experimental_goals_flags_only_the_removed_true_setting() {
        assert!(legacy_experimental_goals(
            &json!({"experimental": {"goals": true}})
        ));
        assert!(!legacy_experimental_goals(
            &json!({"experimental": {"goals": false}})
        ));
        assert!(!legacy_experimental_goals(&json!({"experimental": "yes"})));
        assert!(!legacy_experimental_goals(&json!({})));
    }
}
