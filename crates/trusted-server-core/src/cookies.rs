//! Cookie handling utilities.
//!
//! This module provides functionality for parsing and creating cookies
//! used in the trusted server system.

use std::borrow::Cow;

use cookie::{Cookie, CookieJar};
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header;
use http::Request;
use http::Response;

use crate::constants::{
    COOKIE_EUCONSENT_V2, COOKIE_GPP, COOKIE_GPP_SID, COOKIE_SYNTHETIC_ID, COOKIE_US_PRIVACY,
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

fn is_allowed_synthetic_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
}

// Outbound allowlist for cookie sanitization: permits [a-zA-Z0-9._-] as a
// defense-in-depth backstop when setting the Set-Cookie header. This is
// intentionally broader than the inbound format validator
// (`synthetic::is_valid_synthetic_id`), which enforces the exact
// `<64-hex>.<6-alphanumeric>` structure and is used to reject untrusted
// request values before they enter the system.
#[must_use]
pub(crate) fn synthetic_id_has_only_allowed_chars(synthetic_id: &str) -> bool {
    synthetic_id.chars().all(is_allowed_synthetic_id_char)
}

fn sanitize_synthetic_id_for_cookie(synthetic_id: &str) -> Cow<'_, str> {
    if synthetic_id_has_only_allowed_chars(synthetic_id) {
        return Cow::Borrowed(synthetic_id);
    }

    let safe_id = synthetic_id
        .chars()
        .filter(|c| is_allowed_synthetic_id_char(*c))
        .collect::<String>();

    log::warn!(
        "Stripped disallowed characters from synthetic_id before setting cookie (len {} -> {}); \
         callers should reject invalid request IDs before cookie creation",
        synthetic_id.len(),
        safe_id.len(),
    );

    Cow::Owned(safe_id)
}

