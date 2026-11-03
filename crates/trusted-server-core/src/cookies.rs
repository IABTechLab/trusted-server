//! Cookie handling utilities.
//!
//! This module provides functionality for parsing and extracting cookies used in the
//! trusted server system.

pub(crate) mod template_cache_policy;

use cookie::{Cookie, CookieJar};
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::header;
use http::{HeaderMap, Request};

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

/// Iterates raw semicolon-delimited pairs across every Cookie field in order.
///
/// Reserved-input owners may inspect malformed pairs before sanitation removes
/// them, so duplicate directives cannot become valid through their removal.
pub(crate) fn cookie_header_pairs(headers: &HeaderMap) -> impl Iterator<Item = &[u8]> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .flat_map(|field| field.as_bytes().split(|byte| *byte == b';'))
        .map(<[u8]>::trim_ascii)
}

/// Returns valid ASCII Cookie pairs without dropping their neighboring pairs.
///
/// Field order, duplicate pairs, empty values, and embedded equals are retained.
/// Dropped pairs produce only aggregate reason counts at debug level.
pub(crate) fn request_cookie_pairs(headers: &HeaderMap) -> Vec<&str> {
    parse_cookie_pairs(cookie_header_pairs(headers)).0
}

/// Value-free counts of Cookie pairs discarded during request preparation.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CookieSanitation {
    non_ascii: usize,
    non_visible: usize,
    malformed: usize,
}

impl CookieSanitation {
    /// Whether sanitation removed nonempty textual input rather than an opaque pair.
    pub(crate) fn discarded_textual_pairs(&self) -> bool {
        self.non_visible > 0 || self.malformed > 0
    }
}

/// Validates Cookie pairs and returns value-free sanitation provenance.
pub(crate) fn parse_cookie_pairs<'a>(
    pairs: impl Iterator<Item = &'a [u8]>,
) -> (Vec<&'a str>, CookieSanitation) {
    let mut retained = Vec::new();
    let mut sanitation = CookieSanitation::default();
    for pair in pairs {
        let pair = pair.trim_ascii();
        if pair.is_empty() {
            continue;
        }
        if !pair.is_ascii() {
            sanitation.non_ascii += 1;
            continue;
        }
        if pair.iter().any(|byte| !(b' '..=b'~').contains(byte)) {
            sanitation.non_visible += 1;
            continue;
        }
        let pair = std::str::from_utf8(pair).expect("should decode validated ASCII Cookie pair");
        let Some((name, _)) = pair.split_once('=') else {
            sanitation.malformed += 1;
            continue;
        };
        if !is_cookie_name(name.trim_ascii().as_bytes()) {
            sanitation.malformed += 1;
            continue;
        }
        retained.push(pair);
    }
    if sanitation.non_ascii + sanitation.non_visible + sanitation.malformed > 0 {
        log::debug!("Dropped Cookie pairs: {sanitation:?}");
    }
    (retained, sanitation)
}

fn is_cookie_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(byte))
}

fn cookie_jar(pairs: Vec<&str>) -> CookieJar {
    let mut jar = CookieJar::new();
    for pair in pairs {
        if let Ok(cookie) = Cookie::parse(pair.to_owned()) {
            jar.add_original(cookie);
        }
    }
    jar
}

/// Parses a cookie string into a [`CookieJar`].
///
/// Malformed and non-ASCII pairs are skipped without losing valid neighbors.
/// The jar retains the last value of a duplicate name.
#[must_use]
pub fn parse_cookies_to_jar(s: &str) -> CookieJar {
    cookie_jar(parse_cookie_pairs(s.as_bytes().split(|byte| *byte == b';')).0)
}

/// Extracts and parses every Cookie field from an HTTP request.
///
/// Valid pairs are read in field order. Malformed or non-ASCII pairs are skipped
/// without losing their neighbors. The jar retains the last duplicate value.
/// A present field with no valid pairs produces an empty jar. An absent field
/// produces `None`.
///
/// # Errors
///
/// Malformed Cookie input does not produce an error.
pub fn handle_request_cookies(
    req: &Request<EdgeBody>,
) -> Result<Option<CookieJar>, Report<TrustedServerError>> {
    Ok(req
        .headers()
        .contains_key(header::COOKIE)
        .then(|| cookie_jar(request_cookie_pairs(req.headers()))))
}

