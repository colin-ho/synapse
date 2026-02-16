use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moka::future::Cache;
use tokio::process::Command;

use crate::completion_context::{CompletionContext, ExpectedType, Position};
use crate::config::LlmConfig;
use crate::llm::{scrub_home_paths, LlmClient};
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};
use crate::spec::{find_option, ArgSpec, CommandSpec, OptionSpec, SubcommandSpec};

const CACHE_TTL_SECONDS: u64 = 60;
const CACHE_MAX_ENTRIES: u64 = 200;
const MAX_LLM_VALUES: usize = 5;

#[derive(Clone, Copy)]
enum GathererKind {
    Git,
    Docker,
    Ssh,
}

struct ContextRegistry {
    gatherers: HashMap<&'static str, GathererKind>,
}

impl ContextRegistry {
    fn new() -> Self {
        let mut gatherers = HashMap::new();
        gatherers.insert("git", GathererKind::Git);
        gatherers.insert("docker", GathererKind::Docker);
        gatherers.insert("ssh", GathererKind::Ssh);
        gatherers.insert("scp", GathererKind::Ssh);
        gatherers.insert("sftp", GathererKind::Ssh);
        Self { gatherers }
    }

    fn gatherer_for(&self, command: &str) -> Option<GathererKind> {
        self.gatherers.get(command).copied()
    }
}

impl Default for ContextRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct SpecHints {
    command_description: Option<String>,
    option_name: Option<String>,
    option_description: Option<String>,
    argument_index: Option<usize>,
    argument_name: Option<String>,
    argument_description: Option<String>,
}

#[derive(Clone, Copy)]
struct ActiveSpecView<'a> {
    description: Option<&'a String>,
    options: &'a [OptionSpec],
    args: &'a [ArgSpec],
}

pub struct LlmArgumentProvider {
    client: Arc<LlmClient>,
    context_registry: ContextRegistry,
    cache: Cache<String, Vec<String>>,
    context_timeout: Duration,
    max_context_chars: usize,
    scrub_paths: bool,
}

impl LlmArgumentProvider {
    pub fn new(client: Arc<LlmClient>, llm_config: &LlmConfig, scrub_paths: bool) -> Self {
        let max_context_chars = llm_config.arg_max_context_tokens.saturating_mul(4).max(512);
        Self {
            client,
            context_registry: ContextRegistry::new(),
            cache: Cache::builder()
                .max_capacity(CACHE_MAX_ENTRIES)
                .time_to_live(Duration::from_secs(CACHE_TTL_SECONDS))
                .build(),
            context_timeout: Duration::from_millis(llm_config.arg_context_timeout_ms),
            max_context_chars,
            scrub_paths,
        }
    }

    fn should_activate(ctx: &CompletionContext) -> bool {
        matches!(
            ctx.position,
            Position::Argument { .. } | Position::OptionValue { .. }
        ) && matches!(ctx.expected_type, ExpectedType::Any)
            && ctx.command.is_some()
    }

    async fn gather_spec_hints(&self, request: &ProviderRequest) -> SpecHints {
        let ctx = request.completion();
        let Some(command) = ctx.command.as_deref() else {
            return SpecHints::default();
        };

        let mut hints = SpecHints::default();
        if let Position::OptionValue { option } = &ctx.position {
            hints.option_name = Some(option.clone());
        }
        if let Position::Argument { index } = &ctx.position {
            hints.argument_index = Some(*index);
        }

        let cwd = Path::new(&request.cwd);
        let Some(spec) = request.spec_store.lookup(command, cwd).await else {
            return hints;
        };

        let view = active_spec_view(&spec, &ctx.subcommand_path);
        hints.command_description = view.description.cloned();

        if let Some(option_name) = hints.option_name.as_deref() {
            if let Some(option) = find_option(view.options, option_name) {
                hints.option_description = option.description.clone();
            }
        }

        if let Some(index) = hints.argument_index {
            if let Some(arg) = arg_for_index(view.args, index) {
                hints.argument_name = Some(arg.name.clone());
                hints.argument_description = arg.description.clone();
            }
        }

        hints
    }

    async fn gather_command_context(
        &self,
        command: &str,
        ctx: &CompletionContext,
        cwd: &Path,
    ) -> String {
        let raw = match self.context_registry.gatherer_for(command) {
            Some(GathererKind::Git) => self.gather_git_context(ctx, cwd).await,
            Some(GathererKind::Docker) => self.gather_docker_context(cwd).await,
            Some(GathererKind::Ssh) => self.gather_ssh_context().await,
            None => String::new(),
        };

        truncate_chars(&self.scrub_context(&raw), self.max_context_chars)
    }

