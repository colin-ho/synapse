use std::collections::HashMap;
use std::num::NonZeroUsize;

use crate::completion_context::{CompletionContext, ExpectedType, Position};
use crate::config::WeightsConfig;
use crate::protocol::{SuggestionItem, SuggestionKind, SuggestionSource};
use crate::providers::ProviderSuggestion;

/// Position-dependent weight configuration.
struct PositionWeights {
    spec: f64,
    filesystem: f64,
    history: f64,
    environment: f64,
    llm: f64,
    recency: f64,
}

impl PositionWeights {
    fn weight_for(&self, source: SuggestionSource) -> f64 {
        match source {
            SuggestionSource::Spec => self.spec,
            SuggestionSource::Filesystem => self.filesystem,
            SuggestionSource::History => self.history,
            SuggestionSource::Environment => self.environment,
            SuggestionSource::Llm => self.llm,
        }
    }
}

fn weights_for_position(ctx: &CompletionContext) -> PositionWeights {
    match &ctx.position {
        Position::CommandName => PositionWeights {
            spec: 0.25,
            filesystem: 0.0,
            history: 0.30,
            environment: 0.20,
            llm: 0.0,
            recency: 0.25,
        },
        Position::Subcommand => PositionWeights {
            spec: 0.55,
            filesystem: 0.0,
            history: 0.20,
            environment: 0.0,
            llm: 0.0,
            recency: 0.25,
        },
        Position::OptionFlag => PositionWeights {
            spec: 0.60,
            filesystem: 0.0,
            history: 0.10,
            environment: 0.0,
            llm: 0.0,
            recency: 0.30,
        },
        Position::OptionValue { .. } => match &ctx.expected_type {
            ExpectedType::Generator(_) => PositionWeights {
                spec: 0.40,
                filesystem: 0.20,
                history: 0.20,
                environment: 0.0,
                llm: 0.10,
                recency: 0.20,
            },
            ExpectedType::Any => PositionWeights {
                spec: 0.40,
                filesystem: 0.20,
                history: 0.20,
                environment: 0.0,
                llm: 0.55,
                recency: 0.20,
            },
            _ => PositionWeights {
                spec: 0.40,
                filesystem: 0.20,
                history: 0.20,
                environment: 0.0,
                llm: 0.0,
                recency: 0.20,
            },
        },
        Position::Argument { .. } => match &ctx.expected_type {
            ExpectedType::FilePath | ExpectedType::Directory => PositionWeights {
                spec: 0.10,
                filesystem: 0.50,
                history: 0.15,
                environment: 0.0,
                llm: 0.0,
                recency: 0.25,
            },
            ExpectedType::Generator(_) => PositionWeights {
                spec: 0.45,
                filesystem: 0.0,
                history: 0.25,
                environment: 0.0,
                llm: 0.15,
                recency: 0.30,
            },
            ExpectedType::Any => PositionWeights {
                spec: 0.35,
                filesystem: 0.0,
                history: 0.30,
                environment: 0.0,
                llm: 0.60,
                recency: 0.35,
            },
            _ => PositionWeights {
                spec: 0.35,
                filesystem: 0.0,
                history: 0.30,
                environment: 0.0,
                llm: 0.0,
                recency: 0.35,
            },
        },
        Position::PipeTarget => PositionWeights {
            spec: 0.0,
            filesystem: 0.0,
            history: 0.40,
            environment: 0.25,
            llm: 0.0,
            recency: 0.35,
        },
        Position::Redirect => PositionWeights {
            spec: 0.0,
            filesystem: 0.60,
            history: 0.10,
            environment: 0.0,
            llm: 0.0,
            recency: 0.30,
        },
        Position::Unknown => PositionWeights {
            spec: 0.25,
            filesystem: 0.0,
            history: 0.35,
            environment: 0.0,
            llm: 0.0,
            recency: 0.40,
        },
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
            let pw = weights_for_position(ctx);
            (pw.weight_for(source), pw.recency)
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
