//! Accept loop, CONNECT dispatch, blind tunnel, MITM, and local routes (spec §5).
//!
//! Each accepted connection's first request line decides the path:
//! a `CONNECT host:port` is matched against [`ResolvedConfig::rules`] *before*
//! replying — a match is MITM'd (a leaf is minted, the TLS stream is decrypted
//! and proxied request-by-request); a non-match is blind-tunnelled on loopback
//! or refused (`403`) off loopback. An origin-form `GET /proxy.pac` is served
//! locally.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use error_stack::{Report, ResultExt as _};
use http_body_util::{BodyExt as _, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{HeaderName, HeaderValue};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

use super::ca::CertAuthority;
use super::config::ResolvedConfig;
use super::prefixed_io::PrefixedIo;
use super::rewrite::rewrite_for;
use super::{ProxyError, ProxyState};

const X_ORIG_HOST: &str = "x-orig-host";
const X_FORWARDED_HOST: &str = "x-forwarded-host";
const X_FORWARDED_PROTO: &str = "x-forwarded-proto";

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
    serve_on_with_state(listener, ProxyState::new(cfg), ca, pac).await
}

/// Serves with process-shared upstream pooling and metrics state.
///
/// # Errors
///
/// Returns a server error if the accept loop cannot continue.
pub async fn serve_on_with_state(
    listener: TcpListener,
    state: Arc<ProxyState>,
    ca: Arc<CertAuthority>,
    pac: Arc<str>,
) -> Result<(), Report<ProxyError>> {
    let is_loopback = is_loopback(state.config.listen.ip());
    log::info!("listening on {}", state.config.listen);
    for (host, ip) in &state.config.resolve {
        log::info!("--resolve pin: {host} -> {ip}");
    }
    loop {
        let (client, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                log::warn!("accept failed: {err}");
                continue;
            }
        };
        state.metrics.record_browser_connection();
        let state = Arc::clone(&state);
        let ca = Arc::clone(&ca);
        let pac = Arc::clone(&pac);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(client, is_loopback, &state, &ca, &pac).await {
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
    /// Whether the header terminator (`\r\n\r\n`) was seen before EOF or the read
    /// cap. `false` means a truncated or oversized head that must be rejected
    /// with `400` rather than routed from a partially-read request.
    complete: bool,
    /// Bytes read beyond the header terminator, replayed to the selected path.
    prefix: Vec<u8>,
}

impl RequestHead {
    /// `Some(host:port)` when the request is a `CONNECT`.
    fn connect_authority(&self) -> Option<&str> {
        (self.method.eq_ignore_ascii_case("CONNECT")).then_some(self.target.as_str())
    }

    /// Whether this is the local `GET /proxy.pac` route.
    ///
    /// Matches **origin-form only** (`target == "/proxy.pac"`); an absolute-form
    /// `http://host/proxy.pac` is proxy traffic, not the local route, so it
    /// falls through to blind-forward (spec §8.4).
    fn is_local_pac_route(&self) -> bool {
        self.method.eq_ignore_ascii_case("GET") && self.target == "/proxy.pac"
    }
}

/// Reads bytes until the end of the request head (`\r\n\r\n`) and parses
/// method/target from the first request line.
///
/// The raw bytes are retained on the returned [`RequestHead`] so that a stray
/// absolute-form plain-HTTP request can be forwarded unchanged (spec §8.4) —
/// `blind_forward_http` writes them to the upstream before piping the remainder.
async fn read_request_head<R: AsyncRead + Unpin>(
    client: &mut R,
) -> Result<RequestHead, Report<ProxyError>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let mut complete = false;
    let mut head_end = 0;
    loop {
        let n = client
            .read(&mut chunk)
            .await
            .change_context(ProxyError::Server)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(position) = find_bytes(&buf, b"\r\n\r\n") {
            head_end = position + 4;
            complete = head_end <= 8192;
            break;
        }
        // Oversized head: stop reading, but mark it incomplete so the caller
        // rejects it rather than routing a partially-read request.
        if buf.len() > 8192 {
            break;
        }
    }
    if !complete {
        head_end = buf.len();
    }
    let prefix = buf.split_off(head_end);
    let text = String::from_utf8_lossy(&buf);
    let first_line = text.lines().next().unwrap_or_default();
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    Ok(RequestHead {
        method,
        target,
        raw: buf,
        complete,
        prefix,
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn handle_connection(
    mut client: TcpStream,
    is_loopback: bool,
    state: &ProxyState,
    ca: &CertAuthority,
    pac: &str,
) -> Result<(), Report<ProxyError>> {
    let parse_started = tokio::time::Instant::now();
    let head = read_request_head(&mut client).await?;
    let mut client = PrefixedIo::new(client, head.prefix.clone());
    // A truncated or oversized head (no `\r\n\r\n` within the cap) is malformed —
    // reject it cleanly instead of routing a partially-parsed request.
    if !head.complete {
        state
            .metrics
            .record_initial_head_rejected(parse_started.elapsed());
        return respond_status_line(&mut client, StatusCode::BAD_REQUEST).await;
    }
    state
        .metrics
        .record_initial_head_parsed(parse_started.elapsed());
    if let Some(authority) = head.connect_authority() {
        let authority = authority.to_string();
        return handle_connect(client, &authority, is_loopback, state, ca).await;
    }
    if head.is_local_pac_route() {
        return serve_pac(&mut client, pac).await;
    }
    // Stray absolute-form plain HTTP.
    if is_loopback {
        blind_forward_http(
            client,
            &head,
            &state.config.resolve,
            state.config.connect_timeout,
        )
        .await
    } else {
        respond_status_line(&mut client, StatusCode::FORBIDDEN).await
    }
}

/// Splits `host:port`, defaulting the port to 443 when absent.
///
/// Returns `None` when a port is present but not a valid `u16`, so the caller
/// can reject the CONNECT with `400` instead of silently dialing `443`.
fn split_authority(authority: &str) -> Option<(String, u16)> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, suffix) = bracketed.split_once(']')?;
        let port = if suffix.is_empty() {
            443
        } else {
            suffix.strip_prefix(':')?.parse().ok()?
        };
        return Some((host.to_string(), port));
    }
    if authority.parse::<IpAddr>().is_ok() {
        return Some((authority.to_string(), 443));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), 443)),
    }
}

