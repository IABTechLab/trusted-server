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
use super::manager::{ConnectionId, Lease, Manager, Reservation};
use crate::commands::dev::proxy::metrics::ProxyMetrics;

/// HTTP/1 request sender whose upload body preserves streaming frames.
pub type UpstreamSender = SendRequest<RequestUploadBody>;

/// Successfully opened sender plus its driver controls and manager reservation.
pub struct OpenedConnection {
    sender: Option<UpstreamSender>,
    abort: AbortHandle,
    start: Option<oneshot::Sender<()>>,
    reservation: Option<Reservation>,
}

impl OpenedConnection {
    /// Acknowledges the successful connector result and registers its driver.
    ///
    /// # Panics
    ///
    /// Panics only if internal code attempts to register the same opened
    /// connection more than once.
    pub fn register(
        mut self,
        manager: &Manager<UpstreamSender>,
    ) -> (Lease<UpstreamSender>, oneshot::Sender<()>) {
        let sender = self.sender.take().expect("should retain opened sender");
        let start = self.start.take().expect("should retain driver start latch");
        let reservation = self
            .reservation
            .take()
            .expect("should retain connector reservation");
        let lease = reservation.register(manager, sender, self.abort.clone());
        (lease, start)
    }
}

impl Drop for OpenedConnection {
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            reservation.abort_established(&self.abort);
        }
    }
}

struct EstablishedConnection {
    sender: UpstreamSender,
    abort: AbortHandle,
    start: oneshot::Sender<()>,
}

#[derive(Debug, Clone, Copy)]
/// Timeouts and harness-only delays used while opening an upstream connection.
pub struct ConnectPolicy {
    /// Total deadline for DNS, TCP, TLS, and HTTP handshaking.
    pub timeout: Duration,
    /// Harness-only delay before TCP connection attempts.
    pub connect_delay: Duration,
    /// Harness-only delay before TLS handshaking.
    pub tls_delay: Duration,
}

/// Abort-on-drop network connector owned by one manager reservation.
pub struct PendingConnection {
    task: Option<tokio::task::JoinHandle<Result<OpenedConnection, Report<io::Error>>>>,
}

impl PendingConnection {
    /// Spawns one connector while transferring manager-reservation ownership
    /// into its task.
    #[must_use]
    pub fn spawn(
        key: OriginKey,
        sni: Option<ServerName<'static>>,
        policy: ConnectPolicy,
        metrics: Arc<ProxyMetrics>,
        manager: Arc<Manager<UpstreamSender>>,
        dns: Arc<DnsCache>,
        reservation: Reservation,
    ) -> Self {
        Self {
            task: Some(tokio::spawn(async move {
                let id = reservation.id();
                let established = open(&key, sni, policy, metrics, manager, dns, id).await?;
                Ok(OpenedConnection {
                    sender: Some(established.sender),
                    abort: established.abort,
                    start: Some(established.start),
                    reservation: Some(reservation),
                })
            })),
        }
    }

    #[must_use]
    /// Returns the task abort handle while the connector is pending.
    ///
    /// # Panics
    ///
    /// Panics only if called after this value has already completed.
    pub fn abort_handle(&self) -> AbortHandle {
        self.task
            .as_ref()
            .expect("should retain connector task")
            .abort_handle()
    }

    /// Waits for connector completion while retaining abort-on-caller-drop behavior.
    ///
    /// # Errors
    ///
    /// Returns the connection error or a task cancellation/panic as an I/O report.
    ///
    /// # Panics
    ///
    /// Panics only if internal code removes the task before awaiting it.
    pub async fn finish(mut self) -> Result<OpenedConnection, Report<io::Error>> {
        let result = self
            .task
            .as_mut()
            .expect("should retain connector task")
            .await;
        self.task.take();
        result.map_err(|error| Report::new(io::Error::other(error.to_string())))?
    }
}

