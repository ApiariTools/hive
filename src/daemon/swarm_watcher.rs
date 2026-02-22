//! Swarm state watcher — monitors `.swarm/state.json` for agent completions
//! and generates Telegram notifications.
//!
//! Since swarm does not persist agent status (only pane IDs), we detect completion
//! by tracking changes between polls:
//! - Agent pane disappears (was `Some`, now `None`) → agent finished
//! - Worktree removed entirely → agent was cleaned up (closed/merged)
//! - New worktree appears → new agent spawned

use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Watches `.swarm/state.json` for agent completion events.
pub struct SwarmWatcher {
    state_path: PathBuf,
    /// Tracked worktrees from the previous poll.
    known: HashMap<String, TrackedWorktree>,
    /// Whether we've done the initial load (skip notifications on first poll).
    initialized: bool,
}

/// Snapshot of a worktree's state for diffing between polls.
#[derive(Debug, Clone)]
struct TrackedWorktree {
    branch: String,
    had_agent: bool,
    created_at: DateTime<Local>,
}

// Mirror types for deserializing .swarm/state.json.
// Intentionally separate from swarm's own types to avoid cross-crate dependency.

#[derive(Debug, Deserialize)]
struct SwarmState {
    #[serde(default)]
    worktrees: Vec<WorktreeState>,
}

#[derive(Debug, Deserialize)]
struct WorktreeState {
    id: String,
    branch: String,
    #[serde(default)]
    agent: Option<PaneState>,
    created_at: DateTime<Local>,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PaneState {
    #[allow(dead_code)]
    pane_id: String,
}

impl SwarmWatcher {
    pub fn new(state_path: PathBuf) -> Self {
        Self {
            state_path,
            known: HashMap::new(),
            initialized: false,
        }
    }

    /// Poll the swarm state file and return notification messages for any transitions.
    pub fn poll(&mut self) -> Vec<String> {
        let state = match Self::load_state(&self.state_path) {
            Some(s) => s,
            None => return Vec::new(),
        };

        // Build current snapshot.
        let mut current: HashMap<String, &WorktreeState> = HashMap::new();
        for wt in &state.worktrees {
            current.insert(wt.id.clone(), wt);
        }

        // On first poll, just populate known state without generating notifications.
        if !self.initialized {
            self.initialized = true;
            for wt in &state.worktrees {
                self.known.insert(
                    wt.id.clone(),
                    TrackedWorktree {
                        branch: wt.branch.clone(),
                        had_agent: wt.agent.is_some(),
                        created_at: wt.created_at,
                    },
                );
            }
            if !self.known.is_empty() {
                eprintln!(
                    "[swarm-watcher] Initialized with {} worktree(s)",
                    self.known.len()
                );
            }
            return Vec::new();
        }

        let mut notifications = Vec::new();

        // Check for agent completions (agent pane disappeared) and removals.
        let previous_ids: Vec<String> = self.known.keys().cloned().collect();
        for id in &previous_ids {
            let prev = &self.known[id];

            if let Some(wt) = current.get(id) {
                // Worktree still exists — check if agent finished.
                if prev.had_agent && wt.agent.is_none() {
                    let duration = format_duration(prev.created_at);
                    let branch = short_branch(&wt.branch);
                    notifications.push(format!(
                        "🐝 *Agent completed* — {branch}\nBranch: `{}`\nDuration: {duration}",
                        wt.branch,
                    ));
                }
            } else {
                // Worktree was removed (closed or merged).
                let branch = short_branch(&prev.branch);
                let duration = format_duration(prev.created_at);
                notifications.push(format!(
                    "🐝 *Agent closed* — {branch}\nBranch: `{}`\nDuration: {duration}",
                    prev.branch,
                ));
            }
        }

        // Check for new worktrees.
        for wt in &state.worktrees {
            if !self.known.contains_key(&wt.id) {
                let branch = short_branch(&wt.branch);
                let summary_line = wt
                    .summary
                    .as_deref()
                    .map(|s| format!("\nTask: {s}"))
                    .unwrap_or_default();
                notifications.push(format!(
                    "🐝 *New agent spawned* — {branch}\nBranch: `{}`{summary_line}",
                    wt.branch,
                ));
            }
        }

        // Update known state.
        self.known.clear();
        for wt in &state.worktrees {
            self.known.insert(
                wt.id.clone(),
                TrackedWorktree {
                    branch: wt.branch.clone(),
                    had_agent: wt.agent.is_some(),
                    created_at: wt.created_at,
                },
            );
        }

        notifications
    }

    fn load_state(path: &Path) -> Option<SwarmState> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }
}

/// Extract a short human-readable name from a branch like "swarm/fix-auth-bug".
fn short_branch(branch: &str) -> &str {
    branch
        .strip_prefix("swarm/")
        .unwrap_or(branch)
}

/// Format the duration since a worktree was created.
fn format_duration(created_at: DateTime<Local>) -> String {
    let elapsed = Utc::now() - created_at.with_timezone(&Utc);
    let mins = elapsed.num_minutes();
    if mins < 1 {
        "< 1m".to_string()
    } else if mins < 60 {
        format!("{mins}m")
    } else {
        let hours = mins / 60;
        let remaining = mins % 60;
        format!("{hours}h {remaining}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_state(file: &mut NamedTempFile, json: &str) {
        file.as_file().set_len(0).unwrap();
        std::io::Seek::seek(file, std::io::SeekFrom::Start(0)).unwrap();
        file.write_all(json.as_bytes()).unwrap();
        file.flush().unwrap();
    }

    #[test]
    fn test_first_poll_no_notifications() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/test","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        let notes = watcher.poll();
        assert!(notes.is_empty(), "first poll should not generate notifications");
        assert!(watcher.initialized);
        assert_eq!(watcher.known.len(), 1);
    }

    #[test]
    fn test_agent_completion_detected() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/fix-bug","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Agent pane disappears.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/fix-bug","agent":null,"created_at":"2026-02-22T10:00:00-08:00"}]}"#,
        );

        let notes = watcher.poll();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("Agent completed"));
        assert!(notes[0].contains("fix-bug"));
    }

    #[test]
    fn test_worktree_removal_detected() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/old-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Worktree removed entirely.
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let notes = watcher.poll();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("Agent closed"));
    }

    #[test]
    fn test_new_worktree_detected() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // New worktree appears.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/new-feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","summary":"Add new feature"}]}"#,
        );

        let notes = watcher.poll();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("New agent spawned"));
        assert!(notes[0].contains("new-feat"));
        assert!(notes[0].contains("Add new feature"));
    }

    #[test]
    fn test_missing_state_file() {
        let mut watcher = SwarmWatcher::new(PathBuf::from("/nonexistent/state.json"));
        let notes = watcher.poll();
        assert!(notes.is_empty());
    }

    #[test]
    fn test_short_branch() {
        assert_eq!(short_branch("swarm/fix-bug"), "fix-bug");
        assert_eq!(short_branch("main"), "main");
        assert_eq!(short_branch("feature/auth"), "feature/auth");
    }
}
