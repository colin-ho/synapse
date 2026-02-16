use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::LlmConfig;
use crate::spec::CommandSpec;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("Invalid TOML in LLM response: {0}")]
    InvalidToml(#[from] toml::de::Error),
    #[error("LLM disabled due to recent API errors (backoff active)")]
    BackoffActive,
}

#[derive(Debug, Clone, Copy)]
enum LlmProvider {
    Anthropic,
    OpenAI,
}

pub struct LlmClient {
    provider: LlmProvider,
    api_key: String,
    model: String,
    max_calls_per_discovery: usize,
    client: Client,
    /// Ensures at most 1 LLM call per second.
    rate_limiter: Mutex<Instant>,
    /// Set on API errors, cleared after 5 minutes.
    backoff_active: AtomicBool,
    backoff_until: Mutex<Option<Instant>>,
    scrub_paths: bool,
}

impl LlmClient {
    /// Construct an LlmClient from config. Returns `None` if disabled or API key is unset.
    pub fn from_config(config: &LlmConfig, scrub_paths: bool) -> Option<Self> {
        if !config.enabled {
            return None;
        }

        let api_key = std::env::var(&config.api_key_env).ok()?;
        if api_key.is_empty() {
            tracing::debug!("LLM disabled: env var {} is empty", config.api_key_env);
            return None;
        }

        let provider = match config.provider.as_str() {
            "anthropic" => LlmProvider::Anthropic,
            "openai" => LlmProvider::OpenAI,
            other => {
                tracing::warn!("Unknown LLM provider '{other}', disabling LLM");
                return None;
            }
        };

        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .ok()?;

        Some(Self {
            provider,
            api_key,
            model: config.model.clone(),
            max_calls_per_discovery: config.max_calls_per_discovery,
            client,
            rate_limiter: Mutex::new(Instant::now() - Duration::from_secs(1)),
            backoff_active: AtomicBool::new(false),
            backoff_until: Mutex::new(None),
            scrub_paths,
        })
    }

    pub fn max_calls_per_discovery(&self) -> usize {
        self.max_calls_per_discovery
    }

    /// Send help text to the LLM and parse the response as a `CommandSpec`.
    pub async fn generate_spec(
        &self,
        command_name: &str,
        help_text: &str,
    ) -> Result<CommandSpec, LlmError> {
        self.check_backoff().await?;
        self.rate_limit().await;

        let help_text = if self.scrub_paths {
            scrub_home_paths(help_text)
        } else {
            help_text.to_string()
        };
        let prompt = build_prompt(command_name, &help_text);

        let result = match self.provider {
            LlmProvider::Anthropic => self.call_anthropic(&prompt, 4096).await,
            LlmProvider::OpenAI => self.call_openai(&prompt, 4096).await,
        };

        match result {
            Ok(response_text) => {
                let toml_text = extract_toml(&response_text);
                let mut spec: CommandSpec = toml::from_str(toml_text)?;
                if spec.name != command_name {
                    spec.name = command_name.to_string();
                }
                Ok(spec)
            }
            Err(e) => {
                if matches!(&e, LlmError::Api { status, .. } if *status == 429 || *status >= 500 || *status == 401 || *status == 403)
                {
                    self.activate_backoff().await;
                }
                Err(e)
            }
        }
    }

    /// Ask the LLM for argument value suggestions and parse up to `max_values` lines.
    pub async fn suggest_argument_values(
        &self,
        prompt: &str,
        max_values: usize,
    ) -> Result<Vec<String>, LlmError> {
        self.check_backoff().await?;
        self.rate_limit().await;

        let result = match self.provider {
            LlmProvider::Anthropic => self.call_anthropic(prompt, 256).await,
            LlmProvider::OpenAI => self.call_openai(prompt, 256).await,
        };

        match result {
            Ok(response_text) => Ok(parse_argument_values(&response_text, max_values)),
            Err(e) => {
                if matches!(&e, LlmError::Api { status, .. } if *status == 429 || *status >= 500 || *status == 401 || *status == 403)
                {
                    self.activate_backoff().await;
                }
                Err(e)
            }
        }
    }

