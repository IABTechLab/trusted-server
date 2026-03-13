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

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{Method, StatusCode};
use fastly::{Request, Response};
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

/// Error type for payload size validation
#[derive(Debug)]
enum PayloadSizeError {
    TooLarge { actual: usize, max: usize },
}

/// Regex pattern for validating GTM container IDs.
/// Format: GTM-XXXXXX where X is alphanumeric.
static GTM_CONTAINER_ID_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^GTM-[A-Z0-9]{4,20}$").expect("GTM container ID regex should compile")
});

/// Regex pattern for matching and rewriting GTM and Google Analytics URLs.
///
/// Handles full and protocol-relative URL variants:
/// - `https://www.googletagmanager.com/gtm.js?id=...`
/// - `//www.googletagmanager.com/gtm.js?id=...`
/// - `https://www.google-analytics.com/collect`
/// - `//www.google-analytics.com/g/collect`
/// - `https://analytics.google.com/g/collect`
/// - `//analytics.google.com/g/collect`
///
/// **Requires `//` prefix** — bare domain strings like `"www.googletagmanager.com"`
/// are intentionally NOT matched. gtag.js stores domains as bare strings and
/// constructs URLs dynamically (`"https://" + domain + "/path"`). Rewriting
/// the bare domain produces broken URLs like
/// `https://integrations/google_tag_manager/path` because the script still
/// prepends `"https://"`.
///
/// **Full URL matching for `analytics.google.com`** — Only full URLs with `//` prefix
/// are matched and rewritten (e.g., `https://analytics.google.com/g/collect`).
/// Bare domain strings are not matched due to the same dynamic URL construction issue.
///
/// Captures a trailing delimiter (`/` or `"`) in the last group to prevent false matches
/// on subdomains (e.g., `www.googletagmanager.com.evil.com`).
///
/// The replacement target is `/integrations/google_tag_manager` + the captured delimiter.
static GTM_URL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:https?:)?//(?:www\.(googletagmanager|google-analytics)\.com|analytics\.google\.com)([/"])"#)
        .expect("GTM URL regex should compile")
});

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct GoogleTagManagerConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// GTM Container ID (e.g., "GTM-XXXXXX").
    #[validate(length(min = 1, max = 50), custom(function = "validate_container_id"))]
    pub container_id: String,
    /// Upstream URL for GTM (defaults to <https://www.googletagmanager.com>).
    #[serde(default = "default_upstream")]
    #[validate(url)]
    pub upstream_url: String,
    /// Cache max-age in seconds for the rewritten GTM script (default: 900 to match Google's default).
    #[serde(default = "default_cache_max_age")]
    #[validate(range(min = 60, max = 86400))]
    pub cache_max_age: u32,
    /// Maximum allowed size for POST beacon bodies in bytes (default: 65536 / 64KB).
    /// Prevents memory pressure from oversized payloads on public /collect endpoints.
    #[serde(default = "default_max_beacon_body_size")]
    #[validate(range(min = 1024, max = 1048576))]
    pub max_beacon_body_size: usize,
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

fn default_max_beacon_body_size() -> usize {
    65536 // 64KB - prevents memory pressure from oversized payloads
}

