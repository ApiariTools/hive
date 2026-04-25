use color_eyre::Result;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS conversations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace TEXT NOT NULL,
                bot TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                attachments TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS sessions (
                workspace TEXT NOT NULL,
                bot TEXT NOT NULL,
                session_id TEXT NOT NULL,
                prompt_hash TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (workspace, bot)
            );

            CREATE TABLE IF NOT EXISTS bot_status (
                workspace TEXT NOT NULL,
                bot TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'idle',
                streaming_content TEXT NOT NULL DEFAULT '',
                tool_name TEXT,
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (workspace, bot)
            );

            CREATE TABLE IF NOT EXISTS signals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace TEXT NOT NULL,
                source TEXT NOT NULL,
                external_id TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT,
                severity TEXT NOT NULL DEFAULT 'info',
                status TEXT NOT NULL DEFAULT 'open',
                url TEXT,
                metadata TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(workspace, source, external_id)
            );",
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn add_message(
        &self,
        workspace: &str,
        bot: &str,
        role: &str,
        content: &str,
        attachments: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations (workspace, bot, role, content, attachments)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![workspace, bot, role, content, attachments],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_conversations(
        &self,
        workspace: &str,
        bot: &str,
        limit: i64,
    ) -> Result<Vec<MessageRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, workspace, bot, role, content, attachments, created_at
             FROM conversations
             WHERE workspace = ?1 AND bot = ?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![workspace, bot, limit], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    workspace: row.get(1)?,
                    bot: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    attachments: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut rows = rows;
        rows.reverse();
        Ok(rows)
    }

    pub fn set_session(
        &self,
        workspace: &str,
        bot: &str,
        session_id: &str,
        prompt_hash: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (workspace, bot, session_id, prompt_hash, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(workspace, bot) DO UPDATE SET
               session_id = ?3, prompt_hash = ?4, updated_at = datetime('now')",
            params![workspace, bot, session_id, prompt_hash],
        )?;
        Ok(())
    }

    /// Get session ID only if the prompt hash matches.
    /// If the hash changed (config/context/soul updated), returns None
    /// so a fresh session is started.
    pub fn get_session_id(&self, workspace: &str, bot: &str, current_hash: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT session_id, prompt_hash FROM sessions WHERE workspace = ?1 AND bot = ?2",
            params![workspace, bot],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        match result {
            Ok((id, stored_hash)) => {
                if stored_hash == current_hash {
                    Ok(Some(id))
                } else {
                    tracing::info!("[session] prompt changed for {workspace}/{bot}, starting fresh");
                    let _ = self.add_message(
                        workspace,
                        bot,
                        "system",
                        "Session reset — bot configuration was updated.",
                        None,
                    );
                    Ok(None)
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ── Bot status (streaming state) ──

    pub fn set_bot_status(
        &self,
        workspace: &str,
        bot: &str,
        status: &str,
        streaming_content: &str,
        tool_name: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO bot_status (workspace, bot, status, streaming_content, tool_name, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
             ON CONFLICT(workspace, bot) DO UPDATE SET
               status = ?3, streaming_content = ?4, tool_name = ?5, updated_at = datetime('now')",
            params![workspace, bot, status, streaming_content, tool_name],
        )?;
        Ok(())
    }

    pub fn append_streaming(&self, workspace: &str, bot: &str, text: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE bot_status SET streaming_content = streaming_content || ?1, updated_at = datetime('now')
             WHERE workspace = ?2 AND bot = ?3",
            params![text, workspace, bot],
        )?;
        Ok(())
    }

    pub fn get_bot_status(&self, workspace: &str, bot: &str) -> Result<Option<BotStatus>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT status, streaming_content, tool_name FROM bot_status
             WHERE workspace = ?1 AND bot = ?2",
            params![workspace, bot],
            |row| {
                Ok(BotStatus {
                    status: row.get(0)?,
                    streaming_content: row.get(1)?,
                    tool_name: row.get(2)?,
                })
            },
        );
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_all_conversations(
        &self,
        workspace: &str,
        limit: i64,
    ) -> Result<Vec<MessageRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, workspace, bot, role, content, attachments, created_at
             FROM conversations
             WHERE workspace = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![workspace, limit], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    workspace: row.get(1)?,
                    bot: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    attachments: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut rows = rows;
        rows.reverse();
        Ok(rows)
    }
}

#[derive(Debug, serde::Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub workspace: String,
    pub bot: String,
    pub role: String,
    pub content: String,
    pub attachments: Option<String>,
    pub created_at: String,
}

#[derive(Debug, serde::Serialize)]
pub struct BotStatus {
    pub status: String,
    pub streaming_content: String,
    pub tool_name: Option<String>,
}
