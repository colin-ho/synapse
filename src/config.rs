use serde::Deserialize;
use std::{num::NonZeroUsize, path::PathBuf};

// --- Hardcoded internal constants (previously configurable) ---

/// Maximum character length for suggestion text before truncation.
pub const MAX_SUGGESTION_LENGTH: usize = 200;
/// Minimum interval in ms between LLM API calls.
pub const RATE_LIMIT_MS: u64 = 200;
/// Minimum characters for NL queries.
pub const NL_MIN_QUERY_LENGTH: usize = 5;
/// Max time in ms for generator commands (safety cap for spec-defined timeouts).
pub const GENERATOR_TIMEOUT_MS: u64 = 5_000;
/// Timeout in ms for each --help invocation during discovery.
pub const DISCOVER_TIMEOUT_MS: u64 = 2_000;
/// Maximum age in seconds before re-discovering a command (7 days).
pub const DISCOVER_MAX_AGE_SECS: u64 = 604_800;
/// Maximum recursion depth for subcommand discovery.
pub const DISCOVER_MAX_DEPTH: usize = 1;

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub spec: SpecConfig,
    pub security: SecurityConfig,
    pub llm: LlmConfig,
    pub completions: CompletionsConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct GeneralConfig {
    pub log_level: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SpecConfig {
    pub enabled: bool,
    pub auto_generate: bool,
    pub max_list_results: NonZeroUsize,
    /// Whether to trust and run project-built CLI tools during discovery.
    /// Disabled by default for security: a malicious repo could include
    /// binaries that execute arbitrary code when run with --help.
    pub trust_project_generators: bool,
    pub scan_depth: usize,
    /// Discover specs by running `command --help` for unknown commands
    pub discover_from_help: bool,
    /// Auto-discover specs for CLI tools built by the current project.
    /// This only runs when `trust_project_generators` is also enabled.
    pub discover_project_cli: bool,
    /// Commands to never run --help on
    pub discover_blocklist: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub scrub_paths: bool,
    pub command_blocklist: Vec<String>,
    pub scrub_env_keys: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    pub provider: String,
    pub api_key_env: String,
    /// Optional API base URL override.
    /// Uses {base_url}/v1/chat/completions (or {base_url}/chat/completions if
    /// base_url already ends in /v1).
    pub base_url: Option<String>,
    pub model: String,
    pub timeout_ms: u64,
    pub max_calls_per_discovery: usize,
    pub natural_language: bool,
    pub nl_max_suggestions: usize,
    /// Temperature for single NL suggestion (lower = more deterministic).
    pub temperature: f32,
    /// Temperature for multiple NL suggestions (higher = more variety).
    pub temperature_multi: f32,
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
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct CompletionsConfig {
    /// Override the output directory for generated completions
    pub output_dir: Option<String>,
    /// Only generate for commands without existing compsys functions
    pub gap_only: bool,
}

// --- Defaults ---

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            log_level: "warn".into(),
        }
    }
}

impl Default for SpecConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_generate: true,
            max_list_results: NonZeroUsize::new(50).unwrap(),
            trust_project_generators: false,
            scan_depth: 3,
            discover_from_help: true,
            discover_project_cli: false,
            discover_blocklist: Vec::new(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            scrub_paths: true,
            command_blocklist: vec![
                "export *=".into(),
                "curl -u".into(),
                r#"curl -H "Authorization*"#.into(),
            ],
            scrub_env_keys: vec![
                "*_KEY".into(),
                "*_SECRET".into(),
                "*_TOKEN".into(),
                "*_PASSWORD".into(),
                "*_CREDENTIALS".into(),
            ],
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "openai".into(),
            api_key_env: "LMSTUDIO_API_KEY".into(),
            base_url: Some("http://127.0.0.1:1234".into()),
            model: "gpt-4o-mini".into(),
            timeout_ms: 10_000,
            max_calls_per_discovery: 20,
            natural_language: true,
            nl_max_suggestions: 3,
            temperature: 0.3,
            temperature_multi: 0.7,
            discovery: None,
        }
    }
}

impl Default for CompletionsConfig {
    fn default() -> Self {
        Self {
            output_dir: None,
            gap_only: true,
        }
    }
}

impl LlmDiscoveryConfig {
    /// Produce a fully-resolved `LlmConfig` by overlaying discovery overrides
    /// onto the parent config. Only connection/model fields are overridable;
    /// NL settings are not relevant to discovery.
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
            natural_language: parent.natural_language,
            nl_max_suggestions: parent.nl_max_suggestions,
            temperature: parent.temperature,
            temperature_multi: parent.temperature_multi,
            discovery: None,
        }
    }
}

// --- Methods ---

impl Config {
    pub fn load() -> Self {
        let config_path = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(|d| PathBuf::from(d).join("synapse").join("config.toml"))
            .or_else(|| dirs::config_dir().map(|d| d.join("synapse").join("config.toml")))
            .unwrap_or_else(|| PathBuf::from("~/.config/synapse/config.toml"));

        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(contents) => match toml::from_str(&contents) {
                    Ok(config) => return config,
                    Err(e) => {
                        eprintln!("[synapse] Failed to parse {}: {e}", config_path.display());
                    }
                },
                Err(e) => {
                    eprintln!("[synapse] Failed to read {}: {e}", config_path.display());
                }
            }
        }

        Config::default()
    }
}
