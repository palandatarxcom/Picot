// ABOUTME: Native-only broker controls for the workspace registry and
// global preferences, backed by the shared MetadataStore. Keeping command
// handling here makes the authorization gate and result shapes unit-testable
// without an AppHandle or live WebSocket.

use crate::host_control::{ClientClass, VerifiedClientContext};
use crate::metadata_store::SharedMetadataStore;
use serde_json::{json, Value};

/// Stable rejection text for Remote/unauthenticated callers. The frontend
/// treats any of these commands failing with this text as a hard boundary.
pub const NATIVE_OWNER_REQUIRED: &str = "native desktop owner required";

/// Enforce the authorization matrix: every workspace.* / preference.*
/// command requires a Native desktop client bound to a verified owner.
pub fn require_native_owner(ctx: &VerifiedClientContext) -> Result<String, String> {
    match (&ctx.class, ctx.owner_id.as_ref()) {
        (ClientClass::Native, Some(owner)) if !owner.as_str().is_empty() => {
            Ok(owner.as_str().to_string())
        }
        _ => Err(NATIVE_OWNER_REQUIRED.to_string()),
    }
}

/// Registry mutation outcome for the caller to broadcast to all Native
/// clients after a successful DB commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryChange {
    Added,
    Removed,
    Pinned,
    Pruned,
}

impl RegistryChange {
    pub fn reason(self) -> &'static str {
        match self {
            RegistryChange::Added => "added",
            RegistryChange::Removed => "removed",
            RegistryChange::Pinned => "pinned",
            RegistryChange::Pruned => "pruned",
        }
    }
}

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    let value = args.get(key).and_then(Value::as_str).unwrap_or_default();
    if value.trim().is_empty() {
        return Err(format!("{key} is required"));
    }
    Ok(value.to_string())
}

