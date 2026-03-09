//! Google Publisher Tags (GPT) integration for first-party ad serving.
//!
//! This module provides transparent proxying for Google's entire GPT script
//! chain, enabling first-party ad tag delivery while maintaining privacy
//! controls. GPT loads scripts in a cascade:
//!
//! 1. `gpt.js` – the thin bootstrap loader
//! 2. `pubads_impl.js` – the main GPT implementation (~640 KB)
//! 3. `pubads_impl_*.js` – lazy-loaded sub-modules (page-level ads, side rails, …)
//! 4. Auxiliary scripts – viewability, monitoring, error reporting
//!
//! All of these are served from `securepubads.g.doubleclick.net`. The
//! integration proxies these scripts through the publisher's domain
//! while a client-side shim intercepts dynamic script insertions and
//! rewrites their URLs to the first-party proxy so that every
//! subsequent fetch in the cascade routes back through the trusted
//! server.
//!
//! ## How It Works
//!
//! 1. **HTML rewriting** – The [`IntegrationAttributeRewriter`] swaps `src`/`href`
//!    attributes pointing at Google's GPT script with a first-party URL
//!    (`/integrations/gpt/script`).
//! 2. **Script proxy** – [`IntegrationProxy`] endpoints serve `gpt.js`
//!    (`/integrations/gpt/script`) and all secondary scripts
//!    (`/integrations/gpt/pagead/*`, `/integrations/gpt/tag/*`) through the
//!    publisher's domain. Script bodies are served **verbatim** — no
//!    server-side domain rewriting is performed.
//! 3. **Client-side shim** – A TypeScript module (built into the unified TSJS
//!    bundle) installs a script guard that intercepts dynamically inserted GPT
//!    `<script>` elements and rewrites their URLs to the first-party proxy.
//!    This is the sole mechanism that routes the GPT cascade through the proxy.
//!    The shim also hooks into the `googletag` API for targeting injection.

use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::header;
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use url::Url;
use validator::Validate;

use crate::constants::{HEADER_ACCEPT_ENCODING, HEADER_X_FORWARDED_FOR};
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration,
};
use crate::proxy::{proxy_request, ProxyRequestConfig};
use crate::settings::{IntegrationConfig, Settings};

const GPT_INTEGRATION_ID: &str = "gpt";

/// Primary Google domain that serves GPT scripts.
const SECUREPUBADS_HOST: &str = "securepubads.g.doubleclick.net";

/// Integration route prefix for all GPT proxy endpoints.
const ROUTE_PREFIX: &str = "/integrations/gpt";

/// Configuration for the Google Publisher Tags integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct GptConfig {
    /// Enable/disable the integration.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// URL for the GPT bootstrap script (default: Google's CDN).
    #[serde(default = "default_script_url")]
    #[validate(url)]
    pub script_url: String,

    /// Cache TTL for proxied GPT scripts in seconds (default: 3600 = 1 hour).
    #[serde(default = "default_cache_ttl")]
    #[validate(range(min = 60, max = 86400))]
    pub cache_ttl_seconds: u32,

    /// Whether to rewrite GPT script URLs in publisher HTML.
    #[serde(default = "default_rewrite_script")]
    pub rewrite_script: bool,
}

impl IntegrationConfig for GptConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Google Publisher Tags integration implementation.
///
/// Proxies the full GPT script cascade through first-party endpoints.
/// Script bodies are served verbatim; the client-side GPT shim handles
/// URL rewriting so that every script in the cascade routes back through
/// the trusted server.
pub struct GptIntegration {
    config: GptConfig,
}

