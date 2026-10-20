// ABOUTME: Per-host loopback WS bridge with window grants and independent relay sessions.
// ABOUTME: Rewrites hello, sanitizes ack, delegates pairing persistence; last window stops bridge.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::{routing::get, Router};
use futures_util::{SinkExt, StreamExt};
use serde_json::value::RawValue;
use tokio::sync::{mpsc, watch};

use super::client_role::{
    ClientChannel, ClientRole, ClientRoleConfig, ClientRoleHandle, ClientRoleSeams, ClientRoleSink,
};
use super::host_store::DeviceToken;
use super::transport::ConnectionState;
use crate::transport_limits::{OUTBOUND_QUEUE_CAPACITY, WS_PHYSICAL_FRAME_BYTES};

/// Debug deliberately omitted: grants are local bearer credentials.
pub struct WindowGrant {
    pub token: String,
    pub window_id: String,
    pub server_id: String,
}

pub trait GrantRegistry: Send + Sync {
    /// Exact Origin + host + live-window validation; return trusted window identity.
    fn validate(&self, token: &str, server_id: &str, origin: &str) -> Option<String>;
}

#[derive(Clone, Debug)]
pub enum Credential {
    Device(DeviceToken),
    Pairing {
        pairing_token: DeviceToken,
        device_id: String,
    },
}

/// No error text: storage paths/errors must not accidentally expose credentials.
#[derive(Debug)]
pub struct PairingSaveError;

