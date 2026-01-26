// Chat command handlers

use crate::client::chat::{ChatCommand, ChatManager};

#[tauri::command]
pub async fn send_chat_message(message: String) -> Result<(), String> {
    tracing::info!("Sending chat message: {}", message);

    // Check if it's a command
    if let Some(command) = ChatCommand::parse(&message) {
        match command {
            ChatCommand::Room(room) => {
                tracing::info!("Command: Change room to {}", room);
                // TODO: Implement room change
            }
            ChatCommand::List => {
                tracing::info!("Command: List users");
                // TODO: Implement user list
            }
            ChatCommand::Help => {
                tracing::info!("Command: Show help");
                let help = ChatCommand::help_text();
                tracing::info!("{}", help);
                // TODO: Display help in UI
            }
            ChatCommand::Ready => {
                tracing::info!("Command: Set ready");
                // TODO: Implement ready state
            }
            ChatCommand::Unready => {
                tracing::info!("Command: Set unready");
                // TODO: Implement unready state
            }
            ChatCommand::Unknown(msg) => {
                tracing::warn!("Unknown command: {}", msg);
                return Err(msg);
            }
        }
        Ok(())
    } else {
        // Regular chat message
        // TODO: Send to server
        Ok(())
    }
}
