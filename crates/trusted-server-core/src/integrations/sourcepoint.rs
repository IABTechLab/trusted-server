//! Sourcepoint integration for first-party CMP (Consent Management Platform) delivery.
//!
//! Proxies Sourcepoint's CDN (`cdn.privacy-mgmt.com`) through Trusted Server so
//! the browser loads consent management assets from first-party paths.
//!
//! ## Rewriting layers
//!
//! | Layer | Mechanism | What it catches |
//! |-------|-----------|-----------------|
//! | HTML attributes | `IntegrationAttributeRewriter` | Static `<script src>` / `<link href>` tags |
//! | JS response bodies | `rewrite_script_content` | Webpack chunk paths + hardcoded CDN URLs |
//! | Runtime config | `IntegrationHeadInjector` | `window._sp_` assignments from Next.js chunks |
//! | Dynamic DOM | TS script guard (`script_guard.ts`) | Script/link elements inserted after page load |
//!
//! ## Endpoints
//!
//! | Method | Path | Upstream |
//! |--------|------|----------|
//! | `GET/POST` | `/integrations/sourcepoint/cdn/*` | `cdn.privacy-mgmt.com` |

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use regex::Regex;
use serde::Deserialize;
use url::Url;
use validator::{Validate, ValidationError};

use crate::backend::BackendConfig;
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration,
};
use crate::settings::{IntegrationConfig, Settings};

const SOURCEPOINT_INTEGRATION_ID: &str = "sourcepoint";
const SOURCEPOINT_CDN_HOST: &str = "cdn.privacy-mgmt.com";
const SOURCEPOINT_CDN_PREFIX: &str = "/integrations/sourcepoint/cdn";

/// Matches quoted references to `cdn.privacy-mgmt.com` URLs in script content.
///
/// Pattern breakdown:
/// - `(['"])` — opening quote
/// - `(https?:)?` — optional protocol
/// - `(//)?` — optional protocol-relative slashes
/// - `cdn\.privacy-mgmt\.com` — literal CDN hostname
/// - `(/[^'"]*)?` — optional path (everything until closing quote)
/// - `(['"])` — closing quote
///
/// Handles all common URL styles:
/// - `"https://cdn.privacy-mgmt.com/consent/tcfv2"`
/// - `"//cdn.privacy-mgmt.com/mms/v2"`
/// - `"cdn.privacy-mgmt.com"` (bare domain)
///
/// **Note:** The `regex` crate does not support backreferences, so the opening
/// and closing quote groups (`['"]`) are independent character classes rather
/// than a matched pair.  In practice Sourcepoint's minified JS always uses
/// matching quotes, so this is not a concern for real-world content.
static SP_CDN_URL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(['"])(https?:)?(//)?cdn\.privacy-mgmt\.com(/[^'"]*)?(['"])"#)
        .expect("Sourcepoint CDN URL regex should compile")
});

/// Matches the webpack chunk loading pattern where the script resolves its
/// own origin from `document.currentScript` and appends `/unified/…`.
///
/// The Sourcepoint wrapper builds its public path as:
/// ```js
/// t.origin + "/unified/4.40.1/"
/// // or single-quoted:
/// t.origin + '/unified/4.40.1/'
/// ```
/// We rewrite this so chunks load through the first-party prefix:
/// ```js
/// t.origin + "/integrations/sourcepoint/cdn/unified/4.40.1/"
/// ```
static SP_ORIGIN_UNIFIED_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.origin\s*\+\s*(['"])/unified/"#)
        .expect("Sourcepoint origin+unified regex should compile")
});

/// Configuration for the Sourcepoint first-party proxy.
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct SourcepointConfig {
    /// Whether the integration is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Whether Sourcepoint URLs should be rewritten in HTML.
    #[serde(default = "default_rewrite_sdk")]
    pub rewrite_sdk: bool,
    /// Base URL for Sourcepoint CDN assets and API calls.
    #[serde(default = "default_cdn_origin")]
    #[validate(custom(function = "validate_cdn_origin"))]
    pub cdn_origin: String,
    /// Cache TTL for Sourcepoint static responses in seconds.
    #[serde(default = "default_cache_ttl")]
    #[validate(range(min = 60, max = 86400))]
    pub cache_ttl_seconds: u32,
}

impl IntegrationConfig for SourcepointConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    false
}

fn default_rewrite_sdk() -> bool {
    true
}

