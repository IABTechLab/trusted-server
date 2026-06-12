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
//!    URLs to first-party paths using `DATADOME_URL_PATTERN`
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

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header;
use http::{Method, StatusCode};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use validator::Validate;

use crate::error::TrustedServerError;
use crate::integrations::{
    collect_body_bounded, collect_response_bounded, ensure_integration_backend,
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration, IntegrationRequestFilter, RequestFilterDecision, RequestFilterInput,
    INTEGRATION_MAX_BODY_BYTES, UPSTREAM_SDK_MAX_RESPONSE_BYTES,
};
use crate::platform::{PlatformHttpRequest, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

mod protection;

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
static DATADOME_URL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(['"])(https?:)?(//)?(api-)?js\.datadome\.co(/[^'"]*)?(['"])"#)
        .expect("DataDome URL rewrite regex should compile")
});

/// Configuration for `DataDome` integration.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
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

    /// Whether to call `DataDome` Protection API before route matching.
    #[serde(default)]
    pub enable_protection: bool,

    /// Runtime secret store containing the `DataDome` server-side key.
    #[serde(default = "default_server_side_key_secret_store")]
    pub server_side_key_secret_store: String,

    /// Secret name containing the `DataDome` server-side key.
    #[serde(default = "default_server_side_key_secret_name")]
    pub server_side_key_secret_name: String,

    /// Base URL for the `DataDome` Protection API.
    #[serde(default = "default_protection_api_origin")]
    #[validate(url)]
    pub protection_api_origin: String,

    /// First-byte timeout for Protection API calls, in milliseconds.
    #[serde(default = "default_timeout_ms")]
    #[validate(range(min = 1, max = 10000))]
    pub timeout_ms: u32,

    /// Regex for URLs to exclude from Protection API validation.
    #[serde(default = "default_url_pattern_exclusion")]
    pub url_pattern_exclusion: String,

    /// Regex for URLs to include in Protection API validation.
    #[serde(default)]
    pub url_pattern_inclusion: String,

    /// Reserved flag for future GraphQL payload extraction.
    #[serde(default)]
    pub enable_graphql_support: bool,

    /// `DataDome` client-side key used for auto-injecting the browser tag.
    #[serde(default)]
    pub client_side_key: String,

    /// Whether to auto-inject the `DataDome` browser tag when a client-side key exists.
    #[serde(default = "default_inject_client_side_tag")]
    pub inject_client_side_tag: bool,

    /// URL used for the injected `DataDome` browser tag.
    #[serde(default = "default_client_side_tag_url")]
    pub client_side_tag_url: String,

    /// Options assigned to `window.ddoptions` before loading the browser tag.
    #[serde(default = "default_client_side_configuration")]
    pub client_side_configuration: JsonValue,
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

fn default_protection_api_origin() -> String {
    "https://api-fastly.datadome.co".to_string()
}

fn default_server_side_key_secret_store() -> String {
    "datadome".to_string()
}

fn default_server_side_key_secret_name() -> String {
    "server_side_key".to_string()
}

fn default_timeout_ms() -> u32 {
    1500
}

fn default_url_pattern_exclusion() -> String {
    r"\.(avi|flv|mka|mkv|mov|mp4|mpeg|mpg|mp3|flac|ogg|ogm|opus|wav|webm|webp|bmp|gif|ico|jpeg|jpg|png|svg|svgz|swf|eot|otf|ttf|woff|woff2|css|less|js|map)$".to_string()
}

fn default_inject_client_side_tag() -> bool {
    true
}

fn default_client_side_tag_url() -> String {
    "/integrations/datadome/tags.js".to_string()
}

fn default_client_side_configuration() -> JsonValue {
    serde_json::json!({ "ajaxListenerPath": true })
}

