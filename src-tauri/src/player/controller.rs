use crate::app_state::{AppState, PlayerStateEvent};
use crate::client::media_index::{resolve_exact_in_directory, resolve_similar_in_directory};
use crate::commands::playlist::{
    apply_playlist_index_from_server, change_playlist_from_filename, send_playlist_index,
    shared_playlists_enabled,
};
use crate::config::{SyncplayConfig, UnpauseAction};
use crate::network::messages::{FileInfo, PlayState, ProtocolMessage, ReadyState, SetMessage};
use crate::player::backend::{player_kind_from_path_or_default, PlayerBackend, PlayerKind};
use crate::player::mpc_api::MpcApiBackend;
use crate::player::mplayer_slave::MplayerBackend;
use crate::player::mpv_backend::MpvBackend;
use crate::player::mpv_ipc::MpvIpc;
use crate::player::properties::PlayerState;
use crate::player::vlc_syncplay::VlcSyncplayBackend;
use crate::utils::{
    apply_privacy, is_music_file, is_trustable_and_trusted, is_url, same_filename, truncate_text,
    PRIVACY_HIDDEN_FILENAME,
};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tauri::Manager;
#[cfg(unix)]
use tempfile::Builder;
use tokio::process::Command;
use tokio::time::{sleep, Duration};
use tracing::info;
use url::Url;

const PROTOCOL_TIMEOUT_SECONDS: f64 = 12.5;
const RECENT_REWIND_THRESHOLD_SECONDS: f64 = 5.0;
const RECENT_ADVANCE_GRACE_SECONDS: f64 = 8.0;
const LAST_PAUSED_DIFF_THRESHOLD_SECONDS: f64 = 2.0;
const PLAYLIST_LOAD_NEXT_FILE_MINIMUM_LENGTH: f64 = 10.0;
const PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD: f64 = 5.0;
const DOUBLE_CHECK_REWIND: bool = true;
const DOUBLE_CHECK_REWIND_POSITION_THRESHOLD: f64 = 5.0;
const DOUBLE_CHECK_REWIND_DELAYS: [f64; 3] = [0.5, 1.0, 1.5];
const RECENT_REWIND_FILE_UPDATE_SHIFT_SECONDS: f64 = 4.5;
const FILE_UPDATE_AFTER_LOAD_DELAY_MS: u64 = 200;

struct PlayerConnectingGuard<'a> {
    flag: &'a parking_lot::Mutex<bool>,
}

impl<'a> PlayerConnectingGuard<'a> {
    fn new(flag: &'a parking_lot::Mutex<bool>) -> Self {
        Self { flag }
    }
}

impl<'a> Drop for PlayerConnectingGuard<'a> {
    fn drop(&mut self) {
        *self.flag.lock() = false;
    }
}

pub async fn ensure_player_connected(state: &Arc<AppState>) -> Result<(), String> {
    if state.is_player_connected() {
        return Ok(());
    }
    {
        let mut guard = state.player_connecting.lock();
        if *guard {
            return Ok(());
        }
        *guard = true;
    }
    let _connecting_guard = PlayerConnectingGuard::new(&state.player_connecting);

    let config = state.config.lock().clone();
    let player_path = resolve_player_path(&config);
    let kind = player_kind_from_path_or_default(&player_path);
    let args = build_player_arguments(&config, &player_path);
    let socket_path = ensure_mpv_socket_path(state)?;
    let syncplayintf_path = resolve_syncplayintf_path(state);
    {
        let mut process_guard = state.player_process.lock();
        if let Some(child) = process_guard.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                *process_guard = None;
            }
        }
    }

    let should_spawn = should_spawn_player(state, kind);
    if should_spawn {
        *state.last_player_spawn.lock() = Some(Instant::now());
        *state.last_player_kind.lock() = Some(kind);
    }
    let (backend, child) = match kind {
        PlayerKind::Mpv | PlayerKind::MpvNet | PlayerKind::Iina => {
            let mut child = None;
            if should_spawn {
                if kind == PlayerKind::Iina {
                    let mut last_error = None;
                    for _ in 0..3 {
                        let spawned = start_mpv_process_if_needed(
                            state,
                            &player_path,
                            kind,
                            &args,
                            &socket_path,
                            syncplayintf_path.as_ref(),
                        )?;
                        match spawned {
                            Some(mut spawned_child) => {
                                match wait_for_ipc_socket(
                                    &mut spawned_child,
                                    &socket_path,
                                    Duration::from_secs(10),
                                )
                                .await
                                {
                                    Ok(()) => {
                                        child = Some(spawned_child);
                                        break;
                                    }
                                    Err(e) => {
                                        last_error = Some(e);
                                        let _ = spawned_child.kill().await;
                                        let _ = spawned_child.wait().await;
                                    }
                                }
                            }
                            None => {
                                child = None;
                                break;
                            }
                        }
                        sleep(Duration::from_millis(200)).await;
                    }
                    if child.is_none() {
                        if let Some(error) = last_error {
                            return Err(error);
                        }
                    }
                } else {
                    child = start_mpv_process_if_needed(
                        state,
                        &player_path,
                        kind,
                        &args,
                        &socket_path,
                        syncplayintf_path.as_ref(),
                    )?;
                }
            }
            let mut mpv = MpvIpc::new(socket_path.clone());
            let mut attempts = 0;
            let max_attempts = if kind == PlayerKind::Iina { 50 } else { 10 };
            let event_rx = loop {
                match mpv.connect().await {
                    Ok(rx) => break rx,
                    Err(e) => {
                        attempts += 1;
                        if attempts >= max_attempts {
                            return Err(format!("Failed to connect to mpv IPC: {}", e));
                        }
                        sleep(Duration::from_millis(200)).await;
                    }
                }
            };
            let stdout = child.as_mut().and_then(|process| process.stdout.take());
            let osc_compatible = match kind {
                PlayerKind::Iina | PlayerKind::MpvNet => true,
                _ => check_mpv_version(&player_path)?.osc_visibility_change_compatible,
            };
            let backend = Arc::new(MpvBackend::new(
                kind,
                mpv,
                Arc::downgrade(state),
                osc_compatible,
                stdout,
            ));
            backend.spawn_event_loop(event_rx);
            let backend_dyn: Arc<dyn PlayerBackend> = backend.clone();
            (backend_dyn, child)
        }
        PlayerKind::Vlc => {
            let (backend, child) = if should_spawn {
                let lua_path = resolve_syncplay_lua_path(state)
                    .ok_or_else(|| "Syncplay VLC interface not found".to_string())?;
                VlcSyncplayBackend::start(&player_path, &args, None, lua_path)
                    .await
                    .map_err(|e| e.to_string())?
            } else {
                return Err("Player not running".to_string());
            };
            (Arc::new(backend) as Arc<dyn PlayerBackend>, Some(child))
        }
        PlayerKind::Mplayer => {
            let (backend, child) = if should_spawn {
                MplayerBackend::start(&player_path, &args, None)
                    .await
                    .map_err(|e| e.to_string())?
            } else {
                return Err("Player not running".to_string());
            };
            (Arc::new(backend) as Arc<dyn PlayerBackend>, Some(child))
        }
        PlayerKind::MpcHc | PlayerKind::MpcBe => {
            let (backend, child) = if should_spawn {
                let mut mpc_args = args.clone();
                if !mpc_args.iter().any(|arg| arg.eq_ignore_ascii_case("/open")) {
                    mpc_args.push("/open".to_string());
                }
                if !mpc_args.iter().any(|arg| arg.eq_ignore_ascii_case("/new")) {
                    mpc_args.push("/new".to_string());
                }
                MpcApiBackend::start(kind, &player_path, &mpc_args, None)
                    .await
                    .map_err(|e| e.to_string())?
            } else {
                return Err("Player not running".to_string());
            };
            (Arc::new(backend) as Arc<dyn PlayerBackend>, child)
        }
        PlayerKind::Unknown => {
            return Err(format!("Unsupported player path: {}", player_path));
        }
    };

    prepare_player_after_connect(&backend).await;

    *state.player.lock() = Some(backend);
    if !should_spawn && child.is_some() {
        *state.last_player_spawn.lock() = Some(Instant::now());
        *state.last_player_kind.lock() = Some(kind);
    }
    if let Some(child) = child {
        *state.player_process.lock() = Some(child);
    } else if !matches!(
        kind,
        PlayerKind::Mpv | PlayerKind::MpvNet | PlayerKind::Iina
    ) {
        *state.player_process.lock() = None;
    }
    Ok(())
}

