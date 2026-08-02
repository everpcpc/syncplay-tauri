use crate::app_state::{AppState, PlayerStateEvent};
use crate::client::media_index::{resolve_exact_in_directory, resolve_similar_in_directory};
use crate::client::playback::{CommittedMedia, LoadId, PlaybackEvent};
use crate::client::playback_runtime;
use crate::commands::playlist::shared_playlists_enabled;
use crate::config::{SyncplayConfig, UnpauseAction};
use crate::network::messages::{FileInfo, PlayState, ProtocolMessage, ReadyState, SetMessage};
use crate::player::backend::{player_kind_from_path_or_default, PlayerBackend, PlayerKind};
use crate::player::commands::{LoadfileOptionsSyntax, MpvCommand};
use crate::player::mpc_api::MpcApiBackend;
use crate::player::mplayer_slave::MplayerBackend;
use crate::player::mpv_backend::MpvBackend;
use crate::player::mpv_ipc::MpvIpc;
use crate::player::properties::PlayerState;
use crate::player::vlc_syncplay::VlcSyncplayBackend;
use crate::utils::{
    apply_privacy, is_music_file, is_trustable_and_trusted, is_url, truncate_text,
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
use tokio::time::{sleep, timeout, Duration};
use tracing::info;
use url::Url;

const RECENT_REWIND_THRESHOLD_SECONDS: f64 = 5.0;
const RECENT_ADVANCE_GRACE_SECONDS: f64 = 8.0;
const LAST_PAUSED_DIFF_THRESHOLD_SECONDS: f64 = 2.0;
const PLAYLIST_LOAD_NEXT_FILE_MINIMUM_LENGTH: f64 = 10.0;
const PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD: f64 = 5.0;
const DOUBLE_CHECK_REWIND: bool = true;
const DOUBLE_CHECK_REWIND_POSITION_THRESHOLD: f64 = 5.0;
const DOUBLE_CHECK_REWIND_DELAYS: [f64; 3] = [0.5, 1.0, 1.5];
const RECENT_REWIND_FILE_UPDATE_SHIFT_SECONDS: f64 = 4.5;
const PLAYER_SHUTDOWN_TIMEOUT_MS: u64 = 750;
const PLAYER_PROCESS_KILL_TIMEOUT_MS: u64 = 750;
const PLAYER_LOAD_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) enum LoadMediaError {
    MediaNotFound(String),
    Failed(String),
    Superseded,
}

impl std::fmt::Display for LoadMediaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MediaNotFound(message) | Self::Failed(message) => formatter.write_str(message),
            Self::Superseded => formatter.write_str("Media load was superseded"),
        }
    }
}

pub(crate) struct StartedMediaLoad {
    pub lease: playback_runtime::LoadLease,
    pub is_stream: bool,
}

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
    let _lifecycle_guard = state.player_lifecycle.lock().await;
    if state.is_player_connected() {
        tracing::debug!("player_lifecycle: ensure connected skipped; backend already connected");
        return Ok(());
    }
    {
        let mut guard = state.player_connecting.lock();
        if *guard {
            tracing::debug!(
                "player_lifecycle: ensure connected skipped; connection already in progress"
            );
            return Ok(());
        }
        *guard = true;
    }
    let _connecting_guard = PlayerConnectingGuard::new(&state.player_connecting);
    tracing::info!("player_lifecycle: connecting player backend");

    #[cfg(test)]
    let fake_player_factory = state.fake_player_factory.lock().clone();
    #[cfg(test)]
    if let Some(factory) = fake_player_factory {
        let fake = Arc::new(factory.launch(PlayerKind::Mpv));
        prepare_player_after_connect(&(fake.clone() as Arc<dyn PlayerBackend>)).await;
        *state.player.lock() = Some(fake);
        *state.last_player_spawn.lock() = Some(Instant::now());
        *state.last_player_kind.lock() = Some(PlayerKind::Mpv);
        tracing::info!(
            launch_count = factory.launch_count(),
            kind = ?PlayerKind::Mpv,
            "player_lifecycle: fake player launched"
        );
        return Ok(());
    }

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
            let queried_version_flags = query_mpv_version_flags(&mpv).await;
            let version_flags = match kind {
                PlayerKind::Iina | PlayerKind::MpvNet => MpvVersionFlags {
                    osc_visibility_change_compatible: true,
                    loadfile_options_syntax: queried_version_flags
                        .and_then(|flags| flags.loadfile_options_syntax),
                },
                _ => queried_version_flags.unwrap_or(check_mpv_version(&player_path)?),
            };
            let backend = Arc::new(MpvBackend::new(
                kind,
                mpv,
                Arc::downgrade(state),
                version_flags.loadfile_options_syntax,
                version_flags.osc_visibility_change_compatible,
                stdout,
            ));
            let backend_dyn: Arc<dyn PlayerBackend> = backend.clone();
            *state.player.lock() = Some(backend_dyn.clone());
            backend.spawn_event_loop(event_rx);
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
                MpcApiBackend::start(kind, &player_path, &args, None)
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
    let _lifecycle_guard = state.player_lifecycle.lock().await;
    stop_player_locked(state).await
}

