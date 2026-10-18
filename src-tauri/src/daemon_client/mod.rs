// ABOUTME: The app-side daemon client: the manager (probe/spawn/adopt), the confirmed-exit waiter,
// ABOUTME: the restart transaction, and the localized "Picot is already running" policy.

//! # daemon_client (plan Task 4 Step 3)
//!
//! This module is the app's half of the daemon relationship:
//!
//! - [`manager`] — probe, spawn, adopt, version/build-gated stop, readiness;
//! - [`exit_waiter`] — "accepted ≠ completed" (plan D12): confirm a daemon's
//!   death by pid + process start time, escalating SIGTERM → SIGKILL;
//! - [`navigation_mirror`] — the daemon-fed navigation/authorization image and
//!   the app's label↔owner map;
//! - [`admin_client`] — the admin channel client (hello/welcome, id-matched
//!   requests, the read pump, the surface pump) and [`admin_client::CreatedOwner`];
//! - [`surface_ops`] — the app-side execution of forwarded `app_surface_request`
//!   frames on the real `AppSurface`;
//! - [`quit_policy`] — the app-private `keepRunningAfterQuit` file and the
//!   quit transaction (accepted ≠ completed: app exit blocks on a *confirmed*
//!   daemon death, plan D12/T8);
//! - [`window_ledger`] — the app's own registry of the windows it created
//!   (label, owner, owner home, workspace, route): the restart transaction's
//!   pre-state and the three commands' caller-validation set;
//! - [`restart`] — the app-managed restart transaction (plan D10/T9): stop the
//!   captured instance, confirm its death **by pid + start time**, spawn exactly
//!   one replacement, rebuild one owner per captured window, migrate every view
//!   to the new origin, restore its route;
//! - [`commands`] — the app's whole Tauri command surface (T10):
//!   `daemon_status` (the daemon's facts + the app-private quit policy),
//!   `daemon_set_keep_running` (the policy file and nothing else) and
//!   `daemon_restart` (T9's transaction) — each validating the Tauri-injected
//!   calling window against the ledger and the mirror;
//! - [`present_blocked_by_other_app`] — the localized dialog + non-zero exit for
//!   a second Picot instance while another app manages the daemon; parked by
//!   `setup` and presented from `RunEvent::Ready`, on the same mechanism as
//!   [`startup_failure`] (R-1: it used to join its dialog thread inside `setup`,
//!   which is a hang — the plugin posts the dialog to the very thread that was
//!   blocked waiting for it);
//! - [`startup_failure`] — the *other* dialog-and-exit: an unrecoverable boot
//!   failure (a bad sidecar, a daemon that never becomes ready) is presented as
//!   a localized alert plus `exit(1)`, not as a setup-hook panic (an abort with
//!   a macOS crash report).
//!
//! ## Wiring status
//!
//! Wired (T5): `main.rs::setup_daemon_runtime` boots the daemon as its own
//! process (`DaemonManager::ensure`), attaches through
//! [`admin_client::AdminClient`], mints the landing owner over the admin channel
//! and opens the landing window against the daemon's host origin. T12 deleted
//! the in-process mode: the external daemon is the only runtime.
//! Wired (T8): app exit runs [`quit_policy`]'s transaction from
//! `RunEvent::ExitRequested`, `RunEvent::Exit` and the SIGTERM/SIGINT/SIGHUP
//! handlers. Wired (T9): [`restart`]'s transaction behind the `daemon_restart`
//! command and `crate::run_restart`, with the window ledger feeding the
//! capture. Wired (T10): all three commands registered in
//! `main.rs`'s `invoke_handler` with explicit permissions
//! (`src-tauri/permissions/default.toml`), and
//! `public/settings/daemon-section.js` driving them from Settings → General in
//! both shells.
//!
//! [`navigation_mirror`] is the app-side half of the daemon's authorization
//! mirror: the WebView's synchronous `on_navigation` delegate reads it, and it
//! fails closed (only the opaque `about:` documents survive a dropped channel).

pub mod admin_client;
pub mod commands;
pub mod exit_waiter;
pub mod manager;
pub mod navigation_mirror;
pub mod notification_routing;
pub mod quit_policy;
pub mod restart;
pub mod startup_failure;
pub mod surface_ops;
pub mod window_ledger;

