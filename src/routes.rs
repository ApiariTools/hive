use apiari_claude_sdk::{
    ClaudeClient, Event, SessionOptions, streaming::AssembledEvent, types::ContentBlock,
};
use axum::{
    Router,
    extract::{Multipart, Path, Query, State, WebSocketUpgrade, ws},
    http::StatusCode,
    response::Json,
    routing::{get, post},
};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::db::Db;
use crate::events::{EventHub, HiveEvent};
use crate::pr_review::PrReviewCache;
use crate::usage::UsageCache;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub config_dir: PathBuf,
    pub events: EventHub,
    pub pr_review_cache: PrReviewCache,
    pub usage_cache: UsageCache,
    pub http_client: reqwest::Client,
    pub tts_base_url: String,
    pub stt_base_url: String,
}

pub fn router(
    db: Db,
    config_dir: &std::path::Path,
    events: EventHub,
    pr_review_cache: PrReviewCache,
    usage_cache: UsageCache,
) -> Router {
    router_with_http_client(
        db,
        config_dir,
        events,
        pr_review_cache,
        usage_cache,
        reqwest::Client::new(),
        "http://127.0.0.1:4201".to_string(),
        "http://127.0.0.1:4202".to_string(),
    )
}

pub fn router_with_http_client(
    db: Db,
    config_dir: &std::path::Path,
    events: EventHub,
    pr_review_cache: PrReviewCache,
    usage_cache: UsageCache,
    http_client: reqwest::Client,
    tts_base_url: String,
    stt_base_url: String,
) -> Router {
    let state = AppState {
        db,
        config_dir: config_dir.to_path_buf(),
        events,
        pr_review_cache,
        usage_cache,
        http_client,
        tts_base_url,
        stt_base_url,
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
        .route("/api/workspaces/{workspace}/chat/{bot}", post(send_message))
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
        .route("/api/transcribe", post(transcribe_audio))
        .route("/api/tts", post(text_to_speech))
        .route("/api/tts/speak", get(tts_speak))
        .route("/api/tts/{message_id}", get(tts_for_message))
        .route("/api/workspaces/{workspace}/unread", get(get_unread))
        .route("/api/workspaces/{workspace}/seen/{bot}", post(mark_seen))
        .route("/api/usage", get(get_usage))
        .route("/ws", get(ws_handler))
        .route("/api/workspaces/{workspace}/repos", get(list_repos))
        .route("/api/workspaces/{workspace}/workers", get(list_workers))
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}",
            get(get_worker_detail),
        )
        .route(
            "/api/workspaces/{workspace}/workers/{worker_id}/send",
            post(send_worker_message),
        )
        .route("/api/workspaces/{workspace}/docs", get(list_docs))
        .route(
            "/api/workspaces/{workspace}/docs/{filename}",
            get(get_doc).put(put_doc).delete(delete_doc),
        )
        .fallback(get(serve_frontend))
        .layer(axum::extract::DefaultBodyLimit::max(50 * 1024 * 1024)) // 50MB for image attachments
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
            if path.extension().is_some_and(|e| e == "toml")
                && let Some(name) = path.file_stem().and_then(|s| s.to_str())
            {
                let config = load_workspace_config(&path);
                let (tts_voice, tts_speed) = config
                    .workspace
                    .map(|w| (w.tts_voice, w.tts_speed))
                    .unwrap_or((None, None));
                workspaces.push(WorkspaceInfo {
                    name: name.to_string(),
                    tts_voice,
                    tts_speed,
                });
            }
        }
    }

    workspaces.sort_by(|a, b| a.name.cmp(&b.name));
    Json(workspaces)
}

#[derive(Serialize)]
struct WorkspaceInfo {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tts_voice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tts_speed: Option<f32>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    prompt_file: Option<String>,
    #[serde(default)]
    watch: Vec<String>,
    #[serde(default)]
    services: Vec<String>,
}

fn default_provider() -> String {
    "claude".to_string()
}

fn default_main_bot() -> BotInfo {
    BotInfo {
        name: "Main".to_string(),
        color: Some("#f5c542".to_string()),
        role: Some("Workspace assistant".to_string()),
        description: None,
        provider: default_provider(),
        model: None,
        prompt_file: None,
        watch: vec![],
        services: vec![],
    }
}

fn load_bots_from_config(path: &std::path::Path) -> Vec<BotInfo> {
    let mut bots = vec![default_main_bot()];

    if let Ok(content) = std::fs::read_to_string(path)
        && let Ok(config) = toml::from_str::<WorkspaceConfig>(&content)
    {
        let mut configured_bots = config.bots.unwrap_or_default();
        if let Some(main_idx) = configured_bots.iter().position(|bot| bot.name == "Main") {
            bots[0] = configured_bots.remove(main_idx);
        }
        bots.extend(configured_bots);
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
    tts_voice: Option<String>,
    tts_speed: Option<f32>,
}

fn load_workspace_config(path: &std::path::Path) -> WorkspaceConfig {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| toml::from_str(&c).ok())
        .unwrap_or_default()
}

