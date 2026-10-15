// ABOUTME: The app-side admin client: one loopback WS per attachment, id-matched requests, the
// ABOUTME: navigation mirror's read pump, and the app-surface pump (plan Task 5, rules R1/D5).

//! # AdminClient (plan Task 5)
//!
//! The app's single connection to the daemon's admin channel. One client per
//! attachment: [`AdminClient::connect`] performs the `admin_hello` handshake,
//! installs the `admin_welcome` full mirror snapshot, and starts a **dedicated
//! read pump** that is never blocked by dispatch:
//!
//! | inbound frame | what the client does |
//! | --- | --- |
//! | `admin_result` | settles the pending request **by id** (never by arrival order, with a 10s deadline) |
//! | `navigation_permit` | installs the permit in the mirror, then acks it immediately (`mirror_ack` with that epoch) |
//! | `permit_revoked` | applies it immediately for the current epoch (the permit is gone before it can be used); stale epochs are dropped |
//! | `owner_binding_changed` | updates the owner's committed origin/binding/generation (current epoch only) |
//! | `app_surface_request` | runs the op on the app surface (its own task) and answers with `surface_response`; progress goes out as `surface_progress` |
//! | `surface_progress` | logged: progress originates **app-side** in this phase (the app runs the updater), so the daemon→app direction has no producer yet |
//! | socket close / error | the mirror is **cleared** (fail-closed) and every pending request fails |
//!
//! Both attach paths use this client: the app points it at the daemon's own
//! admin listener over loopback, so the wire path is the only path (plan D2).
//! T12 deleted the in-process mode that used to point it at an in-process
//! listener.
//!
//! ## Failure shapes (implementation-time decisions, recorded)
//!
//! - A failed surface op answers `{"type":"surface_response","id":…,"ok":false,"result":{"error":…}}`:
//!   the protocol table's `result` is opaque JSON, so the message travels there
//!   instead of in a new frame field.
//! - Requests time out after 10s; a timeout removes the pending slot, so a late
//!   answer can never settle a finished call.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use picot_host::daemon::admin::{
    ManagerIdentity, ADMIN_CLOSE_BAD_TOKEN, ADMIN_CLOSE_LEASE_HELD, ADMIN_WS_PATH,
};
use picot_host::host_control::ProgressSink;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::navigation_mirror::{InstalledPermit, MirrorOwner, NavigationMirror};

/// How long one `admin_request` may take before the client gives up (plan Task
/// 5: "10s timeout, id-matched").
pub const ADMIN_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the handshake — connect, `admin_hello`, `admin_welcome` — may take.
pub const ADMIN_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Broadcast fan-out capacity for [`AdminClient::subscribe`] consumers.
const EVENT_CHANNEL_CAPACITY: usize = 64;

type AdminSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The read half of the admin socket: the read pump owns exactly this, and the
/// writer task owns the sink half (a slow write must never stall dispatch).
type AdminStream = futures_util::stream::SplitStream<AdminSocket>;

/// Something the daemon told the app that is not a reply to one of its requests.
///
/// The mirror already applied it — this is the app's chance to react (T7 uses it
/// for the label map, T9 for the restart transaction's re-snapshot).
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AdminEvent {
    /// A permit was installed (the ack went out with it).
    PermitInstalled {
        permit_id: String,
        owner: String,
        origin: String,
    },
    PermitRevoked {
        permit_id: String,
    },
    OwnerBindingChanged {
        owner: String,
        has_workspace: bool,
        generation: u64,
    },
    /// The admin channel dropped: the mirror has been cleared (fail-closed).
    Disconnected,
}

/// Executes one forwarded `app_surface_request` app-side (plan D5).
///
/// A trait, not a concrete call into the Tauri surface, so the client's protocol
/// tests run without an app process (`super::surface_ops::TauriSurfaceExecutor`
/// is the production implementation).
#[async_trait::async_trait]
pub trait SurfaceExecutor: Send + Sync {
    /// Run one op. `progress` forwards `surface_progress` frames to the requester.
    async fn execute(&self, op: &str, args: Value, progress: ProgressSink)
        -> Result<Value, String>;
}

/// One pending request's settlement slot: the pump fills it exactly once, by id.
type PendingRequests = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>>;

/// What `owner.create` produced: the identity the app injects into a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedOwner {
    pub owner: String,
    pub capability: String,
    pub label: String,
    /// The daemon-normalized origin this owner is committed to.
    pub current_origin: String,
    pub generation: u64,
}

