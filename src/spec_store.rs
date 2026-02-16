use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use tokio::process::Command;
use tokio::sync::RwLock;

use crate::config::SpecConfig;
use crate::spec::{CommandSpec, GeneratorSpec, SpecSource};
use crate::spec_autogen;
use crate::{help_parser, spec_cache};

/// Commands that must never be run with --help for safety reasons.
const DISCOVERY_BLOCKLIST: &[&str] = &[
    "rm", "dd", "mkfs", "fdisk", "shutdown", "reboot", "halt", "poweroff", "sudo", "su", "doas",
    "login", "passwd", "format", "diskutil",
];

/// Maximum bytes to read from --help stdout.
const MAX_HELP_OUTPUT_BYTES: usize = 256 * 1024;

/// Manages loading, caching, and resolution of command specs.
pub struct SpecStore {
    builtin: HashMap<String, CommandSpec>,
    discovered: RwLock<HashMap<String, CommandSpec>>,
    discovering: RwLock<HashSet<String>>,
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
            ("ls", include_str!("../specs/builtin/ls.toml")),
            ("grep", include_str!("../specs/builtin/grep.toml")),
            ("find", include_str!("../specs/builtin/find.toml")),
            ("curl", include_str!("../specs/builtin/curl.toml")),
            ("ssh", include_str!("../specs/builtin/ssh.toml")),
            ("python", include_str!("../specs/builtin/python.toml")),
            ("pip", include_str!("../specs/builtin/pip.toml")),
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

        // Load discovered specs from disk
        let discovered = spec_cache::load_all_discovered();
        if !discovered.is_empty() {
            tracing::info!("Loaded {} discovered specs from disk", discovered.len());
        }

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
            discovered: RwLock::new(discovered),
            discovering: RwLock::new(HashSet::new()),
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
        drop(alias_map);

        // Check discovered specs (lowest priority)
        if let Some(spec) = self.discovered.read().await.get(command) {
            return Some(spec.clone());
        }

