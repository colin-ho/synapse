pub mod ai;
pub mod context;
pub mod history;

use async_trait::async_trait;

use crate::protocol::{SuggestRequest, SuggestionSource};

#[derive(Debug, Clone)]
pub struct ProviderSuggestion {
    pub text: String,
    pub source: SuggestionSource,
    pub score: f64,
}

#[async_trait]
pub trait SuggestionProvider: Send + Sync {
    async fn suggest(&self, request: &SuggestRequest) -> Option<ProviderSuggestion>;
    fn source(&self) -> SuggestionSource;
    fn is_available(&self) -> bool;
}
