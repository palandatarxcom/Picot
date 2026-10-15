// ABOUTME: Task 4 process-level suite: the real picot-daemon binary under signals, the single-instance
// ABOUTME: lock race, the kill -9 orphan evidence, the sweep on re-run, and the live admin handshake.
#![cfg(unix)]

//! Plan Task 4 Step 1's process row, plus the Step 2/Step 4 behaviour that only
//! a **real binary** can show:
//!
//! | test | plan row it proves |
//! | --- | --- |
//! | [`the_real_binary_boots_serves_health_and_a_sigterm_clears_its_files`] | Step 2 (ready → instance file), Step 4's `/health` + graceful exit |
//! | [`two_daemons_racing_the_single_instance_lock_yield_one_daemon_and_one_exit_3`] | Step 1's "simultaneous startup: the single-instance lock yields one daemon, one winner" |
//! | [`a_killed_daemon_leaves_orphan_evidence_and_the_next_daemon_takes_over`] | Step 4's `kill -9` → re-run sweep, and D12's "an unconfirmed exit keeps the orphan evidence" |
//! | [`a_second_manager_is_refused_and_the_lease_holder_can_stop_the_real_daemon`] | Step 1's "no lease → the daemon refuses the stop", over the real socket |
//! | [`the_argv_and_usage_contract_is_enforced_by_the_real_binary`] | Step 2's argv surface + the documented exit codes |
//!
//! The other Step 1 rows live where they can be exercised without a process: the
//! instance-file hardening in `picot-host`'s `daemon/instance.rs` unit tests plus
//! `tests/daemon_instance.rs`, the token scan in `tests/token_redaction.rs`, and
//! the probe decision table in `src/daemon_client/manager.rs`.
//!
//! Why these are not mocks: the lock is an OS `flock` held by a *process*, and
//! the signal path is an OS signal. A second `flock` from the same process would
//! prove nothing (`flock` is per-descriptor), and `SIGTERM` cannot be delivered
//! to a fake. Each test gets its own temp `data_dir`, so the daemons never
//! contend for the same lock unless the test wants them to.
//!
//! macOS-first is the plan's scope (D9): `InstanceLock` refuses to pretend on a
//! target without `flock`, so the suite is `unix`-gated with the rest of it.
//!
//! **GREEN STATE:** these tests were written against the implementation in
//! `crates/picot-daemon/src/main.rs` + `crates/picot-host/src/daemon/instance.rs`
//! (Task 4 Step 2/3, WIP commit `22cb041`). They pin the *contract* the app-side
//! manager and the plan's Step 4 smoke depend on: a fresh instance document on
//! every boot, an exit code for every way out, and one daemon per data dir.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use picot_host::daemon::admin::ADMIN_WS_PATH;
use picot_host::daemon::instance::{self, InstanceFile};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// Generous enough for a debug build booting the whole host stack (SQLite,
/// axum listener, Pi path self-heal) under a loaded parallel test run.
const BOOT_DEADLINE: Duration = Duration::from_secs(45);
/// A daemon that received SIGTERM has a 5s grace period by contract.
const EXIT_DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(50);

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;
/// `picot-daemon`'s documented "another daemon holds the lock" code (main.rs).
const EXIT_ALREADY_RUNNING: i32 = 3;
/// `picot-daemon`'s documented usage-error code (main.rs).
const EXIT_USAGE: i32 = 2;
const EXIT_FAILURE: i32 = 1;
const STATIC_MARKER: &str = "<h1>standalone-suite</h1>";

fn daemon_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_picot-daemon"))
}

/// One isolated data/config/static triple — one daemon's whole world.
struct Standalone {
    _temp: tempfile::TempDir,
    data_dir: PathBuf,
    config_dir: PathBuf,
    static_dir: PathBuf,
}

