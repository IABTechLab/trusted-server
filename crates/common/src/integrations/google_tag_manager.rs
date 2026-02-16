//! Google Tag Manager integration for first-party tag delivery.
//!
//! Proxies GTM scripts and Google Analytics beacons through the publisher's
//! domain, improving tracking accuracy and ad-blocker resistance.
//!
//! # Endpoints
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | `GET` | `.../gtm.js` | Proxies and rewrites the GTM script |
//! | `GET` | `.../gtag/js` | Proxies the gtag script |
//! | `GET/POST` | `.../collect` | Proxies GA analytics beacons |
//! | `GET/POST` | `.../g/collect` | Proxies GA4 analytics beacons |

use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{Method, StatusCode};
use fastly::{Request, Response};
use once_cell::sync::Lazy;
use regex::Regex;
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

/// Regex pattern for matching and rewriting GTM and Google Analytics URLs.
///
/// Handles all URL variants:
/// - `https://www.googletagmanager.com/gtm.js?id=...`
/// - `//www.googletagmanager.com/gtm.js?id=...`
/// - `https://www.google-analytics.com/collect`
/// - `//www.google-analytics.com/g/collect`
///
/// The replacement target is `/integrations/google_tag_manager`.
static GTM_URL_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(https?:)?//www\.(googletagmanager|google-analytics)\.com")
        .expect("GTM URL regex should compile")
});

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct GoogleTagManagerConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// GTM Container ID (e.g., "GTM-XXXXXX").
    #[validate(length(min = 1))]
    pub container_id: String,
    /// Upstream URL for GTM (defaults to <https://www.googletagmanager.com>).
    #[serde(default = "default_upstream")]
    #[validate(url)]
    pub upstream_url: String,
    /// Cache max-age in seconds for the rewritten GTM script (default: 900 to match Google's default).
    #[serde(default = "default_cache_max_age")]
    #[validate(range(min = 60, max = 86400))]
    pub cache_max_age: u32,
}

impl IntegrationConfig for GoogleTagManagerConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    false
}

fn default_upstream() -> String {
    DEFAULT_UPSTREAM.to_string()
}

fn default_cache_max_age() -> u32 {
    900 // Match Google's default
}

pub struct GoogleTagManagerIntegration {
    config: GoogleTagManagerConfig,
}

impl GoogleTagManagerIntegration {
    fn new(config: GoogleTagManagerConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: GTM_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    fn upstream_url(&self) -> &str {
        if self.config.upstream_url.is_empty() {
            DEFAULT_UPSTREAM
        } else {
            &self.config.upstream_url
        }
    }

    /// Rewrite GTM and Google Analytics URLs to first-party proxy paths.
    ///
    /// Uses [`GTM_URL_PATTERN`] to handle all URL variants (https, protocol-relative)
    /// for both `googletagmanager.com` and `google-analytics.com`.
    fn rewrite_gtm_urls(content: &str) -> String {
        let replacement = format!("/integrations/{}", GTM_INTEGRATION_ID);
        GTM_URL_PATTERN
            .replace_all(content, replacement.as_str())
            .into_owned()
    }

    fn is_rewritable_script(&self, path: &str) -> bool {
        path.ends_with("/gtm.js") || path.ends_with("/gtag/js") || path.ends_with("/gtag.js")
    }

    fn build_target_url(&self, req: &Request, path: &str) -> Option<String> {
        let upstream_base = self.upstream_url();

        let mut target_url = if path.ends_with("/gtm.js") {
            format!("{}/gtm.js", upstream_base)
        } else if path.ends_with("/gtag/js") || path.ends_with("/gtag.js") {
            format!("{}/gtag/js", upstream_base) // Always normalize to /gtag/js upstream as it's canonical
        } else if path.ends_with("/collect") {
            if path.contains("/g/") {
                "https://www.google-analytics.com/g/collect".to_string()
            } else {
                "https://www.google-analytics.com/collect".to_string()
            }
        } else {
            return None;
        };

        if let Some(query) = req.get_url().query() {
            target_url = format!("{}?{}", target_url, query);
        } else if path.ends_with("/gtm.js") {
            target_url = format!("{}?id={}", target_url, self.config.container_id);
        }

        Some(target_url)
    }

    fn build_proxy_config<'a>(
        &self,
        path: &str,
        req: &mut Request,
        target_url: &'a str,
    ) -> ProxyRequestConfig<'a> {
        let mut proxy_config = ProxyRequestConfig::new(target_url);
        proxy_config.forward_synthetic_id = false;

