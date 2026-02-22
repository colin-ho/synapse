use crate::protocol::{CommandExecutedReport, CwdChangedReport, Response};

use super::super::state::RuntimeState;

pub(crate) async fn command_executed(
    report: CommandExecutedReport,
    state: &RuntimeState,
) -> Response {
    tracing::debug!(
        session = %report.session_id,
        command = %report.command,
        "Command executed"
    );

    // Warm caches for the command (safe strategy: parse system zsh completion
    // files into in-memory cache for NL context — no command execution).
    let command_name = report.command.split_whitespace().next().unwrap_or("");
    if !command_name.is_empty() {
        let cwd = if report.cwd.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(&report.cwd))
        };
        state
            .spec_store
            .warm_command_cache(command_name, cwd.as_deref())
            .await;
    }

    Response::Ack
}

pub(crate) async fn cwd_changed(report: CwdChangedReport) -> Response {
    tracing::debug!(
        session = %report.session_id,
        cwd = %report.cwd,
        "CwdChanged"
    );

    Response::Ack
}
