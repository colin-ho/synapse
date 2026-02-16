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

pub(super) struct RuntimeState {
    pub(super) providers: Vec<Provider>,
    pub(super) phase2_providers: Vec<Provider>,
    pub(super) spec_store: Arc<SpecStore>,
    pub(super) ranker: Ranker,
    pub(super) workflow_predictor: WorkflowPredictor,
    pub(super) session_manager: SessionManager,
    pub(super) interaction_logger: InteractionLogger,
    pub(super) config: Config,
}

impl RuntimeState {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        providers: Vec<Provider>,
        phase2_providers: Vec<Provider>,
        spec_store: Arc<SpecStore>,
        ranker: Ranker,
        workflow_predictor: WorkflowPredictor,
        session_manager: SessionManager,
        interaction_logger: InteractionLogger,
        config: Config,
    ) -> Self {
        Self {
            providers,
            phase2_providers,
            spec_store,
            ranker,
            workflow_predictor,
            session_manager,
            interaction_logger,
            config,
        }
    }
}
