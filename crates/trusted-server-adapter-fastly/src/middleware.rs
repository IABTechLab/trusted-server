//! Middleware implementations for the dual-path entry point.
//!
//! Provides two middleware types that mirror the finalization and auth logic
//! from the legacy [`crate::finalize_response`] and [`crate::route_request`]:
//!
//! - [`FinalizeResponseMiddleware`] — geo lookup and standard TS header injection
//! - [`AuthMiddleware`] — basic-auth enforcement via [`enforce_basic_auth`]
//!
//! Registration order in [`crate::app`]: `FinalizeResponseMiddleware` outermost,
//! then `AuthMiddleware`. This ensures auth-rejected responses also receive the
//! standard TS headers before being returned to the client.

use std::sync::Arc;

use async_trait::async_trait;
use edgezero_adapter_fastly::FastlyRequestContext;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderName, HeaderValue, Response, StatusCode};
use edgezero_core::middleware::{Middleware, Next};
use edgezero_core::response::IntoResponse;
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE,
    HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::platform::PlatformGeo;
use trusted_server_core::settings::Settings;

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
/// the `main.rs` entry point, which is idempotent for normal requests.
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
                e.into_response()
            }
        };

        // Skip geo lookup for authentication rejections — the lookup is unnecessary for 401s.
        let geo_info = if response.status() != StatusCode::UNAUTHORIZED {
            self.geo.lookup(client_ip).unwrap_or_else(|e| {
                log::warn!("geo lookup failed: {e}");
                None
            })
        } else {
            None
        };

        apply_finalize_headers(&self.settings, geo_info.as_ref(), &mut response);

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
/// - `Err(report)` → internal error; log and convert to a 500 HTTP response.
///
/// # Errors
///
/// When [`enforce_basic_auth`] returns an error report, converts it to a 500 HTTP
/// response so that [`FinalizeResponseMiddleware`] can still inject standard TS
/// headers before the response reaches the client.
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
// apply_finalize_headers — extracted for unit testing
// ---------------------------------------------------------------------------

/// Applies all standard Trusted Server response headers to the given response.
///
/// Mirrors [`crate::finalize_response`] exactly, operating on [`Response`] from
/// `edgezero_core::http` instead of `HttpResponse`.
///
/// Header write order (last write wins):
/// 1. Geo headers (`x-geo-*`) — or `X-Geo-Info-Available: false` when absent
/// 2. `X-TS-Version` from `FASTLY_SERVICE_VERSION` env var
/// 3. `X-TS-ENV: staging` when `FASTLY_IS_STAGING == "1"`
/// 4. `settings.response_headers` — operator-configured overrides applied last
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

    for (key, value) in &settings.response_headers {
        let header_name = HeaderName::from_bytes(key.as_bytes())
            .expect("settings.response_headers validated at load time");
        let header_value =
            HeaderValue::from_str(value).expect("settings.response_headers validated at load time");
        response.headers_mut().insert(header_name, header_value);
    }
}

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
    use edgezero_core::http::{request_builder, response_builder, Method, StatusCode};
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

    fn settings_with_response_headers(headers: Vec<(&str, &str)>) -> Settings {
        let mut s =
            trusted_server_core::settings_data::get_settings().expect("should load test settings");
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
    fn auth_handle_passes_through_when_auth_not_configured() {
        let settings =
            trusted_server_core::settings_data::get_settings().expect("should load test settings");
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
