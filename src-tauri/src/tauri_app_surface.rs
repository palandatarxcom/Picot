// ABOUTME: The app package's AppSurface: real AppHandle + owner registry behind the seam.
// ABOUTME: Task 2 Step 2 of the Phase 1 plan — the app-side half of the dispatcher split.

//! The app-side realization of [`AppSurface`]: every inventory row that needs
//! the app process (windows, native dialogs, updater, devtools, relaunch,
//! menus, child webviews, the pending-close map) is implemented here against
//! the real [`AppHandle`].
//!
//! The daemon forwards these ops to the app over `app_surface_request` frames
//! (T5); the app executes them on this surface and answers over the same admin
//! channel. Nothing here is new behavior: each method calls the same app helper
//! the dispatcher used to call inline, with the same arguments and the same
//! results.

use crate::daemon_client::navigation_mirror::NavigationMirror;
use crate::{
    check_for_update_core, close_approvals, download_and_install_update_core,
    open_native_workspace_window, pick_folder_core, pick_image_files_core,
    save_session_export_core, update_new_session_menu_state,
};
use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::window_owner::OwnerId;
use serde_json::Value;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub struct TauriAppSurface {
    app: AppHandle,
    /// The app-side navigation authority the daemon feeds (Task 5). Windows are
    /// opened against it, and the pending-close map is keyed by the owner it
    /// yields — the same answers in both modes, with no in-process registry.
    mirror: Arc<NavigationMirror>,
}

impl TauriAppSurface {
    /// The app handle plus the mirror: the mirror is created before the daemon
    /// boots (the daemon's surface forwards through this one), so it is passed
    /// in rather than looked up through the app handle.
    pub fn new(app: AppHandle, mirror: Arc<NavigationMirror>) -> Self {
        Self { app, mirror }
    }

    /// The owner that owns `label`, per the daemon-fed label↔owner map.
    fn owner_for_label(&self, label: &str) -> Result<OwnerId, String> {
        self.mirror
            .owner_for_label(label)
            .map(OwnerId::from_string)
            .ok_or_else(|| "no pending window close".to_string())
    }
}

#[async_trait::async_trait]
impl AppSurface for TauriAppSurface {
    /// Row A2.
    async fn open_workspace_window(
        &self,
        host_origin: &str,
        target: &RuntimeTarget,
        label: &str,
        owner: &str,
        capability: &str,
    ) -> Result<(), String> {
        open_native_workspace_window(
            &self.app,
            host_origin,
            target,
            label,
            Arc::clone(&self.mirror),
            OwnerId::from_string(owner.to_string()),
            capability,
        )
    }

    /// Row A6. The label lookup is the app package's business: the daemon only
    /// knows the owner, so "no window found" is the same error it produced
    /// inline before the split.
    async fn open_devtools(&self, label: &str) -> Result<(), String> {
        let window = self
            .app
            .get_webview_window(label)
            .ok_or_else(|| format!("No window found for {label}"))?;
        window.open_devtools();
        Ok(())
    }

    /// Multi-window §6.3. Same label lookup as `open_devtools`: the daemon
    /// resolved the holder's label, the app owns the window. A missing window
    /// is exactly the failure the §6.3 caller re-checks the holder against.
    async fn focus_window(&self, label: &str) -> Result<(), String> {
        let window = self
            .app
            .get_webview_window(label)
            .ok_or_else(|| format!("No window found for {label}"))?;
        window.set_focus().map_err(|error| error.to_string())
    }

    /// Multi-window §7 — the single decision point. The daemon only carried
    /// the click over; the ledger, the live routes and the focus stamps the
    /// decision needs are all here, so the snapshot, the pick and the
    /// execution happen in the app (see `notification_executor`).
    async fn notification_activated(&self, payload: Value) -> Result<(), String> {
        crate::daemon_client::notification_executor::route_click(&self.app, &payload).await
    }

