//! `DataDome` integration for bot protection and security.
//!
//! This module provides transparent proxying for `DataDome`'s JavaScript tag and signal
//! collection API, enabling first-party bot protection while maintaining the permissionless
//! Trusted Server approach (no DNS/CNAME changes required).
//!
//! ## Endpoints
//!
//! - `GET /integrations/datadome/tags.js` - Proxies the `DataDome` SDK script
//! - `ANY /integrations/datadome/js/*` - Proxies signal collection API calls
//!
//! ## Script Rewriting
//!
//! The integration rewrites the `tags.js` script to replace hardcoded `DataDome` API
//! endpoints with first-party paths through Trusted Server. This ensures all browser
//! requests go through the publisher's domain rather than directly to `DataDome`.

use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use serde::Deserialize;
use validator::Validate;

use crate::backend::ensure_backend_from_url;
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
use crate::settings::{IntegrationConfig, Settings};

const DATADOME_INTEGRATION_ID: &str = "datadome";

/// Configuration for `DataDome` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct DataDomeConfig {
    /// Enable/disable the integration
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// `DataDome` JavaScript key (client-side key from `DataDome` dashboard)
    /// If provided, Trusted Server can inject the config script automatically
    #[serde(default)]
    pub js_key: Option<String>,

    /// Base URL for `DataDome` SDK/API (default: <https://js.datadome.co>)
    #[serde(default = "default_sdk_origin")]
    #[validate(url)]
    pub sdk_origin: String,

    /// Cache TTL for tags.js in seconds (default: 3600 = 1 hour)
    #[serde(default = "default_cache_ttl")]
    #[validate(range(min = 60, max = 86400))]
    pub cache_ttl_seconds: u32,

    /// Whether to rewrite `DataDome` script URLs in HTML to first-party paths
    #[serde(default = "default_rewrite_sdk")]
    pub rewrite_sdk: bool,
}

fn default_enabled() -> bool {
    false
}

fn default_sdk_origin() -> String {
    "https://js.datadome.co".to_string()
}

fn default_cache_ttl() -> u32 {
    3600
}

fn default_rewrite_sdk() -> bool {
    true
}

impl Default for DataDomeConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            js_key: None,
            sdk_origin: default_sdk_origin(),
            cache_ttl_seconds: default_cache_ttl(),
            rewrite_sdk: default_rewrite_sdk(),
        }
    }
}

impl IntegrationConfig for DataDomeConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// `DataDome` integration implementation.
pub struct DataDomeIntegration {
    config: DataDomeConfig,
}

