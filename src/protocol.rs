use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- Requests (Zsh → Daemon) ---

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Suggest(SuggestRequest),
    ListSuggestions(ListSuggestionsRequest),
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
    #[allow(dead_code)]
    pub cursor_pos: usize,
    pub cwd: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub last_exit_code: i32,
    #[serde(default)]
    pub recent_commands: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub env_hints: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct ListSuggestionsRequest {
    pub session_id: String,
    pub buffer: String,
    pub cursor_pos: usize,
    pub cwd: String,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default)]
    pub last_exit_code: i32,
    #[serde(default)]
    pub recent_commands: Vec<String>,
    #[serde(default)]
    pub env_hints: HashMap<String, String>,
}

fn default_max_results() -> usize {
    10
}

impl ListSuggestionsRequest {
    /// Convert to a SuggestRequest for providers that use the common interface.
    pub fn as_suggest_request(&self) -> SuggestRequest {
        SuggestRequest {
            session_id: self.session_id.clone(),
            buffer: self.buffer.clone(),
            cursor_pos: self.cursor_pos,
            cwd: self.cwd.clone(),
            last_exit_code: self.last_exit_code,
            recent_commands: self.recent_commands.clone(),
            env_hints: self.env_hints.clone(),
        }
    }
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
    Spec,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    Command,
    Subcommand,
    Option,
    Argument,
    File,
    History,
}

// --- Responses (Daemon → Zsh) ---

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Suggestion(SuggestionResponse),
    Update(SuggestionResponse),
    SuggestionList(SuggestionListResponse),
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

#[derive(Debug, Clone, Serialize)]
pub struct SuggestionListResponse {
    pub suggestions: Vec<SuggestionItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestionItem {
    pub text: String,
    pub source: SuggestionSource,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: SuggestionKind,
}

impl std::fmt::Display for SuggestionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SuggestionSource::History => write!(f, "history"),
            SuggestionSource::Context => write!(f, "context"),
            SuggestionSource::Ai => write!(f, "ai"),
            SuggestionSource::Spec => write!(f, "spec"),
        }
    }
}
