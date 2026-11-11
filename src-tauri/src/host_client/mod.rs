// ABOUTME: App-owned relay hosts, window bindings, pairing and bridge lifecycle.
// ABOUTME: Commands authenticate injected windows; device credentials stay in Rust.

pub mod commands;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use picot_host::relay::client_role::ClientRoleConfig;
use picot_host::relay::host_bridge::{
    start_host_bridge, BindingRecord, Credential, HostBridgeHandle, HostBridgeStatus,
    PairingSaveError, PairingSink, GRANT_UNAVAILABLE, GRANT_VERSION,
};
use picot_host::relay::host_store::{DeviceToken, HostProfile, HostStore};
use picot_host::relay::host_tofu::{
    self, Answer, ConfirmationQueue, ConnectionChange, TofuDecision,
};
use picot_host::relay::pairing::{parse_connection_offer, ConnectionOfferV2};
use serde_json::{json, Value};

pub struct HostClientState(pub Result<Arc<HostClient>, String>);

/// Fixed, nonsecret refusal for a command whose captured window binding is no
/// longer the current one (closed, switched or replaced while it waited).
const STALE_WINDOW: &str = "Host window changed; reload the window";

/// One window's attachment to one host. The record is its authorization
/// identity: a switch, close or reopen installs a different record, so a late
/// command or cleanup holding the old one cannot act on the replacement.
#[derive(Clone)]
struct Binding {
    server_id: String,
    origin: String,
    record: Arc<BindingRecord>,
}
impl Binding {
    fn matches(&self, server_id: &str, origin: &str) -> bool {
        self.server_id == server_id && self.origin == origin
    }
}

/// Teardown computed synchronously under the core lock. The authorization is
/// already revoked when this is returned; the caller joins the bridges whose
/// last reference left after dropping the lock.
pub(crate) struct Release {
    window: String,
    record: Option<Arc<BindingRecord>>,
    stops: Vec<HostBridgeHandle>,
}
/// Which host each live window is on: a window serves one host at a time, so a
/// switch compares the new target against this binding before it releases.
///
/// `None` is an explicit closed tombstone, not an absent entry: a command that
/// captured "this window has no binding yet" must not attach after the window
/// was destroyed. The tombstone is dropped by the async cleanup.
#[derive(Default)]
struct WindowHosts(Mutex<HashMap<String, Option<Binding>>>);

struct Bridge {
    handle: HostBridgeHandle,
    windows: HashSet<String>,
}
impl Bridge {
    fn acquire(&mut self, window: &str) -> Result<(), String> {
        if !self.handle.acquire(window) {
            return Err("Host bridge stopped; reconnect the host".into());
        }
        self.windows.insert(window.to_owned());
        Ok(())
    }
}
struct Candidate {
    id: String,
    profile: HostProfile,
    bridge: Bridge,
}
#[derive(Default)]
struct HostRuntime {
    active: Option<Bridge>,
    candidate: Option<Candidate>,
}
struct Pending {
    window: String,
    offer: ConnectionOfferV2,
}
#[derive(Default)]
struct Core {
    hosts: HashMap<String, HostRuntime>,
    questions: ConfirmationQueue,
    pending: Option<Pending>,
    stopped: bool,
}

pub struct HostClient {
    store: HostStore,
    window_hosts: Arc<WindowHosts>,
    // Serializes commands with ack commits: removal cancels before join/delete,
    // and a late callback cannot resurrect a deleted host (even after re-add).
    core: tokio::sync::Mutex<Core>,
}
impl HostClient {
    pub fn load(path: PathBuf) -> Result<Arc<Self>, String> {
        let store = HostStore::load(path).map_err(|_| {
            "Host configuration unavailable or corrupt; restore it or remove it and pair again"
                .to_string()
        })?;
        Ok(Arc::new(Self {
            store,
            window_hosts: Arc::default(),
            core: tokio::sync::Mutex::new(Core::default()),
        }))
    }

    pub async fn list(&self) -> Result<Value, String> {
        let mut core = self.core.lock().await;
        self.refresh_invalid(&mut core).await?;
        Ok(Value::Array(self.store.hosts().into_iter().map(|host| {
            // Explicit projection: never serialize StoredHost (contains bearer).
            json!({"serverId":host.profile.server_id, "label":host.profile.label,
                "relay":host.profile.relay, "daemonPublicKeyB64":host.profile.daemon_public_key_b64,
                "addedAt":host.profile.added_at, "status":status_name(core.hosts.get(&host.profile.server_id)),
                "needsPairing":!host.has_token()})
        }).collect()))
    }

