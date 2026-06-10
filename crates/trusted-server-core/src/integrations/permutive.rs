//! Permutive integration for first-party data collection and audience management.
//!
//! This module provides transparent proxying for Permutive's API and SDK,
//! enabling first-party data collection while maintaining privacy controls.

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
        services: &RuntimeServices,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        log::info!("Handling Permutive SDK request");

        let sdk_url = self.sdk_url();
        log::info!("Fetching Permutive SDK from: {}", sdk_url);

        // TODO: Check KV store cache first (future enhancement)

        // Fetch SDK from Permutive CDN
        let permutive_req = http::Request::builder()
            .method(Method::GET)
            .uri(&sdk_url)
            .header(header::USER_AGENT, "TrustedServer/1.0")
            .header(header::ACCEPT, "application/javascript, */*")
            .body(EdgeBody::empty())
            .change_context(Self::error("Failed to build Permutive SDK request"))?;

        let backend_name = Self::backend_name_for_url(services, &sdk_url)
            .change_context(Self::error("Failed to determine backend for SDK fetch"))?;

        let permutive_response = services
            .http_client()
            .send(PlatformHttpRequest::new(permutive_req, backend_name))
            .await
            .change_context(Self::error(format!(
                "Failed to fetch Permutive SDK from {}",
                sdk_url
            )))?
            .response;

        if !permutive_response.status().is_success() {
            log::error!(
                "Permutive SDK fetch failed with status: {}",
                permutive_response.status()
            );
            return Err(Report::new(Self::error(format!(
                "Permutive SDK returned error status: {}",
                permutive_response.status()
            ))));
        }

        let sdk_body = collect_response_bounded(
            permutive_response.into_body(),
            UPSTREAM_SDK_MAX_RESPONSE_BYTES,
            PERMUTIVE_INTEGRATION_ID,
        )
        .await
        .change_context(Self::error("Failed to read Permutive SDK response body"))?;
        log::info!(
            "Successfully fetched Permutive SDK: {} bytes",
            sdk_body.len()
        );

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
            .header("X-Permutive-SDK-Proxy", "true")
            .header("X-SDK-Source", &sdk_url)
            .body(EdgeBody::from(sdk_body))
            .change_context(Self::error("Failed to build Permutive SDK response"))
    }

    async fn forward_proxy_request(
        &self,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
        route_prefix: &str,
        upstream_base: &str,
        route_name: &str,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let (parts, body) = req.into_parts();
        let original_path = parts.uri.path().to_string();
        let method = parts.method.clone();

        log::info!(
            "Proxying {} request: {} {}",
            route_name,
            method,
            original_path
        );

        let upstream_path = original_path.strip_prefix(route_prefix).ok_or_else(|| {
            Self::error(format!("Invalid {} path: {}", route_name, original_path))
        })?;

        let query = parts
            .uri
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("{}{}{}", upstream_base, upstream_path, query);

        log::info!("Forwarding {} to {}", route_name, target_url);

        let request_body = if method == Method::POST {
            let bytes =
                collect_body_bounded(body, INTEGRATION_MAX_BODY_BYTES, PERMUTIVE_INTEGRATION_ID)
                    .await?;
            EdgeBody::from(bytes)
        } else {
            EdgeBody::empty()
        };

        let mut target_req = http::Request::builder()
            .method(method)
            .uri(&target_url)
            .body(request_body)
            .change_context(Self::error(format!(
                "Failed to build {} proxy request",
                route_name
            )))?;
        self.copy_request_headers(&parts.headers, target_req.headers_mut());

        let backend_name =
            Self::backend_name_for_url(services, upstream_base).change_context(Self::error(
                format!("Failed to determine backend for {} proxy", route_name),
            ))?;

        let response = services
            .http_client()
            .send(PlatformHttpRequest::new(target_req, backend_name))
            .await
            .change_context(Self::error(format!(
                "Failed to forward request to {}",
                target_url
            )))?
            .response;

        log::info!(
            "{} responded with status: {}",
            route_name,
            response.status()
        );

        Ok(response)
    }

    /// Copy relevant request headers for proxying.
    fn copy_request_headers(&self, from: &HeaderMap<HeaderValue>, to: &mut HeaderMap<HeaderValue>) {
        // `Authorization` is intentionally NOT forwarded: it carries the
        // publisher site's own credential (e.g. staging basic-auth), which would
        // leak to the third-party upstream and can break APIs that reject an
        // unexpected `Authorization` header.
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

        // Copy any X-* custom headers, skipping TS-internal headers
        for (name, value) in from {
            let name_str = name.as_str();
            if name_str.starts_with("x-") && !INTERNAL_HEADERS.contains(&name_str) {
                to.append(name.clone(), value.clone());
            }
        }
    }

    fn backend_name_for_url(
        services: &RuntimeServices,
        target_url: &str,
    ) -> Result<String, Report<TrustedServerError>> {
        ensure_integration_backend(services, target_url, PERMUTIVE_INTEGRATION_ID, None)
    }
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<PermutiveIntegration>>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<PermutiveConfig>(PERMUTIVE_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    Ok(Some(PermutiveIntegration::new(config)))
}

