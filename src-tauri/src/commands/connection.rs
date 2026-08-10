// Connection command handlers

use crate::app_state::{
    AppState, ConnectionSnapshot, ConnectionStatusEvent, ServerFeatures, WarningTimerState,
    WarningTimers,
};
use crate::client::playback_runtime;
use crate::client::sync::{
    FASTFORWARD_BEHIND_THRESHOLD, FASTFORWARD_EXTRA_TIME, FASTFORWARD_RESET_THRESHOLD,
};
use crate::config::{save_config, ServerConfig};
use crate::network::connection::{Connection, TerminalConnectionError};
use crate::network::messages::{
    ChatMessage, ClientFeatures, ControllerAuth, HelloMessage, IgnoringInfo, NewControlledRoom,
    PingInfo, PlayState, ProtocolMessage, RoomInfo, SetMessage, StateMessage, TLSMessage,
    UserUpdate,
};
use crate::network::tls::create_tls_connector;
use crate::player::backend::PlayerBackend;
use crate::player::controller::{ensure_player_connected_for_session, stop_player};
use crate::player::properties::PlayerState;
use crate::utils::{
    is_controlled_room, parse_controlled_room_input, same_filename, strip_control_password,
    version_meets_min,
};
use md5::{Digest, Md5};
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Runtime, State};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, Duration};

const AUTOPLAY_DELAY_SECONDS: i32 = 3;
const DIFFERENT_DURATION_THRESHOLD: f64 = 2.5;
const WARNING_OSD_INTERVAL_SECONDS: u64 = 1;
const OSD_WARNING_MESSAGE_DURATION_SECONDS: u32 = 5;
const OSD_MESSAGE_SEPARATOR: &str = "; ";
const TLS_NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(8);
const PROTOCOL_TIMEOUT_SECONDS: f64 = 12.5;
const PROTOCOL_TIMEOUT_CHECK_INTERVAL: Duration = Duration::from_millis(500);
const LAST_PAUSED_DIFF_THRESHOLD_SECONDS: f64 = 2.0;
const RECONNECT_RETRIES: u32 = 999;
const RECONNECT_BASE_DELAY_SECONDS: f64 = 0.1;
const RECONNECT_MAX_EXPONENT: u32 = 5;
const PLAYER_STARTUP_DELAY: Duration = Duration::from_millis(100);
const CONTROLLED_ROOMS_MIN_VERSION: &str = "1.3.0";
const USER_READY_MIN_VERSION: &str = "1.3.0";
const SHARED_PLAYLIST_MIN_VERSION: &str = "1.4.0";
const CHAT_MIN_VERSION: &str = "1.5.0";
const FEATURE_LIST_MIN_VERSION: &str = "1.5.0";
const SET_OTHERS_READINESS_MIN_VERSION: &str = "1.7.2";
const FALLBACK_MAX_CHAT_MESSAGE_LENGTH: usize = 50;
const FALLBACK_MAX_USERNAME_LENGTH: usize = 16;
const FALLBACK_MAX_ROOM_NAME_LENGTH: usize = 35;
const FALLBACK_MAX_FILENAME_LENGTH: usize = 250;
const IGNORE_SEEK_AFTER_REWIND_SECONDS: f64 = 1.0;
const IGNORE_SEEK_AFTER_REWIND_POSITION_THRESHOLD: f64 = 5.0;

fn update_server_features(
    state: &Arc<AppState>,
    server_version: &str,
    feature_list: Option<Value>,
) {
    let mut features = ServerFeatures {
        feature_list: version_meets_min(server_version, FEATURE_LIST_MIN_VERSION),
        shared_playlists: version_meets_min(server_version, SHARED_PLAYLIST_MIN_VERSION),
        chat: version_meets_min(server_version, CHAT_MIN_VERSION),
        readiness: version_meets_min(server_version, USER_READY_MIN_VERSION),
        managed_rooms: version_meets_min(server_version, CONTROLLED_ROOMS_MIN_VERSION),
        persistent_rooms: false,
        set_others_readiness: version_meets_min(server_version, SET_OTHERS_READINESS_MIN_VERSION),
        max_chat_message_length: Some(FALLBACK_MAX_CHAT_MESSAGE_LENGTH),
        max_username_length: Some(FALLBACK_MAX_USERNAME_LENGTH),
        max_room_name_length: Some(FALLBACK_MAX_ROOM_NAME_LENGTH),
        max_filename_length: Some(FALLBACK_MAX_FILENAME_LENGTH),
    };

    if let Some(Value::Object(map)) = feature_list {
        if let Some(value) = map.get("featureList").and_then(|v| v.as_bool()) {
            features.feature_list = value;
        }
        if let Some(value) = map.get("sharedPlaylists").and_then(|v| v.as_bool()) {
            features.shared_playlists = value;
        }
        if let Some(value) = map.get("chat").and_then(|v| v.as_bool()) {
            features.chat = value;
        }
        if let Some(value) = map.get("readiness").and_then(|v| v.as_bool()) {
            features.readiness = value;
        } else if let Some(value) = map.get("readyState").and_then(|v| v.as_bool()) {
            features.readiness = value;
        }
        if let Some(value) = map.get("managedRooms").and_then(|v| v.as_bool()) {
            features.managed_rooms = value;
        }
        if let Some(value) = map.get("persistentRooms").and_then(|v| v.as_bool()) {
            features.persistent_rooms = value;
        }
        if let Some(value) = map.get("setOthersReadiness").and_then(|v| v.as_bool()) {
            features.set_others_readiness = value;
        }
        if let Some(value) = map.get("maxChatMessageLength").and_then(|v| v.as_u64()) {
            features.max_chat_message_length = Some(value as usize);
        }
        if let Some(value) = map.get("maxUsernameLength").and_then(|v| v.as_u64()) {
            features.max_username_length = Some(value as usize);
        }
        if let Some(value) = map.get("maxRoomNameLength").and_then(|v| v.as_u64()) {
            features.max_room_name_length = Some(value as usize);
        }
        if let Some(value) = map.get("maxFilenameLength").and_then(|v| v.as_u64()) {
            features.max_filename_length = Some(value as usize);
        }
    }

    *state.server_features.lock() = features.clone();
    state.emit_event(
        "server-features-updated",
        serde_json::json!({
            "managedRooms": features.managed_rooms,
            "persistentRooms": features.persistent_rooms,
        }),
    );

    let config = state.config.lock().clone();
    if config.user.shared_playlist_enabled {
        if !version_meets_min(server_version, SHARED_PLAYLIST_MIN_VERSION) {
            emit_error_message(
                state,
                &format!(
                    "Shared playlists require server version {} or later",
                    SHARED_PLAYLIST_MIN_VERSION
                ),
            );
        } else if !features.shared_playlists {
            emit_error_message(state, "Shared playlists are disabled by the server");
        }
    }
}

fn server_password_digest(password: Option<&str>) -> Option<String> {
    password
        .filter(|password| !password.is_empty())
        .map(|password| format!("{:x}", Md5::digest(password.as_bytes())))
}

async fn establish_connection(
    state: &Arc<AppState>,
    snapshot: &ConnectionSnapshot,
    emit_reachout: bool,
    connection: Arc<Connection>,
) -> Result<(Arc<Connection>, mpsc::UnboundedReceiver<ProtocolMessage>), String> {
    tracing::info!(
        host = %snapshot.host,
        port = snapshot.port,
        username = %snapshot.username,
        room = %snapshot.room,
        reconnecting = state.reconnect_state.lock().running,
        "connection_lifecycle: opening transport"
    );
    let (receiver, peer_address) = connection
        .connect(snapshot.host.clone(), snapshot.port)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    if !is_current_connection_session(state, &connection) {
        connection.disconnect();
        return Err("Connection session was superseded".to_string());
    }

    tracing::info!("Successfully connected to server");

    let config = state.config.lock().clone();
    let client_features = ClientFeatures {
        shared_playlists: Some(config.user.shared_playlist_enabled),
        chat: Some(true),
        readiness: Some(true),
        managed_rooms: Some(true),
        persistent_rooms: Some(true),
        feature_list: Some(true),
        set_others_readiness: Some(true),
        ui_mode: Some("GUI".to_string()),
    };
    let features_value = serde_json::to_value(client_features).ok();

    let hello_payload = HelloMessage {
        username: snapshot.username.clone(),
        password: server_password_digest(snapshot.password.as_deref()),
        room: Some(RoomInfo {
            name: snapshot.room.clone(),
            password: None,
        }),
        version: "1.2.255".to_string(),
        realversion: "1.7.5".to_string(),
        features: features_value,
        motd: None,
    };

    *state.last_hello.lock() = Some(hello_payload);
    *state.hello_sent.lock() = false;

    let client_supports_tls = create_tls_connector().is_ok();
    #[cfg(test)]
    let client_supports_tls = client_supports_tls && *state.client_supports_tls.lock();
    *state.client_supports_tls.lock() = client_supports_tls;
    let server_supports_tls = *state.server_supports_tls.lock();

    if emit_reachout {
        if let Some(peer_address) = peer_address {
            emit_system_message(
                state,
                &format!("Successfully reached {} ({})", snapshot.host, peer_address),
            );
        } else {
            emit_system_message(state, &format!("Successfully reached {}", snapshot.host));
        }
    }

    if client_supports_tls && server_supports_tls {
        let tls_request = ProtocolMessage::TLS {
            TLS: TLSMessage {
                start_tls: Some("send".to_string()),
            },
        };
        if let Err(e) = connection.send(tls_request) {
            tracing::error!("Failed to send TLS request: {}", e);
            state.emit_event(
                "tls-status-changed",
                serde_json::json!({ "status": "unsupported" }),
            );
            send_hello(state);
        } else {
            tracing::info!("Sent TLS request");
            state.emit_event(
                "tls-status-changed",
                serde_json::json!({ "status": "pending" }),
            );
        }
    } else {
        if !client_supports_tls {
            emit_system_message(state, "This client does not support TLS");
        } else if !server_supports_tls {
            emit_error_message(state, "This server does not support TLS");
        }
        state.emit_event(
            "tls-status-changed",
            serde_json::json!({ "status": "unsupported" }),
        );
        send_hello(state);
    }

    Ok((connection, receiver))
}

fn claim_connection_session(state: &Arc<AppState>, connection: Arc<Connection>) -> bool {
    let mut guard = state.connection.lock();
    if guard.is_some() {
        return false;
    }
    *guard = Some(connection);
    true
}

fn is_current_connection_session(state: &Arc<AppState>, expected: &Arc<Connection>) -> bool {
    state
        .connection
        .lock()
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, expected))
}

fn take_current_connection_session(
    state: &Arc<AppState>,
    expected: &Arc<Connection>,
) -> Option<Arc<Connection>> {
    let mut guard = state.connection.lock();
    match guard.as_ref() {
        Some(current) if Arc::ptr_eq(current, expected) => guard.take(),
        _ => None,
    }
}

fn begin_connection_session(state: &Arc<AppState>) -> u64 {
    state
        .connection_session_generation
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1)
}

fn current_connection_session(state: &Arc<AppState>) -> u64 {
    state.connection_session_generation.load(Ordering::Acquire)
}

fn is_current_connection_generation(state: &Arc<AppState>, expected: u64) -> bool {
    current_connection_session(state) == expected
}

fn invalidate_connection_session(state: &Arc<AppState>, expected: u64) -> bool {
    state
        .connection_session_generation
        .compare_exchange(
            expected,
            expected.wrapping_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

fn invalidate_current_connection_session(state: &Arc<AppState>) {
    state
        .connection_session_generation
        .fetch_add(1, Ordering::AcqRel);
}

fn start_connection_session(
    state: &Arc<AppState>,
    snapshot: &ConnectionSnapshot,
    session_connection: Arc<Connection>,
    session_generation: u64,
    mut receiver: mpsc::UnboundedReceiver<ProtocolMessage>,
) {
    let config = state.config.lock().clone();
    state.client_state.set_username(snapshot.username.clone());
    set_authoritative_room(state, snapshot.room.clone());
    let was_reconnecting = {
        let mut reconnect = state.reconnect_state.lock();
        let was_reconnecting = reconnect.running;
        if was_reconnecting {
            reconnect.running = false;
            reconnect.attempts = 0;
        }
        was_reconnecting
    };
    if !was_reconnecting {
        *state.playlist_may_need_restoring.lock() = false;
    }
    *state.last_advance_time.lock() = None;
    *state.last_rewind_time.lock() = None;
    *state.last_updated_file_time.lock() = None;
    *state.last_paused_on_leave_time.lock() = None;
    *state.last_global_update.lock() = None;
    *state.ping_service.lock() = crate::network::ping::PingService::default();
    *state.ignoring_on_the_fly.lock() = crate::app_state::IgnoringOnTheFlyState::default();
    state.client_state.set_global_state(0.0, true, None);
    *state.last_protocol_activity.lock() = Some(std::time::Instant::now());
    state.sync_engine.lock().update_from_config(&config.user);
    update_autoplay_state(state, &config);

    let state_clone = state.clone();
    let timeout_session_connection = session_connection.clone();
    tokio::spawn(async move {
        let mut ticker = interval(PROTOCOL_TIMEOUT_CHECK_INTERVAL);
        loop {
            ticker.tick().await;
            let same_session =
                match (
                    Some(&timeout_session_connection),
                    state_clone.connection.lock().as_ref(),
                ) {
                    (Some(expected), Some(current)) => Arc::ptr_eq(expected, current),
                    _ => false,
                } && is_current_connection_generation(&state_clone, session_generation);
            if !same_session {
                break;
            }
            if check_protocol_timeout(&state_clone) {
                break;
            }
        }
    });

    let state_clone = state.clone();
    tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            let is_current_session = match (
                Some(&session_connection),
                state_clone.connection.lock().as_ref(),
            ) {
                (Some(expected), Some(current)) => Arc::ptr_eq(expected, current),
                _ => false,
            };
            if !is_current_session {
                tracing::debug!("Ignoring message from superseded connection session");
                break;
            }
            tracing::debug!("Received message: {:?}", message);
            handle_server_message(message, &state_clone).await;
            if session_connection.has_terminal_error() {
                break;
            }
        }
        tracing::info!("Message processing loop ended");
        handle_connection_closed_for_session(
            &state_clone,
            Some(session_connection),
            Some(session_generation),
        )
        .await;
    });
}

pub(crate) fn set_authoritative_room(state: &Arc<AppState>, room: String) {
    state.client_state.set_room(room.clone());
    if let Some(snapshot) = state.reconnect_snapshot.lock().as_mut() {
        snapshot.room = room;
    }
}

pub(crate) fn check_protocol_timeout(state: &Arc<AppState>) -> bool {
    let timed_out = {
        let guard = state.last_global_update.lock();
        let Some(last_global_update) = guard.as_ref() else {
            return false;
        };
        last_global_update.elapsed().as_secs_f64() > PROTOCOL_TIMEOUT_SECONDS
    };
    if !timed_out {
        return false;
    }
    tracing::warn!(
        timeout_seconds = PROTOCOL_TIMEOUT_SECONDS,
        "protocol_timeout: no global State within timeout; disconnecting"
    );
    *state.last_global_update.lock() = None;
    emit_error_message(state, "Server timed out");
    let timed_out_connection = state.connection.lock().clone();
    let timed_out_generation = current_connection_session(state);
    if let Some(connection) = timed_out_connection.as_ref() {
        connection.disconnect();
    }
    let state_clone = state.clone();
    tokio::spawn(async move {
        handle_connection_closed_for_session(
            &state_clone,
            timed_out_connection,
            Some(timed_out_generation),
        )
        .await;
    });
    true
}

async fn complete_server_login(state: &Arc<AppState>, hello: HelloMessage) {
    let connection = state.connection.lock().clone();
    if let Some(connection) = connection.as_ref() {
        connection.set_authenticated();
    }
    if state.reconnect_state.lock().running {
        reset_reconnect_state(state);
    }

    let server_version = if hello.realversion.is_empty() {
        hello.version.clone()
    } else {
        hello.realversion.clone()
    };
    let feature_list = hello.features.clone();

    tracing::info!(
        username = %hello.username,
        room = hello.room.as_ref().map(|room| room.name.as_str()).unwrap_or(""),
        server_version = %server_version,
        "connection_lifecycle: server login completed"
    );

    state.client_state.set_username(hello.username.clone());
    if let Some(room) = hello.room.as_ref() {
        set_authoritative_room(state, room.name.clone());
    }
    *state.last_protocol_activity.lock() = Some(std::time::Instant::now());

    if let Some(motd) = hello.motd {
        state.emit_event(
            "chat-message-received",
            serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "username": null,
                "message": motd,
                "messageType": "system",
            }),
        );
    }
    emit_system_message(state, "Successfully connected to server");

    let config = state.config.lock().clone();
    let ready_at_login = state
        .client_state
        .ready_state()
        .unwrap_or(config.user.ready_at_start);
    if let Err(e) = send_login_ready_state(state, ready_at_login) {
        tracing::warn!("Failed to send ready-at-start: {}", e);
    }
    reidentify_as_controller(state);

    if let Some(media) = playback_runtime::confirmed_media(state) {
        if let Err(error) = crate::player::controller::send_committed_file_update(state, &media) {
            tracing::warn!("Failed to restore confirmed file after login: {}", error);
        }
    }

    state
        .client_state
        .set_server_version(server_version.clone());
    update_server_features(state, &server_version, feature_list);
    if let Some(connection) = connection {
        schedule_player_feature_sync(state.clone(), connection);
    }
    start_room_warning_loop(state.clone());

    state.emit_event(
        "connection-status-changed",
        ConnectionStatusEvent {
            connected: true,
            server: current_server_label(state),
        },
    );
}

fn schedule_player_feature_sync(state: Arc<AppState>, expected_connection: Arc<Connection>) {
    let lifecycle = state.player_lifecycle.clone();
    match lifecycle.clone().try_lock_owned() {
        Ok(guard) => {
            sync_player_features_for_session(&state, &expected_connection);
            drop(guard);
        }
        Err(_) => {
            tokio::spawn(async move {
                let _guard = lifecycle.lock_owned().await;
                sync_player_features_for_session(&state, &expected_connection);
            });
        }
    }
}

fn sync_player_features_for_session(state: &Arc<AppState>, expected_connection: &Arc<Connection>) {
    if !is_current_connection_session(state, expected_connection)
        || expected_connection.state() != crate::network::connection::ConnectionState::Authenticated
    {
        return;
    }
    if let Some(player) = state.player.lock().clone() {
        if let Err(error) = player.set_features() {
            tracing::warn!("Failed to send feature update to player: {}", error);
        }
    }
}

fn sync_player_features_for_generation(state: &Arc<AppState>, expected_generation: u64) {
    if !is_current_connection_generation(state, expected_generation) {
        return;
    }
    let authenticated = state.connection.lock().as_ref().is_some_and(|connection| {
        connection.state() == crate::network::connection::ConnectionState::Authenticated
    });
    if !authenticated {
        return;
    }
    if let Some(player) = state.player.lock().clone() {
        if let Err(error) = player.set_features() {
            tracing::warn!("Failed to send feature update to player: {error}");
        }
    }
}

fn current_server_label(state: &Arc<AppState>) -> Option<String> {
    state
        .reconnect_snapshot
        .lock()
        .as_ref()
        .map(|snapshot| format!("{}:{}", snapshot.host, snapshot.port))
}

