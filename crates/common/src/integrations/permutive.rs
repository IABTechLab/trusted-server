//! Permutive integration for first-party data collection and audience management.
//!
//! This module provides transparent proxying for Permutive's API and SDK,
//! enabling first-party data collection while maintaining privacy controls.

use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use serde::Deserialize;
use validator::Validate;

use crate::backend::BackendConfig;
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
use crate::settings::{IntegrationConfig, Settings};

const PERMUTIVE_INTEGRATION_ID: &str = "permutive";

/// Configuration for Permutive integration.
#[derive(Debug, Deserialize, Validate)]
pub struct PermutiveConfig {
    /// Enable/disable the integration
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Organization ID for Permutive edge CDN (e.g., "myorg" from myorg.edge.permutive.app)
    #[validate(length(min = 1))]
    pub organization_id: String,

    /// Workspace ID for the Permutive SDK
    #[validate(length(min = 1))]
    pub workspace_id: String,

    /// Project ID (optional, for future use)
    #[serde(default)]
    pub project_id: String,

    /// Base URL for Permutive API (default: <https://api.permutive.com>)
    #[serde(default = "default_api_endpoint")]
    #[validate(url)]
    pub api_endpoint: String,

    /// Base URL for Permutive Secure Signals (default: <https://secure-signals.permutive.app>)
    #[serde(default = "default_secure_signals_endpoint")]
    #[validate(url)]
    pub secure_signals_endpoint: String,

    /// Cache TTL for Permutive SDK in seconds (default: 3600 = 1 hour)
    #[serde(default = "default_cache_ttl")]
    #[validate(range(min = 60, max = 86400))]
    pub cache_ttl_seconds: u32,

    /// Whether to rewrite Permutive SDK URLs in HTML
    #[serde(default = "default_rewrite_sdk")]
    pub rewrite_sdk: bool,
}

impl IntegrationConfig for PermutiveConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Permutive integration implementation.
pub struct PermutiveIntegration {
    config: PermutiveConfig,
}

impl PermutiveIntegration {
    fn new(config: PermutiveConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: PERMUTIVE_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    /// Build the Permutive SDK URL from configuration.
    /// Returns URL like: <https://myorg.edge.permutive.app/workspace-12345-web.js>
    fn sdk_url(&self) -> String {
        format!(
            "https://{}.edge.permutive.app/{}-web.js",
            self.config.organization_id, self.config.workspace_id
        )
    }

    /// Check if a URL is a Permutive SDK URL.
    fn is_permutive_sdk_url(&self, url: &str) -> bool {
        let lower = url.to_ascii_lowercase();
        (lower.contains(".edge.permutive.app") || lower.contains("cdn.permutive.com"))
            && lower.ends_with("-web.js")
    }

    /// Handle SDK serving - fetch from Permutive CDN and serve through first-party domain.
    async fn handle_sdk_serving(
        &self,
        _settings: &Settings,
        _req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        log::info!("Handling Permutive SDK request");

        let sdk_url = self.sdk_url();
        log::info!("Fetching Permutive SDK from: {}", sdk_url);

        // TODO: Check KV store cache first (future enhancement)

        // Fetch SDK from Permutive CDN
        let mut permutive_req = Request::new(Method::GET, &sdk_url);
        permutive_req.set_header(header::USER_AGENT, "TrustedServer/1.0");
        permutive_req.set_header(header::ACCEPT, "application/javascript, */*");

        let backend_name = BackendConfig::from_url(&sdk_url, true)
            .change_context(Self::error("Failed to determine backend for SDK fetch"))?;

        let mut permutive_response =
            permutive_req
                .send(backend_name)
                .change_context(Self::error(format!(
                    "Failed to fetch Permutive SDK from {}",
                    sdk_url
                )))?;

        if !permutive_response.get_status().is_success() {
            log::error!(
                "Permutive SDK fetch failed with status: {}",
                permutive_response.get_status()
            );
            return Err(Report::new(Self::error(format!(
                "Permutive SDK returned error status: {}",
                permutive_response.get_status()
            ))));
        }

        let sdk_body = permutive_response.take_body_bytes();
        log::info!(
            "Successfully fetched Permutive SDK: {} bytes",
            sdk_body.len()
        );

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
            .with_header("X-Permutive-SDK-Proxy", "true")
            .with_header("X-SDK-Source", &sdk_url)
            .with_body(sdk_body))
    }

