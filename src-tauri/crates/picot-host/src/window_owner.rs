// ABOUTME: Owns per-window owner identity, bearer capability validation, and the
// ABOUTME: exact-origin navigation boundary for capability-bearing WebViews.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
// The URL crate the app framework's URL type re-exports; the host crate links
// it directly instead of through the framework (plan D1).
use url::Url;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::rngs::OsRng;
use rand::RngCore;

const CAPABILITY_LEN: usize = 32;
const OWNER_ID_LEN: usize = 16;
const PERMIT_ID_LEN: usize = 8;

/// Opaque per-window owner identity. Distinct from the bearer capability.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OwnerId(String);

pub type WorkspaceId = String;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)]
pub enum TemporaryKind {
    DefaultStartup,
    QuickChat,
    /// Cold-start landing owner: no workspace binding, placeholder home cwd,
    /// no runtime. Its first workspace entry is always a cross transition.
    Landing,
}

/// Why an owner record is gone (plan D11/T7). Retained after revocation —
/// together with the fact that the owner is no longer live — so the
/// session-prepare path can decide *atomically* whether a resident runtime may
/// be reattached to a new owner: a runtime claimed by a live attachment is
/// never stolen, while a runtime whose owner the app's own lifecycle
/// relinquished (a destroyed window, or a dead app attachment) may be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerRevocation {
    /// `owner.detach`: the window was destroyed while its app stayed alive.
    Detached,
    /// The manager attachment died (app quit, crash, SIGKILL).
    AttachmentDead,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum OwnerWorkspaceSnapshot {
    Registered {
        wid: WorkspaceId,
        root: PathBuf,
        generation: u64,
    },
    Temporary {
        root: PathBuf,
        generation: u64,
        temporary_kind: TemporaryKind,
    },
    NoWorkspace,
}

impl OwnerWorkspaceSnapshot {
    /// The workspace binding carried by this snapshot, if any. The label is
    /// never a state signal; this accessor is the one source of truth.
    pub fn workspace_id(&self) -> Option<&WorkspaceId> {
        match self {
            Self::Registered { wid, .. } => Some(wid),
            _ => None,
        }
    }
}

/// One navigation-mirror event, emitted by the registry when the daemon's
/// navigation authority changes state (plan Task 3, R1 server side).
///
/// Serialized shape is pinned to the protocol tables: `navigation_permit`,
/// `permit_revoked`, `owner_binding_changed` — each carrying the granting
/// attachment `epoch` so the app can drop stale-epoch events (D11).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OwnerMirrorEvent {
    /// A permit the app's synchronous navigation delegate must install before
    /// the WebView can load `origin`. The granting op waits for `mirror_ack`.
    NavigationPermit {
        epoch: u64,
        #[serde(rename = "permitId")]
        permit_id: String,
        owner: String,
        origin: String,
        #[serde(rename = "expiresAtMs")]
        expires_at_ms: u64,
        generation: u64,
    },
    /// A permit that will never be usable (cancel, revoke, ack timeout): the
    /// app removes it from its mirror immediately.
    PermitRevoked {
        epoch: u64,
        #[serde(rename = "permitId")]
        permit_id: String,
    },
    /// An owner record appeared or changed: its full mirror row plus the
    /// attachment `epoch`.
    ///
    /// Emitted on **owner creation** and on **transition commit**, because the
    /// app's navigation mirror is the delegate's authority (plan D4/Task 5): a
    /// workspace owner is minted daemon-side by the `open_workspace` dispatcher,
    /// so without this event the app would know nothing about the new window's
    /// origin and would deny its first load. The row therefore carries the
    /// daemon-normalized `currentOrigin`, the label (the app's label↔owner map)
    /// and the route, exactly like an `admin_welcome` snapshot row.
    OwnerBindingChanged {
        epoch: u64,
        owner: String,
        label: String,
        #[serde(rename = "hasWorkspace")]
        has_workspace: bool,
        #[serde(rename = "currentOrigin")]
        current_origin: String,
        #[serde(rename = "workspaceId")]
        workspace_id: Option<String>,
        #[serde(rename = "sessionRoute")]
        session_route: Option<String>,
        generation: u64,
    },
}

/// Receives [`OwnerMirrorEvent`]s. Mirrors [`crate::host_control::HostEventSink`]:
/// the registry pushes, the admin channel forwards. Always invoked *after* the
/// registry lock is released, so an implementation may call back into the
/// registry without deadlocking.
pub type OwnerMirrorSink = Arc<dyn Fn(OwnerMirrorEvent) + Send + Sync>;

/// One owner row of the attach snapshot (`admin_welcome.mirror.owners`).
#[derive(Clone, Debug, Serialize)]
pub struct OwnerMirrorEntry {
    pub owner: String,
    pub label: String,
    #[serde(rename = "currentOrigin")]
    pub current_origin: String,
    #[serde(rename = "workspaceId")]
    pub workspace_id: Option<String>,
    /// Route path of the owner's current origin (e.g.
    /// `/workspaces/<wid>/sessions/<sid>`); `None` for the bare origin root.
    #[serde(rename = "sessionRoute")]
    pub session_route: Option<String>,
    pub generation: u64,
}

/// Shared registry of window owners, their capabilities, and navigation permits.
#[derive(Clone, Default)]
pub struct WindowOwnerRegistry {
    inner: Arc<Mutex<RegistryState>>,
}

#[derive(Default)]
struct RegistryState {
    owners: HashMap<OwnerId, OwnerRecord>,
    // O(1) capability → owner lookup. Kept 1:1 with `owners` on insert and
    // revoke, so authentication stays constant-time over the owner count.
    capability_index: HashMap<[u8; CAPABILITY_LEN], OwnerId>,
    next_generation: u64,
    /// Attachment epoch new owners are stamped with (plan D3/D11). The daemon
    /// sets it when it grants the manager lease; owners created by that
    /// attachment carry it, so a dead attachment can be revoked as a set.
    attachment_epoch: u64,
    /// Owner-mirror event sink, when a manager is attached.
    mirror_sink: Option<OwnerMirrorSink>,
    /// Lifecycle reason for every revoked owner (plan D11/T7). The record
    /// itself is deleted on revoke — the capability index and the label must
    /// die with it — so this is the only surviving evidence that an owner
    /// *existed* and how it went away. Bounded by the number of window/owner
    /// terminations in one daemon lifetime (one small entry each); pruning it
    /// would make an old runtime unreachable-but-unrebindable, which is the
    /// failure mode this map exists to prevent.
    revoked: HashMap<OwnerId, OwnerRevocation>,
}

struct OwnerRecord {
    window_label: String,
    canonical_cwd: PathBuf,
    primary_port: u16,
    current_origin: String,
    pending: Option<PendingNavigation>,
    transition: Option<WorkspaceTransition>,
    capability_bytes: [u8; CAPABILITY_LEN],
    workspace_id: Option<WorkspaceId>,
    temporary_kind: TemporaryKind,
    workspace_generation: u64,
    /// The attachment epoch that created this owner (plan D11): the unit the
    /// daemon revokes when the app attachment dies.
    attachment_epoch: u64,
    /// Route path of `current_origin`, when it is not the bare origin root.
    session_route: Option<String>,
}

