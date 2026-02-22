//! Per-chat Claude session tracking with persistence.
//!
//! Each Telegram chat gets its own Claude session. Sessions are persisted to
//! `.hive/sessions.json` so the daemon can resume after restarts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Persistent state for all chat sessions + buzz reader offset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStoreState {
    /// Active session per chat_id.
    #[serde(default)]
    pub active: HashMap<i64, ChatSession>,

    /// Archived (completed) sessions per chat_id.
    #[serde(default)]
    pub archived: HashMap<i64, Vec<ArchivedSession>>,

    /// Buzz JSONL reader byte offset — survives daemon restarts.
    #[serde(default)]
    pub buzz_reader_offset: u64,
}

/// A live Claude session associated with a chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub session_id: String,
    pub turn_count: u64,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
    pub nudge_sent: bool,
}

/// A previously completed session, kept for /history and /resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedSession {
    pub session_id: String,
    pub turn_count: u64,
    pub created_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
}

/// Manages session state with persistence to disk.
pub struct SessionStore {
    path: PathBuf,
    state: SessionStoreState,
}

impl SessionStore {
    /// Load or create a session store at the given path.
    pub fn load(workspace_root: &Path) -> Self {
        let path = workspace_root.join(".hive/sessions.json");
        let state: SessionStoreState =
            apiari_common::state::load_state(&path).unwrap_or_default();
        Self { path, state }
    }

    /// Persist current state to disk.
    pub fn save(&self) -> color_eyre::Result<()> {
        apiari_common::state::save_state(&self.path, &self.state)
            .map_err(|e| color_eyre::eyre::eyre!("failed to save sessions: {e}"))
    }

    /// Get the active session for a chat, if any.
    pub fn get_active(&self, chat_id: i64) -> Option<&ChatSession> {
        self.state.active.get(&chat_id)
    }

    /// Start a new session for a chat. Archives any existing active session.
    pub fn start_session(&mut self, chat_id: i64, session_id: String) {
        // Archive the current session if one exists.
        if let Some(old) = self.state.active.remove(&chat_id) {
            let archived = ArchivedSession {
                session_id: old.session_id,
                turn_count: old.turn_count,
                created_at: old.created_at,
                ended_at: Utc::now(),
            };
            self.state
                .archived
                .entry(chat_id)
                .or_default()
                .push(archived);
        }

        let session = ChatSession {
            session_id,
            turn_count: 0,
            created_at: Utc::now(),
            last_active: Utc::now(),
            nudge_sent: false,
        };
        self.state.active.insert(chat_id, session);
    }

    /// Increment turn count and update last_active for a chat's session.
    /// Returns whether a nudge should be sent (crosses threshold, not yet sent).
    pub fn record_turn(&mut self, chat_id: i64, nudge_threshold: u64) -> bool {
        if let Some(session) = self.state.active.get_mut(&chat_id) {
            session.turn_count += 1;
            session.last_active = Utc::now();

            if session.turn_count >= nudge_threshold && !session.nudge_sent {
                session.nudge_sent = true;
                return true;
            }
        }
        false
    }

    /// Archive the active session for a chat (/reset).
    /// Returns true if there was an active session to archive.
    pub fn reset_session(&mut self, chat_id: i64) -> bool {
        if let Some(old) = self.state.active.remove(&chat_id) {
            let archived = ArchivedSession {
                session_id: old.session_id,
                turn_count: old.turn_count,
                created_at: old.created_at,
                ended_at: Utc::now(),
            };
            self.state
                .archived
                .entry(chat_id)
                .or_default()
                .push(archived);
            true
        } else {
            false
        }
    }

    /// Get archived sessions for a chat (/history).
    pub fn history(&self, chat_id: i64) -> &[ArchivedSession] {
        self.state
            .archived
            .get(&chat_id)
            .map(|v| v.as_slice())
            .unwrap_or_default()
    }

    /// Find an archived session by ID prefix (/resume).
    /// Returns the session_id if found.
    pub fn find_archived(&self, chat_id: i64, id_prefix: &str) -> Option<String> {
        let archives = self.state.archived.get(&chat_id)?;
        let matches: Vec<_> = archives
            .iter()
            .filter(|a| a.session_id.starts_with(id_prefix))
            .collect();
        if matches.len() == 1 {
            Some(matches[0].session_id.clone())
        } else {
            None
        }
    }