fn reset_reconnect_state(state: &Arc<AppState>) {
    let mut reconnect = state.reconnect_state.lock();
    reconnect.running = false;
    reconnect.attempts = 0;
}

fn disable_reconnect(state: &Arc<AppState>) {
    let mut reconnect = state.reconnect_state.lock();
    reconnect.enabled = false;
    reconnect.running = false;
    reconnect.attempts = 0;
}

fn reconnect_delay(attempt: u32) -> Duration {
    let exponent = attempt.min(RECONNECT_MAX_EXPONENT);
    let delay = RECONNECT_BASE_DELAY_SECONDS * 2_f64.powi(exponent as i32);
    Duration::from_secs_f64(delay)
}

fn start_reconnect_loop(state: Arc<AppState>, session_generation: u64) {
    if !is_current_connection_generation(&state, session_generation) {
        return;
    }
    if state.connection.lock().is_some() {
        tracing::debug!("reconnect_lifecycle: a transport is already active; reconnect skipped");
        return;
    }
    let snapshot = state.reconnect_snapshot.lock().clone();
    if snapshot.is_none() {
        tracing::debug!("reconnect_lifecycle: no snapshot available; reconnect skipped");
        return;
    }
    {
        let mut reconnect = state.reconnect_state.lock();
        if reconnect.running || !reconnect.enabled {
            tracing::debug!(
                running = reconnect.running,
                enabled = reconnect.enabled,
                "reconnect_lifecycle: reconnect loop already running or disabled"
            );
            return;
        }
        reconnect.running = true;
    }
    tracing::info!("reconnect_lifecycle: reconnect loop started");

    tokio::spawn(async move {
        loop {
            if !is_current_connection_generation(&state, session_generation) {
                reset_reconnect_state(&state);
                break;
            }
            let snapshot = match state.reconnect_snapshot.lock().clone() {
                Some(snapshot) => snapshot,
                None => {
                    reset_reconnect_state(&state);
                    break;
                }
            };

            let attempt = {
                let mut reconnect = state.reconnect_state.lock();
                reconnect.attempts = reconnect.attempts.saturating_add(1);
                reconnect.attempts
            };

            tracing::info!(
                attempt,
                host = %snapshot.host,
                port = snapshot.port,
                room = %snapshot.room,
                "reconnect_lifecycle: attempting reconnect"
            );

            if attempt == 1 {
                reset_transient_connection_state(&state).await;
                *state.playlist_may_need_restoring.lock() = true;
                state.emit_event(
                    "tls-status-changed",
                    serde_json::json!({ "status": "unknown" }),
                );
                emit_system_message(
                    &state,
                    "Connection with server lost, attempting to reconnect",
                );
                let config = state.config.lock().clone();
                if config.user.pause_on_leave {
                    pause_local_player(&state).await;
                }
            }

            if attempt > RECONNECT_RETRIES {
                finish_reconnect_exhaustion(&state, session_generation).await;
                break;
            }

            sleep(reconnect_delay(attempt.saturating_sub(1))).await;

            if !state.reconnect_state.lock().enabled
                || !is_current_connection_generation(&state, session_generation)
            {
                reset_reconnect_state(&state);
                break;
            }

            let connection = Arc::new(Connection::new());
            if !claim_connection_session(&state, connection.clone()) {
                tracing::debug!(
                    attempt,
                    "reconnect_lifecycle: another connection session became current"
                );
                reset_reconnect_state(&state);
                break;
            }

            match establish_connection(&state, &snapshot, false, connection.clone()).await {
                Ok((_connection, receiver)) => {
                    tracing::info!(attempt, "reconnect_lifecycle: transport re-established");
                    start_connection_session(
                        &state,
                        &snapshot,
                        connection,
                        session_generation,
                        receiver,
                    );
                    break;
                }
                Err(err) => {
                    tracing::warn!("Reconnect attempt failed: {}", err);
                    if let Some(connection) = take_current_connection_session(&state, &connection) {
                        connection.disconnect();
                    }
                    continue;
                }
            }
        }
    });
}

async fn finish_reconnect_exhaustion(state: &Arc<AppState>, session_generation: u64) {
    if !invalidate_connection_session(state, session_generation) {
        return;
    }
    emit_error_message(state, "Connection with server failed");
    disable_reconnect(state);
    if let Err(error) = clear_disconnected_session_state(state, "closed").await {
        tracing::warn!("Failed to stop session after reconnect exhaustion: {error}");
    }
}

#[tauri::command]
pub async fn connect_to_server<R: Runtime>(
    host: String,
    port: u16,
    username: String,
    room: String,
    password: Option<String>,
    app: AppHandle<R>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    connect_to_server_state(
        host,
        port,
        username,
        room,
        password,
        Some(&app),
        state.inner(),
    )
    .await
}

fn mark_protocol_activity(state: &Arc<AppState>) {
    *state.last_protocol_activity.lock() = Some(std::time::Instant::now());
}

fn route_incoming_chat(state: &Arc<AppState>, chat: ChatMessage) -> Option<Value> {
    if !state.server_features.lock().chat {
        return None;
    }

    let (username, message) = match chat {
        ChatMessage::Entry { username, message } => (Some(username), message),
        ChatMessage::Text(message) => (None, message),
    };
    if state.config.lock().user.chat_output_enabled {
        if let Some(player) = state.player.lock().clone() {
            let _ = player.show_chat_message(username.as_deref(), &message);
        }
    }

    Some(serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "username": username,
        "message": message,
        "messageType": "normal",
    }))
}

async fn connect_to_server_state<R: Runtime>(
    host: String,
    port: u16,
    username: String,
    room: String,
    password: Option<String>,
    app: Option<&AppHandle<R>>,
    state: &Arc<AppState>,
) -> Result<(), String> {
    tracing::info!(
        "Connecting to {}:{} as {} in room {}",
        host,
        port,
        username,
        room
    );
    emit_system_message(
        state,
        &format!("Attempting to connect to {}:{}", host, port),
    );

    // A connecting session also owns startup and must not be superseded by a
    // second explicit request.
    if state.connection.lock().is_some() {
        return Err("Already connected to a server".to_string());
    }

    let (room, control_password) = parse_controlled_room_input(&room);
    if let Some(password) = control_password {
        store_control_password(state, &room, &password, true);
    }
    let snapshot = ConnectionSnapshot {
        host: host.clone(),
        port,
        username: username.clone(),
        room: room.clone(),
        password: password.clone(),
    };

    {
        let mut reconnect = state.reconnect_state.lock();
        reconnect.enabled = true;
        reconnect.running = false;
        reconnect.attempts = 0;
    }
    *state.manual_disconnect.lock() = false;
    *state.server_supports_tls.lock() = true;
    *state.reconnect_snapshot.lock() = Some(snapshot.clone());

    let config = state.config.lock().clone();
    let connection = Arc::new(Connection::new());
    if !claim_connection_session(state, connection.clone()) {
        return Err("Already connected to a server".to_string());
    }
    let session_generation = begin_connection_session(state);

    let startup_state = state.clone();
    let mut player_startup = tokio::spawn(async move {
        sleep(PLAYER_STARTUP_DELAY).await;
        ensure_player_connected_for_session(&startup_state, session_generation).await
    });
    let connection_attempt = establish_connection(state, &snapshot, true, connection.clone());
    tokio::pin!(connection_attempt);

    tokio::select! {
        connection_result = &mut connection_attempt => {
            match connection_result {
                Ok((_connection, receiver)) => {
                    if let Some(app) = app {
                        maybe_autosave_connection(state, app, &config, snapshot.clone());
                    }
                    start_connection_session(
                        state,
                        &snapshot,
                        connection,
                        session_generation,
                        receiver,
                    );
                    monitor_player_startup(state.clone(), session_generation, player_startup);
                    Ok(())
                }
                Err(error) => {
                    player_startup.abort();
                    let _ = player_startup.await;
                    tracing::error!("Failed to connect: {}", error);
                    terminate_initial_session(state, session_generation, None).await;
                    Err(error)
                }
            }
        }
        startup_result = &mut player_startup => {
            match flatten_player_startup_result(startup_result) {
                Ok(()) => {
                    match connection_attempt.await {
                        Ok((_connection, receiver)) => {
                            if let Some(app) = app {
                                maybe_autosave_connection(state, app, &config, snapshot.clone());
                            }
                            start_connection_session(
                                state,
                                &snapshot,
                                connection,
                                session_generation,
                                receiver,
                            );
                            Ok(())
                        }
                        Err(error) => {
                            tracing::error!("Failed to connect: {}", error);
                            terminate_initial_session(state, session_generation, None).await;
                            Err(error)
                        }
                    }
                }
                Err(error) => {
                    tracing::error!("Failed to start player: {}", error);
                    terminate_initial_session(state, session_generation, Some(&error)).await;
                    Err(error)
                }
            }
        }
    }
}

fn flatten_player_startup_result(
    result: Result<Result<(), String>, tokio::task::JoinError>,
) -> Result<(), String> {
    match result {
        Ok(result) => result,
        Err(error) => Err(format!("Player startup task failed: {error}")),
    }
}

fn monitor_player_startup(
    state: Arc<AppState>,
    session_generation: u64,
    player_startup: tokio::task::JoinHandle<Result<(), String>>,
) {
    tokio::spawn(async move {
        match flatten_player_startup_result(player_startup.await) {
            Ok(()) => sync_player_features_for_generation(&state, session_generation),
            Err(error) => {
                if is_current_connection_generation(&state, session_generation) {
                    tracing::error!("Failed to start player: {}", error);
                    terminate_initial_session(&state, session_generation, Some(&error)).await;
                } else {
                    tracing::debug!(
                        "Ignoring player startup result from a closed connection session: {}",
                        error
                    );
                }
            }
        }
    });
}

async fn terminate_initial_session(
    state: &Arc<AppState>,
    session_generation: u64,
    error: Option<&str>,
) {
    if !invalidate_connection_session(state, session_generation) {
        tracing::debug!("Ignoring failed startup from superseded connection session");
        return;
    }

    disable_reconnect(state);
    if let Some(connection) = state.connection.lock().take() {
        connection.disconnect();
    }
    if let Err(cleanup_error) = clear_disconnected_session_state(state, "closed").await {
        tracing::warn!("Failed to clean up initial connection session: {cleanup_error}");
    }
    if let Some(error) = error {
        emit_error_message(state, error);
    }
}

async fn handle_server_message(message: ProtocolMessage, state: &Arc<AppState>) {
    match message {
        ProtocolMessage::Hello { Hello } => {
            mark_protocol_activity(state);
            tracing::info!("Received hello message: {:?}", Hello);
            if let Err(error) = validate_server_hello(&Hello) {
                terminate_protocol_session(state, error);
                return;
            }
            emit_system_message(state, &format!("Hello {},", Hello.username));
            complete_server_login(state, Hello).await;
        }
        ProtocolMessage::List { List } => {
            mark_protocol_activity(state);
            tracing::info!("Received user list: {:?}", List);
            if let Some(users_by_room) = List {
                let rooms = apply_list_response(state, users_by_room);
                emit_user_list_with_rooms(state, rooms);
                evaluate_autoplay(state);
                update_room_warnings(state, false);
            }
        }
        ProtocolMessage::Chat { Chat } => {
            mark_protocol_activity(state);
            tracing::info!("Received chat message: {:?}", Chat);
            if let Some(chat_msg) = route_incoming_chat(state, Chat) {
                state.emit_event("chat-message-received", chat_msg);
            }
        }
        ProtocolMessage::State { State: state_msg } => {
            mark_protocol_activity(state);
            if state_msg.playstate.is_some() || state_msg.ignoring_on_the_fly.is_some() {
                tracing::info!(
                    "Received state update: playstate={:?}, ignoring_on_the_fly={:?}",
                    state_msg.playstate.as_ref(),
                    state_msg.ignoring_on_the_fly.as_ref()
                );
            }
            let mut message_age = 0.0;
            if let Some(ignore) = state_msg.ignoring_on_the_fly.as_ref() {
                update_ignoring_on_the_fly(state, ignore);
            }
            let client_ignore_active = state.ignoring_on_the_fly.lock().client != 0;
            if let Some(ping) = state_msg.ping.as_ref() {
                if let (Some(client_latency), Some(server_rtt)) =
                    (ping.client_latency_calculation, ping.server_rtt)
                {
                    state
                        .ping_service
                        .lock()
                        .receive_message(client_latency, server_rtt);
                    message_age = state.ping_service.lock().get_last_forward_delay();
                    let rtt_ms = state.ping_service.lock().get_rtt() * 1000.0;
                    state.emit_event("ping-updated", serde_json::json!({ "rttMs": rtt_ms }));
                }
            }
            if let Some(playstate) = state_msg.playstate {
                if !client_ignore_active {
                    handle_state_update(state, playstate, message_age).await;
                }
            }
            let latency_calculation = state_msg
                .ping
                .as_ref()
                .and_then(|ping| ping.latency_calculation);
            if let Err(e) = send_state_message(
                state,
                build_local_playstate(state),
                latency_calculation,
                false,
            ) {
                tracing::warn!("Failed to send state response: {}", e);
            }
            if state.reconnect_state.lock().running
                && state
                    .connection
                    .lock()
                    .as_ref()
                    .map(|connection| {
                        connection.state()
                            == crate::network::connection::ConnectionState::Authenticated
                    })
                    .unwrap_or(false)
            {
                reset_reconnect_state(state);
            }
        }
        ProtocolMessage::Error { Error } => {
            mark_protocol_activity(state);
            tracing::error!("Received error from server: {:?}", Error);
            let authenticated = state
                .connection
                .lock()
                .as_ref()
                .map(|conn| {
                    conn.state() == crate::network::connection::ConnectionState::Authenticated
                })
                .unwrap_or(false);
            if Error.message.contains("startTLS") && !authenticated {
                *state.server_supports_tls.lock() = false;
                state.emit_event(
                    "tls-status-changed",
                    serde_json::json!({ "status": "unsupported" }),
                );
                send_hello(state);
            } else {
                terminate_protocol_session(state, Error.message);
            }
        }
        ProtocolMessage::Set { Set } => {
            mark_protocol_activity(state);
            tracing::info!("Received set message: {:?}", Set);
            handle_set_message(state, *Set).await;
        }
        ProtocolMessage::TLS { TLS } => {
            mark_protocol_activity(state);
            tracing::info!("Received TLS message: {:?}", TLS);
            handle_tls_message(state, TLS).await;
        }
    }
}

fn validate_server_hello(hello: &HelloMessage) -> Result<(), String> {
    let version = if hello.realversion.is_empty() {
        hello.version.as_str()
    } else {
        hello.realversion.as_str()
    };
    let room = hello.room.as_ref().map(|room| room.name.as_str());
    if hello.username.trim().is_empty()
        || room.is_none_or(|room| room.trim().is_empty())
        || version.trim().is_empty()
    {
        return Err(format!("Invalid Hello message from server: {hello:?}"));
    }
    Ok(())
}

fn terminate_protocol_session(state: &Arc<AppState>, error: impl Into<String>) {
    let error = error.into();
    if let Some(connection) = state.connection.lock().clone() {
        connection.mark_protocol_error(error);
        connection.disconnect();
    } else {
        emit_error_message(state, &error);
    }
}

fn should_ignore_seek_after_rewind(state: &Arc<AppState>, position: f64) -> bool {
    let guard = state.last_rewind_time.lock();
    let Some(last_rewind) = guard.as_ref() else {
        return false;
    };
    last_rewind.elapsed().as_secs_f64() < IGNORE_SEEK_AFTER_REWIND_SECONDS
        && position > IGNORE_SEEK_AFTER_REWIND_POSITION_THRESHOLD
}

fn with_current_player_local_state<R>(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    operation: impl FnOnce(&mut crate::client::local_state::LocalPlaybackState) -> R,
) -> Option<R> {
    let player_slot = state.player.lock();
    if !player_slot
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, player))
    {
        return None;
    }
    let mut local_state = state.local_playback_state.lock();
    Some(operation(&mut local_state))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteCorrectionStart {
    AlreadyActive,
    Started(u64),
}

async fn try_set_position(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    position: f64,
    context: &str,
) -> bool {
    if should_ignore_seek_after_rewind(state, position) {
        tracing::debug!("Ignored seek to {} after rewind ({})", position, context);
        return false;
    }
    let Some(start) = with_current_player_local_state(state, player, |local_state| {
        let snapshot = player.get_state();
        if local_state.remote_position_is_active_for(position) {
            RemoteCorrectionStart::AlreadyActive
        } else {
            RemoteCorrectionStart::Started(
                local_state
                    .begin_remote_position(position, snapshot.position_observation_generation),
            )
        }
    }) else {
        return false;
    };
    let RemoteCorrectionStart::Started(correction) = start else {
        return true;
    };
    if let Err(e) = player.set_position(position).await {
        with_current_player_local_state(state, player, |local_state| {
            local_state.cancel_remote_position(correction);
        });
        tracing::warn!("Failed to set position ({}): {}", context, e);
        return false;
    }
    let global = state.effective_global_state();
    with_current_player_local_state(state, player, |local_state| {
        let snapshot = player.get_state();
        local_state.complete_remote_position(correction);
        if let (Some(position), Some(paused)) = (snapshot.position, snapshot.paused) {
            local_state.update_from_player(
                position,
                paused,
                snapshot.observed_position,
                snapshot.observed_paused,
                snapshot.position_observation_generation,
                snapshot.paused_observation_generation,
                global.position,
                global.paused,
            );
        }
    })
    .is_some()
}

async fn try_set_paused(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    paused: bool,
    context: &str,
) -> bool {
    let Some(start) = with_current_player_local_state(state, player, |local_state| {
        let snapshot = player.get_state();
        if local_state.remote_pause_is_handled(paused, snapshot.observed_paused) {
            RemoteCorrectionStart::AlreadyActive
        } else {
            RemoteCorrectionStart::Started(
                local_state.begin_remote_pause(paused, snapshot.paused_observation_generation),
            )
        }
    }) else {
        return false;
    };
    let RemoteCorrectionStart::Started(correction) = start else {
        return true;
    };
    if let Err(e) = player.set_paused(paused).await {
        with_current_player_local_state(state, player, |local_state| {
            local_state.cancel_remote_pause(correction);
        });
        tracing::warn!("Failed to set paused ({}): {}", context, e);
        return false;
    }
    let global = state.effective_global_state();
    with_current_player_local_state(state, player, |local_state| {
        let snapshot = player.get_state();
        local_state.complete_remote_pause(correction);
        if let (Some(position), Some(paused)) = (snapshot.position, snapshot.paused) {
            local_state.update_from_player(
                position,
                paused,
                snapshot.observed_position,
                snapshot.observed_paused,
                snapshot.position_observation_generation,
                snapshot.paused_observation_generation,
                global.position,
                global.paused,
            );
        }
    })
    .is_some()
}

