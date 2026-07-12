use std::io;
use std::sync::Arc;
use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio_rustls::TlsConnector;

use super::body::RequestUploadBody;
use super::dns::DnsCache;
use super::key::{AddressPolicy, OriginKey, ReferenceIdentity, Transport, VerifyMode};
use super::manager::{ConnectionId, Manager};
use crate::commands::dev::proxy::metrics::ProxyMetrics;

pub type UpstreamSender = SendRequest<RequestUploadBody>;

pub struct OpenedConnection {
    pub sender: UpstreamSender,
    pub abort: AbortHandle,
    pub start: oneshot::Sender<()>,
}

/// Opens TCP/TLS and completes an HTTP/1 client handshake for one reservation.
///
/// # Errors
///
/// Returns the underlying DNS, connection, TLS, or HTTP handshake error.
pub async fn open(
    key: &OriginKey,
    sni: Option<ServerName<'static>>,
    timeout: Duration,
    metrics: Arc<ProxyMetrics>,
    manager: Arc<Manager<UpstreamSender>>,
    dns: Arc<DnsCache>,
    id: ConnectionId,
) -> Result<OpenedConnection, Report<io::Error>> {
    let deadline = tokio::time::Instant::now() + timeout;
    let addresses: Vec<std::net::SocketAddr> = match key.address_policy() {
        AddressPolicy::Resolve(address) => vec![(address, key.port()).into()],
        AddressPolicy::Dns => match key.reference() {
            ReferenceIdentity::Ip(address) => vec![(*address, key.port()).into()],
            ReferenceIdentity::Dns(host) => dns
                .lookup(host, key.port(), deadline, &metrics)
                .await?
                .to_vec(),
        },
    };
    if addresses.is_empty() {
        return Err(Report::new(io::Error::new(
            io::ErrorKind::NotFound,
            "upstream DNS returned no addresses",
        )));
    }
    let mut last_error = None;
    let mut connected = None;
    for (index, address) in addresses.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let addresses_left = u32::try_from(addresses.len() - index).unwrap_or(u32::MAX);
        let slice = remaining / addresses_left;
        metrics.record_tcp_attempt();
        let started = tokio::time::Instant::now();
        match tokio::time::timeout(slice, TcpStream::connect(address)).await {
            Ok(Ok(tcp)) => {
                metrics.record_tcp_established(started.elapsed());
                connected = Some(tcp);
                break;
            }
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                last_error = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("upstream connect to {address} timed out"),
                ));
            }
        }
    }
    let tcp = connected.ok_or_else(|| {
        Report::new(last_error.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "upstream connect deadline elapsed")
        }))
    })?;
    if key.transport() == Transport::Tls {
        let connector = TlsConnector::from(client_config(key.verify_mode()));
        let server_name = sni.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "TLS origin has no server name")
        })?;
        let tls_started = tokio::time::Instant::now();
        let tls = connector
            .connect(server_name, tcp)
            .await
            .map_err(io::Error::other)?;
        metrics.record_tls_handshake(tls_started.elapsed());
        handshake(TokioIo::new(tls), manager, id, metrics).await
    } else {
        handshake(TokioIo::new(tcp), manager, id, metrics).await
    }
}

async fn handshake<I>(
    io: I,
    manager: Arc<Manager<UpstreamSender>>,
    id: ConnectionId,
    metrics: Arc<ProxyMetrics>,
) -> Result<OpenedConnection, Report<io::Error>>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let handshake_started = tokio::time::Instant::now();
    let (sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(io::Error::other)
        .attach("HTTP/1 upstream handshake failed")?;
    metrics.record_http_handshake(handshake_started.elapsed());
    metrics.record_negotiated_http1();
    let (start, started) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _guard = DriverGuard { manager, id };
        let _ = started.await;
        if let Err(error) = connection.await {
            log::debug!("upstream connection closed: {error}");
        }
    });
    Ok(OpenedConnection {
        sender,
        abort: task.abort_handle(),
        start,
    })
}

struct DriverGuard {
    manager: Arc<Manager<UpstreamSender>>,
    id: ConnectionId,
}

impl Drop for DriverGuard {
    fn drop(&mut self) {
        self.manager.driver_closed(self.id);
    }
}

fn client_config(mode: VerifyMode) -> Arc<rustls::ClientConfig> {
    static SECURE: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    static INSECURE: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    let cell = if mode == VerifyMode::Insecure {
        &INSECURE
    } else {
        &SECURE
    };
    Arc::clone(cell.get_or_init(|| {
        let mut config = if mode == VerifyMode::Insecure {
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerifier))
                .with_no_client_auth()
        } else {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        };
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(config)
    }))
}

#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
