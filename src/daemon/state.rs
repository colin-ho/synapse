use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::SplitSink;
use moka::future::Cache;
use tokio_util::codec::{Framed, LinesCodec};
use tokio_util::sync::CancellationToken;

use regex::Regex;

use crate::config::Config;
use crate::llm::LlmClient;
use crate::logging::InteractionLogger;
use crate::nl_cache::NlCache;
use crate::session::SessionManager;
use crate::spec_store::SpecStore;

pub(super) type SharedWriter =
    Arc<tokio::sync::Mutex<SplitSink<Framed<tokio::net::UnixStream, LinesCodec>, String>>>;

pub(super) struct RuntimeState {
    pub(super) spec_store: Arc<SpecStore>,
    pub(super) session_manager: SessionManager,
    pub(super) interaction_logger: InteractionLogger,
    pub(super) config: Config,
    pub(super) llm_client: Option<Arc<LlmClient>>,
    pub(super) nl_cache: NlCache,
    /// Cached project root per cwd.
    pub(super) project_root_cache: Cache<String, Option<PathBuf>>,
    /// Cached project type per project root.
    pub(super) project_type_cache: Cache<PathBuf, Option<String>>,
    /// Cached available tools per PATH string.
    pub(super) tools_cache: Cache<String, Vec<String>>,
    /// Pre-compiled blocklist patterns for command filtering.
    #[allow(dead_code)]
    pub(super) compiled_blocklist: CompiledBlocklist,
    /// Cancellation token for graceful shutdown.
    pub(super) shutdown_token: Option<CancellationToken>,
}

/// Pre-compiled blocklist patterns, built once at config load.
pub(super) struct CompiledBlocklist {
    patterns: Vec<CompiledBlockPattern>,
}

enum CompiledBlockPattern {
    /// Plain substring match (no wildcards).
    Substring(String),
    /// Compiled regex from a wildcard pattern.
    Regex(Regex),
}

impl CompiledBlocklist {
    pub(super) fn new(raw_patterns: &[String]) -> Self {
        let patterns = raw_patterns
            .iter()
            .filter_map(|p| {
                let trimmed = p.trim();
                if trimmed.is_empty() {
                    return None;
                }
                if !trimmed.contains('*') && !trimmed.contains('?') {
                    return Some(CompiledBlockPattern::Substring(trimmed.to_string()));
                }
                let regex_pattern = regex::escape(trimmed)
                    .replace(r"\*", ".*")
                    .replace(r"\?", ".");
                match Regex::new(&regex_pattern) {
                    Ok(re) => Some(CompiledBlockPattern::Regex(re)),
                    Err(_) => Some(CompiledBlockPattern::Substring(trimmed.to_string())),
                }
            })
            .collect();
        Self { patterns }
    }

    pub(super) fn is_blocked(&self, command: &str) -> bool {
        self.patterns.iter().any(|p| match p {
            CompiledBlockPattern::Substring(s) => command.contains(s.as_str()),
            CompiledBlockPattern::Regex(re) => re.is_match(command),
        })
    }
}

impl RuntimeState {
    pub(super) fn new(
        spec_store: Arc<SpecStore>,
        session_manager: SessionManager,
        interaction_logger: InteractionLogger,
        config: Config,
        llm_client: Option<Arc<LlmClient>>,
        nl_cache: NlCache,
    ) -> Self {
        let context_ttl = Duration::from_secs(300); // 5 min
        let compiled_blocklist = CompiledBlocklist::new(&config.security.command_blocklist);
        Self {
            spec_store,
            session_manager,
            interaction_logger,
            config,
            llm_client,
            nl_cache,
            project_root_cache: Cache::builder()
                .max_capacity(50)
                .time_to_live(context_ttl)
                .build(),
            project_type_cache: Cache::builder()
                .max_capacity(50)
                .time_to_live(context_ttl)
                .build(),
            tools_cache: Cache::builder()
                .max_capacity(5)
                .time_to_live(Duration::from_secs(600))
                .build(),
            compiled_blocklist,
            shutdown_token: None,
        }
    }

    pub(super) fn with_shutdown_token(mut self, token: CancellationToken) -> Self {
        self.shutdown_token = Some(token);
        self
    }
}
