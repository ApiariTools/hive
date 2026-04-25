use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions, streaming::AssembledEvent, types::ContentBlock};
use apiari_codex_sdk;
use apiari_gemini_sdk;
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
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    watch: Vec<String>,
}

fn default_provider() -> String {
    "claude".to_string()
}

fn load_bots_from_config(path: &std::path::Path) -> Vec<BotInfo> {
    let mut bots = vec![BotInfo {
        name: "Main".to_string(),
        color: Some("#f5c542".to_string()),
        role: Some("Workspace assistant".to_string()),
        provider: default_provider(),
        model: None,
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
    #[serde(default)]
    attachments: Option<Vec<ChatAttachment>>,
}

#[derive(Deserialize, Serialize, Clone)]
struct ChatAttachment {
    name: String,
    #[serde(rename = "type")]
    mime_type: String,
    #[serde(rename = "dataUrl")]
    data_url: String,
}

async fn send_message(
    State(state): State<AppState>,
    Path((workspace, bot)): Path<(String, String)>,
    Json(body): Json<ChatRequest>,
) -> Response {
    // Store user message with attachments
    let att_json = body
        .attachments
        .as_ref()
        .and_then(|a| serde_json::to_string(a).ok());
    if let Err(e) = state.db.add_message(
        &workspace,
        &bot,
        "user",
        &body.message,
        att_json.as_deref(),
    ) {
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

    // Find this bot's provider
    let bot_config = ws_config
        .bots
        .as_ref()
        .and_then(|bots| bots.iter().find(|b| b.name == bot).cloned());
    let provider = bot_config
        .as_ref()
        .map(|b| b.provider.as_str())
        .unwrap_or("claude");

    // Check for existing session to resume
    let resume_id = state.db.get_session_id(&workspace, &bot).unwrap_or(None);
    if let Some(ref id) = resume_id {
        info!("[chat] resuming session {id} (provider={provider})");
    }

    let system_prompt = if resume_id.is_none() {
        Some(build_system_prompt(&ws_config, &bot))
    } else {
        None
    };

    let images = extract_images(&body.attachments);

    let db = state.db.clone();
    let bot_name = bot.clone();
    let ws_name = workspace.clone();

    match provider {
        "codex" => stream_codex(body.message, system_prompt, working_dir, resume_id, db, ws_name, bot_name),
        "gemini" => stream_gemini(body.message, system_prompt, working_dir, resume_id, db, ws_name, bot_name),
        _ => stream_claude(body.message, system_prompt, working_dir, resume_id, images, db, ws_name, bot_name),
    }
}

fn extract_images(attachments: &Option<Vec<ChatAttachment>>) -> Vec<(String, String)> {
    attachments
        .as_ref()
        .map(|atts| {
            atts.iter()
                .filter(|a| a.mime_type.starts_with("image/"))
                .filter_map(|a| {
                    let parts: Vec<&str> = a.data_url.splitn(2, ',').collect();
                    if parts.len() == 2 {
                        Some((a.mime_type.clone(), parts[1].to_string()))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn stream_claude(
    message: String,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    resume_id: Option<String>,
    images: Vec<(String, String)>,
    db: Db,
    ws_name: String,
    bot_name: String,
) -> Response {
    let stream = async_stream::stream! {
        let opts = SessionOptions {
            dangerously_skip_permissions: true,
            include_partial_messages: true,
            working_dir,
            max_turns: Some(50),
            resume: resume_id,
            system_prompt,
            ..Default::default()
        };

        let client = ClaudeClient::new();
        let mut session = match client.spawn(opts).await {
            Ok(s) => s,
            Err(e) => {
                yield Ok::<_, std::io::Error>(sse_event("data",
                    &serde_json::json!({"type":"error","content":format!("Failed to start claude: {e}")}).to_string()));
                return;
            }
        };

        let send_result = if images.is_empty() {
            session.send_message(&message).await
        } else {
            session.send_message_with_images(&message, images).await
        };
        if let Err(e) = send_result {
            yield Ok::<_, std::io::Error>(sse_event("data",
                &serde_json::json!({"type":"error","content":format!("Failed to send: {e}")}).to_string()));
            return;
        }

        let mut full_text = String::new();
        loop {
            match session.next_event().await {
                Ok(Some(event)) => match event {
                    Event::Stream { assembled, .. } => {
                        for asm in assembled {
                            match asm {
                                AssembledEvent::TextDelta { text, .. } => {
                                    full_text.push_str(&text);
                                    let data = serde_json::json!({"type":"text","content":text});
                                    yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                                }
                                AssembledEvent::ContentBlockComplete { block, .. } => {
                                    if let ContentBlock::ToolUse { name, .. } = block {
                                        let data = serde_json::json!({"type":"tool_use","tool":name,"status":"running"});
                                        yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Event::Assistant { message, .. } => {
                        for block in &message.message.content {
                            if let ContentBlock::Text { text } = block {
                                if !text.is_empty() && full_text.is_empty() {
                                    full_text.push_str(text);
                                    let data = serde_json::json!({"type":"text","content":text});
                                    yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                                }
                            }
                        }
                    }
                    Event::Result(result) => {
                        if !full_text.is_empty() {
                            let _ = db.add_message(&ws_name, &bot_name, "assistant", &full_text, None);
                        }
                        let _ = db.set_session_id(&ws_name, &bot_name, &result.session_id);
                        let data = serde_json::json!({"type":"done","content":full_text,"session_id":result.session_id});
                        yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                        break;
                    }
                    _ => {}
                },
                Ok(None) => {
                    if !full_text.is_empty() {
                        let _ = db.add_message(&ws_name, &bot_name, "assistant", &full_text, None);
                    }
                    let data = serde_json::json!({"type":"done","content":full_text});
                    yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                    break;
                }
                Err(e) => {
                    let data = serde_json::json!({"type":"error","content":format!("SDK error: {e}")});
                    yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                    break;
                }
            }
        }
    };

    sse_response(stream)
}

fn stream_codex(
    message: String,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    resume_id: Option<String>,
    db: Db,
    ws_name: String,
    bot_name: String,
) -> Response {
    let stream = async_stream::stream! {
        let client = apiari_codex_sdk::CodexClient::new();
        let prompt = if let Some(sys) = system_prompt {
            format!("{sys}\n\n---\n\n{message}")
        } else {
            message
        };

        let mut execution = if let Some(ref sid) = resume_id {
            match client.exec_resume(&prompt, apiari_codex_sdk::ResumeOptions {
                session_id: Some(sid.clone()),
                full_auto: true,
                working_dir,
                ..Default::default()
            }).await {
                Ok(e) => e,
                Err(e) => {
                    yield Ok::<_, std::io::Error>(sse_event("data",
                        &serde_json::json!({"type":"error","content":format!("Codex error: {e}")}).to_string()));
                    return;
                }
            }
        } else {
            match client.exec(&prompt, apiari_codex_sdk::ExecOptions {
                full_auto: true,
                working_dir,
                ..Default::default()
            }).await {
                Ok(e) => e,
                Err(e) => {
                    yield Ok::<_, std::io::Error>(sse_event("data",
                        &serde_json::json!({"type":"error","content":format!("Codex error: {e}")}).to_string()));
                    return;
                }
            }
        };

        let mut full_text = String::new();
        while let Ok(Some(event)) = execution.next_event().await {
            match &event {
                apiari_codex_sdk::Event::ThreadStarted { thread_id } => {
                    let _ = db.set_session_id(&ws_name, &bot_name, thread_id);
                }
                apiari_codex_sdk::Event::ItemCompleted { item } => {
                    if let Some(text) = item.text() {
                        if !text.is_empty() {
                            full_text = text.to_string();
                            let data = serde_json::json!({"type":"text","content":text});
                            yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                        }
                    }
                }
                apiari_codex_sdk::Event::TurnFailed { error, .. } => {
                    let msg = error.as_ref().and_then(|e| e.message.as_deref()).unwrap_or("codex failed");
                    let data = serde_json::json!({"type":"error","content":msg});
                    yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                }
                apiari_codex_sdk::Event::Error { message } => {
                    let msg = message.as_deref().unwrap_or("codex error");
                    let data = serde_json::json!({"type":"error","content":msg});
                    yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                }
                _ => {}
            }
        }

        if !full_text.is_empty() {
            let _ = db.add_message(&ws_name, &bot_name, "assistant", &full_text, None);
        }
        let data = serde_json::json!({"type":"done","content":full_text});
        yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
    };

    sse_response(stream)
}

fn stream_gemini(
    message: String,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    resume_id: Option<String>,
    db: Db,
    ws_name: String,
    bot_name: String,
) -> Response {
    let stream = async_stream::stream! {
        let client = apiari_gemini_sdk::GeminiClient::new();
        let prompt = if let Some(sys) = system_prompt {
            format!("{sys}\n\n---\n\n{message}")
        } else {
            message
        };

        let mut execution = if let Some(ref sid) = resume_id {
            match client.exec_resume(&prompt, apiari_gemini_sdk::SessionOptions {
                session_id: Some(sid.clone()),
                working_dir,
                ..Default::default()
            }).await {
                Ok(e) => e,
                Err(e) => {
                    yield Ok::<_, std::io::Error>(sse_event("data",
                        &serde_json::json!({"type":"error","content":format!("Gemini error: {e}")}).to_string()));
                    return;
                }
            }
        } else {
            match client.exec(&prompt, apiari_gemini_sdk::GeminiOptions {
                working_dir,
                ..Default::default()
            }).await {
                Ok(e) => e,
                Err(e) => {
                    yield Ok::<_, std::io::Error>(sse_event("data",
                        &serde_json::json!({"type":"error","content":format!("Gemini error: {e}")}).to_string()));
                    return;
                }
            }
        };

        let mut full_text = String::new();
        while let Ok(Some(event)) = execution.next_event().await {
            match &event {
                apiari_gemini_sdk::Event::ThreadStarted { thread_id } => {
                    let _ = db.set_session_id(&ws_name, &bot_name, thread_id);
                }
                apiari_gemini_sdk::Event::ItemCompleted { item } => {
                    if let Some(text) = item.text() {
                        if !text.is_empty() {
                            full_text = text.to_string();
                            let data = serde_json::json!({"type":"text","content":text});
                            yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                        }
                    }
                }
                apiari_gemini_sdk::Event::TurnFailed { error, .. } => {
                    let msg = error.as_ref().and_then(|e| e.message.as_deref()).unwrap_or("gemini failed");
                    let data = serde_json::json!({"type":"error","content":msg});
                    yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                }
                apiari_gemini_sdk::Event::Error { message } => {
                    let msg = message.as_deref().unwrap_or("gemini error");
                    let data = serde_json::json!({"type":"error","content":msg});
                    yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
                }
                _ => {}
            }
        }

        if !full_text.is_empty() {
            let _ = db.add_message(&ws_name, &bot_name, "assistant", &full_text, None);
        }
        let data = serde_json::json!({"type":"done","content":full_text});
        yield Ok::<_, std::io::Error>(sse_event("data", &data.to_string()));
    };

    sse_response(stream)
}

fn sse_response(stream: impl futures_core::Stream<Item = Result<String, std::io::Error>> + Send + 'static) -> Response {
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