    /// Row A10 — settle the approval, then close. The pending map (D4) is
    /// keyed by owner: the label the daemon passes is resolved app-side, where
    /// the registry lives, exactly as the app's own close delegate does. The
    /// validation results are the ones `window_close_approve` already returned.
    async fn close_window(&self, label: &str, request_id: &str) -> Result<(), String> {
        let owner = self.owner_for_label(label)?;
        {
            let mut guard = close_approvals().lock().unwrap();
            let Some(pending) = guard.get_mut(&owner) else {
                return Err("no pending window close".to_string());
            };
            if pending.request_id != request_id {
                return Err("request id mismatch".to_string());
            }
            pending.approved = true;
        }
        if let Some(win) = self.app.get_webview_window(label) {
            let _ = win.close();
        }
        Ok(())
    }

    /// Row A11 — drop the app's pending close for this request. Matched by
    /// request id (the map is owner-keyed and the id is unique per request);
    /// the daemon's cancel only ever clears what the close delegate created.
    async fn cancel_window_close(&self, _label: &str, request_id: &str) -> Result<(), String> {
        close_approvals()
            .lock()
            .unwrap()
            .retain(|_, pending| pending.request_id != request_id);
        Ok(())
    }

    /// Row A9 — best-effort menu recompute; a failure never fails the caller.
    async fn menu_state_changed(&self) -> Result<(), String> {
        update_new_session_menu_state(&self.app);
        Ok(())
    }

    /// Row A12.
    async fn relaunch_app(&self) -> Result<(), String> {
        // Never returns: the framework re-executes the process.
        self.app.restart()
    }

    /// Row A13 — child webviews. The runtime is the app package's
    /// `browser_pane::BrowserPaneRuntime`; without it (no window, tests) the op
    /// is unavailable, which is the answer the data plane already gives.
    async fn pane(&self, op: PaneOp) -> Result<Value, String> {
        let panes = self
            .app
            .try_state::<crate::browser_pane::BrowserPaneState>()
            .map(|state| state.inner().0.clone())
            .ok_or_else(|| "browser panes are not available".to_string())?;
        match op {
            PaneOp::Create {
                pane_id,
                window_label,
                url,
                rect,
            } => {
                panes.create(&pane_id, &window_label, &url, rect)?;
                Ok(Value::Null)
            }
            PaneOp::SetRect { pane_id, rect } => {
                panes.set_rect(&pane_id, rect)?;
                Ok(Value::Null)
            }
            PaneOp::SetVisible { pane_id, visible } => {
                panes.set_visible(&pane_id, visible)?;
                Ok(Value::Null)
            }
            PaneOp::EvalJson {
                pane_id,
                expression,
            } => Ok(Value::String(panes.eval_json(&pane_id, &expression).await?)),
            PaneOp::Navigate { pane_id, url } => {
                panes.navigate(&pane_id, &url)?;
                Ok(Value::Null)
            }
            PaneOp::Url { pane_id } => Ok(Value::String(panes.url(&pane_id)?)),
            PaneOp::Destroy { pane_id } => {
                panes.destroy(&pane_id)?;
                Ok(Value::Null)
            }
        }
    }

    /// Row A3.
    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Ok(pick_folder_core(&self.app).await)
    }

    /// Row A4.
    async fn pick_image_files(&self, initial_dir: Option<String>) -> Result<Value, String> {
        match pick_image_files_core(&self.app, initial_dir).await? {
            Some(files) => Ok(serde_json::to_value(files).unwrap_or(Value::Null)),
            None => Ok(Value::Null),
        }
    }

    /// Row A5.
    async fn save_session_export(
        &self,
        host_origin: &str,
        export_url: &str,
        file_name: &str,
        initial_dir: Option<String>,
    ) -> Result<Value, String> {
        match save_session_export_core(&self.app, host_origin, export_url, file_name, initial_dir)
            .await?
        {
            Some(path) => Ok(Value::from(path)),
            None => Ok(Value::Null),
        }
    }

    /// Row A7.
    async fn check_for_update(&self) -> Result<Value, String> {
        check_for_update_core(&self.app).await
    }

    /// Row A8.
    async fn download_and_install_update(
        &self,
        progress: SurfaceProgress,
    ) -> Result<Value, String> {
        download_and_install_update_core(&self.app, progress).await
    }
}
