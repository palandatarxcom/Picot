// ABOUTME: Suppresses console windows for subprocesses launched by the GUI host.
// ABOUTME: Provides one std and one Tokio entry point so new spawn sites share the Windows policy.

use std::process::Command as StdCommand;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Hide a child's console window on Windows; no-op on other platforms.
pub fn hide_console(command: &mut StdCommand) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// Tokio counterpart of [`hide_console`].
pub fn hide_console_tokio(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}
