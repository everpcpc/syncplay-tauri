use anyhow::{Context, Result};
use bytes::BytesMut;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::VecDeque;
use std::fmt;
use tokio_util::codec::{Decoder, Encoder, LinesCodec};

use super::messages::ProtocolMessage;

/// Syncplay JSON protocol codec
/// Messages are newline-delimited JSON
pub struct SyncplayCodec {
    lines_codec: LinesCodec,
    pending: VecDeque<std::result::Result<ProtocolMessage, String>>,
}

impl SyncplayCodec {
    pub fn new() -> Self {
        Self {
            lines_codec: LinesCodec::new(),
            pending: VecDeque::new(),
        }
    }
}

struct ProtocolEnvelope(Vec<std::result::Result<ProtocolMessage, String>>);

struct SetEnvelope(Vec<std::result::Result<ProtocolMessage, String>>);

impl<'de> Deserialize<'de> for SetEnvelope {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SetVisitor;

        impl<'de> Visitor<'de> for SetVisitor {
            type Value = SetEnvelope;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Syncplay Set command object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut messages = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    let mut command = serde_json::Map::new();
                    command.insert(key, value);
                    messages.push(ProtocolMessage::from_command("Set", Value::Object(command)));
                }
                if messages.is_empty() {
                    messages.push(ProtocolMessage::from_command(
                        "Set",
                        Value::Object(serde_json::Map::new()),
                    ));
                }
                Ok(SetEnvelope(messages))
            }
        }

        deserializer.deserialize_map(SetVisitor)
    }
}

impl<'de> Deserialize<'de> for ProtocolEnvelope {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EnvelopeVisitor;

        impl<'de> Visitor<'de> for EnvelopeVisitor {
            type Value = ProtocolEnvelope;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Syncplay protocol command object")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut messages = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "Set" {
                        let SetEnvelope(set_messages) = map.next_value()?;
                        messages.extend(set_messages);
                    } else {
                        let value = map.next_value::<Value>()?;
                        messages.push(ProtocolMessage::from_command(&key, value));
                    }
                }
                Ok(ProtocolEnvelope(messages))
            }
        }

        deserializer.deserialize_map(EnvelopeVisitor)
    }
}

impl Default for SyncplayCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for SyncplayCodec {
    type Item = ProtocolMessage;
    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
        if let Some(message) = self.pending.pop_front() {
            return message.map(Some).map_err(anyhow::Error::msg);
        }

        // Decode line
        let line = match self.lines_codec.decode(src)? {
            Some(line) => line,
            None => return Ok(None),
        };

        // Skip empty lines
        if line.trim().is_empty() {
            return Ok(None);
        }

        // Parse JSON
        let ProtocolEnvelope(messages) =
            serde_json::from_str(&line).context("Failed to parse protocol message")?;
        self.pending.extend(messages);

        tracing::debug!("Received: {}", line);
        match self.pending.pop_front() {
            Some(message) => message.map(Some).map_err(anyhow::Error::msg),
            None => Ok(None),
        }
    }
}

impl Encoder<ProtocolMessage> for SyncplayCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, item: ProtocolMessage, dst: &mut BytesMut) -> Result<()> {
        // Serialize to JSON
        let json = serde_json::to_string(&item).context("Failed to serialize protocol message")?;

        tracing::debug!("Sending: {}", json);

        // Encode line with CRLF delimiter (Syncplay server expects \r\n)
        dst.extend_from_slice(json.as_bytes());
        dst.extend_from_slice(b"\r\n");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::messages::*;

