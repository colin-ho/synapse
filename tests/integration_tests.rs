use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use assert_cmd::Command;
use synapse::protocol::Request;

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
        .args(["status"])
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
                    "pong"
                } else {
                    "ack"
                };
                writer
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
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
    writer
        .write_all(b"{\"type\":\"shutdown\"}\n")
        .await
        .unwrap();
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
fn test_cli_socket_path_flag() {
    Command::cargo_bin("synapse")
        .unwrap()
        .args(["status", "--socket-path", "/tmp/nonexistent.sock"])
        .assert()
        .success();
}

/// Helper: spawn a mock daemon that accepts multiple connections and responds to
/// ping with pong and suggest with a canned suggestion. Returns a JoinHandle.
fn spawn_mock_daemon(
    socket_path: std::path::PathBuf,
    pid_path: std::path::PathBuf,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();

        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            // Handle each connection in its own task
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader);
                let mut line = String::new();

                while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    let trimmed = line.trim();
                    let response = if trimmed.contains("\"type\":\"ping\"") {
                        "pong".to_string()
                    } else if trimmed.contains("\"type\":\"suggest\"") {
                        "suggest\tgit status\thistory".to_string()
                    } else {
                        "ack".to_string()
                    };
                    let _ = writer.write_all(format!("{response}\n").as_bytes()).await;
                    let _ = writer.flush().await;
                    line.clear();
                }
            });
        }
    })
}

/// Wait for a socket file to appear on disk.
async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..50 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("Socket not created at {}", path.display());
}

/// Send a request and read one response line.
async fn roundtrip(
    reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    request: &str,
) -> String {
    writer
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    writer.flush().await.unwrap();
    reader
        .next_line()
        .await
        .unwrap()
        .expect("expected a response line")
}