async fn handle_state_update(state: &Arc<AppState>, playstate: PlayState, message_age: f64) {
    let had_last_global = state.last_global_update.lock().is_some();
    *state.last_global_update.lock() = Some(std::time::Instant::now());
    if !had_last_global && state.last_updated_file_time.lock().is_none() {
        if let Some(connection) = state.connection.lock().clone() {
            if let Err(e) = connection.send(ProtocolMessage::List { List: None }) {
                tracing::warn!(
                    "Failed to request user list after first state update: {}",
                    e
                );
            }
        }
    }
    let adjusted_global_position = if !playstate.paused {
        playstate.position + message_age
    } else {
        playstate.position
    };
    let previous_global = state.client_state.get_global_state();
    state.client_state.set_global_state(
        adjusted_global_position,
        playstate.paused,
        playstate.set_by.clone(),
    );

    let player = state.player.lock().clone();
    let Some(player) = player else { return };
    let player_kind = player.kind();
    let mut player_state: PlayerState = player.get_state();
    let (local_position, local_paused) = match (player_state.position, player_state.paused) {
        (Some(pos), Some(paused)) => (pos, paused),
        _ => {
            if let Err(e) = player.poll_state().await {
                tracing::warn!("Failed to refresh player state: {}", e);
                return;
            }
            player_state = player.get_state();
            match (player_state.position, player_state.paused) {
                (Some(pos), Some(paused)) => (pos, paused),
                _ => return,
            }
        }
    };

    let config = state.config.lock().clone();
    let current_username = state.client_state.get_username();
    let actor_name = playstate
        .set_by
        .clone()
        .unwrap_or_else(|| "Unknown".to_string());
    let do_seek = playstate.do_seek.unwrap_or(false);
    let global_pause_changed = playstate.paused != previous_global.paused;
    let pause_correction_pending = state
        .local_playback_state
        .lock()
        .remote_pause_is_active_for(playstate.paused);
    let pause_needs_sync =
        global_pause_changed || (playstate.paused != local_paused && !pause_correction_pending);
    let diff = local_position - adjusted_global_position;
    let mut made_change_on_player = false;
    let mut position_applied = false;
    let mut pause_applied = false;

    if !had_last_global && state.client_state.get_file().is_some() {
        if try_set_position(state, &player, adjusted_global_position, "init").await {
            made_change_on_player = true;
            position_applied = true;
        }
        if try_set_paused(state, &player, playstate.paused, "init").await {
            made_change_on_player = true;
            pause_applied = true;
        }
    }

    if do_seek {
        let from_position = if actor_name == current_username {
            state
                .last_seek_from_position
                .lock()
                .take()
                .unwrap_or(local_position)
        } else {
            *state.last_seek_from_position.lock() = None;
            if position_applied
                || try_set_position(state, &player, adjusted_global_position, "seek").await
            {
                made_change_on_player = true;
            } else {
                return;
            }
            local_position
        };
        let message = format!(
            "{} jumped from {} to {}",
            actor_name,
            format_time(from_position),
            format_time(adjusted_global_position)
        );
        emit_system_message(state, &message);
        maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
    }

    let position_correction_pending = state
        .local_playback_state
        .lock()
        .has_active_remote_position()
        || position_applied;

    if diff > config.user.seek_threshold_rewind
        && !do_seek
        && !position_correction_pending
        && config.user.rewind_on_desync
        && actor_name != current_username
    {
        if try_set_position(state, &player, adjusted_global_position, "rewind").await {
            made_change_on_player = true;
        }
        let message = format!("Rewinded due to time difference with {}", actor_name);
        emit_system_message(state, &message);
        maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
    }

    if config.user.fastforward_on_desync && should_allow_fastforward(state, &config) {
        let mut next_behind_marker = None;
        let mut fastforward_target = None;
        if diff < -FASTFORWARD_BEHIND_THRESHOLD && !do_seek && !position_correction_pending {
            let now = std::time::Instant::now();
            let start = state.sync_engine.lock().behind_first_detected();
            match start {
                None => {
                    next_behind_marker = Some(Some(now));
                }
                Some(start) => {
                    let duration_behind = now
                        .checked_duration_since(start)
                        .unwrap_or_default()
                        .as_secs_f64();
                    if duration_behind
                        > (config.user.seek_threshold_fastforward - FASTFORWARD_BEHIND_THRESHOLD)
                        && diff < -config.user.seek_threshold_fastforward
                    {
                        fastforward_target =
                            Some(adjusted_global_position + FASTFORWARD_EXTRA_TIME);
                        next_behind_marker = Some(Some(
                            now + Duration::from_secs_f64(FASTFORWARD_RESET_THRESHOLD),
                        ));
                    }
                }
            }
        } else {
            next_behind_marker = Some(None);
        }

        if let Some(position) = fastforward_target {
            if actor_name != current_username {
                if try_set_position(state, &player, position, "fastforward").await {
                    made_change_on_player = true;
                }
                let message = format!("Fast-forwarded due to time difference with {}", actor_name);
                emit_system_message(state, &message);
                maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
            }
        }

        if let Some(marker) = next_behind_marker {
            state.sync_engine.lock().set_behind_first_detected(marker);
        }
    }

    if player_supports_speed(player_kind)
        && !do_seek
        && !playstate.paused
        && config.user.slow_on_desync
    {
        let slowdown_active = state.sync_engine.lock().is_slowdown_active();
        if diff > config.user.slowdown_threshold && !slowdown_active && !position_correction_pending
        {
            if actor_name != current_username {
                if let Err(e) = player.set_speed(config.user.slowdown_rate).await {
                    tracing::warn!("Failed to set slowdown: {}", e);
                } else {
                    made_change_on_player = true;
                }
                state.sync_engine.lock().set_slowdown_active(true);
                let message = format!("Slowing down due to time difference with {}", actor_name);
                emit_system_message(state, &message);
                maybe_show_osd(state, &config, &message, config.user.show_slowdown_osd);
            }
        } else if slowdown_active && diff < config.user.slowdown_reset_threshold {
            if let Err(e) = player.set_speed(1.0).await {
                tracing::warn!("Failed to reset speed: {}", e);
            } else {
                made_change_on_player = true;
            }
            state.sync_engine.lock().set_slowdown_active(false);
            let message = "Reverting speed back to normal".to_string();
            emit_system_message(state, &message);
            maybe_show_osd(state, &config, &message, config.user.show_slowdown_osd);
        }
    }

    if pause_needs_sync {
        if playstate.paused {
            if actor_name != current_username
                && !do_seek
                && !position_correction_pending
                && try_set_position(state, &player, adjusted_global_position, "pause-sync").await
            {
                made_change_on_player = true;
            }
            if pause_applied || try_set_paused(state, &player, true, "sync").await {
                made_change_on_player = true;
            }
            if global_pause_changed {
                let message = format!(
                    "{} paused at {}",
                    actor_name,
                    format_time(adjusted_global_position)
                );
                emit_system_message(state, &message);
                maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
            }
        } else {
            if pause_applied || try_set_paused(state, &player, false, "sync").await {
                made_change_on_player = true;
            }
            if global_pause_changed {
                let message = format!("{} unpaused", actor_name);
                emit_system_message(state, &message);
                maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
            }
        }
    }

    if made_change_on_player {
        if let Err(e) = player.poll_state().await {
            tracing::warn!("Failed to refresh player state after update: {}", e);
        }
        let refreshed_state = player.get_state();
        let global = state.effective_global_state();
        with_current_player_local_state(state, &player, |local_state| {
            if let (Some(position), Some(paused)) =
                (refreshed_state.position, refreshed_state.paused)
            {
                local_state.update_from_player(
                    position,
                    paused,
                    refreshed_state.observed_position,
                    refreshed_state.observed_paused,
                    refreshed_state.position_observation_generation,
                    refreshed_state.paused_observation_generation,
                    global.position,
                    global.paused,
                );
            }
        });
    }

    // Report the viewer's own offset from the room-global position so the UI
    // can show an "in sync / behind / ahead" indicator. Read the freshest
    // player state so corrections applied above are reflected immediately.
    let final_local_position = player.get_state().position;
    if let Some(final_local_position) = final_local_position {
        state.emit_event(
            "sync-offset-updated",
            serde_json::json!({ "offsetSeconds": final_local_position - adjusted_global_position }),
        );
    }

    update_room_warnings(state, false);
}

fn update_ignoring_on_the_fly(state: &Arc<AppState>, ignoring: &IgnoringInfo) {
    let mut local = state.ignoring_on_the_fly.lock();
    if let Some(server) = ignoring.server {
        local.server = server;
        local.client = 0;
    } else if let Some(client) = ignoring.client {
        if client == local.client {
            local.client = 0;
        }
    }
}

fn build_local_playstate(state: &Arc<AppState>) -> Option<PlayState> {
    if state.last_global_update.lock().is_none() {
        return None;
    }
    let global = state.effective_global_state();
    let local_state = state.local_playback_state.lock();
    let (local_position, local_paused) =
        local_state.protocol_state(global.position, global.paused)?;
    let config = state.config.lock().clone();
    let position = if config.user.dont_slow_down_with_me {
        global.position
    } else {
        local_position
    };
    let do_seek = if local_state.compute_seeked(position, global.position) {
        Some(true)
    } else {
        None
    };
    Some(PlayState {
        position,
        paused: local_paused,
        do_seek,
        set_by: None,
    })
}

pub(crate) fn send_state_message(
    state: &Arc<AppState>,
    playstate: Option<PlayState>,
    latency_calculation: Option<f64>,
    state_change: bool,
) -> Result<(), String> {
    let mut ignoring = state.ignoring_on_the_fly.lock();
    let client_ignore_is_not_set = ignoring.client == 0 || ignoring.server != 0;
    let playstate = if client_ignore_is_not_set {
        playstate
    } else {
        None
    };
    if state_change {
        ignoring.client = ignoring.client.saturating_add(1);
    }
    let ignoring_info = if ignoring.server != 0 || ignoring.client != 0 {
        Some(IgnoringInfo {
            server: if ignoring.server != 0 {
                Some(ignoring.server)
            } else {
                None
            },
            client: if ignoring.client != 0 {
                Some(ignoring.client)
            } else {
                None
            },
        })
    } else {
        None
    };
    if ignoring.server != 0 {
        ignoring.server = 0;
    }
    drop(ignoring);

    let ping = PingInfo {
        latency_calculation,
        client_latency_calculation: Some(crate::network::ping::PingService::new_timestamp()),
        client_rtt: Some(state.ping_service.lock().get_rtt()),
        server_rtt: None,
    };
    *state.last_state_message_sent.lock() = Some(std::time::Instant::now());
    let message = ProtocolMessage::State {
        State: StateMessage {
            playstate,
            ping: Some(ping),
            ignoring_on_the_fly: ignoring_info,
        },
    };
    let Some(connection) = state.connection.lock().clone() else {
        return Err("Not connected".to_string());
    };
    connection.send(message).map_err(|e| e.to_string())
}

pub(crate) fn emit_system_message(state: &Arc<AppState>, message: &str) {
    state.chat.add_system_message(message.to_string());
    state.emit_event(
        "chat-message-received",
        serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "username": null,
            "message": message,
            "messageType": "system",
        }),
    );
}

fn should_allow_fastforward(state: &Arc<AppState>, config: &crate::config::SyncplayConfig) -> bool {
    if config.user.dont_slow_down_with_me {
        return true;
    }
    let can_control = current_user_can_control(state);
    !can_control
}

fn player_supports_speed(kind: crate::player::backend::PlayerKind) -> bool {
    !matches!(
        kind,
        crate::player::backend::PlayerKind::MpcHc | crate::player::backend::PlayerKind::MpcBe
    )
}

pub(crate) fn emit_error_message(state: &Arc<AppState>, message: &str) {
    state.chat.add_error_message(message.to_string());
    state.emit_event(
        "chat-message-received",
        serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "username": null,
            "message": message,
            "messageType": "error",
        }),
    );
}

pub(crate) async fn reset_room_sync_state(state: &Arc<AppState>) {
    *state.last_global_update.lock() = None;
    state.client_state.set_global_state(0.0, true, None);
    *state.playlist_may_need_restoring.lock() = false;
    playback_runtime::reconnect(state).await;
}

pub(crate) fn maybe_show_osd(
    state: &Arc<AppState>,
    config: &crate::config::SyncplayConfig,
    message: &str,
    allow: bool,
) {
    if !allow || !config.user.show_osd {
        return;
    }
    let player = state.player.lock().clone();
    let Some(player) = player else { return };
    if let Err(e) = player.show_osd(message, Some(config.user.osd_duration)) {
        tracing::warn!("Failed to show OSD: {}", e);
    }
}

fn start_room_warning_loop(state: Arc<AppState>) {
    let mut running = state.room_warning_task_running.lock();
    if *running {
        return;
    }
    *running = true;
    drop(running);

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(WARNING_OSD_INTERVAL_SECONDS));
        loop {
            ticker.tick().await;
            if !state.is_connected() {
                *state.room_warning_task_running.lock() = false;
                break;
            }
            update_room_warnings(&state, true);
        }
    });
}

fn update_room_warnings(state: &Arc<AppState>, osd_only: bool) {
    let config = state.config.lock().clone();
    if autoplay_conditions_met(state) {
        return;
    }
    let warnings = compute_room_warning_state(state, &config);
    let show_osd = config.user.show_osd && config.user.show_osd_warnings;
    let mut last = state.room_warning_state.lock();
    let mut timers = state.warning_timers.lock();

    if !osd_only && warnings.alone && !last.alone {
        emit_system_message(state, "You are currently by yourself in the room");
    }

    let was_not_ready = last.not_ready.is_some();

    update_warning_timer_state(&mut timers.alone, warnings.alone);
    update_warning_timer_state(
        &mut timers.file_differences,
        warnings.file_differences.is_some(),
    );
    update_warning_timer_state(&mut timers.not_ready, warnings.not_ready.is_some());

    if should_reset_not_ready_timer(state, &warnings) {
        timers.not_ready.displayed_for = 0;
    }

    if show_osd {
        if osd_only {
            if tick_warning_timer(&mut timers.alone) {
                show_room_warning_osd(state, &config, &warnings);
            }
            if tick_warning_timer(&mut timers.file_differences) {
                show_room_warning_osd(state, &config, &warnings);
            }
            if tick_warning_timer(&mut timers.not_ready) {
                show_room_warning_osd(state, &config, &warnings);
            }
        } else if warnings.alone
            || warnings.file_differences.is_some()
            || warnings.not_ready.is_some()
            || (was_not_ready && warnings.not_ready.is_none())
        {
            show_room_warning_osd(state, &config, &warnings);
        }
    }

    *last = warnings;
}

fn show_room_warning_osd(
    state: &Arc<AppState>,
    config: &crate::config::SyncplayConfig,
    warnings: &crate::app_state::RoomWarningState,
) {
    let Some(message) = build_room_warning_message(state, config, warnings) else {
        return;
    };
    maybe_show_osd(state, config, &message, true);
}

fn update_warning_timer_state(timer: &mut WarningTimerState, active: bool) {
    if active {
        if !timer.active {
            timer.active = true;
            timer.displayed_for = 0;
        }
    } else {
        timer.active = false;
        timer.displayed_for = 0;
    }
}

fn tick_warning_timer(timer: &mut WarningTimerState) -> bool {
    if !timer.active {
        return false;
    }
    if timer.displayed_for >= OSD_WARNING_MESSAGE_DURATION_SECONDS {
        timer.displayed_for = 0;
        timer.active = false;
        return false;
    }
    timer.displayed_for = timer
        .displayed_for
        .saturating_add(WARNING_OSD_INTERVAL_SECONDS as u32);
    true
}

fn should_reset_not_ready_timer(
    state: &Arc<AppState>,
    warnings: &crate::app_state::RoomWarningState,
) -> bool {
    if warnings.alone || !is_readiness_supported(state, true) {
        return false;
    }
    let player_paused = state
        .local_playback_state
        .lock()
        .current()
        .map(|(_, paused)| paused)
        .unwrap_or(true);
    let current_ready = current_user_ready_with_file(state) == Some(true);
    let all_relevant_ready = are_all_relevant_users_in_room_ready(state, false);
    player_paused || !current_ready || !all_relevant_ready
}

fn build_room_warning_message(
    state: &Arc<AppState>,
    config: &crate::config::SyncplayConfig,
    warnings: &crate::app_state::RoomWarningState,
) -> Option<String> {
    if !config.user.show_osd {
        return None;
    }
    if state.player.lock().is_none() {
        return None;
    }
    if state.autoplay.lock().countdown_active {
        return None;
    }

    if warnings.alone {
        return Some("You are currently by yourself in the room".to_string());
    }

    let file_diff_message = warnings
        .file_differences
        .as_ref()
        .map(|file_diff| format!("File differences: {}", file_diff));

    let readiness_supported = is_readiness_supported(state, true);
    let ready_message = if readiness_supported {
        if are_all_users_in_room_ready(state, false) {
            Some(format!(
                "Everyone is ready ({} users)",
                ready_user_count(state)
            ))
        } else {
            warnings.not_ready.clone()
        }
    } else {
        None
    };

    if let Some(file_diff_message) = file_diff_message {
        if current_user_can_control(state) && readiness_supported {
            if let Some(ready_message) = ready_message {
                return Some(format!(
                    "{}{}{}",
                    file_diff_message, OSD_MESSAGE_SEPARATOR, ready_message
                ));
            }
        }
        return Some(file_diff_message);
    }

    ready_message
}

fn compute_room_warning_state(
    state: &Arc<AppState>,
    config: &crate::config::SyncplayConfig,
) -> crate::app_state::RoomWarningState {
    let current_room = state.client_state.get_room();
    let current_username = state.client_state.get_username();
    let users = state.client_state.get_users();
    let users_in_room: Vec<crate::client::state::User> = users
        .into_iter()
        .filter(|user| user.room == current_room)
        .collect();

    let others_in_room: Vec<crate::client::state::User> = users_in_room
        .iter()
        .filter(|user| user.username != current_username)
        .cloned()
        .collect();
    let alone = others_in_room.is_empty() && !recently_connected(state);

    let current_media = state.client_state.get_file_info();
    let current_file = current_media.name;
    let current_size = current_media.size;
    let current_duration = current_media.duration;
    let mut diff_name = false;
    let mut diff_size = false;
    let mut diff_duration = false;
    if let Some(current_file) = current_file.as_ref() {
        for user in others_in_room
            .iter()
            .filter(|user| user_can_control_in_room(state, user))
        {
            let Some(other_file) = user.file.as_ref() else {
                continue;
            };
            if !same_filename(Some(current_file), Some(other_file)) {
                diff_name = true;
            }
            if !crate::utils::same_filesize(current_size.as_ref(), user.file_size.as_ref()) {
                diff_size = true;
            }
            if !same_duration(
                current_duration,
                user.file_duration,
                config.user.show_duration_notification,
            ) {
                diff_duration = true;
            }
        }
    }

    let mut diff_parts = Vec::new();
    if diff_name {
        diff_parts.push("name");
    }
    if diff_size {
        diff_parts.push("size");
    }
    if diff_duration {
        diff_parts.push("duration");
    }
    let file_differences = if diff_parts.is_empty() {
        None
    } else {
        Some(diff_parts.join(", "))
    };

    let not_ready = if alone
        || !is_readiness_supported(state, true)
        || are_all_relevant_users_in_room_ready(state, false)
    {
        None
    } else {
        let mut not_ready_users: Vec<String> = Vec::new();
        if current_user_ready_with_file(state) != Some(true) {
            not_ready_users.push(current_username.clone());
        }
        for user in users_in_room.iter() {
            if user.username == current_username {
                continue;
            }
            if user.is_ready_with_file() == Some(false) {
                not_ready_users.push(user.username.clone());
            }
        }
        if not_ready_users.is_empty() {
            None
        } else {
            Some(format!("Not ready: {}", not_ready_users.join(", ")))
        }
    };

    crate::app_state::RoomWarningState {
        alone,
        file_differences,
        not_ready,
    }
}

