use axum::{
    extract::WebSocketUpgrade,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{error, warn};

/// Default iPhone 16 Pro logical resolution.
const SIM_WIDTH: f64 = 393.0;
const SIM_HEIGHT: f64 = 852.0;

const FRAME_PATH: &str = "/tmp/hive_sim_frame.jpg";

// ── Status endpoint ──

#[derive(Serialize)]
pub struct SimulatorStatus {
    pub booted: bool,
    pub device: Option<String>,
    pub udid: Option<String>,
}

pub async fn simulator_status() -> Json<SimulatorStatus> {
    match booted_device().await {
        Some((name, udid)) => Json(SimulatorStatus {
            booted: true,
            device: Some(name),
            udid: Some(udid),
        }),
        None => Json(SimulatorStatus {
            booted: false,
            device: None,
            udid: None,
        }),
    }
}

async fn booted_device() -> Option<(String, String)> {
    let output = Command::new("xcrun")
        .args(["simctl", "list", "devices", "booted", "-j"])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let devices = json.get("devices")?.as_object()?;

    for (_runtime, list) in devices {
        if let Some(arr) = list.as_array() {
            for dev in arr {
                if dev.get("state")?.as_str()? == "Booted" {
                    let name = dev.get("name")?.as_str()?.to_string();
                    let udid = dev.get("udid")?.as_str()?.to_string();
                    return Some((name, udid));
                }
            }
        }
    }
    None
}

// ── WebSocket streaming ──

#[derive(Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum InputEvent {
    Tap {
        x: f64,
        y: f64,
    },
    Swipe {
        #[serde(rename = "fromX")]
        from_x: f64,
        #[serde(rename = "fromY")]
        from_y: f64,
        #[serde(rename = "toX")]
        to_x: f64,
        #[serde(rename = "toY")]
        to_y: f64,
    },
    Type {
        text: String,
    },
    Key {
        key: String,
    },
}

pub async fn simulator_ws(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_simulator_ws)
}

async fn handle_simulator_ws(mut socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;

    // Check that a simulator is booted before starting the frame loop
    if booted_device().await.is_none() {
        let _ = socket
            .send(Message::Text(
                r#"{"error":"No simulator booted"}"#.to_string().into(),
            ))
            .await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    }

    let (mut sender, mut receiver) = {
        use futures_util::StreamExt;
        socket.split()
    };

    // Frame capture task
    let frame_task = tokio::spawn(async move {
        use futures_util::SinkExt;

        loop {
            // Capture screenshot
            let result = Command::new("xcrun")
                .args([
                    "simctl",
                    "io",
                    "booted",
                    "screenshot",
                    "--type=jpeg",
                    FRAME_PATH,
                ])
                .output()
                .await;

            match result {
                Ok(out) if out.status.success() => {
                    if let Ok(bytes) = tokio::fs::read(FRAME_PATH).await
                        && sender.send(Message::Binary(bytes.into())).await.is_err()
                    {
                        break;
                    }
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    warn!("simctl screenshot failed: {stderr}");
                }
                Err(e) => {
                    error!("Failed to spawn simctl: {e}");
                    break;
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(66)).await;
        }
    });

    // Input handling task
    let input_task = tokio::spawn(async move {
        use futures_util::StreamExt;

        while let Some(Ok(msg)) = receiver.next().await {
            let text = match msg {
                Message::Text(t) => t.to_string(),
                Message::Close(_) => break,
                _ => continue,
            };

            let event: InputEvent = match serde_json::from_str(&text) {
                Ok(e) => e,
                Err(e) => {
                    warn!("Invalid input event: {e}");
                    continue;
                }
            };

            if let Err(e) = dispatch_input(event).await {
                warn!("Input dispatch failed: {e}");
            }
        }
    });

    // Wait for either task to finish, then abort the other
    tokio::select! {
        _ = frame_task => {}
        _ = input_task => {}
    }
}

async fn dispatch_input(event: InputEvent) -> Result<(), String> {
    match event {
        InputEvent::Tap { x, y } => {
            let sx = (x * SIM_WIDTH) as i32;
            let sy = (y * SIM_HEIGHT) as i32;
            run_cmd(
                "axe",
                &[
                    "tap",
                    "-x",
                    &sx.to_string(),
                    "-y",
                    &sy.to_string(),
                    "--udid",
                    "booted",
                ],
            )
            .await
        }
        InputEvent::Swipe {
            from_x,
            from_y,
            to_x,
            to_y,
        } => {
            let x1 = (from_x * SIM_WIDTH) as i32;
            let y1 = (from_y * SIM_HEIGHT) as i32;
            let x2 = (to_x * SIM_WIDTH) as i32;
            let y2 = (to_y * SIM_HEIGHT) as i32;
            run_cmd(
                "axe",
                &[
                    "swipe",
                    "--from-x",
                    &x1.to_string(),
                    "--from-y",
                    &y1.to_string(),
                    "--to-x",
                    &x2.to_string(),
                    "--to-y",
                    &y2.to_string(),
                    "--udid",
                    "booted",
                ],
            )
            .await
        }
        InputEvent::Type { ref text } => run_cmd("axe", &["type", text, "--udid", "booted"]).await,
        InputEvent::Key { ref key } => run_cmd("axe", &["key", key, "--udid", "booted"]).await,
    }
}

async fn run_cmd(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("Failed to spawn {program}: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{program} failed: {stderr}"));
    }
    Ok(())
}