/// Parse `.apiari/services.toml` and generate prompt sections for the requested services.
/// Public because `watcher.rs` also uses this for proactive bot prompts.
pub fn build_services_prompt(root: &std::path::Path, services: &[String]) -> String {
    if services.is_empty() {
        return String::new();
    }

    let services_path = root.join(".apiari/services.toml");
    let content = match std::fs::read_to_string(&services_path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let config: toml::Value = match toml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let mut prompt = String::new();

    for service_name in services {
        let section = match config.get(service_name).and_then(|v| v.as_table()) {
            Some(s) => s,
            None => continue,
        };

        match service_name.as_str() {
            "sentry" => {
                let token = section.get("token").and_then(|v| v.as_str()).unwrap_or("");
                let org = section.get("org").and_then(|v| v.as_str()).unwrap_or("");
                let project = section
                    .get("project")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !token.is_empty() && !org.is_empty() && !project.is_empty() {
                    prompt.push_str(&format!(
                        "\n## Sentry Access\n\
                         Query unresolved issues:\n\
                         curl -s -H \"Authorization: Bearer {token}\" \
                         \"https://sentry.io/api/0/projects/{org}/{project}/issues/?query=is:unresolved&sort=date&per_page=20\"\n"
                    ));
                }
            }
            "grafana" => {
                let token = section.get("token").and_then(|v| v.as_str()).unwrap_or("");
                let url = section
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim_end_matches('/');
                if !token.is_empty() && !url.is_empty() {
                    prompt.push_str(&format!(
                        "\n## Grafana Access\n\
                         List dashboards: curl -s -H \"Authorization: Bearer {token}\" \"{url}/api/search?type=dash-db\"\n\
                         Get dashboard: curl -s -H \"Authorization: Bearer {token}\" \"{url}/api/dashboards/uid/{{uid}}\"\n\
                         Check alerts: curl -s -H \"Authorization: Bearer {token}\" \"{url}/api/v1/provisioning/alert-rules\"\n"
                    ));
                }
            }
            _ => {}
        }
    }

    if !prompt.is_empty() {
        prompt.insert_str(
            0,
            "\nIMPORTANT: The credentials below are secrets. \
             Never print, log, or expose them in responses. \
             Only use them in curl commands.\n",
        );
    }

    prompt
}

fn build_docs_index(root: &std::path::Path) -> Option<String> {
    let docs_dir = root.join(".apiari/docs");
    if !docs_dir.is_dir() {
        return None;
    }

    let mut entries: Vec<(String, String)> = Vec::new();

    if let Ok(read_dir) = std::fs::read_dir(&docs_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let filename = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Read only the first non-empty line for the description
            let description = std::fs::File::open(&path)
                .ok()
                .and_then(|f| {
                    use std::io::BufRead;
                    std::io::BufReader::new(f)
                        .lines()
                        .map_while(Result::ok)
                        .find(|line| !line.trim().is_empty())
                        .map(|line| line.trim_start_matches('#').trim().to_string())
                })
                .unwrap_or_default();

            entries.push((filename, description));
        }
    }

    if entries.is_empty() {
        return None;
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut index = String::from(
        "\n## Workspace Docs (.apiari/docs/)\nReference docs available in this workspace. Read with `hive docs read` when relevant to the conversation.\n",
    );
    for (filename, desc) in &entries {
        if desc.is_empty() {
            index.push_str(&format!("- {filename}\n"));
        } else {
            index.push_str(&format!("- {filename} — {desc}\n"));
        }
    }

    Some(index)
}

fn build_docs_instructions(ws_id: &str) -> String {
    format!(
        "\n## Workspace Docs\n\
         You can manage reference docs in `.apiari/docs/`. These are markdown files shared across all bots in this workspace.\n\n\
         - **List docs**: `hive docs list --workspace {ws_id}`\n\
         - **Read a doc**: `hive docs read --workspace {ws_id} <filename>`\n\
         - **Create/update a doc**: Write content to a temp file, then `hive docs write --workspace {ws_id} <filename> --file /tmp/doc.md`\n\
         - **Delete a doc**: `hive docs delete --workspace {ws_id} <filename>`\n\n\
         Use docs to store reference material, project notes, cheat sheets, or anything useful for the workspace. \
         The first line of each doc (heading or plain text) becomes its description in the index.\n"
    )
}

fn build_system_prompt(ws_config: &WorkspaceConfig, bot_name: &str, ws_id: &str) -> String {
    let ws = ws_config.workspace.clone().unwrap_or_default();
    let ws_name = ws.name.as_deref().unwrap_or("unknown");
    let ws_desc = ws.description.as_deref().unwrap_or("");

    let bot_config = ws_config
        .bots
        .as_ref()
        .and_then(|bots| bots.iter().find(|b| b.name == bot_name));

    // Check for a bot-level prompt file (replaces the default identity section)
    if let Some(ref prompt_file) = bot_config.and_then(|b| b.prompt_file.clone()) {
        let root = ws.root.as_deref().unwrap_or(".");
        let root_path = std::path::Path::new(root);
        let path = root_path.join(prompt_file);
        if let Ok(custom) = std::fs::read_to_string(&path) {
            // Custom prompt gets workspace context appended
            let mut prompt = custom;
            if !prompt.ends_with('\n') {
                prompt.push('\n');
            }
            prompt.push_str(&format!("\nWorkspace: {ws_name} — {ws_desc}\n"));
            if let Some(ref root) = ws.root {
                prompt.push_str(&format!("Working directory: {root}\n"));
            }
            // Inject docs index and management instructions for custom prompt bots
            if let Some(docs_index) = build_docs_index(root_path) {
                prompt.push_str(&docs_index);
            }
            prompt.push_str(&build_docs_instructions(ws_id));

            // Inject service credentials even for custom prompt bots
            if let Some(bot) = bot_config {
                let services_prompt = build_services_prompt(root_path, &bot.services);
                if !services_prompt.is_empty() {
                    prompt.push_str(&services_prompt);
                }
            }
            return prompt;
        }
    }

    let bot_role = bot_config
        .and_then(|b| b.role.as_deref())
        .unwrap_or("Workspace assistant");

    let mut prompt = format!(
        "You are {bot_name}, a bot in the \"{ws_name}\" workspace.\n\
         Workspace: {ws_desc}\n\
         Your role: {bot_role}\n\n\
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
            if !context.ends_with('\n') {
                prompt.push('\n');
            }
        }

        // Load .apiari/soul.md if it exists
        let soul_path = root_path.join(".apiari/soul.md");
        if let Ok(soul) = std::fs::read_to_string(&soul_path) {
            prompt.push_str("\n## Communication Style\n");
            prompt.push_str(&soul);
            if !soul.ends_with('\n') {
                prompt.push('\n');
            }
        }

        // Docs index from .apiari/docs/
        if let Some(docs_index) = build_docs_index(root_path) {
            prompt.push_str(&docs_index);
        }

        // Docs management instructions (always injected so bots know they can create docs)
        prompt.push_str(&build_docs_instructions(ws_id));

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
                 Each worker prompt must be self-contained — workers cannot see other repos.\n\n\
                 Workers auto-close when their PR is merged — do NOT manually close them or pull after merge. \
                 Swarm handles cleanup and pulls main automatically.\n"
            ));
        }

        // Inject service credentials from .apiari/services.toml
        if let Some(bot) = bot_config {
            let services_prompt = build_services_prompt(root_path, &bot.services);
            if !services_prompt.is_empty() {
                prompt.push_str(&services_prompt);
            }
        }
    }

    // Hive configuration reference — helps bots answer config questions
    prompt.push_str(
        "\n## Hive Configuration Reference\n\
         You are running inside Hive, a workspace chat hub. The user may ask about configuring their workspace.\n\n\
         Workspace config: ~/.config/hive/workspaces/<workspace-id>.toml\n\
         (The workspace id is the config filename stem, which may differ from [workspace].name.)\n\n\
         [workspace]\n\
         root = \"/path/to/project\"    # workspace root directory\n\
         name = \"my-workspace\"        # display name\n\
         description = \"...\"          # optional description\n\
         tts_voice = \"af_nova\"         # TTS voice (optional)\n\
         tts_speed = 1.2              # TTS speed multiplier (optional, default 1.2)\n\n\
         [[bots]]\n\
         name = \"BotName\"             # bot display name\n\
         color = \"#f5c542\"            # hex color for UI\n\
         role = \"Description\"         # short role (shown in sidebar)\n\
         description = \"...\"          # longer description (shown in chat header)\n\
         provider = \"claude\"           # claude | codex | gemini\n\
         model = \"...\"                # optional model override\n\
         prompt_file = \"path.md\"      # custom system prompt file\n\
         watch = [\"github\"]            # signal sources: github, sentry\n\
         schedule_hours = 24           # proactive run interval\n\
         proactive_prompt = \"...\"     # task for scheduled runs\n\
         services = [\"sentry\"]        # inject service credentials from .apiari/services.toml\n\n\
         Context files in workspace root:\n\
         - .apiari/context.md — project context (appended to all bot prompts)\n\
         - .apiari/soul.md — communication style (appended to all bot prompts)\n\
         - .apiari/docs/ — reference docs (indexed, read on demand)\n\
         - .apiari/services.toml — service credentials (sentry, grafana)\n\n\
         To initialize a new workspace: `hive init <name> [--root /path]`\n\
         The user can edit these files directly. If they ask you to help configure, \
         explain the options and suggest what to add to their TOML or context files.\n",
    );

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
    let limit = params.limit.unwrap_or(30);
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
    let limit = params.limit.unwrap_or(30);
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
    state.events.send(HiveEvent::Message {
        workspace: workspace.clone(),
        bot: bot.clone(),
        role: "user".to_string(),
        content: body.message.clone(),
    });

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
    let model = bot_config.as_ref().and_then(|b| b.model.clone());

    // Build system prompt and hash it — if prompt changed, start fresh session
    let full_prompt = build_system_prompt(&ws_config, &bot, &workspace);
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

    let events = state.events.clone();

    // Set bot status to thinking
    let _ = db.set_bot_status(&ws_name, &bot_name, "thinking", "", None);
    events.send(HiveEvent::BotStatus {
        workspace: ws_name.clone(),
        bot: bot_name.clone(),
        status: "thinking".to_string(),
        tool_name: None,
    });

    // Spawn background task with 5-minute timeout
    tokio::spawn(async move {
        let task = async {
            match provider.as_str() {
                "codex" => {
                    run_bot_codex(
                        message,
                        system_prompt,
                        working_dir,
                        resume_id,
                        model,
                        images,
                        &db,
                        &events,
                        &ws_name,
                        &bot_name,
                        &hash,
                    )
                    .await
                }
                "gemini" => {
                    run_bot_gemini(
                        message,
                        system_prompt,
                        working_dir,
                        resume_id,
                        model,
                        &db,
                        &events,
                        &ws_name,
                        &bot_name,
                        &hash,
                    )
                    .await
                }
                _ => {
                    run_bot_claude(
                        message,
                        system_prompt,
                        working_dir,
                        resume_id,
                        images,
                        &db,
                        &events,
                        &ws_name,
                        &bot_name,
                        &hash,
                    )
                    .await
                }
            }
        };

        let result = tokio::time::timeout(std::time::Duration::from_secs(300), task).await;

        match result {
            Ok(Err(e)) => {
                add_message_and_emit(
                    &db,
                    &events,
                    &ws_name,
                    &bot_name,
                    "assistant",
                    &format!("Error: {e}"),
                );
            }
            Err(_) => {
                add_message_and_emit(
                    &db,
                    &events,
                    &ws_name,
                    &bot_name,
                    "system",
                    "Response timed out after 5 minutes.",
                );
            }
            Ok(Ok(())) => {}
        }

        let _ = db.set_bot_status(&ws_name, &bot_name, "idle", "", None);
        events.send(HiveEvent::BotStatus {
            workspace: ws_name.clone(),
            bot: bot_name.clone(),
            status: "idle".to_string(),
            tool_name: None,
        });
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
    let _ = state
        .db
        .set_bot_status(&workspace, &bot, "cancelled", "", None);
    // Give the background task a moment to notice
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    let _ = state.db.set_bot_status(&workspace, &bot, "idle", "", None);
    add_message_and_emit(
        &state.db,
        &state.events,
        &workspace,
        &bot,
        "system",
        "Response cancelled.",
    );
    Json(serde_json::json!({"ok": true}))
}

// ── Unread tracking ──

async fn get_unread(
    State(state): State<AppState>,
    Path(workspace): Path<String>,
) -> Json<serde_json::Value> {
    let counts = state.db.get_unread_counts(&workspace).unwrap_or_default();
    let map: serde_json::Map<String, serde_json::Value> = counts
        .into_iter()
        .map(|(bot, count)| (bot, serde_json::Value::from(count)))
        .collect();
    Json(serde_json::Value::Object(map))
}

async fn mark_seen(
    State(state): State<AppState>,
    Path((workspace, bot)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let _ = state.db.mark_seen(&workspace, &bot);
    Json(serde_json::json!({"ok": true}))
}

// ── WebSocket ──

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> axum::response::Response {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: ws::WebSocket, state: AppState) {
    let mut rx = state.events.subscribe();

    loop {
        tokio::select! {
            // Forward events to the client
            event = rx.recv() => {
                match event {
                    Ok(e) => {
                        let json = serde_json::to_string(&e).unwrap_or_default();
                        if socket.send(ws::Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            // Handle client messages (ping/pong, close)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(ws::Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
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

fn add_message_and_emit(
    db: &Db,
    events: &EventHub,
    workspace: &str,
    bot: &str,
    role: &str,
    content: &str,
) {
    let _ = db.add_message(workspace, bot, role, content, None);
    events.send(HiveEvent::Message {
        workspace: workspace.to_string(),
        bot: bot.to_string(),
        role: role.to_string(),
        content: content.to_string(),
    });
}

// ── Background bot runners (write to DB, not SSE) ──

async fn run_bot_claude(
    message: String,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    resume_id: Option<String>,
    images: Vec<(String, String)>,
    db: &Db,
    events: &EventHub,
    ws: &str,
    bot: &str,
    prompt_hash: &str,
) -> Result<(), String> {
    let opts = SessionOptions {
        dangerously_skip_permissions: true,
        include_partial_messages: true,
        working_dir,
        max_turns: Some(25),
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
                            AssembledEvent::ContentBlockComplete {
                                block: ContentBlock::ToolUse { name, .. },
                                ..
                            } => {
                                let _ = db.set_bot_status(
                                    ws,
                                    bot,
                                    "streaming",
                                    &full_text,
                                    Some(&name),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                Event::Assistant { message: msg, .. } => {
                    for block in &msg.message.content {
                        if let ContentBlock::Text { text } = block
                            && !text.is_empty()
                            && full_text.is_empty()
                        {
                            full_text.push_str(text);
                            let _ = db.set_bot_status(ws, bot, "streaming", &full_text, None);
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
        add_message_and_emit(db, events, ws, bot, "assistant", full_text.trim());
    }
    Ok(())
}

async fn run_bot_codex(
    message: String,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    resume_id: Option<String>,
    model: Option<String>,
    images: Vec<(String, String)>,
    db: &Db,
    events: &EventHub,
    ws: &str,
    bot: &str,
    prompt_hash: &str,
) -> Result<(), String> {
    let client = apiari_codex_sdk::CodexClient::new();
    let prompt = match system_prompt {
        Some(sys) => format!("{sys}\n\n---\n\n{message}"),
        None => message,
    };

    // Save base64 images to temp files for codex --image flag
    let _tmp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let image_paths: Vec<PathBuf> = images
        .iter()
        .enumerate()
        .filter_map(|(i, (mime, data))| {
            let ext = if mime.contains("png") { "png" } else { "jpg" };
            let path = _tmp_dir.path().join(format!("img_{i}.{ext}"));
            let decoded = base64_decode(data)?;
            std::fs::write(&path, decoded).ok()?;
            Some(path)
        })
        .collect();

    let mut execution = if let Some(ref sid) = resume_id {
        client
            .exec_resume(
                &prompt,
                apiari_codex_sdk::ResumeOptions {
                    session_id: Some(sid.clone()),
                    model: model.clone(),
                    full_auto: true,
                    working_dir,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| e.to_string())?
    } else {
        client
            .exec(
                &prompt,
                apiari_codex_sdk::ExecOptions {
                    model: model.clone(),
                    full_auto: true,
                    working_dir,
                    images: image_paths,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| e.to_string())?
    };

    let _ = db.set_bot_status(ws, bot, "streaming", "", None);
    let mut full_text = String::new();

    let mut update_text = |text: &str| {
        if !text.is_empty() {
            full_text = text.to_string();
            let _ = db.set_bot_status(ws, bot, "streaming", &full_text, None);
        }
    };

    loop {
        match execution.next_event().await {
            Ok(Some(event)) => match &event {
                apiari_codex_sdk::Event::ThreadStarted { thread_id } => {
                    let _ = db.set_session(ws, bot, thread_id, prompt_hash);
                }
                apiari_codex_sdk::Event::ItemUpdated { item }
                | apiari_codex_sdk::Event::ItemCompleted { item } => {
                    if let Some(text) = item.text() {
                        update_text(text);
                    }
                }
                apiari_codex_sdk::Event::TurnFailed { error, .. } => {
                    let msg = error
                        .as_ref()
                        .and_then(|e| e.message.as_deref())
                        .unwrap_or("codex failed");
                    return Err(msg.to_string());
                }
                apiari_codex_sdk::Event::Error { message } => {
                    return Err(message.as_deref().unwrap_or("codex error").to_string());
                }
                _ => {}
            },
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }

    if !full_text.is_empty() {
        add_message_and_emit(db, events, ws, bot, "assistant", full_text.trim());
    }
    Ok(())
}

async fn run_bot_gemini(
    message: String,
    system_prompt: Option<String>,
    working_dir: Option<PathBuf>,
    resume_id: Option<String>,
    model: Option<String>,
    db: &Db,
    events: &EventHub,
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
        client
            .exec_resume(
                &prompt,
                apiari_gemini_sdk::SessionOptions {
                    session_id: Some(sid.clone()),
                    model: model.clone(),
                    working_dir,
                },
            )
            .await
            .map_err(|e| e.to_string())?
    } else {
        client
            .exec(
                &prompt,
                apiari_gemini_sdk::GeminiOptions {
                    model: model.clone(),
                    working_dir,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| e.to_string())?
    };

    let _ = db.set_bot_status(ws, bot, "streaming", "", None);
    let mut full_text = String::new();
    let mut recent_events: Vec<String> = Vec::new();

    let mut update_text = |text: &str| {
        if !text.is_empty() {
            full_text = text.to_string();
            let _ = db.set_bot_status(ws, bot, "streaming", &full_text, None);
        }
    };

    loop {
        match execution.next_event().await {
            Ok(Some(event)) => {
                recent_events.push(format!("{event:?}"));
                if recent_events.len() > 6 {
                    recent_events.remove(0);
                }

                match &event {
                    apiari_gemini_sdk::Event::JsonOutput { session_id, .. } => {
                        if let Some(session_id) = session_id.as_deref() {
                            let _ = db.set_session(ws, bot, session_id, prompt_hash);
                        }
                        if let Some(text) = event.text() {
                            update_text(&text);
                        }
                    }
                    apiari_gemini_sdk::Event::Init { session_id, .. } => {
                        let _ = db.set_session(ws, bot, session_id, prompt_hash);
                    }
                    apiari_gemini_sdk::Event::ThreadStarted { thread_id } => {
                        let _ = db.set_session(ws, bot, thread_id, prompt_hash);
                    }
                    apiari_gemini_sdk::Event::Message { .. }
                    | apiari_gemini_sdk::Event::ToolResponse { .. }
                    | apiari_gemini_sdk::Event::AgentEnd { .. }
                    | apiari_gemini_sdk::Event::ItemUpdated { .. }
                    | apiari_gemini_sdk::Event::ItemCompleted { .. } => {
                        if let Some(text) = event.text() {
                            update_text(&text);
                        }
                    }
                    apiari_gemini_sdk::Event::TurnFailed { error, .. } => {
                        let msg = error
                            .as_ref()
                            .and_then(|e| e.message.as_deref())
                            .unwrap_or("gemini failed");
                        return Err(msg.to_string());
                    }
                    apiari_gemini_sdk::Event::Error {
                        message, status, ..
                    } => {
                        let msg = message.as_deref().unwrap_or("gemini error");
                        if let Some(status) = status.as_deref() {
                            return Err(format!("{status}: {msg}"));
                        }
                        return Err(msg.to_string());
                    }
                    _ => {}
                }
            }
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }

    if !full_text.is_empty() {
        add_message_and_emit(db, events, ws, bot, "assistant", full_text.trim());
    } else {
        let summary = if recent_events.is_empty() {
            "no events captured".to_string()
        } else {
            recent_events.join(" | ")
        };
        return Err(format!(
            "gemini returned no assistant text; recent events: {summary}"
        ));
    }
    Ok(())
}

// ── Transcription ──

fn transcribe_err(msg: impl Into<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "error": msg.into() }))
}

async fn transcribe_audio(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> (StatusCode, Json<serde_json::Value>) {
    // Read audio bytes from multipart
    let mut audio_bytes = Vec::new();
    let mut filename = "audio.webm".to_string();
    let mut found_audio = false;

    while let Ok(Some(mut field)) = multipart.next_field().await {
        if field.name() == Some("audio") {
            found_audio = true;
            if let Some(name) = field.file_name() {
                filename = name.to_string();
            }
            while let Ok(Some(chunk)) = field.chunk().await {
                audio_bytes.extend_from_slice(&chunk);
            }
            break;
        }
    }
    if !found_audio {
        return (
            StatusCode::BAD_REQUEST,
            transcribe_err("missing 'audio' field in multipart body"),
        );
    }

    // Try whisper-server first (fast path — model already loaded)
    let stt_url = format!("{}/inference", state.stt_base_url.trim_end_matches('/'));
    let part = reqwest::multipart::Part::bytes(audio_bytes.clone())
        .file_name(filename.clone())
        .mime_str("audio/webm")
        .unwrap_or_else(|_| {
            reqwest::multipart::Part::bytes(audio_bytes.clone()).file_name(filename.clone())
        });
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json")
        .text("temperature", "0.0");

    if let Ok(resp) = state
        .http_client
        .post(&stt_url)
        .multipart(form)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        && resp.status().is_success()
        && let Ok(body) = resp.json::<serde_json::Value>().await
    {
        let text = body["text"].as_str().unwrap_or("").trim().to_string();
        return (StatusCode::OK, Json(serde_json::json!({ "text": text })));
    }

    // Fallback: whisper-cli (cold start, slower but works without server)
    let tmp_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                transcribe_err("failed to create temp dir"),
            );
        }
    };

    let audio_path = tmp_dir.path().join("audio.webm");
    if tokio::fs::write(&audio_path, &audio_bytes).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            transcribe_err("failed to write audio file"),
        );
    }

    // Convert to wav
    let wav_path = tmp_dir.path().join("audio.wav");
    match tokio::process::Command::new("ffmpeg")
        .args(["-i"])
        .arg(&audio_path)
        .args(["-ar", "16000", "-ac", "1", "-y"])
        .arg(&wav_path)
        .output()
        .await
    {
        Ok(o) if !o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                transcribe_err(format!("ffmpeg conversion failed: {stderr}")),
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                StatusCode::OK,
                transcribe_err("ffmpeg not found. Install it with: brew install ffmpeg"),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                transcribe_err(format!("ffmpeg error: {e}")),
            );
        }
        _ => {}
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let model_path = format!("{home}/.local/share/whisper/ggml-base.en.bin");
    let output = match tokio::process::Command::new("whisper-cli")
        .arg("-m")
        .arg(&model_path)
        .arg("--output-txt")
        .arg("--no-timestamps")
        .arg("--output-file")
        .arg(tmp_dir.path().join("audio"))
        .arg(&wav_path)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                StatusCode::OK,
                transcribe_err("whisper-cli not found. Install it with: brew install whisper-cpp"),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                transcribe_err(format!("failed to run whisper: {e}")),
            );
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return (
            StatusCode::OK,
            transcribe_err(format!("whisper failed: {stderr}")),
        );
    }

    let txt_path = tmp_dir.path().join("audio.txt");
    match tokio::fs::read_to_string(&txt_path).await {
        Ok(text) => {
            let trimmed = text.trim().to_string();
            (StatusCode::OK, Json(serde_json::json!({ "text": trimmed })))
        }
        Err(_) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                transcribe_err(format!(
                    "whisper produced no output file. stdout: {stdout}, stderr: {stderr}"
                )),
            )
        }
    }
}

// ── TTS ──

const TTS_MAX_TEXT_LENGTH: usize = 5000;

async fn text_to_speech(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let text = body.get("text").and_then(|t| t.as_str()).unwrap_or("");
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "Missing text").into_response();
    }
    if text.len() > TTS_MAX_TEXT_LENGTH {
        return (StatusCode::BAD_REQUEST, "Text too long (max 5000 chars)").into_response();
    }

    match state
        .http_client
        .post(format!("{}/tts", state.tts_base_url.trim_end_matches('/')))
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(bytes) => {
                (StatusCode::OK, [("content-type", "audio/wav")], bytes).into_response()
            }
            Err(_) => (StatusCode::BAD_GATEWAY, "Failed to read TTS response").into_response(),
        },
        Ok(resp) => {
            let status = resp.status().as_u16();
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                "TTS server error",
            )
                .into_response()
        }
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "TTS server not running. Run: cd tts && ./setup.sh && source .venv/bin/activate && python server.py",
        )
            .into_response(),
    }
}

