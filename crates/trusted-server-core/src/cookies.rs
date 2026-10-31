//! Cookie handling utilities.
//!
//! This module provides functionality for parsing and extracting cookies used in the
//! trusted server system.

pub(crate) mod template_cache_policy;

use cookie::{Cookie, CookieJar};
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
use http::Request;
use http::header;

use crate::constants::{COOKIE_EUCONSENT_V2, COOKIE_GPP, COOKIE_GPP_SID, COOKIE_US_PRIVACY};
use crate::error::TrustedServerError;

/// Cookie names carrying privacy consent signals.
///
/// Used as the PBS consent-cookie allowlist. PBS omits the entire `Cookie`
/// header when consent is sent exclusively through the `OpenRTB` body.
pub const CONSENT_COOKIE_NAMES: &[&str] = &[
    COOKIE_EUCONSENT_V2,
    COOKIE_GPP,
    COOKIE_GPP_SID,
    COOKIE_US_PRIVACY,
];

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
    req: &Request<EdgeBody>,
) -> Result<Option<CookieJar>, Report<TrustedServerError>> {
    if let Some(header_value) = req.headers().get(header::COOKIE) {
        let header_value_str =
            header_value
                .to_str()
                .change_context(TrustedServerError::InvalidHeaderValue {
                    message: "Cookie header contains invalid UTF-8".to_owned(),
                })?;
        let jar = parse_cookies_to_jar(header_value_str);
        Ok(Some(jar))
    } else {
        log::debug!("No cookie header found in request");
        Ok(None)
    }
}

/// Returns the named value from the request's selected `Cookie` header.
///
/// Values are trimmed and may contain additional `=` characters. A missing or
/// non-UTF-8 selected header returns `None`.
#[must_use]
pub fn extract_cookie_value<B>(req: &Request<B>, name: &str) -> Option<String> {
    let cookie_header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    cookie_header.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_owned())
    })
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use crate::error::TrustedServerError;

    use super::*;

    fn build_request(cookie_header: Option<&str>) -> Request<EdgeBody> {
        let mut builder = Request::builder().method("GET").uri("http://example.com");
        if let Some(cookie_header) = cookie_header {
            builder = builder.header(header::COOKIE, cookie_header);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

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
        let req = build_request(Some("c1=v1;c2=v2"));
        let jar = handle_request_cookies(&req)
            .expect("should parse cookies")
            .expect("should have cookie jar");

        assert!(jar.iter().count() == 2);
        assert_eq!(jar.get("c1").expect("should have cookie c1").value(), "v1");
        assert_eq!(jar.get("c2").expect("should have cookie c2").value(), "v2");
    }

    #[test]
    fn test_handle_request_cookies_with_empty_cookie() {
        let req = build_request(Some(""));
        let jar = handle_request_cookies(&req)
            .expect("should parse cookies")
            .expect("should have cookie jar");

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_handle_request_cookies_no_cookie_header() {
        let req = build_request(None);
        let jar = handle_request_cookies(&req).expect("should handle missing cookie header");

        assert!(jar.is_none());
    }

    #[test]
    fn test_handle_request_cookies_invalid_cookie_header() {
        let req = build_request(Some("invalid"));
        let jar = handle_request_cookies(&req)
            .expect("should parse cookies")
            .expect("should have cookie jar");

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_handle_request_cookies_invalid_utf8_cookie_header() {
        // Truncated 4-byte UTF-8 sequence: `\xF0` starts a 4-byte code point but
        // only two continuation bytes follow, so `to_str()` rejects it.
        let invalid_cookie_value =
            HeaderValue::from_bytes(b"\xF0\x90\x80").expect("should build header value");
        let mut req = build_request(None);
        req.headers_mut()
            .insert(header::COOKIE, invalid_cookie_value);

        let err =
            handle_request_cookies(&req).expect_err("should reject invalid UTF-8 cookie header");

        assert!(
            matches!(
                err.current_context(),
                TrustedServerError::InvalidHeaderValue { .. }
            ),
            "should return InvalidHeaderValue for non-UTF-8 cookie header"
        );
    }

    #[test]
    fn extract_cookie_value_returns_none_without_cookie_header() {
        let req = build_request(None);

        assert_eq!(extract_cookie_value(&req, "session"), None);
    }

    #[test]
    fn extract_cookie_value_trims_pairs_and_preserves_embedded_equals() {
        let req = build_request(Some("first=one; token = abc== ; last=three"));

        assert_eq!(
            extract_cookie_value(&req, "token").as_deref(),
            Some("abc==")
        );
        assert_eq!(extract_cookie_value(&req, "last").as_deref(), Some("three"));
    }

    #[test]
    fn extract_cookie_value_returns_none_for_selected_non_utf8_header() {
        let invalid = HeaderValue::from_bytes(b"\xff=value").expect("should build header value");
        let mut req = build_request(None);
        req.headers_mut().append(header::COOKIE, invalid);
        req.headers_mut().append(
            header::COOKIE,
            HeaderValue::from_static("session=from-later-header"),
        );

        assert_eq!(extract_cookie_value(&req, "session"), None);
    }
}
