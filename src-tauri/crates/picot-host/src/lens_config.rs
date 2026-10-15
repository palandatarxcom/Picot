// ABOUTME: Host control ops for pi-lens (~/.pi-lens/config.json global layer,
// ABOUTME: project .pi-lens.json via optional cwd). Registry-driven: the
// ABOUTME: curated key set with per-key defaults; effective precedence is
// ABOUTME: env > project > global > default (the CLI-flag tier is dropped
// ABOUTME: host-side — the host never sets lens flags, fff rationale).
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Curated registry: (dotted key, kind, default). kind: b = bool,
/// e:<a|b|c> = enum of those values, i = integer (>=1), a = string array.
const REGISTRY: &[(&str, &str)] = &[
    ("lens.enabled", "b:default-true"),
    ("lsp.enabled", "b:default-true"),
    ("format.enabled", "b:default-true"),
    ("format.mode", "e:deferred|immediate"),
    ("autofix.enabled", "b:default-true"),
    ("tests.enabled", "b:default-true"),
    ("delta.enabled", "b:default-true"),
    ("guard.enabled", "b:default-true"),
    ("guard.sharedCheckout", "b:default-true"),
    ("readGuard.enabled", "b:default-true"),
    ("contextInjection.enabled", "b:default-true"),
    ("turnSummary.enabled", "b:default-false"),
    ("actionableWarnings.enabled", "b:default-false"),
    (
        "actionableWarnings.includeLspCodeActions",
        "b:default-false",
    ),
    ("actionableWarnings.autoFix.enabled", "b:default-false"),
    ("actionableWarnings.deltaOnly", "b:default-true"),
    ("ui.compactToolLine", "b:default-false"),
    ("tools.lazy", "b:default-true"),
    ("analyzers.knip.enabled", "b:default-true"),
    ("analyzers.jscpd.enabled", "b:default-true"),
    ("analyzers.madge.enabled", "b:default-true"),
    ("analyzers.gitleaks.enabled", "b:default-true"),
    ("analyzers.govulncheck.enabled", "b:default-true"),
    ("analyzers.deadCode.enabled", "b:default-true"),
    ("analyzers.complexity.enabled", "b:default-true"),
    ("maxProjectFiles", "i:8000"),
];

fn kind_of(key: &str) -> Option<&'static str> {
    REGISTRY
        .iter()
        .find(|(entry, _)| *entry == key)
        .map(|(_, kind)| *kind)
}

fn default_of(key: &str) -> Value {
    let kind = kind_of(key).unwrap_or("b:default-true");
    if let Some(rest) = kind.strip_prefix("b:default-") {
        return json!(rest == "true");
    }
    if let Some(rest) = kind.strip_prefix("e:") {
        return json!(rest.split('|').next().unwrap_or_default());
    }
    if let Some(rest) = kind.strip_prefix("i:") {
        return json!(rest.parse::<u64>().unwrap_or(0));
    }
    Value::Null
}

fn validate_value(key: &str, value: &Value) -> Result<(), String> {
    let kind = kind_of(key).ok_or_else(|| format!("unknown pi-lens config key: {key}"))?;
    if let Some(_rest) = kind.strip_prefix("b:") {
        return if value.is_boolean() {
            Ok(())
        } else {
            Err(format!("{key} must be a boolean"))
        };
    }
    if let Some(rest) = kind.strip_prefix("e:") {
        let options: Vec<&str> = rest.split('|').collect();
        let mode = value
            .as_str()
            .ok_or_else(|| format!("{key} must be one of {options:?}"))?;
        return if options.contains(&mode) {
            Ok(())
        } else {
            Err(format!("{key} must be one of {options:?}"))
        };
    }
    if kind.starts_with("i:") {
        let number = value
            .as_u64()
            .filter(|number| *number >= 1)
            .ok_or_else(|| format!("{key} must be an integer ≥ 1"))?;
        let _ = number;
        return Ok(());
    }
    Err(format!("{key} has an unsupported kind"))
}

