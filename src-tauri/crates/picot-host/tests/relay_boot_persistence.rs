// ABOUTME: Relay boot persistence (plan T6 slice 1): a persisted `relay.enabled`
// ABOUTME: true auto-starts the transport at daemon boot with the same identity.

//! This lives in its own test target because the second boot must dial the
//! mock relay through the production start plan, which reads
//! `PICOT_RELAY_ENDPOINT` and `PICOT_RELAY_TLS` from the process environment
//! (spec §4 dev override surface). Mutating env from a shared target would
//! race parallel tests; a dedicated process cannot.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use picot_host::daemon::admin::{AdminConfig, AdminServer, ADMIN_WS_PATH};
use picot_host::daemon::app_surface::{AppSurface, PaneOp, SurfaceProgress};
use picot_host::daemon::{DaemonConfig, DaemonState};
use picot_host::relay::controller::RelayStartPlan;
use picot_host::relay::transport::{TransportSeams, TransportTimings};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

#[path = "support/mock_relay.rs"]
mod mock_relay;

use mock_relay::MockRelay;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Every method refuses: this target never exercises the app surface, so any
/// call is a bug and must not be silently plausible.
#[derive(Default)]
struct NoSurface;

#[async_trait::async_trait]
impl AppSurface for NoSurface {
    async fn open_workspace_window(
        &self,
        _host_origin: &str,
        _target: &picot_host::runtime_coordinator::RuntimeTarget,
        _label: &str,
        _owner: &str,
        _capability: &str,
    ) -> Result<(), String> {
        Err("unused in the relay boot test".into())
    }

    async fn open_devtools(&self, _label: &str) -> Result<(), String> {
        Err("unused in the relay boot test".into())
    }

    async fn focus_window(&self, _label: &str) -> Result<(), String> {
        Err("unused in the relay boot test".into())
    }

    async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in the relay boot test".into())
    }

    async fn cancel_window_close(&self, _label: &str, _request_id: &str) -> Result<(), String> {
        Err("unused in the relay boot test".into())
    }

    async fn menu_state_changed(&self) -> Result<(), String> {
        Err("unused in the relay boot test".into())
    }

    async fn relaunch_app(&self) -> Result<(), String> {
        Err("unused in the relay boot test".into())
    }

    async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
        Err("unused in the relay boot test".into())
    }

    async fn pick_folder(&self) -> Result<Option<String>, String> {
        Err("unused in the relay boot test".into())
    }

    async fn pick_image_files(&self, _initial_dir: Option<String>) -> Result<Value, String> {
        Err("unused in the relay boot test".into())
    }

    async fn save_session_export(
        &self,
        _host_origin: &str,
        _export_url: &str,
        _file_name: &str,
        _initial_dir: Option<String>,
    ) -> Result<Value, String> {
        Err("unused in the relay boot test".into())
    }

    async fn check_for_update(&self) -> Result<Value, String> {
        Err("unused in the relay boot test".into())
    }

    async fn download_and_install_update(
        &self,
        _progress: SurfaceProgress,
    ) -> Result<Value, String> {
        Err("unused in the relay boot test".into())
    }
}

/// A minimal daemon fixture: `DaemonState` + admin channel, one temp dir.
async fn boot(data_dir: &std::path::Path) -> Arc<DaemonState> {
    let static_dir = data_dir.parent().unwrap().join("static");
    std::fs::create_dir_all(&static_dir).unwrap();
    std::fs::write(static_dir.join("index.html"), "<h1>relay boot</h1>").unwrap();
    let state = Arc::new(
        DaemonState::start(
            DaemonConfig {
                data_dir: data_dir.to_path_buf(),
                config_dir: data_dir.parent().unwrap().join("config"),
                static_dir,
                bundle_identifier: "com.palandata.picot.test".to_string(),
            },
            Arc::new(NoSurface),
        )
        .await
        .expect("the daemon host stack boots"),
    );
    let admin = AdminServer::start(AdminConfig::new("boot-test-token".to_string()))
        .await
        .expect("the admin listener binds");
    state.install_admin(admin);
    state
}