        None
    }

    /// Get all available command names for a given cwd.
    pub async fn all_command_names(&self, cwd: &Path) -> Vec<String> {
        let mut names: Vec<String> = self.builtin.keys().cloned().collect();

        // Add discovered command names
        for key in self.discovered.read().await.keys() {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }

        let project_specs = self.get_project_specs(cwd).await;
        for key in project_specs.keys() {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }

        names
    }

    /// Trigger background discovery for an unknown command.
    /// Returns immediately — the spec will be available on subsequent lookups.
    pub async fn trigger_discovery(self: &Arc<Self>, command: &str) {
        if !self.config.discover_from_help {
            return;
        }

        let command = command.to_string();

        // Skip if we already have a spec or are already discovering
        if self.builtin.contains_key(&command) {
            return;
        }
        if self.discovered.read().await.contains_key(&command) {
            return;
        }

        // Check blocklist
        if DISCOVERY_BLOCKLIST.contains(&command.as_str()) {
            return;
        }
        if self.config.discover_blocklist.contains(&command) {
            return;
        }

        // Check if already discovering
        {
            let mut discovering = self.discovering.write().await;
            if !discovering.insert(command.clone()) {
                return; // Already in progress
            }
        }

        let store = Arc::clone(self);
        tokio::spawn(async move {
            store.discover_command_impl(&command).await;
            store.discovering.write().await.remove(&command);
        });
    }

    /// Run the actual discovery process for a command.
    async fn discover_command_impl(&self, command: &str) {
        let timeout = Duration::from_millis(self.config.discover_timeout_ms);

        // Try --help first, then -h
        let help_text = match self.run_help_command(command, "--help", timeout).await {
            Some(text) if !text.trim().is_empty() => text,
            _ => match self.run_help_command(command, "-h", timeout).await {
                Some(text) if !text.trim().is_empty() => text,
                _ => {
                    tracing::debug!("No help output for {command}");
                    return;
                }
            },
        };

        let mut spec = help_parser::parse_help_output(command, &help_text);
        spec.source = SpecSource::Discovered;

        // Skip if we got nothing useful
        if spec.subcommands.is_empty() && spec.options.is_empty() {
            tracing::debug!("No useful spec data from --help for {command}");
            return;
        }

        // Recurse into subcommands if configured
        if self.config.discover_max_depth > 0 && !spec.subcommands.is_empty() {
            self.discover_subcommands(command, &mut spec, 1).await;
        }

        // Resolve command path for staleness tracking
        let command_path = self.resolve_command_path(command).await;

        // Save to disk
        let discovered = spec_cache::DiscoveredSpec {
            discovered_at: Some(chrono::Utc::now().to_rfc3339()),
            command_path,
            version_output: None,
            spec: spec.clone(),
        };

        if let Err(e) = spec_cache::save_discovered(&discovered) {
            tracing::warn!("Failed to save discovered spec for {command}: {e}");
        }

        // Insert into in-memory cache
        self.discovered
            .write()
            .await
            .insert(command.to_string(), spec);
        tracing::info!("Discovered spec for {command}");
    }

    /// Run `command help_flag` and return the stdout (or stderr as fallback).
    async fn run_help_command(
        &self,
        command: &str,
        help_flag: &str,
        timeout: Duration,
    ) -> Option<String> {
        let result = tokio::time::timeout(timeout, async {
            Command::new(command)
                .arg(help_flag)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .await
        })
        .await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                stdout.truncate(MAX_HELP_OUTPUT_BYTES);

                // Some tools print help to stderr
                if stdout.trim().is_empty() {
                    let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    stderr.truncate(MAX_HELP_OUTPUT_BYTES);
                    let lower = stderr.to_lowercase();
                    if lower.contains("usage") || lower.contains("options") {
                        return Some(stderr);
                    }
                }

                if !stdout.trim().is_empty() {
                    Some(stdout)
                } else {
                    None
                }
            }
            Ok(Err(e)) => {
                tracing::debug!("Failed to run {command} {help_flag}: {e}");
                None
            }
            Err(_) => {
                tracing::debug!("{command} {help_flag} timed out");
                None
            }
        }
    }

    /// Recursively discover subcommand specs by running `command subcommand --help`.
    async fn discover_subcommands(&self, base_command: &str, spec: &mut CommandSpec, depth: usize) {
        if depth > self.config.discover_max_depth {
            return;
        }

        let timeout = Duration::from_millis(self.config.discover_timeout_ms);

        // Limit concurrency: only process up to 30 subcommands
        let subcmd_names: Vec<String> = spec
            .subcommands
            .iter()
            .take(30)
            .map(|s| s.name.clone())
            .collect();

        for subcmd_name in &subcmd_names {
            // Skip "help" subcommand — not useful for completions
            if subcmd_name == "help" {
                continue;
            }

            let result = tokio::time::timeout(timeout, async {
                Command::new(base_command)
                    .arg(subcmd_name)
                    .arg("--help")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .output()
                    .await
            })
            .await;

            if let Ok(Ok(output)) = result {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                stdout.truncate(MAX_HELP_OUTPUT_BYTES);

                if stdout.trim().is_empty() {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let lower = stderr.to_lowercase();
                    if lower.contains("usage") || lower.contains("options") {
                        stdout = stderr;
                        stdout.truncate(MAX_HELP_OUTPUT_BYTES);
                    }
                }

                if !stdout.trim().is_empty() {
                    let sub_spec = help_parser::parse_help_output(subcmd_name, &stdout);

                    // Merge parsed data into the existing subcommand entry
                    if let Some(existing) =
                        spec.subcommands.iter_mut().find(|s| s.name == *subcmd_name)
                    {
                        if existing.options.is_empty() {
                            existing.options = sub_spec.options;
                        }
                        if existing.subcommands.is_empty() {
                            existing.subcommands = sub_spec.subcommands;
                        }
                        if existing.args.is_empty() {
                            existing.args = sub_spec.args;
                        }
                        if existing.description.is_none() {
                            existing.description = sub_spec.description;
                        }
                    }
                }
            }
        }
    }

    /// Resolve a command name to its full path via `which`.
    async fn resolve_command_path(&self, command: &str) -> Option<String> {
        let output = Command::new("which").arg(command).output().await.ok()?;

        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
        None
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

        // Resolve project root so specs are found even when cwd is a subdirectory
        let project_root = crate::project::find_project_root(cwd, self.config.scan_depth);
        let scan_root = project_root.as_deref().unwrap_or(cwd);

        // Load user-defined project specs from .synapse/specs/*.toml
        let spec_dir = scan_root.join(".synapse").join("specs");
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
            let auto_specs = spec_autogen::generate_specs(scan_root);
            for mut spec in auto_specs {
                // Don't override user-defined specs
                if !specs.contains_key(&spec.name) {
                    spec.source = SpecSource::ProjectAuto;
                    specs.insert(spec.name.clone(), spec);
                }
            }
        }

        // Discover specs for CLI tools built by the current project
        if self.config.discover_project_cli {
            let cli_specs = spec_autogen::discover_project_cli_specs(
                scan_root,
                self.config.discover_timeout_ms,
            )
            .await;
            for mut spec in cli_specs {
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
