// Room command handlers

use crate::app_state::AppState;
use crate::network::messages::{ProtocolMessage, ReadyState, RoomInfo, SetMessage};
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

    let message = ProtocolMessage::Set {
        Set: SetMessage {
            room: Some(RoomInfo {
                name: room,
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
    send_to_server(&state, message)?;

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

    let username = state.client_state.get_username();
    let message = ProtocolMessage::Set {
        Set: SetMessage {
            room: None,
            file: None,
            user: None,
            ready: Some(ReadyState {
                username: Some(username),
                is_ready: Some(is_ready),
                manually_initiated: Some(true),
                set_by: None,
            }),
            playlist_index: None,
            playlist_change: None,
            features: None,
        },
    };
    send_to_server(&state, message)?;

    Ok(())
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