    /// Wait until at least 1 second has passed since the last LLM call.
    async fn rate_limit(&self) {
        let mut last_call = self.rate_limiter.lock().await;
        let elapsed = last_call.elapsed();
        if elapsed < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_secs(1) - elapsed).await;
        }
        *last_call = Instant::now();
    }

    async fn check_backoff(&self) -> Result<(), LlmError> {
        if !self.backoff_active.load(Ordering::Relaxed) {
            return Ok(());
        }
        let guard = self.backoff_until.lock().await;
        if let Some(until) = *guard {
            if Instant::now() >= until {
                drop(guard);
                self.backoff_active.store(false, Ordering::Relaxed);
                return Ok(());
            }
        }
        Err(LlmError::BackoffActive)
    }

    async fn activate_backoff(&self) {
        tracing::warn!("LLM API error, activating 5-minute backoff");
        *self.backoff_until.lock().await = Some(Instant::now() + Duration::from_secs(300));
        self.backoff_active.store(true, Ordering::Relaxed);
    }

    async fn call_anthropic(&self, prompt: &str, max_tokens: u32) -> Result<String, LlmError> {
        let body = AnthropicRequest {
            model: self.model.clone(),
            max_tokens,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
        };

        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api { status, body });
        }

        let parsed: AnthropicResponse = resp.json().await?;
        Ok(parsed
            .content
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default())
    }

    async fn call_openai(&self, prompt: &str, max_tokens: u32) -> Result<String, LlmError> {
        let body = OpenAIRequest {
            model: self.model.clone(),
            messages: vec![OpenAIMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
            max_tokens,
        };

        let resp = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api { status, body });
        }

        let parsed: OpenAIResponse = resp.json().await?;
        Ok(parsed
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default())
    }
}

// --- Anthropic API types ---

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    text: String,
}

// --- OpenAI API types ---

#[derive(Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    max_tokens: u32,
}

#[derive(Serialize)]
struct OpenAIMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
}

#[derive(Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessageResponse,
}

#[derive(Deserialize)]
struct OpenAIMessageResponse {
    content: String,
}

// --- Helpers ---

fn build_prompt(command_name: &str, help_text: &str) -> String {
    format!(
        r#"Parse this CLI help text into a TOML command spec.

Command name: {command_name}

Help text:
```
{help_text}
```

Return ONLY valid TOML matching this schema:

name = "command_name"
description = "..."

[[subcommands]]
name = "subcommand_name"
description = "..."

[[options]]
long = "--flag-name"
short = "-f"            # omit if none
description = "..."
takes_arg = true/false

  [options.arg_generator]          # only if the value is dynamic
  command = "shell command"        # e.g., "git branch --no-color"

[[args]]
name = "arg_name"
description = "..."
template = "file_paths"   # or "directories" if the arg expects dirs

Rules:
- Set takes_arg = true when the option requires a value (indicated by <VALUE>, =VALUE, or uppercase placeholder)
- Set template = "file_paths" when an argument clearly expects file paths (FILE, PATH, FILENAME)
- Set template = "directories" when an argument clearly expects directories (DIR, DIRECTORY)
- Omit --help and --version options
- For subcommand aliases (e.g., "checkout, co"), use: aliases = ["co"]
- Include arg_generator only when you can infer a reliable shell command for dynamic values"#
    )
}

/// Extract TOML from an LLM response that may contain markdown fences.
pub(crate) fn extract_toml(response: &str) -> &str {
    // Try ```toml ... ``` first
    if let Some(start) = response.find("```toml") {
        let content_start = start + "```toml".len();
        if let Some(end) = response[content_start..].find("```") {
            return response[content_start..content_start + end].trim();
        }
    }
    // Try generic ``` ... ```
    if let Some(start) = response.find("```") {
        let content_start = start + "```".len();
        let content_start = if let Some(nl) = response[content_start..].find('\n') {
            content_start + nl + 1
        } else {
            content_start
        };
        if let Some(end) = response[content_start..].find("```") {
            return response[content_start..content_start + end].trim();
        }
    }
    response.trim()
}

pub(crate) fn scrub_home_paths(text: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        text.replace(&home.to_string_lossy().to_string(), "~")
    } else {
        text.to_string()
    }
}