fn format_time(time_seconds: f64) -> String {
    let mut seconds = time_seconds.round() as i64;
    let sign = if seconds < 0 {
        seconds = -seconds;
        "-"
    } else {
        ""
    };

    let weeks = seconds / 604_800;
    let days = (seconds % 604_800) / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;

    if weeks > 0 {
        format!(
            "{}{}w, {}d, {:02}:{:02}:{:02}",
            sign, weeks, days, hours, minutes, secs
        )
    } else if days > 0 {
        format!("{}{}d, {:02}:{:02}:{:02}", sign, days, hours, minutes, secs)
    } else if hours > 0 {
        format!("{}{:02}:{:02}:{:02}", sign, hours, minutes, secs)
    } else {
        format!("{}{:02}:{:02}", sign, minutes, secs)
    }
}

pub(crate) fn store_control_password(
    state: &Arc<AppState>,
    room: &str,
    password: &str,
    persist: bool,
) {
    let password = strip_control_password(password);
    if password.is_empty() {
        return;
    }
    state
        .controlled_room_passwords
        .lock()
        .insert(room.to_string(), password.clone());

    if !persist {
        return;
    }
    let config = state.config.lock().clone();
    if !config.user.autosave_joins_to_list {
        return;
    }
    let room_entry = format!("{}:{}", room, password);
    if config.user.room_list.contains(&room_entry) {
        return;
    }
    let Some(app) = state.app_handle.lock().clone() else {
        return;
    };
    let mut updated = config.clone();
    updated.user.room_list.push(room_entry);
    if let Err(e) = save_config(&app, &updated) {
        tracing::warn!("Failed to save room list after control password: {}", e);
        return;
    }
    *state.config.lock() = updated.clone();
    state.emit_event("config-updated", updated);
}

pub fn reidentify_as_controller(state: &Arc<AppState>) {
    let room = state.client_state.get_room();
    if !is_controlled_room(&room) {
        return;
    }
    let password = state.controlled_room_passwords.lock().get(&room).cloned();
    let Some(password) = password else {
        return;
    };
    let message = format!(
        "Identifying as room operator with password '{}'...",
        password
    );
    emit_system_message(state, &message);
    *state.last_control_password_attempt.lock() = Some(password.clone());
    if let Err(e) = send_controller_auth(state, &room, &password) {
        tracing::warn!("Failed to send controller auth: {}", e);
    }
}

pub(crate) fn send_controller_auth(
    state: &Arc<AppState>,
    room: &str,
    password: &str,
) -> Result<(), String> {
    let connection = state.connection.lock().clone();
    let Some(connection) = connection else {
        return Err("Not connected to server".to_string());
    };
    connection
        .send(controller_auth_request(room, password))
        .map_err(|e| format!("Failed to send controller auth: {}", e))
}

fn controller_auth_request(room: &str, password: &str) -> ProtocolMessage {
    ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: None,
            playlist_index: None,
            playlist_change: None,
            controller_auth: Some(ControllerAuth {
                room: Some(room.to_string()),
                password: Some(password.to_string()),
                user: None,
                success: None,
            }),
            new_controlled_room: None,
            features: None,
        }),
    }
}

pub(crate) async fn reset_transient_connection_state(state: &Arc<AppState>) {
    state.client_state.clear_users();
    state.client_state.clear_server_version();
    state.local_playback_state.lock().clear_remote_corrections();
    playback_runtime::reconnect(state).await;
    *state.server_features.lock() = ServerFeatures::default();
    *state.ignoring_on_the_fly.lock() = crate::app_state::IgnoringOnTheFlyState::default();
    *state.last_global_update.lock() = None;
    state.client_state.set_global_state(0.0, true, None);
    *state.last_protocol_activity.lock() = None;
    *state.last_rewind_time.lock() = None;
    *state.last_seek_from_position.lock() = None;
    *state.last_advance_time.lock() = None;
    *state.last_updated_file_time.lock() = None;
    *state.last_paused_on_leave_time.lock() = None;
    *state.playlist_may_need_restoring.lock() = false;
    *state.room_warning_state.lock() = crate::app_state::RoomWarningState::default();
    *state.warning_timers.lock() = WarningTimers::default();
    *state.room_warning_task_running.lock() = false;
    *state.autoplay.lock() = crate::app_state::AutoPlayState::default();
}

pub(crate) async fn handle_connection_closed(state: &Arc<AppState>) {
    handle_connection_closed_for_session(state, None, None).await;
}

async fn handle_connection_closed_for_session(
    state: &Arc<AppState>,
    expected_connection: Option<Arc<Connection>>,
    expected_generation: Option<u64>,
) {
    let connection = {
        let mut guard = state.connection.lock();
        if let Some(expected) = expected_connection.as_ref() {
            match guard.as_ref() {
                Some(current) if Arc::ptr_eq(current, expected) => guard.take(),
                _ => return,
            }
        } else {
            guard.take()
        }
    };
    if connection.is_none() {
        return;
    }
    let session_generation =
        expected_generation.unwrap_or_else(|| current_connection_session(state));
    let terminal_error = connection
        .as_ref()
        .and_then(|connection| connection.take_terminal_error());
    let manual_disconnect = *state.manual_disconnect.lock();
    if manual_disconnect {
        tracing::info!(
            "connection_lifecycle: manual disconnect closed connection; stopping player"
        );
        *state.manual_disconnect.lock() = false;
        if let Err(error) = clear_disconnected_session_state(state, "unknown").await {
            tracing::warn!("Failed to clean up after manual disconnect: {error}");
        }
        return;
    }

    if let Some(error) = terminal_error {
        if !invalidate_connection_session(state, session_generation) {
            tracing::debug!("Ignoring terminal close from superseded logical session");
            return;
        }
        disable_reconnect(state);
        let tls_status = match &error {
            TerminalConnectionError::TlsCertificate(_) => "certificate-invalid",
            TerminalConnectionError::Protocol(_) => "closed",
        };
        clear_terminal_server_session_state(state, tls_status);
        emit_error_message(state, error.message());
        return;
    }

    let reconnect_enabled = state.reconnect_state.lock().enabled;
    state.client_state.clear_server_version();
    *state.server_features.lock() = ServerFeatures::default();
    if !reconnect_enabled {
        state.client_state.set_ready_state(None);
    }

    *state.room_warning_state.lock() = crate::app_state::RoomWarningState::default();
    *state.warning_timers.lock() = WarningTimers::default();
    *state.room_warning_task_running.lock() = false;

    state.emit_event(
        "connection-status-changed",
        ConnectionStatusEvent {
            connected: false,
            server: None,
        },
    );
    state.emit_event(
        "tls-status-changed",
        serde_json::json!({ "status": "closed" }),
    );

    if reconnect_enabled && is_current_connection_generation(state, session_generation) {
        start_reconnect_loop(state.clone(), session_generation);
    } else {
        emit_system_message(state, "Disconnected from server");
    }
}

async fn handle_set_message(state: &Arc<AppState>, set_msg: SetMessage) {
    if let Some(room) = set_msg.room {
        set_authoritative_room(state, room.name);
        reset_room_sync_state(state).await;
        reidentify_as_controller(state);
    }

    if set_msg.file.is_some() {
        tracing::debug!("Ignoring inbound top-level Set.file; original client treats it as client-to-server only");
    }

    let mut users_changed = false;
    let mut left_in_room = false;
    if let Some(user_updates) = set_msg.user {
        for (username, update) in user_updates {
            if update
                .event
                .as_ref()
                .and_then(|event| event.left)
                .unwrap_or(false)
            {
                if let Some(user) = state.client_state.get_user(&username) {
                    if user.room == state.client_state.get_room() {
                        left_in_room = true;
                    }
                }
            }
            if apply_user_update(state, username, update) {
                users_changed = true;
            }
        }
    }

    if let Some(ready) = set_msg.ready {
        if let Some(username) = ready.username.clone() {
            if is_placeholder_username(&username) {
                tracing::debug!("Ready update contains placeholder username, ignoring");
            } else {
                let is_ready = ready.is_ready;

                if let Some(mut user) = state.client_state.get_user(&username) {
                    user.is_ready = is_ready;
                    state.client_state.add_user(user);
                    users_changed = true;
                } else {
                    state.client_state.add_user(crate::client::state::User {
                        username: username.clone(),
                        room: state.client_state.get_room(),
                        file: None,
                        file_size: None,
                        file_duration: None,
                        is_ready,
                        is_controller: false,
                        features: None,
                    });
                    users_changed = true;
                }

                if username == state.client_state.get_username() {
                    state.client_state.set_ready_state(is_ready);
                }

                if let Some(set_by) = ready.set_by {
                    let message = if is_ready.unwrap_or(false) {
                        format!("{} was set as ready by {}", username, set_by)
                    } else {
                        format!("{} was set as not ready by {}", username, set_by)
                    };
                    emit_system_message(state, &message);
                }
            }
        } else {
            tracing::debug!("Ready state missing username, ignoring");
        }
    }

    if let Some(controller_auth) = set_msg.controller_auth {
        handle_controller_auth(state, controller_auth);
    }

    if let Some(new_room) = set_msg.new_controlled_room {
        handle_new_controlled_room(state, new_room).await;
    }

    if let Some(features) = set_msg.features {
        if apply_user_features_update(state, features) {
            users_changed = true;
        }
    }

    if users_changed {
        emit_user_list(state);
    }

    if left_in_room {
        let config = state.config.lock().clone();
        if config.user.pause_on_leave {
            pause_local_player(state).await;
        }
    }

    let config = state.config.lock().clone();
    let shared_playlists = shared_playlists_enabled(state, &config);
    let room = state.client_state.get_room();
    let mut playlist_items = None;

    if let Some(change) = set_msg.playlist_change {
        let should_restore = {
            let mut may_restore = state.playlist_may_need_restoring.lock();
            let should_restore = shared_playlists
                && *may_restore
                && change.files.is_empty()
                && change.user.is_none()
                && !state.playlist.snapshot().0.is_empty()
                && !state.playlist.playlist_buffer_is_from_old_room(&room);
            *may_restore = false;
            should_restore
        };

        if should_restore {
            restore_playlist_after_reconnect(state);
        } else {
            playlist_items = Some(change.files);
        }

        if let Some(user) = change.user {
            let message = format!("{} updated the playlist", user);
            emit_system_message(state, &message);
            maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
        }
    }

    let mut playlist_index = None;
    if let Some(index_update) = set_msg.playlist_index {
        playlist_index = Some((index_update.index, true));
        if index_update.index.is_some() {
            if let Some(user) = index_update.user {
                let message = format!("{} changed the playlist selection", user);
                emit_system_message(state, &message);
                maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
            }
        }
    }

    if playlist_items.is_some() || playlist_index.is_some() {
        if let Err(error) =
            playback_runtime::server_playlist_and_index(state, playlist_items, playlist_index).await
        {
            tracing::warn!("Failed to apply server playlist state: {}", error);
        }
    }

    evaluate_autoplay(state);
}

fn restore_playlist_after_reconnect(state: &Arc<AppState>) {
    let Some(connection) = state.connection.lock().clone() else {
        return;
    };
    let (items, index) = state.playlist.snapshot();
    let playlist_message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: None,
            playlist_index: None,
            playlist_change: Some(crate::network::messages::PlaylistChange {
                user: None,
                files: items,
            }),
            controller_auth: None,
            new_controlled_room: None,
            features: None,
        }),
    };
    if let Err(error) = connection.send(playlist_message) {
        tracing::warn!("Failed to restore playlist: {}", error);
        return;
    }

    let index_message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: None,
            playlist_index: Some(crate::network::messages::PlaylistIndexUpdate {
                user: None,
                index,
            }),
            playlist_change: None,
            controller_auth: None,
            new_controlled_room: None,
            features: None,
        }),
    };
    if let Err(error) = connection.send(index_message) {
        tracing::warn!("Failed to restore playlist index: {}", error);
    }
}

fn handle_controller_auth(state: &Arc<AppState>, auth: ControllerAuth) {
    let Some(success) = auth.success else {
        return;
    };
    let username = auth
        .user
        .clone()
        .unwrap_or_else(|| state.client_state.get_username());
    let room = auth
        .room
        .clone()
        .unwrap_or_else(|| state.client_state.get_room());
    let current_room = state.client_state.get_room();
    let current_username = state.client_state.get_username();
    let config = state.config.lock().clone();

    if success {
        let changed = set_user_controller_status(state, &username, Some(&room), true);
        if room == current_room {
            let message = format!("{} authenticated as a room operator", username);
            emit_system_message(state, &message);
            maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
        }
        if username == current_username {
            if let Some(password) = state.last_control_password_attempt.lock().clone() {
                store_control_password(state, &room, &password, true);
            }
        }
        if changed {
            emit_user_list(state);
        }
    } else if username == current_username {
        let message = format!("{} failed to identify as a room operator.", username);
        emit_error_message(state, &message);
    }
}

async fn handle_new_controlled_room(state: &Arc<AppState>, room: NewControlledRoom) {
    let (Some(room_name), Some(password)) = (room.room_name, room.password) else {
        return;
    };
    let room_with_password = format!("{}:{}", room_name, password);
    let message = format!(
        "Created managed room '{}' with password '{}'. Please save this information for future reference!\n\nIn managed rooms everyone is kept in sync with the room operator(s) who are the only ones who can pause, unpause, seek, and change the playlist.\n\nYou should ask regular viewers to join the room '{}' but the room operators can join the room '{}' to automatically authenticate themselves.",
        room_name,
        password,
        room_name,
        room_with_password,
    );
    emit_system_message(state, &message);

    set_authoritative_room(state, room_name.clone());
    reset_room_sync_state(state).await;
    if let Some(connection) = state.connection.lock().clone() {
        let set_room = ProtocolMessage::Set {
            Set: Box::new(SetMessage {
                room: Some(RoomInfo {
                    name: room_name.clone(),
                    password: None,
                }),
                file: None,
                user: None,
                ready: None,
                playlist_index: None,
                playlist_change: None,
                controller_auth: None,
                new_controlled_room: None,
                features: None,
            }),
        };
        if let Err(e) = connection.send(set_room) {
            tracing::warn!("Failed to set room after controlled room creation: {}", e);
            return;
        }
        if let Err(e) = connection.send(ProtocolMessage::List { List: None }) {
            tracing::warn!(
                "Failed to request list after controlled room creation: {}",
                e
            );
        }
    }
    let password = strip_control_password(&password);
    if !password.is_empty() {
        *state.last_control_password_attempt.lock() = Some(password.clone());
        if let Err(e) = send_controller_auth(state, &room_name, &password) {
            tracing::warn!("Failed to authenticate controller after create: {}", e);
        }
    }
}

fn apply_user_features_update(state: &Arc<AppState>, value: Value) -> bool {
    let Value::Object(mut map) = value else {
        return false;
    };
    let username = map
        .remove("username")
        .and_then(|value| value.as_str().map(|s| s.to_string()));
    let features = map.remove("features");
    let (Some(username), Some(features)) = (username, features) else {
        return false;
    };
    if is_placeholder_username(&username) {
        return false;
    }

    let mut user = state
        .client_state
        .get_user(&username)
        .unwrap_or(crate::client::state::User {
            username: username.clone(),
            room: state.client_state.get_room(),
            file: None,
            file_size: None,
            file_duration: None,
            is_ready: None,
            is_controller: false,
            features: None,
        });
    user.features = Some(features);
    state.client_state.add_user(user);
    true
}

fn set_user_controller_status(
    state: &Arc<AppState>,
    username: &str,
    room: Option<&str>,
    is_controller: bool,
) -> bool {
    let mut user = state
        .client_state
        .get_user(username)
        .unwrap_or(crate::client::state::User {
            username: username.to_string(),
            room: room
                .map(|value| value.to_string())
                .unwrap_or_else(|| state.client_state.get_room()),
            file: None,
            file_size: None,
            file_duration: None,
            is_ready: None,
            is_controller: false,
            features: None,
        });
    if let Some(room) = room {
        user.room = room.to_string();
    }
    let changed = user.is_controller != is_controller;
    user.is_controller = is_controller;
    state.client_state.add_user(user);
    changed
}

async fn handle_tls_message(state: &Arc<AppState>, tls: TLSMessage) {
    let Some(answer) = tls.start_tls.as_deref() else {
        return;
    };

    let connection = state.connection.lock().clone();
    let Some(connection) = connection else { return };

    match answer {
        "true" | "accepted" => {
            tracing::info!(
                timeout_seconds = TLS_NEGOTIATION_TIMEOUT.as_secs(),
                "tls_lifecycle: server accepted TLS; upgrading connection"
            );
            state.emit_event(
                "tls-status-changed",
                serde_json::json!({ "status": "accepted" }),
            );
            match connection
                .upgrade_tls_with_timeout(TLS_NEGOTIATION_TIMEOUT)
                .await
            {
                Ok(tls_info) => {
                    state.emit_event(
                        "tls-status-changed",
                        serde_json::json!({ "status": "enabled" }),
                    );
                    let protocol = tls_info.protocol.unwrap_or_else(|| "TLS".to_string());
                    tracing::info!(protocol = %protocol, "tls_lifecycle: TLS enabled");
                    emit_system_message(
                        state,
                        &format!("Secure connection established ({})", protocol),
                    );
                    send_hello(state);
                }
                Err(e) => {
                    tracing::error!("TLS upgrade failed: {}", e);
                    let error = format!("TLS upgrade failed: {e}");
                    let certificate_error = is_tls_certificate_error(&e);
                    let status = if certificate_error {
                        connection.mark_tls_certificate_error(error);
                        "certificate-invalid"
                    } else {
                        emit_error_message(state, &error);
                        "closed"
                    };
                    state.emit_event(
                        "tls-status-changed",
                        serde_json::json!({ "status": status }),
                    );
                    connection.disconnect();
                }
            }
        }
        "false" | "rejected" => {
            tracing::info!(
                answer,
                "tls_lifecycle: TLS rejected by server; continuing plaintext"
            );
            *state.server_supports_tls.lock() = false;
            state.emit_event(
                "tls-status-changed",
                serde_json::json!({ "status": "rejected" }),
            );
            send_hello(state);
        }
        "unsupported" => {
            tracing::info!("tls_lifecycle: server does not support TLS; sending Hello");
            *state.server_supports_tls.lock() = false;
            state.emit_event(
                "tls-status-changed",
                serde_json::json!({ "status": "unsupported" }),
            );
            send_hello(state);
        }
        "certificate-invalid" => {
            tracing::error!("tls_lifecycle: TLS certificate invalid");
            state.emit_event(
                "tls-status-changed",
                serde_json::json!({ "status": "certificate-invalid" }),
            );
            connection.mark_tls_certificate_error("TLS certificate invalid");
            connection.disconnect();
        }
        "closed" => {
            tracing::info!("tls_lifecycle: TLS negotiation closed by server");
            state.emit_event(
                "tls-status-changed",
                serde_json::json!({ "status": "closed" }),
            );
            connection.mark_protocol_error("TLS negotiation closed by server");
            connection.disconnect();
        }
        _ => {
            tracing::debug!("Ignoring TLS message: {}", answer);
        }
    }
}

