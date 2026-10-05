#![cfg_attr(not(test), allow(dead_code))]

use crate::git_service::GitService;
use crate::host_capability::HostCapabilityStore;
use crate::host_control::{ControlHandler, HostEventSink, ProgressSink, VerifiedClientContext};
use crate::host_data::{
    HostDataError, HostDataPlane, SessionSearchResult, WorkspaceSessionBucketResult,
};
use crate::host_router::{HostClientContext, HostRouter, RoutedAction, PROTOCOL_VERSION};
use crate::metadata_store::SharedMetadataStore;
use crate::native_pi_manager::NativePiManager;
use crate::remote_auth::RemoteAuth;
use crate::runtime_coordinator::RuntimeTarget;
use crate::transport_limits::{
    validate, PayloadKind, HTTP_DEFAULT_BODY_BYTES, WS_PHYSICAL_FRAME_BYTES,
};
use crate::window_owner::WindowOwnerRegistry;
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Query;
use axum::extract::{DefaultBodyLimit, Json, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, PRAGMA};
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{any, get, post};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{oneshot, Semaphore};
use tower::ServiceBuilder;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

const MAX_HTTP_BODY_BYTES: usize = HTTP_DEFAULT_BODY_BYTES;
// JSON string escaping can inflate each payload byte up to sixfold
// (\uXXXX); the semantic paste bound in paste_offload stays authoritative,
// so the transport limit only needs headroom for the encoded form.
const MAX_PASTE_BODY_BYTES: usize = crate::paste_offload::MAX_PASTE_BYTES * 6 + 64 * 1024;
const MAX_WS_MESSAGE_BYTES: usize = WS_PHYSICAL_FRAME_BYTES;
const MAX_CONCURRENT_SESSION_SCANS: usize = 2;
/// Token typing re-requests on a 20ms debounce and a client abort does not stop
/// host work, so mention walks need their own small gate: sharing the session
/// scan permits would let keystrokes stall the sidebar.
const MAX_CONCURRENT_MENTION_SCANS: usize = 3;

/// D4/LAN preference gate for the mobile entry. Stored in the shared
/// metadata store so the Settings toggle, the bind decision, and the pairing
/// controls all observe one value.
fn mobile_lan_access_enabled(metadata: &crate::metadata_store::SharedMetadataStore) -> bool {
    metadata
        .lock()
        .ok()
        .and_then(|store| store.pref_get("mobile.lanAccessEnabled").ok())
        .flatten()
        .and_then(|value| value.as_bool())
        // Absent preference must mean loopback-only: the LAN bind is an
        // opt-in mobile feature (D4). This briefly defaulted to `true`
        // (8bd24c8, 2026-09-03; reverted 2026-09-20) — a default-on
        // 0.0.0.0 bind contradicts the loopback promise above and
        // ARCHITECTURE.md's security boundary.
        .unwrap_or(false)
}

/// Best-effort primary LAN URL for the running host, discovered with a
/// routing-table probe (a UDP `connect` sends no packets). Returns `None` on
/// machines without a default route so the UI falls back to manual entry.
fn primary_lan_url(port: u16) -> Option<String> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let local = socket.local_addr().ok()?;
    if local.ip().is_loopback() {
        return None;
    }
    Some(format!("http://{}:{port}", local.ip()))
}

fn bind_is_loopback(address: std::net::IpAddr) -> bool {
    address.is_loopback()
}

/// Fingerprints the static bundle by (path, size, mtime) of every file under
/// `static_dir`, without reading file contents — cheap enough to run once on
/// every server startup even for a bundle with vendored JS/fonts/images, and
/// still changes on every real build (build tooling always rewrites file
/// mtimes). Used to version the URL prefix static assets are served under;
/// see the comment at its call site for why the version string alone isn't
/// enough.
fn fingerprint_static_dir(static_dir: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(static_dir, &mut files);
    files.sort();

    let mut hasher = Sha256::new();
    for path in &files {
        let Ok(meta) = fs::metadata(path) else {
            continue;
        };
        if let Ok(relative) = path.strip_prefix(static_dir) {
            hasher.update(relative.to_string_lossy().as_bytes());
        }
        hasher.update(meta.len().to_le_bytes());
        if let Ok(modified) = meta.modified() {
            if let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) {
                hasher.update(since_epoch.as_millis().to_le_bytes());
            }
        }
    }
    hex::encode(&hasher.finalize()[..8])
}

struct HostState {
    router: Mutex<HostRouter>,
    static_dir: PathBuf,
    #[allow(dead_code)]
    desktop_capabilities: Mutex<HostCapabilityStore>,
    owner_registry: Mutex<Option<Arc<WindowOwnerRegistry>>>,
    browser_panes: Mutex<Option<Arc<crate::browser_pane::BrowserPaneRuntime>>>,
    officecli_watches: Mutex<Option<Arc<crate::officecli_watch::OfficecliWatchRuntime>>>,
    runtimes: NativePiManager,
    auth: Arc<Mutex<RemoteAuth>>,
    data: HostDataPlane,
    session_scan_permits: Arc<Semaphore>,
    /// Native Office preview permits: at most two candidate conversions may
    /// hold input bytes, read, detect, or parse at once (spec 2026-09-17).
    preview_permits: Arc<Semaphore>,
    index_html: Mutex<String>,
    control_handler: Mutex<Option<ControlHandler>>,
    git_service: Mutex<Option<Arc<GitService>>>,
    host_events: HostEventSink,
    ephemeral_hub: Mutex<Option<crate::host_ephemeral::SharedEphemeralHub>>,
    terminal_manager: Mutex<Option<Arc<crate::terminal_manager::TerminalManager>>>,
    oauth: Arc<Mutex<crate::oauth_manager::OAuthManager>>,
    mcp_login: crate::mcp_login_runner::McpLoginRunner,
    /// P4-d: one-shot TTL'd export tokens backing `session_export`.
    session_exports: crate::host_data::SessionExportRegistry,
    /// D8: anonymous client-class hit counts for the retired `/api/rpc`
    /// surface (no per-user/per-token dimensions).
    legacy_rpc_gone_hits: Mutex<HashMap<String, u64>>,
    /// D4/LAN bind decision for this process, read once at start from the
    /// `mobile.lanAccessEnabled` preference. Pairing controls re-check it so a
    /// runtime-disabled host never mints tokens even if the pref flips later.
    lan_access: bool,
    /// Test seam: pins the agent root so subagents scans stay inside the test
    /// fixture instead of the real Pi agent directory. Production never sets it.
    agent_root_override: Mutex<Option<PathBuf>>,
    /// Port the host listener bound; stored after `start` binds because the
    /// OS assigns it. Mobile entry URLs are built from this.
    host_port: std::sync::atomic::AtomicU16,
}

pub struct HostServer {
    origin: String,
    shutdown: Option<oneshot::Sender<()>>,
    state: Arc<HostState>,
}

/// macOS attributes UN notifications to a bundle identifier. The host server
/// never sees the Tauri config, so main.rs publishes the running build's own
/// identifier here (`picot`, `picot.dev`, `picot.internal`); unset callers
/// (tests, hostless runs) fall back to the production identifier.
static NOTIFICATION_BUNDLE_ID: OnceLock<String> = OnceLock::new();

/// Publish the running build's bundle identifier for OS notifications.
pub fn set_notification_bundle_identifier(identifier: impl Into<String>) {
    let _ = NOTIFICATION_BUNDLE_ID.set(identifier.into());
}

fn notification_bundle_identifier() -> &'static str {
    NOTIFICATION_BUNDLE_ID
        .get()
        .map(String::as_str)
        .unwrap_or("com.palandata.picot")
}

impl HostServer {
    /// Advance OAuth generation when native runtime is created.
    pub fn runtime_started(&self) -> Result<u64, String> {
        self.state.mcp_login.abort_all();
        self.state
            .oauth
            .lock()
            .map_err(|_| "OAuth manager unavailable".to_owned())
            .map(|mut oauth| oauth.runtime_started())
    }

    /// Revoke OAuth operations when native runtime stops.
    pub fn runtime_stopped(&self) -> Result<(), String> {
        self.state.mcp_login.abort_all();
        self.state
            .oauth
            .lock()
            .map_err(|_| "OAuth manager unavailable".to_owned())
            .map(|mut oauth| oauth.runtime_stopped())
    }

    pub async fn start(
        static_dir: PathBuf,
        runtimes: NativePiManager,
        auth: Arc<Mutex<RemoteAuth>>,
        metadata: SharedMetadataStore,
    ) -> Result<Self, String> {
        let session_root = dirs::home_dir().map(|home| home.join(".pi/agent/sessions"));
        Self::start_with_session_root(static_dir, runtimes, auth, metadata, session_root).await
    }

    /// Same as [`Self::start`] with an explicit session root. Production
    /// resolves `~/.pi/agent/sessions`; tests inject a hermetic directory so
    /// the whole data plane (routing, auth, scan, parse) is provable without
    /// touching real session history.
    pub async fn start_with_session_root(
        static_dir: PathBuf,
        runtimes: NativePiManager,
        auth: Arc<Mutex<RemoteAuth>>,
        metadata: SharedMetadataStore,
        session_root: Option<PathBuf>,
    ) -> Result<Self, String> {
        // D4/LAN policy: loopback by default; LAN interfaces only when the user
        // explicitly enabled mobile access. Reading the preference here means
        // the Settings toggle and the bind decision observe one value, and a
        // flip takes effect on the next host start.
        let lan_access = mobile_lan_access_enabled(&metadata);
        let mut data = HostDataPlane::new(metadata);
        if let Some(session_root) = session_root {
            data = data.with_session_root(session_root);
        }
        let (host_event_tx, _) = tokio::sync::broadcast::channel(256);
        let host_events = HostEventSink::new(host_event_tx);
        let oauth = Arc::new(Mutex::new(crate::oauth_manager::OAuthManager::default()));
        let mcp_login =
            crate::mcp_login_runner::McpLoginRunner::new(oauth.clone(), host_events.clone());
        let state = Arc::new(HostState {
            router: Mutex::new(HostRouter::new()),
            static_dir: static_dir.clone(),
            desktop_capabilities: Mutex::new(HostCapabilityStore::default()),
            owner_registry: Mutex::new(None),
            browser_panes: Mutex::new(None),
            officecli_watches: Mutex::new(None),
            runtimes,
            auth,
            data,
            session_scan_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_SESSION_SCANS)),
            preview_permits: Arc::new(Semaphore::new(2)),
            index_html: Mutex::new(String::new()),
            control_handler: Mutex::new(None),
            git_service: Mutex::new(None),
            host_events: host_events.clone(),
            ephemeral_hub: Mutex::new(None),
            terminal_manager: Mutex::new(None),
            oauth,
            mcp_login,
            session_exports: crate::host_data::SessionExportRegistry::new(
                8,
                std::time::Duration::from_secs(300),
            ),
            legacy_rpc_gone_hits: Mutex::new(HashMap::new()),
            lan_access,
            agent_root_override: Mutex::new(None),
            host_port: std::sync::atomic::AtomicU16::new(0),
        });
        // Prewarm the cost-metrics cache in the background: one 90d scan at
        // startup parses every file the 7d/30d/90d chips can need, so the
        // first Settings → Usage open answers from cache instead of parsing
        // hundreds of MB of session jsonl on the request path. Guarded off
        // in test builds: the suite starts real servers against the user's
        // real session root, and a background full scan there starves the
        // timing-sensitive spawn/route tests of CPU.
        if !cfg!(test) {
            if let Some(session_root) = state.data.session_root_path() {
                let cache = state.data.cost_metrics_cache();
                std::thread::spawn(move || {
                    let Some(params) = crate::cost_compat::parse_cost_range_params(&[(
                        "range".to_string(),
                        "90d".to_string(),
                    )]) else {
                        return;
                    };
                    let _ = crate::cost_compat::scan_compat_cost_dashboard(
                        &session_root,
                        std::path::Path::new("/"),
                        &params,
                        chrono::Utc::now(),
                        Some(&cache),
                    );
                });
            }
        }
        let index = static_dir.join("index.html");
        // Serve this build's JS/CSS/HTML under a version-stamped path
        // (`/v/<version>/...`) and point index.html's `<base>` at it. The
        // `Cache-Control: no-store` headers below are meant to stop the
        // WebView from reusing stale assets across an auto-update +
        // relaunch (the host listens on a stable port across restarts), but
        // WebKit has been observed to keep serving a URL's very first
        // cached response indefinitely without ever revalidating it against
        // fresh headers. A version-scoped URL sidesteps that entirely: each
        // release is a guaranteed cache miss for every asset, no matter how
        // the WebView's cache behaves.
        // A version string alone isn't a reliable cache-busting key: a
        // hotfix or dev build can ship with the app version unchanged (no
        // version bump), which would leave the WebView's cache pinned to
        // stale assets exactly like the bug this route exists to avoid. A
        // content fingerprint changes on every real rebuild regardless of
        // whether anyone remembered to bump the version.
        let versioned_prefix = format!("/v/{}", fingerprint_static_dir(&static_dir));
        let index_html = fs::read_to_string(&index).unwrap_or_default().replacen(
            "<base href=\"/\" />",
            &format!("<base href=\"{versioned_prefix}/\" />"),
            1,
        );
        if let Ok(mut html) = state.index_html.lock() {
            *html = index_html.clone();
        }
        let index_fallback = tower::service_fn(move |_req: axum::extract::Request| {
            let html = index_html.clone();
            std::future::ready(Ok::<_, Infallible>(
                Response::builder()
                    .header(CONTENT_TYPE, "text/html; charset=utf-8")
                    .body(Body::from(html))
                    .expect("static index.html response is well-formed"),
            ))
        });
        let static_service = ServeDir::new(static_dir.clone()).fallback(index_fallback);
        // Always disable caching for the static bundle, not just in debug
        // builds: the host listens on a stable port across app restarts, so
        // after an auto-update + relaunch the WebView's HTTP cache would
        // otherwise keep serving the previous release's JS/CSS/HTML until a
        // manual hard reload.
        let static_service = ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::overriding(
                CACHE_CONTROL,
                HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                PRAGMA,
                HeaderValue::from_static("no-cache"),
            ))
            .service(static_service);
        let versioned_service = ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::overriding(
                CACHE_CONTROL,
                HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                PRAGMA,
                HeaderValue::from_static("no-cache"),
            ))
            .service(ServeDir::new(static_dir));
        let app = Router::new()
            .route("/health", get(health))
            .route("/api/health", get(health))
            .route("/api/pi-version", get(pi_version))
            // Retain only as authenticated compatibility facades for external
            // clients during migration; frontend production callers use v2.
            .route("/api/files", get(compat_files))
            .route("/api/sessions", get(compat_sessions))
            .route("/api/search", get(compat_search))
            .route("/api/cost-dashboard", get(compat_cost_dashboard))
            .route("/api/instances", get(compat_instances))
            .route("/api/home", get(compat_home))
            .route("/api/workspace-info", get(compat_workspace_info))
            .route("/api/workspace-sessions", get(compat_workspace_sessions))
            .route("/api/sessions/rename", post(compat_sessions_rename))
            .route(
                "/api/sessions/delete-batch",
                post(compat_sessions_delete_batch),
            )
            .route("/api/sessions/switch", post(compat_sessions_switch))
            .route("/api/sessions/{dir_name}/{file}", get(compat_session_file))
            .route("/api/workspace/open", post(compat_workspace_open))
            // Route-only compat: these surfaces have a v2 equivalent (data op or
            // control), so the HTTP entry stays an explicit 410 instead of a
            // silently diverging second implementation.
            .route("/api/files/content", any(api_gone))
            .route("/api/files/raw", any(api_gone))
            .route("/api/file-mentions", any(api_gone))
            .route("/api/git-branch", any(api_gone))
            .route("/api/open", any(api_gone))
            .route("/api/paste-offload", any(api_gone))
            .route("/api/models-config", any(api_gone))
            .route("/api/agent-config", any(api_gone))
            .route("/api/agents-md", any(api_gone))
            .route("/api/append-system-md", any(api_gone))
            .route("/api/chat-config", any(api_gone))
            .route("/api/chat-telegram/{operation}", any(api_gone))
            .route("/api/lan-qr", get(api_gone))
            .route("/api/skill-install-links", post(api_gone))
            .route("/api/skill-install-scan", post(api_gone))
            .route("/api/super-agent/projects", get(api_gone))
            .route("/api/super-agent/tasks", get(api_gone).put(api_gone))
            .route("/v2/session-export/{token}", get(session_export_stream))
            .route(
                "/v2/session-history/{session_id}",
                get(session_history_stream),
            )
            .route("/v2/files/raw", get(raw_file_stream))
            .route("/api/rpc", any(rpc_retired))
            .route("/v2/ws", get(websocket_upgrade))
            // Bare Pi-origin WebSocket paths are never valid on host origin.
            // Keep static fallback from turning `/ws` into a misleading shell.
            .route("/ws", any(reject_legacy_ws))
            .route("/v2/bootstrap", get(bootstrap_target))
            .route("/v2/auth/exchange", post(exchange_pairing))
            .route("/v2/mobile/status", get(mobile_status))
            .route(
                "/v2/paste-offload",
                post(paste_offload).layer(DefaultBodyLimit::max(MAX_PASTE_BODY_BYTES)),
            )
            // Legacy HTTP callers must never receive the static shell by
            // accident. Retained routes are added behind owner-aware adapters;
            // everything else has an explicit migration failure.
            .route("/api/{*path}", any(unimplemented_api_route))
            // Existing shell is production at the registered workspace/session
            // namespace. Static fallback still serves its index so bootstrap-
            // entry.js selects the legacy shell; /app remains experimental.
            .route(
                "/workspaces/{workspace_id}/sessions/{session_id}",
                get(workspace_shell),
            )
            .nest_service(&versioned_prefix, versioned_service)
            .fallback_service(static_service)
            .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
            .with_state(Arc::clone(&state));
        // D4/LAN policy: see the `lan_access` field. Loopback unless the user
        // explicitly turned mobile access on; `0.0.0.0` covers every interface
        // including loopback, so the desktop window keeps working unchanged.
        let bind_host = if state.lan_access {
            log::warn!(
                "[picot-host] mobile/LAN access enabled: the host is reachable from LAN devices"
            );
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        } else {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        };
        let listener = tokio::net::TcpListener::bind((bind_host, 0))
            .await
            .map_err(|error| format!("Cannot bind Picot Host: {error}"))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("Cannot read Picot Host address: {error}"))?;
        state
            .host_port
            .store(address.port(), std::sync::atomic::Ordering::Relaxed);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
            {
                log::error!("[picot-host] server stopped unexpectedly: {error}");
            }
        });
        // Desktop windows keep talking to 127.0.0.1 even when the host is also
        // reachable on LAN: an UNSPECIFIED bind would otherwise put `0.0.0.0`
        // into the window URL and the capability origin.
        let display_address = if address.ip().is_unspecified() {
            std::net::SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                address.port(),
            )
        } else {
            address
        };
        Ok(Self {
            origin: format!("http://{display_address}"),
            shutdown: Some(shutdown_tx),
            state,
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Bind window-owner authority after host bind, before any desktop window connects.
    pub fn set_owner_registry(&self, registry: Arc<WindowOwnerRegistry>) {
        if let Ok(mut slot) = self.state.owner_registry.lock() {
            *slot = Some(registry);
        }
    }

    /// Test seam for the subagents controls: pins the agent root so scans
    /// stay inside a fixture. Production resolves the real root per request.
    pub fn set_agent_root_for_tests(&self, root: PathBuf) {
        if let Ok(mut slot) = self.state.agent_root_override.lock() {
            *slot = Some(root);
        }
    }

    /// Attach the browser-pane runtime (child webviews) after host bind.
    pub fn set_browser_panes(&self, runtime: Arc<crate::browser_pane::BrowserPaneRuntime>) {
        if let Ok(mut slot) = self.state.browser_panes.lock() {
            *slot = Some(runtime);
        }
    }

    /// Attach the officecli watch runtime after host bind.
    pub fn set_officecli_watches(
        &self,
        runtime: Arc<crate::officecli_watch::OfficecliWatchRuntime>,
    ) {
        if let Ok(mut slot) = self.state.officecli_watches.lock() {
            *slot = Some(runtime);
        }
    }
    /// Test-only read-back beside the setter; production code re-reads the
    /// registry through HostState, never through the server handle.
    #[cfg(test)]
    pub fn owner_registry_for_tests(&self) -> Option<Arc<WindowOwnerRegistry>> {
        self.state
            .owner_registry
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// Install the native ephemeral hub so the WebSocket loop can route
    /// `ephemeral_command` frames and greet desktop clients with bootstrap.
    pub fn set_ephemeral_hub(&self, hub: crate::host_ephemeral::SharedEphemeralHub) {
        if let Ok(mut slot) = self.state.ephemeral_hub.lock() {
            *slot = Some(hub);
        }
    }

    /// The native runtime manager this host routes runtime traffic through.
    pub fn native_manager(&self) -> &NativePiManager {
        &self.state.runtimes
    }

    /// Install the terminal manager so the WebSocket loop can route
    /// `terminal_command` frames (PTY lifecycle, input, and checkpoints).
    pub fn set_terminal_manager(&self, manager: Arc<crate::terminal_manager::TerminalManager>) {
        if let Ok(mut slot) = self.state.terminal_manager.lock() {
            *slot = Some(manager);
        }
    }

    /// Drop every outstanding session-export grant for an owner (workspace
    /// transition / owner teardown hygiene; M4).
    pub fn revoke_session_exports(&self, owner: &str) {
        self.state.session_exports.revoke_owner(owner);
    }

    /// Install owner-checked control handler shared by host-origin v2 dispatch.
    /// The host must not grow a second control implementation.
    pub fn set_control_handler(&self, handler: ControlHandler) {
        if let Ok(mut slot) = self.state.control_handler.lock() {
            *slot = Some(handler);
        }
    }

    pub fn set_git_service(&self, service: Arc<GitService>) {
        if let Ok(mut slot) = self.state.git_service.lock() {
            *slot = Some(service);
        }
    }

    pub fn event_sink(&self) -> HostEventSink {
        self.state.host_events.clone()
    }

    pub fn send_owner_event(&self, owner: &crate::window_owner::OwnerId, value: Value) -> bool {
        let has_owner = self
            .state
            .router
            .lock()
            .ok()
            .is_some_and(|router| router.has_desktop_owner(owner));
        has_owner && self.state.host_events.send_owner_event(owner, value)
    }

    #[allow(dead_code)]
    pub fn broadcast_native_event(&self, value: Value) -> usize {
        self.state.host_events.broadcast_native_event(value)
    }

    pub fn stop(mut self) {
        let _ = self.runtime_stopped();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

impl Drop for HostServer {
    fn drop(&mut self) {
        self.state.mcp_login.abort_all();
        let _ = self
            .state
            .oauth
            .lock()
            .map(|mut oauth| oauth.runtime_stopped());
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// Host liveness only. Runtime readiness is a separate concern; the version
/// constant comes from the shared `pi_launch` substrate.
async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "protocolVersion": PROTOCOL_VERSION,
        "piVersion": crate::pi_launch::locked_pi_version(),
    }))
}

/// Retained compatibility endpoint. It exposes only host build metadata.
async fn pi_version() -> Json<Value> {
    Json(json!({
        "success": true,
        "version": crate::pi_launch::locked_pi_version(),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceQuery {
    workspace_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileQuery {
    workspace_id: Option<String>,
    #[serde(default)]
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchQuery {
    workspace_id: Option<String>,
    #[serde(default)]
    q: String,
}

fn compat_owner_workspace(
    state: &HostState,
    headers: &HeaderMap,
    requested_workspace_id: Option<&str>,
) -> Result<String, (StatusCode, Json<Value>)> {
    let capability = headers
        .get("x-picot-desktop-capability")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    let registry = state
        .owner_registry
        .lock()
        .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable"))?
        .clone()
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    let owner = registry
        .authenticate(capability)
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    let snapshot = registry.owner_current_workspace(&owner);
    match snapshot {
        crate::window_owner::OwnerWorkspaceSnapshot::Registered { wid, .. }
            if requested_workspace_id.is_none_or(|requested| requested == wid) =>
        {
            Ok(wid)
        }
        crate::window_owner::OwnerWorkspaceSnapshot::Registered { .. } => {
            Err(api_error(StatusCode::FORBIDDEN, "unauthorized_target"))
        }
        _ => Err(api_error(StatusCode::FORBIDDEN, "not_registered")),
    }
}

#[derive(Deserialize)]
struct PasteRequest {
    content: String,
}

async fn paste_offload(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, None)?;
    let request: PasteRequest = serde_json::from_slice(&body)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_request"))?;
    let root = state
        .data
        .workspace_root(&workspace_id)
        .map_err(host_data_error_response)?;
    let path = crate::paste_offload::write(&root, &request.content, SystemTime::now()).map_err(
        |error| {
            let (status, code) = match error {
                crate::paste_offload::PasteError::TooLarge => {
                    (StatusCode::PAYLOAD_TOO_LARGE, "paste_too_large")
                }
                crate::paste_offload::PasteError::QuotaExceeded => {
                    (StatusCode::PAYLOAD_TOO_LARGE, "paste_quota_exceeded")
                }
                crate::paste_offload::PasteError::InvalidWorkspace
                | crate::paste_offload::PasteError::Symlink => {
                    (StatusCode::FORBIDDEN, "workspace_unavailable")
                }
                crate::paste_offload::PasteError::Io => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "paste_write_failed")
                }
            };
            api_error(status, code)
        },
    )?;
    Ok(Json(json!({ "ok": true, "path": path })))
}

async fn compat_files(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, query.workspace_id.as_deref())?;
    let entries = state
        .data
        .list_files(&workspace_id, &query.path)
        .map_err(host_data_error_response)?;
    Ok(Json(json!({ "success": true, "entries": entries })))
}

async fn compat_sessions(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, query.workspace_id.as_deref())?;
    let sessions = list_sessions_blocking(
        state.data.clone(),
        Arc::clone(&state.session_scan_permits),
        workspace_id,
    )
    .await?;
    Ok(Json(json!({ "success": true, "sessions": sessions })))
}

async fn compat_search(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, query.workspace_id.as_deref())?;
    let results = search_sessions_blocking(
        state.data.clone(),
        Arc::clone(&state.session_scan_permits),
        workspace_id,
        query.q,
    )
    .await?;
    Ok(Json(json!({ "success": true, "results": results })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CostQuery {
    workspace_id: Option<String>,
    range: Option<String>,
    granularity: Option<String>,
    scope: Option<String>,
    models: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

async fn compat_cost_dashboard(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    Query(query): Query<CostQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, query.workspace_id.as_deref())?;
    let pairs = [
        ("range", query.range.as_deref()),
        ("granularity", query.granularity.as_deref()),
        ("scope", query.scope.as_deref()),
        ("models", query.models.as_deref()),
        ("from", query.from.as_deref()),
        ("to", query.to.as_deref()),
    ]
    .iter()
    .filter_map(|(key, value)| value.map(|value| ((*key).to_string(), value.to_string())))
    .collect::<Vec<(String, String)>>();
    let params = crate::cost_compat::parse_cost_range_params(&pairs)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "invalid_cost_range"))?;
    let payload = state
        .data
        .cost_dashboard_compat(&workspace_id, &params, chrono::Utc::now())
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "cost_scan_failed"))?;
    Ok(Json(payload))
}

async fn compat_instances(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    compat_owner_workspace(&state, &headers, None)?;
    let mut instances = Vec::new();
    for target in state.runtimes.running_targets() {
        let pid = state.runtimes.pid_for(&target);
        let cwd = state
            .data
            .workspace_root(&target.workspace_id)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let session_file = state
            .data
            .session_file_path(&target.workspace_id, &target.session_id)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        instances.push(json!({
            "port": 0,
            "pid": pid.unwrap_or(0),
            "sessionFile": session_file,
            "cwd": cwd,
            "startedAt": Value::Null,
        }));
    }
    Ok(Json(json!({ "instances": instances })))
}

async fn compat_home(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // A-HTTP-18: explicit capability boundary on host-origin (legacy was
    // loopback-open; native host applies the same owner boundary).
    compat_owner_workspace(&state, &headers, None)?;
    Ok(Json(json!({
        "home": dirs::home_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
    })))
}

async fn compat_workspace_info(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, query.workspace_id.as_deref())?;
    let path = state
        .data
        .workspace_root(&workspace_id)
        .map_err(host_data_error_response)?;
    Ok(Json(json!({
        "workspaceId": workspace_id,
        "path": path.to_string_lossy(),
    })))
}

