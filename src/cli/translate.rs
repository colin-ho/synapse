use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use regex::Regex;

use crate::config::Config;
use crate::llm::NlTranslationContext;
use crate::spec_store::SpecStore;

/// Maximum entries in the directory listing included in NL context.
const MAX_CWD_ENTRIES: usize = 50;
/// Maximum flags per tool to include in NL context.
const MAX_FLAGS_PER_TOOL: usize = 20;

pub(super) async fn translate(
    query: String,
    cwd: PathBuf,
    recent_commands: Vec<String>,
    env_hints_raw: Vec<String>,
) -> anyhow::Result<()> {
    let config = Config::load();

    if query.len() < crate::config::NL_MIN_QUERY_LENGTH {
        print_error(&format!(
            "Natural language query too short (minimum {} characters)",
            crate::config::NL_MIN_QUERY_LENGTH
        ));
        return Ok(());
    }

    let env_hints: HashMap<String, String> = env_hints_raw
        .into_iter()
        .filter_map(|s| {
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();

    let mut llm_client = match crate::llm::LlmClient::from_config(&config.llm) {
        Some(client) => client,
        None => {
            print_error("LLM client not configured (set llm.enabled and API key)");
            return Ok(());
        }
    };
    llm_client.auto_detect_model().await;

    let context =
        prepare_nl_context(&query, cwd.as_path(), &recent_commands, &env_hints, &config).await;

    let max_suggestions = config.llm.nl_max_suggestions;
    let temperature = if max_suggestions <= 1 {
        config.llm.temperature
    } else {
        (config.llm.temperature + 0.4).min(1.0)
    };

    let result = match llm_client
        .translate_command(&context, max_suggestions, temperature)
        .await
    {
        Ok(result) => result,
        Err(e) => {
            print_error(&format!("Natural language translation failed: {e}"));
            return Ok(());
        }
    };

    let blocklist = CompiledBlocklist::new(&config.security.command_blocklist);

    let valid_items: Vec<_> = result
        .items
        .into_iter()
        .filter(|item| {
            let first_token = item.command.split_whitespace().next().unwrap_or("");
            !first_token.is_empty() && !blocklist.is_blocked(&item.command)
        })
        .collect();

    if valid_items.is_empty() {
        print_error("All NL translations were empty or blocked by security policy");
        return Ok(());
    }

    // Output TSV: list\t<count>\t<text>\t<source>\t<desc>\t<kind>\t...
    let count = valid_items.len();
    let mut out = format!("list\t{count}");
    for item in &valid_items {
        let desc = item.warning.as_deref().unwrap_or("");
        out.push('\t');
        out.push_str(&sanitize_tsv(&item.command));
        out.push_str("\tllm\t");
        out.push_str(&sanitize_tsv(desc));
        out.push_str("\tcommand");
    }
    println!("{out}");

    Ok(())
}

async fn prepare_nl_context(
    query: &str,
    cwd: &std::path::Path,
    recent_commands: &[String],
    env_hints: &HashMap<String, String>,
    config: &Config,
) -> NlTranslationContext {
    let os = detect_os();
    let cwd_str = cwd.to_string_lossy().to_string();
    let scan_depth = config.spec.scan_depth;

    let (project_root, available_tools, git_branch, cwd_entries) = tokio::join!(
        async { crate::project::find_project_root(cwd, scan_depth) },
        async { extract_available_tools(env_hints) },
        async { crate::project::read_git_branch_for_path(cwd) },
        async { read_cwd_entries(cwd).await },
    );

    let project_type = match project_root.as_ref() {
        Some(root) => crate::project::detect_project_type(root),
        None => None,
    };

    let spec_store = SpecStore::new(config.spec.clone());
    let project_commands = extract_project_commands(&spec_store, cwd).await;
    let relevant_specs = extract_relevant_specs(&spec_store, query, cwd).await;

    NlTranslationContext {
        query: query.to_string(),
        cwd: cwd_str,
        os,
        project_type,
        available_tools,
        recent_commands: recent_commands.to_vec(),
        git_branch,
        project_commands,
        cwd_entries,
        relevant_specs,
    }
}

async fn read_cwd_entries(cwd: &std::path::Path) -> Vec<String> {
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut entries = Vec::new();
        let Ok(read_dir) = std::fs::read_dir(&cwd) else {
            return entries;
        };
        for entry in read_dir {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                entries.push(format!("{name}/"));
            } else {
                entries.push(name);
            }
            if entries.len() >= MAX_CWD_ENTRIES {
                break;
            }
        }
        entries.sort();
        entries
    })
    .await
    .unwrap_or_default()
}

