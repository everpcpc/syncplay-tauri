use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info, warn};

use super::backend::PlayerBackend;
use super::properties::PlayerState;

const VLC_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
const VLC_ARGS: &[&str] = &["--extraintf", "rc", "--rc-fake-tty", "--quiet"];

pub struct VlcBackend {
    stdin: Arc<TokioMutex<Option<ChildStdin>>>,
    state: Arc<Mutex<PlayerState>>,
    last_loaded: Arc<Mutex<Option<String>>>,
    connected: Arc<AtomicBool>,
}

impl VlcBackend {
    pub async fn start(
        player_path: &str,
        args: &[String],
        initial_file: Option<&str>,
    ) -> anyhow::Result<(Self, Child)> {
        info!(
            "Starting player: kind=Vlc, path={}, args={:?}, initial_file={:?}",
            player_path, args, initial_file
        );
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
        let stdin = child.stdin.take().context("Failed to capture VLC stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to capture VLC stdout")?;

        let state = Arc::new(Mutex::new(PlayerState::default()));
        let last_loaded = Arc::new(Mutex::new(initial_file.map(|s| s.to_string())));
        let state_clone = state.clone();
        let last_loaded_clone = last_loaded.clone();
        let connected = Arc::new(AtomicBool::new(true));
        let connected_clone = connected.clone();

        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                handle_line(&state_clone, &last_loaded_clone, &line);
            }
            connected_clone.store(false, Ordering::SeqCst);
        });

        let backend = Self {
            stdin: Arc::new(TokioMutex::new(Some(stdin))),
            state,
            last_loaded,
            connected,
        };

        Ok((backend, child))
    }

    async fn send_command(&self, command: &str) -> anyhow::Result<()> {
        if !self.connected.load(Ordering::SeqCst) {
            anyhow::bail!("VLC RC pipe is disconnected");
        }
        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            self.connected.store(false, Ordering::SeqCst);
            anyhow::bail!("VLC RC pipe is disconnected");
        };
        match tokio::time::timeout(VLC_COMMAND_TIMEOUT, async {
            stdin.write_all(format!("{}\n", command).as_bytes()).await?;
            stdin.flush().await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                self.connected.store(false, Ordering::SeqCst);
                *guard = None;
                Err(e.into())
            }
            Err(_) => {
                self.connected.store(false, Ordering::SeqCst);
                *guard = None;
                anyhow::bail!("Timed out writing to VLC RC pipe")
            }
        }
    }

    async fn close(&self) -> anyhow::Result<()> {
        self.connected.store(false, Ordering::SeqCst);
        let mut guard = self.stdin.lock().await;
        if let Some(mut stdin) = guard.take() {
            let _ = tokio::time::timeout(VLC_COMMAND_TIMEOUT, async {
                let _ = stdin.write_all(b"quit\n").await;
                stdin.shutdown().await
            })
            .await;
        }
        Ok(())
    }
}

fn handle_line(
    state: &Arc<Mutex<PlayerState>>,
    last_loaded: &Arc<Mutex<Option<String>>>,
    line: &str,
) {
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
            if let Some(stdin) = guard.as_mut() {
                let _ = stdin
                    .write_all(format!("display {}\n", message).as_bytes())
                    .await;
                let _ = stdin.flush().await;
            }
        });
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.close().await
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}
