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

#[tokio::test]
async fn test_probe_run_generator_returns_complete_result() {
    let bin = synapse_bin();
    let temp = tempfile::tempdir().unwrap();
    let socket_path = temp.path().join("run-gen.sock");

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

    // Send a run_generator request with a simple echo command
    let run_gen = serde_json::json!({
        "type": "run_generator",
        "command": "echo -e 'alpha\nbeta\ngamma'",
        "cwd": temp.path().to_string_lossy(),
    })
    .to_string();

    let result = run_probe_request(&bin, &socket_path, run_gen).await;
    let trimmed = result.trim();

    // Response must be a complete_result TSV frame
    assert!(
        trimmed.starts_with("complete_result\t"),
        "Expected complete_result frame, got: {trimmed}"
    );

    let fields: Vec<&str> = trimmed.split('\t').collect();
    let count: usize = fields.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

    assert!(
        count >= 1,
        "Expected at least 1 generator result, got count={count} in: {trimmed}"
    );

    // Collect the values (every other field starting from index 2)
    let values: Vec<&str> = (0..count)
        .filter_map(|i| fields.get(2 + i * 2).copied())
        .filter(|v| !v.is_empty())
        .collect();

    assert!(
        !values.is_empty(),
        "Expected non-empty generator values, got: {trimmed}"
    );

    // Test with strip_prefix
    let run_gen_strip = serde_json::json!({
        "type": "run_generator",
        "command": "printf '* main\n  dev\n  feature'",
        "cwd": temp.path().to_string_lossy(),
        "strip_prefix": "* ",
    })
    .to_string();

    let result_strip = run_probe_request(&bin, &socket_path, run_gen_strip).await;
    let trimmed_strip = result_strip.trim();
    assert!(
        trimmed_strip.starts_with("complete_result\t"),
        "Expected complete_result frame for strip_prefix test, got: {trimmed_strip}"
    );

    // "* main" should become "main" after strip_prefix
    assert!(
        trimmed_strip.contains("main"),
        "Expected 'main' in results after strip_prefix, got: {trimmed_strip}"
    );

    stop_child(&mut daemon).await;
}
