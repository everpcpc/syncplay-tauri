// Connection command handlers

use crate::app_state::{AppState, ConnectionStatusEvent};
use crate::network::connection::Connection;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn connect_to_server(
    host: String,
    port: u16,
    username: String,
    room: String,
    _password: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("Connecting to {}:{} as {} in room {}", host, port, username, room);

    // Check if already connected
    if state.is_connected() {
        return Err("Already connected to a server".to_string());
    }

    // Create new connection
    let mut connection = Connection::new();

    // Connect to server
    match connection.connect(host.clone(), port).await {
        Ok(_receiver) => {
            tracing::info!("Successfully connected to server");

            // Store connection
            *state.connection.lock() = Some(connection);

            // Update client state
            state.client_state.set_username(username);
            state.client_state.set_room(room);

            // Emit connection status event
            state.emit_event(
                "connection-status-changed",
                ConnectionStatusEvent {
                    connected: true,
                    server: Some(format!("{}:{}", host, port)),
                },
            );

            // TODO: Send Hello message
            // TODO: Start message processing loop

            Ok(())
        }
        Err(e) => {
            tracing::error!("Failed to connect: {}", e);
            Err(format!("Connection failed: {}", e))
        }
    }
}

#[tauri::command]
pub async fn disconnect_from_server(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    tracing::info!("Disconnecting from server");

    // Disconnect
    if let Some(mut connection) = state.connection.lock().take() {
        connection.disconnect();
    }

    // Emit connection status event
    state.emit_event(
        "connection-status-changed",
        ConnectionStatusEvent {
            connected: false,
            server: None,
        },
    );

    Ok(())
}

#[tauri::command]
pub async fn get_connection_status(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.is_connected())
}
