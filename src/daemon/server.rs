use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixListener;
use tokio_util::codec::{Framed, LinesCodec};

use crate::protocol::{Request, Response};

use super::handlers::{handle_request, handle_suggest, spawn_phase2_update};
use super::state::{RuntimeState, SharedWriter};

pub(super) async fn run_server(
    listener: UnixListener,
    state: Arc<RuntimeState>,
) -> anyhow::Result<()> {
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
                break;
            }
        }
    }

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

        let mut phase2_plan = None;
        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(Request::Suggest(req)) => {
                let result = handle_suggest(req, &state).await;
                phase2_plan = result.phase2_plan;
                result.response
            }
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

        if let Some(plan) = phase2_plan {
            spawn_phase2_update(plan, &state, writer.clone());
        }
    }

    Ok(())
}
