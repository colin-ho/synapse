use std::path::PathBuf;

use assert_cmd::cargo::cargo_bin_cmd;
use synapse::protocol::{
    CompleteResultItem, CompleteResultResponse, Response, SuggestionItem, SuggestionKind,
    SuggestionListResponse, SuggestionSource,
};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .trim_end_matches('\n')
        .to_string()
}

#[test]
fn test_tsv_list_contract_matches_fixture() {
    let frame = Response::SuggestionList(SuggestionListResponse {
        suggestions: vec![
            SuggestionItem {
                text: "git status".into(),
                source: SuggestionSource::Llm,
                confidence: 0.9,
                description: None,
                kind: SuggestionKind::Command,
            },
            SuggestionItem {
                text: "git stash".into(),
                source: SuggestionSource::Llm,
                confidence: 0.8,
                description: Some("Stash changes".into()),
                kind: SuggestionKind::Command,
            },
        ],
    })
    .to_tsv();

    assert_eq!(frame, fixture("tsv_list.golden"));
}

#[test]
fn test_tsv_complete_result_contract_matches_fixture() {
    let frame = Response::CompleteResult(CompleteResultResponse {
        values: vec![
            CompleteResultItem {
                value: "--help".into(),
                description: Some("show help".into()),
            },
            CompleteResultItem {
                value: "-v".into(),
                description: Some("verbose".into()),
            },
        ],
    })
    .to_tsv();

    assert_eq!(frame, fixture("tsv_complete_result.golden"));
}

#[test]
fn test_tsv_error_contract_matches_fixture() {
    let frame = Response::Error {
        message: "bad request".into(),
    }
    .to_tsv();

    assert_eq!(frame, fixture("tsv_error.golden"));
}

#[test]
fn test_cli_status_contract_when_pid_missing() {
    cargo_bin_cmd!("synapse")
        .args([
            "status",
            "--socket-path",
            "/tmp/synapse-contract-missing.sock",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "Daemon is not running (no PID file)",
        ));
}

#[test]
fn test_cli_stop_contract_when_pid_missing() {
    cargo_bin_cmd!("synapse")
        .args([
            "stop",
            "--socket-path",
            "/tmp/synapse-contract-missing.sock",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "Daemon is not running (no PID file)",
        ));
}
