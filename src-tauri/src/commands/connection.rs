// Connection command handlers

use crate::app_state::{AppState, ConnectionStatusEvent};
use crate::network::connection::Connection;
use crate::network::messages::{
    ClientFeatures, HelloMessage, PlayState, ProtocolMessage, RoomInfo, SetMessage, StateMessage,
    TLSMessage, UserUpdate,
};
use crate::network::tls::create_tls_connector;
use crate::player::controller::{ensure_mpv_connected, load_media_by_name};
use crate::player::properties::PlayerState;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;

#[tauri::command]
pub async fn connect_to_server(
    host: String,
    port: u16,
    username: String,
    room: String,
    password: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!(
        "Connecting to {}:{} as {} in room {}",
        host,
        port,
        username,
        room
    );

    // Check if already connected
    if state.is_connected() {
        return Err("Already connected to a server".to_string());
    }

    // Create new connection
    let connection = Arc::new(Connection::new());

    // Connect to server
    match connection.connect(host.clone(), port).await {
        Ok(mut receiver) => {
            tracing::info!("Successfully connected to server");

            // Send Hello message
            let hello_payload = HelloMessage {
                username: username.clone(),
                password: password.clone(),
                room: Some(RoomInfo {
                    name: room.clone(),
                    password: None,
                }),
                version: "1.2.255".to_string(),
                realversion: "1.7.3".to_string(),
                features: Some(ClientFeatures {
                    shared_playlists: Some(true),
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
                }
            } else {
                tracing::info!("TLS not supported by client, sending Hello");
                send_hello(state.inner());
            }

            // Store connection
            *state.connection.lock() = Some(connection.clone());

            // Update client state
            state.client_state.set_username(username);
            state.client_state.set_room(room);
            let config = state.config.lock().clone();
            state.sync_engine.lock().update_from_config(&config.user);

            if let Err(e) = ensure_mpv_connected(state.inner()).await {
                tracing::warn!("Failed to connect to mpv: {}", e);
            }

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
        }
        ProtocolMessage::List { List } => {
            tracing::info!("Received user list: {:?}", List);
            if let Some(users_by_room) = List {
                state.client_state.clear_users();
                for (room_name, room_users) in users_by_room {
                    for (username, user_info) in room_users {
                        state.client_state.add_user(crate::client::state::User {
                            username,
                            room: room_name.clone(),
                            file: user_info.file.and_then(|f| f.name),
                            is_ready: user_info.is_ready.unwrap_or(false),
                            is_controller: user_info.controller.unwrap_or(false),
                        });
                    }
                }
                emit_user_list(state);
            }
        }
        ProtocolMessage::Chat { Chat } => {
            tracing::info!("Received chat message: {:?}", Chat);
            // Transform chat message to match frontend format
            let chat_msg = serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "username": Chat.username,
                "message": Chat.message,
                "messageType": "normal",
            });
            state.emit_event("chat-message-received", chat_msg);
        }
        ProtocolMessage::State { State: state_msg } => {
            tracing::info!("Received state update: {:?}", state_msg);
            let message_age = state_msg
                .ping
                .as_ref()
                .and_then(|p| p.server_rtt)
                .map(|rtt| rtt / 2.0)
                .unwrap_or(0.0);
            if let Some(ping) = state_msg.ping.as_ref() {
                if let Some(connection) = state.connection.lock().clone() {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    let response = ProtocolMessage::State {
                        State: StateMessage {
                            playstate: None,
                            ping: Some(crate::network::messages::PingInfo {
                                latency_calculation: ping.latency_calculation,
                                client_latency_calculation: Some(now),
                                client_rtt: None,
                                server_rtt: None,
                            }),
                            ignoring_on_the_fly: None,
                        },
                    };
                    if let Err(e) = connection.send(response) {
                        tracing::warn!("Failed to send ping response: {}", e);
                    }
                }
            }
            if let Some(playstate) = state_msg.playstate {
                handle_state_update(state, playstate, message_age).await;
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
            handle_set_message(state, Set).await;
        }
        ProtocolMessage::TLS { TLS } => {
            tracing::info!("Received TLS message: {:?}", TLS);
            handle_tls_message(state, TLS).await;
        }
    }
}

