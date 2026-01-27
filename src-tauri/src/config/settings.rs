// Settings module
// Configuration structures and defaults

use serde::{Deserialize, Serialize};

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub password: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "syncplay.pl".to_string(),
            port: 8999,
            password: None,
        }
    }
}

/// User preferences
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    pub username: String,
    pub default_room: String,

    // Sync thresholds
    pub seek_threshold_rewind: f64,
    pub seek_threshold_fastforward: f64,
    pub slowdown_threshold: f64,
    pub slowdown_reset_threshold: f64,
    pub slowdown_rate: f64,

    // OSD settings
    pub show_osd: bool,
    pub osd_duration: u64,

    // UI settings
    pub show_playlist: bool,
    pub auto_connect: bool,
}

/// Player configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerConfig {
    #[serde(default)]
    pub player_path: String,
    #[serde(default)]
    pub mpv_socket_path: String,
    pub media_directories: Vec<String>,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        #[cfg(unix)]
        let default_socket = "/tmp/mpvsocket".to_string();
        #[cfg(windows)]
        let default_socket = "\\\\.\\pipe\\mpvsocket".to_string();

        Self {
            player_path: "mpv".to_string(),
            mpv_socket_path: default_socket,
            media_directories: Vec::new(),
        }
    }
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            username: String::new(),
            default_room: "default".to_string(),

            // Default sync thresholds (from sync engine)
            seek_threshold_rewind: 4.0,
            seek_threshold_fastforward: 5.0,
            slowdown_threshold: 1.5,
            slowdown_reset_threshold: 0.5,
            slowdown_rate: 0.95,

            // OSD defaults
            show_osd: true,
            osd_duration: 3000,

            // UI defaults
            show_playlist: true,
            auto_connect: false,
        }
    }
}

/// Complete Syncplay configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncplayConfig {
    pub server: ServerConfig,
    pub user: UserPreferences,
    pub player: PlayerConfig,
    pub recent_servers: Vec<ServerConfig>,
}

impl Default for SyncplayConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            user: UserPreferences::default(),
            player: PlayerConfig::default(),
            recent_servers: vec![
                ServerConfig::default(),
                ServerConfig {
                    host: "syncplay.pl".to_string(),
                    port: 8995,
                    password: None,
                },
            ],
        }
    }
}

impl SyncplayConfig {
    /// Create a new configuration with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.user.username.is_empty() {
            return Err("Username cannot be empty".to_string());
        }

        if self.server.host.is_empty() {
            return Err("Server host cannot be empty".to_string());
        }

        if self.server.port == 0 {
            return Err("Server port must be greater than 0".to_string());
        }

        // Validate thresholds
        if self.user.seek_threshold_rewind <= 0.0 {
            return Err("Seek threshold rewind must be positive".to_string());
        }

        if self.user.seek_threshold_fastforward <= 0.0 {
            return Err("Seek threshold fastforward must be positive".to_string());
        }

        if self.user.slowdown_threshold <= 0.0 {
            return Err("Slowdown threshold must be positive".to_string());
        }

        if self.user.slowdown_rate <= 0.0 || self.user.slowdown_rate >= 1.0 {
            return Err("Slowdown rate must be between 0 and 1".to_string());
        }

        Ok(())
    }

    /// Add a server to recent servers list
    pub fn add_recent_server(&mut self, server: ServerConfig) {
        // Remove duplicates
        self.recent_servers
            .retain(|s| s.host != server.host || s.port != server.port);

        // Add to front
        self.recent_servers.insert(0, server);

        // Keep only last 10
        self.recent_servers.truncate(10);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SyncplayConfig::default();
        assert_eq!(config.server.host, "syncplay.pl");
        assert_eq!(config.server.port, 8999);
        assert_eq!(config.user.default_room, "default");
    }

    #[test]
    fn test_validate_empty_username() {
        let mut config = SyncplayConfig::default();
        config.user.username = String::new();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_valid_config() {
        let mut config = SyncplayConfig::default();
        config.user.username = "testuser".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_add_recent_server() {
        let mut config = SyncplayConfig::default();
        let server = ServerConfig {
            host: "example.com".to_string(),
            port: 8999,
            password: None,
        };

        config.add_recent_server(server.clone());
        assert_eq!(config.recent_servers.len(), 3); // 2 default + 1 new
        assert_eq!(config.recent_servers[0].host, "example.com");
    }

    #[test]
    fn test_recent_servers_limit() {
        let mut config = SyncplayConfig::default();

        for i in 0..15 {
            config.add_recent_server(ServerConfig {
                host: format!("server{}.com", i),
                port: 8999,
                password: None,
            });
        }

        assert_eq!(config.recent_servers.len(), 10);
    }
}
