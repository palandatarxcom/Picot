// ABOUTME: App-owned relay hosts, staged pairing, window grants and bridge lifecycle.
// ABOUTME: Commands authenticate injected windows; device credentials stay in Rust.

pub mod commands;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use picot_host::relay::client_role::ClientRoleConfig;
use picot_host::relay::host_bridge::{
    start_host_bridge, Credential, GrantRegistry, HostBridgeHandle, HostBridgeStatus,
    PairingSaveError, PairingSink,
};
use picot_host::relay::host_store::{
    DeviceToken, HostProfile, HostStore, StagedPairing, StagedState,
};
use picot_host::relay::host_tofu::{
    self, Answer, ConfirmationQueue, ConnectionChange, TofuDecision,
};
use picot_host::relay::pairing::{parse_connection_offer, ConnectionOfferV2};
use serde_json::{json, Value};

pub struct HostClientState(pub Result<Arc<HostClient>, String>);

struct Binding {
    token: String,
    server_id: String,
    origin: String,
}
#[derive(Default)]
struct Grants(Mutex<HashMap<String, Binding>>);
impl GrantRegistry for Grants {
    fn validate(&self, token: &str, server_id: &str, origin: &str) -> Option<String> {
        self.0.lock().unwrap().iter().find_map(|(window, binding)| {
            (!token.is_empty()
                && binding.token == token
                && binding.server_id == server_id
                && binding.origin == origin)
                .then(|| window.clone())
        })
    }
}

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
    staged: StagedPairing,
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
    grants: Arc<Grants>,
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
            grants: Arc::default(),
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
        let mut core = self.core.lock().await;
        running(&core)?;
        self.refresh_invalid(&mut core).await?;
        let saved = self.store.host(&offer.server_id);
        match host_tofu::decide(&offer, saved.as_ref()) {
            TofuDecision::AutoApprove => {
                self.connect_saved(&mut core, window, origin, &offer.server_id)
                    .await?;
                Ok(status_payload(&core, &offer.server_id))
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
        let mut core = self.core.lock().await;
        running(&core)?;
        if core.pending.as_ref().map(|p| p.window.as_str()) != Some(window) {
            return Err("TOFU question belongs to another window".into());
        }
        let answer = core
            .questions
            .answer(question_id, approved)
            .ok_or("TOFU question expired")?;
        let pending = core.pending.take().unwrap();
        if answer == Answer::Denied {
            return Ok(json!({"status":"cancelled", "serverId":pending.offer.server_id}));
        }
        let offer = pending.offer;
        // F-02: a refused pairing must not block the next offer. Discard the
        // failed candidate (its token is spent) before the in-progress guard,
        // so a freshly generated offer replaces it instead of being rejected.
        let mut replaced: Option<Bridge> = None;
        if let Some(runtime) = core.hosts.get_mut(&offer.server_id) {
            let failed = runtime.candidate.as_ref().is_some_and(|candidate| {
                *candidate.bridge.handle.status.borrow() == HostBridgeStatus::PairingFailed
            });
            if failed {
                let mut candidate = runtime.candidate.take().unwrap();
                candidate.staged.cancel();
                replaced = Some(candidate.bridge);
            }
        }
        if let Some(bridge) = replaced {
            bridge.handle.stop().await;
        }
        if core
            .hosts
            .get(&offer.server_id)
            .is_some_and(|runtime| runtime.candidate.is_some())
        {
            return Err(
                "Host pairing already in progress; retry saving or remove the host first".into(),
            );
        }
        let pairing_token = offer
            .pairing_token
            .as_ref()
            .filter(|token| !token.is_empty())
            .ok_or("A fresh pairing offer is required")?;
        let saved = self.store.host(&offer.server_id);
        let staged = StagedPairing::begin(
            HostProfile::from_offer(
                &offer,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            ),
            saved.as_ref().map(|host| host.generation),
        );
        let id = uuid::Uuid::new_v4().to_string();
        let sink = Arc::new(PairingTransaction {
            client: Arc::downgrade(self),
            server_id: offer.server_id.clone(),
            id: id.clone(),
        });
        let mut bridge = self
            .start(
                staged.profile(),
                Credential::Pairing {
                    pairing_token: DeviceToken::new(pairing_token.clone()),
                    device_id: self.store.app_device_id(),
                },
                Some(sink),
            )
            .await?;
        bridge.acquire(window)?;
        // Start/acquire before publishing URL; old profile and bridge stay intact.
        self.switch_window(&mut core, window, origin, &offer.server_id)
            .await;
        core.hosts
            .entry(offer.server_id.clone())
            .or_default()
            .candidate = Some(Candidate { id, staged, bridge });
        Ok(status_payload(&core, &offer.server_id))
    }

    pub async fn status(&self, server_id: &str) -> Result<Value, String> {
        let mut core = self.core.lock().await;
        self.refresh_invalid(&mut core).await?;
        if !core.hosts.contains_key(server_id) && self.store.host(server_id).is_none() {
            return Err("Unknown host".into());
        }
        Ok(status_payload(&core, server_id))
    }

    pub async fn grant(
        self: &Arc<Self>,
        window: &str,
        origin: &str,
        server_id: &str,
    ) -> Result<Value, String> {
        let mut core = self.core.lock().await;
        running(&core)?;
        self.refresh_invalid(&mut core).await?;
        self.connect_saved(&mut core, window, origin, server_id)
            .await?;
        // Rotation first removes registry binding, then closes sockets (T6 contract).
        self.invalidate(&core, window);
        let token = uuid::Uuid::new_v4().to_string();
        self.grants.0.lock().unwrap().insert(
            window.into(),
            Binding {
                token: token.clone(),
                server_id: server_id.into(),
                origin: origin.into(),
            },
        );
        let mut reply = status_payload(&core, server_id);
        reply["grant"] = json!(token);
        reply["windowId"] = json!(window);
        Ok(reply)
    }

    pub async fn retry_save(self: &Arc<Self>, server_id: &str) -> Result<Value, String> {
        let mut core = self.core.lock().await;
        running(&core)?;
        let runtime = core.hosts.get_mut(server_id).ok_or("Unknown host")?;
        let candidate = runtime
            .candidate
            .as_mut()
            .ok_or("No saved candidate to retry")?;
        if !matches!(candidate.staged.state(), StagedState::SaveFailed { .. }) {
            return Err("Host is not waiting for a save retry".into());
        }
        let saved = candidate
            .staged
            .commit(&self.store)
            .map_err(|_| "Host save failed; candidate retained for retry")?;
        // Commit first. Rebuild with saved device credential, never pairingToken.
        let mut windows = candidate.bridge.windows.clone();
        if let Some(active) = &runtime.active {
            windows.extend(active.windows.iter().cloned());
        }
        for window in &windows {
            self.invalidate(&core, window);
        }
        let runtime = core.hosts.get_mut(server_id).unwrap();
        runtime.candidate.take().unwrap().bridge.handle.stop().await;
        if let Some(active) = runtime.active.take() {
            active.handle.stop().await;
        }
        let mut bridge = self
            .start(
                &saved.profile,
                Credential::Device(saved.device_token.unwrap()),
                None,
            )
            .await?;
        for window in windows {
            bridge.acquire(&window)?;
        }
        runtime.active = Some(bridge);
        Ok(status_payload(&core, server_id))
    }

    pub async fn remove(&self, server_id: &str) -> Result<(), String> {
        let mut core = self.core.lock().await;
        if core
            .pending
            .as_ref()
            .is_some_and(|pending| pending.offer.server_id == server_id)
        {
            cancel_question(&mut core);
        }
        if let Some(mut runtime) = core.hosts.remove(server_id) {
            // Cancel candidate before any await. Stale callbacks fail the id lookup.
            if let Some(candidate) = &mut runtime.candidate {
                candidate.staged.cancel();
            }
            self.invalidate_host(server_id, &runtime);
            stop_runtime(runtime).await;
        }
        self.store
            .delete(server_id)
            .map_err(|_| "Could not delete host; local profile retained")?;
        Ok(())
    }

    /// Destroyed-path sync half (contract 3/4): revoke the window's grant
    /// immediately so the bridge rejects new connections before the async
    /// cleanup (`close_window`) joins. The registry is the only sync state.
    pub fn revoke_window_now(&self, window: &str) {
        if let Some(binding) = self.grants.0.lock().unwrap().get_mut(window) {
            binding.token.clear();
        }
    }

    pub async fn close_window(&self, window: &str) {
        let mut core = self.core.lock().await;
        if core
            .pending
            .as_ref()
            .is_some_and(|pending| pending.window == window)
        {
            cancel_question(&mut core);
        }
        self.release_window(&mut core, window, None).await;
    }

    pub async fn shutdown(&self) {
        let mut core = self.core.lock().await;
        core.stopped = true;
        cancel_question(&mut core);
        self.grants.0.lock().unwrap().clear();
        for (_, mut runtime) in core.hosts.drain() {
            if let Some(candidate) = &mut runtime.candidate {
                candidate.staged.cancel();
            }
            for bridge in bridges(&runtime) {
                for window in &bridge.windows {
                    bridge.handle.invalidate_window(window);
                }
            }
            stop_runtime(runtime).await;
        }
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
            self.grants.clone(),
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
    ) -> Result<(), String> {
        let runtime = core.hosts.entry(server_id.into()).or_default();
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
        self.switch_window(core, window, origin, server_id).await;
        Ok(())
    }

    async fn switch_window(&self, core: &mut Core, window: &str, origin: &str, server_id: &str) {
        let changed = self
            .grants
            .0
            .lock()
            .unwrap()
            .get(window)
            .is_some_and(|binding| binding.server_id != server_id || binding.origin != origin);
        if changed {
            self.release_window(core, window, Some(server_id)).await;
        }
        // A ref exists before host_grant; retain a tokenless binding for switches.
        self.grants
            .0
            .lock()
            .unwrap()
            .entry(window.into())
            .or_insert_with(|| Binding {
                token: String::new(),
                server_id: server_id.into(),
                origin: origin.into(),
            });
    }

    fn invalidate(&self, core: &Core, window: &str) {
        if let Some(binding) = self.grants.0.lock().unwrap().get_mut(window) {
            binding.token.clear();
        }
        for runtime in core.hosts.values() {
            for bridge in bridges(runtime) {
                bridge.handle.invalidate_window(window);
            }
        }
    }

    fn invalidate_host(&self, server_id: &str, runtime: &HostRuntime) {
        self.grants
            .0
            .lock()
            .unwrap()
            .retain(|_, binding| binding.server_id != server_id);
        for bridge in bridges(runtime) {
            for window in &bridge.windows {
                bridge.handle.invalidate_window(window);
                bridge.handle.release(window);
            }
        }
    }

    async fn release_window(&self, core: &mut Core, window: &str, keep_host: Option<&str>) {
        self.invalidate(core, window);
        self.grants.0.lock().unwrap().remove(window);
        for (server_id, runtime) in &mut core.hosts {
            if keep_host == Some(server_id.as_str()) {
                continue;
            }
            if let Some(candidate) = &mut runtime.candidate {
                if candidate.bridge.windows.remove(window) {
                    candidate.bridge.handle.release(window);
                }
                if candidate.bridge.windows.is_empty() {
                    let mut candidate = runtime.candidate.take().unwrap();
                    candidate.staged.cancel();
                    candidate.bridge.handle.stop().await;
                }
            }
            if let Some(active) = &mut runtime.active {
                if active.windows.remove(window) {
                    active.handle.release(window);
                }
                if active.windows.is_empty() {
                    runtime.active.take().unwrap().handle.stop().await;
                }
            }
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
                // F-07: a dead bridge must not keep publishing its wsUrl (every
                // reconnect would be closed) nor hold window references. Taking
                // it here also makes the reported status settle on `offline`.
                self.grants
                    .0
                    .lock()
                    .unwrap()
                    .retain(|_, binding| binding.server_id != *server_id);
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
        let runtime = core
            .hosts
            .get_mut(&self.server_id)
            .ok_or(PairingSaveError)?;
        let candidate = runtime
            .candidate
            .as_mut()
            .filter(|candidate| candidate.id == self.id)
            .ok_or(PairingSaveError)?;
        candidate
            .staged
            .remote_authenticated(token)
            .map_err(|_| PairingSaveError)?;
        candidate
            .staged
            .commit(&client.store)
            .map_err(|_| PairingSaveError)?;
        let mut next = runtime.candidate.take().unwrap().bridge;
        let old = runtime.active.take();
        if let Some(old) = &old {
            for window in &old.windows {
                next.acquire(window).map_err(|_| PairingSaveError)?;
            }
        }
        runtime.active = Some(next);
        if let Some(old) = old {
            old.handle.stop().await;
        }
        Ok(())
    }
}

fn bridges(runtime: &HostRuntime) -> impl Iterator<Item = &Bridge> {
    runtime
        .active
        .iter()
        .chain(runtime.candidate.iter().map(|candidate| &candidate.bridge))
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
    if runtime
        .candidate
        .as_ref()
        .is_some_and(|candidate| matches!(candidate.staged.state(), StagedState::SaveFailed { .. }))
    {
        return "saveFailed";
    }
    match runtime
        .candidate
        .as_ref()
        .map(|candidate| &candidate.bridge)
        .or(runtime.active.as_ref())
        .map(|bridge| *bridge.handle.status.borrow())
    {
        Some(HostBridgeStatus::Connecting) => "connecting",
        Some(HostBridgeStatus::Online) => "online",
        Some(HostBridgeStatus::SaveFailed) => "saveFailed",
        Some(HostBridgeStatus::CredentialInvalid) => "credentialInvalid",
        Some(HostBridgeStatus::PairingFailed) => "pairingFailed",
        Some(HostBridgeStatus::Stopped) => "stopped",
        _ => "offline",
    }
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
    json!({"serverId":server_id, "status":status,
        "wsUrl":bridge.map(|bridge| format!("{}/v2/ws", bridge.handle.origin.replacen("http://", "ws://", 1))),
        "pairing":runtime.is_some_and(|runtime| runtime.candidate.is_some()),
        "recovery":if status == "saveFailed" { json!({"code":"remotePairedLocalSaveFailed", "retrySave":true, "onProcessExit":"newOfferRequired"}) } else { Value::Null }})
}
fn check_window_id(injected: &str, reported: Option<&str>) -> Result<(), String> {
    if reported.is_some_and(|reported| reported != injected) {
        Err("Caller window identity mismatch".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
