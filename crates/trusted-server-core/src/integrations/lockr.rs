//! Lockr integration for identity resolution and advertising tokens.
//!
//! This module provides transparent proxying for Lockr's SDK and API,
//! enabling first-party identity resolution.
//!
//! Lockr provides a dedicated trust-server SDK (`identity-lockr-trust-server.js`)
//! that is pre-configured to route API calls through the first-party proxy,
//! so no runtime rewriting of the SDK JavaScript is needed.

use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::outbound::ResponseDecoding;
use error_stack::{Report, ResultExt};
use http::header::{self, HeaderMap, HeaderValue};
use http::{Method, StatusCode};
use serde::Deserialize;
use validator::Validate;

use crate::constants::INTERNAL_HEADERS;
use crate::error::TrustedServerError;
use crate::http_util::read_request_body;
use crate::integrations::sdk_url::parse_sdk_network_url;
use crate::integrations::{
    AttributeRewriteAction, INTEGRATION_MAX_BODY_BYTES, IntegrationAttributeContext,
    IntegrationAttributeRewriter, IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
    UPSTREAM_SDK_MAX_RESPONSE_BYTES,
};
use crate::outbound::{
    BUFFERED_TIMEOUT, DEFAULT_BUFFERED_RESPONSE_BYTES, OutboundRequestSource, buffered,
    outbound_error, outbound_request,
};
use crate::platform::RuntimeServices;
use crate::settings::{IntegrationConfig, Settings};

const LOCKR_INTEGRATION_ID: &str = "lockr";

/// Configuration for Lockr integration.
#[derive(Debug, Deserialize, Validate)]
pub struct LockrConfig {
    /// Enable/disable the integration
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Lockr app ID (from meta tag lockr-signin-app_id)
    #[validate(length(min = 1))]
    pub app_id: String,

    /// Base URL for Lockr API (default: <https://identity.loc.kr>)
    #[serde(default = "default_api_endpoint")]
    #[validate(url)]
    pub api_endpoint: String,

    /// SDK URL (default: <https://aim.loc.kr/identity-lockr-trust-server.js>)
    #[serde(default = "default_sdk_url")]
    #[validate(url)]
    pub sdk_url: String,

    /// Cache TTL for Lockr SDK in seconds (default: 3600 = 1 hour)
    #[serde(default = "default_cache_ttl")]
    #[validate(range(min = 60, max = 86400))]
    pub cache_ttl_seconds: u32,

    /// Whether to rewrite Lockr SDK URLs in HTML
    #[serde(default = "default_rewrite_sdk")]
    pub rewrite_sdk: bool,

    /// Deprecated — the trust-server SDK handles host routing natively.
    /// Kept for backwards compatibility so existing configs don't cause parse errors.
    #[serde(default)]
    pub rewrite_sdk_host: Option<bool>,

    /// Override the Origin header sent to Lockr API.
    /// Use this when running locally or from a domain not registered with Lockr.
    /// Example: "<https://www.example.com>"
    #[serde(default)]
    #[validate(url)]
    pub origin_override: Option<String>,
}

impl IntegrationConfig for LockrConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Lockr integration implementation.
pub struct LockrIntegration {
    config: LockrConfig,
}

