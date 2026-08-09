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
use super::tls::{upgrade_to_tls, TlsInfo};

const CONNECT_TIMEOUT_SECONDS: u64 = 30;

/// Connection state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Authenticated,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TerminalConnectionError {
    Protocol(String),
    TlsCertificate(String),
}

impl TerminalConnectionError {
    pub fn message(&self) -> &str {
        match self {
            Self::Protocol(message) | Self::TlsCertificate(message) => message,
        }
    }
}

enum ConnectionCommand {
    Send(Box<ProtocolMessage>),
    UpgradeTls {
        domain: String,
        timeout: Duration,
        response: oneshot::Sender<Result<TlsInfo>>,
    },
    #[cfg(test)]
    UpgradeTlsWithExtraRoots {
        domain: String,
        extra_roots: Vec<rustls::Certificate>,
        timeout: Duration,
        response: oneshot::Sender<Result<TlsInfo>>,
    },
    Disconnect,
}

enum Transport {
    Plain(Box<Framed<TcpStream, SyncplayCodec>>),
    Tls(Box<Framed<tokio_rustls::client::TlsStream<TcpStream>, SyncplayCodec>>),
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
            Transport::Plain(framed) => framed.next().await,
            Transport::Tls(framed) => framed.next().await,
            Transport::Empty => None,
        }
    }

    #[cfg(test)]
    async fn upgrade_tls_with_extra_roots(
        &mut self,
        domain: &str,
        extra_roots: Vec<rustls::Certificate>,
    ) -> Result<TlsInfo> {
        match std::mem::replace(self, Transport::Empty) {
            Transport::Plain(framed) => {
                let framed = *framed;
                let stream = framed.into_inner();
                let connector = super::tls::create_tls_connector_with_extra_roots(extra_roots)?;
                let domain = match domain.parse::<std::net::IpAddr>() {
                    Ok(ip) => rustls::ServerName::IpAddress(ip),
                    Err(_) => rustls::ServerName::try_from(domain)?,
                };
                let tls_stream = connector.connect(domain, stream).await?;
                let protocol =
                    tls_stream
                        .get_ref()
                        .1
                        .protocol_version()
                        .map(|version| match version {
                            rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2".to_string(),
                            rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3".to_string(),
                            other => format!("{:?}", other),
                        });
                *self = Transport::Tls(Box::new(Framed::new(tls_stream, SyncplayCodec::new())));
                Ok(TlsInfo { protocol })
            }
            Transport::Tls(framed) => {
                *self = Transport::Tls(framed);
                Ok(TlsInfo { protocol: None })
            }
            Transport::Empty => anyhow::bail!("Transport not initialized"),
        }
    }
    async fn upgrade_tls(&mut self, domain: &str) -> Result<TlsInfo> {
        match std::mem::replace(self, Transport::Empty) {
            Transport::Plain(framed) => {
                let framed = *framed;
                let stream = framed.into_inner();
                let (tls_stream, info) = upgrade_to_tls(stream, domain).await?;
                *self = Transport::Tls(Box::new(Framed::new(tls_stream, SyncplayCodec::new())));
                Ok(info)
            }
            Transport::Tls(framed) => {
                *self = Transport::Tls(framed);
                Ok(TlsInfo { protocol: None })
            }
            Transport::Empty => anyhow::bail!("Transport not initialized"),
        }
    }
}

/// Connection manager for Syncplay protocol
pub struct Connection {
    state: std::sync::Arc<Mutex<ConnectionState>>,
    host: Mutex<String>,
    port: Mutex<u16>,
    tx: std::sync::Arc<Mutex<Option<mpsc::UnboundedSender<ConnectionCommand>>>>,
    terminal_error: std::sync::Arc<Mutex<Option<TerminalConnectionError>>>,
}

