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
use edgezero_core::http::{HeaderName, HeaderValue, Response};
use edgezero_core::middleware::{Middleware, Next};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::{
    ENV_FASTLY_IS_STAGING, ENV_FASTLY_SERVICE_VERSION, HEADER_X_GEO_INFO_AVAILABLE,
    HEADER_X_TS_ENV, HEADER_X_TS_VERSION,
};
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::platform::PlatformGeo as _;
use trusted_server_core::settings::Settings;

use crate::platform::FastlyPlatformGeo;

// ---------------------------------------------------------------------------
// FinalizeResponseMiddleware
// ---------------------------------------------------------------------------

/// Outermost middleware: performs geo lookup and injects all standard TS response headers.
///
/// Registered first in the middleware chain so that it wraps all inner middleware
/// (including [`AuthMiddleware`]) and the handler. This guarantees every outgoing
/// response — including auth-rejected ones — carries a consistent set of headers.
///
/// # Header precedence
///
/// Headers are written in this order (last write wins):
/// 1. Geo headers (or `X-Geo-Info-Available: false` when geo is unavailable)
/// 2. `X-TS-Version` from `FASTLY_SERVICE_VERSION` env var
/// 3. `X-TS-ENV: staging` when `FASTLY_IS_STAGING == "1"`
/// 4. Operator-configured `settings.response_headers` (can override any managed header)
// Used in Task 4 when app.rs registers the middleware chain.
#[allow(dead_code)]
pub struct FinalizeResponseMiddleware {
    settings: Arc<Settings>,
}

impl FinalizeResponseMiddleware {
    /// Creates a new [`FinalizeResponseMiddleware`] with the given settings.
    #[allow(dead_code)]
    pub fn new(settings: Arc<Settings>) -> Self {
        Self { settings }
    }
}

#[async_trait(?Send)]
impl Middleware for FinalizeResponseMiddleware {
    async fn handle(&self, ctx: RequestContext, next: Next<'_>) -> Result<Response, EdgeError> {
        let client_ip = FastlyRequestContext::get(ctx.request()).and_then(|c| c.client_ip);

        let geo_info = FastlyPlatformGeo.lookup(client_ip).unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });

        let mut response = next.run(ctx).await?;

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
/// - `Err(report)` → internal error; log and return [`EdgeError::internal`].
///
/// # Errors
///
/// Returns [`EdgeError::internal`] when [`enforce_basic_auth`] returns an error report.
// Used in Task 4 when app.rs registers the middleware chain.
#[allow(dead_code)]
pub struct AuthMiddleware {
    settings: Arc<Settings>,
}

impl AuthMiddleware {
    /// Creates a new [`AuthMiddleware`] with the given settings.
    #[allow(dead_code)]
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
                // `EdgeError::internal` requires `E: Into<anyhow::Error>`.
                // `std::io::Error` satisfies this bound without pulling in anyhow
                // as a direct dependency (which the project convention forbids).
                return Err(EdgeError::internal(std::io::Error::other(format!(
                    "auth check failed: {report}"
                ))));
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
// Called from FinalizeResponseMiddleware::handle and from tests.
// This function is gated behind #[allow(dead_code)] until Task 4 wires app.rs.
#[allow(dead_code)]
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
}
