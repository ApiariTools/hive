use apiari_claude_sdk::{ClaudeClient, Event, SessionOptions, streaming::AssembledEvent, types::ContentBlock};
use apiari_codex_sdk;
use apiari_gemini_sdk;
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
        .route(
            "/api/workspaces/{workspace}/conversations/{bot}/search",
            get(search_conversations),
        )
        .route(
            "/api/workspaces/{workspace}/bots/{bot}/status",
            get(get_bot_status),
        )
        .route(
            "/api/workspaces/{workspace}/bots/{bot}/cancel",
            post(cancel_bot),
        )
        .route("/api/workspaces/{workspace}/workers", get(list_workers))
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}",
            get(get_worker_detail),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/send",
            post(send_worker_message),
        )
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
         Be concise. Short sentences. Use markdown formatting — bullets, bold, \
         code blocks — to make responses scannable. No walls of text. \
         Lead with the answer, explain after if needed.\n\
         If you're unsure, ask instead of guessing.\n"
    );

    if let Some(ref root) = ws.root {
        prompt.push_str(&format!("Working directory: {root}\n"));
        let root_path = std::path::Path::new(root);

        // Load .apiari/context.md if it exists
        let context_path = root_path.join(".apiari/context.md");
        if let Ok(context) = std::fs::read_to_string(&context_path) {
            prompt.push_str("\n## Project Context\n");
            prompt.push_str(&context);
            if !context.ends_with('\n') { prompt.push('\n'); }
        }

        // Load .apiari/soul.md if it exists
        let soul_path = root_path.join(".apiari/soul.md");
        if let Ok(soul) = std::fs::read_to_string(&soul_path) {
            prompt.push_str("\n## Communication Style\n");
            prompt.push_str(&soul);
            if !soul.ends_with('\n') { prompt.push('\n'); }
        }

        // Swarm worker dispatch instructions
        let has_swarm = root_path.join(".swarm").exists();
        if has_swarm {
            prompt.push_str(&format!(
                "\n## Swarm Workers\n\
                 You dispatch coding tasks to swarm workers. Workers run in their own git worktrees \
                 with an LLM agent that writes code, commits, and opens PRs.\n\n\
                 RULE: When the user asks you to implement, fix, build, or code anything, \
                 ALWAYS dispatch a swarm worker. Do NOT write code yourself — never use \
                 Edit, Write, or Bash to create/modify source code. Your job is to \
                 coordinate, not code. Just dispatch the worker immediately without asking.\n\n\
                 Commands (always use `--dir {root}`):\n\
                 - List workers: `swarm --dir {root} status`\n\
                 - Spawn worker: `swarm --dir {root} create --repo {{repo}} --prompt-file /tmp/task.txt`\n\
                   (Write the task prompt to a file first, then pass --prompt-file. Never inline long prompts.)\n\
                 - Send message: `swarm --dir {root} send {{worktree_id}} \"message\"`\n\
                 - Close worker: `swarm --dir {root} close {{worktree_id}}`\n\n\
                 When dispatching, always include in the task prompt:\n\
                 'Plan and implement this completely in one session — do not pause mid-task \
                 for confirmation. Commit and open a PR when done.'\n\n\
                 When a task spans multiple repos, dispatch separate workers for each.\n\
                 Each worker prompt must be self-contained — workers cannot see other repos.\n"
            ));
        }
    }

    // Chat history — bot can query the local DB directly
    prompt.push_str(&format!(
        "\n## Chat History\n\
         Your conversation history is stored in a local SQLite database.\n\
         To look up previous conversations:\n\
         - Recent messages: `sqlite3 ~/.config/hive/hive.db \"SELECT role, content FROM conversations WHERE workspace='{ws_name}' AND bot='{bot_name}' ORDER BY id DESC LIMIT 20\"`\n\
         - Search messages: `sqlite3 ~/.config/hive/hive.db \"SELECT role, content FROM conversations WHERE workspace='{ws_name}' AND bot='{bot_name}' AND content LIKE '%keyword%' ORDER BY id DESC LIMIT 10\"`\n\
         \n\
         Use this when the user references something from a previous conversation \
         or when you need context about what was discussed before.\n"
    ));

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

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<i64>,
}

