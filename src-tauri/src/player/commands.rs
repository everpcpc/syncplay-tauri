use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MPV JSON IPC command
#[derive(Debug, Clone, Serialize)]
pub struct MpvCommand {
    pub command: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
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
}

/// MPV message (either response or event)
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MpvMessage {
    Response(MpvResponse),
    Event(MpvEvent),
}

impl MpvCommand {
    /// Create a get_property command
    pub fn get_property(property: &str, request_id: u64) -> Self {
        Self {
            command: vec![
                Value::String("get_property".to_string()),
                Value::String(property.to_string()),
            ],
            request_id: Some(request_id),
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
        }
    }

    /// Create an observe_property command
    pub fn observe_property(id: u64, property: &str) -> Self {
        Self {
            command: vec![
                Value::String("observe_property".to_string()),
                Value::Number(id.into()),
                Value::String(property.to_string()),
            ],
            request_id: None,
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
        }
    }

    /// Create a quit command
    pub fn quit() -> Self {
        Self {
            command: vec![Value::String("quit".to_string())],
            request_id: None,
        }
    }
}
