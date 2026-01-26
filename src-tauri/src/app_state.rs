use anyhow::Result;
use parking_lot::Mutex;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::client::{
    chat::ChatManager,
    playlist::Playlist,
    state::ClientState,
    sync::SyncEngine,
};
use crate::network::connection::Connection;
use crate::player::mpv_ipc::MpvIpc;

/// Global application state
pub struct AppState {
    /// Network connection to Syncplay server
    pub connection: Arc<Mutex<Option<Connection>>>,
    /// MPV player IPC client
    pub mpv: Arc<Mutex<Option<MpvIpc>>>,
    /// Client state (users, room, etc.)
    pub client_state: Arc<ClientState>,
    /// Playlist manager
    pub playlist: Arc<Playlist>,
    /// Chat manager
    pub chat: Arc<ChatManager>,
    /// Synchronization engine
    pub sync_engine: Arc<Mutex<SyncEngine>>,
    /// Tauri app handle for event emission
    pub app_handle: Arc<Mutex<Option<AppHandle>>>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            connection: Arc::new(Mutex::new(None)),
            mpv: Arc::new(Mutex::new(None)),
            client_state: ClientState::new(),
            playlist: Playlist::new(),
            chat: ChatManager::new(),
            sync_engine: Arc::new(Mutex::new(SyncEngine::new())),
            app_handle: Arc::new(Mutex::new(None)),
        })
    }

    /// Set the Tauri app handle for event emission
    pub fn set_app_handle(&self, handle: AppHandle) {
        *self.app_handle.lock() = Some(handle);
    }

    /// Emit an event to the frontend
    pub fn emit_event(&self, event: &str, payload: impl serde::Serialize + Clone) {
        if let Some(handle) = self.app_handle.lock().as_ref() {
            if let Err(e) = handle.emit_all(event, payload) {
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

    /// Check if MPV is connected
    pub fn is_mpv_connected(&self) -> bool {
        self.mpv.lock().is_some()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            connection: Arc::new(Mutex::new(None)),
            mpv: Arc::new(Mutex::new(None)),
            client_state: ClientState::new(),
            playlist: Playlist::new(),
            chat: ChatManager::new(),
            sync_engine: Arc::new(Mutex::new(SyncEngine::new())),
            app_handle: Arc::new(Mutex::new(None)),
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