async fn extract_project_commands(
    spec_store: &SpecStore,
    cwd: &std::path::Path,
) -> HashMap<String, Vec<String>> {
    let specs = spec_store.get_project_specs(cwd).await;
    let mut commands: HashMap<String, Vec<String>> = HashMap::new();
    for (name, spec) in specs.as_ref() {
        commands.insert(
            name.clone(),
            spec.subcommands.iter().map(|s| s.name.clone()).collect(),
        );
    }
    commands
}

async fn extract_relevant_specs(
    spec_store: &SpecStore,
    query: &str,
    cwd: &std::path::Path,
) -> HashMap<String, Vec<String>> {
    let query_tokens: Vec<&str> = query.split_whitespace().collect();
    let all_names = spec_store.all_command_names(cwd).await;
    let mut result = HashMap::new();

    for name in &all_names {
        if query_tokens.iter().any(|t| t.eq_ignore_ascii_case(name)) {
            if let Some(spec) = spec_store.lookup(name, cwd).await {
                let flags: Vec<String> = spec
                    .options
                    .iter()
                    .take(MAX_FLAGS_PER_TOOL)
                    .filter_map(|opt| opt.long.as_ref().or(opt.short.as_ref()).cloned())
                    .collect();
                if !flags.is_empty() {
                    result.insert(name.clone(), flags);
                }
            }
        }
    }
    result
}

fn detect_os() -> String {
    static OS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OS.get_or_init(detect_os_inner).clone()
}

fn detect_os_inner() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
        {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !version.is_empty() {
                return format!("macOS {version}");
            }
        }
        "macOS".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
            for line in content.lines() {
                if let Some(pretty) = line.strip_prefix("PRETTY_NAME=") {
                    return pretty.trim_matches('"').to_string();
                }
            }
        }
        "Linux".to_string()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        std::env::consts::OS.to_string()
    }
}

fn extract_available_tools(env_hints: &HashMap<String, String>) -> Vec<String> {
    const NOTABLE: &[&str] = &[
        "git",
        "cargo",
        "rustc",
        "npm",
        "yarn",
        "pnpm",
        "node",
        "bun",
        "deno",
        "python",
        "python3",
        "pip",
        "poetry",
        "uv",
        "pdm",
        "go",
        "java",
        "gradle",
        "mvn",
        "ruby",
        "bundle",
        "rails",
        "php",
        "composer",
        "elixir",
        "mix",
        "make",
        "cmake",
        "just",
        "ninja",
        "docker",
        "kubectl",
        "helm",
        "podman",
        "aws",
        "gcloud",
        "az",
        "fly",
        "railway",
        "vercel",
        "netlify",
        "heroku",
        "terraform",
        "ansible",
        "gh",
        "act",
        "mise",
        "direnv",
        "brew",
        "ffmpeg",
        "jq",
        "rg",
        "fd",
        "bat",
        "eza",
        "fzf",
        "tmux",
        "curl",
        "wget",
        "swift",
        "xcodebuild",
        "zig",
        "gleam",
    ];

    let Some(path) = env_hints.get("PATH") else {
        return Vec::new();
    };

    let dirs: Vec<&str> = path.split(':').collect();
    let mut found = Vec::new();
    for &tool in NOTABLE {
        for dir in &dirs {
            if std::path::Path::new(&format!("{dir}/{tool}")).exists() {
                found.push(tool.to_string());
                break;
            }
        }
    }
    found
}

// --- Blocklist ---

struct CompiledBlocklist {
    patterns: Vec<CompiledBlockPattern>,
}

enum CompiledBlockPattern {
    Substring(String),
    Regex(Regex),
}

impl CompiledBlocklist {
    fn new(raw_patterns: &[String]) -> Self {
        let patterns = raw_patterns
            .iter()
            .filter_map(|p| {
                let trimmed = p.trim();
                if trimmed.is_empty() {
                    return None;
                }
                if !trimmed.contains('*') && !trimmed.contains('?') {
                    return Some(CompiledBlockPattern::Substring(trimmed.to_string()));
                }
                let regex_pattern = regex::escape(trimmed)
                    .replace(r"\*", ".*")
                    .replace(r"\?", ".");
                match Regex::new(&regex_pattern) {
                    Ok(re) => Some(CompiledBlockPattern::Regex(re)),
                    Err(_) => Some(CompiledBlockPattern::Substring(trimmed.to_string())),
                }
            })
            .collect();
        Self { patterns }
    }

