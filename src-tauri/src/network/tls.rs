use anyhow::Result;
use rustls::{ClientConfig, RootCertStore};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

/// Create a TLS connector with system root certificates
pub fn create_tls_connector() -> Result<TlsConnector> {
    let mut root_store = RootCertStore::empty();

    // Add system root certificates
    for cert in rustls_native_certs::load_native_certs()? {
        root_store.add(&rustls::Certificate(cert.0))?;
    }

    let config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(TlsConnector::from(Arc::new(config)))
}

/// Upgrade a TCP stream to TLS
pub async fn upgrade_to_tls(stream: TcpStream, domain: &str) -> Result<TlsStream<TcpStream>> {
    let connector = create_tls_connector()?;
    let domain = match domain.parse::<std::net::IpAddr>() {
        Ok(ip) => rustls::ServerName::IpAddress(ip),
        Err(_) => rustls::ServerName::try_from(domain)?,
    };
    let tls_stream = connector.connect(domain, stream).await?;
    Ok(tls_stream)
}
