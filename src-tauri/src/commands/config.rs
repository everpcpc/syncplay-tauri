// Configuration command handlers

use crate::app_state::AppState;
use crate::config::{save_config, SyncplayConfig};
use std::sync::Arc;
use tauri::{AppHandle, Runtime, State};

#[tauri::command]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> Result<SyncplayConfig, String> {
    tracing::info!("Getting configuration");

    Ok(state.config.lock().clone())
}

#[tauri::command]
pub async fn update_config<R: Runtime>(
    app: AppHandle<R>,
    config: SyncplayConfig,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("Updating configuration");
    let shared_playlist_was_enabled = state.config.lock().user.shared_playlist_enabled;

    // Validate config
    config.validate().map_err(|e| {
        tracing::error!("Config validation failed: {}", e);
        e
    })?;

    // Save config
    save_config(&app, &config).map_err(|e| {
        tracing::error!("Failed to save config: {}", e);
        format!("Failed to save configuration: {}", e)
    })?;

    *state.config.lock() = config.clone();
    state.sync_engine.lock().update_from_config(&config.user);
    if state.media_index.update_settings(
        config.player.media_directories.clone(),
        config.player.media_index_timeout_seconds,
    ) {
        state
            .media_index
            .clone()
            .request_refresh(state.inner().clone());
    }
    {
        let mut autoplay = state.autoplay.lock();
        autoplay.enabled = config.user.autoplay_enabled;
        autoplay.min_users = config.user.autoplay_min_users;
        autoplay.require_same_filenames = config.user.autoplay_require_same_filenames;
        autoplay.unpause_action = config.user.unpause_action.clone();
        if !autoplay.enabled {
            autoplay.countdown_active = false;
            autoplay.countdown_remaining = 0;
        }
    }
    if !shared_playlist_was_enabled
        && config.user.shared_playlist_enabled
        && state.server_features.lock().shared_playlists
        && state.connection.lock().is_some()
    {
        if let Err(error) =
            crate::client::playback_runtime::reconcile_shared_playlist(state.inner()).await
        {
            tracing::warn!("Failed to reconcile shared playlist after enabling it: {error}");
        }
    }
    state.emit_event("config-updated", config.clone());

    Ok(())
}

#[tauri::command]
pub async fn get_config_path<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    crate::config::get_config_path(&app)
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("Failed to get config path: {}", e))
}

#[tauri::command]
pub async fn refresh_media_index(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state
        .media_index
        .clone()
        .request_refresh_force(state.inner().clone());
    Ok(())
}

#[tauri::command]
pub async fn get_media_index_refreshing(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.media_index.is_refreshing())
}
