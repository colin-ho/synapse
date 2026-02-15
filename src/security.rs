use crate::config::SecurityConfig;

pub struct Scrubber {
    config: SecurityConfig,
    home_dir: String,
    username: String,
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

        Self {
            config,
            home_dir,
            username,
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

    fn is_sensitive_env_key(&self, key: &str) -> bool {
        let upper = key.to_uppercase();
        self.config.scrub_env_keys.iter().any(|pattern| {
            if let Some(suffix) = pattern.strip_prefix('*') {
                upper.ends_with(&suffix.to_uppercase())
            } else if let Some(prefix) = pattern.strip_suffix('*') {
                upper.starts_with(&prefix.to_uppercase())
            } else {
                upper == pattern.to_uppercase()
            }
        })
    }

    fn is_blocked_command(&self, command: &str) -> bool {
        self.config.command_blocklist.iter().any(|pattern| {
            if pattern.contains('*') {
                // Simple glob: "export *=" matches any command containing "export " followed by "="
                let parts: Vec<&str> = pattern.split('*').collect();
                if parts.len() == 2 {
                    if let Some(pos) = command.find(parts[0]) {
                        return command[pos + parts[0].len()..].contains(parts[1]);
                    }
                }
                false
            } else {
                command.contains(pattern)
            }
        })
    }
}