/// The exit code of a second instance that is blocked by an already-running
/// Picot. Non-zero and distinct from the usage error (2) and from a generic
/// failure (1).
pub const EXIT_BLOCKED_BY_OTHER_APP: i32 = 3;

/// How long the blocked instance waits for the user to dismiss the
/// "already running" dialog before exiting on its own.
///
/// The same **backstop** as [`startup_failure::STARTUP_FAILURE_DIALOG_TIMEOUT`],
/// for the same reason: the dialog itself is modal and stays up as long as the
/// user leaves it up, but a main thread that never services the post (or a
/// session without a window server) must end in the documented non-zero exit
/// rather than in a hang.
pub const BLOCKED_BY_OTHER_APP_DIALOG_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(120);

/// The four locales the app's UI ships in (`public/locales/{en,zh,ja,es}.json`).
pub const LOCALES: [&str; 4] = ["en", "zh", "ja", "es"];

/// The dialog copy, in all four locales.
///
/// This is a *native* dialog shown before the WebView exists, so it cannot go
/// through `public/locales/*.json` (nothing has loaded a locale bundle yet, and
/// the blocked path is exactly the path where the app should not start a
/// WebView). The copy therefore lives here; the function name mirrors T10's
/// `settings.daemon.*` namespace so the daemon lifecycle's copy stays greppable
/// from either side of the process boundary.
pub fn blocked_by_other_app_copy(locale: &str) -> (&'static str, &'static str) {
    match locale {
        "zh" => (
            "Picot 已在运行",
            "另一个 Picot 窗口正在管理后台守护进程和你正在运行的代理。\n\
             请切回那个窗口继续使用，或先退出它再重新打开本窗口。",
        ),
        "ja" => (
            "Picot はすでに実行中です",
            "別の Picot ウィンドウがバックグラウンドのデーモンと実行中のエージェントを管理しています。\n\
             そのウィンドウに戻って続けるか、いったん終了してからこのウィンドウを開き直してください。",
        ),
        "es" => (
            "Picot ya se está ejecutando",
            "Otra ventana de Picot está gestionando el daemon en segundo plano y tus agentes en ejecución.\n\
             Vuelve a esa ventana para continuar, o ciérrala y vuelve a abrir esta.",
        ),
        _ => (
            "Picot is already running",
            "Another Picot window is managing the background daemon and your running agents.\n\
             Switch back to that window to continue, or quit it and open this one again.",
        ),
    }
}

/// Resolve a UI locale preference (`"system"`, `"en"`, `"zh"`, …) to one of
/// [`LOCALES`]. "system" (and an unknown value) falls back to the OS locale,
/// then to English — the same fallback order `public/i18n.js` uses.
pub fn resolve_locale(preference: Option<&str>) -> String {
    fn normalize(value: &str) -> Option<&'static str> {
        let lowered = value.trim().to_ascii_lowercase();
        LOCALES
            .iter()
            .find(|locale| lowered == **locale || lowered.starts_with(&format!("{locale}-")))
            .copied()
    }
    match preference.and_then(normalize) {
        Some(locale) => locale.to_string(),
        None => ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|key| std::env::var(key).ok())
            .find_map(|value| normalize(&value))
            .unwrap_or("en")
            .to_string(),
    }
}

/// Everything the blocked-by-other-app path presents: the resolved locale, the
/// localized copy, and how the process leaves.
///
/// Pure and window-server-free, so the whole decision is a unit-testable seam
/// (the same role [`startup_failure::StartupFailure`] + `startup_failure_copy`
/// play for the boot-failure alert).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedByOtherAppPresentation {
    pub locale: String,
    pub title: String,
    pub message: String,
    pub exit_code: i32,
    pub dialog_timeout: std::time::Duration,
}

/// Resolve the blocked-by-other-app presentation for a UI locale preference.
///
/// One place decides the locale fallback, the copy and the exit code, so the
/// dialog and the exit can never disagree.
pub fn blocked_by_other_app_presentation(
    preference: Option<&str>,
) -> BlockedByOtherAppPresentation {
    let locale = resolve_locale(preference);
    let (title, message) = blocked_by_other_app_copy(&locale);
    BlockedByOtherAppPresentation {
        locale,
        title: title.to_string(),
        message: message.to_string(),
        exit_code: EXIT_BLOCKED_BY_OTHER_APP,
        dialog_timeout: BLOCKED_BY_OTHER_APP_DIALOG_TIMEOUT,
    }
}

