// ABOUTME: The app-side notification-click executor (multi-window §7): snapshot the
// windows, pick one target, execute focus / navigate / open.

//! # Notification-click executor (multi-window §7, plan T3)
//!
//! The daemon forwards a clicked notification's payload here (the admin
//! channel's `notification_activated` op) beside the broadcast every client
//! still receives. This module is the single decision point the spec calls for:
//! it snapshots the app's own window facts — the [`WindowLedger`] rows, each
//! window's **live** route (SPA session switches are only visible in the
//! document URL, the same reason [`crate::capture_open_views`] reads it) and
//! the [`FocusTimes`] stamps — hands them to
//! [`pick_notification_target`](super::notification_routing::pick_notification_target),
//! and executes the pick.
//!
//! Execution maps each pick to one existing path, nothing new:
//!
//! - `FocusShowing` — `set_focus` on the window already showing the session.
//! - `FocusWorkspace` — `set_focus` plus an in-window navigation through the
//!   same `handleSessionSelect` flow the sidebar uses, delivered as a DOM
//!   event over `window.eval` (the established Rust→WebView channel the menu
//!   handlers use).
//! - `OpenNew` — the §6.2 `open_workspace` reuse chain, driven from a live
//!   window's `transport.openWorkspace` (the production caller T2 landed), so
//!   spawn/rebind/focus-existing stays in the one place that already owns it.
//!
//! The tests exercise the pieces that decide: the snapshot assembly and the
//! three actions, against stubs; the Tauri-facing glue stays thin.

use serde_json::Value;
use tauri::Manager;

use super::notification_routing::{
    pick_notification_target, FocusTimes, NotificationPayload, NotificationTarget,
    WindowRoutingState,
};
use super::window_ledger::{route_for_url, LedgerRow};
use crate::{DaemonLink, WindowLedger};

/// The executor-facing half of the forwarded payload: the identity the pick
/// compares with, plus the file arguments the navigation and the open chain
/// need (`sessionFile` is the persistent identity those paths speak).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationClick {
    pub identity: NotificationPayload,
    pub session_file: String,
    pub cwd: Option<String>,
}

