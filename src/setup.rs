use color_eyre::Result;
use std::path::PathBuf;
use std::process::Command;

pub fn run() -> Result<()> {
    println!("\n=== Hive Voice Setup ===\n");

    // STT: whisper-cpp
    install_whisper();

    // STT: whisper model
    download_whisper_model();

    // TTS: Kokoro
    setup_tts();

    println!("\n=== Setup complete ===");
    Ok(())
}

fn install_whisper() {
    print!("[STT] whisper-cpp via brew... ");

    // Check if brew is available
    let brew_check = Command::new("brew").arg("--version").output();
    if brew_check.is_err() || !brew_check.unwrap().status.success() {
        println!("SKIPPED (brew not installed)");
        return;
    }

    // Check if already installed
    let list = Command::new("brew").args(["list", "whisper-cpp"]).output();
    if let Ok(output) = list
        && output.status.success()
    {
        println!("already installed");
        return;
    }

    // Install
    let result = Command::new("brew")
        .args(["install", "whisper-cpp"])
        .status();
    match result {
        Ok(s) if s.success() => println!("installed"),
        Ok(s) => println!("FAILED (exit code {})", s.code().unwrap_or(-1)),
        Err(e) => println!("FAILED ({})", e),
    }
}

fn download_whisper_model() {
    print!("[STT] whisper base.en model... ");

    let model_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".local/share/whisper");
    let model_path = model_dir.join("ggml-base.en.bin");

    if model_path.exists() {
        println!("already exists");
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&model_dir) {
        println!("FAILED (cannot create dir: {})", e);
        return;
    }

    let url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
    let result = Command::new("curl")
        .args(["-fSL", "--progress-bar", "-o"])
        .arg(&model_path)
        .arg(url)
        .status();

    match result {
        Ok(s) if s.success() => println!("downloaded"),
        Ok(s) => {
            // Clean up partial download
            let _ = std::fs::remove_file(&model_path);
            println!("FAILED (exit code {})", s.code().unwrap_or(-1));
        }
        Err(e) => println!("FAILED ({})", e),
    }
}

fn setup_tts() {
    println!("[TTS] Kokoro setup...");

    // Find the tts/ directory relative to the binary or current dir
    let tts_dir = find_tts_dir();
    let Some(tts_dir) = tts_dir else {
        println!("  SKIPPED (tts/ directory not found)");
        return;
    };

    let venv_dir = tts_dir.join(".venv");

    // Create venv if needed
    print!("  [TTS] python venv... ");
    if venv_dir.exists() {
        println!("already exists");
    } else {
        let result = Command::new("python3")
            .args(["-m", "venv"])
            .arg(&venv_dir)
            .status();
        match result {
            Ok(s) if s.success() => println!("created"),
            Ok(_) | Err(_) => {
                println!("FAILED");
                return;
            }
        }
    }

    // Install requirements
    print!("  [TTS] pip install requirements... ");
    let pip = venv_dir.join("bin/pip");
    let req_file = tts_dir.join("requirements.txt");
    let result = Command::new(&pip)
        .args(["install", "-r"])
        .arg(&req_file)
        .output();
    match result {
        Ok(output) if output.status.success() => println!("installed"),
        Ok(output) => {
            println!("FAILED");
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                eprintln!("    {}", stderr.lines().last().unwrap_or(""));
            }
            return;
        }
        Err(e) => {
            println!("FAILED ({})", e);
            return;
        }
    }

    // Download model
    print!("  [TTS] downloading Kokoro model... ");
    let python = venv_dir.join("bin/python3");
    let result = Command::new(&python)
        .args([
            "-c",
            "import kokoro_onnx; kokoro_onnx.Kokoro('kokoro-v1.0.onnx', 'voices-v1.0.bin'); print('ok')",
        ])
        .current_dir(&tts_dir)
        .output();
    match result {
        Ok(output) if output.status.success() => println!("done"),
        Ok(output) => {
            println!("FAILED");
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                eprintln!("    {}", stderr.lines().last().unwrap_or(""));
            }
        }
        Err(e) => println!("FAILED ({})", e),
    }
}

fn find_tts_dir() -> Option<PathBuf> {
    // Check relative to current executable
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let candidate = parent.join("tts");
        if candidate.join("requirements.txt").exists() {
            return Some(candidate);
        }
        // Check one level up (common for cargo builds: target/debug/hive)
        if let Some(grandparent) = parent.parent() {
            let candidate = grandparent.join("tts");
            if candidate.join("requirements.txt").exists() {
                return Some(candidate);
            }
            // target/debug -> target -> project root
            if let Some(root) = grandparent.parent() {
                let candidate = root.join("tts");
                if candidate.join("requirements.txt").exists() {
                    return Some(candidate);
                }
            }
        }
    }

    // Check relative to current working directory
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join("tts");
    if candidate.join("requirements.txt").exists() {
        return Some(candidate);
    }

    None
}

use super::dirs;
