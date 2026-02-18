use serde::Deserialize;
use std::{num::NonZeroUsize, path::PathBuf};

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub history: HistoryConfig,
    pub context: ContextConfig,
    pub spec: SpecConfig,
    pub weights: WeightsConfig,
    pub security: SecurityConfig,
    pub logging: LoggingConfig,
    pub llm: LlmConfig,
    pub workflow: WorkflowConfig,
    #[serde(skip)]
    cli_socket_override: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct GeneralConfig {
    pub socket_path: Option<String>,
    pub max_suggestion_length: usize,
    pub accept_key: String,
    pub log_level: String,
    pub ghost_text_color: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub max_entries: usize,
    pub fuzzy: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    pub enabled: bool,
    pub scan_depth: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SpecConfig {
    pub enabled: bool,
    pub auto_generate: bool,
    pub generator_timeout_ms: u64,
    pub max_list_results: NonZeroUsize,
    /// Whether to run generator commands from project-level specs (.synapse/specs/).
    /// Disabled by default for security: a malicious repo could include specs with
    /// arbitrary shell commands that execute during completion.
    pub trust_project_generators: bool,
    pub scan_depth: usize,
    /// Discover specs by running `command --help` for unknown commands
    pub discover_from_help: bool,
    /// Maximum recursion depth for subcommand discovery (0 = no recursion)
    pub discover_max_depth: usize,
    /// Timeout in ms for each `--help` invocation during discovery
    pub discover_timeout_ms: u64,
    /// Maximum age in seconds before re-discovering a command (default: 7 days)
    pub discover_max_age_secs: u64,
    /// Auto-discover specs for CLI tools built by the current project.
    /// This only runs when `trust_project_generators` is also enabled.
    pub discover_project_cli: bool,
    /// Commands to never run --help on
    pub discover_blocklist: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WeightsConfig {
    pub history: f64,
    pub spec: f64,
    pub recency: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub scrub_paths: bool,
    pub scrub_env_keys: Vec<String>,
    pub command_blocklist: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct LoggingConfig {
    pub interaction_log: String,
    pub max_log_size_mb: u64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    pub provider: String,
    pub api_key_env: String,
    /// Optional API base URL override.
    /// - OpenAI: uses {base_url}/v1/chat/completions (or {base_url}/chat/completions if base_url already ends in /v1)
    /// - Anthropic: uses {base_url}/v1/messages (or {base_url}/messages if base_url already ends in /v1)
    pub base_url: Option<String>,
    pub model: String,
    pub timeout_ms: u64,
    pub max_calls_per_discovery: usize,
    pub natural_language: bool,
    pub nl_min_query_length: usize,
    pub nl_max_suggestions: usize,
    pub workflow_prediction: bool,
    pub workflow_max_diff_tokens: usize,
    pub contextual_args: bool,
    pub arg_context_timeout_ms: u64,
    pub arg_max_context_tokens: usize,
    /// Debounce delay in ms for NL requests (default: 50ms)
    pub nl_debounce_ms: u64,
    /// Minimum interval in ms between LLM API calls (default: 200ms)
    pub rate_limit_ms: u64,
    /// Optional separate LLM config for spec discovery.
    /// When set, a second LLM client is created for discovery only.
    pub discovery: Option<LlmDiscoveryConfig>,
}

/// Optional overrides for the discovery LLM client.
/// All fields are optional — unset fields inherit from the parent `[llm]` config.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct LlmDiscoveryConfig {
    pub provider: Option<String>,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_calls_per_discovery: Option<usize>,
    pub rate_limit_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct WorkflowConfig {
    pub enabled: bool,
    pub min_probability: f64,
}

// --- Defaults ---

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            max_suggestion_length: 200,
            accept_key: "right-arrow".into(),
            log_level: "warn".into(),
            ghost_text_color: "fg=240".into(),
        }
    }
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 50000,
            fuzzy: true,
        }
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scan_depth: 3,
        }
    }
}

impl Default for SpecConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_generate: true,
            generator_timeout_ms: 500,
            max_list_results: NonZeroUsize::new(50).unwrap(),
            trust_project_generators: false,
            scan_depth: 3,
            discover_from_help: true,
            discover_max_depth: 1,
            discover_timeout_ms: 2000,
            discover_max_age_secs: 604800,
            discover_project_cli: false,
            discover_blocklist: Vec::new(),
        }
    }
}

impl Default for WeightsConfig {
    fn default() -> Self {
        Self {
            history: 0.30,
            spec: 0.50,
            recency: 0.20,
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
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
                r#"curl -H "Authorization*"#.into(),
            ],
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            interaction_log: "~/.local/share/synapse/interactions.jsonl".into(),
            max_log_size_mb: 50,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "openai".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            base_url: None,
            model: "gpt-4o-mini".into(),
            timeout_ms: 10_000,
            max_calls_per_discovery: 20,
            natural_language: true,
            nl_min_query_length: 5,
            nl_max_suggestions: 3,
            workflow_prediction: false,
            workflow_max_diff_tokens: 2000,
            contextual_args: true,
            arg_context_timeout_ms: 2_000,
            arg_max_context_tokens: 3_000,
            nl_debounce_ms: 50,
            rate_limit_ms: 200,
            discovery: None,
        }
    }
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_probability: 0.15,
        }
    }
}

