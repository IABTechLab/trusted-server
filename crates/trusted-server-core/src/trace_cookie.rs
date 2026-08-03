//! Render-trace toggle endpoint helpers.
//!
//! `GET /_ts/trace` arms (or with `?enabled=false` disarms) the first-party
//! `ts-trace` cookie and redirects to `/`. While the cookie is present, the
//! TSJS render-trace layer draws a visible badge on every traced creative so
//! an operator can see on the page itself that a creative was delivered by
//! Trusted Server — and via which render path (SSAT/GAM or `/auction`).
//!
//! The route is gated behind `[debug] trace_route_enabled` and returns
//! `404 Not Found` while disabled, mirroring the tester-cookie endpoints. The
//! badge only surfaces data already exposed on `window.tsjs`, so the cookie
//! gates visibility, not access.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{HeaderValue, Response, StatusCode, header};

use crate::constants::COOKIE_TS_TRACE;
use crate::error::TrustedServerError;
use crate::settings::Settings;

/// How long an armed trace cookie lives, in seconds.
///
/// One hour: long enough for a debugging session across reloads and SPA
/// navigations, short enough that a forgotten toggle expires on its own.
const TRACE_COOKIE_MAX_AGE_SECS: u32 = 3600;

/// Formats the trace cookie `Set-Cookie` header value.
///
/// Deliberately host-only (no `Domain` attribute): a `Domain` scoped to
/// `publisher.cookie_domain` would be rejected by the browser during local
/// development against `127.0.0.1`/`localhost`, and the overlay only needs to
/// work on the exact host being debugged. Also neither `HttpOnly` (the TSJS
/// overlay must read it from `document.cookie`) nor `Secure` (the badge is a
/// debug aid that must work through plain-HTTP local dev proxies, and the
/// cookie carries no data worth protecting).
fn format_trace_cookie() -> String {
    format!(
        "{}=1; Path=/; SameSite=Lax; Max-Age={}",
        COOKIE_TS_TRACE, TRACE_COOKIE_MAX_AGE_SECS,
    )
}

/// Formats the trace cookie clearing `Set-Cookie` header value.
fn format_clear_trace_cookie() -> String {
    format!("{}=; Path=/; SameSite=Lax; Max-Age=0", COOKIE_TS_TRACE)
}

/// Whether the request's query string asks to disarm the trace cookie.
///
/// Only an explicit `enabled=false` (or `enabled=0`) disarms; any other query
/// — including none at all — arms it, so `GET /_ts/trace` alone switches the
/// overlay on.
fn query_disables(query: Option<&str>) -> bool {
    query.is_some_and(|q| {
        q.split('&')
            .any(|pair| pair == "enabled=false" || pair == "enabled=0")
    })
}

/// Handles `GET /_ts/trace`.
///
/// Returns `404 Not Found` while `[debug] trace_route_enabled` is false. When
/// enabled, sets (or with `?enabled=false` clears) the `ts-trace` cookie
/// scoped to `publisher.cookie_domain` and returns `302 Found` redirecting to
/// `/` — landing back on the homepage confirms the toggle round-trip worked.
///
/// # Errors
///
/// Returns [`TrustedServerError::InvalidHeaderValue`] if the configured cookie
/// domain cannot be rendered as an HTTP header value.
pub fn handle_trace_mode(
    settings: &Settings,
    query: Option<&str>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    if !settings.debug.trace_route_enabled {
        let mut response = Response::new(EdgeBody::empty());
        *response.status_mut() = StatusCode::NOT_FOUND;
        return Ok(response);
    }

    let cookie_value = if query_disables(query) {
        format_clear_trace_cookie()
    } else {
        format_trace_cookie()
    };
    let set_cookie = HeaderValue::from_str(&cookie_value).change_context(
        TrustedServerError::InvalidHeaderValue {
            message: "trace cookie contains invalid header value".to_string(),
        },
    )?;

    let mut response = Response::new(EdgeBody::empty());
    *response.status_mut() = StatusCode::FOUND;
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/"));
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

    fn trace_enabled_settings() -> Settings {
        let mut settings = create_test_settings();
        settings.debug.trace_route_enabled = true;
        settings
    }

    #[test]
    fn trace_route_arms_cookie_and_redirects_to_root() {
        let settings = trace_enabled_settings();

        let response = handle_trace_mode(&settings, None).expect("should build trace response");

        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "enabled trace route should redirect"
        );
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/"),
            "should redirect to the site root"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store, private"),
            "trace route should not be cacheable"
        );
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should set trace cookie")
            .to_str()
            .expect("should render set-cookie as utf-8");
        assert_eq!(
            set_cookie, "ts-trace=1; Path=/; SameSite=Lax; Max-Age=3600",
            "trace cookie should be host-only with a bounded lifetime"
        );
    }

    #[test]
    fn trace_route_clears_cookie_when_disabled_by_query() {
        let settings = trace_enabled_settings();

        let response = handle_trace_mode(&settings, Some("enabled=false"))
            .expect("should build trace clear response");

        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "clearing should still redirect"
        );
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should clear trace cookie")
            .to_str()
            .expect("should render set-cookie as utf-8");
        assert_eq!(
            set_cookie, "ts-trace=; Path=/; SameSite=Lax; Max-Age=0",
            "trace cookie clear should expire the cookie"
        );
    }

    #[test]
    fn trace_route_arms_cookie_for_unrelated_query() {
        let settings = trace_enabled_settings();

        let response = handle_trace_mode(&settings, Some("enabled=true&foo=bar"))
            .expect("should build trace response");

        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("should set trace cookie")
            .to_str()
            .expect("should render set-cookie as utf-8");
        assert!(
            set_cookie.starts_with("ts-trace=1;"),
            "non-disabling query should arm the cookie"
        );
    }

    #[test]
    fn trace_route_is_disabled_by_default() {
        let settings = create_test_settings();

        let response =
            handle_trace_mode(&settings, None).expect("should build disabled trace response");

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "disabled trace route should return not found"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "disabled trace route should not set a cookie"
        );
    }
}
