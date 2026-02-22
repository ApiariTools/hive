//! Daemon mode — persistent Telegram bot + buzz auto-triage.
//!
//! The daemon runs a `tokio::select!` loop over multiple sources:
//! 1. Telegram messages (via mpsc channel from background long-poll)
//! 2. Buzz signals (inline watchers when enabled, else JSONL file poll)
//! 3. Swarm agent completion notifications (when enabled)
//! 4. Shutdown signals (SIGTERM/SIGINT)

pub mod config;
pub mod origin_map;
pub mod session_store;
pub mod swarm_watcher;

use crate::buzz::config::BuzzConfig;
use crate::buzz::output::{self, OutputMode};
use crate::buzz::reminder::{self, Reminder};
use crate::buzz::signal::{deduplicate, prioritize};
use crate::buzz::watcher::{Watcher, create_watchers, save_cursors};
use crate::channel::telegram::{self, TelegramChannel};
use crate::channel::{Channel, ChannelEvent, OutboundMessage};
use crate::coordinator::Coordinator;
use crate::quest::{QuestStore, TaskOrigin, default_store_path};
use crate::signal::{Severity, Signal};
use origin_map::{OriginEntry, OriginMap};
use crate::workspace::load_workspace;
use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};
use apiari_common::ipc::JsonlReader;
use color_eyre::eyre::{Result, WrapErr};
use config::DaemonConfig;
use session_store::SessionStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// PID file helpers
// ---------------------------------------------------------------------------

fn pid_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".hive").join("daemon.pid")
}

fn write_pid(workspace_root: &Path) -> Result<()> {
    let path = pid_path(workspace_root);
    std::fs::write(&path, std::process::id().to_string())
        .wrap_err_with(|| format!("failed to write PID file {}", path.display()))
}

fn read_pid(workspace_root: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_path(workspace_root))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn remove_pid(workspace_root: &Path) {
    let _ = std::fs::remove_file(pid_path(workspace_root));
}

fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// ---------------------------------------------------------------------------
// Public API: start / stop
// ---------------------------------------------------------------------------

fn log_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".hive").join("daemon.log")
}

/// Start the daemon.
///
/// By default, spawns a background child process with output redirected to
/// `.hive/daemon.log` and returns immediately. With `foreground: true`, runs
/// the event loop inline (blocking).
pub async fn start(cwd: &Path, foreground: bool) -> Result<()> {
    let workspace = load_workspace(cwd)?;
    let root = &workspace.root;

    // Check for stale PID file.
    if let Some(pid) = read_pid(root) {
        if is_process_alive(pid) {
            color_eyre::eyre::bail!("daemon already running (PID {pid})");
        }
        eprintln!("[daemon] Removing stale PID file (PID {pid} is not running)");
        remove_pid(root);
    }

    if !foreground {
        return spawn_background(root, cwd);
    }

    // Foreground mode — write PID and run inline.
    write_pid(root)?;
    let pid = std::process::id();
    eprintln!("[daemon] Started (PID {pid})");

    let config = DaemonConfig::load(root)?;
    eprintln!("[daemon] Loaded config from .hive/daemon.toml");
    eprintln!("[daemon] Model: {}", config.model);
    eprintln!(
        "[daemon] Buzz signals: {}",
        config.buzz_signals_path.display()
    );
    eprintln!(
        "[daemon] Buzz poll interval: {}s",
        config.buzz_poll_interval_secs
    );
    if config
        .buzz
        .as_ref()
        .is_some_and(|b| b.enabled)
    {
        eprintln!("[daemon] Inline buzz watchers: enabled");
    }
    if config
        .swarm_watch
        .as_ref()
        .is_some_and(|sw| sw.enabled)
    {
        eprintln!("[daemon] Swarm watcher: enabled");
    }

    let mut runner = DaemonRunner::new(root.to_path_buf(), config)?;
    let result = runner.run().await;

    // Clean up PID file on exit.
    remove_pid(root);
    eprintln!("[daemon] PID file removed");

    result
}

