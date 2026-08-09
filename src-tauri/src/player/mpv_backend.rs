use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStdout;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::backend::{next_player_instance_id, PlayerBackend, PlayerKind};
use super::commands::{LoadfileOptionsSyntax, MpvCommand};
use super::events::{EndFileReason, MpvPlayerEvent};
use super::media_update::{MediaCommit, MediaField, MediaUpdateState, MediaUpdateTransaction};
use super::mpv_ipc::MpvIpc;
use super::properties::PlayerState;
use crate::app_state::AppState;
use crate::client::playback::LoadId;
use crate::client::playback_runtime;
use crate::commands::chat::send_chat_message_from_player;
use crate::commands::connection::emit_error_message;
use crate::player::controller::report_end_of_file;
use crate::player::controller::{commit_external_player_state, commit_player_state};

pub struct MpvBackend {
    instance_id: u64,
    kind: PlayerKind,
    ipc: Arc<MpvIpc>,
    state: Weak<AppState>,
    file_loaded: Arc<AtomicBool>,
    media_updates: Arc<Mutex<MpvMediaUpdates>>,
    reset_ignore_until: Arc<Mutex<Option<Instant>>>,
    loadfile_options_syntax: Option<LoadfileOptionsSyntax>,
    osc_visibility_change_compatible: bool,
}

#[derive(Debug, Default)]
struct MpvMediaUpdates {
    state: MediaUpdateState,
    transaction: Option<MediaUpdateTransaction>,
    transaction_started_loaded: bool,
    marker_epoch: Option<u64>,
    marker_load_id: Option<u64>,
    retry_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MpvMetadataField {
    Filename,
    Duration,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MpvLineSource {
    Stdout,
    IpcLog,
}

#[derive(Clone)]
struct MpvLineContext {
    instance_id: u64,
    kind: PlayerKind,
    ipc: Arc<MpvIpc>,
    state: Weak<AppState>,
    file_loaded: Arc<AtomicBool>,
    media_updates: Arc<Mutex<MpvMediaUpdates>>,
    reset_ignore_until: Arc<Mutex<Option<Instant>>>,
    osc_visibility_change_compatible: bool,
}

const MPV_NEWFILE_IGNORE_TIME: Duration = Duration::from_secs(1);
const STREAM_ADDITIONAL_IGNORE_TIME: Duration = Duration::from_secs(10);
const PLAYER_ASK_DELAY: Duration = Duration::from_millis(100);
const MPV_UNRESPONSIVE_THRESHOLD: Duration = Duration::from_secs(60);
const MPV_METADATA_RETRY_DELAY: Duration = Duration::from_millis(50);
const DO_NOT_RESET_POSITION_THRESHOLD: f64 = 1.0;
const MPV_INPUT_BACKSLASH_SUBSTITUTE: &str = "＼";
const MPV_ERROR_MESSAGES_TO_REPEAT: [&str; 4] = [
    "[ytdl_hook] Your version of youtube-dl is too old",
    "[ytdl_hook] youtube-dl failed",
    "Failed to recognize file format.",
    "[syncplayintf] Lua error",
];

impl MpvBackend {
    pub fn new(
        kind: PlayerKind,
        ipc: MpvIpc,
        state: Weak<AppState>,
        loadfile_options_syntax: Option<LoadfileOptionsSyntax>,
        osc_visibility_change_compatible: bool,
        stdout: Option<ChildStdout>,
    ) -> Self {
        let backend = Self {
            instance_id: next_player_instance_id(),
            kind,
            ipc: Arc::new(ipc),
            state,
            file_loaded: Arc::new(AtomicBool::new(false)),
            media_updates: Arc::new(Mutex::new(MpvMediaUpdates::default())),
            reset_ignore_until: Arc::new(Mutex::new(None)),
            loadfile_options_syntax,
            osc_visibility_change_compatible,
        };
        if let Some(stdout) = stdout {
            backend.spawn_stdout_reader(stdout);
        }
        backend
    }

    pub fn ipc(&self) -> Arc<MpvIpc> {
        self.ipc.clone()
    }

    pub fn spawn_event_loop(self: &Arc<Self>, mut rx: mpsc::UnboundedReceiver<MpvPlayerEvent>) {
        let context = self.line_context();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if !is_current_player_context(&context) {
                    break;
                }
                match event {
                    MpvPlayerEvent::StartFile { .. } => {}
                    MpvPlayerEvent::EndFile { reason, .. } => {
                        if matches!(reason, EndFileReason::Quit) {
                            stop_player_from_weak(&context.state, context.instance_id).await;
                            break;
                        }
                    }
                    MpvPlayerEvent::GenerationLoadStarted {
                        load_id: Some(load_id),
                        target,
                    } => {
                        handle_generation_start(&context, load_id, target);
                    }
                    MpvPlayerEvent::GenerationLoadStarted { load_id: None, .. } => {}
                    MpvPlayerEvent::GenerationLoadEnded {
                        load_id: Some(load_id),
                        reason,
                        propagated,
                    } => {
                        handle_end_file(&context, &reason, load_id, propagated).await;
                    }
                    MpvPlayerEvent::GenerationLoadEnded { load_id: None, .. } => {}
                    MpvPlayerEvent::GenerationLoadRejected { load_id, error } => {
                        warn!(load_id, "MPV rejected generation load: {error}");
                        context.ipc.end_load(load_id, false);
                        fail_media_load(&context, load_id).await;
                    }
                    MpvPlayerEvent::GenerationLoadProtocolReady => {}
                    MpvPlayerEvent::Quit | MpvPlayerEvent::SocketDisconnected => {
                        stop_player_from_weak(&context.state, context.instance_id).await;
                        break;
                    }
                    MpvPlayerEvent::LogMessage(line) => {
                        handle_syncplayintf_line(&context, MpvLineSource::IpcLog, &line).await;
                    }
                    _ => {}
                }
            }
        });
    }

    fn spawn_stdout_reader(&self, stdout: ChildStdout) {
        let context = self.line_context();
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                handle_syncplayintf_line(&context, MpvLineSource::Stdout, &line).await;
            }
            debug!("Player stdout closed; IPC remains authoritative for lifecycle");
        });
    }

    fn line_context(&self) -> MpvLineContext {
        MpvLineContext {
            instance_id: self.instance_id,
            kind: self.kind,
            ipc: self.ipc.clone(),
            state: self.state.clone(),
            file_loaded: self.file_loaded.clone(),
            media_updates: self.media_updates.clone(),
            reset_ignore_until: self.reset_ignore_until.clone(),
            osc_visibility_change_compatible: self.osc_visibility_change_compatible,
        }
    }

    fn recently_reset(&self) -> bool {
        let guard = self.reset_ignore_until.lock();
        let Some(until) = guard.as_ref() else {
            return false;
        };
        Instant::now() < *until
    }
}

#[async_trait]
impl PlayerBackend for MpvBackend {
    fn instance_id(&self) -> u64 {
        self.instance_id
    }

    fn kind(&self) -> PlayerKind {
        self.kind
    }

