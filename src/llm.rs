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
    #[error("Empty response from LLM")]
    EmptyResponse,
}

pub struct NlTranslationContext {
    pub query: String,
    pub cwd: String,
    pub os: String,
    pub project_type: Option<String>,
    pub available_tools: Vec<String>,
    pub recent_commands: Vec<String>,
}

pub struct NlTranslationResult {
    pub command: String,
    pub warning: Option<String>,
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
        let help_text = self.scrub_if_enabled(help_text);
        let prompt = build_prompt(command_name, &help_text);

        let response_text = self.request_completion(&prompt, 4096).await?;
        let toml_text = extract_toml(&response_text);
        let mut spec: CommandSpec = toml::from_str(toml_text)?;
        if spec.name != command_name {
            spec.name = command_name.to_string();
        }
        Ok(spec)
    }

    /// Translate a natural language query into a shell command.
    pub async fn translate_command(
        &self,
        ctx: &NlTranslationContext,
    ) -> Result<NlTranslationResult, LlmError> {
        let cwd = self.scrub_if_enabled(&ctx.cwd);
        let prompt = build_nl_prompt(ctx, &cwd);

        let response_text = self.request_completion(&prompt, 512).await?;
        let command = extract_command(&response_text);
        if command.is_empty() {
            return Err(LlmError::EmptyResponse);
        }
        let warning = detect_destructive_command(&command);
        Ok(NlTranslationResult { command, warning })
    }

    /// Ask the LLM to explain a command.
    pub async fn explain_command(&self, command: &str) -> Result<String, LlmError> {
        let prompt = build_explain_prompt(command);
        let text = self.request_completion(&prompt, 512).await?;
        let explanation = text.trim().to_string();
        if explanation.is_empty() {
            return Err(LlmError::EmptyResponse);
        }
        Ok(explanation)
    }

    /// Predict the next command based on recent command history and context.
    pub async fn predict_workflow(
        &self,
        cwd: &str,
        project_type: Option<&str>,
        recent_commands: &[String],
        last_exit_code: i32,
    ) -> Result<String, LlmError> {
        let cwd_display = self.scrub_if_enabled(cwd);

        let prompt =
            build_workflow_prompt(&cwd_display, project_type, recent_commands, last_exit_code);
        let text = self.request_completion(&prompt, 256).await?;
        Ok(parse_single_shell_line(&text))
    }

    /// Generate a commit message from staged diff content.
    pub async fn generate_commit_message(&self, staged_diff: &str) -> Result<String, LlmError> {
        let diff = self.scrub_if_enabled(staged_diff);

        let prompt = format!(
            "Generate a concise git commit message (one line, max 72 chars) for this diff. \
             Respond with ONLY the commit message, no quotes or explanation.\n\n{diff}"
        );

        let text = self.request_completion(&prompt, 512).await?;
        Ok(text.trim().lines().next().unwrap_or("").to_string())
    }

    /// Enrich a predicted command with contextual arguments.
    pub async fn enrich_command_args(
        &self,
        command: &str,
        recent_commands: &[String],
        cwd: &str,
    ) -> Result<String, LlmError> {
        let cwd_display = self.scrub_if_enabled(cwd);

        let recent = recent_commands
            .iter()
            .take(3)
            .enumerate()
            .map(|(i, cmd)| format!("{}. {}", i + 1, cmd))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "Complete this command with contextually appropriate arguments based on the recent commands.\n\n\
             Working directory: {cwd_display}\n\
             Recent commands:\n{recent}\n\n\
             Command to complete: {command}\n\n\
             Respond with ONLY the complete command (no explanation)."
        );

        let text = self.request_completion(&prompt, 256).await?;
        Ok(parse_single_shell_line(&text))
    }

    /// Ask the LLM for argument value suggestions and parse up to `max_values` lines.
    pub async fn suggest_argument_values(
        &self,
        prompt: &str,
        max_values: usize,
    ) -> Result<Vec<String>, LlmError> {
        let response_text = self.request_completion(prompt, 256).await?;
        Ok(parse_argument_values(&response_text, max_values))
    }

    fn scrub_if_enabled(&self, value: &str) -> String {
        if self.scrub_paths {
            scrub_home_paths(value)
        } else {
            value.to_string()
        }
    }

    async fn request_completion(&self, prompt: &str, max_tokens: u32) -> Result<String, LlmError> {
        self.check_backoff().await?;
        self.rate_limit().await;

        let result = match self.provider {
            LlmProvider::Anthropic => self.call_anthropic(prompt, max_tokens).await,
            LlmProvider::OpenAI => self.call_openai(prompt, max_tokens).await,
        };

        let should_backoff = result
            .as_ref()
            .err()
            .is_some_and(Self::should_activate_backoff);
        if should_backoff {
            self.activate_backoff().await;
        }

        result
    }

    fn should_activate_backoff(error: &LlmError) -> bool {
        matches!(error, LlmError::Api { status, .. } if *status == 429 || *status >= 500 || *status == 401 || *status == 403)
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

    async fn parse_api_response<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
    ) -> Result<T, LlmError> {
        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api { status, body });
        }
        Ok(resp.json().await?)
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

        let parsed: AnthropicResponse = Self::parse_api_response(resp).await?;
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

        let parsed: OpenAIResponse = Self::parse_api_response(resp).await?;
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

