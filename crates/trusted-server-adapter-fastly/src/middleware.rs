//! Middleware implementations for the `EdgeZero` entry point.
//!
//! Provides the middleware the `EdgeZero` entry point registers,
//! [`FinalizeResponseMiddleware`], which injects the standard TS headers.

use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HeaderValue, Response};
use edgezero_core::middleware::{Middleware, Next};
use edgezero_core::response::IntoResponse;
use trusted_server_core::constants::{ENV_FASTLY_IS_STAGING, HEADER_X_TS_ENV};
use trusted_server_core::http_util::{ROUTE_NAMESPACE, strip_deployment_headers};
use trusted_server_core::settings::Settings;

pub(crate) const HEADER_X_TS_FINALIZED: &str = "x-ts-finalized";

// ---------------------------------------------------------------------------
// FinalizeResponseMiddleware
// ---------------------------------------------------------------------------

/// Outermost middleware, which injects all standard TS response headers.
///
/// Registered first in the middleware chain so that it wraps the handler. This
/// guarantees every registered-route response carries a consistent set of headers.
///
/// Router-level 405/404 responses for unregistered HTTP methods (e.g. TRACE) bypass the
/// middleware chain. Those are covered by a second call to [`apply_finalize_headers`] at
/// the `lib.rs` entry point. Middleware-finalized responses carry
/// [`HEADER_X_TS_FINALIZED`] so the entry point can skip duplicate finalization.
///
/// # Header precedence
///
/// Headers are written in this order (last write wins):
/// 1. `X-TS-ENV: staging` when `FASTLY_IS_STAGING == "1"`
/// 2. Operator-configured `settings.response_headers` (can override any managed header)
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

        let mut response = match next.run(ctx).await {
            Ok(r) => r,
            Err(e) => {
                log::error!("request handler failed: {e:?}");
                e.into_response()?
            }
        };

        // Before finalizing, so a header finalizing writes is kept.
        strip_deployment_headers(&path, ROUTE_NAMESPACE, &mut response);
        apply_finalize_headers(&self.settings, &mut response);
        response
            .headers_mut()
            .insert(HEADER_X_TS_FINALIZED, HeaderValue::from_static("1"));

        Ok(response)
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
/// 1. `X-TS-ENV: staging` when `FASTLY_IS_STAGING == "1"`
/// 2. Set-Cookie cache privacy, which strips surrogate cache headers and
///    downgrades `Cache-Control` to `private, max-age=0` on cookie-bearing
///    responses
/// 3. `settings.response_headers`, the operator's overrides, except that the
///    cache-controlling headers are skipped on uncacheable (`private`/`no-store`)
///    responses so operators cannot re-enable shared caching for per-user payloads
/// 4. `X-Robots-Tag: noindex, nofollow` when `[robots-txt] modules` selects
///    `refuse_all`
pub(crate) fn apply_finalize_headers(settings: &Settings, response: &mut Response) {
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

    // Last, so that nothing an origin or an operator's header set can loosen
    // a site taken out of search.
    trusted_server_core::robots_txt::apply_response_robots_tag(settings, response);
}

