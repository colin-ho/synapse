use std::io::Write;
use std::sync::Mutex;

mod common;

use synapse::completion_context::{CompletionContext, Position};
use synapse::config::HistoryConfig;
use synapse::config::SpecConfig;
use synapse::providers::history::HistoryProvider;
use synapse::providers::SuggestionProvider;
use synapse::spec_store::SpecStore;

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

    let req = common::make_suggest_request("git s", "/tmp");
    let result = provider.suggest(&req, None).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().text, "git status");
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

    let req = common::make_suggest_request("docker c", "/tmp");
    let result = provider.suggest(&req, None).await;
    assert!(result.is_some());
    let text = result.unwrap().text;
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

    let req = common::make_suggest_request("git st", "/tmp");
    let result = provider.suggest(&req, None).await;
    assert!(result.is_some());
    // "git status" has frequency 3 vs "git stash" with 1
    assert_eq!(result.unwrap().text, "git status");
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
    let req = common::make_suggest_request("git chekout", "/tmp");
    let result = provider.suggest(&req, None).await;
    assert!(result.is_some());
    assert!(result.unwrap().text.starts_with("git checkout"));
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

    let req = common::make_suggest_request("", "/tmp");
    let result = provider.suggest(&req, None).await;
    assert!(result.is_none());
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

    let req = common::make_suggest_request("zzz_no_match", "/tmp");
    let result = provider.suggest(&req, None).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_suggest_multi_pipe_target_uses_context() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_history_file(&["git status", "git switch main", "ls -la"]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = common::make_suggest_request("echo hi | gi", "/tmp");
    let store = SpecStore::new(SpecConfig::default());
    let ctx = CompletionContext::build(&req.buffer, std::path::Path::new("/tmp"), &store).await;
    assert_eq!(ctx.position, Position::PipeTarget);

    let results = provider.suggest_multi(&req, 5, Some(&ctx)).await;
    assert!(
        results.iter().any(|r| r.text.starts_with("git ")),
        "expected git command suggestion for pipe target, got: {:?}",
        results.iter().map(|r| r.text.clone()).collect::<Vec<_>>()
    );
}