fn validate_container_id(container_id: &str) -> Result<(), validator::ValidationError> {
    if GTM_CONTAINER_ID_PATTERN.is_match(container_id) {
        Ok(())
    } else {
        Err(validator::ValidationError::new(
            "container_id must match format GTM-XXXXXX where X is alphanumeric",
        ))
    }
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
        &self.config.upstream_url
    }

    /// Rewrite GTM and Google Analytics URLs to first-party proxy paths.
    ///
    /// Uses [`GTM_URL_PATTERN`] to handle all URL variants (https, protocol-relative)
    /// for `googletagmanager.com` and `google-analytics.com`.
    fn rewrite_gtm_urls(content: &str) -> String {
        let replacement = format!("/integrations/{}$2", GTM_INTEGRATION_ID);
        GTM_URL_PATTERN
            .replace_all(content, replacement.as_str())
            .into_owned()
    }

    /// Whether an attribute value URL should be rewritten to first-party.
    /// Only matches URLs for which we have corresponding proxy routes
    /// (gtm.js, gtag/js, collect, g/collect). Excludes ns.html and other
    /// GTM endpoints we don't proxy.
    ///
    /// Uses proper URL parsing to prevent false positives from substring matching.
    /// For example, rejects URLs like `https://evil.com/?u=https://www.google-analytics.com/collect`
    fn is_rewritable_url(url: &str) -> bool {
        // List of supported paths we proxy (must match route handlers)
        const SUPPORTED_PATHS: &[&str] =
            &["/gtm.js", "/gtag/js", "/gtag.js", "/collect", "/g/collect"];

        // Parse URL to extract host and path
        // Support both absolute URLs (https://...) and protocol-relative URLs (//...)
        let url_to_parse = if url.starts_with("//") {
            format!("https:{}", url)
        } else if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            // Relative URLs or other formats - not rewritable via this integration
            return false;
        };

        // Extract host and path from URL
        // Format: https://host/path?query
        let without_protocol = url_to_parse
            .trim_start_matches("https://")
            .trim_start_matches("http://");

        let (host, path_with_query) = match without_protocol.split_once('/') {
            Some((h, p)) => (h, format!("/{}", p)),
            None => return false, // No path component
        };

        // Extract path without query string or fragment
        let path = path_with_query
            .split('?')
            .next()
            .and_then(|p| p.split('#').next())
            .unwrap_or("");

        // Validate host is exactly one of our supported GTM/GA domains
        let valid_host = matches!(
            host,
            "www.googletagmanager.com" | "www.google-analytics.com" | "analytics.google.com"
        );

        if !valid_host {
            return false;
        }

        // Validate path is in our allowlist
        SUPPORTED_PATHS.contains(&path)
    }

    /// Both `/gtag/js` (canonical) and `/gtag.js` (alternate) are accepted;
    /// upstream always normalizes to `/gtag/js`.
    fn is_rewritable_script(&self, path: &str) -> bool {
        path.ends_with("/gtm.js") || path.ends_with("/gtag/js") || path.ends_with("/gtag.js")
    }

    fn build_target_url(&self, req: &Request, path: &str) -> Option<String> {
        let upstream_base = self.upstream_url();

        let mut target_url = if path.ends_with("/gtm.js") {
            format!("{}/gtm.js", upstream_base)
        } else if path.ends_with("/gtag/js") || path.ends_with("/gtag.js") {
            format!("{}/gtag/js", upstream_base) // Always normalize to /gtag/js upstream as it's canonical
        } else if path.ends_with("/g/collect") {
            "https://www.google-analytics.com/g/collect".to_string()
        } else if path.ends_with("/collect") {
            "https://www.google-analytics.com/collect".to_string()
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
    ) -> Result<ProxyRequestConfig<'a>, PayloadSizeError> {
        let mut proxy_config = ProxyRequestConfig::new(target_url);
        proxy_config.forward_synthetic_id = false;

        // If it's a POST request (e.g. /collect beacon), we must manually attach the body
        // because ProxyRequestConfig doesn't automatically copy it from the source request.
        if req.get_method() == Method::POST {
            // Read body with size cap to prevent unbounded memory allocation.
            // Read in chunks and reject early if body exceeds max_beacon_body_size.
            let mut body = req.take_body();
            let mut body_bytes = Vec::new();
            let max_size = self.config.max_beacon_body_size;
            const CHUNK_SIZE: usize = 8192; // 8KB chunks

            for chunk_result in body.read_chunks(CHUNK_SIZE) {
                let chunk = chunk_result.map_err(|e| {
                    log::error!("Error reading request body: {}", e);
                    // Convert I/O error to size error for uniform handling
                    PayloadSizeError::TooLarge {
                        actual: 0,
                        max: max_size,
                    }
                })?;

                // Check if adding this chunk would exceed the limit
                // This prevents buffering oversized bodies into memory
                if body_bytes.len() + chunk.len() > max_size {
                    let total_size = body_bytes.len() + chunk.len();
                    log::warn!(
                        "POST body size {} exceeds max {} (rejected during chunked read)",
                        total_size,
                        max_size
                    );
                    return Err(PayloadSizeError::TooLarge {
                        actual: total_size,
                        max: max_size,
                    });
                }

                body_bytes.extend_from_slice(&chunk);
            }

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

        Ok(proxy_config)
    }
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<GoogleTagManagerIntegration>>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<GoogleTagManagerConfig>(GTM_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    Ok(Some(GoogleTagManagerIntegration::new(config)))
}

/// Register the Google Tag Manager integration when enabled.
///
/// # Errors
///
/// Returns an error when the Google Tag Manager integration is enabled with
/// invalid configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(GTM_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration.clone())
            .with_script_rewriter(integration)
            .build(),
    ))
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

        // Validate body size for POST requests to prevent memory pressure
        // Check Content-Length header if present for early rejection
        if method == Method::POST {
            if let Some(content_length_str) =
                req.get_header_str(fastly::http::header::CONTENT_LENGTH)
            {
                match content_length_str.parse::<usize>() {
                    Ok(content_length) => {
                        // Early rejection based on Content-Length
                        if content_length > self.config.max_beacon_body_size {
                            log::warn!(
                                "Rejecting POST beacon with Content-Length {} exceeding max {}",
                                content_length,
                                self.config.max_beacon_body_size
                            );
                            return Ok(Response::from_status(StatusCode::PAYLOAD_TOO_LARGE));
                        }
                    }
                    Err(_) => {
                        // Invalid Content-Length header
                        log::warn!("POST request with malformed Content-Length header");
                        return Ok(Response::from_status(StatusCode::BAD_REQUEST));
                    }
                }
            }
            // If Content-Length is missing, we'll check actual size after read
            // This maintains compatibility with HTTP/2 and intermediaries
        }

        let Some(target_url) = self.build_target_url(&req, &path) else {
            return Ok(Response::from_status(StatusCode::NOT_FOUND));
        };

        log::debug!("Proxying to upstream: {}", target_url);

        // Handle payload size errors explicitly to return 413 instead of 502
        let proxy_config = match self.build_proxy_config(&path, &mut req, &target_url) {
            Ok(config) => config,
            Err(PayloadSizeError::TooLarge { actual, max }) => {
                // This catches cases where Content-Length was incorrect
                log::warn!(
                    "Returning 413: actual body size {} exceeds max {} (Content-Length mismatch)",
                    actual,
                    max
                );
                return Ok(Response::from_status(StatusCode::PAYLOAD_TOO_LARGE));
            }
        };

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
        if Self::is_rewritable_url(attr_value) {
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
        // Note: analytics.google.com is intentionally excluded — gtag.js stores
        // that domain as a bare string and constructs URLs dynamically, so
        // rewriting it in scripts produces broken URLs.
        if content.contains("googletagmanager.com") || content.contains("google-analytics.com") {
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
            var f = "https://analytics.google.com/g/collect";
            var g = "//analytics.google.com/collect";
        "#;

        let result = GoogleTagManagerIntegration::rewrite_gtm_urls(input);

        assert!(result.contains("/integrations/google_tag_manager/gtm.js"));
        assert!(result.contains("/integrations/google_tag_manager/collect"));
        assert!(result.contains("/integrations/google_tag_manager/g/collect"));
        assert!(!result.contains("www.googletagmanager.com"));
        assert!(!result.contains("www.google-analytics.com"));
        assert!(
            !result.contains("analytics.google.com"),
            "analytics.google.com should be rewritten"
        );
    }

    #[test]
    fn test_rewrite_analytics_google_com_full_urls() {
        // Full analytics.google.com URLs (with // prefix) SHOULD be rewritten
        // for HTML attributes where we see the complete URL.
        let input = r#"var f = "https://analytics.google.com/g/collect";"#;
        let result = GoogleTagManagerIntegration::rewrite_gtm_urls(input);
        assert!(
            result.contains("/integrations/google_tag_manager/g/collect"),
            "Full analytics.google.com URLs should be rewritten"
        );
        assert!(
            !result.contains("analytics.google.com"),
            "analytics.google.com should be replaced"
        );
    }

    #[test]
    fn test_rewrite_preserves_non_gtm_urls() {
        let input = r#"var x = "https://example.com/script.js";"#;
        let result = GoogleTagManagerIntegration::rewrite_gtm_urls(input);
        assert_eq!(input, result);
    }

    #[test]
    fn test_rewrite_rejects_subdomain_spoofing() {
        // Should NOT rewrite URLs where the GTM domain is a subdomain of another domain
        let input = r#"var x = "https://www.googletagmanager.com.evil.com/collect";"#;
        let result = GoogleTagManagerIntegration::rewrite_gtm_urls(input);
        assert_eq!(input, result, "should not rewrite spoofed subdomain URLs");
    }

    #[test]
    fn test_rewrite_does_not_touch_bare_domain_strings() {
        // Bare domain strings (without // prefix) must NOT be rewritten.
        // gtag.js stores domains as bare strings and constructs URLs dynamically:
        //   "https://" + domain + "/g/collect"
        // Rewriting the bare domain produces broken URLs like:
        //   https://integrations/google_tag_manager/g/collect
        let input = r#"var d = "www.googletagmanager.com";"#;
        let result = GoogleTagManagerIntegration::rewrite_gtm_urls(input);
        assert_eq!(input, result, "bare domain strings should not be rewritten");

        let input2 = r#"var d = "www.google-analytics.com";"#;
        let result2 = GoogleTagManagerIntegration::rewrite_gtm_urls(input2);
        assert_eq!(
            input2, result2,
            "bare google-analytics domain should not be rewritten"
        );
    }

    #[test]
    fn test_attribute_rewriter() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: "https://www.googletagmanager.com".to_string(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
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
            "https://www.googletagmanager.com/gtm.js?id=GTM-TEST1234",
            &ctx,
        );
        if let AttributeRewriteAction::Replace(val) = action {
            assert_eq!(
                val,
                "/integrations/google_tag_manager/gtm.js?id=GTM-TEST1234"
            );
        } else {
            panic!("Expected Replace action for HTTPS URL, got {:?}", action);
        }

        // Case 2: Protocol-relative URL
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "src",
            "//www.googletagmanager.com/gtm.js?id=GTM-TEST1234",
            &ctx,
        );
        if let AttributeRewriteAction::Replace(val) = action {
            assert_eq!(
                val,
                "/integrations/google_tag_manager/gtm.js?id=GTM-TEST1234"
            );
        } else {
            panic!(
                "Expected Replace action for protocol-relative URL, got {:?}",
                action
            );
        }

        // Case 3: gtag/js URL in href (preload link)
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "href",
            "https://www.googletagmanager.com/gtag/js?id=G-DQMZGMPHXN",
            &ctx,
        );
        if let AttributeRewriteAction::Replace(val) = action {
            assert_eq!(
                val,
                "/integrations/google_tag_manager/gtag/js?id=G-DQMZGMPHXN"
            );
        } else {
            panic!(
                "Expected Replace action for gtag/js preload href, got {:?}",
                action
            );
        }

        // Case 4: google-analytics.com URL in href
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "href",
            "https://www.google-analytics.com/g/collect",
            &ctx,
        );
        if let AttributeRewriteAction::Replace(val) = action {
            assert_eq!(val, "/integrations/google_tag_manager/g/collect");
        } else {
            panic!(
                "Expected Replace action for google-analytics href, got {:?}",
                action
            );
        }

        // Case 5: analytics.google.com URL in href
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "href",
            "https://analytics.google.com/g/collect?v=2",
            &ctx,
        );
        if let AttributeRewriteAction::Replace(val) = action {
            assert_eq!(val, "/integrations/google_tag_manager/g/collect?v=2");
        } else {
            panic!(
                "Expected Replace action for analytics.google.com href, got {:?}",
                action
            );
        }

        // Case 6: Other URL (should be kept)
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "src",
            "https://other.com/script.js",
            &ctx,
        );
        assert!(matches!(action, AttributeRewriteAction::Keep));
    }

    #[test]
    fn test_attribute_rewriter_rejects_false_positives() {
        // Test that URLs with GTM domains in query parameters or paths are NOT rewritten
        // This verifies the fix for P2: proper URL parsing instead of substring matching
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: "https://www.googletagmanager.com".to_string(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
        };
        let integration = GoogleTagManagerIntegration::new(config);

        let ctx = IntegrationAttributeContext {
            attribute_name: "href",
            request_host: "example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        // Case 1: GTM domain in query parameter - should NOT be rewritten
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "href",
            "https://evil.com/?redirect=https://www.google-analytics.com/collect",
            &ctx,
        );
        assert!(
            matches!(action, AttributeRewriteAction::Keep),
            "URLs with GTM domains in query params should not be rewritten"
        );

        // Case 2: GTM domain in path component - should NOT be rewritten
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "href",
            "https://example.com/www.googletagmanager.com/gtm.js",
            &ctx,
        );
        assert!(
            matches!(action, AttributeRewriteAction::Keep),
            "URLs with GTM domains in path should not be rewritten"
        );

        // Case 3: Unsupported path on valid GTM domain - should NOT be rewritten
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "href",
            "https://www.googletagmanager.com/ns.html",
            &ctx,
        );
        assert!(
            matches!(action, AttributeRewriteAction::Keep),
            "Unsupported paths like ns.html should not be rewritten"
        );

        // Case 4: Fragment with GTM domain - should NOT be rewritten
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "href",
            "https://example.com/page#https://www.googletagmanager.com/gtm.js",
            &ctx,
        );
        assert!(
            matches!(action, AttributeRewriteAction::Keep),
            "URLs with GTM domains in fragment should not be rewritten"
        );

        // Case 5: Valid GTM URL should STILL be rewritten (sanity check)
        let action = IntegrationAttributeRewriter::rewrite(
            &*integration,
            "src",
            "https://www.googletagmanager.com/gtm.js?id=GTM-TEST",
            &ctx,
        );
        assert!(
            matches!(action, AttributeRewriteAction::Replace(_)),
            "Valid GTM URLs should still be rewritten"
        );
    }

    #[test]
    fn test_script_rewriter() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: "https://www.googletagmanager.com".to_string(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
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
            max_beacon_body_size: default_max_beacon_body_size(),
        };

        assert!(!config.enabled);
        assert_eq!(config.upstream_url, "https://www.googletagmanager.com");
    }

    #[test]
    fn test_upstream_url_logic() {
        // Default upstream (via serde default)
        let config_default = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234123".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
        };
        let integration_default = GoogleTagManagerIntegration::new(config_default);
        assert_eq!(
            integration_default.upstream_url(),
            "https://www.googletagmanager.com"
        );

        // Custom upstream
        let config_custom = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234123".to_string(),
            upstream_url: "https://gtm.example.com".to_string(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
        };
        let integration_custom = GoogleTagManagerIntegration::new(config_custom);
        assert_eq!(integration_custom.upstream_url(), "https://gtm.example.com");
    }

    #[test]
    fn test_routes_registered() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
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
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
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
        let proxy_config = integration
            .build_proxy_config(&path, &mut req, &target_url)
            .expect("should build proxy config");

        assert_eq!(
            proxy_config.body.as_deref(),
            Some(payload.as_slice()),
            "collect POST should forward payload body"
        );
    }

    #[test]
    fn test_oversized_post_body_rejected() {
        let max_size = default_max_beacon_body_size();
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: max_size,
        };
        let integration = GoogleTagManagerIntegration::new(config);

        // Create a payload larger than the configured max size (64KB by default)
        let oversized_payload = vec![b'X'; max_size + 1];
        let mut req = Request::new(
            Method::POST,
            "https://edge.example.com/integrations/google_tag_manager/collect",
        );
        req.set_body(oversized_payload.clone());

        let path = req.get_path().to_string();
        let target_url = integration
            .build_target_url(&req, &path)
            .expect("should resolve collect target URL");

        // Attempt to build proxy config should fail due to oversized body
        let result = integration.build_proxy_config(&path, &mut req, &target_url);

        assert!(result.is_err(), "Oversized POST body should be rejected");

        if let Err(PayloadSizeError::TooLarge { actual, max }) = result {
            assert_eq!(actual, max_size + 1, "Should report actual size");
            assert_eq!(max, max_size, "Should report max size");
        } else {
            panic!("Expected PayloadSizeError::TooLarge");
        }
    }

    #[test]
    fn test_custom_max_beacon_body_size() {
        // Test with a custom smaller limit
        let custom_max_size = 1024; // 1KB
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: custom_max_size,
        };
        let integration = GoogleTagManagerIntegration::new(config);

        // Payload just under the custom limit should succeed
        let acceptable_payload = vec![b'X'; custom_max_size - 1];
        let mut req1 = Request::new(
            Method::POST,
            "https://edge.example.com/integrations/google_tag_manager/collect",
        );
        req1.set_body(acceptable_payload.clone());

        let path = req1.get_path().to_string();
        let target_url = integration
            .build_target_url(&req1, &path)
            .expect("should resolve collect target URL");

        let result = integration.build_proxy_config(&path, &mut req1, &target_url);
        assert!(result.is_ok(), "Payload under custom limit should succeed");

        // Payload over the custom limit should fail
        let oversized_payload = vec![b'X'; custom_max_size + 1];
        let mut req2 = Request::new(
            Method::POST,
            "https://edge.example.com/integrations/google_tag_manager/collect",
        );
        req2.set_body(oversized_payload);

        let target_url2 = integration
            .build_target_url(&req2, &path)
            .expect("should resolve collect target URL");

        let result2 = integration.build_proxy_config(&path, &mut req2, &target_url2);
        assert!(
            result2.is_err(),
            "Payload over custom limit should be rejected"
        );
    }

    #[test]
    fn test_incorrect_content_length_returns_413() {
        // Verify that when Content-Length is incorrect (smaller than actual body),
        // we still catch it and return 413 (not 502)
        let max_size = default_max_beacon_body_size();
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: max_size,
        };
        let integration = GoogleTagManagerIntegration::new(config);

        // Create oversized payload but with incorrect (small) Content-Length
        let oversized_payload = vec![b'X'; max_size + 1];
        let mut req = Request::new(
            Method::POST,
            "https://edge.example.com/integrations/google_tag_manager/collect",
        );
        req.set_body(oversized_payload.clone());
        // Set Content-Length to a small value (incorrect)
        req.set_header(
            fastly::http::header::CONTENT_LENGTH,
            (max_size / 2).to_string(),
        );

        let path = req.get_path().to_string();
        let target_url = integration
            .build_target_url(&req, &path)
            .expect("should resolve collect target URL");

        // build_proxy_config should detect the mismatch and return PayloadSizeError
        let result = integration.build_proxy_config(&path, &mut req, &target_url);

        assert!(
            result.is_err(),
            "Should reject when actual body exceeds max despite low Content-Length"
        );

        // Verify it's a PayloadSizeError::TooLarge
        if let Err(PayloadSizeError::TooLarge { actual, max }) = result {
            assert_eq!(actual, oversized_payload.len(), "Should report actual size");
            assert_eq!(max, max_size, "Should report max size");
        } else {
            panic!("Expected PayloadSizeError::TooLarge");
        }
    }

    #[tokio::test]
    async fn test_handle_returns_413_for_oversized_post() {
        // Verify that handle() actually returns 413 status code for oversized POST
        let max_size = 1024; // Use small size for testing
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: max_size,
        };
        let integration = GoogleTagManagerIntegration::new(config);

        // Create oversized payload with correct Content-Length
        let oversized_payload = vec![b'X'; max_size + 1];
        let mut req = Request::new(
            Method::POST,
            "https://edge.example.com/integrations/google_tag_manager/collect",
        );
        req.set_body(oversized_payload.clone());
        req.set_header(
            fastly::http::header::CONTENT_LENGTH,
            oversized_payload.len().to_string(),
        );

        let settings = make_settings();
        let response = integration
            .handle(&settings, req)
            .await
            .expect("handle should not return error");

        // Verify we get 413 Payload Too Large, not 502 Bad Gateway
        assert_eq!(
            response.get_status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "Should return 413 for oversized POST body"
        );
    }

    #[tokio::test]
    async fn test_handle_returns_400_for_invalid_content_length() {
        // Verify that handle() returns 400 Bad Request for malformed Content-Length
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
        };
        let integration = GoogleTagManagerIntegration::new(config);

        // Create POST request with invalid Content-Length header
        let payload = b"v=2&tid=G-TEST&cid=123".to_vec();
        let mut req = Request::new(
            Method::POST,
            "https://edge.example.com/integrations/google_tag_manager/collect",
        );
        req.set_body(payload);
        req.set_header(fastly::http::header::CONTENT_LENGTH, "not-a-number");

        let settings = make_settings();
        let response = integration
            .handle(&settings, req)
            .await
            .expect("handle should not return error");

        // Verify we get 400 Bad Request for malformed Content-Length
        assert_eq!(
            response.get_status(),
            StatusCode::BAD_REQUEST,
            "Should return 400 for malformed Content-Length"
        );
    }

    #[tokio::test]
    async fn test_handle_accepts_post_without_content_length() {
        // Verify that POST without Content-Length is accepted (for HTTP/2 compatibility)
        // but still checked against max size after read
        let max_size = default_max_beacon_body_size();
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: max_size,
        };
        let integration = GoogleTagManagerIntegration::new(config);

        // Create small POST request without Content-Length header
        let small_payload = b"v=2&tid=G-TEST&cid=123".to_vec();
        let mut req = Request::new(
            Method::POST,
            "https://edge.example.com/integrations/google_tag_manager/collect",
        );
        req.set_body(small_payload);
        // Intentionally NOT setting Content-Length header (HTTP/2 scenario)

        let path = req.get_path().to_string();
        let target_url = integration
            .build_target_url(&req, &path)
            .expect("should resolve collect target URL");

        // build_proxy_config should accept small payloads even without Content-Length
        let result = integration.build_proxy_config(&path, &mut req, &target_url);

        assert!(
            result.is_ok(),
            "Should accept small POST without Content-Length (HTTP/2 compat)"
        );
    }

    #[test]
    fn test_collect_proxy_config_strips_client_ip_forwarding() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-TEST1234".to_string(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
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
        let proxy_config = integration
            .build_proxy_config(&path, &mut req, &target_url)
            .expect("should build proxy config");

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
            max_beacon_body_size: default_max_beacon_body_size(),
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
        let proxy_config = integration
            .build_proxy_config(&path, &mut req, &target_url)
            .expect("should build proxy config");

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
            <script src="https://www.googletagmanager.com/gtm.js?id=GTM-TEST1234"></script>
        </head><body></body></html>"#;

        let mut settings = make_settings();
        // Enable GTM
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({
                    "enabled": true,
                    "container_id": "GTM-TEST1234",
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
        assert!(processed.contains("/integrations/google_tag_manager/gtm.js?id=GTM-TEST1234"));
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
    fn test_container_id_validation_accepts_valid_ids() {
        // Valid container IDs with different lengths
        let valid_ids = vec![
            "GTM-ABCD",                 // Minimum length (4 chars)
            "GTM-TEST1234",             // 8 chars
            "GTM-ABC123XYZ",            // 10 chars
            "GTM-12345678901234567890", // Maximum length (20 chars)
            "GTM-MIXEDCASE123",         // Mixed alphanumeric
        ];

        for container_id in valid_ids {
            let config = GoogleTagManagerConfig {
                enabled: true,
                container_id: container_id.to_string(),
                upstream_url: default_upstream(),
                cache_max_age: default_cache_max_age(),
                max_beacon_body_size: default_max_beacon_body_size(),
            };

            assert!(
                config.validate().is_ok(),
                "Container ID '{}' should be valid",
                container_id
            );
        }
    }

    #[test]
    fn test_container_id_validation_rejects_invalid_ids() {
        // Invalid container IDs
        let invalid_ids = vec![
            ("GTM-ABC", "too short (3 chars)"),
            ("GTM-123456789012345678901", "too long (21 chars)"),
            ("INVALID", "missing GTM- prefix"),
            ("GTM-", "empty after prefix"),
            ("gtm-ABCD", "lowercase prefix"),
            ("GTM-abc123", "lowercase chars"),
            ("GTM-AB@CD", "special characters"),
            ("GTM-AB CD", "spaces"),
            ("", "empty string"),
        ];

        for (container_id, reason) in invalid_ids {
            let config = GoogleTagManagerConfig {
                enabled: true,
                container_id: container_id.to_string(),
                upstream_url: default_upstream(),
                cache_max_age: default_cache_max_age(),
                max_beacon_body_size: default_max_beacon_body_size(),
            };

            assert!(
                config.validate().is_err(),
                "Container ID '{}' should be invalid ({})",
                container_id,
                reason
            );
        }
    }

    #[test]
    fn test_container_id_validation_max_length() {
        // Test that max length constraint is enforced
        let too_long = "GTM-".to_string() + &"X".repeat(50); // 54 chars total

        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: too_long.clone(),
            upstream_url: default_upstream(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
        };

        assert!(
            config.validate().is_err(),
            "Container ID with {} chars should be rejected (max 50)",
            too_long.len()
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