fn build_workflow_prompt(
    cwd: &str,
    project_type: Option<&str>,
    recent_commands: &[String],
    last_exit_code: i32,
) -> String {
    let pt = project_type.unwrap_or("unknown");
    let recent = recent_commands
        .iter()
        .take(3)
        .enumerate()
        .map(|(i, cmd)| format!("{}. {}", i + 1, cmd))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "You are a terminal workflow predictor. Given the user's recent commands and context, \
         predict the single most likely next command they will type.\n\n\
         Working directory: {cwd}\n\
         Project type: {pt}\n\
         Recent commands (most recent first):\n{recent}\n\
         Last exit code: {last_exit_code}\n\n\
         Respond with ONLY the complete command (no explanation)."
    )
}

fn build_nl_prompt(ctx: &NlTranslationContext, cwd: &str) -> String {
    let tools_str = if ctx.available_tools.is_empty() {
        "standard POSIX utilities".to_string()
    } else {
        ctx.available_tools.join(", ")
    };

    let recent_str = if ctx.recent_commands.is_empty() {
        "(none)".to_string()
    } else {
        ctx.recent_commands
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    };

    let project_type = ctx.project_type.as_deref().unwrap_or("unknown");

    format!(
        r#"You are a shell command generator. Convert the user's natural language request into a single shell command.

Environment:
- Shell: zsh
- OS: {os}
- Working directory: {cwd}
- Project type: {project_type}
- Available tools: {tools}
- Recent commands:
{recent}

User request: {query}

Rules:
- Return ONLY the shell command, nothing else
- Use tools available on the system (prefer common POSIX utilities)
- Use the working directory context (don't use absolute paths unless necessary)
- If the request is ambiguous, prefer the most common interpretation
- If the request requires multiple commands, chain them with && or |
- Never generate destructive commands (rm -rf /, dd, mkfs) without explicit safeguards
- For file operations, prefer relative paths from the working directory"#,
        os = ctx.os,
        cwd = cwd,
        project_type = project_type,
        tools = tools_str,
        recent = recent_str,
        query = ctx.query,
    )
}

fn build_explain_prompt(command: &str) -> String {
    format!(
        r#"Explain this shell command in 1-2 short sentences. Be concise and specific.

Command: {command}

Rules:
- Explain what it does, not how to use it
- Mention any potentially dangerous effects
- Keep it under 100 words"#,
    )
}

/// Extract the content of a markdown fenced code block, skipping the language tag.
fn extract_fenced_block(text: &str) -> Option<&str> {
    let start = text.find("```")?;
    let after_backticks = start + 3;
    let content_start = if let Some(nl) = text[after_backticks..].find('\n') {
        after_backticks + nl + 1
    } else {
        after_backticks
    };
    let end = text[content_start..].find("```")?;
    Some(text[content_start..content_start + end].trim())
}

/// Extract the shell command from an LLM response.
/// Handles markdown fences, leading/trailing whitespace, and commentary.
fn extract_command(response: &str) -> String {
    let trimmed = response.trim();

    if let Some(block) = extract_fenced_block(trimmed) {
        return block.to_string();
    }

    for line in trimmed.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') && !line.starts_with("//") {
            return line.to_string();
        }
    }

    trimmed.to_string()
}

