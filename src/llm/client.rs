use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::LlmConfig;

use super::prompt::{
    build_nl_prompt, NlTranslationContext, NlTranslationItem, NlTranslationResult,
};
use super::response::{detect_destructive_command, extract_commands};

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("LLM disabled due to recent API errors (backoff active)")]
    BackoffActive,
    #[error("Empty response from LLM")]
    EmptyResponse,
}

pub struct LlmClient {
    api_key: String,
    base_url: Option<String>,
    model: String,
    client: Client,
    /// Minimum interval between LLM calls.
    rate_limiter: Mutex<Instant>,
    rate_limit_duration: Duration,
    /// Set on API errors, cleared after 5 minutes.
    backoff_active: AtomicBool,
    backoff_until: Mutex<Option<Instant>>,
}

impl LlmClient {
    /// Construct an LlmClient from config. Returns `None` if disabled or API key is unset.
    pub fn from_config(config: &LlmConfig) -> Option<Self> {
        if !config.enabled {
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
                    "lm-studio".to_string()
                } else {
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
            client,
            rate_limit_duration: Duration::from_millis(crate::config::RATE_LIMIT_MS),
            rate_limiter: Mutex::new(Instant::now() - Duration::from_secs(1)),
            backoff_active: AtomicBool::new(false),
            backoff_until: Mutex::new(None),
        })
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
            Err(_) => return None,
        };

        let models_resp: ModelsResponse = match resp.json().await {
            Ok(r) => r,
            Err(_) => return None,
        };

        if models_resp.data.is_empty() {
            return None;
        }

        if models_resp.data.iter().any(|m| m.id == self.model) {
            return Some(self.model.clone());
        }

        let new_model = models_resp.data[0].id.clone();
        self.model = new_model.clone();
        Some(new_model)
    }

    /// Translate a natural language query into one or more shell commands.
    pub async fn translate_command(
        &self,
        ctx: &NlTranslationContext,
        max_suggestions: usize,
        temperature: f32,
    ) -> Result<NlTranslationResult, LlmError> {
        let (system_prompt, user_prompt) = build_nl_prompt(ctx, &ctx.cwd, max_suggestions);

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
            .map(|command| NlTranslationItem {
                warning: detect_destructive_command(&command),
                command,
            })
            .collect();

        Ok(NlTranslationResult { items })
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
        if result
            .as_ref()
            .err()
            .is_some_and(Self::should_activate_backoff)
        {
            self.activate_backoff().await;
        }

        result
    }

    fn should_activate_backoff(error: &LlmError) -> bool {
        matches!(error, LlmError::Api { status, .. } if *status == 429 || *status >= 500)
    }

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
            .map(|choice| choice.message.content.clone())
            .unwrap_or_default())
    }

    fn openai_chat_completions_url(&self) -> String {
        match self.base_url.as_deref() {
            Some(base) => url_with_v1_path(base, "chat/completions"),
            None => "https://api.openai.com/v1/chat/completions".to_string(),
        }
    }
}

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

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
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
    let host_part = lower
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&lower);
    host_part.starts_with("127.0.0.1")
        || host_part.starts_with("localhost")
        || host_part.starts_with("[::1]")
}
