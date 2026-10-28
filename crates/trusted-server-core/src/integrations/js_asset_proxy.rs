//! JavaScript asset proxy integration.
//!
//! This integration serves explicitly configured third-party JavaScript assets
//! from first-party paths. Each asset maps one exact publisher-facing path to
//! one exact HTTPS upstream URL and can independently enable proxying, disable
//! proxying, or block matching script tags from publisher HTML.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{Method, Request, Response, StatusCode, header};
use serde::{Deserialize, Serialize};
use url::Url;
use validator::{Validate, ValidationError, ValidationErrors};

use crate::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE, HEADER_USER_AGENT,
    HEADER_X_TS_ERROR, HEADER_X_TS_JS_ASSET_PROXY,
};
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
use crate::middleware::{
    AttributeRewrite, AttributeRewriteFn, Middleware, MiddlewareAction, MiddlewareContext,
    MiddlewarePhase,
};
use crate::platform::RuntimeServices;
use crate::proxy::{ProxyRequestConfig, proxy_request};
use crate::settings::{IntegrationConfig, Settings};

pub(crate) const JS_ASSET_PROXY_INTEGRATION_ID: &str = "js_asset_proxy";

/// The name this module is selected by, in `[proxy]`.
pub const MODULE: &str = "js_asset_proxy";

/// The builder the registry runs when a section selects [`MODULE`].
pub(crate) const BUILDER: crate::integrations::IntegrationBuilder =
    crate::integrations::IntegrationBuilder::new(
        JS_ASSET_PROXY_INTEGRATION_ID,
        crate::integrations::CORE_SOURCE,
        register,
        validate,
    )
    .with_module_name(MODULE)
    .selected_in("proxy");
const JS_ASSET_CONTENT_TYPE: &str = "application/javascript; charset=utf-8";
const X_CONTENT_TYPE_OPTIONS_NOSNIFF: &str = "nosniff";
const ERROR_ORIGIN_UNREACHABLE: &str = "js-asset-origin-unreachable";
const ERROR_ORIGIN_STATUS: &str = "js-asset-origin-status";

/// Configuration for the JavaScript asset proxy integration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsAssetProxyConfig {
    /// Optional downstream cache TTL override for every asset.
    #[serde(default)]
    pub cache_ttl_seconds: Option<u32>,
    /// JavaScript assets managed by this integration.
    #[serde(default)]
    pub assets: Vec<JsAssetProxyAsset>,
}

/// One configured JavaScript asset mapping.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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

impl JsAssetProxyConfig {
    fn normalize_origin_urls(&mut self) {
        for asset in &mut self.assets {
            if let Some(origin_url) = normalize_origin_url(&asset.origin_url) {
                asset.origin_url = origin_url;
            }
        }
    }
}

impl IntegrationConfig for JsAssetProxyConfig {}

impl Validate for JsAssetProxyConfig {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        errors.merge_self("assets", self.assets.validate());

        // A selected integration with nothing to serve is a configuration
        // mistake, so the block has to list at least one asset.
        if self.assets.is_empty() {
            errors.add("assets", ValidationError::new("empty_assets"));
        }

