use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tower_http::cors::CorsLayer;

use crate::db::Db;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub config_dir: PathBuf,
}

pub fn router(db: Db, config_dir: &std::path::Path) -> Router {
    let state = AppState {
        db,
        config_dir: config_dir.to_path_buf(),
    };

    Router::new()
        .route("/api/workspaces", get(list_workspaces))
        .route("/api/workspaces/{workspace}/bots", get(list_bots))
        .route(
            "/api/workspaces/{workspace}/conversations",
            get(get_conversations),
        )
        .route(
            "/api/workspaces/{workspace}/conversations/{bot}",
            get(get_bot_conversations),
        )
        .route(
            "/api/workspaces/{workspace}/chat/{bot}",
            post(send_message),
        )
        .route("/api/workspaces/{workspace}/workers", get(list_workers))
        .fallback(get(serve_frontend))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ── Workspaces ──

async fn list_workspaces(State(state): State<AppState>) -> Json<Vec<WorkspaceInfo>> {
    let workspaces_dir = state.config_dir.join("workspaces");
    let mut workspaces = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&workspaces_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml") {
                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                    workspaces.push(WorkspaceInfo {
                        name: name.to_string(),
                    });
                }
            }
        }
    }

    workspaces.sort_by(|a, b| a.name.cmp(&b.name));
    Json(workspaces)
}

#[derive(Serialize)]
struct WorkspaceInfo {
    name: String,
}

// ── Bots ──

async fn list_bots(
    State(state): State<AppState>,
    Path(workspace): Path<String>,
) -> Json<Vec<BotInfo>> {
    let config_path = state.config_dir.join("workspaces").join(format!("{workspace}.toml"));
    let bots = load_bots_from_config(&config_path);
    Json(bots)
}

#[derive(Serialize, Deserialize, Clone)]
struct BotInfo {
    name: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    watch: Vec<String>,
}

fn load_bots_from_config(path: &std::path::Path) -> Vec<BotInfo> {
    // Always include Main bot
    let mut bots = vec![BotInfo {
        name: "Main".to_string(),
        color: Some("#f5c542".to_string()),
        role: Some("Workspace assistant".to_string()),
        watch: vec![],
    }];

    if let Ok(content) = std::fs::read_to_string(path) {
        if let Ok(config) = toml::from_str::<WorkspaceConfig>(&content) {
            bots.extend(config.bots.unwrap_or_default());
        }
    }

    bots
}

#[derive(Deserialize)]
struct WorkspaceConfig {
    bots: Option<Vec<BotInfo>>,
}

// ── Conversations ──

async fn get_conversations(
    State(state): State<AppState>,
    Path(workspace): Path<String>,
    Query(params): Query<ConvQuery>,
) -> Json<Vec<crate::db::MessageRow>> {
    let limit = params.limit.unwrap_or(100);
    let rows = state
        .db
        .get_all_conversations(&workspace, limit)
        .unwrap_or_default();
    Json(rows)
}

async fn get_bot_conversations(
    State(state): State<AppState>,
    Path((workspace, bot)): Path<(String, String)>,
    Query(params): Query<ConvQuery>,
) -> Json<Vec<crate::db::MessageRow>> {
    let limit = params.limit.unwrap_or(100);
    let rows = state
        .db
        .get_conversations(&workspace, &bot, limit)
        .unwrap_or_default();
    Json(rows)
}

#[derive(Deserialize)]
struct ConvQuery {
    limit: Option<i64>,
}

// ── Chat ──

async fn send_message(
    State(state): State<AppState>,
    Path((workspace, bot)): Path<(String, String)>,
    Json(body): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, StatusCode> {
    // Store user message
    state
        .db
        .add_message(&workspace, &bot, "user", &body.message, None)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // TODO: Send to Claude and stream response
    // For now, echo back a placeholder
    let reply = format!("(hive is not wired to a model yet — you said: {})", body.message);
    state
        .db
        .add_message(&workspace, &bot, "assistant", &reply, None)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ChatResponse { reply }))
}

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
}

#[derive(Serialize)]
struct ChatResponse {
    reply: String,
}

// ── Workers ──

async fn list_workers(
    State(state): State<AppState>,
    Path(workspace): Path<String>,
) -> Json<Vec<WorkerInfo>> {
    // Read from swarm state.json if workspace has a root configured
    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));

    let root = load_workspace_root(&config_path);
    let workers = match root {
        Some(root) => read_swarm_workers(&root),
        None => vec![],
    };

    Json(workers)
}

#[derive(Serialize)]
struct WorkerInfo {
    id: String,
    branch: String,
    status: String,
    agent: String,
    pr_url: Option<String>,
    pr_title: Option<String>,
    description: Option<String>,
    elapsed_secs: Option<u64>,
    dispatched_by: Option<String>,
}

fn load_workspace_root(path: &std::path::Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(path).ok()?;
    let config: toml::Value = toml::from_str(&content).ok()?;
    config
        .get("workspace")?
        .get("root")?
        .as_str()
        .map(PathBuf::from)
}

fn read_swarm_workers(root: &std::path::Path) -> Vec<WorkerInfo> {
    let state_path = root.join(".swarm/state.json");
    let content = match std::fs::read_to_string(&state_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let state: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let workers = match state.get("workers").and_then(|w| w.as_array()) {
        Some(w) => w,
        None => return vec![],
    };

    workers
        .iter()
        .filter_map(|w| {
            let id = w.get("id")?.as_str()?.to_string();
            let branch = w
                .get("branch")
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .to_string();
            let phase = w
                .get("phase")
                .and_then(|p| p.as_str())
                .unwrap_or("unknown")
                .to_string();
            let agent = w
                .get("agent")
                .and_then(|a| a.as_str())
                .unwrap_or("claude")
                .to_string();
            let pr_url = w
                .get("pr")
                .and_then(|p| p.get("url"))
                .and_then(|u| u.as_str())
                .map(|s| s.to_string());
            let pr_title = w
                .get("pr")
                .and_then(|p| p.get("title"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());

            Some(WorkerInfo {
                id,
                branch,
                status: phase,
                agent,
                pr_url,
                pr_title,
                description: None,
                elapsed_secs: None,
                dispatched_by: None,
            })
        })
        .collect()
}

// ── Frontend ──

// In dev, proxy to Vite. In release, serve embedded files.
async fn serve_frontend(
    _uri: axum::http::Uri,
) -> Result<axum::response::Html<String>, StatusCode> {
    // For now, serve a simple redirect to the dev server
    // In production this would use rust-embed
    let html = include_str!("../web/index.html");
    Ok(axum::response::Html(html.to_string()))
}
