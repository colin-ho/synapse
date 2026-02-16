use std::collections::HashMap;
use std::num::NonZeroUsize;

use futures_util::SinkExt;

use crate::protocol::{
    CommandExecutedReport, ExplainRequest, InteractionAction, InteractionReport,
    ListSuggestionsRequest, NaturalLanguageRequest, Request, Response, SuggestRequest,
    SuggestionListResponse, SuggestionResponse, SuggestionSource,
};
use crate::providers::{Provider, ProviderRequest, ProviderSuggestion, SuggestionProvider};

use super::state::{RuntimeState, SharedWriter};

pub(super) async fn handle_request(
    request: Request,
    state: &RuntimeState,
    writer: SharedWriter,
) -> Response {
    match request {
        Request::Suggest(req) => handle_suggest(req, state).await,
        Request::ListSuggestions(req) => handle_list_suggestions(req, state).await,
        Request::NaturalLanguage(req) => handle_natural_language(req, state, writer).await,
        Request::Explain(req) => handle_explain(req, state).await,
        Request::Interaction(report) => handle_interaction(report, state).await,
        Request::CommandExecuted(report) => handle_command_executed(report, state).await,
        Request::Ping => {
            tracing::trace!("Ping");
            Response::Pong
        }
        Request::Shutdown => {
            tracing::info!("Shutdown requested");
            // Trigger graceful shutdown.
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                std::process::exit(0);
            });
            Response::Ack
        }
        Request::ReloadConfig => {
            tracing::info!("Config reload requested");
            // TODO: actually reload config
            Response::Ack
        }
        Request::ClearCache => {
            tracing::info!("Cache clear requested");
            // TODO: clear caches
            Response::Ack
        }
    }
}

async fn handle_suggest(req: SuggestRequest, state: &RuntimeState) -> Response {
    tracing::debug!(
        session = %req.session_id,
        buffer = %req.buffer,
        "Suggest request"
    );

    state.session_manager.update_from_request(&req).await;

    let provider_request =
        ProviderRequest::from_suggest_request(&req, state.spec_store.clone()).await;

    // Phase 1: Immediate - query all providers concurrently.
    let suggestions = collect_provider_suggestions(
        &state.providers,
        &provider_request,
        NonZeroUsize::new(1).unwrap(),
    )
    .await;

    // Rank immediate results.
    let ranked = state.ranker.rank(
        suggestions,
        &provider_request.recent_commands,
        Some(provider_request.completion()),
    );

    match ranked {
        Some(r) => {
            let mut text = r.text;
            if text.len() > state.config.general.max_suggestion_length {
                text.truncate(state.config.general.max_suggestion_length);
            }

            let resp = SuggestionResponse {
                text,
                source: r.source,
                confidence: r.score.min(1.0),
                description: None,
            };

            state
                .session_manager
                .record_suggestion(&req.session_id, resp.clone())
                .await;

            Response::Suggestion(resp)
        }
        None => Response::Suggestion(SuggestionResponse {
            text: String::new(),
            source: SuggestionSource::History,
            confidence: 0.0,
            description: None,
        }),
    }
}

async fn handle_list_suggestions(req: ListSuggestionsRequest, state: &RuntimeState) -> Response {
    tracing::debug!(
        session = %req.session_id,
        buffer = %req.buffer,
        max_results = req.max_results,
        "ListSuggestions request"
    );

    let max = req.max_results.min(state.config.spec.max_list_results);
    let provider_request = ProviderRequest::from_list_request(&req, state.spec_store.clone()).await;
    let all_suggestions =
        collect_provider_suggestions(&state.providers, &provider_request, max).await;

    let ranked = state.ranker.rank_multi(
        all_suggestions,
        &provider_request.recent_commands,
        max,
        Some(provider_request.completion()),
    );

    let items = ranked.iter().map(|r| r.to_suggestion_item()).collect();

    Response::SuggestionList(SuggestionListResponse { suggestions: items })
}