    fn name(&self) -> &'static str {
        self.kind.display_name()
    }

    fn get_state(&self) -> PlayerState {
        let mut state = self.ipc.get_state();
        let is_loaded = self.file_loaded.load(Ordering::SeqCst);
        if let Some(app_state) = self.state.upgrade() {
            if self.recently_reset() {
                let global = app_state.effective_global_state();
                state.position = Some(0.0);
                state.paused = Some(global.paused);
                return state;
            }
            if !is_loaded {
                let global = app_state.effective_global_state();
                state.position = Some(global.position);
                state.paused = Some(global.paused);
                return state;
            }
        }

        if let Some(last_update) = self.ipc.last_position_update() {
            let paused = state.paused.unwrap_or(true);
            let diff = last_update.elapsed();
            if diff > MPV_UNRESPONSIVE_THRESHOLD {
                if let Some(app_state) = self.state.upgrade() {
                    let message = format!(
                        "mpv has not responded for {} seconds so appears to have malfunctioned. Please restart Syncplay.",
                        diff.as_secs()
                    );
                    emit_error_message(&app_state, &message);
                    let app_state_clone = app_state.clone();
                    let instance_id = self.instance_id;
                    tokio::spawn(async move {
                        let _ = crate::player::controller::stop_player_instance(
                            &app_state_clone,
                            instance_id,
                        )
                        .await;
                    });
                }
            }
            if !paused && diff > PLAYER_ASK_DELAY {
                if let Some(position) = state.position {
                    state.position = Some(position + diff.as_secs_f64());
                }
            }
        }

        state
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        if !self.ipc.is_ready_for_send() {
            return Ok(());
        }
        self.ipc.schedule_playback_refresh();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.ipc.is_healthy()
    }

    async fn set_position(&self, position: f64) -> anyhow::Result<()> {
        if position < DO_NOT_RESET_POSITION_THRESHOLD && self.recently_reset() {
            return Ok(());
        }
        self.ipc.set_position(position).await
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        self.ipc.set_paused(paused).await
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        self.ipc.set_speed(speed).await
    }

    fn begin_file_load(&self, load_id: u64, target: &str) {
        let mut media = self.media_updates.lock();
        media.retry_epoch = media.retry_epoch.wrapping_add(1);
        media.transaction = None;
        media.transaction_started_loaded = false;
        media.state.begin_load(load_id, target);
        self.ipc.prepare_load(load_id);
        drop(media);
        self.file_loaded.store(false, Ordering::SeqCst);
        *self.reset_ignore_until.lock() = None;
    }

    fn cancel_file_load(&self, load_id: u64) {
        let mut media = self.media_updates.lock();
        media.retry_epoch = media.retry_epoch.wrapping_add(1);
        media.transaction = None;
        media.transaction_started_loaded = false;
        media.state.cancel_load(load_id);
        self.ipc.cancel_load(load_id);
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        self.ipc.load_file(path).await?;
        Ok(())
    }

    async fn load_file_generation(&self, path: &str, load_id: u64) -> anyhow::Result<()> {
        self.ipc.ensure_load_protocol_ready().await?;
        if let Some(syntax) = self.loadfile_options_syntax {
            self.ipc.load_file_generation(path, load_id, syntax).await?;
        } else {
            self.ipc.load_file_for_generation(path, load_id).await?;
        }
        Ok(())
    }

    fn reports_atomic_media_commits(&self) -> bool {
        true
    }

    fn mark_reset(&self, is_stream: bool) {
        let mut until = Instant::now() + MPV_NEWFILE_IGNORE_TIME;
        if is_stream {
            until += STREAM_ADDITIONAL_IGNORE_TIME;
        }
        *self.reset_ignore_until.lock() = Some(until);
    }

    fn set_features(&self) -> anyhow::Result<()> {
        let Some(state) = self.state.upgrade() else {
            return Ok(());
        };
        send_syncplayintf_options(
            self.ipc.clone(),
            state,
            self.osc_visibility_change_compatible,
        );
        Ok(())
    }

    fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> anyhow::Result<()> {
        if let Some(state) = self.state.upgrade() {
            let config = state.config.lock().clone();
            if config.user.chat_output_enabled {
                let message = text.replace('"', "'");
                let ipc = self.ipc.clone();
                tokio::spawn(async move {
                    let cmd = MpvCommand::script_message_to(
                        "syncplayintf",
                        "notification-osd-neutral",
                        vec![Value::String(message)],
                    );
                    let _ = ipc.send_command_async(cmd).await;
                });
                return Ok(());
            }
        }
        self.ipc.show_osd(text, duration_ms)
    }

    fn show_chat_message(&self, username: Option<&str>, message: &str) -> anyhow::Result<()> {
        let mut output = String::new();
        if let Some(name) = username {
            output.push('<');
            output.push_str(&sanitize_mpv_text(name));
            output.push('>');
            output.push(' ');
        }
        output.push_str(&sanitize_mpv_text(message));
        let ipc = self.ipc.clone();
        tokio::spawn(async move {
            let cmd =
                MpvCommand::script_message_to("syncplayintf", "chat", vec![Value::String(output)]);
            let _ = ipc.send_command_async(cmd).await;
        });
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.ipc.quit()?;
        Ok(())
    }
}

fn is_current_player_context(context: &MpvLineContext) -> bool {
    if context.instance_id == 0 {
        return true;
    }
    context.state.upgrade().is_some_and(|state| {
        state
            .player
            .lock()
            .as_ref()
            .is_some_and(|player| player.instance_id() == context.instance_id)
    })
}

async fn stop_player_from_weak(state: &Weak<AppState>, instance_id: u64) {
    if let Some(app_state) = state.upgrade() {
        let _ = crate::player::controller::stop_player_instance(&app_state, instance_id).await;
    }
}

fn send_syncplayintf_options(
    ipc: Arc<MpvIpc>,
    state: Arc<AppState>,
    osc_visibility_change_compatible: bool,
) {
    tokio::spawn(async move {
        let options = build_syncplayintf_options(&state, osc_visibility_change_compatible);
        let cmd = MpvCommand::script_message_to(
            "syncplayintf",
            "set_syncplayintf_options",
            vec![Value::String(options)],
        );
        let _ = ipc.send_command_async(cmd).await;
        let socket = ipc.socket_path().to_string();
        let _ = ipc
            .send_command_async(MpvCommand::set_property(
                "input-ipc-server",
                Value::String(socket),
                0,
            ))
            .await;
        apply_osd_position(&ipc, &state).await;
    });
}

async fn handle_syncplayintf_line(
    context: &MpvLineContext,
    line_source: MpvLineSource,
    line: &str,
) {
    let ipc = &context.ipc;
    let state = &context.state;
    let file_loaded = &context.file_loaded;
    let media_updates = &context.media_updates;
    let osc_visibility_change_compatible = context.osc_visibility_change_compatible;
    let mut line = line.trim().to_string();
    line = line
        .replace("[cplayer] ", "")
        .replace("[term-msg] ", "")
        .replace("   cplayer: ", "")
        .replace("  term-msg: ", "");
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if !is_current_player_context(context) {
        return;
    }
    if line_source == MpvLineSource::Stdout {
        debug!("mpv stdout >> {}", line);
        if MPV_ERROR_MESSAGES_TO_REPEAT
            .iter()
            .any(|message| line.contains(message))
        {
            if let Some(state) = state.upgrade() {
                emit_error_message(&state, line);
            }
        }
        return;
    }
    debug!("mpv >> {}", line);
    if MPV_ERROR_MESSAGES_TO_REPEAT
        .iter()
        .any(|message| line.contains(message))
    {
        if let Some(state) = state.upgrade() {
            emit_error_message(&state, line);
        }
    }
    if line.contains("Failed to get value of property")
        || (!line.starts_with("ANS_") && line.contains("=(unavailable)"))
    {
        // The marker transaction keeps unavailable media fields missing. They
        // are retried after the closing marker, when the command gate reopens.
        return;
    }
    if line.contains("<chat>") {
        if let Some(message) = extract_tag(line, "chat") {
            let message = message.replace(MPV_INPUT_BACKSLASH_SUBSTITUTE, "\\");
            if let Some(state) = state.upgrade() {
                let _ = send_chat_message_from_player(&state, &message).await;
            }
        }
        return;
    }
    if line.contains("<eof>") {
        if let Some(state) = state.upgrade() {
            report_end_of_file(&state, ipc.get_state().position);
        }
        return;
    }
    if line.contains("<get_syncplayintf_options>") {
        if let Some(state) = state.upgrade() {
            send_syncplayintf_options(ipc.clone(), state, osc_visibility_change_compatible);
        }
        return;
    }
    if line.contains("<SyncplayUpdateFile>") || line.contains("Playing:") {
        let mut media = media_updates.lock();
        media.retry_epoch = media.retry_epoch.wrapping_add(1);
        let transaction_started_loaded = file_loaded.swap(false, Ordering::SeqCst);
        if media.transaction.is_some() || media.marker_epoch.is_some() {
            return;
        }
        let load_id = None;
        let marker_epoch = media.retry_epoch;
        media.transaction_started_loaded = transaction_started_loaded;
        media.marker_epoch = Some(marker_epoch);
        media.marker_load_id = load_id;
        media.transaction = Some(media.state.begin_update(load_id));
        ipc.start_media_marker(marker_epoch, load_id);
        return;
    }
    if line.contains("</SyncplayUpdateFile>") {
        let closing = {
            let mut media = media_updates.lock();
            let marker_epoch = media.marker_epoch.take();
            let marker_load_id = media.marker_load_id.take();
            let transaction = media.transaction.take();
            let transaction_started_loaded = media.transaction_started_loaded;
            media.transaction_started_loaded = false;
            if transaction
                .as_ref()
                .is_some_and(|transaction| transaction.load_id().is_none())
            {
                media.retry_epoch = media.retry_epoch.wrapping_add(1);
            }
            (
                marker_epoch,
                marker_load_id,
                transaction,
                transaction_started_loaded,
                media.retry_epoch,
            )
        };
        let (marker_epoch, marker_load_id, transaction, transaction_started_loaded, retry_epoch) =
            closing;
        if let Some(marker_epoch) = marker_epoch {
            let load_id = transaction
                .as_ref()
                .and_then(MediaUpdateTransaction::load_id)
                .or(marker_load_id);
            ipc.finish_media_marker(marker_epoch, load_id);
        }
        let Some(transaction) = transaction else {
            return;
        };
        let missing = metadata_fields_to_retry(&transaction);
        if missing.is_empty() {
            finish_media_transaction(
                context,
                transaction,
                transaction_started_loaded,
                retry_epoch,
            )
            .await;
        } else {
            let context = context.clone();
            tokio::spawn(async move {
                retry_media_metadata(
                    context,
                    transaction,
                    missing,
                    transaction_started_loaded,
                    retry_epoch,
                )
                .await;
            });
        }
        return;
    }
    if line.starts_with("ANS_") {
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim_start_matches("ANS_").to_ascii_lowercase();
            let value = value.trim();
            if key == "syncplay_load_id" {
                if let Ok(load_id) = value.parse::<u64>() {
                    ipc.mark_load_marker(load_id);
                    let marker_epoch = {
                        let mut media = media_updates.lock();
                        media.marker_load_id = Some(load_id);
                        if let Some(transaction) = media.transaction.as_mut() {
                            update_media_transaction(transaction, &key, value);
                        }
                        media.marker_epoch
                    };
                    if let Some(marker_epoch) = marker_epoch {
                        ipc.bind_media_marker(marker_epoch, load_id);
                    }
                    return;
                }
            }
            if let Some(transaction) = media_updates.lock().transaction.as_mut() {
                update_media_transaction(transaction, &key, value);
            }
        }
        return;
    }
    if line.contains("<paused=") && line.contains(", pos=") {
        if let Some((mut paused, mut position)) = parse_pause_position(line) {
            if paused.is_none() || position.is_none() {
                if let Some(app_state) = state.upgrade() {
                    let global = app_state.effective_global_state();
                    paused = paused.or(Some(global.paused));
                    position = position.or(Some(global.position));
                }
            }
            ipc.update_pause_and_position(paused, position);
        }
        return;
    }
    if line.contains("Error parsing option") || line.contains("Error parsing commandline option") {
        warn!("mpv reported an option parsing error: {}", line);
        if let Some(state) = state.upgrade() {
            emit_error_message(
                &state,
                "This version of mpv is not compatible with Syncplay. Please use mpv >= 0.23.0.",
            );
        }
        ipc.set_ready(true);
    }
    if line.contains("Could not open pipe at '/dev/stdin'") {
        if let Some(state) = state.upgrade() {
            emit_error_message(
                &state,
                "This version of mpv is not compatible with Syncplay. Please use mpv >= 0.23.0.",
            );
            let app_state_clone = state.clone();
            let instance_id = context.instance_id;
            tokio::spawn(async move {
                let _ =
                    crate::player::controller::stop_player_instance(&app_state_clone, instance_id)
                        .await;
            });
        }
        ipc.set_ready(true);
    }
}