async fn prepare_player_after_connect(player: &Arc<dyn PlayerBackend>) {
    if should_pause_on_prepare(player.kind()) {
        if let Err(e) = player.set_paused(true).await {
            tracing::warn!("Failed to pause player during startup: {}", e);
        }
    }
}

fn should_pause_on_prepare(kind: PlayerKind) -> bool {
    matches!(
        kind,
        PlayerKind::Mpv | PlayerKind::MpvNet | PlayerKind::Iina | PlayerKind::Mplayer
    )
}

pub async fn restart_player(state: &Arc<AppState>) -> Result<(), String> {
    stop_player(state).await?;
    ensure_player_connected(state).await
}

pub async fn stop_player(state: &Arc<AppState>) -> Result<(), String> {
    let player = state.player.lock().clone();
    *state.player.lock() = None;
    *state.last_player_spawn.lock() = None;
    *state.last_player_kind.lock() = None;
    if let Some(player) = player {
        if let Err(e) = player.shutdown().await {
            tracing::warn!("Failed to shutdown player: {}", e);
        }
    }
    let child = {
        let mut guard = state.player_process.lock();
        guard.take()
    };
    if let Some(mut child) = child {
        if let Err(e) = child.kill().await {
            tracing::warn!("Failed to stop player process: {}", e);
        }
        let _ = child.wait().await;
    }
    Ok(())
}

