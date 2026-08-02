/// MPV events that we care about
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MpvPlayerEvent {
    /// MPV began processing a playlist entry.
    StartFile { playlist_entry_id: Option<i64> },
    /// File has been loaded
    FileLoaded { playlist_entry_id: Option<i64> },
    /// Playback has started
    PlaybackRestart,
    /// Playback has ended
    EndFile {
        reason: EndFileReason,
        playlist_entry_id: Option<i64>,
    },
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
    pub fn from_event_name(
        name: &str,
        reason: Option<&str>,
        playlist_entry_id: Option<i64>,
    ) -> Self {
        match name {
            "start-file" => Self::StartFile { playlist_entry_id },
            "file-loaded" => Self::FileLoaded { playlist_entry_id },
            "playback-restart" => Self::PlaybackRestart,
            "end-file" => {
                let end_reason = reason
                    .map(EndFileReason::from_str)
                    .unwrap_or(EndFileReason::Unknown("none".to_string()));
                Self::EndFile {
                    reason: end_reason,
                    playlist_entry_id,
                }
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
