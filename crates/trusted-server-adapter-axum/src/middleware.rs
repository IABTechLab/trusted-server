use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Response};
use edgezero_core::middleware::{Middleware, Next};
use trusted_server_core::constants::HEADER_X_GEO_INFO_AVAILABLE;
use trusted_server_core::http_util::{
    ROUTE_NAMESPACE, sanitize_trusted_client_ip_headers, strip_deployment_headers,
};
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
/// Geo lookup is unavailable in the Axum dev server — `X-Geo-Info-Available: false`
/// is always emitted. The Fastly adapter's `X-TS-ENV` is skipped because its
/// env var is not set in a local dev context.
///
/// Registered directly inside [`SanitizeRequestMiddleware`], so that every
/// outgoing response carries a consistent set of headers.
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
        let path = ctx.request().uri().path().to_owned();
        let mut response = next.run(ctx).await?;
        // Before finalizing, so a header finalizing writes is kept.
        strip_deployment_headers(&path, ROUTE_NAMESPACE, &mut response);
        apply_finalize_headers(&self.settings, &mut response);
        Ok(response)
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

    // Last, so that nothing an origin or an operator's header set can loosen
    // a site taken out of search.
    trusted_server_core::robots_txt::apply_response_robots_tag(settings, response);
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
        let mut s = Settings::from_toml(
            r#"
                [publisher]
                domain = "test-publisher.example.com"
                cookie_domain = ".test-publisher.example.com"
                origin_url = "https://origin.test-publisher.example.com"
                proxy_secret = "unit-test-proxy-secret"

                [ec]
                module = "hmac"

                [ec.hmac]
                passphrase = "test-secret-key-32-bytes-minimum"

                [geo]
                assume_single_jurisdiction = true
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

    fn ctx_for(path: &str) -> RequestContext {
        let req = request_builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())
            .expect("should build test request");
        RequestContext::new(req, PathParams::new(HashMap::new()))
    }

    /// A response as an origin behind a cache sends it, naming the node that
    /// answered and how the cache treated the request.
    fn response_naming_its_node() -> Response {
        response_builder()
            .header("x-served-by", "cache-node-1")
            .header("x-cache", "HIT")
            .header("x-cache-hits", "3")
            .body(Body::empty())
            .expect("should build test response")
    }

    /// What the finalize middleware makes of that response for a request to
    /// `path`.
    fn finalized(path: &str, settings: Settings) -> Response {
        let middleware = FinalizeResponseMiddleware::new(Arc::new(settings));
        let handler = Arc::new(|_ctx: RequestContext| async move {
            Ok::<Response, EdgeError>(response_naming_its_node())
        });

        block_on(middleware.handle(ctx_for(path), Next::new(&[], &*handler)))
            .expect("should run middleware")
    }

    #[test]
    fn finalize_middleware_strips_deployment_headers_before_it_writes_the_operators() {
        let response = finalized(
            "/articles/an-article",
            settings_with_response_headers(vec![("X-Cache", "configured")]),
        );

        for name in ["x-served-by", "x-cache-hits"] {
            assert!(
                !response.headers().contains_key(name),
                "should strip {name} from a publisher's page"
            );
        }
        assert_eq!(
            response
                .headers()
                .get("x-cache")
                .and_then(|v| v.to_str().ok()),
            Some("configured"),
            "should carry the operator's header and not the origin's"
        );
    }
}