async fn handle_state_update(state: &Arc<AppState>, playstate: PlayState, message_age: f64) {
    state.client_state.set_global_state(
        playstate.position,
        playstate.paused,
        playstate.set_by.clone(),
    );

    if let Err(e) = ensure_mpv_connected(state).await {
        tracing::warn!("Failed to connect to mpv: {}", e);
        return;
    }

    let mpv = state.mpv.lock().clone();
    let Some(mpv) = mpv else { return };
    let player_state: PlayerState = mpv.get_state();
    let (local_position, local_paused) = match (player_state.position, player_state.paused) {
        (Some(pos), Some(paused)) => (pos, paused),
        _ => return,
    };

    let (actions, slowdown_rate) = {
        let mut engine = state.sync_engine.lock();
        let actions = engine.calculate_sync_actions(
            local_position,
            local_paused,
            playstate.position,
            playstate.paused,
            message_age,
        );
        let slowdown_rate = engine.slowdown_rate();
        (actions, slowdown_rate)
    };

    for action in actions {
        match action {
            crate::client::sync::SyncAction::Seek(position) => {
                if let Err(e) = mpv.set_position(position).await {
                    tracing::warn!("Failed to seek: {}", e);
                }
            }
            crate::client::sync::SyncAction::SetPaused(paused) => {
                if let Err(e) = mpv.set_paused(paused).await {
                    tracing::warn!("Failed to set paused: {}", e);
                }
            }
            crate::client::sync::SyncAction::Slowdown => {
                if let Err(e) = mpv.set_speed(slowdown_rate).await {
                    tracing::warn!("Failed to set slowdown: {}", e);
                }
            }
            crate::client::sync::SyncAction::ResetSpeed => {
                if let Err(e) = mpv.set_speed(1.0).await {
                    tracing::warn!("Failed to reset speed: {}", e);
                }
            }
            crate::client::sync::SyncAction::None => {}
        }
    }
}

async fn handle_set_message(state: &Arc<AppState>, set_msg: SetMessage) {
    if let Some(room) = set_msg.room {
        state.client_state.set_room(room.name);
    }

    if let Some(file) = set_msg.file {
        if let Some(name) = file.name {
            state.client_state.set_file(Some(name.clone()));
            if let Err(e) = load_media_by_name(state, &name, false).await {
                tracing::warn!("Failed to load file from set: {}", e);
            }
        }
    }

    let mut users_changed = false;
    if let Some(user_updates) = set_msg.user {
        for (username, update) in user_updates {
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

    if users_changed {
        emit_user_list(state);
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
            send_hello(state);
            return;
        }
        send_hello(state);
    } else if answer == "false" {
        tracing::info!("Server does not support TLS, sending Hello");
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

fn apply_user_update(state: &Arc<AppState>, username: String, update: UserUpdate) -> bool {
    if let Some(event) = update.event.as_ref() {
        if event.left.unwrap_or(false) {
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
            is_ready: false,
            is_controller: false,
        });

    if let Some(room) = update.room {
        user.room = room.name;
    }
    if let Some(file) = update.file {
        user.file = file.name;
    }
    if let Some(is_ready) = update.is_ready {
        user.is_ready = is_ready;
    }
    if let Some(controller) = update.controller {
        user.is_controller = controller;
    }

    state.client_state.add_user(user);
    true
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

    state.client_state.clear_users();
    state.playlist.clear();
    state.client_state.set_file(None);
    state.client_state.set_ready(false);
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

    Ok(())
}

#[tauri::command]
pub async fn get_connection_status(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.is_connected())
}