pub(crate) async fn stop_player_instance(
    state: &Arc<AppState>,
    instance_id: u64,
) -> Result<(), String> {
    let _lifecycle_guard = state.player_lifecycle.lock().await;
    let is_current = state
        .player
        .lock()
        .as_ref()
        .is_some_and(|player| player.instance_id() == instance_id);
    if !is_current {
        return Ok(());
    }
    stop_player_locked(state).await
}

async fn stop_player_locked(state: &Arc<AppState>) -> Result<(), String> {
    playback_runtime::player_disconnected(state).await;
    let player = state.player.lock().take();
    let child = state.player_process.lock().take();
    let player_kind = player.as_ref().map(|player| player.kind());
    let had_child = child.is_some();
    tracing::info!(
        kind = ?player_kind,
        had_process = had_child,
        "player_lifecycle: stopping player"
    );

    *state.last_player_spawn.lock() = None;
    *state.last_player_kind.lock() = None;
    *state.player_connecting.lock() = false;
    *state.mpv_socket_path.lock() = None;
    *state.mpv_runtime_dir.lock() = None;

    if let Some(player) = player {
        match timeout(
            Duration::from_millis(PLAYER_SHUTDOWN_TIMEOUT_MS),
            player.shutdown(),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::info!(kind = ?player_kind, "player_lifecycle: backend shutdown completed");
            }
            Ok(Err(e)) => tracing::warn!("Failed to shutdown player: {}", e),
            Err(_) => tracing::warn!("Timed out while shutting down player backend"),
        }
    }
    if let Some(mut child) = child {
        if let Err(e) = child.kill().await {
            tracing::warn!("Failed to stop player process: {}", e);
        }
        match timeout(
            Duration::from_millis(PLAYER_PROCESS_KILL_TIMEOUT_MS),
            child.wait(),
        )
        .await
        {
            Ok(_) => {
                tracing::info!(kind = ?player_kind, "player_lifecycle: player process exited after kill");
            }
            Err(_) => tracing::warn!("Timed out while waiting for player process to exit"),
        }
    }
    Ok(())
}

pub fn spawn_player_state_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut media_candidate: Option<PlayerMediaObservation> = None;
        let mut committed_observation: Option<PlayerMediaObservation> = None;
        let mut observed_player: Option<Arc<dyn PlayerBackend>> = None;
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            let player = state.player.lock().clone();
            let Some(player) = player else {
                observed_player = None;
                media_candidate = None;
                committed_observation = None;
                continue;
            };
            let player_changed = observed_player
                .as_ref()
                .is_none_or(|observed| !Arc::ptr_eq(observed, &player));
            if player_changed {
                observed_player = Some(player.clone());
                media_candidate = None;
                committed_observation = None;
            }
            if !player.is_connected() {
                tracing::warn!("Player backend disconnected; clearing stale player state");
                clear_disconnected_player(&state, &player).await;
                media_candidate = None;
                committed_observation = None;
                continue;
            }
            if let Err(e) = player.poll_state().await {
                tracing::warn!("Failed to poll player state: {}", e);
                if !player.is_connected() {
                    clear_disconnected_player(&state, &player).await;
                    media_candidate = None;
                    committed_observation = None;
                    continue;
                }
            }
            let is_current = state
                .player
                .lock()
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &player));
            if !is_current {
                observed_player = None;
                media_candidate = None;
                committed_observation = None;
                continue;
            }
            let player_state = player.get_state();
            emit_player_state(&state, &player_state);

            let is_placeholder = is_placeholder_file(&state, &player_state);
            if !player.reports_atomic_media_commits() && !is_placeholder {
                let observation = PlayerMediaObservation::from(&player_state);
                let stable = media_candidate.as_ref() == Some(&observation);
                let not_committed = committed_observation.as_ref() != Some(&observation);
                if stable && not_committed {
                    let commit = {
                        let _transition_guard = state.playback.media_transition.lock().await;
                        let _lifecycle_guard = state.player_lifecycle.lock().await;
                        let is_current = state
                            .player
                            .lock()
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &player));
                        if !is_current {
                            observed_player = None;
                            media_candidate = None;
                            committed_observation = None;
                            continue;
                        }
                        match commit_player_state(&state, Some(&player), &player_state, None).await
                        {
                            Ok(commit) => commit,
                            Err(error) => {
                                tracing::warn!("Failed to commit player media: {}", error);
                                playback_runtime::DispatchResult::default()
                            }
                        }
                    };
                    if commit.media_settled {
                        committed_observation = Some(observation.clone());
                    }
                }
                media_candidate = Some(observation);
            }

            if state.is_connected() && crate::commands::connection::check_protocol_timeout(&state) {
                continue;
            }

            if !state.is_connected() {
                continue;
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
        }
    });
}

