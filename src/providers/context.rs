use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use moka::future::Cache;

use crate::config::ContextConfig;
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};

#[derive(Debug, Clone)]
pub struct ContextCommand {
    pub command: String,
    pub relevance: f64,
    pub trigger_prefix: String,
}

#[derive(Debug, Clone)]
struct DirectoryContext {
    commands: Vec<ContextCommand>,
}

pub struct ContextProvider {
    config: ContextConfig,
    cache: Cache<PathBuf, DirectoryContext>,
}

impl ContextProvider {
    pub fn new(config: ContextConfig) -> Self {
        let cache = Cache::builder()
            .max_capacity(100)
            .time_to_live(std::time::Duration::from_secs(300))
            .build();

        Self { config, cache }
    }

    async fn scan_directory(&self, cwd: &Path) -> DirectoryContext {
        let scan_depth = self.config.scan_depth;
        let cwd = cwd.to_path_buf();

        let commands = tokio::task::spawn_blocking(move || {
            let project_root = crate::project::find_project_root(&cwd, scan_depth);
            let root = project_root.as_deref().unwrap_or(&cwd);
            build_context_commands(root)
        })
        .await
        .unwrap_or_default();

        DirectoryContext { commands }
    }

    async fn get_context(&self, cwd: &Path) -> DirectoryContext {
        let key = cwd.to_path_buf();
        if let Some(cached) = self.cache.get(&key).await {
            return cached;
        }

        let ctx = self.scan_directory(cwd).await;
        self.cache.insert(key, ctx.clone()).await;
        ctx
    }

    fn word_level_suggest_multi(
        &self,
        dir_ctx: &DirectoryContext,
        command: &str,
        partial: &str,
        prefix: &str,
    ) -> Vec<ProviderSuggestion> {
        dir_ctx
            .commands
            .iter()
            .filter_map(|cmd| self.word_level_match(cmd, command, partial, prefix))
            .collect()
    }

    /// Match a context command at the word level.
    /// If the command's trigger_prefix matches `command`, extract the target portion
    /// and filter by `partial`.
    fn word_level_match(
        &self,
        ctx_cmd: &ContextCommand,
        command: &str,
        partial: &str,
        prefix: &str,
    ) -> Option<ProviderSuggestion> {
        // Primary path: when CompletionContext provides a concrete typed prefix,
        // strip from the full stored command to avoid duplicated segments.
        let target = if !prefix.is_empty() {
            ctx_cmd.command.strip_prefix(prefix)?
        } else {
            // Fallback path for callers without a typed prefix.
            if ctx_cmd.trigger_prefix != command {
                return None;
            }
            ctx_cmd
                .command
                .strip_prefix(&ctx_cmd.trigger_prefix)
                .and_then(|s| s.strip_prefix(' '))
                .unwrap_or("")
        };

        // The typed partial corresponds to the beginning of the remaining target.
        if !partial.is_empty() && !target.starts_with(partial) {
            return None;
        }

        if target.is_empty() {
            return None;
        }

        let text = if !prefix.is_empty() {
            format!("{prefix}{target}")
        } else {
            ctx_cmd.command.clone()
        };

        // Don't suggest if it would be identical to what's already typed
        if text.trim() == prefix.trim() {
            return None;
        }

        let specificity = if !partial.is_empty() {
            partial.len() as f64 / target.len().max(1) as f64
        } else {
            0.0
        };

        Some(ProviderSuggestion {
            text,
            source: SuggestionSource::Context,
            score: ctx_cmd.relevance * (0.7 + 0.3 * specificity),
            description: None,
            kind: SuggestionKind::Command,
        })
    }
}

#[async_trait]
impl SuggestionProvider for ContextProvider {
    async fn suggest(&self, request: &ProviderRequest, max: usize) -> Vec<ProviderSuggestion> {
        if max == 0 || request.buffer.is_empty() {
            return Vec::new();
        }

        let cwd = Path::new(&request.cwd);
        let dir_ctx = self.get_context(cwd).await;

        let mut results: Vec<ProviderSuggestion> = if !request.prefix.is_empty() {
            let command = request.command.as_deref().unwrap_or_default();
            self.word_level_suggest_multi(&dir_ctx, command, &request.partial, &request.prefix)
        } else {
            // Fallback: full-buffer prefix matching
            let buffer = &request.buffer;
            dir_ctx
                .commands
                .iter()
                .filter(|cmd| cmd.command.starts_with(buffer) && cmd.command.len() > buffer.len())
                .map(|cmd| ProviderSuggestion {
                    text: cmd.command.clone(),
                    source: SuggestionSource::Context,
                    score: cmd.relevance,
                    description: None,
                    kind: SuggestionKind::Command,
                })
                .collect()
        };

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(max);
        results
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Context
    }

