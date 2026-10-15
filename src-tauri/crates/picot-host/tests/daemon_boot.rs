// ABOUTME: Boot test for DaemonState::start — a headless daemon with no Tauri objects.
// ABOUTME: Task 2 Step 1's red test; green since Step 3 moved setup_native_runtime's construction in.

//! The daemon must be able to run the whole host stack with **no app process
//! at all**: that is the Phase 1 premise (plan D1/D2, ARCHITECTURE process
//! model). This test pins the four things "boots headless" means:
//!
//! 1. `/health` answers 200 on [`DaemonState::origin`];
//! 2. `GET /` serves the static bundle;
//! 3. the metadata SQLite file lands under the injected `data_dir`;
//! 4. boot makes **zero** app-surface calls — the surface is required for the
//!    control dispatcher, never for construction.
//!
//! Point 4 is the reason this test lives in `picot-host`: this crate cannot
//! name the Tauri framework at all (`scripts/check-daemon-no-tauri.js` fails the
//! build), so a test that compiles here is already proof that no Tauri object
//! can be constructed. The recording surface makes "boot does not *use* the
//! app" explicit on top of that.
//!
//! GREEN STATE: `DaemonState::start` (Task 2 Step 3) owns the construction, so
//! this test boots the real host stack from the four argv fields alone. It went
//! red-to-green in Step 3 without a single change to its assertions — nothing
//! in it was relaxed to make the move pass.
//!
//! The `DaemonConfig` field set is the daemon's argv surface (plan D7:
//! `--data-dir --config-dir --static-dir --bundle-identifier`); Step 3 may add
//! fields, but these four are the contract this test already pins.

use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::{DaemonConfig, DaemonState};
use picot_host::runtime_coordinator::RuntimeTarget;
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// An `AppSurface` that fails every call and records the attempt.
///
/// A headless boot has no window, no updater and no dialogs: any call here is a
/// bug in the construction path, so the methods return `Err` instead of a
/// plausible `Ok`.
#[derive(Default)]
struct RecordingSurface {
    calls: Mutex<Vec<String>>,
}

impl RecordingSurface {
    fn record(&self, op: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(op.to_string());
        Err(format!(
            "unexpected app-surface call during headless boot: {op}"
        ))
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl AppSurface for RecordingSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &RuntimeTarget,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        self.record("open_workspace_window")
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        self.record("open_devtools")
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        self.record("close_window")
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        self.record("cancel_window_close")
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        self.record("menu_state_changed")
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        self.record("relaunch_app")
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        self.record("pane")?;
        Ok(Value::Null)
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        self.record("pick_folder")?;
        Ok(None)
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        self.record("pick_image_files")?;
        Ok(Value::Null)
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        self.record("save_session_export")?;
        Ok(Value::Null)
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        self.record("check_for_update")?;
        Ok(Value::Null)
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        self.record("download_and_install_update")?;
        Ok(Value::Null)
    }
}

/// `data_dir/picot.sqlite3` must exist and really be a SQLite database — the
/// daemon is the sole SQLite writer (plan D7/ARCHITECTURE), so "SQLite landed
/// under data_dir" is asserted by header, not by path alone.
fn assert_sqlite_under(data_dir: &std::path::Path) {
    let db = data_dir.join("picot.sqlite3");
    let bytes = std::fs::read(&db)
        .unwrap_or_else(|error| panic!("expected the metadata store at {}: {error}", db.display()));
    assert!(
        bytes.starts_with(b"SQLite format 3\0"),
        "{} is not a SQLite database",
        db.display()
    );
}

#[tokio::test]
async fn daemon_state_boots_headless_and_serves_health_and_static() {
    const MARKER: &str = "<h1>headless daemon boot</h1>";

    let temp = tempfile::tempdir().expect("tempdir");
    let data_dir = temp.path().join("data");
    let config_dir = temp.path().join("config");
    let static_dir = temp.path().join("static");
    std::fs::create_dir_all(&static_dir).expect("static dir");
    std::fs::write(static_dir.join("index.html"), MARKER).expect("static index");

    let surface = Arc::new(RecordingSurface::default());
    let state = DaemonState::start(
        DaemonConfig {
            data_dir: data_dir.clone(),
            config_dir,
            static_dir,
            bundle_identifier: "com.palandata.picot.test".to_string(),
        },
        surface.clone(),
    )
    .await
    .expect("the daemon host stack boots with no Tauri app");

    let origin = state.origin().to_string();
    assert!(
        origin.starts_with("http://127.0.0.1:"),
        "daemon origin must be a loopback HTTP origin, got {origin}"
    );

    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let health = client
        .get(format!("{origin}/health"))
        .send()
        .await
        .expect("health request");
    assert_eq!(health.status().as_u16(), 200, "/health must answer 200");
    let health: Value = serde_json::from_str(&health.text().await.unwrap()).unwrap();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["protocolVersion"].as_u64(), Some(2));

    let index = client
        .get(format!("{origin}/"))
        .send()
        .await
        .expect("static request");
    assert_eq!(index.status().as_u16(), 200, "GET / must serve the bundle");
    assert!(
        index.text().await.unwrap().contains(MARKER),
        "GET / must serve the injected static bundle"
    );

    assert_sqlite_under(&data_dir);
    assert_eq!(
        surface.calls(),
        Vec::<String>::new(),
        "headless boot must not touch the app surface"
    );

    state.shutdown_graceful("test").await;

    // A shut-down daemon stops answering on its origin: the listener is gone.
    // Polled because `shutdown_graceful` hands the serve task a signal (plan
    // D12 separates "accepted" from "completed"); the assertion is that it
    // stops, not that it stopped before the next tick.
    let mut stopped = false;
    for _ in 0..40 {
        match client.get(format!("{origin}/health")).send().await {
            Ok(response) if response.status().is_success() => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            _ => {
                stopped = true;
                break;
            }
        }
    }
    assert!(
        stopped,
        "origin {origin} must stop serving after shutdown_graceful"
    );
}
