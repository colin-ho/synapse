use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use moka::Expiry;
use tokio::process::Command;
use tokio::sync::RwLock;

use regex::Regex;
use std::sync::LazyLock;

use crate::config::SpecConfig;
use crate::llm::LlmClient;
use crate::spec::{CommandSpec, GeneratorSpec, OptionSpec, SpecSource, SubcommandSpec};
use crate::spec_autogen;

/// Commands that must never be run with --help for safety reasons.
const DISCOVERY_BLOCKLIST: &[&str] = &[
    "rm", "dd", "mkfs", "fdisk", "shutdown", "reboot", "halt", "poweroff", "sudo", "su", "doas",
    "login", "passwd", "format", "diskutil",
];

/// Maximum bytes to read from --help stdout.
const MAX_HELP_OUTPUT_BYTES: usize = 256 * 1024;

/// Cached generator output with its per-entry TTL.
#[derive(Clone)]
struct GeneratorCacheEntry {
    items: Vec<String>,
    ttl: Duration,
}

/// Per-entry expiry policy that uses each generator's `cache_ttl_secs`.
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
/// The spec store handles two tiers of specs:
/// 1. User project specs (`.synapse/specs/*.toml`) — highest priority
/// 2. Project auto-generated specs (Makefile, package.json, etc.)
///
/// Discovery of unknown commands writes compsys files directly to the
/// completions directory — there is no intermediate TOML cache.
pub struct SpecStore {
    discovering: RwLock<HashSet<String>>,
    project_cache: Cache<PathBuf, Arc<HashMap<String, CommandSpec>>>,
    generator_cache: Cache<(String, PathBuf), GeneratorCacheEntry>,
    config: SpecConfig,
    llm_client: Option<Arc<LlmClient>>,
    /// Set of command names that have zsh completion files available.
    zsh_index: HashSet<String>,
    /// Directory for generated compsys completion files.
    completions_dir: PathBuf,
}

impl SpecStore {
    pub fn new(config: SpecConfig, llm_client: Option<Arc<LlmClient>>) -> Self {
        Self::with_completions_dir(config, llm_client, crate::compsys_export::completions_dir())
    }

    pub fn with_completions_dir(
        config: SpecConfig,
        llm_client: Option<Arc<LlmClient>>,
        completions_dir: PathBuf,
    ) -> Self {
        // Build the zsh completion filename index (readdir only, sub-millisecond).
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

        Self {
            discovering: RwLock::new(HashSet::new()),
            project_cache,
            generator_cache,
            config,
            llm_client,
            zsh_index,
            completions_dir,
        }
    }

    /// Look up a spec by command name. Only returns project-level specs
    /// (user-defined and auto-generated).
    pub async fn lookup(&self, command: &str, cwd: &Path) -> Option<CommandSpec> {
        let project_specs = self.get_project_specs(cwd).await;
        project_specs.get(command).cloned()
    }

    /// Return all project specs for the given cwd as a Vec (for compsys export).
    pub async fn lookup_all_project_specs(&self, cwd: &Path) -> Vec<CommandSpec> {
        let project_specs = self.get_project_specs(cwd).await;
        project_specs.values().cloned().collect()
    }

    /// Invalidate all caches (project specs and generator outputs).
    pub async fn clear_caches(&self) {
        self.project_cache.invalidate_all();
        self.generator_cache.invalidate_all();
    }

    /// Check if a command already has a completion file (system or generated).
    fn has_completion(&self, command: &str) -> bool {
        self.zsh_index.contains(command)
            || self.completions_dir.join(format!("_{command}")).exists()
    }

    /// Get all available command names for a given cwd.
    pub async fn all_command_names(&self, cwd: &Path) -> Vec<String> {
        let mut seen: HashSet<String> = self.zsh_index.clone();

        let project_specs = self.get_project_specs(cwd).await;
        for key in project_specs.keys() {
            seen.insert(key.clone());
        }

        seen.into_iter().collect()
    }

