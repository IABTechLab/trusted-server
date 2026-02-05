//! `DataDome` integration for bot protection and security.
//!
//! This module provides transparent proxying for `DataDome`'s JavaScript tag and signal
//! collection API, enabling first-party bot protection while maintaining the permissionless
//! Trusted Server approach (no DNS/CNAME changes required).
//!
//! # Overview
//!
//! `DataDome` provides real-time bot protection and fraud prevention. This integration enables
//! first-party delivery of `DataDome`'s JavaScript SDK and signal collection through Trusted
//! Server, eliminating the need for DNS/CNAME configuration while improving protection against
//! ad blockers that may interfere with third-party scripts.
//!
//! # Benefits
//!
//! - **No DNS changes required**: Works immediately without CNAME setup
//! - **First-party context**: All traffic flows through the publisher's domain
//! - **Ad blocker resistance**: First-party scripts are less likely to be blocked
//! - **Automatic URL rewriting**: SDK scripts are transparently rewritten to use first-party paths
//!
//! # Configuration
//!
//! Add to `trusted-server.toml`:
//!
//! ```toml
//! [integrations.datadome]
//! enabled = true
//! sdk_origin = "https://js.datadome.co"        # SDK script origin
//! api_origin = "https://api-js.datadome.co"    # Signal collection API origin
//! cache_ttl_seconds = 3600                     # Cache TTL for tags.js (1 hour)
//! rewrite_sdk = true                           # Rewrite DataDome URLs in HTML
//! ```
//!
//! # Endpoints
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | `GET` | `/integrations/datadome/tags.js` | Proxies the `DataDome` SDK script |
//! | `GET/POST` | `/integrations/datadome/js/*` | Proxies signal collection API calls |
//!
//! # Request Flow
//!
//! 1. **SDK Loading**: Browser requests `/integrations/datadome/tags.js`
//! 2. **Proxy & Rewrite**: Trusted Server fetches from `js.datadome.co`, rewrites internal
//!    URLs to first-party paths using [`DATADOME_URL_PATTERN`]
//! 3. **Signal Collection**: SDK sends signals to `/integrations/datadome/js/`
//! 4. **Transparent Proxy**: Trusted Server forwards to `api-js.datadome.co`, returns response
//!
//! # HTML Attribute Rewriting
//!
//! When `rewrite_sdk = true`, the integration implements [`IntegrationAttributeRewriter`] to
//! automatically rewrite `DataDome` script URLs in HTML responses:
//!
//! - `<script src="https://js.datadome.co/tags.js">` becomes
//!   `<script src="https://publisher.com/integrations/datadome/tags.js">`
//! - Handles both `src` and `href` attributes (for preload/prefetch links)

use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use once_cell::sync::Lazy;
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

const DATADOME_INTEGRATION_ID: &str = "datadome";

/// Regex pattern for matching and rewriting `DataDome` URLs in script content.
///
/// Pattern breakdown:
/// - `(['"])` - Capture group 1: opening quote (single or double)
/// - `(https?:)?` - Capture group 2: optional protocol (http: or https:)
/// - `(//)?` - Capture group 3: optional protocol-relative slashes
/// - `(api-)?` - Capture group 4: optional "api-" prefix for api-js.datadome.co
/// - `js\.datadome\.co` - Literal domain we're rewriting
/// - `(/[^'"]*)?` - Capture group 5: optional path (everything until closing quote)
/// - `(['"])` - Capture group 6: closing quote
///
/// This handles URLs like:
/// - `"https://js.datadome.co/tags.js"`
/// - `"https://api-js.datadome.co/js/check"`
/// - `'//js.datadome.co/js/check'`
/// - `"api-js.datadome.co/js/check"`
/// - `"js.datadome.co"`
static DATADOME_URL_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(['"])(https?:)?(//)?(api-)?js\.datadome\.co(/[^'"]*)?(['"])"#)
        .expect("DataDome URL rewrite regex should compile")
});

