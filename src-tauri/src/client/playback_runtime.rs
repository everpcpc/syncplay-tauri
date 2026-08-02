use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::app_state::AppState;
use crate::client::playback::{
    CommittedMedia, LoadId, PlaybackEffect, PlaybackEvent, PlaybackState,
};
use crate::commands::playlist::{send_playlist_index, shared_playlists_enabled};
use crate::player::backend::PlayerBackend;
use crate::player::controller::{load_media_by_name, send_committed_file_update, LoadMediaError};

pub struct PlaybackCoordinator {
    pub(crate) state: Mutex<PlaybackState>,
    pub(crate) dispatch: AsyncMutex<()>,
    pub(crate) media_transition: AsyncMutex<()>,
    active_load: Mutex<Option<LoadLease>>,
    latest_generation: Mutex<Option<LatestGeneration>>,
    media_action_epoch: AtomicU64,
}

impl Default for PlaybackCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(PlaybackState::default()),
            dispatch: AsyncMutex::new(()),
            media_transition: AsyncMutex::new(()),
            active_load: Mutex::new(None),
            latest_generation: Mutex::new(None),
            media_action_epoch: AtomicU64::new(1),
        }
    }
}

impl PlaybackCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> PlaybackState {
        self.state.lock().clone()
    }

    pub(crate) fn install_load(
        &self,
        load_id: LoadId,
        target: &str,
        expected_media: &str,
        player: Arc<dyn PlayerBackend>,
    ) -> (LoadLease, Option<LoadLease>) {
        let lease = LoadLease {
            id: load_id,
            target: target.to_string(),
            expected_media: expected_media.to_string(),
            player,
            cancelled: CancellationToken::new(),
        };
        let previous = self.active_load.lock().replace(lease.clone());
        if let Some(previous) = &previous {
            previous.cancelled.cancel();
        }
        *self.latest_generation.lock() = Some(LatestGeneration {
            id: load_id,
            player: lease.player.clone(),
        });
        (lease, previous)
    }

    pub(crate) fn active_load(&self, load_id: LoadId) -> Option<LoadLease> {
        self.active_load
            .lock()
            .as_ref()
            .filter(|load| load.id == load_id)
            .cloned()
    }

    pub(crate) fn current_load(&self) -> Option<LoadLease> {
        self.active_load.lock().clone()
    }

    pub(crate) fn matching_load(
        &self,
        player: &Arc<dyn PlayerBackend>,
        target: &str,
    ) -> Option<LoadLease> {
        self.active_load
            .lock()
            .as_ref()
            .filter(|load| {
                Arc::ptr_eq(&load.player, player)
                    && media_identity_matches(&load.expected_media, target)
            })
            .cloned()
    }

    pub(crate) fn finish_load(&self, load_id: LoadId) -> Option<LoadLease> {
        let mut active = self.active_load.lock();
        if active.as_ref().map(|load| load.id) != Some(load_id) {
            return None;
        }
        let load = active.take().expect("matching active load disappeared");
        load.cancelled.cancel();
        Some(load)
    }

    pub(crate) fn cancel_active_load(&self) -> Option<LoadLease> {
        let load = self.active_load.lock().take()?;
        load.cancelled.cancel();
        self.invalidate_generation(&load);
        Some(load)
    }

    pub(crate) fn abort_load(&self, load_id: LoadId) -> Option<LoadLease> {
        let mut active = self.active_load.lock();
        if active.as_ref().map(|load| load.id) != Some(load_id) {
            return None;
        }
        let load = active.take().expect("matching active load disappeared");
        drop(active);
        load.cancelled.cancel();
        self.invalidate_generation(&load);
        Some(load)
    }

    pub(crate) fn is_current_load(&self, lease: &LoadLease) -> bool {
        self.active_load.lock().as_ref().is_some_and(|current| {
            current.id == lease.id && Arc::ptr_eq(&current.player, &lease.player)
        }) && !lease.cancelled.is_cancelled()
    }

    pub(crate) fn is_latest_generation(
        &self,
        load_id: LoadId,
        player: &Arc<dyn PlayerBackend>,
    ) -> bool {
        self.latest_generation
            .lock()
            .as_ref()
            .is_some_and(|latest| latest.id == load_id && Arc::ptr_eq(&latest.player, player))
    }

    pub(crate) fn clear_latest_generation(&self) {
        *self.latest_generation.lock() = None;
    }

    fn invalidate_media_actions(&self) {
        self.media_action_epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn loop_lease(&self, expected_media: &str, player: Arc<dyn PlayerBackend>) -> LoopLease {
        LoopLease {
            epoch: self.media_action_epoch.load(Ordering::SeqCst),
            expected_media: expected_media.to_string(),
            player,
        }
    }

    fn invalidate_generation(&self, load: &LoadLease) {
        let mut latest = self.latest_generation.lock();
        if latest.as_ref().is_some_and(|generation| {
            generation.id == load.id && Arc::ptr_eq(&generation.player, &load.player)
        }) {
            *latest = None;
        }
    }
}

#[derive(Clone)]
pub(crate) struct LoadLease {
    pub id: LoadId,
    pub target: String,
    pub expected_media: String,
    pub player: Arc<dyn PlayerBackend>,
    cancelled: CancellationToken,
}

impl LoadLease {
    pub(crate) fn cancelled(&self) -> impl Future<Output = ()> + '_ {
        self.cancelled.cancelled()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }
}

impl fmt::Debug for LoadLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadLease")
            .field("id", &self.id)
            .field("target", &self.target)
            .field("expected_media", &self.expected_media)
            .field("cancelled", &self.cancelled.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) struct LoopLease {
    epoch: u64,
    pub(crate) expected_media: String,
    pub(crate) player: Arc<dyn PlayerBackend>,
}

impl fmt::Debug for LoopLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoopLease")
            .field("epoch", &self.epoch)
            .field("expected_media", &self.expected_media)
            .finish_non_exhaustive()
    }
}

