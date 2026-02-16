use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use async_trait::async_trait;

use crate::completion_context::{CompletionContext, Position};
use crate::config::HistoryConfig;
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};

#[derive(Debug, Clone)]
struct HistoryEntry {
    command: String,
    frequency: u32,
    last_used: u64, // epoch seconds
}

pub struct HistoryProvider {
    entries: Arc<RwLock<BTreeMap<String, HistoryEntry>>>,
    config: HistoryConfig,
    max_epoch: Arc<RwLock<u64>>,
    max_freq: Arc<RwLock<u32>>,
}

impl HistoryProvider {
    pub fn new(config: HistoryConfig) -> Self {
        Self {
            entries: Arc::new(RwLock::new(BTreeMap::new())),
            config,
            max_epoch: Arc::new(RwLock::new(0)),
            max_freq: Arc::new(RwLock::new(1)),
        }
    }

    pub async fn load_history(&self) {
        let histfile = std::env::var("HISTFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".zsh_history"));

        if !histfile.exists() {
            tracing::warn!("History file not found: {}", histfile.display());
            return;
        }

        // Read as bytes to handle potentially invalid UTF-8
        let bytes = match std::fs::read(&histfile) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Failed to read history file: {e}");
                return;
            }
        };

        let content = String::from_utf8_lossy(&bytes);
        let mut entries = BTreeMap::new();
        let mut max_epoch: u64 = 0;
        let mut counter: u64 = 0;
        let mut continuation = String::new();

        for line in content.lines() {
            // Handle multi-line commands (lines ending with \)
            if line.ends_with('\\') {
                continuation.push_str(line.trim_end_matches('\\'));
                continuation.push('\n');
                continue;
            }

            let full_line = if continuation.is_empty() {
                line.to_string()
            } else {
                let mut full = std::mem::take(&mut continuation);
                full.push_str(line);
                full
            };

            let (command, timestamp) = parse_history_line(&full_line);

            if command.is_empty() {
                continue;
            }

            // Only take first line for multi-line commands stored in history
            let cmd = command
                .lines()
                .next()
                .unwrap_or(&command)
                .trim()
                .to_string();
            if cmd.is_empty() {
                continue;
            }

            let ts = timestamp.unwrap_or_else(|| {
                counter += 1;
                counter
            });
            if ts > max_epoch {
                max_epoch = ts;
            }

            let entry = entries.entry(cmd.clone()).or_insert_with(|| HistoryEntry {
                command: cmd,
                frequency: 0,
                last_used: 0,
            });
            entry.frequency += 1;
            if ts > entry.last_used {
                entry.last_used = ts;
            }
        }

        // Enforce max_entries: keep the most recently used
        if entries.len() > self.config.max_entries {
            let mut sorted: Vec<_> = entries.into_iter().collect();
            sorted.sort_by(|a, b| b.1.last_used.cmp(&a.1.last_used));
            sorted.truncate(self.config.max_entries);
            entries = sorted.into_iter().collect();
        }

        // Track max frequency across all entries
        let max_freq = entries
            .values()
            .map(|e| e.frequency)
            .max()
            .unwrap_or(1)
            .max(1);

