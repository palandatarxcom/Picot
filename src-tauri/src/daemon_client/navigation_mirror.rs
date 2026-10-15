// ABOUTME: The app-side navigation mirror: the daemon's authorization facts, kept fresh by nav
// ABOUTME: permits/revokes/binding changes and cleared fail-closed when the admin channel drops.

//! # Navigation mirror (plan Task 5, client rules R1)
//!
//! The daemon owns owner identity, capability authorization and the exact-origin
//! navigation boundary (plan D4). The app's WebView delegate is **synchronous**,
//! so it cannot ask the daemon at navigation time. The mirror is what it reads:
//!
//! - it is **initialized from the `admin_welcome` full snapshot** at attach;
//! - `navigation_permit` events install a one-shot permit for an origin, and the
//!   client acks each one immediately (the granting op waits on that ack);
//! - a current-epoch `permit_revoked` is applied **immediately** — the permit is
//!   gone before anything can use it; only stale-epoch events are dropped;
//! - `owner_binding_changed` keeps an owner's committed origin, workspace
//!   binding and generation current (the daemon publishes the origin it
//!   committed, see `OwnerMirrorEvent::OwnerBindingChanged`);
//! - on disconnect the mirror is **cleared**: from then on the delegate permits
//!   only the opaque `about:srcdoc`/`about:blank` documents (fail-closed).
//!
//! The origin rule itself is not re-implemented here: it is
//! [`picot_host::window_owner::navigation_origin`], the same function the daemon
//! uses to produce the `currentOrigin`/`permit.origin` strings this mirror
//! compares against (plan D4: the daemon is the authority; the mirror is its
//! app-side image).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use picot_host::window_owner::{is_about_document, navigation_origin};
use serde::{Deserialize, Serialize};
use tauri::Url;

/// One owner row of the attach snapshot (`admin_welcome.mirror.owners`) or of an
/// `owner.create` reply, with the protocol's field names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorOwner {
    pub owner: String,
    pub label: String,
    #[serde(rename = "currentOrigin")]
    pub current_origin: String,
    #[serde(rename = "workspaceId", default)]
    pub workspace_id: Option<String>,
    #[serde(rename = "sessionRoute", default)]
    pub session_route: Option<String>,
    #[serde(default)]
    pub generation: u64,
}

/// The mirror's own owner row: the wire row plus the workspace binding the
/// `owner_binding_changed` event carries as a boolean (the event intentionally
/// does not repeat the workspace id — nothing app-side keys on it).
#[derive(Clone, Debug, PartialEq, Eq)]
struct MirrorRow {
    owner: String,
    label: String,
    current_origin: String,
    workspace_id: Option<String>,
    session_route: Option<String>,
    generation: u64,
    has_workspace: bool,
}

impl MirrorRow {
    fn from_wire(owner: MirrorOwner) -> Self {
        Self {
            has_workspace: owner.workspace_id.is_some(),
            owner: owner.owner,
            label: owner.label,
            current_origin: owner.current_origin,
            workspace_id: owner.workspace_id,
            session_route: owner.session_route,
            generation: owner.generation,
        }
    }
}

/// A permit the daemon granted for exactly one navigation to `origin`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPermit {
    pub permit_id: String,
    pub owner: String,
    pub origin: String,
    pub expires_at_ms: u64,
    pub generation: u64,
}

#[derive(Default)]
struct MirrorState {
    /// The attachment epoch the mirror currently reflects. `None` = no
    /// attachment, which is the fail-closed state.
    epoch: Option<u64>,
    owners: HashMap<String, MirrorRow>,
    labels: HashMap<String, String>,
    permits: HashMap<String, InstalledPermit>,
}

/// The app's view of the daemon's navigation authority. Cheap to share
/// (`Arc<NavigationMirror>`) and safe to call from the synchronous delegate.
#[derive(Default)]
pub struct NavigationMirror {
    inner: Mutex<MirrorState>,
}