async fn handle_interaction(report: InteractionReport, state: &RuntimeState) -> Response {
    tracing::debug!(
        session = %report.session_id,
        action = ?report.action,
        "Interaction report"
    );

    // Record workflow transition on Accept.
    if report.action == InteractionAction::Accept {
        if let Some(prev) = state
            .session_manager
            .get_last_accepted(&report.session_id)
            .await
        {
            state
                .workflow_predictor
                .record(&prev, &report.suggestion)
                .await;
        }
        state
            .session_manager
            .record_accepted(&report.session_id, report.suggestion.clone())
            .await;
    }

    state.interaction_logger.log_interaction(
        &report.session_id,
        report.action,
        &report.buffer_at_action,
        &report.suggestion,
        report.source,
        0.0,
        "",
        None,
    );

    Response::Ack
}

async fn handle_command_executed(report: CommandExecutedReport, state: &RuntimeState) -> Response {
    tracing::debug!(
        session = %report.session_id,
        command = %report.command,
        "Command executed"
    );

    for provider in &state.providers {
        if let Provider::History(hp) = provider {
            hp.record_command(&report.command).await;
            break;
        }
    }

    // Trigger spec discovery for the command name (first token)
    let command_name = report.command.split_whitespace().next().unwrap_or("");
    if !command_name.is_empty() {
        let cwd = state.session_manager.get_cwd(&report.session_id).await;
        state
            .spec_store
            .trigger_discovery(command_name, cwd.as_deref().map(std::path::Path::new))
            .await;
    }

    Response::Ack
}

