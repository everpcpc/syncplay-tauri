use std::sync::Arc;

use async_trait::async_trait;

use super::backend::{PlayerBackend, PlayerKind};
use super::mpv_ipc::MpvIpc;
use super::properties::PlayerState;

pub struct MpvBackend {
    kind: PlayerKind,
    ipc: Arc<MpvIpc>,
}

impl MpvBackend {
    pub fn new(kind: PlayerKind, ipc: MpvIpc) -> Self {
        Self {
            kind,
            ipc: Arc::new(ipc),
        }
    }

    pub fn ipc(&self) -> Arc<MpvIpc> {
        self.ipc.clone()
    }
}

#[async_trait]
impl PlayerBackend for MpvBackend {
    fn kind(&self) -> PlayerKind {
        self.kind
    }

    fn name(&self) -> &'static str {
        self.kind.display_name()
    }

    fn get_state(&self) -> PlayerState {
        self.ipc.get_state()
    }

    async fn poll_state(&self) -> anyhow::Result<()> {
        self.ipc.refresh_state().await
    }

    async fn set_position(&self, position: f64) -> anyhow::Result<()> {
        self.ipc.set_position(position).await
    }

    async fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        self.ipc.set_paused(paused).await
    }

    async fn set_speed(&self, speed: f64) -> anyhow::Result<()> {
        self.ipc.set_speed(speed).await
    }

    async fn load_file(&self, path: &str) -> anyhow::Result<()> {
        self.ipc.load_file(path).await
    }

    fn show_osd(&self, text: &str, duration_ms: Option<u64>) -> anyhow::Result<()> {
        self.ipc.show_osd(text, duration_ms)
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.ipc.quit()
    }
}
