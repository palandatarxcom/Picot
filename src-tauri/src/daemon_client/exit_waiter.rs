// ABOUTME: Confirmed-exit waiter: the app blocks on pid + process start time, never on an RPC disconnect
// ABOUTME: or an exit code, and escalates SIGTERM -> SIGKILL on a deadline ladder (plan D12 / R4).

//! "Accepted" is not "completed" (plan D12): `daemon.stop` replies accepted and
//! the daemon drains on its own schedule. Something has to answer "is the
//! process *actually* gone?" — and the only trustworthy answer comes from the
//! OS, not from the RPC:
//!
//! - a disconnect can happen while the daemon is still tearing down, so it
//!   never counts as a clean shutdown;
//! - an exit code is unobservable for a daemon this app adopted rather than
//!   spawned, and exit code 81 machinery does not exist in this design (plan
//!   D10) — nothing may depend on a code;
//! - a pid alone is not an identity: it wraps. Every liveness question is
//!   answered with pid **and** process start time, the discipline
//!   `child_supervision` uses for pi runtimes.
//!
//! On a deadline the ladder escalates SIGTERM → SIGKILL, each with its own
//! budget. If the process still lives the outcome is [`ExitOutcome::Unconfirmed`]
//! and the caller keeps the orphan evidence (the instance file is **not**
//! deleted by the app) and names the daemon log path.

use std::process::Child;
use std::sync::Arc;
use std::time::{Duration, Instant};

use picot_host::child_supervision;

/// The pid-reuse-guarded identity of the process we are waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// The start time recorded when the daemon published its instance file.
    pub start_time: Option<String>,
}

impl ProcessIdentity {
    pub fn new(pid: u32, start_time: Option<String>) -> Self {
        Self { pid, start_time }
    }

    /// Read the identity of a pid right now (used when the app spawned it).
    pub fn of(pid: u32) -> Self {
        Self {
            pid,
            start_time: child_supervision::process_start_time(pid),
        }
    }
}

/// The outcome of waiting for a daemon to exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitOutcome {
    /// Gone within the initial deadline — the normal path.
    Confirmed,
    /// Gone only after a signal, and the *signal that worked* is recorded so
    /// the caller can say what it had to do.
    Escalated { signal: i32 },
    /// Still alive after the whole ladder: the caller must keep the orphan
    /// evidence and log the daemon log path (plan D12).
    Unconfirmed,
}

/// The OS facts the waiter needs, behind a trait so tests can script them
/// (plan Task 8 Step 1: "exit-waiter injection tests (fake identity/clock)").
pub trait ProcessHandle: Send + Sync {
    fn alive(&self, pid: u32) -> bool;
    fn start_time(&self, pid: u32) -> Option<String>;
    /// Deliver `signal`; `false` when the process is already gone.
    fn signal(&self, pid: u32, signal: i32) -> bool;
}

/// The real handle: `kill(pid, 0)` for liveness, `ps` for the start time.
pub struct SystemHandle;

impl ProcessHandle for SystemHandle {
    fn alive(&self, pid: u32) -> bool {
        // A zombie has exited but has not been reaped yet: `kill(pid, 0)` still
        // succeeds, yet the process runs nothing. Counting it as alive would
        // make every confirmed-exit wait on a child we have not reaped burn its
        // whole deadline (and then "escalate" a corpse) — the very confusion
        // this waiter exists to avoid.
        child_supervision::pid_is_alive(pid) && !is_zombie(pid)
    }

    fn start_time(&self, pid: u32) -> Option<String> {
        child_supervision::process_start_time(pid)
    }

    fn signal(&self, pid: u32, signal: i32) -> bool {
        #[cfg(unix)]
        {
            unsafe { libc::kill(pid as i32, signal) == 0 }
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, signal);
            false
        }
    }
}