async fn tts_speak(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let text = params.get("text").cloned().unwrap_or_default();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "Missing text").into_response();
    }
    if text.len() > TTS_MAX_TEXT_LENGTH {
        return (StatusCode::BAD_REQUEST, "Text too long").into_response();
    }

    let voice = params.get("voice").cloned().unwrap_or_default();
    let speed = params.get("speed").and_then(|s| s.parse::<f32>().ok());
    let mut body = serde_json::json!({ "text": text });
    if !voice.is_empty() {
        body["voice"] = serde_json::Value::String(voice);
    }
    if let Some(spd) = speed {
        body["speed"] = serde_json::json!(spd);
    }

    match state
        .http_client
        .post(format!("{}/tts", state.tts_base_url.trim_end_matches('/')))
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(bytes) => (StatusCode::OK, [("content-type", "audio/wav")], bytes).into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, "Failed to read TTS response").into_response(),
        },
        Ok(_) => (StatusCode::BAD_GATEWAY, "TTS server error").into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "TTS server not running").into_response(),
    }
}

async fn tts_for_message(
    State(state): State<AppState>,
    Path(message_id): Path<i64>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let content = match state.db.get_message_content(message_id) {
        Ok(Some(c)) => c,
        Ok(None) => return (StatusCode::NOT_FOUND, "Message not found").into_response(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "DB error").into_response(),
    };

    if content.is_empty() {
        return (StatusCode::BAD_REQUEST, "Empty message").into_response();
    }

    let text = if content.len() > TTS_MAX_TEXT_LENGTH {
        &content[..TTS_MAX_TEXT_LENGTH]
    } else {
        &content
    };

    let voice = params.get("voice").cloned().unwrap_or_default();
    let speed = params.get("speed").and_then(|s| s.parse::<f32>().ok());
    let mut body = serde_json::json!({ "text": text });
    if !voice.is_empty() {
        body["voice"] = serde_json::Value::String(voice);
    }
    if let Some(spd) = speed {
        body["speed"] = serde_json::json!(spd);
    }

    match state
        .http_client
        .post(format!("{}/tts", state.tts_base_url.trim_end_matches('/')))
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(bytes) => (
                StatusCode::OK,
                [
                    ("content-type", "audio/wav"),
                    ("cache-control", "public, max-age=3600"),
                ],
                bytes,
            )
                .into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, "Failed to read TTS response").into_response(),
        },
        Ok(_) => (StatusCode::BAD_GATEWAY, "TTS server error").into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "TTS server not running").into_response(),
    }
}

