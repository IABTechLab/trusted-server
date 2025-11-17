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
    IntegrationProxy,
};
use crate::settings::Settings;
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};
use crate::tsjs;

const STARLIGHT_INTEGRATION_ID: &str = "starlight";

#[derive(Debug, Deserialize)]
pub struct StarlightConfig {
    pub endpoint: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    #[serde(default = "default_shim_src")]
    pub shim_src: String,
    #[serde(default)]
    pub rewrite_scripts: bool,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
struct StarlightRequestBody {
    #[validate(nested)]
    #[serde(default)]
    user: StarlightUserSection,
    #[validate(nested)]
    #[serde(default)]
    imp: Vec<StarlightImp>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
struct StarlightUserSection {
    #[serde(default)]
    #[validate(length(min = 1))]
    id: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
struct StarlightImp {
    #[serde(default)]
    #[validate(length(min = 1))]
    id: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct StarlightResponseBody {
    #[serde(flatten)]
    fields: Map<String, Value>,
}

pub struct StarlightIntegration {
    config: StarlightConfig,
}

impl StarlightIntegration {
    fn new(config: StarlightConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: STARLIGHT_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }
}

pub fn build(settings: &Settings) -> Option<Arc<StarlightIntegration>> {
    let raw = settings.integration_config(STARLIGHT_INTEGRATION_ID)?;
    let config: StarlightConfig = serde_json::from_value(raw.clone()).ok()?;
    Some(StarlightIntegration::new(config))
}

#[async_trait(?Send)]
impl IntegrationProxy for StarlightIntegration {
    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![IntegrationEndpoint::post("/integrations/starlight/auction")]
    }

    async fn handle(
        &self,
        settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let mut payload = serde_json::from_slice::<StarlightRequestBody>(&req.take_body_bytes())
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
        match serde_json::from_slice::<StarlightResponseBody>(&response_body) {
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

impl IntegrationAttributeRewriter for StarlightIntegration {
    fn integration_id(&self) -> &'static str {
        STARLIGHT_INTEGRATION_ID
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
        if lowered.contains("starlight.js") {
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
    tsjs::script_src("tsjs-starlight.js")
        .unwrap_or_else(|| "/static/tsjs=tsjs-starlight.min.js".to_string())
}

impl Default for StarlightRequestBody {
    fn default() -> Self {
        Self {
            user: StarlightUserSection::default(),
            imp: Vec::new(),
            extra: Map::new(),
        }
    }
}

impl Default for StarlightResponseBody {
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
        let shim_src = tsjs::script_src("tsjs-starlight.js")
            .expect("tsjs starlight bundle should exist for tests");
        let config = StarlightConfig {
            endpoint: "https://example.com/openrtb".to_string(),
            timeout_ms: 1000,
            shim_src: shim_src.clone(),
            rewrite_scripts: true,
        };
        let integration = StarlightIntegration::new(config);

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        let rewritten =
            integration.rewrite("src", "https://cdn.starlight.net/v1/starlight.js", &ctx);
        assert_eq!(
            rewritten.as_deref(),
            Some(shim_src.as_str()),
            "Should swap integration script for trusted shim"
        );
    }

    #[test]
    fn html_rewriter_is_noop_when_disabled() {
        let shim_src = tsjs::script_src("tsjs-starlight.js")
            .expect("tsjs starlight bundle should exist for tests");
        let config = StarlightConfig {
            endpoint: "https://example.com/openrtb".to_string(),
            timeout_ms: 1000,
            shim_src,
            rewrite_scripts: false,
        };
        let integration = StarlightIntegration::new(config);
        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        assert!(integration
            .rewrite("src", "https://cdn.starlight.net/script.js", &ctx)
            .is_none());
    }

    #[test]
    fn build_uses_settings_integration_block() {
        let mut settings = create_test_settings();
        settings.integrations.insert(
            STARLIGHT_INTEGRATION_ID.to_string(),
            json!({
                "endpoint": "https://example.com/bid",
                "rewrite_scripts": true,
            }),
        );

        let integration = build(&settings).expect("Integration should build with config");
        let routes = integration.routes();
        assert!(
            routes.iter().any(|route| route.method == Method::POST
                && route.path == "/integrations/starlight/auction"),
            "Integration should register POST /integrations/starlight/auction"
        );
    }
}
