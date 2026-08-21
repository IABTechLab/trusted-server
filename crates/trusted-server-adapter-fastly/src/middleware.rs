//! Middleware implementations for the `EdgeZero` entry point.
//!
//! Provides two middleware types used by the `EdgeZero` entry point:
//!
//! - [`FinalizeResponseMiddleware`] — geo lookup and standard TS header injection
//! - [`AuthMiddleware`] — basic-auth enforcement via [`enforce_basic_auth`]
//!
//! Registration order in [`crate::app`]: `FinalizeResponseMiddleware` outermost,
//! then `AuthMiddleware`. This ensures auth-rejected responses also receive the
//! standard TS headers before being returned to the client.

use std::sync::Arc;

use async_trait::async_trait;
use edgezero_adapter_fastly::context::FastlyRequestContext;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Response, StatusCode};
use edgezero_core::middleware::{Middleware, Next};
use edgezero_core::response::IntoResponse;
use std::net::IpAddr;
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE,
    HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::platform::PlatformGeo;
use trusted_server_core::settings::Settings;

pub(crate) const HEADER_X_TS_FINALIZED: &str = "x-ts-finalized";

// ---------------------------------------------------------------------------
// FinalizeResponseMiddleware
// ---------------------------------------------------------------------------

/// Outermost middleware: performs geo lookup and injects all standard TS response headers.
///
/// Registered first in the middleware chain so that it wraps all inner middleware
/// (including [`AuthMiddleware`]) and the handler. This guarantees every registered-route
/// response — including auth-rejected ones — carries a consistent set of headers.
///
/// Router-level 405/404 responses for unregistered HTTP methods (e.g. TRACE) bypass the
/// middleware chain. Those are covered by a second call to [`apply_finalize_headers`] at
/// the `main.rs` entry point. Middleware-finalized responses carry
/// [`HEADER_X_TS_FINALIZED`] so the entry point can skip duplicate finalization.
///
/// # Header precedence
///
/// Headers are written in this order (last write wins):
/// 1. Geo headers (or `X-Geo-Info-Available: false` when geo is unavailable)
/// 2. `X-TS-Version` from `FASTLY_SERVICE_VERSION` env var
/// 3. `X-TS-ENV: staging` when `FASTLY_IS_STAGING == "1"`
/// 4. Operator-configured `settings.response_headers` (can override any managed header)
pub struct FinalizeResponseMiddleware {
    settings: Arc<Settings>,
    geo: Arc<dyn PlatformGeo>,
}

impl FinalizeResponseMiddleware {
    /// Creates a new [`FinalizeResponseMiddleware`] with the given settings and geo lookup service.
    pub fn new(settings: Arc<Settings>, geo: Arc<dyn PlatformGeo>) -> Self {
        Self { settings, geo }
    }
}

