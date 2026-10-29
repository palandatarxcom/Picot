#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// ABOUTME: Picot Tauri host entry point: spawns per-workspace Pi processes and
// ABOUTME: owns windows, HostServer v2, ephemeral chats, and native close lifecycle.

// The host stack lives in the shared `picot-host` library, which links no
// Tauri (plan D1; enforced by scripts/check-daemon-no-tauri.js). Only the
// app-side child-webview surface stays a module of this binary.
mod browser_pane;
// The app-side half of the dispatcher split (Task 2 Step 2): the real
// `AppHandle` behind `picot_host::daemon::app_surface::AppSurface`.
mod tauri_app_surface;
// The app-side daemon client: probe/spawn/adopt (T4), the admin client and the
// navigation mirror it feeds, the surface pump, and the blocked-instance policy.
// Wired into startup by T5; T10 exposes it through the admin commands.
mod daemon_client;
#[cfg(target_os = "macos")]
mod host_client;

use daemon_client::admin_client::{AdminClient, SurfaceExecutor};
use daemon_client::commands::{
    daemon_agent_stop, daemon_restart, daemon_set_keep_running, daemon_status, relay_offer,
    relay_set_enabled, relay_status,
};
use daemon_client::exit_waiter::{ExitWaiter, ProcessIdentity};
use daemon_client::manager::{DaemonManager, EnsureOutcome, ManagerConfig};
use daemon_client::navigation_mirror::NavigationMirror;
use daemon_client::notification_routing::FocusTimes;
use daemon_client::quit_policy::{self, QuitOutcome};
use daemon_client::restart::{
    CapturedView, ManagerSpawner, OwnerBinding, RestartConfig, RestartCoordinator, RestartLink,
    RestartReport, RestartTarget, ViewMigration,
};
use daemon_client::startup_failure::StartupFailure;
use daemon_client::surface_ops::ForwardedSurfaceOps;
use daemon_client::window_ledger::{route_for_url, LedgerRow, WindowLedger};
#[cfg(target_os = "macos")]
use host_client::commands::{
    host_add, host_confirm_tofu, host_list, host_remove, host_status, relay_device_list,
    relay_revoke_device,
};
use picot_host::daemon::admin::ManagerIdentity;
use picot_host::{child_supervision, window_owner};

use picot_host::host_control::ProgressSink;
use picot_host::runtime_coordinator::RuntimeTarget;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, TitleBarStyle, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

// (R5) The `HostServerState` alias is gone with the window-close hand-off: the
// WebView talks to the daemon's own host, and only the daemon can push into it,
// so this process holds no host stack to reach for. The app-surface ops that
// used to reach one now travel through the admin channel.
// (T8) The managed `NativePiManager` alias is gone: the signal and exit paths no
// longer reach for an in-app manager copy, they run `DaemonLink`'s quit flow.

// ─── Tauri Commands ───────────────────────────────────────────────────────────

/// Native folder picker dialog
/// Rebuilds the local URL for a host-granted session export. Only the one-shot
/// token path that `session_export` returns is accepted, so the WebView cannot
/// point the host at another origin or route.
fn session_export_fetch_url(host_origin: &str, export_url: &str) -> Result<String, String> {
    const PREFIX: &str = "/v2/session-export/";
    let token = export_url
        .strip_prefix(PREFIX)
        .filter(|token| token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| "Invalid session export URL".to_string())?;
    Ok(format!("{host_origin}{PREFIX}{token}"))
}

/// Fetches the granted export inside the host process, then shows the native
/// save dialog. Bytes never cross the WebSocket. Returns `Ok(None)` on cancel.
async fn save_session_export_core(
    app: &AppHandle,
    host_origin: &str,
    export_url: &str,
    file_name: &str,
    initial_dir: Option<String>,
) -> Result<Option<String>, String> {
    let fetch_url = session_export_fetch_url(host_origin, export_url)?;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|error| format!("Cannot create export client: {error}"))?
        .get(&fetch_url)
        .send()
        .await
        .map_err(|error| format!("Session export fetch failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Session export fetch failed: HTTP {}",
            response.status()
        ));
    }
    let contents = response
        .bytes()
        .await
        .map_err(|error| format!("Session export read failed: {error}"))?;

    let mut dialog = app
        .dialog()
        .file()
        .add_filter("HTML", &["html"])
        .set_file_name(file_name);

    if let Some(dir) = initial_dir.as_deref() {
        let p = std::path::Path::new(dir);
        if p.is_dir() {
            dialog = dialog.set_directory(p);
        }
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog.save_file(move |path| {
        let _ = tx.send(path);
    });
    let Some(file_path) = rx.await.ok().flatten() else {
        return Ok(None);
    };

    let path = match file_path {
        tauri_plugin_fs::FilePath::Path(pb) => pb,
        tauri_plugin_fs::FilePath::Url(url) => url
            .to_file_path()
            .map_err(|_| "Only local paths can be saved to".to_string())?,
    };
    std::fs::write(&path, &contents)
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

async fn pick_folder_core(app: &AppHandle) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |path| {
        let result = path.map(|p| match p {
            tauri_plugin_fs::FilePath::Path(pb) => pb.to_string_lossy().into_owned(),
            tauri_plugin_fs::FilePath::Url(url) => url.to_string(),
        });
        let _ = tx.send(result);
    });
    rx.await.ok().flatten()
}

/// Maximum per-image file size accepted by the native picker (20 MB raw).
/// Images are read fully into memory and base64-encoded before being sent
/// over the control channel, so a cap prevents memory spikes on large photos.
const MAX_IMAGE_FILE_SIZE: u64 = 20 * 1024 * 1024;

/// Maximum number of images selectable in a single picker invocation.
const MAX_IMAGE_COUNT: usize = 10;

/// One selected image from the native multi-file picker. `data` is raw base64
/// of the file bytes (no `data:` prefix); the frontend image pipeline wraps it.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PickedImageFile {
    name: String,
    mime_type: String,
    data: String,
}

