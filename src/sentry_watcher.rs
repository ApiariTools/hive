//! Sentry watcher — polls the Sentry Issues API for new unresolved issues.
//!
//! Each bot with `watch = ["sentry"]` and `services = ["sentry"]` gets polled
//! every 8 ticks (~2 minutes at 15s base). On first run, existing issues are
//! recorded as the cursor without triggering the bot.

use crate::db::Db;
use crate::tick::{Action, TickContext, Watcher};
use crate::watcher::WatchedBot;
use async_trait::async_trait;
use serde::Deserialize;
use tracing::warn;

/// A Sentry issue as returned by the Issues API (subset of fields).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SentryIssue {
    id: String,
    title: String,
    culprit: Option<String>,
    permalink: Option<String>,
    count: Option<String>,
    user_count: Option<u64>,
    first_seen: Option<String>,
    last_seen: Option<String>,
    level: Option<String>,
    metadata: Option<SentryMetadata>,
}

#[derive(Debug, Deserialize)]
struct SentryMetadata {
    #[serde(rename = "type")]
    error_type: Option<String>,
    value: Option<String>,
}

/// Sentry config parsed from `.apiari/services.toml`.
#[derive(Debug, Clone)]
struct SentryConfig {
    token: String,
    org: String,
    project: String,
}

/// Per-bot state for sentry polling.
struct SentryBotState {
    bot: WatchedBot,
    config: Option<SentryConfig>,
    initialized: bool,
    disabled: bool,
}

pub struct SentryWatcher {
    states: Vec<SentryBotState>,
    db: Db,
    client: reqwest::Client,
}

impl SentryWatcher {
    pub fn new(bots: Vec<WatchedBot>, db: Db) -> Self {
        let states = bots
            .into_iter()
            .filter(|b| b.watch.contains(&"sentry".to_string()))
            .map(|bot| {
                let config = load_sentry_config(&bot);
                SentryBotState {
                    bot,
                    config,
                    initialized: false,
                    disabled: false,
                }
            })
            .collect();

        Self {
            states,
            db,
            client: reqwest::Client::new(),
        }
    }
}

fn load_sentry_config(bot: &WatchedBot) -> Option<SentryConfig> {
    let root = bot.working_dir.as_ref()?;
    let services_path = root.join(".apiari/services.toml");
    let content = std::fs::read_to_string(&services_path).ok()?;
    let config: toml::Value = toml::from_str(&content).ok()?;
    let section = config.get("sentry")?.as_table()?;

    let token = section.get("token")?.as_str()?.to_string();
    let org = section.get("org")?.as_str()?.to_string();
    let project = section.get("project")?.as_str()?.to_string();

    if token.is_empty() || org.is_empty() || project.is_empty() {
        return None;
    }

    Some(SentryConfig {
        token,
        org,
        project,
    })
}

