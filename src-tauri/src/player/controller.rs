use crate::app_state::{AppState, PlayerStateEvent};
use crate::config::{SyncplayConfig, UnpauseAction};
use crate::network::messages::{
    FileInfo, PlayState, ProtocolMessage, ReadyState, SetMessage, StateMessage,
};
use crate::player::backend::{player_kind_from_path_or_default, PlayerBackend, PlayerKind};
use crate::player::events::{EndFileReason, MpvPlayerEvent};
use crate::player::mpc_web::MpcWebBackend;
use crate::player::mplayer_slave::MplayerBackend;
use crate::player::mpv_backend::MpvBackend;
use crate::player::mpv_ipc::MpvIpc;
use crate::player::properties::PlayerState;
use crate::player::vlc_rc::VlcBackend;
use crate::utils::{
    apply_privacy, is_trustable_and_trusted, is_url, same_filename, PRIVACY_HIDDEN_FILENAME,
};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

pub async fn ensure_player_connected(state: &Arc<AppState>) -> Result<(), String> {
    if state.is_player_connected() {
        return Ok(());
    }

    let config = state.config.lock().clone();
    let player_path = resolve_player_path(&config);
    let kind = player_kind_from_path_or_default(&player_path);
    let args = build_player_arguments(&config, &player_path);
    {
        let mut process_guard = state.player_process.lock();
        if let Some(child) = process_guard.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                *process_guard = None;
            }
        }
    }

    let (backend, child) = match kind {
        PlayerKind::Mpv | PlayerKind::MpvNet | PlayerKind::Iina => {
            let child = start_mpv_process_if_needed(state, &config, &player_path, kind, &args)?;
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
            let backend = Arc::new(MpvBackend::new(kind, mpv)) as Arc<dyn PlayerBackend>;
            spawn_event_loop(state.clone(), event_rx);
            (backend, child)
        }
        PlayerKind::Vlc => {
            let (backend, child) =
                VlcBackend::start(&player_path, &args, None).await.map_err(|e| e.to_string())?;
            (Arc::new(backend) as Arc<dyn PlayerBackend>, Some(child))
        }
        PlayerKind::Mplayer => {
            let (backend, child) =
                MplayerBackend::start(&player_path, &args, None).await.map_err(|e| e.to_string())?;
            (Arc::new(backend) as Arc<dyn PlayerBackend>, Some(child))
        }
        PlayerKind::MpcHc | PlayerKind::MpcBe => {
            let (backend, child) = MpcWebBackend::start(kind, &player_path, &args, None)
                .await
                .map_err(|e| e.to_string())?;
            (Arc::new(backend) as Arc<dyn PlayerBackend>, child)
        }
        PlayerKind::Unknown => {
            return Err(format!("Unsupported player path: {}", player_path));
        }
    };

    *state.player.lock() = Some(backend);
    if let Some(child) = child {
        *state.player_process.lock() = Some(child);
    } else if !matches!(kind, PlayerKind::Mpv | PlayerKind::MpvNet | PlayerKind::Iina) {
        *state.player_process.lock() = None;
    }
    Ok(())
}

