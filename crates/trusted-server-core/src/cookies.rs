//! Cookie handling utilities.
//!
//! This module provides functionality for parsing and creating cookies
//! used in the trusted server system.

use std::borrow::Cow;

use cookie::{Cookie, CookieJar};
use error_stack::{Report, ResultExt};
use fastly::http::header;
use fastly::Request;

use crate::constants::{
    COOKIE_EUCONSENT_V2, COOKIE_GPP, COOKIE_GPP_SID, COOKIE_TS_EC, COOKIE_US_PRIVACY,
};
use crate::error::TrustedServerError;
use crate::settings::Settings;

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

const COOKIE_MAX_AGE: i32 = 365 * 24 * 60 * 60; // 1 year

fn is_allowed_ec_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
}

// Outbound allowlist for cookie sanitization: permits [a-zA-Z0-9._-] as a
// defense-in-depth backstop when setting the Set-Cookie header. This is
// intentionally broader than the inbound format validator
// (`synthetic::is_valid_synthetic_id`), which enforces the exact
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

fn ec_cookie_attributes(settings: &Settings, max_age: i32) -> String {
    format!(
        "Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={max_age}",
        settings.publisher.cookie_domain,
    )
}

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
/// are removed before forwarding. If stripping leaves no cookies, the header
/// is omitted entirely. Non-UTF-8 cookie headers are forwarded unchanged.
pub fn forward_cookie_header(from: &Request, to: &mut Request, strip_consent: bool) {
    let Some(cookie_value) = from.get_header(header::COOKIE) else {
        return;
    };

    if !strip_consent {
        to.set_header(header::COOKIE, cookie_value);
        return;
    }

    match cookie_value.to_str() {
        Ok(s) => {
            let stripped = strip_cookies(s, CONSENT_COOKIE_NAMES);
            if !stripped.is_empty() {
                to.set_header(header::COOKIE, &stripped);
            }
        }
        Err(_) => {
            // Non-UTF-8 Cookie header — forward as-is
            to.set_header(header::COOKIE, cookie_value);
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

/// Generates a `Set-Cookie` header value with the following security attributes:
/// - `Secure`: transmitted over HTTPS only.
/// - `HttpOnly`: inaccessible to JavaScript (`document.cookie`), blocking XSS exfiltration.
///   Safe to set because integrations receive the EC ID via the `x-ts-ec`
///   response header instead of reading it from the cookie directly.
/// - `SameSite=Lax`: sent on same-site requests and top-level cross-site navigations.
///   `Strict` is intentionally avoided — it would suppress the cookie on the first
///   request when a user arrives from an external page, breaking first-visit attribution.
/// - `Max-Age`: 1 year retention.
///
/// The `ec_id` is sanitized via an allowlist before embedding in the cookie value.
/// Only ASCII alphanumeric characters and `.`, `-`, `_` are permitted — matching the
/// known EC ID format (`{64-char-hex}.{6-char-alphanumeric}`). Request-sourced IDs
/// with disallowed characters are rejected earlier in [`crate::edge_cookie::get_ec_id`];
/// this sanitization remains as a defense-in-depth backstop for unexpected callers.
///
/// The `cookie_domain` is validated at config load time via [`validator::Validate`] on
/// [`crate::settings::Publisher`]; bad config fails at startup, not per-request.
///
/// # Examples
///
/// ```no_run
/// # use trusted_server_core::cookies::create_ec_cookie;
/// # use trusted_server_core::settings::Settings;
/// // `settings` is loaded at startup via `Settings::from_toml_and_env`.
/// # fn example(settings: &Settings) {
/// let cookie = create_ec_cookie(settings, "abc123.xk92ab");
/// assert!(cookie.contains("HttpOnly"));
/// assert!(cookie.contains("Secure"));
/// # }
/// ```
#[must_use]
pub fn create_ec_cookie(settings: &Settings, ec_id: &str) -> String {
    let safe_id = sanitize_ec_id_for_cookie(ec_id);

    format!(
        "{}={}; {}",
        COOKIE_TS_EC,
        safe_id,
        ec_cookie_attributes(settings, COOKIE_MAX_AGE),
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
pub fn set_ec_cookie(settings: &Settings, response: &mut fastly::Response, ec_id: &str) {
    if !is_safe_cookie_value(ec_id) {
        log::warn!(
            "Rejecting EC ID for Set-Cookie: value of {} bytes contains characters illegal in a cookie value",
            ec_id.len()
        );
        return;
    }
    response.append_header(header::SET_COOKIE, create_ec_cookie(settings, ec_id));
}

/// Expires the EC cookie by setting `Max-Age=0`.
///
/// Used when a user revokes consent — the browser will delete the cookie
/// on receipt of this header.
pub fn expire_ec_cookie(settings: &Settings, response: &mut fastly::Response) {
    let cookie = format!("{}=; {}", COOKIE_TS_EC, ec_cookie_attributes(settings, 0),);
    response.append_header(header::SET_COOKIE, cookie);
}

#[cfg(test)]
mod tests {
    use fastly::http::HeaderValue;

    use crate::error::TrustedServerError;
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
    fn test_handle_request_cookies_malformed_cookie_string() {
        let req = Request::get("http://example.com").with_header(header::COOKIE, "invalid");
        let jar = handle_request_cookies(&req)
            .expect("should parse cookies")
            .expect("should have cookie jar");

        assert!(jar.iter().count() == 0);
    }

    #[test]
    fn test_handle_request_cookies_invalid_utf8_cookie_header() {
        let invalid_cookie_value = HeaderValue::from_bytes(b"ts-ec=valid-prefix\xF0\x90\x80")
            .expect("should build header value");
        let req =
            Request::get("http://example.com").with_header(header::COOKIE, invalid_cookie_value);

        let err =
            handle_request_cookies(&req).expect_err("should reject invalid UTF-8 cookie header");

        assert!(
            matches!(
                err.current_context(),
                TrustedServerError::InvalidHeaderValue { message }
                    if message.contains("invalid UTF-8")
            ),
            "should return invalid header value error for non-UTF-8 cookie header"
        );
    }

    #[test]
    fn test_set_ec_cookie() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_ec_cookie(&settings, &mut response, "abc123.XyZ789");

        let cookie_str = response
            .get_header(header::SET_COOKIE)
            .expect("Set-Cookie header should be present")
            .to_str()
            .expect("header should be valid UTF-8");

        assert_eq!(
            cookie_str,
            format!(
                "{}=abc123.XyZ789; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
                COOKIE_TS_EC, settings.publisher.cookie_domain, COOKIE_MAX_AGE,
            ),
            "Set-Cookie header should match expected format"
        );
    }

    #[test]
    fn test_create_ec_cookie_sanitizes_disallowed_chars_in_id() {
        let settings = create_test_settings();
        // Allowlist permits only ASCII alphanumeric, '.', '-', '_'.
        // ';', '=', '\r', '\n', spaces, NUL bytes, and other control chars are all stripped.
        let result = create_ec_cookie(&settings, "evil;injected\r\nfoo=bar\0baz");
        // Extract the value portion anchored to the cookie name constant to
        // avoid false positives from disallowed chars in cookie attributes.
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
    fn test_create_ec_cookie_preserves_well_formed_id() {
        let settings = create_test_settings();
        // A well-formed ID should pass through the allowlist unmodified.
        let id = "abc123def0123456789abcdef0123456789abcdef0123456789abcdef01234567.xk92ab";
        let result = create_ec_cookie(&settings, id);
        let value = result
            .strip_prefix(&format!("{}=", COOKIE_TS_EC))
            .and_then(|s| s.split_once(';').map(|(v, _)| v))
            .expect("should have cookie value portion");
        assert_eq!(value, id, "should not modify a well-formed EC ID");
    }

    #[test]
    fn test_set_ec_cookie_rejects_semicolon() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_ec_cookie(&settings, &mut response, "evil; Domain=.attacker.com");

        assert!(
            response.get_header(header::SET_COOKIE).is_none(),
            "Set-Cookie should not be set when value contains a semicolon"
        );
    }

    #[test]
    fn test_set_ec_cookie_rejects_crlf() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_ec_cookie(&settings, &mut response, "evil\r\nX-Injected: header");

        assert!(
            response.get_header(header::SET_COOKIE).is_none(),
            "Set-Cookie should not be set when value contains CRLF"
        );
    }

    #[test]
    fn test_set_ec_cookie_rejects_space() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        set_ec_cookie(&settings, &mut response, "bad value");

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
    fn test_is_safe_cookie_value_accepts_valid_ec_id_characters() {
        // Hex digits, dot separator, alphanumeric suffix — the full EC ID character set
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

    #[test]
    fn test_expire_ec_cookie_matches_security_attributes() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();

        expire_ec_cookie(&settings, &mut response);

        let cookie_header = response
            .get_header(header::SET_COOKIE)
            .expect("Set-Cookie header should be present");
        let cookie_str = cookie_header
            .to_str()
            .expect("header should be valid UTF-8");

        assert_eq!(
            cookie_str,
            format!(
                "{}=; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0",
                COOKIE_TS_EC, settings.publisher.cookie_domain,
            ),
            "expiry cookie should retain the same security attributes as the live cookie"
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
        // Cookie values can contain '=' characters
        let header = "euconsent-v2=BOE=xyz; session=abc=123=def";
        let stripped = strip_cookies(header, CONSENT_COOKIE_NAMES);
        assert_eq!(stripped, "session=abc=123=def");
    }
}
