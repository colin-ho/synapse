use crate::config::SecurityConfig;
use globset::{Glob, GlobSet, GlobSetBuilder};

pub struct Scrubber {
    config: SecurityConfig,
    home_dir: String,
    username: String,
    env_key_matcher: Option<GlobSet>,
    command_matcher: Option<GlobSet>,
}

impl Scrubber {
    pub fn new(config: SecurityConfig) -> Self {
        let home_dir = dirs::home_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let env_key_matcher = Self::build_env_key_matcher(&config.scrub_env_keys);
        let command_matcher = Self::build_command_matcher(&config.command_blocklist);

        Self {
            config,
            home_dir,
            username,
            env_key_matcher,
            command_matcher,
        }
    }

    pub fn scrub_path(&self, path: &str) -> String {
        if !self.config.scrub_paths {
            return path.to_string();
        }

        let mut result = path.to_string();

        // Replace home directory with ~
        if !self.home_dir.is_empty() {
            result = result.replace(&self.home_dir, "~");
        }

        // Strip username from paths
        if !self.username.is_empty() {
            result = result.replace(&self.username, "<user>");
        }

        result
    }

    #[allow(dead_code)]
    pub fn scrub_env_hints(
        &self,
        env_hints: &std::collections::HashMap<String, String>,
    ) -> std::collections::HashMap<String, String> {
        env_hints
            .iter()
            .filter(|(key, _)| !self.is_sensitive_env_key(key))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn scrub_commands(&self, commands: &[String]) -> Vec<String> {
        commands
            .iter()
            .filter(|cmd| !self.is_blocked_command(cmd))
            .cloned()
            .collect()
    }

    fn simple_wildcard_to_glob(pattern: &str) -> String {
        let mut escaped = String::new();
        for ch in pattern.chars() {
            if ch == '*' {
                escaped.push('*');
            } else {
                escaped.push_str(&globset::escape(&ch.to_string()));
            }
        }
        escaped
    }

    fn build_globset(patterns: Vec<String>) -> Option<GlobSet> {
        let mut builder = GlobSetBuilder::new();
        let mut has_patterns = false;

        for pattern in patterns {
            match Glob::new(&pattern) {
                Ok(glob) => {
                    builder.add(glob);
                    has_patterns = true;
                }
                Err(e) => {
                    tracing::warn!("Ignoring invalid security pattern '{pattern}': {e}");
                }
            }
        }

        if !has_patterns {
            return None;
        }

        match builder.build() {
            Ok(set) => Some(set),
            Err(e) => {
                tracing::warn!("Failed to build security matcher set: {e}");
                None
            }
        }
    }

    fn build_env_key_matcher(patterns: &[String]) -> Option<GlobSet> {
        let globs = patterns
            .iter()
            .map(|pattern| Self::simple_wildcard_to_glob(&pattern.to_uppercase()))
            .collect();
        Self::build_globset(globs)
    }

    fn build_command_matcher(patterns: &[String]) -> Option<GlobSet> {
        let globs = patterns
            .iter()
            .map(|pattern| {
                let p = Self::simple_wildcard_to_glob(pattern);
                format!("*{p}*")
            })
            .collect();
        Self::build_globset(globs)
    }

    fn is_sensitive_env_key(&self, key: &str) -> bool {
        let upper = key.to_uppercase();
        self.env_key_matcher
            .as_ref()
            .is_some_and(|matcher| matcher.is_match(upper))
    }

    fn is_blocked_command(&self, command: &str) -> bool {
        self.command_matcher
            .as_ref()
            .is_some_and(|matcher| matcher.is_match(command))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::SecurityConfig;

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
        assert!(
            scrubbed.starts_with("~"),
            "Expected ~ prefix, got: {scrubbed}"
        );
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
}
