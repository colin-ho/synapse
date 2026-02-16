use std::path::Path;

use crate::spec::{find_option, ArgTemplate, OptionSpec};
use crate::spec_store::SpecStore;

use super::tokenizer::{last_segment, tokenize, tokenize_with_operators, Token};
use super::{CompletionContext, ExpectedType, Position};

impl CompletionContext {
    /// Build a CompletionContext from a buffer, cwd, and spec store.
    pub async fn build(buffer: &str, cwd: &Path, store: &SpecStore) -> Self {
        if buffer.is_empty() {
            return Self::empty(buffer);
        }

        let all_tokens = tokenize_with_operators(buffer);
        if all_tokens.is_empty() {
            return Self::empty(buffer);
        }

        let trailing_space = buffer.ends_with(' ');
        let buffer_tokens = tokenize(buffer);

        // Check for pipe/redirect by finding the last segment
        let (segment_words, preceding_op) = last_segment(&all_tokens);

        // If after a pipe and no words yet (or one partial word), it's PipeTarget
        if let Some(Token::Pipe) = preceding_op {
            let segment_prefix = Self::prefix_up_to_last_segment(buffer, &all_tokens);
            if segment_words.is_empty() {
                return Self::from_fields(
                    buffer,
                    buffer_tokens.clone(),
                    String::new(),
                    segment_prefix,
                    None,
                    Position::PipeTarget,
                    ExpectedType::Command,
                );
            }
            if segment_words.len() == 1 && !trailing_space {
                return Self::from_fields(
                    buffer,
                    buffer_tokens.clone(),
                    segment_words[0].clone(),
                    segment_prefix,
                    Some(segment_words[0].clone()),
                    Position::PipeTarget,
                    ExpectedType::Command,
                );
            }
            // More than one word after pipe — treat the segment as a new command
            // Build context for just the segment
            let inner = Self::build_segment_words(&segment_words, trailing_space, cwd, store).await;
            return Self::rebase_inner(inner, buffer, buffer_tokens.clone(), &segment_prefix);
        }

        // If after a redirect, position is Redirect with FilePath expected
        if let Some(Token::Redirect(_)) = preceding_op {
            let partial = if !trailing_space {
                segment_words.last().cloned().unwrap_or_default()
            } else {
                String::new()
            };
            return Self::from_fields(
                buffer,
                buffer_tokens.clone(),
                partial,
                Self::prefix_up_to_last_segment(buffer, &all_tokens),
                None,
                Position::Redirect,
                ExpectedType::FilePath,
            );
        }

        // If after && or || or ;, treat as new command
        if matches!(
            preceding_op,
            Some(Token::And) | Some(Token::Or) | Some(Token::Semicolon)
        ) {
            let segment_prefix = Self::prefix_up_to_last_segment(buffer, &all_tokens);
            if segment_words.is_empty() {
                return Self::from_fields(
                    buffer,
                    buffer_tokens.clone(),
                    String::new(),
                    segment_prefix,
                    None,
                    Position::CommandName,
                    ExpectedType::Command,
                );
            }
            let inner = Self::build_segment_words(&segment_words, trailing_space, cwd, store).await;
            return Self::rebase_inner(inner, buffer, buffer_tokens.clone(), &segment_prefix);
        }

        // No operators — use word-only tokens
        let tokens = segment_words;
        if tokens.is_empty() {
            return Self::empty(buffer);
        }

        // Single token, no trailing space → completing the command name
        if tokens.len() == 1 && !trailing_space {
            return Self::from_fields(
                buffer,
                tokens.clone(),
                tokens[0].clone(),
                String::new(),
                Some(tokens[0].clone()),
                Position::CommandName,
                ExpectedType::Command,
            );
        }

        let command_name = &tokens[0];

        // Try spec lookup — handles recursive commands, arg types, and full specs
        if let Some(spec) = store.lookup(command_name, cwd).await {
            if spec.recursive {
                return Self::build_recursive(buffer, &tokens, trailing_space, cwd, store).await;
            }
            return Self::build_from_spec(buffer, &tokens, trailing_space, &spec, store, cwd).await;
        }

        // Fallback: unknown
        let (partial, prefix) = Self::compute_partial_prefix(&tokens, trailing_space);
        Self::from_fields(
            buffer,
            tokens.clone(),
            partial,
            prefix,
            Some(command_name.clone()),
            Position::Unknown,
            ExpectedType::Any,
        )
    }

