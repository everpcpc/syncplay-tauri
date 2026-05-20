use super::properties::PlayerState;
use async_trait::async_trait;
#[cfg(test)]
use parking_lot::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::Arc;

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
    fn set_features(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn mark_reset(&self, _is_stream: bool) {}
    fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> anyhow::Result<()>;
    fn show_chat_message(&self, _username: Option<&str>, _message: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn is_connected(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub enum FakePlayerCommand {
    PollState,
    SetPosition(f64),
    SetPaused(bool),
    SetSpeed(f64),
    LoadFile(String),
    SetFeatures,
    MarkReset(bool),
    ShowOsd(String, Option<u64>),
    ShowChatMessage(Option<String>, String),
    Shutdown,
}

#[cfg(test)]
#[derive(Debug)]
struct FakePlayerInner {
    kind: PlayerKind,
    state: PlayerState,
    commands: Vec<FakePlayerCommand>,
    shutdown_count: usize,
    connected: bool,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct FakePlayerBackend {
    inner: Arc<Mutex<FakePlayerInner>>,
}

#[cfg(test)]
impl FakePlayerBackend {
    pub fn new(kind: PlayerKind) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakePlayerInner {
                kind,
                state: PlayerState::default(),
                commands: Vec::new(),
                shutdown_count: 0,
                connected: true,
            })),
        }
    }

    pub fn with_state(kind: PlayerKind, state: PlayerState) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakePlayerInner {
                kind,
                state,
                commands: Vec::new(),
                shutdown_count: 0,
                connected: true,
            })),
        }
    }

    pub fn commands(&self) -> Vec<FakePlayerCommand> {
        self.inner.lock().commands.clone()
    }

    pub fn shutdown_count(&self) -> usize {
        self.inner.lock().shutdown_count
    }

    pub fn set_fake_state(&self, state: PlayerState) {
        self.inner.lock().state = state;
    }

    pub fn set_connected(&self, connected: bool) {
        self.inner.lock().connected = connected;
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub struct FakePlayerFactory {
    launches: AtomicUsize,
    players: Mutex<Vec<FakePlayerBackend>>,
}

#[cfg(test)]
impl FakePlayerFactory {
    pub fn launch(&self, kind: PlayerKind) -> FakePlayerBackend {
        self.launches.fetch_add(1, Ordering::SeqCst);
        let player = FakePlayerBackend::new(kind);
        self.players.lock().push(player.clone());
        player
    }

    pub fn launch_count(&self) -> usize {
        self.launches.load(Ordering::SeqCst)
    }

    pub fn players(&self) -> Vec<FakePlayerBackend> {
        self.players.lock().clone()
    }
}

#[cfg(test)]
#[async_trait]
impl PlayerBackend for FakePlayerBackend {
    fn kind(&self) -> PlayerKind {
        self.inner.lock().kind
    }

    fn name(&self) -> &'static str {
        "FakePlayer"
    }

    fn get_state(&self) -> PlayerState {
        self.inner.lock().state.clone()
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        self.inner
            .lock()
            .commands
            .push(FakePlayerCommand::PollState);
        Ok(())
    }

    async fn set_position(&self, position: f64) -> anyhow::Result<()> {
        let mut inner = self.inner.lock();
        inner.state.position = Some(position);
        inner
            .commands
            .push(FakePlayerCommand::SetPosition(position));
        Ok(())
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        let mut inner = self.inner.lock();
        inner.state.paused = Some(paused);
        inner.commands.push(FakePlayerCommand::SetPaused(paused));
        Ok(())
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        let mut inner = self.inner.lock();
        inner.state.speed = Some(speed);
        inner.commands.push(FakePlayerCommand::SetSpeed(speed));
        Ok(())
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock();
        inner.state.path = Some(path.to_string());
        inner.state.filename = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_string())
            .or_else(|| Some(path.to_string()));
        inner
            .commands
            .push(FakePlayerCommand::LoadFile(path.to_string()));
        Ok(())
    }

    fn set_features(&self) -> anyhow::Result<()> {
        self.inner
            .lock()
            .commands
            .push(FakePlayerCommand::SetFeatures);
        Ok(())
    }

    fn mark_reset(&self, is_stream: bool) {
        self.inner
            .lock()
            .commands
            .push(FakePlayerCommand::MarkReset(is_stream));
    }

    fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> anyhow::Result<()> {
        self.inner
            .lock()
            .commands
            .push(FakePlayerCommand::ShowOsd(text.to_string(), duration_ms));
        Ok(())
    }

    fn show_chat_message(&self, username: Option<&str>, message: &str) -> anyhow::Result<()> {
        self.inner
            .lock()
            .commands
            .push(FakePlayerCommand::ShowChatMessage(
                username.map(|value| value.to_string()),
                message.to_string(),
            ));
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let mut inner = self.inner.lock();
        inner.shutdown_count += 1;
        inner.connected = false;
        inner.commands.push(FakePlayerCommand::Shutdown);
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.inner.lock().connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_player_records_commands_without_process() {
        let player = FakePlayerBackend::new(PlayerKind::Mpv);

        player.set_paused(true).await.unwrap();
        player.set_position(42.5).await.unwrap();
        player.set_speed(1.25).await.unwrap();
        player.load_file("/tmp/example.mkv").await.unwrap();
        player.show_osd("hello", Some(750)).unwrap();
        player.shutdown().await.unwrap();

        assert_eq!(player.kind(), PlayerKind::Mpv);
        assert_eq!(player.shutdown_count(), 1);
        assert_eq!(player.get_state().paused, Some(true));
        assert_eq!(player.get_state().position, Some(42.5));
        assert_eq!(player.get_state().speed, Some(1.25));
        assert_eq!(player.get_state().filename.as_deref(), Some("example.mkv"));
        assert_eq!(
            player.commands(),
            vec![
                FakePlayerCommand::SetPaused(true),
                FakePlayerCommand::SetPosition(42.5),
                FakePlayerCommand::SetSpeed(1.25),
                FakePlayerCommand::LoadFile("/tmp/example.mkv".to_string()),
                FakePlayerCommand::ShowOsd("hello".to_string(), Some(750)),
                FakePlayerCommand::Shutdown,
            ]
        );
    }
}