    pub async fn add(
        self: &Arc<Self>,
        window: &str,
        origin: &str,
        url: &str,
    ) -> Result<Value, String> {
        // Parser errors may quote caller bytes; no raw offer/error reaches logs or IPC.
        let offer =
            parse_connection_offer(url).map_err(|_| "Invalid connection offer".to_string())?;
        // Captured before the first await: a delayed command must prove it is
        // still acting for the window's current binding.
        let captured = self.capture(window);
        let mut core = self.core.lock().await;
        running(&core)?;
        if !self.still_current(window, captured.as_ref()) {
            return Err(STALE_WINDOW.to_string());
        }
        self.refresh_invalid(&mut core).await?;
        // A new offer supersedes the candidate its predecessor left behind: a
        // candidate never reaches `host_list`, so the user cannot clear it there.
        discard_candidate(&mut core, &offer.server_id).await;
        let saved = self.store.host(&offer.server_id);
        match host_tofu::decide(&offer, saved.as_ref()) {
            TofuDecision::AutoApprove => {
                let release = self
                    .connect_saved(
                        &mut core,
                        window,
                        origin,
                        &offer.server_id,
                        captured.as_ref(),
                    )
                    .await?;
                let payload = status_payload(&core, &offer.server_id);
                drop(core);
                self.finish_release(release).await;
                Ok(payload)
            }
            TofuDecision::Confirm(question) => {
                core.questions.ask(question);
                core.pending = Some(Pending {
                    window: window.into(),
                    offer,
                });
                let asked = core.questions.pending().unwrap();
                let changes: Vec<Value> = asked
                    .question
                    .changes
                    .iter()
                    .map(|change| match change {
                        ConnectionChange::DaemonKey { saved, offered } => {
                            json!({"field":"daemonKey", "saved":saved, "offered":offered})
                        }
                        ConnectionChange::RelayEndpoint { saved, offered } => {
                            json!({"field":"relayEndpoint", "saved":saved, "offered":offered})
                        }
                        ConnectionChange::UseTls { saved, offered } => {
                            json!({"field":"useTls", "saved":saved, "offered":offered})
                        }
                    })
                    .collect();
                Ok(
                    json!({"status":"tofuPending", "questionId":asked.id, "serverId":asked.question.server_id,
                    "keyFingerprint":asked.question.key_fingerprint, "relayEndpoint":asked.question.relay_endpoint,
                    "kind":match asked.question.kind { host_tofu::ConfirmKind::NewHost => "newHost", host_tofu::ConfirmKind::ChangedConnection => "changedConnection" },
                    "changes":changes}),
                )
            }
        }
    }