    async fn gather_git_context(&self, ctx: &CompletionContext, cwd: &Path) -> String {
        let (branch, commits, tags) = tokio::join!(
            run_command(
                cwd,
                self.context_timeout,
                "git",
                &["rev-parse", "--abbrev-ref", "HEAD"]
            ),
            run_command(
                cwd,
                self.context_timeout,
                "git",
                &["log", "--oneline", "-8"]
            ),
            run_command(
                cwd,
                self.context_timeout,
                "git",
                &["tag", "--sort=-creatordate"]
            ),
        );

        let mut sections = Vec::new();
        if let Some(branch) = branch {
            let branch = self.scrub_context(&branch);
            sections.push(format!("Current branch:\n{}", branch.trim()));
        }
        if let Some(commits) = commits {
            let commits = self.scrub_context(&commits);
            sections.push(format!(
                "Recent commits:\n{}",
                take_lines(commits.trim(), 8)
            ));
        }
        if let Some(tags) = tags {
            let tags = self.scrub_context(&tags);
            sections.push(format!("Recent tags:\n{}", take_lines(tags.trim(), 10)));
        }

        if is_git_commit_message(ctx) {
            let (diff_stat, diff_detail) = tokio::join!(
                run_command(
                    cwd,
                    self.context_timeout,
                    "git",
                    &["diff", "--staged", "--stat"]
                ),
                run_command(
                    cwd,
                    self.context_timeout,
                    "git",
                    &["diff", "--staged", "--no-color"]
                ),
            );

            if let Some(stat) = diff_stat {
                let stat = self.scrub_context(&stat);
                sections.push(format!(
                    "Git staged changes:\n{}",
                    truncate_chars(&stat, 1_500)
                ));
            }
            if let Some(detail) = diff_detail {
                let detail = self.scrub_context(&detail);
                sections.push(format!(
                    "Staged diff preview:\n{}",
                    truncate_chars(&detail, self.max_context_chars.saturating_div(2))
                ));
            }
        }

        sections.join("\n\n")
    }

    async fn gather_docker_context(&self, cwd: &Path) -> String {
        let (containers, images) = tokio::join!(
            run_command(
                cwd,
                self.context_timeout,
                "docker",
                &["ps", "--format", "{{.Names}}"]
            ),
            run_command(
                cwd,
                self.context_timeout,
                "docker",
                &["images", "--format", "{{.Repository}}:{{.Tag}}"]
            ),
        );

        let mut sections = Vec::new();
        if let Some(containers) = containers {
            let containers = self.scrub_context(&containers);
            sections.push(format!(
                "Running containers:\n{}",
                take_lines(containers.trim(), 20)
            ));
        }
        if let Some(images) = images {
            let images = self.scrub_context(&images);
            sections.push(format!(
                "Available images:\n{}",
                take_lines(images.trim(), 20)
            ));
        }
        sections.join("\n\n")
    }

    async fn gather_ssh_context(&self) -> String {
        let Some(home) = dirs::home_dir() else {
            return String::new();
        };
        let config_path = home.join(".ssh").join("config");
        let Ok(contents) = std::fs::read_to_string(config_path) else {
            return String::new();
        };

        let hosts = extract_ssh_hosts(&contents);
        if hosts.is_empty() {
            String::new()
        } else {
            format!(
                "SSH hosts from ~/.ssh/config:\n{}",
                hosts.into_iter().take(40).collect::<Vec<_>>().join("\n")
            )
        }
    }

    fn compose_additional_context(&self, hints: &SpecHints, command_context: &str) -> String {
        let mut sections = Vec::new();
        if let Some(desc) = hints.command_description.as_deref() {
            sections.push(format!("Command description: {desc}"));
        }
        if let (Some(name), Some(desc)) = (
            hints.option_name.as_deref(),
            hints.option_description.as_deref(),
        ) {
            sections.push(format!("Option {name} description: {desc}"));
        }
        if let Some(index) = hints.argument_index {
            if let Some(desc) = hints.argument_description.as_deref() {
                let name = hints.argument_name.as_deref().unwrap_or("argument");
                sections.push(format!("Argument #{index} ({name}) description: {desc}"));
            }
        }
        if !command_context.is_empty() {
            sections.push(command_context.to_string());
        }

        truncate_chars(&sections.join("\n\n"), self.max_context_chars)
    }

