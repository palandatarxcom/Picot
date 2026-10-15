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

use daemon_client::admin_client::{AdminClient, SurfaceExecutor};
use daemon_client::manager::{DaemonManager, EnsureOutcome, ManagerConfig};
use daemon_client::navigation_mirror::NavigationMirror;
use daemon_client::surface_ops::ForwardedSurfaceOps;
use picot_host::daemon::admin::ManagerIdentity;
use picot_host::daemon::DaemonState;
use picot_host::{child_supervision, git_service, officecli_watch, window_owner};

use picot_host::ephemeral_registry::EphemeralRegistry;
use picot_host::host_control::ProgressSink;
use picot_host::host_server::HostServer;
use picot_host::native_pi_manager::NativePiManager;
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::skill_source_registry::SkillSourceRegistry;
use picot_host::temp_resources::{canonical_temp_root, cleanup_quick_chat_dir};
use picot_host::terminal_manager::TerminalManager;
use picot_host::window_owner::WindowOwnerRegistry;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, TitleBarStyle, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_window_state::StateFlags;

type HostServerState = Arc<HostServer>;
type NativePiManagerState = NativePiManager;
#[allow(dead_code)]
type OwnerRegistryState = Arc<WindowOwnerRegistry>;
type SkillSourceRegistryState = Arc<SkillSourceRegistry>;
type TerminalManagerState = Arc<TerminalManager>;
#[allow(dead_code)]
type EphemeralRegistryState = Arc<EphemeralRegistry>;

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

/// The cold-start window: host-origin `/`, owner `native-landing`. The label
/// never changes (Tauri labels are immutable); the owner record carries the
/// workspace state.
fn open_native_landing_window(
    app: &AppHandle,
    host_origin: &str,
    mirror: Arc<NavigationMirror>,
    owner: window_owner::OwnerId,
    capability: &str,
) -> Result<(), String> {
    open_native_window(
        app,
        "native-landing",
        &format!("{host_origin}/"),
        mirror,
        owner,
        capability,
    )
}

fn open_native_workspace_window(
    app: &AppHandle,
    host_origin: &str,
    target: &RuntimeTarget,
    mirror: Arc<NavigationMirror>,
    owner: window_owner::OwnerId,
    capability: &str,
) -> Result<(), String> {
    let label = format!("native-workspace-{}", target.workspace_id);
    let url = format!(
        "{}/workspaces/{}/sessions/{}",
        host_origin, target.workspace_id, target.session_id
    );
    open_native_window(app, &label, &url, mirror, owner, capability)
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
        clamp_window_geometry, image_mime_from_path, new_session_menu_enabled, resolve_static_dir,
        session_export_fetch_url, side_chat_startup_rpc_commands,
    };
    // The host-side helpers moved into `picot-host` with the dispatcher
    // (Task 2 Step 2); their tests stayed here.
    use picot_host::host_control_dispatch::{
        cancel_should_stop_target, filter_session_delete_paths, find_existing_runtime_for_prepare,
        persisted_session_id_for_workspace, quick_chat_snapshot, runtime_instance_summaries,
        runtime_started_event, should_stop_owner_runtimes_on_transition, skill_scope_context,
        target_for_owner_in_workspace, touch_registered_workspace, workspace_snapshot_for,
    };
    use picot_host::metadata_store::{MetadataStore, SharedMetadataStore};
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TOUCH_TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        let target = picot_host::runtime_coordinator::RuntimeTarget::with_owner(
            "workspace-a",
            "saved",
            "instance-a",
            "owner-a",
            0,
        );
        manager.register_in_memory(target.clone()).unwrap();

        assert_eq!(
            find_existing_runtime_for_prepare(
                &manager,
                "workspace-a",
                "owner-a",
                Some("saved"),
                false,
            ),
            Some(target),
        );
        assert!(find_existing_runtime_for_prepare(
            &manager,
            "workspace-a",
            "owner-a",
            Some("saved"),
            true,
        )
        .is_none());
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
            find_existing_runtime_for_prepare(
                &manager,
                "workspace-a",
                "owner-a",
                Some("saved-session"),
                false,
            ),
            Some(prior.clone()),
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