/// Check if a command contains potentially destructive operations.
/// Returns a warning description if so.
fn detect_destructive_command(command: &str) -> Option<String> {
    let patterns: &[(&str, &str)] = &[
        ("rm ", "deletes files"),
        ("rm\t", "deletes files"),
        ("rmdir ", "removes directories"),
        ("chmod 777", "makes files world-writable"),
        ("chmod -R", "changes permissions recursively"),
        ("dd ", "raw disk write"),
        ("mkfs", "formats filesystem"),
        ("> ", "overwrites file"),
        ("truncate ", "truncates file"),
        ("kill -9", "force-kills process"),
        ("pkill ", "kills processes by name"),
    ];

    for (pattern, description) in patterns {
        if command.contains(pattern) {
            return Some(description.to_string());
        }
    }
    None
}

/// Extract TOML from an LLM response that may contain markdown fences.
pub(crate) fn extract_toml(response: &str) -> &str {
    extract_fenced_block(response).unwrap_or_else(|| response.trim())
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

fn parse_single_shell_line(text: &str) -> String {
    let trimmed = text.trim();
    let content = if trimmed.starts_with('`') && trimmed.ends_with('`') {
        trimmed.trim_matches('`').trim()
    } else {
        trimmed
    };
    content.lines().next().unwrap_or("").to_string()
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
    fn test_extract_command_plain() {
        assert_eq!(
            extract_command("find . -name '*.rs'"),
            "find . -name '*.rs'"
        );
    }

    #[test]
    fn test_extract_command_with_markdown_fence() {
        let response = "```bash\nfind . -name '*.rs'\n```";
        assert_eq!(extract_command(response), "find . -name '*.rs'");
    }

    #[test]
    fn test_extract_command_with_generic_fence() {
        let response = "```\nfind . -name '*.rs'\n```";
        assert_eq!(extract_command(response), "find . -name '*.rs'");
    }

    #[test]
    fn test_extract_command_with_commentary() {
        let response = "# This finds large files\nfind . -type f -size +100M";
        assert_eq!(extract_command(response), "find . -type f -size +100M");
    }

    #[test]
    fn test_detect_destructive_rm() {
        assert_eq!(
            detect_destructive_command("rm -rf /tmp/old"),
            Some("deletes files".into())
        );
    }

    #[test]
    fn test_detect_destructive_safe_command() {
        assert!(detect_destructive_command("find . -name '*.log'").is_none());
    }

    #[test]
    fn test_detect_destructive_chmod_777() {
        assert_eq!(
            detect_destructive_command("chmod 777 file.sh"),
            Some("makes files world-writable".into())
        );
    }

    #[test]
    fn test_detect_destructive_dd() {
        assert_eq!(
            detect_destructive_command("dd if=/dev/zero of=/dev/sda"),
            Some("raw disk write".into())
        );
    }

    #[test]
    fn test_build_nl_prompt_contains_query() {
        let ctx = NlTranslationContext {
            query: "find large files".into(),
            cwd: "/home/user/project".into(),
            os: "macOS 14.5".into(),
            project_type: Some("rust".into()),
            available_tools: vec!["git".into(), "cargo".into()],
            recent_commands: vec!["cargo build".into()],
        };
        let prompt = build_nl_prompt(&ctx, &ctx.cwd);
        assert!(prompt.contains("find large files"));
        assert!(prompt.contains("macOS 14.5"));
        assert!(prompt.contains("rust"));
        assert!(prompt.contains("git, cargo"));
        assert!(prompt.contains("cargo build"));
    }

    #[test]
    fn test_build_nl_prompt_empty_tools() {
        let ctx = NlTranslationContext {
            query: "list files".into(),
            cwd: "/tmp".into(),
            os: "Linux".into(),
            project_type: None,
            available_tools: vec![],
            recent_commands: vec![],
        };
        let prompt = build_nl_prompt(&ctx, &ctx.cwd);
        assert!(prompt.contains("standard POSIX utilities"));
        assert!(prompt.contains("unknown"));
    }

    #[test]
    fn test_build_explain_prompt() {
        let prompt = build_explain_prompt("find . -type f -size +100M");
        assert!(prompt.contains("find . -type f -size +100M"));
        assert!(prompt.contains("Explain"));
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
