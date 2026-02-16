use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use crate::config::SpecConfig;
use crate::protocol::SuggestRequest;
use crate::providers::ProviderRequest;
use crate::spec_store::SpecStore;

pub fn make_suggest_request(buffer: &str, cwd: &str) -> SuggestRequest {
    SuggestRequest {
        session_id: "test".into(),
        buffer: buffer.into(),
        cursor_pos: buffer.len(),
        cwd: cwd.into(),
        last_exit_code: 0,
        recent_commands: vec![],
        env_hints: HashMap::new(),
    }
}

pub async fn make_provider_request(buffer: &str, cwd: &str) -> ProviderRequest {
    make_provider_request_with_env(buffer, cwd, HashMap::new()).await
}

pub async fn make_provider_request_with_env(
    buffer: &str,
    cwd: &str,
    env_hints: HashMap<String, String>,
) -> ProviderRequest {
    let request = make_suggest_request(buffer, cwd);
    let request = SuggestRequest {
        env_hints,
        ..request
    };
    let store = Arc::new(SpecStore::new(SpecConfig::default()));
    ProviderRequest::from_suggest_request(&request, store).await
}

pub async fn make_provider_request_with_store(
    buffer: &str,
    cwd: &str,
    store: Arc<SpecStore>,
) -> ProviderRequest {
    let request = make_suggest_request(buffer, cwd);
    ProviderRequest::from_suggest_request(&request, store).await
}

pub fn limit(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}
