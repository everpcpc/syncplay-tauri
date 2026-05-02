// Playlist command handlers

use crate::app_state::{AppState, PlaylistEvent};
use crate::config::SyncplayConfig;
use crate::network::messages::{PlayState, StateMessage};
use crate::network::messages::{PlaylistChange, PlaylistIndexUpdate, ProtocolMessage, SetMessage};
use crate::player::controller::{load_media_by_name, resolve_media_path};
use crate::utils::{is_music_file, is_url};
use rand::seq::SliceRandom;
use rand::thread_rng;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn update_playlist(
    action: String,
    filename: Option<String>,
    items: Option<Vec<String>>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("Playlist action: {} for file: {:?}", action, filename);
    let config = state.config.lock().clone();
    if !shared_playlists_enabled(state.inner(), &config) {
        return Err("Shared playlists are disabled".to_string());
    }
    let current_items = state.playlist.get_item_filenames();
    let mut new_items = current_items.clone();

    match action.as_str() {
        "add" => {
            let file = filename.ok_or_else(|| "Filename required for add action".to_string())?;
            let (normalized, override_path) = normalize_playlist_entry(&file);
            if let Some(path) = override_path {
                state.media_index.add_override_path(&normalized, path);
            }
            new_items.push(normalized);
            apply_playlist_change_local(state.inner(), new_items, false)?;
        }
        "remove" => {
            let index_str =
                filename.ok_or_else(|| "Index required for remove action".to_string())?;
            let index = index_str
                .parse::<usize>()
                .map_err(|_| "Invalid index for remove action".to_string())?;
            if index >= new_items.len() {
                return Err("Invalid index for remove action".to_string());
            }
            new_items.remove(index);
            apply_playlist_change_local(state.inner(), new_items, false)?;
        }
        "clear" => {
            new_items.clear();
            apply_playlist_change_local(state.inner(), new_items, false)?;
        }
        "select" => {
            let index_str =
                filename.ok_or_else(|| "Index required for select action".to_string())?;
            let index = index_str
                .parse::<usize>()
                .map_err(|_| "Invalid index for select action".to_string())?;
            if index >= new_items.len() {
                return Err("Invalid index for select action".to_string());
            }
            send_playlist_index(state.inner(), index, true)?;
            if let Err(e) = apply_playlist_index_from_server(state.inner(), index, true).await {
                tracing::warn!("Failed to load selected playlist item: {}", e);
            }
        }
        "next" => {
            let index = next_index(state.inner(), &config)?;
            send_playlist_index(state.inner(), index, true)?;
            if let Err(e) = apply_playlist_index_from_server(state.inner(), index, true).await {
                tracing::warn!("Failed to load next playlist item: {}", e);
            }
        }
        "previous" => {
            let index = previous_index(state.inner())?;
            send_playlist_index(state.inner(), index, true)?;
            if let Err(e) = apply_playlist_index_from_server(state.inner(), index, true).await {
                tracing::warn!("Failed to load previous playlist item: {}", e);
            }
        }
        "undo" => {
            if let Some(previous) = state.playlist.previous_playlist() {
                apply_playlist_change_local(state.inner(), previous, false)?;
            }
        }
        "shuffle" => {
            new_items.shuffle(&mut thread_rng());
            apply_playlist_change_local(state.inner(), new_items, true)?;
            if !state.playlist.get_item_filenames().is_empty() {
                if let Err(e) = apply_playlist_index_from_server(state.inner(), 0, true).await {
                    tracing::warn!("Failed to load shuffled playlist start: {}", e);
                }
            }
        }
        "shuffle_remaining" => {
            let Some(current_index) = state.playlist.get_current_index() else {
                return Ok(());
            };
            let split_point = current_index + 1;
            if split_point < new_items.len() {
                let mut tail = new_items.split_off(split_point);
                tail.shuffle(&mut thread_rng());
                new_items.extend(tail);
            }
            apply_playlist_change_local(state.inner(), new_items, false)?;
        }
        "load" => {
            let path = filename.ok_or_else(|| "Path required for load action".to_string())?;
            let contents = std::fs::read_to_string(&path)
                .map_err(|_| "Failed to read playlist file".to_string())?;
            let items: Vec<String> = contents
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect();
            if items.is_empty() {
                return Err("Playlist file is empty".to_string());
            }
            apply_playlist_change_local(state.inner(), items, true)?;
        }
        "reorder" => {
            let items = items.ok_or_else(|| "Items required for reorder action".to_string())?;
            apply_playlist_change_local(state.inner(), items, false)?;
        }
        "save" => {
            let path = filename.ok_or_else(|| "Path required for save action".to_string())?;
            let contents = current_items.join("\n");
            std::fs::write(&path, contents)
                .map_err(|_| "Failed to save playlist file".to_string())?;
        }
        _ => {
            return Err(format!("Unknown playlist action: {}", action));
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn check_playlist_items(
    items: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<PlaylistItemInfo>, String> {
    let config = state.config.lock().clone();
    let mut results = Vec::with_capacity(items.len());
    for item in items {
        let path = if is_url(&item) {
            Some(item.clone())
        } else {
            state
                .media_index
                .resolve_path(&item)
                .or_else(|| resolve_media_path(&config.player.media_directories, &item))
                .map(|path| path.to_string_lossy().to_string())
        };
        let available = path.is_some();
        results.push(PlaylistItemInfo {
            filename: item,
            path,
            available,
        });
    }
    Ok(results)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlaylistItemInfo {
    pub filename: String,
    pub path: Option<String>,
    pub available: bool,
}

pub(crate) fn shared_playlists_enabled(state: &Arc<AppState>, config: &SyncplayConfig) -> bool {
    config.user.shared_playlist_enabled && state.server_features.lock().shared_playlists
}

pub(crate) fn send_playlist_index(
    state: &Arc<AppState>,
    index: usize,
    reset_position: bool,
) -> Result<(), String> {
    state.playlist.set_current_index(index);
    emit_playlist_update(state);

    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: None,
            playlist_index: Some(PlaylistIndexUpdate {
                user: None,
                index: Some(index),
            }),
            playlist_change: None,
            controller_auth: None,
            new_controlled_room: None,
            features: None,
        }),
    };
    send_to_server(state, message)?;

    if reset_position {
        *state.last_advance_time.lock() = Some(std::time::Instant::now());
        *state.last_rewind_time.lock() = Some(std::time::Instant::now());
        let state_message = ProtocolMessage::State {
            State: StateMessage {
                playstate: Some(PlayState {
                    position: 0.0,
                    paused: true,
                    do_seek: None,
                    set_by: None,
                }),
                ping: None,
                ignoring_on_the_fly: None,
            },
        };
        let _ = send_to_server(state, state_message);
    }

    Ok(())
}

pub(crate) async fn apply_playlist_index_from_server(
    state: &Arc<AppState>,
    index: usize,
    reset_position: bool,
) -> Result<(), String> {
    state.playlist.set_current_index(index);
    let filename = state.playlist.get_current_filename();
    state.playlist.set_queued_index_filename(filename.clone());
    emit_playlist_update(state);

    if let Some(filename) = filename {
        if let Err(e) = load_media_by_name(state, &filename, reset_position, false).await {
            let message = format!("Failed to load playlist item '{}': {}", filename, e);
            tracing::warn!("{}", message);
            crate::commands::connection::emit_error_message(state, &message);
        }
    }

    Ok(())
}

pub(crate) async fn change_playlist_from_filename(
    state: &Arc<AppState>,
    filename: &str,
) -> Result<(), String> {
    let config = state.config.lock().clone();
    if !shared_playlists_enabled(state, &config) {
        return Ok(());
    }

    let normalized =
        crate::utils::playlist_filename_from_path(filename).unwrap_or_else(|| filename.to_string());
    let Some(index) = state.playlist.index_of_filename(&normalized) else {
        return Ok(());
    };

    if state.playlist.get_current_index() != Some(index) {
        send_playlist_index(state, index, true)?;
        return Ok(());
    }

    if state.playlist.get_queued_index_filename().as_deref() == Some(&normalized) {
        return Ok(());
    }

    if let Err(e) = crate::player::controller::rewind_player(state).await {
        tracing::warn!("Failed to rewind after playlist match: {}", e);
    }

    Ok(())
}

fn apply_playlist_change_local(
    state: &Arc<AppState>,
    new_items: Vec<String>,
    reset_index: bool,
) -> Result<(), String> {
    let room = state.client_state.get_room();
    state.playlist.set_queued_index_filename(None);
    state.playlist.update_previous_playlist(&new_items, &room);
    let previous_index = state.playlist.get_current_index();

    let new_index = if new_items.is_empty() {
        None
    } else if reset_index {
        Some(0)
    } else {
        Some(state.playlist.compute_valid_index(&new_items))
    };

    state
        .playlist
        .set_items_with_index(new_items.clone(), new_index);

    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: None,
            playlist_index: None,
            playlist_change: Some(PlaylistChange {
                user: None,
                files: new_items,
            }),
            controller_auth: None,
            new_controlled_room: None,
            features: None,
        }),
    };
    send_to_server(state, message)?;

    if should_send_playlist_index(previous_index, new_index, reset_index) {
        let Some(index) = new_index else {
            emit_playlist_update(state);
            return Ok(());
        };
        send_playlist_index(state, index, false)?;
    } else {
        emit_playlist_update(state);
    }

    Ok(())
}

