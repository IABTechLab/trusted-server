//! Cookie handling utilities.
//!
//! This module provides functionality for parsing, stripping, and forwarding cookies used in the
//! trusted server system.

pub(crate) mod template_cache_policy;

use cookie::{Cookie, CookieJar};
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
use http::Request;
use http::header;

use crate::error::TrustedServerError;

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

/// Parses a cookie string into a [`CookieJar`] that keeps the first cookie
/// of each name.
///
/// RFC 6265 section 5.4 says a browser SHOULD list the cookie with the
/// longer `Path` first. In a browser that does, on a request under a
/// publisher's path this keeps the publisher's cookie over one of the same
/// name written under `/`. A browser that does not may list the root cookie
/// first, and the root cookie is then the one kept.
fn parse_cookies_to_jar_keeping_first(s: &str) -> CookieJar {
    let mut jar = CookieJar::new();
    for cookie in Cookie::split_parse(s.trim().to_owned()).filter_map(Result::ok) {
        if jar.get(cookie.name()).is_none() {
            jar.add_original(cookie);
        }
    }
    jar
}

/// Extracts and parses cookies from an HTTP request.
///
/// Attempts to parse the Cookie header into a [`CookieJar`] for easy access
/// to individual cookies. Where core writes its cookies under the prefix
/// request addressing removed, the first cookie of each name is kept, which
/// is the one under the publisher's path in a browser that lists longer
/// paths first. Otherwise the last is kept, as [`parse_cookies_to_jar`]
/// keeps it, which covers a request with no removed prefix and one whose
/// removed prefix is empty, `/` or not a cookie path.
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
        let under_removed_prefix = req
            .extensions()
            .get::<crate::addressing::RemovedPathPrefix>()
            .and_then(|prefix| crate::addressing::cookie_path_for(prefix.as_str()))
            .is_some();
        let jar = if under_removed_prefix {
            parse_cookies_to_jar_keeping_first(header_value_str)
        } else {
            parse_cookies_to_jar(header_value_str)
        };
        Ok(Some(jar))
    } else {
        log::debug!("No cookie header found in request");
        Ok(None)
    }
}

/// Every value of the cookie `name` in the request's `Cookie` header, in the
/// order the browser sent them, parsed as [`handle_request_cookies`] parses
/// them.
///
/// A browser sends one pair for each cookie of a name it holds, such as one
/// under `/` and one under a publisher's path, where a [`CookieJar`] keeps
/// one. A missing or non-UTF-8 header gives none.
#[must_use]
pub(crate) fn cookie_values<B>(req: &Request<B>, name: &str) -> Vec<String> {
    let Some(header_value) = req
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    else {
        return Vec::new();
    };
    Cookie::split_parse(header_value.trim())
        .filter_map(Result::ok)
        .filter(|cookie| cookie.name() == name)
        .map(|cookie| cookie.value().to_owned())
        .collect()
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

/// Strips named cookies from a `Cookie` header value string.
///
/// Parses the semicolon-separated cookie pairs, filters out any whose name
/// matches one of `cookie_names`, and reconstructs the header string.
///
/// Returns an empty string if all cookies were stripped or the input was empty.
#[must_use]
pub fn strip_cookies(cookie_header: &str, cookie_names: &[&str]) -> String {
    cookie_header
        .split(';')
        .map(str::trim)
        .filter(|pair| {
            if let Some(name) = pair.split('=').next() {
                !cookie_names.contains(&name.trim())
            } else {
                true
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
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

    /// A browser that lists the cookie with the longer path first sends the
    /// publisher's own cookie first, so on an addressed request that cookie
    /// is the one read.
    #[test]
    fn an_addressed_request_reads_the_first_cookie_of_a_name() {
        let mut req = build_request(Some("ts-ec=publisher; ts-ec=root; other=1"));
        req.extensions_mut()
            .insert(crate::addressing::RemovedPathPrefix::new(
                "/publisher-a.example".to_owned(),
            ));
        let jar = handle_request_cookies(&req)
            .expect("should parse cookies")
            .expect("should have cookie jar");

        assert_eq!(jar.get("ts-ec").map(Cookie::value), Some("publisher"));
        assert_eq!(jar.get("other").map(Cookie::value), Some("1"));
    }

    #[test]
    fn a_request_with_no_removed_prefix_reads_the_last_cookie_of_a_name() {
        let req = build_request(Some("ts-ec=publisher; ts-ec=root"));
        let jar = handle_request_cookies(&req)
            .expect("should parse cookies")
            .expect("should have cookie jar");

        assert_eq!(jar.get("ts-ec").map(Cookie::value), Some("root"));
    }

    /// An empty removed prefix, `/`, or a prefix that cannot be a cookie's
    /// `Path` puts core's cookies under `/`, so the request reads them as a
    /// request with no removed prefix does.
    #[test]
    fn a_removed_prefix_whose_cookies_go_under_the_root_reads_the_last_cookie_of_a_name() {
        for prefix in ["", "/", "/a;b"] {
            let mut req = build_request(Some("ts-ec=publisher; ts-ec=root"));
            req.extensions_mut()
                .insert(crate::addressing::RemovedPathPrefix::new(prefix.to_owned()));
            let jar = handle_request_cookies(&req)
                .expect("should parse cookies")
                .expect("should have cookie jar");

            assert_eq!(
                jar.get("ts-ec").map(Cookie::value),
                Some("root"),
                "removed prefix `{prefix}`"
            );
        }
    }

    #[test]
    fn cookie_values_gives_every_value_of_a_name_in_the_order_sent() {
        let req = build_request(Some("ts-ec=publisher; other=1; ts-ec=root"));
        assert_eq!(cookie_values(&req, "ts-ec"), ["publisher", "root"]);
        assert!(cookie_values(&build_request(None), "ts-ec").is_empty());
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

    // ---------------------------------------------------------------
    // strip_cookies tests
    // ---------------------------------------------------------------

    /// The names a forwarder strips, standing in for the cookies a
    /// signal scheme travels in.
    const STRIPPED: &[&str] = &["pref-a", "pref-b", "pref-b-sid", "pref-c"];

    #[test]
    fn test_strip_cookies_removes_consent() {
        let header = "pref-a=BOE; pref-b=DBAC; session=abc123; pref-c=1YNN";
        let stripped = strip_cookies(header, STRIPPED);
        assert_eq!(stripped, "session=abc123");
    }

    #[test]
    fn test_strip_cookies_preserves_non_consent() {
        let header = "session=abc123; theme=dark";
        let stripped = strip_cookies(header, STRIPPED);
        assert_eq!(stripped, "session=abc123; theme=dark");
    }

    #[test]
    fn test_strip_cookies_empty_input() {
        let stripped = strip_cookies("", STRIPPED);
        assert_eq!(stripped, "");
    }

    #[test]
    fn test_strip_cookies_all_stripped() {
        let header = "pref-a=BOE; pref-b=DBAC; pref-b-sid=2,6; pref-c=1YNN";
        let stripped = strip_cookies(header, STRIPPED);
        assert_eq!(stripped, "");
    }

    #[test]
    fn test_strip_cookies_with_complex_values() {
        // Cookie values can contain '=' characters.
        let header = "pref-a=BOE=xyz; session=abc=123=def";
        let stripped = strip_cookies(header, STRIPPED);
        assert_eq!(stripped, "session=abc=123=def");
    }
}