async fn search_conversations(
    State(state): State<AppState>,
    Path((workspace, bot)): Path<(String, String)>,
    Query(params): Query<SearchQuery>,
) -> Json<Vec<crate::db::MessageRow>> {
    let limit = params.limit.unwrap_or(20);
    let rows = state
        .db
        .search_conversations(&workspace, &bot, &params.q, limit)
        .unwrap_or_default();
    Json(rows)
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
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Store user message with attachments
    let att_json = body
        .attachments
        .as_ref()
        .and_then(|a| serde_json::to_string(a).ok());
    state
        .db
        .add_message(&workspace, &bot, "user", &body.message, att_json.as_deref())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
        .map(|b| b.provider.clone())
        .unwrap_or_else(|| "claude".to_string());

    // Build system prompt and hash it — if prompt changed, start fresh session
    let full_prompt = build_system_prompt(&ws_config, &bot);
    let prompt_hash = simple_hash(&full_prompt);

    let resume_id = state
        .db
        .get_session_id(&workspace, &bot, &prompt_hash)
        .unwrap_or(None);
    if let Some(ref id) = resume_id {
        info!("[chat] resuming session {id} (provider={provider})");
    }

    let system_prompt = if resume_id.is_none() {
        Some(full_prompt)
    } else {
        None
    };

    let images = extract_images(&body.attachments);

    let text_attachments = extract_text_attachments(&body.attachments);
    let message = if text_attachments.is_empty() {
        body.message
    } else {
        let mut msg = body.message;
        msg.push_str("\n\n--- Attached files ---\n");
        for (name, content) in &text_attachments {
            msg.push_str(&format!("\n### {name}\n```\n{content}\n```\n"));
        }
        msg
    };

    let db = state.db.clone();
    let ws_name = workspace.clone();
    let bot_name = bot.clone();
    let hash = prompt_hash.clone();

    // Set bot status to thinking
    let _ = db.set_bot_status(&ws_name, &bot_name, "thinking", "", None);

    // Spawn background task — daemon owns the session
    tokio::spawn(async move {
        let result = match provider.as_str() {
            "codex" => run_bot_codex(message, system_prompt, working_dir, resume_id, &db, &ws_name, &bot_name, &hash).await,
            "gemini" => run_bot_gemini(message, system_prompt, working_dir, resume_id, &db, &ws_name, &bot_name, &hash).await,
            _ => run_bot_claude(message, system_prompt, working_dir, resume_id, images, &db, &ws_name, &bot_name, &hash).await,
        };

        if let Err(e) = result {
            let _ = db.add_message(&ws_name, &bot_name, "assistant", &format!("Error: {e}"), None);
        }

        let _ = db.set_bot_status(&ws_name, &bot_name, "idle", "", None);
    });

    Ok(Json(serde_json::json!({"ok": true})))
}

// ── Bot status endpoint ──