impl DataDomeIntegration {
    fn new(config: DataDomeConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: DATADOME_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    /// Rewrite `DataDome` API URLs in the tags.js script to use first-party paths.
    ///
    /// `DataDome`'s script contains hardcoded references like:
    /// - `js.datadome.co/js/` for signal collection
    /// - Various CDN URLs for the script itself
    ///
    /// We rewrite these to `/integrations/datadome/js/` so all traffic
    /// flows through Trusted Server.
    fn rewrite_script_content(&self, content: &str, request_host: &str) -> String {
        let mut result = content.to_string();

        // Rewrite the signal collection endpoint
        // DataDome scripts typically reference their API as:
        // - "js.datadome.co/js/"
        // - "//js.datadome.co/js/"
        // - "https://js.datadome.co/js/"
        let patterns = [
            ("\"js.datadome.co/js/", &format!("\"{}/js/", request_host)),
            ("'js.datadome.co/js/", &format!("'{}/js/", request_host)),
            (
                "\"//js.datadome.co/js/",
                &format!("\"//{}/integrations/datadome/js/", request_host),
            ),
            (
                "'//js.datadome.co/js/",
                &format!("'//{}/integrations/datadome/js/", request_host),
            ),
            (
                "\"https://js.datadome.co/js/",
                &format!("\"https://{}/integrations/datadome/js/", request_host),
            ),
            (
                "'https://js.datadome.co/js/",
                &format!("'https://{}/integrations/datadome/js/", request_host),
            ),
            // Also handle the base domain references for script loading
            (
                "\"js.datadome.co\"",
                &format!("\"{}/integrations/datadome\"", request_host),
            ),
            (
                "'js.datadome.co'",
                &format!("'{}/integrations/datadome'", request_host),
            ),
        ];

        for (pattern, replacement) in patterns {
            result = result.replace(pattern, replacement);
        }

        result
    }

    /// Build target URL for proxying to `DataDome`.
    fn build_target_url(&self, path: &str, query: Option<&str>) -> String {
        let base = self.config.sdk_origin.trim_end_matches('/');
        match query {
            Some(q) => format!("{}{}?{}", base, path, q),
            None => format!("{}{}", base, path),
        }
    }

    /// Handle the /tags.js endpoint - fetch and rewrite the `DataDome` SDK.
    async fn handle_tags_js(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let request_host = req
            .get_header(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or(&settings.publisher.domain);

        let target_url = self.build_target_url("/tags.js", req.get_query_str());

        log::info!(
            "[datadome] Fetching tags.js from {} for host {}",
            target_url,
            request_host
        );

        let backend =
            ensure_backend_from_url(&target_url).change_context(Self::error("Invalid SDK URL"))?;

        let mut backend_req = Request::new(Method::GET, &target_url);
        backend_req.set_header(header::HOST, "js.datadome.co");
        backend_req.set_header(header::ACCEPT, "application/javascript, */*");

        // Copy relevant headers from original request
        if let Some(ua) = req.get_header(header::USER_AGENT) {
            backend_req.set_header(header::USER_AGENT, ua);
        }

        let mut backend_resp = backend_req
            .send(&backend)
            .change_context(Self::error("Failed to fetch tags.js from DataDome"))?;

        if backend_resp.get_status() != StatusCode::OK {
            log::warn!(
                "[datadome] tags.js fetch returned status {}",
                backend_resp.get_status()
            );
            return Ok(backend_resp);
        }

        // Read and rewrite the script content
        let body = backend_resp.take_body_str();

        let rewritten = self.rewrite_script_content(&body, request_host);

        // Build response with caching headers
        let mut response = Response::new();
        response.set_status(StatusCode::OK);
        response.set_header(header::CONTENT_TYPE, "application/javascript; charset=utf-8");
        response.set_header(
            header::CACHE_CONTROL,
            format!("public, max-age={}", self.config.cache_ttl_seconds),
        );

        // Copy CORS headers if present
        if let Some(cors) = backend_resp.get_header(header::ACCESS_CONTROL_ALLOW_ORIGIN) {
            response.set_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, cors);
        }

        response.set_body(rewritten);
        Ok(response)
    }

    /// Handle the /js/* signal collection endpoint - proxy pass-through.
    async fn handle_js_api(
        &self,
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();

        // Strip our prefix to get the DataDome path
        let datadome_path = original_path
            .strip_prefix("/integrations/datadome")
            .unwrap_or(original_path);

        let target_url = self.build_target_url(datadome_path, req.get_query_str());

        log::info!(
            "[datadome] Proxying signal request to {} (method: {})",
            target_url,
            req.get_method()
        );

        let backend =
            ensure_backend_from_url(&target_url).change_context(Self::error("Invalid API URL"))?;

        let mut backend_req = Request::new(req.get_method().clone(), &target_url);
        backend_req.set_header(header::HOST, "js.datadome.co");

        // Copy relevant headers
        let headers_to_copy = [
            header::USER_AGENT,
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
            header::CONTENT_TYPE,
            header::CONTENT_LENGTH,
            header::ORIGIN,
            header::REFERER,
        ];

        for h in &headers_to_copy {
            if let Some(value) = req.get_header(h) {
                backend_req.set_header(h, value);
            }
        }

        // Copy body for POST/PUT requests
        if req.get_method() == Method::POST || req.get_method() == Method::PUT {
            let body = req.into_body();
            backend_req.set_body(body);
        }

        let backend_resp = backend_req
            .send(&backend)
            .change_context(Self::error("Failed to proxy signal request to DataDome"))?;

        log::info!(
            "[datadome] Signal request returned status {}",
            backend_resp.get_status()
        );

        Ok(backend_resp)
    }
}

#[async_trait(?Send)]
impl IntegrationProxy for DataDomeIntegration {
    fn integration_name(&self) -> &'static str {
        DATADOME_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            // SDK script endpoint
            self.get("/tags.js"),
            // Signal collection API - all methods
            self.get("/js/*"),
            self.post("/js/*"),
        ]
    }

    async fn handle(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path();

        if path == "/integrations/datadome/tags.js" {
            self.handle_tags_js(settings, req).await
        } else if path.starts_with("/integrations/datadome/js/") {
            self.handle_js_api(settings, req).await
        } else {
            Err(Report::new(Self::error(format!(
                "Unknown DataDome route: {}",
                path
            ))))
        }
    }
}

/// HTML attribute rewriter to convert `DataDome` script URLs to first-party paths.
struct DataDomeAttributeRewriter {
    config: DataDomeConfig,
}

