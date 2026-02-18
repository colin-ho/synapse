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
async fn test_probe_end_to_end_ping_and_command_executed() {
    let bin = synapse_bin();
    let temp = tempfile::tempdir().unwrap();
    let socket_path = temp.path().join("probe.sock");

    let config_home = temp.path().join("config");
    let config_dir = config_home.join("synapse");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[llm]
enabled = false
natural_language = false

[spec]
discover_from_help = false
"#,
    )
    .unwrap();

    let mut daemon = Command::new(&bin)
        .arg("start")
        .arg("--foreground")
        .arg("--socket-path")
        .arg(&socket_path)
        .env("XDG_CONFIG_HOME", &config_home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .expect("Failed to start synapse daemon");

    wait_for_socket(&socket_path).await;

    // Ping
    let ping = serde_json::json!({"type": "ping"}).to_string();
    let pong = run_probe_request(&bin, &socket_path, ping).await;
    assert_eq!(pong.trim(), "pong");

    // Command executed
    let command_executed = serde_json::json!({
        "type": "command_executed",
        "session_id": "probe-session",
        "command": "echo hello-agent",
        "cwd": temp.path().to_string_lossy(),
    })
    .to_string();

    let ack = run_probe_request(&bin, &socket_path, command_executed).await;
    assert_eq!(ack.trim(), "ack");

    // Complete request (should return empty since no specs match "echo")
    let complete = serde_json::json!({
        "type": "complete",
        "command": "echo",
        "context": [],
        "cwd": temp.path().to_string_lossy(),
    })
    .to_string();

    let result = run_probe_request(&bin, &socket_path, complete).await;
    assert!(
        result.trim().starts_with("complete_result\t"),
        "Expected complete_result frame, got: {result}"
    );

    stop_child(&mut daemon).await;
}
