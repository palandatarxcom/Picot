// ABOUTME: App-side daemon manager: a read-only probe, a detached spawn, the lease-gated adopt/stop
// ABOUTME: decision table, and the readiness wait (plan Task 4 Steps 1-3).

//! # DaemonManager (plan D3/D7, Task 4)
//!
//! The app never *assumes* a daemon: it probes, decides, and only then touches
//! a process or the admin channel. The decision table is the contract:
//!
//! | instance file | recorded daemon process | build identity | manager record | action |
//! | --- | --- | --- | --- | --- |
//! | absent / stale pid / start-time mismatch / `/health` down | — | — | — | **spawn** detached |
//! | live | alive | **mismatch** | none / dead / us | **attach lease → stop → respawn** |
//! | live | alive | **mismatch** | a live other app | **refuse** (never stop another app's daemon) |
//! | live | alive | match | none / dead app | **adopt** (`admin_hello`; 4409 ⇒ blocked) |
//! | live | alive | match | us | **already attached** (idempotent) |
//! | live | alive | match | a live other app | **`BlockedByOtherApp`** |
//!
//! Two rules are load-bearing:
//!
//! 1. **Probing is read-only** (plan D3): the instance file plus `GET /health`.
//!    Probing never opens the admin channel, so it can never disturb the
//!    incumbent manager's lease.
//! 2. **A manager record is only "dead" on pid + process start time**, the same
//!    identity discipline the daemon uses for pi runtimes. A record with no pid
//!    at all is treated as *live*: refusing to adopt is recoverable, kicking a
//!    working app is not.
//!
//! Adoption is a **daemon-authorized transaction**: the app sends
//! `admin_hello`; the daemon grants the lease and rewrites its own instance
//! file's manager record. The app never rewrites the file (plan D3).
//!
//! ## Implementation-time decisions
//!
//! - **Ready deadline 15s** (plan text) with a 100ms poll; the timeout error
//!   carries the daemon log path, because a detached daemon's only other trace
//!   is that file.
//! - **Spawn shape:** `std::process::Command` + `process_group(0)` on Unix, all
//!   three stdio streams null (the daemon writes `<data_dir>/picot-daemon.log`).
//!   The process is not registered with anything the app tears down: Tauri's
//!   `RunEvent::Exit` handlers, `tauri_plugin_shell`'s child store and the quit
//!   policy all leave it alone. Release builds resolve the sidecar beside the
//!   app executable, dev builds the workspace `target/debug/picot-daemon` next
//!   to this app's manifest (both in [`resolve_daemon_binary`], which carries
//!   the reasoning for not using the plugin's sidecar API).
//! - **`wait_ready` also resolves the startup race:** when two apps start at
//!   once, both spawn, the single-instance lock lets exactly one daemon live,
//!   and the loser's `wait_ready` finds the winner's live manager record → the
//!   loser returns [`EnsureOutcome::BlockedByOtherApp`].

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use picot_host::child_supervision;
use picot_host::daemon::admin::ManagerIdentity;
use picot_host::daemon::instance::{self, InstanceFile};

use super::exit_waiter::{ExitOutcome, ExitWaiter, ProcessIdentity};

/// The default ready deadline (plan Task 4 Step 2).
pub const READY_DEADLINE: Duration = Duration::from_secs(15);
/// Read-only probe poll interval while waiting for a daemon to come up.
pub const READY_POLL: Duration = Duration::from_millis(100);
/// The daemon binary's file name, shared by the dev and sidecar paths.
pub const DAEMON_BINARY_NAME: &str = "picot-daemon";
/// How long a `daemon.stop` transaction may take to be *confirmed* (plan D12).
pub const STOP_CONFIRM_DEADLINE: Duration = Duration::from_secs(10);
/// Budget for one admin-channel transaction.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Everything the manager needs to start a daemon that matches this app.
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// The daemon executable to start.
    pub daemon_binary: PathBuf,
    /// The daemon's private data directory (also where it publishes its files).
    pub data_dir: PathBuf,
    /// The daemon's config directory (passed through as `--config-dir`).
    pub config_dir: PathBuf,
    /// The WebView static bundle to serve.
    pub static_dir: PathBuf,
    /// The bundle identifier the daemon publishes for OS notifications.
    pub bundle_identifier: String,
    /// This app's version (`env!("CARGO_PKG_VERSION")`).
    pub app_version: String,
    /// This app's build id (`picot_host::BUILD_ID`) — the staleness rule
    /// compares version **and** build id, because a dev rebuild keeps the
    /// version unchanged.
    pub build_id: String,
    /// This app's pid (the manager identity the daemon records).
    pub self_pid: u32,
    /// This app's process start time (pid-reuse guard for the manager record).
    pub self_start_time: Option<String>,
    /// Ready deadline; tests inject a short one.
    pub ready_deadline: Duration,
}

impl ManagerConfig {
    /// The app-side configuration for one data/config/static triple. The
    /// daemon binary is resolved by [`resolve_daemon_binary`].
    pub fn for_app(
        data_dir: PathBuf,
        config_dir: PathBuf,
        static_dir: PathBuf,
        bundle_identifier: String,
    ) -> Self {
        Self {
            daemon_binary: resolve_daemon_binary(),
            data_dir,
            config_dir,
            static_dir,
            bundle_identifier,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            build_id: picot_host::BUILD_ID.to_string(),
            self_pid: std::process::id(),
            self_start_time: child_supervision::process_start_time(std::process::id()),
            ready_deadline: READY_DEADLINE,
        }
    }

    pub fn instance_path(&self) -> PathBuf {
        instance::instance_path(&self.data_dir)
    }

    pub fn log_path(&self) -> PathBuf {
        instance::log_path(&self.data_dir)
    }

    /// The identity this app presents to the daemon's lease.
    pub fn identity(&self) -> ManagerIdentity {
        ManagerIdentity {
            pid: Some(self.self_pid),
            app_version: Some(self.app_version.clone()),
            build_id: Some(self.build_id.clone()),
        }
    }

    fn endpoint(&self, file: &InstanceFile) -> AdminEndpoint {
        AdminEndpoint {
            origin: file.admin_origin.clone(),
            token: file.admin_token.clone(),
        }
    }
}

/// The daemon binary this app should start.
///
/// Dev builds resolve the workspace artifact next to this manifest
/// (`src-tauri/target/debug/picot-daemon`); release builds resolve the sidecar
/// the bundler places **beside the app executable** — for a macOS bundle
/// `Picot.app/Contents/MacOS/picot-daemon` (plan Task 11 Steps 1-2: the
/// staging path `src-tauri/binaries/picot-daemon-<target-triple>` loses its
/// triple suffix when the bundler copies it in, which is why the bare name is
/// right here).
///
/// ## Implementation-time decision: `current_exe().parent()` over the
/// `tauri_plugin_shell` sidecar API
///
/// Both can locate the packaged file, so the deciding factor is the plan's
/// hard requirement that the daemon "detaches the process group and never
/// joins an app-exit kill list". `tauri_plugin_shell` keeps every child it
/// spawns in a store and kills them all on `RunEvent::Exit` (v2.3.5,
/// `src/lib.rs`: the `on_event` handler that drains `shell.children` and calls
/// `child.kill()`), i.e. membership in exactly the list this daemon must stay
/// out of: `kill -9` on the app and ⌘Q with `keepRunningAfterQuit=true` both
/// rely on the daemon outliving the app. Its sidecar API would additionally
/// require an ACL entry (`shell:allow-execute` scoped to the sidecar), which
/// would hand the WebView permission to launch the daemon — the manager's spawn
/// is app-internal and must stay outside the WebView's reachable surface.
///
/// With `current_exe().parent()` the launch stays a plain
/// `std::process::Command` with `process_group(0)`, no stdio, and no registry:
/// nothing the app can reach owns the daemon's lifetime (plan Task 4 Step 3,
/// D11).
pub fn resolve_daemon_binary() -> PathBuf {
    resolve_daemon_binary_in(
        std::env::current_exe().ok().as_deref(),
        cfg!(debug_assertions),
    )
}

