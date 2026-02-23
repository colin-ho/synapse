use crate::protocol::{CompleteResultItem, CompleteResultResponse, Response, RunGeneratorRequest};

use super::super::state::RuntimeState;

pub(crate) async fn run_generator(req: RunGeneratorRequest, state: &RuntimeState) -> Response {
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

    let values = state.spec_store.run_generator(&generator, &cwd).await;

    Response::CompleteResult(CompleteResultResponse {
        values: values
            .into_iter()
            .map(|value| CompleteResultItem {
                value,
                description: None,
            })
            .collect(),
    })
}
