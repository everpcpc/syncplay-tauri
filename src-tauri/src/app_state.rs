use anyhow::Result;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};
use tempfile::TempDir;

use crate::client::{chat::ChatManager, playlist::Playlist, state::ClientState, sync::SyncEngine};
use crate::config::{SyncplayConfig, UnpauseAction};
use crate::network::connection::Connection;
use crate::network::messages::HelloMessage;
use crate::network::ping::PingService;
use crate::player::backend::{PlayerBackend, PlayerKind};

/// Global application state
pub struct AppState {
    /// Network connection to Syncplay server
    pub connection: Arc<Mutex<Option<Arc<Connection>>>>,
    /// Player backend instance
    pub player: Arc<Mutex<Option<Arc<dyn PlayerBackend>>>>,
    /// Player process handle
    pub player_process: Arc<Mutex<Option<tokio::process::Child>>>,
    /// Client state (users, room, etc.)
    pub client_state: Arc<ClientState>,
    /// Playlist manager
    pub playlist: Arc<Playlist>,
    /// Chat manager
    pub chat: Arc<ChatManager>,
    /// Synchronization engine
    pub sync_engine: Arc<Mutex<SyncEngine>>,
    /// Cached configuration
    pub config: Arc<Mutex<SyncplayConfig>>,
    /// Suppress next file update for server-driven loads
    pub suppress_next_file_update: Arc<Mutex<bool>>,
    /// Suppress unpause checks for remote updates
    pub suppress_unpause_check: Arc<Mutex<bool>>,
    /// Last hello payload (for TLS re-handshake)
    pub last_hello: Arc<Mutex<Option<HelloMessage>>>,
    /// Whether hello has been sent for the current connection
    pub hello_sent: Arc<Mutex<bool>>,
    /// Tauri app handle for event emission
    pub app_handle: Arc<Mutex<Option<AppHandle>>>,
    /// Autoplay countdown state
    pub autoplay: Arc<Mutex<AutoPlayState>>,
    /// Ping RTT tracking
    pub ping_service: Arc<Mutex<PingService>>,
    /// Last latency calculation timestamp from server
    pub last_latency_calculation: Arc<Mutex<Option<f64>>>,
    /// Last time a player process was spawned
    pub last_player_spawn: Arc<Mutex<Option<Instant>>>,
    /// Kind of the last spawned player
    pub last_player_kind: Arc<Mutex<Option<PlayerKind>>>,
    /// Whether a player connection is in progress
    pub player_connecting: Arc<Mutex<bool>>,
    /// Runtime directory for MPV IPC socket
    pub mpv_runtime_dir: Arc<Mutex<Option<TempDir>>>,
    /// Cached MPV IPC socket path
    pub mpv_socket_path: Arc<Mutex<Option<String>>>,
    /// Cached detected players
    pub detected_players: Arc<Mutex<Vec<crate::player::detection::DetectedPlayer>>>,
    /// Timestamp (ms) when players were detected
    pub detected_players_updated_at: Arc<Mutex<Option<i64>>>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            connection: Arc::new(Mutex::new(None)),
            player: Arc::new(Mutex::new(None)),
            player_process: Arc::new(Mutex::new(None)),
            client_state: ClientState::new(),
            playlist: Playlist::new(),
            chat: ChatManager::new(),
            sync_engine: Arc::new(Mutex::new(SyncEngine::new())),
            config: Arc::new(Mutex::new(SyncplayConfig::default())),
            suppress_next_file_update: Arc::new(Mutex::new(false)),
            suppress_unpause_check: Arc::new(Mutex::new(false)),
            last_hello: Arc::new(Mutex::new(None)),
            hello_sent: Arc::new(Mutex::new(false)),
            app_handle: Arc::new(Mutex::new(None)),
            autoplay: Arc::new(Mutex::new(AutoPlayState::default())),
            ping_service: Arc::new(Mutex::new(PingService::default())),
            last_latency_calculation: Arc::new(Mutex::new(None)),
            last_player_spawn: Arc::new(Mutex::new(None)),
            last_player_kind: Arc::new(Mutex::new(None)),
            mpv_runtime_dir: Arc::new(Mutex::new(None)),
            mpv_socket_path: Arc::new(Mutex::new(None)),
            player_connecting: Arc::new(Mutex::new(false)),
            detected_players: Arc::new(Mutex::new(Vec::new())),
            detected_players_updated_at: Arc::new(Mutex::new(None)),
        })
    }

    /// Set the Tauri app handle for event emission
    pub fn set_app_handle(&self, handle: AppHandle) {
        *self.app_handle.lock() = Some(handle);
    }

    /// Emit an event to the frontend
    pub fn emit_event(&self, event: &str, payload: impl serde::Serialize + Clone) {
        if let Some(handle) = self.app_handle.lock().as_ref() {
            if let Err(e) = handle.emit(event, payload) {
                tracing::error!("Failed to emit event {}: {}", event, e);
            }
        }
    }

    /// Check if connected to server
    pub fn is_connected(&self) -> bool {
        self.connection
            .lock()
            .as_ref()
            .map(|c| c.is_connected())
            .unwrap_or(false)
    }

    /// Check if player is connected
    pub fn is_player_connected(&self) -> bool {
        self.player.lock().is_some()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            connection: Arc::new(Mutex::new(None)),
            player: Arc::new(Mutex::new(None)),
            player_process: Arc::new(Mutex::new(None)),
            client_state: ClientState::new(),
            playlist: Playlist::new(),
            chat: ChatManager::new(),
            sync_engine: Arc::new(Mutex::new(SyncEngine::new())),
            config: Arc::new(Mutex::new(SyncplayConfig::default())),
            suppress_next_file_update: Arc::new(Mutex::new(false)),
            suppress_unpause_check: Arc::new(Mutex::new(false)),
            last_hello: Arc::new(Mutex::new(None)),
            hello_sent: Arc::new(Mutex::new(false)),
            app_handle: Arc::new(Mutex::new(None)),
            autoplay: Arc::new(Mutex::new(AutoPlayState::default())),
            ping_service: Arc::new(Mutex::new(PingService::default())),
            last_latency_calculation: Arc::new(Mutex::new(None)),
            last_player_spawn: Arc::new(Mutex::new(None)),
            last_player_kind: Arc::new(Mutex::new(None)),
            mpv_runtime_dir: Arc::new(Mutex::new(None)),
            mpv_socket_path: Arc::new(Mutex::new(None)),
            player_connecting: Arc::new(Mutex::new(false)),
            detected_players: Arc::new(Mutex::new(Vec::new())),
            detected_players_updated_at: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AutoPlayState {
    pub enabled: bool,
    pub min_users: i32,
    pub require_same_filenames: bool,
    pub unpause_action: UnpauseAction,
    pub countdown_active: bool,
    pub countdown_remaining: i32,
}

impl Default for AutoPlayState {
    fn default() -> Self {
        Self {
            enabled: false,
            min_users: -1,
            require_same_filenames: true,
            unpause_action: UnpauseAction::IfOthersReady,
            countdown_active: false,
            countdown_remaining: 0,
        }
    }
}

/// Event payloads for frontend

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectionStatusEvent {
    pub connected: bool,
    pub server: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UserListEvent {
    pub users: Vec<UserInfo>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UserInfo {
    pub username: String,
    pub room: String,
    pub file: Option<String>,
    pub is_ready: bool,
    pub is_controller: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatMessageEvent {
    pub timestamp: String,
    pub username: Option<String>,
    pub message: String,
    pub message_type: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlaylistEvent {
    pub items: Vec<String>,
    pub current_index: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlayerStateEvent {
    pub filename: Option<String>,
    pub position: Option<f64>,
    pub duration: Option<f64>,
    pub paused: Option<bool>,
    pub speed: Option<f64>,
}
