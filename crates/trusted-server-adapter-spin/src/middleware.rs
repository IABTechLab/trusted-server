use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Response};
use edgezero_core::middleware::{Middleware, Next};
use edgezero_core::response::IntoResponse;
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE;
use trusted_server_core::settings::Settings;

// ---------------------------------------------------------------------------
// FinalizeResponseMiddleware
// ---------------------------------------------------------------------------

/// Outermost middleware: injects all standard TS response headers.
///
/// Spin does not expose geo headers to the application, so
/// `X-Geo-Info-Available: false` is emitted for every response.
///
/// Registered first in the middleware chain so that every outgoing response —
/// including auth-rejected ones — carries a consistent set of headers.
pub struct FinalizeResponseMiddleware {
    settings: Arc<Settings>,
}

impl FinalizeResponseMiddleware {
    /// Creates a new [`FinalizeResponseMiddleware`] with the given settings.
    #[must_use]
    pub fn new(settings: Arc<Settings>) -> Self {
        Self { settings }
    }
}

#[async_trait(?Send)]
impl Middleware for FinalizeResponseMiddleware {
    async fn handle(&self, ctx: RequestContext, next: Next<'_>) -> Result<Response, EdgeError> {
        let geo_available = false;

        let mut response = next.run(ctx).await?;
        apply_finalize_headers(&self.settings, geo_available, &mut response);
        trusted_server_core::integrations::ad_trace::finalize_response(&mut response);
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
pub struct AuthMiddleware {
    settings: Arc<Settings>,
}

impl AuthMiddleware {
    /// Creates a new [`AuthMiddleware`] with the given settings.
    #[must_use]
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
// NormalizeMiddleware
// ---------------------------------------------------------------------------

/// Request-normalization chokepoint.
///
/// Runs [`crate::app::normalize_spin_request`] on every routed request before
/// the handler executes, so the de-spoofing invariant — strip client-spoofable
/// `Forwarded` / `X-Forwarded-*` headers, derive the trusted Host, scheme, and
/// client IP from Spin's synthetic runtime headers — holds for *every* route
/// structurally rather than by per-handler convention. A future route, or a
/// signing handler that begins deriving an issuer/audience from `RequestInfo`,
/// cannot silently trust spoofable input by forgetting to opt in.
///
/// Registered outside [`AuthMiddleware`] so de-spoofing and console sanitation
/// also apply when auth short-circuits the request.
pub struct NormalizeMiddleware {
    settings: Arc<Settings>,
}

impl NormalizeMiddleware {
    /// Creates a new [`NormalizeMiddleware`].
    #[must_use]
    pub fn new(settings: Arc<Settings>) -> Self {
        Self { settings }
    }
}

#[async_trait(?Send)]
impl Middleware for NormalizeMiddleware {
    async fn handle(&self, mut ctx: RequestContext, next: Next<'_>) -> Result<Response, EdgeError> {
        crate::app::normalize_spin_request(ctx.request_mut());
        let decision = match trusted_server_core::integrations::ad_trace::prepare_request(
            &self.settings,
            ctx.request_mut(),
        ) {
            Ok(decision) => decision,
            Err(report) => {
                log::error!("ad trace request preparation failed: {report:?}");
                return Ok(crate::app::http_error(&report));
            }
        };
        let mut response = match next.run(ctx).await {
            Ok(response) => response,
            Err(error) => {
                log::error!("request handler failed after ad trace preparation: {error:?}");
                error.into_response()?
            }
        };
        trusted_server_core::integrations::ad_trace::attach_response_decision(
            &decision,
            &mut response,
        );
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// apply_finalize_headers — extracted for unit testing
// ---------------------------------------------------------------------------

/// Applies standard Trusted Server response headers to the given response.
///
/// `geo_available` controls `X-Geo-Info-Available`. Spin passes `false`
/// because it has no geo headers. Operator-configured
/// `settings.response_headers` are applied last (with the shared cookie
/// cache-privacy hardening) and can override any managed header.
pub(crate) fn apply_finalize_headers(
    settings: &Settings,
    geo_available: bool,
    response: &mut Response,
) {
    response.headers_mut().insert(
        HEADER_X_GEO_INFO_AVAILABLE,
        HeaderValue::from_static(if geo_available { "true" } else { "false" }),
    );

    // Cookie-bearing responses stay private to shared caches and operator
    // headers cannot re-enable caching for uncacheable per-user payloads.
    trusted_server_core::response_privacy::apply_response_headers_with_cache_privacy(
        settings, response,
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use edgezero_core::body::Body;
    use edgezero_core::http::response_builder;

    fn empty_response() -> Response {
        response_builder()
            .body(Body::empty())
            .expect("should build empty test response")
    }

    fn settings_with_response_headers(headers: Vec<(&str, &str)>) -> Settings {
        // Build from explicit test settings: the settings baked into the
        // binary contain placeholder secrets that `get_settings()` rejects
        // by design.
        let mut s = Settings::from_toml(
            r#"
                [[handlers]]
                path = "^/_ts/admin"
                username = "admin"
                password = "admin-pass"

                [publisher]
                domain = "test-publisher.example.com"
                cookie_domain = ".test-publisher.example.com"
                origin_url = "https://origin.test-publisher.example.com"
                proxy_secret = "unit-test-proxy-secret"

                [ec]
                passphrase = "test-secret-key-32-bytes-minimum"
            "#,
        )
        .expect("should load test settings");
        s.response_headers = headers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s
    }

    #[test]
    fn sets_geo_available_false_when_spin_has_no_geo() {
        let settings = settings_with_response_headers(vec![]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, false, &mut response);

        assert_eq!(
            response
                .headers()
                .get("x-geo-info-available")
                .and_then(|v| v.to_str().ok()),
            Some("false"),
            "should set X-Geo-Info-Available: false when geo is unavailable"
        );
    }

    #[test]
    fn sets_geo_available_true_when_requested_by_helper() {
        let settings = settings_with_response_headers(vec![]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, true, &mut response);

        assert_eq!(
            response
                .headers()
                .get("x-geo-info-available")
                .and_then(|v| v.to_str().ok()),
            Some("true"),
            "should set X-Geo-Info-Available: true when requested"
        );
    }

    #[test]
    fn operator_response_headers_override_geo_header() {
        let settings =
            settings_with_response_headers(vec![("X-Geo-Info-Available", "operator-override")]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, false, &mut response);

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
    fn applies_custom_operator_headers() {
        let settings = settings_with_response_headers(vec![("X-Custom-Header", "custom-value")]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, false, &mut response);

        assert_eq!(
            response
                .headers()
                .get("x-custom-header")
                .and_then(|v| v.to_str().ok()),
            Some("custom-value"),
            "should apply operator-configured response headers"
        );
    }
}
