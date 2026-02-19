use std::path::PathBuf;
use std::sync::{Arc, RwLock};
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
use crate::spec_store::SpecStore;

pub(super) type SharedWriter =
    Arc<tokio::sync::Mutex<SplitSink<Framed<tokio::net::UnixStream, LinesCodec>, String>>>;

pub(super) struct RuntimeState {
    pub(super) spec_store: Arc<SpecStore>,
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
    /// Recent accepted NL translations for few-shot examples: (query, command).
    pub(super) interaction_examples: RwLock<Vec<(String, String)>>,
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

/// Maximum number of few-shot examples to store.
const MAX_INTERACTION_EXAMPLES: usize = 50;

impl RuntimeState {
    pub(super) fn new(
        spec_store: Arc<SpecStore>,
        interaction_logger: InteractionLogger,
        config: Config,
        llm_client: Option<Arc<LlmClient>>,
        nl_cache: NlCache,
    ) -> Self {
        let context_ttl = Duration::from_secs(300); // 5 min
        let compiled_blocklist = CompiledBlocklist::new(&config.security.command_blocklist);

        // Load interaction examples from log
        let examples = crate::logging::read_recent_accepted(
            &config.interaction_log_path(),
            MAX_INTERACTION_EXAMPLES,
        );
        tracing::debug!(
            "Loaded {} interaction examples for few-shot",
            examples.len()
        );

        Self {
            spec_store,
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
            interaction_examples: RwLock::new(examples),
        }
    }

    pub(super) fn with_shutdown_token(mut self, token: CancellationToken) -> Self {
        self.shutdown_token = Some(token);
        self
    }

    /// Record a new accepted NL translation for future few-shot use.
    pub(super) fn record_interaction_example(&self, query: String, command: String) {
        let mut examples = match self.interaction_examples.write() {
            Ok(guard) => guard,
            Err(e) => e.into_inner(),
        };
        // Avoid duplicates (by query)
        if examples.iter().any(|(q, _)| q == &query) {
            return;
        }
        examples.insert(0, (query, command)); // newest first
        examples.truncate(MAX_INTERACTION_EXAMPLES);
    }

    /// Select the most relevant few-shot examples for a query.
    /// Uses simple token overlap scoring, returns up to 5 examples.
    pub(super) fn get_few_shot_examples(&self, query: &str) -> Vec<(String, String)> {
        let examples = self
            .interaction_examples
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if examples.is_empty() {
            return Vec::new();
        }

        let query_tokens: Vec<String> =
            query.split_whitespace().map(|t| t.to_lowercase()).collect();

        let mut scored: Vec<(usize, &(String, String))> = examples
            .iter()
            .map(|ex| {
                let score =
                    ex.0.split_whitespace()
                        .filter(|t| {
                            let lower = t.to_lowercase();
                            query_tokens.iter().any(|qt| qt == &lower)
                        })
                        .count();
                (score, ex)
            })
            .filter(|(score, _)| *score > 0)
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored
            .into_iter()
            .take(5)
            .map(|(_, ex)| ex.clone())
            .collect()
    }
}