struct PendingNavigation {
    /// Mirror permit id handed to the app (`navigation_permit.permitId`).
    permit_id: String,
    origin: String,
    target_session_id: Option<String>,
    #[allow(dead_code)]
    target_port: u16,
    #[allow(dead_code)]
    target_cwd: PathBuf,
    transition_generation: u64,
    expires_at: Instant,
    consumed: bool,
    /// Set only when this transition SPAWNED its runtime. Cancel may stop
    /// that instance; a reused live runtime must survive cancellation.
    created_instance: Option<String>,
}

struct WorkspaceTransition {
    generation: u64,
    target_cwd: PathBuf,
    #[allow(dead_code)]
    target_port: u16,
}

impl OwnerId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[allow(dead_code)]
    pub fn from_string(value: String) -> Self {
        Self(value)
    }
}

impl WindowOwnerRegistry {
    #[allow(dead_code)]
    pub fn create_owner(
        &self,
        window_label: String,
        canonical_cwd: PathBuf,
        primary_port: u16,
        current_origin: String,
    ) -> Result<(OwnerId, String), String> {
        self.create_owner_with_workspace(
            window_label,
            canonical_cwd,
            primary_port,
            current_origin,
            None,
            TemporaryKind::DefaultStartup,
        )
    }

    pub fn create_owner_with_workspace(
        &self,
        window_label: String,
        canonical_cwd: PathBuf,
        primary_port: u16,
        current_origin: String,
        workspace_id: Option<WorkspaceId>,
        temporary_kind: TemporaryKind,
    ) -> Result<(OwnerId, String), String> {
        let origin = normalize_origin(&current_origin)
            .ok_or_else(|| "owner origin must be an http loopback host origin".to_string())?;

        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        if state
            .owners
            .values()
            .any(|r| r.window_label == window_label)
        {
            return Err("window label is already owned".to_string());
        }

        let attachment_epoch = state.attachment_epoch;
        let mut capability_bytes = [0u8; CAPABILITY_LEN];
        OsRng.fill_bytes(&mut capability_bytes);
        let mut id_bytes = [0u8; OWNER_ID_LEN];
        OsRng.fill_bytes(&mut id_bytes);

        let owner_id = OwnerId(hex_encode(&id_bytes));
        let capability = URL_SAFE_NO_PAD.encode(capability_bytes);
        // The mirror row's values, captured before they move into the record.
        let mirror_label = window_label.clone();
        let mirror_origin = origin.clone();
        let mirror_workspace_id = workspace_id.clone();
        state
            .capability_index
            .insert(capability_bytes, owner_id.clone());
        state.owners.insert(
            owner_id.clone(),
            OwnerRecord {
                window_label,
                canonical_cwd,
                primary_port,
                current_origin: origin,
                pending: None,
                transition: None,
                capability_bytes,
                workspace_id,
                temporary_kind,
                workspace_generation: 0,
                attachment_epoch,
                session_route: None,
            },
        );
        // Publish the new owner's mirror row (plan Task 5). The app's navigation
        // delegate reads its own mirror, and a workspace owner is minted here
        // (by the `open_workspace` dispatcher), not by the app: without this
        // event the new window's first load would be denied fail-closed. Emitted
        // after the lock is released, like every other mirror event.
        let event = OwnerMirrorEvent::OwnerBindingChanged {
            epoch: attachment_epoch,
            owner: owner_id.as_str().to_string(),
            label: mirror_label,
            has_workspace: mirror_workspace_id.is_some(),
            current_origin: mirror_origin,
            workspace_id: mirror_workspace_id,
            session_route: None,
            generation: 0,
        };
        let sink = state.mirror_sink.clone();
        drop(state);
        if let Some(sink) = sink {
            sink(event);
        }
        Ok((owner_id, capability))
    }

    pub fn authenticate(&self, capability: &str) -> Option<OwnerId> {
        let decoded = URL_SAFE_NO_PAD.decode(capability).ok()?;
        if decoded.len() != CAPABILITY_LEN {
            return None;
        }
        let mut bytes = [0u8; CAPABILITY_LEN];
        bytes.copy_from_slice(&decoded);

        let state = self.inner.lock().expect("owner registry lock poisoned");
        // O(1) direct lookup. The key is the full 256-bit capability, so
        // timing no longer varies with the number of owners.
        state.capability_index.get(&bytes).cloned()
    }

    pub fn prepare_navigation(
        &self,
        owner: &OwnerId,
        port: u16,
        canonical_cwd: PathBuf,
        origin: String,
        ttl: Duration,
    ) -> Result<u64, String> {
        self.prepare_navigation_with_permit(owner, port, canonical_cwd, origin, ttl)
            .map(|(generation, _permit_id)| generation)
    }

    /// Same as [`Self::prepare_navigation`], but also returns the mirror permit
    /// id the naviation-mirror event carries, so the caller can await the app's
    /// `mirror_ack` for exactly this permit (plan Task 3, R1).
    pub fn prepare_navigation_with_permit(
        &self,
        owner: &OwnerId,
        port: u16,
        canonical_cwd: PathBuf,
        origin: String,
        ttl: Duration,
    ) -> Result<(u64, String), String> {
        let target_session_id = origin
            .split_once("/sessions/")
            .and_then(|(_, session_id)| session_id.trim_end_matches('/').split('/').next())
            .filter(|session_id| !session_id.is_empty())
            .map(str::to_owned);
        let origin = normalize_origin(&origin)
            .ok_or_else(|| "navigation origin must be an http loopback host origin".to_string())?;
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        let owner_record = state
            .owners
            .get(owner)
            .ok_or_else(|| "unknown owner".to_string())?;
        let generation =
            if canonical_cwd == owner_record.canonical_cwd && owner_record.workspace_id.is_some() {
                // Session navigation within one registered workspace only changes
                // the WebView origin. Keep every runtime in the workspace
                // authorized under the same generation so an active background
                // turn keeps its events. A Landing/temporary owner never takes
                // this branch: its placeholder home is not a workspace binding,
                // so its first workspace entry is always a cross transition
                // ("Landing is never same-workspace"), even when the placeholder
                // path is itself a registered workspace.
                owner_record.workspace_generation
            } else if let Some(transition) = owner_record.transition.as_ref() {
                transition.generation
            } else {
                state.next_generation += 1;
                state.next_generation
            };
        let permit_id = random_id(PERMIT_ID_LEN);
        let epoch = state.attachment_epoch;
        let expires_at = Instant::now() + ttl;
        let expires_at_ms = now_ms() + ttl.as_millis() as u64;
        let permit_origin = origin.clone();
        let record = state
            .owners
            .get_mut(owner)
            .ok_or_else(|| "unknown owner".to_string())?;
        record.pending = Some(PendingNavigation {
            permit_id: permit_id.clone(),
            origin,
            target_session_id,
            target_port: port,
            target_cwd: canonical_cwd,
            transition_generation: generation,
            expires_at,
            consumed: false,
            created_instance: None,
        });
        let sink = state.mirror_sink.clone();
        // Emitted after the lock is released: the sink forwards over the admin
        // channel and must never run under the registry mutex.
        drop(state);
        if let Some(sink) = sink {
            sink(OwnerMirrorEvent::NavigationPermit {
                epoch,
                permit_id: permit_id.clone(),
                owner: owner.as_str().to_string(),
                origin: permit_origin,
                expires_at_ms,
                generation,
            });
        }
        Ok((generation, permit_id))
    }