async fn handle_connect(
    mut client: PrefixedIo<TcpStream>,
    authority: &str,
    is_loopback: bool,
    state: &ProxyState,
    ca: &CertAuthority,
) -> Result<(), Report<ProxyError>> {
    let Some((host, port)) = split_authority(authority) else {
        return respond_status_line(&mut client, StatusCode::BAD_REQUEST).await;
    };

    // Match BEFORE replying, so an unmatched non-loopback request is refused.
    if state.config.rules.first_match(&host).is_some() {
        write_connect_ok(&mut client).await?;
        return mitm(client, &host, state, ca).await;
    }

    if !is_loopback {
        log::warn!("refusing un-mapped CONNECT {host} off loopback");
        return respond_status_line(&mut client, StatusCode::FORBIDDEN).await;
    }

    // No match on loopback: connect upstream FIRST, then reply 200 (else 502).
    blind_tunnel(
        client,
        &host,
        port,
        &state.config.resolve,
        state.config.connect_timeout,
    )
    .await
}

/// Opens an upstream TCP connection, honoring a `--resolve` pin for `host`.
///
/// When `host` (matched case-insensitively) has a pin, the socket dials that IP
/// instead of resolving the name via DNS. The TLS SNI / `Host` set by the caller
/// are unaffected, so the certificate still validates against the hostname. The
/// dial is bounded by `connect_timeout` (from `--connect-timeout`) so a
/// black-holed upstream fails fast into the `502` path.
async fn connect_upstream(
    host: &str,
    port: u16,
    resolve: &HashMap<String, IpAddr>,
    connect_timeout: Duration,
) -> std::io::Result<TcpStream> {
    let dial = async {
        if !resolve.is_empty()
            && let Some(ip) = resolve.get(&host.to_ascii_lowercase())
        {
            log::debug!("--resolve {host}:{port} -> {ip}:{port}");
            return TcpStream::connect((*ip, port)).await;
        }
        TcpStream::connect((host, port)).await
    };
    match tokio::time::timeout(connect_timeout, dial).await {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("upstream connect to {host}:{port} timed out"),
        )),
    }
}

