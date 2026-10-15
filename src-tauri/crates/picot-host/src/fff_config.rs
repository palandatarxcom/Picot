// ABOUTME: Host-op surface for pi-fff's global config (~/.pi/agent/pi-fff.json), usable on the landing page.
// ABOUTME: Same schema discipline as fff's loadConfig: only known keys, atomic lockfile write, best-effort 0600.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use crate::host_config;
use crate::pi_launch;

const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/dmtrKovalenko/fff/main/packages/pi-fff/pi-fff.schema.json";
const CONFIG_FILE: &str = "pi-fff.json";
const FFF_MODES: [&str; 3] = ["tools-and-ui", "tools-only", "override"];

#[derive(Clone, Copy, PartialEq)]
enum FieldKind {
    Mode,
    Bool,
    Path,
}

struct FieldSpec {
    name: &'static str,
    kind: FieldKind,
    /// Env var name copied verbatim from pi-fff's getConfigValue call sites —
    /// only `mode` carries the PI_FFF_ prefix; badge text sources from here.
    env: &'static str,
}

const FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "mode",
        kind: FieldKind::Mode,
        env: "PI_FFF_MODE",
    },
    FieldSpec {
        name: "frecencyDbPath",
        kind: FieldKind::Path,
        env: "FFF_FRECENCY_DB",
    },
    FieldSpec {
        name: "historyDbPath",
        kind: FieldKind::Path,
        env: "FFF_HISTORY_DB",
    },
    FieldSpec {
        name: "enableFsRootScanning",
        kind: FieldKind::Bool,
        env: "FFF_ENABLE_ROOT_SCAN",
    },
    FieldSpec {
        name: "enableHomeDirScanning",
        kind: FieldKind::Bool,
        env: "FFF_ENABLE_HOME_SCAN",
    },
    FieldSpec {
        name: "warnOnHomeDirScan",
        kind: FieldKind::Bool,
        env: "FFF_WARN_HOME_SCAN",
    },
    FieldSpec {
        name: "followSymlinks",
        kind: FieldKind::Bool,
        env: "FFF_FOLLOW_SYMLINKS",
    },
];

fn field_spec(name: &str) -> Option<&'static FieldSpec> {
    FIELDS.iter().find(|spec| spec.name == name)
}

fn default_value(spec: &FieldSpec) -> Value {
    match spec.name {
        "mode" => json!("tools-and-ui"),
        "frecencyDbPath" | "historyDbPath" => Value::Null,
        // Root scanning stays opt-in; home scan / warn / symlinks default on.
        "enableFsRootScanning" => json!(false),
        _ => json!(true),
    }
}

fn is_known_key(key: &str) -> bool {
    key == "$schema" || field_spec(key).is_some()
}

/// fff's loadConfig validation, same failure taxonomy: unknown option, bad
/// mode, wrong types — any of these stops the extension loading.
fn validate_doc(doc: &Map<String, Value>) -> Result<(), String> {
    for key in doc.keys() {
        if !is_known_key(key) {
            return Err(format!("unknown option \"{key}\""));
        }
    }
    if let Some(mode) = doc.get("mode") {
        if !mode.is_string() || !FFF_MODES.contains(&mode.as_str().unwrap_or_default()) {
            return Err(format!("\"mode\" must be one of {}", FFF_MODES.join(", ")));
        }
    }
    for key in ["$schema", "frecencyDbPath", "historyDbPath"] {
        if let Some(value) = doc.get(key) {
            if !value.is_string() || value.as_str().unwrap_or_default().is_empty() {
                return Err(format!("\"{key}\" must be a non-empty string"));
            }
        }
    }
    for spec in FIELDS.iter().filter(|s| s.kind == FieldKind::Bool) {
        if let Some(value) = doc.get(spec.name) {
            if !value.is_boolean() {
                return Err(format!("\"{}\" must be a boolean", spec.name));
            }
        }
    }
    Ok(())
}

/// pi-fff's parse semantics: booleans accept 1/true/0/false, modes accept the
/// enum; anything else is *ignored* (falls through to the next source), never
/// an error — so a garbage env value is not a shadow.
fn parse_env(kind: FieldKind, raw: &str) -> Option<Value> {
    match kind {
        FieldKind::Bool => match raw {
            "1" | "true" => Some(json!(true)),
            "0" | "false" => Some(json!(false)),
            _ => None,
        },
        FieldKind::Mode => {
            if FFF_MODES.contains(&raw) {
                Some(json!(raw))
            } else {
                None
            }
        }
        FieldKind::Path => Some(json!(raw)),
    }
}

