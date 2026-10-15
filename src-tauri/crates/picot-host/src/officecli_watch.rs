// ABOUTME: officecli `watch` subprocess lifecycle for browser-pane office files (spec 2026-09-22).
// ABOUTME: One watch per canonical file path; SIGTERM on stop, sweep on app exit.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

/// Tauri-managed state so the app-exit hook can reach the runtime.
pub struct OfficecliWatchState(pub std::sync::Arc<OfficecliWatchRuntime>);

pub struct OfficecliWatchRuntime {
    watches: Mutex<HashMap<PathBuf, WatchEntry>>,
}

struct WatchEntry {
    child: Child,
    port: u16,
}

impl Drop for WatchEntry {
    /// Every path that drops an entry — an overwritten insert, `status`
    /// reaping a dead server, `stop`, `stop_all`, or the runtime itself —
    /// must terminate and reap the child; otherwise an orphaned officecli
    /// keeps its port for the life of the app.
    fn drop(&mut self) {
        terminate_child(&mut self.child);
    }
}

/// Parse the `Watch: http://localhost:PORT` line from `officecli watch` stdout.
pub fn parse_watch_url(line: &str) -> Option<u16> {
    let rest = line.split("Watch:").nth(1)?;
    let url = rest.split_whitespace().next()?;
    let port = url.rsplit(':').next()?;
    port.parse::<u16>().ok()
}

fn allocate_port() -> Result<u16, String> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("port allocation failed: {e}"))?
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| format!("port allocation failed: {e}"))
}

impl OfficecliWatchRuntime {
    pub fn new() -> Self {
        Self {
            watches: Mutex::new(HashMap::new()),
        }
    }

    /// Start (or dedupe to) a watch server for `file`; blocks until the
    /// server prints its URL or `startup_timeout` elapses.
    pub fn start(&self, file: &str, startup_timeout: Duration) -> Result<String, String> {
        let canonical = std::fs::canonicalize(file).map_err(|e| format!("file not found: {e}"))?;
        {
            let mut watches = self.watches.lock().unwrap();
            if let Some(entry) = watches.get_mut(&canonical) {
                if try_wait_alive(&mut entry.child) {
                    return Ok(format!("http://127.0.0.1:{}", entry.port));
                }
            }
        }
        let port = allocate_port()?;
        let mut child = Command::new("officecli")
            .arg("watch")
            .arg(&canonical)
            .arg("--port")
            .arg(port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "officecli_missing".to_string()
                } else {
                    format!("officecli spawn failed: {e}")
                }
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "officecli stdout unavailable".to_string())?;
        let (tx, rx) = mpsc::channel::<()>();
        let port_for_reader = port;
        std::thread::spawn(move || {
            let mut lines = BufReader::new(stdout).lines();
            for line in lines.by_ref() {
                let Ok(line) = line else { break };
                if parse_watch_url(&line) == Some(port_for_reader) {
                    let _ = tx.send(());
                    // keep draining so the pipe never fills and blocks the child
                    for _ in lines.by_ref() {}
                    break;
                }
            }
            drop(tx);
        });
        let started = rx.recv_timeout(startup_timeout).is_ok();
        if !started {
            let _ = child.kill();
            let _ = child.wait();
            return Err("watch_startup_timeout".to_string());
        }
        let mut watches = self.watches.lock().unwrap();
        // Re-check under the lock: a concurrent start may have established the
        // watch while this one was spawning (up to the startup timeout).
        // Overwriting would silently orphan the earlier process.
        let established = watches
            .get_mut(&canonical)
            .and_then(|entry| try_wait_alive(&mut entry.child).then_some(entry.port));
        if let Some(existing_port) = established {
            drop(child);
            return Ok(format!("http://127.0.0.1:{existing_port}"));
        }
        watches.insert(canonical, WatchEntry { child, port });
        Ok(format!("http://127.0.0.1:{port}"))
    }

    /// Current watch URL for `file`, or None when no live watch exists.
    pub fn status(&self, file: &str) -> Option<String> {
        let canonical = std::fs::canonicalize(file).ok()?;
        let mut watches = self.watches.lock().unwrap();
        let entry = watches.get_mut(&canonical)?;
        if !try_wait_alive(&mut entry.child) {
            watches.remove(&canonical);
            return None;
        }
        Some(format!("http://127.0.0.1:{}", entry.port))
    }

    pub fn stop(&self, file: &str) -> Result<(), String> {
        let canonical = std::fs::canonicalize(file).map_err(|e| format!("file not found: {e}"))?;
        let mut watches = self.watches.lock().unwrap();
        match watches.remove(&canonical) {
            // Dropping the entry terminates and reaps the child.
            Some(entry) => {
                drop(entry);
                Ok(())
            }
            None => Err("watch_not_found".to_string()),
        }
    }

    /// Badge a document path server-side (`watch mark`); the badge survives
    /// page refreshes because the watch server owns it.
    pub fn mark(&self, file: &str, path: &str) -> Result<String, String> {
        let canonical = std::fs::canonicalize(file).map_err(|e| format!("file not found: {e}"))?;
        let output = Command::new("officecli")
            .arg("watch")
            .arg("mark")
            .arg(&canonical)
            .arg(path)
            .output()
            .map_err(|e| format!("officecli mark failed: {e}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(format!(
                "mark_failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    /// Kill every watch (app exit): SIGTERM first, wait briefly, then kill.
    pub fn stop_all(&self) {
        let mut watches = self.watches.lock().unwrap();
        for (_, entry) in watches.drain() {
            drop(entry);
        }
    }
}

fn try_wait_alive(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(None))
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        // SIGTERM first so officecli can release its port; SIGKILL fallback.
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
        for _ in 0..20 {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_watch_url_line() {
        assert_eq!(
            parse_watch_url("Watch: http://localhost:26315"),
            Some(26315)
        );
        assert_eq!(
            parse_watch_url("Watch: http://localhost:26315 "),
            Some(26315)
        );
        assert_eq!(parse_watch_url("Watching: /tmp/a.docx"), None);
        assert_eq!(parse_watch_url(""), None);
    }

    #[test]
    fn rejects_non_numeric_ports() {
        assert_eq!(parse_watch_url("Watch: http://localhost/"), None);
        assert_eq!(parse_watch_url("Watch: http://localhost:notaport"), None);
    }

    #[test]
    fn status_missing_file_is_none() {
        let runtime = OfficecliWatchRuntime::new();
        assert!(runtime.status("/definitely/not/here.docx").is_none());
    }

    #[test]
    fn stop_unknown_file_errors() {
        let runtime = OfficecliWatchRuntime::new();
        assert!(runtime.stop("/definitely/not/here.docx").is_err());
    }
}