/// `ps -o state=` reports `Z…` for a zombie (macOS and Linux `ps` agree on the
/// first letter). Any failure to observe the state is reported as "not a
/// zombie", because treating an unobservable process as gone could skip a real
/// teardown.
#[cfg(unix)]
fn is_zombie(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim_start()
        .starts_with('Z')
}

#[cfg(not(unix))]
fn is_zombie(_pid: u32) -> bool {
    false
}

/// SIGTERM: the polite request the daemon's own handler is built for.
pub const SIGNAL_TERM: i32 = 15;
/// SIGKILL: the last resort; the daemon cannot run any teardown, so the next
/// daemon's orphan sweep is what cleans up (plan D12).
pub const SIGNAL_KILL: i32 = 9;

/// The waiter, with injectable clock granularity and escalation budget.
pub struct ExitWaiter {
    handle: Arc<dyn ProcessHandle>,
    poll: Duration,
    escalation: Duration,
}

impl ExitWaiter {
    /// Production timing: 20ms polls, 2s per escalation step.
    pub fn new() -> Self {
        Self::with_handle(Arc::new(SystemHandle))
    }

    pub fn with_handle(handle: Arc<dyn ProcessHandle>) -> Self {
        Self {
            handle,
            poll: Duration::from_millis(20),
            escalation: Duration::from_secs(2),
        }
    }

    /// Test knob: shorter polls and escalation budgets.
    pub fn with_timing(mut self, poll: Duration, escalation: Duration) -> Self {
        self.poll = poll;
        self.escalation = escalation;
        self
    }

    /// Quit and restart transactions consume this (T8/T9); T5 wires the manager and
    /// the client, and the app's exit path deliberately only detaches for now (the
    /// accepted/completed separation lands with the quit policy). Exported here so
    /// the confirm-by-identity discipline has one implementation, not two.
    #[allow(dead_code)]
    /// Wait for the process to be gone, escalating on a deadline.
    ///
    /// `child` is the handle when *this* app spawned the daemon (a `kill -9`
    /// child is reaped here); an adopted daemon has none, and is polled by
    /// identity instead. Both paths agree on the same evidence: the OS no
    /// longer reports the process, or the pid now belongs to another process.
    pub async fn confirm_exit(
        &self,
        identity: &ProcessIdentity,
        mut child: Option<&mut Child>,
        deadline: Duration,
    ) -> ExitOutcome {
        if self
            .wait_until_gone(identity, child.as_deref_mut(), deadline)
            .await
        {
            return ExitOutcome::Confirmed;
        }
        for signal in [SIGNAL_TERM, SIGNAL_KILL] {
            log::warn!(
                "[picot] daemon pid {} outlived the deadline; sending signal {signal}",
                identity.pid
            );
            self.handle.signal(identity.pid, signal);
            if self
                .wait_until_gone(identity, child.as_deref_mut(), self.escalation)
                .await
            {
                return ExitOutcome::Escalated { signal };
            }
        }
        ExitOutcome::Unconfirmed
    }

    async fn wait_until_gone(
        &self,
        identity: &ProcessIdentity,
        mut child: Option<&mut Child>,
        budget: Duration,
    ) -> bool {
        let started = Instant::now();
        loop {
            // Reaping the child we spawned is the strongest evidence there is.
            if let Some(child) = child.as_deref_mut() {
                if let Ok(Some(_status)) = child.try_wait() {
                    return true;
                }
            }
            if self.gone(identity) {
                return true;
            }
            if started.elapsed() >= budget {
                return false;
            }
            tokio::time::sleep(self.poll).await;
        }
    }

    /// Gone = the pid is unused, or it is used by a *different* process (the
    /// recorded start time no longer matches — pid reuse).
    fn gone(&self, identity: &ProcessIdentity) -> bool {
        if !self.handle.alive(identity.pid) {
            return true;
        }
        match (
            identity.start_time.as_deref(),
            self.handle.start_time(identity.pid),
        ) {
            (Some(recorded), Some(current)) => recorded != current,
            _ => false,
        }
    }
}

impl Default for ExitWaiter {
    fn default() -> Self {
        Self::new()
    }
}

