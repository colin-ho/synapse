use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

/// Exit code classification for workflow transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ExitBucket {
    Success, // 0
    Failure, // 1-125
    Signal,  // 126+
}

impl ExitBucket {
    pub fn from_code(code: i32) -> Self {
        match code {
            0 => Self::Success,
            1..=125 => Self::Failure,
            _ => Self::Signal,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "ok",
            Self::Failure => "fail",
            Self::Signal => "sig",
        }
    }
}

/// Predicts the next command based on bigram patterns from command history.
pub struct WorkflowPredictor {
    data: Arc<RwLock<WorkflowData>>,
    persist_path: PathBuf,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct WorkflowData {
    /// bigrams[previous_command] = vec![(next_command, count)]
    bigrams: HashMap<String, Vec<(String, u32)>>,
    /// Exit-code-aware transitions: key = "{prev}\0{exit_bucket}"
    #[serde(default)]
    transitions: HashMap<String, Vec<(String, u32)>>,
    /// Project-type-scoped bigrams: key = "{project_type}\0{prev}"
    #[serde(default)]
    project_bigrams: HashMap<String, Vec<(String, u32)>>,
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
        increment_entry(data.bigrams.entry(prev_norm).or_default(), &curr_norm);
        self.persist(&data);
    }

    /// Record a command transition with exit code and project type context.
    pub async fn record_with_context(
        &self,
        previous: &str,
        current: &str,
        exit_code: i32,
        project_type: Option<&str>,
    ) {
        let prev_norm = normalize_command(previous);
        let curr_norm = normalize_command(current);

        if prev_norm.is_empty() || curr_norm.is_empty() || prev_norm == curr_norm {
            return;
        }

        let mut data = self.data.write().await;

        // Global bigram
        increment_entry(
            data.bigrams.entry(prev_norm.clone()).or_default(),
            &curr_norm,
        );

        // Exit-code-aware transition
        let bucket = ExitBucket::from_code(exit_code);
        let transition_key = format!("{}\0{}", prev_norm, bucket.as_str());
        increment_entry(
            data.transitions.entry(transition_key).or_default(),
            &curr_norm,
        );

        // Project-type-scoped bigram
        if let Some(pt) = project_type {
            let project_key = format!("{}\0{}", pt, prev_norm);
            increment_entry(
                data.project_bigrams.entry(project_key).or_default(),
                &curr_norm,
            );
        }

        self.persist(&data);
    }

