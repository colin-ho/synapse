use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;

pub(super) async fn run_generator(
    command: String,
    cwd: Option<PathBuf>,
    strip_prefix: Option<String>,
    split_on: Option<String>,
) -> anyhow::Result<()> {
    let cwd = cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    let split_on = split_on.unwrap_or_else(|| "\n".to_string());
    let timeout = Duration::from_millis(crate::config::GENERATOR_TIMEOUT_MS);

    let output = match tokio::time::timeout(timeout, async {
        Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&cwd)
            .output()
            .await
    })
    .await
    {
        Ok(Ok(output)) if output.status.success() => output,
        Ok(Ok(_)) => return Ok(()),
        Ok(Err(_)) => return Ok(()),
        Err(_) => return Ok(()),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for item in stdout.split(split_on.as_str()) {
        let mut item = item.trim().to_string();
        if item.is_empty() {
            continue;
        }
        if let Some(prefix) = &strip_prefix {
            if let Some(stripped) = item.strip_prefix(prefix.as_str()) {
                item = stripped.to_string();
            }
        }
        if !item.is_empty() {
            println!("{item}");
        }
    }

    Ok(())
}