impl GptIntegration {
    fn new(config: GptConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: GPT_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    /// Build the upstream URL for a proxied GPT request.
    ///
    /// Strips the integration prefix from the request path and constructs
    /// a full URL on the GPT host, preserving the original path and query.
    ///
    /// Returns `None` if `request_path` does not start with [`ROUTE_PREFIX`].
    fn build_upstream_url(request_path: &str, query: Option<&str>) -> Option<String> {
        let upstream_path = request_path.strip_prefix(ROUTE_PREFIX)?;
        let query_part = query.map(|q| format!("?{}", q)).unwrap_or_default();
        Some(format!(
            "https://{SECUREPUBADS_HOST}{upstream_path}{query_part}"
        ))
    }

    fn build_proxy_config<'a>(target_url: &'a str, req: &Request) -> ProxyRequestConfig<'a> {
        let mut config = ProxyRequestConfig::new(target_url).with_streaming();
        config.forward_synthetic_id = false;
        config = config.with_header(
            header::USER_AGENT,
            fastly::http::HeaderValue::from_static("TrustedServer/1.0"),
        );
        config = config.with_header(
            HEADER_ACCEPT_ENCODING,
            req.get_header(HEADER_ACCEPT_ENCODING)
                .cloned()
                .unwrap_or_else(|| fastly::http::HeaderValue::from_static("")),
        );
        config = config.with_header(header::REFERER, fastly::http::HeaderValue::from_static(""));
        config.with_header(
            HEADER_X_FORWARDED_FOR,
            fastly::http::HeaderValue::from_static(""),
        )
    }

    fn finalize_gpt_asset_response(&self, mut response: Response) -> Response {
        response.set_header("X-GPT-Proxy", "true");

        if response.get_status().is_success() {
            response.set_header(
                header::CACHE_CONTROL,
                format!("public, max-age={}", self.config.cache_ttl_seconds),
            );
        }

        response
    }

    async fn proxy_gpt_asset(
        &self,
        settings: &Settings,
        req: Request,
        target_url: &str,
        context: &str,
    ) -> Result<Response, Report<TrustedServerError>> {
        let config = Self::build_proxy_config(target_url, &req);
        let response = proxy_request(settings, req, config)
            .await
            .change_context(Self::error(context))?;

        // Preserve upstream non-2xx statuses so GPT failures remain visible to
        // callers. Only successful responses receive a cache directive.
        Ok(self.finalize_gpt_asset_response(response))
    }

    /// Check if a URL points at Google's GPT bootstrap script (`gpt.js`).
    ///
    /// Only matches the canonical host:
    /// - `securepubads.g.doubleclick.net/tag/js/gpt.js`
    ///
    /// This matcher is intentionally strict and only controls HTML attribute
    /// rewriting for the initial bootstrap tag. The `script_url` config option
    /// still controls which upstream URL `/integrations/gpt/script` fetches.
    fn is_gpt_script_url(url: &str) -> bool {
        let parsed = Url::parse(url).or_else(|_| {
            let stripped = url
                .strip_prefix("//")
                .ok_or(url::ParseError::RelativeUrlWithoutBase)?;
            Url::parse(&format!("https://{stripped}"))
        });

        let Ok(parsed) = parsed else {
            return false;
        };

        let Some(host) = parsed.host_str() else {
            return false;
        };

        host.eq_ignore_ascii_case(SECUREPUBADS_HOST)
            && parsed.path().eq_ignore_ascii_case("/tag/js/gpt.js")
    }

    /// Fetch and serve the GPT bootstrap script (`gpt.js`).
    ///
    /// The script body is served verbatim — domain rewriting for the
    /// cascade (`pubads_impl`, sub-modules, etc.) is handled client-side
    /// by the GPT script guard shim.
    async fn handle_script_serving(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let script_url = &self.config.script_url;
        log::info!("Fetching GPT script from: {}", script_url);
        self.proxy_gpt_asset(
            settings,
            req,
            script_url,
            &format!("Failed to fetch GPT script from {script_url}"),
        )
        .await
    }

    /// Proxy a secondary GPT script (anything under `/pagead/*` or `/tag/*`).
    ///
    /// Requests to `/integrations/gpt/pagead/…` (or `/tag/…`) are forwarded
    /// to `securepubads.g.doubleclick.net/…` and served verbatim. The
    /// client-side GPT script guard handles URL rewriting for subsequent
    /// cascade loads.
    async fn handle_pagead_proxy(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();
        let query = req.get_url().query();

        let target_url = Self::build_upstream_url(original_path, query)
            .ok_or_else(|| Self::error(format!("Invalid GPT pagead path: {}", original_path)))?;

        log::info!("GPT proxy: forwarding to {}", target_url);
        self.proxy_gpt_asset(
            settings,
            req,
            &target_url,
            &format!("Failed to fetch GPT resource from {target_url}"),
        )
        .await
    }
}

