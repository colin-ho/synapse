use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LinesCodec};

use crate::config::Config;

type ProbeWriter = SplitSink<Framed<UnixStream, LinesCodec>, String>;
type ProbeReader = SplitStream<Framed<UnixStream, LinesCodec>>;

pub(super) async fn run_probe(
    socket_path: Option<PathBuf>,
    stdio: bool,
    request: Option<String>,
    wait_ms: u64,
    first_response_timeout_ms: u64,
) -> anyhow::Result<()> {
    if stdio == request.is_some() {
        return Err(anyhow!(
            "Choose exactly one mode: pass either --stdio or --request"
        ));
    }

    let config = Config::load().with_socket_override(socket_path);
    let socket_path = config.socket_path();

    let stream = UnixStream::connect(&socket_path).await.with_context(|| {
        format!(
            "Failed to connect to daemon socket at {}",
            socket_path.display()
        )
    })?;

    let framed = Framed::new(stream, LinesCodec::new());
    let (mut writer, mut reader) = framed.split();

    if stdio {
        run_stdio_mode(&mut writer, &mut reader, wait_ms).await
    } else {
        run_request_mode(
            &mut writer,
            &mut reader,
            request,
            wait_ms,
            first_response_timeout_ms,
        )
        .await
    }
}

async fn run_stdio_mode(
    writer: &mut ProbeWriter,
    reader: &mut ProbeReader,
    wait_ms: u64,
) -> anyhow::Result<()> {
    let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();

    loop {
        tokio::select! {
            maybe_input = stdin_lines.next_line() => {
                let Some(input) = maybe_input? else {
                    break;
                };

                if input.trim().is_empty() {
                    continue;
                }

                writer.send(input).await?;
            }
            daemon_line = reader.next() => {
                let Some(line) = daemon_line.transpose()? else {
                    return Ok(());
                };
                print_line(&line).await?;
            }
        }
    }

    drain_until_idle(reader, wait_ms).await
}

async fn run_request_mode(
    writer: &mut ProbeWriter,
    reader: &mut ProbeReader,
    request: Option<String>,
    wait_ms: u64,
    first_response_timeout_ms: u64,
) -> anyhow::Result<()> {
    let request = request
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("--request cannot be empty"))?;

    writer.send(request.to_string()).await?;

    let first_response_timeout = Duration::from_millis(first_response_timeout_ms.max(1));
    let first_response = tokio::time::timeout(first_response_timeout, reader.next())
        .await
        .context("Timed out waiting for daemon response")?;

    let Some(line) = first_response.transpose()? else {
        return Err(anyhow!("Daemon closed the connection before responding"));
    };
    print_line(&line).await?;

    drain_until_idle(reader, wait_ms).await
}

async fn drain_until_idle(reader: &mut ProbeReader, wait_ms: u64) -> anyhow::Result<()> {
    if wait_ms == 0 {
        return Ok(());
    }

    let idle = Duration::from_millis(wait_ms);
    loop {
        let next = tokio::time::timeout(idle, reader.next()).await;
        match next {
            Ok(Some(line)) => {
                let line = line?;
                print_line(&line).await?;
            }
            Ok(None) | Err(_) => return Ok(()),
        }
    }
}

async fn print_line(line: &str) -> anyhow::Result<()> {
    let mut stdout = tokio::io::stdout();
    stdout.write_all(line.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}