/// The system-handle convenience wrapper (plan Task 4's signature).
///
/// T8's quit policy and T9's restart transaction consume this; T5 wires the
/// manager and the client, and its exit path deliberately only detaches (the
/// accepted/completed separation is T8's). Kept exported so the
/// confirm-by-identity discipline has exactly one implementation.
#[allow(dead_code)]
pub async fn confirm_exit(
    identity: &ProcessIdentity,
    child: Option<&mut Child>,
    deadline: Duration,
) -> ExitOutcome {
    ExitWaiter::new()
        .confirm_exit(identity, child, deadline)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A scripted process: reports alive until `dies_after_signal` is delivered.
    struct FakeHandle {
        alive: Mutex<bool>,
        start_time: Mutex<Option<String>>,
        signals: Mutex<Vec<i32>>,
        /// The process exits as soon as this signal arrives (`None`: never dies).
        dies_on: Option<i32>,
    }

    impl FakeHandle {
        fn alive_until(signal: i32) -> Arc<Self> {
            Arc::new(Self {
                alive: Mutex::new(true),
                start_time: Mutex::new(Some("Mon Oct 13 09:15:02 2026".to_string())),
                signals: Mutex::new(Vec::new()),
                dies_on: Some(signal),
            })
        }

        fn signals(&self) -> Vec<i32> {
            self.signals.lock().unwrap().clone()
        }
    }

    impl ProcessHandle for FakeHandle {
        fn alive(&self, _pid: u32) -> bool {
            *self.alive.lock().unwrap()
        }

        fn start_time(&self, _pid: u32) -> Option<String> {
            self.start_time.lock().unwrap().clone()
        }

        fn signal(&self, _pid: u32, signal: i32) -> bool {
            self.signals.lock().unwrap().push(signal);
            if self.dies_on == Some(signal) {
                *self.alive.lock().unwrap() = false;
            }
            true
        }
    }

    fn identity() -> ProcessIdentity {
        ProcessIdentity {
            pid: 4242,
            start_time: Some("Mon Oct 13 09:15:02 2026".to_string()),
        }
    }

    fn waiter(handle: Arc<dyn ProcessHandle>) -> ExitWaiter {
        ExitWaiter::with_handle(handle)
            .with_timing(Duration::from_millis(10), Duration::from_millis(60))
    }

    /// A daemon that stops promptly is confirmed with no signal at all: the
    /// normal, healthy path.
    #[tokio::test]
    async fn a_prompt_exit_is_confirmed_without_signalling() {
        let handle = Arc::new(FakeHandle {
            alive: Mutex::new(false),
            start_time: Mutex::new(None),
            signals: Mutex::new(Vec::new()),
            dies_on: None,
        });
        let outcome = waiter(handle.clone())
            .confirm_exit(&identity(), None, Duration::from_millis(100))
            .await;
        assert_eq!(outcome, ExitOutcome::Confirmed);
        assert!(
            handle.signals().is_empty(),
            "a clean shutdown is never signalled"
        );
    }

    /// An adopted daemon dies on SIGTERM: the ladder stops at step one and
    /// records which signal worked.
    #[tokio::test]
    async fn an_adopted_daemon_that_ignores_its_deadline_is_escalated() {
        let handle = FakeHandle::alive_until(SIGNAL_TERM);
        let outcome = waiter(handle.clone())
            .confirm_exit(&identity(), None, Duration::from_millis(50))
            .await;
        assert_eq!(
            outcome,
            ExitOutcome::Escalated {
                signal: SIGNAL_TERM
            }
        );
        assert_eq!(
            handle.signals(),
            vec![SIGNAL_TERM],
            "SIGKILL is not sent once SIGTERM worked"
        );
    }

    #[tokio::test]
    async fn a_stuck_process_is_escalated_all_the_way_to_sigkill() {
        let handle = FakeHandle::alive_until(SIGNAL_KILL);
        let outcome = waiter(handle.clone())
            .confirm_exit(&identity(), None, Duration::from_millis(50))
            .await;
        assert_eq!(
            outcome,
            ExitOutcome::Escalated {
                signal: SIGNAL_KILL
            }
        );
        assert_eq!(
            handle.signals(),
            vec![SIGNAL_TERM, SIGNAL_KILL],
            "the ladder is ordered"
        );
    }

    /// The pid-reuse guard: a live pid whose start time differs is somebody
    /// else, so the daemon is confirmed gone and **never signalled** — killing
    /// an unrelated process would be far worse than waiting.
    #[tokio::test]
    async fn a_reused_pid_is_confirmed_and_never_signalled() {
        let handle = Arc::new(FakeHandle {
            alive: Mutex::new(true),
            start_time: Mutex::new(Some("Tue Oct 14 07:00:00 2026".to_string())),
            signals: Mutex::new(Vec::new()),
            dies_on: None,
        });
        let outcome = waiter(handle.clone())
            .confirm_exit(&identity(), None, Duration::from_millis(50))
            .await;
        assert_eq!(outcome, ExitOutcome::Confirmed);
        assert!(
            handle.signals().is_empty(),
            "a reused pid is not our process to kill"
        );
    }

    #[tokio::test]
    async fn a_process_that_never_dies_is_unconfirmed() {
        let handle = Arc::new(FakeHandle {
            alive: Mutex::new(true),
            start_time: Mutex::new(Some("Mon Oct 13 09:15:02 2026".to_string())),
            signals: Mutex::new(Vec::new()),
            dies_on: None,
        });
        let outcome = waiter(handle.clone())
            .confirm_exit(&identity(), None, Duration::from_millis(40))
            .await;
        assert_eq!(
            outcome,
            ExitOutcome::Unconfirmed,
            "an unconfirmed exit keeps the orphan evidence (D12)"
        );
        assert_eq!(handle.signals(), vec![SIGNAL_TERM, SIGNAL_KILL]);
    }

    /// The child-handle path against a real process: `sleep` exits on its own,
    /// and the waiter reaps it instead of polling the pid.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_spawned_child_is_reaped_within_the_deadline() {
        let mut child = std::process::Command::new("sleep")
            .arg("0.2")
            .spawn()
            .expect("spawn sleep");
        let identity = ProcessIdentity::of(child.id());
        let outcome = ExitWaiter::new()
            .confirm_exit(&identity, Some(&mut child), Duration::from_secs(5))
            .await;
        assert_eq!(outcome, ExitOutcome::Confirmed);
    }

    /// An *adopted* process (no child handle) that exits on its own is
    /// confirmed by pid + start time alone.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_adopted_process_is_confirmed_by_identity_polling() {
        let child = std::process::Command::new("sleep")
            .arg("0.2")
            .spawn()
            .expect("spawn sleep");
        let identity = ProcessIdentity::of(child.id());
        // No handle kept: the process is only observable through the OS, which
        // is exactly the adopted-daemon path.
        drop(child);
        let outcome = ExitWaiter::new()
            .confirm_exit(&identity, None, Duration::from_secs(5))
            .await;
        assert_eq!(outcome, ExitOutcome::Confirmed);
    }

    /// A process that ignores SIGTERM is escalated by the *system* handle too
    /// (the ladder is not a fake-only path).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_real_process_ignoring_term_is_killed() {
        // `sh -c 'trap "" TERM; sleep 5'` ignores SIGTERM.
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 5")
            .spawn()
            .expect("spawn sh");
        let identity = ProcessIdentity::of(child.id());
        drop(child);
        let outcome = ExitWaiter::new()
            .with_timing(Duration::from_millis(20), Duration::from_millis(400))
            .confirm_exit(&identity, None, Duration::from_millis(200))
            .await;
        assert_eq!(
            outcome,
            ExitOutcome::Escalated {
                signal: SIGNAL_KILL
            },
            "SIGTERM is ignored, SIGKILL is not"
        );
    }
}