#[async_trait(?Send)]
impl Middleware for FinalizeResponseMiddleware {
    async fn handle(&self, ctx: RequestContext, next: Next<'_>) -> Result<Response, EdgeError> {
        let client_ip = FastlyRequestContext::get(ctx.request()).and_then(|c| c.client_ip);

        let mut response = match next.run(ctx).await {
            Ok(r) => r,
            Err(e) => {
                log::error!("request handler failed: {e:?}");
                e.into_response()?
            }
        };

        let geo_info = resolve_geo_for_response(&response, client_ip, |ip| {
            self.geo.lookup(ip).unwrap_or_else(|e| {
                log::warn!("geo lookup failed: {e}");
                None
            })
        });

        apply_finalize_headers(&self.settings, geo_info.as_ref(), &mut response);
        response
            .headers_mut()
            .insert(HEADER_X_TS_FINALIZED, HeaderValue::from_static("1"));

        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// AuthMiddleware
// ---------------------------------------------------------------------------

/// Inner middleware: enforces basic-auth before the handler runs.
///
/// - `Ok(Some(response))` from [`enforce_basic_auth`] → auth failed; return the
///   challenge response (bubbles through [`FinalizeResponseMiddleware`] for header injection).
/// - `Ok(None)` → no auth required or credentials accepted; continue the chain.
/// - `Err(report)` → internal error; log and convert to an HTTP response via
///   [`crate::app::http_error`] using the error's documented status code.
///
/// # Errors
///
/// When [`enforce_basic_auth`] returns an error report, converts it to an HTTP
/// response via [`crate::app::http_error`] (preserving the error's status code)
/// so that [`FinalizeResponseMiddleware`] can still inject standard TS headers
/// before the response reaches the client.
pub struct AuthMiddleware {
    settings: Arc<Settings>,
}

impl AuthMiddleware {
    /// Creates a new [`AuthMiddleware`] with the given settings.
    pub fn new(settings: Arc<Settings>) -> Self {
        Self { settings }
    }
}

#[async_trait(?Send)]
impl Middleware for AuthMiddleware {
    async fn handle(&self, ctx: RequestContext, next: Next<'_>) -> Result<Response, EdgeError> {
        match enforce_basic_auth(&self.settings, ctx.request()) {
            Ok(Some(response)) => return Ok(response),
            Ok(None) => {}
            Err(report) => {
                log::error!("auth check failed: {:?}", report);
                return Ok(crate::app::http_error(&report));
            }
        }

        next.run(ctx).await
    }
}

// ---------------------------------------------------------------------------
// Shared geo resolution helper
// ---------------------------------------------------------------------------

/// Resolves geo for a response, skipping the lookup for 401 responses.
///
/// Returns `None` for authentication rejections (401) without calling `lookup_geo`
/// to avoid unnecessary work and exposing geo data to unauthenticated callers.
/// All other responses call `lookup_geo` and return its result.
///
/// Used by both [`FinalizeResponseMiddleware`] and the entry-point finalization
/// in `main.rs` so the 401-skip rule is defined in one place.
///
/// # Parity note
///
/// Before legacy cleanup, the Fastly-native path skipped geo only for this
/// server's own auth challenges; origin-forwarded 401s still received geo
/// headers there. The `EdgeZero` path skips geo for **all** 401s by status. This
/// is intentionally more conservative: geo data is not sent to any
/// unauthenticated caller regardless of whether the 401 originated from this
/// server or the upstream origin.
pub(crate) fn resolve_geo_for_response<F>(
    response: &Response,
    client_ip: Option<IpAddr>,
    lookup_geo: F,
) -> Option<GeoInfo>
where
    F: FnOnce(Option<IpAddr>) -> Option<GeoInfo>,
{
    if response.status() == StatusCode::UNAUTHORIZED {
        None
    } else {
        lookup_geo(client_ip)
    }
}

// ---------------------------------------------------------------------------
// apply_finalize_headers — extracted for unit testing
// ---------------------------------------------------------------------------

/// Applies all standard Trusted Server response headers to the given response.
///
/// Operates on [`Response`] from `edgezero_core::http`.
///
/// Header write order (last write wins):
/// 1. Geo headers (`x-geo-*`) — or `X-Geo-Info-Available: false` when absent
/// 2. `X-TS-Version` from `FASTLY_SERVICE_VERSION` env var
/// 3. `X-TS-ENV: staging` when `FASTLY_IS_STAGING == "1"`
/// 4. Set-Cookie cache privacy — strip surrogate cache headers and downgrade
///    `Cache-Control` to `private, max-age=0` on cookie-bearing responses
/// 5. `settings.response_headers` — operator-configured overrides, except the
///    cache-controlling headers are skipped on uncacheable (`private`/`no-store`)
///    responses so operators cannot re-enable shared caching for per-user payloads
pub(crate) fn apply_finalize_headers(
    settings: &Settings,
    geo_info: Option<&GeoInfo>,
    response: &mut Response,
) {
    if let Some(geo) = geo_info {
        geo.set_response_headers(response);
    } else {
        response.headers_mut().insert(
            HEADER_X_GEO_INFO_AVAILABLE,
            HeaderValue::from_static("false"),
        );
    }

    if let Ok(v) = std::env::var(ENV_FASTLY_SERVICE_VERSION) {
        if let Ok(value) = HeaderValue::from_str(&v) {
            response.headers_mut().insert(HEADER_X_TS_VERSION, value);
        } else {
            log::warn!("Skipping invalid FASTLY_SERVICE_VERSION response header value");
        }
    }

    if std::env::var(ENV_FASTLY_IS_STAGING).as_deref() == Ok("1") {
        response
            .headers_mut()
            .insert(HEADER_X_TS_ENV, HeaderValue::from_static("staging"));
    }

    // Any response that sets a per-user cookie (notably the EC identity cookie)
    // must never be shared-cached, and per-user responses (assembled HTML,
    // page-bids, cookie-bearing navigations) must not have their uncacheable
    // Cache-Control re-enabled by operator headers. This shared helper runs
    // byte-identically on every adapter so the privacy guarantee can't drift.
    trusted_server_core::response_privacy::apply_response_headers_with_cache_privacy(
        settings, response,
    );
}

/// Forces cookie-bearing responses to stay private to shared caches.
///
/// Re-exported from [`trusted_server_core::response_privacy`] so the `EdgeZero`
/// entry point (`main.rs`) can re-apply it after
/// [`ec_finalize_response`](trusted_server_core::ec::finalize::ec_finalize_response)
/// writes the EC identity `Set-Cookie`, using the single shared implementation.
pub(crate) use trusted_server_core::response_privacy::enforce_set_cookie_cache_privacy;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::sync::Arc;

    use edgezero_core::body::Body;
    use edgezero_core::context::RequestContext;
    use edgezero_core::error::EdgeError;
    use edgezero_core::http::{HeaderName, Method, StatusCode, request_builder, response_builder};
    use edgezero_core::middleware::Next;
    use edgezero_core::params::PathParams;
    use error_stack::Report;
    use futures::executor::block_on;
    use trusted_server_core::platform::{PlatformError, PlatformGeo};

    fn empty_response() -> Response {
        response_builder()
            .body(Body::empty())
            .expect("should build empty test response")
    }

    fn empty_ctx() -> RequestContext {
        let req = request_builder()
            .method(Method::GET)
            .uri("/test")
            .body(Body::empty())
            .expect("should build test request");
        RequestContext::new(req, PathParams::new(HashMap::new()))
    }

    struct FixedGeo(Option<GeoInfo>);

    impl PlatformGeo for FixedGeo {
        fn lookup(&self, _: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            Ok(self.0.clone())
        }
    }

    fn test_settings() -> Settings {
        Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [geo]
            default_country = "FR"

            [ec]
            provider = "hmac"

            [ec.providers.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            enabled = false
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
            "#,
        )
        .expect("should parse test settings")
    }

    fn settings_with_response_headers(headers: Vec<(&str, &str)>) -> Settings {
        let mut s = test_settings();
        s.response_headers = headers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s
    }

    #[test]
    fn operator_response_headers_override_earlier_headers() {
        let settings =
            settings_with_response_headers(vec![("X-Geo-Info-Available", "operator-override")]);
        let mut response = empty_response();

        // No geo_info → would set "false"; operator header should win instead.
        apply_finalize_headers(&settings, None, &mut response);

        assert_eq!(
            response
                .headers()
                .get("x-geo-info-available")
                .and_then(|v| v.to_str().ok()),
            Some("operator-override"),
            "should override the managed geo header with the operator-configured value"
        );
    }

    #[test]
    fn sets_geo_unavailable_header_when_no_geo_info() {
        let settings = settings_with_response_headers(vec![]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, None, &mut response);

        assert_eq!(
            response
                .headers()
                .get("x-geo-info-available")
                .and_then(|v| v.to_str().ok()),
            Some("false"),
            "should set X-Geo-Info-Available: false when no geo info is available"
        );
    }

    fn response_with_headers(pairs: &[(&'static str, &'static str)]) -> Response {
        let mut response = empty_response();
        for (key, value) in pairs {
            response.headers_mut().insert(
                HeaderName::from_static(key),
                HeaderValue::from_static(value),
            );
        }
        response
    }

    #[test]
    fn apply_finalize_headers_downgrades_public_set_cookie_response() {
        // A per-user cookie response that arrives shared-cacheable (origin-public
        // plus a surrogate directive) must be downgraded so a shared cache cannot
        // store and replay one visitor's Set-Cookie.
        let settings = settings_with_response_headers(vec![]);
        let mut response = response_with_headers(&[
            ("set-cookie", "ts-ec=abc; Path=/"),
            ("cache-control", "public, max-age=600"),
            ("surrogate-control", "max-age=600"),
        ]);

        apply_finalize_headers(&settings, None, &mut response);

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("private, max-age=0"),
            "should downgrade a public cookie response to private"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "should strip surrogate-control from a cookie-bearing response"
        );
    }