#[async_trait]
pub trait PairingSink: Send + Sync {
    /// T7 owns the staged transaction and durable commit. On failure it must retain
    /// this candidate for retry-save; the bridge blocks further pairing attempts.
    /// Cancellation (host removal/stop) drops this future: no detached writes.
    async fn persist(&self, token: DeviceToken) -> Result<(), PairingSaveError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostBridgeStatus {
    Connecting,
    Online,
    Offline,
    SaveFailed,
    CredentialInvalid,
    PairingFailed,
    Stopped,
}

#[derive(Clone)]
pub struct BridgeSeams {
    pub first_frame_timeout: Duration,
    pub client: ClientRoleSeams,
}
impl Default for BridgeSeams {
    fn default() -> Self {
        Self {
            first_frame_timeout: Duration::from_secs(15),
            client: ClientRoleSeams::default(),
        }
    }
}

struct Windows {
    refs: HashSet<String>,
    cancel: HashMap<String, watch::Sender<bool>>,
}
struct Auth {
    credential: Credential,
    pairing_busy: bool,
}
struct Shared {
    cfg: ClientRoleConfig,
    grants: Arc<dyn GrantRegistry>,
    pairing: Option<Arc<dyn PairingSink>>,
    auth: Mutex<Auth>,
    windows: Mutex<Windows>,
    slots: Mutex<Vec<Arc<Slot>>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    shutdown: watch::Sender<bool>,
    status: watch::Sender<HostBridgeStatus>,
    seams: BridgeSeams,
}
impl Shared {
    fn set_status(&self, status: HostBridgeStatus) {
        self.status.send_if_modified(|current| {
            if matches!(
                *current,
                HostBridgeStatus::SaveFailed
                    | HostBridgeStatus::CredentialInvalid
                    | HostBridgeStatus::PairingFailed
                    | HostBridgeStatus::Stopped
            ) {
                return false;
            }
            *current = status;
            true
        });
    }
}

pub struct HostBridgeHandle {
    pub origin: String,
    pub status: watch::Receiver<HostBridgeStatus>,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}
impl HostBridgeHandle {
    /// Idempotent per-window acquire, not per socket. Initial refcount is zero;
    /// T7 acquires before publishing the bridge URL. A stopped bridge cannot revive.
    pub fn acquire(&self, window_id: &str) -> bool {
        let mut windows = self.shared.windows.lock().unwrap();
        if *self.shared.shutdown.borrow() {
            return false;
        }
        windows.refs.insert(window_id.to_owned());
        true
    }
    /// Release a window once; unknown/duplicate releases do nothing. Last release
    /// requests shutdown; `stop().await` also joins teardown.
    pub fn release(&self, window_id: &str) {
        let mut windows = self.shared.windows.lock().unwrap();
        if let Some(cancel) = windows.cancel.remove(window_id) {
            cancel.send_replace(true);
        }
        if windows.refs.remove(window_id) && windows.refs.is_empty() {
            self.shared.shutdown.send_replace(true);
        }
    }
    /// T7 MUST invalidate GrantRegistry first, THEN call this to close live sockets.
    /// This does not release the window reference (renewal and release are separate).
    pub fn invalidate_window(&self, window_id: &str) {
        if let Some(cancel) = self.shared.windows.lock().unwrap().cancel.remove(window_id) {
            cancel.send_replace(true);
        }
    }
    /// Cancels upgrades, dial, handshake, callback and all session tasks; joins locally.
    pub async fn stop(self) {
        self.shared.shutdown.send_replace(true);
        let _ = self.task.await;
    }
    /// Test seam (`test-seams`, same posture as `DaemonIdentity::for_tests`): force a
    /// terminal status so app-side reactions to bridge states need no live relay.
    #[cfg(any(test, feature = "test-seams"))]
    pub fn force_status_for_tests(&self, status: HostBridgeStatus) {
        self.shared.status.send_replace(status);
    }
}

pub async fn start_host_bridge(
    cfg: ClientRoleConfig,
    credential: Credential,
    grants: Arc<dyn GrantRegistry>,
    pairing_sink: Option<Arc<dyn PairingSink>>,
) -> std::io::Result<HostBridgeHandle> {
    start_host_bridge_with_seams(
        cfg,
        credential,
        grants,
        pairing_sink,
        BridgeSeams::default(),
    )
    .await
}

pub async fn start_host_bridge_with_seams(
    cfg: ClientRoleConfig,
    credential: Credential,
    grants: Arc<dyn GrantRegistry>,
    pairing_sink: Option<Arc<dyn PairingSink>>,
    seams: BridgeSeams,
) -> std::io::Result<HostBridgeHandle> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let (shutdown, mut stopped) = watch::channel(false);
    let (status_tx, status) = watch::channel(HostBridgeStatus::Offline);
    let shared = Arc::new(Shared {
        cfg,
        grants,
        pairing: pairing_sink,
        auth: Mutex::new(Auth {
            credential,
            pairing_busy: false,
        }),
        windows: Mutex::new(Windows {
            refs: HashSet::new(),
            cancel: HashMap::new(),
        }),
        slots: Mutex::new(Vec::new()),
        tasks: Mutex::new(Vec::new()),
        shutdown,
        status: status_tx,
        seams,
    });
    let app = Router::new()
        .route("/v2/ws", get(upgrade))
        .with_state(shared.clone());
    let owner = shared.clone();
    let task = tokio::spawn(async move {
        // Drop the listener immediately on shutdown; upgraded sockets are joined below.
        tokio::select! {
            _ = axum::serve(listener, app) => {},
            _ = stopped.changed() => {},
        }
        owner.shutdown.send_replace(true);
        let tasks = std::mem::take(&mut *owner.tasks.lock().unwrap());
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        let slots = std::mem::take(&mut *owner.slots.lock().unwrap());
        for slot in slots {
            slot.stop().await;
        }
        owner.status.send_replace(HostBridgeStatus::Stopped);
    });
    Ok(HostBridgeHandle {
        origin,
        status,
        shared,
        task,
    })
}

async fn upgrade(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    ws.max_message_size(WS_PHYSICAL_FRAME_BYTES)
        .max_frame_size(WS_PHYSICAL_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            let mut tasks = shared.tasks.lock().unwrap();
            if *shared.shutdown.borrow() {
                return;
            }
            tasks.retain(|task| !task.is_finished());
            tasks.push(tokio::spawn(proxy(socket, origin, shared.clone())));
        })
}

