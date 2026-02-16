use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use moka::future::Cache;
use tokio::sync::RwLock;

use crate::completion_context::CompletionContext;
use crate::config::ContextConfig;
use crate::protocol::{SuggestRequest, SuggestionKind, SuggestionSource};
use crate::providers::{ProviderSuggestion, SuggestionProvider};

#[derive(Debug, Clone)]
pub struct ContextCommand {
    pub command: String,
    pub relevance: f64,
    #[allow(dead_code)]
    pub trigger_prefix: String,
}

#[derive(Debug, Clone)]
pub struct DirectoryContext {
    #[allow(dead_code)]
    pub project_root: PathBuf,
    #[allow(dead_code)]
    pub project_type: Option<String>,
    #[allow(dead_code)]
    pub git_branch: Option<String>,
    pub commands: Vec<ContextCommand>,
}

pub struct ContextProvider {
    config: ContextConfig,
    cache: Cache<PathBuf, DirectoryContext>,
    _watcher: Arc<RwLock<Option<notify::RecommendedWatcher>>>,
}

impl ContextProvider {
    pub fn new(config: ContextConfig) -> Self {
        let cache = Cache::builder()
            .max_capacity(100)
            .time_to_live(std::time::Duration::from_secs(300))
            .build();

        Self {
            config,
            cache,
            _watcher: Arc::new(RwLock::new(None)),
        }
    }

