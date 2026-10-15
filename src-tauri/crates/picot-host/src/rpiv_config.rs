// ABOUTME: Host control ops for the @juicesharp rpiv family configs
// ABOUTME: (~/.config/<pkg>/config.json, matching the package's plain
// ABOUTME: homedir/.config resolver — NOT XDG-aware; verified against
// ABOUTME: rpiv-config v-latest configPath). Shared KeyId-grammar validation
// ABOUTME: for collapse keys (verbatim port of the packages' own validator).
use std::path::PathBuf;

use serde_json::{json, Value};

const SPECIAL_KEYS: &[&str] = &[
    "escape",
    "esc",
    "enter",
    "return",
    "tab",
    "space",
    "backspace",
    "delete",
    "insert",
    "clear",
    "home",
    "end",
    "pageup",
    "pagedown",
    "up",
    "down",
    "left",
    "right",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
];
const MODIFIERS: &[&str] = &["ctrl", "shift", "alt", "super"];
const TODO_DEFAULT_MAX_WIDGET_LINES: i64 = 12;
const TODO_DEFAULT_COLLAPSE_KEY: &str = "ctrl+shift+t";
const ASKUSER_DEFAULT_COLLAPSE_KEY: &str = "ctrl+]";
const COLLAPSE_KEY_OFF: &str = "off";

/// The packages' KeyId grammar: zero or more distinct modifiers, then a base
/// that is one printable punctuation/alnum char or a named special key. A
/// strict port — pi-tui's parseKeyId takes the LAST `+` part as the base and
/// ignores unknown parts, so typos must be rejected here, not defaulted.
pub fn is_valid_collapse_key_spec(spec: &str) -> bool {
    if spec.is_empty() || spec.starts_with('+') || spec.ends_with('+') || spec.contains("++") {
        return false;
    }
    let parts: Vec<&str> = spec.split('+').collect();
    let base = parts[parts.len() - 1];
    let modifiers = &parts[..parts.len() - 1];
    if modifiers
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != modifiers.len()
    {
        return false;
    }
    if !modifiers.iter().all(|m| MODIFIERS.contains(m)) {
        return false;
    }
    if base.chars().count() == 1 {
        let Some(ch) = base.chars().next() else {
            return false;
        };
        ch.is_ascii_lowercase()
            || ch.is_ascii_digit()
            || "_-!@#$%^&*()|~`'\":;,./<>?[]{}=\\".contains(ch)
    } else {
        SPECIAL_KEYS.contains(&base)
    }
}

fn rpiv_config_file(package_dir: &str) -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot resolve home directory".to_string())?;
    Ok(home.join(".config").join(package_dir).join("config.json"))
}

fn read_rpiv_config(package_dir: &str) -> Result<Value, String> {
    let path = rpiv_config_file(package_dir)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let parsed: Value = serde_json::from_str(&text)
                .map_err(|error| format!("config file is not valid JSON: {error}"))?;
            if !parsed.is_object() {
                return Ok(json!({}));
            }
            Ok(parsed)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(format!("cannot read config file: {error}")),
    }
}

fn write_rpiv_config(package_dir: &str, value: &Value) -> Result<(), String> {
    let path = rpiv_config_file(package_dir)?;
    crate::host_config::write_json(&path, value).map_err(|error| match &error {
        crate::host_config::ConfigError::Io(detail) => {
            format!("{}: {detail}", error.code())
        }
        other => other.code().to_string(),
    })
}

/// Effective `maxWidgetLines`: the package falls back to 12 for missing,
/// non-number, or values below 3.
fn effective_max_widget_lines(config: &Value) -> Value {
    match config.get("maxWidgetLines").and_then(Value::as_i64) {
        Some(lines) if lines >= 3 => json!(lines),
        _ => json!(TODO_DEFAULT_MAX_WIDGET_LINES),
    }
}

/// Effective `collapseKey`: trim + lowercase, `off` sentinel honored, invalid
/// specs fall back to the default (the package does the same on read).
fn effective_collapse_key_with_default(config: &Value, default: &str) -> Value {
    let raw = match config.get("collapseKey").and_then(Value::as_str) {
        Some(raw) => raw.trim().to_lowercase(),
        None => return json!(default),
    };
    if raw.is_empty() {
        return json!(default);
    }
    if raw == COLLAPSE_KEY_OFF {
        return json!(COLLAPSE_KEY_OFF);
    }
    json!(if is_valid_collapse_key_spec(&raw) {
        raw
    } else {
        default.to_string()
    })
}

fn effective_collapse_key(config: &Value) -> Value {
    effective_collapse_key_with_default(config, TODO_DEFAULT_COLLAPSE_KEY)
}

pub fn get_todo_config() -> Result<Value, String> {
    let config = read_rpiv_config("rpiv-todo")?;
    Ok(json!({
        "values": {
            "maxWidgetLines": config.get("maxWidgetLines").cloned().unwrap_or(Value::Null),
            "collapseKey": config.get("collapseKey").cloned().unwrap_or(Value::Null),
        },
        "effective": {
            "maxWidgetLines": effective_max_widget_lines(&config),
            "collapseKey": effective_collapse_key(&config),
        },
    }))
}