type Fields = BTreeMap<String, Box<RawValue>>;
fn string_field(fields: &Fields, key: &str) -> Option<String> {
    serde_json::from_str(fields.get(key)?.get()).ok()
}
fn insert(fields: &mut Fields, key: &str, value: &str) {
    fields.insert(
        key.into(),
        RawValue::from_string(serde_json::to_string(value).unwrap()).unwrap(),
    );
}
fn rewrite(mut fields: Fields, credential: &Credential) -> String {
    for key in [
        "desktopCapability",
        "bridgeGrant",
        "deviceToken",
        "pairingToken",
        "deviceId",
    ] {
        fields.remove(key);
    }
    insert(&mut fields, "clientType", "remote");
    match credential {
        Credential::Device(token) => insert(&mut fields, "deviceToken", token.expose()),
        Credential::Pairing {
            pairing_token,
            device_id,
        } => {
            insert(&mut fields, "pairingToken", pairing_token.expose());
            insert(&mut fields, "deviceId", device_id);
        }
    }
    serde_json::to_string(&fields).unwrap()
}

#[derive(Clone)]
struct Ready {
    generation: u64,
    channel: Option<ClientChannel>,
}
struct Attachment {
    frames: mpsc::Sender<String>,
    lost: watch::Sender<bool>,
}
struct Slot {
    shared: std::sync::Weak<Shared>,
    ready: watch::Sender<Ready>,
    attachment: Mutex<Option<Attachment>>,
    role: Mutex<Option<ClientRoleHandle>>,
}
impl Slot {
    async fn stop(&self) {
        let role = self.role.lock().unwrap().take();
        if let Some(role) = role {
            role.stop().await;
        }
    }
}
#[async_trait]
impl ClientRoleSink for Slot {
    async fn on_state(&self, state: ConnectionState) {
        if state == ConnectionState::Offline {
            self.ready.send_modify(|ready| ready.channel = None);
            if let Some(attachment) = self.attachment.lock().unwrap().as_ref() {
                attachment.lost.send_replace(true);
            }
        }
        if let Some(shared) = self.shared.upgrade() {
            shared.set_status(match state {
                ConnectionState::Connecting => HostBridgeStatus::Connecting,
                ConnectionState::Online => HostBridgeStatus::Online,
                ConnectionState::Offline => HostBridgeStatus::Offline,
            });
        }
    }
    async fn on_online(&self, channel: ClientChannel) {
        self.ready.send_modify(|ready| {
            ready.generation += 1;
            ready.channel = Some(channel);
        });
    }
    async fn on_frame(&self, payload: String) {
        if let Some(attachment) = self.attachment.lock().unwrap().as_ref() {
            if attachment.frames.try_send(payload).is_err() {
                attachment.lost.send_replace(true);
            }
        }
    }
}

fn attach(
    shared: &Arc<Shared>,
    frames: mpsc::Sender<String>,
    lost: watch::Sender<bool>,
) -> Arc<Slot> {
    let mut slots = shared.slots.lock().unwrap();
    for slot in slots.iter() {
        let mut attachment = slot.attachment.lock().unwrap();
        if attachment.is_none() {
            *attachment = Some(Attachment { frames, lost });
            return slot.clone();
        }
    }
    let (ready, _) = watch::channel(Ready {
        generation: 0,
        channel: None,
    });
    let slot = Arc::new(Slot {
        shared: Arc::downgrade(shared),
        ready,
        attachment: Mutex::new(Some(Attachment { frames, lost })),
        role: Mutex::new(None),
    });
    *slot.role.lock().unwrap() = Some(ClientRole::start_with_seams(
        shared.cfg.clone(),
        slot.clone(),
        shared.seams.client.clone(),
    ));
    slots.push(slot.clone());
    slot
}

