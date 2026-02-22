/// Check if a command contains potentially destructive operations.
/// Returns a warning description if so.
///
/// Uses simple substring matching — these are user-facing warnings, not security gates
/// (the blocklist handles actual blocking). Simple checks are more robust and catch
/// cases like `sudo rm` that position-anchored regexes would miss.
pub fn detect_destructive_command(command: &str) -> Option<String> {
    let patterns: &[(&str, &str)] = &[
        ("rm ", "deletes files"),
        ("rmdir ", "removes directories"),
        ("dd ", "raw disk write"),
        ("mkfs", "formats filesystem"),
        ("truncate ", "truncates file"),
        ("shred ", "overwrites file data"),
        ("pkill ", "kills processes by name"),
        ("chmod 777", "makes files world-writable"),
        ("chmod -R", "changes permissions recursively"),
        ("kill -9", "force-kills process"),
        ("-delete", "deletes files (find -delete)"),
    ];

    for (pattern, description) in patterns {
        if command.contains(pattern) {
            return Some(description.to_string());
        }
    }

    if let Some(pos) = command.find("> ") {
        if pos == 0 || command.as_bytes()[pos - 1] != b'>' {
            return Some("overwrites file".to_string());
        }
    }

    None
}

/// Extract multiple shell commands from an LLM response.
/// Handles numbered lists, bullets, markdown fences, and bare commands.
pub fn extract_commands(response: &str, max: usize) -> Vec<String> {
    let commands = parse_unique_lines(response, max);

    if commands.is_empty() {
        let single = extract_command(response);
        if single.is_empty() {
            Vec::new()
        } else {
            vec![single]
        }
    } else {
        commands
    }
}

fn extract_fenced_block(text: &str) -> Option<&str> {
    let start = text.find("```")?;
    let after_backticks = start + 3;
    let content_start = if let Some(newline_index) = text[after_backticks..].find('\n') {
        after_backticks + newline_index + 1
    } else {
        after_backticks
    };
    let end = text[content_start..].find("```")?;
    Some(text[content_start..content_start + end].trim())
}

fn extract_command(response: &str) -> String {
    let trimmed = response.trim();

    if let Some(block) = extract_fenced_block(trimmed) {
        return block.to_string();
    }

    for line in trimmed.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') && !line.starts_with("//") {
            return line.to_string();
        }
    }

    trimmed.to_string()
}

fn parse_unique_lines(response: &str, max_values: usize) -> Vec<String> {
    let trimmed = response.trim();
    let content = extract_fenced_block(trimmed).unwrap_or(trimmed);
    let mut values = Vec::new();

    for raw_line in content.lines() {
        let mut line = raw_line.trim();

        if line.is_empty() || line.starts_with("```") {
            continue;
        }

        line = strip_list_marker(line).trim_matches('`').trim();
        if line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if line.is_empty() {
            continue;
        }

        let candidate = line.to_string();
        if !values.contains(&candidate) {
            values.push(candidate);
            if values.len() >= max_values {
                break;
            }
        }
    }

    values
}

fn strip_list_marker(line: &str) -> &str {
    let line = if let Some(rest) = line.strip_prefix("- ") {
        rest.trim()
    } else {
        line
    };
    strip_numeric_prefix(line).trim()
}

fn strip_numeric_prefix(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }

    if i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' ' {
        &line[i + 2..]
    } else {
        line
    }
}
