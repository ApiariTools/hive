use apiari_swarm::client::{DaemonRequest, DaemonResponse, send_daemon_request};
use clap::Subcommand;
use color_eyre::Result;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum SwarmCommand {
    /// Create a new swarm worker
    Create {
        /// Repository name (subdirectory under workspace root)
        #[arg(long)]
        repo: Option<String>,

        /// Agent to use (claude, codex, or gemini)
        #[arg(long, default_value = "claude")]
        agent: String,

        /// Task prompt text
        #[arg(long, conflicts_with = "prompt_file")]
        prompt: Option<String>,

        /// Path to a file containing the task prompt
        #[arg(long, conflicts_with = "prompt")]
        prompt_file: Option<PathBuf>,
    },
    /// Send a message to a running worker
    Send {
        /// Worker/worktree ID
        worktree_id: String,
        /// Message to send
        message: String,
    },
    /// Close (cancel/abandon) a worker
    Close {
        /// Worker/worktree ID
        worktree_id: String,
    },
    /// List current swarm workers
    Status,
}

pub fn run(dir: PathBuf, cmd: SwarmCommand) -> Result<()> {
    match cmd {
        SwarmCommand::Create {
            repo,
            agent,
            prompt,
            prompt_file,
        } => {
            let prompt_text = match (prompt, prompt_file) {
                (Some(text), _) => text,
                (_, Some(path)) => std::fs::read_to_string(&path)
                    .map_err(|e| color_eyre::eyre::eyre!("failed to read prompt file: {e}"))?,
                (None, None) => {
                    return Err(color_eyre::eyre::eyre!(
                        "either --prompt or --prompt-file is required"
                    ));
                }
            };

            let req = DaemonRequest::CreateWorker {
                prompt: prompt_text,
                agent,
                repo,
                start_point: None,
                workspace: Some(dir.clone()),
                profile: None,
                task_dir: None,
                role: None,
                review_pr: None,
                base_branch: None,
            };

            let resp = send_daemon_request(&dir, &req)?;
            check_response(&resp)?;
        }
        SwarmCommand::Send {
            worktree_id,
            message,
        } => {
            let req = DaemonRequest::SendMessage {
                worktree_id,
                message,
            };
            let resp = send_daemon_request(&dir, &req)?;
            check_response(&resp)?;
        }
        SwarmCommand::Close { worktree_id } => {
            let req = DaemonRequest::CloseWorker { worktree_id };
            let resp = send_daemon_request(&dir, &req)?;
            check_response(&resp)?;
        }
        SwarmCommand::Status => {
            let req = DaemonRequest::ListWorkers {
                workspace: Some(dir.clone()),
            };
            let resp = send_daemon_request(&dir, &req)?;
            match &resp {
                DaemonResponse::Workers { workers } => {
                    if workers.is_empty() {
                        println!("No active workers.");
                    } else {
                        for w in workers {
                            let pr = w
                                .pr_url
                                .as_deref()
                                .map(|u| format!(" PR: {u}"))
                                .unwrap_or_default();
                            println!(
                                "{id}  {phase:>10}  {branch}{pr}",
                                id = w.id,
                                phase = format!("{:?}", w.phase).to_lowercase(),
                                branch = w.branch,
                            );
                        }
                    }
                }
                _ => check_response(&resp)?,
            }
        }
    }

    Ok(())
}

fn check_response(resp: &DaemonResponse) -> Result<()> {
    match resp {
        DaemonResponse::Ok { data } => {
            if let Some(d) = data {
                println!("{}", serde_json::to_string_pretty(d).unwrap_or_default());
            } else {
                println!("ok");
            }
            Ok(())
        }
        DaemonResponse::Error { message } => Err(color_eyre::eyre::eyre!("{message}")),
        DaemonResponse::Workers { workers } => {
            println!(
                "{}",
                serde_json::to_string_pretty(workers).unwrap_or_default()
            );
            Ok(())
        }
        other => {
            println!(
                "{}",
                serde_json::to_string_pretty(other).unwrap_or_default()
            );
            Ok(())
        }
    }
}
