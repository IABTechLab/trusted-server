use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::{self, HeaderMap, HeaderValue};
use http::Method;
use serde::{Deserialize, Serialize};
use url::Url;
use validator::Validate;

use crate::error::TrustedServerError;
use crate::integrations::{
    collect_body_bounded, ensure_integration_backend, IntegrationEndpoint, IntegrationProxy,
    IntegrationRegistration, INTEGRATION_MAX_BODY_BYTES,
};
use crate::platform::{PlatformHttpRequest, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

const DIDOMI_INTEGRATION_ID: &str = "didomi";
const DIDOMI_PREFIX: &str = "/integrations/didomi/consent";

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

        for header_name in [
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
            header::CONTENT_TYPE,
            header::USER_AGENT,
            header::REFERER,
            header::ORIGIN,
            header::AUTHORIZATION,
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
            .with_proxy(integration)
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for DidomiIntegration {
    fn integration_name(&self) -> &'static str {
        DIDOMI_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get("/consent/*"), self.post("/consent/*")]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let (parts, body) = req.into_parts();
        let path = parts.uri.path().to_string();
        let consent_path = path.strip_prefix(DIDOMI_PREFIX).unwrap_or(&path);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::integrations::IntegrationRegistry;
    use crate::platform::test_support::{build_services_with_http_client, StubHttpClient};
    use crate::test_support::tests::create_test_settings;
    use http::Method;
    use std::net::{IpAddr, Ipv4Addr};

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
}