impl Standalone {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        let static_dir = temp.path().join("static");
        std::fs::create_dir_all(&static_dir).expect("static dir");
        std::fs::write(static_dir.join("index.html"), STATIC_MARKER).expect("static index");
        Self {
            _temp: temp,
            data_dir,
            config_dir,
            static_dir,
        }
    }

    /// The real binary's command line, exactly as the app builds it (plan D7:
    /// every path is passed in, the daemon guesses nothing).
    fn command(&self) -> Command {
        let mut command = Command::new(daemon_binary());
        command
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--config-dir")
            .arg(&self.config_dir)
            .arg("--static-dir")
            .arg(&self.static_dir)
            .arg("--bundle-identifier")
            .arg("com.palandata.picot.standalone-test");
        command
    }

    /// A daemon whose stdio is discarded: it logs to
    /// `<data_dir>/picot-daemon.log`, and inheriting its stderr would bypass the
    /// test harness's capture. (The argv test deliberately uses [`Self::command`]
    /// instead, because `Stdio::null` would also swallow `.output()`.)
    fn start(&self) -> Child {
        self.command()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the daemon binary spawns")
    }

    fn instance_path(&self) -> PathBuf {
        instance::instance_path(&self.data_dir)
    }

    fn lock_path(&self) -> PathBuf {
        instance::lock_path(&self.data_dir)
    }

    fn log_path(&self) -> PathBuf {
        instance::log_path(&self.data_dir)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    /// Wait for the published instance document — the app's readiness signal.
    async fn wait_for_instance(&self) -> InstanceFile {
        let started = Instant::now();
        loop {
            if let Some(file) = instance::read(&self.instance_path()) {
                return file;
            }
            assert!(
                started.elapsed() < BOOT_DEADLINE,
                "no instance file at {} after {BOOT_DEADLINE:?}\ndaemon log:\n{}",
                self.instance_path().display(),
                self.log()
            );
            tokio::time::sleep(POLL).await;
        }
    }

    /// Wait for a document that is *not* the stale one a crashed daemon left
    /// behind: a `kill -9` leaves the file in place, so the next start has to be
    /// observed as a republish rather than "the file exists".
    async fn wait_for_republish(&self, stale_pid: u32) -> InstanceFile {
        let started = Instant::now();
        loop {
            if let Some(file) = instance::read(&self.instance_path()) {
                if file.pid != stale_pid {
                    return file;
                }
            }
            assert!(
                started.elapsed() < BOOT_DEADLINE,
                "the instance file {} still describes the dead pid {stale_pid} after \
                 {BOOT_DEADLINE:?}\ndaemon log:\n{}",
                self.instance_path().display(),
                self.log()
            );
            tokio::time::sleep(POLL).await;
        }
    }

    /// Wait for the instance file to disappear (the drain's file step).
    async fn wait_for_files_removed(&self) {
        let started = Instant::now();
        while self.instance_path().exists() || self.lock_path().exists() {
            assert!(
                started.elapsed() < EXIT_DEADLINE,
                "the instance file {} or the lock {} survived a graceful exit",
                self.instance_path().display(),
                self.lock_path().display()
            );
            tokio::time::sleep(POLL).await;
        }
    }
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

fn signal(pid: u32, signal: i32) {
    let result = unsafe { libc::kill(pid as i32, signal) };
    assert_eq!(
        result,
        0,
        "kill({pid}, {signal}) failed: {}",
        std::io::Error::last_os_error()
    );
}

/// `ExitStatus` as the caller observes it: a code, or the negated signal for a
/// process the OS killed.
fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .expect("an exit status is either a code or a signal")
}

