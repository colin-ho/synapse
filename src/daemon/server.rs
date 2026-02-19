use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixListener;
use tokio_util::codec::{Framed, LinesCodec};
use tokio_util::sync::CancellationToken;

use crate::protocol::{Request, Response};

use super::handlers::handle_request;
use super::state::{RuntimeState, SharedWriter};

pub(super) async fn run_server(
    listener: UnixListener,
    state: Arc<RuntimeState>,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // Spawn periodic zsh_index refresh (every 5 minutes)
    // Catches newly-installed tools (e.g. `brew install` while daemon is running)
    {
        let spec_store = state.spec_store.clone();
        let token = shutdown.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            interval.tick().await; // Skip the initial immediate tick
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        spec_store.refresh_zsh_index();
                    }
                    _ = token.cancelled() => break,
                }
            }
        });
    }

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let state = state.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, state).await {
                                tracing::debug!("Connection error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Accept error: {e}");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received Ctrl+C, shutting down");
                shutdown.cancel();
                break;
            }
            _ = shutdown.cancelled() => {
                tracing::info!("Shutdown requested via CancellationToken");
                break;
            }
        }
    }

    // Flush interaction log by dropping the logger (which drops the channel sender)
    tracing::debug!("Draining connections and flushing logs");

    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    state: Arc<RuntimeState>,
) -> anyhow::Result<()> {
    let framed = Framed::new(stream, LinesCodec::new());
    let (writer, mut reader) = framed.split();
    let writer: SharedWriter = Arc::new(tokio::sync::Mutex::new(writer));

    loop {
        let Some(line_result) = reader.next().await else {
            break; // Connection closed
        };
        let line = match line_result {
            Ok(line) => line,
            Err(e) => {
                tracing::debug!("Frame read error: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        tracing::trace!("Received: {trimmed}");

        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(request) => handle_request(request, &state).await,
            Err(e) => {
                tracing::warn!("Parse error: {e}");
                Response::Error {
                    message: format!("Invalid request: {e}"),
                }
            }
        };

        let response_line = response.to_tsv();
        let mut w = writer.lock().await;
        w.send(response_line).await?;
        drop(w);
    }

    Ok(())
}
