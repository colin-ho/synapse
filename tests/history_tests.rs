use std::io::Write;
use std::sync::Mutex;

mod common;

use synapse::completion_context::Position;
use synapse::config::HistoryConfig;
use synapse::providers::history::HistoryProvider;
use synapse::providers::SuggestionProvider;

// Serialize tests that mutate the HISTFILE env var
static HISTFILE_LOCK: Mutex<()> = Mutex::new(());

fn write_history_file(entries: &[&str]) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    for entry in entries {
        writeln!(file, "{entry}").unwrap();
    }
    file
}

fn write_extended_history(entries: &[(u64, &str)]) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    for (ts, cmd) in entries {
        writeln!(file, ": {ts}:0;{cmd}").unwrap();
    }
    file
}

#[tokio::test]
async fn test_simple_prefix_match() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_history_file(&["git status", "git commit -m 'test'", "git push origin main"]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = common::make_provider_request("git s", "/tmp").await;
    let result = provider.suggest(&req, 1).await;
    assert!(!result.is_empty());
    assert_eq!(result[0].text, "git status");
}

#[tokio::test]
async fn test_extended_history_format() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_extended_history(&[
        (1700000000, "docker compose up -d"),
        (1700000100, "docker compose down"),
        (1700000200, "docker ps"),
    ]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = common::make_provider_request("docker c", "/tmp").await;
    let result = provider.suggest(&req, 1).await;
    assert!(!result.is_empty());
    let text = result[0].text.clone();
    assert!(text.starts_with("docker compose"));
}

#[tokio::test]
async fn test_frequency_ranking() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_history_file(&["git status", "git status", "git status", "git stash"]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = common::make_provider_request("git st", "/tmp").await;
    let result = provider.suggest(&req, 1).await;
    assert!(!result.is_empty());
    // "git status" has frequency 3 vs "git stash" with 1
    assert_eq!(result[0].text, "git status");
}

#[tokio::test]
async fn test_fuzzy_match() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_history_file(&["git checkout main", "git commit -m 'fix'"]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: true,
    });
    provider.load_history().await;

    // "git chekout" is misspelled — fuzzy should match "git checkout main"
    let req = common::make_provider_request("git chekout", "/tmp").await;
    let result = provider.suggest(&req, 1).await;
    assert!(!result.is_empty());
    assert!(result[0].text.starts_with("git checkout"));
}

#[tokio::test]
async fn test_empty_buffer_returns_none() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_history_file(&["git status"]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = common::make_provider_request("", "/tmp").await;
    let result = provider.suggest(&req, 1).await;
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_no_match_returns_none() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_history_file(&["git status", "ls -la"]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = common::make_provider_request("zzz_no_match", "/tmp").await;
    let result = provider.suggest(&req, 1).await;
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_suggest_pipe_target_uses_context() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_history_file(&["git status", "git switch main", "ls -la"]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = common::make_provider_request("echo hi | gi", "/tmp").await;
    assert_eq!(req.position, Position::PipeTarget);

    let results = provider.suggest(&req, 5).await;
    assert!(
        results.iter().any(|r| r.text.starts_with("git ")),
        "expected git command suggestion for pipe target, got: {:?}",
        results.iter().map(|r| r.text.clone()).collect::<Vec<_>>()
    );
}