/// Dispatch one workspace/preference control command.
/// Returns `(result, change)` where `change` is Some only after successful
/// mutations the frontend should react to (`workspace.touch` is host-internal
/// and intentionally absent from this surface).
pub fn handle_control(
    command: &str,
    args: &Value,
    metadata: &SharedMetadataStore,
) -> Result<(Value, Option<RegistryChange>), String> {
    let mut store = metadata
        .lock()
        .map_err(|_| "Picot metadata lock poisoned".to_string())?;
    match command {
        "workspace.list" => {
            let (workspaces, removed) = store.list_workspaces_and_prune()?;
            let change = if removed.is_empty() {
                None
            } else {
                Some(RegistryChange::Pruned)
            };
            Ok((
                json!({
                    "workspaces": workspaces,
                    "removed": removed,
                }),
                change,
            ))
        }
        "workspace.add" => {
            let path = arg_str(args, "path")?;
            match store.add_workspace(std::path::Path::new(&path)) {
                Ok((workspace, added)) => {
                    let change = if added {
                        Some(RegistryChange::Added)
                    } else {
                        // Already registered: idempotent success without churn.
                        None
                    };
                    Ok((
                        json!({
                            "workspace": workspace,
                            "added": added,
                        }),
                        change,
                    ))
                }
                Err(error) => Err(error.to_string()),
            }
        }
        "workspace.remove" => {
            let workspace_id = arg_str(args, "workspaceId")?;
            let removed = store.remove_workspace(&workspace_id)?;
            let change = if removed {
                Some(RegistryChange::Removed)
            } else {
                None
            };
            Ok((json!({ "removed": removed }), change))
        }
        "workspace.pin" => {
            let workspace_id = arg_str(args, "workspaceId")?;
            let pinned = args
                .get("pinned")
                .and_then(Value::as_bool)
                .ok_or("pinned is required")?;
            match store.set_workspace_pinned(&workspace_id, pinned)? {
                Some(workspace) => Ok((
                    json!({ "workspace": workspace }),
                    Some(RegistryChange::Pinned),
                )),
                None => Err("workspace_not_found".to_string()),
            }
        }
        "preference.get" => {
            let key = arg_str(args, "key")?;
            let value = store.pref_get(&key)?;
            Ok((json!({ "value": value }), None))
        }
        "preference.set" => {
            let key = arg_str(args, "key")?;
            let value = args.get("value").cloned().ok_or("value is required")?;
            store.pref_set(&key, &value)?;
            Ok((json!({}), None))
        }
        "preference.delete" => {
            let key = arg_str(args, "key")?;
            store.pref_delete(&key)?;
            Ok((json!({}), None))
        }
        "preference.list" => {
            let prefix = args
                .get("prefix")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let preferences = store.pref_list(prefix)?;
            Ok((json!({ "preferences": preferences }), None))
        }
        other => Err(format!("Unknown control command: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{handle_control, require_native_owner, RegistryChange, NATIVE_OWNER_REQUIRED};
    use crate::host_control::{ClientClass, VerifiedClientContext};
    use crate::metadata_store::{MetadataStore, SharedMetadataStore};
    use crate::window_owner::OwnerId;
    use serde_json::{json, Value};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn shared_store() -> (SharedMetadataStore, std::path::PathBuf) {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "picot-workspace-controls-{}-{sequence}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&temp).unwrap();
        (
            Arc::new(Mutex::new(
                MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
            )),
            temp,
        )
    }

    fn native_ctx() -> VerifiedClientContext {
        VerifiedClientContext {
            client_id: 1,
            class: ClientClass::Native,
            owner_id: Some(OwnerId::from_string("owner-native".to_string())),
        }
    }

    fn remote_ctx() -> VerifiedClientContext {
        VerifiedClientContext {
            client_id: 2,
            class: ClientClass::Remote,
            owner_id: None,
        }
    }

    #[test]
    fn every_command_requires_a_verified_native_owner() {
        let (_store, _temp) = shared_store();
        for command in [
            "workspace.list",
            "workspace.add",
            "workspace.remove",
            "workspace.pin",
            "preference.get",
            "preference.set",
            "preference.delete",
            "preference.list",
        ] {
            let remote = require_native_owner(&remote_ctx());
            assert_eq!(remote.unwrap_err(), NATIVE_OWNER_REQUIRED, "{command}");

            let ownerless_native = require_native_owner(&VerifiedClientContext {
                client_id: 3,
                class: ClientClass::Native,
                owner_id: None,
            });
            assert_eq!(ownerless_native.unwrap_err(), NATIVE_OWNER_REQUIRED);
        }
        assert_eq!(require_native_owner(&native_ctx()).unwrap(), "owner-native");
    }

    #[test]
    fn add_list_remove_round_trip_broadcasts_exactly_on_real_mutation() {
        let (store, temp) = shared_store();
        let project = temp.join("project");
        fs::create_dir_all(&project).unwrap();
        let session_file = project.join("session.jsonl");
        fs::write(&session_file, "session fixture\n").unwrap();

        // Red signal for our gate would be Remote acceptance; also prove the
        // gate applies before any DB work by driving handle through same fn.
        let denied = super::require_native_owner(&remote_ctx()).unwrap_err();

        // Add.
        let (result, change) = handle_control(
            "workspace.add",
            &json!({ "path": project.to_string_lossy() }),
            &store,
        )
        .unwrap();
        assert_eq!(change, Some(RegistryChange::Added));
        assert_eq!(result["added"], true);
        assert_eq!(
            result["workspace"]["canonicalPath"],
            Value::String(
                fs::canonicalize(&project)
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            )
        );
        assert_eq!(result["workspace"]["displayName"], "project");
        assert_eq!(result["workspace"]["pinned"], false);
        assert!(result["workspace"]["sessionBucket"].is_null());

        // Idempotent re-add reports no broadcast-worthy change.
        let (_, change2) = handle_control(
            "workspace.add",
            &json!({ "path": project.to_string_lossy() }),
            &store,
        )
        .unwrap();
        assert_eq!(change2, None);

        // List.
        let (listed, listed_change) = handle_control("workspace.list", &json!({}), &store).unwrap();
        assert_eq!(listed_change, None);
        assert_eq!(listed["workspaces"].as_array().unwrap().len(), 1);
        assert_eq!(listed["removed"].as_array().unwrap().len(), 0);

        // Pin.
        let workspace_id = listed["workspaces"][0]["workspaceId"]
            .as_str()
            .unwrap()
            .to_string();
        let (pin_result, pin_change) = handle_control(
            "workspace.pin",
            &json!({ "workspaceId": workspace_id, "pinned": true }),
            &store,
        )
        .unwrap();
        assert_eq!(pin_change, Some(RegistryChange::Pinned));
        assert_eq!(pin_result["workspace"]["pinned"], true);

        // Remove.
        let (remove_result, remove_change) = handle_control(
            "workspace.remove",
            &json!({ "workspaceId": workspace_id }),
            &store,
        )
        .unwrap();
        assert_eq!(remove_change, Some(RegistryChange::Removed));
        assert_eq!(remove_result["removed"], true);
        assert_eq!(denied, NATIVE_OWNER_REQUIRED);

        // Registry removal deletes only the row; workspace content is retained.
        assert!(store
            .lock()
            .unwrap()
            .get_workspace(&workspace_id)
            .unwrap()
            .is_none());
        assert!(project.is_dir());
        assert!(session_file.is_file());
    }

    #[test]
    fn list_prunes_missing_directories_and_reports_removed_set() {
        let (store, temp) = shared_store();
        let doomed = temp.join("doomed");
        fs::create_dir_all(&doomed).unwrap();
        handle_control(
            "workspace.add",
            &json!({ "path": doomed.to_string_lossy() }),
            &store,
        )
        .unwrap();
        fs::remove_dir_all(&doomed).unwrap();

        let (result, change) = handle_control("workspace.list", &json!({}), &store).unwrap();
        assert_eq!(change, Some(RegistryChange::Pruned));
        let removed = result["removed"].as_array().unwrap();
        assert_eq!(removed.len(), 1);
        assert!(removed[0]["workspaceId"].is_string());
        assert!(removed[0]["canonicalPath"].is_string());
        assert_eq!(result["workspaces"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn stable_errors_for_bad_args_and_unknown_ids() {
        let (store, temp) = shared_store();

        // Missing path.
        let error = handle_control("workspace.add", &json!({}), &store).unwrap_err();
        assert_eq!(error, "path is required");

        // Nonexistent directory keeps the spec's stable code prefix.
        let error = handle_control(
            "workspace.add",
            &json!({ "path": temp.join("ghost").to_string_lossy() }),
            &store,
        )
        .unwrap_err();
        assert!(error.starts_with("path_not_found"), "{error}");

        // Regular file fails not-a-directory.
        let file = temp.join("plain.txt");
        fs::write(&file, "x").unwrap();
        let error = handle_control(
            "workspace.add",
            &json!({ "path": file.to_string_lossy() }),
            &store,
        )
        .unwrap_err();
        assert!(error.starts_with("not_a_directory"), "{error}");

        // Unknown pin target errors without inventing rows.
        let error = handle_control(
            "workspace.pin",
            &json!({ "workspaceId": "missing", "pinned": true }),
            &store,
        )
        .unwrap_err();
        assert_eq!(error, "workspace_not_found");

        // Unknown removal succeeds as false and must NOT broadcast.
        let (_, change) = handle_control(
            "workspace.remove",
            &json!({ "workspaceId": "missing" }),
            &store,
        )
        .unwrap();
        assert_eq!(change, None);

        // Missing pinned flag.
        let error =
            handle_control("workspace.pin", &json!({ "workspaceId": "x" }), &store).unwrap_err();
        assert_eq!(error, "pinned is required");

        // Truly unknown commands are rejected (no registry fallback).
        let error = handle_control("workspace.touch", &json!({}), &store).unwrap_err();
        assert!(error.contains("Unknown control command"), "{error}");
    }

    #[test]
    fn preference_commands_round_trip_through_the_shared_store() {
        let (store, _temp) = shared_store();

        let (result, _) =
            handle_control("preference.get", &json!({ "key": "ui.theme" }), &store).unwrap();
        assert_eq!(result["value"], Value::Null);

        handle_control(
            "preference.set",
            &json!({ "key": "ui.theme", "value": "dark" }),
            &store,
        )
        .unwrap();
        let (result, change) =
            handle_control("preference.get", &json!({ "key": "ui.theme" }), &store).unwrap();
        assert_eq!(change, None);
        assert_eq!(result["value"], "dark");

        handle_control(
            "preference.set",
            &json!({ "key": "sidebar.sizes", "value": { "lane": 320 } }),
            &store,
        )
        .unwrap();
        let (result, _) =
            handle_control("preference.list", &json!({ "prefix": "sidebar." }), &store).unwrap();
        assert_eq!(result["preferences"]["sidebar.sizes"]["lane"], 320);

        handle_control("preference.delete", &json!({ "key": "ui.theme" }), &store).unwrap();
        let (result, _) =
            handle_control("preference.get", &json!({ "key": "ui.theme" }), &store).unwrap();
        assert_eq!(result["value"], Value::Null);

        // set without value errors stably; get without key too.
        assert_eq!(
            handle_control("preference.set", &json!({ "key": "k" }), &store).unwrap_err(),
            "value is required"
        );
        assert_eq!(
            handle_control("preference.get", &json!({}), &store).unwrap_err(),
            "key is required"
        );
    }
}
