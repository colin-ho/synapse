use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::protocol::{SuggestRequest, SuggestionKind, SuggestionSource};
use crate::providers::{ProviderSuggestion, SuggestionProvider};
use crate::spec::{ArgSpec, ArgTemplate, OptionSpec, SpecSource};
use crate::spec_store::SpecStore;

pub struct SpecProvider {
    store: Arc<SpecStore>,
}

impl SpecProvider {
    pub fn new(store: Arc<SpecStore>) -> Self {
        Self { store }
    }

    /// Generate completions based on spec tree-walk.
    /// Returns multiple suggestions sorted by relevance.
    async fn complete(&self, buffer: &str, cwd: &Path) -> Vec<ProviderSuggestion> {
        if buffer.is_empty() {
            return Vec::new();
        }

        let tokens = tokenize(buffer);
        if tokens.is_empty() {
            return Vec::new();
        }

        let trailing_space = buffer.ends_with(' ');
        let command_name = &tokens[0];

        // Look up the root spec
        let spec = match self.store.lookup(command_name, cwd).await {
            Some(s) => s,
            None => {
                // If partial first token, try to complete the command name itself
                if tokens.len() == 1 && !trailing_space {
                    return self.complete_command_name(command_name, cwd).await;
                }
                return Vec::new();
            }
        };

        // If we only have the command name and no trailing space, the command is being typed
        if tokens.len() == 1 && !trailing_space {
            // Check if it's an exact match — if so, don't suggest the command itself
            if command_name != &spec.name && !spec.aliases.iter().any(|a| a == command_name) {
                return self.complete_command_name(command_name, cwd).await;
            }
            return Vec::new();
        }

        // Walk the spec tree consuming complete tokens
        let mut current_subcommands = &spec.subcommands;
        let mut current_options = &spec.options;
        let mut current_args = &spec.args;
        let remaining_tokens = &tokens[1..];

        let mut _consumed = 0;
        let mut skip_next = false;

        for (i, token) in remaining_tokens.iter().enumerate() {
            if skip_next {
                skip_next = false;
                _consumed = i + 1;
                continue;
            }

            // Check if this token is a complete subcommand match
            let sub_match = current_subcommands
                .iter()
                .find(|s| s.name == *token || s.aliases.iter().any(|a| a == token));

            if let Some(sub) = sub_match {
                // If this is the last token and there's no trailing space, it's being typed
                if i == remaining_tokens.len() - 1 && !trailing_space {
                    break;
                }
                current_subcommands = &sub.subcommands;
                current_options = &sub.options;
                current_args = &sub.args;
                _consumed = i + 1;
            } else if token.starts_with('-') {
                // Check if this option takes an arg — if so, skip the next token
                if let Some(opt) = find_option(current_options, token) {
                    if opt.takes_arg {
                        skip_next = true;
                    }
                }
                _consumed = i + 1;
            } else {
                // Positional arg or partial token
                _consumed = i;
                break;
            }
        }

        // Determine what to complete.
        // partial is always the last token being typed (when no trailing space).
        // prefix is all complete tokens (everything before the partial).
        let partial = if !trailing_space && !remaining_tokens.is_empty() {
            remaining_tokens.last().map(|s| s.as_str()).unwrap_or("")
        } else {
            ""
        };

        let prefix = if !trailing_space && tokens.len() > 1 {
            format!("{} ", tokens[..tokens.len() - 1].join(" "))
        } else if trailing_space {
            format!("{} ", tokens.join(" "))
        } else {
            // Single token being completed — no prefix
            String::new()
        };

        let mut suggestions = Vec::new();

        // Complete subcommands
        for sub in current_subcommands {
            if sub.name.starts_with(partial) {
                let text = format!("{}{}", prefix, sub.name);
                let similarity = if partial.is_empty() {
                    0.7
                } else {
                    0.7 + 0.3 * (partial.len() as f64 / sub.name.len() as f64)
                };
                suggestions.push(ProviderSuggestion {
                    text,
                    source: SuggestionSource::Spec,
                    score: similarity,
                    description: sub.description.clone(),
                    kind: SuggestionKind::Subcommand,
                });
            }
            // Also check aliases
            for alias in &sub.aliases {
                if alias.starts_with(partial) && alias != &sub.name {
                    let text = format!("{}{}", prefix, alias);
                    suggestions.push(ProviderSuggestion {
                        text,
                        source: SuggestionSource::Spec,
                        score: 0.65,
                        description: sub.description.clone(),
                        kind: SuggestionKind::Subcommand,
                    });
                }
            }
        }

        // Complete options (when partial starts with '-' or we're completing after a space)
        if partial.starts_with('-') || (partial.is_empty() && suggestions.is_empty()) {
            for opt in current_options {
                if let Some(long) = &opt.long {
                    if long.starts_with(partial) {
                        let text = format!("{}{}", prefix, long);
                        let similarity = if partial.is_empty() {
                            0.5
                        } else {
                            0.5 + 0.3 * (partial.len() as f64 / long.len() as f64)
                        };
                        suggestions.push(ProviderSuggestion {
                            text,
                            source: SuggestionSource::Spec,
                            score: similarity,
                            description: opt.description.clone(),
                            kind: SuggestionKind::Option,
                        });
                    }
                }
                if let Some(short) = &opt.short {
                    if short.starts_with(partial) && partial.len() <= 2 {
                        let text = format!("{}{}", prefix, short);
                        suggestions.push(ProviderSuggestion {
                            text,
                            source: SuggestionSource::Spec,
                            score: 0.55,
                            description: opt.description.clone(),
                            kind: SuggestionKind::Option,
                        });
                    }
                }
            }
        }

        // Complete arguments (generators, templates, static suggestions)
        if !partial.starts_with('-') {
            for arg in current_args {
                let mut arg_suggestions = self
                    .resolve_arg_completions(arg, partial, cwd, spec.source)
                    .await;
                for s in &mut arg_suggestions {
                    s.text = format!("{}{}", prefix, s.text);
                }
                suggestions.extend(arg_suggestions);
            }
        }

        suggestions
    }

