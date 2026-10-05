// ABOUTME: Runs embedded Pi MCP CLI commands without exposing credentials to the host.
// ABOUTME: Tracks owner-bound login operations, process-tree cancellation and cached server reports.
use crate::host_control::HostEventSink;
use crate::oauth_manager::{OAuthClient, OAuthManager, OAuthOutcome, OAuthStatus};
use crate::window_owner::OwnerId;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

const LOGIN_TTL: Duration = Duration::from_secs(360);
const CACHE_TTL: Duration = Duration::from_secs(60);

struct Running {
    child: Child,
    tree: crate::process_tree::ProcessTree,
}
struct Login {
    name: String,
    owner: OwnerId,
    generation: u64,
    auth_url: Option<String>,
    error: Option<String>,
    process: Option<Arc<Mutex<Running>>>,
}
#[derive(Clone)]
pub(crate) struct McpLoginRunner {
    inner: Arc<Mutex<HashMap<String, Login>>>,
    cache: Arc<Mutex<HashMap<PathBuf, (Instant, Value)>>>,
    cache_epoch: Arc<AtomicU64>,
    oauth: Arc<Mutex<OAuthManager>>,
    events: HostEventSink,
}

fn command(binary: &Path, cwd: &Path, action: &str, name: Option<&str>) -> Command {
    let mut cmd = Command::new(binary);
    crate::windows_child::hide_console(&mut cmd);
    cmd.current_dir(cwd).arg("mcp").arg(action);
    if let Some(name) = name {
        cmd.arg(name);
    }
    if action == "login" {
        cmd.args(["--timeout", "300"]);
    } else if action == "list" {
        cmd.arg("--json");
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

#[derive(Default)]
struct UrlParser(bool);
impl UrlParser {
    fn feed(&mut self, line: &str, name: &str) -> Option<String> {
        if self.0 {
            self.0 = false;
            let candidate = line.trim();
            if (candidate.starts_with("https://") || candidate.starts_with("http://"))
                && !candidate.chars().any(char::is_whitespace)
                && candidate.len() <= 8192
            {
                return Some(candidate.to_owned());
            }
        }
        self.0 = line.trim() == format!("Sign in to MCP server \"{name}\" in your browser:");
        None
    }
}

const SAFE_CONFIG_ERROR: &str = "MCP configuration error; see Pi logs";
const SAFE_SERVER_ERROR: &str = "MCP server connection failed; see Pi logs";
const SAFE_TRUST_NOTE: &str =
    "The project .pi/mcp.json is ignored because the project is not trusted; open the project in Picot to trust it.";
const MCP_EXPOSURES: [&str; 4] = ["codemode", "direct", "deferred", "hidden"];

/// `pi mcp list --json` reports the transport as the raw URL or command line.
/// The page only ever needs the transport kind, so nothing that could carry a
/// credential, host, path or argument reaches the WebView.
fn safe_transport(raw: &str) -> &'static str {
    // A real URL parse, not a prefix heuristic: only a complete http/https URL
    // with a host is an HTTP transport. Anything else (a command line, a
    // fragment, a scheme-less string) stays stdio.
    match reqwest::Url::parse(raw.trim()) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => "http",
        _ => "stdio",
    }
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("MCP server report field {key} must be a string"))
}

fn optional_string_field(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("MCP server report field {key} must be a string")),
    }
}

fn optional_count(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("MCP server report field {key} must be a non-negative integer")),
        Some(_) => Err(format!(
            "MCP server report field {key} must be a non-negative integer"
        )),
    }
}

