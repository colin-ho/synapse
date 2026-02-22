use std::path::Path;

use crate::protocol::{CompleteRequest, CompleteResultItem, CompleteResultResponse, Response};

use super::super::state::RuntimeState;

pub(crate) async fn complete_command(req: CompleteRequest, state: &RuntimeState) -> Response {
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

    // Look up the spec for the command.
    // Unknown commands return empty results — use `synapse add` to add specs.
    let lookup_cwd = cwd_ref.unwrap_or(Path::new("/"));
    let spec = match state.spec_store.lookup(&req.command, lookup_cwd).await {
        Some(spec) => spec,
        None => {
            return Response::CompleteResult(CompleteResultResponse { values: Vec::new() });
        }
    };

    // Walk the subcommand path using the context.
    let mut current_options = &spec.options;
    let mut current_args = &spec.args;
    let mut current_subs = &spec.subcommands;

    for ctx_part in &req.context {
        if ctx_part == "target" || ctx_part == "subcommand" {
            let values = current_subs
                .iter()
                .map(|s| CompleteResultItem {
                    value: s.name.clone(),
                    description: s.description.clone(),
                })
                .collect();
            return Response::CompleteResult(CompleteResultResponse { values });
        }

        if let Some(sub) = current_subs
            .iter()
            .find(|s| s.name == *ctx_part || s.aliases.iter().any(|a| a == ctx_part))
        {
            current_options = &sub.options;
            current_args = &sub.args;
            current_subs = &sub.subcommands;
        }
    }

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

    let mut values = Vec::new();

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

    for arg in current_args {
        if let Some(ref generator) = arg.generator {
            let gen_values = state
                .spec_store
                .run_generator(generator, cwd_ref.unwrap_or(Path::new("/")), spec.source)
                .await;
            values.extend(gen_values.into_iter().map(|value| CompleteResultItem {
                value,
                description: None,
            }));
        } else if !arg.suggestions.is_empty() {
            values.extend(
                arg.suggestions
                    .iter()
                    .cloned()
                    .map(|value| CompleteResultItem {
                        value,
                        description: None,
                    }),
            );
        }
    }

    Response::CompleteResult(CompleteResultResponse { values })
}