/// The pure half of [`resolve_daemon_binary`], so both profiles' paths are
/// testable in one binary (the dev path cannot be reached at runtime from a
/// release test and vice versa).
///
/// `exe_dir` is the directory holding the running app executable
/// (`std::env::current_exe()`). When it cannot be read — a stripped-down
/// environment — the bare file name is the last resort, resolved through
/// `PATH`; that lookup failing surfaces as a spawn error naming the binary
/// rather than as silence.
fn resolve_daemon_binary_in(exe_dir: Option<&Path>, dev: bool) -> PathBuf {
    if dev {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join(DAEMON_BINARY_NAME);
    }
    exe_dir
        .map(|dir| dir.join(DAEMON_BINARY_NAME))
        .unwrap_or_else(|| PathBuf::from(DAEMON_BINARY_NAME))
}

/// What `ensure` achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// This call started the daemon.
    Started(InstanceFile),
    /// A live, matching daemon accepted this app as its manager (either it was
    /// already there — adopt — or it won a startup race).
    Attached(InstanceFile),
    /// A different live Picot app manages this daemon: the caller shows the
    /// localized dialog and exits non-zero (plan Task 4 Step 3).
    BlockedByOtherApp,
}

/// Why `ensure` could not produce a usable daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonManagerError {
    /// The spawn itself failed.
    Spawn { message: String },
    /// The daemon did not become ready inside the deadline. Carries the daemon
    /// log path: a detached process's only other trace.
    NotReady {
        deadline: Duration,
        log_path: PathBuf,
        last: String,
    },
    /// The admin channel refused or failed the handshake.
    Admin { message: String },
    /// A stale-build daemon is managed by a *different* live app, so this app
    /// must not stop it (plan Task 4 Step 1: "version-mismatch stop without
    /// lease → rejected").
    StopWithoutLease { pid: u32, manager_pid: Option<u32> },
}

impl std::fmt::Display for DaemonManagerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn { message } => {
                write!(formatter, "cannot start the Picot daemon: {message}")
            }
            Self::NotReady {
                deadline,
                log_path,
                last,
            } => write!(
                formatter,
                "the Picot daemon did not become ready within {}s ({last}); see {}",
                deadline.as_secs(),
                log_path.display()
            ),
            Self::Admin { message } => {
                write!(formatter, "the Picot daemon admin channel: {message}")
            }
            Self::StopWithoutLease { pid, manager_pid } => write!(
                formatter,
                "the running Picot daemon (pid {pid}) is managed by another Picot app \
                 (pid {}), so this app will not stop it",
                manager_pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            ),
        }
    }
}

impl std::error::Error for DaemonManagerError {}

/// The read-only facts the decision table consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonObservation {
    /// The instance file as read (never rewritten by the app).
    pub instance: Option<InstanceFile>,
    /// The recorded daemon process is alive **and** its start time matches
    /// **and** its `/health` answers.
    pub daemon_alive: bool,
    /// The recorded daemon's version and build id equal this app's.
    pub identity_matches: bool,
    /// Who holds the daemon's manager lease, judged by pid + start time.
    pub manager: ManagerVerdict,
}

impl DaemonObservation {
    /// Nothing to see: no file, or a stale one.
    pub fn absent() -> Self {
        Self {
            instance: None,
            daemon_alive: false,
            identity_matches: false,
            manager: ManagerVerdict::None,
        }
    }
}

/// The manager record's liveness (plan Task 4's decision table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerVerdict {
    /// No manager record: the daemon is adoptable.
    None,
    /// The record is this app: the lease is already ours.
    Us,
    /// Another live app holds the lease.
    LiveOther,
    /// A record whose process is gone (or whose pid now belongs to another
    /// process): adoptable, the daemon just has not noticed yet.
    DeadOther,
}

/// What the manager is about to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Start a daemon: nothing usable is running.
    Spawn,
    /// `admin_hello` to take the lease of a daemon with none (or a dead one).
    Adopt,
    /// This app already holds the lease.
    AlreadyAttached,
    /// Another live app manages the daemon.
    BlockedByOtherApp,
    /// The daemon runs the wrong build: attach the lease, stop it, respawn.
    StopAndRespawn,
}

/// The decision table, as a pure function so every row is a test (plan Task 4
/// Step 1 asks for the table with injected fakes; this is the part that must
/// not drift).
pub fn decide(observation: &DaemonObservation) -> Result<Decision, DaemonManagerError> {
    let Some(file) = observation.instance.as_ref() else {
        return Ok(Decision::Spawn);
    };
    if !observation.daemon_alive {
        // Dead pid, reused pid, or a listener that answers nothing: the record
        // is stale, and the single-instance lock decides who may start.
        return Ok(Decision::Spawn);
    }
    if !observation.identity_matches {
        return match observation.manager {
            // Never stop a daemon another live app is driving: its capabilities
            // belong to that build, and stopping it is that app's decision.
            ManagerVerdict::LiveOther => Err(DaemonManagerError::StopWithoutLease {
                pid: file.pid,
                manager_pid: file.manager.as_ref().and_then(|manager| manager.pid),
            }),
            _ => Ok(Decision::StopAndRespawn),
        };
    }
    Ok(match observation.manager {
        ManagerVerdict::LiveOther => Decision::BlockedByOtherApp,
        ManagerVerdict::Us => Decision::AlreadyAttached,
        ManagerVerdict::None | ManagerVerdict::DeadOther => Decision::Adopt,
    })
}

/// A read-only prober (plan D3). Injected so the decision table runs without a
/// filesystem, a process table, or a socket.
#[async_trait::async_trait]
pub trait Probe: Send + Sync {
    fn instance(&self, path: &Path) -> Option<InstanceFile>;
    fn pid_alive(&self, pid: u32) -> bool;
    fn process_start_time(&self, pid: u32) -> Option<String>;
    /// `GET /health` on the daemon's host origin.
    async fn health(&self, host_origin: &str) -> bool;
}

/// The real prober: the instance file, `kill(pid, 0)` + `ps` for identity, and
/// `GET /health` with a 2s budget.
pub struct SystemProbe;

#[async_trait::async_trait]
impl Probe for SystemProbe {
    fn instance(&self, path: &Path) -> Option<InstanceFile> {
        instance::read(path)
    }

    fn pid_alive(&self, pid: u32) -> bool {
        child_supervision::pid_is_alive(pid)
    }

    fn process_start_time(&self, pid: u32) -> Option<String> {
        child_supervision::process_start_time(pid)
    }