    fn build_prompt(
        &self,
        request: &ProviderRequest,
        hints: &SpecHints,
        additional_context: &str,
    ) -> String {
        let ctx = request.completion();
        let command = ctx.command.as_deref().unwrap_or_default();
        let command_path = if ctx.subcommand_path.is_empty() {
            command.to_string()
        } else {
            format!("{command} {}", ctx.subcommand_path.join(" "))
        };

        let position_line = match (&ctx.position, hints.option_name.as_deref()) {
            (Position::OptionValue { .. }, Some(option)) => {
                format!("Current option: {option}")
            }
            (Position::Argument { index }, _) => format!("Argument position: {index}"),
            _ => "Current position: argument value".to_string(),
        };

        let partial = sanitize_prompt_field(&ctx.partial);
        let recent_commands = format_recent_commands(&request.recent_commands, 8);
        let context_block = if additional_context.is_empty() {
            "(none)".to_string()
        } else {
            additional_context.to_string()
        };

        format!(
            "You are a terminal argument value predictor. Suggest the most likely value for the current argument position.\n\nCommand: {command_path}\n{position_line}\nPartial input: \"{partial}\"\nWorking directory: {}\nRecent commands:\n{recent_commands}\n\nContext:\n{context_block}\n\nRespond with ONLY the argument value (no quotes, no explanation). If multiple values are likely, separate them with newlines (max 5).",
            sanitize_prompt_field(&request.cwd)
        )
    }

    fn cache_key(
        &self,
        ctx: &CompletionContext,
        command: &str,
        recent_commands: &[String],
        additional_context: &str,
    ) -> String {
        let slot = match &ctx.position {
            Position::OptionValue { option } => format!("opt:{option}"),
            Position::Argument { index } => format!("arg:{index}"),
            _ => "unknown".to_string(),
        };

        let mut hasher = DefaultHasher::new();
        additional_context.hash(&mut hasher);
        for cmd in recent_commands.iter().take(8) {
            cmd.hash(&mut hasher);
        }
        let context_hash = hasher.finish();

        format!(
            "{command}|{}|{slot}|{}|{context_hash:016x}",
            ctx.subcommand_path.join("/"),
            ctx.partial
        )
    }

    fn values_to_suggestions(
        &self,
        ctx: &CompletionContext,
        buffer: &str,
        values: &[String],
        max: NonZeroUsize,
        description_hint: Option<&str>,
    ) -> Vec<ProviderSuggestion> {
        let mut suggestions = Vec::new();
        let mut seen = HashSet::new();
        let commit_message_mode = is_git_commit_message(ctx);

        for (idx, raw_value) in values.iter().enumerate() {
            let value = raw_value.trim();
            if value.is_empty() {
                continue;
            }

            if !ctx.partial.is_empty() && !starts_with_ignore_ascii_case(value, &ctx.partial) {
                continue;
            }

            let rendered_value = if commit_message_mode && ctx.partial.is_empty() {
                shell_quote_double(value)
            } else {
                value.to_string()
            };
            let text = format!("{}{}", ctx.prefix, rendered_value);
            if text == buffer || !text.starts_with(buffer) {
                continue;
            }

            if !seen.insert(text.clone()) {
                continue;
            }

            let score = (0.92 - idx as f64 * 0.07).max(0.55);
            suggestions.push(ProviderSuggestion {
                text,
                source: SuggestionSource::Llm,
                score,
                description: description_hint.map(str::to_string),
                kind: SuggestionKind::Argument,
            });

            if suggestions.len() >= max.get() {
                break;
            }
        }

        suggestions
    }

    fn scrub_context(&self, text: &str) -> String {
        if self.scrub_paths {
            scrub_home_paths(text)
        } else {
            text.to_string()
        }
    }
}

#[async_trait]
impl SuggestionProvider for LlmArgumentProvider {
    async fn suggest(
        &self,
        request: &ProviderRequest,
        max: NonZeroUsize,
    ) -> Vec<ProviderSuggestion> {
        let ctx = request.completion();
        if !Self::should_activate(ctx) {
            return Vec::new();
        }

        let Some(command) = ctx.command.as_deref() else {
            return Vec::new();
        };

        let spec_hints = self.gather_spec_hints(request).await;
        let command_context = self
            .gather_command_context(command, ctx, Path::new(&request.cwd))
            .await;
        let additional_context = self.compose_additional_context(&spec_hints, &command_context);
        let cache_key = self.cache_key(ctx, command, &request.recent_commands, &additional_context);

        let values = if let Some(cached) = self.cache.get(&cache_key).await {
            cached
        } else {
            let prompt = self.build_prompt(request, &spec_hints, &additional_context);
            let generated = match self
                .client
                .suggest_argument_values(&prompt, MAX_LLM_VALUES)
                .await
            {
                Ok(values) => values,
                Err(error) => {
                    tracing::debug!("LLM argument suggestion failed: {error}");
                    return Vec::new();
                }
            };

            if generated.is_empty() {
                return Vec::new();
            }

            self.cache.insert(cache_key, generated.clone()).await;
            generated
        };

        self.values_to_suggestions(
            ctx,
            &request.buffer,
            &values,
            max,
            spec_hints.option_description.as_deref(),
        )
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Llm
    }

