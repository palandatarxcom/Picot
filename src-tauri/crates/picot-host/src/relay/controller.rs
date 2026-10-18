// ABOUTME: Relay lifecycle controller (plan T6 slice 1): enablement preference,
// ABOUTME: transport start/stop, status view, and the pairing-offer op face.

//! The ops' entire semantics live here so the admin dispatcher stays a thin
//! match (the `agent.stop` shape): [`RelayController`] owns the persisted
//! `relay.enabled` preference (reserved namespace), the transport handle, the
//! connection-state feed the status op reads, and the pairing-offer assembly
//! (identity + endpoint + fresh token — spec §4.1/§2.6).
//!
//! Invariants this module is on the hook for:
//!
//! - enable mints the identity **before** persisting the flag, so a corrupt
//!   identity row can never strand `relay.enabled=true` with no way to start
//!   (spec §2.3: corrupt rows refuse, they are never silently re-keyed);
//! - disable stops the transport (local close only — stop never waits on the
//!   network) and clears every unconsumed pairing token while paired device
//!   tokens survive untouched (spec §2.10 relay-off row);
//! - the offer op refuses while disabled — it never implicitly enables.

use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::metadata_store::SharedMetadataStore;
use crate::remote_auth::RemoteAuth;

use super::e2ee::{load_or_create_identity, peek_server_id};
use super::pairing::{build_pairing_offer, relay_endpoint, PairingOffer};
use super::transport::{
    ConnectionState, RelayConfig, RelaySessionSink, RelayTransport, RelayTransportHandle,
    TransportSeams,
};

/// Reserved-namespace preference key holding the persisted enable flag.
pub const ENABLED_PREFERENCE_KEY: &str = "relay.enabled";

/// How the controller dials when it starts the transport: endpoint + TLS plus
/// the injectable seams. The production plan is the company-relay constant
/// (TLS on) with `PICOT_RELAY_ENDPOINT` overriding the endpoint (spec §4);
/// `PICOT_RELAY_TLS=0` is the same dev-override family for local mock relays
/// that have no TLS listener — none of it reaches the UI.
#[derive(Clone)]
pub struct RelayStartPlan {
    pub endpoint: String,
    pub use_tls: bool,
    pub seams: TransportSeams,
}

impl Default for RelayStartPlan {
    fn default() -> Self {
        Self::production()
    }
}

impl RelayStartPlan {
    pub fn production() -> Self {
        let use_tls = !matches!(
            std::env::var("PICOT_RELAY_TLS")
                .ok()
                .map(|value| value.trim().to_ascii_lowercase()),
            Some(ref value) if value == "0" || value == "false"
        );
        Self {
            endpoint: relay_endpoint(),
            use_tls,
            seams: TransportSeams::default(),
        }
    }
}

/// The `relay.status` op result (spec §4.1): everything the Pair Device state
/// row renders. `state` is the wire string (`offline`/`connecting`/`online`);
/// `server_id` is `None` until the first enable mints the identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayStatusView {
    pub enabled: bool,
    pub state: String,
    pub endpoint: String,
    pub server_id: Option<String>,
}

/// Forwarding sink that taps the transport's connection-state reports on their
/// way to the session bridge, so `relay.status` reads live state without the
/// bridge growing a second job.
struct StatusTap {
    inner: Arc<dyn RelaySessionSink>,
    connection_tx: watch::Sender<ConnectionState>,
}

#[async_trait::async_trait]
impl RelaySessionSink for StatusTap {
    async fn on_connection_state(&self, state: ConnectionState) {
        let _ = self.connection_tx.send(state);
        self.inner.on_connection_state(state).await;
    }

    async fn open_session(
        &self,
        connection_id: String,
        channel: super::transport::EncryptedChannelHandle,
    ) {
        self.inner.open_session(connection_id, channel).await;
    }

    async fn data_frame(&self, connection_id: &str, payload: String) {
        self.inner.data_frame(connection_id, payload).await;
    }

    async fn close_session(&self, connection_id: &str) {
        self.inner.close_session(connection_id).await;
    }
}

/// Owns the daemon's relay lifecycle. Built once by `DaemonState::start` and
/// shared with the admin dispatcher; every method is safe to call from any
/// task (the transport slot is an async mutex because `stop` awaits).
pub struct RelayController {
    metadata: SharedMetadataStore,
    remote_auth: Arc<Mutex<RemoteAuth>>,
    session_sink: Arc<dyn RelaySessionSink>,
    plan: Mutex<RelayStartPlan>,
    transport: tokio::sync::Mutex<Option<RelayTransportHandle>>,
    connection_tx: watch::Sender<ConnectionState>,
    connection_rx: watch::Receiver<ConnectionState>,
}

impl RelayController {
    pub fn new(
        metadata: SharedMetadataStore,
        session_sink: Arc<dyn RelaySessionSink>,
        remote_auth: Arc<Mutex<RemoteAuth>>,
    ) -> Self {
        // The receiver stays owned here: dropping every receiver closes a
        // watch channel and turns later `send`s into silent no-ops, so the
        // status read and the tap's writes must share one live channel.
        let (connection_tx, connection_rx) = watch::channel(ConnectionState::Offline);
        Self {
            metadata,
            remote_auth,
            session_sink,
            plan: Mutex::new(RelayStartPlan::production()),
            transport: tokio::sync::Mutex::new(None),
            connection_tx,
            connection_rx,
        }
    }

