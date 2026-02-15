use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- Requests (Zsh → Daemon) ---

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Suggest(SuggestRequest),
    Interaction(InteractionReport),
    Ping,
    Shutdown,
    ReloadConfig,
    ClearCache,
}

#[derive(Debug, Deserialize)]
pub struct SuggestRequest {
    pub session_id: String,
    pub buffer: String,
    pub cursor_pos: usize,
    pub cwd: String,
    #[serde(default)]
    pub last_exit_code: i32,
    #[serde(default)]
    pub recent_commands: Vec<String>,
    #[serde(default)]
    pub env_hints: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct InteractionReport {
    pub session_id: String,
    pub action: InteractionAction,
    pub suggestion: String,
    pub source: SuggestionSource,
    #[serde(default)]
    pub buffer_at_action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionAction {
    Accept,
    Dismiss,
    Ignore,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionSource {
    History,
    Context,
    Ai,
}

// --- Responses (Daemon → Zsh) ---

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Suggestion(SuggestionResponse),
    Update(SuggestionResponse),
    Pong,
    Ack,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestionResponse {
    pub text: String,
    pub source: SuggestionSource,
    pub confidence: f64,
}

impl std::fmt::Display for SuggestionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SuggestionSource::History => write!(f, "history"),
            SuggestionSource::Context => write!(f, "context"),
            SuggestionSource::Ai => write!(f, "ai"),
        }
    }
}
