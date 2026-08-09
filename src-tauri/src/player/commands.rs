use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// MPV JSON IPC command
#[derive(Debug, Clone, Serialize)]
pub struct MpvCommand {
    pub command: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
    #[serde(skip)]
    pub load_id: Option<u64>,
}

/// MPV JSON IPC response
#[derive(Debug, Clone, Deserialize)]
pub struct MpvResponse {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub request_id: Option<u64>,
}

/// MPV event
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct MpvEvent {
    pub event: String,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub playlist_entry_id: Option<i64>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
}

/// MPV message (either response or event)
#[derive(Debug, Clone)]
pub enum MpvMessage {
    Response(MpvResponse),
    Event(MpvEvent),
}

impl<'de> Deserialize<'de> for MpvMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        // MPV replies are identified by request_id. Check it before `event` to
        // match the reference IPC client and avoid treating a reply containing
        // event-shaped extension data as an asynchronous notification.
        if value.get("request_id").is_some() {
            serde_json::from_value(value)
                .map(Self::Response)
                .map_err(serde::de::Error::custom)
        } else if value.get("event").is_some() {
            serde_json::from_value(value)
                .map(Self::Event)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Self::Response)
                .map_err(serde::de::Error::custom)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadfileOptionsSyntax {
    Legacy,
    Modern,
}

#[allow(dead_code)]
impl MpvCommand {
    /// Create a get_property command
    pub fn get_property(property: &str, request_id: u64) -> Self {
        Self {
            command: vec![
                Value::String("get_property".to_string()),
                Value::String(property.to_string()),
            ],
            request_id: Some(request_id),
            load_id: None,
        }
    }

    /// Create a set_property command
    pub fn set_property(property: &str, value: Value, request_id: u64) -> Self {
        Self {
            command: vec![
                Value::String("set_property".to_string()),
                Value::String(property.to_string()),
                value,
            ],
            request_id: Some(request_id),
            load_id: None,
        }
    }

    /// Create a set_property command without a request id.
    pub fn set_property_no_reply(property: &str, value: Value) -> Self {
        Self {
            command: vec![
                Value::String("set_property".to_string()),
                Value::String(property.to_string()),
                value,
            ],
            request_id: None,
            load_id: None,
        }
    }

    pub fn observe_property(id: u64, property: &str) -> Self {
        Self {
            command: vec![
                Value::String("observe_property".to_string()),
                Value::Number(id.into()),
                Value::String(property.to_string()),
            ],
            request_id: None,
            load_id: None,
        }
    }

    /// Create an unobserve_property command
    pub fn unobserve_property(id: u64) -> Self {
        Self {
            command: vec![
                Value::String("unobserve_property".to_string()),
                Value::Number(id.into()),
            ],
            request_id: None,
            load_id: None,
        }
    }

    /// Create a loadfile command
    pub fn loadfile(path: &str, mode: &str, request_id: u64) -> Self {
        Self {
            command: vec![
                Value::String("loadfile".to_string()),
                Value::String(path.to_string()),
                Value::String(mode.to_string()),
            ],
            request_id: Some(request_id),
            load_id: None,
        }
    }

    pub fn loadfile_no_reply(path: &str, mode: &str) -> Self {
        Self {
            command: vec![
                Value::String("loadfile".to_string()),
                Value::String(path.to_string()),
                Value::String(mode.to_string()),
            ],
            request_id: None,
            load_id: None,
        }
    }

    /// Create a seek command
    pub fn seek(position: f64, mode: &str, request_id: u64) -> Self {
        Self {
            command: vec![
                Value::String("seek".to_string()),
                Value::Number(serde_json::Number::from_f64(position).unwrap()),
                Value::String(mode.to_string()),
            ],
            request_id: Some(request_id),
            load_id: None,
        }
    }

    /// Create a show_text command (OSD)
    pub fn show_text(text: &str, duration: Option<u64>) -> Self {
        let mut command = vec![
            Value::String("show_text".to_string()),
            Value::String(text.to_string()),
        ];
        if let Some(dur) = duration {
            command.push(Value::Number(dur.into()));
        }
        Self {
            command,
            request_id: None,
            load_id: None,
        }
    }

