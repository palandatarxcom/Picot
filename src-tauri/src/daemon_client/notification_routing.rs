// ABOUTME: App-side notification-click routing (multi-window spec §7): per-window focus
// timestamps and the three-rule pick that replaces per-WebView self-routing.

//! # Notification routing (multi-window spec §7, plan T3)
//!
//! A clicked task-completion notification must move exactly one window, chosen
//! by one decision point. That point is the app-side Rust (spec §7): the daemon
//! keeps broadcasting `notification_activated` to every WebView, but the *route*
//! decision is made here once, from the app's own window facts — which windows
//! are alive, what each one is showing (its live route), and when each was last
//! focused.
//!
//! [`FocusTimes`] is the focus half: [`WindowLedger`](super::window_ledger)
//! records what a window *is*, this records when it was last looked at, as Tauri
//! `Focused(true)` events stamp it. The pick itself is [`pick_notification_target`]:
//! a pure function over a snapshot, so the three spec rules are unit-testable
//! without a window manager.
//!
//! Rules (first hit wins, spec §7):
//!
//! 1. A live window already shows the target session (route session id match)
//!    → focus the most recently focused one, no navigation.
//! 2. No window shows it, but a live window is bound to the target workspace
//!    → focus the most recently focused one and navigate it in-window.
//! 3. Neither → the §6.2 `open_workspace` chain opens a new window.
//!
//! Session identity is the **session id**, not the file path: the route carries
//! the id, the daemon derives files from it, and a file-stem match across
//! different sessions must never count.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Per-label last-focus timestamps, stamped from `WindowEvent::Focused(true)`.
///
/// Managed as app state next to [`WindowLedger`](super::window_ledger); cheap to
/// share, never authorization state — it only orders candidate windows.
#[derive(Default)]
pub struct FocusTimes {
    stamps: Mutex<HashMap<String, Instant>>,
}

impl FocusTimes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stamp `label` as focused now. A rebuilt label (restart migration) simply
    /// re-stamps; `forget` clears a destroyed window's row.
    pub fn record(&self, label: &str) {
        self.stamps
            .lock()
            .expect("focus times lock poisoned")
            .insert(label.to_string(), Instant::now());
    }

    /// Drop a destroyed window's stamp so a later rebuild starts unfocused.
    pub fn forget(&self, label: &str) {
        self.stamps
            .lock()
            .expect("focus times lock poisoned")
            .remove(label);
    }

    #[allow(dead_code)] // the batch-2 interception executor is the first production consumer
    pub fn focused_at(&self, label: &str) -> Option<Instant> {
        self.stamps
            .lock()
            .expect("focus times lock poisoned")
            .get(label)
            .copied()
    }
}

/// One window's routing facts, snapshot at decision time.
///
/// `workspace_id`/`session_id` come from the window's **live route** (the
/// document URL reflects SPA session switches no mirror event sees — the same
/// reason [`capture_open_views`](crate::capture_open_views) reads URLs);
/// `focused_at` from [`FocusTimes`]; `alive` marks a ledger row whose window no
/// longer exists (destroyed windows keep stale rows until `forget` runs).
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // the batch-2 interception executor is the first production consumer
pub struct WindowRoutingState {
    pub label: String,
    pub workspace_id: Option<String>,
    pub session_id: Option<String>,
    pub focused_at: Option<Instant>,
    pub alive: bool,
}

impl WindowRoutingState {
    /// Build the state from a captured route (path + query, or `None` for the
    /// landing root). Unknown routes leave both ids unset — such a window can
    /// still serve rule 2 only through an explicit workspace binding, which the
    /// caller can patch in from its ledger row.
    #[allow(dead_code)] // the batch-2 interception executor is the first production consumer
    pub fn from_route(
        label: &str,
        route: Option<&str>,
        focused_at: Option<Instant>,
        alive: bool,
    ) -> Self {
        let (workspace_id, session_id) = route
            .and_then(session_route_identity)
            .map(|(workspace_id, session_id)| (Some(workspace_id), Some(session_id)))
            .unwrap_or((None, None));
        Self {
            label: label.to_string(),
            workspace_id,
            session_id,
            focused_at,
            alive,
        }
    }
}

