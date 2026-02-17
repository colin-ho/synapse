use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::completion_context::tokenize;
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};
use crate::spec::{find_option, ArgSpec, ArgTemplate, SpecSource};
use crate::spec_store::SpecStore;

#[derive(Default)]
pub struct SpecProvider;

impl SpecProvider {
    pub fn new() -> Self {
        Self
    }

    /// Generate completions based on spec tree-walk.
    /// Returns multiple suggestions sorted by relevance.
    async fn complete(
        &self,
        buffer: &str,
        cwd: &Path,
        store: &Arc<SpecStore>,
    ) -> Vec<ProviderSuggestion> {
        if buffer.is_empty() {
            return Vec::new();
        }

        let tokens = tokenize(buffer);
        if tokens.is_empty() {
            return Vec::new();
        }

        let trailing_space = buffer.ends_with(' ');
        let command_name = &tokens[0];

        // Look up the root spec
        let spec = match store.lookup(command_name, cwd).await {
            Some(s) => s,
            None => {
                // If partial first token, try to complete the command name itself
                if tokens.len() == 1 && !trailing_space {
                    return self.complete_command_name(command_name, cwd, store).await;
                }
                // Trigger background discovery for this unknown command
                store.trigger_discovery(command_name, Some(cwd)).await;
                return Vec::new();
            }
        };

        // Keep discovered specs fresh in the background when they become stale.
        if spec.source == SpecSource::Discovered {
            store.trigger_discovery(command_name, Some(cwd)).await;
        }

        // If we only have the command name and no trailing space, the command is being typed
        if tokens.len() == 1 && !trailing_space {
            // Check if it's an exact match — if so, don't suggest the command itself
            if command_name != &spec.name && !spec.aliases.iter().any(|a| a == command_name) {
                return self.complete_command_name(command_name, cwd, store).await;
            }
            return Vec::new();
        }

        // Walk the spec tree consuming complete tokens
        let mut current_subcommands = &spec.subcommands;
        let mut current_options = &spec.options;
        let mut current_args = &spec.args;
        let remaining_tokens = &tokens[1..];

        let mut _consumed = 0;
        let mut skip_next = false;
        let mut last_option_name: Option<String> = None;

        for (i, token) in remaining_tokens.iter().enumerate() {
            if skip_next {
                let is_last_incomplete = i == remaining_tokens.len() - 1 && !trailing_space;
                if is_last_incomplete {
                    // Keep option-value mode while the current value token is incomplete.
                    break;
                }
                skip_next = false;
                last_option_name = None;
                _consumed = i + 1;
                continue;
            }

            // Check if this token is a complete subcommand match
            let sub_match = current_subcommands
                .iter()
                .find(|s| s.name == *token || s.aliases.iter().any(|a| a == token));

            if let Some(sub) = sub_match {
                // If this is the last token and there's no trailing space, it's being typed
                if i == remaining_tokens.len() - 1 && !trailing_space {
                    break;
                }
                current_subcommands = &sub.subcommands;
                current_options = &sub.options;
                current_args = &sub.args;
                _consumed = i + 1;
            } else if token.starts_with('-') {
                // Check if this option takes an arg — if so, skip the next token
                if let Some(opt) = find_option(current_options, token) {
                    if opt.takes_arg {
                        skip_next = true;
                        last_option_name = Some(token.clone());
                    }
                }
                _consumed = i + 1;
            } else {
                // Positional arg or partial token
                _consumed = i;
                break;
            }
        }

        // Determine what to complete.
        // partial is always the last token being typed (when no trailing space).
        // prefix is all complete tokens (everything before the partial).
        let partial = if !trailing_space && !remaining_tokens.is_empty() {
            remaining_tokens.last().map(|s| s.as_str()).unwrap_or("")
        } else {
            ""
        };

        let prefix = if !trailing_space && tokens.len() > 1 {
            format!("{} ", tokens[..tokens.len() - 1].join(" "))
        } else if trailing_space {
            format!("{} ", tokens.join(" "))
        } else {
            // Single token being completed — no prefix
            String::new()
        };

        let mut suggestions = Vec::new();

        // Complete subcommands
        for sub in current_subcommands {
            if sub.name.starts_with(partial) {
                let text = format!("{}{}", prefix, sub.name);
                suggestions.push(spec_suggestion(
                    text,
                    prefix_confidence(0.7, 0.3, partial, &sub.name),
                    sub.description.clone(),
                    SuggestionKind::Subcommand,
                ));
            }
            // Also check aliases
            for alias in &sub.aliases {
                if alias.starts_with(partial) && alias != &sub.name {
                    let text = format!("{}{}", prefix, alias);
                    suggestions.push(spec_suggestion(
                        text,
                        0.65,
                        sub.description.clone(),
                        SuggestionKind::Subcommand,
                    ));
                }
            }
        }

        // Complete options (when partial starts with '-' or we're completing after a space)
        if partial.starts_with('-') || (partial.is_empty() && suggestions.is_empty()) {
            for opt in current_options {
                if let Some(long) = &opt.long {
                    if long.starts_with(partial) {
                        let text = format!("{}{}", prefix, long);
                        suggestions.push(spec_suggestion(
                            text,
                            prefix_confidence(0.5, 0.3, partial, long),
                            opt.description.clone(),
                            SuggestionKind::Option,
                        ));
                    }
                }
                if let Some(short) = &opt.short {
                    if short.starts_with(partial) && partial.len() <= 2 {
                        let text = format!("{}{}", prefix, short);
                        suggestions.push(spec_suggestion(
                            text,
                            0.55,
                            opt.description.clone(),
                            SuggestionKind::Option,
                        ));
                    }
                }
            }
        }

        // Complete option argument values (when we're awaiting a value for an option)
        if skip_next {
            if let Some(ref opt_name) = last_option_name {
                if let Some(opt) = find_option(current_options, opt_name) {
                    if let Some(ref gen) = opt.arg_generator {
                        let gen_results = store.run_generator(gen, cwd, spec.source).await;
                        for item in gen_results {
                            if item.starts_with(partial) {
                                suggestions.push(spec_suggestion(
                                    format!("{}{}", prefix, item),
                                    prefix_confidence(0.65, 0.25, partial, &item),
                                    opt.description.clone(),
                                    SuggestionKind::Argument,
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Complete arguments (generators, templates, static suggestions)
        if !partial.starts_with('-') && !skip_next {
            for arg in current_args {
                let mut arg_suggestions = self
                    .resolve_arg_completions(arg, partial, cwd, spec.source, store)
                    .await;
                for s in &mut arg_suggestions {
                    s.text = format!("{}{}", prefix, s.text);
                }
                suggestions.extend(arg_suggestions);
            }
        }

        suggestions
    }

    async fn complete_command_name(
        &self,
        partial: &str,
        cwd: &Path,
        store: &SpecStore,
    ) -> Vec<ProviderSuggestion> {
        let all_names = store.all_command_names(cwd).await;
        all_names
            .into_iter()
            .filter(|name| name.starts_with(partial) && name != partial)
            .map(|name| {
                let score = prefix_confidence(0.6, 0.3, partial, &name);
                spec_suggestion(name, score, None, SuggestionKind::Command)
            })
            .collect()
    }

    async fn resolve_arg_completions(
        &self,
        arg: &ArgSpec,
        partial: &str,
        cwd: &Path,
        source: SpecSource,
        store: &SpecStore,
    ) -> Vec<ProviderSuggestion> {
        let mut results = Vec::new();

        // Static suggestions
        for suggestion in &arg.suggestions {
            if suggestion.starts_with(partial) {
                results.push(spec_suggestion(
                    suggestion.clone(),
                    0.7,
                    arg.description.clone(),
                    SuggestionKind::Argument,
                ));
            }
        }

        // Generator
        if let Some(generator) = &arg.generator {
            let gen_results = store.run_generator(generator, cwd, source).await;
            for item in gen_results {
                if item.starts_with(partial) {
                    results.push(spec_suggestion(
                        item.clone(),
                        prefix_confidence(0.65, 0.25, partial, &item),
                        arg.description.clone(),
                        SuggestionKind::Argument,
                    ));
                }
            }
        }

        // Template
        if let Some(template) = &arg.template {
            match template {
                ArgTemplate::FilePaths | ArgTemplate::Directories => {
                    // File/dir completion is better handled by the shell itself
                    // Just add the template kind info
                    if results.is_empty() && partial.is_empty() {
                        results.push(spec_suggestion(
                            String::new(),
                            0.3,
                            Some(match template {
                                ArgTemplate::FilePaths => "file path".into(),
                                ArgTemplate::Directories => "directory".into(),
                                _ => unreachable!(),
                            }),
                            SuggestionKind::File,
                        ));
                    }
                }
                ArgTemplate::EnvVars | ArgTemplate::History => {
                    // These are handled by other providers
                }
            }
        }

        results
    }
}

#[async_trait]
impl SuggestionProvider for SpecProvider {
    async fn suggest(
        &self,
        request: &ProviderRequest,
        max: NonZeroUsize,
    ) -> Vec<ProviderSuggestion> {
        let cwd = Path::new(&request.cwd);
        let store = request.spec_store.clone();
        let mut completions = self.complete(&request.buffer, cwd, &store).await;

        // Filter out empty text entries and entries matching the buffer exactly
        completions.retain(|s| !s.text.is_empty() && s.text != request.buffer);

        // Return the highest scoring completion
        completions.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        completions.truncate(max.get());
        completions
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Spec
    }

    fn is_available(&self) -> bool {
        true
    }
}

fn prefix_confidence(base: f64, bonus: f64, partial: &str, full: &str) -> f64 {
    if partial.is_empty() {
        base
    } else {
        base + bonus * (partial.len() as f64 / full.len().max(1) as f64)
    }
}

fn spec_suggestion(
    text: String,
    score: f64,
    description: Option<String>,
    kind: SuggestionKind,
) -> ProviderSuggestion {
    ProviderSuggestion {
        text,
        source: SuggestionSource::Spec,
        score,
        description,
        kind,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::config::SpecConfig;
    use crate::protocol::{SuggestionKind, SuggestionSource};
    use crate::providers::SuggestionProvider;
    use crate::spec_store::SpecStore;
    use crate::test_helpers::{limit, make_provider_request, make_provider_request_with_store};

    use super::SpecProvider;

    fn make_spec_provider() -> SpecProvider {
        SpecProvider::new()
    }

    // --- Builtin spec loading ---

    #[tokio::test]
    async fn test_builtin_specs_loaded() {
        let config = SpecConfig::default();
        let store = SpecStore::new(config, None);
        let dir = tempfile::tempdir().unwrap();
        let names = store.all_command_names(dir.path()).await;
        assert!(names.contains(&"git".to_string()));
        assert!(names.contains(&"cargo".to_string()));
        assert!(names.contains(&"npm".to_string()));
        assert!(names.contains(&"docker".to_string()));
    }

    #[tokio::test]
    async fn test_builtin_spec_lookup() {
        let config = SpecConfig::default();
        let store = SpecStore::new(config, None);
        let dir = tempfile::tempdir().unwrap();

        let git = store.lookup("git", dir.path()).await;
        assert!(git.is_some());
        let git = git.unwrap();
        assert_eq!(git.name, "git");
        assert!(!git.subcommands.is_empty());
    }

    // --- Git subcommand completions ---

    #[tokio::test]
    async fn test_git_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("git co", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        let suggestion = &result[0];
        // Should suggest "git commit" or "git config" (starts with "co")
        assert!(
            suggestion.text.starts_with("git co"),
            "Expected suggestion starting with 'git co', got: {}",
            suggestion.text
        );
        assert_eq!(suggestion.source, SuggestionSource::Spec);
        assert_eq!(suggestion.kind, SuggestionKind::Subcommand);
    }

    #[tokio::test]
    async fn test_git_multi_suggestions() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("git ", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        assert!(
            results.len() > 1,
            "Expected multiple suggestions for 'git '"
        );

        // All should be from Spec source
        for r in &results {
            assert_eq!(r.source, SuggestionSource::Spec);
        }
    }

    #[tokio::test]
    async fn test_git_checkout_alias() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        // "git ch" should match both "checkout" and "cherry-pick" etc.
        let req = make_provider_request("git ch", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|r| r.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("checkout") || t.contains("cherry-pick")),
            "Expected checkout or cherry-pick in suggestions, got: {:?}",
            texts
        );
    }

    // --- Cargo subcommand completions ---

    #[tokio::test]
    async fn test_cargo_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("cargo b", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "cargo build");
    }

    #[tokio::test]
    async fn test_cargo_test_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("cargo t", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "cargo test");
    }

    // --- Option completions ---

    #[tokio::test]
    async fn test_git_commit_option_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("git commit --m", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|r| r.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.contains("--message")),
            "Expected --message in suggestions, got: {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_option_arg_generator_while_typing_value() {
        let dir = tempfile::tempdir().unwrap();
        let spec_dir = dir.path().join(".synapse").join("specs");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(
            spec_dir.join("tool.toml"),
            r#"
name = "tool"

[[options]]
long = "--profile"
takes_arg = true
description = "Profile name"

[options.arg_generator]
command = "printf '%s\n' alpha beta"
"#,
        )
        .unwrap();

        let config = SpecConfig {
            auto_generate: false,
            trust_project_generators: true,
            ..SpecConfig::default()
        };
        let store = Arc::new(SpecStore::new(config, None));
        let provider = SpecProvider::new();

        let req = make_provider_request_with_store(
            "tool --profile a",
            dir.path().to_str().unwrap(),
            store,
        )
        .await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|r| r.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| *t == "tool --profile alpha"),
            "Expected option arg generator suggestion, got: {:?}",
            texts
        );
    }

    // --- Empty buffer ---

    #[tokio::test]
    async fn test_empty_buffer_returns_none() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(result.is_empty());
    }

    // --- Unknown command ---

    #[tokio::test]
    async fn test_unknown_command_returns_empty() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("nonexistent_cmd ", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(result.is_empty());
    }

    // --- Project spec auto-generation ---

    #[tokio::test]
    async fn test_autogen_cargo_spec() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let provider = make_spec_provider();

        let req = make_provider_request("cargo b", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "cargo build");
    }