/// The app's daemon link (plan Task 5).
///
/// One mode-independent shape for both process layouts: the admin client, the
/// navigation mirror it feeds, and the mode-specific handles (`embedded` holds
/// the in-process daemon state; `external` holds the manager whose spawn and
/// child handle the quit path (T8) and the restart transaction (T9) reuse).
///
/// Managed as app state so the window lifecycle, the menu gate and
/// `RunEvent::Exit` can reach it; nothing here is a Tauri command (D8 keeps the
/// app's command surface empty until T10).
pub struct DaemonLink {
    pub mode: DaemonMode,
    pub client: Arc<AdminClient>,
    pub mirror: Arc<NavigationMirror>,
    /// `Some` in embedded mode only: the in-process daemon this app runs.
    pub embedded: Option<Arc<DaemonState>>,
    /// `Some` in external mode only: how this app started/adopted the daemon.
    #[allow(dead_code)]
    pub manager: Option<Arc<DaemonManager>>,
    /// The daemon's HTTP origin (the WebView's page origin in both modes).
    pub host_origin: String,
}

impl DaemonLink {
    /// The app-side owner/authorization image the daemon feeds (plan D4).
    fn mirror_for(app: &AppHandle) -> Option<Arc<NavigationMirror>> {
        app.try_state::<DaemonLink>()
            .map(|link| Arc::clone(&link.mirror))
    }

    /// The app-process half of app exit.
    ///
    /// External: disconnect the admin channel only — the daemon keeps running,
    /// and dropping the socket is exactly what makes the daemon run its
    /// dead-attachment cleanup (plan D11). The quit *policy* (keepRunning,
    /// confirmed exit, signal tiers) arrives with T8; this interim behaviour is
    /// recorded in ARCHITECTURE.
    ///
    /// Embedded: the daemon is this process, so exit drains it through the same
    /// `shutdown_graceful` the standalone binary's signal path uses.
    fn on_app_exit(&self) {
        match self.mode {
            DaemonMode::External => {
                log::info!("[picot] exiting: detaching from the Picot daemon (it keeps running)");
                self.client.disconnect();
            }
            DaemonMode::Embedded => {
                if let Some(state) = self.embedded.as_ref() {
                    // The exit path runs on the main thread; the drain is async
                    // (runtimes, terminals, watches, the child registry, the
                    // host listener), so it is driven to completion here exactly
                    // as startup drives the async boot.
                    tauri::async_runtime::block_on(state.shutdown_graceful("app exit"));
                }
            }
        }
    }
}

/// Which process layout this run uses (plan Task 5 Step 3).
///
/// `PICOT_DAEMON=embedded|external` is the dev/validation switch (plan D2): the
/// two modes differ only by the process boundary — both run the same
/// `DaemonState`, the same admin protocol and the same `AdminClient`.
///
/// The defaults are transitional and deliberate: **debug → external** (the M3
/// dev loop exercises the real process boundary) and **release → embedded**
/// until T11 packages the daemon sidecar and T12 deletes the embedded branch —
/// a released app has no `picot-daemon` binary to spawn yet. Both the switch and
/// the defaults are documented in ARCHITECTURE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonMode {
    Embedded,
    External,
}

fn daemon_mode() -> Result<DaemonMode, String> {
    match std::env::var("PICOT_DAEMON").ok().as_deref() {
        None | Some("") => Ok(if cfg!(debug_assertions) {
            DaemonMode::External
        } else {
            DaemonMode::Embedded
        }),
        Some("external") => Ok(DaemonMode::External),
        Some("embedded") => Ok(DaemonMode::Embedded),
        Some(other) => Err(format!(
            "PICOT_DAEMON must be `embedded` or `external`, got {other:?}"
        )),
    }
}

