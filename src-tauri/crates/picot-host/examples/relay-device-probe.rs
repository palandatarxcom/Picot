// ABOUTME: One-shot probe: reuse a paired device token against a real relay and
// ABOUTME: print every frame of the E2EE + data-plane exchange, step by step.

//! Manual probe over the real relay, using an already-paired device token.
//!
//! ```sh
//! cargo run -p picot-host --example relay-device-probe -- \
//!   --endpoint paseo.021521.xyz:443 \
//!   --server-id <uuid> \
//!   --daemon-public-key <base64> \
//!   --device-token <base64-ish token> \
//!   --device-id <id> \
//!   --workspace-id <wid> \
//!   --session-id <header id> \
//!   --session-file <remote path>
//! ```
//!
//! It performs the same handshake the desktop bridge performs: `e2ee_hello` →
//! `e2ee_ready`, then a sealed `hello` carrying the device token, then one
//! `read_session_messages` data request. Every frame is printed.

use std::io::Write;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use picot_host::relay::e2ee;

const PRE_HELLO_DELAY: Duration = Duration::from_millis(300);
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
const HELLO_RESEND_AFTER: Duration = Duration::from_millis(1500);
const HELLO_MAX_SENDS: u32 = 6;
const HANDSHAKE_BUDGET: Duration = Duration::from_secs(25);

struct Opts {
    endpoint: String,
    server_id: String,
    daemon_public_key: String,
    device_token: String,
    device_id: String,
    workspace_id: String,
    session_id: String,
    session_file: Option<String>,
    prepare_cwd: Option<String>,
    list_sessions: bool,
    list_workspaces: bool,
    bootstrap_target: bool,
    list_instances: bool,
}

