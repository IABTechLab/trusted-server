use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::{self, HeaderMap, HeaderValue};
use http::Method;
use serde::{Deserialize, Serialize};
use url::Url;
use validator::{Validate, ValidationError};

use crate::error::TrustedServerError;
use crate::integrations::{
    collect_body_bounded, ensure_integration_backend, IntegrationEndpoint, IntegrationHeadInjector,
    IntegrationHtmlContext, IntegrationProxy, IntegrationRegistration, INTEGRATION_MAX_BODY_BYTES,
};
use crate::platform::{PlatformHttpRequest, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

const DIDOMI_INTEGRATION_ID: &str = "didomi";
const DIDOMI_DEFAULT_PREFIX: &str = "/integrations/didomi/consent";

/// Configuration for the Didomi consent notice reverse proxy.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct DidomiIntegrationConfig {
    /// Whether the integration is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Custom proxy path prefix to avoid ad-blocker detection.
    /// Defaults to "integrations/didomi/consent" if not set.
    #[serde(default)]
    #[validate(custom(function = "validate_proxy_path"))]
    pub proxy_path: Option<String>,
    /// Base URL for the Didomi SDK origin.
    #[serde(default = "default_sdk_origin")]
    #[validate(url)]
    pub sdk_origin: String,
    /// Base URL for the Didomi API origin.
    #[serde(default = "default_api_origin")]
    #[validate(url)]
    pub api_origin: String,
}

/// Validates the optional `proxy_path` value.
/// Rejects empty, root-only, trailing-slash, dot-segment, and values
/// containing characters that are unsafe for URL path routing.
fn validate_proxy_path(value: &str) -> Result<(), ValidationError> {
    let trimmed = value.trim_start_matches('/');

    if trimmed.is_empty() {
        return Err(ValidationError::new("proxy_path_empty"));
    }

    if trimmed.ends_with('/') {
        return Err(ValidationError::new("proxy_path_trailing_slash"));
    }

    if trimmed.contains("//") {
        return Err(ValidationError::new("proxy_path_double_slash"));
    }

    if trimmed
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(ValidationError::new("proxy_path_dot_segment"));
    }

    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~' | '/'))
    {
        return Err(ValidationError::new("proxy_path_forbidden_chars"));
    }

    Ok(())
}

impl IntegrationConfig for DidomiIntegrationConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    true
}

fn default_sdk_origin() -> String {
    "https://sdk.privacy-center.org".to_string()
}

fn default_api_origin() -> String {
    "https://api.privacy-center.org".to_string()
}

enum DidomiBackend {
    Sdk,
    Api,
}

struct DidomiIntegration {
    config: Arc<DidomiIntegrationConfig>,
}

impl DidomiIntegration {
    fn new(config: Arc<DidomiIntegrationConfig>) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: DIDOMI_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    /// Returns the canonicalized proxy prefix: always starts with `/`, no trailing slash.
    fn resolved_prefix(&self) -> String {
        match &self.config.proxy_path {
            Some(custom) => format!("/{}", custom.trim_start_matches('/')),
            None => DIDOMI_DEFAULT_PREFIX.to_string(),
        }
    }

    fn backend_for_path(&self, consent_path: &str) -> DidomiBackend {
        if consent_path.starts_with("/api/") {
            DidomiBackend::Api
        } else {
            DidomiBackend::Sdk
        }
    }

    fn build_target_url(
        &self,
        base: &str,
        consent_path: &str,
        query: Option<&str>,
    ) -> Result<String, Report<TrustedServerError>> {
        let mut target =
            Url::parse(base).change_context(Self::error("Invalid Didomi origin URL"))?;
        let path = if consent_path.is_empty() {
            "/"
        } else {
            consent_path
        };
        target.set_path(path);
        target.set_query(query);
        Ok(target.to_string())
    }

