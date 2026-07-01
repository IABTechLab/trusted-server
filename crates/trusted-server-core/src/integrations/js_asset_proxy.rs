//! JavaScript asset proxy integration.
//!
//! This integration serves explicitly configured third-party JavaScript assets
//! from first-party paths. Each asset maps one exact publisher-facing path to
//! one exact HTTPS upstream URL and can independently enable proxying, disable
//! proxying, or block matching script tags from publisher HTML.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{header, Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;
use validator::{Validate, ValidationError, ValidationErrors};

use crate::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_USER_AGENT,
};
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
use crate::platform::RuntimeServices;
use crate::proxy::{proxy_request, ProxyRequestConfig};
use crate::settings::{IntegrationConfig, Settings};

const JS_ASSET_PROXY_INTEGRATION_ID: &str = "js_asset_proxy";
const HEADER_X_TS_JS_ASSET_PROXY: &str = "X-TS-JS-Asset-Proxy";
const HEADER_X_TS_ERROR: &str = "X-TS-Error";
const ERROR_ORIGIN_UNREACHABLE: &str = "js-asset-origin-unreachable";
const ERROR_ORIGIN_STATUS: &str = "js-asset-origin-status";

/// Configuration for the JavaScript asset proxy integration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsAssetProxyConfig {
    /// Enables or disables the integration.
    #[serde(default)]
    pub enabled: bool,
    /// Optional downstream cache TTL override for every asset.
    #[serde(default)]
    pub cache_ttl_seconds: Option<u32>,
    /// JavaScript assets managed by this integration.
    #[serde(default)]
    pub assets: Vec<JsAssetProxyAsset>,
}

/// One configured JavaScript asset mapping.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsAssetProxyAsset {
    /// Exact first-party request path handled by Trusted Server.
    pub path: String,
    /// Exact upstream JavaScript URL to fetch and to match during HTML rewriting.
    pub origin_url: String,
    /// Per-asset proxy behavior.
    #[serde(default)]
    pub proxy: JsAssetProxyMode,
    /// Optional downstream cache TTL override for this asset.
    #[serde(default)]
    pub cache_ttl_seconds: Option<u32>,
}

/// Per-asset proxy behavior.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JsAssetProxyMode {
    /// Rewrite matching script URLs and serve the configured route.
    #[default]
    Enabled,
    /// Keep the asset in configuration without rewriting or route registration.
    Disabled,
    /// Remove matching script elements without route registration.
    Blocked,
}

impl IntegrationConfig for JsAssetProxyConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl Validate for JsAssetProxyConfig {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        errors.merge_self("assets", self.assets.validate());

        if self.enabled && self.assets.is_empty() {
            errors.add("assets", ValidationError::new("empty_assets"));
        }

