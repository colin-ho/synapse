use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

// --- Requests (Zsh → Daemon) ---

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    NaturalLanguage(NaturalLanguageRequest),
    CommandExecuted(CommandExecutedReport),
    CwdChanged(CwdChangedReport),
    Complete(CompleteRequest),
    RunGenerator(RunGeneratorRequest),
    Ping,
    Shutdown,
    ReloadConfig,
    ClearCache,
}

#[derive(Debug, Deserialize)]
pub struct CommandExecutedReport {
    pub session_id: String,
    pub command: String,
    #[serde(default)]
    pub cwd: String,
}

#[derive(Debug, Deserialize)]
pub struct CwdChangedReport {
    pub session_id: String,
    pub cwd: String,
}

#[derive(Debug, Deserialize)]
pub struct CompleteRequest {
    pub command: String,
    #[serde(default)]
    pub context: Vec<String>,
    #[serde(default)]
    pub cwd: String,
}

#[derive(Debug, Deserialize)]
pub struct RunGeneratorRequest {
    pub command: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub split_on: Option<String>,
    #[serde(default)]
    pub strip_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NaturalLanguageRequest {
    pub session_id: String,
    pub query: String,
    pub cwd: String,
    #[serde(default)]
    pub recent_commands: Vec<String>,
    #[serde(default)]
    pub env_hints: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionAction {
    Accept,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionSource {
    Llm,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    Command,
}

// --- Responses (Daemon → Zsh) ---

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    SuggestionList(SuggestionListResponse),
    CompleteResult(CompleteResultResponse),
    Pong,
    Ack,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct CompleteResultResponse {
    pub values: Vec<CompleteResultItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompleteResultItem {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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

impl SuggestionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Llm => "llm",
        }
    }
}

impl std::fmt::Display for SuggestionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl SuggestionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Command => "command",
        }
    }
}

/// Sanitize a string for TSV transport: replace tabs with spaces,
/// newlines with a space, and strip carriage returns.
fn sanitize_tsv(s: &str) -> Cow<'_, str> {
    if s.contains(['\t', '\n', '\r']) {
        Cow::Owned(s.replace('\t', "    ").replace('\n', " ").replace('\r', ""))
    } else {
        Cow::Borrowed(s)
    }
}

impl Response {
    /// Serialize this response as a single TSV line (no trailing newline).
    pub fn to_tsv(&self) -> String {
        match self {
            Response::SuggestionList(list) => {
                let mut out = format!("list\t{}", list.suggestions.len());
                for item in &list.suggestions {
                    let desc = item.description.as_deref().unwrap_or("");
                    out.push('\t');
                    out.push_str(&sanitize_tsv(&item.text));
                    out.push('\t');
                    out.push_str(item.source.as_str());
                    out.push('\t');
                    out.push_str(&sanitize_tsv(desc));
                    out.push('\t');
                    out.push_str(item.kind.as_str());
                }
                out
            }
            Response::CompleteResult(result) => {
                let mut out = format!("complete_result\t{}", result.values.len());
                for item in &result.values {
                    out.push('\t');
                    out.push_str(&sanitize_tsv(&item.value));
                    out.push('\t');
                    out.push_str(&sanitize_tsv(item.description.as_deref().unwrap_or("")));
                }
                out
            }
            Response::Pong => "pong".to_string(),
            Response::Ack => "ack".to_string(),
            Response::Error { message } => {
                format!("error\t{}", sanitize_tsv(message))
            }
        }
    }
}
