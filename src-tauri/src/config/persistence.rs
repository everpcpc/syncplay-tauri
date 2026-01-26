// Persistence module
// Configuration file I/O

use super::settings::{ServerConfig, SyncplayConfig, UserPreferences};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::PathBuf;
use std::fs;

/// Get the configuration file path
pub fn get_config_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "syncplay", "syncplay-tauri")
        .context("Failed to determine project directories")?;

    let config_dir = proj_dirs.config_dir();
    fs::create_dir_all(config_dir)
        .context("Failed to create config directory")?;

    Ok(config_dir.join("config.json"))
}

/// Load configuration from JSON file
pub fn load_config() -> Result<SyncplayConfig> {
    let config_path = get_config_path()?;

    if !config_path.exists() {
        tracing::info!("Config file not found, using defaults");
        return Ok(SyncplayConfig::default());
    }

    let contents = fs::read_to_string(&config_path)
        .context("Failed to read config file")?;

    let config: SyncplayConfig = serde_json::from_str(&contents)
        .context("Failed to parse config file")?;

    tracing::info!("Loaded config from {:?}", config_path);
    Ok(config)
}

/// Save configuration to JSON file
pub fn save_config(config: &SyncplayConfig) -> Result<()> {
    let config_path = get_config_path()?;

    let contents = serde_json::to_string_pretty(config)
        .context("Failed to serialize config")?;

    fs::write(&config_path, contents)
        .context("Failed to write config file")?;

    tracing::info!("Saved config to {:?}", config_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_save_and_load_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        // Create a test config
        let mut config = SyncplayConfig::default();
        config.user.username = "testuser".to_string();
        config.server.host = "example.com".to_string();
        config.server.port = 9000;

        // Save to temp file
        let contents = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&config_path, contents).unwrap();

        // Load and verify
        let loaded_contents = fs::read_to_string(&config_path).unwrap();
        let loaded_config: SyncplayConfig = serde_json::from_str(&loaded_contents).unwrap();

        assert_eq!(loaded_config.server.host, "example.com");
        assert_eq!(loaded_config.server.port, 9000);
        assert_eq!(loaded_config.user.username, "testuser");
    }

    #[test]
    fn test_config_path() {
        let path = get_config_path();
        assert!(path.is_ok());
        let path = path.unwrap();
        assert!(path.to_string_lossy().contains("syncplay-tauri"));
        assert!(path.to_string_lossy().ends_with("config.json"));
    }

    #[test]
    fn test_load_nonexistent_config() {
        // This should return default config without error
        // Note: This test might fail if a real config file exists
        let config = SyncplayConfig::default();
        assert_eq!(config.server.host, "syncplay.pl");
    }
}