fn default_cdn_origin() -> String {
    format!("https://{SOURCEPOINT_CDN_HOST}")
}

fn default_cache_ttl() -> u32 {
    3600
}

/// Validates that `cdn_origin` is a syntactically valid URL whose host ends
/// with `.privacy-mgmt.com`, preventing SSRF via arbitrary origins.
fn validate_cdn_origin(value: &str) -> Result<(), ValidationError> {
    let url = Url::parse(value).map_err(|_| {
        let mut err = ValidationError::new("invalid_url");
        err.message = Some("cdn_origin must be a valid URL".into());
        err
    })?;

    let host = url.host_str().unwrap_or_default();
    if !host.ends_with(".privacy-mgmt.com") {
        let mut err = ValidationError::new("disallowed_host");
        err.message =
            Some("cdn_origin host must end with .privacy-mgmt.com".into());
        return Err(err);
    }

    Ok(())
}

struct SourcepointIntegration {
    config: Arc<SourcepointConfig>,
}

impl SourcepointIntegration {
    fn new(config: Arc<SourcepointConfig>) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: SOURCEPOINT_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    fn strip_cdn_prefix(path: &str) -> Option<&str> {
        path.strip_prefix(SOURCEPOINT_CDN_PREFIX)
            .map(|target_path| {
                if target_path.is_empty() {
                    "/"
                } else {
                    target_path
                }
            })
    }

    fn build_target_url(
        &self,
        target_path: &str,
        query: Option<&str>,
    ) -> Result<String, Report<TrustedServerError>> {
        let mut target = Url::parse(&self.config.cdn_origin)
            .change_context(Self::error("Invalid Sourcepoint CDN origin URL"))?;
        target.set_path(target_path);
        target.set_query(query);
        Ok(target.to_string())
    }

    fn build_first_party_url(
        &self,
        source_url: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> Option<String> {
        let parsed = parse_sourcepoint_url(source_url)?;
        if parsed.host_str()? != SOURCEPOINT_CDN_HOST {
            return None;
        }

        let path = parsed.path();
        let query = parsed
            .query()
            .map(|value| format!("?{value}"))
            .unwrap_or_default();

        Some(format!(
            "{}://{}{}{}{}",
            ctx.request_scheme, ctx.request_host, SOURCEPOINT_CDN_PREFIX, path, query
        ))
    }

    fn copy_headers(&self, original_req: &Request, proxy_req: &mut Request) {
        if let Some(client_ip) = original_req.get_client_ip_addr() {
            proxy_req.set_header("X-Forwarded-For", client_ip.to_string());
        }

        // Accept-Encoding is deliberately omitted here and handled in the
        // caller: paths that need script rewriting request `identity` encoding
        // so the body can be safely read as UTF-8, while other paths forward
        // the client's original encoding.
        for header_name in [
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::USER_AGENT,
            header::REFERER,
            header::ORIGIN,
            header::AUTHORIZATION,
        ] {
            if let Some(value) = original_req.get_header(&header_name) {
                proxy_req.set_header(&header_name, value);
            }
        }
    }

    fn apply_cache_headers(&self, response: &mut Response) {
        if response.get_header(header::CACHE_CONTROL).is_none()
            && response.get_status().is_success()
        {
            response.set_header(
                header::CACHE_CONTROL,
                format!("public, max-age={}", self.config.cache_ttl_seconds),
            );
        }
    }

    /// Rewrite Sourcepoint CDN URLs inside JavaScript response bodies so that
    /// dynamically loaded chunks and API calls route through the first-party
    /// proxy instead of hitting `cdn.privacy-mgmt.com` directly.
    ///
    /// Two patterns are rewritten:
    ///
    /// 1. **Quoted CDN URL references** — e.g. `"https://cdn.privacy-mgmt.com"`
    ///    becomes `"/integrations/sourcepoint/cdn"`, turning absolute third-party
    ///    URLs into root-relative first-party paths.
    ///
    /// 2. **Webpack `origin + "/unified/"` chunk loader** — the Sourcepoint
    ///    wrapper resolves `document.currentScript.src` and appends
    ///    `"/unified/…"`. We insert the CDN prefix so chunks load from
    ///    `/integrations/sourcepoint/cdn/unified/…`.
    fn rewrite_script_content(content: &str) -> String {
        // Step 1: rewrite quoted cdn.privacy-mgmt.com URLs to root-relative paths.
        let after_cdn = SP_CDN_URL_PATTERN
            .replace_all(content, |caps: &regex::Captures| {
                let open_quote = &caps[1];
                let path = caps.get(4).map_or("", |m| m.as_str());
                let close_quote = &caps[5];
                format!(
                    "{}{}{}{close_quote}",
                    open_quote, SOURCEPOINT_CDN_PREFIX, path
                )
            })
            .into_owned();

        // Step 2: rewrite origin+"/unified/" to origin+"/integrations/sourcepoint/cdn/unified/".
        SP_ORIGIN_UNIFIED_PATTERN
            .replace_all(&after_cdn, |caps: &regex::Captures| {
                let quote = &caps[1];
                format!(".origin+{quote}{SOURCEPOINT_CDN_PREFIX}/unified/")
            })
            .into_owned()
    }

    /// Returns `true` for CDN paths that are likely JavaScript bundles.
    ///
    /// Used to decide whether to request uncompressed content from upstream so
    /// the body can be read and rewritten.  Paths that don't match still get
    /// the `is_javascript_response` check after the response arrives, so this
    /// is a conservative preflight — false negatives just mean we skip the
    /// `Accept-Encoding: identity` optimisation for that request.
    fn is_likely_javascript_path(path: &str) -> bool {
        path.ends_with(".js") || path.starts_with("/unified/") || path.starts_with("/wrapper/")
    }

    /// Returns `true` when the response `Content-Type` looks like JavaScript.
    fn is_javascript_response(response: &Response) -> bool {
        response
            .get_header_str(header::CONTENT_TYPE)
            .is_some_and(|ct| ct.contains("javascript") || ct.contains("ecmascript"))
    }
}

fn parse_sourcepoint_url(url: &str) -> Option<Url> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.starts_with(SOURCEPOINT_CDN_HOST) {
        format!("https://{trimmed}")
    } else {
        return None;
    };

