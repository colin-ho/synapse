use moka::future::Cache;
use std::path::PathBuf;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct AiCacheKey {
    pub buffer_prefix: String,
    pub cwd: PathBuf,
    pub project_type: Option<String>,
    pub git_branch: Option<String>,
}

pub fn create_context_cache<V: Clone + Send + Sync + 'static>() -> Cache<PathBuf, V> {
    Cache::builder()
        .max_capacity(200)
        .time_to_live(std::time::Duration::from_secs(300)) // 5 min TTL
        .build()
}

pub fn create_ai_cache() -> Cache<AiCacheKey, String> {
    Cache::builder()
        .max_capacity(500)
        .time_to_live(std::time::Duration::from_secs(600)) // 10 min TTL
        .build()
}
