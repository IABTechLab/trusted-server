//! Shared fixtures for the proxy end-to-end tests: a self-signed TLS upstream,
//! a dev CA in a tempdir, and proxy-aware clients built on tokio + tokio-rustls.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use trusted_server_cli::commands::dev::proxy::{ca, config, server};

/// The production hostname the matched rule rewrites from (and preserves).
pub const FROM_HOST: &str = "www.example-publisher.com";

/// What the echo upstream reports back to the test.
pub struct ProxiedResponse {
    pub status: u16,
    pub seen_host: String,
    pub seen_orig_host: String,
    pub seen_forwarded_host: String,
    pub path: String,
}

/// A running upstream and the loopback address it bound.
pub struct Upstream {
    pub addr: SocketAddr,
    counters: Arc<UpstreamCounters>,
}

impl Upstream {
    #[must_use]
    pub fn snapshot(&self) -> UpstreamSnapshot {
        UpstreamSnapshot {
            accepted_connections: self.counters.accepted_connections.load(Ordering::Relaxed),
            tls_handshakes: self.counters.tls_handshakes.load(Ordering::Relaxed),
            requests: self.counters.requests.load(Ordering::Relaxed),
            failures: self.counters.failures.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct UpstreamSnapshot {
    pub accepted_connections: u64,
    pub tls_handshakes: u64,
    pub requests: u64,
    pub failures: u64,
}

#[derive(Default)]
struct UpstreamCounters {
    accepted_connections: AtomicU64,
    tls_handshakes: AtomicU64,
    requests: AtomicU64,
    failures: AtomicU64,
}

/// The leaf certificate the client observed at the end of a tunnel.
pub struct ObservedCert {
    pub issuer_common_name: String,
}

/// A dev [`CertAuthority`] generated in a fresh tempdir.
///
/// The tempdir is leaked so the CA files outlive the test (they are tiny and
/// the test process is short-lived).
pub fn dev_ca() -> ca::CertAuthority {
    let dir = tempfile::tempdir().expect("should create tempdir");
    let ca = ca::CertAuthority::load_or_generate(dir.path()).expect("should generate dev CA");
    // Keep the directory alive for the duration of the process.
    std::mem::forget(dir);
    ca
}

/// Builds a config mapping [`FROM_HOST`] to the upstream `addr`, preserving the
/// FROM host, listening on an ephemeral loopback port, with `insecure = true`.
pub fn test_config(addr: &SocketAddr) -> config::ResolvedConfig {
    let map = format!("{FROM_HOST}={}", addr);
    resolve(&["ts", "--map", &map, "--listen", "127.0.0.1:0", "--insecure"])
}

/// Uses a real DNS identity (`localhost`) rather than an IP-literal TO.
pub fn test_config_dns(addr: &SocketAddr) -> config::ResolvedConfig {
    let map = format!("{FROM_HOST}=localhost:{}", addr.port());
    resolve(&["ts", "--map", &map, "--listen", "127.0.0.1:0", "--insecure"])
}

/// Like [`test_config`] but with `--rewrite-host`, so the upstream sees
/// `Host: <TO>` while `X-Forwarded-Host` stays `FROM`.
pub fn test_config_rewrite_host(addr: &SocketAddr) -> config::ResolvedConfig {
    let map = format!("{FROM_HOST}={}", addr);
    resolve(&[
        "ts",
        "--map",
        &map,
        "--rewrite-host",
        "--listen",
        "127.0.0.1:0",
        "--insecure",
    ])
}

/// Builds a config whose TO host is a **non-resolvable** name (`pinned.invalid`)
/// pinned to the upstream `addr` via `--resolve`. The request only reaches the
/// upstream if the pin is honored — DNS for `.invalid` never resolves.
pub fn test_config_with_resolve(addr: &SocketAddr) -> config::ResolvedConfig {
    let map = format!("{FROM_HOST}=pinned.invalid:{}", addr.port());
    let pin = format!("pinned.invalid:{}", addr.ip());
    resolve(&[
        "ts",
        "--map",
        &map,
        "--resolve",
        &pin,
        "--listen",
        "127.0.0.1:0",
        "--insecure",
    ])
}

/// A config with no rewrite rules (every CONNECT is unmatched), on loopback.
pub fn test_config_without_rules() -> config::ResolvedConfig {
    // resolve() rejects an empty rule table, so map an unrelated host the tests
    // never CONNECT to. The host under test stays unmatched → blind tunnel.
    resolve(&[
        "ts",
        "--map",
        "unused.example.com=127.0.0.1:1",
        "--listen",
        "127.0.0.1:0",
        "--insecure",
    ])
}

fn resolve(argv: &[&str]) -> config::ResolvedConfig {
    use clap::Parser as _;
    #[derive(clap::Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: trusted_server_cli::commands::dev::proxy::ProxyArgs,
    }
    let parsed = Wrapper::parse_from(argv);
    config::resolve(&parsed.args).expect("should resolve test config")
}

// ---- self-signed upstream certificate (CN/SAN upstream.localhost) ----

fn upstream_identity() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    use rcgen::{CertificateParams, DnType, KeyPair, SanType};

    let key_pair = KeyPair::generate().expect("should generate upstream key");
    let mut params =
        CertificateParams::new(Vec::<String>::new()).expect("should build cert params");
    // Subject == issuer for a self-signed cert; the test asserts on issuer CN.
    params
        .distinguished_name
        .push(DnType::CommonName, "upstream.localhost");
    params.subject_alt_names = vec![
        SanType::DnsName("upstream.localhost".try_into().expect("dns san")),
        SanType::DnsName("localhost".try_into().expect("dns san")),
        SanType::IpAddress("127.0.0.1".parse().expect("ip san")),
    ];
    let cert = params
        .self_signed(&key_pair)
        .expect("should self-sign upstream cert");
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der =
        PrivateKeyDer::try_from(key_pair.serialize_der()).expect("should encode upstream key");
    (vec![cert_der], key_der)
}

fn upstream_tls_acceptor() -> TlsAcceptor {
    let (chain, key) = upstream_identity();
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("should build upstream server config");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsAcceptor::from(Arc::new(config))
}

/// Starts an HTTPS upstream that echoes the `Host`/`X-Orig-Host`/path it saw and
/// always returns `200`. Serves keep-alive (many requests per connection).
pub async fn start_echo_upstream() -> Upstream {
    start_upstream(false, Duration::ZERO, false).await
}

/// Starts the echo upstream with a fixed delay before every response.
pub async fn start_delayed_echo_upstream(response_delay: Duration) -> Upstream {
    start_upstream(false, response_delay, false).await
}

/// Starts an upstream that requests connection closure after every response.
pub async fn start_closing_upstream() -> Upstream {
    start_upstream(false, Duration::ZERO, true).await
}

/// Starts an HTTPS upstream that returns `401` unless an `Authorization` header
/// is present, otherwise `200`.
pub async fn start_gated_upstream() -> Upstream {
    start_upstream(true, Duration::ZERO, false).await
}

async fn start_upstream(gated: bool, response_delay: Duration, close: bool) -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind upstream");
    let addr = listener.local_addr().expect("should read upstream addr");
    let acceptor = upstream_tls_acceptor();
    let counters = Arc::new(UpstreamCounters::default());
    let task_counters = Arc::clone(&counters);
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            task_counters
                .accepted_connections
                .fetch_add(1, Ordering::Relaxed);
            let acceptor = acceptor.clone();
            let counters = Arc::clone(&task_counters);
            tokio::spawn(async move {
                let mut tls = match acceptor.accept(tcp).await {
                    Ok(tls) => {
                        counters.tls_handshakes.fetch_add(1, Ordering::Relaxed);
                        tls
                    }
                    Err(_) => {
                        counters.failures.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                };
                serve_upstream_connection(&mut tls, gated, response_delay, close, &counters).await;
            });
        }
    });
    Upstream { addr, counters }
}

