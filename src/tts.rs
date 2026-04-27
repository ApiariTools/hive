use std::path::Path;
use tokio::process::Child;
use tracing::{info, warn};

/// Spawn the TTS Python server if the venv and script exist.
/// Returns the child handle (with `kill_on_drop`) so the caller can keep it alive.
pub async fn start_tts_server() -> Option<Child> {
    let venv_python = Path::new("tts/.venv/bin/python");
    let server_py = Path::new("tts/server.py");

    if !venv_python.exists() || !server_py.exists() {
        info!("TTS server not set up (run tts/setup.sh to enable)");
        return None;
    }

    let child = match tokio::process::Command::new(venv_python)
        .arg(server_py)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            warn!("Failed to start TTS server: {e}");
            return None;
        }
    };

    // Wait up to 5 seconds for the server to become ready
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if reqwest::get("http://127.0.0.1:4201/health").await.is_ok() {
            info!("TTS server started on :4201");
            return Some(child);
        }
    }

    warn!("TTS server spawned but health check never passed");
    Some(child)
}
