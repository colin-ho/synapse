use std::path::Path;

use crate::spec::{ArgTemplate, OptionSpec};
use crate::spec_store::SpecStore;

/// Where we are in the command being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    /// Completing the command name itself (`gi▏` → `git`)
    CommandName,
    /// Completing a subcommand (`git ch▏` → `checkout`)
    Subcommand,
    /// Completing an option flag (`git commit --am▏` → `--amend`)
    OptionFlag,
    /// Completing the value for a specific option (`git checkout -b ▏`)
    OptionValue { option: String },
    /// Completing a positional argument (`cd ▏`)
    Argument { index: usize },
    /// Completing a command after a pipe (`cat foo | ▏`)
    PipeTarget,
    /// Completing a file path after a redirect (`echo hello > ▏`)
    Redirect,
    /// Cannot determine position
    Unknown,
}

/// What kind of value is expected at the current position.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ExpectedType {
    Any,
    FilePath,
    Directory,
    Executable,
    Generator(String),
    OneOf(Vec<String>),
    Hostname,
    EnvVar,
    Command,
}

/// Parsed understanding of the current command buffer.
#[derive(Debug, Clone)]
pub struct CompletionContext {
    /// The raw buffer string
    pub buffer: String,
    /// Tokenized buffer (words only, no operators)
    pub tokens: Vec<String>,
    /// Whether the buffer ends with a space
    #[allow(dead_code)]
    pub trailing_space: bool,
    /// The partial word being typed (last token if no trailing space, empty if trailing space)
    pub partial: String,
    /// Everything before the partial — prepend to completions
    pub prefix: String,
    /// The command name (first token of the last segment)
    pub command: Option<String>,
    /// Where we are in the command
    pub position: Position,
    /// What type of value is expected
    pub expected_type: ExpectedType,
    /// The subcommand path walked (e.g. ["checkout"] for `git checkout`)
    pub subcommand_path: Vec<String>,
    /// Options already present in the buffer
    #[allow(dead_code)]
    pub present_options: Vec<String>,
}

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

/// A token that distinguishes words from shell operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(String),
    Pipe,
    Redirect(String), // ">", ">>", "<"
    And,              // "&&"
    Or,               // "||"
    Semicolon,        // ";"
}

/// Tokenize with awareness of shell operators.
/// Returns a list of tokens including operators. Quote-aware.
pub fn tokenize_with_operators(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        if escaped {
            current.push(ch);
            escaped = false;
            i += 1;
            continue;
        }

        if ch == '\\' && !in_single_quote {
            escaped = true;
            i += 1;
            continue;
        }
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }

        if in_single_quote || in_double_quote {
            current.push(ch);
            i += 1;
            continue;
        }

        match ch {
            ' ' | '\t' => {
                if !current.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut current)));
                }
                i += 1;
            }
            '|' => {
                if !current.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut current)));
                }
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    tokens.push(Token::Or);
                    i += 2;
                } else {
                    tokens.push(Token::Pipe);
                    i += 1;
                }
            }
            '&' => {
                if !current.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut current)));
                }
                if i + 1 < chars.len() && chars[i + 1] == '&' {
                    tokens.push(Token::And);
                    i += 2;
                } else {
                    // Background '&' — treat as separator
                    tokens.push(Token::Semicolon);
                    i += 1;
                }
            }
            ';' => {
                if !current.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut current)));
                }
                tokens.push(Token::Semicolon);
                i += 1;
            }
            '>' => {
                if !current.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut current)));
                }
                if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token::Redirect(">>".into()));
                    i += 2;
                } else {
                    tokens.push(Token::Redirect(">".into()));
                    i += 1;
                }
            }
            '<' => {
                if !current.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut current)));
                }
                tokens.push(Token::Redirect("<".into()));
                i += 1;
            }
            _ => {
                current.push(ch);
                i += 1;
            }
        }
    }

    if !current.is_empty() {
        tokens.push(Token::Word(current));
    }

    tokens
}

/// Find the last command segment (after the last pipe/and/or/semicolon).
/// Returns (words_in_last_segment, preceding_operator).
fn last_segment(tokens: &[Token]) -> (Vec<String>, Option<&Token>) {
    let mut last_op_index = None;
    for (i, tok) in tokens.iter().enumerate() {
        match tok {
            Token::Pipe | Token::And | Token::Or | Token::Semicolon | Token::Redirect(_) => {
                last_op_index = Some(i);
            }
            Token::Word(_) => {}
        }
    }

    match last_op_index {
        Some(idx) => {
            let words: Vec<String> = tokens[idx + 1..]
                .iter()
                .filter_map(|t| match t {
                    Token::Word(w) => Some(w.clone()),
                    _ => None,
                })
                .collect();
            (words, Some(&tokens[idx]))
        }
        None => {
            let words: Vec<String> = tokens
                .iter()
                .filter_map(|t| match t {
                    Token::Word(w) => Some(w.clone()),
                    _ => None,
                })
                .collect();
            (words, None)
        }
    }
}

