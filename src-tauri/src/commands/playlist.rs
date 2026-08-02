// Playlist command handlers

use crate::app_state::AppState;
use crate::client::playback_runtime;
use crate::config::SyncplayConfig;
use crate::network::messages::{PlayState, StateMessage};
use crate::network::messages::{PlaylistChange, PlaylistIndexUpdate, ProtocolMessage, SetMessage};
use crate::player::controller::resolve_media_path;
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
    match action.as_str() {
        "add" => {
            let file = filename.ok_or_else(|| "Filename required for add action".to_string())?;
            let (normalized, override_path) = normalize_playlist_entry(&file);
            if let Some(path) = override_path {
                state.media_index.add_override_path(&normalized, path);
            }
            apply_playlist_edit_local(state.inner(), false, move |items, _| {
                items.push(normalized);
                Ok(())
            })
            .await?;
        }
        "remove" => {
            let index_str =
                filename.ok_or_else(|| "Index required for remove action".to_string())?;
            let index = index_str
                .parse::<usize>()
                .map_err(|_| "Invalid index for remove action".to_string())?;
            apply_playlist_edit_local(state.inner(), false, move |items, _| {
                if index >= items.len() {
                    return Err("Invalid index for remove action".to_string());
                }
                items.remove(index);
                Ok(())
            })
            .await?;
        }
        "clear" => {
            apply_playlist_edit_local(state.inner(), false, |items, _| {
                items.clear();
                Ok(())
            })
            .await?;
        }
        "select" => {
            let index_str =
                filename.ok_or_else(|| "Index required for select action".to_string())?;
            let index = index_str
                .parse::<usize>()
                .map_err(|_| "Invalid index for select action".to_string())?;
            playback_runtime::local_select(state.inner(), index, true).await?;
        }
        "next" => {
            let loop_at_end =
                config.user.loop_at_end_of_playlist || is_playing_music(state.inner());
            playback_runtime::local_step(
                state.inner(),
                playback_runtime::PlaylistStep::Next { loop_at_end },
                true,
            )
            .await?;
        }
        "previous" => {
            playback_runtime::local_step(
                state.inner(),
                playback_runtime::PlaylistStep::Previous,
                true,
            )
            .await?;
        }
        "undo" => {
            if state.playlist.previous_playlist().is_some() {
                let playlist = state.playlist.clone();
                apply_playlist_edit_local(state.inner(), false, move |items, _| {
                    if let Some(previous) = playlist.previous_playlist() {
                        *items = previous;
                    }
                    Ok(())
                })
                .await?;
            }
        }
        "shuffle" => {
            apply_playlist_edit_local(state.inner(), true, |items, _| {
                items.shuffle(&mut thread_rng());
                Ok(())
            })
            .await?;
        }
        "shuffle_remaining" => {
            apply_playlist_edit_local(state.inner(), false, |items, current_index| {
                let Some(current_index) = current_index else {
                    return Ok(());
                };
                let split_point = current_index + 1;
                if split_point < items.len() {
                    let mut tail = items.split_off(split_point);
                    tail.shuffle(&mut thread_rng());
                    items.extend(tail);
                }
                Ok(())
            })
            .await?;
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
            apply_playlist_edit_local(state.inner(), true, move |current, _| {
                *current = items;
                Ok(())
            })
            .await?;
        }
        "reorder" => {
            let items = items.ok_or_else(|| "Items required for reorder action".to_string())?;
            apply_playlist_edit_local(state.inner(), false, move |current, _| {
                *current = items;
                Ok(())
            })
            .await?;
        }
        "save" => {
            let path = filename.ok_or_else(|| "Path required for save action".to_string())?;
            let contents = state.playback.snapshot().playlist_items.join("\n");
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

async fn apply_playlist_edit_local(
    state: &Arc<AppState>,
    reset_index: bool,
    edit: impl FnOnce(&mut Vec<String>, Option<usize>) -> Result<(), String>,
) -> Result<playback_runtime::PlaylistEditResult, String> {
    playback_runtime::edit_playlist_and_publish(state, reset_index, edit, |result| {
        let message = ProtocolMessage::Set {
            Set: Box::new(SetMessage {
                room: None,
                file: None,
                user: None,
                ready: None,
                playlist_index: None,
                playlist_change: Some(PlaylistChange {
                    user: None,
                    files: result.items.clone(),
                }),
                controller_auth: None,
                new_controlled_room: None,
                features: None,
            }),
        };
        send_to_server(state, message)?;
        if let Some(index) = result.index {
            send_playlist_index(state, index, false)?;
        }
        Ok(())
    })
    .await
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

fn is_playing_music(state: &Arc<AppState>) -> bool {
    state
        .client_state
        .get_file()
        .as_deref()
        .map(is_music_file)
        .unwrap_or(false)
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
    use crate::network::connection::Connection;
    use crate::network::fake_server::FakeSyncplayServer;

    async fn fixture() -> (Arc<AppState>, FakeSyncplayServer) {
        let state = AppState::new();
        let server = FakeSyncplayServer::start().await.unwrap();
        let connection = Arc::new(Connection::new());
        let (_receiver, _peer) = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();
        *state.connection.lock() = Some(connection);
        playback_runtime::replace_playlist(
            &state,
            vec!["a.mkv".into(), "b.mkv".into(), "c.mkv".into()],
            Some(1),
        )
        .await
        .unwrap();
        (state, server)
    }

    #[tokio::test]
    async fn current_item_removal_sends_index_even_when_its_number_is_unchanged() {
        let (state, mut server) = fixture().await;

        apply_playlist_edit_local(&state, false, |items, _| {
            items.remove(1);
            Ok(())
        })
        .await
        .unwrap();

        let playlist = server.next_received().await.unwrap();
        let index = server.next_received().await.unwrap();
        assert!(matches!(
            playlist,
            ProtocolMessage::Set { Set }
                if Set.playlist_change.is_some() && Set.playlist_index.is_none()
        ));
        assert!(matches!(
            index,
            ProtocolMessage::Set { Set }
                if Set.playlist_index.as_ref().and_then(|update| update.index) == Some(1)
        ));
    }

    #[tokio::test]
    async fn concurrent_edits_publish_complete_transactions_in_coordinator_order() {
        let (state, mut server) = fixture().await;
        let first_state = state.clone();
        let first = tokio::spawn(async move {
            apply_playlist_edit_local(&first_state, false, |items, _| {
                items.push("d.mkv".to_string());
                Ok(())
            })
            .await
        });
        let second_state = state.clone();
        let second = tokio::spawn(async move {
            apply_playlist_edit_local(&second_state, false, |items, _| {
                items.push("e.mkv".to_string());
                Ok(())
            })
            .await
        });
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();

        let mut playlist_lengths = Vec::new();
        for pair in 0..2 {
            let playlist = server.next_received().await.unwrap();
            let index = server.next_received().await.unwrap();
            let ProtocolMessage::Set { Set } = playlist else {
                panic!("expected playlist message for transaction {pair}");
            };
            playlist_lengths.push(Set.playlist_change.unwrap().files.len());
            assert!(matches!(
                index,
                ProtocolMessage::Set { Set } if Set.playlist_index.is_some()
            ));
        }
        assert_eq!(playlist_lengths, vec![4, 5]);
    }
}
