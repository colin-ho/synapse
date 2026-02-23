use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl Default for GeneratorSpec {
    fn default() -> Self {
        Self {
            command: String::new(),
            split_on: default_split_on(),
            strip_prefix: None,
        }
    }
}

fn default_split_on() -> String {
    "\n".to_string()
}

fn is_false(v: &bool) -> bool {
    !v
}

fn is_default_split_on(v: &str) -> bool {
    v == "\n"
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
