use serde::{Deserialize, Serialize};

/// Source priority for specs (higher priority shadows lower).
/// Variant order matters: derived `Ord` uses declaration order,
/// so later variants compare greater (= higher priority).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpecSource {
    Discovered,
    ProjectAuto,
}

/// Root command specification
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CommandSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<OptionSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<ArgSpec>,
    /// Command takes another command as its first argument (e.g. sudo, env).
    #[serde(default, skip_serializing_if = "is_false")]
    pub recursive: bool,
    /// Set at load time, not from TOML
    #[serde(skip)]
    pub source: SpecSource,
}

impl Default for CommandSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            aliases: Vec::new(),
            description: None,
            subcommands: Vec::new(),
            options: Vec::new(),
            args: Vec::new(),
            recursive: false,
            source: SpecSource::ProjectAuto,
        }
    }
}

/// Recursive subcommand definition
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct SubcommandSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<OptionSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<ArgSpec>,
}

/// Option/flag definition
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct OptionSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub takes_arg: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_generator: Option<GeneratorSpec>,
}

/// Argument position definition
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ArgSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub variadic: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<GeneratorSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<ArgTemplate>,
}

/// Dynamic value generator
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GeneratorSpec {
    pub command: String,
    #[serde(
        default = "default_split_on",
        skip_serializing_if = "is_default_split_on"
    )]
    pub split_on: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_prefix: Option<String>,
    #[serde(
        default = "default_cache_ttl",
        skip_serializing_if = "is_default_cache_ttl"
    )]
    pub cache_ttl_secs: u64,
    #[serde(
        default = "default_generator_timeout",
        skip_serializing_if = "is_default_generator_timeout"
    )]
    pub timeout_ms: u64,
}

impl Default for GeneratorSpec {
    fn default() -> Self {
        Self {
            command: String::new(),
            split_on: default_split_on(),
            strip_prefix: None,
            cache_ttl_secs: default_cache_ttl(),
            timeout_ms: default_generator_timeout(),
        }
    }
}

fn default_split_on() -> String {
    "\n".to_string()
}

fn default_cache_ttl() -> u64 {
    10
}

fn default_generator_timeout() -> u64 {
    500
}

fn is_false(v: &bool) -> bool {
    !v
}

fn is_default_split_on(v: &str) -> bool {
    v == "\n"
}

fn is_default_cache_ttl(v: &u64) -> bool {
    *v == 10
}

fn is_default_generator_timeout(v: &u64) -> bool {
    *v == 500
}

/// Template for common argument types
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArgTemplate {
    FilePaths,
    Directories,
    EnvVars,
    History,
}