    pub fn authorize_navigation(&self, owner: &OwnerId, url: &Url) -> bool {
        // Wry invokes this delegate for every navigation, including iframe
        // subframes, and does not expose the target frame. Sandboxed `srcdoc`
        // iframes (the HTML file preview) navigate to about:srcdoc, whose
        // content comes entirely from the embedding document — there is no
        // network origin to authorize, so allow the opaque about: documents.
        // Top-level navigation to them never occurs in this app.
        if url.scheme() == "about" {
            let path = url.path();
            return path == "srcdoc" || path == "blank";
        }
        let Some(origin) = url_origin(url) else {
            return false;
        };
        let mut state = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        let Some(record) = state.owners.get_mut(owner) else {
            return false;
        };
        if origin == record.current_origin {
            return true;
        }
        if let Some(pending) = record.pending.as_mut() {
            if !pending.consumed && Instant::now() < pending.expires_at && origin == pending.origin
            {
                pending.consumed = true;
                return true;
            }
        }
        false
    }

    pub fn begin_workspace_transition(
        &self,
        owner: &OwnerId,
        target_cwd: PathBuf,
        target_port: u16,
    ) -> Result<u64, String> {
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        if state
            .owners
            .get(owner)
            .ok_or_else(|| "unknown owner".to_string())?
            .transition
            .is_some()
        {
            return Err("a workspace transition is already in progress".to_string());
        }
        state.next_generation += 1;
        let generation = state.next_generation;
        let record = state
            .owners
            .get_mut(owner)
            .ok_or_else(|| "unknown owner".to_string())?;
        record.transition = Some(WorkspaceTransition {
            generation,
            target_cwd,
            target_port,
        });
        Ok(generation)
    }

    #[allow(dead_code)]
    pub fn commit_workspace_transition(
        &self,
        owner: &OwnerId,
        transition_generation: u64,
        target_origin: String,
    ) -> Result<(), String> {
        self.commit_workspace_transition_with_workspace(
            owner,
            transition_generation,
            target_origin,
            None,
            TemporaryKind::DefaultStartup,
        )
    }

    pub fn commit_workspace_transition_with_workspace(
        &self,
        owner: &OwnerId,
        transition_generation: u64,
        target_origin: String,
        workspace_id: Option<WorkspaceId>,
        temporary_kind: TemporaryKind,
    ) -> Result<(), String> {
        let target = normalize_origin(&target_origin)
            .ok_or_else(|| "commit origin must be an http loopback host origin".to_string())?;
        let target_origin_for_route = Some(target_origin.clone());
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        let epoch = state.attachment_epoch;
        let record = state
            .owners
            .get_mut(owner)
            .ok_or_else(|| "unknown owner".to_string())?;

        let (target_port, new_cwd) = {
            let pending = record
                .pending
                .as_ref()
                .ok_or_else(|| "no pending navigation permit to commit".to_string())?;
            if pending.transition_generation != transition_generation {
                return Err("transition generation mismatch".to_string());
            }
            if pending.origin != target {
                return Err("commit origin does not match the prepared permit".to_string());
            }
            let new_cwd = record
                .transition
                .as_ref()
                .filter(|t| t.generation == transition_generation)
                .map(|t| t.target_cwd.clone());
            (pending.target_port, new_cwd)
        };

        record.primary_port = target_port;
        if let Some(cwd) = new_cwd {
            record.canonical_cwd = cwd;
        }
        record.workspace_id = workspace_id;
        record.temporary_kind = temporary_kind;
        record.current_origin = target;
        record.workspace_generation = transition_generation;
        record.session_route = session_route_for(target_origin_for_route.as_deref());
        record.pending = None;
        record.transition = None;
        let event = OwnerMirrorEvent::OwnerBindingChanged {
            epoch,
            owner: owner.as_str().to_string(),
            label: record.window_label.clone(),
            has_workspace: record.workspace_id.is_some(),
            current_origin: record.current_origin.clone(),
            workspace_id: record.workspace_id.clone(),
            session_route: record.session_route.clone(),
            generation: transition_generation,
        };
        let sink = state.mirror_sink.clone();
        drop(state);
        if let Some(sink) = sink {
            sink(event);
        }
        Ok(())
    }

    /// True iff the owner record currently carries a workspace binding.
    /// Menu/Cmd+N gate key — never the window label.
    pub fn owner_has_workspace(&self, owner: &OwnerId) -> bool {
        self.owner_current_workspace(owner).workspace_id().is_some()
    }

    /// Return one coherent owner/workspace view. All fields come from one lock
    /// snapshot; callers must not combine the legacy split accessors.
    #[allow(dead_code)]
    pub fn owner_current_workspace(&self, owner: &OwnerId) -> OwnerWorkspaceSnapshot {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        let Some(record) = state.owners.get(owner) else {
            return OwnerWorkspaceSnapshot::NoWorkspace;
        };
        match &record.workspace_id {
            Some(wid) => OwnerWorkspaceSnapshot::Registered {
                wid: wid.clone(),
                root: record.canonical_cwd.clone(),
                generation: record.workspace_generation,
            },
            None => OwnerWorkspaceSnapshot::Temporary {
                root: record.canonical_cwd.clone(),
                generation: record.workspace_generation,
                temporary_kind: record.temporary_kind,
            },
        }
    }