fn read_file(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read config: {error}")),
    };
    let parsed: Value =
        serde_json::from_str(&raw).map_err(|error| format!("not valid JSON ({error})"))?;
    let Value::Object(map) = parsed else {
        return Err("expected a JSON object".to_string());
    };
    Ok(Some(map))
}

/// Effective chain per field: flag > env > file > default. Flags are dropped
/// from host-side detection: the host constructs the embedded Pi's argv and
/// never adds `--fff-*`, while a terminal `pi`'s flags are per-instance and
/// unobservable from here — `flagShadowed` stays in the payload (always
/// empty) so the renderer shape is unchanged.
pub fn get_config() -> Value {
    let path = pi_launch::resolve_pi_agent_root()
        .unwrap_or_else(|_| default_agent_root())
        .join(CONFIG_FILE);
    get_config_at(&path, &|name| std::env::var(name).ok())
}

fn default_agent_root() -> PathBuf {
    // resolve_pi_agent_root only fails when HOME/USERPROFILE are unset, which
    // the host process already needed to start; keep a defensive fallback.
    PathBuf::from(".pi/agent")
}

fn get_config_at(path: &Path, env_lookup: &dyn Fn(&str) -> Option<String>) -> Value {
    let (doc, invalid) = match read_file(path) {
        Ok(doc) => {
            let invalid = doc.as_ref().and_then(|map| validate_doc(map).err());
            (doc, invalid)
        }
        Err(reason) => (None, Some(reason)),
    };

    let mut values = Map::new();
    let mut env_shadowed = Vec::new();
    let mut shadow_names = Map::new();
    for spec in FIELDS {
        let from_env = env_lookup(spec.env).and_then(|raw| parse_env(spec.kind, &raw));
        if let Some(env_value) = from_env {
            values.insert(spec.name.to_string(), env_value);
            env_shadowed.push(json!(spec.name));
            shadow_names.insert(spec.name.to_string(), json!(spec.env));
        } else {
            let file_value = doc.as_ref().and_then(|map| map.get(spec.name)).cloned();
            let value = file_value.unwrap_or_else(|| default_value(spec));
            values.insert(spec.name.to_string(), value);
        }
    }

    let mut payload = Map::new();
    payload.insert("values".to_string(), Value::Object(values));
    payload.insert("envShadowed".to_string(), Value::Array(env_shadowed));
    payload.insert("flagShadowed".to_string(), Value::Array(Vec::new()));
    payload.insert("shadowNames".to_string(), Value::Object(shadow_names));
    if let Some(reason) = invalid {
        payload.insert("invalid".to_string(), json!({ "reason": reason }));
    }
    Value::Object(payload)
}

/// Single-key save-on-change. The file is re-read leniently (JSON parse only)
/// and rebuilt schema-clean: only known keys with valid values carry over, so
/// unknown or wrong-typed junk is dropped and Picot can never write an invalid
/// file. A JSON-parse-broken file is never silently overwritten — the explicit
/// reset is the destructive path. `reset: true` writes the minimal
/// `$schema`-only file.
pub fn set_config(payload: &Value) -> Result<Value, String> {
    let path = pi_launch::resolve_pi_agent_root()
        .unwrap_or_else(|_| default_agent_root())
        .join(CONFIG_FILE);
    set_config_at(&path, payload)
}

fn set_config_at(path: &Path, payload: &Value) -> Result<Value, String> {
    if payload.get("reset").and_then(Value::as_bool) == Some(true) {
        let minimal = json!({ "$schema": SCHEMA_URL });
        host_config::write_json(path, &minimal).map_err(config_error_string)?;
        return Ok(json!({ "config": minimal }));
    }

    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key must be a known pi-fff option")?;
    let spec = field_spec(key).ok_or("key must be a known pi-fff option")?;

    let existing = match read_file(path) {
        Ok(None) => Map::new(),
        Ok(Some(map)) => map,
        Err(reason) => {
            return Err(format!(
                "pi-fff config is invalid — reset it first ({reason})"
            ))
        }
    };

    let mut next = Map::new();
    next.insert(
        "$schema".to_string(),
        existing
            .get("$schema")
            .cloned()
            .unwrap_or_else(|| json!(SCHEMA_URL)),
    );
    for known in FIELDS {
        if let Some(value) = existing.get(known.name) {
            if stored_value_is_valid(known.kind, value) {
                next.insert(known.name.to_string(), value.clone());
            }
        }
    }

    let value = payload.get("value");
    match value {
        None | Some(Value::Null) => {
            next.remove(spec.name);
        }
        Some(Value::String(text)) if text.is_empty() && spec.kind == FieldKind::Path => {
            // Empty path input clears the key back to the fff-managed default.
            next.remove(spec.name);
        }
        Some(candidate) => {
            let validated = validate_new_value(spec.kind, candidate)?;
            next.insert(spec.name.to_string(), validated);
        }
    }

    host_config::write_json(path, &Value::Object(next.clone())).map_err(config_error_string)?;
    Ok(json!({ "config": Value::Object(next) }))
}

