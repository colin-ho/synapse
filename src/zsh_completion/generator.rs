use std::time::Duration;

use crate::spec::CommandSpec;

use super::parser::parse_zsh_completion;

/// Maximum bytes to read from a completion generator's output.
const MAX_GENERATOR_OUTPUT_BYTES: usize = 256 * 1024;

pub(super) async fn try_completion_generator(
    command: &str,
    timeout: Duration,
) -> Option<CommandSpec> {
    let patterns: &[&[&str]] = &[
        &["completion", "zsh"],
        &["completions", "zsh"],
        &["completion", "--shell", "zsh"],
        &["--completions", "zsh"],
    ];

    let scratch = std::env::temp_dir().join("synapse-discovery");
    let _ = std::fs::create_dir_all(&scratch);

    for args in patterns {
        let result = tokio::time::timeout(timeout, async {
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(*args);
            crate::spec_store::sandbox_command(&mut cmd, &scratch);
            cmd.stderr(std::process::Stdio::null());
            cmd.output().await
        })
        .await;

        let output = match result {
            Ok(Ok(output)) if output.status.success() => output,
            _ => continue,
        };

        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        stdout.truncate(MAX_GENERATOR_OUTPUT_BYTES);

        if !stdout.contains("_arguments") && !stdout.contains("#compdef") {
            continue;
        }

        let spec = parse_zsh_completion(command, &stdout);
        if !spec.options.is_empty() || !spec.subcommands.is_empty() {
            tracing::info!(
                "Completion generator succeeded for {command} with args {:?}",
                args
            );
            return Some(spec);
        }
    }

    None
}
