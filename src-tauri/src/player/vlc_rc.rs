use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, warn};

use super::backend::PlayerBackend;
use super::properties::PlayerState;

const VLC_ARGS: &[&str] = &["--extraintf", "rc", "--rc-fake-tty", "--quiet"];

pub struct VlcBackend {
    stdin: Arc<TokioMutex<ChildStdin>>,
    state: Arc<Mutex<PlayerState>>,
    last_loaded: Arc<Mutex<Option<String>>>,
}

impl VlcBackend {
    pub async fn start(
        player_path: &str,
        args: &[String],
        initial_file: Option<&str>,
    ) -> anyhow::Result<(Self, Child)> {
        let mut cmd = Command::new(player_path);
        cmd.args(VLC_ARGS);
        cmd.args(args);
        if let Some(path) = initial_file {
            cmd.arg(path);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = cmd.spawn().context("Failed to start VLC")?;
        let stdin = child
            .stdin
            .take()
            .context("Failed to capture VLC stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to capture VLC stdout")?;

        let state = Arc::new(Mutex::new(PlayerState::default()));
        let last_loaded = Arc::new(Mutex::new(initial_file.map(|s| s.to_string())));
        let state_clone = state.clone();
        let last_loaded_clone = last_loaded.clone();

        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                handle_line(&state_clone, &last_loaded_clone, &line);
            }
        });

        let backend = Self {
            stdin: Arc::new(TokioMutex::new(stdin)),
            state,
            last_loaded,
        };

        Ok((backend, child))
    }

    async fn send_command(&self, command: &str) -> anyhow::Result<()> {
        let mut guard = self.stdin.lock().await;
        guard
            .write_all(format!("{}\n", command).as_bytes())
            .await
            .context("Failed to write to VLC")?;
        guard.flush().await.context("Failed to flush VLC")?;
        Ok(())
    }

}

fn handle_line(state: &Arc<Mutex<PlayerState>>, last_loaded: &Arc<Mutex<Option<String>>>, line: &str) {
    debug!("vlc >> {}", line);
    let trimmed = line.trim();
    if let Some(value) = trimmed.strip_prefix("time:") {
        state.lock().position = value.trim().parse::<f64>().ok();
        return;
    }
    if let Some(value) = trimmed.strip_prefix("length:") {
        state.lock().duration = value.trim().parse::<f64>().ok();
        return;
    }
    if let Some(value) = trimmed.strip_prefix("state ") {
        match value.trim() {
            "playing" => state.lock().paused = Some(false),
            "paused" | "stopped" => state.lock().paused = Some(true),
            _ => {}
        }
        return;
    }
    if let Some(value) = trimmed.strip_prefix("state:") {
        match value.trim() {
            "playing" => state.lock().paused = Some(false),
            "paused" | "stopped" => state.lock().paused = Some(true),
            _ => {}
        }
        return;
    }
    if let Some(value) = trimmed.strip_prefix("rate:") {
        state.lock().speed = value.trim().parse::<f64>().ok();
        return;
    }
    if let Some(value) = trimmed.strip_prefix("file:") {
        let value = value.trim();
        let filename = if let Some(name) = std::path::Path::new(value)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        {
            name
        } else {
            value.to_string()
        };
        let mut state_guard = state.lock();
        state_guard.path = Some(value.to_string());
        state_guard.filename = Some(filename.clone());
        *last_loaded.lock() = Some(value.to_string());
        return;
    }
}

#[async_trait]
impl PlayerBackend for VlcBackend {
    fn kind(&self) -> super::backend::PlayerKind {
        super::backend::PlayerKind::Vlc
    }

    fn name(&self) -> &'static str {
        "VLC"
    }

    fn get_state(&self) -> PlayerState {
        self.state.lock().clone()
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        if let Err(e) = self.send_command("status").await {
            warn!("Failed to query status: {}", e);
        }
        if let Err(e) = self.send_command("get_meta filename").await {
            warn!("Failed to query filename: {}", e);
        }
        Ok(())
    }

    async fn set_position(&self, position: f64) -> anyhow::Result<()> {
        self.send_command(&format!("seek {}", position)).await
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        let current = self.state.lock().paused.unwrap_or(false);
        if paused && !current {
            self.send_command("pause").await
        } else if !paused && current {
            self.send_command("play").await
        } else {
            Ok(())
        }
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        self.send_command(&format!("rate {}", speed)).await
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        *self.last_loaded.lock() = Some(path.to_string());
        self.send_command(&format!("add {}", path)).await
    }

    fn show_osd(&self, text: &str, _duration_ms: Option<u64>) -> anyhow::Result<()> {
        let message = text.replace('"', "'");
        let stdin = self.stdin.clone();
        tokio::spawn(async move {
            let mut guard = stdin.lock().await;
            let _ = guard
                .write_all(format!("display {}\n", message).as_bytes())
                .await;
            let _ = guard.flush().await;
        });
        Ok(())
    }
}
