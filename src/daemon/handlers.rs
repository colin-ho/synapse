use std::num::NonZeroUsize;

use futures_util::SinkExt;

use crate::protocol::{
    CommandExecutedReport, InteractionAction, InteractionReport, ListSuggestionsRequest, Request,
    Response, SuggestRequest, SuggestionListResponse, SuggestionResponse, SuggestionSource,
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

pub(super) async fn handle_request(request: Request, state: &RuntimeState) -> Response {
    match request {
        Request::Suggest(req) => handle_suggest(req, state).await.response,
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
            if text.len() > state.config.general.max_suggestion_length {
                text.truncate(state.config.general.max_suggestion_length);
            }

            let resp = SuggestionResponse {
                text: text.clone(),
                source: r.source,
                confidence: r.score.min(1.0),
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
            }),
            0.0,
            None,
            None,
        ),
    };

    let phase2_plan = if state.phase2_providers.is_empty() {
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
    if phase2_providers.is_empty() {
        return;
    }
    let ranker = state.ranker.clone();
    let session_manager = state.session_manager.clone();
    let max_suggestion_length = state.config.general.max_suggestion_length;
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
        let phase2_suggestions = collect_provider_suggestions(
            &phase2_providers,
            &provider_request,
            NonZeroUsize::new(1).unwrap(),
        )
        .await;
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

        if best.score <= baseline_score {
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
        if text.len() > max_suggestion_length {
            text.truncate(max_suggestion_length);
        }

        let update = SuggestionResponse {
            text,
            source: best.source,
            confidence: best.score.min(1.0),
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
