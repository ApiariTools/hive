use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::tick::{Action, TickContext, Watcher};

/// Cached usage data from `caut` CLI.
pub type UsageCache = Arc<Mutex<CachedUsage>>;

#[derive(Clone, Debug, Default)]
pub enum CachedUsage {
    #[default]
    Unknown,
    NotInstalled,
    Data(UsageData),
}

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub struct UsageData {
    pub providers: Vec<ProviderUsage>,
    pub updated_at: String,
}

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub struct ProviderUsage {
    pub name: String,
    pub status: String,
    pub usage_percent: Option<f64>,
    pub remaining: Option<String>,
    pub limit: Option<String>,
    pub resets_at: Option<String>,
    pub raw: serde_json::Value,
}

/// Fetch usage by running `caut usage --json` and parsing the output.
pub async fn fetch_usage() -> CachedUsage {
    let mut cmd = tokio::process::Command::new("caut");
    cmd.args(["usage", "--json"]);

    if std::env::var("CLAUDECODE").is_ok() {
        cmd.env_remove("GH_TOKEN");
    }

    let output = match cmd.output().await {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            tracing::debug!(
                "[usage] caut failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            return CachedUsage::NotInstalled;
        }
        Err(e) => {
            tracing::debug!("[usage] caut not found or failed to run: {e}");
            return CachedUsage::NotInstalled;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    let raw: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("[usage] Failed to parse caut output: {e}");
            return CachedUsage::NotInstalled;
        }
    };

    let mut providers = Vec::new();

    if let Some(obj) = raw.as_object() {
        for (name, data) in obj {
            if name == "version" || name == "timestamp" || name == "schema" {
                continue;
            }

            let status = if data.get("error").is_some() {
                "error".to_string()
            } else if data
                .get("rate_limited")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                "rate_limited".to_string()
            } else {
                "ok".to_string()
            };

            let usage_percent = data
                .get("usage_percent")
                .or_else(|| data.get("percent_used"))
                .and_then(|v| v.as_f64());

            let remaining = data
                .get("remaining")
                .or_else(|| data.get("remaining_text"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let limit = data
                .get("limit")
                .or_else(|| data.get("limit_text"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let resets_at = data
                .get("resets_at")
                .or_else(|| data.get("reset_time"))
                .and_then(|v| v.as_str())
                .map(String::from);

            providers.push(ProviderUsage {
                name: name.clone(),
                status,
                usage_percent,
                remaining,
                limit,
                resets_at,
                raw: data.clone(),
            });
        }
    }

    CachedUsage::Data(UsageData {
        providers,
        updated_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// Watcher that periodically fetches usage data from `caut`.
pub struct UsageWatcher {
    cache: UsageCache,
}

impl UsageWatcher {
    pub fn new(cache: UsageCache) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl Watcher for UsageWatcher {
    fn name(&self) -> &str {
        "usage-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        8 // every 8th tick = ~2 min at 15s base
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        let result = fetch_usage().await;
        let mut cache = self.cache.lock().await;
        *cache = result;
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_data_default() {
        let data = UsageData::default();
        assert!(data.providers.is_empty());
        assert!(data.updated_at.is_empty());
    }

    #[test]
    fn test_provider_usage_serialization() {
        let provider = ProviderUsage {
            name: "claude".to_string(),
            status: "ok".to_string(),
            usage_percent: Some(42.5),
            remaining: Some("57.5% remaining".to_string()),
            limit: Some("1000 requests".to_string()),
            resets_at: Some("2026-04-28T00:00:00Z".to_string()),
            raw: serde_json::json!({"test": true}),
        };
        let json = serde_json::to_string(&provider).unwrap();
        let deserialized: ProviderUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "claude");
        assert_eq!(deserialized.usage_percent, Some(42.5));
    }

    #[test]
    fn test_usage_cache_default_is_unknown() {
        let cache: UsageCache = Arc::new(Mutex::new(CachedUsage::default()));
        assert!(matches!(*cache.try_lock().unwrap(), CachedUsage::Unknown));
    }

    #[tokio::test]
    async fn test_fetch_usage_returns_not_installed_when_caut_missing() {
        let result = fetch_usage().await;
        assert!(matches!(result, CachedUsage::NotInstalled));
    }

    #[tokio::test]
    async fn test_usage_watcher_tick_with_no_caut() {
        let cache: UsageCache = Arc::new(Mutex::new(CachedUsage::default()));
        let mut watcher = UsageWatcher::new(cache.clone());
        let ctx = TickContext { tick_number: 1 };
        let actions = watcher.tick(&ctx).await;
        assert!(actions.is_empty());
        assert!(matches!(*cache.lock().await, CachedUsage::NotInstalled));
    }

    #[test]
    fn test_usage_watcher_interval() {
        let cache: UsageCache = Default::default();
        let watcher = UsageWatcher::new(cache);
        assert_eq!(watcher.name(), "usage-watcher");
        assert_eq!(watcher.interval_ticks(), 8);
    }
}
