use std::sync::Arc;

use async_trait::async_trait;
use error_stack::Report;
use fastly::http::StatusCode;
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration, IntegrationScriptContext,
    IntegrationScriptRewriter, ScriptRewriteAction,
};
use crate::proxy::{proxy_request, ProxyRequestConfig};
use crate::settings::{IntegrationConfig, Settings};

const GTM_INTEGRATION_ID: &str = "google_tag_manager";
const DEFAULT_UPSTREAM: &str = "https://www.googletagmanager.com";

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct GoogleTagManagerConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// GTM Container ID (e.g., "GTM-XXXXXX").
    #[validate(length(min = 1))]
    pub container_id: String,
    /// Upstream URL for GTM (defaults to https://www.googletagmanager.com).
    #[serde(default = "default_upstream")]
    pub upstream_url: String,
}

impl IntegrationConfig for GoogleTagManagerConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    true
}

fn default_upstream() -> String {
    DEFAULT_UPSTREAM.to_string()
}

pub struct GoogleTagManagerIntegration {
    config: GoogleTagManagerConfig,
}

impl GoogleTagManagerIntegration {
    fn new(config: GoogleTagManagerConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn upstream_url(&self) -> &str {
        if self.config.upstream_url.is_empty() {
            DEFAULT_UPSTREAM
        } else {
            &self.config.upstream_url
        }
    }

    fn rewrite_gtm_script(&self, content: &str) -> String {
        // Rewrite 'www.google-analytics.com' to point to this server's proxy path
        // path would be /integrations/google_tag_manager
        let my_integration_path = format!("/integrations/{}", GTM_INTEGRATION_ID);

        // Simplistic replacements - mimic what Cloudflare/others do
        // Replacements depend on exactly how the string appears in the minified JS.
        // Common target: "https://www.google-analytics.com"
        let mut new_content =
            content.replace("https://www.google-analytics.com", &my_integration_path);
        new_content = new_content.replace("https://www.googletagmanager.com", &my_integration_path);
        new_content
    }
}

pub fn build(settings: &Settings) -> Option<Arc<GoogleTagManagerIntegration>> {
    let config = settings
        .integration_config::<GoogleTagManagerConfig>(GTM_INTEGRATION_ID)
        .ok()
        .flatten()?;

    if !config.enabled {
        return None;
    }

    Some(GoogleTagManagerIntegration::new(config))
}

#[must_use]
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;
    Some(
        IntegrationRegistration::builder(GTM_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration.clone())
            .with_script_rewriter(integration)
            .build(),
    )
}

#[async_trait(?Send)]
impl IntegrationProxy for GoogleTagManagerIntegration {
    fn integration_name(&self) -> &'static str {
        GTM_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            // Proxy for the main GTM script
            self.get("/gtm.js"),
            // Proxy for the gtag script (if used)
            self.get("/gtag/js"),
            // Analytics beacons (GA4/UA)
            // Note: In a real "Tag Gateway" implementation, we'd likely need
            // to rewrite the GTM script to point these beacons to our proxy.
            self.get("/collect"),
            self.post("/collect"),
            self.get("/g/collect"),
            self.post("/g/collect"),
        ]
    }

    async fn handle(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path().to_string();
        let upstream_base = self.upstream_url();

        // Construct full target URL
        let mut target_url = if path.ends_with("/gtm.js") {
            format!("{}/gtm.js", upstream_base)
        } else if path.ends_with("/gtag/js") {
            format!("{}/gtag/js", upstream_base)
        } else if path.ends_with("/collect") {
            if path.contains("/g/") {
                "https://www.google-analytics.com/g/collect".to_string()
            } else {
                "https://www.google-analytics.com/collect".to_string()
            }
        } else {
            return Ok(Response::from_status(StatusCode::NOT_FOUND));
        };

        // Append query params if present, or add default ID for gtm.js
        if let Some(query) = req.get_url().query() {
            target_url = format!("{}?{}", target_url, query);
        } else if path.ends_with("/gtm.js") {
            target_url = format!("{}?id={}", target_url, self.config.container_id);
        }

        let mut proxy_config = ProxyRequestConfig::new(&target_url);

        // If we are fetching gtm.js, we intend to rewrite the body.
        // We must ensure the upstream returns uncompressed content.
        if path.ends_with("/gtm.js") {
            proxy_config = proxy_config.with_header(
                fastly::http::header::ACCEPT_ENCODING,
                fastly::http::HeaderValue::from_static("identity"),
            );
        }

        let mut response = proxy_request(settings, req, proxy_config).await?;

        // Rewrite logic (Primitive version)
        // If we are serving gtm.js, we want to text-replace "www.google-analytics.com"
        // with our proxy details to route beacons through us.
        if path.ends_with("/gtm.js") {
            // Note: This is an expensive operation if the script is large.
            // Ideally should be streamed, but simple string replacement for now.
            let body_bytes = response.into_body_bytes();
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            let rewritten_body = self.rewrite_gtm_script(&body_str);

            response = Response::from_body(rewritten_body)
                .with_header(fastly::http::header::CONTENT_TYPE, "application/javascript");
        }

        Ok(response)
    }
}

