//! Unified hive TUI — workers panel + coordinator chat + workspace tabs.

pub mod app;
pub mod history;
pub mod inbox;
pub mod render;

use app::{App, ChatLine, Panel};
use color_eyre::Result;
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;
use std::io::stdout;
use std::path::Path;
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
    loop {
        terminal.draw(|frame| render::draw(frame, app))?;

        // Check for coordinator responses (non-blocking).
        while let Ok(msg) = coord_rx.try_recv() {
            match msg {
                CoordResponse::Token(text) => {
                    // Append to the last assistant message, or create one.
                    if let Some(ChatLine::Assistant(s)) = app.chat_history.last_mut() {
                        s.push_str(&text);
                    } else {
                        app.chat_history.push(ChatLine::Assistant(text));
                    }
                    app.chat_scroll = 0;
                }
                CoordResponse::Done => {
                    // Persist the completed assistant response.
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

        // Poll keyboard events with 100ms timeout.
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            // Ctrl+C always quits.
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                break;
            }

            match app.focus {
                Panel::Chat => match key.code {
                    KeyCode::Tab | KeyCode::BackTab => app.toggle_focus(),
                    KeyCode::Enter => {
                        if !app.input.is_empty() && !app.streaming {
                            let text = app.take_input();
                            // Persist user message.
                            let _ = history::save_message(
                                &app.workspace_root,
                                &history::ChatMessage {
                                    role: "user".into(),
                                    content: text.clone(),
                                    ts: chrono::Utc::now(),
                                },
                            );
                            // Build prompt with recent history context.
                            let recent = history::load_history(&app.workspace_root, 20);
                            let prompt = build_prompt_with_history(&recent, &text);
                            app.chat_history.push(ChatLine::User(text));
                            app.streaming = true;
                            let _ = user_tx.send(prompt).await;
                        }
                    }
                    KeyCode::Backspace => app.backspace(),
                    KeyCode::Char(c) => app.insert_char(c),
                    KeyCode::Up => app.scroll_chat_up(),
                    KeyCode::Down => app.scroll_chat_down(),
                    KeyCode::Esc => app.toggle_focus(),
                    _ => {}
                },
                Panel::Workers => match key.code {
                    KeyCode::Tab | KeyCode::BackTab => app.toggle_focus(),
                    KeyCode::Char('q') => break,
                    KeyCode::Char('j') | KeyCode::Down => app.select_next_worker(),
                    KeyCode::Char('k') | KeyCode::Up => app.select_prev_worker(),
                    KeyCode::Char(c @ '1'..='9') => {
                        let idx = (c as usize) - ('1' as usize);
                        app.switch_tab(idx);
                    }
                    KeyCode::Esc => app.toggle_focus(),
                    _ => {}
                },
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
         Help the user understand project status, plan work, and manage agents.\n",
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
