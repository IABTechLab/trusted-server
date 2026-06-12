//! Cookie handling utilities.
//!
//! This module provides functionality for parsing, stripping, and forwarding cookies used in the
//! trusted server system.

use cookie::{Cookie, CookieJar};
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header;
use http::Request;

#[cfg(test)]
use crate::constants::COOKIE_TS_EIDS;
use crate::constants::{COOKIE_EUCONSENT_V2, COOKIE_GPP, COOKIE_GPP_SID, COOKIE_US_PRIVACY};
use crate::error::TrustedServerError;
#[cfg(test)]
use base64::{engine::general_purpose::STANDARD, Engine as _};

/// Cookie names carrying privacy consent signals.
///
/// Used by [`strip_cookies`] to remove consent signals from a `Cookie` header
/// before forwarding requests to partners that receive consent through the
/// `OpenRTB` body instead.
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
    match req.headers().get(header::COOKIE) {
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

/// Parse Extended User IDs from the [`COOKIE_TS_EIDS`] cookie.
///
/// The cookie value is a standard-base64-encoded JSON array of
/// [`crate::openrtb::Eid`] objects written by the Trusted Server JS SDK via
/// `btoa(JSON.stringify(eids))`.
///
/// Returns `None` if the cookie is absent, base64-malformed, JSON-malformed,
/// or the decoded array is empty. Parse failures are logged at `debug` level
/// so operators can diagnose JS SDK / server mismatches.
#[cfg(test)]
#[must_use]
pub(crate) fn parse_ts_eids_cookie(jar: Option<&CookieJar>) -> Option<Vec<crate::openrtb::Eid>> {
    let value = jar?.get(COOKIE_TS_EIDS)?.value().to_owned();
    let decoded = match STANDARD.decode(&value) {
        Ok(b) => b,
        Err(e) => {
            log::debug!("ts-eids cookie: base64 decode failed: {e}");
            return None;
        }
    };
    match serde_json::from_slice::<Vec<crate::openrtb::Eid>>(&decoded) {
        Ok(eids) if !eids.is_empty() => {
            if eids.len() > 32 || eids.iter().any(|e| e.uids.len() > 32) {
                log::debug!("ts-eids cookie: too many eids or uids, rejecting");
                return None;
            }
            Some(eids)
        }
        Ok(_) => None,
        Err(e) => {
            log::debug!("ts-eids cookie: JSON parse failed: {e}");
            None
        }
    }
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

/// Copies the `Cookie` header from one request to another, optionally
/// stripping consent cookies.
///
/// When `strip_consent` is `true`, cookies listed in [`CONSENT_COOKIE_NAMES`]
/// are removed before forwarding. If stripping leaves no cookies or yields an
/// invalid header value, the stripped header is omitted. Non-UTF-8 cookie
/// headers are forwarded unchanged. Existing `Cookie` headers on `to` are
/// preserved; source values are appended after them.
pub fn forward_cookie_header(
    from: &Request<EdgeBody>,
    to: &mut Request<EdgeBody>,
    strip_consent: bool,
) {
    for cookie_value in from.headers().get_all(header::COOKIE) {
        if !strip_consent {
            to.headers_mut()
                .append(header::COOKIE, cookie_value.clone());
            continue;
        }

        match cookie_value.to_str() {
            Ok(s) => {
                let stripped = strip_cookies(s, CONSENT_COOKIE_NAMES);
                if !stripped.is_empty() {
                    if let Ok(value) = http::HeaderValue::from_str(&stripped) {
                        to.headers_mut().append(header::COOKIE, value);
                    }
                }
            }
            Err(_) => {
                // Non-UTF-8 Cookie header — forward as-is.
                to.headers_mut()
                    .append(header::COOKIE, cookie_value.clone());
            }
        }
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
    fn test_handle_request_cookies_malformed_cookie_string() {
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

    // ---------------------------------------------------------------
    // forward_cookie_header tests
    // ---------------------------------------------------------------

    #[test]
    fn test_forward_cookie_header_strips_consent() {
        let from = build_request(Some("euconsent-v2=BOE; session=abc123; us_privacy=1YNN"));
        let mut to = build_request(None);

        forward_cookie_header(&from, &mut to, true);

        let forwarded = to
            .headers()
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            !forwarded.contains("euconsent-v2"),
            "should strip consent cookie"
        );
        assert!(
            forwarded.contains("session=abc123"),
            "should keep non-consent cookie"
        );
    }

    #[test]
    fn test_forward_cookie_header_strip_all_leaves_header_absent() {
        let from = build_request(Some("euconsent-v2=BOE; __gpp=DBAC"));
        let mut to = build_request(None);

        forward_cookie_header(&from, &mut to, true);

        assert!(
            to.headers().get(header::COOKIE).is_none(),
            "should omit Cookie header when all cookies are stripped"
        );
    }

    #[test]
    fn test_forward_cookie_header_no_strip_passes_all() {
        let from = build_request(Some("euconsent-v2=BOE; session=abc123"));
        let mut to = build_request(None);

        forward_cookie_header(&from, &mut to, false);

        let forwarded = to
            .headers()
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            forwarded.contains("euconsent-v2"),
            "should forward consent cookie when not stripping"
        );
        assert!(
            forwarded.contains("session=abc123"),
            "should forward non-consent cookie"
        );
    }

    #[test]
    fn test_forward_cookie_header_non_utf8_forwarded_unchanged() {
        let non_utf8 = http::HeaderValue::from_bytes(b"\xff\xfe=value")
            .expect("should build non-UTF-8 header value");
        let mut from = build_request(None);
        from.headers_mut().append(header::COOKIE, non_utf8);
        let mut to = build_request(None);

        forward_cookie_header(&from, &mut to, true);

        let forwarded = to.headers().get(header::COOKIE);
        assert!(
            forwarded.is_some(),
            "should forward non-UTF-8 Cookie header unchanged"
        );
        assert_eq!(
            forwarded.expect("should have cookie header").as_bytes(),
            b"\xff\xfe=value",
            "should preserve raw bytes for non-UTF-8 cookie"
        );
    }

    #[test]
    fn test_forward_cookie_header_multiple_cookie_headers_appended() {
        let mut from = build_request(Some("session=abc123"));
        from.headers_mut().append(
            header::COOKIE,
            "theme=dark".parse().expect("should parse header value"),
        );
        let mut to = build_request(None);

        forward_cookie_header(&from, &mut to, false);

        let all_cookies: Vec<_> = to
            .headers()
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(all_cookies.len(), 2, "should append all Cookie headers");
        assert!(all_cookies.iter().any(|v| v.contains("session=abc123")));
        assert!(all_cookies.iter().any(|v| v.contains("theme=dark")));
    }

    #[test]
    fn test_forward_cookie_header_appends_to_existing_target_cookies() {
        let from = build_request(Some("session=abc123"));
        let mut to = build_request(Some("template=existing"));

        forward_cookie_header(&from, &mut to, false);

        let all_cookies: Vec<_> = to
            .headers()
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert_eq!(
            all_cookies,
            vec!["template=existing", "session=abc123"],
            "should preserve existing target cookies and append source cookies after them"
        );
    }

    // ---------------------------------------------------------------
    // strip_cookies tests
    // ---------------------------------------------------------------

    #[test]
    fn test_strip_cookies_removes_consent() {
        let header = "euconsent-v2=BOE; __gpp=DBAC; session=abc123; us_privacy=1YNN";
        let stripped = strip_cookies(header, CONSENT_COOKIE_NAMES);
        assert_eq!(stripped, "session=abc123");
    }

    #[test]
    fn test_strip_cookies_preserves_non_consent() {
        let header = "session=abc123; theme=dark";
        let stripped = strip_cookies(header, CONSENT_COOKIE_NAMES);
        assert_eq!(stripped, "session=abc123; theme=dark");
    }

    #[test]
    fn test_strip_cookies_empty_input() {
        let stripped = strip_cookies("", CONSENT_COOKIE_NAMES);
        assert_eq!(stripped, "");
    }

    #[test]
    fn test_strip_cookies_all_stripped() {
        let header = "euconsent-v2=BOE; __gpp=DBAC; __gpp_sid=2,6; us_privacy=1YNN";
        let stripped = strip_cookies(header, CONSENT_COOKIE_NAMES);
        assert_eq!(stripped, "");
    }

    #[test]
    fn test_strip_cookies_with_complex_values() {
        // Cookie values can contain '=' characters.
        let header = "euconsent-v2=BOE=xyz; session=abc=123=def";
        let stripped = strip_cookies(header, CONSENT_COOKIE_NAMES);
        assert_eq!(stripped, "session=abc=123=def");
    }

    fn make_jar_with(name: &str, value: &str) -> CookieJar {
        parse_cookies_to_jar(&format!("{name}={value}"))
    }

    fn encode_eids(eids: &[serde_json::Value]) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        STANDARD.encode(serde_json::to_string(eids).expect("should serialize eids"))
    }

    #[test]
    fn parse_ts_eids_cookie_returns_eids_for_valid_input() {
        let encoded = encode_eids(&[serde_json::json!({
            "source": "id5-sync.com",
            "uids": [{"id": "abc123", "atype": 1}]
        })]);
        let jar = make_jar_with(COOKIE_TS_EIDS, &encoded);
        let eids = parse_ts_eids_cookie(Some(&jar)).expect("should parse valid ts-eids cookie");
        assert_eq!(eids.len(), 1, "should return one EID");
        assert_eq!(eids[0].source, "id5-sync.com", "should preserve source");
        assert_eq!(eids[0].uids[0].id, "abc123", "should preserve uid");
    }

    #[test]
    fn parse_ts_eids_cookie_returns_none_when_cookie_absent() {
        let jar = CookieJar::new();
        assert!(
            parse_ts_eids_cookie(Some(&jar)).is_none(),
            "should return None when cookie absent"
        );
    }

    #[test]
    fn parse_ts_eids_cookie_returns_none_for_empty_array() {
        let encoded = encode_eids(&[]);
        let jar = make_jar_with(COOKIE_TS_EIDS, &encoded);
        assert!(
            parse_ts_eids_cookie(Some(&jar)).is_none(),
            "should return None for empty EID array"
        );
    }

    #[test]
    fn parse_ts_eids_cookie_returns_none_for_corrupt_base64() {
        let jar = make_jar_with(COOKIE_TS_EIDS, "not!!valid!!base64");
        assert!(
            parse_ts_eids_cookie(Some(&jar)).is_none(),
            "should return None for corrupt base64"
        );
    }

    #[test]
    fn parse_ts_eids_cookie_returns_none_for_invalid_json() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let encoded = STANDARD.encode(b"this is not json");
        let jar = make_jar_with(COOKIE_TS_EIDS, &encoded);
        assert!(
            parse_ts_eids_cookie(Some(&jar)).is_none(),
            "should return None for invalid JSON"
        );
    }

    #[test]
    fn parse_ts_eids_cookie_returns_none_for_none_jar() {
        assert!(
            parse_ts_eids_cookie(None).is_none(),
            "should return None when jar is None"
        );
    }
}
