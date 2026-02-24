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
use std::time::{Duration, Instant};

/// CI check status for a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiStatus {
    /// All checks passed.
    Passing,
    /// At least one check failed.
    Failing,
    /// At least one check is still pending/in-progress (none failing).
    Pending,
}

impl CiStatus {
    /// Format as a one-line CI status string for Telegram.
    pub fn status_line(&self) -> &'static str {
        match self {
            Self::Passing => "CI: ✅ passing",
            Self::Failing => "CI: ❌ failing",
            Self::Pending => "CI: ⏳ running",
        }
    }
}

/// Fetch CI check status for a PR URL using `gh pr checks`.
///
/// Returns `None` if the command fails, times out, or there are no checks.
/// Uses a 10-second timeout to avoid blocking notifications.
pub async fn fetch_ci_status(pr_url: &str) -> Option<CiStatus> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("gh")
            .args([
                "pr",
                "checks",
                pr_url,
                "--json",
                "name,state,conclusion",
                "--jq",
                "[.[] | {name,conclusion}]",
            ])
            .output(),
    )
    .await
    .ok()? // timeout
    .ok()?; // command error

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let checks: Vec<CiCheck> = serde_json::from_str(stdout.trim()).ok()?;

    if checks.is_empty() {
        return None;
    }

    let has_failure = checks.iter().any(|c| {
        matches!(
            c.conclusion.as_deref(),
            Some("failure")
                | Some("FAILURE")
                | Some("cancelled")
                | Some("CANCELLED")
                | Some("timed_out")
                | Some("TIMED_OUT")
                | Some("action_required")
                | Some("ACTION_REQUIRED")
                | Some("startup_failure")
                | Some("STARTUP_FAILURE")
        )
    });
    if has_failure {
        return Some(CiStatus::Failing);
    }

    let has_pending = checks.iter().any(|c| {
        matches!(
            c.conclusion.as_deref(),
            None | Some("") | Some("pending") | Some("PENDING")
        )
    });
    if has_pending {
        return Some(CiStatus::Pending);
    }

    Some(CiStatus::Passing)
}

