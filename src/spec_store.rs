use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use tokio::process::Command;
use tokio::sync::RwLock;

use regex::Regex;
use std::sync::LazyLock;

use crate::config::SpecConfig;
use crate::llm::LlmClient;
use crate::spec::{
    ArgSpec, ArgTemplate, CommandSpec, GeneratorSpec, OptionSpec, SpecSource, SubcommandSpec,
};
use crate::spec_autogen;
use crate::spec_cache;

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
    discovered: RwLock<HashMap<String, spec_cache::DiscoveredSpec>>,
    discovering: RwLock<HashSet<String>>,
    project_cache: Cache<PathBuf, Arc<HashMap<String, CommandSpec>>>,
    generator_cache: Cache<(String, PathBuf), Vec<String>>,
    config: SpecConfig,
    /// Alias → canonical name mapping for builtins
    alias_map: RwLock<HashMap<String, String>>,
    llm_client: Option<Arc<LlmClient>>,
}

impl SpecStore {
    pub fn new(config: SpecConfig, llm_client: Option<Arc<LlmClient>>) -> Self {
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

        // Register minimal shell command specs (only if no full builtin spec exists)
        for (names, template) in [
            (
                &["cd", "mkdir", "rmdir", "pushd"][..],
                ArgTemplate::Directories,
            ),
            (
                &[
                    "cat", "less", "head", "tail", "vim", "nvim", "code", "nano", "bat", "wc",
                    "sort", "uniq", "file", "stat", "touch", "open", "cp", "mv", "rm", "chmod",
                    "chown", "ln", "node", "ruby", "perl", "bash", "sh", "zsh",
                ][..],
                ArgTemplate::FilePaths,
            ),
            (&["export", "unset"][..], ArgTemplate::EnvVars),
        ] {
            for name in names {
                if !builtin.contains_key(*name) {
                    builtin.insert(
                        name.to_string(),
                        CommandSpec {
                            name: name.to_string(),
                            args: vec![ArgSpec {
                                name: "path".into(),
                                template: Some(template.clone()),
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                    );
                }
            }
        }
        for name in [
            "sudo", "env", "nohup", "time", "watch", "xargs", "nice", "ionice", "strace",
        ] {
            if !builtin.contains_key(name) {
                builtin.insert(
                    name.to_string(),
                    CommandSpec {
                        name: name.to_string(),
                        recursive: true,
                        ..Default::default()
                    },
                );
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
            llm_client,
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
        if let Some(discovered) = self.discovered.read().await.get(command) {
            let mut spec = discovered.spec.clone();
            spec.source = SpecSource::Discovered;
            return Some(spec);
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

    /// Trigger background discovery for an unknown command or stale discovered spec.
    /// Returns immediately — the spec will be available on subsequent lookups.
    pub async fn trigger_discovery(self: &Arc<Self>, command: &str, cwd: Option<&Path>) {
        if !self.config.discover_from_help {
            return;
        }

        let command = command.to_string();
        let cwd = cwd.map(Path::to_path_buf);

        // Skip if we already have a builtin spec
        if self.builtin.contains_key(&command) {
            return;
        }

        // Skip fresh discovered specs; stale ones are eligible for refresh.
        if let Some(discovered) = self.discovered.read().await.get(&command) {
            if !spec_cache::is_stale(discovered, self.config.discover_max_age_secs) {
                return;
            }
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
            store.discover_command_impl(&command, cwd.as_deref()).await;
            store.discovering.write().await.remove(&command);
        });
    }

    /// Run the actual discovery process for a command.
    async fn discover_command_impl(&self, command: &str, cwd: Option<&Path>) {
        let timeout = Duration::from_millis(self.config.discover_timeout_ms);
        let args: Vec<String> = Vec::new();

        let help_text = match self.fetch_help_output(command, &args, timeout, cwd).await {
            Some(text) => text,
            None => {
                tracing::debug!("No help output for {command}");
                return;
            }
        };

        let llm_budget = AtomicUsize::new(
            self.llm_client
                .as_ref()
                .map(|c| c.max_calls_per_discovery())
                .unwrap_or(0),
        );

        let mut spec = self
            .parse_with_llm_or_regex(command, &help_text, &llm_budget)
            .await;
        spec.source = SpecSource::Discovered;

        // Skip if we got nothing useful
        if spec.subcommands.is_empty() && spec.options.is_empty() {
            tracing::debug!("No useful spec data from --help for {command}");
            return;
        }

        // Recurse into subcommands if configured
        if self.config.discover_max_depth > 0 && !spec.subcommands.is_empty() {
            self.discover_subcommands(command, &mut spec, cwd, &llm_budget)
                .await;
        }

        // Resolve command path for staleness tracking
        let command_path = self.resolve_command_path(command, cwd).await;

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
            .insert(command.to_string(), discovered);
        tracing::info!("Discovered spec for {command}");
    }

    /// Run `command help_flag` and return the stdout (or stderr as fallback).
    async fn run_help_command(
        &self,
        command: &str,
        args: &[String],
        help_flag: &str,
        timeout: Duration,
        cwd: Option<&Path>,
    ) -> Option<String> {
        let result = tokio::time::timeout(timeout, async {
            let mut cmd = Command::new(command);
            cmd.args(args).arg(help_flag);
            if let Some(cwd) = cwd {
                cmd.current_dir(cwd);
            }
            cmd.stdin(std::process::Stdio::null())
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

    async fn fetch_help_output(
        &self,
        command: &str,
        args: &[String],
        timeout: Duration,
        cwd: Option<&Path>,
    ) -> Option<String> {
        for help_flag in ["--help", "-h"] {
            if let Some(text) = self
                .run_help_command(command, args, help_flag, timeout, cwd)
                .await
            {
                if !text.trim().is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }

    /// Recursively discover subcommand specs by running `command ...subcommand --help`.
    async fn discover_subcommands(
        &self,
        base_command: &str,
        spec: &mut CommandSpec,
        cwd: Option<&Path>,
        llm_budget: &AtomicUsize,
    ) {
        for subcommand in spec.subcommands.iter_mut().take(30) {
            self.discover_subcommand_tree(base_command, &[], subcommand, 1, cwd, llm_budget)
                .await;
        }
    }

    async fn discover_subcommand_tree(
        &self,
        base_command: &str,
        parent_path: &[String],
        subcommand: &mut SubcommandSpec,
        depth: usize,
        cwd: Option<&Path>,
        llm_budget: &AtomicUsize,
    ) {
        if depth > self.config.discover_max_depth {
            return;
        }

        // Skip "help" subcommands — not useful for completions.
        if subcommand.name == "help" {
            return;
        }

        let timeout = Duration::from_millis(self.config.discover_timeout_ms);

        let mut args = parent_path.to_vec();
        args.push(subcommand.name.clone());

        let help_text = self
            .fetch_help_output(base_command, &args, timeout, cwd)
            .await
            .unwrap_or_default();

        if !help_text.trim().is_empty() {
            let sub_spec = self
                .parse_with_llm_or_regex(&subcommand.name, &help_text, llm_budget)
                .await;
            if subcommand.options.is_empty() {
                subcommand.options = sub_spec.options;
            }
            if subcommand.subcommands.is_empty() {
                subcommand.subcommands = sub_spec.subcommands;
            }
            if subcommand.args.is_empty() {
                subcommand.args = sub_spec.args;
            }
            if subcommand.description.is_none() {
                subcommand.description = sub_spec.description;
            }
        }

        if depth >= self.config.discover_max_depth || subcommand.subcommands.is_empty() {
            return;
        }

        let mut next_parent = parent_path.to_vec();
        next_parent.push(subcommand.name.clone());
        for nested in subcommand.subcommands.iter_mut().take(30) {
            Box::pin(self.discover_subcommand_tree(
                base_command,
                &next_parent,
                nested,
                depth + 1,
                cwd,
                llm_budget,
            ))
            .await;
        }
    }

    /// Try LLM parsing first (if available and budget allows), fall back to basic regex.
    async fn parse_with_llm_or_regex(
        &self,
        command_name: &str,
        help_text: &str,
        llm_budget: &AtomicUsize,
    ) -> CommandSpec {
        if let Some(ref llm) = self.llm_client {
            let prev = llm_budget.fetch_sub(1, Ordering::Relaxed);
            if prev > 0 {
                match llm.generate_spec(command_name, help_text).await {
                    Ok(spec) => {
                        tracing::info!("LLM parsed spec for {command_name}");
                        return spec;
                    }
                    Err(e) => {
                        tracing::debug!(
                            "LLM parse failed for {command_name}, falling back to regex: {e}"
                        );
                        llm_budget.fetch_add(1, Ordering::Relaxed);
                    }
                }
            } else {
                // Restore: fetch_sub already decremented past 0 (wraps for usize)
                llm_budget.fetch_add(1, Ordering::Relaxed);
                tracing::debug!("LLM budget exhausted, using basic regex for {command_name}");
            }
        }
        parse_help_basic(command_name, help_text)
    }

    /// Resolve a command name to its full path.
    async fn resolve_command_path(&self, command: &str, cwd: Option<&Path>) -> Option<String> {
        if command.contains('/') {
            let path = Path::new(command);
            if path.is_absolute() {
                return std::fs::canonicalize(path)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string());
            }

            if let Some(cwd) = cwd {
                return std::fs::canonicalize(cwd.join(path))
                    .ok()
                    .map(|p| p.to_string_lossy().to_string());
            }
        }

        let mut cmd = Command::new("which");
        cmd.arg(command);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        let output = cmd.output().await.ok()?;

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

        // Discover specs for CLI tools built by the current project.
        // This is intentionally gated behind trust_project_generators since it executes
        // project-built binaries.
        if self.config.discover_project_cli && self.config.trust_project_generators {
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

/// Minimal best-effort help text parser used when LLM is unavailable.
/// Extracts obvious `--option` lines and `command  description` subcommand lines.
pub fn parse_help_basic(command_name: &str, help_text: &str) -> CommandSpec {
    static OPT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s+(-\w)?(?:\s*,\s*|\s+)?(--[\w][\w.-]*)?\s*(?:[=\s]\s*(\[?<?[\w.|/-]+>?\]?))?\s{2,}(.+)$").unwrap()
    });
    static SUBCMD_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s+([\w][\w.-]*)\s{2,}(.+)$").unwrap());

    let mut options = Vec::new();
    let mut subcommands = Vec::new();
    let mut in_options = false;
    let mut in_commands = false;

    for line in help_text.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();

        // Detect section headers
        if lower.ends_with("options:") || lower.ends_with("flags:") {
            in_options = true;
            in_commands = false;
            continue;
        }
        if lower.ends_with("commands:") || lower.ends_with("subcommands:") {
            in_commands = true;
            in_options = false;
            continue;
        }
        if !trimmed.is_empty()
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && trimmed.ends_with(':')
        {
            in_options = false;
            in_commands = false;
            continue;
        }

        if in_options {
            if let Some(caps) = OPT_RE.captures(line) {
                let short = caps.get(1).map(|m| m.as_str().to_string());
                let long = caps.get(2).map(|m| m.as_str().to_string());
                if long.as_deref() == Some("--help") || long.as_deref() == Some("--version") {
                    continue;
                }
                let takes_arg = caps.get(3).is_some();
                let description = caps.get(4).map(|m| m.as_str().trim().to_string());
                if short.is_some() || long.is_some() {
                    options.push(OptionSpec {
                        short,
                        long,
                        description,
                        takes_arg,
                        ..Default::default()
                    });
                }
            }
        }

        if in_commands {
            if let Some(caps) = SUBCMD_RE.captures(line) {
                let name = caps.get(1).unwrap().as_str().to_string();
                let description = Some(caps.get(2).unwrap().as_str().trim().to_string());
                subcommands.push(SubcommandSpec {
                    name,
                    description,
                    ..Default::default()
                });
            }
        }
    }

    CommandSpec {
        name: command_name.to_string(),
        subcommands,
        options,
        ..Default::default()
    }
}