    pub fn project_type_for(root: &Path) -> Option<String> {
        if root.join("Cargo.toml").exists() {
            Some("rust".into())
        } else if root.join("package.json").exists() {
            Some("node".into())
        } else if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
            Some("python".into())
        } else if root.join("go.mod").exists() {
            Some("go".into())
        } else if root.join("Makefile").exists() {
            Some("make".into())
        } else {
            None
        }
    }

    async fn scan_directory(&self, cwd: &Path) -> DirectoryContext {
        let project_root = find_project_root(cwd, self.config.scan_depth);
        let root = project_root.as_deref().unwrap_or(cwd);

        let git_branch = read_git_branch(root);
        let project_type = Self::project_type_for(root);
        let mut commands = Vec::new();

        // Scan Makefile
        if let Some(mut targets) = scan_makefile(root) {
            commands.append(&mut targets);
        }

        // Scan package.json
        if let Some(mut scripts) = scan_package_json(root) {
            commands.append(&mut scripts);
        }

        // Scan Cargo.toml
        if let Some(mut cargo_cmds) = scan_cargo_toml(root) {
            commands.append(&mut cargo_cmds);
        }

        // Scan pyproject.toml
        if let Some(mut py_cmds) = scan_pyproject(root) {
            commands.append(&mut py_cmds);
        }

        // Scan docker-compose.yml
        if let Some(mut docker_cmds) = scan_docker_compose(root) {
            commands.append(&mut docker_cmds);
        }

        // Scan Justfile
        if let Some(mut just_cmds) = scan_justfile(root) {
            commands.append(&mut just_cmds);
        }

        DirectoryContext {
            project_root: root.to_path_buf(),
            project_type,
            git_branch,
            commands,
        }
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

    /// Word-level matching: match ctx.command against trigger_prefix,
    /// then filter ctx.partial against the subcommand/target portion.
    fn word_level_suggest(
        &self,
        dir_ctx: &DirectoryContext,
        command: &str,
        partial: &str,
        prefix: &str,
    ) -> Option<ProviderSuggestion> {
        let mut best: Option<ProviderSuggestion> = None;

        for cmd in &dir_ctx.commands {
            if let Some(suggestion) = self.word_level_match(cmd, command, partial, prefix) {
                if best.as_ref().is_none_or(|b| suggestion.score > b.score) {
                    best = Some(suggestion);
                }
            }
        }

        best
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

    #[allow(dead_code)]
    pub async fn invalidate(&self, path: &Path) {
        // Invalidate cache entries whose project root is a prefix of the changed path
        self.cache.invalidate(path).await;
    }

    #[allow(dead_code)]
    pub fn start_watcher(&self, _cwd: &Path) {
        // File watching via notify will be initialized when connections come in.
        // For now, cache TTL handles staleness.
    }
}

#[async_trait]
impl SuggestionProvider for ContextProvider {
    async fn suggest(
        &self,
        request: &SuggestRequest,
        completion_ctx: Option<&CompletionContext>,
    ) -> Option<ProviderSuggestion> {
        if request.buffer.is_empty() {
            return None;
        }

        let cwd = Path::new(&request.cwd);
        let dir_ctx = self.get_context(cwd).await;
        let buffer = &request.buffer;

        // Word-level matching when CompletionContext is available
        if let Some(cc) = completion_ctx {
            if !cc.prefix.is_empty() {
                let command = cc.command.as_deref().unwrap_or_default();
                return self.word_level_suggest(&dir_ctx, command, &cc.partial, &cc.prefix);
            }
        }

        // Fallback: full-buffer prefix matching
        let mut best: Option<(f64, &ContextCommand)> = None;

        for cmd in &dir_ctx.commands {
            if cmd.command.starts_with(buffer) && cmd.command.len() > buffer.len() {
                let score = cmd.relevance;
                if best.as_ref().is_none_or(|(s, _)| score > *s) {
                    best = Some((score, cmd));
                }
            }
        }

        best.map(|(score, cmd)| ProviderSuggestion {
            text: cmd.command.clone(),
            source: SuggestionSource::Context,
            score,
            description: None,
            kind: SuggestionKind::Command,
        })
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Context
    }

    fn is_available(&self) -> bool {
        self.config.enabled
    }

    async fn suggest_multi(
        &self,
        request: &SuggestRequest,
        max: usize,
        completion_ctx: Option<&CompletionContext>,
    ) -> Vec<ProviderSuggestion> {
        if request.buffer.is_empty() {
            return Vec::new();
        }

        let cwd = Path::new(&request.cwd);
        let dir_ctx = self.get_context(cwd).await;
        let buffer = &request.buffer;

        // Word-level matching when CompletionContext is available
        if let Some(cc) = completion_ctx {
            if !cc.prefix.is_empty() {
                let command = cc.command.as_deref().unwrap_or_default();
                let mut results =
                    self.word_level_suggest_multi(&dir_ctx, command, &cc.partial, &cc.prefix);
                results.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                results.truncate(max);
                return results;
            }
        }

        // Fallback: full-buffer prefix matching
        let mut results: Vec<ProviderSuggestion> = dir_ctx
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
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(max);
        results
    }
}

// --- Directory scanning helpers ---

fn find_project_root(cwd: &Path, max_depth: usize) -> Option<PathBuf> {
    // Unbounded walk to find git root (design doc: "walks to git root if inside a git repo")
    let mut current = cwd.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }

    // No git root found — walk up max_depth levels looking for project files
    let mut current = cwd.to_path_buf();
    for _ in 0..max_depth {
        let has_project_file = current.join("Makefile").exists()
            || current.join("package.json").exists()
            || current.join("Cargo.toml").exists()
            || current.join("pyproject.toml").exists()
            || current.join("docker-compose.yml").exists();
        if has_project_file {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }

    None
}

pub(crate) fn read_git_branch_for_path(root: &Path) -> Option<String> {
    // Walk up to find git root first
    let mut current = root.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return read_git_branch(&current);
        }
        if !current.pop() {
            return None;
        }
    }
}