impl Default for DataDomeConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            sdk_origin: default_sdk_origin(),
            api_origin: default_api_origin(),
            cache_ttl_seconds: default_cache_ttl(),
            rewrite_sdk: default_rewrite_sdk(),
            enable_protection: false,
            server_side_key_secret_store: default_server_side_key_secret_store(),
            server_side_key_secret_name: default_server_side_key_secret_name(),
            protection_api_origin: default_protection_api_origin(),
            timeout_ms: default_timeout_ms(),
            url_pattern_exclusion: default_url_pattern_exclusion(),
            url_pattern_inclusion: String::new(),
            enable_graphql_support: false,
            client_side_key: String::new(),
            inject_client_side_tag: default_inject_client_side_tag(),
            client_side_tag_url: default_client_side_tag_url(),
            client_side_configuration: default_client_side_configuration(),
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
    protection_exclusion: Option<Regex>,
    protection_inclusion: Option<Regex>,
}

impl DataDomeIntegration {
    #[cfg(test)]
    fn new(config: DataDomeConfig) -> Arc<Self> {
        Self::try_new(config).expect("should create DataDome integration")
    }

    fn try_new(mut config: DataDomeConfig) -> Result<Arc<Self>, Report<TrustedServerError>> {
        config.server_side_key_secret_store =
            config.server_side_key_secret_store.trim().to_string();
        config.server_side_key_secret_name = config.server_side_key_secret_name.trim().to_string();

        if config.enable_protection
            && (config.server_side_key_secret_store.is_empty()
                || config.server_side_key_secret_name.is_empty())
        {
            return Err(Report::new(Self::error(
                "server_side_key_secret_store and server_side_key_secret_name are required when enable_protection is true",
            )));
        }

        if config.enable_graphql_support {
            log::warn!("[datadome] enable_graphql_support is reserved and ignored in v1");
        }

        let protection_exclusion =
            Self::compile_optional_regex(&config.url_pattern_exclusion, "url_pattern_exclusion")?;
        let protection_inclusion =
            Self::compile_optional_regex(&config.url_pattern_inclusion, "url_pattern_inclusion")?;

        Ok(Arc::new(Self {
            config,
            protection_exclusion,
            protection_inclusion,
        }))
    }

    fn compile_optional_regex(
        pattern: &str,
        name: &str,
    ) -> Result<Option<Regex>, Report<TrustedServerError>> {
        if pattern.trim().is_empty() {
            return Ok(None);
        }

        Regex::new(&format!("(?i:{pattern})"))
            .map(Some)
            .map_err(|err| Report::new(Self::error(format!("Invalid {name}: {err}"))))
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
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let target_url = self.build_sdk_url("/tags.js", req.uri().query());

        log::info!("[datadome] Fetching tags.js from {}", target_url);

        let backend = Self::backend_name_for_url(services, &target_url)
            .change_context(Self::error("Invalid SDK URL"))?;

        let sdk_host = Self::extract_host(&self.config.sdk_origin);

        let mut backend_req = http::Request::builder()
            .method(Method::GET)
            .uri(&target_url)
            .header(header::HOST, sdk_host)
            .header(header::ACCEPT, "application/javascript, */*")
            .body(EdgeBody::empty())
            .change_context(Self::error("Failed to build DataDome SDK request"))?;

        // Copy relevant headers from original request
        if let Some(ua) = req.headers().get(header::USER_AGENT) {
            backend_req
                .headers_mut()
                .insert(header::USER_AGENT, ua.clone());
        }

        let backend_resp = services
            .http_client()
            .send(PlatformHttpRequest::new(backend_req, backend))
            .await
            .change_context(Self::error("Failed to fetch tags.js from DataDome"))?;

        if backend_resp.response.status() != StatusCode::OK {
            log::warn!(
                "[datadome] tags.js fetch returned status {}",
                backend_resp.response.status()
            );
            return Ok(backend_resp.response);
        }

        // Read and rewrite the script content
        let cors_header = backend_resp
            .response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .cloned();
        let body = collect_response_bounded(
            backend_resp.response.into_body(),
            UPSTREAM_SDK_MAX_RESPONSE_BYTES,
            DATADOME_INTEGRATION_ID,
        )
        .await
        .change_context(Self::error("Failed to read DataDome SDK response body"))?;
        let rewritten = self.rewrite_script_content(&String::from_utf8_lossy(&body));

        // Build response with caching headers
        let mut response = http::Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .header(
                header::CACHE_CONTROL,
                format!("public, max-age={}", self.config.cache_ttl_seconds),
            )
            .body(EdgeBody::from(rewritten.into_bytes()))
            .change_context(Self::error("Failed to build DataDome SDK response"))?;

        // Copy CORS headers if present
        if let Some(cors) = cors_header {
            response
                .headers_mut()
                .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, cors);
        }

