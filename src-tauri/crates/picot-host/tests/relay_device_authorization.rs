// ABOUTME: Relay phase-2.5 Task 4: per-device view state with detach- and
// ABOUTME: restart-durable authorization (contract 1 / D1) over real WS frames.

//! Semantics written down before the implementation (契约 1, D1; R2-02):
//!
//! - **device view** = each paired device's own view state (workspace, session
//!   binding, generation) persisted in the relay-reserved MetadataStore
//!   namespace `relay.device_view.<deviceId>`. It is fully orthogonal to the
//!   window-owner registry: no window binding, navigation permit,
//!   `focusedExisting` or rebind is read or written to resolve a device
//!   target (device navigation lands in plan T5).
//! - A remote principal's runtime/data-plane target resolution goes through its
//!   device view **plus the daemon's resident runtime state**. It does not
//!   require the pairing window's owner to be currently Registered, so the
//!   device keeps working after the window detaches (D1) and after a daemon
//!   restart (disk-session reopen only — no live-turn continuation, R2-12).
//! - Desktop (window-owner) resolution is untouched: a desktop session still
//!   resolves through its handshake binding + live registry snapshot.
//! - Revocation keeps its kick semantics (plan T10); revoking also drops the
//!   device view, so a re-paired device starts from a fresh seed.
//!
//! Every await here is timeout-guarded: a hang is a failure, not a slow test.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use picot_host::host_server::HostServer;
use picot_host::metadata_store::{MetadataStore, SharedMetadataStore};
use picot_host::native_pi_manager::NativePiManager;
use picot_host::remote_auth::{DeviceView, RemoteAuth};
use picot_host::runtime_coordinator::RuntimeTarget;
use picot_host::window_owner::{
    OwnerId, OwnerWorkspaceSnapshot, TemporaryKind, WindowOwnerRegistry,
};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const STEP: Duration = Duration::from_secs(20);

fn temp_dir(tag: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "picot-device-auth-{tag}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(temp.join("public")).unwrap();
    fs::write(temp.join("public").join("index.html"), "Picot").unwrap();
    fs::create_dir_all(temp.join("ws")).unwrap();
    temp
}

fn open_store(temp: &std::path::Path) -> SharedMetadataStore {
    Arc::new(Mutex::new(
        MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
    ))
}

/// A resident runtime the remote plane can be authorized against: an
/// in-memory Pi bridge answering every dispatched request, owned by `owner`
/// and spawned at `generation` (the generation the owner held when the runtime
/// started — what survives a detach).
fn seed_runtime(
    runtimes: &NativePiManager,
    owner: &OwnerId,
    workspace_id: &str,
    session_id: &str,
    instance_id: &str,
    generation: u64,
) {
    let target = RuntimeTarget::with_owner(
        workspace_id,
        session_id,
        instance_id,
        owner.as_str(),
        generation,
    );
    let mut fake = runtimes.register_in_memory(target).unwrap();
    tokio::spawn(async move {
        while let Some(request) = fake.read_request().await {
            let _ = fake
                .write_frame(json!({
                    "id": request["id"],
                    "type": "response",
                    "success": true,
                    "data": { "sessionId": "seeded" },
                }))
                .await;
        }
    });
}

fn seed_window(
    registry: &Arc<WindowOwnerRegistry>,
    temp: &std::path::Path,
    workspace_id: &str,
) -> (OwnerId, String) {
    registry
        .create_owner_with_workspace(
            "device-auth-window".into(),
            temp.join("ws").canonicalize().unwrap(),
            0,
            "http://127.0.0.1:1".into(),
            Some(workspace_id.into()),
            TemporaryKind::DefaultStartup,
        )
        .unwrap()
}

async fn start_host(
    temp: &std::path::Path,
    runtimes: NativePiManager,
    auth: Arc<Mutex<RemoteAuth>>,
    metadata: SharedMetadataStore,
    registry: &Arc<WindowOwnerRegistry>,
) -> HostServer {
    let host = tokio::time::timeout(
        STEP,
        HostServer::start(
            temp.join("public"),
            runtimes,
            Arc::clone(&auth),
            Arc::clone(&metadata),
        ),
    )
    .await
    .expect("host start timed out")
    .unwrap();
    host.set_owner_registry(Arc::clone(registry));
    host
}

fn pair_device(auth: &Arc<Mutex<RemoteAuth>>, owner: &OwnerId, device_id: &str) -> String {
    let pairing = auth.lock().unwrap().create_pairing(1, Some(owner.as_str()));
    auth.lock()
        .unwrap()
        .exchange(&pairing.token, device_id, 2)
        .expect("owner-stamped exchange")
}