/// What booting the daemon produced.
enum AttachedDaemon {
    Ready {
        manager: Option<Arc<DaemonManager>>,
        embedded: Option<Arc<DaemonState>>,
        client: Arc<AdminClient>,
        host_origin: String,
    },
    /// Another live Picot app manages this daemon (plan Task 4 policy).
    BlockedByOtherApp,
}

/// Start the daemon as its own process and attach to it (external mode).
async fn attach_external_daemon(
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
    Ok(AttachedDaemon::Ready {
        manager: Some(manager),
        embedded: None,
        client,
        host_origin: file.host_origin,
    })
}

/// Start the daemon inside this process, behind the *same* admin listener the
/// standalone binary uses; the app then attaches to it over the same client
/// (embedded mode, plan D2).
async fn start_embedded_daemon(
    app: &tauri::App,
    data_dir: PathBuf,
    config_dir: PathBuf,
    static_dir: PathBuf,
    bundle_identifier: String,
    executor: Arc<dyn SurfaceExecutor>,
    mirror: Arc<NavigationMirror>,
) -> Result<AttachedDaemon, String> {
    let surface = Arc::new(picot_host::daemon::surface_forward::AdminForwardingSurface::new());
    let state = Arc::new(
        DaemonState::start(
            picot_host::daemon::DaemonConfig {
                data_dir,
                config_dir,
                static_dir,
                bundle_identifier,
            },
            surface.clone(),
        )
        .await?,
    );
    let token = picot_host::daemon::instance::generate_admin_token();
    let admin = picot_host::daemon::admin::AdminServer::start(
        picot_host::daemon::admin::AdminConfig::new(token.clone()),
    )
    .await?;
    state.install_admin(admin.clone());
    surface.attach(admin.clone());
    state.install_control_handler();
    register_embedded_state(app, &state);
    let host_origin = state.origin().to_string();
    let client = AdminClient::connect(
        admin.origin(),
        &token,
        &ManagerIdentity {
            pid: Some(std::process::id()),
            app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            build_id: Some(picot_host::BUILD_ID.to_string()),
        },
        executor,
        mirror,
    )
    .await?;
    log::info!("[picot] embedded daemon host={host_origin}");
    Ok(AttachedDaemon::Ready {
        manager: None,
        embedded: Some(state),
        client,
        host_origin,
    })
}

/// Register the embedded daemon's shared state with the app.
///
/// Embedded mode only: these are the app-side readers that still look host
/// objects up through the app handle (the window-destroyed teardown, the exit
/// path, the browser-pane runtime). In external mode the same objects live in
/// the daemon process and the app holds no copy (plan D4/D5) — the dispatcher
/// reaches them there through `DaemonState`.
fn register_embedded_state(app: &tauri::App, state: &Arc<DaemonState>) {
    let host = state.host.clone();
    app.manage(state.skill_sources.clone());
    app.manage(state.git.clone());
    app.manage(state.terminals.clone());
    // No `EnvironmentInstaller` registration: its only reader was the app's exit
    // path, and the embedded daemon's drain now stops the installer itself
    // (`DaemonState::drain_runtimes`). A managed copy with no reader is exactly
    // the dead state this move is supposed to delete.
    // Browser panes (spec 2026-09-22): child webviews keyed by tab id. The
    // runtime needs the app handle, so it is constructed here and registered
    // with the host through the `PaneSurface` slot (plan D1/Step-3 exceptions).
    let browser_panes = Arc::new(browser_pane::BrowserPaneRuntime::new(
        app.handle().clone(),
        host.origin(),
    ));
    host.set_browser_panes(browser_panes.clone());
    app.manage(browser_pane::BrowserPaneState(browser_panes));
    app.manage(officecli_watch::OfficecliWatchState(
        state.officecli_watches.clone(),
    ));
    app.manage(state.runtimes.clone());
    app.manage(host);
    app.manage(state.owner_registry.clone());
    app.manage(state.ephemeral_registry.clone());
}