async fn wait_exit(child: &mut Child, deadline: Duration) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(
            started.elapsed() < deadline,
            "pid {} did not exit within {deadline:?}",
            child.id()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The first of two children to exit, with its index (0 = first).
async fn wait_any_exit(
    first: &mut Child,
    second: &mut Child,
    deadline: Duration,
) -> (usize, ExitStatus) {
    let started = Instant::now();
    loop {
        if let Some(status) = first.try_wait().expect("try_wait") {
            return (0, status);
        }
        if let Some(status) = second.try_wait().expect("try_wait") {
            return (1, status);
        }
        assert!(
            started.elapsed() < deadline,
            "neither pid {} nor pid {} exited within {deadline:?}",
            first.id(),
            second.id()
        );
        tokio::time::sleep(POLL).await;
    }
}

fn is_running(child: &mut Child) -> bool {
    child.try_wait().expect("try_wait").is_none()
}

/// The host origin's status code, or `None` when nothing answers there.
async fn http_status(origin: &str, path: &str) -> Option<u16> {
    let client = reqwest::Client::builder().no_proxy().build().ok()?;
    client
        .get(format!("{origin}{path}"))
        .send()
        .await
        .ok()
        .map(|response| response.status().as_u16())
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(file: &InstanceFile) -> Socket {
    let url = format!(
        "ws://{}{ADMIN_WS_PATH}",
        file.admin_origin.trim_start_matches("http://")
    );
    let (socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("the admin listener accepts a real client");
    socket
}

fn hello(file: &InstanceFile, pid: u32) -> Message {
    Message::Text(
        json!({
            "type": "admin_hello",
            "adminToken": file.admin_token,
            "role": "manager",
            "appVersion": env!("CARGO_PKG_VERSION"),
            "buildId": picot_host::BUILD_ID,
            "pid": pid,
        })
        .to_string(),
    )
}

/// The next frame, or the close code the daemon chose.
enum Frame {
    Json(Value),
    Closed(u16, String),
}

async fn next_frame(socket: &mut Socket) -> Frame {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(20), socket.next())
            .await
            .expect("a frame within the deadline")
            .expect("the socket is open")
            .expect("a valid frame");
        match message {
            Message::Text(text) => return Frame::Json(serde_json::from_str(&text).expect("JSON")),
            Message::Close(Some(frame)) => {
                return Frame::Closed(u16::from(frame.code), frame.reason.to_string())
            }
            Message::Close(None) => return Frame::Closed(1005, String::new()),
            _ => continue,
        }
    }
}

/// Step 2 + Step 4: the published document is the daemon's own, `/health`
/// answers, and SIGTERM is a graceful exit that clears both files.
#[tokio::test(flavor = "multi_thread")]
async fn the_real_binary_boots_serves_health_and_a_sigterm_clears_its_files() {
    let world = Standalone::new();
    let mut daemon = world.start();

    let file = world.wait_for_instance().await;
    assert_eq!(
        file.pid,
        daemon.id(),
        "the instance file describes the process that wrote it"
    );
    assert_eq!(file.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        file.build_id,
        picot_host::BUILD_ID,
        "the document carries the build identity the app compares against"
    );
    assert!(
        !file.admin_token.is_empty(),
        "a token gates the admin channel"
    );
    assert!(
        file.manager.is_none(),
        "nothing has attached yet, so there is no manager record"
    );
    assert!(
        file.host_origin.starts_with("http://127.0.0.1:"),
        "loopback only (plan D3): {}",
        file.host_origin
    );
    assert!(
        file.admin_origin.starts_with("http://127.0.0.1:") && file.admin_origin != file.host_origin,
        "the admin channel is its own listener: {}",
        file.admin_origin
    );

    // Plan D7 hardening, on the artefacts a *process* created.
    assert_eq!(mode_of(&world.instance_path()) & 0o077, 0, "instance 0600");
    assert_eq!(mode_of(&world.lock_path()) & 0o077, 0, "lock 0600");
    assert_eq!(mode_of(&world.data_dir) & 0o077, 0, "data dir 0700");

    assert_eq!(
        http_status(&file.host_origin, "/health").await,
        Some(200),
        "the host origin serves /health"
    );
    assert_eq!(
        http_status(&file.host_origin, "/").await,
        Some(200),
        "the host origin serves the static bundle"
    );

    signal(daemon.id(), SIGTERM);
    let status = wait_exit(&mut daemon, EXIT_DEADLINE).await;
    assert_eq!(
        exit_code(status),
        0,
        "a SIGTERM is a graceful exit, not a failure:\n{}",
        world.log()
    );
    world.wait_for_files_removed().await;
    let log = world.log();
    assert!(log.contains("graceful shutdown: SIGTERM"), "{log}");
    assert!(log.contains("exit code 0 (SIGTERM)"), "{log}");
    assert_eq!(
        http_status(&file.host_origin, "/health").await,
        None,
        "the origin stops answering once the daemon is gone"
    );
}

/// Step 1's startup row: two daemons race for one lock file. The loser must
/// exit 3 *before* it can bind or publish anything, so exactly one instance
/// document ever exists.
#[tokio::test(flavor = "multi_thread")]
async fn two_daemons_racing_the_single_instance_lock_yield_one_daemon_and_one_exit_3() {
    let world = Standalone::new();
    let mut first = world.start();
    let mut second = world.start();
    let (loser, status) = wait_any_exit(&mut first, &mut second, EXIT_DEADLINE).await;
    let loser_pid = if loser == 0 { first.id() } else { second.id() };
    let winner = if loser == 0 { &mut second } else { &mut first };

    assert_eq!(
        exit_code(status),
        EXIT_ALREADY_RUNNING,
        "pid {loser_pid} lost the lock and must say so with the documented code:\n{}",
        world.log()
    );

    let file = world.wait_for_instance().await;
    assert_eq!(
        file.pid,
        winner.id(),
        "the daemon that holds the lock is the one that published the document"
    );
    assert_ne!(file.pid, loser_pid);
    assert!(
        is_running(winner),
        "the winner keeps running after the loser gives up"
    );

    // One document, one lock holder, one readiness line: the loser never got as
    // far as binding a listener or publishing a file.
    let log = world.log();
    assert_eq!(
        log.matches("ready host=").count(),
        1,
        "exactly one daemon reached ready:\n{log}"
    );
    assert_eq!(
        log.matches("another daemon holds").count(),
        1,
        "the loser logged why it exited:\n{log}"
    );
    assert_eq!(
        http_status(&file.host_origin, "/health").await,
        Some(200),
        "the winner serves"
    );

    // The winner is the only process left to clean up after.
    signal(winner.id(), SIGTERM);
    assert_eq!(exit_code(wait_exit(winner, EXIT_DEADLINE).await), 0);
    world.wait_for_files_removed().await;
}

/// Step 4's `kill -9` row + D12: a daemon that cannot run its teardown leaves
/// its files behind (orphan evidence — not the app's to delete), and the next
/// daemon takes the released lock and publishes a fresh document.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_daemon_leaves_orphan_evidence_and_the_next_daemon_takes_over() {
    let world = Standalone::new();
    let mut first = world.start();
    let published = world.wait_for_instance().await;

    signal(first.id(), SIGKILL);
    let status = wait_exit(&mut first, EXIT_DEADLINE).await;
    assert_eq!(exit_code(status), -SIGKILL, "kill -9 leaves no exit code");

    // No teardown ran, so both files are still there — and the document still
    // names the dead pid, which is exactly what a probe must disbelieve.
    assert!(world.instance_path().exists(), "the orphan evidence stays");
    assert!(world.lock_path().exists(), "so does the lock file");
    let stale = instance::read(&world.instance_path()).expect("still readable");
    assert_eq!(stale.pid, published.pid);

    // The next daemon: the kernel released the flock on death, so acquire
    // succeeds even though the lock *file* is still on disk.
    let mut second = world.start();
    let fresh = world.wait_for_republish(stale.pid).await;
    assert_eq!(fresh.pid, second.id(), "the new daemon republished");
    assert_ne!(
        fresh.pid, stale.pid,
        "swapped identity, not a reuse of the record"
    );
    assert_eq!(
        http_status(&fresh.host_origin, "/health").await,
        Some(200),
        "the successor is usable"
    );

    signal(second.id(), SIGTERM);
    assert_eq!(exit_code(wait_exit(&mut second, EXIT_DEADLINE).await), 0);
    world.wait_for_files_removed().await;
}

