use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStdout;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, warn};

use super::backend::{PlayerBackend, PlayerKind};
use super::commands::MpvCommand;
use super::events::{EndFileReason, MpvPlayerEvent};
use super::mpv_ipc::MpvIpc;
use super::properties::PlayerState;
use crate::app_state::AppState;
use crate::commands::chat::send_chat_message_from_player;
use crate::commands::connection::emit_error_message;
use crate::player::controller::handle_end_of_file;
use crate::player::controller::is_placeholder_file;
use crate::player::controller::send_file_update;
use crate::player::controller::stop_player;

pub struct MpvBackend {
    kind: PlayerKind,
    ipc: Arc<MpvIpc>,
    state: Weak<AppState>,
    file_loaded: Arc<AtomicBool>,
    last_loaded: Arc<Mutex<Option<Instant>>>,
    reset_ignore_until: Arc<Mutex<Option<Instant>>>,
    osc_visibility_change_compatible: bool,
}

const MPV_NEWFILE_IGNORE_TIME: Duration = Duration::from_secs(1);
const STREAM_ADDITIONAL_IGNORE_TIME: Duration = Duration::from_secs(10);
const PLAYER_ASK_DELAY: Duration = Duration::from_millis(100);
const MPV_UNRESPONSIVE_THRESHOLD: Duration = Duration::from_secs(60);
const MPV_SCRIPT_MESSAGE_TIMEOUT: Duration = Duration::from_millis(250);
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
        osc_visibility_change_compatible: bool,
        stdout: Option<ChildStdout>,
    ) -> Self {
        let backend = Self {
            kind,
            ipc: Arc::new(ipc),
            state,
            file_loaded: Arc::new(AtomicBool::new(false)),
            last_loaded: Arc::new(Mutex::new(None)),
            reset_ignore_until: Arc::new(Mutex::new(None)),
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
        let ipc = self.ipc.clone();
        let state = self.state.clone();
        let file_loaded = self.file_loaded.clone();
        let last_loaded = self.last_loaded.clone();
        let reset_ignore_until = self.reset_ignore_until.clone();
        let osc_visibility_change_compatible = self.osc_visibility_change_compatible;
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    MpvPlayerEvent::EndFile { reason } => {
                        if matches!(reason, EndFileReason::Eof) {
                            if let Some(app_state) = state.upgrade() {
                                handle_end_of_file(&app_state).await;
                            }
                        }
                        if matches!(reason, EndFileReason::Quit) {
                            stop_player_from_weak(&state).await;
                            break;
                        }
                    }
                    MpvPlayerEvent::Quit | MpvPlayerEvent::SocketDisconnected => {
                        stop_player_from_weak(&state).await;
                        break;
                    }
                    MpvPlayerEvent::LogMessage(line) => {
                        handle_syncplayintf_line(
                            &ipc,
                            &state,
                            &file_loaded,
                            &last_loaded,
                            &reset_ignore_until,
                            osc_visibility_change_compatible,
                            &line,
                        )
                        .await;
                    }
                    _ => {}
                }
            }
        });
    }

    fn spawn_stdout_reader(&self, stdout: ChildStdout) {
        let ipc = self.ipc.clone();
        let state = self.state.clone();
        let file_loaded = self.file_loaded.clone();
        let last_loaded = self.last_loaded.clone();
        let reset_ignore_until = self.reset_ignore_until.clone();
        let osc_visibility_change_compatible = self.osc_visibility_change_compatible;
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                handle_syncplayintf_line(
                    &ipc,
                    &state,
                    &file_loaded,
                    &last_loaded,
                    &reset_ignore_until,
                    osc_visibility_change_compatible,
                    &line,
                )
                .await;
            }
            stop_player_from_weak(&state).await;
        });
    }

    fn sync_file_loaded_state(&self) {
        let Some(app_state) = self.state.upgrade() else {
            return;
        };
        let state = self.ipc.get_state();
        let has_file = state
            .filename
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
            || state
                .path
                .as_deref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);
        let is_placeholder = is_placeholder_file(&app_state, &state);
        let next_loaded = has_file && !is_placeholder;
        let was_loaded = self.file_loaded.load(Ordering::SeqCst);

        if next_loaded == was_loaded {
            return;
        }

        self.file_loaded.store(next_loaded, Ordering::SeqCst);
        if next_loaded {
            *self.last_loaded.lock() = Some(Instant::now());
            self.ipc.set_ready(true);
        } else {
            *self.last_loaded.lock() = None;
            self.ipc.set_ready(false);
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
                let global = app_state.client_state.get_global_state();
                state.position = Some(0.0);
                state.paused = Some(global.paused);
                return state;
            }
            if !is_loaded {
                let global = app_state.client_state.get_global_state();
                state.position = Some(global.position);
                state.paused = Some(global.paused);
                return state;
            }
        }

        if let Some(last_update) = self.ipc.last_position_update() {
            let paused = state.paused.unwrap_or(true);
            if !paused {
                let diff = last_update.elapsed();
                if diff > PLAYER_ASK_DELAY {
                    if let Some(position) = state.position {
                        state.position = Some(position + diff.as_secs_f64());
                    }
                }
                if diff > MPV_UNRESPONSIVE_THRESHOLD {
                    if let Some(app_state) = self.state.upgrade() {
                        let message = format!(
                            "mpv has not responded for {} seconds so appears to have malfunctioned. Please restart Syncplay.",
                            diff.as_secs()
                        );
                        emit_error_message(&app_state, &message);
                        let app_state_clone = app_state.clone();
                        tokio::spawn(async move {
                            let _ = stop_player(&app_state_clone).await;
                        });
                    }
                }
            }
        }

        state
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        let cmd =
            MpvCommand::script_message_to("syncplayintf", "get_paused_and_position", Vec::new());
        let _ = timeout(MPV_SCRIPT_MESSAGE_TIMEOUT, self.ipc.send_command_async(cmd)).await;
        match timeout(Duration::from_millis(1200), self.ipc.refresh_state()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!("Failed to refresh mpv properties: {}", err),
            Err(err) => warn!("Timed out refreshing mpv properties: {}", err),
        }
        self.sync_file_loaded_state();
        Ok(())
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

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        self.ipc.load_file(path).await
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

