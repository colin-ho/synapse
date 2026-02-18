use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::llm::LlmClient;
use crate::protocol::SuggestionSource;
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};
use crate::workflow::WorkflowPredictor;

use super::workflow::{
    command_suggestion, detect_project_type, is_command_name_request, prefixed_text,
    PREDICTED_NEXT_COMMAND_DESC,
};

pub struct WorkflowLlmProvider {
    predictor: Arc<WorkflowPredictor>,
    llm_client: Arc<LlmClient>,
    inflight: Mutex<HashSet<String>>,
}

impl WorkflowLlmProvider {
    pub fn new(predictor: Arc<WorkflowPredictor>, llm_client: Arc<LlmClient>) -> Self {
        Self {
            predictor,
            llm_client,
            inflight: Mutex::new(HashSet::new()),
        }
    }

    async fn predict_inner(&self, request: &ProviderRequest) -> Option<ProviderSuggestion> {
        let previous = request.recent_commands.first()?;

        // Check if bigram prediction is weak
        let project_type = detect_project_type(&request.cwd);
        let predictions = self
            .predictor
            .predict(
                previous,
                1,
                Some(request.last_exit_code),
                project_type.as_deref(),
            )
            .await;

        let best_prob = predictions.first().map(|(_, p)| *p).unwrap_or(0.0);
        if best_prob >= 0.3 {
            // Bigram is confident enough, check if we should enrich with args
            let best_cmd = &predictions[0].0;
            return self.enrich_prediction(best_cmd, request).await;
        }

        // Bigram is weak or absent — use LLM to predict
        let result = self
            .llm_client
            .predict_workflow(
                &request.cwd,
                project_type.as_deref(),
                &request.recent_commands,
                request.last_exit_code,
            )
            .await;

        match result {
            Ok(cmd) => {
                let cmd = cmd.trim().to_string();
                if cmd.is_empty()
                    || (!request.partial.is_empty() && !cmd.starts_with(&request.partial))
                {
                    return None;
                }
                Some(command_suggestion(
                    prefixed_text(&request.prefix, cmd),
                    0.7,
                    PREDICTED_NEXT_COMMAND_DESC,
                ))
            }
            Err(e) => {
                tracing::debug!("LLM workflow prediction failed: {e}");
                None
            }
        }
    }

    /// Enrich a bigram-predicted command with LLM-generated arguments.
    async fn enrich_prediction(
        &self,
        predicted_cmd: &str,
        request: &ProviderRequest,
    ) -> Option<ProviderSuggestion> {
        // Special case: git commit after git add → generate commit message
        let previous = request.recent_commands.first()?;
        if predicted_cmd == "git commit"
            && crate::workflow::normalize_command(previous) == "git add"
        {
            if let Some(diff) =
                get_staged_diff(&request.cwd, crate::config::WORKFLOW_MAX_DIFF_TOKENS).await
            {
                if let Ok(msg) = self.llm_client.generate_commit_message(&diff).await {
                    let msg = msg.trim().trim_matches('"').to_string();
                    if !msg.is_empty() {
                        let text = format!("git commit -m \"{}\"", msg);
                        return Some(command_suggestion(text, 0.85, "predicted commit"));
                    }
                }
            }
        }

        // General argument enrichment
        match self
            .llm_client
            .enrich_command_args(predicted_cmd, &request.recent_commands, &request.cwd)
            .await
        {
            Ok(enriched) => {
                let enriched = enriched.trim().to_string();
                if enriched.is_empty() || enriched == predicted_cmd {
                    return None;
                }
                Some(command_suggestion(
                    enriched,
                    0.8,
                    PREDICTED_NEXT_COMMAND_DESC,
                ))
            }
            Err(e) => {
                tracing::debug!("LLM argument enrichment failed: {e}");
                None
            }
        }
    }
}

#[async_trait]
impl SuggestionProvider for WorkflowLlmProvider {
    async fn suggest(
        &self,
        request: &ProviderRequest,
        _max: NonZeroUsize,
    ) -> Vec<ProviderSuggestion> {
        if !is_command_name_request(request) {
            return Vec::new();
        }
        if request.recent_commands.is_empty() {
            return Vec::new();
        }

        // Inflight dedup: skip if already running for this session
        let should_run = {
            let mut inflight = self.inflight.lock().await;
            inflight.insert(request.session_id.clone())
        };
        if !should_run {
            return Vec::new();
        }

        let result = self.predict_inner(request).await;

        {
            let mut inflight = self.inflight.lock().await;
            inflight.remove(&request.session_id);
        }

        result.into_iter().collect()
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Workflow
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// Run `git diff --staged` and return the output, truncated to approximate token limit.
async fn get_staged_diff(cwd: &str, max_tokens: usize) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["diff", "--staged"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let diff = String::from_utf8_lossy(&output.stdout).to_string();
    if diff.trim().is_empty() {
        return None;
    }

    // Approximate token limit: ~4 chars per token
    let max_chars = max_tokens * 4;
    Some(truncate_at_char_boundary(&diff, max_chars))
}

fn truncate_at_char_boundary(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cutoff, _)) => text[..cutoff].to_string(),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_at_char_boundary_utf8_safe() {
        let s = "aé中🙂z";
        assert_eq!(truncate_at_char_boundary(s, 0), "");
        assert_eq!(truncate_at_char_boundary(s, 1), "a");
        assert_eq!(truncate_at_char_boundary(s, 2), "aé");
        assert_eq!(truncate_at_char_boundary(s, 4), "aé中🙂");
        assert_eq!(truncate_at_char_boundary(s, 10), s);
    }
}
