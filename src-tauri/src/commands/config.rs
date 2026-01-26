// Configuration command handlers

use crate::config::{load_config, save_config, SyncplayConfig};
use tauri::State;

#[tauri::command]
pub async fn get_config() -> Result<SyncplayConfig, String> {
    tracing::info!("Getting configuration");

    load_config().map_err(|e| {
        tracing::error!("Failed to load config: {}", e);
        format!("Failed to load configuration: {}", e)
    })
}

#[tauri::command]
pub async fn update_config(config: SyncplayConfig) -> Result<(), String> {
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

    Ok(())
}

#[tauri::command]
pub async fn get_config_path() -> Result<String, String> {
    crate::config::get_config_path()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("Failed to get config path: {}", e))
}