impl LockrIntegration {
    fn new(config: LockrConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: LOCKR_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    /// Check if a URL is a Lockr SDK URL.
    fn is_lockr_sdk_url(&self, url: &str) -> bool {
        // Keep browser/Rust cases in crates/trusted-server-js/lib/test/fixtures/sdk-url-ownership.json aligned.
        let Some(parsed) = parse_sdk_network_url(url) else {
            return false;
        };
        let host = parsed.host_str().unwrap_or_default();
        let path = parsed.path().to_ascii_lowercase();
        matches!(host, "aim.loc.kr" | "identity.loc.kr")
            && path.contains("identity-lockr")
            && path.ends_with(".js")
    }

    /// Handle SDK serving: fetch from Lockr CDN and serve through first-party domain.
    async fn handle_sdk_serving(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let sdk_url = &self.config.sdk_url;
        log::info!("Fetching Lockr SDK from {}", sdk_url);

        // TODO: Check KV store cache first (future enhancement)

        let lockr_req = outbound_request(Method::GET, sdk_url)
            .and_then(|request| request.header(header::USER_AGENT, "TrustedServer/1.0"))
            .and_then(|request| request.header(header::ACCEPT, "application/javascript, */*"))
            .map_err(|error| {
                outbound_error(
                    error,
                    Self::error("Failed to build Lockr SDK request"),
                    OutboundRequestSource::Internal,
                )
            })?;
        let fetch_failed = || Self::error(format!("Failed to fetch Lockr SDK from {sdk_url}"));

        let lockr_response = services
            .outbound_client()
            .send(buffered(
                lockr_req,
                BUFFERED_TIMEOUT,
                UPSTREAM_SDK_MAX_RESPONSE_BYTES,
                ResponseDecoding::Decode,
            ))
            .await
            .map_err(|error| {
                outbound_error(error, fetch_failed(), OutboundRequestSource::Internal)
            })?;

        if !lockr_response.is_success() {
            log::error!(
                "Lockr SDK fetch failed with status {}",
                lockr_response.status()
            );
            return Err(Report::new(Self::error(format!(
                "Lockr SDK returned error status: {}",
                lockr_response.status()
            ))));
        }

        let sdk_body = lockr_response
            .into_bytes_bounded(UPSTREAM_SDK_MAX_RESPONSE_BYTES)
            .await
            .map_err(|error| {
                outbound_error(error, fetch_failed(), OutboundRequestSource::Internal)
            })?;
        log::info!("Fetched Lockr SDK ({} bytes)", sdk_body.len());

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
            .header("X-Lockr-SDK-Proxy", "true")
            .header("X-Lockr-SDK-Mode", "trust-server")
            .header("X-SDK-Source", sdk_url)
            .body(EdgeBody::from(sdk_body))
            .change_context(Self::error("Failed to build Lockr SDK response"))
    }

    /// Handle API proxy: forward requests to the configured Lockr API endpoint.
    async fn handle_api_proxy(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let (parts, body) = req.into_parts();
        let original_path = parts.uri.path().to_string();
        let method = parts.method.clone();

        log::info!("Proxying Lockr API request: {} {}", method, original_path);

        // Extract path after /integrations/lockr/api and pass through directly.
        // This allows the Lockr SDK to use any API endpoint without hardcoded mappings.
        let target_path = original_path
            .strip_prefix("/integrations/lockr/api")
            .ok_or_else(|| Self::error(format!("Invalid Lockr API path: {}", original_path)))?;

        let query = parts
            .uri
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("{}{}{}", self.config.api_endpoint, target_path, query);

        log::info!("Forwarding to Lockr API: {}", target_url);

        let request_body = if method == Method::POST {
            let bytes = read_request_body(&parts.headers, body, INTEGRATION_MAX_BODY_BYTES).await?;
            EdgeBody::from(bytes)
        } else {
            EdgeBody::empty()
        };

        let forward_failed = || Self::error(format!("Failed to forward request to {target_url}"));
        let mut target_req = outbound_request(method, &target_url)
            .map_err(|error| {
                outbound_error(
                    error,
                    Self::error("Failed to build Lockr API proxy request"),
                    OutboundRequestSource::Internal,
                )
            })?
            .body(request_body);
        self.copy_request_headers(&parts.headers, target_req.headers_mut())?;

        let response = services
            .outbound_client()
            .send(buffered(
                target_req,
                BUFFERED_TIMEOUT,
                DEFAULT_BUFFERED_RESPONSE_BYTES,
                ResponseDecoding::Original,
            ))
            .await
            .map_err(|error| {
                outbound_error(error, forward_failed(), OutboundRequestSource::Internal)
            })?;

        log::info!("Lockr API responded with status {}", response.status());

        response.into_response().map_err(|error| {
            outbound_error(error, forward_failed(), OutboundRequestSource::Internal)
        })
    }

    /// Copy relevant request headers for proxying.
    ///
    /// Consent cookies are always stripped — consent signals are forwarded
    /// through the `OpenRTB` body by the Prebid integration, not through
    /// Lockr's cookie-based API calls.
    fn copy_request_headers(
        &self,
        from: &HeaderMap<HeaderValue>,
        to: &mut HeaderMap<HeaderValue>,
    ) -> Result<(), Report<TrustedServerError>> {
        // NOTE: `Authorization` and `Cookie` are intentionally NOT forwarded.
        // Under the first-party proxy the browser attaches the publisher's own
        // credentials to `/integrations/lockr/api/...` — `Authorization` (e.g.
        // staging basic-auth) and every publisher session/auth cookie. Both
        // would leak to the third-party upstream, and the Lockr API rejects an
        // unexpected `Authorization` with `{"code":400,"message":"Invalid
        // request"}`. The SDK already passes the identity cookie data it needs
        // in the request body (`firstPartyCookies`), so no `Cookie` header is
        // required upstream.
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

        // Use origin override if configured, otherwise forward original
        let origin = self.config.origin_override.as_deref().or_else(|| {
            from.get(header::ORIGIN)
                .and_then(|value| value.to_str().ok())
        });
        if let Some(origin) = origin {
            match HeaderValue::from_str(origin) {
                Ok(value) => {
                    to.insert(header::ORIGIN, value);
                }
                Err(error) => {
                    log::warn!("Skipping invalid Lockr origin header value '{origin}': {error}");
                }
            }
        }

        for (name, value) in from {
            let name_str = name.as_str();
            if name_str.starts_with("x-") && !INTERNAL_HEADERS.contains(&name_str) {
                to.append(name.clone(), value.clone());
            }
        }

        Ok(())
    }
}

fn build(settings: &Settings) -> Result<Option<Arc<LockrIntegration>>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<LockrConfig>(LOCKR_INTEGRATION_ID)? else {
        return Ok(None);
    };

