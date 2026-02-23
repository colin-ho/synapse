use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;

use crate::config::SpecConfig;
use crate::spec::CommandSpec;

mod discovery;
mod help_parser;
mod project_specs;
mod sandbox;

pub use help_parser::parse_help_basic;
pub use sandbox::sandbox_command;

/// Manages loading, caching, and resolution of command specs.
///
/// The spec store auto-generates specs from project files (Makefile,
/// package.json, etc.) and can discover specs on-demand via completion
/// generators or `--help` parsing. Discovery writes compsys files
/// directly to the completions directory.
pub struct SpecStore {
    project_cache: Cache<PathBuf, Arc<HashMap<String, CommandSpec>>>,
    /// In-memory cache of specs produced by discovery.
    /// Populated by `write_and_cache_discovered`, checked by `lookup` after project cache.
    discovered_cache: Cache<String, CommandSpec>,
    config: SpecConfig,
    /// Set of command names that have zsh completion files available.
    /// Wrapped in RwLock to allow periodic refresh when new tools are installed.
    zsh_index: std::sync::RwLock<HashSet<String>>,
    /// Directory for generated compsys completion files.
    completions_dir: PathBuf,
}

impl SpecStore {
    pub fn new(config: SpecConfig) -> Self {
        Self::with_completions_dir(config, crate::compsys_export::completions_dir())
    }

    pub fn with_completions_dir(config: SpecConfig, completions_dir: PathBuf) -> Self {
        let zsh_index = crate::zsh_completion::scan_available_commands();

        let project_cache = Cache::builder()
            .max_capacity(50)
            .time_to_live(Duration::from_secs(300))
            .build();

        let discovered_cache = Cache::builder()
            .max_capacity(500)
            .time_to_live(Duration::from_secs(crate::config::DISCOVER_MAX_AGE_SECS))
            .build();

        Self {
            project_cache,
            discovered_cache,
            config,
            zsh_index: std::sync::RwLock::new(zsh_index),
            completions_dir,
        }
    }

    pub fn has_system_completion(&self, command: &str) -> bool {
        self.zsh_index
            .read()
            .map(|idx| idx.contains(command))
            .unwrap_or(false)
    }

    /// Get the completions directory path.
    pub fn completions_dir(&self) -> &Path {
        &self.completions_dir
    }

    /// Get the spec config.
    pub fn config(&self) -> &SpecConfig {
        &self.config
    }
}