    #[test]
    fn test_hello_message_serialization() {
        let client_features = ClientFeatures {
            shared_playlists: Some(true),
            chat: Some(true),
            readiness: Some(true),
            managed_rooms: Some(true),
            persistent_rooms: Some(true),
            feature_list: Some(true),
            set_others_readiness: Some(true),
            ui_mode: Some("GUI".to_string()),
        };
        let hello = ProtocolMessage::Hello {
            Hello: HelloMessage {
                username: "testuser".to_string(),
                password: None,
                room: Some(RoomInfo {
                    name: "testroom".to_string(),
                    password: None,
                }),
                version: "1.2.255".to_string(),
                realversion: "1.7.5".to_string(),
                features: serde_json::to_value(client_features).ok(),
                motd: None,
            },
        };

        let json = serde_json::to_string(&hello).unwrap();
        println!("Serialized: {}", json);

        let parsed: ProtocolMessage = serde_json::from_str(&json).unwrap();
        println!("Parsed: {:?}", parsed);
    }

    #[test]
    fn decodes_multiple_commands_in_wire_order() {
        let mut codec = SyncplayCodec::new();
        let mut bytes = BytesMut::from(
            &b"{\"Chat\":{\"username\":\"alice\",\"message\":\"first\"},\"List\":null}\n"[..],
        );

        let first = codec.decode(&mut bytes).unwrap().unwrap();
        let second = codec.decode(&mut bytes).unwrap().unwrap();

        assert!(matches!(
            first,
            ProtocolMessage::Chat {
                Chat: ChatMessage::Entry { ref username, ref message }
            } if username == "alice" && message == "first"
        ));
        assert!(matches!(second, ProtocolMessage::List { List: None }));
        assert!(codec.decode(&mut bytes).unwrap().is_none());
    }

    #[test]
    fn processes_commands_before_a_later_unknown_command() {
        let mut codec = SyncplayCodec::new();
        let mut bytes = BytesMut::from(&b"{\"List\":null,\"Unknown\":{\"value\":1}}\n"[..]);

        assert!(matches!(
            codec.decode(&mut bytes).unwrap().unwrap(),
            ProtocolMessage::List { List: None }
        ));
        let error = codec.decode(&mut bytes).unwrap_err();
        assert!(error
            .to_string()
            .contains("Unknown protocol message: Unknown"));
    }

    #[test]
    fn dispatches_playlist_index_before_playlist_change_in_set_wire_order() {
        let mut codec = SyncplayCodec::new();
        let mut bytes = BytesMut::from(
            &b"{\"Set\":{\"playlistIndex\":{\"user\":\"alice\",\"index\":1},\"playlistChange\":{\"user\":\"alice\",\"files\":[\"one.mkv\",\"two.mkv\"]}}}\n"[..],
        );

        let first = codec.decode(&mut bytes).unwrap().unwrap();
        let second = codec.decode(&mut bytes).unwrap().unwrap();

        assert!(matches!(
            first,
            ProtocolMessage::Set { Set }
                if Set.playlist_index.as_ref().and_then(|update| update.index) == Some(1)
                    && Set.playlist_change.is_none()
        ));
        assert!(matches!(
            second,
            ProtocolMessage::Set { Set }
                if Set.playlist_index.is_none()
                    && Set.playlist_change.as_ref().map(|change| change.files.as_slice())
                        == Some(["one.mkv".to_string(), "two.mkv".to_string()].as_slice())
        ));
    }

    #[test]
    fn dispatches_playlist_change_before_playlist_index_in_set_wire_order() {
        let mut codec = SyncplayCodec::new();
        let mut bytes = BytesMut::from(
            &b"{\"Set\":{\"playlistChange\":{\"user\":\"alice\",\"files\":[\"one.mkv\",\"two.mkv\"]},\"playlistIndex\":{\"user\":\"alice\",\"index\":1}}}\n"[..],
        );

        let first = codec.decode(&mut bytes).unwrap().unwrap();
        let second = codec.decode(&mut bytes).unwrap().unwrap();

        assert!(matches!(
            first,
            ProtocolMessage::Set { Set }
                if Set.playlist_change.is_some() && Set.playlist_index.is_none()
        ));
        assert!(matches!(
            second,
            ProtocolMessage::Set { Set }
                if Set.playlist_change.is_none()
                    && Set.playlist_index.as_ref().and_then(|update| update.index) == Some(1)
        ));
    }
}
