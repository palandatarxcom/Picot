// ABOUTME: The app-side daemon client: the manager (probe/spawn/adopt), the confirmed-exit waiter,
// ABOUTME: and the localized "Picot is already running" policy for a blocked second instance.

//! # daemon_client (plan Task 4 Step 3)
//!
//! This module is the app's half of the daemon relationship:
//!
//! - [`manager`] — probe, spawn, adopt, version/build-gated stop, readiness;
//! - [`exit_waiter`] — "accepted ≠ completed" (plan D12): confirm a daemon's
//!   death by pid + process start time, escalating SIGTERM → SIGKILL;
//! - [`present_blocked_by_other_app`] — the localized dialog + non-zero exit for
//!   a second Picot instance while another app manages the daemon.
//!
//! ## Wiring status
//!
//! T4 delivers this module; **T5 wires it into startup** (`PICOT_DAEMON=external`
//! / embedded branching) and T10 exposes it through the three admin commands.
//! Until then nothing in the app calls it, which is why `main.rs` gates the
//! module with `#[allow(dead_code)]` — a recorded, temporary posture, not a
//! silent one. The M2 milestone contract is explicit that the app stays embedded
//! in this task, so wiring the manager here would be the wrong change.

pub mod exit_waiter;
pub mod manager;

/// The exit code of a second instance that is blocked by an already-running
/// Picot. Non-zero and distinct from the usage error (2) and from a generic
/// failure (1).
pub const EXIT_BLOCKED_BY_OTHER_APP: i32 = 3;

/// The four locales the app's UI ships in (`public/locales/{en,zh,ja,es}.json`).
pub const LOCALES: [&str; 4] = ["en", "zh", "ja", "es"];

/// The dialog copy, in all four locales.
///
/// This is a *native* dialog shown before the WebView exists, so it cannot go
/// through `public/locales/*.json` (nothing has loaded a locale bundle yet, and
/// the blocked path is exactly the path where the app should not start a
/// WebView). The keys are named after the ones T10 adds to the settings section
/// (`daemon.alreadyRunning.*`) so the copy stays greppable across both.
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

/// Show the localized dialog and exit non-zero.
///
/// Runs the dialog on its own thread: `blocking_show` blocks until the user
/// dismisses it, and the caller may well be the app's main thread (Tauri's
/// startup path), which must not block on a modal dialog that the same thread
/// has to service.
pub fn present_blocked_by_other_app(app: &tauri::AppHandle, preference: Option<&str>) -> ! {
    use tauri_plugin_dialog::DialogExt;

    let locale = resolve_locale(preference);
    let (title, message) = blocked_by_other_app_copy(&locale);
    log::warn!("[picot] another Picot instance manages the daemon; exiting ({locale})");
    let handle = app.clone();
    let shown = std::thread::spawn(move || {
        handle
            .dialog()
            .message(message)
            .title(title)
            .blocking_show();
    });
    // A failure to show the dialog (headless, no window server) must not turn
    // into "Picot silently keeps running as a second instance".
    let _ = shown.join();
    std::process::exit(EXIT_BLOCKED_BY_OTHER_APP);
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
