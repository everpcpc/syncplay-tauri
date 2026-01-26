// Playlist command handlers

use crate::app_state::{AppState, PlaylistEvent};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn update_playlist(
    action: String,
    filename: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("Playlist action: {} for file: {:?}", action, filename);

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

    // Emit playlist update event
    let items: Vec<String> = state
        .playlist
        .get_items()
        .iter()
        .map(|item| item.filename.clone())
        .collect();

    state.emit_event(
        "playlist-updated",
        PlaylistEvent {
            items,
            current_index: state.playlist.get_current_index(),
        },
    );

    Ok(())
}