#[tokio::test]
async fn test_client_reconnect_same_daemon() {
    // Verify a client can disconnect and reconnect to the same running daemon.
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("reconnect.sock");
    let pid_path = dir.path().join("reconnect.pid");

    let daemon = spawn_mock_daemon(socket_path.clone(), pid_path.clone());
    wait_for_socket(&socket_path).await;

    // First connection — ping and drop
    {
        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        let resp = roundtrip(&mut lines, &mut writer, r#"{"type":"ping"}"#).await;
        assert!(
            resp.contains("pong"),
            "First connection ping failed: {resp}"
        );
    }
    // Stream is dropped here — server sees EOF on this connection

    // Brief pause to let the server process the disconnect
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Second connection — should work without issues
    {
        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        let resp = roundtrip(&mut lines, &mut writer, r#"{"type":"ping"}"#).await;
        assert!(
            resp.contains("pong"),
            "Second connection ping failed: {resp}"
        );

        // Also verify suggest works on the new connection
        let resp = roundtrip(
            &mut lines,
            &mut writer,
            r#"{"type":"suggest","session_id":"abc123","buffer":"git","cursor_pos":3,"cwd":"/tmp","last_exit_code":0,"recent_commands":[]}"#,
        ).await;
        assert!(
            resp.contains("git status"),
            "Suggest after reconnect failed: {resp}"
        );
    }

    daemon.abort();
}

#[tokio::test]
async fn test_client_reconnect_after_daemon_restart() {
    // Simulate: daemon dies, socket is removed, new daemon binds the same path.
    // A client that connects to the new daemon should work normally.
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("restart.sock");
    let pid_path = dir.path().join("restart.pid");

    // Start first daemon
    let daemon1 = spawn_mock_daemon(socket_path.clone(), pid_path.clone());
    wait_for_socket(&socket_path).await;

    // Connect and verify it works
    {
        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        let resp = roundtrip(&mut lines, &mut writer, r#"{"type":"ping"}"#).await;
        assert!(resp.contains("pong"), "Daemon 1 ping failed: {resp}");
    }

    // Kill the first daemon and remove the socket (mimics real daemon shutdown)
    daemon1.abort();
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&pid_path);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Verify connection fails while daemon is down
    assert!(
        UnixStream::connect(&socket_path).await.is_err(),
        "Should not connect to dead daemon"
    );

    // Start second daemon on the same socket path
    let daemon2 = spawn_mock_daemon(socket_path.clone(), pid_path.clone());
    wait_for_socket(&socket_path).await;

    // Connect to the new daemon — should work
    {
        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        let resp = roundtrip(&mut lines, &mut writer, r#"{"type":"ping"}"#).await;
        assert!(resp.contains("pong"), "Daemon 2 ping failed: {resp}");

        let resp = roundtrip(
            &mut lines,
            &mut writer,
            r#"{"type":"suggest","session_id":"abc123","buffer":"git","cursor_pos":3,"cwd":"/tmp","last_exit_code":0,"recent_commands":[]}"#,
        ).await;
        assert!(
            resp.contains("git status"),
            "Suggest on new daemon failed: {resp}"
        );
    }

    daemon2.abort();
}

#[tokio::test]
async fn test_read_detects_server_close() {
    // When the server side of a connection closes, the client should get EOF.
    // This is the mechanism the Zsh plugin relies on to detect disconnection.
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("eof.sock");

    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();

    // Connect from the client side
    let client = UnixStream::connect(&socket_path).await.unwrap();
    let (reader, mut writer) = client.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Accept on the server side, exchange a message, then drop the server stream
    let (server_stream, _) = listener.accept().await.unwrap();
    let (server_reader, mut server_writer) = server_stream.into_split();
    let mut server_lines = BufReader::new(server_reader).lines();

    // Client sends ping
    writer.write_all(b"{\"type\":\"ping\"}\n").await.unwrap();
    writer.flush().await.unwrap();

    // Server reads and responds
    let req = server_lines.next_line().await.unwrap().unwrap();
    assert!(req.contains("ping"));
    server_writer.write_all(b"pong\n").await.unwrap();
    server_writer.flush().await.unwrap();

    // Client reads the response
    let resp = lines.next_line().await.unwrap().unwrap();
    assert!(resp.contains("pong"));

    // Drop the server side — simulates daemon process exiting
    drop(server_writer);
    drop(server_lines);

    // Client should now get EOF
    let result =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line()).await;

    match result {
        Ok(Ok(None)) => {} // EOF — expected
        Ok(Err(_)) => {}   // IO error — also acceptable
        Ok(Ok(Some(line))) => panic!("Expected EOF, got data: {line}"),
        Err(_) => panic!("Read hung instead of returning EOF on macOS"),
    }
}

#[tokio::test]
async fn test_concurrent_connections() {
    // Multiple clients connected simultaneously should each work independently.
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("concurrent.sock");
    let pid_path = dir.path().join("concurrent.pid");

    let daemon = spawn_mock_daemon(socket_path.clone(), pid_path.clone());
    wait_for_socket(&socket_path).await;

    // Open 3 connections simultaneously
    let mut handles = Vec::new();
    for i in 0..3 {
        let path = socket_path.clone();
        handles.push(tokio::spawn(async move {
            let stream = UnixStream::connect(&path).await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            let resp = roundtrip(&mut lines, &mut writer, r#"{"type":"ping"}"#).await;
            assert!(resp.contains("pong"), "Connection {i} ping failed: {resp}");

            let resp = roundtrip(
                &mut lines,
                &mut writer,
                &format!(
                    r#"{{"type":"suggest","session_id":"sess{i}","buffer":"git","cursor_pos":3,"cwd":"/tmp","last_exit_code":0,"recent_commands":[]}}"#
                ),
            ).await;
            assert!(
                resp.contains("git status"),
                "Connection {i} suggest failed: {resp}"
            );
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    daemon.abort();
}

#[test]
fn test_natural_language_request_parsing() {
    let json = r#"{
        "type": "natural_language",
        "session_id": "abc123",
        "query": "find files bigger than 100mb",
        "cwd": "/home/user",
        "recent_commands": ["ls", "cd /tmp"],
        "env_hints": {"PATH": "/usr/bin:/usr/local/bin"}
    }"#;

    let req: Request = serde_json::from_str(json).unwrap();
    match req {
        Request::NaturalLanguage(nl) => {
            assert_eq!(nl.session_id, "abc123");
            assert_eq!(nl.query, "find files bigger than 100mb");
            assert_eq!(nl.cwd, "/home/user");
            assert_eq!(nl.recent_commands, vec!["ls", "cd /tmp"]);
            assert_eq!(nl.env_hints.get("PATH").unwrap(), "/usr/bin:/usr/local/bin");
        }
        other => panic!("Expected NaturalLanguage request, got: {other:?}"),
    }
}