    #[test]
    fn apply_finalize_headers_blocks_operator_surrogate_on_private_response() {
        // Operator response_headers must not re-enable shared caching for an
        // uncacheable (private) per-user response — neither by replacing
        // Cache-Control nor by reintroducing surrogate cache headers.
        let settings = settings_with_response_headers(vec![
            ("cache-control", "public, max-age=3600"),
            ("surrogate-control", "max-age=3600"),
            ("x-operator", "kept"),
        ]);
        let mut response = response_with_headers(&[("cache-control", "private, max-age=0")]);

        apply_finalize_headers(&settings, None, &mut response);

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("private, max-age=0"),
            "operator cache-control must not weaken a private response"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "operator surrogate-control must not be applied to a private response"
        );
        assert_eq!(
            response
                .headers()
                .get("x-operator")
                .and_then(|v| v.to_str().ok()),
            Some("kept"),
            "non-cache operator headers must still apply"
        );
    }

    #[test]
    fn enforce_set_cookie_cache_privacy_downgrades_late_cookie() {
        // Mirrors the EdgeZero post-ec_finalize guard: a Set-Cookie added after
        // finalize headers ran (origin-public response) must be downgraded.
        let mut response = response_with_headers(&[
            ("set-cookie", "ts-ec=abc; Path=/"),
            ("cache-control", "public, max-age=600"),
            ("surrogate-control", "max-age=600"),
        ]);

        enforce_set_cookie_cache_privacy(&mut response);

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("private, max-age=0"),
            "should downgrade a late public cookie response to private"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "should strip surrogate-control from the late cookie response"
        );
    }

    #[test]
    fn enforce_set_cookie_cache_privacy_keeps_stricter_no_store() {
        // Idempotent: a stricter no-store directive is preserved, but surrogate
        // headers still come off.
        let mut response = response_with_headers(&[
            ("set-cookie", "ts-ec=abc; Path=/"),
            ("cache-control", "no-store"),
            ("surrogate-control", "max-age=600"),
        ]);

        enforce_set_cookie_cache_privacy(&mut response);

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "should keep the stricter no-store directive"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "should strip surrogate-control even when keeping no-store"
        );
    }

    #[test]
    fn enforce_set_cookie_cache_privacy_ignores_cookieless_response() {
        let mut response = response_with_headers(&[("cache-control", "public, max-age=600")]);

        enforce_set_cookie_cache_privacy(&mut response);

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=600"),
            "should leave a cookieless response untouched"
        );
    }

    // ---------------------------------------------------------------------------
    // FinalizeResponseMiddleware::handle tests
    // ---------------------------------------------------------------------------

    #[test]
    fn finalize_handle_injects_geo_unavailable_on_ok_response() {
        let settings = settings_with_response_headers(vec![]);
        let middleware =
            FinalizeResponseMiddleware::new(Arc::new(settings), Arc::new(FixedGeo(None)));
        let handler =
            Arc::new(
                |_ctx: RequestContext| async move { Ok::<Response, EdgeError>(empty_response()) },
            );

        let response = block_on(middleware.handle(empty_ctx(), Next::new(&[], &*handler)))
            .expect("should succeed");

        assert_eq!(
            response
                .headers()
                .get("x-geo-info-available")
                .and_then(|v| v.to_str().ok()),
            Some("false"),
            "should set X-Geo-Info-Available: false when geo returns None"
        );
    }

    #[test]
    fn finalize_handle_marks_response_as_finalized() {
        let settings = settings_with_response_headers(vec![]);
        let middleware =
            FinalizeResponseMiddleware::new(Arc::new(settings), Arc::new(FixedGeo(None)));
        let handler =
            Arc::new(
                |_ctx: RequestContext| async move { Ok::<Response, EdgeError>(empty_response()) },
            );

        let response = block_on(middleware.handle(empty_ctx(), Next::new(&[], &*handler)))
            .expect("should succeed");

        assert_eq!(
            response
                .headers()
                .get("x-ts-finalized")
                .and_then(|v| v.to_str().ok()),
            Some("1"),
            "middleware-finalized responses should carry the entry-point sentinel"
        );
    }

    #[test]
    fn finalize_handle_absorbs_handler_error_and_injects_headers() {
        let settings = settings_with_response_headers(vec![]);
        let middleware =
            FinalizeResponseMiddleware::new(Arc::new(settings), Arc::new(FixedGeo(None)));
        let handler = Arc::new(|_ctx: RequestContext| async move {
            Err::<Response, EdgeError>(EdgeError::service_unavailable("test error"))
        });

        let response = block_on(middleware.handle(empty_ctx(), Next::new(&[], &*handler)))
            .expect("should absorb handler error into a response");

        assert!(
            response.status().is_server_error(),
            "should produce a server-error status for absorbed handler error"
        );
        assert!(
            response.headers().get("x-geo-info-available").is_some(),
            "absorbed error response should still carry geo header"
        );
    }

    #[test]
    #[allow(clippy::panic)]
    fn finalize_handle_skips_geo_lookup_for_401() {
        struct PanicGeo;
        impl PlatformGeo for PanicGeo {
            fn lookup(&self, _: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
                panic!("should not call geo for 401 responses")
            }
        }

        let settings = settings_with_response_headers(vec![]);
        let middleware = FinalizeResponseMiddleware::new(Arc::new(settings), Arc::new(PanicGeo));
        let handler = Arc::new(|_ctx: RequestContext| async move {
            let mut resp = empty_response();
            *resp.status_mut() = StatusCode::UNAUTHORIZED;
            Ok::<Response, EdgeError>(resp)
        });

        let response = block_on(middleware.handle(empty_ctx(), Next::new(&[], &*handler)))
            .expect("should succeed without calling geo");

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should preserve 401 status"
        );
        assert_eq!(
            response
                .headers()
                .get("x-geo-info-available")
                .and_then(|v| v.to_str().ok()),
            Some("false"),
            "should set geo-unavailable header without calling geo for 401"
        );
    }

    // ---------------------------------------------------------------------------
    // AuthMiddleware::handle tests
    // ---------------------------------------------------------------------------

    #[test]
    fn finalize_handle_preserves_duplicate_set_cookie_headers() {
        // Regression guard: FinalizeResponseMiddleware must not drop duplicate
        // Set-Cookie headers. The old dispatch_with_config_handle path silently
        // collapsed them because fastly::Response uses set_header (last-wins).
        // This test verifies the EdgeZero middleware chain is header-transparent.
        let settings = settings_with_response_headers(vec![]);
        let middleware =
            FinalizeResponseMiddleware::new(Arc::new(settings), Arc::new(FixedGeo(None)));
        let handler = Arc::new(|_ctx: RequestContext| async move {
            let resp = response_builder()
                .header("set-cookie", "session=abc; Path=/; HttpOnly")
                .header("set-cookie", "tracker=xyz; Path=/; SameSite=Lax")
                .body(Body::empty())
                .expect("should build response with two Set-Cookie headers");
            Ok::<Response, EdgeError>(resp)
        });

        let response = block_on(middleware.handle(empty_ctx(), Next::new(&[], &*handler)))
            .expect("should succeed");

        let cookie_count = response.headers().get_all("set-cookie").iter().count();
        assert_eq!(
            cookie_count, 2,
            "FinalizeResponseMiddleware must not drop duplicate Set-Cookie headers"
        );
    }

    #[test]
    fn auth_handle_passes_through_when_auth_not_configured() {
        let settings = test_settings();
        let middleware = AuthMiddleware::new(Arc::new(settings));
        let handler =
            Arc::new(
                |_ctx: RequestContext| async move { Ok::<Response, EdgeError>(empty_response()) },
            );

        let response = block_on(middleware.handle(empty_ctx(), Next::new(&[], &*handler)))
            .expect("should pass through when auth is not configured");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should reach the handler when auth is not required"
        );
    }
}
