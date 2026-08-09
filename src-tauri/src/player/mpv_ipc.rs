use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex, Notify};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;
#[cfg(unix)]
use tokio::net::UnixStream;

use super::commands::{LoadfileOptionsSyntax, MpvCommand, MpvMessage, MpvResponse};
use super::events::MpvPlayerEvent;
use super::media_update::MediaSnapshot;
use super::properties::PlayerState;

const MPV_SENDMESSAGE_COOLDOWN_TIME: Duration = Duration::from_millis(50);
const MPV_MAX_NEWFILE_COOLDOWN_TIME: Duration = Duration::from_secs(3);
const MPV_COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
const MPV_SOCKET_WRITE_TIMEOUT: Duration = MPV_MAX_NEWFILE_COOLDOWN_TIME;

enum QueueMessage {
    Command(MpvCommand),
    SetReady(bool),
    CancelLoad(u64),
    LoadWriteResult {
        owner: GateOwner,
        written: bool,
        applied: oneshot::Sender<()>,
    },
    MarkerStarted {
        epoch: u64,
        load_id: Option<u64>,
    },
    MarkerBound {
        epoch: u64,
        load_id: u64,
    },
    MarkerFinished {
        epoch: u64,
        load_id: Option<u64>,
    },
    GenerationFinished(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueKey {
    SetTimePos,
    LoadFile,
    CyclePause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GateOwner {
    Generation(Option<u64>),
    Marker(u64),
}

#[derive(Debug, Default)]
struct SendGate {
    in_flight: Option<GateOwner>,
    completed_in_flight: Option<GateOwner>,
    owners: HashSet<GateOwner>,
    marker_generations: HashMap<u64, Option<u64>>,
    manual_blocked: bool,
    deadline: Option<Instant>,
}

impl SendGate {
    fn is_blocked(&self) -> bool {
        self.manual_blocked || self.in_flight.is_some() || !self.owners.is_empty()
    }

    fn begin_handoff(&mut self, owner: GateOwner) -> bool {
        if self.is_blocked() {
            return false;
        }
        self.in_flight = Some(owner);
        self.completed_in_flight = None;
        true
    }

    fn finish_handoff(&mut self, owner: GateOwner, written: bool) {
        if self.in_flight != Some(owner) {
            return;
        }
        self.in_flight = None;
        let completed_before_ack = self.completed_in_flight == Some(owner);
        if completed_before_ack {
            self.completed_in_flight = None;
        }
        if written && !completed_before_ack {
            self.owners.insert(owner);
            self.reset_deadline();
        } else {
            self.clear_deadline_if_ready();
        }
    }

    fn start_marker(&mut self, epoch: u64, load_id: Option<u64>) {
        self.owners.insert(GateOwner::Marker(epoch));
        self.marker_generations.insert(epoch, load_id);
        self.reset_deadline();
    }

    fn bind_marker(&mut self, epoch: u64, load_id: u64) {
        if let Some(binding) = self.marker_generations.get_mut(&epoch) {
            *binding = Some(load_id);
        }
    }

    fn finish_marker(&mut self, epoch: u64, load_id: Option<u64>) {
        self.owners.remove(&GateOwner::Marker(epoch));
        let bound_load_id = load_id.or_else(|| self.marker_generations.remove(&epoch).flatten());
        self.marker_generations.remove(&epoch);
        let generation = GateOwner::Generation(bound_load_id);
        if self.in_flight == Some(generation) {
            self.completed_in_flight = Some(generation);
        }
        self.owners.remove(&generation);
        self.clear_deadline_if_ready();
    }

    fn finish_generation(&mut self, load_id: u64) {
        let generation = GateOwner::Generation(Some(load_id));
        if self.in_flight == Some(generation) {
            self.completed_in_flight = Some(generation);
        }
        self.owners.remove(&generation);
        self.clear_deadline_if_ready();
    }

    fn set_manual_ready(&mut self, ready: bool) {
        self.manual_blocked = !ready;
        if ready {
            self.clear_deadline_if_ready();
        } else {
            self.reset_deadline();
        }
    }

    fn expire(&mut self, now: Instant) {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.owners.clear();
            self.marker_generations.clear();
            self.manual_blocked = false;
            self.deadline = None;
        }
    }

    fn reset_deadline(&mut self) {
        self.deadline = Some(Instant::now() + MPV_MAX_NEWFILE_COOLDOWN_TIME);
    }

    fn clear_deadline_if_ready(&mut self) {
        if !self.manual_blocked && self.owners.is_empty() {
            self.deadline = None;
        }
    }

    #[cfg(test)]
    fn has_owner(&self, owner: GateOwner) -> bool {
        self.owners.contains(&owner)
    }
}

#[derive(Debug, Default)]
struct LoadEventTracker {
    loads: HashMap<u64, TrackedLoad>,
}

type PendingRequests = HashMap<u64, tokio::sync::oneshot::Sender<MpvResponse>>;

struct PendingRequestGuard {
    request_id: u64,
    pending_requests: Arc<Mutex<PendingRequests>>,
}

impl PendingRequestGuard {
    fn new(request_id: u64, pending_requests: Arc<Mutex<PendingRequests>>) -> Self {
        Self {
            request_id,
            pending_requests,
        }
    }
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        self.pending_requests.lock().remove(&self.request_id);
    }
}

#[derive(Debug, Default)]
struct TrackedLoad {
    started: bool,
    marker_seen: bool,
}

impl LoadEventTracker {
    fn prepare(&mut self, load_id: u64) {
        self.loads.entry(load_id).or_default();
    }

    fn discard_unstarted(&mut self, load_id: u64) {
        if self.loads.get(&load_id).is_some_and(|load| !load.started) {
            self.loads.remove(&load_id);
        }
    }

    fn start_file(&mut self, load_id: u64) -> bool {
        let Some(load) = self.loads.get_mut(&load_id) else {
            return false;
        };
        load.started = true;
        true
    }

    fn mark_marker(&mut self, load_id: u64) -> bool {
        let Some(load) = self.loads.get_mut(&load_id) else {
            return false;
        };
        load.marker_seen = true;
        true
    }

    fn end_file(&mut self, load_id: u64, propagated: bool) -> Option<(u64, bool)> {
        if propagated {
            return None;
        }
        let load = self.loads.remove(&load_id)?;
        Some((load_id, load.marker_seen))
    }
}

/// MPV IPC client
pub struct MpvIpc {
    socket_path: String,
    queue_tx: Option<mpsc::UnboundedSender<QueueMessage>>,
    state: Arc<Mutex<PlayerState>>,
    next_request_id: Arc<Mutex<u64>>,
    pending_requests: Arc<Mutex<PendingRequests>>,
    last_position_update: Arc<Mutex<Option<Instant>>>,
    active_load_generation: Arc<AtomicU64>,
    load_events: Arc<Mutex<LoadEventTracker>>,
    healthy: Arc<AtomicBool>,
    playback_probe_in_flight: Arc<AtomicBool>,
    load_protocol_ready: Arc<AtomicBool>,
    load_protocol_notify: Arc<Notify>,
    load_protocol_probe: Arc<AsyncMutex<()>>,
    terminal_event_tx: Arc<Mutex<Option<mpsc::UnboundedSender<MpvPlayerEvent>>>>,
    ready_for_send: Arc<AtomicBool>,
    send_gate: Arc<Mutex<SendGate>>,
    socket_write_timeout: Duration,
    io_task_abort_handles: Vec<tokio::task::AbortHandle>,
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
            load_events: Arc::new(Mutex::new(LoadEventTracker::default())),
            healthy: Arc::new(AtomicBool::new(true)),
            playback_probe_in_flight: Arc::new(AtomicBool::new(false)),
            load_protocol_ready: Arc::new(AtomicBool::new(false)),
            load_protocol_notify: Arc::new(Notify::new()),
            load_protocol_probe: Arc::new(AsyncMutex::new(())),
            terminal_event_tx: Arc::new(Mutex::new(None)),
            ready_for_send: Arc::new(AtomicBool::new(true)),
            send_gate: Arc::new(Mutex::new(SendGate::default())),
            socket_write_timeout: MPV_SOCKET_WRITE_TIMEOUT,
            io_task_abort_handles: Vec::new(),
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
        self.healthy.store(true, Ordering::SeqCst);
        self.load_protocol_ready.store(false, Ordering::SeqCst);
        self.ready_for_send.store(true, Ordering::SeqCst);
        *self.send_gate.lock() = SendGate::default();
        *self.terminal_event_tx.lock() = Some(event_tx.clone());

