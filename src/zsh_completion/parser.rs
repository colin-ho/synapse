use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

use crate::spec::{CommandSpec, OptionSpec, SpecSource, SubcommandSpec};

pub(super) fn parse_zsh_completion(command: &str, content: &str) -> CommandSpec {
    let mut options = Vec::new();

    static SHORT_LONG_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\{(-[a-zA-Z0-9]\+?),\s*(--[\w][\w.-]*=?)\}\s*'?\[([^\]]*)\]"#).unwrap()
    });

    static LONG_ONLY_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"'(--[\w][\w.-]*)(=?)\[([^\]]*)\]"#).unwrap());

    static SHORT_ONLY_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"'(-[a-zA-Z0-9])(\+?)\[([^\]]*)\]"#).unwrap());

    let mut seen_long: HashSet<String> = HashSet::new();
    let mut seen_short: HashSet<String> = HashSet::new();

    for caps in SHORT_LONG_RE.captures_iter(content) {
        let short_raw = caps.get(1).unwrap().as_str();
        let long_raw = caps.get(2).unwrap().as_str();
        let desc = caps.get(3).unwrap().as_str().trim();

        let takes_arg = short_raw.ends_with('+') || long_raw.ends_with('=');
        let short = short_raw.trim_end_matches('+').to_string();
        let long = long_raw.trim_end_matches('=').to_string();

        if long == "--help" || long == "--version" || short == "-h" || short == "-V" {
            continue;
        }

        seen_short.insert(short.clone());
        seen_long.insert(long.clone());

        options.push(OptionSpec {
            short: Some(short),
            long: Some(long),
            description: if desc.is_empty() {
                None
            } else {
                Some(desc.to_string())
            },
            takes_arg,
            ..Default::default()
        });
    }

    for caps in LONG_ONLY_RE.captures_iter(content) {
        let long = caps.get(1).unwrap().as_str().to_string();
        let eq = caps.get(2).unwrap().as_str();
        let desc = caps.get(3).unwrap().as_str().trim();

        if long == "--help" || long == "--version" || seen_long.contains(&long) {
            continue;
        }
        seen_long.insert(long.clone());

        options.push(OptionSpec {
            long: Some(long),
            description: if desc.is_empty() {
                None
            } else {
                Some(desc.to_string())
            },
            takes_arg: eq == "=",
            ..Default::default()
        });
    }

    for caps in SHORT_ONLY_RE.captures_iter(content) {
        let short = caps.get(1).unwrap().as_str().to_string();
        let plus = caps.get(2).unwrap().as_str();
        let desc = caps.get(3).map(|m| m.as_str().trim()).unwrap_or("");

        if short == "-h" || short == "-V" || seen_short.contains(&short) {
            continue;
        }
        seen_short.insert(short.clone());

        options.push(OptionSpec {
            short: Some(short),
            description: if desc.is_empty() {
                None
            } else {
                Some(desc.to_string())
            },
            takes_arg: plus == "+",
            ..Default::default()
        });
    }

    static SUBCMD_ENTRY_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"'([\w][\w.-]+):([^']*)'").unwrap());

    let mut subcommands = Vec::new();
    let mut seen_subcmds: HashSet<String> = HashSet::new();
    let mut in_commands_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.contains("commands=(") || trimmed.contains("commands =(") {
            in_commands_block = true;
        }

        if in_commands_block {
            for caps in SUBCMD_ENTRY_RE.captures_iter(line) {
                let name = caps.get(1).unwrap().as_str().to_string();
                let desc = caps.get(2).unwrap().as_str().trim().to_string();
                if !seen_subcmds.contains(&name) {
                    seen_subcmds.insert(name.clone());
                    subcommands.push(SubcommandSpec {
                        name,
                        description: if desc.is_empty() { None } else { Some(desc) },
                        ..Default::default()
                    });
                }
            }

            if trimmed.ends_with(')') && !trimmed.contains("=(") {
                in_commands_block = false;
            }
        }
    }

    CommandSpec {
        name: command.to_string(),
        options,
        subcommands,
        source: SpecSource::Discovered,
        ..Default::default()
    }
}
