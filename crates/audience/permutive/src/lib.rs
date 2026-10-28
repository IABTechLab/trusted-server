//! Permutive integration for first-party data collection and audience management.
//!
//! This module provides transparent proxying for Permutive's API and SDK,
//! enabling first-party data collection.

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

use std::rc::Rc;
use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::{self, HeaderMap, HeaderValue};
use http::{Method, StatusCode};
use serde::Deserialize;
use validator::Validate;

use trusted_server_core::constants::INTERNAL_HEADERS;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{
    AttributeRewriteAction, INTEGRATION_MAX_BODY_BYTES, IntegrationEndpoint, IntegrationProxy,
    IntegrationRegistration, UPSTREAM_SDK_MAX_RESPONSE_BYTES, collect_body_bounded,
    collect_response_bounded, ensure_integration_backend,
};
use trusted_server_core::middleware::{
    AttributeRewrite, AttributeRewriteFn, Middleware, MiddlewareAction, MiddlewareContext,
    MiddlewarePhase,
};
use trusted_server_core::platform::{PlatformHttpRequest, RuntimeServices};
use trusted_server_core::settings::{IntegrationConfig, Settings};

const PERMUTIVE_INTEGRATION_ID: &str = "permutive";

/// The name this module is selected by, in `[audience]`.
pub const MODULE: &str = "audience.permutive";

/// The builder a deployment hands to an adapter, which the registry runs when
/// a section selects [`MODULE`].
#[must_use]
pub fn builder() -> trusted_server_core::integrations::IntegrationBuilder {
    trusted_server_core::integrations::IntegrationBuilder::new(
        PERMUTIVE_INTEGRATION_ID,
        env!("CARGO_PKG_NAME"),
        register,
        validate,
    )
    .with_module_name(MODULE)
}

/// Configuration for Permutive integration.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct PermutiveConfig {
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

impl IntegrationConfig for PermutiveConfig {}

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
    let Some(config) = settings.module_config::<PermutiveConfig>(MODULE)? else {
        return Ok(None);
    };

    Ok(Some(PermutiveIntegration::new(config)))
}

/// Validates the Permutive configuration for deployment and reports whether
/// a section selects the integration's module.
///
/// # Errors
///
/// Returns an error when the Permutive configuration cannot be parsed or fails
/// validation.
pub(crate) fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    settings
        .module_config::<PermutiveConfig>(MODULE)
        .map(|config| config.is_some())
}

/// Register the Permutive integration.
///
/// # Errors
///
/// Returns an error when the Permutive integration runs with invalid
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
            .with_middleware(Arc::new(SdkAddress(integration)))
            .build(),
    ))
}

impl PermutiveIntegration {
    async fn route(
        &self,
        req: http::Request<EdgeBody>,
        settings: &Settings,
        services: &RuntimeServices,
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
        call: trusted_server_core::module_context::ModuleCall<'_>,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        call.inject_with(self, req, Self::route)?.await
    }
}

/// The first-party path a page loads the SDK from.
const SDK_PATH: &str = "/integrations/permutive/sdk";

impl PermutiveIntegration {
    /// What becomes of a `src` or an `href`, which is pointed at the
    /// first-party endpoint when it is the SDK's address.
    fn rewrite_sdk_address(&self, value: &str) -> AttributeRewriteAction {
        if self.is_permutive_sdk_url(value) {
            // Rewrite to first-party SDK endpoint.
            // Root-relative so the browser resolves it against the page host.
            // Note: a page-level `<base href>` participates in this resolution,
            // so on pages that set an external base URL these resolve against
            // that base rather than the address-bar origin — an accepted
            // tradeoff, matching GTM/Didomi/Testlight which are also relative.
            AttributeRewriteAction::replace(SDK_PATH.to_string())
        } else {
            AttributeRewriteAction::keep()
        }
    }
}

/// Loads the SDK from the first-party endpoint, on the pages a `[[fetch]]`
/// entry names [`MODULE`] for. It does nothing unless `rewrite_sdk` is set.
struct SdkAddress(Arc<PermutiveIntegration>);

impl Middleware for SdkAddress {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
        if !self.0.config.rewrite_sdk {
            return MiddlewareAction::pass();
        }
        let integration = Arc::clone(&self.0);
        let decide: Rc<AttributeRewriteFn> =
            Rc::new(move |matched| integration.rewrite_sdk_address(matched.value));
        MiddlewareAction {
            element_handlers: AttributeRewrite::each(&["src", "href"], &decide),
            ..MiddlewareAction::pass()
        }
    }

    /// The SDK's path, because it is compiled into this crate and no setting
    /// changes when it does.
    fn template_key(&self) -> &str {
        SDK_PATH
    }
}

