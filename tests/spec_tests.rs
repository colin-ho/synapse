use std::sync::Arc;

mod common;

use synapse::config::SpecConfig;
use synapse::protocol::{SuggestionKind, SuggestionSource};
use synapse::providers::spec::SpecProvider;
use synapse::providers::SuggestionProvider;
use synapse::spec_store::SpecStore;

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

    let req = common::make_provider_request("git co", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, 1).await;
    assert!(!result.is_empty());
    let suggestion = &result[0];
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

    let req = common::make_provider_request("git ", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, 10).await;
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
    let req = common::make_provider_request("git ch", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, 10).await;
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

    let req = common::make_provider_request("cargo b", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, 1).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "cargo build");
}

#[tokio::test]
async fn test_cargo_test_completion() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = common::make_provider_request("cargo t", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, 1).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "cargo test");
}

// --- Option completions ---

#[tokio::test]
async fn test_git_commit_option_completion() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = common::make_provider_request("git commit --m", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, 10).await;
    let texts: Vec<&str> = results.iter().map(|r| r.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("--message")),
        "Expected --message in suggestions, got: {:?}",
        texts
    );
}

#[tokio::test]
async fn test_option_arg_generator_while_typing_value() {
    let dir = tempfile::tempdir().unwrap();
    let spec_dir = dir.path().join(".synapse").join("specs");
    std::fs::create_dir_all(&spec_dir).unwrap();
    std::fs::write(
        spec_dir.join("tool.toml"),
        r#"
name = "tool"

[[options]]
long = "--profile"
takes_arg = true
description = "Profile name"

[options.arg_generator]
command = "printf '%s\n' alpha beta"
"#,
    )
    .unwrap();

    let config = SpecConfig {
        auto_generate: false,
        trust_project_generators: true,
        ..SpecConfig::default()
    };
    let store = Arc::new(SpecStore::new(config));
    let provider = SpecProvider::new(store);

    let req = common::make_provider_request("tool --profile a", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, 10).await;
    let texts: Vec<&str> = results.iter().map(|r| r.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| *t == "tool --profile alpha"),
        "Expected option arg generator suggestion, got: {:?}",
        texts
    );
}

// --- Empty buffer ---

#[tokio::test]
async fn test_empty_buffer_returns_none() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = common::make_provider_request("", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, 1).await;
    assert!(result.is_empty());
}

// --- Unknown command ---

#[tokio::test]
async fn test_unknown_command_returns_empty() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = common::make_provider_request("nonexistent_cmd ", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, 1).await;
    assert!(result.is_empty());
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

    let req = common::make_provider_request("cargo b", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, 1).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "cargo build");
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

    let req = common::make_provider_request("make d", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, 1).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "make deploy");
}

#[tokio::test]
async fn test_autogen_spec_from_subdirectory() {
    let dir = tempfile::tempdir().unwrap();
    // Put Cargo.toml at the root and .git to mark project root
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n",
    )
    .unwrap();

    // cwd is a nested subdirectory
    let nested = dir.path().join("src").join("providers");
    std::fs::create_dir_all(&nested).unwrap();

    let config = SpecConfig::default();
    let store = Arc::new(SpecStore::new(config));
    let provider = SpecProvider::new(store);

    let req = common::make_provider_request("cargo b", nested.to_str().unwrap()).await;
    let result = provider.suggest(&req, 1).await;
    assert!(
        !result.is_empty(),
        "Spec autogen should find Cargo.toml from subdirectory via project root walking"
    );
    assert_eq!(result[0].text, "cargo build");
}

// --- suggest max ---

#[tokio::test]
async fn test_suggest_truncates_to_max() {
    let provider = make_spec_provider();
    let dir = tempfile::tempdir().unwrap();

    let req = common::make_provider_request("git ", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, 3).await;
    assert!(
        results.len() <= 3,
        "Expected at most 3 results, got {}",
        results.len()
    );
}
