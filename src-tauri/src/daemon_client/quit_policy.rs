// ABOUTME: The app-private quit policy (`keepRunningAfterQuit`) and the quit transaction: app exit
// ABOUTME: blocks on a confirmed daemon death and never treats an RPC disconnect as completed.

//! # Quit policy (plan Task 8 / D12, spec §3.2.2)
//!
//! Two halves that must not be confused:
//!
//! 1. **The policy** — `keepRunningAfterQuit` is *app* state. It lives in
//!    `<app_config_dir>/picot-daemon-settings.json` (0600, atomic publish), the
//!    app process is its only reader *and* writer, and the daemon never learns
//!    the value (spec §3.2.2: no two-process truth to synchronize). Missing or
//!    corrupt → warn + `false`, never a startup failure (spec §3.5). Toggling it
//!    writes the file and nothing else — no restart, no pause, no probe; the
//!    *next* quit reads it.
//! 2. **The quit transaction** — when the policy is `false` (the default) and
//!    the app is in external mode, app exit means "stop the daemon, then leave".
//!    `daemon.stop` replies **accepted**; that is not **completed** (plan D12).
//!    Only the OS can answer "is that process gone?", so the flow blocks on
//!    [`ExitWaiter`] (pid + process start time, SIGTERM → SIGKILL escalation). A
//!    dropped admin channel — including a drop *before* the reply — never counts
//!    as a clean shutdown and never short-circuits the waiter.
//!
//! Failure is an outcome, not a panic: if the daemon outlives the whole ladder
//! the flow returns [`QuitOutcome::Unconfirmed`], the app leaves the daemon's
//! orphan evidence alone (it never deletes the instance file — that file is the
//! daemon's, published and cleared by the daemon only) and names the daemon log
//! path, which is the only trace a detached daemon leaves.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use picot_host::daemon::instance;
use serde::{Deserialize, Serialize};

use super::admin_client::AdminClient;
use super::exit_waiter::{ExitOutcome, ExitWaiter, ProcessIdentity};

/// The app-private settings file (spec §3.2.2 / plan D8).
pub const SETTINGS_FILE_NAME: &str = "picot-daemon-settings.json";

/// How long app exit waits for a confirmed daemon death **before** escalating
/// (plan Task 8 Step 2: "confirm_exit blocks app exit (10s deadline, SIGTERM →
/// SIGKILL escalation per the waiter ladder)").
pub const QUIT_CONFIRM_DEADLINE: Duration = Duration::from_secs(10);

/// The persisted document. One key today; `default` keeps a file that predates
/// the key (or a hand-edited one) readable instead of "corrupt".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct DaemonSettings {
    #[serde(rename = "keepRunningAfterQuit", default)]
    keep_running_after_quit: bool,
}

/// Where the quit policy lives inside a config directory.
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SETTINGS_FILE_NAME)
}

/// What reading the policy file produced. Kept separate from the public
/// boolean so the *reason* (missing vs corrupt) is observable — corrupt must
/// warn, missing must not.
#[derive(Debug, PartialEq, Eq)]
enum PolicyRead {
    /// No file yet: the documented default, silently.
    Missing,
    /// Unreadable or unparseable: the documented fallback, loudly.
    Corrupt(String),
    Value(bool),
}

fn read_policy(path: &Path) -> PolicyRead {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return PolicyRead::Missing,
        Err(error) => {
            return PolicyRead::Corrupt(format!("cannot read {}: {error}", path.display()))
        }
    };
    match serde_json::from_str::<DaemonSettings>(&raw) {
        Ok(settings) => PolicyRead::Value(settings.keep_running_after_quit),
        Err(error) => PolicyRead::Corrupt(format!(
            "{} is not a valid daemon settings document: {error}",
            path.display()
        )),
    }
}

/// The persisted quit policy: **false** unless the file says otherwise
/// (spec §3.2.2 default, spec §3.5 corrupt fallback).
///
/// Never fails and never blocks startup: a corrupt file is a warning plus the
/// default, exactly like a missing one (only the log level differs).
pub fn keep_running_after_quit(config_dir: &Path) -> bool {
    let path = settings_path(config_dir);
    match read_policy(&path) {
        PolicyRead::Value(value) => value,
        PolicyRead::Missing => false,
        PolicyRead::Corrupt(reason) => {
            log::warn!("[picot] {reason}; falling back to keepRunningAfterQuit=false");
            false
        }
    }
}

