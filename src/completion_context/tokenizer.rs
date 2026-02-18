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

/// Split a buffer at the last unquoted shell operator (`&&`, `||`, `;`, `|`).
/// Returns `(prefix, segment)` where `prefix` includes the operator and any
/// trailing whitespace, and `segment` is the remaining command to complete.
/// If no operator is found, returns `("", buffer)`.
pub fn split_at_last_operator(buffer: &str) -> (&str, &str) {
    let bytes = buffer.as_bytes();
    let mut last_op_end: usize = 0;
    let mut i: usize = 0;
    let mut in_sq = false;
    let mut in_dq = false;
    let mut escaped = false;

    while i < bytes.len() {
        let b = bytes[i];

        if escaped {
            escaped = false;
            i += 1;
            continue;
        }

        if b == b'\\' && !in_sq {
            escaped = true;
            i += 1;
            continue;
        }
        if b == b'\'' && !in_dq {
            in_sq = !in_sq;
            i += 1;
            continue;
        }
        if b == b'"' && !in_sq {
            in_dq = !in_dq;
            i += 1;
            continue;
        }

        if !in_sq && !in_dq {
            if b == b'&' && i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                last_op_end = i + 2;
                i += 2;
                continue;
            }
            if b == b'|' && i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                last_op_end = i + 2;
                i += 2;
                continue;
            }
            if b == b'|' {
                last_op_end = i + 1;
                i += 1;
                continue;
            }
            if b == b';' {
                last_op_end = i + 1;
                i += 1;
                continue;
            }
        }

        i += 1;
    }

    if last_op_end == 0 {
        return ("", buffer);
    }

    // Include whitespace after the operator in the prefix
    let after = &buffer[last_op_end..];
    let ws_len = after.len() - after.trim_start().len();
    let split_point = last_op_end + ws_len;

    (&buffer[..split_point], &buffer[split_point..])
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