impl Drop for PendingConnection {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Opens TCP/TLS and completes an HTTP/1 client handshake for one reservation.
///
/// # Errors
///
/// Returns the underlying DNS, connection, TLS, or HTTP handshake error.
async fn open(
    key: &OriginKey,
    sni: Option<ServerName<'static>>,
    policy: ConnectPolicy,
    metrics: Arc<ProxyMetrics>,
    manager: Arc<Manager<UpstreamSender>>,
    dns: Arc<DnsCache>,
    id: ConnectionId,
) -> Result<EstablishedConnection, Report<io::Error>> {
    let deadline = tokio::time::Instant::now() + policy.timeout;
    let addresses: Vec<std::net::SocketAddr> = match key.address_policy() {
        AddressPolicy::Resolve(address) => vec![(address, key.port()).into()],
        AddressPolicy::Dns => match key.reference() {
            ReferenceIdentity::Ip(address) => vec![(*address, key.port()).into()],
            ReferenceIdentity::Dns(host) => dns
                .lookup(host, key.port(), deadline, Arc::clone(&metrics))
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
    if !policy.connect_delay.is_zero() {
        tokio::time::sleep(policy.connect_delay).await;
    }
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
        if !policy.tls_delay.is_zero() {
            tokio::time::sleep(policy.tls_delay).await;
        }
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
) -> Result<EstablishedConnection, Report<io::Error>>
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
    Ok(EstablishedConnection {
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
        static SCHEMES: std::sync::OnceLock<Vec<rustls::SignatureScheme>> =
            std::sync::OnceLock::new();
        SCHEMES
            .get_or_init(|| {
                rustls::crypto::aws_lc_rs::default_provider()
                    .signature_verification_algorithms
                    .supported_schemes()
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;
    use crate::commands::dev::proxy::upstream::key::ReferenceIdentity;
    use crate::commands::dev::proxy::upstream::manager::{Acquired, PoolLimits};

    fn key() -> OriginKey {
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        OriginKey::new(
            Transport::Tls,
            ReferenceIdentity::ip(address),
            443,
            VerifyMode::Insecure,
            AddressPolicy::Dns,
        )
    }

    fn delayed_policy() -> ConnectPolicy {
        ConnectPolicy {
            timeout: Duration::from_secs(30),
            connect_delay: Duration::from_secs(10),
            tls_delay: Duration::ZERO,
        }
    }

    #[test]
    fn client_configs_are_cached_by_verification_and_http1_only() {
        let secure = client_config(VerifyMode::Secure);
        let secure_again = client_config(VerifyMode::Secure);
        let insecure = client_config(VerifyMode::Insecure);
        let insecure_again = client_config(VerifyMode::Insecure);

        assert!(Arc::ptr_eq(&secure, &secure_again));
        assert!(Arc::ptr_eq(&insecure, &insecure_again));
        assert!(!Arc::ptr_eq(&secure, &insecure));
        assert_eq!(secure.alpn_protocols, [b"http/1.1".to_vec()]);
        assert_eq!(insecure.alpn_protocols, [b"http/1.1".to_vec()]);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_pending_connector_reconciles_reservation_once() {
        let manager = Manager::start(PoolLimits {
            per_origin_live: 1,
            global_live: 1,
            ..PoolLimits::default()
        });
        let Acquired::Open(reservation) = manager.acquire(key()).await.expect("should reserve")
        else {
            panic!("should open connector reservation");
        };
        let id = reservation.id();
        let pending = PendingConnection::spawn(
            key(),
            Some(ServerName::from(IpAddr::V4(Ipv4Addr::LOCALHOST))),
            delayed_policy(),
            Arc::new(ProxyMetrics::default()),
            Arc::clone(&manager),
            Arc::new(DnsCache::default()),
            reservation,
        );
        let abort = pending.abort_handle();
        manager.register_connector(id, abort.clone());

        drop(pending);
        let replacement = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.acquire(key()).await }
        });
        while !abort.is_finished() {
            assert!(
                !replacement.is_finished(),
                "replacement capacity must remain blocked until connector cancellation completes"
            );
            tokio::task::yield_now().await;
        }
        let Acquired::Open(replacement) = replacement
            .await
            .expect("should join replacement")
            .expect("should reconcile")
        else {
            panic!("should admit one replacement");
        };
        manager.driver_closed(id);
        let overflow = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.acquire(key()).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !overflow.is_finished(),
            "duplicate completion must not free replacement capacity"
        );
        drop(replacement);
        tokio::task::yield_now().await;
        assert!(matches!(
            overflow.await.expect("should join overflow"),
            Ok(Acquired::Open(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn connector_owner_unwind_reconciles_reservation() {
        let manager = Manager::start(PoolLimits {
            per_origin_live: 1,
            global_live: 1,
            ..PoolLimits::default()
        });
        let Acquired::Open(reservation) = manager.acquire(key()).await.expect("should reserve")
        else {
            panic!("should open connector reservation");
        };
        let id = reservation.id();
        let pending = PendingConnection::spawn(
            key(),
            Some(ServerName::from(IpAddr::V4(Ipv4Addr::LOCALHOST))),
            delayed_policy(),
            Arc::new(ProxyMetrics::default()),
            Arc::clone(&manager),
            Arc::new(DnsCache::default()),
            reservation,
        );
        let abort = pending.abort_handle();
        manager.register_connector(id, abort.clone());

        let owner = tokio::spawn(async move {
            let _pending = pending;
            panic!("exercise connector owner unwind");
        });
        assert!(owner.await.expect_err("owner should panic").is_panic());
        tokio::task::yield_now().await;

        assert!(abort.is_finished(), "unwind should abort connector task");
        assert!(matches!(
            manager.acquire(key()).await,
            Ok(Acquired::Open(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_waits_for_connector_cancellation_completion() {
        let manager = Manager::start(PoolLimits {
            per_origin_live: 1,
            global_live: 1,
            ..PoolLimits::default()
        });
        let Acquired::Open(reservation) = manager.acquire(key()).await.expect("should reserve")
        else {
            panic!("should open connector reservation");
        };
        let id = reservation.id();
        let pending = PendingConnection::spawn(
            key(),
            Some(ServerName::from(IpAddr::V4(Ipv4Addr::LOCALHOST))),
            delayed_policy(),
            Arc::new(ProxyMetrics::default()),
            Arc::clone(&manager),
            Arc::new(DnsCache::default()),
            reservation,
        );
        let abort = pending.abort_handle();
        manager.register_connector(id, abort.clone());

        let shutdown = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.shutdown().await }
        });
        while !abort.is_finished() {
            assert!(
                !shutdown.is_finished(),
                "shutdown must not acknowledge while the connector task is still live"
            );
            tokio::task::yield_now().await;
        }
        tokio::task::yield_now().await;
        assert!(
            shutdown.is_finished(),
            "shutdown should acknowledge after connector cleanup reconciles capacity"
        );
        drop(pending);
    }
}
