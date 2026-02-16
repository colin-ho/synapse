use std::sync::Arc;

mod common;

use synapse::completion_context::CompletionContext;
use synapse::config::ContextConfig;
use synapse::config::SpecConfig;
use synapse::providers::context::ContextProvider;
use synapse::providers::SuggestionProvider;
use synapse::spec_store::SpecStore;

#[tokio::test]
async fn test_cargo_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n",
    )
    .unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });

    let req = common::make_suggest_request("cargo b", dir.path().to_str().unwrap());
    let result = provider.suggest(&req, None).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "cargo build");
}

#[tokio::test]
async fn test_package_json_scripts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"scripts": {"dev": "vite", "build": "tsc && vite build", "test": "vitest"}}"#,
    )
    .unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });

    let req = common::make_suggest_request("npm run d", dir.path().to_str().unwrap());
    let result = provider.suggest(&req, None).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "npm run dev");
}

#[tokio::test]
async fn test_package_json_scripts_with_completion_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"scripts": {"dev": "vite", "build": "tsc && vite build"}}"#,
    )
    .unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });
    let store = Arc::new(SpecStore::new(SpecConfig::default()));

    let req = common::make_suggest_request("npm run d", dir.path().to_str().unwrap());
    let ctx = CompletionContext::build(&req.buffer, dir.path(), &store).await;
    let result = provider.suggest(&req, Some(&ctx)).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "npm run dev");
}

#[tokio::test]
async fn test_makefile_targets() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Makefile"),
        "build:\n\tgo build\n\ntest:\n\tgo test\n\nclean:\n\trm -rf bin\n",
    )
    .unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });

    let req = common::make_suggest_request("make b", dir.path().to_str().unwrap());
    let result = provider.suggest(&req, None).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "make build");
}

#[tokio::test]
async fn test_yarn_detection() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"scripts": {"start": "node index.js"}}"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("yarn.lock"), "").unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });

    let req = common::make_suggest_request("yarn s", dir.path().to_str().unwrap());
    let result = provider.suggest(&req, None).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "yarn start");
}

#[tokio::test]
async fn test_empty_buffer_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n",
    )
    .unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });

    let req = common::make_suggest_request("", dir.path().to_str().unwrap());
    let result = provider.suggest(&req, None).await;
    assert!(result.is_none());
}
