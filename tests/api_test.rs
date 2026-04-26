use axum::body::Body;
use axum::http::{Request, StatusCode};
use hive::db::Db;
use hive::events::EventHub;
use tempfile::tempdir;
use tower::ServiceExt;

fn test_app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(config_dir.join("workspaces")).unwrap();

    let ws_root = dir.path().join("workspace");
    std::fs::create_dir_all(&ws_root).unwrap();
    std::fs::write(
        config_dir.join("workspaces/test.toml"),
        format!("[workspace]\nname = \"test\"\nroot = \"{}\"\n\n[[bots]]\nname = \"Customer\"\ncolor = \"#e85555\"\nrole = \"Test bot\"\n", ws_root.display()),
    )
    .unwrap();

    let db = Db::open(&config_dir.join("hive.db")).unwrap();
    let events = EventHub::new();
    let app = hive::routes::router(db, &config_dir, events, Default::default());
    (app, dir)
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, String) {
    let req = Request::builder().uri(path).body(Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

async fn post_json(app: &axum::Router, path: &str, json: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(json.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn test_list_workspaces() {
    let (app, _dir) = test_app();
    let (status, body) = get(&app, "/api/workspaces").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0]["name"], "test");
}

#[tokio::test]
async fn test_list_bots() {
    let (app, _dir) = test_app();
    let (status, body) = get(&app, "/api/workspaces/test/bots").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0]["name"], "Main");
    assert_eq!(parsed[1]["name"], "Customer");
}

#[tokio::test]
async fn test_list_bots_unknown_workspace() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/nonexistent/bots").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.len(), 1); // Just Main default
}

#[tokio::test]
async fn test_conversations_empty() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/test/conversations/Main").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(parsed.is_empty());
}

#[tokio::test]
async fn test_bot_status_default_idle() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/test/bots/Main/status").await;
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["status"], "idle");
}

#[tokio::test]
async fn test_unread_empty() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/test/unread").await;
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(parsed.as_object().unwrap().is_empty());
}

#[tokio::test]
async fn test_mark_seen() {
    let (app, _dir) = test_app();
    let (status, body) = post_json(&app, "/api/workspaces/test/seen/Main", "").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["ok"], true);
}

#[tokio::test]
async fn test_cancel_bot() {
    let (app, _dir) = test_app();
    let (status, body) = post_json(&app, "/api/workspaces/test/bots/Main/cancel", "").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["ok"], true);
}

#[tokio::test]
async fn test_workers_empty() {
    let (app, _dir) = test_app();
    let (_, body) = get(&app, "/api/workspaces/test/workers").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(parsed.is_empty());
}

#[tokio::test]
async fn test_repos() {
    let (app, _dir) = test_app();
    let (status, body) = get(&app, "/api/workspaces/test/repos").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(parsed.is_empty()); // temp dir has no git repos
}

#[tokio::test]
async fn test_worker_detail_not_found() {
    let (app, _dir) = test_app();
    let (status, _) = get(&app, "/api/workspaces/test/workers/nonexistent").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_send_message_stores_user_msg() {
    let (app, _dir) = test_app();
    let (status, _) = post_json(
        &app,
        "/api/workspaces/test/chat/Main",
        r#"{"message":"hello"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Give the background task a moment
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (_, body) = get(&app, "/api/workspaces/test/conversations/Main").await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(
        parsed
            .iter()
            .any(|m| m["content"] == "hello" && m["role"] == "user")
    );
}
