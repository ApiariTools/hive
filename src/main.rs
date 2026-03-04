//! Hive — the orchestration brain of Apiari.
//!
//! Hive uses `apiari-claude-sdk` to run Claude agent sessions (coordinator +
//! workers) and calls the `swarm` CLI for execution in isolated worktrees.

mod buzz;
mod channel;
mod coordinator;
mod daemon;
mod doctor;
#[allow(dead_code)]
mod github;
mod keeper;
mod pipeline;
mod presence;
mod quest;
mod reminder;
#[allow(dead_code)]
mod routing;
pub(crate) mod signal;
mod ui;
mod worker;
mod workspace;

use clap::{Parser, Subcommand};
use color_eyre::eyre::{Result, WrapErr};
use std::path::{Path, PathBuf};

use crate::coordinator::Coordinator;
use crate::quest::{QuestStore, default_store_path};
use crate::workspace::{init_workspace, load_workspace};

/// Apiari Hive — plan and coordinate work across agents.
#[derive(Parser)]
#[command(name = "hive", version, about)]
struct Cli {
    /// Working directory (defaults to current directory).
    #[arg(short = 'C', long, global = true)]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start working on a quest.
    Start {
        /// Quest ID to start. If omitted, lists available quests.
        quest_id: Option<String>,
    },

    /// Interactive chat with the coordinator agent.
    Chat,

    /// Create a new quest with planning.
    Plan {
        /// Description of what you want to accomplish.
        description: String,
    },

    /// Show workspace and quest status.
    Status {
        /// Output status as JSON for machine consumption.
        #[arg(long)]
        json: bool,
    },

    /// Initialize a new workspace.
    Init,

    /// Manage the persistent daemon (Telegram bot + buzz auto-triage).
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Run the signal aggregator (poll Sentry, GitHub, webhooks, reminders).
    Buzz {
        /// Run continuously, polling on interval.
        #[arg(long, conflicts_with = "once")]
        daemon: bool,

        /// Single poll, then exit.
        #[arg(long)]
        once: bool,

        /// Run a single sweep (re-evaluate all unresolved issues), then exit.
        #[arg(long, conflicts_with_all = ["daemon", "once"])]
        sweep: bool,

        /// Path to config file (overrides workspace.yaml buzz section).
        #[arg(long)]
        config: Option<PathBuf>,

        /// Output mode: stdout, file, webhook.
        #[arg(long)]
        output: Option<String>,
    },

    /// Launch the unified hive TUI (chat + workers + tabs).
    Ui,

    /// Launch the read-only dashboard TUI.
    Dashboard {
        /// Print status once and exit (no TUI).
        #[arg(long)]
        once: bool,
    },

    /// Schedule a reminder.
    Remind {
        /// Duration until reminder fires (e.g. 30m, 2h, 1d).
        #[arg(required_unless_present = "cron", conflicts_with = "cron")]
        duration: Option<String>,

        /// Cron expression for repeating reminders (e.g. "0 9 * * *").
        #[arg(long)]
        cron: Option<String>,

        /// The reminder message (multiple words joined).
        #[arg(trailing_var_arg = true, required = true)]
        message: Vec<String>,
    },

    /// Run the task pipeline: refine → context → plan → dispatch to swarm.
    Dispatch {
        /// Raw task description.
        description: String,
        /// Target repo.
        #[arg(long)]
        repo: Option<String>,
        /// Stop after a specific stage (refine, context, plan).
        #[arg(long)]
        until: Option<String>,
        /// Profile slug for the agent.
        #[arg(long, default_value = "default")]
        profile: String,
        /// Print artifacts without dispatching.
        #[arg(long)]
        dry_run: bool,
    },

    /// Verify a worktree against its task's acceptance criteria.
    Verify {
        /// Worktree ID to verify.
        worktree_id: String,
    },

