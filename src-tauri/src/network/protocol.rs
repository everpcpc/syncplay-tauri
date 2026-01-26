use anyhow::{Context, Result};
use serde_json;
use tokio_util::codec::{Decoder, Encoder, LinesCodec};
use bytes::BytesMut;

use super::messages::ProtocolMessage;

/// Syncplay JSON protocol codec
/// Messages are newline-delimited JSON
pub struct SyncplayCodec {
    lines_codec: LinesCodec,
}

impl SyncplayCodec {
    pub fn new() -> Self {
        Self {
            lines_codec: LinesCodec::new(),
        }
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
        let message: ProtocolMessage = serde_json::from_str(&line)
            .context("Failed to parse protocol message")?;

        tracing::debug!("Received: {}", line);
        Ok(Some(message))
    }
}

impl Encoder<ProtocolMessage> for SyncplayCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, item: ProtocolMessage, dst: &mut BytesMut) -> Result<()> {
        // Serialize to JSON
        let json = serde_json::to_string(&item)
            .context("Failed to serialize protocol message")?;

        tracing::debug!("Sending: {}", json);

        // Encode line
        self.lines_codec.encode(json, dst)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::messages::*;

    #[test]
    fn test_hello_message_serialization() {
        let hello = ProtocolMessage::Hello {
            Hello: HelloMessage {
                username: "testuser".to_string(),
                password: None,
                room: Some(RoomInfo {
                    name: "testroom".to_string(),
                    password: None,
                }),
                version: "1.2.255".to_string(),
                realversion: "1.7.0".to_string(),
                features: Some(ClientFeatures {
                    shared_playlists: Some(true),
                    chat: Some(true),
                    ready_state: Some(true),
                    managed_rooms: Some(false),
                    persistent_rooms: Some(false),
                }),
                motd: None,
            },
        };

        let json = serde_json::to_string(&hello).unwrap();
        println!("Serialized: {}", json);

        let parsed: ProtocolMessage = serde_json::from_str(&json).unwrap();
        println!("Parsed: {:?}", parsed);
    }
}