pub fn spawn_player_state_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut last_observed: Option<PlayerStateSnapshot> = None;
        let mut eof_sent = false;
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            let player = state.player.lock().clone();
            let Some(player) = player else { continue };
            if let Err(e) = player.poll_state().await {
                tracing::warn!("Failed to poll player state: {}", e);
            }
            let player_state = player.get_state();
            emit_player_state(&state, &player_state);

            if state.is_connected() && check_protocol_timeout(&state) {
                continue;
            }

            if !state.is_connected() {
                last_observed = Some(PlayerStateSnapshot::from(&player_state));
                continue;
            }

            let is_placeholder = is_placeholder_file(&state, &player_state);

            if !is_placeholder && file_info_changed(&player_state, last_observed.as_ref()) {
                eof_sent = false;
                let mut suppress_guard = state.suppress_next_file_update.lock();
                if *suppress_guard {
                    *suppress_guard = false;
                } else {
                    send_file_update(&state, &player_state);
                }
                if matches!(player.kind(), PlayerKind::MpcHc | PlayerKind::MpcBe) {
                    sync_mpc_after_file_change(state.clone(), player.clone());
                } else if matches!(player.kind(), PlayerKind::Vlc | PlayerKind::Mplayer) {
                    sync_generic_after_file_change(state.clone(), player.clone());
                }
            }

            if let (Some(position), Some(paused_value)) =
                (player_state.position, player_state.paused)
            {
                let global = state.client_state.get_global_state();
                let (mut local_pause_change, local_seeked) = {
                    let mut local_state = state.local_playback_state.lock();
                    let (pause_change, seeked) = local_state.update_from_player(
                        position,
                        paused_value,
                        global.position,
                        global.paused,
                    );
                    (pause_change, seeked)
                };
                if local_seeked {
                    *state.last_seek_from_position.lock() = Some(global.position);
                }
                let mut paused = paused_value;
                let mut skip_ready_toggle = false;
                if local_pause_change && paused {
                    let current_length = state.client_state.get_file_duration().unwrap_or(0.0);
                    let near_end = current_length > PLAYLIST_LOAD_NEXT_FILE_MINIMUM_LENGTH
                        && (position - current_length).abs()
                            < PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD;
                    if near_end {
                        skip_ready_toggle = true;
                        let _ = advance_playlist_check(&state, position).await;
                    }
                }
                if local_pause_change
                    && !local_seeked
                    && is_readiness_supported(&state, false)
                    && !skip_ready_toggle
                {
                    let (adjusted_change, adjusted_paused) =
                        apply_ready_toggle(&state, &player, paused, global.paused).await;
                    local_pause_change = adjusted_change;
                    paused = adjusted_paused;
                }

                if !is_placeholder
                    && state.last_global_update.lock().is_some()
                    && (local_pause_change || local_seeked)
                {
                    let latency_calculation = *state.last_latency_calculation.lock();
                    let play_state = if recently_rewound(&state) || recently_advanced(&state) {
                        let global_state = state.client_state.get_global_state();
                        PlayState {
                            position: global_state.position,
                            paused,
                            do_seek: None,
                            set_by: None,
                        }
                    } else {
                        PlayState {
                            position,
                            paused,
                            do_seek: if local_seeked { Some(true) } else { None },
                            set_by: None,
                        }
                    };
                    if let Err(e) = crate::commands::connection::send_state_message(
                        &state,
                        Some(play_state),
                        latency_calculation,
                        local_pause_change || local_seeked,
                    ) {
                        tracing::warn!("Failed to send state update: {}", e);
                    }
                }
            }

            last_observed = Some(PlayerStateSnapshot::from(&player_state));

            if !eof_sent {
                if let (Some(duration), Some(position)) =
                    (player_state.duration, player_state.position)
                {
                    if duration > 0.0 {
                        let threshold = if duration > 0.2 {
                            duration - 0.2
                        } else {
                            duration
                        };
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
    reset_position: bool,
    suppress_update: bool,
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
        if reset_position {
            player.mark_reset(true);
        }
        player
            .load_file(filename)
            .await
            .map_err(|e| format!("Failed to load URL: {}", e))?;
        state.client_state.set_file(Some(filename.to_string()));
        *state.last_updated_file_time.lock() = Some(std::time::Instant::now());
        state.playlist.opened_file();
        if reset_position {
            rewind_player(state).await?;
            crate::commands::connection::evaluate_autoplay(state);
        }
        if suppress_update {
            *state.suppress_next_file_update.lock() = true;
        } else {
            schedule_file_update_after_load(state.clone());
        }
        return Ok(());
    }

    let media_path = state
        .media_index
        .resolve_path(filename)
        .or_else(|| resolve_media_path(&config.player.media_directories, filename))
        .ok_or_else(|| format!("File not found in media directories: {}", filename))?;
    state.media_index.remember_resolved_path(&media_path);

    ensure_player_connected(state).await?;

    let player = state
        .player
        .lock()
        .clone()
        .ok_or_else(|| "Player not connected".to_string())?;
    if reset_position {
        player.mark_reset(false);
    }
    player
        .load_file(media_path.to_string_lossy().as_ref())
        .await
        .map_err(|e| format!("Failed to load file: {}", e))?;

    state.client_state.set_file(Some(filename.to_string()));
    *state.last_updated_file_time.lock() = Some(std::time::Instant::now());
    state.playlist.opened_file();
    if reset_position {
        rewind_player(state).await?;
        crate::commands::connection::evaluate_autoplay(state);
    }
    if suppress_update {
        *state.suppress_next_file_update.lock() = true;
    } else {
        schedule_file_update_after_load(state.clone());
    }

    Ok(())
}

fn schedule_file_update_after_load(state: Arc<AppState>) {
    tokio::spawn(async move {
        sleep(Duration::from_millis(FILE_UPDATE_AFTER_LOAD_DELAY_MS)).await;
        let player = state.player.lock().clone();
        let Some(player) = player else { return };
        if let Err(e) = player.poll_state().await {
            tracing::warn!("Failed to refresh player state after load: {}", e);
        }
        let player_state = player.get_state();
        if is_placeholder_file(&state, &player_state) {
            return;
        }
        if player_state.filename.is_none() && player_state.path.is_none() {
            return;
        }
        send_file_update(&state, &player_state);
    });
}

fn sync_mpc_after_file_change(state: Arc<AppState>, player: Arc<dyn PlayerBackend>) {
    tokio::spawn(async move {
        let global = state.client_state.get_global_state();
        for _ in 0..3 {
            let _ = player.set_paused(true).await;
            sleep(Duration::from_millis(10)).await;
        }
        sleep(Duration::from_millis(50)).await;
        let _ = player.set_paused(global.paused).await;
        let _ = player.set_position(global.position).await;
    });
}

fn sync_generic_after_file_change(state: Arc<AppState>, player: Arc<dyn PlayerBackend>) {
    tokio::spawn(async move {
        let global = state.client_state.get_global_state();
        let _ = player.set_paused(global.paused).await;
        let _ = player.set_position(global.position).await;
    });
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
        if let Some(path) = resolve_exact_in_directory(Path::new(directory), filename) {
            return Some(path);
        }
    }

    for directory in media_directories {
        let directory = directory.trim();
        if directory.is_empty() {
            continue;
        }
        if let Some(path) = resolve_similar_in_directory(Path::new(directory), filename) {
            return Some(path);
        }
    }

    None
}