/// Show the localized "Picot is already running" alert and exit
/// [`EXIT_BLOCKED_BY_OTHER_APP`].
///
/// Must be called **from the run loop** (the `RunEvent::Ready` arm), never from
/// `setup`: the dialog plugin posts the alert to the main thread, so a caller
/// that blocks on it from `setup` — which runs on that very thread — waits
/// forever for a dialog that can never be drawn (no watchdog, no exit). The
/// `setup` hook therefore *parks* the block and this runs once the main thread
/// is back in its run loop; see [`startup_failure`]'s module docs for the full
/// shape, and [`startup_failure::spawn_alert_and_exit`] for the worker threads
/// this hands the job to. Returns immediately; the process is left by one of
/// them.
///
/// `preference` is the app's UI locale preference; `None` resolves the locale
/// from the OS (`resolve_locale`).
pub fn present_blocked_by_other_app(app: &tauri::AppHandle, preference: Option<&str>) {
    let presentation = blocked_by_other_app_presentation(preference);
    log::warn!(
        "[picot] another Picot instance manages the daemon; exiting {} ({})",
        presentation.exit_code,
        presentation.locale
    );
    startup_failure::spawn_alert_and_exit(
        app,
        presentation.title,
        presentation.message,
        presentation.exit_code,
        presentation.dialog_timeout,
        "picot-blocked-by-other-app",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_locale_has_its_own_copy() {
        let mut messages = Vec::new();
        let mut titles = Vec::new();
        for locale in LOCALES {
            let (title, message) = blocked_by_other_app_copy(locale);
            assert!(!title.is_empty(), "{locale} needs a title");
            assert!(!message.is_empty(), "{locale} needs a message");
            assert!(
                message.contains('\n') || message.len() > 60,
                "{locale} must explain what to do, not just state the fact: {message}"
            );
            titles.push(title);
            messages.push(message);
        }
        let distinct_titles: std::collections::HashSet<_> = titles.iter().collect();
        assert_eq!(
            distinct_titles.len(),
            LOCALES.len(),
            "each locale has its own copy: {titles:?}"
        );
        // The dialog must never leak the other instance's identity or paths.
        for message in messages {
            assert!(!message.contains("pid"), "{message}");
        }
    }

    /// The bytes of a Rust function's body, from the `marker` line to the first
    /// column-zero closing brace. Lets the source itself be asserted on — the
    /// regression below is a *shape*, not a value, so no runtime seam can see it.
    fn source_body<'a>(source: &'a str, marker: &str) -> &'a str {
        let start = source
            .find(marker)
            .unwrap_or_else(|| panic!("{marker} not found in the source"));
        let rest = &source[start..];
        let end = rest.find("\n}").map(|i| i + 2).unwrap_or(rest.len());
        &rest[..end]
    }

    /// The presentation seam, mirroring `startup_failure`'s tests: the copy is
    /// the localized one, the exit code is the product's own (3, distinct from
    /// the boot failure's 1 and the usage error's 2), and the backstop is a
    /// real bound — the hang R-1 fixed had no exit at all.
    #[test]
    fn the_blocked_presentation_carries_the_copy_and_a_distinct_exit_code() {
        let mut titles = Vec::new();
        let mut messages = Vec::new();
        for locale in LOCALES {
            let presentation = blocked_by_other_app_presentation(Some(locale));
            assert_eq!(presentation.locale, locale);
            assert_eq!(presentation.exit_code, EXIT_BLOCKED_BY_OTHER_APP);
            assert_eq!(presentation.exit_code, 3);
            assert_ne!(presentation.exit_code, 1);
            assert_ne!(presentation.exit_code, 2);
            assert_eq!(
                presentation.dialog_timeout,
                BLOCKED_BY_OTHER_APP_DIALOG_TIMEOUT
            );
            assert!(
                presentation.dialog_timeout >= std::time::Duration::from_secs(30)
                    && presentation.dialog_timeout <= std::time::Duration::from_secs(300),
                "the backstop must be a real bound: {:?}",
                presentation.dialog_timeout
            );
            assert_eq!(
                (presentation.title.as_str(), presentation.message.as_str()),
                blocked_by_other_app_copy(locale),
                "the presentation must be the localized copy, not a new string"
            );
            titles.push(presentation.title);
            messages.push(presentation.message);
        }
        assert_eq!(
            titles
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            LOCALES.len()
        );
        assert_eq!(
            messages
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            LOCALES.len()
        );
    }

    /// `None` (the setup hook's call — no settings file has been read yet)
    /// still resolves to one of the shipped locales rather than panicking or
    /// emitting English-only copy.
    #[test]
    fn the_blocked_presentation_resolves_the_os_locale_without_a_preference() {
        let presentation = blocked_by_other_app_presentation(None);
        assert!(LOCALES.contains(&presentation.locale.as_str()));
        assert_eq!(presentation.exit_code, EXIT_BLOCKED_BY_OTHER_APP);
    }

    /// R-1: the blocked instance's backstop is the same kind of bound as the
    /// boot failure's (the mechanism was reused, not re-invented), while its
    /// exit code stays 3.
    #[test]
    fn the_blocked_backstop_matches_the_startup_failure_mechanism() {
        assert_eq!(
            BLOCKED_BY_OTHER_APP_DIALOG_TIMEOUT,
            startup_failure::STARTUP_FAILURE_DIALOG_TIMEOUT
        );
        assert_ne!(
            EXIT_BLOCKED_BY_OTHER_APP,
            startup_failure::EXIT_STARTUP_FAILURE
        );
    }

    /// R-1: the blocked-by-other-app dialog used to be joined *inside* `setup`.
    ///
    /// The dialog plugin posts the alert to the main thread, and `setup` runs
    /// on that very thread — so the join waits on a dialog that can never be
    /// drawn: no alert, no watchdog, no exit (the startup-failure module docs
    /// name this exact shape). The fix parks the block and presents it from
    /// `RunEvent::Ready`, whose presenter returns immediately on worker
    /// threads. Both halves are asserted here, so neither can regress silently.
    #[test]
    fn the_blocked_dialog_is_neither_joined_nor_shown_from_setup() {
        let presenter = source_body(
            include_str!("mod.rs"),
            "pub fn present_blocked_by_other_app",
        );
        for forbidden in ["join(", "std::process::exit"] {
            assert!(
                !presenter.contains(forbidden),
                "the presenter must return at once (a worker thread exits): {forbidden}\n{presenter}"
            );
        }

        let main = include_str!("../main.rs");
        let setup = source_body(main, "fn setup_daemon_runtime");
        assert!(
            !setup.contains("present_blocked_by_other_app"),
            "setup must park the block, never present it (R-1): {setup}"
        );
        assert!(
            setup.contains("SetupOutcome::BlockedByOtherApp"),
            "setup must report the park so `Ready` can present it: {setup}"
        );
        let ready = source_body(main, "tauri::RunEvent::Ready =>");
        assert!(
            ready.contains("present_blocked_by_other_app"),
            "`Ready` is where the parked block is presented: {ready}"
        );
    }

    #[test]
    fn locale_resolution_covers_preference_os_and_fallback() {
        assert_eq!(resolve_locale(Some("zh")), "zh");
        assert_eq!(resolve_locale(Some("JA")), "ja");
        assert_eq!(resolve_locale(Some("es-MX")), "es");
        assert_eq!(resolve_locale(Some("en-US")), "en");
        // Unknown / "system" fall through to the OS locale, then English. The
        // assertion is on the *allowed* set so a machine's LANG cannot fail it.
        let resolved = resolve_locale(Some("system"));
        assert!(LOCALES.contains(&resolved.as_str()), "{resolved}");
        assert!(LOCALES.contains(&resolve_locale(None).as_str()));
    }

    #[test]
    fn the_locale_resolver_never_returns_an_unknown_locale() {
        for preference in [None, Some(""), Some("system"), Some("de"), Some("ZH_cn")] {
            let resolved = resolve_locale(preference);
            assert!(
                LOCALES.contains(&resolved.as_str()),
                "{preference:?} resolved to {resolved}"
            );
        }
    }
}