    Url::parse(&normalized).ok()
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<SourcepointIntegration>>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<SourcepointConfig>(SOURCEPOINT_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    Ok(Some(SourcepointIntegration::new(Arc::new(config))))
}

/// Register the Sourcepoint integration when enabled.
///
/// # Errors
///
/// Returns an error when the Sourcepoint integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(SOURCEPOINT_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration.clone())
            .with_head_injector(integration)
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for SourcepointIntegration {
    fn integration_name(&self) -> &'static str {
        SOURCEPOINT_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get("/cdn/*"), self.post("/cdn/*")]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path().to_string();
        let method = req.get_method().clone();
        let target_path = Self::strip_cdn_prefix(&path).ok_or_else(|| {
            Report::new(Self::error(format!("Unknown Sourcepoint route: {path}")))
        })?;

        let target_url = self
            .build_target_url(target_path, req.get_query_str())
            .change_context(Self::error("Failed to build Sourcepoint target URL"))?;

        log::info!("Sourcepoint: proxying {method} {path} → {target_url}");

        let backend_name = BackendConfig::from_url(&self.config.cdn_origin, true)
            .change_context(Self::error("Failed to configure Sourcepoint backend"))?;

        let mut proxy_req = Request::new(req.get_method().clone(), &target_url);
        self.copy_headers(&req, &mut proxy_req);

        // Request uncompressed content only for paths that are likely
        // JavaScript (the files we need to regex-rewrite).  All other CDN
        // responses (images, JSON API responses, CSS) keep the client's
        // original Accept-Encoding for efficiency.
        if self.config.rewrite_sdk && Self::is_likely_javascript_path(target_path) {
            proxy_req.set_header(header::ACCEPT_ENCODING, "identity");
        } else if let Some(ae) = req.get_header(header::ACCEPT_ENCODING) {
            proxy_req.set_header(header::ACCEPT_ENCODING, ae);
        }

        if matches!(req.get_method(), &Method::POST) {
            if let Some(content_type) = req.get_header(header::CONTENT_TYPE) {
                proxy_req.set_header(header::CONTENT_TYPE, content_type);
            }
            proxy_req.set_body(req.into_body());
        }

        let mut response = proxy_req
            .send(&backend_name)
            .change_context(Self::error("Sourcepoint upstream request failed"))?;

        log::info!(
            "Sourcepoint: upstream responded with status {}",
            response.get_status()
        );

