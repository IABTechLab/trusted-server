//! EC cookie creation and expiration helpers.
//!
//! These functions handle the `Set-Cookie` header for the `ts-ec` cookie.
//! Cookie attributes follow current best practices:
//!
//! - `Domain` is computed as `.{publisher.domain}` for subdomain coverage
//! - `Path=/` makes the cookie available on all paths
//! - `Secure` restricts to HTTPS
//! - `SameSite=Lax` provides CSRF protection while allowing top-level navigations
//! - `Max-Age` of 1 year (or 0 to expire)
//! - `HttpOnly` prevents client-side JS from reading the cookie via
//!   `document.cookie`, providing XSS defense-in-depth. The identify
//!   endpoint (`/_ts/api/v1/identify`) exposes the EC ID in its response
//!   body for legitimate JS use cases.

use edgezero_core::body::Body as EdgeBody;
use http::{HeaderValue, Response, header};

use crate::constants::{COOKIE_TS_EC, COOKIE_TS_EC_RESOLVED};
use crate::settings::Settings;

/// Maximum age for the EC cookie (1 year in seconds).
const COOKIE_MAX_AGE: i32 = 365 * 24 * 60 * 60;

/// Maximum length in bytes of an Edge Cookie identifier.
///
/// A global bound enforced wherever an identifier enters the system (creation,
/// cookie read-back, cookie write), so no provider can emit a value the cookie
/// layer, logs, or the KV key space cannot carry.
pub(crate) const MAX_EC_ID_LEN: usize = 256;

fn is_allowed_ec_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '~')
}

// Identifier allowlist: [A-Za-z0-9._~-], the cookie-safe alphabet every
// Edge Cookie identifier must fit regardless of which provider created it.
// This is intentionally broader than the built-in format validator
// (`generation::is_valid_ec_id`), which enforces the HMAC provider's
// `<64-hex>.<6-alphanumeric>` structure, either bare or under the `hmac~`
// envelope; an opaque vendor identifier only has to fit the alphabet and the
// length bound.
#[must_use]
pub(crate) fn ec_id_has_only_allowed_chars(ec_id: &str) -> bool {
    !ec_id.is_empty() && ec_id.len() <= MAX_EC_ID_LEN && ec_id.chars().all(is_allowed_ec_id_char)
}

/// Formats a `Set-Cookie` header value for the EC cookie.
///
/// Centralises the cookie attribute string so that changes to security
/// attributes (e.g. adding `Partitioned`) only need updating in one place.
fn format_set_cookie(domain: &str, value: &str, max_age: i32) -> String {
    format!(
        "{COOKIE_TS_EC}={value}; Domain={domain}; Path=/; Secure; SameSite=Lax; Max-Age={max_age}; HttpOnly",
    )
}

/// Creates an EC cookie `Set-Cookie` header value.
///
/// Per spec §5.2, the EC cookie domain is computed from
/// `settings.publisher.domain` (not `cookie_domain`) to ensure the EC
/// cookie is always scoped to the publisher's apex domain. Callers validate
/// the identifier with [`ec_id_has_only_allowed_chars`] before this point;
/// an identifier is rejected outright rather than rewritten, so the cookie
/// value and the identity-graph key can never silently diverge.
#[must_use]
pub(crate) fn create_ec_cookie(settings: &Settings, ec_id: &str) -> String {
    format_set_cookie(
        &settings.publisher.ec_cookie_domain(),
        ec_id,
        COOKIE_MAX_AGE,
    )
}

/// Sets the EC ID cookie on the given response.
///
/// Validates `ec_id` against the identifier alphabet and length bound before
/// interpolation. An identifier that fails validation is rejected and the
/// cookie is not set, with an error logged; the value is never rewritten, so
/// a provider identifier survives byte for byte or not at all. This also
/// prevents an attacker from injecting spurious cookie attributes via a
/// controlled ID value.
///
/// `cookie_domain` comes from operator configuration and is considered trusted.
pub fn set_ec_cookie(settings: &Settings, response: &mut Response<EdgeBody>, ec_id: &str) {
    if !ec_id_has_only_allowed_chars(ec_id) {
        log::error!(
            "Rejecting EC ID for Set-Cookie: value of {} bytes is empty, over {} bytes, or \
             contains characters outside the identifier alphabet",
            ec_id.len(),
            MAX_EC_ID_LEN,
        );
        return;
    }

    match HeaderValue::from_str(&create_ec_cookie(settings, ec_id)) {
        Ok(val) => {
            response.headers_mut().append(header::SET_COOKIE, val);
        }
        Err(e) => {
            // Unreachable in practice: the identifier allowlist above gates
            // the value, and format_set_cookie emits only controlled bytes.
            // Logged for defense-in-depth symmetry with the rejection above.
            log::warn!("Skipping EC Set-Cookie: invalid header value: {e}");
        }
    }
}