    Ok(Some(LockrIntegration::new(config)))
}

/// Register the Lockr integration.
///
/// # Errors
///
/// Returns an error when the Lockr integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    if integration.config.rewrite_sdk_host.is_some() {
        log::warn!(
            "lockr: `rewrite_sdk_host` is deprecated and ignored; \
             the trust-server SDK handles host routing natively"
        );
    }
    log::info!(
        "Registering Lockr integration (rewrite_sdk={})",
        integration.config.rewrite_sdk
    );

    Ok(Some(
        IntegrationRegistration::builder(LOCKR_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for LockrIntegration {
    fn integration_name(&self) -> &'static str {
        LOCKR_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get("/sdk"), self.post("/api/*"), self.get("/api/*")]
    }

    async fn handle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let path = req.uri().path().to_string();

        if path == "/integrations/lockr/sdk" {
            self.handle_sdk_serving(settings, services).await
        } else if path.starts_with("/integrations/lockr/api/") {
            self.handle_api_proxy(settings, services, req).await
        } else {
            Err(Report::new(Self::error(format!(
                "Unknown Lockr route: {}",
                path
            ))))
        }
    }
}

impl IntegrationAttributeRewriter for LockrIntegration {
    fn integration_id(&self) -> &'static str {
        LOCKR_INTEGRATION_ID
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
        if !self.config.rewrite_sdk {
            return AttributeRewriteAction::Keep;
        }