fn read_git_branch(root: &Path) -> Option<String> {
    let head_path = root.join(".git").join("HEAD");
    let content = std::fs::read_to_string(head_path).ok()?;
    let trimmed = content.trim();
    if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else {
        // Detached HEAD — return short hash
        Some(trimmed[..8.min(trimmed.len())].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_git_branch_from_ref() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/auth\n").unwrap();

        let branch = read_git_branch_for_path(dir.path());
        assert_eq!(branch.as_deref(), Some("feature/auth"));
    }

    #[test]
    fn test_read_git_branch_detached_head() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(
            git_dir.join("HEAD"),
            "d34db33fd34db33fd34db33fd34db33fd34db33f\n",
        )
        .unwrap();

        let branch = read_git_branch_for_path(dir.path());
        assert_eq!(branch.as_deref(), Some("d34db33f"));
    }

    #[test]
    fn test_read_git_branch_walks_up_to_repo_root() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("src").join("providers");
        std::fs::create_dir_all(&nested).unwrap();

        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let branch = read_git_branch_for_path(&nested);
        assert_eq!(branch.as_deref(), Some("main"));
    }
}

fn scan_makefile(root: &Path) -> Option<Vec<ContextCommand>> {
    let path = root.join("Makefile");
    let content = std::fs::read_to_string(path).ok()?;
    let mut commands = Vec::new();

    for line in content.lines() {
        // Match target lines: "target:" or "target: deps"
        if let Some(target) = line.split(':').next() {
            let target = target.trim();
            if !target.is_empty()
                && !target.starts_with('#')
                && !target.starts_with('.')
                && !target.starts_with('\t')
                && !target.contains(' ')
                && !target.contains('$')
                && !target.contains('=')
            {
                commands.push(ContextCommand {
                    command: format!("make {target}"),
                    relevance: 0.7,
                    trigger_prefix: "make".into(),
                });
            }
        }
    }

    if commands.is_empty() {
        None
    } else {
        Some(commands)
    }
}

fn scan_package_json(root: &Path) -> Option<Vec<ContextCommand>> {
    let path = root.join("package.json");
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let scripts = json.get("scripts")?.as_object()?;
    let mut commands = Vec::new();

    // Detect package manager from lockfile
    let pm = if root.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if root.join("yarn.lock").exists() {
        "yarn"
    } else if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    };

    for (name, _) in scripts {
        let cmd = if pm == "npm" {
            format!("npm run {name}")
        } else {
            format!("{pm} {name}")
        };
        commands.push(ContextCommand {
            command: cmd,
            relevance: 0.8,
            trigger_prefix: pm.into(),
        });
    }

    if commands.is_empty() {
        None
    } else {
        Some(commands)
    }
}

fn scan_cargo_toml(root: &Path) -> Option<Vec<ContextCommand>> {
    let path = root.join("Cargo.toml");
    if !path.exists() {
        return None;
    }

    let mut commands = vec![
        ContextCommand {
            command: "cargo build".into(),
            relevance: 0.7,
            trigger_prefix: "cargo".into(),
        },
        ContextCommand {
            command: "cargo test".into(),
            relevance: 0.7,
            trigger_prefix: "cargo".into(),
        },
        ContextCommand {
            command: "cargo run".into(),
            relevance: 0.7,
            trigger_prefix: "cargo".into(),
        },
        ContextCommand {
            command: "cargo check".into(),
            relevance: 0.7,
            trigger_prefix: "cargo".into(),
        },
        ContextCommand {
            command: "cargo clippy".into(),
            relevance: 0.6,
            trigger_prefix: "cargo".into(),
        },
        ContextCommand {
            command: "cargo fmt".into(),
            relevance: 0.6,
            trigger_prefix: "cargo".into(),
        },
    ];

    // Check for workspace
    let content = std::fs::read_to_string(path).ok()?;
    if content.contains("[workspace]") {
        commands.push(ContextCommand {
            command: "cargo build --workspace".into(),
            relevance: 0.65,
            trigger_prefix: "cargo".into(),
        });
    }

    Some(commands)
}