    /// Handle API proxy - forward requests to api.permutive.com.
    async fn handle_api_proxy(
        &self,
        _settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();
        let method = req.get_method();

        log::info!(
            "Proxying Permutive API request: {} {}",
            method,
            original_path
        );

        // Extract path after /integrations/permutive/api
        let api_path = original_path
            .strip_prefix("/integrations/permutive/api")
            .ok_or_else(|| Self::error(format!("Invalid Permutive API path: {}", original_path)))?;

        // Build full target URL with query parameters
        let query = req
            .get_url()
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("{}{}{}", self.config.api_endpoint, api_path, query);

        log::info!("Forwarding to Permutive API: {}", target_url);

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
        let backend_name = BackendConfig::from_url(&self.config.api_endpoint, true)
            .change_context(Self::error("Failed to determine backend for API proxy"))?;

        let response = target_req
            .send(backend_name)
            .change_context(Self::error(format!(
                "Failed to forward request to {}",
                target_url
            )))?;

        log::info!(
            "Permutive API responded with status: {}",
            response.get_status()
        );

        Ok(response)
    }

    /// Handle Secure Signals proxy - forward requests to secure-signals.permutive.app.
    async fn handle_secure_signals_proxy(
        &self,
        _settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();
        let method = req.get_method();

        log::info!(
            "Proxying Permutive Secure Signals request: {} {}",
            method,
            original_path
        );

        // Extract path after /integrations/permutive/secure-signal
        let signal_path = original_path
            .strip_prefix("/integrations/permutive/secure-signal")
            .ok_or_else(|| {
                Self::error(format!(
                    "Invalid Permutive Secure Signals path: {}",
                    original_path
                ))
            })?;

        // Build full target URL with query parameters
        let query = req
            .get_url()
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!(
            "{}{}{}",
            self.config.secure_signals_endpoint, signal_path, query
        );

        log::info!("Forwarding to Permutive Secure Signals: {}", target_url);

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
        let backend_name = BackendConfig::from_url(&self.config.secure_signals_endpoint, true)
            .change_context(Self::error(
                "Failed to determine backend for Secure Signals proxy",
            ))?;

        let response = target_req
            .send(backend_name)
            .change_context(Self::error(format!(
                "Failed to forward request to {}",
                target_url
            )))?;

        log::info!(
            "Permutive Secure Signals responded with status: {}",
            response.get_status()
        );

        Ok(response)
    }

    /// Handle Events proxy - forward requests to events.permutive.app.
    async fn handle_events_proxy(
        &self,
        _settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();
        let method = req.get_method();

        log::info!(
            "Proxying Permutive Events request: {} {}",
            method,
            original_path
        );

        // Extract path after /integrations/permutive/events
        let events_path = original_path
            .strip_prefix("/integrations/permutive/events")
            .ok_or_else(|| {
                Self::error(format!("Invalid Permutive Events path: {}", original_path))
            })?;

        // Build full target URL with query parameters
        let query = req
            .get_url()
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("https://events.permutive.app{}{}", events_path, query);

        log::info!("Forwarding to Permutive Events: {}", target_url);

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
        let backend_name = BackendConfig::from_url("https://events.permutive.app", true)
            .change_context(Self::error("Failed to determine backend for Events proxy"))?;

        let response = target_req
            .send(backend_name)
            .change_context(Self::error(format!(
                "Failed to forward request to {}",
                target_url
            )))?;

        log::info!(
            "Permutive Events responded with status: {}",
            response.get_status()
        );

        Ok(response)
    }