fn global_config_file() -> Result<PathBuf, String> {
    if let Ok(relocated) = std::env::var("PI_LENS_CONFIG_PATH") {
        if !relocated.trim().is_empty() {
            return Ok(PathBuf::from(relocated));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| "Cannot resolve home directory".to_string())?;
    Ok(home.join(".pi-lens").join("config.json"))
}

fn read_layer(path: &Path) -> Result<Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map(|parsed| match parsed {
                Value::Object(map) => map,
                _ => Map::new(),
            })
            .map_err(|error| format!("config file is not valid JSON: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(format!("cannot read config file: {error}")),
    }
}

fn pointer_get(map: &Map<String, Value>, key: &str) -> Option<Value> {
    let pointer = format!("/{}", key.replace('.', "/"));
    let parsed: Vec<&str> = pointer.split('/').skip(1).collect();
    let mut current = Value::Object(map.clone());
    for segment in parsed {
        current = current.get(segment)?.clone();
    }
    Some(current)
}

fn pointer_set(map: &mut Map<String, Value>, key: &str, value: Value) {
    let segments: Vec<&str> = key.split('.').collect();
    let mut current = map;
    for (index, segment) in segments.iter().enumerate() {
        if index + 1 == segments.len() {
            current.insert(segment.to_string(), value);
            return;
        }
        let entry = current
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        current = match entry.as_object_mut() {
            Some(nested) => nested,
            None => return,
        };
    }
}

/// Remove a dotted leaf; returns true when the leaf existed.
fn pointer_remove(map: &mut Map<String, Value>, key: &str) -> bool {
    let segments: Vec<&str> = key.split('.').collect();
    let Some((last, parents)) = segments.split_last() else {
        return false;
    };
    let mut current: &mut Map<String, Value> = map;
    for segment in parents {
        let Some(entry) = current.get_mut(*segment) else {
            return false;
        };
        if !entry.is_object() {
            return false;
        }
        let Some(nested) = entry.as_object_mut() else {
            return false;
        };
        current = nested;
    }
    current.remove(*last).is_some()
}

/// The only env tier the registry exposes today (package docs).
fn env_override(key: &str) -> Option<Value> {
    if key == "contextInjection.enabled" {
        let set = std::env::var("PI_LENS_NO_CONTEXT_INJECTION")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        return if set { Some(json!(false)) } else { None };
    }
    None
}

pub fn get_config(payload: &Value) -> Result<Value, String> {
    let global_path = global_config_file()?;
    let global = read_layer(&global_path)?;
    let cwd = payload.get("cwd").and_then(Value::as_str);
    let (project_path, project) = match cwd {
        Some(cwd) if !cwd.trim().is_empty() => {
            let path = PathBuf::from(cwd).join(".pi-lens.json");
            let layer = read_layer(&path)?;
            (Some(path), layer)
        }
        _ => (None, Map::new()),
    };
    let mut values = Map::new();
    let mut effective = Map::new();
    let mut sources = Map::new();
    for (key, _) in REGISTRY {
        let stored = pointer_get(&global, key);
        values.insert(key.to_string(), stored.clone().unwrap_or(Value::Null));
        let value = if let Some(env_value) = env_override(key) {
            sources.insert(key.to_string(), json!("env"));
            env_value
        } else if let Some(project_value) = pointer_get(&project, key) {
            sources.insert(key.to_string(), json!("project"));
            project_value
        } else if let Some(stored_value) = stored {
            sources.insert(key.to_string(), json!("global"));
            stored_value
        } else {
            sources.insert(key.to_string(), json!("default"));
            default_of(key)
        };
        effective.insert(key.to_string(), value);
    }
    Ok(json!({
        "values": values,
        "effective": effective,
        "sources": sources,
        "projectFile": project_path
            .map(|path| json!(path.to_string_lossy().into_owned()))
            .unwrap_or(Value::Null),
        "configPath": global_path.to_string_lossy(),
        "relocatedByEnv": std::env::var("PI_LENS_CONFIG_PATH")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false),
    }))
}

pub fn set_config(payload: &Value) -> Result<Value, String> {
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key is required")?;
    let value = payload.get("value").cloned().unwrap_or(Value::Null);
    if !value.is_null() {
        validate_value(key, &value)?;
    }
    // Global layer only (v1); preserve-unknowns + $schema (advisor semantics
    // — the schema is far larger than the curation).
    let path = global_config_file()?;
    let mut global = read_layer(&path)?;
    if value.is_null() {
        // Null clears the global leaf so the tier below shows through.
        pointer_remove(&mut global, key);
    } else {
        pointer_set(&mut global, key, value);
    }
    crate::host_config::write_json(&path, &Value::Object(global)).map_err(
        |error| match &error {
            crate::host_config::ConfigError::Io(detail) => format!("{}: {detail}", error.code()),
            other => other.code().to_string(),
        },
    )?;
    get_config(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_kinds_validate() {
        assert!(validate_value("format.mode", &json!("immediate")).is_ok());
        assert!(validate_value("format.mode", &json!("now")).is_err());
        assert!(validate_value("lsp.enabled", &json!(false)).is_ok());
        assert!(validate_value("lsp.enabled", &json!("off")).is_err());
        assert!(validate_value("maxProjectFiles", &json!(100)).is_ok());
        assert!(validate_value("maxProjectFiles", &json!(0)).is_err());
        assert!(validate_value("nope.enabled", &json!(true)).is_err());
    }

    #[test]
    fn pointer_roundtrip_over_nested_keys() {
        let mut map = Map::new();
        pointer_set(&mut map, "actionableWarnings.autoFix.enabled", json!(true));
        assert_eq!(
            pointer_get(&map, "actionableWarnings.autoFix.enabled"),
            Some(json!(true))
        );
        pointer_set(&mut map, "format.mode", json!("deferred"));
        assert_eq!(pointer_get(&map, "format.mode"), Some(json!("deferred")));
    }

    #[test]
    fn defaults_follow_the_registry() {
        assert_eq!(default_of("lens.enabled"), json!(true));
        assert_eq!(default_of("format.mode"), json!("deferred"));
        assert_eq!(default_of("maxProjectFiles"), json!(8000));
    }
}
