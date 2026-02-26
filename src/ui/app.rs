//! Application state for the unified hive TUI.

use crate::keeper::discovery::{self, SwarmSession, WorktreeInfo};
use crate::workspace::RegistryEntry;
use std::cell::Cell;
use std::path::PathBuf;
use std::time::Instant;

use super::history;

/// Pending confirmation action.
#[derive(Debug, Clone)]
pub enum PendingAction {
    CloseWorker(String), // worktree_id
}

/// A transient flash message shown in the status bar.
#[derive(Debug, Clone)]
pub struct FlashMessage {
    pub text: String,
    pub expires: Instant,
}

/// Which panel has focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel {
    Workers,
    Chat,
}

/// What is selected in the left sidebar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SidebarItem {
    Chat,
    Worker(usize), // index into flat_workers()
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
    pub prefix_active: bool,

    // Left panel — sidebar (Chat + workers)
    pub sessions: Vec<SwarmSession>,
    pub sidebar_selection: SidebarItem,

    // Right panel — chat
    pub chat_history: Vec<ChatLine>,
    pub input: String,
    pub input_cursor: usize,
    pub streaming: bool,

    // Panel focus
    pub focus: Panel,

    // Scroll offset for chat (lines from bottom)
    pub chat_scroll: u16,

    // Worker pane capture (for selected worker)
    pub pane_content: String,

    // Workspace root for history/inbox persistence
    pub workspace_root: PathBuf,

    // UI inbox file position for incremental polling
    pub inbox_pos: u64,

    // Sidebar scroll offset (in logical pixels/rows from top).
    // Uses Cell for interior mutability during rendering.
    pub sidebar_scroll: Cell<usize>,

    // Confirmation / flash
    pub pending_action: Option<PendingAction>,
    pub flash_message: Option<FlashMessage>,

    // Periodic refresh
    last_worker_refresh: Instant,
}

impl App {
    pub fn new(workspaces: Vec<RegistryEntry>, active_tab: usize, workspace_root: PathBuf) -> Self {
        // Load persisted chat history.
        let saved = history::load_history(&workspace_root, 50);
        let mut chat_history: Vec<ChatLine> = vec![ChatLine::System(
            "Welcome to hive ui. Type a message to chat with the coordinator.".into(),
        )];
        for msg in &saved {
            match msg.role.as_str() {
                "user" => chat_history.push(ChatLine::User(msg.content.clone())),
                "assistant" => chat_history.push(ChatLine::Assistant(msg.content.clone())),
                _ => {}
            }
        }

        // Skip to end of inbox file so we only see new events.
        let inbox_pos = std::fs::metadata(workspace_root.join(".hive/ui_inbox.jsonl"))
            .map(|m| m.len())
            .unwrap_or(0);

        Self {
            workspaces,
            active_tab,
            prefix_active: false,
            sessions: Vec::new(),
            sidebar_selection: SidebarItem::Chat,
            chat_history,
            input: String::new(),
            input_cursor: 0,
            streaming: false,
            focus: Panel::Workers,
            chat_scroll: 0,
            pane_content: String::new(),
            workspace_root,
            inbox_pos,
            sidebar_scroll: Cell::new(0),
            pending_action: None,
            flash_message: None,
            last_worker_refresh: Instant::now(),
        }
    }

    /// The root path of the currently active workspace.
    pub fn active_workspace_root(&self) -> Option<&std::path::Path> {
        self.workspaces
            .get(self.active_tab)
            .map(|e| e.path.as_path())
    }

    /// Periodic tick — refresh workers every 2 seconds + expire flashes.
    pub fn tick(&mut self) {
        if self.last_worker_refresh.elapsed().as_secs() >= 2 {
            self.refresh_workers();
            self.refresh_pane_content();
        }
        // Expire flash messages.
        if let Some(ref flash) = self.flash_message
            && Instant::now() >= flash.expires
        {
            self.flash_message = None;
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
            self.clamp_sidebar_selection();
        }
        self.last_worker_refresh = Instant::now();
    }

    /// Refresh pane capture for the currently selected worker.
    pub fn refresh_pane_content(&mut self) {
        let SidebarItem::Worker(idx) = self.sidebar_selection else {
            self.pane_content.clear();
            return;
        };

        let workers = self.flat_workers();
        let Some((_, wt)) = workers.get(idx) else {
            self.pane_content.clear();
            return;
        };

        let Some(ref pane_id) = wt.agent_pane_id else {
            self.pane_content = "No agent pane".to_string();
            return;
        };

        // Capture last 500 lines from the tmux pane.
        // -e preserves ANSI escape sequences (colors).
        match std::process::Command::new("tmux")
            .args(["capture-pane", "-t", pane_id, "-p", "-S", "-500", "-e"])
            .output()
        {
            Ok(output) if output.status.success() => {
                self.pane_content = String::from_utf8_lossy(&output.stdout).to_string();
            }
            _ => {
                self.pane_content = "Pane not available".to_string();
            }
        }
    }

    /// Total sidebar item count (Chat + workers).
    fn sidebar_count(&self) -> usize {
        1 + self.total_workers() // "Chat" + workers
    }