    pub fn current_workspace(&self, owner: &OwnerId) -> Option<(PathBuf, u16)> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state
            .owners
            .get(owner)
            .map(|r| (r.canonical_cwd.clone(), r.primary_port))
    }

    pub fn workspace_transition_in_progress(&self, owner: &OwnerId) -> bool {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state
            .owners
            .get(owner)
            .is_some_and(|record| record.transition.is_some())
    }

    /// The workspace transition generation that established the owner's current
    /// workspace binding. Ephemeral records snapshot this at creation so
    /// cross-workspace cleanup can identify old-workspace chats.
    pub fn current_workspace_generation(&self, owner: &OwnerId) -> Option<u64> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state.owners.get(owner).map(|r| r.workspace_generation)
    }

    /// The pending navigation permit's target origin, if one is prepared but not
    /// yet committed. Used by the workspace-transition commit path.
    pub fn pending_target_origin(&self, owner: &OwnerId) -> Option<String> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state
            .owners
            .get(owner)
            .and_then(|r| r.pending.as_ref().map(|p| p.origin.clone()))
    }

    /// The pending navigation permit's session ID. It is preserved separately
    /// because origin normalization intentionally removes route path segments.
    pub fn pending_target_session_id(&self, owner: &OwnerId) -> Option<String> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state
            .owners
            .get(owner)
            .and_then(|record| record.pending.as_ref()?.target_session_id.clone())
    }

    /// The pending navigation permit's canonical target cwd, if one is prepared.
    pub fn pending_target_cwd(&self, owner: &OwnerId) -> Option<PathBuf> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state
            .owners
            .get(owner)
            .and_then(|r| r.pending.as_ref().map(|p| p.target_cwd.clone()))
    }

    /// The runtime instance this pending transition spawned, if any. A reused
    /// live runtime never records one, so cancel cannot stop it.
    pub fn pending_created_instance(&self, owner: &OwnerId) -> Option<String> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state
            .owners
            .get(owner)
            .and_then(|record| record.pending.as_ref()?.created_instance.clone())
    }

    /// Stamp the pending record with the runtime instance the transition
    /// spawned. Ignored when the pending record was replaced (generation
    /// mismatch) or already consumed.
    pub fn record_pending_created_instance(
        &self,
        owner: &OwnerId,
        transition_generation: u64,
        instance_id: &str,
    ) {
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        if let Some(record) = state.owners.get_mut(owner) {
            if let Some(pending) = record.pending.as_mut() {
                if pending.transition_generation == transition_generation {
                    pending.created_instance = Some(instance_id.to_string());
                }
            }
        }
    }

    /// Resolve the owner bound to a window label (used by the close lifecycle).
    pub fn owner_for_label(&self, label: &str) -> Option<OwnerId> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state
            .owners
            .iter()
            .find(|(_, r)| r.window_label == label)
            .map(|(owner, _)| owner.clone())
    }

    /// The window label bound to an owner.
    pub fn label_for_owner(&self, owner: &OwnerId) -> Option<String> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state.owners.get(owner).map(|r| r.window_label.clone())
    }

    /// Cancel an in-progress workspace transition for this owner, retaining the
    /// old workspace binding. Idempotent when nothing is in progress; rejects a
    /// mismatched generation so a stale cancel cannot clear a newer transition.
    pub fn validate_workspace_transition_generation(
        &self,
        owner: &OwnerId,
        transition_generation: u64,
    ) -> Result<(), String> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        let record = state
            .owners
            .get(owner)
            .ok_or_else(|| "unknown owner".to_string())?;
        let matches = record
            .pending
            .as_ref()
            .is_some_and(|pending| pending.transition_generation == transition_generation)
            || record
                .transition
                .as_ref()
                .is_some_and(|transition| transition.generation == transition_generation);
        if matches {
            Ok(())
        } else {
            Err("transition generation mismatch".to_string())
        }
    }

    pub fn cancel_workspace_transition(
        &self,
        owner: &OwnerId,
        transition_generation: u64,
    ) -> Result<(), String> {
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        let Some(record) = state.owners.get_mut(owner) else {
            return Err("unknown owner".to_string());
        };
        let pending_gen = record.pending.as_ref().map(|p| p.transition_generation);
        let transition_gen = record.transition.as_ref().map(|t| t.generation);
        if pending_gen.is_none() && transition_gen.is_none() {
            return Ok(());
        }
        if pending_gen.is_some_and(|g| g != transition_generation)
            || transition_gen.is_some_and(|g| g != transition_generation)
        {
            return Err("transition generation mismatch".to_string());
        }
        let revoked_permit_id = record.pending.as_ref().map(|p| p.permit_id.clone());
        record.pending = None;
        record.transition = None;
        let epoch = state.attachment_epoch;
        let sink = state.mirror_sink.clone();
        drop(state);
        if let Some(sink) = sink {
            if let Some(permit_id) = revoked_permit_id {
                sink(OwnerMirrorEvent::PermitRevoked { epoch, permit_id });
            }
        }
        Ok(())
    }

    /// Drop an unconsumed pending permit without touching the transition, and
    /// tell the mirror to revoke it. Used as the fail-safe when a granting op
    /// never got its `mirror_ack` (plan Task 3, R1): the WebView delegate must
    /// never keep a permit the app did not install.
    pub fn cancel_pending_permit(&self, owner: &OwnerId) -> Option<String> {
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        let record = state.owners.get_mut(owner)?;
        let permit_id = record.pending.take()?.permit_id;
        let epoch = state.attachment_epoch;
        let sink = state.mirror_sink.clone();
        drop(state);
        if let Some(sink) = sink {
            sink(OwnerMirrorEvent::PermitRevoked {
                epoch,
                permit_id: permit_id.clone(),
            });
        }
        Some(permit_id)
    }

    /// The unconsumed pending permit's mirror id, when one is prepared.
    pub fn pending_permit_id(&self, owner: &OwnerId) -> Option<String> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state
            .owners
            .get(owner)
            .and_then(|record| record.pending.as_ref())
            .filter(|pending| !pending.consumed)
            .map(|pending| pending.permit_id.clone())
    }

    /// Install the owner-mirror event sink (set by the daemon once an admin
    /// channel exists; plan Task 3 Step 2).
    pub fn set_mirror_sink(&self, sink: OwnerMirrorSink) {
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        state.mirror_sink = Some(sink);
    }

    /// The attachment epoch new owners are stamped with.
    pub fn attachment_epoch(&self) -> u64 {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state.attachment_epoch
    }

    /// Stamp subsequent owners with `epoch`. Called when the manager lease is
    /// granted (plan D3/D11); owners already created keep their own epoch.
    pub fn set_attachment_epoch(&self, epoch: u64) {
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        state.attachment_epoch = epoch;
    }

    /// Revoke every owner created by `epoch` — capabilities included — and
    /// return them so the caller can run the per-owner detach cleanup. Labels
    /// are freed with the records, so a later attachment can recreate them
    /// (plan D11). Pending permits of the revoked owners are reported to the
    /// mirror as `permit_revoked`.
    pub fn revoke_attachment(&self, epoch: u64) -> Vec<OwnerId> {
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        let revoked: Vec<OwnerId> = state
            .owners
            .iter()
            .filter(|(_, record)| record.attachment_epoch == epoch)
            .map(|(owner, _)| owner.clone())
            .collect();
        let mut events = Vec::new();
        for owner in &revoked {
            state
                .revoked
                .insert(owner.clone(), OwnerRevocation::AttachmentDead);
            if let Some(record) = state.owners.remove(owner) {
                state.capability_index.remove(&record.capability_bytes);
                if let Some(pending) = record.pending {
                    events.push(OwnerMirrorEvent::PermitRevoked {
                        epoch,
                        permit_id: pending.permit_id,
                    });
                }
            }
        }
        let sink = state.mirror_sink.clone();
        drop(state);
        if let Some(sink) = sink {
            for event in events {
                sink(event);
            }
        }
        revoked
    }

    /// The full attach snapshot (`admin_welcome.mirror`): one row per live
    /// owner, taken under a single lock so the app sees a coherent picture.
    pub fn mirror_snapshot(&self) -> Vec<OwnerMirrorEntry> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state
            .owners
            .iter()
            .map(|(owner, record)| OwnerMirrorEntry {
                owner: owner.as_str().to_string(),
                label: record.window_label.clone(),
                current_origin: record.current_origin.clone(),
                workspace_id: record.workspace_id.clone(),
                session_route: record.session_route.clone(),
                generation: record.workspace_generation,
            })
            .collect()
    }

    /// One owner's mirror row, or `None` for an unknown owner.
    ///
    /// The `owner.create` op returns this row (Task 5): owners created *after*
    /// the attach snapshot are the app's only source for their committed
    /// origin, and the reply must carry the daemon-normalized values rather
    /// than the arguments the app sent.
    pub fn owner_mirror_row(&self, owner: &OwnerId) -> Option<OwnerMirrorEntry> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state.owners.get(owner).map(|record| OwnerMirrorEntry {
            owner: owner.as_str().to_string(),
            label: record.window_label.clone(),
            current_origin: record.current_origin.clone(),
            workspace_id: record.workspace_id.clone(),
            session_route: record.session_route.clone(),
            generation: record.workspace_generation,
        })
    }

    /// Revoke one owner because its window was destroyed while the app stayed
    /// alive (the `owner.detach` op, plan D6). Removes the record (capability
    /// and label die with it) and *remembers* the owner as deliberately
    /// detached, so a later session-prepare may reattach its resident runtime.
    pub fn detach_owner(&self, owner: &OwnerId) {
        self.revoke_owner_with_reason(owner, OwnerRevocation::Detached);
    }

    /// Whether `owner` may be reattached: it is no longer live (its record and
    /// capability are gone) and it went away through the app's own lifecycle.
    /// A live owner is never reattachable (plan D11).
    pub fn owner_is_reattachable(&self, owner: &OwnerId) -> bool {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        !state.owners.contains_key(owner) && state.revoked.contains_key(owner)
    }

    /// Retained revocation reason, for diagnostics and tests.
    pub fn revocation_of(&self, owner: &OwnerId) -> Option<OwnerRevocation> {
        let state = self.inner.lock().expect("owner registry lock poisoned");
        state.revoked.get(owner).copied()
    }

    pub fn revoke_owner(&self, owner: &OwnerId) {
        self.revoke_owner_with_reason(owner, OwnerRevocation::Detached);
    }

    fn revoke_owner_with_reason(&self, owner: &OwnerId, reason: OwnerRevocation) {
        let mut state = self.inner.lock().expect("owner registry lock poisoned");
        state.revoked.entry(owner.clone()).or_insert(reason);
        let mut events = Vec::new();
        if let Some(record) = state.owners.remove(owner) {
            state.capability_index.remove(&record.capability_bytes);
            if let Some(pending) = record.pending {
                events.push(OwnerMirrorEvent::PermitRevoked {
                    epoch: record.attachment_epoch,
                    permit_id: pending.permit_id,
                });
            }
        }
        let sink = state.mirror_sink.clone();
        drop(state);
        if let Some(sink) = sink {
            for event in events {
                sink(event);
            }
        }
    }
}

