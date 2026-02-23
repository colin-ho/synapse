use std::collections::HashMap;

use crate::llm::NlTranslationContext;
use crate::protocol::NaturalLanguageRequest;

use super::super::state::RuntimeState;

/// Maximum entries in the directory listing included in NL context.
const MAX_CWD_ENTRIES: usize = 50;
/// Maximum flags per tool to include in NL context.
const MAX_FLAGS_PER_TOOL: usize = 20;

pub(super) struct PreparedNlContext {
    pub(super) context: NlTranslationContext,
}

pub(super) async fn prepare_nl_context(
    req: &NaturalLanguageRequest,
    state: &RuntimeState,
) -> PreparedNlContext {
    let os = detect_os();

    let scrubbed_env_hints =
        crate::llm::scrub_env_values(&req.env_hints, &state.config.security.scrub_env_keys);

    let query = req.query.clone();
    let cwd = req.cwd.clone();
    let env_hints = scrubbed_env_hints;
    let project_root_cache = state.project_root_cache.clone();
    let project_type_cache = state.project_type_cache.clone();
    let tools_cache = state.tools_cache.clone();

    let scan_depth = state.config.spec.scan_depth;
    let cwd_for_cache = cwd.clone();
    let env_hints_for_cache = env_hints.clone();

    let cwd_path = std::path::PathBuf::from(&cwd);
    let cwd_for_readdir = cwd_path.clone();
    let cwd_for_git = cwd_path.clone();

    let (project_root, available_tools, git_branch, cwd_entries) = tokio::join!(
        project_root_cache.get_with(cwd_for_cache, async {
            crate::project::find_project_root(std::path::Path::new(&cwd), scan_depth)
        }),
        tools_cache.get_with(
            env_hints_for_cache.get("PATH").cloned().unwrap_or_default(),
            async { extract_available_tools(&env_hints_for_cache) }
        ),
        async { crate::project::read_git_branch_for_path(&cwd_for_git) },
        async { read_cwd_entries(&cwd_for_readdir).await },
    );

    let project_type = match project_root.as_ref() {
        Some(root) => {
            let root = root.clone();
            project_type_cache
                .get_with(root.clone(), async {
                    crate::project::detect_project_type(&root)
                })
                .await
        }
        None => None,
    };
    let project_commands = extract_project_commands(state, &cwd_path).await;
    let relevant_specs = extract_relevant_specs(state, &query, &cwd_path).await;

    PreparedNlContext {
        context: NlTranslationContext {
            query,
            cwd,
            os,
            project_type,
            available_tools,
            recent_commands: req.recent_commands.clone(),
            git_branch,
            project_commands,
            cwd_entries,
            relevant_specs,
        },
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
    state: &RuntimeState,
    cwd: &std::path::Path,
) -> HashMap<String, Vec<String>> {
    let specs = state.spec_store.get_project_specs(cwd).await;
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
    state: &RuntimeState,
    query: &str,
    cwd: &std::path::Path,
) -> HashMap<String, Vec<String>> {
    let query_tokens: Vec<&str> = query.split_whitespace().collect();
    let all_names = state.spec_store.all_command_names(cwd).await;
    let mut result = HashMap::new();

    for name in &all_names {
        if query_tokens.iter().any(|t| t.eq_ignore_ascii_case(name)) {
            if let Some(spec) = state.spec_store.lookup(name, cwd).await {
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

pub(super) fn detect_os() -> String {
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