fn scan_pyproject(root: &Path) -> Option<Vec<ContextCommand>> {
    let has_pyproject = root.join("pyproject.toml").exists();
    let has_setup = root.join("setup.py").exists();

    if !has_pyproject && !has_setup {
        return None;
    }

    let mut commands = Vec::new();

    if root.join(".venv").exists() || root.join("venv").exists() {
        commands.push(ContextCommand {
            command: "python -m pytest".into(),
            relevance: 0.7,
            trigger_prefix: "python".into(),
        });
    }

    if has_pyproject {
        // Check for poetry or pip
        if let Ok(content) = std::fs::read_to_string(root.join("pyproject.toml")) {
            if content.contains("[tool.poetry]") {
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
            if content.contains("[tool.ruff]") {
                commands.push(ContextCommand {
                    command: "ruff check .".into(),
                    relevance: 0.6,
                    trigger_prefix: "ruff".into(),
                });
            }
        }
    }

    commands.push(ContextCommand {
        command: "pip install -e .".into(),
        relevance: 0.5,
        trigger_prefix: "pip".into(),
    });

    if commands.is_empty() {
        None
    } else {
        Some(commands)
    }
}

fn scan_docker_compose(root: &Path) -> Option<Vec<ContextCommand>> {
    let path = if root.join("docker-compose.yml").exists() {
        root.join("docker-compose.yml")
    } else if root.join("docker-compose.yaml").exists() {
        root.join("docker-compose.yaml")
    } else if root.join("compose.yml").exists() {
        root.join("compose.yml")
    } else if root.join("compose.yaml").exists() {
        root.join("compose.yaml")
    } else {
        return None;
    };

    let content = std::fs::read_to_string(path).ok()?;
    let mut commands = vec![
        ContextCommand {
            command: "docker compose up".into(),
            relevance: 0.7,
            trigger_prefix: "docker".into(),
        },
        ContextCommand {
            command: "docker compose up -d".into(),
            relevance: 0.7,
            trigger_prefix: "docker".into(),
        },
        ContextCommand {
            command: "docker compose down".into(),
            relevance: 0.7,
            trigger_prefix: "docker".into(),
        },
        ContextCommand {
            command: "docker compose logs".into(),
            relevance: 0.6,
            trigger_prefix: "docker".into(),
        },
    ];

    // Extract service names (simple YAML parsing — look for top-level keys under services:)
    let mut in_services = false;
    for line in content.lines() {
        if line.trim() == "services:" {
            in_services = true;
            continue;
        }
        if in_services {
            // A service is a line with exactly 2 spaces of indentation followed by name:
            if line.starts_with("  ") && !line.starts_with("    ") {
                if let Some(name) = line.trim().strip_suffix(':') {
                    let name = name.trim();
                    if !name.is_empty() && !name.starts_with('#') {
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
            }
            // Stop parsing services when we hit another top-level key
            if !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#') {
                in_services = false;
            }
        }
    }

    Some(commands)
}

fn scan_justfile(root: &Path) -> Option<Vec<ContextCommand>> {
    let path = if root.join("Justfile").exists() {
        root.join("Justfile")
    } else if root.join("justfile").exists() {
        root.join("justfile")
    } else {
        return None;
    };

    let content = std::fs::read_to_string(path).ok()?;
    let mut commands = Vec::new();

    for line in content.lines() {
        // Recipe lines: "recipe-name:" or "recipe-name arg:" etc.
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with(' ')
            && !trimmed.starts_with('\t')
            && !trimmed.starts_with("set ")
            && !trimmed.starts_with("export ")
            && !trimmed.starts_with("alias ")
        {
            if let Some(name) = trimmed.split(':').next() {
                let name = name.split_whitespace().next().unwrap_or(name).trim();
                if !name.is_empty() && !name.contains('=') {
                    commands.push(ContextCommand {
                        command: format!("just {name}"),
                        relevance: 0.7,
                        trigger_prefix: "just".into(),
                    });
                }
            }
        }
    }

    if commands.is_empty() {
        None
    } else {
        Some(commands)
    }
}
