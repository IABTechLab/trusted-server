use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderName, HeaderValue, Response};
use edgezero_core::middleware::{Middleware, Next};
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
/// `geo_available` controls `X-Geo-Info-Available`. Spin passes `false`
/// because it has no geo headers. Operator-configured
/// `settings.response_headers` are applied last and can override any managed
/// header.
pub(crate) fn apply_finalize_headers(
    settings: &Settings,
    geo_available: bool,
    response: &mut Response,
) {
    response.headers_mut().insert(
        HEADER_X_GEO_INFO_AVAILABLE,
        HeaderValue::from_static(if geo_available { "true" } else { "false" }),
    );

    for (key, value) in &settings.response_headers {
        let header_name = HeaderName::from_bytes(key.as_bytes());
        let header_value = HeaderValue::from_str(value);
        if let (Ok(header_name), Ok(header_value)) = (header_name, header_value) {
            response.headers_mut().insert(header_name, header_value);
        } else {
            log::warn!(
                "Skipping invalid configured response header value for {}",
                key
            );
        }
    }
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
        let mut s =
            trusted_server_core::settings_data::get_settings().expect("should load test settings");
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
