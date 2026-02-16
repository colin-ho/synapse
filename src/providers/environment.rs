use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};

use crate::completion_context::{CompletionContext, Position};
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};

pub struct EnvironmentProvider {
    cache: RwLock<HashMap<String, CacheEntry>>,
    refresh_lock: Mutex<()>,
}

#[derive(Clone)]
struct CacheEntry {
    executables: Arc<Vec<String>>,
    valid_until: Instant,
}

impl Default for EnvironmentProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvironmentProvider {
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            refresh_lock: Mutex::new(()),
        }
    }

    /// Scan PATH directories from the daemon environment and refresh the cache.
    pub async fn scan_path(&self) {
        let env_hints = HashMap::new();
        let dirs = Self::collect_search_dirs(&env_hints);
        let cache_key = Self::cache_key(&dirs);
        self.refresh_cache(cache_key, dirs).await;
    }

    fn collect_search_dirs(env_hints: &HashMap<String, String>) -> Vec<PathBuf> {
        let path_value = env_hints
            .get("PATH")
            .cloned()
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();

        let mut dirs = Vec::new();
        let mut seen_dirs = HashSet::new();

        for dir in std::env::split_paths(&path_value) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            if seen_dirs.insert(dir.clone()) {
                dirs.push(dir);
            }
        }

        if let Some(virtual_env) = env_hints
            .get("VIRTUAL_ENV")
            .cloned()
            .or_else(|| std::env::var("VIRTUAL_ENV").ok())
        {
            let bin_dir = PathBuf::from(virtual_env).join("bin");
            if seen_dirs.insert(bin_dir.clone()) {
                dirs.push(bin_dir);
            }
        }

        dirs
    }

    fn cache_key(dirs: &[PathBuf]) -> String {
        dirs.iter()
            .map(|dir| dir.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn scan_directories(dirs: &[PathBuf]) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut executables = Vec::new();

        for dir_path in dirs {
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
                        let Ok(meta) = entry.metadata() else {
                            continue;
                        };
                        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
                            continue;
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let Ok(file_type) = entry.file_type() else {
                            continue;
                        };
                        if !file_type.is_file() {
                            continue;
                        }
                    }
                    if seen.insert(name.clone()) {
                        executables.push(name);
                    }
                }
            }
        }

        executables.sort_unstable();
        executables
    }

    async fn refresh_cache(&self, cache_key: String, dirs: Vec<PathBuf>) {
        let executables = tokio::task::spawn_blocking(move || Self::scan_directories(&dirs))
            .await
            .unwrap_or_default();

        let count = executables.len();
        let entry = CacheEntry {
            executables: Arc::new(executables),
            valid_until: Instant::now() + Duration::from_secs(60),
        };

        self.cache.write().await.insert(cache_key, entry);
        tracing::info!("Scanned PATH: {count} executables");
    }

    async fn ensure_cache(&self, cache_key: &str, dirs: &[PathBuf]) {
        let valid = self
            .cache
            .read()
            .await
            .get(cache_key)
            .is_some_and(|entry| Instant::now() < entry.valid_until);

        if valid {
            return;
        }

        let _guard = self.refresh_lock.lock().await;
        let valid = self
            .cache
            .read()
            .await
            .get(cache_key)
            .is_some_and(|entry| Instant::now() < entry.valid_until);
        if valid {
            return;
        }

        self.refresh_cache(cache_key.to_string(), dirs.to_vec())
            .await;
    }

    async fn cached_executables(&self, env_hints: &HashMap<String, String>) -> Arc<Vec<String>> {
        let dirs = Self::collect_search_dirs(env_hints);
        let cache_key = Self::cache_key(&dirs);
        self.ensure_cache(&cache_key, &dirs).await;

        self.cache
            .read()
            .await
            .get(&cache_key)
            .map(|entry| entry.executables.clone())
            .unwrap_or_default()
    }

    /// Check if this provider should activate for the given context.
    fn should_activate(ctx: &CompletionContext) -> bool {
        matches!(ctx.position, Position::CommandName | Position::PipeTarget)
    }

    /// Complete command names from PATH.
    async fn complete(&self, request: &ProviderRequest, max: usize) -> Vec<ProviderSuggestion> {
        let execs = self.cached_executables(&request.env_hints).await;
        let partial = request.partial.as_str();
        let prefix = request.prefix.as_str();

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
                text: format!("{prefix}{name}"),
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
        let env_hints = HashMap::new();
        let execs = self.cached_executables(&env_hints).await;
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

        self.complete(request, max).await
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Environment
    }

    fn is_available(&self) -> bool {
        true
    }
}