fn synthetic_cookie_attributes(settings: &Settings, max_age: i32) -> String {
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
///
/// # Panics
///
/// Panics if the stripped cookie string cannot be converted into a valid HTTP
/// `Cookie` header value.
pub fn forward_cookie_header(
    from: &Request<EdgeBody>,
    to: &mut Request<EdgeBody>,
    strip_consent: bool,
) {
    let Some(cookie_value) = from.headers().get(header::COOKIE) else {
        return;
    };

    if !strip_consent {
        to.headers_mut()
            .insert(header::COOKIE, cookie_value.clone());
        return;
    }

    match cookie_value.to_str() {
        Ok(s) => {
            let stripped = strip_cookies(s, CONSENT_COOKIE_NAMES);
            if !stripped.is_empty() {
                to.headers_mut().insert(
                    header::COOKIE,
                    http::HeaderValue::from_str(&stripped)
                        .expect("should build stripped Cookie header value"),
                );
            }
        }
        Err(_) => {
            // Non-UTF-8 Cookie header — forward as-is
            to.headers_mut()
                .insert(header::COOKIE, cookie_value.clone());
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
pub(crate) fn synthetic_id_cookie_value_is_safe(value: &str) -> bool {
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
///   Safe to set because integrations receive the synthetic ID via the `x-synthetic-id`
///   response header instead of reading it from the cookie directly.
/// - `SameSite=Lax`: sent on same-site requests and top-level cross-site navigations.
///   `Strict` is intentionally avoided — it would suppress the cookie on the first
///   request when a user arrives from an external page, breaking first-visit attribution.
/// - `Max-Age`: 1 year retention.
///
/// The `synthetic_id` is sanitized via an allowlist before embedding in the cookie value.
/// Only ASCII alphanumeric characters and `.`, `-`, `_` are permitted — matching the
/// known synthetic ID format (`{64-char-hex}.{6-char-alphanumeric}`). Request-sourced IDs
/// with disallowed characters are rejected earlier in [`crate::synthetic::get_synthetic_id`];
/// this sanitization remains as a defense-in-depth backstop for unexpected callers.
///
/// The `cookie_domain` is validated at config load time via [`validator::Validate`] on
/// [`crate::settings::Publisher`]; bad config fails at startup, not per-request.
///
/// # Examples
///
/// ```no_run
/// # use trusted_server_core::cookies::create_synthetic_cookie;
/// # use trusted_server_core::settings::Settings;
/// // `settings` is loaded at startup via `Settings::from_toml_and_env`.
/// # fn example(settings: &Settings) {
/// let cookie = create_synthetic_cookie(settings, "abc123.xk92ab");
/// assert!(cookie.contains("HttpOnly"));
/// assert!(cookie.contains("Secure"));
/// # }
/// ```
#[must_use]
pub fn create_synthetic_cookie(settings: &Settings, synthetic_id: &str) -> String {
    let safe_id = sanitize_synthetic_id_for_cookie(synthetic_id);

    format!(
        "{}={}; {}",
        COOKIE_SYNTHETIC_ID,
        safe_id,
        synthetic_cookie_attributes(settings, COOKIE_MAX_AGE),
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
///
/// # Panics
///
/// Panics if the generated `Set-Cookie` header cannot be represented as a
/// valid HTTP header value.
pub fn set_synthetic_cookie(
    settings: &Settings,
    response: &mut Response<EdgeBody>,
    synthetic_id: &str,
) {
    if !synthetic_id_cookie_value_is_safe(synthetic_id) {
        log::warn!(
            "Rejecting synthetic_id for Set-Cookie: value of {} bytes contains characters illegal in a cookie value",
            synthetic_id.len()
        );
        return;
    }
    response.headers_mut().append(
        header::SET_COOKIE,
        http::HeaderValue::from_str(&create_synthetic_cookie(settings, synthetic_id))
            .expect("should build Set-Cookie header value"),
    );
}

/// Expires the synthetic ID cookie by setting `Max-Age=0`.
///
/// Used when a user revokes consent — the browser will delete the cookie
/// on receipt of this header.
pub(crate) fn create_synthetic_id_expiry_cookie(settings: &Settings) -> String {
    format!(
        "{}=; {}",
        COOKIE_SYNTHETIC_ID,
        synthetic_cookie_attributes(settings, 0),
    )
}

/// Appends an expiry `Set-Cookie` header that clears the synthetic ID cookie.
///
/// # Panics
///
/// Panics if the generated expiry `Set-Cookie` header cannot be represented as
/// a valid HTTP header value.
pub fn expire_synthetic_cookie(settings: &Settings, response: &mut Response<EdgeBody>) {
    response.headers_mut().append(
        header::SET_COOKIE,
        http::HeaderValue::from_str(&create_synthetic_id_expiry_cookie(settings))
            .expect("should build expiry Set-Cookie header value"),
    );
}

#[cfg(test)]
mod tests {
    use crate::test_support::tests::create_test_settings;

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

    fn build_response() -> Response<EdgeBody> {
        Response::builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build test response")
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
    fn test_set_synthetic_cookie() {
        let settings = create_test_settings();
        let mut response = build_response();
        set_synthetic_cookie(&settings, &mut response, "abc123.XyZ789");

        let cookie_str = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("Set-Cookie header should be present")
            .to_str()
            .expect("header should be valid UTF-8");

        assert_eq!(
            cookie_str,
            format!(
                "{}=abc123.XyZ789; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
                COOKIE_SYNTHETIC_ID, settings.publisher.cookie_domain, COOKIE_MAX_AGE,
            ),
            "Set-Cookie header should match expected format"
        );
    }

    #[test]
    fn test_create_synthetic_cookie_sanitizes_disallowed_chars_in_id() {
        let settings = create_test_settings();
        // Allowlist permits only ASCII alphanumeric, '.', '-', '_'.
        // ';', '=', '\r', '\n', spaces, NUL bytes, and other control chars are all stripped.
        let result = create_synthetic_cookie(&settings, "evil;injected\r\nfoo=bar\0baz");
        // Extract the value portion anchored to the cookie name constant to
        // avoid false positives from disallowed chars in cookie attributes.
        let value = result
            .strip_prefix(&format!("{}=", COOKIE_SYNTHETIC_ID))
            .and_then(|s| s.split_once(';').map(|(v, _)| v))
            .expect("should have cookie value portion");
        assert_eq!(
            value, "evilinjectedfoobarbaz",
            "should strip disallowed characters and preserve safe chars"
        );
    }

    #[test]
    fn test_create_synthetic_cookie_preserves_well_formed_id() {
        let settings = create_test_settings();
        // A well-formed ID should pass through the allowlist unmodified.
        let id = "abc123def0123456789abcdef0123456789abcdef0123456789abcdef01234567.xk92ab";
        let result = create_synthetic_cookie(&settings, id);
        let value = result
            .strip_prefix(&format!("{}=", COOKIE_SYNTHETIC_ID))
            .and_then(|s| s.split_once(';').map(|(v, _)| v))
            .expect("should have cookie value portion");
        assert_eq!(value, id, "should not modify a well-formed synthetic ID");
    }

    #[test]
    fn test_set_synthetic_cookie_rejects_semicolon() {
        let settings = create_test_settings();
        let mut response = build_response();
        set_synthetic_cookie(&settings, &mut response, "evil; Domain=.attacker.com");

        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "Set-Cookie should not be set when value contains a semicolon"
        );
    }

    #[test]
    fn test_set_synthetic_cookie_rejects_crlf() {
        let settings = create_test_settings();
        let mut response = build_response();
        set_synthetic_cookie(&settings, &mut response, "evil\r\nX-Injected: header");

        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "Set-Cookie should not be set when value contains CRLF"
        );
    }

    #[test]
    fn test_set_synthetic_cookie_rejects_space() {
        let settings = create_test_settings();
        let mut response = build_response();
        set_synthetic_cookie(&settings, &mut response, "bad value");

        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "Set-Cookie should not be set when value contains whitespace"
        );
    }

    #[test]
    fn test_is_safe_cookie_value_rejects_empty_string() {
        assert!(
            !synthetic_id_cookie_value_is_safe(""),
            "should reject empty string"
        );
    }

    #[test]
    fn test_is_safe_cookie_value_accepts_valid_synthetic_id_characters() {
        // Hex digits, dot separator, alphanumeric suffix — the full synthetic ID character set
        assert!(
            synthetic_id_cookie_value_is_safe("abcdef0123456789.ABCDEFabcdef"),
            "should accept hex digits, dots, and alphanumeric characters"
        );
    }

    #[test]
    fn test_is_safe_cookie_value_rejects_non_ascii() {
        assert!(
            !synthetic_id_cookie_value_is_safe("valüe"),
            "should reject non-ASCII UTF-8 characters"
        );
    }

    #[test]
    fn test_is_safe_cookie_value_rejects_illegal_characters() {
        assert!(
            !synthetic_id_cookie_value_is_safe("val;ue"),
            "should reject semicolon"
        );
        assert!(
            !synthetic_id_cookie_value_is_safe("val,ue"),
            "should reject comma"
        );
        assert!(
            !synthetic_id_cookie_value_is_safe("val\"ue"),
            "should reject double-quote"
        );
        assert!(
            !synthetic_id_cookie_value_is_safe("val\\ue"),
            "should reject backslash"
        );
        assert!(
            !synthetic_id_cookie_value_is_safe("val ue"),
            "should reject space"
        );
        assert!(
            !synthetic_id_cookie_value_is_safe("val\x00ue"),
            "should reject null byte"
        );
        assert!(
            !synthetic_id_cookie_value_is_safe("val\x7fue"),
            "should reject DEL character"
        );
    }

    #[test]
    fn test_expire_synthetic_cookie_matches_security_attributes() {
        let settings = create_test_settings();
        let mut response = build_response();

        expire_synthetic_cookie(&settings, &mut response);

        let cookie_header = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("Set-Cookie header should be present");
        let cookie_str = cookie_header
            .to_str()
            .expect("header should be valid UTF-8");

        assert_eq!(
            cookie_str,
            format!(
                "{}=; Domain={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0",
                COOKIE_SYNTHETIC_ID, settings.publisher.cookie_domain,
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
