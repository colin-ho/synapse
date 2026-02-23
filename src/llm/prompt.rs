use std::collections::HashMap;

pub struct NlTranslationContext {
    pub query: String,
    pub cwd: String,
    pub os: String,
    pub project_type: Option<String>,
    pub available_tools: Vec<String>,
    pub recent_commands: Vec<String>,
    pub git_branch: Option<String>,
    /// Project commands: e.g. {"make": ["build","test"], "npm run": ["dev","lint"]}
    pub project_commands: HashMap<String, Vec<String>>,
    /// Top-level entries in the working directory.
    pub cwd_entries: Vec<String>,
    /// Known flags for tools mentioned in the query.
    pub relevant_specs: HashMap<String, Vec<String>>,
}

pub struct NlTranslationItem {
    pub command: String,
    pub warning: Option<String>,
}

pub struct NlTranslationResult {
    pub items: Vec<NlTranslationItem>,
}

/// Build NL translation prompt as (system_message, user_message).
pub fn build_nl_prompt(
    ctx: &NlTranslationContext,
    cwd: &str,
    max_suggestions: usize,
) -> (String, String) {
    let system = if max_suggestions <= 1 {
        "You are a shell command generator. Convert the user's natural language request into a single shell command.\n\n\
         Rules:\n\
         - Return ONLY the shell command, nothing else\n\
         - Use tools available on the system (prefer common POSIX utilities)\n\
         - Use the working directory context (don't use absolute paths unless necessary)\n\
         - If the request is ambiguous, prefer the most common interpretation\n\
         - If the request requires multiple commands, chain them with && or |\n\
         - Never generate destructive commands (rm -rf /, dd, mkfs) without explicit safeguards\n\
         - For file operations, prefer relative paths from the working directory"
            .to_string()
    } else {
        format!(
            "You are a shell command generator. Convert the user's natural language request into {n} alternative shell commands, ranked from most likely to least likely.\n\n\
             Rules:\n\
             - Return up to {n} alternative commands, one per line, numbered 1. 2. 3. etc.\n\
             - Each line must contain ONLY the number and shell command (no explanations)\n\
             - Vary the approaches: use different tools, flags, or techniques for each alternative\n\
             - Rank from most likely correct interpretation to least likely\n\
             - Use tools available on the system (prefer common POSIX utilities)\n\
             - Use the working directory context (don't use absolute paths unless necessary)\n\
             - If the request requires multiple commands, chain them with && or |\n\
             - Never generate destructive commands (rm -rf /, dd, mkfs) without explicit safeguards\n\
             - For file operations, prefer relative paths from the working directory",
            n = max_suggestions,
        )
    };

    let mut user = String::with_capacity(1024);
    user.push_str("Environment:\n");
    user.push_str("- Shell: zsh\n");
    user.push_str(&format!("- OS: {}\n", ctx.os));
    user.push_str(&format!("- Working directory: {cwd}\n"));
    user.push_str(&format!(
        "- Project type: {}\n",
        ctx.project_type.as_deref().unwrap_or("unknown")
    ));

    if let Some(ref branch) = ctx.git_branch {
        user.push_str(&format!("- Git branch: {branch}\n"));
    }

    if ctx.available_tools.is_empty() {
        user.push_str("- Available tools: standard POSIX utilities\n");
    } else {
        user.push_str(&format!(
            "- Available tools: {}\n",
            ctx.available_tools.join(", ")
        ));
    }

    if !ctx.project_commands.is_empty() {
        user.push_str("- Project commands:\n");
        for (runner, commands) in &ctx.project_commands {
            let cmds: Vec<_> = commands.iter().take(10).cloned().collect();
            user.push_str(&format!("  {runner}: {}\n", cmds.join(", ")));
        }
    }

    if !ctx.cwd_entries.is_empty() {
        let entries: Vec<_> = ctx.cwd_entries.iter().take(50).cloned().collect();
        user.push_str(&format!("- Files in cwd: {}\n", entries.join(", ")));
    }

    if !ctx.relevant_specs.is_empty() {
        for (tool, flags) in &ctx.relevant_specs {
            let flags_str: Vec<_> = flags.iter().take(20).cloned().collect();
            user.push_str(&format!(
                "- Known flags for `{tool}`: {}\n",
                flags_str.join(", ")
            ));
        }
    }

    if ctx.recent_commands.is_empty() {
        user.push_str("- Recent commands: (none)\n");
    } else {
        user.push_str("- Recent commands:\n");
        for cmd in ctx.recent_commands.iter().take(5) {
            user.push_str(&format!("{cmd}\n"));
        }
    }

    user.push_str(&format!("\nUser request: {}", ctx.query));

    (system, user)
}