/// Boot the app around its daemon (plan Task 5 Step 3).
///
/// Both modes run the *same* sequence: get a running `DaemonState` (in this
/// process, or in its own), connect the `AdminClient` to its admin listener,
/// mint the landing owner over that channel, and open the landing window at the
/// daemon's host origin. There is no in-process shortcut in either mode — the
/// only difference is the process boundary (plan D2).
fn setup_native_runtime(app: &mut tauri::App, static_dir: PathBuf) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Cannot resolve Picot app data directory: {error}"))?;
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|error| format!("Cannot resolve Picot app config directory: {error}"))?;
    let bundle_identifier = app.config().identifier.clone();
    let mode = daemon_mode()?;
    // The app's navigation/authorization image and the pump that executes
    // forwarded surface ops, both shared by the two modes.
    let mirror = Arc::new(NavigationMirror::new());
    let executor: Arc<dyn SurfaceExecutor> = Arc::new(ForwardedSurfaceOps::new(Arc::new(
        tauri_app_surface::TauriAppSurface::new(app.handle().clone(), Arc::clone(&mirror)),
    )));
    log::info!("[picot] daemon mode {mode:?} (PICOT_DAEMON)");
    let attached = match mode {
        DaemonMode::External => tauri::async_runtime::block_on(attach_external_daemon(
            data_dir,
            config_dir,
            static_dir,
            bundle_identifier,
            executor,
            Arc::clone(&mirror),
        ))?,
        DaemonMode::Embedded => tauri::async_runtime::block_on(start_embedded_daemon(
            app,
            data_dir,
            config_dir,
            static_dir,
            bundle_identifier,
            executor,
            Arc::clone(&mirror),
        ))?,
    };
    let (manager, embedded, client, host_origin) = match attached {
        AttachedDaemon::BlockedByOtherApp => {
            // The dialog blocks and the process exits non-zero: a second Picot
            // must not fight the first for the daemon's lease. `None` resolves
            // the locale from the OS (T10 passes the app's preference).
            daemon_client::present_blocked_by_other_app(app.handle(), None);
        }
        AttachedDaemon::Ready {
            manager,
            embedded,
            client,
            host_origin,
        } => (manager, embedded, client, host_origin),
    };
    // The landing owner: no registered workspace, placeholder home cwd. Minted
    // over the admin channel in both modes, so the mirror learns its origin
    // before the window can load it.
    let home = canonical_home_dir()?;
    let landing = tauri::async_runtime::block_on(client.create_owner(
        "native-landing",
        &home.to_string_lossy(),
        0,
        &host_origin,
        None,
    ))?;
    open_native_landing_window(
        app.handle(),
        &host_origin,
        Arc::clone(&mirror),
        window_owner::OwnerId::from_string(landing.owner.clone()),
        &landing.capability,
    )?;
    app.manage(DaemonLink {
        mode,
        client,
        mirror,
        embedded,
        manager,
        host_origin: host_origin.clone(),
    });
    // Startup default is disabled (the first window is the landing); recompute
    // once now that the window exists. No-op wherever no menu bar is installed.
    update_new_session_menu_state(app.handle());
    log::info!("[picot] landing ready mode={mode:?} origin={host_origin}");
    Ok(())
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
/// disconnected WebView falls back to a native warning.
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

    let delivered = window
        .try_state::<HostServerState>()
        .map(|host| {
            host.send_owner_event(
                &owner,
                serde_json::json!({ "type": "window_close_request", "requestId": request_id }),
            )
        })
        .unwrap_or(false);
    if delivered {
        return;
    }

    // Disconnected WebView fallback: a native warning. Confirm closes (after
    // settlement of host-owned state); cancel drops the pending request.
    let app = window.app_handle().clone();
    let owner_for_dialog = owner.clone();
    let mirror_for_dialog = mirror.clone();
    window
        .app_handle()
        .dialog()
        .message("Closing this window will discard unsaved changes and any temporary chats.")
        .title("Close window")
        .buttons(tauri_plugin_dialog::MessageDialogButtons::OkCancelCustom(
            "Close anyway".to_string(),
            "Cancel".to_string(),
        ))
        .show(move |result| {
            let mut g = close_approvals().lock().unwrap();
            if result {
                if let Some(pending) = g.get_mut(&owner_for_dialog) {
                    pending.approved = true;
                }
                let label = mirror_for_dialog.label_for_owner(owner_for_dialog.as_str());
                drop(g);
                if let Some(label) = label {
                    let _ = app.get_webview_window(&label).map(|w| w.close());
                }
            } else {
                g.remove(&owner_for_dialog);
            }
        });
}

