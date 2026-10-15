// ABOUTME: Child-webview browser panes for the file-preview panel (spec 2026-09-22).
// ABOUTME: External webviews carry no capability init script; URL policy denies the host origin.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use picot_host::host_server::{PaneRect, PaneSurface};
use tauri::webview::NewWindowResponse;
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewBuilder, WebviewUrl,
};

/// Whether a URL may load inside a browser pane. Only http(s) is allowed and
/// an external page must never reach the Picot host under ANY of its local
/// aliases — jumping back to host origin must not reach the owner WebView's
/// context.
pub fn pane_url_allowed(url: &Url, host_origin: &str, host_port: u16) -> bool {
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    if url.origin().ascii_serialization() == host_origin {
        return false;
    }
    !resolves_to_host(url, host_port)
}

/// The host answers on several names: the serialized-origin comparison alone
/// misses `http://localhost:<port>/`, `http://0.0.0.0:<port>/`, and the LAN
/// address when LAN access binds a route — all reach the same server. Deny by
/// (local alias, port) instead. A different machine's private address sharing
/// the host's ephemeral port is indistinguishable from the host's own LAN
/// alias, so it is denied too: a trust boundary errs closed.
fn resolves_to_host(url: &Url, host_port: u16) -> bool {
    if host_port == 0 || url.port_or_known_default() != Some(host_port) {
        return false;
    }
    url.host_str().is_some_and(is_local_alias)
}

fn is_local_alias(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match bare.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback() || v6.is_unspecified(),
        Err(_) => false,
    }
}

/// Tauri-managed state so exit/window-destroy hooks can reach the runtime.
pub struct BrowserPaneState(pub std::sync::Arc<BrowserPaneRuntime>);

fn webview_label(pane_id: &str) -> String {
    let encoded: String = pane_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("browser-pane-{encoded}")
}

pub struct BrowserPaneRuntime {
    app: AppHandle,
    host_origin: String,
    host_port: u16,
    /// Keyed by (window label, pane id): the same file opened from two
    /// workspace windows is two panes, and webview labels must stay unique
    /// app-wide.
    panes: Mutex<HashMap<String, Webview>>,
}

impl BrowserPaneRuntime {
    pub fn new(app: AppHandle, host_origin: &str) -> Self {
        let host_port = host_origin
            .parse::<Url>()
            .ok()
            .and_then(|url| url.port_or_known_default())
            .unwrap_or(0);
        Self {
            app,
            host_origin: host_origin.to_string(),
            host_port,
            panes: Mutex::new(HashMap::new()),
        }
    }

    /// Create a child webview inside `window_label` at `rect`. The webview is
    /// created hidden-at-position by `add_child` (correct rect from the first
    /// frame) and only shown on demand by the WebView layer.
    pub fn create(
        &self,
        pane_id: &str,
        window_label: &str,
        url: &str,
        rect: PaneRect,
    ) -> Result<(), String> {
        let parsed: Url = url.parse().map_err(|e| format!("invalid URL: {e}"))?;
        if !pane_url_allowed(&parsed, &self.host_origin, self.host_port) {
            return Err("url_not_allowed".to_string());
        }
        let window = self
            .app
            .get_window(window_label)
            .ok_or_else(|| "window_not_found".to_string())?;
        // A pane can outlive the host page that created it: switching
        // workspaces reloads the WebView while the native child survives,
        // so the reloaded page's create is authoritative — replace the
        // stale pane instead of erroring (its tab would be wedged forever).
        if let Some(stale) = self.panes.lock().unwrap().remove(pane_id) {
            log::info!("[browser-pane] replacing stale pane {pane_id} (host page reloaded)");
            let _ = stale.close();
        }
        let host_origin = self.host_origin.clone();
        let host_port = self.host_port;
        let builder = WebviewBuilder::new(webview_label(pane_id), WebviewUrl::External(parsed))
            .on_navigation(move |url| pane_url_allowed(url, &host_origin, host_port))
            .on_new_window(|_url, _features| NewWindowResponse::Deny);
        let webview = window
            .add_child(
                builder,
                LogicalPosition::new(rect.x, rect.y),
                LogicalSize::new(rect.width, rect.height),
            )
            .map_err(|e| format!("webview create failed: {e}"))?;
        // Start hidden: the WebView layer shows the pane once its tab is the
        // active preview and the panel is open.
        let _ = webview.hide();
        self.panes
            .lock()
            .unwrap()
            .insert(pane_id.to_string(), webview);
        Ok(())
    }