fn build(settings: &Settings) -> Option<Arc<GptIntegration>> {
    let config = match settings.integration_config::<GptConfig>(GPT_INTEGRATION_ID) {
        Ok(Some(config)) => config,
        Ok(None) => {
            log::debug!("[gpt] Integration disabled or not configured");
            return None;
        }
        Err(err) => {
            log::error!("Failed to load GPT integration config: {err:?}");
            return None;
        }
    };

    Some(GptIntegration::new(config))
}

/// Register the GPT integration.
#[must_use]
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;
    Some(
        IntegrationRegistration::builder(GPT_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration.clone())
            .with_head_injector(integration)
            .build(),
    )
}

#[async_trait(?Send)]
impl IntegrationProxy for GptIntegration {
    fn integration_name(&self) -> &'static str {
        GPT_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            self.get("/script"),
            self.get("/pagead/*"),
            self.get("/tag/*"),
        ]
    }

    async fn handle(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path();

        if path == "/integrations/gpt/script" {
            self.handle_script_serving(settings, req).await
        } else if path.starts_with("/integrations/gpt/pagead/")
            || path.starts_with("/integrations/gpt/tag/")
        {
            self.handle_pagead_proxy(settings, req).await
        } else {
            Err(Report::new(Self::error(format!(
                "Unknown GPT route: {}",
                path
            ))))
        }
    }
}

impl IntegrationAttributeRewriter for GptIntegration {
    fn integration_id(&self) -> &'static str {
        GPT_INTEGRATION_ID
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        self.config.rewrite_script && matches!(attribute, "src" | "href")
    }

    fn rewrite(
        &self,
        _attr_name: &str,
        attr_value: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if !self.config.rewrite_script {
            return AttributeRewriteAction::keep();
        }

        if Self::is_gpt_script_url(attr_value) {
            AttributeRewriteAction::replace(format!(
                "{}://{}/integrations/gpt/script",
                ctx.request_scheme, ctx.request_host
            ))
        } else {
            AttributeRewriteAction::keep()
        }
    }
}

impl IntegrationHeadInjector for GptIntegration {
    fn integration_id(&self) -> &'static str {
        GPT_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        // Set the enable flag and best-effort call the activation function
        // registered by the GPT shim module. The bundle also auto-installs
        // when it sees the pre-set flag, so this works regardless of whether
        // the inline bootstrap runs before or after the TSJS bundle.
        vec![
            "<script>window.__tsjs_gpt_enabled=true;window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>"
                .to_string(),
        ]
    }
}

// Default value functions

fn default_enabled() -> bool {
    true
}

fn default_script_url() -> String {
    "https://securepubads.g.doubleclick.net/tag/js/gpt.js".to_string()
}

fn default_cache_ttl() -> u32 {
    3600
}