async fn stop_player_from_weak(state: &Weak<AppState>) {
    if let Some(app_state) = state.upgrade() {
        let app_state_clone = app_state.clone();
        tokio::spawn(async move {
            let _ = stop_player(&app_state_clone).await;
        });
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
    ipc: &Arc<MpvIpc>,
    state: &Weak<AppState>,
    file_loaded: &Arc<AtomicBool>,
    last_loaded: &Arc<Mutex<Option<Instant>>>,
    reset_ignore_until: &Arc<Mutex<Option<Instant>>>,
    osc_visibility_change_compatible: bool,
    line: &str,
) {
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
    debug!("mpv >> {}", line);
    if MPV_ERROR_MESSAGES_TO_REPEAT
        .iter()
        .any(|message| line.contains(message))
    {
        if let Some(state) = state.upgrade() {
            emit_error_message(&state, line);
        }
    }
    if line.contains("Failed to get value of property") || line.contains("=(unavailable)") {
        let ipc = ipc.clone();
        tokio::spawn(async move {
            if let Err(err) = ipc.refresh_state().await {
                warn!("Failed to refresh mpv properties: {}", err);
            }
        });
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
            handle_end_of_file(&state).await;
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
        file_loaded.store(false, Ordering::SeqCst);
        *last_loaded.lock() = None;
        ipc.set_ready(false);
        return;
    }
    if line.contains("</SyncplayUpdateFile>") {
        file_loaded.store(true, Ordering::SeqCst);
        *last_loaded.lock() = Some(Instant::now());
        ipc.set_ready(true);
        if let Some(app_state) = state.upgrade() {
            let ipc = ipc.clone();
            let reset_ignore_until = Arc::clone(reset_ignore_until);
            tokio::spawn(async move {
                let ignore_until = *reset_ignore_until.lock();
                if ignore_until
                    .as_ref()
                    .map(|until| Instant::now() < *until)
                    .unwrap_or(false)
                {
                    return;
                }
                let global = app_state.client_state.get_global_state();
                let _ = ipc.set_position(global.position).await;
                let _ = ipc.set_paused(global.paused).await;
                for delay in [50_u64, 150, 300] {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    let before_refresh = ipc.get_state();
                    let refresh_result = ipc.refresh_state().await;
                    let mut stable = ipc.get_state();
                    if refresh_result.is_err() {
                        stable = before_refresh;
                    } else if let Some(duration) = before_refresh.duration {
                        if stable.duration == Some(0.0) {
                            stable.duration = Some(duration);
                            ipc.update_from_term_playing_message("duration", &duration.to_string());
                        }
                    }
                    if stable.filename.is_some() || stable.path.is_some() {
                        app_state.client_state.set_file_duration(stable.duration);
                        send_file_update(&app_state, &stable);
                        break;
                    }
                }
            });
        }
        return;
    }
    if line.starts_with("ANS_") {
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim_start_matches("ANS_").to_ascii_lowercase();
            let value = value.trim();
            if value.is_empty() {
                let ipc = ipc.clone();
                tokio::spawn(async move {
                    if let Err(err) = ipc.refresh_state().await {
                        warn!("Failed to refresh mpv properties: {}", err);
                    }
                });
                return;
            }
            ipc.update_from_term_playing_message(&key, value);
        }
        return;
    }
    if line.contains("<paused=") && line.contains(", pos=") {
        if let Some((paused, position)) = parse_pause_position(line) {
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
            tokio::spawn(async move {
                let _ = stop_player(&app_state_clone).await;
            });
        }
        ipc.set_ready(true);
    }
    if line.contains("Failed")
        || line.contains("failed")
        || line.contains("No video or audio streams selected")
        || line.contains("error")
    {
        ipc.set_ready(true);
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
    use crate::config::PrivacyMode;
    use crate::network::connection::Connection;
    use crate::network::fake_server::FakeSyncplayServer;
    use crate::network::messages::{FileSizeInfo, ProtocolMessage};
    use tokio::time::timeout;

    async fn assert_mpv_lifecycle_event_clears_player_state(event: MpvPlayerEvent) {
        let app_state = AppState::new();
        *app_state.player_connecting.lock() = true;
        *app_state.mpv_socket_path.lock() = Some("stale-socket".to_string());

        let backend = Arc::new(MpvBackend::new(
            PlayerKind::Mpv,
            MpvIpc::new("unused"),
            Arc::downgrade(&app_state),
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
        *app_state.connection.lock() = Some(connection);
        server
    }
    async fn drive_delayed_file_update(
        app_state: &Arc<AppState>,
        ipc: &Arc<MpvIpc>,
        file_loaded: &Arc<AtomicBool>,
        last_loaded: &Arc<Mutex<Option<Instant>>>,
        reset_ignore_until: &Arc<Mutex<Option<Instant>>>,
        path: &str,
    ) {
        let weak_state = Arc::downgrade(app_state);

        handle_syncplayintf_line(
            ipc,
            &weak_state,
            file_loaded,
            last_loaded,
            reset_ignore_until,
            true,
            "<SyncplayUpdateFile>",
        )
        .await;
        assert!(!file_loaded.load(Ordering::SeqCst));

        handle_syncplayintf_line(
            ipc,
            &weak_state,
            file_loaded,
            last_loaded,
            reset_ignore_until,
            true,
            "ANS_filename=delayed-file.mkv",
        )
        .await;
        handle_syncplayintf_line(
            ipc,
            &weak_state,
            file_loaded,
            last_loaded,
            reset_ignore_until,
            true,
            "ANS_duration=123.5",
        )
        .await;
        handle_syncplayintf_line(
            ipc,
            &weak_state,
            file_loaded,
            last_loaded,
            reset_ignore_until,
            true,
            &format!("ANS_path={path}"),
        )
        .await;
        handle_syncplayintf_line(
            ipc,
            &weak_state,
            file_loaded,
            last_loaded,
            reset_ignore_until,
            true,
            "<paused=true, pos=0>",
        )
        .await;
        handle_syncplayintf_line(
            ipc,
            &weak_state,
            file_loaded,
            last_loaded,
            reset_ignore_until,
            true,
            "</SyncplayUpdateFile>",
        )
        .await;
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

    #[test]
    fn syncplay_update_file_refreshes_metadata_after_delayed_term_messages() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app_state = AppState::new();
            let ipc = Arc::new(MpvIpc::new("unused"));
            let file_loaded = Arc::new(AtomicBool::new(false));
            let last_loaded = Arc::new(Mutex::new(None));
            let reset_ignore_until = Arc::new(Mutex::new(None));
            drive_delayed_file_update(
                &app_state,
                &ipc,
                &file_loaded,
                &last_loaded,
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
            let last_loaded = Arc::new(Mutex::new(None));
            let reset_ignore_until = Arc::new(Mutex::new(None));

            drive_delayed_file_update(
                &app_state,
                &ipc,
                &file_loaded,
                &last_loaded,
                &reset_ignore_until,
                "file:///tmp/delayed-file.mkv",
            )
            .await;

            let outbound = timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(ProtocolMessage::Set { Set }) = server.next_received().await {
                        if Set.file.is_some() {
                            break Set.file.unwrap();
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
                Some("delayed-file.mkv")
            );
            assert_eq!(app_state.client_state.get_file_duration(), Some(123.5));
            assert_eq!(outbound.name.as_deref(), Some("**Hidden filename**"));
            assert!(matches!(outbound.size, Some(FileSizeInfo::Number(0))));
            assert_eq!(outbound.duration, Some(123.5));
            assert_eq!(app_state.client_state.get_global_state().paused, true);
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
            })
            .await;
        });
    }
}
