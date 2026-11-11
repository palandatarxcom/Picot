// ABOUTME: Per-host loopback WS bridge with independent relay sessions per window.
// ABOUTME: Rewrites hello, sanitizes ack, delegates pairing persistence; last window stops bridge.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::{routing::get, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use serde_json::value::RawValue;
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, watch};

use super::client_role::{
    ClientChannel, ClientRole, ClientRoleConfig, ClientRoleHandle, ClientRoleSeams, ClientRoleSink,
};
use super::host_store::DeviceToken;
use super::transport::ConnectionState;
use crate::transport_limits::{OUTBOUND_QUEUE_CAPACITY, WS_PHYSICAL_FRAME_BYTES};

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
    /// The app owns the durable commit. A failure ends the exchange: the remote
    /// already counts this device, so the host has to pair again.
    /// Cancellation (host removal/stop) drops this future: no detached writes.
    async fn persist(&self, token: DeviceToken) -> Result<(), PairingSaveError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostBridgeStatus {
    Connecting,
    Online,
    Offline,
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

struct Auth {
    credential: Credential,
    pairing_busy: bool,
}

/// The only grant version this bridge issues and accepts (spec §12, contract 3).
pub const GRANT_VERSION: u32 = 1;
/// Fixed, nonsecret failure text: a grant refusal never quotes caller bytes.
pub const GRANT_UNAVAILABLE: &str = "Host grant unavailable";
const GRANT_PREFIX: &str = "bg1_";
const GRANT_BODY_CHARS: usize = 43;

/// An exact page origin: scheme, normalized hostname and effective port. Two
/// loopback aliases (`localhost` / `127.0.0.1`) are different origins here on
/// purpose — "exact" means field-for-field equality with the trusted page.
#[derive(Clone, PartialEq, Eq)]
pub struct PageOrigin {
    scheme: String,
    host: String,
    port: u16,
}
impl PageOrigin {
    /// Parses an Origin header or a captured page origin. Rejects userinfo,
    /// non-root paths, query/fragment, opaque origins and non-http(s) schemes.
    pub fn parse(raw: &str) -> Option<Self> {
        let url = url::Url::parse(raw).ok()?;
        if !matches!(url.scheme(), "http" | "https") || url.cannot_be_a_base() {
            return None;
        }
        if !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return None;
        }
        Some(Self {
            scheme: url.scheme().to_owned(),
            host: url.host_str()?.to_ascii_lowercase(),
            port: url.port_or_known_default()?,
        })
    }
}
impl fmt::Debug for PageOrigin {
    // Origins are metadata, not secrets; still rendered in one canonical shape.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}:{}", self.scheme, self.host, self.port)
    }
}

/// What the bridge refuses to hand out or admit. Never carries a secret.
#[derive(Debug)]
pub struct GrantError;

struct ProxyLease {
    cancel: Arc<watch::Sender<bool>>,
    revoked: watch::Receiver<bool>,
}

