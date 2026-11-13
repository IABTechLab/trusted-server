use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use chacha20poly1305::{XChaCha20Poly1305, XNonce, aead::Aead as _, aead::KeyInit as _};
use edgezero_core::body::{Body as EdgeBody, BodyPayload};
use edgezero_core::error::EdgeError;
use edgezero_core::http::RequestParts;
use edgezero_core::ingress::{InboundOrigin, RequestIngress};
use error_stack::Report;
use http::{HeaderMap, Request, Response, StatusCode, header};
use sha2::{Digest as _, Sha256};
use std::net::Ipv6Addr;
use std::time::Duration;
use subtle::ConstantTimeEq as _;

use crate::cache_policy::{CachePolicy, EdgeCacheHeader};
use crate::constants::INTERNAL_HEADERS;
use crate::error::TrustedServerError;
use crate::forwarder::public_origin;
use crate::platform::ClientInfo;
use crate::settings::{Settings, TrustedClientIpConfig};

/// Copy `X-*` custom headers from one request to another, skipping TS-internal headers.
///
/// This filters out all headers listed in [`INTERNAL_HEADERS`] **and** any header
/// matching the `x-ts-` prefix (case-insensitive) to prevent leaking internal
/// identity, geo-enrichment, debugging data, and dynamic `X-ts-<source_domain>`
/// headers to downstream third-party services. Integrations that forward custom
/// headers should use this utility instead of manually iterating over header names.
pub fn copy_custom_headers(from: &Request<EdgeBody>, to: &mut Request<EdgeBody>) {
    for (header_name, value) in from.headers() {
        let name_str = header_name.as_str();
        // Header names are normalized by the HTTP library,
        // so only the lowercase prefix check is needed.
        if name_str.starts_with("x-")
            && !name_str.starts_with("x-ts-")
            && !INTERNAL_HEADERS.contains(&name_str)
        {
            to.headers_mut().append(header_name.clone(), value.clone());
        }
    }
}

/// Headers that clients can spoof to hijack URL rewriting or the client address.
///
/// On Fastly Compute these values are client-spoofable at request entry. The
/// Fastly adapter may first consume an authenticated `fastly-client-ip`, but
/// removes it before routing along with every other listed header. Stripping
/// them forces [`RequestInfo::from_request`] to fall back to the trustworthy
/// `Host` header or URI authority and [`ClientInfo`] TLS detection.
pub const SPOOFABLE_FORWARDED_HEADERS: &[&str] = &[
    "forwarded",
    "x-forwarded-host",
    "x-forwarded-proto",
    "fastly-ssl",
    "fastly-client-ip",
];

/// Remove the configured client-IP trust headers before routing.
///
/// Fastly request handling and trace metadata across adapters consume these
/// values before sanitation. Every adapter removes them so a shared configuration
/// cannot expose an authentication secret to publisher or integration handling.
pub fn sanitize_trusted_client_ip_headers(
    req: &mut RequestParts,
    config: Option<&TrustedClientIpConfig>,
) {
    if let Some(config) = config {
        req.headers.remove(config.ip_header.as_str());
        req.headers.remove(config.auth_header.as_str());
    }
}

/// Split a body into buffered bytes or a stream without releasing a streamed body's scope.
///
/// A streaming [`EdgeBody`] becomes a [`BodyPayload::Stream`] that keeps any
/// outbound scope owner and closes it at end of stream, on error, or on drop.
/// A buffered body becomes [`BodyPayload::Once`] and releases its owner,
/// because its bytes no longer depend on the scope. A streaming body is never
/// returned as empty bytes, unlike [`EdgeBody::into_bytes`].
#[must_use]
pub(crate) fn into_body_payload(body: EdgeBody) -> BodyPayload {
    match body.into_stream() {
        Ok(stream) => BodyPayload::Stream(stream),
        Err(buffered) => buffered.into_parts().0,
    }
}

/// Wraps a buffered body for a synchronous rewrite pipeline.
///
/// Callers buffer the responses they rewrite, so a streamed body here is a
/// bug. It fails instead of reading as empty. `pipeline` names the caller's
/// pipeline in the error, such as `"proxy"` or `"publisher"`.
///
/// # Errors
///
/// Returns [`TrustedServerError::Proxy`] for a streamed body.
pub(crate) fn body_as_reader(
    body: EdgeBody,
    pipeline: &str,
) -> Result<std::io::Cursor<Bytes>, Report<TrustedServerError>> {
    match into_body_payload(body) {
        BodyPayload::Once(bytes) => Ok(std::io::Cursor::new(bytes)),
        BodyPayload::Stream(_) => Err(Report::new(TrustedServerError::Proxy {
            message: format!("streaming body cannot be processed by the sync {pipeline} pipeline"),
        })),
    }
}

/// Read an inbound request body into memory, accepting at most `max` bytes.
///
/// When `headers` carry exactly one `Content-Length` field whose value is a
/// decimal number above `max`, the body is rejected without being polled.
/// Otherwise the body is read with [`EdgeBody::into_bytes_bounded`], which
/// stops at the first chunk that would cross `max`, so a duplicate or
/// malformed `Content-Length` never decides the outcome on its own. The
/// read also ends with an error once the inbound read deadline passes.
///
/// This function does not log. The report names the limit or carries the
/// read error, and [`http_error`](crate::error::http_error) logs it at the
/// level its status calls for. Body bytes never reach the report.
///
/// # Errors
///
/// - [`TrustedServerError::RequestTooLarge`] (413) when the declared length or
///   the bytes read exceed `max`.
/// - [`TrustedServerError::RequestTimeout`] (408) when the read passes the
///   inbound read deadline.
/// - [`TrustedServerError::BadRequest`] (400) for any other client error the
///   body stream reports.
/// - [`TrustedServerError::Proxy`] (502) when the host fails to read the body.
///
/// # Examples
///
/// ```
/// use edgezero_core::body::Body;
/// use http::{HeaderMap, HeaderValue, header};
/// use trusted_server_core::error::{IntoHttpResponse as _, TrustedServerError};
/// use trusted_server_core::http_util::read_request_body;
///
/// let bytes = futures::executor::block_on(read_request_body(
///     &HeaderMap::new(),
///     Body::from(r#"{"example":true}"#),
///     64,
/// ))
/// .expect("should read a body under the cap");
/// assert_eq!(bytes.as_ref(), br#"{"example":true}"#);
///
/// let mut headers = HeaderMap::new();
/// headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("65"));
/// let report = futures::executor::block_on(read_request_body(&headers, Body::empty(), 64))
///     .expect_err("should reject a declared length over the cap");
/// assert!(matches!(
///     report.current_context(),
///     TrustedServerError::RequestTooLarge { .. }
/// ));
/// assert_eq!(report.current_context().status_code().as_u16(), 413);
/// ```
pub async fn read_request_body(
    headers: &HeaderMap,
    body: EdgeBody,
    max: usize,
) -> Result<Bytes, Report<TrustedServerError>> {
    if let Some(declared) = single_declared_content_length(headers)
        && declared > u64::try_from(max).unwrap_or(u64::MAX)
    {
        return Err(Report::new(TrustedServerError::RequestTooLarge {
            message: format!("declared Content-Length {declared} exceeds the {max} byte limit"),
        }));
    }

    body.into_bytes_bounded(max)
        .await
        .map_err(|error| request_body_read_error(&error, max))
}

