// ABOUTME: Runs one tool install/update by handing the task to the embedded Pi.
// ABOUTME: One job at a time: spawn `pi --no-session -p <prompt>`, poll, re-check.

use crate::environment_probe::{probe_one, Probe, ProbeStatus, ToolAction, ToolId};
use crate::environment_prompt::render_prompt;
use serde::Serialize;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One install may run this long before it is killed.
pub(crate) const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Bytes of process output kept for the page. The rest is dropped.
const LOG_LIMIT: usize = 64 * 1024;
/// How long the pipe readers may take to finish after the child exits.
const READER_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Phase {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub(crate) tool: ToolId,
    pub(crate) action: ToolAction,
    pub(crate) phase: Phase,
    /// The exact string passed as one argv element to `pi -p`. The page copies
    /// this same string, so what the user copies is what ran.
    pub(crate) prompt: String,
    pub(crate) log: String,
    /// The host's own re-check after the process exited. Never the agent's word.
    pub(crate) probe: Option<Probe>,
    pub(crate) reason: Option<String>,
}

/// Production re-checks by running the tool's version command; tests inject a
/// scripted one.
type Prober = Arc<dyn Fn(ToolId) -> Probe + Send + Sync>;

struct Running {
    tool: ToolId,
    action: ToolAction,
    prompt: String,
    before: Option<String>,
    child: Child,
    started: Instant,
    log: Arc<Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
    cancelled: bool,
}

pub(crate) struct Installer {
    pi_binary: PathBuf,
    prober: Prober,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    last: Option<Snapshot>,
}

impl Installer {
    pub(crate) fn new(pi_binary: PathBuf) -> Self {
        Self::with_prober(pi_binary, Arc::new(probe_one))
    }

    pub(crate) fn with_prober(pi_binary: PathBuf, prober: Prober) -> Self {
        Self {
            pi_binary,
            prober,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Reject a second job while one is running: the page offers one at a time.
    pub(crate) fn start(
        &self,
        tool: ToolId,
        action: ToolAction,
        probe: &Probe,
    ) -> Result<Snapshot, String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "installer lock".to_string())?;
        if inner.running.is_some() {
            return Err("maintenance_busy: an install is already running".to_string());
        }
        let prompt = render_prompt(tool, action, probe);
        let mut command = Command::new(&self.pi_binary);
        command
            .args(["--no-session", "-p", prompt.as_str()])
            // The agent works on installing a tool, not on a workspace: give it
            // a neutral directory so a project's files are not in reach.
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::windows_child::hide_console(&mut command);
        let mut child = command.spawn().map_err(|error| {
            format!("could not start the embedded Pi to install {tool:?}: {error}")
        })?;

        let log = Arc::new(Mutex::new(String::new()));
        let readers = vec![
            read_into_log(child.stdout.take(), log.clone()),
            read_into_log(child.stderr.take(), log.clone()),
        ];
        let running = Running {
            tool,
            action,
            prompt,
            before: probe.version.clone(),
            child,
            started: Instant::now(),
            log,
            readers,
            cancelled: false,
        };
        let snapshot = running.snapshot(Phase::Running, None, None);
        inner.running = Some(running);
        inner.last = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Advance the job: a finished process is re-checked by the host here.
    pub(crate) fn status(&self) -> Option<Snapshot> {
        let mut inner = self.inner.lock().ok()?;
        if inner.running.is_none() {
            // A finished job stays readable. Without this the page loses the
            // panel one poll after the run ends, so a user who leaves Settings
            // and comes back cannot tell which tool was installed or updated.
            return inner.last.clone();
        }
        let running = inner.running.as_mut()?;
        match running.child.try_wait() {
            Ok(Some(exit)) => {
                let running = inner.running.take()?;
                let snapshot = self.finish(running, exit.success());
                inner.last = Some(snapshot.clone());
                Some(snapshot)
            }
            Ok(None) => {
                if running.started.elapsed() >= INSTALL_TIMEOUT {
                    running.cancelled = true;
                    let _ = running.child.kill();
                    let _ = running.child.wait();
                    let running = inner.running.take()?;
                    let snapshot = self.finish(running, false);
                    inner.last = Some(snapshot.clone());
                    return Some(snapshot);
                }
                Some(running.snapshot(Phase::Running, None, None))
            }
            Err(error) => {
                let running = inner.running.take()?;
                let snapshot = self.finish(running, false).with_reason(format!("{error}"));
                inner.last = Some(snapshot.clone());
                Some(snapshot)
            }
        }
    }

    pub(crate) fn cancel(&self) -> Option<Snapshot> {
        let mut inner = self.inner.lock().ok()?;
        let running = inner.running.as_mut()?;
        running.cancelled = true;
        let _ = running.child.kill();
        let _ = running.child.wait();
        let running = inner.running.take()?;
        let snapshot = self.finish(running, false);
        inner.last = Some(snapshot.clone());
        Some(snapshot)
    }

    /// Best effort on the way out: stop the child, do not wait for a re-check.
    pub(crate) fn stop_for_app_exit(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(running) = inner.running.as_mut() {
                let _ = running.child.kill();
            }
        }
    }

