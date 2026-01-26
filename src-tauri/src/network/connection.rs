use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

use super::messages::ProtocolMessage;
use super::protocol::SyncplayCodec;

/// Connection state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Authenticated,
}

/// Connection manager for Syncplay protocol
pub struct Connection {
    state: ConnectionState,
    host: String,
    port: u16,
    tx: Option<mpsc::UnboundedSender<ProtocolMessage>>,
}

impl Connection {
    pub fn new() -> Self {
        Self {
            state: ConnectionState::Disconnected,
            host: String::new(),
            port: 0,
            tx: None,
        }
    }

    pub fn state(&self) -> ConnectionState {
        self.state.clone()
    }

    /// Connect to a Syncplay server
    pub async fn connect(
        &mut self,
        host: String,
        port: u16,
    ) -> Result<mpsc::UnboundedReceiver<ProtocolMessage>> {
        info!("Connecting to {}:{}", host, port);
        self.state = ConnectionState::Connecting;
        self.host = host.clone();
        self.port = port;

        // Connect TCP stream
        let stream = TcpStream::connect(format!("{}:{}", host, port))
            .await
            .context("Failed to connect to server")?;

        info!("TCP connection established");
        self.state = ConnectionState::Connected;

        // Create framed stream with codec
        let framed = Framed::new(stream, SyncplayCodec::new());
        let (mut sink, mut stream) = framed.split();

        // Create channels for bidirectional communication
        let (tx, mut rx) = mpsc::unbounded_channel::<ProtocolMessage>();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel::<ProtocolMessage>();

        self.tx = Some(tx);

        // Spawn task to send messages
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = sink.send(msg).await {
                    error!("Failed to send message: {}", e);
                    break;
                }
            }
            debug!("Send task terminated");
        });

        // Spawn task to receive messages
        tokio::spawn(async move {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(msg) => {
                        if msg_tx.send(msg).is_err() {
                            warn!("Failed to forward received message");
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Failed to receive message: {}", e);
                        break;
                    }
                }
            }
            debug!("Receive task terminated");
        });

        Ok(msg_rx)
    }

    /// Send a message to the server
    pub fn send(&self, message: ProtocolMessage) -> Result<()> {
        if let Some(tx) = &self.tx {
            tx.send(message)
                .context("Failed to send message to connection")?;
            Ok(())
        } else {
            anyhow::bail!("Not connected");
        }
    }

    /// Disconnect from the server
    pub fn disconnect(&mut self) {
        info!("Disconnecting from server");
        self.tx = None;
        self.state = ConnectionState::Disconnected;
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        matches!(
            self.state,
            ConnectionState::Connected | ConnectionState::Authenticated
        )
    }

    /// Mark as authenticated
    pub fn set_authenticated(&mut self) {
        self.state = ConnectionState::Authenticated;
    }
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}