struct LatestGeneration {
    id: LoadId,
    player: Arc<dyn PlayerBackend>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchResult {
    pub media_accepted: bool,
    pub media_settled: bool,
    pub media_reset: bool,
    pub completed_load: Option<LoadId>,
}

pub(crate) struct DispatchOutcome {
    pub result: DispatchResult,
    pub effect_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistEditResult {
    pub items: Vec<String>,
    pub previous_index: Option<usize>,
    pub index: Option<usize>,
}

pub async fn dispatch(
    state: &Arc<AppState>,
    event: PlaybackEvent,
) -> Result<DispatchResult, String> {
    dispatch_all(state, [event]).await
}

pub async fn dispatch_all(
    state: &Arc<AppState>,
    events: impl IntoIterator<Item = PlaybackEvent>,
) -> Result<DispatchResult, String> {
    let outcome = dispatch_all_outcome(state, events).await;
    if let Some(error) = outcome.effect_error {
        return Err(error);
    }
    Ok(outcome.result)
}

pub(crate) async fn dispatch_all_outcome(
    state: &Arc<AppState>,
    events: impl IntoIterator<Item = PlaybackEvent>,
) -> DispatchOutcome {
    let _dispatch_guard = state.playback.dispatch.lock().await;
    dispatch_locked(state, events)
}

fn dispatch_locked(
    state: &Arc<AppState>,
    events: impl IntoIterator<Item = PlaybackEvent>,
) -> DispatchOutcome {
    let shared_playlists = {
        let config = state.config.lock().clone();
        shared_playlists_enabled(state, &config)
    };
    let mut effects = Vec::new();
    let mut result = DispatchResult::default();
    let mut cancel_active = false;
    let mut playlist_replaced = false;
    let mut clear_latest_generation = false;
    let mut invalidate_media_actions = false;
    let (snapshot, superseded) = {
        let mut playback = state.playback.state.lock();
        for event in events {
            let external_media = matches!(
                &event,
                PlaybackEvent::PlayerMediaCommitted { load_id: None, .. }
            );
            cancel_active |= matches!(
                &event,
                PlaybackEvent::PlayerDisconnected | PlaybackEvent::Reset
            );
            invalidate_media_actions |= matches!(&event, PlaybackEvent::Reconnect);
            playlist_replaced |= matches!(&event, PlaybackEvent::ServerPlaylist { .. });
            #[cfg(test)]
            {
                playlist_replaced |= matches!(&event, PlaybackEvent::ReplacePlaylist { .. });
            }
            if let PlaybackEvent::ServerPlaylist { items } = &event {
                let room = state.client_state.get_room();
                state.playlist.update_previous_playlist(items, &room);
            }
            let committed_media = match &event {
                PlaybackEvent::PlayerMediaCommitted { load_id, media } => {
                    let recognized_load = load_id.filter(|load_id| {
                        playback
                            .pending_load
                            .as_ref()
                            .filter(|pending| pending.id == *load_id)
                            .or_else(|| {
                                playback
                                    .interrupted_load
                                    .as_ref()
                                    .filter(|pending| pending.id == *load_id)
                            })
                            .is_some()
                    });
                    let reset_position = recognized_load.and_then(|load_id| {
                        playback
                            .pending_load
                            .as_ref()
                            .filter(|pending| pending.id == load_id)
                            .or_else(|| {
                                playback
                                    .interrupted_load
                                    .as_ref()
                                    .filter(|pending| pending.id == load_id)
                            })
                            .map(|pending| pending.reset_position)
                    });
                    Some((media.clone(), recognized_load, reset_position))
                }
                _ => None,
            };
            let event_effects = playback.reduce(event);
            if let Some((media, recognized_load, reset_position)) = committed_media {
                let accepted = event_effects
                    .iter()
                    .any(|effect| matches!(effect, PlaybackEffect::SendFile { .. }));
                result.media_accepted |= accepted;
                clear_latest_generation |= accepted && external_media;
                invalidate_media_actions |= accepted && external_media;
                result.media_reset |= accepted && reset_position.unwrap_or(false);
                result.media_settled |= recognized_load.is_some()
                    || accepted
                    || (playback.pending_load.is_none()
                        && playback.confirmed_media.as_ref() == Some(&media));
                if recognized_load.is_some() {
                    result.completed_load = recognized_load;
                }
            }
            effects.extend(event_effects);
        }

        let next_load_id = effects.iter().rev().find_map(|effect| match effect {
            PlaybackEffect::Load { load_id, .. } => Some(*load_id),
            _ => None,
        });
        invalidate_media_actions |= next_load_id.is_some() || playlist_replaced || cancel_active;
        let superseded = if cancel_active {
            state.playback.cancel_active_load()
        } else {
            playlist_replaced
                .then(|| state.playback.current_load())
                .flatten()
                .filter(|load| !playback.playlist_items.contains(&load.target))
                .and_then(|_| state.playback.cancel_active_load())
        };
        if let Some(load) = &superseded {
            playback.reduce(PlaybackEvent::LoadFailed { load_id: load.id });
        }
        if !shared_playlists {
            playback.proposed_index = None;
        }
        if invalidate_media_actions {
            state.playback.invalidate_media_actions();
        }
        (playback.clone(), superseded)
    };

    if let Some(load) = &superseded {
        load.player.cancel_file_load(load.id.0);
    }
    if cancel_active || clear_latest_generation {
        state.playback.clear_latest_generation();
    }
    project_playlist(state, &snapshot);

    let mut load_effects = Vec::new();
    let mut effect_error = None;
    for effect in effects {
        match effect {
            effect @ PlaybackEffect::Load { .. } => load_effects.push(effect),
            PlaybackEffect::SendFile { media } => {
                if let Err(error) = send_committed_file_update(state, &media) {
                    effect_error.get_or_insert(error);
                }
            }
            PlaybackEffect::SendPlaylistIndex {
                index,
                reset_position,
            } if shared_playlists => {
                if let Err(error) = send_playlist_index(state, index, reset_position) {
                    effect_error.get_or_insert(error);
                }
            }
            PlaybackEffect::SendPlaylistIndex { .. } => {}
        }
    }
    for effect in load_effects {
        spawn_load_effect(state, effect);
    }

    DispatchOutcome {
        result,
        effect_error,
    }
}

pub async fn local_select(
    state: &Arc<AppState>,
    index: usize,
    reset_position: bool,
) -> Result<(), String> {
    let _dispatch_guard = state.playback.dispatch.lock().await;
    if index >= state.playback.state.lock().playlist_items.len() {
        return Err("Invalid playlist index".to_string());
    }
    let outcome = dispatch_locked(
        state,
        [PlaybackEvent::LocalSelect {
            index,
            reset_position,
        }],
    );
    outcome.effect_error.map_or(Ok(()), Err)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistStep {
    Next { loop_at_end: bool },
    Previous,
}

#[derive(Debug, Clone)]
pub(crate) enum EofAction {
    None,
    Rewind(LoopLease),
    Load,
}

pub async fn local_step(
    state: &Arc<AppState>,
    step: PlaylistStep,
    reset_position: bool,
) -> Result<(), String> {
    let _dispatch_guard = state.playback.dispatch.lock().await;
    let index = {
        let playback = state.playback.state.lock();
        if playback.playlist_items.is_empty() {
            return Err("Playlist is empty".to_string());
        }
        match step {
            PlaylistStep::Next { loop_at_end } => match playback.displayed_index() {
                None => 0,
                Some(current) if current + 1 < playback.playlist_items.len() => current + 1,
                Some(_) if loop_at_end => 0,
                Some(_) => return Err("Already at end of playlist".to_string()),
            },
            PlaylistStep::Previous => match playback.displayed_index() {
                Some(current) if current > 0 => current - 1,
                _ => return Err("Already at start of playlist".to_string()),
            },
        }
    };
    let outcome = dispatch_locked(
        state,
        [PlaybackEvent::LocalSelect {
            index,
            reset_position,
        }],
    );
    outcome.effect_error.map_or(Ok(()), Err)
}

pub async fn advance_after_eof(
    state: &Arc<AppState>,
    expected_media: &str,
    loop_single: bool,
    loop_at_end: bool,
    change_threshold_seconds: f64,
) -> Result<EofAction, String> {
    let _dispatch_guard = state.playback.dispatch.lock().await;
    if !state.playlist.not_just_changed(change_threshold_seconds) {
        return Ok(EofAction::None);
    }
    enum Decision {
        None,
        Rewind,
        Load(usize),
    }

    let decision = {
        let playback = state.playback.state.lock();
        let Some(current) = playback.displayed_index() else {
            return Ok(EofAction::None);
        };
        let Some(current_target) = playback.playlist_items.get(current) else {
            return Ok(EofAction::None);
        };
        if current_target != expected_media
            || playback
                .confirmed_media
                .as_ref()
                .is_none_or(|media| media.name != expected_media)
        {
            return Ok(EofAction::None);
        }
        if playback.playlist_items.len() == 1 && loop_single {
            state.playlist.opened_file();
            Decision::Rewind
        } else if current + 1 < playback.playlist_items.len() {
            Decision::Load(current + 1)
        } else if loop_at_end {
            Decision::Load(0)
        } else {
            Decision::None
        }
    };

    let index = match decision {
        Decision::None => return Ok(EofAction::None),
        Decision::Rewind => {
            let Some(player) = state.player.lock().clone() else {
                return Ok(EofAction::None);
            };
            return Ok(EofAction::Rewind(
                state.playback.loop_lease(expected_media, player),
            ));
        }
        Decision::Load(index) => index,
    };
    let outcome = dispatch_locked(
        state,
        [PlaybackEvent::LocalSelect {
            index,
            reset_position: true,
        }],
    );
    if let Some(error) = outcome.effect_error {
        return Err(error);
    }
    Ok(EofAction::Load)
}

pub(crate) fn is_current_loop_lease(state: &Arc<AppState>, lease: &LoopLease) -> bool {
    if state.playback.media_action_epoch.load(Ordering::SeqCst) != lease.epoch {
        return false;
    }
    if !state
        .player
        .lock()
        .as_ref()
        .is_some_and(|player| Arc::ptr_eq(player, &lease.player))
    {
        return false;
    }
    let playback = state.playback.state.lock();
    if playback.pending_load.is_some()
        || playback.interrupted_load.is_some()
        || playback.media_uncertain
        || playback.confirmed_media.as_ref().map(|media| &media.name) != Some(&lease.expected_media)
    {
        return false;
    }
    playback
        .displayed_index()
        .and_then(|index| playback.playlist_items.get(index))
        == Some(&lease.expected_media)
}

pub async fn server_playlist_and_index(
    state: &Arc<AppState>,
    items: Option<Vec<String>>,
    index: Option<(Option<usize>, bool)>,
) -> Result<(), String> {
    let load_shared_playlist = {
        let config = state.config.lock().clone();
        shared_playlists_enabled(state, &config)
    };
    let mut events = Vec::with_capacity(2);
    if let Some(items) = items {
        events.push(PlaybackEvent::ServerPlaylist { items });
    }
    if let Some((index, reset_position)) = index {
        events.push(if load_shared_playlist {
            PlaybackEvent::ServerIndex {
                index,
                reset_position,
            }
        } else {
            PlaybackEvent::ServerIndexObserved { index }
        });
    }
    dispatch_all(state, events).await.map(|_| ())
}

pub async fn reconcile_shared_playlist(state: &Arc<AppState>) -> Result<(), String> {
    dispatch(state, PlaybackEvent::ReconcileSharedPlaylist)
        .await
        .map(|_| ())
}

#[cfg(test)]
pub async fn replace_playlist(
    state: &Arc<AppState>,
    items: Vec<String>,
    index: Option<usize>,
) -> Result<(), String> {
    dispatch(state, PlaybackEvent::ReplacePlaylist { items, index })
        .await
        .map(|_| ())
}

#[cfg(test)]
pub async fn edit_playlist(
    state: &Arc<AppState>,
    reset_index: bool,
    edit: impl FnOnce(&mut Vec<String>, Option<usize>) -> Result<(), String>,
) -> Result<PlaylistEditResult, String> {
    edit_playlist_and_publish(state, reset_index, edit, |_| Ok(())).await
}

pub async fn edit_playlist_and_publish(
    state: &Arc<AppState>,
    reset_index: bool,
    edit: impl FnOnce(&mut Vec<String>, Option<usize>) -> Result<(), String>,
    publish: impl FnOnce(&PlaylistEditResult) -> Result<(), String>,
) -> Result<PlaylistEditResult, String> {
    let _dispatch_guard = state.playback.dispatch.lock().await;
    let (mut items, previous_index) = {
        let playback = state.playback.state.lock();
        (playback.playlist_items.clone(), playback.displayed_index())
    };
    let previous_items = items.clone();
    edit(&mut items, previous_index)?;

    let room = state.client_state.get_room();
    state.playlist.update_previous_playlist(&items, &room);
    let index = if items.is_empty() {
        None
    } else if reset_index {
        Some(0)
    } else {
        Some(compute_valid_index(&previous_items, &items, previous_index))
    };
    let (snapshot, orphaned) = {
        let mut playback = state.playback.state.lock();
        playback.reduce(PlaybackEvent::LocalPlaylistEdit {
            items: items.clone(),
            index,
        });
        if previous_items != items {
            state.playback.invalidate_media_actions();
        }
        let orphaned = state
            .playback
            .current_load()
            .filter(|load| !playback.playlist_items.contains(&load.target))
            .and_then(|_| state.playback.cancel_active_load());
        if let Some(load) = &orphaned {
            playback.reduce(PlaybackEvent::LoadFailed { load_id: load.id });
        }
        (playback.clone(), orphaned)
    };
    if let Some(load) = orphaned {
        load.player.cancel_file_load(load.id.0);
    }
    project_playlist(state, &snapshot);

    let result = PlaylistEditResult {
        items,
        previous_index,
        index,
    };
    publish(&result)?;
    Ok(result)
}

fn compute_valid_index(
    previous_items: &[String],
    items: &[String],
    previous_index: Option<usize>,
) -> usize {
    let Some(previous_index) = previous_index else {
        return 0;
    };
    if items.len() <= 1 {
        return 0;
    }
    for filename in previous_items.iter().skip(previous_index) {
        if let Some(index) = items.iter().position(|item| item == filename) {
            return index;
        }
    }
    for filename in previous_items.iter().take(previous_index).rev() {
        if let Some(index) = items.iter().position(|item| item == filename) {
            return (index + 1).min(items.len() - 1);
        }
    }
    0
}

pub fn confirmed_media(state: &Arc<AppState>) -> Option<CommittedMedia> {
    state.playback.state.lock().confirmed_media.clone()
}

pub(crate) async fn media_index_refresh_finished(state: &Arc<AppState>) {
    let _dispatch_guard = state.playback.dispatch.lock().await;
    state.media_index.finish_refresh();
    let outcome = dispatch_locked(state, [PlaybackEvent::RetryPending]);
    if let Some(error) = outcome.effect_error {
        tracing::warn!("Failed to retry pending media after index refresh: {error}");
    }
}

pub async fn reconnect(state: &Arc<AppState>) {
    let _ = dispatch(state, PlaybackEvent::Reconnect).await;
}

pub async fn player_disconnected(state: &Arc<AppState>) {
    let _ = dispatch(state, PlaybackEvent::PlayerDisconnected).await;
}

pub async fn reset(state: &Arc<AppState>) {
    let _ = dispatch(state, PlaybackEvent::Reset).await;
}

async fn execute_load(
    state: &Arc<AppState>,
    load_id: crate::client::playback::LoadId,
    target: &str,
    reset_position: bool,
) -> Result<(), String> {
    match load_media_by_name(state, target, reset_position, load_id).await {
        Ok(started) => {
            let duration = if started.is_stream {
                Duration::from_secs(120)
            } else {
                Duration::from_secs(15)
            };
            tokio::select! {
                biased;
                () = started.lease.cancelled() => {}
                () = tokio::time::sleep(duration) => {
                    if state.playback.is_current_load(&started.lease) {
                        tracing::warn!(load_id = load_id.0, "Player load timed out");
                        fail_load(state, load_id).await;
                    }
                }
            }
            Ok(())
        }
        Err(LoadMediaError::Superseded) => Ok(()),
        Err(LoadMediaError::MediaNotFound(message)) => {
            settle_missing_media_load(state, load_id, target).await;
            Err(message)
        }
        Err(error @ LoadMediaError::Failed(_)) => {
            apply_internal_event(state, PlaybackEvent::LoadFailed { load_id }).await;
            Err(error.to_string())
        }
    }
}

async fn settle_missing_media_load(state: &Arc<AppState>, load_id: LoadId, target: &str) {
    let _dispatch_guard = state.playback.dispatch.lock().await;
    let events = if state.media_index.is_refreshing() {
        vec![PlaybackEvent::LoadDeferred { load_id }]
    } else if media_is_available(state, target) {
        vec![
            PlaybackEvent::LoadDeferred { load_id },
            PlaybackEvent::RetryPending,
        ]
    } else {
        vec![PlaybackEvent::LoadFailed { load_id }]
    };
    let outcome = dispatch_locked(state, events);
    if let Some(error) = outcome.effect_error {
        tracing::warn!("Failed to settle unavailable media load: {error}");
    }
}

fn media_is_available(state: &Arc<AppState>, target: &str) -> bool {
    if crate::utils::is_url(target) {
        return true;
    }
    let config = state.config.lock().clone();
    state.media_index.resolve_path(target).is_some()
        || crate::player::controller::resolve_media_path(&config.player.media_directories, target)
            .is_some()
}

pub(crate) fn claim_load_for_issue(
    state: &Arc<AppState>,
    load_id: LoadId,
    target: &str,
    expected_media: &str,
    player: Arc<dyn PlayerBackend>,
) -> Option<LoadLease> {
    let mut playback = state.playback.state.lock();
    let is_current = playback
        .pending_load
        .as_ref()
        .is_some_and(|pending| pending.id == load_id && pending.target == target);
    if is_current {
        playback.reduce(PlaybackEvent::LoadStarted { load_id });
        let (lease, superseded) =
            state
                .playback
                .install_load(load_id, target, expected_media, player);
        if let Some(superseded) = superseded {
            superseded.player.cancel_file_load(superseded.id.0);
            playback.reduce(PlaybackEvent::LoadFailed {
                load_id: superseded.id,
            });
        }
        return Some(lease);
    }
    None
}

fn media_identity_matches(expected: &str, observed: &str) -> bool {
    if crate::utils::is_url(expected) {
        expected == observed
    } else {
        expected == observed || media_identity_name(expected) == observed
    }
}

fn media_identity_name(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}

async fn apply_internal_event(state: &Arc<AppState>, event: PlaybackEvent) {
    let _dispatch_guard = state.playback.dispatch.lock().await;
    let snapshot = {
        let mut playback = state.playback.state.lock();
        playback.reduce(event);
        playback.clone()
    };
    project_playlist(state, &snapshot);
}

pub(crate) fn is_current_load(state: &Arc<AppState>, load_id: LoadId, target: &str) -> bool {
    state
        .playback
        .state
        .lock()
        .pending_load
        .as_ref()
        .is_some_and(|pending| pending.id == load_id && pending.target == target)
}

pub(crate) async fn fail_load(state: &Arc<AppState>, load_id: LoadId) {
    let _transition_guard = state.playback.media_transition.lock().await;
    let _dispatch_guard = state.playback.dispatch.lock().await;
    let should_fail = {
        let playback = state.playback.state.lock();
        playback
            .pending_load
            .as_ref()
            .is_some_and(|pending| pending.id == load_id)
            || playback
                .interrupted_load
                .as_ref()
                .is_some_and(|pending| pending.id == load_id)
    };
    if should_fail {
        if let Some(load) = state.playback.abort_load(load_id) {
            load.player.cancel_file_load(load_id.0);
        }
        let snapshot = {
            let mut playback = state.playback.state.lock();
            playback.reduce(PlaybackEvent::LoadFailed { load_id });
            playback.clone()
        };
        project_playlist(state, &snapshot);
    }
}

fn spawn_load_effect(state: &Arc<AppState>, effect: PlaybackEffect) {
    let state = state.clone();
    tokio::spawn(async move {
        let PlaybackEffect::Load {
            load_id,
            target,
            reset_position,
        } = effect
        else {
            return;
        };
        if !is_current_load(&state, load_id, &target) {
            return;
        }
        if let Err(error) = execute_load(&state, load_id, &target, reset_position).await {
            tracing::warn!(
                load_id = load_id.0,
                "Failed to execute player load: {}",
                error
            );
        }
    });
}

fn project_playlist(state: &Arc<AppState>, playback: &PlaybackState) {
    let index = playback.displayed_index();
    let projected = state.playlist.snapshot();
    if projected != (playback.playlist_items.clone(), index) {
        state
            .playlist
            .set_items_with_index(playback.playlist_items.clone(), index);
        state.emit_event(
            "playlist-updated",
            crate::app_state::PlaylistEvent {
                items: playback.playlist_items.clone(),
                current_index: index,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::playback::{PendingLoadStatus, PlaybackEvent};
    use crate::network::connection::Connection;
    use crate::network::fake_server::FakeSyncplayServer;
    use crate::network::messages::ProtocolMessage;
    use crate::player::backend::{FakePlayerBackend, FakePlayerCommand, PlayerBackend, PlayerKind};
    use crate::player::properties::PlayerState;
    use std::time::Duration;
    use tokio::time::timeout;

    async fn fixture() -> (
        Arc<AppState>,
        FakePlayerBackend,
        FakeSyncplayServer,
        tempfile::TempDir,
    ) {
        let state = AppState::new();
        let directory = tempfile::tempdir().unwrap();
        for filename in ["a.mkv", "b.mkv", "c.mkv"] {
            std::fs::write(directory.path().join(filename), filename).unwrap();
        }
        state.config.lock().player.media_directories =
            vec![directory.path().to_string_lossy().into_owned()];

        let player = FakePlayerBackend::new(PlayerKind::Vlc);
        *state.player.lock() = Some(Arc::new(player.clone()) as Arc<dyn PlayerBackend>);

        let server = FakeSyncplayServer::start().await.unwrap();
        let connection = Arc::new(Connection::new());
        let (_receiver, _peer) = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();
        *state.connection.lock() = Some(connection);

        replace_playlist(
            &state,
            vec!["a.mkv".into(), "b.mkv".into(), "c.mkv".into()],
            Some(0),
        )
        .await
        .unwrap();
        (state, player, server, directory)
    }

    async fn wait_for_load_count(player: &FakePlayerBackend, expected: usize) {
        timeout(Duration::from_secs(1), async {
            loop {
                let count = player
                    .commands()
                    .iter()
                    .filter(|command| matches!(command, FakePlayerCommand::LoadFile(_)))
                    .count();
                if count >= expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    async fn wait_for_pending_status(state: &Arc<AppState>, expected: PendingLoadStatus) {
        timeout(Duration::from_secs(1), async {
            loop {
                if state
                    .playback
                    .snapshot()
                    .pending_load
                    .as_ref()
                    .is_some_and(|pending| pending.status == expected)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    async fn wait_for_no_pending_load(state: &Arc<AppState>) {
        timeout(Duration::from_secs(1), async {
            loop {
                if state.playback.snapshot().pending_load.is_none() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn local_selection_loads_once_and_announces_only_after_commit() {
        let (state, player, mut server, _directory) = fixture().await;

        local_select(&state, 1, true).await.unwrap();
        wait_for_load_count(&player, 1).await;
        let pending = state.playback.snapshot().pending_load.unwrap();
        assert_eq!(pending.target, "b.mkv");
        assert_eq!(
            player
                .commands()
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::LoadFile(_)))
                .count(),
            1
        );
        let commands = player.commands();
        let load_position = commands
            .iter()
            .position(|command| matches!(command, FakePlayerCommand::LoadFile(_)))
            .unwrap();
        let rewind_position = commands
            .iter()
            .position(|command| matches!(command, FakePlayerCommand::SetPosition(0.0)))
            .unwrap();
        assert!(load_position < rewind_position);
        assert!(state.client_state.get_file().is_none());
        assert!(timeout(Duration::from_millis(50), server.next_received())
            .await
            .is_err());

        let commit = dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(pending.id),
                media: CommittedMedia::new("b.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();
        assert!(commit.media_accepted);
        assert!(commit.media_reset);

        let mut saw_file = false;
        let mut saw_index = false;
        while !saw_index {
            let message = timeout(Duration::from_secs(1), server.next_received())
                .await
                .unwrap()
                .unwrap();
            if let ProtocolMessage::Set { Set } = message {
                if Set.file.is_some() {
                    assert!(!saw_index, "file must be announced before playlist index");
                    saw_file = true;
                }
                if Set.playlist_index.is_some() {
                    assert!(saw_file, "playlist index must wait for confirmed media");
                    saw_index = true;
                }
            }
        }
        assert_eq!(state.client_state.get_file().as_deref(), Some("b.mkv"));

        server_playlist_and_index(&state, None, Some((Some(1), true)))
            .await
            .unwrap();
        assert_eq!(
            player
                .commands()
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::LoadFile(_)))
                .count(),
            1,
            "server echo must not reload confirmed media"
        );
    }

    #[tokio::test]
    async fn external_media_does_not_send_playlist_index_when_the_feature_is_disabled() {
        let (state, _player, mut server, _directory) = fixture().await;
        state.server_features.lock().shared_playlists = false;

        let result = dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("b.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();

        assert!(result.media_accepted);
        let message = timeout(Duration::from_secs(1), server.next_received())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            message,
            ProtocolMessage::Set { Set } if Set.file.is_some() && Set.playlist_index.is_none()
        ));
        assert!(matches!(
            timeout(Duration::from_secs(1), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::List { .. }
        ));
        assert!(timeout(Duration::from_millis(50), server.next_received())
            .await
            .is_err());
        assert_eq!(state.playback.snapshot().proposed_index, None);
    }

    #[tokio::test]
    async fn missing_media_without_an_active_refresh_reaches_a_terminal_state() {
        let (state, player, _server, _directory) = fixture().await;
        replace_playlist(&state, vec!["missing.mkv".into()], Some(0))
            .await
            .unwrap();

        local_select(&state, 0, true).await.unwrap();
        wait_for_no_pending_load(&state).await;

        assert!(!player
            .commands()
            .iter()
            .any(|command| matches!(command, FakePlayerCommand::LoadFile(_))));
    }

    #[tokio::test]
    async fn missing_successor_does_not_cancel_the_issued_generation() {
        let (state, player, _server, _directory) = fixture().await;
        replace_playlist(
            &state,
            vec![
                "a.mkv".into(),
                "b.mkv".into(),
                "c.mkv".into(),
                "missing.mkv".into(),
            ],
            Some(0),
        )
        .await
        .unwrap();
        player.set_load_delay(Duration::from_millis(100));

        local_select(&state, 1, true).await.unwrap();
        wait_for_load_count(&player, 1).await;
        let issued = state.playback.snapshot().pending_load.unwrap().id;
        local_select(&state, 3, true).await.unwrap();
        wait_for_no_pending_load(&state).await;

        assert_eq!(
            state.playback.current_load().map(|load| load.id),
            Some(issued)
        );
        assert_eq!(
            state
                .playback
                .snapshot()
                .interrupted_load
                .map(|load| load.id),
            Some(issued)
        );
        let commit = dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(issued),
                media: CommittedMedia::new("b.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();

        assert!(commit.media_accepted);
        assert_eq!(state.client_state.get_file().as_deref(), Some("b.mkv"));
    }

    #[tokio::test]
    async fn index_refresh_retries_deferred_media_once_after_it_becomes_available() {
        let (state, player, _server, directory) = fixture().await;
        replace_playlist(&state, vec!["later.mkv".into()], Some(0))
            .await
            .unwrap();
        state.media_index.set_refreshing_for_test(true);

        local_select(&state, 0, true).await.unwrap();
        wait_for_pending_status(&state, PendingLoadStatus::WaitingForMedia).await;
        std::fs::write(directory.path().join("later.mkv"), "later").unwrap();

        media_index_refresh_finished(&state).await;
        wait_for_load_count(&player, 1).await;

        assert_eq!(
            player
                .commands()
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::LoadFile(_)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn refresh_completion_rechecks_media_before_failing_a_requested_load() {
        let (state, player, _server, directory) = fixture().await;
        replace_playlist(&state, vec!["later.mkv".into()], Some(0))
            .await
            .unwrap();
        let load_id = {
            let mut playback = state.playback.state.lock();
            let effects = playback.reduce(PlaybackEvent::LocalSelect {
                index: 0,
                reset_position: true,
            });
            let [PlaybackEffect::Load { load_id, .. }] = effects.as_slice() else {
                panic!("expected requested load");
            };
            *load_id
        };
        let path = directory.path().join("later.mkv");
        std::fs::write(&path, "later").unwrap();
        state.media_index.add_override_path("later.mkv", path);

        settle_missing_media_load(&state, load_id, "later.mkv").await;
        wait_for_load_count(&player, 1).await;

        assert_eq!(
            state.playback.snapshot().pending_load.unwrap().status,
            PendingLoadStatus::Loading
        );
    }

    #[tokio::test]
    async fn index_refresh_clears_a_deferred_load_when_media_stays_missing() {
        let (state, player, _server, _directory) = fixture().await;
        replace_playlist(&state, vec!["missing.mkv".into()], Some(0))
            .await
            .unwrap();
        state.media_index.set_refreshing_for_test(true);

        local_select(&state, 0, true).await.unwrap();
        wait_for_pending_status(&state, PendingLoadStatus::WaitingForMedia).await;

        media_index_refresh_finished(&state).await;
        wait_for_no_pending_load(&state).await;

        assert!(!player
            .commands()
            .iter()
            .any(|command| matches!(command, FakePlayerCommand::LoadFile(_))));
    }

    #[tokio::test]
    async fn relative_selection_uses_the_coordinator_snapshot() {
        let (state, player, _server, _directory) = fixture().await;
        state.playlist.set_items_with_index(
            vec!["c.mkv".into(), "b.mkv".into(), "a.mkv".into()],
            Some(2),
        );

        local_step(&state, PlaylistStep::Next { loop_at_end: false }, true)
            .await
            .unwrap();
        wait_for_load_count(&player, 1).await;

        assert_eq!(
            state.playback.snapshot().pending_load.unwrap().target,
            "b.mkv"
        );
    }

    #[tokio::test]
    async fn superseding_selection_cancels_old_post_commit_sync_before_the_next_load() {
        let (state, _fixture_player, _server, _directory) = fixture().await;
        let player = FakePlayerBackend::new(PlayerKind::MpcHc);
        let player_dyn = Arc::new(player.clone()) as Arc<dyn PlayerBackend>;
        *state.player.lock() = Some(player_dyn.clone());
        state
            .client_state
            .set_global_state(42.0, false, Some("peer".to_string()));

        local_select(&state, 1, false).await.unwrap();
        wait_for_load_count(&player, 1).await;
        let load_b = state.playback.snapshot().pending_load.unwrap().id;
        let player_state = PlayerState {
            filename: Some("b.mkv".to_string()),
            path: Some("b.mkv".to_string()),
            duration: Some(120.0),
            ..PlayerState::default()
        };

        let commit_state = state.clone();
        let commit_player = player_dyn.clone();
        let commit = tokio::spawn(async move {
            let _transition_guard = commit_state.playback.media_transition.lock().await;
            let _lifecycle_guard = commit_state.player_lifecycle.lock().await;
            crate::player::controller::commit_player_state(
                &commit_state,
                Some(&commit_player),
                &player_state,
                Some(load_b),
            )
            .await
        });

        timeout(Duration::from_secs(1), async {
            loop {
                if player
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

        local_select(&state, 2, true).await.unwrap();
        wait_for_load_count(&player, 2).await;
        commit.await.unwrap().unwrap();

        let commands = player.commands();
        let load_c = commands
            .iter()
            .position(|command| {
                matches!(command, FakePlayerCommand::LoadFile(path) if path.ends_with("c.mkv"))
            })
            .unwrap();
        assert!(!commands[load_c + 1..]
            .iter()
            .any(|command| matches!(command, FakePlayerCommand::SetPosition(42.0))));
    }

    #[tokio::test]
    async fn stale_completion_cannot_replace_latest_request() {
        let (state, _player, _server, _directory) = fixture().await;

        local_select(&state, 1, true).await.unwrap();
        let load_b = state.playback.snapshot().pending_load.unwrap().id;
        local_select(&state, 2, true).await.unwrap();
        let load_c = state.playback.snapshot().pending_load.unwrap().id;
        assert!(load_c > load_b);

        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(load_b),
                media: CommittedMedia::new("b.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();
        assert!(state.client_state.get_file().is_none());

        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(load_c),
                media: CommittedMedia::new("c.mkv", Some(5), Some(130.0)),
            },
        )
        .await
        .unwrap();
        assert_eq!(state.client_state.get_file().as_deref(), Some("c.mkv"));
    }

    #[tokio::test]
    async fn latest_selection_supersedes_a_slow_load_without_waiting_for_settlement() {
        let (state, player, _server, _directory) = fixture().await;
        player.set_load_delay(Duration::from_millis(80));

        let first_state = state.clone();
        let first = tokio::spawn(async move { local_select(&first_state, 1, true).await });
        timeout(Duration::from_secs(1), async {
            loop {
                if player
                    .commands()
                    .iter()
                    .any(|command| matches!(command, FakePlayerCommand::LoadFile(path) if path.ends_with("b.mkv")))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let middle_state = state.clone();
        let middle = tokio::spawn(async move { local_select(&middle_state, 2, true).await });
        timeout(Duration::from_secs(1), async {
            loop {
                if state
                    .playback
                    .snapshot()
                    .pending_load
                    .is_some_and(|pending| pending.target == "c.mkv")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let latest_state = state.clone();
        let latest = tokio::spawn(async move { local_select(&latest_state, 0, true).await });

        first.await.unwrap().unwrap();
        middle.await.unwrap().unwrap();
        latest.await.unwrap().unwrap();
        let latest_load = state.playback.snapshot().pending_load.unwrap().id;

        timeout(Duration::from_secs(1), async {
            loop {
                if player.commands().iter().any(
                    |command| matches!(command, FakePlayerCommand::LoadFile(path) if path.ends_with("a.mkv")),
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let loaded: Vec<String> = player
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                FakePlayerCommand::LoadFile(path) => Some(path),
                _ => None,
            })
            .collect();
        assert!(loaded[0].ends_with("b.mkv"));
        assert!(loaded.last().unwrap().ends_with("a.mkv"));

        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(latest_load),
                media: CommittedMedia::new("a.mkv", Some(5), Some(110.0)),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn reselecting_current_media_queues_a_compensating_latest_load() {
        let (state, player, _server, _directory) = fixture().await;
        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("a.mkv", Some(5), Some(110.0)),
            },
        )
        .await
        .unwrap();

        local_select(&state, 1, true).await.unwrap();
        wait_for_load_count(&player, 1).await;
        let load_b = state.playback.snapshot().pending_load.unwrap().id;
        local_select(&state, 0, true).await.unwrap();
        let load_a = state.playback.snapshot().pending_load.unwrap().id;

        assert!(load_a > load_b);
        wait_for_load_count(&player, 2).await;

        let stale = dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(load_b),
                media: CommittedMedia::new("b.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();
        assert!(!stale.media_accepted);
        assert!(!stale.media_settled);

        let loads: Vec<String> = player
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                FakePlayerCommand::LoadFile(path) => Some(path),
                _ => None,
            })
            .collect();
        assert_eq!(loads.len(), 2);
        assert!(loads[0].ends_with("b.mkv"));
        assert!(loads[1].ends_with("a.mkv"));

        let latest = dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(load_a),
                media: CommittedMedia::new("a.mkv", Some(5), Some(110.0)),
            },
        )
        .await
        .unwrap();
        assert!(latest.media_accepted);
        assert_eq!(state.client_state.get_file().as_deref(), Some("a.mkv"));
    }

    #[tokio::test]
    async fn ambiguous_untagged_observation_is_not_reported_as_settled() {
        let (state, _player, _server, _directory) = fixture().await;
        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("a.mkv", Some(5), Some(110.0)),
            },
        )
        .await
        .unwrap();
        local_select(&state, 1, true).await.unwrap();

        let result = dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("a.mkv", Some(5), Some(110.0)),
            },
        )
        .await
        .unwrap();

        assert!(!result.media_accepted);
        assert!(!result.media_settled);
    }

    #[tokio::test]
    async fn settled_load_is_not_aborted_during_post_commit_sync() {
        let (state, player, _server, _directory) = fixture().await;
        local_select(&state, 1, true).await.unwrap();
        wait_for_load_count(&player, 1).await;
        let load_id = state.playback.snapshot().pending_load.unwrap().id;

        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(load_id),
                media: CommittedMedia::new("b.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();
        assert!(state.playback.snapshot().pending_load.is_none());
        assert!(state.playback.active_load(load_id).is_some());

        fail_load(&state, load_id).await;

        assert!(state.playback.active_load(load_id).is_some());
        state.playback.finish_load(load_id);
    }

    #[tokio::test]
    async fn eof_does_not_treat_fuzzy_playlist_names_as_the_same_item() {
        let (state, _player, _server, _directory) = fixture().await;
        replace_playlist(&state, vec!["Episode-01.mkv".into()], Some(0))
            .await
            .unwrap();
        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("Episode 01.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();

        let action = advance_after_eof(&state, "Episode 01.mkv", true, false, -1.0)
            .await
            .unwrap();

        assert!(matches!(action, EofAction::None));
    }

    #[tokio::test]
    async fn concurrent_eof_reports_advance_exactly_once() {
        let (state, player, _server, _directory) = fixture().await;
        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("a.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();

        let first_state = state.clone();
        let first = tokio::spawn(async move {
            advance_after_eof(&first_state, "a.mkv", false, false, -1.0).await
        });
        let second_state = state.clone();
        let second = tokio::spawn(async move {
            advance_after_eof(&second_state, "a.mkv", false, false, -1.0).await
        });

        let actions = [
            first.await.unwrap().unwrap(),
            second.await.unwrap().unwrap(),
        ];
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, EofAction::Load))
                .count(),
            1
        );
        wait_for_load_count(&player, 1).await;
        assert_eq!(
            state.playback.snapshot().pending_load.unwrap().target,
            "b.mkv"
        );
        assert_eq!(
            player
                .commands()
                .iter()
                .filter(|command| matches!(command, FakePlayerCommand::LoadFile(_)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn disabled_shared_playlist_observes_authority_and_reconciles_when_enabled() {
        let (state, player, _server, _directory) = fixture().await;
        state.server_features.lock().shared_playlists = false;

        server_playlist_and_index(&state, None, Some((Some(1), true)))
            .await
            .unwrap();
        assert_eq!(state.playback.snapshot().authoritative_index, Some(1));
        assert!(state.playback.snapshot().pending_load.is_none());
        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: None,
                media: CommittedMedia::new("a.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();
        assert_eq!(state.playback.snapshot().proposed_index, None);
        assert_eq!(state.playback.snapshot().displayed_index(), Some(1));

        state.server_features.lock().shared_playlists = true;
        reconcile_shared_playlist(&state).await.unwrap();
        wait_for_load_count(&player, 1).await;
        assert_eq!(
            state.playback.snapshot().pending_load.unwrap().target,
            "b.mkv"
        );
    }

    #[tokio::test]
    async fn removing_current_item_at_the_same_index_loads_the_new_server_target() {
        let (state, player, _server, _directory) = fixture().await;
        server_playlist_and_index(&state, None, Some((Some(1), false)))
            .await
            .unwrap();
        wait_for_load_count(&player, 1).await;
        let load_b = state.playback.snapshot().pending_load.unwrap().id;
        dispatch(
            &state,
            PlaybackEvent::PlayerMediaCommitted {
                load_id: Some(load_b),
                media: CommittedMedia::new("b.mkv", Some(5), Some(120.0)),
            },
        )
        .await
        .unwrap();
        state.playback.finish_load(load_b);

        let edit = edit_playlist(&state, false, |items, _| {
            items.remove(1);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(edit.previous_index, Some(1));
        assert_eq!(edit.index, Some(1));

        server_playlist_and_index(&state, None, Some((Some(1), false)))
            .await
            .unwrap();
        wait_for_load_count(&player, 2).await;
        assert_eq!(
            state.playback.snapshot().pending_load.unwrap().target,
            "c.mkv"
        );
    }

    #[tokio::test]
    async fn concurrent_playlist_edits_use_the_latest_serialized_snapshot() {
        let (state, _player, _server, _directory) = fixture().await;
        let first_state = state.clone();
        let first = tokio::spawn(async move {
            edit_playlist(&first_state, false, |items, _| {
                items.push("d.mkv".to_string());
                Ok(())
            })
            .await
        });
        let second_state = state.clone();
        let second = tokio::spawn(async move {
            edit_playlist(&second_state, false, |items, _| {
                items.push("e.mkv".to_string());
                Ok(())
            })
            .await
        });

        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();

        let items = state.playback.snapshot().playlist_items;
        assert_eq!(items.len(), 5);
        assert!(items.iter().any(|item| item == "d.mkv"));
        assert!(items.iter().any(|item| item == "e.mkv"));
    }
}
