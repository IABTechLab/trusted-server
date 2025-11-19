use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use validator::Validate;

use crate::backend::ensure_backend_from_url;
use crate::constants::{HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER};
use crate::error::TrustedServerError;
use crate::integrations::{
    IntegrationAttributeContext, IntegrationAttributeRewriter, IntegrationEndpoint,
    IntegrationProxy, IntegrationRegistration,
};
use crate::settings::{IntegrationConfig as IntegrationConfigTrait, Settings};
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};
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

impl IntegrationConfigTrait for TestlightConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[derive(Debug, Deserialize, Serialize, Validate)]
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

#[derive(Debug, Deserialize, Serialize)]
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
}

fn build(settings: &Settings) -> Option<Arc<TestlightIntegration>> {
    let config = match settings.integration_config::<TestlightConfig>(TESTLIGHT_INTEGRATION_ID) {
        Ok(Some(config)) => config,
        Ok(None) => return None,
        Err(err) => {
            log::error!("Failed to load Testlight integration config: {err:?}");
            return None;
        }
    };

    Some(TestlightIntegration::new(config))
}

pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;
    Some(
        IntegrationRegistration::builder(TESTLIGHT_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
            .build(),
    )
}

#[async_trait(?Send)]
impl IntegrationProxy for TestlightIntegration {
    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![IntegrationEndpoint::post("/integrations/testlight/auction")]
    }

    async fn handle(
        &self,
        settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let mut payload = serde_json::from_slice::<TestlightRequestBody>(&req.take_body_bytes())
            .change_context(Self::error("Failed to parse request body"))?;
        payload
            .validate()
            .map_err(|err| Report::new(Self::error(format!("Invalid request payload: {err}"))))?;

        let synthetic_id = get_or_generate_synthetic_id(settings, &req)
            .change_context(Self::error("Failed to fetch or mint synthetic ID"))?;
        let fresh_id = generate_synthetic_id(settings, &req)
            .change_context(Self::error("Failed to mint fresh synthetic ID"))?;

        payload.user.id = Some(synthetic_id.clone());

        let mut upstream = Request::new(Method::POST, self.config.endpoint.clone());
        upstream.set_header(header::CONTENT_TYPE, "application/json");
        upstream
            .set_body_json(&payload)
            .change_context(Self::error("Failed to serialize request body"))?;

        if let Some(user_agent) = req.get_header(header::USER_AGENT) {
            upstream.set_header(header::USER_AGENT, user_agent);
        }

        let backend = ensure_backend_from_url(&self.config.endpoint)
            .change_context(Self::error("Failed to determine backend"))?;
        let mut response = upstream
            .send(backend)
            .change_context(Self::error("Failed to contact upstream integration"))?;

        // Attempt to parse response into structured form for logging/future transforms.
        let response_body = response.take_body_bytes();
        match serde_json::from_slice::<TestlightResponseBody>(&response_body) {
            Ok(body) => {
                response
                    .set_body_json(&body)
                    .change_context(Self::error("Failed to serialize integration response body"))?;
            }
            Err(_) => {
                // Preserve original body if the integration responded with non-JSON content.
                response.set_body(response_body);
            }
        }

        response.set_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id);
        response.set_header(HEADER_SYNTHETIC_FRESH, &fresh_id);
        Ok(response)
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
    ) -> Option<String> {
        if !self.config.rewrite_scripts {
            return None;
        }

        let lowered = attr_value.to_ascii_lowercase();
        if lowered.contains("testlight.js") {
            Some(self.config.shim_src.clone())
        } else {
            None
        }
    }
}

fn default_timeout_ms() -> u32 {
    1000
}

fn default_shim_src() -> String {
    // Testlight is included in the unified bundle, so we return the unified script source
    tsjs::unified_script_src()
}

fn default_enabled() -> bool {
    true
}

impl Default for TestlightRequestBody {
    fn default() -> Self {
        Self {
            user: TestlightUserSection::default(),
            imp: Vec::new(),
            extra: Map::new(),
        }
    }
}

impl Default for TestlightResponseBody {
    fn default() -> Self {
        Self { fields: Map::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_support::tests::create_test_settings, tsjs};
    use fastly::http::Method;
    use serde_json::json;

    #[test]
    fn build_requires_config() {
        let settings = create_test_settings();
        assert!(
            build(&settings).is_none(),
            "Should not build without integration config"
        );
    }

    #[test]
    fn html_rewriter_replaces_integration_script() {
        let shim_src = tsjs::unified_script_src();
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
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        let rewritten =
            integration.rewrite("src", "https://cdn.testlight.net/v1/testlight.js", &ctx);
        assert_eq!(
            rewritten.as_deref(),
            Some(shim_src.as_str()),
            "Should swap integration script for trusted shim"
        );
    }

    #[test]
    fn html_rewriter_is_noop_when_disabled() {
        let shim_src = tsjs::unified_script_src();
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
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        assert!(integration
            .rewrite("src", "https://cdn.testlight.net/script.js", &ctx)
            .is_none());
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

        let integration = build(&settings).expect("Integration should build with config");
        let routes = integration.routes();
        assert!(
            routes.iter().any(|route| route.method == Method::POST
                && route.path == "/integrations/testlight/auction"),
            "Integration should register POST /integrations/testlight/auction"
        );
    }
}
