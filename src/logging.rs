use chrono::Utc;
use serde::Serialize;
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::protocol::{InteractionAction, SuggestionSource};

#[derive(Debug, Serialize)]
pub struct InteractionLogEntry {
    pub ts: String,
    pub session: String,
    pub action: InteractionAction,
    pub buffer: String,
    pub suggestion: String,
    pub source: SuggestionSource,
    pub confidence: f64,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nl_query: Option<String>,
}

#[derive(Clone)]
pub struct InteractionLogger {
    tx: mpsc::UnboundedSender<InteractionLogEntry>,
}

impl InteractionLogger {
    pub fn new(log_path: PathBuf, max_size_mb: u64) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(Self::writer_task(rx, log_path, max_size_mb));
        Self { tx }
    }

    pub fn log(&self, entry: InteractionLogEntry) {
        if let Err(e) = self.tx.send(entry) {
            tracing::warn!("Failed to send log entry: {e}");
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_interaction(
        &self,
        session_id: &str,
        action: InteractionAction,
        buffer: &str,
        suggestion: &str,
        source: SuggestionSource,
        confidence: f64,
        cwd: &str,
        nl_query: Option<&str>,
    ) {
        self.log(InteractionLogEntry {
            ts: Utc::now().to_rfc3339(),
            session: session_id.to_string(),
            action,
            buffer: buffer.to_string(),
            suggestion: suggestion.to_string(),
            source,
            confidence,
            cwd: cwd.to_string(),
            nl_query: nl_query.map(String::from),
        });
    }

    async fn writer_task(
        mut rx: mpsc::UnboundedReceiver<InteractionLogEntry>,
        log_path: PathBuf,
        max_size_mb: u64,
    ) {
        use std::io::Write;

        if let Some(parent) = log_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::error!("Failed to create log directory: {e}");
                return;
            }
        }

        while let Some(entry) = rx.recv().await {
            // Check rotation
            if let Ok(meta) = std::fs::metadata(&log_path) {
                if meta.len() > max_size_mb * 1024 * 1024 {
                    let rotated = log_path.with_extension("jsonl.1");
                    if let Err(e) = std::fs::rename(&log_path, &rotated) {
                        tracing::warn!("Failed to rotate log: {e}");
                    }
                }
            }

            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                Ok(mut file) => {
                    if let Ok(json) = serde_json::to_string(&entry) {
                        if let Err(e) = writeln!(file, "{json}") {
                            tracing::warn!("Failed to write log entry: {e}");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to open log file: {e}");
                }
            }
        }
    }
}