    /// Predict the next commands based on the previous command.
    /// Returns up to `max` predictions with normalized probabilities.
    /// More specific maps (exit-code, project-type) boost scores.
    pub async fn predict(
        &self,
        previous: &str,
        max: usize,
        exit_code: Option<i32>,
        project_type: Option<&str>,
    ) -> Vec<(String, f64)> {
        let prev_norm = normalize_command(previous);
        if prev_norm.is_empty() {
            return Vec::new();
        }

        let data = self.data.read().await;

        // Collect scores from all maps, merging with weighted contributions.
        let mut scores: HashMap<String, f64> = HashMap::new();

        // Base: global bigrams (weight 1.0)
        if let Some(entries) = data.bigrams.get(&prev_norm) {
            let total: u32 = entries.iter().map(|(_, c)| c).sum();
            if total > 0 {
                for (cmd, count) in entries {
                    let prob = *count as f64 / total as f64;
                    *scores.entry(cmd.clone()).or_default() += prob;
                }
            }
        }

        // Exit-code-aware transitions (boost by 0.5)
        if let Some(code) = exit_code {
            let bucket = ExitBucket::from_code(code);
            let key = format!("{}\0{}", prev_norm, bucket.as_str());
            if let Some(entries) = data.transitions.get(&key) {
                let total: u32 = entries.iter().map(|(_, c)| c).sum();
                if total > 0 {
                    for (cmd, count) in entries {
                        let prob = *count as f64 / total as f64;
                        *scores.entry(cmd.clone()).or_default() += prob * 0.5;
                    }
                }
            }
        }

        // Project-type-scoped bigrams (boost by 0.3)
        if let Some(pt) = project_type {
            let key = format!("{}\0{}", pt, prev_norm);
            if let Some(entries) = data.project_bigrams.get(&key) {
                let total: u32 = entries.iter().map(|(_, c)| c).sum();
                if total > 0 {
                    for (cmd, count) in entries {
                        let prob = *count as f64 / total as f64;
                        *scores.entry(cmd.clone()).or_default() += prob * 0.3;
                    }
                }
            }
        }

        if scores.is_empty() {
            return Vec::new();
        }

        // Normalize into a probability distribution.
        let total_score: f64 = scores.values().sum();
        if total_score > 0.0 {
            for v in scores.values_mut() {
                *v /= total_score;
            }
        }

        let mut predictions: Vec<(String, f64)> = scores.into_iter().collect();
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

/// Increment the count for `cmd` in entries, or insert with count 1.
fn increment_entry(entries: &mut Vec<(String, u32)>, cmd: &str) {
    if let Some(entry) = entries.iter_mut().find(|(c, _)| c == cmd) {
        entry.1 += 1;
    } else {
        entries.push((cmd.to_string(), 1));
    }
}

/// Normalize a command to its "command [subcommand]" prefix.
/// e.g., "git commit -m 'fix'" → "git commit"
pub fn normalize_command(cmd: &str) -> String {
    let parts: Vec<String> = shlex::split(cmd)
        .unwrap_or_else(|| cmd.split_whitespace().map(ToString::to_string).collect());
    match parts.len() {
        0 => String::new(),
        1 => parts[0].clone(),
        _ => {
            // Include the first two words if the second doesn't start with '-'
            if parts[1].starts_with('-') {
                parts[0].clone()
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

    #[test]
    fn test_exit_bucket() {
        assert_eq!(ExitBucket::from_code(0), ExitBucket::Success);
        assert_eq!(ExitBucket::from_code(1), ExitBucket::Failure);
        assert_eq!(ExitBucket::from_code(125), ExitBucket::Failure);
        assert_eq!(ExitBucket::from_code(126), ExitBucket::Signal);
        assert_eq!(ExitBucket::from_code(130), ExitBucket::Signal);
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

        let predictions = predictor.predict("git add", 5, None, None).await;
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

        let predictions = predictor.predict("unknown", 5, None, None).await;
        assert!(predictions.is_empty());
    }

    #[tokio::test]
    async fn test_record_with_context() {
        let predictor = WorkflowPredictor {
            data: Arc::new(RwLock::new(WorkflowData::default())),
            persist_path: PathBuf::from("/tmp/synapse-test-workflows3.json"),
        };

        // Record with context
        predictor
            .record_with_context("cargo build", "cargo build", 1, Some("rust"))
            .await;
        // Same command should be skipped (prev == curr)
        let predictions = predictor
            .predict("cargo build", 5, Some(1), Some("rust"))
            .await;
        assert!(predictions.is_empty());

        // Record different transitions
        predictor
            .record_with_context("cargo build", "cargo test", 0, Some("rust"))
            .await;
        predictor
            .record_with_context("cargo build", "cargo build", 1, None)
            .await;
        // prev == curr is still skipped
        let predictions = predictor
            .predict("cargo build", 5, Some(0), Some("rust"))
            .await;
        assert!(!predictions.is_empty());
        assert_eq!(predictions[0].0, "cargo test");
    }

    #[tokio::test]
    async fn test_exit_code_aware_predictions() {
        let predictor = WorkflowPredictor {
            data: Arc::new(RwLock::new(WorkflowData::default())),
            persist_path: PathBuf::from("/tmp/synapse-test-workflows4.json"),
        };

        // After successful build, user runs tests
        predictor
            .record_with_context("cargo build", "cargo test", 0, Some("rust"))
            .await;
        predictor
            .record_with_context("cargo build", "cargo test", 0, Some("rust"))
            .await;

        // After failed build, user fixes and rebuilds
        predictor
            .record_with_context("cargo build", "cargo clippy", 1, Some("rust"))
            .await;

        // With exit code 0, "cargo test" should score higher
        let predictions = predictor
            .predict("cargo build", 5, Some(0), Some("rust"))
            .await;
        assert!(!predictions.is_empty());
        assert_eq!(predictions[0].0, "cargo test");
    }

    #[tokio::test]
    async fn test_predict_returns_normalized_probabilities() {
        let predictor = WorkflowPredictor {
            data: Arc::new(RwLock::new(WorkflowData::default())),
            persist_path: PathBuf::from("/tmp/synapse-test-workflows5.json"),
        };

        predictor.record("git add .", "git commit -m test").await;
        predictor.record("git add .", "git commit -m fix").await;
        predictor.record("git add .", "git status").await;

        let predictions = predictor.predict("git add", 5, None, None).await;
        assert_eq!(predictions.len(), 2);
        assert_eq!(predictions[0].0, "git commit");
        assert!(predictions[0].1 < 1.0);
        let sum: f64 = predictions.iter().map(|(_, p)| p).sum();
        assert!((sum - 1.0).abs() < 1e-6);
    }
}
