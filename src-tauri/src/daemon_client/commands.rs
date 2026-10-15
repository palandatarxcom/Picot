// ABOUTME: The app's three daemon Tauri commands (plan D8/T10) — status, quit-policy toggle and
// ABOUTME: the T9 restart transaction — plus the caller check every one of them runs first.

//! # Daemon commands (plan D8/T10)
//!
//! Three production commands are the app's whole Tauri surface:
//!
//! | command | what it does |
//! | --- | --- |
//! | `daemon_status` | the settings section's state: the daemon's own facts plus the app-private `keepRunningAfterQuit` (plan T10: the policy is merged app-side, which is the R2 get-gap) |
//! | `daemon_set_keep_running` | writes the policy file and **nothing else** — no probe, no request, no restart (spec §3.2.2) |
//! | `daemon_restart` | shells into [`crate::run_restart`]'s T9 transaction |
//!
//! Two rules are load-bearing, because they are caller-validation rules rather
//! than UI concerns:
//!
//! 1. **The caller is injected by Tauri** — every command takes the calling
//!    `WebviewWindow` — and the check is [`validate_caller`]: the label must be
//!    one *this app* created (the T9 [`WindowLedger`]), the label must have an
//!    owner binding in the daemon-fed [`NavigationMirror`], that binding must be
//!    this app's own owner for that label, and the window's **live document
//!    origin** must be the origin the daemon committed for it. A dropped admin
//!    channel clears the mirror, so the commands fail closed exactly when the
//!    app cannot tell what the daemon authorized. No caller-supplied identity
//!    field is ever trusted, and nothing here relies on `windows:["*"]` /
//!    `core:default` wildcards (the permission wiring is explicit —
//!    `permissions/default.toml`).
//! 2. **The transaction owns the daemon** — nothing here stops, spawns or
//!    navigates anything directly; it only starts the coordinator and formats
//!    its report for the UI.
//!
//! The caller check is a pure function over the two injected states so it is
//! unit-testable: a `WebviewWindow` cannot be constructed without a running
//! Tauri app, so the testable seam is the *decision*, and [`CommandCaller::from_window`]
//! is the two-line adapter Tauri's injection feeds it.

//! The three command names appear in `main.rs`'s `generate_handler!` block as
//! bare identifiers (`use crate::daemon_client::commands::{...}` at the top of
//! `main.rs`), because `scripts/check-tauri-permissions.js` re-parses that block
//! textually and cross-checks the names against `permissions/default.toml`.

use crate::daemon_client::navigation_mirror::NavigationMirror;
use crate::daemon_client::{quit_policy, restart::RestartReport};
use crate::{run_restart, DaemonLink, WindowLedger};
use picot_host::window_owner::navigation_origin;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Url};

/// The facts the caller check reads from the calling window.
///
/// Tauri injects the real `WebviewWindow`; this struct is the testable half of
/// that injection (label + live document URL), so the decision can be asserted
/// without a running app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandCaller {
    pub label: String,
    /// The window's current document URL. `None` when Tauri cannot report one
    /// (a destroyed/undecided webview): refused, never assumed ours.
    pub url: Option<String>,
}

impl CommandCaller {
    pub fn from_window(window: &tauri::WebviewWindow) -> Self {
        Self {
            label: window.label().to_string(),
            url: window.url().ok().map(|url| url.to_string()),
        }
    }
}

/// The whole caller check (plan D8/T10): a label this app created, bound in the
/// daemon-fed mirror to the same owner, presenting the origin the daemon
/// committed for it.
///
/// Order matters for the message, not for the verdict: every failure is a
/// refusal. The ledger answers "did this app create that window"; the mirror
/// answers "does the daemon still authorize that label, for this origin".
pub fn validate_caller(
    ledger: &WindowLedger,
    mirror: &NavigationMirror,
    caller: &CommandCaller,
) -> Result<(), String> {
    let label = caller.label.as_str();
    let row = ledger
        .row_for_label(label)
        .ok_or_else(|| format!("window {label} is not a Picot window"))?;
    let url = caller
        .url
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .ok_or_else(|| format!("window {label} has no readable document URL"))?;
    let origin = navigation_origin(&url)
        .ok_or_else(|| format!("window {label} is not on a loopback host origin"))?;
    let binding = mirror.owner_row_for_label(label).ok_or_else(|| {
        format!("window {label} has no owner binding; Picot is not attached to the daemon")
    })?;
    if binding.owner != row.owner {
        return Err(format!(
            "window {label} is bound to an owner this app did not create"
        ));
    }
    if binding.current_origin != origin {
        return Err(format!(
            "window {label} is on {origin}, not on the origin the daemon committed ({})",
            binding.current_origin
        ));
    }
    Ok(())
}

