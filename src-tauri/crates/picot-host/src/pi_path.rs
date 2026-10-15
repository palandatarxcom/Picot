use std::path::PathBuf;

pub const PREF_KEY: &str = "pi.pathEnabled";

const MARKER_BEGIN: &str = "# >>> picot embedded pi >>>";
const MARKER_END: &str = "# <<< picot embedded pi <<<";

/// Q3: one rc file per detected login shell; unsupported shells disable the
/// toggle instead of guessing a file.
pub fn rc_filename_for_shell(shell: &str) -> Option<&'static str> {
    let basename = shell.rsplit('/').next().unwrap_or("");
    match basename {
        "zsh" => Some(".zshrc"),
        "bash" => Some(".bashrc"),
        _ => None,
    }
}

pub(crate) enum RcApplyOutcome {
    Applied,
    Unchanged,
    Refreshed,
    Conflict(String),
}

fn marker_block(pi_dir: &str) -> String {
    // Q7: append to PATH so an existing user-installed pi keeps precedence.
    format!("{MARKER_BEGIN}\nexport PATH=\"$PATH:{pi_dir}\"\n{MARKER_END}\n")
}

/// Pure rc-text transform behind the toggle: append a managed marker block,
/// refresh it when the embedded path moved, refuse to touch user edits.
pub(crate) fn apply_to_rc_text(text: &str, pi_dir: &str) -> (RcApplyOutcome, String) {
    let expected = marker_block(pi_dir);
    if let (Some(begin), Some(end)) = (text.find(MARKER_BEGIN), text.find(MARKER_END)) {
        if begin < end {
            let block = &text[begin..end + MARKER_END.len()];
            if block == expected.trim_end() {
                return (RcApplyOutcome::Unchanged, text.to_string());
            }
            let inner: Vec<&str> = block
                .lines()
                .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
                .collect();
            // A lone managed export line with a different path is a stale
            // marker (Q8 self-heal); anything else belongs to the user.
            if inner.len() == 1
                && inner[0].starts_with("export PATH=\"$PATH:")
                && inner[0].ends_with('"')
            {
                let refreshed = format!(
                    "{}{}{}",
                    &text[..begin],
                    expected.trim_end(),
                    &text[end + MARKER_END.len()..]
                );
                return (RcApplyOutcome::Refreshed, refreshed);
            }
            return (
                RcApplyOutcome::Conflict(
                    "the marker block in your rc file was edited; expected one managed export line"
                        .to_string(),
                ),
                text.to_string(),
            );
        }
    }
    let mut next = text.to_string();
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&expected);
    (RcApplyOutcome::Applied, next)
}

