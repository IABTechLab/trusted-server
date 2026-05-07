//! EC cookie creation and expiration helpers.
//!
//! These functions handle the `Set-Cookie` header for the `ts-ec` cookie.
//! Cookie attributes follow current best practices:
//!
//! - `Domain` is computed as `.{publisher.domain}` for subdomain coverage
//! - `Path=/` makes the cookie available on all paths
//! - `Secure` restricts to HTTPS
//! - `SameSite=Lax` provides CSRF protection while allowing top-level navigations
//! - `Max-Age` of 1 year (or 0 to expire)
//! - `HttpOnly` prevents client-side JS from reading the cookie via
//!   `document.cookie`, providing XSS defense-in-depth. The identify
//!   endpoint (`/_ts/api/v1/identify`) exposes the EC ID in its response
//!   body and `x-ts-ec` header for legitimate JS use cases.

use std::borrow::Cow;

use fastly::http::header;

use crate::constants::COOKIE_TS_EC;
use crate::settings::Settings;

/// Maximum age for the EC cookie (1 year in seconds).
const COOKIE_MAX_AGE: i32 = 365 * 24 * 60 * 60;

fn is_allowed_ec_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
}

// Outbound allowlist for cookie sanitization: permits [a-zA-Z0-9._-] as a
// defense-in-depth backstop when setting the Set-Cookie header. This is
// intentionally broader than the inbound format validator
// (`generation::is_valid_ec_id`), which enforces the exact
// `<64-hex>.<6-alphanumeric>` structure and is used to reject untrusted
// request values before they enter the system.
#[must_use]
pub(crate) fn ec_id_has_only_allowed_chars(ec_id: &str) -> bool {
    ec_id.chars().all(is_allowed_ec_id_char)
}

fn sanitize_ec_id_for_cookie(ec_id: &str) -> Cow<'_, str> {
    if ec_id_has_only_allowed_chars(ec_id) {
        return Cow::Borrowed(ec_id);
    }

    let safe_id = ec_id
        .chars()
        .filter(|c| is_allowed_ec_id_char(*c))
        .collect::<String>();

    log::warn!(
        "Stripped disallowed characters from EC ID before setting cookie (len {} -> {}); \
         callers should reject invalid request IDs before cookie creation",
        ec_id.len(),
        safe_id.len(),
    );

    Cow::Owned(safe_id)
}

/// Returns `true` if every byte in `value` is a valid RFC 6265 `cookie-octet`.
/// An empty string is always rejected.
///
/// RFC 6265 restricts cookie values to printable US-ASCII excluding whitespace,
/// double-quote, comma, semicolon, and backslash. Rejecting these characters
/// prevents header-injection attacks where a crafted value could append
/// spurious cookie attributes (e.g. `evil; Domain=.attacker.com`).
///
/// Non-ASCII characters (multi-byte UTF-8) are always rejected because their
/// byte values exceed `0x7E`.
#[must_use]
fn is_safe_cookie_value(value: &str) -> bool {
    // RFC 6265 §4.1.1 cookie-octet:
    //   0x21        — '!'
    //   0x23–0x2B  — '#' through '+'   (excludes 0x22 DQUOTE)
    //   0x2D–0x3A  — '-' through ':'   (excludes 0x2C comma)
    //   0x3C–0x5B  — '<' through '['   (excludes 0x3B semicolon)
    //   0x5D–0x7E  — ']' through '~'   (excludes 0x5C backslash, 0x7F DEL)
    // All control characters (0x00–0x20) and non-ASCII (0x80+) are also excluded.
    !value.is_empty()
        && value
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E))
}

/// Formats a `Set-Cookie` header value for the EC cookie.
///
/// Centralises the cookie attribute string so that changes to security
/// attributes (e.g. adding `Partitioned`) only need updating in one place.
fn format_set_cookie(domain: &str, value: &str, max_age: i32) -> String {
    format!(
        "{}={}; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={}; HttpOnly",
        COOKIE_TS_EC, value, domain, max_age,
    )
}

