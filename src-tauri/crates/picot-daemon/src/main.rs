// ABOUTME: picot-daemon entry point: argv, single-instance lock, the headless host stack, the loopback
// ABOUTME: admin channel, the instance-file lifecycle, and the SIGTERM/SIGINT/SIGHUP shutdown path.

//! # picot-daemon (plan Task 4)
//!
//! The daemon is the process the WebView and the app talk to. It is spawned by
//! the app (`src-tauri/src/daemon_client/manager.rs`) and outlives it by design:
//! a `kill -9` of Picot leaves the runtimes running (spec §3.2, plan D11).
//!
//! ## Boot order (matters)
//!
//! 1. logging — a detached process must leave a trace before anything can fail;
//! 2. single-instance lock — before the host stack binds anything, so two
//!    simultaneous starters cannot both come up (plan Task 4 Step 1);
//! 3. `DaemonState::start` — the host server binds its loopback origin;
//! 4. `AdminServer::start` on `127.0.0.1:0` + `install_admin` (lease/epoch);
//! 5. `install_control_handler` — the data plane's dispatcher;
//! 6. `set_daemon_files` — the instance store (the admin channel's lease
//!    observer) and the lock, so the drain's file step can remove both;
//! 7. **publish the instance file** — last, because the file is the app's
//!    invitation to connect: it must not exist before the listeners do.
//!
//! ## Exit codes
//!
//! | code | meaning |
//! | --- | --- |
//! | 0 | graceful exit (signal or `daemon.stop`), instance file + lock removed |
//! | 1 | boot/runtime error (the message names the failing path) |
//! | 2 | usage error |
//! | 3 | another daemon already holds the instance lock |
//! | 4 | teardown exceeded its grace period (registry intentionally left for the next daemon's sweep) |
//!
//! Exit code 81 does **not** exist here and never will (plan D10): nothing
//! depends on observing an adopted daemon's exit code — the app confirms death
//! by pid + process start time (`daemon_client/exit_waiter.rs`).
//!
//! ## The app surface
//!
//! Since T5 the daemon's [`AppSurface`] is
//! [`AdminForwardingSurface`]: every forwarded row of the control dispatcher
//! travels over the admin channel to the attached app (plan D5), so one protocol
//! serves both the standalone daemon and the embedded form. With **no** manager
//! attached the same surface fails every row closed with the typed
//! `app_surface_unavailable` — the truthful answer for a daemon no app is
//! driving, and the same string the client surfaces to the WebView.

mod log_sink;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use picot_host::daemon::admin::{AdminConfig, AdminServer};
use picot_host::daemon::instance::{self, DaemonFiles, InstanceFile, InstanceLock, InstanceStore};
use picot_host::daemon::surface_forward::AdminForwardingSurface;
use picot_host::daemon::{DaemonConfig, DaemonState};

/// Grace period for the signal path, the same 5s the app's own termination
/// handlers use (`main.rs::install_termination_handlers`). On expiry the
/// process exits anyway and the child registry is deliberately left behind so
/// the next daemon's `sweep_orphans` can clean up what this one could not.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

const EXIT_OK: u8 = 0;
const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_ALREADY_RUNNING: u8 = 3;
const EXIT_GRACE_EXCEEDED: u8 = 4;

