use serde::Deserialize;

/// Source priority for specs (higher priority shadows lower)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpecSource {
    Builtin,
    ProjectUser,
    ProjectAuto,
}

/// Root command specification
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CommandSpec {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub description: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub version_command: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
    #[serde(default)]
    pub args: Vec<ArgSpec>,
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
            version_command: None,
            subcommands: Vec::new(),
            options: Vec::new(),
            args: Vec::new(),
            source: SpecSource::Builtin,
        }
    }
}

/// Recursive subcommand definition
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SubcommandSpec {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
    #[serde(default)]
    pub args: Vec<ArgSpec>,
}

/// Option/flag definition
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct OptionSpec {
    #[serde(default)]
    pub long: Option<String>,
    #[serde(default)]
    pub short: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub takes_arg: bool,
    #[serde(default)]
    pub arg_generator: Option<GeneratorSpec>,
    #[serde(default)]
    #[allow(dead_code)]
    pub exclusive_with: Vec<String>,
}

/// Argument position definition
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ArgSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub required: bool,
    #[serde(default)]
    #[allow(dead_code)]
    pub variadic: bool,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub generator: Option<GeneratorSpec>,
    #[serde(default)]
    pub template: Option<ArgTemplate>,
}

/// Dynamic value generator
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct GeneratorSpec {
    pub command: String,
    #[serde(default = "default_split_on")]
    pub split_on: String,
    #[serde(default)]
    pub strip_prefix: Option<String>,
    #[serde(default = "default_cache_ttl")]
    #[allow(dead_code)]
    pub cache_ttl_secs: u64,
    #[serde(default = "default_generator_timeout")]
    pub timeout_ms: u64,
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

/// Template for common argument types
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArgTemplate {
    FilePaths,
    Directories,
    EnvVars,
    History,
}

impl CommandSpec {
    /// Find a subcommand by name or alias
    #[allow(dead_code)]
    pub fn find_subcommand(&self, name: &str) -> Option<&SubcommandSpec> {
        self.subcommands
            .iter()
            .find(|s| s.name == name || s.aliases.iter().any(|a| a == name))
    }
}

impl SubcommandSpec {
    /// Find a nested subcommand by name or alias
    #[allow(dead_code)]
    pub fn find_subcommand(&self, name: &str) -> Option<&SubcommandSpec> {
        self.subcommands
            .iter()
            .find(|s| s.name == name || s.aliases.iter().any(|a| a == name))
    }
}