    async fn complete_command_name(&self, partial: &str, cwd: &Path) -> Vec<ProviderSuggestion> {
        let all_names = self.store.all_command_names(cwd).await;
        all_names
            .into_iter()
            .filter(|name| name.starts_with(partial) && name != partial)
            .map(|name| {
                let similarity = partial.len() as f64 / name.len() as f64;
                ProviderSuggestion {
                    text: name,
                    source: SuggestionSource::Spec,
                    score: 0.6 + 0.3 * similarity,
                    description: None,
                    kind: SuggestionKind::Command,
                }
            })
            .collect()
    }

    async fn resolve_arg_completions(
        &self,
        arg: &ArgSpec,
        partial: &str,
        cwd: &Path,
        source: SpecSource,
    ) -> Vec<ProviderSuggestion> {
        let mut results = Vec::new();

        // Static suggestions
        for suggestion in &arg.suggestions {
            if suggestion.starts_with(partial) {
                results.push(ProviderSuggestion {
                    text: suggestion.clone(),
                    source: SuggestionSource::Spec,
                    score: 0.7,
                    description: arg.description.clone(),
                    kind: SuggestionKind::Argument,
                });
            }
        }

        // Generator
        if let Some(generator) = &arg.generator {
            let gen_results = self.store.run_generator(generator, cwd, source).await;
            for item in gen_results {
                if item.starts_with(partial) {
                    let similarity = if partial.is_empty() {
                        0.65
                    } else {
                        0.65 + 0.25 * (partial.len() as f64 / item.len().max(1) as f64)
                    };
                    results.push(ProviderSuggestion {
                        text: item,
                        source: SuggestionSource::Spec,
                        score: similarity,
                        description: arg.description.clone(),
                        kind: SuggestionKind::Argument,
                    });
                }
            }
        }

        // Template
        if let Some(template) = &arg.template {
            match template {
                ArgTemplate::FilePaths | ArgTemplate::Directories => {
                    // File/dir completion is better handled by the shell itself
                    // Just add the template kind info
                    if results.is_empty() && partial.is_empty() {
                        results.push(ProviderSuggestion {
                            text: String::new(),
                            source: SuggestionSource::Spec,
                            score: 0.3,
                            description: Some(match template {
                                ArgTemplate::FilePaths => "file path".into(),
                                ArgTemplate::Directories => "directory".into(),
                                _ => unreachable!(),
                            }),
                            kind: SuggestionKind::File,
                        });
                    }
                }
                ArgTemplate::EnvVars | ArgTemplate::History => {
                    // These are handled by other providers
                }
            }
        }

        results
    }
}

#[async_trait]
impl SuggestionProvider for SpecProvider {
    async fn suggest(&self, request: &SuggestRequest) -> Option<ProviderSuggestion> {
        let cwd = Path::new(&request.cwd);
        let mut completions = self.complete(&request.buffer, cwd).await;

        // Filter out empty text entries and entries matching the buffer exactly
        completions.retain(|s| !s.text.is_empty() && s.text != request.buffer);

        // Return the highest scoring completion
        completions.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        completions.into_iter().next()
    }

    fn source(&self) -> SuggestionSource {
        SuggestionSource::Spec
    }

    fn is_available(&self) -> bool {
        true
    }

    async fn suggest_multi(&self, request: &SuggestRequest, max: usize) -> Vec<ProviderSuggestion> {
        let cwd = Path::new(&request.cwd);
        let mut completions = self.complete(&request.buffer, cwd).await;

        // Filter out empty text entries and entries matching the buffer exactly
        completions.retain(|s| !s.text.is_empty() && s.text != request.buffer);

        completions.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        completions.truncate(max);
        completions
    }
}

/// Tokenize a command buffer, respecting quotes.
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' if !in_single_quote => {
                escaped = true;
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn find_option<'a>(options: &'a [OptionSpec], token: &str) -> Option<&'a OptionSpec> {
    options
        .iter()
        .find(|opt| opt.long.as_deref() == Some(token) || opt.short.as_deref() == Some(token))
}