/// Final idempotent cleanup when a native window is destroyed: kill the
/// owner's runtime, run generation-checked ephemeral cleanup, drop any pending
/// close, and revoke the owner. Keyed on the owner registry record — never the
/// window label — so a landing window that transitioned into a workspace gets
/// the same cleanup as a workspace window despite its immutable label.
fn handle_window_destroyed(window: &tauri::Window) {
    let label = window.label();
    // Browser panes die with their window; drop the manager's stale entries
    // so labels can be reused without collisions.
    if let Some(panes) = window.try_state::<browser_pane::BrowserPaneState>() {
        panes.0.destroy_all_for_window(label);
    }
    let Some(registry) = window.try_state::<OwnerRegistryState>() else {
        return;
    };
    let registry = registry.inner().clone();
    let Some(owner) = registry.owner_for_label(label) else {
        return;
    };
    if let Some(manager) = window.try_state::<NativePiManagerState>() {
        // Unconditional: a no-op when no runtime was ever spawned (landing
        // pre-transition), and what kills the Pi subprocess of a landing
        // window that already transitioned into a workspace.
        manager.stop_for_owner(owner.as_str());
        if let Some(workspace_id) = registry.owner_current_workspace(&owner).workspace_id() {
            let workspace_id = workspace_id.clone();
            manager.stop_for_window_destroy(&workspace_id);
        }
    }
    if let Some(ephemeral) = window.try_state::<EphemeralRegistryState>() {
        for lease in ephemeral.owner_cleanup(&owner) {
            // Native ephemeral runtimes were already stopped by the native
            // branch's stop_for_owner.
            if let Some((path, token)) = &lease.temporary_directory {
                let _ = cleanup_quick_chat_dir(&canonical_temp_root(), path, token);
            }
            ephemeral.finish_cleanup(&lease);
        }
    }
    if let Some(terminal_manager) = window.try_state::<TerminalManagerState>() {
        terminal_manager.kill_owner(&owner);
    }
    if let Some(git_service) = window.try_state::<Arc<git_service::GitService>>() {
        git_service.inner().clear_owner(owner.as_str());
    }
    close_approvals().lock().unwrap().remove(&owner);
    if let Some(skill_sources) = window.try_state::<SkillSourceRegistryState>() {
        skill_sources.revoke_owner(&owner);
    }
    registry.revoke_owner(&owner);
}

// ─── Main ─────────────────────────────────────────────────────────────────────

// Native menus belong in the macOS system menu bar (same as upstream). The
// Edit submenu's key equivalents are load-bearing for WKWebView text input:
// without a native menu the responder chain eats key auto-repeat events, so
// holding a key in the terminal (xterm.js) types exactly one character.
// Windows/Linux draw in-window menus instead, so the builder stays menu-less
// there exactly as upstream does.
const MENU_NEW_SESSION_ID: &str = "picot-new-session";
const MENU_ADD_PROJECT_ID: &str = "picot-add-project";

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

#[cfg(target_os = "macos")]
fn build_app_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let new_session = MenuItem::with_id(
        app,
        MENU_NEW_SESSION_ID,
        "New Session",
        // Startup default: disabled — the first window is the landing.
        new_session_menu_enabled(None),
        Some("CmdOrCtrl+N"),
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
    let help = Submenu::with_items(app, "Help", true, &[])?;
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
    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[&PredefinedMenuItem::fullscreen(app, None)?],
    )?;
    Menu::with_items(app, &[&app_menu, &file, &edit, &view, &window, &help])
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