/// Creates an EC cookie `Set-Cookie` header value.
///
/// Per spec §5.2, the EC cookie domain is computed from
/// `settings.publisher.domain` (not `cookie_domain`) to ensure the EC
/// cookie is always scoped to the publisher's apex domain. The EC ID is
/// sanitized through a narrow outbound allowlist as a defense-in-depth
/// backstop against header injection.
#[must_use]
pub(crate) fn create_ec_cookie(settings: &Settings, ec_id: &str) -> String {
    let safe_id = sanitize_ec_id_for_cookie(ec_id);

    format_set_cookie(
        &settings.publisher.ec_cookie_domain(),
        safe_id.as_ref(),
        COOKIE_MAX_AGE,
    )
}

/// Sets the EC ID cookie on the given response.
///
/// Validates `ec_id` against RFC 6265 `cookie-octet` rules before
/// interpolation. If the value contains unsafe characters (e.g. semicolons),
/// the cookie is not set and a warning is logged. This prevents an attacker
/// from injecting spurious cookie attributes via a controlled ID value.
///
/// `cookie_domain` comes from operator configuration and is considered trusted.
///
/// # Panics (debug only)
///
/// Debug-asserts that `ec_id` passes [`super::generation::is_valid_ec_id`]
/// as a defense-in-depth check against cookie injection.
pub fn set_ec_cookie(settings: &Settings, response: &mut fastly::Response, ec_id: &str) {
    if !is_safe_cookie_value(ec_id) {
        log::warn!(
            "Rejecting EC ID for Set-Cookie: value of {} bytes contains characters illegal in a cookie value",
            ec_id.len()
        );
        return;
    }

    debug_assert!(
        super::generation::is_valid_ec_id(ec_id),
        "EC ID must be validated before cookie creation: got '{ec_id}'"
    );

    response.append_header(header::SET_COOKIE, create_ec_cookie(settings, ec_id));
}

