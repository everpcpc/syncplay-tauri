use async_trait::async_trait;
use super::properties::PlayerState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerKind {
    Mpv,
    MpvNet,
    Vlc,
    Iina,
    Mplayer,
    MpcHc,
    MpcBe,
    Unknown,
}

impl PlayerKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Mpv => "MPV",
            Self::MpvNet => "mpv.net",
            Self::Vlc => "VLC",
            Self::Iina => "IINA",
            Self::Mplayer => "MPlayer",
            Self::MpcHc => "MPC-HC",
            Self::MpcBe => "MPC-BE",
            Self::Unknown => "Unknown",
        }
    }
}

pub fn player_kind_from_path(path: &str) -> PlayerKind {
    let lower = path.to_ascii_lowercase();
    if lower.contains("mpvnet") || lower.contains("mpv.net") {
        PlayerKind::MpvNet
    } else if lower.contains("mpv") {
        PlayerKind::Mpv
    } else if lower.contains("vlc") {
        PlayerKind::Vlc
    } else if lower.contains("iina") {
        PlayerKind::Iina
    } else if lower.contains("mpc-hc") || lower.contains("mpchc") || lower.contains("shoukaku") {
        PlayerKind::MpcHc
    } else if lower.contains("mpc-be") {
        PlayerKind::MpcBe
    } else if lower.contains("mplayer") {
        PlayerKind::Mplayer
    } else {
        PlayerKind::Unknown
    }
}

pub fn player_kind_from_path_or_default(path: &str) -> PlayerKind {
    if path.trim().is_empty() {
        return PlayerKind::Mpv;
    }
    player_kind_from_path(path)
}

pub fn default_player_path_for_kind(kind: PlayerKind) -> &'static str {
    match kind {
        PlayerKind::Mpv | PlayerKind::MpvNet | PlayerKind::Iina => "mpv",
        PlayerKind::Vlc => "vlc",
        PlayerKind::Mplayer => "mplayer",
        PlayerKind::MpcHc => "mpc-hc",
        PlayerKind::MpcBe => "mpc-be",
        PlayerKind::Unknown => "mpv",
    }
}

#[async_trait]
pub trait PlayerBackend: Send + Sync {
    fn kind(&self) -> PlayerKind;
    fn name(&self) -> &'static str;
    fn get_state(&self) -> PlayerState;
    async fn poll_state(&self) -> anyhow::Result<()>;
    async fn set_position(&self, position: f64) -> anyhow::Result<()>;
    async fn set_paused(&self, paused: bool) -> anyhow::Result<()>;
    async fn set_speed(&self, speed: f64) -> anyhow::Result<()>;
    async fn load_file(&self, path: &str) -> anyhow::Result<()>;
    fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> anyhow::Result<()>;
}
