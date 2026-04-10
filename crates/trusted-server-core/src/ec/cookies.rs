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
//!   body and `x-ts-ec` header for legitimate JS use cases.

use fastly::http::header;

use crate::constants::COOKIE_TS_EC;
use crate::settings::Settings;

/// Maximum age for the EC cookie (1 year in seconds).
const COOKIE_MAX_AGE: i32 = 365 * 24 * 60 * 60;

/// Formats a `Set-Cookie` header value for the EC cookie.
///
/// Centralises the cookie attribute string so that changes to security
/// attributes (e.g. adding `Partitioned`) only need updating in one place.
fn format_set_cookie(domain: &str, value: &str, max_age: i32) -> String {
    format!(
        "{}={}; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age={}; HttpOnly",
        COOKIE_TS_EC, value, domain, max_age,
    )
}

/// Creates an EC cookie `Set-Cookie` header value.
///
/// Per spec §5.2, the EC cookie domain is computed from
/// `settings.publisher.domain` (not `cookie_domain`) to ensure the EC
/// cookie is always scoped to the publisher's apex domain.
#[must_use]
pub fn create_ec_cookie(settings: &Settings, ec_id: &str) -> String {
    format_set_cookie(
        &settings.publisher.ec_cookie_domain(),
        ec_id,
        COOKIE_MAX_AGE,
    )
}

/// Sets the EC ID cookie on the given response.
///
/// # Panics (debug only)
///
/// Debug-asserts that `ec_id` passes [`super::generation::is_valid_ec_id`]
/// as a defense-in-depth check against cookie injection.
pub fn set_ec_cookie(settings: &Settings, response: &mut fastly::Response, ec_id: &str) {
    debug_assert!(
        super::generation::is_valid_ec_id(ec_id),
        "EC ID must be validated before cookie creation: got '{ec_id}'"
    );
    response.append_header(header::SET_COOKIE, create_ec_cookie(settings, ec_id));
}

/// Expires the EC cookie by setting `Max-Age=0`.
///
/// Used when a user revokes consent — the browser will delete the cookie
/// on receipt of this header.
pub fn expire_ec_cookie(settings: &Settings, response: &mut fastly::Response) {
    response.append_header(
        header::SET_COOKIE,
        format_set_cookie(&settings.publisher.ec_cookie_domain(), "", 0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::header;

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
        let mut response = fastly::Response::new();
        set_ec_cookie(&settings, &mut response, TEST_EC_ID);

        let cookie_header = response
            .get_header(header::SET_COOKIE)
            .expect("should have Set-Cookie header");
        let cookie_str = cookie_header.to_str().expect("should be valid UTF-8");

        assert_eq!(
            cookie_str,
            create_ec_cookie(&settings, TEST_EC_ID),
            "should match create_ec_cookie output"
        );
    }

    #[test]
    fn expire_ec_cookie_sets_max_age_zero() {
        let settings = create_test_settings();
        let mut response = fastly::Response::new();
        expire_ec_cookie(&settings, &mut response);

        let cookie_header = response
            .get_header(header::SET_COOKIE)
            .expect("should have Set-Cookie header");
        let cookie_str = cookie_header.to_str().expect("should be valid UTF-8");

        assert!(
            cookie_str.contains("Max-Age=0"),
            "should set Max-Age=0 to expire cookie"
        );
        assert!(
            cookie_str.starts_with(&format!("{}=;", COOKIE_TS_EC)),
            "should clear cookie value"
        );
        assert!(
            cookie_str.contains(&format!("Domain=.{}", settings.publisher.domain)),
            "should use computed cookie domain"
        );
    }
}
