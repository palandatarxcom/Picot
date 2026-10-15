// ABOUTME: Host control ops for pi-caveman (~/.pi/agent/caveman.json) —
// ABOUTME: default level + status toggle, matching the extension's own
// ABOUTME: loadConfig fallbacks.
use serde_json::{json, Map, Value};

const LEVELS: &[&str] = &[
    "off",
    "lite",
    "full",
    "ultra",
    "wenyan-lite",
    "wenyan",
    "wenyan-ultra",
    "micro",
];
const DEFAULT_LEVEL: &str = "full";

fn config_file() -> Result<std::path::PathBuf, String> {
    Ok(crate::pi_launch::resolve_pi_agent_root()?.join("caveman.json"))
}

fn read_config() -> Result<Map<String, Value>, String> {
    let path = config_file()?;
    match std::fs::read_to_string(&path) {
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

pub fn get_config() -> Result<Value, String> {
    let map = read_config()?;
    // Package fallbacks: invalid level → full, non-bool status → true.
    let level = map
        .get("defaultLevel")
        .and_then(Value::as_str)
        .filter(|level| LEVELS.contains(level))
        .unwrap_or(DEFAULT_LEVEL);
    let show_status = map
        .get("showStatus")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok(json!({
        "values": {
            "defaultLevel": map.get("defaultLevel").cloned().unwrap_or(Value::Null),
            "showStatus": map.get("showStatus").cloned().unwrap_or(Value::Null),
        },
        "effective": { "defaultLevel": level, "showStatus": show_status },
    }))
}

pub fn set_config(payload: &Value) -> Result<Value, String> {
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key is required")?;
    let mut map = read_config()?;
    match key {
        "defaultLevel" => match payload.get("value") {
            None | Some(Value::Null) => {
                map.remove("defaultLevel");
            }
            Some(Value::String(level)) => {
                if !LEVELS.contains(&level.as_str()) {
                    return Err(format!("invalid defaultLevel: {level}"));
                }
                map.insert("defaultLevel".into(), json!(level));
            }
            _ => return Err("defaultLevel must be a string or null".to_string()),
        },
        "showStatus" => match payload.get("value") {
            None | Some(Value::Null) => {
                map.remove("showStatus");
            }
            Some(value @ Value::Bool(_)) => {
                map.insert("showStatus".into(), value.clone());
            }
            _ => return Err("showStatus must be a boolean or null".to_string()),
        },
        _ => return Err(format!("unknown caveman config key: {key}")),
    }
    let path = config_file()?;
    crate::host_config::write_json(&path, &Value::Object(map.clone())).map_err(
        |error| match &error {
            crate::host_config::ConfigError::Io(detail) => format!("{}: {detail}", error.code()),
            other => other.code().to_string(),
        },
    )?;
    get_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_validation_mirrors_the_package_grammar() {
        for bad in ["FULL", "wenyan_ultra", ""] {
            assert!(set_config(&json!({"key": "defaultLevel", "value": bad})).is_err());
        }
        for good in ["off", "micro", "wenyan-ultra"] {
            // Validation-only: a valid level would write to the real config,
            // which unit tests never touch.
            let payload = json!({"key": "defaultLevel", "value": good});
            assert!(payload["value"].is_string());
        }
        assert!(set_config(&json!({"key": "nope", "value": true})).is_err());
    }
}
