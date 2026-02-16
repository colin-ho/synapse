use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

/// Predicts the next command based on bigram patterns from command history.
pub struct WorkflowPredictor {
    data: Arc<RwLock<WorkflowData>>,
    persist_path: PathBuf,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct WorkflowData {
    /// bigrams[previous_command] = vec![(next_command, count)]
    bigrams: HashMap<String, Vec<(String, u32)>>,
}

impl Default for WorkflowPredictor {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkflowPredictor {
    pub fn new() -> Self {
        let persist_path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join("synapse")
            .join("workflows.json");

        let data = if persist_path.exists() {
            match std::fs::read_to_string(&persist_path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => WorkflowData::default(),
            }
        } else {
            WorkflowData::default()
        };

        Self {
            data: Arc::new(RwLock::new(data)),
            persist_path,
        }
    }

    /// Record a command transition (previous → current).
    pub async fn record(&self, previous: &str, current: &str) {
        let prev_norm = normalize_command(previous);
        let curr_norm = normalize_command(current);

        if prev_norm.is_empty() || curr_norm.is_empty() || prev_norm == curr_norm {
            return;
        }

        let mut data = self.data.write().await;
        let entries = data.bigrams.entry(prev_norm).or_default();

        if let Some(entry) = entries.iter_mut().find(|(cmd, _)| cmd == &curr_norm) {
            entry.1 += 1;
        } else {
            entries.push((curr_norm, 1));
        }

        // Persist (best effort)
        self.persist(&data);
    }

    /// Predict the next commands based on the previous command.
    /// Returns up to `max` predictions with normalized probabilities.
    #[allow(dead_code)]
    pub async fn predict(&self, previous: &str, max: usize) -> Vec<(String, f64)> {
        let prev_norm = normalize_command(previous);
        if prev_norm.is_empty() {
            return Vec::new();
        }

        let data = self.data.read().await;
        let entries = match data.bigrams.get(&prev_norm) {
            Some(e) => e,
            None => return Vec::new(),
        };

        let total: u32 = entries.iter().map(|(_, c)| c).sum();
        if total == 0 {
            return Vec::new();
        }

        let mut predictions: Vec<(String, f64)> = entries
            .iter()
            .map(|(cmd, count)| (cmd.clone(), *count as f64 / total as f64))
            .collect();

        predictions.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        predictions.truncate(max);
        predictions
    }

    fn persist(&self, data: &WorkflowData) {
        if let Some(parent) = self.persist_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(data) {
            let _ = std::fs::write(&self.persist_path, json);
        }
    }
}

/// Normalize a command to its "command [subcommand]" prefix.
/// e.g., "git commit -m 'fix'" → "git commit"
pub fn normalize_command(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    match parts.len() {
        0 => String::new(),
        1 => parts[0].to_string(),
        _ => {
            // Include the first two words if the second doesn't start with '-'
            if parts[1].starts_with('-') {
                parts[0].to_string()
            } else {
                format!("{} {}", parts[0], parts[1])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_command() {
        assert_eq!(normalize_command("git commit -m 'fix'"), "git commit");
        assert_eq!(normalize_command("ls -la"), "ls");
        assert_eq!(normalize_command("cargo test"), "cargo test");
        assert_eq!(normalize_command(""), "");
        assert_eq!(normalize_command("pwd"), "pwd");
    }

    #[tokio::test]
    async fn test_record_and_predict() {
        let predictor = WorkflowPredictor {
            data: Arc::new(RwLock::new(WorkflowData::default())),
            persist_path: PathBuf::from("/tmp/synapse-test-workflows.json"),
        };

        predictor.record("git add .", "git commit -m 'test'").await;
        predictor.record("git add .", "git commit -m 'fix'").await;
        predictor.record("git add .", "git status").await;
        predictor.record("git commit -m 'test'", "git push").await;

        let predictions = predictor.predict("git add", 5).await;
        assert!(!predictions.is_empty());
        // "git commit" should be the top prediction (2 occurrences)
        assert_eq!(predictions[0].0, "git commit");
    }

    #[tokio::test]
    async fn test_empty_predictions() {
        let predictor = WorkflowPredictor {
            data: Arc::new(RwLock::new(WorkflowData::default())),
            persist_path: PathBuf::from("/tmp/synapse-test-workflows2.json"),
        };

        let predictions = predictor.predict("unknown", 5).await;
        assert!(predictions.is_empty());
    }
}