// ── Docs ──

#[derive(Serialize, Debug)]
struct DocInfo {
    name: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    updated_at: String,
}

fn resolve_workspace_root(state: &AppState, workspace: &str) -> Option<PathBuf> {
    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));
    let ws_config = load_workspace_config(&config_path);
    ws_config
        .workspace
        .as_ref()
        .and_then(|w| w.root.as_ref())
        .map(PathBuf::from)
}

fn validate_doc_filename(filename: &str) -> Result<(), StatusCode> {
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !filename.ends_with(".md") {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn extract_doc_title(content: &str, filename: &str) -> String {
    content
        .lines()
        .find(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim().to_string())
        .unwrap_or_else(|| filename.trim_end_matches(".md").to_string())
}

fn extract_title_from_file(path: &std::path::Path, filename: &str) -> String {
    use std::io::BufRead;
    if let Ok(file) = std::fs::File::open(path) {
        let reader = std::io::BufReader::new(file);
        for line in reader.lines().map_while(Result::ok).take(20) {
            if line.starts_with("# ") {
                return line.trim_start_matches("# ").trim().to_string();
            }
        }
    }
    filename.trim_end_matches(".md").to_string()
}

async fn list_docs(
    State(state): State<AppState>,
    Path(workspace): Path<String>,
) -> Json<Vec<DocInfo>> {
    let root = match resolve_workspace_root(&state, &workspace) {
        Some(r) => r,
        None => return Json(vec![]),
    };

    let docs = tokio::task::spawn_blocking(move || {
        let docs_dir = root.join(".apiari/docs");
        let mut docs = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&docs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "md")
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                {
                    let title = extract_title_from_file(&path, name);
                    let updated_at = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .map(|t| {
                            chrono::DateTime::<chrono::Utc>::from(t)
                                .format("%Y-%m-%dT%H:%M:%SZ")
                                .to_string()
                        })
                        .unwrap_or_default();
                    docs.push(DocInfo {
                        name: name.to_string(),
                        title,
                        content: None,
                        updated_at,
                    });
                }
            }
        }

        docs.sort_by(|a, b| a.name.cmp(&b.name));
        docs
    })
    .await
    .unwrap_or_default();

    Json(docs)
}

async fn get_doc(
    State(state): State<AppState>,
    Path((workspace, filename)): Path<(String, String)>,
) -> Result<Json<DocInfo>, StatusCode> {
    validate_doc_filename(&filename)?;

    let root = resolve_workspace_root(&state, &workspace).ok_or(StatusCode::NOT_FOUND)?;
    let path = root.join(".apiari/docs").join(&filename);

    let content = std::fs::read_to_string(&path).map_err(|_| StatusCode::NOT_FOUND)?;
    let title = extract_doc_title(&content, &filename);
    let updated_at = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| {
            chrono::DateTime::<chrono::Utc>::from(t)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
        .unwrap_or_default();

    Ok(Json(DocInfo {
        name: filename,
        title,
        content: Some(content),
        updated_at,
    }))
}

#[derive(Deserialize)]
struct PutDocBody {
    content: String,
}

async fn put_doc(
    State(state): State<AppState>,
    Path((workspace, filename)): Path<(String, String)>,
    Json(body): Json<PutDocBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    validate_doc_filename(&filename)?;

    let root = resolve_workspace_root(&state, &workspace).ok_or(StatusCode::NOT_FOUND)?;
    let docs_dir = root.join(".apiari/docs");

    std::fs::create_dir_all(&docs_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    std::fs::write(docs_dir.join(&filename), &body.content)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({"ok": true})))
}