/// Expires the EC cookie by setting `Max-Age=0`.
///
/// Used when a user revokes consent — the browser will delete the cookie
/// on receipt of this header.
pub fn expire_ec_cookie(settings: &Settings, response: &mut fastly::Response) {
    response.append_header(
        header::SET_COOKIE,
        format_set_cookie(&settings.publisher.ec_cookie_domain(), "", 0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::header;

    /// A valid EC ID for use in cookie tests.
    const TEST_EC_ID: &str =
        "aaaaaaaabbbbbbbbccccccccddddddddeeeeeeeeffffffff0000000011111111.abcXYZ";

    #[test]
    fn create_ec_cookie_uses_computed_domain() {
        let settings = create_test_settings();
        let result = create_ec_cookie(&settings, TEST_EC_ID);

        assert_eq!(
            result,
            format!(
                "{}={}; Domain=.{}; Path=/; Secure; SameSite=Lax; Max-Age={}; HttpOnly",
                COOKIE_TS_EC, TEST_EC_ID, settings.publisher.domain, COOKIE_MAX_AGE,
            ),
            "should use computed cookie domain (.{{domain}})"
        );
    }

    #[test]
    fn set_ec_cookie_appends_header() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_ec_cookie(&settings, &mut response, TEST_EC_ID);

        let cookie_header = response
            .get_header(header::SET_COOKIE)
            .expect("should have Set-Cookie header");
        let cookie_str = cookie_header.to_str().expect("should be valid UTF-8");

        assert_eq!(
            cookie_str,
            create_ec_cookie(&settings, TEST_EC_ID),
            "should match create_ec_cookie output"
        );
    }

    #[test]
    fn create_ec_cookie_sanitizes_disallowed_chars_in_id() {
        let settings = create_test_settings();
        let result = create_ec_cookie(&settings, "evil;injected\r\nfoo=bar\0baz");
        let value = result
            .strip_prefix(&format!("{}=", COOKIE_TS_EC))
            .and_then(|s| s.split_once(';').map(|(v, _)| v))
            .expect("should have cookie value portion");

        assert_eq!(
            value, "evilinjectedfoobarbaz",
            "should strip disallowed characters and preserve safe chars"
        );
    }

    #[test]
    fn create_ec_cookie_preserves_well_formed_id() {
        let settings = create_test_settings();
        let id = "abc123def0123456789abcdef0123456789abcdef0123456789abcdef01234567.xk92ab";
        let result = create_ec_cookie(&settings, id);
        let value = result
            .strip_prefix(&format!("{}=", COOKIE_TS_EC))
            .and_then(|s| s.split_once(';').map(|(v, _)| v))
            .expect("should have cookie value portion");

        assert_eq!(value, id, "should not modify a well-formed EC ID");
    }

    #[test]
    fn set_ec_cookie_rejects_semicolon() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_ec_cookie(&settings, &mut response, "evil; Domain=.attacker.com");

        assert!(
            response.get_header(header::SET_COOKIE).is_none(),
            "should not set Set-Cookie when value contains a semicolon"
        );
    }

    #[test]
    fn set_ec_cookie_rejects_crlf() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_ec_cookie(&settings, &mut response, "evil\r\nX-Injected: header");

        assert!(
            response.get_header(header::SET_COOKIE).is_none(),
            "should not set Set-Cookie when value contains CRLF"
        );
    }

    #[test]
    fn set_ec_cookie_rejects_space() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_ec_cookie(&settings, &mut response, "bad value");

        assert!(
            response.get_header(header::SET_COOKIE).is_none(),
            "should not set Set-Cookie when value contains whitespace"
        );
    }

    #[test]
    fn is_safe_cookie_value_rejects_empty_string() {
        assert!(!is_safe_cookie_value(""), "should reject empty string");
    }

    #[test]
    fn is_safe_cookie_value_accepts_valid_ec_id_characters() {
        assert!(
            is_safe_cookie_value("abcdef0123456789.ABCDEFabcdef"),
            "should accept hex digits, dots, and alphanumeric characters"
        );
    }

    #[test]
    fn is_safe_cookie_value_rejects_non_ascii() {
        assert!(
            !is_safe_cookie_value("valüe"),
            "should reject non-ASCII UTF-8 characters"
        );
    }

    #[test]
    fn is_safe_cookie_value_rejects_illegal_characters() {
        assert!(!is_safe_cookie_value("val;ue"), "should reject semicolon");
        assert!(!is_safe_cookie_value("val,ue"), "should reject comma");
        assert!(
            !is_safe_cookie_value("val\"ue"),
            "should reject double-quote"
        );
        assert!(!is_safe_cookie_value("val\\ue"), "should reject backslash");
        assert!(!is_safe_cookie_value("val ue"), "should reject space");
        assert!(
            !is_safe_cookie_value("val\x00ue"),
            "should reject null byte"
        );
        assert!(
            !is_safe_cookie_value("val\x7fue"),
            "should reject DEL character"
        );
    }

    #[test]
    fn expire_ec_cookie_sets_max_age_zero() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        expire_ec_cookie(&settings, &mut response);

        let cookie_header = response
            .get_header(header::SET_COOKIE)
            .expect("should have Set-Cookie header");
        let cookie_str = cookie_header.to_str().expect("should be valid UTF-8");

        assert!(
            cookie_str.contains("Max-Age=0"),
            "should set Max-Age=0 to expire cookie"
        );
        assert!(
            cookie_str.starts_with(&format!("{}=;", COOKIE_TS_EC)),
            "should clear cookie value"
        );
        assert!(
            cookie_str.contains(&format!("Domain=.{}", settings.publisher.domain)),
            "should use computed cookie domain"
        );
    }

    #[test]
    fn expire_ec_cookie_matches_security_attributes() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        expire_ec_cookie(&settings, &mut response);

        let cookie_header = response
            .get_header(header::SET_COOKIE)
            .expect("should have Set-Cookie header");
        let cookie_str = cookie_header.to_str().expect("should be valid UTF-8");

        assert_eq!(
            cookie_str,
            format!(
                "{}=; Domain=.{}; Path=/; Secure; SameSite=Lax; Max-Age=0; HttpOnly",
                COOKIE_TS_EC, settings.publisher.domain,
            ),
            "expiry cookie should retain the same security attributes as the live cookie"
        );
    }
}
