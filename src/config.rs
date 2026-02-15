use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub history: HistoryConfig,
    pub context: ContextConfig,
    pub ai: AiConfig,
    pub weights: WeightsConfig,
    pub security: SecurityConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub socket_path: Option<String>,
    pub debounce_ms: u64,
    pub max_suggestion_length: usize,
    pub accept_key: String,
    pub log_level: String,
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
pub struct AiConfig {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub api_key_env: String,
    pub max_tokens: u32,
    pub temperature: f64,
    pub timeout_ms: u64,
    pub fallback_to_local: bool,
    pub rate_limit_rpm: u32,
    pub max_concurrent_requests: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WeightsConfig {
    pub history: f64,
    pub context: f64,
    pub ai: f64,
    pub recency: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub scrub_paths: bool,
    pub scrub_env_keys: Vec<String>,
    pub command_blocklist: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub interaction_log: String,
    pub max_log_size_mb: u64,
}

// --- Defaults ---

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            history: HistoryConfig::default(),
            context: ContextConfig::default(),
            ai: AiConfig::default(),
            weights: WeightsConfig::default(),
            security: SecurityConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            debounce_ms: 150,
            max_suggestion_length: 200,
            accept_key: "right-arrow".into(),
            log_level: "warn".into(),
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

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "ollama".into(),
            model: "llama3".into(),
            endpoint: "http://localhost:11434".into(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
            max_tokens: 50,
            temperature: 0.0,
            timeout_ms: 2000,
            fallback_to_local: true,
            rate_limit_rpm: 30,
            max_concurrent_requests: 2,
        }
    }
}

impl Default for WeightsConfig {
    fn default() -> Self {
        Self {
            history: 0.35,
            context: 0.2,
            ai: 0.3,
            recency: 0.15,
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
                r#"curl -H "Authorization""#.into(),
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
                        tracing::warn!("Failed to parse config: {e}, using defaults");
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read config: {e}, using defaults");
                }
            }
        }

        Config::default()
    }

    pub fn socket_path(&self) -> PathBuf {
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

    pub fn lock_path(&self) -> PathBuf {
        let sock = self.socket_path();
        sock.with_extension("lock")
    }

    pub fn interaction_log_path(&self) -> PathBuf {
        let path = self.logging.interaction_log.replace('~', &dirs::home_dir().unwrap_or_default().to_string_lossy());
        PathBuf::from(path)
    }
}

impl WeightsConfig {
    pub fn normalized(&self) -> WeightsConfig {
        let sum = self.history + self.context + self.ai + self.recency;
        if sum == 0.0 {
            return WeightsConfig::default();
        }
        WeightsConfig {
            history: self.history / sum,
            context: self.context / sum,
            ai: self.ai / sum,
            recency: self.recency / sum,
        }
    }
}
