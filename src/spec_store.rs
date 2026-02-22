use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use moka::Expiry;

use crate::config::SpecConfig;
use crate::spec::{CommandSpec, GeneratorSpec};

mod discovery;
mod generator;
mod help_parser;
mod project_specs;
mod sandbox;

pub use help_parser::parse_help_basic;
pub use sandbox::sandbox_command;

#[derive(Clone)]
struct GeneratorCacheEntry {
    items: Vec<String>,
    ttl: Duration,
}

struct GeneratorExpiry;

impl Expiry<(String, PathBuf), GeneratorCacheEntry> for GeneratorExpiry {
    fn expire_after_create(
        &self,
        _key: &(String, PathBuf),
        value: &GeneratorCacheEntry,
        _current_time: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

/// Manages loading, caching, and resolution of command specs.
///
/// The spec store auto-generates specs from project files (Makefile,
/// package.json, etc.) and can discover specs on-demand via completion
/// generators or `--help` parsing. Discovery writes compsys files
/// directly to the completions directory.
pub struct SpecStore {
    project_cache: Cache<PathBuf, Arc<HashMap<String, CommandSpec>>>,
    generator_cache: Cache<(String, PathBuf), GeneratorCacheEntry>,
    /// In-memory cache of specs produced by discovery.
    /// Populated by `write_and_cache_discovered`, checked by `lookup` after project cache.
    discovered_cache: Cache<String, CommandSpec>,
    config: SpecConfig,
    /// Set of command names that have zsh completion files available.
    /// Wrapped in RwLock to allow periodic refresh when new tools are installed.
    zsh_index: std::sync::RwLock<HashSet<String>>,
    /// Directory for generated compsys completion files.
    completions_dir: PathBuf,
    /// Cache of parsed system zsh completion files (from find_and_parse).
    /// Used as a fallback when no project spec exists — provides flag info
    /// for the NL translator.
    parsed_system_specs: Cache<String, Option<CommandSpec>>,
}

impl SpecStore {
    pub fn new(config: SpecConfig) -> Self {
        Self::with_completions_dir(config, crate::compsys_export::completions_dir())
    }

    pub fn with_completions_dir(config: SpecConfig, completions_dir: PathBuf) -> Self {
        let zsh_index = crate::zsh_completion::scan_available_commands();
        tracing::info!("Indexed {} zsh completion files", zsh_index.len());

        let project_cache = Cache::builder()
            .max_capacity(50)
            .time_to_live(Duration::from_secs(300))
            .build();

        let generator_cache = Cache::builder()
            .max_capacity(200)
            .expire_after(GeneratorExpiry)
            .build();

        let discovered_cache = Cache::builder()
            .max_capacity(500)
            .time_to_live(Duration::from_secs(crate::config::DISCOVER_MAX_AGE_SECS))
            .build();

        let parsed_system_specs = Cache::builder()
            .max_capacity(200)
            .time_to_live(Duration::from_secs(3600))
            .build();

        Self {
            project_cache,
            generator_cache,
            discovered_cache,
            config,
            zsh_index: std::sync::RwLock::new(zsh_index),
            completions_dir,
            parsed_system_specs,
        }
    }

    /// Invalidate all caches (project specs, generator outputs, and discovered specs).
    pub async fn clear_caches(&self) {
        self.project_cache.invalidate_all();
        self.generator_cache.invalidate_all();
        self.discovered_cache.invalidate_all();
    }

    pub fn has_system_completion(&self, command: &str) -> bool {
        self.zsh_index
            .read()
            .map(|idx| idx.contains(command))
            .unwrap_or(false)
    }

    /// Refresh the zsh_index by re-scanning fpath directories.
    /// Picks up newly-installed completions (e.g. from `brew install`).
    pub fn refresh_zsh_index(&self) {
        let new_index = crate::zsh_completion::scan_available_commands();
        let count = new_index.len();
        if let Ok(mut idx) = self.zsh_index.write() {
            *idx = new_index;
        }
        tracing::info!("Refreshed zsh_index: {count} completion files");
    }

    /// Get the completions directory path.
    pub fn completions_dir(&self) -> &Path {
        &self.completions_dir
    }

    /// Get the spec config.
    pub fn config(&self) -> &SpecConfig {
        &self.config
    }

    pub async fn run_generator(
        &self,
        generator: &GeneratorSpec,
        cwd: &Path,
        source: crate::spec::SpecSource,
    ) -> Vec<String> {
        generator::run_generator(self, generator, cwd, source).await
    }
}
