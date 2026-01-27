// Configuration command handlers

use crate::app_state::AppState;
use crate::config::{save_config, SyncplayConfig};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> Result<SyncplayConfig, String> {
    tracing::info!("Getting configuration");

    Ok(state.config.lock().clone())
}

#[tauri::command]
pub async fn update_config(
    config: SyncplayConfig,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("Updating configuration");

    // Validate config
    config.validate().map_err(|e| {
        tracing::error!("Config validation failed: {}", e);
        e
    })?;

    // Save config
    save_config(&config).map_err(|e| {
        tracing::error!("Failed to save config: {}", e);
        format!("Failed to save configuration: {}", e)
    })?;

    *state.config.lock() = config.clone();
    state.sync_engine.lock().update_from_config(&config.user);

    Ok(())
}

#[tauri::command]
pub async fn get_config_path() -> Result<String, String> {
    crate::config::get_config_path()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("Failed to get config path: {}", e))
}