async fn handle_natural_language(
    req: NaturalLanguageRequest,
    state: &RuntimeState,
    writer: SharedWriter,
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
    if req.query.len() < state.config.llm.nl_min_query_length {
        return Response::Ack;
    }

    let os = detect_os();

    // Check cache — return immediately if cached
    if let Some(cached) = state.nl_cache.get(&req.query, &req.cwd, &os).await {
        return Response::Update(SuggestionResponse {
            text: cached.command,
            source: SuggestionSource::Llm,
            confidence: 0.95,
            description: cached.warning,
        });
    }

    // Spawn async task for LLM call — return Ack immediately
    let nl_cache = state.nl_cache.clone();
    let config = state.config.clone();
    let interaction_logger = state.interaction_logger.clone();
    let session_id = req.session_id.clone();
    let query = req.query.clone();
    let cwd = req.cwd.clone();
    let env_hints = req.env_hints.clone();
    let recent_commands = req.recent_commands.clone();

    tokio::spawn(async move {
        // Detect project type
        let project_root =
            crate::project::find_project_root(std::path::Path::new(&cwd), config.spec.scan_depth);
        let project_type = project_root
            .as_ref()
            .and_then(|r| crate::project::detect_project_type(r));

        let available_tools = extract_available_tools(&env_hints);

        let ctx = crate::llm::NlTranslationContext {
            query: query.clone(),
            cwd: cwd.clone(),
            os: os.clone(),
            project_type,
            available_tools,
            recent_commands,
        };

        match llm_client.translate_command(&ctx).await {
            Ok(result) => {
                // Validate: first token must be non-empty
                let first_token = result.command.split_whitespace().next().unwrap_or("");
                if first_token.is_empty() {
                    return;
                }

                // Check against security blocklist
                if is_blocked_command(&result.command, &config.security.command_blocklist) {
                    tracing::warn!(
                        "NL translation blocked by security policy: {}",
                        result.command
                    );
                    return;
                }

                // Cache the result
                nl_cache
                    .insert(
                        &query,
                        &cwd,
                        &os,
                        crate::nl_cache::NlCacheEntry {
                            command: result.command.clone(),
                            warning: result.warning.clone(),
                        },
                    )
                    .await;

                // Log the NL interaction
                interaction_logger.log_interaction(
                    &session_id,
                    crate::protocol::InteractionAction::Accept,
                    &format!("? {query}"),
                    &result.command,
                    SuggestionSource::Llm,
                    0.95,
                    &cwd,
                    Some(&query),
                );

                // Send Update response via the writer
                let response = Response::Update(SuggestionResponse {
                    text: result.command,
                    source: SuggestionSource::Llm,
                    confidence: 0.95,
                    description: result.warning,
                });
                let response_line = response.to_tsv();
                let mut w = writer.lock().await;
                if let Err(e) = w.send(response_line).await {
                    tracing::debug!("Failed to send NL update: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("NL translation failed: {e}");
            }
        }
    });

    Response::Ack
}

async fn handle_explain(req: ExplainRequest, state: &RuntimeState) -> Response {
    tracing::debug!(
        session = %req.session_id,
        command = %req.command,
        "Explain request"
    );

    let llm_client = match &state.llm_client {
        Some(client) => client,
        None => {
            return Response::Error {
                message: "LLM client not configured".into(),
            };
        }
    };

    match llm_client.explain_command(&req.command).await {
        Ok(explanation) => Response::Suggestion(SuggestionResponse {
            text: explanation,
            source: SuggestionSource::Llm,
            confidence: 1.0,
            description: None,
        }),
        Err(e) => {
            tracing::warn!("Explain failed: {e}");
            Response::Error {
                message: format!("Explanation failed: {e}"),
            }
        }
    }
}

fn detect_os() -> String {
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

fn is_blocked_command(command: &str, blocklist: &[String]) -> bool {
    blocklist
        .iter()
        .any(|pattern| command_matches_block_pattern(command, pattern))
}

fn command_matches_block_pattern(command: &str, pattern: &str) -> bool {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return false;
    }

    // Backward-compatible behavior for plain substring patterns.
    if !trimmed.contains('*') && !trimmed.contains('?') {
        return command.contains(trimmed);
    }

    // Wildcard support for security patterns:
    // '*' -> any span, '?' -> any single character.
    let regex_pattern = regex::escape(trimmed)
        .replace(r"\*", ".*")
        .replace(r"\?", ".");

    match regex::Regex::new(&regex_pattern) {
        Ok(re) => re.is_match(command),
        Err(_) => command.contains(trimmed),
    }
}

async fn collect_provider_suggestions(
    providers: &[Provider],
    request: &ProviderRequest,
    max: NonZeroUsize,
) -> Vec<ProviderSuggestion> {
    let mut task_set = tokio::task::JoinSet::new();

    for provider in providers {
        let provider = provider.clone();
        let request = request.clone();
        task_set.spawn(async move { provider.suggest(&request, max).await });
    }

    let mut all_suggestions = Vec::new();
    while let Some(result) = task_set.join_next().await {
        match result {
            Ok(mut suggestions) => all_suggestions.append(&mut suggestions),
            Err(error) => tracing::debug!("Provider task failed: {error}"),
        }
    }

    all_suggestions
}

#[cfg(test)]
mod tests {
    use super::{command_matches_block_pattern, is_blocked_command};

    #[test]
    fn test_block_pattern_plain_substring() {
        assert!(command_matches_block_pattern(
            r#"curl -H "Authorization: Bearer x" https://example.com"#,
            r#"curl -H "Authorization*"#,
        ));
    }

    #[test]
    fn test_block_pattern_wildcard_export_assignment() {
        assert!(command_matches_block_pattern(
            "export API_KEY=secret",
            "export *=",
        ));
    }

    #[test]
    fn test_blocked_command_with_mixed_patterns() {
        let patterns = vec![
            "export *=".to_string(),
            r#"curl -H "Authorization*"#.to_string(),
        ];
        assert!(is_blocked_command("export TOKEN=abc", &patterns));
        assert!(is_blocked_command(
            r#"curl -H "Authorization: Bearer abc" https://example.com"#,
            &patterns
        ));
        assert!(!is_blocked_command("echo hello", &patterns));
    }
}