        let mut paths = HashSet::new();
        let mut origin_urls = HashSet::new();
        for asset in &self.assets {
            if !paths.insert(asset.path.as_str()) {
                errors.add("asset_path", ValidationError::new("duplicate_asset_path"));
            }
            let origin_url =
                normalize_origin_url(&asset.origin_url).unwrap_or_else(|| asset.origin_url.clone());
            if !origin_urls.insert(origin_url) {
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
        if self.path == "/" {
            errors.add("path", ValidationError::new("path_must_not_be_root"));
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

fn normalize_origin_url(origin_url: &str) -> Option<String> {
    let mut url = Url::parse(origin_url).ok()?;
    let has_default_port = matches!(
        (url.scheme(), url.port()),
        ("http", Some(80)) | ("https", Some(443))
    );
    if has_default_port {
        url.set_port(None).ok()?;
    }

    Some(url.to_string())
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

    normalize_origin_url(&candidate)
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
        request_scheme: &str,
    ) -> Option<&JsAssetProxyAsset> {
        self.asset_for_origin_url(script_src).or_else(|| {
            let normalized_src = normalize_script_src(script_src, request_scheme)?;
            self.asset_for_origin_url(&normalized_src)
        })
    }

    /// What happens to a script whose address is `script_src`, on a page
    /// served over `request_scheme`.
    fn rewrite_script_address(
        &self,
        script_src: &str,
        request_scheme: &str,
    ) -> AttributeRewriteAction {
        let Some(asset) = self.asset_for_script_src(script_src, request_scheme) else {
            return AttributeRewriteAction::keep();
        };

        match asset.proxy {
            JsAssetProxyMode::Enabled => AttributeRewriteAction::replace(asset.path.clone()),
            JsAssetProxyMode::Disabled => AttributeRewriteAction::keep(),
            JsAssetProxyMode::Blocked => AttributeRewriteAction::remove_element(),
        }
    }

    fn build_proxy_config<'a>(
        origin_url: &'a str,
        req: &Request<EdgeBody>,
    ) -> ProxyRequestConfig<'a> {
        let mut config = ProxyRequestConfig::new(origin_url)
            .with_streaming()
            .with_stream_response()
            .without_forward_headers()
            .without_ec_id();
        config.follow_redirects = false;

        for header_name in [
            &HEADER_ACCEPT,
            &HEADER_ACCEPT_LANGUAGE,
            &HEADER_ACCEPT_ENCODING,
            &header::IF_NONE_MATCH,
            &header::IF_MODIFIED_SINCE,
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

    fn combined_header_values(
        headers: &http::HeaderMap,
        header_name: &http::header::HeaderName,
    ) -> Option<String> {
        let values = headers
            .get_all(header_name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();

        (!values.is_empty()).then(|| values.join(", "))
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
        let content_encoding = parts.headers.get(header::CONTENT_ENCODING).cloned();
        let etag = parts.headers.get(header::ETAG).cloned();
        let last_modified = parts.headers.get(header::LAST_MODIFIED).cloned();
        let upstream_vary = Self::combined_header_values(&parts.headers, &header::VARY);
        let upstream_cache_control =
            Self::combined_header_values(&parts.headers, &header::CACHE_CONTROL);
        let body = if status == StatusCode::NOT_MODIFIED {
            EdgeBody::empty()
        } else {
            body
        };

        let mut finalized = Response::new(body);
        *finalized.status_mut() = status;
        finalized.headers_mut().insert(
            HEADER_X_TS_JS_ASSET_PROXY,
            http::HeaderValue::from_static("true"),
        );
        // Upstream bytes are served from the publisher origin, so the upstream
        // cannot choose a document MIME type or opt into browser MIME sniffing.
        finalized.headers_mut().insert(
            header::CONTENT_TYPE,
            http::HeaderValue::from_static(JS_ASSET_CONTENT_TYPE),
        );
        finalized.headers_mut().insert(
            header::X_CONTENT_TYPE_OPTIONS,
            http::HeaderValue::from_static(X_CONTENT_TYPE_OPTIONS_NOSNIFF),
        );

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
            finalized.headers_mut().insert(
                header::CACHE_CONTROL,
                http::HeaderValue::from_str(&cache_control)
                    .expect("should preserve JS asset proxy upstream Cache-Control header"),
            );
        }

        finalized
    }
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<JsAssetProxyIntegration>>, Report<TrustedServerError>> {
    let Some(mut config) = settings.module_config::<JsAssetProxyConfig>(MODULE)? else {
        return Ok(None);
    };
    config.normalize_origin_urls();

    Ok(Some(JsAssetProxyIntegration::new(config)))
}

/// Validates the JavaScript asset proxy configuration for deployment and
/// reports whether a section selects the integration's module.
///
/// # Errors
///
/// Returns an error when the configuration cannot be parsed or fails
/// validation.
pub(crate) fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    settings
        .module_config::<JsAssetProxyConfig>(MODULE)
        .map(|config| config.is_some())
}

/// Register the JavaScript asset proxy integration.
///
/// # Errors
///
/// Returns an error when the integration runs with invalid configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(JS_ASSET_PROXY_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_middleware(Arc::new(ScriptAddress(integration)))
            .build(),
    ))
}