/// The notification payload's identity half — what the decision needs. The
/// `sessionFile`/`cwd` fields travel with the event but belong to the *executor*
/// (the §6.2 open chain's arguments), not to the pick.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // the batch-2 interception executor is the first production consumer
pub struct NotificationPayload {
    pub workspace_id: String,
    pub session_id: String,
}

/// The three-rule decision (spec §7). The executor maps each variant to its
/// action: focus only / focus + in-window navigation / the open chain.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // the batch-2 interception executor is the first production consumer
pub enum NotificationTarget {
    /// Rule 1: this live window already shows the session — focus it, navigate
    /// nothing (re-selecting would disturb a live turn).
    FocusShowing { label: String },
    /// Rule 2: no window shows the session but this live window is bound to the
    /// target workspace — focus it and navigate to the session in-window.
    FocusWorkspace { label: String },
    /// Rule 3: no live window displays the session or holds the workspace —
    /// open a new one through the §6.2 `open_workspace` chain.
    OpenNew,
}

/// Parse `/workspaces/{wid}/sessions/{sid}` (query ignored) into its ids.
///
/// Segments compare exactly — ids are UUIDs and date-named session ids, neither
/// of which `encodeURIComponent` alters. shortcut: a future id format that
/// percent-encodes needs decoding here.
#[allow(dead_code)] // the batch-2 interception executor is the first production consumer
pub fn session_route_identity(route: &str) -> Option<(String, String)> {
    let path = route.split('?').next()?;
    let mut segments = path.trim_matches('/').split('/');
    if segments.next()? != "workspaces" {
        return None;
    }
    let workspace_id = segments.next()?.to_string();
    if segments.next()? != "sessions" {
        return None;
    }
    let session_id = segments.next()?.to_string();
    if segments.next().is_some() || workspace_id.is_empty() || session_id.is_empty() {
        return None;
    }
    Some((workspace_id, session_id))
}

/// The spec §7 pick: first matching rule wins, dead windows never match, and
/// "most recently focused" is a deterministic partial order — a missing stamp
/// ranks below any stamp, ties (including both missing) keep the snapshot's
/// first row.
#[allow(dead_code)] // the batch-2 interception executor is the first production consumer
pub fn pick_notification_target(
    windows: &[WindowRoutingState],
    payload: &NotificationPayload,
) -> NotificationTarget {
    // Rule 1 first: a window already showing the session is never navigated
    // past, even when a workspace-bound window was focused more recently.
    if let Some(window) = most_recently_focused(windows.iter().filter(|window| {
        window.alive && window.session_id.as_deref() == Some(payload.session_id.as_str())
    })) {
        return NotificationTarget::FocusShowing {
            label: window.label.clone(),
        };
    }
    // Rule 2: the target workspace's own window takes the in-window
    // navigation; other workspaces' windows are never borrowed (spec §7).
    if let Some(window) = most_recently_focused(windows.iter().filter(|window| {
        window.alive && window.workspace_id.as_deref() == Some(payload.workspace_id.as_str())
    })) {
        return NotificationTarget::FocusWorkspace {
            label: window.label.clone(),
        };
    }
    NotificationTarget::OpenNew
}