    /// Build context using a resolved spec (tree-walk).
    async fn build_from_spec(
        buffer: &str,
        tokens: &[String],
        trailing_space: bool,
        spec: &crate::spec::CommandSpec,
        store: &SpecStore,
        cwd: &Path,
    ) -> Self {
        let command_name = &tokens[0];
        let remaining = &tokens[1..];

        let mut current_subcommands = &spec.subcommands;
        let mut current_options = &spec.options;
        let mut current_args = &spec.args;
        let mut subcommand_path = Vec::new();
        let mut present_options = Vec::new();
        let mut skip_next = false;
        let mut last_option: Option<String> = None;
        let mut arg_index: usize = 0;

        for (i, token) in remaining.iter().enumerate() {
            if skip_next {
                let is_last_incomplete = i == remaining.len() - 1 && !trailing_space;
                if is_last_incomplete {
                    // Keep OptionValue state while the user is typing the value token.
                    break;
                }
                skip_next = false;
                last_option = None;
                continue;
            }

            // Is this the last token and no trailing space? Then it's the partial being typed.
            let is_last_incomplete = i == remaining.len() - 1 && !trailing_space;

            // Check for subcommand match
            let sub_match = current_subcommands
                .iter()
                .find(|s| s.name == *token || s.aliases.iter().any(|a| a == token));

            if let Some(sub) = sub_match {
                if is_last_incomplete {
                    // Partial subcommand being typed
                    break;
                }
                subcommand_path.push(sub.name.clone());
                current_subcommands = &sub.subcommands;
                current_options = &sub.options;
                current_args = &sub.args;
                arg_index = 0;
            } else if token.starts_with('-') {
                present_options.push(token.clone());
                if let Some(opt) = find_option(current_options, token) {
                    if opt.takes_arg {
                        if is_last_incomplete {
                            // The option itself is being typed
                            break;
                        }
                        last_option = Some(token.clone());
                        skip_next = true;
                    }
                }
            } else {
                // Positional argument
                if !is_last_incomplete {
                    arg_index += 1;
                }
            }
        }

        let (partial, prefix) = Self::compute_partial_prefix(tokens, trailing_space);

        // Determine position and expected type
        let (position, expected_type) = if partial.starts_with('-') {
            (Position::OptionFlag, ExpectedType::Any)
        } else if skip_next {
            // We're waiting for an option's value
            let opt_name = last_option.unwrap_or_default();
            let expected = Self::option_expected_type(current_options, &opt_name);
            (Position::OptionValue { option: opt_name }, expected)
        } else if !subcommand_path.is_empty() || remaining.is_empty() || trailing_space {
            // Check if we should be completing a subcommand
            if !current_subcommands.is_empty()
                && (partial.is_empty()
                    || current_subcommands
                        .iter()
                        .any(|s| s.name.starts_with(&partial)))
            {
                // Could be subcommand or argument — if there are subcommands matching, prefer subcommand
                if partial.is_empty() && !current_args.is_empty() && current_subcommands.is_empty()
                {
                    let expected = Self::arg_expected_type(current_args, arg_index, store, cwd);
                    (Position::Argument { index: arg_index }, expected)
                } else {
                    (Position::Subcommand, ExpectedType::Any)
                }
            } else if !current_args.is_empty() || arg_index > 0 {
                let expected = Self::arg_expected_type(current_args, arg_index, store, cwd);
                (Position::Argument { index: arg_index }, expected)
            } else if !current_subcommands.is_empty() {
                (Position::Subcommand, ExpectedType::Any)
            } else {
                (Position::Unknown, ExpectedType::Any)
            }
        } else {
            // Check if partial matches any subcommand prefix
            let matches_sub = current_subcommands
                .iter()
                .any(|s| s.name.starts_with(&partial));
            if matches_sub {
                (Position::Subcommand, ExpectedType::Any)
            } else {
                let expected = Self::arg_expected_type(current_args, arg_index, store, cwd);
                (Position::Argument { index: arg_index }, expected)
            }
        };

        Self {
            buffer: buffer.to_string(),
            tokens: tokens.to_vec(),
            trailing_space,
            partial,
            prefix,
            command: Some(command_name.clone()),
            position,
            expected_type,
            subcommand_path,
            present_options,
        }
    }