        // Rewrite Location headers on redirect responses so the browser
        // follows the redirect through the first-party proxy instead of
        // leaking the CDN origin to the client.
        if response.get_status().is_redirection() {
            if let Some(location) = response
                .get_header(header::LOCATION)
                .and_then(|h| h.to_str().ok())
                .filter(|loc| loc.contains(SOURCEPOINT_CDN_HOST))
            {
                let rewritten_location = location
                    .replace(
                        &format!("https://{SOURCEPOINT_CDN_HOST}"),
                        SOURCEPOINT_CDN_PREFIX,
                    )
                    .replace(
                        &format!("http://{SOURCEPOINT_CDN_HOST}"),
                        SOURCEPOINT_CDN_PREFIX,
                    );
                log::info!(
                    "Sourcepoint: rewrote redirect Location to {rewritten_location}"
                );
                response.set_header(header::LOCATION, &rewritten_location);
            }
            self.apply_cache_headers(&mut response);
            return Ok(response);
        }

        // Rewrite CDN URLs inside JavaScript responses so that dynamically
        // loaded chunks and API calls route through the first-party proxy.
        if response.get_status() == StatusCode::OK
            && self.config.rewrite_sdk
            && Self::is_javascript_response(&response)
        {
            log::info!("Sourcepoint: rewriting JavaScript response body for {path}");

            let body_bytes = response.take_body_bytes();
            let body = match String::from_utf8(body_bytes) {
                Ok(text) => text,
                Err(err) => {
                    log::warn!(
                        "Sourcepoint: upstream body for {path} is not valid UTF-8, \
                         passing through unmodified"
                    );
                    let mut passthrough = Response::new();
                    passthrough.set_status(response.get_status());
                    if let Some(ct) = response.get_header(header::CONTENT_TYPE) {
                        passthrough.set_header(header::CONTENT_TYPE, ct);
                    }
                    passthrough.set_body(err.into_bytes());
                    self.apply_cache_headers(&mut passthrough);
                    return Ok(passthrough);
                }
            };
            let rewritten = Self::rewrite_script_content(&body);

            let mut new_response = Response::new();
            new_response.set_status(StatusCode::OK);
            new_response.set_header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            );
            new_response.set_header(
                header::CACHE_CONTROL,
                format!("public, max-age={}", self.config.cache_ttl_seconds),
            );

            // Preserve CORS headers from upstream so cross-origin consumers
            // continue to work through the first-party proxy.
            for header_name in [
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                header::ACCESS_CONTROL_ALLOW_METHODS,
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                header::ACCESS_CONTROL_EXPOSE_HEADERS,
            ] {
                if let Some(value) = response.get_header(&header_name) {
                    new_response.set_header(&header_name, value);
                }
            }

            new_response.set_body(rewritten);
            return Ok(new_response);
        }

        self.apply_cache_headers(&mut response);
        Ok(response)
    }
}

impl IntegrationAttributeRewriter for SourcepointIntegration {
    fn integration_id(&self) -> &'static str {
        SOURCEPOINT_INTEGRATION_ID
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        self.config.rewrite_sdk && matches!(attribute, "src" | "href")
    }

    fn rewrite(
        &self,
        _attr_name: &str,
        attr_value: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if !self.config.rewrite_sdk {
            return AttributeRewriteAction::keep();
        }

        if let Some(rewritten) = self.build_first_party_url(attr_value, ctx) {
            return AttributeRewriteAction::replace(rewritten);
        }

        AttributeRewriteAction::keep()
    }
}

impl IntegrationHeadInjector for SourcepointIntegration {
    fn integration_id(&self) -> &'static str {
        SOURCEPOINT_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        if !self.config.rewrite_sdk {
            return vec![];
        }

