use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{aead::Aead, aead::KeyInit, XChaCha20Poly1305, XNonce};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use sha2::{Digest, Sha256};

use crate::constants::INTERNAL_HEADERS;
use crate::settings::Settings;

/// Copy `X-*` custom headers from one request to another, skipping TS-internal headers.
///
/// This filters out all headers listed in [`INTERNAL_HEADERS`] to prevent leaking
/// internal identity, geo-enrichment, and debugging data to downstream third-party
/// services. Integrations that forward custom headers should use this utility
/// instead of manually iterating over header names.
pub fn copy_custom_headers(from: &Request, to: &mut Request) {
    for header_name in from.get_header_names() {
        let name_str = header_name.as_str();
        if (name_str.starts_with("x-") || name_str.starts_with("X-"))
            && !INTERNAL_HEADERS.contains(&name_str)
        {
            if let Some(value) = from.get_header(header_name) {
                to.set_header(header_name, value);
            }
        }
    }
}

/// Extracted request information for host rewriting.
///
/// This struct captures the effective host and scheme from an incoming request,
/// accounting for proxy headers like `X-Forwarded-Host` and `X-Forwarded-Proto`.
#[derive(Debug, Clone)]
pub struct RequestInfo {
    /// The effective host for URL rewriting (from Forwarded, X-Forwarded-Host, or Host header)
    pub host: String,
    /// The effective scheme (from TLS detection, Forwarded, X-Forwarded-Proto, or default)
    pub scheme: String,
}

impl RequestInfo {
    /// Extract request info from a Fastly request.
    ///
    /// Host priority:
    /// 1. `Forwarded` header (RFC 7239, `host=...`)
    /// 2. `X-Forwarded-Host` header (for chained proxy setups)
    /// 3. `Host` header
    ///
    /// Scheme priority:
    /// 1. Fastly SDK TLS detection (most reliable)
    /// 2. `Forwarded` header (RFC 7239, `proto=https`)
    /// 3. `X-Forwarded-Proto` header
    /// 4. `Fastly-SSL` header
    /// 5. Default to `http`
    pub fn from_request(req: &Request) -> Self {
        let host = extract_request_host(req);
        let scheme = detect_request_scheme(req);

        Self { host, scheme }
    }
}

fn extract_request_host(req: &Request) -> String {
    req.get_header("forwarded")
        .and_then(|h| h.to_str().ok())
        .and_then(|value| parse_forwarded_param(value, "host"))
        .or_else(|| {
            req.get_header("x-forwarded-host")
                .and_then(|h| h.to_str().ok())
                .and_then(parse_list_header_value)
        })
        .or_else(|| req.get_header(header::HOST).and_then(|h| h.to_str().ok()))
        .unwrap_or_default()
        .to_string()
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

/// Detects the request scheme (HTTP or HTTPS) using Fastly SDK methods and headers.
///
/// Tries multiple methods in order of reliability:
/// 1. Fastly SDK TLS detection methods (most reliable)
/// 2. Forwarded header (RFC 7239)
/// 3. X-Forwarded-Proto header
/// 4. Fastly-SSL header (least reliable, can be spoofed)
/// 5. Default to HTTP
fn detect_request_scheme(req: &Request) -> String {
    // 1. First try Fastly SDK's built-in TLS detection methods
    if let Some(tls_protocol) = req.get_tls_protocol() {
        log::debug!("TLS protocol detected: {}", tls_protocol);
        return "https".to_string();
    }

    // Also check TLS cipher - if present, connection is HTTPS
    if req.get_tls_cipher_openssl_name().is_some() {
        log::debug!("TLS cipher detected, using HTTPS");
        return "https".to_string();
    }

    // 2. Try the Forwarded header (RFC 7239)
    if let Some(forwarded) = req.get_header("forwarded") {
        if let Ok(forwarded_str) = forwarded.to_str() {
            if let Some(proto) = parse_forwarded_param(forwarded_str, "proto") {
                if let Some(scheme) = normalize_scheme(proto) {
                    return scheme;
                }
            }
        }
    }

    // 3. Try X-Forwarded-Proto header
    if let Some(proto) = req.get_header("x-forwarded-proto") {
        if let Ok(proto_str) = proto.to_str() {
            if let Some(value) = parse_list_header_value(proto_str) {
                if let Some(scheme) = normalize_scheme(value) {
                    return scheme;
                }
            }
        }
    }

    // 4. Check Fastly-SSL header (can be spoofed by clients, use as last resort)
    if let Some(ssl) = req.get_header("fastly-ssl") {
        if let Ok(ssl_str) = ssl.to_str() {
            if ssl_str == "1" || ssl_str.to_lowercase() == "true" {
                return "https".to_string();
            }
        }
    }

    // Default to HTTP
    "http".to_string()
}

/// Build a static text response with strong `ETag` and standard caching headers.
/// Handles If-None-Match to return 304 when appropriate.
pub fn serve_static_with_etag(body: &str, req: &Request, content_type: &str) -> Response {
    // Compute ETag for conditional caching
    let hash = Sha256::digest(body.as_bytes());
    let etag = format!("\"sha256-{}\"", hex::encode(hash));

    // If-None-Match handling for 304 responses
    if let Some(if_none_match) = req
        .get_header(header::IF_NONE_MATCH)
        .and_then(|h| h.to_str().ok())
    {
        if if_none_match == etag {
            return Response::from_status(StatusCode::NOT_MODIFIED)
                .with_header(header::ETAG, &etag)
                .with_header(
                    header::CACHE_CONTROL,
                    "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
                )
                .with_header("surrogate-control", "max-age=300")
                .with_header(header::VARY, "Accept-Encoding");
        }
    }

    Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, content_type)
        .with_header(
            header::CACHE_CONTROL,
            "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
        )
        .with_header("surrogate-control", "max-age=300")
        .with_header(header::ETAG, &etag)
        .with_header(header::VARY, "Accept-Encoding")
        .with_body(body)
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
    let key_bytes = Sha256::digest(settings.publisher.proxy_secret.as_bytes());
    let cipher = XChaCha20Poly1305::new(&key_bytes);

    // Deterministic 24-byte nonce derived from secret and plaintext (stable tokens)
    let mut hasher = Sha256::new();
    hasher.update(b"ts-proxy-x1");
    hasher.update(settings.publisher.proxy_secret.as_bytes());
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

    let key_bytes = Sha256::digest(settings.publisher.proxy_secret.as_bytes());
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
    hasher.update(settings.publisher.proxy_secret.as_bytes());
    hasher.update(clear_url.as_bytes());
    let digest = hasher.finalize();
    URL_SAFE_NO_PAD.encode(digest)
}

