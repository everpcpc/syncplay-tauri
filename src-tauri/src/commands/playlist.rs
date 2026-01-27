// Playlist command handlers

use crate::app_state::{AppState, PlaylistEvent};
use crate::network::messages::{PlaylistChange, PlaylistIndexUpdate, ProtocolMessage, SetMessage};
use crate::player::controller::load_media_by_name;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn update_playlist(
    action: String,
    filename: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("Playlist action: {} for file: {:?}", action, filename);
    let previous_index = state.playlist.get_current_index();

    match action.as_str() {
        "add" => {
            if let Some(file) = filename {
                state.playlist.add_item(file);
            } else {
                return Err("Filename required for add action".to_string());
            }
        }
        "remove" => {
            if let Some(index_str) = filename {
                if let Ok(index) = index_str.parse::<usize>() {
                    state.playlist.remove_item(index);
                } else {
                    return Err("Invalid index for remove action".to_string());
                }
            } else {
                return Err("Index required for remove action".to_string());
            }
        }
        "next" => {
            state.playlist.next();
        }
        "previous" => {
            state.playlist.previous();
        }
        "clear" => {
            state.playlist.clear();
        }
        _ => {
            return Err(format!("Unknown playlist action: {}", action));
        }
    }

    let items: Vec<String> = state
        .playlist
        .get_items()
        .iter()
        .map(|item| item.filename.clone())
        .collect();
    let current_index = state.playlist.get_current_index();

    let username = state.client_state.get_username();
    if matches!(action.as_str(), "add" | "remove" | "clear") {
        let message = ProtocolMessage::Set {
            Set: SetMessage {
                room: None,
                file: None,
                user: None,
                ready: None,
                playlist_index: None,
                playlist_change: Some(PlaylistChange {
                    user: Some(username.clone()),
                    files: items.clone(),
                }),
                features: None,
            },
        };
        send_to_server(&state, message)?;
    }

    if current_index != previous_index {
        if let Some(index) = current_index {
            let message = ProtocolMessage::Set {
                Set: SetMessage {
                    room: None,
                    file: None,
                    user: None,
                    ready: None,
                    playlist_index: Some(PlaylistIndexUpdate {
                        user: Some(username.clone()),
                        index: Some(index),
                    }),
                    playlist_change: None,
                    features: None,
                },
            };
            send_to_server(&state, message)?;
        }
    }

    if current_index != previous_index {
        if let Some(item) = state.playlist.get_current_item() {
            if let Err(e) = load_media_by_name(state.inner(), &item.filename, true).await {
                tracing::warn!("Failed to load playlist item: {}", e);
            }
        }
    }

    // Emit playlist update event
    state.emit_event(
        "playlist-updated",
        PlaylistEvent {
            items,
            current_index,
        },
    );

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
