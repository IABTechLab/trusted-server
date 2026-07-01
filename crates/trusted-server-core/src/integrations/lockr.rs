//! Lockr integration for identity resolution and advertising tokens.
//!
//! This module provides transparent proxying for Lockr's SDK and API,
//! enabling first-party identity resolution while maintaining privacy controls.
//!
//! Lockr provides a dedicated trust-server SDK (`identity-lockr-trust-server.js`)
//! that is pre-configured to route API calls through the first-party proxy,
//! so no runtime rewriting of the SDK JavaScript is needed.

use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::{self, HeaderMap, HeaderValue};
use http::{Method, StatusCode};
use serde::Deserialize;
use validator::Validate;

use crate::constants::INTERNAL_HEADERS;
use crate::error::TrustedServerError;
use crate::integrations::{
    collect_body_bounded, collect_response_bounded, ensure_integration_backend,
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration, INTEGRATION_MAX_BODY_BYTES,
    UPSTREAM_SDK_MAX_RESPONSE_BYTES,
};
use crate::platform::{PlatformHttpRequest, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

const LOCKR_INTEGRATION_ID: &str = "lockr";

/// Configuration for Lockr integration.
#[derive(Debug, Deserialize, Validate)]
pub struct LockrConfig {
    /// Enable/disable the integration
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Lockr app ID (from meta tag lockr-signin-app_id)
    #[validate(length(min = 1))]
    pub app_id: String,

    /// Base URL for Lockr API (default: <https://identity.loc.kr>)
    #[serde(default = "default_api_endpoint")]
    #[validate(url)]
    pub api_endpoint: String,

    /// SDK URL (default: <https://aim.loc.kr/identity-lockr-trust-server.js>)
    #[serde(default = "default_sdk_url")]
    #[validate(url)]
    pub sdk_url: String,

    /// Cache TTL for Lockr SDK in seconds (default: 3600 = 1 hour)
    #[serde(default = "default_cache_ttl")]
    #[validate(range(min = 60, max = 86400))]
    pub cache_ttl_seconds: u32,

    /// Whether to rewrite Lockr SDK URLs in HTML
    #[serde(default = "default_rewrite_sdk")]
    pub rewrite_sdk: bool,

    /// Deprecated — the trust-server SDK handles host routing natively.
    /// Kept for backwards compatibility so existing configs don't cause parse errors.
    #[serde(default)]
    pub rewrite_sdk_host: Option<bool>,

    /// Override the Origin header sent to Lockr API.
    /// Use this when running locally or from a domain not registered with Lockr.
    /// Example: "<https://www.example.com>"
    #[serde(default)]
    #[validate(url)]
    pub origin_override: Option<String>,
}

impl IntegrationConfig for LockrConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Lockr integration implementation.
pub struct LockrIntegration {
    config: LockrConfig,
}

impl LockrIntegration {
    fn new(config: LockrConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: LOCKR_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    /// Check if a URL is a Lockr SDK URL.
    fn is_lockr_sdk_url(&self, url: &str) -> bool {
        let lower = url.to_ascii_lowercase();
        (lower.contains("aim.loc.kr") || lower.contains("identity.loc.kr"))
            && lower.contains("identity-lockr")
            && lower.ends_with(".js")
    }

    /// Handle SDK serving — fetch from Lockr CDN and serve through first-party domain.
    async fn handle_sdk_serving(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let sdk_url = &self.config.sdk_url;
        log::info!("Fetching Lockr SDK from {}", sdk_url);

        // TODO: Check KV store cache first (future enhancement)

        let lockr_req = http::Request::builder()
            .method(Method::GET)
            .uri(sdk_url)
            .header(header::USER_AGENT, "TrustedServer/1.0")
            .header(header::ACCEPT, "application/javascript, */*")
            .body(EdgeBody::empty())
            .change_context(Self::error("Failed to build Lockr SDK request"))?;

        let backend_name = Self::backend_name_for_url(services, sdk_url)
            .change_context(Self::error("Failed to determine backend for SDK fetch"))?;

        let lockr_response = services
            .http_client()
            .send(PlatformHttpRequest::new(lockr_req, backend_name))
            .await
            .change_context(Self::error(format!(
                "Failed to fetch Lockr SDK from {}",
                sdk_url
            )))?
            .response;

        if !lockr_response.status().is_success() {
            log::error!(
                "Lockr SDK fetch failed with status {}",
                lockr_response.status()
            );
            return Err(Report::new(Self::error(format!(
                "Lockr SDK returned error status: {}",
                lockr_response.status()
            ))));
        }

        let sdk_body = collect_response_bounded(
            lockr_response.into_body(),
            UPSTREAM_SDK_MAX_RESPONSE_BYTES,
            LOCKR_INTEGRATION_ID,
        )
        .await
        .change_context(Self::error("Failed to read Lockr SDK response body"))?;
        log::info!("Fetched Lockr SDK ({} bytes)", sdk_body.len());

        // TODO: Cache in KV store (future enhancement)

        http::Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .header(
                header::CACHE_CONTROL,
                format!("public, max-age={}", self.config.cache_ttl_seconds),
            )
            .header("X-Lockr-SDK-Proxy", "true")
            .header("X-Lockr-SDK-Mode", "trust-server")
            .header("X-SDK-Source", sdk_url)
            .body(EdgeBody::from(sdk_body))
            .change_context(Self::error("Failed to build Lockr SDK response"))
    }

    /// Handle API proxy — forward requests to the configured Lockr API endpoint.
    async fn handle_api_proxy(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let (parts, body) = req.into_parts();
        let original_path = parts.uri.path().to_string();
        let method = parts.method.clone();

        log::info!("Proxying Lockr API request: {} {}", method, original_path);

        // Extract path after /integrations/lockr/api and pass through directly.
        // This allows the Lockr SDK to use any API endpoint without hardcoded mappings.
        let target_path = original_path
            .strip_prefix("/integrations/lockr/api")
            .ok_or_else(|| Self::error(format!("Invalid Lockr API path: {}", original_path)))?;

        let query = parts
            .uri
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("{}{}{}", self.config.api_endpoint, target_path, query);

        log::info!("Forwarding to Lockr API: {}", target_url);

        let request_body = if method == Method::POST {
            let bytes =
                collect_body_bounded(body, INTEGRATION_MAX_BODY_BYTES, LOCKR_INTEGRATION_ID)
                    .await?;
            EdgeBody::from(bytes)
        } else {
            EdgeBody::empty()
        };

        let mut target_req = http::Request::builder()
            .method(method.clone())
            .uri(&target_url)
            .body(request_body)
            .change_context(Self::error("Failed to build Lockr API proxy request"))?;
        self.copy_request_headers(&parts.headers, target_req.headers_mut())?;

        let backend_name = Self::backend_name_for_url(services, &self.config.api_endpoint)
            .change_context(Self::error("Failed to determine backend for API proxy"))?;

        let response = services
            .http_client()
            .send(PlatformHttpRequest::new(target_req, backend_name))
            .await
            .change_context(Self::error(format!(
                "Failed to forward request to {}",
                target_url
            )))?
            .response;

        log::info!("Lockr API responded with status {}", response.status());

        Ok(response)
    }

    /// Copy relevant request headers for proxying.
    ///
    /// Consent cookies are always stripped — consent signals are forwarded
    /// through the `OpenRTB` body by the Prebid integration, not through
    /// Lockr's cookie-based API calls.
    fn copy_request_headers(
        &self,
        from: &HeaderMap<HeaderValue>,
        to: &mut HeaderMap<HeaderValue>,
    ) -> Result<(), Report<TrustedServerError>> {
        // NOTE: `Authorization` and `Cookie` are intentionally NOT forwarded.
        // Under the first-party proxy the browser attaches the publisher's own
        // credentials to `/integrations/lockr/api/...` — `Authorization` (e.g.
        // staging basic-auth) and every publisher session/auth cookie. Both
        // would leak to the third-party upstream, and the Lockr API rejects an
        // unexpected `Authorization` with `{"code":400,"message":"Invalid
        // request"}`. The SDK already passes the identity cookie data it needs
        // in the request body (`firstPartyCookies`), so no `Cookie` header is
        // required upstream.
        let headers_to_copy = [
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::USER_AGENT,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
        ];

        for header_name in &headers_to_copy {
            if let Some(value) = from.get(header_name) {
                to.insert(header_name, value.clone());
            }
        }

        // Use origin override if configured, otherwise forward original
        let origin = self.config.origin_override.as_deref().or_else(|| {
            from.get(header::ORIGIN)
                .and_then(|value| value.to_str().ok())
        });
        if let Some(origin) = origin {
            match HeaderValue::from_str(origin) {
                Ok(value) => {
                    to.insert(header::ORIGIN, value);
                }
                Err(error) => {
                    log::warn!("Skipping invalid Lockr origin header value '{origin}': {error}");
                }
            }
        }

        for (name, value) in from {
            let name_str = name.as_str();
            if name_str.starts_with("x-") && !INTERNAL_HEADERS.contains(&name_str) {
                to.append(name.clone(), value.clone());
            }
        }

        Ok(())
    }

    fn backend_name_for_url(
        services: &RuntimeServices,
        target_url: &str,
    ) -> Result<String, Report<TrustedServerError>> {
        ensure_integration_backend(services, target_url, LOCKR_INTEGRATION_ID, None)
    }
}

fn build(settings: &Settings) -> Result<Option<Arc<LockrIntegration>>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<LockrConfig>(LOCKR_INTEGRATION_ID)? else {
        return Ok(None);
    };

    Ok(Some(LockrIntegration::new(config)))
}

/// Register the Lockr integration.
///
/// # Errors
///
/// Returns an error when the Lockr integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    if integration.config.rewrite_sdk_host.is_some() {
        log::warn!(
            "lockr: `rewrite_sdk_host` is deprecated and ignored; \
             the trust-server SDK handles host routing natively"
        );
    }
    log::info!(
        "Registering Lockr integration (rewrite_sdk={})",
        integration.config.rewrite_sdk
    );

    Ok(Some(
        IntegrationRegistration::builder(LOCKR_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for LockrIntegration {
    fn integration_name(&self) -> &'static str {
        LOCKR_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get("/sdk"), self.post("/api/*"), self.get("/api/*")]
    }

    async fn handle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let path = req.uri().path().to_string();

        if path == "/integrations/lockr/sdk" {
            self.handle_sdk_serving(settings, services).await
        } else if path.starts_with("/integrations/lockr/api/") {
            self.handle_api_proxy(settings, services, req).await
        } else {
            Err(Report::new(Self::error(format!(
                "Unknown Lockr route: {}",
                path
            ))))
        }
    }
}