pub async fn load_placeholder_if_empty(state: &Arc<AppState>) -> Result<(), String> {
    let placeholder =
        resolve_placeholder_path(state).ok_or_else(|| "Placeholder asset not found".to_string())?;
    let player = state
        .player
        .lock()
        .clone()
        .ok_or_else(|| "Player not connected".to_string())?;
    let player_state = player.get_state();
    if player_state.filename.is_some() {
        return Ok(());
    }
    *state.suppress_next_file_update.lock() = true;
    player
        .load_file(placeholder.to_string_lossy().as_ref())
        .await
        .map_err(|e| format!("Failed to load placeholder: {}", e))?;
    Ok(())
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

fn ensure_mpv_socket_path(state: &Arc<AppState>) -> Result<String, String> {
    if let Some(path) = state.mpv_socket_path.lock().clone() {
        return Ok(path);
    }

    #[cfg(windows)]
    {
        let name = build_windows_pipe_name();
        *state.mpv_socket_path.lock() = Some(name.clone());
        Ok(name)
    }

    #[cfg(unix)]
    {
        let runtime_dir =
            create_runtime_dir().map_err(|e| format!("Failed to create runtime dir: {}", e))?;
        let socket_path = runtime_dir
            .path()
            .join("mpv-socket")
            .to_string_lossy()
            .to_string();
        *state.mpv_runtime_dir.lock() = Some(runtime_dir);
        *state.mpv_socket_path.lock() = Some(socket_path.clone());
        Ok(socket_path)
    }
}

#[cfg(unix)]
fn create_runtime_dir() -> Result<tempfile::TempDir, std::io::Error> {
    let mut builder = Builder::new();
    builder.prefix("syncplay-");
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return builder.tempdir_in(dir);
        }
    }
    builder.tempdir()
}

#[cfg(windows)]
fn build_windows_pipe_name() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("\\\\.\\pipe\\syncplay-mpv-{}-{}", pid, nanos)
}

