use std::num::NonZeroUsize;
use std::sync::Arc;

use futures_util::SinkExt;

use crate::completion_context::Position;
use crate::protocol::{
    CommandExecutedReport, InteractionAction, InteractionReport, ListSuggestionsRequest, Request,
    Response, SuggestRequest, SuggestionListResponse, SuggestionResponse, SuggestionSource,
};
use crate::providers::{Provider, ProviderRequest, ProviderSuggestion, SuggestionProvider};

use super::state::{RuntimeState, SharedWriter};

pub(super) async fn handle_request(
    request: Request,
    state: &RuntimeState,
    writer: SharedWriter,
) -> Response {
    match request {
        Request::Suggest(req) => handle_suggest(req, state, writer).await,
        Request::ListSuggestions(req) => handle_list_suggestions(req, state).await,
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

async fn handle_suggest(
    req: SuggestRequest,
    state: &RuntimeState,
    writer: SharedWriter,
) -> Response {
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

    // Phase 2: Async LLM workflow prediction (if applicable).
    // Spawn in background — the LLM rate limiter ensures this won't write
    // before the Phase 1 response (LLM calls take 200ms+ minimum).
    if matches!(
        provider_request.completion().position,
        Position::CommandName
    ) && provider_request.buffer.len() <= 4
        && state.config.llm.workflow_prediction
        && !provider_request.recent_commands.is_empty()
    {
        if let Some(wp) = find_workflow_provider(&state.providers) {
            let session_id = req.session_id.clone();
            let should_spawn = {
                let mut inflight = state.workflow_llm_inflight.lock().await;
                inflight.insert(session_id.clone())
            };

            if should_spawn {
                let wp = wp.clone();
                let request = provider_request.clone();
                let max_len = state.config.general.max_suggestion_length;
                let session_manager = state.session_manager.clone();
                let inflight = state.workflow_llm_inflight.clone();
                tokio::spawn(async move {
                    if let Some(llm_suggestion) = wp.predict_with_llm(&request).await {
                        let is_latest_buffer = session_manager
                            .get_last_buffer(&request.session_id)
                            .await
                            .as_deref()
                            == Some(request.buffer.as_str());

                        if is_latest_buffer {
                            let mut text = llm_suggestion.text;
                            if text.len() > max_len {
                                text.truncate(max_len);
                            }
                            let update = Response::Update(SuggestionResponse {
                                text,
                                source: SuggestionSource::Workflow,
                                confidence: llm_suggestion.score.min(1.0),
                            });
                            let line = update.to_tsv();
                            let mut w = writer.lock().await;
                            let _ = w.send(line).await;
                        }
                    }

                    let mut guard = inflight.lock().await;
                    guard.remove(&session_id);
                });
            }
        }
    }

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
            // Get exit code and project type for richer workflow recording.
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
    while let Some(result) = task_set.join_next().await {
        match result {
            Ok(mut suggestions) => all_suggestions.append(&mut suggestions),
            Err(error) => tracing::debug!("Provider task failed: {error}"),
        }
    }

    all_suggestions
}
