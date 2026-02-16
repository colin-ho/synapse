use regex::Regex;
use std::sync::LazyLock;

use crate::spec::{CommandSpec, OptionSpec, SubcommandSpec};

/// Parse `--help` output text into a `CommandSpec`.
///
/// This is a best-effort parser that handles common help output formats
/// from popular CLI frameworks (clap, cobra, argparse, click, POSIX).
/// It extracts subcommands, options (with takes_arg detection), and descriptions.
pub fn parse_help_output(command_name: &str, help_text: &str) -> CommandSpec {
    let lines: Vec<&str> = help_text.lines().collect();
    let sections = detect_sections(&lines);

    let mut description = None;
    let mut subcommands = Vec::new();
    let mut options = Vec::new();

    for (section, start, end) in &sections {
        let section_lines = &lines[*start..*end];
        match section {
            Section::Description => {
                if description.is_none() {
                    let desc = section_lines
                        .iter()
                        .map(|l| l.trim())
                        .filter(|l| !l.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !desc.is_empty() {
                        description = Some(desc);
                    }
                }
            }
            Section::Commands => {
                for line in section_lines {
                    if let Some(sub) = parse_subcommand_line(line) {
                        subcommands.push(sub);
                    }
                }
            }
            Section::Options => {
                let parsed = parse_option_block(section_lines);
                options.extend(parsed);
            }
            _ => {}
        }
    }

    // Filter out help/version options — they're noise for completions
    options.retain(|o| {
        !matches!(o.long.as_deref(), Some("--help" | "--version"))
            && !matches!(o.short.as_deref(), Some("-h" | "-V"))
    });

    CommandSpec {
        name: command_name.to_string(),
        description,
        subcommands,
        options,
        ..Default::default()
    }
}

#[derive(Debug, PartialEq)]
enum Section {
    Description,
    Usage,
    Commands,
    Options,
}

/// Detect sections in help text by their headings.
/// Returns (section_type, content_start_line, content_end_line).
fn detect_sections(lines: &[&str]) -> Vec<(Section, usize, usize)> {
    static COMMANDS_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(available\s+)?(commands|subcommands)\s*:?\s*$").unwrap()
    });
    static OPTIONS_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^((\w+\s+)?(options|flags)|optional\s+arguments)\s*:?\s*$").unwrap()
    });
    static USAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^usage\s*:").unwrap());

    let mut sections = Vec::new();
    let mut i = 0;

    // Try to extract a description from the first non-empty lines before any section heading
    let first_content = lines.iter().position(|l| !l.trim().is_empty());
    if let Some(start) = first_content {
        let first_line = lines[start].trim();
        // The first line is a description if it's not a section heading or usage line
        if !COMMANDS_RE.is_match(first_line)
            && !OPTIONS_RE.is_match(first_line)
            && !USAGE_RE.is_match(first_line)
            && !first_line.ends_with(':')
        {
            // Find where this description ends (next blank line or section heading)
            let mut end = start + 1;
            while end < lines.len() {
                let trimmed = lines[end].trim();
                if trimmed.is_empty()
                    || COMMANDS_RE.is_match(trimmed)
                    || OPTIONS_RE.is_match(trimmed)
                    || USAGE_RE.is_match(trimmed)
                {
                    break;
                }
                // Stop if the next line looks like a heading (no leading whitespace, ends with :)
                if !lines[end].starts_with(' ') && trimmed.ends_with(':') {
                    break;
                }
                end += 1;
            }
            sections.push((Section::Description, start, end));
        }
    }

    while i < lines.len() {
        let trimmed = lines[i].trim();

        if COMMANDS_RE.is_match(trimmed) {
            let start = i + 1;
            let end = find_section_end(lines, start);
            sections.push((Section::Commands, start, end));
            i = end;
        } else if OPTIONS_RE.is_match(trimmed) {
            let start = i + 1;
            let end = find_section_end(lines, start);
            sections.push((Section::Options, start, end));
            i = end;
        } else if USAGE_RE.is_match(trimmed) {
            let start = i;
            let end = find_section_end(lines, start + 1);
            sections.push((Section::Usage, start, end));
            i = end;
        } else {
            i += 1;
        }
    }

    sections
}

