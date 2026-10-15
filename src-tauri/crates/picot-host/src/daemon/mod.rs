// ABOUTME: Daemon-side state ownership: DaemonState, the app-surface seam, and (T3+) admin/instance.
// ABOUTME: Step 2 of plan Task 2 lands the state object and the dispatcher extraction; `DaemonState::start` follows in Step 3.

//! Phase 1 (2026-10-15) moves the whole host stack out of the Tauri main
//! process (plan D1/D2). This module owns what `setup_native_runtime`
//! (main.rs:2049–2264) built in the app process and what the control
//! dispatcher needs to reach.
//!
//! Landed so far:
//!
//! - [`app_surface`] — the exhaustive `AppHandle`/`try_state` inventory of the
//!   control dispatcher and the object-safe [`app_surface::AppSurface`] seam
//!   every row maps onto (Task 2 Step 0).
//! - [`DaemonState`] — the explicit Arc set the dispatcher reads, replacing
//!   every `app.try_state::<T>()` lookup (Task 2 Step 2, inventory rows
//!   D1–D4). The *construction* still lives in the app package for this step:
//!   the app builds `DaemonState` in-process and passes a `TauriAppSurface`
//!   directly (embedded form; the admin channel arrives in T3).
//!
//! Not landed yet (tracked by the plan, not placeholders):
//!
//! - `DaemonConfig` + `DaemonState::start` + `shutdown_graceful` (Task 2
//!   Steps 3–4): the construction move, the headless boot path, and the
//!   instance file that the Step-1 red test (`tests/daemon_boot.rs`) pins.
//! - `admin` (Task 3) and `instance` (Task 4).

pub mod app_surface;

use crate::daemon::app_surface::AppSurface;
use crate::environment_install;
use crate::git_service::GitService;
use crate::host_control::HostEventSink;
use crate::host_ephemeral::SharedEphemeralHub;
use crate::host_server::HostServer;
use crate::metadata_store::SharedMetadataStore;
use crate::native_pi_manager::NativePiManager;
use crate::session_ui_profile_store::SessionUiProfileStore;
use crate::skill_source_registry::SkillSourceRegistry;
use crate::window_owner::WindowOwnerRegistry;
use std::path::PathBuf;
use std::sync::Arc;

/// The app package no longer holds the per-process skill-install secret: its
/// only readers were dispatcher rows D2, which now read this field.
pub use crate::host_control_dispatch::HostInstallSecret;

/// Everything the control dispatcher needs that is *not* the app surface.
///
/// Each field is a row of the Step-0 inventory: the `app.try_state::<T>()`
/// lookups it replaces (D1–D4) and the Arc set the dispatcher was already
/// handed as parameters (`runtimes`, `metadata`, `owner_registry`, `git`,
/// `session_ui_profiles`, the ephemeral hub, the host server handle, the event
/// sink). Fields are public on purpose: the app package constructs the object
/// until Step 3 moves that construction into `DaemonState::start`.
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
    /// The app-process surface (rows A2–A12). A headless daemon supplies the
    /// admin-channel forwarding realization (T3/T5).
    pub app_surface: Arc<dyn AppSurface>,
}