/// Map a file path's extension to a supported image MIME type. Case-insensitive.
/// Returns `None` for anything that is not a recognized image type.
fn image_mime_from_path(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Native multi-file image picker. Opens a dialog filtered to image types,
/// optionally starting at `initial_dir`. Returns `Ok(None)` when the user
/// cancels. Each selected file is read and base64-encoded; unsupported or
/// unreadable selections are surfaced as an error rather than silently dropped.
/// Enforces a per-file size limit (`MAX_IMAGE_FILE_SIZE`) and a count limit
/// (`MAX_IMAGE_COUNT`) before reading any bytes.
async fn pick_image_files_core(
    app: &AppHandle,
    initial_dir: Option<String>,
) -> Result<Option<Vec<PickedImageFile>>, String> {
    use base64::Engine;

    let mut dialog = app
        .dialog()
        .file()
        .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"]);

    if let Some(dir) = initial_dir.as_deref() {
        let p = std::path::Path::new(dir);
        if p.is_dir() {
            dialog = dialog.set_directory(p);
        }
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog.pick_files(move |paths| {
        let _ = tx.send(paths);
    });
    let picked = rx.await.ok().flatten();

    let Some(paths) = picked else {
        return Ok(None);
    };

    if paths.len() > MAX_IMAGE_COUNT {
        return Err(format!(
            "Too many images selected: {}; maximum is {}",
            paths.len(),
            MAX_IMAGE_COUNT
        ));
    }

    let mut files = Vec::with_capacity(paths.len());
    for fp in paths {
        let pb = match fp {
            tauri_plugin_fs::FilePath::Path(pb) => pb,
            tauri_plugin_fs::FilePath::Url(url) => url
                .to_file_path()
                .map_err(|_| "Only local image files can be attached".to_string())?,
        };

        let mime = image_mime_from_path(&pb)
            .ok_or_else(|| format!("Unsupported image type: {}", pb.display()))?;

        let metadata = std::fs::metadata(&pb)
            .map_err(|e| format!("Failed to stat image {}: {}", pb.display(), e))?;
        if metadata.len() > MAX_IMAGE_FILE_SIZE {
            return Err(format!(
                "Image is too large: {} is {} MB; maximum is {} MB",
                pb.display(),
                metadata.len() / (1024 * 1024),
                MAX_IMAGE_FILE_SIZE / (1024 * 1024)
            ));
        }

        let bytes = std::fs::read(&pb)
            .map_err(|e| format!("Failed to read image {}: {}", pb.display(), e))?;

        let name = pb
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image")
            .to_string();

        files.push(PickedImageFile {
            name,
            mime_type: mime.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
    }

    Ok(Some(files))
}

/// What the window-state plugin remembers across launches: the inner size, the
/// outer position, and whether the window was maximized/fullscreen.
///
/// Position is safe to restore because the plugin applies it only when the saved
/// rectangle still intersects an attached monitor — a window last closed on a
/// display that is gone keeps the OS placement instead of opening off-screen.
/// While maximized/minimized the plugin skips both SIZE and POSITION, so the
/// saved state keeps the pre-maximize geometry and a restored maximized window
/// has a sane size and position to fall back to.
fn window_state_flags() -> StateFlags {
    StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED | StateFlags::FULLSCREEN
}

/// Clamp a restored physical window size to the monitor's usable work area.
fn clamp_window_size(width: u32, height: u32, work_width: u32, work_height: u32) -> (u32, u32) {
    (width.min(work_width.max(1)), height.min(work_height.max(1)))
}

fn clamp_window_geometry(
    position: tauri::PhysicalPosition<i32>,
    size: tauri::PhysicalSize<u32>,
    area: tauri::PhysicalRect<i32, u32>,
) -> (i32, i32, u32, u32) {
    let (width, height) =
        clamp_window_size(size.width, size.height, area.size.width, area.size.height);
    let max_x = i64::from(area.position.x) + i64::from(area.size.width - width);
    let max_y = i64::from(area.position.y) + i64::from(area.size.height - height);
    (
        i64::from(position.x).clamp(i64::from(area.position.x), max_x) as i32,
        i64::from(position.y).clamp(i64::from(area.position.y), max_y) as i32,
        width,
        height,
    )
}

fn open_native_window(
    app: &AppHandle,
    label: &str,
    url: &str,
    mirror: Arc<NavigationMirror>,
    owner: window_owner::OwnerId,
    capability: &str,
) -> Result<(), String> {
    let icon = Image::from_bytes(include_bytes!("../icons/32x32.png"))
        .map_err(|error| format!("Failed to load window icon: {error}"))?;
    let init_script = window_owner::capability_initialization_script(capability);
    // The synchronous delegate reads the daemon-fed mirror (plan D4/Task 5 R1):
    // fail-closed, with the opaque `about:` exceptions as the only survivors of
    // a dropped admin channel.
    let nav_owner = owner.as_str().to_string();
    let parsed_url: tauri::Url = url
        .parse()
        .map_err(|error| format!("Invalid native Host URL: {error}"))?;
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::External(parsed_url))
        .title("Picot")
        .inner_size(1300.0, 860.0)
        .min_inner_size(800.0, 600.0)
        .icon(icon)
        .map_err(|error| error.to_string())?
        .initialization_script(init_script)
        .on_navigation(move |url| mirror.authorize(&nav_owner, url))
        .on_new_window(|_url, _features| tauri::webview::NewWindowResponse::Deny)
        // Built hidden so the saved geometry is applied before the first
        // paint: restoring a visible window shows a resize jump.
        .visible(false);

    #[cfg(target_os = "macos")]
    let builder = builder
        .decorations(true)
        .title_bar_style(TitleBarStyle::Overlay)
        .hidden_title(true);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.decorations(true);
    let window = builder.build().map_err(|error| error.to_string())?;
    // `window-state` queued its restore from the window-created hook during
    // build(). Queue geometry validation after that restore and keep the window
    // hidden until both have completed, avoiding a visible resize/repaint flash.
    let window_for_ready = window.clone();
    window
        .run_on_main_thread(move || {
            let window = window_for_ready;
            if !window.is_maximized().unwrap_or(false) && !window.is_fullscreen().unwrap_or(false) {
                if let (Ok(size), Ok(monitors), Ok(position)) = (
                    window.inner_size(),
                    window.available_monitors(),
                    window.outer_position(),
                ) {
                    let window_right = i64::from(position.x) + i64::from(size.width);
                    let window_bottom = i64::from(position.y) + i64::from(size.height);
                    let monitor = monitors
                        .iter()
                        .filter_map(|monitor| {
                            let area = monitor.work_area();
                            let left = i64::from(area.position.x);
                            let top = i64::from(area.position.y);
                            let right = left + i64::from(area.size.width);
                            let bottom = top + i64::from(area.size.height);
                            let overlap_width =
                                window_right.min(right) - i64::from(position.x).max(left);
                            let overlap_height =
                                window_bottom.min(bottom) - i64::from(position.y).max(top);
                            // `then` (lazy), not `then_some` (eager): a window
                            // off every monitor yields negative overlaps, and
                            // eagerly casting those to u64 wraps to huge values
                            // whose product overflows and panics in debug.
                            (overlap_width > 0 && overlap_height > 0).then(|| {
                                (monitor, (overlap_width as u64) * (overlap_height as u64))
                            })
                        })
                        .max_by_key(|(_, overlap)| *overlap)
                        .map(|(monitor, _)| monitor)
                        .or_else(|| {
                            monitors.iter().max_by_key(|monitor| {
                                u64::from(monitor.work_area().size.width)
                                    * u64::from(monitor.work_area().size.height)
                            })
                        });
                    if let Some(monitor) = monitor {
                        let (x, y, width, height) =
                            clamp_window_geometry(position, size, *monitor.work_area());
                        if width != size.width || height != size.height {
                            let _ = window.set_size(tauri::PhysicalSize::new(width, height));
                        }
                        if x != position.x || y != position.y {
                            let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
                        }
                    }
                }
            }
            let _ = window.show();
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// A landing window: host-origin `/`. The label comes from the caller — the
/// cold start and ⌘N both allocate through `next_landing_label`, so the first
/// window stays `native-landing` and later ones get suffixed names. Tauri
/// labels are immutable; the owner record carries the workspace state.
fn open_native_landing_window(
    app: &AppHandle,
    host_origin: &str,
    label: &str,
    mirror: Arc<NavigationMirror>,
    owner: window_owner::OwnerId,
    capability: &str,
) -> Result<(), String> {
    open_native_window(
        app,
        label,
        &format!("{host_origin}/"),
        mirror,
        owner,
        capability,
    )
}

/// ⌘N / File → New Window (multi-window §5): open one more landing window as
/// a fresh client of the daemon. Purely Rust-side — no WebView is involved,
/// so the shortcut needs no focused window and no owner binding.
///
/// Mint-first (T1 discipline): allocate the label, mint the owner over the
/// admin channel, then open. The daemon's registry is the atomic conflict
/// point (a label taken by a mint in flight rejects ours), so a collision
/// retries the next suffix instead of pre-checking. An open failure revokes
/// the just-minted owner; the label frees itself because no window exists.
fn open_new_landing_window(app: &AppHandle) -> Result<(), String> {
    let link = app
        .try_state::<DaemonLink>()
        .ok_or_else(|| "Picot's daemon link is not available".to_string())?;
    let mirror = DaemonLink::mirror_for(app)
        .ok_or_else(|| "Picot's navigation mirror is not available".to_string())?;
    let host_origin = link.host_origin().to_string();
    let home = canonical_home_dir()?;
    let client = link.client();

    // Start at the first slot no live window holds, then let the daemon's
    // registry arbitrate: a slot held by a mint still in flight (another ⌘N
    // racing us, window not yet built) rejects ours and the index advances —
    // the §6.1 collision policy, retry the next suffix.
    let mut index = next_landing_index(app);
    let landing = loop {
        match tauri::async_runtime::block_on(client.create_owner(
            &landing_label_at(index),
            Some(&home.to_string_lossy()),
            0,
            &host_origin,
            None,
        )) {
            Ok(created) => break created,
            Err(error) if error.contains("window label is already owned") => index += 1,
            Err(error) => return Err(error),
        }
    };
    let label = landing_label_at(index);

    let owner = window_owner::OwnerId::from_string(landing.owner.clone());
    if let Err(error) = open_native_landing_window(
        app,
        &host_origin,
        &label,
        mirror,
        owner,
        &landing.capability,
    ) {
        let failed_owner = landing.owner.clone();
        if let Err(detach_error) = tauri::async_runtime::block_on(
            client.request("owner.detach", serde_json::json!({ "owner": failed_owner })),
        ) {
            log::warn!(
                "[picot] cannot revoke the owner of the failed landing window {label}: {detach_error}"
            );
        }
        return Err(error);
    }
    if let Some(ledger) = app.try_state::<WindowLedger>() {
        ledger.record(LedgerRow::landing(
            &label,
            &landing.owner,
            &home.to_string_lossy(),
        ));
    }
    Ok(())
}

fn open_native_workspace_window(
    app: &AppHandle,
    host_origin: &str,
    target: &RuntimeTarget,
    label: &str,
    mirror: Arc<NavigationMirror>,
    owner: window_owner::OwnerId,
    capability: &str,
) -> Result<(), String> {
    // Multi-window §6.1 (MW-2): `label` is daemon-allocated and reserved at
    // the owner mint — this factory threads it through verbatim (window,
    // ledger, mirror) and never recomputes it.
    let route = format!(
        "/workspaces/{}/sessions/{}",
        target.workspace_id, target.session_id
    );
    // The ledger remembers what the app knows about this window (T9/T10): the
    // workspace binding and the route. The owner's canonical root is
    // deliberately **not** here — the daemon minted that owner and stays the
    // authority for it (the T9 `owner.create` amendment).
    if let Some(ledger) = app.try_state::<WindowLedger>() {
        ledger.record(LedgerRow::workspace(
            label,
            owner.as_str(),
            &target.workspace_id,
            Some(route.clone()),
        ));
    }
    open_native_window(
        app,
        label,
        &format!("{host_origin}{route}"),
        mirror,
        owner,
        capability,
    )
}

fn canonical_if_exists(dir: PathBuf) -> Option<PathBuf> {
    if dir.join("index.html").exists() {
        Some(fs::canonicalize(&dir).unwrap_or(dir))
    } else {
        None
    }
}

fn resolve_static_dir(
    resource_dir: Option<PathBuf>,
    workspace_public: PathBuf,
    current_dir: Option<PathBuf>,
    debug_assertions: bool,
) -> PathBuf {
    let bundled_public = resource_dir.as_ref().map(|dir| dir.join("public"));
    let current_public = current_dir.unwrap_or_default().join("public");

    if debug_assertions {
        if let Some(dir) = canonical_if_exists(workspace_public) {
            return dir;
        }
        if let Some(dir) = canonical_if_exists(current_public.clone()) {
            return dir;
        }
        return current_public;
    }

    if let Some(dir) = bundled_public.and_then(canonical_if_exists) {
        return dir;
    }

    resource_dir
        .map(|dir| dir.join("public"))
        .unwrap_or_else(|| PathBuf::from("public"))
}

fn find_static_dir(app: &tauri::App) -> PathBuf {
    resolve_static_dir(
        app.path().resource_dir().ok(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("public"),
        std::env::current_dir().ok(),
        cfg!(debug_assertions),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_export_fetch_url_rebuilds_local_url_from_token_only() {
        let token = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            session_export_fetch_url(
                "http://127.0.0.1:47821",
                &format!("/v2/session-export/{token}")
            ),
            Ok(format!("http://127.0.0.1:47821/v2/session-export/{token}"))
        );
    }

    #[test]
    fn session_export_fetch_url_rejects_anything_but_an_export_token_path() {
        let origin = "http://127.0.0.1:47821";
        for export_url in [
            "http://evil.example/v2/session-export/0123456789abcdef0123456789abcdef",
            "//evil.example/v2/session-export/0123456789abcdef0123456789abcdef",
            "/v2/session-export/0123456789abcdef0123456789abcde",
            "/v2/session-export/0123456789abcdef0123456789abcdeg",
            "/v2/session-export/0123456789abcdef0123456789abcdef/extra",
            "/v2/session-export/../../etc/passwd",
            "/v2/files/raw?path=secret",
            "",
        ] {
            assert!(
                session_export_fetch_url(origin, export_url).is_err(),
                "accepted {export_url:?}"
            );
        }
    }

    use super::{
        clamp_window_geometry, close_request_needs_native_fallback, image_mime_from_path,
        new_session_menu_enabled, next_landing_label, resolve_static_dir, session_export_fetch_url,
        side_chat_startup_rpc_commands,
    };
    // The host-side helpers moved into `picot-host` with the dispatcher
    // (Task 2 Step 2); their tests stayed here.
    use picot_host::host_control_dispatch::{
        cancel_should_stop_target, filter_session_delete_paths, persisted_session_id_for_workspace,
        plan_prepare_reuse, quick_chat_snapshot, runtime_instance_summaries, runtime_started_event,
        should_stop_owner_runtimes_on_transition, skill_scope_context,
        target_for_owner_in_workspace, touch_registered_workspace, workspace_snapshot_for,
        PrepareReuse,
    };
    use picot_host::metadata_store::{MetadataStore, SharedMetadataStore};
    use picot_host::window_owner::WindowOwnerRegistry;
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TOUCH_TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    /// R5: the close delegate hands the confirmation to the WebView's
    /// coordinator and only runs its native warning when that hand-off could not
    /// produce an answer. Every non-settlement — including a failed op — must
    /// fall back: "ask the user anyway" is the safe direction.
    #[test]
    fn the_native_close_warning_runs_exactly_when_the_coordinator_did_not_settle() {
        use picot_host::daemon::owner_close::{CloseOutcome, OwnerCloseDelivery};
        let delivery = |delivered: bool, outcome: CloseOutcome| OwnerCloseDelivery {
            request_id: "close-1".to_string(),
            delivered,
            outcome,
        };

        // The coordinator settled: approving closes the window on its own path,
        // cancelling leaves it open — neither needs a native dialog.
        assert!(!close_request_needs_native_fallback(Ok(delivery(
            true,
            CloseOutcome::Approved
        ))));
        assert!(!close_request_needs_native_fallback(Ok(delivery(
            true,
            CloseOutcome::Cancelled
        ))));

        // No WebView held the owner, or it never answered: the native dialog is
        // the only confirmation left.
        assert!(close_request_needs_native_fallback(Ok(delivery(
            false,
            CloseOutcome::Undeliverable
        ))));
        assert!(close_request_needs_native_fallback(Ok(delivery(
            true,
            CloseOutcome::Timeout
        ))));

        // A dead admin channel or a malformed reply is the same answer.
        assert!(close_request_needs_native_fallback(Err(
            "the admin channel is not connected".to_string()
        )));
    }

    #[test]
    fn restored_window_geometry_is_clamped_to_monitor_work_area() {
        assert_eq!(
            clamp_window_geometry(
                tauri::PhysicalPosition::new(0, 68),
                tauri::PhysicalSize::new(2874, 1672),
                tauri::PhysicalRect {
                    position: tauri::PhysicalPosition::new(0, 0),
                    size: tauri::PhysicalSize::new(2560, 1400),
                },
            ),
            (0, 0, 2560, 1400)
        );
        assert_eq!(
            clamp_window_geometry(
                tauri::PhysicalPosition::new(100, 100),
                tauri::PhysicalSize::new(1400, 900),
                tauri::PhysicalRect {
                    position: tauri::PhysicalPosition::new(0, 0),
                    size: tauri::PhysicalSize::new(2560, 1400),
                },
            ),
            (100, 100, 1400, 900)
        );
        assert_eq!(
            clamp_window_geometry(
                tauri::PhysicalPosition::new(2400, 1300),
                tauri::PhysicalSize::new(2874, 900),
                tauri::PhysicalRect {
                    position: tauri::PhysicalPosition::new(0, 0),
                    size: tauri::PhysicalSize::new(2560, 1400),
                },
            ),
            (0, 500, 2560, 900)
        );
    }

    fn shared_test_metadata(label: &str) -> (SharedMetadataStore, PathBuf) {
        // Sequence + pid keeps parallel tests from sharing one database file.
        let sequence = TOUCH_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "pi-studio-{label}-{}-{sequence}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&temp).unwrap();
        (
            Arc::new(Mutex::new(
                MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
            )),
            temp,
        )
    }

    #[test]
    fn session_delete_paths_authorize_by_registered_cwd_and_reject_the_rest() {
        let (metadata, temp) = shared_test_metadata("del-filter");
        let root = temp.canonicalize().unwrap();
        metadata
            .lock()
            .unwrap()
            .add_workspace(&root)
            .expect("workspace registration");

        let write_session = |name: &str, cwd: &str| {
            let file = temp.join(name);
            // A user message is required: parse_session_header suppresses
            // trivially-short sessions with no user input at all.
            fs::write(
                &file,
                format!(
                    "{{\"type\":\"session\",\"version\":3,\"id\":\"{name}\",\"cwd\":\"{cwd}\"}}\n\
                     {{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n"
                ),
            )
            .unwrap();
            file.to_string_lossy().into_owned()
        };
        let registered = write_session("registered.jsonl", &root.to_string_lossy());
        let unregistered = {
            let other = temp.join("other");
            fs::create_dir_all(&other).unwrap();
            let other = other.canonicalize().unwrap();
            write_session("unregistered.jsonl", &other.to_string_lossy())
        };
        let missing = temp.join("missing.jsonl").to_string_lossy().into_owned();

        let (allowed, rejected) = filter_session_delete_paths(
            &[registered, unregistered.clone(), missing.clone()],
            &metadata,
        );
        assert_eq!(allowed.len(), 1);
        assert!(allowed[0].ends_with("registered.jsonl"));
        assert_eq!(rejected, vec![unregistered, missing]);
    }

    #[tokio::test]
    async fn post_spawn_recording_persists_the_pi_reported_bucket() {
        let manager = picot_host::native_pi_manager::NativePiManager::new(8);
        let (metadata, temp) = shared_test_metadata("bucket-record");
        let workspace = temp.join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let (row, added) = metadata.lock().unwrap().add_workspace(&workspace).unwrap();
        assert!(added);
        // The runtime target carries the registered row's workspace id —
        // production prepare resolves it the same way before spawning.
        let target = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
            row.workspace_id.clone(),
            "session-a",
            "instance-a",
            "owner-a",
            0,
        );
        let mut fake = manager.register_in_memory(target.clone()).unwrap();

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(16);
        let sink = picot_host::host_control::HostEventSink::new(event_tx);
        let owner = picot_host::window_owner::OwnerId::from_string("owner-a".to_string());
        let task = tokio::spawn(
            picot_host::host_control_dispatch::record_session_bucket_after_spawn(
                manager.clone(),
                metadata.clone(),
                sink,
                target.clone(),
                Some(owner.clone()),
            ),
        );

        // Pi answers get_state with the session file it allocated at start.
        let outbound = fake.read_request().await.unwrap();
        let id = outbound["id"].as_str().unwrap();
        fake.write_frame(json!({
            "id": id,
            "type": "response",
            "command": "get_state",
            "success": true,
            "data": { "sessionFile": "/home/user/.pi/agent/sessions/--ws-bucket--/s.jsonl" }
        }))
        .await
        .unwrap();
        task.await.unwrap();

        let event = loop {
            let event = event_rx.recv().await.unwrap();
            if event.owner.is_some() {
                break event;
            }
        };
        assert_eq!(event.owner, Some(owner));
        assert_eq!(event.value, runtime_started_event(&target));

        let stored = metadata
            .lock()
            .unwrap()
            .get_workspace(&row.workspace_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.session_bucket.as_deref(), Some("--ws-bucket--"));
    }

    #[tokio::test]
    async fn prepare_reuses_live_runtime_for_matching_persisted_session() {
        let manager = picot_host::native_pi_manager::NativePiManager::new(8);
        let registry = WindowOwnerRegistry::default();
        let target = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
            "workspace-a",
            "saved",
            "instance-a",
            "owner-a",
            0,
        );
        manager.register_in_memory(target.clone()).unwrap();

        assert_eq!(
            plan_prepare_reuse(
                &manager,
                &registry,
                "workspace-a",
                "owner-a",
                Some("saved"),
                false,
            ),
            PrepareReuse::Rebind(target),
        );
        assert_eq!(
            plan_prepare_reuse(
                &manager,
                &registry,
                "workspace-a",
                "owner-a",
                Some("saved"),
                true,
            ),
            PrepareReuse::Spawn,
        );
    }

    #[test]
    fn persisted_session_target_uses_verified_header_id() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let session = temp.path().join("2026-09-03T12-00-00-000Z_saved.jsonl");
        fs::create_dir_all(&workspace).unwrap();
        let canonical = workspace.canonicalize().unwrap();
        fs::write(
            &session,
            format!(
                "{{\"type\":\"session\",\"id\":\"saved\",\"cwd\":{}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n",
                serde_json::to_string(&canonical.to_string_lossy()).unwrap(),
            ),
        )
        .unwrap();

        assert_eq!(
            persisted_session_id_for_workspace(&session, &canonical).unwrap(),
            "saved",
        );
    }

    #[test]
    fn runtime_started_event_identifies_spawned_runtime() {
        use picot_host::runtime_coordinator::RuntimeTarget;
        let target =
            RuntimeTarget::with_owner("workspace-a", "session-a", "instance-a", "owner-a", 4);
        assert_eq!(
            runtime_started_event(&target),
            json!({
                "type": "runtime_started",
                "workspaceId": "workspace-a",
                "sessionId": "session-a",
                "instanceId": "instance-a",
            })
        );
    }

    #[tokio::test]
    async fn runtime_instance_summaries_span_every_registered_workspace() {
        use picot_host::native_pi_manager::NativePiManager;
        use picot_host::runtime_coordinator::RuntimeTarget;

        let manager = NativePiManager::new(8);
        let (metadata, temp) = shared_test_metadata("instance-summaries");
        // Session root mirrors Pi's layout: one bucket dir per workspace with
        // a header-matching JSONL the data plane can resolve.
        let sessions_root = temp.join("sessions");
        let seed = |bucket: &str, session_id: &str, cwd: &std::path::Path| {
            let dir = sessions_root.join(bucket);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join(format!("2026-09-18T00-00-00-000Z_{session_id}.jsonl")),
                format!(
                    "{{\"type\":\"session\",\"version\":3,\"id\":\"{session_id}\",\"cwd\":\"{}\"}}\n\
                     {{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n",
                    cwd.to_string_lossy(),
                ),
            )
            .unwrap();
        };
        let root_a = {
            fs::create_dir_all(temp.join("a")).unwrap();
            temp.join("a").canonicalize().unwrap()
        };
        let root_b = {
            fs::create_dir_all(temp.join("b")).unwrap();
            temp.join("b").canonicalize().unwrap()
        };
        let (row_a, _) = metadata.lock().unwrap().add_workspace(&root_a).unwrap();
        let (row_b, _) = metadata.lock().unwrap().add_workspace(&root_b).unwrap();
        metadata
            .lock()
            .unwrap()
            .set_workspace_session_bucket_from_pi(&row_a.workspace_id, "--ws-a--")
            .unwrap();
        metadata
            .lock()
            .unwrap()
            .set_workspace_session_bucket_from_pi(&row_b.workspace_id, "--ws-b--")
            .unwrap();
        seed("--ws-a--", "session-a", &root_a);
        seed("--ws-b--", "session-b", &root_b);

        // Two live runtimes under different owners; plus one runtime whose
        // workspace never registered (stays host-internal).
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                &row_a.workspace_id,
                "session-a",
                "instance-a",
                "owner-a",
                1,
            ))
            .unwrap();
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                &row_b.workspace_id,
                "session-b",
                "instance-b",
                "owner-b",
                2,
            ))
            .unwrap();
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                "workspace-ghost",
                "session-ghost",
                "instance-ghost",
                "owner-a",
                1,
            ))
            .unwrap();

        let summaries = runtime_instance_summaries(&manager, &metadata, Some(&sessions_root));
        let mut workspace_ids: Vec<&str> = summaries
            .iter()
            .filter_map(|entry| entry["workspaceId"].as_str())
            .collect();
        workspace_ids.sort_unstable();
        let mut expected = vec![row_a.workspace_id.as_str(), row_b.workspace_id.as_str()];
        expected.sort_unstable();
        assert_eq!(
            workspace_ids, expected,
            "every registered workspace's runtime is visible to any owner"
        );
        let a = summaries
            .iter()
            .find(|entry| entry["sessionId"] == "session-a")
            .unwrap();
        assert_eq!(
            a["cwd"],
            serde_json::json!(root_a.to_string_lossy().to_string())
        );
        assert!(a["sessionFile"]
            .as_str()
            .unwrap()
            .contains("session-a.jsonl"));
    }

    #[tokio::test]
    async fn runtime_instance_summaries_prefers_retained_session_file_without_rescan() {
        use picot_host::native_pi_manager::NativePiManager;
        use picot_host::runtime_coordinator::RuntimeTarget;

        let manager = NativePiManager::new(8);
        let (metadata, temp) = shared_test_metadata("instance-retained");
        let sessions_root = temp.join("sessions");
        fs::create_dir_all(&sessions_root).unwrap();
        // No workspace session bucket is registered, so the id rescan cannot
        // resolve any file. A summary that still carries the retained file
        // therefore proves the per-runtime scan was skipped, not merely that a
        // scan happened to succeed.
        let retained = sessions_root.join("retained.jsonl");
        fs::write(
            &retained,
            "{\"type\":\"session\",\"version\":3,\"id\":\"session-retained\"}\n\
             {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();
        let retained = retained.canonicalize().unwrap();
        let root = {
            fs::create_dir_all(temp.join("ws")).unwrap();
            temp.join("ws").canonicalize().unwrap()
        };
        let (row, _) = metadata.lock().unwrap().add_workspace(&root).unwrap();

        let target = RuntimeTarget::with_owner(
            &row.workspace_id,
            "session-retained",
            "instance-retained",
            "owner-a",
            1,
        );
        manager.register_in_memory(target.clone()).unwrap();
        manager
            .note_session_file(&target, retained.clone())
            .unwrap();

        // Precondition: the bucket lookup alone resolves nothing.
        let data = picot_host::host_data::HostDataPlane::new(metadata.clone())
            .with_session_root(sessions_root.clone());
        assert!(data
            .session_file_path(&row.workspace_id, "session-retained")
            .is_none());

        let summaries = runtime_instance_summaries(&manager, &metadata, Some(&sessions_root));
        let entry = summaries
            .iter()
            .find(|entry| entry["instanceId"] == "instance-retained")
            .expect("retained runtime is summarized");
        assert_eq!(
            entry["sessionFile"],
            serde_json::json!(retained.to_string_lossy().to_string())
        );
    }

    #[tokio::test]
    async fn runtime_instance_summaries_report_streaming_state() {
        use picot_host::native_pi_manager::NativePiManager;
        use picot_host::runtime_coordinator::RuntimeTarget;

        // `streaming` mirrors the coordinator's event-driven state so the
        // sidebar can light the green dot for a turn that began before this
        // page subscribed (missed agent_start). agent_start flips the runtime
        // to Working; agent_end flips it back to Idle.
        let manager = NativePiManager::new(8);
        let (metadata, temp) = shared_test_metadata("instance-streaming");
        let sessions_root = temp.join("sessions");
        let bucket = sessions_root.join("--ws-a--");
        fs::create_dir_all(&bucket).unwrap();
        let root_a = {
            fs::create_dir_all(temp.join("a")).unwrap();
            temp.join("a").canonicalize().unwrap()
        };
        let (row_a, _) = metadata.lock().unwrap().add_workspace(&root_a).unwrap();
        metadata
            .lock()
            .unwrap()
            .set_workspace_session_bucket_from_pi(&row_a.workspace_id, "--ws-a--")
            .unwrap();
        fs::write(
            bucket.join("2026-09-18T00-00-00-000Z_session-a.jsonl"),
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"session-a\",\"cwd\":\"{}\"}}\n\
                 {{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n",
                root_a.to_string_lossy(),
            ),
        )
        .unwrap();
        let target =
            RuntimeTarget::with_owner(&row_a.workspace_id, "session-a", "instance-a", "owner-a", 1);
        let mut fake = manager.register_in_memory(target).unwrap();

        let streaming = || {
            runtime_instance_summaries(&manager, &metadata, Some(&sessions_root))
                .into_iter()
                .find(|entry| entry["sessionId"] == "session-a")
                .map(|entry| entry["streaming"].clone())
                .unwrap_or(serde_json::Value::Null)
        };
        assert_eq!(streaming(), serde_json::json!(false), "idle at rest");
        // The event pump applies frames on its own task; a single yield does
        // not guarantee the coordinator state has flipped under parallel
        // test load, so poll briefly for each transition before asserting.
        fake.write_frame(serde_json::json!({"type":"agent_start"}))
            .await
            .unwrap();
        for _ in 0..400 {
            if streaming() == serde_json::json!(true) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(streaming(), serde_json::json!(true), "working mid-turn");
        fake.write_frame(serde_json::json!({"type":"agent_end"}))
            .await
            .unwrap();
        for _ in 0..400 {
            if streaming() == serde_json::json!(false) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(streaming(), serde_json::json!(false), "idle after end");
    }

    #[test]
    fn transition_cleanup_predicate_separates_same_and_cross_workspace() {
        // The predicate still gates per-generation cleanup (export grants,
        // side-chat leases); runtimes themselves are no longer stopped by it.
        let workspace = PathBuf::from("/workspace");
        assert!(!should_stop_owner_runtimes_on_transition(
            Some(workspace.as_path()),
            Some(workspace.as_path()),
        ));
        assert!(should_stop_owner_runtimes_on_transition(
            Some(workspace.as_path()),
            Some(PathBuf::from("/other-workspace").as_path()),
        ));
    }

    #[tokio::test]
    async fn restart_targets_only_the_current_workspaces_runtime() {
        use picot_host::native_pi_manager::NativePiManager;
        use picot_host::runtime_coordinator::RuntimeTarget;

        let manager = NativePiManager::new(8);
        // The owner keeps a stale runtime alive in workspace A (generation 1)
        // after switching to workspace B, where the current runtime lives.
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                "workspace-a",
                "session-a",
                "instance-a",
                "owner-x",
                1,
            ))
            .unwrap();
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                "workspace-b",
                "session-b",
                "instance-b",
                "owner-x",
                5,
            ))
            .unwrap();
        // A second live session in the current workspace: the newest
        // generation is the one the WebView is bound to.
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                "workspace-b",
                "session-b-old",
                "instance-b-old",
                "owner-x",
                4,
            ))
            .unwrap();
        // Another owner's runtime in the same workspace is never a candidate.
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                "workspace-b",
                "session-other",
                "instance-other",
                "owner-y",
                9,
            ))
            .unwrap();

        assert_eq!(
            target_for_owner_in_workspace(&manager, "owner-x", "workspace-b"),
            Some(RuntimeTarget::with_owner(
                "workspace-b",
                "session-b",
                "instance-b",
                "owner-x",
                5,
            )),
            "restart must pick the current workspace's newest runtime, never A's"
        );
        assert_eq!(
            target_for_owner_in_workspace(&manager, "owner-x", "workspace-c"),
            None,
            "no live runtime in an unbound workspace means no restart target"
        );
    }

    /// Side/Quick chat runtimes live in the owner's registered workspace under
    /// the SAME generation, so `max_by_key(generation)` left the pick to
    /// HashMap order and a manual restart could stop the side chat instead.
    #[tokio::test]
    async fn restart_prefers_the_session_runtime_over_a_same_generation_ephemeral_one() {
        use picot_host::native_pi_manager::NativePiManager;
        use picot_host::runtime_coordinator::RuntimeTarget;

        let manager = NativePiManager::new(8);
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                "workspace-b",
                "session-b",
                "instance-a-main",
                "owner-x",
                5,
            ))
            .unwrap();
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                "workspace-b",
                "ephemeral-instance-side",
                "instance-z-side",
                "owner-x",
                5,
            ))
            .unwrap();

        assert_eq!(
            target_for_owner_in_workspace(&manager, "owner-x", "workspace-b").map(|t| t.session_id),
            Some("session-b".to_string()),
            "a same-generation side chat must never win the restart target"
        );
    }

    /// With only a side chat live there is no session runtime to restart: the
    /// caller must fail loudly rather than restart the wrong thing.
    #[tokio::test]
    async fn restart_reports_no_target_when_only_an_ephemeral_runtime_is_live() {
        use picot_host::native_pi_manager::NativePiManager;
        use picot_host::runtime_coordinator::RuntimeTarget;

        let manager = NativePiManager::new(8);
        manager
            .register_in_memory(RuntimeTarget::with_owner(
                "workspace-b",
                "ephemeral-instance-side",
                "instance-z-side",
                "owner-x",
                5,
            ))
            .unwrap();

        assert_eq!(
            target_for_owner_in_workspace(&manager, "owner-x", "workspace-b"),
            None
        );
    }

    #[tokio::test]
    async fn cross_workspace_return_reuses_the_prior_runtime() {
        let manager = picot_host::native_pi_manager::NativePiManager::new(8);
        let prior = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
            "workspace-a",
            "saved-session",
            "instance-a",
            "owner-a",
            1,
        );
        manager.register_in_memory(prior.clone()).unwrap();

        // A cross-workspace commit must not stop A. While B is current,
        // authorize_target denies this old target; returning to A adopts it.
        assert_eq!(
            plan_prepare_reuse(
                &manager,
                &WindowOwnerRegistry::default(),
                "workspace-a",
                "owner-a",
                Some("saved-session"),
                false,
            ),
            PrepareReuse::Rebind(prior.clone()),
        );
        assert!(manager.rebind_owner_generation("owner-a", "saved-session", 3));
        assert_eq!(
            manager.target_for_session_id("saved-session"),
            Some(picot_host::runtime_coordinator::RuntimeTarget::with_owner(
                "workspace-a",
                "saved-session",
                "instance-a",
                "owner-a",
                3,
            )),
        );
    }

    #[test]
    fn cancel_stops_only_the_instance_its_transition_created() {
        let created = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
            "workspace-a",
            "session-new",
            "instance-new",
            "owner-a",
            3,
        );
        // A transition that spawned its own runtime may stop exactly that one.
        assert!(cancel_should_stop_target(Some("instance-new"), &created));
        // A different live runtime (sibling session) is never cancel's target.
        assert!(!cancel_should_stop_target(
            Some("instance-new"),
            &picot_host::runtime_coordinator::RuntimeTarget::with_owner(
                "workspace-a",
                "session-live",
                "instance-live",
                "owner-a",
                1,
            ),
        ));
        // A REUSED prepare records no created instance: cancel must not stop
        // the reused runtime, even when session ids match the pending record.
        let reused = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
            "workspace-b",
            "session-live",
            "instance-live",
            "owner-a",
            1,
        );
        assert!(!cancel_should_stop_target(None, &reused));
    }

    #[test]
    fn new_session_menu_enable_truth_table() {
        // No focused owner-bound window (startup default): disabled.
        assert!(!new_session_menu_enabled(None));
        // Focused owner without a workspace binding (landing pre-transition):
        // disabled.
        assert!(!new_session_menu_enabled(Some(false)));
        // Focused owner bound to a workspace — including a transitioned
        // landing window whose label is still `native-landing`: enabled.
        assert!(new_session_menu_enabled(Some(true)));
    }

    #[test]
    fn next_landing_label_keeps_the_first_window_suffixless() {
        // The first landing window keeps the suffixless label: window-state
        // geometry, ledger rows, and mirror rows all key on it, so existing
        // single-window installations see no change (multi-window §6.1).
        assert_eq!(next_landing_label(|_| false), "native-landing");
    }

    #[test]
    fn next_landing_label_suffixes_from_two_and_takes_the_first_free_slot() {
        assert_eq!(
            next_landing_label(|label| label == "native-landing"),
            "native-landing-2"
        );
        assert_eq!(
            next_landing_label(|label| label == "native-landing" || label == "native-landing-2"),
            "native-landing-3"
        );
        // A closed window frees its label: the first free slot is reused, the
        // sequence does not ratchet monotonically upward.
        assert_eq!(
            next_landing_label(|label| label == "native-landing" || label == "native-landing-3"),
            "native-landing-2"
        );
    }

    #[test]
    fn landing_owner_rejects_workspace_snapshot_but_admits_quick_chat() {
        use picot_host::window_owner::{
            OwnerWorkspaceSnapshot, TemporaryKind, WindowOwnerRegistry,
        };
        use std::sync::{Arc, Mutex as StdMutex};
        let registry = Arc::new(WindowOwnerRegistry::default());
        let (owner, _) = registry
            .create_owner_with_workspace(
                "native-landing".to_string(),
                PathBuf::from("/home"),
                0,
                "http://127.0.0.1:3001".to_string(),
                None,
                TemporaryKind::Landing,
            )
            .unwrap();
        // Side Chat / workspace scope: rejected — the placeholder home root is
        // never a workspace scope.
        assert!(workspace_snapshot_for(&registry, &owner).is_err());
        // Quick Chat: the explicit landing exception — admitted with an empty
        // workspace id; the spawn path supplies its own temp cwd.
        let (wid, _root, generation) = quick_chat_snapshot(&registry, &owner).unwrap();
        assert!(wid.is_empty());
        assert_eq!(generation, 0);
        // A registered owner is admitted by both.
        let (owner2, _) = registry
            .create_owner_with_workspace(
                "native-workspace-x".to_string(),
                PathBuf::from("/workspace"),
                3001,
                "http://127.0.0.1:3001".to_string(),
                Some("wid-x".to_string()),
                TemporaryKind::DefaultStartup,
            )
            .unwrap();
        assert_eq!(
            workspace_snapshot_for(&registry, &owner2).unwrap(),
            ("wid-x".to_string(), PathBuf::from("/workspace"), 0)
        );
        assert_eq!(
            quick_chat_snapshot(&registry, &owner2).unwrap(),
            ("wid-x".to_string(), PathBuf::from("/workspace"), 0)
        );
        // An unknown owner is rejected by both.
        let unknown = registry
            .create_owner_with_workspace(
                "gone".to_string(),
                PathBuf::from("/gone"),
                3001,
                "http://127.0.0.1:3001".to_string(),
                None,
                TemporaryKind::Landing,
            )
            .unwrap()
            .0;
        registry.revoke_owner(&unknown);
        assert!(workspace_snapshot_for(&registry, &unknown).is_err());
        assert!(quick_chat_snapshot(&registry, &unknown).is_err());
        // Project-scoped skills reject the landing owner; global scope is
        // admitted (it never reads the workspace).
        assert!(skill_scope_context(
            &registry,
            &owner,
            "project",
            Path::new("/nonexistent-agent")
        )
        .is_err());
        assert!(
            skill_scope_context(&registry, &owner, "global", Path::new("/nonexistent-agent"))
                .is_ok()
        );
        // Silence unused-import lint for the shared helper type alias in
        // non-test builds.
        let _ = StdMutex::new(());
        let _ = OwnerWorkspaceSnapshot::NoWorkspace;
    }

    #[test]
    fn skill_scope_context_reports_project_trust_for_every_scope() {
        use picot_host::window_owner::{TemporaryKind, WindowOwnerRegistry};
        use std::sync::Arc;
        let registry = Arc::new(WindowOwnerRegistry::default());
        let agent_root = tempfile::tempdir().expect("agent root temp dir");
        let workspace = tempfile::tempdir().expect("workspace temp dir");
        picot_host::project_trust::trust_project(agent_root.path(), workspace.path())
            .expect("seed trust entry");
        let (owner, _) = registry
            .create_owner_with_workspace(
                "native-trust-global".to_string(),
                workspace.path().canonicalize().unwrap(),
                3001,
                "http://127.0.0.1:3001".to_string(),
                Some("wid-trust-global".to_string()),
                TemporaryKind::DefaultStartup,
            )
            .unwrap();
        let canonical = workspace.path().canonicalize().unwrap();
        // Global listings must carry the real project-trust state — the
        // inventory includes project roots in every scope's response so the
        // frontend can compute both scope badges from one load.
        let (cwd, trusted) =
            skill_scope_context(&registry, &owner, "global", agent_root.path()).unwrap();
        assert_eq!(cwd, canonical);
        assert!(trusted, "global scope must report the real trust state");
        let (_, project_trusted) =
            skill_scope_context(&registry, &owner, "project", agent_root.path()).unwrap();
        assert!(project_trusted);
    }

    #[test]
    fn registry_touch_uses_host_recorded_cwd_and_never_creates_rows() {
        let (metadata, temp) = shared_test_metadata("touch");
        let project = temp.join("project");
        fs::create_dir_all(&project).unwrap();
        let row = metadata.lock().unwrap().add_workspace(&project).unwrap().0;

        // Host-recorded spelling differs from the registered canonical path;
        // canonicalization inside the store must still match it.
        let recorded = temp.join(".").join("project");
        assert!(touch_registered_workspace(
            &metadata,
            Some(recorded.to_string_lossy().to_string())
        ));
        let touched = metadata
            .lock()
            .unwrap()
            .get_workspace(&row.workspace_id)
            .unwrap()
            .unwrap();
        assert!(touched.last_opened_at.is_some());

        // Unregistered cwd (e.g. ephemeral dirs): no touch, and crucially no
        // phantom registry row.
        let ghost = temp.join("ghost");
        fs::create_dir_all(&ghost).unwrap();
        assert!(!touch_registered_workspace(
            &metadata,
            Some(ghost.to_string_lossy().to_string())
        ));

        // Unknown port record (None) is a silent no-op.
        assert!(!touch_registered_workspace(&metadata, None));

        let rows = metadata
            .lock()
            .unwrap()
            .list_workspaces_and_prune()
            .unwrap()
            .0;
        assert_eq!(rows.len(), 1, "touch must never grow the registry");
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pi-studio-{label}-{suffix}"))
    }

    #[test]
    fn debug_build_prefers_workspace_public_over_bundled_copy() {
        let root = unique_temp_dir("static-dir-debug");
        let workspace_public = root.join("workspace").join("public");
        let bundled_public = root.join("bundled").join("public");

        fs::create_dir_all(&workspace_public).unwrap();
        fs::create_dir_all(&bundled_public).unwrap();
        fs::write(workspace_public.join("index.html"), "workspace").unwrap();
        fs::write(bundled_public.join("index.html"), "bundled").unwrap();

        let resolved = resolve_static_dir(
            Some(root.join("bundled")),
            workspace_public.clone(),
            Some(root.join("workspace")),
            true,
        );

        assert_eq!(resolved, fs::canonicalize(&workspace_public).unwrap());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn image_mime_from_path_maps_known_extensions() {
        assert_eq!(
            image_mime_from_path(std::path::Path::new("photo.png")),
            Some("image/png")
        );
        assert_eq!(
            image_mime_from_path(std::path::Path::new("photo.JPG")),
            Some("image/jpeg")
        );
        assert_eq!(
            image_mime_from_path(std::path::Path::new("photo.Jpeg")),
            Some("image/jpeg")
        );
        assert_eq!(
            image_mime_from_path(std::path::Path::new("anim.gif")),
            Some("image/gif")
        );
        assert_eq!(
            image_mime_from_path(std::path::Path::new("modern.webp")),
            Some("image/webp")
        );
    }

    #[test]
    fn image_mime_from_path_rejects_unknown_and_missing_extensions() {
        assert_eq!(image_mime_from_path(std::path::Path::new("doc.pdf")), None);
        assert_eq!(image_mime_from_path(std::path::Path::new("img.bmp")), None);
        assert_eq!(image_mime_from_path(std::path::Path::new("noext")), None);
        assert_eq!(image_mime_from_path(std::path::Path::new("/")), None);
    }

    #[test]
    fn side_chat_startup_rpc_emits_set_model_and_thinking_for_active_profile() {
        let profile = json!({
            "provider": "openai-codex",
            "modelId": "gpt-5.6-terra",
            "thinkingLevel": "medium",
        });
        let cmds = side_chat_startup_rpc_commands(&profile);
        assert_eq!(cmds.len(), 2);
        assert_eq!(
            cmds[0],
            json!({"type":"set_model","provider":"openai-codex","modelId":"gpt-5.6-terra"})
        );
        assert_eq!(
            cmds[1],
            json!({"type":"set_thinking_level","level":"medium"})
        );
    }

    #[test]
    fn side_chat_startup_rpc_omits_thinking_when_off() {
        let profile = json!({
            "provider": "anthropic",
            "modelId": "claude-sonnet-4",
            "thinkingLevel": "off",
        });
        let cmds = side_chat_startup_rpc_commands(&profile);
        assert_eq!(cmds.len(), 1);
        assert_eq!(
            cmds[0],
            json!({"type":"set_model","provider":"anthropic","modelId":"claude-sonnet-4"})
        );
    }

    #[test]
    fn side_chat_startup_rpc_drops_profile_without_model_identity() {
        assert!(side_chat_startup_rpc_commands(&json!({"thinkingLevel":"medium"})).is_empty());
        assert!(side_chat_startup_rpc_commands(&json!({"provider":"","modelId":""})).is_empty());
    }
}