/// Persist the quit policy (0600, atomic). Writes state only — the daemon is
/// not touched, not even probed (spec §3.2.2: changing the toggle must not
/// restart or pause anything; the next quit follows the new value).
///
/// T10's `daemon_set_keep_running` command is the caller.
pub fn set_keep_running_after_quit(config_dir: &Path, value: bool) -> Result<(), String> {
    let document = DaemonSettings {
        keep_running_after_quit: value,
    };
    let encoded = serde_json::to_vec(&document)
        .map_err(|error| format!("cannot encode the daemon settings: {error}"))?;
    instance::write_atomic_bytes(&settings_path(config_dir), &encoded)
}

/// Ask the running daemon to stop. `Ok(())` means "accepted" (or the daemon
/// answered in a way that proves it heard us); `Err` is *not* fatal, because a
/// channel that drops before `admin_result` is indistinguishable from a daemon
/// already draining — see [`QuitFlow::run`].
#[async_trait::async_trait]
pub trait StopRequester: Send + Sync {
    async fn request_stop(&self) -> Result<(), String>;
}

/// The real requester: `daemon.stop` over the app's already-attached admin
/// channel (which holds the manager lease, so the op is authorized).
pub struct AdminStopRequester {
    client: Arc<AdminClient>,
}

impl AdminStopRequester {
    pub fn new(client: Arc<AdminClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl StopRequester for AdminStopRequester {
    async fn request_stop(&self) -> Result<(), String> {
        let reply = self
            .client
            .request("daemon.stop", serde_json::json!({}))
            .await?;
        // The daemon replies `{accepted:true}` and *then* drains (its own
        // ordering), so this reply is only the accept half.
        if reply.get("accepted").and_then(serde_json::Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(format!("the daemon did not accept daemon.stop: {reply}"))
        }
    }
}

/// What a graceful exit trigger must do, before any process is touched.
///
/// Pure and total on purpose: **every** graceful trigger — ⌘Q,
/// SIGHUP/SIGINT/SIGTERM — funnels through this one table (plan Task 8 Step 2:
/// "SIGTERM/SIGINT/SIGHUP on app → same as ⌘Q policy"), so the tier decision is
/// testable without a process and cannot drift per entry point. SIGKILL has no
/// action because it cannot be caught at all: the app dies instantly and the
/// detached daemon plus the next launch's orphan sweep are the design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitAction {
    /// `keepRunningAfterQuit=true`: release the admin channel only. The daemon
    /// and its resident agents keep running.
    DetachOnly,
    /// Default: stop the daemon and block on a confirmed exit.
    StopAndConfirm,
}

/// T12 deleted the in-process mode, so the mode no longer enters this table:
/// the daemon is always a separate process and a quit either detaches or stops
/// it for real.
pub fn quit_action(keep_running_after_quit: bool) -> QuitAction {
    if keep_running_after_quit {
        QuitAction::DetachOnly
    } else {
        QuitAction::StopAndConfirm
    }
}

/// How app exit resolved the daemon (plan Task 8 Step 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitOutcome {
    /// `keepRunningAfterQuit`: the app only released the admin channel; the
    /// daemon and its agents keep running.
    Detached,
    /// Asked to stop and confirmed gone inside the first deadline.
    Stopped,
    /// Confirmed gone only after a signal — `signal` is the one that worked.
    Escalated { signal: i32 },
    /// Still alive after the whole ladder: the orphan evidence is retained and
    /// [`QuitFlow::unconfirmed_message`] names the daemon log path.
    Unconfirmed,
}

/// The quit transaction: one drain, one confirmed exit, idempotent.
pub struct QuitFlow {
    requester: Arc<dyn StopRequester>,
    waiter: ExitWaiter,
    identity: ProcessIdentity,
    /// The child handle when this app spawned the daemon (the reaping path);
    /// `None` for an adopted daemon, which is polled by identity.
    child: Mutex<Option<Child>>,
    /// The daemon's instance file — **never** deleted here (plan D12): it is the
    /// orphan evidence a timed-out stop must leave for the next daemon's sweep.
    instance_path: PathBuf,
    /// The daemon's log file, named by a timeout message.
    log_path: PathBuf,
    deadline: Duration,
    /// Serializes concurrent triggers (a signal racing ⌘Q): the second entrant
    /// waits for the first and then reads its outcome instead of draining twice.
    serial: tokio::sync::Mutex<()>,
    outcome: Mutex<Option<QuitOutcome>>,
}

