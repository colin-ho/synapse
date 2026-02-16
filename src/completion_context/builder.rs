use std::path::Path;

use crate::spec::{ArgTemplate, OptionSpec};
use crate::spec_store::SpecStore;

use super::tokenizer::{last_segment, tokenize, tokenize_with_operators, Token};
use super::{CompletionContext, ExpectedType, Position};

/// Hardcoded argument types for common commands without specs.
fn command_arg_type(cmd: &str) -> Option<ExpectedType> {
    match cmd {
        "cd" | "mkdir" | "rmdir" | "pushd" => Some(ExpectedType::Directory),
        "cat" | "less" | "head" | "tail" | "vim" | "nvim" | "code" | "nano" | "bat" | "wc"
        | "sort" | "uniq" | "file" | "stat" | "touch" | "open" => Some(ExpectedType::FilePath),
        "cp" | "mv" | "rm" | "chmod" | "chown" | "ln" => Some(ExpectedType::FilePath),
        "python" | "python3" | "node" | "ruby" | "perl" | "bash" | "sh" | "zsh" => {
            Some(ExpectedType::FilePath)
        }
        "ssh" | "scp" | "sftp" | "ping" => Some(ExpectedType::Hostname),
        "export" | "unset" => Some(ExpectedType::EnvVar),
        _ => None,
    }
}

