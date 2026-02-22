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
    project_type: String,
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

    pub async fn get(
        &self,
        query: &str,
        cwd: &str,
        os: &str,
        project_type: &str,
    ) -> Option<NlCacheEntry> {
        let key = NlCacheKey {
            normalized_query: Self::normalize_query(query),
            cwd: cwd.to_string(),
            os: os.to_string(),
            project_type: project_type.to_string(),
        };
        self.cache.get(&key).await
    }

    pub async fn invalidate_all(&self) {
        self.cache.invalidate_all();
    }

    pub async fn insert(
        &self,
        query: &str,
        cwd: &str,
        os: &str,
        project_type: &str,
        entry: NlCacheEntry,
    ) {
        let key = NlCacheKey {
            normalized_query: Self::normalize_query(query),
            cwd: cwd.to_string(),
            os: os.to_string(),
            project_type: project_type.to_string(),
        };
        self.cache.insert(key, entry).await;
    }
}
