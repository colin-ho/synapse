use std::collections::HashSet;
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
    pub(super) spec_store: Arc<SpecStore>,
    pub(super) ranker: Ranker,
    pub(super) workflow_predictor: Arc<WorkflowPredictor>,
    pub(super) workflow_llm_inflight: Arc<tokio::sync::Mutex<HashSet<String>>>,
    pub(super) session_manager: SessionManager,
    pub(super) interaction_logger: InteractionLogger,
    pub(super) config: Config,
}

impl RuntimeState {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        providers: Vec<Provider>,
        spec_store: Arc<SpecStore>,
        ranker: Ranker,
        workflow_predictor: Arc<WorkflowPredictor>,
        session_manager: SessionManager,
        interaction_logger: InteractionLogger,
        config: Config,
    ) -> Self {
        Self {
            providers,
            spec_store,
            ranker,
            workflow_predictor,
            workflow_llm_inflight: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            session_manager,
            interaction_logger,
            config,
        }
    }
}
