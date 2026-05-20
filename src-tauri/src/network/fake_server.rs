use crate::network::messages::{HelloMessage, ProtocolMessage, RoomInfo, TLSMessage};
use crate::network::protocol::SyncplayCodec;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

/// In-process Syncplay server fixture for regression tests.
///
/// This harness binds to 127.0.0.1 on an ephemeral port and speaks the same
/// newline-delimited JSON codec as the production connection layer. Tests can
/// assert outbound client messages and inject server responses without touching
/// public Syncplay servers.
pub enum FakeServerCommand {
    Message(ProtocolMessage),
    AbortConnection,
    CloseConnection,
    Close,
}

pub struct FakeSyncplayServer {
    address: std::net::SocketAddr,
    received_rx: mpsc::UnboundedReceiver<ProtocolMessage>,
    outbound_tx: mpsc::UnboundedSender<FakeServerCommand>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FakeSyncplayServer {
    async fn bind(address: (&str, u16)) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(address).await?;
        let address = listener.local_addr()?;
        let (received_tx, received_rx) = mpsc::unbounded_channel();
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            let mut outbound_rx = outbound_rx;
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, _peer)) => {
                                if run_fake_server_connection(stream, received_tx.clone(), &mut outbound_rx).await {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    _ = &mut shutdown_rx => break,
                }
            }
        });

        Ok(Self {
            address,
            received_rx,
            outbound_tx,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    pub async fn start() -> anyhow::Result<Self> {
        Self::bind(("127.0.0.1", 0)).await
    }

    pub async fn start_on_port(port: u16) -> anyhow::Result<Self> {
        Self::bind(("127.0.0.1", port)).await
    }

    pub fn host(&self) -> String {
        self.address.ip().to_string()
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    pub fn send(&self, message: ProtocolMessage) -> anyhow::Result<()> {
        self.outbound_tx.send(FakeServerCommand::Message(message))?;
        Ok(())
    }

    pub async fn next_received(&mut self) -> Option<ProtocolMessage> {
        self.received_rx.recv().await
    }

    pub fn abort_connection(&self) {
        let _ = self.outbound_tx.send(FakeServerCommand::AbortConnection);
    }

    pub fn take_command_sender(&self) -> mpsc::UnboundedSender<FakeServerCommand> {
        self.outbound_tx.clone()
    }

    pub async fn wait_for_reconnect_message(&mut self) -> Option<ProtocolMessage> {
        loop {
            if let Some(message) = self.next_received().await {
                if matches!(
                    message,
                    ProtocolMessage::TLS { .. } | ProtocolMessage::Hello { .. }
                ) {
                    return Some(message);
                }
            } else {
                return None;
            }
        }
    }

    pub async fn complete_reconnect_handshake(&mut self) -> anyhow::Result<()> {
        self.abort_connection();
        match self.wait_for_reconnect_message().await {
            Some(ProtocolMessage::TLS { .. }) => {
                self.send(Self::tls_response("unsupported"))?;
                loop {
                    if matches!(
                        self.next_received().await,
                        Some(ProtocolMessage::Hello { .. })
                    ) {
                        break;
                    }
                }
            }
            Some(ProtocolMessage::Hello { .. }) => {}
            Some(other) => anyhow::bail!("unexpected reconnect message: {other:?}"),
            None => anyhow::bail!("fake server closed before reconnect"),
        }
        Ok(())
    }

    pub fn close(mut self) {
        let _ = self.outbound_tx.send(FakeServerCommand::Close);
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }

    pub fn tls_response(start_tls: &str) -> ProtocolMessage {
        ProtocolMessage::TLS {
            TLS: TLSMessage {
                start_tls: Some(start_tls.to_string()),
            },
        }
    }

    pub fn hello_response(username: &str, room: &str) -> ProtocolMessage {
        ProtocolMessage::Hello {
            Hello: HelloMessage {
                username: username.to_string(),
                password: None,
                room: Some(RoomInfo {
                    name: room.to_string(),
                    password: None,
                }),
                version: "1.2.255".to_string(),
                realversion: "1.7.5".to_string(),
                features: Some(serde_json::json!({
                    "featureList": true,
                    "sharedPlaylists": true,
                    "chat": true,
                    "readiness": true,
                    "managedRooms": true,
                    "persistentRooms": false,
                })),
                motd: None,
            },
        }
    }
}

async fn run_fake_server_connection(
    stream: TcpStream,
    received_tx: mpsc::UnboundedSender<ProtocolMessage>,
    outbound_rx: &mut mpsc::UnboundedReceiver<FakeServerCommand>,
) -> bool {
    let mut framed = Framed::new(stream, SyncplayCodec::new());
    loop {
        tokio::select! {
            received = framed.next() => {
                match received {
                    Some(Ok(message)) => {
                        if received_tx.send(message).is_err() {
                            break;
                        }
                    }
                    Some(Err(_)) | None => break,
                }
            }
            command = outbound_rx.recv() => {
                match command {
                    Some(FakeServerCommand::Message(message)) => {
                        if framed.send(message).await.is_err() {
                            break;
                        }
                    }
                    Some(FakeServerCommand::AbortConnection) => return false,
                    Some(FakeServerCommand::CloseConnection) => break,
                    Some(FakeServerCommand::Close) | None => return true,
                }
            }
        }
    }
    false
}

#[cfg(test)]
pub mod tls_fixture {
    use super::*;
    use rcgen::generate_simple_self_signed;
    use rustls::{Certificate, PrivateKey, ServerConfig};
    use std::sync::Arc;
    use tokio_rustls::TlsAcceptor;

    pub struct FakeTlsSyncplayServer {
        inner: FakeSyncplayServer,
        root_certificate: Certificate,
    }

    impl FakeTlsSyncplayServer {
        pub async fn start() -> anyhow::Result<Self> {
            let certified_key = generate_simple_self_signed(vec!["127.0.0.1".to_string()])?;
            let cert_der = certified_key.serialize_der()?;
            let key_der = certified_key.serialize_private_key_der();
            let root_certificate = Certificate(cert_der.clone());
            Self::start_with_certificate(cert_der, key_der, root_certificate).await
        }

        async fn start_with_certificate(
            cert_der: Vec<u8>,
            key_der: Vec<u8>,
            root_certificate: Certificate,
        ) -> anyhow::Result<Self> {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let address = listener.local_addr()?;
            let (received_tx, received_rx) = mpsc::unbounded_channel();
            let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
            let server_config = ServerConfig::builder()
                .with_safe_defaults()
                .with_no_client_auth()
                .with_single_cert(vec![Certificate(cert_der)], PrivateKey(key_der))?;
            let acceptor = TlsAcceptor::from(Arc::new(server_config));

            tokio::spawn(async move {
                let mut shutdown_rx = shutdown_rx;
                let mut outbound_rx = outbound_rx;
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            match accepted {
                                Ok((stream, _peer)) => {
                                    if run_fake_tls_connection(stream, received_tx.clone(), &mut outbound_rx, acceptor.clone()).await {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        _ = &mut shutdown_rx => break,
                    }
                }
            });

            Ok(Self {
                inner: FakeSyncplayServer {
                    address,
                    received_rx,
                    outbound_tx,
                    shutdown_tx: Some(shutdown_tx),
                },
                root_certificate,
            })
        }

        pub fn host(&self) -> String {
            self.inner.host()
        }

        pub fn port(&self) -> u16 {
            self.inner.port()
        }

        pub fn trusted_root(&self) -> Certificate {
            self.root_certificate.clone()
        }

        pub fn send(&self, message: ProtocolMessage) -> anyhow::Result<()> {
            self.inner.send(message)
        }

        pub async fn next_received(&mut self) -> Option<ProtocolMessage> {
            self.inner.next_received().await
        }

        pub fn close(self) {
            self.inner.close();
        }
    }

    async fn run_fake_tls_connection(
        stream: TcpStream,
        received_tx: mpsc::UnboundedSender<ProtocolMessage>,
        outbound_rx: &mut mpsc::UnboundedReceiver<FakeServerCommand>,
        acceptor: TlsAcceptor,
    ) -> bool {
        let mut plain = Framed::new(stream, SyncplayCodec::new());
        let first = plain.next().await;
        let Some(Ok(message)) = first else {
            return false;
        };
        if received_tx.send(message).is_err() {
            return true;
        }
        if plain
            .send(FakeSyncplayServer::tls_response("accepted"))
            .await
            .is_err()
        {
            return false;
        }

        let stream = plain.into_inner();
        let tls_stream = match acceptor.accept(stream).await {
            Ok(stream) => stream,
            Err(_) => return false,
        };
        let mut framed = Framed::new(tls_stream, SyncplayCodec::new());
        loop {
            tokio::select! {
                received = framed.next() => {
                    match received {
                        Some(Ok(message)) => {
                            if received_tx.send(message).is_err() {
                                break;
                            }
                        }
                        Some(Err(_)) | None => break,
                    }
                }
                command = outbound_rx.recv() => {
                    match command {
                        Some(FakeServerCommand::Message(message)) => {
                            if framed.send(message).await.is_err() {
                                break;
                            }
                        }
                        Some(FakeServerCommand::AbortConnection) => return false,
                        Some(FakeServerCommand::CloseConnection) => break,
                        Some(FakeServerCommand::Close) | None => return true,
                    }
                }
            }
        }
        false
    }
}
