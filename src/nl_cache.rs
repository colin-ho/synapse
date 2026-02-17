use moka::future::Cache;
use std::time::Duration;

/// Cache for natural language → command translations.
#[derive(Clone)]
pub struct NlCache {
    cache: Cache<NlCacheKey, NlCacheEntry>,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct NlCacheKey {
    normalized_query: String,
    cwd: String,
    os: String,
}

#[derive(Debug, Clone)]
pub struct NlCacheItem {
    pub command: String,
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NlCacheEntry {
    pub items: Vec<NlCacheItem>,
}

impl Default for NlCache {
    fn default() -> Self {
        Self::new()
    }
}

impl NlCache {
    pub fn new() -> Self {
        let cache = Cache::builder()
            .max_capacity(100)
            .time_to_live(Duration::from_secs(600)) // 10 min TTL
            .build();
        Self { cache }
    }

    /// Normalize a query: lowercase, collapse whitespace, strip trailing punctuation.
    fn normalize_query(query: &str) -> String {
        let collapsed: String = query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        collapsed
            .trim_end_matches(|c: char| c.is_ascii_punctuation())
            .to_string()
    }

    pub async fn get(&self, query: &str, cwd: &str, os: &str) -> Option<NlCacheEntry> {
        let key = NlCacheKey {
            normalized_query: Self::normalize_query(query),
            cwd: cwd.to_string(),
            os: os.to_string(),
        };
        self.cache.get(&key).await
    }

    pub async fn insert(&self, query: &str, cwd: &str, os: &str, entry: NlCacheEntry) {
        let key = NlCacheKey {
            normalized_query: Self::normalize_query(query),
            cwd: cwd.to_string(),
            os: os.to_string(),
        };
        self.cache.insert(key, entry).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_query_lowercase() {
        assert_eq!(
            NlCache::normalize_query("Find Large Files"),
            "find large files"
        );
    }

    #[test]
    fn test_normalize_query_collapse_whitespace() {
        assert_eq!(
            NlCache::normalize_query("find   large   files"),
            "find large files"
        );
    }

    #[test]
    fn test_normalize_query_strip_punctuation() {
        assert_eq!(
            NlCache::normalize_query("find large files?"),
            "find large files"
        );
        assert_eq!(
            NlCache::normalize_query("find large files."),
            "find large files"
        );
    }

    #[test]
    fn test_normalize_query_combined() {
        assert_eq!(
            NlCache::normalize_query("Find  Large Files??"),
            "find large files"
        );
    }

    #[tokio::test]
    async fn test_cache_hit() {
        let cache = NlCache::new();
        cache
            .insert(
                "find large files",
                "/tmp",
                "macOS",
                NlCacheEntry {
                    items: vec![NlCacheItem {
                        command: "find . -size +100M".into(),
                        warning: None,
                    }],
                },
            )
            .await;

        let result = cache.get("find large files", "/tmp", "macOS").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().items[0].command, "find . -size +100M");
    }

    #[tokio::test]
    async fn test_cache_normalization_hit() {
        let cache = NlCache::new();
        cache
            .insert(
                "Find Large Files?",
                "/tmp",
                "macOS",
                NlCacheEntry {
                    items: vec![NlCacheItem {
                        command: "find . -size +100M".into(),
                        warning: None,
                    }],
                },
            )
            .await;

        // Should hit with different capitalization/punctuation
        let result = cache.get("find large files", "/tmp", "macOS").await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_cache_miss_different_cwd() {
        let cache = NlCache::new();
        cache
            .insert(
                "find large files",
                "/tmp",
                "macOS",
                NlCacheEntry {
                    items: vec![NlCacheItem {
                        command: "find . -size +100M".into(),
                        warning: None,
                    }],
                },
            )
            .await;

        let result = cache.get("find large files", "/home", "macOS").await;
        assert!(result.is_none());
    }
}