    fn option_expected_type(options: &[OptionSpec], opt_token: &str) -> ExpectedType {
        if let Some(opt) = find_option(options, opt_token) {
            if let Some(ref gen) = opt.arg_generator {
                return ExpectedType::Generator(gen.command.clone());
            }
        }
        ExpectedType::Any
    }

    fn arg_expected_type(
        args: &[crate::spec::ArgSpec],
        index: usize,
        _store: &SpecStore,
        _cwd: &Path,
    ) -> ExpectedType {
        let arg = if index < args.len() {
            &args[index]
        } else if let Some(last) = args.last() {
            // Variadic: reuse the last arg spec
            last
        } else {
            return ExpectedType::Any;
        };

        if let Some(ref gen) = arg.generator {
            return ExpectedType::Generator(gen.command.clone());
        }
        if let Some(ref template) = arg.template {
            return match template {
                ArgTemplate::FilePaths => ExpectedType::FilePath,
                ArgTemplate::Directories => ExpectedType::Directory,
                ArgTemplate::EnvVars => ExpectedType::EnvVar,
                ArgTemplate::History => ExpectedType::Any,
            };
        }
        if !arg.suggestions.is_empty() {
            return ExpectedType::OneOf(arg.suggestions.clone());
        }
        ExpectedType::Any
    }

    fn from_fields(
        buffer: &str,
        tokens: Vec<String>,
        partial: String,
        prefix: String,
        command: Option<String>,
        position: Position,
        expected_type: ExpectedType,
    ) -> Self {
        Self {
            buffer: buffer.to_string(),
            tokens,
            trailing_space: buffer.ends_with(' '),
            partial,
            prefix,
            command,
            position,
            expected_type,
            subcommand_path: Vec::new(),
            present_options: Vec::new(),
        }
    }

    /// Handle recursive commands (sudo, env, etc.) that take another command as an argument.
    async fn build_recursive(
        buffer: &str,
        tokens: &[String],
        trailing_space: bool,
        cwd: &Path,
        store: &SpecStore,
    ) -> Self {
        let rest_tokens = &tokens[1..];
        // Find the actual command (skip flags for env/sudo)
        let mut cmd_start = 0;
        for (i, tok) in rest_tokens.iter().enumerate() {
            if !tok.starts_with('-') {
                cmd_start = i;
                break;
            }
            cmd_start = i + 1;
        }

        if cmd_start < rest_tokens.len() {
            let prefix_parts: Vec<&str> =
                tokens[..1 + cmd_start].iter().map(|s| s.as_str()).collect();
            let prefix_str = format!("{} ", prefix_parts.join(" "));
            let inner_tokens = &rest_tokens[cmd_start..];

            if inner_tokens.len() == 1 && !trailing_space {
                return Self::from_fields(
                    buffer,
                    tokens.to_vec(),
                    inner_tokens[0].clone(),
                    prefix_str,
                    Some(inner_tokens[0].clone()),
                    Position::CommandName,
                    ExpectedType::Command,
                );
            }

            let inner_ctx =
                Self::build_segment_words(inner_tokens, trailing_space, cwd, store).await;
            Self::rebase_inner(inner_ctx, buffer, tokens.to_vec(), &prefix_str)
        } else {
            Self::from_fields(
                buffer,
                tokens.to_vec(),
                String::new(),
                format!("{} ", tokens.join(" ")),
                None,
                Position::CommandName,
                ExpectedType::Command,
            )
        }
    }

