use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use url::Url;
use validator::{Validate, ValidationError};

use crate::backend::BackendConfig;
use crate::error::TrustedServerError;
use crate::integrations::{
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration,
};
use crate::platform::RuntimeServices;
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
        original_req: &Request,
        proxy_req: &mut Request,
    ) {
        if let Some(client_ip) = original_req.get_client_ip_addr() {
            proxy_req.set_header("X-Forwarded-For", client_ip.to_string());
        }

        for header_name in [
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
            header::USER_AGENT,
            header::REFERER,
            header::ORIGIN,
            header::AUTHORIZATION,
        ] {
            if let Some(value) = original_req.get_header(&header_name) {
                proxy_req.set_header(&header_name, value);
            }
        }

        if matches!(backend, DidomiBackend::Sdk) {
            Self::copy_geo_headers(original_req, proxy_req);
        }
    }

    fn copy_geo_headers(original_req: &Request, proxy_req: &mut Request) {
        let geo_headers = [
            ("X-Geo-Country", "FastlyGeo-CountryCode"),
            ("X-Geo-Region", "FastlyGeo-Region"),
            ("CloudFront-Viewer-Country", "FastlyGeo-CountryCode"),
        ];

        for (target, source) in geo_headers {
            if let Some(value) = original_req.get_header(source) {
                proxy_req.set_header(target, value);
            }
        }
    }

    fn add_cors_headers(response: &mut Response) {
        response.set_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
        response.set_header(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            "Content-Type, Authorization, X-Requested-With",
        );
        response.set_header(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            "GET, POST, PUT, DELETE, OPTIONS",
        );
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
        _services: &RuntimeServices,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path();
        let prefix = self.resolved_prefix();
        let consent_path = path.strip_prefix(&prefix).unwrap_or(path);
        let backend = self.backend_for_path(consent_path);
        let base_origin = match backend {
            DidomiBackend::Sdk => self.config.sdk_origin.as_str(),
            DidomiBackend::Api => self.config.api_origin.as_str(),
        };

        let target_url = self
            .build_target_url(base_origin, consent_path, req.get_query_str())
            .change_context(Self::error("Failed to build Didomi target URL"))?;
        let backend_name = BackendConfig::from_url(base_origin, true)
            .change_context(Self::error("Failed to configure Didomi backend"))?;

        let mut proxy_req = Request::new(req.get_method().clone(), &target_url);
        self.copy_headers(&backend, &req, &mut proxy_req);

        if matches!(req.get_method(), &Method::POST | &Method::PUT) {
            if let Some(content_type) = req.get_header(header::CONTENT_TYPE) {
                proxy_req.set_header(header::CONTENT_TYPE, content_type);
            }
            proxy_req.set_body(req.into_body());
        }

        let mut response = proxy_req
            .send(&backend_name)
            .change_context(Self::error("Didomi upstream request failed"))?;

        if matches!(backend, DidomiBackend::Sdk) {
            Self::add_cors_headers(&mut response);
        }

        Ok(response)
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
    use super::*;
    use crate::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use crate::test_support::tests::create_test_settings;
    use fastly::http::Method;

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
