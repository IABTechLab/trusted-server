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
//! integration proxies these scripts
//! through the publisher's domain while a client-side shim intercepts
//! dynamic script insertions and rewrites their URLs to the first-party
//! proxy so that every subsequent fetch in the cascade routes back through
//! the trusted server.
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
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use url::Url;
use validator::Validate;

use crate::backend::BackendConfig;
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
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
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let script_url = &self.config.script_url;
        log::info!("Fetching GPT script from: {}", script_url);

        let mut gpt_req = Request::new(Method::GET, script_url);
        Self::copy_accept_headers(&req, &mut gpt_req);

        let backend_name = BackendConfig::from_url(script_url, true).change_context(
            Self::error("Failed to determine backend for GPT script fetch"),
        )?;

        let mut gpt_response = gpt_req
            .send(backend_name)
            .change_context(Self::error(format!(
                "Failed to fetch GPT script from {}",
                script_url
            )))?;

        if !gpt_response.get_status().is_success() {
            log::error!(
                "GPT script fetch failed with status: {}",
                gpt_response.get_status()
            );
            return Err(Report::new(Self::error(format!(
                "GPT script returned error status: {}",
                gpt_response.get_status()
            ))));
        }

        let body = gpt_response.take_body_bytes();
        log::info!("Successfully fetched GPT script: {} bytes", body.len());

        let mut response = Response::from_status(StatusCode::OK)
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
            .with_header("X-GPT-Proxy", "true")
            .with_header("X-Script-Source", script_url)
            .with_body(body);

        Self::copy_content_encoding_headers(&gpt_response, &mut response);

        Ok(response)
    }

    /// Proxy a secondary GPT script (anything under `/pagead/*` or `/tag/*`).
    ///
    /// Requests to `/integrations/gpt/pagead/…` (or `/tag/…`) are forwarded
    /// to `securepubads.g.doubleclick.net/…` and served verbatim. The
    /// client-side GPT script guard handles URL rewriting for subsequent
    /// cascade loads.
    async fn handle_pagead_proxy(
        &self,
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let original_path = req.get_path();

        // Strip the integration prefix to recover the upstream path.
        let upstream_path = original_path
            .strip_prefix(ROUTE_PREFIX)
            .ok_or_else(|| Self::error(format!("Invalid GPT pagead path: {}", original_path)))?;

        let query = req
            .get_url()
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();

        let target_url = format!("https://{SECUREPUBADS_HOST}{upstream_path}{query}");

        log::info!("GPT proxy: forwarding to {}", target_url);

        let mut upstream_req = Request::new(Method::GET, &target_url);
        Self::copy_accept_headers(&req, &mut upstream_req);

        let backend_name = BackendConfig::from_url(&format!("https://{SECUREPUBADS_HOST}"), true)
            .change_context(Self::error(
            "Failed to determine backend for GPT pagead proxy",
        ))?;

        let mut upstream_response =
            upstream_req
                .send(backend_name)
                .change_context(Self::error(format!(
                    "Failed to fetch GPT resource from {}",
                    target_url
                )))?;

        if !upstream_response.get_status().is_success() {
            log::error!(
                "GPT pagead proxy: upstream returned status {}",
                upstream_response.get_status()
            );
            return Err(Report::new(Self::error(format!(
                "GPT pagead resource returned error status: {}",
                upstream_response.get_status()
            ))));
        }

        let content_type = upstream_response
            .get_header_str(header::CONTENT_TYPE)
            .unwrap_or("")
            .to_string();

        let body = upstream_response.take_body_bytes();
        log::info!(
            "GPT pagead proxy: fetched {} bytes ({})",
            body.len(),
            content_type
        );

        let mut response = Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, &content_type)
            .with_header(
                header::CACHE_CONTROL,
                format!(
                    "public, max-age={}, immutable",
                    self.config.cache_ttl_seconds
                ),
            )
            .with_header("X-GPT-Proxy", "true")
            .with_header("X-Script-Source", &target_url)
            .with_body(body);

        Self::copy_content_encoding_headers(&upstream_response, &mut response);

        Ok(response)
    }

    /// Copy safe content-negotiation headers to the upstream request.
    fn copy_accept_headers(from: &Request, to: &mut Request) {
        to.set_header(header::USER_AGENT, "TrustedServer/1.0");

        for name in [
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
        ] {
            if let Some(value) = from.get_header(&name) {
                to.set_header(name, value);
            }
        }
    }

    fn copy_content_encoding_headers(from: &Response, to: &mut Response) {
        let Some(content_encoding) = from.get_header(header::CONTENT_ENCODING).cloned() else {
            return;
        };

        to.set_header(header::CONTENT_ENCODING, content_encoding);

        let vary = Self::vary_with_accept_encoding(from.get_header_str(header::VARY));
        to.set_header(header::VARY, vary);
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
            .with_attribute_rewriter(integration)
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
    use crate::test_support::tests::create_test_settings;

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

    // -- Request header forwarding --

    #[test]
    fn copy_accept_headers_forwards_all_negotiation_headers() {
        let mut inbound = Request::new(Method::GET, "https://publisher.example/page");
        inbound.set_header(header::ACCEPT, "application/javascript");
        inbound.set_header(header::ACCEPT_ENCODING, "br, gzip");
        inbound.set_header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9");

        let mut upstream = Request::new(
            Method::GET,
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
        );

        GptIntegration::copy_accept_headers(&inbound, &mut upstream);

        assert_eq!(
            upstream.get_header_str(header::ACCEPT),
            Some("application/javascript"),
            "should forward Accept header for content negotiation"
        );
        assert_eq!(
            upstream.get_header_str(header::ACCEPT_ENCODING),
            Some("br, gzip"),
            "should forward Accept-Encoding from the client"
        );
        assert_eq!(
            upstream.get_header_str(header::ACCEPT_LANGUAGE),
            Some("en-US,en;q=0.9"),
            "should forward Accept-Language header for locale negotiation"
        );
        assert_eq!(
            upstream.get_header_str(header::USER_AGENT),
            Some("TrustedServer/1.0"),
            "should set a stable user agent for GPT upstream requests"
        );
    }

    // -- Response header forwarding --

    #[test]
    fn copy_content_encoding_headers_sets_encoding_and_vary() {
        let upstream = Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_ENCODING, "br")
            .with_header(header::VARY, "Accept-Language");
        let mut downstream = Response::from_status(StatusCode::OK);

        GptIntegration::copy_content_encoding_headers(&upstream, &mut downstream);

        assert_eq!(
            downstream.get_header_str(header::CONTENT_ENCODING),
            Some("br"),
            "should forward Content-Encoding when upstream response is encoded"
        );
        assert_eq!(
            downstream.get_header_str(header::VARY),
            Some("Accept-Language, Accept-Encoding"),
            "should include Accept-Encoding in Vary when forwarding encoded responses"
        );
    }

    #[test]
    fn copy_content_encoding_headers_preserves_existing_accept_encoding_vary() {
        let upstream = Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_ENCODING, "gzip")
            .with_header(header::VARY, "Origin, Accept-Encoding");
        let mut downstream = Response::from_status(StatusCode::OK);

        GptIntegration::copy_content_encoding_headers(&upstream, &mut downstream);

        assert_eq!(
            downstream.get_header_str(header::VARY),
            Some("Origin, Accept-Encoding"),
            "should preserve existing Vary value when Accept-Encoding is already present"
        );
    }

    #[test]
    fn copy_content_encoding_headers_skips_unencoded_responses() {
        let upstream = Response::from_status(StatusCode::OK).with_header(header::VARY, "Origin");
        let mut downstream = Response::from_status(StatusCode::OK);

        GptIntegration::copy_content_encoding_headers(&upstream, &mut downstream);

        assert!(
            downstream.get_header(header::CONTENT_ENCODING).is_none(),
            "should not set Content-Encoding when upstream response is unencoded"
        );
        assert!(
            downstream.get_header(header::VARY).is_none(),
            "should not add Vary when Content-Encoding is absent"
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
}
