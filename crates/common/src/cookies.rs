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

/// Creates a synthetic ID cookie string.
///
/// Generates a `Set-Cookie` header value with the following security attributes:
/// - `Secure`: transmitted over HTTPS only.
/// - `HttpOnly`: inaccessible to JavaScript (`document.cookie`), blocking XSS exfiltration.
///   Safe to set because integrations receive the synthetic ID via the `x-synthetic-id`
///   response header instead of reading it from the cookie directly.
/// - `SameSite=Lax`: sent on same-site requests and top-level cross-site navigations.
///   `Strict` is intentionally avoided — it would suppress the cookie on the first
///   request when a user arrives from an external page, breaking first-visit attribution.
/// - `Max-Age`: 1 year retention.
///
/// # Panics
///
/// Panics if `cookie_domain` in settings contains cookie metacharacters (`;`, `\n`, `\r`).
/// This indicates a configuration error and is enforced in all build profiles.
#[must_use]
pub fn create_synthetic_cookie(settings: &Settings, synthetic_id: &str) -> String {
    // Sanitize synthetic_id at runtime: strip cookie metacharacters to prevent
    // header injection when the ID originates from untrusted input (e.g., the
    // x-synthetic-id request header or an inbound cookie).
    let safe_id: String = synthetic_id
        .chars()
        .filter(|c| !matches!(c, ';' | '=' | '\n' | '\r'))
        .collect();
    // `=` is excluded from the domain check: it only has special meaning in the
    // name=value pair, not within an attribute like Domain.
    assert!(
        !settings.publisher.cookie_domain.contains([';', '\n', '\r']),
        "cookie_domain should not contain cookie metacharacters"
    );
    format!(
        "{}={}; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
        COOKIE_SYNTHETIC_ID, safe_id, settings.publisher.cookie_domain, COOKIE_MAX_AGE,
    )
}

/// Sets the synthetic ID cookie on the given response.
///
/// This helper abstracts the logic of creating the cookie string and appending
/// the Set-Cookie header to the response.
pub fn set_synthetic_cookie(
    settings: &Settings,
    response: &mut fastly::Response,
    synthetic_id: &str,
) {
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
    fn test_parse_cookies_to_jar_emtpy() {
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
    fn test_create_synthetic_cookie() {
        let settings = create_test_settings();
        let result = create_synthetic_cookie(&settings, "12345");
        assert_eq!(
            result,
            format!(
                "{}=12345; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
                COOKIE_SYNTHETIC_ID, settings.publisher.cookie_domain, COOKIE_MAX_AGE,
            )
        );
    }

    #[test]
    fn test_create_synthetic_cookie_sanitizes_metacharacters_in_id() {
        let settings = create_test_settings();
        let result = create_synthetic_cookie(&settings, "evil;injected\r\nfoo=bar");
        // Extract the value portion anchored to the cookie name constant to
        // avoid false positives from metacharacters in cookie attributes.
        let value = result
            .strip_prefix(&format!("{}=", COOKIE_SYNTHETIC_ID))
            .and_then(|s| s.split_once(';').map(|(v, _)| v))
            .expect("should have cookie value portion");
        assert_eq!(
            value, "evilinjectedfoobar",
            "should strip metacharacters and preserve safe chars"
        );
    }

    #[test]
    fn test_set_synthetic_cookie() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_synthetic_cookie(&settings, &mut response, "test-id-123");

        let cookie_header = response
            .get_header(header::SET_COOKIE)
            .expect("Set-Cookie header should be present");
        let cookie_str = cookie_header
            .to_str()
            .expect("header should be valid UTF-8");

        let expected = create_synthetic_cookie(&settings, "test-id-123");
        assert_eq!(
            cookie_str, expected,
            "Set-Cookie header should match create_synthetic_cookie output"
        );
    }
}
