//! Google Publisher Tags (GPT) integration for first-party ad serving.
//!
//! This module provides transparent proxying for Google's entire GPT script
//! chain, enabling first-party ad tag delivery.
//! GPT loads scripts in a cascade:
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
//! 1. **HTML rewriting** – The module's middleware swaps `src`/`href`
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
//!
//! For a page that carries no loader of its own, `write_loader` asks for the
//! loader tag, which a second middleware, [`LOADER_MIDDLEWARE`], writes in
//! the serve phase and only for a reader whose permissions allow storage on
//! the device, because GPT loaded from Google's standard address may use the
//! device's storage at any time.
//!
//! The crate holds a second module, [`diagnostics`], which a deployment
//! selects on its own as `ad-tag.google.diagnostics`.

#![cfg_attr(
    test,
    allow(
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::panic,
        clippy::dbg_macro,
        clippy::unwrap_used,
        reason = "tests use direct diagnostics and panic-on-failure helpers"
    )
)]

pub mod diagnostics;

use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{Request, Response, header};
use serde::{Deserialize, Serialize};
use url::Url;
use validator::Validate;

use trusted_server_core::constants::{
    HEADER_ACCEPT, HEADER_ACCEPT_ENCODING, HEADER_ACCEPT_LANGUAGE,
};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{
    AttributeRewriteAction, IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
use trusted_server_core::middleware::{
    AttributeRewrite, AttributeRewriteFn, Middleware, MiddlewareAction, MiddlewareContext,
    MiddlewarePhase, Preparation,
};
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::permissions::{Permission, PermissionSet, PermissionState};
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{ProxyRequestConfig, proxy_request};
use trusted_server_core::settings::{IntegrationConfig, Settings};

const GPT_INTEGRATION_ID: &str = "gpt";

/// The name this module is selected by, in `[ad-tag]`, and the name of its
/// fetch middleware, which writes the bootstrap and points the GPT script's
/// address at the first-party path.
pub const MODULE: &str = "ad-tag.google";

/// The name of the module's serve middleware, which writes the loader tag,
/// when `write_loader` is set, into the copy of each reader whose
/// permissions allow storage on the device.
pub const LOADER_MIDDLEWARE: &str = "ad-tag.google.loader";

/// The page changes the module supplies, as entries name them.
const SUPPLIED_MIDDLEWARE: &[trusted_server_core::integrations::SuppliedMiddleware] = &[
    trusted_server_core::integrations::SuppliedMiddleware::on_html(
        MODULE,
        trusted_server_core::middleware::MiddlewarePhase::Fetch,
    ),
    trusted_server_core::integrations::SuppliedMiddleware::on_html(
        LOADER_MIDDLEWARE,
        trusted_server_core::middleware::MiddlewarePhase::Serve,
    ),
];

/// The builder a deployment hands to an adapter, which the registry runs when
/// a section selects [`MODULE`].
#[must_use]
pub fn builder() -> trusted_server_core::integrations::IntegrationBuilder {
    trusted_server_core::integrations::IntegrationBuilder::new(
        GPT_INTEGRATION_ID,
        env!("CARGO_PKG_NAME"),
        register,
        validate,
    )
    .with_module_name(MODULE)
    .with_middleware(SUPPLIED_MIDDLEWARE)
}

/// Primary Google domain that serves GPT scripts.
const SECUREPUBADS_HOST: &str = "securepubads.g.doubleclick.net";

/// Integration route prefix for all GPT proxy endpoints.
const ROUTE_PREFIX: &str = "/integrations/gpt";

/// The first-party path a page loads the GPT script from.
const SCRIPT_PATH: &str = "/integrations/gpt/script";

/// Configuration for the Google Publisher Tags integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct GptConfig {
    /// Enable page-level `ts=true` delivery attribution in GAM.
    #[serde(default)]
    pub gam_attribution_enabled: bool,

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

    /// URL for the slim-Prebid bundle loaded post-window.load.
    ///
    /// When set, `installSlimPrebidLoader()` in the GPT bundle will load this
    /// script after `window.load`, enabling scroll/refresh client-side auctions
    /// and userID module warm-up. Set to the publisher's tsjs-prebid bundle URL.
    ///
    /// Override via env var: `TRUSTED_SERVER__AD-TAG__GOOGLE__SLIM_PREBID_URL`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slim_prebid_url: Option<String>,

    /// Whether the loader tag is written into the head, for a page that
    /// carries none of its own. The serve middleware [`LOADER_MIDDLEWARE`]
    /// writes it, for a reader whose permissions allow storage on the device.
    /// The tag asks for the loader on this module's own route, so it is
    /// served first party like the rest of the script chain. Off by default,
    /// because a page that already carries the loader would otherwise load it
    /// twice.
    ///
    /// Override via env var: `TRUSTED_SERVER__AD-TAG__GOOGLE__WRITE_LOADER`
    #[serde(default)]
    pub write_loader: bool,
}

impl IntegrationConfig for GptConfig {}

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

    fn build_proxy_config<'a>(
        target_url: &'a str,
        req: &Request<EdgeBody>,
    ) -> ProxyRequestConfig<'a> {
        let mut config = ProxyRequestConfig::new(target_url)
            .with_streaming()
            .without_forward_headers();
        config.follow_redirects = false;
        config.forward_ec_id = false;

        Self::apply_request_header_allowlist(config, req)
    }

    fn apply_request_header_allowlist<'a>(
        mut config: ProxyRequestConfig<'a>,
        req: &Request<EdgeBody>,
    ) -> ProxyRequestConfig<'a> {
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
            header::USER_AGENT,
            http::HeaderValue::from_static("TrustedServer/1.0"),
        )
    }

    fn ensure_successful_gpt_asset_response(
        response: &Response<EdgeBody>,
        context: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        log::error!(
            "GPT proxy upstream returned status {} for {}",
            status,
            context
        );
        Err(Report::new(Self::error(format!(
            "{context}: upstream returned {status}"
        ))))
    }

    fn finalize_gpt_asset_response(&self, response: Response<EdgeBody>) -> Response<EdgeBody> {
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

        let mut finalized = Response::new(body);
        *finalized.status_mut() = status;
        finalized
            .headers_mut()
            .insert("X-GPT-Proxy", http::HeaderValue::from_static("true"));

        if let Some(content_type) = content_type {
            finalized
                .headers_mut()
                .insert(header::CONTENT_TYPE, content_type);
        }

        if let Some(etag) = etag {
            finalized.headers_mut().insert(header::ETAG, etag);
        }

        if let Some(last_modified) = last_modified {
            finalized
                .headers_mut()
                .insert(header::LAST_MODIFIED, last_modified);
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
                .expect("should build GPT Vary header"),
            );
        }

        if status.is_success() {
            finalized.headers_mut().insert(
                header::CACHE_CONTROL,
                http::HeaderValue::from_str(&format!(
                    "public, max-age={}",
                    self.config.cache_ttl_seconds
                ))
                .expect("should build GPT Cache-Control header"),
            );
        }

        finalized
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

    async fn proxy_gpt_asset(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: Request<EdgeBody>,
        target_url: &str,
        context: &str,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let config = Self::build_proxy_config(target_url, &req);
        let response = proxy_request(settings, req, config, services)
            .await
            .change_context(Self::error(context))?;

        Self::ensure_successful_gpt_asset_response(&response, context)?;
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
        services: &RuntimeServices,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let script_url = &self.config.script_url;
        log::info!("Fetching GPT script from: {}", script_url);
        self.proxy_gpt_asset(
            settings,
            services,
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
        services: &RuntimeServices,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let original_path = req.uri().path().to_string();
        let query = req.uri().query();

        let target_url = Self::build_upstream_url(&original_path, query)
            .ok_or_else(|| Self::error(format!("Invalid GPT pagead path: {}", original_path)))?;

        log::info!("GPT proxy: forwarding to {}", target_url);
        self.proxy_gpt_asset(
            settings,
            services,
            req,
            &target_url,
            &format!("Failed to fetch GPT resource from {target_url}"),
        )
        .await
    }
}

