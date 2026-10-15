// ABOUTME: The app-side window ledger: every window this app created, with the facts the restart
// ABOUTME: transaction (pre-state capture) and the admin commands (caller validation) need.

//! # Window ledger (plan Task 9, extended by T10)
//!
//! The app creates every WebView window, so it is the only place that knows
//! *which* windows exist. The restart transaction needs exactly that before it
//! replaces the daemon: the labels to migrate, each view's route, and — for a
//! window whose owner the app minted itself (the landing window) — the owner's
//! home. A workspace window's owner is minted **daemon-side** by the
//! `open_workspace` dispatcher (T5/T7), so the app only knows its
//! `workspaceId`; its canonical root stays the daemon's business (the T9
//! amendment to `owner.create`: an omitted `cwd` is resolved daemon-side).
//!
//! The ledger is *not* authorization state: the daemon's owner registry is (plan
//! D4). It is the app's own bookkeeping, recorded at window creation and read by
//! the restart transaction (T9) and, for caller validation, by the three admin
//! commands (T10). It never decides whether a navigation is allowed.
//!
//! Route capture is deliberately read from the live window URL in
//! [`crate::capture_open_views`] — the document's own URL reflects SPA
//! (`pushState`) session switches, which no mirror event sees. The row's
//! `route` is the fallback for a window whose URL is unreadable or foreign.

use std::sync::Mutex;

/// What the app recorded about one window it created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerRow {
    /// The window label: Tauri labels are immutable, so this is the identity.
    pub label: String,
    /// The owner id the window was created with.
    pub owner: String,
    /// The owner's canonical home/cwd, when the app supplied it (the landing
    /// window). `None` for a daemon-minted workspace owner, whose root the
    /// daemon resolves from its own registry.
    pub owner_home: Option<String>,
    /// The port the owner was created with (`0` for every native window today).
    pub port: u16,
    /// The registered workspace the owner is bound to, if any.
    pub workspace_id: Option<String>,
    /// The route the window was opened at (path + query), `None` for the root.
    pub route: Option<String>,
}

impl LedgerRow {
    /// A landing-kind window: the app minted its owner with an explicit home.
    pub fn landing(label: &str, owner: &str, owner_home: &str) -> Self {
        Self {
            label: label.to_string(),
            owner: owner.to_string(),
            owner_home: Some(owner_home.to_string()),
            port: 0,
            workspace_id: None,
            route: None,
        }
    }

    /// A workspace window: the owner came from the daemon's `open_workspace`
    /// path, so only the workspace binding and the route are known here.
    pub fn workspace(label: &str, owner: &str, workspace_id: &str, route: Option<String>) -> Self {
        Self {
            label: label.to_string(),
            owner: owner.to_string(),
            owner_home: None,
            port: 0,
            workspace_id: Some(workspace_id.to_string()),
            route,
        }
    }
}

/// The app's window registry. Cheap to share; managed as Tauri state.
#[derive(Default)]
pub struct WindowLedger {
    rows: Mutex<Vec<LedgerRow>>,
}

impl WindowLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record (or replace) the row for a label. A rebuild of an existing label
    /// — the restart transaction's migration — replaces the old row rather than
    /// appending a second one, so a label has exactly one row.
    pub fn record(&self, row: LedgerRow) {
        let mut rows = self.rows.lock().expect("window ledger lock poisoned");
        rows.retain(|existing| existing.label != row.label);
        rows.push(row);
    }

    /// Forget a destroyed window. A window the app no longer has must not be
    /// migrated by a later restart.
    pub fn forget(&self, label: &str) {
        self.rows
            .lock()
            .expect("window ledger lock poisoned")
            .retain(|row| row.label != label);
    }

    pub fn row_for_label(&self, label: &str) -> Option<LedgerRow> {
        self.rows
            .lock()
            .expect("window ledger lock poisoned")
            .iter()
            .find(|row| row.label == label)
            .cloned()
    }

    /// Every recorded window, in creation order.
    pub fn rows(&self) -> Vec<LedgerRow> {
        self.rows
            .lock()
            .expect("window ledger lock poisoned")
            .clone()
    }

    pub fn contains_label(&self, label: &str) -> bool {
        self.rows
            .lock()
            .expect("window ledger lock poisoned")
            .iter()
            .any(|row| row.label == label)
    }

    /// Diagnostics and T10's command validation; the restart transaction
    /// enumerates with [`Self::rows`].
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.rows.lock().expect("window ledger lock poisoned").len()
    }

    /// The `len`/`is_empty` pair clippy expects; no caller needs it yet.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The route (path + query) of `url`, or `None` when it is not a document on
