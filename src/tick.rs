//! Unified tick engine — replaces independent pollers with a single coordinated loop.
//!
//! Each watcher implements the `Watcher` trait and fires at a configurable interval
//! (measured in ticks). The engine builds shared `TickContext` once per tick and
//! passes it to all due watchers.

use crate::db::Db;
use crate::pr_review::PrReviewCache;
use async_trait::async_trait;
use std::path::PathBuf;

/// Shared state built once per tick, passed to all watchers.
#[allow(dead_code)]
pub struct TickContext {
    pub tick_number: u64,
}

/// Actions watchers can request — executed by the engine after all watchers run.
#[allow(dead_code)]
pub enum Action {
    /// Log a system message to a bot's conversation
    LogBotMessage {
        workspace: String,
        bot: String,
        message: String,
    },
    /// Run a bot (proactive/scheduled)
    RunBot {
        workspace: String,
        bot: String,
        provider: String,
        model: Option<String>,
        role: String,
        proactive_prompt: String,
        working_dir: Option<PathBuf>,
        schedule_hours: u64,
        services: Vec<String>,
    },
}

/// Trait that all watchers implement.
#[async_trait]
pub trait Watcher: Send {
    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// How often to run: 1 = every tick, 2 = every other tick, etc.
    fn interval_ticks(&self) -> u64;

    /// Run one tick. Receives shared context, returns actions to execute.
    async fn tick(&mut self, ctx: &TickContext) -> Vec<Action>;
}

/// The engine: holds watchers, runs the loop.
pub struct TickEngine {
    watchers: Vec<Box<dyn Watcher>>,
    tick_interval_secs: u64,
    tick_number: u64,
}

impl TickEngine {
    pub fn new(tick_interval_secs: u64) -> Self {
        Self {
            watchers: Vec::new(),
            tick_interval_secs,
            tick_number: 0,
        }
    }

    pub fn add_watcher(&mut self, watcher: Box<dyn Watcher>) {
        self.watchers.push(watcher);
    }

    /// Start the tick loop. Call this with tokio::spawn.
    pub async fn run(mut self, db: Db) {
        loop {
            self.tick_number += 1;

            let ctx = TickContext {
                tick_number: self.tick_number,
            };

            // Run watchers that are due this tick
            let mut all_actions = Vec::new();
            for watcher in &mut self.watchers {
                if self.tick_number.is_multiple_of(watcher.interval_ticks()) {
                    let actions = watcher.tick(&ctx).await;
                    if !actions.is_empty() {
                        tracing::debug!("{} produced {} actions", watcher.name(), actions.len());
                    }
                    all_actions.extend(actions);
                }
            }

            // Execute actions
            for action in all_actions {
                execute_action(&action, &db).await;
            }

            tokio::time::sleep(std::time::Duration::from_secs(self.tick_interval_secs)).await;
        }
    }
}

async fn execute_action(action: &Action, db: &Db) {
    match action {
        Action::LogBotMessage {
            workspace,
            bot,
            message,
        } => {
            let _ = db.add_message(workspace, bot, "system", message, None);
        }
        Action::RunBot {
            workspace,
            bot,
            provider,
            model: _,
            role,
            proactive_prompt,
            working_dir,
            schedule_hours,
            services,
        } => {
            run_proactive_bot(
                workspace,
                bot,
                provider,
                role,
                proactive_prompt,
                working_dir,
                *schedule_hours,
                services,
                db,
            )
            .await;
        }
    }
}

