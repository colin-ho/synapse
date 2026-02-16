use std::num::NonZeroUsize;
mod common;

use synapse::config::ContextConfig;
use synapse::providers::context::ContextProvider;
use synapse::providers::SuggestionProvider;

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

    let req = common::make_provider_request("cargo b", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, NonZeroUsize::new(1).unwrap()).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "cargo build");
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

    let req = common::make_provider_request("npm run d", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, NonZeroUsize::new(1).unwrap()).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "npm run dev");
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

    let req = common::make_provider_request("npm run d", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, NonZeroUsize::new(1).unwrap()).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "npm run dev");
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

    let req = common::make_provider_request("make b", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, NonZeroUsize::new(1).unwrap()).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "make build");
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

    let req = common::make_provider_request("yarn s", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, NonZeroUsize::new(1).unwrap()).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "yarn start");
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

    let req = common::make_provider_request("", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, NonZeroUsize::new(1).unwrap()).await;
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_docker_compose_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("docker-compose.yml"),
        "services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n",
    )
    .unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });

    let req = common::make_provider_request("docker compose u", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, NonZeroUsize::new(5).unwrap()).await;
    assert!(!result.is_empty());
    let texts: Vec<&str> = result.iter().map(|s| s.text.as_str()).collect();
    assert!(texts.contains(&"docker compose up"));
    assert!(texts.contains(&"docker compose up -d"));
    assert!(texts.contains(&"docker compose up web"));
}

#[tokio::test]
async fn test_justfile_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("justfile"),
        "build:\n  cargo build\n\ntest:\n  cargo test\n",
    )
    .unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });

    let req = common::make_provider_request("just b", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, NonZeroUsize::new(1).unwrap()).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "just build");
}

#[tokio::test]
async fn test_multi_suggestions_sorted() {
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

    let req = common::make_provider_request("cargo ", dir.path().to_str().unwrap()).await;
    let result = provider.suggest(&req, NonZeroUsize::new(10).unwrap()).await;
    assert!(result.len() > 1);
    // Results should be sorted by score descending
    for w in result.windows(2) {
        assert!(w[0].score >= w[1].score);
    }
}
