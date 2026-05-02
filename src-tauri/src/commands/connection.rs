// Connection command handlers

use crate::app_state::{
    AppState, ConnectionSnapshot, ConnectionStatusEvent, ServerFeatures, WarningTimerState,
    WarningTimers,
};
use crate::client::sync::{
    FASTFORWARD_BEHIND_THRESHOLD, FASTFORWARD_EXTRA_TIME, FASTFORWARD_RESET_THRESHOLD,
};
use crate::commands::playlist::apply_playlist_index_from_server;
use crate::config::{save_config, ServerConfig};
use crate::network::connection::Connection;
use crate::network::messages::{
    ClientFeatures, ControllerAuth, HelloMessage, IgnoringInfo, NewControlledRoom, PingInfo,
    PlayState, ProtocolMessage, RoomInfo, SetMessage, StateMessage, TLSMessage, UserUpdate,
};
use crate::network::tls::create_tls_connector;
use crate::player::backend::PlayerBackend;
use crate::player::controller::{ensure_player_connected, load_media_by_name, stop_player};
use crate::player::properties::PlayerState;
use crate::utils::{
    is_controlled_room, parse_controlled_room_input, same_filename, strip_control_password,
    truncate_text, version_meets_min,
};
use serde_json::Value;
use std::sync::Arc;
use tauri::{AppHandle, Runtime, State};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, Duration};

const AUTOPLAY_DELAY_SECONDS: i32 = 3;
const DIFFERENT_DURATION_THRESHOLD: f64 = 2.5;
const WARNING_OSD_INTERVAL_SECONDS: u64 = 1;
const OSD_WARNING_MESSAGE_DURATION_SECONDS: u32 = 5;
const OSD_MESSAGE_SEPARATOR: &str = "; ";
const LAST_PAUSED_DIFF_THRESHOLD_SECONDS: f64 = 2.0;
const RECONNECT_RETRIES: u32 = 999;
const RECONNECT_BASE_DELAY_SECONDS: f64 = 0.1;
const RECONNECT_MAX_EXPONENT: u32 = 5;
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

struct EstablishedConnection {
    connection: Arc<Connection>,
    receiver: mpsc::UnboundedReceiver<ProtocolMessage>,
}

async fn establish_connection(
    state: &Arc<AppState>,
    snapshot: &ConnectionSnapshot,
    emit_reachout: bool,
) -> Result<EstablishedConnection, String> {
    let connection = Arc::new(Connection::new());
    let (receiver, peer_address) = connection
        .connect(snapshot.host.clone(), snapshot.port)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

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
        password: snapshot.password.clone(),
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
        emit_system_message(state, "Attempting secure connection");
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

    *state.connection.lock() = Some(connection.clone());

    Ok(EstablishedConnection {
        connection,
        receiver,
    })
}

fn start_connection_session(
    state: &Arc<AppState>,
    snapshot: &ConnectionSnapshot,
    mut receiver: mpsc::UnboundedReceiver<ProtocolMessage>,
) {
    let config = state.config.lock().clone();
    state.client_state.set_username(snapshot.username.clone());
    state.client_state.set_room(snapshot.room.clone());
    *state.had_first_playlist_index.lock() = false;
    *state.playlist_may_need_restoring.lock() = false;
    *state.last_advance_time.lock() = None;
    *state.last_rewind_time.lock() = None;
    *state.last_updated_file_time.lock() = None;
    *state.last_paused_on_leave_time.lock() = None;
    *state.last_global_update.lock() = None;
    state.sync_engine.lock().update_from_config(&config.user);
    update_autoplay_state(state, &config);

    let state_clone = state.clone();
    tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            tracing::debug!("Received message: {:?}", message);
            handle_server_message(message, &state_clone).await;
        }
        tracing::info!("Message processing loop ended");
        handle_connection_closed(&state_clone).await;
    });
}