/// Forces cookie-bearing responses to stay private to shared caches.
///
/// Re-exported from [`trusted_server_core::response_privacy`] so the `EdgeZero`
/// entry point (`lib.rs`) can re-apply it after
/// [`ec_finalize_response`](trusted_server_core::ec::finalize::ec_finalize_response)
/// writes the EC identity `Set-Cookie`, using the single shared implementation.
pub(crate) use trusted_server_core::response_privacy::{
    enforce_set_cookie_cache_privacy, enforce_uncacheable_cache_privacy,
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use edgezero_core::body::Body;
    use edgezero_core::context::RequestContext;
    use edgezero_core::error::EdgeError;
    use edgezero_core::http::{HeaderName, Method, request_builder, response_builder};
    use edgezero_core::middleware::Next;
    use edgezero_core::params::PathParams;
    use futures::executor::block_on;
    use trusted_server_core::response_privacy::apply_inactive_ad_stack_browser_cache_policy;

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

    fn test_settings() -> Settings {
        Settings::from_toml(
            r#"
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.com"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [geo]
            assume_single_jurisdiction = true

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            enabled = false
            "#,
        )
        .expect("should parse test settings")
        // A loaded domain carries its policy.
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
    }

    fn settings_with_response_headers(headers: Vec<(&str, &str)>) -> Settings {
        let mut s = test_settings();
        s.response_headers = headers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s
    }

    /// The `x-geo-*` headers on `response`, by name.
    fn geo_header_names(response: &Response) -> Vec<String> {
        response
            .headers()
            .keys()
            .map(HeaderName::as_str)
            .filter(|name| name.starts_with("x-geo-"))
            .map(str::to_owned)
            .collect()
    }

    fn finalize_middleware(settings: Settings) -> FinalizeResponseMiddleware {
        FinalizeResponseMiddleware::new(Arc::new(settings))
    }

    #[test]
    fn operator_response_headers_override_earlier_headers() {
        let settings = settings_with_response_headers(vec![("X-Example", "operator-override")]);
        let mut response = response_with_headers(&[("x-example", "origin")]);

        apply_finalize_headers(&settings, &mut response);

        assert_eq!(
            response
                .headers()
                .get("x-example")
                .and_then(|v| v.to_str().ok()),
            Some("operator-override"),
            "should replace a header already on the response with the operator-configured value"
        );
    }

    #[test]
    fn finalizing_writes_no_geo_header() {
        let settings = settings_with_response_headers(vec![]);
        let mut response = empty_response();

        apply_finalize_headers(&settings, &mut response);

        assert!(
            geo_header_names(&response).is_empty(),
            "should write no x-geo-* header, which a serve middleware writes: {:?}",
            geo_header_names(&response)
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
    fn the_data_page_keeps_its_sandbox_under_an_operator_content_security_policy() {
        let settings = settings_with_response_headers(vec![
            ("Content-Security-Policy", "default-src 'self'"),
            ("X-Frame-Options", "SAMEORIGIN"),
        ]);
        let request = request_builder()
            .method(Method::GET)
            .uri("https://publisher.example/_ts/data")
            .body(Body::empty())
            .expect("should build request");
        let mut response = trusted_server_core::inspect::data::handle_data(
            None,
            None,
            &trusted_server_core::prefix::PublicPrefix::default(),
            &request,
        )
        .expect("should answer");

        apply_finalize_headers(&settings, &mut response);

        let policy = response
            .headers()
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .expect("should carry a policy");
        assert!(
            policy.starts_with("sandbox;"),
            "an operator policy must not remove the sandbox: {policy}"
        );
        assert_eq!(
            response
                .headers()
                .get("x-frame-options")
                .and_then(|v| v.to_str().ok()),
            Some("DENY")
        );
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

        apply_finalize_headers(&settings, &mut response);

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

        apply_finalize_headers(&settings, &mut response);

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
    fn enforce_set_cookie_cache_privacy_downgrades_late_cookie_policies() {
        // Mirrors the EdgeZero post-ec_finalize guard: a Set-Cookie added after
        // finalize headers ran must override both origin-public and inactive
        // template cache policies.
        for (cache_control, generated_inactive_policy) in [
            ("public, max-age=600", false),
            ("private, max-age=60", true),
        ] {
            let mut response = response_with_headers(&[
                ("set-cookie", "ts-ec=abc; Path=/"),
                ("cache-control", cache_control),
                ("surrogate-control", "max-age=600"),
            ]);
            if generated_inactive_policy {
                apply_inactive_ad_stack_browser_cache_policy(&mut response);
            }

            enforce_set_cookie_cache_privacy(&mut response);

            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok()),
                Some("private, max-age=0"),
                "should downgrade {cache_control} on a cookie response"
            );
            assert!(
                response.headers().get("surrogate-control").is_none(),
                "should strip surrogate-control from a {cache_control} cookie response"
            );
        }
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

    #[test]
    fn enforce_uncacheable_cache_privacy_handles_late_filter_headers() {
        let mut response = response_with_headers(&[
            ("cache-control", "private, max-age=0"),
            ("surrogate-control", "max-age=600"),
        ]);

        enforce_uncacheable_cache_privacy(&mut response);

        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("private, max-age=0"),
            "should preserve the late private directive"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "should strip the normalized edge header after late filter effects"
        );
    }

    // ---------------------------------------------------------------------------
    // FinalizeResponseMiddleware::handle tests
    // ---------------------------------------------------------------------------

    #[test]
    fn finalize_handle_writes_no_geo_header() {
        let middleware = finalize_middleware(settings_with_response_headers(vec![]));
        let handler =
            Arc::new(
                |_ctx: RequestContext| async move { Ok::<Response, EdgeError>(empty_response()) },
            );

        let response = block_on(middleware.handle(empty_ctx(), Next::new(&[], &*handler)))
            .expect("should succeed");

        assert!(
            geo_header_names(&response).is_empty(),
            "should write no x-geo-* header, which a serve middleware writes: {:?}",
            geo_header_names(&response)
        );
    }

    #[test]
    fn finalize_handle_marks_response_as_finalized() {
        let middleware = finalize_middleware(settings_with_response_headers(vec![]));
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
        let middleware =
            finalize_middleware(settings_with_response_headers(vec![("X-Operator", "kept")]));
        let handler = Arc::new(|_ctx: RequestContext| async move {
            Err::<Response, EdgeError>(EdgeError::service_unavailable("test error"))
        });

        let response = block_on(middleware.handle(empty_ctx(), Next::new(&[], &*handler)))
            .expect("should absorb handler error into a response");

        assert!(
            response.status().is_server_error(),
            "should produce a server-error status for absorbed handler error"
        );
        assert_eq!(
            response
                .headers()
                .get("x-operator")
                .and_then(|v| v.to_str().ok()),
            Some("kept"),
            "absorbed error response should still carry the operator's headers"
        );
    }

    #[test]
    fn finalize_handle_preserves_duplicate_set_cookie_headers() {
        // Regression guard: FinalizeResponseMiddleware must not drop duplicate
        // Set-Cookie headers. The old dispatch_with_config_handle path silently
        // collapsed them because fastly::Response uses set_header (last-wins).
        // This test verifies the EdgeZero middleware chain is header-transparent.
        let middleware = finalize_middleware(settings_with_response_headers(vec![]));
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
        let middleware = finalize_middleware(settings);
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