/// Step 1's last row over a real socket: the lease is single, a competing
/// manager is closed 4409 without disturbing the incumbent, and only the lease
/// holder can order the daemon down.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_manager_is_refused_and_the_lease_holder_can_stop_the_real_daemon() {
    let world = Standalone::new();
    let mut daemon = world.start();
    let file = world.wait_for_instance().await;

    let mut incumbent = connect(&file).await;
    incumbent
        .send(hello(&file, std::process::id()))
        .await
        .expect("hello sent");
    match next_frame(&mut incumbent).await {
        Frame::Json(welcome) => {
            assert_eq!(welcome["type"], "admin_welcome", "{welcome}");
            assert_eq!(welcome["epoch"].as_u64(), Some(1), "{welcome}");
            assert_eq!(
                welcome["manager"]["pid"].as_u64(),
                Some(u64::from(std::process::id())),
                "the daemon accepted this app as the manager: {welcome}"
            );
        }
        Frame::Closed(code, reason) => panic!("the first manager must win: {code} {reason}"),
    }

    // The daemon rewrote its own document with this app's record (plan D3).
    let attached = instance::read(&world.instance_path()).expect("readable");
    assert_eq!(
        attached.manager.and_then(|manager| manager.pid),
        Some(std::process::id()),
        "the lease grant published the manager record"
    );

    // A second app: its hello must be refused, and the incumbent must not be
    // kicked (no "last hello wins").
    let mut challenger = connect(&file).await;
    challenger
        .send(hello(&file, std::process::id().wrapping_add(1)))
        .await
        .expect("hello sent");
    match next_frame(&mut challenger).await {
        Frame::Closed(code, _reason) => assert_eq!(
            code, 4409,
            "a competing live manager is closed with the lease-held code"
        ),
        Frame::Json(frame) => panic!("the second manager must not be granted a lease: {frame}"),
    }
    let still_ours = instance::read(&world.instance_path()).expect("readable");
    assert_eq!(
        still_ours.manager.and_then(|manager| manager.pid),
        Some(std::process::id()),
        "the refused hello changed nothing"
    );

    // The lease holder stops the daemon: accepted, not completed (plan D12).
    incumbent
        .send(Message::Text(
            json!({"type": "admin_request", "id": "stop-1", "op": "daemon.stop", "args": {}})
                .to_string(),
        ))
        .await
        .expect("stop sent");
    match next_frame(&mut incumbent).await {
        Frame::Json(result) => {
            assert_eq!(result["type"], "admin_result", "{result}");
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(result["result"], json!({"accepted": true}), "{result}");
        }
        Frame::Closed(code, reason) => panic!("the stop was refused: {code} {reason}"),
    }

    let status = wait_exit(&mut daemon, EXIT_DEADLINE).await;
    assert_eq!(
        exit_code(status),
        0,
        "`daemon.stop` exits gracefully:\n{}",
        world.log()
    );
    world.wait_for_files_removed().await;
    assert!(
        world.log().contains("exit code 0 (daemon.stop)"),
        "{}",
        world.log()
    );
}