async fn proxy(mut socket: WebSocket, origin: String, shared: Arc<Shared>) {
    let first = tokio::time::timeout(shared.seams.first_frame_timeout, socket.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = first else {
        return;
    };
    let Ok(fields) = serde_json::from_str::<Fields>(&text) else {
        return;
    };
    if string_field(&fields, "type").as_deref() != Some("hello") {
        return;
    }
    let Some(token) = string_field(&fields, "bridgeGrant") else {
        return;
    };
    // Serialize validation with invalidate_window so an invalidation cannot miss
    // an authenticated socket between its registry check and live registration.
    let mut window_cancel = {
        let mut windows = shared.windows.lock().unwrap();
        let Some(window) = shared
            .grants
            .validate(&token, &shared.cfg.server_id, &origin)
        else {
            return;
        };
        windows
            .cancel
            .entry(window)
            .or_insert_with(|| watch::channel(false).0)
            .subscribe()
    };
    let credential = {
        let mut auth = shared.auth.lock().unwrap();
        if matches!(
            *shared.status.borrow(),
            HostBridgeStatus::SaveFailed
                | HostBridgeStatus::CredentialInvalid
                | HostBridgeStatus::PairingFailed
        ) {
            return;
        }
        if matches!(auth.credential, Credential::Pairing { .. }) {
            // Reject, never queue: one host-level exchange and one app hello.
            if auth.pairing_busy || shared.pairing.is_none() {
                return;
            }
            auth.pairing_busy = true;
        }
        auth.credential.clone()
    };
    let pairing = matches!(credential, Credential::Pairing { .. });
    let hello = rewrite(fields, &credential);
    if hello.len() > WS_PHYSICAL_FRAME_BYTES {
        if pairing {
            shared.auth.lock().unwrap().pairing_busy = false;
        }
        return;
    }
    let (frames_tx, mut frames) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
    let (lost_tx, mut lost) = watch::channel(false);
    let slot = attach(&shared, frames_tx, lost_tx);
    let mut ready = slot.ready.subscribe();
    let mut shutdown = shared.shutdown.subscribe();
    let (mut writer, mut reader) = socket.split();
    let (out, mut outbound) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
    let write = async move {
        while let Some(frame) = outbound.recv().await {
            if writer.send(frame).await.is_err() {
                break;
            }
        }
    };
    let mut generation = None;
    let session = async {
        let channel = loop {
            let current = ready.borrow_and_update().clone();
            if let Some(channel) = current.channel {
                generation = Some(current.generation);
                break channel;
            }
            tokio::select! {
                result = ready.changed() => { if result.is_err() { return; } },
                frame = reader.next() => match frame {
                    Some(Ok(Message::Ping(bytes))) => { if out.try_send(Message::Pong(bytes)).is_err() { return; } },
                    Some(Ok(Message::Pong(_))) => {},
                    _ => return,
                },
            }
        };
        if !channel.send(hello) {
            return;
        }
        // Only the first response is interpreted. All later business frames are opaque.
        // Bounded by the same seam as the app hello: a peer that swallows hello must not
        // pin this proxy (and `auth.pairing_busy`) forever; timeout ends as a failure.
        let deadline = tokio::time::sleep(shared.seams.first_frame_timeout);
        tokio::pin!(deadline);
        let ack = loop {
            tokio::select! {
                _ = &mut deadline => return,
                frame = frames.recv() => { let Some(ack) = frame else { return; }; break ack; },
                frame = reader.next() => match frame {
                    Some(Ok(Message::Ping(bytes))) => { if out.try_send(Message::Pong(bytes)).is_err() { return; } },
                    Some(Ok(Message::Pong(_))) => {},
                    _ => return,
                },
            }
        };
        let Ok(mut fields) = serde_json::from_str::<Fields>(&ack) else {
            if pairing {
                shared.set_status(HostBridgeStatus::PairingFailed);
            }
            return;
        };
        let is_ack = string_field(&fields, "type").as_deref() == Some("hello_ack");
        if pairing && !is_ack {
            // Terminal: the single-use offer was refused (expired/used). Re-sending the
            // same token would loop, so no retry — only a new candidate bridge clears this.
            shared.set_status(HostBridgeStatus::PairingFailed);
            return;
        }
        if fields
            .get("error")
            .and_then(|raw| serde_json::from_str::<Fields>(raw.get()).ok())
            .and_then(|error| string_field(&error, "code"))
            .as_deref()
            == Some("unauthorized_device")
        {
            shared.set_status(HostBridgeStatus::CredentialInvalid);
        }
        let token = fields.remove("deviceToken");
        if pairing {
            let Some(token) = token
                .as_ref()
                .and_then(|raw| serde_json::from_str::<String>(raw.get()).ok())
                .filter(|token| !token.is_empty())
            else {
                return;
            };
            let token = DeviceToken::new(token);
            if shared
                .pairing
                .as_ref()
                .unwrap()
                .persist(token.clone())
                .await
                .is_err()
            {
                shared.set_status(HostBridgeStatus::SaveFailed);
                return;
            }
            let mut auth = shared.auth.lock().unwrap();
            auth.credential = Credential::Device(token);
            auth.pairing_busy = false;
        }
        let ack = if token.is_some() {
            serde_json::to_string(&fields).unwrap()
        } else {
            ack
        };
        if out.try_send(Message::Text(ack.into())).is_err() || !is_ack {
            return;
        }
        loop {
            tokio::select! {
                frame = frames.recv() => match frame {
                    Some(text) => { if out.try_send(Message::Text(text.into())).is_err() { return; } },
                    _ => return,
                },
                frame = reader.next() => match frame {
                    Some(Ok(Message::Text(text))) if text.len() <= WS_PHYSICAL_FRAME_BYTES => {
                        if !channel.send(text.to_string()) { return; }
                    },
                    Some(Ok(Message::Ping(bytes))) => { if out.try_send(Message::Pong(bytes)).is_err() { return; } },
                    Some(Ok(Message::Pong(_))) => {},
                    _ => return,
                }
            }
        }
    };
    let transport_dropped = tokio::select! {
        // A terminal error frame can arrive in the same poll as Offline. Consume
        // the queued explicit rejection before the transport-loss notification.
        biased;
        _ = session => false,
        _ = lost.changed() => true,
        _ = write => false,
        _ = window_cancel.changed() => false,
        _ = shutdown.changed() => false,
    };
    if pairing && *shared.status.borrow() != HostBridgeStatus::SaveFailed {
        shared.auth.lock().unwrap().pairing_busy = false;
    }
    // A dropped transport stays alive, backs off and becomes an idle session for
    // the frontend's next hello. Never replay hello on the fresh relay session.
    let current = slot.ready.borrow().clone();
    let keep = transport_dropped
        && (current.channel.is_none() || generation.is_some_and(|old| old != current.generation));
    if keep {
        *slot.attachment.lock().unwrap() = None;
    } else {
        slot.stop().await;
        shared
            .slots
            .lock()
            .unwrap()
            .retain(|candidate| !Arc::ptr_eq(candidate, &slot));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot() -> (Slot, mpsc::Receiver<String>, watch::Receiver<bool>) {
        let (frames, receiver) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
        let (lost, lost_rx) = watch::channel(false);
        let (ready, _) = watch::channel(Ready {
            generation: 0,
            channel: None,
        });
        (
            Slot {
                shared: std::sync::Weak::new(),
                ready,
                attachment: Mutex::new(Some(Attachment { frames, lost })),
                role: Mutex::new(None),
            },
            receiver,
            lost_rx,
        )
    }

    #[tokio::test]
    async fn slow_consumer_overflow_signals_only_its_own_connection() {
        tokio::time::timeout(Duration::from_secs(2), async {
            let (slow, _unread, slow_closed) = slot();
            let (healthy, mut read, healthy_closed) = slot();
            for _ in 0..OUTBOUND_QUEUE_CAPACITY {
                slow.on_frame("frame".into()).await;
            }
            assert!(!*slow_closed.borrow());
            slow.on_frame("overflow".into()).await;
            assert!(*slow_closed.borrow());
            healthy.on_frame("still live".into()).await;
            assert_eq!(read.recv().await.as_deref(), Some("still live"));
            assert!(!*healthy_closed.borrow());
        })
        .await
        .expect("slow-consumer deadline");
    }

    #[test]
    fn credential_debug_redacts_both_token_kinds() {
        assert!(!format!(
            "{:?}",
            Credential::Device(DeviceToken::new("private-token"))
        )
        .contains("private-token"));
        assert!(!format!(
            "{:?}",
            Credential::Pairing {
                pairing_token: DeviceToken::new("private-offer"),
                device_id: "device".into()
            }
        )
        .contains("private-offer"));
    }
}