/// The app's admin-channel client. Cheap to clone through an `Arc`.
pub struct AdminClient {
    admin_origin: String,
    mirror: Arc<NavigationMirror>,
    executor: Arc<dyn SurfaceExecutor>,
    outbound: mpsc::UnboundedSender<WsMessage>,
    pending: PendingRequests,
    events: broadcast::Sender<AdminEvent>,
    epoch: Mutex<Option<u64>>,
    next_id: AtomicU64,
    connected: AtomicBool,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl AdminClient {
    /// Connect, hand over the manager identity, and install the attach snapshot.
    ///
    /// Returns an error naming the reason for a refused handshake: `4401` (wrong
    /// token — the instance file belongs to another daemon) and `4409` (another
    /// live app holds the lease) stay distinguishable, because the caller's
    /// policy differs (retry the file vs. show the blocked-instance dialog).
    pub async fn connect(
        admin_origin: &str,
        token: &str,
        identity: &ManagerIdentity,
        executor: Arc<dyn SurfaceExecutor>,
        mirror: Arc<NavigationMirror>,
    ) -> Result<Arc<Self>, String> {
        let url = admin_ws_url(admin_origin);
        let socket = match tokio::time::timeout(
            ADMIN_CONNECT_TIMEOUT,
            tokio_tungstenite::connect_async(&url),
        )
        .await
        {
            Ok(Ok((socket, _response))) => socket,
            Ok(Err(error)) => return Err(format!("cannot connect to {url}: {error}")),
            Err(_) => return Err(format!("connecting to {url} timed out")),
        };
        let (mut sink, mut stream) = socket.split();
        let hello = json!({
            "type": "admin_hello",
            "adminToken": token,
            "role": "manager",
            "appVersion": identity.app_version,
            "buildId": identity.build_id,
            "pid": identity.pid,
        });
        if sink.send(WsMessage::Text(hello.to_string())).await.is_err() {
            return Err("cannot write the admin_hello frame".to_string());
        }
        let welcome = read_welcome(&mut stream).await?;
        let epoch = welcome["epoch"]
            .as_u64()
            .ok_or_else(|| "the daemon's admin_welcome carried no epoch".to_string())?;
        let owners: Vec<MirrorOwner> = serde_json::from_value(
            welcome
                .get("mirror")
                .and_then(|mirror| mirror.get("owners"))
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|error| format!("the admin_welcome mirror snapshot is malformed: {error}"))?;
        mirror.initialize(epoch, owners);

        let (outbound, mut outbound_rx) = mpsc::unbounded_channel::<WsMessage>();
        let (events, _receiver) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let client = Arc::new(Self {
            admin_origin: admin_origin.to_string(),
            mirror,
            executor,
            outbound: outbound.clone(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            events,
            epoch: Mutex::new(Some(epoch)),
            next_id: AtomicU64::new(1),
            connected: AtomicBool::new(true),
            reader: Mutex::new(None),
        });
        // One writer task per connection: the read pump never touches the sink,
        // so a slow write cannot stall frame dispatch (plan R1 timing rules).
        tokio::spawn(async move {
            while let Some(message) = outbound_rx.recv().await {
                let closing = matches!(message, WsMessage::Close(_));
                if sink.send(message).await.is_err() || closing {
                    break;
                }
            }
        });
        let pump = Arc::clone(&client);
        let handle = tokio::spawn(async move { pump.read_pump(stream).await });
        if let Ok(mut slot) = client.reader.lock() {
            *slot = Some(handle);
        }
        log::info!("[picot] attached to the Picot daemon admin channel (epoch {epoch})");
        Ok(client)
    }

    /// The admin origin this client is attached to.
    pub fn admin_origin(&self) -> &str {
        &self.admin_origin
    }

    /// The attachment epoch, or `None` once the channel dropped.
    pub fn epoch(&self) -> Option<u64> {
        *self.epoch.lock().expect("admin epoch lock")
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// The navigation mirror this attachment feeds.
    pub fn mirror(&self) -> &Arc<NavigationMirror> {
        &self.mirror
    }

    /// Mirror events, epoch-filtered: the mirror has already applied whatever
    /// arrives here.
    #[allow(dead_code)]
    pub fn subscribe(&self) -> broadcast::Receiver<AdminEvent> {
        self.events.subscribe()
    }

    /// One id-matched request with a 10s deadline.
    pub async fn request(&self, op: &str, args: Value) -> Result<Value, String> {
        if !self.is_connected() {
            return Err("the admin channel is not connected".to_string());
        }
        let id = format!("r{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .expect("admin pending lock")
            .insert(id.clone(), sender);
        let frame = json!({"type": "admin_request", "id": id, "op": op, "args": args});
        if let Err(error) = self.send_frame(frame) {
            self.pending.lock().expect("admin pending lock").remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(ADMIN_REQUEST_TIMEOUT, receiver).await {
            Ok(Ok(result)) => result,
            // The pump settled this slot as a failure (channel dropped).
            Ok(Err(_)) => {
                Err("the admin channel closed while the request was in flight".to_string())
            }
            Err(_) => {
                self.pending.lock().expect("admin pending lock").remove(&id);
                Err(format!(
                    "{op} did not answer within {}s",
                    ADMIN_REQUEST_TIMEOUT.as_secs()
                ))
            }
        }
    }

    /// Mint an owner capability and seed the mirror with the sealed row.
    ///
    /// `owner.create` is the only op whose *reply* is authorization state: the
    /// attach snapshot predates every later owner, so an owner created after the
    /// attach reaches the mirror only through this reply. The row is decoded from
    /// what the daemon returned (never from the arguments this app sent), which
    /// is why a landing window can load its origin without a permit while a
    /// caller-supplied origin cannot smuggle itself into the mirror.
    ///
    /// `canonical_cwd` is `None` for a **workspace** owner being rebuilt (the
    /// restart transaction, T9): the app knows only the `workspaceId` the daemon
    /// minted, so the daemon resolves the canonical root from its own workspace
    /// registry. The argument is omitted from the frame rather than sent empty.
    pub async fn create_owner(
        &self,
        label: &str,
        canonical_cwd: Option<&str>,
        port: u16,
        origin: &str,
        workspace_id: Option<&str>,
    ) -> Result<CreatedOwner, String> {
        let mut args = json!({
            "label": label,
            "port": port,
            "origin": origin,
            "workspaceId": workspace_id,
        });
        if let Some(cwd) = canonical_cwd {
            args["cwd"] = Value::String(cwd.to_string());
        }
        let reply = self.request("owner.create", args).await?;
        let field = |key: &str| -> Result<String, String> {
            reply
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("the daemon's owner.create reply has no {key}"))
        };
        let owner = field("owner")?;
        let capability = field("capability")?;
        let label = field("label")?;
        let current_origin = field("currentOrigin")?;
        let row = MirrorOwner {
            owner: owner.clone(),
            label: label.clone(),
            current_origin: current_origin.clone(),
            workspace_id: reply
                .get("workspaceId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            session_route: reply
                .get("sessionRoute")
                .and_then(Value::as_str)
                .map(str::to_owned),
            generation: reply
                .get("generation")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        };
        self.mirror.install_owner(row);
        Ok(CreatedOwner {
            owner,
            capability,
            label,
            current_origin,
            generation: reply
                .get("generation")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        })
    }

    /// Answer one forwarded surface op (the pump's counterpart in `TauriAppSurface`).
    pub fn send_surface_response(&self, id: &str, ok: bool, result: Value) -> bool {
        self.send_frame(json!({
            "type": "surface_response",
            "id": id,
            "ok": ok,
            "result": result,
        }))
        .is_ok()
    }

    /// Disconnect deliberately: close the socket and fail closed.
    ///
    /// The app's `RunEvent::Exit` uses this in external mode (plan Task 5 Step 3,
    /// D12): the daemon keeps running, and the app must not leave a live manager
    /// record behind — the socket close is what triggers the daemon's
    /// dead-attachment cleanup.
    pub fn disconnect(&self) {
        let _ = self.outbound.send(WsMessage::Close(None));
        if let Some(handle) = self.reader.lock().expect("admin reader lock").take() {
            handle.abort();
        }
        self.finish_disconnect();
    }

    fn send_frame(&self, value: Value) -> Result<(), String> {
        if !self.is_connected() {
            return Err("the admin channel is not connected".to_string());
        }
        self.outbound
            .send(WsMessage::Text(value.to_string()))
            .map_err(|_| "the admin channel writer is gone".to_string())
    }

    /// The read pump: never blocked by dispatch, and the only place inbound
    /// frames are interpreted (plan R1).
    async fn read_pump(&self, mut stream: AdminStream) {
        while let Some(message) = stream.next().await {
            let text = match message {
                Ok(WsMessage::Text(text)) => text,
                Ok(WsMessage::Close(frame)) => {
                    log::info!("[picot] the admin channel closed by the daemon: {frame:?}");
                    break;
                }
                Ok(_) => continue,
                Err(error) => {
                    log::warn!("[picot] the admin channel failed: {error}");
                    break;
                }
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                log::warn!("[picot] the daemon sent a malformed admin frame");
                continue;
            };
            self.handle_frame(value);
        }
        self.finish_disconnect();
    }

    fn handle_frame(&self, value: Value) {
        match value["type"].as_str() {
            Some("admin_result") => {
                let id = value["id"].as_str().unwrap_or_default().to_string();
                let result = if value["ok"] == Value::Bool(true) {
                    Ok(value["result"].clone())
                } else {
                    Err(value["error"]
                        .as_str()
                        .unwrap_or("the daemon refused the request")
                        .to_string())
                };
                if let Some(sender) = self.pending.lock().expect("admin pending lock").remove(&id) {
                    let _ = sender.send(result);
                }
            }
            Some("app_surface_request") => self.spawn_surface_op(value),
            Some("navigation_permit") => self.apply_navigation_permit(&value),
            Some("permit_revoked") => {
                let epoch = value["epoch"].as_u64().unwrap_or_default();
                let permit_id = value["permitId"].as_str().unwrap_or_default().to_string();
                if self.mirror.revoke_permit(epoch, &permit_id) {
                    let _ = self.events.send(AdminEvent::PermitRevoked { permit_id });
                }
            }
            Some("owner_binding_changed") => {
                let epoch = value["epoch"].as_u64().unwrap_or_default();
                let has_workspace = value["hasWorkspace"] == Value::Bool(true);
                // The frame is a full owner row (a superset of `MirrorOwner`),
                // so the mirror can decode it with the snapshot's own shape.
                match serde_json::from_value::<MirrorOwner>(value.clone()) {
                    Ok(owner) => {
                        let row = owner.clone();
                        if self.mirror.apply_owner_changed(epoch, owner, has_workspace) {
                            let _ = self.events.send(AdminEvent::OwnerBindingChanged {
                                owner: row.owner,
                                has_workspace,
                                generation: row.generation,
                            });
                        } else {
                            log::debug!(
                                "[picot] dropped a stale owner_binding_changed (epoch {epoch}, \
                                 current {:?})",
                                self.epoch()
                            );
                        }
                    }
                    Err(error) => {
                        log::warn!("[picot] malformed owner_binding_changed frame: {error}")
                    }
                }
            }
            // Progress originates app-side in this phase (the app runs the
            // updater): the daemon→app direction has no producer yet, so there
            // is nothing to route it to.
            Some("surface_progress") => {
                log::debug!("[picot] the daemon replayed a surface_progress frame");
            }
            Some("admin_welcome") => {
                log::warn!("[picot] the daemon re-sent admin_welcome on an established channel");
            }
            other => log::debug!("[picot] unknown admin frame: {other:?}"),
        }
    }

    /// Install a permit the daemon granted, then ack it (plan R1).
    ///
    /// The order matters: the ack travels after the mirror holds the permit, so
    /// the daemon only completes a granting op whose permit the app can actually
    /// honor. A stale-epoch permit is **not** acknowledged, which leaves the
    /// granting op to fail safe on its own deadline.
    fn apply_navigation_permit(&self, value: &Value) {
        let epoch = value["epoch"].as_u64().unwrap_or_default();
        let permit = InstalledPermit {
            permit_id: value["permitId"].as_str().unwrap_or_default().to_string(),
            owner: value["owner"].as_str().unwrap_or_default().to_string(),
            origin: value["origin"].as_str().unwrap_or_default().to_string(),
            expires_at_ms: value["expiresAtMs"].as_u64().unwrap_or_default(),
            generation: value["generation"].as_u64().unwrap_or_default(),
        };
        if !self.mirror.install_permit(epoch, permit.clone()) {
            log::warn!(
                "[picot] dropped a stale navigation permit {} (epoch {epoch}, current {:?})",
                permit.permit_id,
                self.epoch()
            );
            return;
        }
        let _ = self.send_frame(json!({
            "type": "mirror_ack",
            "permitId": permit.permit_id,
            "epoch": epoch,
        }));
        let _ = self.events.send(AdminEvent::PermitInstalled {
            permit_id: permit.permit_id,
            owner: permit.owner,
            origin: permit.origin,
        });
    }

    /// Run one forwarded op on its own task and answer it (plan D5).
    fn spawn_surface_op(&self, value: Value) {
        let id = value["id"].as_str().unwrap_or_default().to_string();
        let op = value["op"].as_str().unwrap_or_default().to_string();
        let args = value["args"].clone();
        let executor = Arc::clone(&self.executor);
        let outbound = self.outbound.clone();
        tokio::spawn(async move {
            let progress: ProgressSink = {
                let outbound = outbound.clone();
                let ack_id = id.clone();
                Arc::new(move |event: Value| {
                    let _ = outbound.send(WsMessage::Text(
                        json!({"type": "surface_progress", "ackId": ack_id, "event": event})
                            .to_string(),
                    ));
                })
            };
            let frame = match executor.execute(&op, args, progress).await {
                Ok(result) => json!({
                    "type": "surface_response",
                    "id": id,
                    "ok": true,
                    "result": result,
                }),
                Err(error) => json!({
                    "type": "surface_response",
                    "id": id,
                    "ok": false,
                    "result": {"error": error},
                }),
            };
            let _ = outbound.send(WsMessage::Text(frame.to_string()));
        });
    }

    /// Fail closed: the mirror is cleared, every pending request fails, and
    /// subscribers learn the attachment is gone. Idempotent.
    fn finish_disconnect(&self) {
        if !self.connected.swap(false, Ordering::SeqCst) {
            return;
        }
        self.mirror.clear();
        if let Ok(mut epoch) = self.epoch.lock() {
            *epoch = None;
        }
        let pending: Vec<_> = self
            .pending
            .lock()
            .expect("admin pending lock")
            .drain()
            .map(|(_, sender)| sender)
            .collect();
        for sender in pending {
            let _ = sender.send(Err("the admin channel closed".to_string()));
        }
        let _ = self.events.send(AdminEvent::Disconnected);
        log::info!("[picot] the admin channel dropped; the navigation mirror is cleared");
    }
}

impl Drop for AdminClient {
    fn drop(&mut self) {
        self.finish_disconnect();
    }
}

/// `ws://host:port/admin` for an `http://host:port` admin origin.
pub fn admin_ws_url(admin_origin: &str) -> String {
    format!(
        "{}{}",
        admin_origin.replacen("http://", "ws://", 1),
        ADMIN_WS_PATH
    )
}

/// Wait for the handshake answer, mapping the protocol close codes onto the
/// reasons the caller's policy distinguishes.
async fn read_welcome(stream: &mut AdminStream) -> Result<Value, String> {
    let deadline = tokio::time::sleep(ADMIN_CONNECT_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        let message = tokio::select! {
            () = &mut deadline => return Err("the daemon did not answer the admin handshake".to_string()),
            message = stream.next() => message,
        };
        match message {
            Some(Ok(WsMessage::Text(text))) => {
                let value: Value = serde_json::from_str(&text).map_err(|error| {
                    format!("malformed admin frame during the handshake: {error}")
                })?;
                if value["type"] == "admin_welcome" {
                    return Ok(value);
                }
            }
            Some(Ok(WsMessage::Close(frame))) => {
                let code = frame
                    .as_ref()
                    .map(|frame| u16::from(frame.code))
                    .unwrap_or(0);
                let reason = frame
                    .as_ref()
                    .map(|frame| frame.reason.to_string())
                    .unwrap_or_default();
                return Err(match code {
                    ADMIN_CLOSE_BAD_TOKEN => format!(
                        "the instance file's admin token was rejected ({reason}); it belongs to \
                         a different daemon"
                    ),
                    ADMIN_CLOSE_LEASE_HELD => {
                        format!("another live Picot app already manages this daemon ({reason})")
                    }
                    other => format!("the admin channel closed with {other}: {reason}"),
                });
            }
            Some(Ok(_)) => continue,
            Some(Err(error)) => return Err(format!("the admin channel failed: {error}")),
            None => {
                return Err("the daemon closed the admin channel during the handshake".to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_client::surface_ops::ForwardedSurfaceOps;
    use picot_host::daemon::admin::{AdminConfig, AdminServer};
    use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
    use picot_host::daemon::surface_forward::{wire, AdminForwardingSurface};
    use picot_host::daemon::{DaemonConfig, DaemonState};
    use picot_host::host_server::PaneRect;
    use picot_host::runtime_coordinator::RuntimeTarget;
    use picot_host::window_owner::{OwnerId, OwnerMirrorEvent, TemporaryKind};
    use std::path::PathBuf;

    /// A real daemon: the host stack plus its loopback admin listener. Nothing
    /// here is mocked — the client under test talks to the production server
    /// over a real TCP socket (engineering-lesson #1 cross-check).
    struct Fixture {
        _temp: tempfile::TempDir,
        state: Arc<DaemonState>,
        admin: AdminServer,
        token: String,
        surface: Arc<AdminForwardingSurface>,
    }

    async fn boot() -> Fixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).expect("static dir");
        std::fs::write(static_dir.join("index.html"), "<h1>client</h1>").expect("index");
        let surface = Arc::new(AdminForwardingSurface::new());
        let state = Arc::new(
            DaemonState::start(
                DaemonConfig {
                    data_dir: temp.path().join("data"),
                    config_dir: temp.path().join("config"),
                    static_dir,
                    bundle_identifier: "com.palandata.picot.test".to_string(),
                },
                surface.clone(),
            )
            .await
            .expect("the daemon host stack boots"),
        );
        let token = "test-admin-token-0123456789abcdef".to_string();
        let admin = AdminServer::start(
            AdminConfig::new(token.clone())
                .with_mirror_ack_timeout(Duration::from_millis(500))
                .with_surface_forward_timeout(Duration::from_millis(500)),
        )
        .await
        .expect("the admin listener binds");
        state.install_admin(admin.clone());
        surface.attach(admin.clone());
        // The data plane's dispatcher (and, with it, the review-B1 pane
        // forwarder) is installed exactly as the daemon binary installs it, so
        // a data-plane op in these tests travels the production path.
        state.install_control_handler();
        Fixture {
            _temp: temp,
            state,
            admin,
            token,
            surface,
        }
    }

    fn identity() -> ManagerIdentity {
        ManagerIdentity {
            pid: Some(std::process::id()),
            app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            build_id: Some(picot_host::BUILD_ID.to_string()),
        }
    }

    /// A recording executor: the app-side half of the surface pump.
    #[derive(Default)]
    struct RecordingExecutor {
        calls: Mutex<Vec<(String, Value)>>,
        /// Ops this executor answers with a failure.
        fail: Vec<&'static str>,
    }

    #[async_trait::async_trait]
    impl SurfaceExecutor for RecordingExecutor {
        async fn execute(
            &self,
            op: &str,
            args: Value,
            progress: ProgressSink,
        ) -> Result<Value, String> {
            self.calls.lock().unwrap().push((op.to_string(), args));
            if self.fail.contains(&op) {
                return Err("the updater is not configured".to_string());
            }
            match op {
                wire::OP_PICK_FOLDER => Ok(Value::from("/tmp/picked")),
                wire::OP_DOWNLOAD_AND_INSTALL_UPDATE => {
                    progress(json!({"percent": 1}));
                    progress(json!({"percent": 2}));
                    Ok(json!({"installed": true}))
                }
                _ => Ok(Value::Null),
            }
        }
    }

    async fn attach(
        fixture: &Fixture,
        executor: Arc<dyn SurfaceExecutor>,
    ) -> (Arc<AdminClient>, Arc<NavigationMirror>) {
        let mirror = Arc::new(NavigationMirror::new());
        let client = AdminClient::connect(
            fixture.admin.origin(),
            &fixture.token,
            &identity(),
            executor,
            Arc::clone(&mirror),
        )
        .await
        .expect("the app attaches to the real daemon");
        (client, mirror)
    }

    fn url(value: &str) -> tauri::Url {
        value.parse().expect("a test url")
    }

    fn owner_args(label: &str, origin: &str) -> Value {
        json!({
            "label": label,
            "cwd": "/workspace",
            "port": origin.rsplit(':').next().unwrap().parse::<u16>().unwrap(),
            "origin": origin,
        })
    }

    async fn wait_until(what: &str, mut predicate: impl FnMut() -> bool) {
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_welcome_snapshot_initializes_the_mirror() {
        let fixture = boot().await;
        // An owner that exists *before* the attach: the reattach case.
        let (owner, _capability) = fixture
            .state
            .owner_registry
            .create_owner_with_workspace(
                "native-workspace-w1".to_string(),
                PathBuf::from("/workspace"),
                5001,
                "http://127.0.0.1:5001".to_string(),
                Some("w1".to_string()),
                TemporaryKind::DefaultStartup,
            )
            .expect("owner");
        let (_client, mirror) = attach(&fixture, Arc::new(RecordingExecutor::default())).await;
        let owner = owner.as_str();

        assert!(mirror.authorize(owner, &url("http://127.0.0.1:5001/")));
        assert!(mirror.authorize(
            owner,
            &url("http://127.0.0.1:5001/workspaces/w1/sessions/s1")
        ));
        assert!(
            !mirror.authorize(owner, &url("http://127.0.0.1:5002/")),
            "a different port is a different origin"
        );
        assert_eq!(
            mirror.owner_for_label("native-workspace-w1").as_deref(),
            Some(owner)
        );
        assert_eq!(mirror.owner_has_workspace(owner), Some(true));
        assert!(mirror.epoch().is_some());
        fixture.state.shutdown_graceful("test").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn owner_create_reply_seeds_the_mirror_and_the_permit_round_trip_acks() {
        let fixture = boot().await;
        let (client, mirror) = attach(&fixture, Arc::new(RecordingExecutor::default())).await;
        let created = client
            .create_owner(
                "native-landing",
                Some("/workspace"),
                5001,
                "http://127.0.0.1:5001",
                None,
            )
            .await
            .expect("owner.create succeeds over the real channel");
        let owner = created.owner.clone();
        assert!(!created.capability.is_empty());
        assert_eq!(
            created.current_origin, "http://127.0.0.1:5001",
            "the reply carries the sealed row: {created:?}"
        );
        assert_eq!(created.label, "native-landing");

        // The landing window's first load must be authorized without a permit:
        // the mirror learned the owner from the reply.
        assert!(
            mirror.authorize(&owner, &url("http://127.0.0.1:5001/")),
            "the owner.create reply must seed the mirror"
        );
        assert_eq!(
            mirror.owner_for_label("native-landing").as_deref(),
            Some(owner.as_str())
        );

        // A prepared navigation is pushed as a permit; the client installs it and
        // acks, which is what lets the granting op complete.
        let state = Arc::clone(&fixture.state);
        let op_owner = OwnerId::from_string(owner.clone());
        let op = tokio::spawn(async move {
            state
                .prepare_navigation_with_mirror_ack(
                    &op_owner,
                    5002,
                    PathBuf::from("/workspace"),
                    "http://127.0.0.1:5002/workspaces/w1/sessions/s1".to_string(),
                    Duration::from_secs(30),
                )
                .await
        });
        let generation = tokio::time::timeout(Duration::from_secs(10), op)
            .await
            .expect("the granting op settles")
            .expect("no panic")
            .expect("the client's mirror_ack completes the op");
        assert!(generation > 0);
        // The permit is installed and single-use: the navigation it allows is
        // the navigation that consumes it.
        assert!(mirror.authorize(&owner, &url("http://127.0.0.1:5002/x")));
        assert!(
            !mirror.authorize(&owner, &url("http://127.0.0.1:5002/again")),
            "a permit is one-shot"
        );
        fixture.state.shutdown_graceful("test").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revocation_without_an_epoch_change_denies_the_permitted_navigation() {
        let fixture = boot().await;
        let (client, mirror) = attach(&fixture, Arc::new(RecordingExecutor::default())).await;
        let created = client
            .create_owner(
                "w1",
                Some("/workspace"),
                5001,
                "http://127.0.0.1:5001",
                None,
            )
            .await
            .expect("owner.create");
        let owner = OwnerId::from_string(created.owner);
        let epoch = mirror.epoch().expect("attached");

        let state = Arc::clone(&fixture.state);
        let op_owner = owner.clone();
        let op = tokio::spawn(async move {
            state
                .prepare_navigation_with_mirror_ack(
                    &op_owner,
                    5002,
                    PathBuf::from("/workspace"),
                    "http://127.0.0.1:5002/workspaces/w1/sessions/s1".to_string(),
                    Duration::from_secs(30),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), op)
            .await
            .expect("the granting op settles")
            .expect("no panic")
            .expect("acked");
        assert_eq!(mirror.permit_count(), 1, "the permit is installed");

        // The daemon cancels the pending permit (a transition cancel): the
        // `permit_revoked` event carries the *same* epoch, so the client must
        // apply it immediately.
        fixture
            .state
            .owner_registry
            .cancel_pending_permit(&owner)
            .expect("a pending permit to cancel");
        wait_until("the mirror to drop the revoked permit", || {
            mirror.permit_count() == 0
        })
        .await;
        assert_eq!(mirror.epoch(), Some(epoch), "no epoch change was involved");
        assert!(
            !mirror.authorize(owner.as_str(), &url("http://127.0.0.1:5002/x")),
            "the revoked permit must not authorize the navigation it would have allowed"
        );
        assert!(
            mirror.authorize(owner.as_str(), &url("http://127.0.0.1:5001/")),
            "the owner's committed origin is untouched by the revocation"
        );
        fixture.state.shutdown_graceful("test").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_binding_change_moves_the_authorized_origin() {
        let fixture = boot().await;
        let (client, mirror) = attach(&fixture, Arc::new(RecordingExecutor::default())).await;
        let created = client
            .create_owner(
                "native-landing",
                Some("/workspace"),
                5001,
                "http://127.0.0.1:5001",
                None,
            )
            .await
            .expect("owner.create");
        let owner = OwnerId::from_string(created.owner);

        let (generation, _permit) = fixture
            .state
            .owner_registry
            .prepare_navigation_with_permit(
                &owner,
                5002,
                PathBuf::from("/workspace"),
                "http://127.0.0.1:5002/workspaces/w1/sessions/s1".to_string(),
                Duration::from_secs(30),
            )
            .expect("permit");
        // Consume the permit the way the WebView's navigation would, then commit.
        wait_until("the mirror to install the pushed permit", || {
            mirror.permit_count() == 1
        })
        .await;
        assert!(mirror.authorize(owner.as_str(), &url("http://127.0.0.1:5002/x")));
        fixture
            .state
            .owner_registry
            .commit_workspace_transition_with_workspace(
                &owner,
                generation,
                "http://127.0.0.1:5002/workspaces/w1/sessions/s1".to_string(),
                Some("w1".to_string()),
                TemporaryKind::DefaultStartup,
            )
            .expect("commit");
        wait_until("the mirror to learn the committed origin", || {
            mirror.authorize(owner.as_str(), &url("http://127.0.0.1:5002/reload"))
        })
        .await;
        assert!(
            !mirror.authorize(owner.as_str(), &url("http://127.0.0.1:5001/")),
            "the previous origin is no longer the owner's committed origin"
        );
        assert_eq!(mirror.owner_has_workspace(owner.as_str()), Some(true));
        fixture.state.shutdown_graceful("test").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stale_epoch_events_are_dropped_after_a_reattach() {
        let fixture = boot().await;
        let (first, first_mirror) = attach(&fixture, Arc::new(RecordingExecutor::default())).await;
        let epoch_one = first_mirror.epoch().expect("attached");
        let created = first
            .create_owner(
                "w1",
                Some("/workspace"),
                5001,
                "http://127.0.0.1:5001",
                None,
            )
            .await
            .expect("owner.create");
        let owner = created.owner;

        // Simulate a reattach: a fresh attachment slot and a second client. The
        // daemon increments the epoch on the new grant.
        fixture.admin.clear_bookkeeping();
        wait_until("the lease to be free", || !fixture.admin.has_manager()).await;
        let (_second, second_mirror) =
            attach(&fixture, Arc::new(RecordingExecutor::default())).await;
        let epoch_two = second_mirror.epoch().expect("attached");
        assert_eq!(
            epoch_two,
            epoch_one + 1,
            "the attach transaction increments the attachment epoch"
        );

        // A permit from the previous attachment must be dropped, not installed.
        fixture
            .admin
            .push_owner_event(OwnerMirrorEvent::NavigationPermit {
                epoch: epoch_one,
                permit_id: "stale-permit".to_string(),
                owner: owner.clone(),
                origin: "http://127.0.0.1:5002".to_string(),
                expires_at_ms: u64::MAX,
                generation: 1,
            });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(second_mirror.permit_count(), 0, "a stale permit is dropped");
        assert!(!second_mirror.authorize(&owner, &url("http://127.0.0.1:5002/x")));

        // The current epoch's event applies.
        fixture
            .admin
            .push_owner_event(OwnerMirrorEvent::NavigationPermit {
                epoch: epoch_two,
                permit_id: "current-permit".to_string(),
                owner: owner.clone(),
                origin: "http://127.0.0.1:5002".to_string(),
                expires_at_ms: u64::MAX,
                generation: 1,
            });
        wait_until("the mirror to install the current-epoch permit", || {
            second_mirror.permit_count() == 1
        })
        .await;
        assert!(second_mirror.authorize(&owner, &url("http://127.0.0.1:5002/x")));
        fixture.state.shutdown_graceful("test").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delayed_responses_are_matched_by_id_not_by_arrival_order() {
        let fixture = boot().await;
        fixture
            .admin
            .set_op_delay_for_tests("owner.create", Duration::from_millis(400));
        let (client, _mirror) = attach(&fixture, Arc::new(RecordingExecutor::default())).await;

        let slow = client.request("owner.create", owner_args("w1", "http://127.0.0.1:5001"));
        let fast = client.request("daemon.status", json!({}));
        let (slow, fast) = tokio::join!(slow, fast);
        let slow = slow.expect("the slow op answers its own id");
        let fast = fast.expect("the fast op answers its own id");
        assert!(
            slow["capability"].is_string(),
            "the delayed answer is owner.create's: {slow}"
        );
        assert_eq!(
            fast["pid"].as_u64(),
            Some(u64::from(std::process::id())),
            "the fast answer is daemon.status's: {fast}"
        );
        assert!(fast["capability"].is_null());
        // The reader pump was never blocked: a third request still works.
        fixture
            .admin
            .set_op_delay_for_tests("owner.create", Duration::ZERO);
        let status = client
            .request("daemon.status", json!({}))
            .await
            .expect("a control op while nothing is in flight");
        assert_eq!(status["epoch"].as_u64(), Some(1));
        fixture.state.shutdown_graceful("test").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_channel_clears_the_mirror_fail_closed() {
        let fixture = boot().await;
        let (client, mirror) = attach(&fixture, Arc::new(RecordingExecutor::default())).await;
        let created = client
            .create_owner(
                "w1",
                Some("/workspace"),
                5001,
                "http://127.0.0.1:5001",
                None,
            )
            .await
            .expect("owner.create");
        let owner = created.owner;
        assert!(mirror.authorize(&owner, &url("http://127.0.0.1:5001/")));

        // The OS closes the socket (app death, daemon drain — both look the same
        // to the client).
        fixture.admin.close_connections();
        wait_until("the mirror to fail closed", || mirror.epoch().is_none()).await;

        assert!(!mirror.authorize(&owner, &url("http://127.0.0.1:5001/")));
        assert!(!mirror.authorize(&owner, &url("http://127.0.0.1:5002/x")));
        // The one exception survives: the opaque about: documents.
        assert!(mirror.authorize(&owner, &url("about:srcdoc")));
        assert!(mirror.authorize(&owner, &url("about:blank")));
        assert!(!mirror.authorize(&owner, &url("about:config")));
        assert!(mirror.owner_for_label("w1").is_none());

        // A request after the drop fails instead of hanging on the deadline.
        let failure = client
            .request("daemon.status", json!({}))
            .await
            .expect_err("a dead channel cannot serve a request");
        assert!(failure.contains("not connected"), "{failure}");
        assert!(!client.is_connected());
        fixture.state.shutdown_graceful("test").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forwarded_ops_are_executed_by_the_pump_and_progress_flows_back() {
        let fixture = boot().await;
        let executor = Arc::new(RecordingExecutor {
            calls: Mutex::new(Vec::new()),
            fail: vec![wire::OP_CHECK_FOR_UPDATE],
        });
        let (client, _mirror) =
            attach(&fixture, Arc::clone(&executor) as Arc<dyn SurfaceExecutor>).await;

        // The daemon forwards the op; the app executes it on this surface.
        let picked = fixture
            .surface
            .pick_folder()
            .await
            .expect("the app's answer completes the forwarded op");
        assert_eq!(picked.as_deref(), Some("/tmp/picked"));

        let failure = fixture
            .surface
            .check_for_update()
            .await
            .expect_err("ok=false must fail the requester");
        assert!(
            failure.contains("the updater is not configured"),
            "{failure}"
        );

        // Streaming: the app's progress frames must reach the daemon-side sink.
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink: ProgressSink = {
            let seen = Arc::clone(&seen);
            Arc::new(move |event: Value| seen.lock().unwrap().push(event))
        };
        fixture
            .surface
            .download_and_install_update(sink)
            .await
            .expect("the streaming op completes");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![json!({"percent": 1}), json!({"percent": 2})],
            "the app's surface_progress frames must reach the requester"
        );

        let calls = executor.calls.lock().unwrap().clone();
        assert_eq!(
            calls.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>(),
            vec![
                wire::OP_PICK_FOLDER,
                wire::OP_CHECK_FOR_UPDATE,
                wire::OP_DOWNLOAD_AND_INSTALL_UPDATE
            ]
        );
        assert_eq!(calls[0].1, json!({}));
        let _ = client;
        fixture.state.shutdown_graceful("test").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_token_is_refused_with_the_instances_own_reason() {
        let fixture = boot().await;
        let mirror = Arc::new(NavigationMirror::new());
        let failure = AdminClient::connect(
            fixture.admin.origin(),
            "not-the-token",
            &identity(),
            Arc::new(RecordingExecutor::default()),
            mirror,
        )
        .await
        .map(|_| ())
        .expect_err("a wrong token must be refused");
        assert!(failure.contains("token"), "{failure}");
        fixture.state.shutdown_graceful("test").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disconnect_releases_the_lease_for_the_next_attachment() {
        let fixture = boot().await;
        let (client, mirror) = attach(&fixture, Arc::new(RecordingExecutor::default())).await;
        assert!(fixture.admin.has_manager());
        client.disconnect();
        assert!(!client.is_connected());
        assert_eq!(mirror.epoch(), None);
        wait_until("the daemon to notice the dropped attachment", || {
            !fixture.admin.has_manager()
        })
        .await;
        fixture.state.shutdown_graceful("test").await;
    }

    // ---- Review B1: the browser-pane data-plane ops must reach the app ----

    /// The app-side half of the pane path: records what the forwarded `pane`
    /// op decoded to and answers a recognizable value per op. It is the only
    /// stand-in in these tests — a real `AppHandle` + `BrowserPaneRuntime`
    /// cannot be constructed headless, but every hop up to this surface is the
    /// production one.
    #[derive(Default)]
    struct RecordingPaneSurface {
        panes: Mutex<Vec<PaneOp>>,
    }

    impl RecordingPaneSurface {
        fn panes(&self) -> Vec<PaneOp> {
            self.panes.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl AppSurface for RecordingPaneSurface {
        async fn open_workspace_window(
            &self,
            _host_origin: &str,
            _target: &RuntimeTarget,
            _owner: &str,
            _capability: &str,
        ) -> Result<(), String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn open_devtools(&self, _label: &str) -> Result<(), String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn menu_state_changed(&self) -> Result<(), String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn relaunch_app(&self) -> Result<(), String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn pane(&self, op: PaneOp) -> Result<Value, String> {
            self.panes.lock().unwrap().push(op.clone());
            Ok(match op {
                PaneOp::EvalJson { .. } => Value::String("42".to_string()),
                PaneOp::Url { .. } => Value::String("https://example.com/".to_string()),
                _ => Value::Null,
            })
        }

        async fn pick_folder(&self) -> Result<Option<String>, String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn save_session_export(
            &self,
            _host_origin: &str,
            _export_url: &str,
            _file_name: &str,
            _initial_dir: Option<String>,
        ) -> Result<Value, String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn check_for_update(&self) -> Result<Value, String> {
            Err("this test only forwards pane ops".to_string())
        }

        async fn download_and_install_update(
            &self,
            _progress: SurfaceProgress,
        ) -> Result<Value, String> {
            Err("this test only forwards pane ops".to_string())
        }
    }

    type HostClient = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn next_json(socket: &mut HostClient) -> Value {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(15), socket.next())
                .await
                .expect("a frame within the deadline")
                .expect("the socket is open")
                .expect("a valid frame");
            match message {
                WsMessage::Text(text) => return serde_json::from_str(&text).expect("JSON frame"),
                WsMessage::Close(frame) => panic!("the host socket closed: {frame:?}"),
                _ => continue,
            }
        }
    }

    /// Read until the frame that carries `request_id` arrives; the host greets
    /// a desktop client with a bootstrap frame first, which this skips.
    async fn frame_for(socket: &mut HostClient, request_id: &str) -> Value {
        for _ in 0..32 {
            let frame = next_json(socket).await;
            if frame.get("requestId").and_then(Value::as_str) == Some(request_id) {
                return frame;
            }
        }
        panic!("no frame for {request_id} on the data plane");
    }

    /// Attach a real desktop data-plane socket to the live host: the frontend's
    /// own half of the chain (owner capability, `hello`, `/v2/ws`).
    async fn attach_desktop(fixture: &Fixture, label: &str) -> HostClient {
        let (_owner, capability) = fixture
            .state
            .owner_registry
            .create_owner(
                label.to_string(),
                PathBuf::from("/workspace"),
                5007,
                "http://127.0.0.1:5007".to_string(),
            )
            .expect("owner");
        let ws_url = fixture.state.origin().replace("http://", "ws://") + "/v2/ws";
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .expect("a real desktop socket connects");
        socket
            .send(WsMessage::Text(
                json!({
                    "type": "hello",
                    "protocolVersion": 2,
                    "clientType": "desktop",
                    "clientId": format!("pane-client-{label}"),
                    "desktopCapability": capability,
                })
                .to_string(),
            ))
            .await
            .expect("hello sent");
        let ack = next_json(&mut socket).await;
        assert_eq!(ack["type"], "hello_ack");
        socket
    }

    /// Review B1, end to end over real sockets: a frontend
    /// `browser_pane_create` on the host data plane must reach the app as the
    /// forwarded `pane` op and come back as that op's answer. Before the fix the
    /// daemon answered `unavailable` here — nothing ever installed the host's
    /// pane slot and the browser pane tabs were a dead end in external mode.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_browser_pane_op_reaches_the_app_and_its_answer_comes_back() {
        let fixture = boot().await;
        // App side: the production pump decode (`ForwardedSurfaceOps`) behind
        // the production `AdminClient` on a real admin socket.
        let recording = Arc::new(RecordingPaneSurface::default());
        let (_client, _mirror) = attach(
            &fixture,
            Arc::new(ForwardedSurfaceOps::new(recording.clone())),
        )
        .await;

        // Frontend side: a real desktop data-plane socket on the host.
        let mut socket = attach_desktop(&fixture, "native-pane").await;
        socket
            .send(WsMessage::Text(
                json!({
                    "type": "data_request",
                    "protocolVersion": 2,
                    "requestId": "pane-create-1",
                    "operation": "browser_pane_create",
                    "paneId": "native-pane:tab-1",
                    "windowLabel": "native-pane",
                    "url": "https://example.com/",
                    "x": 1.0,
                    "y": 2.0,
                    "width": 3.0,
                    "height": 4.0,
                })
                .to_string(),
            ))
            .await
            .expect("pane create sent");

        let response = frame_for(&mut socket, "pane-create-1").await;
        assert_eq!(response["type"], "data_response", "{response}");
        assert_eq!(response["operation"], "browser_pane_create");
        assert!(
            response.get("error").is_none(),
            "the create must not fail: {response}"
        );

        // The app received exactly the op the data frame described.
        assert_eq!(
            recording.panes(),
            vec![PaneOp::Create {
                pane_id: "native-pane:tab-1".to_string(),
                window_label: "native-pane".to_string(),
                url: "https://example.com/".to_string(),
                rect: PaneRect {
                    x: 1.0,
                    y: 2.0,
                    width: 3.0,
                    height: 4.0,
                },
            }],
            "the create must survive the round trip with its exact shape"
        );

        // The answer travels back too: eval carries the app surface's value.
        socket
            .send(WsMessage::Text(
                json!({
                    "type": "data_request",
                    "protocolVersion": 2,
                    "requestId": "pane-eval-1",
                    "operation": "browser_pane_eval",
                    "paneId": "native-pane:tab-1",
                    "js": "document.title",
                })
                .to_string(),
            ))
            .await
            .expect("pane eval sent");
        let eval = frame_for(&mut socket, "pane-eval-1").await;
        assert_eq!(eval["type"], "data_response", "{eval}");
        assert_eq!(eval["operation"], "browser_pane_eval");
        assert_eq!(
            eval["result"], "42",
            "the app's value must come back: {eval}"
        );

        fixture.state.shutdown_graceful("test").await;
    }

    /// A recording in-process [`picot_host::host_server::PaneSurface`]: the
    /// host's own WebView runtime (the app package's `BrowserPaneRuntime`),
    /// which keeps precedence over forwarding whenever it is installed.
    #[derive(Default)]
    struct RecordingLocalPane {
        creates: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl picot_host::host_server::PaneSurface for RecordingLocalPane {
        fn create(
            &self,
            pane_id: &str,
            _window_label: &str,
            _url: &str,
            _rect: PaneRect,
        ) -> Result<(), String> {
            self.creates.lock().unwrap().push(pane_id.to_string());
            Ok(())
        }

        fn set_rect(&self, _pane_id: &str, _rect: PaneRect) -> Result<(), String> {
            Ok(())
        }

        fn set_visible(&self, _pane_id: &str, _visible: bool) -> Result<(), String> {
            Ok(())
        }

        async fn eval_json(&self, _pane_id: &str, _expression: &str) -> Result<String, String> {
            Ok("local".to_string())
        }

        fn navigate(&self, _pane_id: &str, _url: &str) -> Result<(), String> {
            Ok(())
        }

        fn url(&self, _pane_id: &str) -> Result<String, String> {
            Ok("https://local/".to_string())
        }

        fn destroy(&self, _pane_id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// The review-B1 fallback is a fallback: a host that owns an in-process pane
    /// runtime keeps serving the op itself and never asks the app.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_in_process_pane_runtime_wins_over_forwarding() {
        let fixture = boot().await;
        let recording = Arc::new(RecordingPaneSurface::default());
        let (_client, _mirror) = attach(
            &fixture,
            Arc::new(ForwardedSurfaceOps::new(recording.clone())),
        )
        .await;
        let local = Arc::new(RecordingLocalPane::default());
        fixture.state.host.set_browser_panes(local.clone());

        let mut socket = attach_desktop(&fixture, "native-pane-local").await;
        socket
            .send(WsMessage::Text(
                json!({
                    "type": "data_request",
                    "protocolVersion": 2,
                    "requestId": "pane-create-local",
                    "operation": "browser_pane_create",
                    "paneId": "native-pane-local:tab-1",
                    "windowLabel": "native-pane-local",
                    "url": "https://example.com/",
                    "x": 1.0,
                    "y": 2.0,
                    "width": 3.0,
                    "height": 4.0,
                })
                .to_string(),
            ))
            .await
            .expect("pane create sent");

        let response = frame_for(&mut socket, "pane-create-local").await;
        assert_eq!(response["type"], "data_response", "{response}");
        assert_eq!(
            local.creates.lock().unwrap().as_slice(),
            ["native-pane-local:tab-1"],
            "the in-process runtime must serve the op"
        );
        assert!(
            recording.panes().is_empty(),
            "an in-process runtime means the app is never asked: {:?}",
            recording.panes()
        );
        fixture.state.shutdown_graceful("test").await;
    }

    /// The fail-closed edge of the same wiring: with no app attached the
    /// forwarded op cannot be served, and the data plane must say so from the
    /// typed forwarding error instead of hanging or pretending success.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_browser_pane_op_with_no_app_attached_fails_closed() {
        let fixture = boot().await;
        let mut socket = attach_desktop(&fixture, "native-pane").await;
        socket
            .send(WsMessage::Text(
                json!({
                    "type": "data_request",
                    "protocolVersion": 2,
                    "requestId": "pane-create-no-app",
                    "operation": "browser_pane_create",
                    "paneId": "native-pane:tab-1",
                    "windowLabel": "native-pane",
                    "url": "https://example.com/",
                    "x": 1.0,
                    "y": 2.0,
                    "width": 3.0,
                    "height": 4.0,
                })
                .to_string(),
            ))
            .await
            .expect("pane create sent");

        let response = frame_for(&mut socket, "pane-create-no-app").await;
        assert_eq!(response["type"], "error", "{response}");
        assert_eq!(response["error"]["code"], "browser_pane_failed");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("app_surface_unavailable"),
            "the typed forwarding failure must reach the requester: {response}"
        );
        fixture.state.shutdown_graceful("test").await;
    }
}
