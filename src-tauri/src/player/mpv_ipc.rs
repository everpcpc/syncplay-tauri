use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;
#[cfg(unix)]
use tokio::net::UnixStream;

use super::commands::{LoadfileOptionsSyntax, MpvCommand, MpvMessage, MpvResponse};
use super::events::MpvPlayerEvent;
use super::media_update::MediaSnapshot;
use super::properties::{PlayerState, PropertyId};

const MPV_SENDMESSAGE_COOLDOWN_TIME: Duration = Duration::from_millis(50);
const MPV_MAX_NEWFILE_COOLDOWN_TIME: Duration = Duration::from_secs(3);
const MPV_COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_millis(750);

enum QueueMessage {
    Command(MpvCommand),
    SetReady(bool),
    CancelLoad(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueKey {
    SetTimePos,
    LoadFile,
    CyclePause,
}

#[derive(Debug, Default)]
struct UntaggedLoadTracker {
    written: VecDeque<u64>,
    started: VecDeque<StartedUntaggedLoad>,
}

#[derive(Debug)]
struct StartedUntaggedLoad {
    load_id: u64,
    playlist_entry_id: Option<i64>,
    marker_seen: bool,
}

impl UntaggedLoadTracker {
    fn record_written(&mut self, load_id: u64) {
        self.written.push_back(load_id);
    }

    fn start_file(&mut self, playlist_entry_id: Option<i64>) -> Option<u64> {
        let load_id = self.written.pop_front()?;
        self.started.push_back(StartedUntaggedLoad {
            load_id,
            playlist_entry_id,
            marker_seen: false,
        });
        Some(load_id)
    }

    fn begin_marker(&mut self) -> Option<u64> {
        if let Some(load) = self.started.iter_mut().find(|load| !load.marker_seen) {
            load.marker_seen = true;
            return Some(load.load_id);
        }

        let load_id = self.written.pop_front()?;
        self.started.push_back(StartedUntaggedLoad {
            load_id,
            playlist_entry_id: None,
            marker_seen: true,
        });
        Some(load_id)
    }

    fn end_file(
        &mut self,
        playlist_entry_id: Option<i64>,
        redirected: bool,
    ) -> Option<(u64, bool)> {
        let position = match playlist_entry_id {
            Some(entry_id) => self
                .started
                .iter()
                .position(|load| load.playlist_entry_id == Some(entry_id))
                .or_else(|| {
                    self.started
                        .iter()
                        .position(|load| load.playlist_entry_id.is_none())
                }),
            None => (!self.started.is_empty()).then_some(0),
        }?;
        let load = self.started.remove(position)?;
        if redirected && !load.marker_seen {
            self.written.push_front(load.load_id);
            return None;
        }
        Some((load.load_id, load.marker_seen))
    }
}

/// MPV IPC client
pub struct MpvIpc {
    socket_path: String,
    queue_tx: Option<mpsc::UnboundedSender<QueueMessage>>,
    state: Arc<Mutex<PlayerState>>,
    next_request_id: Arc<Mutex<u64>>,
    pending_requests: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<MpvResponse>>>>,
    last_position_update: Arc<Mutex<Option<Instant>>>,
    active_load_generation: Arc<AtomicU64>,
    untagged_loads: Arc<Mutex<UntaggedLoadTracker>>,
}

impl MpvIpc {
    pub fn new(socket_path: impl Into<String>) -> Self {
        Self {
            socket_path: socket_path.into(),
            queue_tx: None,
            state: Arc::new(Mutex::new(PlayerState::default())),
            next_request_id: Arc::new(Mutex::new(1)),
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            last_position_update: Arc::new(Mutex::new(None)),
            active_load_generation: Arc::new(AtomicU64::new(0)),
            untagged_loads: Arc::new(Mutex::new(UntaggedLoadTracker::default())),
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
        let (queue_tx, mut queue_rx) = mpsc::unbounded_channel::<QueueMessage>();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<MpvCommand>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<MpvPlayerEvent>();

        self.queue_tx = Some(queue_tx.clone());

        let state = Arc::clone(&self.state);
        let pending_requests = Arc::clone(&self.pending_requests);
        let last_position_update = Arc::clone(&self.last_position_update);
        let active_load_generation = Arc::clone(&self.active_load_generation);
        let untagged_loads = Arc::clone(&self.untagged_loads);

        let write_event_tx = event_tx.clone();
        // Spawn write task
        tokio::spawn(async move {
            let mut write_half = write_half;
            while let Some(cmd) = cmd_rx.recv().await {
                if cmd
                    .load_id
                    .is_some_and(|load_id| active_load_generation.load(Ordering::SeqCst) != load_id)
                {
                    debug!(load_id = ?cmd.load_id, "Dropping cancelled mpv load command");
                    continue;
                }
                let json = match serde_json::to_string(&cmd) {
                    Ok(j) => j,
                    Err(e) => {
                        error!("Failed to serialize command: {}", e);
                        continue;
                    }
                };

                if is_untagged_generation_load(&cmd) {
                    untagged_loads
                        .lock()
                        .record_written(cmd.load_id.expect("generation load without id"));
                }

                if let Err(e) = write_half.write_all(json.as_bytes()).await {
                    error!("Failed to write to MPV socket: {}", e);
                    break;
                }
                if let Err(e) = write_half.write_all(b"\n").await {
                    error!("Failed to write newline to MPV socket: {}", e);
                    break;
                }
            }
            let _ = write_event_tx.send(MpvPlayerEvent::SocketDisconnected);
            debug!("MPV write task terminated");
        });

        // Spawn queue task
        tokio::spawn(async move {
            let mut pending: VecDeque<MpvCommand> = VecDeque::new();
            let mut ready = true;
            let mut next_send_at: Option<Instant> = None;
            let mut last_not_ready: Option<Instant> = None;
            let mut interval = tokio::time::interval(Duration::from_millis(10));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if !ready {
                            if let Some(last) = last_not_ready {
                                if last.elapsed() >= MPV_MAX_NEWFILE_COOLDOWN_TIME {
                                    ready = true;
                                    last_not_ready = None;
                                }
                            }
                        }
                        send_next_queued_command(
                            &mut pending,
                            &mut ready,
                            &mut last_not_ready,
                            &mut next_send_at,
                            &cmd_tx,
                        );
                    }
                    Some(message) = queue_rx.recv() => {
                        match message {
                            QueueMessage::Command(cmd) => {
                                enqueue_command(cmd, &mut pending, &mut next_send_at);
                            }
                            QueueMessage::SetReady(new_ready) => {
                                if new_ready {
                                    ready = true;
                                    last_not_ready = None;
                                } else {
                                    ready = false;
                                    last_not_ready = Some(Instant::now());
                                }
                            }
                            QueueMessage::CancelLoad(load_id) => {
                                cancel_queued_load(&mut pending, load_id);
                            }
                        }
                        send_next_queued_command(
                            &mut pending,
                            &mut ready,
                            &mut last_not_ready,
                            &mut next_send_at,
                            &cmd_tx,
                        );
                    }
                    else => break,
                }
            }
        });

        let read_event_tx = event_tx.clone();
        // Spawn read task
        tokio::spawn(async move {
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }

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
                            if let Some(id) = event.id {
                                if let Some(prop_id) = PropertyId::from_u64(id) {
                                    let value = event.data.unwrap_or(serde_json::Value::Null);
                                    if prop_id == PropertyId::TimePos && !value.is_null() {
                                        *last_position_update.lock() = Some(Instant::now());
                                    }
                                    state.lock().update_property(prop_id, &value);
                                }
                            }
                        } else if event.event == "log-message" {
                            if let Some(text) = event.text {
                                if event_tx.send(MpvPlayerEvent::LogMessage(text)).is_err() {
                                    warn!("Failed to send player event");
                                    break;
                                }
                            }
                        } else {
                            let player_event = MpvPlayerEvent::from_event_name(
                                &event.event,
                                event.reason.as_deref(),
                                event.playlist_entry_id,
                            );
                            if event_tx.send(player_event).is_err() {
                                warn!("Failed to send player event");
                                break;
                            }
                        }
                    }
                }
            }
            pending_requests.lock().clear();
            let _ = read_event_tx.send(MpvPlayerEvent::SocketDisconnected);
            debug!("MPV read task terminated");
        });

        // Observe properties
        self.observe_properties().await?;
        if let Err(e) = self.request_log_messages("info").await {
            warn!("Failed to enable MPV log messages: {}", e);
        }

        Ok(event_rx)
    }

    /// Observe all important properties
    async fn observe_properties(&self) -> Result<()> {
        let properties = [PropertyId::TimePos, PropertyId::Pause, PropertyId::Speed];

        for prop in properties {
            let cmd = MpvCommand::observe_property(prop.as_u64(), prop.property_name());
            self.send_command(cmd)?;
        }

        Ok(())
    }

    async fn request_log_messages(&self, level: &str) -> Result<()> {
        let cmd = MpvCommand::request_log_messages(level);
        let response = self.send_command_async(cmd).await?;
        if !response.error.is_empty() && response.error != "success" {
            warn!(
                "MPV request_log_messages returned error: {}",
                response.error
            );
        }
        Ok(())
    }

    /// Refresh only continuously changing playback properties.
    ///
    /// File identity is committed atomically by the SyncplayUpdateFile marker
    /// transaction and must never be reconstructed by this polling path.
    pub async fn refresh_playback_state(&self) -> Result<()> {
        for prop in [PropertyId::TimePos, PropertyId::Pause, PropertyId::Speed] {
            let cmd = MpvCommand::get_property(prop.property_name(), 0);
            let response = match self.send_command_async(cmd).await {
                Ok(response) => response,
                Err(err) => {
                    warn!(
                        "Failed to refresh mpv property {}: {}",
                        prop.property_name(),
                        err
                    );
                    continue;
                }
            };
            if let Some(data) = response.data {
                if prop == PropertyId::TimePos && !data.is_null() {
                    *self.last_position_update.lock() = Some(Instant::now());
                }
                self.state.lock().update_property(prop, &data);
            }
        }
        Ok(())
    }

    /// Send a command without waiting for response
    fn send_command(&self, cmd: MpvCommand) -> Result<()> {
        if let Some(tx) = &self.queue_tx {
            tx.send(QueueMessage::Command(cmd))
                .context("Failed to send command to MPV")?;
            Ok(())
        } else {
            anyhow::bail!("Not connected to MPV");
        }
    }

    pub fn set_ready(&self, ready: bool) {
        if let Some(tx) = &self.queue_tx {
            let _ = tx.send(QueueMessage::SetReady(ready));
        }
    }

    pub fn prepare_load(&self, load_id: u64) {
        self.active_load_generation.store(load_id, Ordering::SeqCst);
    }

    pub fn cancel_load(&self, load_id: u64) {
        let _ = self.active_load_generation.compare_exchange(
            load_id,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        if let Some(tx) = &self.queue_tx {
            let _ = tx.send(QueueMessage::CancelLoad(load_id));
        }
    }

    pub fn start_untagged_load(&self, playlist_entry_id: Option<i64>) -> Option<u64> {
        self.untagged_loads.lock().start_file(playlist_entry_id)
    }

    pub fn begin_untagged_marker(&self) -> Option<u64> {
        self.untagged_loads.lock().begin_marker()
    }

    pub fn end_untagged_load(
        &self,
        playlist_entry_id: Option<i64>,
        redirected: bool,
    ) -> Option<(u64, bool)> {
        self.untagged_loads
            .lock()
            .end_file(playlist_entry_id, redirected)
    }

    #[cfg(test)]
    pub(crate) fn record_untagged_load_for_test(&self, load_id: u64) {
        self.untagged_loads.lock().record_written(load_id);
    }

    async fn send_command_async_with_timeout(
        &self,
        mut cmd: MpvCommand,
        timeout_duration: Duration,
    ) -> Result<MpvResponse> {
        let request_id = {
            let mut id = self.next_request_id.lock();
            let current = *id;
            *id += 1;
            current
        };

        cmd.request_id = Some(request_id);

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending_requests.lock().insert(request_id, tx);

        if let Err(err) = self.send_command(cmd) {
            self.pending_requests.lock().remove(&request_id);
            return Err(err);
        }

        match timeout(timeout_duration, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(err)) => {
                self.pending_requests.lock().remove(&request_id);
                Err(err).context("Failed to receive response from MPV")
            }
            Err(_) => {
                self.pending_requests.lock().remove(&request_id);
                anyhow::bail!("Timed out waiting for MPV response")
            }
        }
    }

    /// Send a command and wait for response
    pub async fn send_command_async(&self, cmd: MpvCommand) -> Result<MpvResponse> {
        self.send_command_async_with_timeout(cmd, MPV_COMMAND_RESPONSE_TIMEOUT)
            .await
    }

    /// Get current player state
    pub fn get_state(&self) -> PlayerState {
        self.state.lock().clone()
    }

    pub fn commit_media_snapshot(&self, snapshot: &MediaSnapshot) {
        let mut state = self.state.lock();
        state.filename = snapshot.filename.clone();
        state.path = snapshot.path.clone();
        state.duration = snapshot.duration;
    }

    /// Set playback position
    ///
    /// Mirrors original Syncplay's mpv integration: remote seeks are sent as
    /// fire-and-forget property updates and local state is updated immediately.
    /// Waiting for a JSON IPC response here is dangerous because seek commands
    /// are deliberately coalesced while mpv is busy/loading; an older queued seek
    /// may be dropped in favour of a newer one and would otherwise leave the
    /// caller waiting forever during seek storms.
    pub async fn set_position(&self, position: f64) -> Result<()> {
        let Some(number) = serde_json::Number::from_f64(position.max(0.0)) else {
            anyhow::bail!("Invalid mpv position: {}", position);
        };
        let cmd = MpvCommand::set_property_no_reply("time-pos", serde_json::Value::Number(number));
        self.send_command(cmd)?;
        self.store_position_state(position.max(0.0));
        tokio::time::sleep(Duration::from_millis(30)).await;
        Ok(())
    }

    fn store_position_state(&self, position: f64) {
        self.state.lock().position = Some(position);
        *self.last_position_update.lock() = Some(Instant::now());
    }

    /// Set pause state
    pub async fn set_paused(&self, paused: bool) -> Result<()> {
        if self.get_state().paused == Some(paused) {
            return Ok(());
        }
        let cmd = MpvCommand::set_property("pause", serde_json::Value::Bool(paused), 0);
        self.send_command_async(cmd).await?;
        self.store_pause_state(paused);
        Ok(())
    }

    fn store_pause_state(&self, paused: bool) {
        self.state.lock().paused = Some(paused);
        if !paused {
            *self.last_position_update.lock() = Some(Instant::now());
        }
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
        let cmd = MpvCommand::loadfile_no_reply(path, "replace");
        self.send_command(cmd)?;
        Ok(())
    }

    pub async fn load_file_for_generation(&self, path: &str, load_id: u64) -> Result<()> {
        let mut cmd = MpvCommand::loadfile_no_reply(path, "replace");
        cmd.load_id = Some(load_id);
        self.send_command(cmd)?;
        Ok(())
    }

    pub async fn load_file_generation(
        &self,
        path: &str,
        load_id: u64,
        syntax: LoadfileOptionsSyntax,
    ) -> Result<()> {
        let marker = format!(
            "<SyncplayUpdateFile>\nANS_syncplay_load_id={}\nANS_filename=${{filename}}\nANS_length=${{=duration:${{=length:0}}}}\nANS_path=${{path}}\n</SyncplayUpdateFile>",
            load_id
        );
        let mut options = serde_json::Map::new();
        options.insert("term-playing-msg".to_string(), marker.into());
        let mut cmd = MpvCommand::loadfile_with_options(path, "replace", options, syntax);
        cmd.load_id = Some(load_id);
        self.send_command(cmd)?;
        Ok(())
    }

    /// Show OSD message
    pub fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> Result<()> {
        let cmd = MpvCommand::show_text(text, duration_ms);
        self.send_command(cmd)
    }

    /// Quit MPV/IINA
    pub fn quit(&self) -> Result<()> {
        let cmd = MpvCommand::quit();
        self.send_command(cmd)
    }

    pub fn update_pause_and_position(&self, paused: Option<bool>, position: Option<f64>) {
        let mut state = self.state.lock();
        if let Some(paused) = paused {
            state.paused = Some(paused);
        }
        if let Some(position) = position {
            state.position = Some(position);
            *self.last_position_update.lock() = Some(Instant::now());
        }
    }

    pub fn last_position_update(&self) -> Option<Instant> {
        *self.last_position_update.lock()
    }

    pub fn socket_path(&self) -> &str {
        &self.socket_path
    }
}

