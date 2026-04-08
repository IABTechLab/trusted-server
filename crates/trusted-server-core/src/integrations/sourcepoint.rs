use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use regex::Regex;
use serde::Deserialize;
use url::Url;
use validator::Validate;

use crate::backend::BackendConfig;
use crate::error::TrustedServerError;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
use crate::settings::{IntegrationConfig, Settings};

const SOURCEPOINT_INTEGRATION_ID: &str = "sourcepoint";
const SOURCEPOINT_CDN_HOST: &str = "cdn.privacy-mgmt.com";
const SOURCEPOINT_GEO_HOST: &str = "geo.privacymanager.io";
const SOURCEPOINT_CDN_PREFIX: &str = "/integrations/sourcepoint/cdn";
const SOURCEPOINT_GEO_PREFIX: &str = "/integrations/sourcepoint/geo";

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
/// ```
/// We rewrite this so chunks load through the first-party prefix:
/// ```js
/// t.origin + "/integrations/sourcepoint/cdn/unified/4.40.1/"
/// ```
static SP_ORIGIN_UNIFIED_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.origin\s*\+\s*"/unified/"#)
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
    #[validate(url)]
    pub cdn_origin: String,
    /// Base URL for Sourcepoint geo requests.
    #[serde(default = "default_geo_origin")]
    #[validate(url)]
    pub geo_origin: String,
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

fn default_geo_origin() -> String {
    format!("https://{SOURCEPOINT_GEO_HOST}")
}