    fn is_blocked(&self, command: &str) -> bool {
        self.patterns.iter().any(|p| match p {
            CompiledBlockPattern::Substring(s) => command.contains(s.as_str()),
            CompiledBlockPattern::Regex(re) => re.is_match(command),
        })
    }
}

// --- TSV helpers ---

fn sanitize_tsv(s: &str) -> Cow<'_, str> {
    if s.contains(['\t', '\n', '\r']) {
        Cow::Owned(s.replace('\t', "    ").replace('\n', " ").replace('\r', ""))
    } else {
        Cow::Borrowed(s)
    }
}

fn print_error(message: &str) {
    let sanitized = sanitize_tsv(message);
    println!("error\t{sanitized}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blocklist_substring_match() {
        let bl = CompiledBlocklist::new(&["curl -u".into(), "export *=".into()]);
        assert!(bl.is_blocked("curl -u admin:pass http://x"));
        assert!(!bl.is_blocked("curl http://example.com"));
    }

    #[test]
    fn test_blocklist_wildcard_pattern() {
        let bl = CompiledBlocklist::new(&[r#"curl -H "Authorization*"#.into()]);
        assert!(bl.is_blocked(r#"curl -H "Authorization: Bearer tok""#));
        assert!(!bl.is_blocked("curl -H Accept"));
    }

    #[test]
    fn test_blocklist_empty_and_whitespace() {
        let bl = CompiledBlocklist::new(&["".into(), "  ".into()]);
        assert!(!bl.is_blocked("anything"));
    }

    #[test]
    fn test_sanitize_tsv_clean_string() {
        assert_eq!(sanitize_tsv("hello world"), Cow::Borrowed("hello world"));
    }

    #[test]
    fn test_sanitize_tsv_replaces_tabs_and_newlines() {
        let result = sanitize_tsv("a\tb\nc\r");
        assert_eq!(result, "a    b c");
    }

    #[test]
    fn test_tsv_output_format() {
        // Verify the exact TSV wire format the plugin parses:
        // list\t<count>\t<text>\t<source>\t<desc>\t<kind>\t...
        let items = vec![
            crate::llm::NlTranslationItem {
                command: "git status".into(),
                warning: None,
            },
            crate::llm::NlTranslationItem {
                command: "git stash".into(),
                warning: Some("Stash changes".into()),
            },
        ];

        let count = items.len();
        let mut out = format!("list\t{count}");
        for item in &items {
            let desc = item.warning.as_deref().unwrap_or("");
            out.push('\t');
            out.push_str(&sanitize_tsv(&item.command));
            out.push_str("\tllm\t");
            out.push_str(&sanitize_tsv(desc));
            out.push_str("\tcommand");
        }

        // Parse it back the same way the plugin does (tab-split)
        let fields: Vec<&str> = out.split('\t').collect();
        assert_eq!(fields[0], "list");
        assert_eq!(fields[1], "2");
        // Item 0: text=git status, source=llm, desc="", kind=command
        assert_eq!(fields[2], "git status");
        assert_eq!(fields[3], "llm");
        assert_eq!(fields[4], "");
        assert_eq!(fields[5], "command");
        // Item 1: text=git stash, source=llm, desc=Stash changes, kind=command
        assert_eq!(fields[6], "git stash");
        assert_eq!(fields[7], "llm");
        assert_eq!(fields[8], "Stash changes");
        assert_eq!(fields[9], "command");
    }

    #[test]
    fn test_tsv_error_format() {
        let msg = sanitize_tsv("bad request");
        let out = format!("error\t{msg}");
        let fields: Vec<&str> = out.split('\t').collect();
        assert_eq!(fields, vec!["error", "bad request"]);
    }

    #[test]
    fn test_tsv_sanitizes_embedded_tabs_in_commands() {
        // If an LLM returns a command with tabs/newlines, TSV must not break
        let nasty = "echo\t'hello\nworld'";
        let sanitized = sanitize_tsv(nasty);
        assert!(!sanitized.contains('\t'));
        assert!(!sanitized.contains('\n'));
    }
}
