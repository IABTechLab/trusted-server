use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{aead::Aead, aead::KeyInit, XChaCha20Poly1305, XNonce};
use edgezero_core::body::Body;
use http::{header, StatusCode};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;

use crate::compat::{TlsCipher, TlsProtocol};
use crate::constants::INTERNAL_HEADERS;
use crate::settings::Settings;

/// Copy `X-*` custom headers from one request to another, skipping TS-internal headers.
///
/// This filters out all headers listed in [`INTERNAL_HEADERS`] to prevent leaking
/// internal identity, geo-enrichment, and debugging data to downstream third-party
/// services. Integrations that forward custom headers should use this utility
/// instead of manually iterating over header names.
pub fn copy_custom_headers(from: &http::Request<Body>, to: &mut http::Request<Body>) {
    for (header_name, value) in from.headers() {
        let name_str = header_name.as_str();
        if (name_str.starts_with("x-") || name_str.starts_with("X-"))
            && !INTERNAL_HEADERS.contains(&name_str)
        {
            to.headers_mut().insert(header_name.clone(), value.clone());
        }
    }
}

/// Headers that clients can spoof to hijack URL rewriting.
///
/// On Fastly Compute the service is the edge — there is no upstream proxy that
/// legitimately sets these. Stripping them forces [`RequestInfo::from_request`]
/// to fall back to the trustworthy `Host` header and Fastly SDK TLS detection.
pub(crate) const SPOOFABLE_FORWARDED_HEADERS: &[&str] = &[
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
pub fn sanitize_forwarded_headers(req: &mut http::Request<Body>) {
    for header in SPOOFABLE_FORWARDED_HEADERS {
        if req.headers().contains_key(*header) {
            log::debug!("Stripped spoofable header: {}", header);
            req.headers_mut().remove(*header);
        }
    }
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
    /// Extract request info from an HTTP request.
    ///
    /// Host fallback order (first present wins):
    /// 1. `Forwarded` header (`host=...`)
    /// 2. `X-Forwarded-Host`
    /// 3. `Host` header
    ///
    /// Scheme fallback order:
    /// 1. Fastly SDK TLS detection (via [`TlsProtocol`] / [`TlsCipher`] extensions)
    /// 2. `Forwarded` header (`proto=...`)
    /// 3. `X-Forwarded-Proto`
    /// 4. `Fastly-SSL`
    /// 5. Default `http`
    ///
    /// In production the forwarded headers are stripped by
    /// [`sanitize_forwarded_headers`] at the edge, so `Host` and SDK TLS
    /// detection are the only sources that fire.
    pub fn from_request(req: &http::Request<Body>) -> Self {
        let host = extract_request_host(req);
        let scheme = detect_request_scheme(req);

        Self { host, scheme }
    }
}

fn extract_request_host(req: &http::Request<Body>) -> String {
    get_header_str(req, "forwarded")
        .and_then(|value| parse_forwarded_param(value, "host"))
        .or_else(|| get_header_str(req, "x-forwarded-host").and_then(parse_list_header_value))
        .or_else(|| get_header_str(req, header::HOST.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// Get a header value as `&str`, returning `None` if absent or non-UTF-8.
fn get_header_str<'a>(req: &'a http::Request<Body>, name: &str) -> Option<&'a str> {
    req.headers().get(name).and_then(|v| v.to_str().ok())
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
    if scheme == "https" || scheme == "http" {
        Some(scheme)
    } else {
        None
    }
}

/// Detects the request scheme (HTTP or HTTPS).
///
/// Tries multiple sources in order of reliability:
/// 1. Fastly SDK TLS info stored in [`TlsProtocol`] / [`TlsCipher`] request extensions
/// 2. `Forwarded` header (RFC 7239)
/// 3. `X-Forwarded-Proto` header
/// 4. `Fastly-SSL` header (least reliable, can be spoofed)
/// 5. Default to HTTP
fn detect_request_scheme(req: &http::Request<Body>) -> String {
    // 1. Check Fastly TLS extensions populated by `compat::from_fastly_request_ref`.
    if let Some(TlsProtocol(Some(_))) = req.extensions().get::<TlsProtocol>() {
        log::debug!("TLS protocol detected via extension");
        return "https".to_string();
    }
    if let Some(TlsCipher(Some(_))) = req.extensions().get::<TlsCipher>() {
        log::debug!("TLS cipher detected via extension, using HTTPS");
        return "https".to_string();
    }

    // 2. Try the Forwarded header (RFC 7239)
    if let Some(forwarded_str) = get_header_str(req, "forwarded") {
        if let Some(proto) = parse_forwarded_param(forwarded_str, "proto") {
            if let Some(scheme) = normalize_scheme(proto) {
                return scheme;
            }
        }
    }

    // 3. Try X-Forwarded-Proto header
    if let Some(proto_str) = get_header_str(req, "x-forwarded-proto") {
        if let Some(value) = parse_list_header_value(proto_str) {
            if let Some(scheme) = normalize_scheme(value) {
                return scheme;
            }
        }
    }

    // 4. Check Fastly-SSL header (can be spoofed by clients, use as last resort)
    if let Some(ssl_str) = get_header_str(req, "fastly-ssl") {
        if ssl_str == "1" || ssl_str.to_lowercase() == "true" {
            return "https".to_string();
        }
    }

    // Default to HTTP
    "http".to_string()
}

/// Build a static text response with strong `ETag` and standard caching headers.
/// Handles If-None-Match to return 304 when appropriate.
///
/// # Panics
///
/// Panics if the `http::Response` builder fails (unreachable with valid status codes and headers).
pub fn serve_static_with_etag(
    body: &str,
    req: &http::Request<Body>,
    content_type: &str,
) -> http::Response<Body> {
    // Compute ETag for conditional caching
    let hash = Sha256::digest(body.as_bytes());
    let etag = format!("\"sha256-{}\"", hex::encode(hash));

    // If-None-Match handling for 304 responses
    if let Some(if_none_match) = get_header_str(req, header::IF_NONE_MATCH.as_str()) {
        if if_none_match == etag {
            return http::Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(header::ETAG, &etag)
                .header(
                    header::CACHE_CONTROL,
                    "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
                )
                .header("surrogate-control", "max-age=300")
                .header(header::VARY, "Accept-Encoding")
                .body(Body::empty())
                .expect("should build 304 response");
        }
    }

    http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CACHE_CONTROL,
            "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
        )
        .header("surrogate-control", "max-age=300")
        .header(header::ETAG, &etag)
        .header(header::VARY, "Accept-Encoding")
        .body(Body::from(body.to_string()))
        .expect("should build 200 response with body")
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
    let mut nonce = [0u8; 24];
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
    cipher
        .decrypt(nonce, ciphertext)
        .ok()
        .and_then(|pt| String::from_utf8(pt).ok())
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

    fn make_request_with_header(
        method: &str,
        uri: &str,
        name: &str,
        value: &str,
    ) -> http::Request<Body> {
        http::Request::builder()
            .method(method)
            .uri(uri)
            .header(name, value)
            .body(Body::empty())
            .expect("should build test request with header")
    }

    #[test]
    fn encode_decode_roundtrip() {
        let settings = crate::test_support::tests::create_test_settings();
        let src = "https://t.example/p.gif";
        let enc = encode_url(&settings, src);
        assert!(!enc.ends_with('='));
        let dec = match decode_url(&settings, &enc) {
            Some(s) => s,
            None => {
                panic!("decode failed for token: {}", enc);
            }
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
        let req = make_request_with_header(
            "GET",
            "https://test.example.com/page",
            "host",
            "test.example.com",
        );

        let info = RequestInfo::from_request(&req);
        assert_eq!(
            info.host, "test.example.com",
            "Host should use Host header when forwarded headers are missing"
        );
        // No TLS extensions, defaults to http.
        assert_eq!(
            info.scheme, "http",
            "Scheme should default to http without TLS extensions or forwarded headers"
        );
    }

    #[test]
    fn test_request_info_x_forwarded_host_precedence() {
        let req = http::Request::builder()
            .method("GET")
            .uri("https://test.example.com/page")
            .header("host", "internal-proxy.local")
            .header("x-forwarded-host", "public.example.com, proxy.local")
            .body(Body::empty())
            .expect("should build test request");

        let info = RequestInfo::from_request(&req);
        assert_eq!(
            info.host, "public.example.com",
            "Host should prefer X-Forwarded-Host over Host"
        );
    }

    #[test]
    fn test_request_info_scheme_from_x_forwarded_proto() {
        let req = http::Request::builder()
            .method("GET")
            .uri("https://test.example.com/page")
            .header("host", "test.example.com")
            .header("x-forwarded-proto", "https, http")
            .body(Body::empty())
            .expect("should build test request");

        let info = RequestInfo::from_request(&req);
        assert_eq!(
            info.scheme, "https",
            "Scheme should prefer the first X-Forwarded-Proto value"
        );

        let req2 = http::Request::builder()
            .method("GET")
            .uri("http://test.example.com/page")
            .header("host", "test.example.com")
            .header("x-forwarded-proto", "http")
            .body(Body::empty())
            .expect("should build test request");

        let info2 = RequestInfo::from_request(&req2);
        assert_eq!(
            info2.scheme, "http",
            "Scheme should use the X-Forwarded-Proto value when present"
        );
    }

    #[test]
    fn request_info_forwarded_header_precedence() {
        // Forwarded header takes precedence over X-Forwarded-Proto
        let req = http::Request::builder()
            .method("GET")
            .uri("https://test.example.com/page")
            .header(
                "forwarded",
                "for=192.0.2.60;proto=\"HTTPS\";host=\"public.example.com:443\"",
            )
            .header("host", "internal-proxy.local")
            .header("x-forwarded-host", "proxy.local")
            .header("x-forwarded-proto", "http")
            .body(Body::empty())
            .expect("should build test request");

        let info = RequestInfo::from_request(&req);
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
        let req =
            make_request_with_header("GET", "https://test.example.com/page", "fastly-ssl", "1");

        let info = RequestInfo::from_request(&req);
        assert_eq!(
            info.scheme, "https",
            "Scheme should fall back to Fastly-SSL when other signals are missing"
        );
    }

    #[test]
    fn test_request_info_chained_proxy_scenario() {
        // Simulate: Client (HTTPS) -> Proxy A -> Trusted Server (HTTP internally)
        let req = http::Request::builder()
            .method("GET")
            .uri("http://trusted-server.internal/page")
            .header("host", "trusted-server.internal")
            .header("x-forwarded-host", "public.example.com")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .expect("should build test request");

        let info = RequestInfo::from_request(&req);
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
        let mut req = http::Request::builder()
            .method("GET")
            .uri("https://example.com/page")
            .header("host", "legit.example.com")
            .header("forwarded", "host=evil.com;proto=https")
            .header("x-forwarded-host", "evil.com")
            .header("x-forwarded-proto", "https")
            .header("fastly-ssl", "1")
            .body(Body::empty())
            .expect("should build test request");

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
        let mut req = http::Request::builder()
            .method("GET")
            .uri("https://example.com/page")
            .header("host", "legit.example.com")
            .header("x-forwarded-host", "evil.com")
            .header("x-forwarded-proto", "http")
            .body(Body::empty())
            .expect("should build test request");

        sanitize_forwarded_headers(&mut req);
        let info = RequestInfo::from_request(&req);

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
        let from = http::Request::builder()
            .method("GET")
            .uri("https://example.com")
            .header("x-custom-1", "value1")
            .header("x-custom-2", "value2")
            .header("x-ts-ec", "should not copy")
            .header("x-geo-country", "US")
            .body(Body::empty())
            .expect("should build from request");

        let mut to = http::Request::builder()
            .method("GET")
            .uri("https://target.com")
            .body(Body::empty())
            .expect("should build to request");

        copy_custom_headers(&from, &mut to);

        assert_eq!(
            to.headers().get("x-custom-1").unwrap().to_str().unwrap(),
            "value1",
            "Should copy arbitrary x-header"
        );
        assert_eq!(
            to.headers().get("x-custom-2").unwrap().to_str().unwrap(),
            "value2",
            "Should copy arbitrary x-header (case preserved in lookup)"
        );
        assert!(
            to.headers().get("x-ts-ec").is_none(),
            "Should filter x-ts-ec"
        );
        assert!(
            to.headers().get("x-geo-country").is_none(),
            "Should filter x-geo-country"
        );
    }
}
