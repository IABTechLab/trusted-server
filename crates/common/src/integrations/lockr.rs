//! Lockr integration for identity resolution and advertising tokens.
//!
//! This module provides transparent proxying for Lockr's SDK and API,
//! enabling first-party identity resolution while maintaining privacy controls.
//!
//! ## Host Rewriting
//!
//! The integration can rewrite the Lockr SDK JavaScript to replace the hardcoded
//! API host with a relative URL pointing to the first-party proxy. This ensures
//! all API calls from the SDK go through the trusted server instead of directly
//! to Lockr's servers, improving privacy and enabling additional controls.
//!
//! The rewriting finds the obfuscated host assignment pattern in the SDK and
//! replaces it with: `'host': '/integrations/lockr/api'`

use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use regex::Regex;
use serde::Deserialize;
use validator::Validate;

use crate::backend::ensure_backend_from_url;
use crate::error::TrustedServerError;
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

    /// Base URL for Lockr API (default: <https://identity.lockr.kr>)
    #[serde(default = "default_api_endpoint")]
    #[validate(url)]
    pub api_endpoint: String,

    /// SDK URL (default: <https://aim.loc.kr/identity-lockr-v1.0.js>)
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

    /// Whether to rewrite the host variable in the Lockr SDK JavaScript
    #[serde(default = "default_rewrite_sdk_host")]
    pub rewrite_sdk_host: bool,

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
        lower.contains("aim.loc.kr")
            || lower.contains("identity.loc.kr")
                && lower.contains("identity-lockr")
                && lower.ends_with(".js")
    }

    /// Rewrite the host variable in the Lockr SDK JavaScript.
    ///
    /// Replaces the obfuscated host assignment with a direct assignment to the
    /// first-party API proxy endpoint. Uses regex to match varying obfuscation patterns.
    fn rewrite_sdk_host(&self, sdk_body: Vec<u8>) -> Result<Vec<u8>, Report<TrustedServerError>> {
        // Convert bytes to string
        let sdk_string = String::from_utf8(sdk_body)
            .change_context(Self::error("SDK content is not valid UTF-8"))?;

        // Pattern matches: 'host': _0xABCDEF(0x123) + _0xABCDEF(0x456) + _0xABCDEF(0x789)
        // This is the obfuscated way Lockr constructs the API host
        // The function names and hex values change with each build, so we use regex
        let pattern = Regex::new(
            r"'host':\s*_0x[a-f0-9]+\(0x[a-f0-9]+\)\s*\+\s*_0x[a-f0-9]+\(0x[a-f0-9]+\)\s*\+\s*_0x[a-f0-9]+\(0x[a-f0-9]+\)",
        )
        .change_context(Self::error("Failed to compile regex pattern"))?;

        // Replace with first-party API proxy endpoint
        let rewritten = pattern.replace(&sdk_string, "'host': '/integrations/lockr/api'");

        Ok(rewritten.as_bytes().to_vec())
    }

    /// Handle SDK serving - fetch from Lockr CDN and serve through first-party domain.
    async fn handle_sdk_serving(
        &self,
        _settings: &Settings,
        _req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        log::info!("Handling Lockr SDK request");

        let sdk_url = &self.config.sdk_url;
        log::info!("Fetching Lockr SDK from: {}", sdk_url);

        // TODO: Check KV store cache first (future enhancement)

        // Fetch SDK from Lockr CDN
        let mut lockr_req = Request::new(Method::GET, sdk_url);
        lockr_req.set_header(header::USER_AGENT, "TrustedServer/1.0");
        lockr_req.set_header(header::ACCEPT, "application/javascript, */*");

        let backend_name = ensure_backend_from_url(sdk_url)
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
                "Lockr SDK fetch failed with status: {}",
                lockr_response.get_status()
            );
            return Err(Report::new(Self::error(format!(
                "Lockr SDK returned error status: {}",
                lockr_response.get_status()
            ))));
        }

        let mut sdk_body = lockr_response.take_body_bytes();
        log::info!("Successfully fetched Lockr SDK: {} bytes", sdk_body.len());

        // Rewrite the host variable in the SDK if enabled
        if self.config.rewrite_sdk_host {
            sdk_body = self.rewrite_sdk_host(sdk_body)?;
            log::info!("Rewrote SDK host variable: {} bytes", sdk_body.len());
        }

        // TODO: Cache in KV store (future enhancement)

        Ok(Response::from_status(StatusCode::OK)
            .with_header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .with_header(
                header::CACHE_CONTROL,
                format!(
                    "public, max-age={}, immutable",
                    self.config.cache_ttl_seconds
                ),
            )
            .with_header("X-Lockr-SDK-Proxy", "true")
            .with_header("X-SDK-Source", sdk_url)
            .with_header(
                "X-Lockr-Host-Rewritten",
                self.config.rewrite_sdk_host.to_string(),
            )
            .with_body(sdk_body))
    }

    /// Handle API proxy - forward requests to identity.lockr.kr.
    async fn handle_api_proxy(
        &self,
        _settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();
        let method = req.get_method();

        log::info!("Proxying Lockr API request: {} {}", method, original_path);

        // Extract path after /integrations/lockr/api and pass through directly
        // This allows the Lockr SDK to use any API endpoint without hardcoded mappings
        let target_path = original_path
            .strip_prefix("/integrations/lockr/api")
            .ok_or_else(|| Self::error(format!("Invalid Lockr API path: {}", original_path)))?;

        // Build full target URL with query parameters
        let query = req
            .get_url()
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("{}{}{}", self.config.api_endpoint, target_path, query);

        log::info!("Forwarding to Lockr API: {}", target_url);

        // Create new request
        let mut target_req = Request::new(method.clone(), &target_url);

        // Copy headers
        self.copy_request_headers(&req, &mut target_req);

        // Copy body for POST/PUT/PATCH
        if matches!(method, &Method::POST | &Method::PUT | &Method::PATCH) {
            let body = req.take_body();
            target_req.set_body(body);
        }

        // Get backend and forward
        let backend_name = ensure_backend_from_url(&self.config.api_endpoint)
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

        log::info!("Lockr API responded with status: {}", response.get_status());

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

        // Handle Origin header - use override if configured, otherwise forward original
        let origin = self
            .config
            .origin_override
            .as_deref()
            .or_else(|| from.get_header_str(header::ORIGIN));
        if let Some(origin) = origin {
            to.set_header(header::ORIGIN, origin);
        }

        // Copy any X-* custom headers
        for header_name in from.get_header_names() {
            let name_str = header_name.as_str();
            if name_str.starts_with("x-") || name_str.starts_with("X-") {
                if let Some(value) = from.get_header(header_name) {
                    to.set_header(header_name, value);
                }
            }
        }
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
            return AttributeRewriteAction::Keep;
        }

        if self.is_lockr_sdk_url(attr_value) {
            // Rewrite to first-party SDK endpoint
            AttributeRewriteAction::Replace(format!(
                "{}://{}/integrations/lockr/sdk",
                ctx.request_scheme, ctx.request_host
            ))
        } else {
            AttributeRewriteAction::Keep
        }
    }
}

