use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

use super::commands::{MpvCommand, MpvMessage, MpvResponse};
use super::events::MpvPlayerEvent;
use super::properties::{PlayerState, PropertyId};

/// MPV IPC client
pub struct MpvIpc {
    socket_path: String,
    tx: Option<mpsc::UnboundedSender<MpvCommand>>,
    state: Arc<Mutex<PlayerState>>,
    next_request_id: Arc<Mutex<u64>>,
    pending_requests: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<MpvResponse>>>>,
}

impl MpvIpc {
    pub fn new(socket_path: impl Into<String>) -> Self {
        Self {
            socket_path: socket_path.into(),
            tx: None,
            state: Arc::new(Mutex::new(PlayerState::default())),
            next_request_id: Arc::new(Mutex::new(1)),
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Connect to MPV IPC socket
    pub async fn connect(&mut self) -> Result<mpsc::UnboundedReceiver<MpvPlayerEvent>> {
        info!("Connecting to MPV IPC socket: {}", self.socket_path);

        // Connect to Unix socket or Windows named pipe
        #[cfg(unix)]
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .context("Failed to connect to MPV IPC socket")?;

        #[cfg(windows)]
        let stream = ClientOptions::new()
            .open(&self.socket_path)
            .context("Failed to connect to MPV named pipe")?;

        info!("Connected to MPV IPC socket");

        let (read_half, write_half) = tokio::io::split(stream);
        let reader = BufReader::new(read_half);

        // Create channels
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<MpvCommand>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<MpvPlayerEvent>();

        self.tx = Some(cmd_tx);

        let state = Arc::clone(&self.state);
        let pending_requests = Arc::clone(&self.pending_requests);

        // Spawn write task
        tokio::spawn(async move {
            let mut write_half = write_half;
            while let Some(cmd) = cmd_rx.recv().await {
                let json = match serde_json::to_string(&cmd) {
                    Ok(j) => j,
                    Err(e) => {
                        error!("Failed to serialize command: {}", e);
                        continue;
                    }
                };

                debug!("MPV << {}", json);

                if let Err(e) = write_half.write_all(json.as_bytes()).await {
                    error!("Failed to write to MPV socket: {}", e);
                    break;
                }
                if let Err(e) = write_half.write_all(b"\n").await {
                    error!("Failed to write newline to MPV socket: {}", e);
                    break;
                }
            }
            debug!("MPV write task terminated");
        });

        // Spawn read task
        tokio::spawn(async move {
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }

                debug!("MPV >> {}", line);

                let message: MpvMessage = match serde_json::from_str(&line) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("Failed to parse MPV message: {} - {}", e, line);
                        continue;
                    }
                };

                match message {
                    MpvMessage::Response(response) => {
                        // Handle response
                        if let Some(request_id) = response.request_id {
                            if let Some(sender) = pending_requests.lock().remove(&request_id) {
                                let _ = sender.send(response);
                            }
                        }
                    }
                    MpvMessage::Event(event) => {
                        // Handle event
                        if event.event == "property-change" {
                            if let (Some(id), Some(data)) = (event.id, event.data) {
                                if let Some(prop_id) = PropertyId::from_u64(id) {
                                    state.lock().update_property(prop_id, &data);
                                }
                            }
                        } else {
                            let player_event = MpvPlayerEvent::from_event_name(
                                &event.event,
                                event.reason.as_deref(),
                            );
                            if event_tx.send(player_event).is_err() {
                                warn!("Failed to send player event");
                                break;
                            }
                        }
                    }
                }
            }
            debug!("MPV read task terminated");
        });

        // Observe properties
        self.observe_properties().await?;

        Ok(event_rx)
    }

    /// Observe all important properties
    async fn observe_properties(&self) -> Result<()> {
        let properties = [
            PropertyId::TimePos,
            PropertyId::Pause,
            PropertyId::Filename,
            PropertyId::Duration,
            PropertyId::Path,
            PropertyId::Speed,
        ];

        for prop in properties {
            let cmd = MpvCommand::observe_property(prop.as_u64(), prop.property_name());
            self.send_command(cmd)?;
        }

        Ok(())
    }

    /// Send a command without waiting for response
    fn send_command(&self, cmd: MpvCommand) -> Result<()> {
        if let Some(tx) = &self.tx {
            tx.send(cmd).context("Failed to send command to MPV")?;
            Ok(())
        } else {
            anyhow::bail!("Not connected to MPV");
        }
    }

    /// Send a command and wait for response
    pub async fn send_command_async(&self, mut cmd: MpvCommand) -> Result<MpvResponse> {
        let request_id = {
            let mut id = self.next_request_id.lock();
            let current = *id;
            *id += 1;
            current
        };

        cmd.request_id = Some(request_id);

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending_requests.lock().insert(request_id, tx);

        self.send_command(cmd)?;

        rx.await.context("Failed to receive response from MPV")
    }

    /// Get current player state
    pub fn get_state(&self) -> PlayerState {
        self.state.lock().clone()
    }

    /// Set playback position
    pub async fn set_position(&self, position: f64) -> Result<()> {
        let cmd = MpvCommand::seek(position, "absolute", 0);
        self.send_command_async(cmd).await?;
        Ok(())
    }

    /// Set pause state
    pub async fn set_paused(&self, paused: bool) -> Result<()> {
        let cmd = MpvCommand::set_property("pause", serde_json::Value::Bool(paused), 0);
        self.send_command_async(cmd).await?;
        Ok(())
    }

    /// Set playback speed
    pub async fn set_speed(&self, speed: f64) -> Result<()> {
        let cmd = MpvCommand::set_property(
            "speed",
            serde_json::Value::Number(serde_json::Number::from_f64(speed).unwrap()),
            0,
        );
        self.send_command_async(cmd).await?;
        Ok(())
    }

    /// Load a file
    pub async fn load_file(&self, path: &str) -> Result<()> {
        let cmd = MpvCommand::loadfile(path, "replace", 0);
        self.send_command_async(cmd).await?;
        Ok(())
    }

    /// Show OSD message
    pub fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> Result<()> {
        let cmd = MpvCommand::show_text(text, duration_ms);
        self.send_command(cmd)
    }
}
