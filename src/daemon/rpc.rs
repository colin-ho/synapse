use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub(super) async fn request_tsv_json(
    socket_path: &Path,
    request: &serde_json::Value,
    timeout: Duration,
) -> anyhow::Result<String> {
    let payload = serde_json::to_string(request)?;
    request_tsv_raw(socket_path, &payload, timeout).await
}

pub(super) async fn request_tsv_raw(
    socket_path: &Path,
    request: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("Failed to connect to daemon at {}", socket_path.display()))?;

    let mut request_line = request.to_string();
    request_line.push('\n');
    stream.write_all(request_line.as_bytes()).await?;

    let (reader, _) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    match tokio::time::timeout(timeout, reader.read_line(&mut line)).await {
        Ok(Ok(n)) if n > 0 => Ok(line.trim().to_string()),
        Ok(Ok(_)) => Err(anyhow!("Daemon closed the connection before responding")),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(anyhow!("Timed out waiting for daemon response")),
    }
}