fn parse_argument_values(response: &str, max_values: usize) -> Vec<String> {
    let mut values = Vec::new();
    let mut in_fence = false;

    for raw_line in response.lines() {
        let mut line = raw_line.trim();
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("- ") {
            line = rest.trim();
        } else if let Some((num, rest)) = line.split_once(". ") {
            if num.parse::<usize>().is_ok() {
                line = rest.trim();
            }
        }

        line = line.trim_matches('`').trim();
        line = strip_wrapping_quotes(line);

        if line.is_empty() {
            continue;
        }

        let candidate = line.to_string();
        if !values.contains(&candidate) {
            values.push(candidate);
            if values.len() >= max_values {
                break;
            }
        }
    }

    values
}

fn strip_wrapping_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_toml_with_toml_fence() {
        let response = "Here is the spec:\n```toml\nname = \"test\"\n```\n";
        assert_eq!(extract_toml(response), "name = \"test\"");
    }

    #[test]
    fn test_extract_toml_with_generic_fence() {
        let response = "```\nname = \"test\"\n```";
        assert_eq!(extract_toml(response), "name = \"test\"");
    }

    #[test]
    fn test_extract_toml_without_fences() {
        let response = "name = \"test\"\ndescription = \"A tool\"";
        assert_eq!(extract_toml(response), response);
    }

    #[test]
    fn test_build_prompt_contains_command_name() {
        let prompt = build_prompt("rg", "Usage: rg [OPTIONS] PATTERN [PATH]");
        assert!(prompt.contains("Command name: rg"));
        assert!(prompt.contains("Usage: rg [OPTIONS] PATTERN [PATH]"));
    }

    #[test]
    fn test_scrub_home_paths() {
        if let Some(home) = dirs::home_dir() {
            let text = format!("Usage: tool {}/file.txt", home.display());
            let scrubbed = scrub_home_paths(&text);
            assert!(scrubbed.contains("~/file.txt"));
            assert!(!scrubbed.contains(&home.to_string_lossy().to_string()));
        }
    }

    #[test]
    fn test_from_config_disabled() {
        let config = LlmConfig::default();
        assert!(LlmClient::from_config(&config, false).is_none());
    }

    #[test]
    fn test_parse_argument_values_dedupes_and_strips_markers() {
        let response = r#"
1. "feat: add login endpoint"
2. feat: add login endpoint
- `fix: handle empty response`
```toml
ignored = true
```
"#;

        let values = parse_argument_values(response, 5);
        assert_eq!(
            values,
            vec![
                "feat: add login endpoint".to_string(),
                "fix: handle empty response".to_string()
            ]
        );
    }

    #[test]
    fn test_from_config_unknown_provider() {
        let config = LlmConfig {
            enabled: true,
            provider: "unknown".into(),
            api_key_env: "SYNAPSE_TEST_KEY".into(),
            ..LlmConfig::default()
        };
        // Set the env var so we get past the key check
        unsafe { std::env::set_var("SYNAPSE_TEST_KEY", "test-key") };
        let result = LlmClient::from_config(&config, false);
        unsafe { std::env::remove_var("SYNAPSE_TEST_KEY") };
        assert!(result.is_none());
    }

    #[test]
    fn test_toml_response_parses_to_command_spec() {
        let toml_text = r#"
name = "rg"
description = "Fast line-oriented regex search"

[[subcommands]]
name = "pcre2"
description = "Use PCRE2 regex engine"

[[options]]
long = "--regexp"
short = "-e"
description = "A pattern to search for"
takes_arg = true

[[options]]
long = "--ignore-case"
short = "-i"
description = "Search case insensitively"
takes_arg = false

[[args]]
name = "pattern"
description = "The regex pattern to search for"

[[args]]
name = "path"
description = "Files or directories to search"
template = "file_paths"
"#;
        let spec: CommandSpec = toml::from_str(toml_text).unwrap();
        assert_eq!(spec.name, "rg");
        assert_eq!(spec.subcommands.len(), 1);
        assert_eq!(spec.options.len(), 2);
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--regexp") && o.takes_arg));
        assert_eq!(spec.args.len(), 2);
        assert!(spec
            .args
            .iter()
            .any(|a| a.template == Some(crate::spec::ArgTemplate::FilePaths)));
    }
}
