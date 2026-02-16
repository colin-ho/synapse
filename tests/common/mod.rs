use std::collections::HashMap;
use std::num::NonZeroUsize;

use synapse::config::SpecConfig;
use synapse::protocol::SuggestRequest;
use synapse::providers::ProviderRequest;
use synapse::spec_store::SpecStore;

#[allow(dead_code)]
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

#[allow(dead_code)]
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
    let store = SpecStore::new(SpecConfig::default());
    ProviderRequest::from_suggest_request(&request, &store).await
}

pub fn limit(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}