    /// Create a cycle command (for pause/unpause)
    pub fn cycle(property: &str, request_id: u64) -> Self {
        Self {
            command: vec![
                Value::String("cycle".to_string()),
                Value::String(property.to_string()),
            ],
            request_id: Some(request_id),
            load_id: None,
        }
    }

    /// Create a quit command
    pub fn quit() -> Self {
        Self {
            command: vec![Value::String("quit".to_string())],
            request_id: None,
            load_id: None,
        }
    }

    /// Create a script-message-to command
    pub fn script_message_to(target: &str, message: &str, args: Vec<Value>) -> Self {
        let mut command = vec![
            Value::String("script-message-to".to_string()),
            Value::String(target.to_string()),
            Value::String(message.to_string()),
        ];
        command.extend(args);
        Self {
            command,
            request_id: None,
            load_id: None,
        }
    }

    pub fn load_generation_via_script(
        path: &str,
        load_id: u64,
        syntax: Option<LoadfileOptionsSyntax>,
    ) -> Self {
        let syntax = match syntax {
            Some(LoadfileOptionsSyntax::Legacy) => "legacy",
            Some(LoadfileOptionsSyntax::Modern) => "modern",
            None => "auto",
        };
        let mut command = Self::script_message_to(
            "syncplayintf",
            "syncplay-load-file",
            vec![
                Value::String(load_id.to_string()),
                Value::String(path.to_string()),
                Value::String(syntax.to_string()),
            ],
        );
        command.load_id = Some(load_id);
        command
    }

    /// Request log messages from MPV
    pub fn request_log_messages(level: &str) -> Self {
        Self {
            command: vec![
                Value::String("request_log_messages".to_string()),
                Value::String(level.to_string()),
            ],
            request_id: None,
            load_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_message_is_deserialized_as_event() {
        let message: MpvMessage = serde_json::from_value(serde_json::json!({
            "event": "log-message",
            "prefix": "term-msg",
            "level": "info",
            "text": "ANS_filename=movie.mkv\n"
        }))
        .unwrap();

        let MpvMessage::Event(event) = message else {
            panic!("async MPV message was classified as a command response");
        };
        assert_eq!(event.event, "log-message");
        assert_eq!(event.text.as_deref(), Some("ANS_filename=movie.mkv\n"));
    }

    #[test]
    fn command_reply_is_deserialized_as_response() {
        let message: MpvMessage = serde_json::from_value(serde_json::json!({
            "data": "0.41.0",
            "request_id": 7,
            "error": "success"
        }))
        .unwrap();

        let MpvMessage::Response(response) = message else {
            panic!("MPV command reply was classified as an async event");
        };
        assert_eq!(response.request_id, Some(7));
        assert_eq!(response.error, "success");
    }

    #[test]
    fn request_id_takes_precedence_over_event_in_mixed_envelope() {
        let message: MpvMessage = serde_json::from_value(serde_json::json!({
            "event": "log-message",
            "request_id": 9,
            "error": "success",
            "data": "accepted"
        }))
        .unwrap();

        let MpvMessage::Response(response) = message else {
            panic!("request reply was classified as an asynchronous event");
        };
        assert_eq!(response.request_id, Some(9));
        assert_eq!(response.data, Some(Value::String("accepted".into())));
    }

    #[test]
    fn generation_load_targets_the_syncplay_lua_protocol() {
        let command = MpvCommand::load_generation_via_script(
            "movie with spaces.mkv",
            17,
            Some(LoadfileOptionsSyntax::Modern),
        );

        assert_eq!(
            command.command,
            vec![
                serde_json::json!("script-message-to"),
                serde_json::json!("syncplayintf"),
                serde_json::json!("syncplay-load-file"),
                serde_json::json!("17"),
                serde_json::json!("movie with spaces.mkv"),
                serde_json::json!("modern"),
            ]
        );
        assert_eq!(command.load_id, Some(17));
        assert_eq!(command.request_id, None);
    }
}
