//! Daemon mode — persistent Telegram bot + buzz auto-triage.
//!
//! The daemon runs a `tokio::select!` loop over multiple sources:
//! 1. Telegram messages (via mpsc channel from background long-poll)
//! 2. Buzz signals (inline watchers when enabled, else JSONL file poll)
//! 3. Swarm agent completion notifications (when enabled)
//! 4. Shutdown signals (SIGTERM/SIGINT)

pub mod config;
pub mod markdown;
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
use crate::reminder::{self as user_reminder, ReminderStore};
use crate::signal::{Severity, Signal};
use crate::workspace::load_workspace;
use apiari_claude_sdk::types::ContentBlock;
use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions};
use apiari_common::ipc::JsonlReader;
use color_eyre::eyre::{Result, WrapErr};
use config::DaemonConfig;
use origin_map::{OriginEntry, OriginMap, route_notification};
use session_store::SessionStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    if config.buzz.as_ref().is_some_and(|b| b.enabled) {
        eprintln!("[daemon] Inline buzz watchers: enabled");
    }
    if config.swarm_watch.as_ref().is_some_and(|sw| sw.enabled) {
        eprintln!("[daemon] Swarm watcher: enabled");
    }

    let mut runner = DaemonRunner::new(root.to_path_buf(), config)?;
    let restart = runner.run().await?;

    // Clean up PID file before exit or restart.
    remove_pid(root);
    eprintln!("[daemon] PID file removed");

    if restart {
        // Spawn a new daemon process (new PID, fresh logs).
        eprintln!("[daemon] Spawning new daemon process...");
        spawn_background(root, root)?;
    }

    Ok(())
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
    /// User-facing reminders (file-backed, one-shot and cron).
    reminder_store: ReminderStore,
    /// Cancellation token for the main event loop (set at start of `run()`).
    cancel: Option<CancellationToken>,
    /// Set by `/restart` handler before cancelling the loop.
    restart_requested: Arc<AtomicBool>,
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
                watcher.set_hive_dir(workspace_root.join(".hive"));
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

        // Load user-facing reminders.
        let reminder_store = ReminderStore::load(&workspace_root);
        let active_reminders = reminder_store.active().len();
        if active_reminders > 0 {
            eprintln!("[daemon] Loaded {active_reminders} active reminder(s)");
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
            reminder_store,
            cancel: None,
            restart_requested: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn run(&mut self) -> Result<bool> {
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel.clone());

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
        let mut swarm_timer =
            tokio::time::interval(swarm_interval.unwrap_or(std::time::Duration::from_secs(3600)));
        swarm_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip the first immediate tick.
        swarm_timer.tick().await;

        // Reminder check timer (every 60 seconds).
        let mut reminder_timer = tokio::time::interval(std::time::Duration::from_secs(60));
        reminder_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip the first immediate tick.
        reminder_timer.tick().await;

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

                _ = reminder_timer.tick() => {
                    if let Err(e) = self.poll_reminders().await {
                        eprintln!("[daemon] Reminder poll error: {e}");
                    }
                }
            }
        }

        // Persist state before exiting.
        self.sessions.save()?;
        if let Err(e) = self.reminder_store.save() {
            eprintln!("[daemon] Failed to save reminders: {e}");
        }
        if let Err(e) = self.origin_map.save(&self.workspace_root) {
            eprintln!("[daemon] Failed to save origin map: {e}");
        }

        let restart = self.restart_requested.load(Ordering::Relaxed);
        if restart {
            eprintln!("[daemon] State saved. Restarting...");
        } else {
            eprintln!("[daemon] State saved. Goodbye.");
        }
        Ok(restart)
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
                eprintln!(
                    "[daemon] Message from {user_name} (chat={chat_id}): {}",
                    truncate(&text, 80)
                );
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
            "remind" => self.handle_remind_command(chat_id, args).await,
            "reminders" => self.handle_reminders_command(chat_id, args).await,
            "help" => {
                let mut text = String::from(
                    "Commands:\n\
                     /reset — Archive current session, start fresh\n\
                     /history — List previous sessions\n\
                     /resume <id> — Resume an archived session\n\
                     /status — Show workspace and session info\n\
                     /remind <dur> <msg> — Schedule a reminder\n\
                     /remind --cron \"expr\" <msg> — Cron reminder\n\
                     /reminders — List pending reminders\n\
                     /reminders cancel <id> — Cancel a reminder\n\
                     /help — Show this message",
                );
                // Append custom commands.
                for (name, cmd) in &self.config.commands {
                    let desc = cmd
                        .description
                        .as_deref()
                        .unwrap_or(cmd.run.as_str());
                    text.push_str(&format!("\n/{name} — {desc}"));
                }
                text.push_str("\n\nSend any other message to chat with the coordinator.");
                self.send(chat_id, &text).await
            }
            _ if self.config.commands.contains_key(command) => {
                self.run_custom_command(chat_id, command).await
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

    /// Run a user-defined custom command from `[commands]` config.
    async fn run_custom_command(&mut self, chat_id: i64, command: &str) -> Result<()> {
        let cmd = self.config.commands.get(command).cloned().unwrap();
        self.send(chat_id, &format!("Running /{command}...")).await?;

        let output = tokio::process::Command::new("sh")
            .args(["-c", &cmd.run])
            .current_dir(&self.workspace_root)
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut reply = String::new();
                if !stdout.is_empty() {
                    reply.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !reply.is_empty() {
                        reply.push('\n');
                    }
                    reply.push_str(&stderr);
                }
                if out.status.success() {
                    let msg = if reply.is_empty() {
                        format!("/{command} completed.")
                    } else {
                        // Truncate long output for Telegram.
                        let truncated = if reply.len() > 3500 {
                            format!("{}...\n(truncated)", &reply[..3500])
                        } else {
                            reply
                        };
                        format!("/{command} completed:\n```\n{truncated}\n```")
                    };
                    self.send(chat_id, &msg).await?;
                } else {
                    let code = out.status.code().unwrap_or(-1);
                    let msg = if reply.is_empty() {
                        format!("/{command} failed (exit {code}).")
                    } else {
                        let truncated = if reply.len() > 3500 {
                            format!("{}...\n(truncated)", &reply[..3500])
                        } else {
                            reply
                        };
                        format!("/{command} failed (exit {code}):\n```\n{truncated}\n```")
                    };
                    self.send(chat_id, &msg).await?;
                    return Ok(());
                }
            }
            Err(e) => {
                self.send(chat_id, &format!("/{command} error: {e}"))
                    .await?;
                return Ok(());
            }
        }

        // Handle post-action.
        if cmd.then_action.as_deref() == Some("restart") {
            self.send(chat_id, "Restarting daemon...").await?;
            self.restart_requested.store(true, Ordering::Relaxed);
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
        }

        Ok(())
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

        // Build a callback that sends intermediate text blocks to Telegram immediately,
        // splitting long messages to stay within Telegram's 4096-char limit.
        let channel = self.channel.clone();
        let send_fn = move |text: String| {
            let channel = channel.clone();
            async move {
                for chunk in split_message(&text, 4000) {
                    channel
                        .send_message(&OutboundMessage {
                            chat_id,
                            text: chunk,
                        })
                        .await?;
                }
                Ok(())
            }
        };

        let response = match self
            .run_coordinator_turn(text, resume_id.as_deref(), send_fn)
            .await
        {
            Ok((final_text, session_id)) => {
                // If this was a new session, record it.
                if resume_id.is_none() || resume_id.as_deref() != Some(&session_id) {
                    self.sessions.start_session(chat_id, session_id);
                }

                let should_nudge = self
                    .sessions
                    .record_turn(chat_id, self.config.nudge_turn_threshold);
                self.sessions.save()?;

                let mut full_response = final_text;
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

        // Stop typing indicator before sending the final response.
        typing_cancel.cancel();

        // Only send the final response if there's text (may be empty if all
        // content was already streamed as intermediate messages).
        if !response.is_empty() {
            eprintln!(
                "[daemon] Responding ({} chars): {}",
                response.len(),
                truncate(&response, 80)
            );
            self.send(chat_id, &response).await?;
        }

        Ok(())
    }

    /// Run a single coordinator turn: send message, stream intermediate text blocks
    /// to the caller via `send_fn`, return session_id.
    ///
    /// Each `ContentBlock::Text` from intermediate `Event::Assistant` events (those
    /// without `stop_reason: "end_turn"`) is forwarded immediately through `send_fn`,
    /// giving the user real-time responses as Claude thinks and acts. The final
    /// end_turn text (if any) is returned so the caller can append nudge hints.
    async fn run_coordinator_turn<F, Fut>(
        &self,
        text: &str,
        resume_id: Option<&str>,
        send_fn: F,
    ) -> Result<(String, String)>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
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

        let mut session_id = String::new();
        // Text from the final end_turn event (returned to caller for nudge appending).
        let mut final_text = String::new();
        // Track whether we sent anything so we can detect empty responses.
        let mut sent_any = false;

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

                    let is_end_turn = message.message.stop_reason.as_deref() == Some("end_turn");

                    // Collect text blocks from this event.
                    let mut block_text = String::new();
                    for block in &message.message.content {
                        if let ContentBlock::Text { text } = block {
                            block_text.push_str(text);
                        }
                    }

                    if !block_text.is_empty() {
                        if is_end_turn {
                            // Final response — return to caller so it can append nudge.
                            final_text = block_text;
                        } else {
                            // Intermediate response — send immediately.
                            eprintln!(
                                "[daemon] Streaming intermediate ({} chars): {}",
                                block_text.len(),
                                truncate(&block_text, 80)
                            );
                            if let Err(e) = send_fn(block_text).await {
                                eprintln!("[daemon] Failed to send intermediate text: {e}");
                            } else {
                                sent_any = true;
                            }
                        }
                    }

                    if is_end_turn {
                        break;
                    }
                }
                Some(Event::Result(result)) => {
                    session_id = result.session_id;
                    if let Some(text) = &result.result
                        && final_text.is_empty()
                        && !sent_any
                    {
                        final_text = text.clone();
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

        if final_text.is_empty() && !sent_any {
            final_text = "[No response from coordinator]".to_owned();
        }

        Ok((final_text, session_id))
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
        if let Some(output_mode) = &self.buzz_output
            && let Err(e) = output::emit(&signals, output_mode)
        {
            eprintln!("[daemon] Failed to write buzz signals: {e}");
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

    /// Poll user-facing reminders and send fired ones to Telegram.
    async fn poll_reminders(&mut self) -> Result<()> {
        // Reload from disk to pick up CLI-created reminders.
        self.reminder_store = ReminderStore::load(&self.workspace_root);

        let fired = self.reminder_store.check_and_advance();
        if fired.is_empty() {
            return Ok(());
        }

        eprintln!("[daemon] {} reminder(s) fired", fired.len());

        for r in &fired {
            let chat_id = r.chat_id.unwrap_or(self.config.telegram.alert_chat_id);
            let text = format!("🔔 Reminder: {}", r.message);
            self.send(chat_id, &text).await?;
            eprintln!(
                "[daemon] Reminder fired: \"{}\" -> chat {}",
                r.message, chat_id
            );
        }

        self.reminder_store.save()?;
        Ok(())
    }

    /// Handle `/remind` Telegram command.
    async fn handle_remind_command(&mut self, chat_id: i64, args: &str) -> Result<()> {
        if args.is_empty() {
            return self
                .send(
                    chat_id,
                    "Usage:\n\
                     /remind 30m Check PR status\n\
                     /remind --cron \"0 9 * * *\" Daily standup",
                )
                .await;
        }

        let result = if let Some(rest) = args.strip_prefix("--cron ") {
            match user_reminder::parse_cron_args(rest) {
                Ok((cron_expr, message)) => {
                    user_reminder::create_cron(&cron_expr, &message, Some(chat_id))
                }
                Err(e) => Err(e),
            }
        } else {
            match args.split_once(' ') {
                Some((duration_str, message)) => {
                    user_reminder::create_oneshot(duration_str, message.trim(), Some(chat_id))
                }
                None => Err("Usage: /remind <duration> <message>".into()),
            }
        };

        match result {
            Ok(r) => {
                let fire_str = r
                    .fire_at
                    .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_else(|| "unknown".into());
                let short_id = r.id[..8.min(r.id.len())].to_owned();
                let msg = r.message.clone();

                // Reload, add, save.
                self.reminder_store = ReminderStore::load(&self.workspace_root);
                self.reminder_store.add(r);
                self.reminder_store.save()?;

                eprintln!("[daemon] Created reminder {short_id} from Telegram chat {chat_id}");
                self.send(
                    chat_id,
                    &format!(
                        "Reminder set (ID: `{short_id}`)\n\
                         Next fire: {fire_str}\n\
                         Message: {msg}"
                    ),
                )
                .await
            }
            Err(e) => self.send(chat_id, &format!("Error: {e}")).await,
        }
    }

    /// Handle `/reminders` Telegram command.
    async fn handle_reminders_command(&mut self, chat_id: i64, args: &str) -> Result<()> {
        self.reminder_store = ReminderStore::load(&self.workspace_root);

        if let Some(rest) = args.strip_prefix("cancel") {
            let id_prefix = rest.trim();
            if id_prefix.is_empty() {
                return self.send(chat_id, "Usage: /reminders cancel <id>").await;
            }
            match self.reminder_store.cancel(id_prefix) {
                Ok(cancelled_id) => {
                    self.reminder_store.save()?;
                    let short = &cancelled_id[..8.min(cancelled_id.len())];
                    eprintln!("[daemon] Cancelled reminder {short} from Telegram chat {chat_id}");
                    self.send(chat_id, &format!("Cancelled reminder `{short}`."))
                        .await
                }
                Err(user_reminder::CancelError::NotFound) => {
                    self.send(
                        chat_id,
                        &format!("No reminder found matching `{id_prefix}`."),
                    )
                    .await
                }
                Err(user_reminder::CancelError::Ambiguous(ids)) => {
                    let list = ids
                        .iter()
                        .map(|id| format!("  `{}`", &id[..8.min(id.len())]))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.send(
                        chat_id,
                        &format!("Multiple reminders match `{id_prefix}`:\n{list}"),
                    )
                    .await
                }
            }
        } else {
            let active = self.reminder_store.active();
            let text = user_reminder::format_reminder_list(&active);
            self.send(chat_id, &text).await
        }
    }

    /// Poll swarm state for agent completion notifications.
    ///
    /// To avoid Telegram rate-limiting (~1 msg/sec per chat), notifications are
    /// sent with a 500ms delay between consecutive messages. When 3 or more
    /// notifications land in a single poll cycle for the same chat, they are
    /// batched into a single summary message instead of sent individually.
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

        // Phase 1: Process origin map and collect routed (chat_id, notification) pairs.
        let mut routed: Vec<(i64, &swarm_watcher::SwarmNotification)> = Vec::new();

        for notification in &notifications {
            let wt_id = notification.worktree_id();

            // For new agent spawns, pop a pending origin and record the mapping.
            if let swarm_watcher::SwarmNotification::AgentSpawned {
                worktree_id,
                branch,
                ..
            } = notification
                && let Some(pending) = self.pending_origins.pop()
            {
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

            // Route the notification to the originating chat, or fall back to alert_chat_id.
            let target_chat_id = route_notification(&self.origin_map, wt_id, alert_chat_id);
            routed.push((target_chat_id, notification));

            // Clean up origin map entries for completed/closed agents.
            // PrOpened routes to the origin chat but does NOT clean up —
            // the agent is still running and may produce more notifications.
            match notification {
                swarm_watcher::SwarmNotification::AgentCompleted { worktree_id, .. }
                | swarm_watcher::SwarmNotification::AgentClosed { worktree_id, .. } => {
                    if self.origin_map.remove(worktree_id).is_some() {
                        origin_map_changed = true;
                    }
                }
                swarm_watcher::SwarmNotification::PrOpened { .. }
                | swarm_watcher::SwarmNotification::AgentSpawned { .. }
                | swarm_watcher::SwarmNotification::AgentStalled { .. }
                | swarm_watcher::SwarmNotification::AgentWaiting { .. } => {}
            }
        }

        // Phase 2: Group by chat_id, preserving insertion order.
        let mut by_chat: Vec<(i64, Vec<&swarm_watcher::SwarmNotification>)> = Vec::new();
        for (chat_id, notification) in &routed {
            if let Some((_, group)) = by_chat.iter_mut().find(|(id, _)| id == chat_id) {
                group.push(notification);
            } else {
                by_chat.push((*chat_id, vec![notification]));
            }
        }

        // Phase 3: Send — batch 3+ notifications per chat, else send with 500ms delays.
        // If a send fails (e.g. stale chat_id from origin map), fall back to alert_chat_id.
        let mut first_send = true;
        for (chat_id, group) in &by_chat {
            let texts: Vec<String> = if group.len() >= 3 {
                vec![format_swarm_batch(group)]
            } else {
                group.iter().map(|n| n.format_telegram()).collect()
            };
            for text in &texts {
                if !first_send {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                if let Err(e) = self.send(*chat_id, text).await {
                    eprintln!(
                        "[daemon] Failed to send to chat {chat_id}: {e} — retrying on alert_chat_id"
                    );
                    if *chat_id != alert_chat_id {
                        let _ = self.send(alert_chat_id, text).await;
                    }
                }
                first_send = false;
            }
        }

        // Phase 4: Auto-triage eligible notifications — spawned in background
        // so coordinator calls never block the poll loop.
        if self
            .config
            .swarm_watch
            .as_ref()
            .is_some_and(|sw| sw.auto_triage)
        {
            for (chat_id, notification) in &routed {
                if let Some(prompt) = auto_triage_prompt(notification) {
                    let chat_id = *chat_id;
                    let channel = self.channel.clone();
                    let coordinator_prompt = self.coordinator_prompt.clone();
                    let model = self.config.model.clone();
                    let workspace_root = self.workspace_root.clone();
                    let worktree_id = notification.worktree_id().to_owned();
                    let notification_kind = match notification {
                        swarm_watcher::SwarmNotification::PrOpened { .. } => "PrOpened",
                        swarm_watcher::SwarmNotification::AgentWaiting { .. } => "AgentWaiting",
                        _ => "notification",
                    };
                    tokio::spawn(async move {
                        eprintln!(
                            "[daemon] Auto-triaging {notification_kind} for {worktree_id}"
                        );

                        let system_prompt = format!(
                            "{coordinator_prompt}\n\n## Auto-Triage Mode\n\
                             Reply in 1-2 sentences with a specific suggested action. Be direct.",
                        );

                        let opts = SessionOptions {
                            system_prompt: Some(system_prompt),
                            model: Some(model),
                            max_turns: Some(1),
                            no_session_persistence: true,
                            working_dir: Some(workspace_root),
                            allowed_tools: vec![
                                "Bash".into(),
                                "Read".into(),
                                "Glob".into(),
                                "Grep".into(),
                            ],
                            ..Default::default()
                        };

                        let client = ClaudeClient::new();
                        let session = match client.spawn(opts).await {
                            Ok(s) => s,
                            Err(e) => {
                                eprintln!("[daemon] Failed to spawn auto-triage session: {e}");
                                return;
                            }
                        };

                        let response = match run_ephemeral_session(session, &prompt).await {
                            Ok(text) if !text.is_empty() => text,
                            Ok(_) => {
                                eprintln!("[daemon] Auto-triage session produced no output");
                                return;
                            }
                            Err(e) => {
                                eprintln!("[daemon] Auto-triage session error: {e}");
                                return;
                            }
                        };

                        eprintln!(
                            "[daemon] Auto-triage suggestion ({} chars): {}",
                            response.len(),
                            truncate(&response, 80)
                        );

                        let text = markdown::sanitize_for_telegram(&format!("💡 {response}"));
                        for chunk in split_message(&text, 4000) {
                            if let Err(e) = channel
                                .send_message(&OutboundMessage {
                                    chat_id,
                                    text: chunk,
                                })
                                .await
                            {
                                eprintln!("[daemon] Failed to send auto-triage result: {e}");
                            }
                        }
                    });
                }
            }
        }

        // Persist origin map if it changed.
        if origin_map_changed && let Err(e) = self.origin_map.save(&self.workspace_root) {
            eprintln!("[daemon] Failed to save origin map: {e}");
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

        match run_ephemeral_session(session, &prompt).await {
            Ok(text) if !text.is_empty() => text,
            Ok(_) => "[Triage session produced no output]".to_owned(),
            Err(e) => {
                eprintln!("[daemon] Triage session error: {e}");
                "[Triage session produced no output]".to_owned()
            }
        }
    }

    /// Send a message through the Telegram channel, splitting if too long.
    async fn send(&self, chat_id: i64, text: &str) -> Result<()> {
        let text = markdown::sanitize_for_telegram(text);
        for chunk in split_message(&text, 4000) {
            self.channel
                .send_message(&OutboundMessage {
                    chat_id,
                    text: chunk,
                })
                .await?;
        }
        Ok(())
    }
}

/// Run an ephemeral Claude session: send one message, collect response, return.
async fn run_ephemeral_session(
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

/// Build an auto-triage prompt for a swarm notification, if applicable.
///
/// Returns `Some(prompt)` for notification types that benefit from a coordinator
/// follow-up suggestion (PrOpened, AgentWaiting), `None` for others.
fn auto_triage_prompt(notification: &swarm_watcher::SwarmNotification) -> Option<String> {
    match notification {
        swarm_watcher::SwarmNotification::PrOpened {
            worktree_id,
            pr_url,
            pr_title,
            ..
        } => {
            let title = pr_title.as_deref().unwrap_or("untitled");
            Some(format!(
                "Worker {worktree_id} opened PR: \"{title}\" ({pr_url}). \
                 Should I review and merge it, or is there anything to check first?"
            ))
        }
        swarm_watcher::SwarmNotification::AgentWaiting {
            worktree_id,
            pr_url,
            ..
        } => match pr_url {
            Some(url) => Some(format!(
                "Worker {worktree_id} is waiting and has an open PR ({url}). \
                 Want me to merge the PR and close the worker?"
            )),
            None => Some(format!(
                "Worker {worktree_id} is waiting for input. \
                 Want me to nudge it, or should I close it?"
            )),
        },
        _ => None,
    }
}

/// Format a batch of swarm notifications into a single summary message.
///
/// Used when 3+ notifications arrive in one poll cycle for the same chat,
/// to avoid spamming Telegram with rapid-fire messages.
fn format_swarm_batch(notifications: &[&swarm_watcher::SwarmNotification]) -> String {
    let mut text = format!("🐝 {} swarm updates:", notifications.len());
    for n in notifications {
        text.push_str(&format!("\n• {}", n.summary_line()));
    }
    text
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

/// Split text into chunks of at most `limit` characters, preferring to break at newlines.
fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.len() <= limit {
        return vec![text.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= limit {
            chunks.push(remaining.to_owned());
            break;
        }

        // Look for the last newline within the limit.
        // Find the nearest char boundary at or before `limit` to avoid
        // slicing in the middle of a multi-byte UTF-8 character.
        let safe_limit = floor_char_boundary(remaining, limit);
        let split_at = remaining[..safe_limit]
            .rfind('\n')
            .map(|pos| pos + 1) // include the newline in the current chunk
            .unwrap_or(safe_limit); // no newline found — hard split at limit

        chunks.push(remaining[..split_at].to_owned());
        remaining = &remaining[split_at..];
    }

    chunks
}

/// Truncate a string for display.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..floor_char_boundary(s, max)]
    }
}

/// Find the largest byte index `<= max` that is a valid char boundary.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Dispatch routed swarm notifications through a channel.
///
/// Groups by chat_id, batches 3+ per chat into one summary message, sends
/// individually otherwise. Falls back to `alert_chat_id` on send failure.
#[cfg(test)]
pub(crate) async fn dispatch_notifications(
    channel: &dyn Channel,
    routed: &[(i64, &swarm_watcher::SwarmNotification)],
    alert_chat_id: i64,
) {
    // Group by chat_id, preserving insertion order.
    let mut by_chat: Vec<(i64, Vec<&swarm_watcher::SwarmNotification>)> = Vec::new();
    for &(chat_id, notification) in routed {
        if let Some((_, group)) = by_chat.iter_mut().find(|(id, _)| *id == chat_id) {
            group.push(notification);
        } else {
            by_chat.push((chat_id, vec![notification]));
        }
    }

    let mut first_send = true;
    for (chat_id, group) in &by_chat {
        let texts: Vec<String> = if group.len() >= 3 {
            vec![format_swarm_batch(group)]
        } else {
            group.iter().map(|n| n.format_telegram()).collect()
        };
        for text in &texts {
            if !first_send {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            if let Err(e) = channel
                .send_message(&OutboundMessage {
                    chat_id: *chat_id,
                    text: text.clone(),
                })
                .await
            {
                eprintln!(
                    "[daemon] Failed to send to chat {chat_id}: {e} — retrying on alert_chat_id"
                );
                if *chat_id != alert_chat_id {
                    let _ = channel
                        .send_message(&OutboundMessage {
                            chat_id: alert_chat_id,
                            text: text.clone(),
                        })
                        .await;
                }
            }
            first_send = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_swarm_batch_three_notifications() {
        let n1 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/task-a".into(),
            summary: Some("propagate waiting status".into()),
        };
        let n2 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "2".into(),
            branch: "swarm/task-b".into(),
            summary: Some("agent waiting notification".into()),
        };
        let n3 = swarm_watcher::SwarmNotification::AgentClosed {
            worktree_id: "3".into(),
            branch: "swarm/task-c".into(),
            duration: "5m".into(),
            pr_url: None,
        };

        let batch = format_swarm_batch(&[&n1, &n2, &n3]);
        assert!(batch.starts_with("🐝 3 swarm updates:"));
        assert!(batch.contains("• New agent spawned — task-a (propagate waiting status)"));
        assert!(batch.contains("• New agent spawned — task-b (agent waiting notification)"));
        assert!(batch.contains("• Agent closed — task-c"));
    }

    #[test]
    fn test_format_swarm_batch_mixed_types() {
        let spawned = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/feat-1".into(),
            summary: None,
        };
        let completed = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "2".into(),
            branch: "swarm/feat-2".into(),
            duration: "10m".into(),
            pr_url: Some("https://github.com/example/pull/7".into()),
        };
        let pr = swarm_watcher::SwarmNotification::PrOpened {
            worktree_id: "3".into(),
            branch: "swarm/feat-3".into(),
            pr_url: "https://github.com/example/pull/8".into(),
            pr_title: Some("Add feature 3".into()),
            duration: "12m".into(),
        };
        let stalled = swarm_watcher::SwarmNotification::AgentStalled {
            worktree_id: "4".into(),
            branch: "swarm/feat-4".into(),
            stall_kind: swarm_watcher::StallKind::Idle { minutes: 20 },
        };

        let batch = format_swarm_batch(&[&spawned, &completed, &pr, &stalled]);
        assert!(batch.starts_with("🐝 4 swarm updates:"));
        assert!(batch.contains("• New agent spawned — feat-1"));
        assert!(
            batch.contains("• Agent completed — feat-2 (PR: https://github.com/example/pull/7)")
        );
        assert!(batch.contains("• PR opened — feat-3 (https://github.com/example/pull/8)"));
        assert!(batch.contains("• Agent stalled — feat-4 (idle 20m)"));
    }

    // --- Notification dispatch tests (mockall-backed) ---

    use crate::channel::MockChannel;
    use crate::daemon::origin_map::route_notification;

    fn make_notification(wt_id: &str, branch: &str) -> swarm_watcher::SwarmNotification {
        swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: wt_id.into(),
            branch: branch.into(),
            duration: "5m".into(),
            pr_url: None,
        }
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_notification_routed_to_origin_chat() {
        let mut om = origin_map::OriginMap::default();
        om.insert(
            "wt-1".into(),
            origin_map::OriginEntry {
                origin: crate::quest::TaskOrigin {
                    channel: "telegram".into(),
                    chat_id: Some(100),
                    user_name: None,
                    user_id: None,
                },
                quest_id: None,
                task_id: None,
                branch: None,
                created_at: chrono::Utc::now(),
            },
        );

        let alert_chat_id = 999;
        let target = route_notification(&om, "wt-1", alert_chat_id);
        assert_eq!(target, 100, "should route to origin chat");

        let notification = make_notification("wt-1", "swarm/fix-bug");
        let routed = vec![(target, &notification)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 100)
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, alert_chat_id).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_notification_falls_back_to_alert_chat_when_origin_missing() {
        let om = origin_map::OriginMap::default();
        let alert_chat_id = 999;

        let target = route_notification(&om, "unknown-wt", alert_chat_id);
        assert_eq!(target, 999, "should fall back to alert chat");

        let notification = make_notification("unknown-wt", "swarm/task");
        let routed = vec![(target, &notification)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 999)
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, alert_chat_id).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_notification_falls_back_when_send_fails() {
        let notification = make_notification("wt-1", "swarm/fix");
        let routed = vec![(100_i64, &notification)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 100)
            .times(1)
            .returning(|_| Err(color_eyre::eyre::eyre!("chat not found")));
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 999)
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, 999).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_batch_sends_single_message_for_three_or_more() {
        let n1 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
        };
        let n2 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "2".into(),
            branch: "swarm/b".into(),
            summary: None,
        };
        let n3 = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "3".into(),
            branch: "swarm/c".into(),
            duration: "5m".into(),
            pr_url: None,
        };

        let routed = vec![(100_i64, &n1), (100_i64, &n2), (100_i64, &n3)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| {
                msg.chat_id == 100 && msg.text.contains("3 swarm updates:")
            })
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, 999).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_batch_sends_individually_for_fewer_than_three() {
        let n1 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
        };
        let n2 = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "2".into(),
            branch: "swarm/b".into(),
            duration: "5m".into(),
            pr_url: None,
        };

        let routed = vec![(100_i64, &n1), (100_i64, &n2)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 100)
            .times(2)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, 999).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_all_notifications_sent_even_if_one_fails() {
        let n1 = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
        };
        let n2 = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "2".into(),
            branch: "swarm/b".into(),
            duration: "5m".into(),
            pr_url: None,
        };

        let routed = vec![(100_i64, &n1), (200_i64, &n2)];

        let mut mock = MockChannel::new();
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 100)
            .times(1)
            .returning(|_| Err(color_eyre::eyre::eyre!("error")));
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 999)
            .times(1)
            .returning(|_| Ok(()));
        mock.expect_send_message()
            .withf(|msg: &OutboundMessage| msg.chat_id == 200)
            .times(1)
            .returning(|_| Ok(()));

        dispatch_notifications(&mock, &routed, 999).await;
    }

    // --- Auto-triage prompt tests ---

    #[test]
    fn test_auto_triage_prompt_pr_opened() {
        let n = swarm_watcher::SwarmNotification::PrOpened {
            worktree_id: "hive-3".into(),
            branch: "swarm/add-auth".into(),
            pr_url: "https://github.com/ApiariTools/hive/pull/22".into(),
            pr_title: Some("Add OAuth support".into()),
            duration: "10m".into(),
        };
        let prompt = auto_triage_prompt(&n).unwrap();
        assert!(prompt.contains("hive-3"));
        assert!(prompt.contains("Add OAuth support"));
        assert!(prompt.contains("https://github.com/ApiariTools/hive/pull/22"));
        assert!(prompt.contains("review and merge"));
    }

    #[test]
    fn test_auto_triage_prompt_pr_opened_no_title() {
        let n = swarm_watcher::SwarmNotification::PrOpened {
            worktree_id: "hive-1".into(),
            branch: "swarm/fix".into(),
            pr_url: "https://github.com/example/pull/1".into(),
            pr_title: None,
            duration: "5m".into(),
        };
        let prompt = auto_triage_prompt(&n).unwrap();
        assert!(prompt.contains("untitled"));
    }

    #[test]
    fn test_auto_triage_prompt_agent_waiting_with_pr() {
        let n = swarm_watcher::SwarmNotification::AgentWaiting {
            worktree_id: "hive-2".into(),
            branch: "swarm/feat".into(),
            summary: Some("Add feature".into()),
            pr_url: Some("https://github.com/example/pull/5".into()),
        };
        let prompt = auto_triage_prompt(&n).unwrap();
        assert!(prompt.contains("hive-2"));
        assert!(prompt.contains("https://github.com/example/pull/5"));
        assert!(prompt.contains("merge the PR"));
    }

    #[test]
    fn test_auto_triage_prompt_agent_waiting_no_pr() {
        let n = swarm_watcher::SwarmNotification::AgentWaiting {
            worktree_id: "hive-4".into(),
            branch: "swarm/task".into(),
            summary: None,
            pr_url: None,
        };
        let prompt = auto_triage_prompt(&n).unwrap();
        assert!(prompt.contains("hive-4"));
        assert!(prompt.contains("waiting for input"));
        assert!(prompt.contains("nudge"));
    }

    #[test]
    fn test_auto_triage_prompt_returns_none_for_other_types() {
        let spawned = swarm_watcher::SwarmNotification::AgentSpawned {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            summary: None,
        };
        assert!(auto_triage_prompt(&spawned).is_none());

        let completed = swarm_watcher::SwarmNotification::AgentCompleted {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            duration: "5m".into(),
            pr_url: None,
        };
        assert!(auto_triage_prompt(&completed).is_none());

        let closed = swarm_watcher::SwarmNotification::AgentClosed {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            duration: "5m".into(),
            pr_url: None,
        };
        assert!(auto_triage_prompt(&closed).is_none());

        let stalled = swarm_watcher::SwarmNotification::AgentStalled {
            worktree_id: "1".into(),
            branch: "swarm/a".into(),
            stall_kind: swarm_watcher::StallKind::Idle { minutes: 10 },
        };
        assert!(auto_triage_prompt(&stalled).is_none());
    }
}