#[async_trait]
impl Watcher for SentryWatcher {
    fn name(&self) -> &str {
        "sentry-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        8 // 8 × 15s = 120s = 2 minutes
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        let mut actions = Vec::new();

        for state in &mut self.states {
            if state.disabled {
                continue;
            }

            let config = match &state.config {
                Some(c) => c.clone(),
                None => {
                    warn!(
                        "[sentry] no valid sentry config for {}/{}, disabling",
                        state.bot.workspace, state.bot.name
                    );
                    state.disabled = true;
                    continue;
                }
            };

            let url = format!(
                "https://sentry.io/api/0/projects/{}/{}/issues/?query=is:unresolved&sort=date&limit=25",
                config.org, config.project
            );

            let response = match self
                .client
                .get(&url)
                .header("Authorization", format!("Bearer {}", config.token))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        "[sentry] API error for {}/{}: {e}",
                        state.bot.workspace, state.bot.name
                    );
                    continue;
                }
            };

            // Check rate limiting
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                warn!(
                    "[sentry] rate limited for {}/{}, skipping",
                    state.bot.workspace, state.bot.name
                );
                continue;
            }

            if !response.status().is_success() {
                let status = response.status();
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    warn!(
                        "[sentry] auth failed for {}/{} ({}), disabling",
                        state.bot.workspace, state.bot.name, status
                    );
                    state.disabled = true;
                } else {
                    warn!(
                        "[sentry] API returned {} for {}/{}",
                        status, state.bot.workspace, state.bot.name
                    );
                }
                continue;
            }

            let issues: Vec<SentryIssue> = match response.json().await {
                Ok(i) => i,
                Err(e) => {
                    warn!(
                        "[sentry] failed to parse response for {}/{}: {e}",
                        state.bot.workspace, state.bot.name
                    );
                    continue;
                }
            };

            if issues.is_empty() {
                continue;
            }

            // Get cursor (last known issue ID)
            let cursor = match self
                .db
                .get_sentry_cursor(&state.bot.workspace, &state.bot.name)
            {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        "[sentry] failed to read cursor for {}/{}: {e}",
                        state.bot.workspace, state.bot.name
                    );
                    continue;
                }
            };

            if !state.initialized {
                // First run: record cursor, don't alert
                if let Some(first) = issues.first() {
                    let now = chrono::Utc::now().to_rfc3339();
                    if let Err(e) = self.db.set_sentry_cursor(
                        &state.bot.workspace,
                        &state.bot.name,
                        &first.id,
                        &now,
                    ) {
                        warn!("[sentry] failed to set initial cursor: {e}");
                    }
                }
                state.initialized = true;
                continue;
            }

            // Find new issues (those with IDs we haven't seen)
            let new_issues: Vec<&SentryIssue> = match &cursor {
                Some(last_id) => {
                    // Issues are sorted by date desc. Collect until we hit the last known ID.
                    issues
                        .iter()
                        .take_while(|issue| issue.id != *last_id)
                        .collect()
                }
                None => {
                    // No cursor but initialized — shouldn't happen, take first issue
                    issues.iter().take(1).collect()
                }
            };

            if new_issues.is_empty() {
                continue;
            }

            // Update cursor to newest issue
            if let Some(newest) = new_issues.first() {
                let now = chrono::Utc::now().to_rfc3339();
                if let Err(e) = self.db.set_sentry_cursor(
                    &state.bot.workspace,
                    &state.bot.name,
                    &newest.id,
                    &now,
                ) {
                    warn!("[sentry] failed to update cursor: {e}");
                }
            }

            // Dispatch each new issue as a signal
            for issue in &new_issues {
                let title = format!(
                    "[{}] {}",
                    issue.level.as_deref().unwrap_or("error").to_uppercase(),
                    issue.title
                );

                let mut body = String::new();
                body.push_str(&format!("**{}**\n\n", issue.title));

                if let Some(ref meta) = issue.metadata {
                    if let Some(ref t) = meta.error_type {
                        body.push_str(&format!("**Type:** {t}\n"));
                    }
                    if let Some(ref v) = meta.value {
                        body.push_str(&format!("**Value:** {v}\n"));
                    }
                }

                if let Some(ref culprit) = issue.culprit
                    && !culprit.is_empty()
                {
                    body.push_str(&format!("**Culprit:** {culprit}\n"));
                }

                body.push_str(&format!(
                    "**Level:** {}\n",
                    issue.level.as_deref().unwrap_or("error")
                ));
                body.push_str(&format!(
                    "**Events:** {}\n",
                    issue.count.as_deref().unwrap_or("0")
                ));
                body.push_str(&format!(
                    "**Users affected:** {}\n",
                    issue.user_count.unwrap_or(0)
                ));

                if let Some(ref first) = issue.first_seen {
                    body.push_str(&format!("**First seen:** {first}\n"));
                }
                if let Some(ref last) = issue.last_seen {
                    body.push_str(&format!("**Last seen:** {last}\n"));
                }
                if let Some(ref link) = issue.permalink {
                    body.push_str(&format!("\n[View in Sentry]({link})\n"));
                }

                actions.push(Action::DispatchSignal {
                    bot: state.bot.clone(),
                    signal_source: "sentry".to_string(),
                    signal_title: title,
                    signal_body: body,
                });
            }

            tracing::info!(
                "[sentry] found {} new issue(s) for {}/{}",
                new_issues.len(),
                state.bot.workspace,
                state.bot.name
            );
        }

        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_bot(services: Vec<String>) -> WatchedBot {
        WatchedBot {
            workspace: "test-ws".to_string(),
            name: "test-bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "sentry monitor".to_string(),
            watch: vec!["sentry".to_string()],
            working_dir: None,
            schedule: None,
            schedule_hours: None,
            proactive_prompt: None,
            services,
        }
    }

    #[test]
    fn test_sentry_watcher_interval() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);
        let watcher = SentryWatcher::new(vec![], db);
        assert_eq!(watcher.interval_ticks(), 8);
    }

    #[test]
    fn test_sentry_watcher_name() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);
        let watcher = SentryWatcher::new(vec![], db);
        assert_eq!(watcher.name(), "sentry-watcher");
    }

    #[test]
    fn test_sentry_watcher_filters_bots() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);

        let sentry_bot = make_test_bot(vec!["sentry".to_string()]);
        let github_bot = WatchedBot {
            watch: vec!["github".to_string()],
            ..make_test_bot(vec![])
        };

        let watcher = SentryWatcher::new(vec![sentry_bot, github_bot], db);
        assert_eq!(watcher.states.len(), 1);
    }

    #[test]
    fn test_load_sentry_config_from_services_toml() {
        let dir = tempfile::tempdir().unwrap();
        let apiari_dir = dir.path().join(".apiari");
        std::fs::create_dir_all(&apiari_dir).unwrap();
        std::fs::write(
            apiari_dir.join("services.toml"),
            r#"
[sentry]
token = "sntrys_test123"
org = "my-org"
project = "my-project"
"#,
        )
        .unwrap();

        let bot = WatchedBot {
            workspace: "test".to_string(),
            name: "bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "test".to_string(),
            watch: vec!["sentry".to_string()],
            working_dir: Some(dir.path().to_path_buf()),
            schedule: None,
            schedule_hours: None,
            proactive_prompt: None,
            services: vec!["sentry".to_string()],
        };

        let config = load_sentry_config(&bot).unwrap();
        assert_eq!(config.token, "sntrys_test123");
        assert_eq!(config.org, "my-org");
        assert_eq!(config.project, "my-project");
    }

    #[test]
    fn test_load_sentry_config_missing_file() {
        let bot = WatchedBot {
            workspace: "test".to_string(),
            name: "bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "test".to_string(),
            watch: vec!["sentry".to_string()],
            working_dir: Some(std::path::PathBuf::from("/nonexistent")),
            schedule: None,
            schedule_hours: None,
            proactive_prompt: None,
            services: vec!["sentry".to_string()],
        };

        assert!(load_sentry_config(&bot).is_none());
    }

    #[test]
    fn test_load_sentry_config_empty_fields() {
        let dir = tempfile::tempdir().unwrap();
        let apiari_dir = dir.path().join(".apiari");
        std::fs::create_dir_all(&apiari_dir).unwrap();
        std::fs::write(
            apiari_dir.join("services.toml"),
            r#"
[sentry]
token = ""
org = "my-org"
project = "my-project"
"#,
        )
        .unwrap();

        let bot = WatchedBot {
            workspace: "test".to_string(),
            name: "bot".to_string(),
            provider: "claude".to_string(),
            model: None,
            role: "test".to_string(),
            watch: vec!["sentry".to_string()],
            working_dir: Some(dir.path().to_path_buf()),
            schedule: None,
            schedule_hours: None,
            proactive_prompt: None,
            services: vec!["sentry".to_string()],
        };

        assert!(load_sentry_config(&bot).is_none());
    }

    #[test]
    fn test_sentry_cursor_db_operations() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);

        // Initially no cursor
        assert!(db.get_sentry_cursor("ws", "bot").unwrap().is_none());

        // Set and read back
        db.set_sentry_cursor("ws", "bot", "12345", "2026-04-29T09:00:00Z")
            .unwrap();
        let cursor = db.get_sentry_cursor("ws", "bot").unwrap();
        assert_eq!(cursor.as_deref(), Some("12345"));

        // Update
        db.set_sentry_cursor("ws", "bot", "67890", "2026-04-29T10:00:00Z")
            .unwrap();
        let cursor = db.get_sentry_cursor("ws", "bot").unwrap();
        assert_eq!(cursor.as_deref(), Some("67890"));
    }

    #[tokio::test]
    async fn test_sentry_watcher_first_tick_initializes_without_actions() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        crate::sentry_watcher::ensure_schema(&db);

        // Bot with no working_dir — config will be None, watcher disables it
        let bot = make_test_bot(vec!["sentry".to_string()]);
        let mut watcher = SentryWatcher::new(vec![bot], db);

        let ctx = TickContext { tick_number: 8 };
        let actions = watcher.tick(&ctx).await;
        // Should produce no actions (bot disabled due to missing config)
        assert!(actions.is_empty());
        assert!(watcher.states[0].disabled);
    }
}

/// Create the sentry_cursors table if it doesn't exist.
pub fn ensure_schema(db: &Db) {
    let _ = db.execute_batch(
        "CREATE TABLE IF NOT EXISTS sentry_cursors (
            workspace TEXT NOT NULL,
            bot TEXT NOT NULL,
            last_issue_id TEXT NOT NULL,
            last_poll_at TEXT NOT NULL,
            PRIMARY KEY (workspace, bot)
        )",
    );
}