/// Command line contract from plan D7: the app passes every path in, so the
/// daemon never guesses a platform directory.
#[derive(Debug, Default)]
struct Args {
    data_dir: Option<String>,
    config_dir: Option<String>,
    static_dir: Option<String>,
    bundle_identifier: Option<String>,
    version: bool,
    help: bool,
}

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("picot-daemon: {message}");
            eprintln!("{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    if args.help {
        println!("{USAGE}");
        return ExitCode::from(EXIT_OK);
    }
    if args.version {
        println!("{}", build_identity());
        return ExitCode::from(EXIT_OK);
    }

    let paths = match ResolvedPaths::from_args(&args) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("picot-daemon: {message}");
            eprintln!("{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    if let Err(message) = log_sink::install(&paths.log_path, log::LevelFilter::Info) {
        eprintln!("picot-daemon: {message}");
        return ExitCode::from(EXIT_FAILURE);
    }
    log::info!(
        "[picot-daemon] boot {} data-dir={} static-dir={}",
        build_identity(),
        paths.data_dir.display(),
        paths.static_dir.display()
    );

    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => match runtime.block_on(run(paths)) {
            Ok(code) => ExitCode::from(code),
            Err(message) => {
                log::error!("[picot-daemon] {message}");
                eprintln!("picot-daemon: {message}");
                ExitCode::from(EXIT_FAILURE)
            }
        },
        Err(error) => {
            log::error!("[picot-daemon] cannot start the async runtime: {error}");
            eprintln!("picot-daemon: cannot start the async runtime: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

const USAGE: &str = "usage: picot-daemon --data-dir DIR --config-dir DIR --static-dir DIR \
                     [--bundle-identifier ID] [--version] [--help]";

/// The argv surface resolved into the paths the boot order uses.
struct ResolvedPaths {
    data_dir: PathBuf,
    config_dir: PathBuf,
    static_dir: PathBuf,
    bundle_identifier: String,
    instance_path: PathBuf,
    lock_path: PathBuf,
    log_path: PathBuf,
}

impl ResolvedPaths {
    fn from_args(args: &Args) -> Result<Self, String> {
        let data_dir = required(&args.data_dir, "--data-dir")?;
        let config_dir = required(&args.config_dir, "--config-dir")?;
        let static_dir = required(&args.static_dir, "--static-dir")?;
        Ok(Self {
            instance_path: instance::instance_path(&data_dir),
            lock_path: instance::lock_path(&data_dir),
            log_path: instance::log_path(&data_dir),
            data_dir,
            config_dir,
            static_dir,
            // The app always passes its own identifier so OS notifications
            // attribute to picot / picot.dev / picot.internal.
            bundle_identifier: args
                .bundle_identifier
                .clone()
                .unwrap_or_else(|| "com.palandata.picot".to_string()),
        })
    }
}

fn required(value: &Option<String>, flag: &str) -> Result<PathBuf, String> {
    value
        .as_ref()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{flag} is required"))
}

async fn run(paths: ResolvedPaths) -> Result<u8, String> {
    if !paths.static_dir.is_dir() {
        return Err(format!(
            "the WebView static bundle directory {} does not exist",
            paths.static_dir.display()
        ));
    }
    // Plan Task 4 Step 2: the lock comes first. `Ok(None)` means a daemon is
    // already running against this data directory; that is not an error worth a
    // stack trace, it is this process's whole job to not become a second one.
    let Some(lock) = InstanceLock::acquire(&paths.lock_path)? else {
        log::warn!(
            "[picot-daemon] another daemon holds {}; exiting",
            paths.lock_path.display()
        );
        eprintln!(
            "picot-daemon: another Picot daemon already holds {}",
            paths.lock_path.display()
        );
        return Ok(EXIT_ALREADY_RUNNING);
    };

    let token = instance::generate_admin_token();
    // The daemon's app surface is the admin-channel forwarding one (T5, plan
    // D5/D2): the same realization the embedded form uses.
    let surface = Arc::new(AdminForwardingSurface::new());
    let state = Arc::new(
        DaemonState::start(
            DaemonConfig {
                data_dir: paths.data_dir.clone(),
                config_dir: paths.config_dir.clone(),
                static_dir: paths.static_dir.clone(),
                bundle_identifier: paths.bundle_identifier.clone(),
            },
            surface.clone(),
        )
        .await?,
    );
    let admin = AdminServer::start(AdminConfig::new(token.clone())).await?;
    state.install_admin(admin.clone());
    surface.attach(admin.clone());
    state.install_control_handler();

    let store = InstanceStore::new(paths.instance_path.clone());
    state.set_daemon_files(DaemonFiles {
        store: store.clone(),
        lock,
    });
    let file = InstanceFile {
        pid: std::process::id(),
        process_start_time: picot_host::child_supervision::process_start_time(std::process::id()),
        host_origin: state.origin().to_string(),
        admin_origin: admin.origin().to_string(),
        admin_token: token,
        started_at: instance::now_timestamp(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        build_id: picot_host::BUILD_ID.to_string(),
        manager: None,
    };
    store.publish(file)?;
    log::info!(
        "[picot-daemon] ready host={} admin={} instance={}",
        state.origin(),
        admin.origin(),
        paths.instance_path.display()
    );

    let reason = wait_for_shutdown(&state).await;
    let code = shutdown(&state, reason).await;
    log::info!("[picot-daemon] exit code {code} ({reason})");
    Ok(code)
}

/// Block until either the admin channel drained the state (`daemon.stop`) or a
/// termination signal arrives.
async fn wait_for_shutdown(state: &Arc<DaemonState>) -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).ok();
        let mut interrupt = signal(SignalKind::interrupt()).ok();
        let mut hangup = signal(SignalKind::hangup()).ok();
        tokio::select! {
            () = state.drained() => "daemon.stop",
            _ = recv(terminate.as_mut()) => "SIGTERM",
            _ = recv(interrupt.as_mut()) => "SIGINT",
            _ = recv(hangup.as_mut()) => "SIGHUP",
        }
    }
    #[cfg(not(unix))]
    {
        state.drained().await;
        "daemon.stop"
    }
}

#[cfg(unix)]
async fn recv(stream: Option<&mut tokio::signal::unix::Signal>) {
    match stream {
        Some(stream) => {
            stream.recv().await;
        }
        // No handler could be installed: never resolve, so the select stays on
        // the other arms instead of exiting immediately.
        None => std::future::pending::<()>().await,
    }
}

