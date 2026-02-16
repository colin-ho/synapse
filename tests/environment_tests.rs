use std::collections::HashMap;
use std::num::NonZeroUsize;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
mod common;

#[cfg(unix)]
use synapse::completion_context::Position;
#[cfg(unix)]
use synapse::providers::environment::EnvironmentProvider;
#[cfg(unix)]
use synapse::providers::SuggestionProvider;

#[cfg(unix)]
fn write_executable(dir: &std::path::Path, name: &str) {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\necho ok\n").unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
}

#[cfg(unix)]
fn write_non_executable(dir: &std::path::Path, name: &str) {
    let path = dir.join(name);
    std::fs::write(&path, "not executable\n").unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&path, perms).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn test_environment_suggests_with_prefix_for_pipe_target() {
    let dir = tempfile::tempdir().unwrap();
    write_executable(dir.path(), "synapse_pipe_cmd");

    let mut env_hints = HashMap::new();
    env_hints.insert(
        "PATH".to_string(),
        dir.path().to_string_lossy().into_owned(),
    );

    let provider = EnvironmentProvider::new();
    let req =
        common::make_provider_request_with_env("echo hi | synapse_p", "/tmp", env_hints).await;
    assert_eq!(req.position, Position::PipeTarget);

    let results = provider.suggest(&req, NonZeroUsize::new(10).unwrap()).await;
    assert!(
        results
            .iter()
            .any(|r| r.text == "echo hi | synapse_pipe_cmd"),
        "expected prefixed environment suggestion, got: {:?}",
        results.iter().map(|r| r.text.clone()).collect::<Vec<_>>()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_environment_uses_path_from_env_hints() {
    let dir = tempfile::tempdir().unwrap();
    write_executable(dir.path(), "synapse_only_in_hint_path");

    let mut env_hints = HashMap::new();
    env_hints.insert(
        "PATH".to_string(),
        dir.path().to_string_lossy().into_owned(),
    );

    let provider = EnvironmentProvider::new();
    let req = common::make_provider_request_with_env("synapse_only", "/tmp", env_hints).await;

    let results = provider.suggest(&req, NonZeroUsize::new(10).unwrap()).await;
    assert!(
        results
            .iter()
            .any(|r| r.text == "synapse_only_in_hint_path"),
        "expected command from hinted PATH, got: {:?}",
        results.iter().map(|r| r.text.clone()).collect::<Vec<_>>()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_environment_includes_virtual_env_bin() {
    let path_dir = tempfile::tempdir().unwrap();
    let venv_dir = tempfile::tempdir().unwrap();
    let venv_bin = venv_dir.path().join("bin");
    std::fs::create_dir_all(&venv_bin).unwrap();

    write_executable(path_dir.path(), "synapse_from_path");
    write_executable(&venv_bin, "synapse_from_venv");

    let mut env_hints = HashMap::new();
    env_hints.insert(
        "PATH".to_string(),
        path_dir.path().to_string_lossy().into_owned(),
    );
    env_hints.insert(
        "VIRTUAL_ENV".to_string(),
        venv_dir.path().to_string_lossy().into_owned(),
    );

    let provider = EnvironmentProvider::new();
    let req = common::make_provider_request_with_env("synapse_from_v", "/tmp", env_hints).await;

    let results = provider.suggest(&req, NonZeroUsize::new(10).unwrap()).await;
    assert!(
        results.iter().any(|r| r.text == "synapse_from_venv"),
        "expected command from VIRTUAL_ENV/bin, got: {:?}",
        results.iter().map(|r| r.text.clone()).collect::<Vec<_>>()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_environment_ignores_non_executable_files() {
    let dir = tempfile::tempdir().unwrap();
    write_executable(dir.path(), "synapse_exec_ok");
    write_non_executable(dir.path(), "synapse_exec_no");

    let mut env_hints = HashMap::new();
    env_hints.insert(
        "PATH".to_string(),
        dir.path().to_string_lossy().into_owned(),
    );

    let provider = EnvironmentProvider::new();
    let req = common::make_provider_request_with_env("synapse_exec_", "/tmp", env_hints).await;

    let results = provider.suggest(&req, NonZeroUsize::new(10).unwrap()).await;
    assert!(results.iter().any(|r| r.text == "synapse_exec_ok"));
    assert!(!results.iter().any(|r| r.text == "synapse_exec_no"));
}
