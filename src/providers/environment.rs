use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::completion_context::{CompletionContext, Position};
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};

pub struct EnvironmentProvider {
    executables: Arc<RwLock<Vec<String>>>,
    cache_valid_until: Arc<RwLock<Option<Instant>>>,
}

impl Default for EnvironmentProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvironmentProvider {
    pub fn new() -> Self {
        Self {
            executables: Arc::new(RwLock::new(Vec::new())),
            cache_valid_until: Arc::new(RwLock::new(None)),
        }
    }

    /// Scan all PATH directories and collect executable names.
    pub async fn scan_path(&self) {
        let path_var = std::env::var("PATH").unwrap_or_default();
        let mut seen = HashSet::new();
        let mut executables = Vec::new();

        for dir in path_var.split(':') {
            let dir_path = Path::new(dir);
            if !dir_path.is_dir() {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(dir_path) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    // Check executable bit on Unix
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(meta) = entry.metadata() {
                            if meta.permissions().mode() & 0o111 == 0 {
                                continue;
                            }
                        }
                    }
                    if seen.insert(name.clone()) {
                        executables.push(name);
                    }
                }
            }
        }

        executables.sort();
        let count = executables.len();
        *self.executables.write().await = executables;
        *self.cache_valid_until.write().await =
            Some(Instant::now() + std::time::Duration::from_secs(60));
        tracing::info!("Scanned PATH: {count} executables");
    }

    /// Ensure cache is fresh.
    async fn ensure_cache(&self) {
        let valid = self.cache_valid_until.read().await;
        if valid.is_some_and(|t| Instant::now() < t) {
            return;
        }
        drop(valid);
        self.scan_path().await;
    }

    /// Check if this provider should activate for the given context.
    fn should_activate(ctx: &CompletionContext) -> bool {
        matches!(ctx.position, Position::CommandName | Position::PipeTarget)
    }

    /// Complete command names from PATH.
    async fn complete(&self, partial: &str, max: usize) -> Vec<ProviderSuggestion> {
        self.ensure_cache().await;
        let execs = self.executables.read().await;

        // Binary search for prefix start
        let start = execs.partition_point(|e| e.as_str() < partial);

        let mut results = Vec::new();
        for name in &execs[start..] {
            if !name.starts_with(partial) {
                break;
            }
            let specificity = partial.len() as f64 / name.len() as f64;
            let score = (0.4 + 0.2 * specificity).clamp(0.0, 1.0);

            results.push(ProviderSuggestion {
                text: name.clone(),
                source: SuggestionSource::Environment,
                score,
                description: None,
                kind: SuggestionKind::Command,
            });

            if results.len() >= max {
                break;
            }
        }

        results
    }

    /// Check if a command name exists in PATH.
    #[allow(dead_code)]
    pub async fn command_exists(&self, name: &str) -> bool {
        self.ensure_cache().await;
        let execs = self.executables.read().await;
        execs.binary_search_by(|e| e.as_str().cmp(name)).is_ok()
    }
}

#[async_trait]
impl SuggestionProvider for EnvironmentProvider {
    async fn suggest(&self, request: &ProviderRequest, max: usize) -> Vec<ProviderSuggestion> {
        if max == 0 {
            return Vec::new();
        }

        if !Self::should_activate(request) {
            return Vec::new();
        }

        self.complete(&request.partial, max).await
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Environment
    }

    fn is_available(&self) -> bool {
        true
    }
}