    /// Identify relevant codebase context for a task (CONTEXT.md).
    Context {
        /// Path to TASK.md file, or raw task description.
        input: String,
        /// Write output to a file instead of stdout.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },

    /// Refine a raw prompt into a structured task specification (TASK.md).
    Refine {
        /// Raw task description.
        description: String,
        /// Write output to a file instead of stdout.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },

    /// Run diagnostic checks on workspace, daemon, and tooling health.
    Doctor {
        /// Automatically fix issues that can be resolved safely.
        #[arg(long)]
        fix: bool,
    },

    /// List or manage pending reminders.
    Reminders {
        #[command(subcommand)]
        action: Option<RemindersAction>,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start the daemon (backgrounds by default).
    Start {
        /// Run in foreground instead of daemonizing.
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the running daemon.
    Stop,
    /// Restart the daemon.
    Restart,
}

#[derive(Subcommand)]
enum RemindersAction {
    /// Cancel a reminder by ID (or ID prefix).
    Cancel {
        /// Reminder ID (or prefix).
        id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();
    let cwd = match &cli.dir {
        Some(d) => d.clone(),
        None => std::env::current_dir().wrap_err("failed to get current directory")?,
    };

    match cli.command {
        Command::Init => cmd_init(&cwd),
        Command::Status { json } => cmd_status(&cwd, json),
        Command::Chat => cmd_chat(&cwd).await,
        Command::Plan { description } => cmd_plan(&cwd, &description).await,
        Command::Start { quest_id } => cmd_start(&cwd, quest_id.as_deref()).await,
        Command::Daemon { action } => match action {
            DaemonAction::Start { foreground } => daemon::start(foreground).await,
            DaemonAction::Stop => daemon::stop(),
            DaemonAction::Restart => {
                let _ = daemon::stop();
                daemon::start(false).await
            }
        },
        Command::Buzz {
            daemon,
            once,
            sweep,
            config,
            output,
        } => {
            buzz::run(
                config.as_deref(),
                &cwd,
                daemon,
                once,
                sweep,
                output.as_deref(),
            )
            .await
        }
        Command::Ui => ui::run(&cwd).await,
        Command::Dashboard { once } => keeper::run(once).await,
        Command::Remind {
            duration,
            cron,
            message,
        } => cmd_remind(
            &cwd,
            duration.as_deref(),
            cron.as_deref(),
            &message.join(" "),
        ),
        Command::Verify { worktree_id } => cmd_verify(&cwd, &worktree_id).await,
        Command::Dispatch {
            description,
            repo,
            until,
            profile,
            dry_run,
        } => {
            cmd_dispatch(
                &cwd,
                &description,
                repo.as_deref(),
                until.as_deref(),
                &profile,
                dry_run,
            )
            .await
        }
        Command::Context { input, output } => cmd_context(&cwd, &input, output.as_deref()).await,
        Command::Refine {
            description,
            output,
        } => cmd_refine(&cwd, &description, output.as_deref()).await,
        Command::Doctor { fix } => doctor::run(&cwd, fix),
        Command::Reminders { action } => match action {
            None => cmd_list_reminders(&cwd),
            Some(RemindersAction::Cancel { id }) => cmd_cancel_reminder(&cwd, &id),
        },
    }
}

/// Initialize a new workspace.
fn cmd_init(cwd: &Path) -> Result<()> {
    let config_path = init_workspace(cwd)?;
    println!("Workspace initialized: {}", config_path.display());
    println!(
        "Edit {} to configure your workspace.",
        config_path.display()
    );
    Ok(())
}

// ── JSON output types for `hive status --json` ───────────

#[derive(serde::Serialize)]
struct StatusOutput {
    workers: Vec<WorkerOutput>,
}

#[derive(serde::Serialize)]
struct WorkerOutput {
    id: String,
    branch: String,
    agent_kind: String,
    #[serde(default)]
    agent_session_status: Option<String>,
    #[serde(default)]
    pr: Option<PrOutput>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    phase: Option<String>,
}

#[derive(serde::Serialize)]
struct PrOutput {
    url: String,
    title: String,
    number: u64,
    state: String,
}

/// Show workspace and quest status.
fn cmd_status(cwd: &Path, json: bool) -> Result<()> {
    if json {
        let sessions = keeper::discovery::discover_sessions().unwrap_or_default();

        let workers: Vec<WorkerOutput> = sessions
            .into_iter()
            .flat_map(|s| s.worktrees)
            .map(|wt| WorkerOutput {
                id: wt.id,
                branch: wt.branch,
                agent_kind: wt.agent_kind,
                agent_session_status: wt.agent_session_status,
                pr: wt.pr.map(|p| PrOutput {
                    url: p.url,
                    title: p.title,
                    number: p.number,
                    state: p.state,
                }),
                summary: wt.summary,
                phase: wt.phase,
            })
            .collect();

        let output = StatusOutput { workers };
        let json_str =
            serde_json::to_string_pretty(&output).wrap_err("serializing status JSON")?;
        println!("{json_str}");
        return Ok(());
    }

    let workspace = load_workspace(cwd)?;
    let store = QuestStore::new(default_store_path(&workspace.root));

    println!("Workspace: {}", workspace.name);
    println!("Root: {}", workspace.root.display());
    println!("Default agent: {}", workspace.default_agent);

    if !workspace.repos.is_empty() {
        println!("\nRepositories:");
        for repo in &workspace.repos {
            println!("  - {repo}");
        }
    }

    let quests = store.list()?;
    if quests.is_empty() {
        println!("\nNo quests. Run `hive plan \"description\"` to create one.");
    } else {
        println!("\nQuests:");
        for quest in &quests {
            println!("  {}", quest.summary());
        }
    }

    Ok(())
}

/// Interactive chat with the coordinator.
async fn cmd_chat(cwd: &Path) -> Result<()> {
    let workspace = load_workspace(cwd)?;
    let store = QuestStore::new(default_store_path(&workspace.root));
    let mut coord = Coordinator::new(workspace, store);
    coord.run_chat().await
}

/// Plan a new quest.
async fn cmd_plan(cwd: &Path, description: &str) -> Result<()> {
    let workspace = load_workspace(cwd)?;
    let store = QuestStore::new(default_store_path(&workspace.root));
    let mut coord = Coordinator::new(workspace, store);

    let quest = coord.plan_quest(description).await?;
    println!("Created quest: {}", quest.summary());
    println!("Quest ID: {}", quest.id);
    println!("\nTasks:");
    for task in &quest.tasks {
        println!("  [{}] {} ({})", &task.id[..8], task.title, task.status);
    }
    println!("\nRun `hive start {}` to begin.", &quest.id[..8]);

    Ok(())
}

/// Start working on a quest.
async fn cmd_start(cwd: &Path, quest_id: Option<&str>) -> Result<()> {
    let workspace = load_workspace(cwd)?;
    let store = QuestStore::new(default_store_path(&workspace.root));

    match quest_id {
        Some(id) => {
            // Find the quest — support prefix matching.
            let quests = store.list()?;
            let matching: Vec<_> = quests.iter().filter(|q| q.id.starts_with(id)).collect();

            match matching.len() {
                0 => {
                    color_eyre::eyre::bail!("no quest found matching {id:?}");
                }
                1 => {
                    let mut coord = Coordinator::new(workspace, store);
                    let quest = coord.start_quest(&matching[0].id).await?;
                    println!("Started quest: {}", quest.summary());
                }
                _ => {
                    eprintln!("Multiple quests match {id:?}:");
                    for q in &matching {
                        eprintln!("  {}", q.summary());
                    }
                    color_eyre::eyre::bail!("ambiguous quest ID prefix");
                }
            }
        }
        None => {
            // List available quests.
            let quests = store.list()?;
            if quests.is_empty() {
                println!("No quests. Run `hive plan \"description\"` to create one.");
            } else {
                println!("Available quests:");
                for quest in &quests {
                    println!("  {}", quest.summary());
                }
                println!("\nRun `hive start <quest-id>` to begin.");
            }
        }
    }

    Ok(())
}

/// Schedule a reminder.
fn cmd_remind(cwd: &Path, duration: Option<&str>, cron: Option<&str>, message: &str) -> Result<()> {
    let workspace = load_workspace(cwd)?;
    let mut store = reminder::ReminderStore::load(&workspace.root);

    let r = if let Some(cron_expr) = cron {
        reminder::create_cron(cron_expr, message, None)
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?
    } else if let Some(dur) = duration {
        reminder::create_oneshot(dur, message, None).map_err(|e| color_eyre::eyre::eyre!("{e}"))?
    } else {
        color_eyre::eyre::bail!("either <duration> or --cron is required");
    };

    let fire_str = r
        .fire_at
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".into());
    let short_id = r.id[..8.min(r.id.len())].to_owned();
    let kind = r.kind.to_string();

    store.add(r);
    store.save()?;

    eprintln!("[reminder] Created {kind} reminder {short_id}: \"{message}\" fires at {fire_str}");
    println!("Reminder set (ID: {short_id})");
    println!("Next fire: {fire_str}");
    println!("Message: {message}");

    Ok(())
}

/// List all pending reminders.
fn cmd_list_reminders(cwd: &Path) -> Result<()> {
    let workspace = load_workspace(cwd)?;
    let store = reminder::ReminderStore::load(&workspace.root);

    let active = store.active();
    eprintln!("[reminder] Listed {} active reminder(s)", active.len());
    println!("{}", reminder::format_reminder_list(&active));

    Ok(())
}

/// Verify a worktree against its TASK.md acceptance criteria.
async fn cmd_verify(cwd: &Path, worktree_id: &str) -> Result<()> {
    let worktree_path = pipeline::verify::resolve_worktree_path(cwd, worktree_id)?;

    eprintln!(
        "[verify] Verifying worktree {} at {}",
        worktree_id,
        worktree_path.display()
    );

    let result = pipeline::verify::verify(&worktree_path).await?;

    for c in &result.criteria {
        let status = if c.passed { "PASS" } else { "FAIL" };
        println!("[{status}] {}", c.criterion);
        if !c.evidence.is_empty() {
            println!("       {}", c.evidence);
        }
    }

    println!();
    if result.passed {
        println!("Result: ALL CRITERIA PASSED");
    } else {
        let failed = result.criteria.iter().filter(|c| !c.passed).count();
        println!(
            "Result: {} of {} criteria FAILED",
            failed,
            result.criteria.len()
        );
    }

    if !result.summary.is_empty() {
        println!("\nSummary: {}", result.summary);
    }

    Ok(())
}

/// Run the full task pipeline.
async fn cmd_dispatch(
    cwd: &Path,
    description: &str,
    repo: Option<&str>,
    until: Option<&str>,
    profile: &str,
    dry_run: bool,
) -> Result<()> {
    let workspace = load_workspace(cwd)?;

    let until_stage = if let Some(s) = until {
        Some(pipeline::PipelineStage::from_str(s).ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "unknown pipeline stage: {s:?} (options: refine, context, plan)"
            )
        })?)
    } else {
        None
    };

    let result = pipeline::run_pipeline(pipeline::PipelineOptions {
        raw_prompt: description,
        workspace_root: &workspace.root,
        conventions: workspace.conventions.as_deref(),
        repo,
        profile,
        until: until_stage,
        dry_run,
    })
    .await?;

    eprintln!(
        "[dispatch] Pipeline completed at stage: {}",
        result.stage_reached
    );

    // Print artifacts if dry-run or stopped early
    if dry_run || result.worktree_id.is_none() {
        println!("--- TASK.md ---");
        println!("{}", result.task_md);
        if let Some(ctx) = &result.context_md {
            println!("\n--- CONTEXT.md ---");
            println!("{ctx}");
        }
        if let Some(plan) = &result.plan_md {
            println!("\n--- PLAN.md ---");
            println!("{plan}");
        }
    }

    if let Some(wt_id) = &result.worktree_id {
        println!("{wt_id}");
    }

    Ok(())
}

/// Identify relevant codebase context for a task.
async fn cmd_context(cwd: &Path, input: &str, output: Option<&Path>) -> Result<()> {
    // If input looks like a file path, read it; otherwise treat as raw description
    let task_md = if Path::new(input).is_file() {
        std::fs::read_to_string(input)?
    } else {
        input.to_string()
    };

    let workspace = load_workspace(cwd).ok();
    let repo_path = workspace.as_ref().map(|w| w.root.as_path());

    eprintln!("[context] Identifying context...");
    let result = pipeline::context::identify_context(&task_md, repo_path).await?;
    eprintln!(
        "[context] Found {} relevant files",
        result.relevant_files.len()
    );

    match output {
        Some(path) => {
            std::fs::write(path, &result.context_md)?;
            eprintln!("[context] Written to {}", path.display());
        }
        None => {
            println!("{}", result.context_md);
        }
    }

    Ok(())
}

/// Refine a raw prompt into a structured TASK.md.
async fn cmd_refine(cwd: &Path, description: &str, output: Option<&Path>) -> Result<()> {
    let workspace = load_workspace(cwd).ok();
    let conventions = workspace.as_ref().and_then(|w| w.conventions.as_deref());

    eprintln!("[refine] Refining: {description:?}");
    let result = pipeline::refine::refine(description, conventions).await?;
    eprintln!("[refine] Title: {}", result.title);
    eprintln!(
        "[refine] Acceptance criteria: {}",
        result.acceptance_criteria.len()
    );

    match output {
        Some(path) => {
            std::fs::write(path, &result.task_md)?;
            eprintln!("[refine] Written to {}", path.display());
        }
        None => {
            println!("{}", result.task_md);
        }
    }

    Ok(())
}

/// Cancel a reminder by ID prefix.
fn cmd_cancel_reminder(cwd: &Path, id_prefix: &str) -> Result<()> {
    let workspace = load_workspace(cwd)?;
    let mut store = reminder::ReminderStore::load(&workspace.root);

    match store.cancel(id_prefix) {
        Ok(id) => {
            store.save()?;
            let short = &id[..8.min(id.len())];
            eprintln!("[reminder] Cancelled reminder {short}");
            println!("Cancelled reminder {short}.");
        }
        Err(reminder::CancelError::NotFound) => {
            color_eyre::eyre::bail!("no reminder found matching \"{id_prefix}\"");
        }
        Err(reminder::CancelError::Ambiguous(ids)) => {
            eprintln!("Multiple reminders match \"{id_prefix}\":");
            for id in &ids {
                eprintln!("  {}", &id[..8.min(id.len())]);
            }
            color_eyre::eyre::bail!("ambiguous reminder ID prefix");
        }
    }

    Ok(())
}
