//! Parse zsh completion files (`_arguments` specs) into `CommandSpec`.
//!
//! Zsh ships with ~1000 structured completion functions that describe CLI
//! options, subcommands, and argument types. This module reads those files
//! and extracts option/flag information — a faster and more reliable
//! alternative to parsing `--help` text.

use std::path::{Path, PathBuf};

use regex::Regex;
use std::sync::LazyLock;

use crate::spec::{CommandSpec, OptionSpec, SpecSource};

/// Literal directories to search for zsh completion files.
const FPATH_DIRS: &[&str] = &[
    "/usr/local/share/zsh/site-functions",
    "/opt/homebrew/share/zsh/site-functions",
];

/// Parent directory containing versioned zsh function dirs (e.g. `/usr/share/zsh/5.9/functions`).
const ZSH_SHARE_DIR: &str = "/usr/share/zsh";

/// Scan all fpath directories and return the set of command names that have
/// completion files available. This is just a `readdir` — no file parsing —
/// so it completes in sub-millisecond time.
pub fn scan_available_commands() -> std::collections::HashSet<String> {
    let mut commands = std::collections::HashSet::new();

    let mut dirs: Vec<PathBuf> = FPATH_DIRS.iter().map(PathBuf::from).collect();

    // Expand /usr/share/zsh/*/functions
    if let Ok(entries) = std::fs::read_dir(ZSH_SHARE_DIR) {
        for entry in entries.flatten() {
            dirs.push(entry.path().join("functions"));
        }
    }

    for dir in &dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(cmd) = name.strip_prefix('_') {
                    if !cmd.is_empty() && !cmd.contains('.') {
                        commands.insert(cmd.to_string());
                    }
                }
            }
        }
    }

    commands
}

