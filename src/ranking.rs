use std::collections::HashMap;

use crate::config::WeightsConfig;
use crate::protocol::{SuggestionItem, SuggestionKind, SuggestionSource};
use crate::providers::ProviderSuggestion;

#[derive(Debug, Clone)]
pub struct RankedSuggestion {
    pub text: String,
    pub source: SuggestionSource,
    pub score: f64,
    pub description: Option<String>,
    pub kind: SuggestionKind,
}

pub struct Ranker {
    weights: WeightsConfig,
}

impl Ranker {
    pub fn new(weights: WeightsConfig) -> Self {
        Self {
            weights: weights.normalized(),
        }
    }

    pub fn rank(
        &self,
        suggestions: Vec<ProviderSuggestion>,
        recent_commands: &[String],
    ) -> Option<RankedSuggestion> {
        if suggestions.is_empty() {
            return None;
        }

        let mut best: Option<RankedSuggestion> = None;

        for s in suggestions {
            let weight = self.weight_for(s.source);
            let recency_bonus = compute_recency_bonus(&s.text, recent_commands);
            let score = weight * s.score + self.weights.recency * recency_bonus;

            if best.as_ref().map_or(true, |b| score > b.score) {
                best = Some(RankedSuggestion {
                    text: s.text,
                    source: s.source,
                    score,
                    description: s.description,
                    kind: s.kind,
                });
            }
        }

        best
    }

    /// Rank multiple suggestions, deduplicate by text, return top N.
    pub fn rank_multi(
        &self,
        suggestions: Vec<ProviderSuggestion>,
        recent_commands: &[String],
        max: usize,
    ) -> Vec<RankedSuggestion> {
        if suggestions.is_empty() {
            return Vec::new();
        }

        // Score each suggestion
        let scored: Vec<RankedSuggestion> = suggestions
            .into_iter()
            .map(|s| {
                let weight = self.weight_for(s.source);
                let recency_bonus = compute_recency_bonus(&s.text, recent_commands);
                let score = weight * s.score + self.weights.recency * recency_bonus;
                RankedSuggestion {
                    text: s.text,
                    source: s.source,
                    score,
                    description: s.description,
                    kind: s.kind,
                }
            })
            .collect();

        // Deduplicate by text — keep highest score
        let mut deduped: HashMap<String, RankedSuggestion> = HashMap::new();
        for s in scored {
            let existing = deduped.get(&s.text);
            if existing.map_or(true, |e| s.score > e.score) {
                deduped.insert(s.text.clone(), s);
            }
        }

        let mut results: Vec<RankedSuggestion> = deduped.into_values().collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(max);
        results
    }

    fn weight_for(&self, source: SuggestionSource) -> f64 {
        match source {
            SuggestionSource::History => self.weights.history,
            SuggestionSource::Context => self.weights.context,
            SuggestionSource::Ai => self.weights.ai,
            SuggestionSource::Spec => self.weights.spec,
        }
    }
}

impl RankedSuggestion {
    pub fn to_suggestion_item(&self) -> SuggestionItem {
        SuggestionItem {
            text: self.text.clone(),
            source: self.source,
            confidence: self.score.min(1.0),
            description: self.description.clone(),
            kind: self.kind,
        }
    }
}

/// Recency bonus: exponential decay based on position in recent_commands
fn compute_recency_bonus(suggestion: &str, recent_commands: &[String]) -> f64 {
    for (i, cmd) in recent_commands.iter().enumerate() {
        if suggestion.starts_with(cmd.as_str()) || cmd.starts_with(suggestion) {
            return (-0.1 * i as f64).exp();
        }
    }
    0.0
}