/// Minimal HTTP/1.1 keep-alive loop: parse each request head, echo the headers
/// the test cares about, respond, repeat until the peer closes.
async fn serve_upstream_connection<S>(
    stream: &mut S,
    gated: bool,
    response_delay: Duration,
    close: bool,
    counters: &UpstreamCounters,
) where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        // Read until we have a full header block.
        let head_end = loop {
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
            let n = match stream.read(&mut chunk).await {
                Ok(0) => return,
                Ok(n) => n,
                Err(_) => {
                    counters.failures.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            buf.extend_from_slice(&chunk[..n]);
        };
        counters.requests.fetch_add(1, Ordering::Relaxed);
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        buf.drain(..head_end);

        let path = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        let host = header_value(&head, "host").unwrap_or_default();
        let orig_host = header_value(&head, "x-orig-host").unwrap_or_default();
        let fwd_host = header_value(&head, "x-forwarded-host").unwrap_or_default();
        let has_auth = header_value(&head, "authorization").is_some();

        let (status_line, body) = if gated && !has_auth {
            ("HTTP/1.1 401 Unauthorized", String::new())
        } else {
            let body = format!("host={host};orig={orig_host};fwd={fwd_host};path={path}");
            ("HTTP/1.1 200 OK", body)
        };
        if !response_delay.is_zero() {
            tokio::time::sleep(response_delay).await;
        }
        let connection = if close { "close" } else { "keep-alive" };
        let response = format!(
            "{status_line}\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n{body}",
            body.len()
        );
        if stream.write_all(response.as_bytes()).await.is_err() {
            counters.failures.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if stream.flush().await.is_err() {
            counters.failures.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if close {
            return;
        }
    }
}

fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---- proxy lifecycle ----

/// Spawns the proxy in the background and returns the loopback address it bound.
pub async fn spawn_proxy(cfg: config::ResolvedConfig, ca: Arc<ca::CertAuthority>) -> SocketAddr {
    spawn_proxy_with_state(cfg, ca).await.0
}

/// Spawns the proxy and returns its shared state for deterministic metric gates.
pub async fn spawn_proxy_with_state(
    cfg: config::ResolvedConfig,
    ca: Arc<ca::CertAuthority>,
) -> (
    SocketAddr,
    Arc<trusted_server_cli::commands::dev::proxy::ProxyState>,
) {
    let listener = server::bind(cfg.listen)
        .await
        .expect("should bind proxy listener");
    let addr = listener.local_addr().expect("should read proxy addr");
    let cfg = Arc::new(cfg);
    let state = trusted_server_cli::commands::dev::proxy::ProxyState::new(cfg);
    let pac: Arc<str> = Arc::from("function FindProxyForURL(u, h) { return \"DIRECT\"; }");
    let task_state = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = server::serve_on_with_state(listener, task_state, ca, pac).await;
    });
    (addr, state)
}