/// One window's authorization for one host: the exact page origin captured at
/// attachment, an `issuable` flag, the current secret and the proxies it owns.
///
/// Identity is the `Arc` itself (`Arc::ptr_eq`), never a label: a reopened
/// window with the same label gets a new record, so a late cleanup that still
/// holds the old one cannot act on the replacement.
pub struct BindingRecord {
    server_id: String,
    origin: PageOrigin,
    origin_text: String,
    state: Mutex<BindingState>,
}
#[derive(Default)]
struct BindingState {
    issuable: bool,
    grant: Option<String>,
    /// One cancellation handle per authorized proxy this binding owns. Revoke
    /// signals all of them; each proxy removes its own entry when it ends.
    proxies: Vec<Arc<watch::Sender<bool>>>,
}
impl BindingRecord {
    /// `None` when the captured page origin is not an exact http(s) origin.
    pub fn new(server_id: impl Into<String>, origin: &str) -> Option<Arc<Self>> {
        Some(Arc::new(Self {
            server_id: server_id.into(),
            origin: PageOrigin::parse(origin)?,
            origin_text: origin.to_owned(),
            state: Mutex::new(BindingState {
                issuable: true,
                grant: None,
                proxies: Vec::new(),
            }),
        }))
    }
    pub fn server_id(&self) -> &str {
        &self.server_id
    }
    pub fn origin(&self) -> &str {
        &self.origin_text
    }
    /// False once invalidated: a revoked record never authorizes again, even
    /// while references to it still exist.
    pub fn is_issuable(&self) -> bool {
        self.state.lock().unwrap().issuable
    }
    /// Atomically replaces the current secret. Not issuance authority: the
    /// caller must already have proven it holds this exact live record.
    fn mint(&self) -> Result<String, GrantError> {
        let mut state = self.state.lock().unwrap();
        if !state.issuable {
            return Err(GrantError);
        }
        let secret = random_grant().ok_or(GrantError)?;
        state.grant = Some(secret.clone());
        Ok(secret)
    }
    /// The authority boundary for admission: sets `issuable = false`, drops the
    /// secret and signals every proxy this binding owns, all before returning.
    pub fn revoke(&self) {
        let mut state = self.state.lock().unwrap();
        state.issuable = false;
        state.grant = None;
        for proxy in std::mem::take(&mut state.proxies) {
            proxy.send_replace(true);
        }
    }
    /// Admits one proxy for `presented` when the binding is still issuable, the
    /// secret matches, and the presented origin is this binding's page origin.
    /// Registers proxy ownership before the caller performs any side effect.
    fn authorize(&self, presented: &str, origin: &PageOrigin) -> Option<ProxyLease> {
        let mut state = self.state.lock().unwrap();
        if !state.issuable || self.origin != *origin {
            return None;
        }
        let current = state.grant.as_deref()?;
        if !constant_time_eq(current.as_bytes(), presented.as_bytes()) {
            return None;
        }
        let (cancel, revoked) = watch::channel(false);
        let cancel = Arc::new(cancel);
        state.proxies.push(cancel.clone());
        Some(ProxyLease { cancel, revoked })
    }
    /// Test seam: the current secret, for assertions that status polling and
    /// revoke do (not) rotate it. Never compiled into a production build.
    #[cfg(any(test, feature = "test-seams"))]
    pub fn grant_for_tests(&self) -> Option<String> {
        self.state.lock().unwrap().grant.clone()
    }
    fn release_proxy(&self, cancel: &Arc<watch::Sender<bool>>) {
        self.state
            .lock()
            .unwrap()
            .proxies
            .retain(|proxy| !Arc::ptr_eq(proxy, cancel));
    }
}
impl fmt::Debug for BindingRecord {
    // Derived Debug would print the secret; the record never renders itself.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BindingRecord")
            .field("server_id", &self.server_id)
            .field("origin", &self.origin)
            .field("issuable", &self.is_issuable())
            .finish_non_exhaustive()
    }
}

