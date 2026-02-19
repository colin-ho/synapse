use std::collections::HashMap;
use std::path::Path;

use crate::protocol::{
    CommandExecutedReport, CompleteRequest, CompleteResultItem, CompleteResultResponse,
    CwdChangedReport, NaturalLanguageRequest, Request, Response, RunGeneratorRequest,
    SuggestionItem, SuggestionKind, SuggestionListResponse, SuggestionSource,
};

use super::state::{RuntimeState, SharedWriter};

pub(super) async fn handle_request(
    request: Request,
    state: &RuntimeState,
    writer: SharedWriter,
) -> Response {
    match request {
        Request::NaturalLanguage(req) => handle_natural_language(req, state, writer).await,
        Request::CommandExecuted(report) => handle_command_executed(report, state).await,
        Request::CwdChanged(report) => handle_cwd_changed(report, state).await,
        Request::Complete(req) => handle_complete(req, state).await,
        Request::RunGenerator(req) => handle_run_generator(req, state).await,
        Request::Ping => {
            tracing::trace!("Ping");
            Response::Pong
        }
        Request::Shutdown => {
            tracing::info!("Shutdown requested");
            if let Some(ref token) = state.shutdown_token {
                token.cancel();
            }
            Response::Ack
        }
        Request::ReloadConfig => {
            tracing::info!("Config reload requested");
            let _new_config = crate::config::Config::load();
            tracing::info!("Config reloaded successfully");
            Response::Ack
        }
        Request::ClearCache => {
            tracing::info!("Cache clear requested");
            state.project_root_cache.invalidate_all();
            state.project_type_cache.invalidate_all();
            state.tools_cache.invalidate_all();
            state.nl_cache.invalidate_all().await;
            state.spec_store.clear_caches().await;
            tracing::info!("All caches cleared");
            Response::Ack
        }
    }
}

async fn handle_command_executed(report: CommandExecutedReport, state: &RuntimeState) -> Response {
    tracing::debug!(
        session = %report.session_id,
        command = %report.command,
        "Command executed"
    );

    // Trigger spec discovery for the command name (first token)
    let command_name = report.command.split_whitespace().next().unwrap_or("");
    if !command_name.is_empty() {
        let cwd = if report.cwd.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(&report.cwd))
        };
        state
            .spec_store
            .trigger_discovery(command_name, cwd.as_deref())
            .await;
    }

    Response::Ack
}

async fn handle_cwd_changed(report: CwdChangedReport, state: &RuntimeState) -> Response {
    tracing::debug!(
        session = %report.session_id,
        cwd = %report.cwd,
        "CwdChanged"
    );

    // Pre-warm the project spec cache (and auto-write compsys files if enabled).
    // Fire-and-forget: the lookup populates the cache as a side effect.
    if !report.cwd.is_empty() {
        let spec_store = state.spec_store.clone();
        let cwd = std::path::PathBuf::from(&report.cwd);
        tokio::spawn(async move {
            let _ = spec_store.lookup_all_project_specs(&cwd).await;
        });
    }

    Response::Ack
}