/// Canonical home directory for the landing owner record. It is an
/// owner-record placeholder only — never a workspace identity, workspace
/// scope, or authorization input.
fn canonical_home_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot resolve home directory".to_string())?;
    fs::canonicalize(&home).map_err(|error| format!("Cannot canonicalize home directory: {error}"))
}

/// The app's live daemon attachment (plan Task 5/T9).
///
/// The app's side of the process boundary: the admin client, the navigation
/// mirror it feeds, and the manager whose spawn and child handle the quit path
/// (T8) and the restart transaction (T9) reuse. The daemon is always its own
/// process (T12 deleted the in-process mode) — this struct holds no host state.
///
/// The **session** is swappable: a restart replaces the daemon, so the admin
/// client, the origin the WebViews load from and the process identity all
/// change together ([`DaemonLink::replace_session`]). Everything else in the
/// app reads them through this one place.
///
/// Managed as app state so the window lifecycle, the menu gate and
/// `RunEvent::Exit` can reach it; nothing here is a Tauri command (D8 keeps the
/// app's command surface to the three daemon commands, T10).
pub struct DaemonSession {
    pub client: Arc<AdminClient>,
    /// The daemon's HTTP origin (the WebView's page origin).
    pub host_origin: String,
    /// The daemon process identity captured at attach: pid + process start
    /// time, the only thing a stop/restart is allowed to signal (plan D12).
    /// `None` only when no verified identity could be captured — the quit path
    /// then never waits on or signals anything (defensive: attach publishes
    /// one, plan D12).
    pub daemon: Option<ProcessIdentity>,
}

