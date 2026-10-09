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
use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::{self, HeaderMap, HeaderValue};
use http::{Method, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use validator::Validate;

use crate::ec::identity::{IdentityConsentSignals, IdentityObservation, IdentityOutcome};
use crate::integrations::identity::{IdentityCapture, IdentityCaptureInput, IdentityRequestBody};

use crate::constants::INTERNAL_HEADERS;
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, INTEGRATION_MAX_BODY_BYTES, IntegrationAttributeContext,
    IntegrationAttributeRewriter, IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
    UPSTREAM_SDK_MAX_RESPONSE_BYTES, collect_body_bounded, collect_response_bounded,
    ensure_integration_backend,
};
use crate::platform::{PlatformHttpRequest, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

const LOCKR_INTEGRATION_ID: &str = "lockr";
const ID5_SOURCE: &str = "id5-sync.com";
const LOCKR_API_PREFIX: &str = "/integrations/lockr/api";
const LOCKR_TOKEN_PATHS: [&str; 4] = [
    "/publisher/app/v2/identityLockr/page-view",
    "/publisher/app/v2/identityLockr/generate-tokens",
    "/publisher/app/v2/identityLockr/refresh-tokens",
    "/publisher/app/v2/identityLockr/sync-no-hem-ids",
];
const LOCKR_REVOKE_PATH: &str = "/publisher/app/v2/identityLockr/revoke-consent";

/// Configuration for Lockr integration.
#[derive(Debug, Deserialize, Validate)]
pub struct LockrConfig {
    /// Enable/disable the integration
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Kill switch for token observation; does not remove the source claim.
    #[serde(default = "default_enabled")]
    pub capture_identity: bool,

    /// Separately approved global EC withdrawal from successful revoke responses.
    #[serde(default)]
    pub capture_global_withdrawal: bool,

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
        let lower = url.to_ascii_lowercase();
        (lower.contains("aim.loc.kr") || lower.contains("identity.loc.kr"))
            && lower.contains("identity-lockr")
            && lower.ends_with(".js")
    }

    /// Handle SDK serving — fetch from Lockr CDN and serve through first-party domain.
    async fn handle_sdk_serving(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let sdk_url = &self.config.sdk_url;
        log::info!("Fetching Lockr SDK");

        // TODO: Check KV store cache first (future enhancement)

        let lockr_req = http::Request::builder()
            .method(Method::GET)
            .uri(sdk_url)
            .header(header::USER_AGENT, "TrustedServer/1.0")
            .header(header::ACCEPT, "application/javascript, */*")
            .body(EdgeBody::empty())
            .map_err(|_| Report::new(Self::error("Failed to build Lockr SDK request")))?;

        let backend_name = Self::backend_name_for_url(services, sdk_url)
            .map_err(|_| Report::new(Self::error("Failed to determine backend for SDK fetch")))?;

        let lockr_response = services
            .http_client()
            .send(PlatformHttpRequest::new(lockr_req, backend_name))
            .await
            .map_err(|_| Report::new(Self::error("Failed to fetch Lockr SDK")))?
            .response;

        if !lockr_response.status().is_success() {
            log::error!(
                "Lockr SDK fetch failed with status {}",
                lockr_response.status()
            );
            return Err(Report::new(Self::error(format!(
                "Lockr SDK returned error status: {}",
                lockr_response.status()
            ))));
        }

        let sdk_body = collect_response_bounded(
            lockr_response.into_body(),
            UPSTREAM_SDK_MAX_RESPONSE_BYTES,
            LOCKR_INTEGRATION_ID,
        )
        .await
        .change_context(Self::error("Failed to read Lockr SDK response body"))?;
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
            .body(EdgeBody::from(sdk_body))
            .change_context(Self::error("Failed to build Lockr SDK response"))
    }

    /// Handle API proxy — forward requests to the configured Lockr API endpoint.
    async fn handle_api_proxy(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let (parts, body) = req.into_parts();
        let original_path = parts.uri.path().to_string();
        let method = parts.method.clone();

        log::info!("Proxying Lockr API request: {}", method);

        // Extract path after /integrations/lockr/api and pass through directly.
        // This allows the Lockr SDK to use any API endpoint without hardcoded mappings.
        let target_path = original_path
            .strip_prefix("/integrations/lockr/api")
            .ok_or_else(|| Self::error("Invalid Lockr API path"))?;

        let query = parts
            .uri
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        let target_url = format!("{}{}{}", self.config.api_endpoint, target_path, query);

        let capture_selected = self.matches(&method, &original_path);
        // The same allocation backs the forwarded POST and request-local capture signals.
        // Never read the browser request a second time for capture.
        let (request_body, capture_body) = if method == Method::POST {
            let bytes = Bytes::from(
                collect_body_bounded(body, INTEGRATION_MAX_BODY_BYTES, LOCKR_INTEGRATION_ID)
                    .await?,
            );
            let capture_body = capture_selected.then(|| IdentityRequestBody(bytes.clone()));
            (EdgeBody::from_bytes(bytes), capture_body)
        } else {
            (EdgeBody::empty(), None)
        };

        let mut target_req = http::Request::builder()
            .method(method.clone())
            .uri(&target_url)
            .body(request_body)
            .map_err(|_| Report::new(Self::error("Failed to build Lockr API proxy request")))?;
        self.copy_request_headers(&parts.headers, target_req.headers_mut())?;

        let backend_name = Self::backend_name_for_url(services, &self.config.api_endpoint)
            .map_err(|_| Report::new(Self::error("Failed to determine backend for API proxy")))?;

        let response = services
            .http_client()
            .send(PlatformHttpRequest::new(target_req, backend_name))
            .await
            .map_err(|_| Report::new(Self::error("Failed to forward Lockr API request")))?
            .response;

        log::info!("Lockr API responded with status {}", response.status());

        let mut response = response;
        if let Some(body) = capture_body {
            response.extensions_mut().insert(body);
        }
        Ok(response)
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
                Err(_) => {
                    log::warn!("Skipping invalid Lockr origin header");
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

    fn backend_name_for_url(
        services: &RuntimeServices,
        target_url: &str,
    ) -> Result<String, Report<TrustedServerError>> {
        ensure_integration_backend(services, target_url, LOCKR_INTEGRATION_ID, None)
    }
}

// Only the confirmed ID5 token schema is recognized. Unknown keys do not become
// source claims, even when an EID in the same response names a registered source.
fn id5_token(token: &Value, ids: Option<&Value>, now: Option<u64>) -> Option<IdentityOutcome> {
    if token.get("key_name")?.as_str()? != "id5id" {
        return None;
    }
    let encoded = token.get("advertising_token")?.as_str()?;
    // A malformed percent escape must not silently become a different UID.
    let bytes = encoded.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && (!bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                || !bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
        {
            return None;
        }
    }
    let decoded = urlencoding::decode(encoded).ok()?;
    let uid = serde_json::from_str::<Value>(&decoded)
        .ok()?
        .get("universal_uid")?
        .as_str()?
        .to_string();
    if uid.is_empty() || uid.len() > 512 {
        return None;
    }

    // An associated EID is corroboration, not a source of new authority.
    if let Some(ids) = ids {
        let eid = ids.as_object()?.get("id5id");
        if let Some(eid) = eid {
            if eid.get("source")?.as_str()? != ID5_SOURCE {
                return None;
            }
            let uids = eid.get("uids")?.as_array()?;
            if uids.len() != 1
                || uids[0].get("id")?.as_str()? != uid
                || uids[0].get("atype")?.as_u64()? != 1
            {
                return None;
            }
        }
    }

    let expires_at = match token.get("identity_expires") {
        None => None,
        Some(value) => {
            let millis = value.as_u64()?;
            let seconds = millis / 1000;
            // Without a working clock we cannot prove that a dated token is live.
            if seconds <= now? {
                return None;
            }
            Some(seconds)
        }
    };
    Some(IdentityOutcome::Issued {
        source: ID5_SOURCE.to_string(),
        uid,
        expires_at,
    })
}

fn request_consent(body: &[u8]) -> IdentityConsentSignals {
    let mut signals = IdentityConsentSignals::default();
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        signals.malformed = true;
        return signals;
    };
    let Some(object) = value.as_object() else {
        signals.malformed = true;
        return signals;
    };
    for (field, target) in [
        ("consentString", &mut signals.tcf),
        ("gppString", &mut signals.gpp),
        ("ccpaString", &mut signals.usp),
    ] {
        if let Some(value) = object.get(field) {
            match value.as_str() {
                Some("") => {}
                Some(text) => *target = Some(text.to_string()),
                None => signals.malformed = true,
            }
        }
    }
    signals
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
            .with_identity_capture(integration.clone())
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
            Err(Report::new(Self::error("Unknown Lockr route")))
        }
    }
}