/// Returns the first matching valid Cookie value across all request fields.
///
/// Values are trimmed and may contain additional `=` characters. Malformed and
/// non-ASCII pairs are skipped without losing valid pairs in the same field.
#[must_use]
pub fn extract_cookie_value<B>(req: &Request<B>, name: &str) -> Option<String> {
    request_cookie_pairs(req.headers())
        .into_iter()
        .find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.trim_ascii() == name).then(|| value.trim_ascii().to_owned())
        })
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

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
    fn lenient_cookie_parsing_keeps_valid_pairs_in_every_field() {
        let mut req = build_request(None);
        req.headers_mut().append(
            header::COOKIE,
            HeaderValue::from_bytes(b"first=one; bad=\xff; token=abc==")
                .expect("should build mixed cookie field"),
        );
        req.headers_mut().append(
            header::COOKIE,
            HeaderValue::from_static("missing-equals; first=last; empty=; other=two"),
        );

        let jar = handle_request_cookies(&req)
            .expect("should skip malformed cookie pairs")
            .expect("should retain a cookie jar");

        assert_eq!(
            jar.get("first").expect("should retain first").value(),
            "last"
        );
        assert_eq!(
            jar.get("token").expect("should retain token").value(),
            "abc=="
        );
        assert_eq!(jar.get("empty").expect("should retain empty").value(), "");
        assert_eq!(
            jar.get("other").expect("should retain other").value(),
            "two"
        );
        assert!(
            jar.get("bad").is_none(),
            "should drop only the non-ASCII pair"
        );
        assert_eq!(jar.iter().count(), 4, "should omit malformed pairs");
    }

    #[test]
    fn lenient_cookie_sanitation_reports_only_reason_counts() {
        let (pairs, sanitation) = parse_cookie_pairs(
            [
                b"=SENTINEL_MALFORMED".as_slice(),
                b"secret=\xffSENTINEL_NON_ASCII".as_slice(),
                b"bad\tname=SENTINEL_NON_VISIBLE".as_slice(),
                b"session=SENTINEL_VALUE".as_slice(),
            ]
            .into_iter(),
        );

        assert_eq!(
            pairs,
            ["session=SENTINEL_VALUE"],
            "should retain valid values internally"
        );
        assert_eq!(
            (
                sanitation.non_ascii,
                sanitation.non_visible,
                sanitation.malformed
            ),
            (1, 1, 1)
        );
        let diagnostic = format!("{sanitation:?}");
        assert_eq!(
            diagnostic,
            "CookieSanitation { non_ascii: 1, non_visible: 1, malformed: 1 }"
        );
        assert!(
            !diagnostic.contains("SENTINEL"),
            "should never log Cookie names or values"
        );
    }

    #[test]
    fn lenient_cookie_lookup_skips_bad_pairs_without_changing_first_match() {
        let mut req = build_request(None);
        req.headers_mut().append(
            header::COOKIE,
            HeaderValue::from_bytes(b"bad=\xff; session=first")
                .expect("should build mixed cookie field"),
        );
        req.headers_mut().append(
            header::COOKIE,
            HeaderValue::from_static("session=second; token=abc=="),
        );

        assert_eq!(
            extract_cookie_value(&req, "session").as_deref(),
            Some("first")
        );
        assert_eq!(
            extract_cookie_value(&req, "token").as_deref(),
            Some("abc==")
        );
    }

    #[test]
    fn test_handle_request_cookies_skips_invalid_utf8_cookie_header() {
        // A malformed field without any valid pair leaves an empty jar.
        let invalid_cookie_value =
            HeaderValue::from_bytes(b"\xF0\x90\x80").expect("should build header value");
        let mut req = build_request(None);
        req.headers_mut()
            .insert(header::COOKIE, invalid_cookie_value);

        let jar = handle_request_cookies(&req)
            .expect("should skip malformed Cookie input")
            .expect("should retain a present Cookie field");

        assert_eq!(
            jar.iter().count(),
            0,
            "should skip malformed pairs without an error"
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
    fn extract_cookie_value_checks_later_fields_after_non_utf8_pair() {
        let invalid = HeaderValue::from_bytes(b"\xff=value").expect("should build header value");
        let mut req = build_request(None);
        req.headers_mut().append(header::COOKIE, invalid);
        req.headers_mut().append(
            header::COOKIE,
            HeaderValue::from_static("session=from-later-header"),
        );

        assert_eq!(
            extract_cookie_value(&req, "session").as_deref(),
            Some("from-later-header"),
            "should inspect valid pairs in every field"
        );
    }
}