#[derive(Deserialize)]
struct WorkspaceSessionsQuery {
    path: String,
    mode: Option<String>,
}

#[derive(Deserialize)]
struct RawFileQuery {
    workspace_id: String,
    path: String,
}

async fn raw_file_stream(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    Query(query): Query<RawFileQuery>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, Some(&query.workspace_id))?;
    let root = state
        .data
        .workspace_root(&workspace_id)
        .map_err(host_data_error_response)?;
    let content = crate::host_files::read(&root, &query.path)
        .map_err(|error| api_error(StatusCode::NOT_FOUND, error.code()))?;
    let mime = match Path::new(&query.path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    };
    Ok(Response::builder()
        .header(CONTENT_TYPE, mime)
        .header(CACHE_CONTROL, "no-store")
        .body(Body::from(content.bytes))
        .expect("raw file response is well-formed"))
}

async fn session_history_stream(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, None)?;
    let path = state
        .data
        .session_file_path(&workspace_id, &session_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "session_not_found"))?;
    let content = fs::read_to_string(path)
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "session_io_failed"))?;
    let entries = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    Ok(Json(json!({ "entries": entries })))
}

async fn compat_workspace_sessions(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceSessionsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, None)?;
    if !query.path.starts_with('/') {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid_path"));
    }
    let root = state
        .data
        .workspace_root(&workspace_id)
        .map_err(host_data_error_response)?;
    let count_only = query.mode.as_deref() == Some("count");
    let (dir_name, sessions, session_count, hidden_subagent_count) =
        read_workspace_session_bucket_blocking(
            state.data.clone(),
            Arc::clone(&state.session_scan_permits),
            workspace_id,
            count_only,
        )
        .await?;
    Ok(Json(json!({
        "path": root,
        "dirName": dir_name,
        "sessions": sessions,
        "sessionCount": session_count,
        "hiddenSubagentCount": hidden_subagent_count,
    })))
}

#[derive(Deserialize)]
struct SessionRenameBody {
    #[serde(rename = "filePath")]
    file_path: String,
    name: String,
}

async fn compat_sessions_rename(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    compat_owner_workspace(&state, &headers, None)?;
    // Legacy rename body bound: 8 KiB (readBoundedJsonBody).
    if body.len() > 8 * 1024 {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
        ));
    }
    let body: SessionRenameBody = serde_json::from_slice(&body)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_rename_request"))?;
    let name = body.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Name must be 1-200 characters",
        ));
    }
    let Some(session_root) = state.data.session_root_path() else {
        return Err(api_error(StatusCode::NOT_FOUND, "Session is unavailable"));
    };
    let canonical = std::path::PathBuf::from(&body.file_path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&body.file_path));
    let root_canonical = session_root
        .canonicalize()
        .unwrap_or_else(|_| session_root.clone());
    // Containment must be separator-safe (a sibling like `sessions-evil`
    // shares the string prefix but not the directory).
    if !body.file_path.ends_with(".jsonl") || canonical.strip_prefix(&root_canonical).is_err() {
        return Err(api_error(StatusCode::NOT_FOUND, "Session is unavailable"));
    }
    // Managed-session membership (legacy renameManagedSession): the target
    // must parse as a session file, not an arbitrary .jsonl in the tree.
    if crate::host_data::parse_session_header(&canonical).is_none() {
        return Err(api_error(StatusCode::NOT_FOUND, "Session is unavailable"));
    }
    state
        .data
        .append_session_info_name(&canonical, &name)
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "session_rename_failed"))?;
    Ok(Json(
        json!({ "ok": true, "filePath": body.file_path, "name": name }),
    ))
}

#[derive(Deserialize)]
struct SessionDeleteBatchBody {
    #[serde(rename = "filePaths")]
    file_paths: Vec<String>,
}

async fn compat_sessions_delete_batch(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    compat_owner_workspace(&state, &headers, None)?;
    // Legacy delete body bound: 8 KiB.
    if body.len() > 8 * 1024 {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
        ));
    }
    let body: SessionDeleteBatchBody = serde_json::from_slice(&body)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_delete_batch"))?;
    let running_session_files = state
        .runtimes
        .running_targets()
        .iter()
        .filter_map(|target| {
            state
                .data
                .session_file_path(&target.workspace_id, &target.session_id)
                .map(|path| path.to_string_lossy().into_owned())
        })
        .collect::<Vec<String>>();
    let result = state
        .data
        .delete_session_batch(&body.file_paths, &running_session_files)
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "session_delete_failed"))?;
    Ok(Json(result))
}

async fn compat_sessions_switch() -> Json<Value> {
    Json(json!({
        "success": true,
        "embedded": true,
        "note": "Session switching is controlled by Picot's Rust side",
    }))
}

/// A-HTTP-39: session-file history view. Segments arrive percent-decoded by
/// the extractor; decoding must never reintroduce separators or traversal
/// (legacy `decodeSessionRouteSegments`), the bucket must belong to the
/// requesting owner's workspace, and the resolved path stays inside the
/// shared session root. Responds with the legacy `{entries}` shape.
async fn compat_session_file(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    axum::extract::Path((dir_name, file)): axum::extract::Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let workspace_id = compat_owner_workspace(&state, &headers, None)?;
    let unsafe_segment =
        |segment: &str| segment.contains('/') || segment.contains('\\') || segment.contains("..");
    if unsafe_segment(&dir_name) || unsafe_segment(&file) {
        return Err(api_error(StatusCode::BAD_REQUEST, "Invalid session path"));
    }
    let owned = state
        .data
        .session_bucket_for_workspace(&workspace_id)
        .and_then(|bucket| {
            bucket
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .is_some_and(|bucket| bucket == dir_name);
    if !owned {
        return Err(api_error(StatusCode::NOT_FOUND, "Session not found"));
    }
    let Some(session_root) = state.data.session_root_path() else {
        return Err(api_error(StatusCode::NOT_FOUND, "Session not found"));
    };
    let path = session_root.join(&dir_name).join(&file);
    let resolved = path.canonicalize().unwrap_or_else(|_| path.clone());
    let resolved_root = session_root
        .canonicalize()
        .unwrap_or_else(|_| session_root.clone());
    if resolved == resolved_root || resolved.strip_prefix(&resolved_root).is_err() {
        return Err(api_error(StatusCode::NOT_FOUND, "Session not found"));
    }
    if !resolved.is_file() {
        return Err(api_error(StatusCode::NOT_FOUND, "Session not found"));
    }
    let content = std::fs::read_to_string(&resolved)
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "session_io_failed"))?;
    let entries: Vec<Value> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    Ok(Json(json!({ "entries": entries })))
}

async fn compat_workspace_open(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    compat_owner_workspace(&state, &headers, None)?;
    let body: Value = serde_json::from_slice(&body)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_open_request"))?;
    let path = body
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "path is required"))?;
    let canonical = std::path::PathBuf::from(path)
        .canonicalize()
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "workspace_not_found"))?;
    // Owner-only: the directory must be a registered workspace root.
    state
        .data
        .workspace_root_for_path(&canonical)
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "workspace_not_found"))?;
    open_directory_in_file_manager(&canonical);
    Ok(Json(json!({ "success": true })))
}

/// D8 scope removal: retired routes return 410 Gone uniformly.
async fn api_gone() -> (StatusCode, Json<Value>) {
    (
        StatusCode::GONE,
        Json(json!({
            "error": { "code": "gone" },
            "removalNotice": "This endpoint has been retired per D8.",
        })),
    )
}

fn open_directory_in_file_manager(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    crate::windows_child::hide_console(&mut command);
    let _ = command.arg(path).spawn();
}

/// P4-d: stream a session file for a redeemed one-shot export token.
async fn session_export_stream(
    State(state): State<Arc<HostState>>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let owner_registry = state
        .owner_registry
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    let path = state
        .session_exports
        .redeem_checked(&token, |owner, generation| {
            owner_registry.as_ref().is_some_and(|registry| {
                let owner_id = crate::window_owner::OwnerId::from_string(owner.to_string());
                matches!(
                    registry.owner_current_workspace(&owner_id),
                    crate::window_owner::OwnerWorkspaceSnapshot::Registered {
                        generation: current,
                        ..
                    } if current == generation
                )
            })
        })
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "export_token_invalid"))?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "export_unavailable"))?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session-export.html".to_owned());

    let stream = futures_util::stream::unfold(file, |mut file| async move {
        let mut buf = vec![0u8; 64 * 1024];
        match tokio::io::AsyncReadExt::read(&mut file, &mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(buf)), file))
            }
            Err(error) => Some((Err(std::io::Error::other(error)), file)),
        }
    });
    Ok(Response::builder()
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .header(
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{file_name}\""),
        )
        .body(Body::from_stream(stream))
        .expect("streaming response is well-formed"))
}

/// D8 (2026-08-29 adjudication): the legacy chat RPC surface is retired —
/// no permanent second RPC. Callers get 410 Gone with a removal notice and
/// an anonymous client-class hit count (no per-user/token dimensions).
async fn rpc_retired(State(state): State<Arc<HostState>>, headers: HeaderMap) -> Response {
    let client_class = if headers.contains_key("x-picot-desktop-capability") {
        "desktop"
    } else if headers.contains_key("authorization") {
        "paired_remote"
    } else {
        "unpaired_browser"
    };
    if let Ok(mut hits) = state.legacy_rpc_gone_hits.lock() {
        *hits.entry(client_class.to_string()).or_insert(0) += 1;
    }
    let mut response = (
        StatusCode::GONE,
        Json(json!({
            "error": { "code": "gone" },
            "removalNotice": "/api/rpc retired per D8 — use the v2 WebSocket surface; see the release notes.",
            "clientClass": client_class,
        })),
    )
        .into_response();
    response.headers_mut().insert(
        axum::http::header::HeaderName::from_static("deprecation"),
        HeaderValue::from_static("true"),
    );
    response
}

fn host_data_error_response(error: HostDataError) -> (StatusCode, Json<Value>) {
    let (code, _) = host_data_error(error);
    let status = if code == "workspace_not_found" {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    api_error(status, code)
}

async fn read_workspace_session_bucket_blocking(
    data: HostDataPlane,
    session_scan_permits: Arc<Semaphore>,
    workspace_id: String,
    count_only: bool,
) -> Result<WorkspaceSessionBucketResult, (StatusCode, Json<Value>)> {
    let permit = session_scan_permits.acquire_owned().await.map_err(|_| {
        host_data_error_response(HostDataError::Io("session scan queue closed".to_owned()))
    })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        data.read_workspace_session_bucket(&workspace_id, count_only)
    })
    .await
    .map_err(|_| host_data_error_response(HostDataError::Io("session scan failed".to_owned())))
}

async fn list_sessions_blocking(
    data: HostDataPlane,
    session_scan_permits: Arc<Semaphore>,
    workspace_id: String,
) -> Result<Vec<crate::host_data::SessionSummary>, (StatusCode, Json<Value>)> {
    let permit = session_scan_permits.acquire_owned().await.map_err(|_| {
        host_data_error_response(HostDataError::Io("session scan queue closed".to_owned()))
    })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        data.list_sessions(&workspace_id)
    })
    .await
    .map_err(|_| {
        host_data_error_response(HostDataError::Io("session list scan failed".to_owned()))
    })?
    .map_err(host_data_error_response)
}

async fn search_sessions_blocking(
    data: HostDataPlane,
    session_scan_permits: Arc<Semaphore>,
    workspace_id: String,
    query: String,
) -> Result<Vec<SessionSearchResult>, (StatusCode, Json<Value>)> {
    let permit = session_scan_permits.acquire_owned().await.map_err(|_| {
        host_data_error_response(HostDataError::Io("session scan queue closed".to_owned()))
    })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        data.search_sessions(&workspace_id, &query)
    })
    .await
    .map_err(|_| host_data_error_response(HostDataError::Io("search scan failed".to_owned())))?
    .map_err(host_data_error_response)
}

/// Host-origin entry for production existing-shell windows. Route parameters
/// are validated before serving bootstrap HTML; runtime authorization remains
/// on `/v2/bootstrap` and `/v2/ws`, never in browser-provided URL state.
async fn workspace_shell(
    State(state): State<Arc<HostState>>,
    axum::extract::Path((workspace_id, session_id)): axum::extract::Path<(String, String)>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    if workspace_id.is_empty() || session_id.is_empty() {
        return Err(api_error(StatusCode::NOT_FOUND, "runtime_not_found"));
    }
    if state
        .runtimes
        .target_for_session(&workspace_id, &session_id)
        .is_none()
    {
        return Err(api_error(StatusCode::NOT_FOUND, "runtime_not_found"));
    }
    let html = state
        .index_html
        .lock()
        .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "static_unavailable"))?
        .clone();
    Ok(Response::builder()
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .expect("workspace shell response is well-formed"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapQuery {
    workspace_id: String,
    session_id: String,
}

async fn reject_legacy_ws() -> (StatusCode, Json<Value>) {
    api_error(StatusCode::NOT_FOUND, "unsupported_host_route")
}

async fn bootstrap_target(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    Query(query): Query<BootstrapQuery>,
) -> Result<Json<RuntimeTarget>, (StatusCode, Json<Value>)> {
    // Bootstrap is control-plane data: bind it to authenticated desktop owner,
    // not merely opaque workspace/session query values.
    let _workspace_id = compat_owner_workspace(&state, &headers, Some(&query.workspace_id))?;
    let target = state
        .runtimes
        .target_for_session(&query.workspace_id, &query.session_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "runtime_not_found"))?;
    let registry = state
        .owner_registry
        .lock()
        .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable"))?
        .clone()
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    let capability = headers
        .get("x-picot-desktop-capability")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    let owner = registry
        .authenticate(capability)
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    if target.owner_id.as_deref() != Some(owner.as_str()) {
        return Err(api_error(StatusCode::FORBIDDEN, "unauthorized_target"));
    }
    Ok(Json(target))
}

fn oversized_code(kind: PayloadKind) -> &'static str {
    match kind {
        PayloadKind::Response => "response_too_large",
        PayloadKind::Snapshot => "snapshot_too_large",
        PayloadKind::Event => "event_too_large",
        PayloadKind::Progress => "progress_too_large",
        _ => "response_too_large",
    }
}

/// Outbound payload classification. `get_tree` returns snapshot-scale
/// session content (an 868-entry session serializes to ~3.5MB); routing it
/// under the generic Response cap rejects trees whose chat snapshot already
/// renders, so tree replies share the Snapshot limit.
fn outbound_payload_kind(frame_type: Option<&str>, command_type: Option<&str>) -> PayloadKind {
    match frame_type {
        Some("runtime_snapshot") => PayloadKind::Snapshot,
        _ if command_type == Some("get_tree") => PayloadKind::Snapshot,
        _ => PayloadKind::Response,
    }
}

fn outbound_message(value: Value, kind: PayloadKind) -> Message {
    if validate(&value, kind).is_err() {
        // Keep the requestId from the replaced frame: without it the pending
        // frontend control cannot correlate the rejection and times out
        // instead of surfacing the failure.
        let request_id = value.get("requestId").and_then(Value::as_str);
        let error = structured_error(
            request_id,
            oversized_code(kind),
            "Outbound payload exceeds protocol limit",
        );
        return Message::Text(error.to_string().into());
    }
    Message::Text(value.to_string().into())
}

async fn send_checked(socket: &mut WebSocket, value: Value, kind: PayloadKind) -> bool {
    socket.send(outbound_message(value, kind)).await.is_ok()
}

async fn websocket_upgrade(
    State(state): State<Arc<HostState>>,
    websocket: WebSocketUpgrade,
) -> Response {
    websocket
        .max_message_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_websocket(socket, state))
}

