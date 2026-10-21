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
    start_host_bridge, Credential, HostBridgeHandle, HostBridgeStatus, PairingSaveError,
    PairingSink,
};
use picot_host::relay::host_store::{DeviceToken, HostProfile, HostStore};
use picot_host::relay::host_tofu::{
    self, Answer, ConfirmationQueue, ConnectionChange, TofuDecision,
};
use picot_host::relay::pairing::{parse_connection_offer, ConnectionOfferV2};
use serde_json::{json, Value};

pub struct HostClientState(pub Result<Arc<HostClient>, String>);

struct Binding {
    server_id: String,
    origin: String,
}
/// Which host each live window is on: a window serves one host at a time, so a
/// switch compares the new target against this binding before it releases.
#[derive(Default)]
struct WindowHosts(Mutex<HashMap<String, Binding>>);

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
        let mut core = self.core.lock().await;
        running(&core)?;
        self.refresh_invalid(&mut core).await?;
        // A new offer supersedes the candidate its predecessor left behind: a
        // candidate never reaches `host_list`, so the user cannot clear it there.
        discard_candidate(&mut core, &offer.server_id).await;
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
        self.switch_window(&mut core, window, origin, &offer.server_id)
            .await;
        core.hosts
            .entry(offer.server_id.clone())
            .or_default()
            .candidate = Some(Candidate {
            id,
            profile,
            bridge,
        });
        Ok(status_payload(&core, &offer.server_id))
    }

    /// Report a host's status and hand the calling window its bridge URL. This is
    /// the window's one entry point: it starts the host's bridge and takes the
    /// window's reference on it, exactly as the connection it is about to open.
    pub async fn status(
        self: &Arc<Self>,
        window: &str,
        origin: &str,
        server_id: &str,
    ) -> Result<Value, String> {
        let mut core = self.core.lock().await;
        running(&core)?;
        self.refresh_invalid(&mut core).await?;
        if !core.hosts.contains_key(server_id) && self.store.host(server_id).is_none() {
            return Err("Unknown host".into());
        }
        self.connect_saved(&mut core, window, origin, server_id)
            .await?;
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
        if let Some(runtime) = core.hosts.remove(server_id) {
            // Dropped before any await: a stale callback fails the id lookup.
            self.window_hosts
                .0
                .lock()
                .unwrap()
                .retain(|_, binding| binding.server_id != server_id);
            stop_runtime(runtime).await;
        }
        self.store
            .delete(server_id)
            .map_err(|_| "Could not delete host; local profile retained")?;
        Ok(())
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
        self.window_hosts.0.lock().unwrap().clear();
        for (_, runtime) in core.hosts.drain() {
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

    /// A window serves one host at a time: connecting it to `server_id` releases
    /// its reference on every other host's bridge.
    async fn switch_window(&self, core: &mut Core, window: &str, origin: &str, server_id: &str) {
        let changed = self
            .window_hosts
            .0
            .lock()
            .unwrap()
            .get(window)
            .is_some_and(|binding| binding.server_id != server_id || binding.origin != origin);
        if changed {
            self.release_window(core, window, Some(server_id)).await;
        }
        self.window_hosts.0.lock().unwrap().insert(
            window.into(),
            Binding {
                server_id: server_id.into(),
                origin: origin.into(),
            },
        );
    }

    async fn release_window(&self, core: &mut Core, window: &str, keep_host: Option<&str>) {
        self.window_hosts.0.lock().unwrap().remove(window);
        for (server_id, runtime) in &mut core.hosts {
            if keep_host == Some(server_id.as_str()) {
                continue;
            }
            if let Some(candidate) = &mut runtime.candidate {
                if candidate.bridge.windows.remove(window) {
                    candidate.bridge.handle.release(window);
                }
                if candidate.bridge.windows.is_empty() {
                    runtime.candidate.take().unwrap().bridge.handle.stop().await;
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
                self.window_hosts
                    .0
                    .lock()
                    .unwrap()
                    .retain(|_, binding| binding.server_id != *server_id);
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
        "pairing":runtime.is_some_and(|runtime| runtime.candidate.is_some())})
}

#[cfg(test)]
mod tests;
