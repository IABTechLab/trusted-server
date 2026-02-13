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
/// Generates a properly formatted cookie with security attributes
/// for storing the synthetic ID.
#[must_use]
pub fn create_synthetic_cookie(settings: &Settings, synthetic_id: &str) -> String {
    format!(
        "{}={}; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={}",
        COOKIE_SYNTHETIC_ID, synthetic_id, settings.publisher.cookie_domain, COOKIE_MAX_AGE,
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
        assert_eq!(jar.get("c1").unwrap().value(), "v1");
        assert_eq!(jar.get("c2").unwrap().value(), "v2");
    }

    #[test]
    fn test_parse_cookies_to_jar_not_unique() {
        let cookie_str = "c1=v1;c1=v2";
        let jar = parse_cookies_to_jar(cookie_str);

        assert!(jar.iter().count() == 1);
        assert_eq!(jar.get("c1").unwrap().value(), "v2");
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
        assert_eq!(jar.get("c1").unwrap().value(), "v1");
        assert_eq!(jar.get("c2").unwrap().value(), "v2");
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
                "{}=12345; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={}",
                COOKIE_SYNTHETIC_ID, settings.publisher.cookie_domain, COOKIE_MAX_AGE,
            )
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