    fn copy_headers(
        &self,
        backend: &DidomiBackend,
        client_ip: Option<std::net::IpAddr>,
        original_headers: &HeaderMap<HeaderValue>,
        proxy_headers: &mut HeaderMap<HeaderValue>,
    ) {
        if let Some(ip) = client_ip {
            proxy_headers.insert(
                "X-Forwarded-For",
                HeaderValue::from_str(&ip.to_string())
                    .expect("should format X-Forwarded-For header"),
            );
        }

        // `Authorization` is intentionally NOT forwarded: it carries the
        // publisher site's own credential (e.g. staging basic-auth), which would
        // leak to the third-party upstream and can break APIs that reject an
        // unexpected `Authorization` header.
        for header_name in [
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
            header::CONTENT_TYPE,
            header::USER_AGENT,
            header::REFERER,
            header::ORIGIN,
        ] {
            if let Some(value) = original_headers.get(&header_name) {
                proxy_headers.insert(header_name, value.clone());
            }
        }

        if matches!(backend, DidomiBackend::Sdk) {
            Self::copy_geo_headers(original_headers, proxy_headers);
        }
    }

    fn copy_geo_headers(
        original_headers: &HeaderMap<HeaderValue>,
        proxy_headers: &mut HeaderMap<HeaderValue>,
    ) {
        let geo_headers = [
            ("X-Geo-Country", "FastlyGeo-CountryCode"),
            ("X-Geo-Region", "FastlyGeo-Region"),
            ("CloudFront-Viewer-Country", "FastlyGeo-CountryCode"),
        ];

        for (target, source) in geo_headers {
            if let Some(value) = original_headers.get(source) {
                proxy_headers.insert(target, value.clone());
            }
        }
    }

    fn add_cors_headers(response: &mut http::Response<EdgeBody>) {
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Content-Type, Authorization, X-Requested-With"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
        );
    }

    fn backend_name_for_origin(
        services: &RuntimeServices,
        origin: &str,
    ) -> Result<String, Report<TrustedServerError>> {
        ensure_integration_backend(services, origin, DIDOMI_INTEGRATION_ID, None)
    }
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<DidomiIntegration>>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<DidomiIntegrationConfig>(DIDOMI_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    Ok(Some(DidomiIntegration::new(Arc::new(config))))
}

/// Register the Didomi consent notice integration when enabled.
///
/// # Errors
///
/// Returns an error when the Didomi integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(DIDOMI_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_head_injector(integration)
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for DidomiIntegration {
    fn integration_name(&self) -> &'static str {
        DIDOMI_INTEGRATION_ID
    }

    fn proxy_prefix(&self) -> String {
        self.resolved_prefix()
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get("/*"), self.post("/*")]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let (parts, body) = req.into_parts();
        let path = parts.uri.path().to_string();
        let prefix = self.resolved_prefix();
        let consent_path = path.strip_prefix(&prefix).unwrap_or(&path);
        let backend = self.backend_for_path(consent_path);
        let base_origin = match backend {
            DidomiBackend::Sdk => self.config.sdk_origin.as_str(),
            DidomiBackend::Api => self.config.api_origin.as_str(),
        };

        let target_url = self
            .build_target_url(base_origin, consent_path, parts.uri.query())
            .change_context(Self::error("Failed to build Didomi target URL"))?;
        let backend_name = Self::backend_name_for_origin(services, base_origin)
            .change_context(Self::error("Failed to configure Didomi backend"))?;

        let request_body = if parts.method == Method::POST {
            let bytes =
                collect_body_bounded(body, INTEGRATION_MAX_BODY_BYTES, DIDOMI_INTEGRATION_ID)
                    .await?;
            EdgeBody::from(bytes)
        } else {
            EdgeBody::empty()
        };

        let mut proxy_req = http::Request::builder()
            .method(parts.method.clone())
            .uri(&target_url)
            .body(request_body)
            .change_context(Self::error("Failed to build Didomi proxy request"))?;
        self.copy_headers(
            &backend,
            services.client_info().client_ip,
            &parts.headers,
            proxy_req.headers_mut(),
        );

        let mut response = services
            .http_client()
            .send(PlatformHttpRequest::new(proxy_req, backend_name))
            .await
            .change_context(Self::error("Didomi upstream request failed"))?;

        if matches!(backend, DidomiBackend::Sdk) {
            Self::add_cors_headers(&mut response.response);
        }

        Ok(response.response)
    }
}

impl IntegrationHeadInjector for DidomiIntegration {
    fn integration_id(&self) -> &'static str {
        DIDOMI_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct InjectedDidomiClientConfig {
            proxy_path: String,
        }