fn parse_args() -> Result<Opts, String> {
    let mut endpoint = None;
    let mut server_id = None;
    let mut daemon_public_key = None;
    let mut device_token = None;
    let mut device_id = None;
    let mut workspace_id = None;
    let mut session_id = None;
    let mut session_file = None;
    let mut prepare_cwd = None;
    let mut list_sessions = false;
    let mut list_workspaces = false;
    let mut bootstrap_target = false;
    let mut list_instances = false;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut take = |slot: &mut Option<String>| -> Result<(), String> {
            *slot = Some(
                it.next()
                    .ok_or_else(|| format!("missing value for {flag}"))?,
            );
            Ok(())
        };
        match flag.as_str() {
            "--endpoint" => take(&mut endpoint)?,
            "--server-id" => take(&mut server_id)?,
            "--daemon-public-key" => take(&mut daemon_public_key)?,
            "--device-token" => take(&mut device_token)?,
            "--device-id" => take(&mut device_id)?,
            "--workspace-id" => take(&mut workspace_id)?,
            "--session-id" => take(&mut session_id)?,
            "--session-file" => take(&mut session_file)?,
            "--prepare-cwd" => take(&mut prepare_cwd)?,
            "--list-sessions" => {
                list_sessions = true;
            }
            "--list-workspaces" => {
                list_workspaces = true;
            }
            "--bootstrap" => {
                bootstrap_target = true;
            }
            "--instances" => {
                list_instances = true;
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Opts {
        endpoint: endpoint.ok_or("--endpoint is required")?,
        server_id: server_id.ok_or("--server-id is required")?,
        daemon_public_key: daemon_public_key.ok_or("--daemon-public-key is required")?,
        device_token: device_token.ok_or("--device-token is required")?,
        device_id: device_id.ok_or("--device-id is required")?,
        workspace_id: workspace_id.unwrap_or_default(),
        session_id: session_id.unwrap_or_default(),
        session_file,
        prepare_cwd,
        list_sessions,
        list_workspaces,
        bootstrap_target,
        list_instances,
    })
}

fn seal(shared: &[u8; 32], plaintext: &str) -> String {
    BASE64_STANDARD.encode(e2ee::seal(shared, plaintext.as_bytes()))
}

fn open(shared: &[u8; 32], wire: &str) -> Result<String, String> {
    let bundle = BASE64_STANDARD
        .decode(wire)
        .map_err(|_| "relay frame is not valid base64".to_string())?;
    let plaintext = e2ee::open(shared, &bundle)?;
    String::from_utf8(plaintext).map_err(|_| "relay plaintext is not UTF-8".to_string())
}

#[tokio::main]
async fn main() {
    let opts = match parse_args() {
        Ok(opts) => opts,
        Err(error) => {
            eprintln!("relay-device-probe: {error}");
            std::process::exit(2);
        }
    };

    let scheme = "wss";
    let server_id = utf8_percent_encode(&opts.server_id, NON_ALPHANUMERIC);
    let url = format!(
        "{scheme}://{}/ws?serverId={server_id}&role=client&v=2",
        opts.endpoint
    );
    eprintln!("[probe] dial {url}");
    let request =
        match tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
            url.as_str(),
        ) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("[probe] bad url: {error}");
                std::process::exit(2);
            }
        };
    let dial = tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        None::<tokio_tungstenite::Connector>,
    );
    let (mut stream, response) = match tokio::time::timeout(STEP_TIMEOUT, dial).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(error)) => {
            eprintln!("[probe] dial failed: {error}");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("[probe] dial timed out");
            std::process::exit(1);
        }
    };
    eprintln!("[probe] HTTP {}", response.status());
    if let Some(headers) = response.headers().get("sec-websocket-protocol") {
        eprintln!("[probe] subprotocol: {headers:?}");
    }

    // ---- E2EE handshake: e2ee_hello -> e2ee_ready ----
    let secret = crypto_box::SecretKey::generate(&mut rand::rngs::OsRng);
    let public_key_b64 = BASE64_STANDARD.encode(secret.public_key().as_bytes());
    let shared = match e2ee::derive_shared_key(&secret.to_bytes(), &opts.daemon_public_key) {
        Ok(shared) => shared,
        Err(error) => {
            eprintln!("[probe] derive_shared_key failed: {error}");
            std::process::exit(1);
        }
    };
    let hello = format!("{{\"type\":\"e2ee_hello\",\"key\":\"{public_key_b64}\"}}");
    tokio::time::sleep(PRE_HELLO_DELAY).await;
    eprintln!("[probe] SENT e2ee_hello");
    stream
        .send(Message::Text(hello))
        .await
        .expect("send e2ee_hello");

    let mut sent = 1u32;
    let deadline = tokio::time::Instant::now() + HANDSHAKE_BUDGET;
    let ready = loop {
        let resend_at = tokio::time::Instant::now() + HELLO_RESEND_AFTER;
        let wait_until = if sent < HELLO_MAX_SENDS && resend_at < deadline {
            resend_at
        } else {
            deadline
        };
        match tokio::time::timeout_at(wait_until, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                eprintln!("[probe] RECV {text}");
                if text == "{\"type\":\"e2ee_ready\"}" {
                    break true;
                }
                eprintln!("[probe] unexpected text before e2ee_ready");
                std::process::exit(1);
            }
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(Some(Ok(other))) => {
                eprintln!("[probe] unexpected non-text frame: {other:?}");
                std::process::exit(1);
            }
            Ok(Some(Err(error))) => {
                eprintln!("[probe] socket error: {error}");
                std::process::exit(1);
            }
            Ok(None) => {
                eprintln!("[probe] relay closed before e2ee_ready");
                std::process::exit(1);
            }
            Err(_) => {
                if tokio::time::Instant::now() >= deadline {
                    eprintln!("[probe] timed out waiting for e2ee_ready");
                    std::process::exit(1);
                }
                if sent < HELLO_MAX_SENDS {
                    sent += 1;
                    eprintln!("[probe] resend e2ee_hello ({sent})");
                    let hello = format!("{{\"type\":\"e2ee_hello\",\"key\":\"{public_key_b64}\"}}");
                    stream
                        .send(Message::Text(hello))
                        .await
                        .expect("resend e2ee_hello");
                    continue;
                }
                eprintln!("[probe] timed out waiting for e2ee_ready");
                std::process::exit(1);
            }
        }
    };
    if !ready {
        std::process::exit(1);
    }
    eprintln!("[probe] E2EE OK");

    // ---- sealed application hello carrying the device token ----
    let app_hello = json!({
        "type": "hello",
        "protocolVersion": 2,
        "clientType": "remote",
        "clientId": opts.device_id,
        "deviceId": opts.device_id,
        "deviceToken": opts.device_token,
    })
    .to_string();
    let sealed_hello = seal(&shared, &app_hello);
    eprintln!("[probe] SENT sealed hello (deviceToken)");
    stream
        .send(Message::Text(sealed_hello))
        .await
        .expect("send sealed hello");

    let ack_wire = match tokio::time::timeout(STEP_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => text,
        Ok(Some(Ok(other))) => {
            eprintln!("[probe] unexpected frame for hello_ack: {other:?}");
            std::process::exit(1);
        }
        Ok(Some(Err(error))) => {
            eprintln!("[probe] socket error: {error}");
            std::process::exit(1);
        }
        Ok(None) => {
            eprintln!("[probe] relay closed before hello_ack");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("[probe] timed out waiting for hello_ack");
            std::process::exit(1);
        }
    };
    let ack: Value = match open(&shared, &ack_wire) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[probe] hello_ack is not JSON: {error}");
                std::process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("[probe] cannot open hello_ack: {error}");
            std::process::exit(1);
        }
    };
    eprintln!("[probe] RECV hello_ack: {ack}");
    if ack.get("type").and_then(Value::as_str) != Some("hello_ack") {
        eprintln!("[probe] hello rejected");
        std::process::exit(1);
    }

    let mut _snapshot_target: Option<Value> = None;

    // ---- optional prelude: workspace.list (host_request, no workspaceId) ----
    // Device-path prepare: spawns (or rebinds) the runtime on the remote for
    // the named cwd + session file, exactly like the local click does. The
    // subsequent instances/bootstrap steps then observe what happened to it.
    if let (Some(cwd), Some(file)) = (opts.prepare_cwd.as_ref(), opts.session_file.as_ref()) {
        let prepare_frame = json!({
            "type": "host_request",
            "requestId": "probe-prepare",
            "operation": "workspace_target_prepare",
            "args": {
                "targetCwd": cwd,
                "sessionPath": file,
            },
        });
        eprintln!("[probe] SENT {prepare_frame}");
        let sealed_prepare = seal(&shared, &prepare_frame.to_string());
        stream
            .send(Message::Text(sealed_prepare))
            .await
            .expect("send workspace_target_prepare");
        match tokio::time::timeout(STEP_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let plain = open(&shared, &text).unwrap_or_default();
                eprintln!("[probe] RECV prepare: {plain}");
            }
            Ok(Some(Ok(other))) => {
                eprintln!("[probe] unexpected frame for prepare: {other:?}");
            }
            Ok(Some(Err(error))) => eprintln!("[probe] socket error: {error}"),
            Ok(None) => eprintln!("[probe] relay closed before prepare"),
            Err(_) => eprintln!("[probe] timed out waiting for prepare"),
        }
    }

    // Navigation-face runtime list (the device may call it): shows every live
    // runtime with the session id the daemon registry carries for it.
    if opts.list_instances {
        let list_frame = json!({
            "type": "host_request",
            "requestId": "probe-instances",
            "operation": "runtime_instances",
            "args": {},
        });
        eprintln!("[probe] SENT {list_frame}");
        let sealed_list = seal(&shared, &list_frame.to_string());
        stream
            .send(Message::Text(sealed_list))
            .await
            .expect("send runtime_instances");
        match tokio::time::timeout(STEP_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let plain = open(&shared, &text).unwrap_or_default();
                eprintln!("[probe] RECV runtime_instances: {plain}");
            }
            Ok(Some(Ok(other))) => {
                eprintln!("[probe] unexpected frame for runtime_instances: {other:?}");
            }
            Ok(Some(Err(error))) => eprintln!("[probe] socket error: {error}"),
            Ok(None) => eprintln!("[probe] relay closed before runtime_instances"),
            Err(_) => eprintln!("[probe] timed out waiting for runtime_instances"),
        }
    }

    let mut _snapshot_target: Option<Value> = None;

    // ---- optional prelude: workspace.list (host_request, no workspaceId) ----
    // The device may call the navigation face without naming a workspace, so
    // the probe can discover the remote's own workspace ids by itself.
    if opts.list_workspaces {
        let list_frame = json!({
            "type": "host_request",
            "requestId": "probe-ws-list",
            "operation": "workspace.list",
            "args": {},
        });
        eprintln!("[probe] SENT {list_frame}");
        let sealed_list = seal(&shared, &list_frame.to_string());
        stream
            .send(Message::Text(sealed_list))
            .await
            .expect("send workspace.list");
        match tokio::time::timeout(STEP_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let plain = open(&shared, &text).unwrap_or_default();
                eprintln!("[probe] RECV workspace.list: {plain}");
            }
            Ok(Some(Ok(other))) => {
                eprintln!("[probe] unexpected frame for workspace.list: {other:?}");
            }
            Ok(Some(Err(error))) => eprintln!("[probe] socket error: {error}"),
            Ok(None) => eprintln!("[probe] relay closed before workspace.list"),
            Err(_) => eprintln!("[probe] timed out waiting for workspace.list"),
        }
    }

    // ---- optional: runtime_snapshot with the bootstrap target ----
    if opts.bootstrap_target {
        // The bootstrap reply carries the authoritative target; use its
        // instanceId for the snapshot (the "primary" placeholder fails).
        if let Some(target) = _snapshot_target.as_ref() {
            let snap_frame = json!({
                "type": "runtime_snapshot_request",
                "protocolVersion": 2,
                "requestId": "probe-snap",
                "target": target,
            });
            eprintln!("[probe] SENT snapshot: {snap_frame}");
            let sealed_snap = seal(&shared, &snap_frame.to_string());
            stream
                .send(Message::Text(sealed_snap))
                .await
                .expect("send runtime_snapshot_request");
            match tokio::time::timeout(STEP_TIMEOUT, stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let plain = open(&shared, &text).unwrap_or_default();
                    eprintln!("[probe] RECV snapshot: {}", &plain[..plain.len().min(600)]);
                }
                Ok(Some(Ok(other))) => {
                    eprintln!("[probe] unexpected frame for snapshot: {other:?}");
                }
                Ok(Some(Err(error))) => eprintln!("[probe] socket error: {error}"),
                Ok(None) => eprintln!("[probe] relay closed before snapshot"),
                Err(_) => eprintln!("[probe] timed out waiting for snapshot"),
            }
        }
    }

    // ---- optional prelude: list the remote workspace's sessions ----
    // The post-navigation boot chain resolves its target through the
    // restricted `workspace.bootstrap` op (fixed schema: exactly these five
    // fields, no protocolVersion). Probe it with the caller's real ids.
    if opts.bootstrap_target {
        let boot_frame = json!({
            "type": "data_request",
            "requestId": "probe-boot",
            "operation": "workspace.bootstrap",
            "workspaceId": opts.workspace_id,
            "sessionId": opts.session_id,
        });
        eprintln!("[probe] SENT {boot_frame}");
        let sealed_boot = seal(&shared, &boot_frame.to_string());
        stream
            .send(Message::Text(sealed_boot))
            .await
            .expect("send workspace.bootstrap");
        match tokio::time::timeout(STEP_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let plain = open(&shared, &text).unwrap_or_default();
                eprintln!("[probe] RECV workspace.bootstrap: {plain}");
                // Capture the target for the snapshot step
                if let Ok(v) = serde_json::from_str::<Value>(&plain) {
                    _snapshot_target = v.get("target").cloned();
                }
            }
            Ok(Some(Ok(other))) => {
                eprintln!("[probe] unexpected frame for workspace.bootstrap: {other:?}");
            }
            Ok(Some(Err(error))) => eprintln!("[probe] socket error: {error}"),
            Ok(None) => eprintln!("[probe] relay closed before workspace.bootstrap"),
            Err(_) => eprintln!("[probe] timed out waiting for workspace.bootstrap"),
        }
    }

    // ---- optional: runtime_snapshot with the bootstrap target ----
    if opts.bootstrap_target {
        // The bootstrap reply carries the authoritative target; use its
        // instanceId for the snapshot (the "primary" placeholder fails).
        if let Some(target) = _snapshot_target.as_ref() {
            let snap_frame = json!({
                "type": "runtime_snapshot_request",
                "protocolVersion": 2,
                "requestId": "probe-snap",
                "target": target,
            });
            eprintln!("[probe] SENT snapshot: {snap_frame}");
            let sealed_snap = seal(&shared, &snap_frame.to_string());
            stream
                .send(Message::Text(sealed_snap))
                .await
                .expect("send runtime_snapshot_request");
            match tokio::time::timeout(STEP_TIMEOUT, stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let plain = open(&shared, &text).unwrap_or_default();
                    eprintln!("[probe] RECV snapshot: {}", &plain[..plain.len().min(600)]);
                }
                Ok(Some(Ok(other))) => {
                    eprintln!("[probe] unexpected frame for snapshot: {other:?}");
                }
                Ok(Some(Err(error))) => eprintln!("[probe] socket error: {error}"),
                Ok(None) => eprintln!("[probe] relay closed before snapshot"),
                Err(_) => eprintln!("[probe] timed out waiting for snapshot"),
            }
        }
    }

    // ---- optional prelude: list the remote workspace's sessions ----
    // The remote's own ids and file paths come from the remote, so the
    // operator can read them instead of guessing.
    if opts.list_sessions {
        let list_frame = json!({
            "type": "data_request",
            "protocolVersion": 2,
            "requestId": "probe-list",
            "operation": "workspace_sessions",
            "workspaceId": opts.workspace_id,
        });
        eprintln!("[probe] SENT {list_frame}");
        let sealed_list = seal(&shared, &list_frame.to_string());
        stream
            .send(Message::Text(sealed_list))
            .await
            .expect("send workspace_sessions");
        match tokio::time::timeout(STEP_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let plain = open(&shared, &text).unwrap_or_default();
                eprintln!("[probe] RECV workspace_sessions: {plain}");
            }
            Ok(Some(Ok(other))) => {
                eprintln!("[probe] unexpected frame for workspace_sessions: {other:?}");
            }
            Ok(Some(Err(error))) => eprintln!("[probe] socket error: {error}"),
            Ok(None) => eprintln!("[probe] relay closed before workspace_sessions"),
            Err(_) => eprintln!("[probe] timed out waiting for workspace_sessions"),
        }
    }

    // ---- one data request: read_session_messages ----
    let mut request_frame = json!({
        "type": "data_request",
        "protocolVersion": 2,
        "requestId": "probe-1",
        "operation": "read_session_messages",
        "workspaceId": opts.workspace_id,
        "sessionId": opts.session_id,
    });
    if let Some(file) = opts.session_file.as_ref() {
        request_frame["sessionFile"] = json!(file);
    }
    eprintln!("[probe] SENT {request_frame}");
    let sealed_request = seal(&shared, &request_frame.to_string());
    stream
        .send(Message::Text(sealed_request))
        .await
        .expect("send data request");

    match tokio::time::timeout(STEP_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            let plain = open(&shared, &text).unwrap_or_else(|error| {
                eprintln!("[probe] cannot open data response: {error}");
                std::process::exit(1);
            });
            let value: Value = serde_json::from_str(&plain).unwrap_or(Value::Null);
            let count = value
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            eprintln!("[probe] RECV data_response (messages={count})");
            eprintln!("{value}");
            let _ = std::io::stdout().flush();
        }
        Ok(Some(Ok(other))) => {
            eprintln!("[probe] unexpected frame for data response: {other:?}");
            std::process::exit(1);
        }
        Ok(Some(Err(error))) => {
            eprintln!("[probe] socket error: {error}");
            std::process::exit(1);
        }
        Ok(None) => {
            eprintln!("[probe] relay closed before data response");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("[probe] timed out waiting for data response");
            std::process::exit(1);
        }
    }
}
