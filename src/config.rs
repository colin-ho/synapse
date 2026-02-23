use serde::Deserialize;
use std::path::PathBuf;

// --- Hardcoded internal constants (previously configurable) ---

/// Minimum interval in ms between LLM API calls.
pub const RATE_LIMIT_MS: u64 = 200;
/// Minimum characters for NL queries.
pub const NL_MIN_QUERY_LENGTH: usize = 5;
/// Max time in ms for generator commands (safety cap for spec-defined timeouts).
pub const GENERATOR_TIMEOUT_MS: u64 = 5_000;
/// Timeout in ms for each --help invocation during discovery.
pub const DISCOVER_TIMEOUT_MS: u64 = 2_000;
#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub spec: SpecConfig,
    pub security: SecurityConfig,
    pub llm: LlmConfig,
    pub completions: CompletionsConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SpecConfig {
    pub enabled: bool,
    pub auto_generate: bool,
    pub scan_depth: usize,
    /// Discover specs by running `command --help` for unknown commands
    pub discover_from_help: bool,
    /// Commands to never run --help on
    pub discover_blocklist: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub command_blocklist: Vec<String>,
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
    pub natural_language: bool,
    pub nl_max_suggestions: usize,
    /// Temperature for single NL suggestion (lower = more deterministic).
    pub temperature: f32,
    /// Temperature for multiple NL suggestions (higher = more variety).
    pub temperature_multi: f32,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct CompletionsConfig {
    /// Override the output directory for generated completions
    pub output_dir: Option<String>,
}

// --- Defaults ---

impl Default for SpecConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_generate: true,
            scan_depth: 3,
            discover_from_help: true,
            discover_blocklist: Vec::new(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            command_blocklist: vec![
                "export *=".into(),
                "curl -u".into(),
                r#"curl -H "Authorization*"#.into(),
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
            natural_language: true,
            nl_max_suggestions: 3,
            temperature: 0.3,
            temperature_multi: 0.7,
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