fn metadata_fields_to_retry(transaction: &MediaUpdateTransaction) -> Vec<MpvMetadataField> {
    transaction
        .missing_fields()
        .into_iter()
        .filter_map(|field| match field {
            MediaField::Filename => Some(MpvMetadataField::Filename),
            MediaField::Duration => Some(MpvMetadataField::Duration),
            MediaField::Path => Some(MpvMetadataField::Path),
            MediaField::Size => None,
        })
        .collect()
}

fn media_retry_is_current(
    context: &MpvLineContext,
    load_id: Option<u64>,
    retry_epoch: u64,
) -> bool {
    if !is_current_player_context(context)
        || context.media_updates.lock().retry_epoch != retry_epoch
    {
        return false;
    }
    load_id.is_none_or(|load_id| context.ipc.is_load_active(load_id))
}

async fn retry_media_metadata(
    context: MpvLineContext,
    mut transaction: MediaUpdateTransaction,
    mut missing: Vec<MpvMetadataField>,
    transaction_started_loaded: bool,
    retry_epoch: u64,
) {
    let load_id = transaction.load_id();
    loop {
        if !media_retry_is_current(&context, load_id, retry_epoch) {
            debug!(?load_id, "Cancelling superseded mpv metadata retry");
            return;
        }

        let mut unresolved = Vec::new();
        for field in missing {
            if !media_retry_is_current(&context, load_id, retry_epoch) {
                return;
            }
            let Some(result) = refresh_media_field_while_current(
                &context,
                &mut transaction,
                field,
                load_id,
                retry_epoch,
            )
            .await
            else {
                return;
            };
            match result {
                Ok(true) => {}
                Ok(false) => unresolved.push(field),
                Err(error) => {
                    context
                        .ipc
                        .mark_unhealthy(format!("MPV media metadata refresh failed: {error}"));
                    return;
                }
            }
        }

        if unresolved.is_empty() {
            if media_retry_is_current(&context, load_id, retry_epoch) {
                finish_media_transaction(
                    &context,
                    transaction,
                    transaction_started_loaded,
                    retry_epoch,
                )
                .await;
            }
            return;
        }
        missing = unresolved;
        tokio::time::sleep(MPV_METADATA_RETRY_DELAY).await;
    }
}

async fn refresh_media_field_while_current(
    context: &MpvLineContext,
    transaction: &mut MediaUpdateTransaction,
    field: MpvMetadataField,
    load_id: Option<u64>,
    retry_epoch: u64,
) -> Option<anyhow::Result<bool>> {
    let refresh = refresh_media_field(&context.ipc, transaction, field);
    tokio::pin!(refresh);
    loop {
        tokio::select! {
            result = &mut refresh => return Some(result),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if !media_retry_is_current(context, load_id, retry_epoch) {
                    return None;
                }
            }
        }
    }
}

async fn refresh_media_field(
    ipc: &Arc<MpvIpc>,
    transaction: &mut MediaUpdateTransaction,
    field: MpvMetadataField,
) -> anyhow::Result<bool> {
    match field {
        MpvMetadataField::Filename => {
            let value = ipc.get_property_value("filename").await?;
            let Some(value) = value
                .as_ref()
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return Ok(false);
            };
            transaction.set_filename(Some(value.to_string()));
        }
        MpvMetadataField::Duration => {
            let mut value = ipc.get_property_value("duration").await?;
            if value.as_ref().and_then(Value::as_f64).is_none() {
                value = ipc.get_property_value("length").await?;
            }
            let Some(value) = value.as_ref().and_then(Value::as_f64) else {
                return Ok(false);
            };
            transaction.set_duration(Some(value));
        }
        MpvMetadataField::Path => {
            let value = ipc.get_property_value("path").await?;
            let Some(value) = value
                .as_ref()
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return Ok(false);
            };
            transaction.set_path(Some(value.to_string()));
            transaction.set_size(None);
        }
    }
    Ok(true)
}

async fn finish_media_transaction(
    context: &MpvLineContext,
    transaction: MediaUpdateTransaction,
    transaction_started_loaded: bool,
    retry_epoch: u64,
) {
    let app_state = context.state.upgrade();
    let transition_guard = match app_state.as_ref() {
        Some(app_state) if is_current_player_context(context) => {
            Some(app_state.playback.media_transition.lock().await)
        }
        _ => None,
    };
    if !is_current_player_context(context) {
        return;
    }
    let load_id = transaction.load_id();
    let explicit_external = load_id.is_none();
    let commit = {
        let mut media = context.media_updates.lock();
        if media.retry_epoch != retry_epoch {
            debug!(
                expected = retry_epoch,
                actual = media.retry_epoch,
                "Ignoring superseded mpv media transaction"
            );
            return;
        }
        if context.kind == PlayerKind::Iina
            && explicit_external
            && transaction.filename() == Some("placeholder.png")
        {
            context.file_loaded.store(false, Ordering::SeqCst);
            return;
        }
        if explicit_external {
            media.state.commit_external(transaction)
        } else {
            media.state.commit(transaction)
        }
    };
    let mut failed_load = None;
    match commit {
        MediaCommit::Committed(snapshot) => {
            context.ipc.commit_media_snapshot(&snapshot);
            context.file_loaded.store(true, Ordering::SeqCst);
            if let Some(app_state) = app_state.as_ref() {
                if transition_guard.is_none() || !is_current_player_context(context) {
                    return;
                }
                let stable = context.ipc.get_state();
                let commit = {
                    let _lifecycle_guard = app_state.player_lifecycle.lock().await;
                    if !is_current_player_context(context) {
                        return;
                    }
                    let player = app_state.player.lock().clone();
                    match (explicit_external, player.as_ref()) {
                        (true, Some(player)) => {
                            commit_external_player_state(app_state, player, &stable).await
                        }
                        _ => {
                            commit_player_state(
                                app_state,
                                player.as_ref(),
                                &stable,
                                load_id.map(LoadId),
                            )
                            .await
                        }
                    }
                };
                if let Err(error) = commit {
                    warn!("Failed to commit mpv media: {}", error);
                }
            }
        }
        MediaCommit::Incomplete { load_id, missing } => {
            context.file_loaded.store(false, Ordering::SeqCst);
            warn!(
                "Ignoring incomplete mpv media update; missing: {:?}",
                missing
            );
            failed_load = load_id;
        }
        MediaCommit::MissingIdentity { load_id } => {
            context.file_loaded.store(false, Ordering::SeqCst);
            warn!("Ignoring mpv media update without filename or path");
            failed_load = load_id;
        }
        result @ MediaCommit::Stale { .. } => {
            debug!("Ignoring stale mpv media update: {:?}", result);
            context
                .file_loaded
                .store(transaction_started_loaded, Ordering::SeqCst);
        }
        result @ MediaCommit::TargetMismatch { load_id, .. } => {
            debug!("Ignoring stale mpv media update: {:?}", result);
            failed_load = Some(load_id);
        }
    }
    drop(transition_guard);
    if let Some(load_id) = failed_load {
        fail_media_load(context, load_id).await;
    }
}

async fn fail_media_load(context: &MpvLineContext, load_id: u64) {
    let cancelled_current_load = {
        let mut media = context.media_updates.lock();
        media.retry_epoch = media.retry_epoch.wrapping_add(1);
        media.transaction = None;
        media.transaction_started_loaded = false;
        media.state.cancel_load(load_id)
    };
    context.ipc.finish_generation_gate(load_id);
    if !cancelled_current_load {
        debug!(load_id, "Ignoring failure from a superseded mpv load");
        return;
    }
    context.file_loaded.store(false, Ordering::SeqCst);
    if let Some(state) = context.state.upgrade() {
        playback_runtime::fail_load(&state, LoadId(load_id)).await;
    }
}

fn handle_generation_start(context: &MpvLineContext, load_id: u64, target: Option<String>) {
    if !context.ipc.start_load(load_id) {
        debug!(load_id, "Ignoring unprepared mpv generation start");
        return;
    }
    if let Some(target) = target {
        context
            .media_updates
            .lock()
            .state
            .retarget_load(load_id, target);
    }
}

