use clap::{Parser, Subcommand};
use color_eyre::Result;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::info;

mod config_watcher;
mod db;
mod events;
mod pr_feedback;
mod pr_review;
mod publish;
mod routes;
mod tick;
mod watcher;

#[derive(Parser)]
#[command(name = "hive", about = "Workspace command hub")]
struct Cli {
    /// Port to serve on
    #[arg(long, default_value = "4200")]
    port: u16,

    /// Config directory (default: ~/.config/hive)
    #[arg(long)]
    config_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Publish a report from a specialty bot
    Publish(publish::PublishArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hive=info".into()),
        )
        .init();

    // Strip sandbox GH_TOKEN if running inside Claude Code
    if std::env::var("CLAUDECODE").is_ok() {
        info!("stripping sandbox GH_TOKEN");
        unsafe {
            std::env::remove_var("GH_TOKEN");
        }
    }

    let cli = Cli::parse();

    let config_dir = cli
        .config_dir
        .clone()
        .unwrap_or_else(|| dirs::home_dir().unwrap().join(".config/hive"));
    std::fs::create_dir_all(&config_dir)?;

    // Handle subcommands before daemon startup
    if let Some(command) = cli.command {
        match command {
            Command::Publish(args) => {
                let db_path = config_dir.join("hive.db");
                return publish::run(args, &db_path);
            }
        }
    }

    let db_path = config_dir.join("hive.db");
    let db = db::Db::open(&db_path)?;

    // Build unified tick engine
    let watched_bots = load_watched_bots(&config_dir);
    let watched_workspaces = load_watched_workspaces(&config_dir);
    let pr_review_cache: pr_review::PrReviewCache = Default::default();
    let ws_roots = load_workspace_roots(&config_dir);

    let mut engine = tick::TickEngine::new(15);

    if !watched_bots.is_empty() {
        info!("starting {} specialty bot watcher(s)", watched_bots.len());
        engine.add_watcher(Box::new(tick::SignalWatcher::new(watched_bots.clone())));
        engine.add_watcher(Box::new(tick::ScheduleWatcher::new(watched_bots)));
    }

    if !watched_workspaces.is_empty() {
        info!(
            "[config-watcher] watching {} workspace(s) for prompt changes",
            watched_workspaces.len()
        );
        engine.add_watcher(Box::new(tick::ConfigChangeWatcher::new(watched_workspaces)));
    }

    if !ws_roots.is_empty() {
        info!(
            "starting PR review poller for {} workspace(s)",
            ws_roots.len()
        );
        engine.add_watcher(Box::new(tick::PrReviewWatcher::new(
            pr_review_cache.clone(),
            ws_roots.clone(),
        )));

        let hive_dir = config_dir.join(".hive");
        std::fs::create_dir_all(&hive_dir).ok();
        engine.add_watcher(Box::new(pr_feedback::PrFeedbackWatcher::new(
            ws_roots,
            hive_dir.join("pr_feedback.json"),
            3,
            pr_review_cache.clone(),
        )));
    }

    tokio::spawn(engine.run(db.clone()));

    let event_hub = events::EventHub::new();
    let app = routes::router(db, &config_dir, event_hub, pr_review_cache);

    let addr = SocketAddr::from(([0, 0, 0, 0], cli.port));
    info!("hive listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Scan all workspace configs and collect bots with watch sources.
fn load_watched_bots(config_dir: &std::path::Path) -> Vec<watcher::WatchedBot> {
    let workspaces_dir = config_dir.join("workspaces");
    let mut watched = Vec::new();

    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(e) => e,
        Err(_) => return watched,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml") {
            let workspace = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let config: toml::Value = match toml::from_str(&content) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let working_dir = config
                .get("workspace")
                .and_then(|w| w.get("root"))
                .and_then(|r| r.as_str())
                .map(PathBuf::from);

            if let Some(bots) = config.get("bots").and_then(|b| b.as_array()) {
                for bot in bots {
                    let name = bot
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let watch: Vec<String> = bot
                        .get("watch")
                        .and_then(|w| w.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

                    let schedule_hours = bot
                        .get("schedule_hours")
                        .and_then(|s| s.as_integer())
                        .map(|s| s as u64);
                    let proactive_prompt = bot
                        .get("proactive_prompt")
                        .and_then(|p| p.as_str())
                        .map(String::from);

                    let services: Vec<String> = bot
                        .get("services")
                        .and_then(|s| s.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

                    let has_watch = !watch.is_empty();
                    let has_schedule = schedule_hours.is_some() && proactive_prompt.is_some();

                    if has_watch || has_schedule {
                        watched.push(watcher::WatchedBot {
                            workspace: workspace.clone(),
                            name,
                            provider: bot
                                .get("provider")
                                .and_then(|p| p.as_str())
                                .unwrap_or("claude")
                                .to_string(),
                            model: bot.get("model").and_then(|m| m.as_str()).map(String::from),
                            role: bot
                                .get("role")
                                .and_then(|r| r.as_str())
                                .unwrap_or("")
                                .to_string(),
                            watch,
                            working_dir: working_dir.clone(),
                            schedule_hours,
                            proactive_prompt,
                            services,
                        });
                    }
                }
            }
        }
    }

    watched
}

fn load_watched_workspaces(config_dir: &std::path::Path) -> Vec<config_watcher::WatchedWorkspace> {
    let workspaces_dir = config_dir.join("workspaces");
    let mut watched = Vec::new();

    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(e) => e,
        Err(_) => return watched,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml") {
            let ws_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let config: toml::Value = match toml::from_str(&content) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let root = config
                .get("workspace")
                .and_then(|w| w.get("root"))
                .and_then(|r| r.as_str())
                .map(PathBuf::from);

            // Collect bot names (always include Main)
            let mut bots = vec!["Main".to_string()];
            if let Some(bot_arr) = config.get("bots").and_then(|b| b.as_array()) {
                for bot in bot_arr {
                    if let Some(name) = bot.get("name").and_then(|n| n.as_str()) {
                        bots.push(name.to_string());
                    }
                }
            }

            watched.push(config_watcher::WatchedWorkspace {
                name: ws_name,
                config_path: path,
                root,
                bots,
            });
        }
    }

    watched
}

fn load_workspace_roots(config_dir: &std::path::Path) -> Vec<PathBuf> {
    let workspaces_dir = config_dir.join("workspaces");
    let mut roots = Vec::new();

    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(e) => e,
        Err(_) => return roots,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml")
            && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(config) = toml::from_str::<toml::Value>(&content)
            && let Some(root) = config
                .get("workspace")
                .and_then(|w| w.get("root"))
                .and_then(|r| r.as_str())
        {
            let root_path = PathBuf::from(root);
            if root_path.join(".swarm").exists() && !roots.contains(&root_path) {
                roots.push(root_path);
            }
        }
    }

    roots
}

fn dirs_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

mod dirs {
    pub fn home_dir() -> Option<std::path::PathBuf> {
        super::dirs_home_dir()
    }
}
