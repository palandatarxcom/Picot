// ABOUTME: The unrecoverable boot failure's product alert: localized title/reason/log path, then
// ABOUTME: exit(1) — instead of the setup-hook panic (abort, exit 134, macOS crash report).

//! # A boot failure is a dialog, not a panic
//!
//! The app's one startup sequence is `main.rs::setup_daemon_runtime`: resolve
//! the data/config/static dirs, `DaemonManager::ensure` the daemon, attach the
//! admin channel, mint the landing owner, open the landing window. Every step
//! can fail in a packaged install — a truncated or unsigned sidecar, a daemon
//! that never answers inside the 15 s ready deadline, a read-only data dir.
//!
//! Returning that failure from Tauri's `setup` hook is not an option: Tauri
//! panics on a setup error (v2.11 `app.rs`: `if let Err(e) = setup(&mut self) {
//! panic!("Failed to setup app: {e}") }`), and a panic on the main thread of a
//! bundled macOS app becomes an abort — `exit 134` (SIGABRT) plus a system
//! "Picot quit unexpectedly" crash report. The user sees an OS crash, not a
//! product that tells them which file is bad and where the log is.
//!
//! So the failure is **parked** by the setup hook and **presented** from the
//! run loop's `Ready` event:
//!
//! 1. `setup` records the [`StartupFailure`] and returns `Ok(())` (the process
//!    is on its way out; no window is opened, so nothing else runs).
//! 2. `RunEvent::Ready` — which fires after `setup` returns — hands it to
//!    [`present_startup_failure`], which shows the alert and exits
//!    [`EXIT_STARTUP_FAILURE`].
//!
//! ## Why the alert must be presented from `Ready`, on another thread
//!
//! Both Tauri's `setup` hook and the run-event callback execute **on the main
//! thread, inside the event loop's callback**. The dialog plugin does not draw
//! on the calling thread: `tauri_plugin_dialog::show_message_dialog` posts the
//! alert to the main thread (`run_on_main_thread` → an `EventLoopProxy` user
//! event). A caller that blocks on the user while holding the main thread is
//! therefore a **new hang**: the post can never be serviced, the alert never
//! appears, and the app stays alive forever — strictly worse than the crash
//! report it replaces (`present_blocked_by_other_app` in `mod.rs` joins its
//! dialog thread *inside* `setup` and has exactly this shape).
//!
//! Hence: [`present_startup_failure`] spawns the dialog thread and returns
//! immediately. The main thread finishes the `Ready` callback and goes back to
//! its run loop, which services the post; the worker thread — not the main
//! thread — waits for the dismissal and calls `exit`. A second worker holds the
//! deadline backstop ([`STARTUP_FAILURE_DIALOG_TIMEOUT`]): whatever keeps the
//! alert from being serviced or dismissed, the process still exits non-zero
//! instead of hanging.
//!
//! ## Copy
//!
//! The alert's chrome (title, the "it will quit now" line, the log-file label,
//! the dismissal line) is localized in the app's four locales, resolved without
//! a preference — no settings file has been read at this point, so the OS
//! locale is the only signal (`super::resolve_locale`). The `reason` line is
//! **not** localized: it is the internal error text (`DaemonManagerError`'s
//! `Display`), and keeping it verbatim is what makes the dialog greppable
//! against the daemon log. The daemon log path is the same one the ready-timeout
//! error already carries (`picot_host::daemon::instance::log_path`).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use tauri_plugin_dialog::DialogExt;

use super::resolve_locale;

/// Exit code of a boot the app could not recover from. Non-zero, and distinct
/// from the second-instance block ([`super::EXIT_BLOCKED_BY_OTHER_APP`] = 3,
/// reserved for "another Picot owns the daemon") and from a usage error (2).
pub const EXIT_STARTUP_FAILURE: i32 = 1;

/// How long the app waits for the user to dismiss the startup-failure alert
/// before exiting on its own.
///
/// This is a **backstop**, not a UI deadline: the alert itself is modal and
/// stays up as long as the user leaves it up. What it bounds is the case where
/// the alert can never be serviced at all (a main thread that never returns to
/// its run loop, a session without a window server), which must end in a
/// non-zero exit rather than in a hang.
pub const STARTUP_FAILURE_DIALOG_TIMEOUT: Duration = Duration::from_secs(120);

/// Why the app could not boot, in the shape the alert needs: what went wrong,
/// and which log file carries the detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupFailure {
    /// The internal error text (`DaemonManagerError`'s `Display`, or the
    /// setup step's own message). Displayed verbatim, so it must not carry
    /// secrets — it never does: the manager's messages name paths and pids.
    pub reason: String,
    /// The daemon log file (`<data_dir>/picot-daemon.log`), or `None` when the
    /// failure happened before a data directory was resolved.
    pub log_path: Option<PathBuf>,
}

