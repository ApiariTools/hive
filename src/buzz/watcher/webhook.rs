//! Webhook receiver — accepts inbound POST requests and converts them to signals.
//!
//! This is currently a stub implementation. A full version would:
//!
//!   1. Start an HTTP server on the configured port using `axum` (or similar).
//!   2. Accept POST requests with a JSON body conforming to the Signal schema.
//!   3. Buffer incoming signals in a `tokio::sync::mpsc` channel.
//!   4. Drain the buffer on each `poll()` call and return the accumulated signals.

use crate::signal::Signal;
use async_trait::async_trait;
use color_eyre::Result;

use super::Watcher;
use crate::buzz::config::WebhookConfig;

/// A webhook receiver that listens for inbound HTTP POSTs.
///
/// Currently a stub — `poll()` always returns an empty vector.
/// See module-level docs for the planned implementation.
pub struct WebhookWatcher {
    #[allow(dead_code)]
    config: WebhookConfig,
}

impl WebhookWatcher {
    pub fn new(config: WebhookConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Watcher for WebhookWatcher {
    fn name(&self) -> &str {
        "webhook"
    }

    async fn poll(&mut self) -> Result<Vec<Signal>> {
        // Stub: a real implementation would drain a buffer of signals received
        // via the HTTP server running in a background task.
        eprintln!(
            "[webhook] receiver on port {} not yet implemented",
            self.config.port
        );
        Ok(Vec::new())
    }
}