impl NotificationClick {
    /// Parse the daemon's payload; `None` when it lacks the identity half
    /// (the daemon never forwards one, so this is a malformed-payload guard).
    pub fn from_payload(payload: &Value) -> Option<Self> {
        Some(Self {
            identity: NotificationPayload {
                workspace_id: payload.get("workspaceId")?.as_str()?.to_string(),
                session_id: payload.get("sessionId")?.as_str()?.to_string(),
            },
            session_file: payload.get("sessionFile")?.as_str()?.to_string(),
            cwd: payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

/// Snapshot every ledger row as a routing state: the live route wins (the
/// document URL sees SPA switches no mirror event does), the row's recorded
/// route is the fallback, and the row's workspace binding patches a window
/// whose route names none (a settings page inside a workspace window).
pub fn routing_states(
    rows: &[LedgerRow],
    live_route: impl Fn(&str) -> Option<String>,
    focus: &FocusTimes,
    alive: impl Fn(&str) -> bool,
) -> Vec<WindowRoutingState> {
    rows.iter()
        .map(|row| {
            let route = live_route(&row.label).or_else(|| row.route.clone());
            let mut state = WindowRoutingState::from_route(
                &row.label,
                route.as_deref(),
                focus.focused_at(&row.label),
                alive(&row.label),
            );
            if state.workspace_id.is_none() {
                state.workspace_id = row.workspace_id.clone();
            }
            state
        })
        .collect()
}

/// The three actions a pick maps to (spec §7), behind one trait so the
/// dispatch is testable without an `AppHandle`.
#[async_trait::async_trait]
pub trait ClickActions: Send + Sync {
    async fn focus_window(&self, label: &str) -> Result<(), String>;
    async fn navigate_to_session(
        &self,
        label: &str,
        click: &NotificationClick,
    ) -> Result<(), String>;
    async fn open_workspace(&self, click: &NotificationClick) -> Result<(), String>;
}

/// Dispatch the pick to its action. Rule order already happened in the pick;
/// this only executes, and surfaces failures to the (logging) caller.
pub async fn execute_notification_target(
    target: &NotificationTarget,
    click: &NotificationClick,
    actions: &dyn ClickActions,
) -> Result<(), String> {
    match target {
        NotificationTarget::FocusShowing { label } => actions.focus_window(label).await,
        NotificationTarget::FocusWorkspace { label } => {
            actions.focus_window(label).await?;
            actions.navigate_to_session(label, click).await
        }
        NotificationTarget::OpenNew => actions.open_workspace(click).await,
    }
}

/// One `window.eval` DOM dispatch — the established Rust→WebView channel.
fn eval_event(window: &tauri::WebviewWindow, event: &str, detail: &Value) -> Result<(), String> {
    let detail = serde_json::to_string(detail)
        .map_err(|error| format!("cannot encode the notification detail: {error}"))?;
    window
        .eval(format!(
            "document.dispatchEvent(new CustomEvent('{event}', {{detail: {detail}}}))"
        ))
        .map_err(|error| format!("cannot reach the window: {error}"))
}

/// The production actions against the real windows.
struct TauriClickActions<'a> {
    app: &'a tauri::AppHandle,
}

#[async_trait::async_trait]
impl ClickActions for TauriClickActions<'_> {
    async fn focus_window(&self, label: &str) -> Result<(), String> {
        let window = self
            .app
            .get_webview_window(label)
            .ok_or_else(|| format!("no window found for {label}"))?;
        window.set_focus().map_err(|error| error.to_string())
    }

    async fn navigate_to_session(
        &self,
        label: &str,
        click: &NotificationClick,
    ) -> Result<(), String> {
        let window = self
            .app
            .get_webview_window(label)
            .ok_or_else(|| format!("no window found for {label}"))?;
        eval_event(
            &window,
            "picot:notification-navigate",
            &serde_json::json!({
                "sessionFile": click.session_file,
                "cwd": click.cwd,
            }),
        )
    }

    async fn open_workspace(&self, click: &NotificationClick) -> Result<(), String> {
        // Rule 3 rides the production open chain: any live window's
        // `transport.openWorkspace` drives the daemon's §6.2 reuse/focus
        // guards. The carrier is invisible to the user (the window the chain
        // opens takes the stage), so the first live window — deterministic —
        // is enough.
        let Some(window) = self
            .app
            .webview_windows()
            .into_values()
            .find(|window| window.url().is_ok())
        else {
            return Err("no live window can carry the open request".to_string());
        };
        let cwd = click
            .cwd
            .as_deref()
            .ok_or_else(|| "the notification carries no cwd to open".to_string())?;
        eval_event(
            &window,
            "picot:notification-open",
            &serde_json::json!({
                "cwd": cwd,
                "sessionFile": click.session_file,
            }),
        )
    }
}

/// The single decision point (multi-window §7): snapshot, pick, execute.
pub async fn route_click(app: &tauri::AppHandle, payload: &Value) -> Result<(), String> {
    let Some(link) = app.try_state::<DaemonLink>() else {
        return Err("Picot is not attached to the daemon".to_string());
    };
    let Some(ledger) = app.try_state::<WindowLedger>() else {
        return Err("the window ledger is not available".to_string());
    };
    let Some(focus_times) = app.try_state::<FocusTimes>() else {
        return Err("focus tracking is not available".to_string());
    };
    let click = NotificationClick::from_payload(payload)
        .ok_or_else(|| "the notification payload has no session identity".to_string())?;
    let host_origin = link.host_origin();
    let rows = ledger.rows();
    let states = routing_states(
        &rows,
        |label| {
            app.get_webview_window(label)
                .and_then(|window| window.url().ok())
                .and_then(|url| route_for_url(url.as_str(), &host_origin))
        },
        &focus_times,
        |label| app.get_webview_window(label).is_some(),
    );
    let target = pick_notification_target(&states, &click.identity);
    execute_notification_target(&target, &click, &TauriClickActions { app }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn row(label: &str, workspace: Option<&str>, route: Option<&str>) -> LedgerRow {
        LedgerRow {
            label: label.to_string(),
            owner: "owner-x".to_string(),
            owner_home: None,
            port: 0,
            workspace_id: workspace.map(str::to_string),
            route: route.map(str::to_string),
        }
    }

    #[test]
    fn routing_states_prefer_the_live_route_and_patch_the_workspace_binding() {
        let focus = FocusTimes::new();
        focus.record("w1");
        let rows = [
            row(
                "w1",
                Some("ledger-w1"),
                Some("/workspaces/ledger-w1/sessions/old"),
            ),
            row("w1-settings", Some("ledger-w1"), None),
            row("native-landing", None, None),
        ];
        let states = routing_states(
            &rows,
            |label| (label == "w1").then(|| "/workspaces/live-w1/sessions/live-s".to_string()),
            &focus,
            |label| label != "w1-settings",
        );
        // Live route wins over both the recorded route and the row's binding.
        assert_eq!(states[0].workspace_id.as_deref(), Some("live-w1"));
        assert_eq!(states[0].session_id.as_deref(), Some("live-s"));
        assert!(states[0].alive);
        assert!(states[0].focused_at.is_some());
        // A window whose route names no workspace still serves rule 2 through
        // its ledger binding; a dead one is marked.
        assert_eq!(states[1].workspace_id.as_deref(), Some("ledger-w1"));
        assert!(!states[1].alive);
        assert_eq!(states[2].workspace_id, None);
    }

    #[test]
    fn the_click_payload_parses_identity_file_and_cwd() {
        let click = NotificationClick::from_payload(&json!({
            "type": "notification_activated",
            "workspaceId": "w1",
            "sessionId": "s1",
            "sessionFile": "/tmp/s1.jsonl",
            "cwd": "/tmp/proj",
        }))
        .expect("a full payload parses");
        assert_eq!(click.identity.workspace_id, "w1");
        assert_eq!(click.identity.session_id, "s1");
        assert_eq!(click.session_file, "/tmp/s1.jsonl");
        assert_eq!(click.cwd.as_deref(), Some("/tmp/proj"));

        assert!(
            NotificationClick::from_payload(&json!({"workspaceId": "w1"})).is_none(),
            "a payload without the session identity is refused"
        );
    }

    /// Records the actions the executor ran.
    #[derive(Default)]
    struct RecordingActions {
        calls: Mutex<Vec<(String, Value)>>,
        fail_focus: bool,
    }

    #[async_trait::async_trait]
    impl ClickActions for RecordingActions {
        async fn focus_window(&self, label: &str) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(("focus".into(), json!(label)));
            if self.fail_focus {
                return Err("focus refused".into());
            }
            Ok(())
        }

        async fn navigate_to_session(
            &self,
            label: &str,
            click: &NotificationClick,
        ) -> Result<(), String> {
            self.calls.lock().unwrap().push((
                "navigate".into(),
                json!([label, click.session_file, click.cwd]),
            ));
            Ok(())
        }

        async fn open_workspace(&self, click: &NotificationClick) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(("open".into(), json!([click.session_file, click.cwd])));
            Ok(())
        }
    }

    fn click() -> NotificationClick {
        NotificationClick {
            identity: NotificationPayload {
                workspace_id: "w1".into(),
                session_id: "s1".into(),
            },
            session_file: "/tmp/s1.jsonl".into(),
            cwd: Some("/tmp/proj".into()),
        }
    }

    #[tokio::test]
    async fn focus_showing_only_focuses() {
        let actions = RecordingActions::default();
        execute_notification_target(
            &NotificationTarget::FocusShowing { label: "w1".into() },
            &click(),
            &actions,
        )
        .await
        .expect("the action completes");
        assert_eq!(
            *actions.calls.lock().unwrap(),
            vec![("focus".into(), json!("w1"))],
            "rule 1 must not navigate a window that already shows the session"
        );
    }

    #[tokio::test]
    async fn focus_workspace_focuses_then_navigates_in_window() {
        let actions = RecordingActions::default();
        execute_notification_target(
            &NotificationTarget::FocusWorkspace { label: "w1".into() },
            &click(),
            &actions,
        )
        .await
        .expect("the action completes");
        assert_eq!(
            *actions.calls.lock().unwrap(),
            vec![
                ("focus".into(), json!("w1")),
                (
                    "navigate".into(),
                    json!(["w1", "/tmp/s1.jsonl", "/tmp/proj"])
                ),
            ],
            "rule 2 focuses the workspace's window and navigates it in-window"
        );
    }

    #[tokio::test]
    async fn open_new_rides_the_open_chain() {
        let actions = RecordingActions::default();
        execute_notification_target(&NotificationTarget::OpenNew, &click(), &actions)
            .await
            .expect("the action completes");
        assert_eq!(
            *actions.calls.lock().unwrap(),
            vec![("open".into(), json!(["/tmp/s1.jsonl", "/tmp/proj"]))],
            "rule 3 opens through the reuse chain"
        );
    }

    #[tokio::test]
    async fn a_focus_failure_stops_before_the_navigation() {
        let actions = RecordingActions {
            fail_focus: true,
            ..RecordingActions::default()
        };
        let failure = execute_notification_target(
            &NotificationTarget::FocusWorkspace {
                label: "gone".into(),
            },
            &click(),
            &actions,
        )
        .await
        .expect_err("a refused focus surfaces");
        assert_eq!(failure, "focus refused");
        assert_eq!(
            actions.calls.lock().unwrap().len(),
            1,
            "no navigation may follow a failed focus"
        );
    }
}
