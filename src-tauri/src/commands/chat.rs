// Chat command handlers

use crate::app_state::AppState;
use crate::client::chat::ChatCommand;
use crate::network::messages::ProtocolMessage;
use crate::network::messages::{
    ChatMessage as ProtocolChatMessage, ReadyState, RoomInfo, SetMessage,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn send_chat_message(
    message: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("Sending chat message: {}", message);

    // Check if connected
    if !state.is_connected() {
        return Err("Not connected to server".to_string());
    }

    // Check if it's a command
    if let Some(command) = ChatCommand::parse(&message) {
        match command {
            ChatCommand::Room(room) => {
                tracing::info!("Command: Change room to {}", room);
                state.client_state.set_room(room);
                let set_msg = ProtocolMessage::Set {
                    Set: SetMessage {
                        room: Some(RoomInfo {
                            name: state.client_state.get_room(),
                            password: None,
                        }),
                        file: None,
                        user: None,
                        ready: None,
                        playlist_index: None,
                        playlist_change: None,
                        features: None,
                    },
                };
                send_to_server(&state, set_msg)?;
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
                state.client_state.set_ready(true);
                let username = state.client_state.get_username();
                let set_msg = ProtocolMessage::Set {
                    Set: SetMessage {
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
                        features: None,
                    },
                };
                send_to_server(&state, set_msg)?;
            }
            ChatCommand::Unready => {
                tracing::info!("Command: Set unready");
                state.client_state.set_ready(false);
                let username = state.client_state.get_username();
                let set_msg = ProtocolMessage::Set {
                    Set: SetMessage {
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
                        features: None,
                    },
                };
                send_to_server(&state, set_msg)?;
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
        // Regular chat message
        let chat_msg = ProtocolMessage::Chat {
            Chat: ProtocolChatMessage::Text(message.clone()),
        };
        send_to_server(&state, chat_msg)?;
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