async fn run_proactive_bot(
    workspace: &str,
    bot_name: &str,
    provider: &str,
    role: &str,
    proactive_prompt: &str,
    working_dir: &Option<PathBuf>,
    schedule_hours: u64,
    services: &[String],
    db: &Db,
) {
    use tracing::info;

    info!("[watcher] running proactive task for {}", bot_name);

    let report_path = format!("/tmp/hive-report-{}-{}.md", workspace, bot_name);
    let _ = std::fs::remove_file(&report_path);

    let _ = db.add_message(
        workspace,
        bot_name,
        "system",
        &format!("**Proactive check** — scheduled every {}h", schedule_hours),
        None,
    );

    let services_section = if !services.is_empty() {
        if let Some(dir) = working_dir {
            crate::routes::build_services_prompt(dir, services)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let full_prompt = format!(
        "You are {bot_name}, a specialty bot for the {workspace} workspace.\n\
         Your role: {role}\n\
         {services_section}\n\
         This is a scheduled proactive check. Do the following:\n\n\
         {proactive_prompt}\n\n\
         IMPORTANT: Do your research silently using tools. Do NOT narrate your process.\n\
         When you have your findings, publish your report using this command:\n\
         ```\n\
         hive publish --workspace {workspace} --bot {bot_name} --file /tmp/hive-report-{workspace}-{bot_name}.md\n\
         ```\n\
         First write your report to /tmp/hive-report-{workspace}-{bot_name}.md, then run the command above.\n\n\
         The report should be:\n\
         - Clean markdown, no narration\n\
         - Lead with the most important finding\n\
         - Use tables for structured data\n\
         - Short and scannable\n\n\
         After publishing, say DONE.",
    );

    let response = match provider {
        "codex" => crate::watcher::run_codex_autonomous(&full_prompt, working_dir).await,
        "gemini" => crate::watcher::run_gemini_autonomous(&full_prompt, working_dir).await,
        _ => crate::watcher::run_claude_autonomous(&full_prompt, working_dir).await,
    };

    let report = std::fs::read_to_string(&report_path).ok();
    let _ = std::fs::remove_file(&report_path);

    match (report, response) {
        (Some(text), _) if !text.trim().is_empty() => {
            let _ = db.add_message(workspace, bot_name, "assistant", text.trim(), None);
            info!(
                "[watcher] {} report published ({} chars)",
                bot_name,
                text.len()
            );
        }
        (_, Ok(text)) if !text.trim().is_empty() => {
            let _ = db.add_message(workspace, bot_name, "assistant", text.trim(), None);
            info!(
                "[watcher] {} fallback output ({} chars)",
                bot_name,
                text.len()
            );
        }
        (_, Err(e)) => {
            let _ = db.add_message(
                workspace,
                bot_name,
                "assistant",
                &format!("Proactive check failed: {e}"),
                None,
            );
        }
        _ => {
            let _ = db.add_message(
                workspace,
                bot_name,
                "assistant",
                "No notable findings this check.",
                None,
            );
        }
    }
}

// --- Watcher implementations ---

/// Watches for GitHub signals (failing CI on open PRs).
pub struct SignalWatcher {
    bots: Vec<crate::watcher::WatchedBot>,
    db: Db,
}

impl SignalWatcher {
    pub fn new(bots: Vec<crate::watcher::WatchedBot>, db: Db) -> Self {
        // Only keep bots that have watch sources
        let bots = bots.into_iter().filter(|b| !b.watch.is_empty()).collect();
        Self { bots, db }
    }
}

#[async_trait]
impl Watcher for SignalWatcher {
    fn name(&self) -> &str {
        "signal-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        4 // every 4th tick = ~60s at 15s base
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        // Run existing signal polling logic for each bot
        for bot in &self.bots {
            for source in &bot.watch {
                match source.as_str() {
                    "github" => {
                        if let Some(signal) = crate::watcher::poll_github(&bot.working_dir).await {
                            crate::watcher::dispatch_signal(bot, &self.db, &signal).await;
                        }
                    }
                    "sentry" => {
                        // placeholder
                    }
                    _ => {}
                }
            }
        }
        // Actions are handled directly (dispatch_signal writes to DB and runs bot)
        Vec::new()
    }
}

/// Watches for config/prompt file changes.
pub struct ConfigChangeWatcher {
    workspaces: Vec<crate::config_watcher::WatchedWorkspace>,
    hashes: std::collections::HashMap<(String, String), String>,
    initialized: bool,
}

impl ConfigChangeWatcher {
    pub fn new(workspaces: Vec<crate::config_watcher::WatchedWorkspace>) -> Self {
        Self {
            workspaces,
            hashes: std::collections::HashMap::new(),
            initialized: false,
        }
    }
}

#[async_trait]
impl Watcher for ConfigChangeWatcher {
    fn name(&self) -> &str {
        "config-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        2 // every 2nd tick = ~30s at 15s base
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        // On first tick, just capture initial hashes
        if !self.initialized {
            for ws in &self.workspaces {
                for bot in &ws.bots {
                    let hash = crate::config_watcher::compute_prompt_hash(ws, bot);
                    self.hashes.insert((ws.name.clone(), bot.clone()), hash);
                }
            }
            self.initialized = true;
            return Vec::new();
        }

        let mut actions = Vec::new();
        for ws in &self.workspaces {
            for bot in &ws.bots {
                let new_hash = crate::config_watcher::compute_prompt_hash(ws, bot);
                let key = (ws.name.clone(), bot.clone());

                if let Some(old_hash) = self.hashes.get(&key)
                    && *old_hash != new_hash
                {
                    tracing::info!(
                        "[config-watcher] prompt changed for {}/{}, resetting session",
                        ws.name,
                        bot
                    );
                    actions.push(Action::LogBotMessage {
                        workspace: ws.name.clone(),
                        bot: bot.clone(),
                        message: "Session reset — bot configuration was updated.".to_string(),
                    });
                }

                self.hashes.insert(key, new_hash);
            }
        }
        actions
    }
}

/// Polls GitHub GraphQL for PR review state.
pub struct PrReviewWatcher {
    cache: PrReviewCache,
    workspace_roots: Vec<PathBuf>,
}

impl PrReviewWatcher {
    pub fn new(cache: PrReviewCache, workspace_roots: Vec<PathBuf>) -> Self {
        Self {
            cache,
            workspace_roots,
        }
    }
}

#[async_trait]
impl Watcher for PrReviewWatcher {
    fn name(&self) -> &str {
        "pr-review-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        4 // every 4th tick = ~60s at 15s base
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        crate::pr_review::poll_once(&self.cache, &self.workspace_roots).await;
        Vec::new()
    }
}

/// Checks if scheduled/proactive bots need to run.
pub struct ScheduleWatcher {
    bots: Vec<crate::watcher::WatchedBot>,
    last_run: std::collections::HashMap<String, std::time::Instant>,
    startup: std::time::Instant,
}

impl ScheduleWatcher {
    pub fn new(bots: Vec<crate::watcher::WatchedBot>) -> Self {
        // Only keep bots that have schedule configured
        let bots = bots
            .into_iter()
            .filter(|b| b.schedule_hours.is_some() && b.proactive_prompt.is_some())
            .collect();
        Self {
            bots,
            last_run: std::collections::HashMap::new(),
            startup: std::time::Instant::now(),
        }
    }
}

#[async_trait]
impl Watcher for ScheduleWatcher {
    fn name(&self) -> &str {
        "schedule-watcher"
    }

    fn interval_ticks(&self) -> u64 {
        1 // every tick, but internally checks hours
    }

    async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
        let mut actions = Vec::new();
        let now = std::time::Instant::now();

        for bot in &self.bots {
            let hours = bot.schedule_hours.unwrap_or(24);
            let interval = std::time::Duration::from_secs(hours * 3600);
            let key = format!("{}/{}", bot.workspace, bot.name);

            let should_run = match self.last_run.get(&key) {
                Some(last) => now.duration_since(*last) >= interval,
                // Don't run on startup — wait for first interval
                None => now.duration_since(self.startup) >= interval,
            };

            if should_run {
                self.last_run.insert(key, now);
                actions.push(Action::RunBot {
                    workspace: bot.workspace.clone(),
                    bot: bot.name.clone(),
                    provider: bot.provider.clone(),
                    model: bot.model.clone(),
                    role: bot.role.clone(),
                    proactive_prompt: bot.proactive_prompt.clone().unwrap_or_default(),
                    working_dir: bot.working_dir.clone(),
                    schedule_hours: hours,
                    services: bot.services.clone(),
                });
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockWatcher {
        name: String,
        interval: u64,
        tick_count: u64,
    }

    impl MockWatcher {
        fn new(name: &str, interval: u64) -> Self {
            Self {
                name: name.to_string(),
                interval,
                tick_count: 0,
            }
        }
    }

    #[async_trait]
    impl Watcher for MockWatcher {
        fn name(&self) -> &str {
            &self.name
        }

        fn interval_ticks(&self) -> u64 {
            self.interval
        }

        async fn tick(&mut self, _ctx: &TickContext) -> Vec<Action> {
            self.tick_count += 1;
            Vec::new()
        }
    }

    #[test]
    fn test_tick_engine_fires_at_correct_intervals() {
        // Simulate the tick firing logic without actually running the async loop
        let mut engine = TickEngine::new(15);
        engine.add_watcher(Box::new(MockWatcher::new("every-tick", 1)));
        engine.add_watcher(Box::new(MockWatcher::new("every-2nd", 2)));
        engine.add_watcher(Box::new(MockWatcher::new("every-4th", 4)));

        // Simulate 8 ticks and count how many times each fires
        let mut fire_counts = vec![0u64; 3];
        for tick_num in 1..=8u64 {
            for (i, watcher) in engine.watchers.iter().enumerate() {
                if tick_num.is_multiple_of(watcher.interval_ticks()) {
                    fire_counts[i] += 1;
                }
            }
        }

        assert_eq!(fire_counts[0], 8); // every tick
        assert_eq!(fire_counts[1], 4); // every 2nd tick
        assert_eq!(fire_counts[2], 2); // every 4th tick
    }

    #[test]
    fn test_watcher_interval_2_only_fires_on_even_ticks() {
        let watcher = MockWatcher::new("even-only", 2);

        for tick_num in 1..=10u64 {
            let should_fire = tick_num.is_multiple_of(watcher.interval_ticks());
            if should_fire {
                assert_eq!(tick_num % 2, 0, "tick {tick_num} should be even");
            } else {
                assert_eq!(tick_num % 2, 1, "tick {tick_num} should be odd");
            }
        }
    }

    #[tokio::test]
    async fn test_mock_watcher_tick_increments() {
        let mut watcher = MockWatcher::new("test", 1);
        let ctx = TickContext { tick_number: 1 };

        assert_eq!(watcher.tick_count, 0);
        watcher.tick(&ctx).await;
        assert_eq!(watcher.tick_count, 1);
        watcher.tick(&ctx).await;
        assert_eq!(watcher.tick_count, 2);
    }

    #[test]
    fn test_tick_engine_new() {
        let engine = TickEngine::new(15);
        assert_eq!(engine.tick_interval_secs, 15);
        assert_eq!(engine.tick_number, 0);
        assert!(engine.watchers.is_empty());
    }

    #[test]
    fn test_tick_engine_add_watcher() {
        let mut engine = TickEngine::new(15);
        engine.add_watcher(Box::new(MockWatcher::new("test", 1)));
        assert_eq!(engine.watchers.len(), 1);
    }
}
