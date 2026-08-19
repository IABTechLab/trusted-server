use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Response};
use edgezero_core::middleware::{Middleware, Next};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE;
use trusted_server_core::settings::Settings;

// ---------------------------------------------------------------------------
// FinalizeResponseMiddleware
// ---------------------------------------------------------------------------

/// Outermost middleware: injects all standard TS response headers.
///
/// Geo lookup is unavailable in the Axum dev server — `X-Geo-Info-Available: false`
/// is always emitted. Fastly-specific headers (`X-TS-Version`, `X-TS-ENV`) are
/// skipped because the corresponding env vars are not set in a local dev context.
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
        let mut response = next.run(ctx).await?;
        apply_finalize_headers(&self.settings, &mut response);
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
// apply_finalize_headers — extracted for unit testing
// ---------------------------------------------------------------------------

/// Applies standard Trusted Server response headers to the given response.
///
/// Unlike the Fastly variant, geo is always unavailable so `X-Geo-Info-Available: false`
/// is unconditionally emitted. Fastly-specific headers are omitted.
/// Operator-configured `settings.response_headers` are applied last (with the
/// shared cookie cache-privacy hardening) and can override any managed header.
pub(crate) fn apply_finalize_headers(settings: &Settings, response: &mut Response) {
    response.headers_mut().insert(
        HEADER_X_GEO_INFO_AVAILABLE,
        HeaderValue::from_static("false"),
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
                provider = "hmac"

                [ec.providers.hmac]
                passphrase = "test-secret-key-32-bytes-minimum"

                [geo]
                default_country = "FR"
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
    fn sets_geo_unavailable_header() {
        let settings = settings_with_response_headers(vec![]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, &mut response);

        assert_eq!(
            response
                .headers()
                .get("x-geo-info-available")
                .and_then(|v| v.to_str().ok()),
            Some("false"),
            "should set X-Geo-Info-Available: false"
        );
    }

    #[test]
    fn operator_response_headers_override_geo_header() {
        let settings =
            settings_with_response_headers(vec![("X-Geo-Info-Available", "operator-override")]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, &mut response);

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

        apply_finalize_headers(&settings, &mut response);

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
