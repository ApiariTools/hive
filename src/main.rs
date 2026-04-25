use clap::Parser;
use color_eyre::Result;
use std::net::SocketAddr;
use tracing::info;

mod db;
mod routes;

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

    let app = routes::router(db, &config_dir);

    let addr = SocketAddr::from(([0, 0, 0, 0], cli.port));
    info!("hive listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn dirs_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

mod dirs {
    pub fn home_dir() -> Option<std::path::PathBuf> {
        super::dirs_home_dir()
    }
}