async fn clear_disconnected_player(state: &Arc<AppState>, disconnected: &Arc<dyn PlayerBackend>) {
    let _lifecycle_guard = state.player_lifecycle.lock().await;
    let claimed = {
        let mut guard = state.player.lock();
        if guard
            .as_ref()
            .map(|current| Arc::ptr_eq(current, disconnected))
            .unwrap_or(false)
        {
            guard.take()
        } else {
            None
        }
    };
    if claimed.is_none() {
        return;
    }
    playback_runtime::player_disconnected(state).await;
    *state.player_process.lock() = None;
    *state.last_player_spawn.lock() = None;
    *state.last_player_kind.lock() = None;
    *state.player_connecting.lock() = false;
    state.emit_event(
        "player-state-changed",
        PlayerStateEvent {
            filename: None,
            position: None,
            duration: None,
            paused: None,
            speed: None,
        },
    );
}
pub async fn load_media_by_name(
    state: &Arc<AppState>,
    filename: &str,
    reset_position: bool,
    load_id: LoadId,
) -> Result<StartedMediaLoad, LoadMediaError> {
    let config = state.config.lock().clone();
    let (media_path, is_stream) = if is_url(filename) {
        let (trustable, trusted) = is_trustable_and_trusted(
            filename,
            &config.user.trusted_domains,
            config.user.only_switch_to_trusted_domains,
        );
        if !trustable || !trusted {
            return Err(LoadMediaError::Failed("URL is not trusted".to_string()));
        }
        (filename.to_string(), true)
    } else {
        let media_path = state
            .media_index
            .resolve_path(filename)
            .or_else(|| resolve_media_path(&config.player.media_directories, filename))
            .ok_or_else(|| {
                LoadMediaError::MediaNotFound(format!(
                    "File not found in media directories: {}",
                    filename
                ))
            })?;
        state.media_index.remember_resolved_path(&media_path);
        (media_path.to_string_lossy().into_owned(), false)
    };

    ensure_player_connected(state)
        .await
        .map_err(LoadMediaError::Failed)?;
    let _transition_guard = state.playback.media_transition.lock().await;
    let (player, lease) = {
        let _lifecycle_guard = state.player_lifecycle.lock().await;
        let player = state
            .player
            .lock()
            .clone()
            .ok_or_else(|| LoadMediaError::Failed("Player not connected".to_string()))?;
        let Some(lease) = playback_runtime::claim_load_for_issue(
            state,
            load_id,
            filename,
            &media_path,
            player.clone(),
        ) else {
            return Err(LoadMediaError::Superseded);
        };
        player.begin_file_load(load_id.0, &media_path);
        if reset_position {
            player.mark_reset(is_stream);
        }
        let load_result = tokio::select! {
            biased;
            () = lease.cancelled() => return Err(LoadMediaError::Superseded),
            result = timeout(
                PLAYER_LOAD_COMMAND_TIMEOUT,
                player.load_file_generation(&media_path, load_id.0),
            ) => result,
        };
        match load_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                if state.playback.abort_load(load_id).is_some() {
                    player.cancel_file_load(load_id.0);
                }
                return Err(LoadMediaError::Failed(format!(
                    "Failed to load file: {}",
                    error
                )));
            }
            Err(_) => {
                if state.playback.abort_load(load_id).is_some() {
                    player.cancel_file_load(load_id.0);
                }
                return Err(LoadMediaError::Failed(
                    "Timed out while sending player load command".to_string(),
                ));
            }
        }
        if reset_position {
            rewind_player_instance(state, player.clone(), load_id).await;
        }
        (player, lease)
    };

    state.playlist.opened_file();
    if reset_position {
        crate::commands::connection::evaluate_autoplay(state);
    }
    debug_assert!(Arc::ptr_eq(&player, &lease.player));
    Ok(StartedMediaLoad { lease, is_stream })
}