    async fn health(&self, host_origin: &str) -> bool {
        let Ok(client) = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
        else {
            return false;
        };
        match client.get(format!("{host_origin}/health")).send().await {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }
}

/// A daemon this app started, with the handle the exit waiter needs.
#[derive(Debug)]
pub struct SpawnedDaemon {
    pub pid: u32,
    /// `None` only in tests: a real spawn always keeps the handle.
    pub child: Option<Child>,
}

/// Starts daemon processes. Injected so the table runs without real processes.
pub trait Launcher: Send + Sync {
    fn spawn(&self, cfg: &ManagerConfig) -> Result<SpawnedDaemon, String>;
}

/// The real launcher: detached, own process group, stdio to null.
pub struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn spawn(&self, cfg: &ManagerConfig) -> Result<SpawnedDaemon, String> {
        let mut command = Command::new(&cfg.daemon_binary);
        command
            .arg("--data-dir")
            .arg(&cfg.data_dir)
            .arg("--config-dir")
            .arg(&cfg.config_dir)
            .arg("--static-dir")
            .arg(&cfg.static_dir)
            .arg("--bundle-identifier")
            .arg(&cfg.bundle_identifier)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own process group: a signal aimed at Picot's group (Ctrl-C in
            // `tauri dev`, a terminal closing) must not take the daemon with it.
            // This is what makes "kill -9 the app, the daemon keeps running"
            // true rather than hoped for.
            command.process_group(0);
        }
        let child = command
            .spawn()
            .map_err(|error| format!("{}: {error}", cfg.daemon_binary.display()))?;
        Ok(SpawnedDaemon {
            pid: child.id(),
            child: Some(child),
        })
    }
}

/// Where the admin channel lives for one instance.
#[derive(Debug, Clone)]
pub struct AdminEndpoint {
    pub origin: String,
    pub token: String,
}

/// The result of asking a daemon for its manager lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseOutcome {
    /// The lease was granted (the daemon also incremented its attachment epoch).
    Granted { epoch: u64 },
    /// Another live manager holds the lease (close 4409).
    LeaseHeld,
}

/// The admin-channel operations the manager needs. Injected for the table
/// tests; the real implementation is a loopback WS client ([`WsAdminLink`]).
#[async_trait::async_trait]
pub trait AdminLink: Send + Sync {
    async fn hello(
        &self,
        endpoint: &AdminEndpoint,
        identity: &ManagerIdentity,
    ) -> Result<LeaseOutcome, String>;

    /// Attach the lease and request `daemon.stop` on the same connection.
    /// `Ok(LeaseOutcome::LeaseHeld)` means the daemon refused: another live app
    /// manages it, and this app must not stop it.
    async fn stop(
        &self,
        endpoint: &AdminEndpoint,
        identity: &ManagerIdentity,
    ) -> Result<LeaseOutcome, String>;
}

/// The real admin link: connect, `admin_hello`, and (for `stop`) one
/// `daemon.stop` request. A full client with a mirror and a pending-map arrives
/// with T5; this is only what the manager's own operations need, and the socket
/// closes as soon as the transaction is done.
///
/// Holding the manager lease over a connection that then closes is harmless in
/// this task: the daemon's dead-attachment grace window clears the record, and
/// nothing in T4 depends on the lease afterwards.
pub struct WsAdminLink;

type AdminSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// One parsed admin frame.
#[derive(Debug)]
enum AdminFrame {
    Welcome(serde_json::Value),
    Result(serde_json::Value),
    Closed { code: u16, reason: String },
}

impl WsAdminLink {
    async fn connect(endpoint: &AdminEndpoint) -> Result<AdminSocket, String> {
        let url = format!(
            "{}{}",
            endpoint.origin.replacen("http://", "ws://", 1),
            picot_host::daemon::admin::ADMIN_WS_PATH
        );
        match tokio::time::timeout(ADMIN_TIMEOUT, tokio_tungstenite::connect_async(&url)).await {
            Ok(Ok((socket, _response))) => Ok(socket),
            Ok(Err(error)) => Err(format!("cannot connect to {url}: {error}")),
            Err(_) => Err(format!("connecting to {url} timed out")),
        }
    }

    /// `admin_hello`, returning the grant or the reason it was refused.
    async fn attach(
        socket: &mut AdminSocket,
        endpoint: &AdminEndpoint,
        identity: &ManagerIdentity,
    ) -> Result<LeaseOutcome, String> {
        let hello = serde_json::json!({
            "type": "admin_hello",
            "adminToken": endpoint.token,
            "role": "manager",
            "appVersion": identity.app_version,
            "buildId": identity.build_id,
            "pid": identity.pid,
        });
        send_text(socket, &hello.to_string()).await?;
        match read_frame(socket).await? {
            AdminFrame::Welcome(welcome) => Ok(LeaseOutcome::Granted {
                epoch: welcome["epoch"].as_u64().unwrap_or(0),
            }),
            AdminFrame::Closed { code, reason } => match code {
                picot_host::daemon::admin::ADMIN_CLOSE_LEASE_HELD => Ok(LeaseOutcome::LeaseHeld),
                picot_host::daemon::admin::ADMIN_CLOSE_BAD_TOKEN => Err(format!(
                    "the instance file's admin token was rejected ({reason}); the file \
                     belongs to a different daemon"
                )),
                other => Err(format!("the admin channel closed with {other}: {reason}")),
            },
            AdminFrame::Result(_) => Err("the daemon answered before the handshake".to_string()),
        }
    }
}

#[async_trait::async_trait]
impl AdminLink for WsAdminLink {
    async fn hello(
        &self,
        endpoint: &AdminEndpoint,
        identity: &ManagerIdentity,
    ) -> Result<LeaseOutcome, String> {
        let mut socket = Self::connect(endpoint).await?;
        Self::attach(&mut socket, endpoint, identity).await
    }

    async fn stop(
        &self,
        endpoint: &AdminEndpoint,
        identity: &ManagerIdentity,
    ) -> Result<LeaseOutcome, String> {
        let mut socket = Self::connect(endpoint).await?;
        let granted = match Self::attach(&mut socket, endpoint, identity).await? {
            LeaseOutcome::LeaseHeld => return Ok(LeaseOutcome::LeaseHeld),
            granted => granted,
        };
        let request = serde_json::json!({
            "type": "admin_request",
            "id": "stop-1",
            "op": "daemon.stop",
            "args": {},
        });
        send_text(&mut socket, &request.to_string()).await?;
        // The daemon replies `accepted` and then drains, closing this socket as
        // drain step 2 — so an early close *is* the accepted answer. What
        // matters either way is that the app then confirms the exit by identity
        // (plan D12); this read only catches a refusal early.
        match read_frame(&mut socket).await {
            Ok(AdminFrame::Result(result)) if result["ok"] == serde_json::Value::Bool(true) => {
                Ok(granted)
            }
            Ok(AdminFrame::Result(result)) => Err(format!(
                "daemon.stop was refused: {}",
                result["error"].as_str().unwrap_or("unknown reason")
            )),
            Ok(AdminFrame::Closed { .. }) => Ok(granted),
            Ok(AdminFrame::Welcome(_)) => Err("the admin channel re-handshook".to_string()),
            Err(_) => Ok(granted),
        }
    }
}

async fn send_text(socket: &mut AdminSocket, text: &str) -> Result<(), String> {
    use futures_util::SinkExt;
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
        .await
        .map_err(|error| format!("cannot write to the admin channel: {error}"))
}

