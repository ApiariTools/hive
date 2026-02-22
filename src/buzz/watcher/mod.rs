//! Watcher trait, registry, and state persistence.
//!
//! Each watcher polls an external source and returns normalized [`Signal`]s.
//! Watcher cursors (e.g. "last seen issue ID") are persisted to
//! `.buzz/state.json` so that buzz does not replay old signals between runs.

pub mod github;
pub mod sentry;
pub mod webhook;

use std::collections::HashMap;
use std::path::Path;

use crate::signal::Signal;
use apiari_common::state::{load_state, save_state};
use async_trait::async_trait;
use color_eyre::Result;
use serde::{Deserialize, Serialize};

use super::config::BuzzConfig;

/// Default path for persisted watcher state.
const STATE_PATH: &str = ".buzz/state.json";

/// A pluggable source that can be polled for new signals.
#[async_trait]
pub trait Watcher: Send + Sync {
    /// Human-readable name for this watcher (used in logging and state keys).
    fn name(&self) -> &str;

    /// Poll the external source and return any new signals since the last poll.
    async fn poll(&mut self) -> Result<Vec<Signal>>;

    /// Return the current cursor value for state persistence, if any.
    fn cursor(&self) -> Option<String> {
        None
    }

    /// Restore cursor from persisted state.
    fn set_cursor(&mut self, _cursor: String) {}
}

/// Persisted state for all watchers — maps watcher name to its cursor string.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatcherState {
    /// Map of watcher name -> cursor value.
    pub cursors: HashMap<String, String>,
}

/// Create all enabled watchers based on the configuration.
/// Restores persisted cursors from `.buzz/state.json` if available.
pub fn create_watchers(config: &BuzzConfig) -> Vec<Box<dyn Watcher>> {
    let mut watchers: Vec<Box<dyn Watcher>> = Vec::new();

    if let Some(sentry_config) = &config.sentry {
        watchers.push(Box::new(sentry::SentryWatcher::new(sentry_config.clone())));
    }

    if let Some(github_config) = &config.github {
        watchers.push(Box::new(github::GithubWatcher::new(github_config.clone())));
    }

    if let Some(webhook_config) = &config.webhook {
        watchers.push(Box::new(webhook::WebhookWatcher::new(
            webhook_config.clone(),
        )));
    }

    // Restore persisted cursors.
    load_cursors(&mut watchers);

    watchers
}

/// Load persisted watcher state and restore cursors.
fn load_cursors(watchers: &mut [Box<dyn Watcher>]) {
    let state_path = Path::new(STATE_PATH);
    let state: WatcherState = match load_state(state_path) {
        Ok(state) => state,
        Err(e) => {
            eprintln!("[buzz] failed to load watcher state: {e}");
            WatcherState::default()
        }
    };

    if state.cursors.is_empty() {
        return;
    }

    for watcher in watchers.iter_mut() {
        if let Some(cursor) = state.cursors.get(watcher.name()) {
            eprintln!(
                "[buzz] restored cursor for '{}': {}",
                watcher.name(),
                cursor
            );
            watcher.set_cursor(cursor.clone());
        }
    }
}

/// Save all watcher cursors to `.buzz/state.json`.
pub fn save_cursors(watchers: &[Box<dyn Watcher>]) {
    let mut state = WatcherState::default();

    for watcher in watchers {
        if let Some(cursor) = watcher.cursor() {
            state.cursors.insert(watcher.name().to_string(), cursor);
        }
    }

    let state_path = Path::new(STATE_PATH);
    if let Err(e) = save_state(state_path, &state) {
        eprintln!("[buzz] failed to save watcher state: {e}");
    }
}