/// ConfigError keeps its details private (bounded, redacted); the host op
/// surface only needs a stable code plus context for the Io variant.
fn config_error_string(error: host_config::ConfigError) -> String {
    match &error {
        host_config::ConfigError::Io(detail) => format!("{}: {detail}", error.code()),
        other => other.code().to_string(),
    }
}

fn stored_value_is_valid(kind: FieldKind, value: &Value) -> bool {
    match kind {
        FieldKind::Bool => value.is_boolean(),
        FieldKind::Mode => value.as_str().is_some_and(|mode| FFF_MODES.contains(&mode)),
        FieldKind::Path => value.as_str().is_some_and(|text| !text.is_empty()),
    }
}

fn validate_new_value(kind: FieldKind, value: &Value) -> Result<Value, String> {
    match kind {
        FieldKind::Bool => value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| "expected a boolean".to_string()),
        FieldKind::Mode => {
            let mode = value
                .as_str()
                .filter(|mode| FFF_MODES.contains(mode))
                .ok_or_else(|| format!("mode must be one of {}", FFF_MODES.join(", ")))?;
            Ok(json!(mode))
        }
        FieldKind::Path => {
            let text = value
                .as_str()
                .filter(|text| !text.is_empty())
                .ok_or_else(|| "expected a non-empty string".to_string())?;
            Ok(json!(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn write_config(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn missing_file_is_all_defaults_without_shadows() {
        let dir = tempdir().unwrap();
        let result = get_config_at(&dir.path().join(CONFIG_FILE), &no_env);
        let values = result.get("values").unwrap();
        assert_eq!(values.get("mode"), Some(&json!("tools-and-ui")));
        assert_eq!(values.get("frecencyDbPath"), Some(&Value::Null));
        assert_eq!(values.get("enableFsRootScanning"), Some(&json!(false)));
        assert_eq!(values.get("followSymlinks"), Some(&json!(true)));
        assert_eq!(result.get("envShadowed"), Some(&json!([])));
        assert_eq!(result.get("flagShadowed"), Some(&json!([])));
        assert!(result.get("invalid").is_none());
    }

    #[test]
    fn env_shadow_names_the_exact_var_and_garbage_env_is_not_a_shadow() {
        let dir = tempdir().unwrap();
        write_config(
            &dir.path().join(CONFIG_FILE),
            r#"{ "mode": "override", "followSymlinks": false }"#,
        );
        let env = |name: &str| match name {
            "FFF_ENABLE_HOME_SCAN" => Some("0".to_string()),
            "FFF_WARN_HOME_SCAN" => Some("yes".to_string()),
            _ => None,
        };
        let result = get_config_at(&dir.path().join(CONFIG_FILE), &env);
        let values = result.get("values").unwrap();
        assert_eq!(values.get("mode"), Some(&json!("override")));
        assert_eq!(values.get("enableHomeDirScanning"), Some(&json!(false)));
        assert_eq!(values.get("warnOnHomeDirScan"), Some(&json!(true)));
        assert_eq!(
            result.get("envShadowed"),
            Some(&json!(["enableHomeDirScanning"]))
        );
        assert_eq!(
            result
                .get("shadowNames")
                .unwrap()
                .get("enableHomeDirScanning"),
            Some(&json!("FFF_ENABLE_HOME_SCAN"))
        );
    }

    #[test]
    fn only_mode_carries_the_pi_fff_prefix() {
        let dir = tempdir().unwrap();
        let env = |name: &str| (name == "PI_FFF_MODE").then(|| "tools-only".to_string());
        let result = get_config_at(&dir.path().join(CONFIG_FILE), &env);
        assert_eq!(
            result.get("values").unwrap().get("mode"),
            Some(&json!("tools-only"))
        );
        assert_eq!(
            result.get("shadowNames").unwrap().get("mode"),
            Some(&json!("PI_FFF_MODE"))
        );
    }

    #[test]
    fn invalid_files_report_the_fff_failure_reason() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        write_config(&path, "{ nope");
        assert!(get_config_at(&path, &no_env)["invalid"]["reason"]
            .as_str()
            .unwrap()
            .contains("not valid JSON"));
        write_config(&path, r#"{ "stray": 1 }"#);
        assert!(get_config_at(&path, &no_env)["invalid"]["reason"]
            .as_str()
            .unwrap()
            .contains("unknown option \"stray\""));
        write_config(&path, r#"{ "mode": "bogus" }"#);
        assert!(get_config_at(&path, &no_env)["invalid"]["reason"]
            .as_str()
            .unwrap()
            .contains("\"mode\""));
        write_config(&path, r#"{ "followSymlinks": "yes" }"#);
        assert!(get_config_at(&path, &no_env)["invalid"]["reason"]
            .as_str()
            .unwrap()
            .contains("\"followSymlinks\" must be a boolean"));
    }

    #[test]
    fn single_key_set_never_solidifies_untouched_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        write_config(&path, r#"{ "enableHomeDirScanning": false }"#);
        let result = set_config_at(
            &path,
            &json!({ "key": "enableFsRootScanning", "value": true }),
        )
        .unwrap();
        let config = result.get("config").unwrap();
        assert_eq!(config.get("enableFsRootScanning"), Some(&json!(true)));
        assert_eq!(config.get("enableHomeDirScanning"), Some(&json!(false)));
        assert!(config.get("$schema").is_some());
        assert!(config.get("mode").is_none());
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.get("mode"), None);
    }

    #[test]
    fn unknown_keys_are_dropped_and_schema_is_preserved() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        write_config(
            &path,
            r#"{ "$schema": "https://custom/schema.json", "stray": "x", "mode": "tools-only" }"#,
        );
        let result =
            set_config_at(&path, &json!({ "key": "followSymlinks", "value": false })).unwrap();
        let config = result.get("config").unwrap();
        assert_eq!(
            config.get("$schema"),
            Some(&json!("https://custom/schema.json"))
        );
        assert_eq!(config.get("mode"), Some(&json!("tools-only")));
        assert!(config.get("stray").is_none());
    }

    #[test]
    fn empty_and_null_path_values_clear_the_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        write_config(&path, r#"{ "frecencyDbPath": "/old" }"#);
        set_config_at(&path, &json!({ "key": "frecencyDbPath", "value": null })).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(doc.get("frecencyDbPath").is_none());
        set_config_at(&path, &json!({ "key": "frecencyDbPath", "value": "/new" })).unwrap();
        set_config_at(&path, &json!({ "key": "frecencyDbPath", "value": "" })).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(doc.get("frecencyDbPath").is_none());
    }

    #[test]
    fn validates_key_enum_and_types() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        assert!(set_config_at(&path, &json!({ "key": "stray", "value": 1 })).is_err());
        assert!(set_config_at(&path, &json!({ "key": "mode", "value": "bogus" })).is_err());
        assert!(
            set_config_at(&path, &json!({ "key": "followSymlinks", "value": "true" })).is_err()
        );
    }

    #[test]
    fn parse_broken_file_demands_reset_first() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        write_config(&path, "{ nope");
        let error =
            set_config_at(&path, &json!({ "key": "mode", "value": "override" })).unwrap_err();
        assert!(error.contains("reset it first"));
    }

    #[test]
    fn reset_writes_the_minimal_schema_only_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        write_config(&path, r#"{ "mode": "override", "stray": true }"#);
        let result = set_config_at(&path, &json!({ "reset": true })).unwrap();
        let config = result.get("config").unwrap();
        assert_eq!(config.get("$schema").unwrap().as_str().unwrap(), SCHEMA_URL);
        assert_eq!(config.as_object().unwrap().len(), 1);
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.get("mode"), None);
    }

    #[test]
    fn writes_are_atomic_private_and_pretty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        set_config_at(&path, &json!({ "key": "mode", "value": "override" })).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("  \"mode\"")); // 2-space pretty
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
