pub mod ai;
pub mod context;
pub mod environment;
pub mod filesystem;
pub mod history;
pub mod spec;

use async_trait::async_trait;

use crate::completion_context::CompletionContext;
use crate::protocol::{SuggestRequest, SuggestionKind, SuggestionSource};

#[derive(Debug, Clone)]
pub struct ProviderSuggestion {
    pub text: String,
    pub source: SuggestionSource,
    pub score: f64,
    pub description: Option<String>,
    pub kind: SuggestionKind,
}

#[async_trait]
pub trait SuggestionProvider: Send + Sync {
    async fn suggest(
        &self,
        request: &SuggestRequest,
        ctx: Option<&CompletionContext>,
    ) -> Option<ProviderSuggestion>;
    #[allow(dead_code)]
    fn source(&self) -> SuggestionSource;
    #[allow(dead_code)]
    fn is_available(&self) -> bool;

    /// Return multiple suggestions, up to `max`. Default implementation wraps `suggest()`.
    async fn suggest_multi(
        &self,
        request: &SuggestRequest,
        max: usize,
        ctx: Option<&CompletionContext>,
    ) -> Vec<ProviderSuggestion> {
        let _ = max;
        self.suggest(request, ctx).await.into_iter().collect()
    }
}
