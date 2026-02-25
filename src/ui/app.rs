//! Application state for the unified hive TUI.

use crate::keeper::discovery::{self, SwarmSession};
use crate::workspace::RegistryEntry;
use std::time::Instant;

/// Which panel has focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel {
    Workers,
    Chat,
}

/// A line in the chat history.
#[derive(Clone, Debug)]
pub enum ChatLine {
    User(String),
    Assistant(String),
    System(String),
}

/// Top-level application state.
pub struct App {
    // Tab bar
    pub workspaces: Vec<RegistryEntry>,
    pub active_tab: usize,

    // Left panel — workers
    pub sessions: Vec<SwarmSession>,
    pub selected_worker: usize,

    // Right panel — chat
    pub chat_history: Vec<ChatLine>,
    pub input: String,
    pub input_cursor: usize,
    pub streaming: bool,

    // Panel focus
    pub focus: Panel,

    // Scroll offset for chat (lines from bottom)
    pub chat_scroll: u16,

    // Periodic refresh
    last_worker_refresh: Instant,
}

impl App {
    pub fn new(workspaces: Vec<RegistryEntry>, active_tab: usize) -> Self {
        Self {
            workspaces,
            active_tab,
            sessions: Vec::new(),
            selected_worker: 0,
            chat_history: vec![ChatLine::System(
                "Welcome to hive ui. Type a message to chat with the coordinator.".into(),
            )],
            input: String::new(),
            input_cursor: 0,
            streaming: false,
            focus: Panel::Chat,
            chat_scroll: 0,
            last_worker_refresh: Instant::now(),
        }
    }

    /// The root path of the currently active workspace.
    pub fn active_workspace_root(&self) -> Option<&std::path::Path> {
        self.workspaces
            .get(self.active_tab)
            .map(|e| e.path.as_path())
    }

    /// Periodic tick — refresh workers every 2 seconds.
    pub fn tick(&mut self) {
        if self.last_worker_refresh.elapsed().as_secs() >= 2 {
            self.refresh_workers();
        }
    }

    /// Refresh the worker list from swarm state.
    pub fn refresh_workers(&mut self) {
        if let Ok(result) = discovery::discover_all() {
            // Filter to sessions matching the active workspace.
            if let Some(root) = self.active_workspace_root() {
                self.sessions = result
                    .sessions
                    .into_iter()
                    .filter(|s| s.project_dir == root)
                    .collect();
            } else {
                self.sessions = result.sessions;
            }
            self.clamp_worker_selection();
        }
        self.last_worker_refresh = Instant::now();
    }

    /// Total flat worker count across all sessions.
    fn total_workers(&self) -> usize {
        self.sessions.iter().map(|s| s.worktrees.len()).sum()
    }

    /// Select next worker.
    pub fn select_next_worker(&mut self) {
        let total = self.total_workers();
        if total > 0 {
            self.selected_worker = (self.selected_worker + 1) % total;
        }
    }

    /// Select previous worker.
    pub fn select_prev_worker(&mut self) {
        let total = self.total_workers();
        if total > 0 {
            self.selected_worker = if self.selected_worker == 0 {
                total - 1
            } else {
                self.selected_worker - 1
            };
        }
    }

    /// Toggle focus between panels.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Panel::Workers => Panel::Chat,
            Panel::Chat => Panel::Workers,
        };
    }

    /// Switch to a workspace tab by index (0-based).
    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.workspaces.len() && idx != self.active_tab {
            self.active_tab = idx;
            self.selected_worker = 0;
            self.chat_scroll = 0;
            self.refresh_workers();
        }
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        self.input
            .insert(self.input.len().min(self.input_cursor), c);
        self.input_cursor += c.len_utf8();
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.input_cursor > 0 {
            // Find the previous char boundary.
            let prev = self.input[..self.input_cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.drain(prev..self.input_cursor);
            self.input_cursor = prev;
        }
    }

    /// Take the current input, clearing the buffer.
    pub fn take_input(&mut self) -> String {
        let text = self.input.clone();
        self.input.clear();
        self.input_cursor = 0;
        self.chat_scroll = 0;
        text
    }

    /// Scroll chat up.
    pub fn scroll_chat_up(&mut self) {
        self.chat_scroll = self.chat_scroll.saturating_add(3);
    }

    /// Scroll chat down.
    pub fn scroll_chat_down(&mut self) {
        self.chat_scroll = self.chat_scroll.saturating_sub(3);
    }

    fn clamp_worker_selection(&mut self) {
        let total = self.total_workers();
        if total == 0 {
            self.selected_worker = 0;
        } else if self.selected_worker >= total {
            self.selected_worker = total - 1;
        }
    }

    /// Get flat list of (session_name, worktree_info) for rendering.
    pub fn flat_workers(&self) -> Vec<(&str, &discovery::WorktreeInfo)> {
        self.sessions
            .iter()
            .flat_map(|s| {
                s.worktrees
                    .iter()
                    .map(move |w| (s.session_name.as_str(), w))
            })
            .collect()
    }
}
