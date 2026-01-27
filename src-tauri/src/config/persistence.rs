// Persistence module
// Configuration storage via tauri-plugin-store

use super::settings::SyncplayConfig;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::{resolve_store_path, StoreBuilder};

const STORE_PATH: &str = "syncplay.store.json";
const CONFIG_KEY: &str = "config";

/// Get the configuration store path
pub fn get_config_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    resolve_store_path(app, STORE_PATH).context("Failed to resolve store path")
}

/// Load configuration from the store
pub fn load_config<R: Runtime>(app: &AppHandle<R>) -> Result<SyncplayConfig> {
    let store = StoreBuilder::new(app, STORE_PATH)
        .build()
        .context("Failed to open config store")?;

    if let Some(value) = store.get(CONFIG_KEY) {
        if let Ok(config) = serde_json::from_value::<SyncplayConfig>(value) {
            return Ok(config);
        }
        tracing::warn!("Failed to deserialize config, resetting to defaults");
    }

    let config = SyncplayConfig::default();
    let value = serde_json::to_value(&config).context("Failed to serialize default config")?;
    store.set(CONFIG_KEY.to_string(), value);
    store.save().context("Failed to persist default config")?;
    Ok(config)
}

/// Save configuration to the store
pub fn save_config<R: Runtime>(app: &AppHandle<R>, config: &SyncplayConfig) -> Result<()> {
    let store = StoreBuilder::new(app, STORE_PATH)
        .build()
        .context("Failed to open config store")?;

    let value = serde_json::to_value(config).context("Failed to serialize config")?;
    store.set(CONFIG_KEY.to_string(), value);
    store.save().context("Failed to save config store")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    fn build_test_app() -> tauri::App<tauri::test::MockRuntime> {
        mock_builder()
            .plugin(tauri_plugin_store::Builder::default().build())
            .build(mock_context(noop_assets()))
            .unwrap()
    }

    fn clear_store(app: &tauri::App<tauri::test::MockRuntime>) {
        if let Ok(path) = get_config_path(app.handle()) {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn test_save_and_load_config() {
        let app = build_test_app();
        clear_store(&app);
        let mut config = SyncplayConfig::default();
        config.user.username = "testuser".to_string();
        config.server.host = "example.com".to_string();
        config.server.port = 9000;

        save_config(app.handle(), &config).unwrap();
        let loaded = load_config(app.handle()).unwrap();

        assert_eq!(loaded.server.host, "example.com");
        assert_eq!(loaded.server.port, 9000);
        assert_eq!(loaded.user.username, "testuser");
    }

    #[test]
    fn test_config_path() {
        let app = build_test_app();
        let path = get_config_path(app.handle()).unwrap();
        assert!(path.to_string_lossy().contains("syncplay"));
        assert!(path.to_string_lossy().ends_with(STORE_PATH));
    }

    #[test]
    fn test_load_nonexistent_config() {
        let app = build_test_app();
        clear_store(&app);
        let config = load_config(app.handle()).unwrap();
        assert_eq!(config.server.host, "syncplay.pl");
    }
}