        let count = entries.len();
        *self.entries.write().await = entries;
        *self.max_epoch.write().await = max_epoch;
        *self.max_freq.write().await = max_freq;
        tracing::info!("Loaded {count} history entries (max_freq={max_freq})");
    }

    async fn prefix_search(&self, prefix: &str) -> Option<ProviderSuggestion> {
        if prefix.is_empty() {
            return None;
        }

        let entries = self.entries.read().await;
        let max_epoch = *self.max_epoch.read().await;
        let max_freq = *self.max_freq.read().await;

        // BTreeMap range query for prefix matching
        let end = increment_last_char(prefix);
        let range = match &end {
            Some(e) => entries.range(prefix.to_string()..e.clone()),
            None => entries.range(prefix.to_string()..),
        };

        let mut best: Option<(f64, &HistoryEntry)> = None;

        for (_key, entry) in range {
            if !entry.command.starts_with(prefix) {
                break;
            }
            let score = compute_score(entry, max_epoch, max_freq);
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, entry));
            }
        }

        best.map(|(score, entry)| ProviderSuggestion {
            text: entry.command.clone(),
            source: SuggestionSource::History,
            score,
            description: None,
            kind: SuggestionKind::History,
        })
    }

    /// Search history by argument: use ctx.prefix as a BTreeMap range key,
    /// then filter the argument portion by ctx.partial.
    async fn argument_search(&self, ctx: &CompletionContext) -> Option<ProviderSuggestion> {
        if ctx.prefix.is_empty() {
            return None;
        }

        let entries = self.entries.read().await;
        let max_epoch = *self.max_epoch.read().await;
        let max_freq = *self.max_freq.read().await;

        let end = increment_last_char(&ctx.prefix);
        let range = match &end {
            Some(e) => entries.range(ctx.prefix.clone()..e.clone()),
            None => entries.range(ctx.prefix.clone()..),
        };

        let mut best: Option<(f64, &HistoryEntry)> = None;

        for (_key, entry) in range {
            if !entry.command.starts_with(&ctx.prefix) {
                break;
            }

            // Extract the argument portion after the prefix
            let arg_part = entry.command[ctx.prefix.len()..].trim_start();
            if ctx.partial.is_empty() || arg_part.starts_with(&ctx.partial) {
                let score = compute_score(entry, max_epoch, max_freq);
                if best.as_ref().is_none_or(|(s, _)| score > *s) {
                    best = Some((score, entry));
                }
            }
        }

        best.map(|(score, entry)| ProviderSuggestion {
            text: entry.command.clone(),
            source: SuggestionSource::History,
            score,
            description: None,
            kind: SuggestionKind::History,
        })
    }

    /// Search history by first token: for CommandName/PipeTarget positions,
    /// match the partial against the first word of each history entry.
    async fn first_token_search(&self, partial: &str) -> Option<ProviderSuggestion> {
        if partial.is_empty() {
            return None;
        }

        let entries = self.entries.read().await;
        let max_epoch = *self.max_epoch.read().await;
        let max_freq = *self.max_freq.read().await;

        let mut best: Option<(f64, &HistoryEntry)> = None;

        for (_key, entry) in entries.iter() {
            let first_word = entry.command.split_whitespace().next().unwrap_or("");
            if first_word.starts_with(partial) {
                let score = compute_score(entry, max_epoch, max_freq);
                if best.as_ref().is_none_or(|(s, _)| score > *s) {
                    best = Some((score, entry));
                }
            }
        }

        best.map(|(score, entry)| ProviderSuggestion {
            text: entry.command.clone(),
            source: SuggestionSource::History,
            score,
            description: None,
            kind: SuggestionKind::History,
        })
    }

    async fn argument_search_multi(
        &self,
        ctx: &CompletionContext,
        max: usize,
    ) -> Vec<ProviderSuggestion> {
        if ctx.prefix.is_empty() {
            return Vec::new();
        }

        let entries = self.entries.read().await;
        let max_epoch = *self.max_epoch.read().await;
        let max_freq = *self.max_freq.read().await;

        let end = increment_last_char(&ctx.prefix);
        let range = match &end {
            Some(e) => entries.range(ctx.prefix.clone()..e.clone()),
            None => entries.range(ctx.prefix.clone()..),
        };

        let mut results: Vec<(f64, &HistoryEntry)> = Vec::new();
        for (_key, entry) in range {
            if !entry.command.starts_with(&ctx.prefix) {
                break;
            }

            let arg_part = entry.command[ctx.prefix.len()..].trim_start();
            if ctx.partial.is_empty() || arg_part.starts_with(&ctx.partial) {
                let score = compute_score(entry, max_epoch, max_freq);
                results.push((score, entry));
            }
        }

        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(max);
        results
            .into_iter()
            .map(|(score, entry)| ProviderSuggestion {
                text: entry.command.clone(),
                source: SuggestionSource::History,
                score,
                description: None,
                kind: SuggestionKind::History,
            })
            .collect()
    }

    async fn first_token_search_multi(&self, partial: &str, max: usize) -> Vec<ProviderSuggestion> {
        if partial.is_empty() {
            return Vec::new();
        }

        let entries = self.entries.read().await;
        let max_epoch = *self.max_epoch.read().await;
        let max_freq = *self.max_freq.read().await;

        let mut results: Vec<(f64, &HistoryEntry)> = Vec::new();
        for (_key, entry) in entries.iter() {
            let first_word = entry.command.split_whitespace().next().unwrap_or("");
            if first_word.starts_with(partial) {
                let score = compute_score(entry, max_epoch, max_freq);
                results.push((score, entry));
            }
        }

        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(max);
        results
            .into_iter()
            .map(|(score, entry)| ProviderSuggestion {
                text: entry.command.clone(),
                source: SuggestionSource::History,
                score,
                description: None,
                kind: SuggestionKind::History,
            })
            .collect()
    }

    pub async fn fuzzy_search(&self, query: &str) -> Option<ProviderSuggestion> {
        if query.is_empty() || query.len() < 2 {
            return None;
        }

        let entries = self.entries.read().await;
        let max_epoch = *self.max_epoch.read().await;
        let max_freq = *self.max_freq.read().await;
        let first_char = query.chars().next()?;

        let mut best: Option<(f64, &HistoryEntry)> = None;

        for (_key, entry) in entries.iter() {
            // Only consider commands starting with the same first character
            if !entry.command.starts_with(first_char) {
                continue;
            }

            let distance = levenshtein(
                query,
                &entry.command[..query.len().min(entry.command.len())],
            );
            let max_distance = (query.len() as f64 * 0.3).ceil() as usize; // 30% threshold

            if distance <= max_distance && entry.command.len() > query.len() {
                let base_score = compute_score(entry, max_epoch, max_freq);
                // Penalize by edit distance
                let fuzzy_penalty = 1.0 - (distance as f64 / query.len() as f64);
                let score = (base_score * fuzzy_penalty * 0.8).clamp(0.0, 1.0);

                if best.as_ref().is_none_or(|(s, _)| score > *s) {
                    best = Some((score, entry));
                }
            }
        }

        best.map(|(score, entry)| ProviderSuggestion {
            text: entry.command.clone(),
            source: SuggestionSource::History,
            score,
            description: None,
            kind: SuggestionKind::History,
        })
    }
}

