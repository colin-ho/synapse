use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::completion_context::Position;
use crate::config::{LlmConfig, WorkflowConfig};
use crate::llm::LlmClient;
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};
use crate::workflow::WorkflowPredictor;

pub struct WorkflowProvider {
    predictor: Arc<WorkflowPredictor>,
    config: WorkflowConfig,
    llm_client: Option<Arc<LlmClient>>,
    llm_config: LlmConfig,
}

impl WorkflowProvider {
    pub fn new(
        predictor: Arc<WorkflowPredictor>,
        config: WorkflowConfig,
        llm_client: Option<Arc<LlmClient>>,
        llm_config: LlmConfig,
    ) -> Self {
        Self {
            predictor,
            config,
            llm_client,
            llm_config,
        }
    }

    /// Detect the project type from the working directory.
    fn detect_project_type(cwd: &str) -> Option<String> {
        let path = Path::new(cwd);
        let root = crate::project::find_project_root(path, 3)?;
        crate::project::detect_project_type(&root)
    }

    /// LLM-powered workflow prediction for async Phase 2.
    /// Returns a prediction when bigram data is weak and LLM is available.
    pub async fn predict_with_llm(&self, request: &ProviderRequest) -> Option<ProviderSuggestion> {
        if !self.llm_config.workflow_prediction {
            return None;
        }

        let llm = self.llm_client.as_ref()?;

        // Only activate at command-name position with short/empty buffer
        if !matches!(request.completion().position, Position::CommandName)
            || request.buffer.len() > 4
        {
            return None;
        }

        let previous = request.recent_commands.first()?;

        // Check if bigram prediction is weak
        let project_type = Self::detect_project_type(&request.cwd);
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
            return self.enrich_prediction(llm, best_cmd, request).await;
        }

        // Bigram is weak or absent — use LLM to predict
        let result = llm
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
                let text = if request.prefix.is_empty() {
                    cmd
                } else {
                    format!("{}{}", request.prefix, cmd)
                };
                Some(ProviderSuggestion {
                    text,
                    source: SuggestionSource::Workflow,
                    score: 0.7,
                    description: Some("predicted next command".into()),
                    kind: SuggestionKind::Command,
                })
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
        llm: &LlmClient,
        predicted_cmd: &str,
        request: &ProviderRequest,
    ) -> Option<ProviderSuggestion> {
        // Special case: git commit after git add → generate commit message
        let previous = request.recent_commands.first()?;
        if predicted_cmd == "git commit"
            && crate::workflow::normalize_command(previous) == "git add"
        {
            if let Some(diff) =
                get_staged_diff(&request.cwd, self.llm_config.workflow_max_diff_tokens).await
            {
                if let Ok(msg) = llm.generate_commit_message(&diff).await {
                    let msg = msg.trim().trim_matches('"').to_string();
                    if !msg.is_empty() {
                        let text = format!("git commit -m \"{}\"", msg);
                        return Some(ProviderSuggestion {
                            text,
                            source: SuggestionSource::Workflow,
                            score: 0.85,
                            description: Some("predicted commit".into()),
                            kind: SuggestionKind::Command,
                        });
                    }
                }
            }
        }

        // General argument enrichment
        match llm
            .enrich_command_args(predicted_cmd, &request.recent_commands, &request.cwd)
            .await
        {
            Ok(enriched) => {
                let enriched = enriched.trim().to_string();
                if enriched.is_empty() || enriched == predicted_cmd {
                    return None;
                }
                Some(ProviderSuggestion {
                    text: enriched,
                    source: SuggestionSource::Workflow,
                    score: 0.8,
                    description: Some("predicted next command".into()),
                    kind: SuggestionKind::Command,
                })
            }
            Err(e) => {
                tracing::debug!("LLM argument enrichment failed: {e}");
                None
            }
        }
    }
}

#[async_trait]
impl SuggestionProvider for WorkflowProvider {
    async fn suggest(
        &self,
        request: &ProviderRequest,
        max: NonZeroUsize,
    ) -> Vec<ProviderSuggestion> {
        if !self.config.enabled {
            return Vec::new();
        }

        // Only activate at command-name position with short/empty buffer
        if !matches!(request.completion().position, Position::CommandName)
            || request.buffer.len() > 4
        {
            return Vec::new();
        }

        let previous = match request.recent_commands.first() {
            Some(cmd) => cmd,
            None => return Vec::new(),
        };

        let project_type = Self::detect_project_type(&request.cwd);
        let predictions = self
            .predictor
            .predict(
                previous,
                max.get(),
                Some(request.last_exit_code),
                project_type.as_deref(),
            )
            .await;

        predictions
            .into_iter()
            .filter(|(cmd, prob)| {
                *prob >= self.config.min_probability
                    && (request.partial.is_empty() || cmd.starts_with(&request.partial))
            })
            .map(|(cmd, prob)| {
                let text = if request.prefix.is_empty() {
                    cmd
                } else {
                    format!("{}{}", request.prefix, cmd)
                };
                ProviderSuggestion {
                    text,
                    source: SuggestionSource::Workflow,
                    score: prob,
                    description: Some("predicted next command".into()),
                    kind: SuggestionKind::Command,
                }
            })
            .collect()
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Workflow
    }