        // If it's a POST request (e.g. /collect beacon), we must manually attach the body
        // because ProxyRequestConfig doesn't automatically copy it from the source request.
        if req.get_method() == Method::POST {
            let body_bytes = req.take_body_bytes();
            proxy_config.body = Some(body_bytes);
        }

        // Explicitly strip X-Forwarded-For to prevent client IP leakage to Google.
        // The empty value will override any existing header during proxy forwarding.
        proxy_config = proxy_config.with_header(
            crate::constants::HEADER_X_FORWARDED_FOR,
            fastly::http::HeaderValue::from_static(""),
        );

        if self.is_rewritable_script(path) {
            proxy_config = proxy_config.with_header(
                fastly::http::header::ACCEPT_ENCODING,
                fastly::http::HeaderValue::from_static("identity"),
            );
        }

        proxy_config
    }
}

fn build(settings: &Settings) -> Option<Arc<GoogleTagManagerIntegration>> {
    let config = match settings.integration_config::<GoogleTagManagerConfig>(GTM_INTEGRATION_ID) {
        Ok(Some(config)) => config,
        Ok(None) => return None,
        Err(err) => {
            log::error!("Failed to load GTM integration config: {err:?}");
            return None;
        }
    };

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
            self.get("/gtag.js"),
            // Analytics beacons (GA4/UA)
            // The GTM script is rewritten to point these beacons to our proxy.
            self.get("/collect"),
            self.post("/collect"),
            self.get("/g/collect"),
            self.post("/g/collect"),
        ]
    }

    async fn handle(
        &self,
        settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path().to_string();
        let method = req.get_method();
        log::debug!("Handling GTM request: {} {}", method, path);

        let Some(target_url) = self.build_target_url(&req, &path) else {
            return Ok(Response::from_status(StatusCode::NOT_FOUND));
        };

        log::debug!("Proxying to upstream: {}", target_url);

        let proxy_config = self.build_proxy_config(&path, &mut req, &target_url);

        let mut response = proxy_request(settings, req, proxy_config)
            .await
            .change_context(Self::error("Failed to proxy GTM request"))?;

        // If we are serving gtm.js or gtag.js, rewrite internal URLs to route beacons through us.
        if self.is_rewritable_script(&path) {
            if !response.get_status().is_success() {
                log::warn!("GTM upstream returned status {}", response.get_status());
                return Ok(response);
            }
            log::debug!("Rewriting GTM/gtag script content");
            let body_str = response.take_body_str();
            let rewritten_body = Self::rewrite_gtm_urls(&body_str);

            response = Response::from_body(rewritten_body)
                .with_header(
                    fastly::http::header::CONTENT_TYPE,
                    "application/javascript; charset=utf-8",
                )
                .with_header(
                    fastly::http::header::CACHE_CONTROL,
                    format!("public, max-age={}", self.config.cache_max_age),
                );
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
            AttributeRewriteAction::replace(Self::rewrite_gtm_urls(attr_value))
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
            return ScriptRewriteAction::replace(Self::rewrite_gtm_urls(content));
        }

        ScriptRewriteAction::keep()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};
    use crate::integrations::{
        AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
        IntegrationDocumentState, IntegrationRegistry, IntegrationScriptContext,
        IntegrationScriptRewriter, ScriptRewriteAction,
    };
    use crate::settings::Settings;
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};

    use crate::test_support::tests::crate_test_settings_str;
    use fastly::http::Method;
    use std::io::Cursor;

    #[test]
    fn test_rewrite_gtm_urls() {
        // All URL patterns should be rewritten via the shared regex
        let input = r#"
            var a = "https://www.googletagmanager.com/gtm.js";
            var b = "//www.googletagmanager.com/gtm.js";
            var c = "https://www.google-analytics.com/collect";
            var d = "//www.google-analytics.com/g/collect";
            var e = "http://www.googletagmanager.com/gtm.js";
        "#;

        let result = GoogleTagManagerIntegration::rewrite_gtm_urls(input);

        assert!(result.contains("/integrations/google_tag_manager/gtm.js"));
        assert!(result.contains("/integrations/google_tag_manager/collect"));
        assert!(result.contains("/integrations/google_tag_manager/g/collect"));
        assert!(!result.contains("www.googletagmanager.com"));
        assert!(!result.contains("www.google-analytics.com"));
    }

    #[test]
    fn test_rewrite_preserves_non_gtm_urls() {
        let input = r#"var x = "https://example.com/script.js";"#;
        let result = GoogleTagManagerIntegration::rewrite_gtm_urls(input);
        assert_eq!(input, result);
    }

    #[test]
    fn test_attribute_rewriter() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST".to_string(),
            upstream_url: "https://www.googletagmanager.com".to_string(),
            cache_max_age: default_cache_max_age(),
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
            cache_max_age: default_cache_max_age(),
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
            cache_max_age: default_cache_max_age(),
        };

        assert!(!config.enabled);
        assert_eq!(config.upstream_url, "https://www.googletagmanager.com");
    }

    #[test]
    fn test_upstream_url_logic() {
        // Default upstream
        let config_default = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-123".to_string(),
            upstream_url: "".to_string(), // Empty string should fallback to default in accessor
            cache_max_age: default_cache_max_age(),
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
            cache_max_age: default_cache_max_age(),
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
            cache_max_age: default_cache_max_age(),
        };
        let integration = GoogleTagManagerIntegration::new(config);
        let routes = integration.routes();

        // GTM.js, Gtag.js (/js and .js), and 4 Collect endpoints (GET/POST for standard & dual-tagging)
        assert_eq!(routes.len(), 7);

        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/google_tag_manager/gtm.js"));
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/google_tag_manager/gtag/js"));
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/google_tag_manager/gtag.js"));
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/google_tag_manager/collect"));
        assert!(routes
            .iter()
            .any(|r| r.path == "/integrations/google_tag_manager/g/collect"));
    }

    #[test]
    fn test_post_collect_proxy_config_includes_payload() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
        };
        let integration = GoogleTagManagerIntegration::new(config);

        let payload = b"v=2&tid=G-TEST&cid=123&en=page_view".to_vec();
        let mut req = Request::new(
            Method::POST,
            "https://edge.example.com/integrations/google_tag_manager/g/collect?v=2&tid=G-TEST",
        );
        req.set_body(payload.clone());

        let path = req.get_path().to_string();
        let target_url = integration
            .build_target_url(&req, &path)
            .expect("should resolve collect target URL");
        let proxy_config = integration.build_proxy_config(&path, &mut req, &target_url);

        assert_eq!(
            proxy_config.body.as_deref(),
            Some(payload.as_slice()),
            "collect POST should forward payload body"
        );
    }

    #[test]
    fn test_collect_proxy_config_strips_client_ip_forwarding() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
        };
        let integration = GoogleTagManagerIntegration::new(config);

        let mut req = Request::new(
            Method::GET,
            "https://edge.example.com/integrations/google_tag_manager/collect?v=2",
        );
        req.set_header(crate::constants::HEADER_X_FORWARDED_FOR, "198.51.100.42");

        let path = req.get_path().to_string();
        let target_url = integration
            .build_target_url(&req, &path)
            .expect("should resolve collect target URL");
        let proxy_config = integration.build_proxy_config(&path, &mut req, &target_url);

        // We check if X-Forwarded-For is explicitly overridden with an empty string,
        // which effectively strips it during proxy forwarding due to header override logic.
        let has_header_override = proxy_config.headers.iter().any(|(name, value)| {
            name.as_str()
                .eq_ignore_ascii_case(crate::constants::HEADER_X_FORWARDED_FOR.as_str())
                && value.is_empty()
        });

        assert!(
            has_header_override,
            "collect routes should strip client IP by overriding X-Forwarded-For with empty string"
        );
    }

    #[test]
    fn test_gtag_proxy_config_requests_identity_encoding() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GT-123".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
        };
        let integration = GoogleTagManagerIntegration::new(config);

        let mut req = Request::new(
            Method::GET,
            "https://edge.example.com/integrations/google_tag_manager/gtag/js?id=G-123",
        );

        let path = req.get_path().to_string();
        let target_url = integration
            .build_target_url(&req, &path)
            .expect("should resolve gtag target URL");
        let proxy_config = integration.build_proxy_config(&path, &mut req, &target_url);

        let has_identity = proxy_config.headers.iter().any(|(name, value)| {
            name == fastly::http::header::ACCEPT_ENCODING && value == "identity"
        });

        assert!(
            has_identity,
            "gtag/js requests should force Accept-Encoding: identity for rewriting"
        );
    }

    #[test]
    fn test_handle_response_rewriting() {
        let original_body = r#"
            var x = "https://www.google-analytics.com/collect";
            var y = "https://www.googletagmanager.com/gtm.js";
        "#;

        let rewritten = GoogleTagManagerIntegration::rewrite_gtm_urls(original_body);

        assert!(rewritten.contains("/integrations/google_tag_manager/collect"));
        assert!(rewritten.contains("/integrations/google_tag_manager/gtm.js"));
        assert!(!rewritten.contains("https://www.google-analytics.com"));
    }

    fn make_settings() -> Settings {
        Settings::from_toml(&crate_test_settings_str()).expect("should parse settings")
    }

    fn config_from_settings(
        settings: &Settings,
        registry: &IntegrationRegistry,
    ) -> HtmlProcessorConfig {
        HtmlProcessorConfig::from_settings(
            settings,
            registry,
            "origin.example.com",
            "test.example.com",
            "https",
        )
    }

    #[test]
    fn test_config_parsing() {
        let toml_str = r#"
[publisher]
domain = "test-publisher.com"
cookie_domain = ".test-publisher.com"
origin_url = "https://origin.test-publisher.com"
proxy_secret = "test-secret"

[synthetic]
counter_store = "test-counter-store"
opid_store = "test-opid-store"
secret_key = "test-secret-key"
template = "{{client_ip}}:{{user_agent}}"

[integrations.google_tag_manager]
enabled = true
container_id = "GTM-PARSED"
upstream_url = "https://custom.gtm.example"
"#;
        let settings = Settings::from_toml(toml_str).expect("should parse TOML");
        let config = settings
            .integration_config::<GoogleTagManagerConfig>(GTM_INTEGRATION_ID)
            .expect("should get config")
            .expect("should be enabled");

        assert!(config.enabled);
        assert_eq!(config.container_id, "GTM-PARSED");
        assert_eq!(config.upstream_url, "https://custom.gtm.example");
    }

    #[test]
    fn test_config_defaults() {
        let toml_str = r#"
[publisher]
domain = "test-publisher.com"
cookie_domain = ".test-publisher.com"
origin_url = "https://origin.test-publisher.com"
proxy_secret = "test-secret"

[synthetic]
counter_store = "test-counter-store"
opid_store = "test-opid-store"
secret_key = "test-secret-key"
template = "{{client_ip}}:{{user_agent}}"

[integrations.google_tag_manager]
container_id = "GTM-DEFAULT"
"#;
        let settings = Settings::from_toml(toml_str).expect("should parse TOML");
        let config = settings
            .integration_config::<GoogleTagManagerConfig>(GTM_INTEGRATION_ID)
            .expect("should get config");

        // Default is now false, so integration_config returns None for disabled
        // When we explicitly parse the config with container_id but no enabled field,
        // the config is present but disabled
        assert!(
            config.is_none(),
            "Config with default enabled=false should return None from integration_config"
        );
    }

    #[test]
    fn test_html_processor_pipeline_rewrites_gtm() {
        let html = r#"<html><head>
            <script src="https://www.googletagmanager.com/gtm.js?id=GTM-TEST"></script>
        </head><body></body></html>"#;

        let mut settings = make_settings();
        // Enable GTM
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({
                    "enabled": true,
                    "container_id": "GTM-TEST",
                    "upstream_url": "https://www.googletagmanager.com"
                }),
            )
            .expect("should update gtm config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        let result = pipeline.process(Cursor::new(html.as_bytes()), &mut output);
        assert!(result.is_ok());

        let processed = String::from_utf8_lossy(&output);

        // Verify rewrite happened
        assert!(processed.contains("/integrations/google_tag_manager/gtm.js?id=GTM-TEST"));
        assert!(!processed.contains("https://www.googletagmanager.com/gtm.js"));
    }

    #[test]
    fn test_html_processing_with_fixture() {
        // 1. Configure Settings with GTM enabled
        let mut settings = make_settings();

        // Use the ID from the fixture: GTM-522ZT3X6
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({
                    "enabled": true,
                    "container_id": "GTM-522ZT3X6",
                    "upstream_url": "https://www.googletagmanager.com"
                }),
            )
            .expect("should update gtm config");

        // 2. Setup Pipeline
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        // 3. Load Fixture
        // Path is relative to this file: ../html_processor.test.html
        let html_content = include_str!("../html_processor.test.html");

        // 4. Run Pipeline
        let mut output = Vec::new();
        let result = pipeline.process(Cursor::new(html_content.as_bytes()), &mut output);
        assert!(
            result.is_ok(),
            "Pipeline processing failed: {:?}",
            result.err()
        );

        let processed = String::from_utf8_lossy(&output);

        // 5. Assertions

        // a. Link Preload Rewrite:
        // Original: <link rel="preload" href="https://www.googletagmanager.com/gtm.js?id=GTM-522ZT3X6" ...
        // Expected: href="/integrations/google_tag_manager/gtm.js?id=GTM-522ZT3X6"
        let expected_link = "/integrations/google_tag_manager/gtm.js?id=GTM-522ZT3X6";

        assert!(
            processed.contains(expected_link),
            "Link preload tag not rewritten correctly"
        );

        assert!(
            !processed.contains("href=\"https://www.googletagmanager.com/gtm.js?id=GTM-522ZT3X6\""),
            "Original link preload tag should not exist"
        );

        // b. Noscript Iframe Rewrite
        // Should NOT be rewritten for ns.html
        assert!(
            processed.contains("src=\"https://www.googletagmanager.com/ns.html?id=GTM-522ZT3X6\""),
            "Noscript iframe src should NOT be rewritten (only gtm.js is targeted)"
        );
    }

    #[test]
    fn test_inline_script_rewriting() {
        let mut settings = make_settings();
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({
                    "enabled": true,
                    "container_id": "GTM-12345",
                    "upstream_url": "https://www.googletagmanager.com"
                }),
            )
            .expect("should update config");

        // Inlined Pipeline Creation
        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        // Synthetic HTML with inline script
        let html_input = r#"
            <html>
            <head>
                <script>(function(w,d,s,l,i){w[l]=w[l]||[];w[l].push({'gtm.start':
                new Date().getTime(),event:'gtm.js'});var f=d.getElementsByTagName(s)[0],
                j=d.createElement(s),dl=l!='dataLayer'?'&l='+l:'';j.async=true;j.src=
                'https://www.googletagmanager.com/gtm.js?id='+i+dl;f.parentNode.insertBefore(j,f);
                })(window,document,'script','dataLayer','GTM-12345');</script>
            </head>
            <body></body>
            </html>
        "#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html_input.as_bytes()), &mut output)
            .expect("should process");
        let processed = String::from_utf8_lossy(&output);

        let expected_src = "/integrations/google_tag_manager/gtm.js";

        assert!(
            processed.contains(expected_src),
            "Inline script src not rewritten"
        );

        assert!(
            !processed.contains("j.src='https://www.googletagmanager.com/gtm.js"),
            "Original src should be gone"
        );
    }

    #[test]
    fn test_error_helper() {
        let err = GoogleTagManagerIntegration::error("test failure");
        match err {
            TrustedServerError::Integration {
                integration,
                message,
            } => {
                assert_eq!(integration, "google_tag_manager");
                assert_eq!(message, "test failure");
            }
            other => panic!("Expected Integration error, got {:?}", other),
        }
    }
}
