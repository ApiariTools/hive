//! Unified hive TUI — workers panel + coordinator chat + workspace tabs.

pub mod app;
pub mod history;
pub mod inbox;
pub mod render;

use app::{App, ChatLine, Panel, PendingAction};
use color_eyre::Result;
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::presence;
use crate::workspace::{self, load_workspace};

/// Messages sent from the coordinator background task back to the TUI.
enum CoordResponse {
    /// Streaming text chunk.
    Token(String),
    /// Session finished.
    Done,
    /// Error from coordinator.
    Error(String),
}

/// Async action results sent back to the event loop.
enum ActionResponse {
    Success(String),
    Error(String),
}

/// Launch the unified TUI.
pub async fn run(cwd: &Path) -> Result<()> {
    // Load workspace registry for tabs.
    let mut workspaces = workspace::load_registry();
    if workspaces.is_empty() {
        // Fallback: use current workspace.
        if let Ok(ws) = load_workspace(cwd) {
            workspaces.push(workspace::RegistryEntry {
                name: ws.name.clone(),
                path: ws.root.clone(),
            });
        }
    }

    // Find which tab matches the current directory.
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let active_tab = workspaces
        .iter()
        .position(|e| {
            std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone()) == canonical_cwd
        })
        .unwrap_or(0);

    let workspace_root = canonical_cwd.clone();
    let mut app = App::new(workspaces, active_tab, workspace_root.clone());
    app.refresh_workers();

    // Channels for coordinator communication.
    let (user_tx, user_rx) = mpsc::channel::<String>(32);
    let (coord_tx, coord_rx) = mpsc::channel::<CoordResponse>(64);

    // Spawn coordinator background task.
    let coord_cwd = cwd.to_path_buf();
    tokio::spawn(coordinator_task(coord_cwd, user_rx, coord_tx));

    // Spawn presence heartbeat — touch "ui" channel every 5 seconds.
    let heartbeat_root = workspace_root.clone();
    let heartbeat_cancel = tokio_util::sync::CancellationToken::new();
    let heartbeat_token = heartbeat_cancel.clone();
    tokio::spawn(async move {
        loop {
            let _ = presence::touch_channel(&heartbeat_root, "ui");
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = heartbeat_token.cancelled() => break,
            }
        }
    });

    // Terminal setup.
    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let result = event_loop(&mut terminal, &mut app, &user_tx, coord_rx).await;

    // Stop heartbeat.
    heartbeat_cancel.cancel();

    // Terminal teardown.
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    user_tx: &mpsc::Sender<String>,
    mut coord_rx: mpsc::Receiver<CoordResponse>,
) -> Result<()> {
    // Channel for async action results (swarm send, close, etc.).
    let (action_tx, mut action_rx) = mpsc::channel::<ActionResponse>(16);

    loop {
        // Capture terminal height for page scroll calculations.
        let viewport_height = terminal.size()?.height.saturating_sub(6); // approx content area

        terminal.draw(|frame| render::draw(frame, app))?;

        // Check for coordinator responses (non-blocking).
        while let Ok(msg) = coord_rx.try_recv() {
            match msg {
                CoordResponse::Token(text) => {
                    if let Some(ChatLine::Assistant(s)) = app.chat_history.last_mut() {
                        s.push_str(&text);
                    } else {
                        app.chat_history.push(ChatLine::Assistant(text));
                    }
                    app.chat_scroll = 0;
                }
                CoordResponse::Done => {
                    if let Some(ChatLine::Assistant(s)) = app.chat_history.last() {
                        let _ = history::save_message(
                            &app.workspace_root,
                            &history::ChatMessage {
                                role: "assistant".into(),
                                content: s.clone(),
                                ts: chrono::Utc::now(),
                            },
                        );
                    }
                    app.streaming = false;
                }
                CoordResponse::Error(e) => {
                    app.chat_history
                        .push(ChatLine::System(format!("Error: {e}")));
                    app.streaming = false;
                }
            }
        }

        // Check for action responses (non-blocking).
        while let Ok(resp) = action_rx.try_recv() {
            match resp {
                ActionResponse::Success(msg) => app.flash(msg),
                ActionResponse::Error(msg) => app.flash(format!("Error: {msg}")),
            }
        }

        // Poll keyboard events with 100ms timeout.
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            // Ctrl+C always quits.
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                break;
            }

            // ── Prefix mode (Ctrl+B, then command) ──
            if app.prefix_active {
                app.prefix_active = false;
                match key.code {
                    KeyCode::Char('n') => {
                        let next = (app.active_tab + 1) % app.workspaces.len().max(1);
                        app.switch_tab(next);
                    }
                    KeyCode::Char('p') => {
                        let prev = if app.active_tab == 0 {
                            app.workspaces.len().saturating_sub(1)
                        } else {
                            app.active_tab - 1
                        };
                        app.switch_tab(prev);
                    }
                    KeyCode::Char(c @ '1'..='9') => {
                        let idx = (c as usize) - ('1' as usize);
                        app.switch_tab(idx);
                    }
                    _ => {} // unknown prefix command — ignore
                }
                continue;
            }

            // Ctrl+B activates prefix mode.
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('b') {
                app.prefix_active = true;
                continue;
            }

            // ── Confirmation mode ──
            if app.pending_action.is_some() {
                match key.code {
                    KeyCode::Char('y') => {
                        let action = app.pending_action.take().unwrap();
                        match action {
                            PendingAction::CloseWorker(id) => {
                                let root = app.workspace_root.clone();
                                let tx = action_tx.clone();
                                spawn_close_worker(root, id, tx);
                            }
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        app.pending_action = None;
                        app.flash("Cancelled");
                    }
                    _ => {} // ignore other keys during confirm
                }
                continue; // skip normal key handling
            }

            // ── Global keybindings ──
            let handled = match key.code {
                KeyCode::Char(c @ '1'..='9')
                    if key.modifiers.contains(KeyModifiers::ALT) || app.focus == Panel::Workers =>
                {
                    let idx = (c as usize) - ('1' as usize);
                    app.switch_tab(idx);
                    true
                }
                _ => false,
            };

            if !handled {
                match app.focus {
                    Panel::Chat => {
                        if app.is_chat_selected() {
                            // ── Content: Chat mode ──
                            // Ctrl modifiers first (before generic Char match).
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                match key.code {
                                    KeyCode::Char('u') => app.scroll_chat_half_up(viewport_height),
                                    KeyCode::Char('d') => {
                                        app.scroll_chat_half_down(viewport_height)
                                    }
                                    KeyCode::Char('b') => app.scroll_chat_page_up(viewport_height),
                                    KeyCode::Char('f') => {
                                        app.scroll_chat_page_down(viewport_height)
                                    }
                                    _ => {}
                                }
                            } else {
                                match key.code {
                                    KeyCode::BackTab | KeyCode::Left => app.focus = Panel::Workers,
                                    KeyCode::Tab => app.focus = Panel::Workers,
                                    KeyCode::Esc => app.focus = Panel::Workers,
                                    KeyCode::Enter => {
                                        if !app.input.is_empty() && !app.streaming {
                                            let text = app.take_input();
                                            let _ = history::save_message(
                                                &app.workspace_root,
                                                &history::ChatMessage {
                                                    role: "user".into(),
                                                    content: text.clone(),
                                                    ts: chrono::Utc::now(),
                                                },
                                            );
                                            let recent =
                                                history::load_history(&app.workspace_root, 20);
                                            let prompt = build_prompt_with_history(&recent, &text);
                                            app.chat_history.push(ChatLine::User(text));
                                            app.streaming = true;
                                            let _ = user_tx.send(prompt).await;
                                        }
                                    }
                                    KeyCode::Backspace => app.backspace(),
                                    KeyCode::Up => app.scroll_chat_up(),
                                    KeyCode::Down => app.scroll_chat_down(),
                                    KeyCode::Char(c) => app.insert_char(c),
                                    _ => {}
                                }
                            }
                        } else {
                            // ── Content: Worker pane (forward keys to tmux) ──
                            // Esc / Tab / BackTab return focus to sidebar.
                            // Everything else is forwarded to the tmux pane.
                            match key.code {
                                KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => {
                                    app.focus = Panel::Workers;
                                }
                                _ => {
                                    if let Some((_, wt)) = app.selected_worker()
                                        && let Some(ref pane_id) = wt.agent_pane_id
                                    {
                                        send_key_to_pane(pane_id, key.code, key.modifiers);
                                    }
                                }
                            }
                        }
                    }
                    Panel::Workers => {
                        // ── Sidebar mode ──
                        match key.code {
                            KeyCode::Char('l') | KeyCode::Tab | KeyCode::Right => {
                                app.focus = Panel::Chat
                            }
                            KeyCode::BackTab => app.focus = Panel::Chat,
                            KeyCode::Char('q') => break,
                            KeyCode::Char('j') | KeyCode::Down => app.select_next(),
                            KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
                            KeyCode::Char('g') => app.select_first(),
                            KeyCode::Char('G') => app.select_last(),
                            KeyCode::Esc => app.focus = Panel::Chat,
                            KeyCode::Enter => {
                                // Focus the content panel (chat or worker input).
                                app.focus = Panel::Chat;
                            }
                            KeyCode::Char('o') => {
                                // Jump directly to tmux pane for selected worker.
                                if let Some((_, wt)) = app.selected_worker()
                                    && let Some(ref pane_id) = wt.agent_pane_id
                                {
                                    let _ = jump_to_worker_pane(pane_id);
                                }
                            }
                            KeyCode::Char('x') => {
                                // Close worker (with confirmation).
                                if let Some((id, _)) = app.selected_worker() {
                                    app.pending_action =
                                        Some(PendingAction::CloseWorker(id.to_string()));
                                }
                            }
                            KeyCode::Char('p') => {
                                // Open PR in browser.
                                if let Some((_, wt)) = app.selected_worker() {
                                    if let Some(ref pr) = wt.pr {
                                        let _ =
                                            std::process::Command::new("open").arg(&pr.url).spawn();
                                        app.flash("Opened PR in browser");
                                    } else {
                                        app.flash("No PR for this worker");
                                    }
                                }
                            }
                            KeyCode::Char('c') => {
                                // Copy PR URL to clipboard.
                                if let Some((_, wt)) = app.selected_worker() {
                                    if let Some(ref pr) = wt.pr {
                                        copy_to_clipboard(&pr.url);
                                        app.flash("PR URL copied");
                                    } else {
                                        app.flash("No PR to copy");
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Periodic refresh (workers).
        app.tick();

        // Poll UI inbox for daemon-routed notifications.
        let events = inbox::poll_events(&app.workspace_root, &mut app.inbox_pos);
        for event in events {
            app.chat_history.push(ChatLine::System(event.display()));
            app.chat_scroll = 0;
        }
    }

    Ok(())
}

// ── Async action helpers ─────────────────────────────────

/// Spawn a task to close a worker via `swarm close`.
fn spawn_close_worker(workspace_root: PathBuf, id: String, tx: mpsc::Sender<ActionResponse>) {
    tokio::spawn(async move {
        let result = tokio::process::Command::new("swarm")
            .args(["--dir", &workspace_root.to_string_lossy(), "close", &id])
            .output()
            .await;

        match result {
            Ok(output) if output.status.success() => {
                let _ = tx
                    .send(ActionResponse::Success(format!("Closed {id}")))
                    .await;
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let _ = tx
                    .send(ActionResponse::Error(format!("swarm close: {stderr}")))
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(ActionResponse::Error(format!("swarm close: {e}")))
                    .await;
            }
        }
    });
}

/// Forward a key event to a tmux pane via `tmux send-keys`.
fn send_key_to_pane(pane_id: &str, code: KeyCode, modifiers: KeyModifiers) {
    let key_str = if modifiers.contains(KeyModifiers::CONTROL) {
        // Ctrl+<key> → "C-<key>"
        match code {
            KeyCode::Char(c) => format!("C-{c}"),
            _ => return,
        }
    } else if modifiers.contains(KeyModifiers::ALT) {
        match code {
            KeyCode::Char(c) => format!("M-{c}"),
            KeyCode::Enter => "M-Enter".to_string(),
            _ => return,
        }
    } else {
        match code {
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Backspace => "BSpace".to_string(),
            KeyCode::Up => "Up".to_string(),
            KeyCode::Down => "Down".to_string(),
            KeyCode::Left => "Left".to_string(),
            KeyCode::Right => "Right".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::PageUp => "PageUp".to_string(),
            KeyCode::PageDown => "PageDown".to_string(),
            KeyCode::Delete => "DC".to_string(),
            KeyCode::Char(c) => {
                // Use -l (literal) for printable characters.
                let _ = std::process::Command::new("tmux")
                    .args(["send-keys", "-t", pane_id, "-l", &c.to_string()])
                    .output();
                return;
            }
            _ => return,
        }
    };

    let _ = std::process::Command::new("tmux")
        .args(["send-keys", "-t", pane_id, &key_str])
        .output();
}

/// Jump to a worker's tmux agent pane using `tmux select-pane`.
fn jump_to_worker_pane(pane_id: &str) -> std::io::Result<()> {
    std::process::Command::new("tmux")
        .args(["select-pane", "-t", pane_id])
        .status()?;
    Ok(())
}

/// Copy text to the system clipboard (macOS).
fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    if let Ok(mut child) = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(ref mut stdin) = child.stdin {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// Background task that receives user messages and calls the coordinator.
async fn coordinator_task(
    cwd: std::path::PathBuf,
    mut user_rx: mpsc::Receiver<String>,
    coord_tx: mpsc::Sender<CoordResponse>,
) {
    // Build the coordinator system prompt.
    let workspace = match load_workspace(&cwd) {
        Ok(ws) => ws,
        Err(e) => {
            let _ = coord_tx
                .send(CoordResponse::Error(format!(
                    "Failed to load workspace: {e}"
                )))
                .await;
            return;
        }
    };

    let system_prompt = build_system_prompt(&workspace);

    while let Some(user_message) = user_rx.recv().await {
        // Spawn an ephemeral session for each message.
        let response = run_coordinator_message(&cwd, &system_prompt, &user_message).await;

        match response {
            Ok(text) => {
                let _ = coord_tx.send(CoordResponse::Token(text)).await;
                let _ = coord_tx.send(CoordResponse::Done).await;
            }
            Err(e) => {
                let _ = coord_tx.send(CoordResponse::Error(format!("{e}"))).await;
            }
        }
    }
}

/// Run a single coordinator message and return the response.
async fn run_coordinator_message(
    cwd: &Path,
    system_prompt: &str,
    user_message: &str,
) -> Result<String> {
    use apiari_claude_sdk::types::ContentBlock;
    use apiari_claude_sdk::{ClaudeClient, Event as SdkEvent, SessionOptions};

    let opts = SessionOptions {
        system_prompt: Some(system_prompt.to_owned()),
        model: Some("sonnet".into()),
        max_turns: Some(20),
        no_session_persistence: true,
        working_dir: Some(cwd.to_path_buf()),
        allowed_tools: vec!["Bash".into(), "Read".into(), "Glob".into(), "Grep".into()],
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client
        .spawn(opts)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;

    session
        .send_message(user_message)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
    session.close_stdin();

    let mut text = String::new();
    loop {
        let event = session
            .next_event()
            .await
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        match event {
            Some(SdkEvent::Assistant { message, .. }) => {
                for block in &message.message.content {
                    if let ContentBlock::Text { text: t } = block {
                        text.push_str(t);
                    }
                }
                if message.message.stop_reason.as_deref() == Some("end_turn") {
                    break;
                }
            }
            Some(SdkEvent::Result(r)) => {
                if let Some(result_text) = &r.result
                    && text.is_empty()
                {
                    text = result_text.clone();
                }
                break;
            }
            Some(SdkEvent::RateLimit(_))
            | Some(SdkEvent::System(_))
            | Some(SdkEvent::User(_))
            | Some(SdkEvent::Stream { .. }) => {}
            None => break,
        }
    }

    Ok(text)
}

/// Build a user message with recent conversation history prepended as context.
fn build_prompt_with_history(history: &[history::ChatMessage], user_input: &str) -> String {
    if history.is_empty() {
        return user_input.to_string();
    }
    let mut parts = vec!["[Recent conversation]".to_string()];
    for msg in history.iter().rev().take(20).rev() {
        parts.push(format!("{}: {}", msg.role, msg.content));
    }
    parts.push(format!("\nCurrent message: {user_input}"));
    parts.join("\n")
}

/// Build the coordinator system prompt from workspace config.
fn build_system_prompt(workspace: &crate::workspace::Workspace) -> String {
    let mut prompt = format!(
        "You are a coordinator for the \"{}\" workspace. \
         Help the user understand project status, plan work, and manage agents.\n\n\
         The user is chatting with you from `hive ui` — the local terminal TUI (not Telegram, not Claude Code).\n",
        workspace.name
    );

    if let Some(ref soul) = workspace.soul {
        prompt.push_str("\n--- Soul ---\n");
        prompt.push_str(soul);
        prompt.push('\n');
    }

    if let Some(ref conventions) = workspace.conventions {
        prompt.push_str("\n--- Conventions ---\n");
        prompt.push_str(conventions);
        prompt.push('\n');
    }

    if !workspace.repos.is_empty() {
        prompt.push_str("\nManaged repositories: ");
        prompt.push_str(&workspace.repos.join(", "));
        prompt.push('\n');
    }

    prompt
}