impl QuitFlow {
    pub fn new(
        requester: Arc<dyn StopRequester>,
        waiter: ExitWaiter,
        identity: ProcessIdentity,
        child: Option<Child>,
        instance_path: PathBuf,
        log_path: PathBuf,
        deadline: Duration,
    ) -> Self {
        Self {
            requester,
            waiter,
            identity,
            child: Mutex::new(child),
            instance_path,
            log_path,
            deadline,
            serial: tokio::sync::Mutex::new(()),
            outcome: Mutex::new(None),
        }
    }

    /// Stop the daemon and confirm the death. Idempotent: a repeated trigger
    /// (the process receives SIGTERM while `RunEvent::Exit` is already running
    /// the flow) sees the recorded outcome and performs no second drain.
    pub async fn run(&self) -> QuitOutcome {
        let _serial = self.serial.lock().await;
        if let Some(resolved) = *self.outcome.lock().expect("quit outcome lock") {
            log::info!("[picot] app exit already resolved as {resolved:?}; not draining twice");
            return resolved;
        }
        let outcome = self.perform().await;
        *self.outcome.lock().expect("quit outcome lock") = Some(outcome);
        outcome
    }

    async fn perform(&self) -> QuitOutcome {
        if let Err(error) = self.requester.request_stop().await {
            // Not fatal, and deliberately not an early return: a disconnect
            // proves nothing about the process (D12), and the daemon may well
            // be draining while the socket is already closed. Only the OS
            // answers below.
            log::warn!(
                "[picot] daemon.stop was not acknowledged ({error}); \
                 confirming the exit of pid {} by identity",
                self.identity.pid
            );
        }
        let mut child = self.child.lock().expect("quit child lock").take();
        let outcome = self
            .waiter
            .confirm_exit(&self.identity, child.as_mut(), self.deadline)
            .await;
        match outcome {
            ExitOutcome::Confirmed => {
                log::info!(
                    "[picot] Picot daemon pid {} exited (confirmed)",
                    self.identity.pid
                );
                QuitOutcome::Stopped
            }
            ExitOutcome::Escalated { signal } => {
                log::warn!(
                    "[picot] Picot daemon pid {} needed signal {signal} to exit",
                    self.identity.pid
                );
                QuitOutcome::Escalated { signal }
            }
            ExitOutcome::Unconfirmed => {
                log::error!("[picot] {}", self.unconfirmed_message());
                QuitOutcome::Unconfirmed
            }
        }
    }