/// Static initialization script that exposes the bearer capability only when
/// the document is Picot's canonical native http loopback origin. Defense in
/// depth behind the host navigation callback; never an authorization boundary.
/// Loopback-agnostic capability injection script. The capability is bound to
/// the window owner (not to a specific Pi port), so it is exposed on every
/// loopback HTTP origin the WebView loads. This keeps the capability available
/// across a cross-port workspace navigation, where the document reloads at a new
/// loopback port. The exact-origin boundary is enforced by `authorize_navigation`
/// (the hard gate); this script is defense-in-depth and never an authorization
/// boundary.
pub fn capability_initialization_script(capability: &str) -> String {
    let token = serde_json::to_string(capability).expect("capability JSON");
    format!(
        "if (window.location.protocol === 'http:' && [\"127.0.0.1\",\"localhost\",\"::1\",\"[::1]\"].includes(window.location.hostname)) {{ Object.defineProperty(window, '__PICOT_NATIVE_CAPABILITY__', {{ value: {token}, configurable: true }}); }}"
    )
}

fn normalize_origin(origin: &str) -> Option<String> {
    let url: Url = origin.parse().ok()?;
    url_origin(&url)
}

/// The one navigation-origin rule, shared by the daemon's authorization
/// registry and the app's mirror-fed delegate (plan D4, Task 5 R1).
///
/// Exported because the two sides must not drift: the daemon publishes
/// `currentOrigin`/`permit.origin` strings it produced with this function, and
/// the app compares a live navigation URL against them. The app crate sees the
/// same `url` type (the app framework re-exports it), so it calls this directly
/// instead of re-implementing the loopback/port rules.
///
/// `None` means "not a loopback http origin": `127.0.0.1`, `::1` and
/// `localhost` are accepted; everything else (https, credentials in the URL,
/// `file:`, `data:`, LAN addresses) is not.
pub fn navigation_origin(url: &Url) -> Option<String> {
    url_origin(url)
}

/// The opaque `about:` documents a navigation may always reach: sandboxed
/// `srcdoc` iframes (the HTML file preview) navigate to `about:srcdoc`, whose
/// content comes entirely from the embedding document — there is no network
/// origin to authorize. Top-level navigation to them never occurs in this app.
///
/// This is the *only* allow-rule that survives a dropped admin connection (plan
/// Task 5 R1: the mirror is cleared fail-closed and the delegate still permits
/// exactly these two strings).
pub fn is_about_document(url: &Url) -> bool {
    url.scheme() == "about" && matches!(url.path(), "srcdoc" | "blank")
}

