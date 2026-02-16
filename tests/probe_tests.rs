use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

fn synapse_bin() -> PathBuf {
    PathBuf::from(assert_cmd::cargo::cargo_bin!("synapse"))
}

async fn wait_for_socket(path: &Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("Socket not created at {}", path.display());
}

async fn run_probe_request(bin: &Path, socket_path: &Path, request: String) -> String {
    let output = Command::new(bin)
        .arg("probe")
        .arg("--socket-path")
        .arg(socket_path)
        .arg("--request")
        .arg(request)
        .output()
        .await
        .expect("Failed to run synapse probe");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        output.status.success(),
        "synapse probe failed\nstatus: {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status
    );

    stdout
}

async fn stop_child(child: &mut Child) {
    let _ = child.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

#[tokio::test]
async fn test_probe_end_to_end_history_roundtrip() {
    let bin = synapse_bin();
    let temp = tempfile::tempdir().unwrap();
    let socket_path = temp.path().join("probe.sock");
    let history_path = temp.path().join("history.zsh");
    std::fs::write(&history_path, "").unwrap();

    let config_home = temp.path().join("config");
    let config_dir = config_home.join("synapse");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[llm]
enabled = false
natural_language = false
workflow_prediction = false
contextual_args = false

[spec]
discover_from_help = false

[workflow]
enabled = false
"#,
    )
    .unwrap();

    let mut daemon = Command::new(&bin)
        .arg("start")
        .arg("--foreground")
        .arg("--socket-path")
        .arg(&socket_path)
        .env("HISTFILE", &history_path)
        .env("XDG_CONFIG_HOME", &config_home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .expect("Failed to start synapse daemon");

    wait_for_socket(&socket_path).await;

    let session_id = "probe-session";
    let cwd = temp.path().to_string_lossy().to_string();

    let command_executed = serde_json::json!({
        "type": "command_executed",
        "session_id": session_id,
        "command": "echo hello-agent",
    })
    .to_string();

    let ack = run_probe_request(&bin, &socket_path, command_executed).await;
    assert_eq!(ack.trim(), "ack");

    let suggest_request = serde_json::json!({
        "type": "suggest",
        "session_id": session_id,
        "buffer": "echo he",
        "cursor_pos": 7,
        "cwd": cwd,
        "last_exit_code": 0,
        "recent_commands": [],
    })
    .to_string();

    let suggestion = run_probe_request(&bin, &socket_path, suggest_request).await;
    assert!(
        suggestion.starts_with("suggest\t"),
        "Expected suggest frame, got: {suggestion}"
    );
    assert!(
        suggestion.contains("echo hello-agent"),
        "Expected history suggestion, got: {suggestion}"
    );

    stop_child(&mut daemon).await;
}
