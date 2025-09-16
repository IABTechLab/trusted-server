use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{aead::Aead, aead::KeyInit, XChaCha20Poly1305, XNonce};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use sha2::{Digest, Sha256};

use crate::settings::Settings;

/// Build a static text response with strong ETag and standard caching headers.
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
/// publisher's proxy_secret. This enables proxy URLs to retain the original URL in
/// clear text while still providing integrity/authenticity via a keyed digest.
///
/// Token format: Base64 URL-safe (no padding) of SHA-256("ts-proxy-v2" || secret || url)
/// - Not intended as a general HMAC; sufficient for validating unmodified URLs under a secret.
pub fn sign_clear_url(settings: &Settings, clear_url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ts-proxy-v2");
    hasher.update(settings.publisher.proxy_secret.as_bytes());
    hasher.update(clear_url.as_bytes());
    let digest = hasher.finalize();
    URL_SAFE_NO_PAD.encode(digest)
}

/// Verify a `tstoken` for the given clear-text URL.
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
pub fn compute_encrypted_sha256_token(settings: &Settings, full_url: &str) -> String {
    // Encrypt deterministically using existing helper
    let enc = encode_url(settings, full_url);
    // Decode to raw bytes (x1 + nonce + ciphertext+tag)
    let raw = URL_SAFE_NO_PAD.decode(enc.as_bytes()).unwrap_or_default();
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
}
