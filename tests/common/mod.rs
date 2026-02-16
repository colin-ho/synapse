use std::collections::HashMap;

use synapse::protocol::SuggestRequest;

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
