use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::OnceCell;

use crate::config::SpecConfig;
use crate::spec::CommandSpec;

mod discovery;
mod help_parser;
mod project_specs;
mod sandbox;

pub use help_parser::parse_help_basic;
pub use sandbox::sandbox_command;

/// Manages loading and resolution of command specs.
///
/// The spec store auto-generates specs from project files (Makefile,
/// package.json, etc.) and can discover specs on-demand via completion
/// generators or `--help` parsing. Discovery writes compsys files
/// directly to the completions directory.
pub struct SpecStore {
    /// Lazily computed project specs (computed once per process).
    project_specs: OnceCell<Arc<HashMap<String, CommandSpec>>>,
    config: SpecConfig,
    /// Set of command names that have zsh completion files available.
    zsh_index: HashSet<String>,
    /// Directory for generated compsys completion files.
    completions_dir: PathBuf,
}

impl SpecStore {
    pub fn new(config: SpecConfig) -> Self {
        Self::with_completions_dir(config, crate::compsys_export::completions_dir())
    }

    pub fn with_completions_dir(config: SpecConfig, completions_dir: PathBuf) -> Self {
        let zsh_index = crate::zsh_completion::scan_available_commands();

        Self {
            project_specs: OnceCell::new(),
            config,
            zsh_index,
            completions_dir,
        }
    }

    pub fn has_system_completion(&self, command: &str) -> bool {
        self.zsh_index.contains(command)
    }

    /// Get the completions directory path.
    pub fn completions_dir(&self) -> &Path {
        &self.completions_dir
    }
}