#[async_trait]
impl SuggestionProvider for HistoryProvider {
    async fn suggest(&self, request: &ProviderRequest, max: usize) -> Vec<ProviderSuggestion> {
        if max == 0 {
            return Vec::new();
        }

        let buffer = request.buffer.as_str();
        if buffer.is_empty() {
            return Vec::new();
        }

        if max == 1 {
            // Preserve single-suggestion behavior for inline ghost text mode.
            match &request.position {
                Position::Argument { .. } | Position::Subcommand | Position::OptionValue { .. } => {
                    if let Some(s) = self.argument_search(request).await {
                        return vec![s];
                    }
                }
                Position::CommandName | Position::PipeTarget => {
                    if let Some(s) = self.first_token_search(&request.partial).await {
                        return vec![s];
                    }
                }
                _ => {}
            }

            // Fallback: full prefix match
            if let Some(suggestion) = self.prefix_search(buffer).await {
                return vec![suggestion];
            }

            // Fall back to fuzzy if enabled
            if self.config.fuzzy {
                return self.fuzzy_search(buffer).await.into_iter().collect();
            }

            return Vec::new();
        }

        let contextual = match &request.position {
            Position::Argument { .. } | Position::Subcommand | Position::OptionValue { .. } => {
                self.argument_search_multi(request, max).await
            }
            Position::CommandName | Position::PipeTarget => {
                self.first_token_search_multi(&request.partial, max).await
            }
            _ => Vec::new(),
        };
        if !contextual.is_empty() {
            return contextual;
        }

        let entries = self.entries.read().await;
        let max_epoch = *self.max_epoch.read().await;
        let max_freq = *self.max_freq.read().await;

        // Prefix range query — collect all matches
        let end = increment_last_char(buffer);
        let range = match &end {
            Some(e) => entries.range(buffer.to_string()..e.clone()),
            None => entries.range(buffer.to_string()..),
        };

        let mut results: Vec<(f64, &HistoryEntry)> = Vec::new();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

        for (_key, entry) in range {
            if !entry.command.starts_with(buffer) {
                break;
            }
            let score = compute_score(entry, max_epoch, max_freq);
            results.push((score, entry));
            seen.insert(&entry.command);
        }

        // Also include fuzzy matches if enabled
        if self.config.fuzzy && buffer.len() >= 2 {
            let first_char = buffer.chars().next();
            if let Some(fc) = first_char {
                for (_key, entry) in entries.iter() {
                    if seen.contains(entry.command.as_str()) {
                        continue;
                    }
                    if !entry.command.starts_with(fc) {
                        continue;
                    }
                    let distance = levenshtein(
                        buffer,
                        &entry.command[..buffer.len().min(entry.command.len())],
                    );
                    let max_distance = (buffer.len() as f64 * 0.3).ceil() as usize;
                    if distance <= max_distance
                        && distance > 0
                        && entry.command.len() > buffer.len()
                    {
                        let base_score = compute_score(entry, max_epoch, max_freq);
                        let fuzzy_penalty = 1.0 - (distance as f64 / buffer.len() as f64);
                        let score = (base_score * fuzzy_penalty * 0.8).clamp(0.0, 1.0);
                        results.push((score, entry));
                    }
                }
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(max);

        results
            .into_iter()
            .map(|(score, entry)| ProviderSuggestion {
                text: entry.command.clone(),
                source: SuggestionSource::History,
                score,
                description: None,
                kind: SuggestionKind::History,
            })
            .collect()
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::History
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// Parse a zsh history line. Supports:
/// - Extended format: `: 1234567890:0;command`
/// - Simple format: `command`
fn parse_history_line(line: &str) -> (String, Option<u64>) {
    let trimmed = line.trim();

    // Extended history format: `: timestamp:duration;command`
    if trimmed.starts_with(": ") {
        if let Some(semi_pos) = trimmed.find(';') {
            let meta = &trimmed[2..semi_pos];
            let command = trimmed[semi_pos + 1..].to_string();
            let timestamp = meta
                .split(':')
                .next()
                .and_then(|s| s.trim().parse::<u64>().ok());
            return (command, timestamp);
        }
    }

    (trimmed.to_string(), None)
}

/// Compute a normalized score in [0, 1] based on frequency and recency.
/// Formula: 0.6 * (ln(freq) / ln(max_freq)) + 0.4 * recency_decay
fn compute_score(entry: &HistoryEntry, max_epoch: u64, max_freq: u32) -> f64 {
    let freq_score = if max_freq > 1 {
        (entry.frequency as f64).ln() / (max_freq as f64).ln()
    } else {
        // All entries have frequency 1 — treat as equal
        1.0
    };

    let recency_score = if max_epoch > 0 && entry.last_used > 0 {
        let age = max_epoch.saturating_sub(entry.last_used) as f64;
        (-age / 86400.0 * 0.1).exp()
    } else {
        0.5
    };

    (0.6 * freq_score + 0.4 * recency_score).clamp(0.0, 1.0)
}

/// Increment the last character to create an exclusive upper bound for range queries
fn increment_last_char(s: &str) -> Option<String> {
    let mut chars: Vec<char> = s.chars().collect();
    if let Some(last) = chars.last_mut() {
        *last = char::from_u32(*last as u32 + 1)?;
        Some(chars.into_iter().collect())
    } else {
        None
    }
}

/// Simple Levenshtein distance
#[allow(clippy::needless_range_loop)]
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }

    dp[m][n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("abc", "abd"), 1);
    }

    #[test]
    fn test_parse_history_line_extended_format() {
        let (command, timestamp) = parse_history_line(": 1700000000:0;git status");
        assert_eq!(command, "git status");
        assert_eq!(timestamp, Some(1_700_000_000));
    }

    #[test]
    fn test_parse_history_line_simple_format() {
        let (command, timestamp) = parse_history_line("cargo test");
        assert_eq!(command, "cargo test");
        assert_eq!(timestamp, None);
    }
}
