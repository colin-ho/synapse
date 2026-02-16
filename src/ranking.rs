use std::collections::HashMap;
use std::num::NonZeroUsize;

use crate::completion_context::{CompletionContext, ExpectedType, Position};
use crate::config::WeightsConfig;
use crate::protocol::{SuggestionItem, SuggestionKind, SuggestionSource};
use crate::providers::ProviderSuggestion;

/// Position-dependent weights: [spec, filesystem, history, environment, workflow, llm, recency]
type Weights = [f64; 7];

fn weight_for_source(w: &Weights, source: SuggestionSource) -> f64 {
    match source {
        SuggestionSource::Spec => w[0],
        SuggestionSource::Filesystem => w[1],
        SuggestionSource::History => w[2],
        SuggestionSource::Environment => w[3],
        SuggestionSource::Workflow => w[4],
        SuggestionSource::Llm => w[5],
    }
}

fn weights_for_position(ctx: &CompletionContext) -> Weights {
    //                                           spec   fs    hist   env   flow   llm  recency
    match &ctx.position {
        Position::CommandName => [0.25, 0.00, 0.20, 0.05, 0.30, 0.00, 0.20],
        Position::Subcommand => [0.55, 0.00, 0.20, 0.00, 0.00, 0.00, 0.25],
        Position::OptionFlag => [0.60, 0.00, 0.10, 0.00, 0.00, 0.00, 0.30],
        Position::OptionValue { .. } => match &ctx.expected_type {
            ExpectedType::Generator(_) => [0.40, 0.20, 0.20, 0.00, 0.00, 0.10, 0.20],
            ExpectedType::Any => [0.40, 0.20, 0.20, 0.00, 0.00, 0.55, 0.20],
            _ => [0.40, 0.20, 0.20, 0.00, 0.00, 0.00, 0.20],
        },
        Position::Argument { .. } => match &ctx.expected_type {
            ExpectedType::FilePath | ExpectedType::Directory => {
                [0.10, 0.50, 0.15, 0.00, 0.00, 0.00, 0.25]
            }
            ExpectedType::Generator(_) => [0.45, 0.00, 0.25, 0.00, 0.00, 0.15, 0.30],
            ExpectedType::Any => [0.35, 0.00, 0.30, 0.00, 0.00, 0.60, 0.35],
            _ => [0.35, 0.00, 0.30, 0.00, 0.00, 0.00, 0.35],
        },
        Position::PipeTarget => [0.00, 0.00, 0.40, 0.25, 0.00, 0.00, 0.35],
        Position::Redirect => [0.00, 0.60, 0.10, 0.00, 0.00, 0.00, 0.30],
        Position::Unknown => [0.25, 0.00, 0.35, 0.00, 0.00, 0.00, 0.40],
    }
}

#[derive(Debug, Clone)]
pub struct RankedSuggestion {
    pub text: String,
    pub source: SuggestionSource,
    pub score: f64,
    pub description: Option<String>,
    pub kind: SuggestionKind,
}

#[derive(Clone)]
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
        ctx: Option<&CompletionContext>,
    ) -> Option<RankedSuggestion> {
        if suggestions.is_empty() {
            return None;
        }

        let mut best: Option<RankedSuggestion> = None;

        for s in suggestions {
            let (weight, recency_weight) = self.resolve_weight(s.source, ctx);
            let recency_bonus = compute_recency_bonus(&s.text, recent_commands);
            let score = weight * s.score + recency_weight * recency_bonus;

            if best.as_ref().is_none_or(|b| score > b.score) {
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
        max: NonZeroUsize,
        ctx: Option<&CompletionContext>,
    ) -> Vec<RankedSuggestion> {
        if suggestions.is_empty() {
            return Vec::new();
        }

        // Score each suggestion
        let scored: Vec<RankedSuggestion> = suggestions
            .into_iter()
            .map(|s| {
                let (weight, recency_weight) = self.resolve_weight(s.source, ctx);
                let recency_bonus = compute_recency_bonus(&s.text, recent_commands);
                let score = weight * s.score + recency_weight * recency_bonus;
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
            if existing.is_none_or(|e| s.score > e.score) {
                deduped.insert(s.text.clone(), s);
            }
        }

        let mut results: Vec<RankedSuggestion> = deduped.into_values().collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(max.get());
        results
    }

    /// Returns (source_weight, recency_weight) based on context or static config.
    fn resolve_weight(
        &self,
        source: SuggestionSource,
        ctx: Option<&CompletionContext>,
    ) -> (f64, f64) {
        if let Some(ctx) = ctx {
            let w = weights_for_position(ctx);
            (weight_for_source(&w, source), w[6])
        } else {
            (self.static_weight_for(source), self.weights.recency)
        }
    }

    fn static_weight_for(&self, source: SuggestionSource) -> f64 {
        match source {
            SuggestionSource::History => self.weights.history,
            SuggestionSource::Spec => self.weights.spec,
            SuggestionSource::Filesystem => self.weights.spec,
            SuggestionSource::Environment => self.weights.spec,
            SuggestionSource::Workflow => self.weights.history,
            SuggestionSource::Llm => self.weights.spec,
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

/// Recency bonus: exponential decay based on position in recent_commands.
/// Uses decay factor 0.3 so only the last 3-5 commands are significant.
fn compute_recency_bonus(suggestion: &str, recent_commands: &[String]) -> f64 {
    for (i, cmd) in recent_commands.iter().enumerate() {
        if suggestion.starts_with(cmd.as_str()) || cmd.starts_with(suggestion) {
            return (-0.3 * i as f64).exp();
        }
    }
    0.0
}
