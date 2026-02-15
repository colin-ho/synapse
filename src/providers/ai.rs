use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moka::future::Cache;
use tokio::sync::Semaphore;

use crate::cache::AiCacheKey;
use crate::config::AiConfig;
use crate::protocol::{SuggestRequest, SuggestionSource};
use crate::providers::context::ContextProvider;
use crate::providers::{ProviderSuggestion, SuggestionProvider};
use crate::security::Scrubber;

/// Token bucket for rate limiting
struct RateLimiter {
    tokens: tokio::sync::Mutex<f64>,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: tokio::sync::Mutex<std::time::Instant>,
}

impl RateLimiter {
    fn new(rpm: u32) -> Self {
        let max_tokens = rpm as f64;
        Self {
            tokens: tokio::sync::Mutex::new(max_tokens),
            max_tokens,
            refill_rate: rpm as f64 / 60.0,
            last_refill: tokio::sync::Mutex::new(std::time::Instant::now()),
        }
    }

    async fn try_acquire(&self) -> bool {
        let mut tokens = self.tokens.lock().await;
        let mut last = self.last_refill.lock().await;

        let now = std::time::Instant::now();
        let elapsed = now.duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * self.refill_rate).min(self.max_tokens);
        *last = now;

        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

pub struct AiProvider {
    config: AiConfig,
    client: reqwest::Client,
    cache: Cache<AiCacheKey, String>,
    rate_limiter: RateLimiter,
    concurrency: Arc<Semaphore>,
    scrubber: Option<Arc<Scrubber>>,
}

impl AiProvider {
    pub fn new(config: AiConfig, scrubber: Option<Arc<Scrubber>>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .unwrap_or_default();

        let cache = Cache::builder()
            .max_capacity(500)
            .time_to_live(Duration::from_secs(600))
            .build();

        let rate_limiter = RateLimiter::new(config.rate_limit_rpm);
        let concurrency = Arc::new(Semaphore::new(config.max_concurrent_requests as usize));

        Self {
            config,
            client,
            cache,
            rate_limiter,
            concurrency,
            scrubber,
        }
    }

    fn build_prompt(&self, request: &SuggestRequest, project_type: Option<&str>, git_branch: Option<&str>) -> String {
        let (cwd, recent, buffer) = if let Some(ref scrubber) = self.scrubber {
            let cwd = scrubber.scrub_path(&request.cwd);
            let recent = scrubber.scrub_commands(&request.recent_commands);
            // Scrub the buffer too — it's the most likely place for sensitive data
            // (e.g. "export API_KEY=sk-...", "curl -u admin:pass ...")
            let scrubbed_buffer = scrubber.scrub_commands(&[request.buffer.clone()]);
            let buffer = if scrubbed_buffer.is_empty() {
                // Buffer matched a blocklist pattern — don't send it to the API
                return String::new();
            } else {
                scrubber.scrub_path(&scrubbed_buffer[0])
            };
            (cwd, recent, buffer)
        } else {
            (request.cwd.clone(), request.recent_commands.clone(), request.buffer.clone())
        };

        let recent_str = recent.join(", ");

        let mut prompt = format!(
            "You are a terminal command autocomplete engine. Given the context below, \
             suggest the single most likely command the user is trying to type. \
             Respond with ONLY the completed command on a single line, nothing else.\n\n\
             Working directory: {cwd}\n"
        );

        if let Some(pt) = project_type {
            prompt.push_str(&format!("Project type: {pt}\n"));
        }
        if let Some(branch) = git_branch {
            prompt.push_str(&format!("Git branch: {branch}\n"));
        }
        if !recent_str.is_empty() {
            prompt.push_str(&format!("Recent commands: {recent_str}\n"));
        }
        prompt.push_str(&format!("Current input: \"{buffer}\"\n"));

        prompt
    }

    fn cache_key(&self, request: &SuggestRequest, project_type: Option<String>, git_branch: Option<String>) -> AiCacheKey {
        // Use first few chars as prefix for cache key
        let prefix_len = (request.buffer.len() / 2).max(3).min(request.buffer.len());
        AiCacheKey {
            buffer_prefix: request.buffer[..prefix_len].to_string(),
            cwd: std::path::PathBuf::from(&request.cwd),
            project_type,
            git_branch,
        }
    }