async fn get_bot_status(
    State(state): State<AppState>,
    Path((workspace, bot)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let status = state.db.get_bot_status(&workspace, &bot).unwrap_or(None);
    match status {
        Some(s) => Json(serde_json::json!({
            "status": s.status,
            "streaming_content": s.streaming_content,
            "tool_name": s.tool_name,
        })),
        None => Json(serde_json::json!({
            "status": "idle",
            "streaming_content": "",
            "tool_name": null,
        })),
    }
}

async fn cancel_bot(
    State(state): State<AppState>,
    Path((workspace, bot)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    info!("[chat] cancelling {workspace}/{bot}");
    let _ = state.db.set_bot_status(&workspace, &bot, "cancelled", "", None);
    // Give the background task a moment to notice
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    let _ = state.db.set_bot_status(&workspace, &bot, "idle", "", None);
    let _ = state.db.add_message(&workspace, &bot, "system", "Response cancelled.", None);
    Json(serde_json::json!({"ok": true}))
}

/// Simple hash of a string for change detection. Not cryptographic.
fn simple_hash(s: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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

fn extract_text_attachments(attachments: &Option<Vec<ChatAttachment>>) -> Vec<(String, String)> {
    attachments
        .as_ref()
        .map(|atts| {
            atts.iter()
                .filter(|a| !a.mime_type.starts_with("image/"))
                .filter_map(|a| {
                    // data_url format: "data:text/plain;base64,SGVsbG8..."
                    let parts: Vec<&str> = a.data_url.splitn(2, ',').collect();
                    if parts.len() == 2 {
                        // Decode base64 to text
                        let decoded = base64_decode(parts[1])?;
                        let text = String::from_utf8(decoded).ok()?;
                        Some((a.name.clone(), text))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    // Simple base64 decode without pulling in a crate
    let input = input.trim();
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut buf = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for byte in input.bytes() {
        if byte == b'=' || byte == b'\n' || byte == b'\r' {
            continue;
        }
        let val = table.iter().position(|&b| b == byte)? as u32;
        acc = (acc << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            buf.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(buf)
}

// ── Background bot runners (write to DB, not SSE) ──

async fn run_bot_claude(
    message: String,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    resume_id: Option<String>,
    images: Vec<(String, String)>,
    db: &Db,
    ws: &str,
    bot: &str,
    prompt_hash: &str,
) -> Result<(), String> {
    let opts = SessionOptions {
        dangerously_skip_permissions: true,
        include_partial_messages: true,
        working_dir,
        max_turns: Some(30),
        resume: resume_id,
        system_prompt,
        ..Default::default()
    };

    let client = ClaudeClient::new();
    let mut session = client.spawn(opts).await.map_err(|e| e.to_string())?;

    let send_result = if images.is_empty() {
        session.send_message(&message).await
    } else {
        session.send_message_with_images(&message, images).await
    };
    send_result.map_err(|e| e.to_string())?;

    let _ = db.set_bot_status(ws, bot, "streaming", "", None);

    let mut full_text = String::new();
    loop {
        match session.next_event().await {
            Ok(Some(event)) => match event {
                Event::Stream { assembled, .. } => {
                    for asm in assembled {
                        match asm {
                            AssembledEvent::TextDelta { text, .. } => {
                                full_text.push_str(&text);
                                let _ = db.append_streaming(ws, bot, &text);
                            }
                            AssembledEvent::ContentBlockComplete { block, .. } => {
                                if let ContentBlock::ToolUse { name, .. } = block {
                                    let _ = db.set_bot_status(ws, bot, "streaming", &full_text, Some(&name));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Event::Assistant { message: msg, .. } => {
                    for block in &msg.message.content {
                        if let ContentBlock::Text { text } = block {
                            if !text.is_empty() && full_text.is_empty() {
                                full_text.push_str(text);
                                let _ = db.set_bot_status(ws, bot, "streaming", &full_text, None);
                            }
                        }
                    }
                }
                Event::Result(result) => {
                    let _ = db.set_session(ws, bot, &result.session_id, prompt_hash);
                    break;
                }
                _ => {}
            },
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }

    if !full_text.is_empty() {
        let _ = db.add_message(ws, bot, "assistant", &full_text, None);
    }
    Ok(())
}

async fn run_bot_codex(
    message: String,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    resume_id: Option<String>,
    db: &Db,
    ws: &str,
    bot: &str,
    prompt_hash: &str,
) -> Result<(), String> {
    let client = apiari_codex_sdk::CodexClient::new();
    let prompt = match system_prompt {
        Some(sys) => format!("{sys}\n\n---\n\n{message}"),
        None => message,
    };

    let mut execution = if let Some(ref sid) = resume_id {
        client.exec_resume(&prompt, apiari_codex_sdk::ResumeOptions {
            session_id: Some(sid.clone()),
            full_auto: true,
            working_dir,
            ..Default::default()
        }).await.map_err(|e| e.to_string())?
    } else {
        client.exec(&prompt, apiari_codex_sdk::ExecOptions {
            full_auto: true,
            working_dir,
            ..Default::default()
        }).await.map_err(|e| e.to_string())?
    };

    let _ = db.set_bot_status(ws, bot, "streaming", "", None);
    let mut full_text = String::new();

    while let Ok(Some(event)) = execution.next_event().await {
        match &event {
            apiari_codex_sdk::Event::ThreadStarted { thread_id } => {
                let _ = db.set_session(ws, bot, thread_id, prompt_hash);
            }
            apiari_codex_sdk::Event::ItemCompleted { item } => {
                if let Some(text) = item.text() {
                    if !text.is_empty() {
                        full_text = text.to_string();
                        let _ = db.set_bot_status(ws, bot, "streaming", &full_text, None);
                    }
                }
            }
            apiari_codex_sdk::Event::TurnFailed { error, .. } => {
                let msg = error.as_ref().and_then(|e| e.message.as_deref()).unwrap_or("codex failed");
                return Err(msg.to_string());
            }
            apiari_codex_sdk::Event::Error { message } => {
                return Err(message.as_deref().unwrap_or("codex error").to_string());
            }
            _ => {}
        }
    }

    if !full_text.is_empty() {
        let _ = db.add_message(ws, bot, "assistant", &full_text, None);
    }
    Ok(())
}

async fn run_bot_gemini(
    message: String,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    resume_id: Option<String>,
    db: &Db,
    ws: &str,
    bot: &str,
    prompt_hash: &str,
) -> Result<(), String> {
    let client = apiari_gemini_sdk::GeminiClient::new();
    let prompt = match system_prompt {
        Some(sys) => format!("{sys}\n\n---\n\n{message}"),
        None => message,
    };

    let mut execution = if let Some(ref sid) = resume_id {
        client.exec_resume(&prompt, apiari_gemini_sdk::SessionOptions {
            session_id: Some(sid.clone()),
            working_dir,
            ..Default::default()
        }).await.map_err(|e| e.to_string())?
    } else {
        client.exec(&prompt, apiari_gemini_sdk::GeminiOptions {
            working_dir,
            ..Default::default()
        }).await.map_err(|e| e.to_string())?
    };

    let _ = db.set_bot_status(ws, bot, "streaming", "", None);
    let mut full_text = String::new();

    while let Ok(Some(event)) = execution.next_event().await {
        match &event {
            apiari_gemini_sdk::Event::ThreadStarted { thread_id } => {
                let _ = db.set_session(ws, bot, thread_id, prompt_hash);
            }
            apiari_gemini_sdk::Event::ItemCompleted { item } => {
                if let Some(text) = item.text() {
                    if !text.is_empty() {
                        full_text = text.to_string();
                        let _ = db.set_bot_status(ws, bot, "streaming", &full_text, None);
                    }
                }
            }
            apiari_gemini_sdk::Event::TurnFailed { error, .. } => {
                let msg = error.as_ref().and_then(|e| e.message.as_deref()).unwrap_or("gemini failed");
                return Err(msg.to_string());
            }
            apiari_gemini_sdk::Event::Error { message } => {
                return Err(message.as_deref().unwrap_or("gemini error").to_string());
            }
            _ => {}
        }
    }

    if !full_text.is_empty() {
        let _ = db.add_message(ws, bot, "assistant", &full_text, None);
    }
    Ok(())
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

// ── Worker detail + messaging ──

#[derive(Serialize)]
struct WorkerDetail {
    #[serde(flatten)]
    info: WorkerInfo,
    output: Option<String>,
    conversation: Vec<WorkerMessage>,
}

#[derive(Serialize)]
struct WorkerMessage {
    role: String,
    content: String,
    timestamp: Option<String>,
}

async fn get_worker_detail(
    State(state): State<AppState>,
    Path((workspace, worker_id)): Path<(String, String)>,
) -> Result<Json<WorkerDetail>, StatusCode> {
    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));
    let ws_config = load_workspace_config(&config_path);
    let root = ws_config
        .workspace
        .as_ref()
        .and_then(|w| w.root.as_ref())
        .map(PathBuf::from)
        .ok_or(StatusCode::NOT_FOUND)?;

    let workers = read_swarm_workers(&root);
    let info = workers
        .into_iter()
        .find(|w| w.id == worker_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    // Read worker output
    let state_path = root.join(".swarm/state.json");
    let worktree_path = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
        .and_then(|s| {
            s.get("workers")?
                .as_array()?
                .iter()
                .find(|w| w.get("id").and_then(|i| i.as_str()) == Some(&worker_id))?
                .get("worktree_path")
                .and_then(|p| p.as_str())
                .map(PathBuf::from)
        });

    let output = worktree_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p.join(".swarm/output.md")).ok());

    // Read conversation from agent activity log
    let conversation = worktree_path
        .as_ref()
        .map(|p| read_worker_conversation(p, &root, &worker_id))
        .unwrap_or_default();

    Ok(Json(WorkerDetail {
        info,
        output,
        conversation,
    }))
}

fn read_worker_conversation(
    worktree_path: &std::path::Path,
    root: &std::path::Path,
    worker_id: &str,
) -> Vec<WorkerMessage> {
    let mut messages = Vec::new();

    // Read the prompt that started this worker
    let task_file = worktree_path.join(".task/TASK.md");
    if let Ok(task) = std::fs::read_to_string(&task_file) {
        messages.push(WorkerMessage {
            role: "system".to_string(),
            content: task,
            timestamp: None,
        });
    }

    // Read agent output
    let output_file = worktree_path.join(".swarm/output.md");
    if let Ok(output) = std::fs::read_to_string(&output_file) {
        messages.push(WorkerMessage {
            role: "assistant".to_string(),
            content: output,
            timestamp: None,
        });
    }

    // Read messages sent to this worker via swarm
    let inbox_file = root.join(format!(".swarm/inbox/{worker_id}.jsonl"));
    if let Ok(content) = std::fs::read_to_string(&inbox_file) {
        for line in content.lines() {
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) {
                let content = msg
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                if !content.is_empty() {
                    messages.push(WorkerMessage {
                        role: "user".to_string(),
                        content,
                        timestamp: msg
                            .get("timestamp")
                            .and_then(|t| t.as_str())
                            .map(String::from),
                    });
                }
            }
        }
    }

    messages
}

#[derive(Deserialize)]
struct WorkerMessageRequest {
    message: String,
}

async fn send_worker_message(
    State(state): State<AppState>,
    Path((workspace, worker_id)): Path<(String, String)>,
    Json(body): Json<WorkerMessageRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));
    let ws_config = load_workspace_config(&config_path);
    let root = ws_config
        .workspace
        .as_ref()
        .and_then(|w| w.root.as_ref())
        .map(PathBuf::from)
        .ok_or(StatusCode::NOT_FOUND)?;

    info!("[worker] sending to {worker_id}: {}", body.message);

    let output = tokio::process::Command::new("swarm")
        .arg("--dir")
        .arg(&root)
        .arg("send")
        .arg(&worker_id)
        .arg(&body.message)
        .output()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if output.status.success() {
        Ok(Json(serde_json::json!({"ok": true})))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(Json(
            serde_json::json!({"ok": false, "error": stderr.to_string()}),
        ))
    }
}

// ── Frontend ──

async fn serve_frontend(
    _uri: axum::http::Uri,
) -> Result<axum::response::Html<String>, StatusCode> {
    let html = include_str!("../web/index.html");
    Ok(axum::response::Html(html.to_string()))
}