impl IntegrationAttributeRewriter for LockrIntegration {
    fn integration_id(&self) -> &'static str {
        LOCKR_INTEGRATION_ID
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        self.config.rewrite_sdk && matches!(attribute, "src" | "href")
    }

    fn rewrite(
        &self,
        _attr_name: &str,
        attr_value: &str,
        _ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if !self.config.rewrite_sdk {
            return AttributeRewriteAction::Keep;
        }

        if self.is_lockr_sdk_url(attr_value) {
            // Root-relative so the browser resolves it against the page host.
            // Note: a page-level `<base href>` participates in this resolution,
            // so on pages that set an external base URL these resolve against
            // that base rather than the address-bar origin — an accepted
            // tradeoff, matching GTM/Didomi/Testlight which are also relative.
            let replacement = "/integrations/lockr/sdk".to_string();
            log::debug!("Rewriting Lockr SDK URL to {}", replacement);
            AttributeRewriteAction::Replace(replacement)
        } else {
            AttributeRewriteAction::Keep
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_api_endpoint() -> String {
    "https://identity.loc.kr".to_string()
}

fn default_sdk_url() -> String {
    "https://aim.loc.kr/identity-lockr-trust-server.js".to_string()
}

fn default_cache_ttl() -> u32 {
    3600
}

fn default_rewrite_sdk() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use edgezero_core::http::Method as HttpMethod;
    use serde_json::json;

    use crate::platform::test_support::{build_services_with_http_client, StubHttpClient};
    use crate::test_support::tests::create_test_settings;

    fn test_config() -> LockrConfig {
        LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            rewrite_sdk_host: None,
            origin_override: None,
        }
    }

    fn test_context() -> IntegrationAttributeContext<'static> {
        IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        }
    }

    #[test]
    fn test_lockr_sdk_url_detection() {
        let integration = LockrIntegration::new(test_config());

        // Should match Lockr SDK URLs
        assert!(integration.is_lockr_sdk_url("https://aim.loc.kr/identity-lockr-v1.0.js"));
        assert!(integration.is_lockr_sdk_url("https://aim.loc.kr/identity-lockr-trust-server.js"));
        assert!(integration.is_lockr_sdk_url("https://identity.loc.kr/identity-lockr-v2.0.js"));

        // Should not match non-SDK resources on Lockr domains
        assert!(
            !integration.is_lockr_sdk_url("https://aim.loc.kr/pixel.gif"),
            "should not match non-JS assets on aim.loc.kr"
        );
        assert!(
            !integration.is_lockr_sdk_url("https://aim.loc.kr/styles.css"),
            "should not match CSS files on aim.loc.kr"
        );
        assert!(
            !integration.is_lockr_sdk_url("https://identity.loc.kr/some-other-script.js"),
            "should not match non-SDK JS files on identity.loc.kr"
        );

        // Should not match other URLs
        assert!(
            !integration.is_lockr_sdk_url("https://example.com/script.js"),
            "should not match unrelated domains"
        );
    }

    #[test]
    fn test_default_sdk_url_uses_trust_server() {
        let url = default_sdk_url();
        assert!(
            url.contains("trust-server"),
            "should use the trust-server SDK variant by default"
        );
    }

    #[test]
    fn test_attribute_rewriter_rewrites_sdk_urls() {
        let integration = LockrIntegration::new(test_config());
        let ctx = test_context();

        let result = integration.rewrite("src", "https://aim.loc.kr/identity-lockr-v1.0.js", &ctx);

        assert_eq!(
            result,
            AttributeRewriteAction::Replace("/integrations/lockr/sdk".to_string()),
            "should rewrite Lockr SDK URL to root-relative first-party proxy"
        );
    }

    #[test]
    fn test_attribute_rewriter_keeps_non_lockr_urls() {
        let integration = LockrIntegration::new(test_config());
        let ctx = test_context();

        let result = integration.rewrite("src", "https://example.com/other.js", &ctx);

        assert_eq!(
            result,
            AttributeRewriteAction::Keep,
            "should keep non-Lockr URLs unchanged"
        );
    }

    #[test]
    fn test_attribute_rewriter_noop_when_disabled() {
        let config = LockrConfig {
            rewrite_sdk: false,
            ..test_config()
        };
        let integration = LockrIntegration::new(config);
        let ctx = test_context();

        let result = integration.rewrite("src", "https://aim.loc.kr/identity-lockr-v1.0.js", &ctx);

        assert_eq!(
            result,
            AttributeRewriteAction::Keep,
            "should keep all URLs when rewrite_sdk is disabled"
        );
    }

    #[test]
    fn lockr_proxy_uses_platform_http_client() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"ok".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = LockrIntegration::new(test_config());
        let req = http::Request::builder()
            .method(HttpMethod::GET)
            .uri("https://publisher.example/integrations/lockr/api/publisher/app/v1/identityLockr/settings")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response = futures::executor::block_on(integration.handle(&settings, &services, req))
            .expect("should proxy request");

        assert_eq!(
            response.status(),
            http::StatusCode::OK,
            "should return stubbed response"
        );
        assert_eq!(
            stub.recorded_backend_names().len(),
            1,
            "should route one outbound request through PlatformHttpClient"
        );
    }

    #[test]
    fn lockr_proxy_forwards_body_and_strips_publisher_credentials() {
        // Regression guard for the upstream-rejection / credential-leak causes:
        // 1. The POST body (and content-type) must be forwarded, otherwise the
        //    Lockr API returns `{"code":400,"message":"Invalid request"}`.
        // 2. The publisher's `Authorization` header (e.g. site basic-auth) must
        //    NOT be forwarded — the Lockr API rejects it with the same 400, and
        //    forwarding it would leak the publisher credential to a third party.
        // 3. The publisher's `Cookie` header (session/auth cookies the browser
        //    attaches to the first-party route) must NOT be forwarded either.
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, br#"{"success":true,"data":{}}"#.to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = LockrIntegration::new(test_config());

        let payload = br#"{"appID":"test-app-id"}"#;
        let req = http::Request::builder()
            .method(HttpMethod::POST)
            .uri("https://publisher.example/integrations/lockr/api/publisher/app/v2/identityLockr/settings")
            .header(header::CONTENT_TYPE, "application/json;charset=UTF-8")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .header(header::COOKIE, "session_id=secret; euconsent-v2=tcf")
            .body(EdgeBody::from(payload.to_vec()))
            .expect("should build request");

        let response = futures::executor::block_on(integration.handle(&settings, &services, req))
            .expect("should proxy request");
        assert_eq!(response.status(), http::StatusCode::OK, "should return OK");

        let bodies = stub.recorded_request_bodies();
        assert_eq!(
            bodies.len(),
            1,
            "should forward exactly one upstream request"
        );
        assert_eq!(
            bodies[0], payload,
            "should forward the POST body unchanged to the Lockr API"
        );

        let headers = stub.recorded_request_headers();
        assert!(
            headers[0]
                .iter()
                .any(|(name, value)| name == "content-type"
                    && value == "application/json;charset=UTF-8"),
            "should forward the content-type header to the Lockr API"
        );
        assert!(
            !headers[0]
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("authorization")),
            "should NOT forward the publisher's Authorization header to the Lockr API"
        );
        assert!(
            !headers[0]
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cookie")),
            "should NOT forward the publisher's Cookie header to the Lockr API"
        );
    }

    #[test]
    fn test_api_path_extraction_preserves_casing() {
        let test_cases = [
            (
                "/integrations/lockr/api/publisher/app/v1/identityLockr/settings",
                "/publisher/app/v1/identityLockr/settings",
            ),
            (
                "/integrations/lockr/api/publisher/app/v1/identityLockr/page-view",
                "/publisher/app/v1/identityLockr/page-view",
            ),
            (
                "/integrations/lockr/api/publisher/app/v1/identityLockr/generate-tokens",
                "/publisher/app/v1/identityLockr/generate-tokens",
            ),
        ];

        for (input, expected) in test_cases {
            let result = input
                .strip_prefix("/integrations/lockr/api")
                .expect("should strip prefix");
            assert_eq!(
                result, expected,
                "should preserve casing for path: {}",
                input
            );
        }
    }

    #[test]
    fn test_routes_registered() {
        let integration = LockrIntegration::new(test_config());
        let routes = integration.routes();

        assert_eq!(routes.len(), 3, "should register 3 routes");

        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/lockr/sdk" && r.method == Method::GET),
            "should register SDK GET route"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/lockr/api/*" && r.method == Method::POST),
            "should register API POST route"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/lockr/api/*" && r.method == Method::GET),
            "should register API GET route"
        );
    }

    #[test]
    fn disabled_invalid_config_does_not_error() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                LOCKR_INTEGRATION_ID,
                &json!({
                    "enabled": false,
                    "app_id": "",
                    "sdk_url": "not a url",
                }),
            )
            .expect("should insert disabled invalid Lockr config");

        let registration = register(&settings).expect("disabled invalid Lockr config should skip");
        assert!(
            registration.is_none(),
            "disabled invalid Lockr config should not register"
        );
    }
}