/// Configuration for `DataDome` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct DataDomeConfig {
    /// Enable/disable the integration
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Base URL for `DataDome` SDK script (default: <https://js.datadome.co>)
    /// Used for fetching and serving tags.js
    #[serde(default = "default_sdk_origin")]
    #[validate(url)]
    pub sdk_origin: String,

    /// Base URL for `DataDome` signal collection API (default: <https://api-js.datadome.co>)
    /// Used for proxying /js/* API requests
    #[serde(default = "default_api_origin")]
    #[validate(url)]
    pub api_origin: String,

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

fn default_api_origin() -> String {
    "https://api-js.datadome.co".to_string()
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
            sdk_origin: default_sdk_origin(),
            api_origin: default_api_origin(),
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
    /// - `js.datadome.co/tags.js` for SDK script
    /// - `api-js.datadome.co/js/` for signal collection API
    /// - `js.datadome.co` as bare domain references
    ///
    /// We rewrite these to root-relative paths like `/integrations/datadome/...` so all traffic
    /// flows through Trusted Server. Root-relative paths work correctly regardless of the
    /// current page path.
    ///
    /// Uses the static [`DATADOME_URL_PATTERN`] regex to handle all URL variants:
    /// - Absolute URLs: `https://js.datadome.co/path` or `https://api-js.datadome.co/path`
    /// - Protocol-relative: `//js.datadome.co/path` or `//api-js.datadome.co/path`
    /// - Bare domain: `js.datadome.co/path` or `api-js.datadome.co/path`
    /// - All quote styles: `"..."` and `'...'`
    fn rewrite_script_content(&self, content: &str) -> String {
        DATADOME_URL_PATTERN
            .replace_all(content, |caps: &regex::Captures| {
                let open_quote = &caps[1];
                let path = caps.get(5).map_or("", |m| m.as_str());
                let close_quote = &caps[6];

                // Rewrite to root-relative first-party paths
                // The path already includes the leading slash if present
                if path.is_empty() {
                    // Bare domain reference: "js.datadome.co" or "api-js.datadome.co"
                    format!("{}/integrations/datadome{}", open_quote, close_quote)
                } else {
                    // Domain with path: "js.datadome.co/js/check" or "api-js.datadome.co/js/check"
                    format!(
                        "{}/integrations/datadome{}{}",
                        open_quote, path, close_quote
                    )
                }
            })
            .into_owned()
    }

    /// Build target URL for proxying SDK requests to `DataDome` (js.datadome.co).
    fn build_sdk_url(&self, path: &str, query: Option<&str>) -> String {
        let base = self.config.sdk_origin.trim_end_matches('/');
        match query {
            Some(q) => format!("{}{}?{}", base, path, q),
            None => format!("{}{}", base, path),
        }
    }

    /// Build target URL for proxying API requests to `DataDome` (api-js.datadome.co).
    fn build_api_url(&self, path: &str, query: Option<&str>) -> String {
        let base = self.config.api_origin.trim_end_matches('/');
        match query {
            Some(q) => format!("{}{}?{}", base, path, q),
            None => format!("{}{}", base, path),
        }
    }

    /// Extract the host from a URL for use in the Host header.
    fn extract_host(url: &str) -> &str {
        url.trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("api-js.datadome.co")
    }

    /// Handle the /tags.js endpoint - fetch and rewrite the `DataDome` SDK.
    async fn handle_tags_js(
        &self,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let target_url = self.build_sdk_url("/tags.js", req.get_query_str());

        log::info!("[datadome] Fetching tags.js from {}", target_url);

        let backend =
            ensure_backend_from_url(&target_url).change_context(Self::error("Invalid SDK URL"))?;

        let sdk_host = Self::extract_host(&self.config.sdk_origin);

        let mut backend_req = Request::new(Method::GET, &target_url);
        backend_req.set_header(header::HOST, sdk_host);
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
        let rewritten = self.rewrite_script_content(&body);

        // Build response with caching headers
        let mut response = Response::new();
        response.set_status(StatusCode::OK);
        response.set_header(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        );
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

    /// Handle the /js/* signal collection endpoint - proxy pass-through to api-js.datadome.co.
    async fn handle_js_api(
        &self,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();

        // Strip our prefix to get the DataDome path
        let datadome_path = original_path
            .strip_prefix("/integrations/datadome")
            .unwrap_or(original_path);

        // Use api_origin (api-js.datadome.co) for signal collection requests
        let target_url = self.build_api_url(datadome_path, req.get_query_str());
        let api_host = Self::extract_host(&self.config.api_origin);

        log::info!(
            "[datadome] Proxying signal request to {} (method: {}, host: {})",
            target_url,
            req.get_method(),
            api_host
        );

        let backend =
            ensure_backend_from_url(&target_url).change_context(Self::error("Invalid API URL"))?;

        let mut backend_req = Request::new(req.get_method().clone(), &target_url);
        backend_req.set_header(header::HOST, api_host);

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

    /// Extract the path portion after the `DataDome` domain from a URL.
    ///
    /// Returns the path (including leading slash) or `/tags.js` as default.
    fn extract_datadome_path(url: &str) -> &str {
        url.split_once("js.datadome.co")
            .and_then(|(_, after)| {
                if after.starts_with('/') {
                    Some(after)
                } else {
                    None
                }
            })
            .unwrap_or("/tags.js")
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
            // Need both exact /js/ and wildcard /js/* since matchit's {*rest} requires content
            self.get("/js/"),
            self.get("/js/*"),
            self.post("/js/"),
            self.post("/js/*"),
        ]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path();

        if path == "/integrations/datadome/tags.js" {
            self.handle_tags_js(req).await
        } else if path.starts_with("/integrations/datadome/js/") {
            self.handle_js_api(req).await
        } else {
            Err(Report::new(Self::error(format!(
                "Unknown DataDome route: {}",
                path
            ))))
        }
    }
}

impl IntegrationAttributeRewriter for DataDomeIntegration {
    fn integration_id(&self) -> &'static str {
        DATADOME_INTEGRATION_ID
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
        // Check if this is a DataDome script URL
        let is_datadome =
            attr_value.contains("js.datadome.co") || attr_value.contains("datadome.co/tags.js");

        if !is_datadome {
            return AttributeRewriteAction::Keep;
        }

        let path = Self::extract_datadome_path(attr_value);
        let new_url = format!(
            "{}://{}/integrations/datadome{}",
            ctx.request_scheme, ctx.request_host, path
        );

        log::info!(
            "[datadome] Rewriting script src from {} to {}",
            attr_value,
            new_url
        );

        AttributeRewriteAction::Replace(new_url)
    }
}

fn build(settings: &Settings) -> Option<Arc<DataDomeIntegration>> {
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

    Some(DataDomeIntegration::new(config))
}

/// Register the `DataDome` integration with Trusted Server.
#[must_use]
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;

    Some(
        IntegrationRegistration::builder(DATADOME_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> DataDomeConfig {
        DataDomeConfig {
            enabled: true,
            sdk_origin: "https://js.datadome.co".to_string(),
            api_origin: "https://api-js.datadome.co".to_string(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        }
    }

    #[test]
    fn rewrite_script_content() {
        let integration = DataDomeIntegration::new(test_config());

        let original = r#"
            var endpoint = "js.datadome.co/js/";
            var endpoint2 = "https://js.datadome.co/js/endpoint";
            var host = "js.datadome.co";
        "#;

        let rewritten = integration.rewrite_script_content(original);

        // All URLs should be rewritten to root-relative /integrations/datadome/...
        assert!(
            rewritten.contains("\"/integrations/datadome/js/\""),
            "Bare domain with path should be rewritten to root-relative. Got: {}",
            rewritten
        );
        assert!(
            rewritten.contains("\"/integrations/datadome/js/endpoint\""),
            "Absolute URL should be rewritten to root-relative. Got: {}",
            rewritten
        );
        assert!(
            rewritten.contains("\"/integrations/datadome\""),
            "Bare domain should be rewritten to root-relative. Got: {}",
            rewritten
        );
        // Original domain should not appear
        assert!(
            !rewritten.contains("js.datadome.co"),
            "Original domain should be replaced. Got: {}",
            rewritten
        );
    }

    #[test]
    fn rewrite_script_content_all_url_formats() {
        let integration = DataDomeIntegration::new(test_config());

        // Test all URL format variations
        let original = r#"
            var a = "js.datadome.co/js/check";
            var b = 'js.datadome.co/js/check';
            var c = "//js.datadome.co/js/check";
            var d = '//js.datadome.co/js/check';
            var e = "https://js.datadome.co/js/check";
            var f = 'https://js.datadome.co/js/check';
            var g = "http://js.datadome.co/js/check";
            var h = "js.datadome.co";
            var i = 'js.datadome.co';
        "#;

        let rewritten = integration.rewrite_script_content(original);

        // Check each format is rewritten correctly to root-relative paths
        assert!(rewritten.contains(r#"var a = "/integrations/datadome/js/check""#));
        assert!(rewritten.contains(r#"var b = '/integrations/datadome/js/check'"#));
        assert!(rewritten.contains(r#"var c = "/integrations/datadome/js/check""#));
        assert!(rewritten.contains(r#"var d = '/integrations/datadome/js/check'"#));
        assert!(rewritten.contains(r#"var e = "/integrations/datadome/js/check""#));
        assert!(rewritten.contains(r#"var f = '/integrations/datadome/js/check'"#));
        assert!(rewritten.contains(r#"var g = "/integrations/datadome/js/check""#));
        assert!(rewritten.contains(r#"var h = "/integrations/datadome""#));
        assert!(rewritten.contains(r#"var i = '/integrations/datadome'"#));

        // No original domain should remain
        assert!(!rewritten.contains("js.datadome.co"));
    }

    #[test]
    fn rewrite_script_content_preserves_non_datadome_urls() {
        let integration = DataDomeIntegration::new(test_config());

        let original = r#"
            var other = "https://example.com/some/path";
            var datadome = "https://js.datadome.co/js/check";
            var text = "This mentions js.datadome.co in text";
        "#;

        let rewritten = integration.rewrite_script_content(original);

        // Non-DataDome URLs should be preserved
        assert!(rewritten.contains(r#""https://example.com/some/path""#));
        // DataDome URL should be rewritten to root-relative path
        assert!(rewritten.contains(r#""/integrations/datadome/js/check""#));
        // Plain text mention (not in quotes as URL) should be preserved
        // The regex only matches quoted strings, so inline text is untouched
        assert!(rewritten.contains("mentions js.datadome.co in text"));
    }

    #[test]
    fn rewrite_script_content_api_js_subdomain() {
        let integration = DataDomeIntegration::new(test_config());

        // Test api-js.datadome.co URLs (signal collection API)
        let original = r#"
            var apiEndpoint = "https://api-js.datadome.co/js/";
            var apiCheck = "api-js.datadome.co/js/check";
            var apiProtocolRelative = "//api-js.datadome.co/js/signal";
            var sdkUrl = "https://js.datadome.co/tags.js";
        "#;

        let rewritten = integration.rewrite_script_content(original);

        // api-js.datadome.co URLs should be rewritten to root-relative paths
        assert!(
            rewritten.contains(r#""/integrations/datadome/js/""#),
            "Absolute api-js URL should be rewritten. Got: {}",
            rewritten
        );
        assert!(
            rewritten.contains(r#""/integrations/datadome/js/check""#),
            "Bare api-js URL should be rewritten. Got: {}",
            rewritten
        );
        assert!(
            rewritten.contains(r#""/integrations/datadome/js/signal""#),
            "Protocol-relative api-js URL should be rewritten. Got: {}",
            rewritten
        );
        // js.datadome.co should also be rewritten
        assert!(
            rewritten.contains(r#""/integrations/datadome/tags.js""#),
            "SDK URL should be rewritten. Got: {}",
            rewritten
        );

        // No original DataDome domains should remain
        assert!(
            !rewritten.contains("api-js.datadome.co"),
            "api-js.datadome.co should be replaced. Got: {}",
            rewritten
        );
        assert!(
            !rewritten.contains("js.datadome.co"),
            "js.datadome.co should be replaced. Got: {}",
            rewritten
        );
    }

    #[test]
    fn build_sdk_url() {
        let integration = DataDomeIntegration::new(test_config());

        assert_eq!(
            integration.build_sdk_url("/tags.js", None),
            "https://js.datadome.co/tags.js"
        );

        assert_eq!(
            integration.build_sdk_url("/tags.js", Some("key=abc")),
            "https://js.datadome.co/tags.js?key=abc"
        );
    }

    #[test]
    fn build_api_url() {
        let integration = DataDomeIntegration::new(test_config());

        assert_eq!(
            integration.build_api_url("/js/check", None),
            "https://api-js.datadome.co/js/check"
        );

        assert_eq!(
            integration.build_api_url("/js/check", Some("foo=bar")),
            "https://api-js.datadome.co/js/check?foo=bar"
        );
    }

    #[test]
    fn extract_host() {
        assert_eq!(
            DataDomeIntegration::extract_host("https://api-js.datadome.co"),
            "api-js.datadome.co"
        );
        assert_eq!(
            DataDomeIntegration::extract_host("https://js.datadome.co/path"),
            "js.datadome.co"
        );
        assert_eq!(
            DataDomeIntegration::extract_host("http://example.com:8080/path"),
            "example.com:8080"
        );
    }

    #[test]
    fn extract_datadome_path() {
        assert_eq!(
            DataDomeIntegration::extract_datadome_path("https://js.datadome.co/tags.js"),
            "/tags.js"
        );
        assert_eq!(
            DataDomeIntegration::extract_datadome_path("//js.datadome.co/js/check"),
            "/js/check"
        );
        assert_eq!(
            DataDomeIntegration::extract_datadome_path("js.datadome.co/js/signal"),
            "/js/signal"
        );
        // Bare domain without path should default to /tags.js
        assert_eq!(
            DataDomeIntegration::extract_datadome_path("https://js.datadome.co"),
            "/tags.js"
        );
        // api-js subdomain
        assert_eq!(
            DataDomeIntegration::extract_datadome_path("https://api-js.datadome.co/js/"),
            "/js/"
        );
    }

    #[test]
    fn attribute_rewriter_matches_datadome() {
        let integration = DataDomeIntegration::new(test_config());

        // Should handle both src and href attributes
        assert!(integration.handles_attribute("src"));
        assert!(integration.handles_attribute("href"));
        assert!(!integration.handles_attribute("data-src"));

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "publisher.com",
            request_scheme: "https",
            origin_host: "origin.publisher.com",
        };

        // Should rewrite DataDome URLs in src
        let action = integration.rewrite("src", "https://js.datadome.co/tags.js", &ctx);
        match action {
            AttributeRewriteAction::Replace(new_url) => {
                assert_eq!(
                    new_url,
                    "https://publisher.com/integrations/datadome/tags.js"
                );
            }
            _ => panic!("Expected Replace action"),
        }

        // Should rewrite DataDome URLs in href (for link preload/prefetch)
        let action = integration.rewrite("href", "https://js.datadome.co/tags.js", &ctx);
        match action {
            AttributeRewriteAction::Replace(new_url) => {
                assert_eq!(
                    new_url,
                    "https://publisher.com/integrations/datadome/tags.js"
                );
            }
            _ => panic!("Expected Replace action for href"),
        }

        // Should not rewrite other URLs
        let action = integration.rewrite("src", "https://example.com/script.js", &ctx);
        assert!(matches!(action, AttributeRewriteAction::Keep));
    }

    #[test]
    fn attribute_rewriter_preserves_path() {
        let integration = DataDomeIntegration::new(test_config());

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "publisher.com",
            request_scheme: "https",
            origin_host: "origin.publisher.com",
        };

        // Should preserve /js/... paths for signal collection API
        let action = integration.rewrite("src", "https://js.datadome.co/js/check", &ctx);
        match action {
            AttributeRewriteAction::Replace(new_url) => {
                assert_eq!(
                    new_url,
                    "https://publisher.com/integrations/datadome/js/check"
                );
            }
            _ => panic!("Expected Replace action"),
        }

        // Should handle protocol-relative URLs
        let action = integration.rewrite("href", "//js.datadome.co/js/signal", &ctx);
        match action {
            AttributeRewriteAction::Replace(new_url) => {
                assert_eq!(
                    new_url,
                    "https://publisher.com/integrations/datadome/js/signal"
                );
            }
            _ => panic!("Expected Replace action for protocol-relative URL"),
        }

        // Bare domain without path should default to /tags.js
        let action = integration.rewrite("src", "https://js.datadome.co", &ctx);
        match action {
            AttributeRewriteAction::Replace(new_url) => {
                assert_eq!(
                    new_url,
                    "https://publisher.com/integrations/datadome/tags.js"
                );
            }
            _ => panic!("Expected Replace action for bare domain"),
        }
    }
}