    pub fn set_rect(&self, pane_id: &str, rect: PaneRect) -> Result<(), String> {
        let panes = self.panes.lock().unwrap();
        let webview = panes
            .get(pane_id)
            .ok_or_else(|| "pane_not_found".to_string())?;
        webview
            .set_position(LogicalPosition::new(rect.x, rect.y))
            .map_err(|e| format!("set_position failed: {e}"))?;
        webview
            .set_size(LogicalSize::new(rect.width, rect.height))
            .map_err(|e| format!("set_size failed: {e}"))?;
        Ok(())
    }

    pub fn set_visible(&self, pane_id: &str, visible: bool) -> Result<(), String> {
        let panes = self.panes.lock().unwrap();
        let webview = panes
            .get(pane_id)
            .ok_or_else(|| "pane_not_found".to_string())?;
        if visible {
            webview.show().map_err(|e| format!("show failed: {e}"))?;
        } else {
            webview.hide().map_err(|e| format!("hide failed: {e}"))?;
        }
        Ok(())
    }

    pub fn navigate(&self, pane_id: &str, url: &str) -> Result<(), String> {
        let parsed: Url = url.parse().map_err(|e| format!("invalid URL: {e}"))?;
        if !pane_url_allowed(&parsed, &self.host_origin, self.host_port) {
            return Err("url_not_allowed".to_string());
        }
        let panes = self.panes.lock().unwrap();
        let webview = panes
            .get(pane_id)
            .ok_or_else(|| "pane_not_found".to_string())?;
        webview
            .navigate(parsed)
            .map_err(|e| format!("navigate failed: {e}"))
    }

    pub fn url(&self, pane_id: &str) -> Result<String, String> {
        let panes = self.panes.lock().unwrap();
        let webview = panes
            .get(pane_id)
            .ok_or_else(|| "pane_not_found".to_string())?;
        webview
            .url()
            .map(|u| u.to_string())
            .map_err(|e| format!("url failed: {e}"))
    }

    pub fn destroy(&self, pane_id: &str) -> Result<(), String> {
        let webview = self.panes.lock().unwrap().remove(pane_id);
        match webview {
            Some(webview) => webview.close().map_err(|e| format!("close failed: {e}")),
            None => Err("pane_not_found".to_string()),
        }
    }

    /// Drop every pane belonging to a window that was destroyed; stale map
    /// entries would otherwise leak webviews and collide on label reuse.
    pub fn destroy_all_for_window(&self, window_label: &str) {
        let mut panes = self.panes.lock().unwrap();
        let stale: Vec<String> = panes
            .iter()
            .filter(|(_, webview)| webview.window().label() == window_label)
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            if let Some(webview) = panes.remove(&id) {
                let _ = webview.close();
            }
        }
    }

    /// Evaluate an expression and return its JSON serialization. The wrapper
    /// catches exceptions because `eval_with_callback` ignores them on
    /// Windows; the timeout guards against a callback that never fires.
    pub async fn eval_json(&self, pane_id: &str, expression: &str) -> Result<String, String> {
        let wrapped = format!(
            "(function() {{ try {{ var v = ({expression}); return JSON.stringify({{ ok: true, value: v }}); }} catch (e) {{ return JSON.stringify({{ ok: false, error: String(e) }}); }} }})()"
        );
        let rx = {
            // Scope the pane lock: it must never live across the await below.
            let panes = self.panes.lock().unwrap();
            let webview = panes
                .get(pane_id)
                .ok_or_else(|| "pane_not_found".to_string())?;
            let (tx, rx) = tokio::sync::oneshot::channel::<String>();
            let tx = std::sync::Mutex::new(Some(tx));
            webview
                .eval_with_callback(&wrapped, move |json| {
                    if let Ok(mut slot) = tx.lock() {
                        if let Some(sender) = slot.take() {
                            let _ = sender.send(json);
                        }
                    }
                })
                .map_err(|e| format!("eval failed: {e}"))?;
            rx
        };
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(json)) => Ok(json),
            _ => Err("eval_timeout".to_string()),
        }
    }
}