fn build(settings: &Settings) -> Result<Option<Arc<GptIntegration>>, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<GptConfig>(MODULE)? else {
        log::debug!("[gpt] Integration disabled or not configured");
        return Ok(None);
    };

    Ok(Some(GptIntegration::new(config)))
}

/// Validates the GPT configuration for deployment and reports whether
/// a section selects the integration's module.
///
/// # Errors
///
/// Returns an error when the GPT configuration cannot be parsed or fails
/// validation.
pub(crate) fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    settings
        .module_config::<GptConfig>(MODULE)
        .map(|config| config.is_some())
}

/// Register the GPT integration.
///
/// # Errors
///
/// Returns an error when the GPT integration runs with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    let mut registration = IntegrationRegistration::builder(GPT_INTEGRATION_ID)
        .with_proxy(integration.clone())
        .with_middleware(Arc::new(PageChange::new(integration.clone())))
        .with_middleware(Arc::new(Loader(integration.clone())));
    if let Some((name, value)) = integration.bundle_tag_attribute() {
        registration = registration.with_bundle_tag_attribute(name, value);
    }
    Ok(Some(registration.build()))
}

impl GptIntegration {
    async fn route(
        &self,
        req: http::Request<EdgeBody>,
        settings: &Settings,
        services: &RuntimeServices,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let path = req.uri().path().to_string();

        if path == "/integrations/gpt/script" {
            self.handle_script_serving(settings, services, req).await
        } else if path.starts_with("/integrations/gpt/pagead/")
            || path.starts_with("/integrations/gpt/tag/")
        {
            self.handle_pagead_proxy(settings, services, req).await
        } else {
            Err(Report::new(Self::error(format!(
                "Unknown GPT route: {}",
                path
            ))))
        }
    }
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
        call: trusted_server_core::module_context::ModuleCall<'_>,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        call.inject_with(self, req, Self::route)?.await
    }
}

/// GPT's change to a page, on the pages a `[[fetch]]` entry names [`MODULE`]
/// for. It writes the `tsjs.adInit` bootstrap into the head and, when
/// `rewrite_script` is set, points the GPT script's address at the
/// first-party path.
struct PageChange {
    integration: Arc<GptIntegration>,
    /// The head scripts as written, then [`SCRIPT_PATH`].
    template_key: String,
}

impl PageChange {
    fn new(integration: Arc<GptIntegration>) -> Self {
        let template_key = integration.head_markup().concat() + SCRIPT_PATH;
        Self {
            integration,
            template_key,
        }
    }
}

impl Middleware for PageChange {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let mut action = MiddlewareAction {
            head_inserts: self.integration.head_markup(),
            ..MiddlewareAction::pass()
        };
        if self.integration.config.rewrite_script {
            let decide: Rc<AttributeRewriteFn> =
                Rc::new(|matched| GptIntegration::rewrite_script_address(matched.value));
            action.element_handlers = AttributeRewrite::each(&["src", "href"], &decide);
        }
        action
    }

    /// The head scripts and the script path, because the bootstrap and the
    /// path are compiled into this crate and no setting changes when they do.
    fn template_key(&self) -> &str {
        &self.template_key
    }
}

/// What the loader tag needs from a reader, being storage on the device,
/// because GPT loaded from Google's standard address may use the device's
/// storage at any time. That is TCF Purpose 1, which core's server-side
/// auction also requires where GDPR applies.
const LOADER_REQUIRES: PermissionSet = PermissionSet::none().with(Permission::StoreOnDevice);

/// Whether one reader's permissions allow the loader tag, as the
/// middleware's preparation found them.
struct LoaderAllowed(bool);

/// Writes the loader tag into the head of the copy of each reader whose
/// permissions allow storage on the device, on the pages a `[[serve]]` entry
/// names [`LOADER_MIDDLEWARE`] for, when `write_loader` is set.
///
/// Serve and not fetch, because what a fetch middleware writes is stored and
/// served to every reader, whatever their permissions.
struct Loader(Arc<GptIntegration>);

impl Loader {
    /// Whether `permissions` set everything the loader tag needs.
    fn loader_allowed(&self, permissions: &PermissionState) -> bool {
        permissions.all_set(self.required_permissions())
    }
}

impl Middleware for Loader {
    fn middleware_id(&self) -> &'static str {
        LOADER_MIDDLEWARE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Serve]
    }

    fn required_permissions(&self) -> PermissionSet {
        LOADER_REQUIRES
    }

    /// Reads the reader's permissions, which core hands every serve
    /// preparation, when `write_loader` is set. A preparation handed none
    /// treats the reader as one whose permissions do not allow the loader.
    fn prepare<'a>(
        &'a self,
        call: ModuleCall<'a>,
        _context: &'a MiddlewareContext<'a>,
    ) -> Option<Preparation<'a>> {
        if !self.0.config.write_loader {
            return None;
        }
        let allowed = matches!(call.inject(self, Self::loader_allowed), Ok(true));
        Some(Box::pin(async move {
            Ok(Box::new(LoaderAllowed(allowed)) as Box<dyn Any + Send>)
        }))
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        if !self.0.config.write_loader {
            return MiddlewareAction::pass();
        }
        // A document nothing was prepared for gets no loader.
        let allowed = context
            .take_prepared::<LoaderAllowed>(LOADER_MIDDLEWARE)
            .is_some_and(|prepared| prepared.0);
        if !allowed {
            return MiddlewareAction::pass();
        }
        MiddlewareAction {
            head_inserts: vec![GptIntegration::loader_tag()],
            ..MiddlewareAction::pass()
        }
    }
}