/// 32 cryptographically random bytes as `bg1_` plus 43 unpadded base64url
/// characters. A failed RNG yields no grant at all: no fallback entropy.
fn random_grant() -> Option<String> {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.try_fill_bytes(&mut bytes).ok()?;
    Some(format!("{GRANT_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// Shape check only: the value is compared against the stored secret later.
fn grant_shape_ok(grant: &str) -> bool {
    grant.len() == GRANT_PREFIX.len() + GRANT_BODY_CHARS
        && grant.starts_with(GRANT_PREFIX)
        && grant[GRANT_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

/// Test-only side-effect counters: a denied admission must prove zero
/// credential reads, rewrites and relay dials even while another proxy lives.
#[cfg(any(test, feature = "test-seams"))]
#[derive(Default)]
pub struct BridgeCounters {
    pub credential_reads: std::sync::atomic::AtomicU64,
    pub rewrites: std::sync::atomic::AtomicU64,
    pub dials: std::sync::atomic::AtomicU64,
}
#[cfg(any(test, feature = "test-seams"))]
impl BridgeCounters {
    pub fn snapshot(&self) -> (u64, u64, u64) {
        use std::sync::atomic::Ordering::SeqCst;
        (
            self.credential_reads.load(SeqCst),
            self.rewrites.load(SeqCst),
            self.dials.load(SeqCst),
        )
    }
}

struct Shared {
    cfg: ClientRoleConfig,
    pairing: Option<Arc<dyn PairingSink>>,
    auth: Mutex<Auth>,
    /// Window references: the bridge lives while at least one window holds it.
    windows: Mutex<HashSet<String>>,
    /// Live binding records registered on THIS bridge instance. Admission
    /// searches here, so a grant minted for another host (another bridge, or
    /// this host's previous bridge) authorizes nothing.
    bindings: Mutex<Vec<Arc<BindingRecord>>>,
    slots: Mutex<Vec<Arc<Slot>>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    shutdown: watch::Sender<bool>,
    status: watch::Sender<HostBridgeStatus>,
    seams: BridgeSeams,
    #[cfg(any(test, feature = "test-seams"))]
    counters: Arc<BridgeCounters>,
}
impl Shared {
    fn set_status(&self, status: HostBridgeStatus) {
        self.status.send_if_modified(|current| {
            if matches!(
                *current,
                HostBridgeStatus::CredentialInvalid
                    | HostBridgeStatus::PairingFailed
                    | HostBridgeStatus::Stopped
            ) {
                return false;
            }
            *current = status;
            true
        });
    }
    /// Admission lookup: the presented secret and the exact page origin must
    /// belong to the same live record registered on this bridge instance. The
    /// origin comparison happens inside the record's own lock, so a concurrent
    /// revoke can never slip between the two checks.
    fn authorize(
        &self,
        presented: &str,
        origin: &PageOrigin,
    ) -> Option<(Arc<BindingRecord>, ProxyLease)> {
        if *self.shutdown.borrow() {
            return None;
        }
        let bindings = self.bindings.lock().unwrap().clone();
        for binding in bindings {
            if binding.server_id() != self.cfg.server_id {
                continue;
            }
            if let Some(lease) = binding.authorize(presented, origin) {
                return Some((binding, lease));
            }
        }
        None
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
        windows.insert(window_id.to_owned());
        true
    }
    /// Release a window once; unknown/duplicate releases do nothing. Last release
    /// requests shutdown; `stop().await` also joins teardown.
    pub fn release(&self, window_id: &str) {
        let mut windows = self.shared.windows.lock().unwrap();
        if windows.remove(window_id) && windows.is_empty() {
            self.shared.shutdown.send_replace(true);
        }
    }
    /// Cancels upgrades, dial, handshake, callback and all session tasks; joins locally.
    pub async fn stop(self) {
        self.shared.shutdown.send_replace(true);
        let _ = self.task.await;
    }
    /// Registers a window binding on this bridge instance. False once the
    /// bridge has shut down: a stopped bridge never becomes issuable again.
    pub fn bind(&self, record: &Arc<BindingRecord>) -> bool {
        if *self.shared.shutdown.borrow() || record.server_id() != self.shared.cfg.server_id {
            return false;
        }
        let mut bindings = self.shared.bindings.lock().unwrap();
        if !bindings.iter().any(|known| Arc::ptr_eq(known, record)) {
            bindings.push(record.clone());
        }
        true
    }
    /// Drops the registration once the window's references are gone. The record
    /// itself is dropped by its last owner; a delayed unbind cannot remove the
    /// replacement because it is a different `Arc`.
    pub fn unbind(&self, record: &Arc<BindingRecord>) {
        self.shared
            .bindings
            .lock()
            .unwrap()
            .retain(|known| !Arc::ptr_eq(known, record));
    }
    /// Mints the current secret for a record bound to this live bridge. Issuance
    /// authority only: it never acquires, switches or reconstructs a binding.
    pub fn mint_grant(&self, record: &Arc<BindingRecord>) -> Result<String, GrantError> {
        if *self.shared.shutdown.borrow() || record.server_id() != self.shared.cfg.server_id {
            return Err(GrantError);
        }
        let bound = self
            .shared
            .bindings
            .lock()
            .unwrap()
            .iter()
            .any(|known| Arc::ptr_eq(known, record));
        if !bound {
            return Err(GrantError);
        }
        record.mint()
    }
    /// Synchronous invalidation: issuance and admission stop and every proxy
    /// this binding owns is signalled before this returns. Retained (kept for
    /// redial) slots owned by the record are dropped from the reuse pool and
    /// stopped asynchronously — the sync boundary only severs authorization.
    pub fn revoke(&self, record: &Arc<BindingRecord>) {
        record.revoke();
        let owned: Vec<Arc<Slot>> = self
            .shared
            .slots
            .lock()
            .unwrap()
            .iter()
            .filter(|slot| {
                slot.owner
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|owner| Arc::ptr_eq(owner, record))
            })
            .cloned()
            .collect();
        if owned.is_empty() {
            return;
        }
        self.shared
            .slots
            .lock()
            .unwrap()
            .retain(|slot| !owned.iter().any(|mine| Arc::ptr_eq(mine, slot)));
        tokio::spawn(async move {
            for slot in owned {
                slot.stop().await;
            }
        });
    }
    /// Test seam: side-effect counters for the denial matrix.
    #[cfg(any(test, feature = "test-seams"))]
    pub fn counters_for_tests(&self) -> Arc<BridgeCounters> {
        self.shared.counters.clone()
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
    pairing_sink: Option<Arc<dyn PairingSink>>,
) -> std::io::Result<HostBridgeHandle> {
    start_host_bridge_with_seams(cfg, credential, pairing_sink, BridgeSeams::default()).await
}

/// Starts a bridge; `seams` shortens the budgets only tests can shorten.
pub async fn start_host_bridge_with_seams(
    cfg: ClientRoleConfig,
    credential: Credential,
    pairing_sink: Option<Arc<dyn PairingSink>>,
    seams: BridgeSeams,
) -> std::io::Result<HostBridgeHandle> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let (shutdown, mut stopped) = watch::channel(false);
    let (status_tx, status) = watch::channel(HostBridgeStatus::Offline);
    let shared = Arc::new(Shared {
        cfg,
        pairing: pairing_sink,
        auth: Mutex::new(Auth {
            credential,
            pairing_busy: false,
        }),
        windows: Mutex::new(HashSet::new()),
        bindings: Mutex::new(Vec::new()),
        slots: Mutex::new(Vec::new()),
        tasks: Mutex::new(Vec::new()),
        shutdown,
        status: status_tx,
        seams,
        #[cfg(any(test, feature = "test-seams"))]
        counters: Arc::default(),
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
    let origin = single_origin(&headers);
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
    /// The binding that kept this slot alive (transport drop, session held for
    /// redial). A retained slot serves its own window's reconnect only; a
    /// different binding — or a revoked owner — never reuses it.
    owner: Mutex<Option<Arc<BindingRecord>>>,
}
impl Slot {
    /// True when `binding` is this slot's owner: a kept (reattachable) slot
    /// only accepts its own window back. Any other binding gets a fresh slot.
    fn owned_by(&self, binding: &Arc<BindingRecord>) -> bool {
        self.owner
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|owner| Arc::ptr_eq(owner, binding))
    }
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
    binding: &Arc<BindingRecord>,
    frames: mpsc::Sender<String>,
    lost: watch::Sender<bool>,
) -> Arc<Slot> {
    let mut slots = shared.slots.lock().unwrap();
    for slot in slots.iter() {
        let mut attachment = slot.attachment.lock().unwrap();
        if attachment.is_none() {
            if !slot.owned_by(binding) {
                continue;
            }
            *attachment = Some(Attachment { frames, lost });
            *slot.owner.lock().unwrap() = Some(binding.clone());
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
        owner: Mutex::new(Some(binding.clone())),
    });
    #[cfg(any(test, feature = "test-seams"))]
    shared
        .counters
        .dials
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *slot.role.lock().unwrap() = Some(ClientRole::start_with_seams(
        shared.cfg.clone(),
        slot.clone(),
        shared.seams.client.clone(),
    ));
    slots.push(slot.clone());
    slot
}

/// Exactly one Origin header, or none: duplicate origin headers are a request
/// shape no browser produces, and picking either one would be a guess.
fn single_origin(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all("origin").iter();
    let first = values.next()?.to_str().ok()?.to_owned();
    values.next().is_none().then_some(first)
}

/// Parses a JSON object, rejecting duplicate keys and trailing data: a hello
/// that repeats `bridgeGrant` (or `type`) is refused outright instead of being
/// silently resolved to whichever value came last.
fn parse_object_strict(text: &str) -> Option<Fields> {
    struct Strict;
    impl<'de> serde::de::Visitor<'de> for Strict {
        type Value = Fields;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("one JSON object")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut access: A,
        ) -> Result<Self::Value, A::Error> {
            let mut fields = Fields::new();
            while let Some((key, value)) = access.next_entry::<String, Box<RawValue>>()? {
                if fields.contains_key(&key) {
                    return Err(serde::de::Error::custom("duplicate field"));
                }
                fields.insert(key, value);
            }
            Ok(fields)
        }
    }
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let fields = serde::Deserializer::deserialize_map(&mut deserializer, Strict).ok()?;
    deserializer.end().ok()?;
    Some(fields)
}

/// The admission gate. A correctly shaped secret plus exactly one Origin header
/// equal to the trusted binding's page origin, for a binding that is still
/// issuable and registered on this bridge instance. Proxy ownership is
/// registered before returning, so a racing revoke either finds this proxy or
/// rejects the admission — never an untracked authorized connection.
fn authorize_hello(
    fields: &Fields,
    origin: Option<&str>,
    shared: &Arc<Shared>,
) -> Option<(Arc<BindingRecord>, ProxyLease)> {
    let presented = string_field(fields, "bridgeGrant")?;
    if !grant_shape_ok(&presented) {
        return None;
    }
    let origin = PageOrigin::parse(origin?)?;
    shared.authorize(&presented, &origin)
}

async fn proxy(mut socket: WebSocket, origin: Option<String>, shared: Arc<Shared>) {
    let first = tokio::time::timeout(shared.seams.first_frame_timeout, socket.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = first else {
        log::warn!("bridge: no hello frame within the first-frame deadline");
        return;
    };
    let Some(fields) = parse_object_strict(&text) else {
        log::warn!("bridge: first frame is not a single JSON object");
        return;
    };
    if string_field(&fields, "type").as_deref() != Some("hello") {
        log::warn!("bridge: first frame is not hello");
        return;
    }
    // Authorization precedes every side effect: no credential read, no
    // pairing-state change, no rewrite, no slot attach and no relay dial.
    let Some((binding, lease)) = authorize_hello(&fields, origin.as_deref(), &shared) else {
        log::warn!("bridge: hello refused; origin and grant did not authorize");
        return;
    };
    let credential = {
        let mut auth = shared.auth.lock().unwrap();
        if matches!(
            *shared.status.borrow(),
            HostBridgeStatus::CredentialInvalid | HostBridgeStatus::PairingFailed
        ) {
            log::warn!(
                "bridge: refusing a session in terminal state {:?}",
                *shared.status.borrow()
            );
            return;
        }
        if matches!(auth.credential, Credential::Pairing { .. }) {
            // Reject, never queue: one host-level exchange and one app hello.
            if auth.pairing_busy || shared.pairing.is_none() {
                log::warn!(
                    "bridge: pairing rejected busy={} sink={}",
                    auth.pairing_busy,
                    shared.pairing.is_some()
                );
                return;
            }
            auth.pairing_busy = true;
        }
        #[cfg(any(test, feature = "test-seams"))]
        shared
            .counters
            .credential_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        auth.credential.clone()
    };
    let pairing = matches!(credential, Credential::Pairing { .. });
    #[cfg(any(test, feature = "test-seams"))]
    shared
        .counters
        .rewrites
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let hello = rewrite(fields, &credential);
    if hello.len() > WS_PHYSICAL_FRAME_BYTES {
        if pairing {
            shared.auth.lock().unwrap().pairing_busy = false;
        }
        return;
    }
    let (frames_tx, mut frames) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
    let (lost_tx, mut lost) = watch::channel(false);
    let slot = attach(&shared, &binding, frames_tx, lost_tx);
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
    let mut revoked = lease.revoked.clone();
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
        if is_ack {
            // Authenticated: the only transition that may report `Online`.
            shared.set_status(HostBridgeStatus::Online);
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
                // The pairing itself worked; this device's token just could not be
                // stored. Terminal: re-pairing is the only way to hold a token.
                shared.set_status(HostBridgeStatus::PairingFailed);
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
    // Wait for whichever end condition fires; only a transport loss keeps the
    // slot alive (see the keep decision below).
    let mut ended_by_revocation = false;
    let transport_dropped = tokio::select! {
        // A terminal error frame can arrive in the same poll as Offline. Consume
        // the queued explicit rejection before the transport-loss notification.
        biased;
        _ = session => false,
        _ = lost.changed() => true,
        _ = write => false,
        _ = shutdown.changed() => false,
        // Revocation signalled this proxy: dropping the session future cancels
        // dial, ack wait, pairing persistence and forwarding in place.
        _ = revoked.changed() => { ended_by_revocation = true; false },
    };
    if pairing {
        shared.auth.lock().unwrap().pairing_busy = false;
    }
    // A daemon session answers exactly one hello (F-02): a proxy that reconnects
    // onto a kept-alive session draws invalid_frame instead of the fresh hello_ack
    // and snapshot the frontend needs. So an ended proxy takes its relay session
    // with it. Only a transport drop keeps the slot, and only while its next
    // session is fresh (never helloed) — the still-attached proxy's hello rides
    // that session when the redial completes. Revoke visibility is unchanged:
    // the next hello re-checks the DB on whatever new session carries it.
    // Shutdown stops every slot from the bridge task above.
    let current = slot.ready.borrow().clone();
    let keep = !ended_by_revocation
        && transport_dropped
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
    binding.release_proxy(&lease.cancel);
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
                owner: Mutex::new(None),
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
