/// MPV events that we care about
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MpvPlayerEvent {
    /// File has been loaded
    FileLoaded,
    /// Playback has started
    PlaybackRestart,
    /// Playback has ended
    EndFile { reason: EndFileReason },
    /// MPV reported it is quitting
    Quit,
    /// MPV IPC socket/stdout reader reached EOF or disconnected
    SocketDisconnected,
    /// Log message from MPV
    LogMessage(String),
    /// Seek operation completed
    SeekCompleted,
    /// Property changed (handled separately via property observation)
    PropertyChange,
    /// Unknown event
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndFileReason {
    /// Playback reached end of file
    Eof,
    /// Playback was stopped
    Stop,
    /// Playback was quit
    Quit,
    /// An error occurred
    Error,
    /// Redirected to another file
    Redirect,
    /// Unknown reason
    Unknown(String),
}

impl MpvPlayerEvent {
    pub fn from_event_name(name: &str, reason: Option<&str>) -> Self {
        match name {
            "file-loaded" => Self::FileLoaded,
            "playback-restart" => Self::PlaybackRestart,
            "end-file" => {
                let end_reason = reason
                    .map(EndFileReason::from_str)
                    .unwrap_or(EndFileReason::Unknown("none".to_string()));
                Self::EndFile { reason: end_reason }
            }
            "shutdown" | "quit" => Self::Quit,
            "seek" => Self::SeekCompleted,
            "property-change" => Self::PropertyChange,
            _ => Self::Unknown(name.to_string()),
        }
    }
}

impl EndFileReason {
    pub fn from_str(s: &str) -> Self {
        match s {
            "eof" => Self::Eof,
            "stop" => Self::Stop,
            "quit" => Self::Quit,
            "error" => Self::Error,
            "redirect" => Self::Redirect,
            _ => Self::Unknown(s.to_string()),
        }
    }
}