    fn is_available(&self) -> bool {
        true
    }
}

async fn run_command(
    cwd: &Path,
    timeout: Duration,
    command: &str,
    args: &[&str],
) -> Option<String> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            tracing::trace!("Context command failed ({command}): {error}");
            return None;
        }
        Err(_) => {
            tracing::trace!("Context command timed out ({command})");
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            tracing::trace!(
                "Context command failed ({command}) with status {}",
                output.status
            );
        } else {
            tracing::trace!("Context command failed ({command}): {stderr}");
        }
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return Some(stdout);
    }

    None
}

fn active_spec_view<'a>(spec: &'a CommandSpec, path: &[String]) -> ActiveSpecView<'a> {
    let mut view = ActiveSpecView {
        description: spec.description.as_ref(),
        options: &spec.options,
        args: &spec.args,
    };
    let mut current_subcommands: &[SubcommandSpec] = &spec.subcommands;

    for segment in path {
        let Some(subcommand) = current_subcommands.iter().find(|sub| sub.name == *segment) else {
            break;
        };
        view = ActiveSpecView {
            description: subcommand.description.as_ref(),
            options: &subcommand.options,
            args: &subcommand.args,
        };
        current_subcommands = &subcommand.subcommands;
    }

    view
}

fn arg_for_index(args: &[ArgSpec], index: usize) -> Option<&ArgSpec> {
    if index < args.len() {
        return args.get(index);
    }
    args.last()
}

fn format_recent_commands(commands: &[String], max_lines: usize) -> String {
    if commands.is_empty() {
        return "(none)".to_string();
    }

    commands
        .iter()
        .take(max_lines)
        .map(|cmd| sanitize_prompt_field(cmd))
        .collect::<Vec<_>>()
        .join("\n")
}

fn sanitize_prompt_field(value: &str) -> String {
    value.replace(['\t', '\n'], " ").trim().to_string()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}\n...[truncated]")
    } else {
        value.to_string()
    }
}

fn take_lines(value: &str, max_lines: usize) -> String {
    value.lines().take(max_lines).collect::<Vec<_>>().join("\n")
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len()
        && value.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn is_git_commit_message(ctx: &CompletionContext) -> bool {
    if ctx.command.as_deref() != Some("git") {
        return false;
    }
    if ctx
        .subcommand_path
        .first()
        .is_none_or(|sub| sub != "commit")
    {
        return false;
    }
    matches!(
        &ctx.position,
        Position::OptionValue { option } if option == "-m" || option == "--message"
    )
}

fn shell_quote_double(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn extract_ssh_hosts(config_contents: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    let mut seen = HashSet::new();

    for line in config_contents.lines() {
        let content = line.split('#').next().unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }

        let mut parts = content.split_whitespace();
        let Some(keyword) = parts.next() else {
            continue;
        };
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }

        for part in parts {
            if part.starts_with('!') || part.contains('*') || part.contains('?') {
                continue;
            }
            if seen.insert(part.to_string()) {
                hosts.push(part.to_string());
            }
        }
    }

    hosts
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{extract_ssh_hosts, run_command, starts_with_ignore_ascii_case, truncate_chars};

    #[test]
    fn test_extract_ssh_hosts_filters_globs_and_negations() {
        let config = r#"
Host prod
  HostName prod.example.com

Host staging dev-*
  HostName 10.0.0.5

Host !banned qa
  HostName qa.example.com
"#;
        let hosts = extract_ssh_hosts(config);
        assert_eq!(hosts, vec!["prod", "staging", "qa"]);
    }

    #[test]
    fn test_starts_with_ignore_ascii_case() {
        assert!(starts_with_ignore_ascii_case("FeatureBranch", "feature"));
        assert!(!starts_with_ignore_ascii_case("feat", "feature"));
    }

    #[test]
    fn test_truncate_chars_appends_marker_when_truncated() {
        let truncated = truncate_chars("abcdefghijklmnopqrstuvwxyz", 10);
        assert!(truncated.starts_with("abcdefghij"));
        assert!(truncated.contains("[truncated]"));
    }

    #[tokio::test]
    async fn test_run_command_ignores_stderr_on_non_zero_exit() {
        let result = run_command(
            std::path::Path::new("."),
            Duration::from_secs(1),
            "sh",
            &["-c", "echo fatal: not a repo 1>&2; exit 1"],
        )
        .await;
        assert!(result.is_none());
    }
}