fn is_tls_certificate_error(error: &anyhow::Error) -> bool {
    if error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rustls::Error>(),
            Some(
                rustls::Error::InvalidCertificate(_)
                    | rustls::Error::NoCertificatesPresented
                    | rustls::Error::UnsupportedNameType
            )
        )
    }) {
        return true;
    }

    let message = error.to_string().to_ascii_lowercase();
    message.contains("certificate verify failed")
        || message.contains("invalid peer certificate")
        || message.contains("not valid for name")
        || message.contains("invalid dns-id")
}

fn send_hello(state: &Arc<AppState>) {
    let mut hello_sent = state.hello_sent.lock();
    if *hello_sent {
        return;
    }

    let Some(hello) = state.last_hello.lock().clone() else {
        return;
    };
    let Some(connection) = state.connection.lock().clone() else {
        return;
    };

    if let Err(e) = connection.send(ProtocolMessage::Hello { Hello: hello }) {
        tracing::error!("Failed to send Hello message: {}", e);
        return;
    }

    *hello_sent = true;
    tracing::info!("Sent Hello message");
}

fn update_autoplay_state(state: &Arc<AppState>, config: &crate::config::SyncplayConfig) {
    let mut autoplay = state.autoplay.lock();
    autoplay.enabled = config.user.autoplay_enabled;
    autoplay.min_users = config.user.autoplay_min_users;
    autoplay.require_same_filenames = config.user.autoplay_require_same_filenames;
    autoplay.unpause_action = config.user.unpause_action.clone();
    if !autoplay.enabled {
        autoplay.countdown_active = false;
        autoplay.countdown_remaining = 0;
    }
}

fn maybe_autosave_connection<R: Runtime>(
    state: &Arc<AppState>,
    app: &AppHandle<R>,
    config: &crate::config::SyncplayConfig,
    snapshot: ConnectionSnapshot,
) {
    if !config.user.autosave_joins_to_list {
        return;
    }

    let mut updated = config.clone();
    updated.server.host = snapshot.host.to_string();
    updated.server.port = snapshot.port;
    updated.server.password = snapshot.password.clone();
    updated.user.username = snapshot.username.to_string();
    updated.user.default_room = snapshot.room.to_string();

    updated.add_recent_server(ServerConfig {
        host: snapshot.host.to_string(),
        port: snapshot.port,
        password: snapshot.password.clone(),
    });

    if !updated
        .user
        .room_list
        .iter()
        .any(|entry| entry == &snapshot.room)
    {
        updated.user.room_list.insert(0, snapshot.room.to_string());
    }

    if let Err(e) = save_config(app, &updated) {
        tracing::warn!("Failed to save config after connect: {}", e);
        return;
    }

    *state.config.lock() = updated.clone();
    state.emit_event("config-updated", updated);
}

fn current_user_can_control(state: &Arc<AppState>) -> bool {
    let room = state.client_state.get_room();
    if !is_controlled_room(&room) {
        return true;
    }
    let username = state.client_state.get_username();
    state
        .client_state
        .get_user(&username)
        .map(|user| user.is_controller)
        .unwrap_or(false)
}

fn user_can_control_in_room(state: &Arc<AppState>, user: &crate::client::state::User) -> bool {
    let room = state.client_state.get_room();
    if !is_controlled_room(&room) {
        return true;
    }
    user.is_controller
}

fn current_user_ready_with_file(state: &Arc<AppState>) -> Option<bool> {
    state.client_state.get_file()?;
    state.client_state.ready_state()
}

pub(crate) fn is_readiness_supported(state: &Arc<AppState>, requires_other_users: bool) -> bool {
    if !server_features_ready(state) {
        return false;
    }
    let features = state.server_features.lock();
    if !features.readiness {
        return false;
    }
    if !requires_other_users {
        return true;
    }
    let room = state.client_state.get_room();
    let username = state.client_state.get_username();
    let others_support = state
        .client_state
        .get_users_in_room(&room)
        .iter()
        .any(|user| user.username != username && user.is_ready_with_file().is_some());
    if !others_support {
        return false;
    }
    true
}

fn are_all_users_in_room_ready(state: &Arc<AppState>, require_same_filenames: bool) -> bool {
    let current_ready = current_user_ready_with_file(state);
    if current_ready != Some(true) {
        return false;
    }
    let current_file = state.client_state.get_file();
    if require_same_filenames && current_file.is_none() {
        return false;
    }
    let room = state.client_state.get_room();
    let username = state.client_state.get_username();
    for user in state.client_state.get_users_in_room(&room) {
        if user.username == username {
            continue;
        }
        if user.is_ready_with_file() == Some(false) {
            return false;
        }
        if require_same_filenames {
            let Some(current_file) = current_file.as_ref() else {
                return false;
            };
            let Some(other_file) = user.file.as_ref() else {
                return false;
            };
            if !same_filename(Some(current_file), Some(other_file)) {
                return false;
            }
        }
    }
    true
}

fn are_all_relevant_users_in_room_ready(
    state: &Arc<AppState>,
    require_same_filenames: bool,
) -> bool {
    let current_ready = current_user_ready_with_file(state);
    if current_ready != Some(true) {
        return false;
    }
    if current_user_can_control(state) {
        return are_all_users_in_room_ready(state, require_same_filenames);
    }
    let room = state.client_state.get_room();
    let current_file = state.client_state.get_file();
    for user in state.client_state.get_users_in_room(&room) {
        if !user_can_control_in_room(state, &user) {
            continue;
        }
        if user.is_ready_with_file() == Some(false) {
            return false;
        }
        if require_same_filenames {
            let Some(current_file) = current_file.as_ref() else {
                return false;
            };
            let Some(user_file) = user.file.as_ref() else {
                return false;
            };
            if !same_filename(Some(current_file), Some(user_file)) {
                return false;
            }
        }
    }
    true
}

fn are_all_other_users_ready(state: &Arc<AppState>) -> bool {
    let room = state.client_state.get_room();
    let username = state.client_state.get_username();
    for user in state.client_state.get_users_in_room(&room) {
        if user.username == username {
            continue;
        }
        if user.is_ready_with_file() == Some(false) {
            return false;
        }
    }
    true
}

fn users_in_room_count(state: &Arc<AppState>) -> usize {
    let room = state.client_state.get_room();
    let username = state.client_state.get_username();
    let mut count = 1;
    for user in state.client_state.get_users_in_room(&room) {
        if user.username == username {
            continue;
        }
        if user.is_ready_with_file() == Some(true) {
            count += 1;
        }
    }
    count
}

fn server_features_ready(state: &Arc<AppState>) -> bool {
    state.client_state.get_server_version().is_some()
}

fn shared_playlists_enabled(state: &Arc<AppState>, config: &crate::config::SyncplayConfig) -> bool {
    config.user.shared_playlist_enabled && state.server_features.lock().shared_playlists
}

fn recently_connected(state: &Arc<AppState>) -> bool {
    let guard = state.last_connect_time.lock();
    let Some(last_connect) = guard.as_ref() else {
        return true;
    };
    last_connect.elapsed().as_secs_f64() < LAST_PAUSED_DIFF_THRESHOLD_SECONDS
}

fn recently_advanced(state: &Arc<AppState>) -> bool {
    let guard = state.last_advance_time.lock();
    let Some(last_advance) = guard.as_ref() else {
        return false;
    };
    last_advance.elapsed().as_secs_f64() < (AUTOPLAY_DELAY_SECONDS as f64 + 5.0)
}

fn is_playing_music(state: &Arc<AppState>) -> bool {
    state
        .client_state
        .get_file()
        .as_deref()
        .map(crate::utils::is_music_file)
        .unwrap_or(false)
}

fn seamless_music_override(state: &Arc<AppState>) -> bool {
    is_playing_music(state) && recently_advanced(state)
}

fn maybe_unpause_for_music(state: &Arc<AppState>) {
    if !seamless_music_override(state) {
        return;
    }
    let session_generation = current_connection_session(state);
    let state_clone = state.clone();
    tokio::spawn(async move {
        if let Err(e) = ensure_player_connected_for_session(&state_clone, session_generation).await
        {
            tracing::warn!("Failed to connect player for music override: {}", e);
            return;
        }
        if !is_current_connection_generation(&state_clone, session_generation) {
            return;
        }
        let player = state_clone.player.lock().clone();
        if let Some(player) = player {
            if let Err(e) = player.set_paused(false).await {
                tracing::warn!("Failed to unpause during music override: {}", e);
            }
        }
    });
}

fn send_login_ready_state(state: &Arc<AppState>, is_ready: bool) -> Result<(), String> {
    state.client_state.set_ready(is_ready);
    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: Some(crate::network::messages::ReadyState {
                username: None,
                is_ready: Some(is_ready),
                manually_initiated: Some(false),
                set_by: None,
            }),
            playlist_index: None,
            playlist_change: None,
            controller_auth: None,
            new_controlled_room: None,
            features: None,
        }),
    };
    let connection = state.connection.lock().clone();
    let Some(connection) = connection else {
        return Err("Not connected to server".to_string());
    };
    connection
        .send(message)
        .map_err(|e| format!("Failed to send ready state: {}", e))
}

fn autoplay_conditions_met(state: &Arc<AppState>) -> bool {
    let config = state.config.lock().clone();
    maybe_unpause_for_music(state);
    if is_playing_music(state) {
        return false;
    }
    let autoplay_enabled = config.user.autoplay_enabled;
    let recently_advanced = recently_advanced(state);
    if !autoplay_enabled && !recently_advanced {
        return false;
    }

    if !current_user_can_control(state) {
        return false;
    }
    if !is_readiness_supported(state, true) {
        return false;
    }
    if !are_all_users_in_room_ready(state, config.user.autoplay_require_same_filenames) {
        return false;
    }

    if config.user.autoplay_min_users > 0 {
        let count = users_in_room_count(state) as i32;
        if count < config.user.autoplay_min_users && !recently_advanced {
            return false;
        }
    }

    let player_state = state.player.lock().clone().map(|player| player.get_state());
    if let Some(player_state) = player_state {
        if player_state.paused == Some(false) {
            return false;
        }
    }

    true
}

fn start_autoplay_countdown(state: Arc<AppState>) {
    let session_generation = current_connection_session(&state);
    {
        let mut autoplay = state.autoplay.lock();
        if autoplay.countdown_active {
            return;
        }
        autoplay.countdown_active = true;
        autoplay.countdown_remaining = AUTOPLAY_DELAY_SECONDS;
    }

    tokio::spawn(async move {
        loop {
            let mut should_stop = false;
            let mut should_unpause = false;
            {
                let mut autoplay = state.autoplay.lock();
                if !autoplay.countdown_active {
                    return;
                }
                if !autoplay_conditions_met(&state) {
                    autoplay.countdown_active = false;
                    autoplay.countdown_remaining = 0;
                    return;
                }
                if autoplay.countdown_remaining <= 0 {
                    autoplay.countdown_active = false;
                    should_unpause = true;
                } else {
                    autoplay.countdown_remaining -= 1;
                }
            }

            if !should_unpause {
                let remaining = state.autoplay.lock().countdown_remaining;
                let ready_count = ready_user_count(&state);
                let message = format!(
                    "All users ready ({}) - autoplaying in {}s",
                    ready_count, remaining
                );
                if let Some(player) = state.player.lock().clone() {
                    let _ = player.show_osd(&message, Some(1000));
                }
            }

            if should_unpause {
                if let Err(e) =
                    ensure_player_connected_for_session(&state, session_generation).await
                {
                    tracing::warn!("Failed to connect to player for autoplay: {}", e);
                    return;
                }
                if !is_current_connection_generation(&state, session_generation) {
                    return;
                }
                let player = state.player.lock().clone();
                if let Some(player) = player {
                    if let Err(e) = player.set_paused(false).await {
                        tracing::warn!("Failed to autoplay unpause: {}", e);
                    }
                }
                should_stop = true;
            }

            if should_stop {
                return;
            }

            sleep(Duration::from_secs(1)).await;
        }
    });
}

pub(crate) fn evaluate_autoplay(state: &Arc<AppState>) {
    if autoplay_conditions_met(state) {
        start_autoplay_countdown(state.clone());
    } else {
        let mut autoplay = state.autoplay.lock();
        autoplay.countdown_active = false;
        autoplay.countdown_remaining = 0;
    }
}

fn ready_user_count(state: &Arc<AppState>) -> usize {
    let room = state.client_state.get_room();
    let mut count = 0usize;
    if state.client_state.get_file().is_some() && state.client_state.is_ready() {
        count += 1;
    }
    for user in state.client_state.get_users_in_room(&room) {
        if user.is_ready_with_file() == Some(true) {
            count += 1;
        }
    }
    count
}

async fn pause_local_player(state: &Arc<AppState>) {
    let session_generation = current_connection_session(state);
    if let Err(e) = ensure_player_connected_for_session(state, session_generation).await {
        tracing::warn!("Failed to connect to player for pause: {}", e);
        return;
    }
    if !is_current_connection_generation(state, session_generation) {
        return;
    }
    let player = state.player.lock().clone();
    if let Some(player) = player {
        if let Err(e) = player.set_paused(true).await {
            tracing::warn!("Failed to pause player: {}", e);
        }
        *state.last_paused_on_leave_time.lock() = Some(std::time::Instant::now());
    }
}

fn apply_user_update(state: &Arc<AppState>, username: String, update: UserUpdate) -> bool {
    if is_placeholder_username(&username) {
        tracing::debug!("User update contains placeholder username, ignoring");
        return false;
    }

    let config = state.config.lock().clone();
    let current_username = state.client_state.get_username();
    let current_room = state.client_state.get_room();
    let old_user = state.client_state.get_user(&username);

    if let Some(event) = update.event.as_ref() {
        if event.left.unwrap_or(false) {
            if let Some(old_user) = old_user.as_ref() {
                let allow_osd = if old_user.room == current_room {
                    config.user.show_same_room_osd
                } else {
                    config.user.show_different_room_osd
                };
                let message = format!("{} has left", username);
                emit_system_message(state, &message);
                maybe_show_osd(state, &config, &message, allow_osd);
            }
            state.client_state.remove_user(&username);
            return true;
        }
    }

    let mut user = state
        .client_state
        .get_user(&username)
        .unwrap_or(crate::client::state::User {
            username: username.clone(),
            room: state.client_state.get_room(),
            file: None,
            file_size: None,
            file_duration: None,
            is_ready: None,
            is_controller: false,
            features: None,
        });

    if let Some(room) = update.room {
        user.room = room.name;
    }

    let mut updated_file = None;
    if let Some(file) = update.file {
        user.file = file.name;
        user.file_size = file.size;
        user.file_duration = file.duration;
        updated_file = Some(());
    }
    if let Some(is_ready) = update.is_ready {
        user.is_ready = Some(is_ready);
    }
    if let Some(controller) = update.controller {
        user.is_controller = controller;
    }
    if let Some(features) = update.features {
        user.features = Some(features);
    }

    let room_changed = old_user
        .as_ref()
        .map(|old| old.room != user.room)
        .unwrap_or(true);
    let file_changed = if updated_file.is_some() {
        !is_same_file(old_user.as_ref(), &user, &config)
    } else {
        false
    };

    if updated_file.is_some() && file_changed {
        if let Some(file_name) = user.file.as_ref() {
            let duration = user.file_duration.unwrap_or(0.0);
            let duration_text = if duration > 0.0 {
                format_time(duration)
            } else {
                "--:--".to_string()
            };
            let mut message = format!(
                "{} is playing '{}' ({})",
                username, file_name, duration_text
            );
            if current_room != user.room || username == current_username {
                message.push_str(&format!(" in room: '{}'", user.room));
            }
            emit_system_message(state, &message);
            let allow_osd = allow_osd_for_user(&config, &current_room, old_user.as_ref(), &user);
            maybe_show_osd(state, &config, &message, allow_osd);

            if username != current_username {
                if let Some(diff) = file_differences(state, &user, &config) {
                    let message = format!("Your file differs in the following way(s): {}", diff);
                    emit_system_message(state, &message);
                }
            }
        }
    } else if room_changed {
        let message = format!("{} has joined the room: '{}'", username, user.room);
        emit_system_message(state, &message);
        let allow_osd = allow_osd_for_user(&config, &current_room, old_user.as_ref(), &user);
        maybe_show_osd(state, &config, &message, allow_osd);
    }

    state.client_state.add_user(user);
    true
}

fn allow_osd_for_user(
    config: &crate::config::SyncplayConfig,
    current_room: &str,
    old_user: Option<&crate::client::state::User>,
    user: &crate::client::state::User,
) -> bool {
    let was_in_room = old_user
        .map(|old| old.room == current_room)
        .unwrap_or(false);
    let is_in_room = user.room == current_room;
    let allow = if was_in_room || is_in_room {
        config.user.show_same_room_osd
    } else {
        config.user.show_different_room_osd
    };

    if !config.user.show_non_controller_osd && !user.is_controller {
        return false;
    }

    allow
}

fn is_same_file(
    old_user: Option<&crate::client::state::User>,
    new_user: &crate::client::state::User,
    config: &crate::config::SyncplayConfig,
) -> bool {
    let Some(old_user) = old_user else {
        return false;
    };
    let same_name = same_filename(old_user.file.as_deref(), new_user.file.as_deref());
    let same_size =
        crate::utils::same_filesize(old_user.file_size.as_ref(), new_user.file_size.as_ref());
    let same_duration = same_duration(
        old_user.file_duration,
        new_user.file_duration,
        config.user.show_duration_notification,
    );
    same_name && same_size && same_duration
}

fn same_duration(a: Option<f64>, b: Option<f64>, allow: bool) -> bool {
    if !allow {
        return true;
    }
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    (a.round() - b.round()).abs() < DIFFERENT_DURATION_THRESHOLD
}

fn file_differences(
    state: &Arc<AppState>,
    user: &crate::client::state::User,
    config: &crate::config::SyncplayConfig,
) -> Option<String> {
    if user.room != state.client_state.get_room() {
        return None;
    }
    let current_media = state.client_state.get_file_info();
    let current_file = current_media.name;
    let current_size = current_media.size;
    let current_duration = current_media.duration;
    let (Some(current_file), Some(other_file)) = (current_file.as_ref(), user.file.as_ref()) else {
        return None;
    };

    let mut differences = Vec::new();
    if !same_filename(Some(current_file), Some(other_file)) {
        differences.push("name");
    }
    if !crate::utils::same_filesize(current_size.as_ref(), user.file_size.as_ref()) {
        differences.push("size");
    }
    if !same_duration(
        current_duration,
        user.file_duration,
        config.user.show_duration_notification,
    ) {
        differences.push("duration");
    }

    if differences.is_empty() {
        None
    } else {
        Some(differences.join(", "))
    }
}

fn is_placeholder_username(username: &str) -> bool {
    username.trim().is_empty()
}

