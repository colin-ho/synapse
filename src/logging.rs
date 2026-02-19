use chrono::Utc;
use serde::{Deserialize, Serialize};
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

    fn open_log_file(path: &PathBuf) -> Option<std::fs::File> {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!("Failed to open log file: {e}");
                None
            }
        }
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

        let mut file = Self::open_log_file(&log_path);

        while let Some(entry) = rx.recv().await {
            // Check rotation
            if let Ok(meta) = std::fs::metadata(&log_path) {
                if meta.len() > max_size_mb * 1024 * 1024 {
                    let rotated = log_path.with_extension("jsonl.1");
                    if let Err(e) = std::fs::rename(&log_path, &rotated) {
                        tracing::warn!("Failed to rotate log: {e}");
                    }
                    // Reopen after rotation
                    file = Self::open_log_file(&log_path);
                }
            }

            let f = match file.as_mut() {
                Some(f) => f,
                None => {
                    // Retry opening on next entry
                    file = Self::open_log_file(&log_path);
                    continue;
                }
            };
            if let Ok(json) = serde_json::to_string(&entry) {
                if let Err(e) = writeln!(f, "{json}") {
                    tracing::warn!("Failed to write log entry: {e}");
                    // Reopen on write error
                    file = Self::open_log_file(&log_path);
                }
            }
        }
    }
}

/// Entry used when reading back the interaction log. Only the fields we need.
#[derive(Deserialize)]
struct ReadLogEntry {
    action: InteractionAction,
    suggestion: String,
    nl_query: Option<String>,
    ts: String,
}

/// Read recent accepted NL translations from the interaction log.
/// Returns `(query, command)` pairs, most recent first, up to `limit`.
pub fn read_recent_accepted(log_path: &std::path::Path, limit: usize) -> Vec<(String, String)> {
    let Ok(content) = std::fs::read_to_string(log_path) else {
        return Vec::new();
    };

    let cutoff = chrono::Utc::now() - chrono::Duration::days(7);

    let mut results: Vec<(String, String)> = content
        .lines()
        .rev() // most recent first
        .filter_map(|line| {
            let entry: ReadLogEntry = serde_json::from_str(line).ok()?;
            if !matches!(entry.action, InteractionAction::Accept) {
                return None;
            }
            let query = entry.nl_query?;
            if query.is_empty() || entry.suggestion.is_empty() {
                return None;
            }
            // Filter by recency
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&entry.ts) {
                if ts < cutoff {
                    return None;
                }
            }
            Some((query, entry.suggestion))
        })
        .take(limit * 2) // oversample, then dedup
        .collect();

    // Dedup by query
    let mut seen = std::collections::HashSet::new();
    results.retain(|(q, _)| seen.insert(q.clone()));
    results.truncate(limit);
    results
}
