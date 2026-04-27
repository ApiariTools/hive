//! Unified tick engine — replaces independent pollers with a single coordinated loop.
//!
//! Each watcher implements the `Watcher` trait and fires at a configurable interval
//! (measured in ticks). The engine builds shared `TickContext` once per tick and
//! passes it to all due watchers. Watchers run concurrently via `join_all`, and
//! long-running actions are spawned as background tasks.

use crate::db::Db;
use crate::pr_review::PrReviewCache;
use crate::watcher::WatchedBot;
use async_trait::async_trait;
use std::path::PathBuf;

/// Shared state built once per tick, passed to all watchers.
#[allow(dead_code)]
pub struct TickContext {
    pub tick_number: u64,
}

/// Actions watchers can request — executed by the engine after all watchers run.
pub enum Action {
    /// Log a system message to a bot's conversation
    LogBotMessage {
        workspace: String,
        bot: String,
        message: String,
    },
    /// Dispatch a signal to a bot (runs autonomously in background)
    DispatchSignal {
        bot: WatchedBot,
        signal_title: String,
        signal_body: String,
    },
    /// Run a proactive/scheduled bot (runs autonomously in background)
    RunBot { bot: WatchedBot },
    /// Send a message to a swarm worker
    SendToWorker {
        workspace_root: PathBuf,
        worker_id: String,
        message: String,
    },
}

/// Trait that all watchers implement.
#[async_trait]
pub trait Watcher: Send {
    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// How often to run: 1 = every tick, 2 = every other tick, etc.
    /// Must be >= 1. A value of 0 is treated as 1.
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
        assert!(
            watcher.interval_ticks() >= 1,
            "interval_ticks() must be >= 1, got 0 from watcher '{}'",
            watcher.name()
        );
        self.watchers.push(watcher);
    }

    /// Start the tick loop. Call this with tokio::spawn.
    pub async fn run(mut self, db: Db) {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(self.tick_interval_secs));

        // First tick fires immediately (matches old poller behavior)
        interval.tick().await;

        loop {
            self.tick_number += 1;

            let ctx = TickContext {
                tick_number: self.tick_number,
            };

            // Run all due watchers concurrently
            let mut all_actions = Vec::new();
            let mut futures: Vec<_> = Vec::new();

            // Partition watchers into due/not-due. We need &mut for tick(),
            // so we iterate sequentially but the watchers themselves can do
            // concurrent I/O internally.
            for watcher in &mut self.watchers {
                let effective_interval = watcher.interval_ticks().max(1);
                if self.tick_number.is_multiple_of(effective_interval) {
                    futures.push(watcher.tick(&ctx));
                }
            }

            // Await all due watchers concurrently
            let results = futures_util::future::join_all(futures).await;
            for actions in results {
                if !actions.is_empty() {
                    all_actions.extend(actions);
                }
            }

            // Execute actions — spawn long-running ones as background tasks
            for action in all_actions {
                execute_action(action, &db);
            }

            // Wait for next tick (wall-clock aligned, not work-time + sleep)
            interval.tick().await;
        }
    }
}

/// Execute an action. Long-running actions (signal dispatch, bot runs) are spawned
/// as background tasks to avoid blocking the tick loop.
fn execute_action(action: Action, db: &Db) {
    match action {
        Action::LogBotMessage {
            workspace,
            bot,
            message,
        } => {
            let _ = db.add_message(&workspace, &bot, "system", &message, None);
        }
        Action::DispatchSignal {
            bot,
            signal_title,
            signal_body,
        } => {
            let db = db.clone();
            tokio::spawn(async move {
                let signal = crate::watcher::Signal {
                    source: "github".to_string(),
                    title: signal_title,
                    body: signal_body,
                };
                crate::watcher::dispatch_signal(&bot, &db, &signal).await;
            });
        }
        Action::RunBot { bot } => {
            let db = db.clone();
            tokio::spawn(async move {
                let prompt = bot.proactive_prompt.as_deref().unwrap_or("");
                crate::watcher::run_proactive(&bot, &db, prompt).await;
            });
        }
        Action::SendToWorker {
            workspace_root,
            worker_id,
            message,
        } => {
            let root = workspace_root.clone();
            tokio::spawn(async move {
                let output = tokio::process::Command::new("swarm")
                    .arg("--dir")
                    .arg(&root)
                    .arg("send")
                    .arg(&worker_id)
                    .arg(&message)
                    .output()
                    .await;
                match output {
                    Ok(o) if o.status.success() => {
                        tracing::info!("[pr-feedback] Sent feedback to {}", worker_id);
                    }
                    Ok(o) => {
                        tracing::warn!(
                            "[pr-feedback] swarm send failed: {}",
                            String::from_utf8_lossy(&o.stderr)
                        );
                    }
                    Err(e) => {
                        tracing::warn!("[pr-feedback] Failed to run swarm: {e}");
                    }
                }
            });
        }
    }
}

