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

#[cfg(test)]
mod tests {
    use super::{ArgTemplate, CommandSpec, SubcommandSpec};

    #[test]
    fn test_spec_toml_parsing() {
        let toml_str = r#"
name = "myapp"
description = "A test app"

[[subcommands]]
name = "serve"
description = "Start the server"

[[subcommands.options]]
long = "--port"
short = "-p"
takes_arg = true
description = "Port number"

[[subcommands]]
name = "build"
description = "Build the project"
"#;

        let spec: CommandSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.name, "myapp");
        assert_eq!(spec.subcommands.len(), 2);
        assert_eq!(spec.subcommands[0].name, "serve");
        assert_eq!(spec.subcommands[0].options.len(), 1);
        assert_eq!(
            spec.subcommands[0].options[0].long.as_deref(),
            Some("--port")
        );
        assert!(spec.subcommands[0].options[0].takes_arg);
        assert_eq!(spec.subcommands[1].name, "build");
    }

    #[test]
    fn test_spec_with_aliases() {
        let toml_str = r#"
name = "test"
aliases = ["t", "tst"]

[[subcommands]]
name = "run"
aliases = ["r"]
"#;

        let spec: CommandSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.aliases, vec!["t", "tst"]);
        assert_eq!(spec.subcommands[0].aliases, vec!["r"]);
    }

    #[test]
    fn test_spec_with_arg_template() {
        let toml_str = r#"
name = "cat"

[[args]]
name = "file"
template = "file_paths"
"#;

        let spec: CommandSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.args.len(), 1);
        assert_eq!(spec.args[0].template, Some(ArgTemplate::FilePaths));
    }

    #[test]
    fn test_spec_find_subcommand() {
        let spec = CommandSpec {
            name: "test".into(),
            subcommands: vec![
                SubcommandSpec {
                    name: "run".into(),
                    aliases: vec!["r".into()],
                    ..Default::default()
                },
                SubcommandSpec {
                    name: "build".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        assert!(spec.find_subcommand("run").is_some());
        assert!(spec.find_subcommand("r").is_some());
        assert!(spec.find_subcommand("build").is_some());
        assert!(spec.find_subcommand("nonexistent").is_none());
    }
}
