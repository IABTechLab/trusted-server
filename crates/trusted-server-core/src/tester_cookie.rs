//! Tester-cookie endpoint helpers.
//!
//! The tester routes are intentionally disabled unless configured. When enabled,
//! they set or clear a first-party `<public prefix>-tester` cookie, such as
//! `ts-tester`, scoped to the configured
//! publisher cookie domain. They are self-service routes rather than admin
//! routes; the cookie only affects tester routing and must not gate sensitive
//! behavior.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{HeaderValue, Response, StatusCode, header};

use crate::constants::COOKIE_TESTER;
use crate::error::TrustedServerError;
use crate::prefix::PublicPrefix;
use crate::settings::Settings;

/// Formats the tester cookie `Set-Cookie` header value.
fn format_tester_cookie(public: &PublicPrefix, domain: &str, max_age_seconds: u32) -> String {
    format!(
        "{}=true; Domain={}; Path={}; Secure; SameSite=Lax; Max-Age={}",
        public.cookie_name(COOKIE_TESTER),
        domain,
        public.cookie_path(),
        max_age_seconds,
    )
}

/// Formats the tester cookie clearing `Set-Cookie` header value.
fn format_clear_tester_cookie(public: &PublicPrefix, domain: &str) -> String {
    format!(
        "{}=; Domain={}; Path={}; Secure; SameSite=Lax; Max-Age=0",
        public.cookie_name(COOKIE_TESTER),
        domain,
        public.cookie_path(),
    )
}

/// Handles `GET /_ts/set-tester`.
///
/// Returns `404 Not Found` while `[tester_cookie].enabled` is false. When the
/// feature is enabled, returns `204 No Content` with `Set-Cookie` writing
/// `<public prefix>-tester=true`, such as `ts-tester=true`, scoped to
/// `publisher.cookie_domain` and persisted for
/// `[tester_cookie].max_age_seconds` (30 days by default).
///
/// # Errors
///
/// Returns [`TrustedServerError::InvalidHeaderValue`] if the configured cookie
/// domain cannot be rendered as an HTTP header value.
pub fn handle_set_tester(
    settings: &Settings,
    public: &PublicPrefix,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    if !settings.tester_cookie.enabled {
        let mut response = Response::new(EdgeBody::empty());
        *response.status_mut() = StatusCode::NOT_FOUND;
        return Ok(response);
    }

    let set_cookie = HeaderValue::from_str(&format_tester_cookie(
        public,
        &settings.publisher.cookie_domain,
        settings.tester_cookie.max_age_seconds,
    ))
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
/// tester cookie scoped to `publisher.cookie_domain`.
///
/// # Errors
///
/// Returns [`TrustedServerError::InvalidHeaderValue`] if the configured cookie
/// domain cannot be rendered as an HTTP header value.
pub fn handle_clear_tester(
    settings: &Settings,
    public: &PublicPrefix,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    if !settings.tester_cookie.enabled {
        let mut response = Response::new(EdgeBody::empty());
        *response.status_mut() = StatusCode::NOT_FOUND;
        return Ok(response);
    }

    let set_cookie = HeaderValue::from_str(&format_clear_tester_cookie(
        public,
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

        let response = handle_set_tester(&settings, &PublicPrefix::default())
            .expect("should build tester response");

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
            set_cookie,
            "ts-tester=true; Domain=.tester.example; Path=/; Secure; SameSite=Lax; Max-Age=2592000",
            "tester cookie should use publisher.cookie_domain and persist across browser restarts"
        );
    }

    #[test]
    fn tester_cookie_uses_configured_max_age() {
        let mut settings = create_test_settings();
        settings.tester_cookie.enabled = true;
        settings.tester_cookie.max_age_seconds = 3_600;
        settings.publisher.cookie_domain = ".tester.example".to_string();

        let response = handle_set_tester(&settings, &PublicPrefix::default())
            .expect("should build tester response");

        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should set tester cookie")
            .to_str()
            .expect("should render set-cookie as utf-8");
        assert_eq!(
            set_cookie,
            "ts-tester=true; Domain=.tester.example; Path=/; Secure; SameSite=Lax; Max-Age=3600",
            "tester cookie should use the configured max_age_seconds"
        );
    }

    #[test]
    fn tester_cookie_is_named_after_the_public_prefix() {
        let mut settings = create_test_settings();
        settings.tester_cookie.enabled = true;
        let public = PublicPrefix::new(["_ex"]).expect("should accept the prefix");

        for response in [
            handle_set_tester(&settings, &public).expect("should set the tester cookie"),
            handle_clear_tester(&settings, &public).expect("should clear the tester cookie"),
        ] {
            let set_cookie = response
                .headers()
                .get(header::SET_COOKIE)
                .expect("should write the tester cookie")
                .to_str()
                .expect("should render set-cookie as utf-8");
            assert!(set_cookie.starts_with("_ex-tester="), "{set_cookie}");
        }
    }

    #[test]
    fn tester_cookie_is_under_the_removed_prefix_on_an_addressed_request() {
        let mut settings = create_test_settings();
        settings.tester_cookie.enabled = true;
        let public = PublicPrefix::default().under_path(Some("/publisher-a.example"));

        for response in [
            handle_set_tester(&settings, &public).expect("should set the tester cookie"),
            handle_clear_tester(&settings, &public).expect("should clear the tester cookie"),
        ] {
            let set_cookie = response
                .headers()
                .get(header::SET_COOKIE)
                .expect("should write the tester cookie")
                .to_str()
                .expect("should render set-cookie as utf-8");
            assert!(
                set_cookie.contains("; Path=/publisher-a.example; "),
                "{set_cookie}"
            );
        }
    }

    #[test]
    fn tester_cookie_route_is_disabled_by_default() {
        let settings = create_test_settings();

        let response = handle_set_tester(&settings, &PublicPrefix::default())
            .expect("should build disabled tester response");

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

        let response = handle_clear_tester(&settings, &PublicPrefix::default())
            .expect("should build clear tester response");

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

        let response = handle_clear_tester(&settings, &PublicPrefix::default())
            .expect("should build disabled clear tester response");

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