fn run_daemon(argv: &[&str]) -> std::process::Output {
    Command::new(daemon_binary())
        .args(argv)
        .output()
        .expect("the daemon binary runs")
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Step 2's argv surface, against the real binary: the documented exit codes and
/// a `--version` that names the build identity both sides compare.
#[tokio::test(flavor = "multi_thread")]
async fn the_argv_and_usage_contract_is_enforced_by_the_real_binary() {
    let world = Standalone::new();

    // `--version` is what a human uses to compare the two sides' identity, so it
    // must print version *and* build id.
    let version = run_daemon(&["--version"]);
    assert_eq!(exit_code(version.status), 0);
    let printed = stdout_of(&version);
    assert!(printed.contains("picot-daemon"), "{printed}");
    assert!(printed.contains(env!("CARGO_PKG_VERSION")), "{printed}");
    assert!(
        printed.contains(picot_host::BUILD_ID),
        "the app compares version + build id, so --version must print both: {printed}"
    );

    let help = run_daemon(&["--help"]);
    assert_eq!(exit_code(help.status), 0);
    assert!(
        stdout_of(&help).contains("--data-dir"),
        "{}",
        stdout_of(&help)
    );

    // An unknown flag is refused, never ignored.
    let unknown = run_daemon(&["--nope"]);
    assert_eq!(exit_code(unknown.status), EXIT_USAGE);
    assert!(
        stderr_of(&unknown).contains("--nope"),
        "the message names the offending flag: {}",
        stderr_of(&unknown)
    );

    // Every path in the contract is required: this argv carries only one of the
    // three (plan D7: the app passes all of them; the daemon guesses none).
    let incomplete = run_daemon(&["--data-dir", "/tmp"]);
    assert_eq!(exit_code(incomplete.status), EXIT_USAGE);
    assert!(
        stderr_of(&incomplete).contains("--config-dir"),
        "the message names the missing flag: {}",
        stderr_of(&incomplete)
    );

    // A flag whose value is missing is a usage error, not a panic.
    let dangling = run_daemon(&["--data-dir"]);
    assert_eq!(exit_code(dangling.status), EXIT_USAGE);

    // A static bundle that does not exist is a boot failure that names the path
    // (and happens before any listener, lock or file is created).
    let bogus = run_daemon(&[
        "--data-dir",
        &world.data_dir.to_string_lossy(),
        "--config-dir",
        &world.config_dir.to_string_lossy(),
        "--static-dir",
        "/definitely/not/here",
    ]);
    assert_eq!(exit_code(bogus.status), EXIT_FAILURE);
    assert!(
        stderr_of(&bogus).contains("/definitely/not/here"),
        "{}",
        stderr_of(&bogus)
    );
    assert!(
        !world.instance_path().exists() && !world.lock_path().exists(),
        "a failed boot must not leave an instance file or a lock behind"
    );
}
