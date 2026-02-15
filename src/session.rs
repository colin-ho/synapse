use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::protocol::{SuggestRequest, SuggestionResponse};

#[derive(Debug, Clone)]
pub struct SessionState {
    #[allow(dead_code)]
    pub id: String,
    pub cwd: String,
    pub last_buffer: String,
    pub last_suggestion: Option<SuggestionResponse>,
    pub recent_commands: Vec<String>,
    #[allow(dead_code)]
    pub connected_at: std::time::Instant,
    pub last_activity: std::time::Instant,
}

impl SessionState {
    fn new(id: String) -> Self {
        let now = std::time::Instant::now();
        Self {
            id,
            cwd: String::new(),
            last_buffer: String::new(),
            last_suggestion: None,
            recent_commands: Vec::new(),
            connected_at: now,
            last_activity: now,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, SessionState>>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    #[allow(dead_code)]
    pub async fn get_or_create(&self, session_id: &str) -> SessionState {
        let mut sessions = self.sessions.write().await;
        sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionState::new(session_id.to_string()))
            .clone()
    }

    pub async fn update_from_request(&self, request: &SuggestRequest) {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .entry(request.session_id.clone())
            .or_insert_with(|| SessionState::new(request.session_id.clone()));

        session.cwd = request.cwd.clone();
        session.last_buffer = request.buffer.clone();
        session.recent_commands = request.recent_commands.clone();
        session.last_activity = std::time::Instant::now();
    }

    pub async fn record_suggestion(&self, session_id: &str, suggestion: SuggestionResponse) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.last_suggestion = Some(suggestion);
        }
    }

    pub async fn get_last_buffer(&self, session_id: &str) -> Option<String> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).map(|s| s.last_buffer.clone())
    }

    #[allow(dead_code)]
    pub async fn remove(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(session_id);
    }

    #[allow(dead_code)]
    pub async fn prune_inactive(&self, max_idle: std::time::Duration) {
        let mut sessions = self.sessions.write().await;
        let now = std::time::Instant::now();
        sessions.retain(|_, s| now.duration_since(s.last_activity) < max_idle);
    }
}