        if self.is_lockr_sdk_url(attr_value) {
            // Anchor SDK scripts and preloads to the page origin, not an external base URL.
            let replacement = format!(
                "{}://{}/integrations/lockr/sdk",
                ctx.request_scheme, ctx.request_host
            );
            log::debug!("Rewriting Lockr SDK URL to {}", replacement);
            AttributeRewriteAction::Replace(replacement)
        } else {
            AttributeRewriteAction::Keep
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_api_endpoint() -> String {
    "https://identity.loc.kr".to_string()
}

fn default_sdk_url() -> String {
    "https://aim.loc.kr/identity-lockr-trust-server.js".to_string()
}

fn default_cache_ttl() -> u32 {
    3600
}

fn default_rewrite_sdk() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use edgezero_core::outbound::{OutboundCachePolicy, ResponseDecoding, ResponseMode};
    use futures::executor::block_on;
    use serde_json::json;

    use super::*;
    use crate::integrations::sdk_url_test_support::{
        assert_sdk_html_attribute_rewrite, assert_sdk_url_rewrite, sdk_url_cases,
    };
    use crate::outbound::{BUFFERED_TIMEOUT, DEFAULT_BUFFERED_RESPONSE_BYTES};
    use crate::platform::test_support::build_services_with_outbound_client;
    use crate::test_support::body_read::body_read_failure_cases;
    use crate::test_support::scripted_client::{
        ExpectedTransport, ScriptedHttpClient, ScriptedReply, TestClock,
    };
    use crate::test_support::tests::create_test_settings;

    fn test_config() -> LockrConfig {
        LockrConfig {
            enabled: true,
            app_id: "test-app-id".to_string(),
            api_endpoint: default_api_endpoint(),
            sdk_url: default_sdk_url(),
            cache_ttl_seconds: 3600,
            rewrite_sdk: true,
            rewrite_sdk_host: None,
            origin_override: None,
        }
    }

    fn test_context() -> IntegrationAttributeContext<'static> {
        IntegrationAttributeContext {
            attribute_name: "src",
            element_name: "script",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        }
    }

    #[test]
    fn sdk_url_ownership_matches_shared_corpus() {
        let integration = LockrIntegration::new(test_config());

        for case in sdk_url_cases() {
            let matches = case.owner.as_deref() == Some(LOCKR_INTEGRATION_ID);
            assert_eq!(
                integration.is_lockr_sdk_url(&case.url),
                matches,
                "should match expected lockr ownership for {}: {}",
                case.name,
                case.url
            );

            assert_sdk_url_rewrite(
                integration.as_ref(),
                &case,
                "https://edge.example.com/integrations/lockr/sdk",
            );
            assert_sdk_html_attribute_rewrite(
                integration.as_ref(),
                &case,
                "https://edge.example.com/integrations/lockr/sdk",
            );
        }
    }

    #[test]
    fn test_lockr_sdk_url_detection() {
        let integration = LockrIntegration::new(test_config());

        // Should match Lockr SDK URLs
        assert!(integration.is_lockr_sdk_url("https://aim.loc.kr/identity-lockr-v1.0.js"));
        assert!(integration.is_lockr_sdk_url("https://aim.loc.kr/identity-lockr-trust-server.js"));
        assert!(integration.is_lockr_sdk_url("https://identity.loc.kr/identity-lockr-v2.0.js"));

        // Hosts that only end with a Lockr SDK host are different hosts.
        assert!(
            !integration.is_lockr_sdk_url("https://notaim.loc.kr/identity-lockr.js"), // allow-domain: notaim.loc.kr
            "should require an exact aim.loc.kr hostname"
        );
        assert!(
            !integration.is_lockr_sdk_url("https://notidentity.loc.kr/identity-lockr.js"), // allow-domain: notidentity.loc.kr
            "should require an exact identity.loc.kr hostname"
        );

        // Should not match non-SDK resources on Lockr domains
        assert!(
            !integration.is_lockr_sdk_url("https://aim.loc.kr/pixel.gif"),
            "should not match non-JS assets on aim.loc.kr"
        );
        assert!(
            !integration.is_lockr_sdk_url("https://aim.loc.kr/styles.css"),
            "should not match CSS files on aim.loc.kr"
        );
        assert!(
            !integration.is_lockr_sdk_url("https://identity.loc.kr/some-other-script.js"),
            "should not match non-SDK JS files on identity.loc.kr"
        );

        // Should not match other URLs
        assert!(
            !integration.is_lockr_sdk_url("https://example.com/script.js"),
            "should not match unrelated domains"
        );
    }