/// Find the zsh completion file for a command (e.g. `_htop` for `htop`).
pub fn find_completion_file(command: &str) -> Option<PathBuf> {
    let target = format!("_{command}");

    // Check literal directories first.
    for dir in FPATH_DIRS {
        let candidate = Path::new(dir).join(&target);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Expand /usr/share/zsh/*/functions — iterate version subdirs.
    if let Ok(entries) = std::fs::read_dir(ZSH_SHARE_DIR) {
        for entry in entries.flatten() {
            let functions_dir = entry.path().join("functions");
            let candidate = functions_dir.join(&target);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Try to find and parse a zsh completion file for the given command.
/// Returns `None` if no completion file exists or parsing yields nothing useful.
pub fn find_and_parse(command: &str) -> Option<CommandSpec> {
    let path = find_completion_file(command)?;
    let content = std::fs::read_to_string(&path).ok()?;
    let spec = parse_zsh_completion(command, &content);
    if spec.options.is_empty() && spec.subcommands.is_empty() {
        return None;
    }
    Some(spec)
}

/// Parse a zsh completion file's content into a `CommandSpec`.
pub fn parse_zsh_completion(command: &str, content: &str) -> CommandSpec {
    let mut options = Vec::new();

    // Regex to match zsh _arguments option spec strings.
    //
    // Patterns we handle:
    //   '(-o --option)'{-o+,--option=}'[description]'
    //   '--long-option=[description]'
    //   '-f[description]'
    //   '-f+[description]:arg'
    //   \*{-p+,--pid=}'[description]'
    //   '*'{-p+,--pid=}'[description]'

    // Strategy: extract individual option-like tokens from the file, rather
    // than trying to parse the full zsh scripting language.  We look for
    // short options (-X, -X+) and long options (--foo, --foo=) followed by
    // an optional description in square brackets.

    static SHORT_LONG_RE: LazyLock<Regex> = LazyLock::new(|| {
        // Match patterns like: {-d+,--delay=}  or  {-C,--no-color}
        // May be preceded by exclusion group and/or * prefix.
        Regex::new(r#"\{(-[a-zA-Z0-9]\+?),\s*(--[\w][\w.-]*=?)\}\s*'?\[([^\]]*)\]"#).unwrap()
    });

    static LONG_ONLY_RE: LazyLock<Regex> = LazyLock::new(|| {
        // Match standalone long options:
        //   '--option[description]'     (inside same quotes)
        //   '--option=[description]'    (with = inside same quotes)
        //   '--option=''[description]'  (description in separate quotes)
        Regex::new(r#"'(--[\w][\w.-]*)(=?)\[([^\]]*)\]"#).unwrap()
    });

    static SHORT_ONLY_RE: LazyLock<Regex> = LazyLock::new(|| {
        // Match standalone short options:
        //   '-f[description]'
        //   '-f+[description]'
        Regex::new(r#"'(-[a-zA-Z0-9])(\+?)\[([^\]]*)\]"#).unwrap()
    });

    // Track seen options to deduplicate.
    let mut seen_long: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_short: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Pass 1: short+long pairs from brace expansions.
    for caps in SHORT_LONG_RE.captures_iter(content) {
        let short_raw = caps.get(1).unwrap().as_str();
        let long_raw = caps.get(2).unwrap().as_str();
        let desc = caps.get(3).unwrap().as_str().trim();

        let takes_arg = short_raw.ends_with('+') || long_raw.ends_with('=');
        let short = short_raw.trim_end_matches('+').to_string();
        let long = long_raw.trim_end_matches('=').to_string();

        if long == "--help" || long == "--version" {
            continue;
        }
        if short == "-h" || short == "-V" {
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

    // Pass 2: standalone long options not already captured.
    for caps in LONG_ONLY_RE.captures_iter(content) {
        let long = caps.get(1).unwrap().as_str().to_string();
        let eq = caps.get(2).unwrap().as_str();
        let desc = caps.get(3).unwrap().as_str().trim();

        if long == "--help" || long == "--version" {
            continue;
        }
        if seen_long.contains(&long) {
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

    // Pass 3: standalone short options not already captured.
    for caps in SHORT_ONLY_RE.captures_iter(content) {
        let short = caps.get(1).unwrap().as_str().to_string();
        let plus = caps.get(2).unwrap().as_str();
        let desc = caps.get(3).map(|m| m.as_str().trim()).unwrap_or("");

        if short == "-h" || short == "-V" {
            continue;
        }
        if seen_short.contains(&short) {
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

    CommandSpec {
        name: command.to_string(),
        options,
        source: SpecSource::Discovered,
        ..Default::default()
    }
}

/// Maximum bytes to read from a completion generator's output.
const MAX_GENERATOR_OUTPUT_BYTES: usize = 256 * 1024;

/// Try to obtain a zsh completion script by running common completion-generator
/// subcommands (e.g. `kubectl completion zsh`). Returns a parsed `CommandSpec`
/// on success, or `None` if no generator pattern works.
pub async fn try_completion_generator(
    command: &str,
    timeout: std::time::Duration,
) -> Option<CommandSpec> {
    let patterns: &[&[&str]] = &[
        &["completion", "zsh"],
        &["completions", "zsh"],
        &["completion", "--shell", "zsh"],
        &["--completions", "zsh"],
    ];

    for args in patterns {
        let result = tokio::time::timeout(timeout, async {
            tokio::process::Command::new(command)
                .args(*args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .await
        })
        .await;

        let output = match result {
            Ok(Ok(o)) if o.status.success() => o,
            _ => continue,
        };

        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        stdout.truncate(MAX_GENERATOR_OUTPUT_BYTES);

        // Validate that the output looks like a zsh completion script.
        if !stdout.contains("_arguments") && !stdout.contains("#compdef") {
            continue;
        }

        let spec = parse_zsh_completion(command, &stdout);
        if !spec.options.is_empty() || !spec.subcommands.is_empty() {
            tracing::info!(
                "Completion generator succeeded for {command} with args {:?}",
                args
            );
            return Some(spec);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_htop_completion() {
        let content = r#"#compdef htop pcp-htop
args=(
  '(-d --delay)'{-d+,--delay=}'[specify update frequency]:delay (tenths of seconds) (1-100) [15]'
  '(-C --no-color --no-colour)'{-C,--no-colo{,u}r}'[use monochrome colour scheme]'
  '(-F --filter)'{-F+,--filter=}'[show only commands matching specified filter]:case-insensitive command-line sub-string:_process_names -a'
  '(-)'{-h,--help}'[display usage information]'
  '(-M --no-mouse)'{-M,--no-mouse}'[disable mouse]'
  \*{-p+,--pid=}'[show only specified PIDs]: : _sequence _pids'
  '--readonly[disable all system and process changing features]'
  '(-s --sort-key)'{-s+,--sort-key=}'[sort by specified column]: :->sort-keys'
  '(-t --tree)'{-t,--tree}'[show tree view of processes]'
  '(-u --user)'{-u+,--user=}'[show only processes of current or specified user]:: : _users'
  '(-U --no-unicode)'{-U,--no-unicode}'[disable Unicode]'
  '(-)'{-V,--version}'[display version information]'
)
_arguments -s -S : $args && ret=0
"#;
        let spec = parse_zsh_completion("htop", content);

        let long_names: Vec<&str> = spec
            .options
            .iter()
            .filter_map(|o| o.long.as_deref())
            .collect();

        assert!(
            long_names.contains(&"--delay"),
            "missing --delay: {long_names:?}"
        );
        assert!(
            long_names.contains(&"--no-mouse"),
            "missing --no-mouse: {long_names:?}"
        );
        assert!(
            long_names.contains(&"--tree"),
            "missing --tree: {long_names:?}"
        );
        assert!(
            long_names.contains(&"--sort-key"),
            "missing --sort-key: {long_names:?}"
        );
        assert!(
            long_names.contains(&"--readonly"),
            "missing --readonly: {long_names:?}"
        );
        assert!(!long_names.contains(&"--help"), "--help should be excluded");
        assert!(
            !long_names.contains(&"--version"),
            "--version should be excluded"
        );

        // Check takes_arg
        let delay = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--delay"))
            .unwrap();
        assert!(delay.takes_arg, "--delay should take an arg");
        assert_eq!(delay.short.as_deref(), Some("-d"));

        let tree = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--tree"))
            .unwrap();
        assert!(!tree.takes_arg, "--tree should not take an arg");

        assert!(
            spec.options.len() >= 7,
            "expected at least 7 options, got {}",
            spec.options.len()
        );
    }

    #[test]
    fn test_parse_killall_style() {
        let content = r#"
  args=(
    '(-e --exact)'{-e,--exact}'[require exact match for names longer than 15 chars]'
    '(-I --ignore-case)'{-I,--ignore-case}'[do case insensitive process name match]'
    '(-g --process-group)'{-g,--process-group}'[kill the process group to which the process belongs]'
    '(-i --interactive)'{-i,--interactive}'[interactively ask for confirmation before killing]'
    '(- : *)'{-l,--list}'[list all known signal names]'
    '(-q --quiet)'{-q,--quiet}'[do not complain if no processes were killed]'
    '(-r --regexp)'{-r,--regexp}'[interpret process name as extended regular expression]'
    '(-v --verbose)'{-v,--verbose}'[report if the signal was successfully sent]'
    '(-w --wait)'{-w,--wait}'[wait for all killed processes to die]'
  )
  _arguments -s -S -C : $args && ret=0
"#;
        let spec = parse_zsh_completion("killall", content);

        let long_names: Vec<&str> = spec
            .options
            .iter()
            .filter_map(|o| o.long.as_deref())
            .collect();

        assert!(long_names.contains(&"--exact"), "missing --exact");
        assert!(long_names.contains(&"--quiet"), "missing --quiet");
        assert!(long_names.contains(&"--wait"), "missing --wait");
        // All should be flags (no args)
        for opt in &spec.options {
            assert!(!opt.takes_arg, "{:?} should not take an arg", opt.long);
        }
    }

    #[test]
    fn test_parse_standalone_long_option() {
        let content = r#"
  '--readonly[disable all system and process changing features]'
  '--color=[enable colors]:when:(auto always never)'
"#;
        let spec = parse_zsh_completion("test", content);
        let readonly = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--readonly"))
            .expect("missing --readonly");
        assert!(!readonly.takes_arg);

        let color = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--color"))
            .expect("missing --color");
        assert!(color.takes_arg);
    }

    #[test]
    fn test_skips_help_and_version() {
        let content = r#"
  '(-)'{-h,--help}'[display usage information]'
  '(-)'{-V,--version}'[display version information]'
  '(-t --tree)'{-t,--tree}'[show tree view]'
"#;
        let spec = parse_zsh_completion("test", content);
        assert_eq!(spec.options.len(), 1);
        assert_eq!(spec.options[0].long.as_deref(), Some("--tree"));
    }

    #[test]
    fn test_find_completion_file() {
        // This test depends on the system having zsh completions installed.
        // On macOS with default zsh, _ls should exist.
        if let Some(path) = find_completion_file("ls") {
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("_ls"));
        }
        // Non-existent command should return None.
        assert!(find_completion_file("nonexistent_command_xyz_12345").is_none());
    }
}
