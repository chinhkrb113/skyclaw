//! Session manager — thread-safe storage for active session contexts.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use skyclaw_core::types::session::SessionContext;

/// Thread-safe session manager backed by an in-memory HashMap.
#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, SessionContext>>>,
}

impl SessionManager {
    /// Create a new empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Build a deterministic session key from channel + chat_id + user_id.
    fn session_key(channel: &str, chat_id: &str, user_id: &str) -> String {
        format!("{}:{}:{}", channel, chat_id, user_id)
    }

    /// Get an existing session or create a new one for the given channel/chat/user.
    pub async fn get_or_create_session(
        &self,
        channel: &str,
        chat_id: &str,
        user_id: &str,
    ) -> SessionContext {
        let key = Self::session_key(channel, chat_id, user_id);

        // Fast path: read lock
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(&key) {
                return session.clone();
            }
        }

        // Slow path: write lock, create new session
        let mut sessions = self.sessions.write().await;

        // Double-check after acquiring write lock
        if let Some(session) = sessions.get(&key) {
            return session.clone();
        }

        let session = SessionContext {
            session_id: key.clone(),
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
            user_id: user_id.to_string(),
            history: Vec::new(),
            workspace_path: std::env::current_dir().unwrap_or_else(|_| "/tmp".into()),
        };

        sessions.insert(key, session.clone());
        session
    }

    /// Update a session in the store (e.g., after history changes).
    pub async fn update_session(&self, session: SessionContext) {
        let key = Self::session_key(&session.channel, &session.chat_id, &session.user_id);
        let mut sessions = self.sessions.write().await;
        sessions.insert(key, session);
    }

    /// Remove a session from the store.
    pub async fn remove_session(&self, channel: &str, chat_id: &str, user_id: &str) {
        let key = Self::session_key(channel, chat_id, user_id);
        let mut sessions = self.sessions.write().await;
        sessions.remove(&key);
    }

    /// Get the number of active sessions.
    pub async fn session_count(&self) -> usize {
        let sessions = self.sessions.read().await;
        sessions.len()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_session_returns_new_session() {
        let mgr = SessionManager::new();
        let session = mgr.get_or_create_session("telegram", "chat1", "user1").await;
        assert_eq!(session.channel, "telegram");
        assert_eq!(session.chat_id, "chat1");
        assert_eq!(session.user_id, "user1");
        assert!(session.history.is_empty());
    }

    #[tokio::test]
    async fn get_existing_session_returns_same() {
        let mgr = SessionManager::new();
        let s1 = mgr.get_or_create_session("cli", "c1", "u1").await;
        let s2 = mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(s1.session_id, s2.session_id);
    }

    #[tokio::test]
    async fn different_users_get_different_sessions() {
        let mgr = SessionManager::new();
        let s1 = mgr.get_or_create_session("cli", "c1", "user_a").await;
        let s2 = mgr.get_or_create_session("cli", "c1", "user_b").await;
        assert_ne!(s1.session_id, s2.session_id);
    }

    #[tokio::test]
    async fn session_count_tracks_active() {
        let mgr = SessionManager::new();
        assert_eq!(mgr.session_count().await, 0);

        mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(mgr.session_count().await, 1);

        mgr.get_or_create_session("tg", "c2", "u2").await;
        assert_eq!(mgr.session_count().await, 2);

        // Same session, no increase
        mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(mgr.session_count().await, 2);
    }

    #[tokio::test]
    async fn remove_session_decreases_count() {
        let mgr = SessionManager::new();
        mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(mgr.session_count().await, 1);

        mgr.remove_session("cli", "c1", "u1").await;
        assert_eq!(mgr.session_count().await, 0);
    }

    #[tokio::test]
    async fn update_session_preserves_changes() {
        let mgr = SessionManager::new();
        let mut session = mgr.get_or_create_session("cli", "c1", "u1").await;

        // Simulate adding history
        session.history.push(skyclaw_core::types::message::ChatMessage {
            role: skyclaw_core::types::message::Role::User,
            content: skyclaw_core::types::message::MessageContent::Text("hello".to_string()),
        });
        mgr.update_session(session).await;

        let restored = mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(restored.history.len(), 1);
    }

    #[test]
    fn session_key_is_deterministic() {
        let k1 = SessionManager::session_key("telegram", "123", "456");
        let k2 = SessionManager::session_key("telegram", "123", "456");
        assert_eq!(k1, k2);
        assert_eq!(k1, "telegram:123:456");
    }

    // ── T5b: New edge case tests ──────────────────────────────────────

    #[tokio::test]
    async fn concurrent_session_creation() {
        let mgr = std::sync::Arc::new(SessionManager::new());
        let mut handles = Vec::new();

        for i in 0..10 {
            let m = mgr.clone();
            handles.push(tokio::spawn(async move {
                m.get_or_create_session("cli", &format!("chat{i}"), "user").await
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(mgr.session_count().await, 10);
    }

    #[tokio::test]
    async fn remove_nonexistent_session_is_noop() {
        let mgr = SessionManager::new();
        mgr.remove_session("missing", "c", "u").await;
        assert_eq!(mgr.session_count().await, 0);
    }

    #[tokio::test]
    async fn default_session_manager_empty() {
        let mgr = SessionManager::default();
        assert_eq!(mgr.session_count().await, 0);
    }

    #[tokio::test]
    async fn session_key_with_special_characters() {
        let mgr = SessionManager::new();
        let session = mgr.get_or_create_session("tg", "chat:with:colons", "user@domain").await;
        assert_eq!(session.session_id, "tg:chat:with:colons:user@domain");
    }

    #[tokio::test]
    async fn update_nonexistent_session_creates_it() {
        let mgr = SessionManager::new();

        let session = SessionContext {
            session_id: "new".to_string(),
            channel: "cli".to_string(),
            chat_id: "c1".to_string(),
            user_id: "u1".to_string(),
            history: Vec::new(),
            workspace_path: std::path::PathBuf::from("/tmp"),
        };

        mgr.update_session(session).await;
        // The session_key function builds key from channel+chat_id+user_id
        let count = mgr.session_count().await;
        assert_eq!(count, 1);
    }
}