async fn complete_server_login(state: &Arc<AppState>, hello: HelloMessage) {
    if let Some(connection) = state.connection.lock().clone() {
        connection.set_authenticated();
    }

    let server_version = hello.realversion.clone();
    let feature_list = hello.features.clone();

    state.client_state.set_username(hello.username.clone());
    if let Some(room) = hello.room.as_ref() {
        state.client_state.set_room(room.name.clone());
    }
    *state.last_connect_time.lock() = Some(std::time::Instant::now());

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
    if let Err(e) = send_ready_state(state, ready_at_login, false) {
        tracing::warn!("Failed to send ready-at-start: {}", e);
    }
    reidentify_as_controller(state);

    if let Err(e) = ensure_player_connected(state).await {
        tracing::warn!("Failed to connect to player after login: {}", e);
    }
    if let Some(player) = state.player.lock().clone() {
        let player_state = player.get_state();
        if (player_state.filename.is_some() || player_state.path.is_some())
            && !crate::player::controller::is_placeholder_file(state, &player_state)
        {
            crate::player::controller::send_file_update(state, &player_state);
        }
    }

    state
        .client_state
        .set_server_version(server_version.clone());
    update_server_features(state, &server_version, feature_list);
    if let Some(player) = state.player.lock().clone() {
        if let Err(e) = player.set_features() {
            tracing::warn!("Failed to send feature update to player: {}", e);
        }
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

fn reconnect_delay(attempt: u32) -> Duration {
    let exponent = attempt.min(RECONNECT_MAX_EXPONENT);
    let delay = RECONNECT_BASE_DELAY_SECONDS * 2_f64.powi(exponent as i32);
    Duration::from_secs_f64(delay)
}

fn start_reconnect_loop(state: Arc<AppState>) {
    let snapshot = state.reconnect_snapshot.lock().clone();
    if snapshot.is_none() {
        return;
    }
    {
        let mut reconnect = state.reconnect_state.lock();
        if reconnect.running || !reconnect.enabled {
            return;
        }
        reconnect.running = true;
    }

    tokio::spawn(async move {
        loop {
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

            if attempt == 1 {
                *state.last_global_update.lock() = None;
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
                emit_error_message(&state, "Connection with server failed");
                let mut reconnect = state.reconnect_state.lock();
                reconnect.enabled = false;
                reconnect.running = false;
                reconnect.attempts = 0;
                break;
            }

            sleep(reconnect_delay(attempt.saturating_sub(1))).await;

            if !state.reconnect_state.lock().enabled {
                reset_reconnect_state(&state);
                break;
            }

            match establish_connection(&state, &snapshot, false).await {
                Ok(established) => {
                    start_connection_session(&state, &snapshot, established.receiver);
                    reset_reconnect_state(&state);
                    break;
                }
                Err(err) => {
                    tracing::warn!("Reconnect attempt failed: {}", err);
                    continue;
                }
            }
        }
    });
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
    tracing::info!(
        "Connecting to {}:{} as {} in room {}",
        host,
        port,
        username,
        room
    );
    emit_system_message(
        state.inner(),
        &format!("Attempting to connect to {}:{}", host, port),
    );

    // Check if already connected
    if state.is_connected() {
        return Err("Already connected to a server".to_string());
    }

    let (normalized_room, control_password) = parse_controlled_room_input(&room);
    let room = truncate_text(&normalized_room, FALLBACK_MAX_ROOM_NAME_LENGTH);
    if let Some(password) = control_password {
        store_control_password(state.inner(), &room, &password, true);
    }
    let username = truncate_text(&username, FALLBACK_MAX_USERNAME_LENGTH);

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

    match establish_connection(state.inner(), &snapshot, true).await {
        Ok(established) => {
            maybe_autosave_connection(state.inner(), &app, &config, snapshot.clone());
            start_connection_session(state.inner(), &snapshot, established.receiver);
            Ok(())
        }
        Err(err) => {
            tracing::error!("Failed to connect: {}", err);
            Err(err)
        }
    }
}

async fn handle_server_message(message: ProtocolMessage, state: &Arc<AppState>) {
    match message {
        ProtocolMessage::Hello { Hello } => {
            tracing::info!("Received hello message: {:?}", Hello);
            emit_system_message(state, &format!("Hello {},", Hello.username));
            complete_server_login(state, Hello).await;
        }
        ProtocolMessage::List { List } => {
            tracing::info!("Received user list: {:?}", List);
            if let Some(users_by_room) = List {
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
                        let file = user_info.file.as_ref().and_then(|f| f.name.clone());
                        let file_size = user_info.file.as_ref().and_then(|f| f.size.clone());
                        let file_duration = user_info.file.as_ref().and_then(|f| f.duration);
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
                emit_user_list(state);
                evaluate_autoplay(state);
                update_room_warnings(state, false);
            }
        }
        ProtocolMessage::Chat { Chat } => {
            tracing::info!("Received chat message: {:?}", Chat);
            let config = state.config.lock().clone();
            if !state.server_features.lock().chat {
                return;
            }
            if !config.user.chat_output_enabled {
                return;
            }
            // Transform chat message to match frontend format
            let (username, message) = match Chat {
                crate::network::messages::ChatMessage::Entry { username, message } => {
                    (Some(username), message)
                }
                crate::network::messages::ChatMessage::Text(message) => (None, message),
            };
            if let Some(player) = state.player.lock().clone() {
                let _ = player.show_chat_message(username.as_deref(), &message);
            }
            let chat_msg = serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "username": username,
                "message": message,
                "messageType": "normal",
            });
            state.emit_event("chat-message-received", chat_msg);
        }
        ProtocolMessage::State { State: state_msg } => {
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
                *state.last_latency_calculation.lock() = ping.latency_calculation;
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
        }
        ProtocolMessage::Error { Error } => {
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
                emit_error_message(state, &Error.message);
                let mut reconnect = state.reconnect_state.lock();
                reconnect.enabled = false;
                reconnect.running = false;
                let connection = state.connection.lock().clone();
                drop(reconnect);
                if let Some(connection) = connection {
                    connection.disconnect();
                }
            }
        }
        ProtocolMessage::Set { Set } => {
            tracing::info!("Received set message: {:?}", Set);
            handle_set_message(state, *Set).await;
        }
        ProtocolMessage::TLS { TLS } => {
            tracing::info!("Received TLS message: {:?}", TLS);
            handle_tls_message(state, TLS).await;
        }
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
    if let Err(e) = player.set_position(position).await {
        tracing::warn!("Failed to set position ({}): {}", context, e);
        return false;
    }
    true
}

