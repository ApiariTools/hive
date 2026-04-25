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
        // Return in chronological order
        let mut rows = rows;
        rows.reverse();
        Ok(rows)
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