/// Find where a section's content ends (next section heading or end of text).
fn find_section_end(lines: &[&str], start: usize) -> usize {
    static HEADING_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Z][A-Za-z\s]*:\s*$").unwrap());

    for (i, line) in lines.iter().enumerate().skip(start) {
        let trimmed = line.trim();
        // A new section starts at a non-indented line ending with ':'
        if !trimmed.is_empty()
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && (HEADING_RE.is_match(trimmed) || trimmed.ends_with(':'))
        {
            return i;
        }
    }
    lines.len()
}

/// Parse a single line from the commands/subcommands section into a SubcommandSpec.
fn parse_subcommand_line(line: &str) -> Option<SubcommandSpec> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Skip lines that look like section headings or notes
    if !line.starts_with(' ') && !line.starts_with('\t') {
        return None;
    }

    // Pattern: "  name[, alias]    description"
    // Split on 2+ spaces to separate name from description
    static SUBCMD_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s+([\w][\w.-]*(?:\s*,\s*[\w][\w.-]*)*)\s{2,}(.+)$").unwrap()
    });

    if let Some(caps) = SUBCMD_RE.captures(line) {
        let names_str = caps.get(1)?.as_str();
        let description = caps.get(2).map(|m| m.as_str().trim().to_string());

        let mut names: Vec<&str> = names_str.split(',').map(|s| s.trim()).collect();
        let name = names.remove(0).to_string();
        let aliases: Vec<String> = names.iter().map(|s| s.to_string()).collect();

        return Some(SubcommandSpec {
            name,
            aliases,
            description,
            ..Default::default()
        });
    }

    // Simpler pattern: just an indented word with no description
    static SIMPLE_SUBCMD_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s+([\w][\w.-]*)\s*$").unwrap());

    if let Some(caps) = SIMPLE_SUBCMD_RE.captures(line) {
        let name = caps.get(1)?.as_str().to_string();
        return Some(SubcommandSpec {
            name,
            ..Default::default()
        });
    }

    None
}

/// Parse a block of option lines, handling multi-line descriptions.
fn parse_option_block(lines: &[&str]) -> Vec<OptionSpec> {
    let mut options = Vec::new();
    let mut current: Option<OptionSpec> = None;
    let mut current_indent = 0usize;

    for line in lines {
        if line.trim().is_empty() {
            if let Some(opt) = current.take() {
                options.push(opt);
            }
            continue;
        }

        let indent = line.len() - line.trim_start().len();

        // Try parsing as a new option line
        if let Some(opt) = parse_option_line(line) {
            if let Some(prev) = current.take() {
                options.push(prev);
            }
            current_indent = indent;
            current = Some(opt);
        } else if let Some(ref mut opt) = current {
            // Continuation line — append to description if more indented
            if indent > current_indent {
                let extra = line.trim();
                if !extra.is_empty() {
                    if let Some(ref mut desc) = opt.description {
                        desc.push(' ');
                        desc.push_str(extra);
                    } else {
                        opt.description = Some(extra.to_string());
                    }
                }
            } else {
                // Not a continuation and not parseable — flush current
                options.push(current.take().unwrap());
            }
        }
    }

    if let Some(opt) = current {
        options.push(opt);
    }

    options
}

