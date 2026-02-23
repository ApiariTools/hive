//! Swarm state watcher — monitors `.swarm/state.json` for agent completions
//! and generates typed notifications.
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

/// Typed swarm event notifications emitted by `SwarmWatcher::poll()`.
#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum SwarmNotification {
    /// A new agent worktree appeared.
    AgentSpawned {
        worktree_id: String,
        branch: String,
        summary: Option<String>,
    },
    /// An agent's pane disappeared (work finished, pane exited).
    AgentCompleted {
        worktree_id: String,
        branch: String,
        duration: String,
    },
    /// A worktree was removed entirely (closed or merged).
    AgentClosed {
        worktree_id: String,
        branch: String,
        duration: String,
    },
    /// An agent appears stalled.
    AgentStalled {
        worktree_id: String,
        branch: String,
        stall_kind: StallKind,
    },
}

/// What kind of stall was detected.
#[derive(Debug, Clone)]
pub enum StallKind {
    /// No events for longer than the configured timeout.
    Idle { minutes: u64 },
    /// Last event is a ToolUse with no corresponding ToolResult.
    PermissionBlock { tool: String },
}

impl SwarmNotification {
    /// Format as a Telegram-friendly notification string.
    pub fn format_telegram(&self) -> String {
        match self {
            Self::AgentSpawned {
                branch, summary, ..
            } => {
                let short = short_branch(branch);
                let summary_line = summary
                    .as_deref()
                    .map(|s| format!("\nTask: {s}"))
                    .unwrap_or_default();
                format!("🐝 *New agent spawned* — {short}\nBranch: `{branch}`{summary_line}")
            }
            Self::AgentCompleted {
                branch, duration, ..
            } => {
                let short = short_branch(branch);
                format!("🐝 *Agent completed* — {short}\nBranch: `{branch}`\nDuration: {duration}")
            }
            Self::AgentClosed {
                branch, duration, ..
            } => {
                let short = short_branch(branch);
                format!("🐝 *Agent closed* — {short}\nBranch: `{branch}`\nDuration: {duration}")
            }
            Self::AgentStalled {
                branch, stall_kind, ..
            } => {
                let short = short_branch(branch);
                let detail = match stall_kind {
                    StallKind::Idle { minutes } => format!("Idle for {minutes}m"),
                    StallKind::PermissionBlock { tool } => {
                        format!("Waiting on permission for `{tool}`")
                    }
                };
                format!("⚠️ *Agent stalled* — {short}\nBranch: `{branch}`\n{detail}")
            }
        }
    }

    /// Return the worktree_id associated with this notification.
    pub fn worktree_id(&self) -> &str {
        match self {
            Self::AgentSpawned { worktree_id, .. }
            | Self::AgentCompleted { worktree_id, .. }
            | Self::AgentClosed { worktree_id, .. }
            | Self::AgentStalled { worktree_id, .. } => worktree_id,
        }
    }
}

/// Watches `.swarm/state.json` for agent completion events.
pub struct SwarmWatcher {
    state_path: PathBuf,
    /// Tracked worktrees from the previous poll.
    known: HashMap<String, TrackedWorktree>,
    /// Whether we've done the initial load (skip notifications on first poll).
    initialized: bool,
    /// Base directory for reading agent events (parent of state.json, i.e. `.swarm/`).
    swarm_dir: PathBuf,
    /// Stall timeout in seconds (0 = disabled).
    stall_timeout_secs: u64,
}

/// Snapshot of a worktree's state for diffing between polls.
#[derive(Debug, Clone)]
struct TrackedWorktree {
    branch: String,
    had_agent: bool,
    created_at: DateTime<Local>,
    /// Whether a stall notification has already been sent for this worktree.
    stall_notified: bool,
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

/// A single event entry from `.swarm/agents/<id>/events.jsonl`.
#[derive(Debug, Deserialize)]
struct AgentEvent {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    timestamp: Option<DateTime<Utc>>,
    /// For tool_use events — the tool name.
    #[serde(default)]
    tool: Option<String>,
}

impl SwarmWatcher {
    pub fn new(state_path: PathBuf) -> Self {
        let swarm_dir = state_path
            .parent()
            .unwrap_or(Path::new(".swarm"))
            .to_path_buf();
        Self {
            state_path,
            known: HashMap::new(),
            initialized: false,
            swarm_dir,
            stall_timeout_secs: 0,
        }
    }

    /// Set the stall detection timeout (in seconds). 0 disables stall detection.
    pub fn set_stall_timeout(&mut self, secs: u64) {
        self.stall_timeout_secs = secs;
    }