impl StartupFailure {
    pub fn new(reason: impl Into<String>, log_path: Option<PathBuf>) -> Self {
        Self {
            reason: reason.into(),
            log_path,
        }
    }

    /// A failure from a boot step that had already resolved `data_dir`, so the
    /// alert can point at the daemon log even when the error text does not.
    pub fn for_data_dir(reason: impl Into<String>, data_dir: &Path) -> Self {
        Self::new(
            reason,
            Some(picot_host::daemon::instance::log_path(data_dir)),
        )
    }
}

impl From<String> for StartupFailure {
    fn from(reason: String) -> Self {
        Self::new(reason, None)
    }
}

/// The alert's copy for one locale: `(title, message)`.
///
/// `message` is assembled as: the localised "Picot could not start" line, the
/// verbatim reason, the log file (a localised label plus the path, or the
/// localised "no log file could be written" line), and the localised dismissal
/// line. The label is a plain "Log file:" rather than a claim that the detail
/// is in there: a spawn that never reached the daemon leaves no log behind.
/// Pure, so the seam is unit-testable without a window server.
pub fn startup_failure_copy(locale: &str, failure: &StartupFailure) -> (String, String) {
    struct Copy {
        title: &'static str,
        intro: &'static str,
        log_label: &'static str,
        no_log: &'static str,
        dismiss: &'static str,
    }
    let copy = match locale {
        "zh" => Copy {
            title: "Picot 无法启动",
            intro: "Picot 启动失败，即将退出。",
            log_label: "日志文件：",
            no_log: "本次未能写入日志文件。",
            dismiss: "点击「好」退出 Picot。",
        },
        "ja" => Copy {
            title: "Picot を起動できませんでした",
            intro: "Picot の起動に失敗したため、まもなく終了します。",
            log_label: "ログファイル：",
            no_log: "今回はログファイルを書き出せませんでした。",
            dismiss: "「OK」をクリックすると Picot を終了します。",
        },
        "es" => Copy {
            title: "Picot no pudo iniciarse",
            intro: "Picot no pudo iniciarse, así que se cerrará ahora.",
            log_label: "Archivo de registro:",
            no_log: "No se pudo escribir ningún archivo de registro.",
            dismiss: "Haz clic en Aceptar para cerrar Picot.",
        },
        _ => Copy {
            title: "Picot could not start",
            intro: "Picot could not start, so it will quit now.",
            log_label: "Log file:",
            no_log: "No log file could be written this time.",
            dismiss: "Click OK to quit Picot.",
        },
    };

    let mut message = String::new();
    message.push_str(copy.intro);
    message.push_str("\n\n");
    message.push_str(failure.reason.trim());
    message.push_str("\n\n");
    match failure.log_path.as_deref() {
        Some(path) => {
            message.push_str(copy.log_label);
            message.push('\n');
            message.push_str(&path.display().to_string());
        }
        None => message.push_str(copy.no_log),
    }
    message.push_str("\n\n");
    message.push_str(copy.dismiss);

    (copy.title.to_string(), message)
}