    #[test]
    fn test_default_sdk_url_uses_trust_server() {
        let url = default_sdk_url();
        assert!(
            url.contains("trust-server"),
            "should use the trust-server SDK variant by default"
        );
    }

    #[test]
    fn test_attribute_rewriter_rewrites_sdk_urls() {
        let integration = LockrIntegration::new(test_config());
        let ctx = test_context();

        let result = integration.rewrite("src", "https://aim.loc.kr/identity-lockr-v1.0.js", &ctx);

        assert_eq!(
            result,
            AttributeRewriteAction::Replace(
                "https://edge.example.com/integrations/lockr/sdk".to_string()
            ),
            "should rewrite Lockr SDK URL to absolute first-party proxy"
        );
    }

    #[test]
    fn test_attribute_rewriter_ignores_external_base_and_preserves_request_origin() {
        let integration = LockrIntegration::new(test_config());
        let external_base = url::Url::parse("https://external-base.example/assets/")
            .expect("should parse external base URL");

        for (scheme, host) in [
            ("https", "edge.example.com"),
            ("http", "edge.example.com:8080"),
            ("https", "[2001:db8::1]:8443"),
        ] {
            for (element_name, attribute_name) in [("script", "src"), ("link", "href")] {
                let ctx = IntegrationAttributeContext {
                    attribute_name,
                    element_name,
                    request_host: host,
                    request_scheme: scheme,
                    ..test_context()
                };
                assert!(
                    integration.handles_attribute(attribute_name),
                    "should handle SDK script and preload attributes"
                );
                let AttributeRewriteAction::Replace(replacement) = integration.rewrite(
                    attribute_name,
                    "https://aim.loc.kr/identity-lockr-trust-server.js",
                    &ctx,
                ) else {
                    panic!("should rewrite SDK URL");
                };

                let expected = format!("{scheme}://{host}/integrations/lockr/sdk");
                assert_eq!(
                    external_base
                        .join(&replacement)
                        .expect("should resolve rewritten SDK URL")
                        .as_str(),
                    expected,
                    "should preserve page scheme, host and port despite external base"
                );
            }
        }
    }

    #[test]
    fn test_attribute_rewriter_keeps_non_lockr_urls() {
        let integration = LockrIntegration::new(test_config());
        let ctx = test_context();

        let result = integration.rewrite("src", "https://example.com/other.js", &ctx);

        assert_eq!(
            result,
            AttributeRewriteAction::Keep,
            "should keep non-Lockr URLs unchanged"
        );
    }

    #[test]
    fn test_attribute_rewriter_noop_when_disabled() {
        let config = LockrConfig {
            rewrite_sdk: false,
            ..test_config()
        };
        let integration = LockrIntegration::new(config);
        let ctx = test_context();

        let result = integration.rewrite("src", "https://aim.loc.kr/identity-lockr-v1.0.js", &ctx);

        assert_eq!(
            result,
            AttributeRewriteAction::Keep,
            "should keep all URLs when rewrite_sdk is disabled"
        );
    }

    fn readback_config() -> LockrConfig {
        LockrConfig {
            api_endpoint: "https://identity.example.com".to_string(),
            sdk_url: "https://sdk.example.com/identity-lockr-trust-server.js".to_string(),
            ..test_config()
        }
    }