        Ok(response)
    }

    /// Handle the /js/* signal collection endpoint - proxy pass-through to api-js.datadome.co.
    async fn handle_js_api(
        &self,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let (parts, body) = req.into_parts();
        let original_path = parts.uri.path().to_string();

        // Strip our prefix to get the DataDome path
        let datadome_path = original_path
            .strip_prefix("/integrations/datadome")
            .unwrap_or(&original_path);

        // Use api_origin (api-js.datadome.co) for signal collection requests
        let target_url = self.build_api_url(datadome_path, parts.uri.query());
        let api_host = Self::extract_host(&self.config.api_origin);

        log::info!(
            "[datadome] Proxying signal request to {} (method: {}, host: {})",
            target_url,
            parts.method,
            api_host
        );

        let backend = Self::backend_name_for_url(services, &target_url)
            .change_context(Self::error("Invalid API URL"))?;

        let request_body = if parts.method == Method::POST || parts.method == Method::PUT {
            let bytes =
                collect_body_bounded(body, INTEGRATION_MAX_BODY_BYTES, DATADOME_INTEGRATION_ID)
                    .await?;
            EdgeBody::from(bytes)
        } else {
            EdgeBody::empty()
        };

        let mut backend_req = http::Request::builder()
            .method(parts.method.clone())
            .uri(&target_url)
            .header(header::HOST, api_host)
            .body(request_body)
            .change_context(Self::error("Failed to build DataDome API request"))?;

        // Copy relevant headers from the original client request.
        // CONTENT_LENGTH is intentionally omitted: the body is re-materialized
        // via collect_body_bounded, so its length may differ from the original.
        let headers_to_copy = [
            header::USER_AGENT,
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
            header::CONTENT_TYPE,
            header::ORIGIN,
            header::REFERER,
        ];

        for h in &headers_to_copy {
            if let Some(value) = parts.headers.get(h) {
                backend_req.headers_mut().insert(h, value.clone());
            }
        }

        let backend_resp = services
            .http_client()
            .send(PlatformHttpRequest::new(backend_req, backend))
            .await
            .change_context(Self::error("Failed to proxy signal request to DataDome"))?;

        log::info!(
            "[datadome] Signal request returned status {}",
            backend_resp.response.status()
        );

        Ok(backend_resp.response)
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

    fn backend_name_for_url(
        services: &RuntimeServices,
        target_url: &str,
    ) -> Result<String, Report<TrustedServerError>> {
        ensure_integration_backend(services, target_url, DATADOME_INTEGRATION_ID, None)
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
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let path = req.uri().path().to_string();

        if path == "/integrations/datadome/tags.js" {
            self.handle_tags_js(services, req).await
        } else if path.starts_with("/integrations/datadome/js/") {
            self.handle_js_api(services, req).await
        } else {
            Err(Report::new(Self::error(format!(
                "Unknown DataDome route: {}",
                path
            ))))
        }
    }
}