/// Remove only the managed marker block (Q2); leave everything else intact.
pub(crate) fn remove_from_rc_text(text: &str) -> (bool, String) {
    if let (Some(begin), Some(end)) = (text.find(MARKER_BEGIN), text.find(MARKER_END)) {
        if begin < end {
            let head = &text[..begin];
            let tail = &text[end + MARKER_END.len()..];
            // The block carries its own trailing newline; when the head also
            // ends with one, drop the duplicate at the join.
            let tail = if head.ends_with('\n') && tail.starts_with('\n') {
                &tail[1..]
            } else {
                tail
            };
            return (true, format!("{head}{tail}"));
        }
    }
    (false, text.to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn apply_posix(home: &std::path::Path, shell: &str, pi_dir: &str) -> Result<String, String> {
    let rc_name =
        rc_filename_for_shell(shell).ok_or_else(|| format!("unsupported shell: {shell}"))?;
    let rc_path = home.join(rc_name);
    let text = std::fs::read_to_string(&rc_path).unwrap_or_default();
    let (outcome, next) = apply_to_rc_text(&text, pi_dir);
    match outcome {
        RcApplyOutcome::Conflict(message) => Err(format!("{rc_name}: {message}")),
        RcApplyOutcome::Unchanged => Ok(format!("{rc_name}: already configured")),
        RcApplyOutcome::Applied | RcApplyOutcome::Refreshed => {
            std::fs::write(&rc_path, next).map_err(|e| format!("{rc_name}: {e}"))?;
            Ok(format!("{rc_name}: updated"))
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn remove_posix(home: &std::path::Path, shell: &str) -> Result<String, String> {
    let rc_name =
        rc_filename_for_shell(shell).ok_or_else(|| format!("unsupported shell: {shell}"))?;
    let rc_path = home.join(rc_name);
    let text = std::fs::read_to_string(&rc_path).unwrap_or_default();
    let (removed, next) = remove_from_rc_text(&text);
    if !removed {
        return Ok(format!("{rc_name}: nothing to remove"));
    }
    std::fs::write(&rc_path, next).map_err(|e| format!("{rc_name}: {e}"))?;
    Ok(format!("{rc_name}: removed"))
}

/// Directory containing the embedded `pi` binary; the PATH entry itself.
pub fn bundled_pi_dir(static_dir: &std::path::Path) -> Result<PathBuf, String> {
    crate::pi_launch::resolve_bundled_pi(static_dir)?
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "embedded pi has no parent directory".to_string())
}
// ABOUTME: System-wide PATH entry for the embedded Pi binary, toggled from
// ABOUTME: Settings → General: marker-managed rc lines on POSIX, user Path
// ABOUTME: registry value on Windows.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_maps_to_single_rc_file() {
        assert_eq!(rc_filename_for_shell("/bin/zsh"), Some(".zshrc"));
        assert_eq!(rc_filename_for_shell("/bin/bash"), Some(".bashrc"));
        assert_eq!(rc_filename_for_shell("/usr/local/bin/fish"), None);
        assert_eq!(rc_filename_for_shell("/bin/sh"), None);
        assert_eq!(rc_filename_for_shell(""), None);
    }

    #[test]
    fn apply_appends_marker_block_to_existing_rc() {
        let (outcome, text) = apply_to_rc_text("export EDITOR=vim\n", "/opt/pi");
        assert!(matches!(outcome, RcApplyOutcome::Applied));
        assert!(text.starts_with("export EDITOR=vim\n"));
        assert!(text.contains("# >>> picot embedded pi >>>"));
        // Q7: append, never shadow an existing pi install.
        assert!(text.contains("export PATH=\"$PATH:/opt/pi\""));
        assert!(text.ends_with("# <<< picot embedded pi <<<\n"));
    }

    #[test]
    fn apply_creates_rc_from_empty_when_missing() {
        let (outcome, text) = apply_to_rc_text("", "/opt/pi");
        assert!(matches!(outcome, RcApplyOutcome::Applied));
        assert!(text.trim_start().starts_with("# >>> picot embedded pi >>>"));
    }

    #[test]
    fn apply_with_current_path_is_unchanged() {
        let (_, once) = apply_to_rc_text("", "/opt/pi");
        let (outcome, again) = apply_to_rc_text(&once, "/opt/pi");
        assert!(matches!(outcome, RcApplyOutcome::Unchanged));
        assert_eq!(once, again);
    }

    #[test]
    fn apply_refreshes_a_stale_marker_path() {
        let (_, stale) = apply_to_rc_text("", "/old/location");
        let (outcome, text) = apply_to_rc_text(&stale, "/new/location");
        assert!(matches!(outcome, RcApplyOutcome::Refreshed));
        assert!(text.contains("export PATH=\"$PATH:/new/location\""));
        assert!(!text.contains("/old/location"));
        // Exactly one marker block after the refresh.
        assert_eq!(text.matches("# >>> picot embedded pi >>>").count(), 1);
    }

    #[test]
    fn apply_reports_conflict_when_user_edited_the_block() {
        let (_, base) = apply_to_rc_text("", "/opt/pi");
        let edited = base.replace(
            "export PATH=\"$PATH:/opt/pi\"",
            "export PATH=\"/opt/pi:$PATH\"\nexport OTHER=1",
        );
        let (outcome, text) = apply_to_rc_text(&edited, "/opt/pi");
        assert!(matches!(outcome, RcApplyOutcome::Conflict(_)));
        assert_eq!(text, edited, "conflict must not rewrite the file");
    }

    #[test]
    fn remove_strips_only_the_marker_block() {
        let (_, with_block) = apply_to_rc_text("export EDITOR=vim\n", "/opt/pi");
        let (removed, text) = remove_from_rc_text(&with_block);
        assert!(removed);
        assert_eq!(text, "export EDITOR=vim\n");
    }

    #[test]
    fn remove_without_block_is_a_noop() {
        let (removed, text) = remove_from_rc_text("export EDITOR=vim\n");
        assert!(!removed);
        assert_eq!(text, "export EDITOR=vim\n");
    }
}

// ── Windows: user Path registry value (Q4) ──────────────────────────────
// No setx (1024-char truncation); HKCU\Environment via the registry API,
// then WM_SETTINGCHANGE so new processes pick the value up. Existing
// processes keep their environment — the UI copy says "new terminals only".

#[cfg(target_os = "windows")]
mod windows_impl {
    const ENVIRONMENT: &str = "Environment";
    const PATH_VALUE: &str = "Path";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read_user_path() -> Result<(Vec<u16>, u32), String> {
        use windows_sys::Win32::System::Registry as reg;
        #[allow(non_snake_case)]
        let HKEY_CURRENT_USER = unsafe { reg::HKEY_CURRENT_USER };
        unsafe {
            let mut key = reg::HKEY::default();
            if reg::RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(ENVIRONMENT).as_ptr(),
                0,
                reg::KEY_QUERY_VALUE,
                &mut key,
            ) != 0
            {
                return Ok((Vec::new(), reg::REG_EXPAND_SZ));
            }
            let mut kind = 0u32;
            let mut len = 0u32;
            if reg::RegQueryValueExW(
                key,
                wide(PATH_VALUE).as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                std::ptr::null_mut(),
                &mut len,
            ) != 0
            {
                reg::RegCloseKey(key);
                return Ok((Vec::new(), reg::REG_EXPAND_SZ));
            }
            let mut buffer = vec![0u8; len as usize];
            let result = reg::RegQueryValueExW(
                key,
                wide(PATH_VALUE).as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                buffer.as_mut_ptr(),
                &mut len,
            );
            reg::RegCloseKey(key);
            if result != 0 {
                return Err("failed to read the user Path value".into());
            }
            buffer.truncate(len as usize);
            let units: Vec<u16> = buffer
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|&unit| unit != 0)
                .collect();
            Ok((units, kind))
        }
    }

    fn write_user_path(units: &[u16], kind: u32) -> Result<(), String> {
        use windows_sys::Win32::System::Registry as reg;
        #[allow(non_snake_case)]
        let HKEY_CURRENT_USER = unsafe { reg::HKEY_CURRENT_USER };
        unsafe {
            let mut key = reg::HKEY::default();
            if reg::RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(ENVIRONMENT).as_ptr(),
                0,
                std::ptr::null_mut(),
                0,
                reg::KEY_SET_VALUE,
                std::ptr::null_mut(),
                &mut key,
                std::ptr::null_mut(),
            ) != 0
            {
                return Err("failed to open the user Environment key".into());
            }
            let bytes: Vec<u8> = units
                .iter()
                .chain(std::iter::once(&0))
                .flat_map(|&unit| unit.to_le_bytes())
                .collect();
            let result = reg::RegSetValueExW(
                key,
                wide(PATH_VALUE).as_ptr(),
                0,
                kind,
                bytes.as_ptr(),
                bytes.len() as u32,
            );
            reg::RegCloseKey(key);
            if result != 0 {
                return Err("failed to write the user Path value".into());
            }
            broadcast_environment_change();
            Ok(())
        }
    }

    fn broadcast_environment_change() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        unsafe {
            // Best-effort: a lost broadcast only delays pickup until the next
            // process reads the registry; never fail the write for it.
            let sent = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                wide("Environment").as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                std::ptr::null_mut(),
            );
            if sent == 0 {
                log::warn!("WM_SETTINGCHANGE broadcast was not delivered");
            }
        }
    }

    fn decode(units: &[u16]) -> String {
        String::from_utf16_lossy(units)
    }

    fn split_entries(value: &str) -> Vec<&str> {
        value.split(';').filter(|entry| !entry.is_empty()).collect()
    }

    /// Q7 append: an existing user-installed pi keeps precedence; matching is
    /// case-insensitive so a stale entry whose case differs still refreshes.
    pub(super) fn apply(pi_dir: &str) -> Result<String, String> {
        let (units, kind) = read_user_path()?;
        let current = decode(&units);
        let entries = split_entries(&current);
        let managed: Vec<&str> = entries
            .iter()
            .copied()
            .filter(|entry| entry.eq_ignore_ascii_case(pi_dir))
            .collect();
        if managed.is_empty() {
            let next = format!("{current};{pi_dir}");
            write_user_path(&wide(&next)[..next.len()], kind)?;
            return Ok("user Path: updated".into());
        }
        Ok("user Path: already configured".into())
    }

    pub(super) fn remove(pi_dir: &str) -> Result<String, String> {
        let (units, kind) = read_user_path()?;
        let current = decode(&units);
        if !split_entries(&current)
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(pi_dir))
        {
            return Ok("user Path: nothing to remove".into());
        }
        let next = split_entries(&current)
            .into_iter()
            .filter(|entry| !entry.eq_ignore_ascii_case(pi_dir))
            .collect::<Vec<_>>()
            .join(";");
        write_user_path(&wide(&next)[..next.len()], kind)?;
        Ok("user Path: removed".into())
    }
}
#[cfg(target_os = "windows")]
pub(crate) fn apply_windows(pi_dir: &str) -> Result<String, String> {
    windows_impl::apply(pi_dir)
}

#[cfg(target_os = "windows")]
pub(crate) fn remove_windows(pi_dir: &str) -> Result<String, String> {
    windows_impl::remove(pi_dir)
}