impl JsAssetProxyIntegration {
    async fn route(
        &self,
        req: Request<EdgeBody>,
        settings: &Settings,
        services: &RuntimeServices,
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

        if !response.status().is_success() && response.status() != StatusCode::NOT_MODIFIED {
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
        call: crate::module_context::ModuleCall<'_>,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        call.inject_with(self, req, Self::route)?.await
    }
}

/// Points a configured script at its first-party path, or removes one that
/// is blocked, on the pages a `[[fetch]]` entry names [`MODULE`] for.
///
/// It is asked about a script's address as the middleware named before it
/// left it, so it is named first where it has to see the address the page
/// wrote.
struct ScriptAddress(Arc<JsAssetProxyIntegration>);

impl Middleware for ScriptAddress {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let integration = Arc::clone(&self.0);
        let request_scheme = context.request_scheme.to_owned();
        let decide: Rc<AttributeRewriteFn> = Rc::new(move |matched| {
            integration.rewrite_script_address(matched.value, &request_scheme)
        });
        MiddlewareAction {
            element_handlers: vec![Box::new(AttributeRewrite::matching(
                "script[src]",
                "src",
                decide,
            ))],
            ..MiddlewareAction::pass()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    use crate::constants::{HEADER_REFERER, HEADER_X_FORWARDED_FOR, HEADER_X_TS_EC};
    use crate::html_processor::test_support::{place_on_every_page, processed_page};
    use crate::html_processor::{
        BodyCloseInjection, HtmlProcessorConfig, create_html_processor_with_middleware,
    };
    use crate::integrations::registry_test_support::tag_fixture as tag;
    use crate::integrations::{AttributeRewriteAction, IntegrationRegistry};
    use crate::middleware::MiddlewareChain;
    use crate::platform::test_support::{StubHttpClient, build_services_with_http_client};
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
            cache_ttl_seconds: None,
            assets,
        }
    }