    /// Handle Sync proxy - forward requests to sync.permutive.com.
    async fn handle_sync_proxy(
        &self,
        _settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();
        let method = req.get_method();

        log::info!(
            "Proxying Permutive Sync request: {} {}",
            method,
            original_path
        );

        // Extract path after /integrations/permutive/sync
        let sync_path = original_path
            .strip_prefix("/integrations/permutive/sync")
            .ok_or_else(|| {
                Self::error(format!("Invalid Permutive Sync path: {}", original_path))
            })?;

        // Build full target URL with query parameters
        let query = req
            .get_url()
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("https://sync.permutive.com{}{}", sync_path, query);

        log::info!("Forwarding to Permutive Sync: {}", target_url);

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
        let backend_name = BackendConfig::from_url("https://sync.permutive.com", true)
            .change_context(Self::error("Failed to determine backend for Sync proxy"))?;

        let response = target_req
            .send(backend_name)
            .change_context(Self::error(format!(
                "Failed to forward request to {}",
                target_url
            )))?;

        log::info!(
            "Permutive Sync responded with status: {}",
            response.get_status()
        );

        Ok(response)
    }

    /// Handle CDN proxy - forward requests to cdn.permutive.com.
    async fn handle_cdn_proxy(
        &self,
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();
        let method = req.get_method();

        log::info!(
            "Proxying Permutive CDN request: {} {}",
            method,
            original_path
        );

        // Extract path after /integrations/permutive/cdn
        let cdn_path = original_path
            .strip_prefix("/integrations/permutive/cdn")
            .ok_or_else(|| Self::error(format!("Invalid Permutive CDN path: {}", original_path)))?;

        // Build full target URL with query parameters
        let query = req
            .get_url()
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("https://cdn.permutive.com{}{}", cdn_path, query);

        log::info!("Forwarding to Permutive CDN: {}", target_url);

        // Create new request
        let mut target_req = Request::new(method.clone(), &target_url);

        // Copy headers
        self.copy_request_headers(&req, &mut target_req);

        // Get backend and forward
        let backend_name = BackendConfig::from_url("https://cdn.permutive.com", true)
            .change_context(Self::error("Failed to determine backend for CDN proxy"))?;

        let response = target_req
            .send(backend_name)
            .change_context(Self::error(format!(
                "Failed to forward request to {}",
                target_url
            )))?;

        log::info!(
            "Permutive CDN responded with status: {}",
            response.get_status()
        );

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
        ];

        for header_name in &headers_to_copy {
            if let Some(value) = from.get_header(header_name) {
                to.set_header(header_name, value);
            }
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

fn build(settings: &Settings) -> Option<Arc<PermutiveIntegration>> {
    let config = match settings.integration_config::<PermutiveConfig>(PERMUTIVE_INTEGRATION_ID) {
        Ok(Some(config)) => config,
        Ok(None) => return None,
        Err(err) => {
            log::error!("Failed to load Permutive integration config: {err:?}");
            return None;
        }
    };

    Some(PermutiveIntegration::new(config))
}

/// Register the Permutive integration.
#[must_use]
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;
    Some(
        IntegrationRegistration::builder(PERMUTIVE_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
            .build(),
    )
}

#[async_trait(?Send)]
impl IntegrationProxy for PermutiveIntegration {
    fn integration_name(&self) -> &'static str {
        PERMUTIVE_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            // API proxy endpoints
            self.get("/api/*"),
            self.post("/api/*"),
            // Secure Signals endpoints
            self.get("/secure-signal/*"),
            self.post("/secure-signal/*"),
            // Events endpoints
            self.get("/events/*"),
            self.post("/events/*"),
            // Sync endpoints
            self.get("/sync/*"),
            self.post("/sync/*"),
            // CDN endpoint
            self.get("/cdn/*"),
            // SDK serving
            self.get("/sdk"),
        ]
    }

    async fn handle(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path();

        if path.starts_with("/integrations/permutive/api/") {
            self.handle_api_proxy(settings, req).await
        } else if path.starts_with("/integrations/permutive/secure-signal/") {
            self.handle_secure_signals_proxy(settings, req).await
        } else if path.starts_with("/integrations/permutive/events/") {
            self.handle_events_proxy(settings, req).await
        } else if path.starts_with("/integrations/permutive/sync/") {
            self.handle_sync_proxy(settings, req).await
        } else if path.starts_with("/integrations/permutive/cdn/") {
            self.handle_cdn_proxy(settings, req).await
        } else if path == "/integrations/permutive/sdk" {
            self.handle_sdk_serving(settings, req).await
        } else {
            Err(Report::new(Self::error(format!(
                "Unknown Permutive route: {}",
                path
            ))))
        }
    }
}

