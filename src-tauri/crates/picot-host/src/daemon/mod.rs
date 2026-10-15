// ABOUTME: Daemon-side state ownership: DaemonState::start, the app-surface seam, and (T3+) admin/instance.
// ABOUTME: Step 3 of plan Task 2 moved `setup_native_runtime`'s construction in here; the app keeps only app-only state.

//! Phase 1 (2026-10-15) moves the whole host stack out of the Tauri main
//! process (plan D1/D2). This module owns what `setup_native_runtime`
//! (main.rs, pre-T2) built in the app process and what the control dispatcher
//! needs to reach.
//!
//! Landed so far:
//!
//! - [`admin`] — the loopback admin WS: manager lease, attachment epochs, the
//!   navigation mirror and the drain order (Task 3).
//! - [`instance`] — the instance file, the single-instance lock and the daemon
//!   log path: atomic, owner-only-at-creation, token-redacted (Task 4).
//! - [`app_surface`] — the exhaustive `AppHandle`/`try_state` inventory of the
//!   control dispatcher and the object-safe [`app_surface::AppSurface`] seam
//!   every row maps onto (Task 2 Step 0).
//! - [`surface_forward`] — the daemon's realization of that seam: forward the
//!   row over the admin channel (`app_surface_request` / `surface_response`),
//!   which is the same surface in both modes (Task 5, plan D2/D5).
//! - [`DaemonState`] — the explicit Arc set the dispatcher reads, replacing
//!   every `app.try_state::<T>()` lookup (Task 2 Step 2, inventory rows
//!   D1–D4).
//! - [`DaemonState::start`] + [`DaemonConfig`] + [`DaemonState::shutdown_graceful`]
//!   (Task 2 Step 3): the whole host stack is constructed here from the argv
//!   surface alone, so a process with no app at all can boot it. The app still
//!   calls it in-process and hands over a `TauriAppSurface` (embedded form;
//!   T3 swaps in the admin-channel realization, T4 makes the binary use it).
//!
//! Not landed yet (tracked by the plan, not placeholders):
//!
//! - nothing in [`DrainStep::DrainFiles`]: the daemon's file step now removes
//!   the instance file and the lock (Task 4, [`DaemonState::set_daemon_files`]).

pub mod admin;
pub mod app_surface;
pub mod instance;
pub mod surface_forward;

use crate::daemon::admin::AdminServer;
use crate::daemon::app_surface::AppSurface;
use crate::daemon::instance::DaemonFiles;
use crate::environment_install;
use crate::ephemeral_registry::EphemeralRegistry;
use crate::git_service::GitService;
use crate::host_control::HostEventSink;
use crate::host_ephemeral::{EphemeralHub, SharedEphemeralHub};
use crate::host_server::{self, HostServer};
use crate::metadata_store::{MetadataStore, SharedMetadataStore};
use crate::native_pi_manager::NativePiManager;
use crate::officecli_watch::OfficecliWatchRuntime;
use crate::remote_auth::RemoteAuth;
use crate::session_ui_profile_store::SessionUiProfileStore;
use crate::skill_source_registry::SkillSourceRegistry;
use crate::terminal_manager::TerminalManager;
use crate::terminal_registry::TerminalRegistry;
use crate::terminal_state_store::TerminalStateStore;
use crate::window_owner::{OwnerId, WindowOwnerRegistry};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One step of the drain sequence, reported to a [`DrainObserver`].
///
/// The order is fixed by plan Task 3: stop accepting admin frames → close the
/// admin WS → drain runtimes → files → exit. `DrainFiles` is where T4 removes
/// the instance file and lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainStep {
    StopAcceptingAdminFrames,
    CloseAdminWs,
    DrainRuntimes,
    DrainFiles,
    Exit,
}

/// Observes drain steps so the sequence is assertable (fakes in tests) without
/// weakening the production path. Test seam, not a plugin point.
pub trait DrainObserver: Send + Sync {
    fn step(&self, step: DrainStep);
}

