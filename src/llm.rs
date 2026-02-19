use std::collections::HashMap;
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
    pub git_branch: Option<String>,
    /// Project commands: e.g. {"make": ["build","test"], "npm run": ["dev","lint"]}
    pub project_commands: HashMap<String, Vec<String>>,
    /// Top-level entries in the working directory.
    pub cwd_entries: Vec<String>,
    /// Known flags for tools mentioned in the query.
    pub relevant_specs: HashMap<String, Vec<String>>,
    /// Few-shot examples from accepted interaction history: (query, command).
    pub few_shot_examples: Vec<(String, String)>,
}

pub struct NlTranslationItem {
    pub command: String,
    pub warning: Option<String>,
}

pub struct NlTranslationResult {
    pub items: Vec<NlTranslationItem>,
}

pub struct LlmClient {
    api_key: String,
    base_url: Option<String>,
    model: String,
    max_calls_per_discovery: usize,
    client: Client,
    /// Minimum interval between LLM calls.
    rate_limiter: Mutex<Instant>,
    rate_limit_duration: Duration,
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

        if config.provider != "openai" {
            tracing::warn!(
                "Unsupported LLM provider '{}' (only 'openai' is supported), disabling LLM",
                config.provider
            );
            return None;
        }

        let base_url = config
            .base_url
            .as_deref()
            .map(str::trim)
            .map(|v| v.trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty());

        let api_key = match std::env::var(&config.api_key_env) {
            Ok(v) if !v.is_empty() => v,
            _ => {
                // For local OpenAI-compatible endpoints (LM Studio, etc.), allow a placeholder.
                if base_url.as_deref().is_some_and(is_local_base_url) {
                    tracing::info!(
                        "LLM key env {} missing; using placeholder key for local endpoint",
                        config.api_key_env
                    );
                    "lm-studio".to_string()
                } else {
                    tracing::debug!("LLM disabled: env var {} is empty", config.api_key_env);
                    return None;
                }
            }
        };

        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .ok()?;