/// Commands that take another command as their first argument (recursive).
fn is_recursive_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "sudo" | "env" | "nohup" | "time" | "watch" | "xargs" | "nice" | "ionice" | "strace"
    )
}

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

        // Check for pipe/redirect by finding the last segment
        let (segment_words, preceding_op) = last_segment(&all_tokens);

        // If after a pipe and no words yet (or one partial word), it's PipeTarget
        if let Some(Token::Pipe) = preceding_op {
            if segment_words.is_empty() {
                return Self {
                    buffer: buffer.to_string(),
                    tokens: tokenize(buffer),
                    trailing_space,
                    partial: String::new(),
                    prefix: Self::prefix_up_to_last_segment(buffer, &all_tokens),
                    command: None,
                    position: Position::PipeTarget,
                    expected_type: ExpectedType::Command,
                    subcommand_path: Vec::new(),
                    present_options: Vec::new(),
                };
            }
            if segment_words.len() == 1 && !trailing_space {
                return Self {
                    buffer: buffer.to_string(),
                    tokens: tokenize(buffer),
                    trailing_space,
                    partial: segment_words[0].clone(),
                    prefix: Self::prefix_up_to_last_segment(buffer, &all_tokens),
                    command: Some(segment_words[0].clone()),
                    position: Position::PipeTarget,
                    expected_type: ExpectedType::Command,
                    subcommand_path: Vec::new(),
                    present_options: Vec::new(),
                };
            }
            // More than one word after pipe — treat the segment as a new command
            // Build context for just the segment
            let segment_buffer = segment_words.join(" ");
            let segment_buffer = if trailing_space {
                format!("{segment_buffer} ")
            } else {
                segment_buffer
            };
            let mut inner = Box::pin(Self::build(&segment_buffer, cwd, store)).await;
            inner.prefix = format!(
                "{}{}",
                Self::prefix_up_to_last_segment(buffer, &all_tokens),
                inner.prefix
            );
            inner.buffer = buffer.to_string();
            inner.tokens = tokenize(buffer);
            return inner;
        }

        // If after a redirect, position is Redirect with FilePath expected
        if let Some(Token::Redirect(_)) = preceding_op {
            let partial = if !trailing_space {
                segment_words.last().cloned().unwrap_or_default()
            } else {
                String::new()
            };
            return Self {
                buffer: buffer.to_string(),
                tokens: tokenize(buffer),
                trailing_space,
                partial,
                prefix: Self::prefix_up_to_last_segment(buffer, &all_tokens),
                command: None,
                position: Position::Redirect,
                expected_type: ExpectedType::FilePath,
                subcommand_path: Vec::new(),
                present_options: Vec::new(),
            };
        }

        // If after && or || or ;, treat as new command
        if matches!(
            preceding_op,
            Some(Token::And) | Some(Token::Or) | Some(Token::Semicolon)
        ) {
            if segment_words.is_empty() {
                return Self {
                    buffer: buffer.to_string(),
                    tokens: tokenize(buffer),
                    trailing_space,
                    partial: String::new(),
                    prefix: Self::prefix_up_to_last_segment(buffer, &all_tokens),
                    command: None,
                    position: Position::CommandName,
                    expected_type: ExpectedType::Command,
                    subcommand_path: Vec::new(),
                    present_options: Vec::new(),
                };
            }
            let segment_buffer = segment_words.join(" ");
            let segment_buffer = if trailing_space {
                format!("{segment_buffer} ")
            } else {
                segment_buffer
            };
            let mut inner = Box::pin(Self::build(&segment_buffer, cwd, store)).await;
            inner.prefix = format!(
                "{}{}",
                Self::prefix_up_to_last_segment(buffer, &all_tokens),
                inner.prefix
            );
            inner.buffer = buffer.to_string();
            inner.tokens = tokenize(buffer);
            return inner;
        }

        // No operators — use word-only tokens
        let tokens = segment_words;
        if tokens.is_empty() {
            return Self::empty(buffer);
        }

        // Single token, no trailing space → completing the command name
        if tokens.len() == 1 && !trailing_space {
            return Self {
                buffer: buffer.to_string(),
                tokens: tokens.clone(),
                trailing_space,
                partial: tokens[0].clone(),
                prefix: String::new(),
                command: Some(tokens[0].clone()),
                position: Position::CommandName,
                expected_type: ExpectedType::Command,
                subcommand_path: Vec::new(),
                present_options: Vec::new(),
            };
        }

        let command_name = &tokens[0];

        // Handle recursive commands (sudo, env, etc.)
        if is_recursive_command(command_name) {
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
                // Reconstruct buffer from the actual command onwards
                let prefix_parts: Vec<&str> =
                    tokens[..1 + cmd_start].iter().map(|s| s.as_str()).collect();
                let prefix_str = format!("{} ", prefix_parts.join(" "));
                let inner_tokens = &rest_tokens[cmd_start..];

                if inner_tokens.len() == 1 && !trailing_space {
                    // Still typing the inner command name
                    return Self {
                        buffer: buffer.to_string(),
                        tokens: tokens.clone(),
                        trailing_space,
                        partial: inner_tokens[0].clone(),
                        prefix: prefix_str,
                        command: Some(inner_tokens[0].clone()),
                        position: Position::CommandName,
                        expected_type: ExpectedType::Command,
                        subcommand_path: Vec::new(),
                        present_options: Vec::new(),
                    };
                }

                // Recurse: build context for the inner command
                let inner_buffer_parts: Vec<&str> =
                    inner_tokens.iter().map(|s| s.as_str()).collect();
                let mut inner_buffer = inner_buffer_parts.join(" ");
                if trailing_space {
                    inner_buffer.push(' ');
                }
                let mut inner_ctx = Box::pin(Self::build(&inner_buffer, cwd, store)).await;
                // Adjust prefix to include the recursive command
                inner_ctx.prefix = format!("{}{}", prefix_str, inner_ctx.prefix);
                inner_ctx.buffer = buffer.to_string();
                inner_ctx.tokens = tokens;
                return inner_ctx;
            } else {
                // After recursive command + its flags, waiting for the inner command
                return Self {
                    buffer: buffer.to_string(),
                    tokens: tokens.clone(),
                    trailing_space,
                    partial: String::new(),
                    prefix: format!("{} ", tokens.join(" ")),
                    command: None,
                    position: Position::CommandName,
                    expected_type: ExpectedType::Command,
                    subcommand_path: Vec::new(),
                    present_options: Vec::new(),
                };
            }
        }

        // Try spec lookup
        if let Some(spec) = store.lookup(command_name, cwd).await {
            return Self::build_from_spec(buffer, &tokens, trailing_space, &spec, store, cwd).await;
        }

        // No spec — check command argument table
        if let Some(expected) = command_arg_type(command_name) {
            let (partial, prefix) = Self::compute_partial_prefix(&tokens, trailing_space);
            return Self {
                buffer: buffer.to_string(),
                tokens: tokens.clone(),
                trailing_space,
                partial,
                prefix,
                command: Some(command_name.clone()),
                position: Position::Argument { index: 0 },
                expected_type: expected,
                subcommand_path: Vec::new(),
                present_options: Vec::new(),
            };
        }

        // Fallback: unknown
        let (partial, prefix) = Self::compute_partial_prefix(&tokens, trailing_space);
        Self {
            buffer: buffer.to_string(),
            tokens: tokens.clone(),
            trailing_space,
            partial,
            prefix,
            command: Some(command_name.clone()),
            position: Position::Unknown,
            expected_type: ExpectedType::Any,
            subcommand_path: Vec::new(),
            present_options: Vec::new(),
        }
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
        Self {
            buffer: buffer.to_string(),
            tokens: Vec::new(),
            trailing_space: false,
            partial: String::new(),
            prefix: String::new(),
            command: None,
            position: Position::CommandName,
            expected_type: ExpectedType::Command,
            subcommand_path: Vec::new(),
            present_options: Vec::new(),
        }
    }
}

fn find_option<'a>(options: &'a [OptionSpec], token: &str) -> Option<&'a OptionSpec> {
    options
        .iter()
        .find(|opt| opt.long.as_deref() == Some(token) || opt.short.as_deref() == Some(token))
}