fn main() {
    // Host tasks spawned through `picot_host::runtime::spawn` must land on the
    // app's tokio runtime (the one `tauri::async_runtime` owns): the pre-split
    // code called `tauri::async_runtime::spawn` for exactly those tasks.
    picot_host::runtime::install(tauri::async_runtime::handle().inner().clone());

    // Sync the user's login-shell environment before anything else.
    // macOS GUI apps (launched from Finder/Dock) inherit neither PATH nor
    // provider API keys exported by shell startup files. `fix_all_vars()`
    // makes the embedded pi process see the same provider configuration as a
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
    #[cfg(target_os = "macos")]
    let builder = builder.menu(build_app_menu).on_menu_event(|app, event| {
        match event.id().as_ref() {
            MENU_NEW_SESSION_ID => {
                // Re-keyed on the owner record: the menu dispatches Cmd+N only
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
                    let _ = window.eval(
                        "document.dispatchEvent(new KeyboardEvent('keydown', {key: 'n', metaKey: true, bubbles: true, cancelable: true}))",
                    );
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
            setup_native_runtime(app, static_dir).map_err(std::io::Error::other)?;
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                handle_close_requested(window, api);
            }
            tauri::WindowEvent::Destroyed => {
                handle_window_destroyed(window);
            }
            tauri::WindowEvent::Focused(true) => {
                // Menu state tracks the focused owner's binding; a landing
                // window's focus also evaluates (its binding can change).
                update_new_session_menu_state(window.app_handle());
            }
            _ => {}
        })
        // The main UI talks to HostServer exclusively over its v2 WebSocket.
        // No legacy broker or Tauri IPC control path remains in production.
        .invoke_handler(tauri::generate_handler![])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle: &tauri::AppHandle, event| {
            if let tauri::RunEvent::Ready = event {
                install_termination_handlers(app_handle.clone());
            }
            if let tauri::RunEvent::Exit = event {
                // The mode decides what app exit means (plan Task 5 Step 3): the
                // embedded daemon is this process and drains here; the external
                // one owns its own lifetime, so the app only detaches. The
                // accepted/completed separation and the keepRunning policy are
                // T8's.
                match app_handle.try_state::<DaemonLink>() {
                    Some(link) => link.on_app_exit(),
                    // No daemon link (boot failed before startup finished):
                    // nothing daemon-side to tear down.
                    None => child_supervision::clear_registry(),
                }
            }
        });
}

/// Tear runtimes down on the signals that otherwise skip `RunEvent::Exit`
/// entirely: Ctrl-C under `tauri dev`, a logout or shutdown (SIGTERM), and a
/// closing terminal (SIGHUP). SIGKILL cannot be caught — the startup sweep
/// exists for that case.
#[cfg(unix)]
fn install_termination_handlers(app_handle: tauri::AppHandle) {
    use std::sync::atomic::{AtomicBool, Ordering};
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
            // Tear down on a detached thread and put a deadline on this
            // handler: `stop_for_app_exit` waits on children that ignore
            // SIGTERM, and the default disposition of these signals is already
            // taken over — blocking here would leave Picot answering nothing but
            // SIGKILL. The registry is intentionally NOT cleared on the timeout
            // path: any runtime the teardown never reached must stay recorded so
            // the next launch can sweep it.
            let teardown = app_handle.clone();
            std::thread::spawn(move || {
                if let Some(manager) = teardown.try_state::<NativePiManagerState>() {
                    manager.stop_for_app_exit();
                }
                child_supervision::clear_registry();
                std::process::exit(0);
            });
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            log::warn!("[picot-native] teardown exceeded its grace period; exiting");
            std::process::exit(0);
        });
    }
}

#[cfg(not(unix))]
fn install_termination_handlers(_app_handle: tauri::AppHandle) {}

#[cfg(test)]
mod skill_source_scope_tests {
    use super::*;
    use picot_host::host_control_dispatch::skill_source_scope;

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
