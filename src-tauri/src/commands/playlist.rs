// Playlist command handlers

#[tauri::command]
pub async fn update_playlist(action: String, filename: Option<String>) -> Result<(), String> {
    // TODO: Implement playlist update logic
    tracing::info!("Playlist action: {} for file: {:?}", action, filename);
    Ok(())
}
