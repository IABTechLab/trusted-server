//! Cookie handling utilities.
//!
//! This module provides functionality for parsing and creating cookies
//! used in the trusted server system.

use cookie::{Cookie, CookieJar};
use error_stack::{Report, ResultExt};
use fastly::http::header;
use fastly::Request;

use crate::constants::COOKIE_SYNTHETIC_ID;
use crate::error::TrustedServerError;
use crate::settings::Settings;

const COOKIE_MAX_AGE: i32 = 365 * 24 * 60 * 60; // 1 year

/// Parses a cookie string into a [`CookieJar`].
///
/// Returns an empty jar if the cookie string is unparseable.
/// Individual invalid cookies are skipped rather than failing the entire parse.
pub fn parse_cookies_to_jar(s: &str) -> CookieJar {
    let cookie_str = s.trim().to_owned();
    let mut jar = CookieJar::new();
    let cookies = Cookie::split_parse(cookie_str).filter_map(Result::ok);

    for cookie in cookies {
        jar.add_original(cookie);
    }

    jar
}

/// Extracts and parses cookies from an HTTP request.
///
/// Attempts to parse the Cookie header into a [`CookieJar`] for easy access
/// to individual cookies.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if the Cookie header contains invalid UTF-8
pub fn handle_request_cookies(
    req: &Request,
) -> Result<Option<CookieJar>, Report<TrustedServerError>> {
    match req.get_header(header::COOKIE) {
        Some(header_value) => {
            let header_value_str =
                header_value
                    .to_str()
                    .change_context(TrustedServerError::InvalidHeaderValue {
                        message: "Cookie header contains invalid UTF-8".to_string(),
                    })?;
            let jar = parse_cookies_to_jar(header_value_str);
            Ok(Some(jar))
        }
        None => {
            log::debug!("No cookie header found in request");
            Ok(None)
        }
    }
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

/// Formats the `Set-Cookie` header value for the synthetic ID cookie.
#[must_use]
fn create_synthetic_cookie(settings: &Settings, synthetic_id: &str) -> String {
    format!(
        "{}={}; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={}",
        COOKIE_SYNTHETIC_ID, synthetic_id, settings.publisher.cookie_domain, COOKIE_MAX_AGE,
    )
}

/// Sets the synthetic ID cookie on the given response.
///
/// Validates `synthetic_id` against RFC 6265 `cookie-octet` rules before
/// interpolation. If the value contains unsafe characters (e.g. semicolons),
/// the cookie is not set and a warning is logged. This prevents an attacker
/// from injecting spurious cookie attributes via a controlled ID value.
///
/// `cookie_domain` comes from operator configuration and is considered trusted.
pub fn set_synthetic_cookie(
    settings: &Settings,
    response: &mut fastly::Response,
    synthetic_id: &str,
) {
    if !is_safe_cookie_value(synthetic_id) {
        log::warn!(
            "Rejecting synthetic_id for Set-Cookie: value of {} bytes contains characters illegal in a cookie value",
            synthetic_id.len()
        );
        return;
    }
    response.append_header(
        header::SET_COOKIE,
        create_synthetic_cookie(settings, synthetic_id),
    );
}

#[cfg(test)]
mod tests {
    use crate::test_support::tests::create_test_settings;

    use super::*;

    #[test]
    fn test_parse_cookies_to_jar() {
        let header_value = "c1=v1; c2=v2";
        let jar = parse_cookies_to_jar(header_value);

        assert!(jar.iter().count() == 2);
        assert_eq!(jar.get("c1").expect("should have cookie c1").value(), "v1");
        assert_eq!(jar.get("c2").expect("should have cookie c2").value(), "v2");
    }

    #[test]
    fn test_parse_cookies_to_jar_not_unique() {
        let cookie_str = "c1=v1;c1=v2";
        let jar = parse_cookies_to_jar(cookie_str);

        assert!(jar.iter().count() == 1);
        assert_eq!(jar.get("c1").expect("should have cookie c1").value(), "v2");
    }

    #[test]
    fn test_parse_cookies_to_jar_empty() {
        let cookie_str = "";
        let jar = parse_cookies_to_jar(cookie_str);

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_parse_cookies_to_jar_invalid() {
        let cookie_str = "invalid";
        let jar = parse_cookies_to_jar(cookie_str);

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_handle_request_cookies() {
        let req = Request::get("http://example.com").with_header(header::COOKIE, "c1=v1;c2=v2");
        let jar = handle_request_cookies(&req)
            .expect("should parse cookies")
            .expect("should have cookie jar");

        assert!(jar.iter().count() == 2);
        assert_eq!(jar.get("c1").expect("should have cookie c1").value(), "v1");
        assert_eq!(jar.get("c2").expect("should have cookie c2").value(), "v2");
    }

    #[test]
    fn test_handle_request_cookies_with_empty_cookie() {
        let req = Request::get("http://example.com").with_header(header::COOKIE, "");
        let jar = handle_request_cookies(&req)
            .expect("should parse cookies")
            .expect("should have cookie jar");

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_handle_request_cookies_no_cookie_header() {
        let req: Request = Request::get("https://example.com");
        let jar = handle_request_cookies(&req).expect("should handle missing cookie header");

        assert!(jar.is_none());
    }

    #[test]
    fn test_handle_request_cookies_invalid_cookie_header() {
        let req = Request::get("http://example.com").with_header(header::COOKIE, "invalid");
        let jar = handle_request_cookies(&req)
            .expect("should parse cookies")
            .expect("should have cookie jar");

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_set_synthetic_cookie() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_synthetic_cookie(&settings, &mut response, "abc123.XyZ789");

        let cookie_str = response
            .get_header(header::SET_COOKIE)
            .expect("Set-Cookie header should be present")
            .to_str()
            .expect("header should be valid UTF-8");

        assert_eq!(
            cookie_str,
            format!(
                "{}=abc123.XyZ789; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={}",
                COOKIE_SYNTHETIC_ID, settings.publisher.cookie_domain, COOKIE_MAX_AGE,
            ),
            "Set-Cookie header should match expected format"
        );
    }

    #[test]
    fn test_set_synthetic_cookie_rejects_semicolon() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_synthetic_cookie(&settings, &mut response, "evil; Domain=.attacker.com");

        assert!(
            response.get_header(header::SET_COOKIE).is_none(),
            "Set-Cookie should not be set when value contains a semicolon"
        );
    }

    #[test]
    fn test_set_synthetic_cookie_rejects_crlf() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_synthetic_cookie(&settings, &mut response, "evil\r\nX-Injected: header");

        assert!(
            response.get_header(header::SET_COOKIE).is_none(),
            "Set-Cookie should not be set when value contains CRLF"
        );
    }

    #[test]
    fn test_set_synthetic_cookie_rejects_space() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_synthetic_cookie(&settings, &mut response, "bad value");

        assert!(
            response.get_header(header::SET_COOKIE).is_none(),
            "Set-Cookie should not be set when value contains whitespace"
        );
    }

    #[test]
    fn test_is_safe_cookie_value_rejects_empty_string() {
        assert!(!is_safe_cookie_value(""), "should reject empty string");
    }

    #[test]
    fn test_is_safe_cookie_value_accepts_valid_synthetic_id_characters() {
        // Hex digits, dot separator, alphanumeric suffix — the full synthetic ID character set
        assert!(
            is_safe_cookie_value("abcdef0123456789.ABCDEFabcdef"),
            "should accept hex digits, dots, and alphanumeric characters"
        );
    }

    #[test]
    fn test_is_safe_cookie_value_rejects_non_ascii() {
        assert!(
            !is_safe_cookie_value("valüe"),
            "should reject non-ASCII UTF-8 characters"
        );
    }

    #[test]
    fn test_is_safe_cookie_value_rejects_illegal_characters() {
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
}
