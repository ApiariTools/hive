use clap::Parser;
use color_eyre::Result;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::info;

mod db;
mod routes;
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

    let cli = Cli::parse();

    let config_dir = cli
        .config_dir
        .unwrap_or_else(|| dirs::home_dir().unwrap().join(".config/hive"));
    std::fs::create_dir_all(&config_dir)?;

    let db_path = config_dir.join("hive.db");
    let db = db::Db::open(&db_path)?;

    // Start signal watchers for specialty bots
    let watched_bots = load_watched_bots(&config_dir);
    if !watched_bots.is_empty() {
        info!("starting {} specialty bot watcher(s)", watched_bots.len());
        watcher::start_watchers(watched_bots, db.clone());
    }

    let app = routes::router(db, &config_dir);

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

                    if !watch.is_empty() {
                        watched.push(watcher::WatchedBot {
                            workspace: workspace.clone(),
                            name,
                            provider: bot
                                .get("provider")
                                .and_then(|p| p.as_str())
                                .unwrap_or("claude")
                                .to_string(),
                            model: bot
                                .get("model")
                                .and_then(|m| m.as_str())
                                .map(String::from),
                            role: bot
                                .get("role")
                                .and_then(|r| r.as_str())
                                .unwrap_or("")
                                .to_string(),
                            watch,
                            working_dir: working_dir.clone(),
                        });
                    }
                }
            }
        }
    }

    watched
}

fn dirs_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

mod dirs {
    pub fn home_dir() -> Option<std::path::PathBuf> {
        super::dirs_home_dir()
    }
}