    fn is_available(&self) -> bool {
        self.config.enabled
    }
}

// --- Build context commands directly from parsers ---

fn build_context_commands(root: &Path) -> Vec<ContextCommand> {
    let mut commands = Vec::new();

    for name in crate::project::parse_makefile_targets(root) {
        commands.push(ContextCommand {
            command: format!("make {name}"),
            relevance: 0.7,
            trigger_prefix: "make".into(),
        });
    }

    if let Some(scripts) = crate::project::parse_npm_scripts(root) {
        let manager = crate::project::detect_package_manager(root);
        for (name, _) in scripts {
            let cmd = if manager == "npm" {
                format!("npm run {name}")
            } else {
                format!("{manager} {name}")
            };
            commands.push(ContextCommand {
                command: cmd,
                relevance: 0.8,
                trigger_prefix: manager.into(),
            });
        }
    }

    if let Some((is_workspace, _)) = crate::project::parse_cargo_info(root) {
        for name in ["build", "test", "run", "check", "clippy", "fmt"] {
            let relevance = if matches!(name, "clippy" | "fmt") {
                0.6
            } else {
                0.7
            };
            commands.push(ContextCommand {
                command: format!("cargo {name}"),
                relevance,
                trigger_prefix: "cargo".into(),
            });
        }
        if is_workspace {
            commands.push(ContextCommand {
                command: "cargo build --workspace".into(),
                relevance: 0.65,
                trigger_prefix: "cargo".into(),
            });
        }
    }

    if let Some(services) = crate::project::parse_docker_services(root) {
        commands.push(ContextCommand {
            command: "docker compose up".into(),
            relevance: 0.7,
            trigger_prefix: "docker".into(),
        });
        commands.push(ContextCommand {
            command: "docker compose up -d".into(),
            relevance: 0.7,
            trigger_prefix: "docker".into(),
        });
        commands.push(ContextCommand {
            command: "docker compose down".into(),
            relevance: 0.7,
            trigger_prefix: "docker".into(),
        });
        commands.push(ContextCommand {
            command: "docker compose logs".into(),
            relevance: 0.6,
            trigger_prefix: "docker".into(),
        });
        for name in &services {
            commands.push(ContextCommand {
                command: format!("docker compose up {name}"),
                relevance: 0.65,
                trigger_prefix: "docker".into(),
            });
            commands.push(ContextCommand {
                command: format!("docker compose logs {name}"),
                relevance: 0.6,
                trigger_prefix: "docker".into(),
            });
        }
    }

    for name in crate::project::parse_justfile_recipes(root) {
        commands.push(ContextCommand {
            command: format!("just {name}"),
            relevance: 0.7,
            trigger_prefix: "just".into(),
        });
    }

    if let Some(py) = crate::project::parse_python_info(root) {
        if py.has_venv {
            commands.push(ContextCommand {
                command: "python -m pytest".into(),
                relevance: 0.7,
                trigger_prefix: "python".into(),
            });
        }
        if py.has_poetry {
            commands.push(ContextCommand {
                command: "poetry install".into(),
                relevance: 0.7,
                trigger_prefix: "poetry".into(),
            });
            commands.push(ContextCommand {
                command: "poetry run".into(),
                relevance: 0.7,
                trigger_prefix: "poetry".into(),
            });
        }
        if py.has_ruff {
            commands.push(ContextCommand {
                command: "ruff check .".into(),
                relevance: 0.6,
                trigger_prefix: "ruff".into(),
            });
        }
        commands.push(ContextCommand {
            command: "pip install -e .".into(),
            relevance: 0.5,
            trigger_prefix: "pip".into(),
        });
    }

    commands
}
