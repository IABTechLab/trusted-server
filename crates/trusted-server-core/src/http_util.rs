use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{XChaCha20Poly1305, XNonce, aead::Aead as _, aead::KeyInit as _};
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{Request, Response, StatusCode, header};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

use crate::constants::INTERNAL_HEADERS;
use crate::error::TrustedServerError;
use crate::platform::ClientInfo;
use crate::settings::Settings;

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

/// Headers that clients can spoof to hijack URL rewriting.
///
/// On Fastly Compute the service is the edge — there is no upstream proxy that
/// legitimately sets these. Stripping them forces [`RequestInfo::from_request`]
/// to fall back to the trustworthy `Host` header and [`ClientInfo`] TLS detection.
pub const SPOOFABLE_FORWARDED_HEADERS: &[&str] = &[
    "forwarded",
    "x-forwarded-host",
    "x-forwarded-proto",
    "fastly-ssl",
];

/// Strip forwarded headers that clients can spoof.
///
/// Call this at the edge entry point (before routing) to prevent
/// `X-Forwarded-Host: evil.com` from hijacking all URL rewriting.
/// See <https://github.com/IABTechLab/trusted-server/issues/409>.
pub fn sanitize_forwarded_headers(req: &mut Request<EdgeBody>) {
    for header in SPOOFABLE_FORWARDED_HEADERS {
        if req.headers().contains_key(*header) {
            log::debug!("Stripped spoofable header: {header}");
            req.headers_mut().remove(*header);
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

/// Extracted request information for host rewriting.
///
/// This struct captures the effective host and scheme from an incoming request.
/// The parser checks forwarded headers (`Forwarded`, `X-Forwarded-Host`,
/// `X-Forwarded-Proto`) as fallbacks, but on the Fastly edge
/// [`sanitize_forwarded_headers`] strips those headers before this method is
/// called, so the `Host` header and Fastly SDK TLS detection are the effective
/// sources in production.
#[derive(Debug, Clone)]
pub struct RequestInfo {
    /// The effective host for URL rewriting (typically the `Host` header after edge sanitization).
    pub host: String,
    /// The effective scheme (typically from Fastly SDK TLS detection after edge sanitization).
    pub scheme: String,
}

impl RequestInfo {
    /// Extract request info from a Fastly request.
    ///
    /// Host fallback order (first present wins):
    /// 1. `Forwarded` header (`host=...`)
    /// 2. `X-Forwarded-Host`
    /// 3. `Host` header
    ///
    /// Scheme fallback order:
    /// 1. Fastly SDK TLS detection
    /// 2. `Forwarded` header (`proto=...`)
    /// 3. `X-Forwarded-Proto`
    /// 4. `Fastly-SSL`
    /// 5. Default `http`
    ///
    /// In production the forwarded headers are stripped by
    /// [`sanitize_forwarded_headers`] at the edge, so `Host` and
    /// [`ClientInfo`] TLS detection are the only sources that fire.
    pub fn from_request(req: &Request<EdgeBody>, client_info: &ClientInfo) -> Self {
        let host = extract_request_host(req);
        let scheme = detect_request_scheme(
            req,
            client_info.tls_protocol.as_deref(),
            client_info.tls_cipher.as_deref(),
        );

        Self { host, scheme }
    }
}

fn extract_request_host(req: &Request<EdgeBody>) -> String {
    req.headers()
        .get("forwarded")
        .and_then(|h| h.to_str().ok())
        .and_then(|value| parse_forwarded_param(value, "host"))
        .or_else(|| {
            req.headers()
                .get("x-forwarded-host")
                .and_then(|h| h.to_str().ok())
                .and_then(parse_list_header_value)
        })
        .or_else(|| {
            req.headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
        })
        .unwrap_or_default()
        .to_owned()
}

fn parse_forwarded_param<'a>(forwarded: &'a str, param: &str) -> Option<&'a str> {
    for entry in forwarded.split(',') {
        for part in entry.split(';') {
            let mut iter = part.splitn(2, '=');
            let key = iter.next().unwrap_or("").trim();
            let value = iter.next().unwrap_or("").trim();
            if key.is_empty() || value.is_empty() {
                continue;
            }
            if key.eq_ignore_ascii_case(param) {
                let value = strip_quotes(value);
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    None
}

fn parse_list_header_value(value: &str) -> Option<&str> {
    value
        .split(',')
        .map(str::trim)
        .find(|part| !part.is_empty())
        .map(strip_quotes)
        .filter(|part| !part.is_empty())
}

fn strip_quotes(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

fn normalize_scheme(value: &str) -> Option<String> {
    let scheme = value.trim().to_ascii_lowercase();
    (scheme == "https" || scheme == "http").then_some(scheme)
}

/// Detects the request scheme (HTTP or HTTPS) using Fastly SDK methods and headers.
///
/// Tries multiple methods in order of reliability:
/// 1. Fastly SDK TLS detection methods (most reliable)
/// 2. Forwarded header (RFC 7239)
/// 3. X-Forwarded-Proto header
/// 4. Fastly-SSL header (trusted on `EdgeZero` path; can be spoofed on legacy path)
/// 5. Default to HTTP
fn detect_request_scheme(
    req: &Request<EdgeBody>,
    tls_protocol: Option<&str>,
    tls_cipher: Option<&str>,
) -> String {
    // 1. First try ClientInfo TLS fields populated at the adapter entry point.
    if let Some(tls_protocol) = tls_protocol {
        log::debug!("TLS protocol detected: {tls_protocol}");
        return "https".to_owned();
    }

    // Also check TLS cipher - if present, connection is HTTPS.
    if tls_cipher.is_some() {
        log::debug!("TLS cipher detected, using HTTPS");
        return "https".to_owned();
    }

    // 2. Try the Forwarded header (RFC 7239)
    if let Some(forwarded) = req.headers().get("forwarded")
        && let Ok(forwarded_str) = forwarded.to_str()
        && let Some(proto) = parse_forwarded_param(forwarded_str, "proto")
        && let Some(scheme) = normalize_scheme(proto)
    {
        return scheme;
    }

    // 3. Try X-Forwarded-Proto header
    if let Some(proto) = req.headers().get("x-forwarded-proto")
        && let Ok(proto_str) = proto.to_str()
        && let Some(value) = parse_list_header_value(proto_str)
        && let Some(scheme) = normalize_scheme(value)
    {
        return scheme;
    }

    // 4. Check Fastly-SSL header. On the `EdgeZero` path this is injected from
    //    authoritative Fastly TLS metadata after spoofable headers are stripped,
    //    so it is reliable. On direct or legacy paths it can be spoofed by clients.
    //
    //    Layering wart: this is a vendor-specific header name living in
    //    platform-neutral core. It is only a fallback — signal #1 above
    //    (`ClientInfo::tls_protocol`) is the neutral path adapters populate. The
    //    `fastly-ssl` fallback (plus its entry in `SPOOFABLE_FORWARDED_HEADERS`
    //    and the origin-forwarding strip in `publisher::rewrite_origin_request`)
    //    should be replaced by a platform-neutral scheme signal in a separate
    //    change, after confirming the legacy path is covered by `ClientInfo`.
    if let Some(ssl) = req.headers().get("fastly-ssl")
        && let Ok(ssl_str) = ssl.to_str()
        && (ssl_str == "1" || ssl_str.to_lowercase() == "true")
    {
        return "https".to_owned();
    }

    // Default to HTTP
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
) -> Response<EdgeBody> {
    // Compute ETag for conditional caching
    let hash = Sha256::digest(body.as_bytes());
    let etag = format!("\"sha256-{}\"", hex::encode(hash));

    // If-None-Match handling for 304 responses
    if let Some(if_none_match) = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|h| h.to_str().ok())
        && if_none_match == etag
    {
        return Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(header::ETAG, &etag)
                .header(
                    header::CACHE_CONTROL,
                    "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
                )
                .header("surrogate-control", "max-age=300")
                .header(header::VARY, "Accept-Encoding")
                .body(EdgeBody::empty())
                .expect("should build 304 static response");
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CACHE_CONTROL,
            "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
        )
        .header("surrogate-control", "max-age=300")
        .header(header::ETAG, &etag)
        .header(header::VARY, "Accept-Encoding")
        .body(EdgeBody::from(body.as_bytes()))
        .expect("should build static response")
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
    let nonce = XNonce::from_slice(&nonce);

    let ciphertext = cipher
        .encrypt(nonce, plaintext_url.as_bytes())
        .expect("encryption failure");

    let mut out: Vec<u8> = Vec::with_capacity(2 + 24 + ciphertext.len());
    out.extend_from_slice(b"x1");
    out.extend_from_slice(nonce);
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
    let nonce = XNonce::from_slice(nonce_bytes);
    let ciphertext = &data[2 + 24..];

    let key_bytes = Sha256::digest(settings.publisher.proxy_secret.expose().as_bytes());
    let cipher = XChaCha20Poly1305::new(&key_bytes);
    let pt = cipher.decrypt(nonce, ciphertext).ok()?;
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

/// Return an error if `bytes` exceeds `limit`.
///
/// # Errors
///
/// Returns [`TrustedServerError::RequestTooLarge`] when `bytes.len() > limit`.
pub fn enforce_max_body_size(
    bytes: &[u8],
    limit: usize,
    what: &str,
) -> Result<(), Report<TrustedServerError>> {
    if bytes.len() > limit {
        return Err(Report::new(TrustedServerError::RequestTooLarge {
            message: format!(
                "{what} payload {} exceeds limit of {limit} bytes",
                bytes.len()
            ),
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ClientInfo;
    use http::{HeaderName, HeaderValue, Method};

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
    fn test_request_info_from_host_header() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        set_header(&mut req, "host", "test.example.com");

        let info = RequestInfo::from_request(&req, &default_client_info());
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
    fn test_request_info_x_forwarded_host_precedence() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        set_header(&mut req, "host", "internal-proxy.local");
        set_header(
            &mut req,
            "x-forwarded-host",
            "public.example.com, proxy.local",
        );

        let info = RequestInfo::from_request(&req, &default_client_info());
        assert_eq!(
            info.host, "public.example.com",
            "Host should prefer X-Forwarded-Host over Host"
        );
    }

    #[test]
    fn test_request_info_scheme_from_x_forwarded_proto() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        set_header(&mut req, "host", "test.example.com");
        set_header(&mut req, "x-forwarded-proto", "https, http");

        let info = RequestInfo::from_request(&req, &default_client_info());
        assert_eq!(
            info.scheme, "https",
            "Scheme should prefer the first X-Forwarded-Proto value"
        );

        // Test HTTP
        let mut req = build_request(Method::GET, "http://test.example.com/page");
        set_header(&mut req, "host", "test.example.com");
        set_header(&mut req, "x-forwarded-proto", "http");

        let info = RequestInfo::from_request(&req, &default_client_info());
        assert_eq!(
            info.scheme, "http",
            "Scheme should use the X-Forwarded-Proto value when present"
        );
    }

    #[test]
    fn request_info_forwarded_header_precedence() {
        // Forwarded header takes precedence over X-Forwarded-Proto
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        set_header(
            &mut req,
            "forwarded",
            "for=192.0.2.60;proto=\"HTTPS\";host=\"public.example.com:443\"",
        );
        set_header(&mut req, "host", "internal-proxy.local");
        set_header(&mut req, "x-forwarded-host", "proxy.local");
        set_header(&mut req, "x-forwarded-proto", "http");

        let info = RequestInfo::from_request(&req, &default_client_info());
        assert_eq!(
            info.host, "public.example.com:443",
            "Host should prefer Forwarded host over X-Forwarded-Host"
        );
        assert_eq!(
            info.scheme, "https",
            "Scheme should prefer Forwarded proto over X-Forwarded-Proto"
        );
    }

    #[test]
    fn test_request_info_scheme_from_fastly_ssl() {
        let mut req = build_request(Method::GET, "https://test.example.com/page");
        set_header(&mut req, "fastly-ssl", "1");

        let info = RequestInfo::from_request(&req, &default_client_info());
        assert_eq!(
            info.scheme, "https",
            "Scheme should fall back to Fastly-SSL when other signals are missing"
        );
    }

    #[test]
    fn test_request_info_chained_proxy_scenario() {
        // Simulate: Client (HTTPS) -> Proxy A -> Trusted Server (HTTP internally)
        // Proxy A sets X-Forwarded-Host and X-Forwarded-Proto
        let mut req = build_request(Method::GET, "http://trusted-server.internal/page");
        set_header(&mut req, "host", "trusted-server.internal");
        set_header(&mut req, "x-forwarded-host", "public.example.com");
        set_header(&mut req, "x-forwarded-proto", "https");

        let info = RequestInfo::from_request(&req, &default_client_info());
        assert_eq!(
            info.host, "public.example.com",
            "Host should use X-Forwarded-Host in chained proxy scenarios"
        );
        assert_eq!(
            info.scheme, "https",
            "Scheme should use X-Forwarded-Proto in chained proxy scenarios"
        );
    }

    // Sanitization tests

    #[test]
    fn sanitize_removes_all_spoofable_headers() {
        let mut req = build_request(Method::GET, "https://example.com/page");
        set_header(&mut req, "host", "legit.example.com");
        set_header(&mut req, "forwarded", "host=evil.com;proto=https");
        set_header(&mut req, "x-forwarded-host", "evil.com");
        set_header(&mut req, "x-forwarded-proto", "https");
        set_header(&mut req, "fastly-ssl", "1");

        sanitize_forwarded_headers(&mut req);

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

        sanitize_forwarded_headers(&mut req);
        let info = RequestInfo::from_request(&req, &default_client_info());

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

        let info = RequestInfo::from_request(&req, &client_info);

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

        let info = RequestInfo::from_request(&req, &client_info);

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
}