    pub async fn confirm(
        self: &Arc<Self>,
        window: &str,
        origin: &str,
        question_id: u64,
        approved: bool,
    ) -> Result<Value, String> {
        let captured = self.capture(window);
        let mut core = self.core.lock().await;
        running(&core)?;
        if !self.still_current(window, captured.as_ref()) {
            return Err(STALE_WINDOW.to_string());
        }
        if core.pending.as_ref().map(|p| p.window.as_str()) != Some(window) {
            return Err("TOFU question belongs to another window".into());
        }
        let answer = core
            .questions
            .answer(question_id, approved)
            .ok_or("TOFU question expired")?;
        if answer == Answer::Denied {
            // A cancelled pairing leaves no candidate behind to deadlock the next one.
            let server_id = core
                .pending
                .take()
                .map(|pending| pending.offer.server_id)
                .unwrap_or_default();
            discard_candidate(&mut core, &server_id).await;
            return Ok(json!({"status":"cancelled", "serverId":server_id}));
        }
        // The prerequisites are checked before the pending exchange is consumed:
        // an unusable offer must not swallow the question that produced it.
        let pairing_token = core
            .pending
            .as_ref()
            .map(|pending| &pending.offer)
            .ok_or("TOFU question expired")?
            .pairing_token
            .as_ref()
            .filter(|token| !token.is_empty())
            .ok_or("A fresh pairing offer is required")?
            .clone();
        let offer = core.pending.take().unwrap().offer;
        // F-02: a refused pairing must not block the next offer, and a candidate
        // is invisible to `host_list` — so the new offer cancels it outright.
        discard_candidate(&mut core, &offer.server_id).await;
        let profile = HostProfile::from_offer(
            &offer,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        let id = uuid::Uuid::new_v4().to_string();
        let sink = Arc::new(PairingTransaction {
            client: Arc::downgrade(self),
            server_id: offer.server_id.clone(),
            id: id.clone(),
        });
        let mut bridge = self
            .start(
                &profile,
                Credential::Pairing {
                    pairing_token: DeviceToken::new(pairing_token),
                    device_id: self.store.app_device_id(),
                },
                Some(sink),
            )
            .await?;
        bridge.acquire(window)?;
        // Start/acquire before publishing URL; old profile and bridge stay intact.
        core.hosts
            .entry(offer.server_id.clone())
            .or_default()
            .candidate = Some(Candidate {
            id,
            profile,
            bridge,
        });
        let release = self.attach_window(
            &mut core,
            window,
            origin,
            &offer.server_id,
            captured.as_ref(),
        )?;
        let payload = status_payload(&core, &offer.server_id);
        drop(core);
        self.finish_release(release).await;
        Ok(payload)
    }

    /// Report a host's status and hand the calling window its bridge URL. This is
    /// the window's one entry point: it starts the host's bridge and takes the
    /// window's reference on it, exactly as the connection it is about to open.
    /// `captured` is the record the command saw at entry; a resumed command that
    /// finds a different current record must not reauthorize the old selection.
    pub async fn status(
        self: &Arc<Self>,
        window: &str,
        origin: &str,
        server_id: &str,
        captured: Option<Arc<BindingRecord>>,
    ) -> Result<Value, String> {
        let mut core = self.core.lock().await;
        running(&core)?;
        if !self.still_current(window, captured.as_ref()) {
            return Err(STALE_WINDOW.to_string());
        }
        self.refresh_invalid(&mut core).await?;
        if !core.hosts.contains_key(server_id) && self.store.host(server_id).is_none() {
            return Err("Unknown host".into());
        }
        let release = self
            .connect_saved(&mut core, window, origin, server_id, captured.as_ref())
            .await?;
        let payload = status_payload(&core, server_id);
        drop(core);
        self.finish_release(release).await;
        Ok(payload)
    }

    /// The window's current authorization record, captured synchronously at
    /// command entry before any await.
    pub fn capture(&self, window: &str) -> Option<Arc<BindingRecord>> {
        self.window_hosts
            .0
            .lock()
            .unwrap()
            .get(window)
            .and_then(|slot| slot.as_ref())
            .map(|binding| binding.record.clone())
    }

    /// Drops our own index entry (a tombstone, or the record we were handed)
    /// without waiting for the core lock. A replacement owns a different record
    /// and is left alone.
    fn retire_closed_entry(&self, window: &str, captured: Option<&Arc<BindingRecord>>) {
        let mut hosts = self.window_hosts.0.lock().unwrap();
        let ours = match (captured, hosts.get(window)) {
            (Some(record), Some(Some(binding))) => Arc::ptr_eq(&binding.record, record),
            (_, Some(None)) => true,
            _ => false,
        };
        if ours {
            hosts.remove(window);
        }
    }

    /// True when the window was explicitly closed and its cleanup has not run.
    fn is_closed(&self, window: &str) -> bool {
        matches!(self.window_hosts.0.lock().unwrap().get(window), Some(None))
    }

    /// True when the window's binding is unchanged since `captured` was taken.
    /// A captured record must still be the current one AND still issuable — a
    /// revoked-but-referenced record never reauthorizes. `None` is valid only
    /// while the window still has no binding: a binding installed while the
    /// command waited means the window changed hands.
    fn still_current(&self, window: &str, captured: Option<&Arc<BindingRecord>>) -> bool {
        match captured {
            None => !self.is_closed(window) && self.capture(window).is_none(),
            Some(captured) => self
                .capture(window)
                .is_some_and(|current| Arc::ptr_eq(&current, captured) && current.is_issuable()),
        }
    }

    /// Issues a fresh in-memory secret for the calling window's current host
    /// binding. The caller-checked window captured `entry`, and the record must
    /// still be that same live, issuable, acquired one — issuance never
    /// acquires, switches or reconstructs a binding.
    pub async fn grant(
        self: &Arc<Self>,
        window: &str,
        origin: &str,
        server_id: &str,
        captured: Option<Arc<BindingRecord>>,
    ) -> Result<Value, String> {
        let core = self.core.lock().await;
        running(&core)?;
        let unavailable = || GRANT_UNAVAILABLE.to_string();
        let captured = captured.ok_or_else(unavailable)?;
        let current = self
            .window_hosts
            .0
            .lock()
            .unwrap()
            .get(window)
            .cloned()
            .flatten()
            .ok_or_else(unavailable)?;
        if !Arc::ptr_eq(&current.record, &captured)
            || !current.matches(server_id, origin)
            || !captured.is_issuable()
        {
            return Err(unavailable());
        }
        let bridge = live_bridge(&core, server_id, window).ok_or_else(unavailable)?;
        let grant = bridge
            .handle
            .mint_grant(&captured)
            .map_err(|_| unavailable())?;
        Ok(json!({
            "bridgeGrantVersion": GRANT_VERSION,
            "bridgeGrant": grant,
            "wsUrl": bridge_url(&bridge.handle),
        }))
    }

    pub async fn remove(&self, server_id: &str) -> Result<(), String> {
        let runtime = {
            let mut core = self.core.lock().await;
            if core
                .pending
                .as_ref()
                .is_some_and(|pending| pending.offer.server_id == server_id)
            {
                cancel_question(&mut core);
            }
            // Authorization first, teardown second: dropping the bindings before
            // any await means a stale callback cannot mint or admit.
            let revoked: Vec<Arc<BindingRecord>> = self
                .window_hosts
                .0
                .lock()
                .unwrap()
                .values()
                .flatten()
                .filter(|binding| binding.server_id == server_id)
                .map(|binding| binding.record.clone())
                .collect();
            self.window_hosts.0.lock().unwrap().retain(|_, slot| {
                slot.as_ref()
                    .is_none_or(|binding| binding.server_id != server_id)
            });
            for record in &revoked {
                record.revoke();
            }
            core.hosts.remove(server_id)
        };
        if let Some(runtime) = runtime {
            stop_runtime(runtime).await;
        }
        self.store
            .delete(server_id)
            .map_err(|_| "Could not delete host; local profile retained")?;
        Ok(())
    }

    /// Synchronous invalidation for real lifecycle edges (window destroyed,
    /// accepted navigation away): the index entry leaves and the record is
    /// revoked before this returns, so issuance, admission and owned proxies
    /// die immediately — the async cleanup only joins what is left.
    pub fn invalidate_window(&self, window: &str) -> Option<Arc<BindingRecord>> {
        let mut hosts = self.window_hosts.0.lock().unwrap();
        let old = hosts.remove(window).flatten();
        // Leave the tombstone: a command that captured "no binding yet" must
        // not attach to a window that has since closed.
        hosts.insert(window.to_owned(), None);
        old.map(|binding| {
            binding.record.revoke();
            binding.record
        })
    }

    /// Invalidation on an accepted navigation whose page identity (origin, or
    /// `?host=` on the same origin) no longer matches the window's binding.
    /// The same host at the same origin is the designed reconnect: its grant
    /// stays valid so the reloaded page reconnects without a fresh pair.
    pub fn invalidate_window_on_navigation(
        &self,
        window: &str,
        page_origin: &str,
        host_param: Option<&str>,
    ) -> Option<Arc<BindingRecord>> {
        let binding = self.capture(window)?;
        // Same origin and the same host selection is the designed reload: its
        // grant stays valid, so the page reconnects without re-pairing.
        if binding.origin() != page_origin || host_param != Some(binding.server_id()) {
            return self.invalidate_window(window);
        }
        None
    }

    /// Test seam: the plain close without a pre-invalidated identity.
    #[cfg(test)]
    pub(crate) async fn close_window(&self, window: &str) {
        self.close_window_with(window, None).await;
    }

    /// `captured` is a binding already invalidated synchronously by
    /// [`Self::invalidate_window`]; the cleanup keeps its identity so a
    /// same-label replacement installed in the meantime survives.
    pub async fn close_window_with(&self, window: &str, captured: Option<Arc<BindingRecord>>) {
        self.retire_closed_entry(window, captured.as_ref());
        let release = {
            let mut core = self.core.lock().await;
            if core
                .pending
                .as_ref()
                .is_some_and(|pending| pending.window == window)
            {
                cancel_question(&mut core);
            }
            self.plan_release_with(&mut core, window, None, captured)
        };
        self.finish_release(release).await;
    }

    pub async fn shutdown(&self) {
        let runtimes = {
            let mut core = self.core.lock().await;
            core.stopped = true;
            cancel_question(&mut core);
            // No record survives shutdown in an issuable state.
            let records: Vec<Arc<BindingRecord>> = self
                .window_hosts
                .0
                .lock()
                .unwrap()
                .values()
                .flatten()
                .map(|binding| binding.record.clone())
                .collect();
            self.window_hosts.0.lock().unwrap().clear();
            for record in &records {
                record.revoke();
            }
            core.hosts
                .drain()
                .map(|(_, runtime)| runtime)
                .collect::<Vec<_>>()
        };
        for runtime in runtimes {
            stop_runtime(runtime).await;
        }
    }

    /// Test seam: the `host_grant` path with an explicit pause between the
    /// caller's capture and the core lock, so a delayed command can be resumed
    /// after the window closed, switched or was replaced.
    #[cfg(test)]
    pub(crate) async fn grant_paused(
        self: &Arc<Self>,
        window: &str,
        origin: &str,
        server_id: &str,
        captured: Option<Arc<BindingRecord>>,
        entered: &tokio::sync::Notify,
        resume: &tokio::sync::Notify,
    ) -> Result<Value, String> {
        entered.notify_one();
        resume.notified().await;
        self.grant(window, origin, server_id, captured).await
    }

    /// The same pause for the binding-mutating status path.
    #[cfg(test)]
    pub(crate) async fn status_paused(
        self: &Arc<Self>,
        window: &str,
        origin: &str,
        server_id: &str,
        captured: Option<Arc<BindingRecord>>,
        entered: &tokio::sync::Notify,
        resume: &tokio::sync::Notify,
    ) -> Result<Value, String> {
        entered.notify_one();
        resume.notified().await;
        self.status(window, origin, server_id, captured).await
    }

    /// Test seam: the synchronous half of `close_window`, so a test can delay
    /// the teardown until after a replacement binding exists.
    #[cfg(test)]
    pub(crate) async fn plan_close_for_tests(&self, window: &str) -> Release {
        let mut core = self.core.lock().await;
        if core
            .pending
            .as_ref()
            .is_some_and(|pending| pending.window == window)
        {
            cancel_question(&mut core);
        }
        self.plan_release(&mut core, window, None)
    }

    #[cfg(test)]
    pub(crate) async fn finish_release_for_tests(&self, release: Release) {
        self.finish_release(release).await;
    }

    /// Test seam: invalidate the window's authorization WITHOUT releasing its
    /// references, the state a revoked-but-still-referenced record is in.
    #[cfg(test)]
    pub(crate) fn revoke_binding_for_tests(&self, window: &str) -> bool {
        let Some(record) = self.capture(window) else {
            return false;
        };
        record.revoke();
        true
    }

    async fn start(
        &self,
        profile: &HostProfile,
        credential: Credential,
        sink: Option<Arc<dyn PairingSink>>,
    ) -> Result<Bridge, String> {
        let handle = start_host_bridge(
            ClientRoleConfig {
                endpoint: profile.relay.endpoint.clone(),
                use_tls: profile.relay.use_tls,
                server_id: profile.server_id.clone(),
                peer_public_key_b64: profile.daemon_public_key_b64.clone(),
            },
            credential,
            sink,
        )
        .await
        .map_err(|_| "Could not start host bridge")?;
        Ok(Bridge {
            handle,
            windows: HashSet::new(),
        })
    }

    async fn connect_saved(
        &self,
        core: &mut Core,
        window: &str,
        origin: &str,
        server_id: &str,
        captured: Option<&Arc<BindingRecord>>,
    ) -> Result<Release, String> {
        let runtime = core.hosts.entry(server_id.into()).or_default();
        // A stopped bridge keeps its origin, but nothing listens there: reusing
        // it produced a dead wsUrl, a failed page load and the local-recovery
        // bounce loop. Drop it and start a fresh one.
        let stopped = |bridge: &Bridge| *bridge.handle.status.borrow() == HostBridgeStatus::Stopped;
        if runtime.active.as_ref().is_some_and(stopped) {
            runtime.active = None;
        }
        if runtime
            .candidate
            .as_ref()
            .is_some_and(|c| stopped(&c.bridge))
        {
            runtime.candidate = None;
        }
        if runtime.candidate.is_none() && runtime.active.is_none() {
            let saved = self.store.host(server_id).ok_or("Unknown host")?;
            let token = saved
                .device_token
                .ok_or("Host credential invalid; a fresh pairing offer is required")?;
            runtime.active = Some(
                self.start(&saved.profile, Credential::Device(token), None)
                    .await?,
            );
        }
        let bridge = runtime
            .candidate
            .as_mut()
            .map(|candidate| &mut candidate.bridge)
            .or(runtime.active.as_mut())
            .unwrap();
        bridge.acquire(window)?;
        self.attach_window(core, window, origin, server_id, captured)
    }

    /// A window serves one host at a time. The same live target keeps its
    /// record (reconnect); anything else installs a new record, and the old one
    /// is revoked before its references leave.
    fn attach_window(
        &self,
        core: &mut Core,
        window: &str,
        origin: &str,
        server_id: &str,
        captured: Option<&Arc<BindingRecord>>,
    ) -> Result<Release, String> {
        // One critical section for the check AND the publication: a synchronous
        // destroy/navigation invalidation cannot slip between them, so a
        // revoked record never comes back as a fresh binding.
        let mut hosts = self.window_hosts.0.lock().unwrap();
        let slot = hosts.get(window).cloned();
        let current = slot.clone().flatten();
        // The command's entry authorization still has to hold HERE, after every
        // await it performed. A tombstone means the window closed meanwhile.
        match captured {
            Some(captured) => {
                let valid = current.as_ref().is_some_and(|binding| {
                    Arc::ptr_eq(&binding.record, captured) && binding.record.is_issuable()
                });
                if !valid {
                    return Err(STALE_WINDOW.to_string());
                }
            }
            None => {
                if slot.is_some() {
                    return Err(STALE_WINDOW.to_string());
                }
            }
        }
        let reusable = current.as_ref().is_some_and(|binding| {
            binding.matches(server_id, origin) && binding.record.is_issuable()
        });
        if reusable {
            // Reconnect on the same selection: the identity must not change, or
            // the window's fresh grant would be minted for a record the page's
            // in-flight attempt already left behind.
            let record = current.unwrap().record;
            let bound = bridge_handle(core, server_id).is_some_and(|handle| handle.bind(&record));
            if !bound {
                return Err("Host bridge stopped; reconnect the host".into());
            }
            return Ok(Release {
                window: window.into(),
                record: None,
                stops: Vec::new(),
            });
        }
        let record =
            BindingRecord::new(server_id, origin).ok_or_else(|| STALE_WINDOW.to_string())?;
        let bound = bridge_handle(core, server_id).is_some_and(|handle| handle.bind(&record));
        if !bound {
            return Err("Host bridge stopped; reconnect the host".into());
        }
        hosts.insert(
            window.into(),
            Some(Binding {
                server_id: server_id.into(),
                origin: origin.into(),
                record: record.clone(),
            }),
        );
        // Release the index before the old record's teardown: drop_references
        // reads the index to decide whether a replacement took the label.
        drop(hosts);
        let mut release = Release {
            window: window.into(),
            record: None,
            stops: Vec::new(),
        };
        if let Some(old) = current {
            // The authority boundary runs before any await: the old record can
            // never issue or admit again, whatever still references it.
            old.record.revoke();
            self.drop_references(
                core,
                window,
                Some(server_id),
                Some(&old.record),
                &mut release.stops,
            );
            release.record = Some(old.record);
        }
        Ok(release)
    }

    /// Takes the window out of its binding without awaiting anything: the
    /// record is revoked, its bridge references are released, and the bridges
    /// left without a window are returned for the caller to join.
    #[cfg(test)]
    fn plan_release(&self, core: &mut Core, window: &str, keep_host: Option<&str>) -> Release {
        self.plan_release_with(core, window, keep_host, None)
    }

    /// `pre_invalidated` carries a record already revoked and removed from the
    /// index by a synchronous lifecycle edge: its bridge references (and only
    /// theirs) are still released here, under the captured identity. The index
    /// is not touched again — a same-label replacement installed meanwhile
    /// owns a different record and must survive.
    fn plan_release_with(
        &self,
        core: &mut Core,
        window: &str,
        keep_host: Option<&str>,
        pre_invalidated: Option<Arc<BindingRecord>>,
    ) -> Release {
        if let Some(record) = pre_invalidated {
            let mut release = Release {
                window: window.into(),
                record: Some(record.clone()),
                stops: Vec::new(),
            };
            self.drop_references(core, window, keep_host, Some(&record), &mut release.stops);
            return release;
        }
        let old = self.window_hosts.0.lock().unwrap().remove(window).flatten();
        let mut release = Release {
            window: window.into(),
            record: None,
            stops: Vec::new(),
        };
        if let Some(old) = old {
            old.record.revoke();
            self.drop_references(
                core,
                window,
                keep_host,
                Some(&old.record),
                &mut release.stops,
            );
            release.record = Some(old.record);
        } else {
            // A window with no binding may still hold bridge references (for
            // example after a failed attach); release them all the same.
            self.drop_references(core, window, keep_host, None, &mut release.stops);
        }
        release
    }

    fn drop_references(
        &self,
        core: &mut Core,
        window: &str,
        keep_host: Option<&str>,
        record: Option<&Arc<BindingRecord>>,
        stops: &mut Vec<HostBridgeHandle>,
    ) {
        if let Some(record) = record {
            // Retained (detached) slots belong to this record: revoke must stop
            // them too, or a closed window keeps a redial loop alive.
            for runtime in core.hosts.values() {
                if let Some(bridge) = runtime
                    .candidate
                    .as_ref()
                    .map(|candidate| &candidate.bridge)
                    .or(runtime.active.as_ref())
                {
                    bridge.handle.revoke(record);
                }
            }
        }
        // A replacement installed under the same label owns its own reference;
        // the old record's cleanup must not take it away.
        let replaced = {
            let hosts = self.window_hosts.0.lock().unwrap();
            match (record, hosts.get(window)) {
                (Some(record), Some(Some(current))) => !Arc::ptr_eq(&current.record, record),
                _ => false,
            }
        };
        if replaced {
            return;
        }
        for (server_id, runtime) in &mut core.hosts {
            if keep_host == Some(server_id.as_str()) {
                continue;
            }
            let mut empty_candidate = false;
            if let Some(candidate) = &mut runtime.candidate {
                if candidate.bridge.windows.remove(window) {
                    candidate.bridge.handle.release(window);
                    if let Some(record) = record {
                        candidate.bridge.handle.unbind(record);
                    }
                }
                empty_candidate = candidate.bridge.windows.is_empty();
            }
            if empty_candidate {
                if let Some(candidate) = runtime.candidate.take() {
                    stops.push(candidate.bridge.handle);
                }
            }
            let mut empty_active = false;
            if let Some(active) = &mut runtime.active {
                if active.windows.remove(window) {
                    active.handle.release(window);
                    if let Some(record) = record {
                        active.handle.unbind(record);
                    }
                }
                empty_active = active.windows.is_empty();
            }
            if empty_active {
                if let Some(active) = runtime.active.take() {
                    stops.push(active.handle);
                }
            }
        }
    }

    /// Removes the label's entry only when it still points at the released
    /// record, then joins the bridges whose last reference left. A reopened
    /// window with the same label owns a different record and survives.
    async fn finish_release(&self, release: Release) {
        {
            let mut hosts = self.window_hosts.0.lock().unwrap();
            let ours = match (&release.record, hosts.get(&release.window)) {
                // Our record is still the current one.
                (Some(record), Some(Some(binding))) => Arc::ptr_eq(&binding.record, record),
                // Our own tombstone, and no replacement attached meanwhile:
                // this cleanup is the one that closed the window.
                (_, Some(None)) => true,
                _ => false,
            };
            if ours {
                hosts.remove(&release.window);
            }
        }
        for handle in release.stops {
            handle.stop().await;
        }
    }

    /// Consume bridge terminal states. `CredentialInvalid` clears the stored token and
    /// stops that host's bridge: a dead bridge would otherwise keep publishing its
    /// `wsUrl` and close every reconnect. Host-scoped, so no other host is touched.
    /// `PairingFailed` stays visible as a status and is cleared by `confirm`.
    async fn refresh_invalid(&self, core: &mut Core) -> Result<(), String> {
        let mut stopped: Vec<Bridge> = Vec::new();
        for (server_id, runtime) in &mut core.hosts {
            if runtime.active.as_ref().is_some_and(|bridge| {
                *bridge.handle.status.borrow() == HostBridgeStatus::CredentialInvalid
            }) {
                self.store
                    .clear_token(server_id)
                    .map_err(|_| "Could not clear invalid host credential")?;
                let revoked: Vec<Arc<BindingRecord>> = self
                    .window_hosts
                    .0
                    .lock()
                    .unwrap()
                    .values()
                    .flatten()
                    .filter(|binding| binding.server_id == *server_id)
                    .map(|binding| binding.record.clone())
                    .collect();
                self.window_hosts.0.lock().unwrap().retain(|_, slot| {
                    slot.as_ref()
                        .is_none_or(|binding| binding.server_id != *server_id)
                });
                for record in &revoked {
                    record.revoke();
                }
                // F-07: a dead bridge must not keep publishing its wsUrl (every
                // reconnect would be closed) nor hold window references. Taking
                // it here also makes the reported status settle on `offline`.
                if let Some(bridge) = runtime.active.take() {
                    stopped.push(bridge);
                }
            }
        }
        for bridge in stopped {
            bridge.handle.stop().await;
        }
        Ok(())
    }
}

/// Sole application pairing transaction. T6 sends one rewritten hello on the
/// first proxy session and consumes ack through this sink; no second client dial.
struct PairingTransaction {
    client: Weak<HostClient>,
    server_id: String,
    id: String,
}
#[async_trait]
impl PairingSink for PairingTransaction {
    async fn persist(&self, token: DeviceToken) -> Result<(), PairingSaveError> {
        let client = self.client.upgrade().ok_or(PairingSaveError)?;
        let mut core = client.core.lock().await;
        // A late or superseded callback is not a save failure: the host is gone or
        // re-pairing, and reporting an error would mark that newer pairing's row.
        let Some(runtime) = core.hosts.get_mut(&self.server_id) else {
            log::warn!("pairing ack for a removed host; dropping it");
            return Ok(());
        };
        let Some(profile) = runtime
            .candidate
            .as_ref()
            .filter(|candidate| candidate.id == self.id)
            .map(|candidate| candidate.profile.clone())
        else {
            log::warn!("pairing ack for a superseded candidate; dropping it");
            return Ok(());
        };
        // Save before promoting: the token is this pairing's only durable record.
        client
            .store
            .upsert(&profile, token)
            .map_err(|_| PairingSaveError)?;
        let mut next = runtime.candidate.take().unwrap().bridge;
        let old = runtime.active.take();
        if let Some(old) = &old {
            for window in &old.windows {
                next.acquire(window).map_err(|_| PairingSaveError)?;
            }
            // Promotion must not change the window's authorization identity:
            // the page's current grant keeps applying on the new bridge.
            let bindings: Vec<(String, Arc<BindingRecord>)> = client
                .window_hosts
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|(window, slot)| {
                    slot.as_ref().is_some_and(|binding| {
                        binding.server_id == self.server_id && old.windows.contains(*window)
                    })
                })
                .filter_map(|(window, slot)| {
                    slot.as_ref()
                        .map(|binding| (window.clone(), binding.record.clone()))
                })
                .collect();
            for (_, record) in &bindings {
                if !next.handle.bind(record) {
                    return Err(PairingSaveError);
                }
                old.handle.unbind(record);
            }
        }
        runtime.active = Some(next);
        if let Some(old) = old {
            old.handle.stop().await;
        }
        Ok(())
    }
}

