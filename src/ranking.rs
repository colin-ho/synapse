use crate::config::WeightsConfig;
use crate::protocol::SuggestionSource;
use crate::providers::ProviderSuggestion;

#[derive(Debug, Clone)]
pub struct RankedSuggestion {
    pub text: String,
    pub source: SuggestionSource,
    pub score: f64,
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
            let weight = match s.source {
                SuggestionSource::History => self.weights.history,
                SuggestionSource::Context => self.weights.context,
                SuggestionSource::Ai => self.weights.ai,
            };

            let recency_bonus = compute_recency_bonus(&s.text, recent_commands);
            let score = weight * s.score + self.weights.recency * recency_bonus;

            if best.as_ref().map_or(true, |b| score > b.score) {
                best = Some(RankedSuggestion {
                    text: s.text,
                    source: s.source,
                    score,
                });
            }
        }

        best
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
