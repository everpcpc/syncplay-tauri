use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Main protocol message envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(non_snake_case)]
pub enum ProtocolMessage {
    Hello { Hello: HelloMessage },
    Set { Set: SetMessage },
    State { State: StateMessage },
    List { List: Option<ListResponse> },
    Chat { Chat: ChatMessage },
    Error { Error: ErrorMessage },
    #[allow(clippy::upper_case_acronyms)]
    TLS { TLS: TLSMessage },
}

/// Hello message - initial handshake
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloMessage {
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<RoomInfo>,
    pub version: String,
    pub realversion: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<ClientFeatures>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientFeatures {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_playlists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_state: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_rooms: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistent_rooms: Option<bool>,
}

/// Set message - update settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<RoomInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<HashMap<String, UserUpdate>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<ReadyState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_index: Option<PlaylistIndexUpdate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_change: Option<PlaylistChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<ClientFeatures>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<RoomInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<UserEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<ClientFeatures>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserEvent {
    Joined,
    Left,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyState {
    pub username: String,
    pub is_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manually_initiated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistIndexUpdate {
    pub user: String,
    pub index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistChange {
    pub user: String,
    pub files: Vec<String>,
}

/// State message - synchronization state
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playstate: Option<PlayState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ping: Option<PingInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignoring_on_the_fly: Option<IgnoringInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayState {
    pub position: f64,
    pub paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub do_seek: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_calculation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_latency_calculation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_rtt: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IgnoringInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<u32>,
}

/// List response - user list
pub type ListResponse = HashMap<String, HashMap<String, UserInfo>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<ClientFeatures>,
}

/// Chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub username: String,
    pub message: String,
}

/// Error message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub message: String,
}

/// TLS message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TLSMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_tls: Option<String>,
}