    async fn build_segment_words(
        segment_words: &[String],
        trailing_space: bool,
        cwd: &Path,
        store: &SpecStore,
    ) -> Self {
        let mut segment_buffer = segment_words.join(" ");
        if trailing_space {
            segment_buffer.push(' ');
        }
        Box::pin(Self::build(&segment_buffer, cwd, store)).await
    }

    fn rebase_inner(mut inner: Self, buffer: &str, tokens: Vec<String>, prefix: &str) -> Self {
        inner.prefix = format!("{prefix}{}", inner.prefix);
        inner.buffer = buffer.to_string();
        inner.tokens = tokens;
        inner
    }

    fn compute_partial_prefix(tokens: &[String], trailing_space: bool) -> (String, String) {
        let partial = if !trailing_space && !tokens.is_empty() {
            tokens.last().cloned().unwrap_or_default()
        } else {
            String::new()
        };

        let prefix = if !trailing_space && tokens.len() > 1 {
            format!("{} ", tokens[..tokens.len() - 1].join(" "))
        } else if trailing_space {
            format!("{} ", tokens.join(" "))
        } else {
            String::new()
        };

        (partial, prefix)
    }

    /// Compute the buffer prefix up to (and including) the last operator token.
    fn prefix_up_to_last_segment(buffer: &str, all_tokens: &[Token]) -> String {
        // Find the position in the buffer just after the last operator
        let mut last_op_end = 0;
        let mut pos = 0;

        for tok in all_tokens {
            // Skip whitespace
            while pos < buffer.len() && buffer[pos..].starts_with([' ', '\t']) {
                pos += 1;
            }

            match tok {
                Token::Word(w) => {
                    // Advance past the word (handling quotes)
                    let start = pos;
                    let remaining = &buffer[start..];
                    // Find how far this word extends in the original buffer
                    let mut wpos = 0;
                    let mut in_sq = false;
                    let mut in_dq = false;
                    let mut esc = false;
                    let rchars: Vec<char> = remaining.chars().collect();
                    while wpos < rchars.len() {
                        let c = rchars[wpos];
                        if esc {
                            esc = false;
                            wpos += 1;
                            continue;
                        }
                        if c == '\\' && !in_sq {
                            esc = true;
                            wpos += 1;
                            continue;
                        }
                        if c == '\'' && !in_dq {
                            in_sq = !in_sq;
                            wpos += 1;
                            continue;
                        }
                        if c == '"' && !in_sq {
                            in_dq = !in_dq;
                            wpos += 1;
                            continue;
                        }
                        if !in_sq
                            && !in_dq
                            && (c == ' '
                                || c == '\t'
                                || c == '|'
                                || c == '&'
                                || c == ';'
                                || c == '>'
                                || c == '<')
                        {
                            break;
                        }
                        wpos += 1;
                    }
                    pos = start + rchars[..wpos].iter().collect::<String>().len();
                    let _ = w;
                }
                Token::Pipe => {
                    pos += 1; // '|'
                    last_op_end = pos;
                }
                Token::Or => {
                    pos += 2; // '||'
                    last_op_end = pos;
                }
                Token::And => {
                    pos += 2; // '&&'
                    last_op_end = pos;
                }
                Token::Semicolon => {
                    pos += 1; // ';' or '&'
                    last_op_end = pos;
                }
                Token::Redirect(r) => {
                    pos += r.len();
                    last_op_end = pos;
                }
            }
        }

        // Include any whitespace after the last operator
        let after = &buffer[last_op_end..];
        let ws_len = after.len() - after.trim_start().len();
        buffer[..last_op_end + ws_len].to_string()
    }

    fn empty(buffer: &str) -> Self {
        Self::from_fields(
            buffer,
            Vec::new(),
            String::new(),
            String::new(),
            None,
            Position::CommandName,
            ExpectedType::Command,
        )
    }
}