pub struct DaemonLink {
    session: RwLock<DaemonSession>,
    pub mirror: Arc<NavigationMirror>,
    /// `Some` once the daemon was started/adopted by this app (plan D4/T4):
    /// its config/identity, the spawn path the restart transaction reuses
    /// (T9) and the child handle the quit flow reaps (T8).
    pub manager: Option<Arc<DaemonManager>>,
    /// The daemon's private data directory: `instance_path`/`log_path` below
    /// are derived from it (the quit flow names both).
    pub data_dir: PathBuf,
    /// `app_config_dir`: the app-private quit policy (`keepRunningAfterQuit`)
    /// lives here and is read *at exit*, never by the daemon (spec §3.2.2).
    pub config_dir: PathBuf,
    /// Labels the restart transaction is destroying and re-creating (T9): the
    /// window-destroyed path must not detach the owner it is about to re-mint
    /// for the same label. Shared with its [`MigrationGuard`], which is what
    /// clears the marks on every exit from the transaction.
    migrating: Arc<Mutex<std::collections::HashSet<String>>>,
}

/// RAII owner of the restart transaction's migration marks (T9; A4 follow-up
/// #1).
///
/// `migrate_views` runs `begin_migration → recreate_windows().await →
/// end_migration`. If that future is dropped mid-`await` — or a panic unwinds
/// through it — a plain `end_migration` call never runs: the marks stay set,
/// [`DaemonLink::is_migrating_any`] stays `true`, and since
/// `classify_exit_request` holds **every** exit request while a migration is in
/// flight, the app holds every later quit with no window left to re-open — the
/// window-less zombie app this guard exists to prevent.
///
/// Clearing in `Drop` covers all three abandonment paths (early return, a
/// dropped future, an unwind) with one piece of code, so no caller can forget
/// it. The marks travel through an `Arc` rather than a borrow of the link so
/// the guard can live across the migration's `.await` points.
pub struct MigrationGuard {
    migrating: Arc<Mutex<std::collections::HashSet<String>>>,
    labels: Vec<String>,
}

impl MigrationGuard {
    /// Mark `labels` as mid-migration and take ownership of clearing them.
    fn begin(migrating: Arc<Mutex<std::collections::HashSet<String>>>, labels: &[String]) -> Self {
        migrating
            .lock()
            .expect("migration label lock")
            .extend(labels.iter().cloned());
        Self {
            migrating,
            labels: labels.to_vec(),
        }
    }
}

impl Drop for MigrationGuard {
    fn drop(&mut self) {
        let mut marks = self.migrating.lock().expect("migration label lock");
        for label in &self.labels {
            marks.remove(label);
        }
    }
}

impl DaemonLink {
    pub fn new(
        session: DaemonSession,
        mirror: Arc<NavigationMirror>,
        manager: Option<Arc<DaemonManager>>,
        data_dir: PathBuf,
        config_dir: PathBuf,
    ) -> Self {
        Self {
            session: RwLock::new(session),
            mirror,
            manager,
            data_dir,
            config_dir,
            migrating: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// The live admin client (a restart swaps it).
    pub fn client(&self) -> Arc<AdminClient> {
        Arc::clone(&self.session.read().expect("daemon session lock").client)
    }

    /// The origin the WebViews load from.
    pub fn host_origin(&self) -> String {
        self.session
            .read()
            .expect("daemon session lock")
            .host_origin
            .clone()
    }

    /// The verified daemon identity, if this app captured one at attach.
    pub fn daemon(&self) -> Option<ProcessIdentity> {
        self.session
            .read()
            .expect("daemon session lock")
            .daemon
            .clone()
    }

    /// Install a replacement attachment and return the superseded session.
    ///
    /// The caller disconnects the old client immediately: its fail-closed
    /// mirror clear must never land after the new snapshot (plan T9 step 4).
    pub fn replace_session(&self, session: DaemonSession) -> DaemonSession {
        let mut slot = self.session.write().expect("daemon session lock");
        std::mem::replace(&mut *slot, session)
    }

    /// Mark labels as being migrated by the restart transaction (T9).
    ///
    /// The returned [`MigrationGuard`] clears exactly these marks when it drops
    /// — the fix for the abandoned-transaction zombie app (A4 follow-up #1):
    /// the caller cannot forget an `end_migration` it never has to call.
    pub fn begin_migration(&self, labels: &[String]) -> MigrationGuard {
        MigrationGuard::begin(Arc::clone(&self.migrating), labels)
    }

    /// Whether the restart transaction currently owns this label.
    pub fn is_migrating(&self, label: &str) -> bool {
        self.migrating
            .lock()
            .expect("migration label lock")
            .contains(label)
    }

    /// Whether **any** label is mid-migration: the transaction destroys every
    /// captured window before re-creating it at the new origin (T9 step 5), so
    /// for a moment the app legitimately has zero windows. The exit path reads
    /// this to tell "the user closed Picot" apart from "the migration is
    /// between two windows" (plan D10/T9 × T8).
    pub fn is_migrating_any(&self) -> bool {
        !self
            .migrating
            .lock()
            .expect("migration label lock")
            .is_empty()
    }

    /// The app-side owner/authorization image the daemon feeds (plan D4).
    fn mirror_for(app: &AppHandle) -> Option<Arc<NavigationMirror>> {
        app.try_state::<DaemonLink>()
            .map(|link| Arc::clone(&link.mirror))
    }

    /// The daemon's instance file (`<data_dir>/picot-daemon.json`).
    pub fn instance_path(&self) -> PathBuf {
        picot_host::daemon::instance::instance_path(&self.data_dir)
    }

    /// The daemon's log file (`<data_dir>/picot-daemon.log`).
    fn log_path(&self) -> PathBuf {
        picot_host::daemon::instance::log_path(&self.data_dir)
    }

    /// The app-process half of app exit (plan Task 8 Step 2 / D12).
    ///
    /// `keepRunningAfterQuit=true` releases the admin channel only — dropping
    /// the socket is what triggers the daemon's dead-attachment cleanup, and the
    /// daemon plus its resident runtimes keep running. With the default `false`,
    /// the daemon is stopped and the app waits for a **confirmed** death
    /// (pid + start time; SIGTERM → SIGKILL on the ladder) before it lets the
    /// process go.
    fn shutdown(&self) -> QuitOutcome {
        let action = quit_policy::quit_action(
            // Read *at exit* (spec §3.2.2): a toggle flipped during the session
            // applies to the next quit, never to the daemon's current life.
            quit_policy::keep_running_after_quit(&self.config_dir),
        );
        match action {
            quit_policy::QuitAction::DetachOnly => {
                log::info!(
                    "[picot] exiting: keepRunningAfterQuit — detaching; the daemon and its agents keep running"
                );
                self.client().disconnect();
                QuitOutcome::Detached
            }
            quit_policy::QuitAction::StopAndConfirm => self.stop_and_confirm(),
        }
    }

    /// Default exit: stop the daemon, then block on a **confirmed**
    /// death (plan D12). Only a verified identity is waited on or signalled.
    fn stop_and_confirm(&self) -> QuitOutcome {
        let client = self.client();
        let Some(identity) = self.daemon() else {
            // No verified process identity was captured (the daemon is not this
            // app's to signal): releasing the channel is all that is safe.
            log::warn!(
                "[picot] exiting: no verified daemon identity; releasing the admin channel only"
            );
            client.disconnect();
            return QuitOutcome::Detached;
        };
        let flow = quit_policy::QuitFlow::new(
            Arc::new(quit_policy::AdminStopRequester::new(client)),
            ExitWaiter::new(),
            identity,
            self.manager
                .as_ref()
                .and_then(|manager| manager.take_child()),
            self.instance_path(),
            self.log_path(),
            quit_policy::QUIT_CONFIRM_DEADLINE,
        );
        let outcome = tauri::async_runtime::block_on(flow.run());
        // The daemon is gone (or unconfirmed): either way the app's channel is
        // done, and closing it deliberately keeps the next attach clean.
        self.client().disconnect();
        outcome
    }
}

/// What booting the daemon produced.
enum AttachedDaemon {
    Ready {
        manager: Option<Arc<DaemonManager>>,
        client: Arc<AdminClient>,
        host_origin: String,
        /// The daemon process identity: what the quit flow is allowed to wait
        /// on and signal (plan D12).
        daemon: Option<ProcessIdentity>,
    },
    /// Another live Picot app manages this daemon (plan Task 4 policy).
    BlockedByOtherApp,
}

/// Start the daemon as its own process and attach to it (plan Task 5 Step 3;
/// the only path since T12 deleted the in-process mode).
async fn attach_daemon(
    data_dir: PathBuf,
    config_dir: PathBuf,
    static_dir: PathBuf,
    bundle_identifier: String,
    executor: Arc<dyn SurfaceExecutor>,
    mirror: Arc<NavigationMirror>,
) -> Result<AttachedDaemon, String> {
    let manager = Arc::new(DaemonManager::for_app(ManagerConfig::for_app(
        data_dir,
        config_dir,
        static_dir,
        bundle_identifier,
    )));
    let file = match manager.ensure().await {
        Ok(EnsureOutcome::Started(file)) | Ok(EnsureOutcome::Attached(file)) => file,
        Ok(EnsureOutcome::BlockedByOtherApp) => return Ok(AttachedDaemon::BlockedByOtherApp),
        Err(error) => return Err(format!("cannot reach the Picot daemon: {error}")),
    };
    let client = AdminClient::connect(
        &file.admin_origin,
        &file.admin_token,
        &manager.config().identity(),
        executor,
        mirror,
    )
    .await?;
    log::info!(
        "[picot] external daemon pid={} host={} admin={}",
        file.pid,
        file.host_origin,
        file.admin_origin
    );
    // The identity the quit path will wait on and (worst case) signal: pid plus
    // the start time the daemon itself published, so a reused pid is never
    // mistaken for this daemon (plan D12).
    let daemon = ProcessIdentity::new(file.pid, file.process_start_time.clone());
    Ok(AttachedDaemon::Ready {
        manager: Some(manager),
        client,
        host_origin: file.host_origin,
        daemon: Some(daemon),
    })
}

/// What `setup_daemon_runtime` produced when it did *not* fail.
///
/// The second-instance block is not an error: it is a successful decision to
/// leave. It still cannot present its dialog from there (see
/// [`daemon_client::present_blocked_by_other_app`]), so `setup` records it and
/// the `Ready` arm presents it — hence it is a value returned to the hook, not
/// a side effect performed in it.
enum SetupOutcome {
    /// The app booted around its daemon: the landing window is open and the
    /// runtime state is managed.
    Ready,
    /// Another live Picot app manages this daemon (plan Task 4 policy). Parked
    /// for `Ready` to present; nothing past this point ran.
    BlockedByOtherApp,
}

/// Boot the app around its daemon (plan Task 5 Step 3).
///
/// Errors come back as a [`StartupFailure`] — the reason plus the daemon log
/// file to point the user at — because the caller does not panic on them: the
/// dialog is presented from the run loop's `Ready` event (see
/// [`daemon_client::startup_failure`]). Every step past the data directory is
/// tagged with that directory's daemon log.
///
/// One sequence since T12 deleted the in-process mode: get the daemon running
/// as its own process (`DaemonManager::ensure`), connect the `AdminClient` to
/// its admin listener, mint the landing owner over that channel, and open the
/// landing window at the daemon's host origin. There is no in-process shortcut —
/// the app holds no host state (plan D2/D4).
///
/// A successful call returns which of the two non-error outcomes it reached
/// ([`SetupOutcome`]); a second-instance block returns before any of the above
/// is set up.
fn setup_daemon_runtime(
    app: &mut tauri::App,
    static_dir: PathBuf,
) -> Result<SetupOutcome, StartupFailure> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Cannot resolve Picot app data directory: {error}"))?;
    // From here on a failure can name the daemon log: a detached daemon's only
    // other trace (`StartupFailure::for_data_dir`).
    let fail = {
        let data_dir = data_dir.clone();
        move |reason: String| StartupFailure::for_data_dir(reason, &data_dir)
    };
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|error| format!("Cannot resolve Picot app config directory: {error}"))
        .map_err(&fail)?;
    let bundle_identifier = app.config().identifier.clone();
    // The app's window registry (T9/T10): recorded as windows are created, read
    // by the restart transaction's capture. Managed before the daemon boots, so
    // the landing window's row can be written as it opens.
    app.manage(WindowLedger::new());
    // Per-window focus stamps (multi-window §7): the notification-click route
    // ranks candidate windows by most-recently-focused.
    app.manage(FocusTimes::new());
    app.manage(host_client::HostClientState(host_client::HostClient::load(
        picot_host::relay::host_store::hosts_path(&config_dir),
    )));
    // The app's navigation/authorization image and the pump that executes
    // forwarded surface ops.
    let mirror = Arc::new(NavigationMirror::new());
    let executor: Arc<dyn SurfaceExecutor> = Arc::new(ForwardedSurfaceOps::new(Arc::new(
        tauri_app_surface::TauriAppSurface::new(app.handle().clone(), Arc::clone(&mirror)),
    )));
    let attached = tauri::async_runtime::block_on(attach_daemon(
        data_dir.clone(),
        config_dir.clone(),
        static_dir,
        bundle_identifier,
        executor,
        Arc::clone(&mirror),
    ))
    .map_err(&fail)?;
    let (manager, client, host_origin, daemon) = match attached {
        AttachedDaemon::BlockedByOtherApp => {
            // A second Picot must not fight the first for the daemon's lease,
            // so this instance leaves non-zero. It cannot show that dialog from
            // here: `setup` runs on the main thread that the dialog plugin
            // posts to, and joining the dialog thread there deadlocks (no
            // alert, no watchdog, no exit — R-1). Park it and let the `Ready`
            // arm present it; `None` resolves the locale from the OS (T10
            // passes the app's preference).
            return Ok(SetupOutcome::BlockedByOtherApp);
        }
        AttachedDaemon::Ready {
            manager,
            client,
            host_origin,
            daemon,
        } => (manager, client, host_origin, daemon),
    };
    // Browser panes (spec 2026-09-22): child webviews keyed by tab id. They are
    // the app's by construction (plan D1/Step-3 exceptions) — the webview needs
    // this process's `AppHandle` — and the daemon forwards `browser_pane_*` ops
    // to them as one app-surface op (row A13). Registered against the daemon
    // origin the panes must never reach.
    let browser_panes = Arc::new(browser_pane::BrowserPaneRuntime::new(
        app.handle().clone(),
        &host_origin,
    ));
    app.manage(browser_pane::BrowserPaneState(browser_panes));
    // The landing owner: no registered workspace, placeholder home cwd. Minted
    // over the admin channel, so the mirror learns its origin before the window
    // can load it. The cold start always wins the suffixless label (no window
    // exists yet), so existing single-window installations are unaffected.
    let home = canonical_home_dir().map_err(&fail)?;
    let landing_label = next_free_landing_label(app.handle());
    let landing = tauri::async_runtime::block_on(client.create_owner(
        &landing_label,
        Some(&home.to_string_lossy()),
        0,
        &host_origin,
        None,
    ))
    .map_err(&fail)?;
    open_native_landing_window(
        app.handle(),
        &host_origin,
        &landing_label,
        Arc::clone(&mirror),
        window_owner::OwnerId::from_string(landing.owner.clone()),
        &landing.capability,
    )
    .map_err(&fail)?;
    if let Some(ledger) = app.try_state::<WindowLedger>() {
        ledger.record(LedgerRow::landing(
            &landing_label,
            &landing.owner,
            &home.to_string_lossy(),
        ));
    }
    app.manage(DaemonLink::new(
        DaemonSession {
            client,
            host_origin: host_origin.clone(),
            daemon,
        },
        mirror,
        manager,
        data_dir,
        config_dir,
    ));
    // The restart transaction's cross-request state (T9): managed, so two
    // triggers coalesce onto one replacement however many coordinators they
    // build.
    app.manage(Arc::new(daemon_client::restart::RestartGate::default()));
    // Startup default is disabled (the first window is the landing); recompute
    // once now that the window exists. No-op wherever no menu bar is installed.
    update_new_session_menu_state(app.handle());
    log::info!("[picot] landing ready origin={host_origin}");
    Ok(SetupOutcome::Ready)
}