impl IntegrationAttributeRewriter for GoogleTagManagerIntegration {
    fn integration_id(&self) -> &'static str {
        GTM_INTEGRATION_ID
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        matches!(attribute, "src" | "href")
    }

    fn rewrite(
        &self,
        _attr_name: &str,
        attr_value: &str,
        _ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if attr_value.contains("googletagmanager.com/gtm.js") {
            let encoded_integration_id = urlencoding::encode(self.integration_name());
            let mut new_value = attr_value.replace(
                "https://www.googletagmanager.com/gtm.js",
                &format!("/integrations/{}/gtm.js", encoded_integration_id),
            );
            new_value = new_value.replace(
                "//www.googletagmanager.com/gtm.js",
                &format!("/integrations/{}/gtm.js", encoded_integration_id),
            );

            AttributeRewriteAction::replace(new_value)
        } else {
            AttributeRewriteAction::keep()
        }
    }
}

impl IntegrationScriptRewriter for GoogleTagManagerIntegration {
    fn integration_id(&self) -> &'static str {
        GTM_INTEGRATION_ID
    }

    fn selector(&self) -> &'static str {
        "script" // Match all scripts to find inline GTM snippets
    }

    fn rewrite(&self, content: &str, _ctx: &IntegrationScriptContext<'_>) -> ScriptRewriteAction {
        // Look for the GTM snippet pattern.
        // Standard snippet contains: "googletagmanager.com/gtm.js"
        if content.contains("googletagmanager.com/gtm.js") {
            let encoded_integration_id = urlencoding::encode(self.integration_name());
            let my_integration_path = format!("/integrations/{}/gtm.js", encoded_integration_id);

            let mut new_content = content.replace(
                "https://www.googletagmanager.com/gtm.js",
                &my_integration_path,
            );
            new_content =
                new_content.replace("//www.googletagmanager.com/gtm.js", &my_integration_path);

            return ScriptRewriteAction::replace(new_content);
        }

        ScriptRewriteAction::keep()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{
        AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
        IntegrationDocumentState, IntegrationScriptContext, IntegrationScriptRewriter,
        ScriptRewriteAction,
    };

    #[test]
    fn test_attribute_rewriter() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST".to_string(),
            upstream_url: "https://www.googletagmanager.com".to_string(),
        };
        let integration = GoogleTagManagerIntegration::new(config);

        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        // Case 1: Standard HTTPS URL
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "src",
            "https://www.googletagmanager.com/gtm.js?id=GTM-TEST",
            &ctx,
        );
        if let AttributeRewriteAction::Replace(val) = action {
            assert_eq!(val, "/integrations/google_tag_manager/gtm.js?id=GTM-TEST");
        } else {
            panic!("Expected Replace action for HTTPS URL, got {:?}", action);
        }

        // Case 2: Protocol-relative URL
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "src",
            "//www.googletagmanager.com/gtm.js?id=GTM-TEST",
            &ctx,
        );
        if let AttributeRewriteAction::Replace(val) = action {
            assert_eq!(val, "/integrations/google_tag_manager/gtm.js?id=GTM-TEST");
        } else {
            panic!(
                "Expected Replace action for protocol-relative URL, got {:?}",
                action
            );
        }

        // Case 3: Other URL (should be kept)
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "src",
            "https://other.com/script.js",
            &ctx,
        );
        assert!(matches!(action, AttributeRewriteAction::Keep));
    }

    #[test]
    fn test_script_rewriter() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST".to_string(),
            upstream_url: "https://www.googletagmanager.com".to_string(),
        };
        let integration = GoogleTagManagerIntegration::new(config);
        let doc_state = IntegrationDocumentState::default();

        let ctx = IntegrationScriptContext {
            selector: "script",
            request_host: "example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node: true,
            document_state: &doc_state,
        };

        // Case 1: Inline GTM snippet
        let snippet = r#"(function(w,d,s,l,i){w[l]=w[l]||[];w[l].push({'gtm.start':
