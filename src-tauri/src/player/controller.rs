use crate::app_state::{AppState, PlayerStateEvent};
use crate::client::media_index::resolve_exact_in_directory;
use crate::client::playback::{CommittedMedia, LoadId, PlaybackEvent};
use crate::client::playback_runtime;
use crate::commands::playlist::shared_playlists_enabled;
use crate::config::{SyncplayConfig, UnpauseAction};
use crate::network::connection::ConnectionState;
use crate::network::messages::{FileInfo, PlayState, ProtocolMessage, ReadyState, SetMessage};
use crate::player::backend::{player_kind_from_path_or_default, PlayerBackend, PlayerKind};
use crate::player::commands::{LoadfileOptionsSyntax, MpvCommand};
use crate::player::detection::normalize_iina_player_path;
use crate::player::mpc_api::MpcApiBackend;
use crate::player::mplayer_slave::MplayerBackend;
use crate::player::mpv_backend::MpvBackend;
use crate::player::mpv_ipc::MpvIpc;
use crate::player::properties::PlayerState;
use crate::player::vlc_syncplay::VlcSyncplayBackend;
use crate::utils::{
    apply_privacy, is_music_file, is_trustable_and_trusted, is_url, same_filename,
    PRIVACY_HIDDEN_FILENAME,
};
use regex::Regex;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tauri::Manager;
#[cfg(unix)]
use tempfile::Builder;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
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
const MPV_LAUNCH_ATTEMPTS: usize = 3;
const MPV_SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const MPV_VERSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const PLAYER_STARTUP_SCOPE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MPV_TERM_PLAYING_MESSAGE: &str = "<SyncplayUpdateFile>\nANS_filename=${filename}\nANS_length=${=duration:${=length:0}}\nANS_path=${path}\n</SyncplayUpdateFile>";

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
    ensure_player_connected_for_media(state, None, None, None)
        .await
        .map(|_| ())
}

pub async fn ensure_player_connected_for_session(
    state: &Arc<AppState>,
    expected_generation: u64,
) -> Result<(), String> {
    ensure_player_connected_for_media(state, None, Some(expected_generation), None)
        .await
        .map(|_| ())
}

// When multiple playback coordination locks are needed, the order is
// media_transition -> player_lifecycle -> dispatch. Dispatch only spawns load
// effects, so it never waits for player_lifecycle inline.
async fn ensure_player_connected_for_media(
    state: &Arc<AppState>,
    initial_media: Option<&str>,
    expected_generation: Option<u64>,
    expected_load: Option<&playback_runtime::MediaLoadStartupLease>,
) -> Result<bool, String> {
    let startup_epoch = state.player_startup_epoch.load(Ordering::Acquire);
    ensure_player_connected_for_media_at_epoch(
        state,
        initial_media,
        expected_generation,
        expected_load,
        startup_epoch,
    )
    .await
}