impl IdentityCapture for LockrIntegration {
    fn sources(&self) -> &'static [&'static str] {
        &[ID5_SOURCE]
    }

    // The trait's enabled switch controls capture, not the entire integration proxy.
    #[allow(clippy::misnamed_getters)]
    fn enabled(&self) -> bool {
        self.config.capture_identity
    }

    fn matches(&self, method: &Method, path: &str) -> bool {
        if *method != Method::POST || !self.enabled() {
            return false;
        }
        let Some(path) = path.strip_prefix(LOCKR_API_PREFIX) else {
            return false;
        };
        LOCKR_TOKEN_PATHS.contains(&path)
            || (self.config.capture_global_withdrawal && path == LOCKR_REVOKE_PATH)
    }

    fn observe(&self, input: IdentityCaptureInput<'_>) -> Option<IdentityObservation> {
        if !self.config.capture_identity || !input.status.is_success() {
            return None;
        }
        let response: Value = serde_json::from_slice(input.decoded_body).ok()?;
        if !response.get("success")?.as_bool()? {
            return None;
        }
        let path = input.path.strip_prefix(LOCKR_API_PREFIX)?;
        if path == LOCKR_REVOKE_PATH {
            return self
                .config
                .capture_global_withdrawal
                .then(|| IdentityObservation {
                    outcomes: vec![IdentityOutcome::ConsentWithdrawn],
                    consent: IdentityConsentSignals::default(),
                });
        }
        if !LOCKR_TOKEN_PATHS.contains(&path) {
            return None;
        }
        let tokens = match response.get("aimTokens") {
            None => &[][..],
            Some(tokens) => tokens.as_array()?.as_slice(),
        };
        let ids = response.get("ids");
        if ids.is_some_and(|value| !value.is_object()) {
            return None;
        }
        let now = crate::ec::checked_current_timestamp();
        let outcomes = tokens
            .iter()
            .filter_map(|token| id5_token(token, ids, now))
            .collect::<Vec<_>>();
        // The provider has not defined ordering between overlapping tokens.
        // Never choose an arbitrary winner for this one-source mapping.
        if outcomes.len() > 1 {
            return None;
        }
        let consent = request_consent(input.request_body);
        // Bad request JSON or signal shapes may not acquire an ID, even if the
        // upstream returned one. Revocation above is deliberately independent.
        if consent.malformed {
            return None;
        }
        Some(IdentityObservation {
            outcomes: if outcomes.is_empty() {
                vec![IdentityOutcome::NoChange]
            } else {
                outcomes
            },
            consent,
        })
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
        _ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if !self.config.rewrite_sdk {
            return AttributeRewriteAction::Keep;
        }

        if self.is_lockr_sdk_url(attr_value) {
            // Root-relative so the browser resolves it against the page host.
            // Note: a page-level `<base href>` participates in this resolution,
            // so on pages that set an external base URL these resolve against
            // that base rather than the address-bar origin — an accepted
            // tradeoff, matching GTM/Didomi/Testlight which are also relative.
            let replacement = "/integrations/lockr/sdk".to_string();
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
    use std::sync::Arc;

    use super::*;
    use edgezero_core::http::Method as HttpMethod;
    use serde_json::json;

    use crate::platform::test_support::{StubHttpClient, build_services_with_http_client};
    use crate::test_support::tests::create_test_settings;

    fn test_config() -> LockrConfig {
        LockrConfig {
            enabled: true,
            capture_identity: true,
            capture_global_withdrawal: false,
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
    fn test_lockr_sdk_url_detection() {
        let integration = LockrIntegration::new(test_config());

        // Should match Lockr SDK URLs
        assert!(integration.is_lockr_sdk_url("https://aim.loc.kr/identity-lockr-v1.0.js"));
        assert!(integration.is_lockr_sdk_url("https://aim.loc.kr/identity-lockr-trust-server.js"));
        assert!(integration.is_lockr_sdk_url("https://identity.loc.kr/identity-lockr-v2.0.js"));

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
            AttributeRewriteAction::Replace("/integrations/lockr/sdk".to_string()),
            "should rewrite Lockr SDK URL to root-relative first-party proxy"
        );
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

    #[test]
    fn lockr_proxy_uses_platform_http_client() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"ok".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = LockrIntegration::new(test_config());
        let req = http::Request::builder()
            .method(HttpMethod::GET)
            .uri("https://publisher.example/integrations/lockr/api/publisher/app/v1/identityLockr/settings")
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
            stub.recorded_backend_names().len(),
            1,
            "should route one outbound request through PlatformHttpClient"
        );
    }

    #[test]
    fn lockr_proxy_forwards_body_and_strips_publisher_credentials() {
        // Regression guard for the upstream-rejection / credential-leak causes:
        // 1. The POST body (and content-type) must be forwarded, otherwise the
        //    Lockr API returns `{"code":400,"message":"Invalid request"}`.
        // 2. The publisher's `Authorization` header (e.g. site basic-auth) must
        //    NOT be forwarded — the Lockr API rejects it with the same 400, and
        //    forwarding it would leak the publisher credential to a third party.
        // 3. The publisher's `Cookie` header (session/auth cookies the browser
        //    attaches to the first-party route) must NOT be forwarded either.
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, br#"{"success":true,"data":{}}"#.to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = LockrIntegration::new(test_config());

        let payload = br#"{"appID":"test-app-id"}"#;
        let req = http::Request::builder()
            .method(HttpMethod::POST)
            .uri("https://publisher.example/integrations/lockr/api/publisher/app/v2/identityLockr/settings")
            .header(header::CONTENT_TYPE, "application/json;charset=UTF-8")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .header(header::COOKIE, "session_id=secret; euconsent-v2=tcf")
            .body(EdgeBody::from(payload.to_vec()))
            .expect("should build request");

        let response = futures::executor::block_on(integration.handle(&settings, &services, req))
            .expect("should proxy request");
        assert_eq!(response.status(), http::StatusCode::OK, "should return OK");
        assert!(response.extensions().get::<IdentityRequestBody>().is_none());

        let bodies = stub.recorded_request_bodies();
        assert_eq!(
            bodies.len(),
            1,
            "should forward exactly one upstream request"
        );
        assert_eq!(
            bodies[0], payload,
            "should forward the POST body unchanged to the Lockr API"
        );

        let headers = stub.recorded_request_headers();
        assert!(
            headers[0]
                .iter()
                .any(|(name, value)| name == "content-type"
                    && value == "application/json;charset=UTF-8"),
            "should forward the content-type header to the Lockr API"
        );
        assert!(
            !headers[0]
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("authorization")),
            "should NOT forward the publisher's Authorization header to the Lockr API"
        );
        assert!(
            !headers[0]
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cookie")),
            "should NOT forward the publisher's Cookie header to the Lockr API"
        );
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

    const TOKEN_PATH: &str = "/integrations/lockr/api/publisher/app/v2/identityLockr/page-view";
    const REVOKE_PATH: &str =
        "/integrations/lockr/api/publisher/app/v2/identityLockr/revoke-consent";

    fn sample_token() -> Value {
        json!({
            "success": true,
            "aimTokens": [{
                "key_name": "id5id",
                "advertising_token": "%7B%22universal_uid%22%3A%22ID5*synthetic%22%7D",
                "identity_expires": 4102444800000_u64
            }]
        })
    }

    fn observed(
        integration: &LockrIntegration,
        path: &str,
        response: &Value,
        request: &[u8],
    ) -> Option<IdentityObservation> {
        let decoded = serde_json::to_vec(response).expect("should serialize fixture");
        integration.observe(IdentityCaptureInput {
            path,
            request_body: request,
            status: StatusCode::OK,
            decoded_body: &decoded,
        })
    }

    #[test]
    fn capture_route_and_kill_switch() {
        let integration = LockrIntegration::new(test_config());
        for route in LOCKR_TOKEN_PATHS {
            let path = format!("{LOCKR_API_PREFIX}{route}");
            assert!(integration.matches(&Method::POST, &path));
            assert!(!integration.matches(&Method::GET, &path));
            assert!(!integration.matches(&Method::POST, &format!("{path}/extra")));
        }
        assert!(!integration.matches(&Method::POST, REVOKE_PATH));
        assert!(!integration.matches(&Method::POST, "/integrations/lockr/sdk"));
        assert!(!integration.matches(
            &Method::POST,
            "/integrations/lockr/api/publisher/app/v2/identityLockr/settings"
        ));
        let killed = LockrIntegration::new(LockrConfig {
            capture_identity: false,
            ..test_config()
        });
        assert_eq!(killed.sources(), &[ID5_SOURCE]);
        assert!(!killed.enabled());
        assert!(!killed.matches(&Method::POST, TOKEN_PATH));
    }

    #[test]
    fn id5_capture_validates_associated_eid_expiry_and_uid() {
        let integration = LockrIntegration::new(test_config());
        let request = br#"{"consentString":"","gppString":"","ccpaString":""}"#;
        let result = observed(&integration, TOKEN_PATH, &sample_token(), request)
            .expect("should capture synthetic ID5 token");
        assert!(
            matches!(&result.outcomes[..], [IdentityOutcome::Issued { source, uid, expires_at: Some(4102444800) }] if source == ID5_SOURCE && uid == "ID5*synthetic")
        );
        assert!(result.consent.tcf.is_none());
        let mut with_eid = sample_token();
        with_eid["ids"] = json!({"id5id":{"eid":{"source":"id5-sync.com","uids":[{"id":"ID5*synthetic","atype":1}]}}});
        assert_eq!(
            observed(&integration, TOKEN_PATH, &with_eid, request)
                .expect("should capture matching EID")
                .outcomes
                .len(),
            1
        );
        with_eid["ids"]["id5id"]["eid"]["uids"][0]["id"] = json!("different");
        assert!(matches!(
            &observed(&integration, TOKEN_PATH, &with_eid, request)
                .expect("should treat mismatched EID as no change")
                .outcomes[..],
            [IdentityOutcome::NoChange]
        ));
        let mut changed_expiry = sample_token();
        changed_expiry["aimTokens"][0]["identity_expires"] = json!(4102444801000_u64);
        assert!(matches!(
            &observed(&integration, TOKEN_PATH, &changed_expiry, request)
                .expect("should observe extended expiry")
                .outcomes[..],
            [IdentityOutcome::Issued { uid, expires_at: Some(4102444801), .. }] if uid == "ID5*synthetic"
        ));
        let mut mismatch = sample_token();
        mismatch["ids"] = json!({"id5id":{"eid":{"source":"other.example","uids":[{"id":"ID5*synthetic","atype":1}]}}});
        assert!(matches!(
            &observed(&integration, TOKEN_PATH, &mismatch, request)
                .expect("should reject mismatched source")
                .outcomes[..],
            [IdentityOutcome::NoChange]
        ));
        let mut expired = sample_token();
        expired["aimTokens"][0]["identity_expires"] = json!(1000);
        assert!(matches!(
            &observed(&integration, TOKEN_PATH, &expired, request)
                .expect("should reject expired token")
                .outcomes[..],
            [IdentityOutcome::NoChange]
        ));
        let mut missing = sample_token();
        missing["aimTokens"][0]
            .as_object_mut()
            .expect("should access fixture token")
            .remove("identity_expires");
        assert!(matches!(
            &observed(&integration, TOKEN_PATH, &missing, request)
                .expect("should capture token without expiry")
                .outcomes[..],
            [IdentityOutcome::Issued {
                expires_at: None,
                ..
            }]
        ));
        let mut unknown = sample_token();
        unknown["aimTokens"][0]["key_name"] = json!("liveramp");
        assert!(matches!(
            &observed(&integration, TOKEN_PATH, &unknown, request)
                .expect("should ignore unknown provider key")
                .outcomes[..],
            [IdentityOutcome::NoChange]
        ));
        let mut huge = sample_token();
        huge["aimTokens"][0]["advertising_token"] = json!(
            urlencoding::encode(&json!({"universal_uid":"x".repeat(513)}).to_string()).to_string()
        );
        assert!(matches!(
            &observed(&integration, TOKEN_PATH, &huge, request)
                .expect("should reject oversized UID")
                .outcomes[..],
            [IdentityOutcome::NoChange]
        ));
    }

    #[test]
    fn acquisition_requires_valid_request_json_and_signal_shapes() {
        let integration = LockrIntegration::new(test_config());
        assert!(observed(&integration, TOKEN_PATH, &sample_token(), b"{").is_none());
        assert!(
            observed(
                &integration,
                TOKEN_PATH,
                &sample_token(),
                br#"{"gppString":4}"#
            )
            .is_none()
        );
        let signals = observed(
            &integration,
            TOKEN_PATH,
            &sample_token(),
            br#"{"consentString":"opt-out","gppString":"gpp","ccpaString":"usp"}"#,
        )
        .expect("should parse populated consent signals")
        .consent;
        assert_eq!(signals.tcf.as_deref(), Some("opt-out"));
        assert_eq!(signals.gpp.as_deref(), Some("gpp"));
        assert_eq!(signals.usp.as_deref(), Some("usp"));
        assert!(
            observed(
                &integration,
                TOKEN_PATH,
                &json!({"success":true,"aimTokens":[]}),
                b"{}"
            )
            .is_some()
        );
        assert!(
            observed(
                &integration,
                TOKEN_PATH,
                &json!({"success":false,"aimTokens":[]}),
                b"{}"
            )
            .is_none()
        );
        assert!(
            observed(
                &integration,
                TOKEN_PATH,
                &json!({"success":true,"aimTokens":{}}),
                b"{}"
            )
            .is_none()
        );
    }

    #[test]
    fn revoke_needs_explicit_global_approval_and_application_success() {
        let integration = LockrIntegration::new(LockrConfig {
            capture_global_withdrawal: true,
            ..test_config()
        });
        assert!(integration.matches(&Method::POST, REVOKE_PATH));
        assert!(matches!(
            &observed(&integration, REVOKE_PATH, &json!({"success":true}), b"{")
                .expect("should observe approved explicit withdrawal")
                .outcomes[..],
            [IdentityOutcome::ConsentWithdrawn]
        ));
        assert!(observed(&integration, REVOKE_PATH, &json!({"success":false}), b"{}").is_none());
        let default = LockrIntegration::new(test_config());
        assert!(observed(&default, REVOKE_PATH, &json!({"success":true}), b"{}").is_none());
    }

    #[test]
    fn selected_proxy_reuses_forwarded_body_and_preserves_response() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"synthetic upstream".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let integration = LockrIntegration::new(test_config());
        let payload = br#"{"consentString":"synthetic"}"#;
        let req = http::Request::builder()
            .method(Method::POST)
            .uri(format!("https://publisher.example{TOKEN_PATH}?id=private"))
            .header(header::COOKIE, "session=private")
            .header(header::AUTHORIZATION, "Bearer private")
            .body(EdgeBody::from(payload.to_vec()))
            .expect("should build selected proxy request");
        let response = futures::executor::block_on(integration.handle(
            &create_test_settings(),
            &services,
            req,
        ))
        .expect("should forward selected proxy request");
        assert_eq!(stub.recorded_request_bodies(), vec![payload.to_vec()]);
        let headers = stub.recorded_request_headers();
        assert!(
            !headers[0]
                .iter()
                .any(|(key, _)| key == "cookie" || key == "authorization")
        );
        assert_eq!(
            response
                .extensions()
                .get::<IdentityRequestBody>()
                .expect("should attach shared forwarding bytes")
                .0
                .as_ref(),
            payload
        );
        assert!(
            matches!(response.into_body(), EdgeBody::Once(bytes) if bytes.as_ref() == b"synthetic upstream")
        );
    }

    #[test]
    fn malformed_or_failed_upstream_does_not_acquire_or_revoke() {
        let integration = LockrIntegration::new(LockrConfig {
            capture_global_withdrawal: true,
            ..test_config()
        });
        for path in [TOKEN_PATH, REVOKE_PATH] {
            assert!(
                integration
                    .observe(IdentityCaptureInput {
                        path,
                        request_body: b"{}",
                        status: StatusCode::BAD_REQUEST,
                        decoded_body: br#"{"success":true}"#,
                    })
                    .is_none()
            );
            assert!(
                integration
                    .observe(IdentityCaptureInput {
                        path,
                        request_body: b"{}",
                        status: StatusCode::OK,
                        decoded_body: b"{",
                    })
                    .is_none()
            );
        }
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
}