        let pending_requests = Arc::clone(&self.pending_requests);
        let active_load_generation = Arc::clone(&self.active_load_generation);
        let load_events = Arc::clone(&self.load_events);
        let queue_load_events = Arc::clone(&self.load_events);
        let write_healthy = Arc::clone(&self.healthy);
        let queue_healthy = Arc::clone(&self.healthy);
        let queue_ready_for_send = Arc::clone(&self.ready_for_send);
        let queue_send_gate = Arc::clone(&self.send_gate);
        let write_queue_tx = queue_tx.clone();
        let socket_write_timeout = self.socket_write_timeout;

        let write_event_tx = event_tx.clone();
        // Spawn write task
        let write_task = tokio::spawn(async move {
            let mut write_half = write_half;
            while let Some(cmd) = cmd_rx.recv().await {
                let load_owner = load_gate_owner(&cmd);
                let generation_load_id = is_generation_load(&cmd)
                    .then(|| cmd.load_id.expect("generation load without id"));
                if generation_load_id
                    .is_some_and(|load_id| active_load_generation.load(Ordering::SeqCst) != load_id)
                {
                    debug!(load_id = ?cmd.load_id, "Dropping cancelled mpv load command");
                    if let Some(load_id) = generation_load_id {
                        load_events.lock().discard_unstarted(load_id);
                    }
                    if let Some(owner) = load_owner {
                        if !acknowledge_load_write_result(&write_queue_tx, owner, false).await {
                            break;
                        }
                    }
                    continue;
                }
                let json = match serde_json::to_string(&cmd) {
                    Ok(j) => j,
                    Err(e) => {
                        error!("Failed to serialize command: {}", e);
                        if let Some(load_id) = generation_load_id {
                            load_events.lock().discard_unstarted(load_id);
                        }
                        if let Some(owner) = load_owner {
                            if !acknowledge_load_write_result(&write_queue_tx, owner, false).await {
                                break;
                            }
                        }
                        continue;
                    }
                };

                if generation_load_id
                    .is_some_and(|load_id| active_load_generation.load(Ordering::SeqCst) != load_id)
                {
                    debug!(load_id = ?cmd.load_id, "Dropping cancelled mpv load command");
                    if let Some(load_id) = generation_load_id {
                        load_events.lock().discard_unstarted(load_id);
                    }
                    if let Some(owner) = load_owner {
                        if !acknowledge_load_write_result(&write_queue_tx, owner, false).await {
                            break;
                        }
                    }
                    continue;
                }
                let frame_write = timeout(socket_write_timeout, async {
                    write_half.write_all(json.as_bytes()).await?;
                    write_half.write_all(b"\n").await
                })
                .await;
                let write_failed = match frame_write {
                    Ok(Ok(())) => false,
                    Ok(Err(error)) => {
                        error!("Failed to write to MPV socket: {}", error);
                        true
                    }
                    Err(_) => {
                        error!(
                            "Timed out writing to MPV socket after {:?}",
                            socket_write_timeout
                        );
                        true
                    }
                };
                if write_failed {
                    if let Some(load_id) = generation_load_id {
                        load_events.lock().discard_unstarted(load_id);
                    }
                    if let Some(owner) = load_owner {
                        let _ = acknowledge_load_write_result(&write_queue_tx, owner, false).await;
                    }
                    break;
                }
                if let Some(owner) = load_owner {
                    if !acknowledge_load_write_result(&write_queue_tx, owner, true).await {
                        break;
                    }
                }
            }
            write_healthy.store(false, Ordering::SeqCst);
            let _ = write_event_tx.send(MpvPlayerEvent::SocketDisconnected);
            debug!("MPV write task terminated");
        });
        self.io_task_abort_handles.push(write_task.abort_handle());