async fn read_frame(socket: &mut AdminSocket) -> Result<AdminFrame, String> {
    use futures_util::StreamExt;
    loop {
        let message = match tokio::time::timeout(ADMIN_TIMEOUT, socket.next()).await {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(error))) => return Err(format!("the admin channel failed: {error}")),
            // The daemon closes the admin socket as drain step 2: a normal end
            // of the `stop` transaction, not a failure.
            Ok(None) => {
                return Ok(AdminFrame::Closed {
                    code: 0,
                    reason: "the daemon is shutting down".to_string(),
                })
            }
            Err(_) => return Err("the admin channel did not answer in time".to_string()),
        };
        match message {
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                let value: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(value) => value,
                    Err(error) => return Err(format!("malformed admin frame: {error}")),
                };
                match value["type"].as_str() {
                    Some("admin_welcome") => return Ok(AdminFrame::Welcome(value)),
                    Some("admin_result") => return Ok(AdminFrame::Result(value)),
                    _ => continue,
                }
            }
            tokio_tungstenite::tungstenite::Message::Close(frame) => {
                let (code, reason) = match frame {
                    Some(frame) => (u16::from(frame.code), frame.reason.to_string()),
                    None => (0, String::new()),
                };
                return Ok(AdminFrame::Closed { code, reason });
            }
            _ => continue,
        }
    }
}

/// The app's daemon manager.
pub struct DaemonManager {
    cfg: ManagerConfig,
    probe: Arc<dyn Probe>,
    launcher: Arc<dyn Launcher>,
    admin: Arc<dyn AdminLink>,
    exit_waiter: ExitWaiter,
    spawned: Mutex<Option<SpawnedDaemon>>,
}

impl DaemonManager {
    pub fn new(
        cfg: ManagerConfig,
        probe: Arc<dyn Probe>,
        launcher: Arc<dyn Launcher>,
        admin: Arc<dyn AdminLink>,
        exit_waiter: ExitWaiter,
    ) -> Self {
        Self {
            cfg,
            probe,
            launcher,
            admin,
            exit_waiter,
            spawned: Mutex::new(None),
        }
    }

    /// The production wiring.
    pub fn for_app(cfg: ManagerConfig) -> Self {
        Self::new(
            cfg,
            Arc::new(SystemProbe),
            Arc::new(ProcessLauncher),
            Arc::new(WsAdminLink),
            ExitWaiter::new(),
        )
    }

    pub fn config(&self) -> &ManagerConfig {
        &self.cfg
    }

    /// Take the spawned child handle for the confirmed-exit waiter (T8/T9).
    pub fn take_child(&self) -> Option<Child> {
        self.spawned
            .lock()
            .ok()
            .and_then(|mut slot| slot.as_mut().and_then(|spawned| spawned.child.take()))
    }

