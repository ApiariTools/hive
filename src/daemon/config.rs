//! Daemon configuration loaded from `.hive/daemon.toml`.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Top-level daemon configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// Model to use for coordinator sessions (default: "sonnet").
    #[serde(default = "default_model")]
    pub model: String,

    /// After this many turns, nudge the user to consider /reset.
    #[serde(default = "default_nudge_threshold")]
    pub nudge_turn_threshold: u64,

    /// Path to buzz signals JSONL file.
    #[serde(default = "default_buzz_signals_path")]
    pub buzz_signals_path: PathBuf,

    /// Max agentic turns per message (tool-use rounds). Keeps responses snappy.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// How often to poll buzz signals (seconds).
    #[serde(default = "default_buzz_poll_interval")]
    pub buzz_poll_interval_secs: u64,

    /// User IDs allowed to interact with the bot. Empty = allow all users.
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,

    /// Chat IDs (groups/DMs) the bot will respond in. Empty = allow all chats.
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,

    /// Telegram bot configuration.
    pub telegram: TelegramConfig,
}

/// Telegram-specific configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramConfig {
    /// Bot API token from @BotFather.
    pub bot_token: String,

    /// Chat ID where buzz triage alerts are sent.
    pub alert_chat_id: i64,
}

fn default_model() -> String {
    "sonnet".into()
}

fn default_nudge_threshold() -> u64 {
    50
}

fn default_buzz_signals_path() -> PathBuf {
    PathBuf::from(".buzz/signals.jsonl")
}

fn default_max_turns() -> u32 {
    3
}

fn default_buzz_poll_interval() -> u64 {
    30
}

impl DaemonConfig {
    /// Load config from `.hive/daemon.toml` under the given workspace root.
    pub fn load(workspace_root: &Path) -> color_eyre::Result<Self> {
        let path = workspace_root.join(".hive/daemon.toml");
        let content = std::fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                color_eyre::eyre::eyre!(
                    "No daemon config found at {}\n\n\
                     To set up the Telegram bot:\n\
                     1. Message @BotFather on Telegram → /newbot\n\
                     2. Run: hive chat  (ask it to help set up the daemon)\n\
                     3. Or create .hive/daemon.toml manually:\n\n\
                     [telegram]\n\
                     bot_token = \"your-token-here\"\n\
                     alert_chat_id = 123456789\n",
                    path.display()
                )
            } else {
                color_eyre::eyre::eyre!("failed to read {}: {e}", path.display())
            }
        })?;
        let config: DaemonConfig = toml::from_str(&content)
            .map_err(|e| color_eyre::eyre::eyre!("failed to parse {}: {e}", path.display()))?;
        Ok(config)
    }

    /// Resolve the buzz signals path relative to the workspace root.
    pub fn resolved_buzz_path(&self, workspace_root: &Path) -> PathBuf {
        if self.buzz_signals_path.is_absolute() {
            self.buzz_signals_path.clone()
        } else {
            workspace_root.join(&self.buzz_signals_path)
        }
    }

    /// Check if a user ID is allowed to interact with the bot.
    #[allow(dead_code)]
    pub fn is_user_allowed(&self, user_id: i64) -> bool {
        self.allowed_user_ids.is_empty() || self.allowed_user_ids.contains(&user_id)
    }

    /// Check if a chat ID is allowed.
    pub fn is_chat_allowed(&self, chat_id: i64) -> bool {
        self.allowed_chat_ids.is_empty() || self.allowed_chat_ids.contains(&chat_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
model = "opus"
nudge_turn_threshold = 25
max_turns = 5
buzz_signals_path = "/tmp/signals.jsonl"
buzz_poll_interval_secs = 10
allowed_user_ids = [111, 222]
allowed_chat_ids = [-100111, -100222]

[telegram]
bot_token = "7000000000:AAxxxxxxxxxxxxxxxxx"
alert_chat_id = 123456789
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.model, "opus");
        assert_eq!(config.nudge_turn_threshold, 25);
        assert_eq!(config.max_turns, 5);
        assert_eq!(
            config.buzz_signals_path,
            PathBuf::from("/tmp/signals.jsonl")
        );
        assert_eq!(config.buzz_poll_interval_secs, 10);
        assert_eq!(config.allowed_user_ids, vec![111, 222]);
        assert_eq!(config.allowed_chat_ids, vec![-100111, -100222]);
        assert_eq!(config.telegram.bot_token, "7000000000:AAxxxxxxxxxxxxxxxxx");
        assert_eq!(config.telegram.alert_chat_id, 123456789);
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.model, "sonnet");
        assert_eq!(config.nudge_turn_threshold, 50);
        assert_eq!(config.max_turns, 3);
        assert_eq!(
            config.buzz_signals_path,
            PathBuf::from(".buzz/signals.jsonl")
        );
        assert_eq!(config.buzz_poll_interval_secs, 30);
        assert!(config.allowed_user_ids.is_empty());
    }

    #[test]
    fn test_is_user_allowed_empty_list() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.is_user_allowed(999));
    }

    #[test]
    fn test_is_user_allowed_restricted() {
        let config: DaemonConfig = toml::from_str(
            r#"
allowed_user_ids = [100, 200]
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.is_user_allowed(100));
        assert!(config.is_user_allowed(200));
        assert!(!config.is_user_allowed(300));
    }

    #[test]
    fn test_is_chat_allowed_empty_list() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.is_chat_allowed(-100123));
    }

    #[test]
    fn test_is_chat_allowed_restricted() {
        let config: DaemonConfig = toml::from_str(
            r#"
allowed_chat_ids = [-100111, -100222]
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        assert!(config.is_chat_allowed(-100111));
        assert!(config.is_chat_allowed(-100222));
        assert!(!config.is_chat_allowed(-100333));
    }

    #[test]
    fn test_resolved_buzz_path_relative() {
        let config: DaemonConfig = toml::from_str(
            r#"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        let resolved = config.resolved_buzz_path(Path::new("/workspace"));
        assert_eq!(resolved, PathBuf::from("/workspace/.buzz/signals.jsonl"));
    }

    #[test]
    fn test_resolved_buzz_path_absolute() {
        let config: DaemonConfig = toml::from_str(
            r#"
buzz_signals_path = "/abs/signals.jsonl"
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        )
        .unwrap();
        let resolved = config.resolved_buzz_path(Path::new("/workspace"));
        assert_eq!(resolved, PathBuf::from("/abs/signals.jsonl"));
    }

    #[test]
    fn test_reject_unknown_fields() {
        let result: Result<DaemonConfig, _> = toml::from_str(
            r#"
bogus_field = true
[telegram]
bot_token = "tok"
alert_chat_id = 42
"#,
        );
        assert!(result.is_err());
    }
}