impl GptIntegration {
    /// What becomes of a `src` or an `href`, which is pointed at the
    /// first-party path when it is the GPT script's address.
    fn rewrite_script_address(attr_value: &str) -> AttributeRewriteAction {
        if Self::is_gpt_script_url(attr_value) {
            // Root-relative so the browser resolves it against the page host.
            // Note: a page-level `<base href>` participates in this resolution,
            // so on pages that set an external base URL these resolve against
            // that base rather than the address-bar origin — an accepted
            // tradeoff, matching GTM/Didomi/Testlight which are also relative.
            AttributeRewriteAction::replace(SCRIPT_PATH.to_string())
        } else {
            AttributeRewriteAction::keep()
        }
    }

    /// The `tsjs.adInit` bootstrap scripts written into `<head>`.
    ///
    /// ## Scroll / refresh handoff contract (Phase 1)
    ///
    /// `tsjs.adInit` handles **initial render only**: it wires server-side bid
    /// targeting into GPT slots and refreshes them. Win/billing beacons fire
    /// only from the TS render bridge in the JS bundle, where a matching
    /// Prebid Universal Creative request proves the TS creative rendered.
    /// It does **not** trigger refresh auctions or handle GPT slot refresh events.
    ///
    /// Post-`window.load`, slim-Prebid owns scroll and GPT refresh: it listens
    /// for GPT refresh events, runs client-side auctions, and sets targeting for
    /// subsequent impressions. SPA navigation is handled separately by
    /// `installSpaAuctionHook()` in the GPT bundle, which re-runs the server-side
    /// auction via `GET /_ts/page-bids` on pushState / replaceState / popstate
    /// route changes (see `auction/endpoints.rs`).
    /// The `POST /auction` endpoint is not involved in scroll or refresh flows.
    fn head_markup(&self) -> Vec<String> {
        let gam_attribution_flag = if self.config.gam_attribution_enabled {
            "window.__tsjs_gam_attribution_enabled=true;"
        } else {
            ""
        };

        let mut scripts = vec![
            format!(
                "<script>window.__tsjs_gpt_enabled=true;{gam_attribution_flag}\
                 window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>"
            ),
            format!("<script>{}</script>", GPT_BOOTSTRAP_JS),
        ];

        if let Some(ref url) = self.config.slim_prebid_url {
            // JSON-encode the URL, then escape `</` so a configured value
            // containing the literal `</script>` cannot close this inline tag and
            // let trailing markup execute (standard JSON-in-HTML mitigation).
            let encoded = serde_json::to_string(url)
                .expect("should serialize string")
                .replace("</", "<\\/");
            scripts.push(format!(
                "<script>window.__tsjs_slim_prebid_url={encoded};</script>"
            ));
        }

        scripts
    }

    /// The loader tag, which asks for GPT on this module's own route so the
    /// whole script chain stays first party. A serve middleware's head markup
    /// goes after every fetch middleware's, so the tag follows the shim and
    /// the bootstrap it relies on.
    fn loader_tag() -> String {
        format!("<script async src=\"{ROUTE_PREFIX}/script\"></script>")
    }

    /// The attribute on the script bundle's tag that lets the bundle apply
    /// GAM attribution, when `gam_attribution_enabled` is set.
    fn bundle_tag_attribute(&self) -> Option<(&'static str, &'static str)> {
        self.config
            .gam_attribution_enabled
            .then_some(("data-ts-gam-attribution", "true"))
    }
}

/// Inline `window.tsjs.adInit` bootstrap injected at `<head>` so the bids
/// script at `</body>` can call it before the TSJS bundle has loaded.
///
/// The bundle's idempotent implementation in
/// `crates/ad-tag/google/js/src/gpt/index.ts` later overwrites this stub.
/// Both implementations guard the one-time-per-page setup with
/// `window.tsjs.servicesEnabled` so neither double-enables services if the
/// publisher's own init code also calls `googletag.enableServices()`.
const GPT_BOOTSTRAP_JS: &str = include_str!("gpt_bootstrap.js");

