// ABOUTME: Task 4 integration: the daemon's instance file + lock over a real admin socket —
// ABOUTME: lease grant republishes the manager record, and the drain removes both files.

//! The two things only a real (non-mocked) pair can show:
//!
//! 1. **The daemon — not the app — publishes the manager record.** A real admin
//!    client completes `admin_hello`; the instance file on disk must then carry
//!    that app's pid, without the client ever writing a file (plan D3).
//! 2. **The drain removes the instance file and the lock** — the single plug
//!    point T3 reserved (`DrainStep::DrainFiles`), reachable from `daemon.stop`
//!    exactly as it is from a signal.
//!
//! The client side speaks the literal protocol frames; the daemon side is the
//! production `AdminServer` + `DaemonState`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use picot_host::daemon::admin::{AdminConfig, AdminServer, ADMIN_WS_PATH};
use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::instance::{self, DaemonFiles, InstanceFile, InstanceLock, InstanceStore};
use picot_host::daemon::{DaemonConfig, DaemonState, DrainObserver, DrainStep};
use picot_host::runtime_coordinator::RuntimeTarget;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// A surface that fails every call: this test never exercises the app surface.
struct NeverCalledSurface;

#[async_trait::async_trait]
impl AppSurface for NeverCalledSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &RuntimeTarget,
        _label: &str,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Err("unused".into())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused".into())
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused".into())
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused".into())
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        Err("unused".into())
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused".into())
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        Err("unused".into())
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused".into())
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        Err("unused".into())
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        Err("unused".into())
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused".into())
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        Err("unused".into())
    }
}

#[derive(Default)]
struct StepLog(std::sync::Mutex<Vec<DrainStep>>);

impl DrainObserver for StepLog {
    fn step(&self, step: DrainStep) {
        self.0.lock().unwrap().push(step);
    }
}

impl StepLog {
    fn steps(&self) -> Vec<DrainStep> {
        self.0.lock().unwrap().clone()
    }
}

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn wait_for(deadline: Duration, mut predicate: impl FnMut() -> bool, what: &str) {
    let started = std::time::Instant::now();
    while started.elapsed() < deadline {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn next_json(socket: &mut Client) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(15), socket.next())
            .await
            .expect("a frame within the deadline")
            .expect("the socket is open")
            .expect("a valid frame");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).expect("a JSON frame");
        }
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    data_dir: PathBuf,
    state: Arc<DaemonState>,
    admin: AdminServer,
    token: String,
}

impl Fixture {
    async fn boot() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = temp.path().join("data");
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).expect("static dir");
        std::fs::write(static_dir.join("index.html"), "<h1>instance</h1>").expect("index");

        let state = Arc::new(
            DaemonState::start(
                DaemonConfig {
                    data_dir: data_dir.clone(),
                    config_dir: temp.path().join("config"),
                    static_dir,
                    bundle_identifier: "com.palandata.picot.test".to_string(),
                },
                Arc::new(NeverCalledSurface),
            )
            .await
            .expect("the daemon host stack boots"),
        );
        let token = instance::generate_admin_token();
        let admin = AdminServer::start(AdminConfig::new(token.clone()))
            .await
            .expect("the admin listener binds");
        state.install_admin(admin.clone());

        // The daemon binary's boot order, minus argv: lock first, then publish.
        let lock = InstanceLock::acquire(&instance::lock_path(&data_dir))
            .expect("the lock is available")
            .expect("no other daemon holds it");
        let store = InstanceStore::new(instance::instance_path(&data_dir));
        state.set_daemon_files(DaemonFiles {
            store: store.clone(),
            lock,
        });
        store
            .publish(InstanceFile {
                pid: std::process::id(),
                process_start_time: picot_host::child_supervision::process_start_time(
                    std::process::id(),
                ),
                host_origin: state.origin().to_string(),
                admin_origin: admin.origin().to_string(),
                admin_token: token.clone(),
                started_at: instance::now_timestamp(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                build_id: picot_host::BUILD_ID.to_string(),
                manager: None,
            })
            .expect("the instance file is published");

        Self {
            _temp: temp,
            data_dir,
            state,
            admin,
            token,
        }
    }

    fn instance_path(&self) -> PathBuf {
        instance::instance_path(&self.data_dir)
    }

    fn lock_path(&self) -> PathBuf {
        instance::lock_path(&self.data_dir)
    }

    async fn hello(&self, pid: u32) -> (Client, Value) {
        let url = format!(
            "ws://{}{ADMIN_WS_PATH}",
            self.admin.origin().trim_start_matches("http://")
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("a real admin client connects");
        let hello = json!({
            "type": "admin_hello",
            "adminToken": self.token,
            "role": "manager",
            "appVersion": env!("CARGO_PKG_VERSION"),
            "buildId": picot_host::BUILD_ID,
            "pid": pid,
        });
        socket
            .send(Message::Text(hello.to_string()))
            .await
            .expect("hello sent");
        let welcome = next_json(&mut socket).await;
        assert_eq!(welcome["type"], "admin_welcome", "{welcome}");
        (socket, welcome)
    }
}