/// Returns the `Content-Length` when `headers` carry exactly one field whose
/// value is a decimal number that fits in a `u64`.
fn single_declared_content_length(headers: &HeaderMap) -> Option<u64> {
    let mut values = headers.get_all(header::CONTENT_LENGTH).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let value = value.to_str().ok()?;
    // `u64::from_str` also accepts a leading `+`, which `Content-Length` does not.
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// Maps a failed bounded body read onto the Trusted Server error for its status.
fn request_body_read_error(error: &EdgeError, max: usize) -> Report<TrustedServerError> {
    let status = error.status();
    let context = match status {
        StatusCode::PAYLOAD_TOO_LARGE => TrustedServerError::RequestTooLarge {
            message: format!("request body exceeds the {max} byte limit"),
        },
        StatusCode::REQUEST_TIMEOUT => TrustedServerError::RequestTimeout {
            message: "request body was not read before the read deadline".to_string(),
        },
        status if status.is_client_error() => TrustedServerError::BadRequest {
            message: "request body could not be read".to_string(),
        },
        _ => TrustedServerError::Proxy {
            message: "failed to read request body".to_string(),
        },
    };
    Report::new(context).attach(format!("body read failed with {status}: {error}"))
}

/// Strip forwarded headers that clients can spoof.
///
/// Call this at the edge entry point (before routing) to prevent
/// `X-Forwarded-Host: evil.com` from hijacking all URL rewriting.
/// See <https://github.com/IABTechLab/trusted-server/issues/409>.
pub fn sanitize_forwarded_headers(headers: &mut HeaderMap) {
    for header in SPOOFABLE_FORWARDED_HEADERS {
        if headers.remove(*header).is_some() {
            log::debug!("Stripped spoofable header: {header}");
        }
    }
}

/// Returns `true` when the request looks like a top-level document navigation.
///
/// Uses [`Sec-Fetch-Dest`] when available (preferred — it is a forbidden
/// header that cannot be spoofed by client-side JS). Falls back to checking
/// the `Accept` header for an explicit `text/html` MIME type.
///
/// This distinction prevents EC identity cookies from being generated on
/// subresource requests (fonts, images, scripts) where browsers may omit
/// consent signals such as `Sec-GPC`.
///
/// [`Sec-Fetch-Dest`]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Sec-Fetch-Dest
#[must_use]
pub fn is_navigation_request(req: &Request<EdgeBody>) -> bool {
    // Prefer Sec-Fetch-Dest (reliable, unspoofable by JS). All modern
    // browsers send this header on every request.
    if let Some(dest) = req
        .headers()
        .get("sec-fetch-dest")
        .and_then(|v| v.to_str().ok())
    {
        return dest.trim().eq_ignore_ascii_case("document");
    }

    // Fallback for clients that don't send Fetch Metadata headers:
    // only match an explicit text/html (not */* which fonts also send).
    // This path is weaker — `fetch()` can set Accept: text/html — so log
    // it for monitoring how many clients lack Sec-Fetch-Dest.
    log::debug!("is_navigation_request: Sec-Fetch-Dest absent, falling back to Accept header");
    req.headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| {
            accept.split(',').any(|part| {
                let mime = part.split(';').next().unwrap_or("").trim();
                mime.eq_ignore_ascii_case("text/html")
            })
        })
}

/// Validated adapter runtime origin used when immutable ingress has no origin.
///
/// Adapters construct this only from trusted runtime metadata. It supplies a
/// fallback for [`RequestInfo`] without changing [`RequestIngress`] or granting
/// origin trust to trace actions.
#[derive(Clone)]
pub struct RuntimeRequestOrigin(InboundOrigin);

impl RuntimeRequestOrigin {
    /// Wrap an origin already validated by [`InboundOrigin::parse`].
    ///
    /// # Examples
    ///
    /// ```
    /// use edgezero_core::ingress::{InboundOrigin, OriginSource};
    /// use trusted_server_core::http_util::RuntimeRequestOrigin;
    /// let origin = InboundOrigin::parse(
    ///     "https", "publisher.example.com:8443", OriginSource::RuntimeUri,
    /// )?;
    /// let runtime_origin = RuntimeRequestOrigin::new(origin);
    /// # Ok::<(), edgezero_core::error::EdgeError>(())
    /// ```
    #[must_use]
    pub fn new(origin: InboundOrigin) -> Self {
        Self(origin)
    }
}

/// Effective public request information for URL rewriting.
///
/// Authenticated forwarding takes precedence over transport origin. Without
/// authenticated metadata, immutable ingress, adapter runtime origin, Host,
/// URI authority and platform TLS facts remain the sources; raw forwarding
/// headers never grant origin trust.
#[derive(Debug, Clone)]
pub struct RequestInfo {
    /// The effective public authority, including a non-default port.
    pub host: String,
    /// The effective public HTTP or HTTPS scheme.
    pub scheme: String,
}

impl RequestInfo {
    /// Extract the validated public origin information from a prepared request.
    ///
    /// Host selection order:
    /// 1. Authenticated trusted-forwarder public origin
    /// 2. Immutable ingress origin, then adapter runtime origin
    /// 3. Nonempty `Host` header
    /// 4. Request URI authority (host and port only, excluding userinfo)
    ///
    /// Every selected host except the authenticated forwarder origin, which is
    /// validated more strictly when it is parsed, must be an ASCII hostname
    /// (including underscores for local service names), IPv4 address, or
    /// bracketed IPv6 address, optionally followed by a numeric port. Requests
    /// without any host source retain an empty host.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let info = RequestInfo::from_request(&request, &client_info)?;
    /// let origin = format!("{}://{}", info.scheme, info.host);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::BadRequest`] when the selected host is
    /// malformed or the `Host` header is not text, rather than falling back to
    /// a lower-priority source.
    pub fn from_request(
        req: &Request<EdgeBody>,
        client_info: &ClientInfo,
    ) -> Result<Self, Report<TrustedServerError>> {
        if let Some(origin) = public_origin(req.extensions()) {
            return Ok(Self {
                host: origin.authority.clone(),
                scheme: origin.scheme.clone(),
            });
        }
        let runtime_origin = req
            .extensions()
            .get::<RequestIngress>()
            .and_then(RequestIngress::origin)
            .or_else(|| {
                req.extensions()
                    .get::<RuntimeRequestOrigin>()
                    .map(|origin| &origin.0)
            });
        let host = match runtime_origin {
            Some(origin) => origin.authority().to_owned(),
            None => header_or_uri_host(req)?,
        };
        if !host.is_empty() && !is_valid_request_host(&host) {
            return Err(invalid_request_host());
        }
        let scheme = runtime_origin
            .map(|origin| origin.scheme().to_owned())
            .unwrap_or_else(|| detect_request_scheme(req, client_info));
        Ok(Self { host, scheme })
    }
}