async fn handle_websocket(mut socket: WebSocket, state: Arc<HostState>) {
    let Some(Ok(Message::Text(first))) = socket.next().await else {
        return;
    };
    let hello = match serde_json::from_str::<Value>(&first) {
        Ok(frame) => frame,
        Err(_) => {
            let _ = send_error(&mut socket, None, "invalid_json", "Invalid JSON frame").await;
            return;
        }
    };
    let client_id = match hello.get("clientId").and_then(Value::as_str) {
        Some(value) if !value.is_empty() => value.to_owned(),
        _ => {
            let _ = send_error(
                &mut socket,
                None,
                "invalid_client_id",
                "clientId is required",
            )
            .await;
            return;
        }
    };
    if hello.get("clientType").and_then(Value::as_str) == Some("desktop") {
        let Some(capability) = hello.get("desktopCapability").and_then(Value::as_str) else {
            let _ = send_error(
                &mut socket,
                None,
                "unauthenticated",
                "Desktop capability required",
            )
            .await;
            return;
        };
        let configured = state
            .owner_registry
            .lock()
            .ok()
            .and_then(|registry| registry.clone());
        if let Some(registry) = configured {
            if registry.authenticate(capability).is_none() {
                let _ = send_error(
                    &mut socket,
                    None,
                    "unauthenticated",
                    "Desktop capability rejected",
                )
                .await;
                return;
            }
        }
    }
    if hello.get("clientType").and_then(Value::as_str) == Some("remote") {
        let authorized = hello
            .get("deviceToken")
            .and_then(Value::as_str)
            .and_then(|token| state.auth.lock().ok()?.authorize(token).ok())
            .unwrap_or(false);
        if !authorized {
            let _ = send_error(
                &mut socket,
                None,
                "unauthorized_device",
                "Device token rejected",
            )
            .await;
            return;
        }
    }
    let context = match hello.get("clientType").and_then(Value::as_str) {
        Some("desktop") => {
            let capability = hello
                .get("desktopCapability")
                .and_then(Value::as_str)
                .unwrap_or("");
            let Some(registry) = state
                .owner_registry
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
            else {
                let _ = send_error(
                    &mut socket,
                    None,
                    "unauthenticated",
                    "Desktop capability rejected",
                )
                .await;
                return;
            };
            let Some(owner) = registry.authenticate(capability) else {
                let _ = send_error(
                    &mut socket,
                    None,
                    "unauthenticated",
                    "Desktop capability rejected",
                )
                .await;
                return;
            };
            HostClientContext::desktop(
                client_id.clone(),
                owner.clone(),
                registry.owner_current_workspace(&owner),
            )
        }
        Some("remote") => HostClientContext::remote(client_id.clone()),
        Some("browser") | Some("unpaired") => HostClientContext::public(client_id.clone()),
        _ => {
            let _ = send_error(
                &mut socket,
                None,
                "invalid_client_type",
                "Unsupported client type",
            )
            .await;
            return;
        }
    };
    let handshake = state
        .router
        .lock()
        .map_err(|_| "Host router unavailable".to_string())
        .and_then(|mut router| {
            router
                .connect(&client_id, &hello, context)
                .map_err(|error| error.message)
        });
    if let Err(message) = handshake {
        let _ = send_error(&mut socket, None, "handshake_rejected", &message).await;
        return;
    }
    if socket
        .send(Message::Text(
            json!({ "type": "hello_ack", "protocolVersion": PROTOCOL_VERSION })
                .to_string()
                .into(),
        ))
        .await
        .is_err()
    {
        return;
    }

    let mut runtime_events = state.runtimes.subscribe();
    let mut host_events = state.host_events.subscribe();
    let mut subscriptions = HashSet::new();
    let client_context = state
        .router
        .lock()
        .ok()
        .and_then(|router| router.client_context(&client_id).cloned());

    // Greet desktop clients with the owner-scoped bootstrap frame (workspace
    // generation + live ephemeral instances) before the request loop starts.
    // Terminal/Git/UI surfaces gate on this frame's workspaceGeneration.
    if let Some(context) = client_context
        .as_ref()
        .filter(|context| context.kind == crate::host_router::ClientKind::Desktop)
    {
        if let Some(owner) = context.owner_id.as_ref() {
            let hub = state
                .ephemeral_hub
                .lock()
                .ok()
                .and_then(|slot| slot.clone());
            if let Some(hub) = hub {
                let generation = state
                    .owner_registry
                    .lock()
                    .ok()
                    .and_then(|registry| registry.clone())
                    .and_then(|registry| registry.current_workspace_generation(owner))
                    .unwrap_or(0);
                let _ = send_checked(
                    &mut socket,
                    hub.bootstrap_value(owner, generation),
                    PayloadKind::Event,
                )
                .await;
            }
        }
    }

    // Detached Git commits can finish while browser socket is disconnected.
    // Replay outcomes only to same desktop owner and current workspace generation;
    // this preserves owner isolation without restoring broker-side routing.
    if let Some(context) = client_context
        .as_ref()
        .filter(|context| context.kind == crate::host_router::ClientKind::Desktop)
    {
        if let (Some(owner), Some(service), Some(registry)) = (
            context.owner_id.as_ref(),
            state.git_service.lock().ok().and_then(|slot| slot.clone()),
            state
                .owner_registry
                .lock()
                .ok()
                .and_then(|slot| slot.clone()),
        ) {
            if let Some((root, _)) = registry.current_workspace(owner) {
                if let Some(generation) = registry.current_workspace_generation(owner) {
                    for outcome in service.take_pending_outcomes(owner.as_str(), &root, generation)
                    {
                        if !send_checked(
                            &mut socket,
                            json!({
                                "type": "git_commit_result",
                                "requestId": outcome.request_id,
                                "workspaceGeneration": outcome.generation,
                                "status": outcome.status,
                                "commitOid": outcome.commit_oid,
                                "hookChangedTree": outcome.hook_changed_tree,
                                "error": outcome.error,
                            }),
                            PayloadKind::Event,
                        )
                        .await
                        {
                            return;
                        }
                    }
                }
            }
        }
    }
    // One writer owns the socket. Routed requests run independently so a slow
    // Pi RPC cannot block sidebar data-plane reads or runtime event delivery.
    let (mut socket_sink, mut socket_stream) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            if socket_sink.send(message).await.is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            incoming = socket_stream.next() => {
                let Some(Ok(message)) = incoming else { break };
                let Message::Text(text) = message else {
                    if matches!(message, Message::Close(_)) { break; }
                    continue;
                };
                let frame = match serde_json::from_str::<Value>(&text) {
                    Ok(frame) => frame,
                    Err(_) => {
                        let _ = outgoing_tx.send(outbound_message(
                            structured_error(None, "invalid_json", "Invalid JSON frame"),
                            PayloadKind::Response,
                        ));
                        continue;
                    }
                };
                let request_id = frame
                    .get("requestId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                // Owner-scoped ephemeral RPC frames bypass the router: the hub
                // derives the owner from the authenticated context and routes
                // to the instance runtime. Responses flow back as
                // `ephemeral_event` frames on the owner's event stream.
                if frame.get("type").and_then(Value::as_str) == Some("ephemeral_command") {
                    // Off the socket loop: a parked forward_command (slow Pi
                    // RPC up to its 30s bound) must not stall data-plane
                    // reads or event delivery for the rest of this socket.
                    let state = Arc::clone(&state);
                    let context = client_context.clone();
                    let outgoing_tx = outgoing_tx.clone();
                    let request_id = request_id.clone();
                    tokio::spawn(async move {
                        let hub = state
                            .ephemeral_hub
                            .lock()
                            .ok()
                            .and_then(|slot| slot.clone());
                        let outcome = match (hub, context.as_ref()) {
                            (Some(hub), Some(context))
                                if context.kind == crate::host_router::ClientKind::Desktop =>
                            {
                                let owner = context
                                    .owner_id
                                    .as_ref()
                                    .ok_or_else(|| {
                                        "ephemeral transport requires an authenticated owner"
                                            .to_string()
                                    });
                                match owner {
                                    Ok(owner) => {
                                        let instance_id = frame
                                            .get("ephemeralInstanceId")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string();
                                        let generation = frame
                                            .get("generation")
                                            .and_then(Value::as_u64)
                                            .unwrap_or(0);
                                        let command_request_id = frame
                                            .get("requestId")
                                            .and_then(Value::as_str)
                                            .unwrap_or("");
                                        let payload =
                                            frame.get("payload").cloned().unwrap_or(Value::Null);
                                        hub.forward_command(
                                            &state.runtimes,
                                            owner,
                                            &instance_id,
                                            generation,
                                            payload,
                                            command_request_id,
                                        )
                                        .await
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            _ => Err("ephemeral transport requires a desktop owner".to_string()),
                        };
                        if let Err(error) = outcome {
                            let _ = outgoing_tx.send(outbound_message(
                                json!({
                                    "type": "ephemeral_command_failed",
                                    "requestId": request_id,
                                    "error": error,
                                }),
                                PayloadKind::Response,
                            ));
                        }
                    });
                    continue;
                }
                // Terminal PTY lifecycle: owner-scoped `terminal_command`
                // envelopes carry their payload inline; the manager answers
                // synchronously (terminal_listed/created/...) and streams
                // output later through the owner event channel.
                if frame.get("type").and_then(Value::as_str) == Some("terminal_command") {
                    let manager = state
                        .terminal_manager
                        .lock()
                        .ok()
                        .and_then(|slot| slot.clone());
                    let outcome = match (manager, client_context.as_ref()) {
                        (Some(manager), Some(context))
                            if context.kind == crate::host_router::ClientKind::Desktop =>
                        {
                            let Some(owner) = context.owner_id.as_ref() else {
                                break;
                            };
                            let root = state
                                .owner_registry
                                .lock()
                                .ok()
                                .and_then(|registry| registry.clone())
                                .and_then(|registry| {
                                    // Terminals are workspace-scoped: Registered-only;
                                    // a landing placeholder root never hosts a PTY.
                                    match registry.owner_current_workspace(owner) {
                                        crate::window_owner::OwnerWorkspaceSnapshot::Registered {
                                            root,
                                            ..
                                        } => Some(root),
                                        _ => None,
                                    }
                                });
                            match root {
                                Some(root) => {
                                    let payload =
                                        frame.get("payload").cloned().unwrap_or(Value::Null);
                                    manager.dispatch(owner, &root, &payload)
                                }
                                None => Err(
                                    "terminal commands require a registered workspace".to_string(),
                                ),
                            }
                        }
                        _ => Err("terminal transport requires a desktop owner".to_string()),
                    };
                    let outgoing = match outcome {
                        Ok(mut value) => {
                            if let Some(object) = value.as_object_mut() {
                                object.insert(
                                    "requestId".into(),
                                    Value::String(request_id.clone().unwrap_or_default()),
                                );
                            }
                            value
                        }
                        Err(error) => json!({
                            "type": "terminal_command_failed",
                            "requestId": request_id,
                            "error": error,
                        }),
                    };
                    if outgoing_tx.send(outbound_message(outgoing, PayloadKind::Response)).is_err() {
                        break;
                    }
                    continue;
                }
                let routed = state
                    .router
                    .lock()
                    .map_err(|_| ("router_unavailable", "Host router unavailable".to_string()))
                    .and_then(|router| {
                        router
                            .route(&client_id, &frame)
                            .map_err(|error| (error.code, error.message))
                    });
                let mut after_response = Vec::new();
                let progress_frames = Arc::new(Mutex::new(Vec::<Value>::new()));
                let progress_sink = {
                    let progress_frames = Arc::clone(&progress_frames);
                    let request_id = request_id.clone();
                    Arc::new(move |data: Value| {
                        if let Ok(mut frames) = progress_frames.lock() {
                            frames.push(json!({
                                "type": "control_progress",
                                "requestId": request_id,
                                "data": data,
                            }));
                        }
                    }) as ProgressSink
                };
                let response = match routed {
                    Ok(RoutedAction::Subscribe { request_id, target, .. }) => {
                        match serde_json::from_value::<RuntimeTarget>(target) {
                            Ok(requested) => {
                                let target = state.runtimes.target_for_session(&requested.workspace_id, &requested.session_id);
                                // Any authenticated desktop owner may watch a live
                                // runtime's activity (sidebar green/blue dots across
                                // workspaces); command admission stays authorize_target's
                                // job in runtime_request dispatch.
                                let desktop_owner = client_context.as_ref().is_some_and(|context| {
                                    context.kind == crate::host_router::ClientKind::Desktop
                                        && context.owner_id.is_some()
                                });
                                if let Some(target) = target.filter(|target| desktop_owner && runtime_target_is_live(&state, target)) {
                                subscriptions.insert(target.clone());
                                // Pending blocking dialogs replay only to the
                                // subscriber whose workspace generation still matches
                                // (upstream extension_ui_requires_owner semantics).
                                if authorize_target(&state, client_context.as_ref(), &target) {
                                    if let Ok(pending) = state.runtimes.pending_extension_ui(&target) {
                                        after_response.extend(pending.into_iter().map(runtime_event_frame));
                                    }
                                }
                                Ok(json!({ "type": "runtime_subscribed", "requestId": request_id }))
                                } else {
                                    Err(("unauthorized_target", "Runtime target is not authorized".into()))
                                }
                            }
                            Err(_) => Err(("invalid_target", "Runtime target is invalid".into())),
                        }
                    }
                    Ok(action) => {
                        let state = Arc::clone(&state);
                        let context = client_context.clone();
                        let outgoing_tx = outgoing_tx.clone();
                        // Snapshot-scale replies (get_tree) must share the
                        // Snapshot payload limit, so capture the command type
                        // before dispatch consumes the action.
                        let command_type = match &action {
                            RoutedAction::Runtime { frame, .. } => frame
                                .pointer("/command/type")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            _ => None,
                        };
                        let trace_get_tree = command_type.as_deref() == Some("get_tree");
                        let t0 = std::time::Instant::now();
                        tokio::spawn(async move {
                            let response = dispatch(action, &state, context.as_ref(), progress_sink).await;
                            for progress in progress_frames.lock().map(|frames| frames.clone()).unwrap_or_default() {
                                let _ = outgoing_tx.send(outbound_message(progress, PayloadKind::Progress));
                            }
                            let outgoing = match response {
                                Ok(value) => value,
                                Err((code, message)) => {
                                    structured_error(request_id.as_deref(), code, &message)
                                }
                            };
                            let kind = outbound_payload_kind(
                                outgoing.get("type").and_then(Value::as_str),
                                command_type.as_deref(),
                            );
                            if trace_get_tree {
                                log::info!(
                                    "[get_tree] dispatched: requestId={:?} elapsed={}ms size={}B kind={:?}",
                                    request_id,
                                    t0.elapsed().as_millis(),
                                    outgoing.to_string().len(),
                                    kind
                                );
                            }
                            let _ = outgoing_tx.send(outbound_message(outgoing, kind));
                        });
                        continue;
                    }
                    Err((code, message)) => Err((code, message)),
                };
                let outgoing = match response {
                    Ok(value) => value,
                    Err((code, message)) => structured_error(request_id.as_deref(), code, &message),
                };
                let outbound_kind = match outgoing.get("type").and_then(Value::as_str) {
                    Some("runtime_snapshot") => PayloadKind::Snapshot,
                    Some("control_progress") => PayloadKind::Progress,
                    Some("runtime_event") | Some("event_sequence_gap") => PayloadKind::Event,
                    _ => PayloadKind::Response,
                };
                if outgoing_tx.send(outbound_message(outgoing, outbound_kind)).is_err() {
                    break;
                }
                for replay in after_response {
                    if outgoing_tx.send(outbound_message(replay, PayloadKind::Event)).is_err() {
                        return;
                    }
                }
            }
            host_event = host_events.recv() => {
                match host_event {
                    Ok(event)
                        if client_context.as_ref().is_some_and(|context| {
                            context.kind == crate::host_router::ClientKind::Desktop
                                && event.owner.as_ref().is_none_or(|owner| {
                                    context.owner_id.as_ref() == Some(owner)
                                })
                        }) => {
                            if outgoing_tx.send(outbound_message(event.value, PayloadKind::Event)).is_err() {
                                break;
                            }
                        }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            event = runtime_events.recv() => {
                match event {
                    Ok(event) if subscriptions.contains(&event.target) => {
                        // Blocking dialogs stay scoped to the authorize_target-passing
                        // subscriber (upstream extension_ui_requires_owner): a page
                        // viewing another workspace never pops the old workspace's
                        // questionnaire. Every other event flows to any subscribed
                        // desktop owner so background agent activity keeps the
                        // sidebar's streaming/unread dots live across workspaces. The
                        // subscription itself outlives a workspace transition.
                        if extension_ui_requires_owner(&event.event)
                            && !authorize_target(&state, client_context.as_ref(), &event.target)
                        {
                            continue;
                        }
                        let outgoing = runtime_event_frame(event);
                        if outgoing_tx.send(outbound_message(outgoing, PayloadKind::Event)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let outgoing = structured_error(
                            None,
                            "event_sequence_gap",
                            "Runtime events were missed; request a snapshot",
                        );
                        if outgoing_tx.send(outbound_message(outgoing, PayloadKind::Event)).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    drop(outgoing_tx);
    let _ = writer.await;
    if let Ok(mut router) = state.router.lock() {
        router.disconnect(&client_id);
    }
}

fn runtime_event_frame(event: crate::native_pi_manager::NativeRuntimeEvent) -> Value {
    json!({
        "type": "runtime_event",
        "target": event.target,
        "sequence": event.sequence,
        "event": event.event,
    })
}

fn runtime_target_is_live(state: &HostState, target: &RuntimeTarget) -> bool {
    // Wire targets carry only the routing triple; the live entry is the
    // authority. Matching the triple against the resolved runtime is enough —
    // owner/generation equality is authorize_target's job, host-side.
    state
        .runtimes
        .target_for_session_id(&target.session_id)
        .is_some_and(|live| {
            live.workspace_id == target.workspace_id && live.instance_id == target.instance_id
        })
}

/// Blocking dialogs demand an answer only the owning window can give; they
/// must never surface in a page that is viewing another workspace (upstream
/// `extension_ui_requires_owner`). Non-blocking UI payloads (`setWidget`,
/// `notify`) are ordinary subscribed events.
fn extension_ui_requires_owner(event: &Value) -> bool {
    if event.get("type").and_then(Value::as_str) != Some("extension_ui_request") {
        return false;
    }
    matches!(
        event.get("method").and_then(Value::as_str),
        Some("select" | "confirm" | "input" | "editor")
    )
}

fn current_registered_context(state: &HostState, context: &HostClientContext) -> bool {
    let Some(owner) = context.owner_id.as_ref() else {
        return false;
    };
    let current = state
        .owner_registry
        .lock()
        .ok()
        .and_then(|registry| registry.clone())
        .map(|registry| registry.owner_current_workspace(owner));
    matches!(
        current,
        Some(crate::window_owner::OwnerWorkspaceSnapshot::Registered {
            wid,
            generation,
            ..
        }) if context.kind == crate::host_router::ClientKind::Desktop
            && context.workspace_id.as_deref() == Some(wid.as_str())
            && context.workspace_generation == Some(generation)
    )
}

/// Authority snapshot captured at admission for a long-running preview: the
/// workspace binding may be revoked (transition committed) while the request
/// waits for a permit or converts.
#[derive(Clone)]
struct PreviewScope {
    owner: crate::window_owner::OwnerId,
    workspace_id: String,
    generation: u64,
}

fn preview_scope_from(context: &HostClientContext) -> Option<PreviewScope> {
    Some(PreviewScope {
        owner: context.owner_id.clone()?,
        workspace_id: context.workspace_id.clone()?,
        generation: context.workspace_generation?,
    })
}

fn preview_scope_valid(state: &HostState, scope: &PreviewScope) -> bool {
    state
        .owner_registry
        .lock()
        .ok()
        .and_then(|registry| registry.clone())
        .map(|registry| registry.owner_current_workspace(&scope.owner))
        .is_some_and(|snapshot| {
            matches!(
                snapshot,
                crate::window_owner::OwnerWorkspaceSnapshot::Registered {
                    wid,
                    generation,
                    ..
                } if wid == scope.workspace_id && generation == scope.generation
            )
        })
}

/// Native Office preview: the candidate branch of `file_read` (spec
/// 2026-09-17). Two permits cap concurrent in-memory parsing; the captured
/// PreviewScope is revalidated before waiting, after the permit lands, and
/// after conversion — a workspace transition mid-flight returns
/// `unauthorized_target` and discards the result. In-process parsing has no
/// hard cancellation: a browser abort only ignores the response; the permit
/// leaves with the blocking closure.
#[allow(clippy::too_many_arguments)]
async fn convert_office_preview(
    state: &HostState,
    context: Option<&HostClientContext>,
    request_id: &str,
    root: &std::path::Path,
    path: &str,
) -> Result<Value, (&'static str, String)> {
    let context = context.ok_or(("unauthenticated", "Authentication required".into()))?;
    let scope = preview_scope_from(context)
        .ok_or(("unauthorized_target", "Workspace is not authorized".into()))?;
    if !preview_scope_valid(state, &scope) {
        return Err(("unauthorized_target", "Workspace is not authorized".into()));
    }
    let permit = Arc::clone(&state.preview_permits)
        .acquire_owned()
        .await
        .map_err(|_| ("host_operation_failed", "Preview permits closed".into()))?;
    if !preview_scope_valid(state, &scope) {
        return Err(("unauthorized_target", "Workspace is not authorized".into()));
    }
    let root = root.to_path_buf();
    let relative = path.to_owned();
    let relative_for_response = relative.clone();
    let result = tokio::task::spawn_blocking(move || {
        // The permit moves into the closure: read, detect, and parse form
        // one resource unit, released only when the buffers leave scope.
        let _permit = permit;
        match crate::host_files::read_with_cap(
            &root,
            &relative,
            crate::anydoc_preview::PREVIEW_INPUT_CAP_BYTES,
        ) {
            Ok(content) => {
                match crate::anydoc_preview::convert_candidate(&content.bytes, &relative) {
                    crate::anydoc_preview::PreviewOutcome::Ready(markdown) => {
                        PreviewTaskResult::Ready {
                            markdown,
                            modified_at_ms: content.modified_at_ms,
                        }
                    }
                    crate::anydoc_preview::PreviewOutcome::Failed(code) => {
                        PreviewTaskResult::Failed(code)
                    }
                }
            }
            // An input-cap overrun is a preview failure, not a generic
            // file_too_large; every other file error keeps its code.
            Err(crate::host_files::FileError::TooLarge) => {
                PreviewTaskResult::Failed(crate::anydoc_preview::PreviewErrorCode::InputTooLarge)
            }
            Err(error) => PreviewTaskResult::FileError(error.code()),
        }
    })
    .await
    .map_err(|_| ("host_operation_failed", "Preview task failed".into()))?;
    if !preview_scope_valid(state, &scope) {
        // A transition landed mid-flight: discard the result unread.
        return Err(("unauthorized_target", "Workspace is not authorized".into()));
    }
    match result {
        PreviewTaskResult::Ready {
            markdown,
            modified_at_ms,
        } => Ok(json!({
            "type": "data_response",
            "requestId": request_id,
            "operation": "file_read",
            "path": relative_for_response,
            "content": markdown,
            "mtimeMs": modified_at_ms,
            "isBinary": false,
            "truncated": false,
            "editable": false,
            "previewStatus": "ready",
            "renderAs": "markdown",
        })),
        PreviewTaskResult::Failed(code) => {
            // The fixed closed code is the only permitted log detail.
            log::debug!("office preview failed: {code:?}");
            Ok(json!({
            "type": "data_response",
            "requestId": request_id,
            "operation": "file_read",
            "path": relative_for_response,
            "previewStatus": "conversionFailed",
            "editable": false,
            }))
        }
        PreviewTaskResult::FileError(code) => Err((code, "File read failed".to_owned())),
    }
}

enum PreviewTaskResult {
    Ready {
        markdown: String,
        modified_at_ms: u128,
    },
    Failed(crate::anydoc_preview::PreviewErrorCode),
    FileError(&'static str),
}

fn dialog_response_allowed(context: Option<&HostClientContext>, live_owner: Option<&str>) -> bool {
    let Some(context) = context else { return false };
    let Some(owner) = context.owner_id.as_ref() else {
        return false;
    };
    context.kind == crate::host_router::ClientKind::Desktop && live_owner == Some(owner.as_str())
}

/// Wire frames carry only the routing triple (workspace/session/instance).
/// Owner binding and workspace generation never cross the wire: admission
/// derives them from the live runtime entry and the owner registry, so a
/// browser cannot claim ownership by echoing fields it was never told.
fn authorize_target(
    state: &HostState,
    context: Option<&HostClientContext>,
    target: &RuntimeTarget,
) -> bool {
    let Some(context) = context else {
        return false;
    };
    let Some(owner) = context.owner_id.as_ref() else {
        return false;
    };
    if context.kind != crate::host_router::ClientKind::Desktop {
        return false;
    }
    let Some(live) = state.runtimes.target_for_session_id(&target.session_id) else {
        return false;
    };
    if live.workspace_id != target.workspace_id || live.instance_id != target.instance_id {
        return false;
    }
    if live.owner_id.as_deref() != Some(owner.as_str()) {
        return false;
    }
    // Re-read registry authority for every admission. Handshake context is only
    // an identity proof; workspace/generation can change while socket remains open.
    let current = state
        .owner_registry
        .lock()
        .ok()
        .and_then(|registry| registry.clone())
        .map(|registry| registry.owner_current_workspace(owner));
    matches!(
        current,
        Some(crate::window_owner::OwnerWorkspaceSnapshot::Registered {
            wid,
            generation,
            ..
        }) if wid == live.workspace_id && generation == live.workspace_generation
    )
}

fn operation_scope(
    context: Option<&HostClientContext>,
    session_id: &str,
) -> Result<crate::operation_registry::OperationScope, (&'static str, String)> {
    let context = context.ok_or(("unauthenticated", "Authentication required".into()))?;
    let owner = context.owner_id.as_ref().ok_or((
        "unauthorized_target",
        "Registered desktop owner required".into(),
    ))?;
    let workspace = context.workspace_id.as_ref().ok_or((
        "not_registered",
        "Temporary workspace has no registered target".into(),
    ))?;
    let generation = context.workspace_generation.ok_or((
        "stale_generation",
        "Workspace generation is unavailable".into(),
    ))?;
    Ok(crate::operation_registry::OperationScope::new(
        owner.as_str(),
        workspace,
        session_id,
        generation,
    ))
}

/// Wire name for a workspace-file mutation's registry verdict. A pending
/// duplicate is an error the caller answers with `duplicate_pending`; the two
/// successful verdicts are reported as an `acceptance` field on the response.
fn file_mutation_acceptance(
    acceptance: crate::operation_registry::OperationAcceptance,
) -> Result<&'static str, (&'static str, String)> {
    match acceptance {
        crate::operation_registry::OperationAcceptance::Accepted => Ok("accepted_pending"),
        crate::operation_registry::OperationAcceptance::DuplicatePending => Err((
            "duplicate_pending",
            "File mutation is still pending for this idempotency key".into(),
        )),
        crate::operation_registry::OperationAcceptance::DuplicateCompleted => {
            Ok("duplicate_completed")
        }
    }
}

fn read_bounded_utf8(path: &Path) -> Result<String, &'static str> {
    let file = fs::File::open(path).map_err(|_| "config_not_found")?;
    let mut bytes = Vec::new();
    use std::io::Read;
    file.take(crate::host_config::MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "config_not_found")?;
    if bytes.len() > crate::host_config::MAX_CONFIG_BYTES {
        return Err("config_too_large");
    }
    String::from_utf8(bytes).map_err(|_| "config_invalid_encoding")
}

async fn dispatch(
    action: RoutedAction,
    state: &HostState,
    context: Option<&HostClientContext>,
    progress: ProgressSink,
) -> Result<Value, (&'static str, String)> {
    match action {
        RoutedAction::OperationStatus {
            request_id,
            operation_id,
            ..
        } => {
            let context = context.ok_or(("unauthenticated", "Authentication required".into()))?;
            if context.kind != crate::host_router::ClientKind::Desktop {
                return Err((
                    "forbidden_class",
                    "Only desktop owner may query operations".into(),
                ));
            }
            let owner = context.owner_id.as_ref().ok_or((
                "unauthorized_target",
                "Registered desktop owner required".into(),
            ))?;
            let workspace = context.workspace_id.as_ref().ok_or((
                "not_registered",
                "Temporary workspace has no registered target".into(),
            ))?;
            let generation = context.workspace_generation.ok_or((
                "stale_generation",
                "Workspace generation is unavailable".into(),
            ))?;
            if !current_registered_context(state, context) {
                return Err(("stale_generation", "Workspace generation is stale".into()));
            }
            let record = state
                .runtimes
                .operation_status_for_context(&operation_id, owner.as_str(), workspace, generation)
                .map_err(|_| {
                    (
                        "operation_not_found",
                        "Operation is not visible to this owner".into(),
                    )
                })?;
            Ok(json!({
                "type": "operation_status_response",
                "requestId": request_id,
                "operationId": operation_id,
                "state": record.state,
                "crashReason": record.crash_reason,
                "response": record.terminal_response,
            }))
        }
        RoutedAction::Runtime {
            request_id, frame, ..
        } => {
            let target_value = frame
                .get("target")
                .cloned()
                .ok_or(("invalid_target", "Runtime target is required".into()))?;
            let requested_target: RuntimeTarget = serde_json::from_value(target_value)
                .map_err(|_| ("invalid_target", "Runtime target is invalid".into()))?;
            if !authorize_target(state, context, &requested_target)
                || !runtime_target_is_live(state, &requested_target)
            {
                // Admission diagnostics: the wire triple plus what the host
                // resolved it to, so a rejected frame is attributable without
                // guessing which check failed.
                let resolved = state
                    .runtimes
                    .target_for_session_id(&requested_target.session_id);
                log::warn!(
                    "[runtime-admission] rejected: session={} workspace={} instance={} authenticated_owner={} live={}",
                    requested_target.session_id,
                    requested_target.workspace_id,
                    requested_target.instance_id,
                    context
                        .and_then(|ctx| ctx.owner_id.as_ref())
                        .map(|owner| owner.as_str())
                        .unwrap_or("<none>"),
                    resolved
                        .map(|target| format!(
                            "session={} workspace={} instance={} owner={}",
                            target.session_id,
                            target.workspace_id,
                            target.instance_id,
                            target.owner_id.as_deref().unwrap_or("<none>")
                        ))
                        .unwrap_or_else(|| "<none>".to_string())
                );
                return Err((
                    "unauthorized_target",
                    "Runtime target is not authorized".into(),
                ));
            }
            if frame.get("type").and_then(Value::as_str) == Some("runtime_snapshot_request") {
                let session_id = requested_target.session_id.as_str();
                let mut target = state
                    .runtimes
                    .target_for_session_id(session_id)
                    .ok_or(("runtime_not_found", "Runtime session is not running".into()))?;
                let state_response = state
                    .runtimes
                    .request(
                        &target,
                        json!({ "type": "get_state" }),
                        None,
                        // A newly started Pi runtime may need longer than the
                        // ordinary command timeout before its first get_state.
                        // Align with the 30s runtime readiness policy.
                        Duration::from_secs(30),
                    )
                    .await
                    .map_err(|message| ("snapshot_failed", message))?;
                if target.session_id.starts_with("temporary-") {
                    if let Some(formal_session_id) = state_response
                        .pointer("/data/sessionId")
                        .and_then(Value::as_str)
                        .filter(|session_id| !session_id.is_empty())
                    {
                        target = state
                            .runtimes
                            .bind_session_id(&target, formal_session_id)
                            .map_err(|message| ("session_binding_failed", message))?;
                    }
                }
                if let Some(session_file) = state_response
                    .pointer("/data/sessionFile")
                    .and_then(Value::as_str)
                    .filter(|session_file| !session_file.is_empty())
                {
                    match state
                        .data
                        .record_pi_session_bucket(&target.workspace_id, session_file)
                    {
                        Ok(true) => {
                            state.host_events.broadcast_native_event(json!({
                                "type": "registry_changed",
                                "reason": "session_bucket_recorded",
                            }));
                        }
                        Ok(false) => {}
                        Err(error) => {
                            log::warn!(
                                "[picot-host] Pi returned invalid session bucket for workspace {}: {error}",
                                target.workspace_id
                            );
                        }
                    }
                }
                let messages_response = state
                    .runtimes
                    .request(
                        &target,
                        json!({ "type": "get_messages" }),
                        None,
                        Duration::from_secs(10),
                    )
                    .await
                    .map_err(|message| ("snapshot_failed", message))?;
                let host_snapshot = state
                    .runtimes
                    .snapshot(&target)
                    .map_err(|message| ("snapshot_failed", message))?;
                return Ok(json!({
                    "type": "runtime_snapshot",
                    "requestId": request_id,
                    "target": target,
                    "sequence": host_snapshot.sequence,
                    "state": {
                        "lifecycle": host_snapshot.state,
                        "pi": state_response.get("data").cloned().unwrap_or(Value::Null),
                        "messages": messages_response.pointer("/data/messages").cloned().unwrap_or_else(|| json!([])),
                    }
                }));
            }
            if frame.get("type").and_then(Value::as_str) == Some("runtime_capabilities_request") {
                return Ok(json!({
                    "type": "runtime_capabilities",
                    "requestId": request_id,
                    "protocolVersion": PROTOCOL_VERSION,
                    "nativeRpc": true,
                    "extensionUi": true,
                    "sessionTree": true,
                    "oauth": false,
                    "hostDataPlane": true,
                    "sourcePreservingFork": false,
                }));
            }
            if frame.get("type").and_then(Value::as_str) != Some("runtime_request") {
                return Err((
                    "unsupported_runtime_request",
                    "Unsupported runtime request".into(),
                ));
            }
            let target = requested_target;
            let command = frame
                .get("command")
                .cloned()
                .ok_or(("invalid_command", "Runtime command is required".into()))?;
            let scope = operation_scope(context, &target.session_id)?;
            if command.get("type").and_then(Value::as_str) == Some("extension_ui_response") {
                // Ownership and the full routing target are resolved host-side;
                // the wire target carries only the routing triple, so it can
                // never equal the coordinator's registered target (owner Some,
                // generation N) — the live target must bridge the response.
                let live_target = state.runtimes.target_for_session_id(&target.session_id);
                if !dialog_response_allowed(
                    context,
                    live_target
                        .as_ref()
                        .and_then(|live| live.owner_id.as_deref()),
                ) {
                    return Err((
                        "dialog_response_forbidden",
                        "Only the current workspace owner may answer this dialog".into(),
                    ));
                }
                let Some(live_target) = live_target else {
                    return Err((
                        "dialog_response_failed",
                        "No running runtime for this session".into(),
                    ));
                };
                state
                    .runtimes
                    .respond_extension_ui(&live_target, command)
                    .await
                    .map_err(|message| ("dialog_response_failed", message))?;
                return Ok(json!({
                    "type": "runtime_response",
                    "requestId": request_id,
                    "acceptance": "duplicate_completed",
                    "operationId": format!("dialog-{request_id}"),
                    "response": { "success": true },
                }));
            }
            if command.get("type").and_then(Value::as_str) == Some("abort") {
                let response = state
                    .runtimes
                    .abort_turn(&target, &scope, Duration::from_secs(30))
                    .await
                    .map_err(|message| ("runtime_request_failed", message))?;
                return Ok(
                    json!({ "type": "runtime_response", "requestId": request_id, "acceptance": "accepted_pending", "operationId": format!("abort-{request_id}"), "response": response }),
                );
            }
            let key = frame.get("idempotencyKey").and_then(Value::as_str).ok_or((
                "idempotency_key_required",
                "Runtime mutations require idempotencyKey".into(),
            ))?;
            let (operation_id, acceptance, response) = state
                .runtimes
                .request_scoped_receipt(&target, scope, command, key, Duration::from_secs(30))
                .await
                .map_err(|message| ("runtime_request_failed", message))?;
            let acceptance = match acceptance {
                crate::operation_registry::OperationAcceptance::Accepted => "accepted_pending",
                crate::operation_registry::OperationAcceptance::DuplicatePending => {
                    "duplicate_pending"
                }
                crate::operation_registry::OperationAcceptance::DuplicateCompleted => {
                    "duplicate_completed"
                }
            };
            Ok(json!({
                "type": "runtime_response", "requestId": request_id,
                "acceptance": acceptance, "operationId": operation_id,
                "response": response,
            }))
        }
        // Pairing tokens are minted by the desktop only (`mobile_pairing_create`
        // control): an unpaired-browser mint would let any LAN device self-pair
        // the moment LAN bind is enabled, so no auth_request operation remains.
        RoutedAction::Auth { .. } => Err((
            "unknown_auth_operation",
            "Unsupported auth operation".into(),
        )),
        RoutedAction::Host {
            client_id,
            request_id,
            operation,
            frame,
        } => {
            let context = context.ok_or(("unauthenticated", "Authentication required".into()))?;
            // Control operations are owner-scoped: the capability proves the
            // desktop identity, and every op re-derives its authority from the
            // live registry (native-owner registry ops, Registered-only
            // workspace ops, Quick Chat admission). The landing owner is
            // unbound by design, so requiring a Registered snapshot here would
            // lock the whole control surface behind the very state landing
            // exists to move past.
            if context.kind != crate::host_router::ClientKind::Desktop || context.owner_id.is_none()
            {
                return Err((
                    "stale_generation",
                    "Verified desktop workspace context is stale".into(),
                ));
            }
            let owner = context.owner_id.clone().ok_or((
                "unauthorized_target",
                "Registered desktop owner required".into(),
            ))?;
            if matches!(
                operation.as_str(),
                "mcp_login_start"
                    | "mcp_login_cancel"
                    | "mcp_login_status"
                    | "mcp_logout"
                    | "mcp_server_status"
            ) {
                let workspace_id = context
                    .workspace_id
                    .as_deref()
                    .ok_or(("not_registered", "Registered workspace required".to_owned()))?;
                let registry = state
                    .owner_registry
                    .lock()
                    .map_err(|_| ("auth_unavailable", "Owner registry unavailable".to_owned()))?
                    .clone()
                    .ok_or(("auth_unavailable", "Owner registry unavailable".to_owned()))?;
                if !matches!(registry.owner_current_workspace(&owner), crate::window_owner::OwnerWorkspaceSnapshot::Registered { wid, generation, .. } if wid == workspace_id && Some(generation) == context.workspace_generation)
                {
                    return Err(("stale_generation", "Desktop workspace changed".into()));
                }
                let cwd = state
                    .data
                    .workspace_root(workspace_id)
                    .map_err(host_data_error)?;
                let args = frame.get("args").unwrap_or(&Value::Null);
                let runner = state.mcp_login.clone();
                let response = match operation.as_str() {
                    "mcp_login_status" | "mcp_login_cancel" => {
                        let id = args
                            .get("operationId")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .ok_or(("invalid_operation", "operationId is required".into()))?;
                        if operation == "mcp_login_cancel" {
                            runner
                                .cancel(&owner, id)
                                .map(|()| json!({ "ok": true, "cancelled": true }))
                        } else {
                            runner.status(&owner, id)
                        }
                    }
                    "mcp_login_start" => {
                        let result = args
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|name| {
                                !name.is_empty()
                                    && name.len() <= 256
                                    && !name.chars().any(char::is_control)
                            })
                            .ok_or_else(|| "Valid MCP server name is required".to_owned())
                            .and_then(|name| {
                                crate::pi_launch::resolve_bundled_pi(&state.static_dir)
                                    .and_then(|binary| runner.start(&binary, &cwd, name, &owner))
                            });
                        Ok(match result {
                            Ok(id) => json!({ "ok": true, "operationId": id }),
                            Err(error) => json!({ "ok": false, "error": error }),
                        })
                    }
                    "mcp_logout" => {
                        let name = args
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|name| {
                                !name.is_empty()
                                    && name.len() <= 256
                                    && !name.chars().any(char::is_control)
                            })
                            .ok_or((
                                "invalid_mcp_name",
                                "Valid MCP server name is required".into(),
                            ))?;
                        let binary = crate::pi_launch::resolve_bundled_pi(&state.static_dir)
                            .map_err(|error| ("mcp_unavailable", error))?;
                        let name = name.to_owned();
                        tokio::task::spawn_blocking(move || runner.logout(&binary, &cwd, &name))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|result| result.map(|()| json!({ "ok": true })))
                    }
                    "mcp_server_status" => {
                        // `refresh: true` drops the 60s host cache after a
                        // configuration write; an invalid value is a caller
                        // error, not a silent no-op.
                        let refresh = match args.get("refresh") {
                            None => false,
                            Some(Value::Bool(flag)) => *flag,
                            Some(_) => {
                                return Err((
                                    "invalid_refresh",
                                    "refresh must be a boolean".into(),
                                ));
                            }
                        };
                        let binary = crate::pi_launch::resolve_bundled_pi(&state.static_dir)
                            .map_err(|error| ("mcp_unavailable", error))?;
                        tokio::task::spawn_blocking(move || {
                            if refresh {
                                runner.invalidate();
                            }
                            runner.list(&binary, &cwd)
                        })
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|result| {
                            // The runner already validated and projected the
                            // report: only safe fields and fixed diagnostic
                            // text leave this process.
                            result.map(|report| {
                                let mut response = json!({
                                    "ok": true,
                                    "servers": report.get("servers").cloned().unwrap_or(Value::Array(Vec::new())),
                                    "errors": report.get("errors").cloned().unwrap_or(Value::Array(Vec::new())),
                                });
                                if let Some(note) = report.get("note") {
                                    response["note"] = note.clone();
                                }
                                response
                            })
                        })
                    }
                    _ => unreachable!(),
                };
                return Ok(
                    json!({ "type": "host_response", "requestId": request_id, "operation": operation, "response": response.map_err(|error| ("mcp_operation_failed", error))? }),
                );
            }
            // D4 mobile entry: the desktop mints pairing tokens for phones on
            // the same LAN. Gated on the same preference that drove the bind
            // decision, so a loopback-only host never mints tokens.
            if operation == "mobile_access_info" {
                let port = state.host_port.load(std::sync::atomic::Ordering::Relaxed);
                let lan_urls = if state.lan_access {
                    primary_lan_url(port).into_iter().collect()
                } else {
                    Vec::new()
                };
                return Ok(json!({
                    "type": "host_response",
                    "requestId": request_id,
                    "operation": operation,
                    "response": {
                        "enabled": state.lan_access,
                        "port": port,
                        "lanUrls": lan_urls,
                    },
                }));
            }
            if operation == "mobile_pairing_create" {
                if !state.lan_access {
                    return Err((
                        "mobile_access_disabled",
                        "Enable mobile/LAN access before pairing a device".into(),
                    ));
                }
                let pairing = state
                    .auth
                    .lock()
                    .map_err(|_| ("auth_unavailable", "Remote auth unavailable".into()))?
                    .create_pairing(now_seconds());
                return Ok(json!({
                    "type": "host_response",
                    "requestId": request_id,
                    "operation": operation,
                    "response": {
                        "pairingToken": pairing.token,
                        "expiresAt": pairing.expires_at,
                    },
                }));
            }
            // P4-d: session export — one-shot TTL'd token bound to the owner
            // and the resolved session file.
            if operation == "session_export" {
                let session_id = frame
                    .pointer("/args/sessionId")
                    .and_then(Value::as_str)
                    .ok_or((
                        "invalid_export",
                        "sessionId is required for session_export".to_owned(),
                    ))?;
                let workspace_id = context
                    .workspace_id
                    .as_deref()
                    .ok_or(("not_registered", "Registered workspace required".to_owned()))?;
                let session_path = state
                    .data
                    .session_file_path(workspace_id, session_id)
                    .ok_or(("session_unavailable", "Session file unavailable".to_owned()))?;
                // session_file_path performs identifier validation, bucket
                // membership, canonical containment, and regular-file checks.
                let html_path = session_path.with_extension("html");
                if std::fs::symlink_metadata(&html_path)
                    .map(|metadata| !metadata.file_type().is_file())
                    .unwrap_or(false)
                {
                    return Err((
                        "export_unavailable",
                        "Export target is not a regular file".into(),
                    ));
                }
                let pi = crate::pi_launch::resolve_bundled_pi(&state.static_dir)
                    .map_err(|error| ("export_unavailable", error))?;
                let mut command = Command::new(pi);
                crate::windows_child::hide_console(&mut command);
                let output = command
                    .arg("--export")
                    .arg(&session_path)
                    .arg(&html_path)
                    .current_dir(
                        state
                            .data
                            .workspace_root(workspace_id)
                            .map_err(host_data_error)?,
                    )
                    .output()
                    .map_err(|error| ("export_unavailable", format!("Export failed: {error}")))?;
                if !output.status.success() {
                    return Err(("export_unavailable", "Embedded Pi export failed".into()));
                }
                let bucket = session_path
                    .parent()
                    .ok_or(("export_unavailable", "Session bucket unavailable".into()))?;
                let resolved_html = html_path
                    .canonicalize()
                    .map_err(|_| ("export_unavailable", "Export output unavailable".into()))?;
                if resolved_html.strip_prefix(bucket).is_err() || !resolved_html.is_file() {
                    return Err((
                        "export_unavailable",
                        "Export output escaped session bucket".into(),
                    ));
                }
                let generation = context.workspace_generation.unwrap_or_default();
                let (token, _) = state
                    .session_exports
                    .issue(owner.as_str(), generation, resolved_html)
                    .map_err(|error| ("export_quota", error))?;
                return Ok(json!({
                    "type": "host_response",
                    "requestId": request_id,
                    "operation": "session_export",
                    // Every host op delivers its payload under `response`; the
                    // client resolves the promise with that object. A top-level
                    // field here would silently resolve to undefined.
                    "response": {
                        "exportUrl": format!("/v2/session-export/{token}"),
                        "expiresInSecs": state.session_exports.ttl_secs(),
                    },
                }));
            }
            if matches!(
                operation.as_str(),
                "settings_get" | "settings_put" | "agent_text_file_get" | "agent_text_file_put"
            ) {
                let args = frame.get("args").cloned().unwrap_or_else(|| json!({}));
                let name = args
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("AGENTS.md");
                let scope = args
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or("workspace");
                let root = if scope == "global" {
                    dirs::home_dir()
                        .ok_or((
                            "config_unavailable",
                            "Global config root unavailable".to_owned(),
                        ))?
                        .join(".pi/agent")
                } else {
                    let workspace_id = context
                        .workspace_id
                        .as_deref()
                        .ok_or(("not_registered", "Registered workspace required".to_owned()))?;
                    state
                        .data
                        .workspace_root(workspace_id)
                        .map_err(host_data_error)?
                };
                if name.contains('/')
                    || name.contains('\\')
                    || name.is_empty()
                    || matches!(name, "." | "..")
                {
                    return Err((
                        "invalid_config_path",
                        "Config path is not allowed".to_owned(),
                    ));
                }
                let path = root.join(name);
                let response = match operation.as_str() {
                    "settings_get" => crate::host_config::read_json(&path)
                        .map(|value| json!({ "value": value, "path": path.display().to_string() }))
                        .map_err(|error| (error.code(), "Settings read failed".to_owned()))?,
                    "settings_put" => {
                        let value = args
                            .get("value")
                            .ok_or(("invalid_config", "value is required".to_owned()))?;
                        // Plan §14: model config keeps backup + restart notice.
                        let requires_restart =
                            matches!(name, "models.json" | "agent.json" | "APPEND_SYSTEM.md");
                        if requires_restart && path.exists() {
                            let backup = path.with_extension(format!(
                                "{}.bak",
                                path.extension()
                                    .and_then(|value| value.to_str())
                                    .unwrap_or_default()
                            ));
                            let _ = std::fs::copy(&path, &backup);
                        }
                        crate::host_config::write_json(&path, value)
                            .map_err(|error| (error.code(), "Settings write failed".to_owned()))?;
                        json!({
                            "saved": true,
                            "path": path.display().to_string(),
                            "restartRequired": requires_restart,
                        })
                    }
                    "agent_text_file_get" => {
                        let content = read_bounded_utf8(&path).map_err(|code| match code {
                            "config_too_large" => {
                                (code, "Config file exceeds the text bound".to_owned())
                            }
                            "config_invalid_encoding" => {
                                (code, "Config file is not UTF-8".to_owned())
                            }
                            _ => ("config_not_found", "Config file unavailable".to_owned()),
                        })?;
                        json!({ "content": content, "path": path.display().to_string() })
                    }
                    "agent_text_file_put" => {
                        let content = args
                            .get("content")
                            .and_then(Value::as_str)
                            .ok_or(("invalid_config", "content is required".to_owned()))?;
                        crate::host_config::write_text(&path, content)
                            .map_err(|error| (error.code(), "Config write failed".to_owned()))?;
                        json!({ "saved": true, "path": path.display().to_string() })
                    }
                    _ => unreachable!(),
                };
                return Ok(
                    json!({ "type": "host_response", "requestId": request_id, "operation": operation, "response": response }),
                );
            }
            // Subagents settings controls: four owner-scoped operations whose
            // identity comes only from the live owner registry — the frame
            // may carry scope labels, never roots or workspace identity.
            if matches!(
                operation.as_str(),
                "subagents_inventory"
                    | "subagents_get_detail"
                    | "subagents_create"
                    | "subagents_set_override"
            ) {
                let registry = state
                    .owner_registry
                    .lock()
                    .map_err(|_| ("auth_unavailable", "Owner registry unavailable".into()))?
                    .clone()
                    .ok_or(("auth_unavailable", "Owner registry unavailable".to_owned()))?;
                let before = registry.owner_current_workspace(&owner);
                let agent_root = state
                    .agent_root_override
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone())
                    .map(Ok)
                    .unwrap_or_else(crate::pi_launch::resolve_pi_agent_root)
                    .map_err(|error| ("config_unavailable", error))?;
                let args = frame.get("args").cloned().unwrap_or(Value::Null);
                let scan_operation = operation.clone();
                let authorized_snapshot = before.clone();
                // Bounded disk scans run off the WS worker; blocking reads
                // must never stall the socket loop.
                let result = tokio::task::spawn_blocking(move || {
                    crate::subagents_settings::handle(
                        &scan_operation,
                        &args,
                        &authorized_snapshot,
                        &agent_root,
                    )
                })
                .await
                .map_err(|_| {
                    (
                        "host_operation_failed",
                        "Subagents scan task failed".to_owned(),
                    )
                })?
                .map_err(|error| (error.code(), error.message()))?;
                // The scan raced a workspace transition: only deliver data
                // the binding that authorized it still matches.
                if registry.owner_current_workspace(&owner) != before {
                    return Err((
                        "stale_generation",
                        "Workspace binding changed during the scan".to_owned(),
                    ));
                }
                return Ok(json!({
                    "type": "host_response",
                    "requestId": request_id,
                    "operation": operation,
                    "response": result,
                }));
            }
            let handler = state
                .control_handler
                .lock()
                .map_err(|_| {
                    (
                        "control_unavailable",
                        "Host control handler unavailable".into(),
                    )
                })?
                .clone()
                .ok_or((
                    "control_unavailable",
                    "Host control handler unavailable".into(),
                ))?;
            let client_id = client_id.parse::<u64>().unwrap_or(0);
            let verified = VerifiedClientContext {
                client_id,
                class: crate::host_control::ClientClass::Native,
                owner_id: Some(owner),
            };
            let args = frame.get("args").cloned().unwrap_or(Value::Null);
            let canonical = json!({
                "type": "host_request",
                "requestId": request_id,
                "operation": operation,
                "args": args,
            });
            let result = handler(verified, canonical, progress)
                .await
                .map_err(|message| ("host_operation_failed", message))?;
            Ok(json!({
                "type": "host_response",
                "requestId": request_id,
                "operation": operation,
                "response": result,
            }))
        }
        RoutedAction::Data {
            request_id, frame, ..
        } => {
            let operation = frame.get("operation").and_then(Value::as_str).unwrap_or("");
            let authorized = context.is_some_and(|ctx| {
                if ctx.kind != crate::host_router::ClientKind::Desktop || ctx.owner_id.is_none() {
                    return false;
                }
                // Sidebar registry rows and the cost dashboard are
                // owner-scoped surfaces over the GLOBAL session tree —
                // including from the landing page, whose owner has no
                // workspace binding and whose cost frames carry no workspace
                // target at all. Data reads still resolve roots through
                // MetadataStore; only the current desktop owner may request
                // these operations.
                if matches!(
                    operation,
                    "workspace_sessions"
                        | "cost_dashboard"
                        | "reset_credit_open"
                        | "reset_credit_settle"
                        | "browser_pane_create"
                        | "browser_pane_set_rect"
                        | "browser_pane_set_visible"
                        | "browser_pane_eval"
                        | "browser_pane_navigate"
                        | "browser_pane_url"
                        | "browser_pane_destroy"
                        | "officecli_watch_start"
                        | "officecli_watch_stop"
                        | "officecli_watch_status"
                        | "officecli_watch_mark"
                ) {
                    return true;
                }
                current_registered_context(state, ctx)
                    && ctx.workspace_id.as_deref()
                        == frame.get("workspaceId").and_then(Value::as_str)
            });
            if !authorized {
                return Err(("unauthorized_target", "Workspace is not authorized".into()));
            }
            // Owner-scoped global ops send no workspaceId; every wid-matched
            // op was gated against a real binding above, so the empty default
            // is unreachable for them.
            let workspace_id = frame
                .get("workspaceId")
                .and_then(Value::as_str)
                .unwrap_or("");
            match Some(operation) {
                Some("file_mentions") => {
                    let query = frame
                        .get("query")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    // Contract D: the declared root is required and must equal
                    // what the host itself parses from the query.
                    let declared = frame.get("root").and_then(|root| {
                        let kind = root.get("kind").and_then(Value::as_str)?;
                        let value = root.get("value").and_then(Value::as_str)?;
                        Some((kind.to_string(), value.to_string()))
                    });
                    let Some(declared) = declared else {
                        return Err(host_data_error(HostDataError::InvalidMentionQuery));
                    };
                    // The walk is synchronous fs recursion bounded by the
                    // 500ms/10k budgets — wide roots (`@/`, `@~/`) can spend
                    // the whole budget, so it must not sit on an async worker.
                    let data_plane = state.data.clone();
                    let workspace = workspace_id.to_string();
                    let _permit = mention_scan_permit().await?;
                    let result = tokio::task::spawn_blocking(move || {
                        data_plane.search_file_mentions(
                            &workspace,
                            &query,
                            Some((declared.0.as_str(), declared.1.as_str())),
                        )
                    })
                    .await
                    .map_err(|error| ("mention_search_failed", error.to_string()))?
                    .map_err(host_data_error)?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "file_mentions",
                        "items": result.items,
                        "truncated": result.truncated,
                    }))
                }
                Some("workspace_info") => {
                    let root = state
                        .data
                        .workspace_root(workspace_id)
                        .map_err(host_data_error)?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "workspace_info",
                        "workspaceId": workspace_id,
                        "path": root.to_string_lossy(),
                        "isGit": root.join(".git").exists(),
                    }))
                }
                Some("workspace_sessions") => {
                    let root = state
                        .data
                        .workspace_root(workspace_id)
                        .map_err(host_data_error)?;
                    let count_only = frame
                        .get("countOnly")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let (dir_name, sessions, session_count, hidden_subagent_count) =
                        read_workspace_session_bucket_blocking(
                            state.data.clone(),
                            Arc::clone(&state.session_scan_permits),
                            workspace_id.to_owned(),
                            count_only,
                        )
                        .await
                        .map_err(|_| ("session_scan_failed", "Session scan failed".to_owned()))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "workspace_sessions",
                        "workspaceId": workspace_id,
                        "path": root.to_string_lossy(),
                        "dirName": dir_name,
                        "sessions": sessions,
                        "sessionCount": session_count,
                        "hiddenSubagentCount": hidden_subagent_count,
                    }))
                }
                Some("read_session_messages") => {
                    let session_id = frame
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_session", "sessionId is required".into()))?;
                    let messages = state
                        .data
                        .read_session_messages(workspace_id, session_id)
                        .map_err(host_data_error)?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "read_session_messages",
                        "messages": messages,
                    }))
                }
                Some("read_session_tree") => {
                    let session_id = frame
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_session", "sessionId is required".into()))?;
                    let tree = state
                        .data
                        .read_session_tree(workspace_id, session_id)
                        .map_err(host_data_error)?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "read_session_tree",
                        "tree": tree,
                    }))
                }
                Some("show_task_notification") => {
                    let title = frame
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("Task finished")
                        .to_string();
                    let body = frame
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let session_id = frame
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_session", "sessionId is required".into()))?
                        .to_string();
                    // macOS attributes UN notifications to a bundle identifier.
                    // The host server never sees the Tauri config, so this
                    // mirrors tauri.conf.json's identifier.
                    #[cfg(target_os = "macos")]
                    let _ = notify_rust::set_application(notification_bundle_identifier());
                    let notification = notify_rust::Notification::new()
                        .summary(&title)
                        .body(&body)
                        .show()
                        .map_err(|error| {
                            (
                                "notification_failed",
                                format!("Cannot show task notification: {error}"),
                            )
                        })?;
                    let session_file = state
                        .data
                        .session_file_path(workspace_id, &session_id)
                        .map(|path| path.to_string_lossy().into_owned());
                    let cwd = state
                        .data
                        .workspace_root(workspace_id)
                        .ok()
                        .map(|path| path.to_string_lossy().into_owned());
                    let host_events = state.host_events.clone();
                    let ws_id = workspace_id.to_string();
                    // wait_for_action blocks until the user clicks; one thread
                    // per notification is fine at task-completion volume.
                    std::thread::spawn(move || {
                        notification.wait_for_action(move |action| {
                            if action == "__closed" {
                                return;
                            }
                            host_events.broadcast_native_event(json!({
                                "type": "notification_activated",
                                "workspaceId": ws_id,
                                "sessionId": session_id,
                                "sessionFile": session_file,
                                "cwd": cwd,
                            }));
                        });
                    });
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "show_task_notification",
                    }))
                }
                Some("list_files") => {
                    let workspace_id = frame
                        .get("workspaceId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_workspace", "workspaceId is required".into()))?;
                    let relative_path = frame
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let entries = state
                        .data
                        .list_files(workspace_id, relative_path)
                        .map_err(host_data_error)?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "list_files",
                        "entries": entries,
                    }))
                }
                Some("list_sessions") => {
                    let workspace_id = frame
                        .get("workspaceId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_workspace", "workspaceId is required".into()))?;
                    let sessions = list_sessions_blocking(
                        state.data.clone(),
                        Arc::clone(&state.session_scan_permits),
                        workspace_id.to_owned(),
                    )
                    .await
                    .map_err(|_| {
                        (
                            "session_list_scan_failed",
                            "Session list scan failed".to_owned(),
                        )
                    })?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "list_sessions",
                        "sessions": sessions,
                    }))
                }
                Some("search_sessions") => {
                    let workspace_id = frame
                        .get("workspaceId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_workspace", "workspaceId is required".into()))?;
                    let query = frame
                        .get("query")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let results = search_sessions_blocking(
                        state.data.clone(),
                        Arc::clone(&state.session_scan_permits),
                        workspace_id.to_owned(),
                        query,
                    )
                    .await
                    .map_err(|_| ("search_scan_failed", "Session search failed".to_owned()))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "search_sessions",
                        "results": results,
                    }))
                }
                Some("cost_dashboard") => {
                    // Same P4-parity payload as the /api/cost-dashboard facade:
                    // range/granularity/scope/models shape the aggregation and
                    // the compat keys land at the top level of the frame, which
                    // is what the embedded Cost Dashboard infobar renders.
                    let pairs = ["range", "granularity", "scope", "models"]
                        .iter()
                        .filter_map(|key| {
                            frame
                                .get(*key)
                                .and_then(Value::as_str)
                                .map(|value| ((*key).to_string(), value.to_string()))
                        })
                        .collect::<Vec<(String, String)>>();
                    let params = crate::cost_compat::parse_cost_range_params(&pairs)
                        .ok_or(("invalid_cost_range", "Invalid cost range".into()))?;
                    let payload = state
                        .data
                        .cost_dashboard_compat(workspace_id, &params, chrono::Utc::now())
                        .map_err(host_data_error)?;
                    let mut response = json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "cost_dashboard",
                    });
                    if let (Some(target), Some(source)) =
                        (response.as_object_mut(), payload.as_object())
                    {
                        for (key, value) in source {
                            target.insert(key.clone(), value.clone());
                        }
                    }
                    Ok(response)
                }
                Some("reset_credit_open") => {
                    // Codex reset-credit ledger open (spec 2026-09-22):
                    // same owner-scoped, landing-visible surface as the cost
                    // dashboard; the operation id doubles as the upstream
                    // redeem_request_id idempotency key.
                    let operation_id = state
                        .data
                        .reset_credit_open()
                        .map_err(|error| ("host_operation_failed", error))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "reset_credit_open",
                        "operationId": operation_id,
                    }))
                }
                Some("reset_credit_settle") => {
                    let operation_id = frame
                        .get("operationId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "operationId is required".into()))?;
                    let ambiguous = frame.get("ambiguous") == Some(&Value::Bool(true));
                    state
                        .data
                        .reset_credit_settle(
                            operation_id,
                            if ambiguous { "ambiguous" } else { "settled" },
                        )
                        .map_err(|error| ("host_operation_failed", error))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "reset_credit_settle",
                    }))
                }
                Some("browser_pane_create") => {
                    let panes = state
                        .browser_panes
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "browser panes are not initialized".into()))?;
                    let pane_id = frame
                        .get("paneId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "paneId is required".into()))?;
                    let window_label = frame
                        .get("windowLabel")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "windowLabel is required".into()))?;
                    let url = frame
                        .get("url")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "url is required".into()))?;
                    let num = |key: &str| -> Result<f64, (&'static str, String)> {
                        frame
                            .get(key)
                            .and_then(Value::as_f64)
                            .ok_or(("invalid_operation", format!("{key} is required")))
                    };
                    panes
                        .create(
                            pane_id,
                            window_label,
                            url,
                            crate::browser_pane::PaneRect {
                                x: num("x")?,
                                y: num("y")?,
                                width: num("width")?,
                                height: num("height")?,
                            },
                        )
                        .map_err(|e| ("browser_pane_failed", e))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "browser_pane_create",
                    }))
                }
                Some("browser_pane_set_rect") => {
                    let panes = state
                        .browser_panes
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "browser panes are not initialized".into()))?;
                    let pane_id = frame
                        .get("paneId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "paneId is required".into()))?;
                    let num = |key: &str| -> Result<f64, (&'static str, String)> {
                        frame
                            .get(key)
                            .and_then(Value::as_f64)
                            .ok_or(("invalid_operation", format!("{key} is required")))
                    };
                    panes
                        .set_rect(
                            pane_id,
                            crate::browser_pane::PaneRect {
                                x: num("x")?,
                                y: num("y")?,
                                width: num("width")?,
                                height: num("height")?,
                            },
                        )
                        .map_err(|e| ("browser_pane_failed", e))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "browser_pane_set_rect",
                    }))
                }
                Some("browser_pane_set_visible") => {
                    let panes = state
                        .browser_panes
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "browser panes are not initialized".into()))?;
                    let pane_id = frame
                        .get("paneId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "paneId is required".into()))?;
                    let visible = frame.get("visible") == Some(&Value::Bool(true));
                    panes
                        .set_visible(pane_id, visible)
                        .map_err(|e| ("browser_pane_failed", e))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "browser_pane_set_visible",
                    }))
                }
                Some("browser_pane_eval") => {
                    let panes = state
                        .browser_panes
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "browser panes are not initialized".into()))?;
                    let pane_id = frame
                        .get("paneId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "paneId is required".into()))?;
                    let expression = frame
                        .get("js")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "js is required".into()))?;
                    let result = panes
                        .eval_json(pane_id, expression)
                        .await
                        .map_err(|e| ("browser_pane_failed", e))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "browser_pane_eval",
                        "result": result,
                    }))
                }
                Some("browser_pane_navigate") => {
                    let panes = state
                        .browser_panes
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "browser panes are not initialized".into()))?;
                    let pane_id = frame
                        .get("paneId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "paneId is required".into()))?;
                    let url = frame
                        .get("url")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "url is required".into()))?;
                    panes
                        .navigate(pane_id, url)
                        .map_err(|e| ("browser_pane_failed", e))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "browser_pane_navigate",
                    }))
                }
                Some("browser_pane_url") => {
                    let panes = state
                        .browser_panes
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "browser panes are not initialized".into()))?;
                    let pane_id = frame
                        .get("paneId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "paneId is required".into()))?;
                    let url = panes.url(pane_id).map_err(|e| ("browser_pane_failed", e))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "browser_pane_url",
                        "url": url,
                    }))
                }
                Some("browser_pane_destroy") => {
                    let panes = state
                        .browser_panes
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "browser panes are not initialized".into()))?;
                    let pane_id = frame
                        .get("paneId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "paneId is required".into()))?;
                    let _ = panes.destroy(pane_id);
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "browser_pane_destroy",
                    }))
                }
                Some("officecli_watch_start") => {
                    let watches = state
                        .officecli_watches
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "officecli watch is not initialized".into()))?;
                    let file = frame
                        .get("file")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "file is required".into()))?
                        .to_string();
                    let url = tokio::task::spawn_blocking(move || {
                        watches.start(&file, std::time::Duration::from_secs(15))
                    })
                    .await
                    .map_err(|e| ("host_operation_failed", e.to_string()))?
                    .map_err(|e| ("officecli_watch_failed", e))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "officecli_watch_start",
                        "url": url,
                    }))
                }
                Some("officecli_watch_stop") => {
                    let watches = state
                        .officecli_watches
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "officecli watch is not initialized".into()))?;
                    let file = frame
                        .get("file")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "file is required".into()))?;
                    let _ = watches.stop(file);
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "officecli_watch_stop",
                    }))
                }
                Some("officecli_watch_status") => {
                    let watches = state
                        .officecli_watches
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "officecli watch is not initialized".into()))?;
                    let file = frame
                        .get("file")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "file is required".into()))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "officecli_watch_status",
                        "url": watches.status(file),
                    }))
                }
                Some("officecli_watch_mark") => {
                    let watches = state
                        .officecli_watches
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or(("unavailable", "officecli watch is not initialized".into()))?;
                    let file = frame
                        .get("file")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "file is required".into()))?
                        .to_string();
                    let path = frame
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_operation", "path is required".into()))?
                        .to_string();
                    let output = tokio::task::spawn_blocking(move || watches.mark(&file, &path))
                        .await
                        .map_err(|e| ("host_operation_failed", e.to_string()))?
                        .map_err(|e| ("officecli_watch_failed", e))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "officecli_watch_mark",
                        "output": output,
                    }))
                }
                Some("file_read") => {
                    let path = frame
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_path", "path is required".into()))?;
                    let root = state
                        .data
                        .workspace_root(workspace_id)
                        .map_err(host_data_error)?;
                    if crate::anydoc_preview::is_candidate(path) {
                        return convert_office_preview(
                            state,
                            context,
                            request_id.as_str(),
                            &root,
                            path,
                        )
                        .await;
                    }
                    let content = crate::host_files::read(&root, path)
                        .map_err(|error| (error.code(), "File read failed".to_owned()))?;
                    let content_type = Path::new(path)
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.to_ascii_lowercase());
                    let mime_type = match content_type.as_deref() {
                        Some("png") => Some("image/png"),
                        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
                        Some("gif") => Some("image/gif"),
                        Some("webp") => Some("image/webp"),
                        Some("svg") => Some("image/svg+xml"),
                        Some("ico") => Some("image/x-icon"),
                        Some("bmp") => Some("image/bmp"),
                        Some("pdf") => Some("application/pdf"),
                        _ => None,
                    };
                    if let Some(mime_type) = mime_type {
                        return Ok(json!({
                            "type": "data_response",
                            "requestId": request_id,
                            "operation": "file_read",
                            "path": content.relative_path,
                            "mtimeMs": content.modified_at_ms,
                            "mimeType": mime_type,
                            "isBinary": true,
                            "truncated": false,
                            "editable": false,
                        }));
                    }
                    let text = String::from_utf8(content.bytes)
                        .map_err(|_| ("binary_file", "Binary file requires raw download".into()))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "file_read",
                        "path": content.relative_path,
                        "content": text,
                        "mtimeMs": content.modified_at_ms,
                        "isBinary": false,
                        "truncated": false,
                        "editable": true,
                    }))
                }
                Some("file_write") => {
                    let path = frame
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_path", "path is required".into()))?;
                    let content = frame
                        .get("content")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_content", "content is required".into()))?;
                    let expected = frame
                        .get("expectedModifiedAtMs")
                        .and_then(Value::as_u64)
                        .map(u128::from);
                    let key = frame.get("idempotencyKey").and_then(Value::as_str).ok_or((
                        "idempotency_key_required",
                        "File writes require idempotencyKey".into(),
                    ))?;
                    // Plan §14: writes walk the mutation/Operation Registry.
                    let scope = operation_scope(context, "workspace-files")?;
                    let root = state
                        .data
                        .workspace_root(workspace_id)
                        .map_err(host_data_error)?;
                    let path_for_write = path;
                    let content_for_write = content;
                    let (operation_id, acceptance, modified_at_ms) = state.runtimes.host_mutation(
                        scope,
                        key,
                        "file_write",
                        "workspace-files",
                        || {
                            crate::host_files::write(
                                &root,
                                path_for_write,
                                content_for_write.as_bytes(),
                                expected,
                            )
                            .map(|modified| json!(modified))
                            .map_err(|error| (error.code(), "File write failed".to_owned()))
                        },
                    )?;
                    let acceptance = file_mutation_acceptance(acceptance)?;
                    let modified_at_ms =
                        modified_at_ms.and_then(|value| value.as_u64()).ok_or((
                            "file_write_failed",
                            "Completed write lost its terminal result".into(),
                        ))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "file_write",
                        "operationId": operation_id,
                        "acceptance": acceptance,
                        "path": path,
                        "modifiedAtMs": modified_at_ms,
                    }))
                }
                Some("file_create") => {
                    let parent_path = frame
                        .get("parentPath")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_path", "parentPath is required".into()))?;
                    let name = frame
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_name", "name is required".into()))?;
                    let kind = match frame.get("kind").and_then(Value::as_str) {
                        Some("file") => crate::host_files::CreateKind::File,
                        Some("directory") => crate::host_files::CreateKind::Directory,
                        _ => {
                            return Err((
                                "invalid_kind",
                                "kind must be \"file\" or \"directory\"".into(),
                            ))
                        }
                    };
                    let key = frame.get("idempotencyKey").and_then(Value::as_str).ok_or((
                        "idempotency_key_required",
                        "File mutations require idempotencyKey".into(),
                    ))?;
                    let scope = operation_scope(context, "workspace-files")?;
                    let root = state
                        .data
                        .workspace_root(workspace_id)
                        .map_err(host_data_error)?;
                    let (operation_id, acceptance, result) = state.runtimes.host_mutation(
                        scope,
                        key,
                        "file_create",
                        "workspace-files",
                        || {
                            crate::host_files::create(&root, parent_path, name, kind)
                                .map(|relative| json!(relative))
                                .map_err(|error| (error.code(), "File create failed".to_owned()))
                        },
                    )?;
                    let acceptance = file_mutation_acceptance(acceptance)?;
                    let relative_path = result
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .ok_or((
                            "file_create_failed",
                            "Completed create lost its terminal result".into(),
                        ))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "file_create",
                        "operationId": operation_id,
                        "acceptance": acceptance,
                        "path": relative_path,
                        "kind": match kind {
                            crate::host_files::CreateKind::File => "file",
                            crate::host_files::CreateKind::Directory => "directory",
                        },
                    }))
                }
                Some("file_rename") => {
                    let path = frame
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_path", "path is required".into()))?;
                    let name = frame
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_name", "name is required".into()))?;
                    let key = frame.get("idempotencyKey").and_then(Value::as_str).ok_or((
                        "idempotency_key_required",
                        "File mutations require idempotencyKey".into(),
                    ))?;
                    let scope = operation_scope(context, "workspace-files")?;
                    let root = state
                        .data
                        .workspace_root(workspace_id)
                        .map_err(host_data_error)?;
                    let (operation_id, acceptance, result) = state.runtimes.host_mutation(
                        scope,
                        key,
                        "file_rename",
                        "workspace-files",
                        || {
                            crate::host_files::rename(&root, path, name)
                                .map(|relative| json!(relative))
                                .map_err(|error| (error.code(), "File rename failed".to_owned()))
                        },
                    )?;
                    let acceptance = file_mutation_acceptance(acceptance)?;
                    let relative_path = result
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .ok_or((
                            "file_rename_failed",
                            "Completed rename lost its terminal result".into(),
                        ))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "file_rename",
                        "operationId": operation_id,
                        "acceptance": acceptance,
                        "path": relative_path,
                    }))
                }
                Some("file_delete") => {
                    let path = frame
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_path", "path is required".into()))?;
                    let key = frame.get("idempotencyKey").and_then(Value::as_str).ok_or((
                        "idempotency_key_required",
                        "File mutations require idempotencyKey".into(),
                    ))?;
                    let scope = operation_scope(context, "workspace-files")?;
                    let root = state
                        .data
                        .workspace_root(workspace_id)
                        .map_err(host_data_error)?;
                    let (operation_id, acceptance, result) = state.runtimes.host_mutation(
                        scope,
                        key,
                        "file_delete",
                        "workspace-files",
                        || {
                            crate::host_files::remove(&root, path)
                                .map(|relative| json!(relative))
                                .map_err(|error| (error.code(), "File delete failed".to_owned()))
                        },
                    )?;
                    let acceptance = file_mutation_acceptance(acceptance)?;
                    let deleted_path = result
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .ok_or((
                        "file_delete_failed",
                        "Completed delete lost its terminal result".into(),
                    ))?;
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "file_delete",
                        "operationId": operation_id,
                        "acceptance": acceptance,
                        "deletedPath": deleted_path,
                    }))
                }
                Some("file_raw") => {
                    let path = frame
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_path", "path is required".into()))?;
                    let root = state
                        .data
                        .workspace_root(workspace_id)
                        .map_err(host_data_error)?;
                    let content = crate::host_files::read(&root, path)
                        .map_err(|error| (error.code(), "File read failed".to_owned()))?;
                    use base64::Engine;
                    let encoded =
                        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(content.bytes);
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "file_raw",
                        "path": content.relative_path,
                        "contentBase64": encoded,
                        "modifiedAtMs": content.modified_at_ms,
                    }))
                }
                Some("session_history") => {
                    let session_id = frame
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_session", "sessionId is required".into()))?;
                    let session_file = frame
                        .get("sessionFile")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_session", "sessionFile is required".into()))?;
                    let path = state
                        .data
                        .session_file_path_by_path(workspace_id, session_id, session_file)
                        .ok_or(("session_not_found", "Session is not available".into()))?;
                    let content = fs::read_to_string(path)
                        .map_err(|_| ("session_io_failed", "Session read failed".into()))?;
                    let entries = content
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                        .collect::<Vec<_>>();
                    Ok(json!({
                        "type": "data_response",
                        "requestId": request_id,
                        "operation": "session_history",
                        "entries": entries,
                    }))
                }
                _ => Err((
                    "unknown_data_operation",
                    "Unsupported data operation".into(),
                )),
            }
        }
        RoutedAction::Subscribe { request_id, .. } => Ok(json!({
            "type": "runtime_subscribed",
            "requestId": request_id,
        })),
    }
}