/// Show the boot failure as a native alert, then exit
/// [`EXIT_STARTUP_FAILURE`].
///
/// Must be called **from the run loop** (the `RunEvent::Ready` arm), never from
/// `setup` — see the module docs: the alert is serviced by the main thread, so
/// the caller has to let the main thread return to its run loop. This function
/// therefore returns as soon as the two worker threads are started; the process
/// is left by one of them.
pub fn present_startup_failure(app: &tauri::AppHandle, failure: StartupFailure) {
    let locale = resolve_locale(None);
    let (title, message) = startup_failure_copy(&locale, &failure);
    log::error!(
        "[picot] startup failed ({locale}): {} — exiting {EXIT_STARTUP_FAILURE}",
        failure.reason
    );
    if let Some(path) = failure.log_path.as_deref() {
        log::error!("[picot] daemon log: {}", path.display());
    }

    let (dismissed_tx, dismissed_rx) = mpsc::channel::<()>();
    let handle = app.clone();
    let dialog = std::thread::Builder::new()
        .name("picot-startup-failure-dialog".to_string())
        .spawn(move || {
            handle
                .dialog()
                .message(message)
                .title(title)
                .blocking_show();
            // The alert is gone; the watcher below owns the exit from here.
            let _ = dismissed_tx.send(());
        });
    if let Err(error) = dialog {
        // No alert is possible at all (thread creation failed): the log line
        // above is the report, and the exit code still says "failed to boot".
        log::error!("[picot] could not open the startup-failure alert: {error}");
        std::process::exit(EXIT_STARTUP_FAILURE);
    }

    // The deadline backstop. A thread of its own, so the caller (the run loop's
    // `Ready` callback, on the main thread) returns immediately and the posted
    // alert can actually be drawn.
    let watcher = std::thread::Builder::new()
        .name("picot-startup-failure-watchdog".to_string())
        .spawn(move || {
            match dismissed_rx.recv_timeout(STARTUP_FAILURE_DIALOG_TIMEOUT) {
                Ok(()) => log::info!("[picot] the startup-failure alert was dismissed"),
                Err(_) => log::warn!(
                    "[picot] the startup-failure alert was not dismissed within {}s; exiting",
                    STARTUP_FAILURE_DIALOG_TIMEOUT.as_secs()
                ),
            }
            std::process::exit(EXIT_STARTUP_FAILURE);
        });
    if let Err(error) = watcher {
        // Without the watcher the process would hang after the alert: exit
        // instead, since a boot failure must never leave a live app behind.
        log::error!("[picot] could not start the startup-failure watchdog: {error}");
        std::process::exit(EXIT_STARTUP_FAILURE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breaker() -> StartupFailure {
        StartupFailure::new(
            "the Picot daemon did not become ready within 15s (connection refused); see \
             /Users/x/Library/Application Support/com.palandata.picot/picot-daemon.log",
            Some(PathBuf::from(
                "/Users/x/Library/Application Support/com.palandata.picot/picot-daemon.log",
            )),
        )
    }

    /// The alert has to say three things — what happened, why, and where the
    /// detail is — and must never be the OS crash report it replaces.
    #[test]
    fn every_locale_names_the_reason_and_the_log_file() {
        let failure = breaker();
        let mut titles = Vec::new();
        let mut messages = Vec::new();
        for locale in super::super::LOCALES {
            let (title, message) = startup_failure_copy(locale, &failure);
            assert!(!title.is_empty(), "{locale} needs a title");
            assert!(
                message.contains(failure.reason.trim()),
                "{locale} must carry the reason verbatim: {message}"
            );
            assert!(
                message.contains("/Users/x/Library/Application Support"),
                "{locale} must name the log path: {message}"
            );
            for forbidden in ["panic", "abort", "SIGABRT", "134", "expect("] {
                for surface in [&title, &message] {
                    assert!(
                        !surface.contains(forbidden),
                        "{locale} must read as a product message, not a crash: {surface}"
                    );
                }
            }
            titles.push(title);
            messages.push(message);
        }
        let distinct_titles: std::collections::HashSet<_> = titles.iter().collect();
        let distinct_messages: std::collections::HashSet<_> = messages.iter().collect();
        assert_eq!(
            distinct_titles.len(),
            super::super::LOCALES.len(),
            "{titles:?}"
        );
        assert_eq!(
            distinct_messages.len(),
            super::super::LOCALES.len(),
            "each locale has its own copy"
        );
    }

    /// A failure before the data directory existed still explains itself, and
    /// says the log is missing rather than printing a blank path.
    #[test]
    fn a_failure_without_a_log_path_says_so() {
        let failure = StartupFailure::from(String::from(
            "Cannot resolve Picot app data directory: boom",
        ));
        let (_, message) = startup_failure_copy("en", &failure);
        assert!(
            message.contains("Cannot resolve Picot app data directory"),
            "{message}"
        );
        assert!(
            message.contains("No log file could be written"),
            "{message}"
        );
        assert!(!message.contains(":\n\n"), "{message}");
    }

    /// The `<data_dir>/picot-daemon.log` chain is the same one the manager's
    /// ready-timeout error uses, so both halves of the alert agree.
    #[test]
    fn a_failure_from_a_data_dir_points_at_the_daemon_log() {
        let failure = StartupFailure::for_data_dir("daemon died", Path::new("/tmp/picot-data"));
        assert_eq!(
            failure.log_path,
            Some(picot_host::daemon::instance::log_path(Path::new(
                "/tmp/picot-data"
            )))
        );
        let (_, message) = startup_failure_copy("en", &failure);
        assert!(
            message.contains("/tmp/picot-data/picot-daemon.log"),
            "{message}"
        );
    }

    /// The exit code is the product's own, and the backstop is a real bound.
    #[test]
    fn the_exit_code_and_backstop_are_distinct_and_bounded() {
        assert_eq!(EXIT_STARTUP_FAILURE, 1);
        assert_ne!(
            EXIT_STARTUP_FAILURE,
            super::super::EXIT_BLOCKED_BY_OTHER_APP
        );
        assert!(STARTUP_FAILURE_DIALOG_TIMEOUT >= Duration::from_secs(30));
        assert!(STARTUP_FAILURE_DIALOG_TIMEOUT <= Duration::from_secs(300));
    }
}