/// Plan D3: the daemon — not the app — rewrites the instance file's manager
/// record when it grants the lease; the app never touches the file.
#[tokio::test(flavor = "multi_thread")]
async fn the_lease_grant_publishes_the_manager_record_and_the_drain_removes_the_files() {
    let fixture = Fixture::boot().await;
    let steps = Arc::new(StepLog::default());
    fixture.state.set_drain_observer(steps.clone());

    // The published document is what an app probes: no manager yet.
    let published = instance::read(&fixture.instance_path()).expect("readable");
    assert!(published.manager.is_none());
    assert_eq!(published.admin_origin, fixture.admin.origin());
    assert_eq!(published.host_origin, fixture.state.origin());

    // A second daemon cannot take the lock while this one holds it.
    assert!(
        InstanceLock::acquire(&fixture.lock_path())
            .expect("the lock file is present")
            .is_none(),
        "the single-instance lock is held for the daemon's whole life"
    );

    let (mut socket, welcome) = fixture.hello(4242).await;
    assert_eq!(welcome["epoch"].as_u64(), Some(1));

    // The daemon rewrote its own file: the manager record is the app that just
    // completed the handshake.
    let with_manager = instance::read(&fixture.instance_path()).expect("still readable");
    let manager = with_manager.manager.expect("the manager record landed");
    assert_eq!(manager.pid, Some(4242));
    assert_eq!(
        manager.build_id.as_deref(),
        Some(picot_host::BUILD_ID),
        "the record carries the app's build identity"
    );

    // Now stop it: the reply is accepted, then the drain removes the files.
    socket
        .send(Message::Text(
            json!({"type": "admin_request", "id": "r1", "op": "daemon.stop", "args": {}})
                .to_string(),
        ))
        .await
        .expect("stop sent");
    let response = next_json(&mut socket).await;
    assert_eq!(response["result"], json!({"accepted": true}), "{response}");

    let instance_path = fixture.instance_path();
    let lock_path = fixture.lock_path();
    wait_for(
        Duration::from_secs(10),
        || !instance_path.exists() && !lock_path.exists(),
        "the drain to remove the instance file and the lock",
    )
    .await;

    assert!(
        steps.steps().contains(&DrainStep::DrainFiles),
        "the removal is the drain's file step: {:?}",
        steps.steps()
    );
    assert!(
        instance::read(&instance_path).is_none(),
        "a graceful exit leaves no instance file behind"
    );
}

/// The dead-attachment path clears the manager record too (plan D11): the app
/// died, the daemon lives on, and the next app must be able to adopt.
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_attachment_clears_the_manager_record() {
    let fixture = Fixture::boot().await;
    let (socket, _welcome) = fixture.hello(5150).await;
    assert!(fixture.admin.has_manager());
    assert!(
        instance::read(&fixture.instance_path())
            .unwrap()
            .manager
            .is_some(),
        "the grant published the record"
    );

    // Kill the app abruptly: the socket closes without any goodbye frame.
    drop(socket);
    wait_for(
        Duration::from_secs(10),
        || {
            instance::read(&fixture.instance_path())
                .map(|file| file.manager.is_none())
                .unwrap_or(true)
        },
        "the dead attachment to clear the manager record",
    )
    .await;

    // The daemon is still there, adoptable: a new app's hello succeeds.
    assert!(!fixture.admin.has_manager(), "the lease is free again");
    let (mut socket, welcome) = fixture.hello(6161).await;
    assert_eq!(
        welcome["epoch"].as_u64(),
        Some(2),
        "adoption after a death increments the epoch"
    );
    assert_eq!(
        instance::read(&fixture.instance_path())
            .unwrap()
            .manager
            .and_then(|manager| manager.pid),
        Some(6161)
    );

    socket
        .send(Message::Text(
            json!({"type": "admin_request", "id": "r1", "op": "daemon.stop", "args": {}})
                .to_string(),
        ))
        .await
        .expect("stop sent");
    let _ = next_json(&mut socket).await;
    fixture.state.shutdown_graceful("test").await;
}
