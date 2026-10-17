// ABOUTME: The daemon-side app-surface realization: every forwarded inventory row travels over
// ABOUTME: the admin channel as an `app_surface_request` frame (plan Task 5, D5).

//! # App-surface forwarding (plan D5, Task 5)
//!
//! A headless daemon has no windows, no native dialogs and no updater, so the
//! [`AppSurface`] rows of the Task 2 Step-0 inventory are served by forwarding
//! them to the attached app: [`AdminForwardingSurface`] turns each trait method
//! into one `app_surface_request` frame, the app's pump executes it, and the
//! `surface_response` settles the call. `download_and_install_update` streams
//! `surface_progress` frames while it runs.
//!
//! The frame shapes and the op/argument names live in [`wire`], which both sides
//! of the channel import: the daemon encodes with it and the app's pump decodes
//! with it, so a renamed op cannot drift between the two ends. The op set is
//! pinned to the plan's protocol table by the literal fixture in
//! `tests/admin_surface.rs`.
//!
//! ## Failure mode
//!
//! With no manager attached, or when the app does not answer inside
//! [`crate::daemon::admin::AdminServer::request_app_surface`]'s idle deadline,
//! every row fails closed with the typed `app_surface_unavailable` error — the
//! same string a daemon with no app produces (plan D5).

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::daemon::admin::AdminServer;
use crate::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use crate::runtime_coordinator::RuntimeTarget;

/// The wire contract of the forwarded surface: frame fields, op names and
/// argument keys. Both ends import it (see the module doc).
pub mod wire {
    /// The typed failure every unavailable forwarded row reports (plan D5).
    pub const APP_SURFACE_UNAVAILABLE: &str = "app_surface_unavailable";

    /// The `app_surface_request` op names, in the plan's table order.
    pub const SURFACE_OPS: [&str; 12] = [
        "open_workspace_window",
        "open_devtools",
        "close_window",
        "cancel_window_close",
        "menu_state_changed",
        "relaunch_app",
        "pane",
        "pick_folder",
        "pick_image_files",
        "save_session_export",
        "check_for_update",
        "download_and_install_update",
    ];

    pub const OP_OPEN_WORKSPACE_WINDOW: &str = SURFACE_OPS[0];
    pub const OP_OPEN_DEVTOOLS: &str = SURFACE_OPS[1];
    pub const OP_CLOSE_WINDOW: &str = SURFACE_OPS[2];
    pub const OP_CANCEL_WINDOW_CLOSE: &str = SURFACE_OPS[3];
    pub const OP_MENU_STATE_CHANGED: &str = SURFACE_OPS[4];
    pub const OP_RELAUNCH_APP: &str = SURFACE_OPS[5];
    pub const OP_PANE: &str = SURFACE_OPS[6];
    pub const OP_PICK_FOLDER: &str = SURFACE_OPS[7];
    pub const OP_PICK_IMAGE_FILES: &str = SURFACE_OPS[8];
    pub const OP_SAVE_SESSION_EXPORT: &str = SURFACE_OPS[9];
    pub const OP_CHECK_FOR_UPDATE: &str = SURFACE_OPS[10];
    pub const OP_DOWNLOAD_AND_INSTALL_UPDATE: &str = SURFACE_OPS[11];

    pub const ARG_HOST_ORIGIN: &str = "hostOrigin";
    pub const ARG_TARGET: &str = "target";
    pub const ARG_OWNER: &str = "owner";
    pub const ARG_CAPABILITY: &str = "capability";
    pub const ARG_LABEL: &str = "label";
    pub const ARG_REQUEST_ID: &str = "requestId";
    pub const ARG_INITIAL_DIR: &str = "initialDir";
    pub const ARG_EXPORT_URL: &str = "exportUrl";
    pub const ARG_FILE_NAME: &str = "fileName";
    pub const ARG_OP: &str = "op";
    pub const ARG_ERROR: &str = "error";
}

/// The typed, fail-closed error for a forwarded row the app cannot serve.
pub fn surface_unavailable(op: &str, why: &str) -> String {
    format!(
        "{}: {op} needs the Picot app ({why})",
        wire::APP_SURFACE_UNAVAILABLE
    )
}

/// The daemon's [`AppSurface`]: forward to the manager attachment.
///
/// The admin channel both serves this surface and is started *after* the daemon
/// state it serves (`DaemonState::start` takes the surface, `install_admin`
/// installs the channel), so the channel is a late-bound slot: before
/// [`Self::attach`] every row fails closed rather than panicking.
pub struct AdminForwardingSurface {
    admin: Mutex<Option<AdminServer>>,
}

impl Default for AdminForwardingSurface {
    fn default() -> Self {
        Self::new()
    }
}

impl AdminForwardingSurface {
    pub fn new() -> Self {
        Self {
            admin: Mutex::new(None),
        }
    }

    /// Install the admin channel this surface forwards over.
    pub fn attach(&self, admin: AdminServer) {
        if let Ok(mut slot) = self.admin.lock() {
            *slot = Some(admin);
        }
    }

    fn admin(&self) -> Result<AdminServer, String> {
        self.admin
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .ok_or_else(|| {
                surface_unavailable("the app surface", "the admin channel is not attached yet")
            })
    }