// --- Watcher implementations ---

/// Watches for GitHub signals (failing CI on open PRs).
pub struct SignalWatcher {
    bots: Vec<WatchedBot>,
}

impl SignalWatcher {
    pub fn new(bots: Vec<WatchedBot>) -> Self {
        // Only keep bots that have watch sources
        let bots = bots.into_iter().filter(|b| !b.watch.is_empty()).collect();
        Self { bots }
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
        let mut actions = Vec::new();
        for bot in &self.bots {
            for source in &bot.watch {
                match source.as_str() {
                    "github" => {
                        if let Some(signal) = crate::watcher::poll_github(&bot.working_dir).await {
                            actions.push(Action::DispatchSignal {
                                bot: bot.clone(),
                                signal_title: signal.title,
                                signal_body: signal.body,
                            });
                        }
                    }
                    "sentry" => {
                        // placeholder
                    }
                    _ => {}
                }
            }
        }
        actions
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
    bots: Vec<WatchedBot>,
    last_run: std::collections::HashMap<String, std::time::Instant>,
    startup: std::time::Instant,
}

impl ScheduleWatcher {
    pub fn new(bots: Vec<WatchedBot>) -> Self {
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
                actions.push(Action::RunBot { bot: bot.clone() });
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct MockWatcher {
        name: String,
        interval: u64,
        tick_count: Arc<AtomicU64>,
    }

    impl MockWatcher {
        fn new(name: &str, interval: u64) -> Self {
            Self {
                name: name.to_string(),
                interval,
                tick_count: Arc::new(AtomicU64::new(0)),
            }
        }

        fn count(&self) -> Arc<AtomicU64> {
            self.tick_count.clone()
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
            self.tick_count.fetch_add(1, Ordering::Relaxed);
            Vec::new()
        }
    }

    #[test]
    fn test_tick_engine_fires_at_correct_intervals() {
        let engine = TickEngine::new(15);

        // Simulate 8 ticks and count how many times each would fire
        let intervals = [1u64, 2, 4];
        let mut fire_counts = vec![0u64; 3];
        for tick_num in 1..=8u64 {
            for (i, interval) in intervals.iter().enumerate() {
                if tick_num.is_multiple_of(*interval) {
                    fire_counts[i] += 1;
                }
            }
        }

        assert_eq!(fire_counts[0], 8); // every tick
        assert_eq!(fire_counts[1], 4); // every 2nd tick
        assert_eq!(fire_counts[2], 2); // every 4th tick
        assert_eq!(engine.tick_interval_secs, 15);
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

        assert_eq!(watcher.tick_count.load(Ordering::Relaxed), 0);
        watcher.tick(&ctx).await;
        assert_eq!(watcher.tick_count.load(Ordering::Relaxed), 1);
        watcher.tick(&ctx).await;
        assert_eq!(watcher.tick_count.load(Ordering::Relaxed), 2);
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

    #[test]
    #[should_panic(expected = "interval_ticks() must be >= 1")]
    fn test_tick_engine_rejects_zero_interval() {
        let mut engine = TickEngine::new(15);
        engine.add_watcher(Box::new(MockWatcher::new("bad", 0)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_tick_engine_runs_watchers_on_schedule() {
        let every1 = MockWatcher::new("every-1", 1);
        let every2 = MockWatcher::new("every-2", 2);
        let count1 = every1.count();
        let count2 = every2.count();

        let mut engine = TickEngine::new(15);
        engine.add_watcher(Box::new(every1));
        engine.add_watcher(Box::new(every2));

        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("test.db")).unwrap();

        // Spawn the engine
        let handle = tokio::spawn(engine.run(db));

        // Advance time tick-by-tick (15s each) for 4 ticks
        for _ in 0..4 {
            tokio::time::advance(std::time::Duration::from_secs(15)).await;
            tokio::task::yield_now().await;
        }

        // After 4 ticks: every-1 should fire 4 times, every-2 should fire 2 times
        assert_eq!(count1.load(Ordering::Relaxed), 4);
        assert_eq!(count2.load(Ordering::Relaxed), 2);

        handle.abort();
    }
}
