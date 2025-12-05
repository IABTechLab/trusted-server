use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use url::Url;
use validator::Validate;

use crate::backend::ensure_backend_from_url;
use crate::error::TrustedServerError;
use crate::integrations::{IntegrationEndpoint, IntegrationProxy, IntegrationRegistration};
use crate::settings::{IntegrationConfig, Settings};

const DIDOMI_INTEGRATION_ID: &str = "didomi";
const DIDOMI_PREFIX: &str = "/didomi/consent";

/// Configuration for the Didomi consent notice reverse proxy.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct DidomiIntegrationConfig {
    /// Whether the integration is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Base URL for the Didomi SDK origin.
    #[serde(default = "default_sdk_origin")]
    #[validate(url)]
    pub sdk_origin: String,
    /// Base URL for the Didomi API origin.
    #[serde(default = "default_api_origin")]
    #[validate(url)]
    pub api_origin: String,
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

fn build(settings: &Settings) -> Option<Arc<DidomiIntegration>> {
    let config = match settings.integration_config::<DidomiIntegrationConfig>(DIDOMI_INTEGRATION_ID)
    {
        Ok(Some(config)) => Arc::new(config),
        Ok(None) => return None,
        Err(err) => {
            log::error!("Failed to load Didomi integration config: {err:?}");
            return None;
        }
    };
    Some(DidomiIntegration::new(config))
}

/// Register the Didomi consent notice integration when enabled.
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;
    Some(
        IntegrationRegistration::builder(DIDOMI_INTEGRATION_ID)
            .with_proxy(integration)
            .build(),
    )
}

#[async_trait(?Send)]
impl IntegrationProxy for DidomiIntegration {
    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            IntegrationEndpoint::get_prefix(DIDOMI_PREFIX),
            IntegrationEndpoint::post_prefix(DIDOMI_PREFIX),
        ]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path();
        let consent_path = path.strip_prefix(DIDOMI_PREFIX).unwrap_or(path);
        let backend = self.backend_for_path(consent_path);
        let base_origin = match backend {
            DidomiBackend::Sdk => self.config.sdk_origin.as_str(),
            DidomiBackend::Api => self.config.api_origin.as_str(),
        };

        let target_url = self
            .build_target_url(base_origin, consent_path, req.get_query_str())
            .change_context(Self::error("Failed to build Didomi target URL"))?;
        let backend_name = ensure_backend_from_url(base_origin)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationRegistry;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::Method;

    fn config(enabled: bool) -> DidomiIntegrationConfig {
        DidomiIntegrationConfig {
            enabled,
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
            .unwrap();
        assert_eq!(url, "https://sdk.privacy-center.org/loader.js?v=1");
    }

    #[test]
    fn registers_prefix_routes() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(DIDOMI_INTEGRATION_ID, &config(true))
            .expect("should insert config");

        let registry = IntegrationRegistry::new(&settings);
        assert!(registry.has_route(&Method::GET, "/didomi/consent/loader.js"));
        assert!(registry.has_route(&Method::POST, "/didomi/consent/api/events"));
        assert!(!registry.has_route(&Method::GET, "/other"));
    }
}