    /// Collect the output, then believe the host's re-check, not the exit code:
    /// a clean exit only means the agent stopped talking.
    fn finish(&self, running: Running, exit_ok: bool) -> Snapshot {
        let deadline = Instant::now() + READER_GRACE;
        while running.readers.iter().any(|h| !h.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let log = running
            .log
            .lock()
            .map(|log| log.clone())
            .unwrap_or_default();
        let probe = (self.prober)(running.tool);
        let (phase, reason) = decide(
            running.cancelled,
            exit_ok,
            running.action,
            running.before.as_deref(),
            &probe,
        );
        Snapshot {
            tool: running.tool,
            action: running.action,
            phase,
            prompt: running.prompt,
            log,
            probe: Some(probe),
            reason,
        }
    }
}

/// The re-check decides. The agent's exit code and its own claims do not.
fn decide(
    cancelled: bool,
    exit_ok: bool,
    action: ToolAction,
    before: Option<&str>,
    probe: &Probe,
) -> (Phase, Option<String>) {
    if cancelled {
        return (Phase::Cancelled, Some("install was cancelled".to_string()));
    }
    if probe.status != ProbeStatus::Ready {
        let detail = probe
            .reason
            .clone()
            .unwrap_or_else(|| "the tool is still not runnable".to_string());
        return (
            Phase::Failed,
            Some(format!("the re-check after the run failed: {detail}")),
        );
    }
    if !exit_ok {
        return (
            Phase::Failed,
            Some("the embedded Pi exited with an error".to_string()),
        );
    }
    if action == ToolAction::Update && before.is_some() && probe.version.as_deref() == before {
        return (
            Phase::Done,
            Some(format!(
                "version is unchanged ({})",
                probe.version.as_deref().unwrap_or("unknown")
            )),
        );
    }
    (Phase::Done, None)
}

fn read_into_log(
    pipe: Option<impl Read + Send + 'static>,
    log: Arc<Mutex<String>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else { return };
        let mut chunk = [0u8; 4096];
        while let Ok(read) = pipe.read(&mut chunk) {
            if read == 0 {
                break;
            }
            if let Ok(mut log) = log.lock() {
                if log.len() < LOG_LIMIT {
                    log.push_str(&String::from_utf8_lossy(&chunk[..read]));
                }
            }
        }
    })
}

impl Running {
    fn snapshot(&self, phase: Phase, probe: Option<Probe>, reason: Option<String>) -> Snapshot {
        Snapshot {
            tool: self.tool,
            action: self.action,
            phase,
            prompt: self.prompt.clone(),
            log: self.log.lock().map(|log| log.clone()).unwrap_or_default(),
            probe,
            reason,
        }
    }
}