// ─── Auto-updater cores ─────────────────────────────────────────────────────

/// Check GitHub for a newer release. Returns update metadata as JSON, or
/// `Value::Null` when already up to date. Mirrors the shape the old JS
/// `checkForUpdate` returned so the frontend renderer is unchanged.
async fn check_for_update_core(app: &AppHandle) -> Result<Value, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await.map_err(|e| e.to_string())? {
        Some(update) => Ok(serde_json::json!({
            "available": true,
            "version": update.version,
            "currentVersion": update.current_version,
            "date": update.date.map(|d| d.to_string()),
            "notes": update.body.clone().unwrap_or_default(),
        })),
        None => Ok(Value::Null),
    }
}

/// Download + install the available update, streaming progress frames through
/// `progress` (broker → client). Replaces the Tauri `Channel` the JS used.
async fn download_and_install_update_core(
    app: &AppHandle,
    progress: ProgressSink,
) -> Result<Value, String> {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use tauri_plugin_updater::UpdaterExt;

    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = match updater.check().await.map_err(|e| e.to_string())? {
        Some(update) => update,
        None => return Ok(serde_json::json!({ "installed": false, "reason": "no_update" })),
    };
    let version = update.version.clone();

    let downloaded = Arc::new(AtomicU64::new(0));
    let started = Arc::new(AtomicBool::new(false));
    let chunk_sink = progress.clone();
    let dl = downloaded.clone();
    let started_flag = started.clone();
    let finish_sink = progress.clone();

    update
        .download_and_install(
            move |chunk_length, content_length| {
                let total =
                    dl.fetch_add(chunk_length as u64, Ordering::Relaxed) + chunk_length as u64;
                if !started_flag.swap(true, Ordering::Relaxed) {
                    chunk_sink(serde_json::json!({
                        "phase": "started",
                        "contentLength": content_length,
                    }));
                }
                chunk_sink(serde_json::json!({
                    "phase": "progress",
                    "downloaded": total,
                    "contentLength": content_length,
                }));
            },
            move || {
                finish_sink(serde_json::json!({ "phase": "finished" }));
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({ "installed": true, "version": version }))
}

// ─── Host control handler ────────────────────────────────────────────────────

/// Spawn an ephemeral candidate, wait for readiness, and compare-and-commit it.
/// On any failure the candidate process, broker route, registry record, and
/// (for Quick Chat) temp directory are cleaned up before returning the error.
#[allow(clippy::too_many_arguments)]
/// Adopt a pre-warmed standby pi for an ephemeral chat. The standby is
/// already spawned and healthy; we only register it with the broker and
/// registry, then apply the startup profile. This is the fast path
/// (~milliseconds) compared to spawn + health-wait (~seconds).
/// Build the RPC commands that mirror the active workspace session's model
/// and thinking level into a freshly spawned Side Chat. Returns None when the
/// profile is missing or incomplete so the caller can skip the round-trip.
#[allow(dead_code)] // pending native ephemeral work package
fn side_chat_startup_rpc_commands(profile: &Value) -> Vec<Value> {
    let mut commands = Vec::new();
    let provider = profile.get("provider").and_then(Value::as_str);
    let model_id = profile.get("modelId").and_then(Value::as_str);
    let thinking = profile.get("thinkingLevel").and_then(Value::as_str);
    if let (Some(provider), Some(model_id)) = (provider, model_id) {
        if !provider.is_empty() && !model_id.is_empty() {
            commands.push(serde_json::json!({
                "type": "set_model",
                "provider": provider,
                "modelId": model_id,
            }));
            if let Some(level) = thinking {
                if !level.is_empty() && level != "off" {
                    commands.push(serde_json::json!({
                        "type": "set_thinking_level",
                        "level": level,
                    }));
                }
            }
        }
    }
    commands
}

/// Remove an uncommitted/failed candidate: generation-checked registry cleanup
/// plus exact Quick Chat temp directory deletion. Never touches another record.
/// Generation-checked close of a live ephemeral instance: mark closing, kill the
/// exact (port, pid), unregister the route, delete an owned temp directory, and
/// remove the record only when identity still matches.
/// A pending window-close transaction: the request id issued to the frontend
/// coordinator and whether its final approval has been received.
#[derive(Clone)]
struct PendingClose {
    request_id: String,
    approved: bool,
}

static CLOSE_REQUEST_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn close_approvals(
) -> &'static std::sync::Mutex<std::collections::HashMap<window_owner::OwnerId, PendingClose>> {
    static CLOSE_APPROVALS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<window_owner::OwnerId, PendingClose>>,
    > = std::sync::OnceLock::new();
    CLOSE_APPROVALS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Intercept the native close. The first request is prevented and one
/// owner-targeted close_request is issued; a matching window_close_approve sets
/// the one-shot approval consumed by the close triggered from the host. A
/// WebView that cannot run its confirmation flow falls back to a native warning.
///
/// The confirmation itself belongs to the WebView's close coordinator (R5): its
/// per-participant risk/settlement dialogs are the product's answer to "discard
/// unsaved work?". The window is loaded from the **daemon's** host origin and
/// holds its own data-plane connection, so the app asks through the admin
/// channel and the daemon pushes the frame into the WebView:
///
/// ```text
/// CloseRequested → admin `owner.close_request` → data-plane `window_close_request`
///   → coordinator settles → data-plane `window_close_approve` | `window_close_cancel`
///     → app-surface `close_window` | `cancel_window_close` (this process)
///   ← `{requestId, delivered, outcome}`
/// ```
///
/// When that round trip cannot produce a coordinator answer — no WebView holds
/// the owner (`undeliverable`), the coordinator never answers (`timeout`), or
/// the admin channel itself is down — the native dialog below still confirms the
/// close. The degradation path is deliberate: it is never *less* safe, only less
/// informative.
fn handle_close_requested(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    // The label↔owner map is the app's, fed by the daemon's mirror (plan D4).
    // Using the mirror — not the in-process registry — is what keeps this path
    // working in both modes; with no attachment the lookup is empty and the
    // close is simply allowed (fail-open only for *confirmation*, never for
    // authorization: the window's own navigation authority is the mirror too).
    let app = window.app_handle();
    let Some(mirror) = DaemonLink::mirror_for(app) else {
        return;
    };
    let Some(owner) = mirror
        .owner_for_label(window.label())
        .map(window_owner::OwnerId::from_string)
    else {
        return;
    };

    let approvals = close_approvals();
    let mut guard = approvals.lock().unwrap();
    if let Some(pending) = guard.get(&owner) {
        if pending.approved {
            // Consumed: allow this close.
            guard.remove(&owner);
            return;
        }
        // Still pending: prevent and let the existing dialog keep focus.
        api.prevent_close();
        return;
    }
    api.prevent_close();
    let request_id = format!(
        "close-{}",
        CLOSE_REQUEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    guard.insert(
        owner.clone(),
        PendingClose {
            request_id: request_id.clone(),
            approved: false,
        },
    );
    drop(guard);

    // The WebView is the confirmation authority, and only the daemon can reach
    // it (R5). A missing link means the app has no channel at all: the native
    // dialog is the only confirmation left.
    let Some(link) = app.try_state::<DaemonLink>() else {
        show_close_fallback_dialog(app, owner, request_id);
        return;
    };
    let client = link.client();
    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        let delivery = client
            .request_owner_close(owner.as_str(), &request_id)
            .await;
        if !close_request_needs_native_fallback(delivery) {
            // The coordinator owns the outcome: approving (the app-surface op
            // already closed the window) or cancelling (the pending close was
            // dropped) — either way this request is settled.
            return;
        }
        // Only this request may open the dialog: a settlement can land while we
        // waited, and a native warning over a window that is already closing
        // would be worse than none.
        if !close_request_still_pending(&owner, &request_id) {
            return;
        }
        show_close_fallback_dialog(&app_for_task, owner, request_id);
    });
}

/// The one decision the close delegate makes from an `owner.close_request`
/// result: does the native dialog still have to run?
///
/// Everything that is not a WebView settlement is a fallback — including a
/// failed op (a dead admin channel, a malformed reply). The safe direction is
/// always "ask the user anyway".
fn close_request_needs_native_fallback(
    delivery: Result<picot_host::daemon::owner_close::OwnerCloseDelivery, String>,
) -> bool {
    match delivery {
        Ok(delivery) => delivery.requires_native_fallback(),
        Err(_) => true,
    }
}

/// True while this exact request is the owner's pending close and nobody has
/// approved it yet.
fn close_request_still_pending(owner: &window_owner::OwnerId, request_id: &str) -> bool {
    close_approvals()
        .lock()
        .unwrap()
        .get(owner)
        .is_some_and(|pending| pending.request_id == request_id && !pending.approved)
}

/// The window label the app's mirror has for one owner (the daemon owns the
/// registry and the app's copy is the daemon-fed mirror, plan D4).
fn mirror_label_for_owner(app: &AppHandle, owner: &window_owner::OwnerId) -> Option<String> {
    DaemonLink::mirror_for(app).and_then(|mirror| mirror.label_for_owner(owner.as_str()))
}

/// The degradation path: the WebView never owned the close question, so the
/// native warning asks it instead. Confirm closes (after settlement of
/// host-owned state); cancel drops the pending request.
fn show_close_fallback_dialog(app: &AppHandle, owner: window_owner::OwnerId, request_id: String) {
    let app = app.clone();
    // The dialog API is main-thread, and the fallback also has to run when it is
    // reached from a task on the async runtime (a settlement timeout), so the
    // dialog is built and shown inside the main-thread hop.
    let dispatched = app.clone().run_on_main_thread(move || {
        let dialogs = app.dialog();
        let app_for_dialog = app.clone();
        let owner_for_dialog = owner.clone();
        dialogs
            .message("Closing this window will discard unsaved changes and any temporary chats.")
            .title("Close window")
            .buttons(tauri_plugin_dialog::MessageDialogButtons::OkCancelCustom(
                "Close anyway".to_string(),
                "Cancel".to_string(),
            ))
            .show(move |result| {
                let mut guard = close_approvals().lock().unwrap();
                // A settlement may have landed while the dialog was up; only
                // the request that opened it may be settled here.
                match guard.get(&owner_for_dialog) {
                    Some(pending) if pending.request_id == request_id => {}
                    _ => return,
                }
                if result {
                    if let Some(pending) = guard.get_mut(&owner_for_dialog) {
                        pending.approved = true;
                    }
                    let label = mirror_label_for_owner(&app_for_dialog, &owner_for_dialog);
                    drop(guard);
                    if let Some(label) = label {
                        let _ = app_for_dialog.get_webview_window(&label).map(|w| w.close());
                    }
                } else {
                    guard.remove(&owner_for_dialog);
                }
            });
    });
    if let Err(error) = dispatched {
        log::warn!("[picot] no native close warning could be presented: {error}");
    }
}

/// Final idempotent cleanup when a native window is destroyed: drop the
/// app-local pane state for the label, then ask the daemon to **detach** the
/// window's owner (plan D6/T7).
///
/// Detach is not a teardown of the Pi runtimes: the daemon revokes the owner's
/// capability (freeing the label), stops its owner-scoped ephemeral processes
/// for real, and releases its terminals/git/skills — while every resident
/// workspace/session runtime keeps running, so a reopened window (or a relaunch)
/// reattaches to the same Pi process.
///
/// The op travels the admin channel in **both** modes: closed windows are not
/// an in-process shortcut (plan D2), and the app must not reach into host state
/// it no longer owns (T7 removed those managed copies).
fn handle_window_destroyed(window: &tauri::Window) {
    let label = window.label();
    // Browser panes die with their window; drop the manager's stale entries
    // so labels can be reused without collisions. App-local by construction
    // (D1/Step-3 exceptions).
    if let Some(panes) = window.try_state::<browser_pane::BrowserPaneState>() {
        panes.0.destroy_all_for_window(label);
    }
    // The app's own window record dies with the window (T9): a label the app no
    // longer has must not be migrated by a later restart.
    if let Some(ledger) = window.try_state::<WindowLedger>() {
        ledger.forget(label);
    }
    // The focus stamp dies with the window too: a rebuilt label must not
    // inherit the destroyed window's recency (multi-window §7).
    if let Some(focus_times) = window.try_state::<FocusTimes>() {
        focus_times.forget(label);
    }
    let Some(link) = window.app_handle().try_state::<DaemonLink>() else {
        return;
    };
    // The restart transaction destroys a window only to re-create it at the new
    // origin with a fresh capability (T9 step 5). Detaching here would revoke
    // the owner it just minted for the same label, so a migration in progress
    // owns the label for the duration of the transaction.
    if link.is_migrating(label) {
        log::info!("[picot] window {label} destroyed by the restart migration; not detaching");
        return;
    }
    // The daemon owns the label↔owner registry; the app's copy is the
    // daemon-fed mirror (D4), so a label with no owner is simply nothing to do.
    let Some(owner) = link.mirror.owner_for_label(label) else {
        return;
    };
    // The pending-close map is app-local (D4): drop it here, or a destroyed
    // window's entry outlives its window forever.
    let owner_id = window_owner::OwnerId::from_string(owner.clone());
    close_approvals().lock().unwrap().remove(&owner_id);
    let client = link.client();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = client
            .request("owner.detach", serde_json::json!({ "owner": owner }))
            .await
        {
            // A dropped admin channel (daemon gone) is the one case the
            // dead-attachment cleanup already covers; anything else is worth
            // a log line because it would leak a capability until restart.
            log::warn!("[picot] window detach failed for owner {owner}: {error}");
        }
    });
}

// ─── Restart transaction (plan Task 9 / D10) ─────────────────────────────────

/// How long a migrated label's window may take to finish destroying before the
/// transaction gives up on it (Tauri refuses a duplicate label).
const MIGRATION_DESTROY_DEADLINE: Duration = Duration::from_secs(5);
/// Poll interval while waiting for a destroyed window's label to be free.
const MIGRATION_POLL: Duration = Duration::from_millis(20);

/// The app-side restart seam: the admin channel, one fresh owner per captured
/// window, and the window migration itself (plan Task 9 steps 4-6).
struct AppRestartLink {
    app: AppHandle,
    /// Canonical home: the landing owner's cwd, at boot and after every
    /// restart (the same value `setup_daemon_runtime` uses).
    home: String,
}

impl AppRestartLink {
    fn new(app: AppHandle) -> Result<Self, String> {
        Ok(Self {
            app,
            home: canonical_home_dir()?.to_string_lossy().to_string(),
        })
    }

