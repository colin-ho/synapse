use std::io::Write;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use assert_cmd::Command;

#[test]
fn test_cli_help() {
    Command::cargo_bin("synapse")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn test_daemon_status_no_daemon() {
    Command::cargo_bin("synapse")
        .unwrap()
        .args(["daemon", "status"])
        .assert()
        .success(); // Should not crash even with no daemon
}

#[tokio::test]
async fn test_daemon_lifecycle() {
    // Use a unique socket path for this test
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("test-synapse.sock");
    let pid_path = dir.path().join("test-synapse.pid");

    // Start daemon as a background task
    let sock = socket_path.clone();
    let pid = pid_path.clone();
    let daemon = tokio::spawn(async move {
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        std::fs::write(&pid, std::process::id().to_string()).unwrap();

        // Accept one connection for the test
        if let Ok((stream, _)) = listener.accept().await {
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            while reader.read_line(&mut line).await.unwrap() > 0 {
                let trimmed = line.trim();
                let response = if trimmed.contains("\"type\":\"ping\"") {
                    r#"{"type":"pong"}"#
                } else {
                    r#"{"type":"ack"}"#
                };
                writer.write_all(format!("{response}\n").as_bytes()).await.unwrap();
                writer.flush().await.unwrap();
                line.clear();
            }
        }
    });

    // Wait for socket to appear
    for _ in 0..50 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(socket_path.exists(), "Socket not created");

    // Connect and send ping
    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer.write_all(b"{\"type\":\"ping\"}\n").await.unwrap();
    writer.flush().await.unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).await.unwrap();
    assert!(response.contains("pong"), "Expected pong, got: {response}");

    // Send shutdown
    writer.write_all(b"{\"type\":\"shutdown\"}\n").await.unwrap();
    writer.flush().await.unwrap();

    let mut ack = String::new();
    reader.read_line(&mut ack).await.unwrap();
    assert!(ack.contains("ack"), "Expected ack, got: {ack}");

    // Cleanup
    drop(writer);
    daemon.abort();
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&pid_path);
}

#[test]
fn test_protocol_serialization() {
    // Test that ping request parses correctly
    let req: synapse::protocol::Request =
        serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
    assert!(matches!(req, synapse::protocol::Request::Ping));

    // Test suggest request
    let req: synapse::protocol::Request = serde_json::from_str(
        r#"{"type":"suggest","session_id":"abc","buffer":"git","cursor_pos":3,"cwd":"/tmp","last_exit_code":0,"recent_commands":[]}"#,
    ).unwrap();
    assert!(matches!(req, synapse::protocol::Request::Suggest(_)));

    // Test interaction report
    let req: synapse::protocol::Request = serde_json::from_str(
        r#"{"type":"interaction","session_id":"abc","action":"accept","suggestion":"git status","source":"history","buffer_at_action":"git"}"#,
    ).unwrap();
    assert!(matches!(req, synapse::protocol::Request::Interaction(_)));
}

#[test]
fn test_response_serialization() {
    let resp = synapse::protocol::Response::Pong;
    let json = serde_json::to_string(&resp).unwrap();
    assert_eq!(json, r#"{"type":"pong"}"#);

    let resp = synapse::protocol::Response::Suggestion(synapse::protocol::SuggestionResponse {
        text: "git status".into(),
        source: synapse::protocol::SuggestionSource::History,
        confidence: 0.92,
    });
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("suggestion"));
    assert!(json.contains("git status"));
    assert!(json.contains("history"));
}

#[test]
fn test_config_defaults() {
    let config = synapse::config::Config::default();
    assert_eq!(config.general.debounce_ms, 150);
    assert_eq!(config.general.max_suggestion_length, 200);
    assert!(config.history.enabled);
    assert_eq!(config.history.max_entries, 50000);
    assert!(config.context.enabled);
    assert_eq!(config.weights.history, 0.30);
    assert_eq!(config.weights.context, 0.15);
    assert_eq!(config.weights.ai, 0.25);
    assert_eq!(config.weights.spec, 0.15);
    assert_eq!(config.weights.recency, 0.15);
}

#[test]
fn test_weights_normalization() {
    let weights = synapse::config::WeightsConfig {
        history: 1.0,
        context: 1.0,
        ai: 1.0,
        spec: 1.0,
        recency: 1.0,
    };
    let normalized = weights.normalized();
    let sum = normalized.history + normalized.context + normalized.ai + normalized.spec + normalized.recency;
    assert!((sum - 1.0).abs() < 0.001);
    assert!((normalized.history - 0.2).abs() < 0.001);
}