impl Snapshot {
    fn with_reason(mut self, reason: String) -> Snapshot {
        self.reason = Some(reason);
        self
    }
}

#[cfg(test)]
mod install_tests {
    use super::*;
    use crate::environment_probe::ToolTier;

    fn probe(status: ProbeStatus, version: Option<&str>) -> Probe {
        Probe {
            tool_id: ToolId::Git,
            status,
            version: version.map(str::to_string),
            executable_path: None,
            reason: None,
            official_url: crate::environment_probe::spec(ToolId::Git).official_url,
            tier: ToolTier::Basic,
        }
    }

    #[test]
    fn the_host_recheck_decides_not_the_agent() {
        // A clean exit only means the agent stopped talking.
        let (phase, reason) = decide(
            false,
            true,
            ToolAction::Install,
            None,
            &probe(ProbeStatus::Failed, None),
        );
        assert_eq!(phase, Phase::Failed);
        assert!(reason.unwrap().contains("re-check"));

        let (phase, reason) = decide(
            false,
            true,
            ToolAction::Install,
            None,
            &probe(ProbeStatus::Ready, Some("2.55.0")),
        );
        assert_eq!(phase, Phase::Done);
        assert_eq!(reason, None);

        // An update that changed nothing says so instead of claiming success.
        let (phase, reason) = decide(
            false,
            true,
            ToolAction::Update,
            Some("2.55.0"),
            &probe(ProbeStatus::Ready, Some("2.55.0")),
        );
        assert_eq!(phase, Phase::Done);
        assert!(reason.unwrap().contains("unchanged"));

        let (phase, _) = decide(
            false,
            true,
            ToolAction::Update,
            Some("2.46.0"),
            &probe(ProbeStatus::Ready, Some("2.55.0")),
        );
        assert_eq!(phase, Phase::Done);

        // Cancelling wins over whatever the host happens to see.
        let (phase, _) = decide(
            true,
            false,
            ToolAction::Install,
            None,
            &probe(ProbeStatus::Ready, Some("2.55.0")),
        );
        assert_eq!(phase, Phase::Cancelled);
    }

    /// Regression guard for the panel that vanished after navigation: the
    /// snapshot of a finished job must survive the end of the run.
    #[test]
    fn a_finished_job_stays_readable_after_it_stops() {
        use std::io::Write;

        // A stand-in for the embedded Pi: exits immediately, ignores argv.
        let script = std::env::temp_dir().join(format!("picot-fake-pi-{}", std::process::id()));
        let mut file = std::fs::File::create(&script).expect("write fake pi");
        writeln!(file, "#!/bin/sh\necho installing\nexit 0").expect("script body");
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake pi");
        }

        let installer = Installer::with_prober(
            script.clone(),
            Arc::new(|_: ToolId| probe(ProbeStatus::Ready, Some("2.55.0"))),
        );
        let started = installer
            .start(
                ToolId::Git,
                ToolAction::Update,
                &probe(ProbeStatus::Ready, Some("2.46.0")),
            )
            .expect("start");
        assert_eq!(started.phase, Phase::Running);

        // Poll until the re-check ran, then keep asking: the panel must not
        // disappear just because nothing is running any more.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut finished = None;
        while Instant::now() < deadline {
            if let Some(snapshot) = installer.status() {
                if snapshot.phase != Phase::Running {
                    finished = Some(snapshot);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let finished = finished.expect("the job reaches a terminal phase");
        assert_eq!(finished.phase, Phase::Done);
        let afterwards = installer
            .status()
            .expect("the last snapshot stays readable");
        assert_eq!(afterwards.phase, Phase::Done);
        assert_eq!(afterwards.tool, ToolId::Git);
        assert!(!afterwards.prompt.is_empty());

        let _ = std::fs::remove_file(&script);
    }
}