fn invalid_request_host() -> Report<TrustedServerError> {
    Report::new(TrustedServerError::BadRequest {
        message: "Invalid request host".to_owned(),
    })
}

/// Select a nonempty `Host` header, then the URI authority without userinfo.
///
/// # Errors
///
/// Returns a bad-request error for a non-text `Host` header.
fn header_or_uri_host(req: &Request<EdgeBody>) -> Result<String, Report<TrustedServerError>> {
    let header_host = req
        .headers()
        .get(header::HOST)
        .map(|value| value.to_str().map_err(|_| invalid_request_host()))
        .transpose()?
        .filter(|host| !host.is_empty());
    let host = header_host.or_else(|| {
        // Absolute-form HTTP/1.1 and HTTP/2 may carry the host only in the URI.
        // Drop userinfo so credentials never reach rewritten URLs.
        req.uri().authority().map(|authority| {
            let authority = authority.as_str();
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host_and_port)| host_and_port)
        })
    });
    Ok(host.unwrap_or_default().to_owned())
}

/// Accept only literal host syntax that is safe to insert into script source.
fn is_valid_request_host(value: &str) -> bool {
    let port = if let Some(ipv6) = value.strip_prefix('[') {
        let Some((address, suffix)) = ipv6.split_once(']') else {
            return false;
        };
        if address.parse::<Ipv6Addr>().is_err() {
            return false;
        }
        if suffix.is_empty() {
            None
        } else if let Some(port) = suffix.strip_prefix(':') {
            Some(port)
        } else {
            return false;
        }
    } else {
        let (host, port) = value
            .split_once(':')
            .map_or((value, None), |(host, port)| (host, Some(port)));
        let host = host.strip_suffix('.').unwrap_or(host);
        if host.is_empty()
            || host.len() > 253
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || !label.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                    || !label.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                    || !label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
        {
            return false;
        }
        port
    };
    port.is_none_or(|port| {
        !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && port.parse::<u16>().is_ok()
    })
}

fn detect_request_scheme(req: &Request<EdgeBody>, client_info: &ClientInfo) -> String {
    if client_info.tls_protocol.is_some() || client_info.tls_cipher.is_some() {
        return "https".to_owned();
    }
    // Fastly injects this signal from runtime TLS metadata after removing the
    // client-supplied copy. Preserve the compatibility fallback for requests
    // without a runtime ingress extension.
    if req
        .headers()
        .get("fastly-ssl")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    {
        return "https".to_owned();
    }
    "http".to_owned()
}

/// Build a static text response with strong `ETag` and standard caching headers.
/// Handles If-None-Match to return 304 when appropriate.
///
/// # Panics
///
/// Panics if the generated response headers cannot be represented in an
/// `http::Response`.
pub fn serve_static_with_etag(
    body: &str,
    req: &Request<EdgeBody>,
    content_type: &str,
    edge_header: EdgeCacheHeader,
) -> Response<EdgeBody> {
    let hash = Sha256::digest(body.as_bytes());
    let etag = format!("\"sha256-{}\"", hex::encode(hash));
    let short_policy = CachePolicy::public_short_with_stale(
        Duration::from_secs(300),
        Duration::from_secs(60),
        Duration::from_secs(86_400),
    );

    if let Some(if_none_match) = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|h| h.to_str().ok())
        && if_none_match == etag
    {
        let mut response = Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .header(header::VARY, "Accept-Encoding")
            .body(EdgeBody::empty())
            .expect("should build 304 static response");
        short_policy.apply_to_headers(response.headers_mut(), edge_header);
        return response;
    }

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ETAG, &etag)
        .header(header::VARY, "Accept-Encoding")
        .body(EdgeBody::from(body.as_bytes()))
        .expect("should build static response");
    short_policy.apply_to_headers(response.headers_mut(), edge_header);
    response
}

/// Encrypts a URL using XChaCha20-Poly1305 with a key derived from the publisher `proxy_secret`.
/// Returns a Base64 URL-safe (no padding) token: b"x1" || nonce(24) || ciphertext+tag.
///
/// # Panics
///
/// Panics if encryption fails (which should not happen under normal circumstances).
#[must_use]
pub fn encode_url(settings: &Settings, plaintext_url: &str) -> String {
    // Derive a 32-byte key via SHA-256(secret)
    let key_bytes = Sha256::digest(settings.publisher.proxy_secret.expose().as_bytes());
    let cipher = XChaCha20Poly1305::new(&key_bytes);

    // Deterministic 24-byte nonce derived from secret and plaintext (stable tokens)
    let mut hasher = Sha256::new();
    hasher.update(b"ts-proxy-x1");
    hasher.update(settings.publisher.proxy_secret.expose().as_bytes());
    hasher.update(plaintext_url.as_bytes());
    let nonce_full = hasher.finalize();
    let mut nonce = [0_u8; 24];
    nonce[..24].copy_from_slice(&nonce_full[..24]);
    let nonce = XNonce::from(nonce);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext_url.as_bytes())
        .expect("encryption failure");

    let mut out: Vec<u8> = Vec::with_capacity(2 + 24 + ciphertext.len());
    out.extend_from_slice(b"x1");
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    URL_SAFE_NO_PAD.encode(out)
}

/// Decrypts and verifies a token produced by `encode_url`. Returns None if invalid.
#[must_use]
pub fn decode_url(settings: &Settings, token: &str) -> Option<String> {
    let data = URL_SAFE_NO_PAD.decode(token.as_bytes()).ok()?;
    if data.len() < 2 + 24 + 16 {
        return None;
    }
    if &data[..2] != b"x1" {
        return None;
    }
    let nonce_bytes = &data[2..2 + 24];
    let nonce = XNonce::try_from(nonce_bytes).ok()?;
    let ciphertext = &data[2 + 24..];

    let key_bytes = Sha256::digest(settings.publisher.proxy_secret.expose().as_bytes());
    let cipher = XChaCha20Poly1305::new(&key_bytes);
    let pt = cipher.decrypt(&nonce, ciphertext).ok()?;
    String::from_utf8(pt).ok()
}

/// Compute a deterministic signature token (tstoken) for a clear-text URL using the
/// publisher's `proxy_secret`. This enables proxy URLs to retain the original URL in
/// clear text while still providing integrity/authenticity via a keyed digest.
///
/// Token format: Base64 URL-safe (no padding) of SHA-256("ts-proxy-v2" || secret || url)
/// - Not intended as a general HMAC; sufficient for validating unmodified URLs under a secret.
#[must_use]
pub fn sign_clear_url(settings: &Settings, clear_url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ts-proxy-v2");
    hasher.update(settings.publisher.proxy_secret.expose().as_bytes());
    hasher.update(clear_url.as_bytes());
    let digest = hasher.finalize();
    URL_SAFE_NO_PAD.encode(digest)
}