fn default_rewrite_script() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationDocumentState;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::Method;

    fn test_config() -> GptConfig {
        GptConfig {
            enabled: true,
            script_url: default_script_url(),
            cache_ttl_seconds: 3600,
            rewrite_script: true,
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

    // -- URL detection --

    #[test]
    fn gpt_script_url_detection() {
        assert!(
            GptIntegration::is_gpt_script_url(
                "https://securepubads.g.doubleclick.net/tag/js/gpt.js"
            ),
            "should match the standard GPT CDN URL"
        );

        assert!(
            GptIntegration::is_gpt_script_url("//securepubads.g.doubleclick.net/tag/js/gpt.js"),
            "should match protocol-relative GPT CDN URLs"
        );

        assert!(
            GptIntegration::is_gpt_script_url(
                "https://SECUREPUBADS.G.DOUBLECLICK.NET/tag/js/gpt.js"
            ),
            "should match case-insensitively"
        );

        assert!(
            !GptIntegration::is_gpt_script_url("https://example.com/script.js"),
            "should not match unrelated URLs"
        );

        assert!(
            !GptIntegration::is_gpt_script_url(
                "https://securepubads.g.doubleclick.net/other/script.js"
            ),
            "should not match other doubleclick paths"
        );

        assert!(
            !GptIntegration::is_gpt_script_url(
                "https://cdn.example.com/loader.js?ref=securepubads.g.doubleclick.net/tag/js/gpt.js"
            ),
            "should not match when GPT host appears only in query text"
        );

        assert!(
            !GptIntegration::is_gpt_script_url(
                "https://cdn.example.com/assets/securepubads.g.doubleclick.net/tag/js/gpt.js"
            ),
            "should not match when GPT host appears only in path text"
        );
    }

    // -- Attribute rewriter --

    #[test]
    fn attribute_rewriter_rewrites_gpt_urls() {
        let integration = GptIntegration::new(test_config());
        let ctx = test_context();

        let result = integration.rewrite(
            "src",
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
            &ctx,
        );

        match result {
            AttributeRewriteAction::Replace(url) => {
                assert_eq!(
                    url, "https://edge.example.com/integrations/gpt/script",
                    "should rewrite to first-party script endpoint"
                );
            }
            other => panic!("Expected Replace action, got {:?}", other),
        }
    }

    #[test]
    fn attribute_rewriter_keeps_non_gpt_urls() {
        let integration = GptIntegration::new(test_config());
        let ctx = test_context();

        let result = integration.rewrite("src", "https://cdn.example.com/analytics.js", &ctx);

        assert_eq!(
            result,
            AttributeRewriteAction::Keep,
            "should keep non-GPT URLs unchanged"
        );
    }

    #[test]
    fn attribute_rewriter_noop_when_disabled() {
        let config = GptConfig {
            rewrite_script: false,
            ..test_config()
        };
        let integration = GptIntegration::new(config);
        let ctx = test_context();

        let result = integration.rewrite(
            "src",
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
            &ctx,
        );

        assert_eq!(
            result,
            AttributeRewriteAction::Keep,
            "should keep GPT URLs when rewrite_script is disabled"
        );
    }

    #[test]
    fn handles_attribute_respects_config() {
        let enabled = GptIntegration::new(test_config());
        assert!(
            enabled.handles_attribute("src"),
            "should handle src when rewrite_script is true"
        );
        assert!(
            enabled.handles_attribute("href"),
            "should handle href when rewrite_script is true"
        );
        assert!(
            !enabled.handles_attribute("action"),
            "should not handle action attribute"
        );

        let disabled = GptIntegration::new(GptConfig {
            rewrite_script: false,
            ..test_config()
        });
        assert!(
            !disabled.handles_attribute("src"),
            "should not handle src when rewrite_script is false"
        );
    }

    // -- GPT proxy configuration --

    #[test]
    fn build_proxy_config_uses_streaming_without_synthetic_forwarding() {
        let req = Request::new(
            Method::GET,
            "https://edge.example.com/integrations/gpt/script",
        );
        let config = GptIntegration::build_proxy_config(
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
            &req,
        );

        assert!(
            config.stream_passthrough,
            "should stream GPT assets verbatim without rewrite processing"
        );
        assert!(
            !config.forward_synthetic_id,
            "should not append synthetic_id to GPT asset requests"
        );
    }

    #[test]
    fn build_proxy_config_overrides_privacy_sensitive_headers() {
        let mut req = Request::new(
            Method::GET,
            "https://edge.example.com/integrations/gpt/script",
        );
        req.set_header(HEADER_ACCEPT_ENCODING, "gzip");

        let config = GptIntegration::build_proxy_config(
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
            &req,
        );

        let user_agent = config
            .headers
            .iter()
            .find(|(name, _)| name == header::USER_AGENT)
            .and_then(|(_, value)| value.to_str().ok());
        let referer = config
            .headers
            .iter()
            .find(|(name, _)| name == header::REFERER)
            .and_then(|(_, value)| value.to_str().ok());
        let x_forwarded_for = config
            .headers
            .iter()
            .find(|(name, _)| name == HEADER_X_FORWARDED_FOR)
            .and_then(|(_, value)| value.to_str().ok());
        let accept_encoding = config
            .headers
            .iter()
            .find(|(name, _)| name == HEADER_ACCEPT_ENCODING)
            .and_then(|(_, value)| value.to_str().ok());

        assert_eq!(
            user_agent,
            Some("TrustedServer/1.0"),
            "should use a stable user agent for GPT upstream requests"
        );
        assert_eq!(
            referer,
            Some(""),
            "should clear Referer before proxying GPT assets"
        );
        assert_eq!(
            x_forwarded_for,
            Some(""),
            "should strip X-Forwarded-For before proxying GPT assets"
        );
        assert_eq!(
            accept_encoding,
            Some("gzip"),
            "should preserve the caller Accept-Encoding for streamed GPT assets"
        );
    }

    #[test]
    fn build_proxy_config_clears_accept_encoding_when_client_omits_it() {
        let req = Request::new(
            Method::GET,
            "https://edge.example.com/integrations/gpt/script",
        );
        let config = GptIntegration::build_proxy_config(
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
            &req,
        );

        let accept_encoding = config
            .headers
            .iter()
            .find(|(name, _)| name == HEADER_ACCEPT_ENCODING)
            .and_then(|(_, value)| value.to_str().ok());

        assert_eq!(
            accept_encoding,
            Some(""),
            "should avoid advertising encodings the client did not request"
        );
    }

    #[test]
    fn finalize_gpt_asset_response_adds_proxy_headers_only_for_successes() {
        let integration = GptIntegration::new(test_config());
        let response = Response::from_status(fastly::http::StatusCode::OK);
        let response = integration.finalize_gpt_asset_response(response);

        assert_eq!(
            response.get_status(),
            fastly::http::StatusCode::OK,
            "should preserve successful upstream statuses"
        );
        assert_eq!(
            response.get_header_str("X-GPT-Proxy"),
            Some("true"),
            "should tag proxied GPT responses"
        );
        assert_eq!(
            response.get_header_str(header::CACHE_CONTROL),
            Some("public, max-age=3600"),
            "should add cache headers for successful GPT asset responses"
        );
    }

    #[test]
    fn finalize_gpt_asset_response_preserves_non_success_statuses_without_cache_headers() {
        let integration = GptIntegration::new(test_config());
        let response = Response::from_status(fastly::http::StatusCode::SERVICE_UNAVAILABLE);
        let response = integration.finalize_gpt_asset_response(response);

        assert_eq!(
            response.get_status(),
            fastly::http::StatusCode::SERVICE_UNAVAILABLE,
            "should preserve upstream non-success statuses for callers"
        );
        assert_eq!(
            response.get_header_str("X-GPT-Proxy"),
            Some("true"),
            "should still identify non-success GPT responses as proxied"
        );
        assert_eq!(
            response.get_header_str(header::CACHE_CONTROL),
            None,
            "should not cache upstream non-success GPT responses"
        );
    }

    // -- Route registration --

    #[test]
    fn routes_registered() {
        let integration = GptIntegration::new(test_config());
        let routes = integration.routes();

        assert_eq!(routes.len(), 3, "should register three routes");

        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/gpt/script" && r.method == Method::GET),
            "should register the bootstrap script endpoint"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/gpt/pagead/*" && r.method == Method::GET),
            "should register the pagead wildcard proxy"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/gpt/tag/*" && r.method == Method::GET),
            "should register the tag wildcard proxy"
        );
    }

    // -- Build / register --

    #[test]
    fn build_requires_config() {
        let settings = create_test_settings();
        assert!(
            build(&settings).is_none(),
            "should not build without integration config"
        );
    }

    #[test]
    fn build_with_valid_config() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                GPT_INTEGRATION_ID,
                &serde_json::json!({
                    "enabled": true,
                    "script_url": "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
                    "cache_ttl_seconds": 3600,
                    "rewrite_script": true
                }),
            )
            .expect("should insert GPT config");

        assert!(
            build(&settings).is_some(),
            "should build with valid integration config"
        );
    }

    #[test]
    fn build_disabled_returns_none() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                GPT_INTEGRATION_ID,
                &serde_json::json!({
                    "enabled": false
                }),
            )
            .expect("should insert GPT config");

        assert!(
            build(&settings).is_none(),
            "should not build when integration is disabled"
        );
    }

    // -- Upstream URL building --

    #[test]
    fn build_upstream_url_strips_prefix_and_preserves_path() {
        let url = GptIntegration::build_upstream_url(
            "/integrations/gpt/pagead/managed/js/gpt/m202603020101/pubads_impl.js",
            None,
        );
        assert_eq!(
            url.as_deref(),
            Some("https://securepubads.g.doubleclick.net/pagead/managed/js/gpt/m202603020101/pubads_impl.js"),
            "should strip the integration prefix and build the upstream URL"
        );
    }

    #[test]
    fn build_upstream_url_preserves_query_string() {
        let url = GptIntegration::build_upstream_url(
            "/integrations/gpt/pagead/managed/js/gpt/m202603020101/pubads_impl.js",
            Some("cb=123&foo=bar"),
        );
        assert_eq!(
            url.as_deref(),
            Some("https://securepubads.g.doubleclick.net/pagead/managed/js/gpt/m202603020101/pubads_impl.js?cb=123&foo=bar"),
            "should preserve the query string in the upstream URL"
        );
    }

    #[test]
    fn build_upstream_url_handles_tag_routes() {
        let url =
            GptIntegration::build_upstream_url("/integrations/gpt/tag/js/gpt.js", Some("v=2"));
        assert_eq!(
            url.as_deref(),
            Some("https://securepubads.g.doubleclick.net/tag/js/gpt.js?v=2"),
            "should handle /tag/* routes correctly"
        );
    }

    #[test]
    fn build_upstream_url_returns_none_for_invalid_prefix() {
        let url = GptIntegration::build_upstream_url("/some/other/path", None);
        assert!(
            url.is_none(),
            "should return None when path does not start with the integration prefix"
        );
    }

    #[test]
    fn build_upstream_url_handles_empty_path_after_prefix() {
        let url = GptIntegration::build_upstream_url("/integrations/gpt", None);
        assert_eq!(
            url.as_deref(),
            Some("https://securepubads.g.doubleclick.net"),
            "should handle path that is exactly the prefix"
        );
    }

    // -- Head injector --

    #[test]
    fn head_injector_emits_enable_flag() {
        let integration = GptIntegration::new(test_config());
        let doc_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "example.com",
            document_state: &doc_state,
        };

        let inserts = integration.head_inserts(&ctx);

        assert_eq!(inserts.len(), 1, "should emit exactly one head insert");
        assert_eq!(
            inserts[0],
            "<script>window.__tsjs_gpt_enabled=true;window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>",
            "should set the enable flag and call the GPT shim activation function"
        );
    }

    #[test]
    fn head_injector_integration_id() {
        let integration = GptIntegration::new(test_config());
        assert_eq!(
            IntegrationHeadInjector::integration_id(integration.as_ref()),
            "gpt"
        );
    }
}
