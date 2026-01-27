use crate::app_state::{AppState, PlayerStateEvent};
use crate::config::SyncplayConfig;
use crate::network::messages::{FileInfo, PlayState, ProtocolMessage, SetMessage, StateMessage};
use crate::player::events::{EndFileReason, MpvPlayerEvent};
use crate::player::mpv_ipc::MpvIpc;
use crate::player::properties::PlayerState;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

pub async fn ensure_mpv_connected(state: &Arc<AppState>) -> Result<(), String> {
    if state.is_mpv_connected() {
        return Ok(());
    }

    let config = state.config.lock().clone();
    start_mpv_process_if_needed(state, &config)?;

    let mut mpv = MpvIpc::new(config.player.mpv_socket_path.clone());
    let mut attempts = 0;
    let event_rx = loop {
        match mpv.connect().await {
            Ok(rx) => break rx,
            Err(e) => {
                attempts += 1;
                if attempts >= 10 {
                    return Err(format!("Failed to connect to mpv IPC: {}", e));
                }
                sleep(Duration::from_millis(200)).await;
            }
        }
    };

    let mpv = Arc::new(mpv);
    *state.mpv.lock() = Some(mpv.clone());

    spawn_event_loop(state.clone(), event_rx);
    Ok(())
}

pub fn spawn_player_state_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut last_sent: Option<PlayerStateSnapshot> = None;
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            interval.tick().await;
            let mpv = state.mpv.lock().clone();
            let Some(mpv) = mpv else { continue };

            let player_state = mpv.get_state();
            emit_player_state(&state, &player_state);

            if !state.is_connected() {
                continue;
            }

            let filename_changed = last_sent
                .as_ref()
                .map(|prev| prev.filename != player_state.filename)
                .unwrap_or(true);

            if filename_changed {
                let mut suppress_guard = state.suppress_next_file_update.lock();
                if *suppress_guard {
                    *suppress_guard = false;
                } else if let Some(filename) = player_state.filename.as_ref() {
                    send_file_update(&state, filename);
                }
            }

            if should_send_state(&player_state, last_sent.as_ref()) {
                if let Some(play_state) = to_play_state(&state, &player_state) {
                    let state_msg = ProtocolMessage::State {
                        State: StateMessage {
                            playstate: Some(play_state),
                            ping: None,
                            ignoring_on_the_fly: None,
                        },
                    };
                    if let Some(connection) = state.connection.lock().clone() {
                        if let Err(e) = connection.send(state_msg) {
                            tracing::warn!("Failed to send state update: {}", e);
                        }
                    }
                }
                last_sent = Some(PlayerStateSnapshot::from(&player_state));
            }
        }
    });
}

pub async fn load_media_by_name(
    state: &Arc<AppState>,
    filename: &str,
    send_update: bool,
) -> Result<(), String> {
    let config = state.config.lock().clone();
    let media_path = resolve_media_path(&config.player.media_directories, filename)
        .ok_or_else(|| format!("File not found in media directories: {}", filename))?;

    ensure_mpv_connected(state).await?;

    let mpv = state
        .mpv
        .lock()
        .clone()
        .ok_or_else(|| "MPV not connected".to_string())?;
    mpv.load_file(media_path.to_string_lossy().as_ref())
        .await
        .map_err(|e| format!("Failed to load file: {}", e))?;

    state.client_state.set_file(Some(filename.to_string()));
    if send_update {
        send_file_update(state, filename);
    } else {
        *state.suppress_next_file_update.lock() = true;
    }

    Ok(())
}

