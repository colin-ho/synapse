pub mod ai;
pub mod context;
pub mod environment;
pub mod filesystem;
pub mod history;
pub mod spec;

use std::path::Path;

use async_trait::async_trait;

use crate::completion_context::CompletionContext;
use crate::protocol::{ListSuggestionsRequest, SuggestRequest, SuggestionKind, SuggestionSource};
use crate::spec_store::SpecStore;

#[derive(Debug, Clone)]
pub struct ProviderSuggestion {
    pub text: String,
    pub source: SuggestionSource,
    pub score: f64,
    pub description: Option<String>,
    pub kind: SuggestionKind,
}

/// Unified provider input: request metadata + parsed completion context.
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub session_id: String,
    pub cwd: String,
    pub recent_commands: Vec<String>,
    completion: CompletionContext,
}

impl ProviderRequest {
    pub async fn from_suggest_request(request: &SuggestRequest, store: &SpecStore) -> Self {
        let completion =
            CompletionContext::build(&request.buffer, Path::new(&request.cwd), store).await;
        Self {
            session_id: request.session_id.clone(),
            cwd: request.cwd.clone(),
            recent_commands: request.recent_commands.clone(),
            completion,
        }
    }

    pub async fn from_list_request(request: &ListSuggestionsRequest, store: &SpecStore) -> Self {
        // Preserve compatibility with protocol fields that are currently unused by providers.
        let _ = request.cursor_pos;
        let _ = request.last_exit_code;
        let _ = &request.env_hints;

        let completion =
            CompletionContext::build(&request.buffer, Path::new(&request.cwd), store).await;
        Self {
            session_id: request.session_id.clone(),
            cwd: request.cwd.clone(),
            recent_commands: request.recent_commands.clone(),
            completion,
        }
    }

    pub fn completion(&self) -> &CompletionContext {
        &self.completion
    }
}

impl std::ops::Deref for ProviderRequest {
    type Target = CompletionContext;

    fn deref(&self) -> &Self::Target {
        &self.completion
    }
}

#[async_trait]
pub trait SuggestionProvider: Send + Sync {
    async fn suggest(&self, request: &ProviderRequest, max: usize) -> Vec<ProviderSuggestion>;
    #[allow(dead_code)]
    fn source(&self) -> SuggestionSource;
    #[allow(dead_code)]
    fn is_available(&self) -> bool;
}