async fn attach(state: &Arc<DaemonState>) -> Client {
    let admin = state.admin().expect("admin attached");
    let url = format!(
        "ws://{}{ADMIN_WS_PATH}",
        admin.origin().trim_start_matches("http://")
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("a real admin client connects");
    send_json(
        &mut socket,
        json!({
            "type": "admin_hello",
            "adminToken": "boot-test-token",
            "role": "manager",
            "appVersion": "0.5.0",
            "buildId": "test-build",
            "pid": 4242,
        }),
    )
    .await;
    socket
}

async fn send_json(socket: &mut Client, frame: Value) {
    socket
        .send(Message::Text(frame.to_string()))
        .await
        .expect("frame sends");
}

async fn request(socket: &mut Client, id: &str, op: &str, args: Value) -> Value {
    send_json(
        socket,
        json!({"type": "admin_request", "id": id, "op": op, "args": args}),
    )
    .await;
    loop {
        let message = socket.next().await.expect("socket open").expect("frame ok");
        if let Message::Text(text) = message {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if value["type"] == "admin_result" && value["id"] == id {
                    return value;
                }
            }
        }
    }
}

fn fast_seams() -> TransportSeams {
    TransportSeams {
        timings: TransportTimings {
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(2),
            ping_interval: Duration::from_millis(50),
            stale_timeout: Duration::from_millis(400),
            ready_timeout: Duration::from_millis(400),
            handshake_timeout: Duration::from_secs(2),
            data_open_timeout: Duration::from_secs(2),
            e2ee_handshake_timeout: Duration::from_secs(5),
        },
        sleep: Arc::new(|duration: Duration| Box::pin(tokio::time::sleep(duration))),
        connector: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_persisted_enabled_flag_auto_starts_the_transport_at_boot() {
    let relay = MockRelay::start().await;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join(format!("relay-boot-{nonce}"));
    let data_dir = root.join("data");

    // Phase 1: enable through the controller (the same write the admin op
    // performs), wait for online, then stop only the transport — the enabled
    // preference stays persisted exactly as a daemon restart would leave it.
    let first = boot(&data_dir).await;
    first.relay.set_start_plan_for_tests(RelayStartPlan {
        endpoint: relay.addr().to_string(),
        use_tls: false,
        seams: fast_seams(),
    });
    first
        .relay
        .set_enabled(true)
        .await
        .expect("first enable starts the transport");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let first_server_id = loop {
        let status = first.relay.status().expect("status reads");
        if status.state == "online" {
            break status.server_id.expect("identity minted");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "first daemon never reached online: {:?}",
            status.state
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    first.relay.stop_transport().await;
    drop(first);

    // Phase 2: a fresh daemon on the same data dir, no test plan override —
    // the production start plan picks up the mock through the dev env vars,
    // proving boot reads the persisted flag and auto-starts.
    let saved_endpoint = std::env::var("PICOT_RELAY_ENDPOINT").ok();
    let saved_tls = std::env::var("PICOT_RELAY_TLS").ok();
    std::env::set_var("PICOT_RELAY_ENDPOINT", relay.addr().to_string());
    std::env::set_var("PICOT_RELAY_TLS", "0");
    let second = boot(&data_dir).await;
    let mut socket = attach(&second).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        let response = request(&mut socket, "r-boot", "relay.status", json!({})).await;
        assert_eq!(response["ok"], true, "{response}");
        if response["result"]["state"] == "online" {
            break response["result"].clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the rebooted daemon never reached online: {response}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(status["enabled"], true, "{status}");
    assert_eq!(
        status["endpoint"],
        relay.addr().to_string(),
        "the production plan honors the dev endpoint override"
    );
    assert_eq!(
        status["serverId"].as_str().expect("identity survives"),
        first_server_id,
        "the identity is load-or-create, never re-keyed across restarts"
    );
    drop(socket);
    drop(second);
    match saved_endpoint {
        Some(value) => std::env::set_var("PICOT_RELAY_ENDPOINT", value),
        None => std::env::remove_var("PICOT_RELAY_ENDPOINT"),
    }
    match saved_tls {
        Some(value) => std::env::set_var("PICOT_RELAY_TLS", value),
        None => std::env::remove_var("PICOT_RELAY_TLS"),
    }
}