    #[test]
    fn lockr_sdk_url_with_fragment_is_fetched_without_it() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            Method::GET,
            "https://sdk.example.com/identity-lockr-trust-server.js",
            ScriptedReply::status(StatusCode::OK).body("window.lockr = {};"),
        );
        let services = build_services_with_outbound_client(&client, &clock);
        let integration = LockrIntegration::new(LockrConfig {
            sdk_url: "https://sdk.example.com/identity-lockr-trust-server.js#v2".to_string(),
            ..readback_config()
        });
        let req = http::Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/integrations/lockr/sdk")
            .body(EdgeBody::empty())
            .expect("should build SDK request");

        let response = block_on(integration.handle(&create_test_settings(), &services, req))
            .expect("should serve an SDK whose configured URL carries a fragment");

        assert_eq!(response.status(), StatusCode::OK, "should serve the SDK");
        assert_eq!(
            client
                .requests()
                .iter()
                .map(|request| request.uri.to_string())
                .collect::<Vec<_>>(),
            vec!["https://sdk.example.com/identity-lockr-trust-server.js"],
            "should fetch the SDK URL without its fragment"
        );
    }

    #[test]
    fn lockr_sdk_request_readback() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            Method::GET,
            "https://sdk.example.com/identity-lockr-trust-server.js",
            ScriptedReply::status(StatusCode::OK).body("window.lockr = {};"),
        );
        let services = build_services_with_outbound_client(&client, &clock);
        let integration = LockrIntegration::new(readback_config());
        let req = http::Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/integrations/lockr/sdk")
            .header(header::ACCEPT_ENCODING, "gzip")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .body(EdgeBody::empty())
            .expect("should build SDK request");

        let response = block_on(integration.handle(&create_test_settings(), &services, req))
            .expect("should serve the Lockr SDK");

        assert_eq!(
            response
                .into_body()
                .into_bytes()
                .expect("should buffer the SDK body")
                .as_ref(),
            b"window.lockr = {};",
            "should serve the fetched SDK bytes"
        );
        let requests = client.requests();
        assert_eq!(requests.len(), 1, "should send one SDK request");
        let sdk = &requests[0];
        assert_eq!(
            (&sdk.method, sdk.uri.to_string()),
            (
                &Method::GET,
                "https://sdk.example.com/identity-lockr-trust-server.js".to_owned()
            ),
            "should fetch the configured SDK URL"
        );
        sdk.assert_headers(&[
            ("user-agent", "TrustedServer/1.0"),
            ("accept", "application/javascript, */*"),
        ]);
        sdk.assert_transport(ExpectedTransport {
            timeout: BUFFERED_TIMEOUT,
            response_mode: ResponseMode::Buffered {
                max_bytes: 16 * 1024 * 1024,
            },
            response_decoding: ResponseDecoding::Decode,
            cache_policy: OutboundCachePolicy::PlatformDefault,
        });
    }

    #[test]
    fn lockr_api_request_readback() {
        // The POST body and content type must reach the Lockr API, which
        // answers `{"code":400,"message":"Invalid request"}` without them. The
        // publisher's `Authorization` and `Cookie` headers must not: the API
        // rejects an unexpected `Authorization` with the same 400, and both
        // carry publisher credentials that must not leak to a third party.
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        let target =
            "https://identity.example.com/publisher/app/v2/identityLockr/settings?source=example";
        client.respond_to(
            Method::POST,
            target,
            ScriptedReply::status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(r#"{"success":true,"data":{}}"#),
        );
        let services = build_services_with_outbound_client(&client, &clock);
        let integration = LockrIntegration::new(readback_config());
        let payload = br#"{"appID":"test-app-id"}"#;
        let req = http::Request::builder()
            .method(Method::POST)
            .uri("https://publisher.example.com/integrations/lockr/api/publisher/app/v2/identityLockr/settings?source=example")
            .header(header::CONTENT_TYPE, "application/json;charset=UTF-8")
            .header(header::CONTENT_LENGTH, payload.len().to_string())
            .header(header::ACCEPT, "application/json")
            .header(header::USER_AGENT, "example-agent")
            .header(header::ACCEPT_LANGUAGE, "en")
            .header(header::ACCEPT_ENCODING, "gzip")
            .header(header::ORIGIN, "https://publisher.example.com")
            .header("x-example-trace", "trace-1")
            .header("x-ts-ec", "internal-ec")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .header(header::COOKIE, "session_id=secret; euconsent-v2=tcf")
            .body(EdgeBody::from(payload.to_vec()))
            .expect("should build API request");

        let response = block_on(integration.handle(&create_test_settings(), &services, req))
            .expect("should proxy the Lockr API request");

        assert_eq!(response.status(), StatusCode::OK, "should relay the status");
        assert_eq!(
            response
                .into_body()
                .into_bytes()
                .expect("should buffer the API body")
                .as_ref(),
            br#"{"success":true,"data":{}}"#,
            "should relay the API body"
        );
        let requests = client.requests();
        assert_eq!(requests.len(), 1, "should send one API request");
        let api = &requests[0];
        assert_eq!(
            (&api.method, api.uri.to_string(), api.body.as_ref()),
            (&Method::POST, target.to_owned(), payload.as_slice()),
            "should forward the method, path, query, and body"
        );
        api.assert_headers(&[
            ("content-type", "application/json;charset=UTF-8"),
            ("accept", "application/json"),
            ("user-agent", "example-agent"),
            ("accept-language", "en"),
            ("accept-encoding", "gzip"),
            ("origin", "https://publisher.example.com"),
            ("x-example-trace", "trace-1"),
        ]);
        api.assert_transport(ExpectedTransport {
            timeout: BUFFERED_TIMEOUT,
            response_mode: ResponseMode::Buffered {
                max_bytes: DEFAULT_BUFFERED_RESPONSE_BYTES,
            },
            response_decoding: ResponseDecoding::Original,
            cache_policy: OutboundCachePolicy::PlatformDefault,
        });
    }

    #[test]
    fn test_api_path_extraction_preserves_casing() {
        let test_cases = [
            (
                "/integrations/lockr/api/publisher/app/v1/identityLockr/settings",
                "/publisher/app/v1/identityLockr/settings",
            ),
            (
                "/integrations/lockr/api/publisher/app/v1/identityLockr/page-view",
                "/publisher/app/v1/identityLockr/page-view",
            ),
            (
                "/integrations/lockr/api/publisher/app/v1/identityLockr/generate-tokens",
                "/publisher/app/v1/identityLockr/generate-tokens",
            ),
        ];

        for (input, expected) in test_cases {
            let result = input
                .strip_prefix("/integrations/lockr/api")
                .expect("should strip prefix");
            assert_eq!(
                result, expected,
                "should preserve casing for path: {}",
                input
            );
        }
    }

    #[test]
    fn test_routes_registered() {
        let integration = LockrIntegration::new(test_config());
        let routes = integration.routes();

        assert_eq!(routes.len(), 3, "should register 3 routes");

        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/lockr/sdk" && r.method == Method::GET),
            "should register SDK GET route"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/lockr/api/*" && r.method == Method::POST),
            "should register API POST route"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.path == "/integrations/lockr/api/*" && r.method == Method::GET),
            "should register API GET route"
        );
    }

    #[test]
    fn disabled_invalid_config_does_not_error() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                LOCKR_INTEGRATION_ID,
                &json!({
                    "enabled": false,
                    "app_id": "",
                    "sdk_url": "not a url",
                }),
            )
            .expect("should insert disabled invalid Lockr config");

        let registration = register(&settings).expect("disabled invalid Lockr config should skip");
        assert!(
            registration.is_none(),
            "disabled invalid Lockr config should not register"
        );
    }

    #[test]
    fn lockr_api_body_read_failures_map_to_statuses() {
        let settings = create_test_settings();
        let integration = LockrIntegration::new(test_config());
        for case in body_read_failure_cases(INTEGRATION_MAX_BODY_BYTES) {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::forbidding();
            let services = build_services_with_outbound_client(&client, &clock);
            let (req, expectation) = case.into_request(
                http::Request::builder()
                    .method(http::Method::POST)
                    .uri("https://publisher.example/integrations/lockr/api/publisher/app/v2/identityLockr/settings"),
            );

            let outcome = block_on(integration.handle(&settings, &services, req));

            expectation.assert_outcome("Lockr API", &outcome);
            client.assert_no_requests();
        }
    }
}
