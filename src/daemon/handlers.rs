use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use futures_util::SinkExt;

use crate::protocol::{
    CommandExecutedReport, InteractionAction, InteractionReport, ListSuggestionsRequest,
    NaturalLanguageRequest, Request, Response, SuggestRequest, SuggestionListResponse,
    SuggestionResponse, SuggestionSource,
};
use crate::providers::{Provider, ProviderRequest, ProviderSuggestion, SuggestionProvider};

use super::state::{RuntimeState, SharedWriter};

pub(super) struct SuggestHandling {
    pub(super) response: Response,
    pub(super) phase2_plan: Option<Phase2UpdatePlan>,
}

pub(super) struct Phase2UpdatePlan {
    provider_request: ProviderRequest,
    phase1_suggestions: Vec<ProviderSuggestion>,
    session_id: String,
    buffer_snapshot: String,
    baseline_score: f64,
    baseline_text: Option<String>,
    baseline_source: Option<SuggestionSource>,
}

pub(super) async fn handle_request(
    request: Request,
    state: &RuntimeState,
    writer: SharedWriter,
) -> Response {
    match request {
        Request::Suggest(req) => handle_suggest(req, state).await.response,
        Request::ListSuggestions(req) => handle_list_suggestions(req, state).await,
        Request::NaturalLanguage(req) => handle_natural_language(req, state, writer).await,
        Request::Interaction(report) => handle_interaction(report, state).await,
        Request::CommandExecuted(report) => handle_command_executed(report, state).await,
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

pub(super) async fn handle_suggest(req: SuggestRequest, state: &RuntimeState) -> SuggestHandling {
    tracing::debug!(
        session = %req.session_id,
        buffer = %req.buffer,
        "Suggest request"
    );

    state.session_manager.update_from_request(&req).await;

    let provider_request =
        ProviderRequest::from_suggest_request(&req, state.spec_store.clone()).await;

    // Phase 1: Immediate - query all providers concurrently.
    let phase1_suggestions = collect_provider_suggestions(
        &state.providers,
        &provider_request,
        NonZeroUsize::new(1).unwrap(),
    )
    .await;

    // Rank immediate results.
    let ranked = state.ranker.rank(
        phase1_suggestions.clone(),
        &provider_request.recent_commands,
        Some(provider_request.completion()),
    );

    let (response, baseline_score, baseline_text, baseline_source) = match ranked {
        Some(r) => {
            let mut text = r.text;
            if text.len() > crate::config::MAX_SUGGESTION_LENGTH {
                text.truncate(crate::config::MAX_SUGGESTION_LENGTH);
            }

            let resp = SuggestionResponse {
                text: text.clone(),
                source: r.source,
                confidence: r.score.min(1.0),
                description: None,
            };

            state
                .session_manager
                .record_suggestion(&req.session_id, resp.clone())
                .await;

            (
                Response::Suggestion(resp),
                r.score,
                Some(text),
                Some(r.source),
            )
        }
        None => (
            Response::Suggestion(SuggestionResponse {
                text: String::new(),
                source: SuggestionSource::History,
                confidence: 0.0,
                description: None,
            }),
            0.0,
            None,
            None,
        ),
    };

    let has_workflow_phase2 =
        state.config.llm.workflow_prediction && find_workflow_provider(&state.providers).is_some();

    let phase2_plan = if state.phase2_providers.is_empty() && !has_workflow_phase2 {
        None
    } else {
        Some(Phase2UpdatePlan {
            provider_request,
            phase1_suggestions,
            session_id: req.session_id.clone(),
            buffer_snapshot: req.buffer.clone(),
            baseline_score,
            baseline_text,
            baseline_source,
        })
    };

    SuggestHandling {
        response,
        phase2_plan,
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

pub(super) fn spawn_phase2_update(
    plan: Phase2UpdatePlan,
    state: &RuntimeState,
    writer: SharedWriter,
) {
    let phase2_providers = state.phase2_providers.clone();
    let workflow_provider = if state.config.llm.workflow_prediction {
        find_workflow_provider(&state.providers).cloned()
    } else {
        None
    };

    if phase2_providers.is_empty() && workflow_provider.is_none() {
        return;
    }

    let ranker = state.ranker.clone();
    let session_manager = state.session_manager.clone();
    let workflow_llm_inflight = state.workflow_llm_inflight.clone();

    let Phase2UpdatePlan {
        provider_request,
        mut phase1_suggestions,
        session_id,
        buffer_snapshot,
        baseline_score,
        baseline_text,
        baseline_source,
    } = plan;

    tokio::spawn(async move {
        let mut phase2_suggestions = collect_provider_suggestions(
            &phase2_providers,
            &provider_request,
            NonZeroUsize::new(1).unwrap(),
        )
        .await;

        if let Some(wp) = workflow_provider {
            let should_predict = !provider_request.recent_commands.is_empty();
            if should_predict {
                let should_run = {
                    let mut inflight = workflow_llm_inflight.lock().await;
                    inflight.insert(session_id.clone())
                };

                if should_run {
                    if let Some(suggestion) = wp.predict_with_llm(&provider_request).await {
                        phase2_suggestions.push(suggestion);
                    }

                    let mut inflight = workflow_llm_inflight.lock().await;
                    inflight.remove(&session_id);
                }
            }
        }

        if phase2_suggestions.is_empty() {
            return;
        }

        phase1_suggestions.extend(phase2_suggestions);
        let Some(best) = ranker.rank(
            phase1_suggestions,
            &provider_request.recent_commands,
            Some(provider_request.completion()),
        ) else {
            return;
        };

        // Require meaningful improvement before pushing a visual update
        const PHASE2_MIN_MARGIN: f64 = 0.05;
        if best.score <= baseline_score + PHASE2_MIN_MARGIN {
            return;
        }
        if baseline_text.as_deref() == Some(best.text.as_str())
            && baseline_source == Some(best.source)
        {
            return;
        }

        if session_manager
            .get_last_buffer(&session_id)
            .await
            .as_deref()
            != Some(buffer_snapshot.as_str())
        {
            return;
        }

        let mut text = best.text;
        if text.len() > crate::config::MAX_SUGGESTION_LENGTH {
            text.truncate(crate::config::MAX_SUGGESTION_LENGTH);
        }

        let update = SuggestionResponse {
            text,
            source: best.source,
            confidence: best.score.min(1.0),
            description: best.description,
        };
        session_manager
            .record_suggestion(&session_id, update.clone())
            .await;

        let response_line = Response::Update(update).to_tsv();
        let mut sink = writer.lock().await;
        if let Err(error) = sink.send(response_line).await {
            tracing::debug!("Failed to send async update: {error}");
        }
    });
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
            let exit_code = state
                .session_manager
                .get_last_exit_code(&report.session_id)
                .await;
            let project_type = state
                .session_manager
                .get_cwd(&report.session_id)
                .await
                .and_then(|cwd| {
                    let path = std::path::Path::new(&cwd);
                    let root = crate::project::find_project_root(path, 3)?;
                    crate::project::detect_project_type(&root)
                });

            state
                .workflow_predictor
                .record_with_context(
                    &prev,
                    &report.suggestion,
                    exit_code,
                    project_type.as_deref(),
                )
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

    // Debounce generation: always bump first so older in-flight requests become stale,
    // even when this request is served from cache or rejected early.
    let nl_gens = state.nl_generations.clone();
    let gen = {
        let mut gens = nl_gens.lock().unwrap();
        let g = gens.entry(req.session_id.clone()).or_insert(0);
        *g += 1;
        *g
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

    // Check cache — send all cached results via writer, return Ack
    if let Some(cached) = state.nl_cache.get(&req.query, &req.cwd, &os).await {
        for item in &cached.items {
            send_async_response(
                &writer,
                Response::Update(SuggestionResponse {
                    text: item.command.clone(),
                    source: SuggestionSource::Llm,
                    confidence: 0.95,
                    description: item.warning.clone(),
                }),
                "NL cached update",
            )
            .await;
        }
        send_async_response(&writer, Response::SuggestDone, "NL cached done").await;
        return Response::Ack;
    }

    // Spawn async task for LLM call — return Ack immediately
    let nl_cache = state.nl_cache.clone();
    let config = state.config.clone();
    let interaction_logger = state.interaction_logger.clone();
    let session_id = req.session_id.clone();
    let query = req.query.clone();
    let cwd = req.cwd.clone();
    let env_hints = scrubbed_env_hints;
    let recent_commands = req.recent_commands.clone();

    let project_root_cache = state.project_root_cache.clone();
    let project_type_cache = state.project_type_cache.clone();
    let tools_cache = state.tools_cache.clone();

    tokio::spawn(async move {
        // Overlap debounce sleep with context detection (both can run in parallel)
        let debounce_ms = crate::config::NL_DEBOUNCE_MS;
        let scan_depth = config.spec.scan_depth;
        let cwd_for_cache = cwd.clone();
        let env_hints_for_cache = env_hints.clone();

        let (_, project_root, available_tools) = tokio::join!(
            tokio::time::sleep(std::time::Duration::from_millis(debounce_ms)),
            project_root_cache.get_with(cwd_for_cache, async {
                crate::project::find_project_root(std::path::Path::new(&cwd), scan_depth)
            }),
            tools_cache.get_with(
                env_hints_for_cache.get("PATH").cloned().unwrap_or_default(),
                async { extract_available_tools(&env_hints_for_cache) }
            ),
        );

        let is_stale = {
            let gens = nl_gens.lock().unwrap();
            gens.get(&session_id).copied().unwrap_or(0) != gen
        };
        if is_stale {
            tracing::debug!(session = %session_id, "NL debounce: skipping stale request");
            send_async_response(
                &writer,
                Response::Error {
                    message: "Natural language request superseded by a newer query".into(),
                },
                "NL stale error",
            )
            .await;
            send_async_response(&writer, Response::SuggestDone, "NL stale done").await;
            return;
        }

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
            recent_commands,
        };

        let max_suggestions = config.llm.nl_max_suggestions;
        let compiled_blocklist =
            super::state::CompiledBlocklist::new(&config.security.command_blocklist);
        let writer_for_stream = writer.clone();

        // Use streaming to send each suggestion as it's parsed from the LLM response
        let result = llm_client
            .translate_command_streaming(&ctx, max_suggestions, move |item| {
                // Validate: skip empty or blocked commands
                let first_token = item.command.split_whitespace().next().unwrap_or("");
                if first_token.is_empty() {
                    return;
                }
                if compiled_blocklist.is_blocked(&item.command) {
                    tracing::warn!(
                        "NL translation blocked by security policy: {}",
                        item.command
                    );
                    return;
                }

                // Send update frame immediately (fire-and-forget from sync callback)
                let w = writer_for_stream.clone();
                let response = Response::Update(SuggestionResponse {
                    text: item.command.clone(),
                    source: SuggestionSource::Llm,
                    confidence: 0.95,
                    description: item.warning.clone(),
                });
                tokio::spawn(async move {
                    send_async_response(&w, response, "NL streaming update").await;
                });
            })
            .await;

        match result {
            Ok(result) => {
                let final_blocklist =
                    super::state::CompiledBlocklist::new(&config.security.command_blocklist);
                let valid_items: Vec<_> = result
                    .items
                    .into_iter()
                    .filter(|item| {
                        let first_token = item.command.split_whitespace().next().unwrap_or("");
                        !first_token.is_empty() && !final_blocklist.is_blocked(&item.command)
                    })
                    .collect();

                if valid_items.is_empty() {
                    send_async_response(
                        &writer,
                        Response::Error {
                            message: "All NL translations were empty or blocked by security policy"
                                .into(),
                        },
                        "NL all-blocked error",
                    )
                    .await;
                    send_async_response(&writer, Response::SuggestDone, "NL blocked done").await;
                    return;
                }

                // Cache the valid results
                nl_cache
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

                // Log the first result as the primary NL interaction
                if let Some(first) = valid_items.first() {
                    interaction_logger.log_interaction(
                        &session_id,
                        crate::protocol::InteractionAction::Accept,
                        &format!("? {query}"),
                        &first.command,
                        SuggestionSource::Llm,
                        0.95,
                        &cwd,
                        Some(&query),
                    );
                }
            }
            Err(e) => {
                tracing::warn!("NL translation failed: {e}");
                send_async_response(
                    &writer,
                    Response::Error {
                        message: format!("Natural language translation failed: {e}"),
                    },
                    "NL translation error",
                )
                .await;
            }
        }
        // Signal to the plugin that all NL results have been sent
        send_async_response(&writer, Response::SuggestDone, "NL done").await;
    });

    Response::Ack
}

async fn send_async_response(writer: &SharedWriter, response: Response, context: &str) {
    let response_line = response.to_tsv();
    let mut w = writer.lock().await;
    if let Err(e) = w.send(response_line).await {
        tracing::debug!("Failed to send {context}: {e}");
    }
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

/// Extract the WorkflowProvider from the provider list.
fn find_workflow_provider(
    providers: &[Provider],
) -> Option<&Arc<crate::providers::workflow::WorkflowProvider>> {
    for provider in providers {
        if let Provider::Workflow(wp) = provider {
            return Some(wp);
        }
    }
    None
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
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);

    loop {
        match tokio::time::timeout_at(deadline, task_set.join_next()).await {
            Ok(Some(Ok(mut suggestions))) => all_suggestions.append(&mut suggestions),
            Ok(Some(Err(error))) => tracing::debug!("Provider task failed: {error}"),
            Ok(None) => break, // All tasks completed
            Err(_) => {
                tracing::debug!(
                    "Phase 1 timeout: returning {} suggestions from {} providers",
                    all_suggestions.len(),
                    providers.len()
                );
                break;
            }
        }
    }

    all_suggestions
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use tokio_util::codec::{Framed, LinesCodec};

    use super::super::state::CompiledBlocklist;
    use super::{detect_os, handle_natural_language, RuntimeState, SharedWriter};
    use crate::config::Config;
    use crate::logging::InteractionLogger;
    use crate::nl_cache::{NlCache, NlCacheEntry, NlCacheItem};
    use crate::protocol::{NaturalLanguageRequest, Response};
    use crate::ranking::Ranker;
    use crate::session::SessionManager;
    use crate::spec_store::SpecStore;
    use crate::workflow::WorkflowPredictor;

    // NL minimum query length is now a hardcoded constant, so we define a
    // local helper for the test that needs to exercise the "too short" path.
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
            Vec::new(),
            Vec::new(),
            Arc::new(SpecStore::new(config.spec.clone(), llm_client.clone())),
            Ranker::new(),
            Arc::new(WorkflowPredictor::new()),
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

        // Query must be shorter than NL_MIN_QUERY_LENGTH (5) to trigger the error
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
    async fn test_nl_cache_hit_still_bumps_generation() {
        let mut config = Config::default();
        config.llm.enabled = true;
        config.llm.base_url = Some("http://127.0.0.1:1".to_string());
        config.llm.timeout_ms = 100;
        let state = test_runtime_state(config);

        let (writer_stream, reader_stream) =
            tokio::net::UnixStream::pair().expect("UnixStream::pair");
        let (sink, _) = Framed::new(writer_stream, LinesCodec::new()).split();
        let writer: SharedWriter = Arc::new(tokio::sync::Mutex::new(sink));
        let mut reader = Framed::new(reader_stream, LinesCodec::new());

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

        // Cache hit now returns Ack; updates are sent via the writer
        assert!(matches!(resp, Response::Ack));

        // Read the two update frames
        let line1 = tokio::time::timeout(Duration::from_secs(2), reader.next())
            .await
            .expect("timed out")
            .expect("writer closed")
            .expect("read error");
        assert!(line1.starts_with("update\tfd -e rs\t"), "got: {line1}");

        let line2 = tokio::time::timeout(Duration::from_secs(2), reader.next())
            .await
            .expect("timed out")
            .expect("writer closed")
            .expect("read error");
        assert!(
            line2.starts_with("update\tfind . -name '*.rs'\t"),
            "got: {line2}"
        );

        let gens = state.nl_generations.lock().unwrap();
        assert_eq!(gens.get("sess-cache").copied(), Some(1));
    }

    #[tokio::test]
    async fn test_nl_llm_failure_sends_async_error() {
        let mut config = Config::default();
        config.llm.enabled = true;
        config.llm.base_url = Some("http://127.0.0.1:1".to_string());
        config.llm.timeout_ms = 100;
        let state = test_runtime_state(config);

        let (writer_stream, reader_stream) =
            tokio::net::UnixStream::pair().expect("UnixStream::pair");
        let (sink, _) = Framed::new(writer_stream, LinesCodec::new()).split();
        let writer: SharedWriter = Arc::new(tokio::sync::Mutex::new(sink));
        let mut reader = Framed::new(reader_stream, LinesCodec::new());

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
        assert!(matches!(resp, Response::Ack));

        let line = tokio::time::timeout(Duration::from_secs(5), reader.next())
            .await
            .expect("timed out waiting for async NL response")
            .expect("writer closed unexpectedly")
            .expect("failed to read async NL frame");

        assert!(
            line.starts_with("error\t"),
            "expected async error frame, got: {line}"
        );
        assert!(
            line.contains("Natural language translation failed"),
            "unexpected async error message: {line}"
        );
    }
}