impl Connection {
    pub fn new() -> Self {
        Self {
            state: std::sync::Arc::new(Mutex::new(ConnectionState::Disconnected)),
            host: Mutex::new(String::new()),
            port: Mutex::new(0),
            tx: std::sync::Arc::new(Mutex::new(None)),
            terminal_error: std::sync::Arc::new(Mutex::new(None)),
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
    ) -> Result<(mpsc::UnboundedReceiver<ProtocolMessage>, Option<String>)> {
        info!("Connecting to {}:{}", host, port);
        *self.state.lock() = ConnectionState::Connecting;
        *self.host.lock() = host.clone();
        *self.port.lock() = port;
        *self.terminal_error.lock() = None;

        // Connect TCP stream
        let address = format!("{}:{}", host, port);
        let stream = tokio::time::timeout(
            Duration::from_secs(CONNECT_TIMEOUT_SECONDS),
            TcpStream::connect(&address),
        )
        .await
        .context("Connection attempt timed out")?
        .context("Failed to connect to server")?;

        let peer_address = stream.peer_addr().ok().map(|addr| addr.ip().to_string());

        info!("TCP connection established");
        *self.state.lock() = ConnectionState::Connected;

        // Create framed stream with codec
        let framed = Framed::new(stream, SyncplayCodec::new());
        let mut transport = Transport::Plain(Box::new(framed));

        // Create channels for bidirectional communication
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ConnectionCommand>();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel::<ProtocolMessage>();

        *self.tx.lock() = Some(cmd_tx);

        let state_ref = self.state.clone();
        let tx_ref = self.tx.clone();
        let terminal_error_ref = self.terminal_error.clone();
        tokio::spawn(async move {
            info!("Connection loop started");
            let mut idle_tick = tokio::time::interval(Duration::from_secs(10));
            let mut last_received = Instant::now();
            loop {
                tokio::select! {
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            ConnectionCommand::Send(msg) => {
                                if let Err(e) = transport.send(*msg).await {
                                    error!("Failed to send message: {}", e);
                                    break;
                                }
                            }
                            ConnectionCommand::UpgradeTls { domain, timeout, response } => {
                                let result = tokio::time::timeout(timeout, transport.upgrade_tls(&domain)).await;
                                let timed_out = result.is_err();
                                let result = result
                                    .context("TLS upgrade timed out")
                                    .and_then(std::convert::identity);
                                let _ = response.send(result);
                                if timed_out {
                                    break;
                                }
                            }
                            #[cfg(test)]
                            ConnectionCommand::UpgradeTlsWithExtraRoots { domain, extra_roots, timeout, response } => {
                                let result = tokio::time::timeout(
                                    timeout,
                                    transport.upgrade_tls_with_extra_roots(&domain, extra_roots),
                                )
                                .await;
                                let timed_out = result.is_err();
                                let result = result
                                    .context("TLS upgrade timed out")
                                    .and_then(std::convert::identity);
                                let _ = response.send(result);
                                if timed_out {
                                    break;
                                }
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
                                *terminal_error_ref.lock() =
                                    Some(TerminalConnectionError::Protocol(e.to_string()));
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
            *tx_ref.lock() = None;
            *state_ref.lock() = ConnectionState::Disconnected;
        });

        Ok((msg_rx, peer_address))
    }

    /// Send a message to the server
    pub fn send(&self, message: ProtocolMessage) -> Result<()> {
        if let Some(tx) = self.tx.lock().as_ref() {
            tx.send(ConnectionCommand::Send(Box::new(message)))
                .context("Failed to send message to connection")?;
            Ok(())
        } else {
            anyhow::bail!("Not connected");
        }
    }

    #[cfg(not(test))]
    async fn upgrade_tls_with_timeout_and_extra_roots<I>(
        &self,
        timeout_duration: Duration,
        _extra_roots: I,
    ) -> Result<TlsInfo>
    where
        I: IntoIterator<Item = rustls::Certificate> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let domain = self.host.lock().clone();
        if let Some(cmd_tx) = self.tx.lock().as_ref() {
            cmd_tx
                .send(ConnectionCommand::UpgradeTls {
                    domain,
                    timeout: timeout_duration,
                    response: tx,
                })
                .context("Failed to send upgrade TLS command")?;
        } else {
            anyhow::bail!("Not connected");
        }

        rx.await.context("TLS upgrade response dropped")?
    }

    pub async fn upgrade_tls_with_timeout(&self, timeout_duration: Duration) -> Result<TlsInfo> {
        self.upgrade_tls_with_timeout_and_extra_roots(
            timeout_duration,
            std::iter::empty::<rustls::Certificate>(),
        )
        .await
    }

    #[cfg(test)]
    pub async fn upgrade_tls_with_timeout_and_extra_roots<I>(
        &self,
        timeout_duration: Duration,
        extra_roots: I,
    ) -> Result<TlsInfo>
    where
        I: IntoIterator<Item = rustls::Certificate> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let domain = self.host.lock().clone();
        if let Some(cmd_tx) = self.tx.lock().as_ref() {
            cmd_tx
                .send(ConnectionCommand::UpgradeTlsWithExtraRoots {
                    domain,
                    extra_roots: extra_roots.into_iter().collect(),
                    timeout: timeout_duration,
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

    pub fn take_terminal_error(&self) -> Option<TerminalConnectionError> {
        self.terminal_error.lock().take()
    }

    pub fn has_terminal_error(&self) -> bool {
        self.terminal_error.lock().is_some()
    }

    pub fn mark_protocol_error(&self, error: impl Into<String>) {
        *self.terminal_error.lock() = Some(TerminalConnectionError::Protocol(error.into()));
    }

    pub fn mark_tls_certificate_error(&self, error: impl Into<String>) {
        *self.terminal_error.lock() = Some(TerminalConnectionError::TlsCertificate(error.into()));
    }

    /// Mark as authenticated
    pub fn set_authenticated(&self) {
        *self.state.lock() = ConnectionState::Authenticated;
    }
}

#[cfg(test)]
mod tests {
    use super::{Connection, TerminalConnectionError};
    use crate::network::fake_server::tls_fixture::FakeTlsSyncplayServer;
    use crate::network::fake_server::FakeSyncplayServer;
    use crate::network::messages::{HelloMessage, ProtocolMessage, RoomInfo};
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn tls_upgrade_timeout_aborts_worker_and_closes_receiver() {
        let server = FakeSyncplayServer::start().await.unwrap();
        let connection = Connection::new();
        let (mut receiver, _) = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();

        let result = timeout(
            Duration::from_secs(2),
            connection.upgrade_tls_with_timeout(Duration::from_millis(25)),
        )
        .await
        .expect("TLS timeout must settle")
        .unwrap_err();

        assert!(result.to_string().contains("timed out"));
        assert!(timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("connection worker must close its receiver")
            .is_none());
        assert_eq!(connection.state(), super::ConnectionState::Disconnected);
        server.close();
    }

    #[tokio::test]
    async fn malformed_server_command_records_terminal_protocol_error() {
        let server = FakeSyncplayServer::start().await.unwrap();
        let connection = Connection::new();
        let (mut receiver, _) = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();

        server.send_raw_line(r#"{"Unknown":{"value":1}}"#).unwrap();
        assert!(timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .is_none());

        let error = connection
            .take_terminal_error()
            .expect("decoder failure must be retained for lifecycle handling");
        assert!(matches!(
            error,
            TerminalConnectionError::Protocol(message)
                if message.contains("Unknown protocol message: Unknown")
        ));
        server.close();
    }

    #[tokio::test]
    async fn tls_accepted_branch_upgrades_and_exchanges_hello() {
        let mut server = FakeTlsSyncplayServer::start().await.unwrap();
        let connection = Connection::new();
        let (mut receiver, _) = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();

        connection
            .send(ProtocolMessage::TLS {
                TLS: crate::network::messages::TLSMessage {
                    start_tls: Some("send".to_string()),
                },
            })
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::TLS { .. }
        ));
        assert!(matches!(
            timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::TLS { TLS } if TLS.start_tls.as_deref() == Some("accepted")
        ));

        connection
            .upgrade_tls_with_timeout_and_extra_roots(
                Duration::from_secs(2),
                [server.trusted_root()],
            )
            .await
            .unwrap();

        connection
            .send(ProtocolMessage::Hello {
                Hello: HelloMessage {
                    username: "tester".to_string(),
                    password: None,
                    room: Some(RoomInfo {
                        name: "room".to_string(),
                        password: None,
                    }),
                    version: "1.2.255".to_string(),
                    realversion: "1.7.5".to_string(),
                    features: None,
                    motd: None,
                },
            })
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));

        server
            .send(FakeSyncplayServer::hello_response("tester", "room"))
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::Hello { .. }
        ));
        connection.disconnect();
        server.close();
    }

    #[tokio::test]
    async fn fake_server_no_tls_state_exchange_close_and_reconnect() {
        use crate::network::messages::{PingInfo, PlayState, StateMessage};

        async fn connect_once(
            server: &mut FakeSyncplayServer,
        ) -> (Connection, mpsc::UnboundedReceiver<ProtocolMessage>) {
            let connection = Connection::new();
            let (receiver, _) = connection
                .connect(server.host(), server.port())
                .await
                .unwrap();
            connection
                .send(ProtocolMessage::TLS {
                    TLS: crate::network::messages::TLSMessage {
                        start_tls: Some("send".to_string()),
                    },
                })
                .unwrap();
            let tls_request = timeout(Duration::from_secs(2), server.next_received())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(tls_request, ProtocolMessage::TLS { .. }));
            server
                .send(FakeSyncplayServer::tls_response("unsupported"))
                .unwrap();
            (connection, receiver)
        }

        let mut server1 = FakeSyncplayServer::start().await.unwrap();
        let (connection1, mut receiver1) = connect_once(&mut server1).await;
        assert!(matches!(
            timeout(Duration::from_secs(2), receiver1.recv())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::TLS { .. }
        ));

        for index in 0..3 {
            server1
                .send(ProtocolMessage::State {
                    State: StateMessage {
                        playstate: Some(PlayState {
                            position: index as f64,
                            paused: index == 0,
                            do_seek: None,
                            set_by: Some("server".to_string()),
                        }),
                        ping: Some(PingInfo {
                            latency_calculation: Some(index as f64 + 0.5),
                            client_latency_calculation: None,
                            client_rtt: None,
                            server_rtt: Some(0.01),
                        }),
                        ignoring_on_the_fly: None,
                    },
                })
                .unwrap();
            assert!(matches!(
                timeout(Duration::from_secs(2), receiver1.recv())
                    .await
                    .unwrap()
                    .unwrap(),
                ProtocolMessage::State { .. }
            ));
        }

        server1.close();
        timeout(Duration::from_secs(2), async {
            loop {
                if receiver1.recv().await.is_none() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        connection1.disconnect();

        let mut server2 = FakeSyncplayServer::start().await.unwrap();
        let (connection2, mut receiver2) = connect_once(&mut server2).await;
        assert!(matches!(
            timeout(Duration::from_secs(2), receiver2.recv())
                .await
                .unwrap()
                .unwrap(),
            ProtocolMessage::TLS { .. }
        ));
        connection2.disconnect();
    }
    #[tokio::test]
    async fn connection_exchanges_messages_with_fake_syncplay_server() {
        let mut server = FakeSyncplayServer::start().await.unwrap();
        let connection = Connection::new();
        let (mut receiver, peer_address) = connection
            .connect(server.host(), server.port())
            .await
            .unwrap();

        assert_eq!(peer_address.as_deref(), Some("127.0.0.1"));
        assert!(connection.is_connected());

        let hello = ProtocolMessage::Hello {
            Hello: HelloMessage {
                username: "tester".to_string(),
                password: None,
                room: Some(RoomInfo {
                    name: "room".to_string(),
                    password: None,
                }),
                version: "1.2.255".to_string(),
                realversion: "1.7.5".to_string(),
                features: None,
                motd: None,
            },
        };
        connection.send(hello).unwrap();

        let received = timeout(Duration::from_secs(2), server.next_received())
            .await
            .unwrap()
            .unwrap();
        match received {
            ProtocolMessage::Hello { Hello } => {
                assert_eq!(Hello.username, "tester");
                assert_eq!(Hello.room.unwrap().name, "room");
            }
            other => panic!("expected Hello from client, got {other:?}"),
        }

        server
            .send(FakeSyncplayServer::hello_response("tester", "room"))
            .unwrap();
        let response = timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        match response {
            ProtocolMessage::Hello { Hello } => {
                assert_eq!(Hello.username, "tester");
                assert_eq!(Hello.room.unwrap().name, "room");
            }
            other => panic!("expected Hello from fake server, got {other:?}"),
        }

        connection.disconnect();
        assert!(!connection.is_connected());
    }
}
