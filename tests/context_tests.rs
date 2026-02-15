use std::collections::HashMap;
use std::io::Write;

use synapse::config::ContextConfig;
use synapse::protocol::SuggestRequest;
use synapse::providers::context::ContextProvider;
use synapse::providers::SuggestionProvider;

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

#[tokio::test]
async fn test_cargo_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });

    let req = make_request("cargo b", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
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

    let req = make_request("npm run d", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
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

    let req = make_request("make b", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "make build");
}

#[tokio::test]
async fn test_git_branch_detection() {
    let dir = tempfile::tempdir().unwrap();
    let git_dir = dir.path().join(".git");
    std::fs::create_dir(&git_dir).unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/auth\n").unwrap();

    let branch = synapse::providers::context::read_git_branch_pub(dir.path());
    assert_eq!(branch.as_deref(), Some("feature/auth"));
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

    let req = make_request("yarn s", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "yarn start");
}

#[tokio::test]
async fn test_empty_buffer_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();

    let provider = ContextProvider::new(ContextConfig {
        enabled: true,
        scan_depth: 3,
    });

    let req = make_request("", dir.path().to_str().unwrap());
    let result = provider.suggest(&req).await;
    assert!(result.is_none());
}
