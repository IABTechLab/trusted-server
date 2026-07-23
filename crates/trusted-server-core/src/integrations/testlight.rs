use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::Response;
use http::header::{self, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use validator::Validate;

use crate::edge_cookie::get_ec_id;
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, INTEGRATION_MAX_BODY_BYTES, IntegrationAttributeContext,
    IntegrationAttributeRewriter, IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
    UPSTREAM_RTB_MAX_RESPONSE_BYTES, collect_body_bounded, collect_response_bounded,
};
use crate::platform::RuntimeServices;
use crate::proxy::{ProxyRequestConfig, proxy_request};
use crate::settings::{IntegrationConfig, Settings};
use crate::tsjs;

const TESTLIGHT_INTEGRATION_ID: &str = "testlight";

#[derive(Debug, Deserialize, Validate)]
pub struct TestlightConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[validate(url)]
    pub endpoint: String,
    #[serde(default = "default_timeout_ms")]
    #[validate(range(min = 10, max = 60000))]
    pub timeout_ms: u32,
    #[serde(default = "default_shim_src")]
    #[validate(length(min = 1))]
    pub shim_src: String,
    #[serde(default)]
    pub rewrite_scripts: bool,
}

impl IntegrationConfig for TestlightConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
struct TestlightRequestBody {
    #[validate(nested)]
    #[serde(default)]
    user: TestlightUserSection,
    #[validate(nested)]
    #[serde(default)]
    imp: Vec<TestlightImp>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
struct TestlightUserSection {
    #[serde(default)]
    #[validate(length(min = 1))]
    id: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
struct TestlightImp {
    #[serde(default)]
    #[validate(length(min = 1))]
    id: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct TestlightResponseBody {
    #[serde(flatten)]
    fields: Map<String, Value>,
}

pub struct TestlightIntegration {
    config: TestlightConfig,
}

impl TestlightIntegration {
    fn new(config: TestlightConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: TESTLIGHT_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    fn rewrite_request_body(
        payload_bytes: &[u8],
        synthetic_id: &str,
    ) -> Result<Vec<u8>, Report<TrustedServerError>> {
        let mut payload = serde_json::from_slice::<TestlightRequestBody>(payload_bytes)
            .change_context(Self::error("Failed to parse request body"))?;
        payload
            .validate()
            .map_err(|err| Report::new(Self::error(format!("Invalid request payload: {err}"))))?;

        payload.user.id = Some(synthetic_id.to_string());

        serde_json::to_vec(&payload).change_context(Self::error("Failed to serialize request body"))
    }

    fn rebuild_response(
        mut parts: http::response::Parts,
        body_bytes: Vec<u8>,
        json_content_type: bool,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        parts.headers.remove(header::CONTENT_LENGTH);

        if json_content_type {
            parts.headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }

        Ok(Response::from_parts(parts, EdgeBody::from(body_bytes)))
    }
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<TestlightIntegration>>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<TestlightConfig>(TESTLIGHT_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    Ok(Some(TestlightIntegration::new(config)))
}

/// Register the Testlight integration when enabled.
///
/// # Errors
///
/// Returns an error when the Testlight integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(TESTLIGHT_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for TestlightIntegration {
    fn integration_name(&self) -> &'static str {
        TESTLIGHT_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.post("/auction")]
    }

    async fn handle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let (parts, body) = req.into_parts();
        let payload_bytes =
            collect_body_bounded(body, INTEGRATION_MAX_BODY_BYTES, TESTLIGHT_INTEGRATION_ID)
                .await?;
        let req = http::Request::from_parts(parts, EdgeBody::empty());

        // Read EC ID from the ts-ec cookie forwarded by the client.
        // The registry strips x-ts-ec before dispatching, so only the cookie is available here.
        let ec_id = get_ec_id(&req)
            .change_context(Self::error("Failed to read EC ID"))?
            .ok_or_else(|| {
                Report::new(Self::error(
                    "EC ID not found in ts-ec cookie — the client must carry a valid EC cookie",
                ))
            })?;

