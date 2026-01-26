// Chat command handlers

use crate::app_state::AppState;
use crate::client::chat::{ChatCommand, ChatManager};
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
                // TODO: Send room change to server
            }
            ChatCommand::List => {
                tracing::info!("Command: List users");
                let users = state.client_state.get_users();
                let user_list: Vec<String> = users
                    .iter()
                    .map(|u| format!("{} ({})", u.username, u.room))
                    .collect();
                state.chat.add_system_message(format!("Users: {}", user_list.join(", ")));
            }
            ChatCommand::Help => {
                tracing::info!("Command: Show help");
                let help = ChatCommand::help_text();
                state.chat.add_system_message(help);
            }
            ChatCommand::Ready => {
                tracing::info!("Command: Set ready");
                state.client_state.set_ready(true);
                // TODO: Send ready state to server
            }
            ChatCommand::Unready => {
                tracing::info!("Command: Set unready");
                state.client_state.set_ready(false);
                // TODO: Send ready state to server
            }
            ChatCommand::Unknown(msg) => {
                tracing::warn!("Unknown command: {}", msg);
                state.chat.add_error_message(msg.clone());
                return Err(msg);
            }
        }
        Ok(())
    } else {
        // Regular chat message
        let username = state.client_state.get_username();
        state.chat.add_user_message(username, message);
        // TODO: Send to server
        Ok(())
    }
}
