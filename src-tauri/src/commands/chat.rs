// Chat command handlers

use crate::app_state::AppState;
use crate::client::chat::ChatCommand;
use crate::commands::connection::{
    reidentify_as_controller, reset_room_sync_state, set_authoritative_room, store_control_password,
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
    if message.is_empty() {
        return Ok(());
    }

    let config = state.config.lock().clone();
    if !config.user.chat_input_enabled {
        return Err("Chat input is disabled".to_string());
    }
    if !state.server_features.lock().chat {
        return Err("Chat is disabled by the server".to_string());
    }

    let sanitized = message.replace(['\n', '\r'], "");
    let (message, command) = if let Some(escaped) = sanitized.strip_prefix("//") {
        (format!("/{escaped}"), None)
    } else if sanitized.starts_with('/') && sanitized != "/" {
        (sanitized.clone(), ChatCommand::parse(&sanitized))
    } else {
        (sanitized, None)
    };
    let message = if command.is_none() {
        let max_length = state
            .server_features
            .lock()
            .max_chat_message_length
            .unwrap_or(150);
        truncate_text(&message, max_length)
    } else {
        message
    };
    tracing::info!("Sending chat message: {}", message);

    if !state.is_connected() {
        return Err("Not connected to server".to_string());
    }

    if let Some(command) = command {
        match command {
            ChatCommand::Room(room) => {
                tracing::info!("Command: Change room to {}", room);
                let (room, control_password) = parse_controlled_room_input(&room);
                if let Some(password) = control_password {
                    store_control_password(state, &room, &password, true);
                }
                set_authoritative_room(state, room);
                reset_room_sync_state(state).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::connection::Connection;
    use crate::network::fake_server::FakeSyncplayServer;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn outbound_chat_preserves_whitespace_and_escapes_double_slash() {
        let state = AppState::new();
        state.server_features.lock().chat = true;
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let connection = Arc::new(Connection::new());
        let (_receiver, _) = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();
        connection.set_authenticated();
        *state.connection.lock() = Some(connection);

        for (input, expected) in [
            ("  hello  ", "  hello  "),
            ("//ready", "/ready"),
            ("/", "/"),
        ] {
            send_chat_message_inner(&state, input).await.unwrap();
            let message = timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                message,
                ProtocolMessage::Chat {
                    Chat: ProtocolChatMessage::Text(message)
                } if message == expected
            ));
        }

        server.close();
    }

    #[tokio::test]
    async fn chat_truncates_unicode_but_room_command_sends_the_complete_name() {
        let state = AppState::new();
        {
            let mut features = state.server_features.lock();
            features.chat = true;
            features.max_chat_message_length = Some(2);
            features.max_room_name_length = Some(3);
        }
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let connection = Arc::new(Connection::new());
        let (_receiver, _) = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();
        connection.set_authenticated();
        *state.connection.lock() = Some(connection);

        send_chat_message_inner(&state, "测试😀").await.unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Chat {
                Chat: ProtocolChatMessage::Text(message)
            } if message == "测试"
        ));

        let room = "这是一个远远超过服务端展示上限的完整房间名";
        send_chat_message_inner(&state, &format!("/room {room}"))
            .await
            .unwrap();
        let message = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            message,
            ProtocolMessage::Set { Set }
                if Set.room.as_ref().map(|room| room.name.as_str()) == Some(room)
        ));

        server.close();
    }
}