fn apply_list_response(
    state: &Arc<AppState>,
    users_by_room: crate::network::messages::ListResponse,
) -> Vec<String> {
    let mut rooms: Vec<String> = users_by_room.keys().cloned().collect();
    let current_room = state.client_state.get_room();
    if !current_room.is_empty() && !rooms.contains(&current_room) {
        rooms.push(current_room);
    }
    sort_room_names(&mut rooms);

    state.client_state.clear_users();
    for (room_name, room_users) in users_by_room {
        for (username, user_info) in room_users {
            if is_placeholder_username(&username) {
                tracing::debug!(
                    "Ignoring placeholder user entry from List in room '{}'",
                    room_name
                );
                continue;
            }
            let file = user_info.file.as_ref().and_then(|file| file.name.clone());
            let file_size = user_info.file.as_ref().and_then(|file| file.size.clone());
            let file_duration = user_info.file.as_ref().and_then(|file| file.duration);
            state.client_state.add_user(crate::client::state::User {
                username,
                room: room_name.clone(),
                file,
                file_size,
                file_duration,
                is_ready: user_info.is_ready,
                is_controller: user_info.controller.unwrap_or(false),
                features: user_info.features,
            });
        }
    }

    rooms
}

fn emit_user_list(state: &Arc<AppState>) {
    emit_user_list_payload(state, incremental_room_projection(state));
}

fn emit_user_list_with_rooms(state: &Arc<AppState>, rooms: Vec<String>) {
    emit_user_list_payload(state, Some(rooms));
}

fn incremental_room_projection(state: &Arc<AppState>) -> Option<Vec<String>> {
    if state.server_features.lock().persistent_rooms {
        return None;
    }

    let mut rooms = Vec::new();
    for user in state.client_state.get_users() {
        if !user.room.is_empty() && !rooms.contains(&user.room) {
            rooms.push(user.room);
        }
    }
    let current_room = state.client_state.get_room();
    if !current_room.is_empty() && !rooms.contains(&current_room) {
        rooms.push(current_room);
    }
    sort_room_names(&mut rooms);
    Some(rooms)
}

fn sort_room_names(rooms: &mut [String]) {
    rooms.sort_by(|left, right| {
        left.to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left.cmp(right))
    });
}

fn emit_user_list_payload(state: &Arc<AppState>, rooms: Option<Vec<String>>) {
    let users = state.client_state.get_users();
    let users_json: Vec<serde_json::Value> = users
        .into_iter()
        .filter(|u| !is_placeholder_username(&u.username))
        .map(|u| {
            serde_json::json!({
                "username": u.username,
                "room": u.room,
                "file": u.file,
                "fileSize": u.file_size,
                "fileDuration": u.file_duration,
                "isReady": u.is_ready.unwrap_or(false),
                "isController": u.is_controller,
            })
        })
        .collect();
    let mut payload = serde_json::json!({ "users": users_json });
    if let Some(rooms) = rooms {
        payload["rooms"] = serde_json::json!(rooms);
    }
    state.emit_event("user-list-updated", payload);
}

#[tauri::command]
pub async fn disconnect_from_server(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    disconnect_from_server_state(state.inner()).await
}

async fn clear_disconnected_session_state(
    state: &Arc<AppState>,
    tls_status: &str,
) -> Result<(), String> {
    stop_player(state).await?;

    state.client_state.set_file(None);
    playback_runtime::reset(state).await;
    *state.last_rewind_time.lock() = None;
    *state.last_advance_time.lock() = None;
    *state.last_updated_file_time.lock() = None;
    *state.last_paused_on_leave_time.lock() = None;
    clear_terminal_server_session_state(state, tls_status);

    Ok(())
}

fn clear_terminal_server_session_state(state: &Arc<AppState>, tls_status: &str) {
    state.client_state.clear_users();
    state.client_state.set_ready_state(None);
    state.client_state.clear_server_version();
    *state.server_features.lock() = ServerFeatures::default();
    *state.playlist_may_need_restoring.lock() = false;
    *state.last_connect_time.lock() = None;
    {
        let mut autoplay = state.autoplay.lock();
        autoplay.countdown_active = false;
        autoplay.countdown_remaining = 0;
    }
    *state.room_warning_state.lock() = crate::app_state::RoomWarningState::default();
    *state.warning_timers.lock() = WarningTimers::default();
    *state.room_warning_task_running.lock() = false;
    state.emit_event(
        "user-list-updated",
        serde_json::json!({ "users": [], "rooms": [] }),
    );
    state.emit_event(
        "server-features-updated",
        serde_json::json!({ "managedRooms": false, "persistentRooms": false }),
    );
    state.emit_event(
        "connection-status-changed",
        ConnectionStatusEvent {
            connected: false,
            server: None,
        },
    );
    state.emit_event(
        "tls-status-changed",
        serde_json::json!({ "status": tls_status }),
    );
}

pub(crate) async fn disconnect_from_server_state(state: &Arc<AppState>) -> Result<(), String> {
    tracing::info!("connection_lifecycle: manual disconnect requested");

    invalidate_current_connection_session(state);
    disable_reconnect(state);
    *state.manual_disconnect.lock() = true;

    if let Some(connection) = state.connection.lock().take() {
        connection.disconnect();
    }

    let result = clear_disconnected_session_state(state, "unknown").await;
    *state.manual_disconnect.lock() = false;
    result
}

#[tauri::command]
pub async fn get_connection_status(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.is_connected())
}

#[cfg(test)]
mod protocol_message_tests {
    use crate::network::messages::*;

    #[test]
    fn hello_set_state_tls_list_and_error_roundtrip_examples() {
        let hello_json = r#"{"Hello":{"username":"alice","password":"secret","room":{"name":"room","password":"roompw"},"version":"1.2.255","realversion":"1.7.5","features":{"sharedPlaylists":true,"chat":true,"readiness":true,"managedRooms":true,"persistentRooms":false,"featureList":true,"setOthersReadiness":true,"uiMode":"GUI"},"motd":"welcome"}}"#;
        let tls_json = r#"{"TLS":{"startTLS":"accepted"}}"#;
        let state_json = r#"{"State":{"playstate":{"position":12.5,"paused":false,"doSeek":true,"setBy":"alice"},"ping":{"latencyCalculation":0.25,"clientLatencyCalculation":1.5,"clientRtt":0.12,"serverRtt":0.08},"ignoringOnTheFly":{"server":1,"client":2}}}"#;
        let ready_json = r#"{"Set":{"ready":{"username":"pc","isReady":true,"manuallyInitiated":false,"setBy":"host"}}}"#;
        let user_json = r#"{"Set":{"user":{"pc":{"room":{"name":"default"},"file":{"name":"movie.mkv","size":12345,"duration":3600},"controller":true,"isReady":false,"features":{"managedRooms":true}}}}}"#;
        let playlist_json = r#"{"Set":{"playlistChange":{"user":"host","files":["movie.mkv","bonus.mkv"]},"playlistIndex":{"user":"host","index":1}}}"#;
        let list_json = r#"{"List":{"default":{"ghost":{"file":{"name":"movie.mkv","size":"12345","duration":3600},"controller":false,"isReady":true,"features":[]}}}}"#;
        let error_json = r#"{"Error":{"message":"This server does not support TLS"}}"#;

        let hello: ProtocolMessage = serde_json::from_str(hello_json).unwrap();
        let tls: ProtocolMessage = serde_json::from_str(tls_json).unwrap();
        let state: ProtocolMessage = serde_json::from_str(state_json).unwrap();
        let ready: ProtocolMessage = serde_json::from_str(ready_json).unwrap();
        let user: ProtocolMessage = serde_json::from_str(user_json).unwrap();
        let playlist: ProtocolMessage = serde_json::from_str(playlist_json).unwrap();
        let list: ProtocolMessage = serde_json::from_str(list_json).unwrap();
        let error: ProtocolMessage = serde_json::from_str(error_json).unwrap();

        let roundtrip = serde_json::to_value(&hello).unwrap();
        assert_eq!(
            roundtrip,
            serde_json::from_str::<serde_json::Value>(hello_json).unwrap()
        );
        assert!(matches!(tls, ProtocolMessage::TLS { .. }));
        assert!(matches!(state, ProtocolMessage::State { .. }));
        assert!(matches!(ready, ProtocolMessage::Set { .. }));
        assert!(matches!(user, ProtocolMessage::Set { .. }));
        assert!(matches!(playlist, ProtocolMessage::Set { .. }));
        assert!(matches!(list, ProtocolMessage::List { .. }));
        assert!(matches!(error, ProtocolMessage::Error { .. }));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::client::playback::CommittedMedia;
    use crate::network::fake_server::FakeSyncplayServer;
    use crate::network::messages::{ChatMessage, PingInfo, PlayState, StateMessage};
    use crate::player::backend::{
        FakePlayerBackend, FakePlayerCommand, FakePlayerFactory, PlayerKind,
    };
    use crate::player::properties::PlayerState;
    use std::sync::Arc;
    use tokio::time::{sleep, timeout, Duration};

    #[tokio::test]
    async fn remote_seek_uses_one_player_command_without_a_follower_seek() {
        let state = AppState::new();
        state.client_state.set_username("alice".to_string());
        state
            .client_state
            .set_global_state(120.0, false, Some("bob".to_string()));
        *state.last_global_update.lock() = Some(std::time::Instant::now());

        let player = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        player.set_fake_state(PlayerState {
            position: Some(120.0),
            paused: Some(false),
            ..PlayerState::default()
        });
        let snapshot = player.get_state();
        state.local_playback_state.lock().update_from_player(
            120.0,
            false,
            snapshot.observed_position,
            snapshot.observed_paused,
            snapshot.position_observation_generation,
            snapshot.paused_observation_generation,
            120.0,
            false,
        );
        *state.player.lock() = Some(player.clone());

        handle_state_update(
            &state,
            PlayState {
                position: 80.0,
                paused: false,
                do_seek: Some(true),
                set_by: Some("bob".to_string()),
            },
            0.0,
        )
        .await;

        assert_eq!(
            player
                .commands()
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::SetPosition(80.0)))
                .count(),
            1
        );
        let response = build_local_playstate(&state).expect("missing follower playstate");
        assert!((response.position - 80.0).abs() < 0.1);
        assert_eq!(response.do_seek, None);

        for position in [120.0, 100.0, 80.0] {
            player.set_fake_state(PlayerState {
                position: Some(position),
                paused: Some(false),
                ..PlayerState::default()
            });
            let snapshot = player.get_state();
            let changes = state.local_playback_state.lock().update_from_player(
                position,
                false,
                snapshot.observed_position,
                snapshot.observed_paused,
                snapshot.position_observation_generation,
                snapshot.paused_observation_generation,
                80.0,
                false,
            );
            assert_eq!(changes, (false, false));
        }

