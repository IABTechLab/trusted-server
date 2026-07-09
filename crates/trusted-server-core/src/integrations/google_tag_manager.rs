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

use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
use http::{header, Method, Request, Response, StatusCode};
use regex::Regex;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::error::TrustedServerError;
use crate::integrations::{
    collect_response_bounded, AttributeRewriteAction, IntegrationAttributeContext,
    IntegrationAttributeRewriter, IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
    IntegrationScriptContext, IntegrationScriptRewriter, ScriptRewriteAction,
    UPSTREAM_SDK_MAX_RESPONSE_BYTES,
};
use crate::platform::RuntimeServices;
use crate::proxy::{proxy_request, ProxyRequestConfig};
use crate::settings::{IntegrationConfig, Settings};

const GTM_INTEGRATION_ID: &str = "google_tag_manager";
const DEFAULT_UPSTREAM: &str = "https://www.googletagmanager.com";

/// Error type for payload size validation
#[derive(Debug)]
enum PayloadSizeError {
    TooLarge {
        actual: usize,
        max: usize,
    },
    /// Transport error while reading a streaming body chunk.
    StreamRead(String),
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
    #[validate(
        length(min = 1, max = 50),
        regex(
            path = *GTM_CONTAINER_ID_PATTERN,
            message = "container_id must match format GTM-XXXXXX where X is alphanumeric"
        )
    )]
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

/// GTM domain markers the script rewriter looks for. Kept in one place so the
/// boundary-safe prefix check in [`might_contain_gtm_prefix`] and the full-match
/// check in [`GoogleTagManagerIntegration::rewrite`] cannot drift apart.
const GTM_SCRIPT_MARKERS: &[&str] = &["googletagmanager.com", "google-analytics.com"];

/// Minimum trailing-prefix length required to engage the accumulation path.
///
/// Both markers share the prefix `"google"` (6 bytes); below that, the
/// tail is ambiguous with common English (`g`, `go`) and minified tokens
/// (`img`, `slug`, …) and would over-claim. Requiring ≥ 6 bytes means only
/// fragments ending with `"google"` or a longer prefix (`"googletag"`,
/// `"googletagmanager"`, etc.) engage accumulation. Fragments split *within*
/// the common prefix (e.g. "go"|"ogletagmanager.com") will miss the rewrite,
/// but this is extremely rare with the production 8 KB chunk size and is the
/// explicit trade-off documented above.
const GTM_MIN_PREFIX_LEN: usize = 6;

/// Return true if `text` either already contains a GTM marker or ends with a
/// proper prefix of one of length ≥ [`GTM_MIN_PREFIX_LEN`] — i.e., the next
/// fragment could still complete a match. Used to gate whether the GTM
/// rewriter should claim ownership of a script's output via
/// `RemoveNode`/`Replace`.
///
/// Returning false means the rewriter can safely return `Keep` and leave the
/// script untouched, preserving any replacement a more-specific rewriter
/// (e.g., `NextJsNextDataRewriter`) made on the same element.
fn might_contain_gtm_prefix(text: &str) -> bool {
    for marker in GTM_SCRIPT_MARKERS {
        if text.contains(marker) {
            return true;
        }
        // Walk proper prefixes of `marker` from longest down to the minimum
        // length. Below the minimum, trailing prefixes are too short to
        // uniquely identify a GTM path and would over-claim unrelated
        // scripts ending in "g", "go", "goo", "goog", or "googl".
        for len in (GTM_MIN_PREFIX_LEN..marker.len()).rev() {
            if text.ends_with(&marker[..len]) {
                return true;
            }
        }
    }
    false
}

pub struct GoogleTagManagerIntegration {
    config: GoogleTagManagerConfig,
    /// Accumulates text fragments when `lol_html` splits a text node across
    /// chunk boundaries. Drained on `is_last_in_text_node`.
    ///
    /// Uses `Mutex` to satisfy the `Sync` bound on `IntegrationScriptRewriter`.
    /// The pipeline is single-threaded (`lol_html::HtmlRewriter` is `!Send`),
    /// so the lock is uncontended. `lol_html` delivers text chunks sequentially
    /// per element — the buffer is always empty when a new element's text begins.
    accumulated_text: Mutex<String>,
}