// Default value functions
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
    use trusted_server_core::platform::test_support::{
        StubHttpClient, build_services_with_http_client,
    };
    use trusted_server_core::test_support::tests::create_test_settings;

    #[test]
    fn test_permutive_sdk_url_generation() {
        let config = PermutiveConfig {
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
        assert!(
            integration
                .is_permutive_sdk_url("https://myorg.edge.permutive.app/workspace-123-web.js")
        );

        // Should match cdn.permutive.com URLs
        assert!(integration.is_permutive_sdk_url("https://cdn.permutive.com/myworkspace-web.js"));

        // Should not match other URLs
        assert!(!integration.is_permutive_sdk_url("https://example.com/script.js"));
        assert!(!integration.is_permutive_sdk_url("https://myorg.edge.permutive.app/other.js"));
    }

    #[test]
    fn test_element_handler_rewrites_sdk_urls() {
        let config = PermutiveConfig {
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: String::new(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        };
        let integration = PermutiveIntegration::new(config);

        let rewritten = integration
            .rewrite_sdk_address("https://myorg.edge.permutive.app/workspace-123-web.js");

        assert!(matches!(rewritten, AttributeRewriteAction::Replace(_)));
        if let AttributeRewriteAction::Replace(url) = rewritten {
            assert_eq!(url, "/integrations/permutive/sdk");
        }
    }

    #[test]
    fn test_element_handler_noop_when_disabled() {
        let config = PermutiveConfig {
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: String::new(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: false, // Disabled
        };
        let integration = PermutiveIntegration::new(config);
        let document_state = trusted_server_core::integrations::IntegrationDocumentState::default();
        let context = trusted_server_core::middleware::test_support::context(
            MiddlewarePhase::Fetch,
            &document_state,
        );

        assert!(
            SdkAddress(integration).create(&context).is_pass(),
            "should leave every page alone when SDK rewriting is off"
        );
    }

    #[test]
    fn test_build_requires_config() {
        let settings = create_test_settings();
        // Without [audience.permutive] config, should not build
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
            Arc::clone(&stub) as Arc<dyn trusted_server_core::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = PermutiveIntegration::new(PermutiveConfig {
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

        let response = futures::executor::block_on(integration.route(req, &settings, &services))
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
            Arc::clone(&stub) as Arc<dyn trusted_server_core::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = PermutiveIntegration::new(PermutiveConfig {
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

        let response = futures::executor::block_on(integration.route(req, &settings, &services))
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

    #[test]
    fn module_constant_is_the_crate_folder() {
        assert_eq!(
            super::MODULE,
            trusted_server_core::module_name!(),
            "should be named by the folder this crate lives in"
        );
    }

    /// The SDK's path is compiled into this crate and no setting changes when
    /// it does, so it has to be the text the registered middleware keys a
    /// stored page by, or a page stored before a change keeps loading the SDK
    /// from the old path until it expires.
    #[test]
    fn the_sdk_path_is_what_keys_a_stored_page() {
        let settings = trusted_server_core::html_processor::test_support::page_settings(
            include_str!("fixtures/page-change.settings.toml"),
        );
        let registry = trusted_server_core::integrations::IntegrationRegistry::with_registrations(
            &settings,
            &[builder()],
        )
        .expect("should build the registry");

        let keys = registry.middleware_template_keys();
        let (_, key) = keys
            .iter()
            .find(|(name, _)| *name == MODULE)
            .expect("the SDK's middleware should be registered");
        assert_eq!(*key, SDK_PATH);
        assert!(
            include_str!("fixtures/page-change.recorded.html")
                .contains(&format!("src=\"{SDK_PATH}\"")),
            "the recorded page should load the SDK from the path the key holds"
        );
    }

    /// The page a reader receives with this module running, kept as a file
    /// so that changing how the page change is made can be shown to leave
    /// the page as it was.
    #[test]
    fn the_page_a_reader_receives_is_the_recorded_one() {
        trusted_server_core::html_processor::test_support::assert_page_is_recorded(
            include_str!("fixtures/page-change.settings.toml"),
            &[super::builder()],
            include_str!("fixtures/page-change.input.html"),
            include_str!("fixtures/page-change.recorded.html"),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/fixtures/page-change.recorded.html"
            ),
        );
    }

    #[test]
    fn permutive_proxy_does_not_forward_host_signal_headers() {
        // Every proxied route forwards the reader's custom `x-` headers, so
        // each must filter out the host's TLS and HTTP/2 client signals.
        let routes = [
            "/integrations/permutive/api/v2.0/events",
            "/integrations/permutive/secure-signal/v1/segments",
            "/integrations/permutive/events/v1/track",
            "/integrations/permutive/sync/v1/sync",
            "/integrations/permutive/cdn/v1/config.json",
        ];
        let stub = Arc::new(StubHttpClient::new());
        for _ in routes {
            stub.push_response(200, b"ok".to_vec());
        }
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn trusted_server_core::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = PermutiveIntegration::new(PermutiveConfig {
            organization_id: "myorg".to_string(),
            workspace_id: "workspace-123".to_string(),
            project_id: String::new(),
            api_endpoint: default_api_endpoint(),
            secure_signals_endpoint: default_secure_signals_endpoint(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
        });

        for route in routes {
            let req = http::Request::builder()
                .method(http::Method::GET)
                .uri(format!("https://publisher.example{route}"))
                .header("x-ts-tls-ja4", "t13d1516h2_8daaf6152771_02713d6af862")
                .header(
                    "x-ts-h2-fingerprint",
                    "1:65536;4:6291456|15663105|0|m,a,s,p",
                )
                .header("x-permutive-trace", "kept")
                .body(EdgeBody::empty())
                .expect("should build request");
            futures::executor::block_on(integration.route(req, &settings, &services))
                .expect("should proxy request");
        }

        let recorded = stub.recorded_request_headers();
        assert_eq!(
            recorded.len(),
            routes.len(),
            "should forward one request per route"
        );
        for (route, headers) in routes.iter().zip(&recorded) {
            for name in ["x-ts-tls-ja4", "x-ts-h2-fingerprint"] {
                assert!(
                    headers.iter().all(|(header_name, _)| header_name != name),
                    "{route} should not forward the host signal header `{name}`"
                );
            }
            assert!(
                headers
                    .iter()
                    .any(|(name, value)| name == "x-permutive-trace" && value == "kept"),
                "{route} should still forward the reader's other custom headers"
            );
        }
    }
}
