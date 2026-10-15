// ABOUTME: Resolves fixed, server-defined shell profiles into an executable and
// ABOUTME: argument array. macOS uses the user's shell with a safe system-shell
// ABOUTME: fallback; Windows defaults to Git Bash and never silently substitutes PowerShell.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Server-defined shell profile identifiers. The WebView may only name one of
/// these; there is no arbitrary-executable profile, so user text is never
/// interpolated into a command line.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ShellProfileId {
    Default,
    GitBash,
    PowerShell,
    CommandPrompt,
}

impl ShellProfileId {
    /// Stable wire identifier persisted in terminal-state.json and carried in
    /// the broker protocol.
    pub fn as_id_str(&self) -> &'static str {
        match self {
            ShellProfileId::Default => "default",
            ShellProfileId::GitBash => "git-bash",
            ShellProfileId::PowerShell => "powershell",
            ShellProfileId::CommandPrompt => "command-prompt",
        }
    }

    pub fn from_id_str(s: &str) -> Option<ShellProfileId> {
        Some(match s {
            "default" => ShellProfileId::Default,
            "git-bash" => ShellProfileId::GitBash,
            "powershell" => ShellProfileId::PowerShell,
            "command-prompt" => ShellProfileId::CommandPrompt,
            _ => return None,
        })
    }
}

/// A resolved shell: an executable plus a fixed argument array. Never a
/// shell-concatenated command string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedShell {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    /// The requested profile is not available on this host. `guidance` is safe
    /// to show the user (e.g. Git for Windows installation instructions).
    ProfileUnavailable {
        profile: ShellProfileId,
        guidance: String,
    },
}

/// Abstracts filesystem probes so profile resolution is unit-testable without a
/// real shell installation and without coupling to a specific host layout.
pub trait ShellProbe: Send + Sync {
    /// True when `path` exists, is a regular file, and is executable for the
    /// current process.
    fn is_valid_executable(&self, path: &Path) -> bool;

    /// Windows-only Git Bash discovery hook. Returns the Git for Windows
    /// installation root (the directory containing `bin/bash.exe`) when
    /// discoverable, or `None`. The default implementation probes a small set
    /// of standard install paths; the Windows build may extend this with a
    /// registry lookup once a compatible registry crate is added.
    fn discover_git_bash_root(&self) -> Option<PathBuf> {
        None
    }
}

/// Production filesystem probe backed by `std::fs`.
pub struct SystemShellProbe;

impl ShellProbe for SystemShellProbe {
    fn is_valid_executable(&self, path: &Path) -> bool {
        is_executable_real(path)
    }

    fn discover_git_bash_root(&self) -> Option<PathBuf> {
        standard_git_bash_roots()
            .into_iter()
            .find(|root| is_executable_real(&root.join("bin").join("bash.exe")))
    }
}

#[cfg(unix)]
fn is_executable_real(path: &Path) -> bool {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !metadata.is_file() {
        return false;
    }
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_real(path: &Path) -> bool {
    use std::fs;
    fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// Standard Git for Windows installation roots, probed in order. The full
/// detection also consults the registry and a discovered `git.exe`; those paths
/// require a Windows registry crate and are added alongside the Windows build.
fn standard_git_bash_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(prog_files) = std::env::var_os("ProgramFiles") {
        roots.push(PathBuf::from(&prog_files).join("Git"));
    }
    if let Some(prog_files_x86) = std::env::var_os("ProgramFiles(x86)") {
        roots.push(PathBuf::from(&prog_files_x86).join("Git"));
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(&local_app_data).join("Programs").join("Git"));
    }
    roots
}

/// Resolve the macOS default profile: the user's preferred `$SHELL` when it is
/// a valid executable, otherwise `/bin/zsh`, otherwise `/bin/bash`. The shell
/// starts interactively; no user text is interpolated.
pub fn resolve_macos_default(
    preferred_shell: &str,
    probe: &dyn ShellProbe,
) -> Result<ResolvedShell, ProfileError> {
    let candidates = [
        PathBuf::from(preferred_shell),
        PathBuf::from("/bin/zsh"),
        PathBuf::from("/bin/bash"),
    ];
    for program in candidates {
        if probe.is_valid_executable(&program) {
            return Ok(interactive_posix_shell(program));
        }
    }
    Err(ProfileError::ProfileUnavailable {
        profile: ShellProfileId::Default,
        guidance: "No usable interactive shell was found on this Mac.".to_string(),
    })
}

fn interactive_posix_shell(program: PathBuf) -> ResolvedShell {
    ResolvedShell {
        program,
        args: vec!["-i".to_string()],
    }
}

/// Resolve a Windows shell profile. `Default` is treated as Git Bash. Git Bash
/// launches `bin/bash.exe --login -i` (never `git-bash.exe`, which opens a
/// separate MinTTY window). PowerShell and Command Prompt are selectable
/// alternatives. Missing Git Bash yields a visible failure with installation
/// guidance rather than a silent PowerShell fallback.
pub fn resolve_windows_profile(
    profile: ShellProfileId,
    probe: &dyn ShellProbe,
) -> Result<ResolvedShell, ProfileError> {
    match profile {
        ShellProfileId::Default | ShellProfileId::GitBash => resolve_git_bash(probe),
        ShellProfileId::PowerShell => resolve_powershell(probe),
        ShellProfileId::CommandPrompt => resolve_command_prompt(probe),
    }
}