impl NavigationMirror {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the `admin_welcome` full snapshot: the attach transaction
    /// replaces every fact the mirror holds, including permits from a previous
    /// attachment (stale permits must never survive a reattach).
    pub fn initialize(&self, epoch: u64, owners: Vec<MirrorOwner>) {
        let mut state = self.inner.lock().expect("mirror lock poisoned");
        state.epoch = Some(epoch);
        state.owners.clear();
        state.labels.clear();
        state.permits.clear();
        for owner in owners {
            state
                .labels
                .insert(owner.label.clone(), owner.owner.clone());
            state
                .owners
                .insert(owner.owner.clone(), MirrorRow::from_wire(owner));
        }
    }

    /// Install one owner row minted by `owner.create`. Owners created *after*
    /// the attach snapshot only reach the mirror this way (the daemon's reply is
    /// the authoritative, normalized row).
    pub fn install_owner(&self, owner: MirrorOwner) {
        let mut state = self.inner.lock().expect("mirror lock poisoned");
        state
            .labels
            .insert(owner.label.clone(), owner.owner.clone());
        state
            .owners
            .insert(owner.owner.clone(), MirrorRow::from_wire(owner));
    }

    /// Forget every fact: no attachment means nothing is authorized beyond the
    /// opaque `about:` documents (fail-closed, plan R1).
    pub fn clear(&self) {
        let mut state = self.inner.lock().expect("mirror lock poisoned");
        state.epoch = None;
        state.owners.clear();
        state.labels.clear();
        state.permits.clear();
    }

    /// The attachment epoch the mirror reflects, or `None` while disconnected.
    pub fn epoch(&self) -> Option<u64> {
        self.inner.lock().expect("mirror lock poisoned").epoch
    }

    /// Install a permit from a `navigation_permit` event. `false` means the
    /// event belongs to a stale attachment epoch and was dropped.
    pub fn install_permit(&self, epoch: u64, permit: InstalledPermit) -> bool {
        let mut state = self.inner.lock().expect("mirror lock poisoned");
        if state.epoch != Some(epoch) {
            return false;
        }
        prune_expired(&mut state, now_ms());
        state.permits.insert(permit.permit_id.clone(), permit);
        true
    }

    /// Apply a `permit_revoked` event. A current-epoch revocation removes the
    /// permit **immediately** (the navigation it would have allowed is denied
    /// from now on); a stale-epoch one is dropped.
    pub fn revoke_permit(&self, epoch: u64, permit_id: &str) -> bool {
        let mut state = self.inner.lock().expect("mirror lock poisoned");
        if state.epoch != Some(epoch) {
            return false;
        }
        state.permits.remove(permit_id).is_some()
    }

    /// Apply an `owner_binding_changed` event: a full owner row the daemon just
    /// created (a workspace window's owner is minted daemon-side) or committed
    /// (the transition moved it to a new origin). Stale-epoch events are dropped.
    ///
    /// This is an **upsert**: the event is the only way the mirror learns about
    /// owners created after the attach snapshot, so an unknown owner is inserted
    /// rather than ignored (an ignored row would leave the app denying the new
    /// window's first load, fail-closed but useless).
    pub fn apply_owner_changed(&self, epoch: u64, owner: MirrorOwner, has_workspace: bool) -> bool {
        let mut state = self.inner.lock().expect("mirror lock poisoned");
        if state.epoch != Some(epoch) {
            return false;
        }
        let mut row = MirrorRow::from_wire(owner);
        row.has_workspace = has_workspace;
        state.labels.insert(row.label.clone(), row.owner.clone());
        state.owners.insert(row.owner.clone(), row);
        true
    }