async fn handle_state_update(state: &Arc<AppState>, playstate: PlayState, message_age: f64) {
    let had_last_global = state.last_global_update.lock().is_some();
    *state.last_global_update.lock() = Some(std::time::Instant::now());
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
    let pause_changed =
        playstate.paused != previous_global.paused || playstate.paused != local_paused;
    let diff = local_position - adjusted_global_position;
    let mut made_change_on_player = false;

    if !had_last_global && state.client_state.get_file().is_some() {
        if try_set_position(state, &player, adjusted_global_position, "init").await {
            made_change_on_player = true;
        }
        if let Err(e) = player.set_paused(playstate.paused).await {
            tracing::warn!("Failed to set paused on init: {}", e);
        } else {
            made_change_on_player = true;
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
            if try_set_position(state, &player, adjusted_global_position, "seek").await {
                made_change_on_player = true;
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

    if diff > config.user.seek_threshold_rewind
        && !do_seek
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
        if diff < -FASTFORWARD_BEHIND_THRESHOLD && !do_seek {
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
        if diff > config.user.slowdown_threshold && !slowdown_active {
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

    if pause_changed {
        if playstate.paused {
            if actor_name != current_username
                && try_set_position(state, &player, adjusted_global_position, "pause-sync").await
            {
                made_change_on_player = true;
            }
            if let Err(e) = player.set_paused(true).await {
                tracing::warn!("Failed to set paused: {}", e);
            } else {
                made_change_on_player = true;
            }
            let message = format!(
                "{} paused at {}",
                actor_name,
                format_time(adjusted_global_position)
            );
            emit_system_message(state, &message);
            maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
        } else {
            if let Err(e) = player.set_paused(false).await {
                tracing::warn!("Failed to set paused: {}", e);
            } else {
                made_change_on_player = true;
            }
            let message = format!("{} unpaused", actor_name);
            emit_system_message(state, &message);
            maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
        }
    }

    if made_change_on_player {
        if let Err(e) = player.poll_state().await {
            tracing::warn!("Failed to refresh player state after update: {}", e);
        }
        let refreshed_state = player.get_state();
        if let (Some(position), Some(paused)) = (refreshed_state.position, refreshed_state.paused) {
            let global = state.client_state.get_global_state();
            state.local_playback_state.lock().update_from_player(
                position,
                paused,
                global.position,
                global.paused,
            );
        }
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
    let global = state.client_state.get_global_state();
    let local_state = state.local_playback_state.lock();
    let (local_position, local_paused) = local_state.current()?;
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

pub(crate) fn reset_room_sync_state(state: &Arc<AppState>) {
    *state.last_global_update.lock() = None;
    *state.had_first_playlist_index.lock() = false;
    *state.playlist_may_need_restoring.lock() = false;
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

    let current_file = state.client_state.get_file();
    let current_size = state.client_state.get_file_size();
    let current_duration = state.client_state.get_file_duration();
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

fn send_controller_auth(state: &Arc<AppState>, room: &str, password: &str) -> Result<(), String> {
    let connection = state.connection.lock().clone();
    let Some(connection) = connection else {
        return Err("Not connected to server".to_string());
    };
    let message = ProtocolMessage::Set {
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
    };
    connection
        .send(message)
        .map_err(|e| format!("Failed to send controller auth: {}", e))
}

pub(crate) async fn handle_connection_closed(state: &Arc<AppState>) {
    let connection = state.connection.lock().take();
    if connection.is_none() {
        return;
    }
    let manual_disconnect = *state.manual_disconnect.lock();
    if manual_disconnect {
        *state.manual_disconnect.lock() = false;
        return;
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
        serde_json::json!({ "status": "unknown" }),
    );

    if state.reconnect_state.lock().enabled {
        start_reconnect_loop(state.clone());
    } else {
        emit_system_message(state, "Disconnected from server");
    }
}

async fn handle_set_message(state: &Arc<AppState>, set_msg: SetMessage) {
    let has_index_update = set_msg.playlist_index.is_some();
    if let Some(room) = set_msg.room {
        state.client_state.set_room(room.name);
        reset_room_sync_state(state);
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
    if shared_playlists_enabled(state, &config) {
        let mut emit_playlist = false;
        if let Some(change) = set_msg.playlist_change {
            let room = state.client_state.get_room();
            let mut should_restore = false;
            {
                let mut may_restore = state.playlist_may_need_restoring.lock();
                if *may_restore {
                    *may_restore = false;
                    if change.files.is_empty()
                        && change.user.is_none()
                        && !state.playlist.get_item_filenames().is_empty()
                        && !state.playlist.playlist_buffer_is_from_old_room(&room)
                    {
                        should_restore = true;
                    }
                }
            }

            if should_restore {
                let items = state.playlist.get_item_filenames();
                let restore_message = ProtocolMessage::Set {
                    Set: Box::new(SetMessage {
                        room: None,
                        file: None,
                        user: None,
                        ready: None,
                        playlist_index: None,
                        playlist_change: Some(crate::network::messages::PlaylistChange {
                            user: None,
                            files: items.clone(),
                        }),
                        controller_auth: None,
                        new_controlled_room: None,
                        features: None,
                    }),
                };
                if let Some(connection) = state.connection.lock().clone() {
                    if let Err(e) = connection.send(restore_message) {
                        tracing::warn!("Failed to restore playlist: {}", e);
                    }
                    if let Some(index) = state.playlist.get_current_index() {
                        let index_message = ProtocolMessage::Set {
                            Set: Box::new(SetMessage {
                                room: None,
                                file: None,
                                user: None,
                                ready: None,
                                playlist_index: Some(
                                    crate::network::messages::PlaylistIndexUpdate {
                                        user: None,
                                        index: Some(index),
                                    },
                                ),
                                playlist_change: None,
                                controller_auth: None,
                                new_controlled_room: None,
                                features: None,
                            }),
                        };
                        if let Err(e) = connection.send(index_message) {
                            tracing::warn!("Failed to restore playlist index: {}", e);
                        }
                    }
                }
            } else {
                state
                    .playlist
                    .update_previous_playlist(&change.files, &room);
                let current_index = state.playlist.get_current_index();
                let next_index = match current_index {
                    Some(index) if index < change.files.len() => Some(index),
                    _ if change.files.is_empty() => None,
                    _ => Some(0),
                };
                state
                    .playlist
                    .set_items_with_index(change.files, next_index);
                emit_playlist = true;
                if let Some(user) = change.user {
                    let message = format!("{} updated the playlist", user);
                    emit_system_message(state, &message);
                    maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
                }
                if !has_index_update && state.client_state.get_file().is_none() {
                    if let Some(index) = state.playlist.get_current_index() {
                        if let Err(e) = apply_playlist_index_from_server(state, index, false).await
                        {
                            tracing::warn!("Failed to load playlist after sync: {}", e);
                        }
                    }
                }
            }
        }

        if let Some(index_update) = set_msg.playlist_index {
            if let Some(index) = index_update.index {
                let reset_position = {
                    let mut had_first = state.had_first_playlist_index.lock();
                    if !*had_first {
                        *had_first = true;
                        false
                    } else {
                        true
                    }
                };
                let mut skipped_load = false;
                let user = index_update.user.clone();
                let items = state.playlist.get_item_filenames();
                if let Some(filename) = items.get(index) {
                    if same_filename(state.client_state.get_file().as_deref(), Some(filename)) {
                        state.playlist.set_current_index(index);
                        state
                            .playlist
                            .set_queued_index_filename(Some(filename.clone()));
                        emit_playlist_update(state);
                        skipped_load = true;
                    }
                }
                if !skipped_load {
                    if let Err(e) =
                        apply_playlist_index_from_server(state, index, reset_position).await
                    {
                        tracing::warn!("Failed to apply playlist index: {}", e);
                    }
                }
                if let Some(user) = user {
                    let message = format!("{} changed the playlist selection", user);
                    emit_system_message(state, &message);
                    maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
                }
                emit_playlist = false;
            }
        }

        if emit_playlist {
            emit_playlist_update(state);
        }
    }

    evaluate_autoplay(state);
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

    state.client_state.set_room(room_name.clone());
    reset_room_sync_state(state);
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

    if answer == "true" {
        tracing::info!("Server accepted TLS, upgrading connection");
        let tls_info = match connection.upgrade_tls().await {
            Ok(info) => info,
            Err(e) => {
                tracing::error!("TLS upgrade failed: {}", e);
                state.emit_event(
                    "tls-status-changed",
                    serde_json::json!({ "status": "unsupported" }),
                );
                send_hello(state);
                return;
            }
        };
        state.emit_event(
            "tls-status-changed",
            serde_json::json!({ "status": "enabled" }),
        );
        let protocol = tls_info.protocol.unwrap_or_else(|| "TLS".to_string());
        emit_system_message(
            state,
            &format!("Secure connection established ({})", protocol),
        );
        send_hello(state);
    } else if answer == "false" {
        tracing::info!("Server does not support TLS, sending Hello");
        *state.server_supports_tls.lock() = false;
        state.emit_event(
            "tls-status-changed",
            serde_json::json!({ "status": "unsupported" }),
        );
        send_hello(state);
    } else {
        tracing::debug!("Ignoring TLS message: {}", answer);
    }
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

fn is_readiness_supported(state: &Arc<AppState>, requires_other_users: bool) -> bool {
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
    let state_clone = state.clone();
    tokio::spawn(async move {
        if let Err(e) = ensure_player_connected(&state_clone).await {
            tracing::warn!("Failed to connect player for music override: {}", e);
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

fn send_ready_state(
    state: &Arc<AppState>,
    is_ready: bool,
    manually_initiated: bool,
) -> Result<(), String> {
    if !is_readiness_supported(state, false) {
        return Ok(());
    }
    state.client_state.set_ready(is_ready);
    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: Some(crate::network::messages::ReadyState {
                username: None,
                is_ready: Some(is_ready),
                manually_initiated: Some(manually_initiated),
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
                if let Err(e) = ensure_player_connected(&state).await {
                    tracing::warn!("Failed to connect to player for autoplay: {}", e);
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
    if let Err(e) = ensure_player_connected(state).await {
        tracing::warn!("Failed to connect to player for pause: {}", e);
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
    let current_file = state.client_state.get_file();
    let current_size = state.client_state.get_file_size();
    let current_duration = state.client_state.get_file_duration();
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

fn emit_user_list(state: &Arc<AppState>) {
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
    state.emit_event(
        "user-list-updated",
        serde_json::json!({ "users": users_json }),
    );
}

fn emit_playlist_update(state: &Arc<AppState>) {
    let items: Vec<String> = state
        .playlist
        .get_items()
        .iter()
        .map(|item| item.filename.clone())
        .collect();
    state.emit_event(
        "playlist-updated",
        crate::app_state::PlaylistEvent {
            items,
            current_index: state.playlist.get_current_index(),
        },
    );
}

#[tauri::command]
pub async fn disconnect_from_server(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    tracing::info!("Disconnecting from server");

    {
        let mut reconnect = state.reconnect_state.lock();
        reconnect.enabled = false;
        reconnect.running = false;
        reconnect.attempts = 0;
    }
    *state.manual_disconnect.lock() = true;

    // Disconnect
    if let Some(connection) = state.connection.lock().take() {
        connection.disconnect();
    }

    state.client_state.clear_users();
    state.playlist.clear();
    state.client_state.set_file(None);
    state.client_state.set_ready_state(None);
    *state.server_features.lock() = ServerFeatures::default();
    *state.playlist_may_need_restoring.lock() = false;
    *state.had_first_playlist_index.lock() = false;
    *state.last_connect_time.lock() = None;
    *state.last_rewind_time.lock() = None;
    *state.last_advance_time.lock() = None;
    *state.last_updated_file_time.lock() = None;
    *state.last_paused_on_leave_time.lock() = None;
    {
        let mut autoplay = state.autoplay.lock();
        autoplay.countdown_active = false;
        autoplay.countdown_remaining = 0;
    }
    *state.room_warning_state.lock() = crate::app_state::RoomWarningState::default();
    *state.warning_timers.lock() = WarningTimers::default();
    *state.room_warning_task_running.lock() = false;
    state.emit_event("user-list-updated", serde_json::json!({ "users": [] }));
    state.emit_event(
        "playlist-updated",
        crate::app_state::PlaylistEvent {
            items: Vec::new(),
            current_index: None,
        },
    );

    // Emit connection status event
    state.emit_event(
        "connection-status-changed",
        ConnectionStatusEvent {
            connected: false,
            server: None,
        },
    );
    state.emit_event(
        "tls-status-changed",
        serde_json::json!({ "status": "unknown" }),
    );

    Ok(())
}

#[tauri::command]
pub async fn get_connection_status(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.is_connected())
}