async fn ensure_player_connected_for_media_at_epoch(
    state: &Arc<AppState>,
    initial_media: Option<&str>,
    expected_generation: Option<u64>,
    expected_load: Option<&playback_runtime::MediaLoadStartupLease>,
    startup_epoch: u64,
) -> Result<bool, String> {
    let _lifecycle_guard = await_player_startup_operation(
        state,
        startup_epoch,
        expected_generation,
        expected_load,
        state.player_lifecycle.clone().lock_owned(),
    )
    .await?;
    if !player_startup_scope_is_current(state, startup_epoch, expected_generation, expected_load) {
        return Err("Connection session ended before player startup".to_string());
    }
    let current_player_connected = state
        .player
        .lock()
        .as_ref()
        .map(|player| player.is_connected());
    match current_player_connected {
        Some(true) => {
            tracing::debug!(
                "player_lifecycle: ensure connected skipped; backend already connected"
            );
            return Ok(false);
        }
        Some(false) => {
            return Err(
                "Disconnected player teardown must finish before a new startup".to_string(),
            );
        }
        None => {}
    }
    {
        let mut guard = state.player_connecting.lock();
        if *guard {
            tracing::debug!(
                "player_lifecycle: ensure connected skipped; connection already in progress"
            );
            return Ok(false);
        }
        *guard = true;
    }
    let _connecting_guard = PlayerConnectingGuard::new(&state.player_connecting);
    tracing::info!("player_lifecycle: connecting player backend");

    let config = state.config.lock().clone();
    let configured_player_path = resolve_player_path(&config);
    let kind = player_kind_from_path_or_default(&configured_player_path);
    if kind == PlayerKind::Mplayer && initial_media.is_none() {
        tracing::debug!(
            "player_lifecycle: deferring MPlayer startup until an initial file is available"
        );
        return Ok(false);
    }

    #[cfg(test)]
    let fake_player_factory = state.fake_player_factory.lock().clone();
    #[cfg(test)]
    if fake_player_factory.is_none() {
        return Err(
            "Real player launch is disabled in tests; install FakePlayerFactory".to_string(),
        );
    }
    #[cfg(test)]
    if let Some(factory) = fake_player_factory {
        let fake = Arc::new(factory.launch(kind));
        let fake_dyn = fake.clone() as Arc<dyn PlayerBackend>;
        if let Err(error) = await_player_startup_operation(
            state,
            startup_epoch,
            expected_generation,
            expected_load,
            prepare_player_after_connect(&fake_dyn),
        )
        .await
        {
            shutdown_player_handles(Some(fake_dyn), None, Some(kind)).await;
            return Err(error);
        }
        let load_dispatch_guard = if expected_load.is_some() {
            match await_player_startup_operation(
                state,
                startup_epoch,
                expected_generation,
                expected_load,
                state.playback.dispatch.lock(),
            )
            .await
            {
                Ok(guard) => Some(guard),
                Err(error) => {
                    shutdown_player_handles(Some(fake_dyn), None, Some(kind)).await;
                    return Err(error);
                }
            }
        } else {
            None
        };
        if !player_startup_scope_is_current(
            state,
            startup_epoch,
            expected_generation,
            expected_load,
        ) {
            drop(load_dispatch_guard);
            shutdown_player_handles(Some(fake_dyn), None, Some(kind)).await;
            return Err("Connection session ended during player startup".to_string());
        }
        if initial_media.is_some() && state.client_state.get_server_version().is_some() {
            if let Err(error) = fake.set_features() {
                tracing::warn!("Failed to send feature update to player: {error}");
            }
        }
        *state.player.lock() = Some(fake);
        *state.last_player_spawn.lock() = Some(Instant::now());
        *state.last_player_kind.lock() = Some(kind);
        tracing::info!(
            launch_count = factory.launch_count(),
            kind = ?kind,
            "player_lifecycle: fake player launched"
        );
        drop(load_dispatch_guard);
        return Ok(kind == PlayerKind::Mplayer && initial_media.is_some());
    }

    let args = build_player_arguments(&config, &configured_player_path);
    let player_path = if kind == PlayerKind::Iina {
        normalize_iina_player_path(&configured_player_path)
    } else {
        configured_player_path
    };
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
    let startup = async {
        let mut started_with_initial_media = false;
        let mut mpv_event_loop = None;
        let (backend, child) = match kind {
            PlayerKind::Mpv | PlayerKind::MpvNet | PlayerKind::Iina => {
                let (mpv, event_rx, mut child) = if should_spawn {
                    let mut connected = None;
                    for attempt in 1..=MPV_LAUNCH_ATTEMPTS {
                        let Some(mut spawned_child) = start_mpv_process_if_needed(
                            state,
                            &player_path,
                            kind,
                            &args,
                            &socket_path,
                            syncplayintf_path.as_deref(),
                        )?
                        else {
                            return Err("Player process is already running".to_string());
                        };

                        let launch_result: Result<_, String> = async {
                            wait_for_ipc_socket(
                                &mut spawned_child,
                                &socket_path,
                                kind,
                                MPV_SOCKET_WAIT_TIMEOUT,
                            )
                            .await?;
                            let mut mpv = MpvIpc::new(socket_path.clone());
                            let event_rx = mpv.connect().await.map_err(|error| {
                                format!("Failed to connect to mpv IPC: {error}")
                            })?;
                            if kind == PlayerKind::Iina {
                                prepare_iina_after_connect(&mpv, syncplayintf_path.as_deref())
                                    .await?;
                            }
                            Ok((mpv, event_rx))
                        }
                        .await;

                        match launch_result {
                            Ok((mpv, event_rx)) => {
                                connected = Some((mpv, event_rx, Some(spawned_child)));
                                break;
                            }
                            Err(error) => {
                                tracing::warn!(
                                    attempt,
                                    max_attempts = MPV_LAUNCH_ATTEMPTS,
                                    "Player launch attempt failed: {error}"
                                );
                                let _ = spawned_child.kill().await;
                                let _ = spawned_child.wait().await;
                            }
                        }
                    }
                    connected.ok_or_else(|| {
                        let player_name = if kind == PlayerKind::Iina {
                            "IINA"
                        } else {
                            "MPV"
                        };
                        format!("{player_name} process retry limit reached.")
                    })?
                } else {
                    let mut mpv = MpvIpc::new(socket_path.clone());
                    let event_rx = mpv
                        .connect()
                        .await
                        .map_err(|error| format!("Failed to connect to mpv IPC: {error}"))?;
                    if kind == PlayerKind::Iina {
                        prepare_iina_after_connect(&mpv, syncplayintf_path.as_deref()).await?;
                    }
                    (mpv, event_rx, None)
                };
                let stdout = child.as_mut().and_then(|process| process.stdout.take());
                let queried_version_flags = query_mpv_version_flags(&mpv).await?;
                let version_flags = match kind {
                    PlayerKind::Iina | PlayerKind::MpvNet => MpvVersionFlags {
                        osc_visibility_change_compatible: true,
                        loadfile_options_syntax: queried_version_flags
                            .and_then(|flags| flags.loadfile_options_syntax),
                    },
                    _ => match queried_version_flags {
                        Some(flags) => flags,
                        None => check_mpv_version(&player_path).await?,
                    },
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
                mpv_event_loop = Some((backend, event_rx));
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
                    MplayerBackend::start(&player_path, &args, initial_media)
                        .await
                        .map_err(|e| e.to_string())?
                } else {
                    return Err("Player not running".to_string());
                };
                started_with_initial_media = initial_media.is_some();
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
        Ok((backend, child, started_with_initial_media, mpv_event_loop))
    };
    let (backend, child, started_with_initial_media, mpv_event_loop) =
        await_player_startup_operation(
            state,
            startup_epoch,
            expected_generation,
            expected_load,
            startup,
        )
        .await??;
    let load_dispatch_guard = if expected_load.is_some() {
        match await_player_startup_operation(
            state,
            startup_epoch,
            expected_generation,
            expected_load,
            state.playback.dispatch.lock(),
        )
        .await
        {
            Ok(guard) => Some(guard),
            Err(error) => {
                drop(mpv_event_loop);
                shutdown_player_handles(Some(backend), child, Some(kind)).await;
                return Err(error);
            }
        }
    } else {
        None
    };
    if !player_startup_scope_is_current(state, startup_epoch, expected_generation, expected_load) {
        drop(load_dispatch_guard);
        shutdown_player_handles(Some(backend), child, Some(kind)).await;
        return Err("Connection session ended during player startup".to_string());
    }
    if initial_media.is_some() && state.client_state.get_server_version().is_some() {
        if let Err(error) = backend.set_features() {
            tracing::warn!("Failed to send feature update to player: {error}");
        }
    }

    let installed = {
        let mut player = state.player.lock();
        if state.player_startup_epoch.load(Ordering::Acquire) == startup_epoch {
            *player = Some(backend.clone());
            true
        } else {
            false
        }
    };
    if !installed {
        drop(load_dispatch_guard);
        shutdown_player_handles(Some(backend), child, Some(kind)).await;
        return Err("Player startup was cancelled before installation".to_string());
    }
    if should_spawn {
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
    if let Some((backend, event_rx)) = mpv_event_loop {
        backend.spawn_event_loop(event_rx);
    }
    drop(load_dispatch_guard);
    Ok(started_with_initial_media)
}

fn player_startup_scope_is_current(
    state: &Arc<AppState>,
    startup_epoch: u64,
    expected_generation: Option<u64>,
    expected_load: Option<&playback_runtime::MediaLoadStartupLease>,
) -> bool {
    state.player_startup_epoch.load(Ordering::Acquire) == startup_epoch
        && expected_generation.is_none_or(|expected| {
            state.connection_session_generation.load(Ordering::Acquire) == expected
        })
        && expected_load
            .is_none_or(|lease| playback_runtime::is_current_media_load_startup(state, lease))
}

async fn wait_for_player_startup_scope_end(
    state: &Arc<AppState>,
    startup_epoch: u64,
    expected_generation: Option<u64>,
    expected_load: Option<&playback_runtime::MediaLoadStartupLease>,
) {
    loop {
        if !player_startup_scope_is_current(
            state,
            startup_epoch,
            expected_generation,
            expected_load,
        ) {
            return;
        }
        sleep(PLAYER_STARTUP_SCOPE_POLL_INTERVAL).await;
    }
}

async fn await_player_startup_operation<T>(
    state: &Arc<AppState>,
    startup_epoch: u64,
    expected_generation: Option<u64>,
    expected_load: Option<&playback_runtime::MediaLoadStartupLease>,
    operation: impl Future<Output = T>,
) -> Result<T, String> {
    tokio::select! {
        biased;
        _ = wait_for_player_startup_scope_end(
            state,
            startup_epoch,
            expected_generation,
            expected_load,
        ) => Err("Player startup was cancelled".to_string()),
        result = operation => Ok(result),
    }
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
    state.player_startup_epoch.fetch_add(1, Ordering::AcqRel);
    let _lifecycle_guard = state.player_lifecycle.lock().await;
    stop_player_locked(state).await
}

pub(crate) async fn stop_player_instance(
    state: &Arc<AppState>,
    instance_id: u64,
) -> Result<(), String> {
    let stopped_current_player = {
        let _lifecycle_guard = state.player_lifecycle.lock().await;
        let is_current = state
            .player
            .lock()
            .as_ref()
            .is_some_and(|player| player.instance_id() == instance_id);
        if is_current {
            state.player_startup_epoch.fetch_add(1, Ordering::AcqRel);
            stop_player_locked(state).await?;
            true
        } else {
            false
        }
    };

    if stopped_current_player {
        disconnect_server_after_player_exit(state).await?;
    }
    Ok(())
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

    shutdown_player_handles(player, child, player_kind).await;
    Ok(())
}

async fn shutdown_player_handles(
    player: Option<Arc<dyn PlayerBackend>>,
    child: Option<Child>,
    player_kind: Option<PlayerKind>,
) {
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
                let global = state.effective_global_state();
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
                    let play_state = if recently_rewound(&state) || recently_advanced(&state) {
                        let global_state = state.effective_global_state();
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
                        None,
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
    let stopped_current_player = {
        let _lifecycle_guard = state.player_lifecycle.lock().await;
        let is_current = state
            .player
            .lock()
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, disconnected));
        if is_current {
            state.player_startup_epoch.fetch_add(1, Ordering::AcqRel);
            if let Err(error) = stop_player_locked(state).await {
                tracing::warn!("Failed to stop disconnected player: {error}");
            }
            true
        } else {
            false
        }
    };

    if stopped_current_player {
        if let Err(error) = disconnect_server_after_player_exit(state).await {
            tracing::warn!("Failed to disconnect server after player exit: {}", error);
        }
    }
}

async fn disconnect_server_after_player_exit(state: &Arc<AppState>) -> Result<(), String> {
    tracing::info!("player_lifecycle: player exited unexpectedly; disconnecting server session");
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
    crate::commands::connection::disconnect_from_server_state(state).await
}

pub async fn load_media_by_name(
    state: &Arc<AppState>,
    filename: &str,
    reset_position: bool,
    load_id: LoadId,
    startup_lease: &playback_runtime::MediaLoadStartupLease,
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
        let media_path = current_media_path_for_target(state, filename)
            .or_else(|| state.media_index.resolve_path(filename))
            .or_else(|| resolve_media_path(&config.player.media_directories, filename))
            .ok_or_else(|| {
                LoadMediaError::MediaNotFound(format!(
                    "File not found in media directories: {}",
                    filename
                ))
            })?;
        (media_path.to_string_lossy().into_owned(), false)
    };

    let configured_kind = player_kind_from_path_or_default(&resolve_player_path(&config));
    let serializes_initial_mplayer_start =
        configured_kind == PlayerKind::Mplayer && !state.is_player_connected();
    let mut transition_guard = if serializes_initial_mplayer_start {
        Some(state.playback.media_transition.lock().await)
    } else {
        None
    };
    let started_with_initial_media = match ensure_player_connected_for_media(
        state,
        Some(&media_path),
        None,
        Some(startup_lease),
    )
    .await
    {
        Ok(started) => started,
        Err(_) if !playback_runtime::is_current_media_load_startup(state, startup_lease) => {
            return Err(LoadMediaError::Superseded);
        }
        Err(error) => return Err(LoadMediaError::Failed(error)),
    };
    if transition_guard.is_none() {
        transition_guard = Some(state.playback.media_transition.lock().await);
    }
    let _transition_guard = transition_guard.expect("media transition guard must be acquired");
    let (player, lease) = {
        let _lifecycle_guard = state.player_lifecycle.lock().await;
        let dispatch_guard = state.playback.dispatch.lock().await;
        if !playback_runtime::is_current_media_load_startup(state, startup_lease) {
            return Err(LoadMediaError::Superseded);
        }
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
        drop(dispatch_guard);
        if !started_with_initial_media {
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

fn current_media_path_for_target(state: &Arc<AppState>, filename: &str) -> Option<PathBuf> {
    let current_name = state.client_state.get_file()?;
    if !same_filename(Some(filename), Some(&current_name)) {
        return None;
    }
    let player = state.player.lock().clone()?;
    let player_state = player.get_state();
    let current_path = normalize_local_path(player_state.path.as_deref()?)?;
    let player_name = player_state
        .filename
        .as_deref()
        .or_else(|| current_path.file_name().and_then(|name| name.to_str()))?;
    if !same_filename(Some(filename), Some(player_name)) {
        return None;
    }
    Some(current_path)
}

async fn sync_mpc_after_file_change(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    reset_position: bool,
    load_id: Option<LoadId>,
) -> Result<(), String> {
    if !can_sync_committed_media(state, player, load_id) {
        return Err("MPC media commit was superseded before settling".to_string());
    }
    let global = state.effective_global_state();
    let position = if reset_position { 0.0 } else { global.position };
    player
        .settle_media_change(global.paused, position)
        .await
        .map_err(|error| format!("Failed to settle MPC media state: {error}"))?;
    if !can_sync_committed_media(state, player, load_id) {
        return Err("MPC media commit was superseded while settling".to_string());
    }
    Ok(())
}

async fn sync_generic_after_file_change(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    reset_position: bool,
    load_id: Option<LoadId>,
) {
    let global = state.effective_global_state();
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
    let global = state.effective_global_state();
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
            .map(|load| Arc::ptr_eq(&load.player, player) && !load.is_cancelled())
            .unwrap_or_else(|| {
                state.playback.current_load().is_none()
                    && state.playback.state.lock().pending_load.is_none()
            }),
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
    resolve_resource_path(
        state,
        &[
            "resources/placeholder.png",
            "placeholder.png",
            "src-tauri/resources/placeholder.png",
            "icon.svg",
        ],
    )
}

fn resolve_syncplay_lua_path(state: &AppState) -> Option<PathBuf> {
    resolve_resource_path(
        state,
        &[
            "resources/syncplay.lua",
            "syncplay.lua",
            "src-tauri/resources/syncplay.lua",
        ],
    )
}

fn resolve_syncplayintf_path(state: &AppState) -> Option<PathBuf> {
    resolve_resource_path(
        state,
        &[
            "resources/syncplayintf.lua",
            "syncplayintf.lua",
            "src-tauri/resources/syncplayintf.lua",
        ],
    )
}

fn resolve_resource_path(state: &AppState, candidates: &[&str]) -> Option<PathBuf> {
    if let Some(handle) = state.app_handle.lock().clone() {
        for name in candidates {
            if let Ok(path) = handle
                .path()
                .resolve(*name, tauri::path::BaseDirectory::Resource)
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

#[derive(Debug)]
struct MpvVersionFlags {
    osc_visibility_change_compatible: bool,
    loadfile_options_syntax: Option<LoadfileOptionsSyntax>,
}

async fn check_mpv_version(player_path: &str) -> Result<MpvVersionFlags, String> {
    let Ok(Some(output)) = run_mpv_version_command(player_path).await else {
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

async fn query_mpv_version_flags(mpv: &MpvIpc) -> Result<Option<MpvVersionFlags>, String> {
    let Some(value) = mpv
        .get_property_value("mpv-version")
        .await
        .map_err(|error| format!("Failed to query mpv version: {error}"))?
    else {
        return Ok(None);
    };
    let Some(version) = value.as_str() else {
        return Ok(None);
    };
    Ok(parse_mpv_version_flags(version).ok())
}

async fn run_mpv_version_command(
    player_path: &str,
) -> std::io::Result<Option<std::process::Output>> {
    let mut command = Command::new(player_path);
    command
        .arg("--version")
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_hidden_version_command(command.as_std_mut());
    let mut child = command.spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .expect("version command stdout must be piped");
    let output = async {
        let stdout = async {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await?;
            Ok::<_, std::io::Error>(bytes)
        };
        let (status, stdout) = tokio::try_join!(child.wait(), stdout)?;
        Ok::<_, std::io::Error>(std::process::Output {
            status,
            stdout,
            stderr: Vec::new(),
        })
    };
    match timeout(MPV_VERSION_COMMAND_TIMEOUT, output).await {
        Ok(result) => result.map(Some),
        Err(_) => {
            let _ = child.kill().await;
            let _ = timeout(
                Duration::from_millis(PLAYER_PROCESS_KILL_TIMEOUT_MS),
                child.wait(),
            )
            .await;
            Ok(None)
        }
    }
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

fn build_mpv_launch_arguments(
    kind: PlayerKind,
    user_args: &[String],
    socket_path: &str,
    placeholder_path: Option<&Path>,
    syncplayintf_path: Option<&Path>,
) -> Result<Vec<String>, String> {
    if kind == PlayerKind::Iina {
        let placeholder_path = placeholder_path
            .ok_or_else(|| "Placeholder asset not found for IINA startup".to_string())?;
        let mut options = Vec::new();
        for argument in user_args {
            if let Some((name, value)) = parse_player_argument(argument, "yes", false) {
                set_launch_option(&mut options, name, value);
            }
        }
        set_default_launch_option(
            &mut options,
            "mpv-input-ipc-server".to_string(),
            socket_path.to_string(),
        );

        let mut arguments = vec![
            "--no-stdin".to_string(),
            placeholder_path.to_string_lossy().to_string(),
        ];
        arguments.extend(render_launch_options(options));
        return Ok(arguments);
    }

    let mut options = vec![
        ("force-window".to_string(), "yes".to_string()),
        ("idle".to_string(), "yes".to_string()),
        ("hr-seek".to_string(), "always".to_string()),
        ("keep-open".to_string(), "always".to_string()),
        ("input-terminal".to_string(), "no".to_string()),
        (
            "term-playing-msg".to_string(),
            MPV_TERM_PLAYING_MESSAGE.to_string(),
        ),
        ("keep-open-pause".to_string(), "yes".to_string()),
    ];
    if let Some(script_path) = syncplayintf_path {
        options.push((
            "script".to_string(),
            script_path.to_string_lossy().to_string(),
        ));
    }
    for argument in user_args {
        if let Some((name, value)) = parse_player_argument(argument, "", true) {
            set_launch_option(&mut options, name, value);
        }
    }
    if kind == PlayerKind::MpvNet {
        set_launch_option(&mut options, "auto-load-folder".to_string(), String::new());
    }
    set_default_launch_option(
        &mut options,
        "input-ipc-server".to_string(),
        socket_path.to_string(),
    );
    set_default_launch_option(&mut options, "terminal".to_string(), "no".to_string());
    Ok(render_launch_options(options))
}

fn parse_player_argument(
    argument: &str,
    missing_value: &str,
    strip_quoted_value: bool,
) -> Option<(String, String)> {
    let argument = argument
        .strip_prefix("--")
        .or_else(|| argument.strip_prefix('-'))
        .unwrap_or(argument);
    if argument.trim().is_empty() {
        return None;
    }
    let (name, value) = argument
        .split_once('=')
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .unwrap_or_else(|| (argument.to_string(), missing_value.to_string()));
    let value =
        if strip_quoted_value && value.len() >= 2 && value.starts_with('"') && value.ends_with('"')
        {
            value[1..value.len() - 1].to_string()
        } else {
            value
        };
    Some((name, value))
}

fn set_launch_option(options: &mut Vec<(String, String)>, name: String, value: String) {
    if let Some((_, current_value)) = options
        .iter_mut()
        .find(|(current_name, _)| *current_name == name)
    {
        *current_value = value;
    } else {
        options.push((name, value));
    }
}

fn set_default_launch_option(options: &mut Vec<(String, String)>, name: String, value: String) {
    if !options
        .iter()
        .any(|(current_name, _)| *current_name == name)
    {
        options.push((name, value));
    }
}

fn render_launch_options(options: Vec<(String, String)>) -> Vec<String> {
    options
        .into_iter()
        .map(|(name, value)| format!("--{}={value}", name.replace('_', "-")))
        .collect()
}

fn build_iina_prepare_commands(syncplayintf_path: &Path) -> Vec<MpvCommand> {
    let properties = [
        ("geometry", "25%+100+100"),
        ("idle", "yes"),
        ("hr-seek", "always"),
        ("input-terminal", "no"),
        ("term-playing-msg", MPV_TERM_PLAYING_MESSAGE),
        ("keep-open-pause", "yes"),
    ];
    let mut commands = properties
        .into_iter()
        .map(|(property, value)| {
            MpvCommand::set_property(property, serde_json::Value::String(value.to_string()), 0)
        })
        .collect::<Vec<_>>();
    commands.push(MpvCommand {
        command: vec![
            serde_json::Value::String("load-script".to_string()),
            serde_json::Value::String(syncplayintf_path.to_string_lossy().to_string()),
        ],
        request_id: None,
        load_id: None,
    });
    commands
}

async fn prepare_iina_after_connect(
    mpv: &MpvIpc,
    syncplayintf_path: Option<&Path>,
) -> Result<(), String> {
    let syncplayintf_path = syncplayintf_path
        .ok_or_else(|| "Syncplay MPV interface not found for IINA startup".to_string())?;
    for command in build_iina_prepare_commands(syncplayintf_path) {
        mpv.send_command_async(command)
            .await
            .map_err(|error| format!("Failed to prepare IINA: {error}"))?;
    }
    Ok(())
}

fn start_mpv_process_if_needed(
    state: &Arc<AppState>,
    player_path: &str,
    kind: PlayerKind,
    args: &[String],
    socket_path: &str,
    syncplayintf_path: Option<&Path>,
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

    #[cfg(unix)]
    if Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path)
            .map_err(|error| format!("Failed to remove stale MPV IPC socket: {error}"))?;
    }

    let mut cmd = Command::new(player_path);
    cmd.kill_on_drop(true);
    cmd.env_remove("TERM");
    let placeholder_path = (kind == PlayerKind::Iina)
        .then(|| resolve_placeholder_path(state))
        .flatten();
    let full_args = build_mpv_launch_arguments(
        kind,
        args,
        socket_path,
        placeholder_path.as_deref(),
        syncplayintf_path,
    )?;
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
    kind: PlayerKind,
    timeout: Duration,
) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if Path::new(socket_path).exists() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            if kind != PlayerKind::Iina || !status.success() {
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
    commit_player_state_with_source(
        state,
        player,
        player_state,
        load_id,
        PlayerMediaCommitSource::Observed,
    )
    .await
}

pub(crate) async fn commit_external_player_state(
    state: &Arc<AppState>,
    player: &Arc<dyn PlayerBackend>,
    player_state: &PlayerState,
) -> Result<playback_runtime::DispatchResult, String> {
    commit_player_state_with_source(
        state,
        Some(player),
        player_state,
        None,
        PlayerMediaCommitSource::ExplicitExternal,
    )
    .await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlayerMediaCommitSource {
    Observed,
    ExplicitExternal,
}

async fn commit_player_state_with_source(
    state: &Arc<AppState>,
    player: Option<&Arc<dyn PlayerBackend>>,
    player_state: &PlayerState,
    load_id: Option<LoadId>,
    source: PlayerMediaCommitSource,
) -> Result<playback_runtime::DispatchResult, String> {
    if is_placeholder_file(state, player_state) {
        return Ok(playback_runtime::DispatchResult::default());
    }
    let Some(media) = committed_media_from_player_state(player_state) else {
        return Ok(playback_runtime::DispatchResult::default());
    };
    let load_id = (source == PlayerMediaCommitSource::Observed)
        .then(|| {
            load_id.or_else(|| {
                player.and_then(|player| {
                    state
                        .playback
                        .matching_load(player, &media.name)
                        .map(|load| load.id)
                })
            })
        })
        .flatten();
    let event = match source {
        PlayerMediaCommitSource::Observed => PlaybackEvent::PlayerMediaCommitted {
            load_id,
            media: media.clone(),
        },
        PlayerMediaCommitSource::ExplicitExternal => PlaybackEvent::PlayerMediaOpenedExternally {
            media: media.clone(),
        },
    };
    let mpc_player = player.filter(|player| {
        matches!(player.kind(), PlayerKind::MpcHc | PlayerKind::MpcBe) && state.is_connected()
    });
    let outcome = if let Some(player) = mpc_player {
        let _dispatch_guard = state.playback.dispatch.lock().await;
        let preview = playback_runtime::preview_playback_event(state, event.clone());
        if preview.media_accepted {
            sync_mpc_after_file_change(state, player, preview.media_reset, preview.completed_load)
                .await?;
        }
        playback_runtime::dispatch_all_outcome_locked(state, [event])
    } else {
        playback_runtime::dispatch_all_outcome(state, [event]).await
    };
    let result = outcome.result;

    if let (true, Some(player)) = (result.media_accepted && state.is_connected(), player) {
        match player.kind() {
            PlayerKind::MpcHc | PlayerKind::MpcBe => {}
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

    let (name, size) = apply_privacy(
        Some(media.name.clone()),
        media.size,
        &config.user.filename_privacy_mode,
        &config.user.filesize_privacy_mode,
    );

    state.client_state.set_file_info(FileInfo {
        name: name.clone(),
        size: size.clone(),
        duration: media.duration,
    });
    *state.last_updated_file_time.lock() = Some(std::time::Instant::now());

    let Some(connection) = state.connection.lock().clone() else {
        return Ok(());
    };
    if connection.state() != ConnectionState::Authenticated {
        return Ok(());
    }

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
        let started_at = tokio::time::Instant::now();
        for delay in DOUBLE_CHECK_REWIND_DELAYS {
            tokio::time::sleep_until(started_at + Duration::from_secs_f64(delay)).await;
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
    if let Some(path) = player_state.path.as_deref() {
        let path = Path::new(path);
        if resolve_placeholder_path(state).is_some_and(|placeholder_path| path == placeholder_path)
        {
            return true;
        }
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
    #[cfg(unix)]
    use super::query_mpv_version_flags;
    use super::{
        await_player_startup_operation, build_iina_prepare_commands, build_mpv_launch_arguments,
        check_mpv_version, clear_disconnected_player, commit_player_state,
        current_media_path_for_target, ensure_player_connected, ensure_player_connected_for_media,
        ensure_player_connected_for_media_at_epoch, parse_mpv_version_flags, resolve_media_path,
        rewind_looping_media, schedule_double_check_rewind, send_committed_file_update,
        should_pause_on_prepare, stop_player, stop_player_instance, LoadfileOptionsSyntax,
        RewindGeneration, MPV_LAUNCH_ATTEMPTS, MPV_SOCKET_WAIT_TIMEOUT, MPV_TERM_PLAYING_MESSAGE,
        MPV_VERSION_COMMAND_TIMEOUT,
    };
    use crate::app_state::AppState;
    use crate::client::playback::{
        CommittedMedia, LoadId, PlaybackEffect, PlaybackEvent, PlaybackState,
    };
    use crate::client::playback_runtime::{self, EofAction};
    use crate::config::PrivacyMode;
    use crate::network::connection::Connection;
    use crate::network::fake_server::FakeSyncplayServer;
    use crate::network::messages::{FileSizeInfo, ProtocolMessage};
    use crate::player::backend::{FakePlayerBackend, FakePlayerCommand, PlayerBackend, PlayerKind};
    use crate::player::properties::PlayerState;
    use crate::player::{mpv_backend::MpvBackend, mpv_ipc::MpvIpc as TestMpvIpc};
    use crate::utils::{hash_filename, hash_filesize};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use tempfile::TempDir;
    #[cfg(unix)]
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::time::{sleep, timeout, Duration};

    async fn attach_connected_server(
        state: &Arc<AppState>,
    ) -> (FakeSyncplayServer, Arc<Connection>) {
        let server = FakeSyncplayServer::start().await.unwrap();
        let connection = Arc::new(Connection::new());
        let _ = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();
        connection.set_authenticated();
        *state.connection.lock() = Some(connection.clone());
        (server, connection)
    }

    #[tokio::test]
    async fn committed_file_update_uses_complete_privacy_snapshot_after_login() {
        let state = AppState::new();
        state.server_features.lock().max_filename_length = Some(4);
        {
            let mut config = state.config.lock();
            config.user.filename_privacy_mode = PrivacyMode::SendHashed;
            config.user.filesize_privacy_mode = PrivacyMode::SendHashed;
        }
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let connection = Arc::new(Connection::new());
        let _ = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();
        *state.connection.lock() = Some(connection.clone());
        let media = CommittedMedia::new("完整文件名-example.mkv", Some(123_456), Some(90.0));
        let expected_name = hash_filename(&media.name, true);
        let expected_size = hash_filesize(123_456);

        send_committed_file_update(&state, &media).unwrap();

        let local = state.client_state.get_file_info();
        assert_eq!(local.name.as_deref(), Some(expected_name.as_str()));
        assert!(matches!(
            local.size.as_ref(),
            Some(FileSizeInfo::Text(size)) if size == &expected_size
        ));
        assert!(timeout(Duration::from_millis(25), server.next_received())
            .await
            .is_err());

        connection.set_authenticated();
        send_committed_file_update(&state, &media).unwrap();
        let message = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        let ProtocolMessage::Set { Set } = message else {
            panic!("expected Set.file update");
        };
        let file = Set.file.expect("file update missing");
        assert_eq!(file.name.as_deref(), Some(expected_name.as_str()));
        assert!(matches!(
            file.size.as_ref(),
            Some(FileSizeInfo::Text(size)) if size == &expected_size
        ));
        assert_ne!(expected_name, hash_filename("完整", true));

        server.close();
    }

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
    fn media_directory_lookup_requires_an_exact_filename() {
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join("A_B.mkv"), b"test").unwrap();
        let directories = vec![directory.path().to_string_lossy().to_string()];

        assert!(resolve_media_path(&directories, "A-B.mkv").is_none());
    }

    #[test]
    fn media_directory_lookup_preserves_relative_components() {
        let directory = TempDir::new().unwrap();
        let nested = directory.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let file = nested.join("movie.mkv");
        fs::write(&file, b"test").unwrap();
        let directories = vec![directory.path().to_string_lossy().to_string()];

        assert_eq!(
            resolve_media_path(&directories, "nested/movie.mkv"),
            Some(file)
        );
    }

    #[test]
    fn current_media_shortcut_matches_the_privacy_transformed_name() {
        let state = AppState::new();
        let current_path = PathBuf::from("/media/current/movie.mkv");
        let player = Arc::new(FakePlayerBackend::with_state(
            PlayerKind::Mpv,
            PlayerState {
                path: Some(current_path.to_string_lossy().into_owned()),
                ..PlayerState::default()
            },
        ));
        *state.player.lock() = Some(player);
        state
            .client_state
            .set_file(Some(hash_filename("movie.mkv", true)));

        assert_eq!(
            current_media_path_for_target(&state, "movie.mkv"),
            Some(current_path)
        );
    }

    #[test]
    fn current_media_shortcut_rejects_a_player_path_from_a_newer_load() {
        let state = AppState::new();
        let player = Arc::new(FakePlayerBackend::with_state(
            PlayerKind::Mpv,
            PlayerState {
                filename: Some("b.mkv".to_string()),
                path: Some("/media/b.mkv".to_string()),
                ..PlayerState::default()
            },
        ));
        *state.player.lock() = Some(player);
        state.client_state.set_file(Some("a.mkv".to_string()));

        assert_eq!(current_media_path_for_target(&state, "a.mkv"), None);
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

    #[tokio::test]
    async fn mplayer_start_is_deferred_until_an_initial_file_is_available() {
        let state = Arc::new(AppState::new());
        state.config.lock().player.player_path = "mplayer".to_string();

        ensure_player_connected(&state).await.unwrap();

        assert!(!state.is_player_connected());
        assert!(state.player.lock().is_none());
    }

    #[tokio::test]
    async fn mplayer_initial_file_is_consumed_by_the_player_launch() {
        let state = Arc::new(AppState::new());
        state.config.lock().player.player_path = "mplayer".to_string();
        *state.fake_player_factory.lock() = Some(Arc::new(Default::default()));

        let consumed =
            ensure_player_connected_for_media(&state, Some("/media/example.mkv"), None, None)
                .await
                .unwrap();

        assert!(consumed);
        assert_eq!(
            state.player.lock().as_ref().unwrap().kind(),
            PlayerKind::Mplayer
        );
    }

    #[tokio::test]
    async fn mpc_media_settles_before_the_file_is_published() {
        let state = Arc::new(AppState::new());
        let fake = FakePlayerBackend::new(PlayerKind::MpcHc);
        fake.set_settle_delay(Duration::from_millis(100));
        let player = Arc::new(fake.clone()) as Arc<dyn PlayerBackend>;
        *state.player.lock() = Some(player.clone());
        state
            .client_state
            .set_global_state(27.0, true, Some("peer".to_string()));
        let (mut server, _connection) = attach_connected_server(&state).await;
        let player_state = PlayerState {
            filename: Some("movie.mkv".to_string()),
            path: Some("/media/movie.mkv".to_string()),
            duration: Some(90.0),
            ..PlayerState::default()
        };

        let commit_state = state.clone();
        let commit_player = player.clone();
        let commit = tokio::spawn(async move {
            commit_player_state(&commit_state, Some(&commit_player), &player_state, None).await
        });
        timeout(Duration::from_secs(1), async {
            loop {
                if fake
                    .commands()
                    .iter()
                    .any(|command| matches!(command, FakePlayerCommand::SetPaused(true)))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        assert!(timeout(Duration::from_millis(25), server.next_received())
            .await
            .is_err());
        let result = commit.await.unwrap().unwrap();
        assert!(result.media_accepted);
        assert!(matches!(
            timeout(Duration::from_secs(1), server.next_received())
                .await
                .unwrap(),
            Some(crate::network::messages::ProtocolMessage::Set { .. })
        ));
        assert_eq!(
            fake.commands(),
            vec![
                FakePlayerCommand::SetPaused(true),
                FakePlayerCommand::SetPaused(true),
                FakePlayerCommand::SetPosition(27.0),
            ]
        );
    }

    #[tokio::test]
    async fn failed_mpc_settle_does_not_publish_or_complete_the_load() {
        let state = Arc::new(AppState::new());
        let fake = FakePlayerBackend::new(PlayerKind::MpcHc);
        fake.set_settle_error("settle failed");
        let player = Arc::new(fake) as Arc<dyn PlayerBackend>;
        *state.player.lock() = Some(player.clone());
        let load_id = {
            let mut playback = PlaybackState::new(vec!["movie.mkv".to_string()]);
            let effects = playback.reduce(PlaybackEvent::LocalSelect {
                index: 0,
                reset_position: true,
            });
            assert!(matches!(
                effects.as_slice(),
                [PlaybackEffect::SendPlaylistIndex { index: 0, .. }]
            ));
            let effects = playback.reduce(PlaybackEvent::ServerIndex {
                index: Some(0),
                reset_position: true,
            });
            *state.playback.state.lock() = playback;
            match effects.as_slice() {
                [PlaybackEffect::Load { load_id, .. }] => *load_id,
                _ => panic!("expected one load effect"),
            }
        };
        state
            .playback
            .install_load(load_id, "movie.mkv", "/media/movie.mkv", player.clone());
        let (mut server, _connection) = attach_connected_server(&state).await;
        let player_state = PlayerState {
            filename: Some("movie.mkv".to_string()),
            path: Some("/media/movie.mkv".to_string()),
            duration: Some(90.0),
            ..PlayerState::default()
        };

        let error = commit_player_state(&state, Some(&player), &player_state, None)
            .await
            .unwrap_err();

        assert!(error.contains("Failed to settle MPC media state"));
        assert_eq!(
            state.playback.snapshot().pending_load.map(|load| load.id),
            Some(load_id)
        );
        assert!(state.playback.active_load(load_id).is_some());
        assert!(state.client_state.get_file().is_none());
        assert!(timeout(Duration::from_millis(25), server.next_received())
            .await
            .is_err());
    }

    #[test]
    fn iina_launch_arguments_match_original_cli_contract() {
        let arguments = build_mpv_launch_arguments(
            PlayerKind::Iina,
            &["--profile=cinema".into(), "--osc".into()],
            "/tmp/syncplay-mpv",
            Some(std::path::Path::new("/resources/placeholder.png")),
            Some(std::path::Path::new("/resources/syncplayintf.lua")),
        )
        .unwrap();

        assert_eq!(
            arguments,
            vec![
                "--no-stdin",
                "/resources/placeholder.png",
                "--profile=cinema",
                "--osc=yes",
                "--mpv-input-ipc-server=/tmp/syncplay-mpv",
            ]
        );
        assert!(!arguments.iter().any(|argument| {
            [
                "sub-auto",
                "sid=auto",
                "force-window",
                "keep-open",
                "script=",
            ]
            .iter()
            .any(|forbidden| argument.contains(forbidden))
        }));
    }

    #[test]
    fn mpv_launch_arguments_match_original_default_order_and_overrides() {
        let arguments = build_mpv_launch_arguments(
            PlayerKind::Mpv,
            &["--keep-open=no".into(), "--profile=cinema".into()],
            "/tmp/syncplay-mpv",
            None,
            Some(std::path::Path::new("/resources/syncplayintf.lua")),
        )
        .unwrap();

        assert_eq!(
            arguments,
            vec![
                "--force-window=yes".to_string(),
                "--idle=yes".to_string(),
                "--hr-seek=always".to_string(),
                "--keep-open=no".to_string(),
                "--input-terminal=no".to_string(),
                format!("--term-playing-msg={MPV_TERM_PLAYING_MESSAGE}"),
                "--keep-open-pause=yes".to_string(),
                "--script=/resources/syncplayintf.lua".to_string(),
                "--profile=cinema".to_string(),
                "--input-ipc-server=/tmp/syncplay-mpv".to_string(),
                "--terminal=no".to_string(),
            ]
        );
    }

    #[test]
    fn iina_prepare_commands_match_original_post_connect_order() {
        let commands =
            build_iina_prepare_commands(std::path::Path::new("/resources/syncplayintf.lua"));
        let wire_commands = commands
            .into_iter()
            .map(|command| command.command)
            .collect::<Vec<_>>();

        assert_eq!(
            wire_commands,
            vec![
                serde_json::json!(["set_property", "geometry", "25%+100+100"])
                    .as_array()
                    .unwrap()
                    .clone(),
                serde_json::json!(["set_property", "idle", "yes"])
                    .as_array()
                    .unwrap()
                    .clone(),
                serde_json::json!(["set_property", "hr-seek", "always"])
                    .as_array()
                    .unwrap()
                    .clone(),
                serde_json::json!(["set_property", "input-terminal", "no"])
                    .as_array()
                    .unwrap()
                    .clone(),
                serde_json::json!(["set_property", "term-playing-msg", MPV_TERM_PLAYING_MESSAGE])
                    .as_array()
                    .unwrap()
                    .clone(),
                serde_json::json!(["set_property", "keep-open-pause", "yes"])
                    .as_array()
                    .unwrap()
                    .clone(),
                serde_json::json!(["load-script", "/resources/syncplayintf.lua"])
                    .as_array()
                    .unwrap()
                    .clone(),
            ]
        );
    }

    #[test]
    fn mpv_launch_retry_policy_matches_original() {
        assert_eq!(MPV_LAUNCH_ATTEMPTS, 3);
        assert_eq!(MPV_SOCKET_WAIT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(MPV_VERSION_COMMAND_TIMEOUT, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn missing_mpv_version_check_matches_original_unknown_version_fallback() {
        let flags = check_mpv_version("/path/to/missing/mpv").await.unwrap();

        assert!(!flags.osc_visibility_change_compatible);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unavailable_async_mpv_version_property_keeps_the_fallback_connection_healthy() {
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("mpv-version.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            for error in ["success", "property unavailable"] {
                let request = lines.next_line().await.unwrap().unwrap();
                let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                let response = serde_json::json!({
                    "request_id": request["request_id"],
                    "error": error,
                });
                write_half
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            }
            let _ = release_rx.await;
        });

        let mut ipc = TestMpvIpc::new(socket_path.to_string_lossy());
        let _events = ipc.connect().await.unwrap();
        assert!(query_mpv_version_flags(&ipc).await.unwrap().is_none());
        assert!(ipc.is_healthy());

        release_tx.send(()).unwrap();
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_player_cancels_a_hanging_mpv_version_fallback_within_one_second() {
        let state = AppState::new();
        let directory = tempfile::tempdir().unwrap();
        let player_path = directory.path().join("mpv");
        let marker_path = directory.path().join("started");
        fs::write(
            &player_path,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 30\n",
                marker_path.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&player_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&player_path, permissions).unwrap();

        let startup_epoch = state.player_startup_epoch.load(Ordering::Acquire);
        let startup_state = state.clone();
        let startup_path = player_path.to_string_lossy().into_owned();
        let startup = tokio::spawn(async move {
            let _lifecycle_guard = startup_state.player_lifecycle.lock().await;
            await_player_startup_operation(
                &startup_state,
                startup_epoch,
                None,
                None,
                check_mpv_version(&startup_path),
            )
            .await
        });
        timeout(Duration::from_secs(1), async {
            while !marker_path.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("hanging version command did not start");

        timeout(Duration::from_secs(1), stop_player(&state))
            .await
            .expect("stop_player did not cancel the hanging version command")
            .unwrap();
        assert_eq!(
            startup.await.unwrap().unwrap_err(),
            "Player startup was cancelled"
        );
        assert!(state.player_lifecycle.try_lock().is_ok());
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
    async fn real_player_launch_is_disabled_in_unit_tests() {
        let state = AppState::new();

        let error = ensure_player_connected(&state).await.unwrap_err();

        assert_eq!(
            error,
            "Real player launch is disabled in tests; install FakePlayerFactory"
        );
        assert!(state.player.lock().is_none());
        assert!(state.player_process.lock().is_none());
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

    #[tokio::test]
    async fn explicit_player_stop_keeps_server_session_connected() {
        let state = AppState::new();
        let (server, connection) = attach_connected_server(&state).await;
        let fake = Arc::new(FakePlayerBackend::new(PlayerKind::Vlc));
        *state.player.lock() = Some(fake.clone());
        state.reconnect_state.lock().enabled = true;

        stop_player(&state).await.unwrap();

        assert!(connection.is_connected());
        assert!(state
            .connection
            .lock()
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &connection)));
        assert!(state.reconnect_state.lock().enabled);
        assert!(!*state.manual_disconnect.lock());
        assert_eq!(fake.shutdown_count(), 1);

        connection.disconnect();
        server.close();
    }

    #[tokio::test]
    async fn fatal_player_callback_disconnects_server_without_reconnect() {
        let state = AppState::new();
        let (server, connection) = attach_connected_server(&state).await;
        let fake = Arc::new(FakePlayerBackend::new(PlayerKind::Mpv));
        let player: Arc<dyn PlayerBackend> = fake.clone();
        let instance_id = player.instance_id();
        *state.player.lock() = Some(player);
        {
            let mut reconnect = state.reconnect_state.lock();
            reconnect.enabled = true;
            reconnect.running = true;
            reconnect.attempts = 3;
        }
        state.client_state.set_file(Some("movie.mkv".to_string()));
        state.client_state.set_ready(true);

        stop_player_instance(&state, instance_id).await.unwrap();

        assert!(state.player.lock().is_none());
        assert!(state.connection.lock().is_none());
        assert!(!connection.is_connected());
        assert_eq!(fake.shutdown_count(), 1);
        assert_eq!(state.client_state.get_file(), None);
        assert_eq!(state.client_state.ready_state(), None);
        let reconnect = state.reconnect_state.lock();
        assert!(!reconnect.enabled);
        assert!(!reconnect.running);
        assert_eq!(reconnect.attempts, 0);

        server.close();
    }

    #[tokio::test]
    async fn current_exit_wins_over_concurrent_startup_and_duplicate_callbacks_are_stale() {
        let state = AppState::new();
        let (server, connection) = attach_connected_server(&state).await;
        let old = Arc::new(MpvBackend::new(
            PlayerKind::Mpv,
            TestMpvIpc::new("unused-old-player"),
            Arc::downgrade(&state),
            None,
            false,
            None,
        ));
        old.ipc().mark_unhealthy("test player exited");
        let old_player: Arc<dyn PlayerBackend> = old.clone();
        let old_instance_id = old_player.instance_id();
        *state.player.lock() = Some(old_player);
        let factory = Arc::new(crate::player::backend::FakePlayerFactory::default());
        *state.fake_player_factory.lock() = Some(factory.clone());
        let startup_epoch = state.player_startup_epoch.load(Ordering::Acquire);
        let lifecycle_blocker = state.player_lifecycle.lock().await;

        let first_exit_state = state.clone();
        let first_exit =
            tokio::spawn(
                async move { stop_player_instance(&first_exit_state, old_instance_id).await },
            );
        let duplicate_exit_state = state.clone();
        let duplicate_exit = tokio::spawn(async move {
            stop_player_instance(&duplicate_exit_state, old_instance_id).await
        });
        let startup_state = state.clone();
        let startup = tokio::spawn(async move {
            ensure_player_connected_for_media_at_epoch(
                &startup_state,
                None,
                None,
                None,
                startup_epoch,
            )
            .await
        });
        drop(lifecycle_blocker);

        timeout(Duration::from_secs(1), first_exit)
            .await
            .expect("current exit callback did not finish")
            .unwrap()
            .unwrap();
        timeout(Duration::from_secs(1), duplicate_exit)
            .await
            .expect("duplicate exit callback did not finish")
            .unwrap()
            .unwrap();
        assert!(timeout(Duration::from_secs(1), startup)
            .await
            .expect("concurrent startup did not settle")
            .unwrap()
            .is_err());

        assert_eq!(factory.launch_count(), 0);
        assert!(state.player.lock().is_none());
        assert!(state.connection.lock().is_none());
        assert!(!connection.is_connected());

        let (replacement_server, replacement_connection) = attach_connected_server(&state).await;
        let replacement: Arc<dyn PlayerBackend> = Arc::new(MpvBackend::new(
            PlayerKind::Mpv,
            TestMpvIpc::new("unused-replacement-player"),
            Arc::downgrade(&state),
            None,
            false,
            None,
        ));
        *state.player.lock() = Some(replacement.clone());
        stop_player_instance(&state, old_instance_id).await.unwrap();

        assert!(state
            .player
            .lock()
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &replacement)));
        assert!(state
            .connection
            .lock()
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &replacement_connection)));
        assert!(replacement_connection.is_connected());

        replacement_connection.disconnect();
        server.close();
        replacement_server.close();
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
        let (server, connection) = attach_connected_server(&state).await;
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
        assert_eq!(fake.shutdown_count(), 1);
        assert_eq!(fake.commands(), vec![FakePlayerCommand::Shutdown]);
        assert!(state.connection.lock().is_none());
        assert!(!connection.is_connected());

        server.close();
    }

    #[tokio::test]
    async fn stale_disconnect_callback_cannot_clear_new_player() {
        let state = AppState::new();
        let (server, connection) = attach_connected_server(&state).await;
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
        assert!(connection.is_connected());
        assert!(state
            .connection
            .lock()
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &connection)));

        connection.disconnect();
        server.close();
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
    async fn double_check_rewind_has_no_late_three_second_seek() {
        let state = AppState::new();
        let fake = Arc::new(FakePlayerBackend::with_state(
            PlayerKind::Vlc,
            PlayerState {
                position: Some(0.0),
                ..PlayerState::default()
            },
        ));
        let player: Arc<dyn PlayerBackend> = fake.clone();
        *state.player.lock() = Some(player.clone());
        state
            .playback
            .install_load(LoadId(1), "a.mkv", "/media/a.mkv", player.clone());

        schedule_double_check_rewind(state, player, RewindGeneration::Load(LoadId(1)));
        sleep(Duration::from_millis(1_700)).await;
        fake.set_fake_state(PlayerState {
            position: Some(12.0),
            ..PlayerState::default()
        });
        sleep(Duration::from_millis(1_500)).await;

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

    #[tokio::test]
    async fn stop_player_cancels_a_startup_operation_before_waiting_for_lifecycle() {
        let state = AppState::new();
        let startup_epoch = state
            .player_startup_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let startup_state = state.clone();
        let startup = tokio::spawn(async move {
            let _lifecycle_guard = startup_state.player_lifecycle.lock().await;
            entered_tx.send(()).unwrap();
            await_player_startup_operation(
                &startup_state,
                startup_epoch,
                None,
                None,
                std::future::pending::<()>(),
            )
            .await
        });
        entered_rx.await.unwrap();

        timeout(Duration::from_secs(1), stop_player(&state))
            .await
            .expect("stop_player waited for the uncancelled startup operation")
            .unwrap();

        let error = startup.await.unwrap().unwrap_err();
        assert_eq!(error, "Player startup was cancelled");
        assert!(state.player_lifecycle.try_lock().is_ok());
    }
}