    /// The synchronous delegate's verdict (plan R1 + `window_owner.rs:259-294`).
    ///
    /// Fail-closed: with no attachment, an unknown owner, or an origin the daemon
    /// never authorized, the answer is `false`. The one exception is the opaque
    /// `about:` document (`srcdoc`/`blank`) the sandboxed preview iframes use —
    /// exactly the exception the daemon-side registry keeps.
    pub fn authorize(&self, owner: &str, url: &Url) -> bool {
        if is_about_document(url) {
            return true;
        }
        let Some(origin) = navigation_origin(url) else {
            return false;
        };
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        let now = now_ms();
        prune_expired(&mut state, now);
        let Some(record) = state.owners.get(owner) else {
            return false;
        };
        if record.current_origin == origin {
            return true;
        }
        // A permit is one-shot: consuming it here is what makes a second
        // navigation to the same origin require a fresh permit.
        let permit_id = state
            .permits
            .values()
            .find(|permit| permit.owner == owner && permit.origin == origin)
            .map(|permit| permit.permit_id.clone());
        match permit_id {
            Some(permit_id) => {
                state.permits.remove(&permit_id);
                true
            }
            None => false,
        }
    }

    /// The owner that owns `label` (the app keeps the label↔owner map, plan D4).
    pub fn owner_for_label(&self, label: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("mirror lock poisoned")
            .labels
            .get(label)
            .cloned()
    }

    pub fn label_for_owner(&self, owner: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("mirror lock poisoned")
            .owners
            .get(owner)
            .map(|record| record.label.clone())
    }

    /// Whether the owner is bound to a workspace, or `None` for an unknown
    /// owner — the menu/Cmd+N gate's input (never the window label).
    pub fn owner_has_workspace(&self, owner: &str) -> Option<bool> {
        self.inner
            .lock()
            .expect("mirror lock poisoned")
            .owners
            .get(owner)
            .map(|record| record.has_workspace)
    }

    /// Installed, unexpired permits: an observation for tests and diagnostics.
    pub fn permit_count(&self) -> usize {
        let mut state = self.inner.lock().expect("mirror lock poisoned");
        prune_expired(&mut state, now_ms());
        state.permits.len()
    }

    pub fn owner_count(&self) -> usize {
        self.inner
            .lock()
            .expect("mirror lock poisoned")
            .owners
            .len()
    }
}

fn prune_expired(state: &mut MirrorState, now: u64) {
    state.permits.retain(|_, permit| permit.expires_at_ms > now);
}

