// Connection command handlers

use crate::app_state::{AppState, ConnectionStatusEvent};
use crate::config::{save_config, ServerConfig};
use crate::network::connection::Connection;
use crate::network::messages::{
    ClientFeatures, ControllerAuth, HelloMessage, IgnoringInfo, NewControlledRoom, PingInfo,
    PlayState, ProtocolMessage, RoomInfo, SetMessage, StateMessage, TLSMessage, UserUpdate,
};
use crate::network::tls::create_tls_connector;
use crate::player::controller::{
    ensure_player_connected, load_media_by_name, load_placeholder_if_empty, stop_player,
};
use crate::player::properties::PlayerState;
use crate::utils::{
    is_controlled_room, parse_controlled_room_input, same_filename, strip_control_password,
};
use std::sync::Arc;
use tauri::{AppHandle, Runtime, State};
use tokio::time::{interval, sleep, Duration};

const AUTOPLAY_DELAY_SECONDS: i32 = 3;
const DIFFERENT_DURATION_THRESHOLD: f64 = 2.5;
const WARNING_OSD_INTERVAL_SECONDS: u64 = 1;
const OSD_MESSAGE_SEPARATOR: &str = "; ";

struct ConnectionSnapshot<'a> {
    host: &'a str,
    port: u16,
    username: &'a str,
    room: &'a str,
    password: Option<&'a str>,
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
    let room = normalized_room;
    if let Some(password) = control_password {
        store_control_password(state.inner(), &room, &password, true);
    }

    // Create new connection
    let connection = Arc::new(Connection::new());

    // Connect to server
    match connection.connect(host.clone(), port).await {
        Ok(mut receiver) => {
            tracing::info!("Successfully connected to server");

            let config = state.config.lock().clone();
            // Send Hello message
            let hello_payload = HelloMessage {
                username: username.clone(),
                password: password.clone(),
                room: Some(RoomInfo {
                    name: room.clone(),
                    password: None,
                }),
                version: "1.2.255".to_string(),
                realversion: "1.7.4".to_string(),
                features: Some(ClientFeatures {
                    shared_playlists: Some(config.user.shared_playlist_enabled),
                    chat: Some(true),
                    ready_state: Some(true),
                    managed_rooms: Some(false),
                    persistent_rooms: Some(false),
                }),
                motd: None,
            };

            *state.last_hello.lock() = Some(hello_payload.clone());
            *state.hello_sent.lock() = false;

            if create_tls_connector().is_ok() {
                emit_system_message(state.inner(), "Attempting secure connection");
                emit_system_message(state.inner(), &format!("Successfully reached {}", host));
                let tls_request = ProtocolMessage::TLS {
                    TLS: TLSMessage {
                        start_tls: Some("send".to_string()),
                    },
                };
                if let Err(e) = connection.send(tls_request) {
                    tracing::error!("Failed to send TLS request: {}", e);
                    send_hello(state.inner());
                } else {
                    tracing::info!("Sent TLS request");
                    state.emit_event(
                        "tls-status-changed",
                        serde_json::json!({ "status": "pending" }),
                    );
                }
            } else {
                tracing::info!("TLS not supported by client, sending Hello");
                emit_system_message(state.inner(), &format!("Successfully reached {}", host));
                state.emit_event(
                    "tls-status-changed",
                    serde_json::json!({ "status": "unsupported" }),
                );
                send_hello(state.inner());
            }

            // Store connection
            *state.connection.lock() = Some(connection.clone());

            // Update client state
            state.client_state.set_username(username.clone());
            state.client_state.set_room(room.clone());
            state.sync_engine.lock().update_from_config(&config.user);
            update_autoplay_state(state.inner(), &config);
            maybe_autosave_connection(
                state.inner(),
                &app,
                &config,
                ConnectionSnapshot {
                    host: &host,
                    port,
                    username: &username,
                    room: &room,
                    password: password.as_deref(),
                },
            );

            if let Err(e) = ensure_player_connected(state.inner()).await {
                tracing::warn!("Failed to connect to player: {}", e);
            } else if let Err(e) = load_placeholder_if_empty(state.inner()).await {
                tracing::warn!("Failed to load placeholder: {}", e);
            }
            start_room_warning_loop(state.inner().clone());

            // Emit connection status event
            state.emit_event(
                "connection-status-changed",
                ConnectionStatusEvent {
                    connected: true,
                    server: Some(format!("{}:{}", host, port)),
                },
            );

            // Spawn message processing task
            let state_clone = state.inner().clone();
            tokio::spawn(async move {
                while let Some(message) = receiver.recv().await {
                    tracing::debug!("Received message: {:?}", message);
                    handle_server_message(message, &state_clone).await;
                }
                tracing::info!("Message processing loop ended");
                handle_connection_closed(&state_clone).await;
            });

            Ok(())
        }
        Err(e) => {
            tracing::error!("Failed to connect: {}", e);
            Err(format!("Connection failed: {}", e))
        }
    }
}

