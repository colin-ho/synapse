use std::path::{Path, PathBuf};

use async_trait::async_trait;
use moka::future::Cache;

use crate::completion_context::{CompletionContext, ExpectedType, Position};
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};

pub struct FilesystemProvider {
    dir_cache: Cache<PathBuf, Vec<DirEntry>>,
}

#[derive(Debug, Clone)]
struct DirEntry {
    name: String,
    is_dir: bool,
}

impl Default for FilesystemProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FilesystemProvider {
    pub fn new() -> Self {
        Self {
            dir_cache: Cache::builder()
                .max_capacity(200)
                .time_to_live(std::time::Duration::from_secs(5))
                .build(),
        }
    }

    /// Check if this provider should activate for the given context.
    fn should_activate(ctx: &CompletionContext) -> bool {
        matches!(
            ctx.expected_type,
            ExpectedType::FilePath | ExpectedType::Directory
        ) || matches!(ctx.position, Position::Redirect)
    }

    /// List directory entries, using cache.
    async fn list_dir(&self, dir: &Path) -> Vec<DirEntry> {
        if let Some(cached) = self.dir_cache.get(&dir.to_path_buf()).await {
            return cached;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    // Skip hidden files
                    if name.starts_with('.') {
                        return None;
                    }
                    let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                    Some(DirEntry { name, is_dir })
                })
                .collect(),
            Err(_) => Vec::new(),
        };

        self.dir_cache
            .insert(dir.to_path_buf(), entries.clone())
            .await;
        entries
    }

    /// Complete file/directory paths given the partial and cwd.
    async fn complete(&self, ctx: &CompletionContext, cwd: &Path) -> Vec<ProviderSuggestion> {
        let partial = &ctx.partial;
        let dirs_only = matches!(ctx.expected_type, ExpectedType::Directory);

        // Parse partial into directory prefix and filename prefix
        let (search_dir, file_prefix) = if partial.contains('/') {
            let path = Path::new(partial);
            let parent = path.parent().unwrap_or(Path::new(""));
            let fname = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let resolved = if parent.is_absolute() {
                parent.to_path_buf()
            } else {
                cwd.join(parent)
            };
            (resolved, fname)
        } else {
            (cwd.to_path_buf(), partial.clone())
        };

        let entries = self.list_dir(&search_dir).await;

        let mut results = Vec::new();
        for entry in &entries {
            if dirs_only && !entry.is_dir {
                continue;
            }
            if !file_prefix.is_empty() && !entry.name.starts_with(&file_prefix) {
                continue;
            }

            // Build the completion text: prefix + dir_part + name
            let dir_part = if partial.contains('/') {
                let idx = partial.rfind('/').unwrap();
                &partial[..=idx]
            } else {
                ""
            };

            let suffix = if entry.is_dir { "/" } else { "" };
            let completed = format!("{}{}{}{}", ctx.prefix, dir_part, entry.name, suffix);

            // Score: base 0.5 + specificity bonus for longer prefix matches
            let specificity = if file_prefix.is_empty() {
                0.0
            } else {
                0.1 * (file_prefix.len() as f64 / entry.name.len() as f64).min(1.0)
            };
            let score = (0.5 + specificity).clamp(0.0, 1.0);

            let kind = SuggestionKind::File;

            results.push(ProviderSuggestion {
                text: completed,
                source: SuggestionSource::Filesystem,
                score,
                description: None,
                kind,
            });
        }

        results
    }
}

#[async_trait]
impl SuggestionProvider for FilesystemProvider {
    async fn suggest(&self, request: &ProviderRequest, max: usize) -> Vec<ProviderSuggestion> {
        if max == 0 {
            return Vec::new();
        }

        if !Self::should_activate(request) {
            return Vec::new();
        }

        let cwd = Path::new(&request.cwd);
        let mut results = self.complete(request, cwd).await;

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(max);
        results
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Filesystem
    }

    fn is_available(&self) -> bool {
        true
    }
}