/// The settings section's state: the daemon's own facts plus the app-private
/// quit policy, merged app-side (plan T10 / spec §3.2.2 — the daemon never
/// knows the policy, so only the app can answer this one).
///
/// A daemon that cannot answer is a *fact*, not an error: the reply is
/// `{running:false, reason}` so the panel can say "stopped" instead of showing
/// a failed command. Only a caller/state problem is an `Err`.
#[tauri::command]
pub async fn daemon_status(window: tauri::WebviewWindow, app: AppHandle) -> Result<Value, String> {
    let link = caller_link(&app, &window)?;
    let keep_running_after_quit = quit_policy::keep_running_after_quit(&link.config_dir);
    let status = link.client().request("daemon.status", json!({})).await;
    Ok(status_payload(keep_running_after_quit, status))
}

/// Persist the quit policy. Writes the app-private file and **nothing else**:
/// the toggle must not restart, pause or even probe the daemon (spec §3.2.2) —
/// the next quit is what reads the new value.
#[tauri::command]
pub async fn daemon_set_keep_running(
    window: tauri::WebviewWindow,
    app: AppHandle,
    value: bool,
) -> Result<(), String> {
    let link = caller_link(&app, &window)?;
    quit_policy::set_keep_running_after_quit(&link.config_dir, value)
}

/// Restart the Picot daemon through the app-managed replacement transaction
/// (plan Task 9 / D10): the caller gets the new status, or the failure stage.
#[tauri::command]
pub async fn daemon_restart(window: tauri::WebviewWindow, app: AppHandle) -> Result<Value, String> {
    caller_link(&app, &window)?;
    let report = run_restart(&app).await?;
    Ok(report_json(&report))
}

/// Validate the caller against the two injected states and hand back the
/// daemon link. Every command starts here; none of them trusts an argument for
/// its own identity.
fn caller_link<'a>(
    app: &'a AppHandle,
    window: &tauri::WebviewWindow,
) -> Result<tauri::State<'a, DaemonLink>, String> {
    let ledger = app
        .try_state::<WindowLedger>()
        .ok_or_else(|| "Picot's window ledger is not available".to_string())?;
    let link = app
        .try_state::<DaemonLink>()
        .ok_or_else(|| "Picot's daemon link is not available".to_string())?;
    validate_caller(&ledger, &link.mirror, &CommandCaller::from_window(window))?;
    Ok(link)
}