        Some(Self {
            api_key,
            base_url,
            model: config.model.clone(),
            max_calls_per_discovery: config.max_calls_per_discovery,
            client,
            rate_limit_duration: Duration::from_millis(crate::config::RATE_LIMIT_MS),
            rate_limiter: Mutex::new(Instant::now() - Duration::from_secs(1)),
            backoff_active: AtomicBool::new(false),
            backoff_until: Mutex::new(None),
            scrub_paths,
        })
    }

    pub fn max_calls_per_discovery(&self) -> usize {
        self.max_calls_per_discovery
    }

    /// For local OpenAI-compatible endpoints, query /v1/models to auto-detect the loaded model.
    /// If the configured model is in the list, keeps it. Otherwise switches to the first
    /// available model. Skips non-local endpoints entirely.
    pub async fn auto_detect_model(&mut self) -> Option<String> {
        let base = self.base_url.as_deref()?;
        if !is_local_base_url(base) {
            return None;
        }

        let models_url = url_with_v1_path(base, "models");
        let detect_client = Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .ok()?;

        let resp = match detect_client.get(&models_url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Model auto-detect: failed to reach {models_url}: {e}");
                return None;
            }
        };

        let models_resp: ModelsResponse = match resp.json().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Model auto-detect: failed to parse /v1/models response: {e}");
                return None;
            }
        };

        if models_resp.data.is_empty() {
            tracing::warn!("Model auto-detect: no models loaded at {models_url}");
            return None;
        }

        // Check if configured model is in the list
        if models_resp.data.iter().any(|m| m.id == self.model) {
            tracing::info!(
                "Model auto-detect: configured model '{}' is available",
                self.model
            );
            return Some(self.model.clone());
        }

        // Use first available model
        let new_model = models_resp.data[0].id.clone();
        tracing::info!(
            "Model auto-detect: configured '{}' not found, switching to '{}'",
            self.model,
            new_model
        );
        self.model = new_model.clone();
        Some(new_model)
    }

    /// Startup health check: verify the LLM endpoint is reachable.
    /// Returns true if any response is received (even errors like 405).
    /// Does NOT activate backoff — this is a one-time startup check.
    pub async fn probe_health(&self) -> bool {
        // For OpenAI-compatible endpoints, use GET /v1/models which is a standard
        // lightweight read-only endpoint. HEAD on /v1/chat/completions causes
        // spurious errors in LM Studio and similar local servers.
        let url = url_with_v1_path(
            self.base_url.as_deref().unwrap_or("https://api.openai.com"),
            "models",
        );

        let health_client = match Client::builder().timeout(Duration::from_secs(3)).build() {
            Ok(c) => c,
            Err(_) => return false,
        };

        let result = health_client.get(&url).send().await;

        match result {
            Ok(_) => {
                tracing::info!("LLM health check: endpoint reachable at {url}");
                true
            }
            Err(e) => {
                tracing::warn!(
                    "LLM health check: endpoint unreachable at {url}: {e} — LLM features will be unavailable until the endpoint comes online"
                );
                false
            }
        }
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

    /// Translate a natural language query into one or more shell commands.
    pub async fn translate_command(
        &self,
        ctx: &NlTranslationContext,
        max_suggestions: usize,
        temperature: f32,
    ) -> Result<NlTranslationResult, LlmError> {
        let cwd = self.scrub_if_enabled(&ctx.cwd);
        let (system_prompt, user_prompt) = build_nl_prompt(ctx, &cwd, max_suggestions);

        let messages = vec![
            OpenAIMessage {
                role: "system".to_string(),
                content: system_prompt,
            },
            OpenAIMessage {
                role: "user".to_string(),
                content: user_prompt,
            },
        ];

        let max_tokens = (max_suggestions as u32 * 80).max(512);
        let response_text = self
            .request_completion_raw(messages, max_tokens, Some(temperature))
            .await?;
        let commands = extract_commands(&response_text, max_suggestions);
        if commands.is_empty() {
            return Err(LlmError::EmptyResponse);
        }
        let items = commands
            .into_iter()
            .map(|cmd| {
                let warning = detect_destructive_command(&cmd);
                NlTranslationItem {
                    command: cmd,
                    warning,
                }
            })
            .collect();
        Ok(NlTranslationResult { items })
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
        let messages = vec![OpenAIMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
        }];
        self.request_completion_raw(messages, max_tokens, None)
            .await
    }

    async fn request_completion_raw(
        &self,
        messages: Vec<OpenAIMessage>,
        max_tokens: u32,
        temperature: Option<f32>,
    ) -> Result<String, LlmError> {
        self.check_backoff().await?;
        self.rate_limit().await;

        let result = self.call_openai(messages, max_tokens, temperature).await;

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
        // Only backoff on transient errors (rate-limit, server errors).
        // Auth errors (401/403) are permanent config problems — return them immediately
        // so the user can fix their setup instead of waiting out a 5-minute backoff.
        matches!(error, LlmError::Api { status, .. } if *status == 429 || *status >= 500)
    }

    /// Wait until the configured rate limit interval has passed since the last LLM call.
    async fn rate_limit(&self) {
        let mut last_call = self.rate_limiter.lock().await;
        let elapsed = last_call.elapsed();
        if elapsed < self.rate_limit_duration {
            tokio::time::sleep(self.rate_limit_duration - elapsed).await;
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

    async fn call_openai(
        &self,
        messages: Vec<OpenAIMessage>,
        max_tokens: u32,
        temperature: Option<f32>,
    ) -> Result<String, LlmError> {
        let body = OpenAIRequest {
            model: self.model.clone(),
            messages,
            max_tokens,
            temperature,
        };

        let resp = self
            .client
            .post(self.openai_chat_completions_url())
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

    fn openai_chat_completions_url(&self) -> String {
        match self.base_url.as_deref() {
            Some(base) => url_with_v1_path(base, "chat/completions"),
            None => "https://api.openai.com/v1/chat/completions".to_string(),
        }
    }
}

// --- OpenAI API types ---

#[derive(Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize, Clone)]
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

// --- Models endpoint types (for auto-detection) ---

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
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

/// Build NL translation prompt as (system_message, user_message).
pub fn build_nl_prompt(
    ctx: &NlTranslationContext,
    cwd: &str,
    max_suggestions: usize,
) -> (String, String) {
    // --- System prompt: behavioral rules and output format ---
    let system = if max_suggestions <= 1 {
        "You are a shell command generator. Convert the user's natural language request into a single shell command.\n\n\
         Rules:\n\
         - Return ONLY the shell command, nothing else\n\
         - Use tools available on the system (prefer common POSIX utilities)\n\
         - Use the working directory context (don't use absolute paths unless necessary)\n\
         - If the request is ambiguous, prefer the most common interpretation\n\
         - If the request requires multiple commands, chain them with && or |\n\
         - Never generate destructive commands (rm -rf /, dd, mkfs) without explicit safeguards\n\
         - For file operations, prefer relative paths from the working directory".to_string()
    } else {
        format!(
            "You are a shell command generator. Convert the user's natural language request into {n} alternative shell commands, ranked from most likely to least likely.\n\n\
             Rules:\n\
             - Return up to {n} alternative commands, one per line, numbered 1. 2. 3. etc.\n\
             - Each line must contain ONLY the number and shell command (no explanations)\n\
             - Vary the approaches: use different tools, flags, or techniques for each alternative\n\
             - Rank from most likely correct interpretation to least likely\n\
             - Use tools available on the system (prefer common POSIX utilities)\n\
             - Use the working directory context (don't use absolute paths unless necessary)\n\
             - If the request requires multiple commands, chain them with && or |\n\
             - Never generate destructive commands (rm -rf /, dd, mkfs) without explicit safeguards\n\
             - For file operations, prefer relative paths from the working directory",
            n = max_suggestions,
        )
    };

    // --- User prompt: environment context + query ---
    let mut user = String::with_capacity(1024);
    user.push_str("Environment:\n");
    user.push_str("- Shell: zsh\n");
    user.push_str(&format!("- OS: {}\n", ctx.os));
    user.push_str(&format!("- Working directory: {cwd}\n"));
    user.push_str(&format!(
        "- Project type: {}\n",
        ctx.project_type.as_deref().unwrap_or("unknown")
    ));

    if let Some(ref branch) = ctx.git_branch {
        user.push_str(&format!("- Git branch: {branch}\n"));
    }

    if ctx.available_tools.is_empty() {
        user.push_str("- Available tools: standard POSIX utilities\n");
    } else {
        user.push_str(&format!(
            "- Available tools: {}\n",
            ctx.available_tools.join(", ")
        ));
    }

    if !ctx.project_commands.is_empty() {
        user.push_str("- Project commands:\n");
        for (runner, commands) in &ctx.project_commands {
            let cmds: Vec<_> = commands.iter().take(10).cloned().collect();
            user.push_str(&format!("  {runner}: {}\n", cmds.join(", ")));
        }
    }

    if !ctx.cwd_entries.is_empty() {
        let entries: Vec<_> = ctx.cwd_entries.iter().take(50).cloned().collect();
        user.push_str(&format!("- Files in cwd: {}\n", entries.join(", ")));
    }

    if !ctx.relevant_specs.is_empty() {
        for (tool, flags) in &ctx.relevant_specs {
            let flags_str: Vec<_> = flags.iter().take(20).cloned().collect();
            user.push_str(&format!(
                "- Known flags for `{tool}`: {}\n",
                flags_str.join(", ")
            ));
        }
    }

    if ctx.recent_commands.is_empty() {
        user.push_str("- Recent commands: (none)\n");
    } else {
        user.push_str("- Recent commands:\n");
        for cmd in ctx.recent_commands.iter().take(5) {
            user.push_str(&format!("{cmd}\n"));
        }
    }

    // Few-shot examples
    if !ctx.few_shot_examples.is_empty() {
        user.push_str("\nExamples of commands you've previously generated:\n");
        for (q, a) in ctx.few_shot_examples.iter().take(5) {
            user.push_str(&format!("Q: \"{q}\"\nA: {a}\n\n"));
        }
    }

    user.push_str(&format!("\nUser request: {}", ctx.query));

    (system, user)
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

/// Extract multiple shell commands from an LLM response.
/// Handles numbered lists, bullets, markdown fences, and bare commands.
pub fn extract_commands(response: &str, max: usize) -> Vec<String> {
    let commands = parse_unique_lines(response, max, FenceMode::PreferFirstFence, true, false);

    // Fallback: if parsing found nothing, try the old single-command extraction
    if commands.is_empty() {
        let single = extract_command(response);
        if single.is_empty() {
            Vec::new()
        } else {
            vec![single]
        }
    } else {
        commands
    }
}

/// Check if a command contains potentially destructive operations.
/// Returns a warning description if so.
///
/// Uses simple substring matching — these are user-facing warnings, not security gates
/// (the blocklist handles actual blocking). Simple checks are more robust and catch
/// cases like `sudo rm` that position-anchored regexes would miss.
pub fn detect_destructive_command(command: &str) -> Option<String> {
    let patterns: &[(&str, &str)] = &[
        ("rm ", "deletes files"),
        ("rmdir ", "removes directories"),
        ("dd ", "raw disk write"),
        ("mkfs", "formats filesystem"),
        ("truncate ", "truncates file"),
        ("shred ", "overwrites file data"),
        ("pkill ", "kills processes by name"),
        ("chmod 777", "makes files world-writable"),
        ("chmod -R", "changes permissions recursively"),
        ("kill -9", "force-kills process"),
        ("-delete", "deletes files (find -delete)"),
    ];

    for (pattern, description) in patterns {
        if command.contains(pattern) {
            return Some(description.to_string());
        }
    }

    // Detect truncating redirects: `> file` but not `>>` (append).
    // Look for `> ` not preceded by another `>`.
    if let Some(pos) = command.find("> ") {
        if pos == 0 || command.as_bytes()[pos - 1] != b'>' {
            return Some("overwrites file".to_string());
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

/// Redact values of environment variables whose names match the configured
/// `scrub_env_keys` glob patterns (e.g. `*_KEY`, `*_SECRET`).
pub fn scrub_env_values(
    env_hints: &std::collections::HashMap<String, String>,
    scrub_patterns: &[String],
) -> std::collections::HashMap<String, String> {
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
        // Simple glob: * matches any span, ? matches one character
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

fn parse_argument_values(response: &str, max_values: usize) -> Vec<String> {
    parse_unique_lines(
        response,
        max_values,
        FenceMode::SkipFencedBlocks,
        false,
        true,
    )
}

#[derive(Clone, Copy)]
enum FenceMode {
    PreferFirstFence,
    SkipFencedBlocks,
}

fn parse_unique_lines(
    response: &str,
    max_values: usize,
    fence_mode: FenceMode,
    skip_comments: bool,
    strip_quotes: bool,
) -> Vec<String> {
    let trimmed = response.trim();
    let content = match fence_mode {
        FenceMode::PreferFirstFence => extract_fenced_block(trimmed).unwrap_or(trimmed),
        FenceMode::SkipFencedBlocks => trimmed,
    };

    let mut values = Vec::new();
    let mut in_fence = false;

    for raw_line in content.lines() {
        let mut line = raw_line.trim();

        if matches!(fence_mode, FenceMode::SkipFencedBlocks) && line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || line.is_empty() || line.starts_with("```") {
            continue;
        }

        line = strip_list_marker(line).trim_matches('`').trim();
        if strip_quotes {
            line = strip_wrapping_quotes(line);
        }
        if skip_comments && (line.starts_with('#') || line.starts_with("//")) {
            continue;
        }
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

fn strip_list_marker(line: &str) -> &str {
    let line = if let Some(rest) = line.strip_prefix("- ") {
        rest.trim()
    } else {
        line
    };
    strip_numeric_prefix(line).trim()
}

fn strip_numeric_prefix(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' ' {
        &line[i + 2..]
    } else {
        line
    }
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

fn url_with_v1_path(base_url: &str, suffix: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let suffix = suffix.trim_start_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/{suffix}")
    } else {
        format!("{base}/v1/{suffix}")
    }
}

fn is_local_base_url(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    let host_part = lower.split_once("://").map(|(_, r)| r).unwrap_or(&lower);
    host_part.starts_with("127.0.0.1")
        || host_part.starts_with("localhost")
        || host_part.starts_with("[::1]")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static LLM_ENV_LOCK: Mutex<()> = Mutex::new(());

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
        let config = LlmConfig {
            enabled: false,
            ..LlmConfig::default()
        };
        assert!(LlmClient::from_config(&config, false).is_none());
    }

    #[test]
    fn test_from_config_defaults() {
        let config = LlmConfig::default();
        assert!(config.enabled);
        assert_eq!(config.provider, "openai");
        assert_eq!(config.api_key_env, "LMSTUDIO_API_KEY");
        assert_eq!(config.base_url, Some("http://127.0.0.1:1234".to_string()));
        // Local endpoint should allow placeholder key even when env var is missing.
        let client = LlmClient::from_config(&config, false)
            .expect("default local config should create client");
        assert_eq!(client.api_key, "lm-studio");
    }

    #[test]
    fn test_from_config_lmstudio() {
        let config = LlmConfig {
            enabled: true,
            provider: "openai".into(),
            api_key_env: "LMSTUDIO_API_KEY".into(),
            base_url: Some("http://127.0.0.1:1234".into()),
            model: "qwen2.5-coder-7b-instruct-mlx".into(),
            ..Default::default()
        };
        // Local endpoint should allow placeholder key
        let client =
            LlmClient::from_config(&config, false).expect("LM Studio config should create client");
        assert_eq!(client.api_key, "lm-studio");
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
        let _guard = LLM_ENV_LOCK.lock().unwrap();
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
    fn test_from_config_local_openai_without_api_key_uses_placeholder() {
        let _guard = LLM_ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("SYNAPSE_TEST_LOCAL_KEY") };

        let config = LlmConfig {
            enabled: true,
            provider: "openai".into(),
            api_key_env: "SYNAPSE_TEST_LOCAL_KEY".into(),
            base_url: Some("http://127.0.0.1:1234".into()),
            ..LlmConfig::default()
        };

        let client = LlmClient::from_config(&config, false)
            .expect("local openai-compatible endpoint should allow placeholder key");
        assert_eq!(client.api_key, "lm-studio");
        assert_eq!(
            client.openai_chat_completions_url(),
            "http://127.0.0.1:1234/v1/chat/completions"
        );
    }

    #[test]
    fn test_from_config_non_local_without_api_key_is_none() {
        let _guard = LLM_ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("SYNAPSE_TEST_REMOTE_KEY") };

        let config = LlmConfig {
            enabled: true,
            provider: "openai".into(),
            api_key_env: "SYNAPSE_TEST_REMOTE_KEY".into(),
            base_url: Some("https://api.openai.com".into()),
            ..LlmConfig::default()
        };

        assert!(LlmClient::from_config(&config, false).is_none());
    }

    #[test]
    fn test_url_with_v1_path() {
        assert_eq!(
            url_with_v1_path("http://127.0.0.1:1234", "chat/completions"),
            "http://127.0.0.1:1234/v1/chat/completions"
        );
        assert_eq!(
            url_with_v1_path("http://127.0.0.1:1234/v1", "chat/completions"),
            "http://127.0.0.1:1234/v1/chat/completions"
        );
    }

    #[test]
    fn test_is_local_base_url() {
        assert!(is_local_base_url("http://127.0.0.1:1234"));
        assert!(is_local_base_url("http://localhost:1234"));
        assert!(is_local_base_url("http://[::1]:1234"));
        assert!(!is_local_base_url("https://api.openai.com"));
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

    fn test_ctx(query: &str) -> NlTranslationContext {
        NlTranslationContext {
            query: query.into(),
            cwd: "/home/user/project".into(),
            os: "macOS 14.5".into(),
            project_type: Some("rust".into()),
            available_tools: vec!["git".into(), "cargo".into()],
            recent_commands: vec!["cargo build".into()],
            git_branch: None,
            project_commands: HashMap::new(),
            cwd_entries: Vec::new(),
            relevant_specs: HashMap::new(),
            few_shot_examples: Vec::new(),
        }
    }

    #[test]
    fn test_build_nl_prompt_contains_query() {
        let ctx = test_ctx("find large files");
        let (system, user) = build_nl_prompt(&ctx, &ctx.cwd, 3);
        assert!(user.contains("find large files"));
        assert!(user.contains("macOS 14.5"));
        assert!(user.contains("rust"));
        assert!(user.contains("git, cargo"));
        assert!(user.contains("cargo build"));
        assert!(system.contains("3 alternative"));
    }

    #[test]
    fn test_build_nl_prompt_single_mode() {
        let mut ctx = test_ctx("list files");
        ctx.cwd = "/tmp".into();
        ctx.os = "Linux".into();
        ctx.project_type = None;
        ctx.available_tools = vec![];
        ctx.recent_commands = vec![];
        let (system, user) = build_nl_prompt(&ctx, &ctx.cwd, 1);
        assert!(system.contains("single shell command"));
        assert!(user.contains("standard POSIX utilities"));
    }

    #[test]
    fn test_build_nl_prompt_empty_tools() {
        let mut ctx = test_ctx("list files");
        ctx.cwd = "/tmp".into();
        ctx.os = "Linux".into();
        ctx.project_type = None;
        ctx.available_tools = vec![];
        ctx.recent_commands = vec![];
        let (_system, user) = build_nl_prompt(&ctx, &ctx.cwd, 3);
        assert!(user.contains("standard POSIX utilities"));
        assert!(user.contains("unknown"));
    }

    #[test]
    fn test_build_nl_prompt_includes_git_branch() {
        let mut ctx = test_ctx("rebase onto main");
        ctx.git_branch = Some("feature/auth-flow".into());
        let (_system, user) = build_nl_prompt(&ctx, &ctx.cwd, 1);
        assert!(user.contains("Git branch: feature/auth-flow"));
    }

    #[test]
    fn test_build_nl_prompt_includes_project_commands() {
        let mut ctx = test_ctx("run tests");
        ctx.project_commands
            .insert("make".into(), vec!["build".into(), "test".into()]);
        let (_system, user) = build_nl_prompt(&ctx, &ctx.cwd, 1);
        assert!(user.contains("make: build, test"));
    }

    #[test]
    fn test_build_nl_prompt_includes_cwd_entries() {
        let mut ctx = test_ctx("compress logs");
        ctx.cwd_entries = vec!["src/".into(), "logs/".into(), "Cargo.toml".into()];
        let (_system, user) = build_nl_prompt(&ctx, &ctx.cwd, 1);
        assert!(user.contains("src/, logs/, Cargo.toml"));
    }

    #[test]
    fn test_build_nl_prompt_includes_few_shot() {
        let mut ctx = test_ctx("find rust files");
        ctx.few_shot_examples = vec![("find python files".into(), "fd -e py".into())];
        let (_system, user) = build_nl_prompt(&ctx, &ctx.cwd, 1);
        assert!(user.contains("find python files"));
        assert!(user.contains("fd -e py"));
    }

    #[test]
    fn test_extract_commands_numbered_list() {
        let response = "1. find . -type f -size +100M\n2. du -sh * | sort -rh\n3. ls -lhS";
        let cmds = extract_commands(response, 5);
        assert_eq!(
            cmds,
            vec![
                "find . -type f -size +100M",
                "du -sh * | sort -rh",
                "ls -lhS",
            ]
        );
    }

    #[test]
    fn test_extract_commands_with_fence() {
        let response = "```bash\n1. find . -size +100M\n2. du -sh *\n```";
        let cmds = extract_commands(response, 5);
        assert_eq!(cmds, vec!["find . -size +100M", "du -sh *"]);
    }

    #[test]
    fn test_extract_commands_dedup() {
        let response = "1. ls -la\n2. ls -la\n3. ls -lh";
        let cmds = extract_commands(response, 5);
        assert_eq!(cmds, vec!["ls -la", "ls -lh"]);
    }

    #[test]
    fn test_extract_commands_fallback_single() {
        let response = "find . -name '*.rs'";
        let cmds = extract_commands(response, 3);
        assert_eq!(cmds, vec!["find . -name '*.rs'"]);
    }

    #[test]
    fn test_extract_commands_with_bullets() {
        let response = "- find . -size +100M\n- du -sh *";
        let cmds = extract_commands(response, 5);
        assert_eq!(cmds, vec!["find . -size +100M", "du -sh *"]);
    }

    #[test]
    fn test_extract_commands_respects_max() {
        let response = "1. cmd1\n2. cmd2\n3. cmd3\n4. cmd4";
        let cmds = extract_commands(response, 2);
        assert_eq!(cmds, vec!["cmd1", "cmd2"]);
    }

    #[test]
    fn test_extract_commands_skips_comments() {
        let response =
            "# Here are the commands:\n1. find . -size +100M\n// another comment\n2. du -sh *";
        let cmds = extract_commands(response, 5);
        assert_eq!(cmds, vec!["find . -size +100M", "du -sh *"]);
    }

    #[tokio::test]
    async fn test_auto_detect_model_skips_non_local_endpoint() {
        let _guard = LLM_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("SYNAPSE_TEST_DETECT_KEY", "test-key") };

        let config = LlmConfig {
            enabled: true,
            provider: "openai".into(),
            api_key_env: "SYNAPSE_TEST_DETECT_KEY".into(),
            base_url: Some("https://api.openai.com".into()),
            ..LlmConfig::default()
        };

        let mut client = LlmClient::from_config(&config, false).unwrap();
        let result = client.auto_detect_model().await;
        assert!(result.is_none(), "should skip non-local endpoints");

        unsafe { std::env::remove_var("SYNAPSE_TEST_DETECT_KEY") };
    }

    #[tokio::test]
    async fn test_auto_detect_model_handles_unreachable_endpoint() {
        // Use a port that (almost certainly) has nothing listening
        let config = LlmConfig {
            enabled: true,
            provider: "openai".into(),
            api_key_env: "SYNAPSE_TEST_NOEXIST".into(),
            base_url: Some("http://127.0.0.1:19999".into()),
            ..LlmConfig::default()
        };

        let mut client = LlmClient::from_config(&config, false).unwrap();
        let result = client.auto_detect_model().await;
        assert!(
            result.is_none(),
            "should return None for unreachable endpoint"
        );
    }

    #[tokio::test]
    async fn test_probe_health_handles_unreachable_endpoint() {
        let config = LlmConfig {
            enabled: true,
            provider: "openai".into(),
            api_key_env: "SYNAPSE_TEST_NOEXIST".into(),
            base_url: Some("http://127.0.0.1:19999".into()),
            ..LlmConfig::default()
        };

        let client = LlmClient::from_config(&config, false).unwrap();
        let healthy = client.probe_health().await;
        assert!(!healthy, "unreachable endpoint should not be healthy");
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