/// Run the drain with the 5s grace (plan: on timeout the child registry is NOT
/// cleared, so the next daemon's orphan sweep takes over).
async fn shutdown(state: &Arc<DaemonState>, reason: &str) -> u8 {
    if reason == "daemon.stop" {
        // `daemon.stop` already ran the drain (the reply is "accepted", and the
        // file step removed the instance file + lock); this is the exit.
        state.drained().await;
        return EXIT_OK;
    }
    let teardown = Arc::clone(state);
    let label = reason.to_string();
    let handle = tokio::spawn(async move { teardown.shutdown_graceful(&label).await });
    match tokio::time::timeout(SHUTDOWN_GRACE, handle).await {
        Ok(_) => EXIT_OK,
        Err(_) => {
            log::warn!(
                "[picot-daemon] {reason}: teardown exceeded the {}s grace period; exiting \
                 without clearing the child registry (the next daemon sweeps it)",
                SHUTDOWN_GRACE.as_secs()
            );
            EXIT_GRACE_EXCEEDED
        }
    }
}

/// Same string the app and the host crate report: one version and one build id
/// per cargo invocation, which is what the version+build identity match
/// (`src-tauri/src/daemon_client/manager.rs`) compares.
fn build_identity() -> String {
    format!(
        "picot-daemon {} (build {})",
        env!("CARGO_PKG_VERSION"),
        picot_host::BUILD_ID
    )
}

impl Args {
    fn parse(argv: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = Args::default();
        let mut argv = argv.peekable();
        while let Some(flag) = argv.next() {
            match flag.as_str() {
                "--version" | "-V" => args.version = true,
                "--help" | "-h" => args.help = true,
                "--data-dir" | "--config-dir" | "--static-dir" | "--bundle-identifier" => {
                    let value = argv
                        .next()
                        .ok_or_else(|| format!("{flag} requires a value"))?;
                    match flag.as_str() {
                        "--data-dir" => args.data_dir = Some(value),
                        "--config-dir" => args.config_dir = Some(value),
                        "--static-dir" => args.static_dir = Some(value),
                        _ => args.bundle_identifier = Some(value),
                    }
                }
                other => return Err(format!("unrecognized argument {other}")),
            }
        }
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_matches_the_plan_contract() {
        let args = Args::parse(
            [
                "--data-dir",
                "/d",
                "--config-dir",
                "/c",
                "--static-dir",
                "/s",
                "--bundle-identifier",
                "com.palandata.picot.dev",
            ]
            .iter()
            .map(|value| value.to_string()),
        )
        .expect("the documented argv parses");
        let paths = ResolvedPaths::from_args(&args).expect("every path resolves");
        assert_eq!(paths.data_dir, PathBuf::from("/d"));
        assert_eq!(paths.config_dir, PathBuf::from("/c"));
        assert_eq!(paths.static_dir, PathBuf::from("/s"));
        assert_eq!(paths.bundle_identifier, "com.palandata.picot.dev");
        // Both sides derive the file names from the shared lib (plan D7).
        assert_eq!(paths.instance_path, PathBuf::from("/d/picot-daemon.json"));
        assert_eq!(paths.lock_path, PathBuf::from("/d/picot-daemon.lock"));
        assert_eq!(paths.log_path, PathBuf::from("/d/picot-daemon.log"));

        let version = Args::parse(["--version"].iter().map(|v| v.to_string())).expect("version");
        assert!(version.version);

        assert!(
            Args::parse(["--nope"].iter().map(|v| v.to_string())).is_err(),
            "unknown flags are refused, not ignored"
        );
        let incomplete =
            Args::parse(["--data-dir", "/d"].iter().map(|v| v.to_string())).expect("parses");
        assert!(
            ResolvedPaths::from_args(&incomplete).is_err(),
            "every path in the contract is required"
        );
    }

    #[test]
    fn a_daemon_with_no_app_fails_closed_on_every_forwarded_row() {
        use picot_host::daemon::app_surface::AppSurface;
        // No admin attachment: the forwarding surface must still fail every row
        // closed with the typed error the client surfaces to the WebView.
        let surface = AdminForwardingSurface::new();
        let failures = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                vec![
                    surface.open_devtools("w1").await,
                    surface.close_window("w1", "r1").await,
                    surface.cancel_window_close("w1", "r1").await,
                    surface.menu_state_changed().await,
                    surface.relaunch_app().await,
                    surface.pick_folder().await.map(|_| ()),
                    surface.check_for_update().await.map(|_| ()),
                ]
            });
        for failure in failures {
            let message = failure.expect_err("a daemon with no app cannot serve this");
            assert!(
                message.starts_with("app_surface_unavailable:"),
                "the typed failure is what the client surfaces: {message}"
            );
        }
    }
}
