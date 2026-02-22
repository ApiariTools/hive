//! Application state for the keeper dashboard.

use crate::keeper::discovery::{self, BuzzSummary, DiscoveryResult, SwarmSession};
use std::time::Instant;

/// Which panel has focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel {
    Sessions,
    Worktrees,
}

/// Modal overlay state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Overlay {
    None,
    Help,
}

/// Top-level application state.
pub struct App {
    /// Discovered swarm sessions.
    pub sessions: Vec<SwarmSession>,

    /// Buzz signal summary (if available).
    pub buzz: Option<BuzzSummary>,

    /// Currently selected session index.
    pub selected_session: usize,

    /// Currently selected worktree index within the selected session.
    pub selected_worktree: usize,

    /// Which panel is focused.
    pub panel: Panel,

    /// Current overlay.
    pub overlay: Overlay,

    /// When we last refreshed discovery.
    last_refresh: Instant,

    /// When we last refreshed PR info.
    last_pr_refresh: Instant,
}

impl App {
    pub fn new(result: DiscoveryResult) -> Self {
        Self {
            sessions: result.sessions,
            buzz: result.buzz,
            selected_session: 0,
            selected_worktree: 0,
            panel: Panel::Sessions,
            overlay: Overlay::None,
            last_refresh: Instant::now(),
            last_pr_refresh: Instant::now(),
        }
    }

    /// Periodic tick -- re-discover sessions every 5 seconds, PR info every 30s.
    pub fn tick(&mut self) {
        if self.last_refresh.elapsed().as_secs() >= 5 {
            self.refresh();
        }

        // PR refresh every 30 seconds (it shells out to `gh` per worktree).
        if self.last_pr_refresh.elapsed().as_secs() >= 30 {
            self.refresh_prs();
        }
    }

    /// Force a refresh of discovered sessions.
    pub fn refresh(&mut self) {
        if let Ok(result) = discovery::discover_all() {
            // Preserve selection if possible.
            let prev_name = self
                .sessions
                .get(self.selected_session)
                .map(|s| s.session_name.clone());

            self.sessions = result.sessions;
            self.buzz = result.buzz;

            // Try to re-select the same session.
            if let Some(name) = prev_name
                && let Some(idx) = self.sessions.iter().position(|s| s.session_name == name)
            {
                self.selected_session = idx;
            }

            self.clamp_selection();
        }
        self.last_refresh = Instant::now();
    }

    /// Refresh PR info for all sessions (shells out to `gh`).
    fn refresh_prs(&mut self) {
        for session in &mut self.sessions {
            discovery::refresh_pr_info(session);
        }
        self.last_pr_refresh = Instant::now();
    }

    /// Move selection down.
    pub fn select_next(&mut self) {
        match self.panel {
            Panel::Sessions => {
                if !self.sessions.is_empty() {
                    self.selected_session = (self.selected_session + 1) % self.sessions.len();
                    self.selected_worktree = 0;
                }
            }
            Panel::Worktrees => {
                if let Some(session) = self.sessions.get(self.selected_session)
                    && !session.worktrees.is_empty()
                {
                    self.selected_worktree = (self.selected_worktree + 1) % session.worktrees.len();
                }
            }
        }
    }

    /// Move selection up.
    pub fn select_prev(&mut self) {
        match self.panel {
            Panel::Sessions => {
                if !self.sessions.is_empty() {
                    self.selected_session = if self.selected_session == 0 {
                        self.sessions.len() - 1
                    } else {
                        self.selected_session - 1
                    };
                    self.selected_worktree = 0;
                }
            }
            Panel::Worktrees => {
                if let Some(session) = self.sessions.get(self.selected_session)
                    && !session.worktrees.is_empty()
                {
                    self.selected_worktree = if self.selected_worktree == 0 {
                        session.worktrees.len() - 1
                    } else {
                        self.selected_worktree - 1
                    };
                }
            }
        }
    }

    /// Toggle between session list and worktree detail panel.
    pub fn toggle_panel(&mut self) {
        self.panel = match self.panel {
            Panel::Sessions => Panel::Worktrees,
            Panel::Worktrees => Panel::Sessions,
        };
    }

    /// Toggle help overlay.
    pub fn toggle_help(&mut self) {
        self.overlay = match self.overlay {
            Overlay::Help => Overlay::None,
            Overlay::None => Overlay::Help,
        };
    }

    /// Dismiss any overlay.
    pub fn dismiss_overlay(&mut self) {
        self.overlay = Overlay::None;
    }

    /// Whether an overlay is currently shown.
    pub fn has_overlay(&self) -> bool {
        self.overlay != Overlay::None
    }

    /// Jump to the selected session/worktree in tmux.
    pub fn jump_to_selected(&mut self) {
        let Some(session) = self.sessions.get(self.selected_session) else {
            return;
        };

        // Try to jump to the agent pane of the selected worktree, or just
        // the session if no worktree is selected.
        let target = if let Some(wt) = session.worktrees.get(self.selected_worktree) {
            if let Some(ref pane_id) = wt.agent_pane_id {
                pane_id.clone()
            } else {
                session.session_name.clone()
            }
        } else {
            session.session_name.clone()
        };

        // Fire and forget -- if it fails we just stay in keeper.
        let _ = std::process::Command::new("tmux")
            .args(["switch-client", "-t", &target])
            .output();
    }

    /// The currently selected session, if any.
    pub fn current_session(&self) -> Option<&SwarmSession> {
        self.sessions.get(self.selected_session)
    }

    /// Total agent count across all sessions.
    pub fn total_agents(&self) -> (usize, usize) {
        let mut alive = 0;
        let mut total = 0;
        for session in &self.sessions {
            for wt in &session.worktrees {
                total += 1;
                if wt.agent_alive {
                    alive += 1;
                }
            }
        }
        (alive, total)
    }

    /// Clamp selection indices to valid ranges.
    fn clamp_selection(&mut self) {
        if self.sessions.is_empty() {
            self.selected_session = 0;
            self.selected_worktree = 0;
            return;
        }

        if self.selected_session >= self.sessions.len() {
            self.selected_session = self.sessions.len() - 1;
        }

        let wt_count = self.sessions[self.selected_session].worktrees.len();
        if wt_count == 0 {
            self.selected_worktree = 0;
        } else if self.selected_worktree >= wt_count {
            self.selected_worktree = wt_count - 1;
        }
    }
}
