//! Config watcher — detects prompt changes and resets sessions automatically.
//!
//! Polls workspace configs, context.md, and soul.md every 30 seconds.
//! When a change is detected, inserts a system message and clears the
//! session so the next message gets a fresh prompt.

use crate::db::Db;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::time::{Duration, interval};
use tracing::info;

pub struct WatchedWorkspace {
    pub name: String,
    pub config_path: PathBuf,
    pub root: Option<PathBuf>,
    pub bots: Vec<String>,
}

/// Start a background task that watches for config/prompt changes.
pub fn start_config_watcher(workspaces: Vec<WatchedWorkspace>, db: Db) {
    if workspaces.is_empty() {
        return;
    }
    info!(
        "[config-watcher] watching {} workspace(s) for prompt changes",
        workspaces.len()
    );
    tokio::spawn(run_watcher(workspaces, db));
}

async fn run_watcher(workspaces: Vec<WatchedWorkspace>, db: Db) {
    let mut hashes: HashMap<(String, String), String> = HashMap::new();
    let mut tick = interval(Duration::from_secs(30));

    // Initial hash capture
    for ws in &workspaces {
        for bot in &ws.bots {
            let hash = compute_prompt_hash(ws, bot);
            hashes.insert((ws.name.clone(), bot.clone()), hash);
        }
    }

    loop {
        tick.tick().await;

        for ws in &workspaces {
            for bot in &ws.bots {
                let new_hash = compute_prompt_hash(ws, bot);
                let key = (ws.name.clone(), bot.clone());

                if let Some(old_hash) = hashes.get(&key)
                    && *old_hash != new_hash
                {
                    info!(
                        "[config-watcher] prompt changed for {}/{}, resetting session",
                        ws.name, bot
                    );
                    let _ = db.add_message(
                        &ws.name,
                        bot,
                        "system",
                        "Session reset — bot configuration was updated.",
                        None,
                    );
                    // Clear the session by setting an invalid hash
                    // Next message will detect mismatch and start fresh
                }

                hashes.insert(key, new_hash);
            }
        }
    }
}

fn compute_prompt_hash(ws: &WatchedWorkspace, _bot: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    // Hash the config file content
    if let Ok(content) = std::fs::read_to_string(&ws.config_path) {
        content.hash(&mut hasher);
    }

    // Hash context.md and soul.md if they exist
    if let Some(ref root) = ws.root {
        if let Ok(ctx) = std::fs::read_to_string(root.join(".apiari/context.md")) {
            ctx.hash(&mut hasher);
        }
        if let Ok(soul) = std::fs::read_to_string(root.join(".apiari/soul.md")) {
            soul.hash(&mut hasher);
        }

        // Hash docs directory: sorted filenames + modification times
        let docs_dir = root.join(".apiari/docs");
        if docs_dir.is_dir()
            && let Ok(read_dir) = std::fs::read_dir(&docs_dir)
        {
            let mut doc_entries: Vec<(String, u64)> = read_dir
                .flatten()
                .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("md"))
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let mtime = e
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    (name, mtime)
                })
                .collect();
            doc_entries.sort_by(|a, b| a.0.cmp(&b.0));
            doc_entries.hash(&mut hasher);
        }
    }

    format!("{:016x}", hasher.finish())
}