async fn handle_end_file(
    context: &MpvLineContext,
    reason: &EndFileReason,
    load_id: u64,
    propagated: bool,
) {
    if matches!(reason, EndFileReason::Quit) {
        return;
    }
    let Some((load_id, marker_seen)) = context.ipc.end_load(load_id, propagated) else {
        return;
    };
    if matches!(reason, EndFileReason::Redirect) {
        context.ipc.retire_active_load(load_id);
        context.ipc.finish_generation_gate(load_id);
        return;
    }
    if !marker_seen {
        fail_media_load(context, load_id).await;
    }
}

fn update_media_transaction(transaction: &mut MediaUpdateTransaction, key: &str, value: &str) {
    let available = !value.is_empty() && value != "(unavailable)" && value != "nil";
    if !available {
        return;
    }
    match key {
        "syncplay_load_id" => transaction.set_load_id(value.parse::<u64>().ok()),
        "filename" => transaction.set_filename(Some(value.to_string())),
        "path" => {
            transaction.set_path(Some(value.to_string()));
            transaction.set_size(None);
        }
        "length" | "duration" => {
            if let Ok(duration) = value.parse::<f64>() {
                transaction.set_duration(Some(duration));
            }
        }
        _ => {}
    }
}

fn extract_tag(line: &str, tag: &str) -> Option<String> {
    let start_tag = format!("<{}>", tag);
    let end_tag = format!("</{}>", tag);
    let start = line.find(&start_tag)? + start_tag.len();
    let end = line.find(&end_tag)?;
    Some(line[start..end].to_string())
}

fn parse_pause_position(line: &str) -> Option<(Option<bool>, Option<f64>)> {
    let trimmed = line.trim_matches(|c| c == '<' || c == '>');
    let mut paused = None;
    let mut position = None;
    for part in trimmed.split(',') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("paused=") {
            paused = match value {
                "true" => Some(true),
                "false" => Some(false),
                "nil" => None,
                _ => None,
            };
        } else if let Some(value) = part.strip_prefix("pos=") {
            position = value.parse::<f64>().ok();
        }
    }
    Some((paused, position))
}

fn build_syncplayintf_options(
    state: &Arc<AppState>,
    osc_visibility_change_compatible: bool,
) -> String {
    let config = state.config.lock().clone();
    let server_features = state.server_features.lock().clone();
    let mut options = Vec::new();

    let bool_value = |value: bool| if value { "True" } else { "False" };

    options.push(format!(
        "chatInputEnabled={}",
        bool_value(config.user.chat_input_enabled)
    ));
    options.push(format!(
        "chatInputFontFamily={}",
        config.user.chat_input_font_family
    ));
    options.push(format!(
        "chatInputRelativeFontSize={}",
        config.user.chat_input_relative_font_size
    ));
    options.push(format!(
        "chatInputFontWeight={}",
        config.user.chat_input_font_weight
    ));
    options.push(format!(
        "chatInputFontUnderline={}",
        bool_value(config.user.chat_input_font_underline)
    ));
    options.push(format!(
        "chatInputFontColor={}",
        config.user.chat_input_font_color
    ));
    options.push(format!(
        "chatInputPosition={}",
        match config.user.chat_input_position {
            crate::config::ChatInputPosition::Top => "Top",
            crate::config::ChatInputPosition::Middle => "Middle",
            crate::config::ChatInputPosition::Bottom => "Bottom",
        }
    ));
    options.push(format!(
        "chatOutputFontFamily={}",
        config.user.chat_output_font_family
    ));
    options.push(format!(
        "chatOutputRelativeFontSize={}",
        config.user.chat_output_relative_font_size
    ));
    options.push(format!(
        "chatOutputFontWeight={}",
        config.user.chat_output_font_weight
    ));
    options.push(format!(
        "chatOutputFontUnderline={}",
        bool_value(config.user.chat_output_font_underline)
    ));
    options.push(format!(
        "chatOutputMode={}",
        match config.user.chat_output_mode {
            crate::config::ChatOutputMode::Chatroom => "Chatroom",
            crate::config::ChatOutputMode::Scrolling => "Scrolling",
        }
    ));
    options.push(format!("chatMaxLines={}", config.user.chat_max_lines));
    options.push(format!("chatTopMargin={}", config.user.chat_top_margin));
    options.push(format!("chatLeftMargin={}", config.user.chat_left_margin));
    options.push(format!(
        "chatBottomMargin={}",
        config.user.chat_bottom_margin
    ));
    options.push(format!(
        "chatDirectInput={}",
        bool_value(config.user.chat_direct_input)
    ));
    options.push(format!(
        "notificationTimeout={}",
        config.user.notification_timeout
    ));
    options.push(format!("alertTimeout={}", config.user.alert_timeout));
    options.push(format!("chatTimeout={}", config.user.chat_timeout));
    options.push(format!(
        "chatOutputEnabled={}",
        bool_value(config.user.chat_output_enabled)
    ));

    let max_chat = server_features.max_chat_message_length.unwrap_or(150);
    options.push(format!("MaxChatMessageLength={}", max_chat));
    options.push("inputPromptStartCharacter=〉".to_string());
    options.push("inputPromptEndCharacter= 〈".to_string());
    options.push("backslashSubstituteCharacter=＼".to_string());

    options.push(format!(
        "mpv-key-tab-hint={}",
        "[TAB] to toggle access to alphabet row key shortcuts."
    ));
    options.push(format!(
        "mpv-key-hint={}",
        "[ENTER] to send message. [ESC] to escape chat mode."
    ));
    options.push(format!(
        "alphakey-mode-warning-first-line={}",
        "You can temporarily use old mpv bindings with a-z keys."
    ));
    options.push(format!(
        "alphakey-mode-warning-second-line={}",
        "Press [TAB] to return to Syncplay chat mode."
    ));
    options.push(format!(
        "OscVisibilityChangeCompatible={}",
        bool_value(osc_visibility_change_compatible)
    ));

    options.join(", ")
}

async fn apply_osd_position(ipc: &Arc<MpvIpc>, state: &Arc<AppState>) {
    let config = state.config.lock().clone();
    let should_move = config.user.chat_move_osd
        && (config.user.chat_output_enabled
            || (config.user.chat_input_enabled
                && matches!(
                    config.user.chat_input_position,
                    crate::config::ChatInputPosition::Top
                )));
    if !should_move {
        return;
    }
    let _ = ipc
        .send_command_async(MpvCommand::set_property(
            "osd-align-y",
            Value::String("bottom".to_string()),
            0,
        ))
        .await;
    let _ = ipc
        .send_command_async(MpvCommand::set_property(
            "osd-margin-y",
            Value::Number(serde_json::Number::from(config.user.chat_osd_margin)),
            0,
        ))
        .await;
}