    /// The message an unconfirmed stop must leave behind (plan D12): the pid,
    /// the explicit "orphan evidence retained" statement, and the daemon log
    /// path. Public so T9's restart transaction can surface the same sentence.
    pub fn unconfirmed_message(&self) -> String {
        format!(
            "the Picot daemon (pid {}) is still alive after daemon.stop and the \
             SIGTERM/SIGKILL ladder; keeping the orphan evidence {} so the next daemon \
             sweeps it (daemon log: {})",
            self.identity.pid,
            self.instance_path.display(),
            self.log_path.display(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_client::exit_waiter::{ProcessHandle, SIGNAL_KILL, SIGNAL_TERM};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A requester that records every call and answers with a scripted result.
    struct FakeRequester {
        calls: AtomicUsize,
        answer: Result<(), String>,
    }

    impl FakeRequester {
        fn accepting() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                answer: Ok(()),
            })
        }

        fn dropping() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                answer: Err("the admin channel closed while the request was in flight".to_string()),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl StopRequester for FakeRequester {
        async fn request_stop(&self) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    /// A scripted daemon process: alive until `dies_on` arrives (`None`: never
    /// dies), recording every signal it was sent, in order.
    struct FakeHandle {
        alive: Mutex<bool>,
        start_time: Mutex<Option<String>>,
        signals: Mutex<Vec<i32>>,
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

        fn immortal() -> Arc<Self> {
            Arc::new(Self {
                alive: Mutex::new(true),
                start_time: Mutex::new(Some("Mon Oct 13 09:15:02 2026".to_string())),
                signals: Mutex::new(Vec::new()),
                dies_on: None,
            })
        }

        fn already_gone() -> Arc<Self> {
            Arc::new(Self {
                alive: Mutex::new(false),
                start_time: Mutex::new(None),
                signals: Mutex::new(Vec::new()),
                dies_on: None,
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
        ProcessIdentity::new(4242, Some("Mon Oct 13 09:15:02 2026".to_string()))
    }

    /// Test timing: 5ms polls, 40ms per escalation step, 40ms initial deadline.
    fn flow(
        requester: Arc<dyn StopRequester>,
        handle: Arc<FakeHandle>,
        instance_path: PathBuf,
    ) -> QuitFlow {
        QuitFlow::new(
            requester,
            ExitWaiter::with_handle(handle)
                .with_timing(Duration::from_millis(5), Duration::from_millis(40)),
            identity(),
            None,
            instance_path,
            PathBuf::from("/tmp/data/picot-daemon.log"),
            Duration::from_millis(40),
        )
    }

    // ─── The settings file (spec §3.2.2 / §3.5, plan Task 8 Step 1) ──────────

    #[test]
    fn a_missing_settings_file_is_the_documented_default_false() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        assert_eq!(read_policy(&settings_path(&config)), PolicyRead::Missing);
        assert!(!keep_running_after_quit(&config), "default is false");
        assert!(
            !settings_path(&config).exists(),
            "reading the policy must never create the file"
        );
    }

    #[test]
    fn a_corrupt_settings_file_falls_back_to_false_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        let path = settings_path(&config);
        std::fs::write(&path, b"{not json at all").unwrap();
        match read_policy(&path) {
            PolicyRead::Corrupt(reason) => {
                assert!(
                    reason.contains(SETTINGS_FILE_NAME),
                    "the warning names the file: {reason}"
                );
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
        assert!(
            !keep_running_after_quit(&config),
            "corrupt must not block startup and must not enable the policy"
        );
        // The unreadable-file branch (a directory where a file is expected) is
        // the other corrupt case; it must not panic either.
        let nested = config.join("as-a-directory");
        std::fs::create_dir_all(settings_path(&nested)).unwrap();
        assert!(!keep_running_after_quit(&nested));
    }

    #[test]
    fn the_toggle_round_trips_and_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        set_keep_running_after_quit(&config, true).expect("write true");
        assert!(keep_running_after_quit(&config), "true round-trips");
        assert_eq!(
            std::fs::read_to_string(settings_path(&config)).unwrap(),
            r#"{"keepRunningAfterQuit":true}"#,
            "the on-disk shape is the documented document, not the Rust field name"
        );
        set_keep_running_after_quit(&config, false).expect("write false");
        assert!(!keep_running_after_quit(&config), "false round-trips");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(settings_path(&config))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the settings file is app-private");
        }
        // A document missing the key (an older/hand-edited file) reads false
        // rather than "corrupt".
        std::fs::write(settings_path(&config), b"{}").unwrap();
        assert_eq!(
            read_policy(&settings_path(&config)),
            PolicyRead::Value(false)
        );
    }

    // ─── The tier table (plan Task 8 Step 2: every graceful trigger) ─────────

    /// One table for ⌘Q, SIGHUP, SIGINT and SIGTERM: the policy decides, the
    /// trigger never does.
    #[test]
    fn every_graceful_trigger_maps_through_the_same_action_table() {
        assert_eq!(quit_action(false), QuitAction::StopAndConfirm);
        assert_eq!(quit_action(true), QuitAction::DetachOnly);
    }

    // ─── The quit transaction (plan D12, Task 8 Step 1) ─────────────────────

    #[test]
    fn a_prompt_stop_is_confirmed_without_any_signal() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("picot-daemon.json");
        std::fs::write(&instance, b"{}").unwrap();
        let requester = FakeRequester::accepting();
        let handle = FakeHandle::already_gone();
        let flow = flow(requester.clone(), handle.clone(), instance.clone());
        let outcome = rt.block_on(flow.run());
        assert_eq!(outcome, QuitOutcome::Stopped);
        assert_eq!(requester.calls(), 1, "daemon.stop was requested once");
        assert!(
            handle.signals().is_empty(),
            "a clean stop is never signalled"
        );
        assert!(instance.exists(), "the app never deletes the daemon's file");
    }

    /// Slow drain: accepted, but the process lingers past the deadline — the
    /// ladder runs, in order, and the signal that worked is recorded.
    #[test]
    fn a_slow_drain_records_the_escalation_ladder_in_order() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("picot-daemon.json");
        let handle = FakeHandle::alive_until(SIGNAL_KILL);
        let flow = flow(FakeRequester::accepting(), handle.clone(), instance);
        let outcome = rt.block_on(flow.run());
        assert_eq!(
            outcome,
            QuitOutcome::Escalated {
                signal: SIGNAL_KILL
            }
        );
        assert_eq!(
            handle.signals(),
            vec![SIGNAL_TERM, SIGNAL_KILL],
            "SIGTERM first, SIGKILL only if SIGTERM failed"
        );
    }

    #[test]
    fn a_sigterm_success_stops_the_ladder() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("picot-daemon.json");
        let handle = FakeHandle::alive_until(SIGNAL_TERM);
        let flow = flow(FakeRequester::accepting(), handle.clone(), instance);
        let outcome = rt.block_on(flow.run());
        assert_eq!(
            outcome,
            QuitOutcome::Escalated {
                signal: SIGNAL_TERM
            }
        );
        assert_eq!(handle.signals(), vec![SIGNAL_TERM]);
    }