/// The status document the settings section renders.
///
/// The daemon's reply is passed through by name (`version`, `buildId`, `pid`,
/// `hostOrigin`, `agents`) and the app-side policy is merged in. Nothing else
/// from the reply is forwarded — a capability or token must never reach a
/// settings panel.
pub fn status_payload(keep_running_after_quit: bool, status: Result<Value, String>) -> Value {
    match status {
        Ok(status) => json!({
            "running": true,
            "version": status.get("version").cloned().unwrap_or(Value::Null),
            "buildId": status.get("buildId").cloned().unwrap_or(Value::Null),
            "pid": status.get("pid").cloned().unwrap_or(Value::Null),
            "hostOrigin": status.get("hostOrigin").cloned().unwrap_or(Value::Null),
            "agents": status.get("agents").cloned().unwrap_or_else(|| json!([])),
            "keepRunningAfterQuit": keep_running_after_quit,
        }),
        Err(reason) => json!({
            "running": false,
            "reason": reason,
            "agents": [],
            "keepRunningAfterQuit": keep_running_after_quit,
        }),
    }
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

    // ─── Caller validation (plan D8/T10 Step 1, Rust red) ───────────────────
    //
    // The caller is a Tauri-injected `WebviewWindow`, which cannot exist without
    // a running app: these tests feed the same decision through
    // `CommandCaller`, the two-field adapter `from_window` produces. The
    // asserts are on the verdict *and* on which of the four facts refused.

    const ORIGIN: &str = "http://127.0.0.1:6001";

    use crate::daemon_client::window_ledger::LedgerRow;

    fn ledger_with(rows: Vec<LedgerRow>) -> WindowLedger {
        let ledger = WindowLedger::new();
        for row in rows {
            ledger.record(row);
        }
        ledger
    }

    fn landing_row() -> LedgerRow {
        LedgerRow::landing("native-landing", "owner-l", "/Users/x")
    }

    /// A mirror attached at epoch 3 with one owner row: exactly what the
    /// `admin_welcome` snapshot installs.
    fn attached_mirror(label: &str, owner: &str, origin: &str) -> NavigationMirror {
        let mirror = NavigationMirror::new();
        mirror.initialize(
            3,
            vec![crate::daemon_client::navigation_mirror::MirrorOwner {
                owner: owner.to_string(),
                label: label.to_string(),
                current_origin: origin.to_string(),
                workspace_id: None,
                session_route: None,
                generation: 1,
            }],
        );
        mirror
    }

    fn caller(label: &str, url: &str) -> CommandCaller {
        CommandCaller {
            label: label.to_string(),
            url: Some(url.to_string()),
        }
    }

    #[test]
    fn the_authorized_label_on_its_committed_origin_passes() {
        let ledger = ledger_with(vec![landing_row()]);
        let mirror = attached_mirror("native-landing", "owner-l", ORIGIN);
        // A settings deep link is a *route*, not a different origin.
        let verdict = validate_caller(
            &ledger,
            &mirror,
            &caller("native-landing", "http://127.0.0.1:6001/#/settings/general"),
        );
        assert_eq!(verdict, Ok(()));
    }

    #[test]
    fn a_label_this_app_never_created_is_rejected() {
        let ledger = ledger_with(vec![landing_row()]);
        let mirror = attached_mirror("native-landing", "owner-l", ORIGIN);
        // Another window's label (a workspace window this app never opened), an
        // injected foreign origin, and the raw daemon origin as a "label": none
        // of them is a Picot window.
        for label in ["native-workspace-w9", "evil", ""] {
            let verdict = validate_caller(&ledger, &mirror, &caller(label, ORIGIN));
            assert!(
                verdict.unwrap_err().contains("is not a Picot window"),
                "label {label:?} must be refused by the ledger"
            );
        }
    }

    #[test]
    fn an_authorized_label_on_an_unauthorized_origin_is_rejected() {
        let ledger = ledger_with(vec![landing_row()]);
        let mirror = attached_mirror("native-landing", "owner-l", ORIGIN);
        // The stale origin (the previous daemon in a restart), another local
        // port, and a non-loopback page: all refused even though the label is
        // legitimate.
        for url in [
            "http://127.0.0.1:6002/#/settings/general",
            "http://localhost:6001/",
            "https://evil.example/",
            "file:///etc/passwd",
        ] {
            let verdict = validate_caller(&ledger, &mirror, &caller("native-landing", url));
            assert!(verdict.is_err(), "{url} must be refused");
        }
        // The opaque document the sandboxed previews use is never a settings
        // caller either (it has no origin to compare).
        assert!(
            validate_caller(&ledger, &mirror, &caller("native-landing", "about:blank")).is_err()
        );
        // An unreadable URL is a refusal, never an assumption.
        let no_url = CommandCaller {
            label: "native-landing".to_string(),
            url: None,
        };
        assert!(validate_caller(&ledger, &mirror, &no_url).is_err());
        assert!(validate_caller(&ledger, &mirror, &caller("native-landing", "not a url")).is_err());
    }

    #[test]
    fn a_disconnected_mirror_refuses_every_caller() {
        let ledger = ledger_with(vec![landing_row()]);
        let mirror = attached_mirror("native-landing", "owner-l", ORIGIN);
        // Sanity: while attached, the same caller passes.
        assert!(validate_caller(&ledger, &mirror, &caller("native-landing", ORIGIN)).is_ok());
        // The channel dropped: the mirror is cleared fail-closed (plan R1), so
        // no command may proceed — not even with a perfectly good label.
        mirror.clear();
        let verdict = validate_caller(&ledger, &mirror, &caller("native-landing", ORIGIN));
        assert!(
            verdict.unwrap_err().contains("no owner binding"),
            "a cleared mirror must refuse on the binding"
        );
    }

    #[test]
    fn a_label_bound_to_another_owner_is_rejected() {
        let ledger = ledger_with(vec![landing_row()]);
        // The daemon says this label belongs to a *different* owner than the one
        // this app created the window with: the app's record and the daemon's
        // authority disagree, so nothing is authorized.
        let mirror = attached_mirror("native-landing", "owner-other", ORIGIN);
        let verdict = validate_caller(&ledger, &mirror, &caller("native-landing", ORIGIN));
        assert!(
            verdict
                .unwrap_err()
                .contains("an owner this app did not create"),
            "the owner mismatch must be named"
        );
    }

    /// A workspace window's row (the daemon minted its owner) validates the same
    /// way: label from the ledger, origin from the mirror.
    #[test]
    fn a_workspace_window_passes_on_its_own_committed_origin() {
        let ledger = ledger_with(vec![LedgerRow::workspace(
            "native-workspace-w1",
            "owner-w",
            "w1",
            Some("/workspaces/w1/sessions/s1".to_string()),
        )]);
        let mirror = attached_mirror("native-workspace-w1", "owner-w", ORIGIN);
        assert_eq!(
            validate_caller(
                &ledger,
                &mirror,
                &caller(
                    "native-workspace-w1",
                    "http://127.0.0.1:6001/workspaces/w1/sessions/s1"
                )
            ),
            Ok(())
        );
    }

    // ─── The status payload (plan T10 interfaces) ───────────────────────────

    #[test]
    fn the_status_payload_merges_the_policy_into_the_daemon_facts() {
        let daemon = json!({
            "version": "0.5.0",
            "buildId": "build-2",
            "pid": 4242,
            "hostOrigin": ORIGIN,
            "agents": [
                {"workspaceId": "w1", "sessionId": "s1", "instanceId": "i1", "streaming": true}
            ],
            // Never forwarded, whatever the daemon happens to add later.
            "adminToken": "secret-token"
        });
        let payload = status_payload(true, Ok(daemon));
        assert_eq!(payload["running"], true);
        assert_eq!(payload["version"], "0.5.0");
        assert_eq!(payload["buildId"], "build-2");
        assert_eq!(payload["pid"], 4242);
        assert_eq!(payload["hostOrigin"], ORIGIN);
        assert_eq!(payload["agents"][0]["sessionId"], "s1");
        assert_eq!(payload["keepRunningAfterQuit"], true);
        let encoded = payload.to_string();
        assert!(!encoded.contains("secret-token"), "{encoded}");
        assert!(!encoded.contains("adminToken"), "{encoded}");
    }

    #[test]
    fn a_stopped_daemon_is_a_successful_payload_with_a_reason() {
        let payload = status_payload(
            false,
            Err("the admin channel closed while the request was in flight".to_string()),
        );
        assert_eq!(payload["running"], false, "{payload}");
        assert_eq!(
            payload["reason"],
            "the admin channel closed while the request was in flight"
        );
        assert_eq!(payload["agents"], json!([]));
        assert_eq!(payload["keepRunningAfterQuit"], false);
        assert!(
            payload.get("version").is_none(),
            "a stopped daemon reports no version: {payload}"
        );
        // A daemon that answers without an agents list is not an error either.
        let bare = status_payload(true, Ok(json!({"version": "0.5.0"})));
        assert_eq!(bare["running"], true);
        assert_eq!(bare["agents"], json!([]));
        assert_eq!(bare["pid"], Value::Null);
    }

    // ─── The restart report ─────────────────────────────────────────────────

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
