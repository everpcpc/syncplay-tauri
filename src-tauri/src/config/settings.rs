// Settings module
// Configuration structures and defaults

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode {
    SendRaw,
    SendHashed,
    DoNotSend,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnpauseAction {
    IfAlreadyReady,
    IfOthersReady,
    IfMinUsersReady,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatInputPosition {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatOutputMode {
    Chatroom,
    Scrolling,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TransparencyMode {
    #[default]
    Off,
    Low,
    High,
}

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
    pub room_list: Vec<String>,
    pub theme: String,
    #[serde(default)]
    pub transparency_mode: TransparencyMode,

    // Sync thresholds
    pub seek_threshold_rewind: f64,
    pub seek_threshold_fastforward: f64,
    pub slowdown_threshold: f64,
    pub slowdown_reset_threshold: f64,
    pub slowdown_rate: f64,
    pub slow_on_desync: bool,
    pub rewind_on_desync: bool,
    pub fastforward_on_desync: bool,
    pub dont_slow_down_with_me: bool,

    // Ready & autoplay
    pub ready_at_start: bool,
    pub pause_on_leave: bool,
    pub unpause_action: UnpauseAction,
    pub autoplay_enabled: bool,
    pub autoplay_min_users: i32,
    pub autoplay_require_same_filenames: bool,

    // Privacy
    pub filename_privacy_mode: PrivacyMode,
    pub filesize_privacy_mode: PrivacyMode,

    // Trusted domains
    pub only_switch_to_trusted_domains: bool,
    pub trusted_domains: Vec<String>,

    // OSD settings
    pub show_osd: bool,
    pub osd_duration: u64,
    pub show_osd_warnings: bool,
    pub show_slowdown_osd: bool,
    pub show_different_room_osd: bool,
    pub show_same_room_osd: bool,
    pub show_non_controller_osd: bool,
    pub show_duration_notification: bool,

    // Chat settings
    pub chat_input_enabled: bool,
    pub chat_direct_input: bool,
    pub chat_input_font_family: String,
    pub chat_input_relative_font_size: u32,
    pub chat_input_font_weight: u32,
    pub chat_input_font_underline: bool,
    pub chat_input_font_color: String,
    pub chat_input_position: ChatInputPosition,
    pub chat_output_enabled: bool,
    pub chat_output_font_family: String,
    pub chat_output_relative_font_size: u32,
    pub chat_output_font_weight: u32,
    pub chat_output_font_underline: bool,
    pub chat_output_mode: ChatOutputMode,
    pub chat_max_lines: u32,
    pub chat_top_margin: u32,
    pub chat_left_margin: u32,
    pub chat_bottom_margin: u32,
    pub chat_move_osd: bool,
    pub chat_osd_margin: u32,
    pub notification_timeout: u32,
    pub alert_timeout: u32,
    pub chat_timeout: u32,

    // UI settings
    pub autosave_joins_to_list: bool,
    pub shared_playlist_enabled: bool,
    pub loop_at_end_of_playlist: bool,
    pub loop_single_files: bool,
    pub show_playlist: bool,
    #[serde(default = "default_side_panel_layout")]
    pub side_panel_layout: String,
    pub auto_connect: bool,
    pub force_gui_prompt: bool,
    pub check_for_updates_automatically: Option<bool>,
    pub debug: bool,
}

/// Player configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerConfig {
    #[serde(default)]
    pub player_path: String,
    #[serde(default)]
    pub mpv_socket_path: String,
    pub media_directories: Vec<String>,
    #[serde(default)]
    pub player_arguments: Vec<String>,
    #[serde(default)]
    pub per_player_arguments: HashMap<String, Vec<String>>,
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
            player_arguments: Vec::new(),
            per_player_arguments: HashMap::new(),
        }
    }
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            username: String::new(),
            default_room: "default".to_string(),
            room_list: Vec::new(),
            theme: "dark".to_string(),
            transparency_mode: TransparencyMode::Off,

            // Default sync thresholds (from sync engine)
            seek_threshold_rewind: 4.0,
            seek_threshold_fastforward: 5.0,
            slowdown_threshold: 1.5,
            slowdown_reset_threshold: 0.1,
            slowdown_rate: 0.95,
            slow_on_desync: true,
            rewind_on_desync: true,
            fastforward_on_desync: true,
            dont_slow_down_with_me: false,

            // Ready & autoplay defaults
            ready_at_start: false,
            pause_on_leave: false,
            unpause_action: UnpauseAction::IfOthersReady,
            autoplay_enabled: false,
            autoplay_min_users: -1,
            autoplay_require_same_filenames: true,

            // Privacy defaults
            filename_privacy_mode: PrivacyMode::SendRaw,
            filesize_privacy_mode: PrivacyMode::SendRaw,

            // Trusted domains defaults
            only_switch_to_trusted_domains: true,
            trusted_domains: vec!["youtube.com".to_string(), "youtu.be".to_string()],

            // OSD defaults
            show_osd: true,
            osd_duration: 3000,
            show_osd_warnings: true,
            show_slowdown_osd: true,
            show_different_room_osd: false,
            show_same_room_osd: true,
            show_non_controller_osd: false,
            show_duration_notification: true,

            // Chat defaults
            chat_input_enabled: true,
            chat_direct_input: false,
            chat_input_font_family: "sans-serif".to_string(),
            chat_input_relative_font_size: 24,
            chat_input_font_weight: 1,
            chat_input_font_underline: false,
            chat_input_font_color: "#FFFF00".to_string(),
            chat_input_position: ChatInputPosition::Top,
            chat_output_enabled: true,
            chat_output_font_family: "sans-serif".to_string(),
            chat_output_relative_font_size: 24,
            chat_output_font_weight: 1,
            chat_output_font_underline: false,
            chat_output_mode: ChatOutputMode::Chatroom,
            chat_max_lines: 7,
            chat_top_margin: 25,
            chat_left_margin: 20,
            chat_bottom_margin: 30,
            chat_move_osd: true,
            chat_osd_margin: 110,
            notification_timeout: 3,
            alert_timeout: 5,
            chat_timeout: 7,

            // UI defaults
            autosave_joins_to_list: true,
            shared_playlist_enabled: true,
            loop_at_end_of_playlist: false,
            loop_single_files: false,
            show_playlist: true,
            side_panel_layout: default_side_panel_layout(),
            auto_connect: false,
            force_gui_prompt: true,
            check_for_updates_automatically: None,
            debug: false,
        }
    }
}

