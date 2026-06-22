//! Accept loop, CONNECT dispatch, blind tunnel, MITM, and local routes (spec §5).
//!
//! Each accepted connection's first request line decides the path:
//! a `CONNECT host:port` is matched against [`ResolvedConfig::rules`] *before*
//! replying — a match is MITM'd (a leaf is minted, the TLS stream is decrypted
//! and proxied request-by-request); a non-match is blind-tunnelled on loopback
//! or refused (`403`) off loopback. An origin-form `GET /proxy.pac` is served
//! locally.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use error_stack::{Report, ResultExt as _};
use http_body_util::{BodyExt as _, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{HeaderName, HeaderValue};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use super::ProxyError;
use super::ca::CertAuthority;
use super::config::ResolvedConfig;
use super::rewrite::rewrite_for;

const X_ORIG_HOST: &str = "x-orig-host";

/// Binds the listen socket. Separate from [`serve_on`] so the caller can open
/// the port (queueing connections) before launching browsers (spec §9, Task 6).
///
/// # Errors
///
/// Returns the bind I/O error if the address is unavailable.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

/// Accepts and serves connections on `listener` until the task is dropped.
///
/// One bad connection never tears down the loop: per-connection failures are
/// logged and the loop continues.
///
/// # Errors
///
/// Returns [`ProxyError::Server`] only on an unrecoverable accept-loop failure.
pub async fn serve_on(
    listener: TcpListener,
    cfg: Arc<ResolvedConfig>,
    ca: Arc<CertAuthority>,
    pac: Arc<str>,
) -> Result<(), Report<ProxyError>> {
    let is_loopback = is_loopback(cfg.listen.ip());
    log::info!("listening on {}", cfg.listen);
    loop {
        let (client, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                log::warn!("accept failed: {err}");
                continue;
            }
        };
        let cfg = Arc::clone(&cfg);
        let ca = Arc::clone(&ca);
        let pac = Arc::clone(&pac);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(client, is_loopback, &cfg, &ca, &pac).await {
                log::debug!("connection from {peer} ended: {err:?}");
            }
        });
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// The first request head of an accepted connection, buffered for routing.
struct RequestHead {
    method: String,
    target: String,
    /// The raw bytes consumed from the client socket (the full HTTP head up to
    /// and including `\r\n\r\n`). Retained so they can be forwarded verbatim
    /// when the request is blind-forwarded as plain HTTP (spec §8.4).
    raw: Vec<u8>,
}

impl RequestHead {
    /// `Some(host:port)` when the request is a `CONNECT`.
    fn connect_authority(&self) -> Option<&str> {
        (self.method.eq_ignore_ascii_case("CONNECT")).then_some(self.target.as_str())
    }

    /// Whether this is the local `GET /proxy.pac` route.
    fn is_local_pac_route(&self) -> bool {
        self.method.eq_ignore_ascii_case("GET")
            && (self.target == "/proxy.pac" || self.target.ends_with("/proxy.pac"))
    }
}

/// Reads bytes until the end of the request head (`\r\n\r\n`) and parses
/// method/target from the first request line.
///
/// The raw bytes are retained on the returned [`RequestHead`] so that a stray
/// absolute-form plain-HTTP request can be forwarded unchanged (spec §8.4) —
/// `blind_forward_http` writes them to the upstream before piping the remainder.
async fn read_request_head(client: &mut TcpStream) -> Result<RequestHead, Report<ProxyError>> {
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    // Read up to the end of the headers (\r\n\r\n) or a sane cap.
    loop {
        let n = client
            .read(&mut byte)
            .await
            .change_context(ProxyError::Server)?;
        if n == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") || buf.len() > 8192 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let first_line = text.lines().next().unwrap_or_default();
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    Ok(RequestHead { method, target, raw: buf })
}

async fn handle_connection(
    mut client: TcpStream,
    is_loopback: bool,
    cfg: &ResolvedConfig,
    ca: &CertAuthority,
    pac: &str,
) -> Result<(), Report<ProxyError>> {
    let head = read_request_head(&mut client).await?;
    if let Some(authority) = head.connect_authority() {
        let authority = authority.to_string();
        return handle_connect(client, &authority, is_loopback, cfg, ca).await;
    }
    if head.is_local_pac_route() {
        return serve_pac(&mut client, pac).await;
    }
    // Stray absolute-form plain HTTP.
    if is_loopback {
        blind_forward_http(client, &head).await
    } else {
        respond_status_line(&mut client, StatusCode::FORBIDDEN).await
    }
}

/// Splits `host:port`, defaulting the port to 443.
fn split_authority(authority: &str) -> (String, u16) {
    match authority.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse().unwrap_or(443)),
        None => (authority.to_string(), 443),
    }
}

