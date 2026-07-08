//! Tester-cookie endpoint helpers.
//!
//! The tester routes are intentionally disabled unless configured. When enabled,
//! they set or clear a first-party `ts-tester` cookie scoped to the configured
//! publisher cookie domain. They are self-service routes rather than admin
//! routes; the cookie only affects tester routing and must not gate sensitive
//! behavior.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{header, HeaderValue, Response, StatusCode};

use crate::constants::COOKIE_TS_TESTER;
use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Formats the tester cookie `Set-Cookie` header value.
fn format_tester_cookie(domain: &str) -> String {
    format!(
        "{}=true; Domain={}; Path=/; Secure; SameSite=Lax",
        COOKIE_TS_TESTER, domain,
    )
}

/// Formats the tester cookie clearing `Set-Cookie` header value.
fn format_clear_tester_cookie(domain: &str) -> String {
    format!(
        "{}=; Domain={}; Path=/; Secure; SameSite=Lax; Max-Age=0",
        COOKIE_TS_TESTER, domain,
    )
}

/// Handles `GET /_ts/set-tester`.
///
/// Returns `404 Not Found` while `[tester_cookie].enabled` is false. When the
/// feature is enabled, returns `204 No Content` with `Set-Cookie: ts-tester=true`
/// scoped to `publisher.cookie_domain`.
///
/// # Errors
///
/// Returns [`TrustedServerError::InvalidHeaderValue`] if the configured cookie
/// domain cannot be rendered as an HTTP header value.
pub fn handle_set_tester(
    settings: &Settings,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    if !settings.tester_cookie.enabled {
        let mut response = Response::new(EdgeBody::empty());
        *response.status_mut() = StatusCode::NOT_FOUND;
        return Ok(response);
    }

    let set_cookie =
        HeaderValue::from_str(&format_tester_cookie(&settings.publisher.cookie_domain))
            .change_context(TrustedServerError::InvalidHeaderValue {
                message: "tester cookie contains invalid header value".to_string(),
            })?;

    let mut response = Response::new(EdgeBody::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );
    response
        .headers_mut()
        .insert(header::SET_COOKIE, set_cookie);
    Ok(response)
}

/// Handles `GET /_ts/clear-tester`.
///
/// Returns `404 Not Found` while `[tester_cookie].enabled` is false. When the
/// feature is enabled, returns `204 No Content` with `Set-Cookie` expiring the
/// `ts-tester` cookie scoped to `publisher.cookie_domain`.
///
/// # Errors
///
/// Returns [`TrustedServerError::InvalidHeaderValue`] if the configured cookie
/// domain cannot be rendered as an HTTP header value.
pub fn handle_clear_tester(
    settings: &Settings,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    if !settings.tester_cookie.enabled {
        let mut response = Response::new(EdgeBody::empty());
        *response.status_mut() = StatusCode::NOT_FOUND;
        return Ok(response);
    }

    let set_cookie = HeaderValue::from_str(&format_clear_tester_cookie(
        &settings.publisher.cookie_domain,
    ))
    .change_context(TrustedServerError::InvalidHeaderValue {
        message: "tester cookie clear contains invalid header value".to_string(),
    })?;

    let mut response = Response::new(EdgeBody::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );
    response
        .headers_mut()
        .insert(header::SET_COOKIE, set_cookie);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn tester_cookie_uses_configured_cookie_domain() {
        let mut settings = create_test_settings();
        settings.tester_cookie.enabled = true;
        settings.publisher.cookie_domain = ".tester.example".to_string();

        let response = handle_set_tester(&settings).expect("should build tester response");

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "enabled tester route should return no content"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private"),
            "tester route should not be cacheable"
        );
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should set tester cookie")
            .to_str()
            .expect("should render set-cookie as utf-8");
        assert_eq!(
            set_cookie, "ts-tester=true; Domain=.tester.example; Path=/; Secure; SameSite=Lax",
            "tester cookie should use publisher.cookie_domain"
        );
    }

    #[test]
    fn tester_cookie_route_is_disabled_by_default() {
        let settings = create_test_settings();

        let response = handle_set_tester(&settings).expect("should build disabled tester response");

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "disabled tester route should return not found"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "disabled tester route should not set a cookie"
        );
    }

    #[test]
    fn clear_tester_cookie_uses_configured_cookie_domain() {
        let mut settings = create_test_settings();
        settings.tester_cookie.enabled = true;
        settings.publisher.cookie_domain = ".tester.example".to_string();

        let response = handle_clear_tester(&settings).expect("should build clear tester response");

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "enabled clear tester route should return no content"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private"),
            "clear tester route should not be cacheable"
        );
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should clear tester cookie")
            .to_str()
            .expect("should render set-cookie as utf-8");
        assert_eq!(
            set_cookie,
            "ts-tester=; Domain=.tester.example; Path=/; Secure; SameSite=Lax; Max-Age=0",
            "tester cookie clear should use publisher.cookie_domain"
        );
    }

    #[test]
    fn clear_tester_cookie_route_is_disabled_by_default() {
        let settings = create_test_settings();

        let response =
            handle_clear_tester(&settings).expect("should build disabled clear tester response");

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "disabled clear tester route should return not found"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "disabled clear tester route should not set a cookie"
        );
    }
}