        let payload_bytes = Self::rewrite_request_body(&payload_bytes, &ec_id)?;

        let mut proxy_config = ProxyRequestConfig::new(&self.config.endpoint);
        proxy_config.forward_ec_id = false;
        proxy_config.body = Some(payload_bytes);
        proxy_config.stream_passthrough = true;
        proxy_config.headers.push((
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        ));

        let response = proxy_request(settings, req, proxy_config, services)
            .await
            .change_context(Self::error("Failed to contact upstream integration"))?;
        let (parts, body) = response.into_parts();

        // Attempt to parse response into structured form for logging/future transforms.
        let response_body = collect_response_bounded(
            body,
            UPSTREAM_RTB_MAX_RESPONSE_BYTES,
            TESTLIGHT_INTEGRATION_ID,
        )
        .await?;
        match serde_json::from_slice::<TestlightResponseBody>(&response_body) {
            Ok(body) => {
                let response_body = serde_json::to_vec(&body)
                    .change_context(Self::error("Failed to serialize integration response body"))?;
                Self::rebuild_response(parts, response_body, true)
            }
            Err(_) => {
                // Preserve original body if the integration responded with non-JSON content.
                Self::rebuild_response(parts, response_body, false)
            }
        }
    }
}

impl IntegrationAttributeRewriter for TestlightIntegration {
    fn integration_id(&self) -> &'static str {
        TESTLIGHT_INTEGRATION_ID
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        self.config.rewrite_scripts && matches!(attribute, "src" | "href")
    }

    fn rewrite(
        &self,
        _attr_name: &str,
        attr_value: &str,
        _ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if !self.config.rewrite_scripts {
            return AttributeRewriteAction::keep();
        }

        let lowered = attr_value.to_ascii_lowercase();
        if lowered.contains("testlight.js") {
            AttributeRewriteAction::replace(self.config.shim_src.clone())
        } else {
            AttributeRewriteAction::keep()
        }
    }
}

fn default_timeout_ms() -> u32 {
    1000
}

fn default_shim_src() -> String {
    // Testlight is included in the unified bundle, so we return the unified script source.
    // Uses conservative all-module hash since the registry is unavailable at config time.
    tsjs::tsjs_unified_script_src()
}