#[async_trait::async_trait]
impl PaneSurface for BrowserPaneRuntime {
    fn create(
        &self,
        pane_id: &str,
        window_label: &str,
        url: &str,
        rect: PaneRect,
    ) -> Result<(), String> {
        BrowserPaneRuntime::create(self, pane_id, window_label, url, rect)
    }

    fn set_rect(&self, pane_id: &str, rect: PaneRect) -> Result<(), String> {
        BrowserPaneRuntime::set_rect(self, pane_id, rect)
    }

    fn set_visible(&self, pane_id: &str, visible: bool) -> Result<(), String> {
        BrowserPaneRuntime::set_visible(self, pane_id, visible)
    }

    async fn eval_json(&self, pane_id: &str, expression: &str) -> Result<String, String> {
        BrowserPaneRuntime::eval_json(self, pane_id, expression).await
    }

    fn navigate(&self, pane_id: &str, url: &str) -> Result<(), String> {
        BrowserPaneRuntime::navigate(self, pane_id, url)
    }

    fn url(&self, pane_id: &str) -> Result<String, String> {
        BrowserPaneRuntime::url(self, pane_id)
    }

    fn destroy(&self, pane_id: &str) -> Result<(), String> {
        BrowserPaneRuntime::destroy(self, pane_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        s.parse().unwrap()
    }

    const HOST_ORIGIN: &str = "http://127.0.0.1:41000";
    const HOST_PORT: u16 = 41000;

    fn allowed(target: &str) -> bool {
        pane_url_allowed(&url(target), HOST_ORIGIN, HOST_PORT)
    }

    #[test]
    fn allows_http_https_except_host_origin() {
        assert!(allowed("http://localhost:5173/"));
        assert!(allowed("https://example.com/page"));
        assert!(!allowed("http://127.0.0.1:41000/app"));
        // userinfo is not a host: this URL actually loads evil.com, which is
        // an ordinary external page, not a jump back to the host origin.
        assert!(allowed("http://127.0.0.1:41000@evil.com/"));
    }

    #[test]
    fn denies_non_http_schemes() {
        assert!(!allowed("file:///etc/passwd"));
        assert!(!allowed("tauri://localhost/x"));
        assert!(!allowed("about:blank"));
    }

    #[test]
    fn webview_label_encodes_pane_keys_without_invalid_characters() {
        let label = webview_label("native-workspace-w1:p1");
        assert_eq!(
            label,
            "browser-pane-6e61746976652d776f726b73706163652d77313a7031"
        );
        assert!(label
            .bytes()
            .all(|byte| { byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' }));
    }

    #[test]
    fn different_port_on_same_host_is_allowed() {
        // officecli watch servers bind their own loopback ports
        assert!(allowed("http://127.0.0.1:41001/"));
        assert!(allowed("http://localhost:41001/"));
    }

    #[test]
    fn denies_every_local_alias_of_the_host_port() {
        // The serialized-origin comparison alone misses every one of these,
        // yet each reaches the host server.
        for alias in [
            "http://localhost:41000/",
            "http://127.0.0.1:41000/",
            "http://127.0.0.2:41000/x",
            "http://0.0.0.0:41000/",
            "http://[::1]:41000/",
            "http://192.168.1.10:41000/",
            "http://10.0.0.5:41000/",
            "http://172.16.3.4:41000/",
            "http://169.254.1.1:41000/",
        ] {
            assert!(!allowed(alias), "{alias} must not reach the host");
        }
    }

    #[test]
    fn private_addresses_on_other_ports_stay_allowed() {
        // A LAN dev server stays a legitimate target; only the host's own
        // port is denied on local aliases.
        assert!(allowed("http://192.168.1.50:5173/"));
        assert!(allowed("http://10.0.0.9:8080/"));
    }

    #[test]
    fn unknown_host_port_leaves_origin_comparison_only() {
        // port 0 means the origin did not parse; the exact-origin deny still
        // holds and unrelated hosts stay reachable.
        assert!(!pane_url_allowed(
            &url("http://127.0.0.1:41000/x"),
            HOST_ORIGIN,
            0
        ));
        assert!(pane_url_allowed(
            &url("http://127.0.0.1:41001/"),
            HOST_ORIGIN,
            0
        ));
    }
}
