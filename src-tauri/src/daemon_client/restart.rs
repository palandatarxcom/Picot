// ABOUTME: The app-managed restart transaction (plan Task 9 / D10): stop the captured instance,
// ABOUTME: confirm its death by identity, spawn exactly one replacement, rebuild bindings, migrate views.

//! # Restart transaction (plan Task 9, D10)
//!
//! Restarting the daemon changes the WebView's page **origin** and every
//! capability, so it cannot be a bare WebSocket retry (B2): the app owns a
//! seven-step replacement transaction.
//!
//! 1. **Capture pre-state** — the open windows (`{label, ownerHome, route}`) and
//!    the quit-policy value. Done by the app shell
//!    (`crate::capture_open_views`, fed by the window ledger) and handed to
//!    [`RestartCoordinator::run`].
//! 2. **Stop + confirm** — `daemon.stop` (*accepted*), then
//!    [`ExitWaiter::confirm_exit`] on the captured identity (pid + process start
//!    time). An unconfirmed exit **aborts** the transaction: spawning a second
//!    daemon against an unverified live one is the one thing this must never do.
//!    An RPC disconnect and an exit code prove nothing (plan D12, no code 81).
//! 3. **Spawn exactly one replacement** and wait ready + attach the lease
//!    ([`ReplacementSpawner`]; the daemon's single-instance lock is the
//!    backstop).
//! 4. **Rebuild bindings** — one fresh `owner.create` per captured window.
//! 5. **Migrate each view** — controlled window **recreation** at the new origin
//!    with the new capability init script. Implementation-time decision (plan
//!    Task 9 step 5 offers (a) recreation or (b) in-place navigation): (a) is the
//!    only deterministic option, because a Tauri WebView's initialization script
//!    cannot be re-injected after creation — a navigated page would keep
//!    presenting the *stale* capability and fail its handshake. Recreation also
//!    needs no `navigation_permit`: the fresh owner's committed origin *is* the
//!    new origin.
//! 6. **Restore the session route** — the re-created window is loaded at
//!    `new_origin + captured route` (SPA `pushState` routes included, which is
//!    why the route is read from the live document URL).
//! 7. **Refresh status** — [`RestartReport`] carries the new pid/version/buildId,
//!    the new origin, the attachment epoch and every migrated view.
//!
//! ## Concurrency
//!
//! Two triggers (a user clicking Restart twice, a command racing another) must
//! produce **one** replacement: [`RestartGate`] serializes the transaction and
//! coalesces requests that arrive while it runs (or before it settles) onto its
//! outcome. A request that arrives afterwards is a genuine new restart.
//!
//! ## Seams
//!
//! Everything process- or Tauri-shaped is injected: [`StopRequester`] (T8),
//! [`ExitWaiter`] + [`ProcessHandle`] (T8), [`ReplacementSpawner`] and
//! [`RestartLink`]. The ordering discipline this file exists to enforce is
//! therefore provable without a process, and *is* also proven against a real
//! process in the tests below.

use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::exit_waiter::{ExitOutcome, ExitWaiter, ProcessIdentity};
use super::manager::{DaemonManager, EnsureOutcome};
use super::quit_policy::StopRequester;
use super::window_ledger::view_url;

/// How long the captured instance may take to die before the ladder escalates
/// (the same budget T8's quit flow uses).
pub const RESTART_CONFIRM_DEADLINE: Duration = Duration::from_secs(10);

/// One open window as the transaction captured it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedView {
    pub label: String,
    pub owner: String,
    /// The owner's home, when the app minted the owner (the landing window).
    /// `None` for a workspace owner, whose root the daemon resolves from its
    /// own registry (the T9 `owner.create` amendment).
    pub owner_home: Option<String>,
    /// The registered workspace the view is bound to, if any.
    pub workspace_id: Option<String>,
    /// The route to restore, `None` for the origin root.
    pub route: Option<String>,
}

impl CapturedView {
    /// The URL this view is re-created at on the replacement daemon.
    pub fn url(&self, host_origin: &str) -> String {
        view_url(host_origin, self.route.as_deref())
    }
}

/// The replacement daemon as the transaction needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartTarget {
    pub pid: u32,
    pub process_start_time: Option<String>,
    pub host_origin: String,
    pub admin_origin: String,
    pub admin_token: String,
    pub version: String,
    pub build_id: String,
}

impl RestartTarget {
    /// The identity a *later* transaction would confirm against (pid + start
    /// time, never the pid alone).
    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity::new(self.pid, self.process_start_time.clone())
    }
}

/// What `RestartLink::rebuild_owner` produced for one captured view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerBinding {
    pub owner: String,
    pub capability: String,
    /// The view could not be rebuilt for its captured binding and lands on the
    /// **landing** route instead: a working window on the wrong route beats a
    /// dead window (and beats failing the whole transaction for one view).
    pub degraded: bool,
}

/// One view's migration: the window to recreate and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewMigration {
    pub label: String,
    pub owner: String,
    pub capability: String,
    /// The absolute URL the window is re-created at.
    pub url: String,
    /// The workspace the *new* window is bound to; `None` for a landing view
    /// (and for a degraded view, which lands on the origin root as a landing
    /// window).
    pub workspace_id: Option<String>,
    pub degraded: bool,
}

/// The migration rows for `views`, paired by position with `bindings`.
///
/// Pure so the route mapping is provable without a WebView: every captured view
/// produces exactly one row, and a degraded view lands on the origin root.
pub fn migration_rows(
    views: &[CapturedView],
    bindings: &[OwnerBinding],
    host_origin: &str,
) -> Vec<ViewMigration> {
    assert_eq!(
        views.len(),
        bindings.len(),
        "one binding per captured view: the coordinator builds them in lockstep"
    );
    views
        .iter()
        .zip(bindings)
        .map(|(view, binding)| ViewMigration {
            label: view.label.clone(),
            owner: binding.owner.clone(),
            capability: binding.capability.clone(),
            url: if binding.degraded {
                view_url(host_origin, None)
            } else {
                view.url(host_origin)
            },
            workspace_id: if binding.degraded {
                None
            } else {
                view.workspace_id.clone()
            },
            degraded: binding.degraded,
        })
        .collect()
}

/// Where a transaction stopped when it could not finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartStage {
    StopAndConfirm,
    Spawn,
    Attach,
    RebuildBindings,
    MigrateViews,
}

impl RestartStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::StopAndConfirm => "stop-and-confirm",
            Self::Spawn => "spawn",
            Self::Attach => "attach",
            Self::RebuildBindings => "rebuild-bindings",
            Self::MigrateViews => "migrate-views",
        }
    }
}