    /// Total flat worker count across all sessions.
    fn total_workers(&self) -> usize {
        self.sessions.iter().map(|s| s.worktrees.len()).sum()
    }

    /// Current sidebar selection as a numeric index (0 = Chat, 1+ = workers).
    fn sidebar_index(&self) -> usize {
        match self.sidebar_selection {
            SidebarItem::Chat => 0,
            SidebarItem::Worker(i) => i + 1,
        }
    }

    /// Convert a numeric index back to a SidebarItem.
    fn sidebar_item_at(&self, index: usize) -> SidebarItem {
        if index == 0 {
            SidebarItem::Chat
        } else {
            SidebarItem::Worker(index - 1)
        }
    }

    /// Select next sidebar item.
    pub fn select_next(&mut self) {
        let count = self.sidebar_count();
        if count > 0 {
            let next = (self.sidebar_index() + 1) % count;
            self.set_sidebar_selection(self.sidebar_item_at(next));
        }
    }

    /// Select previous sidebar item.
    pub fn select_prev(&mut self) {
        let count = self.sidebar_count();
        if count > 0 {
            let cur = self.sidebar_index();
            let prev = if cur == 0 { count - 1 } else { cur - 1 };
            self.set_sidebar_selection(self.sidebar_item_at(prev));
        }
    }

    /// Set the sidebar selection and refresh pane if needed.
    fn set_sidebar_selection(&mut self, item: SidebarItem) {
        if self.sidebar_selection != item {
            self.sidebar_selection = item;
            self.input.clear();
            self.input_cursor = 0;
            self.refresh_pane_content();
        }
    }

    /// Whether chat is the active sidebar item.
    pub fn is_chat_selected(&self) -> bool {
        self.sidebar_selection == SidebarItem::Chat
    }

    /// Switch to a workspace tab by index (0-based).
    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.workspaces.len() && idx != self.active_tab {
            self.active_tab = idx;
            self.sidebar_selection = SidebarItem::Chat;
            self.focus = Panel::Workers;
            self.chat_scroll = 0;

            // Update workspace root and reload chat history for the new tab.
            if let Some(entry) = self.workspaces.get(idx) {
                self.workspace_root = entry.path.clone();

                // Reload chat history from the new workspace.
                let saved = history::load_history(&self.workspace_root, 50);
                self.chat_history = vec![ChatLine::System(format!(
                    "Switched to {} workspace.",
                    entry.name
                ))];
                for msg in &saved {
                    match msg.role.as_str() {
                        "user" => self.chat_history.push(ChatLine::User(msg.content.clone())),
                        "assistant" => self
                            .chat_history
                            .push(ChatLine::Assistant(msg.content.clone())),
                        _ => {}
                    }
                }

                // Reset inbox position for the new workspace.
                self.inbox_pos =
                    std::fs::metadata(self.workspace_root.join(".hive/ui_inbox.jsonl"))
                        .map(|m| m.len())
                        .unwrap_or(0);
            }

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

    fn clamp_sidebar_selection(&mut self) {
        let total = self.total_workers();
        match self.sidebar_selection {
            SidebarItem::Chat => {} // always valid
            SidebarItem::Worker(i) => {
                if total == 0 {
                    self.sidebar_selection = SidebarItem::Chat;
                } else if i >= total {
                    self.sidebar_selection = SidebarItem::Worker(total - 1);
                }
            }
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

    /// Select first sidebar item.
    pub fn select_first(&mut self) {
        self.set_sidebar_selection(SidebarItem::Chat);
    }

    /// Select last sidebar item.
    pub fn select_last(&mut self) {
        let count = self.sidebar_count();
        if count > 0 {
            self.set_sidebar_selection(self.sidebar_item_at(count - 1));
        }
    }

    /// Half-page scroll up for chat.
    pub fn scroll_chat_half_up(&mut self, viewport: u16) {
        let half = (viewport / 2).max(1);
        self.chat_scroll = self.chat_scroll.saturating_add(half);
    }

    /// Half-page scroll down for chat.
    pub fn scroll_chat_half_down(&mut self, viewport: u16) {
        let half = (viewport / 2).max(1);
        self.chat_scroll = self.chat_scroll.saturating_sub(half);
    }

    /// Full-page scroll up for chat.
    pub fn scroll_chat_page_up(&mut self, viewport: u16) {
        self.chat_scroll = self.chat_scroll.saturating_add(viewport);
    }

    /// Full-page scroll down for chat.
    pub fn scroll_chat_page_down(&mut self, viewport: u16) {
        self.chat_scroll = self.chat_scroll.saturating_sub(viewport);
    }

    /// Set a flash message (3 seconds).
    pub fn flash(&mut self, text: impl Into<String>) {
        self.flash_message = Some(FlashMessage {
            text: text.into(),
            expires: Instant::now() + std::time::Duration::from_secs(3),
        });
    }

    /// Get the currently selected worker (id, info), if any.
    pub fn selected_worker(&self) -> Option<(&str, &WorktreeInfo)> {
        let SidebarItem::Worker(idx) = self.sidebar_selection else {
            return None;
        };
        let workers = self.flat_workers();
        workers.get(idx).map(|(_, wt)| (wt.id.as_str(), *wt))
    }
}