    /// Resume an archived session — makes it the active session for a chat.
    /// Returns true if the session was found and resumed.
    pub fn resume_session(&mut self, chat_id: i64, session_id: &str) -> bool {
        let archives = match self.state.archived.get_mut(&chat_id) {
            Some(a) => a,
            None => return false,
        };

        let idx = archives
            .iter()
            .position(|a| a.session_id == session_id);

        let Some(idx) = idx else { return false };

        let archived = archives.remove(idx);

        // Archive current active session if any.
        if let Some(old) = self.state.active.remove(&chat_id) {
            archives.push(ArchivedSession {
                session_id: old.session_id,
                turn_count: old.turn_count,
                created_at: old.created_at,
                ended_at: Utc::now(),
            });
        }

        // Restore the archived session as active.
        self.state.active.insert(
            chat_id,
            ChatSession {
                session_id: archived.session_id,
                turn_count: archived.turn_count,
                created_at: archived.created_at,
                last_active: Utc::now(),
                nudge_sent: false,
            },
        );

        true
    }

    /// Get/set the buzz reader byte offset.
    pub fn buzz_offset(&self) -> u64 {
        self.state.buzz_reader_offset
    }

    pub fn set_buzz_offset(&mut self, offset: u64) {
        self.state.buzz_reader_offset = offset;
    }

    /// Remove active session for a chat (e.g. when session dies unexpectedly).
    pub fn remove_active(&mut self, chat_id: i64) {
        self.state.active.remove(&chat_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_store() -> SessionStore {
        SessionStore {
            path: PathBuf::from("/tmp/test-sessions.json"),
            state: SessionStoreState::default(),
        }
    }

    #[test]
    fn test_start_session() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        let active = store.get_active(100).unwrap();
        assert_eq!(active.session_id, "sess-1");
        assert_eq!(active.turn_count, 0);
        assert!(!active.nudge_sent);
    }

    #[test]
    fn test_start_session_archives_previous() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.start_session(100, "sess-2".into());

        let active = store.get_active(100).unwrap();
        assert_eq!(active.session_id, "sess-2");

        let history = store.history(100);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].session_id, "sess-1");
    }

    #[test]
    fn test_record_turn_and_nudge() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());

        // No nudge below threshold.
        for _ in 0..49 {
            assert!(!store.record_turn(100, 50));
        }

        // Nudge at threshold.
        assert!(store.record_turn(100, 50));

        // No second nudge.
        assert!(!store.record_turn(100, 50));

        assert_eq!(store.get_active(100).unwrap().turn_count, 51);
    }

    #[test]
    fn test_reset_session() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.record_turn(100, 50);

        assert!(store.reset_session(100));
        assert!(store.get_active(100).is_none());

        let history = store.history(100);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].session_id, "sess-1");
        assert_eq!(history[0].turn_count, 1);

        // Double reset is a no-op.
        assert!(!store.reset_session(100));
    }

    #[test]
    fn test_find_archived_by_prefix() {
        let mut store = empty_store();
        store.start_session(100, "abc-123-def".into());
        store.reset_session(100);
        store.start_session(100, "xyz-789-ghi".into());
        store.reset_session(100);

        assert_eq!(
            store.find_archived(100, "abc"),
            Some("abc-123-def".into())
        );
        assert_eq!(
            store.find_archived(100, "xyz"),
            Some("xyz-789-ghi".into())
        );
        assert_eq!(store.find_archived(100, "nope"), None);
    }

    #[test]
    fn test_resume_session() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        store.record_turn(100, 50);
        store.reset_session(100);
        store.start_session(100, "sess-2".into());

        assert!(store.resume_session(100, "sess-1"));

        let active = store.get_active(100).unwrap();
        assert_eq!(active.session_id, "sess-1");
        assert_eq!(active.turn_count, 1);

        // sess-2 should be archived now.
        let history = store.history(100);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].session_id, "sess-2");
    }

    #[test]
    fn test_buzz_offset() {
        let mut store = empty_store();
        assert_eq!(store.buzz_offset(), 0);
        store.set_buzz_offset(1234);
        assert_eq!(store.buzz_offset(), 1234);
    }

    #[test]
    fn test_history_empty() {
        let store = empty_store();
        assert!(store.history(999).is_empty());
    }

    #[test]
    fn test_remove_active() {
        let mut store = empty_store();
        store.start_session(100, "sess-1".into());
        assert!(store.get_active(100).is_some());
        store.remove_active(100);
        assert!(store.get_active(100).is_none());
    }
}
