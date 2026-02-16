use std::collections::HashMap;

use synapse::config::SpecConfig;
use synapse::protocol::SuggestRequest;
use synapse::providers::ProviderRequest;
use synapse::spec_store::SpecStore;

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
    let request = make_suggest_request(buffer, cwd);
    let store = SpecStore::new(SpecConfig::default());
    ProviderRequest::from_suggest_request(&request, &store).await
}
