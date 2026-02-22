use std::path::Path;

use tokio::process::Command;

/// Configure a Command for safe sandboxed execution during discovery.
/// - Uses a temp directory as CWD (prevents file writes to user's workspace)
/// - Nulls stdin (prevents interactive prompts)
/// - Sanitizes environment to prevent GUI launches and credential prompts
pub fn sandbox_command(cmd: &mut Command, scratch_dir: &Path) {
    cmd.current_dir(scratch_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    cmd.env("__CF_USER_TEXT_ENCODING", "")
        .env("DISPLAY", "")
        .env("SSH_ASKPASS", "")
        .env("SUDO_ASKPASS", "")
        .env("GIT_ASKPASS", "")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env("CI", "1");
}

pub(super) fn is_safe_command_name(command: &str) -> bool {
    if command.len() <= 1 {
        return false;
    }

    const RISKY_NAMES: &[&str] = &[
        "completion",
        "completions",
        "generate",
        "install",
        "setup",
        "configure",
        "init",
        "bootstrap",
        "deploy",
        "migrate",
        "update",
        "upgrade",
        "uninstall",
        "remove",
        "clean",
        "purge",
        "reset",
        "destroy",
    ];

    if RISKY_NAMES.contains(&command) {
        return false;
    }

    !command.starts_with('_')
}