        // Install a property trap on `window._sp_` so that when the
        // publisher's code (typically a Next.js hydration chunk) sets the
        // Sourcepoint config object, we intercept it and rewrite any
        // `cdn.privacy-mgmt.com` URLs to the first-party proxy prefix.
        //
        // The trap is transparent: the getter returns the (patched) value and
        // the setter accepts any shape the SDK expects.  We also handle the
        // case where `window._sp_` is already set before our script runs.
        vec![format!(
            concat!(
                "<script>",
                "(function(){{",
                "var C=\"{cdn_host}\";",
                "var P=\"{cdn_prefix}\";",
                "function r(s){{",
                "if(typeof s!==\"string\")return s;",
                "return s.replace(\"https://\"+C,P).replace(\"http://\"+C,P).replace(\"//\"+C,P)",
                "}}",
                "function p(o){{",
                "if(!o||typeof o!==\"object\")return o;",
                "if(o.config){{",
                "if(typeof o.config.baseEndpoint===\"string\")o.config.baseEndpoint=r(o.config.baseEndpoint);",
                "if(typeof o.config.mmsDomain===\"string\")o.config.mmsDomain=r(o.config.mmsDomain);",
                "if(typeof o.config.wrapperAPIOrigin===\"string\")o.config.wrapperAPIOrigin=r(o.config.wrapperAPIOrigin);",
                "if(typeof o.config.cmpOrigin===\"string\")o.config.cmpOrigin=r(o.config.cmpOrigin);",
                "}}",
                "if(typeof o.metricUrl===\"string\")o.metricUrl=r(o.metricUrl);",
                "return o",
                "}}",
                "var v=window._sp_?p(window._sp_):undefined;",
                "Object.defineProperty(window,\"_sp_\",{{",
                "configurable:true,",
                "get:function(){{return v}},",
                "set:function(n){{v=p(n)}}",
                "}});",
                "}})();",
                "</script>",
            ),
            cdn_host = SOURCEPOINT_CDN_HOST,
            cdn_prefix = SOURCEPOINT_CDN_PREFIX,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use crate::test_support::tests::create_test_settings;
    use fastly::http::Method;
    use serde_json::json;

    fn config(enabled: bool) -> SourcepointConfig {
        SourcepointConfig {
            enabled,
            rewrite_sdk: true,
            cdn_origin: default_cdn_origin(),
            cache_ttl_seconds: default_cache_ttl(),
        }
    }

    #[test]
    fn strips_cdn_prefix_from_routes() {
        assert_eq!(
            SourcepointIntegration::strip_cdn_prefix(
                "/integrations/sourcepoint/cdn/wrapper/v2/messages"
            ),
            Some("/wrapper/v2/messages")
        );
        assert_eq!(
            SourcepointIntegration::strip_cdn_prefix("/integrations/sourcepoint/cdn"),
            Some("/")
        );
        assert_eq!(
            SourcepointIntegration::strip_cdn_prefix("/some/other/path"),
            None
        );
    }

    #[test]
    fn rewrites_cdn_urls_to_first_party_paths() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        let rewritten = integration.rewrite(
            "src",
            "https://cdn.privacy-mgmt.com/mms/v2/get_site_data?account_id=821",
            &ctx,
        );

        assert_eq!(
            rewritten,
            AttributeRewriteAction::replace(
                "https://edge.example.com/integrations/sourcepoint/cdn/mms/v2/get_site_data?account_id=821",
            )
        );
    }

    #[test]
    fn leaves_non_sourcepoint_urls_unchanged() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        assert_eq!(
            integration.rewrite("src", "https://example.com/script.js", &ctx),
            AttributeRewriteAction::keep()
        );
    }

    #[test]
    fn rewrites_quoted_cdn_urls_to_root_relative_paths() {
        let input = r#"var fallback="https://cdn.privacy-mgmt.com";var api="https://cdn.privacy-mgmt.com/consent/tcfv2";"#;
        let output = SourcepointIntegration::rewrite_script_content(input);

        assert_eq!(
            output,
            r#"var fallback="/integrations/sourcepoint/cdn";var api="/integrations/sourcepoint/cdn/consent/tcfv2";"#
        );
    }

    #[test]
    fn rewrites_protocol_relative_cdn_urls() {
        let input = r#"url="//cdn.privacy-mgmt.com/mms/v2/get_site_data""#;
        let output = SourcepointIntegration::rewrite_script_content(input);

        assert!(
            output.contains("\"/integrations/sourcepoint/cdn/mms/v2/get_site_data\""),
            "Should rewrite protocol-relative CDN URL. Got: {output}",
        );
    }

    #[test]
    fn rewrites_origin_plus_unified_chunk_pattern() {
        let input = r#"return t.origin+"/unified/4.40.1/"}"#;
        let output = SourcepointIntegration::rewrite_script_content(input);

        assert_eq!(
            output,
            r#"return t.origin+"/integrations/sourcepoint/cdn/unified/4.40.1/"}"#
        );
    }