async fn handle_complete(req: CompleteRequest, state: &RuntimeState) -> Response {
    tracing::debug!(
        command = %req.command,
        context = ?req.context,
        cwd = %req.cwd,
        "Complete request"
    );

    let cwd = if req.cwd.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&req.cwd))
    };
    let cwd_ref = cwd.as_deref();

    // Look up the spec for the command
    let spec = match state
        .spec_store
        .lookup(&req.command, cwd_ref.unwrap_or(Path::new("/")))
        .await
    {
        Some(spec) => spec,
        None => {
            return Response::CompleteResult(CompleteResultResponse { values: Vec::new() });
        }
    };

    // Walk the subcommand path using the context
    let mut current_options = &spec.options;
    let mut current_args = &spec.args;
    let mut current_subs = &spec.subcommands;

    for ctx_part in &req.context {
        if ctx_part == "target" || ctx_part == "subcommand" {
            // Return subcommand names
            let values = current_subs
                .iter()
                .map(|s| CompleteResultItem {
                    value: s.name.clone(),
                    description: s.description.clone(),
                })
                .collect();
            return Response::CompleteResult(CompleteResultResponse { values });
        }

        // Try to find a matching subcommand to walk into
        if let Some(sub) = current_subs
            .iter()
            .find(|s| s.name == *ctx_part || s.aliases.iter().any(|a| a == ctx_part))
        {
            current_options = &sub.options;
            current_args = &sub.args;
            current_subs = &sub.subcommands;
        }
    }

    // If there are subcommands at current level, return them
    if !current_subs.is_empty() {
        let values = current_subs
            .iter()
            .map(|s| CompleteResultItem {
                value: s.name.clone(),
                description: s.description.clone(),
            })
            .collect();
        return Response::CompleteResult(CompleteResultResponse { values });
    }

    // Otherwise return arg completions
    let mut values = Vec::new();

    // Add option completions
    for opt in current_options {
        if let Some(ref long) = opt.long {
            values.push(CompleteResultItem {
                value: long.clone(),
                description: opt.description.clone(),
            });
        }
        if let Some(ref short) = opt.short {
            values.push(CompleteResultItem {
                value: short.clone(),
                description: opt.description.clone(),
            });
        }
    }

    // Run generators for args
    for arg in current_args {
        if let Some(ref generator) = arg.generator {
            let gen_values = state
                .spec_store
                .run_generator(generator, cwd_ref.unwrap_or(Path::new("/")), spec.source)
                .await;
            for val in gen_values {
                values.push(CompleteResultItem {
                    value: val,
                    description: None,
                });
            }
        } else if !arg.suggestions.is_empty() {
            for s in &arg.suggestions {
                values.push(CompleteResultItem {
                    value: s.clone(),
                    description: None,
                });
            }
        }
    }

    Response::CompleteResult(CompleteResultResponse { values })
}

async fn handle_run_generator(req: RunGeneratorRequest, state: &RuntimeState) -> Response {
    tracing::debug!(
        command = %req.command,
        cwd = %req.cwd,
        "RunGenerator request"
    );

    let cwd = if req.cwd.is_empty() {
        std::path::PathBuf::from("/")
    } else {
        std::path::PathBuf::from(&req.cwd)
    };

    let generator = crate::spec::GeneratorSpec {
        command: req.command,
        split_on: req.split_on.unwrap_or_else(|| "\n".to_string()),
        strip_prefix: req.strip_prefix,
        ..Default::default()
    };

    let values = state
        .spec_store
        .run_generator(&generator, &cwd, crate::spec::SpecSource::Discovered)
        .await;

    let items = values
        .into_iter()
        .map(|v| CompleteResultItem {
            value: v,
            description: None,
        })
        .collect();

    Response::CompleteResult(CompleteResultResponse { values: items })
}