/// Verify a `tstoken` for the given clear-text URL.
#[must_use]
pub fn verify_clear_url_signature(settings: &Settings, clear_url: &str, token: &str) -> bool {
    sign_clear_url(settings, clear_url) == token
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

    // RequestInfo tests

    #[test]
    fn test_request_info_from_host_header() {
        let mut req = Request::new(fastly::http::Method::GET, "https://test.example.com/page");
        req.set_header("host", "test.example.com");

        let info = RequestInfo::from_request(&req);
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
        let mut req = Request::new(fastly::http::Method::GET, "https://test.example.com/page");
        req.set_header("host", "internal-proxy.local");
        req.set_header("x-forwarded-host", "public.example.com, proxy.local");

        let info = RequestInfo::from_request(&req);
        assert_eq!(
            info.host, "public.example.com",
            "Host should prefer X-Forwarded-Host over Host"
        );
    }

    #[test]
    fn test_request_info_scheme_from_x_forwarded_proto() {
        let mut req = Request::new(fastly::http::Method::GET, "https://test.example.com/page");
        req.set_header("host", "test.example.com");
        req.set_header("x-forwarded-proto", "https, http");

        let info = RequestInfo::from_request(&req);
        assert_eq!(
            info.scheme, "https",
            "Scheme should prefer the first X-Forwarded-Proto value"
        );

        // Test HTTP
        let mut req = Request::new(fastly::http::Method::GET, "http://test.example.com/page");
        req.set_header("host", "test.example.com");
        req.set_header("x-forwarded-proto", "http");

        let info = RequestInfo::from_request(&req);
        assert_eq!(
            info.scheme, "http",
            "Scheme should use the X-Forwarded-Proto value when present"
        );
    }

    #[test]
    fn request_info_forwarded_header_precedence() {
        // Forwarded header takes precedence over X-Forwarded-Proto
        let mut req = Request::new(fastly::http::Method::GET, "https://test.example.com/page");
        req.set_header(
            "forwarded",
            "for=192.0.2.60;proto=\"HTTPS\";host=\"public.example.com:443\"",
        );
        req.set_header("host", "internal-proxy.local");
        req.set_header("x-forwarded-host", "proxy.local");
        req.set_header("x-forwarded-proto", "http");

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
        let mut req = Request::new(fastly::http::Method::GET, "https://test.example.com/page");
        req.set_header("fastly-ssl", "1");

        let info = RequestInfo::from_request(&req);
        assert_eq!(
            info.scheme, "https",
            "Scheme should fall back to Fastly-SSL when other signals are missing"
        );
    }

    #[test]
    fn test_request_info_chained_proxy_scenario() {
        // Simulate: Client (HTTPS) -> Proxy A -> Trusted Server (HTTP internally)
        // Proxy A sets X-Forwarded-Host and X-Forwarded-Proto
        let mut req = Request::new(
            fastly::http::Method::GET,
            "http://trusted-server.internal/page",
        );
        req.set_header("host", "trusted-server.internal");
        req.set_header("x-forwarded-host", "public.example.com");
        req.set_header("x-forwarded-proto", "https");

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

    #[test]
    fn test_copy_custom_headers_filters_internal() {
        let mut req = Request::new(fastly::http::Method::GET, "https://example.com");
        req.set_header("x-custom-1", "value1");
        // HeaderName is case-insensitive and always lowercase, but set_header accepts strings
        req.set_header("X-Custom-2", "value2");
        req.set_header("x-synthetic-id", "should not copy");
        req.set_header("x-geo-country", "US");

        let mut target = Request::new(fastly::http::Method::GET, "https://target.com");
        copy_custom_headers(&req, &mut target);

        assert_eq!(
            target.get_header("x-custom-1").unwrap().to_str().unwrap(),
            "value1",
            "Should copy arbitrary x-header"
        );
        assert_eq!(
            target.get_header("x-custom-2").unwrap().to_str().unwrap(),
            "value2",
            "Should copy arbitrary X-header (case insensitive)"
        );
        assert!(
            target.get_header("x-synthetic-id").is_none(),
            "Should filter x-synthetic-id"
        );
        assert!(
            target.get_header("x-geo-country").is_none(),
            "Should filter x-geo-country"
        );
    }
}
