// ABOUTME: Caller-checked Tauri commands for remote host pairing, status and grants.
// ABOUTME: Identity and exact page origin come from injected WebviewWindow, never arguments.

use super::{check_window_id, HostClient, HostClientState};
use crate::daemon_client::commands::caller_link;
use picot_host::window_owner::navigation_origin;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

fn caller(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
) -> Result<(Arc<HostClient>, String, String), String> {
    caller_link(app, window)?;
    let url = window.url().map_err(|_| "Caller has no document URL")?;
    let origin = navigation_origin(&url).ok_or("Caller has no loopback origin")?;
    let state = app
        .try_state::<HostClientState>()
        .ok_or("Host client unavailable")?;
    let client = state.0.as_ref().map_err(Clone::clone)?.clone();
    Ok((client, window.label().to_string(), origin))
}

#[tauri::command]
pub async fn host_list(window: tauri::WebviewWindow, app: AppHandle) -> Result<Value, String> {
    let (client, _, _) = caller(&app, &window)?;
    client.list().await
}
#[tauri::command]
pub async fn host_add(
    window: tauri::WebviewWindow,
    app: AppHandle,
    offer_url: String,
) -> Result<Value, String> {
    let (client, id, origin) = caller(&app, &window)?;
    client.add(&id, &origin, &offer_url).await
}
#[tauri::command]
pub async fn host_confirm_tofu(
    window: tauri::WebviewWindow,
    app: AppHandle,
    question_id: u64,
    approved: bool,
) -> Result<Value, String> {
    let (client, id, origin) = caller(&app, &window)?;
    client.confirm(&id, &origin, question_id, approved).await
}
#[tauri::command]
pub async fn host_remove(
    window: tauri::WebviewWindow,
    app: AppHandle,
    server_id: String,
) -> Result<Value, String> {
    let (client, _, _) = caller(&app, &window)?;
    client.remove(&server_id).await?;
    Ok(json!({"ok":true, "remoteDeviceRetained":true}))
}
#[tauri::command]
pub async fn host_status(
    window: tauri::WebviewWindow,
    app: AppHandle,
    server_id: String,
) -> Result<Value, String> {
    let (client, _, _) = caller(&app, &window)?;
    client.status(&server_id).await
}
#[tauri::command]
pub async fn host_grant(
    window: tauri::WebviewWindow,
    app: AppHandle,
    server_id: String,
    window_id: Option<String>,
) -> Result<Value, String> {
    let (client, id, origin) = caller(&app, &window)?;
    check_window_id(&id, window_id.as_deref())?;
    client.grant(&id, &origin, &server_id).await
}
#[tauri::command]
pub async fn host_retry_save(
    window: tauri::WebviewWindow,
    app: AppHandle,
    server_id: String,
) -> Result<Value, String> {
    let (client, _, _) = caller(&app, &window)?;
    client.retry_save(&server_id).await
}

#[tauri::command]
pub async fn host_release(window: tauri::WebviewWindow, app: AppHandle) -> Result<Value, String> {
    // T8 calls this synchronously before reload navigation (contract 4):
    // unload-time cleanup is unreliable, so the explicit release is the main
    // path and the Destroyed event is the safety net.
    let (client, id, _) = caller(&app, &window)?;
    client.close_window(&id).await;
    Ok(json!({"ok":true}))
}

#[tauri::command]
pub async fn relay_device_list(
    window: tauri::WebviewWindow,
    app: AppHandle,
) -> Result<Value, String> {
    let link = caller_link(&app, &window)?;
    let reply = link
        .client()
        .request("relay.device_list", json!({}))
        .await?;
    // Project again at IPC boundary: future daemon additions cannot leak secrets.
    let devices = reply
        .get("devices")
        .and_then(Value::as_array)
        .ok_or("Invalid device list reply")?;
    Ok(json!({"devices": devices.iter().map(|device| json!({
        "deviceId":device.get("deviceId"), "pairedAt":device.get("pairedAt"), "owner":device.get("owner")
    })).collect::<Vec<_>>()}))
}

/// Revoke one paired device (spec §3.4): the daemon drops its tokens and kicks
/// its live sessions through the host server's one teardown path. Device ids
/// are the only argument; no token ever crosses this boundary.
#[tauri::command]
pub async fn relay_revoke_device(
    window: tauri::WebviewWindow,
    app: AppHandle,
    device_id: String,
) -> Result<Value, String> {
    let link = caller_link(&app, &window)?;
    link.client()
        .request("relay.revoke_device", json!({ "deviceId": device_id }))
        .await
}