        let mut paths = HashSet::new();
        let mut origin_urls = HashSet::new();
        for asset in &self.assets {
            if !paths.insert(asset.path.as_str()) {
                errors.add("asset_path", ValidationError::new("duplicate_asset_path"));
            }
            if !origin_urls.insert(asset.origin_url.as_str()) {
                errors.add(
                    "asset_origin_url",
                    ValidationError::new("duplicate_asset_origin_url"),
                );
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl Validate for JsAssetProxyAsset {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();

        if !self.path.starts_with('/') {
            errors.add("path", ValidationError::new("path_must_start_with_slash"));
        }
        if self.path.starts_with("//") {
            errors.add(
                "path",
                ValidationError::new("path_must_not_be_protocol_relative"),
            );
        }
        if self.path.contains('*') {
            errors.add(
                "path",
                ValidationError::new("path_must_not_contain_wildcard"),
            );
        }
        if path_contains_parent_segment(&self.path) {
            errors.add(
                "path",
                ValidationError::new("path_must_not_contain_parent_segment"),
            );
        }
        if self.path.contains(['{', '}']) {
            errors.add("path", ValidationError::new("path_must_be_exact_route"));
        }
        if self.path.contains(['?', '#']) {
            errors.add(
                "path",
                ValidationError::new("path_must_not_contain_query_or_fragment"),
            );
        }
        if self
            .path
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            errors.add(
                "path",
                ValidationError::new("path_must_not_contain_whitespace_or_control"),
            );
        }

        match Url::parse(&self.origin_url) {
            Ok(url) => {
                if url.scheme() != "https" {
                    errors.add(
                        "origin_url",
                        ValidationError::new("origin_url_must_be_https"),
                    );
                }
                if url.host_str().is_none() {
                    errors.add(
                        "origin_url",
                        ValidationError::new("origin_url_must_have_host"),
                    );
                }
            }
            Err(_) => {
                errors.add("origin_url", ValidationError::new("invalid_origin_url"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn path_contains_parent_segment(path: &str) -> bool {
    path.split('/').any(|segment| segment == "..")
}

fn normalize_script_src(script_src: &str, request_scheme: &str) -> Option<String> {
    let candidate = if script_src.starts_with("//") {
        let request_scheme = request_scheme.to_ascii_lowercase();
        if !matches!(request_scheme.as_str(), "http" | "https") {
            return None;
        }
        format!("{request_scheme}:{script_src}")
    } else {
        script_src.to_string()
    };

    let mut url = Url::parse(&candidate).ok()?;
    let has_default_port = matches!(
        (url.scheme(), url.port()),
        ("http", Some(80)) | ("https", Some(443))
    );
    if has_default_port {
        url.set_port(None).ok()?;
    }

    Some(url.to_string())
}

/// JavaScript asset proxy integration implementation.
pub struct JsAssetProxyIntegration {
    config: JsAssetProxyConfig,
}

impl JsAssetProxyIntegration {
    fn new(config: JsAssetProxyConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: JS_ASSET_PROXY_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    fn enabled_asset_for_path(&self, path: &str) -> Option<&JsAssetProxyAsset> {
        self.config
            .assets
            .iter()
            .find(|asset| asset.proxy == JsAssetProxyMode::Enabled && asset.path == path)
    }

    fn asset_for_origin_url(&self, origin_url: &str) -> Option<&JsAssetProxyAsset> {
        self.config
            .assets
            .iter()
            .find(|asset| asset.origin_url == origin_url)
    }

    fn asset_for_script_src(
        &self,
        script_src: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> Option<&JsAssetProxyAsset> {
        self.asset_for_origin_url(script_src).or_else(|| {
            let normalized_src = normalize_script_src(script_src, ctx.request_scheme)?;
            self.asset_for_origin_url(&normalized_src)
        })
    }

    fn build_proxy_config<'a>(
        origin_url: &'a str,
        req: &Request<EdgeBody>,
    ) -> ProxyRequestConfig<'a> {
        let mut config = ProxyRequestConfig::new(origin_url)
            .with_streaming()
            .with_stream_response()
            .without_forward_headers();
        config.follow_redirects = false;
        config.forward_ec_id = false;

        for header_name in [
            &HEADER_ACCEPT,
            &HEADER_ACCEPT_LANGUAGE,
            &HEADER_ACCEPT_ENCODING,
        ] {
            if let Some(value) = req.headers().get(header_name).cloned() {
                config = config.with_header(header_name.clone(), value);
            }
        }

        config.with_header(
            HEADER_USER_AGENT.clone(),
            http::HeaderValue::from_static("TrustedServer/1.0"),
        )
    }

    fn origin_host(origin_url: &str) -> String {
        Url::parse(origin_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn origin_unreachable_response() -> Response<EdgeBody> {
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header(HEADER_X_TS_ERROR, ERROR_ORIGIN_UNREACHABLE)
            .body(EdgeBody::empty())
            .expect("should build JS asset proxy unreachable response")
    }

    fn origin_status_response() -> Response<EdgeBody> {
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header(HEADER_X_TS_ERROR, ERROR_ORIGIN_STATUS)
            .body(EdgeBody::empty())
            .expect("should build JS asset proxy upstream status response")
    }

    fn vary_with_accept_encoding(upstream_vary: Option<&str>) -> String {
        match upstream_vary.map(str::trim) {
            Some("*") => "*".to_string(),
            Some(vary) if !vary.is_empty() => {
                if vary
                    .split(',')
                    .any(|header_name| header_name.trim().eq_ignore_ascii_case("accept-encoding"))
                {
                    vary.to_string()
                } else {
                    format!("{vary}, Accept-Encoding")
                }
            }
            _ => "Accept-Encoding".to_string(),
        }
    }

    fn resolved_cache_ttl_seconds(&self, asset: &JsAssetProxyAsset) -> Option<u32> {
        asset.cache_ttl_seconds.or(self.config.cache_ttl_seconds)
    }

    fn finalize_asset_response(
        &self,
        asset: &JsAssetProxyAsset,
        response: Response<EdgeBody>,
    ) -> Response<EdgeBody> {
        let (parts, body) = response.into_parts();
        let status = parts.status;
        let content_type = parts.headers.get(header::CONTENT_TYPE).cloned();
        let content_encoding = parts.headers.get(header::CONTENT_ENCODING).cloned();
        let etag = parts.headers.get(header::ETAG).cloned();
        let last_modified = parts.headers.get(header::LAST_MODIFIED).cloned();
        let upstream_vary = parts
            .headers
            .get(header::VARY)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let upstream_cache_control = parts.headers.get(header::CACHE_CONTROL).cloned();

        let mut finalized = Response::new(body);
        *finalized.status_mut() = status;
        finalized.headers_mut().insert(
            HEADER_X_TS_JS_ASSET_PROXY,
            http::HeaderValue::from_static("true"),
        );

        if let Some(content_type) = content_type {
            finalized
                .headers_mut()
                .insert(header::CONTENT_TYPE, content_type);
        }
        if let Some(content_encoding) = content_encoding {
            finalized
                .headers_mut()
                .insert(header::CONTENT_ENCODING, content_encoding);
            finalized.headers_mut().insert(
                header::VARY,
                http::HeaderValue::from_str(&Self::vary_with_accept_encoding(
                    upstream_vary.as_deref(),
                ))
                .expect("should build JS asset proxy Vary header"),
            );
        } else if let Some(upstream_vary) = upstream_vary {
            finalized.headers_mut().insert(
                header::VARY,
                http::HeaderValue::from_str(&upstream_vary)
                    .expect("should preserve JS asset proxy upstream Vary header"),
            );
        }
        if let Some(etag) = etag {
            finalized.headers_mut().insert(header::ETAG, etag);
        }
        if let Some(last_modified) = last_modified {
            finalized
                .headers_mut()
                .insert(header::LAST_MODIFIED, last_modified);
        }

        if let Some(ttl) = self.resolved_cache_ttl_seconds(asset) {
            finalized.headers_mut().insert(
                header::CACHE_CONTROL,
                http::HeaderValue::from_str(&format!("public, max-age={ttl}"))
                    .expect("should build JS asset proxy Cache-Control header"),
            );
        } else if let Some(cache_control) = upstream_cache_control {
            finalized
                .headers_mut()
                .insert(header::CACHE_CONTROL, cache_control);
        }

        finalized
    }
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<JsAssetProxyIntegration>>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<JsAssetProxyConfig>(JS_ASSET_PROXY_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    Ok(Some(JsAssetProxyIntegration::new(config)))
}

/// Register the JavaScript asset proxy integration.
///
/// # Errors
///
/// Returns an error when the integration is enabled with invalid configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(JS_ASSET_PROXY_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for JsAssetProxyIntegration {
    fn integration_name(&self) -> &'static str {
        JS_ASSET_PROXY_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        self.config
            .assets
            .iter()
            .filter(|asset| asset.proxy == JsAssetProxyMode::Enabled)
            .map(|asset| IntegrationEndpoint::new(Method::GET, asset.path.clone()))
            .collect()
    }

    async fn handle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let request_path = req.uri().path().to_string();
        let asset = self.enabled_asset_for_path(&request_path).ok_or_else(|| {
            Report::new(Self::error(format!(
                "Unknown JavaScript asset proxy route: {request_path}"
            )))
        })?;

        let origin_host = Self::origin_host(&asset.origin_url);
        let proxy_config = Self::build_proxy_config(&asset.origin_url, &req);
        let response = match proxy_request(settings, req, proxy_config, services).await {
            Ok(response) => response,
            Err(error) => {
                log::warn!(
                    "JS asset origin unreachable for path {} host {}: {:?}",
                    request_path,
                    origin_host,
                    error
                );
                return Ok(Self::origin_unreachable_response());
            }
        };

        if !response.status().is_success() {
            log::warn!(
                "JS asset origin returned status {} for path {} host {}",
                response.status(),
                request_path,
                origin_host
            );
            return Ok(Self::origin_status_response());
        }

        Ok(self.finalize_asset_response(asset, response))
    }
}

impl IntegrationAttributeRewriter for JsAssetProxyIntegration {
    fn integration_id(&self) -> &'static str {
        JS_ASSET_PROXY_INTEGRATION_ID
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        attribute == "src"
    }

    fn rewrite(
        &self,
        attr_name: &str,
        attr_value: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if attr_name != "src" || !ctx.element_name.eq_ignore_ascii_case("script") {
            return AttributeRewriteAction::keep();
        }

        let Some(asset) = self.asset_for_script_src(attr_value, ctx) else {
            return AttributeRewriteAction::keep();
        };

        match asset.proxy {
            JsAssetProxyMode::Enabled => AttributeRewriteAction::replace(asset.path.clone()),
            JsAssetProxyMode::Disabled => AttributeRewriteAction::keep(),
            JsAssetProxyMode::Blocked => AttributeRewriteAction::remove_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    use crate::constants::{HEADER_REFERER, HEADER_X_FORWARDED_FOR, HEADER_X_TS_EC};
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};
    use crate::integrations::{
        AttributeRewriteAction, IntegrationAttributeRewriter, IntegrationRegistry,
    };
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::create_test_settings;
    use http::header;
    use serde_json::json;

    fn build_http_request(method: Method, uri: &str) -> Request<EdgeBody> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(EdgeBody::empty())
            .expect("should build HTTP request")
    }

    fn asset(path: &str, origin_url: &str, proxy: JsAssetProxyMode) -> JsAssetProxyAsset {
        JsAssetProxyAsset {
            path: path.to_string(),
            origin_url: origin_url.to_string(),
            proxy,
            cache_ttl_seconds: None,
        }
    }

    fn config_with_assets(assets: Vec<JsAssetProxyAsset>) -> JsAssetProxyConfig {
        JsAssetProxyConfig {
            enabled: true,
            cache_ttl_seconds: None,
            assets,
        }
    }

    fn rewrite_context() -> IntegrationAttributeContext<'static> {
        IntegrationAttributeContext {
            attribute_name: "src",
            element_name: "script",
            request_host: "publisher.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        }
    }

    fn process_html_with_integration(
        html: &str,
        integration: Arc<JsAssetProxyIntegration>,
    ) -> String {
        let rewriter: Arc<dyn IntegrationAttributeRewriter> = integration;
        process_html_with_registry(
            html,
            IntegrationRegistry::from_rewriters(vec![rewriter], Vec::new()),
        )
    }

    fn process_html_with_registry(html: &str, integrations: IntegrationRegistry) -> String {
        let processor = create_html_processor(HtmlProcessorConfig {
            origin_host: "origin.example.com".to_string(),
            request_host: "publisher.example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations,
            max_buffered_body_bytes: 16 * 1024 * 1024,
        });
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("should process HTML");
        String::from_utf8(output).expect("should produce UTF-8 HTML")
    }

    #[test]
    fn disabled_config_does_not_register_routes() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                JS_ASSET_PROXY_INTEGRATION_ID,
                &json!({
                    "enabled": false,
                    "assets": [{
                        "path": "/assets/vendor.js",
                        "origin_url": "https://cdn.example.com/vendor.js"
                    }]
                }),
            )
            .expect("should insert integration config");

        let registry = IntegrationRegistry::new(&settings).expect("should build registry");

        assert!(
            !registry.has_route(&Method::GET, "/assets/vendor.js"),
            "disabled integration should not register asset route"
        );
    }

    #[test]
    fn enabled_config_requires_at_least_one_asset() {
        let config = JsAssetProxyConfig {
            enabled: true,
            cache_ttl_seconds: None,
            assets: Vec::new(),
        };

        assert!(
            config.validate().is_err(),
            "enabled config should reject empty assets"
        );
    }

    #[test]
    fn proxy_modes_control_routes_and_rewriting() {
        let integration = JsAssetProxyIntegration::new(config_with_assets(vec![
            asset(
                "/assets/enabled.js",
                "https://cdn.example.com/enabled.js",
                JsAssetProxyMode::Enabled,
            ),
            asset(
                "/assets/disabled.js",
                "https://cdn.example.com/disabled.js",
                JsAssetProxyMode::Disabled,
            ),
            asset(
                "/assets/blocked.js",
                "https://cdn.example.com/blocked.js",
                JsAssetProxyMode::Blocked,
            ),
        ]));

        let routes = integration.routes();
        assert_eq!(
            routes.len(),
            1,
            "only enabled assets should register routes"
        );
        assert_eq!(routes[0].method, Method::GET);
        assert_eq!(routes[0].path, "/assets/enabled.js");

        let ctx = rewrite_context();
        assert!(matches!(
            integration.rewrite("src", "https://cdn.example.com/enabled.js", &ctx),
            AttributeRewriteAction::Replace(ref value) if value == "/assets/enabled.js"
        ));
        assert!(matches!(
            integration.rewrite("src", "https://cdn.example.com/disabled.js", &ctx),
            AttributeRewriteAction::Keep
        ));
        assert!(matches!(
            integration.rewrite("src", "https://cdn.example.com/blocked.js", &ctx),
            AttributeRewriteAction::RemoveElement
        ));
    }

    #[test]
    fn non_exact_origin_url_matches_are_not_rewritten_or_blocked() {
        let integration = JsAssetProxyIntegration::new(config_with_assets(vec![asset(
            "/assets/vendor.js",
            "https://cdn.example.com/vendor.js",
            JsAssetProxyMode::Enabled,
        )]));
        let ctx = rewrite_context();

        assert!(matches!(
            integration.rewrite("src", "https://cdn.example.com/vendor.js?v=1", &ctx),
            AttributeRewriteAction::Keep
        ));
        assert!(matches!(
            integration.rewrite("src", "https://cdn.example.com/other.js", &ctx),
            AttributeRewriteAction::Keep
        ));
    }

    #[test]
    fn non_script_src_matches_are_not_rewritten_or_blocked() {
        let integration = JsAssetProxyIntegration::new(config_with_assets(vec![
            asset(
                "/assets/enabled.js",
                "https://cdn.example.com/enabled.js",
                JsAssetProxyMode::Enabled,
            ),
            asset(
                "/assets/blocked.js",
                "https://cdn.example.com/blocked.js",
                JsAssetProxyMode::Blocked,
            ),
        ]));
        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            element_name: "img",
            request_host: "publisher.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        assert!(matches!(
            integration.rewrite("src", "https://cdn.example.com/enabled.js", &ctx),
            AttributeRewriteAction::Keep
        ));
        assert!(matches!(
            integration.rewrite("src", "https://cdn.example.com/blocked.js", &ctx),
            AttributeRewriteAction::Keep
        ));
    }

    #[test]
    fn html_rewriting_only_applies_to_script_src_elements() {
        let integration = JsAssetProxyIntegration::new(config_with_assets(vec![
            asset(
                "/assets/enabled.js",
                "https://cdn.example.com/enabled.js",
                JsAssetProxyMode::Enabled,
            ),
            asset(
                "/assets/blocked.js",
                "https://cdn.example.com/blocked.js",
                JsAssetProxyMode::Blocked,
            ),
        ]));
        let html = r#"<html><body>
            <script src="https://cdn.example.com/enabled.js"></script>
            <img src="https://cdn.example.com/enabled.js">
            <script src="https://cdn.example.com/blocked.js">blocked()</script>
            <img src="https://cdn.example.com/blocked.js">
        </body></html>"#;

        let processed = process_html_with_integration(html, integration);

        assert!(processed.contains(r#"<script src="/assets/enabled.js"></script>"#));
        assert!(processed.contains(r#"<img src="https://cdn.example.com/enabled.js">"#));
        assert!(!processed.contains("blocked()"));
        assert!(processed.contains(r#"<img src="https://cdn.example.com/blocked.js">"#));
    }

    #[test]
    fn script_src_matching_normalizes_common_browser_url_forms() {
        let integration = JsAssetProxyIntegration::new(config_with_assets(vec![asset(
            "/assets/vendor.js",
            "https://cdn.example.com/vendor.js",
            JsAssetProxyMode::Enabled,
        )]));
        let ctx = rewrite_context();

        for script_src in [
            "//cdn.example.com/vendor.js",
            "HTTPS://CDN.EXAMPLE.COM/vendor.js",
            "https://cdn.example.com:443/vendor.js",
        ] {
            assert!(
                matches!(
                    integration.rewrite("src", script_src, &ctx),
                    AttributeRewriteAction::Replace(ref value) if value == "/assets/vendor.js"
                ),
                "script src {script_src} should normalize to the configured origin URL"
            );
        }
    }

    #[test]
    fn js_asset_proxy_rewriter_takes_precedence_over_native_rewriters() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt", &json!({ "enabled": true }))
            .expect("should insert GPT config");
        settings
            .integrations
            .insert_config(
                JS_ASSET_PROXY_INTEGRATION_ID,
                &json!({
                    "enabled": true,
                    "assets": [{
                        "path": "/assets/gpt.js",
                        "origin_url": "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
                        "proxy": "enabled"
                    }]
                }),
            )
            .expect("should insert JS asset proxy config");
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let html = r#"<html><body><script src="https://securepubads.g.doubleclick.net/tag/js/gpt.js"></script></body></html>"#;

        let processed = process_html_with_registry(html, registry);

        assert!(
            processed.contains(r#"<script src="/assets/gpt.js"></script>"#),
            "JS asset proxy should rewrite before GPT native rewriter: {processed}"
        );
        assert!(
            !processed.contains("/integrations/gpt/script"),
            "GPT native rewrite should not override JS asset proxy"
        );
    }

    #[test]
    fn js_asset_proxy_blocking_takes_precedence_over_native_rewriters() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt", &json!({ "enabled": true }))
            .expect("should insert GPT config");
        settings
            .integrations
            .insert_config(
                JS_ASSET_PROXY_INTEGRATION_ID,
                &json!({
                    "enabled": true,
                    "assets": [{
                        "path": "/assets/gpt.js",
                        "origin_url": "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
                        "proxy": "blocked"
                    }]
                }),
            )
            .expect("should insert JS asset proxy config");
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let html = r#"<html><body><script src="https://securepubads.g.doubleclick.net/tag/js/gpt.js">googletag.cmd.push(() => {});</script></body></html>"#;

        let processed = process_html_with_registry(html, registry);

        assert!(
            !processed.contains("googletag.cmd"),
            "blocked JS asset should remove the script element before GPT can rewrite it"
        );
        assert!(
            !processed.contains("/integrations/gpt/script"),
            "GPT native rewrite should not keep a blocked script"
        );
    }

    #[test]
    fn disabled_js_asset_proxy_candidate_allows_native_rewriters() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt", &json!({ "enabled": true }))
            .expect("should insert GPT config");
        settings
            .integrations
            .insert_config(
                JS_ASSET_PROXY_INTEGRATION_ID,
                &json!({
                    "enabled": true,
                    "assets": [{
                        "path": "/assets/gpt.js",
                        "origin_url": "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
                        "proxy": "disabled"
                    }]
                }),
            )
            .expect("should insert JS asset proxy config");
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let html = r#"<html><body><script src="https://securepubads.g.doubleclick.net/tag/js/gpt.js"></script></body></html>"#;

        let processed = process_html_with_registry(html, registry);

        assert!(
            processed.contains(r#"<script src="/integrations/gpt/script"></script>"#),
            "disabled JS asset proxy entries should not suppress native integration rewrites"
        );
    }

    #[test]
    fn rejects_duplicate_asset_paths() {
        let config = config_with_assets(vec![
            asset(
                "/assets/vendor.js",
                "https://cdn.example.com/vendor-a.js",
                JsAssetProxyMode::Enabled,
            ),
            asset(
                "/assets/vendor.js",
                "https://cdn.example.com/vendor-b.js",
                JsAssetProxyMode::Enabled,
            ),
        ]);

        assert!(
            config.validate().is_err(),
            "duplicate asset paths should be rejected"
        );
    }

    #[test]
    fn rejects_duplicate_origin_urls() {
        let config = config_with_assets(vec![
            asset(
                "/assets/vendor-a.js",
                "https://cdn.example.com/vendor.js",
                JsAssetProxyMode::Enabled,
            ),
            asset(
                "/assets/vendor-b.js",
                "https://cdn.example.com/vendor.js",
                JsAssetProxyMode::Enabled,
            ),
        ]);

        assert!(
            config.validate().is_err(),
            "duplicate origin URLs should be rejected"
        );
    }

    #[test]
    fn rejects_invalid_paths() {
        for invalid_path in [
            "assets/vendor.js",
            "//cdn.example.com/vendor.js",
            "/assets/*.js",
            "/assets/../vendor.js",
            "/assets/{vendor}.js",
            "/assets/vendor.js?v=1",
            "/assets/vendor.js#v1",
            "/assets/vendor js",
            "/assets/vendor\n.js",
        ] {
            let config = config_with_assets(vec![asset(
                invalid_path,
                "https://cdn.example.com/vendor.js",
                JsAssetProxyMode::Enabled,
            )]);

            assert!(
                config.validate().is_err(),
                "path {invalid_path} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_non_https_origins() {
        let config = config_with_assets(vec![asset(
            "/assets/vendor.js",
            "http://cdn.example.com/vendor.js",
            JsAssetProxyMode::Enabled,
        )]);

        assert!(
            config.validate().is_err(),
            "non-HTTPS origin should be rejected"
        );
    }

    #[test]
    fn rejects_unknown_proxy_mode() {
        let toml = r#"
            [[handlers]]
            path = "^/secure"
            username = "user"
            password = "pass"

            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            [request_signing]
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"

            [integrations.js_asset_proxy]
            enabled = true

            [[integrations.js_asset_proxy.assets]]
            path = "/assets/vendor.js"
            origin_url = "https://cdn.example.com/vendor.js"
            proxy = "passthrough"
        "#;
        let settings = Settings::from_toml(toml).expect("should parse settings TOML");

        assert!(
            settings
                .integration_config::<JsAssetProxyConfig>(JS_ASSET_PROXY_INTEGRATION_ID)
                .is_err(),
            "unknown proxy mode should fail deserialization"
        );
    }

    #[test]
    fn exact_configured_routes_are_registered() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                JS_ASSET_PROXY_INTEGRATION_ID,
                &json!({
                    "enabled": true,
                    "assets": [
                        {
                            "path": "/assets/vendor.js",
                            "origin_url": "https://cdn.example.com/vendor.js"
                        },
                        {
                            "path": "/assets/blocked.js",
                            "origin_url": "https://cdn.example.com/blocked.js",
                            "proxy": "blocked"
                        }
                    ]
                }),
            )
            .expect("should insert integration config");

        let registry = IntegrationRegistry::new(&settings).expect("should build registry");

        assert!(registry.has_route(&Method::GET, "/assets/vendor.js"));
        assert!(!registry.has_route(&Method::GET, "/assets/vendor.js/extra"));
        assert!(!registry.has_route(&Method::POST, "/assets/vendor.js"));
        assert!(!registry.has_route(&Method::GET, "/assets/blocked.js"));
    }

    #[test]
    fn request_path_selects_the_correct_asset() {
        let integration = JsAssetProxyIntegration::new(config_with_assets(vec![
            asset(
                "/assets/a.js",
                "https://cdn.example.com/a.js",
                JsAssetProxyMode::Enabled,
            ),
            asset(
                "/assets/b.js",
                "https://cdn.example.com/b.js",
                JsAssetProxyMode::Enabled,
            ),
        ]));

        let selected = integration
            .enabled_asset_for_path("/assets/b.js")
            .expect("should select configured asset");

        assert_eq!(selected.origin_url, "https://cdn.example.com/b.js");
    }

    #[test]
    fn successful_response_preserves_body_and_expected_headers() {
        let mut configured_asset = asset(
            "/assets/vendor.js",
            "https://cdn.example.com/vendor.js",
            JsAssetProxyMode::Enabled,
        );
        configured_asset.cache_ttl_seconds = Some(900);
        let integration =
            JsAssetProxyIntegration::new(config_with_assets(vec![configured_asset.clone()]));
        let upstream = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/javascript")
            .header(header::CONTENT_ENCODING, "gzip")
            .header(header::ETAG, "\"asset-etag\"")
            .header(header::LAST_MODIFIED, "Tue, 10 Jun 2026 00:00:00 GMT")
            .header(header::VARY, "Origin")
            .header(header::CACHE_CONTROL, "private, max-age=1")
            .header(header::SET_COOKIE, "session=1")
            .body(EdgeBody::from("console.log('ok');"))
            .expect("should build upstream JS asset response");

        let response = integration.finalize_asset_response(&configured_asset, upstream);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(HEADER_X_TS_JS_ASSET_PROXY)
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/javascript")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("gzip")
        );
        assert_eq!(
            response
                .headers()
                .get(header::ETAG)
                .and_then(|value| value.to_str().ok()),
            Some("\"asset-etag\"")
        );
        assert_eq!(
            response
                .headers()
                .get(header::LAST_MODIFIED)
                .and_then(|value| value.to_str().ok()),
            Some("Tue, 10 Jun 2026 00:00:00 GMT")
        );
        assert_eq!(
            response
                .headers()
                .get(header::VARY)
                .and_then(|value| value.to_str().ok()),
            Some("Origin, Accept-Encoding")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=900")
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "Set-Cookie should not be forwarded"
        );
    }

    #[test]
    fn preserves_upstream_cache_control_without_ttl_override() {
        let configured_asset = asset(
            "/assets/vendor.js",
            "https://cdn.example.com/vendor.js",
            JsAssetProxyMode::Enabled,
        );
        let integration =
            JsAssetProxyIntegration::new(config_with_assets(vec![configured_asset.clone()]));
        let upstream = Response::builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "public, max-age=123")
            .body(EdgeBody::from("body"))
            .expect("should build upstream JS asset response");

        let response = integration.finalize_asset_response(&configured_asset, upstream);

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=123")
        );
    }

    #[test]
    fn integration_cache_ttl_overrides_upstream_cache_control() {
        let configured_asset = asset(
            "/assets/vendor.js",
            "https://cdn.example.com/vendor.js",
            JsAssetProxyMode::Enabled,
        );
        let mut config = config_with_assets(vec![configured_asset.clone()]);
        config.cache_ttl_seconds = Some(300);
        let integration = JsAssetProxyIntegration::new(config);
        let upstream = Response::builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "private, max-age=1")
            .body(EdgeBody::from("body"))
            .expect("should build upstream JS asset response");

        let response = integration.finalize_asset_response(&configured_asset, upstream);

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=300")
        );
    }

    #[test]
    fn upstream_error_responses_have_expected_headers() {
        let unreachable = JsAssetProxyIntegration::origin_unreachable_response();
        assert_eq!(unreachable.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            unreachable
                .headers()
                .get(HEADER_X_TS_ERROR)
                .and_then(|value| value.to_str().ok()),
            Some(ERROR_ORIGIN_UNREACHABLE)
        );

        let origin_status = JsAssetProxyIntegration::origin_status_response();
        assert_eq!(origin_status.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            origin_status
                .headers()
                .get(HEADER_X_TS_ERROR)
                .and_then(|value| value.to_str().ok()),
            Some(ERROR_ORIGIN_STATUS)
        );
    }

    #[test]
    fn build_proxy_config_forwards_only_asset_header_allowlist() {
        let mut req = build_http_request(
            Method::GET,
            "https://publisher.example.com/assets/vendor.js",
        );
        req.headers_mut().insert(
            HEADER_ACCEPT.clone(),
            http::HeaderValue::from_static("application/javascript"),
        );
        req.headers_mut().insert(
            HEADER_ACCEPT_LANGUAGE.clone(),
            http::HeaderValue::from_static("en-US"),
        );
        req.headers_mut().insert(
            HEADER_ACCEPT_ENCODING.clone(),
            http::HeaderValue::from_static("gzip, br"),
        );
        req.headers_mut().insert(
            HEADER_REFERER.clone(),
            http::HeaderValue::from_static("https://publisher.example.com/page"),
        );
        req.headers_mut().insert(
            HEADER_X_FORWARDED_FOR.clone(),
            http::HeaderValue::from_static("192.0.2.10"),
        );
        req.headers_mut().insert(
            HEADER_X_TS_EC.clone(),
            http::HeaderValue::from_static("edge-cookie-id"),
        );
        req.headers_mut()
            .insert(header::COOKIE, http::HeaderValue::from_static("session=1"));

        let config =
            JsAssetProxyIntegration::build_proxy_config("https://cdn.example.com/vendor.js", &req);

        assert!(!config.copy_request_headers);
        assert!(!config.follow_redirects);
        assert!(!config.forward_ec_id);
        assert!(config.stream_passthrough);
        assert!(config.stream_response);

        let forwarded: Vec<(String, String)> = config
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value
                        .to_str()
                        .expect("should expose header value in test")
                        .to_string(),
                )
            })
            .collect();

