//! Hive — the orchestration brain of Apiari.
//!
//! Hive uses `apiari-claude-sdk` to run Claude agent sessions (coordinator +
//! workers) and calls the `swarm` CLI for execution in isolated worktrees.

mod buzz;
mod channel;
mod coordinator;
mod daemon;
#[allow(dead_code)]
mod github;
mod keeper;
mod quest;
pub(crate) mod signal;
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
    Status,

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

        /// Path to config file.
        #[arg(long, default_value = ".buzz/config.toml")]
        config: PathBuf,

        /// Output mode: stdout, file, webhook.
        #[arg(long)]
        output: Option<String>,
    },

    /// Launch the read-only dashboard TUI.
    Dashboard {
        /// Print status once and exit (no TUI).
        #[arg(long)]
        once: bool,
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
        Command::Status => cmd_status(&cwd),
        Command::Chat => cmd_chat(&cwd).await,
        Command::Plan { description } => cmd_plan(&cwd, &description).await,
        Command::Start { quest_id } => cmd_start(&cwd, quest_id.as_deref()).await,
        Command::Daemon { action } => match action {
            DaemonAction::Start { foreground } => daemon::start(&cwd, foreground).await,
            DaemonAction::Stop => daemon::stop(&cwd),
            DaemonAction::Restart => {
                let _ = daemon::stop(&cwd);
                daemon::start(&cwd, false).await
            }
        },
        Command::Buzz {
            daemon,
            once,
            config,
            output,
        } => buzz::run(&config, daemon, once, output.as_deref()).await,
        Command::Dashboard { once } => keeper::run(once).await,
    }
}

/// Initialize a new workspace.
fn cmd_init(cwd: &Path) -> Result<()> {
    let config_path = init_workspace(cwd)?;
    println!("Workspace initialized: {}", config_path.display());
    println!("Edit {} to configure your workspace.", config_path.display());
    Ok(())
}

/// Show workspace and quest status.
fn cmd_status(cwd: &Path) -> Result<()> {
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
            let matching: Vec<_> = quests
                .iter()
                .filter(|q| q.id.starts_with(id))
                .collect();

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
