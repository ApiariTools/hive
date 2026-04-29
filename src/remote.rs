//! Remote hive discovery, WebSocket bridging, and API proxying.
//!
//! When `~/.config/hive/remotes.toml` exists, hive connects to remote hive
//! instances, discovers their workspaces, bridges their WebSocket events into
//! the local EventHub, and exposes proxy routes so the frontend can talk to
//! remote workspaces transparently.

use crate::events::{EventHub, HiveEvent};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::Path as StdPath;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

// ── Config ──

#[derive(Debug, Clone, Deserialize)]
pub struct RemotesConfig {
    #[serde(default)]
    pub remotes: Vec<RemoteEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteEntry {
    pub name: String,
    pub url: String,
}

/// Validate that a remote name is safe for use in URL path segments and JSON.
fn is_valid_remote_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn load_remotes_config(config_dir: &StdPath) -> Vec<RemoteEntry> {
    let path = config_dir.join("remotes.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let config: RemotesConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            warn!("failed to parse remotes.toml: {e}");
            return Vec::new();
        }
    };
    config
        .remotes
        .into_iter()
        .filter(|r| {
            if !is_valid_remote_name(&r.name) {
                warn!(
                    "skipping remote with invalid name {:?} (must be alphanumeric, hyphens, underscores, max 64 chars)",
                    r.name
                );
                return false;
            }
            true
        })
        .collect()
}

// ── Shared state ──

#[derive(Debug, Clone, Serialize)]
pub struct RemoteState {
    pub name: String,
    pub url: String,
    pub online: bool,
    pub workspaces: Vec<String>,
}

pub type RemoteRegistry = Arc<RwLock<Vec<RemoteState>>>;

pub fn new_registry() -> RemoteRegistry {
    Arc::new(RwLock::new(Vec::new()))
}

// ── Discovery ──