async fn delete_doc(
    State(state): State<AppState>,
    Path((workspace, filename)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    validate_doc_filename(&filename)?;

    let root = resolve_workspace_root(&state, &workspace).ok_or(StatusCode::NOT_FOUND)?;
    let path = root.join(".apiari/docs").join(&filename);

    if !path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    std::fs::remove_file(&path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

// ── Repos ──

#[derive(Serialize, Clone)]
struct RepoInfo {
    name: String,
    path: String,
    has_swarm: bool,
    is_clean: bool,
    branch: String,
    workers: Vec<WorkerInfo>,
}

async fn list_repos(
    State(state): State<AppState>,
    Path(workspace): Path<String>,
) -> Json<Vec<RepoInfo>> {
    let config_path = state
        .config_dir
        .join("workspaces")
        .join(format!("{workspace}.toml"));
    let ws_config = load_workspace_config(&config_path);
    let root = match ws_config
        .workspace
        .as_ref()
        .and_then(|w| w.root.as_ref())
        .map(PathBuf::from)
    {
        Some(r) => r,
        None => return Json(vec![]),
    };

    let mut repos = tokio::task::spawn_blocking(move || build_repos_list(&root))
        .await
        .unwrap_or_default();

    let cache = &state.pr_review_cache;
    for repo in &mut repos {
        enrich_workers_with_reviews(&mut repo.workers, cache).await;
    }

    Json(repos)
}

fn build_repos_list(root: &std::path::Path) -> Vec<RepoInfo> {
    let all_workers = read_swarm_workers(root);

    // Map workers to repos from swarm state
    let mut repo_workers: std::collections::HashMap<String, Vec<WorkerInfo>> =
        std::collections::HashMap::new();
    let state_path = root.join(".swarm/state.json");
    if let Ok(content) = std::fs::read_to_string(&state_path)
        && let Ok(state) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(worktrees) = state.get("worktrees").and_then(|w| w.as_array())
    {
        for wt in worktrees {
            let repo_name = wt
                .get("repo_path")
                .and_then(|p| p.as_str())
                .and_then(|p| std::path::Path::new(p).file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let wt_id = wt.get("id").and_then(|i| i.as_str()).unwrap_or("");
            if let Some(worker) = all_workers.iter().find(|w| w.id == wt_id) {
                repo_workers
                    .entry(repo_name)
                    .or_default()
                    .push(worker.clone());
            }
        }
    }

    // Scan for git repos
    let mut repos = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join(".git").exists() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                // Quick git status check
                let is_clean = std::process::Command::new("git")
                    .args(["-C"])
                    .arg(&path)
                    .args(["status", "--porcelain"])
                    .output()
                    .map(|o| o.status.success() && o.stdout.is_empty())
                    .unwrap_or(false);

                let branch = std::process::Command::new("git")
                    .args(["-C"])
                    .arg(&path)
                    .args(["rev-parse", "--abbrev-ref", "HEAD"])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let has_swarm = root.join(".swarm").exists();
                let workers = repo_workers.remove(&name).unwrap_or_default();

                repos.push(RepoInfo {
                    name,
                    path: path.to_string_lossy().to_string(),
                    has_swarm,
                    is_clean,
                    branch,
                    workers,
                });
            }
        }
    }

    repos.sort_by(|a, b| a.name.cmp(&b.name));
    repos
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
    let mut workers = match root {
        Some(root) => read_swarm_workers(&root),
        None => vec![],
    };

    enrich_workers_with_reviews(&mut workers, &state.pr_review_cache).await;

    Json(workers)
}

#[derive(Serialize, Clone)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    review_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ci_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_comments: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_comments: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_comments: Option<u32>,
}

fn read_swarm_workers(root: &std::path::Path) -> Vec<WorkerInfo> {
    let state_path = root.join(".swarm/state.json");
    let content = match std::fs::read_to_string(state_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let state: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let workers = match state.get("worktrees").and_then(|w| w.as_array()) {
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
                .get("agent_kind")
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
                review_state: None,
                ci_status: None,
                total_comments: None,
                open_comments: None,
                resolved_comments: None,
            })
        })
        .collect()
}

/// Enrich workers with PR review data from the cache.
async fn enrich_workers_with_reviews(workers: &mut [WorkerInfo], cache: &PrReviewCache) {
    let guard = cache.lock().await;

    for worker in workers.iter_mut() {
        if let Some(ref url) = worker.pr_url
            && let Some(info) = parse_pr_url_for_cache_key(url)
            && let Some(review) = guard.get(&info)
        {
            worker.review_state.clone_from(&review.review_state);
            worker.ci_status.clone_from(&review.ci_status);
            if review.total_comments > 0 {
                worker.total_comments = Some(review.total_comments);
                worker.open_comments = Some(review.open_comments);
                worker.resolved_comments = Some(review.resolved_comments);
            }
        }
    }
}

/// Extract a cache key from a PR URL (owner/repo/number).
fn parse_pr_url_for_cache_key(url: &str) -> Option<String> {
    let url = url.trim_end_matches('/');
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() < 5 {
        return None;
    }
    let len = parts.len();
    if parts[len - 2] != "pull" {
        return None;
    }
    let number = parts[len - 1].parse::<i64>().ok()?;
    let repo = parts[len - 3];
    let owner = parts[len - 4];
    Some(crate::pr_review::cache_key(owner, repo, number))
}

// ── Worker detail + messaging ──

#[derive(Serialize)]
struct WorkerDetail {
    #[serde(flatten)]
    info: WorkerInfo,
    prompt: Option<String>,
    output: Option<String>,
    conversation: Vec<WorkerMessage>,
}

#[derive(Serialize, Clone)]
struct WorkerMessage {
    role: String,
    content: String,
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

    let mut workers = read_swarm_workers(&root);
    enrich_workers_with_reviews(&mut workers, &state.pr_review_cache).await;
    let info = workers
        .into_iter()
        .find(|w| w.id == worker_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    // Read full worktree entry from state.json
    let state_path = root.join(".swarm/state.json");
    let worktree_entry = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
        .and_then(|s| {
            s.get("worktrees")?
                .as_array()?
                .iter()
                .find(|w| w.get("id").and_then(|i| i.as_str()) == Some(&worker_id))?
                .clone()
                .into()
        });

    let worktree_path = worktree_entry
        .as_ref()
        .and_then(|w: &serde_json::Value| w.get("worktree_path")?.as_str().map(PathBuf::from));

    // Prompt from state.json (the original task)
    let prompt = worktree_entry
        .as_ref()
        .and_then(|w| w.get("prompt")?.as_str().map(String::from));

    // Output from .swarm/output.md
    let output = worktree_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p.join(".swarm/output.md")).ok());

    // Read conversation from swarm's events.jsonl
    let conversation = read_agent_events(&root, &worker_id);

    Ok(Json(WorkerDetail {
        info,
        prompt,
        output,
        conversation,
    }))
}

