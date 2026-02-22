//! Sentry watcher — polls the Sentry API for unresolved issues.
//!
//! Uses the Sentry Web API to fetch unresolved issues for a given
//! organization/project, converting each into a Signal.
//!
//! The watcher tracks a `last_seen_id` cursor so that subsequent polls only
//! return issues newer than the most recent one already processed.

use crate::signal::{Severity, Signal};
use async_trait::async_trait;
use color_eyre::Result;

use super::Watcher;
use crate::buzz::config::SentryConfig;

/// Watches Sentry for new unresolved issues via the Sentry REST API.
pub struct SentryWatcher {
    config: SentryConfig,
    /// HTTP client, reused across polls for connection pooling.
    client: reqwest::Client,
    /// Cursor for incremental polling — the ID of the last seen issue.
    /// Issues are sorted by date descending, so we track the newest ID
    /// and skip everything at or before it on subsequent polls.
    last_seen_id: Option<String>,
}

impl SentryWatcher {
    pub fn new(config: SentryConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            last_seen_id: None,
        }
    }

    /// Map a Sentry issue level string to our severity enum.
    fn map_severity(level: &str) -> Severity {
        match level {
            "fatal" | "error" => Severity::Critical,
            "warning" => Severity::Warning,
            _ => Severity::Info,
        }
    }

    /// Fetch unresolved issues from the Sentry API and return them as Signals.
    async fn fetch_issues(&self) -> Result<Vec<serde_json::Value>> {
        let url = format!(
            "https://sentry.io/api/0/projects/{org}/{project}/issues/",
            org = self.config.org,
            project = self.config.project,
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .query(&[
                ("query", "is:unresolved"),
                ("sort", "date"),
                ("per_page", "25"),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            eprintln!(
                "[sentry] API returned {status} for {org}/{project}: {body}",
                org = self.config.org,
                project = self.config.project,
            );
            return Ok(Vec::new());
        }

        let issues: Vec<serde_json::Value> = response.json().await?;
        Ok(issues)
    }

    /// Convert a Sentry issue JSON object to a Signal.
    fn issue_to_signal(&self, issue: &serde_json::Value) -> Option<Signal> {
        let id = issue.get("id")?.as_str()?;
        let title = issue.get("title")?.as_str()?;
        let culprit = issue.get("culprit").and_then(|v| v.as_str()).unwrap_or("");
        let level = issue
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        let permalink = issue.get("permalink").and_then(|v| v.as_str());
        let count = issue.get("count").and_then(|v| v.as_str()).unwrap_or("0");

        let severity = Self::map_severity(level);

        let body = format!("{culprit}\nLevel: {level} | Events: {count}",);

        let mut signal = Signal::new(
            "sentry",
            severity,
            format!(
                "[{org}/{project}] {title}",
                org = self.config.org,
                project = self.config.project,
            ),
            body,
        )
        .with_dedup_key(format!("sentry-{id}"))
        .with_tags(["sentry", level]);

        if let Some(url) = permalink {
            signal = signal.with_url(url);
        }

        Some(signal)
    }
}

#[async_trait]
impl Watcher for SentryWatcher {
    fn name(&self) -> &str {
        "sentry"
    }

    fn cursor(&self) -> Option<String> {
        self.last_seen_id.clone()
    }

    fn set_cursor(&mut self, cursor: String) {
        self.last_seen_id = Some(cursor);
    }

    async fn poll(&mut self) -> Result<Vec<Signal>> {
        let issues = match self.fetch_issues().await {
            Ok(issues) => issues,
            Err(e) => {
                eprintln!(
                    "[sentry] failed to fetch issues for {org}/{project}: {e}",
                    org = self.config.org,
                    project = self.config.project,
                );
                return Ok(Vec::new());
            }
        };

        if issues.is_empty() {
            return Ok(Vec::new());
        }

        let mut signals = Vec::new();
        let mut new_last_seen_id: Option<String> = None;

        for issue in &issues {
            let id = match issue.get("id").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => continue,
            };

            // Track the newest ID (first in the list since sorted by date desc).
            if new_last_seen_id.is_none() {
                new_last_seen_id = Some(id.to_string());
            }

            // If we have a cursor, skip issues we've already seen.
            // Sentry IDs are numeric strings; once we hit our cursor, everything
            // after it is older, so we can stop.
            if let Some(ref cursor) = self.last_seen_id
                && id == cursor
            {
                break;
            }

            if let Some(signal) = self.issue_to_signal(issue) {
                signals.push(signal);
            }
        }

        // Update cursor to the newest issue we saw.
        if let Some(new_id) = new_last_seen_id {
            self.last_seen_id = Some(new_id);
        }

        Ok(signals)
    }
}
