use std::collections::HashMap;

pub(crate) fn scrub_home_paths(text: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        text.replace(&home.to_string_lossy().to_string(), "~")
    } else {
        text.to_string()
    }
}

/// Redact values of environment variables whose names match the configured
/// `scrub_env_keys` glob patterns (e.g. `*_KEY`, `*_SECRET`).
pub fn scrub_env_values(
    env_hints: &HashMap<String, String>,
    scrub_patterns: &[String],
) -> HashMap<String, String> {
    env_hints
        .iter()
        .map(|(key, value)| {
            if env_key_matches(key, scrub_patterns) {
                (key.clone(), "[REDACTED]".to_string())
            } else {
                (key.clone(), value.clone())
            }
        })
        .collect()
}

fn env_key_matches(key: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            return false;
        }
        if !trimmed.contains('*') && !trimmed.contains('?') {
            return key == trimmed;
        }

        glob_matches(key, trimmed)
    })
}

fn glob_matches(text: &str, pattern: &str) -> bool {
    let regex_pattern = regex::escape(pattern)
        .replace(r"\*", ".*")
        .replace(r"\?", ".");
    regex::Regex::new(&format!("^{regex_pattern}$"))
        .map(|re| re.is_match(text))
        .unwrap_or(false)
}
