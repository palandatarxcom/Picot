// ABOUTME: The app's daemon Tauri commands (plan D8/T10): `daemon_restart` runs the T9 restart
// ABOUTME: transaction; the status/toggle commands and every permission wire land with T10.

//! # Daemon commands (plan D8/T10)
//!
//! Three production commands are the app's whole Tauri surface (`daemon_status`,
//! `daemon_set_keep_running`, `daemon_restart`). This file owns T9's half:
//! `daemon_restart` shells into [`crate::run_restart`]'s transaction.
//!
//! Two rules are load-bearing and are already enforced here, because they are
//! caller-validation rules rather than UI concerns:
//!
//! 1. **The caller is injected by Tauri** — the command takes the calling
//!    `WebviewWindow` — and its label must appear in the app's own window
//!    ledger. A caller-supplied identity field is never trusted, and the check
//!    never relies on `windows:["*"]` / `core:default` wildcards.
//! 2. **The transaction owns the daemon** — nothing here stops, spawns or
//!    navigates anything directly; it only starts the coordinator and formats
//!    its report for the UI.
//!
//! T10 adds the `daemon_status` / `daemon_set_keep_running` commands, the
//! origin/binding half of the caller validation, and `permissions/default.toml`
//! plus `generate_handler!` wiring. Until then this command is unregistered
//! (marked `#[allow(dead_code)]`), which is exactly why the permission checker
//! stays green: it requires every *registered* command to be declared.

use crate::daemon_client::restart::RestartReport;
use crate::{run_restart, DaemonLink, WindowLedger};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

/// Restart the Picot daemon through the app-managed replacement transaction
/// (plan Task 9 / D10): the caller gets the new status, or the failure stage.
#[allow(dead_code)] // registered by T10, with its permission wiring
#[tauri::command]
pub async fn daemon_restart(window: tauri::WebviewWindow, app: AppHandle) -> Result<Value, String> {
    validate_caller(&app, window.label())?;
    let report = run_restart(&app).await?;
    Ok(report_json(&report))
}

/// The caller-labelled window must be one this app created, and the app must
/// actually have a daemon link to restart.
fn validate_caller(app: &AppHandle, label: &str) -> Result<(), String> {
    let ledger = app
        .try_state::<WindowLedger>()
        .ok_or_else(|| "Picot's window ledger is not available".to_string())?;
    if !ledger.contains_label(label) {
        return Err(format!("window {label} is not a Picot window"));
    }
    if app.try_state::<DaemonLink>().is_none() {
        return Err("Picot's daemon link is not available".to_string());
    }
    Ok(())
}

/// The status refresh the UI renders (step 7): the replacement's identity, the
/// new origin, and one row per migrated view. Capabilities are deliberately not
/// part of this payload — they belong in the WebView's init script only.
fn report_json(report: &RestartReport) -> Value {
    json!({
        "pid": report.pid,
        "hostOrigin": report.host_origin,
        "version": report.version,
        "buildId": report.build_id,
        "epoch": report.epoch,
        "oldPid": report.old_pid,
        "keepRunningAfterQuit": report.keep_running_after_quit,
        "views": report
            .views
            .iter()
            .map(|view| json!({
                "label": view.label,
                "owner": view.owner,
                "url": view.url,
                "degraded": view.degraded,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The migration report the UI gets never carries a capability: that is
    /// authorization state for the WebView, not for a settings panel.
    #[test]
    fn the_report_payload_has_no_capability_field() {
        let report = RestartReport {
            pid: 42,
            host_origin: "http://127.0.0.1:6001".to_string(),
            version: "0.5.0".to_string(),
            build_id: "build-2".to_string(),
            epoch: 3,
            old_exit: None,
            old_pid: None,
            views: vec![crate::daemon_client::restart::ViewMigration {
                label: "native-landing".to_string(),
                owner: "owner-l".to_string(),
                capability: "secret-capability".to_string(),
                url: "http://127.0.0.1:6001/".to_string(),
                workspace_id: None,
                degraded: false,
            }],
            keep_running_after_quit: true,
        };
        let encoded = report_json(&report).to_string();
        assert!(!encoded.contains("secret-capability"), "{encoded}");
        assert!(!encoded.contains("capability"), "{encoded}");
        let payload = report_json(&report);
        assert_eq!(payload["pid"], 42);
        assert_eq!(payload["hostOrigin"], "http://127.0.0.1:6001");
        assert_eq!(payload["views"][0]["label"], "native-landing");
        assert_eq!(payload["keepRunningAfterQuit"], true);
    }
}