pub fn set_todo_config(payload: &Value) -> Result<Value, String> {
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key is required")?;
    let mut config = read_rpiv_config("rpiv-todo")?;
    let object = config
        .as_object_mut()
        .ok_or("config root must be an object")?;
    match key {
        "maxWidgetLines" => match payload.get("value") {
            None | Some(Value::Null) => {
                object.remove("maxWidgetLines");
            }
            Some(Value::Number(_)) => {
                let lines = payload["value"]
                    .as_i64()
                    .filter(|lines| *lines >= 3)
                    .ok_or("maxWidgetLines must be an integer >= 3")?;
                object.insert("maxWidgetLines".into(), json!(lines));
            }
            _ => return Err("maxWidgetLines must be a number or null".to_string()),
        },
        "collapseKey" => match payload.get("value") {
            None | Some(Value::Null) => {
                object.remove("collapseKey");
            }
            Some(Value::String(raw)) => {
                let normalized = raw.trim().to_lowercase();
                if normalized != COLLAPSE_KEY_OFF && !is_valid_collapse_key_spec(&normalized) {
                    return Err(format!("invalid collapseKey spec: {raw}"));
                }
                if normalized.is_empty() {
                    object.remove("collapseKey");
                } else {
                    object.insert("collapseKey".into(), json!(normalized));
                }
            }
            _ => return Err("collapseKey must be a string or null".to_string()),
        },
        _ => return Err(format!("unknown todo config key: {key}")),
    }
    write_rpiv_config("rpiv-todo", &config)?;
    get_todo_config()
}
pub fn get_askuser_config() -> Result<Value, String> {
    let config = read_rpiv_config("rpiv-ask-user-question")?;
    Ok(json!({
        "values": {
            "collapseKey": config.get("collapseKey").cloned().unwrap_or(Value::Null),
        },
        "effective": {
            "collapseKey": effective_collapse_key_with_default(&config, ASKUSER_DEFAULT_COLLAPSE_KEY),
        },
    }))
}

pub fn set_askuser_config(payload: &Value) -> Result<Value, String> {
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key is required")?;
    if key != "collapseKey" {
        return Err(format!("unknown ask-user-question config key: {key}"));
    }
    let mut config = read_rpiv_config("rpiv-ask-user-question")?;
    let object = config
        .as_object_mut()
        .ok_or("config root must be an object")?;
    match payload.get("value") {
        None | Some(Value::Null) => {
            object.remove("collapseKey");
        }
        Some(Value::String(raw)) => {
            let normalized = raw.trim().to_lowercase();
            if normalized != COLLAPSE_KEY_OFF && !is_valid_collapse_key_spec(&normalized) {
                return Err(format!("invalid collapseKey spec: {raw}"));
            }
            if normalized.is_empty() {
                object.remove("collapseKey");
            } else {
                object.insert("collapseKey".into(), json!(normalized));
            }
        }
        _ => return Err("collapseKey must be a string or null".to_string()),
    }
    write_rpiv_config("rpiv-ask-user-question", &config)?;
    get_askuser_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_key_grammar_accepts_and_rejects() {
        assert!(is_valid_collapse_key_spec("ctrl+shift+t"));
        assert!(is_valid_collapse_key_spec("]"));
        assert!(is_valid_collapse_key_spec("f5"));
        assert!(is_valid_collapse_key_spec("ctrl+alt+delete"));
        // Strict rejections: typos that pi-tui would silently widen.
        assert!(!is_valid_collapse_key_spec("ctr+]"));
        assert!(!is_valid_collapse_key_spec("ctrl+shift"));
        assert!(!is_valid_collapse_key_spec("ctrl+ctrl+t"));
        assert!(!is_valid_collapse_key_spec("hyper+t"));
        assert!(!is_valid_collapse_key_spec(""));
        assert!(!is_valid_collapse_key_spec("ctrl++t"));
    }

    #[test]
    fn effective_values_mirror_package_defaults() {
        let empty = json!({});
        assert_eq!(effective_max_widget_lines(&empty), json!(12));
        assert_eq!(effective_collapse_key(&empty), json!("ctrl+shift+t"));
        assert_eq!(
            effective_max_widget_lines(&json!({"maxWidgetLines": 2})),
            json!(12)
        );
        assert_eq!(
            effective_collapse_key(&json!({"collapseKey": "  OFF "})),
            json!("off")
        );
        assert_eq!(
            effective_collapse_key(&json!({"collapseKey": "bogus++"})),
            json!("ctrl+shift+t")
        );
    }

    #[test]
    fn set_rejects_invalid_values_before_write() {
        let bad = json!({"key": "collapseKey", "value": "ctr+]"});
        assert!(set_askuser_config(&bad).is_err());
        let bad = json!({"key": "maxWidgetLines", "value": 1});
        assert!(set_todo_config(&bad).is_err());
        let bad_key = json!({"key": "collapseKey", "value": "ctr+]"});
        assert!(set_todo_config(&bad_key).is_err());
        let unknown = json!({"key": "nope", "value": 1});
        assert!(set_todo_config(&unknown).is_err());
    }

    #[test]
    fn askuser_effective_uses_its_own_default() {
        assert_eq!(
            effective_collapse_key_with_default(&json!({}), ASKUSER_DEFAULT_COLLAPSE_KEY),
            json!("ctrl+]")
        );
    }
}
