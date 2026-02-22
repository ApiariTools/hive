//! Worker agent — autonomous Claude agents that execute individual tasks.
//!
//! Workers run inside swarm worktrees. Hive spawns them by calling the `swarm`
//! CLI as a subprocess and monitors their progress.

use crate::quest::Task;
use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

/// Status reported by a worker.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerStatus {
    /// Worker is starting up.
    Starting,
    /// Worker is actively working on the task.
    Running,
    /// Worker has finished the task.
    Complete,
    /// Worker encountered an error.
    Failed,
    /// Worker status is unknown (e.g., pane died).
    Unknown,
}

impl std::fmt::Display for WorkerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Complete => write!(f, "complete"),
            Self::Failed => write!(f, "failed"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Manages worker lifecycle through the swarm CLI.
pub struct Worker {
    /// Path to the swarm binary. Defaults to "swarm" (assumes it is on PATH).
    swarm_bin: String,
}

impl Worker {
    /// Create a new worker manager.
    pub fn new() -> Self {
        Self {
            swarm_bin: "swarm".to_owned(),
        }
    }

    /// Create a worker manager with a custom swarm binary path.
    #[allow(dead_code)]
    pub fn with_swarm_bin(swarm_bin: impl Into<String>) -> Self {
        Self {
            swarm_bin: swarm_bin.into(),
        }
    }

    /// Spawn a worker for the given task via `swarm create`.
    ///
    /// This launches a new swarm worktree with the task description as the
    /// prompt. The swarm CLI handles worktree creation, agent launch, and
    /// tmux pane management.
    pub async fn spawn_worker(&self, task: &Task) -> Result<String> {
        let prompt = format!("Task: {}\nQuest: {}", task.title, task.quest_id);

        let output = tokio::process::Command::new(&self.swarm_bin)
            .args(["create", &prompt])
            .output()
            .await
            .wrap_err("failed to run swarm create")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            color_eyre::eyre::bail!("swarm create failed: {stderr}");
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let worktree_id = stdout.trim().to_owned();
        Ok(worktree_id)
    }

    /// Poll the status of a worker by checking swarm state.
    ///
    /// Calls `swarm status --json` and looks for the worktree associated
    /// with the task.
    #[allow(dead_code)]
    pub async fn poll_worker_status(&self, task: &Task) -> Result<WorkerStatus> {
        let assigned = match &task.assigned_to {
            Some(id) => id,
            None => return Ok(WorkerStatus::Unknown),
        };

        let output = tokio::process::Command::new(&self.swarm_bin)
            .args(["status", "--json"])
            .output()
            .await
            .wrap_err("failed to run swarm status")?;

        if !output.status.success() {
            return Ok(WorkerStatus::Unknown);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse the swarm state to find our worktree.
        // The exact JSON shape depends on swarm's state format;
        // for now we do a simple check.
        if stdout.contains(assigned) {
            // If the worktree ID appears in the output, it is still tracked.
            // A more sophisticated check would parse the JSON and inspect
            // pane liveness, but this is sufficient as a stub.
            Ok(WorkerStatus::Running)
        } else {
            // Worktree no longer in swarm state — could be complete or cleaned up.
            Ok(WorkerStatus::Unknown)
        }
    }

    /// Send a message to a running worker via `swarm send`.
    #[allow(dead_code)]
    pub async fn send_message(&self, worktree_id: &str, message: &str) -> Result<()> {
        let output = tokio::process::Command::new(&self.swarm_bin)
            .args(["send", worktree_id, message])
            .output()
            .await
            .wrap_err("failed to run swarm send")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            color_eyre::eyre::bail!("swarm send failed: {stderr}");
        }

        Ok(())
    }

    /// Close a worker's worktree via `swarm close`.
    #[allow(dead_code)]
    pub async fn close_worker(&self, worktree_id: &str) -> Result<()> {
        let output = tokio::process::Command::new(&self.swarm_bin)
            .args(["close", worktree_id])
            .output()
            .await
            .wrap_err("failed to run swarm close")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            color_eyre::eyre::bail!("swarm close failed: {stderr}");
        }

        Ok(())
    }

    /// Merge a worker's changes via `swarm merge`.
    #[allow(dead_code)]
    pub async fn merge_worker(&self, worktree_id: &str) -> Result<()> {
        let output = tokio::process::Command::new(&self.swarm_bin)
            .args(["merge", worktree_id])
            .output()
            .await
            .wrap_err("failed to run swarm merge")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            color_eyre::eyre::bail!("swarm merge failed: {stderr}");
        }

        Ok(())
    }
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quest::{Task, TaskStatus};

    // ---- WorkerStatus Display ----

    #[test]
    fn worker_status_display() {
        assert_eq!(WorkerStatus::Starting.to_string(), "starting");
        assert_eq!(WorkerStatus::Running.to_string(), "running");
        assert_eq!(WorkerStatus::Complete.to_string(), "complete");
        assert_eq!(WorkerStatus::Failed.to_string(), "failed");
        assert_eq!(WorkerStatus::Unknown.to_string(), "unknown");
    }

    // ---- WorkerStatus serde ----

    #[test]
    fn worker_status_json_roundtrip() {
        let statuses = vec![
            WorkerStatus::Starting,
            WorkerStatus::Running,
            WorkerStatus::Complete,
            WorkerStatus::Failed,
            WorkerStatus::Unknown,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).unwrap();
            let back: WorkerStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, status);
        }
    }

    #[test]
    fn worker_status_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&WorkerStatus::Starting).unwrap(),
            "\"starting\""
        );
        assert_eq!(
            serde_json::to_string(&WorkerStatus::Failed).unwrap(),
            "\"failed\""
        );
    }

    #[test]
    fn worker_status_rejects_unknown_variant() {
        let result = serde_json::from_str::<WorkerStatus>("\"paused\"");
        assert!(result.is_err());
    }

    // ---- Worker construction ----

    #[test]
    fn worker_new_defaults_swarm_bin() {
        let worker = Worker::new();
        assert_eq!(worker.swarm_bin, "swarm");
    }

    #[test]
    fn worker_default_same_as_new() {
        let a = Worker::new();
        let b = Worker::default();
        assert_eq!(a.swarm_bin, b.swarm_bin);
    }

    #[test]
    fn worker_with_custom_swarm_bin() {
        let worker = Worker::with_swarm_bin("/usr/local/bin/swarm");
        assert_eq!(worker.swarm_bin, "/usr/local/bin/swarm");
    }

    #[test]
    fn worker_with_swarm_bin_accepts_string() {
        let worker = Worker::with_swarm_bin(String::from("my-swarm"));
        assert_eq!(worker.swarm_bin, "my-swarm");
    }

    // ---- spawn_worker prompt format ----

    fn make_task(title: &str, quest_id: &str) -> Task {
        Task {
            id: "task-1".into(),
            quest_id: quest_id.into(),
            title: title.into(),
            status: TaskStatus::Pending,
            assigned_to: None,
            branch: None,
        }
    }

    #[tokio::test]
    async fn spawn_worker_formats_prompt_correctly() {
        // Use a dummy binary that just echoes stdin/args — we use `echo` to
        // verify the command is built. The worker calls
        //   swarm create "<prompt>"
        // so we use `printf` as the "swarm" binary to capture the prompt arg.
        let worker = Worker::with_swarm_bin("echo");
        let task = make_task("Fix the login bug", "quest-42");
        let result = worker.spawn_worker(&task).await.unwrap();
        // `echo create "Task: Fix the login bug\nQuest: quest-42"` outputs:
        assert!(result.contains("create"));
        assert!(result.contains("Task: Fix the login bug"));
        assert!(result.contains("Quest: quest-42"));
    }

    #[tokio::test]
    async fn spawn_worker_returns_error_on_bad_binary() {
        let worker = Worker::with_swarm_bin("/nonexistent/binary");
        let task = make_task("test", "q-1");
        let result = worker.spawn_worker(&task).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn spawn_worker_returns_error_on_failure_exit() {
        let worker = Worker::with_swarm_bin("false");
        let task = make_task("test", "q-1");
        let result = worker.spawn_worker(&task).await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("swarm create failed"));
    }

    // ---- poll_worker_status ----

    #[tokio::test]
    async fn poll_status_returns_unknown_when_no_assignment() {
        let worker = Worker::new();
        let task = make_task("test", "q-1");
        // task.assigned_to is None
        let status = worker.poll_worker_status(&task).await.unwrap();
        assert_eq!(status, WorkerStatus::Unknown);
    }

    // ---- send / close / merge error on bad binary ----

    #[tokio::test]
    async fn send_message_error_on_bad_binary() {
        let worker = Worker::with_swarm_bin("/nonexistent/binary");
        let result = worker.send_message("wt-1", "hello").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn close_worker_error_on_bad_binary() {
        let worker = Worker::with_swarm_bin("/nonexistent/binary");
        let result = worker.close_worker("wt-1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn merge_worker_error_on_bad_binary() {
        let worker = Worker::with_swarm_bin("/nonexistent/binary");
        let result = worker.merge_worker("wt-1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn send_message_error_on_failure_exit() {
        let worker = Worker::with_swarm_bin("false");
        let result = worker.send_message("wt-1", "hello").await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("swarm send failed"));
    }

    #[tokio::test]
    async fn close_worker_error_on_failure_exit() {
        let worker = Worker::with_swarm_bin("false");
        let result = worker.close_worker("wt-1").await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("swarm close failed"));
    }

    #[tokio::test]
    async fn merge_worker_error_on_failure_exit() {
        let worker = Worker::with_swarm_bin("false");
        let result = worker.merge_worker("wt-1").await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("swarm merge failed"));
    }
}