pub fn resolve_media_path(media_directories: &[String], filename: &str) -> Option<PathBuf> {
    for directory in media_directories {
        let directory = directory.trim();
        if directory.is_empty() {
            continue;
        }
        let candidate = Path::new(directory).join(filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn start_mpv_process_if_needed(
    state: &Arc<AppState>,
    config: &SyncplayConfig,
) -> Result<(), String> {
    if state.mpv_process.lock().is_some() {
        return Ok(());
    }

    let player_path = if config.player.player_path.trim().is_empty() {
        "mpv"
    } else {
        config.player.player_path.as_str()
    };

    let mut cmd = Command::new(player_path);
    cmd.arg("--idle=yes")
        .arg("--no-terminal")
        .arg(format!(
            "--input-ipc-server={}",
            config.player.mpv_socket_path
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start mpv: {}", e))?;
    *state.mpv_process.lock() = Some(child);
    Ok(())
}

fn spawn_event_loop(
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<MpvPlayerEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                MpvPlayerEvent::EndFile {
                    reason: EndFileReason::Eof,
                } => {
                    if let Some(next) = state.playlist.next() {
                        let items: Vec<String> = state
                            .playlist
                            .get_items()
                            .iter()
                            .map(|item| item.filename.clone())
                            .collect();
                        state.emit_event(
                            "playlist-updated",
                            crate::app_state::PlaylistEvent {
                                items,
                                current_index: state.playlist.get_current_index(),
                            },
                        );
                        if let Some(index) = state.playlist.get_current_index() {
                            let username = state.client_state.get_username();
                            let message = ProtocolMessage::Set {
                                Set: SetMessage {
                                    room: None,
                                    file: None,
                                    user: None,
                                    ready: None,
                                    playlist_index: Some(
                                        crate::network::messages::PlaylistIndexUpdate {
                                            user: Some(username),
                                            index: Some(index),
                                        },
                                    ),
                                    playlist_change: None,
                                    features: None,
                                },
                            };
                            if let Some(connection) = state.connection.lock().clone() {
                                if let Err(e) = connection.send(message) {
                                    tracing::warn!("Failed to send playlist index update: {}", e);
                                }
                            }
                        }
                        if let Err(e) = load_media_by_name(&state, &next.filename, true).await {
                            tracing::warn!("Failed to load next item: {}", e);
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

fn emit_player_state(state: &Arc<AppState>, player_state: &PlayerState) {
    state.emit_event(
        "player-state-changed",
        PlayerStateEvent {
            filename: player_state.filename.clone(),
            position: player_state.position,
            duration: player_state.duration,
            paused: player_state.paused,
            speed: player_state.speed,
        },
    );
}

fn send_file_update(state: &Arc<AppState>, filename: &str) {
    state.client_state.set_file(Some(filename.to_string()));
    let Some(connection) = state.connection.lock().clone() else {
        return;
    };
    let message = ProtocolMessage::Set {
        Set: SetMessage {
            room: None,
            file: Some(FileInfo {
                name: Some(filename.to_string()),
                size: None,
                duration: None,
            }),
            user: None,
            ready: None,
            playlist_index: None,
            playlist_change: None,
            features: None,
        },
    };
    if let Err(e) = connection.send(message) {
        tracing::warn!("Failed to send file update: {}", e);
    }
}

fn to_play_state(state: &Arc<AppState>, player_state: &PlayerState) -> Option<PlayState> {
    let position = player_state.position?;
    let paused = player_state.paused?;
    let username = state.client_state.get_username();

    Some(PlayState {
        position,
        paused,
        do_seek: None,
        set_by: Some(username),
    })
}

fn should_send_state(player_state: &PlayerState, last_sent: Option<&PlayerStateSnapshot>) -> bool {
    match last_sent {
        None => true,
        Some(prev) => {
            if player_state.paused != prev.paused || player_state.filename != prev.filename {
                return true;
            }
            match (player_state.position, prev.position) {
                (Some(current), Some(last)) => (current - last).abs() >= 0.5,
                _ => false,
            }
        }
    }
}

#[derive(Clone, Debug)]
struct PlayerStateSnapshot {
    filename: Option<String>,
    position: Option<f64>,
    paused: Option<bool>,
}

impl PlayerStateSnapshot {
    fn from(state: &PlayerState) -> Self {
        Self {
            filename: state.filename.clone(),
            position: state.position,
            paused: state.paused,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_media_path;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_resolve_media_path_multiple_directories() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        let filename = "movie.mp4";
        let file_path = dir2.path().join(filename);
        fs::write(&file_path, b"test").unwrap();

        let directories = vec![
            dir1.path().to_string_lossy().to_string(),
            dir2.path().to_string_lossy().to_string(),
        ];
        let resolved = resolve_media_path(&directories, filename).unwrap();
        assert_eq!(resolved, file_path);
    }

    #[test]
    fn test_resolve_media_path_empty() {
        let directories: Vec<String> = Vec::new();
        assert!(resolve_media_path(&directories, "file.mp4").is_none());
    }
}