impl LlmDiscoveryConfig {
    /// Produce a fully-resolved `LlmConfig` by overlaying discovery overrides
    /// onto the parent config. Only connection/model fields are overridable;
    /// NL/workflow/contextual-args settings are not relevant to discovery.
    pub fn resolve(&self, parent: &LlmConfig) -> LlmConfig {
        let provider_changed = self.provider.is_some();
        LlmConfig {
            enabled: parent.enabled,
            provider: self
                .provider
                .clone()
                .unwrap_or_else(|| parent.provider.clone()),
            api_key_env: self
                .api_key_env
                .clone()
                .unwrap_or_else(|| parent.api_key_env.clone()),
            // If provider is overridden, don't inherit parent's base_url
            // (e.g. switching from local OpenAI to Anthropic cloud — the local
            // base_url would be wrong). Use discovery's base_url or None.
            base_url: if provider_changed {
                self.base_url.clone()
            } else {
                self.base_url.clone().or_else(|| parent.base_url.clone())
            },
            model: self.model.clone().unwrap_or_else(|| parent.model.clone()),
            timeout_ms: self.timeout_ms.unwrap_or(parent.timeout_ms),
            max_calls_per_discovery: self
                .max_calls_per_discovery
                .unwrap_or(parent.max_calls_per_discovery),
            rate_limit_ms: self.rate_limit_ms.unwrap_or(parent.rate_limit_ms),
            // Irrelevant for discovery — carry parent values for struct completeness
            natural_language: parent.natural_language,
            nl_min_query_length: parent.nl_min_query_length,
            nl_max_suggestions: parent.nl_max_suggestions,
            workflow_prediction: parent.workflow_prediction,
            workflow_max_diff_tokens: parent.workflow_max_diff_tokens,
            contextual_args: parent.contextual_args,
            arg_context_timeout_ms: parent.arg_context_timeout_ms,
            arg_max_context_tokens: parent.arg_max_context_tokens,
            nl_debounce_ms: parent.nl_debounce_ms,
            discovery: None,
        }
    }
}

// --- Methods ---

impl Config {
    pub fn load() -> Self {
        let config_path = dirs::config_dir()
            .map(|d| d.join("synapse").join("config.toml"))
            .unwrap_or_else(|| PathBuf::from("~/.config/synapse/config.toml"));

        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(contents) => match toml::from_str(&contents) {
                    Ok(config) => {
                        tracing::info!("Loaded config from {}", config_path.display());
                        return config;
                    }
                    Err(e) => {
                        eprintln!("[synapse] Failed to parse {}: {e}", config_path.display());
                        tracing::warn!("Failed to parse config: {e}, using defaults");
                    }
                },
                Err(e) => {
                    eprintln!("[synapse] Failed to read {}: {e}", config_path.display());
                    tracing::warn!("Failed to read config: {e}, using defaults");
                }
            }
        }

        Config::default()
    }

    pub fn with_socket_override(mut self, path: Option<PathBuf>) -> Self {
        if let Some(p) = path {
            self.cli_socket_override = Some(p.to_string_lossy().into_owned());
        }
        self
    }

    pub fn socket_path(&self) -> PathBuf {
        if let Some(ref path) = self.cli_socket_override {
            return PathBuf::from(path);
        }

        if let Ok(path) = std::env::var("SYNAPSE_SOCKET") {
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }

        if let Some(ref path) = self.general.socket_path {
            return PathBuf::from(path);
        }

        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            return PathBuf::from(runtime_dir).join("synapse.sock");
        }

        let uid = nix::unistd::getuid();
        PathBuf::from(format!("/tmp/synapse-{}.sock", uid))
    }

    pub fn pid_path(&self) -> PathBuf {
        let sock = self.socket_path();
        sock.with_extension("pid")
    }

    #[allow(dead_code)]
    pub fn lock_path(&self) -> PathBuf {
        let sock = self.socket_path();
        sock.with_extension("lock")
    }

    pub fn interaction_log_path(&self) -> PathBuf {
        let path = self
            .logging
            .interaction_log
            .replace('~', &dirs::home_dir().unwrap_or_default().to_string_lossy());
        PathBuf::from(path)
    }
}