    pub fn spawned_pid(&self) -> Option<u32> {
        self.spawned
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|spawned| spawned.pid))
    }

    /// Probe, decide, act (plan Task 4 Step 2's numbered path).
    pub async fn ensure(&self) -> Result<EnsureOutcome, DaemonManagerError> {
        let observation = self.observe().await;
        match decide(&observation)? {
            Decision::Spawn => {
                self.spawn()?;
                self.wait_ready(self.cfg.ready_deadline).await
            }
            Decision::Adopt => {
                let file = observation
                    .instance
                    .clone()
                    .ok_or_else(|| self.not_ready("no instance file to adopt".to_string()))?;
                self.adopt(&file).await
            }
            Decision::AlreadyAttached => {
                let file = observation
                    .instance
                    .clone()
                    .ok_or_else(|| self.not_ready("no instance file recorded".to_string()))?;
                Ok(self.outcome_for(file))
            }
            Decision::BlockedByOtherApp => Ok(EnsureOutcome::BlockedByOtherApp),
            Decision::StopAndRespawn => {
                let file = observation
                    .instance
                    .clone()
                    .ok_or_else(|| self.not_ready("no instance file to stop".to_string()))?;
                self.stop_stale_daemon(&file).await?;
                self.spawn()?;
                self.wait_ready(self.cfg.ready_deadline).await
            }
        }
    }

    /// One read-only probe pass: instance file, process identity, `/health`.
    async fn observe(&self) -> DaemonObservation {
        let Some(file) = self.probe.instance(&self.cfg.instance_path()) else {
            return DaemonObservation::absent();
        };
        let recorded = file.process_start_time.clone();
        let current = self.probe.process_start_time(file.pid);
        let same_process = match (recorded.as_deref(), current.as_deref()) {
            (Some(recorded), Some(current)) => recorded == current,
            // No recorded start time (an older file) or `ps` unavailable: a live
            // pid is the best available evidence.
            _ => self.probe.pid_alive(file.pid),
        };
        let daemon_alive = self.probe.pid_alive(file.pid)
            && same_process
            && self.probe.health(&file.host_origin).await;
        let manager = self.manager_verdict(&file);
        let identity_matches =
            file.version == self.cfg.app_version && file.build_id == self.cfg.build_id;
        DaemonObservation {
            instance: Some(file),
            daemon_alive,
            identity_matches,
            manager,
        }
    }

    /// pid + start-time liveness of the recorded manager.
    fn manager_verdict(&self, file: &InstanceFile) -> ManagerVerdict {
        let Some(manager) = file.manager.as_ref() else {
            return ManagerVerdict::None;
        };
        if manager.pid == Some(self.cfg.self_pid) {
            return ManagerVerdict::Us;
        }
        let Some(pid) = manager.pid else {
            // A record with no pid: refuse to adopt rather than risk kicking a
            // live app (the daemon clears the record when the attachment dies).
            return ManagerVerdict::LiveOther;
        };
        if !self.probe.pid_alive(pid) {
            return ManagerVerdict::DeadOther;
        }
        match (
            manager.process_start_time.as_deref(),
            self.probe.process_start_time(pid).as_deref(),
        ) {
            (Some(recorded), Some(current)) if recorded == current => ManagerVerdict::LiveOther,
            (Some(_), Some(_)) => ManagerVerdict::DeadOther,
            // The record has no start time (or `ps` failed): a live pid is the
            // live manager.
            _ => ManagerVerdict::LiveOther,
        }
    }

    /// Distinguish "we started it" from "we attached to someone else's daemon".
    fn outcome_for(&self, instance: InstanceFile) -> EnsureOutcome {
        match self.spawned_pid() {
            Some(pid) if pid == instance.pid => EnsureOutcome::Started(instance),
            _ => EnsureOutcome::Attached(instance),
        }
    }

    fn spawn(&self) -> Result<u32, DaemonManagerError> {
        let spawned = self
            .launcher
            .spawn(&self.cfg)
            .map_err(|message| DaemonManagerError::Spawn { message })?;
        log::info!(
            "[picot] started picot-daemon pid={} data-dir={}",
            spawned.pid,
            self.cfg.data_dir.display()
        );
        let pid = spawned.pid;
        if let Ok(mut slot) = self.spawned.lock() {
            *slot = Some(spawned);
        }
        Ok(pid)
    }

    /// Adopt a live daemon: the daemon grants the lease and rewrites its own
    /// manager record; the app re-reads the file for the authoritative copy.
    async fn adopt(&self, file: &InstanceFile) -> Result<EnsureOutcome, DaemonManagerError> {
        let endpoint = self.cfg.endpoint(file);
        match self.admin.hello(&endpoint, &self.cfg.identity()).await {
            Ok(LeaseOutcome::Granted { epoch }) => {
                log::info!(
                    "[picot] adopted picot-daemon pid={} epoch={epoch}",
                    file.pid
                );
                let adopted = self
                    .probe
                    .instance(&self.cfg.instance_path())
                    .unwrap_or_else(|| file.clone());
                Ok(self.outcome_for(adopted))
            }
            // A lost race is an outcome, not an error: another app won the
            // lease while we were probing.
            Ok(LeaseOutcome::LeaseHeld) => Ok(EnsureOutcome::BlockedByOtherApp),
            Err(message) => Err(DaemonManagerError::Admin { message }),
        }
    }

    /// Version/build mismatch: attach the lease **first**, then stop, then
    /// confirm the exit by identity before anything replaces it (plan D10/D12).
    async fn stop_stale_daemon(&self, file: &InstanceFile) -> Result<(), DaemonManagerError> {
        let endpoint = self.cfg.endpoint(file);
        match self.admin.stop(&endpoint, &self.cfg.identity()).await {
            Ok(LeaseOutcome::Granted { .. }) => {}
            Ok(LeaseOutcome::LeaseHeld) => {
                return Err(DaemonManagerError::StopWithoutLease {
                    pid: file.pid,
                    manager_pid: file.manager.as_ref().and_then(|manager| manager.pid),
                })
            }
            Err(message) => return Err(DaemonManagerError::Admin { message }),
        }
        let identity = ProcessIdentity::new(file.pid, file.process_start_time.clone());
        match self
            .exit_waiter
            .confirm_exit(&identity, None, STOP_CONFIRM_DEADLINE)
            .await
        {
            ExitOutcome::Confirmed | ExitOutcome::Escalated { .. } => Ok(()),
            ExitOutcome::Unconfirmed => Err(DaemonManagerError::NotReady {
                deadline: STOP_CONFIRM_DEADLINE,
                log_path: self.cfg.log_path(),
                last: format!("pid {} survived the stop ladder", file.pid),
            }),
        }
    }

    /// Wait for a usable daemon: `/health` + a live matching identity + a lease
    /// we can hold or take. This is also where a lost startup race resolves —
    /// the winner's live manager record returns
    /// [`EnsureOutcome::BlockedByOtherApp`] instead of a timeout.
    pub async fn wait_ready(
        &self,
        deadline: Duration,
    ) -> Result<EnsureOutcome, DaemonManagerError> {
        let started = Instant::now();
        loop {
            let observation = self.observe().await;
            match decide(&observation)? {
                Decision::Adopt => {
                    let file = observation
                        .instance
                        .clone()
                        .ok_or_else(|| self.not_ready("no instance file to adopt".to_string()))?;
                    return self.adopt(&file).await;
                }
                Decision::AlreadyAttached => {
                    let file = observation
                        .instance
                        .clone()
                        .ok_or_else(|| self.not_ready("no instance file recorded".to_string()))?;
                    return Ok(self.outcome_for(file));
                }
                Decision::BlockedByOtherApp => return Ok(EnsureOutcome::BlockedByOtherApp),
                // Nothing usable yet: the daemon is still booting, or the lock
                // holder has not published its instance file.
                Decision::Spawn => {}
                // A live daemon of a different build appeared while we waited
                // (another app upgrading). Stopping it is its own transaction,
                // so report instead of looping.
                Decision::StopAndRespawn => {
                    return Err(
                        self.not_ready("a daemon with a different build is running".to_string())
                    )
                }
            }
            if started.elapsed() >= deadline {
                return Err(self.not_ready("no daemon published a ready instance file".to_string()));
            }
            tokio::time::sleep(READY_POLL).await;
        }
    }

    fn not_ready(&self, last: String) -> DaemonManagerError {
        DaemonManagerError::NotReady {
            deadline: self.cfg.ready_deadline,
            log_path: self.cfg.log_path(),
            last,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use picot_host::daemon::instance::ManagerInfo;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn instance_with(version: &str, build: &str, manager: Option<ManagerInfo>) -> InstanceFile {
        InstanceFile {
            pid: 9001,
            process_start_time: Some("Mon Oct 13 09:15:02 2026".to_string()),
            host_origin: "http://127.0.0.1:51000".to_string(),
            admin_origin: "http://127.0.0.1:51001".to_string(),
            admin_token: "token".to_string(),
            started_at: "2026-10-13T09:15:02Z".to_string(),
            version: version.to_string(),
            build_id: build.to_string(),
            manager,
        }
    }

    fn manager_info(pid: Option<u32>, start_time: Option<&str>) -> ManagerInfo {
        ManagerInfo {
            pid,
            process_start_time: start_time.map(str::to_string),
            started_at: "2026-10-13T09:15:03Z".to_string(),
            app_version: Some("0.5.0".to_string()),
            build_id: Some("build-1".to_string()),
        }
    }

    fn observation(
        instance: InstanceFile,
        daemon_alive: bool,
        identity_matches: bool,
        manager: ManagerVerdict,
    ) -> DaemonObservation {
        DaemonObservation {
            instance: Some(instance),
            daemon_alive,
            identity_matches,
            manager,
        }
    }

    // ── The decision table (plan Task 4 Step 1) ─────────────────────────────

    /// No file → spawn.
    #[test]
    fn no_instance_file_spawns() {
        assert_eq!(
            decide(&DaemonObservation::absent()).unwrap(),
            Decision::Spawn
        );
    }

    /// A dead daemon pid → spawn.
    #[test]
    fn a_dead_daemon_pid_spawns() {
        let file = instance_with("0.5.0", "build-1", None);
        let decision = decide(&observation(file, false, true, ManagerVerdict::None)).unwrap();
        assert_eq!(decision, Decision::Spawn);
    }

    /// The pid-reuse row: the *probe* collapses "pid alive, start time
    /// different" into `daemon_alive = false`, so the table must spawn.
    #[test]
    fn a_reused_pid_is_not_a_daemon() {
        let file = instance_with("0.5.0", "build-1", None);
        let decision = decide(&observation(file, false, true, ManagerVerdict::None)).unwrap();
        assert_eq!(decision, Decision::Spawn);
    }

    /// A live daemon without a manager record → adopt.
    #[test]
    fn a_managerless_daemon_is_adopted() {
        let file = instance_with("0.5.0", "build-1", None);
        let decision = decide(&observation(file, true, true, ManagerVerdict::None)).unwrap();
        assert_eq!(decision, Decision::Adopt);
    }

    /// A live daemon whose recorded manager is dead → adopt.
    #[test]
    fn a_daemon_whose_manager_died_is_adopted() {
        let file = instance_with("0.5.0", "build-1", Some(manager_info(Some(7), Some("old"))));
        let decision = decide(&observation(file, true, true, ManagerVerdict::DeadOther)).unwrap();
        assert_eq!(decision, Decision::Adopt);
    }

    /// A live daemon with a *live* other manager → blocked.
    #[test]
    fn a_daemon_managed_by_a_live_other_app_blocks() {
        let file = instance_with("0.5.0", "build-1", Some(manager_info(Some(7), Some("now"))));
        let decision = decide(&observation(file, true, true, ManagerVerdict::LiveOther)).unwrap();
        assert_eq!(decision, Decision::BlockedByOtherApp);
    }

    /// A manager record without a pid is conservative: block, never adopt.
    #[test]
    fn a_manager_record_without_a_pid_blocks() {
        let file = instance_with("0.5.0", "build-1", Some(manager_info(None, None)));
        let decision = decide(&observation(file, true, true, ManagerVerdict::LiveOther)).unwrap();
        assert_eq!(decision, Decision::BlockedByOtherApp);
    }

    /// The lease is already ours: idempotent attach, no second hello.
    #[test]
    fn our_own_manager_record_is_already_attached() {
        let file = instance_with(
            "0.5.0",
            "build-1",
            Some(manager_info(Some(777), Some("now"))),
        );
        let decision = decide(&observation(file, true, true, ManagerVerdict::Us)).unwrap();
        assert_eq!(decision, Decision::AlreadyAttached);
    }

    /// A live daemon of a different build is stopped (lease first) and respawned;
    /// the version *or* the build id alone is enough to be stale.
    #[test]
    fn a_build_mismatch_is_stopped_and_respawned() {
        let older = instance_with("0.4.9", "build-1", None);
        assert_eq!(
            decide(&observation(older, true, false, ManagerVerdict::None)).unwrap(),
            Decision::StopAndRespawn
        );
        let rebuilt = instance_with("0.5.0", "build-other", None);
        assert_eq!(
            decide(&observation(rebuilt, true, false, ManagerVerdict::None)).unwrap(),
            Decision::StopAndRespawn
        );
    }

    /// "Version-mismatch stop without lease → rejected": a stale build behind
    /// another live app is refused, never stopped.
    #[test]
    fn a_stale_build_managed_by_another_live_app_is_refused() {
        let file = instance_with("0.4.9", "build-1", Some(manager_info(Some(7), Some("now"))));
        let error = decide(&observation(file, true, false, ManagerVerdict::LiveOther))
            .expect_err("this app must not stop another app's daemon");
        match error {
            DaemonManagerError::StopWithoutLease { pid, manager_pid } => {
                assert_eq!(pid, 9001);
                assert_eq!(manager_pid, Some(7));
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    // ── End-to-end paths with injected fakes ────────────────────────────────

    #[derive(Default)]
    struct Calls {
        spawn: AtomicUsize,
        hello: AtomicUsize,
        stop: AtomicUsize,
    }

    /// A probe whose answers come from the scripted observation stream: later
    /// passes repeat the last entry. Every answer is *derived* from the current
    /// observation, so a test only has to state what it wants the world to look
    /// like.
    struct FakeProbe {
        observations: Mutex<Vec<DaemonObservation>>,
        next: AtomicUsize,
        current: AtomicUsize,
    }

    impl FakeProbe {
        fn scripted(observations: Vec<DaemonObservation>) -> Arc<Self> {
            assert!(!observations.is_empty(), "the script needs one entry");
            Arc::new(Self {
                observations: Mutex::new(observations),
                next: AtomicUsize::new(0),
                current: AtomicUsize::new(0),
            })
        }

        fn active(&self) -> DaemonObservation {
            let observations = self.observations.lock().unwrap();
            observations[self.current.load(Ordering::SeqCst)].clone()
        }
    }

    #[async_trait::async_trait]
    impl Probe for FakeProbe {
        fn instance(&self, _path: &Path) -> Option<InstanceFile> {
            let index = {
                let observations = self.observations.lock().unwrap();
                self.next
                    .fetch_add(1, Ordering::SeqCst)
                    .min(observations.len() - 1)
            };
            self.current.store(index, Ordering::SeqCst);
            self.observations.lock().unwrap()[index].instance.clone()
        }

        fn pid_alive(&self, pid: u32) -> bool {
            let active = self.active();
            let Some(file) = active.instance.as_ref() else {
                return false;
            };
            if file.pid == pid {
                // The recorded daemon: alive by construction, and whether it is
                // *usable* is what `daemon_alive` decides through `/health`.
                return true;
            }
            file.manager
                .as_ref()
                .filter(|manager| manager.pid == Some(pid))
                .is_some_and(|_| matches!(active.manager, ManagerVerdict::LiveOther))
        }

        fn process_start_time(&self, pid: u32) -> Option<String> {
            let active = self.active();
            let file = active.instance.as_ref()?;
            if file.pid == pid {
                return file.process_start_time.clone();
            }
            let manager = file.manager.as_ref().filter(|m| m.pid == Some(pid))?;
            match active.manager {
                ManagerVerdict::LiveOther => manager.process_start_time.clone(),
                // A pid that now belongs to another process: the start time
                // differs, which is what makes the record stale.
                ManagerVerdict::DeadOther => Some("Tue Oct 14 07:00:00 2026".to_string()),
                _ => None,
            }
        }

        async fn health(&self, _host_origin: &str) -> bool {
            self.active().daemon_alive
        }
    }

    struct FakeLauncher {
        calls: Arc<Calls>,
        fail: bool,
    }

    impl Launcher for FakeLauncher {
        fn spawn(&self, _cfg: &ManagerConfig) -> Result<SpawnedDaemon, String> {
            self.calls.spawn.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(format!("{}: no such binary", _cfg.daemon_binary.display()));
            }
            Ok(SpawnedDaemon {
                pid: 4242,
                child: None,
            })
        }
    }

    struct FakeAdminLink {
        calls: Arc<Calls>,
        hello: LeaseOutcome,
        stop: LeaseOutcome,
    }

    #[async_trait::async_trait]
    impl AdminLink for FakeAdminLink {
        async fn hello(
            &self,
            _endpoint: &AdminEndpoint,
            _identity: &ManagerIdentity,
        ) -> Result<LeaseOutcome, String> {
            self.calls.hello.fetch_add(1, Ordering::SeqCst);
            Ok(self.hello.clone())
        }

        async fn stop(
            &self,
            _endpoint: &AdminEndpoint,
            _identity: &ManagerIdentity,
        ) -> Result<LeaseOutcome, String> {
            self.calls.stop.fetch_add(1, Ordering::SeqCst);
            Ok(self.stop.clone())
        }
    }

    fn cfg(dir: &Path) -> ManagerConfig {
        ManagerConfig {
            daemon_binary: dir.join(DAEMON_BINARY_NAME),
            data_dir: dir.to_path_buf(),
            config_dir: dir.join("config"),
            static_dir: dir.join("static"),
            bundle_identifier: "com.palandata.picot.test".to_string(),
            app_version: "0.5.0".to_string(),
            build_id: "build-1".to_string(),
            self_pid: 777,
            self_start_time: Some("Mon Oct 13 09:15:00 2026".to_string()),
            ready_deadline: Duration::from_millis(150),
        }
    }

    struct Harness {
        _temp: tempfile::TempDir,
        manager: DaemonManager,
        calls: Arc<Calls>,
    }

    fn harness(
        observations: Vec<DaemonObservation>,
        admin: (LeaseOutcome, LeaseOutcome),
    ) -> Harness {
        harness_with_waiter(observations, admin, ExitWaiter::new())
    }

    fn harness_with_waiter(
        observations: Vec<DaemonObservation>,
        admin: (LeaseOutcome, LeaseOutcome),
        waiter: ExitWaiter,
    ) -> Harness {
        let temp = tempfile::tempdir().unwrap();
        let calls = Arc::new(Calls::default());
        let manager = DaemonManager::new(
            cfg(temp.path()),
            FakeProbe::scripted(observations),
            Arc::new(FakeLauncher {
                calls: calls.clone(),
                fail: false,
            }),
            Arc::new(FakeAdminLink {
                calls: calls.clone(),
                hello: admin.0,
                stop: admin.1,
            }),
            waiter.with_timing(Duration::from_millis(5), Duration::from_millis(10)),
        );
        Harness {
            _temp: temp,
            manager,
            calls,
        }
    }

    /// Spawn when nothing runs — and never touch the admin channel while
    /// probing. Nothing becomes ready in the scripted world, so the honest
    /// outcome is the deadline error carrying the daemon log path.
    #[tokio::test]
    async fn ensure_spawns_when_nothing_runs_and_never_touches_the_admin_channel() {
        let h = harness(
            vec![DaemonObservation::absent()],
            (
                LeaseOutcome::Granted { epoch: 1 },
                LeaseOutcome::Granted { epoch: 1 },
            ),
        );
        let outcome = h.manager.ensure().await;
        assert_eq!(h.calls.spawn.load(Ordering::SeqCst), 1, "exactly one spawn");
        assert_eq!(
            h.calls.hello.load(Ordering::SeqCst),
            0,
            "probing never opens the admin channel"
        );
        match outcome {
            Err(DaemonManagerError::NotReady { log_path, .. }) => {
                assert_eq!(log_path, instance::log_path(&h.manager.config().data_dir));
            }
            other => panic!("expected a ready timeout, got {other:?}"),
        }
    }

    /// A spawned daemon that comes up is `Started` (the instance pid is ours).
    #[tokio::test]
    async fn ensure_reports_started_for_the_daemon_it_spawned() {
        let mut ours = instance_with("0.5.0", "build-1", None);
        ours.pid = 4242;
        let h = harness(
            vec![
                DaemonObservation::absent(),
                observation(ours, true, true, ManagerVerdict::None),
            ],
            (
                LeaseOutcome::Granted { epoch: 1 },
                LeaseOutcome::Granted { epoch: 1 },
            ),
        );
        let outcome = h.manager.ensure().await.expect("started");
        match outcome {
            EnsureOutcome::Started(file) => assert_eq!(file.pid, 4242),
            other => panic!("expected Started, got {other:?}"),
        }
        assert_eq!(h.calls.spawn.load(Ordering::SeqCst), 1);
        assert_eq!(
            h.calls.hello.load(Ordering::SeqCst),
            1,
            "adopted its own daemon"
        );
    }

    #[tokio::test]
    async fn ensure_adopts_a_ready_daemon_without_spawning() {
        let file = instance_with("0.5.0", "build-1", None);
        let live = observation(file.clone(), true, true, ManagerVerdict::None);
        let h = harness(
            vec![live],
            (
                LeaseOutcome::Granted { epoch: 3 },
                LeaseOutcome::Granted { epoch: 3 },
            ),
        );
        match h.manager.ensure().await.expect("adopted") {
            EnsureOutcome::Attached(adopted) => assert_eq!(adopted.pid, file.pid),
            other => panic!("expected Attached, got {other:?}"),
        }
        assert_eq!(h.calls.spawn.load(Ordering::SeqCst), 0, "no spawn");
        assert_eq!(h.calls.hello.load(Ordering::SeqCst), 1, "one hello");
    }

    #[tokio::test]
    async fn ensure_blocks_when_another_live_app_manages_the_daemon() {
        let file = instance_with("0.5.0", "build-1", Some(manager_info(Some(7), Some("now"))));
        let h = harness(
            vec![observation(file, true, true, ManagerVerdict::LiveOther)],
            (LeaseOutcome::LeaseHeld, LeaseOutcome::LeaseHeld),
        );
        assert_eq!(
            h.manager.ensure().await.expect("an outcome, not an error"),
            EnsureOutcome::BlockedByOtherApp
        );
        assert_eq!(
            h.calls.spawn.load(Ordering::SeqCst),
            0,
            "never spawn past it"
        );
        assert_eq!(h.calls.hello.load(Ordering::SeqCst), 0, "no kick attempt");
    }

    /// The adoption race is resolved by the daemon's lease: a lost race is
    /// `BlockedByOtherApp`, not an error.
    #[tokio::test]
    async fn a_lost_adoption_race_reports_blocked() {
        let file = instance_with("0.5.0", "build-1", None);
        let h = harness(
            vec![observation(file, true, true, ManagerVerdict::None)],
            (LeaseOutcome::LeaseHeld, LeaseOutcome::LeaseHeld),
        );
        assert_eq!(
            h.manager.ensure().await.expect("outcome"),
            EnsureOutcome::BlockedByOtherApp
        );
        assert_eq!(h.calls.hello.load(Ordering::SeqCst), 1);
    }

    /// The startup race: two apps spawn, the lock admits one daemon, the loser
    /// discovers the winner's live manager record while waiting.
    #[tokio::test]
    async fn a_lost_startup_race_reports_blocked() {
        let winner = instance_with("0.5.0", "build-1", Some(manager_info(Some(7), Some("now"))));
        let h = harness(
            vec![
                DaemonObservation::absent(),
                observation(winner, true, true, ManagerVerdict::LiveOther),
            ],
            (LeaseOutcome::LeaseHeld, LeaseOutcome::LeaseHeld),
        );
        assert_eq!(
            h.manager.ensure().await.expect("outcome"),
            EnsureOutcome::BlockedByOtherApp
        );
        assert_eq!(
            h.calls.spawn.load(Ordering::SeqCst),
            1,
            "the loser did spawn; the daemon's lock refused the second process"
        );
        assert_eq!(
            h.calls.hello.load(Ordering::SeqCst),
            0,
            "the loser never even asks for the winner's lease"
        );
    }

    /// A daemon whose build differs is stopped before anything replaces it.
    #[tokio::test]
    async fn a_build_mismatch_stops_the_daemon_before_respawning() {
        let stale = instance_with("0.4.9", "build-old", None);
        let mut fresh = instance_with("0.5.0", "build-1", None);
        // The replacement is the daemon this app spawned.
        fresh.pid = 4242;
        let h = harness(
            vec![
                observation(stale, true, false, ManagerVerdict::None),
                DaemonObservation::absent(),
                observation(fresh, true, true, ManagerVerdict::None),
            ],
            (
                LeaseOutcome::Granted { epoch: 9 },
                LeaseOutcome::Granted { epoch: 9 },
            ),
        );
        let outcome = h.manager.ensure().await.expect("respawned");
        assert!(
            matches!(outcome, EnsureOutcome::Started(_)),
            "the replacement is ours: {outcome:?}"
        );
        assert_eq!(
            h.calls.stop.load(Ordering::SeqCst),
            1,
            "stopped the stale build"
        );
        assert_eq!(h.calls.spawn.load(Ordering::SeqCst), 1, "one replacement");
    }

    /// A stale build that outlives the stop ladder is reported with the daemon
    /// log path instead of being replaced blindly (plan D12).
    /// A fake process that ignores everything: the stop ladder can never
    /// confirm its exit, so nothing may be spawned against it.
    struct ImmortalHandle;

    impl super::super::exit_waiter::ProcessHandle for ImmortalHandle {
        fn alive(&self, _pid: u32) -> bool {
            true
        }

        fn start_time(&self, _pid: u32) -> Option<String> {
            Some("Mon Oct 13 09:15:02 2026".to_string())
        }

        fn signal(&self, _pid: u32, _signal: i32) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn an_unconfirmed_stop_aborts_before_respawning() {
        let stale = instance_with("0.4.9", "build-old", None);
        let h = harness_with_waiter(
            vec![observation(stale, true, false, ManagerVerdict::None)],
            (
                LeaseOutcome::Granted { epoch: 9 },
                LeaseOutcome::Granted { epoch: 9 },
            ),
            ExitWaiter::with_handle(Arc::new(ImmortalHandle)),
        );
        // The stop is accepted but the process never dies, so the waiter's
        // whole ladder runs and the outcome is Unconfirmed (plan D12).
        let error = h.manager.ensure().await.expect_err("refused to spawn");
        assert!(
            matches!(error, DaemonManagerError::NotReady { .. }),
            "{error:?}"
        );
        assert_eq!(
            h.calls.spawn.load(Ordering::SeqCst),
            0,
            "never spawn against a daemon whose death is unconfirmed"
        );
    }

    /// A stale build managed by another live app is refused outright: the app
    /// never issues the stop, so the incumbent daemon and app are untouched.
    #[tokio::test]
    async fn a_stale_build_behind_a_live_other_app_is_never_stopped() {
        let stale = instance_with(
            "0.4.9",
            "build-old",
            Some(manager_info(Some(7), Some("now"))),
        );
        let h = harness(
            vec![observation(stale, true, false, ManagerVerdict::LiveOther)],
            (LeaseOutcome::LeaseHeld, LeaseOutcome::LeaseHeld),
        );
        let error = h.manager.ensure().await.expect_err("refused");
        assert!(matches!(error, DaemonManagerError::StopWithoutLease { .. }));
        assert_eq!(h.calls.stop.load(Ordering::SeqCst), 0);
        assert_eq!(h.calls.spawn.load(Ordering::SeqCst), 0);
    }

    /// A probe that reports every pid as alive but with a *different* start
    /// time than the instance file recorded (the pid-reuse row).
    struct ReusedPidProbe;

    #[async_trait::async_trait]
    impl Probe for ReusedPidProbe {
        fn instance(&self, path: &Path) -> Option<InstanceFile> {
            instance::read(path)
        }
        fn pid_alive(&self, _pid: u32) -> bool {
            true
        }
        fn process_start_time(&self, _pid: u32) -> Option<String> {
            Some("Tue Oct 14 07:00:00 2026".to_string())
        }
        async fn health(&self, _host_origin: &str) -> bool {
            true
        }
    }

    /// The probe's own identity rule: a live pid whose recorded start time
    /// differs is a different process, so the daemon record is stale.
    #[tokio::test]
    async fn the_probe_treats_a_reused_daemon_pid_as_absent() {
        let temp = tempfile::tempdir().unwrap();
        let mut file = instance_with("0.5.0", "build-1", None);
        file.pid = 5150;
        instance::write_atomic(&instance::instance_path(temp.path()), &file).unwrap();

        let calls = Arc::new(Calls::default());
        let manager = DaemonManager::new(
            cfg(temp.path()),
            Arc::new(ReusedPidProbe),
            Arc::new(FakeLauncher {
                calls: calls.clone(),
                fail: false,
            }),
            Arc::new(FakeAdminLink {
                calls: calls.clone(),
                hello: LeaseOutcome::Granted { epoch: 1 },
                stop: LeaseOutcome::Granted { epoch: 1 },
            }),
            ExitWaiter::new(),
        );
        let observation = manager.observe().await;
        assert!(
            !observation.daemon_alive,
            "a pid whose start time changed is not the recorded daemon"
        );
        assert_eq!(decide(&observation).unwrap(), Decision::Spawn);
    }

    /// A spawn failure is reported with the binary path, not swallowed.
    #[tokio::test]
    async fn a_failed_spawn_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let calls = Arc::new(Calls::default());
        let manager = DaemonManager::new(
            cfg(temp.path()),
            FakeProbe::scripted(vec![DaemonObservation::absent()]),
            Arc::new(FakeLauncher {
                calls: calls.clone(),
                fail: true,
            }),
            Arc::new(FakeAdminLink {
                calls: calls.clone(),
                hello: LeaseOutcome::Granted { epoch: 1 },
                stop: LeaseOutcome::Granted { epoch: 1 },
            }),
            ExitWaiter::new(),
        );
        match manager.ensure().await {
            Err(DaemonManagerError::Spawn { message }) => {
                assert!(message.contains(DAEMON_BINARY_NAME), "{message}");
            }
            other => panic!("expected a spawn failure, got {other:?}"),
        }
    }

    /// The dev path the app resolves without a bundle T11 has to have built.
    #[test]
    fn the_dev_daemon_binary_resolves_next_to_the_app_manifest() {
        let resolved = resolve_daemon_binary();
        assert!(
            resolved.starts_with(env!("CARGO_MANIFEST_DIR")),
            "dev builds resolve the workspace artifact: {}",
            resolved.display()
        );
        assert_eq!(
            resolved.file_name().and_then(|name| name.to_str()),
            Some(DAEMON_BINARY_NAME)
        );
    }

    /// The release sidecar (plan Task 11 Step 2): the bundler copies
    /// `binaries/picot-daemon-<target-triple>` next to the app executable and
    /// drops the triple suffix, so the sidecar is `Contents/MacOS/picot-daemon`.
    #[test]
    fn the_release_daemon_binary_is_the_sidecar_beside_the_app_executable() {
        let resolved = resolve_daemon_binary_in(
            Some(Path::new("/Applications/Picot.app/Contents/MacOS")),
            false,
        );
        assert_eq!(
            resolved,
            PathBuf::from("/Applications/Picot.app/Contents/MacOS/picot-daemon")
        );
    }

    /// An unreadable `current_exe()` must not invent a bundle path: the bare
    /// name (a `PATH` lookup) is the last resort, and its failure surfaces as a
    /// spawn error naming the binary.
    #[test]
    fn the_release_path_without_an_exe_dir_falls_back_to_the_bare_name() {
        assert_eq!(
            resolve_daemon_binary_in(None, false),
            PathBuf::from(DAEMON_BINARY_NAME)
        );
    }

    /// The launch shape that makes "`kill -9` the app, the daemon keeps running"
    /// true rather than hoped for: the daemon leads its **own** process group
    /// (a signal aimed at Picot's group cannot reach it) and the launcher keeps
    /// no registry the app tears down on exit. Real process, real `getpgid`.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_launcher_detaches_the_daemon_into_its_own_process_group() {
        let temp = tempfile::tempdir().unwrap();
        // A stand-in "daemon": ProcessLauncher passes the daemon's argv, which
        // the script ignores.
        let stand_in = temp.path().join("stand-in-daemon");
        std::fs::write(&stand_in, "#!/bin/sh\nsleep 30\n").unwrap();
        let mut permissions = std::fs::metadata(&stand_in).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&stand_in, permissions).unwrap();

        let mut config = cfg(temp.path());
        config.daemon_binary = stand_in;
        let spawned = ProcessLauncher
            .spawn(&config)
            .expect("spawn the stand-in daemon");
        let pid = spawned.pid as libc::pid_t;

        let pgid = unsafe { libc::getpgid(pid) };
        assert_eq!(
            pgid, pid,
            "the daemon must lead its own process group, not the app's"
        );

        // The test owns this process; it is not the app's to kill.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        if let Some(mut child) = spawned.child {
            let _ = child.wait();
        }
    }
}