    /// The managed link state, or the documented "not booted" error.
    fn link(&self) -> Result<tauri::State<'_, DaemonLink>, String> {
        self.app
            .try_state::<DaemonLink>()
            .ok_or_else(|| "Picot's daemon link is not available".to_string())
    }

    /// Mint one owner over the (already attached) new admin channel.
    async fn mint(
        &self,
        label: &str,
        cwd: Option<&str>,
        workspace_id: Option<&str>,
        host_origin: &str,
    ) -> Result<(String, String), String> {
        let created = self
            .link()?
            .client()
            .create_owner(label, cwd, 0, host_origin, workspace_id)
            .await?;
        Ok((created.owner, created.capability))
    }

    /// Destroy and re-create every migrated window at its new URL, with the new
    /// capability injected as the initialization script.
    ///
    /// `destroy()` (not `close()`) is deliberate: the app's own close-request
    /// handler must not be able to veto a migration it knows nothing about.
    async fn recreate_windows(&self, rows: &[ViewMigration]) -> Result<(), String> {
        let mirror = DaemonLink::mirror_for(&self.app)
            .ok_or_else(|| "Picot's navigation mirror is not available".to_string())?;
        let host_origin = self.link()?.host_origin();
        for row in rows {
            if let Some(window) = self.app.get_webview_window(&row.label) {
                // `destroy()` fires no `CloseRequested`, and a migration is not
                // a `RunEvent::Exit`: the window-state plugin never records
                // what this window actually looks like, so the re-created
                // window restored whatever a session of `Resized`/`Moved`
                // events had left in the plugin's cache — an oversized window
                // that `open_native_window` then clamps to the work area (the
                // "maximized after restart" window). Saving here overwrites
                // that cache, and the file, from the live window.
                //
                // shortcut: saving a maximized window stores the maximized
                // geometry (the plugin skips SIZE while maximized), so a
                // restart comes back maximized rather than at the size the
                // window had before it was maximized.
                if let Err(error) = self.app.save_window_state(window_state_flags()) {
                    log::warn!(
                        "[picot] cannot save the window geometry of {} before migrating it: \
                         {error}",
                        row.label
                    );
                }
                window.destroy().map_err(|error| {
                    format!("cannot close window {} for migration: {error}", row.label)
                })?;
            }
            let deadline = std::time::Instant::now() + MIGRATION_DESTROY_DEADLINE;
            while self.app.get_webview_window(&row.label).is_some() {
                if std::time::Instant::now() >= deadline {
                    return Err(format!(
                        "window {} was still alive {}s after destroy",
                        row.label,
                        MIGRATION_DESTROY_DEADLINE.as_secs()
                    ));
                }
                tokio::time::sleep(MIGRATION_POLL).await;
            }
            open_native_window(
                &self.app,
                &row.label,
                &row.url,
                Arc::clone(&mirror),
                window_owner::OwnerId::from_string(row.owner.clone()),
                &row.capability,
            )
            .map_err(|error| format!("cannot re-create {} at {}: {error}", row.label, row.url))?;
            if let Some(ledger) = self.app.try_state::<WindowLedger>() {
                ledger.record(self.ledger_row_for(row, &host_origin));
            }
        }
        Ok(())
    }

    /// The ledger row the re-created window gets: the new owner, the binding it
    /// was minted with, and the route it actually loaded.
    fn ledger_row_for(&self, row: &ViewMigration, host_origin: &str) -> LedgerRow {
        let previous = self
            .app
            .try_state::<WindowLedger>()
            .and_then(|ledger| ledger.row_for_label(&row.label));
        let home = previous
            .and_then(|row| row.owner_home)
            .unwrap_or_else(|| self.home.clone());
        match row.workspace_id.as_deref() {
            Some(workspace_id) => LedgerRow::workspace(
                &row.label,
                &row.owner,
                workspace_id,
                route_for_url(&row.url, host_origin),
            ),
            None => LedgerRow::landing(&row.label, &row.owner, &home),
        }
    }

    /// Re-open the first migrated view when a failed migration left the app
    /// with no window at all: the exit path holds a request that arrives during
    /// the migration ([`classify_exit_request`]), so a window-less app would
    /// never get another one. Best effort — a failure here is logged, not
    /// fatal, because the transaction still has to report its own error.
    fn redeem_windowless(&self, rows: &[ViewMigration]) {
        if !self.app.webview_windows().is_empty() {
            return;
        }
        let Some(row) = rows.first() else {
            return;
        };
        let Some(mirror) = DaemonLink::mirror_for(&self.app) else {
            return;
        };
        match open_native_window(
            &self.app,
            &row.label,
            &row.url,
            mirror,
            window_owner::OwnerId::from_string(row.owner.clone()),
            &row.capability,
        ) {
            Ok(_) => log::warn!(
                "[picot] the restart migration left no window; re-opened {}",
                row.label
            ),
            Err(error) => log::error!(
                "[picot] the restart migration left no window and {} could not be re-opened: \
                 {error}",
                row.label
            ),
        }
    }
}

#[async_trait::async_trait]
impl RestartLink for AppRestartLink {
    async fn attach(&self, target: &RestartTarget) -> Result<u64, String> {
        let mirror = DaemonLink::mirror_for(&self.app)
            .ok_or_else(|| "Picot's navigation mirror is not available".to_string())?;
        // The superseded daemon is gone (confirmed by the coordinator), so its
        // channel is done: closing it here — *before* the new snapshot — is what
        // keeps a late fail-closed `clear()` from wiping the fresh mirror state.
        self.link()?.client().disconnect();
        let executor: Arc<dyn SurfaceExecutor> = Arc::new(ForwardedSurfaceOps::new(Arc::new(
            tauri_app_surface::TauriAppSurface::new(self.app.clone(), Arc::clone(&mirror)),
        )));
        let identity = ManagerIdentity {
            pid: Some(std::process::id()),
            app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            build_id: Some(picot_host::BUILD_ID.to_string()),
        };
        let client = AdminClient::connect(
            &target.admin_origin,
            &target.admin_token,
            &identity,
            executor,
            Arc::clone(&mirror),
        )
        .await?;
        let epoch = client.epoch().unwrap_or_default();
        let superseded = self.link()?.replace_session(DaemonSession {
            client,
            host_origin: target.host_origin.clone(),
            daemon: Some(target.identity()),
        });
        superseded.client.disconnect();
        Ok(epoch)
    }

    async fn rebuild_owner(
        &self,
        view: &CapturedView,
        target: &RestartTarget,
    ) -> Result<OwnerBinding, String> {
        match view.workspace_id.as_deref() {
            // A workspace window: the app knows only the workspace id, so the
            // daemon resolves the canonical root from its own registry (T9
            // amendment).
            Some(workspace_id) => {
                match self
                    .mint(&view.label, None, Some(workspace_id), &target.host_origin)
                    .await
                {
                    Ok((owner, capability)) => Ok(OwnerBinding {
                        owner,
                        capability,
                        degraded: false,
                    }),
                    Err(error) => {
                        // The view cannot be rebuilt for its captured binding
                        // (the workspace left the registry, or the daemon
                        // refused). Landing it on the new origin root is better
                        // than leaving a window that cannot load at all.
                        log::warn!(
                            "[picot] cannot rebuild the owner of {} (workspace {workspace_id}): \
                             {error}; migrating it as a landing window instead",
                            view.label
                        );
                        let home = self.home.clone();
                        let (owner, capability) = self
                            .mint(&view.label, Some(&home), None, &target.host_origin)
                            .await?;
                        Ok(OwnerBinding {
                            owner,
                            capability,
                            degraded: true,
                        })
                    }
                }
            }
            // A landing (or workspaceless) window: the app minted its owner and
            // knows the home it used.
            None => {
                let cwd = view.owner_home.clone().unwrap_or_else(|| self.home.clone());
                let (owner, capability) = self
                    .mint(&view.label, Some(&cwd), None, &target.host_origin)
                    .await?;
                Ok(OwnerBinding {
                    owner,
                    capability,
                    degraded: false,
                })
            }
        }
    }

    async fn migrate_views(&self, rows: &[ViewMigration]) -> Result<(), String> {
        let labels: Vec<String> = rows.iter().map(|row| row.label.clone()).collect();
        let link = self.link()?;
        // The destroy path must not detach the owner this transaction is about
        // to re-mint for the same label. The guard clears the marks on *every*
        // exit from this scope — the `?` below, a panic, or this future being
        // dropped mid-await; a leaked mark would hold every later exit request
        // forever (A4 follow-up #1).
        let migration = link.begin_migration(&labels);
        let result = self.recreate_windows(rows).await;
        drop(migration);
        if result.is_err() {
            // A failed migration must never leave the app with no window at
            // all: the exit path holds a request that arrives during the
            // migration (see `classify_exit_request`), and a window-less app
            // would never get another one. Best effort — a failure here is
            // logged, not fatal, because the transaction still has to report.
            self.redeem_windowless(rows);
        }
        result
    }
}

/// The captured pre-state (plan Task 9 step 1): every window the app created
/// that still exists and still has an owner, with the route its **live**
/// document is at (SPA `pushState` session switches are only visible there).
fn capture_open_views(app: &AppHandle) -> Vec<CapturedView> {
    let Some(link) = app.try_state::<DaemonLink>() else {
        return Vec::new();
    };
    let Some(ledger) = app.try_state::<WindowLedger>() else {
        return Vec::new();
    };
    let host_origin = link.host_origin();
    let mut views = Vec::new();
    for row in ledger.rows() {
        let Some(window) = app.get_webview_window(&row.label) else {
            continue;
        };
        let Some(owner) = link.mirror.owner_for_label(&row.label) else {
            continue;
        };
        let live_route = window
            .url()
            .ok()
            .and_then(|url| route_for_url(url.as_str(), &host_origin));
        let workspace_id = link
            .mirror
            .owner_row_for_label(&row.label)
            .and_then(|owner_row| owner_row.workspace_id)
            .or_else(|| row.workspace_id.clone());
        views.push(CapturedView {
            label: row.label.clone(),
            owner,
            owner_home: row.owner_home.clone(),
            workspace_id,
            route: live_route.or_else(|| row.route.clone()),
        });
    }
    views
}

/// Run the restart transaction against the app's current daemon (plan D10).
///
/// The gate is app state, so two triggers (a double click, a command racing
/// another) produce **one** replacement; each request still builds the
/// coordinator from the daemon facts of *its* moment.
pub async fn run_restart(app: &AppHandle) -> Result<RestartReport, String> {
    let link = app
        .try_state::<DaemonLink>()
        .ok_or_else(|| "Picot's daemon link is not available".to_string())?;
    let gate = app
        .try_state::<Arc<daemon_client::restart::RestartGate>>()
        .map(|state| Arc::clone(state.inner()))
        .ok_or_else(|| "Picot's restart gate is not available".to_string())?;
    let views = capture_open_views(app);
    let config = RestartConfig {
        identity: link.daemon(),
        child: link
            .manager
            .as_ref()
            .and_then(|manager| manager.take_child()),
        instance_path: link.instance_path(),
        log_path: link.log_path(),
        config_dir: link.config_dir.clone(),
        confirm_deadline: daemon_client::restart::RESTART_CONFIRM_DEADLINE,
    };
    let app_link: Arc<dyn RestartLink> = Arc::new(AppRestartLink::new(app.clone())?);
    // T12: the daemon is always its own process, so a restart is always the real
    // process replacement. It must never be attempted against a daemon whose
    // identity this app did not verify: without one there is nothing to confirm
    // is gone before the replacement starts (plan D12).
    if config.identity.is_none() {
        return Err("the running daemon has no verified process identity".to_string());
    }
    let manager = link
        .manager
        .clone()
        .ok_or_else(|| "the daemon manager is not available".to_string())?;
    let coordinator = RestartCoordinator::new(
        gate,
        config,
        Arc::new(quit_policy::AdminStopRequester::new(link.client())),
        ExitWaiter::new(),
        Arc::new(ManagerSpawner::new(manager)),
        app_link,
    );
    let outcome = coordinator.run(views).await;
    outcome
        .report()
        .cloned()
        .ok_or_else(|| outcome.failure_message().unwrap_or_default())
}

// ─── Main ─────────────────────────────────────────────────────────────────────

// Native menus: macOS shows them in the system menu bar (same as upstream);
// Windows shows the same builder's menu as an in-window menu bar — the app
// has no in-window File menu of its own, so without it New Window has no
// entry point at all. The Edit submenu's key equivalents are load-bearing
// for WKWebView text input on macOS: without a native menu the responder
// chain eats key auto-repeat events, so holding a key in the terminal
// (xterm.js) types exactly one character. Linux stays menu-less exactly as
// upstream does.
const MENU_NEW_SESSION_ID: &str = "picot-new-session";
const MENU_NEW_WINDOW_ID: &str = "picot-new-window";
const MENU_ADD_PROJECT_ID: &str = "picot-add-project";

/// The landing window family's base label (multi-window §6.1): the app is the
/// only creator of landing windows, so it is also the label allocator.
const LANDING_WINDOW_LABEL: &str = "native-landing";

/// The `index`-th label of the `native-landing` family: 1 is the suffixless
/// cold-start name, 2 and up carry the `-2`/`-3`… suffix. Numeric, so retry
/// arithmetic advances even while lexicographic order would not
/// (`-10` sorts before `-2`).
fn landing_label_at(index: u32) -> String {
    if index == 1 {
        LANDING_WINDOW_LABEL.to_string()
    } else {
        format!("{LANDING_WINDOW_LABEL}-{index}")
    }
}

/// The next free label in the `native-landing` family: the first window keeps
/// the suffixless name, later ones take the first free `-2`, `-3`, … suffix.
/// Nothing is reserved — a closed window's label is immediately reusable, and
/// existing single-window installations keep the suffixless name everywhere
/// (window-state geometry files, ledger rows, and mirror rows all key on the
/// label, so no migration is needed).
fn next_landing_label(is_taken: impl Fn(&str) -> bool) -> String {
    (1..)
        .map(landing_label_at)
        .find(|label| !is_taken(label))
        .expect("the landing label suffix space is never exhausted")
}

/// `next_landing_label` over the app's live windows.
fn next_free_landing_label(app: &AppHandle) -> String {
    next_landing_label(|label| app.get_webview_window(label).is_some())
}

/// The first landing label index no live window holds (multi-window §6.1).
fn next_landing_index(app: &AppHandle) -> u32 {
    (1..)
        .find(|index| app.get_webview_window(&landing_label_at(*index)).is_none())
        .expect("the landing label suffix space is never exhausted")
}

/// File → New Session is enabled exactly when the focused window's owner is
/// bound to a workspace, read from the owner registry — never from the label.
/// `None` means no focused owner-bound window (menu disabled). Pure for unit
/// testing; the truth table is asserted in startup_tests.
fn new_session_menu_enabled(focused_owner_has_workspace: Option<bool>) -> bool {
    focused_owner_has_workspace.unwrap_or(false)
}

/// The focused webview window's owner workspace binding, if any window is
/// focused and owns a registry record.
fn focused_owner_has_workspace(app: &AppHandle) -> Option<bool> {
    let focused = app
        .webview_windows()
        .into_values()
        .find(|window| window.is_focused().unwrap_or(false))?;
    // The mirror carries the binding the daemon holds (plan D4): the same answer
    // in both modes, and no in-process registry read.
    let mirror = DaemonLink::mirror_for(app)?;
    let owner = mirror.owner_for_label(focused.label())?;
    mirror.owner_has_workspace(&owner)
}