    /// Lost ACK: the channel drops before `admin_result`, so `daemon.stop` is
    /// *unacknowledged* — and the flow still tracks the verified pid to a
    /// confirmed exit. There is no second stop request and no early return.
    #[test]
    fn a_lost_ack_still_confirms_the_verified_pid() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("picot-daemon.json");
        std::fs::write(&instance, b"{}").unwrap();
        let requester = FakeRequester::dropping();
        let handle = FakeHandle::alive_until(SIGNAL_TERM);
        let flow = flow(requester.clone(), handle.clone(), instance.clone());
        let outcome = rt.block_on(flow.run());
        assert_eq!(
            outcome,
            QuitOutcome::Escalated {
                signal: SIGNAL_TERM
            },
            "a dropped channel never counts as completed (plan D12)"
        );
        assert_eq!(
            requester.calls(),
            1,
            "no repeated stop request after a drop"
        );
        assert!(
            instance.exists(),
            "the instance file is the daemon's, kept even on escalation"
        );
    }

    /// A repeated trigger while the flow is running (a signal racing ⌘Q) must
    /// not drain twice: the second entrant waits and reads the same outcome.
    #[test]
    fn a_repeated_trigger_is_idempotent_and_stops_once() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("picot-daemon.json");
        let requester = FakeRequester::accepting();
        let handle = FakeHandle::alive_until(SIGNAL_TERM);
        let flow = Arc::new(flow(requester.clone(), handle.clone(), instance));
        let (first, second) = rt.block_on(async { tokio::join!(flow.run(), flow.run()) });
        assert_eq!(first, second);
        assert_eq!(
            requester.calls(),
            1,
            "one drain, however many triggers arrive"
        );
        assert_eq!(
            handle.signals(),
            vec![SIGNAL_TERM],
            "the escalation ladder runs once"
        );
    }

    /// Timeout: the process survives the whole ladder. The outcome is
    /// `Unconfirmed`, the orphan evidence stays in place, and the message names
    /// the daemon log path (plan D12).
    #[test]
    fn an_unconfirmed_timeout_keeps_the_orphan_evidence_and_names_the_log() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("picot-daemon.json");
        std::fs::write(&instance, b"{\"pid\":4242}").unwrap();
        let handle = FakeHandle::immortal();
        let flow = flow(FakeRequester::accepting(), handle.clone(), instance.clone());
        let outcome = rt.block_on(flow.run());
        assert_eq!(outcome, QuitOutcome::Unconfirmed);
        assert_eq!(
            handle.signals(),
            vec![SIGNAL_TERM, SIGNAL_KILL],
            "the whole ladder is attempted before giving up"
        );
        assert!(
            instance.exists(),
            "orphan evidence retained: the app must never delete the instance file"
        );
        assert_eq!(
            std::fs::read_to_string(&instance).unwrap(),
            "{\"pid\":4242}",
            "and it is untouched, not rewritten"
        );
        let message = flow.unconfirmed_message();
        assert!(
            message.contains("/tmp/data/picot-daemon.log"),
            "the message names the daemon log path: {message}"
        );
        assert!(
            message.contains("4242"),
            "the message names the verified pid: {message}"
        );
        assert!(
            message.contains(&instance.display().to_string()),
            "the message names the retained evidence: {message}"
        );
    }
}