fn queue_key(cmd: &MpvCommand) -> Option<QueueKey> {
    let head = cmd.command.first()?;
    let head_str = head.as_str()?;
    match head_str {
        "set_property" => {
            if cmd.command.get(1).and_then(|v| v.as_str()) == Some("time-pos") {
                Some(QueueKey::SetTimePos)
            } else {
                None
            }
        }
        "loadfile" => Some(QueueKey::LoadFile),
        "cycle" => {
            if cmd.command.get(1).and_then(|v| v.as_str()) == Some("pause") {
                Some(QueueKey::CyclePause)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn load_has_generation_marker(cmd: &MpvCommand) -> bool {
    queue_key(cmd) == Some(QueueKey::LoadFile)
        && cmd.command.iter().skip(3).any(|value| {
            value
                .as_object()
                .and_then(|options| options.get("term-playing-msg"))
                .and_then(|marker| marker.as_str())
                .is_some_and(|marker| marker.contains("ANS_syncplay_load_id="))
        })
}

fn is_untagged_generation_load(cmd: &MpvCommand) -> bool {
    cmd.load_id.is_some()
        && queue_key(cmd) == Some(QueueKey::LoadFile)
        && !load_has_generation_marker(cmd)
}

fn drop_replaced_pending_requests(pending: &mut VecDeque<MpvCommand>, key: QueueKey) {
    pending.retain(|cmd| {
        let replaced = queue_key(cmd) == Some(key);
        if replaced {
            // If this command was created by send_command_async, resolve its
            // waiter by dropping the sender rather than leaking a future while
            // a newer command supersedes it.
            debug!("Dropping superseded mpv command: {:?}", cmd.command);
        }
        !replaced
    });
}

fn stale_across_load(cmd: &MpvCommand) -> bool {
    let head = cmd.command.first().and_then(|value| value.as_str());
    match head {
        Some("loadfile" | "get_property" | "seek") => true,
        Some("set_property") => matches!(
            cmd.command.get(1).and_then(|value| value.as_str()),
            Some("time-pos" | "pause" | "speed")
        ),
        Some("cycle") => cmd.command.get(1).and_then(|value| value.as_str()) == Some("pause"),
        Some("script-message-to") => {
            cmd.command.get(2).and_then(|value| value.as_str()) == Some("get_paused_and_position")
        }
        _ => false,
    }
}

fn cancel_queued_load(pending: &mut VecDeque<MpvCommand>, load_id: u64) {
    pending.retain(|command| command.load_id != Some(load_id));
}

fn enqueue_command(
    cmd: MpvCommand,
    pending: &mut VecDeque<MpvCommand>,
    next_send_at: &mut Option<Instant>,
) {
    let key = queue_key(&cmd);
    if let Some(key) = key {
        match key {
            QueueKey::CyclePause => {
                if let Some(pos) = pending
                    .iter()
                    .position(|c| queue_key(c) == Some(QueueKey::CyclePause))
                {
                    pending.remove(pos);
                    return;
                }
            }
            QueueKey::SetTimePos | QueueKey::LoadFile => {
                drop_replaced_pending_requests(pending, key);
            }
        }
    }

    if key == Some(QueueKey::LoadFile) {
        pending.retain(|queued| !stale_across_load(queued));
        pending.push_front(cmd);
        *next_send_at = Some(Instant::now() + MPV_SENDMESSAGE_COOLDOWN_TIME);
        return;
    }

    pending.push_back(cmd);
}

fn send_next_queued_command(
    pending: &mut VecDeque<MpvCommand>,
    ready: &mut bool,
    last_not_ready: &mut Option<Instant>,
    next_send_at: &mut Option<Instant>,
    cmd_tx: &mpsc::UnboundedSender<MpvCommand>,
) -> bool {
    if next_send_at.is_some_and(|deadline| Instant::now() < deadline) {
        return false;
    }

    let cmd = if *ready {
        pending.pop_front()
    } else {
        let position = pending.iter().position(load_has_generation_marker);
        position.and_then(|position| pending.remove(position))
    };
    let Some(cmd) = cmd else {
        return false;
    };

    let starts_media_transition = queue_key(&cmd) == Some(QueueKey::LoadFile);
    if cmd_tx.send(cmd).is_ok() {
        if starts_media_transition {
            *ready = false;
            *last_not_ready = Some(Instant::now());
        }
        *next_send_at = Some(Instant::now() + MPV_SENDMESSAGE_COOLDOWN_TIME);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_pause_state_refreshes_position_clock_when_unpausing() {
        let ipc = MpvIpc::new("unused");
        ipc.update_pause_and_position(Some(true), Some(10.0));
        let before_unpause = ipc.last_position_update().expect("position update missing");

        std::thread::sleep(Duration::from_millis(1));
        ipc.store_pause_state(false);

        let after_unpause = ipc.last_position_update().expect("position update missing");
        assert!(after_unpause > before_unpause);
        assert_eq!(ipc.get_state().paused, Some(false));
    }

    #[test]
    fn store_pause_state_keeps_position_clock_when_pausing() {
        let ipc = MpvIpc::new("unused");
        ipc.update_pause_and_position(Some(false), Some(10.0));
        let before_pause = ipc.last_position_update().expect("position update missing");

        std::thread::sleep(Duration::from_millis(1));
        ipc.store_pause_state(true);

        let after_pause = ipc.last_position_update().expect("position update missing");
        assert_eq!(after_pause, before_pause);
        assert_eq!(ipc.get_state().paused, Some(true));
    }

    #[tokio::test]
    async fn set_paused_skips_command_when_state_already_matches() {
        let ipc = MpvIpc::new("unused");
        ipc.update_pause_and_position(Some(true), Some(10.0));

        let result = ipc.set_paused(true).await;

        assert!(result.is_ok());
    }

    #[test]
    fn store_position_state_refreshes_position_clock_after_seek() {
        let ipc = MpvIpc::new("unused");
        ipc.update_pause_and_position(Some(false), Some(10.0));
        let before_seek = ipc.last_position_update().expect("position update missing");

        std::thread::sleep(Duration::from_millis(1));
        ipc.store_position_state(25.0);

        let after_seek = ipc.last_position_update().expect("position update missing");
        assert!(after_seek > before_seek);
        assert_eq!(ipc.get_state().position, Some(25.0));
    }

    #[test]
    fn newer_pending_seek_replaces_older_pending_seek() {
        let mut pending = VecDeque::from([
            MpvCommand::set_property_no_reply(
                "time-pos",
                serde_json::Value::Number(serde_json::Number::from_f64(10.0).unwrap()),
            ),
            MpvCommand::show_text("keep", Some(1000)),
        ]);

        drop_replaced_pending_requests(&mut pending, QueueKey::SetTimePos);

        assert_eq!(pending.len(), 1);
        assert_ne!(queue_key(&pending[0]), Some(QueueKey::SetTimePos));
    }

    #[tokio::test]
    async fn queued_pause_commands_send_in_original_order() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut pending = VecDeque::from([
            MpvCommand::set_property_no_reply("pause", serde_json::Value::Bool(true)),
            MpvCommand::set_property_no_reply("pause", serde_json::Value::Bool(false)),
        ]);
        let mut ready = true;
        let mut last_not_ready = None;
        let mut next_send_at = None;

        assert!(send_next_queued_command(
            &mut pending,
            &mut ready,
            &mut last_not_ready,
            &mut next_send_at,
            &cmd_tx,
        ));
        next_send_at = None;
        assert!(send_next_queued_command(
            &mut pending,
            &mut ready,
            &mut last_not_ready,
            &mut next_send_at,
            &cmd_tx,
        ));

        let first = cmd_rx.recv().await.expect("first command missing");
        let second = cmd_rx.recv().await.expect("second command missing");
        assert_eq!(
            first.command.get(2).and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            second.command.get(2).and_then(|value| value.as_bool()),
            Some(false)
        );
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn tagged_load_bypasses_the_gate_and_discards_stale_commands() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut pending = VecDeque::from([
            MpvCommand::set_property_no_reply("time-pos", serde_json::Value::Number(10.into())),
            MpvCommand::show_text("keep", Some(1000)),
        ]);
        let mut options = serde_json::Map::new();
        options.insert(
            "term-playing-msg".to_string(),
            "ANS_syncplay_load_id=7".into(),
        );
        let mut load = MpvCommand::loadfile_with_options(
            "latest.mkv",
            "replace",
            options,
            LoadfileOptionsSyntax::Legacy,
        );
        load.load_id = Some(7);
        let mut ready = false;
        let mut last_not_ready = Some(Instant::now());
        let mut next_send_at = None;

        enqueue_command(load, &mut pending, &mut next_send_at);

        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending[1].command.first().and_then(|value| value.as_str()),
            Some("show_text")
        );
        next_send_at = None;
        assert!(send_next_queued_command(
            &mut pending,
            &mut ready,
            &mut last_not_ready,
            &mut next_send_at,
            &cmd_tx,
        ));
        assert_eq!(
            queue_key(&cmd_rx.recv().await.expect("load command missing")),
            Some(QueueKey::LoadFile)
        );
    }

    #[tokio::test]
    async fn rapid_loads_are_debounced_to_the_latest_target() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut pending = VecDeque::new();
        let mut ready = false;
        let mut last_not_ready = Some(Instant::now());
        let mut next_send_at = None;

        for target in ["a.mkv", "b.mkv", "c.mkv"] {
            enqueue_command(
                MpvCommand::loadfile_no_reply(target, "replace"),
                &mut pending,
                &mut next_send_at,
            );
        }

        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].command.get(1).and_then(|value| value.as_str()),
            Some("c.mkv")
        );
        assert!(!send_next_queued_command(
            &mut pending,
            &mut ready,
            &mut last_not_ready,
            &mut next_send_at,
            &cmd_tx,
        ));

        next_send_at = None;
        assert!(!send_next_queued_command(
            &mut pending,
            &mut ready,
            &mut last_not_ready,
            &mut next_send_at,
            &cmd_tx,
        ));

        ready = true;
        assert!(send_next_queued_command(
            &mut pending,
            &mut ready,
            &mut last_not_ready,
            &mut next_send_at,
            &cmd_tx,
        ));
        let command = cmd_rx.recv().await.expect("latest load missing");
        assert_eq!(
            command.command.get(1).and_then(|value| value.as_str()),
            Some("c.mkv")
        );
        assert!(cmd_rx.try_recv().is_err());
    }

    #[test]
    fn untagged_marker_and_end_file_settle_the_same_generation() {
        let ipc = MpvIpc::new("unused");
        ipc.record_untagged_load_for_test(3);
        assert_eq!(ipc.start_untagged_load(Some(30)), Some(3));
        ipc.record_untagged_load_for_test(4);
        assert_eq!(ipc.start_untagged_load(Some(40)), Some(4));

        assert_eq!(ipc.begin_untagged_marker(), Some(3));
        assert_eq!(ipc.end_untagged_load(Some(30), false), Some((3, true)));
        assert_eq!(ipc.begin_untagged_marker(), Some(4));
    }

    #[test]
    fn untagged_end_file_retires_a_markerless_generation() {
        let ipc = MpvIpc::new("unused");
        ipc.record_untagged_load_for_test(3);
        assert_eq!(ipc.start_untagged_load(Some(30)), Some(3));
        ipc.record_untagged_load_for_test(4);

        assert_eq!(ipc.end_untagged_load(Some(30), false), Some((3, false)));
        assert_eq!(ipc.start_untagged_load(Some(40)), Some(4));
        assert_eq!(ipc.begin_untagged_marker(), Some(4));
    }

    #[test]
    fn cancelled_generation_is_removed_before_socket_write() {
        let ipc = MpvIpc::new("unused");
        ipc.prepare_load(7);
        let mut command = MpvCommand::loadfile_no_reply("a.mkv", "replace");
        command.load_id = Some(7);
        let mut pending = VecDeque::from([command]);

        ipc.cancel_load(7);
        cancel_queued_load(&mut pending, 7);

        assert_eq!(ipc.active_load_generation.load(Ordering::SeqCst), 0);
        assert!(pending.is_empty());
    }
}
