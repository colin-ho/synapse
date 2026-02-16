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
pub(super) fn last_segment(tokens: &[Token]) -> (Vec<String>, Option<&Token>) {
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