/// `host_origin` or when it *is* the origin root.
///
/// The comparison is on the **origin only** (`scheme://host:port`), exactly the
/// boundary the daemon's navigation authority draws: paths are routes, not
/// authority (plan D4).
pub fn route_for_url(url: &str, host_origin: &str) -> Option<String> {
    let parsed = tauri::Url::parse(url).ok()?;
    let origin = format!(
        "{}://{}",
        parsed.scheme(),
        parsed.host_str().map(|host| match parsed.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        })?
    );
    if origin != host_origin.trim_end_matches('/') {
        return None;
    }
    let mut route = parsed.path().to_string();
    if let Some(query) = parsed.query() {
        route.push('?');
        route.push_str(query);
    }
    if route.is_empty() || route == "/" {
        return None;
    }
    Some(route)
}

/// The absolute URL a view is loaded at on `host_origin`.
pub fn view_url(host_origin: &str, route: Option<&str>) -> String {
    let base = host_origin.trim_end_matches('/');
    match route {
        Some(route) if route.starts_with('/') => format!("{base}{route}"),
        Some(route) if !route.is_empty() => format!("{base}/{route}"),
        _ => format!("{base}/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_row(label: &str) -> LedgerRow {
        LedgerRow::workspace(
            label,
            "owner-a",
            "w1",
            Some("/workspaces/w1/sessions/s1".to_string()),
        )
    }

    #[test]
    fn a_label_has_exactly_one_row_and_recording_it_again_replaces_it() {
        let ledger = WindowLedger::new();
        ledger.record(workspace_row("native-workspace-w1"));
        ledger.record(workspace_row("native-workspace-w1"));
        assert_eq!(ledger.len(), 1, "a rebuilt label does not append a row");
        assert_eq!(
            ledger.row_for_label("native-workspace-w1"),
            Some(workspace_row("native-workspace-w1"))
        );
        assert!(!ledger.contains_label("native-landing"));
        assert_eq!(ledger.rows().len(), 1, "creation order is preserved");
    }

    #[test]
    fn a_forgotten_window_is_gone() {
        let ledger = WindowLedger::new();
        ledger.record(LedgerRow::landing("native-landing", "owner-l", "/Users/x"));
        ledger.record(workspace_row("native-workspace-w1"));
        ledger.forget("native-landing");
        assert!(!ledger.contains_label("native-landing"));
        assert!(ledger.contains_label("native-workspace-w1"));
        assert_eq!(ledger.len(), 1);
        // Forgetting an unknown label is a no-op, not an error.
        ledger.forget("native-landing");
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn a_landing_row_carries_the_home_and_no_workspace() {
        let row = LedgerRow::landing("native-landing", "owner-l", "/Users/x");
        assert_eq!(row.owner_home.as_deref(), Some("/Users/x"));
        assert_eq!(row.workspace_id, None);
        assert_eq!(row.route, None, "the landing window opens at the root");
        assert_eq!(row.port, 0);
    }

    #[test]
    fn route_capture_reads_path_and_query_of_our_own_origin_only() {
        let origin = "http://127.0.0.1:5001";
        assert_eq!(
            route_for_url("http://127.0.0.1:5001/workspaces/w1/sessions/s1", origin).as_deref(),
            Some("/workspaces/w1/sessions/s1")
        );
        assert_eq!(
            route_for_url(
                "http://127.0.0.1:5001/workspaces/w1/sessions/s1?tab=x",
                origin
            )
            .as_deref(),
            Some("/workspaces/w1/sessions/s1?tab=x"),
            "a query survives the round trip"
        );
        assert_eq!(
            route_for_url("http://127.0.0.1:5001/", origin),
            None,
            "the origin root is no route"
        );
        assert_eq!(
            route_for_url("http://127.0.0.1:5001", origin),
            None,
            "a bare origin is no route"
        );
        assert_eq!(
            route_for_url("http://127.0.0.1:6002/workspaces/w1/sessions/s1", origin),
            None,
            "another origin's document is not this view's route"
        );
        assert_eq!(
            route_for_url("about:blank", origin),
            None,
            "opaque documents have no route"
        );
        assert_eq!(route_for_url("not a url", origin), None);
    }

    #[test]
    fn the_view_url_is_the_new_origin_plus_the_captured_route() {
        let origin = "http://127.0.0.1:6002";
        assert_eq!(
            view_url(origin, Some("/workspaces/w1/sessions/s1")),
            "http://127.0.0.1:6002/workspaces/w1/sessions/s1"
        );
        assert_eq!(
            view_url("http://127.0.0.1:6002/", Some("/workspaces/w1/sessions/s1")),
            "http://127.0.0.1:6002/workspaces/w1/sessions/s1",
            "a trailing slash on the origin never doubles"
        );
        assert_eq!(view_url(origin, None), "http://127.0.0.1:6002/");
        assert_eq!(
            view_url(origin, Some("workspaces/w1")),
            "http://127.0.0.1:6002/workspaces/w1"
        );
        assert_eq!(view_url(origin, Some("")), "http://127.0.0.1:6002/");
    }
}