/// Drops a host's in-flight pairing bridge. A candidate is not a saved host, so
/// `host_list` cannot show it and the user has no row to remove.
async fn discard_candidate(core: &mut Core, server_id: &str) {
    let stopped = core
        .hosts
        .get_mut(server_id)
        .and_then(|runtime| runtime.candidate.take());
    if let Some(candidate) = stopped {
        candidate.bridge.handle.stop().await;
    }
}

async fn stop_runtime(runtime: HostRuntime) {
    if let Some(candidate) = runtime.candidate {
        candidate.bridge.handle.stop().await;
    }
    if let Some(active) = runtime.active {
        active.handle.stop().await;
    }
}
fn cancel_question(core: &mut Core) {
    if let Some(id) = core.questions.pending().map(|question| question.id) {
        core.questions.answer(id, false);
    }
    core.pending = None;
}
fn running(core: &Core) -> Result<(), String> {
    if core.stopped {
        Err("Host client stopped".into())
    } else {
        Ok(())
    }
}
fn status_name(runtime: Option<&HostRuntime>) -> &'static str {
    let Some(runtime) = runtime else {
        return "offline";
    };
    match runtime
        .candidate
        .as_ref()
        .map(|candidate| &candidate.bridge)
        .or(runtime.active.as_ref())
        .map(|bridge| *bridge.handle.status.borrow())
    {
        Some(HostBridgeStatus::Connecting) => "connecting",
        Some(HostBridgeStatus::Online) => "online",
        Some(HostBridgeStatus::CredentialInvalid) => "credentialInvalid",
        Some(HostBridgeStatus::Stopped) => "stopped",
        // Offline, and a refused pairing with it: the user re-opens the offer.
        _ => "offline",
    }
}
/// The bridge serving a host, if it has one.
fn bridge_handle<'a>(core: &'a Core, server_id: &str) -> Option<&'a HostBridgeHandle> {
    let runtime = core.hosts.get(server_id)?;
    runtime
        .candidate
        .as_ref()
        .map(|candidate| &candidate.bridge.handle)
        .or(runtime.active.as_ref().map(|bridge| &bridge.handle))
}

