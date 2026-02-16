use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use tokio::sync::RwLock;

use async_trait::async_trait;

use crate::completion_context::Position;
use crate::config::HistoryConfig;
use crate::protocol::{SuggestionKind, SuggestionSource};
use crate::providers::{ProviderRequest, ProviderSuggestion, SuggestionProvider};

#[derive(Debug, Clone)]
struct HistoryEntry {
    frequency: u32,
    last_used: u64, // epoch seconds
}

struct HistoryData {
    entries: BTreeMap<String, HistoryEntry>,
    max_epoch: u64,
    max_freq: u32,
}

pub struct HistoryProvider {
    data: RwLock<HistoryData>,
    config: HistoryConfig,
}

impl HistoryProvider {
    pub fn new(config: HistoryConfig) -> Self {
        Self {
            data: RwLock::new(HistoryData {
                entries: BTreeMap::new(),
                max_epoch: 0,
                max_freq: 1,
            }),
            config,
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

            let entry = entries.entry(cmd).or_insert(HistoryEntry {
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
        *self.data.write().await = HistoryData {
            entries,
            max_epoch,
            max_freq,
        };
        tracing::info!("Loaded {count} history entries (max_freq={max_freq})");
    }
}

fn make_suggestion(command: &str, score: f64) -> ProviderSuggestion {
    ProviderSuggestion {
        text: command.to_string(),
        source: SuggestionSource::History,
        score,
        description: None,
        kind: SuggestionKind::History,
    }
}

/// Collect entries matching `prefix`, optionally filtering the argument portion
/// after `prefix` by `partial`. Returns raw scored tuples for further composition.
fn collect_prefix_matches<'a>(
    data: &'a HistoryData,
    prefix: &str,
    partial: &str,
) -> Vec<(f64, &'a str)> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut results = Vec::new();
    for (cmd, entry) in data.entries.range(prefix.to_string()..) {
        if !cmd.starts_with(prefix) {
            break;
        }
        if !partial.is_empty() {
            let arg_part = cmd[prefix.len()..].trim_start();
            if !arg_part.starts_with(partial) {
                continue;
            }
        }
        results.push((
            compute_score(entry, data.max_epoch, data.max_freq),
            cmd.as_str(),
        ));
    }
    results
}

/// Sort scored tuples by score descending, truncate, and convert to suggestions.
fn to_suggestions(mut results: Vec<(f64, &str)>, max: usize) -> Vec<ProviderSuggestion> {
    results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(max);
    results
        .into_iter()
        .map(|(score, cmd)| make_suggestion(cmd, score))
        .collect()
}

/// Collect fuzzy matches (Levenshtein distance within 30% threshold),
/// excluding entries already in `seen`.
fn fuzzy_matches<'a>(
    data: &'a HistoryData,
    query: &str,
    seen: &HashSet<&str>,
) -> Vec<(f64, &'a str)> {
    if query.len() < 2 {
        return Vec::new();
    }
    let first_char = match query.chars().next() {
        Some(c) => c,
        None => return Vec::new(),
    };

    let mut results = Vec::new();
    for (cmd, entry) in data.entries.range(first_char.to_string()..) {
        if !cmd.starts_with(first_char) {
            break;
        }
        if seen.contains(cmd.as_str()) {
            continue;
        }
        let distance = levenshtein(query, &cmd[..query.len().min(cmd.len())]);
        let max_distance = (query.len() as f64 * 0.3).ceil() as usize;
        if distance <= max_distance && distance > 0 && cmd.len() > query.len() {
            let base_score = compute_score(entry, data.max_epoch, data.max_freq);
            let fuzzy_penalty = 1.0 - (distance as f64 / query.len() as f64);
            let score = (base_score * fuzzy_penalty * 0.8).clamp(0.0, 1.0);
            results.push((score, cmd.as_str()));
        }
    }
    results
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

        let data = self.data.read().await;

        // Position-based contextual search
        let contextual = match &request.position {
            Position::Argument { .. } | Position::Subcommand | Position::OptionValue { .. } => {
                collect_prefix_matches(&data, &request.prefix, &request.partial)
            }
            Position::CommandName | Position::PipeTarget => {
                collect_prefix_matches(&data, &request.partial, "")
            }
            _ => Vec::new(),
        };
        if !contextual.is_empty() {
            return to_suggestions(contextual, max);
        }

        // Fallback: prefix match + optional fuzzy
        let mut results = collect_prefix_matches(&data, buffer, "");

        if self.config.fuzzy {
            let seen: HashSet<&str> = results.iter().map(|&(_, cmd)| cmd).collect();
            results.extend(fuzzy_matches(&data, buffer, &seen));
        }

        to_suggestions(results, max)
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
/// Formula: 0.6 * (ln(1+freq) / ln(1+max_freq)) + 0.4 * recency_decay
fn compute_score(entry: &HistoryEntry, max_epoch: u64, max_freq: u32) -> f64 {
    let freq_score = if max_freq > 1 {
        (1.0 + entry.frequency as f64).ln() / (1.0 + max_freq as f64).ln()
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

/// Simple Levenshtein distance (single-row DP)
fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let n = b_chars.len();
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0; n + 1];

    for (i, a_char) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &b_char) in b_chars.iter().enumerate() {
            let cost = usize::from(a_char != b_char);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
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

    #[test]
    fn test_parse_history_line_with_semicolons_in_command() {
        let (command, timestamp) = parse_history_line(": 1700000000:0;echo 'a;b;c'");
        assert_eq!(command, "echo 'a;b;c'");
        assert_eq!(timestamp, Some(1_700_000_000));
    }

    #[test]
    fn test_score_freq_one_is_positive() {
        let entry = HistoryEntry {
            frequency: 1,
            last_used: 1000,
        };
        let score = compute_score(&entry, 1000, 10);
        assert!(
            score > 0.0,
            "freq=1 entry must have positive score, got {score}"
        );
    }

    #[test]
    fn test_score_all_freq_one_equal() {
        let e1 = HistoryEntry {
            frequency: 1,
            last_used: 100,
        };
        let e2 = HistoryEntry {
            frequency: 1,
            last_used: 100,
        };
        let s1 = compute_score(&e1, 100, 1);
        let s2 = compute_score(&e2, 100, 1);
        assert!((s1 - s2).abs() < f64::EPSILON);
    }

    #[test]
    fn test_score_in_unit_range() {
        for freq in [1, 5, 100] {
            for max_freq in [1, 100, 50000] {
                if freq > max_freq {
                    continue;
                }
                for age in [0u64, 86400, 864000] {
                    let entry = HistoryEntry {
                        frequency: freq,
                        last_used: 1000u64.saturating_sub(age),
                    };
                    let score = compute_score(&entry, 1000, max_freq);
                    assert!(
                        (0.0..=1.0).contains(&score),
                        "score={score} out of range for freq={freq}, max_freq={max_freq}, age={age}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_score_recency_ordering() {
        let recent = HistoryEntry {
            frequency: 3,
            last_used: 1000,
        };
        let old = HistoryEntry {
            frequency: 3,
            last_used: 100,
        };
        assert!(compute_score(&recent, 1000, 10) > compute_score(&old, 1000, 10));
    }

    #[test]
    fn test_score_frequency_ordering() {
        let high = HistoryEntry {
            frequency: 50,
            last_used: 1000,
        };
        let low = HistoryEntry {
            frequency: 2,
            last_used: 1000,
        };
        assert!(compute_score(&high, 1000, 50) > compute_score(&low, 1000, 50));
    }
}