    fn is_available(&self) -> bool {
        self.config.enabled
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
    use crate::workflow::WorkflowPredictor;

    fn make_predictor() -> Arc<WorkflowPredictor> {
        Arc::new(WorkflowPredictor::new())
    }

    fn make_config() -> WorkflowConfig {
        WorkflowConfig {
            enabled: true,
            min_probability: 0.15,
        }
    }

    fn make_provider(predictor: Arc<WorkflowPredictor>) -> WorkflowProvider {
        WorkflowProvider::new(predictor, make_config(), None, LlmConfig::default())
    }

    #[tokio::test]
    async fn test_no_recent_commands() {
        let predictor = make_predictor();
        let provider = make_provider(predictor);

        let store = Arc::new(crate::spec_store::SpecStore::new(
            crate::config::SpecConfig::default(),
            None,
        ));
        let request = ProviderRequest::from_suggest_request(
            &crate::protocol::SuggestRequest {
                session_id: "test".into(),
                buffer: String::new(),
                cursor_pos: 0,
                cwd: "/tmp".into(),
                last_exit_code: 0,
                recent_commands: vec![],
                env_hints: Default::default(),
            },
            store,
        )
        .await;

        let suggestions = provider
            .suggest(&request, NonZeroUsize::new(5).unwrap())
            .await;
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn test_predicts_from_bigrams() {
        let predictor = make_predictor();
        predictor.record("git add .", "git commit -m 'test'").await;
        predictor.record("git add .", "git commit -m 'fix'").await;

        let provider = make_provider(predictor);

        let store = Arc::new(crate::spec_store::SpecStore::new(
            crate::config::SpecConfig::default(),
            None,
        ));
        let request = ProviderRequest::from_suggest_request(
            &crate::protocol::SuggestRequest {
                session_id: "test".into(),
                buffer: String::new(),
                cursor_pos: 0,
                cwd: "/tmp".into(),
                last_exit_code: 0,
                recent_commands: vec!["git add .".into()],
                env_hints: Default::default(),
            },
            store,
        )
        .await;

        let suggestions = provider
            .suggest(&request, NonZeroUsize::new(5).unwrap())
            .await;
        assert!(!suggestions.is_empty());
        assert_eq!(suggestions[0].text, "git commit");
        assert_eq!(suggestions[0].source, SuggestionSource::Workflow);
    }

    #[tokio::test]
    async fn test_disabled_returns_empty() {
        let predictor = make_predictor();
        predictor.record("git add .", "git commit -m 'test'").await;

        let mut provider = make_provider(predictor);
        provider.config.enabled = false;

        let store = Arc::new(crate::spec_store::SpecStore::new(
            crate::config::SpecConfig::default(),
            None,
        ));
        let request = ProviderRequest::from_suggest_request(
            &crate::protocol::SuggestRequest {
                session_id: "test".into(),
                buffer: String::new(),
                cursor_pos: 0,
                cwd: "/tmp".into(),
                last_exit_code: 0,
                recent_commands: vec!["git add .".into()],
                env_hints: Default::default(),
            },
            store,
        )
        .await;

        let suggestions = provider
            .suggest(&request, NonZeroUsize::new(5).unwrap())
            .await;
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn test_long_buffer_returns_empty() {
        let predictor = make_predictor();
        predictor.record("git add .", "git commit -m 'test'").await;

        let provider = make_provider(predictor);

        let store = Arc::new(crate::spec_store::SpecStore::new(
            crate::config::SpecConfig::default(),
            None,
        ));
        let request = ProviderRequest::from_suggest_request(
            &crate::protocol::SuggestRequest {
                session_id: "test".into(),
                buffer: "git commit".into(),
                cursor_pos: 10,
                cwd: "/tmp".into(),
                last_exit_code: 0,
                recent_commands: vec!["git add .".into()],
                env_hints: Default::default(),
            },
            store,
        )
        .await;

        let suggestions = provider
            .suggest(&request, NonZeroUsize::new(5).unwrap())
            .await;
        assert!(suggestions.is_empty());
    }

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