/// Wall-clock milliseconds since the Unix epoch: the unit the daemon's
/// `navigation_permit.expiresAtMs` uses (the app only compares it against its
/// own clock, exactly as it did inside the daemon-side registry).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(value: &str) -> Url {
        value.parse().expect("a test url")
    }

    fn owner(owner: &str, label: &str, origin: &str) -> MirrorOwner {
        MirrorOwner {
            owner: owner.to_string(),
            label: label.to_string(),
            current_origin: origin.to_string(),
            workspace_id: None,
            session_route: None,
            generation: 0,
        }
    }

    fn permit(id: &str, owner: &str, origin: &str, expires_at_ms: u64) -> InstalledPermit {
        InstalledPermit {
            permit_id: id.to_string(),
            owner: owner.to_string(),
            origin: origin.to_string(),
            expires_at_ms,
            generation: 1,
        }
    }

    #[test]
    fn the_snapshot_row_shape_is_the_protocol_table() {
        // Engineering-lesson #1: typed by hand from the `admin_welcome.mirror`
        // row in the plan's Task 3 table.
        let row = r#"{"owner":"o1","label":"w1","currentOrigin":"http://127.0.0.1:5001","workspaceId":"w1","sessionRoute":"/workspaces/w1/sessions/s1","generation":7}"#;
        let decoded: MirrorOwner = serde_json::from_str(row).expect("the table row parses");
        assert_eq!(
            decoded,
            MirrorOwner {
                owner: "o1".to_string(),
                label: "w1".to_string(),
                current_origin: "http://127.0.0.1:5001".to_string(),
                workspace_id: Some("w1".to_string()),
                session_route: Some("/workspaces/w1/sessions/s1".to_string()),
                generation: 7,
            }
        );
        // A landing row carries nulls for the workspace fields.
        let landing = r#"{"owner":"o2","label":"native-landing","currentOrigin":"http://127.0.0.1:5001","workspaceId":null,"sessionRoute":null,"generation":0}"#;
        let decoded: MirrorOwner = serde_json::from_str(landing).expect("parses");
        assert_eq!(decoded.workspace_id, None);
        assert_eq!(decoded.session_route, None);
    }

    #[test]
    fn a_snapshot_authorizes_only_the_owners_committed_origin() {
        let mirror = NavigationMirror::new();
        mirror.initialize(3, vec![owner("o1", "w1", "http://127.0.0.1:5001")]);

        assert!(mirror.authorize("o1", &url("http://127.0.0.1:5001/")));
        assert!(mirror.authorize(
            "o1",
            &url("http://127.0.0.1:5001/workspaces/w1/sessions/s1")
        ));
        // A different port is a different origin (the daemon's rule).
        assert!(!mirror.authorize("o1", &url("http://127.0.0.1:5002/")));
        // An unknown owner is never authorized.
        assert!(!mirror.authorize("o2", &url("http://127.0.0.1:5001/")));
        // Non-loopback and non-http schemes are never authorized.
        assert!(!mirror.authorize("o1", &url("https://example.com/")));
        assert!(!mirror.authorize("o1", &url("http://192.168.1.5:5001/")));
        assert!(!mirror.authorize("o1", &url("file:///etc/passwd")));
        assert!(!mirror.authorize("o1", &url("data:text/html,<h1>x</h1>")));
        // The opaque about: documents are the one exception — exactly the two
        // strings the daemon-side registry allows (window_owner.rs:259-294).
        assert!(mirror.authorize("o1", &url("about:srcdoc")));
        assert!(mirror.authorize("o1", &url("about:blank")));
        assert!(!mirror.authorize("o1", &url("about:config")));
    }

    #[test]
    fn a_cleared_mirror_denies_everything_but_the_about_documents() {
        let mirror = NavigationMirror::new();
        mirror.initialize(3, vec![owner("o1", "w1", "http://127.0.0.1:5001")]);
        mirror.install_permit(3, permit("p1", "o1", "http://127.0.0.1:5002", u64::MAX));
        assert!(mirror.authorize("o1", &url("http://127.0.0.1:5002/x")));

        mirror.clear();
        assert_eq!(mirror.epoch(), None);
        assert_eq!(mirror.owner_count(), 0);
        assert_eq!(mirror.permit_count(), 0);
        assert!(!mirror.authorize("o1", &url("http://127.0.0.1:5001/")));
        assert!(!mirror.authorize("o1", &url("http://127.0.0.1:5002/x")));
        assert!(mirror.authorize("o1", &url("about:srcdoc")));
        assert!(mirror.authorize("o1", &url("about:blank")));
    }

    #[test]
    fn a_permit_is_installed_once_and_consumed_by_the_navigation_it_allowed() {
        let mirror = NavigationMirror::new();
        mirror.initialize(3, vec![owner("o1", "w1", "http://127.0.0.1:5001")]);
        assert!(mirror.install_permit(
            3,
            permit("p1", "o1", "http://127.0.0.1:5002", now_ms() + 30_000)
        ));
        assert!(mirror.authorize("o1", &url("http://127.0.0.1:5002/x")));
        assert!(
            !mirror.authorize("o1", &url("http://127.0.0.1:5002/again")),
            "a permit is one-shot"
        );
        assert_eq!(mirror.permit_count(), 0);
        // Someone else's permit never authorizes this owner.
        assert!(mirror.install_permit(
            3,
            permit("p2", "o2", "http://127.0.0.1:5003", now_ms() + 30_000)
        ));
        assert!(!mirror.authorize("o1", &url("http://127.0.0.1:5003/x")));
    }

    #[test]
    fn a_revocation_without_an_epoch_change_denies_the_permitted_navigation() {
        let mirror = NavigationMirror::new();
        mirror.initialize(3, vec![owner("o1", "w1", "http://127.0.0.1:5001")]);
        // A permit that works, so "denied" below is a change of verdict and not
        // a mirror that never authorized anything.
        mirror.install_permit(
            3,
            permit("p0", "o1", "http://127.0.0.1:5009", now_ms() + 30_000),
        );
        assert!(mirror.authorize("o1", &url("http://127.0.0.1:5009/x")));

        mirror.install_permit(
            3,
            permit("p1", "o1", "http://127.0.0.1:5002", now_ms() + 30_000),
        );
        assert!(mirror.revoke_permit(3, "p1"), "same epoch: applied at once");
        assert!(
            !mirror.authorize("o1", &url("http://127.0.0.1:5002/x")),
            "the revoked permit must not authorize"
        );
        assert_eq!(mirror.epoch(), Some(3), "no epoch change was involved");
    }

    #[test]
    fn stale_epoch_events_are_dropped() {
        let mirror = NavigationMirror::new();
        mirror.initialize(4, vec![owner("o1", "w1", "http://127.0.0.1:5001")]);
        // A permit from the previous attachment (epoch 3) must not install.
        assert!(!mirror.install_permit(
            3,
            permit("p1", "o1", "http://127.0.0.1:5002", now_ms() + 30_000)
        ));
        assert!(!mirror.authorize("o1", &url("http://127.0.0.1:5002/x")));
        // A revoke and a binding change from the previous attachment are dropped.
        assert!(!mirror.revoke_permit(3, "p0"));
        assert!(!mirror.apply_owner_changed(
            3,
            MirrorOwner {
                generation: 9,
                current_origin: "http://127.0.0.1:5004".to_string(),
                workspace_id: Some("w1".to_string()),
                ..owner("o1", "w1", "http://127.0.0.1:5004")
            },
            true
        ));
        assert_eq!(
            mirror.owner_has_workspace("o1"),
            Some(false),
            "a stale binding change must not move the owner"
        );
        // The current epoch's events apply.
        assert!(mirror.apply_owner_changed(
            4,
            MirrorOwner {
                generation: 9,
                current_origin: "http://127.0.0.1:5005".to_string(),
                workspace_id: Some("w1".to_string()),
                ..owner("o1", "w1", "http://127.0.0.1:5005")
            },
            true
        ));
        assert_eq!(mirror.owner_has_workspace("o1"), Some(true));
        assert!(mirror.authorize("o1", &url("http://127.0.0.1:5005/x")));
        assert!(
            !mirror.authorize("o1", &url("http://127.0.0.1:5001/")),
            "the old committed origin is gone after the commit"
        );
    }

    #[test]
    fn expired_permits_never_authorize() {
        let mirror = NavigationMirror::new();
        mirror.initialize(3, vec![owner("o1", "w1", "http://127.0.0.1:5001")]);
        mirror.install_permit(3, permit("p1", "o1", "http://127.0.0.1:5002", now_ms() - 1));
        assert!(!mirror.authorize("o1", &url("http://127.0.0.1:5002/x")));
        assert_eq!(mirror.permit_count(), 0, "expiry prunes the permit");
    }

    #[test]
    fn label_and_owner_lookups_are_the_apps_label_map() {
        let mirror = NavigationMirror::new();
        mirror.initialize(
            3,
            vec![owner("o1", "native-landing", "http://127.0.0.1:5001")],
        );
        mirror.install_owner(owner("o2", "native-workspace-w1", "http://127.0.0.1:5001"));
        assert_eq!(
            mirror.owner_for_label("native-workspace-w1").as_deref(),
            Some("o2")
        );
        assert_eq!(
            mirror.label_for_owner("o1").as_deref(),
            Some("native-landing")
        );
        assert_eq!(mirror.owner_for_label("nope"), None);
        assert_eq!(mirror.owner_has_workspace("nope"), None);
    }
}