    /// Trigger background discovery for an unknown command.
    /// Returns immediately — a compsys completion file will be written
    /// to the completions directory when discovery completes.
    pub async fn trigger_discovery(self: &Arc<Self>, command: &str, cwd: Option<&Path>) {
        if !self.config.discover_from_help {
            return;
        }

        let command = command.to_string();
        let cwd = cwd.map(Path::to_path_buf);

        // Skip if a completion already exists (system or generated)
        if self.has_completion(&command) {
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
            store.discover_command_impl(&command, cwd.as_deref()).await;
            store.discovering.write().await.remove(&command);
        });
    }

    /// Write a discovered spec as a compsys completion file.
    /// The compsys file IS the persistent cache — no TOML intermediate.
    fn save_discovered_spec(&self, command: &str, spec: &CommandSpec) {
        if self.zsh_index.contains(command) {
            return; // Don't overwrite existing system completions
        }
        match crate::compsys_export::write_completion_file(spec, &self.completions_dir) {
            Ok(path) => {
                tracing::info!(
                    "Wrote compsys completion for {command} at {}",
                    path.display()
                );
            }
            Err(e) => {
                tracing::warn!("Failed to write compsys completion for {command}: {e}");
            }
        }
    }

    /// Run fast discovery inline and return whether a spec was produced.
    /// Tries completion generators first (structured), then `--help` regex.
    /// No LLM, no subcommand recursion — suitable for the dropdown path.
    pub async fn discover_and_wait(
        self: &Arc<Self>,
        command: &str,
        cwd: Option<&Path>,
        _timeout: Duration,
    ) -> bool {
        // Already have a completion file?
        if self.has_completion(command) {
            return true;
        }

        // Already have a project spec?
        let lookup_cwd = cwd.unwrap_or(Path::new(""));
        if self.lookup(command, lookup_cwd).await.is_some() {
            return true;
        }

        // Same guards as trigger_discovery.
        if !self.config.discover_from_help {
            return false;
        }
        if DISCOVERY_BLOCKLIST.contains(&command) {
            return false;
        }
        if self
            .config
            .discover_blocklist
            .contains(&command.to_string())
        {
            return false;
        }

        // Strategy 1: Try completion generator (structured output from the tool itself).
        let gen_timeout = Duration::from_millis(crate::config::DISCOVER_TIMEOUT_MS);
        if let Some(mut spec) =
            crate::zsh_completion::try_completion_generator(command, gen_timeout).await
        {
            spec.source = SpecSource::Discovered;
            tracing::info!("Completion generator produced spec for {command}");
            self.save_discovered_spec(command, &spec);
            return true;
        }

        // Strategy 2: Parse --help with regex (no LLM, no subcommand recursion).
        let help_timeout = Duration::from_millis(crate::config::DISCOVER_TIMEOUT_MS);
        let args: Vec<String> = Vec::new();
        let help_text = match self
            .fetch_help_output(command, &args, help_timeout, cwd)
            .await
        {
            Some(text) => text,
            None => return false,
        };

        let mut spec = parse_help_basic(command, &help_text);
        spec.source = SpecSource::Discovered;

        if spec.subcommands.is_empty() && spec.options.is_empty() {
            return false;
        }

        self.save_discovered_spec(command, &spec);
        tracing::info!("Fast-discovered spec for {command}");
        true
    }

    /// Run the actual discovery process for a command.
    async fn discover_command_impl(&self, command: &str, cwd: Option<&Path>) {
        // Strategy 1: Try completion generator (structured output from the tool itself).
        let gen_timeout = Duration::from_millis(crate::config::DISCOVER_TIMEOUT_MS);
        if let Some(mut spec) =
            crate::zsh_completion::try_completion_generator(command, gen_timeout).await
        {
            spec.source = SpecSource::Discovered;
            tracing::info!("Completion generator produced spec for {command}");
            self.save_discovered_spec(command, &spec);
            tracing::info!("Discovered spec for {command}");
            return;
        }

        // Strategy 2: Parse --help output (LLM then regex fallback).
        let timeout = Duration::from_millis(crate::config::DISCOVER_TIMEOUT_MS);
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
        if crate::config::DISCOVER_MAX_DEPTH > 0 && !spec.subcommands.is_empty() {
            self.discover_subcommands(command, &mut spec, cwd, &llm_budget)
                .await;
        }

        self.save_discovered_spec(command, &spec);
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
        if depth > crate::config::DISCOVER_MAX_DEPTH {
            return;
        }

        // Skip "help" subcommands — not useful for completions.
        if subcommand.name == "help" {
            return;
        }

        let timeout = Duration::from_millis(crate::config::DISCOVER_TIMEOUT_MS);

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

        if depth >= crate::config::DISCOVER_MAX_DEPTH || subcommand.subcommands.is_empty() {
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
            // Use compare_exchange loop to avoid wrapping to usize::MAX
            let acquired = loop {
                let current = llm_budget.load(Ordering::Relaxed);
                if current == 0 {
                    break false;
                }
                match llm_budget.compare_exchange_weak(
                    current,
                    current - 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break true,
                    Err(_) => continue, // Retry on spurious failure
                }
            };

            if acquired {
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
                tracing::debug!("LLM budget exhausted, using basic regex for {command_name}");
            }
        }
        parse_help_basic(command_name, help_text)
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

        // Resolve project root and load specs on a blocking thread to avoid
        // blocking the async runtime with filesystem I/O.
        let scan_depth = self.config.scan_depth;
        let auto_generate = self.config.auto_generate;
        let cwd_owned = cwd.to_path_buf();

        let mut specs = tokio::task::spawn_blocking(move || {
            let mut specs = HashMap::new();
            let project_root = crate::project::find_project_root(&cwd_owned, scan_depth);
            let scan_root = project_root.as_deref().unwrap_or(&cwd_owned);

            // Auto-generate specs from project files
            if auto_generate {
                let auto_specs = spec_autogen::generate_specs(scan_root, &cwd_owned);
                for mut spec in auto_specs {
                    if !specs.contains_key(&spec.name) {
                        spec.source = SpecSource::ProjectAuto;
                        specs.insert(spec.name.clone(), spec);
                    }
                }
            }

            specs
        })
        .await
        .unwrap_or_default();

        let project_root = crate::project::find_project_root(cwd, self.config.scan_depth);
        let scan_root = project_root.as_deref().unwrap_or(cwd);

        // Discover specs for CLI tools built by the current project.
        // This is intentionally gated behind trust_project_generators since it executes
        // project-built binaries.
        if self.config.discover_project_cli && self.config.trust_project_generators {
            let cli_specs = spec_autogen::discover_project_cli_specs(
                scan_root,
                crate::config::DISCOVER_TIMEOUT_MS,
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
    pub async fn run_generator(
        &self,
        generator: &GeneratorSpec,
        cwd: &Path,
        _source: SpecSource,
    ) -> Vec<String> {
        let cache_key = (generator.command.clone(), cwd.to_path_buf());

        if let Some(cached) = self.generator_cache.get(&cache_key).await {
            return cached.items;
        }

        let timeout = Duration::from_millis(
            generator
                .timeout_ms
                .min(crate::config::GENERATOR_TIMEOUT_MS),
        );

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

        // Cache the result with the generator's own TTL
        let ttl = Duration::from_secs(generator.cache_ttl_secs);
        let entry = GeneratorCacheEntry {
            items: result.clone(),
            ttl,
        };
        self.generator_cache.insert(cache_key, entry).await;

        result
    }
}

/// Minimal best-effort help text parser used when LLM is unavailable.
/// Extracts obvious `--option` lines and `command  description` subcommand lines.
pub fn parse_help_basic(command_name: &str, help_text: &str) -> CommandSpec {
    static OPT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(-\w)?(?:\s*,\s*|\s+)?(--[\w][\w.-]*)?\s*(?:[=\s]\s*(\[?<?[\w.|/-]+>?\]?))?\s{2,}(.+)$").unwrap()
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

        // Match option lines inside an "Options:" section, or anywhere if no section
        // header was found (many commands like htop list options without a header).
        if in_options || !in_commands {
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
                    continue;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_parse_help_basic_no_section_header() {
        // htop-style help: options listed without an "Options:" header.
        let help = "\
htop 3.4.1
(C) 2004-2019 Hisham Muhammad. (C) 2020-2025 htop dev team.
Released under the GNU GPLv2+.

-C --no-color                   Use a monochrome color scheme
-d --delay=DELAY                Set the delay between updates, in tenths of seconds
-F --filter=FILTER              Show only the commands matching the given filter
-h --help                       Print this help screen
-M --no-mouse                   Disable the mouse
-t --tree                       Show the tree view (can be combined with -s)
-V --version                    Print version info
";
        let spec = parse_help_basic("htop", help);
        let long_names: Vec<&str> = spec
            .options
            .iter()
            .filter_map(|o| o.long.as_deref())
            .collect();
        // --help and --version are excluded by the parser
        assert!(
            long_names.contains(&"--no-color"),
            "missing --no-color: {long_names:?}"
        );
        assert!(
            long_names.contains(&"--tree"),
            "missing --tree: {long_names:?}"
        );
        assert!(
            long_names.contains(&"--delay"),
            "missing --delay: {long_names:?}"
        );
        assert!(
            !long_names.contains(&"--help"),
            "--help should be excluded: {long_names:?}"
        );
        assert!(
            spec.options.len() >= 4,
            "expected at least 4 options, got {}",
            spec.options.len()
        );
    }

    #[tokio::test]
    async fn test_lookup_returns_project_auto_specs() {
        let tmp = tempfile::tempdir().unwrap();
        let compose_path = tmp.path().join("docker-compose.yml");
        std::fs::write(
            &compose_path,
            "services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n",
        )
        .unwrap();

        let config = SpecConfig::default();
        let store = SpecStore::new(config, None);

        let spec = store.lookup("docker", tmp.path()).await;
        let spec = spec.expect("docker spec should exist from docker-compose.yml");

        // Should have compose subcommand from auto-gen
        let sub_names: Vec<&str> = spec.subcommands.iter().map(|s| s.name.as_str()).collect();
        assert!(
            sub_names.contains(&"compose"),
            "missing 'compose': {sub_names:?}"
        );
    }
}