/// The finished transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartReport {
    pub pid: u32,
    pub host_origin: String,
    pub version: String,
    pub build_id: String,
    pub epoch: u64,
    /// How the captured instance went away (confirmed, or only after a signal).
    /// `None` when the daemon's process identity was never captured: the stop
    /// request's own return is then the only evidence, and nothing is waited on
    /// or signalled (the app always captures one — T12 deleted the in-process
    /// daemon, whose `None` arm this used to be).
    pub old_exit: Option<ExitOutcome>,
    pub old_pid: Option<u32>,
    pub views: Vec<ViewMigration>,
    /// Read at transaction start and never written (spec §3.2.2): a restart
    /// does not change the quit policy.
    pub keep_running_after_quit: bool,
}

/// Restarted, or stopped at a named stage with a reason the caller can show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestartOutcome {
    Restarted(RestartReport),
    Aborted { stage: RestartStage, reason: String },
}

impl RestartOutcome {
    pub fn report(&self) -> Option<&RestartReport> {
        match self {
            Self::Restarted(report) => Some(report),
            Self::Aborted { .. } => None,
        }
    }

    /// T10's command surface and the tests read this; the transaction itself
    /// only needs [`Self::report`].
    #[allow(dead_code)]
    pub fn is_restarted(&self) -> bool {
        self.report().is_some()
    }

    /// The message a failure must leave behind: the stage and the reason.
    pub fn failure_message(&self) -> Option<String> {
        match self {
            Self::Restarted(_) => None,
            Self::Aborted { stage, reason } => Some(format!(
                "the Picot daemon restart failed at {}: {reason}",
                stage.as_str()
            )),
        }
    }
}

/// Starts (or adopts) the one replacement daemon and waits until it is ready
/// with a lease this app can hold.
#[async_trait::async_trait]
pub trait ReplacementSpawner: Send + Sync {
    async fn spawn_and_ready(&self) -> Result<RestartTarget, String>;
}

/// The app-side half: attach the new admin channel, mint one fresh owner per
/// captured view, and migrate the live windows.
#[async_trait::async_trait]
pub trait RestartLink: Send + Sync {
    /// Attach to the replacement: connect the admin client, install it as the
    /// app's live session, and return the new attachment epoch.
    async fn attach(&self, target: &RestartTarget) -> Result<u64, String>;

    /// Mint one fresh capability/owner for a captured view (the owner registry
    /// is fresh, so the label is free again).
    async fn rebuild_owner(
        &self,
        view: &CapturedView,
        target: &RestartTarget,
    ) -> Result<OwnerBinding, String>;

    /// Migrate the windows: recreate each one at its new URL with the new
    /// capability injected.
    async fn migrate_views(&self, rows: &[ViewMigration]) -> Result<(), String>;
}

/// The production spawner: the app's own [`DaemonManager`], so the replacement
/// goes through the same probe → spawn → wait-ready → attach transaction every
/// other boot uses (and the single-instance lock backstops a double spawn).
pub struct ManagerSpawner {
    manager: Arc<DaemonManager>,
}

