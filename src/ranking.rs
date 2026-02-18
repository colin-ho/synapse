use std::collections::HashMap;
use std::num::NonZeroUsize;

use crate::completion_context::{CompletionContext, ExpectedType, Position};
use crate::config::{WEIGHT_HISTORY, WEIGHT_RECENCY, WEIGHT_SPEC};
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
    // All arrays normalized to sum to 1.0.
    //                                           spec   fs    hist   env   flow   llm  recency
    match &ctx.position {
        Position::CommandName => [0.25, 0.00, 0.20, 0.05, 0.30, 0.00, 0.20],
        Position::Subcommand => [0.55, 0.00, 0.20, 0.00, 0.00, 0.00, 0.25],
        Position::OptionFlag => [0.60, 0.00, 0.10, 0.00, 0.00, 0.00, 0.30],
        Position::OptionValue { .. } => match &ctx.expected_type {
            ExpectedType::Generator(_) => [0.36, 0.18, 0.18, 0.00, 0.00, 0.09, 0.18],
            ExpectedType::Any => [0.26, 0.13, 0.13, 0.00, 0.00, 0.35, 0.13],
            _ => [0.40, 0.20, 0.20, 0.00, 0.00, 0.00, 0.20],
        },
        Position::Argument { .. } => match &ctx.expected_type {
            ExpectedType::FilePath | ExpectedType::Directory => {
                [0.10, 0.50, 0.15, 0.00, 0.00, 0.00, 0.25]
            }
            ExpectedType::Generator(_) => [0.39, 0.00, 0.22, 0.00, 0.00, 0.13, 0.26],
            ExpectedType::Any => [0.21, 0.07, 0.18, 0.00, 0.00, 0.36, 0.18],
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

#[derive(Clone, Default)]
pub struct Ranker;

impl Ranker {
    pub fn new() -> Self {
        Self
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
            (Self::static_weight_for(source), WEIGHT_RECENCY)
        }
    }

    fn static_weight_for(source: SuggestionSource) -> f64 {
        match source {
            SuggestionSource::History => WEIGHT_HISTORY,
            SuggestionSource::Spec => WEIGHT_SPEC,
            SuggestionSource::Filesystem => WEIGHT_SPEC,
            SuggestionSource::Environment => WEIGHT_SPEC,
            SuggestionSource::Workflow => WEIGHT_HISTORY,
            SuggestionSource::Llm => WEIGHT_SPEC,
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
    let suggestion_cmd = suggestion.split_whitespace().next().unwrap_or("");
    if suggestion_cmd.is_empty() {
        return 0.0;
    }

    for (i, cmd) in recent_commands.iter().enumerate() {
        // Full forward match: suggestion extends a recent command
        if suggestion.starts_with(cmd.as_str()) {
            return (-0.3 * i as f64).exp();
        }
        // Command-token match: same base command was recently used (half strength)
        let recent_cmd = cmd.split_whitespace().next().unwrap_or("");
        if !recent_cmd.is_empty() && suggestion_cmd == recent_cmd {
            return (-0.3 * i as f64).exp() * 0.5;
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recency_bonus_forward_match() {
        let recent = vec!["git status".to_string()];
        let bonus = compute_recency_bonus("git status --short", &recent);
        assert!(
            (bonus - 1.0).abs() < 0.01,
            "Direct extension gets full bonus"
        );
    }

    #[test]
    fn test_recency_bonus_no_false_positive_for_short_partial() {
        let recent = vec!["git status".to_string()];
        // "gi" should get command-token match (half), not full bonus
        let bonus = compute_recency_bonus("gi", &recent);
        assert!(
            bonus == 0.0,
            "Single-token partial 'gi' != 'git', bonus={bonus}"
        );
    }

    #[test]
    fn test_recency_bonus_command_token_match() {
        let recent = vec!["git status".to_string()];
        let bonus = compute_recency_bonus("git checkout main", &recent);
        assert!(bonus > 0.0, "Same base command should get partial bonus");
        assert!(
            bonus < 0.6,
            "Command-only match should be half strength, got {bonus}"
        );
    }

    #[test]
    fn test_recency_bonus_no_match() {
        let recent = vec!["git status".to_string()];
        let bonus = compute_recency_bonus("cargo build", &recent);
        assert_eq!(bonus, 0.0, "Different command should get no bonus");
    }

    fn test_ctx(position: Position, expected_type: ExpectedType) -> CompletionContext {
        CompletionContext {
            buffer: String::new(),
            tokens: Vec::new(),
            trailing_space: false,
            partial: String::new(),
            prefix: String::new(),
            command: None,
            position,
            expected_type,
            subcommand_path: Vec::new(),
            present_options: Vec::new(),
        }
    }

    #[test]
    fn test_position_weights_sum_to_one() {
        let test_cases: Vec<(CompletionContext, &str)> = vec![
            (
                test_ctx(Position::CommandName, ExpectedType::Any),
                "CommandName",
            ),
            (
                test_ctx(Position::Subcommand, ExpectedType::Any),
                "Subcommand",
            ),
            (
                test_ctx(Position::OptionFlag, ExpectedType::Any),
                "OptionFlag",
            ),
            (
                test_ctx(
                    Position::OptionValue {
                        option: String::new(),
                    },
                    ExpectedType::Any,
                ),
                "OptionValue{Any}",
            ),
            (
                test_ctx(
                    Position::OptionValue {
                        option: String::new(),
                    },
                    ExpectedType::Generator("test".to_string()),
                ),
                "OptionValue{Generator}",
            ),
            (
                test_ctx(Position::Argument { index: 0 }, ExpectedType::FilePath),
                "Argument{FilePath}",
            ),
            (
                test_ctx(
                    Position::Argument { index: 0 },
                    ExpectedType::Generator("test".to_string()),
                ),
                "Argument{Generator}",
            ),
            (
                test_ctx(Position::Argument { index: 0 }, ExpectedType::Any),
                "Argument{Any}",
            ),
            (
                test_ctx(Position::PipeTarget, ExpectedType::Any),
                "PipeTarget",
            ),
            (test_ctx(Position::Redirect, ExpectedType::Any), "Redirect"),
            (test_ctx(Position::Unknown, ExpectedType::Any), "Unknown"),
        ];

        for (ctx, name) in &test_cases {
            let w = weights_for_position(ctx);
            let sum: f64 = w.iter().sum();
            assert!(
                (sum - 1.0).abs() < 0.02,
                "Weights for {name} sum to {sum}, expected ~1.0"
            );
        }
    }
}