impl DataDomeAttributeRewriter {
    fn new(config: DataDomeConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }
}

impl IntegrationAttributeRewriter for DataDomeAttributeRewriter {
    fn integration_id(&self) -> &'static str {
        DATADOME_INTEGRATION_ID
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        self.config.rewrite_sdk && attribute == "src"
    }

    fn rewrite(
        &self,
        _attr_name: &str,
        attr_value: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        // Check if this is a DataDome script URL
        let is_datadome = attr_value.contains("js.datadome.co")
            || attr_value.contains("datadome.co/tags.js");

        if !is_datadome {
            return AttributeRewriteAction::Keep;
        }

        // Rewrite to first-party path
        let new_url = if attr_value.contains("/tags.js") {
            format!(
                "{}://{}/integrations/datadome/tags.js",
                ctx.request_scheme, ctx.request_host
            )
        } else {
            // Generic DataDome URL - point to our proxy base
            format!(
                "{}://{}/integrations/datadome/tags.js",
                ctx.request_scheme, ctx.request_host
            )
        };

        log::info!(
            "[datadome] Rewriting script src from {} to {}",
            attr_value,
            new_url
        );

        AttributeRewriteAction::Replace(new_url)
    }
}

fn build(settings: &Settings) -> Option<(Arc<DataDomeIntegration>, DataDomeConfig)> {
    let config = match settings.integration_config::<DataDomeConfig>(DATADOME_INTEGRATION_ID) {
        Ok(Some(config)) => config,
        Ok(None) => {
            log::debug!("[datadome] Integration disabled or not configured");
            return None;
        }
        Err(err) => {
            log::error!("[datadome] Failed to load integration config: {err:?}");
            return None;
        }
    };

    log::info!(
        "[datadome] Registering integration (sdk_origin: {}, rewrite_sdk: {})",
        config.sdk_origin,
        config.rewrite_sdk
    );

    Some((DataDomeIntegration::new(config.clone()), config))
}

/// Register the `DataDome` integration with Trusted Server.
#[must_use] 
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let (integration, config) = build(settings)?;
    let rewriter = DataDomeAttributeRewriter::new(config);

    Some(
        IntegrationRegistration::builder(DATADOME_INTEGRATION_ID)
            .with_proxy(integration)
            .with_attribute_rewriter(rewriter)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> DataDomeConfig {
        DataDomeConfig {
            enabled: true,
            js_key: Some("test-key".to_string()),
            sdk_origin: "https://js.datadome.co".to_string(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        }
    }

    #[test]
    fn test_rewrite_script_content() {
        let integration = DataDomeIntegration::new(test_config());

        let original = r#"
            var endpoint = "js.datadome.co/js/";
            var endpoint2 = "https://js.datadome.co/js/endpoint";
            var host = "js.datadome.co";
        "#;

        let rewritten = integration.rewrite_script_content(original, "publisher.com");

        assert!(rewritten.contains("publisher.com/js/"));
        assert!(rewritten.contains("https://publisher.com/integrations/datadome/js/endpoint"));
        assert!(rewritten.contains("publisher.com/integrations/datadome"));
        assert!(!rewritten.contains("js.datadome.co/js/"));
    }

    #[test]
    fn test_build_target_url() {
        let integration = DataDomeIntegration::new(test_config());

        assert_eq!(
            integration.build_target_url("/tags.js", None),
            "https://js.datadome.co/tags.js"
        );

        assert_eq!(
            integration.build_target_url("/js/check", Some("foo=bar")),
            "https://js.datadome.co/js/check?foo=bar"
        );
    }

    #[test]
    fn test_attribute_rewriter_matches_datadome() {
        let rewriter = DataDomeAttributeRewriter::new(test_config());

        assert!(rewriter.handles_attribute("src"));
        assert!(!rewriter.handles_attribute("href"));

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "publisher.com",
            request_scheme: "https",
            origin_host: "origin.publisher.com",
        };

        // Should rewrite DataDome URLs
        let action = rewriter.rewrite("src", "https://js.datadome.co/tags.js", &ctx);
        match action {
            AttributeRewriteAction::Replace(new_url) => {
                assert_eq!(new_url, "https://publisher.com/integrations/datadome/tags.js");
            }
            _ => panic!("Expected Replace action"),
        }

        // Should not rewrite other URLs
        let action = rewriter.rewrite("src", "https://example.com/script.js", &ctx);
        assert!(matches!(action, AttributeRewriteAction::Keep));
    }
}
