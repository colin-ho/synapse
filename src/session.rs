use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct SessionState {
    #[allow(dead_code)]
    pub id: String,
    pub cwd: String,
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

    pub async fn update_cwd(&self, session_id: &str, cwd: &str) {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionState::new(session_id.to_string()));
        session.cwd = cwd.to_string();
        session.last_activity = std::time::Instant::now();
    }

    pub async fn get_cwd(&self, session_id: &str) -> Option<String> {
        self.sessions
            .read()
            .await
            .get(session_id)
            .map(|s| s.cwd.clone())
            .filter(|cwd| !cwd.is_empty())
    }

    pub async fn prune_inactive(&self, max_idle: std::time::Duration) {
        let mut sessions = self.sessions.write().await;
        let now = std::time::Instant::now();
        sessions.retain(|_, s| now.duration_since(s.last_activity) < max_idle);
    }
}
