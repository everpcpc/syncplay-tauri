// Chat command handlers

use crate::app_state::AppState;
use crate::client::chat::ChatCommand;
use crate::commands::connection::{
    reidentify_as_controller, reset_room_sync_state, store_control_password,
};
use crate::network::messages::ProtocolMessage;
use crate::network::messages::{
    ChatMessage as ProtocolChatMessage, ReadyState, RoomInfo, SetMessage,
};
use crate::utils::{parse_controlled_room_input, truncate_text};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn send_chat_message(
    message: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    send_chat_message_inner(state.inner(), &message).await
}

pub async fn send_chat_message_from_player(
    state: &Arc<AppState>,
    message: &str,
) -> Result<(), String> {
    send_chat_message_inner(state, message).await
}

async fn send_chat_message_inner(state: &Arc<AppState>, message: &str) -> Result<(), String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let config = state.config.lock().clone();
    if !config.user.chat_input_enabled {
        return Err("Chat input is disabled".to_string());
    }
    if !state.server_features.lock().chat {
        return Err("Chat is disabled by the server".to_string());
    }

    let max_length = state
        .server_features
        .lock()
        .max_chat_message_length
        .unwrap_or(150);
    let sanitized = trimmed.replace(['\n', '\r'], "");
    let message = truncate_text(&sanitized, max_length);
    tracing::info!("Sending chat message: {}", message);

    if !state.is_connected() {
        return Err("Not connected to server".to_string());
    }

    if let Some(command) = ChatCommand::parse(&message) {
        match command {
            ChatCommand::Room(room) => {
                tracing::info!("Command: Change room to {}", room);
                let max_len = state
                    .server_features
                    .lock()
                    .max_room_name_length
                    .unwrap_or(35);
                let trimmed_room = truncate_text(&room, max_len);
                let (normalized_room, control_password) =
                    parse_controlled_room_input(&trimmed_room);
                let room = normalized_room;
                if let Some(password) = control_password {
                    store_control_password(state, &room, &password, true);
                }
                state.client_state.set_room(room);
                reset_room_sync_state(state);
                let set_msg = ProtocolMessage::Set {
                    Set: Box::new(SetMessage {
                        room: Some(RoomInfo {
                            name: state.client_state.get_room(),
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
                send_to_server_arc(state, set_msg)?;
                send_to_server_arc(state, ProtocolMessage::List { List: None })?;
                reidentify_as_controller(state);
            }
            ChatCommand::List => {
                tracing::info!("Command: List users");
                let users = state.client_state.get_users();
                let user_list: Vec<String> = users
                    .iter()
                    .map(|u| format!("{} ({})", u.username, u.room))
                    .collect();
                let message = format!("Users: {}", user_list.join(", "));
                state.chat.add_system_message(message.clone());
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
            ChatCommand::Help => {
                tracing::info!("Command: Show help");
                let help = ChatCommand::help_text();
                state.chat.add_system_message(help.clone());
                state.emit_event(
                    "chat-message-received",
                    serde_json::json!({
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                        "username": null,
                        "message": help,
                        "messageType": "system",
                    }),
                );
            }
            ChatCommand::Ready => {
                tracing::info!("Command: Set ready");
                if !state.server_features.lock().readiness {
                    return Err("Ready state is not supported by the server".to_string());
                }
                state.client_state.set_ready(true);
                let set_msg = ProtocolMessage::Set {
                    Set: Box::new(SetMessage {
                        room: None,
                        file: None,
                        user: None,
                        ready: Some(ReadyState {
                            username: None,
                            is_ready: Some(true),
                            manually_initiated: Some(true),
                            set_by: None,
                        }),
                        playlist_index: None,
                        playlist_change: None,
                        controller_auth: None,
                        new_controlled_room: None,
                        features: None,
                    }),
                };
                send_to_server_arc(state, set_msg)?;
            }
            ChatCommand::Unready => {
                tracing::info!("Command: Set unready");
                if !state.server_features.lock().readiness {
                    return Err("Ready state is not supported by the server".to_string());
                }
                state.client_state.set_ready(false);
                let set_msg = ProtocolMessage::Set {
                    Set: Box::new(SetMessage {
                        room: None,
                        file: None,
                        user: None,
                        ready: Some(ReadyState {
                            username: None,
                            is_ready: Some(false),
                            manually_initiated: Some(true),
                            set_by: None,
                        }),
                        playlist_index: None,
                        playlist_change: None,
                        controller_auth: None,
                        new_controlled_room: None,
                        features: None,
                    }),
                };
                send_to_server_arc(state, set_msg)?;
            }
            ChatCommand::SetReady(username) => {
                tracing::info!("Command: Set other user ready");
                if !state.server_features.lock().set_others_readiness {
                    return Err("Readiness override is not supported by the server".to_string());
                }
                let set_msg = ProtocolMessage::Set {
                    Set: Box::new(SetMessage {
                        room: None,
                        file: None,
                        user: None,
                        ready: Some(ReadyState {
                            username: Some(username),
                            is_ready: Some(true),
                            manually_initiated: Some(true),
                            set_by: None,
                        }),
                        playlist_index: None,
                        playlist_change: None,
                        controller_auth: None,
                        new_controlled_room: None,
                        features: None,
                    }),
                };
                send_to_server_arc(state, set_msg)?;
            }
            ChatCommand::SetNotReady(username) => {
                tracing::info!("Command: Set other user not ready");
                if !state.server_features.lock().set_others_readiness {
                    return Err("Readiness override is not supported by the server".to_string());
                }
                let set_msg = ProtocolMessage::Set {
                    Set: Box::new(SetMessage {
                        room: None,
                        file: None,
                        user: None,
                        ready: Some(ReadyState {
                            username: Some(username),
                            is_ready: Some(false),
                            manually_initiated: Some(true),
                            set_by: None,
                        }),
                        playlist_index: None,
                        playlist_change: None,
                        controller_auth: None,
                        new_controlled_room: None,
                        features: None,
                    }),
                };
                send_to_server_arc(state, set_msg)?;
            }
            ChatCommand::Unknown(msg) => {
                tracing::warn!("Unknown command: {}", msg);
                state.chat.add_error_message(msg.clone());
                state.emit_event(
                    "chat-message-received",
                    serde_json::json!({
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                        "username": null,
                        "message": msg,
                        "messageType": "error",
                    }),
                );
                return Err(msg);
            }
        }
        Ok(())
    } else {
        let chat_msg = ProtocolMessage::Chat {
            Chat: ProtocolChatMessage::Text(message.clone()),
        };
        send_to_server_arc(state, chat_msg)?;
        Ok(())
    }
}

fn send_to_server(
    state: &State<'_, Arc<AppState>>,
    message: ProtocolMessage,
) -> Result<(), String> {
    let connection = state.connection.lock().clone();
    let Some(connection) = connection else {
        return Err("Not connected to server".to_string());
    };
    connection
        .send(message)
        .map_err(|e| format!("Failed to send message: {}", e))
}

fn send_to_server_arc(state: &Arc<AppState>, message: ProtocolMessage) -> Result<(), String> {
    let connection = state.connection.lock().clone();
    let Some(connection) = connection else {
        return Err("Not connected to server".to_string());
    };
    connection
        .send(message)
        .map_err(|e| format!("Failed to send message: {}", e))
}
