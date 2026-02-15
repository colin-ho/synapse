use std::collections::HashMap;
use std::sync::Arc;

use synapse::config::SpecConfig;
use synapse::protocol::{SuggestRequest, SuggestionKind, SuggestionSource};
use synapse::providers::spec::SpecProvider;
use synapse::providers::SuggestionProvider;
use synapse::spec::{ArgTemplate, CommandSpec, SubcommandSpec};
use synapse::spec_store::SpecStore;

fn make_request(buffer: &str, cwd: &str) -> SuggestRequest {
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

fn make_spec_provider() -> SpecProvider {
    let config = SpecConfig::default();
    let store = Arc::new(SpecStore::new(config));
    SpecProvider::new(store)
}

// --- Builtin spec loading ---

#[tokio::test]
async fn test_builtin_specs_loaded() {
    let config = SpecConfig::default();
    let store = SpecStore::new(config);
    let dir = tempfile::tempdir().unwrap();
    let names = store.all_command_names(dir.path()).await;
    assert!(names.contains(&"git".to_string()));
    assert!(names.contains(&"cargo".to_string()));
    assert!(names.contains(&"npm".to_string()));
    assert!(names.contains(&"docker".to_string()));
}

#[tokio::test]
async fn test_builtin_spec_lookup() {
    let config = SpecConfig::default();
    let store = SpecStore::new(config);
    let dir = tempfile::tempdir().unwrap();

    let git = store.lookup("git", dir.path()).await;
    assert!(git.is_some());
    let git = git.unwrap();
    assert_eq!(git.name, "git");
    assert!(!git.subcommands.is_empty());
}

// --- Git subcommand completions ---

#[tokio::test]
async fn test_git_subcommand_completion() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = make_request("git co", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_some());
    let suggestion = result.unwrap();
    // Should suggest "git commit" or "git config" (starts with "co")
    assert!(
        suggestion.text.starts_with("git co"),
        "Expected suggestion starting with 'git co', got: {}",
        suggestion.text
    );
    assert_eq!(suggestion.source, SuggestionSource::Spec);
    assert_eq!(suggestion.kind, SuggestionKind::Subcommand);
}

#[tokio::test]
async fn test_git_multi_suggestions() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = make_request("git ", dir.path().to_str().unwrap());
    let results = provider.suggest_multi(&req, 10).await;
    assert!(
        results.len() > 1,
        "Expected multiple suggestions for 'git '"
    );

    // All should be from Spec source
    for r in &results {
        assert_eq!(r.source, SuggestionSource::Spec);
    }
}

#[tokio::test]
async fn test_git_checkout_alias() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    // "git ch" should match both "checkout" and "cherry-pick" etc.
    let req = make_request("git ch", dir.path().to_str().unwrap());
    let results = provider.suggest_multi(&req, 10).await;
    let texts: Vec<&str> = results.iter().map(|r| r.text.as_str()).collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("checkout") || t.contains("cherry-pick")),
        "Expected checkout or cherry-pick in suggestions, got: {:?}",
        texts
    );
}

// --- Cargo subcommand completions ---

#[tokio::test]
async fn test_cargo_subcommand_completion() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = make_request("cargo b", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "cargo build");
}

#[tokio::test]
async fn test_cargo_test_completion() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = make_request("cargo t", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "cargo test");
}

// --- Option completions ---

#[tokio::test]
async fn test_git_commit_option_completion() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = make_request("git commit --m", dir.path().to_str().unwrap());
    let results = provider.suggest_multi(&req, 10).await;
    let texts: Vec<&str> = results.iter().map(|r| r.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("--message")),
        "Expected --message in suggestions, got: {:?}",
        texts
    );
}

// --- Empty buffer ---

#[tokio::test]
async fn test_empty_buffer_returns_none() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = make_request("", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_none());
}

// --- Unknown command ---

#[tokio::test]
async fn test_unknown_command_returns_empty() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = make_request("nonexistent_cmd ", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_none());
}

// --- Spec parsing ---

#[test]
fn test_spec_toml_parsing() {
    let toml_str = r#"
name = "myapp"
description = "A test app"

[[subcommands]]
name = "serve"
description = "Start the server"

[[subcommands.options]]
long = "--port"
short = "-p"
takes_arg = true
description = "Port number"

[[subcommands]]
name = "build"
description = "Build the project"
"#;

    let spec: CommandSpec = toml::from_str(toml_str).unwrap();
    assert_eq!(spec.name, "myapp");
    assert_eq!(spec.subcommands.len(), 2);
    assert_eq!(spec.subcommands[0].name, "serve");
    assert_eq!(spec.subcommands[0].options.len(), 1);
    assert_eq!(
        spec.subcommands[0].options[0].long.as_deref(),
        Some("--port")
    );
    assert!(spec.subcommands[0].options[0].takes_arg);
    assert_eq!(spec.subcommands[1].name, "build");
}

#[test]
fn test_spec_with_aliases() {
    let toml_str = r#"
name = "test"
aliases = ["t", "tst"]

[[subcommands]]
name = "run"
aliases = ["r"]
"#;

    let spec: CommandSpec = toml::from_str(toml_str).unwrap();
    assert_eq!(spec.aliases, vec!["t", "tst"]);
    assert_eq!(spec.subcommands[0].aliases, vec!["r"]);
}

#[test]
fn test_spec_with_arg_template() {
    let toml_str = r#"
name = "cat"

[[args]]
name = "file"
template = "file_paths"
"#;

    let spec: CommandSpec = toml::from_str(toml_str).unwrap();
    assert_eq!(spec.args.len(), 1);
    assert_eq!(spec.args[0].template, Some(ArgTemplate::FilePaths));
}

#[test]
fn test_spec_find_subcommand() {
    let spec = CommandSpec {
        name: "test".into(),
        subcommands: vec![
            SubcommandSpec {
                name: "run".into(),
                aliases: vec!["r".into()],
                ..Default::default()
            },
            SubcommandSpec {
                name: "build".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    assert!(spec.find_subcommand("run").is_some());
    assert!(spec.find_subcommand("r").is_some());
    assert!(spec.find_subcommand("build").is_some());
    assert!(spec.find_subcommand("nonexistent").is_none());
}

// --- Project spec auto-generation ---

#[tokio::test]
async fn test_autogen_cargo_spec() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let config = SpecConfig::default();
    let store = Arc::new(SpecStore::new(config));
    let provider = SpecProvider::new(store);

    let req = make_request("cargo b", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "cargo build");
}

#[tokio::test]
async fn test_autogen_makefile_spec() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Makefile"),
        "build:\n\tgo build\n\ntest:\n\tgo test\n\ndeploy:\n\tgo deploy\n",
    )
    .unwrap();

    let config = SpecConfig::default();
    let store = Arc::new(SpecStore::new(config));
    let provider = SpecProvider::new(store);

    let req = make_request("make d", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "make deploy");
}

// --- suggest_multi ---

#[tokio::test]
async fn test_suggest_multi_truncates() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = make_request("git ", dir.path().to_str().unwrap());
    let results = provider.suggest_multi(&req, 3).await;
    assert!(
        results.len() <= 3,
        "Expected at most 3 results, got {}",
        results.len()
    );
}

// --- Protocol types ---

#[test]
fn test_list_suggestions_response_serialization() {
    use synapse::protocol::{Response, SuggestionItem, SuggestionListResponse};

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
    use synapse::protocol::{ListSuggestionsRequest, Request};

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
    use synapse::protocol::{ListSuggestionsRequest, Request};

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
