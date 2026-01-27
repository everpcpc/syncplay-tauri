use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant};
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

use super::messages::ProtocolMessage;
use super::protocol::SyncplayCodec;
use super::tls::upgrade_to_tls;

/// Connection state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Authenticated,
}

enum ConnectionCommand {
    Send(ProtocolMessage),
    UpgradeTls {
        domain: String,
        response: oneshot::Sender<Result<()>>,
    },
    Disconnect,
}

enum Transport {
    Plain(Framed<TcpStream, SyncplayCodec>),
    Tls(Framed<tokio_rustls::client::TlsStream<TcpStream>, SyncplayCodec>),
    Empty,
}

impl Transport {
    async fn send(&mut self, message: ProtocolMessage) -> Result<()> {
        match self {
            Transport::Plain(framed) => framed.send(message).await?,
            Transport::Tls(framed) => framed.send(message).await?,
            Transport::Empty => anyhow::bail!("Transport not initialized"),
        }
        Ok(())
    }

    async fn next_message(&mut self) -> Option<Result<ProtocolMessage>> {
        match self {
            Transport::Plain(framed) => framed.next().await.map(|r| r.map_err(|e| e.into())),
            Transport::Tls(framed) => framed.next().await.map(|r| r.map_err(|e| e.into())),
            Transport::Empty => None,
        }
    }

    async fn upgrade_tls(&mut self, domain: &str) -> Result<()> {
        match std::mem::replace(self, Transport::Empty) {
            Transport::Plain(framed) => {
                let stream = framed.into_inner();
                let tls_stream = upgrade_to_tls(stream, domain).await?;
                *self = Transport::Tls(Framed::new(tls_stream, SyncplayCodec::new()));
                Ok(())
            }
            Transport::Tls(framed) => {
                *self = Transport::Tls(framed);
                Ok(())
            }
            Transport::Empty => anyhow::bail!("Transport not initialized"),
        }
    }
}

/// Connection manager for Syncplay protocol
pub struct Connection {
    state: Mutex<ConnectionState>,
    host: Mutex<String>,
    port: Mutex<u16>,
    tx: Mutex<Option<mpsc::UnboundedSender<ConnectionCommand>>>,
}

impl Connection {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ConnectionState::Disconnected),
            host: Mutex::new(String::new()),
            port: Mutex::new(0),
            tx: Mutex::new(None),
        }
    }

    pub fn state(&self) -> ConnectionState {
        self.state.lock().clone()
    }

    /// Connect to a Syncplay server
    pub async fn connect(
        &self,
        host: String,
        port: u16,
    ) -> Result<mpsc::UnboundedReceiver<ProtocolMessage>> {
        info!("Connecting to {}:{}", host, port);
        *self.state.lock() = ConnectionState::Connecting;
        *self.host.lock() = host.clone();
        *self.port.lock() = port;

        // Connect TCP stream
        let stream = TcpStream::connect(format!("{}:{}", host, port))
            .await
            .context("Failed to connect to server")?;

        info!("TCP connection established");
        *self.state.lock() = ConnectionState::Connected;

        // Create framed stream with codec
        let framed = Framed::new(stream, SyncplayCodec::new());
        let mut transport = Transport::Plain(framed);

        // Create channels for bidirectional communication
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ConnectionCommand>();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel::<ProtocolMessage>();

        *self.tx.lock() = Some(cmd_tx);

        tokio::spawn(async move {
            info!("Connection loop started");
            let mut idle_tick = tokio::time::interval(Duration::from_secs(10));
            let mut last_received = Instant::now();
            loop {
                tokio::select! {
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            ConnectionCommand::Send(msg) => {
                                if let Err(e) = transport.send(msg).await {
                                    error!("Failed to send message: {}", e);
                                    break;
                                }
                            }
                            ConnectionCommand::UpgradeTls { domain, response } => {
                                let result = transport.upgrade_tls(&domain).await;
                                let _ = response.send(result);
                            }
                            ConnectionCommand::Disconnect => {
                                break;
                            }
                        }
                    }
                    message = transport.next_message() => {
                        match message {
                            Some(Ok(msg)) => {
                                last_received = Instant::now();
                                if msg_tx.send(msg).is_err() {
                                    warn!("Failed to forward received message");
                                    break;
                                }
                            }
                            Some(Err(e)) => {
                                error!("Failed to receive message: {}", e);
                                break;
                            }
                            None => {
                                debug!("Receive task terminated");
                                break;
                            }
                        }
                    }
                    _ = idle_tick.tick() => {
                        let idle = last_received.elapsed().as_secs();
                        if idle >= 10 {
                            debug!("No server messages received for {}s", idle);
                        }
                    }
                }
            }
        });

        Ok(msg_rx)
    }

    /// Send a message to the server
    pub fn send(&self, message: ProtocolMessage) -> Result<()> {
        if let Some(tx) = self.tx.lock().as_ref() {
            tx.send(ConnectionCommand::Send(message))
                .context("Failed to send message to connection")?;
            Ok(())
        } else {
            anyhow::bail!("Not connected");
        }
    }

    /// Upgrade connection to TLS
    pub async fn upgrade_tls(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        let domain = self.host.lock().clone();
        if let Some(cmd_tx) = self.tx.lock().as_ref() {
            cmd_tx
                .send(ConnectionCommand::UpgradeTls {
                    domain,
                    response: tx,
                })
                .context("Failed to send upgrade TLS command")?;
        } else {
            anyhow::bail!("Not connected");
        }

        rx.await.context("TLS upgrade response dropped")?
    }

    /// Disconnect from the server
    pub fn disconnect(&self) {
        info!("Disconnecting from server");
        if let Some(tx) = self.tx.lock().as_ref() {
            let _ = tx.send(ConnectionCommand::Disconnect);
        }
        *self.tx.lock() = None;
        *self.state.lock() = ConnectionState::Disconnected;
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        matches!(
            *self.state.lock(),
            ConnectionState::Connected | ConnectionState::Authenticated
        )
    }

    /// Mark as authenticated
    pub fn set_authenticated(&self) {
        *self.state.lock() = ConnectionState::Authenticated;
    }
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}