async fn sync_mpc_after_file_change(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    reset_position: bool,
    load_id: Option<LoadId>,
) {
    let global = state.client_state.get_global_state();
    let position = if reset_position { 0.0 } else { global.position };
    for _ in 0..3 {
        if !can_sync_committed_media(state, player, load_id) {
            return;
        }
        let _ = player.set_paused(true).await;
        sleep(Duration::from_millis(10)).await;
    }
    sleep(Duration::from_millis(50)).await;
    if !can_sync_committed_media(state, player, load_id) {
        return;
    }
    let _ = player.set_paused(global.paused).await;
    if can_sync_committed_media(state, player, load_id) {
        let _ = player.set_position(position).await;
    }
}

async fn sync_generic_after_file_change(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    reset_position: bool,
    load_id: Option<LoadId>,
) {
    let global = state.client_state.get_global_state();
    let position = if reset_position { 0.0 } else { global.position };
    if can_sync_committed_media(state, player, load_id) {
        let _ = player.set_paused(global.paused).await;
    }
    if can_sync_committed_media(state, player, load_id) {
        let _ = player.set_position(position).await;
    }
}

async fn sync_mpv_after_file_change(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    load_id: Option<LoadId>,
) {
    let global = state.client_state.get_global_state();
    if can_sync_committed_media(state, player, load_id) {
        let _ = player.set_position(global.position).await;
    }
    if can_sync_committed_media(state, player, load_id) {
        let _ = player.set_paused(global.paused).await;
    }
}

fn can_sync_committed_media(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    load_id: Option<LoadId>,
) -> bool {
    if !state
        .player
        .lock()
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, player))
    {
        return false;
    }
    match load_id {
        Some(load_id) => state
            .playback
            .active_load(load_id)
            .is_some_and(|load| Arc::ptr_eq(&load.player, player) && !load.is_cancelled()),
        None => state.playback.state.lock().pending_load.is_none(),
    }
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
    loadfile_options_syntax: Option<LoadfileOptionsSyntax>,
}

fn check_mpv_version(player_path: &str) -> Result<MpvVersionFlags, String> {
    let Ok(output) = run_mpv_version_command(player_path) else {
        return Ok(MpvVersionFlags {
            osc_visibility_change_compatible: false,
            loadfile_options_syntax: None,
        });
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_mpv_version_flags(&stdout)
}

fn parse_mpv_version_flags(stdout: &str) -> Result<MpvVersionFlags, String> {
    let re = Regex::new(r"(?:mpv\s+)?(\d+)\.(\d+)\.").map_err(|e| e.to_string())?;
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
        let loadfile_options_syntax = Some(if major > 0 || minor >= 38 {
            LoadfileOptionsSyntax::Modern
        } else {
            LoadfileOptionsSyntax::Legacy
        });
        return Ok(MpvVersionFlags {
            osc_visibility_change_compatible,
            loadfile_options_syntax,
        });
    }
    Ok(MpvVersionFlags {
        osc_visibility_change_compatible: false,
        loadfile_options_syntax: None,
    })
}