fn should_send_playlist_index(
    previous_index: Option<usize>,
    next_index: Option<usize>,
    reset_index: bool,
) -> bool {
    next_index.is_some() && (reset_index || previous_index != next_index)
}

fn normalize_playlist_entry(entry: &str) -> (String, Option<PathBuf>) {
    if is_url(entry) {
        return (entry.to_string(), None);
    }
    let path = Path::new(entry);
    if path.is_absolute() && path.is_file() {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            return (name.to_string(), Some(path.to_path_buf()));
        }
    }
    (entry.to_string(), None)
}

fn next_index(state: &Arc<AppState>, config: &SyncplayConfig) -> Result<usize, String> {
    let items = state.playlist.get_item_filenames();
    if items.is_empty() {
        return Err("Playlist is empty".to_string());
    }
    let current = state.playlist.get_current_index().unwrap_or(0);
    let loop_at_end = config.user.loop_at_end_of_playlist || is_playing_music(state);
    if current + 1 < items.len() {
        return Ok(current + 1);
    }
    if loop_at_end {
        return Ok(0);
    }
    Err("Already at end of playlist".to_string())
}

fn previous_index(state: &Arc<AppState>) -> Result<usize, String> {
    let items = state.playlist.get_item_filenames();
    if items.is_empty() {
        return Err("Playlist is empty".to_string());
    }
    let current = state.playlist.get_current_index().unwrap_or(0);
    if current == 0 {
        return Err("Already at start of playlist".to_string());
    }
    Ok(current - 1)
}

fn is_playing_music(state: &Arc<AppState>) -> bool {
    state
        .client_state
        .get_file()
        .as_deref()
        .map(is_music_file)
        .unwrap_or(false)
}

fn emit_playlist_update(state: &Arc<AppState>) {
    let items = state.playlist.get_item_filenames();
    state.emit_event(
        "playlist-updated",
        PlaylistEvent {
            items,
            current_index: state.playlist.get_current_index(),
        },
    );
}

fn send_to_server(state: &Arc<AppState>, message: ProtocolMessage) -> Result<(), String> {
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
    fn unchanged_playlist_index_does_not_need_broadcast() {
        assert!(!should_send_playlist_index(Some(0), Some(0), false));
    }

    #[test]
    fn changed_playlist_index_needs_broadcast() {
        assert!(should_send_playlist_index(Some(1), Some(0), false));
    }

    #[test]
    fn reset_playlist_index_needs_broadcast() {
        assert!(should_send_playlist_index(Some(0), Some(0), true));
    }

    #[test]
    fn empty_playlist_index_does_not_need_broadcast() {
        assert!(!should_send_playlist_index(Some(0), None, false));
    }
}
