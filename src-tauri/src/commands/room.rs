// Room command handlers

use crate::app_state::AppState;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn change_room(room: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    tracing::info!("Changing to room: {}", room);

    // Check if connected
    if !state.is_connected() {
        return Err("Not connected to server".to_string());
    }

    // Update client state
    state.client_state.set_room(room.clone());

    // TODO: Send room change message to server

    Ok(())
}

#[tauri::command]
pub async fn set_ready(is_ready: bool, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    tracing::info!("Setting ready state to: {}", is_ready);

    // Check if connected
    if !state.is_connected() {
        return Err("Not connected to server".to_string());
    }

    // Update client state
    state.client_state.set_ready(is_ready);

    // TODO: Send ready state to server

    Ok(())
}
