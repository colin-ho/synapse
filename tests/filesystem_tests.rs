use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

mod common;

use synapse::providers::filesystem::FilesystemProvider;
use synapse::providers::SuggestionProvider;

#[tokio::test]
async fn test_filesystem_provider_trailing_slash_descends_into_directory() {
    let provider = FilesystemProvider::new();
    let dir = tempfile::tempdir().unwrap();

    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src").join("main.rs"), "fn main() {}\n").unwrap();

    let req = common::make_provider_request("cat src/", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, common::limit(10)).await;
    let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

    assert!(
        texts.iter().any(|t| *t == "cat src/main.rs"),
        "Expected completion from inside src/, got: {:?}",
        texts
    );
    assert!(
        !texts.iter().any(|t| *t == "cat src/src/"),
        "Unexpected duplicated path segment in suggestions: {:?}",
        texts
    );
}

#[tokio::test]
async fn test_filesystem_provider_hides_dotfiles_unless_user_types_dot_prefix() {
    let provider = FilesystemProvider::new();
    let dir = tempfile::tempdir().unwrap();

    fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();
    fs::write(dir.path().join("app.toml"), "name = \"app\"\n").unwrap();

    let req_default = common::make_provider_request("cat ", dir.path().to_str().unwrap()).await;
    let default_results = provider.suggest(&req_default, common::limit(20)).await;
    let default_texts: Vec<&str> = default_results.iter().map(|s| s.text.as_str()).collect();
    assert!(
        !default_texts.iter().any(|t| *t == "cat .env"),
        "Dotfile should not appear without dot prefix, got: {:?}",
        default_texts
    );

    let req_dot = common::make_provider_request("cat .", dir.path().to_str().unwrap()).await;
    let dot_results = provider.suggest(&req_dot, common::limit(20)).await;
    let dot_texts: Vec<&str> = dot_results.iter().map(|s| s.text.as_str()).collect();
    assert!(
        dot_texts.iter().any(|t| *t == "cat .env"),
        "Expected dotfile completion when partial starts with '.', got: {:?}",
        dot_texts
    );
}

#[tokio::test]
async fn test_filesystem_provider_directory_mode_filters_out_files() {
    let provider = FilesystemProvider::new();
    let dir = tempfile::tempdir().unwrap();

    fs::create_dir_all(dir.path().join("logs")).unwrap();
    fs::write(dir.path().join("log.txt"), "log\n").unwrap();

    let req = common::make_provider_request("cd l", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, common::limit(10)).await;
    let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

    assert!(
        texts.iter().any(|t| *t == "cd logs/"),
        "Expected directory suggestion, got: {:?}",
        texts
    );
    assert!(
        !texts.iter().any(|t| *t == "cd log.txt"),
        "File should not appear for directory completions, got: {:?}",
        texts
    );
}

#[tokio::test]
async fn test_filesystem_provider_supports_redirect_context() {
    let provider = FilesystemProvider::new();
    let dir = tempfile::tempdir().unwrap();

    fs::write(dir.path().join("output.txt"), "data\n").unwrap();

    let req = common::make_provider_request("echo hi > out", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, common::limit(10)).await;
    let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

    assert!(
        texts.iter().any(|t| *t == "echo hi > output.txt"),
        "Expected redirect file completion, got: {:?}",
        texts
    );
}

#[tokio::test]
async fn test_filesystem_provider_orders_results_deterministically() {
    let provider = FilesystemProvider::new();
    let dir = tempfile::tempdir().unwrap();

    fs::create_dir_all(dir.path().join("a_dir")).unwrap();
    fs::create_dir_all(dir.path().join("b_dir")).unwrap();
    fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    fs::write(dir.path().join("b.txt"), "b\n").unwrap();

    let req = common::make_provider_request("cat ", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, common::limit(10)).await;

    assert!(
        !results.is_empty(),
        "Expected non-empty filesystem suggestions"
    );
    assert_eq!(
        results[0].text, "cat a_dir/",
        "Expected deterministic ordering by score then text"
    );
}

#[tokio::test]
async fn test_filesystem_provider_escapes_spaces_for_unquoted_input() {
    let provider = FilesystemProvider::new();
    let dir = tempfile::tempdir().unwrap();

    fs::write(dir.path().join("My File.txt"), "x\n").unwrap();

    let req = common::make_provider_request("cat My", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, common::limit(10)).await;
    let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

    assert!(
        texts.iter().any(|t| *t == r"cat My\ File.txt"),
        "Expected escaped-space completion for unquoted input, got: {:?}",
        texts
    );
}

#[tokio::test]
async fn test_filesystem_provider_preserves_double_quote_context() {
    let provider = FilesystemProvider::new();
    let dir = tempfile::tempdir().unwrap();

    fs::write(dir.path().join("My File.txt"), "x\n").unwrap();

    let req = common::make_provider_request(r#"cat "My"#, dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, common::limit(10)).await;
    let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

    assert!(
        texts.iter().any(|t| *t == r#"cat "My File.txt"#),
        "Expected quoted completion to keep spaces unescaped in quote context, got: {:?}",
        texts
    );
}

#[tokio::test]
async fn test_filesystem_provider_preserves_escaped_partial_prefix() {
    let provider = FilesystemProvider::new();
    let dir = tempfile::tempdir().unwrap();

    fs::write(dir.path().join("My File.txt"), "x\n").unwrap();

    let req = common::make_provider_request(r"cat My\ F", dir.path().to_str().unwrap()).await;
    let results = provider.suggest(&req, common::limit(10)).await;
    let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

    assert!(
        texts.iter().any(|t| *t == r"cat My\ File.txt"),
        "Expected completion to extend an escaped partial without breaking prefix, got: {:?}",
        texts
    );
}

#[tokio::test]
async fn test_filesystem_provider_expands_tilde_paths() {
    let provider = FilesystemProvider::new();
    let home = dirs::home_dir().expect("home directory should exist");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let test_dir_name = format!("synapse_fs_test_{unique}");
    let test_dir = home.join(&test_dir_name);
    fs::create_dir_all(&test_dir).unwrap();
    fs::write(test_dir.join("file.txt"), "x\n").unwrap();

    let request_buffer = format!("cat ~/{test_dir_name}/fi");
    let expected = format!("cat ~/{test_dir_name}/file.txt");

    let req = common::make_provider_request(&request_buffer, "/tmp").await;
    let results = provider.suggest(&req, common::limit(10)).await;
    let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

    let _ = fs::remove_dir_all(&test_dir);

    assert!(
        texts.iter().any(|t| *t == expected),
        "Expected tilde-expanded completion, got: {:?}",
        texts
    );
}