/// Spawns a proxy that behaves as if it were bound on a non-loopback address,
/// while the actual socket is on loopback so the test can connect without
/// privilege.
///
/// The trick: bind the listener on `127.0.0.1:0` (real socket), then patch
/// `cfg.listen` to `0.0.0.0:<port>` before handing it to `serve_on`. The
/// server derives `is_loopback = false` from `cfg.listen`, so CONNECT requests
/// to unmatched authorities are refused with `403` instead of blind-tunnelled.
pub async fn spawn_proxy_as_non_loopback(
    mut cfg: config::ResolvedConfig,
    ca: Arc<ca::CertAuthority>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind proxy listener on loopback");
    let real_port = listener
        .local_addr()
        .expect("should read proxy addr")
        .port();
    // Override listen so is_loopback computed in serve_on is false.
    cfg.listen = format!("0.0.0.0:{real_port}")
        .parse()
        .expect("should parse non-loopback socket addr");
    let connect_addr: SocketAddr = format!("127.0.0.1:{real_port}")
        .parse()
        .expect("should parse loopback connect addr");
    let cfg = Arc::new(cfg);
    let pac: Arc<str> = Arc::from("function FindProxyForURL(u, h) { return \"DIRECT\"; }");
    tokio::spawn(async move {
        let _ = server::serve_on(listener, cfg, ca, pac).await;
    });
    connect_addr
}

