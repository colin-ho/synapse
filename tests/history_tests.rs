use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;

use synapse::config::HistoryConfig;
use synapse::protocol::SuggestRequest;
use synapse::providers::history::HistoryProvider;
use synapse::providers::SuggestionProvider;

// Serialize tests that mutate the HISTFILE env var
static HISTFILE_LOCK: Mutex<()> = Mutex::new(());

fn make_request(buffer: &str) -> SuggestRequest {
    SuggestRequest {
        session_id: "test".into(),
        buffer: buffer.into(),
        cursor_pos: buffer.len(),
        cwd: "/tmp".into(),
        last_exit_code: 0,
        recent_commands: vec![],
        env_hints: HashMap::new(),
    }
}

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

    let req = make_request("git s");
    let result = provider.suggest(&req).await;
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

    let req = make_request("docker c");
    let result = provider.suggest(&req).await;
    assert!(result.is_some());
    let text = result.unwrap().text;
    assert!(text.starts_with("docker compose"));
}

#[tokio::test]
async fn test_frequency_ranking() {
    let _lock = HISTFILE_LOCK.lock().unwrap();
    let file = write_history_file(&[
        "git status",
        "git status",
        "git status",
        "git stash",
    ]);
    std::env::set_var("HISTFILE", file.path().to_str().unwrap());

    let provider = HistoryProvider::new(HistoryConfig {
        enabled: true,
        max_entries: 50000,
        fuzzy: false,
    });
    provider.load_history().await;

    let req = make_request("git st");
    let result = provider.suggest(&req).await;
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
    let req = make_request("git chekout");
    let result = provider.suggest(&req).await;
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

    let req = make_request("");
    let result = provider.suggest(&req).await;
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

    let req = make_request("zzz_no_match");
    let result = provider.suggest(&req).await;
    assert!(result.is_none());
}

#[test]
fn test_levenshtein_distance() {
    use synapse::providers::history::levenshtein;

    assert_eq!(levenshtein("kitten", "sitting"), 3);
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("abc", "abc"), 0);
    assert_eq!(levenshtein("abc", "abd"), 1);
}
