use crate::protocol::{
    InteractionAction, InteractionReport, ListSuggestionsRequest, Request, Response,
    SuggestRequest, SuggestionListResponse, SuggestionResponse, SuggestionSource,
};
use crate::providers::{Provider, ProviderRequest, ProviderSuggestion, SuggestionProvider};

use super::state::{RuntimeState, SharedWriter};

pub(super) async fn handle_request(
    request: Request,
    state: &RuntimeState,
    _writer: SharedWriter,
) -> Response {
    match request {
        Request::Suggest(req) => handle_suggest(req, state).await,
        Request::ListSuggestions(req) => handle_list_suggestions(req, state).await,
        Request::Interaction(report) => handle_interaction(report, state).await,
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

    let provider_request = ProviderRequest::from_suggest_request(&req, &state.spec_store).await;

    // Phase 1: Immediate - query all providers concurrently.
    let suggestions = collect_provider_suggestions(&state.providers, &provider_request, 1).await;

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
    let provider_request = ProviderRequest::from_list_request(&req, &state.spec_store).await;
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
    );

    Response::Ack
}

async fn collect_provider_suggestions(
    providers: &[Provider],
    request: &ProviderRequest,
    max: usize,
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
