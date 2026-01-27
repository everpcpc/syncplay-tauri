use serde::de::{self, Deserializer};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Main protocol message envelope
#[derive(Debug, Clone)]
#[allow(non_snake_case)]
pub enum ProtocolMessage {
    Hello {
        Hello: HelloMessage,
    },
    Set {
        Set: SetMessage,
    },
    State {
        State: StateMessage,
    },
    Chat {
        Chat: ChatMessage,
    },
    Error {
        Error: ErrorMessage,
    },
    #[allow(clippy::upper_case_acronyms)]
    TLS {
        TLS: TLSMessage,
    },
    List {
        List: Option<ListResponse>,
    },
}

impl Serialize for ProtocolMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            ProtocolMessage::Hello { Hello } => map.serialize_entry("Hello", Hello)?,
            ProtocolMessage::Set { Set } => map.serialize_entry("Set", Set)?,
            ProtocolMessage::State { State } => map.serialize_entry("State", State)?,
            ProtocolMessage::Chat { Chat } => map.serialize_entry("Chat", Chat)?,
            ProtocolMessage::Error { Error } => map.serialize_entry("Error", Error)?,
            ProtocolMessage::TLS { TLS } => map.serialize_entry("TLS", TLS)?,
            ProtocolMessage::List { List } => map.serialize_entry("List", List)?,
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ProtocolMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let obj = value
            .as_object()
            .ok_or_else(|| de::Error::custom("Protocol message must be a JSON object"))?;

        if obj.len() != 1 {
            return Err(de::Error::custom(
                "Protocol message must contain exactly one top-level key",
            ));
        }

        let (key, val) = obj.iter().next().unwrap();
        match key.as_str() {
            "Hello" => {
                let message = serde_json::from_value(val.clone()).map_err(de::Error::custom)?;
                Ok(ProtocolMessage::Hello { Hello: message })
            }
            "Set" => {
                let message = serde_json::from_value(val.clone()).map_err(de::Error::custom)?;
                Ok(ProtocolMessage::Set { Set: message })
            }
            "State" => {
                let message = serde_json::from_value(val.clone()).map_err(de::Error::custom)?;
                Ok(ProtocolMessage::State { State: message })
            }
            "Chat" => {
                let message = serde_json::from_value(val.clone()).map_err(de::Error::custom)?;
                Ok(ProtocolMessage::Chat { Chat: message })
            }
            "Error" => {
                let message = serde_json::from_value(val.clone()).map_err(de::Error::custom)?;
                Ok(ProtocolMessage::Error { Error: message })
            }
            "TLS" => {
                let message = serde_json::from_value(val.clone()).map_err(de::Error::custom)?;
                Ok(ProtocolMessage::TLS { TLS: message })
            }
            "List" => {
                if val.is_null() {
                    Ok(ProtocolMessage::List { List: None })
                } else {
                    let list = serde_json::from_value(val.clone()).map_err(de::Error::custom)?;
                    Ok(ProtocolMessage::List { List: Some(list) })
                }
            }
            _ => Err(de::Error::custom(format!(
                "Unknown protocol message: {}",
                key
            ))),
        }
    }
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
#[serde(rename_all = "camelCase")]
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
    pub features: Option<Value>,
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
#[serde(rename_all = "camelCase")]
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
    pub features: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joined: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<bool>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manually_initiated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistIndexUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistChange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
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
    pub client_rtt: Option<f64>,
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
pub struct TLSMessage {
    #[serde(rename = "startTLS", skip_serializing_if = "Option::is_none")]
    pub start_tls: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_set_ready_null() {
        let json =
            r#"{"Set":{"ready":{"username":"pc","isReady":null,"manuallyInitiated":false}}}"#;
        let message: ProtocolMessage = serde_json::from_str(json).unwrap();
        match message {
            ProtocolMessage::Set { Set } => {
                let ready = Set.ready.expect("ready state missing");
                assert_eq!(ready.username.as_deref(), Some("pc"));
                assert_eq!(ready.is_ready, None);
                assert_eq!(ready.manually_initiated, Some(false));
            }
            _ => panic!("Unexpected message type"),
        }
    }

    #[test]
    fn test_deserialize_playlist_index_null() {
        let json = r#"{"Set":{"playlistIndex":{"user":"AS1","index":null}}}"#;
        let message: ProtocolMessage = serde_json::from_str(json).unwrap();
        match message {
            ProtocolMessage::Set { Set } => {
                let index = Set.playlist_index.expect("playlist index missing");
                assert_eq!(index.user.as_deref(), Some("AS1"));
                assert_eq!(index.index, None);
            }
            _ => panic!("Unexpected message type"),
        }
    }

    #[test]
    fn test_deserialize_user_event_left() {
        let json = r#"{"Set":{"user":{"pc":{"room":{"name":"default"},"event":{"left":true}}}}}"#;
        let message: ProtocolMessage = serde_json::from_str(json).unwrap();
        match message {
            ProtocolMessage::Set { Set } => {
                let users = Set.user.expect("user update missing");
                let update = users.get("pc").expect("pc update missing");
                let event = update.event.as_ref().expect("event missing");
                assert_eq!(event.left, Some(true));
            }
            _ => panic!("Unexpected message type"),
        }
    }
}