fn resolve_placeholder_path(state: &AppState) -> Option<PathBuf> {
    let candidates = [
        "resources/placeholder.png",
        "placeholder.png",
        "src-tauri/resources/placeholder.png",
        "icon.svg",
    ];
    if let Some(handle) = state.app_handle.lock().clone() {
        for name in candidates {
            if let Ok(path) = handle
                .path()
                .resolve(name, tauri::path::BaseDirectory::Resource)
            {
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }
    let cwd = std::env::current_dir().ok()?;
    for name in candidates {
        let path = cwd.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn resolve_syncplay_lua_path(state: &AppState) -> Option<PathBuf> {
    let candidates = [
        "resources/syncplay.lua",
        "syncplay.lua",
        "src-tauri/resources/syncplay.lua",
    ];
    if let Some(handle) = state.app_handle.lock().clone() {
        for name in candidates {
            if let Ok(path) = handle
                .path()
                .resolve(name, tauri::path::BaseDirectory::Resource)
            {
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }
    let cwd = std::env::current_dir().ok()?;
    for name in candidates {
        let path = cwd.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn resolve_syncplayintf_path(state: &AppState) -> Option<PathBuf> {
    let candidates = [
        "resources/syncplayintf.lua",
        "syncplayintf.lua",
        "src-tauri/resources/syncplayintf.lua",
    ];
    if let Some(handle) = state.app_handle.lock().clone() {
        for name in candidates {
            if let Ok(path) = handle
                .path()
                .resolve(name, tauri::path::BaseDirectory::Resource)
            {
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }
    let cwd = std::env::current_dir().ok()?;
    for name in candidates {
        let path = cwd.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

struct MpvVersionFlags {
    osc_visibility_change_compatible: bool,
}

fn check_mpv_version(player_path: &str) -> Result<MpvVersionFlags, String> {
    let Ok(output) = run_mpv_version_command(player_path) else {
        return Ok(MpvVersionFlags {
            osc_visibility_change_compatible: false,
        });
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_mpv_version_flags(&stdout)
}

fn parse_mpv_version_flags(stdout: &str) -> Result<MpvVersionFlags, String> {
    let re = Regex::new(r"mpv\s+(\d+)\.(\d+)\.").map_err(|e| e.to_string())?;
    if let Some(captures) = re.captures(stdout) {
        let major = captures
            .get(1)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(0);
        let minor = captures
            .get(2)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(0);
        if major == 0 && minor < 23 {
            return Err(
                "This version of mpv is not compatible with Syncplay. Please use mpv >= 0.23.0."
                    .to_string(),
            );
        }
        let osc_visibility_change_compatible = major > 0 || minor >= 28;
        return Ok(MpvVersionFlags {
            osc_visibility_change_compatible,
        });
    }
    Ok(MpvVersionFlags {
        osc_visibility_change_compatible: false,
    })
}

fn run_mpv_version_command(player_path: &str) -> std::io::Result<std::process::Output> {
    let mut command = std::process::Command::new(player_path);
    command.arg("--version");
    configure_hidden_version_command(&mut command);
    command.output()
}

#[cfg(target_os = "windows")]
fn configure_hidden_version_command(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_hidden_version_command(_command: &mut std::process::Command) {}

fn should_spawn_player(state: &AppState, kind: PlayerKind) -> bool {
    if kind != PlayerKind::Iina {
        return true;
    }
    let now = Instant::now();
    let last_spawn = *state.last_player_spawn.lock();
    let last_kind = *state.last_player_kind.lock();
    let recent = last_spawn
        .map(|instant| now.duration_since(instant) < Duration::from_secs(15))
        .unwrap_or(false);
    !(recent && last_kind == Some(PlayerKind::Iina))
}

fn start_mpv_process_if_needed(
    state: &Arc<AppState>,
    player_path: &str,
    kind: PlayerKind,
    args: &[String],
    socket_path: &str,
    syncplayintf_path: Option<&PathBuf>,
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
    cmd.env_remove("TERM");
    let launch_args = args.to_vec();
    let mut full_args = Vec::new();
    let term_playing_msg = "<SyncplayUpdateFile>\nANS_filename=${filename}\nANS_length=${=duration:${=length:0}}\nANS_path=${path}\n</SyncplayUpdateFile>";
    match kind {
        PlayerKind::Iina => {
            let has_sub_auto = launch_args
                .iter()
                .any(|arg| arg.starts_with("--mpv-sub-auto") || arg.starts_with("--sub-auto"));
            let has_sid = launch_args
                .iter()
                .any(|arg| arg.starts_with("--mpv-sid") || arg.starts_with("--sid"));
            full_args.push("--no-stdin".to_string());
            if let Some(placeholder) = resolve_placeholder_path(state) {
                full_args.push(placeholder.to_string_lossy().to_string());
            } else {
                tracing::warn!("Placeholder asset not found for player startup");
            }
            full_args.push("--mpv-keep-open=always".to_string());
            full_args.push("--mpv-keep-open-pause=yes".to_string());
            full_args.push("--mpv-idle=yes".to_string());
            full_args.push("--mpv-input-terminal=no".to_string());
            full_args.push("--mpv-hr-seek=always".to_string());
            full_args.push("--mpv-force-window=yes".to_string());
            full_args.push(format!("--mpv-input-ipc-server={}", socket_path));
            full_args.push(format!("--mpv-term-playing-msg={}", term_playing_msg));
            if !has_sub_auto {
                full_args.push("--mpv-sub-auto=fuzzy".to_string());
            }
            if !has_sid {
                full_args.push("--mpv-sid=auto".to_string());
            }
            if let Some(script_path) = syncplayintf_path {
                full_args.push(format!("--mpv-script={}", script_path.to_string_lossy()));
            }
        }
        _ => {
            full_args.push("--force-window=yes".to_string());
            full_args.push("--idle=yes".to_string());
            full_args.push("--keep-open=always".to_string());
            full_args.push("--keep-open-pause=yes".to_string());
            full_args.push("--hr-seek=always".to_string());
            full_args.push("--input-terminal=no".to_string());
            full_args.push(format!("--input-ipc-server={}", socket_path));
            full_args.push(format!("--term-playing-msg={}", term_playing_msg));
            if let Some(script_path) = syncplayintf_path {
                full_args.push(format!("--script={}", script_path.to_string_lossy()));
            }
            if kind == PlayerKind::MpvNet {
                full_args.push("--auto-load-folder=no".to_string());
            }
        }
    }
    full_args.extend(launch_args.clone());
    cmd.args(&full_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    info!(
        "Starting player: kind={:?}, path={}, socket={}, args={:?}",
        kind, player_path, socket_path, full_args
    );
    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start player: {}", e))?;
    Ok(Some(child))
}

async fn wait_for_ipc_socket(
    child: &mut tokio::process::Child,
    socket_path: &str,
    timeout: Duration,
) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if Path::new(socket_path).exists() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                return Err(format!("Player exited with status {}", status));
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err("Timed out waiting for MPV IPC socket".to_string())
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

pub(crate) fn send_file_update(state: &Arc<AppState>, player_state: &PlayerState) {
    if player_state.filename.is_none() && player_state.path.is_none() {
        return;
    }
    let config = state.config.lock().clone();
    let raw_path = player_state.path.clone();
    let local_path = raw_path.as_deref().and_then(normalize_local_path);
    let raw_name = if let Some(path) = raw_path.as_deref() {
        if let Some(local_path) = local_path.as_ref() {
            local_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string())
                .or_else(|| player_state.filename.clone())
        } else if is_url(path) {
            Some(path.to_string())
        } else {
            player_state.filename.clone().or_else(|| {
                Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.to_string())
            })
        }
    } else {
        let filename = player_state.filename.as_deref();
        if let Some(filename) = filename {
            if is_url(filename) {
                Some(filename.to_string())
            } else {
                return;
            }
        } else {
            return;
        }
    };
    let raw_size = if let Some(local_path) = local_path.as_ref() {
        match std::fs::metadata(local_path) {
            Ok(metadata) => Some(metadata.len()),
            Err(_) => Some(0),
        }
    } else if raw_path.as_deref().map(is_url).unwrap_or(false) {
        Some(0)
    } else {
        raw_name.as_deref().filter(|name| is_url(name)).map(|_| 0)
    };
    let raw_duration = player_state.duration;

    let max_len = state
        .server_features
        .lock()
        .max_filename_length
        .unwrap_or(250);
    let outbound_name = raw_name.clone().map(|name| truncate_text(&name, max_len));
    let (name, size) = apply_privacy(
        outbound_name,
        raw_size,
        &config.user.filename_privacy_mode,
        &config.user.filesize_privacy_mode,
    );

    state.client_state.set_file(raw_name.clone());
    state.client_state.set_file_size(size.clone());
    state.client_state.set_file_duration(raw_duration);
    *state.last_updated_file_time.lock() = Some(std::time::Instant::now());

    let Some(connection) = state.connection.lock().clone() else {
        return;
    };

    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
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
            controller_auth: None,
            new_controlled_room: None,
            features: None,
        }),
    };
    if let Err(e) = connection.send(message) {
        tracing::warn!("Failed to send file update: {}", e);
        return;
    }
    if let Err(e) = connection.send(ProtocolMessage::List { List: None }) {
        tracing::warn!("Failed to request user list after file update: {}", e);
    }

    if let Some(raw_name) = raw_name {
        let state_clone = state.clone();
        tokio::spawn(async move {
            if let Err(e) = change_playlist_from_filename(&state_clone, &raw_name).await {
                tracing::warn!("Failed to sync playlist from filename: {}", e);
            }
        });
    }
}

fn normalize_local_path(raw_path: &str) -> Option<PathBuf> {
    if raw_path.trim().is_empty() {
        return None;
    }
    if raw_path.starts_with("file://") {
        if let Ok(url) = Url::parse(raw_path) {
            if url.scheme() == "file" {
                if let Ok(path) = url.to_file_path() {
                    return Some(path);
                }
            }
        }
        return decode_file_url_fallback(raw_path);
    }
    if is_url(raw_path) {
        return None;
    }
    Some(PathBuf::from(raw_path))
}

fn decode_file_url_fallback(raw_path: &str) -> Option<PathBuf> {
    let mut value = raw_path.trim_start_matches("file://");
    if let Some(rest) = value.strip_prefix("localhost/") {
        value = rest;
    }
    let decoded = urlencoding::decode(value).unwrap_or_else(|_| value.into());
    let mut decoded = decoded.to_string();

    if cfg!(windows) {
        if decoded.starts_with("//") {
            return Some(PathBuf::from(decoded));
        }
        if decoded.starts_with('/') {
            let without = decoded.trim_start_matches('/');
            if without.len() >= 2 && without.as_bytes()[1] == b':' {
                return Some(PathBuf::from(without));
            }
            return Some(PathBuf::from(format!("//{}", without)));
        }
        if !decoded.contains(':') && decoded.contains('/') {
            return Some(PathBuf::from(format!("//{}", decoded)));
        }
        return Some(PathBuf::from(decoded));
    }

    if !decoded.starts_with('/') {
        decoded = format!("/{}", decoded);
    }
    Some(PathBuf::from(decoded))
}

pub(crate) async fn rewind_player(state: &Arc<AppState>) -> Result<(), String> {
    ensure_player_connected(state).await?;
    let player = state
        .player
        .lock()
        .clone()
        .ok_or_else(|| "Player not connected".to_string())?;
    if let Err(e) = player.set_position(0.0).await {
        tracing::warn!("Failed to rewind player: {}", e);
    }
    *state.last_rewind_time.lock() = Some(Instant::now());
    schedule_double_check_rewind(player);
    Ok(())
}

fn schedule_double_check_rewind(player: Arc<dyn PlayerBackend>) {
    if !DOUBLE_CHECK_REWIND {
        return;
    }
    tokio::spawn(async move {
        for delay in DOUBLE_CHECK_REWIND_DELAYS {
            sleep(Duration::from_secs_f64(delay)).await;
            if let Err(e) = player.poll_state().await {
                tracing::warn!("Failed to poll player during rewind check: {}", e);
            }
            if let Some(position) = player.get_state().position {
                if position > DOUBLE_CHECK_REWIND_POSITION_THRESHOLD {
                    if let Err(e) = player.set_position(0.0).await {
                        tracing::warn!("Failed to rewind after double-check: {}", e);
                    }
                }
            }
        }
    });
}

fn file_info_changed(player_state: &PlayerState, last_sent: Option<&PlayerStateSnapshot>) -> bool {
    match last_sent {
        None => true,
        Some(prev) => {
            prev.filename != player_state.filename
                || prev.path != player_state.path
                || prev.duration != player_state.duration
        }
    }
}

pub(crate) fn is_placeholder_file(state: &Arc<AppState>, player_state: &PlayerState) -> bool {
    if let Some(name) = player_state.filename.as_deref() {
        if name == "placeholder.png" {
            return true;
        }
    }
    if let (Some(path), Some(placeholder_path)) = (
        player_state.path.as_deref(),
        resolve_placeholder_path(state),
    ) {
        return Path::new(path) == placeholder_path;
    }
    false
}

#[derive(Clone, Debug)]
struct PlayerStateSnapshot {
    filename: Option<String>,
    position: Option<f64>,
    paused: Option<bool>,
    duration: Option<f64>,
    path: Option<String>,
}

impl PlayerStateSnapshot {
    fn from(state: &PlayerState) -> Self {
        Self {
            filename: state.filename.clone(),
            position: state.position,
            paused: state.paused,
            duration: state.duration,
            path: state.path.clone(),
        }
    }
}

async fn advance_playlist_check(state: &Arc<AppState>, position: f64) -> bool {
    let config = state.config.lock().clone();
    if !shared_playlists_enabled(state, &config) {
        return false;
    }
    if state
        .playlist
        .not_just_changed(PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD)
        && state.client_state.get_file().is_some()
    {
        state.client_state.set_file_duration(Some(position));
    }
    let current_length = state.client_state.get_file_duration().unwrap_or(0.0);
    if current_length <= PLAYLIST_LOAD_NEXT_FILE_MINIMUM_LENGTH {
        return false;
    }
    if (position - current_length).abs() >= PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD {
        return false;
    }
    if !state
        .playlist
        .not_just_changed(PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD)
    {
        return false;
    }
    load_next_file_in_playlist(state, &config).await;
    true
}

async fn load_next_file_in_playlist(state: &Arc<AppState>, config: &SyncplayConfig) {
    if !shared_playlists_enabled(state, config) {
        return;
    }
    if !is_playing_current_index(state) {
        return;
    }

    let items = state.playlist.get_item_filenames();
    if items.is_empty() {
        return;
    }

    let loop_single = config.user.loop_single_files || is_playing_music(state);
    if items.len() == 1 && loop_single {
        state.playlist.opened_file();
        let _ = rewind_player(state).await;
        let player = state.player.lock().clone();
        if let Some(player) = player {
            if let Err(e) = player.set_paused(false).await {
                tracing::warn!("Failed to unpause after looping file: {}", e);
            }
            let player_clone = player.clone();
            tokio::spawn(async move {
                sleep(Duration::from_millis(500)).await;
                let _ = player_clone.set_paused(false).await;
            });
        }
        return;
    }

    let loop_at_end = config.user.loop_at_end_of_playlist || is_playing_music(state);
    let current_index = match state.playlist.get_current_index() {
        Some(index) => index,
        None => return,
    };
    let next_index = if current_index + 1 < items.len() {
        current_index + 1
    } else if loop_at_end {
        0
    } else {
        return;
    };

    if let Some(filename) = items.get(next_index) {
        if !playlist_item_available(state, filename) {
            return;
        }
    }

    *state.last_advance_time.lock() = Some(Instant::now());
    if let Err(e) = send_playlist_index(state, next_index, true) {
        tracing::warn!("Failed to send playlist index advance: {}", e);
    }
    if let Err(e) = apply_playlist_index_from_server(state, next_index, true).await {
        tracing::warn!("Failed to advance playlist: {}", e);
    }
}

fn is_playing_current_index(state: &Arc<AppState>) -> bool {
    let Some(index) = state.playlist.get_current_index() else {
        return false;
    };
    let items = state.playlist.get_item_filenames();
    let Some(filename) = items.get(index) else {
        return false;
    };
    let current_file = state.client_state.get_file();
    same_filename(current_file.as_deref(), Some(filename))
}

pub fn playlist_item_available(state: &Arc<AppState>, filename: &str) -> bool {
    if filename == PRIVACY_HIDDEN_FILENAME {
        return false;
    }
    let (trusted_domains, only_trusted, media_directories) = {
        let config = state.config.lock();
        (
            config.user.trusted_domains.clone(),
            config.user.only_switch_to_trusted_domains,
            config.player.media_directories.clone(),
        )
    };
    if is_url(filename) {
        let (trustable, trusted) =
            is_trustable_and_trusted(filename, &trusted_domains, only_trusted);
        return trustable && trusted;
    }
    if state.media_index.is_available(filename) {
        return true;
    }
    resolve_media_path(&media_directories, filename).is_some()
}

pub(crate) async fn handle_end_of_file(state: &Arc<AppState>) {
    if state
        .playlist
        .not_just_changed(PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD)
        && state.client_state.get_file().is_some()
    {
        let player = state.player.lock().clone();
        if let Some(player) = player {
            if let Some(position) = player.get_state().position {
                state.client_state.set_file_duration(Some(position));
            }
        }
    }

    let config = state.config.lock().clone();
    if !shared_playlists_enabled(state, &config) {
        return;
    }
    if !state
        .playlist
        .not_just_changed(PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD)
    {
        return;
    }
    load_next_file_in_playlist(state, &config).await;
}

fn current_user_can_control(state: &Arc<AppState>) -> bool {
    let room = state.client_state.get_room();
    if !crate::utils::is_controlled_room(&room) {
        return true;
    }
    let username = state.client_state.get_username();
    state
        .client_state
        .get_user(&username)
        .map(|user| user.is_controller)
        .unwrap_or(false)
}

fn is_playing_music(state: &Arc<AppState>) -> bool {
    state
        .client_state
        .get_file()
        .as_deref()
        .map(is_music_file)
        .unwrap_or(false)
}

fn seamless_music_override(state: &Arc<AppState>) -> bool {
    is_playing_music(state) && recently_advanced(state)
}

fn is_readiness_supported(state: &Arc<AppState>, requires_other_users: bool) -> bool {
    if !state.server_features.lock().readiness {
        return false;
    }
    if !requires_other_users {
        return true;
    }
    let room = state.client_state.get_room();
    let username = state.client_state.get_username();
    state
        .client_state
        .get_users_in_room(&room)
        .iter()
        .any(|user| user.username != username && user.is_ready_with_file().is_some())
}

fn recently_rewound(state: &Arc<AppState>) -> bool {
    let Some(mut last_rewind) = *state.last_rewind_time.lock() else {
        return false;
    };
    if let Some(last_updated) = *state.last_updated_file_time.lock() {
        if last_updated > last_rewind {
            if let Some(adjusted) = last_rewind.checked_sub(Duration::from_secs_f64(
                RECENT_REWIND_FILE_UPDATE_SHIFT_SECONDS,
            )) {
                last_rewind = adjusted;
            }
        }
    }
    last_rewind.elapsed().as_secs_f64() < RECENT_REWIND_THRESHOLD_SECONDS
}

fn recently_advanced(state: &Arc<AppState>) -> bool {
    let guard = state.last_advance_time.lock();
    let Some(last_advance) = guard.as_ref() else {
        return false;
    };
    last_advance.elapsed().as_secs_f64() < RECENT_ADVANCE_GRACE_SECONDS
}

fn check_protocol_timeout(state: &Arc<AppState>) -> bool {
    let guard = state.last_global_update.lock();
    let Some(last_global) = guard.as_ref() else {
        return false;
    };
    if last_global.elapsed().as_secs_f64() <= PROTOCOL_TIMEOUT_SECONDS {
        return false;
    }
    *state.last_global_update.lock() = None;
    crate::commands::connection::emit_error_message(state, "Server timed out");
    if let Some(connection) = state.connection.lock().clone() {
        connection.disconnect();
    }
    let state_clone = state.clone();
    tokio::spawn(async move {
        crate::commands::connection::handle_connection_closed(&state_clone).await;
    });
    true
}

async fn apply_ready_toggle(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    paused: bool,
    global_paused: bool,
) -> (bool, bool) {
    let config = state.config.lock().clone();
    let mut paused_value = paused;

    if !current_user_can_control(state) {
        let new_ready = !state.client_state.is_ready();
        if let Err(e) = player.set_paused(global_paused).await {
            tracing::warn!("Failed to enforce pause state: {}", e);
        }
        paused_value = global_paused;
        if !(recently_rewound(state) || (global_paused && !recently_advanced(state))) {
            let _ = send_ready_state(state, new_ready, true);
            let message = if new_ready {
                "You are now set as ready"
            } else {
                "You are now set as not ready"
            };
            crate::commands::connection::emit_system_message(state, message);
            crate::commands::connection::maybe_show_osd(state, &config, message, true);
        }
        return (false, paused_value);
    }

    if seamless_music_override(state) {
        if let Err(e) = player.set_paused(paused_value).await {
            tracing::warn!(
                "Failed to enforce pause during seamless music override: {}",
                e
            );
        }
        return (false, paused_value);
    }

    if recently_rewound(state) && global_paused && !recently_advanced(state) {
        if let Err(e) = player.set_paused(global_paused).await {
            tracing::warn!("Failed to enforce pause after rewind: {}", e);
        }
        paused_value = global_paused;
        return (false, paused_value);
    }

    if !paused_value && !instaplay_conditions_met(state, &config) {
        if let Err(e) = player.set_paused(true).await {
            tracing::warn!("Failed to block unpause: {}", e);
        }
        paused_value = true;
        let _ = send_ready_state(state, true, true);
        let message = "You are now set as ready - unpause again to unpause";
        crate::commands::connection::emit_system_message(state, message);
        crate::commands::connection::maybe_show_osd(state, &config, message, true);
        return (false, paused_value);
    }

    if let Some(last_paused) = state.last_paused_on_leave_time.lock().take() {
        if last_paused.elapsed().as_secs_f64() < LAST_PAUSED_DIFF_THRESHOLD_SECONDS {
            return (true, paused_value);
        }
    }

    let desired_ready = !paused_value;
    if desired_ready != state.client_state.is_ready() {
        let _ = send_ready_state(state, desired_ready, false);
    }

    (true, paused_value)
}

fn instaplay_conditions_met(state: &Arc<AppState>, config: &SyncplayConfig) -> bool {
    if is_playing_music(state) {
        return true;
    }
    if !current_user_can_control(state) {
        return false;
    }
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
                let count = users_in_room_count(state, &state.client_state.get_room());
                return count >= min_users as usize;
            }
            true
        }
    }
}

fn all_other_users_ready(state: &Arc<AppState>, room: &str) -> bool {
    let username = state.client_state.get_username();
    for user in state.client_state.get_users_in_room(room) {
        if user.username != username && user.is_ready_with_file() == Some(false) {
            return false;
        }
    }
    true
}

fn users_in_room_count(state: &Arc<AppState>, room: &str) -> usize {
    let mut count = 1;
    let username = state.client_state.get_username();
    for user in state.client_state.get_users_in_room(room) {
        if user.username == username {
            continue;
        }
        if user.is_ready_with_file() == Some(true) {
            count += 1;
        }
    }
    count
}

fn send_ready_state(
    state: &Arc<AppState>,
    is_ready: bool,
    manually_initiated: bool,
) -> Result<(), String> {
    if !state.server_features.lock().readiness {
        return Ok(());
    }
    state.client_state.set_ready(is_ready);
    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: None,
            user: None,
            ready: Some(ReadyState {
                username: None,
                is_ready: Some(is_ready),
                manually_initiated: Some(manually_initiated),
                set_by: None,
            }),
            playlist_index: None,
            playlist_change: None,
            controller_auth: None,
            new_controlled_room: None,
            features: None,
        }),
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
    use super::{
        check_mpv_version, parse_mpv_version_flags, resolve_media_path, should_pause_on_prepare,
    };
    use crate::player::backend::PlayerKind;
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

    #[test]
    fn pause_on_prepare_matches_original_player_prepare_flow() {
        assert!(should_pause_on_prepare(PlayerKind::Mpv));
        assert!(should_pause_on_prepare(PlayerKind::MpvNet));
        assert!(should_pause_on_prepare(PlayerKind::Iina));
        assert!(should_pause_on_prepare(PlayerKind::Mplayer));
        assert!(!should_pause_on_prepare(PlayerKind::Vlc));
        assert!(!should_pause_on_prepare(PlayerKind::MpcHc));
        assert!(!should_pause_on_prepare(PlayerKind::MpcBe));
    }

    #[test]
    fn missing_mpv_version_check_matches_original_unknown_version_fallback() {
        let flags = check_mpv_version("/path/to/missing/mpv").unwrap();

        assert!(!flags.osc_visibility_change_compatible);
    }

    #[test]
    fn mpv_version_flags_match_original_thresholds() {
        assert!(parse_mpv_version_flags("mpv 0.22.0 Copyright").is_err());
        assert!(
            !parse_mpv_version_flags("mpv 0.23.0 Copyright")
                .unwrap()
                .osc_visibility_change_compatible
        );
        assert!(
            parse_mpv_version_flags("mpv 0.28.0 Copyright")
                .unwrap()
                .osc_visibility_change_compatible
        );
        assert!(
            !parse_mpv_version_flags("unexpected output")
                .unwrap()
                .osc_visibility_change_compatible
        );
    }
}