/// Tokenize a command buffer, respecting quotes. Returns words only (no operators).
pub fn tokenize(input: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{CompletionContext, ExpectedType, Position};
    use crate::config::SpecConfig;
    use crate::spec_store::SpecStore;

    fn make_store() -> Arc<SpecStore> {
        Arc::new(SpecStore::new(SpecConfig::default()))
    }

    #[tokio::test]
    async fn test_empty_buffer() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::CommandName);
        assert!(ctx.tokens.is_empty());
    }

    #[tokio::test]
    async fn test_command_name_partial() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("gi", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::CommandName);
        assert_eq!(ctx.partial, "gi");
        assert_eq!(ctx.prefix, "");
        assert_eq!(ctx.command, Some("gi".into()));
    }

    #[tokio::test]
    async fn test_git_subcommand() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git ch", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Subcommand);
        assert_eq!(ctx.partial, "ch");
        assert_eq!(ctx.prefix, "git ");
        assert_eq!(ctx.command, Some("git".into()));
    }

    #[tokio::test]
    async fn test_git_subcommand_trailing_space() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Subcommand);
        assert_eq!(ctx.partial, "");
        assert_eq!(ctx.prefix, "git ");
    }

    #[tokio::test]
    async fn test_git_checkout_argument() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git checkout ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        assert_eq!(ctx.subcommand_path, vec!["checkout"]);
        assert_eq!(ctx.command, Some("git".into()));
        // git checkout has a generator for branches
        assert!(matches!(ctx.expected_type, ExpectedType::Generator(_)));
    }

    #[tokio::test]
    async fn test_git_commit_option() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git commit --am", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::OptionFlag);
        assert_eq!(ctx.partial, "--am");
        assert_eq!(ctx.subcommand_path, vec!["commit"]);
    }

    #[tokio::test]
    async fn test_option_value_while_typing() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git checkout -b fe", dir.path(), &store).await;
        assert_eq!(
            ctx.position,
            Position::OptionValue {
                option: "-b".into()
            }
        );
        assert_eq!(ctx.partial, "fe");
        assert_eq!(ctx.prefix, "git checkout -b ");
    }

    #[tokio::test]
    async fn test_cd_directory() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("cd ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        assert_eq!(ctx.expected_type, ExpectedType::Directory);
        assert_eq!(ctx.command, Some("cd".into()));
    }

    #[tokio::test]
    async fn test_cat_filepath() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("cat ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        assert_eq!(ctx.expected_type, ExpectedType::FilePath);
    }

    #[tokio::test]
    async fn test_ssh_argument() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("ssh ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        // ssh has a builtin spec now, so expected type comes from the spec (Any)
        assert_eq!(ctx.expected_type, ExpectedType::Any);
    }

    #[tokio::test]
    async fn test_unknown_command() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("unknown_cmd ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Unknown);
        assert_eq!(ctx.expected_type, ExpectedType::Any);
    }

    #[tokio::test]
    async fn test_sudo_recursive() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("sudo git ch", dir.path(), &store).await;
        // Should resolve to git subcommand completion
        assert_eq!(ctx.position, Position::Subcommand);
        assert_eq!(ctx.partial, "ch");
        assert_eq!(ctx.command, Some("git".into()));
        // Prefix should include "sudo "
        assert!(ctx.prefix.starts_with("sudo "));
    }

    #[tokio::test]
    async fn test_sudo_command_name() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("sudo gi", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::CommandName);
        assert_eq!(ctx.partial, "gi");
    }

    #[tokio::test]
    async fn test_present_options_tracked() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git commit --verbose --am", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::OptionFlag);
        assert!(ctx.present_options.contains(&"--verbose".to_string()));
    }

    #[tokio::test]
    async fn test_cargo_subcommand() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("cargo b", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Subcommand);
        assert_eq!(ctx.partial, "b");
        assert_eq!(ctx.command, Some("cargo".into()));
    }

    #[tokio::test]
    async fn test_export_envvar() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("export ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        assert_eq!(ctx.expected_type, ExpectedType::EnvVar);
    }
}
