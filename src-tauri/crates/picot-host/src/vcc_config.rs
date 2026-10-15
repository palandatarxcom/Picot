// ABOUTME: Host control ops for @sting8k/pi-vcc (~/.pi/agent/pi-vcc-config.json,
// ABOUTME: relocatable via PI_VCC_CONFIG_PATH — the env shadow makes the
// ABOUTME: whole section read-only, fff badge pattern).
use std::path::PathBuf;

use serde_json::{json, Map, Value};

const FIELDS: &[(&str, bool)] = &[
    ("overrideDefaultCompaction", true),
    ("smartKeepTail", true),
    ("continueAfterThresholdCompact", true),
    ("debug", false),
];

fn config_file() -> Result<PathBuf, String> {
    if let Ok(relocated) = std::env::var("PI_VCC_CONFIG_PATH") {
        if !relocated.trim().is_empty() {
            return Ok(PathBuf::from(relocated));
        }
    }
    Ok(crate::pi_launch::resolve_pi_agent_root()?.join("pi-vcc-config.json"))
}

fn relocated_by_env() -> bool {
    std::env::var("PI_VCC_CONFIG_PATH")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn read_config() -> Result<Map<String, Value>, String> {
    let path = config_file()?;
    match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => {
            let parsed: Value = serde_json::from_str(&text)
                .map_err(|error| format!("config file is not valid JSON: {error}"))?;
            match parsed {
                Value::Object(map) => Ok(map),
                _ => Ok(Map::new()),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(format!("cannot read config file: {error}")),
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

pub fn get_config() -> Result<Value, String> {
    let map = read_config()?;
    // scaffoldSettings() semantics: missing keys read as package defaults.
    let mut values = Map::new();
    for (field, default) in FIELDS {
        let stored = map.get(*field).and_then(Value::as_bool);
        values.insert(field.to_string(), json!(stored.unwrap_or(*default)));
    }
    Ok(json!({
        "values": values,
        "configPath": config_file()?.to_string_lossy(),
        "relocatedByEnv": relocated_by_env(),
    }))
}

pub fn set_config(payload: &Value) -> Result<Value, String> {
    if relocated_by_env() {
        return Err(
            "config relocated by PI_VCC_CONFIG_PATH: edit the file at that path".to_string(),
        );
    }
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key is required")?;
    if !FIELDS.iter().any(|(field, _)| *field == key) {
        return Err(format!("unknown pi-vcc config key: {key}"));
    }
    let value = payload
        .get("value")
        .and_then(Value::as_bool)
        .ok_or("value must be a boolean")?;
    let mut map = read_config()?;
    map.insert(key.to_string(), json!(value));
    write_config(&map)?;
    get_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_reject_and_bools_validate() {
        assert!(set_config(&json!({"key": "nope", "value": true})).is_err());
        assert!(set_config(&json!({"key": "debug", "value": "yes"})).is_err());
        assert!(set_config(&json!({"key": "debug"})).is_err());
    }

    #[test]
    fn defaults_cover_the_four_fields() {
        assert_eq!(FIELDS.len(), 4);
        assert!(FIELDS.iter().filter(|(_, d)| *d).count() == 3);
    }
}