/// Sets an Edge Cookie created by a client-cycle provider on the response.
///
/// Unlike [`set_ec_cookie`], the value is treated as opaque: it need not match
/// the canonical HMAC id shape, because a client-cycle provider's identifier
/// (for example a signed envelope, or the client-random demo's value) is not in
/// that format. The value is still validated against the same identifier
/// alphabet and length bound as every other identifier
/// ([`ec_id_has_only_allowed_chars`]); a value outside the bounds is rejected
/// (no cookie set) and logged, never rewritten, which prevents header
/// injection. The same `Secure`, `HttpOnly`, `SameSite=Lax`, `Path=/`, and
/// `Domain` attributes as [`set_ec_cookie`] apply.
pub(crate) fn set_provider_ec_cookie(
    settings: &Settings,
    response: &mut Response<EdgeBody>,
    value: &str,
) {
    if !ec_id_has_only_allowed_chars(value) {
        log::error!(
            "Rejecting provider Edge Cookie value of {} bytes: empty, over {} bytes, or outside the identifier alphabet",
            value.len(),
            MAX_EC_ID_LEN,
        );
        return;
    }

    let cookie = format_set_cookie(
        &settings.publisher.ec_cookie_domain(),
        value,
        COOKIE_MAX_AGE,
    );
    match HeaderValue::from_str(&cookie) {
        Ok(val) => {
            response.headers_mut().append(header::SET_COOKIE, val);
        }
        Err(e) => {
            // Unreachable in practice: the identifier allowlist gates the value
            // and format_set_cookie emits only controlled bytes.
            log::warn!("Skipping provider EC Set-Cookie: invalid header value: {e}");
        }
    }
}

/// Expires the EC cookie by setting `Max-Age=0`.
///
/// Used when a user revokes consent — the browser will delete the cookie
/// on receipt of this header.
pub fn expire_ec_cookie(settings: &Settings, response: &mut Response<EdgeBody>) {
    match HeaderValue::from_str(&format_set_cookie(
        &settings.publisher.ec_cookie_domain(),
        "",
        0,
    )) {
        Ok(val) => {
            response.headers_mut().append(header::SET_COOKIE, val);
        }
        Err(e) => {
            // Unreachable in practice: format_set_cookie emits only
            // controlled bytes from operator-trusted configuration.
            log::warn!("Skipping EC cookie expiry Set-Cookie: invalid header value: {e}");
        }
    }
    // Expire the resolved marker together with the Edge Cookie, so a page
    // whose visitor later re-establishes the permission can resolve again.
    let marker = format!(
        "{COOKIE_TS_EC_RESOLVED}=; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age=0",
        settings.publisher.ec_cookie_domain(),
    );
    match HeaderValue::from_str(&marker) {
        Ok(val) => {
            response.headers_mut().append(header::SET_COOKIE, val);
        }
        Err(e) => {
            log::warn!("Skipping resolved-marker expiry Set-Cookie: invalid header value: {e}");
        }
    }
}