impl ManagerSpawner {
    pub fn new(manager: Arc<DaemonManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl ReplacementSpawner for ManagerSpawner {
    async fn spawn_and_ready(&self) -> Result<RestartTarget, String> {
        let file = match self.manager.ensure().await {
            Ok(EnsureOutcome::Started(file)) | Ok(EnsureOutcome::Attached(file)) => file,
            Ok(EnsureOutcome::BlockedByOtherApp) => {
                return Err("another Picot app manages the replacement daemon".to_string())
            }
            Err(error) => return Err(error.to_string()),
        };
        Ok(RestartTarget {
            pid: file.pid,
            process_start_time: file.process_start_time.clone(),
            host_origin: file.host_origin,
            admin_origin: file.admin_origin,
            admin_token: file.admin_token,
            version: file.version,
            build_id: file.build_id,
        })
    }
}

/// Everything the transaction needs that is not a seam.
pub struct RestartConfig {
    /// The identity of the daemon being replaced (captured at attach).
    /// `None` when no verified identity was captured: there is then nothing to
    /// confirm or signal, and the stop request's return is the whole
    /// transition (plan D12). The app refuses to restart in that case.
    pub identity: Option<ProcessIdentity>,
    /// The child handle when this app spawned it (reaping beats polling).
    pub child: Option<Child>,
    /// The daemon's instance file: never deleted here (plan D12/D10) — it is the
    /// orphan evidence an unconfirmed exit must leave for the next daemon.
    pub instance_path: PathBuf,
    /// The daemon's log file, named by an unconfirmed-exit message.
    pub log_path: PathBuf,
    /// `app_config_dir`: where the quit policy is read (never written) here.
    pub config_dir: PathBuf,
    pub confirm_deadline: Duration,
}

/// The cross-request state of the restart transaction: serialization, request
/// ordering and the settled outcome.
///
/// A restart rebuilds the app's whole attachment, so each request constructs a
/// fresh coordinator from the *current* daemon facts — but coalescing must span
/// requests, which is why this state is shared and managed app-side.
#[derive(Default)]
pub struct RestartGate {
    /// `my_seq <= covered` means "a transaction that already covered my request
    /// has settled": that is how coalescing is decided.
    request_seq: AtomicU64,
    settled: Mutex<Option<(u64, RestartOutcome)>>,
    serial: tokio::sync::Mutex<()>,
}

/// The transaction (plan Task 9 / D10).
pub struct RestartCoordinator {
    stop: Arc<dyn StopRequester>,
    waiter: ExitWaiter,
    spawner: Arc<dyn ReplacementSpawner>,
    link: Arc<dyn RestartLink>,
    config: RestartConfig,
    gate: Arc<RestartGate>,
    /// The captured instance's child handle, taken once by the confirmed-exit
    /// waiter.
    child: Mutex<Option<Child>>,
}

impl RestartCoordinator {
    pub fn new(
        gate: Arc<RestartGate>,
        mut config: RestartConfig,
        stop: Arc<dyn StopRequester>,
        waiter: ExitWaiter,
        spawner: Arc<dyn ReplacementSpawner>,
        link: Arc<dyn RestartLink>,
    ) -> Self {
        let child = Mutex::new(config.child.take());
        Self {
            stop,
            waiter,
            spawner,
            link,
            config,
            gate,
            child,
        }
    }

    /// Run the transaction for `views` (the captured pre-state).
    ///
    /// Coalescing: a request arriving while a transaction runs — or after one
    /// settled that already covered it — returns that transaction's outcome
    /// instead of starting a second replacement. A later, new request runs a
    /// real transaction.
    pub async fn run(&self, views: Vec<CapturedView>) -> RestartOutcome {
        let my_seq = self.gate.request_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let _serial = self.gate.serial.lock().await;
        if let Some((covered, outcome)) = self
            .gate
            .settled
            .lock()
            .expect("restart settle lock")
            .clone()
        {
            if covered >= my_seq {
                log::info!(
                    "[picot] a restart transaction already settled for this request; \
                     coalescing instead of replacing the daemon twice"
                );
                return outcome;
            }
        }
        let outcome = self.perform(views).await;
        let covered = self.gate.request_seq.load(Ordering::SeqCst);
        *self.gate.settled.lock().expect("restart settle lock") = Some((covered, outcome.clone()));
        outcome
    }

    async fn perform(&self, views: Vec<CapturedView>) -> RestartOutcome {
        // Read at transaction start, never written: a restart does not change
        // the quit policy (spec §3.2.2).
        let keep_running_after_quit =
            super::quit_policy::keep_running_after_quit(&self.config.config_dir);
        log::info!(
            "[picot] restarting the Picot daemon: {} view(s) to migrate \
             (keepRunningAfterQuit={keep_running_after_quit})",
            views.len()
        );

        // Step 2 — stop, then *confirm the death* by identity.
        if let Err(error) = self.stop.request_stop().await {
            // Not fatal and deliberately not an early return: a dropped channel
            // proves nothing about the process (D12).
            log::warn!(
                "[picot] daemon.stop was not acknowledged ({error}); confirming the exit by identity"
            );
        }
        let old_exit = match self.config.identity.clone() {
            Some(identity) => {
                let mut child = self.child.lock().expect("restart child lock").take();
                let outcome = self
                    .waiter
                    .confirm_exit(&identity, child.as_mut(), self.config.confirm_deadline)
                    .await;
                if outcome == ExitOutcome::Unconfirmed {
                    // Never spawn a second daemon against an unverified live one.
                    let reason = format!(
                        "the captured Picot daemon (pid {}) is still alive after daemon.stop and \
                         the SIGTERM/SIGKILL ladder; keeping the orphan evidence {} so the next \
                         daemon sweeps it (daemon log: {})",
                        identity.pid,
                        self.config.instance_path.display(),
                        self.config.log_path.display(),
                    );
                    log::error!("[picot] {reason}");
                    return RestartOutcome::Aborted {
                        stage: RestartStage::StopAndConfirm,
                        reason,
                    };
                }
                if let ExitOutcome::Escalated { signal } = outcome {
                    log::warn!(
                        "[picot] the Picot daemon (pid {}) needed signal {signal} to exit",
                        identity.pid
                    );
                }
                Some(outcome)
            }
            // No verified process identity: the stop request's return is the
            // only evidence available, and the confirmed-exit discipline
            // (plan D12) has nothing to wait on or signal.
            None => None,
        };

        // Step 3 — exactly one replacement, ready and leased.
        let target = match self.spawner.spawn_and_ready().await {
            Ok(target) => target,
            Err(reason) => {
                return RestartOutcome::Aborted {
                    stage: RestartStage::Spawn,
                    reason,
                }
            }
        };

        // Step 4 (app side) — attach the new admin channel and install it as
        // the app's live session before anything is minted against it.
        let epoch = match self.link.attach(&target).await {
            Ok(epoch) => epoch,
            Err(reason) => {
                return RestartOutcome::Aborted {
                    stage: RestartStage::Attach,
                    reason,
                }
            }
        };

        // Step 4 — one fresh capability per captured window.
        let mut bindings = Vec::with_capacity(views.len());
        for view in &views {
            match self.link.rebuild_owner(view, &target).await {
                Ok(binding) => bindings.push(binding),
                Err(reason) => {
                    return RestartOutcome::Aborted {
                        stage: RestartStage::RebuildBindings,
                        reason: format!("{}: {reason}", view.label),
                    }
                }
            }
        }

        // Steps 5 + 6 — migrate every view to the new origin, each at its own
        // captured route.
        let rows = migration_rows(&views, &bindings, &target.host_origin);
        if let Err(reason) = self.link.migrate_views(&rows).await {
            return RestartOutcome::Aborted {
                stage: RestartStage::MigrateViews,
                reason,
            };
        }

        // Step 7 — the new identity, for the UI's status refresh.
        log::info!(
            "[picot] Picot daemon replaced: pid {} → {} origin {} (epoch {epoch}, {} view(s) \
             migrated)",
            self.config
                .identity
                .as_ref()
                .map(|identity| identity.pid.to_string())
                .unwrap_or_else(|| "unverified".to_string()),
            target.pid,
            target.host_origin,
            rows.len()
        );
        RestartOutcome::Restarted(RestartReport {
            pid: target.pid,
            host_origin: target.host_origin,
            version: target.version,
            build_id: target.build_id,
            epoch,
            old_exit,
            old_pid: self.config.identity.as_ref().map(|identity| identity.pid),
            views: rows,
            keep_running_after_quit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_client::admin_client::AdminClient;
    use crate::daemon_client::exit_waiter::{ProcessHandle, SIGNAL_KILL, SIGNAL_TERM};
    use crate::daemon_client::navigation_mirror::NavigationMirror;
    use std::sync::atomic::AtomicUsize;

    /// One shared, ordered journal: every seam appends to it, so the *order* of
    /// the transaction's steps is asserted as one sequence.
    #[derive(Default)]
    struct Journal(Mutex<Vec<String>>);

    impl Journal {
        fn push(&self, entry: impl Into<String>) {
            self.0.lock().unwrap().push(entry.into());
        }

        fn entries(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    /// A stop requester that records the request and (optionally) signals the
    /// real process it stands for — the "accepted, then the daemon drains" half.
    struct RecordingStop {
        journal: Arc<Journal>,
        signal_pid: Option<u32>,
        answer: Result<(), String>,
    }

    #[async_trait::async_trait]
    impl StopRequester for RecordingStop {
        async fn request_stop(&self) -> Result<(), String> {
            self.journal.push("stop");
            if let Some(pid) = self.signal_pid {
                unsafe { libc::kill(pid as i32, SIGNAL_TERM) };
            }
            self.answer.clone()
        }
    }

    /// A scripted old instance. `alive` answers the waiter's liveness probe and
    /// records it; `start_time` lets a test script pid reuse.
    struct FakeHandle {
        journal: Arc<Journal>,
        alive: Mutex<bool>,
        start_time: Mutex<Option<String>>,
        signals: Mutex<Vec<i32>>,
        dies_on: Option<i32>,
    }

    impl FakeHandle {
        fn gone(journal: Arc<Journal>) -> Arc<Self> {
            Arc::new(Self {
                journal,
                alive: Mutex::new(false),
                start_time: Mutex::new(None),
                signals: Mutex::new(Vec::new()),
                dies_on: None,
            })
        }

        fn immortal(journal: Arc<Journal>) -> Arc<Self> {
            Arc::new(Self {
                journal,
                alive: Mutex::new(true),
                start_time: Mutex::new(Some(RECORDED_START.to_string())),
                signals: Mutex::new(Vec::new()),
                dies_on: None,
            })
        }

        /// A live pid that now belongs to somebody else: same pid, other start
        /// time (pid reuse).
        fn reused_pid(journal: Arc<Journal>) -> Arc<Self> {
            Arc::new(Self {
                journal,
                alive: Mutex::new(true),
                start_time: Mutex::new(Some("Tue Oct 14 07:00:00 2026".to_string())),
                signals: Mutex::new(Vec::new()),
                dies_on: None,
            })
        }

        fn signals(&self) -> Vec<i32> {
            self.signals.lock().unwrap().clone()
        }
    }

    impl ProcessHandle for FakeHandle {
        fn alive(&self, pid: u32) -> bool {
            self.journal.push(format!("probe:{pid}"));
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

    /// A spawner that records the spawn and can be held open so two concurrent
    /// triggers are genuinely concurrent.
    struct FakeSpawner {
        journal: Arc<Journal>,
        calls: AtomicUsize,
        gate: Option<Arc<tokio::sync::Notify>>,
        /// Asserted inside `spawn_and_ready`: the old instance must already be
        /// gone when this runs.
        must_be_dead: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    }

    impl FakeSpawner {
        fn new(journal: Arc<Journal>) -> Arc<Self> {
            Arc::new(Self {
                journal,
                calls: AtomicUsize::new(0),
                gate: None,
                must_be_dead: None,
            })
        }

        fn gated(journal: Arc<Journal>, gate: Arc<tokio::sync::Notify>) -> Arc<Self> {
            Arc::new(Self {
                journal,
                calls: AtomicUsize::new(0),
                gate: Some(gate),
                must_be_dead: None,
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ReplacementSpawner for FakeSpawner {
        async fn spawn_and_ready(&self) -> Result<RestartTarget, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(dead) = &self.must_be_dead {
                assert!(dead(), "the spawn must happen only after a confirmed death");
            }
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            self.journal.push("spawn");
            Ok(target())
        }
    }

    /// A link that records every app-side step.
    struct FakeLink {
        journal: Arc<Journal>,
        captures: Mutex<Vec<CapturedView>>,
        /// Labels whose owner rebuild must degrade to a landing view.
        degrade: Mutex<Vec<String>>,
        fail_attach: bool,
    }

    impl FakeLink {
        fn new(journal: Arc<Journal>) -> Arc<Self> {
            Arc::new(Self {
                journal,
                captures: Mutex::new(Vec::new()),
                degrade: Mutex::new(Vec::new()),
                fail_attach: false,
            })
        }

        fn captures(&self) -> Vec<CapturedView> {
            self.captures.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl RestartLink for FakeLink {
        async fn attach(&self, target: &RestartTarget) -> Result<u64, String> {
            if self.fail_attach {
                return Err("the replacement refused the lease".to_string());
            }
            self.journal.push(format!("attach:{}", target.pid));
            Ok(7)
        }

        async fn rebuild_owner(
            &self,
            view: &CapturedView,
            _target: &RestartTarget,
        ) -> Result<OwnerBinding, String> {
            self.captures.lock().unwrap().push(view.clone());
            let degraded = self
                .degrade
                .lock()
                .unwrap()
                .iter()
                .any(|label| label == &view.label);
            self.journal.push(format!("owner:{}", view.label));
            Ok(OwnerBinding {
                owner: format!("owner-{}", view.label),
                capability: format!("cap-{}", view.label),
                degraded,
            })
        }

        async fn migrate_views(&self, rows: &[ViewMigration]) -> Result<(), String> {
            self.journal.push(format!("migrate:{}", rows.len()));
            Ok(())
        }
    }

    const RECORDED_START: &str = "Mon Oct 13 09:15:02 2026";
    const OLD_PID: u32 = 4242;

    fn target() -> RestartTarget {
        RestartTarget {
            pid: 9001,
            process_start_time: Some("Mon Oct 13 10:00:00 2026".to_string()),
            host_origin: "http://127.0.0.1:6001".to_string(),
            admin_origin: "http://127.0.0.1:6002".to_string(),
            admin_token: "token".to_string(),
            version: "0.5.0".to_string(),
            build_id: "build-2".to_string(),
        }
    }

    fn workspace_view() -> CapturedView {
        CapturedView {
            label: "native-workspace-w1".to_string(),
            owner: "owner-old-w1".to_string(),
            owner_home: None,
            workspace_id: Some("w1".to_string()),
            route: Some("/workspaces/w1/sessions/s1".to_string()),
        }
    }

    fn landing_view() -> CapturedView {
        CapturedView {
            label: "native-landing".to_string(),
            owner: "owner-old-landing".to_string(),
            owner_home: Some("/Users/x".to_string()),
            workspace_id: None,
            route: None,
        }
    }

    fn views() -> Vec<CapturedView> {
        vec![workspace_view(), landing_view()]
    }

    /// A stop requester that answers "accepted" and records the request in the
    /// shared journal.
    fn quiet_stop(journal: &Arc<Journal>) -> Arc<dyn StopRequester> {
        Arc::new(RecordingStop {
            journal: Arc::clone(journal),
            signal_pid: None,
            answer: Ok(()),
        })
    }

    fn coordinator(
        journal: Arc<Journal>,
        handle: Arc<FakeHandle>,
        spawner: Arc<FakeSpawner>,
        link: Arc<FakeLink>,
    ) -> Arc<RestartCoordinator> {
        let stop = quiet_stop(&journal);
        coordinator_with_gate(
            Arc::new(RestartGate::default()),
            handle,
            spawner,
            link,
            stop,
        )
    }

    fn coordinator_with_gate(
        gate: Arc<RestartGate>,
        handle: Arc<FakeHandle>,
        spawner: Arc<FakeSpawner>,
        link: Arc<FakeLink>,
        stop: Arc<dyn StopRequester>,
    ) -> Arc<RestartCoordinator> {
        Arc::new(RestartCoordinator::new(
            gate,
            RestartConfig {
                identity: Some(ProcessIdentity::new(
                    OLD_PID,
                    Some(RECORDED_START.to_string()),
                )),
                child: None,
                instance_path: PathBuf::from("/tmp/picot-daemon.json"),
                log_path: PathBuf::from("/tmp/picot-daemon.log"),
                config_dir: PathBuf::from("/tmp/never-read-config"),
                confirm_deadline: Duration::from_millis(40),
            },
            stop,
            ExitWaiter::with_handle(handle)
                .with_timing(Duration::from_millis(5), Duration::from_millis(40)),
            spawner,
            link,
        ))
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    // ── Ordering: stop → confirmed death → spawn → attach → owners → migrate ──

    #[test]
    fn the_transaction_runs_stop_confirm_spawn_attach_rebuild_migrate_in_order() {
        let journal = Arc::new(Journal::default());
        let handle = FakeHandle::gone(Arc::clone(&journal));
        let spawner = FakeSpawner::new(Arc::clone(&journal));
        let link = FakeLink::new(Arc::clone(&journal));
        let coordinator = coordinator(
            Arc::clone(&journal),
            handle,
            Arc::clone(&spawner),
            Arc::clone(&link),
        );

        let outcome = runtime().block_on(coordinator.run(views()));

        let report = outcome.report().expect("the transaction restarted");
        assert_eq!(report.pid, 9001);
        assert_eq!(report.host_origin, "http://127.0.0.1:6001");
        assert_eq!(report.epoch, 7);
        assert_eq!(report.old_pid, Some(OLD_PID));
        assert_eq!(report.old_exit, Some(ExitOutcome::Confirmed));
        assert!(!report.keep_running_after_quit, "read, never changed");
        assert_eq!(
            journal.entries(),
            vec![
                "stop".to_string(),
                // The confirmed-exit probe happens *before* the spawn: this is
                // the ordering the whole task exists to enforce.
                format!("probe:{OLD_PID}"),
                "spawn".to_string(),
                "attach:9001".to_string(),
                "owner:native-workspace-w1".to_string(),
                "owner:native-landing".to_string(),
                "migrate:2".to_string(),
            ],
            "the seven steps run in their documented order"
        );
        assert_eq!(spawner.calls(), 1, "exactly one replacement");
        assert_eq!(link.captures(), views(), "every captured view was rebuilt");
    }

    // ── The abort that must never spawn ─────────────────────────────────────

    #[test]
    fn an_unconfirmed_old_instance_aborts_without_spawning() {
        let journal = Arc::new(Journal::default());
        let handle = FakeHandle::immortal(Arc::clone(&journal));
        let spawner = FakeSpawner::new(Arc::clone(&journal));
        let link = FakeLink::new(Arc::clone(&journal));
        let coordinator = coordinator(
            Arc::clone(&journal),
            Arc::clone(&handle),
            Arc::clone(&spawner),
            Arc::clone(&link),
        );

        let outcome = runtime().block_on(coordinator.run(views()));

        let message = outcome.failure_message().expect("the transaction aborted");
        match &outcome {
            RestartOutcome::Aborted { stage, reason } => {
                assert_eq!(*stage, RestartStage::StopAndConfirm);
                assert!(
                    reason.contains("/tmp/picot-daemon.json"),
                    "the orphan evidence is named: {reason}"
                );
                assert!(
                    reason.contains("/tmp/picot-daemon.log"),
                    "the daemon log is named: {reason}"
                );
                assert!(
                    reason.contains(&OLD_PID.to_string()),
                    "the verified pid is named: {reason}"
                );
            }
            other => panic!("expected an abort, got {other:?}"),
        }
        assert!(message.contains("stop-and-confirm"), "{message}");
        assert_eq!(
            handle.signals(),
            vec![SIGNAL_TERM, SIGNAL_KILL],
            "the whole ladder is attempted before giving up"
        );
        assert_eq!(
            spawner.calls(),
            0,
            "never spawn against an unverified live daemon"
        );
        assert!(
            !journal.entries().iter().any(|entry| entry == "spawn"),
            "and no spawn reached the journal: {:?}",
            journal.entries()
        );
    }

    // ── Identity, not pid: pid reuse is a confirmed death ───────────────────

    #[test]
    fn a_reused_pid_counts_as_a_confirmed_death_and_is_never_signalled() {
        let journal = Arc::new(Journal::default());
        let handle = FakeHandle::reused_pid(Arc::clone(&journal));
        let spawner = FakeSpawner::new(Arc::clone(&journal));
        let link = FakeLink::new(Arc::clone(&journal));
        let coordinator = coordinator(
            Arc::clone(&journal),
            Arc::clone(&handle),
            Arc::clone(&spawner),
            Arc::clone(&link),
        );

        let outcome = runtime().block_on(coordinator.run(views()));

        let report = outcome.report().expect("a reused pid is a dead instance");
        assert_eq!(report.old_exit, Some(ExitOutcome::Confirmed));
        assert!(
            handle.signals().is_empty(),
            "the pid now belongs to somebody else: signalling it would be a bug"
        );
        assert_eq!(spawner.calls(), 1);
    }

    // ── Exactly one replacement under concurrent triggers ───────────────────

    #[test]
    fn two_concurrent_triggers_coalesce_into_one_replacement() {
        let journal = Arc::new(Journal::default());
        let handle = FakeHandle::gone(Arc::clone(&journal));
        let gate = Arc::new(tokio::sync::Notify::new());
        let spawner = FakeSpawner::gated(Arc::clone(&journal), Arc::clone(&gate));
        let link = FakeLink::new(Arc::clone(&journal));
        let coordinator = coordinator(
            Arc::clone(&journal),
            handle,
            Arc::clone(&spawner),
            Arc::clone(&link),
        );

        let runtime = runtime();
        let (first, second) = runtime.block_on(async {
            let first = {
                let coordinator = Arc::clone(&coordinator);
                let views = views();
                tokio::spawn(async move { coordinator.run(views).await })
            };
            // Give the first transaction time to reach the gate (it is inside
            // `spawn_and_ready`, past stop + confirmed exit).
            while !journal
                .entries()
                .iter()
                .any(|entry| entry.starts_with("probe:"))
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let second = {
                let coordinator = Arc::clone(&coordinator);
                let views = views();
                tokio::spawn(async move { coordinator.run(views).await })
            };
            tokio::time::sleep(Duration::from_millis(20)).await;
            gate.notify_waiters();
            (first.await.unwrap(), second.await.unwrap())
        });

        assert_eq!(first, second, "the second trigger reads the first outcome");
        assert_eq!(spawner.calls(), 1, "exactly one replacement was spawned");
        assert_eq!(
            journal
                .entries()
                .iter()
                .filter(|entry| entry.as_str() == "spawn")
                .count(),
            1
        );
        assert_eq!(
            journal
                .entries()
                .iter()
                .filter(|entry| entry.as_str() == "stop")
                .count(),
            1,
            "the daemon is stopped once, whatever the trigger count"
        );
    }

    #[test]
    fn a_later_trigger_is_a_real_second_restart() {
        let journal = Arc::new(Journal::default());
        let spawner = FakeSpawner::new(Arc::clone(&journal));
        let link = FakeLink::new(Arc::clone(&journal));
        let runtime = runtime();
        // One gate, two coordinators: exactly the app-side shape (each request
        // builds a coordinator from the current facts but shares the gate).
        let gate = Arc::new(RestartGate::default());
        let build = || {
            coordinator_with_gate(
                Arc::clone(&gate),
                FakeHandle::gone(Arc::clone(&journal)),
                Arc::clone(&spawner),
                Arc::clone(&link),
                quiet_stop(&journal),
            )
        };

        assert!(runtime.block_on(build().run(views())).is_restarted());
        // A second request after the first settled is a genuine new restart.
        assert!(runtime.block_on(build().run(views())).is_restarted());
        assert_eq!(
            spawner.calls(),
            2,
            "a settled transaction must not swallow a later request"
        );
    }

    // ── Migration: one capability per view, one route per view ──────────────

    #[test]
    fn migration_rebuilds_one_capability_per_captured_view_at_its_own_route() {
        let journal = Arc::new(Journal::default());
        let handle = FakeHandle::gone(Arc::clone(&journal));
        let spawner = FakeSpawner::new(Arc::clone(&journal));
        let link = FakeLink::new(Arc::clone(&journal));
        let coordinator = coordinator(Arc::clone(&journal), handle, spawner, Arc::clone(&link));

        let outcome = runtime().block_on(coordinator.run(views()));
        let report = outcome.report().expect("restarted");

        assert_eq!(report.views.len(), 2, "every captured window is migrated");
        assert_eq!(
            report.views[0],
            ViewMigration {
                label: "native-workspace-w1".to_string(),
                owner: "owner-native-workspace-w1".to_string(),
                capability: "cap-native-workspace-w1".to_string(),
                url: "http://127.0.0.1:6001/workspaces/w1/sessions/s1".to_string(),
                workspace_id: Some("w1".to_string()),
                degraded: false,
            },
            "the workspace view keeps its session route on the new origin"
        );
        assert_eq!(
            report.views[1],
            ViewMigration {
                label: "native-landing".to_string(),
                owner: "owner-native-landing".to_string(),
                capability: "cap-native-landing".to_string(),
                url: "http://127.0.0.1:6001/".to_string(),
                workspace_id: None,
                degraded: false,
            },
            "the landing view lands on the new origin root"
        );
        let owners: std::collections::HashSet<_> =
            report.views.iter().map(|view| &view.owner).collect();
        assert_eq!(owners.len(), report.views.len(), "no owner is shared");
        assert_eq!(
            link.captures(),
            views(),
            "every captured window got exactly one rebuild"
        );
    }

    #[test]
    fn a_view_whose_owner_cannot_be_rebuilt_degrades_to_the_landing_route() {
        let journal = Arc::new(Journal::default());
        let handle = FakeHandle::gone(Arc::clone(&journal));
        let spawner = FakeSpawner::new(Arc::clone(&journal));
        let link = FakeLink::new(Arc::clone(&journal));
        link.degrade
            .lock()
            .unwrap()
            .push("native-workspace-w1".to_string());
        let coordinator = coordinator(Arc::clone(&journal), handle, spawner, link);

        let outcome = runtime().block_on(coordinator.run(views()));
        let report = outcome.report().expect("the transaction still restarted");
        assert_eq!(
            report.views[0].url, "http://127.0.0.1:6001/",
            "a degraded view lands on the new origin root, not on a route it cannot serve"
        );
        assert!(report.views[0].degraded);
        assert_eq!(
            report.views[0].workspace_id, None,
            "a degraded view is rebound as a landing window"
        );
        assert_eq!(
            report.views[1].url, "http://127.0.0.1:6001/",
            "the landing view is unaffected"
        );
        assert!(!report.views[1].degraded);
    }

    // ── Aborts after the spawn still name their stage ───────────────────────

    #[test]
    fn an_attach_failure_aborts_at_attach_without_touching_the_windows() {
        let journal = Arc::new(Journal::default());
        let handle = FakeHandle::gone(Arc::clone(&journal));
        let spawner = FakeSpawner::new(Arc::clone(&journal));
        let link = Arc::new(FakeLink {
            journal: Arc::clone(&journal),
            captures: Mutex::new(Vec::new()),
            degrade: Mutex::new(Vec::new()),
            fail_attach: true,
        });
        let coordinator = coordinator(Arc::clone(&journal), handle, spawner, link);

        let outcome = runtime().block_on(coordinator.run(views()));
        match outcome {
            RestartOutcome::Aborted { stage, reason } => {
                assert_eq!(stage, RestartStage::Attach);
                assert!(reason.contains("lease"), "{reason}");
            }
            other => panic!("expected an attach abort, got {other:?}"),
        }
        assert!(
            !journal
                .entries()
                .iter()
                .any(|entry| entry.starts_with("owner:")),
            "no owner is minted against a daemon we could not attach to: {:?}",
            journal.entries()
        );
        assert!(
            !journal
                .entries()
                .iter()
                .any(|entry| entry.starts_with("migrate:")),
            "and no window is touched"
        );
    }

    // ── An identity-less daemon: no process to confirm ─────────────────────

    /// Without a captured identity the stop request's return is the whole
    /// transition: there is nothing to confirm or signal. The app never builds
    /// this shape (run_restart refuses it), but the transaction must not pretend
    /// to confirm a process it never met.
    #[test]
    fn an_identity_less_daemon_is_drained_before_the_replacement_boots() {
        let journal = Arc::new(Journal::default());
        let handle = FakeHandle::gone(Arc::clone(&journal));
        let spawner = FakeSpawner::new(Arc::clone(&journal));
        let link = FakeLink::new(Arc::clone(&journal));
        let stop = quiet_stop(&journal);
        let coordinator = Arc::new(RestartCoordinator::new(
            Arc::new(RestartGate::default()),
            RestartConfig {
                identity: None,
                child: None,
                instance_path: PathBuf::from("/tmp/identity-less.json"),
                log_path: PathBuf::from("/tmp/identity-less.log"),
                config_dir: PathBuf::from("/tmp/never-read-config"),
                confirm_deadline: Duration::from_millis(40),
            },
            stop,
            ExitWaiter::with_handle(handle)
                .with_timing(Duration::from_millis(5), Duration::from_millis(40)),
            spawner,
            link,
        ));

        let outcome = runtime().block_on(coordinator.run(views()));

        let report = outcome.report().expect("restart without an identity");
        assert_eq!(report.old_exit, None);
        assert_eq!(report.old_pid, None);
        assert_eq!(
            journal.entries(),
            vec![
                "stop".to_string(),
                "spawn".to_string(),
                "attach:9001".to_string(),
                "owner:native-workspace-w1".to_string(),
                "owner:native-landing".to_string(),
                "migrate:2".to_string(),
            ],
            "the stop request precedes the replacement boot, and nothing probes a process"
        );
    }

    // ── The same discipline against a real process ──────────────────────────

    /// The process is no longer **running**: the pid is unused, or it is a
    /// zombie that has exited and merely awaits reaping — the exact distinction
    /// [`crate::daemon_client::exit_waiter::SystemHandle`] draws, so the
    /// assertion measures the same fact the waiter does.
    #[cfg(unix)]
    fn not_running(pid: u32) -> bool {
        if !picot_host::child_supervision::pid_is_alive(pid) {
            return true;
        }
        std::process::Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .trim_start()
                    .starts_with('Z')
            })
            .unwrap_or(false)
    }

    /// The confirmed-exit half against a **real** process: a live `sleep` is
    /// replaced only after the OS reports it gone. The spawner asserts it, so a
    /// spawn before the confirmation fails the test rather than passing quietly.
    #[cfg(unix)]
    #[test]
    fn a_real_old_instance_is_confirmed_dead_before_the_spawn() {
        let journal = Arc::new(Journal::default());
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn the stand-in daemon");
        let pid = child.id();
        let identity = ProcessIdentity::of(pid);

        let spawner = Arc::new(FakeSpawner {
            journal: Arc::clone(&journal),
            calls: AtomicUsize::new(0),
            gate: None,
            must_be_dead: Some(Arc::new(move || not_running(pid))),
        });
        let link = FakeLink::new(Arc::clone(&journal));
        // The stop request stands in for the daemon's own SIGTERM handler: it
        // answers "accepted", and the process goes away on its own.
        let stop = Arc::new(RecordingStop {
            journal: Arc::clone(&journal),
            signal_pid: Some(pid),
            answer: Ok(()),
        });
        let coordinator = Arc::new(RestartCoordinator::new(
            Arc::new(RestartGate::default()),
            RestartConfig {
                identity: Some(identity),
                child: Some(child),
                instance_path: PathBuf::from("/tmp/picot-daemon.json"),
                log_path: PathBuf::from("/tmp/picot-daemon.log"),
                config_dir: PathBuf::from("/tmp/never-read-config"),
                confirm_deadline: Duration::from_secs(5),
            },
            stop,
            ExitWaiter::new(),
            spawner,
            link,
        ));

        let outcome = runtime().block_on(coordinator.run(views()));

        assert!(
            outcome.is_restarted(),
            "the real process exited on SIGTERM: {outcome:?}"
        );
        assert!(not_running(pid), "the stand-in instance is gone");
        assert_eq!(
            journal.entries(),
            vec![
                "stop".to_string(),
                "spawn".to_string(),
                "attach:9001".to_string(),
                "owner:native-workspace-w1".to_string(),
                "owner:native-landing".to_string(),
                "migrate:2".to_string(),
            ],
            "the real process was gone before the replacement was spawned"
        );
    }

    // ── The whole transaction against a real daemon process ─────────────────

    /// The dev daemon binary (`target/debug/picot-daemon`), **current** for this
    /// checkout.
    ///
    /// `cargo test --workspace` builds it; a narrower `-p picot` run may leave a
    /// stale artifact behind, so this ensures freshness instead of mere
    /// existence. The identity the manager compares is taken from the daemon the
    /// test starts (see the test), because the *app crate's* own
    /// `picot_host::BUILD_ID` is baked in at its compile time and can legitimately
    /// differ by one commit from a binary built later in the same session.
    fn daemon_binary() -> PathBuf {
        let binary = crate::daemon_client::manager::resolve_daemon_binary();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let status = std::process::Command::new("cargo")
            .arg("build")
            .arg("--manifest-path")
            .arg(&manifest)
            .args(["-p", "picot-daemon"])
            .status()
            .expect("cargo must be runnable to build the daemon under test");
        assert!(
            status.success() && binary.exists(),
            "cannot build {}: {status}",
            binary.display()
        );
        binary
    }

    /// An app surface the restart test never needs: a real daemon restart over
    /// the admin channel mints owners but forwards no surface op.
    struct NeverUsedSurface;

    #[async_trait::async_trait]
    impl crate::daemon_client::admin_client::SurfaceExecutor for NeverUsedSurface {
        async fn execute(
            &self,
            op: &str,
            _args: serde_json::Value,
            _progress: picot_host::host_control::ProgressSink,
        ) -> Result<serde_json::Value, String> {
            Err(format!(
                "the restart test must not need an app surface ({op})"
            ))
        }
    }

    /// A link that really connects: the app-side seams against the real daemon
    /// (no windows, so migration only records its rows — the WebView half is the
    /// manual matrix row).
    struct RealLink {
        mirror: Arc<NavigationMirror>,
        executor: Arc<dyn crate::daemon_client::admin_client::SurfaceExecutor>,
        session: Mutex<Option<Arc<AdminClient>>>,
        migrated: Mutex<Vec<ViewMigration>>,
    }

    impl RealLink {
        fn client(&self) -> Result<Arc<AdminClient>, String> {
            self.session
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| "the link is not attached".to_string())
        }
    }

    #[async_trait::async_trait]
    impl RestartLink for RealLink {
        async fn attach(&self, target: &RestartTarget) -> Result<u64, String> {
            if let Some(old) = self.session.lock().unwrap().take() {
                old.disconnect();
            }
            let client = AdminClient::connect(
                &target.admin_origin,
                &target.admin_token,
                &picot_host::daemon::admin::ManagerIdentity {
                    pid: Some(std::process::id()),
                    app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    build_id: Some(picot_host::BUILD_ID.to_string()),
                },
                Arc::clone(&self.executor),
                Arc::clone(&self.mirror),
            )
            .await?;
            let epoch = client.epoch().unwrap_or_default();
            *self.session.lock().unwrap() = Some(client);
            Ok(epoch)
        }

        async fn rebuild_owner(
            &self,
            view: &CapturedView,
            target: &RestartTarget,
        ) -> Result<OwnerBinding, String> {
            let created = self
                .client()?
                .create_owner(
                    &view.label,
                    view.owner_home.as_deref(),
                    0,
                    &target.host_origin,
                    view.workspace_id.as_deref(),
                )
                .await?;
            Ok(OwnerBinding {
                owner: created.owner,
                capability: created.capability,
                degraded: false,
            })
        }

        async fn migrate_views(&self, rows: &[ViewMigration]) -> Result<(), String> {
            *self.migrated.lock().unwrap() = rows.to_vec();
            Ok(())
        }
    }

    /// The process-level end of Task 9 Step 2: a **real** `picot-daemon` is
    /// started by the real manager, replaced by the real transaction, and the
    /// new daemon is asserted to be a different process on a different origin
    /// with the captured view rebound to it.
    ///
    /// What a headless test cannot reach is the WebView half (windows landing on
    /// their routes); that is the manual matrix row. Everything the process and
    /// protocol boundary can prove is proven here against the shipping binary.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_real_daemon_process_is_replaced_end_to_end() {
        use crate::daemon_client::admin_client::SurfaceExecutor;
        use crate::daemon_client::manager::{Launcher, ManagerConfig};
        use picot_host::daemon::instance;

        let temp = tempfile::tempdir().expect("temp dir");
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).expect("static dir");
        std::fs::write(static_dir.join("index.html"), "<h1>restart-suite</h1>").expect("marker");

        // The old daemon, started exactly as the launcher starts it (detached,
        // its own process group), then adopted by a manager configured to the
        // identity **it** published.
        let mut boot_cfg = ManagerConfig::for_app(
            data_dir.clone(),
            config_dir.clone(),
            static_dir.clone(),
            "com.palandata.picot.restart-test".to_string(),
        );
        boot_cfg.daemon_binary = daemon_binary();
        boot_cfg.ready_deadline = Duration::from_secs(45);
        let spawned = crate::daemon_client::manager::ProcessLauncher
            .spawn(&boot_cfg)
            .expect("the stand-in daemon starts");
        let instance_path = instance::instance_path(&data_dir);
        let started = std::time::Instant::now();
        while !instance_path.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(45),
                "the daemon under test never published {}",
                instance_path.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let old_file = instance::read(&instance_path).expect("the daemon's instance file");
        let mut cfg = boot_cfg;
        cfg.app_version = old_file.version.clone();
        cfg.build_id = old_file.build_id.clone();
        let manager = Arc::new(DaemonManager::for_app(cfg));
        let old_child = spawned.child;

        let mirror = Arc::new(NavigationMirror::new());
        let executor: Arc<dyn SurfaceExecutor> = Arc::new(NeverUsedSurface);
        let old_client = AdminClient::connect(
            &old_file.admin_origin,
            &old_file.admin_token,
            &manager.config().identity(),
            Arc::clone(&executor),
            Arc::clone(&mirror),
        )
        .await
        .expect("the app attaches to the daemon it started");
        let old_epoch = old_client.epoch().expect("an attach epoch");
        let home = std::env::temp_dir().to_string_lossy().to_string();
        let landing = old_client
            .create_owner(
                "native-landing",
                Some(&home),
                0,
                &old_file.host_origin,
                None,
            )
            .await
            .expect("the landing owner");
        let view = CapturedView {
            label: "native-landing".to_string(),
            owner: landing.owner.clone(),
            owner_home: Some(home),
            workspace_id: None,
            route: None,
        };

        let link = Arc::new(RealLink {
            mirror: Arc::clone(&mirror),
            executor,
            session: Mutex::new(Some(Arc::clone(&old_client))),
            migrated: Mutex::new(Vec::new()),
        });
        let stop: Arc<dyn StopRequester> = Arc::new(
            super::super::quit_policy::AdminStopRequester::new(Arc::clone(&old_client)),
        );
        let coordinator = RestartCoordinator::new(
            Arc::new(RestartGate::default()),
            RestartConfig {
                identity: Some(ProcessIdentity::new(
                    old_file.pid,
                    old_file.process_start_time.clone(),
                )),
                child: old_child,
                instance_path: instance_path.clone(),
                log_path: instance::log_path(&data_dir),
                config_dir: config_dir.clone(),
                confirm_deadline: Duration::from_secs(15),
            },
            stop,
            ExitWaiter::new(),
            Arc::new(ManagerSpawner::new(Arc::clone(&manager))),
            Arc::clone(&link) as Arc<dyn RestartLink>,
        );

        let outcome = coordinator.run(vec![view]).await;
        let report = outcome
            .report()
            .unwrap_or_else(|| panic!("the transaction restarted: {outcome:?}"));

        // One old process gone, one new process — never the same one.
        assert_eq!(report.old_pid, Some(old_file.pid));
        assert_eq!(report.old_exit, Some(ExitOutcome::Confirmed));
        assert_ne!(report.pid, old_file.pid, "a replacement, not the old pid");
        assert_ne!(
            report.host_origin, old_file.host_origin,
            "the origin changed across the restart (B2: the page must be migrated)"
        );
        assert!(not_running(old_file.pid), "the old daemon is gone (ps)");
        let published = instance::read(&instance::instance_path(&data_dir))
            .expect("the replacement published its instance file");
        assert_eq!(published.pid, report.pid);
        assert_eq!(published.host_origin, report.host_origin);

        // The captured view was rebuilt on the new daemon, the old attachment
        // failed closed, and the mirror holds the fresh owner under the new
        // origin.
        assert_eq!(report.views.len(), 1);
        assert_eq!(report.views[0].url, format!("{}/", report.host_origin));
        assert_ne!(
            report.views[0].owner, landing.owner,
            "a fresh capability, not the pre-restart owner"
        );
        assert!(report.epoch > old_epoch, "the attachment epoch advanced");
        assert!(
            !old_client.is_connected(),
            "the old attachment failed closed"
        );
        assert_eq!(link.migrated.lock().unwrap().len(), 1);
        assert_eq!(
            mirror.owner_for_label("native-landing").as_deref(),
            Some(report.views[0].owner.as_str())
        );
        assert_eq!(
            mirror
                .owner_row_for_label("native-landing")
                .expect("the rebuilt row")
                .current_origin,
            report.host_origin,
            "the mirror was re-snapshotted on the new origin"
        );

        // Leave no daemon behind: the same accepted/confirmed path the quit
        // flow uses.
        let replacement = link.client().expect("the replacement is attached");
        let target_identity = ProcessIdentity::of(report.pid);
        let _ = super::super::quit_policy::AdminStopRequester::new(Arc::clone(&replacement))
            .request_stop()
            .await;
        replacement.disconnect();
        let mut reaped = manager.take_child();
        let outcome = ExitWaiter::new()
            .confirm_exit(&target_identity, reaped.as_mut(), Duration::from_secs(10))
            .await;
        assert_ne!(outcome, ExitOutcome::Unconfirmed, "the test daemon stops");
    }
}
