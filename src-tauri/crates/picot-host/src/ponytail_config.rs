// ABOUTME: Host control ops for @dietrichgebert/ponytail's config
// ABOUTME: (~/.config/ponytail/config.json, XDG_CONFIG_HOME-aware, BOM-tolerant).
use std::path::PathBuf;

use serde_json::{json, Map, Value};

const MODES: &[&str] = &["lite", "full", "ultra"];
const DEFAULT_MODE: &str = "full";

fn env_shadows() -> Vec<(&'static str, &'static str)> {
    vec![
        ("defaultMode", "PONYTAIL_DEFAULT_MODE"),
        ("quietStartup", "PONYTAIL_QUIET_STARTUP"),
        ("hideStatus", "PONYTAIL_HIDE_STATUS"),
    ]
}

fn config_file() -> Result<PathBuf, String> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return Ok(PathBuf::from(xdg).join("ponytail").join("config.json"));
        }
    }
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.trim().is_empty() {
                return Ok(PathBuf::from(appdata).join("ponytail").join("config.json"));
            }
        }
    }
    let home = dirs::home_dir().ok_or_else(|| "Cannot resolve home directory".to_string())?;
    Ok(home.join(".config").join("ponytail").join("config.json"))
}

/// The package strips a UTF-8 BOM before JSON parsing — mirror that here.
fn read_config() -> Result<Map<String, Value>, String> {
    let path = config_file()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text.trim_start_matches('\u{feff}').to_string(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("cannot read config file: {error}")),
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|error| format!("config file is not valid JSON: {error}"))?;
    match parsed {
        Value::Object(map) => Ok(map),
        _ => Ok(Map::new()),
    }
}

fn write_config(map: &Map<String, Value>) -> Result<(), String> {
    let path = config_file()?;
    crate::host_config::write_json(&path, &Value::Object(map.clone())).map_err(|error| match &error
    {
        crate::host_config::ConfigError::Io(detail) => format!("{}: {detail}", error.code()),
        other => other.code().to_string(),
    })
}

fn env_var_set(name: &str) -> bool {
    std::env::var(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

pub fn get_config() -> Result<Value, String> {
    let map = read_config()?;
    let env_shadowed: Vec<String> = env_shadows()
        .iter()
        .filter(|(_field, var)| env_var_set(var))
        .map(|(field, _)| field.to_string())
        .collect();
    let shadow_names: Value = env_shadows()
        .iter()
        .map(|(field, var)| (field.to_string(), json!(var)))
        .collect::<Map<String, Value>>()
        .into();
    // Effective default mode: env > file > built-in "full" (package rule).
    let effective_mode = std::env::var("PONYTAIL_DEFAULT_MODE")
        .ok()
        .filter(|mode| MODES.contains(&mode.as_str()))
        .or_else(|| {
            map.get("defaultMode")
                .and_then(Value::as_str)
                .filter(|mode| MODES.contains(&mode.to_string().as_str()))
                .map(str::to_string)
        })
        .unwrap_or_else(|| DEFAULT_MODE.to_string());
    Ok(json!({
        "defaultMode": map.get("defaultMode").cloned().unwrap_or(Value::Null),
        "quietStartup": map.get("quietStartup").cloned().unwrap_or(Value::Null),
        "hideStatus": map.get("hideStatus").cloned().unwrap_or(Value::Null),
        "effective": { "defaultMode": effective_mode },
        "envShadowed": env_shadowed,
        "shadowNames": shadow_names,
    }))
}

pub fn set_config(payload: &Value) -> Result<Value, String> {
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key is required")?;
    let mut map = read_config()?;
    match key {
        "defaultMode" => match payload.get("value") {
            None | Some(Value::Null) => {
                map.remove("defaultMode");
            }
            Some(Value::String(mode)) => {
                // `review` is session-only by package rule and unknown values
                // silently fall back — reject both so the file never lies.
                if !MODES.contains(&mode.as_str()) {
                    return Err(format!("invalid defaultMode: {mode}"));
                }
                map.insert("defaultMode".into(), json!(mode));
            }
            _ => return Err("defaultMode must be a string or null".to_string()),
        },
        "quietStartup" | "hideStatus" => match payload.get("value") {
            None | Some(Value::Null) => {
                map.remove(key);
            }
            Some(value @ Value::Bool(_)) => {
                map.insert(key.to_string(), value.clone());
            }
            _ => return Err(format!("{key} must be a boolean or null")),
        },
        _ => return Err(format!("unknown ponytail config key: {key}")),
    }
    write_config(&map)?;
    get_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_validation_rejects_review_and_unknowns() {
        for bad in ["review", "off", "FULL", ""] {
            let payload = json!({"key": "defaultMode", "value": bad});
            assert!(
                set_config(&payload).is_err(),
                "mode {bad:?} must be rejected"
            );
        }
        // Valid modes pass validation and would write to the real config
        // dir — unit tests never touch the developer's config, so the
        // happy-write path is covered by the renderer tests' mocked ops.
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(set_config(&json!({"key": "nope", "value": true})).is_err());
    }

    #[test]
    fn bom_is_stripped_before_parse() {
        let text = "\u{feff}{\"defaultMode\":\"lite\"}";
        let parsed: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap();
        assert_eq!(parsed["defaultMode"], json!("lite"));
    }

    #[test]
    fn env_shadow_names_cover_all_three_fields() {
        assert_eq!(env_shadows().len(), 3);
    }
}
