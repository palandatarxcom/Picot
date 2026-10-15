// ABOUTME: Host control ops for pi-cache-optimizer (~/.pi/agent/pi-cache-optimizer-config.json).
// ABOUTME: Strict package schema: unknown top-level keys make the whole file
// ABOUTME: unparsable in-package, so writes rebuild schema-clean (version +
// ABOUTME: footerMode + preserved promptCacheKey block) and drop unknowns.
use std::path::PathBuf;

use serde_json::{json, Map, Value};

const FOOTER_MODES: &[&str] = &["total", "session", "process"];
const DEFAULT_FOOTER_MODE: &str = "session";
const ENV_FOOTER_MODE: &str = "PI_CACHE_OPTIMIZER_FOOTER_MODE";
const ENV_SWITCHES: &[&str] = &[
    "PI_CACHE_OPTIMIZER_NO_PROMPT_REWRITE",
    "PI_CACHE_OPTIMIZER_NO_SKILL_COMPRESSION",
    "PI_CACHE_OPTIMIZER_NO_OPENAI_CACHE_KEY",
    "PI_CACHE_OPTIMIZER_OPENAI_CACHE_KEY",
];

fn config_file() -> Result<PathBuf, String> {
    Ok(crate::pi_launch::resolve_pi_agent_root()?.join("pi-cache-optimizer-config.json"))
}

fn read_config() -> Result<Option<Value>, String> {
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

/// Validate against the strict schema: version 1|2, footerMode enum,
/// promptCacheKey.omit string[]. Any unknown top-level key is invalid —
/// the package falls back to defaults wholesale on such files.
fn validate(doc: &Value) -> Result<(), String> {
    let Some(object) = doc.as_object() else {
        return Err("config root must be an object".to_string());
    };
    for key in object.keys() {
        if key != "version" && key != "footerMode" && key != "promptCacheKey" {
            return Err(format!("unknown key breaks the package schema: {key}"));
        }
    }
    if let Some(version) = object.get("version") {
        match version.as_u64() {
            Some(1) | Some(2) => {}
            _ => return Err("version must be 1 or 2".to_string()),
        }
    }
    if let Some(mode) = object.get("footerMode") {
        let mode = mode
            .as_str()
            .ok_or_else(|| "footerMode must be a string".to_string())?;
        if !FOOTER_MODES.contains(&mode) {
            return Err(format!("invalid footerMode: {mode}"));
        }
    }
    if let Some(cache_key) = object.get("promptCacheKey") {
        let Some(cache_key) = cache_key.as_object() else {
            return Err("promptCacheKey must be an object".to_string());
        };
        for key in cache_key.keys() {
            if key != "omit" {
                return Err(format!("unknown promptCacheKey key: {key}"));
            }
        }
        if let Some(omit) = cache_key.get("omit") {
            let Some(list) = omit.as_array() else {
                return Err("promptCacheKey.omit must be an array".to_string());
            };
            if list.iter().any(|entry| !entry.is_string()) {
                return Err("promptCacheKey.omit entries must be strings".to_string());
            }
        }
    }
    Ok(())
}

fn env_var_set(name: &str) -> bool {
    std::env::var(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

pub fn get_config() -> Result<Value, String> {
    // footerMode source precedence: config > env > default.
    let (config_mode, omit) = match read_config()? {
        Some(doc) => {
            if let Err(reason) = validate(&doc) {
                // No reset escape: repairs are receipt-bound; the user fixes
                // by hand (deliberate divergence from the fff reset pattern).
                return Ok(json!({ "invalid": { "reason": reason } }));
            }
            let mode = doc
                .pointer("/footerMode")
                .and_then(Value::as_str)
                .map(str::to_string);
            let omit = doc
                .pointer("/promptCacheKey/omit")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            (mode, omit)
        }
        None => (None, Vec::new()),
    };
    let env_mode = std::env::var(ENV_FOOTER_MODE)
        .ok()
        .filter(|mode| FOOTER_MODES.contains(&mode.as_str()));
    let (effective, source) = if let Some(mode) = &config_mode {
        (mode.clone(), "config")
    } else if let Some(mode) = &env_mode {
        (mode.clone(), "env")
    } else {
        (DEFAULT_FOOTER_MODE.to_string(), "default")
    };
    let switches: Value = ENV_SWITCHES
        .iter()
        .map(|name| (name.to_string(), json!(env_var_set(name))))
        .collect::<Map<String, Value>>()
        .into();
    Ok(json!({
        "footerMode": config_mode,
        "effectiveFooterMode": effective,
        "footerModeSource": source,
        "omitList": omit,
        "envSwitches": switches,
    }))
}

pub fn set_config(payload: &Value) -> Result<Value, String> {
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key is required")?;
    if key != "footerMode" {
        return Err(format!("{key} is read-only; only footerMode is writable"));
    }
    let mode = payload
        .get("value")
        .and_then(Value::as_str)
        .ok_or("footerMode must be a string")?;
    if !FOOTER_MODES.contains(&mode) {
        return Err(format!("invalid footerMode: {mode}"));
    }
    // Schema-clean rebuild: version (2, or preserved) + footerMode +
    // promptCacheKey block preserved verbatim; unknown keys dropped by design.
    let raw = read_config()?.unwrap_or_else(|| json!({}));
    if let Err(reason) = validate(&raw) {
        return Err(format!("cannot write onto an invalid file ({reason})"));
    }
    let mut next = Map::new();
    let version = raw.get("version").cloned().unwrap_or(json!(2));
    next.insert("version".into(), version);
    next.insert("footerMode".into(), json!(mode));
    if let Some(cache_key) = raw.get("promptCacheKey") {
        next.insert("promptCacheKey".into(), cache_key.clone());
    }
    let path = config_file()?;
    crate::host_config::write_json(&path, &Value::Object(next)).map_err(|error| match &error {
        crate::host_config::ConfigError::Io(detail) => format!("{}: {detail}", error.code()),
        other => other.code().to_string(),
    })?;
    get_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_schema_rejects_unknown_and_bad_values() {
        assert!(validate(&json!({"version": 2, "footerMode": "total"})).is_ok());
        assert!(validate(&json!({"version": 3})).is_err());
        assert!(validate(&json!({"footerMode": "daily"})).is_err());
        assert!(validate(&json!({"extra": 1})).is_err());
        assert!(validate(&json!({"promptCacheKey": {"omit": [1]}})).is_err());
        assert!(validate(&json!({"promptCacheKey": {"nope": []}})).is_err());
    }

    #[test]
    fn only_footer_mode_is_writable() {
        assert!(set_config(&json!({"key": "version", "value": 1})).is_err());
        assert!(set_config(&json!({"key": "footerMode", "value": "nope"})).is_err());
        assert!(set_config(&json!({"key": "footerMode"})).is_err());
    }
}
