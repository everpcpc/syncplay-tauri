use anyhow::Result;
use rustls::{Certificate, ClientConfig, RootCertStore};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

#[derive(Debug, Clone)]
pub struct TlsInfo {
    pub protocol: Option<String>,
}

/// Create a TLS connector with system root certificates
pub fn create_tls_connector() -> Result<TlsConnector> {
    create_tls_connector_with_extra_roots(std::iter::empty::<Certificate>())
}

pub fn create_tls_connector_with_extra_roots<I>(extra_roots: I) -> Result<TlsConnector>
where
    I: IntoIterator<Item = Certificate>,
{
    let mut root_store = RootCertStore::empty();

    // Add system root certificates
    for cert in rustls_native_certs::load_native_certs()? {
        root_store.add(&rustls::Certificate(cert.0))?;
    }

    for cert in extra_roots {
        root_store.add(&cert)?;
    }

    let config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(TlsConnector::from(Arc::new(config)))
}

/// Upgrade a TCP stream to TLS
pub async fn upgrade_to_tls(
    stream: TcpStream,
    domain: &str,
) -> Result<(TlsStream<TcpStream>, TlsInfo)> {
    let connector = create_tls_connector()?;
    let domain = match domain.parse::<std::net::IpAddr>() {
        Ok(ip) => rustls::ServerName::IpAddress(ip),
        Err(_) => rustls::ServerName::try_from(domain)?,
    };
    let tls_stream = connector.connect(domain, stream).await?;
    let protocol = tls_stream
        .get_ref()
        .1
        .protocol_version()
        .map(|version| match version {
            rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2".to_string(),
            rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3".to_string(),
            other => format!("{:?}", other),
        });
    Ok((tls_stream, TlsInfo { protocol }))
}