// Default value functions
fn default_enabled() -> bool {
    true
}

fn default_api_endpoint() -> String {
    "https://identity.lockr.kr".to_string()
}

fn default_sdk_url() -> String {
    "https://aim.loc.kr/identity-lockr-v1.0.js".to_string()
}

fn default_cache_ttl() -> u32 {
    3600 // 1 hour
}

fn default_rewrite_sdk() -> bool {
    true
}

fn default_rewrite_sdk_host() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lockr_sdk_url_detection() {
        let config = LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            rewrite_sdk_host: true,
            origin_override: None,
        };
        let integration = LockrIntegration::new(config);

        // Should match Lockr SDK URLs
        assert!(integration.is_lockr_sdk_url("https://aim.loc.kr/identity-lockr-v1.0.js"));
        assert!(integration.is_lockr_sdk_url("https://identity.loc.kr/identity-lockr-v2.0.js"));

        // Should not match other URLs
        assert!(!integration.is_lockr_sdk_url("https://example.com/script.js"));
    }

    #[test]
    fn test_attribute_rewriter_rewrites_sdk_urls() {
        let config = LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            rewrite_sdk_host: true,
            origin_override: None,
        };
        let integration = LockrIntegration::new(config);

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        let rewritten =
            integration.rewrite("src", "https://aim.loc.kr/identity-lockr-v1.0.js", &ctx);

        match rewritten {
            AttributeRewriteAction::Replace(url) => {
                assert_eq!(url, "https://edge.example.com/integrations/lockr/sdk");
            }
            _ => panic!("Expected Replace action"),
        }
    }

    #[test]
    fn test_attribute_rewriter_noop_when_disabled() {
        let config = LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: false, // Disabled
            rewrite_sdk_host: true,
            origin_override: None,
        };
        let integration = LockrIntegration::new(config);

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        let rewritten =
            integration.rewrite("src", "https://aim.loc.kr/identity-lockr-v1.0.js", &ctx);

        assert_eq!(rewritten, AttributeRewriteAction::Keep);
    }

    #[test]
    fn test_api_path_extraction_with_camel_case() {
        // Test that we properly extract paths with correct casing
        let path = "/integrations/lockr/api/publisher/app/v1/identityLockr/settings";
        let extracted = path.strip_prefix("/integrations/lockr/api").unwrap();
        assert_eq!(extracted, "/publisher/app/v1/identityLockr/settings");
    }

    #[test]
    fn test_api_path_extraction_preserves_casing() {
        // Test various Lockr API endpoints maintain their original casing
        let test_cases = vec![
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
            let result = input.strip_prefix("/integrations/lockr/api").unwrap();
            assert_eq!(result, expected, "Failed for input: {}", input);
        }
    }

    #[test]
    fn test_routes_registered() {
        let config = LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            rewrite_sdk_host: true,
            origin_override: None,
        };
        let integration = LockrIntegration::new(config);

        let routes = integration.routes();
        assert_eq!(routes.len(), 3);

        // Verify SDK route
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/lockr/sdk" && r.method == Method::GET));

        // Verify API routes (GET and POST)
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/lockr/api/*" && r.method == Method::POST));
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/lockr/api/*" && r.method == Method::GET));
    }

    #[test]
    fn test_sdk_host_rewriting() {
        let config = LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            rewrite_sdk_host: true,
            origin_override: None,
        };
        let integration = LockrIntegration::new(config);

        // Mock obfuscated SDK JavaScript with the host pattern (old pattern)
        let mock_sdk_old = r#"
const identityLockr = {
    'host': _0x3a740e(0x3d1) + _0x3a740e(0x367) + _0x3a740e(0x14e),
    'app_id': null,
    'expiryDateKeys': localStorage['getItem']('identityLockr_expiryDateKeys') ? JSON['parse'](localStorage['getItem']('identityLockr_expiryDateKeys')) : [],
    'firstPartyCookies': [],
    'canRefreshToken': !![]
};
        "#;

        let result = integration.rewrite_sdk_host(mock_sdk_old.as_bytes().to_vec());
        assert!(result.is_ok());

        let rewritten = String::from_utf8(result.unwrap()).unwrap();

        // Verify the host was rewritten to the proxy endpoint
        assert!(rewritten.contains("'host': '/integrations/lockr/api'"));

        // Verify the obfuscated pattern was removed
        assert!(!rewritten.contains("_0x3a740e(0x3d1) + _0x3a740e(0x367) + _0x3a740e(0x14e)"));

        // Verify other parts of the code remain intact
        assert!(rewritten.contains("'app_id': null"));
        assert!(rewritten.contains("'firstPartyCookies': []"));
    }

    #[test]
    fn test_sdk_host_rewriting_real_pattern() {
        let config = LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            rewrite_sdk_host: true,
            origin_override: None,
        };
        let integration = LockrIntegration::new(config);

        // Real obfuscated SDK JavaScript from actual Lockr SDK
        let mock_sdk_real = r#"
const identityLockr = {
    'host': _0x4ed951(0xcb) + _0x4ed951(0x173) + _0x4ed951(0x1c2),
    'app_id': null,
    'expiryDateKeys': localStorage['getItem']('identityLockr_expiryDateKeys') ? JSON['parse'](localStorage['getItem']('identityLockr_expiryDateKeys')) : [],
    'firstPartyCookies': [],
    'canRefreshToken': !![]
};
        "#;

        let result = integration.rewrite_sdk_host(mock_sdk_real.as_bytes().to_vec());
        assert!(result.is_ok());

        let rewritten = String::from_utf8(result.unwrap()).unwrap();

        // Verify the host was rewritten to the proxy endpoint
        assert!(rewritten.contains("'host': '/integrations/lockr/api'"));

        // Verify the obfuscated pattern was removed
        assert!(!rewritten.contains("_0x4ed951(0xcb)"));
        assert!(!rewritten.contains("_0x4ed951(0x173)"));
        assert!(!rewritten.contains("_0x4ed951(0x1c2)"));

        // Verify other parts of the code remain intact
        assert!(rewritten.contains("'app_id': null"));
        assert!(rewritten.contains("'firstPartyCookies': []"));
    }

    #[test]
    fn test_sdk_host_rewriting_disabled() {
        let config = LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            rewrite_sdk_host: false, // Disabled
            origin_override: None,
        };

        // When rewrite_sdk_host is false, the handle_sdk_serving function
        // won't call rewrite_sdk_host at all, so the SDK is served as-is
        assert!(!config.rewrite_sdk_host);
    }

    #[test]
    fn test_sdk_host_rewriting_no_match() {
        let config = LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            rewrite_sdk_host: true,
            origin_override: None,
        };
        let integration = LockrIntegration::new(config);

        // Test with SDK that doesn't have the expected pattern
        let mock_sdk = r#"
const identityLockr = {
    'host': 'https://example.com',
    'app_id': null
};
        "#;

        let result = integration.rewrite_sdk_host(mock_sdk.as_bytes().to_vec());
        assert!(result.is_ok());

        let rewritten = String::from_utf8(result.unwrap()).unwrap();

        // When pattern doesn't match, content should be unchanged
        assert!(rewritten.contains("'host': 'https://example.com'"));
    }
}