/// Sets the `non-HttpOnly` resolved-marker cookie alongside a client-cycle Edge
/// Cookie.
///
/// The Edge Cookie itself is `HttpOnly`, so the page script cannot see it and
/// would otherwise post to the resolve endpoint on every page view. The marker
/// carries no identity (its value is `1`); it only signals that a resolve
/// succeeded. It shares the Edge Cookie's `Domain`, `Path`, `Secure`,
/// `SameSite`, and lifetime, and is expired together with it by
/// [`expire_ec_cookie`].
pub(crate) fn set_resolved_marker_cookie(settings: &Settings, response: &mut Response<EdgeBody>) {
    let cookie = format!(
        "{COOKIE_TS_EC_RESOLVED}=1; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={COOKIE_MAX_AGE}",
        settings.publisher.ec_cookie_domain(),
    );
    match HeaderValue::from_str(&cookie) {
        Ok(val) => {
            response.headers_mut().append(header::SET_COOKIE, val);
        }
        Err(e) => {
            // Unreachable in practice: the value is a constant and the domain
            // comes from operator-trusted configuration.
            log::warn!("Skipping resolved-marker Set-Cookie: invalid header value: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ec_cookie_lifetime_is_one_year() {
        // The legacy bare-identifier reader's retirement condition (see
        // `provider_owns_id`) is written in terms of this lifetime and the
        // identity-graph `ENTRY_TTL`, which `kv::tests::constants_have_expected_values`
        // pins to the same figure. Changing either moves the earliest safe
        // retirement, so neither may drift unnoticed.
        assert_eq!(
            COOKIE_MAX_AGE, 31_536_000,
            "the EC cookie should live one year"
        );
    }

    #[test]
    fn identifier_bounds_reject_oversize_and_accept_tilde() {
        assert!(
            ec_id_has_only_allowed_chars("a.~-_Z9"),
            "the cookie-safe alphabet includes the tilde"
        );
        assert!(
            !ec_id_has_only_allowed_chars(""),
            "an empty identifier is rejected"
        );
        let oversize = "a".repeat(MAX_EC_ID_LEN + 1);
        assert!(
            !ec_id_has_only_allowed_chars(&oversize),
            "an identifier over the length cap is rejected"
        );
        let at_cap = "a".repeat(MAX_EC_ID_LEN);
        assert!(
            ec_id_has_only_allowed_chars(&at_cap),
            "an identifier at the length cap is accepted"
        );
    }
    use crate::test_support::tests::create_test_settings;
    use http::header;

    fn empty_response() -> Response<EdgeBody> {
        Response::builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build test response")
    }

    /// A valid EC ID for use in cookie tests.
    const TEST_EC_ID: &str =
        "aaaaaaaabbbbbbbbccccccccddddddddeeeeeeeeffffffff0000000011111111.abcXYZ";

    #[test]
    fn create_ec_cookie_uses_computed_domain() {
        let settings = create_test_settings();
        let result = create_ec_cookie(&settings, TEST_EC_ID);

        assert_eq!(
            result,
            format!(
                "{}={}; Domain=.{}; Path=/; Secure; SameSite=Lax; Max-Age={}; HttpOnly",
                COOKIE_TS_EC, TEST_EC_ID, settings.publisher.domain, COOKIE_MAX_AGE,
            ),
            "should use computed cookie domain (.{{domain}})"
        );
    }

    #[test]
    fn set_ec_cookie_appends_header() {
        let settings = create_test_settings();
        let mut response = empty_response();
        set_ec_cookie(&settings, &mut response, TEST_EC_ID);

        let cookie_header = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should have Set-Cookie header");
        let cookie_str = cookie_header.to_str().expect("should be valid UTF-8");

        assert_eq!(
            cookie_str,
            create_ec_cookie(&settings, TEST_EC_ID),
            "should match create_ec_cookie output"
        );
    }

    #[test]
    fn set_ec_cookie_rejects_disallowed_chars_outright() {
        // Rejection, never rewriting: an identifier outside the alphabet must
        // not produce a cookie at all, so the cookie value and the identity
        // graph key can never silently diverge.
        let settings = create_test_settings();
        let mut response = Response::new(EdgeBody::empty());
        set_ec_cookie(
            &settings,
            &mut response,
            "evil;injected
foo=bar",
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "an identifier outside the alphabet should set no cookie"
        );
    }

    #[test]
    fn create_ec_cookie_preserves_well_formed_id() {
        let settings = create_test_settings();
        let id = "abc123def0123456789abcdef0123456789abcdef0123456789abcdef01234567.xk92ab";
        let result = create_ec_cookie(&settings, id);
        let value = result
            .strip_prefix(&format!("{COOKIE_TS_EC}="))
            .and_then(|s| s.split_once(';').map(|(v, _)| v))
            .expect("should have cookie value portion");

        assert_eq!(value, id, "should not modify a well-formed EC ID");
    }

    #[test]
    fn set_ec_cookie_rejects_semicolon() {
        let settings = create_test_settings();
        let mut response = empty_response();
        set_ec_cookie(&settings, &mut response, "evil; Domain=.attacker.com");

        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "should not set Set-Cookie when value contains a semicolon"
        );
    }

    #[test]
    fn set_ec_cookie_rejects_crlf() {
        let settings = create_test_settings();
        let mut response = empty_response();
        set_ec_cookie(&settings, &mut response, "evil\r\nX-Injected: header");

        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "should not set Set-Cookie when value contains CRLF"
        );
    }

    #[test]
    fn set_ec_cookie_rejects_space() {
        let settings = create_test_settings();
        let mut response = empty_response();
        set_ec_cookie(&settings, &mut response, "bad value");

        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "should not set Set-Cookie when value contains whitespace"
        );
    }

    #[test]
    fn expire_ec_cookie_sets_max_age_zero() {
        let settings = create_test_settings();
        let mut response = empty_response();
        expire_ec_cookie(&settings, &mut response);

        let cookie_header = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should have Set-Cookie header");
        let cookie_str = cookie_header.to_str().expect("should be valid UTF-8");

        assert!(
            cookie_str.contains("Max-Age=0"),
            "should set Max-Age=0 to expire cookie"
        );
        assert!(
            cookie_str.starts_with(&format!("{COOKIE_TS_EC}=;")),
            "should clear cookie value"
        );
        assert!(
            cookie_str.contains(&format!("Domain=.{}", settings.publisher.domain)),
            "should use computed cookie domain"
        );
    }

    #[test]
    fn expire_ec_cookie_matches_security_attributes() {
        let settings = create_test_settings();
        let mut response = empty_response();
        expire_ec_cookie(&settings, &mut response);

        let cookie_header = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should have Set-Cookie header");
        let cookie_str = cookie_header.to_str().expect("should be valid UTF-8");

        assert_eq!(
            cookie_str,
            format!(
                "{}=; Domain=.{}; Path=/; Secure; SameSite=Lax; Max-Age=0; HttpOnly",
                COOKIE_TS_EC, settings.publisher.domain,
            ),
            "expiry cookie should retain the same security attributes as the live cookie"
        );
    }

    #[test]
    fn set_provider_ec_cookie_sets_opaque_value_with_security_attributes() {
        let settings = create_test_settings();
        let mut response = empty_response();
        // A client-cycle value that is not the canonical HMAC id shape.
        set_provider_ec_cookie(&settings, &mut response, "8473625190");

        let cookie_str = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should set the EC cookie")
            .to_str()
            .expect("should be valid UTF-8");

        assert_eq!(
            cookie_str,
            format!(
                "{}=8473625190; Domain=.{}; Path=/; Secure; SameSite=Lax; Max-Age={}; HttpOnly",
                COOKIE_TS_EC, settings.publisher.domain, COOKIE_MAX_AGE,
            ),
            "an opaque provider value should be set verbatim with the standard security attributes"
        );
    }

    #[test]
    fn set_provider_ec_cookie_preserves_base64url_value() {
        let settings = create_test_settings();
        let mut response = empty_response();
        // A base64url value (RFC 4648 section 5, unpadded) fits the identifier
        // alphabet exactly and must be preserved verbatim. Standard base64
        // (`+`, `/`, `=`) is outside the alphabet, so a provider that carries
        // binary data re-encodes it as base64url before creating the cookie value.
        let value = "abcDEF123-_x";
        set_provider_ec_cookie(&settings, &mut response, value);

        let cookie_str = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should set the EC cookie")
            .to_str()
            .expect("should be valid UTF-8");

        assert!(
            cookie_str.starts_with(&format!("{COOKIE_TS_EC}={value};")),
            "a base64url provider value should be preserved verbatim, got {cookie_str}"
        );
    }

    #[test]
    fn set_provider_ec_cookie_rejects_unsafe_value() {
        let settings = create_test_settings();
        let mut response = empty_response();
        set_provider_ec_cookie(&settings, &mut response, "evil; Domain=.attacker.com");

        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "an unsafe value must not set a cookie, preventing header injection"
        );
    }
}