    /// The persisted enable flag (reserved namespace; absent reads as false).
    pub fn enabled(&self) -> Result<bool, String> {
        let guard = self
            .metadata
            .lock()
            .map_err(|_| "Picot metadata lock poisoned".to_string())?;
        Ok(guard
            .relay_pref_get(ENABLED_PREFERENCE_KEY)?
            .and_then(|value| value.as_bool())
            .unwrap_or(false))
    }

    /// The `relay.status` view: enabled + live connection state + dial plan +
    /// identity serverId (None before first enable).
    pub fn status(&self) -> Result<RelayStatusView, String> {
        let enabled = self.enabled()?;
        let state = if enabled {
            connection_state_str(&self.connection_rx.borrow())
        } else {
            "offline"
        };
        let endpoint = self
            .plan
            .lock()
            .map_err(|_| "relay start plan lock poisoned".to_string())?
            .endpoint
            .clone();
        let server_id = peek_server_id(&self.metadata)?;
        Ok(RelayStatusView {
            enabled,
            state: state.to_string(),
            endpoint,
            server_id,
        })
    }

    /// Flip the enable flag and move the transport with it (spec §4.1
    /// `relay_set_enabled`). Idempotent: enabling with the transport already
    /// running keeps it; disabling without one just persists and revokes.
    pub async fn set_enabled(&self, enabled: bool) -> Result<bool, String> {
        if enabled {
            // Identity first, flag second: a corrupt identity row must leave
            // the stored state unchanged rather than promise a transport the
            // next boot cannot start (spec §2.3).
            let identity = load_or_create_identity(&self.metadata)?;
            self.metadata
                .lock()
                .map_err(|_| "Picot metadata lock poisoned".to_string())?
                .relay_pref_set(ENABLED_PREFERENCE_KEY, &serde_json::json!(true))?;
            let mut slot = self.transport.lock().await;
            if slot.is_none() {
                let plan = self
                    .plan
                    .lock()
                    .map_err(|_| "relay start plan lock poisoned".to_string())?
                    .clone();
                let sink = Arc::new(StatusTap {
                    inner: Arc::clone(&self.session_sink),
                    connection_tx: self.connection_tx.clone(),
                });
                *slot = Some(RelayTransport::start_with_seams(
                    RelayConfig {
                        endpoint: plan.endpoint,
                        use_tls: plan.use_tls,
                        server_id: identity.server_id.clone(),
                    },
                    identity,
                    sink,
                    plan.seams,
                ));
            }
            Ok(true)
        } else {
            self.metadata
                .lock()
                .map_err(|_| "Picot metadata lock poisoned".to_string())?
                .relay_pref_set(ENABLED_PREFERENCE_KEY, &serde_json::json!(false))?;
            self.stop_transport().await;
            // Relay-off clears every unconsumed pairing token; paired device
            // tokens are a different pool and stay valid (spec §2.10).
            self.remote_auth
                .lock()
                .map_err(|_| "remote auth lock poisoned".to_string())?
                .revoke_pending();
            Ok(false)
        }
    }

    /// Stop the transport without touching the persisted flag — the drain path
    /// and the test fixture use this (a restart must not read as "disabled").
    pub async fn stop_transport(&self) {
        if let Some(handle) = self.transport.lock().await.take() {
            handle.stop().await;
        }
        let _ = self.connection_tx.send(ConnectionState::Offline);
    }

    /// The `relay.offer` op (spec §4.1 `relay_pairing_offer`): identity +
    /// endpoint + a freshly minted one-scan pairing token rendered as the
    /// `#offer=` URL. The mint records `owner` — the binding exchange stamps
    /// onto the device token (Gate B, spec §2.8). Refuses while disabled —
    /// no implicit enable.
    pub fn pairing_offer(&self, owner: &str) -> Result<PairingOffer, String> {
        if !self.enabled()? {
            return Err(
                "relay is not enabled: enable relay before minting a pairing offer".to_string(),
            );
        }
        let identity = load_or_create_identity(&self.metadata)?;
        let (endpoint, use_tls) = {
            let plan = self
                .plan
                .lock()
                .map_err(|_| "relay start plan lock poisoned".to_string())?;
            (plan.endpoint.clone(), plan.use_tls)
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let pairing = self
            .remote_auth
            .lock()
            .map_err(|_| "remote auth lock poisoned".to_string())?
            .mint_fresh_pairing_token(now, Some(owner));
        build_pairing_offer(&identity, &endpoint, use_tls, &pairing)
    }

    /// Test seam: point the dial plan at a local mock relay before enabling.
    #[cfg(any(test, feature = "test-seams"))]
    pub fn set_start_plan_for_tests(&self, plan: RelayStartPlan) {
        *self.plan.lock().expect("relay start plan lock") = plan;
    }

    /// Test seam: the shared `RemoteAuth` the offer/disable paths use, so
    /// token-lifecycle assertions go through the same instance as the ops.
    #[cfg(any(test, feature = "test-seams"))]
    pub fn remote_auth_for_tests(&self) -> &Arc<Mutex<RemoteAuth>> {
        &self.remote_auth
    }
}

fn connection_state_str(state: &ConnectionState) -> &'static str {
    match state {
        ConnectionState::Offline => "offline",
        ConnectionState::Connecting => "connecting",
        ConnectionState::Online => "online",
    }
}