    /// The page a reader of `html` receives when `integration`'s middleware
    /// alone runs on it.
    fn process_html_with_integration(
        html: &str,
        integration: Arc<JsAssetProxyIntegration>,
    ) -> String {
        let chain = MiddlewareChain::new(
            MiddlewarePhase::Fetch,
            vec![Arc::new(ScriptAddress(integration))],
        );
        let config = HtmlProcessorConfig {
            csp_nonce_observed: None,
            body_close: BodyCloseInjection::None,
            origin_host: "origin.example.com".to_string(),
            request_host: "publisher.example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::default(),
            ad_slots_script: None,
            ad_bids_state: Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            request_state: crate::integrations::IntegrationRequestState::default(),
            permissions_script: None,
        };
        let (processor, _) = create_html_processor_with_middleware(config, &chain, None)
            .expect("should plan the asset proxy's middleware");
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

    /// The page a reader of `html` receives from a deployment running
    /// `settings`.
    fn process_html_with_settings(html: &str, settings: &Settings) -> String {
        let registry = IntegrationRegistry::new(settings).expect("should build registry");
        processed_page(settings, &registry, html, 8192)
    }

    /// Settings that run the tag stand-in and proxy its script in `mode`,
    /// with the two middleware named in the order given.
    fn settings_proxying_the_tag(mode: &str, middleware: &[&str]) -> Settings {
        let mut settings = create_test_settings();
        settings.select_module("testing", tag::MODULE);
        settings
            .insert_module_config(
                "proxy",
                "js_asset_proxy",
                &json!({
                    "assets": [{
                        "path": "/assets/sdk.js",
                        "origin_url": tag::SCRIPT_URL,
                        "proxy": mode
                    }]
                }),
            )
            .expect("should insert JS asset proxy config");
        place_on_every_page(&mut settings, MiddlewarePhase::Fetch, middleware);
        settings
    }

    #[test]
    fn an_integration_that_is_not_named_registers_no_routes() {
        let settings = create_test_settings();

        let registry = IntegrationRegistry::new(&settings).expect("should build registry");

        assert!(
            !registry.has_route(&Method::GET, "/assets/vendor.js"),
            "an integration that is not named should register no asset route"
        );
    }

    #[test]
    fn enabled_config_requires_at_least_one_asset() {
        let config = JsAssetProxyConfig {
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

        assert!(matches!(
            integration.rewrite_script_address("https://cdn.example.com/enabled.js", "https"),
            AttributeRewriteAction::Replace(ref value) if value == "/assets/enabled.js"
        ));
        assert!(matches!(
            integration.rewrite_script_address("https://cdn.example.com/disabled.js", "https"),
            AttributeRewriteAction::Keep
        ));
        assert!(matches!(
            integration.rewrite_script_address("https://cdn.example.com/blocked.js", "https"),
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

        assert!(matches!(
            integration.rewrite_script_address("https://cdn.example.com/vendor.js?v=1", "https"),
            AttributeRewriteAction::Keep
        ));
        assert!(matches!(
            integration.rewrite_script_address("https://cdn.example.com/other.js", "https"),
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
    fn a_script_tag_written_in_upper_case_is_a_script() {
        let integration = JsAssetProxyIntegration::new(config_with_assets(vec![asset(
            "/assets/enabled.js",
            "https://cdn.example.com/enabled.js",
            JsAssetProxyMode::Enabled,
        )]));

        let processed = process_html_with_integration(
            r#"<html><body><SCRIPT SRC="https://cdn.example.com/enabled.js"></SCRIPT></body></html>"#,
            integration,
        );

        assert!(
            processed.contains(r#"SRC="/assets/enabled.js""#),
            "should rewrite a script whatever case its tag is written in: {processed}"
        );
    }

    #[test]
    fn script_src_matching_normalizes_common_browser_url_forms() {
        let integration = JsAssetProxyIntegration::new(config_with_assets(vec![asset(
            "/assets/vendor.js",
            "https://cdn.example.com/vendor.js",
            JsAssetProxyMode::Enabled,
        )]));

        for script_src in [
            "//cdn.example.com/vendor.js",
            "HTTPS://CDN.EXAMPLE.COM/vendor.js",
            "https://cdn.example.com:443/vendor.js",
        ] {
            assert!(
                matches!(
                    integration.rewrite_script_address(script_src, "https"),
                    AttributeRewriteAction::Replace(ref value) if value == "/assets/vendor.js"
                ),
                "script src {script_src} should normalize to the configured origin URL"
            );
        }
    }

    #[test]
    fn the_asset_proxy_named_first_rewrites_a_script_ahead_of_a_module() {
        let settings = settings_proxying_the_tag("enabled", &[MODULE, tag::MODULE]);
        let html = format!(
            r#"<html><body><script src="{}"></script></body></html>"#,
            tag::SCRIPT_URL
        );

        let processed = process_html_with_settings(&html, &settings);

        assert!(
            processed.contains(r#"<script src="/assets/sdk.js"></script>"#),
            "JS asset proxy should rewrite before the module named after it: {processed}"
        );
        assert!(
            !processed.contains(tag::FIRST_PARTY_SCRIPT),
            "the module's own rewrite should not override JS asset proxy"
        );
    }

    #[test]
    fn a_module_named_ahead_of_the_asset_proxy_rewrites_the_script_first() {
        let settings = settings_proxying_the_tag("enabled", &[tag::MODULE, MODULE]);
        let html = format!(
            r#"<html><body><script src="{}"></script></body></html>"#,
            tag::SCRIPT_URL
        );

        let processed = process_html_with_settings(&html, &settings);

        assert!(
            processed.contains(&format!(
                r#"<script src="{}"></script>"#,
                tag::FIRST_PARTY_SCRIPT
            )),
            "the module named first should rewrite the script: {processed}"
        );
        assert!(
            !processed.contains("/assets/sdk.js"),
            "JS asset proxy should not know an address the module has already moved"
        );
    }

    #[test]
    fn the_asset_proxy_named_first_blocks_a_script_ahead_of_a_module() {
        let settings = settings_proxying_the_tag("blocked", &[MODULE, tag::MODULE]);
        let html = format!(
            r#"<html><body><script src="{}">vendor.cmd.push(() => {{}});</script></body></html>"#,
            tag::SCRIPT_URL
        );

        let processed = process_html_with_settings(&html, &settings);

        assert!(
            !processed.contains("vendor.cmd"),
            "blocked JS asset should remove the script element before the module can rewrite it"
        );
        assert!(
            !processed.contains(tag::FIRST_PARTY_SCRIPT),
            "the module's own rewrite should not keep a blocked script"
        );
    }

    #[test]
    fn disabled_js_asset_proxy_candidate_allows_native_rewriters() {
        let settings = settings_proxying_the_tag("disabled", &[MODULE, tag::MODULE]);
        let html = format!(
            r#"<html><body><script src="{}"></script></body></html>"#,
            tag::SCRIPT_URL
        );

        let processed = process_html_with_settings(&html, &settings);

        assert!(
            processed.contains(&format!(
                r#"<script src="{}"></script>"#,
                tag::FIRST_PARTY_SCRIPT
            )),
            "disabled JS asset proxy entries should not suppress a module's own rewrite"
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
            "/",
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
            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true

            [request_signing]
            enabled = false

            [proxy]
            modules = ["js_asset_proxy"]

            [[proxy.js_asset_proxy.assets]]
            path = "/assets/vendor.js"
            origin_url = "https://cdn.example.com/vendor.js"
            proxy = "passthrough"
        "#;
        let settings = Settings::from_toml(toml).expect("should parse settings TOML");

        assert!(
            settings
                .module_config::<JsAssetProxyConfig>(MODULE)
                .is_err(),
            "unknown proxy mode should fail deserialization"
        );
    }

    #[test]
    fn exact_configured_routes_are_registered() {
        let mut settings = create_test_settings();
        settings
            .insert_module_config(
                "proxy",
                "js_asset_proxy",
                &json!({
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
    fn successful_response_preserves_body_and_controls_expected_headers() {
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
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
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
            Some(JS_ASSET_CONTENT_TYPE)
        );
        assert_eq!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|value| value.to_str().ok()),
            Some(X_CONTENT_TYPE_OPTIONS_NOSNIFF)
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
        let body = futures::executor::block_on(response.into_body().into_bytes_bounded(1024))
            .expect("should read finalized JS asset body");
        assert_eq!(
            body.to_vec(),
            b"console.log('ok');".to_vec(),
            "should preserve upstream body bytes"
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
    fn configured_origin_urls_are_canonicalized_for_matching_and_duplicates() {
        let mut settings = create_test_settings();
        settings
            .insert_module_config(
                "proxy",
                "js_asset_proxy",
                &json!({
                    "assets": [{
                        "path": "/assets/vendor.js",
                        "origin_url": "HTTPS://CDN.EXAMPLE.COM:443/vendor.js"
                    }]
                }),
            )
            .expect("should insert integration config");
        place_on_every_page(&mut settings, MiddlewarePhase::Fetch, &[MODULE]);
        let processed = process_html_with_settings(
            r#"<script src="https://cdn.example.com/vendor.js"></script>"#,
            &settings,
        );

        assert!(processed.contains(r#"<script src="/assets/vendor.js"></script>"#));

        let config = config_with_assets(vec![
            asset(
                "/assets/one.js",
                "https://cdn.example.com/vendor.js",
                JsAssetProxyMode::Enabled,
            ),
            asset(
                "/assets/two.js",
                "HTTPS://CDN.EXAMPLE.COM:443/vendor.js",
                JsAssetProxyMode::Enabled,
            ),
        ]);
        assert!(
            config.validate().is_err(),
            "canonical duplicate origin URLs should be rejected"
        );
    }

    #[test]
    fn finalize_asset_response_preserves_repeated_vary_and_cache_control_headers() {
        let configured_asset = asset(
            "/assets/vendor.js",
            "https://cdn.example.com/vendor.js",
            JsAssetProxyMode::Enabled,
        );
        let integration =
            JsAssetProxyIntegration::new(config_with_assets(vec![configured_asset.clone()]));
        let upstream = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_ENCODING, "gzip")
            .header(header::VARY, "Origin")
            .header(header::VARY, "User-Agent")
            .header(header::CACHE_CONTROL, "public, max-age=60")
            .header(header::CACHE_CONTROL, "immutable")
            .body(EdgeBody::from("body"))
            .expect("should build upstream JS asset response");

        let response = integration.finalize_asset_response(&configured_asset, upstream);

        assert_eq!(
            response
                .headers()
                .get(header::VARY)
                .and_then(|value| value.to_str().ok()),
            Some("Origin, User-Agent, Accept-Encoding")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=60, immutable")
        );
    }

    #[test]
    fn handle_forwards_conditional_headers_and_preserves_not_modified() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let integration = JsAssetProxyIntegration::new(config_with_assets(vec![asset(
                "/assets/vendor.js",
                "https://cdn.example.com/vendor.js",
                JsAssetProxyMode::Enabled,
            )]));
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response_with_headers(
                StatusCode::NOT_MODIFIED.as_u16(),
                b"must not reach the client".to_vec(),
                vec![(header::ETAG.as_str(), "\"current\"")],
            );
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let mut request = build_http_request(
                Method::GET,
                "https://publisher.example.com/assets/vendor.js",
            );
            request.headers_mut().insert(
                header::IF_NONE_MATCH,
                http::HeaderValue::from_static("\"current\""),
            );
            request.headers_mut().insert(
                header::IF_MODIFIED_SINCE,
                http::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
            );

            let response = integration
                .route(request, &settings, &services)
                .await
                .expect("should proxy conditional asset response");

            assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
            assert_eq!(
                response
                    .headers()
                    .get(header::ETAG)
                    .and_then(|value| value.to_str().ok()),
                Some("\"current\"")
            );
            let response_body = response
                .into_body()
                .into_bytes_bounded(1024)
                .await
                .expect("should collect not-modified JS asset body");
            assert!(
                response_body.is_empty(),
                "not-modified responses should not contain a body"
            );
            let request_headers = stub.recorded_request_headers();
            assert_eq!(request_headers.len(), 1);
            assert!(request_headers[0].iter().any(|(name, value)| {
                name == header::IF_NONE_MATCH.as_str() && value == "\"current\""
            }));
            assert!(request_headers[0].iter().any(|(name, value)| {
                name == header::IF_MODIFIED_SINCE.as_str()
                    && value == "Wed, 21 Oct 2015 07:28:00 GMT"
            }));
        });
    }

    #[test]
    fn handle_maps_upstream_outcomes_and_respects_streaming_capability() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let integration = JsAssetProxyIntegration::new(config_with_assets(vec![asset(
                "/assets/vendor.js",
                "https://cdn.example.com/vendor.js",
                JsAssetProxyMode::Enabled,
            )]));

            let buffered_stub = Arc::new(StubHttpClient::new());
            buffered_stub.set_streaming_responses_supported(false);
            buffered_stub.push_response(200, b"ok".to_vec());
            let buffered_services = build_services_with_http_client(
                Arc::clone(&buffered_stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let success = integration
                .route(
                    build_http_request(
                        Method::GET,
                        "https://publisher.example.com/assets/vendor.js",
                    ),
                    &settings,
                    &buffered_services,
                )
                .await
                .expect("should proxy buffered asset response");
            assert_eq!(success.status(), StatusCode::OK);
            assert_eq!(buffered_stub.recorded_stream_response_flags(), vec![false]);

            let streaming_stub = Arc::new(StubHttpClient::new());
            streaming_stub.set_streaming_responses_supported(true);
            streaming_stub.push_streaming_response(200, b"streamed".to_vec());
            let streaming_services = build_services_with_http_client(
                Arc::clone(&streaming_stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let success = integration
                .route(
                    build_http_request(
                        Method::GET,
                        "https://publisher.example.com/assets/vendor.js",
                    ),
                    &settings,
                    &streaming_services,
                )
                .await
                .expect("should proxy streaming asset response");
            assert_eq!(success.status(), StatusCode::OK);
            assert_eq!(streaming_stub.recorded_stream_response_flags(), vec![true]);
            assert!(
                matches!(success.body(), EdgeBody::Stream(_)),
                "streaming-capable clients should preserve the origin response stream"
            );
            let streamed_body = success
                .into_body()
                .into_bytes_bounded(1024)
                .await
                .expect("should collect streamed JS asset body");
            assert_eq!(streamed_body.as_ref(), b"streamed");

            let unavailable_stub = Arc::new(StubHttpClient::new());
            let unavailable_services = build_services_with_http_client(
                Arc::clone(&unavailable_stub) as Arc<dyn crate::platform::PlatformHttpClient>,
            );
            let unavailable = integration
                .route(
                    build_http_request(
                        Method::GET,
                        "https://publisher.example.com/assets/vendor.js",
                    ),
                    &settings,
                    &unavailable_services,
                )
                .await
                .expect("should map unavailable origin response");
            assert_eq!(unavailable.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(
                unavailable
                    .headers()
                    .get(HEADER_X_TS_ERROR)
                    .and_then(|value| value.to_str().ok()),
                Some(ERROR_ORIGIN_UNREACHABLE)
            );

            let status_stub = Arc::new(StubHttpClient::new());
            status_stub.push_response(404, Vec::new());
            let status_services = build_services_with_http_client(
                Arc::clone(&status_stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let status = integration
                .route(
                    build_http_request(
                        Method::GET,
                        "https://publisher.example.com/assets/vendor.js",
                    ),
                    &settings,
                    &status_services,
                )
                .await
                .expect("should map non-success origin response");
            assert_eq!(status.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(
                status
                    .headers()
                    .get(HEADER_X_TS_ERROR)
                    .and_then(|value| value.to_str().ok()),
                Some(ERROR_ORIGIN_STATUS)
            );
        });
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

    #[test]
    fn rejects_unknown_asset_fields() {
        let error = serde_json::from_value::<JsAssetProxyAsset>(json!({
            "path": "/assets/vendor.js",
            "origin_url": "https://cdn.example.com/vendor.js",
            "mode": "blocked"
        }))
        .expect_err("should reject unknown asset fields");

        assert!(
            error.to_string().contains("unknown field `mode`"),
            "should identify the unknown asset field"
        );
    }

    /// The page a reader receives with the script proxy running, kept as a
    /// file so that changing how the page change is made can be shown to
    /// leave the page as it was.
    #[test]
    fn the_page_a_reader_receives_is_the_recorded_one() {
        crate::html_processor::test_support::assert_page_is_recorded(
            include_str!("fixtures/js-asset-proxy.settings.toml"),
            &[],
            include_str!("fixtures/js-asset-proxy.input.html"),
            include_str!("fixtures/js-asset-proxy.recorded.html"),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/integrations/fixtures/js-asset-proxy.recorded.html"
            ),
        );
    }
}
