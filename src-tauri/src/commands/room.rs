// Room command handlers

#[tauri::command]
pub async fn change_room(room: String) -> Result<(), String> {
    // TODO: Implement room change logic
    tracing::info!("Changing to room: {}", room);
    Ok(())
}

#[tauri::command]
pub async fn set_ready(is_ready: bool) -> Result<(), String> {
    // TODO: Implement ready state logic
    tracing::info!("Setting ready state to: {}", is_ready);
    Ok(())
}