/// Bounded concurrency for mention walks (see MAX_CONCURRENT_MENTION_SCANS).
/// Process-wide because the walk is a per-keystroke operation rather than
/// per-connection state.
fn mention_scan_permits() -> Arc<Semaphore> {
    static PERMITS: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(PERMITS.get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_MENTION_SCANS))))
}

async fn mention_scan_permit() -> Result<tokio::sync::OwnedSemaphorePermit, (&'static str, String)>
{
    mention_scan_permits().acquire_owned().await.map_err(|_| {
        (
            "mention_search_failed",
            "mention scan gate closed".to_string(),
        )
    })
}

fn host_data_error(error: HostDataError) -> (&'static str, String) {
    match error {
        HostDataError::UnknownWorkspace => {
            ("workspace_not_found", "Workspace is not registered".into())
        }
        HostDataError::InvalidRelativePath | HostDataError::OutsideWorkspace => (
            "path_outside_workspace",
            "Requested path is outside the registered workspace".into(),
        ),
        HostDataError::NotDirectory => (
            "not_a_directory",
            "Requested path is not a directory".into(),
        ),
        HostDataError::InvalidMentionQuery => (
            "invalid_mention_query",
            "Mention query has invalid syntax".into(),
        ),
        HostDataError::MentionRootUnavailable(message) => ("mention_root_unavailable", message),
        // §6.2: the wire carries a host-authored sentence, never an OS error or
        // an absolute path. The frontend shows its own localized "stale" copy.
        HostDataError::TemporarilyUnavailable => (
            "temporarily_unavailable",
            "Workspace is temporarily unreachable".into(),
        ),
        HostDataError::Io(message) => ("file_access_failed", message),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingExchangeRequest {
    pairing_token: String,
    device_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingExchangeResponse {
    device_token: String,
}

async fn exchange_pairing(
    State(state): State<Arc<HostState>>,
    Json(request): Json<PairingExchangeRequest>,
) -> Result<Json<PairingExchangeResponse>, (StatusCode, Json<Value>)> {
    let token = state
        .auth
        .lock()
        .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable"))?
        .exchange(&request.pairing_token, &request.device_id, now_seconds())
        .map_err(|_| api_error(StatusCode::UNAUTHORIZED, "pairing_rejected"))?;
    Ok(Json(PairingExchangeResponse {
        device_token: token,
    }))
}

/// D4 pairing matrix v1: a paired device gets a minimal, read-only liveness
/// payload identified by its device token. Anything beyond this (runtime
/// reads, workspace visibility, commands) is the Gate B remote-surface work
/// and stays unauthorized.
async fn mobile_status(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "unauthenticated"))?;
    let authorized = state
        .auth
        .lock()
        .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable"))?
        .authorize(token)
        .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable"))?;
    if !authorized {
        return Err(api_error(StatusCode::UNAUTHORIZED, "pairing_rejected"));
    }
    Ok(Json(json!({
        "protocolVersion": 2,
        "appVersion": env!("CARGO_PKG_VERSION"),
        "piVersion": crate::pi_launch::locked_pi_version(),
    })))
}

async fn unimplemented_api_route() -> (StatusCode, Json<Value>) {
    api_error(StatusCode::NOT_FOUND, "unimplemented_route")
}

fn api_error(status: StatusCode, code: &'static str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": { "code": code } })))
}

async fn send_error(
    socket: &mut WebSocket,
    request_id: Option<&str>,
    code: &'static str,
    message: &str,
) -> Result<(), axum::Error> {
    socket
        .send(Message::Text(
            structured_error(request_id, code, message)
                .to_string()
                .into(),
        ))
        .await
}

fn structured_error(request_id: Option<&str>, code: &'static str, message: &str) -> Value {
    json!({
        "type": "error",
        "requestId": request_id,
        "error": { "code": code, "message": message },
    })
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::notification_bundle_identifier;

    #[test]
    fn notification_bundle_identifier_defaults_to_the_production_bundle() {
        // Reads only: the OnceLock is process-global and a set here would
        // leak into every other test in this binary.
        assert_eq!(notification_bundle_identifier(), "com.palandata.picot");
    }

    use super::{
        bind_is_loopback, dialog_response_allowed, outbound_message, outbound_payload_kind,
        read_bounded_utf8, HostServer,
    };
    use crate::host_router::HostClientContext;
    use crate::metadata_store::MetadataStore;
    use crate::native_pi_manager::NativePiManager;
    use crate::remote_auth::RemoteAuth;
    use crate::runtime_coordinator::RuntimeTarget;
    use crate::transport_limits::PayloadKind;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn oversized_outbound_responses_keep_their_request_id() {
        // A get_tree reply for an 868-entry session serializes past the
        // Response cap. Replacing it with an error that has no requestId
        // makes the pending frontend control time out instead of failing —
        // the caller must be able to correlate the rejection.
        let value = json!({
            "type": "runtime_response",
            "requestId": "tree-1",
            "response": { "data": "x".repeat(crate::transport_limits::OUTBOUND_PAYLOAD_BYTES + 1) }
        });
        let message = outbound_message(value, PayloadKind::Response);
        let error: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert_eq!(error["type"], "error");
        assert_eq!(error["requestId"], "tree-1");
        assert_eq!(error["error"]["code"], "response_too_large");
    }

    #[test]
    fn get_tree_responses_share_the_snapshot_payload_limit() {
        // The tree carries the same session content the snapshot already
        // delivers under the shared outbound cap; the generic Response
        // trees for sessions whose chat renders fine.
        assert_eq!(
            outbound_payload_kind(Some("runtime_response"), Some("get_tree")),
            PayloadKind::Snapshot
        );
        assert_eq!(
            outbound_payload_kind(Some("runtime_response"), Some("get_state")),
            PayloadKind::Response
        );
        assert_eq!(
            outbound_payload_kind(Some("runtime_snapshot"), None),
            PayloadKind::Snapshot
        );
        assert_eq!(
            outbound_payload_kind(Some("runtime_response"), None),
            PayloadKind::Response
        );
    }

    #[tokio::test]
    async fn large_get_tree_response_reaches_the_ws_client() {
        use tokio_tungstenite::tungstenite::Message;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-tree-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let runtimes = NativePiManager::new(32);
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let (owner, capability) = registry
            .create_owner_with_workspace(
                "owner".into(),
                temp.clone(),
                0,
                "http://127.0.0.1:1".into(),
                Some("workspace-tree".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let target = RuntimeTarget::with_owner(
            "workspace-tree",
            "session-tree",
            "instance-tree",
            owner.as_str(),
            0,
        );
        let mut fake = runtimes.register_in_memory(target.clone()).unwrap();
        let host = HostServer::start(public, runtimes, auth, metadata)
            .await
            .unwrap();
        host.set_owner_registry(registry);
        let ws_url = host.origin().replace("http://", "ws://") + "/v2/ws";
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                    "clientId": "tree-client", "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "runtime_subscribe", "requestId": "sub", "target": target
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();

        async fn request_tree(
            socket: &mut tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            fake: &mut crate::pi_rpc_bridge::InMemoryPiProcess,
            request_id: &str,
            payload_bytes: usize,
        ) -> Value {
            socket
                .send(Message::Text(
                    json!({
                        "type": "runtime_request", "requestId": request_id,
                        "idempotencyKey": format!("key-{request_id}"),
                        "target": {
                            "workspaceId": "workspace-tree",
                            "sessionId": "session-tree",
                            "instanceId": "instance-tree"
                        },
                        "command": { "type": "get_tree" }
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            let outbound = fake.read_request().await.expect("get_tree dispatched");
            let id = outbound["id"].as_str().expect("request id").to_owned();
            fake.write_frame(json!({
                "id": id,
                "type": "response",
                "command": "get_tree",
                "success": true,
                "data": { "tree": "t".repeat(payload_bytes) }
            }))
            .await
            .unwrap();
            // Every socket read is deadline-guarded: a dropped frame must
            // fail the test here, not hang the suite.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let frame = socket.next().await.unwrap().unwrap();
                    let parsed: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                    if parsed["requestId"] == request_id {
                        return parsed;
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("reply for {request_id} within 5s"))
        }

        // A 3.5MB tree (868-entry session measured at 3,509,731 bytes) is
        // snapshot-scale: it must reach the client, not time out.
        let big = request_tree(&mut socket, &mut fake, "tree-1", 3_500_000).await;
        assert_eq!(big["type"], "runtime_response", "frame: {big:?}");
        assert_eq!(
            big["response"]["data"]["tree"].as_str().unwrap().len(),
            3_500_000
        );

        // A 4.3MB tree also delivers: outbound session payloads share the
        // physical frame ceiling (upstream reference + legacy embedded-server
        // behavior), not a smaller business cap.
        let also_big = request_tree(&mut socket, &mut fake, "tree-2", 4_300_000).await;
        assert_eq!(also_big["type"], "runtime_response", "frame: {also_big:?}");

        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn read_session_tree_data_op_serves_flat_entries_over_v2() {
        use tokio_tungstenite::tungstenite::Message;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-tree-op-{nonce}"));
        let session_root = temp.join("sessions");
        let (host, mut harness, harness_temp) =
            desktop_harness("tree-op", Some(session_root.clone())).await;

        // Simulate Pi returning the bucket before staging a branching
        // session; Picot must not derive it from the workspace path.
        host.state
            .data
            .record_pi_session_bucket(
                &harness.workspace_id,
                "/pi/sessions/--tree-op-bucket--/session.jsonl",
            )
            .unwrap();
        let bucket_dir = host
            .state
            .data
            .session_bucket_for_workspace(&harness.workspace_id)
            .expect("persisted bucket");
        fs::create_dir_all(&bucket_dir).unwrap();
        let workspace = harness_temp.join("workspace");
        let cwd = serde_json::to_string(&workspace.to_string_lossy()).unwrap();
        fs::write(
            bucket_dir.join("2026-01-01T00-00-00-000Z_tree-op.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"tree-op\",\"cwd\":{cwd}}}\n\
                 {{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"message\":{{\"role\":\"user\",\"content\":\"q\"}}}}\n\
                 {{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}]}}}}\n"
            ),
        )
        .unwrap();

        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "data_request", "protocolVersion": 2, "requestId": "tree-op-1",
                    "operation": "read_session_tree",
                    "workspaceId": harness.workspace_id,
                    "sessionId": "tree-op"
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(5), harness.socket.next())
            .await
            .expect("reply within 5s")
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(response.to_text().unwrap()).unwrap();
        assert_eq!(response["type"], "data_response", "{response}");
        assert_eq!(response["operation"], "read_session_tree");
        let ids: Vec<&str> = response["tree"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["tree-op", "u1", "a1"]);
        assert_eq!(response["tree"]["leafId"], "a1");

        let _ = harness.socket.close(None).await;
        host.stop();
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn oauth_runtime_restart_revokes_previous_operation() {
        let mut manager = crate::oauth_manager::OAuthManager::default();
        let first_generation = manager.runtime_started();
        let operation = manager
            .start(
                crate::oauth_manager::OAuthClient::Desktop,
                "owner",
                "oauth-test",
                Duration::from_secs(60),
            )
            .expect("OAuth operation starts");
        assert_eq!(operation.generation, first_generation);
        manager.runtime_stopped();
        let second_generation = manager.runtime_started();
        assert_eq!(
            manager.status("owner", first_generation, "oauth-test"),
            Err(crate::oauth_manager::OAuthError::OperationNotFound)
        );
        assert!(second_generation > first_generation);
    }

    #[tokio::test]
    async fn oauth_host_controls_retired_with_pi_config_bridge() {
        use tokio_tungstenite::tungstenite::Message;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-oauth-{nonce}"));
        let workspace = temp.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let host = HostServer::start(public, NativePiManager::new(8), auth, metadata)
            .await
            .expect("host server starts");
        let (_owner, capability) = registry
            .create_owner_with_workspace(
                "oauth-contract-window".into(),
                workspace,
                0,
                host.origin().into(),
                Some("oauth-workspace".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        host.set_owner_registry(registry);
        host.runtime_started().expect("generation advances");
        let (mut socket, _) = tokio_tungstenite::connect_async(
            host.origin().replacen("http://", "ws://", 1) + "/v2/ws",
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                    "clientId": "oauth-contract-window", "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let ack: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ack["type"], "hello_ack");
        let request = |operation: &str| {
            json!({
                "type": "host_request", "requestId": format!("oauth-{operation}"),
                "operation": operation, "args": {}
            })
            .to_string()
        };
        socket
            .send(Message::Text(request("get_oauth_login_capabilities")))
            .await
            .unwrap();
        let response: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        // OAuth operations moved to the Pi `/picot-config` bridge. The host
        // surface must not fabricate responses (empty capabilities, a fake
        // cancel state) nor the retired `oauth_pi_bridge_unavailable` code:
        // every retired op falls through to the control handler, which this
        // harness leaves uninstalled (`control_unavailable`).
        assert_eq!(
            response["type"], "error",
            "get_oauth_login_capabilities must not fabricate a host_response: {response}"
        );
        assert_ne!(
            response["error"]["code"], "oauth_pi_bridge_unavailable",
            "the CP7 fail-closed stub is retired: {response}"
        );
        for operation in [
            "start_oauth_login",
            "cancel_oauth_login",
            "get_oauth_login_status",
            "logout_oauth_login",
        ] {
            socket
                .send(Message::Text(request(operation)))
                .await
                .unwrap();
            let error: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(
                error["type"], "error",
                "{operation} is retired from the host surface: {error}"
            );
            assert_ne!(error["error"]["code"], "oauth_pi_bridge_unavailable");
        }
        let _ = socket.close(None).await;
        host.stop();
        let _ = fs::remove_dir_all(temp);
    }

    /// Shared harness: temp workspace + host on a random loopback port with
    /// a desktop capability registered. Returns the connected socket and the
    /// workspace id bound to the owner.
    use std::path::PathBuf;

    struct DesktopHarness {
        socket: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        workspace_id: String,
        owner_id: String,
    }

    async fn desktop_harness(
        label: &str,
        session_root: Option<PathBuf>,
    ) -> (HostServer, DesktopHarness, PathBuf) {
        use tokio_tungstenite::tungstenite::Message;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-{label}-{nonce}"));
        let workspace = temp.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        // Data-plane operations resolve workspaces through the metadata
        // registry (like production startup); register the row and reuse its
        // id for the owner binding so both authorities agree.
        let workspace_row = metadata
            .lock()
            .unwrap()
            .add_workspace(&workspace)
            .unwrap()
            .0;
        let workspace_id = workspace_row.workspace_id;
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let host = HostServer::start_with_session_root(
            public,
            NativePiManager::new(8),
            auth,
            metadata,
            session_root,
        )
        .await
        .expect("host server starts");
        let (owner, capability) = registry
            .create_owner_with_workspace(
                format!("{label}-window"),
                workspace.clone(),
                0,
                host.origin().into(),
                Some(workspace_id.clone()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        host.set_owner_registry(registry);
        host.runtime_started().expect("generation advances");
        let (mut socket, _) = tokio_tungstenite::connect_async(
            host.origin().replacen("http://", "ws://", 1) + "/v2/ws",
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                    "clientId": format!("{label}-client"), "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let ack: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ack["type"], "hello_ack");
        (
            host,
            DesktopHarness {
                socket,
                workspace_id,
                owner_id: owner.as_str().to_string(),
            },
            temp,
        )
    }

    /// Landing-owner variant of `desktop_harness`: the owner carries NO
    /// workspace binding (TemporaryKind::Landing, like the cold-start
    /// window), so wid-matched data ops must be rejected while owner-scoped
    /// global ops (cost dashboard) are admitted.
    async fn landing_harness(label: &str) -> (HostServer, DesktopHarness, PathBuf) {
        use tokio_tungstenite::tungstenite::Message;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-landing-{label}-{nonce}"));
        let session_root = temp.join("sessions");
        fs::create_dir_all(&session_root).unwrap();
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let host = HostServer::start_with_session_root(
            public,
            NativePiManager::new(8),
            auth,
            metadata,
            Some(session_root),
        )
        .await
        .expect("host server starts");
        let home = temp.join("home");
        fs::create_dir_all(&home).unwrap();
        let (owner, capability) = registry
            .create_owner_with_workspace(
                format!("{label}-landing"),
                home,
                0,
                host.origin().into(),
                None,
                crate::window_owner::TemporaryKind::Landing,
            )
            .unwrap();
        host.set_owner_registry(registry);
        let (mut socket, _) = tokio_tungstenite::connect_async(
            host.origin().replacen("http://", "ws://", 1) + "/v2/ws",
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                    "clientId": format!("{label}-client"), "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let ack: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ack["type"], "hello_ack");
        (
            host,
            DesktopHarness {
                socket,
                workspace_id: String::new(),
                owner_id: owner.as_str().to_string(),
            },
            temp,
        )
    }

    #[tokio::test]
    async fn cost_dashboard_admitted_and_workspace_ops_rejected_for_landing_owner() {
        use tokio_tungstenite::tungstenite::Message;
        let (host, mut harness, temp) = landing_harness("cost-gate").await;

        // The landing page sends cost_dashboard with NO workspaceId at all.
        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "data_request", "protocolVersion": 2, "requestId": "landing-cost",
                    "operation": "cost_dashboard",
                    "range": "7d", "granularity": "day", "scope": "all"
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let reply: Value = serde_json::from_str(
            harness
                .socket
                .next()
                .await
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            reply["type"], "data_response",
            "cost_dashboard must be admitted for an unbound landing owner: {reply}"
        );
        assert_eq!(reply["operation"], "cost_dashboard");

        // A workspace-contained data op stays rejected for the same owner.
        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "data_request", "protocolVersion": 2, "requestId": "landing-file",
                    "operation": "file_mentions", "workspaceId": "ws-any", "query": "re"
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let reply: Value = serde_json::from_str(
            harness
                .socket
                .next()
                .await
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reply["type"], "error");
        assert_eq!(reply["error"]["code"], "unauthorized_target");

        host.stop();
        let _ = fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn stuck_ephemeral_command_does_not_block_same_socket_frames() {
        use tokio_tungstenite::tungstenite::Message;
        let (host, mut harness, temp) = desktop_harness("ephemeral-block", None).await;

        // Registry-backed ephemeral record + hub target: the same token
        // contract production admission enforces (registry generation).
        let registry = Arc::new(crate::ephemeral_registry::EphemeralRegistry::default());
        let owner = crate::window_owner::OwnerId::from_string(harness.owner_id.clone());
        let reservation = registry
            .reserve_create(&owner, crate::ephemeral_registry::EphemeralKind::SideChat)
            .unwrap();
        registry
            .commit_ready(
                &reservation,
                crate::ephemeral_registry::OwnedProcess {
                    port: 0,
                    pid: 1,
                    child_identity: 1,
                    canonical_cwd: PathBuf::new(),
                    transition_generation: 0,
                    temporary_directory: None,
                },
            )
            .unwrap();
        let instance_id = reservation.instance_id.clone();
        let (sender, _events) = tokio::sync::broadcast::channel(16);
        let hub = Arc::new(crate::host_ephemeral::EphemeralHub::new(
            Arc::clone(&registry),
            crate::host_control::HostEventSink::new(sender),
            temp.clone(),
        ));
        let target = crate::runtime_coordinator::RuntimeTarget::with_owner(
            harness.workspace_id.clone(),
            format!("ephemeral-{instance_id}"),
            instance_id.clone(),
            harness.owner_id.as_str(),
            1,
        );
        hub.install_test_target(target.clone());
        host.set_ephemeral_hub(hub);

        // A live runtime whose reply we control: forward_command parks in
        // bridge.request until WE answer via the in-memory process handle.
        let mut process = host.native_manager().register_in_memory(target).unwrap();

        // A non-mutation payload: mutations without an idempotency key fail
        // fast at admission and would never reach the runtime.
        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "ephemeral_command", "requestId": "ep-stall",
                    "ephemeralInstanceId": instance_id,
                    "generation": reservation.generation,
                    "payload": { "type": "get_state" }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        // The command reached the runtime: forward_command is now parked in
        // bridge.request awaiting this reply.
        let runtime_request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let request = process.read_request().await.expect("runtime request");
                if request["type"] == "get_state" {
                    break request;
                }
            }
        })
        .await
        .expect("forward_command must dispatch get_state to the runtime");
        let _ = runtime_request;

        // Same socket, while the ephemeral command is parked: an unrelated
        // data-plane probe must still be served.
        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "data_request", "protocolVersion": 2,
                    "requestId": "probe-1", "operation": "workspace_info",
                    "workspaceId": harness.workspace_id
                })
                .to_string(),
            ))
            .await
            .unwrap();

        let probe = tokio::time::timeout(std::time::Duration::from_millis(500), async {
            loop {
                let message = harness.socket.next().await.unwrap().unwrap();
                let frame: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                if frame["requestId"] == "probe-1" {
                    break frame;
                }
            }
        })
        .await
        .expect("probe must be served while the ephemeral command is parked");
        assert_eq!(probe["type"], "data_response", "{probe}");

        // Unblock the parked command so the (spawned) arm settles quickly.
        process
            .write_frame(json!({
                "id": runtime_request["id"],
                "type": "response",
                "success": true,
                "data": {}
            }))
            .await
            .unwrap();

        // Deliberately no host.stop(): the socket closes with the test.
    }

    fn anydoc_fixture(name: &str) -> std::path::PathBuf {
        [
            env!("CARGO_MANIFEST_DIR"),
            "..",
            "extensions",
            "fixtures",
            "anydoc",
            name,
        ]
        .iter()
        .collect()
    }

    async fn send_data_request(
        harness: &mut DesktopHarness,
        request_id: &str,
        operation: &str,
        path: &str,
    ) -> Value {
        use tokio_tungstenite::tungstenite::Message;
        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "data_request", "protocolVersion": 2,
                    "requestId": request_id, "operation": operation,
                    "workspaceId": harness.workspace_id, "path": path
                })
                .to_string(),
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let message = harness.socket.next().await.unwrap().unwrap();
                let frame: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                if frame["requestId"] == request_id {
                    return frame;
                }
            }
        })
        .await
        .expect("file_read reply within 20s")
    }

    /// Send a `data_request` whose body is spelled out by the caller. Used by
    /// the workspace-file mutation tests, which carry fields `list_files` and
    /// `file_read` do not.
    async fn send_data_request_body(
        harness: &mut DesktopHarness,
        request_id: &str,
        mut body: serde_json::Map<String, Value>,
    ) -> Value {
        use tokio_tungstenite::tungstenite::Message;
        body.insert("type".into(), json!("data_request"));
        body.insert("protocolVersion".into(), json!(2));
        body.insert("requestId".into(), json!(request_id));
        harness
            .socket
            .send(Message::Text(Value::Object(body).to_string()))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let message = harness.socket.next().await.unwrap().unwrap();
                let frame: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                if frame["requestId"] == request_id {
                    return frame;
                }
            }
        })
        .await
        .expect("file mutation reply within 20s")
    }

    async fn send_file_mutation(
        harness: &mut DesktopHarness,
        request_id: &str,
        operation: &str,
        fields: Value,
        idempotency_key: &str,
    ) -> Value {
        let mut body = fields.as_object().cloned().unwrap_or_default();
        body.insert("operation".into(), json!(operation));
        body.insert("workspaceId".into(), json!(harness.workspace_id.clone()));
        body.insert("idempotencyKey".into(), json!(idempotency_key));
        send_data_request_body(harness, request_id, body).await
    }

    #[tokio::test]
    async fn file_mutations_create_rename_and_delete_inside_the_workspace() {
        let (_host, mut harness, temp) = desktop_harness("file-mutations", None).await;
        let workspace = temp.join("workspace");
        // `.` is the workspace root; a directory then a file inside it.
        let directory = send_file_mutation(
            &mut harness,
            "fm-1",
            "file_create",
            json!({ "parentPath": ".", "name": "notes", "kind": "directory" }),
            "fm-key-1",
        )
        .await;
        assert_eq!(directory["type"], "data_response", "{directory:?}");
        assert_eq!(directory["path"], "notes");
        assert_eq!(directory["kind"], "directory");
        assert_eq!(directory["acceptance"], "accepted_pending");
        assert!(workspace.join("notes").is_dir());

        let file = send_file_mutation(
            &mut harness,
            "fm-2",
            "file_create",
            json!({ "parentPath": "notes", "name": "a.md", "kind": "file" }),
            "fm-key-2",
        )
        .await;
        assert_eq!(file["path"], "notes/a.md");
        assert_eq!(
            fs::read_to_string(workspace.join("notes/a.md")).unwrap(),
            ""
        );

        // Create never overwrites, and the refusal carries no OS detail.
        let clash = send_file_mutation(
            &mut harness,
            "fm-3",
            "file_create",
            json!({ "parentPath": "notes", "name": "a.md", "kind": "file" }),
            "fm-key-3",
        )
        .await;
        assert_eq!(clash["type"], "error", "{clash:?}");
        assert_eq!(clash["error"]["code"], "already_exists");
        assert_eq!(clash["error"]["message"], "File create failed");
        assert!(
            !clash.to_string().contains(temp.to_str().unwrap()),
            "{clash}"
        );

        // Rename stays inside the parent directory.
        let renamed = send_file_mutation(
            &mut harness,
            "fm-4",
            "file_rename",
            json!({ "path": "notes/a.md", "name": "b.md" }),
            "fm-key-4",
        )
        .await;
        assert_eq!(renamed["path"], "notes/b.md");
        assert!(!workspace.join("notes/a.md").exists());

        // A separator in the new name would make rename a move API.
        let move_attempt = send_file_mutation(
            &mut harness,
            "fm-5",
            "file_rename",
            json!({ "path": "notes/b.md", "name": "../escaped.md" }),
            "fm-key-5",
        )
        .await;
        assert_eq!(move_attempt["error"]["code"], "invalid_name");
        assert!(!temp.join("escaped.md").exists());

        // A non-empty directory is refused: no recursion, no trash, no undo.
        let not_empty = send_file_mutation(
            &mut harness,
            "fm-6",
            "file_delete",
            json!({ "path": "notes" }),
            "fm-key-6",
        )
        .await;
        assert_eq!(not_empty["error"]["code"], "directory_not_empty");
        assert!(workspace.join("notes/b.md").exists());

        let deleted_file = send_file_mutation(
            &mut harness,
            "fm-7",
            "file_delete",
            json!({ "path": "notes/b.md" }),
            "fm-key-7",
        )
        .await;
        assert_eq!(deleted_file["deletedPath"], "notes/b.md");
        assert!(!workspace.join("notes/b.md").exists());

        let deleted_directory = send_file_mutation(
            &mut harness,
            "fm-8",
            "file_delete",
            json!({ "path": "notes" }),
            "fm-key-8",
        )
        .await;
        assert_eq!(deleted_directory["deletedPath"], "notes");
        assert!(!workspace.join("notes").exists());
    }

    #[tokio::test]
    async fn file_mutations_replay_a_completed_operation_by_idempotency_key() {
        let (_host, mut harness, temp) = desktop_harness("file-idempotency", None).await;
        let create = json!({ "parentPath": ".", "name": "once.txt", "kind": "file" });

        let first = send_file_mutation(
            &mut harness,
            "fi-1",
            "file_create",
            create.clone(),
            "fi-key",
        )
        .await;
        assert_eq!(first["acceptance"], "accepted_pending");
        assert_eq!(first["path"], "once.txt");

        // The replay returns the recorded terminal result and does not re-run
        // the operation, so it never trips `already_exists`.
        let replay =
            send_file_mutation(&mut harness, "fi-2", "file_create", create, "fi-key").await;
        assert_eq!(replay["type"], "data_response", "{replay:?}");
        assert_eq!(replay["acceptance"], "duplicate_completed");
        assert_eq!(replay["path"], "once.txt");
        assert!(temp.join("workspace/once.txt").exists());
    }

    #[tokio::test]
    async fn file_mutations_require_the_registered_workspace() {
        let (_host, mut harness, temp) = desktop_harness("file-gate", None).await;
        let mut body = json!({
            "operation": "file_create",
            "parentPath": ".",
            "name": "escape.txt",
            "kind": "file",
            "idempotencyKey": "fg-key",
            "workspaceId": "some-other-workspace",
        })
        .as_object()
        .cloned()
        .unwrap();
        body.insert("type".into(), json!("data_request"));
        body.insert("protocolVersion".into(), json!(2));
        body.insert("requestId".into(), json!("fg-1"));

        let reply = send_data_request_body(&mut harness, "fg-1", body).await;
        assert_eq!(reply["type"], "error", "{reply:?}");
        assert_eq!(reply["error"]["code"], "unauthorized_target");
        assert!(!temp.join("workspace/escape.txt").exists());
    }

    #[tokio::test]
    async fn list_files_reports_an_unreachable_workspace_as_temporarily_unavailable() {
        let (_host, mut harness, temp) = desktop_harness("list-transient", None).await;
        // An unmounted volume leaves the registered root pointing at nothing.
        fs::rename(temp.join("workspace"), temp.join("unmounted")).unwrap();
        let reply = send_data_request(&mut harness, "lt-1", "list_files", "").await;
        assert_eq!(reply["type"], "error", "{reply:?}");
        assert_eq!(reply["error"]["code"], "temporarily_unavailable");
        assert_eq!(
            reply["error"]["message"],
            "Workspace is temporarily unreachable"
        );
        assert!(
            !reply.to_string().contains(temp.to_str().unwrap()),
            "{reply}"
        );
    }

    #[tokio::test]
    async fn file_read_office_candidate_converts_to_read_only_markdown() {
        let (_host, mut harness, temp) = desktop_harness("anydoc-ready", None).await;
        fs::copy(
            anydoc_fixture("text.docx"),
            temp.join("workspace/report.docx"),
        )
        .unwrap();
        let reply = send_data_request(&mut harness, "prev-1", "file_read", "report.docx").await;
        assert_eq!(reply["type"], "data_response", "{reply:?}");
        assert_eq!(reply["previewStatus"], "ready");
        assert_eq!(reply["renderAs"], "markdown");
        assert_eq!(reply["editable"], false);
        assert_eq!(reply["isBinary"], false);
        assert_eq!(reply["truncated"], false);
        let content = reply["content"].as_str().unwrap();
        assert!(!content.trim().is_empty());
        assert!(reply["mtimeMs"].as_u64().unwrap_or(0) > 0);
    }

    #[tokio::test]
    async fn file_read_candidate_pdf_bytes_fail_closed_without_reroute() {
        let (_host, mut harness, temp) = desktop_harness("anydoc-pdf", None).await;
        fs::copy(
            anydoc_fixture("text.pdf"),
            temp.join("workspace/mislabeled.docx"),
        )
        .unwrap();
        let reply = send_data_request(&mut harness, "prev-2", "file_read", "mislabeled.docx").await;
        assert_eq!(reply["previewStatus"], "conversionFailed", "{reply:?}");
        assert!(reply.get("content").is_none());
        assert_eq!(reply["editable"], false);
    }

    #[tokio::test]
    async fn file_read_plain_text_stays_generic_and_editable() {
        let (_host, mut harness, temp) = desktop_harness("anydoc-plain", None).await;
        fs::write(temp.join("workspace/notes.txt"), "hello").unwrap();
        let reply = send_data_request(&mut harness, "prev-3", "file_read", "notes.txt").await;
        assert_eq!(reply["content"], "hello");
        assert_eq!(reply["editable"], true);
        assert!(reply.get("previewStatus").is_none());
    }

    #[tokio::test]
    async fn file_read_missing_candidate_keeps_established_error_code() {
        let (_host, mut harness, _temp) = desktop_harness("anydoc-missing", None).await;
        let reply = send_data_request(&mut harness, "prev-4", "file_read", "ghost.docx").await;
        assert_eq!(reply["type"], "error", "{reply:?}");
        assert_eq!(reply["error"]["code"], "file_not_found");
    }

    #[tokio::test]
    async fn file_read_candidate_over_input_cap_is_conversion_failed() {
        let (_host, mut harness, temp) = desktop_harness("anydoc-cap", None).await;
        let oversized = vec![0u8; 33 * 1024 * 1024];
        fs::write(temp.join("workspace/huge.docx"), &oversized).unwrap();
        let reply = send_data_request(&mut harness, "prev-5", "file_read", "huge.docx").await;
        assert_eq!(reply["previewStatus"], "conversionFailed", "{reply:?}");
    }

    #[tokio::test]
    async fn file_read_preview_rejects_after_workspace_transition() {
        let (host, mut harness, temp) = desktop_harness("anydoc-scope", None).await;
        fs::copy(
            anydoc_fixture("text.docx"),
            temp.join("workspace/report.docx"),
        )
        .unwrap();
        // Hold both preview permits so the request passes admission, then
        // parks on the permit wait — the window where a transition lands.
        let permit_a = host
            .state
            .preview_permits
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let permit_b = host
            .state
            .preview_permits
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let harness_owner_id = harness.owner_id.clone();
        let reply_task = tokio::spawn(async move {
            send_data_request(&mut harness, "prev-6", "file_read", "report.docx").await
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        // A workspace transition commits while the request is parked on the
        // permit wait: the captured scope's generation is now stale and must
        // fail revalidation after the permit lands.
        let registry = host.owner_registry_for_tests().expect("registry set");
        let owner = crate::window_owner::OwnerId::from_string(harness_owner_id.clone());
        let generation = registry
            .begin_workspace_transition(&owner, temp.join("workspace-b"), 3011)
            .expect("transition begins");
        registry
            .prepare_navigation(
                &owner,
                3011,
                temp.join("workspace-b"),
                "http://127.0.0.1:3011".to_string(),
                Duration::from_secs(30),
            )
            .expect("navigation permit");
        registry
            .commit_workspace_transition(&owner, generation, "http://127.0.0.1:3011".to_string())
            .expect("transition commits");
        drop(permit_a);
        drop(permit_b);
        let reply = reply_task.await.unwrap();
        assert_eq!(reply["type"], "error", "{reply:?}");
        assert_eq!(reply["error"]["code"], "unauthorized_target");
    }

    #[tokio::test]
    async fn terminal_commands_route_over_v2_to_the_manager() {
        use tokio_tungstenite::tungstenite::Message;
        let (host, mut harness, temp) = desktop_harness("terminal", None).await;
        let manager = Arc::new(crate::terminal_manager::TerminalManager::new(
            crate::terminal_registry::TerminalRegistry::new(4),
            crate::terminal_state_store::TerminalStateStore::new(temp.join("terminal-state")),
        ));
        host.set_terminal_manager(manager);
        let terminal = |payload: Value, request_id: &str| {
            json!({
                "type": "terminal_command", "requestId": request_id,
                "workspaceGeneration": 1, "payload": payload
            })
            .to_string()
        };
        harness
            .socket
            .send(Message::Text(terminal(
                json!({ "type": "terminal_list" }),
                "term-1",
            )))
            .await
            .unwrap();
        let listed: Value = serde_json::from_str(
            harness
                .socket
                .next()
                .await
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(listed["type"], "terminal_listed", "{listed}");
        assert_eq!(listed["requestId"], "term-1");

        harness
            .socket
            .send(Message::Text(terminal(
                json!({ "type": "terminal_nonsense" }),
                "term-2",
            )))
            .await
            .unwrap();
        let failed: Value = serde_json::from_str(
            harness
                .socket
                .next()
                .await
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(failed["type"], "terminal_command_failed", "{failed}");
        assert_eq!(failed["requestId"], "term-2");
        let _ = harness.socket.close(None).await;
        host.stop();
        let _ = fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn data_requests_are_not_blocked_by_slow_host_requests() {
        use std::time::Duration;
        use tokio_tungstenite::tungstenite::Message;

        let (host, mut harness, temp) = desktop_harness("data-priority", None).await;
        host.set_control_handler(Arc::new(|_, _, _| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(json!({ "delayed": true }))
            })
        }));
        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "host_request", "protocolVersion": 2, "requestId": "slow-host",
                    "operation": "slow_test_operation", "args": {}
                })
                .to_string(),
            ))
            .await
            .unwrap();
        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "data_request", "protocolVersion": 2, "requestId": "sidebar-count",
                    "operation": "workspace_sessions", "workspaceId": harness.workspace_id,
                    "countOnly": true
                })
                .to_string(),
            ))
            .await
            .unwrap();

        let response = tokio::time::timeout(Duration::from_millis(150), harness.socket.next())
            .await
            .expect("sidebar count must not wait for slow host request")
            .expect("socket stays open")
            .expect("websocket frame");
        let response: Value = serde_json::from_str(response.to_text().unwrap()).unwrap();
        assert_eq!(response["type"], "data_response", "{response}");
        assert_eq!(response["requestId"], "sidebar-count", "{response}");
        assert_eq!(response["operation"], "workspace_sessions", "{response}");
        assert_eq!(response["sessions"], json!([]), "{response}");

        let _ = harness.socket.close(None).await;
        host.stop();
        let _ = fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn workspace_sessions_data_plane_returns_scanned_entries() {
        use tokio_tungstenite::tungstenite::Message;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let outer = std::env::temp_dir().join(format!("picot-sessions-{nonce}"));
        let session_root = outer.join("sessions");
        fs::create_dir_all(&session_root).unwrap();

        // The bucket is an explicit value returned by Pi in this fixture;
        // Picot must not derive it from the workspace path.
        let (host, mut harness, temp) =
            desktop_harness("sessions", Some(session_root.clone())).await;
        let canonical = fs::canonicalize(temp.join("workspace")).unwrap();
        let bucket_name = "--pi-returned-session-bucket--";
        let bucket = session_root.join(bucket_name);
        host.state
            .data
            .record_pi_session_bucket(
                &harness.workspace_id,
                "/pi/sessions/--pi-returned-session-bucket--/session.jsonl",
            )
            .unwrap();
        fs::create_dir_all(&bucket).unwrap();
        fs::write(
            bucket.join("2026-09-01T00-00-00-000Z_sess-1.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"sess-1\",\"timestamp\":\"2026-09-01T00:00:00Z\",\"cwd\":\"{cwd}\"}}\n\
                 {{\"type\":\"session_info\",\"name\":\"History probe\"}}\n\
                 {{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hello history\"}}}}\n",
                cwd = canonical.to_string_lossy()
            ),
        )
        .unwrap();

        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "data_request", "protocolVersion": 2, "requestId": "data-1",
                    "operation": "workspace_sessions",
                    "workspaceId": harness.workspace_id
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let response: Value = serde_json::from_str(
            harness
                .socket
                .next()
                .await
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["type"], "data_response", "{response}");
        assert_eq!(response["operation"], "workspace_sessions");
        let sessions = response["sessions"].as_array().expect("sessions array");
        assert_eq!(sessions.len(), 1, "{response}");
        assert_eq!(sessions[0]["id"], "sess-1");
        assert_eq!(sessions[0]["name"], "History probe");
        assert_eq!(sessions[0]["firstMessage"], "hello history");
        assert_eq!(response["sessionCount"], 1);
        assert_eq!(response["hiddenSubagentCount"], 0);

        harness
            .socket
            .send(Message::Text(
                json!({
                    "type": "data_request", "protocolVersion": 2, "requestId": "data-count",
                    "operation": "workspace_sessions", "workspaceId": harness.workspace_id,
                    "countOnly": true
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let count_only: Value = serde_json::from_str(
            harness
                .socket
                .next()
                .await
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(count_only["sessionCount"], 1);
        assert_eq!(count_only["hiddenSubagentCount"], Value::Null);
        assert_eq!(count_only["sessions"], json!([]));
        let _ = harness.socket.close(None).await;
        host.stop();
        let _ = fs::remove_dir_all(temp);
        let _ = fs::remove_dir_all(outer);
    }

    #[tokio::test]
    #[ignore = "real host-origin + embedded Pi smoke; run via scripts/smoke-host-origin-p3.mjs"]
    async fn native_smoke_host_origin_p3() {
        use crate::native_pi_manager::NativeRuntimeType;
        use crate::pi_launch::native_launch_spec_for;
        use std::time::Duration;
        use tokio_tungstenite::tungstenite::Message;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-origin-p3-{nonce}"));
        let workspace = temp.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let public = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("public");
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let runtimes = NativePiManager::new(32);
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let host = HostServer::start(public.clone(), runtimes.clone(), auth, metadata)
            .await
            .expect("host server starts");
        let (owner, capability) = registry
            .create_owner_with_workspace(
                "p3-smoke-window".into(),
                workspace.clone(),
                0,
                host.origin().into(),
                Some("p3-workspace".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        host.set_owner_registry(registry);
        let target = RuntimeTarget::with_owner(
            "p3-workspace",
            "p3-session",
            format!("p3-instance-{nonce}"),
            owner.as_str(),
            0,
        );
        let spec = native_launch_spec_for(&public, NativeRuntimeType::Primary, &workspace, None)
            .expect("native launch spec resolves embedded Pi");
        runtimes
            .spawn(target.clone(), spec)
            .expect("embedded Pi spawns");

        if let Some(output_path) = std::env::var_os("PICOT_P3_PERF_OUTPUT") {
            let samples = std::env::var("PICOT_P3_PERF_SAMPLES")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(20);
            let warmup = std::env::var("PICOT_P3_PERF_WARMUP")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(3);
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let shell_url = format!(
                "{}/workspaces/{}/sessions/{}",
                host.origin(),
                target.workspace_id,
                target.session_id
            );
            let capability_url = format!(
                "{}/v2/bootstrap?workspaceId={}&sessionId={}",
                host.origin(),
                target.workspace_id,
                target.session_id
            );
            let shell_start = std::time::Instant::now();
            let shell_status = client.get(&shell_url).send().await.unwrap().status();
            let shell_ms = shell_start.elapsed().as_secs_f64() * 1000.0;
            let bootstrap_start = std::time::Instant::now();
            let bootstrap_status = client
                .get(&capability_url)
                .header("x-picot-desktop-capability", &capability)
                .send()
                .await
                .unwrap()
                .status();
            let bootstrap_ms = bootstrap_start.elapsed().as_secs_f64() * 1000.0;
            assert!(shell_status.is_success() && bootstrap_status.is_success());
            let ws_url = host.origin().replacen("http://", "ws://", 1) + "/v2/ws";
            let (mut socket, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                        "clientId": "p3-perf-window", "desktopCapability": capability
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            let _: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "type": "runtime_subscribe", "requestId": "perf-sub", "target": target
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            let _: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            let mut snapshot_ms = Vec::with_capacity(samples);
            let mut prompt_ms = Vec::with_capacity(samples);
            for index in 0..(warmup + samples) {
                let snapshot_started = std::time::Instant::now();
                let snapshot_id = format!("perf-snapshot-{index}");
                socket
                    .send(Message::Text(
                        json!({
                            "type": "runtime_snapshot_request", "requestId": snapshot_id,
                            "target": target.clone()
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
                let snapshot = loop {
                    let frame: Value = serde_json::from_str(
                        tokio::time::timeout(Duration::from_secs(30), socket.next())
                            .await
                            .unwrap()
                            .unwrap()
                            .unwrap()
                            .to_text()
                            .unwrap(),
                    )
                    .unwrap();
                    if frame["type"] == "runtime_snapshot" && frame["requestId"] == snapshot_id {
                        break snapshot_started.elapsed().as_secs_f64() * 1000.0;
                    }
                };
                let prompt_started = std::time::Instant::now();
                let prompt_id = format!("perf-prompt-{index}");
                socket
                    .send(Message::Text(
                        json!({
                            "type": "runtime_request", "requestId": prompt_id,
                            "idempotencyKey": prompt_id, "target": target.clone(),
                            "command": { "type": "prompt", "message": "Reply with one short word." }
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
                let prompt = loop {
                    let frame: Value = serde_json::from_str(
                        tokio::time::timeout(Duration::from_secs(30), socket.next())
                            .await
                            .unwrap()
                            .unwrap()
                            .unwrap()
                            .to_text()
                            .unwrap(),
                    )
                    .unwrap();
                    if frame["type"] == "runtime_event" {
                        break prompt_started.elapsed().as_secs_f64() * 1000.0;
                    }
                };
                {
                    socket.send(Message::Text(json!({
                        "type": "runtime_request", "requestId": format!("perf-abort-{index}"),
                        "target": target.clone(), "command": { "type": "abort" }
                    }).to_string())).await.unwrap();
                    loop {
                        let frame: Value = serde_json::from_str(
                            socket.next().await.unwrap().unwrap().to_text().unwrap(),
                        )
                        .unwrap();
                        if frame["type"] == "runtime_response"
                            && frame["requestId"] == format!("perf-abort-{index}")
                        {
                            break;
                        }
                    }
                }
                if index >= warmup {
                    snapshot_ms.push(snapshot);
                    prompt_ms.push(prompt);
                }
            }
            fs::write(
                output_path,
                serde_json::to_vec_pretty(&json!({
                    "samples": samples, "warmup": warmup,
                    "shellMs": shell_ms, "bootstrapMs": bootstrap_ms,
                    "snapshotMs": snapshot_ms, "promptToFirstEventMs": prompt_ms
                }))
                .unwrap(),
            )
            .unwrap();
            let _ = socket.close(None).await;
            runtimes.stop(&target).expect("perf runtime stops");
            host.stop();
            let _ = fs::remove_dir_all(temp);
            return;
        }

        // Optional browser harness rendezvous. The Rust test owns real HostServer
        // and embedded Pi; a separate real Chromium process drives browser APIs.
        if let Some(handshake_path) = std::env::var_os("PICOT_P3_BROWSER_HANDSHAKE") {
            let handshake = json!({
                "origin": host.origin(),
                "capability": capability,
                "workspaceId": target.workspace_id,
                "sessionId": target.session_id,
                "owner": owner.as_str(),
            });
            fs::write(handshake_path, serde_json::to_vec(&handshake).unwrap()).unwrap();
            let done_path =
                std::env::var_os("PICOT_P3_BROWSER_DONE").expect("browser harness done path");
            let deadline = std::time::Instant::now() + Duration::from_secs(120);
            while !std::path::Path::new(&done_path).exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "browser harness timed out"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let shell = client
            .get(format!(
                "{}/workspaces/{}/sessions/{}",
                host.origin(),
                target.workspace_id,
                target.session_id
            ))
            .send()
            .await
            .unwrap();
        assert!(
            shell.status().is_success(),
            "host shell status={}",
            shell.status()
        );
        assert!(shell.text().await.unwrap().contains("<base href="));
        let bootstrap = client
            .get(format!(
                "{}/v2/bootstrap?workspaceId={}&sessionId={}",
                host.origin(),
                target.workspace_id,
                target.session_id
            ))
            .header("x-picot-desktop-capability", &capability)
            .send()
            .await
            .unwrap();
        let bootstrap_status = bootstrap.status();
        let bootstrap_body = bootstrap.text().await.unwrap();
        assert!(
            bootstrap_status.is_success(),
            "bootstrap status={bootstrap_status} body={bootstrap_body}"
        );
        assert_eq!(
            serde_json::from_str::<RuntimeTarget>(&bootstrap_body).unwrap(),
            target
        );
        assert_eq!(
            client
                .get(format!("{}/ws", host.origin()))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .get(format!(
                    "{}/v2/bootstrap?workspaceId=other&sessionId=s",
                    host.origin()
                ))
                .header("x-picot-desktop-capability", &capability)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(format!(
                    "{}/v2/bootstrap?workspaceId={}&sessionId={}",
                    host.origin(),
                    target.workspace_id,
                    target.session_id
                ))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );

        let ws_url = host.origin().replacen("http://", "ws://", 1) + "/v2/ws";
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                    "clientId": "p3-smoke-window", "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let ack: Value = serde_json::from_str(
            tokio::time::timeout(Duration::from_secs(30), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(ack["type"], "hello_ack");
        socket
            .send(Message::Text(
                json!({
                    "type": "runtime_subscribe", "requestId": "sub-1", "target": target
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let subscribed: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(subscribed["type"], "runtime_subscribed");
        socket
            .send(Message::Text(
                json!({
                    "type": "runtime_snapshot_request", "requestId": "state-1", "target": target
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let response = loop {
            let frame: Value = serde_json::from_str(
                tokio::time::timeout(Duration::from_secs(30), socket.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .to_text()
                    .unwrap(),
            )
            .unwrap();
            if frame["type"] == "runtime_snapshot" && frame["requestId"] == "state-1" {
                break frame;
            }
        };
        assert_eq!(
            response["type"], "runtime_snapshot",
            "read-only snapshot response: {response}"
        );
        assert_eq!(response["requestId"], "state-1");
        assert!(response["state"].is_object());
        assert!(
            response["state"].get("stats").is_none(),
            "snapshot must not fetch stats before an active session is confirmed: {response}"
        );
        let snapshot_sequence = response["sequence"].as_u64().unwrap();

        // Drive one real prompt through HostServer → native bridge → embedded Pi.
        // No model result is required: the accepted response plus first runtime
        // event proves prompt dispatch and event forwarding without asserting
        // provider-specific text.
        socket
            .send(Message::Text(
                json!({
                    "type": "runtime_request", "requestId": "prompt-1",
                    "idempotencyKey": "p3-prompt-1", "target": target,
                    "command": { "type": "prompt", "message": "Reply with one short word." }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let mut prompt_response = None;
        let mut first_event_sequence = None;
        for _ in 0..16 {
            let frame: Value = serde_json::from_str(
                tokio::time::timeout(Duration::from_secs(30), socket.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .to_text()
                    .unwrap(),
            )
            .unwrap();
            if frame["type"] == "runtime_response" && frame["requestId"] == "prompt-1" {
                prompt_response = Some(frame);
                break;
            }
            if frame["type"] == "runtime_event" {
                first_event_sequence = frame["sequence"].as_u64();
            }
        }
        let prompt_response = prompt_response.expect("prompt response must arrive");
        assert_eq!(
            prompt_response["type"], "runtime_response",
            "prompt response: {prompt_response}"
        );
        assert_eq!(prompt_response["requestId"], "prompt-1");
        assert!(matches!(
            prompt_response["acceptance"].as_str(),
            Some("accepted_pending" | "duplicate_pending" | "duplicate_completed")
        ));

        assert!(
            first_event_sequence.is_some(),
            "prompt must produce runtime event"
        );
        // Bare abort (pi's command carries no turnId): the running turn stops.
        socket
            .send(Message::Text(
                json!({
                    "type": "runtime_request", "requestId": "abort-1", "target": target,
                    "command": { "type": "abort" }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let abort_response: Value = serde_json::from_str(
            tokio::time::timeout(Duration::from_secs(30), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            abort_response["type"], "runtime_response",
            "abort response: {abort_response}"
        );
        assert_eq!(abort_response["requestId"], "abort-1");
        assert_ne!(abort_response["response"]["disposition"], "stale_turn");

        // Reconnect on same authorized owner, then hydrate from authoritative snapshot.
        let _ = socket.close(None).await;
        let (mut reconnected, _) = tokio_tungstenite::connect_async(
            host.origin().replacen("http://", "ws://", 1) + "/v2/ws",
        )
        .await
        .unwrap();
        reconnected
            .send(Message::Text(
                json!({
                    "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                    "clientId": "p3-smoke-window-reconnect", "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let reconnect_ack: Value = serde_json::from_str(
            tokio::time::timeout(Duration::from_secs(30), reconnected.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reconnect_ack["type"], "hello_ack");
        reconnected
            .send(Message::Text(
                json!({
                    "type": "runtime_subscribe", "requestId": "sub-reconnect", "target": target
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let subscribed: Value = serde_json::from_str(
            reconnected
                .next()
                .await
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(subscribed["type"], "runtime_subscribed");
        reconnected
            .send(Message::Text(json!({
                "type": "runtime_snapshot_request", "requestId": "state-reconnect", "target": target
            }).to_string()))
            .await.unwrap();
        let reconnect_snapshot = loop {
            let frame: Value = serde_json::from_str(
                tokio::time::timeout(Duration::from_secs(30), reconnected.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .to_text()
                    .unwrap(),
            )
            .unwrap();
            if frame["type"] == "runtime_snapshot" && frame["requestId"] == "state-reconnect" {
                break frame;
            }
        };
        assert_eq!(reconnect_snapshot["type"], "runtime_snapshot");
        assert_eq!(reconnect_snapshot["requestId"], "state-reconnect");
        assert!(reconnect_snapshot["sequence"].as_u64().unwrap() >= snapshot_sequence);
        assert!(first_event_sequence
            .is_none_or(|sequence| reconnect_snapshot["sequence"].as_u64().unwrap() >= sequence));
        let _ = reconnected.close(None).await;
        runtimes.stop(&target).expect("smoke runtime stops");
        host.stop();
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn dialog_response_policy_requires_owner_desktop_class() {
        let owner = crate::window_owner::OwnerId::from_string("owner".into());
        let desktop = HostClientContext::desktop(
            "desktop",
            owner,
            crate::window_owner::OwnerWorkspaceSnapshot::Registered {
                wid: "workspace".into(),
                root: "/workspace".into(),
                generation: 3,
            },
        );
        assert!(dialog_response_allowed(Some(&desktop), Some("owner")));
        assert!(!dialog_response_allowed(
            Some(&HostClientContext::remote("remote")),
            Some("owner")
        ));
        assert!(!dialog_response_allowed(
            Some(&HostClientContext::public("browser")),
            Some("owner")
        ));
        assert!(!dialog_response_allowed(Some(&desktop), None));
        assert!(!dialog_response_allowed(
            Some(&desktop),
            Some("other-owner")
        ));
        let other = HostClientContext::desktop(
            "other",
            crate::window_owner::OwnerId::from_string("other-owner".into()),
            crate::window_owner::OwnerWorkspaceSnapshot::Registered {
                wid: "workspace".into(),
                root: "/workspace".into(),
                generation: 3,
            },
        );
        assert!(!dialog_response_allowed(Some(&other), Some("owner")));
    }

    #[test]
    fn bind_policy_rejects_non_loopback_addresses() {
        assert!(bind_is_loopback("127.0.0.1".parse().unwrap()));
        assert!(bind_is_loopback("::1".parse().unwrap()));
        assert!(!bind_is_loopback("192.168.1.10".parse().unwrap()));
    }

    #[test]
    fn absent_lan_preference_defaults_to_loopback() {
        // The LAN bind is opt-in: a fresh install with no stored preference
        // must stay loopback-only (see `mobile_lan_access_enabled`).
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-lan-default-{nonce}"));
        fs::create_dir_all(&temp).unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        assert!(!super::mobile_lan_access_enabled(&metadata));
        metadata
            .lock()
            .unwrap()
            .pref_set("mobile.lanAccessEnabled", &serde_json::json!(true))
            .unwrap();
        assert!(super::mobile_lan_access_enabled(&metadata));
        let _ = fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn retained_routes_have_complete_auth_method_and_limit_contract() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-api-contract-{nonce}"));
        let workspace_a = temp.join("workspace-a");
        let workspace_b = temp.join("workspace-b");
        fs::create_dir_all(&workspace_a).unwrap();
        fs::create_dir_all(&workspace_b).unwrap();
        fs::write(workspace_a.join("note.txt"), "contract").unwrap();
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let (workspace_a_id, workspace_b_id) = {
            let store = metadata.lock().unwrap();
            let a = store.add_workspace(&workspace_a).unwrap().0.workspace_id;
            let b = store.add_workspace(&workspace_b).unwrap().0.workspace_id;
            (a, b)
        };
        assert_ne!(workspace_a_id, workspace_b_id);
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let host = HostServer::start(public, NativePiManager::new(32), auth, metadata)
            .await
            .unwrap();
        let (owner_a, capability_a) = registry
            .create_owner_with_workspace(
                "contract-a".into(),
                workspace_a,
                0,
                host.origin().into(),
                Some(workspace_a_id.clone()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let (_owner_b, capability_b) = registry
            .create_owner_with_workspace(
                "contract-b".into(),
                workspace_b,
                0,
                host.origin().into(),
                Some(workspace_b_id.clone()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        host.set_owner_registry(registry);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        // Health/version are deliberately public host metadata. They must not
        // accidentally inherit workspace authorization, but remain GET-only.
        for route in ["/health", "/api/health", "/api/pi-version"] {
            let response = client
                .get(format!("{}{}", host.origin(), route))
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success(), "route={route}");
            let response = client
                .post(format!("{}{}", host.origin(), route))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                reqwest::StatusCode::METHOD_NOT_ALLOWED,
                "route={route}"
            );
        }

        let retained = [
            ("/api/files?workspaceId=", "files"),
            ("/api/sessions?workspaceId=", "sessions"),
            ("/api/search?workspaceId=", "search"),
            ("/api/cost-dashboard?workspaceId=", "cost-dashboard"),
        ];
        for (prefix, name) in retained {
            let route_a = if name == "files" {
                format!("{prefix}{workspace_a_id}&path=.")
            } else if name == "search" {
                format!("{prefix}{workspace_a_id}&q=contract")
            } else {
                format!("{prefix}{workspace_a_id}")
            };
            let url = |route: &str| format!("{}{}", host.origin(), route);

            let missing = client.get(url(&route_a)).send().await.unwrap();
            assert_eq!(
                missing.status(),
                reqwest::StatusCode::UNAUTHORIZED,
                "route={name}"
            );
            assert_eq!(
                missing.json::<Value>().await.unwrap()["error"]["code"],
                "unauthenticated"
            );

            let wrong_owner = client
                .get(url(&route_a))
                .header("x-picot-desktop-capability", &capability_b)
                .send()
                .await
                .unwrap();
            let wrong_owner_status = wrong_owner.status();
            let wrong_owner_body = wrong_owner.text().await.unwrap();
            assert_eq!(
                wrong_owner_status,
                reqwest::StatusCode::FORBIDDEN,
                "route={name} body={wrong_owner_body}"
            );
            assert_eq!(
                serde_json::from_str::<Value>(&wrong_owner_body).unwrap()["error"]["code"],
                "unauthorized_target"
            );

            let wrong_workspace = if name == "files" {
                format!("{prefix}{workspace_b_id}&path=.")
            } else if name == "search" {
                format!("{prefix}{workspace_b_id}&q=contract")
            } else {
                format!("{prefix}{workspace_b_id}")
            };
            let wrong_workspace = client
                .get(url(&wrong_workspace))
                .header("x-picot-desktop-capability", &capability_a)
                .send()
                .await
                .unwrap();
            assert_eq!(
                wrong_workspace.status(),
                reqwest::StatusCode::FORBIDDEN,
                "route={name}"
            );
            assert_eq!(
                wrong_workspace.json::<Value>().await.unwrap()["error"]["code"],
                "unauthorized_target"
            );

            let valid = client
                .get(url(&route_a))
                .header("x-picot-desktop-capability", &capability_a)
                .send()
                .await
                .unwrap();
            assert!(valid.status().is_success(), "route={name}");

            let method = client
                .post(url(&route_a))
                .header("x-picot-desktop-capability", &capability_a)
                .send()
                .await
                .unwrap();
            // Retained compatibility routes are read-only GET routes.
            assert_eq!(
                method.status(),
                reqwest::StatusCode::METHOD_NOT_ALLOWED,
                "route={name}"
            );
        }

        // Body limits apply before handler dispatch on a body-bearing route;
        // retained GET routes remain method-rejected without reading a body.
        let oversized = client
            .post(format!("{}/v2/auth/exchange", host.origin()))
            .header("content-type", "application/json")
            .body(vec![b'x'; 2 * 1024 * 1024])
            .send()
            .await;
        assert!(
            oversized.is_err()
                || oversized.unwrap().status() == reqwest::StatusCode::PAYLOAD_TOO_LARGE,
            "HTTP body limit must reject oversized input"
        );

        assert_eq!(owner_a.as_str().len(), 32);
        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn legacy_api_routes_require_owner_capability_and_unknown_routes_fail_closed() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-api-auth-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let host = HostServer::start(public, NativePiManager::new(32), auth, metadata)
            .await
            .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        // Every retained existing-shell route must enforce the same desktop
        // capability boundary. Keep this list in lockstep with the router
        // registrations above; a new retained route without this assertion is
        // an accidental unauthenticated compatibility surface.
        for route in [
            "/api/files?workspaceId=w",
            "/api/sessions?workspaceId=w",
            "/api/search?workspaceId=w&q=ab",
            "/api/cost-dashboard?workspaceId=w",
        ] {
            let response = client
                .get(format!("{}{}", host.origin(), route))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                reqwest::StatusCode::UNAUTHORIZED,
                "retained route must require capability: {route}"
            );
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["error"]["code"], "unauthenticated", "route={route}");
        }

        // D8 (2026-08-29): /api/rpc is retired with an explicit 410 Gone +
        // deprecation header + anonymous client-class hit counting.
        let rpc_gone = client
            .get(format!("{}/api/rpc", host.origin()))
            .send()
            .await
            .unwrap();
        assert_eq!(rpc_gone.status(), reqwest::StatusCode::GONE);
        assert_eq!(
            rpc_gone
                .headers()
                .get("deprecation")
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
        let rpc_body: Value = rpc_gone.json().await.unwrap();
        assert_eq!(rpc_body["error"]["code"], "gone");
        assert!(rpc_body["removalNotice"].is_string());
        assert!(rpc_body["clientClass"].is_string());
        let rpc_gone_post = client
            .post(format!("{}/api/rpc", host.origin()))
            .send()
            .await
            .unwrap();
        assert_eq!(rpc_gone_post.status(), reqwest::StatusCode::GONE);

        // P6 scope removal: /api/super-agent/tasks is 410 Gone (retired).
        let sa_gone = client
            .get(format!("{}/api/super-agent/tasks", host.origin()))
            .send()
            .await
            .unwrap();
        assert_eq!(sa_gone.status(), reqwest::StatusCode::GONE);

        // Unretained routes without a migration path fail closed (404).
        for route in ["/api/not-retained"] {
            for request in [
                client.get(format!("{}{}", host.origin(), route)),
                client.post(format!("{}{}", host.origin(), route)),
            ] {
                let response = request.send().await.unwrap();
                assert_eq!(
                    response.status(),
                    reqwest::StatusCode::NOT_FOUND,
                    "route={route}"
                );
                let body: Value = response.json().await.unwrap();
                assert_eq!(
                    body["error"]["code"], "unimplemented_route",
                    "route={route}"
                );
            }
        }
        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn serves_health_and_static_assets_from_one_origin() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "<h1>Picot native host</h1>").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let host = HostServer::start(public, NativePiManager::new(32), auth, metadata)
            .await
            .unwrap();

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let health_response = client
            .get(format!("{}/health", host.origin()))
            .send()
            .await
            .unwrap();
        assert!(health_response.status().is_success());
        let health: serde_json::Value =
            serde_json::from_str(&health_response.text().await.unwrap()).unwrap();
        assert_eq!(health["status"], "ok");
        assert_eq!(health["protocolVersion"].as_u64(), Some(2));
        assert!(health["piVersion"].as_str().is_some());
        let index = client
            .get(format!("{}/app/settings", host.origin()))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(index.contains("Picot native host"));

        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn serves_static_assets_under_a_content_fingerprinted_path() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-versioned-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(public.join("native")).unwrap();
        fs::write(
            public.join("index.html"),
            "<html><head><base href=\"/\" /></head><body>Picot</body></html>",
        )
        .unwrap();
        fs::write(public.join("native/app.js"), "export const marker = 1;").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let host = HostServer::start(public.clone(), NativePiManager::new(32), auth, metadata)
            .await
            .unwrap();

        // The entry document's <base> should point at a `/v/<fingerprint>/`
        // path derived from the bundle contents, not the literal "/" that's
        // on disk — every relative script/import resolves under it.
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let index = client
            .get(format!("{}/app/settings", host.origin()))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let base_start = index.find("<base href=\"").unwrap() + "<base href=\"".len();
        let base_end = index[base_start..].find('"').unwrap();
        let base_href = &index[base_start..base_start + base_end];
        assert!(
            base_href.starts_with("/v/") && base_href.ends_with('/'),
            "expected a versioned base href, got {base_href:?}"
        );

        // The versioned path actually serves the underlying files.
        let app_js = client
            .get(format!("{}{}native/app.js", host.origin(), base_href))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(app_js, "export const marker = 1;");

        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn sends_runtime_events_only_after_an_explicit_target_subscription() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-ws-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let runtimes = NativePiManager::new(32);
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let (owner, capability) = registry
            .create_owner_with_workspace(
                "desktop-a".into(),
                temp.clone(),
                0,
                "http://127.0.0.1:1".into(),
                Some("workspace-a".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let target =
            RuntimeTarget::with_owner("workspace-a", "session-a", "instance-a", owner.as_str(), 0);
        let mut fake = runtimes.register_in_memory(target.clone()).unwrap();
        let host = HostServer::start(public, runtimes, auth, metadata)
            .await
            .unwrap();
        host.set_owner_registry(registry);
        let ws_url = host.origin().replace("http://", "ws://") + "/v2/ws";
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "hello",
                    "protocolVersion": 2,
                    "clientType": "desktop",
                    "clientId": "desktop-a",
                    "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "runtime_subscribe",
                    "requestId": "subscribe-1",
                    "target": target,
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();

        fake.write_frame(json!({ "type": "agent_start" }))
            .await
            .unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), socket.next())
            .await
            .expect("subscribed runtime event")
            .unwrap()
            .unwrap();
        let event: serde_json::Value = serde_json::from_str(event.to_text().unwrap()).unwrap();
        assert_eq!(event["type"], "runtime_event");
        assert_eq!(event["target"]["sessionId"], "session-a");
        assert_eq!(event["sequence"], 1);

        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn first_subscriber_cannot_claim_another_owners_extension_ui_response() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-dialog-owners-{nonce}"));
        let public = temp.join("public");
        let owner_b_root = temp.join("owner-b");
        fs::create_dir_all(&public).unwrap();
        fs::create_dir_all(&owner_b_root).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let runtimes = NativePiManager::new(32);
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let (owner_a, capability_a) = registry
            .create_owner_with_workspace(
                "desktop-a".into(),
                temp.clone(),
                0,
                "http://127.0.0.1:1".into(),
                Some("workspace-a".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let (owner_b, capability_b) = registry
            .create_owner_with_workspace(
                "desktop-b".into(),
                owner_b_root,
                0,
                "http://127.0.0.1:2".into(),
                Some("workspace-b".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        assert_ne!(owner_a, owner_b);
        let target = RuntimeTarget::with_owner(
            "workspace-b",
            "session-b",
            "instance-b",
            owner_b.as_str(),
            0,
        );
        let mut fake = runtimes.register_in_memory(target.clone()).unwrap();
        fake.write_frame(json!({
            "type": "extension_ui_request",
            "id": "dialog-b",
            "method": "select",
            "title": "Project trust",
            "options": ["Trust once", "Open untrusted"]
        }))
        .await
        .unwrap();
        tokio::task::yield_now().await;

        let host = HostServer::start(public, runtimes, auth, metadata)
            .await
            .unwrap();
        host.set_owner_registry(registry);
        let ws_url = host.origin().replace("http://", "ws://") + "/v2/ws";
        let (mut socket_a, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        socket_a
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "hello",
                    "protocolVersion": 2,
                    "clientType": "desktop",
                    "clientId": "desktop-a",
                    "desktopCapability": capability_a
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket_a.next().await.unwrap().unwrap();
        socket_a
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "runtime_subscribe",
                    "requestId": "subscribe-a",
                    "target": target,
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let admitted = socket_a.next().await.unwrap().unwrap();
        let admitted: serde_json::Value =
            serde_json::from_str(admitted.to_text().unwrap()).unwrap();
        assert_eq!(
            admitted["type"], "runtime_subscribed",
            "cross-owner activity subscriptions are admitted: {admitted}"
        );
        // The pending dialog must not replay to the non-owning subscriber;
        // the next frame owner-a can see is ordinary activity.
        fake.write_frame(json!({ "type": "agent_start" }))
            .await
            .unwrap();
        let probe = socket_a.next().await.unwrap().unwrap();
        let probe: serde_json::Value = serde_json::from_str(probe.to_text().unwrap()).unwrap();
        assert_eq!(
            probe["event"]["type"], "agent_start",
            "no dialog replay before the activity probe: {probe}"
        );

        let (mut socket_b, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        socket_b
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "hello",
                    "protocolVersion": 2,
                    "clientType": "desktop",
                    "clientId": "desktop-b",
                    "desktopCapability": capability_b
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket_b.next().await.unwrap().unwrap();
        socket_b
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "runtime_subscribe",
                    "requestId": "subscribe-b",
                    "target": target,
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket_b.next().await.unwrap().unwrap();
        let replay = socket_b.next().await.unwrap().unwrap();
        let replay: serde_json::Value = serde_json::from_str(replay.to_text().unwrap()).unwrap();
        assert_eq!(replay["event"]["id"], "dialog-b");

        // Admission to watch is not admission to answer: owner-a's attempt to
        // claim owner-b's dialog response must stay rejected.
        socket_a
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "runtime_request",
                    "requestId": "dialog-response-a",
                    "target": target,
                    "command": {
                        "type": "extension_ui_response",
                        "id": "dialog-b",
                        "value": "Claimed by A"
                    }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let claim_denied = socket_a.next().await.unwrap().unwrap();
        let claim_denied: serde_json::Value =
            serde_json::from_str(claim_denied.to_text().unwrap()).unwrap();
        assert_eq!(claim_denied["type"], "error");
        assert_eq!(claim_denied["error"]["code"], "unauthorized_target");

        socket_b
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "runtime_request",
                    "requestId": "dialog-response-b",
                    "target": target,
                    "command": {
                        "type": "extension_ui_response",
                        "id": "dialog-b",
                        "value": "Trust once"
                    }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket_b.next().await.unwrap().unwrap();
        assert_eq!(
            fake.read_request().await.unwrap(),
            json!({
                "type": "extension_ui_response",
                "id": "dialog-b",
                "value": "Trust once"
            })
        );

        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn mobile_pairing_create_refuses_while_lan_access_is_disabled() {
        use tokio_tungstenite::tungstenite::Message;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-mobile-disabled-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        metadata
            .lock()
            .unwrap()
            .pref_set("mobile.lanAccessEnabled", &json!(false))
            .unwrap();
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let host = HostServer::start(public, NativePiManager::new(8), auth, Arc::clone(&metadata))
            .await
            .unwrap();
        let (_owner, capability) = registry
            .create_owner_with_workspace(
                "mobile-disabled-window".into(),
                temp.clone(),
                0,
                host.origin().into(),
                Some("mobile-workspace".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        host.set_owner_registry(registry);
        host.runtime_started().expect("generation advances");
        let (mut socket, _) = tokio_tungstenite::connect_async(
            host.origin().replacen("http://", "ws://", 1) + "/v2/ws",
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                    "clientId": "mobile-disabled", "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let ack: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ack["type"], "hello_ack");
        socket
            .send(Message::Text(
                json!({
                    "type": "host_request", "requestId": "mobile-info",
                    "operation": "mobile_access_info", "args": {}
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let info: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(info["response"]["enabled"], false);
        assert_eq!(info["response"]["lanUrls"], json!([]));
        socket
            .send(Message::Text(
                json!({
                    "type": "host_request", "requestId": "mobile-pair",
                    "operation": "mobile_pairing_create", "args": {}
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let error: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(error["type"], "error");
        assert_eq!(error["error"]["code"], "mobile_access_disabled");
        let _ = socket.close(None).await;
        host.stop();
        let _ = fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn mobile_pairing_flow_exchanges_token_and_authorizes_status() {
        use tokio_tungstenite::tungstenite::Message;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-mobile-pair-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        metadata
            .lock()
            .unwrap()
            .pref_set("mobile.lanAccessEnabled", &json!(true))
            .unwrap();
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let host = HostServer::start(public, NativePiManager::new(8), auth, Arc::clone(&metadata))
            .await
            .unwrap();
        // Desktop windows keep using the loopback alias even on a LAN bind.
        assert!(host.origin().starts_with("http://127.0.0.1"));
        let (_owner, capability) = registry
            .create_owner_with_workspace(
                "mobile-pair-window".into(),
                temp.clone(),
                0,
                host.origin().into(),
                Some("mobile-workspace".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        host.set_owner_registry(registry);
        host.runtime_started().expect("generation advances");
        let (mut socket, _) = tokio_tungstenite::connect_async(
            host.origin().replacen("http://", "ws://", 1) + "/v2/ws",
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                    "clientId": "mobile-pair", "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let ack: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ack["type"], "hello_ack");
        socket
            .send(Message::Text(
                json!({
                    "type": "host_request", "requestId": "mobile-info",
                    "operation": "mobile_access_info", "args": {}
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let info: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(info["response"]["enabled"], true);
        socket
            .send(Message::Text(
                json!({
                    "type": "host_request", "requestId": "mobile-pair",
                    "operation": "mobile_pairing_create", "args": {}
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let response: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        let pairing_token = response["response"]["pairingToken"]
            .as_str()
            .expect("pairing token")
            .to_owned();

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let exchange = client
            .post(format!("{}/v2/auth/exchange", host.origin()))
            .json(&json!({ "pairingToken": pairing_token, "deviceId": "phone-1" }))
            .send()
            .await
            .unwrap();
        assert_eq!(exchange.status(), 200);
        let device_token = exchange.json::<Value>().await.unwrap()["deviceToken"]
            .as_str()
            .expect("device token")
            .to_owned();

        let status_url = format!("{}/v2/mobile/status", host.origin());
        let paired = client
            .get(&status_url)
            .header("authorization", format!("Bearer {device_token}"))
            .send()
            .await
            .unwrap();
        assert_eq!(paired.status(), 200);
        let status = paired.json::<Value>().await.unwrap();
        assert_eq!(status["protocolVersion"], 2);
        assert!(status["appVersion"].is_string());

        for unauthorized in [
            Vec::new(),
            vec![("authorization", "Bearer picot_device_not_a_token")],
        ] {
            let mut request = client.get(&status_url);
            for (key, value) in unauthorized {
                request = request.header(key, value);
            }
            let rejected = request.send().await.unwrap();
            assert_eq!(rejected.status(), 401);
        }

        let _ = socket.close(None).await;
        host.stop();
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn bounded_agent_text_reader_rejects_growth_beyond_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, vec![b'x'; crate::host_config::MAX_CONFIG_BYTES + 1]).unwrap();

        assert_eq!(read_bounded_utf8(&path), Err("config_too_large"));
    }

    #[test]
    fn bounded_agent_text_reader_rejects_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, [0xff, 0xfe]).unwrap();

        assert_eq!(read_bounded_utf8(&path), Err("config_invalid_encoding"));
    }

    #[tokio::test]
    async fn replays_startup_extension_ui_and_routes_the_owners_response_exactly_once() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-dialog-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let runtimes = NativePiManager::new(32);
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let (owner, capability) = registry
            .create_owner_with_workspace(
                "owner".into(),
                temp.clone(),
                0,
                "http://127.0.0.1:1".into(),
                Some("workspace-a".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let target =
            RuntimeTarget::with_owner("workspace-a", "session-a", "instance-a", owner.as_str(), 0);
        let mut fake = runtimes.register_in_memory(target.clone()).unwrap();
        fake.write_frame(json!({
            "type": "extension_ui_request",
            "id": "dialog-1",
            "method": "select",
            "title": "Project trust",
            "options": ["Trust once", "Open untrusted"]
        }))
        .await
        .unwrap();
        tokio::task::yield_now().await;

        let host = HostServer::start(public, runtimes, auth, metadata)
            .await
            .unwrap();
        host.set_owner_registry(registry);
        let ws_url = host.origin().replace("http://", "ws://") + "/v2/ws";
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "hello",
                    "protocolVersion": 2,
                    "clientType": "desktop",
                    "clientId": "owner",
                    "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "runtime_subscribe",
                    "requestId": "subscribe",
                    "target": target,
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        let replay = socket.next().await.unwrap().unwrap();
        let replay: serde_json::Value = serde_json::from_str(replay.to_text().unwrap()).unwrap();
        assert_eq!(replay["event"]["id"], "dialog-1");

        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "runtime_request",
                    "requestId": "dialog-response",
                    "target": target,
                    "command": {
                        "type": "extension_ui_response",
                        "id": "dialog-1",
                        "value": "Trust once"
                    }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        assert_eq!(
            fake.read_request().await.unwrap(),
            json!({
                "type": "extension_ui_response",
                "id": "dialog-1",
                "value": "Trust once"
            })
        );

        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn routes_extension_ui_response_sent_with_wire_triple_only() {
        // The desktop WebView sends only the routing triple: owner binding and
        // workspace generation never cross the wire. The dialog branch must
        // resolve the live target host-side; exact-equality validation against
        // the wire form would reject every production answer (live targets
        // carry owner Some and generation N) and strand pi's dialog forever.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-dialog-triple-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let runtimes = NativePiManager::new(32);
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let (owner, capability) = registry
            .create_owner_with_workspace(
                "owner".into(),
                temp.clone(),
                0,
                "http://127.0.0.1:1".into(),
                Some("workspace-a".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let target =
            RuntimeTarget::with_owner("workspace-a", "session-a", "instance-a", owner.as_str(), 0);
        let mut fake = runtimes.register_in_memory(target.clone()).unwrap();
        fake.write_frame(json!({
            "type": "extension_ui_request",
            "id": "dialog-1",
            "method": "select",
            "title": "Project trust",
            "options": ["Trust once", "Open untrusted"]
        }))
        .await
        .unwrap();
        tokio::task::yield_now().await;

        let host = HostServer::start(public, runtimes, auth, metadata)
            .await
            .unwrap();
        host.set_owner_registry(registry);
        let ws_url = host.origin().replace("http://", "ws://") + "/v2/ws";
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "hello",
                    "protocolVersion": 2,
                    "clientType": "desktop",
                    "clientId": "owner",
                    "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        let wire_target = json!({
            "workspaceId": "workspace-a",
            "sessionId": "session-a",
            "instanceId": "instance-a"
        });
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "runtime_subscribe",
                    "requestId": "subscribe",
                    "target": wire_target,
                })
                .to_string(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        let replay = socket.next().await.unwrap().unwrap();
        let replay: serde_json::Value = serde_json::from_str(replay.to_text().unwrap()).unwrap();
        assert_eq!(replay["event"]["id"], "dialog-1");

        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type": "runtime_request",
                    "requestId": "dialog-response",
                    "target": wire_target,
                    "command": {
                        "type": "extension_ui_response",
                        "id": "dialog-1",
                        "value": "Trust once"
                    }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let reply = socket.next().await.unwrap().unwrap();
        let reply: serde_json::Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
        assert_eq!(
            reply["type"], "runtime_response",
            "dialog response was rejected: {reply}"
        );
        assert_eq!(reply["response"]["success"], serde_json::json!(true));
        let delivered = tokio::time::timeout(Duration::from_secs(2), fake.read_request())
            .await
            .expect("dialog answer must reach pi stdin")
            .unwrap();
        assert_eq!(
            delivered,
            json!({
                "type": "extension_ui_response",
                "id": "dialog-1",
                "value": "Trust once"
            })
        );

        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    async fn ws_send(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        frame: Value,
    ) {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                frame.to_string(),
            ))
            .await
            .unwrap();
    }

    async fn ws_next(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("frame deadline")
            .unwrap()
            .unwrap();
        serde_json::from_str(frame.to_text().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn cross_workspace_activity_events_reach_other_subscribers_but_dialogs_stay_scoped() {
        // Cross-workspace switch (upstream parity): the B page must see A's
        // agent activity (sidebar green/blue dots) while never receiving A's
        // blocking dialogs, and its runtime commands must stay rejected.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-cross-ws-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(public.join("")).unwrap();
        fs::create_dir_all(temp.join("ws-a")).unwrap();
        fs::create_dir_all(temp.join("ws-b")).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let runtimes = NativePiManager::new(32);
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let (owner_a, capability_a) = registry
            .create_owner_with_workspace(
                "owner-a".into(),
                temp.join("ws-a").canonicalize().unwrap(),
                0,
                "http://127.0.0.1:1".into(),
                Some("workspace-a".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let (_owner_b, capability_b) = registry
            .create_owner_with_workspace(
                "owner-b".into(),
                temp.join("ws-b").canonicalize().unwrap(),
                0,
                "http://127.0.0.1:1".into(),
                Some("workspace-b".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let target = RuntimeTarget::with_owner(
            "workspace-a",
            "session-a",
            "instance-a",
            owner_a.as_str(),
            0,
        );
        let mut fake = runtimes.register_in_memory(target.clone()).unwrap();

        let host = HostServer::start(public, runtimes, auth, metadata)
            .await
            .unwrap();
        host.set_owner_registry(registry);
        let ws_url = host.origin().replace("http://", "ws://") + "/v2/ws";
        let (mut socket_a, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        let (mut socket_b, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        ws_send(
            &mut socket_a,
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                "clientId": "client-a", "desktopCapability": capability_a
            }),
        )
        .await;
        ws_send(
            &mut socket_b,
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                "clientId": "client-b", "desktopCapability": capability_b
            }),
        )
        .await;
        ws_next(&mut socket_a).await; // hello_ack
        ws_next(&mut socket_b).await; // hello_ack
        let wire_target = json!({
            "workspaceId": "workspace-a",
            "sessionId": "session-a",
            "instanceId": "instance-a"
        });
        ws_send(
            &mut socket_a,
            json!({ "type": "runtime_subscribe", "requestId": "sub-a", "target": wire_target }),
        )
        .await;
        let ack_a = ws_next(&mut socket_a).await;
        assert_eq!(
            ack_a["type"], "runtime_subscribed",
            "owner subscribes: {ack_a}"
        );
        ws_send(
            &mut socket_b,
            json!({ "type": "runtime_subscribe", "requestId": "sub-b", "target": wire_target }),
        )
        .await;
        let ack_b = ws_next(&mut socket_b).await;
        assert_eq!(
            ack_b["type"], "runtime_subscribed",
            "cross-workspace subscribe must be admitted: {ack_b}"
        );

        // Ordinary activity flows to both subscribers (green/blue dot feed).
        fake.write_frame(json!({ "type": "agent_start" }))
            .await
            .unwrap();
        let event_a = ws_next(&mut socket_a).await;
        assert_eq!(event_a["event"]["type"], "agent_start");
        let event_b = ws_next(&mut socket_b).await;
        assert_eq!(
            event_b["event"]["type"], "agent_start",
            "cross-workspace subscriber sees the activity event: {event_b}"
        );

        // Blocking dialogs reach only the owner-scoped subscriber.
        fake.write_frame(json!({
            "type": "extension_ui_request",
            "id": "dialog-1",
            "method": "select",
            "title": "Project trust",
            "options": ["Trust once", "Open untrusted"]
        }))
        .await
        .unwrap();
        let dialog_a = ws_next(&mut socket_a).await;
        assert_eq!(dialog_a["event"]["id"], "dialog-1");

        // Non-blocking UI payloads stay broadcast to every subscriber.
        fake.write_frame(json!({
            "type": "extension_ui_request",
            "id": "widget-1",
            "method": "setWidget",
            "payload": { "kind": "todo" }
        }))
        .await
        .unwrap();
        let widget_b = ws_next(&mut socket_b).await;
        assert_eq!(
            widget_b["event"]["id"], "widget-1",
            "cross-workspace subscriber's next frame is the widget, not the dialog: {widget_b}"
        );

        // Command admission is unchanged: B's runtime requests stay rejected.
        ws_send(
            &mut socket_b,
            json!({
                "type": "runtime_request", "requestId": "prompt-b",
                "target": wire_target,
                "command": { "type": "prompt", "message": "hi" }
            }),
        )
        .await;
        let reply_b = ws_next(&mut socket_b).await;
        assert_eq!(reply_b["requestId"], "prompt-b");
        assert_eq!(
            reply_b["type"], "error",
            "cross-workspace prompt must be rejected"
        );

        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn pending_dialog_replay_skips_cross_workspace_subscribers() {
        // A dialog queued before any subscription must replay only to the
        // authorize_target-passing owner; a cross-workspace subscriber gets
        // the ack and later activity, never the pending dialog.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-host-cross-replay-{nonce}"));
        let public = temp.join("public");
        fs::create_dir_all(public.join("")).unwrap();
        fs::create_dir_all(temp.join("ws-a")).unwrap();
        fs::create_dir_all(temp.join("ws-b")).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let runtimes = NativePiManager::new(32);
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let (owner_a, capability_a) = registry
            .create_owner_with_workspace(
                "owner-a".into(),
                temp.join("ws-a").canonicalize().unwrap(),
                0,
                "http://127.0.0.1:1".into(),
                Some("workspace-a".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let (_owner_b, capability_b) = registry
            .create_owner_with_workspace(
                "owner-b".into(),
                temp.join("ws-b").canonicalize().unwrap(),
                0,
                "http://127.0.0.1:1".into(),
                Some("workspace-b".into()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let target = RuntimeTarget::with_owner(
            "workspace-a",
            "session-a",
            "instance-a",
            owner_a.as_str(),
            0,
        );
        let mut fake = runtimes.register_in_memory(target.clone()).unwrap();
        fake.write_frame(json!({
            "type": "extension_ui_request",
            "id": "dialog-pending",
            "method": "select",
            "title": "Project trust",
            "options": ["Trust once", "Open untrusted"]
        }))
        .await
        .unwrap();
        tokio::task::yield_now().await;

        let host = HostServer::start(public, runtimes, auth, metadata)
            .await
            .unwrap();
        host.set_owner_registry(registry);
        let ws_url = host.origin().replace("http://", "ws://") + "/v2/ws";
        let (mut socket_b, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        ws_send(
            &mut socket_b,
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                "clientId": "client-b", "desktopCapability": capability_b
            }),
        )
        .await;
        ws_next(&mut socket_b).await; // hello_ack
        let wire_target = json!({
            "workspaceId": "workspace-a",
            "sessionId": "session-a",
            "instanceId": "instance-a"
        });
        ws_send(
            &mut socket_b,
            json!({ "type": "runtime_subscribe", "requestId": "sub-b", "target": wire_target }),
        )
        .await;
        let ack_b = ws_next(&mut socket_b).await;
        assert_eq!(ack_b["type"], "runtime_subscribed");

        // The very next frame the cross-workspace subscriber may see is
        // ordinary activity — the pending dialog must not have been replayed.
        fake.write_frame(json!({ "type": "agent_start" }))
            .await
            .unwrap();
        let next_b = ws_next(&mut socket_b).await;
        assert_eq!(
            next_b["event"]["type"], "agent_start",
            "pending dialog must not replay to a cross-workspace subscriber: {next_b}"
        );

        // The authorized owner still gets the replay.
        let (mut socket_a, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        ws_send(
            &mut socket_a,
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                "clientId": "client-a", "desktopCapability": capability_a
            }),
        )
        .await;
        ws_next(&mut socket_a).await; // hello_ack
        ws_send(
            &mut socket_a,
            json!({ "type": "runtime_subscribe", "requestId": "sub-a", "target": wire_target }),
        )
        .await;
        let _ = ws_next(&mut socket_a).await; // subscribe ack
        let replay_a = ws_next(&mut socket_a).await;
        assert_eq!(replay_a["event"]["id"], "dialog-pending");

        host.stop();
        fs::remove_dir_all(temp).unwrap();
    }
    #[tokio::test]
    async fn subagents_host_ops_scope_gates_over_ws() {
        use tokio_tungstenite::tungstenite::Message;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("picot-subagents-ws-{nonce}"));
        let agent_root = temp.join("agent");
        let workspace = temp.join("workspace");
        fs::create_dir_all(agent_root.join("agents")).unwrap();
        fs::create_dir_all(workspace.join(".pi/agents")).unwrap();
        fs::write(
            agent_root.join("agents/global-agent.md"),
            "---\nname: global-agent\ndescription: fixture\n---\nWS_RAW_PROMPT",
        )
        .unwrap();
        fs::write(
            workspace.join(".pi/agents/project-agent.md"),
            "---\nname: project-agent\ndescription: fixture\n---\nPROJECT_PROMPT",
        )
        .unwrap();
        fs::write(
            agent_root.join("trust.json"),
            format!(
                "{{\n  \"{}\": true\n}}\n",
                workspace.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        let public = temp.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("index.html"), "Picot").unwrap();
        let metadata = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        let workspace_id = metadata
            .lock()
            .unwrap()
            .add_workspace(&workspace)
            .unwrap()
            .0
            .workspace_id;
        let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
        let registry = Arc::new(crate::window_owner::WindowOwnerRegistry::default());
        let host = HostServer::start_with_session_root(
            public,
            NativePiManager::new(8),
            auth,
            metadata,
            None,
        )
        .await
        .expect("host server starts");
        let (owner, capability) = registry
            .create_owner_with_workspace(
                "subagents-ws-window".into(),
                workspace.clone(),
                0,
                host.origin().into(),
                Some(workspace_id.clone()),
                crate::window_owner::TemporaryKind::DefaultStartup,
            )
            .unwrap();
        host.set_owner_registry(registry.clone());
        host.set_agent_root_for_tests(agent_root.clone());
        host.runtime_started().expect("generation advances");
        let generation = registry
            .current_workspace_generation(&owner)
            .expect("generation present");
        let (mut socket, _) = tokio_tungstenite::connect_async(
            host.origin().replacen("http://", "ws://", 1) + "/v2/ws",
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                    "clientId": "subagents-ws-client", "desktopCapability": capability
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let ack: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ack["type"], "hello_ack");

        async fn roundtrip(
            socket: &mut tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            body: Value,
        ) -> Value {
            socket.send(Message::Text(body.to_string())).await.unwrap();
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
        }

        // Global scope from the bound desktop owner: agent-root entries only.
        let global = roundtrip(
            &mut socket,
            json!({
                "type": "host_request", "requestId": "sub-global",
                "operation": "subagents_inventory", "args": {"scope": "global"}
            }),
        )
        .await;
        assert_eq!(global["type"], "host_response", "{global}");
        assert_eq!(global["response"]["workspaceRoot"], Value::Null);
        let entries = global["response"]["entries"].as_array().unwrap();
        assert!(entries
            .iter()
            .any(|entry| entry["runtimeName"] == "global-agent"));
        assert!(!global.to_string().contains("PROJECT_PROMPT"));

        // Project identity smuggled onto global scope is refused.
        let forged_global = roundtrip(
            &mut socket,
            json!({
                "type": "host_request", "requestId": "sub-forged-global",
                "operation": "subagents_inventory",
                "args": {"scope": "global", "workspaceId": workspace_id}
            }),
        )
        .await;
        assert_eq!(forged_global["type"], "error");
        assert_eq!(forged_global["error"]["code"], "unauthorized_target");

        // Forged wid on project scope: no project paths leak.
        let forged_wid = roundtrip(&mut socket, json!({
            "type": "host_request", "requestId": "sub-forged-wid",
            "operation": "subagents_inventory",
            "args": {"scope": "project", "workspaceId": "forged", "workspaceGeneration": generation}
        }))
        .await;
        assert_eq!(forged_wid["type"], "error");
        assert_eq!(forged_wid["error"]["code"], "unauthorized_target");
        assert!(!forged_wid
            .to_string()
            .contains(&workspace.display().to_string()));

        // Correct binding: project scope returns the trusted project root.
        let project = roundtrip(&mut socket, json!({
            "type": "host_request", "requestId": "sub-project",
            "operation": "subagents_inventory",
            "args": {"scope": "project", "workspaceId": workspace_id, "workspaceGeneration": generation}
        }))
        .await;
        assert_eq!(project["type"], "host_response", "{project}");
        let project_entries = project["response"]["entries"].as_array().unwrap();
        assert!(project_entries
            .iter()
            .any(|entry| entry["runtimeName"] == "project-agent"));

        // Detail over the wire: forged candidate IDs fail closed.
        let forged_detail = roundtrip(
            &mut socket,
            json!({
                "type": "host_request", "requestId": "sub-detail",
                "operation": "subagents_get_detail",
                "args": {"scope": "global", "candidateId": "0".repeat(64)}
            }),
        )
        .await;
        assert_eq!(forged_detail["type"], "error");
        assert_eq!(forged_detail["error"]["code"], "candidate_stale");

        // Write control dispatches but the write-qualification gate refuses.
        let real_candidate = project_entries
            .iter()
            .find(|entry| entry["runtimeName"] == "project-agent")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let refused = roundtrip(&mut socket, json!({
            "type": "host_request", "requestId": "sub-override",
            "operation": "subagents_set_override",
            "args": {
                "scope": "project", "workspaceId": workspace_id, "workspaceGeneration": generation,
                "candidateId": real_candidate, "runtimeName": "project-agent",
                "model": {"op": "keep"}, "thinking": {"op": "keep"}
            }
        }))
        .await;
        assert_eq!(refused["type"], "error");
        assert_eq!(refused["error"]["code"], "revision_conflict");

        let _ = socket.close(None).await;
        host.stop();
        let _ = fs::remove_dir_all(temp);
    }
}