/// Register the Permutive integration.
///
/// # Errors
///
/// Returns an error when the Permutive integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(PERMUTIVE_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
            .build(),
    ))
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
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let path = req.uri().path().to_string();

        if path.starts_with("/integrations/permutive/api/") {
            self.forward_proxy_request(
                services,
                req,
                "/integrations/permutive/api",
                &self.config.api_endpoint,
                "Permutive API",
            )
            .await
        } else if path.starts_with("/integrations/permutive/secure-signal/") {
            self.forward_proxy_request(
                services,
                req,
                "/integrations/permutive/secure-signal",
                &self.config.secure_signals_endpoint,
                "Permutive Secure Signals",
            )
            .await
        } else if path.starts_with("/integrations/permutive/events/") {
            self.forward_proxy_request(
                services,
                req,
                "/integrations/permutive/events",
                "https://events.permutive.app",
                "Permutive Events",
            )
            .await
        } else if path.starts_with("/integrations/permutive/sync/") {
            self.forward_proxy_request(
                services,
                req,
                "/integrations/permutive/sync",
                "https://sync.permutive.com",
                "Permutive Sync",
            )
            .await
        } else if path.starts_with("/integrations/permutive/cdn/") {
            self.forward_proxy_request(
                services,
                req,
                "/integrations/permutive/cdn",
                "https://cdn.permutive.com",
                "Permutive CDN",
            )
            .await
        } else if path == "/integrations/permutive/sdk" {
            self.handle_sdk_serving(settings, services).await
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
        _ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if !self.config.rewrite_sdk {
            return AttributeRewriteAction::keep();
        }

        if self.is_permutive_sdk_url(attr_value) {
            // Rewrite to first-party SDK endpoint.
            // Root-relative so the browser resolves it against the page host.
            // Note: a page-level `<base href>` participates in this resolution,
            // so on pages that set an external base URL these resolve against
            // that base rather than the address-bar origin — an accepted
            // tradeoff, matching GTM/Didomi/Testlight which are also relative.
            AttributeRewriteAction::replace("/integrations/permutive/sdk".to_string())
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
    use std::sync::Arc;

    use super::*;
    use crate::platform::test_support::{build_services_with_http_client, StubHttpClient};
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
            element_name: "script",
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
            assert_eq!(url, "/integrations/permutive/sdk");
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
            element_name: "script",
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
            build(&settings)
                .expect("should evaluate integration build")
                .is_none(),
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

    #[test]
    fn permutive_proxy_uses_platform_http_client() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"ok".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = PermutiveIntegration::new(PermutiveConfig {
            enabled: true,
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: String::new(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        });
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://publisher.example/integrations/permutive/api/v2.0/events")
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
            stub.recorded_backend_names(),
            vec!["stub-backend".to_string()],
            "should route outbound request through PlatformHttpClient"
        );
    }

    #[test]
    fn permutive_proxy_strips_authorization() {
        // Security regression guard: the publisher's Authorization header must
        // not be forwarded to the Permutive upstream (credential leak).
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"ok".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = PermutiveIntegration::new(PermutiveConfig {
            enabled: true,
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: String::new(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        });
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://publisher.example/integrations/permutive/api/v2.0/events")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .header(header::USER_AGENT, "test-agent")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response = futures::executor::block_on(integration.handle(&settings, &services, req))
            .expect("should proxy request");
        assert_eq!(
            response.status(),
            http::StatusCode::OK,
            "should return stubbed response"
        );

        let headers = stub.recorded_request_headers();
        assert!(
            !headers[0]
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("authorization")),
            "should NOT forward the publisher's Authorization header to Permutive"
        );
        assert!(
            headers[0]
                .iter()
                .any(|(name, value)| name == "user-agent" && value == "test-agent"),
            "should still forward required headers (user-agent)"
        );
    }
}