async fn handle_server_message(message: ProtocolMessage, state: &Arc<AppState>) {
    match message {
        ProtocolMessage::Hello { Hello } => {
            tracing::info!("Received hello message: {:?}", Hello);
            state.client_state.set_server_version(Hello.realversion);
            emit_system_message(state, &format!("Hello {},", Hello.username));
            if let Some(motd) = Hello.motd {
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
            if let Some(connection) = state.connection.lock().clone() {
                if let Err(e) = connection.send(ProtocolMessage::List { List: None }) {
                    tracing::warn!("Failed to request user list: {}", e);
                }
            }
            reidentify_as_controller(state);
        }
        ProtocolMessage::List { List } => {
            tracing::info!("Received user list: {:?}", List);
            if let Some(users_by_room) = List {
                state.client_state.clear_users();
                for (room_name, room_users) in users_by_room {
                    for (username, user_info) in room_users {
                        let file = user_info.file.as_ref().and_then(|f| f.name.clone());
                        let file_size = user_info.file.as_ref().and_then(|f| f.size.clone());
                        let file_duration = user_info.file.as_ref().and_then(|f| f.duration);
                        state.client_state.add_user(crate::client::state::User {
                            username,
                            room: room_name.clone(),
                            file,
                            file_size,
                            file_duration,
                            is_ready: user_info.is_ready.unwrap_or(false),
                            is_controller: user_info.controller.unwrap_or(false),
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
            let chat_msg = serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "username": username,
                "message": message,
                "messageType": "normal",
            });
            state.emit_event("chat-message-received", chat_msg);
        }
        ProtocolMessage::State { State: state_msg } => {
            tracing::info!("Received state update: {:?}", state_msg);
            let mut message_age = 0.0;
            if let Some(ignore) = state_msg.ignoring_on_the_fly.as_ref() {
                update_ignoring_on_the_fly(state, ignore);
            }
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
                handle_state_update(state, playstate, message_age).await;
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
            let error_msg = serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "username": null,
                "message": Error.message,
                "messageType": "error",
            });
            state.emit_event("chat-message-received", error_msg);
            if Error.message.contains("startTLS") {
                send_hello(state);
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

async fn handle_state_update(state: &Arc<AppState>, playstate: PlayState, message_age: f64) {
    *state.last_global_update.lock() = Some(std::time::Instant::now());
    let adjusted_global_position = if !playstate.paused {
        playstate.position + message_age
    } else {
        playstate.position
    };
    state.client_state.set_global_state(
        adjusted_global_position,
        playstate.paused,
        playstate.set_by.clone(),
    );

    let player = state.player.lock().clone();
    let Some(player) = player else { return };
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
    let (actions, slowdown_rate) = {
        let mut engine = state.sync_engine.lock();
        let actions = engine.calculate_sync_actions(crate::client::sync::SyncInputs {
            local_position,
            local_paused,
            global_position: playstate.position,
            global_paused: playstate.paused,
            message_age,
            do_seek: playstate.do_seek.unwrap_or(false),
            allow_fastforward: should_allow_fastforward(state, &config),
        });
        let slowdown_rate = engine.slowdown_rate();
        (actions, slowdown_rate)
    };
    let actor = playstate.set_by.clone();
    let actor_name = actor.clone().unwrap_or_else(|| "Unknown".to_string());

    for action in actions {
        match action {
            crate::client::sync::SyncAction::Seek(position) => {
                if let Err(e) = player.set_position(position).await {
                    tracing::warn!("Failed to seek: {}", e);
                }
                if actor_name != state.client_state.get_username() {
                    let message = if local_position > adjusted_global_position {
                        format!("Rewinded due to time difference with {}", actor_name)
                    } else {
                        format!("Fast-forwarded due to time difference with {}", actor_name)
                    };
                    emit_system_message(state, &message);
                    maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
                }
            }
            crate::client::sync::SyncAction::SetPaused(paused) => {
                if !paused {
                    *state.suppress_unpause_check.lock() = true;
                }
                if let Err(e) = player.set_paused(paused).await {
                    tracing::warn!("Failed to set paused: {}", e);
                }
                let message = if paused {
                    let timecode = format_time(playstate.position);
                    format!("{} paused at {}", actor_name, timecode)
                } else {
                    format!("{} unpaused", actor_name)
                };
                emit_system_message(state, &message);
                maybe_show_osd(state, &config, &message, config.user.show_same_room_osd);
            }
            crate::client::sync::SyncAction::Slowdown => {
                if let Err(e) = player.set_speed(slowdown_rate).await {
                    tracing::warn!("Failed to set slowdown: {}", e);
                }
                if actor_name != state.client_state.get_username() {
                    let message =
                        format!("Slowing down due to time difference with {}", actor_name);
                    emit_system_message(state, &message);
                    maybe_show_osd(state, &config, &message, config.user.show_slowdown_osd);
                }
            }
            crate::client::sync::SyncAction::ResetSpeed => {
                if let Err(e) = player.set_speed(1.0).await {
                    tracing::warn!("Failed to reset speed: {}", e);
                }
                let message = "Reverting speed back to normal".to_string();
                emit_system_message(state, &message);
                maybe_show_osd(state, &config, &message, config.user.show_slowdown_osd);
            }
            crate::client::sync::SyncAction::None => {}
        }
    }

    evaluate_autoplay(state);
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

fn emit_system_message(state: &Arc<AppState>, message: &str) {
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
    let username = state.client_state.get_username();
    let can_control = state
        .client_state
        .get_user(&username)
        .map(|user| user.is_controller)
        .unwrap_or(false);
    !can_control
}

fn emit_error_message(state: &Arc<AppState>, message: &str) {
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

fn maybe_show_osd(
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
    let warnings = compute_room_warning_state(state, &config);
    let mut last = state.room_warning_state.lock();

    if !osd_only && warnings.alone && !last.alone {
        emit_system_message(state, "You're alone in the room");
    }

    if config.user.show_osd_warnings {
        show_room_warning_osd(state, &config, &warnings);
    }

    *last = warnings;
}

fn show_room_warning_osd(
    state: &Arc<AppState>,
    config: &crate::config::SyncplayConfig,
    warnings: &crate::app_state::RoomWarningState,
) {
    if !config.user.show_osd {
        return;
    }

    let message = if warnings.alone {
        Some("You're alone in the room".to_string())
    } else {
        match (&warnings.file_differences, &warnings.not_ready) {
            (Some(file_diff), Some(not_ready)) => Some(format!(
                "File differences: {}{}{}",
                file_diff, OSD_MESSAGE_SEPARATOR, not_ready
            )),
            (Some(file_diff), None) => Some(format!("File differences: {}", file_diff)),
            (None, Some(not_ready)) => Some(not_ready.to_string()),
            (None, None) => None,
        }
    };

    let Some(message) = message else { return };
    maybe_show_osd(state, config, &message, true);
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

    if users_in_room.is_empty() {
        return crate::app_state::RoomWarningState::default();
    }

    let others_in_room: Vec<crate::client::state::User> = users_in_room
        .iter()
        .filter(|user| user.username != current_username)
        .cloned()
        .collect();
    let alone = others_in_room.is_empty();

    let current_file = state.client_state.get_file();
    let current_size = state.client_state.get_file_size();
    let current_duration = state.client_state.get_file_duration();
    let mut diff_name = false;
    let mut diff_size = false;
    let mut diff_duration = false;
    if let Some(current_file) = current_file.as_ref() {
        for user in others_in_room.iter() {
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

    let not_ready = if alone {
        None
    } else {
        let not_ready_users: Vec<String> = users_in_room
            .iter()
            .filter(|user| !user.is_ready)
            .map(|user| user.username.clone())
            .collect();
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

async fn handle_connection_closed(state: &Arc<AppState>) {
    let connection = state.connection.lock().take();
    if connection.is_none() {
        return;
    }

    state.client_state.clear_users();
    state.playlist.clear();
    state.client_state.set_file(None);
    state.client_state.set_ready(false);
    {
        let mut autoplay = state.autoplay.lock();
        autoplay.countdown_active = false;
        autoplay.countdown_remaining = 0;
    }

    if let Err(e) = stop_player(state).await {
        tracing::warn!("Failed to stop player after disconnect: {}", e);
    }

    *state.room_warning_state.lock() = crate::app_state::RoomWarningState::default();
    *state.room_warning_task_running.lock() = false;

    state.emit_event("user-list-updated", serde_json::json!({ "users": [] }));
    state.emit_event(
        "playlist-updated",
        crate::app_state::PlaylistEvent {
            items: Vec::new(),
            current_index: None,
        },
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
        serde_json::json!({ "status": "unknown" }),
    );

    emit_system_message(state, "Disconnected from server");
}

async fn handle_set_message(state: &Arc<AppState>, set_msg: SetMessage) {
    if let Some(room) = set_msg.room {
        state.client_state.set_room(room.name);
        reidentify_as_controller(state);
    }

    if let Some(file) = set_msg.file {
        if let Some(name) = file.name {
            state.client_state.set_file(Some(name.clone()));
            state.client_state.set_file_size(file.size.clone());
            state.client_state.set_file_duration(file.duration);
            if let Err(e) = load_media_by_name(state, &name, false).await {
                tracing::warn!("Failed to load file from set: {}", e);
            }
        }
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
        if let Some(username) = ready.username {
            let is_ready = match ready.is_ready {
                Some(value) => value,
                None => state
                    .client_state
                    .get_user(&username)
                    .map(|user| user.is_ready)
                    .unwrap_or(false),
            };

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
                });
                users_changed = true;
            }
            if ready.is_ready.is_some() && username == state.client_state.get_username() {
                state.client_state.set_ready(is_ready);
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

    if users_changed {
        emit_user_list(state);
    }

    if left_in_room {
        let config = state.config.lock().clone();
        if config.user.pause_on_leave {
            pause_local_player(state).await;
        }
    }

    let mut playlist_changed = false;
    if let Some(change) = set_msg.playlist_change {
        state.playlist.set_items(change.files);
        playlist_changed = true;
    }

    if let Some(index_update) = set_msg.playlist_index {
        if let Some(index) = index_update.index {
            if state.playlist.set_current_index(index) {
                playlist_changed = true;
            }
        }
    }

    if playlist_changed {
        emit_playlist_update(state);
        if let Some(item) = state.playlist.get_current_item() {
            if let Err(e) = load_media_by_name(state, &item.filename, false).await {
                tracing::warn!("Failed to load playlist item: {}", e);
            }
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
            is_ready: false,
            is_controller: false,
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
        if let Err(e) = connection.upgrade_tls().await {
            tracing::error!("TLS upgrade failed: {}", e);
            state.emit_event(
                "tls-status-changed",
                serde_json::json!({ "status": "unsupported" }),
            );
            send_hello(state);
            return;
        }
        state.emit_event(
            "tls-status-changed",
            serde_json::json!({ "status": "enabled" }),
        );
        emit_system_message(state, "Secure connection established");
        send_hello(state);
    } else if answer == "false" {
        tracing::info!("Server does not support TLS, sending Hello");
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

    let config = state.config.lock().clone();
    if config.user.ready_at_start {
        if let Err(e) = send_ready_state(state, true, false) {
            tracing::warn!("Failed to send ready-at-start: {}", e);
        }
    }
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
    snapshot: ConnectionSnapshot<'_>,
) {
    if !config.user.autosave_joins_to_list {
        return;
    }

    let mut updated = config.clone();
    updated.server.host = snapshot.host.to_string();
    updated.server.port = snapshot.port;
    updated.server.password = snapshot.password.map(|value| value.to_string());
    updated.user.username = snapshot.username.to_string();
    updated.user.default_room = snapshot.room.to_string();

    updated.add_recent_server(ServerConfig {
        host: snapshot.host.to_string(),
        port: snapshot.port,
        password: snapshot.password.map(|value| value.to_string()),
    });

    if !updated
        .user
        .room_list
        .iter()
        .any(|entry| entry == snapshot.room)
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

fn send_ready_state(
    state: &Arc<AppState>,
    is_ready: bool,
    manually_initiated: bool,
) -> Result<(), String> {
    state.client_state.set_ready(is_ready);
    let username = state.client_state.get_username();
    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: Some(crate::network::messages::ReadyState {
                username: Some(username),
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
    if !config.user.autoplay_enabled {
        return false;
    }

    let room = state.client_state.get_room();
    let users = state.client_state.get_users_in_room(&room);
    if users.is_empty() {
        return false;
    }

    let current_file = state.client_state.get_file();
    for user in &users {
        if !user.is_ready {
            return false;
        }
        if config.user.autoplay_require_same_filenames
            && !same_filename(current_file.as_deref(), user.file.as_deref())
        {
            return false;
        }
    }

    if config.user.autoplay_min_users >= 2 && (users.len() as i32) < config.user.autoplay_min_users
    {
        return false;
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

fn evaluate_autoplay(state: &Arc<AppState>) {
    if autoplay_conditions_met(state) {
        start_autoplay_countdown(state.clone());
    } else {
        let mut autoplay = state.autoplay.lock();
        autoplay.countdown_active = false;
        autoplay.countdown_remaining = 0;
    }
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
    }
}

fn apply_user_update(state: &Arc<AppState>, username: String, update: UserUpdate) -> bool {
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
            is_ready: false,
            is_controller: false,
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
        user.is_ready = is_ready;
    }
    if let Some(controller) = update.controller {
        user.is_controller = controller;
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

fn emit_user_list(state: &Arc<AppState>) {
    let users = state.client_state.get_users();
    let users_json: Vec<serde_json::Value> = users
        .into_iter()
        .map(|u| {
            serde_json::json!({
                "username": u.username,
                "room": u.room,
                "file": u.file,
                "isReady": u.is_ready,
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

    // Disconnect
    if let Some(connection) = state.connection.lock().take() {
        connection.disconnect();
    }

    if let Err(e) = stop_player(state.inner()).await {
        tracing::warn!("Failed to stop player: {}", e);
    }

    state.client_state.clear_users();
    state.playlist.clear();
    state.client_state.set_file(None);
    state.client_state.set_ready(false);
    {
        let mut autoplay = state.autoplay.lock();
        autoplay.countdown_active = false;
        autoplay.countdown_remaining = 0;
    }
    *state.room_warning_state.lock() = crate::app_state::RoomWarningState::default();
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