impl WeightsConfig {
    pub fn normalized(&self) -> WeightsConfig {
        let sum = self.history + self.spec + self.recency;
        if sum == 0.0 {
            return WeightsConfig::default();
        }
        WeightsConfig {
            history: self.history / sum,
            spec: self.spec / sum,
            recency: self.recency / sum,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::{Config, LlmDiscoveryConfig, WeightsConfig};

    static SOCKET_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_config_defaults() {
        let config = Config::default();
        assert_eq!(config.general.max_suggestion_length, 200);
        assert!(config.history.enabled);
        assert_eq!(config.history.max_entries, 50000);
        assert_eq!(config.weights.history, 0.30);
        assert_eq!(config.weights.spec, 0.50);
        assert_eq!(config.weights.recency, 0.20);
        assert!(config.llm.contextual_args);
        assert_eq!(config.llm.arg_context_timeout_ms, 2_000);
        assert_eq!(config.llm.arg_max_context_tokens, 3_000);
        assert_eq!(config.llm.base_url, None);
    }

    #[test]
    fn test_weights_normalization() {
        let weights = WeightsConfig {
            history: 1.0,
            spec: 1.0,
            recency: 1.0,
        };
        let normalized = weights.normalized();
        let sum = normalized.history + normalized.spec + normalized.recency;
        assert!((sum - 1.0).abs() < 0.001);
        assert!((normalized.history - 0.333).abs() < 0.01);
    }

    #[test]
    fn test_socket_path_env_override() {
        let _guard = SOCKET_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("SYNAPSE_SOCKET", "/tmp/test-override.sock") };
        let config = Config::default();
        assert_eq!(
            config.socket_path(),
            std::path::PathBuf::from("/tmp/test-override.sock")
        );
        assert_eq!(
            config.pid_path(),
            std::path::PathBuf::from("/tmp/test-override.pid")
        );
        unsafe { std::env::remove_var("SYNAPSE_SOCKET") };
    }

    #[test]
    fn test_socket_path_cli_override_beats_env() {
        let _guard = SOCKET_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("SYNAPSE_SOCKET", "/tmp/test-env.sock") };
        let config = Config::default()
            .with_socket_override(Some(std::path::PathBuf::from("/tmp/test-cli.sock")));
        assert_eq!(
            config.socket_path(),
            std::path::PathBuf::from("/tmp/test-cli.sock")
        );
        unsafe { std::env::remove_var("SYNAPSE_SOCKET") };
    }

    #[test]
    fn test_socket_path_env_empty_ignored() {
        let _guard = SOCKET_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("SYNAPSE_SOCKET", "") };
        let config = Config::default();
        // Should fall through to default, not use empty string
        assert_ne!(config.socket_path(), std::path::PathBuf::from(""));
        unsafe { std::env::remove_var("SYNAPSE_SOCKET") };
    }

    #[test]
    fn test_discovery_config_resolves_with_overrides() {
        let parent = super::LlmConfig::default();
        let discovery = LlmDiscoveryConfig {
            provider: Some("anthropic".into()),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            model: Some("claude-haiku-4-5-20251001".into()),
            timeout_ms: Some(30000),
            ..Default::default()
        };
        let resolved = discovery.resolve(&parent);
        assert_eq!(resolved.provider, "anthropic");
        assert_eq!(resolved.api_key_env, "ANTHROPIC_API_KEY");
        assert_eq!(resolved.model, "claude-haiku-4-5-20251001");
        assert_eq!(resolved.timeout_ms, 30000);
        // Inherited from parent
        assert_eq!(
            resolved.max_calls_per_discovery,
            parent.max_calls_per_discovery
        );
        assert_eq!(resolved.rate_limit_ms, parent.rate_limit_ms);
    }

    #[test]
    fn test_discovery_config_inherits_all_when_empty() {
        let parent = super::LlmConfig::default();
        let discovery = LlmDiscoveryConfig::default();
        let resolved = discovery.resolve(&parent);
        assert_eq!(resolved.provider, parent.provider);
        assert_eq!(resolved.model, parent.model);
        assert_eq!(resolved.base_url, parent.base_url);
        assert_eq!(resolved.timeout_ms, parent.timeout_ms);
    }

    #[test]
    fn test_discovery_provider_override_clears_base_url() {
        let mut parent = super::LlmConfig::default();
        // Set a base_url so we can test that provider override clears it
        parent.base_url = Some("http://127.0.0.1:1234".into());
        assert!(parent.base_url.is_some());
        let discovery = LlmDiscoveryConfig {
            provider: Some("anthropic".into()),
            // No base_url set — should NOT inherit parent's local endpoint
            ..Default::default()
        };
        let resolved = discovery.resolve(&parent);
        assert_eq!(resolved.base_url, None);
    }

    #[test]
    fn test_discovery_absent_parses_as_none() {
        let toml_str = r#"
[llm]
provider = "openai"
model = "test-model"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.llm.discovery.is_none());
    }

    #[test]
    fn test_discovery_config_parses_from_toml() {
        let toml_str = r#"
[llm]
provider = "openai"
model = "local-model"

[llm.discovery]
provider = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-haiku-4-5-20251001"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let disc = config.llm.discovery.unwrap();
        assert_eq!(disc.provider.unwrap(), "anthropic");
        assert_eq!(disc.model.unwrap(), "claude-haiku-4-5-20251001");
        assert_eq!(disc.api_key_env.unwrap(), "ANTHROPIC_API_KEY");
    }
}