/// Parse a single option line into an OptionSpec.
///
/// Handles formats like:
/// - `-v, --verbose          Be verbose`
/// - `    --output <FILE>    Output file`
/// - `-o FILE                Output file`
/// - `-n, --count=NUM        Number of results`
/// - `    --color[=WHEN]     Colorize output`
fn parse_option_line(line: &str) -> Option<OptionSpec> {
    // Must be indented (options are always indented in help text)
    if !line.starts_with(' ') && !line.starts_with('\t') {
        return None;
    }

    let trimmed = line.trim();
    if !trimmed.starts_with('-') {
        return None;
    }

    // Match the flag portion: everything up to the description (2+ spaces gap)
    // Pattern: optional short flag, optional long flag, optional value indicator
    static OPT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^\s+(-\w)?(?:\s*,\s*|\s+)?(--[\w][\w.-]*)?\s*(?:[=\s]\s*(\[?<?[\w.|/-]+>?\]?))?\s{2,}(.+)$"
        ).unwrap()
    });

    // Simpler pattern for options with no description (just flags)
    static OPT_NODESC_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^\s+(-\w)?(?:\s*,\s*|\s+)?(--[\w][\w.-]*)?\s*(?:[=\s]\s*(\[?<?[\w.|/-]+>?\]?))?\s*$",
        )
        .unwrap()
    });

    let (short, long, value_hint, description) = if let Some(caps) = OPT_RE.captures(line) {
        (
            caps.get(1).map(|m| m.as_str().to_string()),
            caps.get(2).map(|m| m.as_str().to_string()),
            caps.get(3).map(|m| m.as_str().to_string()),
            caps.get(4).map(|m| m.as_str().trim().to_string()),
        )
    } else if let Some(caps) = OPT_NODESC_RE.captures(line) {
        (
            caps.get(1).map(|m| m.as_str().to_string()),
            caps.get(2).map(|m| m.as_str().to_string()),
            caps.get(3).map(|m| m.as_str().to_string()),
            None,
        )
    } else {
        return None;
    };

    // Must have at least one flag
    if short.is_none() && long.is_none() {
        return None;
    }

    let takes_arg = value_hint.is_some();

    Some(OptionSpec {
        short,
        long,
        description,
        takes_arg,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clap_style_help() {
        let help = r#"A fast line-oriented regex search tool

Usage: rg [OPTIONS] <PATTERN> [PATH]...

Commands:
  pcre2    Use PCRE2 regex engine
  help     Print this message or the help of the given subcommand(s)

Options:
  -e, --regexp <PATTERN>     A pattern to search for
  -f, --file <PATTERNFILE>   Search for patterns from the given file
  -i, --ignore-case          Search case insensitively
  -v, --invert-match         Invert matching
  -w, --word-regexp          Show matches surrounded by word boundaries
  -c, --count                Show count of matching lines
      --color <WHEN>         Controls when to use color [default: auto]
  -h, --help                 Print help
  -V, --version              Print version
"#;

        let spec = parse_help_output("rg", help);
        assert_eq!(spec.name, "rg");
        assert_eq!(
            spec.description.as_deref(),
            Some("A fast line-oriented regex search tool")
        );

        // Should have subcommands (help is filtered because it's not useful for completions)
        assert!(spec.subcommands.iter().any(|s| s.name == "pcre2"));
        assert!(spec.subcommands.iter().any(|s| s.name == "help"));

        // Should have options (minus -h/--help and -V/--version which are filtered)
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--regexp")));
        assert!(spec
            .options
            .iter()
            .any(|o| o.short.as_deref() == Some("-i")));
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--color")));

        // --help and --version should be filtered out
        assert!(!spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--help")));
        assert!(!spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--version")));

        // takes_arg detection
        let regexp = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--regexp"))
            .unwrap();
        assert!(regexp.takes_arg);

        let ignore_case = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--ignore-case"))
            .unwrap();
        assert!(!ignore_case.takes_arg);

        let color = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--color"))
            .unwrap();
        assert!(color.takes_arg);
    }

    #[test]
    fn test_cobra_style_help() {
        let help = r#"Work with GitHub pull requests.

Usage:
  gh pr [command]

Available Commands:
  checkout    Check out a pull request in git
  close       Close a pull request
  create      Create a pull request
  diff        View changes in a pull request
  list        List pull requests in a repository
  merge       Merge a pull request
  review      Add a review to a pull request
  view        View a pull request

Flags:
  -R, --repo OWNER/REPO   Select another repository
  -h, --help               Help for pr

Global Flags:
      --verbose   Enable verbose output
"#;

        let spec = parse_help_output("gh", help);
        assert_eq!(spec.name, "gh");

        // All subcommands should be parsed
        assert!(spec.subcommands.len() >= 8);
        assert!(spec.subcommands.iter().any(|s| s.name == "checkout"));
        assert!(spec.subcommands.iter().any(|s| s.name == "create"));
        assert!(spec.subcommands.iter().any(|s| s.name == "merge"));

        // Descriptions should be captured
        let checkout = spec
            .subcommands
            .iter()
            .find(|s| s.name == "checkout")
            .unwrap();
        assert_eq!(
            checkout.description.as_deref(),
            Some("Check out a pull request in git")
        );

        // Flags section should be parsed as options
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--repo")));
        let repo = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--repo"))
            .unwrap();
        assert!(repo.takes_arg);

        // Global flags should also be captured
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--verbose")));
    }

    #[test]
    fn test_argparse_style_help() {
        let help = r#"usage: black [options] [src ...]

The uncompromising code formatter.

Options:
  -c, --code TEXT           Format the code passed in as a string.
  -l, --line-length INT     How many characters per line to allow.
  -t, --target-version VER  Python versions that should be supported.
      --check               Don't write the files back, just return the status.
      --diff                Don't write the files back, just output a diff.
  -q, --quiet               Don't emit non-error messages to stderr.
  -v, --verbose             Also emit messages to stderr about files
                            that were not changed.
  -h, --help                Show this help message and exit.
  --version                 Show program's version number and exit.
"#;

        let spec = parse_help_output("black", help);
        assert_eq!(spec.name, "black");

        // Options
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--code")));
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--check")));
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--quiet")));

        let code = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--code"))
            .unwrap();
        assert!(code.takes_arg);
        assert_eq!(code.short.as_deref(), Some("-c"));

        let check = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--check"))
            .unwrap();
        assert!(!check.takes_arg);

        // Multi-line description should be joined
        let verbose = spec
            .options
            .iter()
            .find(|o| o.long.as_deref() == Some("--verbose"))
            .unwrap();
        assert!(verbose
            .description
            .as_ref()
            .unwrap()
            .contains("not changed"));
    }

    #[test]
    fn test_option_takes_arg_detection() {
        // Value in angle brackets
        let opt = parse_option_line("  -o, --output <FILE>   Output file").unwrap();
        assert!(opt.takes_arg);

        // Value with equals sign
        let opt = parse_option_line("  -n, --count=NUM       Number of results").unwrap();
        assert!(opt.takes_arg);

        // Boolean flag (no value)
        let opt = parse_option_line("  -v, --verbose         Be verbose").unwrap();
        assert!(!opt.takes_arg);

        // Long flag only with value
        let opt = parse_option_line("      --color <WHEN>    Colorize output").unwrap();
        assert!(opt.takes_arg);
        assert!(opt.short.is_none());
    }

    #[test]
    fn test_subcommand_with_aliases() {
        let line = "  checkout, co    Check out a branch";
        let sub = parse_subcommand_line(line).unwrap();
        assert_eq!(sub.name, "checkout");
        assert_eq!(sub.aliases, vec!["co"]);
        assert_eq!(sub.description.as_deref(), Some("Check out a branch"));
    }

    #[test]
    fn test_subcommand_no_description() {
        let line = "  serve";
        let sub = parse_subcommand_line(line).unwrap();
        assert_eq!(sub.name, "serve");
        assert!(sub.description.is_none());
    }

    #[test]
    fn test_empty_help() {
        let spec = parse_help_output("unknown", "");
        assert_eq!(spec.name, "unknown");
        assert!(spec.subcommands.is_empty());
        assert!(spec.options.is_empty());
    }

    #[test]
    fn test_minimal_help() {
        let help = "Usage: mytool [FILE]\n";
        let spec = parse_help_output("mytool", help);
        assert_eq!(spec.name, "mytool");
    }

    #[test]
    fn test_click_style_help() {
        let help = r#"Usage: flask [OPTIONS] COMMAND [ARGS]...

  A general utility script for Flask applications.

Options:
  --version  Show the flask version.
  --help     Show this message and exit.

Commands:
  routes  Show the routes for the app.
  run     Run a development server.
  shell   Run a shell in the app context.
"#;

        let spec = parse_help_output("flask", help);
        assert!(spec.subcommands.iter().any(|s| s.name == "routes"));
        assert!(spec.subcommands.iter().any(|s| s.name == "run"));
        assert!(spec.subcommands.iter().any(|s| s.name == "shell"));

        let routes = spec
            .subcommands
            .iter()
            .find(|s| s.name == "routes")
            .unwrap();
        assert_eq!(
            routes.description.as_deref(),
            Some("Show the routes for the app.")
        );
    }

    #[test]
    fn test_help_and_version_filtered() {
        let help = r#"Options:
  -h, --help      Print help
  -V, --version   Print version
  -v, --verbose   Be verbose
"#;
        let spec = parse_help_output("test", help);
        assert_eq!(spec.options.len(), 1);
        assert_eq!(spec.options[0].long.as_deref(), Some("--verbose"));
    }

    #[test]
    fn test_option_long_only() {
        let opt = parse_option_line("      --frozen      Do not update the lock file").unwrap();
        assert!(opt.short.is_none());
        assert_eq!(opt.long.as_deref(), Some("--frozen"));
        assert!(!opt.takes_arg);
    }

    #[test]
    fn test_non_option_line_rejected() {
        assert!(parse_option_line("This is a description line").is_none());
        assert!(parse_option_line("").is_none());
        assert!(parse_option_line("  subcommand    description").is_none());
    }

    #[test]
    fn test_non_subcommand_line_rejected() {
        // Non-indented lines should be rejected
        assert!(parse_subcommand_line("Commands:").is_none());
        assert!(parse_subcommand_line("").is_none());
    }

    #[test]
    fn test_description_extraction() {
        let help = r#"ripgrep 14.1.0
Andrew Gallant <jamslam@gmail.com>

ripgrep (rg) recursively searches the current directory for lines matching
a regex pattern.

Usage:
  rg [OPTIONS] PATTERN [PATH ...]

Options:
  -i, --ignore-case   Search case insensitively
"#;

        let spec = parse_help_output("rg", help);
        // Should capture first line(s) as description
        assert!(spec.description.is_some());
    }

    #[test]
    fn test_multiple_option_sections() {
        let help = r#"Usage: tool [OPTIONS]

Options:
  -v, --verbose     Be verbose
  -q, --quiet       Be quiet

Global Flags:
      --debug       Enable debug mode
"#;

        let spec = parse_help_output("tool", help);
        // Both "Options:" and "Global Flags:" should be matched
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--verbose")));
        assert!(spec
            .options
            .iter()
            .any(|o| o.long.as_deref() == Some("--debug")));
    }

    #[test]
    fn test_roundtrip_serialization() {
        let help = r#"My tool does things

Usage: mytool [OPTIONS] [COMMAND]

Commands:
  serve     Start the server
  build     Build the project

Options:
  -p, --port <PORT>   Port number
  -v, --verbose        Be verbose
  -h, --help           Print help
"#;

        let spec = parse_help_output("mytool", help);
        let toml_str = toml::to_string_pretty(&spec).unwrap();
        let roundtrip: CommandSpec = toml::from_str(&toml_str).unwrap();

        assert_eq!(roundtrip.name, spec.name);
        assert_eq!(roundtrip.subcommands.len(), spec.subcommands.len());
        assert_eq!(roundtrip.options.len(), spec.options.len());
    }
}
