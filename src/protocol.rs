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

#[cfg(test)]
mod tests {
    use super::{
        Request, Response, SuggestionItem, SuggestionKind, SuggestionListResponse, SuggestionSource,
    };

    #[test]
    fn test_protocol_serialization() {
        let req: Request = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert!(matches!(req, Request::Ping));

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
    }

    #[test]
    fn test_list_suggestions_response_serialization() {
        let response = Response::SuggestionList(SuggestionListResponse {
            suggestions: vec![
                SuggestionItem {
                    text: "git commit".into(),
                    source: SuggestionSource::Llm,
                    confidence: 0.9,
                    description: Some("Record changes to the repository".into()),
                    kind: SuggestionKind::Command,
                },
                SuggestionItem {
                    text: "git checkout".into(),
                    source: SuggestionSource::Llm,
                    confidence: 0.85,
                    description: Some("Switch branches".into()),
                    kind: SuggestionKind::Command,
                },
            ],
        });

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("suggestion_list"));
        assert!(json.contains("git commit"));
        assert!(json.contains("git checkout"));
        assert!(json.contains("\"kind\":\"command\""));
        assert!(json.contains("\"source\":\"llm\""));
    }

    #[test]
    fn test_suggestion_kind_serialization() {
        assert_eq!(
            serde_json::to_string(&SuggestionKind::Command).unwrap(),
            "\"command\""
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
    fn test_to_tsv_suggestion_list() {
        let resp = Response::SuggestionList(SuggestionListResponse {
            suggestions: vec![
                SuggestionItem {
                    text: "git status".into(),
                    source: SuggestionSource::Llm,
                    confidence: 0.9,
                    description: None,
                    kind: SuggestionKind::Command,
                },
                SuggestionItem {
                    text: "git stash".into(),
                    source: SuggestionSource::Llm,
                    confidence: 0.88,
                    description: Some("Stash changes".into()),
                    kind: SuggestionKind::Command,
                },
            ],
        });
        assert_eq!(
            resp.to_tsv(),
            "list\t2\tgit status\tllm\t\tcommand\tgit stash\tllm\tStash changes\tcommand"
        );
    }

    #[test]
    fn test_to_tsv_sanitizes_tabs_and_newlines() {
        let resp = Response::Error {
            message: "bad\trequest\nwith newline".into(),
        };
        assert_eq!(resp.to_tsv(), "error\tbad    request with newline");
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
    fn test_to_tsv_suggestion_list_empty_descriptions_preserve_field_count() {
        let resp = Response::SuggestionList(SuggestionListResponse {
            suggestions: vec![
                SuggestionItem {
                    text: "daft-sync".into(),
                    source: SuggestionSource::Llm,
                    confidence: 0.9,
                    description: None,
                    kind: SuggestionKind::Command,
                },
                SuggestionItem {
                    text: "daft-sync stop".into(),
                    source: SuggestionSource::Llm,
                    confidence: 0.8,
                    description: None,
                    kind: SuggestionKind::Command,
                },
                SuggestionItem {
                    text: "daft-sync watch".into(),
                    source: SuggestionSource::Llm,
                    confidence: 0.7,
                    description: None,
                    kind: SuggestionKind::Command,
                },
            ],
        });
        let tsv = resp.to_tsv();
        let fields: Vec<&str> = tsv.split('\t').collect();

        // 2 header fields + 3 items * 4 fields each = 14
        assert_eq!(fields.len(), 14, "TSV field count mismatch: {tsv:?}");
        assert_eq!(fields[0], "list");
        assert_eq!(fields[1], "3");

        for i in 0..3 {
            let base = 2 + i * 4;
            assert!(
                !fields[base].is_empty(),
                "item {i} text should not be empty"
            );
            assert_eq!(fields[base + 1], "llm", "item {i} source");
            assert_eq!(fields[base + 2], "", "item {i} desc should be empty");
            assert_eq!(fields[base + 3], "command", "item {i} kind");
        }
    }

    #[test]
    fn test_as_str_matches_serde() {
        for source in [SuggestionSource::Llm] {
            let serde_str = serde_json::to_string(&source).unwrap();
            assert_eq!(format!("\"{}\"", source.as_str()), serde_str);
        }
        for kind in [SuggestionKind::Command] {
            let serde_str = serde_json::to_string(&kind).unwrap();
            assert_eq!(format!("\"{}\"", kind.as_str()), serde_str);
        }
    }

    #[test]
    fn test_command_executed_with_cwd() {
        let json = r#"{"type":"command_executed","session_id":"abc","command":"git status","cwd":"/home/user/project"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::CommandExecuted(report) => {
                assert_eq!(report.command, "git status");
                assert_eq!(report.cwd, "/home/user/project");
            }
            _ => panic!("Expected CommandExecuted request"),
        }
    }

    #[test]
    fn test_command_executed_without_cwd() {
        let json = r#"{"type":"command_executed","session_id":"abc","command":"ls"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::CommandExecuted(report) => {
                assert_eq!(report.command, "ls");
                assert_eq!(report.cwd, "");
            }
            _ => panic!("Expected CommandExecuted request"),
        }
    }

    #[test]
    fn test_cwd_changed_deserialization() {
        let json = r#"{"type":"cwd_changed","session_id":"abc","cwd":"/home/user/project"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::CwdChanged(report) => {
                assert_eq!(report.session_id, "abc");
                assert_eq!(report.cwd, "/home/user/project");
            }
            _ => panic!("Expected CwdChanged request"),
        }
    }

    #[test]
    fn test_run_generator_request_deserialization() {
        let json = r#"{"type":"run_generator","command":"git branch --no-color","cwd":"/tmp","strip_prefix":"* "}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::RunGenerator(rg) => {
                assert_eq!(rg.command, "git branch --no-color");
                assert_eq!(rg.cwd, "/tmp");
                assert_eq!(rg.strip_prefix.as_deref(), Some("* "));
                assert!(rg.split_on.is_none());
            }
            _ => panic!("Expected RunGenerator request"),
        }
    }

    #[test]
    fn test_run_generator_request_minimal() {
        let json = r#"{"type":"run_generator","command":"git remote"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::RunGenerator(rg) => {
                assert_eq!(rg.command, "git remote");
                assert_eq!(rg.cwd, "");
                assert!(rg.strip_prefix.is_none());
                assert!(rg.split_on.is_none());
            }
            _ => panic!("Expected RunGenerator request"),
        }
    }
}