fn url_origin(url: &Url) -> Option<String> {
    if url.scheme() != "http" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?;
    if !is_loopback_host(host) {
        return None;
    }
    let port = url.port_or_known_default()?;
    Some(format!("http://{host}:{port}"))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "[::1]" | "localhost")
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Random hex id for mirror permits (plan Task 3 protocol table).
fn random_id(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    OsRng.fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

/// Wall-clock milliseconds since the Unix epoch; the mirror's `expiresAtMs`
/// unit (the app only compares it against its own clock).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Owner-record route path for the mirror snapshot: the path component of the
/// raw navigation origin, or `None` when it is the bare origin root.
fn session_route_for(origin: Option<&str>) -> Option<String> {
    let url: Url = origin?.parse().ok()?;
    let path = url.path().to_string();
    if path.is_empty() || path == "/" {
        None
    } else {
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        s.parse().expect("valid url")
    }

    fn new_owner(reg: &WindowOwnerRegistry, label: &str, port: u16) -> (OwnerId, String) {
        reg.create_owner(
            label.to_string(),
            PathBuf::from("/workspace"),
            port,
            format!("http://127.0.0.1:{port}"),
        )
        .expect("owner created")
    }

    #[test]
    fn capability_is_32_random_url_safe_bytes() {
        let reg = WindowOwnerRegistry::default();
        let (owner, capability) = new_owner(&reg, "w1", 3001);
        assert_eq!(owner.0.len(), 32); // 16 bytes hex
        let decoded = URL_SAFE_NO_PAD.decode(&capability).expect("decodes");
        assert_eq!(decoded.len(), CAPABILITY_LEN);
        for ch in capability.chars() {
            assert!(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
            assert!(ch != '='); // no padding
        }
    }

    #[test]
    fn authenticate_validates_in_constant_time() {
        let reg = WindowOwnerRegistry::default();
        let (owner, capability) = new_owner(&reg, "w1", 3001);

        assert_eq!(reg.authenticate(&capability), Some(owner.clone()));

        // Tamper one byte -> still 32 bytes, must not match.
        let bytes = URL_SAFE_NO_PAD.decode(&capability).unwrap();
        let mut flipped = bytes.clone();
        flipped[0] ^= 0xff;
        let tampered = URL_SAFE_NO_PAD.encode(&flipped);
        assert_eq!(reg.authenticate(&tampered), None);

        // Wrong length / garbage -> None, never panics.
        assert_eq!(reg.authenticate("short"), None);
        assert_eq!(reg.authenticate(""), None);
    }

    #[test]
    fn window_label_can_be_reused_after_revocation() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        assert!(reg
            .create_owner(
                "w1".to_string(),
                PathBuf::from("/ws"),
                3002,
                "http://127.0.0.1:3002".to_string(),
            )
            .is_err());
        reg.revoke_owner(&owner);
        assert!(new_owner(&reg, "w1", 3003).0 != owner);
    }

    #[test]
    fn owner_snapshot_reports_registered_workspace() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = reg
            .create_owner_with_workspace(
                "registered".to_string(),
                PathBuf::from("/workspace"),
                3001,
                "http://127.0.0.1:3001".to_string(),
                Some("wid-1".to_string()),
                TemporaryKind::DefaultStartup,
            )
            .unwrap();
        assert_eq!(
            reg.owner_current_workspace(&owner),
            OwnerWorkspaceSnapshot::Registered {
                wid: "wid-1".to_string(),
                root: PathBuf::from("/workspace"),
                generation: 0,
            }
        );
    }

    #[test]
    fn owner_snapshot_reports_temporary_without_workspace_id() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = reg
            .create_owner_with_workspace(
                "temporary".to_string(),
                PathBuf::from("/tmp/picot"),
                3001,
                "http://127.0.0.1:3001".to_string(),
                None,
                TemporaryKind::QuickChat,
            )
            .unwrap();
        assert_eq!(
            reg.owner_current_workspace(&owner),
            OwnerWorkspaceSnapshot::Temporary {
                root: PathBuf::from("/tmp/picot"),
                generation: 0,
                temporary_kind: TemporaryKind::QuickChat,
            }
        );
    }

    #[test]
    fn owner_snapshot_reports_no_workspace_for_unknown_owner() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "missing", 3001);
        reg.revoke_owner(&owner);
        assert_eq!(
            reg.owner_current_workspace(&owner),
            OwnerWorkspaceSnapshot::NoWorkspace
        );
    }

    #[test]
    fn transition_snapshot_updates_identity_atomically() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "transition", 3001);
        let generation = reg
            .begin_workspace_transition(&owner, PathBuf::from("/workspace-b"), 3002)
            .unwrap();
        reg.prepare_navigation(
            &owner,
            3002,
            PathBuf::from("/workspace-b"),
            "http://127.0.0.1:3002".to_string(),
            Duration::from_secs(5),
        )
        .unwrap();
        reg.commit_workspace_transition_with_workspace(
            &owner,
            generation,
            "http://127.0.0.1:3002".to_string(),
            Some("wid-b".to_string()),
            TemporaryKind::DefaultStartup,
        )
        .unwrap();
        assert_eq!(
            reg.owner_current_workspace(&owner),
            OwnerWorkspaceSnapshot::Registered {
                wid: "wid-b".to_string(),
                root: PathBuf::from("/workspace-b"),
                generation,
            }
        );
    }

    #[test]
    fn current_exact_origin_is_authorized() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        assert!(reg.authorize_navigation(&owner, &url("http://127.0.0.1:3001/chat")));
        // Same host, different port is not equivalent.
        assert!(!reg.authorize_navigation(&owner, &url("http://127.0.0.1:3002/chat")));
    }

    #[test]
    fn sandboxed_about_documents_are_authorized_for_subframes() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        // Sandboxed srcdoc iframes (HTML file preview) navigate to about:srcdoc;
        // their content comes from the embedding document, not a network origin.
        assert!(reg.authorize_navigation(&owner, &url("about:srcdoc")));
        assert!(reg.authorize_navigation(&owner, &url("about:blank")));
        // Other about: paths (or non-http schemes) stay unauthorized.
        assert!(!reg.authorize_navigation(&owner, &url("about:config")));
        assert!(!reg.authorize_navigation(&owner, &url("data:text/html,<h1>x</h1>")));
    }

    #[test]
    fn prepared_pending_origin_is_authorized_within_ttl() {
        let reg = WindowOwnerRegistry::default();
        // Same-cwd retention applies to workspace-bound owners — the shape of
        // every real same-workspace navigation.
        let (owner, _) = reg
            .create_owner_with_workspace(
                "w1".to_string(),
                PathBuf::from("/workspace"),
                3001,
                "http://127.0.0.1:3001".to_string(),
                Some("wid-1".to_string()),
                TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let gen = reg
            .prepare_navigation(
                &owner,
                3002,
                PathBuf::from("/workspace"),
                "http://127.0.0.1:3002".to_string(),
                Duration::from_secs(5),
            )
            .expect("prepare");
        assert_eq!(gen, 0);
        assert!(reg.authorize_navigation(&owner, &url("http://127.0.0.1:3002/x")));
        // A pending destination is a one-shot bearer permit.
        assert!(!reg.authorize_navigation(&owner, &url("http://127.0.0.1:3002/again")));
        // Old origin remains valid alongside the pending permit.
        assert!(reg.authorize_navigation(&owner, &url("http://127.0.0.1:3001/x")));
    }

    #[test]
    fn expired_permit_is_not_authorized_but_current_remains() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        reg.prepare_navigation(
            &owner,
            3002,
            PathBuf::from("/workspace"),
            "http://127.0.0.1:3002".to_string(),
            Duration::ZERO,
        )
        .expect("prepare");
        assert!(!reg.authorize_navigation(&owner, &url("http://127.0.0.1:3002/x")));
        assert!(reg.authorize_navigation(&owner, &url("http://127.0.0.1:3001/x")));
    }

    #[test]
    fn permit_is_consumed_by_commit_and_old_origin_becomes_unauthorized() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        let gen = reg
            .prepare_navigation(
                &owner,
                3002,
                PathBuf::from("/workspace"),
                "http://127.0.0.1:3002".to_string(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(reg.authorize_navigation(&owner, &url("http://127.0.0.1:3002/x")));

        reg.commit_workspace_transition(&owner, gen, "http://127.0.0.1:3002".to_string())
            .unwrap();

        // New origin is now current; a redirect back to the old origin is rejected.
        assert!(reg.authorize_navigation(&owner, &url("http://127.0.0.1:3002/x")));
        assert!(!reg.authorize_navigation(&owner, &url("http://127.0.0.1:3001/x")));
        // No pending permit remains for an unregistered third origin.
        assert!(!reg.authorize_navigation(&owner, &url("http://127.0.0.1:3003/x")));
    }

    #[test]
    fn non_loopback_or_non_http_destinations_are_rejected() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        assert!(!reg.authorize_navigation(&owner, &url("https://example.com/")));
        assert!(!reg.authorize_navigation(&owner, &url("http://192.168.1.5:3001/")));
        assert!(!reg.authorize_navigation(&owner, &url("file:///etc/passwd")));
        assert!(!reg.authorize_navigation(&owner, &url("picot://internal/app")));
    }

    #[test]
    fn unregistered_loopback_ports_are_rejected() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        assert!(!reg.authorize_navigation(&owner, &url("http://localhost:9999/")));
        assert!(!reg.authorize_navigation(&owner, &url("http://127.0.0.1:9999/")));
    }

    #[test]
    fn userinfo_is_rejected() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        assert!(!reg.authorize_navigation(&owner, &url("http://user:pass@127.0.0.1:3001/")));
    }

    #[test]
    fn owner_isolation_prevents_cross_owner_authorization() {
        let reg = WindowOwnerRegistry::default();
        let (owner_a, cap_a) = new_owner(&reg, "w1", 3001);
        let (owner_b, cap_b) = new_owner(&reg, "w2", 3002);
        // owner_b cannot authorize owner_a's current origin.
        assert!(!reg.authorize_navigation(&owner_b, &url("http://127.0.0.1:3001/x")));
        // Each capability authenticates as exactly its own owner, never the other.
        assert_eq!(reg.authenticate(&cap_a), Some(owner_a));
        assert_eq!(reg.authenticate(&cap_b), Some(owner_b));
        assert_ne!(reg.authenticate(&cap_a), reg.authenticate(&cap_b));
    }

    #[test]
    fn capability_never_appears_in_debug_or_descriptors() {
        let reg = WindowOwnerRegistry::default();
        let (owner, capability) = new_owner(&reg, "w1", 3001);
        let debug = format!("{owner:?}");
        assert!(!debug.contains(&capability));
        // current_workspace descriptor carries no capability either.
        let (cwd, port) = reg.current_workspace(&owner).unwrap();
        assert_eq!(port, 3001);
        assert!(!format!("{cwd:?}").contains(&capability));
    }

    #[test]
    fn initialization_script_exposes_capability_on_any_loopback_origin() {
        let script = capability_initialization_script("secret-cap");
        assert!(script.contains("\"secret-cap\""));
        assert!(script.contains("window.location.protocol === 'http:'"));
        // Loopback hosts are all accepted so a cross-port workspace navigation
        // (new loopback port, same owner capability) still receives it.
        assert!(script.contains("\"127.0.0.1\""));
        assert!(script.contains("\"localhost\""));
        // The script is loopback-agnostic by design: the exact-origin boundary
        // is enforced by `authorize_navigation`, not by this init script.
        assert!(!script.contains("window.location.port"));
        assert!(script.contains("__PICOT_NATIVE_CAPABILITY__"));
        // Same-origin host-served iframes (e.g. the /cost settings embed) also
        // receive the capability: they speak the same v2 protocol to the same
        // loopback host. The exact-origin boundary is authorize_navigation.
        assert!(!script.contains("window === window.top"));
    }

    #[test]
    fn workspace_transition_updates_cwd_and_port_on_commit() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        let gen = reg
            .begin_workspace_transition(&owner, PathBuf::from("/ws-b"), 3002)
            .unwrap();
        reg.prepare_navigation(
            &owner,
            3002,
            PathBuf::from("/ws-b"),
            "http://127.0.0.1:3002".to_string(),
            Duration::from_secs(5),
        )
        .unwrap();
        reg.commit_workspace_transition(&owner, gen, "http://127.0.0.1:3002".to_string())
            .unwrap();
        let (cwd, port) = reg.current_workspace(&owner).unwrap();
        assert_eq!(cwd, PathBuf::from("/ws-b"));
        assert_eq!(port, 3002);
    }

    #[test]
    fn same_workspace_navigation_preserves_runtime_generation() {
        let reg = WindowOwnerRegistry::default();
        // Real same-workspace navigation always runs on a registered owner.
        let (owner, _) = reg
            .create_owner_with_workspace(
                "same-workspace".to_string(),
                PathBuf::from("/workspace"),
                3001,
                "http://127.0.0.1:3001".to_string(),
                Some("wid-1".to_string()),
                TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let generation = reg
            .prepare_navigation(
                &owner,
                3002,
                PathBuf::from("/workspace"),
                "http://127.0.0.1:3002".to_string(),
                Duration::from_secs(5),
            )
            .unwrap();

        assert_eq!(generation, 0);
        reg.commit_workspace_transition_with_workspace(
            &owner,
            generation,
            "http://127.0.0.1:3002".to_string(),
            Some("wid-1".to_string()),
            TemporaryKind::DefaultStartup,
        )
        .unwrap();
        assert_eq!(reg.current_workspace_generation(&owner), Some(0));
    }

    #[test]
    fn pending_target_session_id_is_derived_from_prepared_origin() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        reg.prepare_navigation(
            &owner,
            0,
            PathBuf::from("/workspace"),
            "http://127.0.0.1:3002/workspaces/wid/sessions/new-session".to_string(),
            Duration::from_secs(5),
        )
        .unwrap();

        assert_eq!(
            reg.pending_target_session_id(&owner).as_deref(),
            Some("new-session")
        );
    }

    #[test]
    fn created_instance_record_survives_until_generation_mismatch() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        let generation = reg
            .prepare_navigation(
                &owner,
                0,
                PathBuf::from("/workspace"),
                "http://127.0.0.1:3002/workspaces/wid/sessions/new-session".to_string(),
                Duration::from_secs(5),
            )
            .unwrap();

        // A reused prepare never records a created instance.
        assert_eq!(reg.pending_created_instance(&owner), None);

        reg.record_pending_created_instance(&owner, generation, "instance-new");
        assert_eq!(
            reg.pending_created_instance(&owner).as_deref(),
            Some("instance-new")
        );

        // A stale generation (a newer transition replaced the pending record)
        // must not stamp the newer record.
        reg.prepare_navigation(
            &owner,
            0,
            PathBuf::from("/workspace"),
            "http://127.0.0.1:3002/workspaces/wid/sessions/next-session".to_string(),
            Duration::from_secs(5),
        )
        .unwrap();
        reg.record_pending_created_instance(&owner, generation + 100, "instance-stale");
        assert_eq!(reg.pending_created_instance(&owner), None);
    }

    #[test]
    fn pending_target_origin_tracks_prepare_and_commit() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        assert_eq!(reg.pending_target_origin(&owner), None);
        reg.prepare_navigation(
            &owner,
            3002,
            PathBuf::from("/workspace"),
            "http://127.0.0.1:3002".to_string(),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            reg.pending_target_origin(&owner).as_deref(),
            Some("http://127.0.0.1:3002")
        );
        let gen = reg
            .begin_workspace_transition(&owner, PathBuf::from("/ws-b"), 3002)
            .unwrap();
        // prepare during an active transition reuses that generation.
        let prepare_gen = reg
            .prepare_navigation(
                &owner,
                3002,
                PathBuf::from("/ws-b"),
                "http://127.0.0.1:3002".to_string(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(prepare_gen, gen);
        reg.commit_workspace_transition(&owner, gen, "http://127.0.0.1:3002".to_string())
            .unwrap();
        assert_eq!(reg.pending_target_origin(&owner), None);
    }

    #[test]
    fn cancel_workspace_transition_retains_old_binding() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        let gen = reg
            .begin_workspace_transition(&owner, PathBuf::from("/ws-b"), 3002)
            .unwrap();
        reg.prepare_navigation(
            &owner,
            3002,
            PathBuf::from("/ws-b"),
            "http://127.0.0.1:3002".to_string(),
            Duration::from_secs(5),
        )
        .unwrap();

        reg.cancel_workspace_transition(&owner, gen).unwrap();
        // Old binding retained: cwd/port/origin unchanged, no pending/transition.
        let (cwd, port) = reg.current_workspace(&owner).unwrap();
        assert_eq!(cwd, PathBuf::from("/workspace"));
        assert_eq!(port, 3001);
        assert_eq!(reg.pending_target_origin(&owner), None);
        // A new transition can begin after cancel.
        assert!(reg
            .begin_workspace_transition(&owner, PathBuf::from("/ws-c"), 3003)
            .is_ok());
    }

    #[test]
    fn validate_workspace_transition_rejects_stale_generation() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w-validate", 3001);
        let generation = reg
            .begin_workspace_transition(&owner, PathBuf::from("/ws-b"), 3002)
            .unwrap();
        assert!(reg
            .validate_workspace_transition_generation(&owner, generation + 1)
            .is_err());
        assert!(reg
            .validate_workspace_transition_generation(&owner, generation)
            .is_ok());
    }

    #[test]
    fn cancel_workspace_transition_rejects_mismatched_generation() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        let gen = reg
            .begin_workspace_transition(&owner, PathBuf::from("/ws-b"), 3002)
            .unwrap();
        assert!(reg.cancel_workspace_transition(&owner, gen + 999).is_err());
        // The transition is still in progress.
        assert!(reg
            .begin_workspace_transition(&owner, PathBuf::from("/ws-c"), 3003)
            .is_err());
    }

    #[test]
    fn cancel_is_idempotent_when_nothing_in_progress() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = new_owner(&reg, "w1", 3001);
        assert!(reg.cancel_workspace_transition(&owner, 42).is_ok());
    }

    // ── Landing owner matrix ────────────────────────────────────────────

    fn landing_owner(reg: &WindowOwnerRegistry, home: &std::path::Path) -> (OwnerId, String) {
        reg.create_owner_with_workspace(
            "native-landing".to_string(),
            home.to_path_buf(),
            0,
            "http://127.0.0.1:3001".to_string(),
            None,
            TemporaryKind::Landing,
        )
        .expect("landing owner created")
    }

    #[test]
    fn landing_owner_is_an_unbound_temporary_record() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = landing_owner(&reg, std::path::Path::new("/home"));
        assert_eq!(
            reg.owner_current_workspace(&owner),
            OwnerWorkspaceSnapshot::Temporary {
                root: PathBuf::from("/home"),
                generation: 0,
                temporary_kind: TemporaryKind::Landing,
            }
        );
        // Menu/Cmd+N gate: unbound before the first transition.
        assert!(!reg.owner_has_workspace(&owner));
        assert_eq!(reg.owner_current_workspace(&owner).workspace_id(), None);
    }

    #[test]
    fn landing_first_transition_is_cross_even_when_home_is_the_target() {
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = landing_owner(&reg, std::path::Path::new("/home"));

        // The placeholder home is itself a registered workspace target: the
        // transition must still be cross — begin_workspace_transition always
        // increments, and prepare_navigation must reuse the transition
        // generation instead of retaining generation 0 via cwd equality.
        let gen = reg
            .begin_workspace_transition(&owner, PathBuf::from("/home"), 3002)
            .unwrap();
        assert_eq!(gen, 1, "landing entry always increments the generation");
        reg.prepare_navigation(
            &owner,
            3002,
            PathBuf::from("/home"),
            "http://127.0.0.1:3002/workspaces/home-wid/sessions/s1".to_string(),
            Duration::from_secs(5),
        )
        .unwrap();
        // Commit at the transition generation must not mismatch.
        reg.commit_workspace_transition_with_workspace(
            &owner,
            gen,
            "http://127.0.0.1:3002".to_string(),
            Some("home-wid".to_string()),
            TemporaryKind::DefaultStartup,
        )
        .expect("commit at the transition generation");
        assert_eq!(
            reg.owner_current_workspace(&owner),
            OwnerWorkspaceSnapshot::Registered {
                wid: "home-wid".to_string(),
                root: PathBuf::from("/home"),
                generation: gen,
            }
        );
        // The label never changed; the binding did.
        assert_eq!(
            reg.label_for_owner(&owner).as_deref(),
            Some("native-landing")
        );
        assert!(reg.owner_has_workspace(&owner));
    }

    #[test]
    fn registered_owner_keeps_same_workspace_generation_on_cwd_match() {
        // The retention shortcut still applies to registered owners only.
        let reg = WindowOwnerRegistry::default();
        let (owner, _) = reg
            .create_owner_with_workspace(
                "registered".to_string(),
                PathBuf::from("/workspace"),
                3001,
                "http://127.0.0.1:3001".to_string(),
                Some("wid-1".to_string()),
                TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let gen = reg
            .prepare_navigation(
                &owner,
                3002,
                PathBuf::from("/workspace"),
                "http://127.0.0.1:3002/workspaces/wid-1/sessions/s1".to_string(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(gen, 0, "same registered workspace retains its generation");
    }
}