async fn handle_connect(
    mut client: TcpStream,
    authority: &str,
    is_loopback: bool,
    cfg: &ResolvedConfig,
    ca: &CertAuthority,
) -> Result<(), Report<ProxyError>> {
    let (host, port) = split_authority(authority);

    // Match BEFORE replying, so an unmatched non-loopback request is refused.
    if cfg.rules.first_match(&host).is_some() {
        write_connect_ok(&mut client).await?;
        return mitm(client, &host, cfg, ca).await;
    }

    if !is_loopback {
        log::warn!("refusing un-mapped CONNECT {host} off loopback");
        return respond_status_line(&mut client, StatusCode::FORBIDDEN).await;
    }

    // No match on loopback: connect upstream FIRST, then reply 200 (else 502).
    blind_tunnel(client, &host, port).await
}

/// Connects to the upstream first; on success replies `200` then pipes bytes
/// in both directions without decrypting anything.
async fn blind_tunnel(
    mut client: TcpStream,
    host: &str,
    port: u16,
) -> Result<(), Report<ProxyError>> {
    let mut upstream = match TcpStream::connect((host, port)).await {
        Ok(stream) => stream,
        Err(err) => {
            log::warn!("blind tunnel to {host}:{port} failed: {err}");
            return respond_status_line(&mut client, StatusCode::BAD_GATEWAY).await;
        }
    };
    write_connect_ok(&mut client).await?;
    match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        Ok(_) => Ok(()),
        Err(err) => {
            log::debug!("blind tunnel to {host}:{port} closed: {err}");
            Ok(())
        }
    }
}

async fn write_connect_ok(client: &mut TcpStream) -> Result<(), Report<ProxyError>> {
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .change_context(ProxyError::Server)?;
    client.flush().await.change_context(ProxyError::Server)
}

async fn respond_status_line(
    client: &mut TcpStream,
    status: StatusCode,
) -> Result<(), Report<ProxyError>> {
    let reason = status.canonical_reason().unwrap_or("");
    let body = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        status.as_u16(),
    );
    client
        .write_all(body.as_bytes())
        .await
        .change_context(ProxyError::Server)?;
    client.flush().await.change_context(ProxyError::Server)
}

async fn serve_pac(client: &mut TcpStream, pac: &str) -> Result<(), Report<ProxyError>> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{pac}",
        pac.len(),
    );
    client
        .write_all(response.as_bytes())
        .await
        .change_context(ProxyError::Server)?;
    client.flush().await.change_context(ProxyError::Server)
}