    /// One forwarded op: no progress channel.
    async fn forward(&self, op: &str, args: Value) -> Result<Value, String> {
        self.forward_with_progress(op, args, None).await
    }

    /// One forwarded op, streaming `progress` frames back to the requester.
    async fn forward_with_progress(
        &self,
        op: &str,
        args: Value,
        progress: Option<SurfaceProgress>,
    ) -> Result<Value, String> {
        self.admin()?.request_app_surface(op, args, progress).await
    }

    /// `Ok(())` rows: the app's reply payload is `null`/`{}` and carries no
    /// information, so only the failure matters.
    async fn forward_void(&self, op: &str, args: Value) -> Result<(), String> {
        self.forward(op, args).await.map(|_| ())
    }
}

#[async_trait::async_trait]
impl AppSurface for AdminForwardingSurface {
    async fn open_workspace_window(
        &self,
        host_origin: &str,
        target: &RuntimeTarget,
        label: &str,
        owner: &str,
        capability: &str,
    ) -> Result<(), String> {
        self.forward_void(
            wire::OP_OPEN_WORKSPACE_WINDOW,
            json!({
                wire::ARG_HOST_ORIGIN: host_origin,
                wire::ARG_TARGET: target,
                wire::ARG_LABEL: label,
                wire::ARG_OWNER: owner,
                wire::ARG_CAPABILITY: capability,
            }),
        )
        .await
    }

    async fn open_devtools(&self, label: &str) -> Result<(), String> {
        self.forward_void(wire::OP_OPEN_DEVTOOLS, json!({ wire::ARG_LABEL: label }))
            .await
    }

    async fn close_window(&self, label: &str, request_id: &str) -> Result<(), String> {
        self.forward_void(
            wire::OP_CLOSE_WINDOW,
            json!({ wire::ARG_LABEL: label, wire::ARG_REQUEST_ID: request_id }),
        )
        .await
    }

    async fn cancel_window_close(&self, label: &str, request_id: &str) -> Result<(), String> {
        self.forward_void(
            wire::OP_CANCEL_WINDOW_CLOSE,
            json!({ wire::ARG_LABEL: label, wire::ARG_REQUEST_ID: request_id }),
        )
        .await
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        self.forward_void(wire::OP_MENU_STATE_CHANGED, json!({}))
            .await
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        self.forward_void(wire::OP_RELAUNCH_APP, json!({})).await
    }

    async fn pane(&self, op: PaneOp) -> Result<Value, String> {
        let encoded = serde_json::to_value(&op)
            .map_err(|error| format!("cannot encode the pane op: {error}"))?;
        self.forward(wire::OP_PANE, json!({ wire::ARG_OP: encoded }))
            .await
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        let reply = self.forward(wire::OP_PICK_FOLDER, json!({})).await?;
        Ok(reply.as_str().map(str::to_owned))
    }

    async fn pick_image_files(&self, initial_dir: Option<String>) -> Result<Value, String> {
        self.forward(
            wire::OP_PICK_IMAGE_FILES,
            json!({ wire::ARG_INITIAL_DIR: initial_dir }),
        )
        .await
    }

    async fn save_session_export(
        &self,
        host_origin: &str,
        export_url: &str,
        file_name: &str,
        initial_dir: Option<String>,
    ) -> Result<Value, String> {
        self.forward(
            wire::OP_SAVE_SESSION_EXPORT,
            json!({
                wire::ARG_HOST_ORIGIN: host_origin,
                wire::ARG_EXPORT_URL: export_url,
                wire::ARG_FILE_NAME: file_name,
                wire::ARG_INITIAL_DIR: initial_dir,
            }),
        )
        .await
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        self.forward(wire::OP_CHECK_FOR_UPDATE, json!({})).await
    }

    async fn download_and_install_update(
        &self,
        progress: SurfaceProgress,
    ) -> Result<Value, String> {
        self.forward_with_progress(
            wire::OP_DOWNLOAD_AND_INSTALL_UPDATE,
            json!({}),
            Some(progress),
        )
        .await
    }
}

/// The daemon's [`crate::host_server::PaneForwarder`]: the same forwarding
/// realization, one `AppSurface::pane` call per `browser_pane_*` op.
///
/// Review B1: before this existed the daemon's data plane answered every
/// `browser_pane_*` op with `unavailable`, because nothing ever installed the
/// host's in-process pane runtime — the app owns the child webviews, so the op
/// has to travel. It travels as the same `app_surface_request { op: "pane" }`
/// frame every other forwarded row uses; a host with no manager attached fails
/// closed with the typed `app_surface_unavailable` error.
pub struct AppSurfacePaneForwarder {
    surface: Arc<dyn AppSurface>,
}

impl AppSurfacePaneForwarder {
    pub fn new(surface: Arc<dyn AppSurface>) -> Self {
        Self { surface }
    }
}

#[async_trait::async_trait]
impl crate::host_server::PaneForwarder for AppSurfacePaneForwarder {
    async fn forward_pane(&self, op: PaneOp) -> Result<Value, String> {
        self.surface.pane(op).await
    }
}

/// The forwarding surface as the daemon's shared surface, late-bound to its
/// admin channel. Both the daemon binary and the app's embedded form boot this
/// way (plan D2: one protocol for both modes).
pub type SharedForwardingSurface = Arc<AdminForwardingSurface>;