fn sanitize_mpv_text(input: &str) -> String {
    let mut text = input.replace("\r", "").replace("\n", "\\n");
    text = text.replace('\\', MPV_INPUT_BACKSLASH_SUBSTITUTE);
    text = text.replace('"', "'");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::client::playback::{PlaybackEffect, PlaybackEvent};
    use crate::config::PrivacyMode;
    use crate::network::connection::Connection;
    use crate::network::fake_server::FakeSyncplayServer;
    use crate::network::messages::{FileInfo, FileSizeInfo, ProtocolMessage};
    use crate::player::backend::FakePlayerBackend;
    #[cfg(unix)]
    use tokio::io::AsyncWriteExt;
    use tokio::time::timeout;

    fn start_server_load(
        playback: &mut crate::client::playback::PlaybackState,
        index: usize,
        reset_position: bool,
    ) -> LoadId {
        assert_eq!(
            playback.reduce(PlaybackEvent::LocalSelect {
                index,
                reset_position,
            }),
            vec![PlaybackEffect::SendPlaylistIndex {
                index,
                reset_position,
            }]
        );
        let effects = playback.reduce(PlaybackEvent::ServerIndex {
            index: Some(index),
            reset_position,
        });
        let [PlaybackEffect::Load { load_id, .. }] = effects.as_slice() else {
            panic!("expected one load effect, got {effects:?}");
        };
        *load_id
    }

    #[test]
    fn syncplay_load_id_is_collected_inside_the_media_transaction() {
        let media = MediaUpdateState::new();
        let mut transaction = media.begin_update(None);

        update_media_transaction(&mut transaction, "syncplay_load_id", "42");

        assert_eq!(transaction.load_id(), Some(42));
    }

    #[tokio::test]
    async fn tagged_external_marker_clears_loaded_state_and_closes_the_gate() {
        let app_state = AppState::new();
        let ipc = Arc::new(MpvIpc::new("unused"));
        let file_loaded = Arc::new(AtomicBool::new(true));
        let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
        let reset_ignore_until = Arc::new(Mutex::new(None));
        let context = test_line_context(
            &app_state,
            &ipc,
            &file_loaded,
            &media_updates,
            &reset_ignore_until,
        );

        feed_test_line(&context, MpvLineSource::IpcLog, "<SyncplayUpdateFile>").await;

        assert!(!file_loaded.load(Ordering::SeqCst));
        assert!(!ipc.is_ready_for_send());
    }

    #[tokio::test]
    async fn duplicate_marker_start_keeps_the_existing_transaction_and_gate_epoch() {
        let app_state = AppState::new();
        let ipc = Arc::new(MpvIpc::new("unused"));
        let file_loaded = Arc::new(AtomicBool::new(true));
        let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
        let reset_ignore_until = Arc::new(Mutex::new(None));
        let context = test_line_context(
            &app_state,
            &ipc,
            &file_loaded,
            &media_updates,
            &reset_ignore_until,
        );

        feed_test_line(&context, MpvLineSource::IpcLog, "<SyncplayUpdateFile>").await;
        let first_epoch = media_updates
            .lock()
            .marker_epoch
            .expect("first marker epoch missing");

        feed_test_line(&context, MpvLineSource::IpcLog, "<SyncplayUpdateFile>").await;

        let media = media_updates.lock();
        assert_eq!(media.marker_epoch, Some(first_epoch));
        assert!(media.transaction.is_some());
        assert_eq!(media.retry_epoch, first_epoch.wrapping_add(1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn queued_generation_load_waits_for_an_external_marker_to_close() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (load_tx, mut load_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: Value = serde_json::from_str(&line).unwrap();
                let command = message["command"][0].as_str().unwrap();
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
                if command == "script-message-to" && message["command"][2] == "syncplay-load-file" {
                    load_tx
                        .send(message["command"][4].as_str().unwrap().to_string())
                        .unwrap();
                    break;
                }
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let _events = ipc.connect().await.unwrap();
        let ipc = Arc::new(ipc);
        let app_state = AppState::new();
        let file_loaded = Arc::new(AtomicBool::new(true));
        let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
        let reset_ignore_until = Arc::new(Mutex::new(None));
        let context = test_line_context(
            &app_state,
            &ipc,
            &file_loaded,
            &media_updates,
            &reset_ignore_until,
        );

        feed_test_line(&context, MpvLineSource::IpcLog, "<SyncplayUpdateFile>").await;
        ipc.prepare_load(7);
        ipc.load_file_generation("queued.mkv", 7, LoadfileOptionsSyntax::Legacy)
            .await
            .unwrap();

        assert!(timeout(Duration::from_millis(100), load_rx.recv())
            .await
            .is_err());
        for line in [
            "ANS_filename=external.mkv",
            "ANS_duration=100",
            "ANS_path=/tmp/external.mkv",
            "</SyncplayUpdateFile>",
        ] {
            feed_test_line(&context, MpvLineSource::IpcLog, line).await;
        }

        assert_eq!(
            timeout(Duration::from_secs(1), load_rx.recv())
                .await
                .unwrap()
                .as_deref(),
            Some("queued.mkv")
        );
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn poll_without_a_loaded_file_uses_only_the_ready_lua_status_command() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (status_tx, mut status_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: Value = serde_json::from_str(&line).unwrap();
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
                if message["command"][0] == "script-message-to" {
                    status_tx.send(message["command"].clone()).unwrap();
                }
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let _events = ipc.connect().await.unwrap();
        let app_state = AppState::new();
        let backend = MpvBackend::new(
            PlayerKind::Mpv,
            ipc,
            Arc::downgrade(&app_state),
            Some(LoadfileOptionsSyntax::Legacy),
            true,
            None,
        );
        assert!(!backend.file_loaded.load(Ordering::SeqCst));

        backend.poll_state().await.unwrap();

        assert_eq!(
            timeout(Duration::from_secs(1), status_rx.recv())
                .await
                .unwrap(),
            Some(serde_json::json!([
                "script-message-to",
                "syncplayintf",
                "get_paused_and_position"
            ]))
        );
        timeout(Duration::from_secs(1), async {
            while backend.ipc.pending_request_count() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        backend.ipc.set_ready(false);
        backend.poll_state().await.unwrap();
        assert!(timeout(Duration::from_millis(100), status_rx.recv())
            .await
            .is_err());
        server.abort();
    }

    async fn assert_mpv_lifecycle_event_clears_player_state(event: MpvPlayerEvent) {
        let app_state = AppState::new();
        *app_state.player_connecting.lock() = true;
        *app_state.mpv_socket_path.lock() = Some("stale-socket".to_string());

        let backend = Arc::new(MpvBackend::new(
            PlayerKind::Mpv,
            MpvIpc::new("unused"),
            Arc::downgrade(&app_state),
            None,
            true,
            None,
        ));
        *app_state.player.lock() = Some(backend.clone());

        let (tx, rx) = mpsc::unbounded_channel();
        backend.spawn_event_loop(rx);
        tx.send(event).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(app_state.player.lock().is_none());
        assert!(app_state.player_process.lock().is_none());
        assert!(!*app_state.player_connecting.lock());
        assert!(app_state.mpv_socket_path.lock().is_none());
    }

    async fn connect_fake_server(app_state: &Arc<AppState>) -> FakeSyncplayServer {
        let server = FakeSyncplayServer::start().await.unwrap();
        let connection = Arc::new(Connection::new());
        let (_rx, _peer) = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();
        connection.set_authenticated();
        *app_state.connection.lock() = Some(connection);
        server
    }

    fn test_line_context(
        app_state: &Arc<AppState>,
        ipc: &Arc<MpvIpc>,
        file_loaded: &Arc<AtomicBool>,
        media_updates: &Arc<Mutex<MpvMediaUpdates>>,
        reset_ignore_until: &Arc<Mutex<Option<Instant>>>,
    ) -> MpvLineContext {
        MpvLineContext {
            instance_id: 0,
            kind: PlayerKind::Mpv,
            ipc: ipc.clone(),
            state: Arc::downgrade(app_state),
            file_loaded: file_loaded.clone(),
            media_updates: media_updates.clone(),
            reset_ignore_until: reset_ignore_until.clone(),
            osc_visibility_change_compatible: true,
        }
    }

    async fn feed_test_line(context: &MpvLineContext, source: MpvLineSource, line: &str) {
        handle_syncplayintf_line(context, source, line).await;
    }

    async fn drive_delayed_file_update(
        app_state: &Arc<AppState>,
        ipc: &Arc<MpvIpc>,
        file_loaded: &Arc<AtomicBool>,
        media_updates: &Arc<Mutex<MpvMediaUpdates>>,
        reset_ignore_until: &Arc<Mutex<Option<Instant>>>,
        path: &str,
    ) {
        let context = test_line_context(
            app_state,
            ipc,
            file_loaded,
            media_updates,
            reset_ignore_until,
        );

        feed_test_line(&context, MpvLineSource::IpcLog, "<SyncplayUpdateFile>").await;
        assert!(!file_loaded.load(Ordering::SeqCst));

        feed_test_line(
            &context,
            MpvLineSource::IpcLog,
            "ANS_filename=delayed-file.mkv",
        )
        .await;
        feed_test_line(&context, MpvLineSource::IpcLog, "ANS_duration=123.5").await;
        feed_test_line(&context, MpvLineSource::IpcLog, &format!("ANS_path={path}")).await;
        feed_test_line(&context, MpvLineSource::IpcLog, "<paused=true, pos=0>").await;
        feed_test_line(&context, MpvLineSource::IpcLog, "</SyncplayUpdateFile>").await;
    }

    #[cfg(unix)]
    async fn connect_metadata_test_ipc(
        delay_first_filename: bool,
    ) -> (
        Arc<MpvIpc>,
        mpsc::UnboundedReceiver<MpvPlayerEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("mpv.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let _temp_dir = temp_dir;
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            let mut filename_requests = 0usize;
            while let Some(line) = lines.next_line().await.unwrap() {
                let message: Value = serde_json::from_str(&line).unwrap();
                let command = message["command"][0].as_str().unwrap();
                let Some(request_id) = message["request_id"].as_u64() else {
                    continue;
                };
                let mut response = serde_json::json!({
                    "request_id": request_id,
                    "error": "success"
                });
                if command == "get_property" {
                    match message["command"][1].as_str().unwrap() {
                        "filename" => {
                            filename_requests += 1;
                            if delay_first_filename && filename_requests == 1 {
                                tokio::time::sleep(Duration::from_millis(500)).await;
                                response["data"] = Value::String("a.mkv".to_string());
                            } else if filename_requests == 1 {
                                response["error"] = Value::String("property unavailable".into());
                            } else {
                                response["data"] = Value::String("restored.mkv".to_string());
                            }
                        }
                        "duration" | "length" => {
                            response["data"] = serde_json::json!(123.5);
                        }
                        "path" => {
                            response["data"] = Value::String("/tmp/restored.mkv".to_string());
                        }
                        property => panic!("unexpected metadata property: {property}"),
                    }
                }
                write_half
                    .write_all(format!("{}\n", response).as_bytes())
                    .await
                    .unwrap();
            }
        });

        let mut ipc = MpvIpc::new(socket_path.to_string_lossy());
        let events = ipc.connect().await.unwrap();
        (Arc::new(ipc), events, server)
    }

    #[test]
    fn get_state_reports_zero_position_while_recently_reset() {
        let app_state = AppState::new();
        app_state
            .client_state
            .set_global_state(42.0, true, Some("peer".to_string()));

        let backend = MpvBackend::new(
            PlayerKind::Mpv,
            MpvIpc::new("unused"),
            Arc::downgrade(&app_state),
            None,
            true,
            None,
        );
        backend.file_loaded.store(true, Ordering::SeqCst);
        backend
            .ipc
            .update_pause_and_position(Some(false), Some(12.0));
        backend.mark_reset(false);

        let state = backend.get_state();

        assert_eq!(state.position, Some(0.0));
        assert_eq!(state.paused, Some(true));
    }

    #[tokio::test]
    async fn paused_player_without_lua_markers_triggers_the_unresponsive_watchdog() {
        let app_state = AppState::new();
        let backend = Arc::new(MpvBackend::new(
            PlayerKind::Mpv,
            MpvIpc::new("unused"),
            Arc::downgrade(&app_state),
            None,
            true,
            None,
        ));
        backend.file_loaded.store(true, Ordering::SeqCst);
        backend
            .ipc
            .update_pause_and_position(Some(true), Some(12.0));
        backend.ipc.set_last_position_update_for_test(
            Instant::now() - MPV_UNRESPONSIVE_THRESHOLD - Duration::from_secs(1),
        );
        *app_state.player.lock() = Some(backend.clone());

        let _ = backend.get_state();
        timeout(Duration::from_secs(1), async {
            loop {
                if app_state.player.lock().is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        assert!(app_state.player.lock().is_none());
    }

    #[test]
    fn newer_non_reset_load_clears_previous_reset_window() {
        let app_state = AppState::new();
        let backend = MpvBackend::new(
            PlayerKind::Mpv,
            MpvIpc::new("unused"),
            Arc::downgrade(&app_state),
            None,
            true,
            None,
        );

        backend.begin_file_load(1, "reset.mkv");
        backend.mark_reset(false);
        assert!(backend.recently_reset());

        backend.begin_file_load(2, "continue.mkv");

        assert!(!backend.recently_reset());
    }

    #[test]
    fn syncplay_update_file_refreshes_metadata_after_delayed_term_messages() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let ipc = Arc::new(MpvIpc::new("unused"));
            let file_loaded = Arc::new(AtomicBool::new(false));
            let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
            let reset_ignore_until = Arc::new(Mutex::new(None));
            drive_delayed_file_update(
                &app_state,
                &ipc,
                &file_loaded,
                &media_updates,
                &reset_ignore_until,
                "/tmp/delayed-file.mkv",
            )
            .await;
            tokio::time::sleep(Duration::from_millis(80)).await;

            let state = ipc.get_state();
            assert!(file_loaded.load(Ordering::SeqCst));
            assert_eq!(state.filename.as_deref(), Some("delayed-file.mkv"));
            assert_eq!(state.duration, Some(123.5));
            assert_eq!(state.path.as_deref(), Some("/tmp/delayed-file.mkv"));
            assert_eq!(state.paused, Some(true));
        });
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unavailable_media_metadata_retries_until_the_same_snapshot_is_complete() {
        let app_state = AppState::new();
        let (ipc, _events, server) = connect_metadata_test_ipc(false).await;
        let file_loaded = Arc::new(AtomicBool::new(false));
        let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
        let reset_ignore_until = Arc::new(Mutex::new(None));
        let context = test_line_context(
            &app_state,
            &ipc,
            &file_loaded,
            &media_updates,
            &reset_ignore_until,
        );

        for line in [
            "<SyncplayUpdateFile>",
            "ANS_filename=(unavailable)",
            "ANS_duration=(unavailable)",
            "ANS_path=(unavailable)",
            "</SyncplayUpdateFile>",
        ] {
            feed_test_line(&context, MpvLineSource::IpcLog, line).await;
        }
        assert!(!file_loaded.load(Ordering::SeqCst));

        timeout(Duration::from_secs(2), async {
            while !file_loaded.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let player_state = ipc.get_state();
        assert_eq!(player_state.filename.as_deref(), Some("restored.mkv"));
        assert_eq!(player_state.duration, Some(123.5));
        assert_eq!(player_state.path.as_deref(), Some("/tmp/restored.mkv"));
        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_external_marker_cancels_the_previous_metadata_retry() {
        let app_state = AppState::new();
        let (ipc, _events, server) = connect_metadata_test_ipc(true).await;
        let file_loaded = Arc::new(AtomicBool::new(false));
        let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
        let reset_ignore_until = Arc::new(Mutex::new(None));
        let context = test_line_context(
            &app_state,
            &ipc,
            &file_loaded,
            &media_updates,
            &reset_ignore_until,
        );

        for line in [
            "<SyncplayUpdateFile>",
            "ANS_filename=(unavailable)",
            "ANS_duration=100",
            "ANS_path=/tmp/a.mkv",
            "</SyncplayUpdateFile>",
        ] {
            feed_test_line(&context, MpvLineSource::IpcLog, line).await;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(ipc.pending_request_count(), 1);

        for line in [
            "<SyncplayUpdateFile>",
            "ANS_filename=b.mkv",
            "ANS_duration=200",
            "ANS_path=/tmp/b.mkv",
            "</SyncplayUpdateFile>",
        ] {
            feed_test_line(&context, MpvLineSource::IpcLog, line).await;
        }
        timeout(Duration::from_secs(1), async {
            while ipc.pending_request_count() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(ipc.get_state().filename.as_deref(), Some("b.mkv"));

        tokio::time::sleep(Duration::from_millis(550)).await;
        let player_state = ipc.get_state();
        assert!(file_loaded.load(Ordering::SeqCst));
        assert_eq!(player_state.filename.as_deref(), Some("b.mkv"));
        assert_eq!(player_state.path.as_deref(), Some("/tmp/b.mkv"));
        server.abort();
    }

    #[tokio::test]
    async fn closing_marker_epoch_is_rechecked_after_waiting_for_transition_lock() {
        let app_state = AppState::new();
        let ipc = Arc::new(MpvIpc::new("unused"));
        let file_loaded = Arc::new(AtomicBool::new(false));
        let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
        let reset_ignore_until = Arc::new(Mutex::new(None));
        let context = test_line_context(
            &app_state,
            &ipc,
            &file_loaded,
            &media_updates,
            &reset_ignore_until,
        );
        let mut transaction = media_updates.lock().state.begin_update(None);
        transaction.set_filename(Some("a.mkv".to_string()));
        transaction.set_duration(Some(100.0));
        transaction.set_path(Some("/tmp/a.mkv".to_string()));
        transaction.set_size(None);

        let transition_guard = app_state.playback.media_transition.lock().await;
        let finish_context = context.clone();
        let finish = tokio::spawn(async move {
            finish_media_transaction(&finish_context, transaction, false, 0).await;
        });
        feed_test_line(&context, MpvLineSource::IpcLog, "<SyncplayUpdateFile>").await;
        drop(transition_guard);
        finish.await.unwrap();

        assert!(!file_loaded.load(Ordering::SeqCst));
        assert!(ipc.get_state().filename.is_none());
    }

    #[tokio::test]
    async fn iina_startup_placeholder_is_ignored_without_generalizing_the_filename() {
        async fn feed_snapshot(kind: PlayerKind) -> (bool, Option<String>) {
            let app_state = AppState::new();
            let ipc = Arc::new(MpvIpc::new("unused"));
            let file_loaded = Arc::new(AtomicBool::new(false));
            let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
            let reset_ignore_until = Arc::new(Mutex::new(None));
            let mut context = test_line_context(
                &app_state,
                &ipc,
                &file_loaded,
                &media_updates,
                &reset_ignore_until,
            );
            context.kind = kind;
            for line in [
                "<SyncplayUpdateFile>",
                "ANS_filename=placeholder.png",
                "ANS_duration=0",
                "ANS_path=/resources/placeholder.png",
                "</SyncplayUpdateFile>",
            ] {
                feed_test_line(&context, MpvLineSource::IpcLog, line).await;
            }
            (file_loaded.load(Ordering::SeqCst), ipc.get_state().filename)
        }

        assert_eq!(feed_snapshot(PlayerKind::Iina).await, (false, None));
        assert_eq!(
            feed_snapshot(PlayerKind::Mpv).await,
            (true, Some("placeholder.png".to_string()))
        );
    }

    #[test]
    fn stdout_cannot_duplicate_ipc_control_or_media_lines() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let ipc = Arc::new(MpvIpc::new("unused"));
            let file_loaded = Arc::new(AtomicBool::new(false));
            let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
            let reset_ignore_until = Arc::new(Mutex::new(None));
            let context = test_line_context(
                &app_state,
                &ipc,
                &file_loaded,
                &media_updates,
                &reset_ignore_until,
            );

            for (source, line) in [
                (MpvLineSource::IpcLog, "<SyncplayUpdateFile>"),
                (MpvLineSource::IpcLog, "ANS_filename=canonical.mkv"),
                (MpvLineSource::Stdout, "<SyncplayUpdateFile>"),
                (MpvLineSource::Stdout, "ANS_filename=canonical.mkv"),
                (MpvLineSource::IpcLog, "ANS_duration=42"),
                (MpvLineSource::IpcLog, "ANS_path=/tmp/canonical.mkv"),
                (MpvLineSource::Stdout, "</SyncplayUpdateFile>"),
            ] {
                feed_test_line(&context, source, line).await;
            }
            assert!(!file_loaded.load(Ordering::SeqCst));

            feed_test_line(&context, MpvLineSource::IpcLog, "</SyncplayUpdateFile>").await;

            assert!(file_loaded.load(Ordering::SeqCst));
            assert_eq!(ipc.get_state().filename.as_deref(), Some("canonical.mkv"));

            ipc.update_pause_and_position(None, None);
            feed_test_line(&context, MpvLineSource::Stdout, "<paused=false, pos=19>").await;
            assert_eq!(ipc.get_state().position, None);

            feed_test_line(&context, MpvLineSource::IpcLog, "<paused=false, pos=19>").await;
            assert_eq!(ipc.get_state().position, Some(19.0));
        });
    }

    #[tokio::test]
    async fn nil_pause_and_position_fall_back_to_the_global_state() {
        let app_state = AppState::new();
        app_state
            .client_state
            .set_global_state(23.5, true, Some("remote".to_string()));
        let ipc = Arc::new(MpvIpc::new("unused"));
        let file_loaded = Arc::new(AtomicBool::new(true));
        let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
        let reset_ignore_until = Arc::new(Mutex::new(None));
        let context = test_line_context(
            &app_state,
            &ipc,
            &file_loaded,
            &media_updates,
            &reset_ignore_until,
        );

        feed_test_line(&context, MpvLineSource::IpcLog, "<paused=nil, pos=nil>").await;

        let state = ipc.get_state();
        assert_eq!(state.paused, Some(true));
        assert_eq!(state.position, Some(23.5));
    }

    #[tokio::test]
    async fn manual_open_supersedes_a_pending_load_without_discarding_a_written_marker() {
        let app_state = AppState::new();
        let player: Arc<dyn PlayerBackend> = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        *app_state.player.lock() = Some(player.clone());
        let load_id = {
            let mut playback = app_state.playback.state.lock();
            playback.reduce(PlaybackEvent::ServerPlaylist {
                items: vec!["requested.mkv".into(), "manual.mkv".into()],
            });
            start_server_load(&mut playback, 0, true)
        };
        let lease = playback_runtime::claim_load_for_issue(
            &app_state,
            load_id,
            "requested.mkv",
            "/media/requested.mkv",
            player,
        )
        .unwrap();

        let ipc = Arc::new(MpvIpc::new("unused"));
        let file_loaded = Arc::new(AtomicBool::new(false));
        let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
        media_updates
            .lock()
            .state
            .begin_load(load_id.0, "/media/requested.mkv");
        let reset_ignore_until = Arc::new(Mutex::new(None));
        let context = test_line_context(
            &app_state,
            &ipc,
            &file_loaded,
            &media_updates,
            &reset_ignore_until,
        );

        for line in [
            "<SyncplayUpdateFile>",
            "ANS_filename=manual.mkv",
            "ANS_duration=200",
            "ANS_path=/media/manual.mkv",
            "</SyncplayUpdateFile>",
        ] {
            feed_test_line(&context, MpvLineSource::IpcLog, line).await;
        }
        assert!(lease.is_cancelled());
        let playback = app_state.playback.snapshot();
        assert_eq!(
            playback
                .confirmed_media
                .as_ref()
                .map(|media| media.name.as_str()),
            Some("manual.mkv")
        );
        assert!(playback.pending_load.is_none());
        assert_eq!(
            playback.interrupted_load.as_ref().map(|load| load.id),
            Some(load_id)
        );

        for line in [
            "<SyncplayUpdateFile>".to_string(),
            format!("ANS_syncplay_load_id={}", load_id.0),
            "ANS_filename=requested.mkv".to_string(),
            "ANS_duration=100".to_string(),
            "ANS_path=/media/requested.mkv".to_string(),
            "</SyncplayUpdateFile>".to_string(),
        ] {
            feed_test_line(&context, MpvLineSource::IpcLog, &line).await;
        }
        let playback = app_state.playback.snapshot();
        assert_eq!(
            playback
                .confirmed_media
                .as_ref()
                .map(|media| media.name.as_str()),
            Some("requested.mkv")
        );
        assert!(playback.interrupted_load.is_none());
    }

    #[test]
    fn stale_generation_marker_preserves_the_current_media_gate() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let ipc = Arc::new(MpvIpc::new("unused"));
            let file_loaded = Arc::new(AtomicBool::new(false));
            let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
            let reset_ignore_until = Arc::new(Mutex::new(None));
            media_updates.lock().state.begin_load(2, "b.mkv");
            let context = test_line_context(
                &app_state,
                &ipc,
                &file_loaded,
                &media_updates,
                &reset_ignore_until,
            );

            for line in [
                "<SyncplayUpdateFile>",
                "ANS_syncplay_load_id=2",
                "ANS_filename=b.mkv",
                "ANS_duration=120",
                "ANS_path=/tmp/b.mkv",
                "</SyncplayUpdateFile>",
            ] {
                feed_test_line(&context, MpvLineSource::IpcLog, line).await;
            }
            assert!(file_loaded.load(Ordering::SeqCst));
            assert_eq!(ipc.get_state().filename.as_deref(), Some("b.mkv"));

            for line in [
                "<SyncplayUpdateFile>",
                "ANS_syncplay_load_id=1",
                "ANS_filename=a.mkv",
                "ANS_duration=100",
                "ANS_path=/tmp/a.mkv",
                "</SyncplayUpdateFile>",
            ] {
                feed_test_line(&context, MpvLineSource::IpcLog, line).await;
            }
            assert!(file_loaded.load(Ordering::SeqCst));
            assert_eq!(ipc.get_state().filename.as_deref(), Some("b.mkv"));

            media_updates.lock().state.begin_load(3, "c.mkv");
            file_loaded.store(false, Ordering::SeqCst);
            for line in [
                "<SyncplayUpdateFile>",
                "ANS_syncplay_load_id=2",
                "ANS_filename=b.mkv",
                "ANS_duration=120",
                "ANS_path=/tmp/b.mkv",
                "</SyncplayUpdateFile>",
            ] {
                feed_test_line(&context, MpvLineSource::IpcLog, line).await;
            }
            assert!(!file_loaded.load(Ordering::SeqCst));
            assert_eq!(
                media_updates.lock().state.active_load().map(|load| load.id),
                Some(3)
            );
        });
    }

    #[test]
    fn explicit_markers_do_not_commit_a_superseded_generation() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let ipc = Arc::new(MpvIpc::new("unused"));
            let file_loaded = Arc::new(AtomicBool::new(false));
            let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
            let reset_ignore_until = Arc::new(Mutex::new(None));
            {
                let mut media = media_updates.lock();
                media.state.begin_load(1, "a.mkv");
                media.state.cancel_load(1);
                media.state.begin_load(2, "b.mkv");
            }
            ipc.record_load_for_test(1);
            ipc.start_load(1);
            let context = test_line_context(
                &app_state,
                &ipc,
                &file_loaded,
                &media_updates,
                &reset_ignore_until,
            );

            for line in [
                "<SyncplayUpdateFile>",
                "ANS_syncplay_load_id=1",
                "Playing: /tmp/a.mkv",
                "ANS_filename=a.mkv",
                "ANS_duration=100",
                "ANS_path=/tmp/a.mkv",
                "</SyncplayUpdateFile>",
            ] {
                feed_test_line(&context, MpvLineSource::IpcLog, line).await;
            }
            assert!(!file_loaded.load(Ordering::SeqCst));
            assert_eq!(
                media_updates.lock().state.active_load().map(|load| load.id),
                Some(2)
            );

            ipc.record_load_for_test(2);
            ipc.start_load(2);
            handle_end_file(&context, &EndFileReason::Error, 1, false).await;
            for line in [
                "<SyncplayUpdateFile>",
                "ANS_syncplay_load_id=2",
                "ANS_filename=b.mkv",
                "ANS_duration=120",
                "ANS_path=/tmp/b.mkv",
                "</SyncplayUpdateFile>",
            ] {
                feed_test_line(&context, MpvLineSource::IpcLog, line).await;
            }
            assert!(file_loaded.load(Ordering::SeqCst));
            assert_eq!(ipc.get_state().filename.as_deref(), Some("b.mkv"));
            assert!(media_updates.lock().state.active_load().is_none());
        });
    }

    #[test]
    fn explicit_end_file_error_retires_orphan_before_next_marker() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let ipc = Arc::new(MpvIpc::new("unused"));
            let file_loaded = Arc::new(AtomicBool::new(false));
            let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
            let reset_ignore_until = Arc::new(Mutex::new(None));
            {
                let mut media = media_updates.lock();
                media.state.begin_load(1, "broken.mkv");
                media.state.cancel_load(1);
                media.state.begin_load(2, "working.mkv");
            }
            ipc.record_load_for_test(1);
            ipc.start_load(1);
            let context = test_line_context(
                &app_state,
                &ipc,
                &file_loaded,
                &media_updates,
                &reset_ignore_until,
            );

            handle_end_file(&context, &EndFileReason::Error, 1, false).await;
            ipc.record_load_for_test(2);
            ipc.start_load(2);
            for line in [
                "<SyncplayUpdateFile>",
                "ANS_syncplay_load_id=2",
                "ANS_filename=working.mkv",
                "ANS_duration=120",
                "ANS_path=/tmp/working.mkv",
                "</SyncplayUpdateFile>",
            ] {
                feed_test_line(&context, MpvLineSource::IpcLog, line).await;
            }

            assert!(file_loaded.load(Ordering::SeqCst));
            assert_eq!(ipc.get_state().filename.as_deref(), Some("working.mkv"));
            assert!(media_updates.lock().state.active_load().is_none());
        });
    }

    #[test]
    fn lua_eof_only_corrects_duration_for_the_pause_transition_owner() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            app_state.client_state.set_file_info(FileInfo {
                name: Some("movie.mkv".to_string()),
                size: Some(FileSizeInfo::Number(1)),
                duration: Some(120.0),
            });
            let ipc = Arc::new(MpvIpc::new("unused"));
            ipc.update_pause_and_position(Some(true), Some(119.4));
            let file_loaded = Arc::new(AtomicBool::new(true));
            let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
            let reset_ignore_until = Arc::new(Mutex::new(None));
            let context = test_line_context(
                &app_state,
                &ipc,
                &file_loaded,
                &media_updates,
                &reset_ignore_until,
            );

            feed_test_line(&context, MpvLineSource::IpcLog, "<eof>").await;

            assert_eq!(app_state.client_state.get_file_duration(), Some(119.4));
            assert!(app_state.playback.snapshot().pending_load.is_none());
        });
    }

    #[test]
    fn delayed_file_update_sends_stable_fileinfo_with_privacy_and_paused_state() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            app_state.config.lock().user.filename_privacy_mode = PrivacyMode::DoNotSend;
            app_state.config.lock().user.filesize_privacy_mode = PrivacyMode::DoNotSend;
            let mut server = connect_fake_server(&app_state).await;
            let ipc = Arc::new(MpvIpc::new("unused"));
            let file_loaded = Arc::new(AtomicBool::new(false));
            let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
            let reset_ignore_until = Arc::new(Mutex::new(None));

            drive_delayed_file_update(
                &app_state,
                &ipc,
                &file_loaded,
                &media_updates,
                &reset_ignore_until,
                "file:///tmp/delayed-file.mkv",
            )
            .await;

            let outbound = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(ProtocolMessage::Set { Set }) = server.next_received().await {
                        if let Some(file) = Set.file {
                            break file;
                        }
                    }
                }
            })
            .await
            .expect("timed out waiting for file update");

            let state = ipc.get_state();
            assert!(file_loaded.load(Ordering::SeqCst));
            assert_eq!(state.filename.as_deref(), Some("delayed-file.mkv"));
            assert_eq!(state.duration, Some(123.5));
            assert_eq!(state.path.as_deref(), Some("file:///tmp/delayed-file.mkv"));
            assert_eq!(state.paused, Some(true));
            assert_eq!(
                app_state.client_state.get_file().as_deref(),
                Some("**Hidden filename**")
            );
            assert_eq!(app_state.client_state.get_file_duration(), Some(123.5));
            assert_eq!(outbound.name.as_deref(), Some("**Hidden filename**"));
            assert!(matches!(outbound.size, Some(FileSizeInfo::Number(0))));
            assert_eq!(outbound.duration, Some(123.5));
            assert!(app_state.client_state.get_global_state().paused);
        });
    }

    #[test]
    fn mpv_socket_disconnect_clears_player_state() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert_mpv_lifecycle_event_clears_player_state(MpvPlayerEvent::SocketDisconnected)
                .await;
        });
    }

    #[test]
    fn mpv_quit_event_clears_player_state() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert_mpv_lifecycle_event_clears_player_state(MpvPlayerEvent::Quit).await;
        });
    }

    #[test]
    fn mpv_end_file_quit_clears_player_state() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert_mpv_lifecycle_event_clears_player_state(MpvPlayerEvent::EndFile {
                reason: EndFileReason::Quit,
                playlist_entry_id: None,
            })
            .await;
        });
    }

    #[test]
    fn uncorrelated_mpv_end_file_error_does_not_cancel_latest_load() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let backend = Arc::new(MpvBackend::new(
                PlayerKind::Mpv,
                MpvIpc::new("unused"),
                Arc::downgrade(&app_state),
                None,
                true,
                None,
            ));
            *app_state.player.lock() = Some(backend.clone() as Arc<dyn PlayerBackend>);
            let load_id = {
                let mut playback = app_state.playback.state.lock();
                playback.playlist_items = vec!["broken.mkv".to_string()];
                let load_id = start_server_load(&mut playback, 0, true);
                playback.reduce(PlaybackEvent::LoadStarted { load_id });
                load_id
            };
            backend.begin_file_load(load_id.0, "broken.mkv");

            let (tx, rx) = mpsc::unbounded_channel();
            backend.spawn_event_loop(rx);
            tx.send(MpvPlayerEvent::EndFile {
                reason: EndFileReason::Error,
                playlist_entry_id: None,
            })
            .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;

            assert_eq!(
                app_state
                    .playback
                    .snapshot()
                    .pending_load
                    .map(|load| load.id),
                Some(load_id)
            );
            assert!(backend.media_updates.lock().state.is_loading());
        });
    }

    #[test]
    fn superseded_end_file_cannot_clear_the_committed_media_gate() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let ipc = Arc::new(MpvIpc::new("unused"));
            let file_loaded = Arc::new(AtomicBool::new(true));
            let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
            let reset_ignore_until = Arc::new(Mutex::new(None));
            {
                let mut media = media_updates.lock();
                media.state.begin_load(1, "old.mkv");
                media.state.cancel_load(1);
                media.state.begin_load(2, "current.mkv");
                let mut update = media.state.begin_update(Some(2));
                update.set_filename(Some("current.mkv".to_string()));
                update.set_path(Some("/media/current.mkv".to_string()));
                update.set_duration(Some(90.0));
                update.set_size(Some(1));
                assert!(matches!(
                    media.state.commit(update),
                    MediaCommit::Committed(_)
                ));
            }
            let context = test_line_context(
                &app_state,
                &ipc,
                &file_loaded,
                &media_updates,
                &reset_ignore_until,
            );

            fail_media_load(&context, 1).await;

            assert!(file_loaded.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn tagged_end_file_failure_settles_only_the_matching_load() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            for reason in [EndFileReason::Error, EndFileReason::Stop] {
                let app_state = AppState::new();
                let backend = Arc::new(MpvBackend::new(
                    PlayerKind::Mpv,
                    MpvIpc::new("unused"),
                    Arc::downgrade(&app_state),
                    Some(LoadfileOptionsSyntax::Legacy),
                    true,
                    None,
                ));
                *app_state.player.lock() = Some(backend.clone() as Arc<dyn PlayerBackend>);
                let load_id = {
                    let mut playback = app_state.playback.state.lock();
                    playback.playlist_items = vec!["broken.mkv".to_string()];
                    let load_id = start_server_load(&mut playback, 0, true);
                    playback.reduce(PlaybackEvent::LoadStarted { load_id });
                    load_id
                };
                backend.begin_file_load(load_id.0, "broken.mkv");
                backend.ipc.record_load_for_test(load_id.0);

                let (tx, rx) = mpsc::unbounded_channel();
                backend.spawn_event_loop(rx);
                tx.send(MpvPlayerEvent::GenerationLoadStarted {
                    load_id: Some(load_id.0),
                    target: Some("broken.mkv".to_string()),
                })
                .unwrap();
                tx.send(MpvPlayerEvent::GenerationLoadEnded {
                    load_id: Some(load_id.0),
                    reason,
                    propagated: false,
                })
                .unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;

                assert!(app_state.playback.snapshot().pending_load.is_none());
                assert!(!backend.media_updates.lock().state.is_loading());
            }
        });
    }

    #[test]
    fn legacy_redirect_retires_attribution_without_failing_the_load() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let backend = Arc::new(MpvBackend::new(
                PlayerKind::Mpv,
                MpvIpc::new("unused"),
                Arc::downgrade(&app_state),
                Some(LoadfileOptionsSyntax::Legacy),
                true,
                None,
            ));
            let load_id = 7;
            backend.begin_file_load(load_id, "playlist.m3u");
            assert!(backend.ipc.start_load(load_id));

            let context = backend.line_context();
            handle_end_file(&context, &EndFileReason::Redirect, load_id, false).await;

            assert!(backend.media_updates.lock().state.is_loading());
            assert!(!backend.ipc.has_tracked_load_for_test(load_id));
            assert!(!backend.ipc.is_load_active(load_id));
        });
    }

    #[test]
    fn structured_start_retargets_only_the_matching_active_generation() {
        let app_state = AppState::new();
        let ipc = Arc::new(MpvIpc::new("unused"));
        let file_loaded = Arc::new(AtomicBool::new(false));
        let media_updates = Arc::new(Mutex::new(MpvMediaUpdates::default()));
        let reset_ignore_until = Arc::new(Mutex::new(None));
        let context = test_line_context(
            &app_state,
            &ipc,
            &file_loaded,
            &media_updates,
            &reset_ignore_until,
        );

        media_updates
            .lock()
            .state
            .begin_load(7, "/media/playlist.m3u");
        ipc.prepare_load(7);
        handle_generation_start(&context, 7, Some("/media/redirect-child.mkv".to_string()));
        assert_eq!(
            media_updates
                .lock()
                .state
                .latest_load()
                .map(|load| load.target.as_str()),
            Some("/media/redirect-child.mkv")
        );

        media_updates
            .lock()
            .state
            .begin_load(8, "/media/current.mkv");
        ipc.prepare_load(9);
        handle_generation_start(&context, 9, Some("/media/stale-child.mkv".to_string()));
        assert_eq!(
            media_updates
                .lock()
                .state
                .latest_load()
                .map(|load| load.target.as_str()),
            Some("/media/current.mkv")
        );
    }

    #[test]
    fn native_mpv_eof_waits_for_the_lua_control_message() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let backend = Arc::new(MpvBackend::new(
                PlayerKind::Mpv,
                MpvIpc::new("unused"),
                Arc::downgrade(&app_state),
                None,
                true,
                None,
            ));
            *app_state.player.lock() = Some(backend.clone());

            let (tx, rx) = mpsc::unbounded_channel();
            backend.spawn_event_loop(rx);
            tx.send(MpvPlayerEvent::EndFile {
                reason: EndFileReason::Eof,
                playlist_entry_id: None,
            })
            .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;

            assert!(app_state.player.lock().is_some());
        });
    }
}
