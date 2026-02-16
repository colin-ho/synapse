pub mod environment;
pub mod filesystem;
pub mod history;
pub mod llm_argument;
pub mod spec;
pub mod workflow;

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use enum_dispatch::enum_dispatch;
use std::collections::HashMap;

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
#[derive(Clone)]
pub struct ProviderRequest {
    pub session_id: String,
    pub cwd: String,
    pub recent_commands: Vec<String>,
    pub last_exit_code: i32,
    pub env_hints: HashMap<String, String>,
    completion: CompletionContext,
    pub spec_store: Arc<SpecStore>,
}

impl std::fmt::Debug for ProviderRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRequest")
            .field("session_id", &self.session_id)
            .field("cwd", &self.cwd)
            .field("recent_commands", &self.recent_commands)
            .field("env_hints", &self.env_hints)
            .field("completion", &self.completion)
            .finish_non_exhaustive()
    }
}

impl ProviderRequest {
    async fn from_parts(
        session_id: String,
        buffer: &str,
        cwd: String,
        recent_commands: Vec<String>,
        last_exit_code: i32,
        env_hints: HashMap<String, String>,
        store: Arc<SpecStore>,
    ) -> Self {
        let completion = CompletionContext::build(buffer, Path::new(&cwd), &store).await;
        Self {
            session_id,
            cwd,
            recent_commands,
            last_exit_code,
            env_hints,
            completion,
            spec_store: store,
        }
    }

    pub async fn from_suggest_request(request: &SuggestRequest, store: Arc<SpecStore>) -> Self {
        Self::from_parts(
            request.session_id.clone(),
            &request.buffer,
            request.cwd.clone(),
            request.recent_commands.clone(),
            request.last_exit_code,
            request.env_hints.clone(),
            store,
        )
        .await
    }

    pub async fn from_list_request(
        request: &ListSuggestionsRequest,
        store: Arc<SpecStore>,
    ) -> Self {
        // Preserve compatibility with protocol fields that are currently unused by providers.
        let _ = request.cursor_pos;

        Self::from_parts(
            request.session_id.clone(),
            &request.buffer,
            request.cwd.clone(),
            request.recent_commands.clone(),
            request.last_exit_code,
            request.env_hints.clone(),
            store,
        )
        .await
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
#[enum_dispatch]
pub trait SuggestionProvider: Send + Sync {
    async fn suggest(
        &self,
        request: &ProviderRequest,
        max: NonZeroUsize,
    ) -> Vec<ProviderSuggestion>;
    #[allow(dead_code)]
    fn source(&self) -> SuggestionSource;
    #[allow(dead_code)]
    fn is_available(&self) -> bool;
}

#[async_trait]
impl<T> SuggestionProvider for Arc<T>
where
    T: SuggestionProvider + ?Sized,
{
    async fn suggest(
        &self,
        request: &ProviderRequest,
        max: NonZeroUsize,
    ) -> Vec<ProviderSuggestion> {
        (**self).suggest(request, max).await
    }

    fn source(&self) -> SuggestionSource {
        (**self).source()
    }

    fn is_available(&self) -> bool {
        (**self).is_available()
    }
}

/// Enum-backed provider dispatch used by the daemon runtime.
#[derive(Clone)]
#[enum_dispatch(SuggestionProvider)]
pub enum Provider {
    History(Arc<history::HistoryProvider>),
    Spec(Arc<spec::SpecProvider>),
    Filesystem(Arc<filesystem::FilesystemProvider>),
    Environment(Arc<environment::EnvironmentProvider>),
    Workflow(Arc<workflow::WorkflowProvider>),
    LlmArgument(Arc<llm_argument::LlmArgumentProvider>),
}
