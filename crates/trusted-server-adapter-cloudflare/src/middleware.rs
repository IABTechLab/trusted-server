use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Response};
use edgezero_core::middleware::{Middleware, Next};
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE;
use trusted_server_core::http_util::sanitize_trusted_client_ip_headers;
use trusted_server_core::settings::Settings;

// ---------------------------------------------------------------------------
// SanitizeRequestMiddleware
// ---------------------------------------------------------------------------

/// Outermost middleware: strips the configured client-IP trust headers from the
/// request before any inner middleware or handler observes them.
///
/// Must stay the first middleware registered in [`crate::app`]. Registering
/// another middleware ahead of it would re-expose the shared-secret
/// authentication header to request handling. Only the Fastly adapter consumes
/// these headers for client-IP resolution; every other adapter removes them so
/// a shared configuration cannot leak the secret into publisher or integration
/// request handling.
pub struct SanitizeRequestMiddleware {
    settings: Arc<Settings>,
}

impl SanitizeRequestMiddleware {
    /// Creates a new [`SanitizeRequestMiddleware`] with the given settings.
    #[must_use]
    pub fn new(settings: Arc<Settings>) -> Self {
        Self { settings }
    }
}

#[async_trait(?Send)]
impl Middleware for SanitizeRequestMiddleware {
    async fn handle(&self, mut ctx: RequestContext, next: Next<'_>) -> Result<Response, EdgeError> {
        sanitize_trusted_client_ip_headers(
            ctx.request_mut(),
            self.settings.trusted_client_ip.as_ref(),
        );
        next.run(ctx).await
    }
}

// ---------------------------------------------------------------------------
// FinalizeResponseMiddleware
// ---------------------------------------------------------------------------

/// Response-finalization middleware: injects all standard TS response headers.
///
/// Geo availability is determined by the presence of the `cf-ipcountry` header
/// (injected by the Cloudflare Workers runtime). On the native host target the
/// header is absent, so `X-Geo-Info-Available: false` is emitted.
///
/// Registered directly inside [`SanitizeRequestMiddleware`] and ahead of
/// [`AuthMiddleware`] so that every outgoing response — including auth-rejected
/// ones — carries a consistent set of headers.
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
        let geo_available = ctx
            .request()
            .headers()
            .get("cf-ipcountry")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty() && *s != "XX")
            .is_some();

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
/// `geo_available` controls `X-Geo-Info-Available`; pass `true` when
/// `cf-ipcountry` was present and non-`XX` in the incoming request.
/// Operator-configured `settings.response_headers` are applied last (with the
/// shared cookie cache-privacy hardening) and can override any managed header.
pub(crate) fn apply_finalize_headers(
    settings: &Settings,
    geo_available: bool,
    response: &mut Response,
) {
    response.headers_mut().insert(
        HEADER_X_GEO_INFO_AVAILABLE,
        HeaderValue::from_static(if geo_available { "true" } else { "false" }),
    );

    // Cloudflare is a real shared cache: cookie-bearing responses must stay
    // private and operator headers must not re-enable caching for uncacheable
    // per-user payloads.
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

    use std::collections::HashMap;
    use std::sync::Mutex;

    use edgezero_core::body::Body;
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::{Method, request_builder, response_builder};
    use edgezero_core::middleware::Next;
    use edgezero_core::params::PathParams;
    use futures::executor::block_on;
    use trusted_server_core::redacted::Redacted;
    use trusted_server_core::settings::TrustedClientIpConfig;

    fn empty_response() -> Response {
        response_builder()
            .body(Body::empty())
            .expect("should build empty test response")
    }

    fn empty_ctx() -> RequestContext {
        let req = request_builder()
            .method(Method::GET)
            .uri("/test")
            .header("x-reader-ip", "198.51.100.7")
            .header("x-reader-ip-auth", "fictional-shared-secret-0123456789")
            .body(Body::empty())
            .expect("should build test request");
        RequestContext::new(req, PathParams::new(HashMap::new()))
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
    fn sets_geo_available_false_when_no_country_header() {
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
    fn sets_geo_available_true_when_country_header_present() {
        let settings = settings_with_response_headers(vec![]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, true, &mut response);

        assert_eq!(
            response
                .headers()
                .get("x-geo-info-available")
                .and_then(|v| v.to_str().ok()),
            Some("true"),
            "should set X-Geo-Info-Available: true when cf-ipcountry is present"
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

    #[test]
    fn sanitize_middleware_strips_configured_trust_headers_before_routing() {
        let mut settings = settings_with_response_headers(vec![]);
        settings.trusted_client_ip = Some(TrustedClientIpConfig {
            ip_header: "x-reader-ip".to_owned(),
            auth_header: "x-reader-ip-auth".to_owned(),
            shared_secret: Redacted::new("fictional-shared-secret-0123456789".to_owned()),
        });
        let middleware = SanitizeRequestMiddleware::new(Arc::new(settings));
        let observed = Arc::new(Mutex::new(None));
        let handler_observed = Arc::clone(&observed);
        let handler = Arc::new(move |ctx: RequestContext| {
            let handler_observed = Arc::clone(&handler_observed);
            async move {
                *handler_observed.lock().expect("should lock observation") = Some((
                    ctx.request().headers().contains_key("x-reader-ip"),
                    ctx.request().headers().contains_key("x-reader-ip-auth"),
                ));
                Ok::<Response, EdgeError>(empty_response())
            }
        });

        block_on(middleware.handle(empty_ctx(), Next::new(&[], &*handler)))
            .expect("should run middleware");

        assert_eq!(
            *observed.lock().expect("should lock observation"),
            Some((false, false)),
            "should remove both configured trust headers before the handler"
        );
    }
}