    async fn call_ollama(&self, prompt: &str) -> Option<String> {
        let body = serde_json::json!({
            "model": self.config.model,
            "prompt": prompt,
            "stream": false,
            "options": {
                "temperature": self.config.temperature,
                "num_predict": self.config.max_tokens,
            }
        });

        let url = format!("{}/api/generate", self.config.endpoint);
        let resp = self.client.post(&url).json(&body).send().await.ok()?;

        if !resp.status().is_success() {
            tracing::warn!("Ollama API error: {}", resp.status());
            return None;
        }

        let json: serde_json::Value = resp.json().await.ok()?;
        let text = json.get("response")?.as_str()?;

        // Take first line only
        let first_line = text.lines().next()?.trim().to_string();
        if first_line.is_empty() { None } else { Some(first_line) }
    }

    async fn call_anthropic(&self, prompt: &str) -> Option<String> {
        let api_key = std::env::var(&self.config.api_key_env).ok()?;

        let body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": self.config.max_tokens,
            "messages": [{
                "role": "user",
                "content": prompt,
            }],
            "temperature": self.config.temperature,
        });

        let resp = self.client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            tracing::warn!("Anthropic API error: {}", resp.status());
            return None;
        }

        let json: serde_json::Value = resp.json().await.ok()?;
        let text = json
            .get("content")?
            .as_array()?
            .first()?
            .get("text")?
            .as_str()?;

        let first_line = text.lines().next()?.trim().to_string();
        if first_line.is_empty() { None } else { Some(first_line) }
    }

    async fn call_openai(&self, prompt: &str) -> Option<String> {
        let api_key = std::env::var(&self.config.api_key_env).ok()?;

        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [{
                "role": "user",
                "content": prompt,
            }],
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
        });

        let endpoint = if self.config.endpoint.contains("openai.com") {
            "https://api.openai.com/v1/chat/completions".to_string()
        } else {
            format!("{}/v1/chat/completions", self.config.endpoint)
        };

        let resp = self.client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            tracing::warn!("OpenAI API error: {}", resp.status());
            return None;
        }

        let json: serde_json::Value = resp.json().await.ok()?;
        let text = json
            .get("choices")?
            .as_array()?
            .first()?
            .get("message")?
            .get("content")?
            .as_str()?;

        let first_line = text.lines().next()?.trim().to_string();
        if first_line.is_empty() { None } else { Some(first_line) }
    }

    async fn call_backend(&self, prompt: &str) -> Option<String> {
        match self.config.provider.as_str() {
            "ollama" => self.call_ollama(prompt).await,
            "anthropic" => self.call_anthropic(prompt).await,
            "openai" => self.call_openai(prompt).await,
            other => {
                tracing::warn!("Unknown AI provider: {other}");
                None
            }
        }
    }

    fn is_local_provider(&self) -> bool {
        self.config.provider == "ollama"
    }
}

#[async_trait]
impl SuggestionProvider for AiProvider {
    async fn suggest(&self, request: &SuggestRequest) -> Option<ProviderSuggestion> {
        if !self.config.enabled || request.buffer.is_empty() {
            return None;
        }

        // Determine project context
        let cwd = std::path::Path::new(&request.cwd);
        let project_type = ContextProvider::project_type_for(cwd);
        let git_branch = crate::providers::context::read_git_branch_pub(cwd);

        // Check cache first
        let key = self.cache_key(request, project_type.clone(), git_branch.clone());
        if let Some(cached) = self.cache.get(&key).await {
            if cached.starts_with(&request.buffer) {
                return Some(ProviderSuggestion {
                    text: cached,
                    source: SuggestionSource::Ai,
                    score: 0.8,
                });
            }
        }

        // Rate limit check
        if !self.rate_limiter.try_acquire().await {
            tracing::debug!("AI rate limit exceeded, skipping");
            return None;
        }

        // Concurrency limit
        let _permit = self.concurrency.try_acquire().ok()?;

        let prompt = self.build_prompt(request, project_type.as_deref(), git_branch.as_deref());
        if prompt.is_empty() {
            tracing::debug!("Buffer matched security blocklist, skipping AI");
            return None;
        }
        let result = self.call_backend(&prompt).await;

        match result {
            Some(text) if text.starts_with(&request.buffer) || request.buffer.starts_with(&text[..text.len().min(request.buffer.len())]) => {
                // Cache the result
                self.cache.insert(key, text.clone()).await;

                Some(ProviderSuggestion {
                    text,
                    source: SuggestionSource::Ai,
                    score: 0.85,
                })
            }
            Some(text) => {
                tracing::debug!("AI suggestion doesn't match buffer prefix, discarding: {text}");
                None
            }
            None => {
                if self.config.fallback_to_local && !self.is_local_provider() {
                    tracing::debug!("AI API failed, skipping");
                }
                None
            }
        }
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Ai
    }

    fn is_available(&self) -> bool {
        self.config.enabled
    }
}
