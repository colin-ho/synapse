use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::completion_context::Position;
use crate::config::WorkflowConfig;
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};
use crate::workflow::WorkflowPredictor;

pub(crate) const COMMAND_NAME_BUFFER_MAX: usize = 4;
pub(crate) const PREDICTED_NEXT_COMMAND_DESC: &str = "predicted next command";

pub struct WorkflowProvider {
    predictor: Arc<WorkflowPredictor>,
    config: WorkflowConfig,
}

impl WorkflowProvider {
    pub fn new(predictor: Arc<WorkflowPredictor>, config: WorkflowConfig) -> Self {
        Self { predictor, config }
    }
}

/// Detect the project type from the working directory.
pub(crate) fn detect_project_type(cwd: &str) -> Option<String> {
    let path = Path::new(cwd);
    let root = crate::project::find_project_root(path, 3)?;
    crate::project::detect_project_type(&root)
}

pub(crate) fn is_command_name_request(request: &ProviderRequest) -> bool {
    matches!(request.completion().position, Position::CommandName)
        && request.buffer.len() <= COMMAND_NAME_BUFFER_MAX
}

pub(crate) fn prefixed_text(prefix: &str, value: String) -> String {
    if prefix.is_empty() {
        value
    } else {
        format!("{prefix}{value}")
    }
}

pub(crate) fn command_suggestion(
    text: String,
    score: f64,
    description: &'static str,
) -> ProviderSuggestion {
    ProviderSuggestion {
        text,
        source: SuggestionSource::Workflow,
        score,
        description: Some(description.into()),
        kind: SuggestionKind::Command,
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
        if !is_command_name_request(request) {
            return Vec::new();
        }

        let previous = match request.recent_commands.first() {
            Some(cmd) => cmd,
            None => return Vec::new(),
        };

        let project_type = detect_project_type(&request.cwd);
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
                command_suggestion(
                    prefixed_text(&request.prefix, cmd),
                    prob,
                    PREDICTED_NEXT_COMMAND_DESC,
                )
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
        WorkflowProvider::new(predictor, make_config())
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
}
