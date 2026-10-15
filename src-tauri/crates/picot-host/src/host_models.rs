// ABOUTME: Host-side Pi settings.json object IO shared across windows.
// ABOUTME: The static provider catalog / auth.json / health-check surfaces
// ABOUTME: retired here moved to the Pi `/picot-config` bridge (live registry);
// ABOUTME: the landing page removed the host-wide model cache (no cold-start
// ABOUTME: runtime — the dropdown's live path is the runtime request).

use serde_json::{Map, Value};
use std::path::Path;

fn write_json_atomically(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create config dir: {error}"))?;
    }
    let tmp = path.with_extension("picot-tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(value).unwrap_or_default(),
    )
    .map_err(|error| format!("Cannot write config: {error}"))?;
    std::fs::rename(&tmp, path).map_err(|error| format!("Cannot replace config: {error}"))
}

/// Read a Pi settings JSON file as an object; missing/unparsable files
/// yield an empty object (never a hard failure for UI surfaces).
pub fn read_settings_object(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// Atomically replace a Pi settings JSON file, preserving unknown keys.
pub fn write_settings_object(path: &Path, value: &Value) -> Result<(), String> {
    write_json_atomically(path, value)
}