/// Sends a `CONNECT` request to `proxy` and returns the status line received.
/// Unlike `proxy_connect`, this does not assert on the status — it just returns
/// it so callers can check for `403` or other rejection codes.
pub async fn connect_and_read_status(proxy: SocketAddr, authority: &str) -> String {
    let mut stream = TcpStream::connect(proxy)
        .await
        .expect("should connect to proxy");
    let request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("should send CONNECT");
    stream.flush().await.expect("should flush CONNECT");
    read_status_line(&mut stream).await
}

// ---- client legs: a no-verify verifier so the test can trust either CA ----

#[derive(Debug)]
struct AcceptAny;

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
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

fn accept_any_connector() -> TlsConnector {
    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(config))
}

/// Sends `CONNECT host:port` to the proxy and reads its status line.
async fn proxy_connect(proxy: SocketAddr, authority: &str) -> TcpStream {
    let mut stream = TcpStream::connect(proxy)
        .await
        .expect("should connect to proxy");
    let request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("should send CONNECT");
    stream.flush().await.expect("should flush CONNECT");
    let status = read_status_line(&mut stream).await;
    assert!(
        status.contains(" 200 "),
        "proxy should accept CONNECT, got: {status}"
    );
    stream
}

/// Reads bytes until the end of the response head and returns its first line.
async fn read_status_line(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await.expect("should read status");
        if n == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buf)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Issues a single GET through the proxy (CONNECT to `FROM_HOST`, MITM, request
/// `/`) and parses the upstream echo.
pub async fn drive_request_through_proxy(
    cfg: config::ResolvedConfig,
    ca: Arc<ca::CertAuthority>,
) -> ProxiedResponse {
    let responses = drive_sequential_requests(cfg, ca, &["/"]).await;
    responses
        .into_iter()
        .next()
        .expect("should get one response")
}

/// Issues several GETs over ONE keep-alive MITM tunnel and returns them in order.
pub async fn drive_sequential_requests(
    cfg: config::ResolvedConfig,
    ca: Arc<ca::CertAuthority>,
    paths: &[&str],
) -> Vec<ProxiedResponse> {
    let proxy = spawn_proxy(cfg, ca).await;
    drive_sequential_requests_through_proxy(proxy, paths).await
}

/// Issues several GETs over one keep-alive MITM tunnel to an existing proxy.
pub async fn drive_sequential_requests_through_proxy(
    proxy: SocketAddr,
    paths: &[&str],
) -> Vec<ProxiedResponse> {
    let authority = format!("{FROM_HOST}:443");
    let tcp = proxy_connect(proxy, &authority).await;

    let connector = accept_any_connector();
    let server_name = ServerName::try_from(FROM_HOST.to_string()).expect("valid server name");
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .expect("client TLS handshake with proxy leaf");

    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {FROM_HOST}\r\nConnection: keep-alive\r\n\r\n");
        tls.write_all(request.as_bytes())
            .await
            .expect("should send request over tunnel");
        tls.flush().await.expect("should flush request");
        results.push(read_http_response(&mut tls).await);
    }
    results
}

/// CONNECTs to the mapped [`FROM_HOST`] (so the tunnel is MITM'd), then sends a
/// single `GET /` over it carrying an arbitrary `Host` header, and returns the
/// response status. Used to prove a `Host` that matches no rule is refused with
/// `421` rather than rerouted through the CONNECT-authority rule (spec §8.2).
pub async fn drive_request_with_host_header(
    cfg: config::ResolvedConfig,
    ca: Arc<ca::CertAuthority>,
    host_header: &str,
) -> u16 {
    let proxy = spawn_proxy(cfg, ca).await;
    let authority = format!("{FROM_HOST}:443");
    let tcp = proxy_connect(proxy, &authority).await;

    let connector = accept_any_connector();
    let server_name = ServerName::try_from(FROM_HOST.to_string()).expect("valid server name");
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .expect("client TLS handshake with proxy leaf");

    let request =
        format!("GET / HTTP/1.1\r\nHost: {host_header}\r\nConnection: keep-alive\r\n\r\n");
    tls.write_all(request.as_bytes())
        .await
        .expect("should send request over tunnel");
    tls.flush().await.expect("should flush request");
    read_http_response(&mut tls).await.status
}