/// Spawn `hive daemon start --foreground` as a detached background process,
/// with stdout/stderr redirected to `.hive/daemon.log`.
fn spawn_background(workspace_root: &Path, cwd: &Path) -> Result<()> {
    let exe = std::env::current_exe().wrap_err("failed to find hive executable")?;
    let log = log_path(workspace_root);

    let log_file = std::fs::File::create(&log)
        .wrap_err_with(|| format!("failed to create log file {}", log.display()))?;
    let stderr_file = log_file
        .try_clone()
        .wrap_err("failed to clone log file handle")?;

    let mut cmd = std::process::Command::new(exe);
    cmd.args(["daemon", "start", "--foreground"]);
    // Pass through -C if the user specified a different working dir.
    if cwd != workspace_root {
        cmd.args(["-C", &cwd.display().to_string()]);
    }
    cmd.stdout(log_file);
    cmd.stderr(stderr_file);
    cmd.stdin(std::process::Stdio::null());

    // Detach from parent process group so it survives our exit.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = cmd.spawn().wrap_err("failed to spawn daemon process")?;
    let pid = child.id();

    println!("daemon started (PID {pid})");
    println!("logs: {}", log.display());

    Ok(())
}

/// Stop the running daemon by reading its PID file and sending SIGTERM.
pub fn stop(cwd: &Path) -> Result<()> {
    let workspace = load_workspace(cwd)?;
    let root = &workspace.root;

    let pid = match read_pid(root) {
        Some(pid) => pid,
        None => {
            eprintln!("daemon is not running (no PID file)");
            return Ok(());
        }
    };

    if !is_process_alive(pid) {
        eprintln!("daemon is not running (PID {pid} is stale), removing PID file");
        remove_pid(root);
        return Ok(());
    }

    // Send SIGTERM.
    let _ = std::process::Command::new("kill")
        .args([&pid.to_string()])
        .status();

    // Wait up to 5 seconds for the process to exit.
    for _ in 0..50 {
        if !is_process_alive(pid) {
            remove_pid(root);
            eprintln!("daemon stopped (PID {pid})");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Still alive — force kill.
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    remove_pid(root);
    eprintln!("daemon killed (PID {pid})");

    Ok(())
}

/// A pending origin awaiting association with a spawned worktree.
struct PendingOrigin {
    origin: TaskOrigin,
    created_at: std::time::Instant,
}

/// Main daemon event loop.
struct DaemonRunner {
    workspace_root: std::path::PathBuf,
    config: DaemonConfig,
    channel: Arc<TelegramChannel>,
    sessions: SessionStore,
    buzz_reader: JsonlReader<Signal>,
    coordinator_prompt: String,
    /// Inline buzz watchers (when [buzz] enabled = true).
    inline_watchers: Option<Vec<Box<dyn Watcher>>>,
    /// Inline buzz reminders (when [buzz] enabled = true).
    inline_reminders: Option<Vec<Reminder>>,
    /// Buzz output mode for writing signals to JSONL (when inline watchers are active).
    buzz_output: Option<OutputMode>,
    /// Swarm agent completion watcher.
    swarm_watcher: Option<swarm_watcher::SwarmWatcher>,
    /// Maps worktree_id → who asked for it (for notification routing).
    origin_map: OriginMap,
    /// FIFO queue of origins waiting to be matched with newly spawned worktrees.
    pending_origins: Vec<PendingOrigin>,
}

impl DaemonRunner {
    fn new(workspace_root: std::path::PathBuf, config: DaemonConfig) -> Result<Self> {
        let channel = Arc::new(TelegramChannel::new(
            config.telegram.bot_token.clone(),
            config.allowed_user_ids.clone(),
        ));

        let mut sessions = SessionStore::load(&workspace_root);
        let buzz_path = config.resolved_buzz_path(&workspace_root);
        let mut buzz_reader = JsonlReader::<Signal>::new(&buzz_path);

        // Restore buzz reader offset from persisted state.
        let saved_offset = sessions.buzz_offset();
        if saved_offset > 0 {
            buzz_reader.set_offset(saved_offset);
            eprintln!("[daemon] Restored buzz reader offset: {saved_offset}");
        } else {
            // On first run, skip to end so we don't triage old signals.
            let offset = buzz_reader.skip_to_end().unwrap_or(0);
            sessions.set_buzz_offset(offset);
            eprintln!("[daemon] Skipped to end of buzz signals (offset: {offset})");
        }

        // Build the coordinator system prompt once, with a daemon-specific addendum.
        let workspace = load_workspace(&workspace_root)?;
        let store = QuestStore::new(default_store_path(&workspace.root));
        let coord = Coordinator::new(workspace, store);
        let mut coordinator_prompt = coord.build_system_prompt()?;
        coordinator_prompt.push_str("\n## Daemon Mode Constraints\n");
        coordinator_prompt
            .push_str("You are running as a bot in daemon mode (not an interactive terminal).\n");
        coordinator_prompt.push_str("Keep responses conversational and concise.\n");
        coordinator_prompt.push_str(
            "Do NOT modify files directly — no Write, Edit, or file-writing shell commands.\n",
        );
        coordinator_prompt.push_str("Dispatch coding work to swarm agents via `swarm create`.\n");
        coordinator_prompt.push_str("Only use Bash for: swarm commands, git status/log, cargo check, and other read-only operations.\n");

        // Conditionally init inline buzz watchers.
        let (inline_watchers, inline_reminders, buzz_output) =
            if let Some(buzz_config_path) = config.resolved_buzz_config_path(&workspace_root) {
                match BuzzConfig::load(&buzz_config_path) {
                    Ok(buzz_config) => {
                        let watchers = create_watchers(&buzz_config);
                        let reminders: Vec<Reminder> = buzz_config
                            .reminders
                            .iter()
                            .map(Reminder::from_config)
                            .collect();
                        // Always write to JSONL so the dashboard can still read signals.
                        let output = OutputMode::File(
                            buzz_config
                                .output
                                .path
                                .clone()
                                .unwrap_or_else(|| PathBuf::from(".buzz/signals.jsonl")),
                        );
                        eprintln!(
                            "[daemon] Inline buzz watchers enabled ({} watcher(s), {} reminder(s))",
                            watchers.len(),
                            reminders.len()
                        );
                        (Some(watchers), Some(reminders), Some(output))
                    }
                    Err(e) => {
                        eprintln!("[daemon] Failed to load buzz config: {e}");
                        eprintln!("[daemon] Falling back to JSONL file polling");
                        (None, None, None)
                    }
                }
            } else {
                (None, None, None)
            };

        // Conditionally init swarm watcher.
        let swarm_watcher_instance =
            if let Some(state_path) = config.resolved_swarm_state_path(&workspace_root) {
                let stall_timeout = config
                    .swarm_watch
                    .as_ref()
                    .map(|sw| sw.stall_timeout_secs)
                    .unwrap_or(300);
                eprintln!(
                    "[daemon] Swarm watcher enabled (state: {}, stall_timeout: {stall_timeout}s)",
                    state_path.display()
                );
                let mut watcher = swarm_watcher::SwarmWatcher::new(state_path);
                watcher.set_stall_timeout(stall_timeout);
                Some(watcher)
            } else {
                None
            };

        // Load origin map for notification routing.
        let origin_map = OriginMap::load(&workspace_root);
        if !origin_map.entries.is_empty() {
            eprintln!(
                "[daemon] Loaded {} origin mapping(s)",
                origin_map.entries.len()
            );
        }

        Ok(Self {
            workspace_root,
            config,
            channel,
            sessions,
            buzz_reader,
            coordinator_prompt,
            inline_watchers,
            inline_reminders,
            buzz_output,
            swarm_watcher: swarm_watcher_instance,
            origin_map,
            pending_origins: Vec::new(),
        })
    }

    async fn run(&mut self) -> Result<()> {
        let cancel = CancellationToken::new();

        // Set up SIGTERM/SIGINT handler.
        let shutdown_cancel = cancel.clone();
        tokio::spawn(async move {
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            {
                let mut sigterm =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("failed to install SIGTERM handler");
                tokio::select! {
                    _ = ctrl_c => {}
                    _ = sigterm.recv() => {}
                }
            }
            #[cfg(not(unix))]
            {
                let _ = ctrl_c.await;
            }
            eprintln!("\n[daemon] Shutdown signal received");
            shutdown_cancel.cancel();
        });

        // Start the Telegram polling loop in a background task.
        let (tx, mut rx) = mpsc::channel::<ChannelEvent>(64);
        let channel_clone = self.channel.clone();
        let poll_cancel = cancel.clone();
        tokio::spawn(async move {
            channel_clone.run(tx, poll_cancel).await;
        });

        let buzz_interval = std::time::Duration::from_secs(self.config.buzz_poll_interval_secs);
        let mut buzz_timer = tokio::time::interval(buzz_interval);
        buzz_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip the first immediate tick.
        buzz_timer.tick().await;

        // Swarm watcher timer (only if configured).
        let swarm_interval = self
            .config
            .swarm_watch
            .as_ref()
            .filter(|sw| sw.enabled)
            .map(|sw| std::time::Duration::from_secs(sw.poll_interval_secs));
        let mut swarm_timer = tokio::time::interval(
            swarm_interval.unwrap_or(std::time::Duration::from_secs(3600)),
        );
        swarm_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip the first immediate tick.
        swarm_timer.tick().await;

        eprintln!("[daemon] Ready. Listening for Telegram messages and buzz signals.");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    eprintln!("[daemon] Shutting down...");
                    break;
                }

                event = rx.recv() => {
                    match event {
                        Some(event) => {
                            if let Err(e) = self.handle_channel_event(event).await {
                                eprintln!("[daemon] Error handling event: {e}");
                            }
                        }
                        None => {
                            eprintln!("[daemon] Channel closed, shutting down.");
                            break;
                        }
                    }
                }

                _ = buzz_timer.tick() => {
                    if let Err(e) = self.poll_buzz().await {
                        eprintln!("[daemon] Buzz poll error: {e}");
                    }
                }

                _ = swarm_timer.tick(), if self.swarm_watcher.is_some() => {
                    if let Err(e) = self.poll_swarm().await {
                        eprintln!("[daemon] Swarm poll error: {e}");
                    }
                }
            }
        }

        // Persist state before exiting.
        self.sessions.save()?;
        eprintln!("[daemon] State saved. Goodbye.");
        Ok(())
    }

    /// Handle a message or command from a channel.
    async fn handle_channel_event(&mut self, event: ChannelEvent) -> Result<()> {
        let (chat_id, message_id) = match &event {
            ChannelEvent::Command {
                chat_id,
                message_id,
                ..
            }
            | ChannelEvent::Message {
                chat_id,
                message_id,
                ..
            } => (*chat_id, *message_id),
        };

        if !self.config.is_chat_allowed(chat_id) {
            eprintln!("[daemon] Ignoring message from disallowed chat {chat_id}");
            return Ok(());
        }

        // React immediately so the user knows we received their message.
        self.channel.send_reaction(chat_id, message_id, "👀").await;

        match event {
            ChannelEvent::Command {
                chat_id,
                user_name,
                command,
                args,
                ..
            } => {
                eprintln!("[daemon] Command from {user_name}: /{command} {args}");
                self.handle_command(chat_id, &command, &args).await
            }
            ChannelEvent::Message {
                chat_id,
                user_id,
                user_name,
                text,
                ..
            } => {
                eprintln!("[daemon] Message from {user_name}: {}", truncate(&text, 80));
                self.handle_message(chat_id, user_id, &user_name, &text)
                    .await
            }
        }
    }

    /// Handle a slash command.
    async fn handle_command(&mut self, chat_id: i64, command: &str, args: &str) -> Result<()> {
        match command {
            "start" => {
                self.send(
                    chat_id,
                    "Hive daemon is running. Send me a message to chat with the coordinator.",
                )
                .await
            }
            "reset" => {
                let had_session = self.sessions.reset_session(chat_id);
                self.sessions.save()?;
                if had_session {
                    self.send(
                        chat_id,
                        "Session reset. Next message starts a fresh conversation.",
                    )
                    .await
                } else {
                    self.send(chat_id, "No active session to reset.").await
                }
            }
            "history" => {
                let history = self.sessions.history(chat_id);
                if history.is_empty() {
                    return self.send(chat_id, "No previous sessions.").await;
                }
                let mut text = String::from("Previous sessions:\n");
                for (i, s) in history.iter().enumerate().rev().take(10) {
                    text.push_str(&format!(
                        "\n{}. `{}` — {} turns ({})",
                        i + 1,
                        &s.session_id[..8.min(s.session_id.len())],
                        s.turn_count,
                        s.ended_at.format("%Y-%m-%d %H:%M"),
                    ));
                }
                self.send(chat_id, &text).await
            }
            "resume" => {
                if args.is_empty() {
                    return self
                        .send(chat_id, "Usage: /resume <session-id-prefix>")
                        .await;
                }
                match self.sessions.find_archived(chat_id, args) {
                    Some(session_id) => {
                        self.sessions.resume_session(chat_id, &session_id);
                        self.sessions.save()?;
                        let short = &session_id[..8.min(session_id.len())];
                        self.send(
                            chat_id,
                            &format!("Resumed session `{short}`. Send a message to continue."),
                        )
                        .await
                    }
                    None => {
                        self.send(chat_id, "No unique session found matching that prefix.")
                            .await
                    }
                }
            }
            "status" => {
                let mut text = format!("Workspace: {}\n", self.workspace_root.display());
                if let Some(session) = self.sessions.get_active(chat_id) {
                    text.push_str(&format!(
                        "Active session: `{}`\nTurns: {}\nStarted: {}",
                        &session.session_id[..8.min(session.session_id.len())],
                        session.turn_count,
                        session.created_at.format("%Y-%m-%d %H:%M"),
                    ));
                } else {
                    text.push_str("No active session.");
                }
                self.send(chat_id, &text).await
            }
            "help" => {
                self.send(
                    chat_id,
                    "Commands:\n\
                     /reset — Archive current session, start fresh\n\
                     /history — List previous sessions\n\
                     /resume <id> — Resume an archived session\n\
                     /status — Show workspace and session info\n\
                     /help — Show this message\n\n\
                     Send any other message to chat with the coordinator.",
                )
                .await
            }
            _ => {
                self.send(
                    chat_id,
                    &format!("Unknown command: /{command}\nSend /help for available commands."),
                )
                .await
            }
        }
    }

    /// Handle a regular text message — route to Claude session.
    async fn handle_message(
        &mut self,
        chat_id: i64,
        user_id: i64,
        user_name: &str,
        text: &str,
    ) -> Result<()> {
        // Push a pending origin so that if the coordinator spawns a swarm agent,
        // we can associate it with this chat for notification routing.
        self.pending_origins.push(PendingOrigin {
            origin: TaskOrigin {
                channel: "telegram".into(),
                chat_id: Some(chat_id),
                user_name: Some(user_name.to_owned()),
                user_id: Some(user_id),
            },
            created_at: std::time::Instant::now(),
        });
        // Start typing indicator loop (expires after 5s, so we resend every 4s).
        let channel = self.channel.clone();
        let typing_cancel = CancellationToken::new();
        let typing_token = typing_cancel.clone();
        tokio::spawn(async move {
            loop {
                channel.send_typing(chat_id).await;
                tokio::select! {
                    _ = typing_token.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {}
                }
            }
        });

        // Check if we have an active session to resume, or start a new one.
        let resume_id = self
            .sessions
            .get_active(chat_id)
            .map(|s| s.session_id.clone());

        let response = match self.run_coordinator_turn(text, resume_id.as_deref()).await {
            Ok((response, session_id)) => {
                // If this was a new session, record it.
                if resume_id.is_none() || resume_id.as_deref() != Some(&session_id) {
                    self.sessions.start_session(chat_id, session_id);
                }

                let should_nudge = self
                    .sessions
                    .record_turn(chat_id, self.config.nudge_turn_threshold);
                self.sessions.save()?;

                let mut full_response = response;
                if should_nudge {
                    full_response.push_str(
                        "\n\n_This session has many turns. Consider /reset for better performance._",
                    );
                }
                full_response
            }
            Err(e) => {
                eprintln!("[daemon] Claude session error: {e}");
                // If session died, remove it so next message auto-reconnects.
                if resume_id.is_some() {
                    self.sessions.remove_active(chat_id);
                    self.sessions.save()?;
                }
                format!(
                    "⚠️ Error: {}\n\nSession has been reset. Send another message to reconnect.",
                    truncate(&e.to_string(), 200),
                )
            }
        };

        // Stop typing indicator before sending the response.
        typing_cancel.cancel();

        eprintln!(
            "[daemon] Responding ({} chars): {}",
            response.len(),
            truncate(&response, 80)
        );
        self.send(chat_id, &response).await
    }

    /// Run a single coordinator turn: send message, collect response text, return (text, session_id).
    async fn run_coordinator_turn(
        &self,
        text: &str,
        resume_id: Option<&str>,
    ) -> Result<(String, String)> {
        let opts = SessionOptions {
            system_prompt: if resume_id.is_none() {
                Some(self.coordinator_prompt.clone())
            } else {
                None
            },
            model: Some(self.config.model.clone()),
            resume: resume_id.map(|s| s.to_owned()),
            max_turns: Some(self.config.max_turns.into()),
            working_dir: Some(self.workspace_root.clone()),
            allowed_tools: vec![
                "Bash".into(),
                "Read".into(),
                "Glob".into(),
                "Grep".into(),
                "WebSearch".into(),
                "WebFetch".into(),
            ],
            ..Default::default()
        };

        let client = ClaudeClient::new();
        let mut session = client
            .spawn(opts)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("failed to spawn Claude session: {e}"))?;

        // Send the user message immediately — do NOT wait for system event first.
        // Claude CLI with --input-format stream-json doesn't emit system event until
        // it receives input, so waiting would deadlock.
        session
            .send_message(text)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("failed to send message: {e}"))?;

        // Collect the response.
        let mut response_text = String::new();
        let mut session_id = String::new();

        loop {
            let event = session
                .next_event()
                .await
                .map_err(|e| color_eyre::eyre::eyre!("error reading event: {e}"))?;

            match event {
                Some(Event::Assistant { message, .. }) => {
                    if let Some(sid) = &message.session_id {
                        session_id.clone_from(sid);
                    }
                    for block in &message.message.content {
                        if let ContentBlock::Text { text } = block {
                            response_text.push_str(text);
                        }
                    }
                    if message.message.stop_reason.as_deref() == Some("end_turn") {
                        break;
                    }
                }
                Some(Event::Result(result)) => {
                    session_id = result.session_id;
                    if let Some(text) = &result.result
                        && response_text.is_empty()
                    {
                        response_text = text.clone();
                    }
                    break;
                }
                Some(Event::RateLimit(rl)) => {
                    if let Some(info) = &rl.rate_limit_info {
                        eprintln!("[daemon] Rate limit status: {info}");
                    }
                }
                Some(_) => {}
                None => break,
            }
        }

        // Close the session gracefully (stdin EOF, let process finish).
        session.close_stdin();

        if response_text.is_empty() {
            response_text = "[No response from coordinator]".to_owned();
        }

        Ok((response_text, session_id))
    }

    /// Poll buzz signals and auto-triage critical/warning ones.
    ///
    /// When inline watchers are enabled, polls them directly and writes to JSONL
    /// for the dashboard. Otherwise, reads new signals from the JSONL file.
    async fn poll_buzz(&mut self) -> Result<()> {
        let signals = if self.inline_watchers.is_some() {
            self.poll_inline_watchers().await
        } else {
            self.poll_buzz_jsonl()
        };

        // Filter for important signals.
        let important: Vec<&Signal> = signals
            .iter()
            .filter(|s| matches!(s.severity, Severity::Critical | Severity::Warning))
            .collect();

        if important.is_empty() {
            return Ok(());
        }

        eprintln!("[daemon] {} new buzz signal(s) to triage", important.len());

        for signal in important {
            let assessment = self.triage_signal(signal).await;
            let alert = format_triage_alert(signal, &assessment);
            let alert_chat_id = self.config.telegram.alert_chat_id;
            self.send(alert_chat_id, &alert).await?;
        }

        self.sessions.save()?;
        Ok(())
    }

    /// Poll inline buzz watchers directly, dedup + prioritize, emit to JSONL.
    async fn poll_inline_watchers(&mut self) -> Vec<Signal> {
        let watchers = match self.inline_watchers.as_mut() {
            Some(w) => w,
            None => return Vec::new(),
        };

        let mut all_signals = Vec::new();

        // Poll each watcher.
        for watcher in watchers.iter_mut() {
            match watcher.poll().await {
                Ok(signals) => {
                    if !signals.is_empty() {
                        eprintln!(
                            "[daemon] {} returned {} signal(s)",
                            watcher.name(),
                            signals.len()
                        );
                    }
                    all_signals.extend(signals);
                }
                Err(e) => {
                    eprintln!("[daemon] {} error: {e}", watcher.name());
                }
            }
        }

        // Check reminders.
        if let Some(reminders) = self.inline_reminders.as_mut() {
            let reminder_signals = reminder::check_reminders(reminders);
            if !reminder_signals.is_empty() {
                eprintln!("[daemon] {} reminder(s) fired", reminder_signals.len());
                all_signals.extend(reminder_signals);
            }
        }

        // Dedup + prioritize.
        let mut signals = deduplicate(&all_signals);
        prioritize(&mut signals);

        // Emit to JSONL so the dashboard can read them.
        if let Some(output_mode) = &self.buzz_output {
            if let Err(e) = output::emit(&signals, output_mode) {
                eprintln!("[daemon] Failed to write buzz signals: {e}");
            }
        }

        // Save watcher cursors.
        save_cursors(watchers);

        signals
    }

    /// Poll buzz signals from the JSONL file (fallback when inline watchers are disabled).
    fn poll_buzz_jsonl(&mut self) -> Vec<Signal> {
        let signals = self.buzz_reader.poll().unwrap_or_default();
        self.sessions.set_buzz_offset(self.buzz_reader.offset());
        signals
    }

    /// Poll swarm state for agent completion notifications.
    async fn poll_swarm(&mut self) -> Result<()> {
        let watcher = match self.swarm_watcher.as_mut() {
            Some(w) => w,
            None => return Ok(()),
        };

        let notifications = watcher.poll();
        if notifications.is_empty() {
            return Ok(());
        }

        // Expire stale pending origins (older than 60 seconds).
        self.pending_origins
            .retain(|p| p.created_at.elapsed().as_secs() < 60);

        let alert_chat_id = self.config.telegram.alert_chat_id;
        let mut origin_map_changed = false;

        for notification in &notifications {
            let wt_id = notification.worktree_id();

            // For new agent spawns, pop a pending origin and record the mapping.
            if let swarm_watcher::SwarmNotification::AgentSpawned {
                worktree_id,
                branch,
                ..
            } = notification
            {
                if let Some(pending) = self.pending_origins.pop() {
                    self.origin_map.insert(
                        worktree_id.clone(),
                        OriginEntry {
                            origin: pending.origin,
                            quest_id: None,
                            task_id: None,
                            branch: Some(branch.clone()),
                            created_at: chrono::Utc::now(),
                        },
                    );
                    origin_map_changed = true;
                }
            }

            // Route the notification to the originating chat, or fall back to alert_chat_id.
            let target_chat_id = self
                .origin_map
                .route_target(wt_id)
                .unwrap_or(alert_chat_id);

            let text = notification.format_telegram();
            self.send(target_chat_id, &text).await?;

            // Clean up origin map entries for completed/closed agents.
            match notification {
                swarm_watcher::SwarmNotification::AgentCompleted { worktree_id, .. }
                | swarm_watcher::SwarmNotification::AgentClosed { worktree_id, .. } => {
                    if self.origin_map.remove(worktree_id).is_some() {
                        origin_map_changed = true;
                    }
                }
                _ => {}
            }
        }

        // Persist origin map if it changed.
        if origin_map_changed {
            if let Err(e) = self.origin_map.save(&self.workspace_root) {
                eprintln!("[daemon] Failed to save origin map: {e}");
            }
        }

        Ok(())
    }

    /// Send a signal to an ephemeral Claude session for triage assessment.
    async fn triage_signal(&self, signal: &Signal) -> String {
        let prompt = format!(
            "You are a triage assistant. Assess this signal and recommend an action.\n\
             Keep your response under 3 sentences.\n\n\
             Source: {}\n\
             Severity: {}\n\
             Title: {}\n\
             Body: {}\n\
             {}",
            signal.source,
            signal.severity,
            signal.title,
            signal.body,
            signal
                .url
                .as_deref()
                .map(|u| format!("URL: {u}"))
                .unwrap_or_default(),
        );

        let opts = SessionOptions {
            system_prompt: Some("You are a concise triage assistant. Assess signals and recommend actions in 1-3 sentences.".into()),
            model: Some(self.config.model.clone()),
            max_turns: Some(1),
            no_session_persistence: true,
            working_dir: Some(self.workspace_root.clone()),
            ..Default::default()
        };

        let client = ClaudeClient::new();
        let session = match client.spawn(opts).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[daemon] Failed to spawn triage session: {e}");
                return "[Triage session could not be started]".to_owned();
            }
        };

        match self.run_ephemeral_session(session, &prompt).await {
            Ok(text) if !text.is_empty() => text,
            Ok(_) => "[Triage session produced no output]".to_owned(),
            Err(e) => {
                eprintln!("[daemon] Triage session error: {e}");
                "[Triage session produced no output]".to_owned()
            }
        }
    }

    /// Run an ephemeral Claude session: send one message, collect response, return.
    async fn run_ephemeral_session(
        &self,
        mut session: apiari_claude_sdk::Session,
        prompt: &str,
    ) -> Result<String> {
        // Send message immediately — don't wait for system event (same deadlock issue).
        session
            .send_message(prompt)
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
                Some(Event::Assistant { message, .. }) => {
                    for block in &message.message.content {
                        if let ContentBlock::Text { text: t } = block {
                            text.push_str(t);
                        }
                    }
                    if message.message.stop_reason.as_deref() == Some("end_turn") {
                        break;
                    }
                }
                Some(Event::Result(r)) => {
                    if let Some(result_text) = &r.result
                        && text.is_empty()
                    {
                        text = result_text.clone();
                    }
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }

        Ok(text)
    }

    /// Send a message through the Telegram channel.
    async fn send(&self, chat_id: i64, text: &str) -> Result<()> {
        self.channel
            .send_message(&OutboundMessage {
                chat_id,
                text: text.to_owned(),
            })
            .await
    }
}

/// Format a triage alert for Telegram.
fn format_triage_alert(signal: &Signal, assessment: &str) -> String {
    let severity_emoji = match signal.severity {
        Severity::Critical => "🔴",
        Severity::Warning => "🟡",
        Severity::Info => "🔵",
    };

    let esc_title = telegram::escape_markdown(&signal.title);
    let esc_source = telegram::escape_markdown(&signal.source);
    let esc_assessment = telegram::escape_markdown(assessment);

    let mut alert = format!(
        "{severity_emoji} *{severity}* — {esc_title}\n\
         Source: {esc_source}\n\n\
         {esc_assessment}",
        severity = signal.severity,
    );

    if let Some(url) = &signal.url {
        alert.push_str(&format!("\n\n[View]({url})"));
    }

    alert
}

/// Truncate a string for display.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}