pub fn spawn_player_state_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut last_sent: Option<PlayerStateSnapshot> = None;
        let mut eof_sent = false;
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            interval.tick().await;
            let player = state.player.lock().clone();
            let Some(player) = player else { continue };
            if let Err(e) = player.poll_state().await {
                tracing::warn!("Failed to poll player state: {}", e);
            }
            let player_state = player.get_state();
            emit_player_state(&state, &player_state);

            if !state.is_connected() {
                continue;
            }

            if let Some(prev) = last_sent.as_ref() {
                if prev.paused == Some(true) && player_state.paused == Some(false) {
                    let suppressed = {
                        let mut guard = state.suppress_unpause_check.lock();
                        let suppressed = *guard;
                        if suppressed {
                            *guard = false;
                        }
                        suppressed
                    };
                    if !suppressed {
                        let config = state.config.lock().clone();
                        if !instaplay_conditions_met(&state, &config) {
                            if let Err(e) = player.set_paused(true).await {
                                tracing::warn!("Failed to block unpause: {}", e);
                            }
                            if !state.client_state.is_ready() {
                                if let Err(e) = send_ready_state(&state, true, true) {
                                    tracing::warn!("Failed to send ready state: {}", e);
                                }
                            }
                            continue;
                        }
                    }
                }
            }

            if file_info_changed(&player_state, last_sent.as_ref()) {
                eof_sent = false;
                let mut suppress_guard = state.suppress_next_file_update.lock();
                if *suppress_guard {
                    *suppress_guard = false;
                } else {
                    send_file_update(&state, &player_state);
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

            if !eof_sent {
                if let (Some(duration), Some(position)) =
                    (player_state.duration, player_state.position)
                {
                    if duration > 0.0 {
                        let threshold = if duration > 0.2 { duration - 0.2 } else { duration };
                        if position >= threshold {
                            eof_sent = true;
                            handle_end_of_file(&state).await;
                        }
                    }
                }
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
    if is_url(filename) {
        let (trustable, trusted) = is_trustable_and_trusted(
            filename,
            &config.user.trusted_domains,
            config.user.only_switch_to_trusted_domains,
        );
        if !trustable || !trusted {
            return Err("URL is not trusted".to_string());
        }
        ensure_player_connected(state).await?;
        let player = state
            .player
            .lock()
            .clone()
            .ok_or_else(|| "Player not connected".to_string())?;
        player.load_file(filename)
            .await
            .map_err(|e| format!("Failed to load URL: {}", e))?;
        state.client_state.set_file(Some(filename.to_string()));
        if send_update {
            let player_state = player.get_state();
            send_file_update(state, &player_state);
        } else {
            *state.suppress_next_file_update.lock() = true;
        }
        return Ok(());
    }

    let media_path = resolve_media_path(&config.player.media_directories, filename)
        .ok_or_else(|| format!("File not found in media directories: {}", filename))?;

    ensure_player_connected(state).await?;

    let player = state
        .player
        .lock()
        .clone()
        .ok_or_else(|| "Player not connected".to_string())?;
    player
        .load_file(media_path.to_string_lossy().as_ref())
        .await
        .map_err(|e| format!("Failed to load file: {}", e))?;

    state.client_state.set_file(Some(filename.to_string()));
    if send_update {
        let player_state = state
            .player
            .lock()
            .clone()
            .map(|player| player.get_state())
            .unwrap_or_default();
        send_file_update(state, &player_state);
    } else {
        *state.suppress_next_file_update.lock() = true;
    }

    Ok(())
}

pub fn resolve_media_path(media_directories: &[String], filename: &str) -> Option<PathBuf> {
    if filename == PRIVACY_HIDDEN_FILENAME {
        return None;
    }
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

    for directory in media_directories {
        let directory = directory.trim();
        if directory.is_empty() {
            continue;
        }
        let dir_path = Path::new(directory);
        let entries = match std::fs::read_dir(dir_path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let candidate_name = path.file_name()?.to_string_lossy();
            if same_filename(Some(filename), Some(candidate_name.as_ref())) {
                return Some(path);
            }
        }
    }

    None
}

fn resolve_player_path(config: &SyncplayConfig) -> String {
    let trimmed = config.player.player_path.trim();
    if trimmed.is_empty() || trimmed == "custom" {
        "mpv".to_string()
    } else {
        trimmed.to_string()
    }
}

fn build_player_arguments(config: &SyncplayConfig, player_path: &str) -> Vec<String> {
    let mut args = config.player.player_arguments.clone();
    if let Some(extra_args) = config.player.per_player_arguments.get(player_path) {
        args.extend(extra_args.clone());
    }
    args
}

fn find_iina_placeholder() -> Option<String> {
    let candidates = ["app-icon.png", "icon.svg", "app-icon.svg"];
    let cwd = std::env::current_dir().ok()?;
    for name in candidates {
        let path = cwd.join(name);
        if path.exists() {
            return Some(path.to_string_lossy().to_string());
        }
    }
    None
}

fn start_mpv_process_if_needed(
    state: &Arc<AppState>,
    config: &SyncplayConfig,
    player_path: &str,
    kind: PlayerKind,
    args: &[String],
) -> Result<Option<tokio::process::Child>, String> {
    let should_start = {
        let mut process_guard = state.player_process.lock();
        if let Some(child) = process_guard.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                *process_guard = None;
            } else {
                return Ok(None);
            }
        }
        process_guard.is_none()
    };

    if !should_start {
        return Ok(None);
    }

    let mut cmd = Command::new(player_path);
    let launch_args = args.to_vec();
    match kind {
        PlayerKind::Iina => {
            cmd.arg("--no-stdin")
                .arg(format!(
                    "--mpv-input-ipc-server={}",
                    config.player.mpv_socket_path
                ));
            if let Some(placeholder) = find_iina_placeholder() {
                cmd.arg(placeholder);
            }
        }
        _ => {
            cmd.arg("--idle=yes")
                .arg("--no-terminal")
                .arg(format!(
                    "--input-ipc-server={}",
                    config.player.mpv_socket_path
                ));
        }
    }
    cmd.args(&launch_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start player: {}", e))?;
    Ok(Some(child))
}

fn spawn_event_loop(
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<MpvPlayerEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let MpvPlayerEvent::EndFile {
                reason: EndFileReason::Eof,
            } = event
            {
                handle_end_of_file(&state).await;
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

fn send_file_update(state: &Arc<AppState>, player_state: &PlayerState) {
    if player_state.filename.is_none() {
        return;
    }
    let config = state.config.lock().clone();
    let raw_name = player_state.filename.clone();
    let raw_size = player_state
        .path
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len());
    let raw_duration = player_state.duration;

    let (name, size) = apply_privacy(
        raw_name.clone(),
        raw_size,
        &config.user.filename_privacy_mode,
        &config.user.filesize_privacy_mode,
    );

    state.client_state.set_file(raw_name);
    state.client_state.set_file_size(size.clone());
    state.client_state.set_file_duration(raw_duration);

    let Some(connection) = state.connection.lock().clone() else {
        return;
    };

    let message = ProtocolMessage::Set {
        Set: SetMessage {
            room: None,
            file: Some(FileInfo {
                name,
                size,
                duration: raw_duration,
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

fn file_info_changed(player_state: &PlayerState, last_sent: Option<&PlayerStateSnapshot>) -> bool {
    match last_sent {
        None => true,
        Some(prev) => {
            prev.filename != player_state.filename || prev.duration != player_state.duration
        }
    }
}

#[derive(Clone, Debug)]
struct PlayerStateSnapshot {
    filename: Option<String>,
    position: Option<f64>,
    paused: Option<bool>,
    duration: Option<f64>,
}

impl PlayerStateSnapshot {
    fn from(state: &PlayerState) -> Self {
        Self {
            filename: state.filename.clone(),
            position: state.position,
            paused: state.paused,
            duration: state.duration,
        }
    }
}

async fn handle_end_of_file(state: &Arc<AppState>) {
    let config = state.config.lock().clone();
    if !config.user.shared_playlist_enabled {
        return;
    }

    if let Some(next) = state
        .playlist
        .next_with_loop(config.user.loop_at_end_of_playlist)
    {
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
                    playlist_index: Some(crate::network::messages::PlaylistIndexUpdate {
                        user: Some(username),
                        index: Some(index),
                    }),
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
        if let Err(e) = load_media_by_name(state, &next.filename, true).await {
            tracing::warn!("Failed to load next item: {}", e);
        }
    } else if config.user.loop_single_files {
        if let Some(current) = state.playlist.get_current_item() {
            if let Err(e) = load_media_by_name(state, &current.filename, true).await {
                tracing::warn!("Failed to loop current item: {}", e);
            }
        } else if let Some(current) = state.client_state.get_file() {
            if let Err(e) = load_media_by_name(state, &current, true).await {
                tracing::warn!("Failed to loop current file: {}", e);
            }
        }
    }
}

fn instaplay_conditions_met(state: &Arc<AppState>, config: &SyncplayConfig) -> bool {
    match config.user.unpause_action {
        UnpauseAction::Always => true,
        UnpauseAction::IfAlreadyReady => state.client_state.is_ready(),
        UnpauseAction::IfOthersReady => {
            all_other_users_ready(state, &state.client_state.get_room())
        }
        UnpauseAction::IfMinUsersReady => {
            if !all_other_users_ready(state, &state.client_state.get_room()) {
                return false;
            }
            let min_users = config.user.autoplay_min_users;
            if min_users > 0 {
                let count = users_in_room_count(
                    state,
                    &state.client_state.get_room(),
                    &state.client_state.get_username(),
                );
                return count >= min_users as usize;
            }
            true
        }
    }
}

fn all_other_users_ready(state: &Arc<AppState>, room: &str) -> bool {
    let username = state.client_state.get_username();
    for user in state.client_state.get_users_in_room(room) {
        if user.username != username && !user.is_ready {
            return false;
        }
    }
    true
}

fn users_in_room_count(state: &Arc<AppState>, room: &str, username: &str) -> usize {
    let users = state.client_state.get_users_in_room(room);
    let mut count = users.len();
    if !users.iter().any(|user| user.username == username) {
        count += 1;
    }
    count
}

fn send_ready_state(
    state: &Arc<AppState>,
    is_ready: bool,
    manually_initiated: bool,
) -> Result<(), String> {
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
                manually_initiated: Some(manually_initiated),
                set_by: None,
            }),
            playlist_index: None,
            playlist_change: None,
            features: None,
        },
    };

    let Some(connection) = state.connection.lock().clone() else {
        return Err("Not connected to server".to_string());
    };
    connection
        .send(message)
        .map_err(|e| format!("Failed to send ready state: {}", e))
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