/// Constant-time string comparison.
///
/// The explicit length check documents the invariant that both values have known,
/// non-secret lengths. Both checks always run — the short-circuit `&&` is safe
/// here because token lengths are public information, not secrets.
///
/// # Security
///
/// The length equality check short-circuits (via `&&`), which reveals whether the
/// two strings have equal length via timing. This is safe when both strings have
/// **publicly known, fixed lengths** (e.g. base64url-encoded SHA-256 digests are
/// always 43 bytes). Do **not** use this function to compare secrets of
/// variable or confidential length — use a constant-time comparison that
/// also hides length, such as comparing HMAC outputs.
#[must_use]
pub(crate) fn ct_str_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// Verify a `tstoken` for the given clear-text URL.
///
/// Uses constant-time comparison to prevent timing side-channel attacks.
/// Length is not secret (always 43 bytes for base64url-encoded SHA-256),
/// but we check explicitly to document the invariant.
#[must_use]
pub fn verify_clear_url_signature(settings: &Settings, clear_url: &str, token: &str) -> bool {
    let expected = sign_clear_url(settings, clear_url);
    ct_str_eq(&expected, token)
}

/// Compute tstoken for the new proxy scheme: SHA-256 of the encrypted full URL (including query).
///
/// Steps:
/// 1) Encrypt the full URL via `encode_url` (XChaCha20-Poly1305 with deterministic nonce)
/// 2) Base64-decode the `x1||nonce||ciphertext+tag` bytes
/// 3) Compute SHA-256 over those bytes
/// 4) Return Base64 URL-safe (no padding) digest as `tstoken`
///
/// # Panics
///
/// This function will not panic under normal circumstances. The internal base64 decode
/// cannot fail because it operates on data that was just encoded by `encode_url`.
#[must_use]
pub fn compute_encrypted_sha256_token(settings: &Settings, full_url: &str) -> String {
    // Encrypt deterministically using existing helper
    let enc = encode_url(settings, full_url);
    // Decode to raw bytes (x1 + nonce + ciphertext+tag)
    let raw = URL_SAFE_NO_PAD
        .decode(enc.as_bytes())
        .expect("decode must succeed for just-encoded data");
    let digest = Sha256::digest(&raw);
    URL_SAFE_NO_PAD.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{IntoHttpResponse as _, http_error};
    use crate::platform::ClientInfo;
    use crate::redacted::Redacted;
    use crate::settings::TrustedClientIpConfig;
    use crate::test_support::body_read::body_read_failure_cases;
    use crate::test_support::logging::LogCapture;
    use http::{HeaderName, HeaderValue, Method};

    #[test]
    fn into_body_payload_keeps_buffered_and_streamed_bodies_apart() {
        let BodyPayload::Once(bytes) = into_body_payload(EdgeBody::from("example")) else {
            panic!("should keep a buffered body buffered");
        };
        let streamed = EdgeBody::stream(futures::stream::iter([bytes::Bytes::from_static(
            b"example",
        )]));

        assert_eq!(
            bytes.as_ref(),
            b"example",
            "should return the buffered bytes"
        );
        assert!(
            matches!(into_body_payload(streamed), BodyPayload::Stream(_)),
            "should keep a streamed body streamed"
        );
    }

    #[test]
    fn body_as_reader_rejects_a_streamed_body() {
        let buffered = body_as_reader(EdgeBody::from("example"), "proxy")
            .expect("should read a buffered body")
            .into_inner();
        let streamed = EdgeBody::stream(futures::stream::iter([bytes::Bytes::from_static(
            b"example",
        )]));

        let error =
            body_as_reader(streamed, "publisher").expect_err("should reject a streamed body");

        assert_eq!(
            buffered.as_ref(),
            b"example",
            "should keep the buffered bytes"
        );
        let TrustedServerError::Proxy { message } = error.current_context() else {
            panic!("should fail with the proxy context instead of reading as empty, got {error:?}");
        };
        assert_eq!(
            message, "streaming body cannot be processed by the sync publisher pipeline",
            "should name the caller's pipeline"
        );
    }

    fn build_request(method: Method, uri: &str) -> Request<EdgeBody> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    fn set_header(req: &mut Request<EdgeBody>, name: &str, value: &str) {
        req.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).expect("should build header name"),
            HeaderValue::from_str(value).expect("should build header value"),
        );
    }

    fn default_client_info() -> ClientInfo {
        ClientInfo::default()
    }

    #[test]
    fn encode_decode_roundtrip() {
        let settings = crate::test_support::tests::create_test_settings();
        let src = "https://t.example/p.gif";
        let enc = encode_url(&settings, src);
        assert!(!enc.ends_with('='));
        let Some(dec) = decode_url(&settings, &enc) else {
            panic!("decode failed for token: {enc}");
        };
        assert_eq!(dec, src);
    }

    #[test]
    fn decode_invalid() {
        let settings = crate::test_support::tests::create_test_settings();
        assert!(decode_url(&settings, "@@invalid@@").is_none());
    }

    #[test]
    fn sign_and_verify_clear_url() {
        let settings = crate::test_support::tests::create_test_settings();
        let url = "https://cdn.example/a.png?x=1";
        let t1 = sign_clear_url(&settings, url);
        assert!(!t1.is_empty());
        assert!(verify_clear_url_signature(&settings, url, &t1));
        // Different URL should not verify
        assert!(!verify_clear_url_signature(
            &settings,
            "https://cdn.example/a.png?x=2",
            &t1
        ));
    }

    #[test]
    fn verify_clear_url_rejects_tampered_token() {
        let settings = crate::test_support::tests::create_test_settings();
        let url = "https://cdn.example/a.png?x=1";
        let valid_token = sign_clear_url(&settings, url);

        // Flip one bit in the first byte — same URL, same length, wrong bytes
        let mut tampered = valid_token.into_bytes();
        tampered[0] ^= 0x01;
        let tampered =
            String::from_utf8(tampered).expect("should be valid utf8 after single-bit flip");

        assert!(
            !verify_clear_url_signature(&settings, url, &tampered),
            "should reject token with tampered bytes"
        );
    }

    #[test]
    fn verify_clear_url_rejects_empty_token() {
        let settings = crate::test_support::tests::create_test_settings();
        assert!(
            !verify_clear_url_signature(&settings, "https://cdn.example/a.png", ""),
            "should reject empty token"
        );
    }

    // RequestInfo tests

    #[test]
    fn request_info_prioritizes_forwarder_then_ingress_then_runtime_origin() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.publisher.domain = "publisher.example.com".to_owned();
        settings.trusted_forwarder = Some(crate::settings::TrustedForwarderConfig {
            auth_header: crate::forwarder::FORWARDER_AUTH_HEADER.to_owned(),
            shared_secret: crate::redacted::Redacted::new(
                "fictional-forwarder-secret-0123456789".to_owned(),
            ),
        });
        for (accepted, ingress_available, expected_host, expected_scheme) in [
            (true, true, "publisher.example.com:8443", "https"),
            (false, true, "internal.example.com:8080", "http"),
            (false, false, "runtime.example.com:9443", "https"),
        ] {
            let mut request = build_request(Method::GET, "/article");
            set_header(&mut request, "host", "client-supplied.example.com");
            set_header(
                &mut request,
                "x-forwarded-host",
                "publisher.example.com:8443",
            );
            set_header(&mut request, "x-forwarded-proto", "https");
            if accepted {
                set_header(
                    &mut request,
                    crate::forwarder::FORWARDER_AUTH_HEADER,
                    "fictional-forwarder-secret-0123456789",
                );
            }
            request.extensions_mut().insert(RuntimeRequestOrigin::new(
                InboundOrigin::parse(
                    "https",
                    "runtime.example.com:9443",
                    edgezero_core::ingress::OriginSource::RuntimeUri,
                )
                .expect("should validate runtime origin"),
            ));
            request.extensions_mut().insert(RequestIngress::new(
                edgezero_core::ingress::CapturedTarget::Unavailable(
                    edgezero_core::ingress::TargetUnavailable::NotExposed,
                ),
                ingress_available.then(|| {
                    InboundOrigin::parse(
                        "http",
                        "internal.example.com:8080",
                        edgezero_core::ingress::OriginSource::RuntimeUri,
                    )
                    .expect("should validate ingress origin")
                }),
                edgezero_core::ingress::HeaderFidelity::default(),
            ));
            crate::test_support::tests::with_request_parts(&mut request, |parts| {
                crate::forwarder::prepare_trusted_forwarder(parts, &settings);
            });

            let info = RequestInfo::from_request(&request, &default_client_info())
                .expect("should accept trusted origin metadata");

            assert_eq!(
                info.host, expected_host,
                "should honor public, immutable ingress and runtime authority precedence"
            );
            assert_eq!(
                info.scheme, expected_scheme,
                "should honor public, immutable ingress and runtime scheme precedence"
            );
        }
    }

    #[test]
    fn request_info_uses_uri_authority_without_host_headers() {
        for (uri, expected_host) in [
            (
                "https://publisher.example.com/page",
                "publisher.example.com",
            ),
            (
                "https://publisher.example.com:8443/page",
                "publisher.example.com:8443",
            ),
            ("https://[2001:db8::1]:8443/page", "[2001:db8::1]:8443"),
            (
                "https://user:secret@publisher.example.com/page",
                "publisher.example.com",
            ),
            (
                "https://user:pass@publisher.example.com:8443/page",
                "publisher.example.com:8443",
            ),
            (
                "https://user:pass@[2001:db8::1]:8443/page",
                "[2001:db8::1]:8443",
            ),
            (
                "https://a@b@publisher.example.com/page",
                "publisher.example.com",
            ),
        ] {
            let req = Request::builder()
                .version(http::Version::HTTP_2)
                .uri(uri)
                .body(EdgeBody::empty())
                .expect("should build authority-only HTTP/2 request");
            assert!(req.headers().is_empty(), "should have no host headers");

            let info = RequestInfo::from_request(&req, &default_client_info())
                .expect("should accept valid request metadata");

            assert_eq!(
                info.host, expected_host,
                "should use only the host and port, preserving IPv6 brackets"
            );
        }
    }

    #[test]
    fn request_info_ignores_unauthenticated_public_origin_hints() {
        let mut request = build_request(Method::GET, "http://upstream.example.com/path");
        set_header(&mut request, "host", "upstream.example.com");
        set_header(
            &mut request,
            "forwarded",
            "proto=https;host=other.example.com",
        );
        set_header(&mut request, "x-forwarded-host", "other.example.com");
        set_header(&mut request, "x-forwarded-proto", "https");

        let info = RequestInfo::from_request(&request, &default_client_info())
            .expect("should accept request metadata");

        assert_eq!(
            info.host, "upstream.example.com",
            "should require authenticated public host metadata"
        );
        assert_eq!(
            info.scheme, "http",
            "should require authenticated public scheme metadata"
        );
    }

    #[test]
    fn request_info_prefers_nonempty_host_header_over_uri_authority() {
        for (host, expected_host) in [
            ("header.example.com", "header.example.com"),
            ("", "authority.example.com"),
        ] {
            let mut req = build_request(Method::GET, "https://authority.example.com/page");
            set_header(&mut req, "host", host);

            let info = RequestInfo::from_request(&req, &default_client_info())
                .expect("should accept valid request metadata");

            assert_eq!(
                info.host, expected_host,
                "should prefer a nonempty Host header over URI authority"
            );
        }
    }

    #[test]
    fn request_info_validates_uri_authority_before_rewriting() {
        for uri in [
            "https://publisher.example.com:invalid/page",
            "https://publisher.example.com:65536/page",
            "https://user:secret@-publisher.example.com/page",
            "https://-publisher.example.com/page",
        ] {
            for empty_host_header in [false, true] {
                let mut req = build_request(Method::GET, uri);
                if empty_host_header {
                    set_header(&mut req, "host", "");
                }
                let error = RequestInfo::from_request(&req, &default_client_info())
                    .expect_err("should validate the URI fallback host");
                assert!(
                    matches!(
                        error.current_context(),
                        TrustedServerError::BadRequest { .. }
                    ),
                    "should reject unsafe URI authority with a bad-request error"
                );
            }
        }
    }

    #[test]
    fn request_info_keeps_empty_host_without_headers_or_uri_authority() {
        let req = build_request(Method::GET, "/page");

        let info = RequestInfo::from_request(&req, &default_client_info())
            .expect("should accept valid request metadata");

        assert!(
            info.host.is_empty(),
            "should keep the host empty when no host source is available"
        );
    }

    #[test]
    fn request_info_accepts_valid_hosts_without_normalizing_them() {
        for host in [
            "example.com",
            "Sub-Domain.EXAMPLE.com.",
            "xn--bcher-kva.example.com",
            "example.com:443",
            "localhost:3000",
            "internal-proxy:8080",
            "my_service:8080",
            "service_.example.com:8443",
            "_service.example.com",
            "_.example.com",
            "bad_label.example.com",
            "192.0.2.1:8080",
            "[2001:db8::1]",
            "[2001:db8::1]:443",
            "[::1]:3000",
            "[::ffff:192.0.2.1]:65535",
        ] {
            let mut req = build_request(Method::GET, "https://example.com/page");
            set_header(&mut req, "host", host);
            let info = RequestInfo::from_request(&req, &default_client_info())
                .expect("should accept valid host syntax");
            assert_eq!(info.host, host, "should preserve Host: {host}");
        }
    }

    #[test]
    fn request_info_rejects_malformed_host_headers_without_fallback() {
        for host in [
            "example.com</script><script>alert(1)</script>",
            "example.com'",
            "example.com\"",
            "example.com`",
            "example.com\\path",
            "example.com/path",
            "example.com?query",
            "example.com#fragment",
            "user@example.com",
            "https://example.com",
            "example.com%22",
            "example.com:invalid",
            "example.com:",
            "example.com:65536",
            "example.com:+443",
            "example.com:443:80",
            "bad host.example.com",
            "-bad.example.com",
            "bad-.example.com",
            "example..com",
            ".example.com",
            ".",
            "::1",
            "[not-ipv6]",
            "[2001:db8::1",
            "[2001:db8::1]suffix",
            "[2001:db8::1]:",
        ] {
            let mut req = build_request(Method::GET, "https://example.com/page");
            set_header(&mut req, "host", host);
            let error = RequestInfo::from_request(&req, &default_client_info())
                .expect_err("should reject malformed Host");
            assert!(
                matches!(
                    error.current_context(),
                    TrustedServerError::BadRequest { .. }
                ),
                "should reject Host with a bad-request error: {host}"
            );
            assert!(
                !format!("{error}").contains("</script>"),
                "should not reflect hostile host bytes in the error"
            );
        }
    }

    #[test]
    fn request_info_rejects_non_text_host_header() {
        let mut req = build_request(Method::GET, "https://example.com/page");
        req.headers_mut().insert(
            header::HOST,
            HeaderValue::from_bytes(b"example.com\xff").expect("should build non-text header"),
        );
        assert!(
            RequestInfo::from_request(&req, &default_client_info()).is_err(),
            "should reject a non-text Host rather than falling back"
        );
    }

    #[test]
    fn request_info_rejects_oversized_dns_names() {
        for host in [
            format!("{}.example.com", "a".repeat(64)),
            format!(
                "{}.{}.{}.{}",
                "a".repeat(63),
                "b".repeat(63),
                "c".repeat(63),
                "d".repeat(63)
            ),
        ] {
            let mut req = build_request(Method::GET, "https://example.com/page");
            set_header(&mut req, "host", &host);
            assert!(
                RequestInfo::from_request(&req, &default_client_info()).is_err(),
                "should reject oversized DNS name: {host}"
            );
        }
    }

    #[test]
    fn request_info_validates_host_header_and_runtime_origin_sources() {
        for host in [
            "example.com'",
            "example.com;x",
            "example.com(x)",
            "example.com</script>",
        ] {
            let mut req = build_request(Method::GET, "/page");
            req.headers_mut().insert(
                header::HOST,
                HeaderValue::from_str(host).expect("should build visible host"),
            );
            assert!(
                RequestInfo::from_request(&req, &default_client_info()).is_err(),
                "should reject unsafe Host: {host}"
            );
            if let Ok(origin) = InboundOrigin::parse(
                "https",
                host,
                edgezero_core::ingress::OriginSource::RuntimeUri,
            ) {
                let mut req = build_request(Method::GET, "/page");
                req.extensions_mut()
                    .insert(RuntimeRequestOrigin::new(origin));
                assert!(
                    RequestInfo::from_request(&req, &default_client_info()).is_err(),
                    "should reject unsafe runtime authority: {host}"
                );
            }
        }
    }

    #[test]
    fn test_request_info_from_host_header() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        set_header(&mut req, "host", "test.example.com");

        let info = RequestInfo::from_request(&req, &default_client_info())
            .expect("should accept Host header");
        assert_eq!(
            info.host, "test.example.com",
            "Host should use Host header when forwarded headers are missing"
        );
        // No TLS or forwarded headers, defaults to http.
        assert_eq!(
            info.scheme, "http",
            "Scheme should default to http without TLS or forwarded headers"
        );
    }

    #[test]
    fn test_request_info_scheme_from_fastly_ssl() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        set_header(&mut req, "fastly-ssl", "1");

        let info = RequestInfo::from_request(&req, &default_client_info())
            .expect("should accept request without host");
        assert_eq!(
            info.scheme, "https",
            "Scheme should fall back to Fastly-SSL when other signals are missing"
        );
    }

    // Sanitization tests

    #[test]
    fn sanitize_trusted_client_ip_headers_removes_only_configured_headers() {
        let config = TrustedClientIpConfig {
            ip_header: "x-reader-ip".to_owned(),
            auth_header: "x-reader-ip-auth".to_owned(),
            shared_secret: Redacted::new("fictional-shared-secret-0123456789".to_owned()),
        };
        let mut req = build_request(Method::GET, "https://example.com/page");
        set_header(&mut req, "x-reader-ip", "198.51.100.7");
        set_header(
            &mut req,
            "x-reader-ip-auth",
            "fictional-shared-secret-0123456789",
        );
        set_header(&mut req, "x-unrelated", "preserved");

        crate::test_support::tests::with_request_parts(&mut req, |parts| {
            sanitize_trusted_client_ip_headers(parts, Some(&config));
        });

        assert!(
            req.headers().get("x-reader-ip").is_none(),
            "should remove the configured IP header"
        );
        assert!(
            req.headers().get("x-reader-ip-auth").is_none(),
            "should remove the configured authentication header"
        );
        assert_eq!(
            req.headers()
                .get("x-unrelated")
                .expect("should preserve an unrelated header"),
            "preserved",
            "should not remove unrelated headers"
        );
    }

    #[test]
    fn sanitize_removes_all_spoofable_headers() {
        let mut req = build_request(Method::GET, "https://example.com/page");
        set_header(&mut req, "host", "legit.example.com");
        set_header(&mut req, "forwarded", "host=evil.com;proto=https");
        set_header(&mut req, "x-forwarded-host", "evil.com");
        set_header(&mut req, "x-forwarded-proto", "https");
        set_header(&mut req, "fastly-ssl", "1");

        sanitize_forwarded_headers(req.headers_mut());

        assert!(
            req.headers().get("forwarded").is_none(),
            "should strip Forwarded header"
        );
        assert!(
            req.headers().get("x-forwarded-host").is_none(),
            "should strip X-Forwarded-Host header"
        );
        assert!(
            req.headers().get("x-forwarded-proto").is_none(),
            "should strip X-Forwarded-Proto header"
        );
        assert!(
            req.headers().get("fastly-ssl").is_none(),
            "should strip Fastly-SSL header"
        );
        assert_eq!(
            req.headers()
                .get("host")
                .expect("should have Host header")
                .to_str()
                .expect("should be valid UTF-8"),
            "legit.example.com",
            "should preserve Host header"
        );
    }

    #[test]
    fn sanitize_then_request_info_falls_back_to_host() {
        let mut req = build_request(Method::GET, "https://example.com/page");
        set_header(&mut req, "host", "legit.example.com");
        set_header(&mut req, "x-forwarded-host", "evil.com");
        set_header(&mut req, "x-forwarded-proto", "http");

        sanitize_forwarded_headers(req.headers_mut());
        let info = RequestInfo::from_request(&req, &default_client_info())
            .expect("should accept sanitized Host header");

        assert_eq!(
            info.host, "legit.example.com",
            "should fall back to Host header after sanitization"
        );
        assert_eq!(
            info.scheme, "http",
            "should default to http when forwarded proto is stripped and no TLS"
        );
    }

    #[test]
    fn test_ct_str_eq() {
        assert!(ct_str_eq("hello", "hello"), "should match equal strings");
        assert!(
            !ct_str_eq("hello", "world"),
            "should not match different strings"
        );
        assert!(
            !ct_str_eq("hello", "hell"),
            "should not match different lengths"
        );
        assert!(
            !ct_str_eq("hell", "hello"),
            "should not match when first is shorter"
        );
        assert!(ct_str_eq("", ""), "should match empty strings");
    }

    #[test]
    fn test_copy_custom_headers_filters_internal() {
        let mut req = build_request(Method::GET, "https://example.com");
        set_header(&mut req, "x-custom-1", "value1");
        // HeaderName is case-insensitive and normalized by `http`.
        set_header(&mut req, "X-Custom-2", "value2");
        set_header(&mut req, "x-ts-ec", "should not copy");
        set_header(&mut req, "x-geo-country", "US");
        // Dynamic partner header (x-ts-<source_domain>).
        set_header(&mut req, "x-ts-ssp_x", "partner-uid-123");
        set_header(&mut req, "x-ts-liveramp", "lr-uid-456");

        let mut target = build_request(Method::GET, "https://target.com");
        copy_custom_headers(&req, &mut target);

        assert_eq!(
            target
                .headers()
                .get("x-custom-1")
                .unwrap()
                .to_str()
                .unwrap(),
            "value1",
            "Should copy arbitrary x-header"
        );
        assert_eq!(
            target
                .headers()
                .get("x-custom-2")
                .unwrap()
                .to_str()
                .unwrap(),
            "value2",
            "Should copy arbitrary X-header (case insensitive)"
        );
        assert!(
            target.headers().get("x-ts-ec").is_none(),
            "Should filter x-ts-ec"
        );
        assert!(
            target.headers().get("x-geo-country").is_none(),
            "Should filter x-geo-country"
        );
        assert!(
            target.headers().get("x-ts-ssp_x").is_none(),
            "Should filter dynamic x-ts-<source_domain> headers"
        );
        assert!(
            target.headers().get("x-ts-liveramp").is_none(),
            "Should filter dynamic x-ts-<source_domain> headers"
        );
    }

    // -----------------------------------------------------------------------
    // is_navigation_request
    // -----------------------------------------------------------------------

    #[test]
    fn copy_custom_headers_preserves_duplicate_values() {
        let mut from = build_request(Method::GET, "https://example.com");
        from.headers_mut().append(
            HeaderName::from_bytes(b"x-custom-data").expect("should build header name"),
            HeaderValue::from_str("first").expect("should build header value"),
        );
        from.headers_mut().append(
            HeaderName::from_bytes(b"x-custom-data").expect("should build header name"),
            HeaderValue::from_str("second").expect("should build header value"),
        );

        let mut target = build_request(Method::GET, "https://target.com");
        copy_custom_headers(&from, &mut target);

        let values: Vec<_> = target
            .headers()
            .get_all("x-custom-data")
            .iter()
            .map(|v| v.to_str().expect("should be valid utf8"))
            .collect();
        assert_eq!(
            values,
            vec!["first", "second"],
            "should preserve duplicate x-header values"
        );
    }

    #[test]
    fn request_info_https_from_client_info_tls_protocol() {
        let req = build_request(Method::GET, "https://test.example.com/page");
        let client_info = ClientInfo {
            tls_protocol: Some("TLSv1.3".to_string()),
            ..ClientInfo::default()
        };

        let info = RequestInfo::from_request(&req, &client_info)
            .expect("should accept TLS protocol metadata");

        assert_eq!(
            info.scheme, "https",
            "should detect https from ClientInfo tls_protocol"
        );
    }

    #[test]
    fn request_info_https_from_client_info_tls_cipher() {
        let req = build_request(Method::GET, "https://test.example.com/page");
        let client_info = ClientInfo {
            tls_cipher: Some("TLS_AES_128_GCM_SHA256".to_string()),
            ..ClientInfo::default()
        };

        let info = RequestInfo::from_request(&req, &client_info)
            .expect("should accept TLS cipher metadata");

        assert_eq!(
            info.scheme, "https",
            "should detect https from ClientInfo tls_cipher"
        );
    }

    #[test]
    fn navigation_true_for_sec_fetch_dest_document() {
        let mut req = build_request(Method::GET, "https://example.com");
        set_header(&mut req, "sec-fetch-dest", "document");
        assert!(
            is_navigation_request(&req),
            "should detect Sec-Fetch-Dest: document as navigation"
        );
    }

    #[test]
    fn navigation_false_for_sec_fetch_dest_font() {
        let mut req = build_request(Method::GET, "https://example.com/font.woff2");
        set_header(&mut req, "sec-fetch-dest", "font");
        set_header(&mut req, header::ACCEPT.as_str(), "*/*");
        assert!(
            !is_navigation_request(&req),
            "should reject font even when Accept is */*"
        );
    }

    #[test]
    fn navigation_false_for_sec_fetch_dest_script() {
        let mut req = build_request(Method::GET, "https://example.com/app.js");
        set_header(&mut req, "sec-fetch-dest", "script");
        set_header(&mut req, header::ACCEPT.as_str(), "*/*");
        assert!(
            !is_navigation_request(&req),
            "should reject script even when Accept is */*"
        );
    }

    #[test]
    fn navigation_false_for_sec_fetch_dest_style() {
        let mut req = build_request(Method::GET, "https://example.com/style.css");
        set_header(&mut req, "sec-fetch-dest", "style");
        set_header(&mut req, header::ACCEPT.as_str(), "text/css,*/*;q=0.1");
        assert!(
            !is_navigation_request(&req),
            "should reject style subresource"
        );
    }

    #[test]
    fn navigation_false_for_sec_fetch_dest_image() {
        let mut req = build_request(Method::GET, "https://example.com/logo.png");
        set_header(&mut req, "sec-fetch-dest", "image");
        set_header(
            &mut req,
            header::ACCEPT.as_str(),
            "image/webp,image/png,*/*;q=0.8",
        );
        assert!(
            !is_navigation_request(&req),
            "should reject image subresource"
        );
    }

    #[test]
    fn navigation_false_for_sec_fetch_dest_empty() {
        let mut req = build_request(Method::GET, "https://example.com/api/data");
        set_header(&mut req, "sec-fetch-dest", "empty");
        set_header(&mut req, header::ACCEPT.as_str(), "*/*");
        assert!(
            !is_navigation_request(&req),
            "should reject fetch/XHR requests (dest=empty)"
        );
    }

    #[test]
    fn navigation_sec_fetch_dest_case_insensitive() {
        let mut req = build_request(Method::GET, "https://example.com");
        set_header(&mut req, "sec-fetch-dest", "Document");
        assert!(
            is_navigation_request(&req),
            "should match Sec-Fetch-Dest case-insensitively"
        );
    }

    #[test]
    fn navigation_fallback_accept_text_html() {
        let mut req = build_request(Method::GET, "https://example.com");
        set_header(
            &mut req,
            header::ACCEPT.as_str(),
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        );
        assert!(
            is_navigation_request(&req),
            "should fall back to Accept: text/html when Sec-Fetch-Dest is absent"
        );
    }

    #[test]
    fn navigation_fallback_wildcard_only_is_false() {
        let mut req = build_request(Method::GET, "https://example.com");
        set_header(&mut req, header::ACCEPT.as_str(), "*/*");
        assert!(
            !is_navigation_request(&req),
            "should not treat bare */* as navigation in fallback path"
        );
    }

    #[test]
    fn navigation_false_when_no_headers() {
        let req = build_request(Method::GET, "https://example.com/resource");
        assert!(
            !is_navigation_request(&req),
            "should return false when no Accept or Sec-Fetch-Dest headers are present"
        );
    }

    #[test]
    fn navigation_fallback_accept_case_insensitive() {
        let mut req = build_request(Method::GET, "https://example.com");
        set_header(&mut req, header::ACCEPT.as_str(), "TEXT/HTML");
        assert!(
            is_navigation_request(&req),
            "should match text/html case-insensitively in fallback"
        );
    }

    /// Cap the `read_request_body` tests read against.
    const READ_TEST_MAX: usize = 16;

    /// Returns headers that carry each of `values` as a `Content-Length` field.
    fn content_length_headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(value).expect("should build Content-Length value"),
            );
        }
        headers
    }

    /// Returns a streamed body that yields `chunks` in order.
    fn streamed_body(chunks: &[&'static [u8]]) -> EdgeBody {
        let items: Vec<Result<bytes::Bytes, EdgeError>> = chunks
            .iter()
            .map(|chunk| Ok(bytes::Bytes::from_static(chunk)))
            .collect();
        EdgeBody::from_stream(futures::stream::iter(items))
    }

    /// Returns a streamed body whose first item is `error`.
    fn failing_body(error: EdgeError) -> EdgeBody {
        EdgeBody::from_stream(futures::stream::iter([Err::<bytes::Bytes, _>(error)]))
    }

    fn read_body(
        headers: &HeaderMap,
        body: EdgeBody,
    ) -> Result<bytes::Bytes, Report<TrustedServerError>> {
        futures::executor::block_on(read_request_body(headers, body, READ_TEST_MAX))
    }

    /// Asserts that `result` failed with the variant `is_expected` accepts and with `status`.
    fn assert_read_error(
        result: Result<bytes::Bytes, Report<TrustedServerError>>,
        is_expected: fn(&TrustedServerError) -> bool,
        status: StatusCode,
    ) {
        let report = result.expect_err("should reject the body");
        let error = report.current_context();
        assert!(
            is_expected(error),
            "should map to the expected variant, got {error:?}"
        );
        assert_eq!(
            error.status_code(),
            status,
            "should answer {status} for {error:?}"
        );
    }

    #[test]
    fn declared_length_over_max_returns_413_without_polling() {
        let polled = std::rc::Rc::new(std::cell::Cell::new(false));
        let poll_flag = std::rc::Rc::clone(&polled);
        let body = EdgeBody::from_stream(futures::stream::poll_fn(move |_| {
            poll_flag.set(true);
            std::task::Poll::Ready(None::<Result<bytes::Bytes, EdgeError>>)
        }));
        let headers = content_length_headers(&["17"]);

        let result = read_body(&headers, body);

        assert_read_error(
            result,
            |error| matches!(error, TrustedServerError::RequestTooLarge { .. }),
            StatusCode::PAYLOAD_TOO_LARGE,
        );
        assert!(
            !polled.get(),
            "should reject a declared overflow without polling the body"
        );
    }

    #[test]
    fn stream_crossing_max_returns_413() {
        let body = streamed_body(&[b"0123456789", b"0123456789"]);

        let result = read_body(&HeaderMap::new(), body);

        assert_read_error(
            result,
            |error| matches!(error, TrustedServerError::RequestTooLarge { .. }),
            StatusCode::PAYLOAD_TOO_LARGE,
        );
    }

    #[test]
    fn read_after_deadline_returns_408() {
        let body = failing_body(EdgeError::request_timeout("expired"));

        let result = read_body(&HeaderMap::new(), body);

        assert_read_error(
            result,
            |error| matches!(error, TrustedServerError::RequestTimeout { .. }),
            StatusCode::REQUEST_TIMEOUT,
        );
    }

    #[test]
    fn client_caused_stream_error_returns_400() {
        let body = failing_body(EdgeError::bad_request("malformed chunk framing"));

        let result = read_body(&HeaderMap::new(), body);

        assert_read_error(
            result,
            |error| matches!(error, TrustedServerError::BadRequest { .. }),
            StatusCode::BAD_REQUEST,
        );
    }

    #[test]
    fn host_read_error_returns_502() {
        let body = failing_body(EdgeError::bad_gateway("host read failed"));

        let result = read_body(&HeaderMap::new(), body);

        assert_read_error(
            result,
            |error| matches!(error, TrustedServerError::Proxy { .. }),
            StatusCode::BAD_GATEWAY,
        );
    }

    #[test]
    fn body_read_failure_logs_once_at_its_status_level() {
        for case in body_read_failure_cases(READ_TEST_MAX) {
            let (request, expectation) =
                case.into_request(http::Request::builder().uri("https://example.com/"));
            let (parts, body) = request.into_parts();
            let capture = LogCapture::start();

            let report = read_body(&parts.headers, body).expect_err("should reject the body");
            let response = http_error(&report);

            let records = capture.take_all();
            drop(capture);
            let name = expectation.name();
            let expected = expectation.expected();
            assert_eq!(
                response.status(),
                expected,
                "should answer the {name} case with {expected}"
            );
            let levels: Vec<log::Level> = records.iter().map(|record| record.level).collect();
            let level = if expected.is_client_error() {
                log::Level::Warn
            } else {
                log::Level::Error
            };
            assert_eq!(
                levels,
                [level],
                "should log the {name} case once, at {level}"
            );
        }
    }

    #[test]
    fn duplicate_content_length_falls_through_to_bounded_read() {
        // Both declarations exceed the cap, so a precheck that trusted either
        // one would answer 413 instead of reading the body.
        let headers = content_length_headers(&["999", "999"]);

        let bytes = read_body(&headers, streamed_body(&[b"hello"]))
            .expect("should read a body under the cap despite duplicate declarations");

        assert_eq!(
            bytes.as_ref(),
            b"hello",
            "should return the body the bounded read collected"
        );
    }

    #[test]
    fn non_numeric_content_length_falls_through_to_bounded_read() {
        // `+999` parses as a Rust integer but is not a valid `Content-Length`.
        for declared in ["not-a-number", "+999", "1e9", ""] {
            let headers = content_length_headers(&[declared]);

            let bytes = read_body(&headers, streamed_body(&[b"hello"])).unwrap_or_else(|error| {
                panic!("should read the body when Content-Length is `{declared}`: {error:?}")
            });

            assert_eq!(
                bytes.as_ref(),
                b"hello",
                "should return the body for Content-Length `{declared}`"
            );
        }
    }

    #[test]
    fn body_under_max_returns_exact_bytes() {
        let at_cap: &'static [u8] = b"0123456789abcdef";
        let declared = at_cap.len().to_string();
        let cases = [
            ("buffered", HeaderMap::new(), EdgeBody::from(at_cap)),
            (
                "streamed",
                content_length_headers(&[declared.as_str()]),
                streamed_body(&[&at_cap[..7], &at_cap[7..]]),
            ),
            ("empty", HeaderMap::new(), EdgeBody::empty()),
        ];

        for (label, headers, body) in cases {
            let expected: &[u8] = if label == "empty" { b"" } else { at_cap };

            let bytes = read_body(&headers, body)
                .unwrap_or_else(|error| panic!("should read the {label} body: {error:?}"));

            assert_eq!(
                bytes.as_ref(),
                expected,
                "should return the exact {label} body bytes"
            );
        }
    }
}