        // Spawn queue task
        let queue_task = tokio::spawn(async move {
            let mut pending: VecDeque<MpvCommand> = VecDeque::new();
            let mut next_send_at: Option<Instant> = None;
            let mut interval = tokio::time::interval(Duration::from_millis(10));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let mut gate = queue_send_gate.lock();
                        if !queue_healthy.load(Ordering::SeqCst) {
                            queue_ready_for_send.store(false, Ordering::SeqCst);
                            if gate.in_flight.is_none() {
                                break;
                            }
                            continue;
                        }
                        gate.expire(Instant::now());
                        send_next_queued_command(
                            &mut pending,
                            &mut gate,
                            &mut next_send_at,
                            &cmd_tx,
                        );
                        publish_queue_ready(&gate, &pending, &queue_ready_for_send);
                    }
                    Some(message) = queue_rx.recv() => {
                        let mut gate = queue_send_gate.lock();
                        if !queue_healthy.load(Ordering::SeqCst) {
                            if let QueueMessage::LoadWriteResult {
                                owner,
                                written,
                                applied,
                            } = message
                            {
                                gate.finish_handoff(owner, written);
                                let _ = applied.send(());
                            }
                            queue_ready_for_send.store(false, Ordering::SeqCst);
                            if gate.in_flight.is_none() {
                                break;
                            }
                            continue;
                        }
                        match message {
                            QueueMessage::Command(cmd) => {
                                enqueue_command(cmd, &mut pending, &mut next_send_at);
                            }
                            QueueMessage::SetReady(new_ready) => {
                                gate.set_manual_ready(new_ready);
                            }
                            QueueMessage::CancelLoad(load_id) => {
                                if cancel_queued_load(&mut pending, load_id) {
                                    queue_load_events.lock().discard_unstarted(load_id);
                                }
                            }
                            QueueMessage::LoadWriteResult {
                                owner,
                                written,
                                applied,
                            } => {
                                gate.finish_handoff(owner, written);
                                let _ = applied.send(());
                            }
                            QueueMessage::MarkerStarted { epoch, load_id } => {
                                gate.start_marker(epoch, load_id);
                            }
                            QueueMessage::MarkerBound { epoch, load_id } => {
                                gate.bind_marker(epoch, load_id);
                            }
                            QueueMessage::MarkerFinished { epoch, load_id } => {
                                gate.finish_marker(epoch, load_id);
                            }
                            QueueMessage::GenerationFinished(load_id) => {
                                gate.finish_generation(load_id);
                            }
                        }
                        send_next_queued_command(
                            &mut pending,
                            &mut gate,
                            &mut next_send_at,
                            &cmd_tx,
                        );
                        publish_queue_ready(&gate, &pending, &queue_ready_for_send);
                    }
                    else => break,
                }
            }
            queue_ready_for_send.store(false, Ordering::SeqCst);
        });
        self.io_task_abort_handles.push(queue_task.abort_handle());

        let read_event_tx = event_tx.clone();
        let read_healthy = Arc::clone(&self.healthy);
        let read_load_protocol_ready = Arc::clone(&self.load_protocol_ready);
        let read_load_protocol_notify = Arc::clone(&self.load_protocol_notify);
        // Spawn read task
        let read_task = tokio::spawn(async move {
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
                        if event.event == "log-message" {
                            if let Some(text) = event.text {
                                if event_tx.send(MpvPlayerEvent::LogMessage(text)).is_err() {
                                    warn!("Failed to send player event");
                                    break;
                                }
                            }
                        } else if event.event == "client-message" {
                            let Some(args) = event.args else {
                                continue;
                            };
                            match parse_syncplay_load_event(&args) {
                                Ok(Some(player_event)) => {
                                    if matches!(
                                        player_event,
                                        MpvPlayerEvent::GenerationLoadProtocolReady
                                    ) {
                                        read_load_protocol_ready.store(true, Ordering::SeqCst);
                                        read_load_protocol_notify.notify_one();
                                        continue;
                                    }
                                    if event_tx.send(player_event).is_err() {
                                        warn!("Failed to send player event");
                                        break;
                                    }
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    warn!("Ignoring invalid Syncplay load event: {error}");
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
            read_healthy.store(false, Ordering::SeqCst);
            let _ = read_event_tx.send(MpvPlayerEvent::SocketDisconnected);
            debug!("MPV read task terminated");
        });
        self.io_task_abort_handles.push(read_task.abort_handle());

        self.request_log_messages("info").await?;

        Ok(event_rx)
    }

    async fn request_log_messages(&self, level: &str) -> Result<()> {
        let cmd = MpvCommand::request_log_messages(level);
        self.send_command_async(cmd).await?;
        Ok(())
    }

    pub(crate) async fn get_property_value(
        &self,
        property: &str,
    ) -> Result<Option<serde_json::Value>> {
        let result = self
            .send_command_async_with_timeout(
                MpvCommand::get_property(property, 0),
                MPV_COMMAND_RESPONSE_TIMEOUT,
                true,
            )
            .await;
        if let Err(error) = result.as_ref() {
            self.mark_unhealthy(format!("MPV command failed: {error}"));
        }
        let response = result?;
        Ok(response.data.filter(|value| !value.is_null()))
    }

    pub fn schedule_playback_refresh(self: &Arc<Self>) {
        self.schedule_playback_refresh_with_timeout(MPV_COMMAND_RESPONSE_TIMEOUT);
    }

    fn schedule_playback_refresh_with_timeout(self: &Arc<Self>, terminal_timeout: Duration) {
        if !self.is_healthy() || self.playback_probe_in_flight.swap(true, Ordering::SeqCst) {
            return;
        }

        let ipc = Arc::clone(self);
        tokio::spawn(async move {
            let refresh = async {
                let command = MpvCommand::script_message_to(
                    "syncplayintf",
                    "get_paused_and_position",
                    Vec::new(),
                );
                ipc.send_command_async(command).await.map(|_| ())
            };
            let result = timeout(terminal_timeout, refresh).await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    ipc.mark_unhealthy(format!("MPV status refresh failed: {error}"));
                }
                Err(_) => {
                    ipc.mark_unhealthy(format!(
                        "MPV status refresh timed out after {} seconds",
                        terminal_timeout.as_secs()
                    ));
                }
            }
            ipc.playback_probe_in_flight.store(false, Ordering::SeqCst);
        });
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }

    pub fn is_ready_for_send(&self) -> bool {
        self.is_healthy() && self.ready_for_send.load(Ordering::SeqCst)
    }

    pub fn is_load_active(&self, load_id: u64) -> bool {
        self.active_load_generation.load(Ordering::SeqCst) == load_id
    }

    pub async fn ensure_load_protocol_ready(&self) -> Result<()> {
        self.ensure_load_protocol_ready_with_timeout(MPV_MAX_NEWFILE_COOLDOWN_TIME)
            .await
    }

    async fn ensure_load_protocol_ready_with_timeout(
        &self,
        timeout_duration: Duration,
    ) -> Result<()> {
        if self.load_protocol_ready.load(Ordering::SeqCst) {
            return Ok(());
        }

        let _probe = self.load_protocol_probe.lock().await;
        if self.load_protocol_ready.load(Ordering::SeqCst) {
            return Ok(());
        }

        let ready = self.load_protocol_notify.notified();
        let command = MpvCommand::script_message_to(
            "syncplayintf",
            "syncplay-load-ping",
            vec![serde_json::Value::String("1".to_string())],
        );
        self.send_command_async(command).await?;
        if self.load_protocol_ready.load(Ordering::SeqCst) {
            return Ok(());
        }

        if timeout(timeout_duration, ready).await.is_err() {
            let error = anyhow::anyhow!(
                "Syncplay MPV script did not answer within {:?}",
                timeout_duration
            );
            self.mark_unhealthy(error.to_string());
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn mark_unhealthy(&self, reason: impl AsRef<str>) {
        if self.healthy.swap(false, Ordering::SeqCst) {
            self.ready_for_send.store(false, Ordering::SeqCst);
            error!("{}", reason.as_ref());
            if let Some(tx) = self.terminal_event_tx.lock().as_ref() {
                let _ = tx.send(MpvPlayerEvent::SocketDisconnected);
            }
        }
    }

    #[cfg(test)]
    fn schedule_playback_refresh_for_test(self: &Arc<Self>, terminal_timeout: Duration) {
        self.schedule_playback_refresh_with_timeout(terminal_timeout);
    }

    #[cfg(test)]
    pub(crate) fn pending_request_count(&self) -> usize {
        self.pending_requests.lock().len()
    }

    /// Send a command without waiting for response
    fn send_command(&self, cmd: MpvCommand) -> Result<()> {
        if queue_key(&cmd) == Some(QueueKey::LoadFile) {
            self.ready_for_send.store(false, Ordering::SeqCst);
        }
        let result = if !self.is_healthy() {
            Err(anyhow::anyhow!("MPV IPC is unhealthy"))
        } else if let Some(tx) = &self.queue_tx {
            tx.send(QueueMessage::Command(cmd))
                .context("Failed to send command to MPV")
        } else {
            Err(anyhow::anyhow!("Not connected to MPV"))
        };
        if let Err(error) = result.as_ref() {
            self.mark_unhealthy(format!("MPV command failed: {error}"));
        }
        result
    }

    pub fn set_ready(&self, ready: bool) {
        if !ready {
            self.ready_for_send.store(false, Ordering::SeqCst);
        }
        if let Some(tx) = &self.queue_tx {
            let _ = tx.send(QueueMessage::SetReady(ready));
        }
    }

    pub fn start_media_marker(&self, epoch: u64, load_id: Option<u64>) {
        self.ready_for_send.store(false, Ordering::SeqCst);
        if let Some(tx) = &self.queue_tx {
            let _ = tx.send(QueueMessage::MarkerStarted { epoch, load_id });
        }
    }

    pub fn bind_media_marker(&self, epoch: u64, load_id: u64) {
        if let Some(tx) = &self.queue_tx {
            let _ = tx.send(QueueMessage::MarkerBound { epoch, load_id });
        }
    }

    pub fn finish_media_marker(&self, epoch: u64, load_id: Option<u64>) {
        if let Some(tx) = &self.queue_tx {
            let _ = tx.send(QueueMessage::MarkerFinished { epoch, load_id });
        }
    }

    pub fn finish_generation_gate(&self, load_id: u64) {
        if let Some(tx) = &self.queue_tx {
            let _ = tx.send(QueueMessage::GenerationFinished(load_id));
        }
    }

    pub fn retire_active_load(&self, load_id: u64) {
        let _ = self.active_load_generation.compare_exchange(
            load_id,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn prepare_load(&self, load_id: u64) {
        self.active_load_generation.store(load_id, Ordering::SeqCst);
        self.load_events.lock().prepare(load_id);
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

    pub fn start_load(&self, load_id: u64) -> bool {
        self.load_events.lock().start_file(load_id)
    }

    pub fn mark_load_marker(&self, load_id: u64) -> bool {
        self.load_events.lock().mark_marker(load_id)
    }

    pub fn end_load(&self, load_id: u64, propagated: bool) -> Option<(u64, bool)> {
        self.load_events.lock().end_file(load_id, propagated)
    }

    #[cfg(test)]
    pub(crate) fn record_load_for_test(&self, load_id: u64) {
        self.load_events.lock().prepare(load_id);
    }

    #[cfg(test)]
    pub(crate) fn has_tracked_load_for_test(&self, load_id: u64) -> bool {
        self.load_events.lock().loads.contains_key(&load_id)
    }

    #[cfg(test)]
    fn set_socket_write_timeout_for_test(&mut self, timeout: Duration) {
        self.socket_write_timeout = timeout;
    }

    async fn send_command_async_with_timeout(
        &self,
        mut cmd: MpvCommand,
        timeout_duration: Duration,
        allow_property_unavailable: bool,
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
        let _pending_guard =
            PendingRequestGuard::new(request_id, Arc::clone(&self.pending_requests));

        self.send_command(cmd)?;

        match timeout(timeout_duration, rx).await {
            Ok(Ok(response)) => validate_response(response, allow_property_unavailable),
            Ok(Err(err)) => Err(err).context("Failed to receive response from MPV"),
            Err(_) => anyhow::bail!("Timed out waiting for MPV response"),
        }
    }

    /// Send a command and wait for response
    pub async fn send_command_async(&self, cmd: MpvCommand) -> Result<MpvResponse> {
        let result = self
            .send_command_async_with_timeout(cmd, MPV_COMMAND_RESPONSE_TIMEOUT, false)
            .await;
        if let Err(error) = result.as_ref() {
            self.mark_unhealthy(format!("MPV command failed: {error}"));
        }
        result
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
        self.state.lock().speed = Some(speed);
        Ok(())
    }

    /// Load a file
    pub async fn load_file(&self, path: &str) -> Result<()> {
        let cmd = MpvCommand::loadfile_no_reply(path, "replace");
        self.send_command(cmd)?;
        Ok(())
    }

    pub async fn load_file_for_generation(&self, path: &str, load_id: u64) -> Result<()> {
        let cmd = MpvCommand::load_generation_via_script(path, load_id, None);
        self.send_command(cmd)?;
        Ok(())
    }

    pub async fn load_file_generation(
        &self,
        path: &str,
        load_id: u64,
        syntax: LoadfileOptionsSyntax,
    ) -> Result<()> {
        let cmd = MpvCommand::load_generation_via_script(path, load_id, Some(syntax));
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

    #[cfg(test)]
    pub(crate) fn set_last_position_update_for_test(&self, instant: Instant) {
        *self.last_position_update.lock() = Some(instant);
    }

    pub fn socket_path(&self) -> &str {
        &self.socket_path
    }
}

impl Drop for MpvIpc {
    fn drop(&mut self) {
        self.queue_tx.take();
        self.terminal_event_tx.lock().take();
        self.pending_requests.lock().clear();
        self.healthy.store(false, Ordering::SeqCst);
        self.ready_for_send.store(false, Ordering::SeqCst);
        for handle in self.io_task_abort_handles.drain(..) {
            handle.abort();
        }
    }
}

async fn acknowledge_load_write_result(
    queue_tx: &mpsc::UnboundedSender<QueueMessage>,
    owner: GateOwner,
    written: bool,
) -> bool {
    let (applied_tx, applied_rx) = oneshot::channel();
    if queue_tx
        .send(QueueMessage::LoadWriteResult {
            owner,
            written,
            applied: applied_tx,
        })
        .is_err()
    {
        return false;
    }
    applied_rx.await.is_ok()
}

fn validate_response(
    mut response: MpvResponse,
    allow_property_unavailable: bool,
) -> Result<MpvResponse> {
    match response.error.as_str() {
        "success" => Ok(response),
        "property unavailable" if allow_property_unavailable => {
            response.data = None;
            Ok(response)
        }
        error => anyhow::bail!("MPV command failed: {}", error),
    }
}

fn parse_syncplay_load_event(args: &[String]) -> Result<Option<MpvPlayerEvent>> {
    if args.first().map(String::as_str) != Some("syncplay-load-event") {
        return Ok(None);
    }
    if args.len() != 7 {
        anyhow::bail!("expected 7 arguments, got {}", args.len());
    }

    let parse_load_id = |value: &str| -> Result<Option<u64>> {
        if value.is_empty() {
            Ok(None)
        } else {
            value
                .parse::<u64>()
                .map(Some)
                .context("invalid generation token")
        }
    };

    match args[1].as_str() {
        "ready" => {
            if args[2] != "1" {
                anyhow::bail!("ready event has an unexpected probe nonce");
            }
            Ok(Some(MpvPlayerEvent::GenerationLoadProtocolReady))
        }
        "accepted" => {
            parse_load_id(&args[2])?
                .ok_or_else(|| anyhow::anyhow!("accepted event omitted its generation token"))?;
            Ok(None)
        }
        "start" => Ok(Some(MpvPlayerEvent::GenerationLoadStarted {
            load_id: parse_load_id(&args[2])?,
            target: (!args[5].is_empty()).then(|| args[5].clone()),
        })),
        "end" => {
            if args[3].is_empty() {
                anyhow::bail!("end event omitted its reason");
            }
            let propagated = match args[6].as_str() {
                "true" => true,
                "false" => false,
                value => anyhow::bail!("end event has invalid propagation flag {value:?}"),
            };
            let reason = super::events::EndFileReason::from_str(&args[3]);
            if propagated && !matches!(reason, super::events::EndFileReason::Redirect) {
                anyhow::bail!("only redirect events can propagate a generation token");
            }
            Ok(Some(MpvPlayerEvent::GenerationLoadEnded {
                load_id: parse_load_id(&args[2])?,
                reason,
                propagated,
            }))
        }
        "rejected" => {
            let load_id = parse_load_id(&args[2])?
                .ok_or_else(|| anyhow::anyhow!("rejected event omitted its generation token"))?;
            if args[3].is_empty() {
                anyhow::bail!("rejected event omitted its error");
            }
            Ok(Some(MpvPlayerEvent::GenerationLoadRejected {
                load_id,
                error: args[3].clone(),
            }))
        }
        phase => anyhow::bail!("unknown phase {phase:?}"),
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
        "script-message-to"
            if cmd.load_id.is_some()
                && cmd.command.get(1).and_then(|value| value.as_str()) == Some("syncplayintf")
                && cmd.command.get(2).and_then(|value| value.as_str())
                    == Some("syncplay-load-file") =>
        {
            Some(QueueKey::LoadFile)
        }
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

fn is_generation_load(cmd: &MpvCommand) -> bool {
    cmd.load_id.is_some() && queue_key(cmd) == Some(QueueKey::LoadFile)
}

fn load_gate_owner(cmd: &MpvCommand) -> Option<GateOwner> {
    (queue_key(cmd) == Some(QueueKey::LoadFile)).then_some(GateOwner::Generation(cmd.load_id))
}

fn pending_has_load(pending: &VecDeque<MpvCommand>) -> bool {
    pending
        .iter()
        .any(|command| queue_key(command) == Some(QueueKey::LoadFile))
}

fn publish_queue_ready(
    gate: &SendGate,
    pending: &VecDeque<MpvCommand>,
    ready_for_send: &AtomicBool,
) {
    ready_for_send.store(
        !gate.is_blocked() && !pending_has_load(pending),
        Ordering::SeqCst,
    );
}

fn drop_replaced_pending_requests(pending: &mut VecDeque<MpvCommand>, key: QueueKey) {
    pending.retain(|cmd| {
        let replaced = cmd.request_id.is_none() && queue_key(cmd) == Some(key);
        if replaced {
            debug!("Dropping superseded mpv command: {:?}", cmd.command);
        }
        !replaced
    });
}

fn cancel_queued_load(pending: &mut VecDeque<MpvCommand>, load_id: u64) -> bool {
    let original_len = pending.len();
    pending.retain(|command| command.load_id != Some(load_id));
    pending.len() != original_len
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
                if cmd.request_id.is_none() {
                    if let Some(pos) = pending.iter().position(|queued| {
                        queued.request_id.is_none()
                            && queue_key(queued) == Some(QueueKey::CyclePause)
                    }) {
                        pending.remove(pos);
                        return;
                    }
                }
            }
            QueueKey::SetTimePos | QueueKey::LoadFile => {
                if cmd.request_id.is_none() {
                    drop_replaced_pending_requests(pending, key);
                }
            }
        }
    }

    if key == Some(QueueKey::LoadFile) {
        pending.push_back(cmd);
        *next_send_at = Some(Instant::now() + MPV_SENDMESSAGE_COOLDOWN_TIME);
        return;
    }

    pending.push_back(cmd);
}

fn send_next_queued_command(
    pending: &mut VecDeque<MpvCommand>,
    gate: &mut SendGate,
    next_send_at: &mut Option<Instant>,
    cmd_tx: &mpsc::UnboundedSender<MpvCommand>,
) -> bool {
    if gate.is_blocked() || next_send_at.is_some_and(|deadline| Instant::now() < deadline) {
        return false;
    }

    let cmd = pending.pop_back();
    let Some(cmd) = cmd else {
        return false;
    };

    let owner = load_gate_owner(&cmd);
    if owner.is_some_and(|owner| !gate.begin_handoff(owner)) {
        pending.push_back(cmd);
        return false;
    }
    if cmd_tx.send(cmd).is_ok() {
        *next_send_at = Some(Instant::now() + MPV_SENDMESSAGE_COOLDOWN_TIME);
        true
    } else {
        if let Some(owner) = owner {
            gate.finish_handoff(owner, false);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use tokio::io::AsyncReadExt;

    fn response(error: &str, data: Option<serde_json::Value>) -> MpvResponse {
        MpvResponse {
            error: error.to_string(),
            data,
            request_id: Some(1),
        }
    }

    fn load_event_args(phase: &str, token: &str, reason: &str) -> Vec<String> {
        ["syncplay-load-event", phase, token, reason, "", "", ""]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn end_load_event_args(token: &str, reason: &str, propagated: bool) -> Vec<String> {
        let mut args = load_event_args("end", token, reason);
        args[6] = propagated.to_string();
        args
    }

    #[test]
    fn structured_load_events_parse_explicit_and_external_lifecycles() {
        let mut redirected_start = load_event_args("start", "7", "");
        redirected_start[5] = "/media/redirect-child.mkv".to_string();
        assert_eq!(
            parse_syncplay_load_event(&redirected_start).unwrap(),
            Some(MpvPlayerEvent::GenerationLoadStarted {
                load_id: Some(7),
                target: Some("/media/redirect-child.mkv".to_string()),
            })
        );
        assert_eq!(
            parse_syncplay_load_event(&load_event_args("start", "", "")).unwrap(),
            Some(MpvPlayerEvent::GenerationLoadStarted {
                load_id: None,
                target: None,
            })
        );
        assert_eq!(
            parse_syncplay_load_event(&end_load_event_args("7", "redirect", false)).unwrap(),
            Some(MpvPlayerEvent::GenerationLoadEnded {
                load_id: Some(7),
                reason: super::super::events::EndFileReason::Redirect,
                propagated: false,
            })
        );
        assert_eq!(
            parse_syncplay_load_event(&end_load_event_args("7", "redirect", true)).unwrap(),
            Some(MpvPlayerEvent::GenerationLoadEnded {
                load_id: Some(7),
                reason: super::super::events::EndFileReason::Redirect,
                propagated: true,
            })
        );
        assert_eq!(
            parse_syncplay_load_event(&load_event_args("rejected", "7", "invalid parameter"))
                .unwrap(),
            Some(MpvPlayerEvent::GenerationLoadRejected {
                load_id: 7,
                error: "invalid parameter".to_string(),
            })
        );
    }

    #[test]
    fn structured_load_events_reject_malformed_protocol_messages() {
        assert!(parse_syncplay_load_event(&load_event_args("start", "not-a-number", "")).is_err());
        assert!(parse_syncplay_load_event(&load_event_args("end", "7", "")).is_err());
        assert!(parse_syncplay_load_event(&end_load_event_args("7", "stop", true)).is_err());
        assert!(parse_syncplay_load_event(&load_event_args("ready", "stale", "")).is_err());
        assert!(parse_syncplay_load_event(&["syncplay-load-event".to_string()]).is_err());
        assert_eq!(
            parse_syncplay_load_event(&["unrelated-message".to_string()]).unwrap(),
            None
        );
    }

    #[test]
    fn generation_script_message_uses_the_load_queue_contract() {
        let command = MpvCommand::load_generation_via_script(
            "movie.mkv",
            7,
            Some(LoadfileOptionsSyntax::Legacy),
        );

        assert_eq!(queue_key(&command), Some(QueueKey::LoadFile));
        assert_eq!(
            load_gate_owner(&command),
            Some(GateOwner::Generation(Some(7)))
        );
        assert!(is_generation_load(&command));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn generation_protocol_probe_waits_for_the_lua_ready_event() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: serde_json::Value = serde_json::from_str(&line).unwrap();
                let request_id = message["request_id"].as_u64().unwrap();
                write_half
                    .write_all(
                        format!(
                            "{}\n",
                            serde_json::json!({
                                "request_id": request_id,
                                "error": "success"
                            })
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                if message["command"][0] == "script-message-to" {
                    write_half
                        .write_all(
                            b"{\"event\":\"client-message\",\"args\":[\"syncplay-load-event\",\"ready\",\"1\",\"\",\"\",\"\",\"\"]}\n",
                        )
                        .await
                        .unwrap();
                    break;
                }
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let _events = ipc.connect().await.unwrap();

        ipc.ensure_load_protocol_ready().await.unwrap();
        assert!(ipc.load_protocol_ready.load(Ordering::SeqCst));
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_lua_ready_event_is_terminal() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: serde_json::Value = serde_json::from_str(&line).unwrap();
                let request_id = message["request_id"].as_u64().unwrap();
                write_half
                    .write_all(
                        format!(
                            "{}\n",
                            serde_json::json!({
                                "request_id": request_id,
                                "error": "success"
                            })
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                if message["command"][0] == "script-message-to" {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    break;
                }
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let _events = ipc.connect().await.unwrap();

        let error = ipc
            .ensure_load_protocol_ready_with_timeout(Duration::from_millis(30))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("did not answer"));
        assert!(!ipc.is_healthy());
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_a_silent_connect_closes_the_owned_socket_tasks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("silent-mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).await.unwrap();
            request_tx.send(request).unwrap();
            let mut remainder = Vec::new();
            reader.read_to_end(&mut remainder).await.unwrap();
        });

        let connect = tokio::spawn(async move {
            let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
            ipc.connect().await
        });
        let request = request_rx.await.unwrap();
        assert!(request.contains("request_log_messages"));

        connect.abort();
        assert!(connect.await.unwrap_err().is_cancelled());
        timeout(Duration::from_secs(1), server)
            .await
            .expect("cancelled MPV startup kept its socket tasks alive")
            .unwrap();
    }

    #[test]
    fn successful_response_preserves_data() {
        let response =
            validate_response(response("success", Some(serde_json::json!(42))), false).unwrap();

        assert_eq!(response.data, Some(serde_json::json!(42)));
    }

    #[test]
    fn unavailable_property_is_a_success_without_data() {
        let response = validate_response(
            response("property unavailable", Some(serde_json::json!("stale"))),
            true,
        )
        .unwrap();

        assert_eq!(response.data, None);
    }

    #[test]
    fn unavailable_property_is_an_error_without_the_metadata_policy() {
        let error = validate_response(response("property unavailable", None), false).unwrap_err();

        assert_eq!(
            error.to_string(),
            "MPV command failed: property unavailable"
        );
    }

    #[test]
    fn command_error_is_returned_to_the_caller() {
        let error = validate_response(response("invalid parameter", None), false).unwrap_err();

        assert_eq!(error.to_string(), "MPV command failed: invalid parameter");
    }

    #[tokio::test]
    async fn dropping_request_future_removes_pending_sender() {
        let (queue_tx, mut queue_rx) = mpsc::unbounded_channel();
        let mut ipc = MpvIpc::new("unused");
        ipc.queue_tx = Some(queue_tx);
        let ipc = Arc::new(ipc);
        let request_ipc = Arc::clone(&ipc);
        let request = tokio::spawn(async move {
            request_ipc
                .send_command_async(MpvCommand::get_property("pause", 0))
                .await
        });

        let message = queue_rx.recv().await.expect("request was not queued");
        let QueueMessage::Command(command) = message else {
            panic!("unexpected queue message");
        };
        assert!(command.request_id.is_some());
        assert_eq!(ipc.pending_requests.lock().len(), 1);

        request.abort();
        let _ = request.await;

        assert!(ipc.pending_requests.lock().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn log_subscription_error_fails_connection() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let message: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(message["command"][0], "request_log_messages");
            let request_id = message["request_id"].as_u64().unwrap();
            let response = serde_json::json!({
                "request_id": request_id,
                "error": "invalid parameter"
            });
            write_half
                .write_all(format!("{}\n", response).as_bytes())
                .await
                .unwrap();
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let error = match ipc.connect().await {
            Ok(_) => panic!("connection succeeded without MPV log events"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "MPV command failed: invalid parameter");
        assert!(!ipc.is_healthy());
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn playback_response_error_marks_ipc_unhealthy() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: serde_json::Value = serde_json::from_str(&line).unwrap();
                let command = message["command"][0].as_str().unwrap();
                let Some(request_id) = message["request_id"].as_u64() else {
                    continue;
                };
                let error = if command == "script-message-to" {
                    "invalid parameter"
                } else {
                    "success"
                };
                let response = serde_json::json!({
                    "request_id": request_id,
                    "error": error
                });
                write_half
                    .write_all(format!("{}\n", response).as_bytes())
                    .await
                    .unwrap();
                if command == "script-message-to" {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    break;
                }
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let mut events = ipc.connect().await.unwrap();
        let ipc = Arc::new(ipc);
        ipc.schedule_playback_refresh_for_test(Duration::from_secs(1));

        let event = timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, MpvPlayerEvent::SocketDisconnected));
        assert!(!ipc.is_healthy());
        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn set_paused_response_error_marks_ipc_unhealthy() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: serde_json::Value = serde_json::from_str(&line).unwrap();
                let command = message["command"][0].as_str().unwrap();
                let Some(request_id) = message["request_id"].as_u64() else {
                    continue;
                };
                let error = if command == "set_property" {
                    "invalid parameter"
                } else {
                    "success"
                };
                let response = serde_json::json!({
                    "request_id": request_id,
                    "error": error
                });
                write_half
                    .write_all(format!("{}\n", response).as_bytes())
                    .await
                    .unwrap();
                if command == "set_property" {
                    break;
                }
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let mut events = ipc.connect().await.unwrap();

        let error = ipc.set_paused(false).await.unwrap_err();

        assert_eq!(error.to_string(), "MPV command failed: invalid parameter");
        assert!(!ipc.is_healthy());
        assert!(!ipc.is_ready_for_send());
        let event = timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, MpvPlayerEvent::SocketDisconnected));
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_write_timeout_rolls_back_reservation_and_releases_the_gate() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut reader = BufReader::new(read_half);
            let mut request = String::new();
            reader.read_line(&mut request).await.unwrap();
            let message: serde_json::Value = serde_json::from_str(&request).unwrap();
            let response = serde_json::json!({
                "request_id": message["request_id"],
                "error": "success"
            });
            write_half
                .write_all(format!("{}\n", response).as_bytes())
                .await
                .unwrap();

            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut incomplete_frame = Vec::new();
            let _ = timeout(
                Duration::from_millis(100),
                reader.read_until(b'\n', &mut incomplete_frame),
            )
            .await;
            incomplete_frame
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        ipc.set_socket_write_timeout_for_test(Duration::from_millis(30));
        let mut events = ipc.connect().await.unwrap();
        ipc.prepare_load(7);
        let oversized_path = "x".repeat(16 * 1024 * 1024);
        ipc.load_file_for_generation(&oversized_path, 7)
            .await
            .unwrap();

        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, MpvPlayerEvent::SocketDisconnected));
        assert!(!ipc.is_healthy());
        assert_eq!(ipc.send_gate.lock().in_flight, None);
        assert!(!ipc.load_events.lock().loads.contains_key(&7));

        let incomplete_frame = timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        assert!(!incomplete_frame.ends_with(b"\n"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ready_load_reaches_the_socket_before_closing_the_gate() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: serde_json::Value = serde_json::from_str(&line).unwrap();
                let command = message["command"][0].as_str().unwrap();
                if command != "request_log_messages" {
                    command_tx.send(command.to_string()).unwrap();
                }
                if let Some(request_id) = message["request_id"].as_u64() {
                    let response = serde_json::json!({
                        "request_id": request_id,
                        "error": "success"
                    });
                    write_half
                        .write_all(format!("{}\n", response).as_bytes())
                        .await
                        .unwrap();
                }
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let _events = ipc.connect().await.unwrap();
        assert!(ipc.is_ready_for_send());

        ipc.load_file("new.mkv").await.unwrap();

        assert_eq!(
            timeout(Duration::from_millis(200), command_rx.recv())
                .await
                .unwrap()
                .as_deref(),
            Some("loadfile")
        );
        timeout(Duration::from_millis(200), async {
            while ipc.is_ready_for_send() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn queued_pause_survives_a_later_load_and_clears_its_pending_request() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: serde_json::Value = serde_json::from_str(&line).unwrap();
                let command = message["command"][0].as_str().unwrap();
                if command != "request_log_messages" {
                    command_tx.send(command.to_string()).unwrap();
                }
                if let Some(request_id) = message["request_id"].as_u64() {
                    let response = serde_json::json!({
                        "request_id": request_id,
                        "error": "success"
                    });
                    write_half
                        .write_all(format!("{}\n", response).as_bytes())
                        .await
                        .unwrap();
                }
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let _events = ipc.connect().await.unwrap();
        let ipc = Arc::new(ipc);
        ipc.set_ready(false);
        let pause_ipc = Arc::clone(&ipc);
        let pause = tokio::spawn(async move { pause_ipc.set_paused(false).await });
        timeout(Duration::from_secs(1), async {
            while ipc.pending_request_count() != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        ipc.load_file("new.mkv").await.unwrap();

        ipc.set_ready(true);
        assert_eq!(
            timeout(Duration::from_secs(1), command_rx.recv())
                .await
                .unwrap()
                .as_deref(),
            Some("loadfile")
        );
        assert!(!pause.is_finished());

        ipc.start_media_marker(1, None);
        ipc.finish_media_marker(1, None);
        assert_eq!(
            timeout(Duration::from_secs(1), command_rx.recv())
                .await
                .unwrap()
                .as_deref(),
            Some("set_property")
        );
        pause.await.unwrap().unwrap();
        assert_eq!(ipc.pending_request_count(), 0);
        assert!(ipc.is_healthy());
        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn media_property_unavailable_remains_non_terminal() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: serde_json::Value = serde_json::from_str(&line).unwrap();
                let command = message["command"][0].as_str().unwrap();
                let Some(request_id) = message["request_id"].as_u64() else {
                    continue;
                };
                let error = if command == "get_property" {
                    "property unavailable"
                } else {
                    "success"
                };
                let response = serde_json::json!({
                    "request_id": request_id,
                    "error": error
                });
                write_half
                    .write_all(format!("{}\n", response).as_bytes())
                    .await
                    .unwrap();
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let mut events = ipc.connect().await.unwrap();
        let value = ipc.get_property_value("filename").await.unwrap();

        assert!(value.is_none());
        assert!(ipc.is_healthy());
        assert!(timeout(Duration::from_millis(50), events.recv())
            .await
            .is_err());
        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn playback_terminal_timeout_marks_ipc_unhealthy_without_blocking_caller() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: serde_json::Value = serde_json::from_str(&line).unwrap();
                let command = message["command"][0].as_str().unwrap();
                if command != "request_log_messages" {
                    continue;
                }
                let response = serde_json::json!({
                    "request_id": message["request_id"],
                    "error": "success"
                });
                write_half
                    .write_all(format!("{}\n", response).as_bytes())
                    .await
                    .unwrap();
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let mut events = ipc.connect().await.unwrap();
        let ipc = Arc::new(ipc);
        ipc.schedule_playback_refresh_for_test(Duration::from_millis(30));
        assert!(ipc.is_healthy());

        let event = timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, MpvPlayerEvent::SocketDisconnected));
        assert!(!ipc.is_healthy());
        server.abort();
    }

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

    #[test]
    fn duplicate_coalescing_never_drops_commands_with_response_waiters() {
        let pending_load = MpvCommand::loadfile("old.mkv", "replace", 7);
        let mut pending = VecDeque::from([pending_load]);
        let mut next_send_at = None;

        enqueue_command(
            MpvCommand::loadfile_no_reply("latest.mkv", "replace"),
            &mut pending,
            &mut next_send_at,
        );

        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].request_id, Some(7));
        assert_eq!(pending[1].request_id, None);
    }

    #[tokio::test]
    async fn stale_handoff_drop_immediately_drives_the_latest_generation() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut pending = VecDeque::new();
        let mut next_send_at = None;
        let mut gate = SendGate::default();
        let mut load_a = MpvCommand::loadfile_no_reply("a.mkv", "replace");
        load_a.load_id = Some(1);
        enqueue_command(load_a, &mut pending, &mut next_send_at);
        next_send_at = None;
        assert!(send_next_queued_command(
            &mut pending,
            &mut gate,
            &mut next_send_at,
            &cmd_tx,
        ));
        assert_eq!(cmd_rx.recv().await.unwrap().load_id, Some(1));

        let mut load_b = MpvCommand::loadfile_no_reply("b.mkv", "replace");
        load_b.load_id = Some(2);
        enqueue_command(load_b, &mut pending, &mut next_send_at);
        next_send_at = None;
        let started = Instant::now();
        gate.finish_handoff(GateOwner::Generation(Some(1)), false);
        assert!(send_next_queued_command(
            &mut pending,
            &mut gate,
            &mut next_send_at,
            &cmd_tx,
        ));

        assert!(started.elapsed() < Duration::from_millis(200));
        assert_eq!(cmd_rx.recv().await.unwrap().load_id, Some(2));
        assert_eq!(gate.in_flight, Some(GateOwner::Generation(Some(2))));
    }

    #[test]
    fn marker_close_releases_only_its_matching_gate_owner() {
        let mut gate = SendGate::default();
        let generation_b = GateOwner::Generation(Some(2));
        assert!(gate.begin_handoff(generation_b));
        gate.finish_handoff(generation_b, true);

        gate.start_marker(10, Some(1));
        gate.start_marker(12, Some(2));
        gate.finish_marker(10, Some(1));
        assert!(gate.has_owner(generation_b));
        assert!(gate.has_owner(GateOwner::Marker(12)));
        assert!(gate.is_blocked());

        gate.start_marker(11, None);
        gate.finish_marker(11, None);
        assert!(gate.has_owner(generation_b));

        gate.finish_marker(12, Some(2));
        assert!(!gate.has_owner(generation_b));
        assert!(!gate.is_blocked());
    }

    #[test]
    fn generation_finish_releases_only_its_matching_gate_owner() {
        let mut gate = SendGate::default();
        let generation_b = GateOwner::Generation(Some(2));
        assert!(gate.begin_handoff(generation_b));
        gate.finish_handoff(generation_b, true);

        gate.finish_generation(1);
        assert!(gate.has_owner(generation_b));
        assert!(gate.is_blocked());

        gate.finish_generation(2);
        assert!(!gate.has_owner(generation_b));
        assert!(!gate.is_blocked());
    }

    #[test]
    fn marker_close_before_written_ack_does_not_rearm_the_generation_gate() {
        let mut gate = SendGate::default();
        let generation = GateOwner::Generation(Some(2));
        assert!(gate.begin_handoff(generation));
        gate.start_marker(12, Some(2));

        gate.finish_marker(12, Some(2));
        assert!(gate.is_blocked());

        gate.finish_handoff(generation, true);
        assert!(!gate.has_owner(generation));
        assert!(!gate.is_blocked());
        assert!(gate.deadline.is_none());
    }

    #[test]
    fn timeout_clears_old_owners_without_arming_a_late_close_against_new_generation() {
        let mut gate = SendGate::default();
        let generation_a = GateOwner::Generation(Some(1));
        let generation_b = GateOwner::Generation(Some(2));
        assert!(gate.begin_handoff(generation_a));
        gate.finish_handoff(generation_a, true);
        gate.start_marker(10, Some(1));
        gate.deadline = Some(Instant::now() - Duration::from_millis(1));

        gate.expire(Instant::now());
        assert!(gate.owners.is_empty());
        assert!(gate.marker_generations.is_empty());

        assert!(gate.begin_handoff(generation_b));
        gate.finish_handoff(generation_b, true);
        gate.finish_marker(10, Some(1));
        assert!(gate.has_owner(generation_b));
    }

    #[tokio::test]
    async fn queued_pause_commands_send_in_original_lifo_order() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut pending = VecDeque::from([
            MpvCommand::set_property_no_reply("pause", serde_json::Value::Bool(true)),
            MpvCommand::set_property_no_reply("pause", serde_json::Value::Bool(false)),
        ]);
        let mut gate = SendGate::default();
        let mut next_send_at = None;

        assert!(send_next_queued_command(
            &mut pending,
            &mut gate,
            &mut next_send_at,
            &cmd_tx,
        ));
        next_send_at = None;
        assert!(send_next_queued_command(
            &mut pending,
            &mut gate,
            &mut next_send_at,
            &cmd_tx,
        ));

        let first = cmd_rx.recv().await.expect("first command missing");
        let second = cmd_rx.recv().await.expect("second command missing");
        assert_eq!(
            first.command.get(2).and_then(|value| value.as_bool()),
            Some(false)
        );
        assert_eq!(
            second.command.get(2).and_then(|value| value.as_bool()),
            Some(true)
        );
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn mixed_queue_preserves_original_lifo_order_and_latest_load() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut pending = VecDeque::new();
        let mut next_send_at = None;
        for command in [
            MpvCommand::show_text("before-load", Some(1000)),
            MpvCommand::loadfile_no_reply("old.mkv", "replace"),
            MpvCommand::loadfile_no_reply("latest.mkv", "replace"),
            MpvCommand::set_property_no_reply("pause", serde_json::Value::Bool(true)),
            MpvCommand::show_text("latest", Some(1000)),
        ] {
            enqueue_command(command, &mut pending, &mut next_send_at);
        }
        let mut gate = SendGate::default();
        next_send_at = None;

        for _ in 0..3 {
            assert!(send_next_queued_command(
                &mut pending,
                &mut gate,
                &mut next_send_at,
                &cmd_tx,
            ));
            next_send_at = None;
        }
        assert_eq!(gate.in_flight, Some(GateOwner::Generation(None)));
        assert!(!send_next_queued_command(
            &mut pending,
            &mut gate,
            &mut next_send_at,
            &cmd_tx,
        ));
        gate.finish_handoff(GateOwner::Generation(None), false);
        next_send_at = None;
        assert!(send_next_queued_command(
            &mut pending,
            &mut gate,
            &mut next_send_at,
            &cmd_tx,
        ));

        let commands = [
            cmd_rx.recv().await.unwrap(),
            cmd_rx.recv().await.unwrap(),
            cmd_rx.recv().await.unwrap(),
            cmd_rx.recv().await.unwrap(),
        ];
        assert_eq!(commands[0].command[0], serde_json::json!("show_text"));
        assert_eq!(commands[0].command[1], serde_json::json!("latest"));
        assert_eq!(commands[1].command[0], serde_json::json!("set_property"));
        assert_eq!(commands[1].command[1], serde_json::json!("pause"));
        assert_eq!(commands[2].command[0], serde_json::json!("loadfile"));
        assert_eq!(commands[2].command[1], serde_json::json!("latest.mkv"));
        assert_eq!(commands[3].command[0], serde_json::json!("show_text"));
        assert_eq!(commands[3].command[1], serde_json::json!("before-load"));
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn tagged_load_waits_for_the_media_gate_without_discarding_prior_commands() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut pending = VecDeque::from([
            MpvCommand::set_property_no_reply("time-pos", serde_json::Value::Number(10.into())),
            MpvCommand::show_text("keep", Some(1000)),
        ]);
        let load = MpvCommand::load_generation_via_script(
            "latest.mkv",
            7,
            Some(LoadfileOptionsSyntax::Legacy),
        );
        let mut gate = SendGate::default();
        gate.start_marker(1, None);
        let mut next_send_at = None;

        enqueue_command(load, &mut pending, &mut next_send_at);

        assert_eq!(pending.len(), 3);
        assert_eq!(
            pending[0].command.get(1).and_then(|value| value.as_str()),
            Some("time-pos")
        );
        assert_eq!(
            pending[1].command.first().and_then(|value| value.as_str()),
            Some("show_text")
        );
        next_send_at = None;
        assert!(!send_next_queued_command(
            &mut pending,
            &mut gate,
            &mut next_send_at,
            &cmd_tx,
        ));
        assert!(cmd_rx.try_recv().is_err());

        gate.finish_marker(1, None);
        assert!(send_next_queued_command(
            &mut pending,
            &mut gate,
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
        let mut gate = SendGate::default();
        gate.set_manual_ready(false);
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
            &mut gate,
            &mut next_send_at,
            &cmd_tx,
        ));

        next_send_at = None;
        assert!(!send_next_queued_command(
            &mut pending,
            &mut gate,
            &mut next_send_at,
            &cmd_tx,
        ));

        gate.set_manual_ready(true);
        assert!(send_next_queued_command(
            &mut pending,
            &mut gate,
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
    fn explicit_marker_and_end_settle_the_same_generation() {
        let ipc = MpvIpc::new("unused");
        ipc.record_load_for_test(3);
        assert!(ipc.start_load(3));
        ipc.record_load_for_test(4);
        assert!(ipc.start_load(4));

        assert!(ipc.mark_load_marker(3));
        assert_eq!(ipc.end_load(3, false), Some((3, true)));
        assert!(!ipc.load_events.lock().loads.contains_key(&3));
        assert!(ipc.load_events.lock().loads.contains_key(&4));
    }

    #[test]
    fn tagged_marker_marks_only_its_explicit_generation() {
        let ipc = MpvIpc::new("unused");
        ipc.record_load_for_test(7);
        ipc.record_load_for_test(8);
        assert!(ipc.start_load(7));

        assert!(ipc.mark_load_marker(7));
        assert!(!ipc.load_events.lock().loads[&8].marker_seen);

        assert_eq!(ipc.end_load(7, false), Some((7, true)));
    }

    #[test]
    fn explicit_end_retires_only_its_markerless_generation() {
        let ipc = MpvIpc::new("unused");
        ipc.record_load_for_test(3);
        ipc.record_load_for_test(4);
        assert!(ipc.start_load(3));
        assert!(ipc.start_load(4));

        assert_eq!(ipc.end_load(3, false), Some((3, false)));
        assert!(ipc.mark_load_marker(4));
        assert_eq!(ipc.end_load(4, false), Some((4, true)));
    }

    #[test]
    fn redirect_propagation_keeps_attribution_until_the_claimed_child_ends() {
        let ipc = MpvIpc::new("unused");
        ipc.prepare_load(7);
        assert!(ipc.start_load(7));
        assert!(ipc.mark_load_marker(7));

        assert_eq!(ipc.end_load(7, true), None);
        assert!(ipc.load_events.lock().loads.contains_key(&7));
        assert_eq!(ipc.active_load_generation.load(Ordering::SeqCst), 7);

        assert_eq!(ipc.end_load(7, false), Some((7, true)));
        assert!(!ipc.load_events.lock().loads.contains_key(&7));
        assert_eq!(ipc.active_load_generation.load(Ordering::SeqCst), 7);
        ipc.retire_active_load(7);
        assert_eq!(ipc.active_load_generation.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn bundled_lua_redirect_contract_is_explicit_and_single_claim() {
        let script = include_str!("../../resources/syncplayintf.lua").replace("\r\n", "\n");
        let claim = script
            .find("syncplay_clear_redirects(redirected_token)")
            .expect("redirect claim must clear sibling mappings");
        let nested_mapping = script
            .rfind("syncplay_redirects_by_entry[tostring(inserted_id)] = token")
            .expect("redirect end must establish child mappings");
        let propagation = script
            .rfind("propagated = count > 0")
            .expect("redirect end must report whether it mapped a child");
        let end_event = script
            .rfind("syncplay_emit_load_event(\n        \"end\"")
            .expect("redirect end must emit its propagation result");

        assert!(script.contains("local propagated = false"));
        assert!(claim < nested_mapping);
        assert!(nested_mapping < propagation);
        assert!(propagation < end_event);
    }

    #[test]
    fn prepared_generation_correlates_end_before_writer_ack() {
        let mut tracker = LoadEventTracker::default();
        tracker.prepare(7);

        assert!(tracker.start_file(7));
        assert_eq!(tracker.end_file(7, false), Some((7, false)));

        tracker.prepare(8);
        tracker.discard_unstarted(8);
        assert!(!tracker.start_file(8));
        assert!(tracker.loads.is_empty());
    }

    #[test]
    fn cancelled_generation_is_removed_before_socket_write() {
        let ipc = MpvIpc::new("unused");
        ipc.prepare_load(7);
        let mut command = MpvCommand::loadfile_no_reply("a.mkv", "replace");
        command.load_id = Some(7);
        let mut pending = VecDeque::from([command]);

        ipc.cancel_load(7);
        if cancel_queued_load(&mut pending, 7) {
            ipc.load_events.lock().discard_unstarted(7);
        }

        assert_eq!(ipc.active_load_generation.load(Ordering::SeqCst), 0);
        assert!(pending.is_empty());
        assert!(!ipc.load_events.lock().loads.contains_key(&7));
    }
}