    #[tokio::test]
    async fn test_autogen_makefile_spec() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "build:\n\tgo build\n\ntest:\n\tgo test\n\ndeploy:\n\tgo deploy\n",
        )
        .unwrap();

        let provider = make_spec_provider();

        let req = make_provider_request("make d", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "make deploy");
    }

    #[tokio::test]
    async fn test_autogen_spec_from_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        // Put Cargo.toml at the root and .git to mark project root
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();

        // cwd is a nested subdirectory
        let nested = dir.path().join("src").join("providers");
        std::fs::create_dir_all(&nested).unwrap();

        let provider = make_spec_provider();

        let req = make_provider_request("cargo b", nested.to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(
            !result.is_empty(),
            "Spec autogen should find Cargo.toml from subdirectory via project root walking"
        );
        assert_eq!(result[0].text, "cargo build");
    }

    // --- suggest max ---

    #[tokio::test]
    async fn test_suggest_truncates_to_max() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("git ", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(3)).await;
        assert!(
            results.len() <= 3,
            "Expected at most 3 results, got {}",
            results.len()
        );
    }

    // --- Autogen: npm scripts ---

    #[tokio::test]
    async fn test_autogen_npm_scripts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"dev": "vite", "build": "tsc && vite build", "test": "vitest"}}"#,
        )
        .unwrap();

        let provider = make_spec_provider();

        let req = make_provider_request("npm run d", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "npm run dev");
    }

    #[tokio::test]
    async fn test_autogen_yarn_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"start": "node index.js"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("yarn.lock"), "").unwrap();

        let provider = make_spec_provider();

        let req = make_provider_request("yarn s", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "yarn start");
    }

    // --- Autogen: docker compose ---

    #[tokio::test]
    async fn test_autogen_docker_compose() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            "services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n",
        )
        .unwrap();

        let provider = make_spec_provider();

        let req = make_provider_request("docker compose u", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(5)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "docker compose up");
    }

    // --- Autogen: justfile ---

    #[tokio::test]
    async fn test_autogen_justfile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("justfile"),
            "build:\n  cargo build\n\ntest:\n  cargo test\n",
        )
        .unwrap();

        let provider = make_spec_provider();

        let req = make_provider_request("just b", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "just build");
    }

    // --- Autogen: Python tools ---

    #[tokio::test]
    async fn test_autogen_poetry_spec() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"myproject\"\n",
        )
        .unwrap();

        let provider = make_spec_provider();

        let req = make_provider_request("poetry i", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "poetry install");
    }

    #[tokio::test]
    async fn test_autogen_pytest_spec() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"test\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join(".venv")).unwrap();

        let provider = make_spec_provider();

        let req = make_provider_request("pytest -", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(5)).await;
        assert!(!result.is_empty());
        let texts: Vec<&str> = result.iter().map(|r| r.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.contains("--verbose")),
            "Expected --verbose in suggestions, got: {:?}",
            texts
        );
    }

    // --- All builtin specs parse and produce completions ---

    #[tokio::test]
    async fn test_all_builtin_specs_are_registered() {
        let config = SpecConfig::default();
        let store = SpecStore::new(config, None);
        let dir = tempfile::tempdir().unwrap();
        let names = store.all_command_names(dir.path()).await;

        // Every full builtin spec should be present
        for cmd in [
            "git",
            "cargo",
            "npm",
            "docker",
            "ls",
            "grep",
            "find",
            "curl",
            "ssh",
            "python",
            "pip",
            "brew",
            "tar",
            "make",
            "sed",
            "wget",
            "rsync",
            "kubectl",
            "tmux",
            "jq",
            "awk",
            "scp",
            "go",
            "yarn",
            "pnpm",
            "cp",
            "mv",
            "rm",
            "chmod",
            "systemctl",
            "diff",
            "kill",
            "du",
            "df",
            "helm",
            "terraform",
            "gh",
            "uv",
        ] {
            assert!(
                names.contains(&cmd.to_string()),
                "Missing builtin spec for '{cmd}'"
            );
        }
    }

    #[tokio::test]
    async fn test_all_builtin_specs_have_subcommands_or_options() {
        let config = SpecConfig::default();
        let store = SpecStore::new(config, None);
        let dir = tempfile::tempdir().unwrap();

        for cmd in [
            "git",
            "cargo",
            "npm",
            "docker",
            "pip",
            "brew",
            "kubectl",
            "tmux",
            "go",
            "yarn",
            "pnpm",
            "systemctl",
            "helm",
            "terraform",
            "gh",
            "uv",
        ] {
            let spec = store.lookup(cmd, dir.path()).await;
            assert!(spec.is_some(), "Spec for '{cmd}' not found");
            let spec = spec.unwrap();
            assert!(
                !spec.subcommands.is_empty(),
                "Spec for '{cmd}' should have subcommands"
            );
        }

        for cmd in [
            "ls", "grep", "find", "curl", "ssh", "python", "tar", "sed", "wget", "rsync", "jq",
            "awk", "scp", "cp", "mv", "rm", "chmod", "diff", "kill", "du", "df",
        ] {
            let spec = store.lookup(cmd, dir.path()).await;
            assert!(spec.is_some(), "Spec for '{cmd}' not found");
            let spec = spec.unwrap();
            assert!(
                !spec.options.is_empty(),
                "Spec for '{cmd}' should have options"
            );
        }
    }

    #[tokio::test]
    async fn test_brew_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("brew ins", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "brew install");
    }

    #[tokio::test]
    async fn test_kubectl_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("kubectl g", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "kubectl get");
    }

    #[tokio::test]
    async fn test_docker_compose_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("docker compose u", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "docker compose up");
    }

    #[tokio::test]
    async fn test_gh_pr_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("gh pr c", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(5)).await;
        assert!(!result.is_empty());
        let texts: Vec<&str> = result.iter().map(|r| r.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| *t == "gh pr create"),
            "Expected 'gh pr create' in {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_uv_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("uv ", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        assert!(results.len() > 1, "Expected multiple suggestions for 'uv '");
        let texts: Vec<&str> = results.iter().map(|r| r.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| *t == "uv pip"),
            "Expected 'uv pip' in {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_go_mod_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("go mod t", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "go mod tidy");
    }

    #[tokio::test]
    async fn test_terraform_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("terraform p", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "terraform plan");
    }

    #[tokio::test]
    async fn test_helm_subcommand_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("helm ins", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "helm install");
    }

    #[tokio::test]
    async fn test_option_completion_for_new_specs() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        // jq -r should suggest --raw-output
        let req = make_provider_request("jq -", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(10)).await;
        assert!(!result.is_empty());
        let texts: Vec<&str> = result.iter().map(|r| r.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("-r") || t.contains("--raw-output")),
            "Expected -r/--raw-output in jq suggestions, got: {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_cp_option_completion() {
        let provider = make_spec_provider();
        let dir = tempfile::tempdir().unwrap();

        let req = make_provider_request("cp --r", dir.path().to_str().unwrap()).await;
        let result = provider.suggest(&req, limit(1)).await;
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "cp --recursive");
    }

    #[tokio::test]
    async fn test_alias_resolution() {
        let config = SpecConfig::default();
        let store = SpecStore::new(config, None);
        let dir = tempfile::tempdir().unwrap();

        // pip3 should resolve to pip spec via alias
        let spec = store.lookup("pip3", dir.path()).await;
        assert!(spec.is_some(), "pip3 alias should resolve");
        assert_eq!(spec.unwrap().name, "pip");

        // python3 should resolve to python spec
        let spec = store.lookup("python3", dir.path()).await;
        assert!(spec.is_some(), "python3 alias should resolve");
        assert_eq!(spec.unwrap().name, "python");

        // gawk should resolve to awk spec
        let spec = store.lookup("gawk", dir.path()).await;
        assert!(spec.is_some(), "gawk alias should resolve");
        assert_eq!(spec.unwrap().name, "awk");
    }
}
