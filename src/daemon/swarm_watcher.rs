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
use std::collections::{HashMap, HashSet};
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
        pr_url: Option<String>,
    },
    /// A worktree was removed entirely (closed or merged).
    AgentClosed {
        worktree_id: String,
        branch: String,
        duration: String,
        pr_url: Option<String>,
    },
    /// A PR was opened by an agent (pr_url transitioned from None to Some).
    PrOpened {
        worktree_id: String,
        branch: String,
        pr_url: String,
        pr_title: Option<String>,
        duration: String,
    },
    /// An agent appears stalled.
    AgentStalled {
        worktree_id: String,
        branch: String,
        stall_kind: StallKind,
    },
    /// A claude-tui agent is waiting for user input.
    AgentWaiting {
        worktree_id: String,
        branch: String,
        summary: Option<String>,
        pr_url: Option<String>,
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
                branch,
                duration,
                pr_url,
                ..
            } => {
                let short = short_branch(branch);
                let pr_line = pr_url
                    .as_deref()
                    .map(|url| format!("\nPR: {url}"))
                    .unwrap_or_default();
                format!(
                    "🐝 *Agent completed* — {short}\nBranch: `{branch}`\nDuration: {duration}{pr_line}"
                )
            }
            Self::AgentClosed {
                branch,
                duration,
                pr_url,
                ..
            } => {
                let short = short_branch(branch);
                let pr_line = pr_url
                    .as_deref()
                    .map(|url| format!("\nPR: {url}"))
                    .unwrap_or_default();
                format!(
                    "🐝 *Agent closed* — {short}\nBranch: `{branch}`\nDuration: {duration}{pr_line}"
                )
            }
            Self::PrOpened {
                branch,
                pr_url,
                pr_title,
                duration,
                ..
            } => {
                let short = short_branch(branch);
                let title_line = pr_title
                    .as_deref()
                    .map(|t| format!("\n{t}"))
                    .unwrap_or_default();
                format!("🔗 *PR opened* — {short}{title_line}\n{pr_url}\nDuration: {duration}")
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
            Self::AgentWaiting {
                worktree_id,
                branch,
                summary,
                pr_url,
            } => {
                let short = short_branch(branch);
                let summary_line = summary
                    .as_deref()
                    .map(|s| format!("\n_{s}_"))
                    .unwrap_or_default();
                let pr_line = pr_url
                    .as_deref()
                    .map(|url| format!("\nPR: {url}"))
                    .unwrap_or_default();
                format!(
                    "⏳ *Worker waiting* — {short}{summary_line}{pr_line}\n\nReply: `swarm send {worktree_id} <message>`"
                )
            }
        }
    }

    /// One-line summary for use in batched notification messages.
    pub fn summary_line(&self) -> String {
        match self {
            Self::AgentSpawned {
                branch, summary, ..
            } => {
                let short = short_branch(branch);
                match summary {
                    Some(s) => format!("New agent spawned — {short} ({s})"),
                    None => format!("New agent spawned — {short}"),
                }
            }
            Self::AgentCompleted { branch, pr_url, .. } => {
                let short = short_branch(branch);
                match pr_url {
                    Some(url) => format!("Agent completed — {short} (PR: {url})"),
                    None => format!("Agent completed — {short}"),
                }
            }
            Self::AgentClosed { branch, .. } => {
                let short = short_branch(branch);
                format!("Agent closed — {short}")
            }
            Self::PrOpened { branch, pr_url, .. } => {
                let short = short_branch(branch);
                format!("PR opened — {short} ({pr_url})")
            }
            Self::AgentStalled {
                branch, stall_kind, ..
            } => {
                let short = short_branch(branch);
                match stall_kind {
                    StallKind::Idle { minutes } => {
                        format!("Agent stalled — {short} (idle {minutes}m)")
                    }
                    StallKind::PermissionBlock { tool } => {
                        format!("Agent stalled — {short} (permission: {tool})")
                    }
                }
            }
            Self::AgentWaiting { branch, .. } => {
                let short = short_branch(branch);
                format!("Worker waiting — {short}")
            }
        }
    }

    /// Return the worktree_id associated with this notification.
    pub fn worktree_id(&self) -> &str {
        match self {
            Self::AgentSpawned { worktree_id, .. }
            | Self::AgentCompleted { worktree_id, .. }
            | Self::AgentClosed { worktree_id, .. }
            | Self::PrOpened { worktree_id, .. }
            | Self::AgentStalled { worktree_id, .. }
            | Self::AgentWaiting { worktree_id, .. } => worktree_id,
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
    /// Workers that were already in "waiting" state at initialization.
    /// Drained on the first poll after init to re-emit AgentWaiting.
    waiting_at_init: HashSet<String>,
    /// Persisted set of pr_urls that have already triggered a PrOpened notification.
    pr_notified: HashSet<String>,
    /// Worktree IDs with unnotified pr_urls at init.
    /// Drained on the first poll after init to emit PrOpened (handles daemon restarts).
    prs_at_init: HashSet<String>,
    /// Directory for persisting `pr_notified.json` (e.g. `.hive/`).
    hive_dir: Option<PathBuf>,
}

/// Snapshot of a worktree's state for diffing between polls.
#[derive(Debug, Clone)]
struct TrackedWorktree {
    branch: String,
    had_agent: bool,
    created_at: DateTime<Local>,
    /// Whether a stall notification has already been sent for this worktree.
    stall_notified: bool,
    agent_kind: Option<String>,
    pr_url: Option<String>,
    agent_session_status: Option<String>,
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
    #[serde(default)]
    agent_kind: Option<String>,
    #[serde(default)]
    pr: Option<WtPrInfo>,
    #[serde(default)]
    agent_session_status: Option<String>,
}

impl WorktreeState {
    fn pr_url(&self) -> Option<String> {
        self.pr.as_ref().map(|p| p.url.clone())
    }
    fn pr_title(&self) -> Option<String> {
        self.pr.as_ref().and_then(|p| p.title.clone())
    }
}

#[derive(Debug, Deserialize)]
struct WtPrInfo {
    url: String,
    #[serde(default)]
    title: Option<String>,
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
            waiting_at_init: HashSet::new(),
            pr_notified: HashSet::new(),
            prs_at_init: HashSet::new(),
            hive_dir: None,
        }
    }

    /// Set the stall detection timeout (in seconds). 0 disables stall detection.
    pub fn set_stall_timeout(&mut self, secs: u64) {
        self.stall_timeout_secs = secs;
    }

    /// Set the hive directory for persisting PR notification state.
    /// When set, loads/saves `pr_notified.json` to track which PrOpened
    /// notifications have already been sent, avoiding duplicates across restarts.
    pub fn set_hive_dir(&mut self, dir: PathBuf) {
        self.hive_dir = Some(dir);
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
                        agent_kind: wt.agent_kind.clone(),
                        pr_url: wt.pr_url(),
                        agent_session_status: wt.agent_session_status.clone(),
                    },
                );
            }
            // Track workers already in "waiting" state so we can re-emit
            // AgentWaiting on the next poll (handles daemon restarts).
            for wt in &state.worktrees {
                if wt.agent_session_status.as_deref() == Some("waiting") {
                    self.waiting_at_init.insert(wt.id.clone());
                }
            }
            // Load persisted PR notification set and track unnotified PRs at init.
            // Only active when hive_dir is set (i.e. running as daemon).
            if let Some(dir) = &self.hive_dir {
                self.pr_notified = Self::load_pr_notified(&dir.join("pr_notified.json"));
                for wt in &state.worktrees {
                    if let Some(url) = &wt.pr_url()
                        && !self.pr_notified.contains(url)
                    {
                        self.prs_at_init.insert(wt.id.clone());
                    }
                }
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
        let mut pr_notified_changed = false;

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
                        pr_url: wt.pr_url(),
                    });
                }

                // Check if a PR was opened (pr_url transitioned from None to Some).
                if prev.pr_url.is_none() && wt.pr_url().is_some() {
                    let duration = format_duration(prev.created_at);
                    let url = wt.pr_url().unwrap();
                    notifications.push(SwarmNotification::PrOpened {
                        worktree_id: id.clone(),
                        branch: wt.branch.clone(),
                        pr_url: url.clone(),
                        pr_title: wt.pr_title(),
                        duration,
                    });
                    self.pr_notified.insert(url);
                    pr_notified_changed = true;
                }

                // Check if agent transitioned to "waiting" (needs user input).
                if prev.agent_session_status.as_deref() != Some("waiting")
                    && wt.agent_session_status.as_deref() == Some("waiting")
                {
                    notifications.push(SwarmNotification::AgentWaiting {
                        worktree_id: id.clone(),
                        branch: wt.branch.clone(),
                        summary: wt.summary.clone(),
                        pr_url: wt.pr_url(),
                    });
                }
            } else {
                // Worktree was removed (closed or merged).
                let duration = format_duration(prev.created_at);
                notifications.push(SwarmNotification::AgentClosed {
                    worktree_id: id.clone(),
                    branch: prev.branch.clone(),
                    duration,
                    pr_url: prev.pr_url.clone(),
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
                // If the worker is already in waiting state on first sight, emit AgentWaiting
                // immediately — it completed within one poll window so the transition was missed.
                if wt.agent_session_status.as_deref() == Some("waiting") {
                    notifications.push(SwarmNotification::AgentWaiting {
                        worktree_id: wt.id.clone(),
                        branch: wt.branch.clone(),
                        summary: wt.summary.clone(),
                        pr_url: wt.pr_url(),
                    });
                }
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
                    agent_kind: wt.agent_kind.clone(),
                    pr_url: wt.pr_url(),
                    agent_session_status: wt.agent_session_status.clone(),
                },
            );
        }

        // Preserve stall_notified for worktrees that were already known.
        // (We cleared and re-built, so we need to carry forward.)
        // Re-read previous stall state from notifications: if we just emitted
        // AgentCompleted or AgentClosed, the worktree is gone anyway.
        // For stall detection, check_stalls() handles the stall_notified flag.

        // Re-emit AgentWaiting for workers that were already waiting at init.
        // Drain ensures this only fires once (the first poll after initialization).
        if !self.waiting_at_init.is_empty() {
            let init_ids: HashSet<String> = self.waiting_at_init.drain().collect();
            for id in init_ids {
                if let Some(wt) = current.get(&id)
                    && wt.agent_session_status.as_deref() == Some("waiting")
                    && wt.pr_url().is_none()
                {
                    notifications.push(SwarmNotification::AgentWaiting {
                        worktree_id: id,
                        branch: wt.branch.clone(),
                        summary: wt.summary.clone(),
                        pr_url: None,
                    });
                }
            }
        }

        // Re-emit PrOpened for PRs that existed at init but weren't yet notified.
        // Drained on first poll after init (handles daemon restarts).
        if !self.prs_at_init.is_empty() {
            let init_ids: HashSet<String> = self.prs_at_init.drain().collect();
            for id in init_ids {
                if let Some(wt) = current.get(&id)
                    && let Some(pr_url) = &wt.pr_url()
                {
                    notifications.push(SwarmNotification::PrOpened {
                        worktree_id: id,
                        branch: wt.branch.clone(),
                        pr_url: pr_url.clone(),
                        pr_title: wt.pr_title(),
                        duration: format_duration(wt.created_at),
                    });
                    self.pr_notified.insert(pr_url.clone());
                    pr_notified_changed = true;
                }
            }
        }

        // Check for stalled agents.
        if self.stall_timeout_secs > 0 {
            notifications.extend(self.check_stalls());
        }

        if pr_notified_changed {
            self.save_pr_notified();
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

            // claude-tui agents sit idle waiting for user input — not a stall.
            if tracked.agent_kind.as_deref() == Some("claude-tui") {
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

    fn load_pr_notified(path: &Path) -> HashSet<String> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_pr_notified(&self) {
        if let Some(dir) = &self.hive_dir {
            let path = dir.join("pr_notified.json");
            if let Ok(json) = serde_json::to_string_pretty(&self.pr_notified) {
                let _ = std::fs::write(&path, json);
            }
        }
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
            pr_url: None,
        };
        assert!(completed.format_telegram().contains("Agent completed"));
        assert!(completed.format_telegram().contains("5m"));
        assert!(!completed.format_telegram().contains("PR:"));

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

    #[test]
    fn test_pr_url_in_completed_notification() {
        let completed = SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/feat".into(),
            duration: "12m".into(),
            pr_url: Some("https://github.com/ApiariTools/hive/pull/42".into()),
        };
        let msg = completed.format_telegram();
        assert!(msg.contains("PR: https://github.com/ApiariTools/hive/pull/42"));
    }

    #[test]
    fn test_pr_url_in_closed_notification() {
        let closed = SwarmNotification::AgentClosed {
            worktree_id: "1".into(),
            branch: "swarm/feat".into(),
            duration: "8m".into(),
            pr_url: Some("https://github.com/ApiariTools/hive/pull/99".into()),
        };
        let msg = closed.format_telegram();
        assert!(msg.contains("PR: https://github.com/ApiariTools/hive/pull/99"));
    }

    #[test]
    fn test_agent_kind_deserialized() {
        let json = r#"{"worktrees":[{"id":"1","branch":"swarm/tui","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui"}]}"#;
        let state: SwarmState = serde_json::from_str(json).unwrap();
        assert_eq!(state.worktrees[0].agent_kind.as_deref(), Some("claude-tui"));
    }

    #[test]
    fn test_claude_tui_skips_stall_detection() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/tui-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_stall_timeout(1); // 1 second — would trigger immediately
        watcher.poll(); // Initialize

        // Poll again — agent is still running, but should NOT get a stall notification.
        let notes = watcher.poll();
        let stall_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentStalled { .. }))
            .collect();
        assert!(
            stall_notes.is_empty(),
            "claude-tui agents should not trigger stall notifications"
        );
    }

    #[test]
    fn test_pr_opened_none_to_some() {
        let mut file = NamedTempFile::new().unwrap();
        // Initial state: agent running, no PR yet.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/add-auth","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // PR appears while agent is still running.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/add-auth","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","pr":{"url":"https://github.com/ApiariTools/hive/pull/42","title":"Add OAuth support"}}]}"#,
        );

        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert_eq!(pr_notes.len(), 1, "should emit exactly one PrOpened");

        if let SwarmNotification::PrOpened {
            worktree_id,
            branch,
            pr_url,
            pr_title,
            ..
        } = &pr_notes[0]
        {
            assert_eq!(worktree_id, "1");
            assert_eq!(branch, "swarm/add-auth");
            assert_eq!(pr_url, "https://github.com/ApiariTools/hive/pull/42");
            assert_eq!(pr_title.as_deref(), Some("Add OAuth support"));
        }

        let msg = pr_notes[0].format_telegram();
        assert!(msg.contains("PR opened"));
        assert!(msg.contains("add-auth"));
        assert!(msg.contains("Add OAuth support"));
        assert!(msg.contains("https://github.com/ApiariTools/hive/pull/42"));
        assert!(msg.contains("Duration:"));
    }

    #[test]
    fn test_pr_opened_same_url_no_duplicate() {
        let mut file = NamedTempFile::new().unwrap();
        // Initial state: agent running with a PR already set.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/42"}}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Same PR URL on next poll — should NOT emit PrOpened again.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/42"}}]}"#,
        );

        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert!(
            pr_notes.is_empty(),
            "should NOT emit PrOpened when pr_url stays the same"
        );
    }

    #[test]
    fn test_pr_opened_changed_url_no_spam() {
        let mut file = NamedTempFile::new().unwrap();
        // Initial state: agent running with a PR already set.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/42"}}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // PR URL changed — should NOT emit PrOpened (only None→Some triggers).
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/99"}}]}"#,
        );

        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert!(
            pr_notes.is_empty(),
            "should NOT emit PrOpened when pr_url changes (Some→Some)"
        );
    }

    #[test]
    fn test_pr_opened_format_telegram() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/add-auth".into(),
            pr_url: "https://github.com/ApiariTools/hive/pull/42".into(),
            pr_title: Some("Add OAuth support".into()),
            duration: "15m".into(),
        };
        let msg = pr.format_telegram();
        assert_eq!(
            msg,
            "🔗 *PR opened* — add-auth\nAdd OAuth support\nhttps://github.com/ApiariTools/hive/pull/42\nDuration: 15m"
        );

        // Without pr_title.
        let pr_no_title = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/fix-bug".into(),
            pr_url: "https://github.com/ApiariTools/hive/pull/7".into(),
            pr_title: None,
            duration: "3m".into(),
        };
        let msg2 = pr_no_title.format_telegram();
        assert_eq!(
            msg2,
            "🔗 *PR opened* — fix-bug\nhttps://github.com/ApiariTools/hive/pull/7\nDuration: 3m"
        );
    }

    #[test]
    fn test_pr_opened_worktree_id() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "xyz".into(),
            branch: "swarm/test".into(),
            pr_url: "https://example.com/pr/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        assert_eq!(pr.worktree_id(), "xyz");
    }

    #[test]
    fn test_agent_waiting_transition() {
        let mut file = NamedTempFile::new().unwrap();
        // Initial state: agent running, not waiting.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/do-stuff","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","summary":"Fix the thing"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Agent transitions to waiting.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/do-stuff","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","summary":"Fix the thing","agent_session_status":"waiting"}]}"#,
        );

        let notes = watcher.poll();
        let waiting_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting_notes.len(),
            1,
            "should emit exactly one AgentWaiting"
        );

        let msg = waiting_notes[0].format_telegram();
        assert!(msg.contains("Worker waiting"));
        assert!(msg.contains("do-stuff"));
        assert!(msg.contains("Fix the thing"));
        assert!(msg.contains("swarm send 1"));
    }

    #[test]
    fn test_agent_waiting_no_duplicate() {
        let mut file = NamedTempFile::new().unwrap();
        // Initial state: agent already waiting.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Second poll — should emit AgentWaiting (re-notify for workers waiting at init).
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );
        let notes = watcher.poll();
        let waiting_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting_notes.len(),
            1,
            "should emit AgentWaiting on second poll for workers waiting at init"
        );

        // Third poll with same state — should NOT emit again.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );
        let notes = watcher.poll();
        let waiting_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting_notes.is_empty(),
            "should NOT emit AgentWaiting again after the re-notify"
        );
    }

    #[test]
    fn test_agent_waiting_retriggers_after_active() {
        let mut file = NamedTempFile::new().unwrap();
        // Agent starts waiting.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Agent becomes active (got input).
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );
        let notes = watcher.poll();
        assert!(
            notes
                .iter()
                .all(|n| !matches!(n, SwarmNotification::AgentWaiting { .. })),
            "should not emit when transitioning away from waiting"
        );

        // Agent finishes task and goes back to waiting.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );
        let notes = watcher.poll();
        let waiting_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting_notes.len(),
            1,
            "should re-emit AgentWaiting after active→waiting transition"
        );
    }

    #[test]
    fn test_agent_waiting_format_no_summary() {
        let waiting = SwarmNotification::AgentWaiting {
            worktree_id: "abc".into(),
            branch: "swarm/my-task".into(),
            summary: None,
            pr_url: None,
        };
        let msg = waiting.format_telegram();
        assert_eq!(
            msg,
            "⏳ *Worker waiting* — my-task\n\nReply: `swarm send abc <message>`"
        );
    }

    #[test]
    fn test_agent_waiting_includes_pr_url_when_set() {
        let mut file = NamedTempFile::new().unwrap();
        // Agent running, no PR yet.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/do-stuff","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","summary":"Fix the thing"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Agent transitions to waiting AND has a PR.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/do-stuff","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","summary":"Fix the thing","agent_session_status":"waiting","pr":{"url":"https://github.com/ApiariTools/hive/pull/77"}}]}"#,
        );

        let notes = watcher.poll();
        let waiting_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting_notes.len(),
            1,
            "should emit exactly one AgentWaiting"
        );

        let msg = waiting_notes[0].format_telegram();
        assert!(msg.contains("Worker waiting"));
        assert!(msg.contains("do-stuff"));
        assert!(msg.contains("Fix the thing"));
        assert!(
            msg.contains("PR: https://github.com/ApiariTools/hive/pull/77"),
            "message should contain PR URL: {msg}"
        );
        assert!(msg.contains("swarm send 1"));
    }

    #[test]
    fn test_summary_line_spawned() {
        let n = SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/fix-auth".into(),
            summary: Some("Fix authentication bug".into()),
        };
        assert_eq!(
            n.summary_line(),
            "New agent spawned — fix-auth (Fix authentication bug)"
        );

        let n2 = SwarmNotification::AgentSpawned {
            worktree_id: "2".into(),
            branch: "swarm/cleanup".into(),
            summary: None,
        };
        assert_eq!(n2.summary_line(), "New agent spawned — cleanup");
    }

    #[test]
    fn test_summary_line_completed() {
        let n = SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/done".into(),
            duration: "5m".into(),
            pr_url: Some("https://github.com/example/pull/1".into()),
        };
        assert_eq!(
            n.summary_line(),
            "Agent completed — done (PR: https://github.com/example/pull/1)"
        );

        let n2 = SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/done".into(),
            duration: "5m".into(),
            pr_url: None,
        };
        assert_eq!(n2.summary_line(), "Agent completed — done");
    }

    #[test]
    fn test_summary_line_closed() {
        let n = SwarmNotification::AgentClosed {
            worktree_id: "1".into(),
            branch: "swarm/old".into(),
            duration: "10m".into(),
            pr_url: None,
        };
        assert_eq!(n.summary_line(), "Agent closed — old");
    }

    #[test]
    fn test_summary_line_pr_opened() {
        let n = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://github.com/example/pull/42".into(),
            pr_title: Some("Add feature".into()),
            duration: "8m".into(),
        };
        assert_eq!(
            n.summary_line(),
            "PR opened — feat (https://github.com/example/pull/42)"
        );
    }

    #[test]
    fn test_summary_line_stalled() {
        let idle = SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/stuck".into(),
            stall_kind: StallKind::Idle { minutes: 15 },
        };
        assert_eq!(idle.summary_line(), "Agent stalled — stuck (idle 15m)");

        let perm = SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/blocked".into(),
            stall_kind: StallKind::PermissionBlock {
                tool: "Write".into(),
            },
        };
        assert_eq!(
            perm.summary_line(),
            "Agent stalled — blocked (permission: Write)"
        );
    }

    // --- Tests for waiting_at_init re-notification ---

    #[test]
    fn test_waiting_at_init_fires_on_second_poll() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker already in "waiting" when daemon starts.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/stale-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Do the thing"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        let notes = watcher.poll(); // Initialize
        assert!(notes.is_empty(), "first poll should not notify");
        assert!(watcher.waiting_at_init.contains("w1"));

        // Second poll — still waiting, no PR → should re-emit AgentWaiting.
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].worktree_id(), "w1");
        let msg = waiting[0].format_telegram();
        assert!(msg.contains("Worker waiting"));
        assert!(msg.contains("Do the thing"));
    }

    #[test]
    fn test_waiting_at_init_with_pr_url_no_notify() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker already waiting AND already has a PR.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/done-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","pr":{"url":"https://github.com/ApiariTools/hive/pull/99"}}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Second poll — still waiting but has pr_url → no re-notify.
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting.is_empty(),
            "should NOT re-notify waiting worker that already has a PR"
        );
    }

    #[test]
    fn test_running_to_waiting_still_fires_immediately() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker starts in "active" (running) state.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/running","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize
        assert!(
            watcher.waiting_at_init.is_empty(),
            "active worker should not be in waiting_at_init"
        );

        // Worker transitions to waiting — standard detection should fire.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/running","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Need input"}]}"#,
        );
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting.len(),
            1,
            "running→waiting transition should fire AgentWaiting immediately"
        );
    }

    #[test]
    fn test_waiting_at_init_gone_before_second_poll() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker already waiting at init.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/ephemeral","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize
        assert!(watcher.waiting_at_init.contains("w1"));

        // Worker disappears before second poll.
        write_state(&mut file, r#"{"worktrees":[]}"#);
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting.is_empty(),
            "should NOT notify for worker that disappeared before second poll"
        );
        // Should still get AgentClosed though.
        let closed: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentClosed { .. }))
            .collect();
        assert_eq!(closed.len(), 1);
    }

    #[test]
    fn test_waiting_at_init_processed_only_once() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker already waiting at init.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/once","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize
        assert!(!watcher.waiting_at_init.is_empty());

        // Second poll — fires re-notify, drains waiting_at_init.
        let notes = watcher.poll();
        assert_eq!(
            notes
                .iter()
                .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
                .count(),
            1
        );
        assert!(
            watcher.waiting_at_init.is_empty(),
            "waiting_at_init should be drained after second poll"
        );

        // Third poll — same state, no re-notify.
        let notes = watcher.poll();
        assert_eq!(
            notes
                .iter()
                .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
                .count(),
            0,
            "waiting_at_init should not produce notifications on subsequent polls"
        );

        // Fourth poll — still no re-notify.
        let notes = watcher.poll();
        assert_eq!(
            notes
                .iter()
                .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
                .count(),
            0,
            "waiting_at_init must remain empty after being drained"
        );
    }

    // --- Edge case tests ---

    #[test]
    fn test_waiting_to_running_to_waiting_fires_twice() {
        let mut file = NamedTempFile::new().unwrap();
        // Start with a running agent.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // First transition: active → waiting.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Need input 1"}]}"#,
        );
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(waiting.len(), 1, "first waiting transition should fire");

        // Back to active (user sent input).
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );
        let notes = watcher.poll();
        assert!(
            notes
                .iter()
                .all(|n| !matches!(n, SwarmNotification::AgentWaiting { .. })),
            "should not emit when transitioning away from waiting"
        );

        // Second transition: active → waiting again.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Need input 2"}]}"#,
        );
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting.len(),
            1,
            "second waiting transition should also fire"
        );
    }

    #[test]
    fn test_pr_already_set_at_init_no_pr_opened() {
        let mut file = NamedTempFile::new().unwrap();
        // Worker has pr_url from the start.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/example/pull/1"}}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize — pr_url is recorded in known state.

        // Same state on next poll — pr_url was already Some, no None→Some transition.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/example/pull/1"}}]}"#,
        );
        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert!(
            pr_notes.is_empty(),
            "should NOT emit PrOpened when pr_url was already set at initialization"
        );
    }

    #[test]
    fn test_multiple_workers_appear_simultaneously() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize with no worktrees.

        // Two new workers appear in the same poll cycle.
        write_state(
            &mut file,
            r#"{"worktrees":[
                {"id":"1","branch":"swarm/task-a","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","summary":"Task A"},
                {"id":"2","branch":"swarm/task-b","agent":{"pane_id":"%2"},"created_at":"2026-02-22T10:01:00-08:00","summary":"Task B"}
            ]}"#,
        );

        let notes = watcher.poll();
        let spawned: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentSpawned { .. }))
            .collect();
        assert_eq!(
            spawned.len(),
            2,
            "should emit AgentSpawned for both new workers"
        );

        // Verify both worktree IDs are represented.
        let ids: Vec<&str> = spawned.iter().map(|n| n.worktree_id()).collect();
        assert!(ids.contains(&"1"), "should have worktree 1");
        assert!(ids.contains(&"2"), "should have worktree 2");
    }

    #[test]
    fn test_state_file_disappears_no_panic() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00"}]}"#,
        );

        let path = file.path().to_path_buf();
        let mut watcher = SwarmWatcher::new(path.clone());
        watcher.poll(); // Initialize with one worktree.
        assert_eq!(watcher.known.len(), 1);

        // Delete the state file.
        std::fs::remove_file(&path).unwrap();

        // Should not panic, returns empty vec (load_state returns None).
        let notes = watcher.poll();
        assert!(
            notes.is_empty(),
            "should return empty notifications when state file disappears"
        );

        // Known state is preserved (stale) since poll short-circuited.
        assert_eq!(
            watcher.known.len(),
            1,
            "known state should be unchanged when file is missing"
        );
    }

    // --- Tests for PR notification on daemon startup ---

    #[test]
    fn test_pr_opened_fires_on_startup_for_existing_pr() {
        let mut file = NamedTempFile::new().unwrap();
        let hive_dir = tempfile::tempdir().unwrap();

        // Worker has pr_url from the start — no pr_notified.json exists.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/1","title":"Add feature"}}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_hive_dir(hive_dir.path().to_path_buf());
        watcher.poll(); // Initialize

        // Second poll — should emit PrOpened for the unnotified PR.
        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert_eq!(
            pr_notes.len(),
            1,
            "should emit PrOpened for existing PR on startup"
        );

        if let SwarmNotification::PrOpened {
            pr_url, pr_title, ..
        } = &pr_notes[0]
        {
            assert_eq!(pr_url, "https://github.com/ApiariTools/hive/pull/1");
            assert_eq!(pr_title.as_deref(), Some("Add feature"));
        }

        // Verify pr_notified.json was written.
        let notified_path = hive_dir.path().join("pr_notified.json");
        assert!(notified_path.exists(), "pr_notified.json should be created");
        let content: HashSet<String> =
            serde_json::from_str(&std::fs::read_to_string(&notified_path).unwrap()).unwrap();
        assert!(content.contains("https://github.com/ApiariTools/hive/pull/1"));
    }

    #[test]
    fn test_pr_opened_not_duplicated_after_restart() {
        let mut file = NamedTempFile::new().unwrap();
        let hive_dir = tempfile::tempdir().unwrap();

        // Pre-populate pr_notified.json with the URL (simulates previous daemon run).
        let notified_path = hive_dir.path().join("pr_notified.json");
        std::fs::write(
            &notified_path,
            r#"["https://github.com/ApiariTools/hive/pull/1"]"#,
        )
        .unwrap();

        // Worker has the same pr_url.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/1"}}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_hive_dir(hive_dir.path().to_path_buf());
        watcher.poll(); // Initialize

        // Second poll — should NOT emit PrOpened (already notified).
        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert!(
            pr_notes.is_empty(),
            "should NOT emit PrOpened for already-notified PR"
        );
    }

    #[test]
    fn test_pr_opened_fires_for_new_pr_after_restart() {
        let mut file = NamedTempFile::new().unwrap();
        let hive_dir = tempfile::tempdir().unwrap();

        // Pre-populate pr_notified.json with one URL.
        let notified_path = hive_dir.path().join("pr_notified.json");
        std::fs::write(
            &notified_path,
            r#"["https://github.com/ApiariTools/hive/pull/1"]"#,
        )
        .unwrap();

        // Two workers: one with already-notified PR, one without any PR.
        write_state(
            &mut file,
            r#"{"worktrees":[
                {"id":"1","branch":"swarm/old-feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/1"}},
                {"id":"2","branch":"swarm/new-feat","agent":{"pane_id":"%2"},"created_at":"2026-02-22T11:00:00-08:00"}
            ]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_hive_dir(hive_dir.path().to_path_buf());
        watcher.poll(); // Initialize

        // No PrOpened on second poll (old PR is notified, new worker has no PR yet).
        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert!(pr_notes.is_empty(), "no new PRs yet");

        // New PR appears on worker 2 (None→Some transition).
        write_state(
            &mut file,
            r#"{"worktrees":[
                {"id":"1","branch":"swarm/old-feat","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/1"}},
                {"id":"2","branch":"swarm/new-feat","agent":{"pane_id":"%2"},"created_at":"2026-02-22T11:00:00-08:00","pr":{"url":"https://github.com/ApiariTools/hive/pull/42","title":"New feature"}}
            ]}"#,
        );

        let notes = watcher.poll();
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert_eq!(pr_notes.len(), 1, "should emit PrOpened for new PR only");

        if let SwarmNotification::PrOpened {
            worktree_id,
            pr_url,
            ..
        } = &pr_notes[0]
        {
            assert_eq!(worktree_id, "2");
            assert_eq!(pr_url, "https://github.com/ApiariTools/hive/pull/42");
        }

        // Verify pr_notified.json now contains both URLs.
        let content: HashSet<String> =
            serde_json::from_str(&std::fs::read_to_string(&notified_path).unwrap()).unwrap();
        assert!(content.contains("https://github.com/ApiariTools/hive/pull/1"));
        assert!(content.contains("https://github.com/ApiariTools/hive/pull/42"));
    }

    // --- Tests for fast-worker AgentWaiting miss ---

    #[test]
    fn test_new_worker_already_waiting_fires_agent_waiting() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize with no worktrees.

        // New worker appears already in "waiting" state (completed within one poll window).
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"fast-1","branch":"swarm/quick-fix","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Quick fix"}]}"#,
        );

        let notes = watcher.poll();
        let spawned: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentSpawned { .. }))
            .collect();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();

        assert_eq!(spawned.len(), 1, "should emit AgentSpawned");
        assert_eq!(
            waiting.len(),
            1,
            "should emit AgentWaiting for new worker already in waiting state"
        );
        assert_eq!(waiting[0].worktree_id(), "fast-1");

        let msg = waiting[0].format_telegram();
        assert!(msg.contains("Worker waiting"));
        assert!(msg.contains("quick-fix"));
        assert!(msg.contains("Quick fix"));
    }

    #[test]
    fn test_new_worker_not_waiting_no_premature_agent_waiting() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize with no worktrees.

        // New worker appears in "running" state (still working).
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"run-1","branch":"swarm/long-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active","summary":"Long task"}]}"#,
        );

        let notes = watcher.poll();
        let spawned: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentSpawned { .. }))
            .collect();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();

        assert_eq!(spawned.len(), 1, "should emit AgentSpawned");
        assert!(
            waiting.is_empty(),
            "should NOT emit AgentWaiting for worker in running state"
        );
    }
}
