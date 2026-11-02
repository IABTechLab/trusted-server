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

use crate::constants::{
    COOKIE_EUCONSENT_V2, COOKIE_GPP, COOKIE_GPP_SID, COOKIE_TS_EC, COOKIE_TS_EIDS,
    COOKIE_TS_EIDS_S, COOKIE_US_PRIVACY,
};
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

/// Trusted Server identity cookies that never reach a third party.
///
/// Stripped from Prebid Server requests and never added to proxy or provider
/// requests.
pub const TS_IDENTITY_COOKIE_NAMES: &[&str] = &[COOKIE_TS_EC, COOKIE_TS_EIDS, COOKIE_TS_EIDS_S];

/// Identity cookies removed from publisher-origin requests by default.
///
/// `ts-ec` and `ts-eids` still reach the origin, as they did before the
/// `ts-eids-s` mirror existed, so the origin's `Cookie` header stays at its
/// earlier size. See [`TS_ORIGIN_STRIPPED_COOKIE_NAMES_WITH_EIDS`].
pub const TS_ORIGIN_STRIPPED_COOKIE_NAMES: &[&str] = &[COOKIE_TS_EIDS_S];

/// Identity cookies removed from publisher-origin requests when the operator
/// sets `ec.strip_eids_cookie_from_origin`: `ts-eids` as well as the mirror.
pub const TS_ORIGIN_STRIPPED_COOKIE_NAMES_WITH_EIDS: &[&str] = &[COOKIE_TS_EIDS, COOKIE_TS_EIDS_S];

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

/// Strips cookies whose name is exactly one of `names` from a `Cookie` header
/// value.
///
/// Returns an empty string when nothing is left.
#[must_use]
pub fn strip_cookie_pairs(cookie_header: &str, names: &[&str]) -> String {
    cookie_header
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or_default().trim();
            !names.contains(&name)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Strips matching cookies from every `Cookie` field in `headers`.
///
/// All `Cookie` fields are read, matching pairs are removed, and the rest is
/// written back as one field. A field that is not valid UTF-8 is dropped
/// rather than forwarded, because it cannot be checked. The header is removed
/// when nothing is left.
pub fn strip_cookie_headers(headers: &mut http::HeaderMap, names: &[&str]) {
    let retained = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| strip_cookie_pairs(value, names))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("; ");

    headers.remove(header::COOKIE);
    if retained.is_empty() {
        return;
    }
    match http::HeaderValue::from_str(&retained) {
        Ok(value) => {
            headers.insert(header::COOKIE, value);
        }
        Err(_) => log::debug!("Dropping Cookie header that is invalid after stripping"),
    }
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
    #[test]
    fn strip_cookie_pairs_removes_exact_names_only() {
        let header =
            "ts-ec=abc; ts-eids=W10; __Host-ts-eids-s=eyJ2IjoxfQ; other=1; ts-eids-extra=keep";

        let stripped = strip_cookie_pairs(header, TS_ORIGIN_STRIPPED_COOKIE_NAMES_WITH_EIDS);

        assert_eq!(
            stripped, "ts-ec=abc; other=1; ts-eids-extra=keep",
            "should drop the ID cookies and keep ts-ec and a cookie that only shares a prefix"
        );

        assert_eq!(
            strip_cookie_pairs(header, TS_ORIGIN_STRIPPED_COOKIE_NAMES),
            "ts-ec=abc; ts-eids=W10; other=1; ts-eids-extra=keep",
            "should drop only the mirror by default"
        );
    }

    #[test]
    fn strip_cookie_headers_joins_every_cookie_field_and_drops_non_utf8() {
        let mut headers = http::HeaderMap::new();
        headers.append(
            header::COOKIE,
            http::HeaderValue::from_static("ts-ec=abc; ts-eids=W10"),
        );
        headers.append(
            header::COOKIE,
            http::HeaderValue::from_bytes(b"bad=\xff").expect("should build an opaque header"),
        );
        headers.append(header::COOKIE, http::HeaderValue::from_static("other=1"));

        strip_cookie_headers(&mut headers, TS_IDENTITY_COOKIE_NAMES);

        let values: Vec<_> = headers.get_all(header::COOKIE).iter().collect();
        assert_eq!(values.len(), 1, "should collapse into one Cookie header");
        assert_eq!(
            values[0], "other=1",
            "should keep only the unrelated cookie"
        );
    }

    #[test]
    fn strip_cookie_headers_removes_header_when_nothing_is_left() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            header::COOKIE,
            http::HeaderValue::from_static("ts-ec=abc; __Host-ts-eids-s=eyJ2IjoxfQ"),
        );
        headers.insert(
            header::USER_AGENT,
            http::HeaderValue::from_static("ExampleBrowser/1.0"),
        );

        strip_cookie_headers(&mut headers, TS_IDENTITY_COOKIE_NAMES);

        assert!(
            headers.get(header::COOKIE).is_none(),
            "should remove the empty Cookie header"
        );
        assert!(
            headers.get(header::USER_AGENT).is_some(),
            "should leave other headers alone"
        );
    }
}