pub fn spawn_discovery(
    registry: RemoteRegistry,
    remotes: Vec<RemoteEntry>,
    events: EventHub,
    http_client: reqwest::Client,
) {
    // Initialize all remotes synchronously so the registry is populated
    // before any discovery or proxy requests arrive.
    {
        let mut reg = registry.blocking_write();
        for remote in &remotes {
            reg.push(RemoteState {
                name: remote.name.clone(),
                url: remote.url.clone(),
                online: false,
                workspaces: Vec::new(),
            });
        }
    }

    for remote in remotes {
        let registry = registry.clone();
        let events = events.clone();
        let client = http_client.clone();

        // Spawn discovery poller
        let disc_registry = registry.clone();
        let disc_client = client.clone();
        let disc_remote = remote.clone();
        tokio::spawn(async move {
            loop {
                let result = discover_workspaces(&disc_client, &disc_remote.url).await;
                let (online, workspaces) = match result {
                    Ok(ws) => (true, ws),
                    Err(_) => (false, Vec::new()),
                };

                {
                    let mut reg = disc_registry.write().await;
                    if let Some(state) = reg.iter_mut().find(|s| s.name == disc_remote.name) {
                        let was_online = state.online;
                        state.online = online;
                        state.workspaces = workspaces;
                        if online && !was_online {
                            info!("[remote] {} came online", disc_remote.name);
                        } else if !online && was_online {
                            warn!("[remote] {} went offline", disc_remote.name);
                        }
                    }
                }

                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });

        // Spawn WS bridge
        tokio::spawn(ws_bridge(remote, events, client));
    }
}

async fn discover_workspaces(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<Vec<String>, reqwest::Error> {
    #[derive(Deserialize)]
    struct WsInfo {
        name: String,
    }

    let url = format!("{base_url}/api/workspaces");
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?;
    let workspaces: Vec<WsInfo> = resp.json().await?;
    Ok(workspaces.into_iter().map(|w| w.name).collect())
}

// ── WebSocket bridge ──

async fn ws_bridge(remote: RemoteEntry, events: EventHub, _client: reqwest::Client) {
    let mut backoff = 1u64;

    loop {
        let ws_url = remote
            .url
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        let ws_url = format!("{ws_url}/ws");

        match tokio_tungstenite::connect_async(&ws_url).await {
            Ok((ws_stream, _)) => {
                info!("[remote] WS connected to {}", remote.name);
                backoff = 1; // reset on successful connect

                let (_write, mut read) = ws_stream.split();
                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            // Parse the remote event and re-broadcast with remote field
                            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text)
                            {
                                // Tag with remote name
                                if let Some(obj) = value.as_object_mut() {
                                    obj.insert(
                                        "remote".to_string(),
                                        serde_json::Value::String(remote.name.clone()),
                                    );
                                }
                                // Re-broadcast as a raw JSON event via EventHub
                                // We use the RawRemoteEvent variant for this
                                let workspace = value
                                    .get("workspace")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let bot = value
                                    .get("bot")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let event_type = value
                                    .get("type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                events.send(HiveEvent::RemoteEvent {
                                    remote: remote.name.clone(),
                                    workspace,
                                    bot,
                                    event_type,
                                    raw_json: serde_json::to_string(&value).unwrap_or_default(),
                                });
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
                        Err(e) => {
                            warn!("[remote] WS error from {}: {e}", remote.name);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                warn!("[remote] WS connect failed to {}: {e}", remote.name);
            }
        }

        // Exponential backoff: 1s, 2s, 4s, 8s, ... max 30s
        tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

// ── Helper to get remote workspaces for the unified workspace list ──

pub async fn get_remote_workspaces(registry: &RemoteRegistry) -> Vec<(String, String)> {
    let reg = registry.read().await;
    let mut result = Vec::new();
    for remote in reg.iter() {
        if remote.online {
            for ws in &remote.workspaces {
                result.push((ws.clone(), remote.name.clone()));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let remotes = load_remotes_config(dir.path());
        assert!(remotes.is_empty());
    }

    #[test]
    fn test_load_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = r#"
[[remotes]]
name = "mini-office"
url = "http://100.64.0.2:4200"

[[remotes]]
name = "mini-home"
url = "http://100.64.0.3:4200"
"#;
        std::fs::write(dir.path().join("remotes.toml"), config).unwrap();
        let remotes = load_remotes_config(dir.path());
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[0].name, "mini-office");
        assert_eq!(remotes[1].url, "http://100.64.0.3:4200");
    }

    #[test]
    fn test_load_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("remotes.toml"), "not valid toml {{{{").unwrap();
        let remotes = load_remotes_config(dir.path());
        assert!(remotes.is_empty());
    }

    #[test]
    fn test_valid_remote_names() {
        assert!(is_valid_remote_name("mini-office"));
        assert!(is_valid_remote_name("mini_home"));
        assert!(is_valid_remote_name("server1"));
        assert!(!is_valid_remote_name(""));
        assert!(!is_valid_remote_name("has spaces"));
        assert!(!is_valid_remote_name("path/traversal"));
        assert!(!is_valid_remote_name("special!chars"));
        assert!(!is_valid_remote_name(&"a".repeat(65)));
    }

    #[test]
    fn test_load_config_skips_invalid_names() {
        let dir = tempfile::tempdir().unwrap();
        let config = r#"
[[remotes]]
name = "valid-name"
url = "http://10.0.0.1:4200"

[[remotes]]
name = "has spaces"
url = "http://10.0.0.2:4200"

[[remotes]]
name = "also/invalid"
url = "http://10.0.0.3:4200"
"#;
        std::fs::write(dir.path().join("remotes.toml"), config).unwrap();
        let remotes = load_remotes_config(dir.path());
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].name, "valid-name");
    }

    #[test]
    fn test_new_registry() {
        let reg = new_registry();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let r = reg.read().await;
            assert!(r.is_empty());
        });
    }

    #[test]
    fn test_get_remote_workspaces() {
        let registry = new_registry();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            {
                let mut reg = registry.write().await;
                reg.push(RemoteState {
                    name: "mini".to_string(),
                    url: "http://10.0.0.1:4200".to_string(),
                    online: true,
                    workspaces: vec!["proj-a".to_string(), "proj-b".to_string()],
                });
                reg.push(RemoteState {
                    name: "offline".to_string(),
                    url: "http://10.0.0.2:4200".to_string(),
                    online: false,
                    workspaces: vec!["proj-c".to_string()],
                });
            }
            let ws = get_remote_workspaces(&registry).await;
            assert_eq!(ws.len(), 2);
            assert_eq!(ws[0], ("proj-a".to_string(), "mini".to_string()));
            assert_eq!(ws[1], ("proj-b".to_string(), "mini".to_string()));
        });
    }
}