/// Find a menu item by id, descending into submenus. Tauri's `Menu::get`
/// only matches top-level entries, and the File items live one level down —
/// the New Session enable-state recompute must walk the submenus itself.
fn find_menu_item(
    items: Vec<tauri::menu::MenuItemKind<tauri::Wry>>,
    id: &str,
) -> Option<tauri::menu::MenuItem<tauri::Wry>> {
    for item in items {
        match item {
            tauri::menu::MenuItemKind::MenuItem(mi) => {
                if mi.id().0 == id {
                    return Some(mi);
                }
            }
            tauri::menu::MenuItemKind::Submenu(sub) => {
                if let Ok(sub_items) = sub.items() {
                    if let Some(found) = find_menu_item(sub_items, id) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Recompute the New Session menu item from the focused owner's binding.
/// Runs on window focus events and after workspace transition commits (a
/// transition inside the landing window changes binding without any focus
/// change). No-op wherever no menu bar is installed.
fn update_new_session_menu_state(app: &AppHandle) {
    let Some(menu) = app.menu() else {
        return;
    };
    let bound = focused_owner_has_workspace(app);
    let enabled = new_session_menu_enabled(bound);
    if let Ok(items) = menu.items() {
        if let Some(item) = find_menu_item(items, MENU_NEW_SESSION_ID) {
            let _ = item.set_enabled(enabled);
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn build_app_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let new_window = MenuItem::with_id(
        app,
        MENU_NEW_WINDOW_ID,
        "New Window",
        // Always enabled: ⌘N is Rust-side (multi-window §5) and needs no
        // focused owner-bound window.
        true,
        Some("CmdOrCtrl+N"),
    )?;
    #[cfg(target_os = "macos")]
    let new_session = MenuItem::with_id(
        app,
        MENU_NEW_SESSION_ID,
        "New Session",
        // Startup default: disabled — the first window is the landing.
        new_session_menu_enabled(None),
        Some("CmdOrCtrl+T"),
    )?;
    #[cfg(not(target_os = "macos"))]
    let new_session = MenuItem::with_id(
        app,
        MENU_NEW_SESSION_ID,
        "New Session",
        // Startup default: disabled — the first window is the landing.
        new_session_menu_enabled(None),
        // No accelerator: the webview handles Ctrl+T itself (native menu
        // accelerators are intercepted before the webview sees keys), so
        // binding it here would double-dispatch new session.
        None::<&str>,
    )?;
    let add_project = MenuItem::with_id(
        app,
        MENU_ADD_PROJECT_ID,
        "Add a Project",
        true,
        // Deliberately no accelerator.
        None::<&str>,
    )?;
    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &new_window,
            &new_session,
            &add_project,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    #[cfg(target_os = "macos")]
    let help = Submenu::with_items(app, "Help", true, &[])?;
    #[cfg(not(target_os = "macos"))]
    let help = Submenu::with_items(
        app,
        "Help",
        true,
        // About works on every desktop platform.
        &[&PredefinedMenuItem::about(app, None, None)?],
    )?;
    #[cfg(target_os = "macos")]
    let app_menu = Submenu::with_items(
        app,
        app.package_info().name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(app, None, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;
    #[cfg(target_os = "macos")]
    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[&PredefinedMenuItem::fullscreen(app, None)?],
    )?;
    #[cfg(target_os = "macos")]
    return Menu::with_items(app, &[&app_menu, &file, &edit, &view, &window, &help]);
    #[cfg(not(target_os = "macos"))]
    return Menu::with_items(app, &[&file, &edit, &window, &help]);
}

// With a native menu bar installed, macOS wires WKWebView's text-input
// context fully — including the "Press and Hold" accent picker, which
// swallows key auto-repeat and pops the diacritic popover (hold "u" → ü…).
// Terminal-style repeat requires the picker off; the flag lives in this
// app's own defaults domain, so the change is scoped to Picot only. Must run
// before the first webview creates its NSTextInputContext.
#[cfg(target_os = "macos")]
fn disable_press_and_hold_accents() {
    use objc2_foundation::{NSString, NSUserDefaults};
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str("ApplePressAndHoldEnabled");
    defaults.setBool_forKey(false, &key);
}

/// A boot failure parked by the `setup` hook, for the `RunEvent::Ready` arm to
/// present. `setup` runs on the main thread inside the run loop, so it can only
/// record the failure; the alert is shown once the main thread is back in its
/// run loop (`daemon_client::startup_failure`).
static STARTUP_FAILURE: Mutex<Option<StartupFailure>> = Mutex::new(None);

fn record_startup_failure(failure: StartupFailure) {
    *STARTUP_FAILURE.lock().expect("startup failure slot") = Some(failure);
}

fn take_startup_failure() -> Option<StartupFailure> {
    STARTUP_FAILURE.lock().expect("startup failure slot").take()
}

/// A second-instance block parked by the `setup` hook, for the `RunEvent::Ready`
/// arm to present. The same reason `STARTUP_FAILURE` is parked: `setup` runs on
/// the main thread, and the dialog plugin draws by posting to that thread — so
/// the hook can only record the block, never wait on the dialog
/// (`daemon_client::present_blocked_by_other_app`).
static BLOCKED_BY_OTHER_APP: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn record_blocked_by_other_app() {
    BLOCKED_BY_OTHER_APP.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Consume the parked block: `true` exactly once, on the `Ready` arm that
/// presents it.
fn take_blocked_by_other_app() -> bool {
    BLOCKED_BY_OTHER_APP.swap(false, std::sync::atomic::Ordering::SeqCst)
}

fn main() {
    // Host tasks spawned through `picot_host::runtime::spawn` must land on the
    // app's tokio runtime (the one `tauri::async_runtime` owns): the pre-split
    // code called `tauri::async_runtime::spawn` for exactly those tasks.
    picot_host::runtime::install(tauri::async_runtime::handle().inner().clone());

    // Sync the user's login-shell environment before anything else.
    // macOS GUI apps (launched from Finder/Dock) inherit neither PATH nor
    // provider API keys exported by shell startup files. `fix_all_vars()`
    // makes the bundled pi process see the same provider configuration as a
    // normal terminal session.
    if let Err(err) = fix_path_env::fix_all_vars() {
        eprintln!("[picot] failed to sync PATH and provider environment from login shell: {err}");
    }

    // Runtimes left behind by a Picot that was killed outright: no teardown of
    // ours ran for those, so this is the only chance to collect them.
    let swept = child_supervision::sweep_orphans();
    if swept > 0 {
        log::info!("[picot-native] cleaned up {swept} orphaned pi runtime(s) from a previous run");
    }

    #[cfg(target_os = "macos")]
    disable_press_and_hold_accents();

    let builder = tauri::Builder::default();
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let builder = builder.menu(build_app_menu).on_menu_event(|app, event| {
        match event.id().as_ref() {
            MENU_NEW_WINDOW_ID => {
                // ⌘N opens another landing window Rust-side (multi-window
                // §5): no WebView synthesis, no focused-window requirement.
                if let Err(error) = open_new_landing_window(app) {
                    log::warn!("[picot] cannot open a new window: {error}");
                }
            }
            MENU_NEW_SESSION_ID => {
                // Re-keyed on the owner record: the menu dispatches Cmd+T only
                // when the focused window's owner is bound to a workspace, so
                // a transitioned landing window qualifies despite its label.
                let Some(window) = app
                    .webview_windows()
                    .into_values()
                    .find(|window| window.is_focused().unwrap_or(false))
                else {
                    return;
                };
                let Some(mirror) = DaemonLink::mirror_for(app) else {
                    return;
                };
                let bound = mirror
                    .owner_for_label(window.label())
                    .and_then(|owner| mirror.owner_has_workspace(&owner))
                    .unwrap_or(false);
                if bound {
                    // The webview keydown handler checks metaKey || ctrlKey;
                    // Windows needs Ctrl so the synthesized event matches the
                    // key a user would actually press there.
                    let modifier = if cfg!(target_os = "macos") {
                        "metaKey"
                    } else {
                        "ctrlKey"
                    };
                    let _ = window.eval(format!(
                        "document.dispatchEvent(new KeyboardEvent('keydown', {{key: 't', {modifier}: true, bubbles: true, cancelable: true}}))",
                    ));
                }
            }
            MENU_ADD_PROJECT_ID => {
                // Available no matter which window kind is focused; the web
                // side runs the real picker/register flow.
                if let Some(window) = app
                    .webview_windows()
                    .into_values()
                    .find(|window| window.is_focused().unwrap_or(false))
                {
                    let _ = window.eval("document.getElementById(\"add-project-btn\")?.click();");
                }
            }
            _ => {}
        }
    });
    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Window geometry is tracked on resize and saved on close/app exit, then
        // restored at window creation (see open_native_window).
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                .build(),
        )
        .plugin(tauri_plugin_process::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .level_for("tokio_tungstenite", log::LevelFilter::Warn)
                .level_for("tungstenite", log::LevelFilter::Warn)
                .level_for("tokio_util", log::LevelFilter::Warn)
                .level_for("hyper", log::LevelFilter::Warn)
                .build(),
        )
        .setup(|app| {
            let static_dir = find_static_dir(app);
            // Native HostServer and stdio Pi runtime are the only startup path.
            // A boot failure is *not* returned to Tauri: `setup` runs on the
            // main thread inside the run loop's `Ready` callback, so an `Err`
            // there becomes `panicked at 'Failed to setup app'` — an abort on
            // the main thread of a bundled app, i.e. exit 134 plus a system
            // crash report. The failure is parked instead and presented by the
            // `Ready` arm below (`daemon_client::startup_failure` names the
            // reason why the alert can only be drawn from there).
            //
            // The second-instance block is parked for the same reason: the
            // dialog is posted to this thread, so joining it here would hang.
            match setup_daemon_runtime(app, static_dir) {
                Ok(SetupOutcome::Ready) => {}
                Ok(SetupOutcome::BlockedByOtherApp) => record_blocked_by_other_app(),
                Err(failure) => record_startup_failure(failure),
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                handle_close_requested(window, api);
            }
            tauri::WindowEvent::Destroyed => {
                handle_window_destroyed(window);
                if let Some(state) = window.try_state::<host_client::HostClientState>() {
                    if let Ok(client) = &state.0 {
                        let client = client.clone();
                        let label = window.label().to_owned();
                        tauri::async_runtime::spawn(async move {
                            client.close_window(&label).await;
                        });
                    }
                }
            }
            tauri::WindowEvent::Focused(true) => {
                // The §7 focus stamp: which window was looked at last decides
                // which one a clicked notification routes to.
                if let Some(focus_times) = window.try_state::<FocusTimes>() {
                    focus_times.record(window.label());
                }
                // Menu state tracks the focused owner's binding; a landing
                // window's focus also evaluates (its binding can change).
                update_new_session_menu_state(window.app_handle());
            }
            _ => {}
        })
        // The main UI talks to HostServer exclusively over its v2 WebSocket.
        // No legacy broker or Tauri IPC control path remains in production.
        // The app's whole Tauri command surface (plan D8/T10): the three daemon
        // commands. Each validates the Tauri-injected calling window against the
        // window ledger + the daemon-fed navigation mirror
        // (`commands::validate_caller`) and each has an explicit permission in
        // `permissions/default.toml` — `bun run check:permissions` re-parses this
        // block textually, so the entries stay bare command names.
        .invoke_handler(tauri::generate_handler![
            daemon_status,
            daemon_set_keep_running,
            daemon_restart,
            daemon_agent_stop,
            relay_status,
            relay_set_enabled,
            relay_offer,
            host_list,
            host_add,
            host_confirm_tofu,
            host_remove,
            host_status,
            relay_device_list,
            relay_revoke_device
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle: &tauri::AppHandle, event| match event {
            tauri::RunEvent::Ready => {
                // A parked exit pre-empts the rest of startup: a boot failure
                // shows its alert and exits 1, a second-instance block shows
                // "already running" and exits 3. `Ready` (unlike the `setup`
                // hook, which runs inside this very callback) is the first
                // moment the main thread can return to its run loop, which is
                // what lets the posted dialog be drawn — see
                // `daemon_client::startup_failure` and
                // `daemon_client::present_blocked_by_other_app`.
                if take_blocked_by_other_app() {
                    daemon_client::present_blocked_by_other_app(app_handle, None);
                    return;
                }
                if let Some(failure) = take_startup_failure() {
                    daemon_client::startup_failure::present_startup_failure(app_handle, failure);
                    return;
                }
                install_termination_handlers(app_handle.clone())
            }
            tauri::RunEvent::ExitRequested { api, code, .. } => {
                // Three different things arrive here (plan T8 step 2 × T9 step 5):
                // an updater relaunch (code `RESTART_EXIT_CODE`), the user quit
                // path, and — critically — the transient "no windows" of a
                // restart migration, which destroys every captured window
                // before re-creating it at the new origin. The last one must
                // **not** run the quit flow: that flow stops (or detaches from)
                // the daemon the user is migrating to and takes the app down.
                let migration_in_flight = app_handle
                    .try_state::<DaemonLink>()
                    .is_some_and(|link| link.is_migrating_any());
                let windows_open = !app_handle.webview_windows().is_empty();
                match classify_exit_request(
                    code,
                    migration_in_flight,
                    windows_open,
                    quit_flow_started(),
                ) {
                    ExitRequestVerdict::RestartRelaunch => {
                        // An app restart (the updater's relaunch surface op) is
                        // not a quit: the daemon is left alone — the replacement
                        // app instance adopts it, or version-stops it — and the
                        // state machine is resolved so the `RunEvent::Exit`
                        // that follows does not stop it either.
                        // `prevent_exit` would be a no-op here anyway (tauri
                        // ignores it for this code).
                        log::info!(
                            "[picot] app restart requested; detaching without stopping the \
                             daemon"
                        );
                        if let Some(link) = app_handle.try_state::<DaemonLink>() {
                            link.client().disconnect();
                        }
                        QUIT_STATE.store(2, Ordering::SeqCst);
                        return;
                    }
                    ExitRequestVerdict::Hold => {
                        // Either a migration is between the destroyed and the
                        // re-created window, or the "no windows left" request is
                        // a stale one whose window already came back: hold the
                        // app open without touching the daemon.
                        log::info!(
                            "[picot] holding the app open: the restart migration owns this exit"
                        );
                        api.prevent_exit();
                        return;
                    }
                    ExitRequestVerdict::Quit => {}
                }
                // The "all windows closed" / explicit `app.exit` path (plan Task 8
                // Step 2): hold the exit while the daemon is stopped, and show the
                // "stopping" splash — which requires the run loop to stay alive,
                // so the transaction itself runs on a detached thread that exits
                // the app when it is done.
                //
                // NOTE: macOS ⌘Q does *not* arrive here. The predefined Quit menu
                // item is a nil-targeted `terminate:` selector (muda), so AppKit
                // terminates the app and tauri only ever sees `RunEvent::Exit`
                // (tao's `applicationWillTerminate` → `LoopDestroyed`). Both paths
                // run the same idempotent flow; only this one can paint.
                if !quit_flow_started() {
                    api.prevent_exit();
                    show_quit_splash(app_handle);
                    spawn_quit_flow(app_handle.clone());
                }
            }
            tauri::RunEvent::Exit => {
                // ⌘Q, and the re-entered exit the splash path triggers. The
                // daemon is stopped and its death *confirmed* by pid + start
                // time, or only detached when `keepRunningAfterQuit` is on
                // (plan D12/T8).
                run_app_exit_flow(app_handle);
            }
            _ => {}
        });
}

/// What an `ExitRequested` event means for this app (plan T8 step 2 × T9 step
/// 5 × D10). Pure so the truth table is testable without a running event loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitRequestVerdict {
    /// The updater's relaunch: leave the daemon alone and resolve the state.
    RestartRelaunch,
    /// Not a quit: the restart migration is between two windows, or the request
    /// predates a window the migration already re-created. Hold, touch nothing.
    Hold,
    /// The user path: run the quit flow (stop/detach the daemon, then exit).
    Quit,
}

/// Decide what an `ExitRequested` means.
///
/// `migration_in_flight` is [`DaemonLink::is_migrating_any`]: the T9 migration
/// destroys each captured window before re-creating it at the new origin, so
/// "all windows closed" is momentarily true **by design**. Treating that as a
/// quit stops the daemon the transaction just started and takes the app down
/// mid-migration (the Restart-≈-quit defect).
///
/// `windows_open` catches the same event arriving after the migration finished
/// (the re-created window already exists): a "no windows left" request with a
/// window open is stale, not a quit. `quit_in_flight` keeps the quit flow's own
/// windows (the stopping splash) from holding that flow open.
fn classify_exit_request(
    code: Option<i32>,
    migration_in_flight: bool,
    windows_open: bool,
    quit_in_flight: bool,
) -> ExitRequestVerdict {
    if code == Some(tauri::RESTART_EXIT_CODE) {
        return ExitRequestVerdict::RestartRelaunch;
    }
    if migration_in_flight {
        return ExitRequestVerdict::Hold;
    }
    if code.is_none() && windows_open && !quit_in_flight {
        return ExitRequestVerdict::Hold;
    }
    ExitRequestVerdict::Quit
}

/// The quit flow's process-global state (plan Task 8 Step 2's "idempotent"
/// requirement): `0` idle, `1` running, `2` resolved. Two triggers arriving at
/// once — ⌘Q racing `RunEvent::ExitRequested`, or a signal racing either — must
/// produce exactly one drain.
static QUIT_STATE: AtomicU8 = AtomicU8::new(0);

/// `true` once the flow has been started (running or resolved): the splash path
/// must not `prevent_exit` a second time, or the app could never leave.
fn quit_flow_started() -> bool {
    QUIT_STATE.load(Ordering::SeqCst) != 0
}

/// How long a signal-triggered teardown may take before the app gives up and
/// exits anyway: the waiter's 10s deadline plus two 2s escalation steps, with
/// slack. The registry is deliberately **not** cleared on this path: whatever
/// the teardown never reached must stay recorded for the next launch's sweep.
const SIGNAL_TEARDOWN_GRACE: Duration = Duration::from_secs(20);

/// Run the quit transaction on a detached thread, then exit. Used by the
/// splash path, which keeps the run loop servicing the splash window while the
/// daemon is being stopped.
fn spawn_quit_flow(app_handle: AppHandle) {
    if QUIT_STATE
        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    std::thread::spawn(move || {
        // The exit was already prevented, so a panic in here would leave Picot
        // showing the splash until it is force-quit: resolve and exit whatever
        // the flow did.
        let ran =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_shutdown(&app_handle)));
        if ran.is_err() {
            log::error!("[picot] the app exit flow panicked; exiting anyway");
        }
        QUIT_STATE.store(2, Ordering::SeqCst);
        app_handle.exit(0);
    });
}

/// The app-exit flow, idempotent across every trigger (plan Task 8 Step 2).
fn run_app_exit_flow(app_handle: &AppHandle) {
    if QUIT_STATE
        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        run_shutdown(app_handle);
        QUIT_STATE.store(2, Ordering::SeqCst);
        return;
    }
    // Already running elsewhere (the splash path) or done: wait for the resolved
    // state instead of draining twice.
    let started = std::time::Instant::now();
    while QUIT_STATE.load(Ordering::SeqCst) != 2 {
        if started.elapsed() > SIGNAL_TEARDOWN_GRACE {
            log::warn!("[picot] app exit flow did not resolve in time; exiting anyway");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One shutdown, whatever the trigger. A boot failure (no `DaemonLink`) still
/// clears the pi runtime registry, exactly as the pre-T8 exit path did.
fn run_shutdown(app_handle: &AppHandle) {
    if let Some(state) = app_handle.try_state::<host_client::HostClientState>() {
        if let Ok(client) = &state.0 {
            tauri::async_runtime::block_on(client.shutdown());
        }
    }
    match app_handle.try_state::<DaemonLink>() {
        Some(link) => log::info!("[picot] app exit resolved: {:?}", link.shutdown()),
        None => child_supervision::clear_registry(),
    }
}

/// The borderless "stopping" splash (plan Task 8 Step 2 implementation-time
/// decision: a prebuilt single-file page, so nothing has to load from the daemon
/// that is being stopped). Best effort: a failure to show it must never block or
/// break the quit flow.
fn show_quit_splash(app_handle: &AppHandle) {
    if let Some(existing) = app_handle.get_webview_window("quit-splash") {
        let _ = existing.show();
        return;
    }
    let built = WebviewWindowBuilder::new(
        app_handle,
        "quit-splash",
        WebviewUrl::App("daemon-stopping.html".into()),
    )
    .title("Picot")
    .inner_size(360.0, 132.0)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .center()
    .build();
    if let Err(error) = built {
        log::warn!("[picot] could not show the stopping splash: {error}");
    }
}

/// Tear runtimes down on the signals that otherwise skip `RunEvent::Exit`
/// entirely: Ctrl-C under `tauri dev`, a logout or shutdown (SIGTERM), and a
/// closing terminal (SIGHUP). SIGKILL cannot be caught — the startup sweep
/// exists for that case (spec §3.5's signal tiers).
///
/// All three signals run the **same** app-exit flow as ⌘Q (plan Task 8 Step 2):
/// `keepRunningAfterQuit=false` stops the daemon and waits for a confirmed
/// death; `true` only detaches.
#[cfg(unix)]
fn install_termination_handlers(app_handle: tauri::AppHandle) {
    use std::sync::atomic::AtomicBool;
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    for signal in [
        tokio::signal::unix::SignalKind::terminate(),
        tokio::signal::unix::SignalKind::interrupt(),
        tokio::signal::unix::SignalKind::hangup(),
    ] {
        let app_handle = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let Ok(mut stream) = tokio::signal::unix::signal(signal) else {
                return;
            };
            if stream.recv().await.is_none() {
                return;
            }
            // Run the exit flow on a detached thread and put a deadline on this
            // handler: the flow waits on a daemon that may ignore SIGTERM, and
            // the default disposition of these signals is already taken over —
            // blocking here would leave Picot answering nothing but SIGKILL.
            // The registry is intentionally NOT cleared on the timeout path: any
            // runtime the teardown never reached must stay recorded so the next
            // launch can sweep it.
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let teardown = app_handle.clone();
            std::thread::spawn(move || {
                run_app_exit_flow(&teardown);
                let _ = done_tx.send(());
            });
            if tokio::time::timeout(SIGNAL_TEARDOWN_GRACE, done_rx)
                .await
                .is_err()
            {
                log::warn!("[picot-native] teardown exceeded its grace period; exiting");
            }
            std::process::exit(0);
        });
    }
}

#[cfg(not(unix))]
fn install_termination_handlers(_app_handle: tauri::AppHandle) {}

#[cfg(test)]
mod exit_request_tests {
    use super::{classify_exit_request, ExitRequestVerdict};
    use tauri::RESTART_EXIT_CODE;

    /// The migration's destroy→re-create gap: no window exists, but the app is
    /// mid-restart. This is exactly the state the Restart-≈-quit defect fell
    /// into: quitting here stops the daemon the transaction just started.
    #[test]
    fn the_migrations_window_gap_is_never_a_quit() {
        assert_eq!(
            classify_exit_request(None, true, false, false),
            ExitRequestVerdict::Hold
        );
        // A migration with a window still open (rows migrate one by one) is the
        // same "not a quit".
        assert_eq!(
            classify_exit_request(None, true, true, false),
            ExitRequestVerdict::Hold
        );
    }

    /// A "no windows" request that arrives after the migration re-created the
    /// window is stale: there is a window now, so nothing is quitting.
    #[test]
    fn a_stale_no_windows_request_is_held_when_a_window_exists() {
        assert_eq!(
            classify_exit_request(None, false, true, false),
            ExitRequestVerdict::Hold
        );
    }

    /// The user path stays the user path: every window closed, nothing else
    /// running, so the quit flow owns it.
    #[test]
    fn the_last_window_closing_is_still_a_quit() {
        assert_eq!(
            classify_exit_request(None, false, false, false),
            ExitRequestVerdict::Quit
        );
    }

    /// An explicit `exit` (code-carrying, e.g. the splash path's `exit(0)`) is
    /// never held — not even while the splash window is open.
    #[test]
    fn an_explicit_exit_is_never_held_by_its_own_window() {
        assert_eq!(
            classify_exit_request(Some(0), false, true, true),
            ExitRequestVerdict::Quit
        );
        // The quit flow's own "no windows" re-entry (splash still up) must not
        // prevent the exit it is completing.
        assert_eq!(
            classify_exit_request(None, false, true, true),
            ExitRequestVerdict::Quit
        );
    }

    /// The updater's relaunch keeps its own verdict ahead of every other rule:
    /// it must detach instead of stopping the daemon (and never be held).
    #[test]
    fn the_updater_relaunch_outranks_the_other_rules() {
        for (migration, windows, quit) in [
            (false, false, false),
            (true, false, false),
            (true, true, true),
        ] {
            assert_eq!(
                classify_exit_request(Some(RESTART_EXIT_CODE), migration, windows, quit),
                ExitRequestVerdict::RestartRelaunch
            );
        }
    }
}

#[cfg(test)]
mod migration_guard_tests {
    use super::{classify_exit_request, ExitRequestVerdict, MigrationGuard};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    fn marks_any(marks: &Arc<Mutex<HashSet<String>>>) -> bool {
        !marks.lock().expect("migration marks").is_empty()
    }

    /// The restart transaction runs `begin_migration → recreate_windows().await
    /// → end_migration`; if that future is dropped mid-`await` (or a panic
    /// unwinds it), `end_migration` never runs. The marks stay set,
    /// `is_migrating_any()` stays true and **every** later exit request is held
    /// — a window-less app that can never quit (A4 follow-up #1). The guard is
    /// the fix: dropping it, with no explicit end, must clear exactly the labels
    /// it set.
    #[test]
    fn dropping_the_guard_without_ending_the_migration_clears_the_marks() {
        let marks = Arc::new(Mutex::new(HashSet::new()));
        let labels = vec!["w1".to_string(), "w2".to_string()];
        let guard = MigrationGuard::begin(Arc::clone(&marks), &labels);
        // Mid-migration, between the destroyed and the re-created window: the
        // exit request is held by design.
        assert_eq!(
            classify_exit_request(None, marks_any(&marks), false, false),
            ExitRequestVerdict::Hold
        );
        // The abandoning path: no explicit end, the transaction's future is
        // dropped (or a panic unwinds it).
        drop(guard);
        assert!(
            !marks_any(&marks),
            "the guard must clear its labels on drop, or the app holds every later exit forever"
        );
        assert_eq!(
            classify_exit_request(None, marks_any(&marks), false, false),
            ExitRequestVerdict::Quit,
            "a dropped migration must not hold the next exit"
        );
    }
}

/// The restart migration's window-geometry hand-off (see `recreate_windows`).
///
/// The plugin writes `.window-state.json` only on `RunEvent::Exit`, and only
/// refreshes a window's cache entry on `CloseRequested`; `destroy()` fires
/// neither. A migrated window was therefore re-created from whatever the
/// in-memory cache held at that moment — the geometry a session of
/// `Resized`/`Moved` events had left there — and `open_native_window`'s clamp
/// then turned an oversized restore into the "maximized after restart" window
/// Dr. Lin saw. The transaction must save each window's live geometry **before**
/// destroying it, which is the whole fix.
///
/// Asserted against the source, not a live `AppHandle`: the call needs a real
/// `WebviewWindow`, so the assertion locks the ordering inside the
/// `if let Some(window)` arm.
#[cfg(test)]
mod migration_window_state_tests {
    fn source_body<'a>(source: &'a str, marker: &str) -> &'a str {
        let start = source
            .find(marker)
            .unwrap_or_else(|| panic!("{marker} not found in the source"));
        let rest = &source[start..];
        let end = rest.find("\n}").map(|i| i + 2).unwrap_or(rest.len());
        &rest[..end]
    }

    #[test]
    fn the_migration_saves_a_windows_geometry_before_destroying_it() {
        let body = source_body(include_str!("main.rs"), "async fn recreate_windows");
        let save = body
            .find("save_window_state(window_state_flags())")
            .unwrap_or_else(|| {
                panic!(
                    "the migration must overwrite the plugin's cache from the live window:\n{body}"
                )
            });
        let destroy = body
            .find(".destroy()")
            .unwrap_or_else(|| panic!("the migration destroys each window:\n{body}"));
        assert!(
            save < destroy,
            "the save must run while the window is still alive:\n{body}"
        );
    }
}

/// Source-inspection tests for the ⌘N/⌘T migration (multi-window §5/D6): the
/// menu is native muda state that cannot be built without a running Tauri app,
/// so the accelerator wiring and the handler arms are pinned against the
/// source, the same form `migration_window_state_tests` uses.
#[cfg(test)]
mod multi_window_menu_tests {
    fn section_between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let start_at = source
            .find(start)
            .unwrap_or_else(|| panic!("marker {start:?} not found in the source"));
        let rest = &source[start_at..];
        let end_at = rest
            .find(end)
            .unwrap_or_else(|| panic!("marker {end:?} not found after {start:?}"));
        &rest[..end_at]
    }

    #[test]
    fn file_menu_binds_new_window_to_cmd_n_and_new_session_to_cmd_t_in_that_order() {
        let body = section_between(
            include_str!("main.rs"),
            "fn build_app_menu",
            "fn disable_press_and_hold_accents",
        );
        let new_window = body.find("MENU_NEW_WINDOW_ID").expect("New Window item");
        let cmd_n = body.find("CmdOrCtrl+N").expect("New Window accelerator");
        let new_session = body.find("MENU_NEW_SESSION_ID").expect("New Session item");
        let cmd_t = body.find("CmdOrCtrl+T").expect("New Session accelerator");
        assert!(
            new_window < cmd_n && cmd_n < new_session && new_session < cmd_t,
            "New Window (⌘N) must precede New Session (⌘T), Safari/Finder order:\n{body}"
        );
    }

    #[test]
    fn cmd_n_menu_event_opens_a_landing_window_without_evaling_any_webview() {
        let arm = section_between(
            include_str!("main.rs"),
            "MENU_NEW_WINDOW_ID =>",
            "MENU_NEW_SESSION_ID =>",
        );
        assert!(
            arm.contains("open_new_landing_window"),
            "⌘N is Rust-side (spec §5): {arm}"
        );
        assert!(
            !arm.contains(".eval("),
            "the New Window arm must not synthesize a WebView keydown: {arm}"
        );
    }

    #[test]
    fn cmd_t_menu_event_synthesizes_the_rebound_key() {
        let arm = section_between(
            include_str!("main.rs"),
            "MENU_NEW_SESSION_ID =>",
            "MENU_ADD_PROJECT_ID =>",
        );
        assert!(
            arm.contains("key: 't'"),
            "New Session synthesizes ⌘T into the focused webview: {arm}"
        );
        assert!(
            !arm.contains("key: 'n'"),
            "the old ⌘N synthesis must be gone: {arm}"
        );
    }

    #[test]
    fn windows_get_the_native_menu_and_event_wiring() {
        let source = include_str!("main.rs");
        let before_menu_fn =
            section_between(source, "const MENU_ADD_PROJECT_ID", "fn build_app_menu");
        assert!(
            before_menu_fn.contains("#[cfg(any(target_os = \"macos\", target_os = \"windows\"))]"),
            "build_app_menu must compile for Windows too:\n{before_menu_fn}"
        );
        let wiring = section_between(
            source,
            "let builder = tauri::Builder::default();",
            ".plugin(tauri_plugin_dialog::init())",
        );
        assert!(
            wiring.contains("#[cfg(any(target_os = \"macos\", target_os = \"windows\"))]"),
            "the menu and event wiring must not be macOS-only:\n{wiring}"
        );
        assert!(
            wiring.contains("ctrlKey"),
            "the New Session synthesis must send Ctrl outside macOS:\n{wiring}"
        );
        assert!(
            wiring.contains("open_new_landing_window"),
            "the New Window arm stays Rust-side on every menu platform:\n{wiring}"
        );
    }

    #[test]
    fn new_landing_window_mints_before_opening_and_rolls_back_on_open_failure() {
        let body = section_between(
            include_str!("main.rs"),
            "fn open_new_landing_window",
            "fn open_native_workspace_window",
        );
        let label = body.find("next_landing_index").expect("label allocation");
        let mint = body.find("create_owner").expect("owner mint");
        let open = body
            .find("open_native_landing_window")
            .expect("window open");
        let detach = body.find("owner.detach").expect("rollback detach");
        assert!(
            label < mint && mint < open,
            "mint-first discipline (T1): allocate → mint → open:\n{body}"
        );
        assert!(
            open < detach,
            "an open failure must revoke the minted owner (the label frees itself with no window):\n{body}"
        );
    }
}

#[cfg(test)]
mod skill_source_scope_tests {
    use super::*;
    use picot_host::host_control_dispatch::skill_source_scope;
    use picot_host::window_owner::WindowOwnerRegistry;

    fn owner_registry() -> (WindowOwnerRegistry, PathBuf) {
        let home = std::env::temp_dir().join(format!(
            "picot-skill-scope-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        (WindowOwnerRegistry::default(), home)
    }

    #[test]
    fn landing_owner_binds_its_root_without_project_authority() {
        let (registry, home) = owner_registry();
        let (owner, _) = registry
            .create_owner_with_workspace(
                "native-landing".to_string(),
                home.clone(),
                0,
                "http://127.0.0.1:1".to_string(),
                None,
                window_owner::TemporaryKind::Landing,
            )
            .expect("landing owner");
        let (root, generation, port, registered) =
            skill_source_scope(&registry, &owner).expect("landing binds its placeholder root");
        assert_eq!(root, home);
        assert_eq!(generation, 0);
        assert_eq!(port, 0);
        // Only the global scope is installable for a workspaceless owner; the
        // install op refuses `project` on this flag.
        assert!(!registered);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn registered_owner_keeps_project_authority() {
        let (registry, home) = owner_registry();
        let (owner, _) = registry
            .create_owner_with_workspace(
                "native-workspace".to_string(),
                home.clone(),
                0,
                "http://127.0.0.1:1".to_string(),
                Some("wid-1".to_string()),
                window_owner::TemporaryKind::DefaultStartup,
            )
            .expect("registered owner");
        let (_, _, _, registered) =
            skill_source_scope(&registry, &owner).expect("registered owner binds");
        assert!(registered);
        let _ = std::fs::remove_dir_all(home);
    }
}

#[cfg(test)]
mod startup_tests {
    use picot_host::temp_resources::{create_quick_chat_temp_dir_in, ensure_picot_tmp_root_in};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn picot_tmp_root_is_created_canonical_and_forced_owner_only() {
        let home = unique_temp_dir("tmp-root-home");
        fs::create_dir_all(&home).unwrap();
        let loose = home.join(".pi").join("tmp");
        fs::create_dir_all(&loose).unwrap();
        // A previous run may have left the root world-readable; ensure must heal it.
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o755)).unwrap();

        let root = ensure_picot_tmp_root_in(&home).expect("tmp root");
        assert!(root.is_absolute());
        assert_eq!(root.file_name().and_then(|n| n.to_str()), Some("tmp"));
        let mode = fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "root must be owner-only");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn quick_chat_children_live_private_under_the_picot_root() {
        let home = unique_temp_dir("tmp-root-home2");
        fs::create_dir_all(&home).unwrap();
        let root = ensure_picot_tmp_root_in(&home).expect("tmp root");

        let (dir, token) = create_quick_chat_temp_dir_in(&root).expect("chat dir");
        assert!(dir.starts_with(&root));
        assert!(dir.to_string_lossy().contains(&token));
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "chat dir must be owner-only");
        let _ = fs::remove_dir_all(home);
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("picot-startup-{label}-{suffix}"))
    }
}