/// Blind-forwards a stray absolute-form plain-HTTP request on loopback.
///
/// Connects to the target authority, writes the already-buffered request head
/// verbatim, then pipes the remaining bytes bidirectionally (spec §8.4).
/// Best-effort: failures are logged, never fatal.
async fn blind_forward_http(
    mut client: TcpStream,
    head: &RequestHead,
) -> Result<(), Report<ProxyError>> {
    let Ok(uri) = head.target.parse::<Uri>() else {
        return respond_status_line(&mut client, StatusCode::BAD_REQUEST).await;
    };
    let Some(host) = uri.host() else {
        return respond_status_line(&mut client, StatusCode::BAD_REQUEST).await;
    };
    let port = uri.port_u16().unwrap_or(80);
    let mut upstream = match TcpStream::connect((host, port)).await {
        Ok(stream) => stream,
        Err(err) => {
            log::warn!("plain-HTTP forward to {host}:{port} failed: {err}");
            return respond_status_line(&mut client, StatusCode::BAD_GATEWAY).await;
        }
    };
    // Replay the buffered request head so the upstream receives a complete,
    // unchanged request (the socket only held the remainder after the head).
    if let Err(err) = upstream.write_all(&head.raw).await {
        log::warn!("plain-HTTP forward to {host}:{port} failed writing head: {err}");
        return Ok(());
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

/// MITM path: TLS-accept the client with a freshly minted leaf for `host`, then
/// run a hyper server connection whose service rewrites and forwards each
/// request to the upstream over a fresh client connection (spec §5/§8).
async fn mitm(
    client: TcpStream,
    host: &str,
    cfg: &ResolvedConfig,
    ca: &CertAuthority,
) -> Result<(), Report<ProxyError>> {
    let server_config = ca.server_config(host).change_context(ProxyError::Server)?;
    let acceptor = TlsAcceptor::from(server_config);
    let tls = acceptor
        .accept(client)
        .await
        .change_context(ProxyError::Server)?;

    let host = host.to_string();
    let log_host = host.clone();
    let service = service_fn(move |req: Request<Incoming>| {
        // Clone the per-request inputs into the future.
        let host = host.clone();
        let rules = cfg.rules.clone();
        let basic_auth = cfg.basic_auth.clone();
        let insecure = cfg.insecure;
        async move { forward_request(req, &host, &rules, basic_auth.as_ref(), insecure).await }
    });

    // serve_connection drives keep-alive: many sequential requests per tunnel.
    if let Err(err) = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(tls), service)
        .await
    {
        log::debug!("MITM connection for {log_host} ended: {err}");
    }
    Ok(())
}

/// Rewrites one decrypted request and forwards it to the upstream.
///
/// This is infallible at the hyper layer — upstream errors become a `502` so
/// the keep-alive tunnel survives a single bad request (spec §11).
async fn forward_request(
    req: Request<Incoming>,
    connect_host: &str,
    rules: &super::rewrite::RuleTable,
    basic_auth: Option<&super::config::BasicAuth>,
    insecure: bool,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Report<ProxyError>> {
    if req.headers().contains_key(hyper::header::UPGRADE) {
        log::info!("closing tunnel for {connect_host}: Upgrade (WebSocket) is out of scope");
        return Ok(status_response(StatusCode::NOT_IMPLEMENTED));
    }

    let Some(rule) = rules.first_match(connect_host) else {
        // Should not happen: MITM is only entered on a match.
        return Ok(status_response(StatusCode::BAD_GATEWAY));
    };
    let outcome = rewrite_for(rule);
    let upstream_host = rule.to.host().to_string();
    let upstream_port = rule.to.port;

    match proxy_to_upstream(
        req,
        &outcome,
        basic_auth,
        insecure,
        &upstream_host,
        upstream_port,
    )
    .await
    {
        Ok(response) => Ok(response),
        Err(err) => {
            log::warn!("upstream {upstream_host}:{upstream_port} failed: {err:?}");
            Ok(status_response(StatusCode::BAD_GATEWAY))
        }
    }
}

async fn proxy_to_upstream(
    mut req: Request<Incoming>,
    outcome: &super::rewrite::RewriteOutcome,
    basic_auth: Option<&super::config::BasicAuth>,
    insecure: bool,
    upstream_host: &str,
    upstream_port: u16,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Report<ProxyError>> {
    log::debug!(
        "{} {} -> {}:{} (Host={}, X-Orig-Host={})",
        req.method(),
        redact_target(req.uri()),
        upstream_host,
        upstream_port,
        outcome.host_header,
        outcome.orig_host,
    );

    rewrite_headers(req.headers_mut(), outcome, basic_auth);

    let tcp = TcpStream::connect((upstream_host, upstream_port))
        .await
        .change_context(ProxyError::Server)?;

    let response = if outcome.scheme_is_tls {
        let connector = TlsConnector::from(client_config(insecure));
        let server_name =
            ServerName::try_from(outcome.sni.clone()).change_context(ProxyError::Server)?;
        let tls = connector
            .connect(server_name, tcp)
            .await
            .change_context(ProxyError::Server)?;
        send_over(TokioIo::new(tls), req).await?
    } else {
        send_over(TokioIo::new(tcp), req).await?
    };

    Ok(response.map(|body| body.boxed()))
}

/// Drives one HTTP/1.1 request/response over an established (TLS or plain) IO.
async fn send_over<I>(
    io: I,
    req: Request<Incoming>,
) -> Result<Response<Incoming>, Report<ProxyError>>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .change_context(ProxyError::Server)?;
    tokio::spawn(async move {
        if let Err(err) = conn.await {
            log::debug!("upstream connection closed: {err}");
        }
    });
    sender
        .send_request(req)
        .await
        .change_context(ProxyError::Server)
}

/// Applies the rewrite outcome: upstream `Host`, `X-Orig-Host`, and (only when
/// absent) the injected `Authorization`. The request URI is left origin-form,
/// which is what an HTTP/1.1 upstream expects.
fn rewrite_headers(
    headers: &mut hyper::HeaderMap,
    outcome: &super::rewrite::RewriteOutcome,
    basic_auth: Option<&super::config::BasicAuth>,
) {
    if let Ok(value) = HeaderValue::from_str(&outcome.host_header) {
        headers.insert(hyper::header::HOST, value);
    }
    if let Ok(value) = HeaderValue::from_str(&outcome.orig_host) {
        headers.insert(HeaderName::from_static(X_ORIG_HOST), value);
    }
    if let Some(auth) = basic_auth
        && !headers.contains_key(hyper::header::AUTHORIZATION)
        && let Ok(value) = HeaderValue::from_str(&auth.header_value())
    {
        headers.insert(hyper::header::AUTHORIZATION, value);
    }
}

/// Builds a rustls client config: a no-verification verifier when `insecure`,
/// otherwise the bundled webpki roots.
fn client_config(insecure: bool) -> Arc<rustls::ClientConfig> {
    let config = if insecure {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(insecure::NoVerifier))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    let mut config = config;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

fn status_response(status: StatusCode) -> Response<BoxBody<Bytes, hyper::Error>> {
    let body = Full::new(Bytes::new())
        .map_err(|never| match never {})
        .boxed();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
}

/// Renders the request target without exposing credentials in query strings.
fn redact_target(uri: &Uri) -> String {
    uri.path().to_string()
}

mod insecure {
    use rustls::DigitallySignedStruct;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

    /// A verifier that accepts any upstream certificate — only used under
    /// `--insecure` for local development against self-signed origins.
    #[derive(Debug)]
    pub struct NoVerifier;

    impl ServerCertVerifier for NoVerifier {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::aws_lc_rs::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
}
