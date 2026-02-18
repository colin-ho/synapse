use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::num::NonZeroUsize;

// --- Requests (Zsh → Daemon) ---

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Suggest(SuggestRequest),
    ListSuggestions(ListSuggestionsRequest),
    NaturalLanguage(NaturalLanguageRequest),
    Explain(ExplainRequest),
    Interaction(InteractionReport),
    CommandExecuted(CommandExecutedReport),
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
    pub max_results: NonZeroUsize,
    #[serde(default)]
    pub last_exit_code: i32,
    #[serde(default)]
    pub recent_commands: Vec<String>,
    #[serde(default)]
    pub env_hints: HashMap<String, String>,
}

fn default_max_results() -> NonZeroUsize {
    NonZeroUsize::new(50).unwrap()
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

#[derive(Debug, Deserialize)]
pub struct CommandExecutedReport {
    pub session_id: String,
    pub command: String,
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

#[derive(Debug, Deserialize)]
pub struct ExplainRequest {
    pub session_id: String,
    pub command: String,
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
    Spec,
    Filesystem,
    Environment,
    Workflow,
    Llm,
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
    /// Signals that all async results (e.g. NL translations) have been sent.
    SuggestDone,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestionResponse {
    pub text: String,
    pub source: SuggestionSource,
    pub confidence: f64,
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
            Self::History => "history",
            Self::Spec => "spec",
            Self::Filesystem => "filesystem",
            Self::Environment => "environment",
            Self::Workflow => "workflow",
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
            Self::Subcommand => "subcommand",
            Self::Option => "option",
            Self::Argument => "argument",
            Self::File => "file",
            Self::History => "history",
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
    fn to_tsv_suggestion(prefix: &str, suggestion: &SuggestionResponse) -> String {
        let mut line = format!(
            "{prefix}\t{}\t{}",
            sanitize_tsv(&suggestion.text),
            suggestion.source.as_str()
        );
        if let Some(ref desc) = suggestion.description {
            line.push('\t');
            line.push_str(&sanitize_tsv(desc));
        }
        line
    }

    /// Serialize this response as a single TSV line (no trailing newline).
    pub fn to_tsv(&self) -> String {
        match self {
            Response::Suggestion(s) => Self::to_tsv_suggestion("suggest", s),
            Response::Update(s) => Self::to_tsv_suggestion("update", s),
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
            Response::Pong => "pong".to_string(),
            Response::Ack => "ack".to_string(),
            Response::SuggestDone => "suggest_done".to_string(),
            Response::Error { message } => {
                format!("error\t{}", sanitize_tsv(message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

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

        // Test command_executed report
        let req: Request = serde_json::from_str(
            r#"{"type":"command_executed","session_id":"abc","command":"git status"}"#,
        )
        .unwrap();
        assert!(matches!(req, Request::CommandExecuted(_)));
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
            description: None,
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
                assert_eq!(ls.max_results, NonZeroUsize::new(5).unwrap());
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
                assert_eq!(ls.max_results, NonZeroUsize::new(50).unwrap());
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

    #[test]
    fn test_to_tsv_pong() {
        assert_eq!(Response::Pong.to_tsv(), "pong");
    }

    #[test]
    fn test_to_tsv_ack() {
        assert_eq!(Response::Ack.to_tsv(), "ack");
    }

    #[test]
    fn test_to_tsv_error() {
        let resp = Response::Error {
            message: "bad request".into(),
        };
        assert_eq!(resp.to_tsv(), "error\tbad request");
    }

    #[test]
    fn test_to_tsv_suggestion() {
        let resp = Response::Suggestion(SuggestionResponse {
            text: "git status".into(),
            source: SuggestionSource::History,
            confidence: 0.9,
            description: None,
        });
        assert_eq!(resp.to_tsv(), "suggest\tgit status\thistory");
    }

    #[test]
    fn test_to_tsv_update() {
        let resp = Response::Update(SuggestionResponse {
            text: "git status --verbose".into(),
            source: SuggestionSource::Spec,
            confidence: 0.87,
            description: None,
        });
        assert_eq!(resp.to_tsv(), "update\tgit status --verbose\tspec");
    }

    #[test]
    fn test_to_tsv_suggestion_list() {
        let resp = Response::SuggestionList(SuggestionListResponse {
            suggestions: vec![
                SuggestionItem {
                    text: "git status".into(),
                    source: SuggestionSource::History,
                    confidence: 0.9,
                    description: None,
                    kind: SuggestionKind::Command,
                },
                SuggestionItem {
                    text: "git stash".into(),
                    source: SuggestionSource::Spec,
                    confidence: 0.88,
                    description: Some("Stash changes".into()),
                    kind: SuggestionKind::Subcommand,
                },
            ],
        });
        assert_eq!(
            resp.to_tsv(),
            "list\t2\tgit status\thistory\t\tcommand\tgit stash\tspec\tStash changes\tsubcommand"
        );
    }

    #[test]
    fn test_to_tsv_sanitizes_tabs_and_newlines() {
        let resp = Response::Suggestion(SuggestionResponse {
            text: "echo\thello\nworld".into(),
            source: SuggestionSource::History,
            confidence: 0.5,
            description: None,
        });
        assert_eq!(resp.to_tsv(), "suggest\techo    hello world\thistory");
    }

    #[test]
    fn test_to_tsv_empty_suggestion() {
        let resp = Response::Suggestion(SuggestionResponse {
            text: String::new(),
            source: SuggestionSource::History,
            confidence: 0.0,
            description: None,
        });
        assert_eq!(resp.to_tsv(), "suggest\t\thistory");
    }

    #[test]
    fn test_natural_language_request_deserialization() {
        let json = r#"{"type":"natural_language","session_id":"abc","query":"find large files","cwd":"/tmp"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::NaturalLanguage(nl) => {
                assert_eq!(nl.query, "find large files");
                assert_eq!(nl.cwd, "/tmp");
                assert!(nl.recent_commands.is_empty());
                assert!(nl.env_hints.is_empty());
            }
            _ => panic!("Expected NaturalLanguage request"),
        }
    }

    #[test]
    fn test_explain_request_deserialization() {
        let json =
            r#"{"type":"explain","session_id":"abc","command":"find . -type f -size +100M"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::Explain(ex) => {
                assert_eq!(ex.command, "find . -type f -size +100M");
            }
            _ => panic!("Expected Explain request"),
        }
    }

    #[test]
    fn test_to_tsv_suggestion_with_description() {
        let resp = Response::Suggestion(SuggestionResponse {
            text: "rm -rf /tmp/old".into(),
            source: SuggestionSource::Llm,
            confidence: 0.95,
            description: Some("deletes files".into()),
        });
        assert_eq!(
            resp.to_tsv(),
            "suggest\trm -rf /tmp/old\tllm\tdeletes files"
        );
    }

    #[test]
    fn test_to_tsv_suggestion_list_empty_descriptions_preserve_field_count() {
        // Regression: the Zsh plugin splits TSV with a stride of 4 fields per item.
        // Empty descriptions MUST produce an empty field (\t\t) so the stride stays aligned.
        let resp = Response::SuggestionList(SuggestionListResponse {
            suggestions: vec![
                SuggestionItem {
                    text: "daft-sync".into(),
                    source: SuggestionSource::History,
                    confidence: 0.9,
                    description: None,
                    kind: SuggestionKind::History,
                },
                SuggestionItem {
                    text: "daft-sync stop".into(),
                    source: SuggestionSource::History,
                    confidence: 0.8,
                    description: None,
                    kind: SuggestionKind::History,
                },
                SuggestionItem {
                    text: "daft-sync watch".into(),
                    source: SuggestionSource::History,
                    confidence: 0.7,
                    description: None,
                    kind: SuggestionKind::History,
                },
            ],
        });
        let tsv = resp.to_tsv();
        let fields: Vec<&str> = tsv.split('\t').collect();

        // 2 header fields + 3 items * 4 fields each = 14
        assert_eq!(fields.len(), 14, "TSV field count mismatch: {tsv:?}");
        assert_eq!(fields[0], "list");
        assert_eq!(fields[1], "3");

        // Each item must have exactly 4 fields: text, source, desc, kind
        for i in 0..3 {
            let base = 2 + i * 4;
            assert!(
                !fields[base].is_empty(),
                "item {i} text should not be empty"
            );
            assert_eq!(fields[base + 1], "history", "item {i} source");
            assert_eq!(fields[base + 2], "", "item {i} desc should be empty");
            assert_eq!(fields[base + 3], "history", "item {i} kind");
        }
    }

    #[test]
    fn test_as_str_matches_serde() {
        for source in [
            SuggestionSource::History,
            SuggestionSource::Spec,
            SuggestionSource::Filesystem,
            SuggestionSource::Environment,
            SuggestionSource::Workflow,
            SuggestionSource::Llm,
        ] {
            let serde_str = serde_json::to_string(&source).unwrap();
            assert_eq!(format!("\"{}\"", source.as_str()), serde_str);
        }
        for kind in [
            SuggestionKind::Command,
            SuggestionKind::Subcommand,
            SuggestionKind::Option,
            SuggestionKind::Argument,
            SuggestionKind::File,
            SuggestionKind::History,
        ] {
            let serde_str = serde_json::to_string(&kind).unwrap();
            assert_eq!(format!("\"{}\"", kind.as_str()), serde_str);
        }
    }
}
