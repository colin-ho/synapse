use std::cmp::Ordering;
use std::num::NonZeroUsize;
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

#[derive(Debug)]
struct PathQuery {
    search_dir: PathBuf,
    typed_dir_part: String,
    file_prefix: String,
    include_hidden: bool,
}

#[derive(Debug, Clone, Copy)]
enum QuoteMode {
    None,
    Single,
    Double,
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
        if matches!(
            ctx.expected_type,
            ExpectedType::FilePath | ExpectedType::Directory
        ) || matches!(ctx.position, Position::Redirect)
        {
            return true;
        }

        // Fallback: activate for argument positions with no specific type
        if matches!(ctx.position, Position::Argument { .. })
            && matches!(ctx.expected_type, ExpectedType::Any)
        {
            return true;
        }

        // Activate when the partial looks like a path
        let p = ctx.partial.as_str();
        p.starts_with('/')
            || p.starts_with("./")
            || p.starts_with("../")
            || p.starts_with("~/")
            || p == "."
            || p == ".."
            || p == "~"
    }

    fn resolve_dir_input(input: &str, cwd: &Path) -> PathBuf {
        let trimmed = input.trim_end_matches('/');

        if trimmed.is_empty() {
            return cwd.to_path_buf();
        }

        if trimmed == "~" {
            if let Some(home) = dirs::home_dir() {
                return home;
            }
        }

        if let Some(rest) = trimmed.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(rest);
            }
        }

        let path = Path::new(trimmed);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }
    }

    fn parse_path_query(partial: &str, cwd: &Path) -> PathQuery {
        if partial.is_empty() {
            return PathQuery {
                search_dir: cwd.to_path_buf(),
                typed_dir_part: String::new(),
                file_prefix: String::new(),
                include_hidden: false,
            };
        }

        let (typed_dir_part, file_prefix) = if partial.ends_with('/') {
            (partial.to_string(), String::new())
        } else if let Some(idx) = partial.rfind('/') {
            (partial[..=idx].to_string(), partial[idx + 1..].to_string())
        } else {
            (String::new(), partial.to_string())
        };

        let search_dir = if typed_dir_part.is_empty() {
            cwd.to_path_buf()
        } else {
            Self::resolve_dir_input(&typed_dir_part, cwd)
        };

        PathQuery {
            search_dir,
            typed_dir_part,
            include_hidden: file_prefix.starts_with('.'),
            file_prefix,
        }
    }

    fn quote_mode_for_buffer(buffer: &str) -> QuoteMode {
        let mut in_single = false;
        let mut in_double = false;
        let mut escaped = false;

        for ch in buffer.chars() {
            if escaped {
                escaped = false;
                continue;
            }

            if ch == '\\' && !in_single {
                escaped = true;
                continue;
            }
            if ch == '\'' && !in_double {
                in_single = !in_single;
                continue;
            }
            if ch == '"' && !in_single {
                in_double = !in_double;
                continue;
            }
        }

        if in_single {
            QuoteMode::Single
        } else if in_double {
            QuoteMode::Double
        } else {
            QuoteMode::None
        }
    }

    fn should_escape_unquoted_char(ch: char) -> bool {
        ch.is_ascii_whitespace()
            || matches!(
                ch,
                '\\' | '"'
                    | '\''
                    | '$'
                    | '`'
                    | '&'
                    | '|'
                    | ';'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '*'
                    | '?'
                    | '!'
                    | '#'
            )
    }

    fn escape_suffix(suffix: &str, mode: QuoteMode) -> String {
        let mut out = String::with_capacity(suffix.len());

        match mode {
            QuoteMode::None => {
                for ch in suffix.chars() {
                    if Self::should_escape_unquoted_char(ch) {
                        out.push('\\');
                    }
                    out.push(ch);
                }
            }
            QuoteMode::Double => {
                for ch in suffix.chars() {
                    if matches!(ch, '\\' | '"' | '$' | '`') {
                        out.push('\\');
                    }
                    out.push(ch);
                }
            }
            QuoteMode::Single => {
                for ch in suffix.chars() {
                    if ch == '\'' {
                        out.push_str("'\\''");
                        continue;
                    }
                    out.push(ch);
                }
            }
        }

        out
    }

    fn build_suggestion_text(
        buffer: &str,
        logical_partial: &str,
        logical_completed: &str,
    ) -> String {
        debug_assert!(
            logical_completed.starts_with(logical_partial),
            "logical completion must extend the typed partial"
        );

        let Some(suffix) = logical_completed.strip_prefix(logical_partial) else {
            return buffer.to_string();
        };
        if !suffix.is_empty() {
            let escaped_suffix = Self::escape_suffix(suffix, Self::quote_mode_for_buffer(buffer));
            return format!("{buffer}{escaped_suffix}");
        }

        buffer.to_string()
    }

    fn prefix_matches(name: &str, prefix: &str) -> bool {
        if cfg!(target_os = "macos") {
            name.len() >= prefix.len()
                && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        } else {
            name.starts_with(prefix)
        }
    }

    fn read_dir_entries(dir: &Path) -> Vec<DirEntry> {
        let mut entries = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let is_dir = e
                        .file_type()
                        .map(|ft| ft.is_dir() || (ft.is_symlink() && e.path().is_dir()))
                        .unwrap_or(false);
                    DirEntry { name, is_dir }
                })
                .collect::<Vec<_>>(),
            Err(err) => {
                tracing::debug!(
                    path = %dir.display(),
                    "FilesystemProvider: failed to read directory: {err}"
                );
                Vec::new()
            }
        };

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    /// List directory entries, using cache.
    async fn list_dir(&self, dir: &Path) -> Vec<DirEntry> {
        let key = dir.to_path_buf();
        self.dir_cache
            .get_with(key.clone(), async {
                match tokio::task::spawn_blocking(move || Self::read_dir_entries(&key)).await {
                    Ok(entries) => entries,
                    Err(err) => {
                        tracing::debug!("FilesystemProvider: directory task failed: {err}");
                        Vec::new()
                    }
                }
            })
            .await
    }

    /// Complete file/directory paths given the partial and cwd.
    async fn complete(&self, ctx: &CompletionContext, cwd: &Path) -> Vec<ProviderSuggestion> {
        let dirs_only = matches!(ctx.expected_type, ExpectedType::Directory);
        let path_query = Self::parse_path_query(&ctx.partial, cwd);

        let entries = self.list_dir(&path_query.search_dir).await;

        let mut results = Vec::new();
        for entry in &entries {
            if !path_query.include_hidden && entry.name.starts_with('.') {
                continue;
            }
            if dirs_only && !entry.is_dir {
                continue;
            }
            if !path_query.file_prefix.is_empty()
                && !Self::prefix_matches(&entry.name, &path_query.file_prefix)
            {
                continue;
            }

            let suffix = if entry.is_dir { "/" } else { "" };
            let logical_completed =
                format!("{}{}{}", path_query.typed_dir_part, entry.name, suffix);
            let completed =
                Self::build_suggestion_text(&ctx.buffer, &ctx.partial, &logical_completed);

            let specificity = if path_query.file_prefix.is_empty() {
                0.0
            } else {
                path_query.file_prefix.len() as f64 / entry.name.len().max(1) as f64
            };
            let dir_bonus = if entry.is_dir { 0.03 } else { 0.0 };
            let exact_bonus =
                if !path_query.file_prefix.is_empty() && path_query.file_prefix == entry.name {
                    0.06
                } else {
                    0.0
                };
            let score = (0.55 + 0.25 * specificity + dir_bonus + exact_bonus).clamp(0.0, 1.0);

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
    async fn suggest(
        &self,
        request: &ProviderRequest,
        max: NonZeroUsize,
    ) -> Vec<ProviderSuggestion> {
        if !Self::should_activate(request) {
            return Vec::new();
        }

        let cwd = Path::new(&request.cwd);
        let mut results = self.complete(request, cwd).await;

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.text.cmp(&b.text))
        });
        results.truncate(max.get());
        results
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Filesystem
    }

    fn is_available(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::providers::SuggestionProvider;
    use crate::test_helpers::{limit, make_provider_request};

    #[tokio::test]
    async fn test_filesystem_provider_trailing_slash_descends_into_directory() {
        let provider = FilesystemProvider::new();
        let dir = tempfile::tempdir().unwrap();

        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src").join("main.rs"), "fn main() {}\n").unwrap();

        let req = make_provider_request("cat src/", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

        assert!(
            texts.iter().any(|t| *t == "cat src/main.rs"),
            "Expected completion from inside src/, got: {:?}",
            texts
        );
        assert!(
            !texts.iter().any(|t| *t == "cat src/src/"),
            "Unexpected duplicated path segment in suggestions: {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_filesystem_provider_hides_dotfiles_unless_user_types_dot_prefix() {
        let provider = FilesystemProvider::new();
        let dir = tempfile::tempdir().unwrap();

        fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();
        fs::write(dir.path().join("app.toml"), "name = \"app\"\n").unwrap();

        let req_default = make_provider_request("cat ", dir.path().to_str().unwrap()).await;
        let default_results = provider.suggest(&req_default, limit(20)).await;
        let default_texts: Vec<&str> = default_results.iter().map(|s| s.text.as_str()).collect();
        assert!(
            !default_texts.iter().any(|t| *t == "cat .env"),
            "Dotfile should not appear without dot prefix, got: {:?}",
            default_texts
        );

        let req_dot = make_provider_request("cat .", dir.path().to_str().unwrap()).await;
        let dot_results = provider.suggest(&req_dot, limit(20)).await;
        let dot_texts: Vec<&str> = dot_results.iter().map(|s| s.text.as_str()).collect();
        assert!(
            dot_texts.iter().any(|t| *t == "cat .env"),
            "Expected dotfile completion when partial starts with '.', got: {:?}",
            dot_texts
        );
    }

    #[tokio::test]
    async fn test_filesystem_provider_directory_mode_filters_out_files() {
        let provider = FilesystemProvider::new();
        let dir = tempfile::tempdir().unwrap();

        fs::create_dir_all(dir.path().join("logs")).unwrap();
        fs::write(dir.path().join("log.txt"), "log\n").unwrap();

        let req = make_provider_request("cd l", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

        assert!(
            texts.iter().any(|t| *t == "cd logs/"),
            "Expected directory suggestion, got: {:?}",
            texts
        );
        assert!(
            !texts.iter().any(|t| *t == "cd log.txt"),
            "File should not appear for directory completions, got: {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_filesystem_provider_supports_redirect_context() {
        let provider = FilesystemProvider::new();
        let dir = tempfile::tempdir().unwrap();

        fs::write(dir.path().join("output.txt"), "data\n").unwrap();

        let req = make_provider_request("echo hi > out", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

        assert!(
            texts.iter().any(|t| *t == "echo hi > output.txt"),
            "Expected redirect file completion, got: {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_filesystem_provider_orders_results_deterministically() {
        let provider = FilesystemProvider::new();
        let dir = tempfile::tempdir().unwrap();

        fs::create_dir_all(dir.path().join("a_dir")).unwrap();
        fs::create_dir_all(dir.path().join("b_dir")).unwrap();
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        fs::write(dir.path().join("b.txt"), "b\n").unwrap();

        let req = make_provider_request("cat ", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;

        assert!(
            !results.is_empty(),
            "Expected non-empty filesystem suggestions"
        );
        assert_eq!(
            results[0].text, "cat a_dir/",
            "Expected deterministic ordering by score then text"
        );
    }

    #[tokio::test]
    async fn test_filesystem_provider_escapes_spaces_for_unquoted_input() {
        let provider = FilesystemProvider::new();
        let dir = tempfile::tempdir().unwrap();

        fs::write(dir.path().join("My File.txt"), "x\n").unwrap();

        let req = make_provider_request("cat My", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

        assert!(
            texts.iter().any(|t| *t == r"cat My\ File.txt"),
            "Expected escaped-space completion for unquoted input, got: {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_filesystem_provider_preserves_double_quote_context() {
        let provider = FilesystemProvider::new();
        let dir = tempfile::tempdir().unwrap();

        fs::write(dir.path().join("My File.txt"), "x\n").unwrap();

        let req = make_provider_request(r#"cat "My"#, dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

        assert!(
            texts.iter().any(|t| *t == r#"cat "My File.txt"#),
            "Expected quoted completion to keep spaces unescaped in quote context, got: {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_filesystem_provider_preserves_escaped_partial_prefix() {
        let provider = FilesystemProvider::new();
        let dir = tempfile::tempdir().unwrap();

        fs::write(dir.path().join("My File.txt"), "x\n").unwrap();

        let req = make_provider_request(r"cat My\ F", dir.path().to_str().unwrap()).await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

        assert!(
            texts.iter().any(|t| *t == r"cat My\ File.txt"),
            "Expected completion to extend an escaped partial without breaking prefix, got: {:?}",
            texts
        );
    }

    #[tokio::test]
    async fn test_filesystem_provider_expands_tilde_paths() {
        let provider = FilesystemProvider::new();
        let home = dirs::home_dir().expect("home directory should exist");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let test_dir_name = format!("synapse_fs_test_{unique}");
        let test_dir = home.join(&test_dir_name);
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(test_dir.join("file.txt"), "x\n").unwrap();

        let request_buffer = format!("cat ~/{test_dir_name}/fi");
        let expected = format!("cat ~/{test_dir_name}/file.txt");

        let req = make_provider_request(&request_buffer, "/tmp").await;
        let results = provider.suggest(&req, limit(10)).await;
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

        let _ = fs::remove_dir_all(&test_dir);

        assert!(
            texts.iter().any(|t| *t == expected),
            "Expected tilde-expanded completion, got: {:?}",
            texts
        );
    }
}