    #[test]
    fn rewrites_both_patterns_in_realistic_snippet() {
        // Mirrors the real Sourcepoint webpack public path resolution:
        //   try { ... return t.origin+"/unified/4.40.1/" }
        //   catch(e) {} return e+"/unified/4.40.1/"
        // where e defaults to "https://cdn.privacy-mgmt.com"
        let input = concat!(
            r#"var e="https://cdn.privacy-mgmt.com";"#,
            r#"try{var t=document.createElement("a");"#,
            r#"t.href=document.currentScript.src;"#,
            r#"return t.origin+"/unified/4.40.1/"}"#,
            r#"catch(n){}return e+"/unified/4.40.1/""#,
        );

        let output = SourcepointIntegration::rewrite_script_content(input);

        assert!(
            output.contains(r#"var e="/integrations/sourcepoint/cdn";"#),
            "Fallback CDN default should be rewritten. Got: {output}",
        );
        assert!(
            output.contains(r#"t.origin+"/integrations/sourcepoint/cdn/unified/4.40.1/"}"#),
            "Origin chunk path should be prefixed. Got: {output}",
        );
        assert!(
            output.contains(r#"e+"/unified/4.40.1/""#),
            "Fallback concatenation should keep /unified/ since e is already rewritten. Got: {output}",
        );
    }

    #[test]
    fn preserves_non_sourcepoint_urls() {
        let input = r#"var cdn="https://example.com/script.js";var x=t.origin+"/assets/app.js""#;
        let output = SourcepointIntegration::rewrite_script_content(input);

        assert_eq!(output, input, "Non-Sourcepoint URLs should be untouched");
    }

    #[test]
    fn registers_sourcepoint_routes() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(SOURCEPOINT_INTEGRATION_ID, &json!({ "enabled": true }))
            .expect("should insert config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        assert!(
            registry.has_route(
                &Method::GET,
                "/integrations/sourcepoint/cdn/wrapper/v2/messages"
            ),
            "should register CDN proxy route"
        );
    }

    #[test]
    fn attribute_rewriter_skips_when_rewrite_disabled() {
        let mut cfg = config(true);
        cfg.rewrite_sdk = false;
        let integration = SourcepointIntegration::new(Arc::new(cfg));
        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        assert_eq!(
            integration.rewrite("src", "https://cdn.privacy-mgmt.com/wrapper.js", &ctx,),
            AttributeRewriteAction::keep(),
            "should not rewrite when rewrite_sdk is false"
        );
    }

    #[test]
    fn identifies_likely_javascript_paths() {
        assert!(SourcepointIntegration::is_likely_javascript_path(
            "/unified/4.40.1/gdpr-tcf.bundle.js"
        ));
        assert!(SourcepointIntegration::is_likely_javascript_path(
            "/wrapper/v2/messages"
        ));
        assert!(SourcepointIntegration::is_likely_javascript_path(
            "/wrapperMessagingWithoutDetection.js"
        ));
        assert!(!SourcepointIntegration::is_likely_javascript_path(
            "/mms/v2/get_site_data"
        ));
        assert!(!SourcepointIntegration::is_likely_javascript_path(
            "/consent/tcfv2"
        ));
    }

    #[test]
    fn head_injector_emits_sp_property_trap() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "ts.autoblog.com",
            request_scheme: "https",
            origin_host: "origin.autoblog.com",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert_eq!(inserts.len(), 1, "should produce exactly one head insert");

        let script = &inserts[0];
        assert!(
            script.starts_with("<script>") && script.ends_with("</script>"),
            "should be wrapped in script tags: {script}",
        );
        assert!(
            script.contains("cdn.privacy-mgmt.com"),
            "should reference the CDN host to rewrite: {script}",
        );
        assert!(
            script.contains("/integrations/sourcepoint/cdn"),
            "should contain the first-party CDN prefix: {script}",
        );
        assert!(
            script.contains("Object.defineProperty"),
            "should install a property trap on window._sp_: {script}",
        );
        assert!(
            script.contains("baseEndpoint"),
            "should patch baseEndpoint in the config: {script}",
        );
        assert!(
            script.contains("metricUrl"),
            "should patch metricUrl: {script}",
        );
    }

    #[test]
    fn head_injector_returns_empty_when_rewrite_disabled() {
        let mut cfg = config(true);
        cfg.rewrite_sdk = false;
        let integration = SourcepointIntegration::new(Arc::new(cfg));
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "ts.autoblog.com",
            request_scheme: "https",
            origin_host: "origin.autoblog.com",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert!(
            inserts.is_empty(),
            "should not inject anything when rewrite_sdk is false"
        );
    }
}