/// Device hello carrying an existing device token; returns the ack.
async fn connect_device(host: &HostServer, client_id: &str, token: &str) -> (Client, Value) {
    let (mut socket, _) = tokio::time::timeout(
        STEP,
        tokio_tungstenite::connect_async(host.origin().replacen("http://", "ws://", 1) + "/v2/ws"),
    )
    .await
    .expect("device dial timed out")
    .unwrap();
    socket
        .send(Message::Text(
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "remote",
                "clientId": client_id, "deviceToken": token
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let ack = next_frame(&mut socket).await;
    (socket, ack)
}

/// Desktop hello through the real capability path (the window's own class).
async fn connect_desktop(host: &HostServer, client_id: &str, capability: &str) -> (Client, Value) {
    let (mut socket, _) = tokio::time::timeout(
        STEP,
        tokio_tungstenite::connect_async(host.origin().replacen("http://", "ws://", 1) + "/v2/ws"),
    )
    .await
    .expect("desktop dial timed out")
    .unwrap();
    socket
        .send(Message::Text(
            json!({
                "type": "hello", "protocolVersion": 2, "clientType": "desktop",
                "clientId": client_id, "desktopCapability": capability
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let ack = next_frame(&mut socket).await;
    (socket, ack)
}

async fn next_frame(socket: &mut Client) -> Value {
    let message = tokio::time::timeout(STEP, socket.next())
        .await
        .expect("frame timed out")
        .expect("socket closed")
        .unwrap();
    serde_json::from_str(&message.into_text().unwrap()).unwrap()
}

async fn send_frame(socket: &mut Client, frame: Value) -> Value {
    socket.send(Message::Text(frame.to_string())).await.unwrap();
    next_frame(socket).await
}

fn wire_target(workspace_id: &str, session_id: &str, instance_id: &str) -> Value {
    json!({
        "workspaceId": workspace_id, "sessionId": session_id, "instanceId": instance_id
    })
}

async fn runtime_request(socket: &mut Client, request_id: &str, target: Value) -> Value {
    send_frame(
        socket,
        json!({
            "type": "runtime_request", "requestId": request_id, "target": target,
            "idempotencyKey": format!("{request_id}-key"),
            "command": { "type": "get_state" }
        }),
    )
    .await
}

async fn workspace_info(socket: &mut Client, request_id: &str, workspace_id: &str) -> Value {
    send_frame(
        socket,
        json!({
            "type": "data_request", "requestId": request_id,
            "operation": "workspace_info", "workspaceId": workspace_id
        }),
    )
    .await
}

/// 契约 1 / D1: the pairing window detaches and the device keeps its data
/// plane — subscribe, runtime command, and workspace-bound data op all resolve
/// through the device's own view instead of the window-owner registry.
#[tokio::test]
async fn detached_window_leaves_the_device_data_plane_authorized() {
    let temp = temp_dir("detach");
    let metadata = open_store(&temp);
    let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
    let runtimes = NativePiManager::new(8);
    let registry = Arc::new(WindowOwnerRegistry::default());
    let (owner, _capability) = seed_window(&registry, &temp, "ws-one");
    seed_runtime(
        &runtimes,
        &owner,
        "ws-one",
        "session-one",
        "instance-one",
        0,
    );

    let host = start_host(
        &temp,
        runtimes,
        Arc::clone(&auth),
        Arc::clone(&metadata),
        &registry,
    )
    .await;
    let token = pair_device(&auth, &owner, "phone-a");
    let (mut device, ack) = connect_device(&host, "device-a", &token).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");

    // The seed landed at hello: the device view names the pairing owner's
    // workspace and the generation the runtime was spawned under.
    assert_eq!(
        auth.lock().unwrap().device_view("phone-a").unwrap(),
        Some(DeviceView {
            workspace_id: "ws-one".into(),
            session_id: None,
            generation: 0,
        })
    );

    // The host app quits: the window's registration is gone, runtimes stay.
    registry.detach_owner(&owner);
    assert!(matches!(
        registry.owner_current_workspace(&owner),
        OwnerWorkspaceSnapshot::NoWorkspace
    ));

    let target = wire_target("ws-one", "session-one", "instance-one");
    let subscribed = send_frame(
        &mut device,
        json!({ "type": "runtime_subscribe", "requestId": "sub-1", "target": target }),
    )
    .await;
    assert_eq!(subscribed["type"], "runtime_subscribed", "{subscribed}");

    let reply = runtime_request(&mut device, "cmd-1", target).await;
    assert_eq!(reply["type"], "runtime_response", "{reply}");
    assert_eq!(reply["requestId"], "cmd-1", "{reply}");

    let data = workspace_info(&mut device, "data-1", "ws-one").await;
    assert_eq!(data["requestId"], "data-1", "{data}");
    assert_ne!(data["error"]["code"], "unauthorized_target", "{data}");

    // A workspace outside the device view stays refused — the view is a
    // binding, not a blanket grant.
    let other = workspace_info(&mut device, "data-2", "ws-other").await;
    assert_eq!(other["error"]["code"], "unauthorized_target", "{other}");

    // Routing-layer direct check (`host_router.rs:346-370`), asserted over the
    // real frame path rather than through `admits()`: the management plane is
    // closed to the device class even with its data plane fully authorized.
    let management = send_frame(
        &mut device,
        json!({
            "type": "host_request", "requestId": "manage-1",
            "operation": "preference_get", "args": {}
        }),
    )
    .await;
    assert_eq!(
        management["error"]["code"], "forbidden_class",
        "{management}"
    );

    let _ = device.close(None).await;
    host.stop();
    let _ = fs::remove_dir_all(temp);
}

/// 契约 1 / D1 / R2-12: a daemon restart reopens the disk session from the same
/// database — the device authenticates without re-pairing and its view (and
/// therefore its authorization) is still there. No claim about live turns.
#[tokio::test]
async fn daemon_restart_reopens_the_device_session_from_disk() {
    let temp = temp_dir("restart");
    let metadata = open_store(&temp);
    let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
    let runtimes = NativePiManager::new(8);
    let registry = Arc::new(WindowOwnerRegistry::default());
    let (owner, _capability) = seed_window(&registry, &temp, "ws-one");
    seed_runtime(
        &runtimes,
        &owner,
        "ws-one",
        "session-one",
        "instance-one",
        0,
    );

    let host = start_host(
        &temp,
        runtimes,
        Arc::clone(&auth),
        Arc::clone(&metadata),
        &registry,
    )
    .await;
    let token = pair_device(&auth, &owner, "phone-a");
    let (mut device, ack) = connect_device(&host, "device-a", &token).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");
    let _ = device.close(None).await;
    host.stop();

    // Daemon restart: the process state is gone, the database is not.
    drop(auth);
    drop(metadata);
    let metadata = open_store(&temp);
    let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));

    // Disk session: the token still authenticates to the same principal.
    let principal = auth
        .lock()
        .unwrap()
        .authenticate(&token)
        .unwrap()
        .expect("device token survives restart");
    assert_eq!(principal.device_id, "phone-a");
    assert_eq!(principal.owner_id.as_deref(), Some(owner.as_str()));
    assert_eq!(
        auth.lock().unwrap().device_view("phone-a").unwrap(),
        Some(DeviceView {
            workspace_id: "ws-one".into(),
            session_id: None,
            generation: 0,
        })
    );

    // A fresh daemon: no window owner registered, a resident runtime owned by
    // the pairing owner (whatever re-opened the workspace after the restart).
    let runtimes = NativePiManager::new(8);
    let registry = Arc::new(WindowOwnerRegistry::default());
    seed_runtime(
        &runtimes,
        &owner,
        "ws-one",
        "session-one",
        "instance-one",
        0,
    );
    let host = start_host(
        &temp,
        runtimes,
        Arc::clone(&auth),
        Arc::clone(&metadata),
        &registry,
    )
    .await;
    let (mut device, ack) = connect_device(&host, "device-a-restarted", &token).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");
    let reply = runtime_request(
        &mut device,
        "cmd-1",
        wire_target("ws-one", "session-one", "instance-one"),
    )
    .await;
    assert_eq!(reply["type"], "runtime_response", "{reply}");
    assert_eq!(reply["requestId"], "cmd-1", "{reply}");

    let _ = device.close(None).await;
    host.stop();
    let _ = fs::remove_dir_all(temp);
}

/// 契约 1: device view and window binding are orthogonal in both directions —
/// the device's view writes touch no window state, and the window's workspace
/// switch neither changes the device's view nor revokes the device.
#[tokio::test]
async fn device_view_and_window_binding_are_orthogonal() {
    let temp = temp_dir("orthogonal");
    let metadata = open_store(&temp);
    let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
    let runtimes = NativePiManager::new(8);
    let registry = Arc::new(WindowOwnerRegistry::default());
    let (owner, _capability) = seed_window(&registry, &temp, "ws-one");
    seed_runtime(
        &runtimes,
        &owner,
        "ws-one",
        "session-one",
        "instance-one",
        0,
    );

    let host = start_host(
        &temp,
        runtimes,
        Arc::clone(&auth),
        Arc::clone(&metadata),
        &registry,
    )
    .await;
    let token = pair_device(&auth, &owner, "phone-a");
    let (mut device, ack) = connect_device(&host, "device-a", &token).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");

    // Direction 1: the device's own view state leaves every window authority
    // field untouched (binding, generation, pending permit, transition).
    let before = registry.owner_current_workspace(&owner);
    let before_generation = registry.current_workspace_generation(&owner);
    let before_permit = registry.pending_permit_id(&owner);
    let before_pending_origin = registry.pending_target_origin(&owner);
    auth.lock()
        .unwrap()
        .store_device_view(
            "phone-a",
            &DeviceView {
                workspace_id: "ws-one".into(),
                session_id: Some("session-one".into()),
                generation: 0,
            },
        )
        .unwrap();
    assert_eq!(registry.owner_current_workspace(&owner), before);
    assert_eq!(
        registry.current_workspace_generation(&owner),
        before_generation
    );
    assert_eq!(registry.pending_permit_id(&owner), before_permit);
    assert_eq!(
        registry.pending_target_origin(&owner),
        before_pending_origin
    );
    assert!(!registry.workspace_transition_in_progress(&owner));

    // Direction 2: the local window switches workspace (ws-one → ws-two, the
    // registry path a desktop transition takes). The target cwd must differ
    // from the current one — a same-cwd navigation is a session move, not a
    // workspace transition.
    fs::create_dir_all(temp.join("ws-two")).unwrap();
    let target_cwd = temp.join("ws-two").canonicalize().unwrap();
    let generation = registry
        .begin_workspace_transition(&owner, target_cwd.clone(), 3002)
        .unwrap();
    registry
        .prepare_navigation(
            &owner,
            3002,
            target_cwd,
            "http://127.0.0.1:3002".to_string(),
            Duration::from_secs(5),
        )
        .unwrap();
    registry
        .commit_workspace_transition_with_workspace(
            &owner,
            generation,
            "http://127.0.0.1:3002".to_string(),
            Some("ws-two".into()),
            TemporaryKind::DefaultStartup,
        )
        .unwrap();
    assert!(matches!(
        registry.owner_current_workspace(&owner),
        OwnerWorkspaceSnapshot::Registered { wid, .. } if wid == "ws-two"
    ));

    // The device view is unchanged by the window's switch, and the device
    // still reaches the runtime its view names.
    assert_eq!(
        auth.lock().unwrap().device_view("phone-a").unwrap(),
        Some(DeviceView {
            workspace_id: "ws-one".into(),
            session_id: Some("session-one".into()),
            generation: 0,
        })
    );
    let reply = runtime_request(
        &mut device,
        "cmd-1",
        wire_target("ws-one", "session-one", "instance-one"),
    )
    .await;
    assert_eq!(reply["type"], "runtime_response", "{reply}");
    assert_eq!(reply["requestId"], "cmd-1", "{reply}");

    let _ = device.close(None).await;
    host.stop();
    let _ = fs::remove_dir_all(temp);
}

/// 契约 1: two devices plus one local window coexist; each device resolves its
/// own view, a cross-device target is refused, and a request made against a
/// superseded generation is rejected explicitly (error frame, never a silent
/// drop).
#[tokio::test]
async fn two_devices_and_a_local_window_stay_independent_and_generation_is_not_a_device_guard() {
    let temp = temp_dir("multi");
    let metadata = open_store(&temp);
    let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
    let runtimes = NativePiManager::new(8);
    let registry = Arc::new(WindowOwnerRegistry::default());
    let (owner, capability) = seed_window(&registry, &temp, "ws-one");
    seed_runtime(
        &runtimes,
        &owner,
        "ws-one",
        "session-one",
        "instance-one",
        0,
    );
    seed_runtime(
        &runtimes,
        &owner,
        "ws-two",
        "session-two",
        "instance-two",
        0,
    );

    let host = start_host(
        &temp,
        runtimes,
        Arc::clone(&auth),
        Arc::clone(&metadata),
        &registry,
    )
    .await;
    let token_a = pair_device(&auth, &owner, "phone-a");
    let token_b = pair_device(&auth, &owner, "phone-b");
    let (mut device_a, ack_a) = connect_device(&host, "device-a", &token_a).await;
    assert_eq!(ack_a["type"], "hello_ack", "{ack_a}");
    let (mut device_b, ack_b) = connect_device(&host, "device-b", &token_b).await;
    assert_eq!(ack_b["type"], "hello_ack", "{ack_b}");

    // Device B's own navigation (T5 owns this op; here the persisted view is
    // the same mutation it will perform) moves it to ws-two.
    auth.lock()
        .unwrap()
        .store_device_view(
            "phone-b",
            &DeviceView {
                workspace_id: "ws-two".into(),
                session_id: Some("session-two".into()),
                generation: 0,
            },
        )
        .unwrap();

    // Each device reaches only its own workspace's runtime.
    let a_own = runtime_request(
        &mut device_a,
        "a-own",
        wire_target("ws-one", "session-one", "instance-one"),
    )
    .await;
    assert_eq!(a_own["type"], "runtime_response", "{a_own}");
    let b_own = runtime_request(
        &mut device_b,
        "b-own",
        wire_target("ws-two", "session-two", "instance-two"),
    )
    .await;
    assert_eq!(b_own["type"], "runtime_response", "{b_own}");
    let a_cross = runtime_request(
        &mut device_a,
        "a-cross",
        wire_target("ws-two", "session-two", "instance-two"),
    )
    .await;
    assert_eq!(a_cross["error"]["code"], "unauthorized_target", "{a_cross}");
    assert_eq!(a_cross["requestId"], "a-cross", "{a_cross}");
    let b_cross = runtime_request(
        &mut device_b,
        "b-cross",
        wire_target("ws-one", "session-one", "instance-one"),
    )
    .await;
    assert_eq!(b_cross["error"]["code"], "unauthorized_target", "{b_cross}");
    assert_eq!(b_cross["requestId"], "b-cross", "{b_cross}");

    // The local window keeps its own (desktop) resolution path throughout.
    let (mut desktop, desktop_ack) = connect_desktop(&host, "window-1", &capability).await;
    assert_eq!(desktop_ack["type"], "hello_ack", "{desktop_ack}");
    let desktop_reply = runtime_request(
        &mut desktop,
        "desktop-1",
        wire_target("ws-one", "session-one", "instance-one"),
    )
    .await;
    assert_eq!(desktop_reply["type"], "runtime_response", "{desktop_reply}");

    // A device's view generation is not a rebind guard: moving it does not
    // revoke the runtime the device is looking at (Paseo parity — only a
    // desktop window can be re-pointed). The request still succeeds.
    auth.lock()
        .unwrap()
        .store_device_view(
            "phone-a",
            &DeviceView {
                workspace_id: "ws-one".into(),
                session_id: Some("session-one".into()),
                generation: 1,
            },
        )
        .unwrap();
    let stale = runtime_request(
        &mut device_a,
        "a-stale",
        wire_target("ws-one", "session-one", "instance-one"),
    )
    .await;
    assert_eq!(stale["type"], "runtime_response", "{stale}");
    assert_eq!(stale["requestId"], "a-stale", "{stale}");

    // Device B is unaffected by device A's generation move.
    let b_after = runtime_request(
        &mut device_b,
        "b-after",
        wire_target("ws-two", "session-two", "instance-two"),
    )
    .await;
    assert_eq!(b_after["type"], "runtime_response", "{b_after}");

    let _ = desktop.close(None).await;
    let _ = device_a.close(None).await;
    let _ = device_b.close(None).await;
    host.stop();
    let _ = fs::remove_dir_all(temp);
}

/// 契约 1 / T10: revoking a device drops its view with its tokens — a
/// re-paired device starts from the pairing seed, never from the pre-revocation
/// workspace.
#[tokio::test]
async fn revoke_drops_the_device_view_with_the_tokens() {
    let temp = temp_dir("revoke");
    let metadata = open_store(&temp);
    let auth = Arc::new(Mutex::new(RemoteAuth::new(Arc::clone(&metadata))));
    let runtimes = NativePiManager::new(8);
    let registry = Arc::new(WindowOwnerRegistry::default());
    let (owner, _capability) = seed_window(&registry, &temp, "ws-one");
    let host = start_host(
        &temp,
        runtimes,
        Arc::clone(&auth),
        Arc::clone(&metadata),
        &registry,
    )
    .await;
    let token = pair_device(&auth, &owner, "phone-a");
    let (mut device, ack) = connect_device(&host, "device-a", &token).await;
    assert_eq!(ack["type"], "hello_ack", "{ack}");
    assert!(auth
        .lock()
        .unwrap()
        .device_view("phone-a")
        .unwrap()
        .is_some());

    auth.lock().unwrap().revoke("phone-a").unwrap();
    assert_eq!(auth.lock().unwrap().device_view("phone-a").unwrap(), None);
    assert!(auth.lock().unwrap().authenticate(&token).unwrap().is_none());

    let _ = device.close(None).await;
    host.stop();
    let _ = fs::remove_dir_all(temp);
}
