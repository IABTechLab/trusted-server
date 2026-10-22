//! The server-owned identity cookies: sealing, reading and formatting.
//!
//! `__Host-ts-eids-s` and every `__Host-ts-eids-r-*` cookie is sealed with
//! AES-256-GCM as `v1.<nonce>.<ciphertext and tag>`, base64url without
//! padding, with the full EC ID as associated data. A cookie copied from
//! another visitor, or left over after an EC rotation, fails to open and is
//! ignored. Values are never logged.

use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::{HeaderMap, header};

use super::input::RandomSource;

/// Lifetime of both identity cookie kinds, in seconds.
pub(crate) const COOKIE_MAX_AGE_SECS: u64 = 31_536_000;

const SEAL_VERSION: &str = "v1";
const NONCE_BYTES: usize = 12;

/// Seals `payload` with AES-256-GCM, the EC ID as associated data.
#[must_use]
pub(crate) fn seal(
    key: &[u8; 32],
    ec_id: &str,
    payload: &[u8],
    random: &dyn RandomSource,
) -> String {
    let cipher = Aes256Gcm::new_from_slice(key).expect("should accept a 32-byte key");
    let mut nonce = [0u8; NONCE_BYTES];
    random.fill(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: payload,
                aad: ec_id.as_bytes(),
            },
        )
        .expect("should encrypt a bounded in-memory payload");
    format!(
        "{SEAL_VERSION}.{}.{}",
        URL_SAFE_NO_PAD.encode(nonce),
        URL_SAFE_NO_PAD.encode(ciphertext)
    )
}

/// Opens a sealed value. `None` for any failure.
#[must_use]
pub(crate) fn open(key: &[u8; 32], ec_id: &str, value: &str) -> Option<Vec<u8>> {
    let mut parts = value.split('.');
    if parts.next()? != SEAL_VERSION {
        return None;
    }
    let nonce = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    let ciphertext = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    if parts.next().is_some() || nonce.len() != NONCE_BYTES {
        return None;
    }
    Aes256Gcm::new_from_slice(key)
        .ok()?
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: ec_id.as_bytes(),
            },
        )
        .ok()
}

/// One cookie read by exact name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CookieRead {
    /// Not present.
    Absent,
    /// Two or more cookies share the name; treated as none.
    Duplicate,
    /// Present once.
    Value(String),
}

fn cookie_pairs(headers: &HeaderMap) -> impl Iterator<Item = (&str, &str)> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .map(|(name, value)| (name.trim(), value.trim()))
}

/// Reads a cookie that must occur at most once.
#[must_use]
pub(crate) fn read_single_cookie(headers: &HeaderMap, name: &str) -> CookieRead {
    let mut found = cookie_pairs(headers)
        .filter(|(cookie, _)| *cookie == name)
        .map(|(_, value)| value);
    match (found.next(), found.next()) {
        (None, _) => CookieRead::Absent,
        (Some(_), Some(_)) => CookieRead::Duplicate,
        (Some(value), None) => CookieRead::Value(value.to_owned()),
    }
}

/// Every cookie whose name starts with `prefix`, as `(name, value)`.
#[must_use]
pub(crate) fn read_prefixed_cookies(headers: &HeaderMap, prefix: &str) -> Vec<(String, String)> {
    cookie_pairs(headers)
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// A `Set-Cookie` value with the identity cookie attributes. Host-only: no
/// `Domain`.
#[must_use]
pub(crate) fn set_cookie_header(name: &str, value: &str) -> String {
    format!("{name}={value}; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age={COOKIE_MAX_AGE_SECS}")
}

/// A `Set-Cookie` value that deletes an identity cookie.
#[must_use]
pub(crate) fn delete_cookie_header(name: &str) -> String {
    format!("{name}=; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header};

    use super::*;
    use crate::ec::identity::input::SeededRandom;

    const EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";
    const OTHER_EC: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.ABC123";

    #[test]
    fn seal_and_open_round_trip_bound_to_the_ec_id() {
        let key = [7u8; 32];
        let random = SeededRandom::new(3);
        let sealed = seal(&key, EC, br#"{"v":1}"#, &random);

        assert!(sealed.starts_with("v1."), "should carry the version prefix");
        assert_eq!(sealed.split('.').count(), 3, "should have three parts");
        assert!(!sealed.contains('='), "should be unpadded base64url");
        assert_eq!(
            open(&key, EC, &sealed).as_deref(),
            Some(&br#"{"v":1}"#[..]),
            "should open for its EC"
        );
        assert!(
            open(&key, OTHER_EC, &sealed).is_none(),
            "should not open for another EC"
        );
        assert!(
            open(&[8u8; 32], EC, &sealed).is_none(),
            "should not open with another key"
        );
        let mut tampered = sealed.clone();
        let last = tampered.pop().expect("should have a last character");
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        assert!(
            open(&key, EC, &tampered).is_none(),
            "should reject tampering"
        );
        assert!(
            open(&key, EC, "v2.x.y").is_none(),
            "should reject another version"
        );
        assert!(
            open(&key, EC, "v1.x").is_none(),
            "should reject a short value"
        );
        assert_ne!(
            seal(&key, EC, b"same", &random),
            seal(&key, EC, b"same", &random),
            "every seal draws a fresh nonce"
        );
    }

    #[test]
    fn duplicate_cookie_names_count_as_none() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("__Host-ts-eids-s=one"),
        );
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("other=1; __Host-ts-eids-s=two"),
        );
        assert_eq!(
            read_single_cookie(&headers, "__Host-ts-eids-s"),
            CookieRead::Duplicate,
            "a duplicate counts as none"
        );

        let mut single = HeaderMap::new();
        single.insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-ts-eids-s=one; x=2"),
        );
        assert_eq!(
            read_single_cookie(&single, "__Host-ts-eids-s"),
            CookieRead::Value("one".to_owned()),
            "a single cookie is read"
        );
        assert_eq!(
            read_single_cookie(&HeaderMap::new(), "__Host-ts-eids-s"),
            CookieRead::Absent,
            "absent"
        );
    }

    #[test]
    fn prefixed_cookies_and_attributes() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static(
                "__Host-ts-eids-r-0a1b2c3d=a; __Host-ts-eids-r-11111111=b; x=1",
            ),
        );
        let found = read_prefixed_cookies(&headers, "__Host-ts-eids-r-");
        assert_eq!(
            found,
            vec![
                ("__Host-ts-eids-r-0a1b2c3d".to_owned(), "a".to_owned()),
                ("__Host-ts-eids-r-11111111".to_owned(), "b".to_owned()),
            ],
            "should find both removal cookies"
        );
        assert_eq!(
            set_cookie_header("__Host-ts-eids-s", "v1.a.b"),
            "__Host-ts-eids-s=v1.a.b; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=31536000",
            "should use the identity cookie attributes and no Domain"
        );
        assert_eq!(
            delete_cookie_header("__Host-ts-eids-s"),
            "__Host-ts-eids-s=; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=0",
            "should expire with matching attributes"
        );
    }
}
