use std::collections::HashMap;
use synapse::config::SecurityConfig;
use synapse::security::Scrubber;

fn default_scrubber() -> Scrubber {
    Scrubber::new(SecurityConfig {
        scrub_paths: true,
        scrub_env_keys: vec![
            "*_KEY".into(),
            "*_SECRET".into(),
            "*_TOKEN".into(),
            "*_PASSWORD".into(),
            "*_CREDENTIALS".into(),
        ],
        command_blocklist: vec![
            "export *=".into(),
            "curl -u".into(),
            r#"curl -H "Authorization""#.into(),
        ],
    })
}

#[test]
fn test_path_scrubbing() {
    let scrubber = default_scrubber();

    let home = dirs::home_dir().unwrap();
    let home_str = home.to_string_lossy();

    let path = format!("{home_str}/projects/myapp");
    let scrubbed = scrubber.scrub_path(&path);
    assert!(scrubbed.starts_with("~"), "Expected ~ prefix, got: {scrubbed}");
    assert!(scrubbed.contains("projects/myapp"));
}

#[test]
fn test_path_scrubbing_disabled() {
    let scrubber = Scrubber::new(SecurityConfig {
        scrub_paths: false,
        scrub_env_keys: vec![],
        command_blocklist: vec![],
    });

    let home = dirs::home_dir().unwrap();
    let path = format!("{}/projects/myapp", home.display());
    let scrubbed = scrubber.scrub_path(&path);
    assert_eq!(scrubbed, path);
}

#[test]
fn test_env_key_filtering() {
    let scrubber = default_scrubber();

    let mut env = HashMap::new();
    env.insert("NODE_ENV".into(), "development".into());
    env.insert("API_KEY".into(), "secret123".into());
    env.insert("DATABASE_PASSWORD".into(), "pass".into());
    env.insert("HOME".into(), "/home/user".into());
    env.insert("AWS_SECRET".into(), "xxx".into());

    let filtered = scrubber.scrub_env_hints(&env);
    assert!(filtered.contains_key("NODE_ENV"));
    assert!(filtered.contains_key("HOME"));
    assert!(!filtered.contains_key("API_KEY"));
    assert!(!filtered.contains_key("DATABASE_PASSWORD"));
    assert!(!filtered.contains_key("AWS_SECRET"));
}

#[test]
fn test_command_blocklist() {
    let scrubber = default_scrubber();

    let commands = vec![
        "git status".into(),
        "export API_KEY=secret123".into(),
        "curl -u user:pass https://example.com".into(),
        "ls -la".into(),
        r#"curl -H "Authorization" https://api.example.com"#.into(),
    ];

    let filtered = scrubber.scrub_commands(&commands);
    assert_eq!(filtered.len(), 2);
    assert!(filtered.contains(&"git status".to_string()));
    assert!(filtered.contains(&"ls -la".to_string()));
}

#[test]
fn test_export_glob_pattern() {
    let scrubber = default_scrubber();

    let commands = vec![
        "export FOO=bar".into(),
        "export PATH=/usr/bin".into(),
        "echo hello".into(),
    ];

    let filtered = scrubber.scrub_commands(&commands);
    // Both exports should be filtered
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0], "echo hello");
}
