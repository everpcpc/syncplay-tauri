use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info, warn};

use super::backend::{PlayerBackend, PlayerKind};
use super::properties::PlayerState;

const MPLAYER_ARGS: &[&str] = &[
    "-slave",
    "-idle",
    "-quiet",
    "-nomsgcolor",
    "-msglevel",
    "all=1:global=4:cplayer=4",
    "-af-add",
    "scaletempo",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ResponseKey {
    Position,
    Duration,
    Filename,
    Path,
    Pause,
    Speed,
}

pub struct MplayerBackend {
    kind: PlayerKind,
    stdin: Arc<TokioMutex<ChildStdin>>,
    state: Arc<Mutex<PlayerState>>,
}

impl MplayerBackend {
    pub async fn start(
        player_path: &str,
        args: &[String],
        initial_file: Option<&str>,
    ) -> anyhow::Result<(Self, Child)> {
        info!(
            "Starting player: kind=Mplayer, path={}, args={:?}, initial_file={:?}",
            player_path, args, initial_file
        );
        let mut cmd = Command::new(player_path);
        cmd.args(MPLAYER_ARGS);
        cmd.args(args);
        if let Some(path) = initial_file {
            cmd.arg(path);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = cmd.spawn().context("Failed to start MPlayer")?;
        let stdin = child
            .stdin
            .take()
            .context("Failed to capture MPlayer stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to capture MPlayer stdout")?;

        let state = Arc::new(Mutex::new(PlayerState::default()));
        let state_clone = state.clone();

        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }
                handle_line(&state_clone, &line);
            }
        });

        let backend = Self {
            kind: PlayerKind::Mplayer,
            stdin: Arc::new(TokioMutex::new(stdin)),
            state,
        };

        Ok((backend, child))
    }

    async fn send_command(&self, command: &str) -> anyhow::Result<()> {
        let mut guard = self.stdin.lock().await;
        guard
            .write_all(format!("{}\n", command).as_bytes())
            .await
            .context("Failed to write to MPlayer")?;
        guard.flush().await.context("Failed to flush MPlayer")?;
        Ok(())
    }
}

fn handle_line(state: &Arc<Mutex<PlayerState>>, line: &str) {
    debug!("mplayer >> {}", line);
    if let Some((key, value)) = parse_response(line) {
        let mut state_guard = state.lock();
        match key {
            ResponseKey::Position => state_guard.position = value.parse::<f64>().ok(),
            ResponseKey::Duration => state_guard.duration = value.parse::<f64>().ok(),
            ResponseKey::Filename => state_guard.filename = Some(value),
            ResponseKey::Path => state_guard.path = Some(value),
            ResponseKey::Pause => {
                let paused = match value.trim() {
                    "yes" | "true" | "1" => Some(true),
                    "no" | "false" | "0" => Some(false),
                    _ => None,
                };
                if paused.is_some() {
                    state_guard.paused = paused;
                }
            }
            ResponseKey::Speed => state_guard.speed = value.parse::<f64>().ok(),
        }
    }
}

fn parse_response(line: &str) -> Option<(ResponseKey, String)> {
    let line = line.trim();
    if let Some(value) = line.strip_prefix("ANS_TIME_POSITION=") {
        return Some((ResponseKey::Position, value.to_string()));
    }
    if let Some(value) = line.strip_prefix("ANS_LENGTH=") {
        return Some((ResponseKey::Duration, value.to_string()));
    }
    if let Some(value) = line.strip_prefix("ANS_FILENAME=") {
        let value = value.trim_matches('"').to_string();
        return Some((ResponseKey::Path, value.clone()));
    }
    if let Some(value) = line.strip_prefix("ANS_FILE_NAME=") {
        let value = value.trim_matches('"').to_string();
        return Some((ResponseKey::Filename, value));
    }
    if let Some(value) = line.strip_prefix("ANS_PATH=") {
        let value = value.trim_matches('"').to_string();
        return Some((ResponseKey::Path, value));
    }
    if let Some(value) = line.strip_prefix("ANS_pause=") {
        return Some((ResponseKey::Pause, value.to_string()));
    }
    if let Some(value) = line.strip_prefix("ANS_speed=") {
        return Some((ResponseKey::Speed, value.to_string()));
    }
    None
}

#[async_trait]
impl PlayerBackend for MplayerBackend {
    fn kind(&self) -> PlayerKind {
        self.kind
    }

    fn name(&self) -> &'static str {
        "MPlayer"
    }

    fn get_state(&self) -> PlayerState {
        self.state.lock().clone()
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        if let Err(e) = self.send_command("get_time_pos").await {
            warn!("Failed to request time position: {}", e);
        }
        if let Err(e) = self.send_command("get_time_length").await {
            warn!("Failed to request duration: {}", e);
        }
        if let Err(e) = self.send_command("get_file_name").await {
            warn!("Failed to request filename: {}", e);
        }
        if let Err(e) = self.send_command("get_property pause").await {
            warn!("Failed to request pause state: {}", e);
        }
        if let Err(e) = self.send_command("get_property speed").await {
            warn!("Failed to request speed: {}", e);
        }
        Ok(())
    }

    async fn set_position(&self, position: f64) -> anyhow::Result<()> {
        self.send_command(&format!("seek {} 2", position)).await
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        let current = self.state.lock().paused.unwrap_or(false);
        if paused != current {
            self.send_command("pause").await
        } else {
            Ok(())
        }
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        self.send_command(&format!("set_property speed {}", speed))
            .await
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        self.send_command(&format!("loadfile \"{}\" 0", path)).await
    }

    fn show_osd(&self, text: &str, _duration_ms: Option<u64>) -> anyhow::Result<()> {
        let cmd = format!("osd_show_text \"{}\"", text.replace('"', "'"));
        let stdin = self.stdin.clone();
        tokio::spawn(async move {
            let mut guard = stdin.lock().await;
            let _ = guard.write_all(format!("{}\n", cmd).as_bytes()).await;
            let _ = guard.flush().await;
        });
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.send_command("quit").await
    }
}