#[derive(Debug, Deserialize)]
struct CiCheck {
    #[allow(dead_code)]
    name: Option<String>,
    conclusion: Option<String>,
}

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
        summary: Option<String>,
        duration: String,
        pr_url: Option<String>,
    },
    /// A worktree was removed entirely (closed or merged).
    AgentClosed {
        worktree_id: String,
        branch: String,
        summary: Option<String>,
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
                let lead = summary_or_short(summary, branch);
                format!("🐝 *Agent spawned* — {lead}\nBranch: `{branch}`")
            }
            Self::AgentCompleted {
                branch,
                summary,
                duration,
                pr_url,
                ..
            } => {
                let lead = summary_or_short(summary, branch);
                let pr_line = pr_url
                    .as_deref()
                    .map(|url| format!("\nPR: {url}"))
                    .unwrap_or_default();
                format!("✅ *Agent finished* — {lead}\nBranch: `{branch}` · {duration}{pr_line}")
            }
            Self::AgentClosed {
                branch,
                summary,
                duration,
                pr_url,
                ..
            } => {
                let lead = summary_or_short(summary, branch);
                let pr_line = pr_url
                    .as_deref()
                    .map(|url| format!("\nPR: {url}"))
                    .unwrap_or_default();
                format!("🗑 *Worker closed* — {lead}\nBranch: `{branch}` · {duration}{pr_line}")
            }
            Self::PrOpened {
                branch,
                pr_url,
                pr_title,
                duration,
                ..
            } => {
                let lead = pr_title.as_deref().unwrap_or_else(|| short_branch(branch));
                format!("🔗 *PR opened* — {lead}\n{pr_url}\nBranch: `{branch}` · {duration}")
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
                branch,
                summary,
                pr_url,
                ..
            } => {
                let lead = summary_or_short(summary, branch);
                let pr_line = pr_url
                    .as_deref()
                    .map(|url| format!("\nPR: {url}"))
                    .unwrap_or_default();
                format!(
                    "⏳ *Worker waiting* — {lead}\nBranch: `{branch}`{pr_line}\nReply here to send a message to this worker."
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

    /// Return inline keyboard button rows for this notification type.
    ///
    /// Returns empty vec for notification types that don't need action buttons.
    pub fn inline_buttons(&self) -> Vec<Vec<crate::channel::InlineButton>> {
        use crate::channel::InlineButton;

        match self {
            Self::PrOpened { worktree_id, .. } => {
                vec![vec![
                    InlineButton {
                        text: "✅ Merge PR".into(),
                        callback_data: format!("merge_pr:{worktree_id}"),
                    },
                    InlineButton {
                        text: "🗑 Close Worker".into(),
                        callback_data: format!("close_worker:{worktree_id}"),
                    },
                ]]
            }
            Self::AgentWaiting {
                worktree_id,
                pr_url,
                ..
            } => {
                let mut row = Vec::new();
                if pr_url.is_some() {
                    row.push(InlineButton {
                        text: "✅ Merge PR".into(),
                        callback_data: format!("merge_pr:{worktree_id}"),
                    });
                }
                row.push(InlineButton {
                    text: "🗑 Close Worker".into(),
                    callback_data: format!("close_worker:{worktree_id}"),
                });
                vec![row]
            }
            Self::AgentStalled { worktree_id, .. } => {
                vec![vec![InlineButton {
                    text: "🗑 Close Worker".into(),
                    callback_data: format!("close_worker:{worktree_id}"),
                }]]
            }
            Self::AgentSpawned { .. } | Self::AgentCompleted { .. } | Self::AgentClosed { .. } => {
                vec![]
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

    /// Format a PrOpened notification with CI status appended.
    /// For non-PrOpened notifications, falls back to `format_telegram()`.
    pub fn format_telegram_with_ci(&self, ci: Option<&CiStatus>) -> String {
        match self {
            Self::PrOpened {
                branch,
                pr_url,
                pr_title,
                duration,
                ..
            } => {
                let lead = pr_title.as_deref().unwrap_or_else(|| short_branch(branch));
                let ci_line = ci
                    .map(|s| format!("\n{}", s.status_line()))
                    .unwrap_or_default();
                format!(
                    "🔗 *PR opened* — {lead}\n{pr_url}\nBranch: `{branch}` · {duration}{ci_line}"
                )
            }
            _ => self.format_telegram(),
        }
    }

    /// Return inline keyboard buttons for a PrOpened notification with CI awareness.
    /// For non-PrOpened notifications, falls back to `inline_buttons()`.
    pub fn inline_buttons_with_ci(
        &self,
        ci: Option<&CiStatus>,
    ) -> Vec<Vec<crate::channel::InlineButton>> {
        use crate::channel::InlineButton;

        match self {
            Self::PrOpened { worktree_id, .. } => {
                let merge_btn = match ci {
                    Some(CiStatus::Passing) | None => InlineButton {
                        text: "✅ Merge PR".into(),
                        callback_data: format!("merge_pr:{worktree_id}"),
                    },
                    Some(CiStatus::Pending) => InlineButton {
                        text: "⏳ CI still running".into(),
                        callback_data: format!("ci_pending:{worktree_id}"),
                    },
                    Some(CiStatus::Failing) => InlineButton {
                        text: "❌ CI failing — merge anyway?".into(),
                        callback_data: format!("ci_failing_merge:{worktree_id}"),
                    },
                };
                vec![vec![
                    merge_btn,
                    InlineButton {
                        text: "🗑 Close Worker".into(),
                        callback_data: format!("close_worker:{worktree_id}"),
                    },
                ]]
            }
            _ => self.inline_buttons(),
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
    /// Seconds a worker must be in "waiting" before AgentWaiting fires (0 = disabled).
    waiting_debounce_secs: u64,
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
    summary: Option<String>,
    /// When this worker entered "waiting" state (for debounce tracking).
    waiting_since: Option<Instant>,
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
            waiting_debounce_secs: 0,
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

    /// Set the waiting debounce duration (in seconds). 0 disables debounce
    /// (AgentWaiting fires immediately on transition, backwards compatible).
    pub fn set_waiting_debounce(&mut self, secs: u64) {
        self.waiting_debounce_secs = secs;
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
                        summary: wt.summary.clone(),
                        waiting_since: None, // Handled by waiting_at_init path, not debounce.
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
                        summary: wt.summary.clone(),
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

                // NOTIFICATION DESIGN: Debounce AgentWaiting to prevent false positives.
                // Workers briefly enter "waiting" state between tool calls. Only emit
                // AgentWaiting after the worker has been waiting for waiting_debounce_secs.
                // If debounce is 0, emit immediately (backwards compatible, useful in tests).
                // Also suppress when pr_url is set — PrOpened is the actionable signal.
                // When debounce > 0, waiting_since is set in the known state update
                // below. The debounce check pass will emit AgentWaiting once enough
                // time has elapsed.
                if prev.agent_session_status.as_deref() != Some("waiting")
                    && wt.agent_session_status.as_deref() == Some("waiting")
                    && wt.pr_url().is_none()
                    && self.waiting_debounce_secs == 0
                {
                    notifications.push(SwarmNotification::AgentWaiting {
                        worktree_id: id.clone(),
                        branch: wt.branch.clone(),
                        summary: wt.summary.clone(),
                        pr_url: None,
                    });
                }
            } else {
                // Worktree was removed (closed or merged).
                let duration = format_duration(prev.created_at);
                notifications.push(SwarmNotification::AgentClosed {
                    worktree_id: id.clone(),
                    branch: prev.branch.clone(),
                    summary: prev.summary.clone(),
                    duration,
                    pr_url: prev.pr_url.clone(),
                });
            }
        }

        // NOTIFICATION DESIGN: Emit exactly ONE notification per new worker.
        // - If already waiting with PR → PrOpened (the actionable signal)
        // - If already waiting without PR → AgentWaiting
        // - Otherwise (running) → AgentSpawned
        // This prevents AgentSpawned+AgentWaiting double-notifications for fast workers.
        for wt in &state.worktrees {
            if !self.known.contains_key(&wt.id) {
                if wt.agent_session_status.as_deref() == Some("waiting") {
                    if let Some(url) = wt.pr_url() {
                        // Fast worker: appeared already with PR — emit PrOpened.
                        // (PrOpened transition check won't fire since there's no prev state.)
                        let duration = format_duration(wt.created_at);
                        notifications.push(SwarmNotification::PrOpened {
                            worktree_id: wt.id.clone(),
                            branch: wt.branch.clone(),
                            pr_url: url.clone(),
                            pr_title: wt.pr_title(),
                            duration,
                        });
                        self.pr_notified.insert(url);
                        pr_notified_changed = true;
                    } else {
                        // Fast worker: appeared already waiting, no PR yet.
                        if self.waiting_debounce_secs == 0 {
                            notifications.push(SwarmNotification::AgentWaiting {
                                worktree_id: wt.id.clone(),
                                branch: wt.branch.clone(),
                                summary: wt.summary.clone(),
                                pr_url: None,
                            });
                        }
                        // When debounce > 0, waiting_since is set in the known state
                        // update below. The debounce check pass handles the delay.
                    }
                } else {
                    // Normal spawn: worker is running, not yet waiting.
                    notifications.push(SwarmNotification::AgentSpawned {
                        worktree_id: wt.id.clone(),
                        branch: wt.branch.clone(),
                        summary: wt.summary.clone(),
                    });
                }
            }
        }

        // Update known state, carrying forward waiting_since for debounce tracking.
        let old_known = std::mem::take(&mut self.known);
        for wt in &state.worktrees {
            let is_waiting = wt.agent_session_status.as_deref() == Some("waiting");
            let old = old_known.get(&wt.id);
            let was_waiting =
                old.is_some_and(|o| o.agent_session_status.as_deref() == Some("waiting"));

            // NOTIFICATION DESIGN: Track when a worker entered "waiting" state.
            // - Transitioning TO waiting: record Instant::now()
            // - Already waiting: preserve existing timestamp
            // - Not waiting: clear (worker left waiting, debounce resets)
            let waiting_since = if is_waiting {
                if was_waiting {
                    old.and_then(|o| o.waiting_since)
                } else {
                    Some(Instant::now())
                }
            } else {
                None
            };

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
                    summary: wt.summary.clone(),
                    waiting_since,
                },
            );
        }

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

        // NOTIFICATION DESIGN: Emit debounced AgentWaiting for workers that have
        // been in "waiting" state long enough. This prevents false positives from
        // brief mid-task flickers while still notifying for genuine human-input waits.
        // A real wait is sustained across multiple polls; a tool-call gap clears
        // within one 15s poll cycle, so waiting_since gets reset before the threshold.
        if self.waiting_debounce_secs > 0 {
            for (id, tracked) in &mut self.known {
                if tracked.agent_session_status.as_deref() == Some("waiting")
                    && tracked.pr_url.is_none()
                    && let Some(since) = tracked.waiting_since
                    && since.elapsed().as_secs() >= self.waiting_debounce_secs
                {
                    notifications.push(SwarmNotification::AgentWaiting {
                        worktree_id: id.clone(),
                        branch: tracked.branch.clone(),
                        summary: tracked.summary.clone(),
                        pr_url: None,
                    });
                    tracked.waiting_since = None; // fired, don't repeat
                }
            }
        }

        // Check for stalled agents.
        if self.stall_timeout_secs > 0 {
            notifications.extend(self.check_stalls());
        }

        // NOTIFICATION DESIGN: Deduplicate AgentCompleted + AgentClosed for same worker.
        // When closing a claude-tui worker, both can fire in the same poll cycle
        // (agent pane gone + worktree removed). Keep only AgentClosed — it's the
        // terminal event and includes all relevant info (PR url from prev state).
        let completed_and_closed: HashSet<String> = notifications
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentCompleted { .. }))
            .map(|n| n.worktree_id().to_owned())
            .filter(|id| {
                notifications
                    .iter()
                    .any(|n| matches!(n, SwarmNotification::AgentClosed { worktree_id, .. } if worktree_id == id))
            })
            .collect();
        if !completed_and_closed.is_empty() {
            notifications.retain(|n| {
                !matches!(n, SwarmNotification::AgentCompleted { worktree_id, .. }
                    if completed_and_closed.contains(worktree_id.as_str()))
            });
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

/// Return summary if set, else the short branch name.
fn summary_or_short<'a>(summary: &'a Option<String>, branch: &'a str) -> &'a str {
    summary.as_deref().unwrap_or_else(|| short_branch(branch))
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
        assert!(msg.contains("Agent finished"));
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
        assert!(msg.contains("Worker closed"));
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
        assert!(msg.contains("Agent spawned"));
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
        assert!(spawned.format_telegram().contains("Agent spawned"));
        assert!(spawned.format_telegram().contains("Do the thing"));

        let completed = SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/done".into(),
            summary: None,
            duration: "5m".into(),
            pr_url: None,
        };
        assert!(completed.format_telegram().contains("Agent finished"));
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
            summary: None,
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
            summary: None,
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
        assert!(msg.contains("Branch:"));
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
            "🔗 *PR opened* — Add OAuth support\nhttps://github.com/ApiariTools/hive/pull/42\nBranch: `swarm/add-auth` · 15m"
        );

        // Without pr_title — falls back to short branch.
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
            "🔗 *PR opened* — fix-bug\nhttps://github.com/ApiariTools/hive/pull/7\nBranch: `swarm/fix-bug` · 3m"
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
        assert!(msg.contains("Reply here"));
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
            "⏳ *Worker waiting* — my-task\nBranch: `swarm/my-task`\nReply here to send a message to this worker."
        );
    }

    #[test]
    fn test_agent_waiting_suppressed_when_pr_set() {
        // NOTIFICATION DESIGN: When pr_url is set, PrOpened is the actionable
        // signal — AgentWaiting is redundant and creates noise.
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

        // PrOpened should fire (None→Some transition).
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert_eq!(pr_notes.len(), 1, "PrOpened should fire");

        // AgentWaiting should NOT fire — PrOpened is the signal.
        let waiting_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting_notes.is_empty(),
            "AgentWaiting should be suppressed when pr_url is set"
        );
    }

    #[test]
    fn test_agent_waiting_fires_when_no_pr() {
        // NOTIFICATION DESIGN: When no PR is set, AgentWaiting is the signal.
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/do-stuff","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","summary":"Fix the thing"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Agent transitions to waiting, no PR.
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
            "AgentWaiting should fire when no PR is set"
        );
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
            summary: None,
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
            summary: None,
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
            summary: None,
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
    fn test_fast_worker_with_pr_emits_pr_opened_only() {
        // NOTIFICATION DESIGN: Fast worker appeared already with PR — emit only
        // PrOpened. No AgentSpawned or AgentWaiting to avoid noise.
        let mut file = NamedTempFile::new().unwrap();
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize with no worktrees.

        // New worker appears mid-session already in "waiting" state with PR.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"fast-1","branch":"swarm/quick-fix","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Fix typo","pr":{"url":"https://github.com/ApiariTools/hive/pull/55","title":"Fix typo in README"}}]}"#,
        );

        let notes = watcher.poll();

        // Only PrOpened should fire.
        let pr_notes: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert_eq!(pr_notes.len(), 1, "should emit PrOpened");

        if let SwarmNotification::PrOpened {
            worktree_id,
            pr_url,
            pr_title,
            ..
        } = &pr_notes[0]
        {
            assert_eq!(worktree_id, "fast-1");
            assert_eq!(pr_url, "https://github.com/ApiariTools/hive/pull/55");
            assert_eq!(pr_title.as_deref(), Some("Fix typo in README"));
        }

        // No AgentSpawned or AgentWaiting.
        let spawned: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentSpawned { .. }))
            .collect();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            spawned.is_empty(),
            "no AgentSpawned for fast worker with PR"
        );
        assert!(waiting.is_empty(), "no AgentWaiting when PR is the signal");
    }

    #[test]
    fn test_fast_worker_no_pr_emits_waiting_only() {
        // NOTIFICATION DESIGN: Fast worker appeared already waiting, no PR —
        // emit only AgentWaiting. No AgentSpawned.
        let mut file = NamedTempFile::new().unwrap();
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize with no worktrees.

        // New worker appears mid-session already waiting, no PR.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"fast-2","branch":"swarm/quick-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Do the thing"}]}"#,
        );

        let notes = watcher.poll();

        // Only AgentWaiting should fire.
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(waiting.len(), 1, "should emit AgentWaiting");
        assert_eq!(waiting[0].worktree_id(), "fast-2");

        // No AgentSpawned.
        let spawned: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentSpawned { .. }))
            .collect();
        assert!(
            spawned.is_empty(),
            "no AgentSpawned for fast worker already waiting"
        );
    }

    #[test]
    fn test_normal_spawn_emits_spawned_only() {
        // NOTIFICATION DESIGN: Normal worker appears running — emit only AgentSpawned.
        let mut file = NamedTempFile::new().unwrap();
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize with no worktrees.

        // New worker appears in running state.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"run-1","branch":"swarm/big-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active","summary":"Big refactor"}]}"#,
        );

        let notes = watcher.poll();

        let spawned: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentSpawned { .. }))
            .collect();
        assert_eq!(spawned.len(), 1, "should emit AgentSpawned");

        // No AgentWaiting or PrOpened.
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        let pr: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::PrOpened { .. }))
            .collect();
        assert!(waiting.is_empty(), "no AgentWaiting for running worker");
        assert!(pr.is_empty(), "no PrOpened for running worker");
    }

    #[test]
    fn test_new_worker_running_no_waiting_notification() {
        let mut file = NamedTempFile::new().unwrap();
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize with no worktrees.

        // New worker appears mid-session in "active" (running) state.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"run-1","branch":"swarm/big-task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active","summary":"Big refactor"}]}"#,
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
            "should NOT emit AgentWaiting for worker in active/running state"
        );
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

    // --- Tests for notification deduplication ---

    #[test]
    fn test_completed_and_closed_dedup() {
        // NOTIFICATION DESIGN: When a worktree is removed, only AgentClosed
        // should fire — AgentCompleted is redundant. The dedup logic in poll()
        // removes AgentCompleted when AgentClosed also fires for the same worker.
        let mut file = NamedTempFile::new().unwrap();
        // Worker with agent pane running.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","summary":"Do stuff"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.poll(); // Initialize

        // Worktree removed entirely (agent was still running).
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let notes = watcher.poll();

        // Only AgentClosed should fire, not AgentCompleted.
        let completed: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentCompleted { .. }))
            .collect();
        let closed: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentClosed { .. }))
            .collect();

        assert!(
            completed.is_empty(),
            "AgentCompleted should not fire when worktree is removed"
        );
        assert_eq!(closed.len(), 1, "AgentClosed should fire");
        assert_eq!(closed[0].worktree_id(), "1");
    }

    // --- Tests for AgentWaiting debounce ---

    #[test]
    fn test_debounce_suppresses_brief_flicker() {
        // NOTIFICATION DESIGN: With debounce enabled, a worker that briefly enters
        // "waiting" then returns to "active" should NOT trigger AgentWaiting.
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_waiting_debounce(30); // 30s debounce
        watcher.poll(); // Initialize

        // Worker transitions to waiting (mid-task tool call gap).
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting.is_empty(),
            "debounce should suppress immediate AgentWaiting"
        );

        // Worker returns to active (tool call resumes).
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting.is_empty(),
            "no AgentWaiting after returning to active — flicker successfully suppressed"
        );
    }

    #[test]
    fn test_debounce_zero_fires_immediately() {
        // With debounce disabled (0), AgentWaiting fires on the transition.
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        // debounce defaults to 0 — explicit for clarity
        watcher.set_waiting_debounce(0);
        watcher.poll(); // Initialize

        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting.len(),
            1,
            "debounce=0 should fire AgentWaiting immediately"
        );
    }

    #[test]
    fn test_debounce_fast_worker_suppressed() {
        // NOTIFICATION DESIGN: New worker appearing already in "waiting" with
        // debounce enabled should NOT immediately fire AgentWaiting.
        let mut file = NamedTempFile::new().unwrap();
        write_state(&mut file, r#"{"worktrees":[]}"#);

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_waiting_debounce(30);
        watcher.poll(); // Initialize

        // New worker appears already waiting.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"fast-1","branch":"swarm/quick","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Quick task"}]}"#,
        );
        let notes = watcher.poll();

        // Should emit AgentSpawned or nothing, but NOT AgentWaiting (debounce).
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting.is_empty(),
            "debounce should suppress AgentWaiting for fast worker"
        );
    }

    #[test]
    fn test_debounce_resets_on_active_waiting_cycle() {
        // When a worker goes waiting→active→waiting, the debounce timer resets.
        // With debounce > 0, neither transition should fire immediately.
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_waiting_debounce(30);
        watcher.poll(); // Initialize

        // First transition: active → waiting
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );
        let notes = watcher.poll();
        assert!(
            notes
                .iter()
                .all(|n| !matches!(n, SwarmNotification::AgentWaiting { .. })),
            "first transition suppressed by debounce"
        );

        // Verify waiting_since is set.
        assert!(
            watcher.known["1"].waiting_since.is_some(),
            "waiting_since should be set on transition to waiting"
        );

        // Back to active.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );
        watcher.poll();
        assert!(
            watcher.known["1"].waiting_since.is_none(),
            "waiting_since should be cleared when leaving waiting state"
        );

        // Second transition: active → waiting again.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting"}]}"#,
        );
        let notes = watcher.poll();
        assert!(
            notes
                .iter()
                .all(|n| !matches!(n, SwarmNotification::AgentWaiting { .. })),
            "second transition also suppressed by debounce"
        );

        // waiting_since should be freshly set (reset, not carried from first cycle).
        assert!(
            watcher.known["1"].waiting_since.is_some(),
            "waiting_since should be set again on re-entry to waiting"
        );
    }

    #[test]
    fn test_debounce_fires_when_threshold_reached() {
        // NOTIFICATION DESIGN: With a very short debounce (1 second), verify that
        // the debounce check pass fires AgentWaiting after the threshold.
        // Note: This test uses a real sleep, so the debounce is kept very short.
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"active"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_waiting_debounce(1); // 1 second debounce
        watcher.poll(); // Initialize

        // Transition to waiting.
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"1","branch":"swarm/task","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Need input"}]}"#,
        );
        let notes = watcher.poll();
        assert!(
            notes
                .iter()
                .all(|n| !matches!(n, SwarmNotification::AgentWaiting { .. })),
            "should not fire immediately with debounce=1"
        );

        // Wait for the debounce threshold.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Next poll — debounce check should fire.
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting.len(),
            1,
            "debounce check should fire AgentWaiting after threshold"
        );

        // Subsequent poll — should NOT fire again (waiting_since cleared).
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert!(
            waiting.is_empty(),
            "should not re-fire after debounce already emitted"
        );
    }

    #[test]
    fn test_debounce_waiting_at_init_fires_regardless() {
        // Workers waiting at daemon startup should re-emit immediately via the
        // waiting_at_init path, even when debounce is enabled.
        let mut file = NamedTempFile::new().unwrap();
        write_state(
            &mut file,
            r#"{"worktrees":[{"id":"w1","branch":"swarm/stale","agent":{"pane_id":"%1"},"created_at":"2026-02-22T10:00:00-08:00","agent_kind":"claude-tui","agent_session_status":"waiting","summary":"Waiting for input"}]}"#,
        );

        let mut watcher = SwarmWatcher::new(file.path().to_path_buf());
        watcher.set_waiting_debounce(30); // 30s debounce
        watcher.poll(); // Initialize

        // Second poll — waiting_at_init should fire regardless of debounce.
        let notes = watcher.poll();
        let waiting: Vec<_> = notes
            .iter()
            .filter(|n| matches!(n, SwarmNotification::AgentWaiting { .. }))
            .collect();
        assert_eq!(
            waiting.len(),
            1,
            "waiting_at_init should fire even with debounce enabled"
        );
    }

    // --- Tests for CI status ---

    #[test]
    fn test_ci_status_line() {
        assert_eq!(CiStatus::Passing.status_line(), "CI: ✅ passing");
        assert_eq!(CiStatus::Failing.status_line(), "CI: ❌ failing");
        assert_eq!(CiStatus::Pending.status_line(), "CI: ⏳ running");
    }

    #[test]
    fn test_format_telegram_with_ci_passing() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/add-auth".into(),
            pr_url: "https://github.com/ApiariTools/hive/pull/42".into(),
            pr_title: Some("Add OAuth support".into()),
            duration: "15m".into(),
        };
        let msg = pr.format_telegram_with_ci(Some(&CiStatus::Passing));
        assert_eq!(
            msg,
            "🔗 *PR opened* — Add OAuth support\nhttps://github.com/ApiariTools/hive/pull/42\nBranch: `swarm/add-auth` · 15m\nCI: ✅ passing"
        );
    }

    #[test]
    fn test_format_telegram_with_ci_failing() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/fix".into(),
            pr_url: "https://github.com/example/pull/1".into(),
            pr_title: None,
            duration: "5m".into(),
        };
        let msg = pr.format_telegram_with_ci(Some(&CiStatus::Failing));
        assert!(msg.contains("CI: ❌ failing"));
    }

    #[test]
    fn test_format_telegram_with_ci_pending() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/fix".into(),
            pr_url: "https://github.com/example/pull/1".into(),
            pr_title: None,
            duration: "5m".into(),
        };
        let msg = pr.format_telegram_with_ci(Some(&CiStatus::Pending));
        assert!(msg.contains("CI: ⏳ running"));
    }

    #[test]
    fn test_format_telegram_with_ci_none() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "1".into(),
            branch: "swarm/fix".into(),
            pr_url: "https://github.com/example/pull/1".into(),
            pr_title: None,
            duration: "5m".into(),
        };
        let msg = pr.format_telegram_with_ci(None);
        // Should be identical to format_telegram when CI is None.
        assert_eq!(msg, pr.format_telegram());
        assert!(!msg.contains("CI:"));
    }

    #[test]
    fn test_format_telegram_with_ci_non_pr_falls_back() {
        let spawned = SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/task".into(),
            summary: Some("Do stuff".into()),
        };
        assert_eq!(
            spawned.format_telegram_with_ci(Some(&CiStatus::Passing)),
            spawned.format_telegram()
        );
    }

    #[test]
    fn test_inline_buttons_with_ci_passing() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://example.com/pull/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        let buttons = pr.inline_buttons_with_ci(Some(&CiStatus::Passing));
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].len(), 2);
        assert_eq!(buttons[0][0].callback_data, "merge_pr:test-1");
        assert!(buttons[0][0].text.contains("Merge PR"));
    }

    #[test]
    fn test_inline_buttons_with_ci_pending() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://example.com/pull/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        let buttons = pr.inline_buttons_with_ci(Some(&CiStatus::Pending));
        assert_eq!(buttons[0][0].callback_data, "ci_pending:test-1");
        assert!(buttons[0][0].text.contains("CI still running"));
    }

    #[test]
    fn test_inline_buttons_with_ci_failing() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://example.com/pull/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        let buttons = pr.inline_buttons_with_ci(Some(&CiStatus::Failing));
        assert_eq!(buttons[0][0].callback_data, "ci_failing_merge:test-1");
        assert!(buttons[0][0].text.contains("CI failing"));
    }

    #[test]
    fn test_inline_buttons_with_ci_none_shows_merge() {
        let pr = SwarmNotification::PrOpened {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            pr_url: "https://example.com/pull/1".into(),
            pr_title: None,
            duration: "1m".into(),
        };
        let buttons = pr.inline_buttons_with_ci(None);
        // None CI → show normal merge button (same as no CI info).
        assert_eq!(buttons[0][0].callback_data, "merge_pr:test-1");
    }

    #[test]
    fn test_inline_buttons_with_ci_non_pr_falls_back() {
        let waiting = SwarmNotification::AgentWaiting {
            worktree_id: "test-1".into(),
            branch: "swarm/feat".into(),
            summary: None,
            pr_url: Some("https://example.com/pull/1".into()),
        };
        let buttons_ci = waiting.inline_buttons_with_ci(Some(&CiStatus::Passing));
        let buttons_plain = waiting.inline_buttons();
        assert_eq!(buttons_ci.len(), buttons_plain.len());
    }
}