/// Connects to the upstream first; on success replies `200` then pipes bytes
/// in both directions without decrypting anything.
async fn blind_tunnel(
    mut client: PrefixedIo<TcpStream>,
    host: &str,
    port: u16,
    resolve: &HashMap<String, IpAddr>,
    connect_timeout: Duration,
) -> Result<(), Report<ProxyError>> {
    let mut upstream = match connect_upstream(host, port, resolve, connect_timeout).await {
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

async fn write_connect_ok(client: &mut PrefixedIo<TcpStream>) -> Result<(), Report<ProxyError>> {
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .change_context(ProxyError::Server)?;
    client.flush().await.change_context(ProxyError::Server)
}

async fn respond_status_line(
    client: &mut PrefixedIo<TcpStream>,
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

async fn serve_pac(
    client: &mut PrefixedIo<TcpStream>,
    pac: &str,
) -> Result<(), Report<ProxyError>> {
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
    mut client: PrefixedIo<TcpStream>,
    head: &RequestHead,
    resolve: &HashMap<String, IpAddr>,
    connect_timeout: Duration,
) -> Result<(), Report<ProxyError>> {
    let Ok(uri) = head.target.parse::<Uri>() else {
        return respond_status_line(&mut client, StatusCode::BAD_REQUEST).await;
    };
    let Some(host) = uri.host() else {
        return respond_status_line(&mut client, StatusCode::BAD_REQUEST).await;
    };
    let port = uri.port_u16().unwrap_or(80);
    let mut upstream = match connect_upstream(host, port, resolve, connect_timeout).await {
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
    client: PrefixedIo<TcpStream>,
    host: &str,
    state: &ProxyState,
    ca: &CertAuthority,
) -> Result<(), Report<ProxyError>> {
    let normalized_host = host.to_ascii_lowercase();
    let cached = ca.is_cached(&normalized_host);
    let mint_started = tokio::time::Instant::now();
    let server_config = ca
        .server_config(&normalized_host)
        .change_context(ProxyError::Server)?;
    if cached {
        state.metrics.record_ca_hit();
    } else {
        state.metrics.record_ca_miss(mint_started.elapsed(), true);
    }
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
        let state = state;
        async move { forward_request(req, &host, state).await }
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
/// Each request is routed by its own inbound `Host` (before any rewrite), so a
/// keep-alive tunnel that carries requests for several hosts routes each one
/// independently (spec §8.2). A request whose `Host` matches no rule is **not**
/// rerouted through the CONNECT-authority rule — it is refused with `421`
/// (Misdirected Request), so a client cannot `CONNECT mapped.example` then send
/// `Host: other.example` to smuggle traffic through a rule it never matched. The
/// CONNECT authority is consulted only when the request carries no `Host` at all.
///
/// This is infallible at the hyper layer — upstream errors become a `502` so
/// the keep-alive tunnel survives a single bad request (spec §11).
async fn forward_request(
    req: Request<Incoming>,
    connect_host: &str,
    state: &ProxyState,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Report<ProxyError>> {
    if req.headers().contains_key(hyper::header::UPGRADE) {
        log::info!("closing tunnel for {connect_host}: Upgrade (WebSocket) is out of scope");
        return Ok(status_response(StatusCode::NOT_IMPLEMENTED));
    }

    // Route by the request's own Host when present (spec §8.2). A Host that
    // matches no rule is refused (421) rather than rerouted through the CONNECT
    // authority. Only a request with no Host falls back to the CONNECT authority.
    let rule = match request_host(&req) {
        Some(host) => match state.config.rules.first_match(&host) {
            Some(rule) => rule,
            None => return Ok(status_response(StatusCode::MISDIRECTED_REQUEST)),
        },
        None => match state.config.rules.first_match(connect_host) {
            Some(rule) => rule,
            // Should not happen: MITM is only entered on a CONNECT-authority match.
            None => return Ok(status_response(StatusCode::BAD_GATEWAY)),
        },
    };
    let outcome = rewrite_for(rule);
    let upstream_host = rule.to.host();
    let upstream_port = rule.to.port;

    match proxy_to_upstream(
        req,
        outcome,
        state.config.basic_auth.as_ref(),
        rule,
        &state.upstream,
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

/// Extracts the inbound request host from the `Host` header (origin-form
/// requests over a MITM tunnel always carry one), else the URI authority.
///
/// Returned verbatim (including any `:port`); [`RuleTable::first_match`] strips
/// the port when matching.
fn request_host(req: &Request<Incoming>) -> Option<String> {
    if let Some(value) = req.headers().get(hyper::header::HOST)
        && let Ok(text) = value.to_str()
    {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    req.uri().host().map(str::to_string)
}

async fn proxy_to_upstream(
    mut req: Request<Incoming>,
    outcome: &super::rewrite::RewriteOutcome,
    basic_auth: Option<&super::config::BasicAuth>,
    rule: &super::rewrite::Rule,
    upstream: &super::upstream::UpstreamClient,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Report<ProxyError>> {
    let upstream_host = rule.to.host();
    let upstream_port = rule.to.port;
    log::debug!(
        "{} {} -> {}:{} (Host={}, X-Orig-Host={})",
        req.method(),
        redact_target(req.uri()),
        upstream_host,
        upstream_port,
        outcome
            .host_header
            .to_str()
            .expect("should prevalidate Host header"),
        outcome
            .orig_host
            .to_str()
            .expect("should prevalidate original host header"),
    );

    let metadata = super::upstream::RequestMetadata::capture(&req);
    rewrite_headers(req.headers_mut(), outcome, basic_auth);

    let mut response = upstream.send(req, metadata, rule, outcome).await?;

    // Strip hop-by-hop headers from the upstream response too. A `Connection: close`
    // (or a named connection token) is specific to the upstream leg and must not
    // leak onto the reusable browser↔proxy MITM tunnel and tear it down.
    let downstream_trailer = response.headers().get(hyper::header::TRAILER).cloned();
    strip_hop_by_hop(response.headers_mut());
    if let Some(trailer) = downstream_trailer {
        // Regenerate the trailer declaration for the downstream HTTP/1 leg so
        // Hyper serializes forwarded trailer frames. This is new downstream
        // framing metadata, not leaked upstream connection state.
        response
            .headers_mut()
            .insert(hyper::header::TRAILER, trailer);
    }
    Ok(response)
}

/// Removes RFC 7230 hop-by-hop request headers, plus every header named in an
/// inbound `Connection` token, before authoritative headers are stamped.
///
/// Without this, a client could send `Connection: X-Forwarded-Host, …` and any
/// downstream HTTP intermediary that honors hop-by-hop semantics would discard
/// exactly the headers this proxy relies on for host anchoring, scheme
/// preservation, or Basic-auth injection. Token-named headers are collected
/// before `Connection` itself is removed.
fn strip_hop_by_hop(headers: &mut hyper::HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(&name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

/// Applies the rewrite outcome: strips inbound hop-by-hop headers, then sets
/// upstream `Host`, `X-Forwarded-Host`/`X-Orig-Host` (both `FROM`, after
/// stripping any higher-priority inbound `Forwarded`), an authoritative
/// `X-Forwarded-Proto: https` (the browser leg is always TLS), and (only when
/// absent) the injected `Authorization`. The request URI is left origin-form,
/// which is what an HTTP/1.1 upstream expects.
fn rewrite_headers(
    headers: &mut hyper::HeaderMap,
    outcome: &super::rewrite::RewriteOutcome,
    basic_auth: Option<&super::config::BasicAuth>,
) {
    // Strip hop-by-hop headers first, so a client cannot flag the authoritative
    // headers we stamp below as connection-specific and have them dropped.
    strip_hop_by_hop(headers);
    headers.insert(hyper::header::HOST, outcome.host_header.clone());
    // Tell the upstream the original first-party host (always `FROM`). Trusted
    // Server resolves the request host from `Forwarded` → `X-Forwarded-Host` →
    // `Host`, so a client-supplied `Forwarded` would outrank the value we inject.
    // Remove it first so the `X-Forwarded-Host` we stamp is the one core reads,
    // aiming to keep emitted first-party URLs on the production host even when
    // `--rewrite-host` sends `Host: TO` for routing/validation (spec §8.3). NOTE:
    // this only holds if the upstream preserves `X-Forwarded-Host` — the real
    // Fastly/Spin adapter paths strip it before routing, in which case core falls
    // back to `Host` (`TO`). See the `--rewrite-host` caveat in the user guide.
    // The `insert`s below already overwrite any inbound `X-Forwarded-Host`/`X-Orig-Host`.
    headers.remove("forwarded");
    headers.insert(
        HeaderName::from_static(X_FORWARDED_HOST),
        outcome.orig_host.clone(),
    );
    headers.insert(
        HeaderName::from_static(X_ORIG_HOST),
        outcome.orig_host.clone(),
    );
    // The browser→proxy leg is always TLS, so the original scheme is `https`.
    // Stamp it authoritatively (overwriting any inbound value) and drop the
    // spoofable `Fastly-SSL` signal, so a plaintext upstream (`--upstream-plaintext`)
    // — or a spoofed header — cannot downgrade the first-party scheme Trusted
    // Server derives for its URL rewriting (spec §8.3).
    headers.insert(
        HeaderName::from_static(X_FORWARDED_PROTO),
        HeaderValue::from_static("https"),
    );
    headers.remove("fastly-ssl");
    if let Some(auth) = basic_auth
        && !headers.contains_key(hyper::header::AUTHORIZATION)
    {
        headers.insert(hyper::header::AUTHORIZATION, auth.header_value().clone());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dev::proxy::rewrite::RewriteOutcome;

    async fn parse_bytes(chunks: Vec<Vec<u8>>) -> RequestHead {
        let capacity = chunks.iter().map(Vec::len).sum::<usize>().max(1);
        let (mut writer, mut reader) = tokio::io::duplex(capacity);
        tokio::spawn(async move {
            for chunk in chunks {
                writer.write_all(&chunk).await.expect("should write chunk");
                tokio::task::yield_now().await;
            }
        });
        read_request_head(&mut reader)
            .await
            .expect("should parse head")
    }

    fn rewrite_outcome(host: &'static str) -> RewriteOutcome {
        RewriteOutcome {
            sni: Some(
                rustls::pki_types::ServerName::try_from("to.edgecompute.app")
                    .expect("should parse server name"),
            ),
            host_header: HeaderValue::from_static(host),
            orig_host: HeaderValue::from_static("www.example-publisher.com"),
            scheme_is_tls: true,
        }
    }

    fn head(method: &str, target: &str) -> RequestHead {
        RequestHead {
            method: method.to_string(),
            target: target.to_string(),
            raw: Vec::new(),
            complete: true,
            prefix: Vec::new(),
        }
    }

    #[test]
    fn local_pac_route_is_origin_form_get_only() {
        assert!(
            head("GET", "/proxy.pac").is_local_pac_route(),
            "origin-form GET /proxy.pac is the local route"
        );
        assert!(
            head("get", "/proxy.pac").is_local_pac_route(),
            "method match is case-insensitive"
        );
        assert!(
            !head("GET", "http://x.example.com/proxy.pac").is_local_pac_route(),
            "absolute-form /proxy.pac is proxy traffic, not the local route"
        );
        assert!(
            !head("POST", "/proxy.pac").is_local_pac_route(),
            "non-GET is never the local PAC route"
        );
    }

    #[test]
    fn split_authority_normalizes_bracketed_ipv6() {
        assert_eq!(
            split_authority("[::1]:8443"),
            Some(("::1".to_string(), 8443))
        );
        assert_eq!(split_authority("[::1]"), Some(("::1".to_string(), 443)));
    }

    #[tokio::test]
    async fn buffered_head_parser_handles_delimiter_splits_and_exact_overread() {
        let head = parse_bytes(vec![
            b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\n\r".to_vec(),
            b"\nclient-hello".to_vec(),
        ])
        .await;
        assert!(head.complete);
        assert_eq!(head.prefix, b"client-hello");
        assert_eq!(head.connect_authority(), Some("example.com:443"));
    }

    #[tokio::test]
    async fn buffered_head_parser_accepts_exactly_eight_kib_and_rejects_larger() {
        let mut exact = b"GET / HTTP/1.1\r\nX-Fill: ".to_vec();
        exact.resize(8192 - 4, b'a');
        exact.extend_from_slice(b"\r\n\r\n");
        assert!(parse_bytes(vec![exact]).await.complete);

        let mut oversized = b"GET / HTTP/1.1\r\nX-Fill: ".to_vec();
        oversized.resize(8192, b'a');
        oversized.extend_from_slice(b"\r\n\r\n");
        assert!(!parse_bytes(vec![oversized]).await.complete);
    }

    #[test]
    fn rewrite_headers_strips_proxy_connection() {
        let outcome = rewrite_outcome("www.example-publisher.com");
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            HeaderName::from_static("proxy-connection"),
            HeaderValue::from_static("keep-alive"),
        );
        rewrite_headers(&mut headers, &outcome, None);
        assert!(
            !headers.contains_key("proxy-connection"),
            "Proxy-Connection is a hop-by-hop header and must be removed"
        );
        assert_eq!(
            headers
                .get(hyper::header::HOST)
                .and_then(|v| v.to_str().ok()),
            Some("www.example-publisher.com"),
            "Host is still rewritten alongside the strip"
        );
    }

    #[test]
    fn request_metadata_captures_chunked_upload_before_sanitation() {
        let mut request = Request::new(Full::new(Bytes::from_static(b"upload")));
        *request.method_mut() = hyper::Method::POST;
        request.headers_mut().insert(
            hyper::header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );

        let metadata = super::super::upstream::RequestMetadata::capture(&request);
        rewrite_headers(
            request.headers_mut(),
            &rewrite_outcome("to.edgecompute.app"),
            None,
        );

        assert!(
            !request
                .headers()
                .contains_key(hyper::header::TRANSFER_ENCODING),
            "sanitation should still remove hop-by-hop framing"
        );
        assert!(
            !metadata.upload_initially_complete(),
            "chunked upload must remain streaming after sanitation"
        );
        assert!(
            !metadata.replayable(),
            "chunked upload must never enter stale replay"
        );
    }

    #[test]
    fn rewrite_headers_strips_inbound_forwarded_so_injected_host_wins() {
        // Trusted Server resolves the request host from `Forwarded` BEFORE
        // `X-Forwarded-Host`. A client-supplied `Forwarded` must therefore be
        // dropped, or it would outrank the FROM host the proxy injects.
        let outcome = rewrite_outcome("to.edgecompute.app");
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            HeaderName::from_static("forwarded"),
            HeaderValue::from_static("host=evil.example.com"),
        );
        rewrite_headers(&mut headers, &outcome, None);
        assert!(
            !headers.contains_key("forwarded"),
            "inbound Forwarded must be stripped so it cannot outrank X-Forwarded-Host"
        );
        assert_eq!(
            headers.get(X_FORWARDED_HOST).and_then(|v| v.to_str().ok()),
            Some("www.example-publisher.com"),
            "X-Forwarded-Host is the injected FROM host"
        );
    }

    #[test]
    fn rewrite_headers_stamps_https_scheme_and_drops_spoofed_signals() {
        // The browser leg is always TLS, so the scheme is authoritatively https;
        // a client-supplied X-Forwarded-Proto / Fastly-SSL must not downgrade it.
        let outcome = rewrite_outcome("www.example-publisher.com");
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            HeaderName::from_static(X_FORWARDED_PROTO),
            HeaderValue::from_static("http"),
        );
        headers.insert(
            HeaderName::from_static("fastly-ssl"),
            HeaderValue::from_static("0"),
        );
        rewrite_headers(&mut headers, &outcome, None);
        assert_eq!(
            headers.get(X_FORWARDED_PROTO).and_then(|v| v.to_str().ok()),
            Some("https"),
            "X-Forwarded-Proto is stamped https, overwriting the inbound http"
        );
        assert!(
            !headers.contains_key("fastly-ssl"),
            "spoofable Fastly-SSL is stripped"
        );
    }

    #[test]
    fn rewrite_headers_strips_connection_named_headers_but_keeps_injected() {
        // A client naming the proxy's own headers in `Connection` must not cause
        // them to be dropped downstream: we strip the client's copies + the
        // `Connection` header, then stamp our authoritative values.
        let outcome = rewrite_outcome("to.edgecompute.app");
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            hyper::header::CONNECTION,
            HeaderValue::from_static("x-forwarded-host, x-forwarded-proto, keep-alive"),
        );
        headers.insert(
            HeaderName::from_static(X_FORWARDED_HOST),
            HeaderValue::from_static("evil.example.com"),
        );
        headers.insert(
            HeaderName::from_static("keep-alive"),
            HeaderValue::from_static("timeout=5"),
        );
        rewrite_headers(&mut headers, &outcome, None);
        assert!(
            !headers.contains_key(hyper::header::CONNECTION),
            "inbound Connection is stripped"
        );
        assert!(
            !headers.contains_key("keep-alive"),
            "a Connection-named hop-by-hop header is stripped"
        );
        assert_eq!(
            headers.get(X_FORWARDED_HOST).and_then(|v| v.to_str().ok()),
            Some("www.example-publisher.com"),
            "the proxy-injected X-Forwarded-Host survives even though the client named it in Connection"
        );
        assert_eq!(
            headers.get(X_FORWARDED_PROTO).and_then(|v| v.to_str().ok()),
            Some("https"),
            "the proxy-injected X-Forwarded-Proto survives too"
        );
    }
}
