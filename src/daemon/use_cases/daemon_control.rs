use crate::protocol::Response;

use super::super::state::RuntimeState;

pub(crate) async fn ping() -> Response {
    tracing::trace!("Ping");
    Response::Pong
}

pub(crate) async fn shutdown(state: &RuntimeState) -> Response {
    tracing::info!("Shutdown requested");
    if let Some(ref token) = state.shutdown_token {
        token.cancel();
    }
    Response::Ack
}

pub(crate) async fn reload_config() -> Response {
    tracing::info!("Config reload requested");
    let _ = crate::config::Config::load();
    tracing::info!("Config reloaded successfully");
    Response::Ack
}

pub(crate) async fn clear_cache(state: &RuntimeState) -> Response {
    tracing::info!("Cache clear requested");
    state.project_root_cache.invalidate_all();
    state.project_type_cache.invalidate_all();
    state.tools_cache.invalidate_all();
    state.nl_cache.invalidate_all().await;
    state.spec_store.clear_caches().await;
    tracing::info!("All caches cleared");
    Response::Ack
}