/// The app package no longer holds the per-process skill-install secret: its
/// only readers were dispatcher rows D2, which now read this field.
pub use crate::host_control_dispatch::HostInstallSecret;

/// The daemon's argv surface (plan D7): everything [`DaemonState::start`]
/// needs that is not the app surface.
///
/// Mirrors the `picot-daemon` flags `--data-dir / --config-dir / --static-dir
/// / --bundle-identifier`, so the embedded (T2) and the external (T4/T5) form
/// boot the same host stack through one code path.
pub struct DaemonConfig {
    /// The daemon's private data directory: the SQLite metadata store, the
    /// terminal state store, and the session-UI profile file all live here.
    pub data_dir: PathBuf,
    /// The daemon's config directory. Part of the argv contract; the host
    /// stack does not read it yet (T4's instance file lives under `data_dir`).
    pub config_dir: PathBuf,
    /// The WebView static bundle (`index.html` + hashed assets) to serve.
    pub static_dir: PathBuf,
    /// The running build's bundle identifier, published for OS notifications
    /// (`host_server::set_notification_bundle_identifier`) because the host
    /// server cannot see the app's Tauri config.
    pub bundle_identifier: String,
}

/// Everything the control dispatcher needs that is *not* the app surface.
///
/// Each field is a row of the Step-0 inventory: the `app.try_state::<T>()`
/// lookups it replaces (D1–D4) and the Arc set the dispatcher was already
/// handed as parameters (`runtimes`, `metadata`, `owner_registry`, `git`,
/// `session_ui_profiles`, the ephemeral hub, the host server handle, the event
/// sink). Fields are public because the app package reads them to register its
/// own managed state (the window-lifecycle and exit paths still look these up
/// through the app handle — those readers are outside the dispatcher).
///
/// No `AdminServer` field yet — T3 adds it together with the admin channel.
pub struct DaemonState {
    /// Owner-scoped and native-broadcast events (`HostServer::event_sink`).
    pub host_events: HostEventSink,
    /// Live Pi runtimes.
    pub runtimes: NativePiManager,
    /// The host origin every op resolves against (`http://127.0.0.1:<port>`).
    pub host_origin: String,
    /// The static bundle directory the embedded Pi and package ops resolve from.
    pub static_dir: PathBuf,
    /// Registry rows, preferences, and the workspace→id map.
    pub metadata: SharedMetadataStore,
    /// Window-owner authority: labels, capabilities, workspace generations.
    pub owner_registry: Arc<WindowOwnerRegistry>,
    /// Git service (row D4: workspace-transition cleanup).
    pub git: Arc<GitService>,
    /// Per-session UI profile store.
    pub session_ui_profiles: Arc<SessionUiProfileStore>,
    /// Side/Quick/Config ephemeral runtimes.
    pub ephemeral_hub: SharedEphemeralHub,
    /// Skill-source handles (row D1).
    pub skill_sources: Arc<SkillSourceRegistry>,
    /// Per-process skill-install candidate-id secret (row D2).
    pub install_secret: Arc<HostInstallSecret>,
    /// Export-grant revocation on workspace transitions (row D3).
    pub host: Arc<HostServer>,
    /// The embedded-Pi installer resolver, when the bundled Pi is available
    /// (row A1). `Err` carries the reason every install op reports; the app's
    /// `RunEvent::Exit` keeps its own managed-state copy for teardown.
    pub environment_installer: Result<Arc<environment_install::Installer>, String>,
    /// Ephemeral runtime records (the app's window-destroyed teardown reads
    /// this through its app handle).
    pub ephemeral_registry: Arc<EphemeralRegistry>,
    /// Owner terminals (the app's window-destroyed teardown and its exit path
    /// kill them through the app handle).
    pub terminals: Arc<TerminalManager>,
    /// The officecli watch subprocess pool behind browser panes (the app's
    /// exit path stops it through the app handle).
    pub officecli_watches: Arc<OfficecliWatchRuntime>,
    /// The app-process surface (rows A2–A12). A headless daemon supplies the
    /// admin-channel forwarding realization (T3/T5).
    pub app_surface: Arc<dyn AppSurface>,
    /// The admin channel, once [`Self::install_admin`] ran (T4 starts it in the
    /// daemon binary; embedded mode starts it in the app process). `None` means
    /// no admin channel exists yet, which is why it is not a constructor
    /// argument: the admin server and the state reference each other.
    admin: Mutex<Option<AdminServer>>,
    /// The daemon's persisted bookkeeping: the instance file it publishes and
    /// the single-instance lock it holds (plan D7/T4). `None` in the app's
    /// embedded form — that process owns no instance file, and
    /// [`Self::drain_persisted_state`] then has nothing to remove.
    daemon_files: Mutex<Option<DaemonFiles>>,
    /// Set when [`Self::drain`] completed, so the daemon binary's main loop can
    /// exit on the `daemon.stop` path exactly as it does on a signal (T4).
    drained: AtomicBool,
    /// Paired with `drained` (`notify_one` keeps a permit, so a signal racing
    /// the wait cannot be lost).
    drained_notify: tokio::sync::Notify,
    /// Drain-step observer (test seam; see [`DrainObserver`]).
    drain_observer: Mutex<Option<Arc<dyn DrainObserver>>>,
    /// Guards the drain itself: a signal arriving while `daemon.stop` drains
    /// must not tear the runtimes down twice.
    draining: AtomicBool,
}

