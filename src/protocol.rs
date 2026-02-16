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
    Spec,
    Filesystem,
    Environment,
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
            SuggestionSource::Spec => write!(f, "spec"),
            SuggestionSource::Filesystem => write!(f, "filesystem"),
            SuggestionSource::Environment => write!(f, "environment"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Request, Response, SuggestionItem, SuggestionKind, SuggestionListResponse,
        SuggestionResponse, SuggestionSource,
    };

    #[test]
    fn test_protocol_serialization() {
        // Test that ping request parses correctly
        let req: Request = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert!(matches!(req, Request::Ping));

        // Test suggest request
        let req: Request = serde_json::from_str(
            r#"{"type":"suggest","session_id":"abc","buffer":"git","cursor_pos":3,"cwd":"/tmp","last_exit_code":0,"recent_commands":[]}"#,
        )
        .unwrap();
        assert!(matches!(req, Request::Suggest(_)));

        // Test interaction report
        let req: Request = serde_json::from_str(
            r#"{"type":"interaction","session_id":"abc","action":"accept","suggestion":"git status","source":"history","buffer_at_action":"git"}"#,
        )
        .unwrap();
        assert!(matches!(req, Request::Interaction(_)));
    }

    #[test]
    fn test_response_serialization() {
        let resp = Response::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"pong"}"#);

        let resp = Response::Suggestion(SuggestionResponse {
            text: "git status".into(),
            source: SuggestionSource::History,
            confidence: 0.92,
        });
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("suggestion"));
        assert!(json.contains("git status"));
        assert!(json.contains("history"));
    }

    #[test]
    fn test_list_suggestions_response_serialization() {
        let response = Response::SuggestionList(SuggestionListResponse {
            suggestions: vec![
                SuggestionItem {
                    text: "git commit".into(),
                    source: SuggestionSource::Spec,
                    confidence: 0.9,
                    description: Some("Record changes to the repository".into()),
                    kind: SuggestionKind::Subcommand,
                },
                SuggestionItem {
                    text: "git checkout".into(),
                    source: SuggestionSource::Spec,
                    confidence: 0.85,
                    description: Some("Switch branches".into()),
                    kind: SuggestionKind::Subcommand,
                },
            ],
        });

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("suggestion_list"));
        assert!(json.contains("git commit"));
        assert!(json.contains("git checkout"));
        assert!(json.contains("\"kind\":\"subcommand\""));
        assert!(json.contains("\"source\":\"spec\""));
    }

    #[test]
    fn test_list_suggestions_request_deserialization() {
        let json = r#"{"type":"list_suggestions","session_id":"abc123","buffer":"git co","cursor_pos":6,"cwd":"/tmp","max_results":5}"#;
        let req: Request = serde_json::from_str(json).unwrap();

        match req {
            Request::ListSuggestions(ls) => {
                assert_eq!(ls.session_id, "abc123");
                assert_eq!(ls.buffer, "git co");
                assert_eq!(ls.cursor_pos, 6);
                assert_eq!(ls.max_results, 5);
            }
            _ => panic!("Expected ListSuggestions request"),
        }
    }

    #[test]
    fn test_list_suggestions_default_max_results() {
        let json = r#"{"type":"list_suggestions","session_id":"abc","buffer":"git ","cursor_pos":4,"cwd":"/tmp"}"#;
        let req: Request = serde_json::from_str(json).unwrap();

        match req {
            Request::ListSuggestions(ls) => {
                assert_eq!(ls.max_results, 10);
            }
            _ => panic!("Expected ListSuggestions request"),
        }
    }

    #[test]
    fn test_suggestion_kind_serialization() {
        assert_eq!(
            serde_json::to_string(&SuggestionKind::Command).unwrap(),
            "\"command\""
        );
        assert_eq!(
            serde_json::to_string(&SuggestionKind::Subcommand).unwrap(),
            "\"subcommand\""
        );
        assert_eq!(
            serde_json::to_string(&SuggestionKind::Option).unwrap(),
            "\"option\""
        );
        assert_eq!(
            serde_json::to_string(&SuggestionKind::Argument).unwrap(),
            "\"argument\""
        );
        assert_eq!(
            serde_json::to_string(&SuggestionKind::File).unwrap(),
            "\"file\""
        );
        assert_eq!(
            serde_json::to_string(&SuggestionKind::History).unwrap(),
            "\"history\""
        );
    }
}