// Default value functions

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
    use http::Method;
    use trusted_server_core::constants::HEADER_X_FORWARDED_FOR;
    use trusted_server_core::html_processor::HtmlProcessorConfig;
    use trusted_server_core::html_processor::test_support::{
        ORIGIN_HOST, REQUEST_HOST, REQUEST_PATH, create_page_processor, place_on_every_page,
    };
    use trusted_server_core::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use trusted_server_core::middleware::test_support as middleware_test_support;
    use trusted_server_core::middleware::{
        HTML_MEDIA_TYPE, MiddlewareChain, MiddlewarePlan, Prepared,
    };
    use trusted_server_core::module_context::ModuleContext;
    use trusted_server_core::module_context::test_support::request;
    use trusted_server_core::permissions::{ConsentSignal, PermissionMaps};
    use trusted_server_core::streaming_processor::StreamProcessor as _;
    use trusted_server_core::test_support::tests::{
        create_test_settings, storage_only_permissions,
    };

    fn test_config() -> GptConfig {
        GptConfig {
            gam_attribution_enabled: false,
            script_url: default_script_url(),
            cache_ttl_seconds: 3600,
            rewrite_script: true,
            slim_prebid_url: None,
            write_loader: false,
        }
    }

    /// What the module's middleware decides to do with a page.
    fn page_action(integration: Arc<GptIntegration>) -> MiddlewareAction {
        let document_state = IntegrationDocumentState::default();
        let context = trusted_server_core::middleware::test_support::context(
            MiddlewarePhase::Fetch,
            &document_state,
        );
        PageChange::new(integration).create(&context)
    }

    fn build_http_request(method: Method, uri: &str) -> http::Request<EdgeBody> {
        http::Request::builder()
            .method(method)
            .uri(uri)
            .body(EdgeBody::empty())
            .expect("should build HTTP request")
    }

    #[test]
    fn gam_attribution_defaults_to_disabled() {
        let config: GptConfig =
            serde_json::from_value(serde_json::json!({})).expect("should parse defaults");

        assert!(!config.gam_attribution_enabled);
    }

    #[test]
    fn gam_attribution_deserializes_explicit_values() {
        let disabled: GptConfig = serde_json::from_value(serde_json::json!({
            "gam_attribution_enabled": false
        }))
        .expect("should parse explicit false");
        let enabled: GptConfig = serde_json::from_value(serde_json::json!({
            "gam_attribution_enabled": true
        }))
        .expect("should parse explicit true");

        assert!(!disabled.gam_attribution_enabled);
        assert!(enabled.gam_attribution_enabled);
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

    // -- Element handler --

    #[test]
    fn element_handler_rewrites_gpt_urls() {
        let result = GptIntegration::rewrite_script_address(
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
        );

        match result {
            AttributeRewriteAction::Replace(url) => {
                assert_eq!(
                    url, "/integrations/gpt/script",
                    "should rewrite to root-relative first-party script endpoint"
                );
            }
            other => panic!("Expected Replace action, got {:?}", other),
        }
    }

    #[test]
    fn element_handler_keeps_non_gpt_urls() {
        let result = GptIntegration::rewrite_script_address("https://cdn.example.com/analytics.js");

        assert_eq!(
            result,
            AttributeRewriteAction::Keep,
            "should keep non-GPT URLs unchanged"
        );
    }

    #[test]
    fn element_handler_noop_when_disabled() {
        let config = GptConfig {
            rewrite_script: false,
            ..test_config()
        };
        let action = page_action(GptIntegration::new(config));

        assert!(
            action.element_handlers.is_empty(),
            "should judge no address when rewrite_script is disabled"
        );
        assert_eq!(
            action.head_inserts.len(),
            2,
            "should still write the bootstrap into the head"
        );
    }

    #[test]
    fn handles_attribute_respects_config() {
        let enabled = page_action(GptIntegration::new(test_config()));
        let judged: Vec<&str> = enabled
            .element_handlers
            .iter()
            .map(|handler| handler.attribute())
            .collect();
        assert_eq!(
            judged,
            ["src", "href"],
            "should judge src and href, and no other attribute, when rewrite_script is true"
        );

        let disabled = page_action(GptIntegration::new(GptConfig {
            rewrite_script: false,
            ..test_config()
        }));
        assert!(
            disabled.element_handlers.is_empty(),
            "should judge no attribute when rewrite_script is false"
        );
    }

    // -- GPT proxy configuration --

    #[test]
    fn build_proxy_config_uses_streaming_without_ec_forwarding_or_redirects() {
        let req = build_http_request(
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
            !config.forward_ec_id,
            "should not append EC ID to GPT asset requests"
        );
        assert!(
            !config.follow_redirects,
            "should keep GPT asset proxying on the original single-hop trust boundary"
        );
    }

    #[test]
    fn build_proxy_config_forwards_only_required_headers() {
        let mut req = build_http_request(
            Method::GET,
            "https://edge.example.com/integrations/gpt/script",
        );
        req.headers_mut().insert(
            HEADER_ACCEPT,
            http::HeaderValue::from_static("application/javascript"),
        );
        req.headers_mut().insert(
            HEADER_ACCEPT_LANGUAGE,
            http::HeaderValue::from_static("en-US,en;q=0.9"),
        );
        req.headers_mut().insert(
            HEADER_ACCEPT_ENCODING,
            http::HeaderValue::from_static("gzip"),
        );

        let config = GptIntegration::build_proxy_config(
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
            &req,
        );

        let accept = config
            .headers
            .iter()
            .find(|(name, _)| name == HEADER_ACCEPT)
            .and_then(|(_, value)| value.to_str().ok());
        let accept_language = config
            .headers
            .iter()
            .find(|(name, _)| name == HEADER_ACCEPT_LANGUAGE)
            .and_then(|(_, value)| value.to_str().ok());
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
            accept,
            Some("application/javascript"),
            "should preserve Accept for upstream content negotiation"
        );
        assert_eq!(
            accept_language,
            Some("en-US,en;q=0.9"),
            "should preserve Accept-Language for upstream locale negotiation"
        );
        assert_eq!(
            user_agent,
            Some("TrustedServer/1.0"),
            "should use a stable user agent for GPT upstream requests"
        );
        assert_eq!(
            referer, None,
            "should not forward Referer when proxying GPT assets"
        );
        assert_eq!(
            x_forwarded_for, None,
            "should not forward X-Forwarded-For when proxying GPT assets"
        );
        assert_eq!(
            accept_encoding,
            Some("gzip"),
            "should preserve the caller Accept-Encoding for streamed GPT assets"
        );
    }

    #[test]
    fn build_proxy_config_does_not_advertise_accept_encoding_when_client_omits_it() {
        let req = build_http_request(
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
            accept_encoding, None,
            "should avoid advertising encodings the client did not request"
        );
    }

    #[test]
    fn finalize_gpt_asset_response_rebuilds_successful_responses_with_safe_headers() {
        let integration = GptIntegration::new(test_config());
        let response = http::Response::builder()
            .status(http::StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .header(header::ETAG, "\"gpt-etag\"")
            .header(header::LAST_MODIFIED, "Thu, 13 Mar 2025 08:00:00 GMT")
            .header(header::CONTENT_ENCODING, "br")
            .header(header::VARY, "Origin")
            .header(header::SET_COOKIE, "gpt=1; Secure")
            .body(EdgeBody::empty())
            .expect("should build GPT response");
        let response = integration.finalize_gpt_asset_response(response);

        assert_eq!(
            response.status(),
            http::StatusCode::OK,
            "should preserve successful upstream statuses"
        );
        assert_eq!(
            response
                .headers()
                .get("X-GPT-Proxy")
                .and_then(|value| value.to_str().ok()),
            Some("true"),
            "should tag proxied GPT responses"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/javascript; charset=utf-8"),
            "should preserve upstream content type for GPT assets"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ETAG)
                .and_then(|value| value.to_str().ok()),
            Some("\"gpt-etag\""),
            "should preserve upstream ETag validators for GPT assets"
        );
        assert_eq!(
            response
                .headers()
                .get(header::LAST_MODIFIED)
                .and_then(|value| value.to_str().ok()),
            Some("Thu, 13 Mar 2025 08:00:00 GMT"),
            "should preserve upstream Last-Modified validators for GPT assets"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("br"),
            "should preserve upstream content encoding for GPT assets"
        );
        assert_eq!(
            response
                .headers()
                .get(header::VARY)
                .and_then(|value| value.to_str().ok()),
            Some("Origin, Accept-Encoding"),
            "should normalize Vary when returning encoded GPT assets"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=3600"),
            "should add cache headers for successful GPT asset responses"
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "should not project unrelated upstream headers to first-party clients"
        );
    }

    #[test]
    fn ensure_successful_gpt_asset_response_rejects_non_success_statuses() {
        let response = http::Response::builder()
            .status(http::StatusCode::SERVICE_UNAVAILABLE)
            .body(EdgeBody::empty())
            .expect("should build service unavailable response");
        let err = GptIntegration::ensure_successful_gpt_asset_response(
            &response,
            "Failed to fetch GPT script from https://securepubads.g.doubleclick.net/tag/js/gpt.js",
        )
        .expect_err("should reject non-success GPT upstream responses");

        match err.current_context() {
            TrustedServerError::Integration {
                integration,
                message,
            } => {
                assert_eq!(
                    integration, GPT_INTEGRATION_ID,
                    "should classify GPT upstream failures as integration errors"
                );
                assert!(
                    message.contains("upstream returned 503 Service Unavailable"),
                    "should report the upstream failure status"
                );
            }
            other => panic!("expected GPT integration error, got {other:?}"),
        }
    }

    #[test]
    fn vary_with_accept_encoding_preserves_wildcard() {
        let vary = GptIntegration::vary_with_accept_encoding(Some("*"));

        assert_eq!(vary, "*", "should preserve wildcard Vary values");
    }

    #[test]
    fn vary_with_accept_encoding_adds_accept_encoding_when_missing() {
        let vary = GptIntegration::vary_with_accept_encoding(Some("Origin"));

        assert_eq!(
            vary, "Origin, Accept-Encoding",
            "should explicitly vary encoded GPT assets on Accept-Encoding"
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
            build(&settings)
                .expect("should evaluate integration build")
                .is_none(),
            "should not build without integration config"
        );
    }

    #[test]
    fn build_with_valid_config() {
        let mut settings = create_test_settings();
        settings
            .insert_module_config(
                "ad-tag",
                "ad-tag.google",
                &serde_json::json!({
                    "script_url": "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
                    "cache_ttl_seconds": 3600,
                    "rewrite_script": true
                }),
            )
            .expect("should insert GPT config");

        assert!(
            build(&settings)
                .expect("should evaluate integration build")
                .is_some(),
            "should build with valid integration config"
        );
    }

    #[test]
    fn build_without_the_integration_named_returns_none() {
        let settings = create_test_settings();

        assert!(
            build(&settings)
                .expect("should evaluate integration build")
                .is_none(),
            "should not build an integration no section selects"
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
            Some(
                "https://securepubads.g.doubleclick.net/pagead/managed/js/gpt/m202603020101/pubads_impl.js"
            ),
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
            Some(
                "https://securepubads.g.doubleclick.net/pagead/managed/js/gpt/m202603020101/pubads_impl.js?cb=123&foo=bar"
            ),
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

    // -- Head markup --

    #[test]
    fn head_markup_emits_enable_flag() {
        let integration = GptIntegration::new(test_config());
        let inserts = integration.head_markup();

        assert_eq!(inserts.len(), 2, "should emit exactly two head inserts");
        assert_eq!(
            inserts[0],
            "<script>window.__tsjs_gpt_enabled=true;window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>",
            "should set the enable flag and call the GPT shim activation function"
        );
        assert!(
            integration.bundle_tag_attribute().is_none(),
            "should not authorize GAM attribution metadata by default"
        );
    }

    #[test]
    fn gam_attribution_true_adds_both_activation_signals_without_a_new_insert() {
        let integration = GptIntegration::new(GptConfig {
            gam_attribution_enabled: true,
            ..test_config()
        });
        let inserts = integration.head_markup();

        assert_eq!(inserts.len(), 2, "should not add another head insert");
        assert!(
            inserts[0].contains("window.__tsjs_gam_attribution_enabled=true;"),
            "should activate the early bootstrap marker"
        );
        assert_eq!(
            integration.bundle_tag_attribute(),
            Some(("data-ts-gam-attribution", "true")),
            "should authorize the bundle fallback on the publisher tag"
        );
    }

    #[test]
    fn head_inserts_includes_ts_ad_init_with_synchronous_bids_read() {
        let config = test_config();
        let integration = GptIntegration::new(config);
        let inserts = integration.head_markup();
        let combined = inserts.join("");
        assert!(combined.contains("ts.adInit"), "should define tsjs.adInit");
        assert!(
            combined.contains("ts.bids"),
            "should read tsjs.bids synchronously"
        );
        assert!(
            combined.contains("ts_initial"),
            "should set ts_initial sentinel"
        );
        assert!(
            combined.contains("addEventListener(\"slotRequested\""),
            "should observe publisher GPT requests before delayed adInit"
        );
        assert!(
            combined.contains("addEventListener(\"slotRenderEnded\""),
            "should observe publisher GPT renders before delayed adInit"
        );
        assert!(
            !combined.contains("sendBeacon"),
            "inline bootstrap lifecycle ownership must not fire win/billing beacons"
        );
        assert!(
            !combined.contains("getTargeting(\"hb_adid\")"),
            "inline bootstrap must not treat GPT targeting as winner proof"
        );
        assert!(
            !combined.contains("/ts-bids"),
            "must NOT fetch /ts-bids — bids are inline on the page"
        );
        assert!(
            !combined.contains("bidsPromise"),
            "must NOT use bidsPromise — bids are synchronous"
        );
        assert!(
            !combined.contains("__ts_request_id"),
            "must NOT reference request_id — no longer used"
        );
    }

    #[test]
    fn head_inserts_bootstrap_installs_fallback_scheduler() {
        // The `</body>` bids script hands its payload to
        // `tsjs.scheduleInitialAdInit`. The bundle installs the real scheduler,
        // but when the bundle fails to load this head bootstrap must provide
        // the degradation path — otherwise a failed bundle request would leave
        // initial server-side ads uninitialized. Executable coverage of the
        // fallback lives in the Vitest suite (gpt_bootstrap.test.ts).
        let config = test_config();
        let integration = GptIntegration::new(config);
        let combined = integration.head_markup().join("");
        assert!(
            combined.contains("ts.scheduleInitialAdInit"),
            "should install the fallback scheduler for bundle-load failures"
        );
        assert!(
            combined.contains("navGeneration"),
            "fallback scheduler should honor the navigation-generation guard"
        );
        assert!(
            combined.contains("requestAnimationFrame"),
            "fallback scheduler should defer past hydration frames"
        );
        assert!(
            combined.contains("\"load\""),
            "fallback scheduler should gate on window load"
        );
        // The no-retry-timer property is owned by the executable suite
        // (gpt_bootstrap.test.ts asserts adInit runs exactly once); a
        // `!contains("setTimeout")` over the whole joined head-insert output
        // would misattribute any future unrelated timer to the scheduler.
    }

    #[test]
    fn head_inserts_bootstrap_uses_css_safe_div_prefix_lookup() {
        let config = test_config();
        let integration = GptIntegration::new(config);
        let combined = integration.head_markup().join("");
        assert!(
            combined.contains("querySelectorAll(\"[id]\")"),
            "bootstrap should scan ID-bearing elements instead of interpolating div_id into CSS"
        );
        assert!(
            combined.contains("candidate.id.startsWith(divId)"),
            "bootstrap should match metacharacter-containing div_id prefixes with startsWith"
        );
        assert!(
            !combined.contains("[id^='\" + slot.div_id"),
            "bootstrap must not build a CSS attribute selector from raw div_id"
        );
    }

    #[test]
    fn head_inserts_bootstrap_installs_inner_div_slot_handoff() {
        let integration = GptIntegration::new(test_config());
        let combined = integration.head_markup().join("");
        assert!(
            combined.contains("gptSlotHandoffs"),
            "bootstrap should keep late publisher slot handoff state on window.tsjs"
        );
        assert!(
            combined.contains("__tsSlotHandoffPatched"),
            "bootstrap should install idempotent GPT handoff wrappers"
        );
        assert!(
            combined.contains("return defineWithViewportSizes(googletag, slot, actualDivId)"),
            "bootstrap should define the TS fallback on the actual inner div"
        );
        assert!(
            !combined.contains("actualDivId + \"-container\""),
            "bootstrap must not define a competing outer-container GPT slot"
        );
    }

    #[test]
    fn head_inserts_bootstrap_guards_enable_services_with_idempotency_flag() {
        let config = test_config();
        let integration = GptIntegration::new(config);
        let combined = integration.head_markup().join("");
        assert!(
            combined.contains("ts.servicesEnabled"),
            "should guard enableServices/enableSingleRequest with the tsjs.servicesEnabled flag"
        );
        assert!(combined.contains("ts.adInit"), "should install tsjs.adInit");
        assert!(
            !combined.contains("googletag.pubads().refresh()"),
            "should never call unbounded refresh() — only refresh(newSlots)"
        );
    }

    #[test]
    fn head_inserts_bootstrap_refreshes_ts_slots_when_initial_load_disabled() {
        // Mirrors the bundle: when the publisher disables initial load through
        // setConfig() or the legacy disableInitialLoad() method, display() only
        // registers a TS-defined slot, so the bootstrap must also refresh those
        // slots or they render blank.
        let config = test_config();
        let integration = GptIntegration::new(config);
        let combined = integration.head_markup().join("");
        assert!(
            combined.contains("gpt.setConfig"),
            "bootstrap should wrap googletag.setConfig() to detect the disabled state"
        );
        assert!(
            combined.contains("gpt.getConfig"),
            "bootstrap should read GPT's modern initial-load configuration"
        );
        assert!(
            combined.contains("pubads.disableInitialLoad"),
            "bootstrap should wrap legacy disableInitialLoad() calls"
        );
        assert!(
            combined.contains("gptInitialLoadDisabled"),
            "bootstrap should record the initial-load-disabled state on window.tsjs"
        );
        assert!(
            combined.contains("slotsNeedingRefresh"),
            "bootstrap should refresh TS-defined slots when initial load is disabled"
        );
    }

    #[test]
    fn head_inserts_queue_gam_attribution_before_guard_and_ad_requests() {
        let targeting_index = GPT_BOOTSTRAP_JS
            .find("gpt.setConfig({ targeting: { ts: \"true\" } })")
            .expect("should apply the fixed page-level GAM targeting pair");
        let guard_index = GPT_BOOTSTRAP_JS
            .find("if (ts.adInit) return;")
            .expect("should retain the preinstalled adInit guard");
        let display_index = GPT_BOOTSTRAP_JS
            .find("googletag.display(divId);")
            .expect("should retain the executable GPT display call");
        let refresh_index = GPT_BOOTSTRAP_JS
            .find("googletag.pubads().refresh(slotsNeedingRefresh);")
            .expect("should retain the bounded GPT refresh call");

        assert!(
            targeting_index < guard_index,
            "should enqueue attribution before the preinstalled adInit guard"
        );
        assert!(
            targeting_index < display_index,
            "should enqueue attribution before the executable display call"
        );
        assert!(
            targeting_index < refresh_index,
            "should enqueue attribution before the executable refresh call"
        );
    }

    #[test]
    fn the_middleware_is_named_as_the_module() {
        let integration = GptIntegration::new(test_config());
        assert_eq!(PageChange::new(integration).middleware_id(), MODULE);
    }

    /// The bootstrap and the script path are compiled into this crate and no
    /// setting changes when they do, so they have to be in the text the
    /// registered middleware keys a stored page by, or a page stored before a
    /// change to either keeps the old one until it expires.
    #[test]
    fn the_bootstrap_is_in_what_keys_a_stored_page() {
        let settings = trusted_server_core::html_processor::test_support::page_settings(
            include_str!("fixtures/page-change.settings.toml"),
        );
        let registry = trusted_server_core::integrations::IntegrationRegistry::with_registrations(
            &settings,
            &[super::builder(), super::diagnostics::builder()],
        )
        .expect("should build the registry");

        let keys = registry.middleware_template_keys();
        let (_, key) = keys
            .iter()
            .find(|(name, _)| *name == MODULE)
            .expect("the page change should be registered");
        assert!(key.contains(GPT_BOOTSTRAP_JS), "{key}");
        assert!(key.contains(SCRIPT_PATH), "{key}");
    }

    #[test]
    fn a_registration_states_the_attribution_attribute_only_when_it_is_enabled() {
        let attributes_with = |gam_attribution_enabled: bool| {
            let mut settings = create_test_settings();
            settings
                .insert_module_config(
                    "ad-tag",
                    MODULE,
                    &serde_json::json!({ "gam_attribution_enabled": gam_attribution_enabled }),
                )
                .expect("should select GPT");
            register(&settings)
                .expect("should register GPT")
                .expect("should run GPT")
                .bundle_tag_attributes
        };

        assert_eq!(
            attributes_with(true),
            [("data-ts-gam-attribution", "true")],
            "should put the attribute on the bundle's tag"
        );
        assert!(
            attributes_with(false).is_empty(),
            "should put nothing on the bundle's tag by default"
        );
    }

    #[test]
    fn head_inserts_emits_slim_prebid_url_when_configured() {
        let config = GptConfig {
            slim_prebid_url: Some("https://cdn.example.com/tsjs-prebid.min.js".to_string()),
            ..test_config()
        };
        let integration = GptIntegration::new(config);
        let inserts = integration.head_markup();

        assert_eq!(
            inserts.len(),
            3,
            "should emit three head inserts when slim_prebid_url is set"
        );
        assert_eq!(
            inserts[2],
            r#"<script>window.__tsjs_slim_prebid_url="https://cdn.example.com/tsjs-prebid.min.js";</script>"#,
            "should emit the slim-Prebid URL as a JSON-encoded string assignment"
        );
    }

    #[test]
    fn head_inserts_escapes_script_terminator_in_slim_prebid_url() {
        // A configured URL containing `</script>` must not close the inline tag.
        let config = GptConfig {
            slim_prebid_url: Some("https://cdn.example.com/x</script><img src=x>".to_string()),
            ..test_config()
        };
        let integration = GptIntegration::new(config);
        let inserts = integration.head_markup();

        // The injected `</script><img ...>` must be neutralised: the only
        // `</script>` left is the tag's own legitimate closer.
        assert!(
            !inserts[2].contains("</script><img"),
            "should escape the injected </script> terminator, got: {}",
            inserts[2]
        );
        assert_eq!(
            inserts[2].matches("</script>").count(),
            1,
            "only the tag's own closing </script> should remain, got: {}",
            inserts[2]
        );
        assert!(
            inserts[2].contains("<\\/script>"),
            "should emit the escaped terminator, got: {}",
            inserts[2]
        );
    }

    #[test]
    fn head_inserts_omits_slim_prebid_url_when_not_configured() {
        let integration = GptIntegration::new(test_config());
        let inserts = integration.head_markup();

        assert_eq!(
            inserts.len(),
            2,
            "should emit exactly two head inserts when slim_prebid_url is absent"
        );
        assert!(
            inserts
                .iter()
                .all(|s| !s.contains("__tsjs_slim_prebid_url")),
            "should not emit slim-Prebid URL tag when not configured"
        );
    }

    /// The loader tag, as a page carries it.
    const LOADER_TAG: &str = r#"<script async src="/integrations/gpt/script"></script>"#;

    /// An invented publisher's page that carries no GPT loader of its own,
    /// which is the page `write_loader` is for.
    const PAGE_WITHOUT_A_LOADER: &str = concat!(
        "<!doctype html><html><head><title>An article</title></head>",
        "<body><div id=\"ad-slot-1\"></div></body></html>"
    );

    /// Settings selecting the module with `config` as its table.
    fn settings_with_gpt(config: &serde_json::Value) -> Settings {
        let mut settings = create_test_settings();
        settings
            .insert_module_config("ad-tag", MODULE, config)
            .expect("should select GPT");
        settings
    }

    /// Settings selecting the module with `write_loader` set.
    fn settings_writing_the_loader() -> Settings {
        settings_with_gpt(&serde_json::json!({ "write_loader": true }))
    }

    /// The loader middleware the module registers for `settings`.
    fn loader_for(settings: &Settings) -> Arc<dyn Middleware> {
        register(settings)
            .expect("should register GPT")
            .expect("should run GPT")
            .middleware
            .into_iter()
            .find(|middleware| middleware.middleware_id() == LOADER_MIDDLEWARE)
            .expect("the module should register its loader middleware")
    }

    /// What the loader middleware plans for one reader's copy, after the
    /// serve phase's preparation has run with `module_context`, as core
    /// prepares and plans every reader's copy.
    fn loader_plan_with(settings: &Settings, module_context: &ModuleContext<'_>) -> MiddlewarePlan {
        let chain = MiddlewareChain::new(
            MiddlewarePhase::Serve,
            HTML_MEDIA_TYPE,
            vec![loader_for(settings)],
        );
        let document_state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        let context = MiddlewareContext {
            prepared: &prepared,
            ..middleware_test_support::context(MiddlewarePhase::Serve, &document_state)
        };
        futures::executor::block_on(chain.prepare(module_context, &context))
            .expect("should prepare the middleware");
        chain.plan(&context).expect("should plan the middleware")
    }

    /// What the loader middleware plans for a reader whose request resolved
    /// to `permissions`.
    fn loader_plan_for(settings: &Settings, permissions: &PermissionState) -> MiddlewarePlan {
        loader_plan_with(
            settings,
            &ModuleContext::new(request(REQUEST_PATH)).with_permissions(permissions),
        )
    }

    /// The page a reader whose request resolved to `permissions` receives of
    /// `html`, with the module's fetch middleware named in a `[[fetch]]`
    /// entry and its loader in a `[[serve]]` entry. Both phases are prepared
    /// and then run on one pass, as core does for a page it does not store.
    fn process_for(settings: &Settings, html: &str, permissions: &PermissionState) -> String {
        let mut placed = settings.clone();
        place_on_every_page(&mut placed, MiddlewarePhase::Fetch, &[MODULE]);
        place_on_every_page(&mut placed, MiddlewarePhase::Serve, &[LOADER_MIDDLEWARE]);
        let registry = IntegrationRegistry::with_registrations(&placed, &[builder()])
            .expect("should build a registry with the module");

        let prepared = Arc::new(Prepared::default());
        let module_context = ModuleContext::new(request(REQUEST_PATH))
            .with_settings(&placed)
            .with_permissions(permissions);
        for phase in MiddlewarePhase::ALL {
            let chain = registry.middleware_chain(
                placed.phase_entries(phase),
                phase,
                HTML_MEDIA_TYPE,
                REQUEST_PATH,
            );
            let document_state = IntegrationDocumentState::default();
            let context = MiddlewareContext {
                prepared: &prepared,
                ..middleware_test_support::context(phase, &document_state)
            };
            futures::executor::block_on(chain.prepare(&module_context, &context))
                .expect("should prepare the page's middleware");
        }

        let config = HtmlProcessorConfig::from_settings(
            &placed,
            &registry,
            ORIGIN_HOST,
            REQUEST_HOST,
            "https",
        )
        .with_prepared(prepared);
        let page = create_page_processor(&placed, &registry, config)
            .process_chunk(html.as_bytes(), true)
            .expect("should process the page");
        String::from_utf8(page).expect("should leave the page UTF-8")
    }

    #[test]
    fn the_fetch_phase_writes_no_loader_tag_even_when_asked() {
        // What the fetch phase writes is stored and served to every reader,
        // so the loader tag, which depends on the reader's permissions, is
        // never part of it.
        let action = page_action(GptIntegration::new(GptConfig {
            write_loader: true,
            ..test_config()
        }));

        assert!(
            action
                .head_inserts
                .iter()
                .all(|insert| !insert.contains("/integrations/gpt/script")),
            "the fetch phase should write no loader tag, got: {:?}",
            action.head_inserts
        );
    }

    #[test]
    fn a_reader_who_has_not_allowed_storage_gets_no_loader() {
        // Arrange
        // Every permission but storage on the device, which the loader needs.
        let settings = settings_writing_the_loader();
        let all_but_storage: PermissionSet = Permission::all()
            .filter(|permission| *permission != Permission::StoreOnDevice)
            .collect();

        // Act
        let plan = loader_plan_for(&settings, &PermissionState::new(all_but_storage));

        // Assert
        assert!(
            plan.head_inserts.is_empty(),
            "no loader tag should be written: {:?}",
            plan.head_inserts
        );
    }

    #[test]
    fn a_reader_who_allows_storage_gets_the_loader() {
        // Arrange
        let settings = settings_writing_the_loader();

        // Act
        let plan = loader_plan_for(&settings, &storage_only_permissions());

        // Assert
        assert_eq!(
            plan.head_inserts,
            vec![LOADER_TAG.to_owned()],
            "should ask for the loader on this module's own route"
        );
    }

    #[test]
    fn no_loader_is_written_unless_asked() {
        // Arrange
        // A reader who allows storage on the device, and a table that leaves
        // `write_loader` at its default.
        let settings = settings_with_gpt(&serde_json::json!({}));

        // Act
        let plan = loader_plan_for(&settings, &storage_only_permissions());

        // Assert
        assert!(
            plan.head_inserts.is_empty(),
            "no loader tag should be written unless asked: {:?}",
            plan.head_inserts
        );
    }

    #[test]
    fn no_loader_is_written_where_no_permission_state_reached_the_middleware() {
        // Arrange
        // A preparation whose context carries no permission state, and a
        // document nothing was prepared for.
        let settings = settings_writing_the_loader();
        let document_state = IntegrationDocumentState::default();

        // Act
        let unresolved = loader_plan_with(&settings, &ModuleContext::new(request(REQUEST_PATH)));
        let context = middleware_test_support::context(MiddlewarePhase::Serve, &document_state);
        let unprepared = loader_for(&settings).create(&context);

        // Assert
        assert!(
            unresolved.head_inserts.is_empty(),
            "no permission state: no loader tag should be written: {:?}",
            unresolved.head_inserts
        );
        assert!(
            unprepared.head_inserts.is_empty(),
            "nothing prepared: no loader tag should be written: {:?}",
            unprepared.head_inserts
        );
    }

    #[test]
    fn a_page_for_a_reader_who_has_not_agreed_carries_no_gpt_loader() {
        // Arrange
        // Under the repository's sample policy, a reader in Germany whose
        // request carries no signal is still waiting to allow storage on the
        // device, and a reader in California who sends an opt-out, such as
        // Global Privacy Control, has it revoked.
        let settings = settings_writing_the_loader();
        let policy = PermissionMaps::sample();
        let readers = [
            ("not yet agreed", policy.baseline(Some("DE"), None)),
            (
                "opted out",
                policy.resolve_with(Some("US"), Some("CA"), |permission, _| {
                    if policy.signals().opt_out_revokes(permission) {
                        ConsentSignal::Revoke
                    } else {
                        ConsentSignal::Neutral
                    }
                }),
            ),
        ];

        for (reader, permissions) in readers {
            // Act
            let processed = process_for(&settings, PAGE_WITHOUT_A_LOADER, &permissions);

            // Assert
            assert!(
                processed.contains(GPT_BOOTSTRAP_JS),
                "{reader}: the bootstrap should be written: {processed}"
            );
            assert!(
                !processed.contains("/integrations/gpt/script"),
                "{reader}: no loader tag should reach the page: {processed}"
            );
        }
    }

    #[test]
    fn a_page_for_a_reader_who_allows_storage_carries_the_loader_after_the_bootstrap() {
        // Arrange
        // Under the repository's sample policy, a reader in the United States
        // who sends no opt-out allows storage on the device.
        let settings = settings_writing_the_loader();
        let permissions = PermissionMaps::sample().baseline(Some("US"), None);

        // Act
        let processed = process_for(&settings, PAGE_WITHOUT_A_LOADER, &permissions);

        // Assert
        assert_eq!(
            processed.matches(LOADER_TAG).count(),
            1,
            "the loader tag should be written once: {processed}"
        );
        // The script that installs the shim comes before the bootstrap, so a
        // tag after the bootstrap's end follows both.
        let bootstrap_end = processed
            .find(GPT_BOOTSTRAP_JS)
            .map(|at| at + GPT_BOOTSTRAP_JS.len())
            .expect("the bootstrap should be written");
        let loader = processed
            .find(LOADER_TAG)
            .expect("the loader tag should be written");
        assert!(
            bootstrap_end <= loader,
            "the loader tag should follow the shim and the bootstrap it relies on: {processed}"
        );
    }

    #[test]
    fn the_loader_middleware_is_named_after_the_module() {
        assert_eq!(
            LOADER_MIDDLEWARE,
            trusted_server_core::module_name!("loader"),
            "should be the module's name with a part of its own"
        );
    }

    #[test]
    fn module_constant_is_the_crate_folder() {
        assert_eq!(
            super::MODULE,
            trusted_server_core::module_name!(),
            "should be named by the folder this crate lives in"
        );
    }

    /// The page a reader receives with this module running, kept as a file
    /// so that changing how the page change is made can be shown to leave
    /// the page as it was.
    #[test]
    fn the_page_a_reader_receives_is_the_recorded_one() {
        trusted_server_core::html_processor::test_support::assert_page_is_recorded(
            include_str!("fixtures/page-change.settings.toml"),
            &[super::builder(), super::diagnostics::builder()],
            include_str!("fixtures/page-change.input.html"),
            include_str!("fixtures/page-change.recorded.html"),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/fixtures/page-change.recorded.html"
            ),
        );
    }

    /// The builder describes the middleware the registration supplies, so a
    /// tool that writes a configuration places what the module registers.
    #[test]
    fn the_builder_describes_the_middleware_the_module_registers() {
        let settings = trusted_server_core::html_processor::test_support::page_settings(
            include_str!("fixtures/page-change.settings.toml"),
        );
        trusted_server_core::test_support::middleware::assert_described_middleware_is_registered(
            &settings,
            &[super::builder(), super::diagnostics::builder()],
            super::builder().id(),
        );
    }
}
