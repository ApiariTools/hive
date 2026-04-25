use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions, streaming::AssembledEvent, types::ContentBlock};
use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Json, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tower_http::cors::CorsLayer;
use tracing::info;

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
    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));
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

#[derive(Deserialize, Default)]
struct WorkspaceConfig {
    workspace: Option<WorkspaceInfo_>,
    bots: Option<Vec<BotInfo>>,
}

#[derive(Deserialize, Default, Clone)]
struct WorkspaceInfo_ {
    root: Option<String>,
    name: Option<String>,
    description: Option<String>,
}

fn load_workspace_config(path: &std::path::Path) -> WorkspaceConfig {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| toml::from_str(&c).ok())
        .unwrap_or_default()
}

fn build_system_prompt(ws_config: &WorkspaceConfig, bot_name: &str) -> String {
    let ws = ws_config.workspace.clone().unwrap_or_default();
    let ws_name = ws.name.as_deref().unwrap_or("unknown");
    let ws_desc = ws.description.as_deref().unwrap_or("");

    // Find this bot's role
    let bot_role = ws_config
        .bots
        .as_ref()
        .and_then(|bots| bots.iter().find(|b| b.name == bot_name))
        .and_then(|b| b.role.as_deref())
        .unwrap_or("Workspace assistant");

    let mut prompt = format!(
        "You are {bot_name}, a bot in the \"{ws_name}\" workspace.\n\
         Workspace: {ws_desc}\n\
         Your role: {bot_role}\n\n\
         Be concise and helpful. You have access to the workspace's codebase.\n"
    );

    // Add workspace root context
    if let Some(ref root) = ws.root {
        prompt.push_str(&format!("Working directory: {root}\n"));
    }

    prompt
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

// ── Chat (SSE streaming via apiari-claude-sdk) ──

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
}

async fn send_message(
    State(state): State<AppState>,
    Path((workspace, bot)): Path<(String, String)>,
    Json(body): Json<ChatRequest>,
) -> Response {
    // Store user message
    if let Err(e) = state
        .db
        .add_message(&workspace, &bot, "user", &body.message, None)
    {
        return sse_error(&format!("DB error: {e}"));
    }

    info!("[chat] {workspace}/{bot}: {}", body.message);

    // Load workspace config
    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));
    let ws_config = load_workspace_config(&config_path);
    let working_dir = ws_config
        .workspace
        .as_ref()
        .and_then(|w| w.root.as_ref())
        .map(PathBuf::from);

    // Check for existing session to resume
    let resume_id = state.db.get_session_id(&workspace, &bot).unwrap_or(None);
    if let Some(ref id) = resume_id {
        info!("[chat] resuming session {id}");
    }

    // Build system prompt from workspace config
    let system_prompt = if resume_id.is_none() {
        Some(build_system_prompt(&ws_config, &bot))
    } else {
        None // Don't re-send system prompt on resume
    };

    // Build session options
    let opts = SessionOptions {
        dangerously_skip_permissions: true,
        include_partial_messages: true,
        working_dir,
        max_turns: Some(50),
        resume: resume_id,
        system_prompt,
        ..Default::default()
    };

    // Spawn claude session via SDK
    let client = ClaudeClient::new();
    let mut session = match client.spawn(opts).await {
        Ok(s) => s,
        Err(e) => return sse_error(&format!("Failed to start claude: {e}")),
    };

    // Send the user's message
    if let Err(e) = session.send_message(&body.message).await {
        return sse_error(&format!("Failed to send message: {e}"));
    }

    // Stream events back as SSE
    let db = state.db.clone();
    let bot_name = bot.clone();
    let ws_name = workspace.clone();

    let stream = async_stream::stream! {
        let mut full_text = String::new();

        loop {
            match session.next_event().await {
                Ok(Some(event)) => {
                    match event {
                        Event::Stream { assembled, .. } => {
                            for asm in assembled {
                                match asm {
                                    AssembledEvent::TextDelta { text, .. } => {
                                        full_text.push_str(&text);
                                        let data = serde_json::json!({
                                            "type": "text",
                                            "content": text
                                        });
                                        yield Ok::<_, std::io::Error>(
                                            sse_event("data", &data.to_string())
                                        );
                                    }
                                    AssembledEvent::ContentBlockComplete { block, .. } => {
                                        if let ContentBlock::ToolUse { name, .. } = block {
                                            let data = serde_json::json!({
                                                "type": "tool_use",
                                                "tool": name,
                                                "status": "running"
                                            });
                                            yield Ok::<_, std::io::Error>(
                                                sse_event("data", &data.to_string())
                                            );
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Event::Assistant { message, .. } => {
                            // Fallback: if not streaming, extract text from full message
                            for block in &message.message.content {
                                if let ContentBlock::Text { text } = block {
                                    if !text.is_empty() && full_text.is_empty() {
                                        full_text.push_str(text);
                                        let data = serde_json::json!({
                                            "type": "text",
                                            "content": text
                                        });
                                        yield Ok::<_, std::io::Error>(
                                            sse_event("data", &data.to_string())
                                        );
                                    }
                                }
                            }
                        }
                        Event::Result(result) => {
                            // Store the full response and session ID
                            if !full_text.is_empty() {
                                let _ = db.add_message(
                                    &ws_name, &bot_name, "assistant", &full_text, None
                                );
                            }
                            let _ = db.set_session_id(
                                &ws_name, &bot_name, &result.session_id
                            );
                            let data = serde_json::json!({
                                "type": "done",
                                "content": full_text,
                                "session_id": result.session_id,
                                "cost": result.total_cost_usd,
                            });
                            yield Ok::<_, std::io::Error>(
                                sse_event("data", &data.to_string())
                            );
                            break;
                        }
                        _ => {} // System, User, RateLimit — skip
                    }
                }
                Ok(None) => {
                    // EOF — store whatever we have
                    if !full_text.is_empty() {
                        let _ = db.add_message(
                            &ws_name, &bot_name, "assistant", &full_text, None
                        );
                    }
                    let data = serde_json::json!({
                        "type": "done",
                        "content": full_text,
                    });
                    yield Ok::<_, std::io::Error>(
                        sse_event("data", &data.to_string())
                    );
                    break;
                }
                Err(e) => {
                    let data = serde_json::json!({
                        "type": "error",
                        "content": format!("SDK error: {e}"),
                    });
                    yield Ok::<_, std::io::Error>(
                        sse_event("data", &data.to_string())
                    );
                    break;
                }
            }
        }
    };

    Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn sse_event(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn sse_error(msg: &str) -> Response {
    let data = serde_json::json!({"type": "error", "content": msg});
    Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(Body::from(format!("event: data\ndata: {}\n\n", data)))
        .unwrap()
}

// ── Workers ──

async fn list_workers(
    State(state): State<AppState>,
    Path(workspace): Path<String>,
) -> Json<Vec<WorkerInfo>> {
    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));

    let ws_config = load_workspace_config(&config_path);
    let root = ws_config
        .workspace
        .as_ref()
        .and_then(|w| w.root.as_ref())
        .map(PathBuf::from);
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

async fn serve_frontend(
    _uri: axum::http::Uri,
) -> Result<axum::response::Html<String>, StatusCode> {
    let html = include_str!("../web/index.html");
    Ok(axum::response::Html(html.to_string()))
}