async fn query_mpv_version_flags(mpv: &MpvIpc) -> Option<MpvVersionFlags> {
    let response = mpv
        .send_command_async(MpvCommand::get_property("mpv-version", 0))
        .await
        .ok()?;
    let version = response.data?.as_str()?.to_string();
    parse_mpv_version_flags(&version).ok()
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

pub(crate) async fn commit_player_state(
    state: &Arc<AppState>,
    player: Option<&Arc<dyn PlayerBackend>>,
    player_state: &PlayerState,
    load_id: Option<LoadId>,
) -> Result<playback_runtime::DispatchResult, String> {
    if is_placeholder_file(state, player_state) {
        return Ok(playback_runtime::DispatchResult::default());
    }
    let Some(media) = committed_media_from_player_state(player_state) else {
        return Ok(playback_runtime::DispatchResult::default());
    };
    let load_id = load_id.or_else(|| {
        player.and_then(|player| {
            state
                .playback
                .matching_load(player, &media.name)
                .map(|load| load.id)
        })
    });
    let outcome = playback_runtime::dispatch_all_outcome(
        state,
        [PlaybackEvent::PlayerMediaCommitted { load_id, media }],
    )
    .await;
    let result = outcome.result;

    if let (true, Some(player)) = (result.media_accepted && state.is_connected(), player) {
        match player.kind() {
            PlayerKind::MpcHc | PlayerKind::MpcBe => {
                sync_mpc_after_file_change(
                    state,
                    player,
                    result.media_reset,
                    result.completed_load,
                )
                .await;
            }
            PlayerKind::Mpv | PlayerKind::MpvNet | PlayerKind::Iina if !result.media_reset => {
                sync_mpv_after_file_change(state, player, result.completed_load).await;
            }
            PlayerKind::Vlc | PlayerKind::Mplayer | PlayerKind::Unknown => {
                sync_generic_after_file_change(
                    state,
                    player,
                    result.media_reset,
                    result.completed_load,
                )
                .await;
            }
            PlayerKind::Mpv | PlayerKind::MpvNet | PlayerKind::Iina => {}
        }
    }
    if let Some(load_id) = result.completed_load {
        state.playback.finish_load(load_id);
    }
    if let Some(error) = outcome.effect_error {
        return Err(error);
    }
    Ok(result)
}

fn committed_media_from_player_state(player_state: &PlayerState) -> Option<CommittedMedia> {
    if player_state.filename.is_none() && player_state.path.is_none() {
        return None;
    }
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
        let filename = player_state.filename.as_deref()?;
        if is_url(filename) {
            Some(filename.to_string())
        } else {
            return None;
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
    raw_name.map(|name| CommittedMedia::new(name, raw_size, player_state.duration))
}

pub(crate) fn send_committed_file_update(
    state: &Arc<AppState>,
    media: &CommittedMedia,
) -> Result<(), String> {
    let config = state.config.lock().clone();

    let max_len = state
        .server_features
        .lock()
        .max_filename_length
        .unwrap_or(250);
    let outbound_name = Some(truncate_text(&media.name, max_len));
    let (name, size) = apply_privacy(
        outbound_name,
        media.size,
        &config.user.filename_privacy_mode,
        &config.user.filesize_privacy_mode,
    );

    state.client_state.set_file_info(FileInfo {
        name: Some(media.name.clone()),
        size: size.clone(),
        duration: media.duration,
    });
    *state.last_updated_file_time.lock() = Some(std::time::Instant::now());

    let Some(connection) = state.connection.lock().clone() else {
        return Ok(());
    };

    let message = ProtocolMessage::Set {
        Set: Box::new(SetMessage {
            room: None,
            file: Some(FileInfo {
                name,
                size,
                duration: media.duration,
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
        return Err(format!("Failed to send file update: {}", e));
    }
    if let Err(e) = connection.send(ProtocolMessage::List { List: None }) {
        tracing::warn!("Failed to request user list after file update: {}", e);
    }
    Ok(())
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

async fn rewind_player_instance(
    state: &Arc<AppState>,
    player: Arc<dyn PlayerBackend>,
    load_id: LoadId,
) {
    if let Err(e) = player.set_position(0.0).await {
        tracing::warn!("Failed to rewind player: {}", e);
    }
    *state.last_rewind_time.lock() = Some(Instant::now());
    schedule_double_check_rewind(state.clone(), player, RewindGeneration::Load(load_id));
}

#[derive(Clone)]
enum RewindGeneration {
    Load(LoadId),
    Loop(playback_runtime::LoopLease),
}

fn schedule_double_check_rewind(
    state: Arc<AppState>,
    player: Arc<dyn PlayerBackend>,
    generation: RewindGeneration,
) {
    if !DOUBLE_CHECK_REWIND {
        return;
    }
    tokio::spawn(async move {
        for delay in DOUBLE_CHECK_REWIND_DELAYS {
            sleep(Duration::from_secs_f64(delay)).await;
            if !is_current_rewind_target(&state, &player, &generation) {
                return;
            }
            if let Err(e) = player.poll_state().await {
                tracing::warn!("Failed to poll player during rewind check: {}", e);
            }
            let _transition_guard = state.playback.media_transition.lock().await;
            let _lifecycle_guard = state.player_lifecycle.lock().await;
            let _dispatch_guard = state.playback.dispatch.lock().await;
            if !is_current_rewind_target(&state, &player, &generation) {
                return;
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

fn is_current_rewind_target(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    generation: &RewindGeneration,
) -> bool {
    let current_player = state
        .player
        .lock()
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, player));
    current_player
        && match generation {
            RewindGeneration::Load(load_id) => {
                state.playback.is_latest_generation(*load_id, player)
            }
            RewindGeneration::Loop(lease) => playback_runtime::is_current_loop_lease(state, lease),
        }
}

async fn rewind_looping_media(state: &Arc<AppState>, lease: playback_runtime::LoopLease) {
    let player = lease.player.clone();
    let _transition_guard = state.playback.media_transition.lock().await;
    let _lifecycle_guard = state.player_lifecycle.lock().await;
    let _dispatch_guard = state.playback.dispatch.lock().await;
    if !playback_runtime::is_current_loop_lease(state, &lease) {
        return;
    }
    if let Err(error) = player.set_position(0.0).await {
        tracing::warn!("Failed to rewind looping media: {error}");
    }
    *state.last_rewind_time.lock() = Some(Instant::now());
    if let Err(error) = player.set_paused(false).await {
        tracing::warn!("Failed to unpause looping media: {error}");
    }
    drop(_dispatch_guard);
    drop(_lifecycle_guard);
    drop(_transition_guard);

    schedule_double_check_rewind(state.clone(), player, RewindGeneration::Loop(lease.clone()));
    schedule_loop_unpause(state.clone(), lease);
}

fn schedule_loop_unpause(state: Arc<AppState>, lease: playback_runtime::LoopLease) {
    tokio::spawn(async move {
        sleep(Duration::from_millis(500)).await;
        let _transition_guard = state.playback.media_transition.lock().await;
        let _lifecycle_guard = state.player_lifecycle.lock().await;
        let _dispatch_guard = state.playback.dispatch.lock().await;
        if !playback_runtime::is_current_loop_lease(&state, &lease) {
            return;
        }
        if let Err(error) = lease.player.set_paused(false).await {
            tracing::warn!("Failed to confirm loop unpause: {error}");
        }
    });
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

#[derive(Clone, Debug, PartialEq)]
struct PlayerMediaObservation {
    filename: Option<String>,
    duration: Option<f64>,
    path: Option<String>,
}

impl PlayerMediaObservation {
    fn from(state: &PlayerState) -> Self {
        Self {
            filename: state.filename.clone(),
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
    let current_length = state.client_state.get_file_duration().unwrap_or(0.0);
    if current_length <= PLAYLIST_LOAD_NEXT_FILE_MINIMUM_LENGTH {
        return false;
    }
    if (position - current_length).abs() >= PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD {
        return false;
    }
    load_next_file_in_playlist(state, &config).await;
    true
}

async fn load_next_file_in_playlist(state: &Arc<AppState>, config: &SyncplayConfig) {
    if !shared_playlists_enabled(state, config) {
        return;
    }
    let Some(expected_media) = state.client_state.get_file() else {
        return;
    };

    let loop_single = config.user.loop_single_files || is_playing_music(state);
    let loop_at_end = config.user.loop_at_end_of_playlist || is_playing_music(state);
    let action = playback_runtime::advance_after_eof(
        state,
        &expected_media,
        loop_single,
        loop_at_end,
        PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD,
    )
    .await;
    match action {
        Ok(playback_runtime::EofAction::Rewind(lease)) => {
            rewind_looping_media(state, lease).await;
        }
        Ok(playback_runtime::EofAction::None | playback_runtime::EofAction::Load) => {}
        Err(error) => tracing::warn!("Failed to advance playlist: {error}"),
    }
}

pub(crate) fn report_end_of_file(state: &Arc<AppState>, position: Option<f64>) {
    if !state
        .playlist
        .not_just_changed(PLAYLIST_LOAD_NEXT_FILE_TIME_FROM_END_THRESHOLD)
        || state.client_state.get_file().is_none()
    {
        return;
    }
    if let Some(position) = position {
        state.client_state.set_file_duration(Some(position));
    }
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
    if state.client_state.get_server_version().is_none() {
        return false;
    }
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
        check_mpv_version, clear_disconnected_player, parse_mpv_version_flags, resolve_media_path,
        rewind_looping_media, schedule_double_check_rewind, should_pause_on_prepare, stop_player,
        LoadfileOptionsSyntax, RewindGeneration,
    };
    use crate::app_state::AppState;
    use crate::client::playback::{CommittedMedia, LoadId, PlaybackEvent};
    use crate::client::playback_runtime::{self, EofAction};
    use crate::player::backend::{FakePlayerBackend, FakePlayerCommand, PlayerBackend, PlayerKind};
    use crate::player::properties::PlayerState;
    use std::fs;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::time::{sleep, timeout, Duration};

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
        let legacy = parse_mpv_version_flags("mpv 0.23.0 Copyright").unwrap();
        assert!(!legacy.osc_visibility_change_compatible);
        assert_eq!(
            legacy.loadfile_options_syntax,
            Some(LoadfileOptionsSyntax::Legacy)
        );
        assert!(
            parse_mpv_version_flags("mpv 0.28.0 Copyright")
                .unwrap()
                .osc_visibility_change_compatible
        );
        assert_eq!(
            parse_mpv_version_flags("0.38.0")
                .unwrap()
                .loadfile_options_syntax,
            Some(LoadfileOptionsSyntax::Modern)
        );
        let unknown = parse_mpv_version_flags("unexpected output").unwrap();
        assert!(!unknown.osc_visibility_change_compatible);
        assert_eq!(unknown.loadfile_options_syntax, None);
    }

    #[tokio::test]
    async fn stop_player_shuts_down_non_mpv_fake_backends_without_real_players() {
        for kind in [PlayerKind::Vlc, PlayerKind::Mplayer, PlayerKind::MpcHc] {
            let state = AppState::new();
            let fake = Arc::new(FakePlayerBackend::new(kind));
            let player: Arc<dyn PlayerBackend> = fake.clone();
            *state.player.lock() = Some(player);

            stop_player(&state).await.unwrap();
            stop_player(&state).await.unwrap();

            assert!(state.player.lock().is_none());
            assert_eq!(
                fake.shutdown_count(),
                1,
                "{kind:?} shutdown must be idempotent"
            );
            assert_eq!(fake.commands(), vec![FakePlayerCommand::Shutdown]);
        }
    }

    #[test]
    fn app_state_player_connected_uses_backend_freshness() {
        let state = AppState::new();
        let fake = Arc::new(FakePlayerBackend::new(PlayerKind::Mplayer));
        let player: Arc<dyn PlayerBackend> = fake.clone();
        *state.player.lock() = Some(player);

        assert!(state.is_player_connected());

        fake.set_connected(false);

        assert!(!state.is_player_connected());
    }

    #[tokio::test]
    async fn disconnected_non_mpv_backend_clears_stale_app_state() {
        let state = AppState::new();
        let fake = Arc::new(FakePlayerBackend::new(PlayerKind::Vlc));
        let player: Arc<dyn PlayerBackend> = fake.clone();
        *state.player.lock() = Some(player.clone());
        *state.last_player_spawn.lock() = Some(std::time::Instant::now());
        *state.last_player_kind.lock() = Some(PlayerKind::Vlc);
        *state.player_connecting.lock() = true;

        clear_disconnected_player(&state, &player).await;

        assert!(state.player.lock().is_none());
        assert!(state.player_process.lock().is_none());
        assert!(state.last_player_spawn.lock().is_none());
        assert!(state.last_player_kind.lock().is_none());
        assert!(!*state.player_connecting.lock());
    }

    #[tokio::test]
    async fn stale_disconnect_callback_cannot_clear_new_player() {
        let state = AppState::new();
        let old: Arc<dyn PlayerBackend> = Arc::new(FakePlayerBackend::new(PlayerKind::Vlc));
        let new: Arc<dyn PlayerBackend> = Arc::new(FakePlayerBackend::new(PlayerKind::Mplayer));
        *state.player.lock() = Some(new.clone());
        *state.last_player_kind.lock() = Some(PlayerKind::Mplayer);
        *state.player_connecting.lock() = true;

        clear_disconnected_player(&state, &old).await;

        assert!(state
            .player
            .lock()
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &new)));
        assert_eq!(*state.last_player_kind.lock(), Some(PlayerKind::Mplayer));
        assert!(*state.player_connecting.lock());
    }

    #[tokio::test]
    async fn stale_double_check_rewind_cannot_seek_a_new_generation() {
        let state = AppState::new();
        let fake = Arc::new(FakePlayerBackend::with_state(
            PlayerKind::Vlc,
            PlayerState {
                position: Some(12.0),
                ..PlayerState::default()
            },
        ));
        fake.set_poll_delay(Duration::from_millis(150));
        let player: Arc<dyn PlayerBackend> = fake.clone();
        *state.player.lock() = Some(player.clone());
        state
            .playback
            .install_load(LoadId(1), "a.mkv", "/media/a.mkv", player.clone());
        schedule_double_check_rewind(
            state.clone(),
            player.clone(),
            RewindGeneration::Load(LoadId(1)),
        );

        timeout(Duration::from_secs(1), async {
            loop {
                if fake
                    .commands()
                    .iter()
                    .any(|command| matches!(command, FakePlayerCommand::PollState))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        state
            .playback
            .install_load(LoadId(2), "b.mkv", "/media/b.mkv", player.clone());
        sleep(Duration::from_millis(250)).await;

        assert!(!fake
            .commands()
            .iter()
            .any(|command| matches!(command, FakePlayerCommand::SetPosition(0.0))));
    }

    #[tokio::test]
    async fn stale_loop_lease_cannot_rewind_after_an_aba_media_change() {
        let state = AppState::new();
        let fake = FakePlayerBackend::new(PlayerKind::Vlc);
        *state.player.lock() = Some(Arc::new(fake.clone()) as Arc<dyn PlayerBackend>);
        playback_runtime::replace_playlist(&state, vec!["a.mkv".into()], Some(0))
            .await
            .unwrap();
        playback_runtime::dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("a.mkv", Some(1), Some(120.0)),
            },
        )
        .await
        .unwrap();
        let lease = match playback_runtime::advance_after_eof(&state, "a.mkv", true, false, -1.0)
            .await
            .unwrap()
        {
            EofAction::Rewind(lease) => lease,
            _ => panic!("expected loop rewind lease"),
        };

        for filename in ["b.mkv", "a.mkv"] {
            playback_runtime::dispatch(
                &state,
                PlaybackEvent::PlayerMediaCommitted {
                    load_id: None,
                    media: CommittedMedia::new(filename, Some(1), Some(120.0)),
                },
            )
            .await
            .unwrap();
        }
        rewind_looping_media(&state, lease).await;

        assert!(!fake.commands().iter().any(|command| matches!(
            command,
            FakePlayerCommand::SetPosition(0.0) | FakePlayerCommand::SetPaused(false)
        )));
    }

    #[tokio::test]
    async fn media_change_cancels_delayed_loop_rewind_and_unpause() {
        let state = AppState::new();
        let fake = FakePlayerBackend::new(PlayerKind::Vlc);
        *state.player.lock() = Some(Arc::new(fake.clone()) as Arc<dyn PlayerBackend>);
        playback_runtime::replace_playlist(&state, vec!["a.mkv".into()], Some(0))
            .await
            .unwrap();
        playback_runtime::dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("a.mkv", Some(1), Some(120.0)),
            },
        )
        .await
        .unwrap();
        let lease = match playback_runtime::advance_after_eof(&state, "a.mkv", true, false, -1.0)
            .await
            .unwrap()
        {
            EofAction::Rewind(lease) => lease,
            _ => panic!("expected loop rewind lease"),
        };
        rewind_looping_media(&state, lease).await;

        playback_runtime::dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("b.mkv", Some(1), Some(120.0)),
            },
        )
        .await
        .unwrap();
        sleep(Duration::from_millis(650)).await;

        let commands = fake.commands();
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::SetPosition(0.0)))
                .count(),
            1
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::SetPaused(false)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn stop_player_shuts_down_fake_backend_and_is_idempotent() {
        let state = AppState::new();
        let fake = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        let player: Arc<dyn PlayerBackend> = fake.clone();
        *state.player.lock() = Some(player);
        *state.last_player_spawn.lock() = Some(std::time::Instant::now());
        *state.last_player_kind.lock() = Some(PlayerKind::Mpv);
        *state.player_connecting.lock() = true;
        *state.mpv_socket_path.lock() = Some("stale-socket".to_string());
        *state.mpv_runtime_dir.lock() = Some(TempDir::new().unwrap());

        stop_player(&state).await.unwrap();

        assert!(state.player.lock().is_none());
        assert!(state.player_process.lock().is_none());
        assert!(state.last_player_spawn.lock().is_none());
        assert!(state.last_player_kind.lock().is_none());
        assert!(!*state.player_connecting.lock());
        assert!(state.mpv_socket_path.lock().is_none());
        assert!(state.mpv_runtime_dir.lock().is_none());
        assert_eq!(fake.shutdown_count(), 1);
        assert_eq!(fake.commands(), vec![FakePlayerCommand::Shutdown]);

        stop_player(&state).await.unwrap();
        assert_eq!(fake.shutdown_count(), 1);
    }
}