/// Reads one HTTP/1.1 response (head + Content-Length body) and parses the echo.
async fn read_http_response<S>(stream: &mut S) -> ProxiedResponse
where
    S: AsyncReadExt + Unpin,
{
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream
            .read(&mut chunk)
            .await
            .expect("should read response head");
        assert!(n > 0, "upstream closed before sending a response");
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("should parse status code");
    let content_length: usize = header_value(&head, "content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).await.expect("should read body");
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&body[..content_length.min(body.len())]).to_string();
    let (seen_host, seen_orig_host, seen_forwarded_host, path) = parse_echo(&body);
    ProxiedResponse {
        status,
        seen_host,
        seen_orig_host,
        seen_forwarded_host,
        path,
    }
}

/// Parses `host=..;orig=..;fwd=..;path=..` echoed by the upstream.
fn parse_echo(body: &str) -> (String, String, String, String) {
    let mut host = String::new();
    let mut orig = String::new();
    let mut fwd = String::new();
    let mut path = String::new();
    for field in body.split(';') {
        if let Some(v) = field.strip_prefix("host=") {
            host = v.to_string();
        } else if let Some(v) = field.strip_prefix("orig=") {
            orig = v.to_string();
        } else if let Some(v) = field.strip_prefix("fwd=") {
            fwd = v.to_string();
        } else if let Some(v) = field.strip_prefix("path=") {
            path = v.to_string();
        }
    }
    (host, orig, fwd, path)
}

/// CONNECTs through the proxy to an UNMATCHED authority, completes the TLS
/// handshake, and returns the issuer CN of the leaf the client received — used
/// to prove a blind tunnel presents the upstream cert (not the dev CA leaf).
pub async fn connect_through_proxy_capturing_cert(
    cfg: config::ResolvedConfig,
    ca: Arc<ca::CertAuthority>,
    upstream: &SocketAddr,
    sni: &str,
) -> ObservedCert {
    let proxy = spawn_proxy(cfg, ca).await;
    // CONNECT to the upstream's real loopback authority (no rule matches it),
    // so the proxy blind-tunnels straight to it.
    let authority = format!("{}:{}", upstream.ip(), upstream.port());
    let tcp = proxy_connect(proxy, &authority).await;

    let captured = Arc::new(std::sync::Mutex::new(None));
    let connector = capturing_connector(Arc::clone(&captured));
    let server_name = ServerName::try_from(sni.to_string()).expect("valid sni");
    let _ = connector
        .connect(server_name, tcp)
        .await
        .expect("client TLS handshake with upstream through blind tunnel");

    let issuer_common_name = captured
        .lock()
        .expect("lock")
        .clone()
        .expect("verifier captured a leaf certificate");
    ObservedCert { issuer_common_name }
}

/// A verifier that records the issuer CN of the presented leaf, then accepts it.
#[derive(Debug)]
struct CapturingVerifier {
    issuer_common_name: Arc<std::sync::Mutex<Option<String>>>,
}

impl ServerCertVerifier for CapturingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Some(cn) = issuer_cn(end_entity) {
            *self.issuer_common_name.lock().expect("lock") = Some(cn);
        }
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

fn capturing_connector(slot: Arc<std::sync::Mutex<Option<String>>>) -> TlsConnector {
    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(CapturingVerifier {
            issuer_common_name: slot,
        }))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(config))
}

/// Extracts the issuer CN from a DER certificate (self-signed leaf → its own CN).
fn issuer_cn(cert: &CertificateDer<'_>) -> Option<String> {
    use x509_parser::prelude::FromDer as _;
    let (_, parsed) = x509_parser::certificate::X509Certificate::from_der(cert.as_ref()).ok()?;
    parsed
        .issuer()
        .iter_common_name()
        .next()
        .and_then(|attr| attr.as_str().ok())
        .map(str::to_string)
}
