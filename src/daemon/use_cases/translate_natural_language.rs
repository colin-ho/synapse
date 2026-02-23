use crate::protocol::{
    NaturalLanguageRequest, Response, SuggestionItem, SuggestionKind, SuggestionListResponse,
    SuggestionSource,
};

use super::super::state::RuntimeState;
use super::nl_context::prepare_nl_context;

const NL_SUGGESTION_CONFIDENCE: f64 = 0.95;

pub(crate) async fn translate_natural_language(
    req: NaturalLanguageRequest,
    state: &RuntimeState,
) -> Response {
    tracing::debug!(
        session = %req.session_id,
        query = %req.query,
        "NaturalLanguage request"
    );

    if !state.config.llm.natural_language {
        return Response::Error {
            message: "Natural language mode is disabled".into(),
        };
    }

    let llm_client = match &state.llm_client {
        Some(client) => client.clone(),
        None => {
            return Response::Error {
                message: "LLM client not configured (set llm.enabled and API key)".into(),
            };
        }
    };

    if req.query.len() < crate::config::NL_MIN_QUERY_LENGTH {
        return Response::Error {
            message: format!(
                "Natural language query too short (minimum {} characters)",
                crate::config::NL_MIN_QUERY_LENGTH
            ),
        };
    }

    let prepared = prepare_nl_context(&req, state).await;

    let max_suggestions = state.config.llm.nl_max_suggestions;
    let temperature = if max_suggestions <= 1 {
        state.config.llm.temperature
    } else {
        state.config.llm.temperature_multi
    };

    let result = match llm_client
        .translate_command(&prepared.context, max_suggestions, temperature)
        .await
    {
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
            !first_token.is_empty() && !state.compiled_blocklist.is_blocked(&item.command)
        })
        .collect();

    if valid_items.is_empty() {
        return Response::Error {
            message: "All NL translations were empty or blocked by security policy".into(),
        };
    }

    let suggestions = suggestion_items_from_pairs(
        valid_items
            .into_iter()
            .map(|item| (item.command, item.warning)),
    );
    Response::SuggestionList(SuggestionListResponse { suggestions })
}

fn suggestion_items_from_pairs<I>(items: I) -> Vec<SuggestionItem>
where
    I: IntoIterator<Item = (String, Option<String>)>,
{
    items
        .into_iter()
        .map(|(text, description)| SuggestionItem {
            text,
            source: SuggestionSource::Llm,
            confidence: NL_SUGGESTION_CONFIDENCE,
            description,
            kind: SuggestionKind::Command,
        })
        .collect()
}
