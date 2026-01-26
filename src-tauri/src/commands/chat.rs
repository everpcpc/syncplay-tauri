// Chat command handlers

#[tauri::command]
pub async fn send_chat_message(message: String) -> Result<(), String> {
    // TODO: Implement chat message sending
    tracing::info!("Sending chat message: {}", message);
    Ok(())
}
