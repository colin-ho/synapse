use crate::protocol::{Request, Response};

use super::state::RuntimeState;
use super::use_cases::{
    command_executed, complete_command, daemon_control, run_generator, translate_natural_language,
};

pub(super) async fn handle_request(request: Request, state: &RuntimeState) -> Response {
    match request {
        Request::NaturalLanguage(req) => {
            translate_natural_language::translate_natural_language(req, state).await
        }
        Request::CommandExecuted(report) => command_executed::command_executed(report, state).await,
        Request::CwdChanged(report) => command_executed::cwd_changed(report).await,
        Request::Complete(req) => complete_command::complete_command(req, state).await,
        Request::RunGenerator(req) => run_generator::run_generator(req, state).await,
        Request::Ping => daemon_control::ping().await,
        Request::Shutdown => daemon_control::shutdown(state).await,
        Request::ReloadConfig => daemon_control::reload_config().await,
        Request::ClearCache => daemon_control::clear_cache(state).await,
    }
}