fn read_agent_events(root: &std::path::Path, worker_id: &str) -> Vec<WorkerMessage> {
    let events_path = root.join(format!(".swarm/agents/{worker_id}/events.jsonl"));
    let content = match std::fs::read_to_string(&events_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut messages = Vec::new();
    let mut current_text = String::new();

    for line in content.lines() {
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "assistant_text" => {
                if let Some(text) = event.get("text").and_then(|t| t.as_str()) {
                    current_text.push_str(text);
                }
            }
            "tool_use" => {
                // Flush any accumulated text
                if !current_text.is_empty() {
                    messages.push(WorkerMessage {
                        role: "assistant".to_string(),
                        content: std::mem::take(&mut current_text),
                    });
                }
                let tool = event.get("tool").and_then(|t| t.as_str()).unwrap_or("tool");
                messages.push(WorkerMessage {
                    role: "tool".to_string(),
                    content: format!("*Using {tool}*"),
                });
            }
            "user_message" => {
                // Flush any accumulated text
                if !current_text.is_empty() {
                    messages.push(WorkerMessage {
                        role: "assistant".to_string(),
                        content: std::mem::take(&mut current_text),
                    });
                }
                if let Some(text) = event.get("text").and_then(|t| t.as_str()) {
                    messages.push(WorkerMessage {
                        role: "user".to_string(),
                        content: text.to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    // Flush remaining text
    if !current_text.is_empty() {
        messages.push(WorkerMessage {
            role: "assistant".to_string(),
            content: current_text,
        });
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

// ── Usage ──

async fn get_usage(State(state): State<AppState>) -> Json<crate::usage::UsageData> {
    let response = {
        let cache = state.usage_cache.lock().await;
        match &*cache {
            crate::usage::CachedUsage::Data(data) => data.clone(),
            crate::usage::CachedUsage::NotInstalled => crate::usage::UsageData {
                installed: false,
                providers: vec![],
                updated_at: None,
            },
            crate::usage::CachedUsage::Unknown => crate::usage::UsageData {
                installed: false,
                providers: vec![],
                updated_at: None,
            },
        }
    };
    Json(response)
}

// ── Frontend ──

#[derive(Embed)]
#[folder = "web/dist/"]
struct FrontendAssets;

async fn serve_frontend(uri: axum::http::Uri) -> axum::response::Response {
    use axum::response::IntoResponse;

    let path = uri.path().trim_start_matches('/');

    // Try to serve the requested file
    if !path.is_empty()
        && let Some(file) = FrontendAssets::get(path)
    {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let cache = if !path.ends_with(".html") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            StatusCode::OK,
            [("content-type", mime.as_ref()), ("cache-control", cache)],
            file.data.into_owned(),
        )
            .into_response();
    }

    // SPA fallback: only for navigation requests, not missing static assets
    if path.starts_with("assets/") {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Some(index) = FrontendAssets::get("index.html") {
        return (
            StatusCode::OK,
            [("content-type", "text/html"), ("cache-control", "no-cache")],
            index.data.into_owned(),
        )
            .into_response();
    }

    StatusCode::NOT_FOUND.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── simple_hash ──

    #[test]
    fn test_hash_deterministic() {
        assert_eq!(simple_hash("hello"), simple_hash("hello"));
    }

    #[test]
    fn test_hash_different_inputs() {
        assert_ne!(simple_hash("hello"), simple_hash("world"));
    }

    #[test]
    fn test_hash_empty() {
        let h = simple_hash("");
        assert!(!h.is_empty());
    }

    // ── base64_decode ──

    #[test]
    fn test_base64_decode_hello() {
        let decoded = base64_decode("SGVsbG8=").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_base64_decode_empty() {
        let decoded = base64_decode("").unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_base64_decode_with_newlines() {
        let decoded = base64_decode("SGVs\nbG8=").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_base64_decode_invalid() {
        // Invalid chars should return None
        assert!(base64_decode("!!!").is_none());
    }

    // ── extract_images ──

    #[test]
    fn test_extract_images_none() {
        let images = extract_images(&None);
        assert!(images.is_empty());
    }

    #[test]
    fn test_extract_images_empty() {
        let images = extract_images(&Some(vec![]));
        assert!(images.is_empty());
    }

    #[test]
    fn test_extract_images_filters_non_images() {
        let atts = vec![
            ChatAttachment {
                name: "doc.txt".into(),
                mime_type: "text/plain".into(),
                data_url: "data:text/plain;base64,SGVsbG8=".into(),
            },
            ChatAttachment {
                name: "photo.jpg".into(),
                mime_type: "image/jpeg".into(),
                data_url: "data:image/jpeg;base64,abc123".into(),
            },
        ];
        let images = extract_images(&Some(atts));
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].0, "image/jpeg");
        assert_eq!(images[0].1, "abc123");
    }

    #[test]
    fn test_extract_images_multiple() {
        let atts = vec![
            ChatAttachment {
                name: "a.png".into(),
                mime_type: "image/png".into(),
                data_url: "data:image/png;base64,AAA".into(),
            },
            ChatAttachment {
                name: "b.jpg".into(),
                mime_type: "image/jpeg".into(),
                data_url: "data:image/jpeg;base64,BBB".into(),
            },
        ];
        let images = extract_images(&Some(atts));
        assert_eq!(images.len(), 2);
    }

    // ── extract_text_attachments ──

    #[test]
    fn test_extract_text_none() {
        let texts = extract_text_attachments(&None);
        assert!(texts.is_empty());
    }

    #[test]
    fn test_extract_text_decodes_base64() {
        let atts = vec![ChatAttachment {
            name: "readme.md".into(),
            mime_type: "text/markdown".into(),
            data_url: "data:text/markdown;base64,SGVsbG8gV29ybGQ=".into(),
        }];
        let texts = extract_text_attachments(&Some(atts));
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].0, "readme.md");
        assert_eq!(texts[0].1, "Hello World");
    }

    #[test]
    fn test_extract_text_skips_images() {
        let atts = vec![ChatAttachment {
            name: "photo.jpg".into(),
            mime_type: "image/jpeg".into(),
            data_url: "data:image/jpeg;base64,abc".into(),
        }];
        let texts = extract_text_attachments(&Some(atts));
        assert!(texts.is_empty());
    }

    // ── build_system_prompt ──

    #[test]
    fn test_build_prompt_basic() {
        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: None,
                name: Some("test".into()),
                description: Some("A test workspace".into()),
                ..Default::default()
            }),
            bots: None,
        };
        let prompt = build_system_prompt(&config, "Main", "test");
        assert!(prompt.contains("Main"));
        assert!(prompt.contains("test"));
        assert!(prompt.contains("A test workspace"));
        assert!(prompt.contains("Workspace assistant")); // default role
    }

    #[test]
    fn test_build_prompt_with_bot_role() {
        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: None,
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: Some(vec![BotInfo {
                name: "Customer".into(),
                color: None,
                role: Some("Handles errors".into()),
                description: None,
                provider: "claude".into(),
                model: None,
                prompt_file: None,
                watch: vec![],
                services: vec![],
            }]),
        };
        let prompt = build_system_prompt(&config, "Customer", "test");
        assert!(prompt.contains("Handles errors"));
    }

    #[test]
    fn test_build_prompt_includes_swarm_when_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".swarm")).unwrap();

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: None,
        };
        let prompt = build_system_prompt(&config, "Main", "test");
        assert!(prompt.contains("Swarm Workers"));
        assert!(prompt.contains("swarm"));
    }

    #[test]
    fn test_build_prompt_no_swarm_when_missing() {
        let dir = tempfile::tempdir().unwrap();

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: None,
        };
        let prompt = build_system_prompt(&config, "Main", "test");
        assert!(!prompt.contains("Swarm Workers"));
    }

    #[test]
    fn test_build_prompt_loads_context_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(
            dir.path().join(".apiari/context.md"),
            "This is a Rust project",
        )
        .unwrap();

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: None,
        };
        let prompt = build_system_prompt(&config, "Main", "test");
        assert!(prompt.contains("This is a Rust project"));
    }

    #[test]
    fn test_build_prompt_loads_soul_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(dir.path().join(".apiari/soul.md"), "Be concise. No filler.").unwrap();

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: None,
        };
        let prompt = build_system_prompt(&config, "Main", "test");
        assert!(prompt.contains("Be concise"));
    }

    #[test]
    fn test_build_prompt_includes_chat_history_tool() {
        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some("/tmp".into()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: None,
        };
        let prompt = build_system_prompt(&config, "Main", "test");
        assert!(prompt.contains("sqlite3"));
        assert!(prompt.contains("Chat History"));
    }

    // ── load_bots_from_config ──

    #[test]
    fn test_load_bots_always_has_main() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let bots = load_bots_from_config(&path);
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].name, "Main");
    }

    #[test]
    fn test_load_bots_from_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(&path, "[[bots]]\nname = \"Perf\"\nrole = \"Monitor\"\n").unwrap();
        let bots = load_bots_from_config(&path);
        assert_eq!(bots.len(), 2);
        assert_eq!(bots[1].name, "Perf");
    }

    #[test]
    fn test_load_bots_description_from_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(
            &path,
            "[[bots]]\nname = \"Customer\"\nrole = \"Error monitor\"\ndescription = \"Monitors user-facing errors via Sentry\"\n",
        )
        .unwrap();
        let bots = load_bots_from_config(&path);
        assert_eq!(bots.len(), 2);
        assert_eq!(
            bots[1].description,
            Some("Monitors user-facing errors via Sentry".to_string())
        );
        // Default Main bot has no description
        assert_eq!(bots[0].description, None);
    }

    #[test]
    fn test_load_bots_main_override_replaces_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(
            &path,
            "[[bots]]\nname = \"Main\"\nprovider = \"codex\"\nmodel = \"o4-mini\"\nrole = \"Primary bot\"\n\n[[bots]]\nname = \"Perf\"\nrole = \"Monitor\"\n",
        )
        .unwrap();

        let bots = load_bots_from_config(&path);
        assert_eq!(bots.len(), 2);
        assert_eq!(bots[0].name, "Main");
        assert_eq!(bots[0].provider, "codex");
        assert_eq!(bots[0].model, Some("o4-mini".to_string()));
        assert_eq!(bots[0].role, Some("Primary bot".to_string()));
        assert_eq!(bots[1].name, "Perf");
    }

    #[test]
    fn test_description_omitted_from_json_when_none() {
        let bot = BotInfo {
            name: "Main".into(),
            color: None,
            role: None,
            description: None,
            provider: "claude".into(),
            model: None,
            prompt_file: None,
            watch: vec![],
            services: vec![],
        };
        let json = serde_json::to_string(&bot).unwrap();
        assert!(!json.contains("description"));
    }

    // ── read_agent_events ──

    #[test]
    fn test_read_agent_events_empty() {
        let dir = tempfile::tempdir().unwrap();
        let events = read_agent_events(dir.path(), "worker-1");
        assert!(events.is_empty());
    }

    #[test]
    fn test_read_agent_events_parses_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".swarm/agents/worker-1");
        std::fs::create_dir_all(&events_dir).unwrap();
        std::fs::write(
            events_dir.join("events.jsonl"),
            r#"{"type":"assistant_text","text":"Hello "}
{"type":"assistant_text","text":"world"}
{"type":"tool_use","tool":"Read"}
{"type":"assistant_text","text":"Done"}
"#,
        )
        .unwrap();

        let msgs = read_agent_events(dir.path(), "worker-1");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[0].content, "Hello world");
        assert_eq!(msgs[1].role, "tool");
        assert_eq!(msgs[1].content, "*Using Read*");
        assert_eq!(msgs[2].role, "assistant");
        assert_eq!(msgs[2].content, "Done");
    }

    #[test]
    fn test_read_agent_events_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join(".swarm/agents/w1");
        std::fs::create_dir_all(&events_dir).unwrap();
        std::fs::write(
            events_dir.join("events.jsonl"),
            r#"{"type":"user_message","text":"fix the bug"}
{"type":"assistant_text","text":"On it"}
"#,
        )
        .unwrap();

        let msgs = read_agent_events(dir.path(), "w1");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "fix the bug");
    }

    // ── read_swarm_workers ──

    #[test]
    fn test_read_workers_no_state() {
        let dir = tempfile::tempdir().unwrap();
        let workers = read_swarm_workers(dir.path());
        assert!(workers.is_empty());
    }

    #[test]
    fn test_read_workers_from_state_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".swarm")).unwrap();
        std::fs::write(
            dir.path().join(".swarm/state.json"),
            r#"{"worktrees":[{"id":"cli-3","branch":"swarm/fix","phase":"running","agent_kind":"claude","pr":{"url":"https://github.com/test/pull/1","title":"Fix stuff"}}]}"#,
        ).unwrap();

        let workers = read_swarm_workers(dir.path());
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, "cli-3");
        assert_eq!(workers[0].branch, "swarm/fix");
        assert_eq!(workers[0].status, "running");
        assert_eq!(workers[0].agent, "claude");
        assert_eq!(
            workers[0].pr_url,
            Some("https://github.com/test/pull/1".to_string())
        );
        assert_eq!(workers[0].pr_title, Some("Fix stuff".to_string()));
    }

    #[test]
    fn test_read_workers_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".swarm")).unwrap();
        std::fs::write(dir.path().join(".swarm/state.json"), r#"{"worktrees":[]}"#).unwrap();

        let workers = read_swarm_workers(dir.path());
        assert!(workers.is_empty());
    }

    // ── build_repos_list ──

    #[test]
    fn test_build_repos_no_git() {
        let dir = tempfile::tempdir().unwrap();
        let repos = build_repos_list(dir.path());
        assert!(repos.is_empty());
    }

    #[test]
    fn test_build_repos_finds_git_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("repo-a/.git")).unwrap();
        std::fs::create_dir_all(dir.path().join("repo-b/.git")).unwrap();
        std::fs::create_dir_all(dir.path().join("not-a-repo")).unwrap();

        let repos = build_repos_list(dir.path());
        assert_eq!(repos.len(), 2);
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"repo-a"));
        assert!(names.contains(&"repo-b"));
    }

    // ── default_provider ──

    #[test]
    fn test_default_provider_is_claude() {
        assert_eq!(default_provider(), "claude");
    }

    // ── build_services_prompt ──

    #[test]
    fn test_services_prompt_empty_services() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = build_services_prompt(dir.path(), &[]);
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_services_prompt_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = build_services_prompt(dir.path(), &["sentry".to_string()]);
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_services_prompt_sentry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(
            dir.path().join(".apiari/services.toml"),
            "[sentry]\norg = \"my-org\"\nproject = \"my-proj\"\ntoken = \"sntryu_abc\"\n",
        )
        .unwrap();

        let prompt = build_services_prompt(dir.path(), &["sentry".to_string()]);
        assert!(prompt.contains("Sentry Access"));
        assert!(prompt.contains("sntryu_abc"));
        assert!(prompt.contains("my-org"));
        assert!(prompt.contains("my-proj"));
        assert!(prompt.contains("is:unresolved"));
    }

    #[test]
    fn test_services_prompt_grafana() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(
            dir.path().join(".apiari/services.toml"),
            "[grafana]\nurl = \"https://grafana.example.com\"\ntoken = \"glsa_xyz\"\n",
        )
        .unwrap();

        let prompt = build_services_prompt(dir.path(), &["grafana".to_string()]);
        assert!(prompt.contains("Grafana Access"));
        assert!(prompt.contains("glsa_xyz"));
        assert!(prompt.contains("grafana.example.com"));
        assert!(prompt.contains("dash-db"));
        assert!(prompt.contains("alert-rules"));
    }

    #[test]
    fn test_services_prompt_multiple() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(
            dir.path().join(".apiari/services.toml"),
            "[sentry]\norg = \"o\"\nproject = \"p\"\ntoken = \"t1\"\n\n[grafana]\nurl = \"https://g.io\"\ntoken = \"t2\"\n",
        )
        .unwrap();

        let prompt =
            build_services_prompt(dir.path(), &["sentry".to_string(), "grafana".to_string()]);
        assert!(prompt.contains("Sentry Access"));
        assert!(prompt.contains("Grafana Access"));
    }

    #[test]
    fn test_services_prompt_unknown_service_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(
            dir.path().join(".apiari/services.toml"),
            "[sentry]\norg = \"o\"\nproject = \"p\"\ntoken = \"t\"\n",
        )
        .unwrap();

        let prompt = build_services_prompt(dir.path(), &["unknown_service".to_string()]);
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_services_prompt_missing_fields_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        // Missing token
        std::fs::write(
            dir.path().join(".apiari/services.toml"),
            "[sentry]\norg = \"o\"\nproject = \"p\"\n",
        )
        .unwrap();

        let prompt = build_services_prompt(dir.path(), &["sentry".to_string()]);
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_services_in_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(
            dir.path().join(".apiari/services.toml"),
            "[sentry]\norg = \"o\"\nproject = \"p\"\ntoken = \"tok\"\n",
        )
        .unwrap();

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: Some(vec![BotInfo {
                name: "Monitor".into(),
                color: None,
                role: Some("Monitors errors".into()),
                description: None,
                provider: "claude".into(),
                model: None,
                prompt_file: None,
                watch: vec![],
                services: vec!["sentry".to_string()],
            }]),
        };
        let prompt = build_system_prompt(&config, "Monitor", "test");
        assert!(prompt.contains("Sentry Access"));
        assert!(prompt.contains("tok"));
    }

    #[test]
    fn test_services_not_injected_for_bot_without_services() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(
            dir.path().join(".apiari/services.toml"),
            "[sentry]\norg = \"o\"\nproject = \"p\"\ntoken = \"tok\"\n",
        )
        .unwrap();

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: Some(vec![BotInfo {
                name: "Plain".into(),
                color: None,
                role: Some("Just chatting".into()),
                description: None,
                provider: "claude".into(),
                model: None,
                prompt_file: None,
                watch: vec![],
                services: vec![],
            }]),
        };
        let prompt = build_system_prompt(&config, "Plain", "test");
        assert!(!prompt.contains("Sentry Access"));
    }

    #[test]
    fn test_bot_config_deserializes_services() {
        let toml_str = r#"
[[bots]]
name = "Perf"
role = "Monitor"
services = ["sentry", "grafana"]
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        let bots = config.bots.unwrap();
        assert_eq!(bots[0].services, vec!["sentry", "grafana"]);
    }

    #[test]
    fn test_services_prompt_includes_secrecy_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(
            dir.path().join(".apiari/services.toml"),
            "[sentry]\norg = \"o\"\nproject = \"p\"\ntoken = \"t\"\n",
        )
        .unwrap();

        let prompt = build_services_prompt(dir.path(), &["sentry".to_string()]);
        assert!(prompt.contains("credentials below are secrets"));
        assert!(prompt.contains("Never print, log, or expose"));
    }

    #[test]
    fn test_services_injected_with_prompt_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari")).unwrap();
        std::fs::write(
            dir.path().join(".apiari/services.toml"),
            "[sentry]\norg = \"o\"\nproject = \"p\"\ntoken = \"tok\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("custom.md"), "You are a custom bot.\n").unwrap();

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: Some(vec![BotInfo {
                name: "Custom".into(),
                color: None,
                role: None,
                description: None,
                provider: "claude".into(),
                model: None,
                prompt_file: Some("custom.md".to_string()),
                watch: vec![],
                services: vec!["sentry".to_string()],
            }]),
        };
        let prompt = build_system_prompt(&config, "Custom", "test");
        assert!(prompt.contains("You are a custom bot."));
        assert!(prompt.contains("Sentry Access"));
    }

    #[test]
    fn test_bot_config_deserializes_without_services() {
        let toml_str = r#"
[[bots]]
name = "Plain"
role = "Chat"
"#;
        let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
        let bots = config.bots.unwrap();
        assert!(bots[0].services.is_empty());
    }

    // ── build_docs_index ──

    #[test]
    fn test_docs_index_no_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(build_docs_index(dir.path()).is_none());
    }

    #[test]
    fn test_docs_index_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".apiari/docs")).unwrap();
        assert!(build_docs_index(dir.path()).is_none());
    }

    #[test]
    fn test_docs_index_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("architecture.md"),
            "# System Architecture\nDetails here.",
        )
        .unwrap();
        std::fs::write(
            docs.join("direction.md"),
            "Product direction and Q2 priorities",
        )
        .unwrap();

        let index = build_docs_index(dir.path()).unwrap();
        assert!(index.contains("architecture.md — System Architecture"));
        assert!(index.contains("direction.md — Product direction and Q2 priorities"));
        // Should be sorted alphabetically
        let arch_pos = index.find("architecture.md").unwrap();
        let dir_pos = index.find("direction.md").unwrap();
        assert!(arch_pos < dir_pos);
    }

    #[test]
    fn test_docs_index_description_from_heading() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("guide.md"), "# My Guide\nContent").unwrap();

        let index = build_docs_index(dir.path()).unwrap();
        assert!(index.contains("guide.md — My Guide"));
    }

    #[test]
    fn test_docs_index_description_from_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("notes.md"), "Plain text first line\nMore content").unwrap();

        let index = build_docs_index(dir.path()).unwrap();
        assert!(index.contains("notes.md — Plain text first line"));
    }

    #[test]
    fn test_docs_index_skips_non_md() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("readme.txt"), "Not a markdown file").unwrap();
        std::fs::write(docs.join("actual.md"), "# Real Doc").unwrap();

        let index = build_docs_index(dir.path()).unwrap();
        assert!(!index.contains("readme.txt"));
        assert!(index.contains("actual.md"));
    }

    #[test]
    fn test_docs_in_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("overview.md"), "# Project Overview").unwrap();

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: None,
        };
        let prompt = build_system_prompt(&config, "Main", "test");
        assert!(prompt.contains("Workspace Docs (.apiari/docs/)"));
        assert!(prompt.contains("overview.md — Project Overview"));
        // Management instructions should reference hive docs commands
        assert!(prompt.contains("## Workspace Docs"));
        assert!(prompt.contains("hive docs list --workspace test"));
        assert!(prompt.contains("hive docs read --workspace test"));
        assert!(prompt.contains("hive docs write --workspace test"));
        assert!(prompt.contains("hive docs delete --workspace test"));
    }

    #[test]
    fn test_docs_instructions_in_custom_prompt_bot() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("guide.md"), "# User Guide").unwrap();
        std::fs::write(dir.path().join("custom.md"), "You are a custom bot.").unwrap();

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: Some(vec![BotInfo {
                name: "Custom".into(),
                color: None,
                role: None,
                description: None,
                provider: "claude".into(),
                model: None,
                prompt_file: Some("custom.md".to_string()),
                watch: vec![],
                services: vec![],
            }]),
        };
        let prompt = build_system_prompt(&config, "Custom", "test");
        assert!(prompt.contains("You are a custom bot."));
        assert!(prompt.contains("Workspace Docs (.apiari/docs/)"));
        assert!(prompt.contains("guide.md — User Guide"));
        assert!(prompt.contains("## Workspace Docs"));
        assert!(prompt.contains("hive docs list --workspace test"));
        assert!(prompt.contains("hive docs write --workspace test"));
    }

    #[test]
    fn test_docs_instructions_present_without_existing_docs() {
        let dir = tempfile::tempdir().unwrap();
        // No .apiari/docs/ directory at all

        let config = WorkspaceConfig {
            workspace: Some(WorkspaceInfo_ {
                root: Some(dir.path().to_string_lossy().to_string()),
                name: Some("test".into()),
                description: None,
                ..Default::default()
            }),
            bots: None,
        };
        let prompt = build_system_prompt(&config, "Main", "test");
        // Even without docs, management instructions should be present
        assert!(prompt.contains("## Workspace Docs"));
        assert!(prompt.contains("hive docs list --workspace test"));
        // But the docs index should NOT be present (no docs dir)
        assert!(!prompt.contains("Workspace Docs (.apiari/docs/)"));
    }

    // ── serve_frontend ──

    #[tokio::test]
    async fn test_spa_fallback_returns_index_html() {
        use axum::http::Uri;

        let uri: Uri = "/some/route".parse().unwrap();
        let resp = serve_frontend(uri).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let headers = resp.headers();
        assert_eq!(headers.get("content-type").unwrap(), "text/html");
        assert_eq!(headers.get("cache-control").unwrap(), "no-cache");
    }

    #[tokio::test]
    async fn test_missing_asset_returns_404() {
        use axum::http::Uri;

        let uri: Uri = "/assets/nonexistent.js".parse().unwrap();
        let resp = serve_frontend(uri).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── docs endpoints ──

    #[test]
    fn test_validate_doc_filename_valid() {
        assert!(validate_doc_filename("architecture.md").is_ok());
        assert!(validate_doc_filename("my-doc.md").is_ok());
    }

    #[test]
    fn test_validate_doc_filename_rejects_traversal() {
        assert!(validate_doc_filename("../etc/passwd").is_err());
        assert!(validate_doc_filename("foo/bar.md").is_err());
        assert!(validate_doc_filename("foo\\bar.md").is_err());
    }

    #[test]
    fn test_validate_doc_filename_rejects_non_md() {
        assert!(validate_doc_filename("readme.txt").is_err());
        assert!(validate_doc_filename("script.js").is_err());
    }

    #[test]
    fn test_extract_doc_title_from_heading() {
        assert_eq!(
            extract_doc_title("# My Title\n\nSome content", "file.md"),
            "My Title"
        );
    }

    #[test]
    fn test_extract_doc_title_fallback_to_filename() {
        assert_eq!(
            extract_doc_title("No heading here", "architecture.md"),
            "architecture"
        );
    }

    #[tokio::test]
    async fn test_list_docs_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let ws_dir = config_dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                dir.path().to_string_lossy()
            ),
        )
        .unwrap();

        let state = AppState {
            db: crate::db::Db::open(&config_dir.path().join("test.db")).unwrap(),
            config_dir: config_dir.path().to_path_buf(),
            events: crate::events::EventHub::new(),
            pr_review_cache: Default::default(),
            usage_cache: Default::default(),
            http_client: reqwest::Client::new(),
            tts_base_url: "http://localhost".to_string(),
            stt_base_url: "http://localhost".to_string(),
        };

        let result = list_docs(State(state), Path("test".to_string())).await;
        assert!(result.0.is_empty());
    }

    #[tokio::test]
    async fn test_list_docs_returns_files() {
        let dir = tempfile::tempdir().unwrap();
        let docs_dir = dir.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("arch.md"), "# Architecture\nDetails").unwrap();
        std::fs::write(docs_dir.join("setup.md"), "Setup guide").unwrap();

        let config_dir = tempfile::tempdir().unwrap();
        let ws_dir = config_dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                dir.path().to_string_lossy()
            ),
        )
        .unwrap();

        let state = AppState {
            db: crate::db::Db::open(&config_dir.path().join("test.db")).unwrap(),
            config_dir: config_dir.path().to_path_buf(),
            events: crate::events::EventHub::new(),
            pr_review_cache: Default::default(),
            usage_cache: Default::default(),
            http_client: reqwest::Client::new(),
            tts_base_url: "http://localhost".to_string(),
            stt_base_url: "http://localhost".to_string(),
        };

        let result = list_docs(State(state), Path("test".to_string())).await;
        assert_eq!(result.0.len(), 2);
        assert_eq!(result.0[0].name, "arch.md");
        assert_eq!(result.0[0].title, "Architecture");
        assert!(result.0[0].content.is_none());
    }

    #[tokio::test]
    async fn test_get_doc_success() {
        let dir = tempfile::tempdir().unwrap();
        let docs_dir = dir.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("arch.md"), "# Architecture\nDetails here").unwrap();

        let config_dir = tempfile::tempdir().unwrap();
        let ws_dir = config_dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                dir.path().to_string_lossy()
            ),
        )
        .unwrap();

        let state = AppState {
            db: crate::db::Db::open(&config_dir.path().join("test.db")).unwrap(),
            config_dir: config_dir.path().to_path_buf(),
            events: crate::events::EventHub::new(),
            pr_review_cache: Default::default(),
            usage_cache: Default::default(),
            http_client: reqwest::Client::new(),
            tts_base_url: "http://localhost".to_string(),
            stt_base_url: "http://localhost".to_string(),
        };

        let result = get_doc(
            State(state),
            Path(("test".to_string(), "arch.md".to_string())),
        )
        .await
        .unwrap();
        assert_eq!(result.0.name, "arch.md");
        assert_eq!(result.0.title, "Architecture");
        assert_eq!(
            result.0.content.as_deref(),
            Some("# Architecture\nDetails here")
        );
    }

    #[tokio::test]
    async fn test_get_doc_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let ws_dir = config_dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                dir.path().to_string_lossy()
            ),
        )
        .unwrap();

        let state = AppState {
            db: crate::db::Db::open(&config_dir.path().join("test.db")).unwrap(),
            config_dir: config_dir.path().to_path_buf(),
            events: crate::events::EventHub::new(),
            pr_review_cache: Default::default(),
            usage_cache: Default::default(),
            http_client: reqwest::Client::new(),
            tts_base_url: "http://localhost".to_string(),
            stt_base_url: "http://localhost".to_string(),
        };

        let result = get_doc(
            State(state),
            Path(("test".to_string(), "nonexistent.md".to_string())),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_put_doc_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let ws_dir = config_dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                dir.path().to_string_lossy()
            ),
        )
        .unwrap();

        let state = AppState {
            db: crate::db::Db::open(&config_dir.path().join("test.db")).unwrap(),
            config_dir: config_dir.path().to_path_buf(),
            events: crate::events::EventHub::new(),
            pr_review_cache: Default::default(),
            usage_cache: Default::default(),
            http_client: reqwest::Client::new(),
            tts_base_url: "http://localhost".to_string(),
            stt_base_url: "http://localhost".to_string(),
        };

        let result = put_doc(
            State(state),
            Path(("test".to_string(), "new.md".to_string())),
            Json(PutDocBody {
                content: "# New Doc\nHello".to_string(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(result.0["ok"], true);

        let written = std::fs::read_to_string(dir.path().join(".apiari/docs/new.md")).unwrap();
        assert_eq!(written, "# New Doc\nHello");
    }

    #[tokio::test]
    async fn test_delete_doc_success() {
        let dir = tempfile::tempdir().unwrap();
        let docs_dir = dir.path().join(".apiari/docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("todelete.md"), "content").unwrap();

        let config_dir = tempfile::tempdir().unwrap();
        let ws_dir = config_dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                dir.path().to_string_lossy()
            ),
        )
        .unwrap();

        let state = AppState {
            db: crate::db::Db::open(&config_dir.path().join("test.db")).unwrap(),
            config_dir: config_dir.path().to_path_buf(),
            events: crate::events::EventHub::new(),
            pr_review_cache: Default::default(),
            usage_cache: Default::default(),
            http_client: reqwest::Client::new(),
            tts_base_url: "http://localhost".to_string(),
            stt_base_url: "http://localhost".to_string(),
        };

        let result = delete_doc(
            State(state),
            Path(("test".to_string(), "todelete.md".to_string())),
        )
        .await
        .unwrap();
        assert_eq!(result.0["ok"], true);
        assert!(!docs_dir.join("todelete.md").exists());
    }

    #[tokio::test]
    async fn test_delete_doc_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let ws_dir = config_dir.path().join("workspaces");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("test.toml"),
            format!(
                "[workspace]\nroot = \"{}\"\nname = \"test\"\n",
                dir.path().to_string_lossy()
            ),
        )
        .unwrap();

        let state = AppState {
            db: crate::db::Db::open(&config_dir.path().join("test.db")).unwrap(),
            config_dir: config_dir.path().to_path_buf(),
            events: crate::events::EventHub::new(),
            pr_review_cache: Default::default(),
            usage_cache: Default::default(),
            http_client: reqwest::Client::new(),
            tts_base_url: "http://localhost".to_string(),
            stt_base_url: "http://localhost".to_string(),
        };

        let result = delete_doc(
            State(state),
            Path(("test".to_string(), "nonexistent.md".to_string())),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_mime_type_detection() {
        assert_eq!(
            mime_guess::from_path("index.css")
                .first_or_octet_stream()
                .as_ref(),
            "text/css"
        );
        assert_eq!(
            mime_guess::from_path("app.js")
                .first_or_octet_stream()
                .as_ref(),
            "text/javascript"
        );
        assert_eq!(
            mime_guess::from_path("index.html")
                .first_or_octet_stream()
                .as_ref(),
            "text/html"
        );
    }
}
