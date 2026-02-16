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

#[tokio::test]
async fn test_max_entries_enforcement() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let entries: Vec<String> = (0..20)
        .map(|i| format!(": {}:0;command_{}", 1000 + i, i))
        .collect();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    for entry in &entries {
        writeln!(file, "{entry}").unwrap();
    }
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 5,
        fuzzy: false,
    });
    provider.load_history().await;

    // Should only keep the 5 most recent entries (timestamps 1015-1019)
    let req = common::make_provider_request("command_", "/tmp").await;
    let results = provider.suggest(&req, 20).await;
    assert!(
        results.len() <= 5,
        "expected at most 5 results, got {}",
        results.len()
    );
    // Most recent entry (command_19, ts=1019) must be present
    assert!(results.iter().any(|r| r.text == "command_19"));
}

#[tokio::test]
async fn test_multiline_command_handling() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, ": 1000:0;echo hello \\").unwrap();
    writeln!(file, "world").unwrap();
    writeln!(file, ": 1001:0;git status").unwrap();
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = common::make_provider_request("echo", "/tmp").await;
    let results = provider.suggest(&req, 5).await;
    assert!(
        results.iter().any(|r| r.text.starts_with("echo hello")),
        "expected multi-line command stored as first line, got: {:?}",
        results.iter().map(|r| r.text.clone()).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_multi_results_ordering() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_extended_history(&[
        (1000, "git status"),
        (1000, "git status"),
        (1000, "git status"),
        (1001, "git stash"),
        (1002, "git switch main"),
    ]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = common::make_provider_request("git s", "/tmp").await;
    let results = provider.suggest(&req, 3).await;
    assert_eq!(results.len(), 3);
    // "git status" should rank highest (frequency 3)
    assert_eq!(results[0].text, "git status");
    // Scores should be descending
    for pair in results.windows(2) {
        assert!(pair[0].score >= pair[1].score);
    }
}