        let payload = InjectedDidomiClientConfig {
            proxy_path: format!("{}/", self.resolved_prefix()),
        };

        // Escape `</` to prevent breaking out of the script tag.
        let config_json = serde_json::to_string(&payload)
            .unwrap_or_else(|e| {
                log::warn!("Didomi: failed to serialize client config: {e}");
                "{}".to_string()
            })
            .replace("</", "<\\/");

        vec![format!(
            r#"<script>window.__tsjs_didomi={config_json};</script>"#
        )]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use crate::platform::test_support::{build_services_with_http_client, StubHttpClient};
    use crate::test_support::tests::create_test_settings;
    use http::Method;
    use std::net::{IpAddr, Ipv4Addr};

    fn config(enabled: bool) -> DidomiIntegrationConfig {
        DidomiIntegrationConfig {
            enabled,
            proxy_path: None,
            sdk_origin: default_sdk_origin(),
            api_origin: default_api_origin(),
        }
    }

    #[test]
    fn selects_api_backend_for_api_paths() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        assert!(matches!(
            integration.backend_for_path("/api/events"),
            DidomiBackend::Api
        ));
        assert!(matches!(
            integration.backend_for_path("/24cd/loader.js"),
            DidomiBackend::Sdk
        ));
    }

    #[test]
    fn builds_target_url_with_query() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let url = integration
            .build_target_url("https://sdk.privacy-center.org", "/loader.js", Some("v=1"))
            .expect("should build target URL");
        assert_eq!(url, "https://sdk.privacy-center.org/loader.js?v=1");
    }

    #[test]
    fn registers_prefix_routes() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(DIDOMI_INTEGRATION_ID, &config(true))
            .expect("should insert config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        assert!(registry.has_route(&Method::GET, "/integrations/didomi/consent/loader.js"));
        assert!(registry.has_route(&Method::POST, "/integrations/didomi/consent/api/events"));
        assert!(!registry.has_route(&Method::GET, "/other"));
    }

    #[test]
    fn copy_headers_sets_x_forwarded_for_from_client_ip() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let backend = DidomiBackend::Sdk;
        let original_req = http::Request::builder()
            .method(Method::GET)
            .uri("https://example.com/test")
            .body(EdgeBody::empty())
            .expect("should build original request");
        let mut proxy_req = http::Request::builder()
            .method(Method::GET)
            .uri("https://sdk.privacy-center.org/test")
            .body(EdgeBody::empty())
            .expect("should build proxy request");
        let client_ip = Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)));

        integration.copy_headers(
            &backend,
            client_ip,
            original_req.headers(),
            proxy_req.headers_mut(),
        );

        assert_eq!(
            proxy_req
                .headers()
                .get("X-Forwarded-For")
                .and_then(|v| v.to_str().ok()),
            Some("1.2.3.4"),
            "should set X-Forwarded-For from client_ip"
        );
    }

    #[test]
    fn copy_headers_strips_authorization() {
        // Security regression guard: the publisher's Authorization header must
        // not be forwarded to the Didomi upstream (credential leak).
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let backend = DidomiBackend::Api;
        let original_req = http::Request::builder()
            .method(Method::POST)
            .uri("https://example.com/test")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .header(header::USER_AGENT, "test-agent")
            .body(EdgeBody::empty())
            .expect("should build original request");
        let mut proxy_req = http::Request::builder()
            .method(Method::POST)
            .uri("https://api.privacy-center.org/test")
            .body(EdgeBody::empty())
            .expect("should build proxy request");

        integration.copy_headers(
            &backend,
            None,
            original_req.headers(),
            proxy_req.headers_mut(),
        );

        assert!(
            proxy_req.headers().get(header::AUTHORIZATION).is_none(),
            "should NOT forward the publisher's Authorization header to Didomi"
        );
        assert_eq!(
            proxy_req
                .headers()
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok()),
            Some("test-agent"),
            "should still forward required headers (user-agent)"
        );
    }

    #[test]
    fn registers_custom_proxy_path() {
        let mut settings = create_test_settings();
        let custom_config = DidomiIntegrationConfig {
            enabled: true,
            proxy_path: Some("my-custom-consent".to_string()),
            sdk_origin: default_sdk_origin(),
            api_origin: default_api_origin(),
        };
        settings
            .integrations
            .insert_config(DIDOMI_INTEGRATION_ID, &custom_config)
            .expect("should insert config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        assert!(registry.has_route(&Method::GET, "/my-custom-consent/loader.js"));
        assert!(registry.has_route(&Method::POST, "/my-custom-consent/api/events"));
        assert!(!registry.has_route(&Method::GET, "/integrations/didomi/consent/loader.js"));
    }

    #[test]
    fn validates_proxy_path_rejects_empty() {
        assert!(validate_proxy_path("").is_err());
        assert!(validate_proxy_path("/").is_err());
    }

    #[test]
    fn validates_proxy_path_rejects_trailing_slash() {
        assert!(validate_proxy_path("my-path/").is_err());
    }

    #[test]
    fn validates_proxy_path_rejects_forbidden_chars() {
        assert!(validate_proxy_path("path?query").is_err());
        assert!(validate_proxy_path("path#frag").is_err());
        assert!(validate_proxy_path("{param}").is_err());
        assert!(validate_proxy_path("wild*card").is_err());
        assert!(validate_proxy_path("has space").is_err());
        assert!(validate_proxy_path("has\"quote").is_err());
        assert!(validate_proxy_path("has\\backslash").is_err());
        assert!(validate_proxy_path("has\nnewline").is_err());
        assert!(validate_proxy_path("encoded%2e%2e/path").is_err());
    }

    #[test]
    fn validates_proxy_path_rejects_double_slash() {
        assert!(validate_proxy_path("my//path").is_err());
    }

    #[test]
    fn validates_proxy_path_rejects_dot_segments() {
        assert!(validate_proxy_path("my/./path").is_err());
        assert!(validate_proxy_path("my/../path").is_err());
    }

    #[test]
    fn validates_proxy_path_accepts_valid() {
        assert!(validate_proxy_path("my-custom-path").is_ok());
        assert!(validate_proxy_path("nested/path/here").is_ok());
        assert!(validate_proxy_path("/leading-slash-ok").is_ok());
    }

    #[test]
    fn head_injector_emits_proxy_path() {
        let custom_config = DidomiIntegrationConfig {
            enabled: true,
            proxy_path: Some("my-consent".to_string()),
            sdk_origin: default_sdk_origin(),
            api_origin: default_api_origin(),
        };
        let integration = DidomiIntegration::new(Arc::new(custom_config));
        let doc_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "example.com",
            request_scheme: "https",
            origin_host: "example.com",
            document_state: &doc_state,
        };
        let inserts = integration.head_inserts(&ctx);
        assert_eq!(inserts.len(), 1);
        assert_eq!(
            inserts[0],
            r#"<script>window.__tsjs_didomi={"proxyPath":"/my-consent/"};</script>"#
        );
    }

    #[test]
    fn copy_headers_omits_x_forwarded_for_when_no_client_ip() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let backend = DidomiBackend::Sdk;
        let original_req = http::Request::builder()
            .method(Method::GET)
            .uri("https://example.com/test")
            .body(EdgeBody::empty())
            .expect("should build original request");
        let mut proxy_req = http::Request::builder()
            .method(Method::GET)
            .uri("https://sdk.privacy-center.org/test")
            .body(EdgeBody::empty())
            .expect("should build proxy request");

        integration.copy_headers(
            &backend,
            None,
            original_req.headers(),
            proxy_req.headers_mut(),
        );

        assert!(
            proxy_req.headers().get("X-Forwarded-For").is_none(),
            "should omit X-Forwarded-For when client_ip is None"
        );
    }

    #[test]
    fn didomi_proxy_uses_platform_http_client() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"ok".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://publisher.example/integrations/didomi/consent/api/events")
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

    #[test]
    fn head_injector_default_path() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let doc_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "example.com",
            request_scheme: "https",
            origin_host: "example.com",
            document_state: &doc_state,
        };
        let inserts = integration.head_inserts(&ctx);
        assert_eq!(
            inserts[0],
            r#"<script>window.__tsjs_didomi={"proxyPath":"/integrations/didomi/consent/"};</script>"#
        );
    }
}