/// The live bridge for a host, if this window holds a reference on it.
fn live_bridge<'a>(core: &'a Core, server_id: &str, window: &str) -> Option<&'a Bridge> {
    let runtime = core.hosts.get(server_id)?;
    runtime
        .candidate
        .as_ref()
        .map(|candidate| &candidate.bridge)
        .or(runtime.active.as_ref())
        .filter(|bridge| bridge.windows.contains(window))
}

fn bridge_url(handle: &HostBridgeHandle) -> String {
    format!("{}/v2/ws", handle.origin.replacen("http://", "ws://", 1))
}

fn status_payload(core: &Core, server_id: &str) -> Value {
    let runtime = core.hosts.get(server_id);
    let status = status_name(runtime);
    let bridge = runtime.and_then(|runtime| {
        runtime
            .candidate
            .as_ref()
            .map(|candidate| &candidate.bridge)
            .or(runtime.active.as_ref())
    });
    let mut payload = json!({"serverId":server_id, "status":status,
        "wsUrl":bridge.map(|bridge| bridge_url(&bridge.handle)),
        "pairing":runtime.is_some_and(|runtime| runtime.candidate.is_some())});
    // The version is the only grant metadata status may advertise: the secret
    // itself is issued by `host_grant` and never serialized here.
    if bridge.is_some() {
        payload["bridgeGrantVersion"] = json!(GRANT_VERSION);
    }
    payload
}

#[cfg(test)]
mod tests;