impl DaemonState {
    /// Build the whole host stack headless: metadata, runtimes, git, terminals,
    /// the host server (which binds its loopback listener here), the owner
    /// registry, the ephemeral hub and the installer resolver.
    ///
    /// This is `setup_native_runtime` minus the three things that are the
    /// app's by the Step-3 exception list: the browser panes (constructed
    /// app-side, registered afterwards through
    /// `host.set_browser_panes`), the landing owner + window, and the
    /// `app.manage` of app-only state. Nothing here touches the app surface —
    /// [`AppSurface`] is required by the control dispatcher, never by
    /// construction (pinned by `tests/daemon_boot.rs`).
    pub async fn start(
        cfg: DaemonConfig,
        app_surface: Arc<dyn AppSurface>,
    ) -> Result<Self, String> {
        let data_dir = cfg.data_dir.clone();
        let metadata_path = data_dir.join("picot.sqlite3");
        let shared_metadata = Arc::new(Mutex::new(MetadataStore::open(&metadata_path)?));
        // Q8 self-heal: when the PATH toggle is on, re-verify the managed
        // marker after upgrades or app moves and refresh a stale path.
        // Release-only and advisory: a failure logs and never blocks startup.
        {
            let metadata = shared_metadata.clone();
            let static_dir_for_heal = cfg.static_dir.clone();
            tokio::task::spawn_blocking(move || {
                let enabled = metadata
                    .lock()
                    .ok()
                    .and_then(|store| store.pref_get(crate::pi_path::PREF_KEY).ok().flatten())
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                if !enabled || cfg!(debug_assertions) {
                    return;
                }
                match crate::pi_path::bundled_pi_dir(&static_dir_for_heal) {
                    Ok(pi_dir) => {
                        #[cfg(target_os = "windows")]
                        let outcome = crate::pi_path::apply_windows(&pi_dir.to_string_lossy());
                        #[cfg(not(target_os = "windows"))]
                        let outcome = std::env::var("SHELL")
                            .ok()
                            .and_then(|shell| dirs::home_dir().map(|home| (home, shell)))
                            .map(|(home, shell)| {
                                crate::pi_path::apply_posix(
                                    &home,
                                    &shell,
                                    &pi_dir.to_string_lossy(),
                                )
                            })
                            .unwrap_or_else(|| Err("SHELL or home directory unavailable".into()));
                        if let Err(message) = outcome {
                            log::warn!("pi path self-heal skipped: {message}");
                        }
                    }
                    Err(message) => log::warn!("pi path self-heal skipped: {message}"),
                }
            });
        }
        // Cold start creates NO default workspace, NO session target, and NO Pi
        // runtime. The registry is untouched; the landing page is the first
        // view (the landing owner is the app's, see the Step-3 exception list).
        let runtimes = NativePiManager::new(256);
        let remote_auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&shared_metadata))));
        let ephemeral_registry = Arc::new(EphemeralRegistry::default());
        runtimes.set_ephemeral_registry(ephemeral_registry.clone());
        // Keep one native host-control authority. Host-origin v2 dispatch
        // invokes this same handler; it must not grow a parallel OS-control
        // implementation.
        let skill_sources = Arc::new(SkillSourceRegistry::new());
        let git = Arc::new(GitService::new());
        let terminals = Arc::new(TerminalManager::new(
            TerminalRegistry::new(15),
            TerminalStateStore::new(data_dir.join("terminal")),
        ));
        let session_ui_profiles = Arc::new(SessionUiProfileStore::open(
            data_dir.join("session-ui-profiles.json"),
        )?);
        let host = Arc::new(
            HostServer::start(
                cfg.static_dir.clone(),
                runtimes.clone(),
                remote_auth,
                Arc::clone(&shared_metadata),
            )
            .await?,
        );
        // OS notifications attribute to the running build's own bundle
        // identifier (picot / picot.dev / picot.internal): the host server
        // cannot see the Tauri config (or the daemon argv), so publish it here.
        host_server::set_notification_bundle_identifier(cfg.bundle_identifier.clone());
        // No host.runtime_started() here: that is the OAuth generation bump
        // tied to a runtime existing, and the landing has none.
        host.set_terminal_manager(terminals.clone());
        host.set_git_service(git.clone());
        let owner_registry = Arc::new(WindowOwnerRegistry::default());
        host.set_owner_registry(owner_registry.clone());
        // The officecli watch subprocess pool the browser panes render from
        // (the panes themselves are the app's; see the Step-3 exception list).
        let officecli_watches = Arc::new(OfficecliWatchRuntime::new());
        host.set_officecli_watches(officecli_watches.clone());
        let host_events = host.event_sink();
        terminals.set_event_sink({
            let host_events = host_events.clone();
            Arc::new(move |owner, event| {
                host_events.send_owner_event(owner, event);
            })
        });
        // Native ephemeral chats (Side/Quick): one hub owns render-state
        // reduction, command forwarding, and owner bootstrap delivery.
        let ephemeral_hub = Arc::new(EphemeralHub::new(
            ephemeral_registry.clone(),
            host_events.clone(),
            cfg.static_dir.clone(),
        ));
        ephemeral_hub.spawn_pump(&runtimes);
        host.set_ephemeral_hub(ephemeral_hub.clone());
        let environment_installer = crate::pi_launch::resolve_bundled_pi(&cfg.static_dir)
            .map(|binary| Arc::new(environment_install::Installer::new(binary)))
            .map_err(|error| format!("the embedded Pi is unavailable: {error}"));
        Ok(Self {
            host_events,
            runtimes,
            host_origin: host.origin().to_string(),
            static_dir: cfg.static_dir,
            metadata: Arc::clone(&shared_metadata),
            owner_registry,
            git,
            session_ui_profiles,
            ephemeral_hub,
            skill_sources,
            install_secret: Arc::new(HostInstallSecret::generate()),
            host,
            environment_installer,
            ephemeral_registry,
            terminals,
            officecli_watches,
            app_surface,
            admin: Mutex::new(None),
            daemon_files: Mutex::new(None),
            drained: AtomicBool::new(false),
            drained_notify: tokio::sync::Notify::new(),
            drain_observer: Mutex::new(None),
            draining: AtomicBool::new(false),
        })
    }

    /// Attach the admin channel: set the daemon back-reference, feed the owner
    /// registry's mirror events into it, and remember it for the drain order.
    ///
    /// Mirrors [`Self::install_control_handler`]: the admin server holds a weak
    /// back-reference to this state and this state holds the server, so neither
    /// needs the other at construction time.
    pub fn install_admin(self: &Arc<Self>, admin: AdminServer) {
        admin.attach_state(self);
        let mirror = admin.clone();
        self.owner_registry.set_mirror_sink(Arc::new(move |event| {
            mirror.push_owner_event(event);
        }));
        if let Ok(mut slot) = self.admin.lock() {
            *slot = Some(admin);
        }
    }

    /// The attached admin channel, when one exists.
    pub fn admin(&self) -> Option<AdminServer> {
        self.admin.lock().ok().and_then(|slot| slot.clone())
    }

    /// Install the daemon's persisted bookkeeping (plan D7/T4).
    ///
    /// Called only by the daemon binary: the app's embedded form has no
    /// instance file to publish and no single-instance lock to take. The store
    /// is also handed to the admin channel as its `LeaseObserver`, so the
    /// manager record is rewritten by the daemon alone (plan D3).
    pub fn set_daemon_files(&self, files: DaemonFiles) {
        let store = files.store.clone();
        if let Ok(mut slot) = self.daemon_files.lock() {
            *slot = Some(files);
        }
        if let Some(admin) = self.admin() {
            admin.set_lease_observer(Arc::new(store));
        }
    }

    /// Resolves once the drain completed (a signal or a `daemon.stop`).
    ///
    /// The daemon binary's main loop awaits this so both shutdown triggers
    /// leave through one exit path; `DAEMON_STOP` is "accepted" first and the
    /// process is confirmed gone by identity, never by this returning (D12).
    pub async fn drained(&self) {
        if self.drained.load(Ordering::SeqCst) {
            return;
        }
        self.drained_notify.notified().await;
    }

    /// Install a drain-step observer. Test seam: the production path uses the
    /// same sequence with no observer installed.
    pub fn set_drain_observer(&self, observer: Arc<dyn DrainObserver>) {
        if let Ok(mut slot) = self.drain_observer.lock() {
            *slot = Some(observer);
        }
    }

    /// Report one drain step (no-op without an observer).
    pub fn observe_drain(&self, step: DrainStep) {
        let observer = self
            .drain_observer
            .lock()
            .ok()
            .and_then(|slot| slot.clone());
        if let Some(observer) = observer {
            observer.step(step);
        }
    }

    /// Prepare a navigation permit **and** wait for the app's `mirror_ack`
    /// before the granting op completes (plan Task 3, R1).
    ///
    /// Fails safe: when no manager is attached, or the ack does not arrive
    /// within [`admin::MIRROR_ACK_TIMEOUT`], the permit is revoked so the
    /// WebView's synchronous navigation delegate can never hit a permit the
    /// app's mirror has not installed.
    pub async fn prepare_navigation_with_mirror_ack(
        &self,
        owner: &OwnerId,
        port: u16,
        canonical_cwd: PathBuf,
        origin: String,
        ttl: Duration,
    ) -> Result<u64, String> {
        let (generation, permit_id) = self.owner_registry.prepare_navigation_with_permit(
            owner,
            port,
            canonical_cwd,
            origin,
            ttl,
        )?;
        let Some(admin) = self.admin() else {
            self.owner_registry.cancel_pending_permit(owner);
            return Err(
                "no admin channel is attached: the navigation mirror cannot be updated".into(),
            );
        };
        match admin.await_mirror_ack(&permit_id).await {
            Ok(_epoch) => Ok(generation),
            Err(error) => {
                self.owner_registry.cancel_pending_permit(owner);
                Err(format!(
                    "the navigation mirror did not acknowledge permit {permit_id}: {error}"
                ))
            }
        }
    }

    /// Owner-scoped detach cleanup (plan D6/D11): capabilities are already
    /// revoked by the registry, and **no resident workspace runtime is
    /// stopped** — only owner-scoped process/state is released.
    ///
    /// T7 completes this path: temp-dir cleanup, close-approval drops and the
    /// ephemeral *process* stop (R5) arrive with `detach_owner`; the app-side
    /// approval map is unreachable from here by design (D4).
    pub fn detach_owner_scoped_cleanup(&self, owner: &OwnerId) {
        let _ = self.ephemeral_registry.owner_cleanup(owner);
        self.terminals.kill_owner(owner);
        self.git.clear_owner(owner.as_str());
        self.skill_sources.revoke_owner(owner);
    }

    /// Drain step "runtimes": stop what this process owns, in the order the
    /// app's `RunEvent::Exit` uses today.
    pub async fn drain_runtimes(&self) {
        if let Ok(installer) = self.environment_installer.as_ref() {
            installer.stop_for_app_exit();
        }
        self.runtimes.stop_for_app_exit();
        self.terminals.kill_all();
        self.officecli_watches.stop_all();
        crate::child_supervision::clear_registry();
    }

    /// Drain step "files": the daemon's persisted bookkeeping — the instance
    /// file and the single-instance lock (plan D7/T4).
    ///
    /// This is the single plug point the drain order reserves for files, so
    /// both shutdown triggers (a signal and `daemon.stop`) leave the same clean
    /// directory. It runs **after** the runtimes are down and the admin channel
    /// is closed: unlinking the lock file while this process still holds its
    /// descriptor is therefore the last thing before exit, which is what keeps
    /// the unlink race in [`instance::InstanceLock`] narrow.
    pub fn drain_persisted_state(&self) {
        let slot = self
            .daemon_files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(files) = slot.as_ref() else {
            return;
        };
        files.store.clear();
        files.lock.remove_file();
    }

    /// Hand the data plane its control handler.
    ///
    /// Deliberately *not* part of [`Self::start`]: the handler holds an
    /// `Arc<DaemonState>` and `DaemonState` holds the host, so installing it
    /// needs the state to be inside its `Arc` first. The app package does this
    /// immediately after `start` (embedded form); T4's daemon main does the
    /// same.
    pub fn install_control_handler(self: &Arc<Self>) {
        self.host
            .set_control_handler(crate::host_control_dispatch::host_control_dispatch(
                Arc::clone(self),
            ));
    }

    /// The host origin every op resolves against (`http://127.0.0.1:<port>`).
    pub fn origin(&self) -> &str {
        &self.host_origin
    }

    /// Stop everything the process owns, in the drain order the plan fixes:
    /// stop accepting admin frames → close the admin WS → drain runtimes →
    /// files → exit.
    ///
    /// T4 extends this with the instance file + lock. "Accepted" is not
    /// "completed" (plan D12): signalling the listener is a request; callers
    /// that need the process gone confirm by identity, not by this returning.
    pub async fn shutdown_graceful(&self, reason: &str) {
        log::info!("[picot-daemon] graceful shutdown: {reason}");
        self.drain().await;
    }

    /// The one drain sequence, shared by the signal/exit path
    /// ([`Self::shutdown_graceful`]) and the `daemon.stop` op: stop accepting
    /// admin frames → close the admin WS → drain runtimes → files → exit.
    ///
    /// Deliberately one implementation, not two: the order is a correctness
    /// property (an admin frame must not arrive while runtimes are being torn
    /// down) and a second copy would drift from it.
    pub async fn drain(&self) {
        if self.draining.swap(true, Ordering::SeqCst) {
            // A second trigger (a signal during `daemon.stop`, or the reverse)
            // must not stop the runtimes twice; the first drain is the shutdown.
            log::info!("[picot-daemon] drain already in progress; ignoring the repeat trigger");
            return;
        }
        self.observe_drain(DrainStep::StopAcceptingAdminFrames);
        if let Some(admin) = self.admin() {
            admin.stop_accepting();
        }
        self.observe_drain(DrainStep::CloseAdminWs);
        if let Some(admin) = self.admin() {
            admin.close_connections();
        }
        self.observe_drain(DrainStep::DrainRuntimes);
        self.drain_runtimes().await;
        self.observe_drain(DrainStep::DrainFiles);
        self.drain_persisted_state();
        self.observe_drain(DrainStep::Exit);
        if let Some(admin) = self.admin() {
            admin.clear_bookkeeping();
        }
        self.host.stop();
        self.drained.store(true, Ordering::SeqCst);
        self.drained_notify.notify_one();
    }
}