fn default_side_panel_layout() -> String {
    "rows".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicServer {
    pub name: String,
    pub address: String,
}

/// Complete Syncplay configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncplayConfig {
    pub server: ServerConfig,
    pub user: UserPreferences,
    pub player: PlayerConfig,
    pub recent_servers: Vec<ServerConfig>,
    pub public_servers: Vec<PublicServer>,
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
            public_servers: vec![
                PublicServer {
                    name: "syncplay.pl:8995 (France)".to_string(),
                    address: "syncplay.pl:8995".to_string(),
                },
                PublicServer {
                    name: "syncplay.pl:8996 (France)".to_string(),
                    address: "syncplay.pl:8996".to_string(),
                },
                PublicServer {
                    name: "syncplay.pl:8997 (France)".to_string(),
                    address: "syncplay.pl:8997".to_string(),
                },
                PublicServer {
                    name: "syncplay.pl:8998 (France)".to_string(),
                    address: "syncplay.pl:8998".to_string(),
                },
                PublicServer {
                    name: "syncplay.pl:8999 (France)".to_string(),
                    address: "syncplay.pl:8999".to_string(),
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

        if self.user.osd_duration == 0 {
            return Err("OSD duration must be positive".to_string());
        }

        if self.user.chat_max_lines == 0 {
            return Err("Chat max lines must be positive".to_string());
        }

        if self.user.autoplay_min_users < -1 {
            return Err("Autoplay min users must be >= -1".to_string());
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
    fn test_validate_empty_host() {
        let mut config = SyncplayConfig::default();
        config.server.host = String::new();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_valid_config() {
        let mut config = SyncplayConfig::default();
        config.user.username = String::new();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_default_public_servers() {
        let config = SyncplayConfig::default();
        assert!(!config.public_servers.is_empty());
        assert!(config
            .public_servers
            .iter()
            .any(|entry| entry.address == "syncplay.pl:8999"));
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