        assert_eq!(
            forwarded,
            vec![
                ("accept".to_string(), "application/javascript".to_string()),
                ("accept-language".to_string(), "en-US".to_string()),
                ("accept-encoding".to_string(), "gzip, br".to_string()),
                ("user-agent".to_string(), "TrustedServer/1.0".to_string()),
            ]
        );
    }

    #[test]
    fn vary_with_accept_encoding_preserves_wildcard_and_existing_value() {
        assert_eq!(
            JsAssetProxyIntegration::vary_with_accept_encoding(Some("*")),
            "*"
        );
        assert_eq!(
            JsAssetProxyIntegration::vary_with_accept_encoding(Some("Accept-Encoding")),
            "Accept-Encoding"
        );
        assert_eq!(
            JsAssetProxyIntegration::vary_with_accept_encoding(Some("Origin")),
            "Origin, Accept-Encoding"
        );
        assert_eq!(
            JsAssetProxyIntegration::vary_with_accept_encoding(None),
            "Accept-Encoding"
        );
    }

    #[test]
    fn proxy_mode_defaults_to_enabled() {
        let parsed: JsAssetProxyAsset = serde_json::from_value(json!({
            "path": "/assets/vendor.js",
            "origin_url": "https://cdn.example.com/vendor.js"
        }))
        .expect("should deserialize asset");

        assert_eq!(parsed.proxy, JsAssetProxyMode::Enabled);
    }
}
