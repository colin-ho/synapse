use std::sync::Arc;

use futures_util::stream::SplitSink;
use tokio_util::codec::{Framed, LinesCodec};

use crate::config::Config;
use crate::logging::InteractionLogger;
use crate::providers::Provider;
use crate::ranking::Ranker;
use crate::session::SessionManager;
use crate::spec_store::SpecStore;
use crate::workflow::WorkflowPredictor;

pub(super) type SharedWriter =
    Arc<tokio::sync::Mutex<SplitSink<Framed<tokio::net::UnixStream, LinesCodec>, String>>>;

#[derive(Clone)]
pub(super) struct RuntimeState {
    pub(super) providers: Arc<Vec<Provider>>,
    pub(super) spec_store: Arc<SpecStore>,
    pub(super) ranker: Arc<Ranker>,
    pub(super) workflow_predictor: Arc<WorkflowPredictor>,
    pub(super) session_manager: SessionManager,
    pub(super) interaction_logger: Arc<InteractionLogger>,
    pub(super) config: Arc<Config>,
}

impl RuntimeState {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        providers: Arc<Vec<Provider>>,
        spec_store: Arc<SpecStore>,
        ranker: Arc<Ranker>,
        workflow_predictor: Arc<WorkflowPredictor>,
        session_manager: SessionManager,
        interaction_logger: Arc<InteractionLogger>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            providers,
            spec_store,
            ranker,
            workflow_predictor,
            session_manager,
            interaction_logger,
            config,
        }
    }
}
