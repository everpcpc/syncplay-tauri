// Connection command handlers

use crate::app_state::{AppState, ConnectionStatusEvent};
use crate::network::connection::Connection;
use crate::network::messages::{HelloMessage, ProtocolMessage, RoomInfo, ClientFeatures};
use std::sync::Arc;
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
    let mut connection = Connection::new();

    // Connect to server
    match connection.connect(host.clone(), port).await {
        Ok(mut receiver) => {
            tracing::info!("Successfully connected to server");

            // Send Hello message
            let hello = ProtocolMessage::Hello {
                Hello: HelloMessage {
                    username: username.clone(),
                    password: password.clone(),
                    room: Some(RoomInfo {
                        name: room.clone(),
                        password: None,
                    }),
                    version: "1.7.0".to_string(),
                    realversion: "syncplay-tauri-0.1.0".to_string(),
                    features: Some(ClientFeatures {
                        shared_playlists: Some(true),
                        chat: Some(true),
                        ready_state: Some(true),
                        managed_rooms: Some(false),
                        persistent_rooms: Some(false),
                    }),
                    motd: None,
                },
            };

            if let Err(e) = connection.send(hello) {
                tracing::error!("Failed to send Hello message: {}", e);
                return Err(format!("Failed to send Hello message: {}", e));
            }

            tracing::info!("Sent Hello message");

            // Store connection
            *state.connection.lock() = Some(connection);

            // Update client state
            state.client_state.set_username(username);
            state.client_state.set_room(room);

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
        ProtocolMessage::List { List } => {
            tracing::info!("Received user list: {:?}", List);
            if let Some(users_by_room) = List {
                // Transform nested HashMap into flat array of users
                let mut users = Vec::new();
                for (room_name, room_users) in users_by_room {
                    for (username, user_info) in room_users {
                        users.push(serde_json::json!({
                            "username": username,
                            "room": room_name,
                            "file": user_info.file.and_then(|f| f.name),
                            "isReady": user_info.is_ready.unwrap_or(false),
                            "isController": user_info.controller.unwrap_or(false),
                        }));
                    }
                }
                tracing::info!("Transformed user list: {:?}", users);
                // Emit user list event
                state.emit_event("user-list-updated", serde_json::json!({ "users": users }));
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
            // TODO: Handle state update
        }
        ProtocolMessage::Error { Error } => {
            tracing::error!("Received error from server: {:?}", Error);
            // TODO: Handle error
        }
        _ => {
            tracing::debug!("Received other message type");
        }
    }
}

#[tauri::command]
pub async fn disconnect_from_server(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    tracing::info!("Disconnecting from server");

    // Disconnect
    if let Some(mut connection) = state.connection.lock().take() {
        connection.disconnect();
    }

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