new Date().getTime(),event:'gtm.js'});var f=d.getElementsByTagName(s)[0],
j=d.createElement(s),dl=l!='dataLayer'?'&l='+l:'';j.async=true;j.src=
'https://www.googletagmanager.com/gtm.js?id='+i+dl;f.parentNode.insertBefore(j,f);
})(window,document,'script','dataLayer','GTM-XXXX');"#;

        let action = IntegrationScriptRewriter::rewrite(&*integration, snippet, &ctx);
        if let ScriptRewriteAction::Replace(val) = action {
            assert!(val.contains("/integrations/google_tag_manager/gtm.js"));
            assert!(!val.contains("https://www.googletagmanager.com/gtm.js"));
        } else {
            panic!("Expected Replace action for GTM snippet, got {:?}", action);
        }

        // Case 2: Protocol relative
        let snippet_proto = r#"j.src='//www.googletagmanager.com/gtm.js?id='+i+dl;"#;
        let action = IntegrationScriptRewriter::rewrite(&*integration, snippet_proto, &ctx);
        if let ScriptRewriteAction::Replace(val) = action {
            assert!(val.contains("/integrations/google_tag_manager/gtm.js"));
            assert!(!val.contains("//www.googletagmanager.com/gtm.js"));
        } else {
            panic!(
                "Expected Replace action for proto-relative snippet, got {:?}",
                action
            );
        }

        // Case 3: Irrelevant script
        let other_script = "console.log('hello');";
        let action = IntegrationScriptRewriter::rewrite(&*integration, other_script, &ctx);
        assert!(matches!(action, ScriptRewriteAction::Keep));
    }

    #[test]
    fn test_default_configuration() {
        let config = GoogleTagManagerConfig {
            enabled: default_enabled(),
            container_id: "GTM-DEFAULT".to_string(),
            upstream_url: default_upstream(),
        };

        assert!(config.enabled);
        assert_eq!(config.upstream_url, "https://www.googletagmanager.com");
    }

    #[test]
    fn test_upstream_url_logic() {
        // Default upstream
        let config_default = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-123".to_string(),
            upstream_url: "".to_string(), // Empty string should fallback to default in accessor
        };
        let integration_default = GoogleTagManagerIntegration::new(config_default);
        assert_eq!(
            integration_default.upstream_url(),
            "https://www.googletagmanager.com"
        );

        // Custom upstream
        let config_custom = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-123".to_string(),
            upstream_url: "https://gtm.example.com".to_string(),
        };
        let integration_custom = GoogleTagManagerIntegration::new(config_custom);
        assert_eq!(integration_custom.upstream_url(), "https://gtm.example.com");
    }

    #[test]
    fn test_routes_registered() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST".to_string(),
            upstream_url: default_upstream(),
        };
        let integration = GoogleTagManagerIntegration::new(config);
        let routes = integration.routes();

        // GTM.js, Gtag.js, and 4 Collect endpoints (GET/POST for standard & dual-tagging)
        assert_eq!(routes.len(), 6);

        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/google_tag_manager/gtm.js"));
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/google_tag_manager/gtag/js"));
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/google_tag_manager/collect"));
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/google_tag_manager/g/collect"));
    }

    #[test]
    fn test_handle_response_rewriting() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST".to_string(),
            upstream_url: default_upstream(),
        };
        let integration = GoogleTagManagerIntegration::new(config);

        let original_body = r#"
            var x = "https://www.google-analytics.com/collect";
            var y = "https://www.googletagmanager.com/gtm.js";
        "#;

        let rewritten = integration.rewrite_gtm_script(original_body);

        assert!(rewritten.contains("/integrations/google_tag_manager/collect"));
        assert!(rewritten.contains("/integrations/google_tag_manager/gtm.js"));
        assert!(!rewritten.contains("https://www.google-analytics.com"));
    }
}
