use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use tokio::process::Command;
use tokio::sync::RwLock;

use crate::config::SpecConfig;
use crate::spec::{CommandSpec, GeneratorSpec, SpecSource};
use crate::spec_autogen;

/// Manages loading, caching, and resolution of command specs.
pub struct SpecStore {
    builtin: HashMap<String, CommandSpec>,
    project_cache: Cache<PathBuf, Arc<HashMap<String, CommandSpec>>>,
    generator_cache: Cache<(String, PathBuf), Vec<String>>,
    config: SpecConfig,
    /// Alias → canonical name mapping for builtins
    alias_map: RwLock<HashMap<String, String>>,
}

impl SpecStore {
    pub fn new(config: SpecConfig) -> Self {
        let mut builtin = HashMap::new();
        let mut alias_map = HashMap::new();

        // Load embedded built-in specs
        let specs_raw: &[(&str, &str)] = &[
            ("git", include_str!("../specs/builtin/git.toml")),
            ("cargo", include_str!("../specs/builtin/cargo.toml")),
            ("npm", include_str!("../specs/builtin/npm.toml")),
            ("docker", include_str!("../specs/builtin/docker.toml")),
        ];

        for (name, toml_str) in specs_raw {
            match toml::from_str::<CommandSpec>(toml_str) {
                Ok(mut spec) => {
                    spec.source = SpecSource::Builtin;
                    // Register aliases
                    for alias in &spec.aliases {
                        alias_map.insert(alias.clone(), name.to_string());
                    }
                    builtin.insert(name.to_string(), spec);
                }
                Err(e) => {
                    tracing::warn!("Failed to parse builtin spec {name}: {e}");
                }
            }
        }

        tracing::info!("Loaded {} builtin specs", builtin.len());

        let project_cache = Cache::builder()
            .max_capacity(50)
            .time_to_live(Duration::from_secs(300))
            .build();

        let generator_cache = Cache::builder()
            .max_capacity(200)
            .time_to_live(Duration::from_secs(30))
            .build();

        Self {
            builtin,
            project_cache,
            generator_cache,
            config,
            alias_map: RwLock::new(alias_map),
        }
    }

    /// Look up a spec by command name, checking project specs first (higher priority).
    pub async fn lookup(&self, command: &str, cwd: &Path) -> Option<CommandSpec> {
        // Check project specs first (user > auto > builtin)
        let project_specs = self.get_project_specs(cwd).await;
        if let Some(spec) = project_specs.get(command) {
            return Some(spec.clone());
        }

        // Check builtin specs
        if let Some(spec) = self.builtin.get(command) {
            return Some(spec.clone());
        }

        // Check aliases
        let alias_map = self.alias_map.read().await;
        if let Some(canonical) = alias_map.get(command) {
            if let Some(spec) = self.builtin.get(canonical) {
                return Some(spec.clone());
            }
        }

        None
    }

    /// Get all available command names for a given cwd.
    pub async fn all_command_names(&self, cwd: &Path) -> Vec<String> {
        let mut names: Vec<String> = self.builtin.keys().cloned().collect();

        let project_specs = self.get_project_specs(cwd).await;
        for key in project_specs.keys() {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }

        names
    }

    /// Get project-specific specs (user-defined + auto-generated), cached.
    async fn get_project_specs(&self, cwd: &Path) -> Arc<HashMap<String, CommandSpec>> {
        if !self.config.enabled {
            return Arc::new(HashMap::new());
        }

        let key = cwd.to_path_buf();
        if let Some(cached) = self.project_cache.get(&key).await {
            return cached;
        }

        let mut specs = HashMap::new();

        // Load user-defined project specs from .synapse/specs/*.toml
        let spec_dir = cwd.join(".synapse").join("specs");
        if spec_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&spec_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "toml") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            match toml::from_str::<CommandSpec>(&content) {
                                Ok(mut spec) => {
                                    spec.source = SpecSource::ProjectUser;
                                    specs.insert(spec.name.clone(), spec);
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to parse project spec {}: {e}",
                                        path.display()
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Auto-generate specs from project files
        if self.config.auto_generate {
            let auto_specs = spec_autogen::generate_specs(cwd);
            for mut spec in auto_specs {
                // Don't override user-defined specs
                if !specs.contains_key(&spec.name) {
                    spec.source = SpecSource::ProjectAuto;
                    specs.insert(spec.name.clone(), spec);
                }
            }
        }

        let specs = Arc::new(specs);
        self.project_cache.insert(key, specs.clone()).await;
        specs
    }

    /// Run a generator command and return the results.
    /// For project-level specs (ProjectUser), generators are only executed if
    /// `trust_project_generators` is enabled in config to prevent arbitrary
    /// command execution from untrusted repos.
    pub async fn run_generator(
        &self,
        generator: &GeneratorSpec,
        cwd: &Path,
        source: SpecSource,
    ) -> Vec<String> {
        // Block generators from project-level user specs unless explicitly trusted
        if source == SpecSource::ProjectUser && !self.config.trust_project_generators {
            tracing::debug!(
                "Skipping generator from untrusted project spec: {}",
                generator.command
            );
            return Vec::new();
        }

        let cache_key = (generator.command.clone(), cwd.to_path_buf());

        // Check generator cache with the generator's own TTL
        if let Some(cached) = self.generator_cache.get(&cache_key).await {
            return cached;
        }

        let timeout =
            Duration::from_millis(generator.timeout_ms.min(self.config.generator_timeout_ms));

        let result = match tokio::time::timeout(timeout, async {
            Command::new("sh")
                .arg("-c")
                .arg(&generator.command)
                .current_dir(cwd)
                .output()
                .await
        })
        .await
        {
            Ok(Ok(output)) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let split_on = &generator.split_on;
                let items: Vec<String> = stdout
                    .split(split_on.as_str())
                    .filter_map(|line| {
                        let mut item = line.trim().to_string();
                        if item.is_empty() {
                            return None;
                        }
                        // Apply strip_prefix if configured
                        if let Some(prefix) = &generator.strip_prefix {
                            if let Some(stripped) = item.strip_prefix(prefix.as_str()) {
                                item = stripped.to_string();
                            }
                        }
                        if item.is_empty() {
                            None
                        } else {
                            Some(item)
                        }
                    })
                    .collect();
                items
            }
            Ok(Ok(output)) => {
                tracing::debug!(
                    "Generator command failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                Vec::new()
            }
            Ok(Err(e)) => {
                tracing::debug!("Generator command error: {e}");
                Vec::new()
            }
            Err(_) => {
                tracing::debug!("Generator command timed out: {}", generator.command);
                Vec::new()
            }
        };

        // Cache the result
        self.generator_cache.insert(cache_key, result.clone()).await;

        result
    }
}