fn resolve_git_bash(probe: &dyn ShellProbe) -> Result<ResolvedShell, ProfileError> {
    let root = probe
        .discover_git_bash_root()
        .ok_or_else(|| ProfileError::ProfileUnavailable {
            profile: ShellProfileId::GitBash,
            guidance: "Git for Windows was not found. Install Git for Windows \
                       (https://git-scm.com/download/win) to use the default terminal, \
                       or choose PowerShell / Command Prompt."
                .to_string(),
        })?;
    Ok(ResolvedShell {
        program: root.join("bin").join("bash.exe"),
        args: vec!["--login".to_string(), "-i".to_string()],
    })
}

fn resolve_powershell(probe: &dyn ShellProbe) -> Result<ResolvedShell, ProfileError> {
    let candidates = [
        PathBuf::from("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
        PathBuf::from("C:\\Program Files\\PowerShell\\7\\pwsh.exe"),
    ];
    for program in candidates {
        if probe.is_valid_executable(&program) {
            return Ok(ResolvedShell {
                program,
                args: vec!["-NoLogo".to_string()],
            });
        }
    }
    Err(ProfileError::ProfileUnavailable {
        profile: ShellProfileId::PowerShell,
        guidance: "PowerShell was not found on this system.".to_string(),
    })
}

fn resolve_command_prompt(probe: &dyn ShellProbe) -> Result<ResolvedShell, ProfileError> {
    let program = PathBuf::from("C:\\Windows\\System32\\cmd.exe");
    if probe.is_valid_executable(&program) {
        return Ok(ResolvedShell {
            program,
            args: Vec::new(),
        });
    }
    Err(ProfileError::ProfileUnavailable {
        profile: ShellProfileId::CommandPrompt,
        guidance: "Command Prompt was not found on this system.".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Probe that treats an allow-list of paths as valid executables.
    struct FakeProbe {
        valid: Vec<PathBuf>,
        git_root: Option<PathBuf>,
    }

    impl ShellProbe for FakeProbe {
        fn is_valid_executable(&self, path: &Path) -> bool {
            self.valid.iter().any(|p| p == path)
        }
        fn discover_git_bash_root(&self) -> Option<PathBuf> {
            self.git_root.clone()
        }
    }

    fn fake_fs(valid: &[&str]) -> FakeProbe {
        FakeProbe {
            valid: valid.iter().map(PathBuf::from).collect(),
            git_root: None,
        }
    }

    #[test]
    fn invalid_macos_shell_falls_back_to_zsh() {
        let probe = fake_fs(&["/bin/zsh"]);
        let profile = resolve_macos_default("/not/executable", &probe).unwrap();
        assert_eq!(profile.program, PathBuf::from("/bin/zsh"));
        assert_eq!(profile.args, vec!["-i".to_string()]);
    }

    #[test]
    fn valid_preferred_macos_shell_is_used() {
        let probe = fake_fs(&["/usr/local/bin/fish"]);
        let profile = resolve_macos_default("/usr/local/bin/fish", &probe).unwrap();
        assert_eq!(profile.program, PathBuf::from("/usr/local/bin/fish"));
    }

    #[test]
    fn macos_falls_back_to_bash_when_zsh_missing() {
        let probe = fake_fs(&["/bin/bash"]);
        let profile = resolve_macos_default("/nope", &probe).unwrap();
        assert_eq!(profile.program, PathBuf::from("/bin/bash"));
    }

    #[test]
    fn macos_with_no_shell_errors_visibly() {
        let probe = fake_fs(&[]);
        let err = resolve_macos_default("/nope", &probe).unwrap_err();
        assert!(matches!(
            err,
            ProfileError::ProfileUnavailable {
                profile: ShellProfileId::Default,
                ..
            }
        ));
    }

    #[test]
    fn windows_git_bash_missing_is_visible_not_silent_powershell() {
        let probe = fake_fs(&[]);
        let err = resolve_windows_profile(ShellProfileId::Default, &probe).unwrap_err();
        assert!(matches!(
            err,
            ProfileError::ProfileUnavailable {
                profile: ShellProfileId::GitBash,
                ..
            }
        ));
        // Guidance mentions Git for Windows, not a silent PowerShell switch.
        match err {
            ProfileError::ProfileUnavailable { guidance, .. } => {
                assert!(guidance.contains("Git for Windows"));
            }
        }
    }

    #[test]
    fn windows_git_bash_present_launches_bin_bash_login_i() {
        let probe = FakeProbe {
            valid: vec![PathBuf::from("C:\\Program Files\\Git\\bin\\bash.exe")],
            git_root: Some(PathBuf::from("C:\\Program Files\\Git")),
        };
        let shell = resolve_windows_profile(ShellProfileId::Default, &probe).unwrap();
        let expected_program = PathBuf::from("C:\\Program Files\\Git")
            .join("bin")
            .join("bash.exe");
        assert_eq!(shell.program, expected_program);
        assert_eq!(shell.args, vec!["--login".to_string(), "-i".to_string()]);
    }

    #[test]
    fn profile_id_round_trips_through_wire_strings() {
        for id in [
            ShellProfileId::Default,
            ShellProfileId::GitBash,
            ShellProfileId::PowerShell,
            ShellProfileId::CommandPrompt,
        ] {
            assert_eq!(
                ShellProfileId::from_id_str(id.as_id_str()),
                Some(id),
                "round trip for {:?}",
                id
            );
        }
        assert_eq!(ShellProfileId::from_id_str("nonsense"), None);
    }
}
