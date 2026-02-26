use assert_cmd::cargo::cargo_bin_cmd;

#[test]
fn test_cli_help() {
    cargo_bin_cmd!("synapse").arg("--help").assert().success();
}

#[test]
fn test_run_generator_echo() {
    let output = cargo_bin_cmd!("synapse")
        .args(["run-generator", "echo hello", "--cwd", "/tmp"])
        .output()
        .expect("Failed to run synapse run-generator");

    assert!(output.status.success(), "run-generator should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "Expected 'hello' in output, got: {stdout}"
    );
}

#[test]
fn test_run_generator_multiline() {
    let output = cargo_bin_cmd!("synapse")
        .args([
            "run-generator",
            "printf 'alpha\nbeta\ngamma'",
            "--cwd",
            "/tmp",
        ])
        .output()
        .expect("Failed to run synapse run-generator");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn test_run_generator_strip_prefix() {
    let output = cargo_bin_cmd!("synapse")
        .args([
            "run-generator",
            "printf '* main\n  dev\n  feature'",
            "--cwd",
            "/tmp",
            "--strip-prefix",
            "* ",
        ])
        .output()
        .expect("Failed to run synapse run-generator");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("main"),
        "Expected 'main' after strip_prefix, got: {stdout}"
    );
    assert!(
        stdout.contains("dev"),
        "Expected 'dev' in output, got: {stdout}"
    );
}

#[test]
fn test_run_generator_split_on() {
    let output = cargo_bin_cmd!("synapse")
        .args([
            "run-generator",
            "echo 'a,b,c'",
            "--cwd",
            "/tmp",
            "--split-on",
            ",",
        ])
        .output()
        .expect("Failed to run synapse run-generator");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["a", "b", "c"]);
}

#[test]
fn test_run_generator_failing_command() {
    let output = cargo_bin_cmd!("synapse")
        .args(["run-generator", "false", "--cwd", "/tmp"])
        .output()
        .expect("Failed to run synapse run-generator");

    // Failing generators silently produce no output (exit 0)
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.is_empty(), "Expected empty output, got: {stdout}");
}

#[test]
fn test_translate_no_llm() {
    // With LLM disabled, translate should return an error TSV
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("synapse");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "[llm]\nenabled = false\n").unwrap();

    let output = cargo_bin_cmd!("synapse")
        .args(["translate", "list all files", "--cwd", "/tmp"])
        .env("XDG_CONFIG_HOME", dir.path())
        .output()
        .expect("Failed to run synapse translate");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("error\t"),
        "Expected error TSV, got: {stdout}"
    );
}

#[test]
fn test_translate_query_too_short() {
    let output = cargo_bin_cmd!("synapse")
        .args(["translate", "hi", "--cwd", "/tmp"])
        .output()
        .expect("Failed to run synapse translate");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("error\t"),
        "Expected error TSV for short query, got: {stdout}"
    );
    assert!(
        stdout.contains("too short"),
        "Expected 'too short' message, got: {stdout}"
    );
}

#[test]
fn test_add_blocked_command() {
    // Dangerous commands should be blocked by the discovery blocklist
    let output = cargo_bin_cmd!("synapse")
        .args(["add", "rm"])
        .output()
        .expect("Failed to run synapse add");

    assert!(
        !output.status.success(),
        "Adding 'rm' should fail (blocked)"
    );
}

#[test]
fn test_scan_with_makefile() {
    let dir = tempfile::tempdir().unwrap();
    let output_dir = dir.path().join("completions");
    std::fs::create_dir_all(&output_dir).unwrap();

    // Create a Makefile with some targets
    std::fs::write(
        dir.path().join("Makefile"),
        "build:\n\techo build\ntest:\n\techo test\nclean:\n\techo clean\n",
    )
    .unwrap();

    let output = cargo_bin_cmd!("synapse")
        .args([
            "scan",
            "--output-dir",
            output_dir.to_str().unwrap(),
            "--force",
        ])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run synapse scan");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Generated"),
        "Expected generation report, got: {stdout}"
    );
    assert!(
        stdout.contains("_make"),
        "Expected _make completion, got: {stdout}"
    );
}

#[test]
fn test_scan_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let output_dir = dir.path().join("completions");
    std::fs::create_dir_all(&output_dir).unwrap();

    let output = cargo_bin_cmd!("synapse")
        .args(["scan", "--output-dir", output_dir.to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run synapse scan");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Generated 0"),
        "Expected 0 completions in empty dir, got: {stdout}"
    );
}

#[test]
fn test_init_output_includes_fpath_unconditionally() {
    // Regression: init code used to guard fpath addition with [[ -d ... ]],
    // which broke completions on fresh installs where the directory didn't exist yet.
    let output = cargo_bin_cmd!("synapse")
        .output()
        .expect("Failed to run synapse init");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(r#"fpath=("$HOME/.synapse/completions" $fpath)"#),
        "Expected unconditional fpath addition, got: {stdout}"
    );
    assert!(
        !stdout.contains("[[ -d"),
        "fpath should not be guarded by directory existence check"
    );
}

#[test]
fn test_translate_passes_recent_commands_and_env_hints() {
    // Verify the CLI accepts --recent-command and --env-hint flags without error.
    // We disable LLM so this is just an arg-parsing smoke test.
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("synapse");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "[llm]\nenabled = false\n").unwrap();

    let output = cargo_bin_cmd!("synapse")
        .args([
            "translate",
            "list all files",
            "--cwd",
            "/tmp",
            "--recent-command",
            "ls -la",
            "--recent-command",
            "cd /tmp",
            "--env-hint",
            "PATH=/usr/bin",
            "--env-hint",
            "VIRTUAL_ENV=/venv",
        ])
        .env("XDG_CONFIG_HOME", dir.path())
        .output()
        .expect("Failed to run synapse translate");

    assert!(
        output.status.success(),
        "translate with flags should not crash"
    );
}