        player.set_fake_state(PlayerState {
            position: Some(60.0),
            paused: Some(false),
            ..PlayerState::default()
        });
        let snapshot = player.get_state();
        assert_eq!(
            state.local_playback_state.lock().update_from_player(
                60.0,
                false,
                snapshot.observed_position,
                snapshot.observed_paused,
                snapshot.position_observation_generation,
                snapshot.paused_observation_generation,
                80.0,
                false,
            ),
            (false, true)
        );
    }

    #[tokio::test]
    async fn first_seek_state_does_not_duplicate_init_corrections() {
        let state = AppState::new();
        state.client_state.set_username("alice".to_string());
        state.client_state.set_file(Some("movie.mkv".to_string()));

        let player = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        player.set_confirm_commands(true);
        player.set_fake_state(PlayerState {
            position: Some(120.0),
            paused: Some(true),
            ..PlayerState::default()
        });
        let snapshot = player.get_state();
        *state.player.lock() = Some(player.clone());
        state.local_playback_state.lock().update_from_player(
            120.0,
            true,
            snapshot.observed_position,
            snapshot.observed_paused,
            snapshot.position_observation_generation,
            snapshot.paused_observation_generation,
            120.0,
            true,
        );

        handle_state_update(
            &state,
            PlayState {
                position: 80.0,
                paused: false,
                do_seek: Some(true),
                set_by: Some("bob".to_string()),
            },
            0.0,
        )
        .await;

        let commands = player.commands();
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::SetPosition(80.0)))
                .count(),
            1
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::SetPaused(false)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn stale_raw_position_does_not_skip_a_backward_seek() {
        let state = AppState::new();
        let player = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        player.set_fake_state(PlayerState {
            position: Some(80.0),
            paused: Some(false),
            ..PlayerState::default()
        });
        player.set_position(83.0).await.unwrap();
        let backend: Arc<dyn PlayerBackend> = player.clone();
        *state.player.lock() = Some(backend.clone());

        assert!(try_set_position(&state, &backend, 80.0, "test").await);
        assert_eq!(
            player
                .commands()
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::SetPosition(80.0)))
                .count(),
            1
        );
    }

    #[test]
    fn stale_player_snapshot_cannot_cross_a_player_replacement() {
        let state = AppState::new();
        let old_player = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        for position in 1..=5 {
            old_player.set_fake_state(PlayerState {
                position: Some(position as f64),
                paused: Some(false),
                ..PlayerState::default()
            });
        }
        let old_backend: Arc<dyn PlayerBackend> = old_player.clone();
        *state.player.lock() = Some(old_backend.clone());

        let new_player = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        new_player.set_fake_state(PlayerState {
            position: Some(1.0),
            paused: Some(true),
            ..PlayerState::default()
        });
        let new_backend: Arc<dyn PlayerBackend> = new_player.clone();
        {
            let mut player_slot = state.player.lock();
            state.local_playback_state.lock().reset_player_observation();
            *player_slot = Some(new_backend.clone());
        }

        let stale_snapshot = old_player.get_state();
        assert!(
            with_current_player_local_state(&state, &old_backend, |local_state| {
                local_state.update_from_player(
                    5.0,
                    false,
                    stale_snapshot.observed_position,
                    stale_snapshot.observed_paused,
                    stale_snapshot.position_observation_generation,
                    stale_snapshot.paused_observation_generation,
                    5.0,
                    false,
                );
            })
            .is_none()
        );

        let new_snapshot = new_player.get_state();
        assert_eq!(
            with_current_player_local_state(&state, &new_backend, |local_state| {
                local_state.update_from_player(
                    1.0,
                    true,
                    new_snapshot.observed_position,
                    new_snapshot.observed_paused,
                    new_snapshot.position_observation_generation,
                    new_snapshot.paused_observation_generation,
                    1.0,
                    true,
                )
            }),
            Some((false, false))
        );
        assert_eq!(
            state.local_playback_state.lock().current(),
            Some((1.0, true))
        );
    }

    #[tokio::test]
    async fn rejected_remote_unpause_stays_an_ack_instead_of_a_local_pause() {
        let state = AppState::new();
        state.client_state.set_username("alice".to_string());
        state
            .client_state
            .set_global_state(120.0, true, Some("bob".to_string()));
        *state.last_global_update.lock() = Some(std::time::Instant::now());

        let player = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        player.set_fake_state(PlayerState {
            position: Some(120.0),
            paused: Some(true),
            ..PlayerState::default()
        });
        let snapshot = player.get_state();
        state.local_playback_state.lock().update_from_player(
            120.0,
            true,
            snapshot.observed_position,
            snapshot.observed_paused,
            snapshot.position_observation_generation,
            snapshot.paused_observation_generation,
            120.0,
            true,
        );
        *state.player.lock() = Some(player.clone());

        let unpause = PlayState {
            position: 120.0,
            paused: false,
            do_seek: None,
            set_by: Some("bob".to_string()),
        };
        handle_state_update(&state, unpause.clone(), 0.0).await;

        player.set_fake_state(PlayerState {
            position: Some(120.0),
            paused: Some(true),
            ..PlayerState::default()
        });
        let snapshot = player.get_state();
        assert_eq!(
            state.local_playback_state.lock().update_from_player(
                120.0,
                true,
                snapshot.observed_position,
                snapshot.observed_paused,
                snapshot.position_observation_generation,
                snapshot.paused_observation_generation,
                120.0,
                false,
            ),
            (false, false)
        );

        handle_state_update(&state, unpause, 0.0).await;

        assert_eq!(
            player
                .commands()
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::SetPaused(false)))
                .count(),
            1
        );
        let response = build_local_playstate(&state).expect("missing follower playstate");
        assert!(!response.paused);
        assert_eq!(response.do_seek, None);
        assert_eq!(
            state
                .chat
                .get_messages()
                .iter()
                .filter(|message| message.message == "bob unpaused")
                .count(),
            1
        );
    }

    #[test]
    fn list_projection_keeps_empty_persistent_room_without_fake_user() {
        let state = AppState::new();
        state.client_state.set_room("current".to_string());
        let message: ProtocolMessage = serde_json::from_str(
            r#"{"List":{"Zoo":{"alice":{"file":{},"controller":false,"isReady":true,"features":[]}},"empty":{" ":{"position":0,"file":{},"controller":false,"isReady":true,"features":[]}}}}"#,
        )
        .unwrap();
        let ProtocolMessage::List { List: Some(list) } = message else {
            panic!("expected List response");
        };

        let rooms = apply_list_response(&state, list);

        assert_eq!(rooms, vec!["current", "empty", "Zoo"]);
        assert_eq!(state.client_state.get_users().len(), 1);
        assert!(state.client_state.get_user("alice").is_some());
        assert!(state.client_state.get_user(" ").is_none());
    }

    #[test]
    fn incremental_room_projection_tracks_live_rooms_on_nonpersistent_servers() {
        let state = AppState::new();
        state.client_state.set_room("current".to_string());
        for (username, room) in [("alice", "Zoo"), ("bob", "alpha"), ("carol", "Zoo")] {
            state.client_state.add_user(crate::client::state::User {
                username: username.to_string(),
                room: room.to_string(),
                file: None,
                file_size: None,
                file_duration: None,
                is_ready: None,
                is_controller: false,
                features: None,
            });
        }

        assert_eq!(
            incremental_room_projection(&state),
            Some(vec![
                "alpha".to_string(),
                "current".to_string(),
                "Zoo".to_string()
            ])
        );

        state.server_features.lock().persistent_rooms = true;
        assert_eq!(incremental_room_projection(&state), None);
    }

    #[test]
    fn controller_auth_sender_uses_reference_payload() {
        assert_eq!(
            serde_json::to_value(controller_auth_request("room", "AB-123-456")).unwrap(),
            serde_json::json!({
                "Set": {
                    "controllerAuth": {
                        "room": "room",
                        "password": "AB-123-456"
                    }
                }
            })
        );
    }

    #[test]
    fn managed_room_capability_uses_version_then_server_feature_override() {
        let state = AppState::new();

        update_server_features(&state, "1.2.255", None);
        assert!(!state.server_features.lock().managed_rooms);

        update_server_features(
            &state,
            "1.7.5",
            Some(serde_json::json!({ "managedRooms": false })),
        );
        assert!(!state.server_features.lock().managed_rooms);

        update_server_features(
            &state,
            "1.7.5",
            Some(serde_json::json!({ "managedRooms": true })),
        );
        assert!(state.server_features.lock().managed_rooms);
    }

    #[tokio::test]
    async fn reconnect_clears_stale_global_playback_state() {
        let state = AppState::new();
        state
            .client_state
            .set_global_state(73.0, false, Some("old-peer".to_string()));
        *state.last_global_update.lock() = Some(std::time::Instant::now());

        reset_transient_connection_state(&state).await;

        let global = state.client_state.get_global_state();
        assert_eq!(global.position, 0.0);
        assert!(global.paused);
        assert!(global.set_by.is_none());
        assert!(state.last_global_update.lock().is_none());
    }

    #[tokio::test]
    async fn protocol_timeout_uses_last_global_state_not_other_messages() {
        let state = AppState::new();
        *state.last_global_update.lock() = Some(
            std::time::Instant::now() - Duration::from_secs_f64(PROTOCOL_TIMEOUT_SECONDS + 1.0),
        );
        *state.last_protocol_activity.lock() = Some(std::time::Instant::now());

        assert!(check_protocol_timeout(&state));
        assert!(state.last_global_update.lock().is_none());

        *state.last_protocol_activity.lock() = Some(
            std::time::Instant::now() - Duration::from_secs_f64(PROTOCOL_TIMEOUT_SECONDS + 1.0),
        );
        assert!(!check_protocol_timeout(&state));
    }

    async fn expect_client_tls_or_hello(server: &mut FakeSyncplayServer) -> HelloMessage {
        match timeout(Duration::from_secs(3), server.next_received())
            .await
            .unwrap()
            .unwrap()
        {
            ProtocolMessage::TLS { .. } => {
                server
                    .send(FakeSyncplayServer::tls_response("unsupported"))
                    .unwrap();
                let message = timeout(Duration::from_secs(2), server.next_received())
                    .await
                    .unwrap()
                    .unwrap();
                let ProtocolMessage::Hello { Hello } = message else {
                    panic!("expected Hello after TLS fallback, got {message:?}");
                };
                Hello
            }
            ProtocolMessage::Hello { Hello } => Hello,
            other => panic!("expected TLS request or Hello, got {other:?}"),
        }
    }

    async fn wait_for_fake_player_launch(
        state: &Arc<AppState>,
        factory: &Arc<FakePlayerFactory>,
    ) -> FakePlayerBackend {
        timeout(Duration::from_secs(2), async {
            loop {
                if let Some(player) = factory.players().last().cloned() {
                    if state.is_player_connected() {
                        break player;
                    }
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake player startup timed out")
    }

    #[test]
    fn explicit_player_startup_delay_matches_original_scheduler_offset() {
        assert_eq!(PLAYER_STARTUP_DELAY, Duration::from_millis(100));
    }

    #[tokio::test]
    async fn client_hello_preserves_complete_username_and_normalized_room() {
        let state = AppState::new();
        *state.fake_player_factory.lock() = Some(Arc::new(FakePlayerFactory::default()));
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let username = "用户名".repeat(10);
        let room = format!("+{}:123456789ABC", "房间".repeat(20));
        let room_with_password = format!("{room}:AB-123-456");

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            username.clone(),
            room_with_password,
            None,
            None,
            &state,
        )
        .await
        .unwrap();

        let hello = expect_client_tls_or_hello(&mut server).await;
        assert_eq!(hello.username, username);
        assert_eq!(
            hello.room.as_ref().map(|room| room.name.as_str()),
            Some(room.as_str())
        );
        assert_eq!(
            state
                .reconnect_snapshot
                .lock()
                .as_ref()
                .map(|snapshot| snapshot.room.as_str()),
            Some(room.as_str())
        );
        assert_eq!(
            state
                .controlled_room_passwords
                .lock()
                .get(&room)
                .map(String::as_str),
            Some("AB-123-456")
        );

        disconnect_from_server_state(&state).await.unwrap();
        server.close();
    }

    #[tokio::test]
    async fn explicit_connect_starts_player_after_original_delay_without_server_hello() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let started = std::time::Instant::now();

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();

        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        let player = wait_for_fake_player_launch(&state, &factory).await;
        assert!(started.elapsed() >= PLAYER_STARTUP_DELAY);
        assert_eq!(
            state
                .connection
                .lock()
                .as_ref()
                .map(|connection| connection.state()),
            Some(crate::network::connection::ConnectionState::Connected)
        );
        assert!(!state.is_connected());
        assert!(!player.commands().contains(&FakePlayerCommand::SetFeatures));

        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                if player.commands().contains(&FakePlayerCommand::SetFeatures) {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("player-ready-before-Hello feature sync timed out");
        assert!(state.is_connected());

        disconnect_from_server_state(&state).await.unwrap();
        server.close();
    }

    #[tokio::test]
    async fn hello_before_player_ready_defers_feature_sync_until_startup_finishes() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let startup_blocker = state.player_lifecycle.lock().await;

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();

        timeout(Duration::from_secs(2), async {
            loop {
                if state.client_state.get_server_version().is_some() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(factory.launch_count(), 0);

        drop(startup_blocker);
        let player = wait_for_fake_player_launch(&state, &factory).await;
        timeout(Duration::from_secs(2), async {
            loop {
                if player.commands().contains(&FakePlayerCommand::SetFeatures) {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Hello-before-player-ready feature sync timed out");

        disconnect_from_server_state(&state).await.unwrap();
        server.close();
    }

    #[tokio::test]
    async fn player_startup_failure_closes_only_the_current_connection_session() {
        let state = AppState::new();
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let startup_blocker = state.player_lifecycle.lock().await;

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));

        drop(startup_blocker);
        timeout(Duration::from_secs(2), async {
            loop {
                if state.connection.lock().is_none()
                    && !state.reconnect_state.lock().enabled
                    && !*state.player_connecting.lock()
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("failed player startup did not close its server session");
        assert!(state.player.lock().is_none());
        assert!(state.player_process.lock().is_none());

        server.close();
    }

    #[tokio::test]
    async fn manual_disconnect_cancels_delayed_startup_without_reinstalling_player() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let startup_blocker = state.player_lifecycle.lock().await;

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));

        let disconnect_state = state.clone();
        let disconnect =
            tokio::spawn(async move { disconnect_from_server_state(&disconnect_state).await });
        timeout(Duration::from_secs(2), async {
            loop {
                if state.connection.lock().is_none() {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        drop(startup_blocker);
        disconnect.await.unwrap().unwrap();
        sleep(PLAYER_STARTUP_DELAY + Duration::from_millis(50)).await;

        assert_eq!(factory.launch_count(), 0);
        assert!(state.player.lock().is_none());
        assert!(!*state.player_connecting.lock());
        server.close();
    }

    #[tokio::test]
    async fn initial_network_failure_cancels_delayed_player_startup() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());
        *state.client_supports_tls.lock() = false;

        let result = connect_to_server_state::<tauri::test::MockRuntime>(
            "127.0.0.1".to_string(),
            port,
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await;

        assert!(result.is_err());
        sleep(PLAYER_STARTUP_DELAY + Duration::from_millis(50)).await;
        assert!(state.connection.lock().is_none());
        assert!(state.player.lock().is_none());
        assert!(state.player_process.lock().is_none());
        assert!(!state.reconnect_state.lock().enabled);
        assert!(factory
            .players()
            .iter()
            .all(|player| player.shutdown_count() == 1));
    }

    #[tokio::test]
    async fn early_transport_loss_keeps_delayed_startup_in_the_logical_session() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        let session_generation = current_connection_session(&state);

        server.abort_connection();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.wait_for_reconnect_message())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();

        let player = wait_for_fake_player_launch(&state, &factory).await;
        timeout(Duration::from_secs(2), async {
            loop {
                if state.is_connected() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reconnected logical session did not authenticate");
        assert_eq!(current_connection_session(&state), session_generation);
        assert_eq!(factory.launch_count(), 1);
        assert_eq!(player.shutdown_count(), 0);

        disconnect_from_server_state(&state).await.unwrap();
        server.close();
    }

    #[tokio::test]
    async fn terminal_close_invalidates_delayed_startup_before_player_installation() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let startup_blocker = state.player_lifecycle.lock().await;

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        let session_generation = current_connection_session(&state);
        server
            .send(ProtocolMessage::Error {
                Error: crate::network::messages::ErrorMessage {
                    message: "terminal before player startup".to_string(),
                },
            })
            .unwrap();

        timeout(Duration::from_secs(2), async {
            loop {
                if state.connection.lock().is_none()
                    && !state.reconnect_state.lock().enabled
                    && !is_current_connection_generation(&state, session_generation)
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("terminal close did not invalidate logical startup session");
        drop(startup_blocker);
        sleep(PLAYER_STARTUP_DELAY + Duration::from_millis(50)).await;
        assert_eq!(factory.launch_count(), 0);
        assert!(state.player.lock().is_none());

        server.close();
    }

    #[tokio::test]
    async fn stale_music_task_cannot_restart_player_after_session_invalidation() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());
        state.client_state.set_file(Some("track.mp3".to_string()));
        *state.last_advance_time.lock() = Some(std::time::Instant::now());

        maybe_unpause_for_music(&state);
        invalidate_current_connection_session(&state);
        state.player_startup_epoch.fetch_add(1, Ordering::AcqRel);

        sleep(Duration::from_millis(100)).await;
        assert_eq!(factory.launch_count(), 0);
        assert!(state.player.lock().is_none());
    }

    #[tokio::test]
    async fn stale_startup_failure_cannot_clear_a_new_logical_session() {
        let state = AppState::new();
        let old_connection = Arc::new(Connection::new());
        assert!(claim_connection_session(&state, old_connection.clone()));
        let old_generation = begin_connection_session(&state);
        assert!(invalidate_connection_session(&state, old_generation));
        assert!(take_current_connection_session(&state, &old_connection).is_some());

        let new_connection = Arc::new(Connection::new());
        assert!(claim_connection_session(&state, new_connection.clone()));
        let new_generation = begin_connection_session(&state);
        state.reconnect_state.lock().enabled = true;
        let player = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        *state.player.lock() = Some(player.clone());

        terminate_initial_session(&state, old_generation, Some("stale startup failure")).await;

        assert_eq!(current_connection_session(&state), new_generation);
        assert!(state
            .connection
            .lock()
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &new_connection)));
        assert!(state.reconnect_state.lock().enabled);
        assert!(state.player.lock().is_some());
        assert_eq!(player.shutdown_count(), 0);
    }

    #[tokio::test]
    async fn terminal_protocol_loss_and_transport_loss_preserve_the_player() {
        let terminal_state = AppState::new();
        let terminal_factory = Arc::new(FakePlayerFactory::default());
        *terminal_state.fake_player_factory.lock() = Some(terminal_factory.clone());
        *terminal_state.client_supports_tls.lock() = false;
        let mut terminal_server = FakeSyncplayServer::start().await.unwrap();
        connect_to_server_state::<tauri::test::MockRuntime>(
            terminal_server.host(),
            terminal_server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &terminal_state,
        )
        .await
        .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), terminal_server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        let terminal_player = wait_for_fake_player_launch(&terminal_state, &terminal_factory).await;
        terminal_state
            .client_state
            .set_file(Some("movie.mkv".to_string()));
        terminal_state.client_state.set_ready(true);
        terminal_server
            .send(ProtocolMessage::Error {
                Error: crate::network::messages::ErrorMessage {
                    message: "terminal server error".to_string(),
                },
            })
            .unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                if terminal_state.connection.lock().is_none()
                    && !terminal_state.reconnect_state.lock().enabled
                    && terminal_state.client_state.ready_state().is_none()
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("terminal closure did not detach the server session");
        assert!(terminal_state.player.lock().is_some());
        assert_eq!(terminal_player.shutdown_count(), 0);
        assert_eq!(
            terminal_state.client_state.get_file().as_deref(),
            Some("movie.mkv")
        );
        stop_player(&terminal_state).await.unwrap();
        terminal_server.close();

        let transport_state = AppState::new();
        let transport_factory = Arc::new(FakePlayerFactory::default());
        *transport_state.fake_player_factory.lock() = Some(transport_factory.clone());
        *transport_state.client_supports_tls.lock() = false;
        let mut transport_server = FakeSyncplayServer::start().await.unwrap();
        connect_to_server_state::<tauri::test::MockRuntime>(
            transport_server.host(),
            transport_server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &transport_state,
        )
        .await
        .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), transport_server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        let transport_player =
            wait_for_fake_player_launch(&transport_state, &transport_factory).await;
        transport_server.abort_connection();
        timeout(Duration::from_secs(2), async {
            loop {
                if transport_state.reconnect_state.lock().running {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("transport closure did not enter reconnect lifecycle");
        assert!(transport_state.player.lock().is_some());
        assert_eq!(transport_player.shutdown_count(), 0);

        disconnect_from_server_state(&transport_state)
            .await
            .unwrap();
        transport_server.close();
    }

    #[tokio::test]
    async fn reconnect_exhaustion_stops_the_owned_player() {
        let state = AppState::new();
        let player = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        *state.player.lock() = Some(player.clone());
        state.reconnect_state.lock().enabled = true;
        state.reconnect_state.lock().running = true;

        let session_generation = current_connection_session(&state);
        finish_reconnect_exhaustion(&state, session_generation).await;

        assert!(!state.reconnect_state.lock().enabled);
        assert!(!state.reconnect_state.lock().running);
        assert!(state.player.lock().is_none());
        assert_eq!(player.shutdown_count(), 1);
    }

    #[test]
    fn hello_password_digest_hashes_nonempty_plaintext_only() {
        assert_eq!(server_password_digest(None), None);
        assert_eq!(server_password_digest(Some("")), None);
        assert_eq!(
            server_password_digest(Some("secret")).as_deref(),
            Some("5ebe2294ecd0e0f08eab7690d2a6ee69")
        );
    }

    #[test]
    fn server_hello_requires_username_room_and_effective_version() {
        let mut hello = FakeSyncplayServer::hello_response("alice", "room");
        let ProtocolMessage::Hello {
            Hello: ref mut hello_payload,
        } = hello
        else {
            unreachable!();
        };
        assert!(validate_server_hello(hello_payload).is_ok());

        hello_payload.username.clear();
        assert!(validate_server_hello(hello_payload).is_err());
        hello_payload.username = "alice".to_string();
        hello_payload.room = None;
        assert!(validate_server_hello(hello_payload).is_err());
        hello_payload.room = Some(RoomInfo {
            name: "room".to_string(),
            password: None,
        });
        hello_payload.version.clear();
        hello_payload.realversion.clear();
        assert!(validate_server_hello(hello_payload).is_err());
    }

    #[test]
    fn disabled_player_chat_output_still_produces_frontend_payload() {
        let state = AppState::new();
        state.config.lock().user.chat_output_enabled = false;
        let player = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        *state.player.lock() = Some(player.clone());

        let payload = route_incoming_chat(
            &state,
            ChatMessage::Entry {
                username: "alice".to_string(),
                message: "hello".to_string(),
            },
        )
        .expect("frontend chat payload must not depend on player OSD output");

        assert_eq!(payload["username"], "alice");
        assert_eq!(payload["message"], "hello");
        assert_eq!(payload["messageType"], "normal");
        assert!(!player
            .commands()
            .iter()
            .any(|command| matches!(command, FakePlayerCommand::ShowChatMessage(_, _))));
    }

    async fn send_server_state_and_expect_client_state(
        server: &mut FakeSyncplayServer,
        position: f64,
    ) -> StateMessage {
        server
            .send(ProtocolMessage::State {
                State: StateMessage {
                    playstate: Some(PlayState {
                        position,
                        paused: position == 0.0,
                        do_seek: None,
                        set_by: Some("server".to_string()),
                    }),
                    ping: Some(PingInfo {
                        latency_calculation: Some(200.0 + position),
                        client_latency_calculation: Some(
                            crate::network::ping::PingService::new_timestamp(),
                        ),
                        client_rtt: Some(0.1),
                        server_rtt: Some(0.02),
                    }),
                    ignoring_on_the_fly: None,
                },
            })
            .unwrap();

        timeout(Duration::from_secs(2), async {
            loop {
                if let Some(ProtocolMessage::State { State }) = server.next_received().await {
                    break State;
                }
            }
        })
        .await
        .unwrap()
    }

    async fn connect_fake_server_and_player(
        state: &Arc<AppState>,
        factory: &Arc<FakePlayerFactory>,
    ) -> FakeSyncplayServer {
        *state.fake_player_factory.lock() = Some(factory.clone());
        let expected_launch_count = factory.launch_count() + 1;
        let server = FakeSyncplayServer::start().await.unwrap();
        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            state,
        )
        .await
        .unwrap();

        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();

        timeout(Duration::from_secs(2), async {
            loop {
                if factory.launch_count() >= expected_launch_count && state.is_player_connected() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        server
    }

    #[tokio::test]
    async fn disconnect_stops_fake_player_and_clears_backend_state() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        let _server = connect_fake_server_and_player(&state, &factory).await;

        let first_player = factory.players().first().cloned().unwrap();
        *state.mpv_socket_path.lock() = Some("stale-ipc".to_string());
        disconnect_from_server_state(&state).await.unwrap();

        assert!(!state.is_connected());
        assert!(state.connection.lock().is_none());
        assert!(state.player.lock().is_none());
        assert!(state.player_process.lock().is_none());
        assert!(state.mpv_socket_path.lock().is_none());
        assert!(state.mpv_runtime_dir.lock().is_none());
        assert_eq!(first_player.shutdown_count(), 1);
        assert!(first_player
            .commands()
            .contains(&FakePlayerCommand::Shutdown));
    }

    #[tokio::test]
    async fn reconnect_after_disconnect_launches_fresh_fake_player() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        let server1 = connect_fake_server_and_player(&state, &factory).await;
        disconnect_from_server_state(&state).await.unwrap();
        server1.close();

        let _server = connect_fake_server_and_player(&state, &factory).await;
        let players = factory.players();
        assert_eq!(factory.launch_count(), 2);
        assert_eq!(players.len(), 2);
        assert_eq!(players[0].shutdown_count(), 1);
        assert_eq!(players[1].shutdown_count(), 0);
        assert_ne!(players[0].commands(), players[1].commands());
        assert!(state.player.lock().is_some());
    }

    #[tokio::test]
    async fn tls_runtime_branches_send_hello_or_drop_cleanly() {
        for (answer, expect_hello) in [
            ("unsupported", true),
            ("rejected", true),
            ("certificate-invalid", false),
            ("closed", false),
        ] {
            let state = AppState::new();
            *state.fake_player_factory.lock() = Some(Arc::new(FakePlayerFactory::default()));
            let mut server = FakeSyncplayServer::start().await.unwrap();
            connect_to_server_state::<tauri::test::MockRuntime>(
                server.host(),
                server.port(),
                "alice".to_string(),
                "room".to_string(),
                None,
                None,
                &state,
            )
            .await
            .unwrap();

            assert!(matches!(
                timeout(Duration::from_secs(2), server.next_received())
                    .await
                    .unwrap()
                    .unwrap(),
                ProtocolMessage::TLS { .. }
            ));
            server
                .send(FakeSyncplayServer::tls_response(answer))
                .unwrap();

            if expect_hello {
                assert!(matches!(
                    timeout(Duration::from_secs(2), server.next_received())
                        .await
                        .unwrap()
                        .unwrap(),
                    ProtocolMessage::Hello { .. }
                ));
                assert!(*state.hello_sent.lock(), "{answer} should send Hello");
            } else {
                timeout(Duration::from_secs(2), async {
                    loop {
                        let disconnected = state
                            .connection
                            .lock()
                            .as_ref()
                            .map(|connection| !connection.is_connected())
                            .unwrap_or(true);
                        if disconnected {
                            break;
                        }
                        sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                assert!(!*state.hello_sent.lock(), "{answer} must not send Hello");
                if answer == "certificate-invalid" {
                    timeout(Duration::from_secs(2), async {
                        loop {
                            if !state.reconnect_state.lock().enabled {
                                break;
                            }
                            sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    assert!(!state.reconnect_state.lock().running);
                }
            }
            server.close();
        }
    }

    #[tokio::test]
    async fn client_unsupported_tls_sends_plain_hello_without_tls_request() {
        let state = AppState::new();
        *state.fake_player_factory.lock() = Some(Arc::new(FakePlayerFactory::default()));
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();
        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();

        let first = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(first, ProtocolMessage::Hello { .. }));
        assert!(!*state.client_supports_tls.lock());
        assert!(*state.hello_sent.lock());
        server.close();
    }

    #[tokio::test]
    async fn hello_hashes_plaintext_password_consistently_across_reconnect() {
        let state = AppState::new();
        *state.fake_player_factory.lock() = Some(Arc::new(FakePlayerFactory::default()));
        *state.client_supports_tls.lock() = false;
        state.config.lock().server.password = Some("secret".to_string());
        let password = state.config.lock().server.password.clone();
        let mut server = FakeSyncplayServer::start().await.unwrap();

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            password,
            None,
            &state,
        )
        .await
        .unwrap();

        let first_hello = expect_client_tls_or_hello(&mut server).await;
        assert_eq!(
            first_hello.password.as_deref(),
            Some("5ebe2294ecd0e0f08eab7690d2a6ee69")
        );
        assert_eq!(
            state.config.lock().server.password.as_deref(),
            Some("secret")
        );
        assert_eq!(
            state
                .reconnect_snapshot
                .lock()
                .as_ref()
                .and_then(|snapshot| snapshot.password.as_deref()),
            Some("secret")
        );

        server.abort_connection();
        let reconnect_hello = expect_client_tls_or_hello(&mut server).await;
        assert_eq!(reconnect_hello.password, first_hello.password);
        assert_eq!(
            state
                .reconnect_snapshot
                .lock()
                .as_ref()
                .and_then(|snapshot| snapshot.password.as_deref()),
            Some("secret")
        );

        disconnect_from_server_state(&state).await.unwrap();
        server.close();
    }

    #[tokio::test]
    async fn reconnect_hello_uses_latest_authoritative_room() {
        let state = AppState::new();
        *state.fake_player_factory.lock() = Some(Arc::new(FakePlayerFactory::default()));
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "first-room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();

        let initial_hello = expect_client_tls_or_hello(&mut server).await;
        assert_eq!(
            initial_hello.room.as_ref().map(|room| room.name.as_str()),
            Some("first-room")
        );

        server
            .send_raw_line(r#"{"Set":{"room":{"name":"second-room"}}}"#)
            .unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                let current_room = state.client_state.get_room();
                let reconnect_room = state
                    .reconnect_snapshot
                    .lock()
                    .as_ref()
                    .map(|snapshot| snapshot.room.clone());
                if current_room == "second-room" && reconnect_room.as_deref() == Some("second-room")
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        server.abort_connection();
        let reconnect_hello = expect_client_tls_or_hello(&mut server).await;
        assert_eq!(
            reconnect_hello.room.as_ref().map(|room| room.name.as_str()),
            Some("second-room")
        );

        disconnect_from_server_state(&state).await.unwrap();
        server.close();
    }

    #[tokio::test]
    async fn login_sends_ready_before_controller_auth_and_confirmed_file() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory);
        state.config.lock().user.ready_at_start = true;
        state.playback.state.lock().confirmed_media =
            Some(CommittedMedia::new("movie.mkv", Some(42), Some(120.0)));
        let controlled_room = "+controlled:123456789012";
        let mut server = FakeSyncplayServer::start().await.unwrap();

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            format!("{controlled_room}:AB-123-456"),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        expect_client_tls_or_hello(&mut server).await;
        server
            .send(FakeSyncplayServer::hello_response("alice", controlled_room))
            .unwrap();

        let first = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        let ProtocolMessage::Set { Set: first } = first else {
            panic!("expected Set.ready first, got {first:?}");
        };
        let ready = first.ready.expect("first login Set must contain ready");
        assert_eq!(ready.is_ready, Some(true));
        assert_eq!(ready.manually_initiated, Some(false));
        assert!(first.controller_auth.is_none());
        assert!(first.file.is_none());

        let second = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        let ProtocolMessage::Set { Set: second } = second else {
            panic!("expected Set.controllerAuth second, got {second:?}");
        };
        let controller_auth = second
            .controller_auth
            .expect("second login Set must contain controllerAuth");
        assert_eq!(controller_auth.room.as_deref(), Some(controlled_room));
        assert_eq!(controller_auth.password.as_deref(), Some("AB-123-456"));
        assert!(second.file.is_none());

        let third = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        let ProtocolMessage::Set { Set: third } = third else {
            panic!("expected Set.file third, got {third:?}");
        };
        let file = third.file.expect("third login Set must contain file");
        assert_eq!(file.name.as_deref(), Some("movie.mkv"));

        let fourth = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(fourth, ProtocolMessage::List { List: None }));

        server
            .send(ProtocolMessage::State {
                State: StateMessage {
                    playstate: Some(PlayState {
                        position: 0.0,
                        paused: true,
                        do_seek: None,
                        set_by: Some("server".to_string()),
                    }),
                    ping: None,
                    ignoring_on_the_fly: None,
                },
            })
            .unwrap();
        let first_state_response = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            first_state_response,
            ProtocolMessage::State { .. }
        ));

        disconnect_from_server_state(&state).await.unwrap();
        server.close();
    }

    #[tokio::test]
    async fn automatic_remote_close_reconnect_resets_state_and_rejoins_cleanly() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());

        let mut server = FakeSyncplayServer::start().await.unwrap();
        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        expect_client_tls_or_hello(&mut server).await;
        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();

        timeout(Duration::from_secs(2), async {
            loop {
                if factory.launch_count() == 1 {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        state.client_state.add_user(crate::client::state::User {
            username: "bob".to_string(),
            room: "room".to_string(),
            file: Some("other.mkv".to_string()),
            file_size: None,
            file_duration: None,
            is_ready: Some(true),
            is_controller: false,
            features: None,
        });
        state.client_state.set_file(Some("local.mkv".to_string()));
        state.client_state.set_ready_state(Some(true));
        playback_runtime::replace_playlist(
            &state,
            vec!["one.mkv".to_string(), "two.mkv".to_string()],
            Some(1),
        )
        .await
        .unwrap();
        *state.last_global_update.lock() = Some(std::time::Instant::now());
        *state.last_state_message_sent.lock() = Some(std::time::Instant::now());
        *state.last_protocol_activity.lock() = Some(std::time::Instant::now());

        let reconnect_tx = server.take_command_sender();
        reconnect_tx
            .send(crate::network::fake_server::FakeServerCommand::AbortConnection)
            .unwrap();
        let reconnect_message = {
            let reconnect_wait = server.wait_for_reconnect_message();
            tokio::pin!(reconnect_wait);
            timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        message = &mut reconnect_wait => break message,
                        _ = sleep(Duration::from_millis(10)) => {
                            if state.reconnect_state.lock().running {
                                assert_eq!(state.client_state.get_users().len(), 0);
                                assert_eq!(state.client_state.get_file().as_deref(), Some("local.mkv"));
                                assert_eq!(state.client_state.ready_state(), Some(true));
                                assert_eq!(
                                    state.playlist.snapshot(),
                                    (
                                        vec!["one.mkv".to_string(), "two.mkv".to_string()],
                                        Some(1)
                                    )
                                );
                                assert!(*state.playlist_may_need_restoring.lock());
                                assert!(state.connection.lock().is_none() || state.connection.lock().as_ref().map(|connection| !matches!(connection.state(), crate::network::connection::ConnectionState::Authenticated)).unwrap_or(true));
                                assert!(state.player.lock().is_some(), "remote close must not stop player");
                                assert_eq!(factory.players()[0].shutdown_count(), 0);
                                assert!(state.reconnect_snapshot.lock().is_some());
                            }
                        }
                    }
                }
            })
            .await
            .unwrap()
            .unwrap()
        };
        match reconnect_message {
            ProtocolMessage::TLS { .. } => {
                server
                    .send(FakeSyncplayServer::tls_response("unsupported"))
                    .unwrap();
                assert!(matches!(
                    timeout(Duration::from_secs(2), server.next_received())
                        .await
                        .unwrap()
                        .unwrap(),
                    ProtocolMessage::Hello { .. }
                ));
            }
            ProtocolMessage::Hello { .. } => {}
            other => panic!("expected reconnected TLS or Hello, got {other:?}"),
        }

        assert_eq!(state.client_state.get_users().len(), 0);
        assert_eq!(state.client_state.get_file().as_deref(), Some("local.mkv"));
        assert_eq!(state.client_state.ready_state(), Some(true));
        assert_eq!(
            state.playlist.snapshot(),
            (vec!["one.mkv".to_string(), "two.mkv".to_string()], Some(1))
        );
        assert!(*state.playlist_may_need_restoring.lock());
        assert!(
            state.connection.lock().is_none()
                || state
                    .connection
                    .lock()
                    .as_ref()
                    .map(|connection| !matches!(
                        connection.state(),
                        crate::network::connection::ConnectionState::Authenticated
                    ))
                    .unwrap_or(true)
        );
        assert_eq!(factory.players()[0].shutdown_count(), 0);
        assert!(state.reconnect_snapshot.lock().is_some());

        let duplicate_attempts = state.reconnect_state.lock().attempts;
        start_reconnect_loop(state.clone(), current_connection_session(&state));
        sleep(Duration::from_millis(20)).await;
        assert!(state.reconnect_state.lock().attempts <= duplicate_attempts + 1);

        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();
        let reconnect_ready = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        let ProtocolMessage::Set {
            Set: reconnect_ready,
        } = reconnect_ready
        else {
            panic!("expected Set.ready after reconnect Hello, got {reconnect_ready:?}");
        };
        let reconnect_ready = reconnect_ready
            .ready
            .expect("reconnect login must restore runtime ready state");
        assert_eq!(reconnect_ready.is_ready, Some(true));
        assert_eq!(reconnect_ready.manually_initiated, Some(false));
        timeout(Duration::from_secs(2), async {
            loop {
                let authenticated = state
                    .connection
                    .lock()
                    .as_ref()
                    .map(|connection| {
                        connection.state()
                            == crate::network::connection::ConnectionState::Authenticated
                    })
                    .unwrap_or(false);
                if authenticated {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(state.client_state.get_username(), "alice");
        assert_eq!(state.client_state.get_room(), "room");
        assert_eq!(state.client_state.ready_state(), Some(true));
        assert!(!state.reconnect_state.lock().running);
        server.close();
    }

    #[tokio::test]
    async fn reconnect_continues_when_the_replacement_transport_closes_before_hello() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory);
        *state.client_supports_tls.lock() = false;
        let mut server = FakeSyncplayServer::start().await.unwrap();

        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                if state.is_connected() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        server.abort_connection();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.wait_for_reconnect_message())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        {
            let mut ignoring = state.ignoring_on_the_fly.lock();
            ignoring.client = 3;
            ignoring.server = 4;
        }
        state.ping_service.lock().receive_message(
            crate::network::ping::PingService::new_timestamp() - 1.0,
            0.1,
        );
        assert!(state.ping_service.lock().get_rtt() > 0.0);
        server.abort_connection();

        assert!(matches!(
            timeout(Duration::from_secs(2), server.wait_for_reconnect_message())
                .await
                .expect("reconnect stopped after the pre-Hello transport loss")
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        assert_eq!(state.ignoring_on_the_fly.lock().client, 0);
        assert_eq!(state.ignoring_on_the_fly.lock().server, 0);
        assert_eq!(state.ping_service.lock().get_rtt(), 0.0);
        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                if state.is_connected() && !state.reconnect_state.lock().running {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        disconnect_from_server_state(&state).await.unwrap();
        server.close();
    }

    #[tokio::test]
    async fn scripted_connect_player_launch_disconnect_player_closed_reconnect_player_relaunched() {
        let _ = tracing_subscriber::fmt::try_init();
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());

        let mut server1 = FakeSyncplayServer::start().await.unwrap();
        connect_to_server_state::<tauri::test::MockRuntime>(
            server1.host(),
            server1.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        expect_client_tls_or_hello(&mut server1).await;
        server1
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();

        timeout(Duration::from_secs(2), async {
            loop {
                if factory.launch_count() == 1 && state.is_player_connected() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("connect -> player launch evidence timed out");
        assert_eq!(
            factory.launch_count(),
            1,
            "connect launched first fake player"
        );
        assert_eq!(
            factory.players()[0].shutdown_count(),
            0,
            "first fake player is running before disconnect"
        );

        disconnect_from_server_state(&state).await.unwrap();
        assert!(
            state.player.lock().is_none(),
            "disconnect closed player state"
        );
        assert_eq!(
            factory.players()[0].shutdown_count(),
            1,
            "manual disconnect shut down first fake player"
        );
        assert!(
            !state.is_connected(),
            "manual disconnect closed server connection"
        );
        server1.close();

        let mut server2 = FakeSyncplayServer::start().await.unwrap();
        connect_to_server_state::<tauri::test::MockRuntime>(
            server2.host(),
            server2.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        expect_client_tls_or_hello(&mut server2).await;
        server2
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();

        timeout(Duration::from_secs(2), async {
            loop {
                if factory.launch_count() == 2 && state.is_player_connected() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reconnect -> player relaunch evidence timed out");
        assert_eq!(
            factory.launch_count(),
            2,
            "reconnect relaunched fresh fake player"
        );
        assert_eq!(
            factory.players()[1].shutdown_count(),
            0,
            "second fake player remains running after reconnect"
        );
        assert_eq!(
            factory.players()[0].shutdown_count(),
            1,
            "first fake player stayed closed after reconnect"
        );
        server2.close();
    }
    #[tokio::test]
    async fn app_fake_server_hello_state_close_and_reconnect_integration() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());

        let mut server = FakeSyncplayServer::start().await.unwrap();
        let host = server.host();
        let port = server.port();
        connect_to_server_state::<tauri::test::MockRuntime>(
            host.clone(),
            port,
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();
        expect_client_tls_or_hello(&mut server).await;
        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();

        timeout(Duration::from_secs(2), async {
            loop {
                if state
                    .connection
                    .lock()
                    .as_ref()
                    .map(|connection| {
                        connection.state()
                            == crate::network::connection::ConnectionState::Authenticated
                    })
                    .unwrap_or(false)
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        for index in 0..3 {
            let response =
                send_server_state_and_expect_client_state(&mut server, index as f64).await;
            assert_eq!(
                response.ping.unwrap().latency_calculation,
                Some(200.0 + index as f64)
            );
        }

        server.abort_connection();
        expect_client_tls_or_hello(&mut server).await;
        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                if !state.reconnect_state.lock().running
                    && state
                        .connection
                        .lock()
                        .as_ref()
                        .map(|connection| connection.is_connected())
                        .unwrap_or(false)
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let final_response = send_server_state_and_expect_client_state(&mut server, 3.0).await;
        assert!(final_response
            .ping
            .unwrap()
            .client_latency_calculation
            .is_some());
        assert_eq!(state.client_state.get_room(), "room");
        assert_eq!(state.client_state.get_username(), "alice");
        assert!(factory.launch_count() >= 1);
        server.close();
    }

    #[tokio::test]
    async fn every_state_response_includes_client_timing() {
        let state = AppState::new();
        let factory = Arc::new(FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());
        let mut server = FakeSyncplayServer::start().await.unwrap();
        connect_to_server_state::<tauri::test::MockRuntime>(
            server.host(),
            server.port(),
            "alice".to_string(),
            "room".to_string(),
            None,
            None,
            &state,
        )
        .await
        .unwrap();

        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::TLS { .. }
        ));
        server
            .send(FakeSyncplayServer::tls_response("unsupported"))
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        server
            .send(FakeSyncplayServer::hello_response("alice", "room"))
            .unwrap();
        // Complete login before the first State so the local response is not lost while
        // the connection is still marked only as Connected.
        timeout(Duration::from_secs(2), async {
            loop {
                let authenticated = state
                    .connection
                    .lock()
                    .as_ref()
                    .map(|connection| {
                        connection.state()
                            == crate::network::connection::ConnectionState::Authenticated
                    })
                    .unwrap_or(false);
                if authenticated && factory.launch_count() == 1 {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        server
            .send(ProtocolMessage::State {
                State: StateMessage {
                    playstate: Some(PlayState {
                        position: 10.0,
                        paused: true,
                        do_seek: None,
                        set_by: Some("bob".to_string()),
                    }),
                    ping: Some(PingInfo {
                        latency_calculation: Some(100.0),
                        client_latency_calculation: None,
                        client_rtt: None,
                        server_rtt: Some(0.05),
                    }),
                    ignoring_on_the_fly: None,
                },
            })
            .unwrap();

        let first_state = timeout(Duration::from_secs(2), async {
            loop {
                if let Some(ProtocolMessage::State { State }) = server.next_received().await {
                    break State;
                }
            }
        })
        .await
        .unwrap();
        let first_ping = first_state.ping.unwrap();
        assert_eq!(first_ping.latency_calculation, Some(100.0));
        assert!(first_ping.client_latency_calculation.is_some());
        assert!(first_ping.client_rtt.is_some());

        server
            .send(ProtocolMessage::State {
                State: StateMessage {
                    playstate: Some(PlayState {
                        position: 11.0,
                        paused: false,
                        do_seek: None,
                        set_by: Some("bob".to_string()),
                    }),
                    ping: Some(PingInfo {
                        latency_calculation: Some(101.0),
                        client_latency_calculation: Some(
                            crate::network::ping::PingService::new_timestamp(),
                        ),
                        client_rtt: Some(0.2),
                        server_rtt: Some(0.05),
                    }),
                    ignoring_on_the_fly: None,
                },
            })
            .unwrap();
        let second_state = timeout(Duration::from_secs(2), async {
            loop {
                if let Some(ProtocolMessage::State { State }) = server.next_received().await {
                    break State;
                }
            }
        })
        .await
        .unwrap();
        let second_ping = second_state.ping.unwrap();
        assert_eq!(second_ping.latency_calculation, Some(101.0));
        assert!(second_ping.client_latency_calculation.is_some());
        assert!(second_ping.client_rtt.is_some());

        let player = factory.players().first().cloned().unwrap();
        state.server_features.lock().readiness = false;
        player.set_fake_state(PlayerState {
            position: Some(11.0),
            paused: Some(false),
            ..PlayerState::default()
        });
        crate::player::controller::spawn_player_state_loop(state.clone());
        sleep(Duration::from_millis(150)).await;
        player.set_fake_state(PlayerState {
            position: Some(11.0),
            paused: Some(true),
            ..PlayerState::default()
        });
        let local_state = timeout(Duration::from_secs(2), async {
            loop {
                if let Some(ProtocolMessage::State { State }) = server.next_received().await {
                    break State;
                }
            }
        })
        .await
        .expect("local player pause did not produce a State update");
        assert_eq!(
            local_state
                .ping
                .expect("local State must include client timing")
                .latency_calculation,
            None
        );

        disconnect_from_server_state(&state).await.unwrap();
        assert_eq!(player.shutdown_count(), 1);
        server.close();
    }
}