async fn handle_natural_language(
    req: NaturalLanguageRequest,
    state: &RuntimeState,
    _writer: SharedWriter,
) -> Response {
    tracing::debug!(
        session = %req.session_id,
        query = %req.query,
        "NaturalLanguage request"
    );

    // Check if NL is enabled
    if !state.config.llm.natural_language {
        return Response::Error {
            message: "Natural language mode is disabled".into(),
        };
    }

    // Check if LLM client is available
    let llm_client = match &state.llm_client {
        Some(client) => client.clone(),
        None => {
            return Response::Error {
                message: "LLM client not configured (set llm.enabled and API key)".into(),
            };
        }
    };

    // Check minimum query length
    if req.query.len() < crate::config::NL_MIN_QUERY_LENGTH {
        return Response::Error {
            message: format!(
                "Natural language query too short (minimum {} characters)",
                crate::config::NL_MIN_QUERY_LENGTH
            ),
        };
    }

    let os = detect_os();

    // Scrub sensitive env var values before passing to LLM context
    let scrubbed_env_hints: std::collections::HashMap<String, String> =
        crate::llm::scrub_env_values(&req.env_hints, &state.config.security.scrub_env_keys);

    // Check cache first
    if let Some(cached) = state.nl_cache.get(&req.query, &req.cwd, &os).await {
        let suggestions = cached
            .items
            .into_iter()
            .map(|item| SuggestionItem {
                text: item.command,
                source: SuggestionSource::Llm,
                confidence: 0.95,
                description: item.warning,
                kind: SuggestionKind::Command,
            })
            .collect();
        return Response::SuggestionList(SuggestionListResponse { suggestions });
    }

    let query = req.query.clone();
    let cwd = req.cwd.clone();
    let env_hints = scrubbed_env_hints;
    let project_root_cache = state.project_root_cache.clone();
    let project_type_cache = state.project_type_cache.clone();
    let tools_cache = state.tools_cache.clone();

    let scan_depth = state.config.spec.scan_depth;
    let cwd_for_cache = cwd.clone();
    let env_hints_for_cache = env_hints.clone();

    let (project_root, available_tools) = tokio::join!(
        project_root_cache.get_with(cwd_for_cache, async {
            crate::project::find_project_root(std::path::Path::new(&cwd), scan_depth)
        }),
        tools_cache.get_with(
            env_hints_for_cache.get("PATH").cloned().unwrap_or_default(),
            async { extract_available_tools(&env_hints_for_cache) }
        ),
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

    let ctx = crate::llm::NlTranslationContext {
        query: query.clone(),
        cwd: cwd.clone(),
        os: os.clone(),
        project_type,
        available_tools,
        recent_commands: req.recent_commands.clone(),
    };

    let max_suggestions = state.config.llm.nl_max_suggestions;
    let compiled_blocklist =
        super::state::CompiledBlocklist::new(&state.config.security.command_blocklist);

    let result = match llm_client.translate_command(&ctx, max_suggestions).await {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!("NL translation failed: {e}");
            return Response::Error {
                message: format!("Natural language translation failed: {e}"),
            };
        }
    };

    let valid_items: Vec<_> = result
        .items
        .into_iter()
        .filter(|item| {
            let first_token = item.command.split_whitespace().next().unwrap_or("");
            !first_token.is_empty() && !compiled_blocklist.is_blocked(&item.command)
        })
        .collect();

    if valid_items.is_empty() {
        return Response::Error {
            message: "All NL translations were empty or blocked by security policy".into(),
        };
    }

    state
        .nl_cache
        .insert(
            &query,
            &cwd,
            &os,
            crate::nl_cache::NlCacheEntry {
                items: valid_items
                    .iter()
                    .map(|item| crate::nl_cache::NlCacheItem {
                        command: item.command.clone(),
                        warning: item.warning.clone(),
                    })
                    .collect(),
            },
        )
        .await;

    if let Some(first) = valid_items.first() {
        state.interaction_logger.log_interaction(
            &req.session_id,
            crate::protocol::InteractionAction::Accept,
            &format!("? {query}"),
            &first.command,
            SuggestionSource::Llm,
            0.95,
            &cwd,
            Some(&query),
        );
    }

    let suggestions = valid_items
        .into_iter()
        .map(|item| SuggestionItem {
            text: item.command,
            source: SuggestionSource::Llm,
            confidence: 0.95,
            description: item.warning,
            kind: SuggestionKind::Command,
        })
        .collect();

    Response::SuggestionList(SuggestionListResponse { suggestions })
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
        "git", "cargo", "npm", "yarn", "pnpm", "docker", "kubectl", "python", "python3", "pip",
        "node", "go", "rustc", "java", "make", "cmake", "just", "brew", "ffmpeg", "jq", "rg", "fd",
        "bat", "eza", "fzf", "tmux",
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use futures_util::StreamExt;
    use tokio_util::codec::{Framed, LinesCodec};

    use super::super::state::CompiledBlocklist;
    use super::{detect_os, handle_natural_language, RuntimeState, SharedWriter};
    use crate::config::Config;
    use crate::logging::InteractionLogger;
    use crate::nl_cache::{NlCache, NlCacheEntry, NlCacheItem};
    use crate::protocol::{NaturalLanguageRequest, Response};
    use crate::session::SessionManager;
    use crate::spec_store::SpecStore;

    const TEST_NL_MIN_QUERY_LENGTH: usize = crate::config::NL_MIN_QUERY_LENGTH;

    fn test_runtime_state(config: Config) -> RuntimeState {
        let llm_client =
            crate::llm::LlmClient::from_config(&config.llm, config.security.scrub_paths)
                .map(Arc::new);
        let log_path = std::env::temp_dir().join(format!(
            "synapse-handlers-test-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        RuntimeState::new(
            Arc::new(SpecStore::new(config.spec.clone(), llm_client.clone())),
            SessionManager::new(),
            InteractionLogger::new(log_path, 1),
            config,
            llm_client,
            NlCache::new(),
        )
    }

    fn test_shared_writer() -> SharedWriter {
        let (stream, _) = tokio::net::UnixStream::pair().expect("UnixStream::pair");
        let (sink, _) = Framed::new(stream, LinesCodec::new()).split();
        Arc::new(tokio::sync::Mutex::new(sink))
    }

    #[test]
    fn test_block_pattern_plain_substring() {
        let bl = CompiledBlocklist::new(&[r#"curl -H "Authorization*"#.to_string()]);
        assert!(bl.is_blocked(r#"curl -H "Authorization: Bearer x" https://example.com"#,));
    }

    #[test]
    fn test_block_pattern_wildcard_export_assignment() {
        let bl = CompiledBlocklist::new(&["export *=".to_string()]);
        assert!(bl.is_blocked("export API_KEY=secret"));
    }

    #[test]
    fn test_blocked_command_with_mixed_patterns() {
        let patterns = vec![
            "export *=".to_string(),
            r#"curl -H "Authorization*"#.to_string(),
        ];
        let bl = CompiledBlocklist::new(&patterns);
        assert!(bl.is_blocked("export TOKEN=abc"));
        assert!(bl.is_blocked(r#"curl -H "Authorization: Bearer abc" https://example.com"#,));
        assert!(!bl.is_blocked("echo hello"));
    }

    #[tokio::test]
    async fn test_nl_short_query_returns_error() {
        let mut config = Config::default();
        config.llm.enabled = true;
        config.llm.base_url = Some("http://127.0.0.1:1".to_string());
        config.llm.timeout_ms = 100;
        let state = test_runtime_state(config.clone());
        let writer = test_shared_writer();

        assert!(TEST_NL_MIN_QUERY_LENGTH > 4, "test assumes min length > 4");
        let resp = handle_natural_language(
            NaturalLanguageRequest {
                session_id: "sess-short".to_string(),
                query: "tiny".to_string(),
                cwd: "/tmp".to_string(),
                recent_commands: Vec::new(),
                env_hints: HashMap::new(),
            },
            &state,
            writer,
        )
        .await;

        match resp {
            Response::Error { message } => {
                assert!(
                    message.contains("query too short"),
                    "unexpected error message: {message}"
                );
            }
            other => panic!("expected error response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_nl_cache_hit_returns_suggestions() {
        let mut config = Config::default();
        config.llm.enabled = true;
        config.llm.base_url = Some("http://127.0.0.1:1".to_string());
        config.llm.timeout_ms = 100;
        let state = test_runtime_state(config);
        let writer = test_shared_writer();

        let query = "find rust files".to_string();
        let cwd = "/tmp".to_string();
        let os = detect_os();
        state
            .nl_cache
            .insert(
                &query,
                &cwd,
                &os,
                NlCacheEntry {
                    items: vec![
                        NlCacheItem {
                            command: "fd -e rs".to_string(),
                            warning: None,
                        },
                        NlCacheItem {
                            command: "find . -name '*.rs'".to_string(),
                            warning: None,
                        },
                    ],
                },
            )
            .await;

        let resp = handle_natural_language(
            NaturalLanguageRequest {
                session_id: "sess-cache".to_string(),
                query,
                cwd,
                recent_commands: Vec::new(),
                env_hints: HashMap::new(),
            },
            &state,
            writer,
        )
        .await;

        let list = match resp {
            Response::SuggestionList(list) => list,
            other => panic!("expected suggestion list response, got: {other:?}"),
        };
        assert_eq!(list.suggestions.len(), 2);
        assert_eq!(list.suggestions[0].text, "fd -e rs");
        assert_eq!(list.suggestions[1].text, "find . -name '*.rs'");
        assert!(list
            .suggestions
            .iter()
            .all(|s| s.source == crate::protocol::SuggestionSource::Llm));
    }

    #[tokio::test]
    async fn test_nl_llm_failure_returns_error() {
        let mut config = Config::default();
        config.llm.enabled = true;
        config.llm.base_url = Some("http://127.0.0.1:1".to_string());
        config.llm.timeout_ms = 100;
        let state = test_runtime_state(config);
        let writer = test_shared_writer();

        let resp = handle_natural_language(
            NaturalLanguageRequest {
                session_id: "sess-fail".to_string(),
                query: "show me git status in porcelain mode".to_string(),
                cwd: "/tmp".to_string(),
                recent_commands: Vec::new(),
                env_hints: HashMap::new(),
            },
            &state,
            writer,
        )
        .await;
        match resp {
            Response::Error { message } => assert!(
                message.contains("Natural language translation failed"),
                "unexpected error message: {message}"
            ),
            other => panic!("expected error response, got: {other:?}"),
        }
    }
}