fn default_cache_ttl() -> u32 {
    3600
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum SourcepointBackend {
    Cdn,
    Geo,
}

pub struct SourcepointIntegration {
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

    fn backend_for_route(path: &str) -> Option<(SourcepointBackend, &str)> {
        if let Some(target_path) = path.strip_prefix(SOURCEPOINT_CDN_PREFIX) {
            return Some((SourcepointBackend::Cdn, normalize_target_path(target_path)));
        }

        path.strip_prefix(SOURCEPOINT_GEO_PREFIX)
            .map(|target_path| (SourcepointBackend::Geo, normalize_target_path(target_path)))
    }

    fn build_target_url(
        &self,
        backend: SourcepointBackend,
        target_path: &str,
        query: Option<&str>,
    ) -> Result<String, Report<TrustedServerError>> {
        let base = match backend {
            SourcepointBackend::Cdn => self.config.cdn_origin.as_str(),
            SourcepointBackend::Geo => self.config.geo_origin.as_str(),
        };

        let mut target =
            Url::parse(base).change_context(Self::error("Invalid Sourcepoint origin URL"))?;
        target.set_path(target_path);
        target.set_query(query);
        Ok(target.to_string())
    }

    fn build_first_party_url(
        &self,
        backend: SourcepointBackend,
        source_url: &str,
        ctx: &IntegrationAttributeContext<'_>,
    ) -> Option<String> {
        let parsed = parse_sourcepoint_url(source_url)?;
        let target_backend = match parsed.host_str()? {
            SOURCEPOINT_CDN_HOST => SourcepointBackend::Cdn,
            SOURCEPOINT_GEO_HOST => SourcepointBackend::Geo,
            _ => return None,
        };

        if target_backend != backend {
            return None;
        }

        let prefix = match target_backend {
            SourcepointBackend::Cdn => SOURCEPOINT_CDN_PREFIX,
            SourcepointBackend::Geo => SOURCEPOINT_GEO_PREFIX,
        };
        let path = parsed.path();
        let query = parsed
            .query()
            .map(|value| format!("?{value}"))
            .unwrap_or_default();

        Some(format!(
            "{}://{}{}{}{}",
            ctx.request_scheme, ctx.request_host, prefix, path, query
        ))
    }

    fn copy_headers(&self, original_req: &Request, proxy_req: &mut Request) {
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
    }

    fn apply_cache_headers(&self, backend: SourcepointBackend, response: &mut Response) {
        if backend == SourcepointBackend::Cdn
            && response.get_header(header::CACHE_CONTROL).is_none()
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
            .replace_all(
                &after_cdn,
                &format!(r#".origin+"{SOURCEPOINT_CDN_PREFIX}/unified/"#),
            )
            .into_owned()
    }

    /// Returns `true` when the response `Content-Type` looks like JavaScript.
    fn is_javascript_response(response: &Response) -> bool {
        response
            .get_header_str(header::CONTENT_TYPE)
            .is_some_and(|ct| {
                ct.contains("javascript") || ct.contains("ecmascript")
            })
    }
}

fn normalize_target_path(target_path: &str) -> &str {
    if target_path.is_empty() {
        "/"
    } else {
        target_path
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
    } else if trimmed.starts_with(SOURCEPOINT_CDN_HOST) || trimmed.starts_with(SOURCEPOINT_GEO_HOST)
    {
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
            .with_attribute_rewriter(integration)
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for SourcepointIntegration {
    fn integration_name(&self) -> &'static str {
        SOURCEPOINT_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            self.get("/cdn/*"),
            self.post("/cdn/*"),
            self.get("/geo"),
            self.get("/geo/*"),
        ]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path().to_string();
        let (backend, target_path) = Self::backend_for_route(&path).ok_or_else(|| {
            Report::new(Self::error(format!("Unknown Sourcepoint route: {path}")))
        })?;

        let target_url = self
            .build_target_url(backend, target_path, req.get_query_str())
            .change_context(Self::error("Failed to build Sourcepoint target URL"))?;
        let base_origin = match backend {
            SourcepointBackend::Cdn => self.config.cdn_origin.as_str(),
            SourcepointBackend::Geo => self.config.geo_origin.as_str(),
        };
        let backend_name = BackendConfig::from_url(base_origin, true)
            .change_context(Self::error("Failed to configure Sourcepoint backend"))?;

        let mut proxy_req = Request::new(req.get_method().clone(), &target_url);
        self.copy_headers(&req, &mut proxy_req);

        if matches!(
            req.get_method(),
            &Method::POST | &Method::PUT | &Method::PATCH
        ) {
            if let Some(content_type) = req.get_header(header::CONTENT_TYPE) {
                proxy_req.set_header(header::CONTENT_TYPE, content_type);
            }
            proxy_req.set_body(req.into_body());
        }

        let mut response = proxy_req
            .send(&backend_name)
            .change_context(Self::error("Sourcepoint upstream request failed"))?;

        // Rewrite CDN URLs inside JavaScript responses so that dynamically
        // loaded chunks and API calls route through the first-party proxy.
        if backend == SourcepointBackend::Cdn
            && response.get_status() == StatusCode::OK
            && self.config.rewrite_sdk
            && Self::is_javascript_response(&response)
        {
            let body = response.take_body_str();
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
            new_response.set_body(rewritten);
            return Ok(new_response);
        }

        self.apply_cache_headers(backend, &mut response);
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

        if let Some(rewritten) =
            self.build_first_party_url(SourcepointBackend::Cdn, attr_value, ctx)
        {
            return AttributeRewriteAction::replace(rewritten);
        }

        if let Some(rewritten) =
            self.build_first_party_url(SourcepointBackend::Geo, attr_value, ctx)
        {
            return AttributeRewriteAction::replace(rewritten);
        }

        AttributeRewriteAction::keep()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::IntegrationRegistry;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::Method;
    use serde_json::json;

    fn config(enabled: bool) -> SourcepointConfig {
        SourcepointConfig {
            enabled,
            rewrite_sdk: true,
            cdn_origin: default_cdn_origin(),
            geo_origin: default_geo_origin(),
            cache_ttl_seconds: default_cache_ttl(),
        }
    }

    #[test]
    fn selects_backend_for_cdn_and_geo_routes() {
        assert_eq!(
            SourcepointIntegration::backend_for_route(
                "/integrations/sourcepoint/cdn/wrapper/v2/messages"
            ),
            Some((SourcepointBackend::Cdn, "/wrapper/v2/messages"))
        );
        assert_eq!(
            SourcepointIntegration::backend_for_route("/integrations/sourcepoint/geo/"),
            Some((SourcepointBackend::Geo, "/"))
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
    fn rewrites_geo_urls_to_first_party_paths() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let ctx = IntegrationAttributeContext {
            attribute_name: "href",
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
        };

        let rewritten = integration.rewrite("href", "https://geo.privacymanager.io/", &ctx);

        assert_eq!(
            rewritten,
            AttributeRewriteAction::replace(
                "https://edge.example.com/integrations/sourcepoint/geo/"
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
        assert!(
            registry.has_route(&Method::GET, "/integrations/sourcepoint/geo"),
            "should register geo proxy route"
        );
    }
}