/// One report row, validated and projected. Unknown additive CLI fields are
/// accepted but never forwarded; the raw transport and error text are replaced.
fn project_server_row(row: &Value) -> Result<Value, String> {
    let object = row
        .as_object()
        .ok_or_else(|| "MCP server report row must be an object".to_string())?;
    let name = string_field(object, "name")?;
    let scope = string_field(object, "scope")?;
    let source = string_field(object, "source")?;
    let state = string_field(object, "state")?;
    let transport = string_field(object, "transport")?;
    let enabled = object
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "MCP server report field enabled must be a boolean".to_string())?;
    let exposure = string_field(object, "exposure")?;
    if !MCP_EXPOSURES.contains(&exposure.as_str()) {
        // Fixed text: a bad schema must not echo whatever the CLI printed.
        return Err("MCP server report exposure is not canonical".to_string());
    }
    let tools = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| "MCP server report field tools must be an array".to_string())?
        .iter()
        .map(|tool| {
            tool.as_str()
                .map(|name| Value::String(name.to_owned()))
                .ok_or_else(|| "MCP server report tool names must be strings".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let override_path = optional_string_field(object, "override")?;
    let tool_exposure = match object.get("toolExposure") {
        None => None,
        Some(Value::Object(map)) => {
            let mut safe = serde_json::Map::new();
            for (tool, value) in map {
                let exposure = value.as_str().ok_or_else(|| {
                    "MCP server report toolExposure values must be strings".to_string()
                })?;
                if !MCP_EXPOSURES.contains(&exposure) {
                    return Err("MCP server report toolExposure value is not canonical".to_string());
                }
                safe.insert(tool.clone(), Value::String(exposure.to_owned()));
            }
            Some(Value::Object(safe))
        }
        Some(_) => return Err("MCP server report field toolExposure must be an object".to_string()),
    };
    let resources = optional_count(object, "resources")?;
    let resource_templates = optional_count(object, "resourceTemplates")?;
    let failed = match object.get("error") {
        None => false,
        Some(Value::String(_)) => true,
        Some(_) => return Err("MCP server report field error must be a string".to_string()),
    };

    let mut projected = serde_json::Map::new();
    projected.insert("name".into(), Value::String(name));
    projected.insert("scope".into(), Value::String(scope));
    projected.insert("source".into(), Value::String(source));
    projected.insert("enabled".into(), Value::Bool(enabled));
    projected.insert("exposure".into(), Value::String(exposure));
    projected.insert(
        "transport".into(),
        Value::String(safe_transport(&transport).into()),
    );
    projected.insert("state".into(), Value::String(state));
    projected.insert("tools".into(), Value::Array(tools));
    if let Some(override_path) = override_path {
        projected.insert("override".into(), Value::String(override_path));
    }
    if let Some(tool_exposure) = tool_exposure {
        projected.insert("toolExposure".into(), tool_exposure);
    }
    if let Some(resources) = resources {
        projected.insert("resources".into(), Value::Number(resources.into()));
    }
    if let Some(resource_templates) = resource_templates {
        projected.insert(
            "resourceTemplates".into(),
            Value::Number(resource_templates.into()),
        );
    }
    if failed {
        projected.insert("error".into(), Value::String(SAFE_SERVER_ERROR.into()));
    }
    Ok(Value::Object(projected))
}

/// Validate one `pi mcp list --json` run and project it into the safe envelope
/// the host returns. Exit 0 and exit 1 both carry a usable report (exit 1 means
/// "some server failed or the config has errors"); any other exit, a signal, or
/// a malformed document is a query failure, never an empty report.
fn parse_list_report(stdout: &[u8], status: &std::process::ExitStatus) -> Result<Value, String> {
    let code = status
        .code()
        .ok_or_else(|| "MCP list terminated by a signal".to_string())?;
    if code != 0 && code != 1 {
        return Err(format!("MCP list exited with code {code}"));
    }
    let report: Value =
        serde_json::from_slice(stdout).map_err(|_| "MCP list returned invalid JSON".to_string())?;
    let object = report
        .as_object()
        .ok_or_else(|| "MCP list report is not an object".to_string())?;
    let rows = object
        .get("servers")
        .and_then(Value::as_array)
        .ok_or_else(|| "MCP list report needs a servers array".to_string())?;
    let errors = object
        .get("errors")
        .and_then(Value::as_array)
        .ok_or_else(|| "MCP list report needs an errors array".to_string())?;
    if !errors.iter().all(Value::is_string) {
        return Err("MCP list report errors must be strings".to_string());
    }
    let note = match object.get("note") {
        None => None,
        Some(Value::String(_)) => Some(SAFE_TRUST_NOTE.to_string()),
        Some(_) => return Err("MCP list report note must be a string".to_string()),
    };
    let servers = rows
        .iter()
        .map(project_server_row)
        .collect::<Result<Vec<_>, _>>()?;

    let mut projected = serde_json::Map::new();
    projected.insert("servers".into(), Value::Array(servers));
    // Diagnostics keep their count but never their text: CLI config errors can
    // quote URLs, commands and credentials.
    projected.insert(
        "errors".into(),
        Value::Array(
            errors
                .iter()
                .map(|_| Value::String(SAFE_CONFIG_ERROR.into()))
                .collect(),
        ),
    );
    if let Some(note) = note {
        projected.insert("note".into(), Value::String(note));
    }
    Ok(Value::Object(projected))
}

fn status_name(status: &OAuthStatus) -> &'static str {
    match status {
        OAuthStatus::Pending => "pending",
        OAuthStatus::Succeeded => "succeeded",
        OAuthStatus::Failed => "failed",
        OAuthStatus::Cancelled => "cancelled",
    }
}
fn emit(
    events: &HostEventSink,
    owner: &OwnerId,
    id: &str,
    status: OAuthStatus,
    url: Option<&str>,
    error: Option<&str>,
) {
    let mut payload = json!({ "operationId": id, "status": status_name(&status) });
    if let Some(url) = url {
        payload["authUrl"] = json!(url);
    }
    if let Some(error) = error {
        payload["error"] = json!(error);
    }
    events.send_owner_event(
        owner,
        json!({ "type": "mcpLoginUpdate", "payload": payload }),
    );
}
impl McpLoginRunner {
    pub(crate) fn new(oauth: Arc<Mutex<OAuthManager>>, events: HostEventSink) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_epoch: Arc::new(AtomicU64::new(0)),
            oauth,
            events,
        }
    }
    pub(crate) fn start(
        &self,
        binary: &Path,
        cwd: &Path,
        name: &str,
        owner: &OwnerId,
    ) -> Result<String, String> {
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err("Invalid MCP server name".into());
        }
        let mut entries = self.inner.lock().map_err(|_| "MCP manager unavailable")?;
        let mut oauth = self.oauth.lock().map_err(|_| "OAuth manager unavailable")?;
        entries.retain(|id, entry| {
            oauth
                .status(entry.owner.as_str(), entry.generation, id)
                .is_ok()
        });
        if entries.iter().any(|(id, entry)| {
            entry.name == name
                && entry.process.is_some()
                && oauth.status(entry.owner.as_str(), entry.generation, id)
                    == Ok(OAuthStatus::Pending)
        }) {
            return Err("MCP login already active for this server".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let generation = oauth.generation();
        oauth
            .start(OAuthClient::Desktop, owner.as_str(), &id, LOGIN_TTL)
            .map_err(|e| e.code().to_owned())?;
        let mut cmd = command(binary, cwd, "login", Some(name));
        crate::process_tree::configure_child(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = oauth.complete(owner.as_str(), generation, &id, OAuthOutcome::Failed);
                return Err(format!("Cannot start MCP login: {error}"));
            }
        };
        let tree = match crate::process_tree::ProcessTree::attach(&mut child) {
            Ok(tree) => tree,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = oauth.complete(owner.as_str(), generation, &id, OAuthOutcome::Failed);
                return Err(error);
            }
        };
        let stdout = child.stdout.take().ok_or("MCP stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("MCP stderr unavailable")?;
        let process = Arc::new(Mutex::new(Running { child, tree }));
        entries.insert(
            id.clone(),
            Login {
                name: name.to_owned(),
                owner: owner.clone(),
                generation,
                auth_url: None,
                error: None,
                process: Some(process.clone()),
            },
        );
        drop(oauth);
        drop(entries);
        let runner = self.clone();
        let id_worker = id.clone();
        let name_worker = name.to_owned();
        std::thread::spawn(move || {
            let stderr_thread = std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = stderr.take(64 * 1024).read_to_end(&mut buf);
            });
            let url_runner = runner.clone();
            let url_id = id_worker.clone();
            let stdout_thread = std::thread::spawn(move || {
                let mut parser = UrlParser::default();
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Some(url) = parser.feed(&line, &name_worker) {
                        if let Ok(mut entries) = url_runner.inner.lock() {
                            if let Some(entry) = entries.get_mut(&url_id) {
                                if url_runner.oauth.lock().ok().and_then(|mut oauth| {
                                    oauth
                                        .status(entry.owner.as_str(), entry.generation, &url_id)
                                        .ok()
                                }) == Some(OAuthStatus::Pending)
                                {
                                    entry.auth_url = Some(url.clone());
                                    emit(
                                        &url_runner.events,
                                        &entry.owner,
                                        &url_id,
                                        OAuthStatus::Pending,
                                        Some(&url),
                                        None,
                                    );
                                }
                            }
                        }
                    }
                }
            });
            let result = loop {
                let result = process
                    .lock()
                    .map_err(|_| "MCP process unavailable".to_owned())
                    .and_then(|mut running| running.child.try_wait().map_err(|e| e.to_string()));
                match result {
                    Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                    Ok(Some(exit)) => break Ok(exit.success()),
                    Err(error) => break Err(error),
                }
            };
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            let mut entries = match runner.inner.lock() {
                Ok(entries) => entries,
                Err(_) => return,
            };
            let Some(entry) = entries.get_mut(&id_worker) else {
                return;
            };
            entry.process = None;
            let Ok(mut oauth) = runner.oauth.lock() else {
                return;
            };
            if oauth.status(entry.owner.as_str(), entry.generation, &id_worker)
                != Ok(OAuthStatus::Pending)
            {
                return;
            }
            let status = if result == Ok(true) {
                OAuthStatus::Succeeded
            } else {
                OAuthStatus::Failed
            };
            if status == OAuthStatus::Failed {
                // Pi stderr may contain OAuth callback details: never persist it in host state.
                entry.error = Some("MCP login failed; see Pi logs".into());
            }
            if oauth
                .complete(
                    entry.owner.as_str(),
                    entry.generation,
                    &id_worker,
                    if status == OAuthStatus::Succeeded {
                        OAuthOutcome::Succeeded
                    } else {
                        OAuthOutcome::Failed
                    },
                )
                .is_ok()
            {
                if status == OAuthStatus::Succeeded {
                    runner.invalidate();
                }
                emit(
                    &runner.events,
                    &entry.owner,
                    &id_worker,
                    status,
                    entry.auth_url.as_deref(),
                    entry.error.as_deref(),
                );
            }
        });
        Ok(id)
    }
    pub(crate) fn status(&self, owner: &OwnerId, id: &str) -> Result<Value, String> {
        let entries = self.inner.lock().map_err(|_| "MCP manager unavailable")?;
        let entry = entries.get(id).ok_or("oauth_operation_not_found")?;
        let status = self
            .oauth
            .lock()
            .map_err(|_| "OAuth manager unavailable")?
            .status(owner.as_str(), entry.generation, id)
            .map_err(|e| e.code().to_owned())?;
        let mut value = json!({ "ok": true, "status": status_name(&status) });
        if let Some(url) = &entry.auth_url {
            value["authUrl"] = json!(url);
        }
        if let Some(error) = &entry.error {
            value["error"] = json!(error);
        }
        Ok(value)
    }
    pub(crate) fn cancel(&self, owner: &OwnerId, id: &str) -> Result<(), String> {
        let mut entries = self.inner.lock().map_err(|_| "MCP manager unavailable")?;
        let entry = entries.get_mut(id).ok_or("oauth_operation_not_found")?;
        let mut oauth = self.oauth.lock().map_err(|_| "OAuth manager unavailable")?;
        let status = oauth
            .status(owner.as_str(), entry.generation, id)
            .map_err(|e| e.code().to_owned())?;
        if status == OAuthStatus::Pending {
            if let Some(process) = &entry.process {
                let mut process = process.lock().map_err(|_| "MCP process unavailable")?;
                // Set terminal state before the worker can process a kill exit.
                oauth
                    .cancel(owner.as_str(), entry.generation, id)
                    .map_err(|e| e.code().to_owned())?;
                let Running { child, tree } = &mut *process;
                tree.terminate(child)?;
            }
            emit(
                &self.events,
                owner,
                id,
                OAuthStatus::Cancelled,
                entry.auth_url.as_deref(),
                None,
            );
        }
        Ok(())
    }
    pub(crate) fn abort_all(&self) {
        let operations = self
            .inner
            .lock()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(_, entry)| entry.process.is_some())
                    .map(|(id, entry)| (entry.owner.clone(), id.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (owner, id) in operations {
            let _ = self.cancel(&owner, &id);
        }
    }
    pub(crate) fn invalidate(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            self.cache_epoch.fetch_add(1, Ordering::SeqCst);
            cache.clear();
        }
    }
    pub(crate) fn logout(&self, binary: &Path, cwd: &Path, name: &str) -> Result<(), String> {
        let output = command(binary, cwd, "logout", Some(name))
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err("MCP logout failed; see Pi logs".into());
        }
        self.invalidate();
        Ok(())
    }
    pub(crate) fn list(&self, binary: &Path, cwd: &Path) -> Result<Value, String> {
        if let Ok(cache) = self.cache.lock() {
            if let Some((when, value)) = cache.get(cwd) {
                if when.elapsed() < CACHE_TTL {
                    return Ok(value.clone());
                }
            }
        }
        let epoch = self.cache_epoch.load(Ordering::SeqCst);
        let output = command(binary, cwd, "list", None)
            .output()
            .map_err(|_| "MCP list could not be started".to_string())?;
        let report = parse_list_report(&output.stdout, &output.status)?;
        if let Ok(mut cache) = self.cache.lock() {
            // Epoch guard: a report that started before an invalidation must not
            // reinsert itself into the fresh cache.
            if epoch == self.cache_epoch.load(Ordering::SeqCst) {
                cache.insert(cwd.to_owned(), (Instant::now(), report.clone()));
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    fn fixture(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-pi");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        (dir, path)
    }
    #[cfg(unix)]
    fn runner() -> (McpLoginRunner, OwnerId) {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let manager = Arc::new(Mutex::new(OAuthManager::default()));
        manager.lock().unwrap().runtime_started();
        (
            McpLoginRunner::new(manager, HostEventSink::new(tx)),
            OwnerId::from_string("test-owner".into()),
        )
    }
    #[cfg(unix)]
    fn wait_terminal(runner: &McpLoginRunner, owner: &OwnerId, id: &str) -> Value {
        for _ in 0..100 {
            let status = runner.status(owner, id).unwrap();
            if status["status"] != "pending" {
                return status;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("login did not finish");
    }
    #[cfg(unix)]
    #[test]
    fn mcp_login_fake_binary_tracks_url_exit_and_rejects_concurrent_start() {
        let (dir, binary) = fixture("echo 'Sign in to MCP server \"test\" in your browser:'; echo 'https://example.org/authorize'; sleep 0.3; exit 0");
        let (runner, owner) = runner();
        let mut events = runner.events.subscribe();
        let id = runner.start(&binary, dir.path(), "test", &owner).unwrap();
        assert!(runner.start(&binary, dir.path(), "test", &owner).is_err());
        let status = wait_terminal(&runner, &owner, &id);
        assert_eq!(status["status"], "succeeded");
        assert_eq!(status["authUrl"], "https://example.org/authorize");
        let frames = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| event.value)
            .collect::<Vec<_>>();
        assert!(frames.iter().any(|frame| frame["type"] == "mcpLoginUpdate"
            && frame["payload"]["authUrl"] == "https://example.org/authorize"));
        assert!(frames
            .iter()
            .any(|frame| frame["payload"]["status"] == "succeeded"));
        let (_failing_dir, failing) = fixture("exit 1");
        let id = runner.start(&failing, dir.path(), "test", &owner).unwrap();
        assert_eq!(wait_terminal(&runner, &owner, &id)["status"], "failed");
    }
    #[cfg(unix)]
    #[test]
    fn mcp_login_cancel_and_list_cache_invalidation() {
        let (dir, binary) = fixture("if [ \"$2\" = login ]; then sleep 30; elif [ \"$2\" = logout ]; then echo logout >> calls; else echo list >> calls; echo '{\"servers\":[{\"name\":\"test\",\"scope\":\"global\",\"source\":\"/agent/mcp.json\",\"enabled\":true,\"exposure\":\"codemode\",\"transport\":\"npx -y test\",\"state\":\"needs-auth\",\"tools\":[]}],\"errors\":[]}'; fi");
        let (runner, owner) = runner();
        let id = runner.start(&binary, dir.path(), "test", &owner).unwrap();
        runner.cancel(&owner, &id).unwrap();
        assert_eq!(runner.status(&owner, &id).unwrap()["status"], "cancelled");
        let first = runner.list(&binary, dir.path()).unwrap();
        assert_eq!(first["servers"][0]["name"], "test");
        runner.list(&binary, dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        runner.logout(&binary, dir.path(), "test").unwrap();
        runner.list(&binary, dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        runner.cache.lock().unwrap().get_mut(dir.path()).unwrap().0 =
            Instant::now() - CACHE_TTL - Duration::from_secs(1);
        runner.list(&binary, dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            4
        );
    }

    #[cfg(unix)]
    fn list_fixture(json: &str, code: i32) -> (tempfile::TempDir, PathBuf) {
        let escaped = json.replace('\'', "'\\''");
        fixture(&format!("echo '{escaped}'; exit {code}"))
    }

    #[cfg(unix)]
    const ROW_NEEDS_AUTH: &str = r#"{"name":"sentry","scope":"global","source":"/agent/mcp.json","enabled":true,"exposure":"codemode","transport":"https://mcp.sentry.dev/mcp","state":"needs-auth","tools":[]}"#;

    #[cfg(unix)]
    #[test]
    fn mcp_list_accepts_exit_zero_and_one_reports() {
        let (dir, binary) = list_fixture(r#"{"servers":[],"errors":[]}"#, 0);
        let (runner, _owner) = runner();
        let report = runner.list(&binary, dir.path()).unwrap();
        assert_eq!(report["servers"].as_array().unwrap().len(), 0);
        assert_eq!(report["errors"].as_array().unwrap().len(), 0);

        // Exit 1 with a usable report is a normal partial failure, not a host
        // query failure: needs-auth rows and config errors must survive.
        let (dir, binary) = list_fixture(
            &format!(
                r#"{{"servers":[{ROW_NEEDS_AUTH}],"errors":["/agent/mcp.json: server \"broken\" must be an object"]}}"#
            ),
            1,
        );
        let report = runner.list(&binary, dir.path()).unwrap();
        assert_eq!(report["servers"][0]["state"], "needs-auth");
        assert_eq!(report["servers"][0]["name"], "sentry");
        assert_eq!(report["errors"].as_array().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn mcp_list_rejects_malformed_reports_and_foreign_exit_codes() {
        let cases = [
            (r#"[]"#, 0),                                   // top-level array
            (r#"{"servers":[]}"#, 0),                       // missing errors
            (r#"{"servers":"nope","errors":[]}"#, 0),       // wrong servers type
            (r#"{"servers":[],"errors":"nope"}"#, 0),       // wrong errors type
            (r#"{"servers":[{"name":1}],"errors":[]}"#, 0), // malformed row
            (r#"{"servers":[],"errors":[],"note":5}"#, 0),  // wrong note type
            (r#"{"servers":[],"errors":[]"#, 0),            // truncated
            (r#"{"servers":[],"errors":[]} trailing"#, 0),  // trailing junk
            (r#"{"servers":[],"errors":[]}"#, 2),           // foreign exit code
        ];
        for (json, code) in cases {
            let (dir, binary) = list_fixture(json, code);
            let (runner, _owner) = runner();
            assert!(
                runner.list(&binary, dir.path()).is_err(),
                "report {json} with exit {code} must be rejected"
            );
        }

        let (dir, binary) = fixture("kill -TERM $$");
        let (runner, _owner) = runner();
        assert!(runner.list(&binary, dir.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn mcp_list_projects_safe_transport_and_diagnostics() {
        let secret_row = r#"{"name":"docs","scope":"global","source":"/agent/mcp.json","override":"/work/.pi/mcp.json","enabled":true,"exposure":"direct","transport":"https://user:tok3n@example.test/mcp?key=SECRET","state":"error","tools":["read"],"toolExposure":{"delete_*":"hidden"},"resources":2,"error":"failed with SECRET and /usr/local/bin/secret-command --token"}"#;
        let (dir, binary) = list_fixture(
            &format!(
                r#"{{"servers":[{secret_row}],"errors":["/agent/mcp.json: bad SECRET token"],"note":"/work/.pi/mcp.json is ignored because the project is not trusted. Start pi in the project to trust it."}}"#
            ),
            1,
        );
        let (runner, _owner) = runner();
        let report = runner.list(&binary, dir.path()).unwrap();
        let serialized = report.to_string();
        for needle in [
            "SECRET",
            "tok3n",
            "user:",
            "example.test",
            "/usr/local/bin",
            "--token",
            "key=",
        ] {
            assert!(
                !serialized.contains(needle),
                "projected report must not carry {needle}: {serialized}"
            );
        }
        assert_eq!(report["servers"][0]["transport"], "http");
        assert_eq!(report["servers"][0]["source"], "/agent/mcp.json");
        assert_eq!(report["servers"][0]["override"], "/work/.pi/mcp.json");
        assert_eq!(report["servers"][0]["exposure"], "direct");
        assert_eq!(report["servers"][0]["toolExposure"]["delete_*"], "hidden");
        assert_eq!(report["servers"][0]["resources"], 2);
        assert!(report["servers"][0]["error"].is_string());
        assert_eq!(report["errors"].as_array().unwrap().len(), 1);
        assert!(report["note"].is_string());

        let stdio_row = r#"{"name":"local","scope":"project","source":"/work/.pi/mcp.json","enabled":false,"exposure":"hidden","transport":"/opt/bin/mcp --flag","state":"disabled","tools":[]}"#;
        let (dir, binary) = list_fixture(&format!(r#"{{"servers":[{stdio_row}],"errors":[]}}"#), 0);
        let report = runner.list(&binary, dir.path()).unwrap();
        assert_eq!(report["servers"][0]["transport"], "stdio");
        assert!(report.get("note").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn mcp_list_inflight_result_cannot_reinsert_after_invalidation() {
        let (dir, binary) = fixture(
            "if [ \"$2\" = list ]; then echo list >> calls; sleep 0.4; echo '{\"servers\":[],\"errors\":[]}'; fi",
        );
        let (runner, _owner) = runner();
        let (runner_a, binary_a, cwd_a) = (runner.clone(), binary.clone(), dir.path().to_owned());
        let handle = std::thread::spawn(move || runner_a.list(&binary_a, &cwd_a));
        std::thread::sleep(Duration::from_millis(80));
        runner.invalidate();
        assert!(handle.join().unwrap().is_ok());
        // The stale in-flight result must not have been cached: a fresh list
        // spawns the binary again.
        runner.list(&binary, dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }
    #[cfg(unix)]
    #[test]
    fn mcp_login_success_invalidates_cached_server_reports() {
        let (dir, binary) =
            fixture("if [ \"$2\" = list ]; then echo list >> calls; echo '{\"servers\":[],\"errors\":[]}'; else exit 0; fi");
        let (runner, owner) = runner();
        runner.list(&binary, dir.path()).unwrap();
        let id = runner.start(&binary, dir.path(), "test", &owner).unwrap();
        assert_eq!(wait_terminal(&runner, &owner, &id)["status"], "succeeded");
        runner.list(&binary, dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }
    #[test]
    fn mcp_login_command_and_url_parser() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = command(Path::new("pi"), dir.path(), "login", Some("example"));
        assert_eq!(
            cmd.get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["mcp", "login", "example", "--timeout", "300"]
        );
        let mut parser = UrlParser::default();
        assert_eq!(
            parser.feed(
                "Sign in to MCP server \"example\" in your browser:",
                "example"
            ),
            None
        );
        assert_eq!(
            parser.feed("https://localhost/auth?code=x", "example"),
            Some("https://localhost/auth?code=x".into())
        );
        assert_eq!(parser.feed("not-a-url", "example"), None);
    }
}