fn default_enabled() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::test_support::{StubHttpClient, build_services_with_http_client};
    use crate::test_support::tests::{VALID_SYNTHETIC_ID, create_test_settings};
    use crate::tsjs;
    use http::Method;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn build_requires_config() {
        let settings = create_test_settings();
        assert!(
            build(&settings)
                .expect("should evaluate integration build")
                .is_none(),
            "Should not build without integration config"
        );
    }

    #[test]
    fn html_rewriter_replaces_integration_script() {
        let shim_src = tsjs::tsjs_unified_script_src();
        let config = TestlightConfig {
            enabled: true,
            endpoint: "https://example.com/openrtb".to_string(),
            timeout_ms: 1000,
            shim_src: shim_src.clone(),
            rewrite_scripts: true,
        };
        let integration = TestlightIntegration::new(config);

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            element_name: "script",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        let rewritten =
            integration.rewrite("src", "https://cdn.testlight.net/v1/testlight.js", &ctx);
        assert!(
            matches!(
                rewritten,
                AttributeRewriteAction::Replace(ref value) if value == &shim_src
            ),
            "Should swap integration script for trusted shim"
        );
    }

    #[test]
    fn html_rewriter_is_noop_when_disabled() {
        let shim_src = tsjs::tsjs_unified_script_src();
        let config = TestlightConfig {
            enabled: true,
            endpoint: "https://example.com/openrtb".to_string(),
            timeout_ms: 1000,
            shim_src,
            rewrite_scripts: false,
        };
        let integration = TestlightIntegration::new(config);
        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            element_name: "script",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        assert!(matches!(
            integration.rewrite("src", "https://cdn.testlight.net/script.js", &ctx),
            AttributeRewriteAction::Keep
        ));
    }

    #[test]
    fn build_uses_settings_integration_block() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                TESTLIGHT_INTEGRATION_ID.to_string(),
                &json!({
                    "enabled": true,
                    "endpoint": "https://example.com/bid",
                    "rewrite_scripts": true,
                }),
            )
            .expect("should insert integration config");

        let integration = build(&settings)
            .expect("should evaluate integration build")
            .expect("Integration should build with config");
        let routes = integration.routes();
        assert!(
            routes.iter().any(|route| route.method == Method::POST
                && route.path == "/integrations/testlight/auction"),
            "Integration should register POST /integrations/testlight/auction"
        );
    }

    #[test]
    fn rewrite_request_body_injects_synthetic_id_without_fastly_types() {
        let payload = br#"{"imp":[{"id":"slot-1"}]}"#;

        let rewritten = TestlightIntegration::rewrite_request_body(payload, "abc123.XyZ789")
            .expect("should rewrite Testlight payload");
        let rewritten_json: serde_json::Value =
            serde_json::from_slice(&rewritten).expect("should parse rewritten payload");

        assert_eq!(
            rewritten_json["user"]["id"], "abc123.XyZ789",
            "should inject the synthetic ID into the Testlight user payload"
        );
    }

    #[test]
    fn rebuild_response_drops_stale_content_length_when_body_changes() {
        let response = http::Response::builder()
            .status(http::StatusCode::OK)
            .header(header::CONTENT_LENGTH, "99")
            .body(EdgeBody::from(br#"{ "ok" : true }"#.to_vec()))
            .expect("should build Testlight response");
        let (parts, _) = response.into_parts();

        let rebuilt =
            TestlightIntegration::rebuild_response(parts, br#"{"ok":true}"#.to_vec(), true)
                .expect("should rebuild Testlight response");

        assert!(
            rebuilt.headers().get(header::CONTENT_LENGTH).is_none(),
            "should drop stale Content-Length when rebuilding the response body"
        );
        assert_eq!(
            rebuilt
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json"),
            "should normalize JSON responses to application/json"
        );
    }

    #[test]
    fn handle_uses_platform_http_client_with_http_request() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, br#"{"ok":true}"#.to_vec());
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let settings = create_test_settings();
            let integration = TestlightIntegration::new(TestlightConfig {
                enabled: true,
                endpoint: "https://example.com/openrtb".to_string(),
                timeout_ms: 1000,
                shim_src: tsjs::tsjs_unified_script_src(),
                rewrite_scripts: true,
            });
            let mut req = http::Request::builder()
                .method(Method::POST)
                .uri("https://edge.example.com/integrations/testlight/auction")
                .body(EdgeBody::from(br#"{"imp":[{"id":"slot-1"}]}"#.to_vec()))
                .expect("should build request");
            req.headers_mut().insert(
                crate::constants::HEADER_X_TS_EC.clone(),
                http::HeaderValue::from_static(VALID_SYNTHETIC_ID),
            );

            let response = integration
                .handle(&settings, &services, req)
                .await
                .expect("should proxy Testlight request");

            assert_eq!(
                response.status(),
                http::StatusCode::OK,
                "should return stubbed upstream status"
            );
            assert_eq!(
                stub.recorded_backend_names(),
                vec!["stub-backend".to_string()],
                "should route outbound request through PlatformHttpClient"
            );
            let response_json: serde_json::Value =
                serde_json::from_slice(&response.into_body().into_bytes().unwrap_or_default())
                    .expect("should parse JSON response");
            assert_eq!(
                response_json["ok"], true,
                "should preserve the upstream JSON response body"
            );
        });
    }
}