/// The deterministic "most recently focused" pick over candidates in
/// snapshot order: a stamp beats no stamp, a later stamp beats an earlier one,
/// and a tie keeps the first row (spec §7's partial order).
fn most_recently_focused<'a>(
    mut candidates: impl Iterator<Item = &'a WindowRoutingState>,
) -> Option<&'a WindowRoutingState> {
    let mut best = candidates.next()?;
    for window in candidates {
        let beats = match (window.focused_at, best.focused_at) {
            (Some(_), None) => true,
            (Some(window_at), Some(best_at)) => window_at > best_at,
            _ => false,
        };
        if beats {
            best = window;
        }
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn stamp(later_than: Instant, secs: u64) -> Instant {
        later_than + Duration::from_secs(secs)
    }

    fn payload() -> NotificationPayload {
        NotificationPayload {
            workspace_id: "w1".to_string(),
            session_id: "20261017_090000_aaa".to_string(),
        }
    }

    fn showing(
        label: &str,
        workspace: &str,
        session: &str,
        focused_at: Option<Instant>,
        alive: bool,
    ) -> WindowRoutingState {
        WindowRoutingState {
            label: label.to_string(),
            workspace_id: Some(workspace.to_string()),
            session_id: Some(session.to_string()),
            focused_at,
            alive,
        }
    }

    // Rule 1: the live window showing the target session is focused, un-navigated.

    #[test]
    fn rule1_a_live_window_showing_the_session_is_focused() {
        let windows = [showing("a", "w1", &payload().session_id, None, true)];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::FocusShowing {
                label: "a".to_string()
            }
        );
    }

    #[test]
    fn rule1_same_session_multiple_windows_pick_the_most_recently_focused() {
        let base = Instant::now();
        let windows = [
            showing(
                "older",
                "w1",
                &payload().session_id,
                Some(stamp(base, 1)),
                true,
            ),
            showing(
                "newer",
                "w1",
                &payload().session_id,
                Some(stamp(base, 5)),
                true,
            ),
        ];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::FocusShowing {
                label: "newer".to_string()
            }
        );
    }

    #[test]
    fn rule1_a_focused_stamp_beats_none_and_a_tie_keeps_snapshot_order() {
        let base = Instant::now();
        // Focused beats never-focused even when the never-focused row is first.
        let windows = [
            showing("unfocused", "w1", &payload().session_id, None, true),
            showing(
                "focused",
                "w1",
                &payload().session_id,
                Some(stamp(base, 1)),
                true,
            ),
        ];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::FocusShowing {
                label: "focused".to_string()
            }
        );
        // Equal stamps (including both `None`) keep the first row.
        let tied = [
            showing("first", "w1", &payload().session_id, None, true),
            showing("second", "w1", &payload().session_id, None, true),
        ];
        assert_eq!(
            pick_notification_target(&tied, &payload()),
            NotificationTarget::FocusShowing {
                label: "first".to_string()
            }
        );
        let tied_stamped = [
            showing(
                "first",
                "w1",
                &payload().session_id,
                Some(stamp(base, 3)),
                true,
            ),
            showing(
                "second",
                "w1",
                &payload().session_id,
                Some(stamp(base, 3)),
                true,
            ),
        ];
        assert_eq!(
            pick_notification_target(&tied_stamped, &payload()),
            NotificationTarget::FocusShowing {
                label: "first".to_string()
            }
        );
    }

    #[test]
    fn rule1_a_dead_window_showing_the_session_is_ignored() {
        let windows = [showing(
            "closed",
            "w1",
            &payload().session_id,
            Some(Instant::now()),
            false,
        )];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::OpenNew,
            "a stale ledger row must not capture the click"
        );
    }

    // Rule 2: nobody shows it, but the workspace's window takes the navigation.

    #[test]
    fn rule2_a_workspace_window_without_the_session_takes_focus_and_navigation() {
        let windows = [showing("b", "w1", "some_other_session", None, true)];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::FocusWorkspace {
                label: "b".to_string()
            }
        );
    }

    #[test]
    fn rule2_multiple_workspace_windows_pick_the_most_recently_focused() {
        let base = Instant::now();
        let windows = [
            showing("older", "w1", "other", Some(stamp(base, 1)), true),
            showing("newer", "w1", "another", Some(stamp(base, 4)), true),
        ];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::FocusWorkspace {
                label: "newer".to_string()
            }
        );
    }

    #[test]
    fn rule2_a_window_of_another_workspace_is_not_eligible() {
        let windows = [showing(
            "other-workspace",
            "w2",
            "any",
            Some(Instant::now()),
            true,
        )];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::OpenNew,
            "rule 2 never navigates another workspace's window (spec §7)"
        );
    }

    #[test]
    fn rule2_a_landing_window_with_no_workspace_binding_is_not_eligible() {
        let windows = [WindowRoutingState {
            label: "native-landing".to_string(),
            workspace_id: None,
            session_id: None,
            focused_at: Some(Instant::now()),
            alive: true,
        }];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::OpenNew
        );
    }

    #[test]
    fn rule2_a_dead_workspace_window_falls_through_to_open_new() {
        let windows = [showing(
            "closed",
            "w1",
            "other",
            Some(Instant::now()),
            false,
        )];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::OpenNew
        );
    }

    // Rule 1 outranks rule 2 even when the workspace window is fresher.

    #[test]
    fn rule1_outranks_rule2_regardless_of_focus_recency() {
        let base = Instant::now();
        let windows = [
            showing(
                "showing-stale",
                "w1",
                &payload().session_id,
                Some(stamp(base, 1)),
                true,
            ),
            showing("workspace-fresh", "w1", "other", Some(stamp(base, 9)), true),
        ];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::FocusShowing {
                label: "showing-stale".to_string()
            }
        );
    }

    // Identity: session id, never a file-stem match.

    #[test]
    fn identity_a_session_id_match_is_exact_never_a_file_stem_match() {
        // Same file stem prefix, different session ids: no rule-1 match.
        let windows = [showing("a", "w1", "20261017_090000_bbb", None, true)];
        assert_eq!(
            pick_notification_target(&windows, &payload()),
            NotificationTarget::FocusWorkspace {
                label: "a".to_string()
            },
            "a near-identical session file name must not count as the session"
        );
    }

    // Route parsing.

    #[test]
    fn routes_parse_workspace_and_session_ids_and_ignore_the_query() {
        assert_eq!(
            session_route_identity("/workspaces/w1/sessions/s2"),
            Some(("w1".to_string(), "s2".to_string()))
        );
        assert_eq!(
            session_route_identity("/workspaces/w1/sessions/s2?focus=%2Ftmp"),
            Some(("w1".to_string(), "s2".to_string())),
            "the focus query never leaks into the session id"
        );
        assert_eq!(session_route_identity("/"), None);
        assert_eq!(session_route_identity("/workspaces/w1"), None);
        assert_eq!(session_route_identity("/workspaces/w1/sessions"), None);
        assert_eq!(session_route_identity("/settings/general"), None);
        assert_eq!(
            session_route_identity("/workspaces/w1/sessions/s2/extra"),
            None
        );
    }

    #[test]
    fn from_route_fills_the_ids_and_keeps_a_landing_row_unbound() {
        let state = WindowRoutingState::from_route(
            "native-workspace-w1",
            Some("/workspaces/w1/sessions/s2?x=1"),
            None,
            true,
        );
        assert_eq!(state.workspace_id.as_deref(), Some("w1"));
        assert_eq!(state.session_id.as_deref(), Some("s2"));
        assert!(state.alive);

        let landing =
            WindowRoutingState::from_route("native-landing", None, Some(Instant::now()), true);
        assert_eq!(landing.workspace_id, None);
        assert_eq!(landing.session_id, None);
    }

    // FocusTimes stamping.

    #[test]
    fn focus_times_stamp_overwrite_and_forget() {
        let times = FocusTimes::new();
        assert_eq!(times.focused_at("a"), None, "an unknown label has no stamp");
        times.record("a");
        let first = times.focused_at("a").expect("recorded");
        times.record("a");
        assert!(
            times.focused_at("a").expect("re-recorded") >= first,
            "re-recording never moves the stamp backwards"
        );
        times.forget("a");
        assert_eq!(
            times.focused_at("a"),
            None,
            "a forgotten label is unstamped"
        );
        times.forget("never-recorded");
    }

    #[test]
    fn focus_times_track_labels_independently() {
        let times = FocusTimes::new();
        times.record("a");
        assert_eq!(times.focused_at("b"), None);
        assert!(times.focused_at("a").is_some());
    }
}
