use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;

use crate::spec::{CommandSpec, SpecSource};

use super::help_parser::parse_help_basic;
use super::sandbox::{is_safe_command_name, sandbox_command};
use super::SpecStore;

/// Commands that must never be run with --help for safety reasons.
const DISCOVERY_BLOCKLIST: &[&str] = &[
    "rm",
    "dd",
    "mkfs",
    "fdisk",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "format",
    "diskutil",
    "sudo",
    "su",
    "doas",
    "login",
    "passwd",
    "kinit",
    "security",
    "open",
    "osascript",
    "say",
    "afplay",
    "screencapture",
    "pmset",
    "caffeinate",
    "networksetup",
    "systemsetup",
    "launchctl",
    "defaults",
    "softwareupdate",
    "xcode-select",
    "xcodebuild",
    "instruments",
    "installer",
    "hdiutil",
    "codesign",
    "spctl",
    "ssh",
    "scp",
    "sftp",
    "ssh-agent",
    "ssh-add",
    "telnet",
    "ftp",
    "apt",
    "apt-get",
    "dpkg",
    "yum",
    "dnf",
    "pacman",
    "snap",
    "flatpak",
    "port",
    "mysql",
    "psql",
    "mongo",
    "redis-cli",
    "sqlite3",
];

/// Maximum bytes to read from --help stdout.
const MAX_HELP_OUTPUT_BYTES: usize = 256 * 1024;

impl SpecStore {
    pub fn can_discover_command(&self, command: &str) -> bool {
        self.config.discover_from_help
            && !DISCOVERY_BLOCKLIST.contains(&command)
            && !self
                .config
                .discover_blocklist
                .iter()
                .any(|blocked| blocked == command)
            && is_safe_command_name(command)
    }

    async fn discover_with_generator(&self, command: &str) -> Option<CommandSpec> {
        let timeout = Duration::from_millis(crate::config::DISCOVER_TIMEOUT_MS);
        let mut spec = crate::zsh_completion::try_completion_generator(command, timeout).await?;
        spec.source = SpecSource::Discovered;
        Some(spec)
    }

    async fn discover_with_help(&self, command: &str) -> Option<CommandSpec> {
        let timeout = Duration::from_millis(crate::config::DISCOVER_TIMEOUT_MS);
        let args: Vec<String> = Vec::new();
        let help_text = self.fetch_help_output(command, &args, timeout).await?;

        let mut spec = parse_help_basic(command, &help_text);
        spec.source = SpecSource::Discovered;
        (!spec.subcommands.is_empty() || !spec.options.is_empty()).then_some(spec)
    }

    /// Run discovery for a command and return the spec + compsys file path.
    /// Tries completion generators first (structured), then `--help` regex.
    pub async fn discover_command(&self, command: &str) -> Option<(CommandSpec, PathBuf)> {
        if !self.can_discover_command(command) {
            return None;
        }

        if let Some(spec) = self.discover_with_generator(command).await {
            return self.write_discovered(command, spec);
        }

        let spec = self.discover_with_help(command).await?;
        self.write_discovered(command, spec)
    }

    fn write_discovered(&self, command: &str, spec: CommandSpec) -> Option<(CommandSpec, PathBuf)> {
        if self.zsh_index.contains(command) {
            return None;
        }

        let path =
            crate::compsys_export::write_completion_file(&spec, &self.completions_dir).ok()?;
        Some((spec, path))
    }

    async fn run_help_command(
        &self,
        command: &str,
        args: &[String],
        help_flag: &str,
        timeout: Duration,
    ) -> Option<String> {
        let result = tokio::time::timeout(timeout, async {
            let scratch = std::env::temp_dir().join("synapse-discovery");
            let _ = std::fs::create_dir_all(&scratch);
            let mut cmd = Command::new(command);
            cmd.args(args).arg(help_flag);
            sandbox_command(&mut cmd, &scratch);
            cmd.output().await
        })
        .await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                stdout.truncate(MAX_HELP_OUTPUT_BYTES);

                if stdout.trim().is_empty() {
                    let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    stderr.truncate(MAX_HELP_OUTPUT_BYTES);
                    let lower = stderr.to_lowercase();
                    if lower.contains("usage") || lower.contains("options") {
                        return Some(stderr);
                    }
                }

                if !stdout.trim().is_empty() {
                    Some(stdout)
                } else {
                    None
                }
            }
            Ok(Err(_)) => None,
            Err(_) => None,
        }
    }

    async fn fetch_help_output(
        &self,
        command: &str,
        args: &[String],
        timeout: Duration,
    ) -> Option<String> {
        for help_flag in ["--help", "-h"] {
            if let Some(text) = self
                .run_help_command(command, args, help_flag, timeout)
                .await
            {
                if !text.trim().is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }
}