impl IntegrationAttributeRewriter for PermutiveIntegration {
    fn integration_id(&self) -> &'static str {
        PERMUTIVE_INTEGRATION_ID
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
            return AttributeRewriteAction::keep();
        }

        if self.is_permutive_sdk_url(attr_value) {
            // Rewrite to first-party SDK endpoint
            AttributeRewriteAction::replace(format!(
                "{}://{}/integrations/permutive/sdk",
                ctx.request_scheme, ctx.request_host
            ))
        } else {
            AttributeRewriteAction::keep()
        }
    }
}

// Default value functions
fn default_enabled() -> bool {
    true
}

fn default_api_endpoint() -> String {
    "https://api.permutive.com".to_string()
}

fn default_secure_signals_endpoint() -> String {
    "https://secure-signals.permutive.app".to_string()
}

fn default_cache_ttl() -> u32 {
    3600 // 1 hour
}

fn default_rewrite_sdk() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn test_permutive_sdk_url_generation() {
        let config = PermutiveConfig {
            enabled: true,
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: "project-456".to_string(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        };
        let integration = PermutiveIntegration::new(config);

        assert_eq!(
            integration.sdk_url(),
            "https://myorg.edge.permutive.app/workspace-123-web.js"
        );
    }

    #[test]
    fn test_permutive_sdk_url_detection() {
        let config = PermutiveConfig {
            enabled: true,
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: String::new(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        };
        let integration = PermutiveIntegration::new(config);

        // Should match edge.permutive.app URLs
        assert!(integration
            .is_permutive_sdk_url("https://myorg.edge.permutive.app/workspace-123-web.js"));

        // Should match cdn.permutive.com URLs
        assert!(integration.is_permutive_sdk_url("https://cdn.permutive.com/myworkspace-web.js"));

        // Should not match other URLs
        assert!(!integration.is_permutive_sdk_url("https://example.com/script.js"));
        assert!(!integration.is_permutive_sdk_url("https://myorg.edge.permutive.app/other.js"));
    }

    #[test]
    fn test_attribute_rewriter_rewrites_sdk_urls() {
        let config = PermutiveConfig {
            enabled: true,
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: String::new(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        };
        let integration = PermutiveIntegration::new(config);

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        let rewritten = integration.rewrite(
            "src",
            "https://myorg.edge.permutive.app/workspace-123-web.js",
            &ctx,
        );

        assert!(matches!(rewritten, AttributeRewriteAction::Replace(_)));
        if let AttributeRewriteAction::Replace(url) = rewritten {
            assert_eq!(url, "https://edge.example.com/integrations/permutive/sdk");
        }
    }

    #[test]
    fn test_attribute_rewriter_noop_when_disabled() {
        let config = PermutiveConfig {
            enabled: true,
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: String::new(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: false, // Disabled
        };
        let integration = PermutiveIntegration::new(config);

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        let rewritten = integration.rewrite(
            "src",
            "https://myorg.edge.permutive.app/workspace-123-web.js",
            &ctx,
        );

        assert!(matches!(rewritten, AttributeRewriteAction::Keep));
    }

    #[test]
    fn test_build_requires_config() {
        let settings = create_test_settings();
        // Without [integrations.permutive] config, should not build
        assert!(
            build(&settings).is_none(),
            "Should not build without integration config"
        );
    }

    #[test]
    fn test_routes_registration() {
        let config = PermutiveConfig {
            enabled: true,
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: String::new(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        };
        let integration = PermutiveIntegration::new(config);

        let routes = integration.routes();

        // Should have API, Secure Signals, and SDK routes
        assert!(routes.len() >= 5, "Should register at least 5 routes");

        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/permutive/sdk" && r.method == Method::GET),
            "Should register SDK endpoint"
        );
    }
}