#[async_trait(?Send)]
impl IntegrationRequestFilter for DataDomeIntegration {
    fn integration_id(&self) -> &'static str {
        DATADOME_INTEGRATION_ID
    }

    async fn filter_request(
        &self,
        input: RequestFilterInput<'_>,
    ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
        Ok(self.filter_protection_request(input).await)
    }
}

impl IntegrationHeadInjector for DataDomeIntegration {
    fn integration_id(&self) -> &'static str {
        DATADOME_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        if !self.config.inject_client_side_tag || self.config.client_side_key.trim().is_empty() {
            return Vec::new();
        }

        let key = serde_json::to_string(&self.config.client_side_key)
            .unwrap_or_else(|err| {
                log::warn!("[datadome] Failed to serialize client-side key: {err}");
                "\"\"".to_string()
            })
            .replace("</", "<\\/");
        let tag_url = serde_json::to_string(&self.config.client_side_tag_url)
            .unwrap_or_else(|err| {
                log::warn!("[datadome] Failed to serialize client-side tag URL: {err}");
                "\"/integrations/datadome/tags.js\"".to_string()
            })
            .replace("</", "<\\/");
        let options = serde_json::to_string(&self.config.client_side_configuration)
            .unwrap_or_else(|err| {
                log::warn!("[datadome] Failed to serialize client-side configuration: {err}");
                "{}".to_string()
            })
            .replace("</", "<\\/");

        vec![format!(
            "<script>window.ddjskey={key};window.ddoptions={options};</script><script src={tag_url} async></script>"
        )]
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

fn build(
    settings: &Settings,
) -> Result<Option<Arc<DataDomeIntegration>>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<DataDomeConfig>(DATADOME_INTEGRATION_ID)?
    else {
        log::debug!("[datadome] Integration disabled or not configured");
        return Ok(None);
    };

    log::info!(
        "[datadome] Registering integration (sdk_origin: {}, rewrite_sdk: {})",
        config.sdk_origin,
        config.rewrite_sdk
    );

    Ok(Some(DataDomeIntegration::try_new(config)?))
}

/// Register the `DataDome` integration with Trusted Server.
///
/// # Errors
///
/// Returns an error when the `DataDome` integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    let mut builder = IntegrationRegistration::builder(DATADOME_INTEGRATION_ID)
        .with_proxy(integration.clone())
        .with_attribute_rewriter(integration.clone())
        .with_head_injector(integration.clone());

    if integration.config.enable_protection {
        builder = builder.with_request_filter(integration);
    }

    Ok(Some(builder.build()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::platform::test_support::{build_services_with_http_client, StubHttpClient};
    use crate::test_support::tests::create_test_settings;

    fn test_config() -> DataDomeConfig {
        DataDomeConfig {
            enabled: true,
            sdk_origin: "https://js.datadome.co".to_string(),
            api_origin: "https://api-js.datadome.co".to_string(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            ..DataDomeConfig::default()
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
    fn protection_enabled_requires_server_side_key_secret_store() {
        let mut config = test_config();
        config.enable_protection = true;
        config.server_side_key_secret_store = " ".to_string();

        let err = match DataDomeIntegration::try_new(config) {
            Ok(_) => panic!("should reject empty store"),
            Err(err) => err,
        };
        assert!(
            format!("{err:?}").contains("server_side_key_secret_store"),
            "should mention secret store config"
        );
    }

    #[test]
    fn protection_enabled_requires_server_side_key_secret_name() {
        let mut config = test_config();
        config.enable_protection = true;
        config.server_side_key_secret_name = " ".to_string();

        let err = match DataDomeIntegration::try_new(config) {
            Ok(_) => panic!("should reject empty name"),
            Err(err) => err,
        };
        assert!(
            format!("{err:?}").contains("server_side_key_secret_name"),
            "should mention secret name config"
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

    #[test]
    fn datadome_proxy_uses_platform_http_client() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"ok".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = DataDomeIntegration::new(test_config());
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://publisher.example/integrations/datadome/js/check")
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
}
