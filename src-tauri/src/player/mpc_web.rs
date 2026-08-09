use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::Client;
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

use super::backend::PlayerBackend;
use super::mpc_api::{MpcMediaSettle, MpcMediaSettleAction};
use super::properties::PlayerState;

const DEFAULT_MPC_PORT: u16 = 13579;
const MPC_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
const MPC_CMD_PLAYPAUSE: u32 = 0xA0000003;
const MPC_CMD_CLOSEAPP: u32 = 0xA0004006;

pub struct MpcWebBackend {
    kind: super::backend::PlayerKind,
    client: Client,
    state: Arc<Mutex<PlayerState>>,
    connected: Arc<AtomicBool>,
}

impl MpcWebBackend {
    pub async fn start(
        kind: super::backend::PlayerKind,
        player_path: &str,
        args: &[String],
        initial_file: Option<&str>,
    ) -> anyhow::Result<(Self, Option<Child>)> {
        info!(
            "Starting player: kind={:?}, path={}, args={:?}, initial_file={:?}",
            kind, player_path, args, initial_file
        );
        let mut cmd = Command::new(player_path);
        cmd.kill_on_drop(true);
        cmd.args(args);
        if let Some(path) = initial_file {
            cmd.arg(path);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let child = cmd.spawn().ok();
        let backend = Self {
            kind,
            client: Client::builder().timeout(MPC_HTTP_TIMEOUT).build()?,
            state: Arc::new(Mutex::new(PlayerState::default())),
            connected: Arc::new(AtomicBool::new(true)),
        };
        Ok((backend, child))
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", DEFAULT_MPC_PORT)
    }

    async fn get_variables(&self) -> anyhow::Result<String> {
        if !self.connected.load(Ordering::SeqCst) {
            anyhow::bail!("MPC web interface is disconnected");
        }
        let url = format!("{}/variables.html", self.base_url());
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("Failed to fetch MPC variables")?;
        let text = response.text().await?;
        Ok(text)
    }

    async fn send_command(&self, command: u32, value: Option<&str>) -> anyhow::Result<()> {
        if !self.connected.load(Ordering::SeqCst) {
            anyhow::bail!("MPC web interface is disconnected");
        }
        let mut url = format!("{}/command.html?wm_command={}", self.base_url(), command);
        if let Some(value) = value {
            url.push_str("&p1=");
            url.push_str(&urlencoding::encode(value));
        }
        let response = self.client.get(url).send().await.inspect_err(|_error| {
            self.connected.store(false, Ordering::SeqCst);
        })?;
        if !response.status().is_success() {
            self.connected.store(false, Ordering::SeqCst);
            anyhow::bail!("MPC command failed with status {}", response.status());
        }
        Ok(())
    }

    async fn refresh_state(&self) -> anyhow::Result<()> {
        let text = self.get_variables().await?;
        debug!("mpc variables: {}", text);
        let new_state = self.parse_variables(&text);
        *self.state.lock() = new_state;
        Ok(())
    }

    fn parse_variables(&self, text: &str) -> PlayerState {
        let mut state = PlayerState::default();
        for line in text.lines() {
            let mut parts = line.splitn(2, '=');
            let key = parts.next().unwrap_or("").trim();
            let value = parts.next().unwrap_or("").trim();
            match key {
                "position" => state.position = value.parse::<f64>().ok(),
                "duration" => state.duration = value.parse::<f64>().ok(),
                "filepath" => {
                    state.path = Some(value.to_string());
                    state.filename = std::path::Path::new(value)
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string());
                }
                "paused" => {
                    state.paused = match value {
                        "1" | "true" | "yes" => Some(true),
                        "0" | "false" | "no" => Some(false),
                        _ => None,
                    }
                }
                "speed" => state.speed = value.parse::<f64>().ok(),
                _ => {}
            }
        }
        state
    }
}

#[async_trait]
impl PlayerBackend for MpcWebBackend {
    fn kind(&self) -> super::backend::PlayerKind {
        self.kind
    }

    fn name(&self) -> &'static str {
        self.kind.display_name()
    }

    fn get_state(&self) -> PlayerState {
        self.state.lock().clone()
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        match self.refresh_state().await {
            Ok(()) => {}
            Err(e) => {
                self.connected.store(false, Ordering::SeqCst);
                warn!("Failed to read MPC variables: {}", e);
            }
        }
        Ok(())
    }

    async fn set_position(&self, position: f64) -> anyhow::Result<()> {
        self.send_command(0xA0002000, Some(&position.to_string()))
            .await
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        let command = if paused { 0xA0000005 } else { 0xA0000004 };
        self.send_command(command, None).await
    }

    async fn settle_media_change(&self, paused: bool, position: f64) -> anyhow::Result<()> {
        let mut settle = MpcMediaSettle::new(paused, position);
        loop {
            if settle.needs_pause_observation() {
                self.refresh_state().await.inspect_err(|_error| {
                    self.connected.store(false, Ordering::SeqCst);
                })?;
            }
            let observed_paused = self.state.lock().paused;
            let Some(action) = settle.next(observed_paused) else {
                return Ok(());
            };
            match action {
                MpcMediaSettleAction::SetPaused { paused, delay } => {
                    self.set_paused(paused).await?;
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                }
                MpcMediaSettleAction::TogglePaused { delay } => {
                    self.send_command(MPC_CMD_PLAYPAUSE, None).await?;
                    tokio::time::sleep(delay).await;
                }
                MpcMediaSettleAction::SetPosition(position) => {
                    self.set_position(position).await?;
                }
                MpcMediaSettleAction::Failed => {
                    anyhow::bail!("MPC pause state did not settle after retries");
                }
            }
        }
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        self.send_command(0xA0004008, Some(&speed.to_string()))
            .await
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        self.send_command(0xA0000000, Some(path)).await
    }

    fn show_osd(&self, text: &str, _duration_ms: Option<u64>) -> anyhow::Result<()> {
        let message = text.replace('"', "'");
        let client = self.client.clone();
        let url = format!(
            "{}/command.html?wm_command=0xA0005000&p1={}",
            self.base_url(),
            urlencoding::encode(&message)
        );
        tokio::spawn(async move {
            let _ = client.get(url).send().await;
        });
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        if self.connected.load(Ordering::SeqCst) {
            let _ = self.send_command(MPC_CMD_CLOSEAPP, None).await;
            self.connected.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}
