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
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use serde::Deserialize;
use validator::Validate;

use crate::backend::BackendConfig;
use crate::error::TrustedServerError;
use crate::http_util::copy_custom_headers;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
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
        _req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let sdk_url = &self.config.sdk_url;
        log::info!("Fetching Lockr SDK from {}", sdk_url);

        // TODO: Check KV store cache first (future enhancement)

        let mut lockr_req = Request::new(Method::GET, sdk_url);
        lockr_req.set_header(header::USER_AGENT, "TrustedServer/1.0");
        lockr_req.set_header(header::ACCEPT, "application/javascript, */*");

        let backend_name = BackendConfig::from_url(sdk_url, true)
            .change_context(Self::error("Failed to determine backend for SDK fetch"))?;

        let mut lockr_response =
            lockr_req
                .send(backend_name)
                .change_context(Self::error(format!(
                    "Failed to fetch Lockr SDK from {}",
                    sdk_url
                )))?;

        if !lockr_response.get_status().is_success() {
            log::error!(
                "Lockr SDK fetch failed with status {}",
                lockr_response.get_status()
            );
            return Err(Report::new(Self::error(format!(
                "Lockr SDK returned error status: {}",
                lockr_response.get_status()
            ))));
        }

        let sdk_body = lockr_response.take_body_bytes();
        log::info!("Fetched Lockr SDK ({} bytes)", sdk_body.len());

        // TODO: Cache in KV store (future enhancement)

        Ok(Response::from_status(StatusCode::OK)
            .with_header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .with_header(
                header::CACHE_CONTROL,
                format!("public, max-age={}", self.config.cache_ttl_seconds),
            )
            .with_header("X-Lockr-SDK-Proxy", "true")
            .with_header("X-Lockr-SDK-Mode", "trust-server")
            .with_header("X-SDK-Source", sdk_url)
            .with_body(sdk_body))
    }

    /// Handle API proxy — forward requests to identity.loc.kr.
    async fn handle_api_proxy(
        &self,
        _settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();
        let method = req.get_method();

        log::info!("Proxying Lockr API request: {} {}", method, original_path);

        // Extract path after /integrations/lockr/api and pass through directly.
        // This allows the Lockr SDK to use any API endpoint without hardcoded mappings.
        let target_path = original_path
            .strip_prefix("/integrations/lockr/api")
            .ok_or_else(|| Self::error(format!("Invalid Lockr API path: {}", original_path)))?;

        let query = req
            .get_url()
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("{}{}{}", self.config.api_endpoint, target_path, query);

        log::info!("Forwarding to Lockr API: {}", target_url);

        let mut target_req = Request::new(method.clone(), &target_url);
        self.copy_request_headers(&req, &mut target_req);

        if matches!(method, &Method::POST | &Method::PUT | &Method::PATCH) {
            let body = req.take_body();
            target_req.set_body(body);
        }

        let backend_name = BackendConfig::from_url(&self.config.api_endpoint, true)
            .change_context(Self::error("Failed to determine backend for API proxy"))?;

        let response = match target_req.send(backend_name) {
            Ok(res) => res,
            Err(e) => {
                return Err(Self::error(format!(
                    "failed to forward request to {}, {}",
                    target_url,
                    e.root_cause()
                ))
                .into());
            }
        };

        log::info!("Lockr API responded with status {}", response.get_status());

        Ok(response)
    }

    /// Copy relevant request headers for proxying.
    fn copy_request_headers(&self, from: &Request, to: &mut Request) {
        let headers_to_copy = [
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::USER_AGENT,
            header::AUTHORIZATION,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
            header::COOKIE,
        ];

        for header_name in &headers_to_copy {
            if let Some(value) = from.get_header(header_name) {
                to.set_header(header_name, value);
            }
        }

        // Use origin override if configured, otherwise forward original
        let origin = self
            .config
            .origin_override
            .as_deref()
            .or_else(|| from.get_header_str(header::ORIGIN));
        if let Some(origin) = origin {
            to.set_header(header::ORIGIN, origin);
        }

        copy_custom_headers(from, to);
    }
}

fn build(settings: &Settings) -> Option<Arc<LockrIntegration>> {
    let config = match settings.integration_config::<LockrConfig>(LOCKR_INTEGRATION_ID) {
        Ok(Some(config)) => config,
        Ok(None) => return None,
        Err(err) => {
            log::error!("Failed to load Lockr integration config: {err:?}");
            return None;
        }
    };

    Some(LockrIntegration::new(config))
}

/// Register the Lockr integration.
#[must_use]
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;
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
    Some(
        IntegrationRegistration::builder(LOCKR_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
            .build(),
    )
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
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path();

        if path == "/integrations/lockr/sdk" {
            self.handle_sdk_serving(settings, req).await
        } else if path.starts_with("/integrations/lockr/api/") {
            self.handle_api_proxy(settings, req).await
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
        ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if !self.config.rewrite_sdk {
            log::debug!("[lockr] Rewrite skipped, rewrite_sdk is disabled");
            return AttributeRewriteAction::Keep;
        }

        if self.is_lockr_sdk_url(attr_value) {
            let replacement = format!(
                "{}://{}/integrations/lockr/sdk",
                ctx.request_scheme, ctx.request_host
            );
            log::debug!("[lockr] Rewriting SDK URL to {}", replacement);
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
    use super::*;

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
            AttributeRewriteAction::Replace(
                "https://edge.example.com/integrations/lockr/sdk".to_string()
            ),
            "should rewrite Lockr SDK URL to first-party proxy"
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
}