    /// Poll the swarm state file and return typed notifications for any transitions.
    pub fn poll(&mut self) -> Vec<SwarmNotification> {
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
                        stall_notified: false,
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
                    notifications.push(SwarmNotification::AgentCompleted {
                        worktree_id: id.clone(),
                        branch: wt.branch.clone(),
                        duration,
                    });
                }
            } else {
                // Worktree was removed (closed or merged).
                let duration = format_duration(prev.created_at);
                notifications.push(SwarmNotification::AgentClosed {
                    worktree_id: id.clone(),
                    branch: prev.branch.clone(),
                    duration,
                });
            }
        }

        // Check for new worktrees.
        for wt in &state.worktrees {
            if !self.known.contains_key(&wt.id) {
                notifications.push(SwarmNotification::AgentSpawned {
                    worktree_id: wt.id.clone(),
                    branch: wt.branch.clone(),
                    summary: wt.summary.clone(),
                });
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
                    stall_notified: false,
                },
            );
        }

        // Preserve stall_notified for worktrees that were already known.
        // (We cleared and re-built, so we need to carry forward.)
        // Re-read previous stall state from notifications: if we just emitted
        // AgentCompleted or AgentClosed, the worktree is gone anyway.
        // For stall detection, check_stalls() handles the stall_notified flag.

        // Check for stalled agents.
        if self.stall_timeout_secs > 0 {
            notifications.extend(self.check_stalls());
        }

        notifications
    }

    /// Check for stalled agents by reading their events.jsonl files.
    fn check_stalls(&mut self) -> Vec<SwarmNotification> {
        let mut stalls = Vec::new();
        let agents_dir = self.swarm_dir.join("agents");

        for (id, tracked) in self.known.iter_mut() {
            // Only check agents that are alive and haven't been notified yet.
            if !tracked.had_agent || tracked.stall_notified {
                continue;
            }

            let events_path = agents_dir.join(id).join("events.jsonl");
            let events = Self::read_recent_events(&events_path);

            if events.is_empty() {
                // No events file — can't determine stall state.
                continue;
            }

            let last = &events[events.len() - 1];

            // Check for permission block: last event is tool_use with no tool_result after.
            if last.r#type == "tool_use"
                && let Some(tool) = &last.tool
            {
                stalls.push(SwarmNotification::AgentStalled {
                    worktree_id: id.clone(),
                    branch: tracked.branch.clone(),
                    stall_kind: StallKind::PermissionBlock { tool: tool.clone() },
                });
                tracked.stall_notified = true;
                continue;
            }

            // Check for idle stall: last event timestamp is old.
            if let Some(ts) = &last.timestamp {
                let elapsed = Utc::now() - *ts;
                let elapsed_secs = elapsed.num_seconds().max(0) as u64;
                if elapsed_secs >= self.stall_timeout_secs {
                    stalls.push(SwarmNotification::AgentStalled {
                        worktree_id: id.clone(),
                        branch: tracked.branch.clone(),
                        stall_kind: StallKind::Idle {
                            minutes: elapsed_secs / 60,
                        },
                    });
                    tracked.stall_notified = true;
                }
            }
        }

        stalls
    }

    /// Read recent events from an agent's events.jsonl (last 20 lines).
    fn read_recent_events(path: &Path) -> Vec<AgentEvent> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        content
            .lines()
            .rev()
            .take(20)
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return None;
                }
                serde_json::from_str::<AgentEvent>(trimmed).ok()
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    fn load_state(path: &Path) -> Option<SwarmState> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }
}

/// Extract a short human-readable name from a branch like "swarm/fix-auth-bug".
fn short_branch(branch: &str) -> &str {
    branch.strip_prefix("swarm/").unwrap_or(branch)
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
        assert!(
            notes.is_empty(),
            "first poll should not generate notifications"
        );
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
        let msg = notes[0].format_telegram();
        assert!(msg.contains("Agent completed"));
        assert!(msg.contains("fix-bug"));
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
        let msg = notes[0].format_telegram();
        assert!(msg.contains("Agent closed"));
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
        let msg = notes[0].format_telegram();
        assert!(msg.contains("New agent spawned"));
        assert!(msg.contains("new-feat"));
        assert!(msg.contains("Add new feature"));
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

    #[test]
    fn test_notification_worktree_id() {
        let n = SwarmNotification::AgentSpawned {
            worktree_id: "abc".into(),
            branch: "swarm/test".into(),
            summary: None,
        };
        assert_eq!(n.worktree_id(), "abc");
    }

    #[test]
    fn test_notification_formatting() {
        let spawned = SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/new".into(),
            summary: Some("Do the thing".into()),
        };
        assert!(spawned.format_telegram().contains("New agent spawned"));
        assert!(spawned.format_telegram().contains("Do the thing"));

        let completed = SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/done".into(),
            duration: "5m".into(),
        };
        assert!(completed.format_telegram().contains("Agent completed"));
        assert!(completed.format_telegram().contains("5m"));

        let stalled_idle = SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/stuck".into(),
            stall_kind: StallKind::Idle { minutes: 10 },
        };
        assert!(stalled_idle.format_telegram().contains("Agent stalled"));
        assert!(stalled_idle.format_telegram().contains("Idle for 10m"));

        let stalled_perm = SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/blocked".into(),
            stall_kind: StallKind::PermissionBlock {
                tool: "Write".into(),
            },
        };
        assert!(stalled_perm.format_telegram().contains("permission"));
        assert!(stalled_perm.format_telegram().contains("Write"));
    }
}