impl GoogleTagManagerIntegration {
    fn new(config: GoogleTagManagerConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            accumulated_text: Mutex::new(String::new()),
        })
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

    fn build_target_url(&self, req: &Request<EdgeBody>, path: &str) -> Option<String> {
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

        if let Some(query) = req.uri().query() {
            target_url = format!("{}?{}", target_url, query);
        } else if path.ends_with("/gtm.js") {
            target_url = format!("{}?id={}", target_url, self.config.container_id);
        }

        Some(target_url)
    }

    async fn build_proxy_config<'a>(
        &self,
        path: &str,
        req: &mut Request<EdgeBody>,
        target_url: &'a str,
    ) -> Result<ProxyRequestConfig<'a>, PayloadSizeError> {
        let mut proxy_config = ProxyRequestConfig::new(target_url);
        proxy_config.forward_ec_id = false;

        // If it's a POST request (e.g. /collect beacon), we must manually attach the body
        // because ProxyRequestConfig doesn't automatically copy it from the source request.
        if req.method() == Method::POST {
            // Read body with size cap to prevent unbounded memory allocation.
            let body = std::mem::replace(req.body_mut(), EdgeBody::empty());
            let body_bytes =
                Self::collect_request_body_bounded(body, self.config.max_beacon_body_size).await?;
            proxy_config.body = Some(body_bytes);
        }

        // Explicitly strip X-Forwarded-For to prevent client IP leakage to Google.
        // The empty value will override any existing header during proxy forwarding.
        proxy_config = proxy_config.with_header(
            crate::constants::HEADER_X_FORWARDED_FOR,
            http::HeaderValue::from_static(""),
        );

        if self.is_rewritable_script(path) {
            proxy_config = proxy_config.with_header(
                header::ACCEPT_ENCODING,
                http::HeaderValue::from_static("identity"),
            );
        }

        Ok(proxy_config)
    }

    async fn collect_request_body_bounded(
        body: EdgeBody,
        max_size: usize,
    ) -> Result<Vec<u8>, PayloadSizeError> {
        match body {
            EdgeBody::Once(bytes) => {
                if bytes.len() > max_size {
                    log::warn!(
                        "POST body size {} exceeds max {} (rejected before proxy)",
                        bytes.len(),
                        max_size
                    );
                    return Err(PayloadSizeError::TooLarge {
                        actual: bytes.len(),
                        max: max_size,
                    });
                }
                Ok(bytes.to_vec())
            }
            EdgeBody::Stream(mut stream) => {
                let mut body_bytes = Vec::new();
                while let Some(chunk_result) = stream.next().await {
                    let chunk = chunk_result.map_err(|error| {
                        log::error!("Error reading request body stream: {}", error);
                        PayloadSizeError::StreamRead(error.to_string())
                    })?;

                    if body_bytes.len() + chunk.len() > max_size {
                        let total_size = body_bytes.len() + chunk.len();
                        log::warn!(
                            "POST body size {} exceeds max {} (rejected during stream read)",
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
                Ok(body_bytes)
            }
        }
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
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let mut req = req;
        let path = req.uri().path().to_string();
        let method = req.method().clone();
        log::debug!("Handling GTM request: {} {}", method, path);

        // Validate body size for POST requests to prevent memory pressure
        // Check Content-Length header if present for early rejection
        if method == Method::POST {
            if let Some(content_length_str) = req
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
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
                            return Response::builder()
                                .status(StatusCode::PAYLOAD_TOO_LARGE)
                                .body(EdgeBody::empty())
                                .change_context(Self::error(
                                    "Failed to build GTM payload-too-large response",
                                ));
                        }
                    }
                    Err(_) => {
                        // Invalid Content-Length header
                        log::warn!("POST request with malformed Content-Length header");
                        return Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .body(EdgeBody::empty())
                            .change_context(Self::error(
                                "Failed to build GTM bad-request response",
                            ));
                    }
                }
            }
            // If Content-Length is missing, we'll check actual size after read
            // This maintains compatibility with HTTP/2 and intermediaries
        }

        let Some(target_url) = self.build_target_url(&req, &path) else {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(EdgeBody::empty())
                .change_context(Self::error("Failed to build GTM not-found response"));
        };

        log::debug!("Proxying to upstream: {}", target_url);

        // Handle payload size errors explicitly to return 413 instead of 502
        let proxy_config = match self.build_proxy_config(&path, &mut req, &target_url).await {
            Ok(config) => config,
            Err(PayloadSizeError::TooLarge { actual, max }) => {
                // This catches cases where Content-Length was incorrect
                log::warn!(
                    "Returning 413: actual body size {} exceeds max {} (Content-Length mismatch)",
                    actual,
                    max
                );
                return Response::builder()
                    .status(StatusCode::PAYLOAD_TOO_LARGE)
                    .body(EdgeBody::empty())
                    .change_context(Self::error(
                        "Failed to build GTM payload-too-large response",
                    ));
            }
            Err(PayloadSizeError::StreamRead(error)) => {
                log::error!("Returning 502: failed to read GTM request body stream: {error}");
                return Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(EdgeBody::empty())
                    .change_context(Self::error("Failed to build GTM bad-gateway response"));
            }
        };

        let response = proxy_request(settings, req, proxy_config, services)
            .await
            .change_context(Self::error("Failed to proxy GTM request"))?;

        // If we are serving gtm.js or gtag.js, rewrite internal URLs to route beacons through us.
        if self.is_rewritable_script(&path) {
            if !response.status().is_success() {
                log::warn!("GTM upstream returned status {}", response.status());
                return Ok(response);
            }
            log::debug!("Rewriting GTM/gtag script content");
            let status = response.status();
            let body_bytes = collect_response_bounded(
                response.into_body(),
                UPSTREAM_SDK_MAX_RESPONSE_BYTES,
                GTM_INTEGRATION_ID,
            )
            .await?;
            let body_str = String::from_utf8_lossy(&body_bytes);
            let rewritten_body = Self::rewrite_gtm_urls(&body_str);

            return Response::builder()
                .status(status)
                .header(
                    header::CONTENT_TYPE,
                    "application/javascript; charset=utf-8",
                )
                .header(
                    header::CACHE_CONTROL,
                    format!("public, max-age={}", self.config.cache_max_age),
                )
                .body(EdgeBody::from(rewritten_body.into_bytes()))
                .change_context(Self::error("Failed to build rewritten GTM response"));
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

    fn rewrite(&self, content: &str, ctx: &IntegrationScriptContext<'_>) -> ScriptRewriteAction {
        let mut buf = self
            .accumulated_text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Cheap gate: only engage the accumulation path for scripts whose
        // running text could plausibly contain a GTM/GA domain. Unrelated
        // scripts (the vast majority) return Keep on every fragment so they
        // stay visible in `lol_html`'s output unchanged — and, critically,
        // so a Replace from this rewriter does not clobber a Replace from
        // another rewriter on the same element (e.g., `NextJsNextDataRewriter`
        // on `script#__NEXT_DATA__`). See `might_contain_gtm_prefix` for the
        // boundary-safe substring check.
        let prior_and_current_might_match =
            might_contain_gtm_prefix(&buf) || might_contain_gtm_prefix(content);

        if !ctx.is_last_in_text_node {
            // Intermediate fragment. Only accumulate + suppress when the script
            // might still resolve to a GTM match. Otherwise return Keep so the
            // fragment is emitted as-is by `lol_html` and untouched by us.
            if prior_and_current_might_match {
                buf.push_str(content);
                return ScriptRewriteAction::RemoveNode;
            }
            return ScriptRewriteAction::keep();
        }

        // Last fragment. If we accumulated prior fragments, combine them.
        let full_content: Option<String> = if buf.is_empty() {
            None
        } else {
            buf.push_str(content);
            Some(std::mem::take(&mut *buf))
        };
        let text = full_content.as_deref().unwrap_or(content);

        // Look for the GTM snippet pattern.
        // Standard snippet contains: "googletagmanager.com/gtm.js"
        // Note: analytics.google.com is intentionally excluded — gtag.js stores
        // that domain as a bare string and constructs URLs dynamically, so
        // rewriting it in scripts produces broken URLs.
        if GTM_SCRIPT_MARKERS
            .iter()
            .any(|marker| text.contains(marker))
        {
            return ScriptRewriteAction::replace(Self::rewrite_gtm_urls(text));
        }

        // No GTM content — if we accumulated fragments, emit them unchanged.
        // Intermediate fragments were already suppressed via RemoveNode.
        if full_content.is_some() {
            return ScriptRewriteAction::replace(text.to_string());
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

    #[test]
    fn container_id_validation_matches_gtm_pattern() {
        use validator::Validate as _;

        let config = |id: &str| -> GoogleTagManagerConfig {
            serde_json::from_value(serde_json::json!({ "container_id": id }))
                .expect("should deserialize GTM config")
        };

        // Well-formed container ids pass.
        for good in ["GTM-ABCD", "GTM-ABCD1234", "GTM-A1B2C3D4E5"] {
            config(good)
                .validate()
                .unwrap_or_else(|err| panic!("valid container id {good:?} should pass: {err:?}"));
        }

        // Malformed ids are rejected: wrong prefix, too short, lowercase, bad
        // chars, empty.
        for bad in ["ABCD1234", "GTM-abc", "gtm-ABCD", "GTM-AB", "GTM_ABCD", "GTM-ABCD!", ""] {
            config(bad)
                .validate()
                .expect_err(&format!("invalid container id {bad:?} should be rejected"));
        }
    }

    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;
    use http::Method;
    use std::io::Cursor;

    fn build_http_request(method: Method, uri: &str, body: EdgeBody) -> http::Request<EdgeBody> {
        http::Request::builder()
            .method(method)
            .uri(uri)
            .body(body)
            .expect("should build HTTP request")
    }

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
        futures::executor::block_on(async {
            let config = GoogleTagManagerConfig {
                enabled: true,
                container_id: "GTM-TEST1234".to_string(),
                upstream_url: default_upstream(),
                cache_max_age: default_cache_max_age(),
                max_beacon_body_size: default_max_beacon_body_size(),
            };
            let integration = GoogleTagManagerIntegration::new(config);

            let payload = b"v=2&tid=G-TEST&cid=123&en=page_view".to_vec();
            let mut req = build_http_request(
                Method::POST,
                "https://edge.example.com/integrations/google_tag_manager/g/collect?v=2&tid=G-TEST",
                EdgeBody::from(payload.clone()),
            );

            let path = req.uri().path().to_string();
            let target_url = integration
                .build_target_url(&req, &path)
                .expect("should resolve collect target URL");
            let proxy_config = integration
                .build_proxy_config(&path, &mut req, &target_url)
                .await
                .expect("should build proxy config");

            assert_eq!(
                proxy_config.body.as_deref(),
                Some(payload.as_slice()),
                "collect POST should forward payload body"
            );
        });
    }

    #[test]
    fn test_oversized_post_body_rejected() {
        futures::executor::block_on(async {
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
            let mut req = build_http_request(
                Method::POST,
                "https://edge.example.com/integrations/google_tag_manager/collect",
                EdgeBody::from(oversized_payload.clone()),
            );

            let path = req.uri().path().to_string();
            let target_url = integration
                .build_target_url(&req, &path)
                .expect("should resolve collect target URL");

            // Attempt to build proxy config should fail due to oversized body
            let result = integration
                .build_proxy_config(&path, &mut req, &target_url)
                .await;

            assert!(result.is_err(), "Oversized POST body should be rejected");

            if let Err(PayloadSizeError::TooLarge { actual, max }) = result {
                assert_eq!(actual, max_size + 1, "Should report actual size");
                assert_eq!(max, max_size, "Should report max size");
            } else {
                panic!("Expected PayloadSizeError::TooLarge");
            }
        });
    }

    #[test]
    fn test_custom_max_beacon_body_size() {
        futures::executor::block_on(async {
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
            let mut req1 = build_http_request(
                Method::POST,
                "https://edge.example.com/integrations/google_tag_manager/collect",
                EdgeBody::from(acceptable_payload.clone()),
            );

            let path = req1.uri().path().to_string();
            let target_url = integration
                .build_target_url(&req1, &path)
                .expect("should resolve collect target URL");

            let result = integration
                .build_proxy_config(&path, &mut req1, &target_url)
                .await;
            assert!(result.is_ok(), "Payload under custom limit should succeed");

            // Payload over the custom limit should fail
            let oversized_payload = vec![b'X'; custom_max_size + 1];
            let mut req2 = build_http_request(
                Method::POST,
                "https://edge.example.com/integrations/google_tag_manager/collect",
                EdgeBody::from(oversized_payload),
            );

            let target_url2 = integration
                .build_target_url(&req2, &path)
                .expect("should resolve collect target URL");

            let result2 = integration
                .build_proxy_config(&path, &mut req2, &target_url2)
                .await;
            assert!(
                result2.is_err(),
                "Payload over custom limit should be rejected"
            );
        });
    }

    #[test]
    fn test_incorrect_content_length_returns_413() {
        futures::executor::block_on(async {
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
            let mut req = build_http_request(
                Method::POST,
                "https://edge.example.com/integrations/google_tag_manager/collect",
                EdgeBody::from(oversized_payload.clone()),
            );
            // Set Content-Length to a small value (incorrect)
            req.headers_mut().insert(
                http::header::CONTENT_LENGTH,
                http::HeaderValue::from_str(&(max_size / 2).to_string())
                    .expect("should build Content-Length header"),
            );

            let path = req.uri().path().to_string();
            let target_url = integration
                .build_target_url(&req, &path)
                .expect("should resolve collect target URL");

            // build_proxy_config should detect the mismatch and return PayloadSizeError
            let result = integration
                .build_proxy_config(&path, &mut req, &target_url)
                .await;

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
        });
    }

    #[test]
    fn test_handle_returns_413_for_oversized_post() {
        futures::executor::block_on(async {
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
            let mut req = http::Request::builder()
                .method(Method::POST)
                .uri("https://edge.example.com/integrations/google_tag_manager/collect")
                .body(EdgeBody::from(oversized_payload.clone()))
                .expect("should build oversized request");
            req.headers_mut().insert(
                http::header::CONTENT_LENGTH,
                http::HeaderValue::from_str(&oversized_payload.len().to_string())
                    .expect("should build Content-Length header"),
            );

            let settings = make_settings();
            let response = integration
                .handle(&settings, &noop_services(), req)
                .await
                .expect("handle should not return error");

            // Verify we get 413 Payload Too Large, not 502 Bad Gateway
            assert_eq!(
                response.status(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "Should return 413 for oversized POST body"
            );
        });
    }

    #[test]
    fn test_handle_returns_400_for_invalid_content_length() {
        futures::executor::block_on(async {
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
            let mut req = http::Request::builder()
                .method(Method::POST)
                .uri("https://edge.example.com/integrations/google_tag_manager/collect")
                .body(EdgeBody::from(payload))
                .expect("should build malformed request");
            req.headers_mut().insert(
                http::header::CONTENT_LENGTH,
                http::HeaderValue::from_static("not-a-number"),
            );

            let settings = make_settings();
            let response = integration
                .handle(&settings, &noop_services(), req)
                .await
                .expect("handle should not return error");

            // Verify we get 400 Bad Request for malformed Content-Length
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "Should return 400 for malformed Content-Length"
            );
        });
    }

    #[test]
    fn test_handle_accepts_post_without_content_length() {
        futures::executor::block_on(async {
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
            let mut req = build_http_request(
                Method::POST,
                "https://edge.example.com/integrations/google_tag_manager/collect",
                EdgeBody::from(small_payload),
            );
            // Intentionally NOT setting Content-Length header (HTTP/2 scenario)

            let path = req.uri().path().to_string();
            let target_url = integration
                .build_target_url(&req, &path)
                .expect("should resolve collect target URL");

            // build_proxy_config should accept small payloads even without Content-Length
            let result = integration
                .build_proxy_config(&path, &mut req, &target_url)
                .await;

            assert!(
                result.is_ok(),
                "Should accept small POST without Content-Length (HTTP/2 compat)"
            );
        });
    }

    #[test]
    fn test_collect_proxy_config_strips_client_ip_forwarding() {
        futures::executor::block_on(async {
            let config = GoogleTagManagerConfig {
                enabled: true,
                container_id: "GTM-TEST1234".to_string(),
                upstream_url: default_upstream(),
                cache_max_age: default_cache_max_age(),
                max_beacon_body_size: default_max_beacon_body_size(),
            };
            let integration = GoogleTagManagerIntegration::new(config);

            let mut req = build_http_request(
                Method::GET,
                "https://edge.example.com/integrations/google_tag_manager/collect?v=2",
                EdgeBody::empty(),
            );
            req.headers_mut().insert(
                crate::constants::HEADER_X_FORWARDED_FOR,
                http::HeaderValue::from_static("198.51.100.42"),
            );

            let path = req.uri().path().to_string();
            let target_url = integration
                .build_target_url(&req, &path)
                .expect("should resolve collect target URL");
            let proxy_config = integration
                .build_proxy_config(&path, &mut req, &target_url)
                .await
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
        });
    }

    #[test]
    fn test_gtag_proxy_config_requests_identity_encoding() {
        futures::executor::block_on(async {
            let config = GoogleTagManagerConfig {
                enabled: true,
                container_id: "GT-123".to_string(),
                upstream_url: default_upstream(),
                cache_max_age: default_cache_max_age(),
                max_beacon_body_size: default_max_beacon_body_size(),
            };
            let integration = GoogleTagManagerIntegration::new(config);

            let mut req = build_http_request(
                Method::GET,
                "https://edge.example.com/integrations/google_tag_manager/gtag/js?id=G-123",
                EdgeBody::empty(),
            );

            let path = req.uri().path().to_string();
            let target_url = integration
                .build_target_url(&req, &path)
                .expect("should resolve gtag target URL");
            let proxy_config = integration
                .build_proxy_config(&path, &mut req, &target_url)
                .await
                .expect("should build proxy config");

            let has_identity = proxy_config
                .headers
                .iter()
                .any(|(name, value)| name == http::header::ACCEPT_ENCODING && value == "identity");

            assert!(
                has_identity,
                "gtag/js requests should force Accept-Encoding: identity for rewriting"
            );
        });
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
        create_test_settings()
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
[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "admin-pass"

[publisher]
domain = "test-publisher.com"
cookie_domain = ".test-publisher.com"
origin_url = "https://origin.test-publisher.com"
proxy_secret = "test-secret"

[ec]
passphrase = "test-secret-key-32-bytes-minimum"

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
[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "admin-pass"

[publisher]
domain = "test-publisher.com"
cookie_domain = ".test-publisher.com"
origin_url = "https://origin.test-publisher.com"
proxy_secret = "test-secret"

[ec]
passphrase = "test-secret-key-32-bytes-minimum"

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

    #[test]
    fn fragmented_gtm_snippet_is_accumulated_and_rewritten() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-FRAG1".to_string(),
            upstream_url: "https://www.googletagmanager.com".to_string(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
        };
        let integration = GoogleTagManagerIntegration::new(config);

        let document_state = IntegrationDocumentState::default();

        // Simulate lol_html splitting the GTM snippet mid-domain.
        let fragment1 = r#"(function(w,d,s,l,i){j.src='https://www.google"#;
        let fragment2 = r#"tagmanager.com/gtm.js?id='+i;f.parentNode.insertBefore(j,f);})(window,document,'script','dataLayer','GTM-FRAG1');"#;

        let ctx_intermediate = IntegrationScriptContext {
            selector: "script",
            request_host: "publisher.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node: false,
            document_state: &document_state,
        };
        let ctx_last = IntegrationScriptContext {
            is_last_in_text_node: true,
            ..ctx_intermediate
        };

        // Intermediate fragment: should be suppressed.
        let action1 =
            IntegrationScriptRewriter::rewrite(&*integration, fragment1, &ctx_intermediate);
        assert_eq!(
            action1,
            ScriptRewriteAction::RemoveNode,
            "should suppress intermediate fragment"
        );

        // Last fragment: should emit full rewritten content.
        let action2 = IntegrationScriptRewriter::rewrite(&*integration, fragment2, &ctx_last);
        match action2 {
            ScriptRewriteAction::Replace(rewritten) => {
                assert!(
                    rewritten.contains("/integrations/google_tag_manager/gtm.js"),
                    "should rewrite GTM URL. Got: {rewritten}"
                );
                assert!(
                    !rewritten.contains("googletagmanager.com"),
                    "should not contain original GTM domain. Got: {rewritten}"
                );
            }
            other => panic!("expected Replace for fragmented GTM, got {other:?}"),
        }
    }

    #[test]
    fn non_gtm_fragmented_script_returns_keep_on_every_fragment() {
        // A script with no GTM marker in flight (and no plausible prefix) must
        // return `Keep` on every fragment. Returning `RemoveNode` +
        // `Replace(unchanged)` would stomp on other rewriters' replacements
        // when selectors overlap (see `GoogleTagManagerIntegration::rewrite`).
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-PASS1".to_string(),
            upstream_url: "https://www.googletagmanager.com".to_string(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
        };
        let integration = GoogleTagManagerIntegration::new(config);

        let document_state = IntegrationDocumentState::default();

        let fragment1 = "console.log('hel";
        let fragment2 = "lo world');";

        let ctx_intermediate = IntegrationScriptContext {
            selector: "script",
            request_host: "publisher.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node: false,
            document_state: &document_state,
        };
        let ctx_last = IntegrationScriptContext {
            is_last_in_text_node: true,
            ..ctx_intermediate
        };

        let action1 =
            IntegrationScriptRewriter::rewrite(&*integration, fragment1, &ctx_intermediate);
        assert_eq!(
            action1,
            ScriptRewriteAction::Keep,
            "non-GTM intermediate fragment should not be claimed"
        );

        let action2 = IntegrationScriptRewriter::rewrite(&*integration, fragment2, &ctx_last);
        assert_eq!(
            action2,
            ScriptRewriteAction::Keep,
            "non-GTM final fragment should not be claimed"
        );
    }

    /// Verify the accumulation buffer drains correctly between two consecutive
    /// `<script>` elements. The first is a fragmented GTM script, the second
    /// is a fragmented non-GTM script. Both must produce correct output.
    #[test]
    fn accumulation_buffer_drains_between_consecutive_script_elements() {
        let config = GoogleTagManagerConfig {
            enabled: true,
            container_id: "GTM-MULTI1".to_string(),
            upstream_url: "https://www.googletagmanager.com".to_string(),
            cache_max_age: default_cache_max_age(),
            max_beacon_body_size: default_max_beacon_body_size(),
        };
        let integration = GoogleTagManagerIntegration::new(config);
        let document_state = IntegrationDocumentState::default();

        // --- First <script>: fragmented GTM snippet ---
        let gtm_frag1 = r#"j.src='https://www.google"#;
        let gtm_frag2 = r#"tagmanager.com/gtm.js?id=GTM-MULTI1';"#;

        let ctx_intermediate = IntegrationScriptContext {
            selector: "script",
            request_host: "publisher.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            is_last_in_text_node: false,
            document_state: &document_state,
        };
        let ctx_last = IntegrationScriptContext {
            is_last_in_text_node: true,
            ..ctx_intermediate
        };

        let action =
            IntegrationScriptRewriter::rewrite(&*integration, gtm_frag1, &ctx_intermediate);
        assert_eq!(action, ScriptRewriteAction::RemoveNode);

        let action = IntegrationScriptRewriter::rewrite(&*integration, gtm_frag2, &ctx_last);
        assert!(
            matches!(action, ScriptRewriteAction::Replace(ref s) if s.contains("/integrations/google_tag_manager/gtm.js")),
            "first element: should rewrite GTM URL. Got: {action:?}"
        );

        // --- Second <script>: fragmented non-GTM script ---
        // Buffer must be empty here — no leftover from the first element. With
        // no GTM marker in flight, both fragments return Keep so `lol_html`
        // emits them unchanged and other rewriters remain unaffected.
        let other_frag1 = "console.log('hel";
        let other_frag2 = "lo');";

        let action =
            IntegrationScriptRewriter::rewrite(&*integration, other_frag1, &ctx_intermediate);
        assert_eq!(
            action,
            ScriptRewriteAction::Keep,
            "second element intermediate (non-GTM) should not be claimed"
        );

        let action = IntegrationScriptRewriter::rewrite(&*integration, other_frag2, &ctx_last);
        assert_eq!(
            action,
            ScriptRewriteAction::Keep,
            "second element final (non-GTM) should not be claimed"
        );
    }

    /// Regression test: with a small chunk size, `lol_html` fragments the
    /// inline GTM script text node. The rewriter must accumulate fragments
    /// and produce correct output through the full HTML pipeline.
    #[test]
    fn small_chunk_gtm_rewrite_survives_fragmentation() {
        let mut settings = make_settings();
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({
                    "enabled": true,
                    "container_id": "GTM-SMALL1"
                }),
            )
            .expect("should update config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);

        // Use a very small chunk size to force fragmentation mid-domain.
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 32,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html_input = r#"<html><head><script>j.src='https://www.googletagmanager.com/gtm.js?id=GTM-SMALL1';</script></head><body></body></html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html_input.as_bytes()), &mut output)
            .expect("should process with small chunks");
        let processed = String::from_utf8_lossy(&output);

        assert!(
            processed.contains("/integrations/google_tag_manager/gtm.js"),
            "should rewrite fragmented GTM URL. Got: {processed}"
        );
        assert!(
            !processed.contains("googletagmanager.com"),
            "should not contain original GTM domain. Got: {processed}"
        );
    }

    /// Regression test for the overlapping-rewriter bug: when both the GTM and
    /// Next.js integrations are enabled and a `<script id="__NEXT_DATA__">`
    /// payload is fragmented across chunk boundaries, the GTM rewriter must
    /// NOT clobber the Next.js URL rewrite. In `lol_html`, multiple `text!`
    /// handlers on overlapping selectors run in registration order and the
    /// last `text.replace(...)` wins. Before the fix, GTM accumulated every
    /// script's fragments and re-emitted the unchanged text on `is_last`,
    /// overwriting Next.js's rewrite. The fix is to return `Keep` on scripts
    /// that can't plausibly contain a GTM domain.
    #[test]
    fn fragmented_next_data_survives_with_gtm_enabled() {
        use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
        use std::io::Cursor;

        let mut settings = make_settings();
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({
                    "enabled": true,
                    "container_id": "GTM-MIX1"
                }),
            )
            .expect("should update gtm config");
        settings
            .integrations
            .insert_config(
                "nextjs",
                &serde_json::json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);

        // Small chunks force fragmentation of the __NEXT_DATA__ text node.
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 32,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html_input = r#"<html><body><script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"href":"https://origin.example.com/reviews","title":"Hello World"}}}</script></body></html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html_input.as_bytes()), &mut output)
            .expect("should process with small chunks");
        let processed = String::from_utf8_lossy(&output);

        assert!(
            processed.contains("test.example.com") && processed.contains("/reviews"),
            "Next.js rewrite must survive when GTM is also enabled. Got: {processed}"
        );
        assert!(
            !processed.contains("origin.example.com/reviews"),
            "origin host must not leak through. Got: {processed}"
        );
    }

    /// Regression test for PR #618 P1: fragmented `__NEXT_DATA__` where an
    /// intermediate fragment boundary lands on a short tail like `g` (which
    /// used to be treated as a plausible GTM prefix) must NOT trigger GTM
    /// accumulation. Otherwise GTM would claim the script and overwrite
    /// `NextJs`'s URL rewrite with an unchanged Replace.
    ///
    /// The payload is crafted so a 32-byte chunk boundary lands at the end
    /// of a word ending in `g` ("config"/"img"/"slug"/"thing"), and the
    /// rewritable origin URL appears later in the payload.
    #[test]
    fn fragmented_next_data_with_trailing_g_survives_gtm() {
        use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
        use std::io::Cursor;

        let mut settings = make_settings();
        settings
            .integrations
            .insert_config(
                "google_tag_manager",
                &serde_json::json!({
                    "enabled": true,
                    "container_id": "GTM-GTAIL1"
                }),
            )
            .expect("should update gtm config");
        settings
            .integrations
            .insert_config(
                "nextjs",
                &serde_json::json!({
                    "enabled": true,
                    "rewrite_attributes": ["href", "link", "url"],
                }),
            )
            .expect("should update nextjs config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);

        // chunk_size=32 with this payload produces fragments whose tails
        // include "config", "img", "slug", and "thing" — all ending in `g`.
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 32,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html_input = r#"<html><body><script id="__NEXT_DATA__" type="application/json">{"config":{"img":"slug","thing":"x","href":"https://origin.example.com/reviews"}}</script></body></html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html_input.as_bytes()), &mut output)
            .expect("should process with small chunks");
        let processed = String::from_utf8_lossy(&output);

        assert!(
            processed.contains("test.example.com") && processed.contains("/reviews"),
            "Next.js rewrite must survive when fragments end in short `g`-tails. Got: {processed}"
        );
        assert!(
            !processed.contains("origin.example.com/reviews"),
            "origin host must not leak through. Got: {processed}"
        );
    }

    #[test]
    fn might_contain_gtm_prefix_detects_full_match_and_boundary_prefix() {
        // Full marker present.
        assert!(might_contain_gtm_prefix("xxx googletagmanager.com yyy"));
        assert!(might_contain_gtm_prefix("x google-analytics.com"));

        // Boundary: text ends with a proper prefix of length ≥ GTM_MIN_PREFIX_LEN.
        // "google" itself (6 bytes) is the shortest accepted trailing prefix.
        assert!(might_contain_gtm_prefix("src='https://www.google"));
        assert!(might_contain_gtm_prefix("src='https://www.googletag"));
        assert!(might_contain_gtm_prefix(
            "src='https://www.googletagmanager"
        ));
    }

    #[test]
    fn might_contain_gtm_prefix_rejects_short_ambiguous_tails() {
        // Short tails (< GTM_MIN_PREFIX_LEN) are ambiguous with ordinary
        // English or minified tokens and must NOT engage GTM accumulation.
        // Previously these returned true because any non-empty prefix of a
        // marker was accepted, which let GTM claim and clobber fragments
        // from overlapping script rewriters (see PR #618 P1).
        for text in [
            "x",                  // "g"-less
            "img",                // ends in 'g'
            "slug",               // ends in 'g'
            "config",             // ends in 'g'
            "thing",              // ends in 'g'
            "y go",               // ends in 'go'
            "xgoo",               // ends in 'goo'
            "xgoog",              // ends in 'goog'
            "xgoogl",             // ends in 'googl'
            "console.log('hi');", // no tail match at all
            "",
            "}",
        ] {
            assert!(
                !might_contain_gtm_prefix(text),
                "`{text}` should not engage GTM accumulation"
            );
        }
    }
}
