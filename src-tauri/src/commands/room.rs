// Room command handlers

use crate::app_state::AppState;
use crate::commands::connection::{
    emit_system_message, is_readiness_supported, reidentify_as_controller, reset_room_sync_state,
    send_controller_auth, set_authoritative_room, store_control_password,
};
use crate::config::save_config;
use crate::network::messages::{ProtocolMessage, ReadyState, RoomInfo, SetMessage};
use crate::utils::parse_controlled_room_input;
use rand::Rng;
use std::sync::Arc;
use tauri::{AppHandle, Runtime, State};

#[tauri::command]
pub async fn change_room<R: Runtime>(
    room: String,
    app: AppHandle<R>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("Changing to room: {}", room);

    // Check if connected
    if !state.is_connected() {
        return Err("Not connected to server".to_string());
    }
    let (room, control_password) = parse_controlled_room_input(&room);
    if let Some(password) = control_password {
        store_control_password(state.inner(), &room, &password, true);
    }

    // Update client state
    set_authoritative_room(state.inner(), room.clone());
    reset_room_sync_state(state.inner()).await;

    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: Some(RoomInfo {
                name: room.clone(),
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
    send_to_server(&state, message)?;
    send_to_server(&state, ProtocolMessage::List { List: None })?;
    reidentify_as_controller(state.inner());

    let config = state.config.lock().clone();
    if config.user.autosave_joins_to_list {
        let mut updated = config.clone();
        if !updated.user.room_list.contains(&room) {
            updated.user.room_list.push(room.clone());
        }
        updated.user.default_room = room.clone();
        if let Err(e) = save_config(&app, &updated) {
            tracing::warn!("Failed to save config after room change: {}", e);
        }
        *state.config.lock() = updated.clone();
        state.emit_event("config-updated", updated);
    }

    Ok(())
}

#[tauri::command]
pub async fn set_ready(is_ready: bool, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    tracing::info!("Setting ready state to: {}", is_ready);

    // Check if connected
    if !state.is_connected() {
        return Err("Not connected to server".to_string());
    }
    if !is_readiness_supported(state.inner(), false) {
        return Err("Readiness is not supported by this server".to_string());
    }

    // Update client state
    state.client_state.set_ready(is_ready);

    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: Some(ReadyState {
                username: None,
                is_ready: Some(is_ready),
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
    send_to_server(&state, message)?;

    Ok(())
}

#[tauri::command]
pub async fn create_managed_room(
    room: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    ensure_managed_rooms_supported(state.inner())?;
    if room.is_empty() {
        return Err("Room name cannot be empty".to_string());
    }

    let password = generate_control_password();
    *state.last_control_password_attempt.lock() = Some(password.clone());
    send_controller_auth(state.inner(), &room, &password)
}

#[tauri::command]
pub async fn identify_as_controller(
    password: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    ensure_managed_rooms_supported(state.inner())?;
    let password = crate::utils::strip_control_password(&password);
    if password.is_empty() {
        return Err("Controller password cannot be empty".to_string());
    }

    let room = state.client_state.get_room();
    if room.is_empty() {
        return Err("Not in a room".to_string());
    }

    emit_system_message(
        state.inner(),
        &format!(
            "Identifying as room operator with password '{}'...",
            password
        ),
    );
    *state.last_control_password_attempt.lock() = Some(password.clone());
    send_controller_auth(state.inner(), &room, &password)
}

fn ensure_managed_rooms_supported(state: &Arc<AppState>) -> Result<(), String> {
    if !state.is_connected() {
        return Err("Not connected to server".to_string());
    }
    if state.client_state.get_server_version().is_none() {
        return Err("Server login is not complete".to_string());
    }
    if !state.server_features.lock().managed_rooms {
        return Err("Managed rooms are not supported by this server".to_string());
    }
    Ok(())
}

fn generate_control_password() -> String {
    let mut rng = rand::thread_rng();
    let first = rng.gen_range(b'A'..=b'Z') as char;
    let second = rng.gen_range(b'A'..=b'Z') as char;
    let first_number = rng.gen_range(0..=999);
    let second_number = rng.gen_range(0..=999);
    format!("{first}{second}-{first_number:03}-{second_number:03}")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_control_password_matches_reference_format() {
        for _ in 0..32 {
            let password = generate_control_password();
            let bytes = password.as_bytes();
            assert_eq!(bytes.len(), 10);
            assert!(bytes[0].is_ascii_uppercase());
            assert!(bytes[1].is_ascii_uppercase());
            assert_eq!(bytes[2], b'-');
            assert!(bytes[3..6].iter().all(u8::is_ascii_digit));
            assert_eq!(bytes[6], b'-');
            assert!(bytes[7..10].iter().all(u8::is_ascii_digit));
        }
    }
}
