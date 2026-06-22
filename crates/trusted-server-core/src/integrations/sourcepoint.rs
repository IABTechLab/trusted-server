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
//! | `GET/POST/HEAD/OPTIONS` | `/integrations/sourcepoint/cdn/*` | `cdn.privacy-mgmt.com` |

use std::net::IpAddr;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::{self, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use regex::Regex;
use serde::Deserialize;
use url::Url;
use validator::{Validate, ValidationError};

use crate::error::TrustedServerError;
use crate::integrations::{
    collect_body_bounded, collect_response_bounded, ensure_integration_backend,
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration, INTEGRATION_MAX_BODY_BYTES,
};
use crate::platform::{PlatformHttpRequest, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

const SOURCEPOINT_INTEGRATION_ID: &str = "sourcepoint";
const SOURCEPOINT_CDN_HOST: &str = "cdn.privacy-mgmt.com";
const SOURCEPOINT_CDN_PREFIX: &str = "/integrations/sourcepoint/cdn";

/// Maximum response body size (5 MB) that will be read into memory for
/// JavaScript rewriting. Responses larger than this are passed through
/// unmodified to avoid unbounded memory consumption.
const MAX_REWRITE_BODY_SIZE: u64 = 5 * 1024 * 1024;

/// Sourcepoint cookie names that are safe to round-trip to the upstream CDN.
///
/// This intentionally excludes unrelated publisher cookies to avoid leaking
/// first-party application state to Sourcepoint. A custom `authCookie` name
/// can be added via [`SourcepointConfig::auth_cookie_name`].
const SOURCEPOINT_COOKIE_ALLOWLIST: &[&str] = &[
    "consentUUID",
    "euconsent-v2",
    "dnsDisplayed",
    "ccpaApplies",
    "signedLspa",
    "_sp_su",
    "consentDate",
    "usnatUUID",
    "consentDateUsnat",
    "globalcmpUUID",
    "consentDateGlobalcmp",
];

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

/// Matches root-relative asset attributes inside Sourcepoint HTML apps.
///
/// Sourcepoint privacy-manager HTML uses root-relative script and stylesheet
/// paths like `/PrivacyManagerUS…`. When served through the first-party proxy,
/// those paths must keep flowing through `/integrations/sourcepoint/cdn` rather
/// than resolving against the publisher origin root.
static SP_ROOT_RELATIVE_ASSET_ATTR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(\b(?:src|href)\s*=\s*)(['"])(/[^/'"][^'"]*)(['"])"#)
        .expect("Sourcepoint root-relative asset regex should compile")
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
    /// Optional custom Sourcepoint auth cookie name to forward upstream.
    ///
    /// Sourcepoint's standard cookie set is allowlisted automatically.
    /// Configure this only when the CMP uses a custom `authCookie` name and
    /// that cookie must round-trip through the first-party proxy.
    #[validate(custom(function = "validate_auth_cookie_name"))]
    pub auth_cookie_name: Option<String>,
    /// Cache TTL for Sourcepoint static responses in seconds.
    #[serde(default = "default_cache_ttl")]
    #[validate(range(min = 60, max = 86400))]
    pub cache_ttl_seconds: u32,
    /// Debug-only override that forces inserted Sourcepoint message containers visible.
    #[serde(default)]
    pub debug_force_banner_visible: bool,
    /// Debug-only override that clears Sourcepoint state before the CMP loads.
    #[serde(default)]
    pub debug_clear_state_on_load: bool,
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

/// Validates that `cdn_origin` is a syntactically valid HTTP(S) URL pointing
/// to exactly `cdn.privacy-mgmt.com`, preventing SSRF via arbitrary origins.
///
/// The host is pinned to `cdn.privacy-mgmt.com` (not `*.privacy-mgmt.com`)
/// because all four rewriting layers (HTML attributes, JS body regex, runtime
/// config trap, client-side DOM guard) hardcode this host.  Allowing a
/// different subdomain would create a config/rewriter mismatch where the
/// proxy works but rewriting silently does nothing.
fn validate_cdn_origin(value: &str) -> Result<(), ValidationError> {
    let url = Url::parse(value).map_err(|_| {
        let mut err = ValidationError::new("invalid_url");
        err.message = Some("cdn_origin must be a valid URL".into());
        err
    })?;

    if !matches!(url.scheme(), "http" | "https") {
        let mut err = ValidationError::new("invalid_scheme");
        err.message = Some("cdn_origin scheme must be http or https".into());
        return Err(err);
    }

    let host = url.host_str().unwrap_or_default();
    if host != SOURCEPOINT_CDN_HOST {
        let mut err = ValidationError::new("disallowed_host");
        err.message = Some(format!("cdn_origin host must be {SOURCEPOINT_CDN_HOST}").into());
        return Err(err);
    }

    let path = url.path();
    if !matches!(path, "" | "/") {
        let mut err = ValidationError::new("disallowed_path");
        err.message = Some("cdn_origin must not include a path".into());
        return Err(err);
    }

    if url.query().is_some() {
        let mut err = ValidationError::new("disallowed_query");
        err.message = Some("cdn_origin must not include a query string".into());
        return Err(err);
    }

    if url.fragment().is_some() {
        let mut err = ValidationError::new("disallowed_fragment");
        err.message = Some("cdn_origin must not include a fragment".into());
        return Err(err);
    }

    Ok(())
}

fn validate_auth_cookie_name(value: &str) -> Result<(), ValidationError> {
    if value.is_empty() || value.trim() != value {
        let mut err = ValidationError::new("invalid_auth_cookie_name");
        err.message =
            Some("auth_cookie_name must be non-empty with no surrounding whitespace".into());
        return Err(err);
    }

    if value.len() > 64 {
        let mut err = ValidationError::new("invalid_auth_cookie_name");
        err.message = Some("auth_cookie_name must be at most 64 characters".into());
        return Err(err);
    }

    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        let mut err = ValidationError::new("invalid_auth_cookie_name");
        err.message = Some("auth_cookie_name may contain only A-Z, a-z, 0-9, '_' and '-'".into());
        return Err(err);
    }

    if [
        "domain", "path", "secure", "httponly", "samesite", "max-age", "expires",
    ]
    .iter()
    .any(|reserved| value.eq_ignore_ascii_case(reserved))
    {
        let mut err = ValidationError::new("invalid_auth_cookie_name");
        err.message = Some("auth_cookie_name must not be a reserved cookie attribute name".into());
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

    fn copy_headers(
        &self,
        client_ip: Option<IpAddr>,
        original_req: &Request<EdgeBody>,
        proxy_req: &mut Request<EdgeBody>,
    ) -> bool {
        if let Some(client_ip) = client_ip {
            if let Ok(val) = HeaderValue::from_str(&client_ip.to_string()) {
                proxy_req.headers_mut().insert("x-forwarded-for", val);
            }
        }

        // Accept-Encoding is deliberately omitted here and handled in the
        // caller: paths that need script rewriting request `identity` encoding
        // so the body can be safely read as UTF-8, while other paths forward
        // the client's original encoding.
        // Authorization is intentionally omitted — forwarding the
        // publisher's bearer token to a third-party CDN would be a
        // credential-leak risk.
        for header_name in [
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::USER_AGENT,
            header::REFERER,
            header::ORIGIN,
            header::HeaderName::from_static("access-control-request-method"),
            header::HeaderName::from_static("access-control-request-headers"),
        ] {
            if let Some(value) = original_req.headers().get(&header_name) {
                proxy_req.headers_mut().insert(header_name, value.clone());
            }
        }

        if let Some(filtered_cookie_header) = self.filtered_sourcepoint_cookie_header(original_req)
        {
            if let Ok(val) = HeaderValue::from_str(&filtered_cookie_header) {
                proxy_req.headers_mut().insert(header::COOKIE, val);
            }
            return true;
        }

        false
    }

    fn filtered_sourcepoint_cookie_header(
        &self,
        original_req: &Request<EdgeBody>,
    ) -> Option<String> {
        let cookie_header = original_req.headers().get(header::COOKIE)?;
        let cookie_header = match cookie_header.to_str() {
            Ok(value) => value,
            Err(_) => {
                log::warn!(
                    "Sourcepoint: request Cookie header is not valid UTF-8, skipping upstream cookie forwarding"
                );
                return None;
            }
        };

        let filtered = cookie_header
            .split(';')
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
            .filter(|pair| {
                let name = pair.split('=').next().unwrap_or_default().trim();
                self.should_forward_sourcepoint_cookie(name)
            })
            .collect::<Vec<_>>()
            .join("; ");

        if filtered.is_empty() {
            None
        } else {
            Some(filtered)
        }
    }

    fn should_forward_sourcepoint_cookie(&self, cookie_name: &str) -> bool {
        SOURCEPOINT_COOKIE_ALLOWLIST.contains(&cookie_name)
            || self.config.auth_cookie_name.as_deref() == Some(cookie_name)
    }

    fn response_sets_cookie(response: &Response<EdgeBody>) -> bool {
        response.headers().contains_key(header::SET_COOKIE)
    }

    fn apply_cookie_safety(response: &mut Response<EdgeBody>) -> bool {
        if Self::response_sets_cookie(response) {
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            );
            return true;
        }

        false
    }

    fn apply_cache_headers(&self, response: &mut Response<EdgeBody>, forwarded_cookies: bool) {
        if Self::apply_cookie_safety(response) {
            return;
        }

        if response.headers().get(header::CACHE_CONTROL).is_none() && response.status().is_success()
        {
            let val = if forwarded_cookies {
                HeaderValue::from_static("private, max-age=0")
            } else {
                HeaderValue::from_str(&format!(
                    "public, max-age={}",
                    self.config.cache_ttl_seconds
                ))
                .unwrap_or(HeaderValue::from_static("public"))
            };
            response.headers_mut().insert(header::CACHE_CONTROL, val);
        }
    }

    /// Rewrites a redirect `Location` header that points to the Sourcepoint CDN
    /// so the browser follows the redirect through the first-party proxy.
    ///
    /// Handles absolute (`https://cdn.privacy-mgmt.com/…`), protocol-relative
    /// (`//cdn.privacy-mgmt.com/…`), and relative locations. Returns `None`
    /// when the location does not reference the CDN host.
    fn rewrite_redirect_location(location: &str, target_url: &str) -> Option<String> {
        // Resolve against the target URL to handle both absolute and
        // protocol-relative Location values.
        let base = Url::parse(target_url).ok()?;
        let resolved = base.join(location).ok()?;

        if resolved.host_str() != Some(SOURCEPOINT_CDN_HOST) {
            return None;
        }

        let query = resolved
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        let fragment = resolved
            .fragment()
            .map(|fragment| format!("#{fragment}"))
            .unwrap_or_default();
        Some(format!(
            "{}{}{}{}",
            SOURCEPOINT_CDN_PREFIX,
            resolved.path(),
            query,
            fragment
        ))
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

    /// Rewrite Sourcepoint HTML app asset URLs through the CDN proxy.
    ///
    /// Sourcepoint privacy-manager apps are hosted from CDN subpaths like
    /// `/us_pm/index.html`, but the HTML points at root-relative assets such as
    /// `/PrivacyManagerUS.b9d1f.css`. Without rewriting, browsers request those
    /// assets from the publisher origin root and receive 404s.
    fn rewrite_html_content(content: &str) -> String {
        SP_ROOT_RELATIVE_ASSET_ATTR_PATTERN
            .replace_all(content, |caps: &regex::Captures| {
                let full_match = &caps[0];
                let attr_prefix = &caps[1];
                let open_quote = &caps[2];
                let path = &caps[3];
                let close_quote = &caps[4];

                if path.starts_with(SOURCEPOINT_CDN_PREFIX) {
                    return full_match.to_string();
                }

                format!("{attr_prefix}{open_quote}{SOURCEPOINT_CDN_PREFIX}{path}{close_quote}")
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
        path.ends_with(".js") || path.ends_with(".mjs") || path.starts_with("/unified/")
    }

    /// Returns `true` for CDN paths that are likely HTML app documents.
    fn is_likely_html_path(path: &str) -> bool {
        path.ends_with(".html") || path.ends_with(".htm")
    }

    /// Returns `true` when the response `Content-Type` looks like JavaScript.
    fn is_javascript_response(response: &Response<EdgeBody>) -> bool {
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("javascript") || ct.contains("ecmascript"))
    }

    /// Returns `true` when the response `Content-Type` looks like HTML.
    fn is_html_response(response: &Response<EdgeBody>) -> bool {
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/html") || ct.contains("application/xhtml"))
    }

    fn remove_vary_accept_encoding(response: &mut Response<EdgeBody>) {
        let vary_owned = match response
            .headers()
            .get(header::VARY)
            .and_then(|v| v.to_str().ok())
        {
            Some(v) => v.to_string(),
            None => return,
        };

        if vary_owned.trim() == "*" {
            return;
        }

        let kept = vary_owned
            .split(',')
            .map(str::trim)
            .filter(|value| !value.eq_ignore_ascii_case("accept-encoding"))
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();

        if kept.is_empty() {
            response.headers_mut().remove(header::VARY);
        } else if let Ok(val) = HeaderValue::from_str(&kept.join(", ")) {
            response.headers_mut().insert(header::VARY, val);
        }
    }

    fn rewrite_javascript_response(&self, response: &mut Response<EdgeBody>, rewritten: String) {
        response.headers_mut().remove(header::CONTENT_ENCODING);
        response.headers_mut().remove(header::CONTENT_LENGTH);
        Self::remove_vary_accept_encoding(response);

        if !Self::apply_cookie_safety(response) {
            // Rewritten JS bundles are static, versioned assets (paths like
            // `/unified/4.40.1/…`), so we apply a fixed public cache policy
            // regardless of what upstream sent. This intentionally diverges from the
            // passthrough path's `apply_cache_headers` (which only sets a default
            // when upstream omitted Cache-Control).
            if let Ok(val) = HeaderValue::from_str(&format!(
                "public, max-age={}",
                self.config.cache_ttl_seconds
            )) {
                response.headers_mut().insert(header::CACHE_CONTROL, val);
            }
        }
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript; charset=utf-8"),
        );

        *response.body_mut() = EdgeBody::from(rewritten.into_bytes());
    }

    fn rewrite_html_response(
        &self,
        response: &mut Response<EdgeBody>,
        rewritten: String,
        forwarded_cookies: bool,
    ) {
        response.headers_mut().remove(header::CONTENT_ENCODING);
        response.headers_mut().remove(header::CONTENT_LENGTH);
        Self::remove_vary_accept_encoding(response);
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        *response.body_mut() = EdgeBody::from(rewritten.into_bytes());
        self.apply_cache_headers(response, forwarded_cookies);
    }

    fn debug_clear_state_script() -> String {
        concat!(
            "<script>",
            "(function(){try{",
            "if(window.__tsjs_sourcepoint_debug_clear_state_installed)return;",
            "window.__tsjs_sourcepoint_debug_clear_state_installed=true;",
            "if(window.console&&console.warn)console.warn(\"Sourcepoint debug_clear_state_on_load enabled; clearing Sourcepoint consent state\");",
            "function m(n){",
            "n=String(n||\"\").toLowerCase();",
            "return n.indexOf(\"_sp\")!==-1||n.indexOf(\"sp_\")!==-1||n.indexOf(\"sourcepoint\")!==-1||n.indexOf(\"consent\")!==-1||n.indexOf(\"gpp\")!==-1||n.indexOf(\"usnat\")!==-1||n.indexOf(\"privacy\")!==-1||n.indexOf(\"ccpa\")!==-1;",
            "}",
            "function clearStorage(store,label){",
            "if(!store)return;",
            "var keys=[];",
            "for(var i=0;i<store.length;i++){var k=store.key(i);if(m(k))keys.push(k)}",
            "for(var j=0;j<keys.length;j++){if(window.console&&console.debug)console.debug(\"Sourcepoint debug: removing \"+label,keys[j]);store.removeItem(keys[j])}",
            "}",
            "clearStorage(window.localStorage,\"localStorage\");",
            "clearStorage(window.sessionStorage,\"sessionStorage\");",
            "var paths=[\"/\",\"/integrations\",\"/integrations/sourcepoint\",\"/integrations/sourcepoint/cdn\",\"/integrations/sourcepoint/cdn/us_pm\"];",
            "var cookies=document.cookie?document.cookie.split(\"; \"):[];",
            "for(var c=0;c<cookies.length;c++){",
            "var name=cookies[c].split(\"=\")[0];",
            "if(!m(name))continue;",
            "for(var p=0;p<paths.length;p++){",
            "document.cookie=name+\"=; path=\"+paths[p]+\"; max-age=0\";",
            "document.cookie=name+\"=; path=\"+paths[p]+\"; max-age=0; Secure; SameSite=Lax\";",
            "}",
            "}",
            "}catch(e){if(window.console&&console.warn)console.warn(\"Sourcepoint: failed to clear debug state\",e)}})();",
            "</script>",
        )
        .to_string()
    }

    fn debug_force_banner_script() -> String {
        concat!(
            "<script>",
            "(function(){try{",
            "if(window.__tsjs_sourcepoint_force_banner_visible_installed)return;",
            "window.__tsjs_sourcepoint_force_banner_visible_installed=true;",
            "if(window.console&&console.warn)console.warn(\"Sourcepoint debug_force_banner_visible enabled; forcing Sourcepoint message containers visible\");",
            "function force(el){",
            "if(!el||!el.id||!/^sp_message_container_/.test(el.id))return;",
            "el.style.setProperty(\"display\",\"block\",\"important\");",
            "el.style.setProperty(\"visibility\",\"visible\",\"important\");",
            "el.style.setProperty(\"opacity\",\"1\",\"important\");",
            "el.style.setProperty(\"pointer-events\",\"auto\",\"important\");",
            "var f=el.querySelector(\"iframe[id^='sp_message_iframe_']\");",
            "if(f){",
            "f.style.setProperty(\"display\",\"block\",\"important\");",
            "f.style.setProperty(\"visibility\",\"visible\",\"important\");",
            "f.style.setProperty(\"opacity\",\"1\",\"important\");",
            "}",
            "}",
            "function scan(root){",
            "if(!root||root.nodeType!==1)return;",
            "if(root.id&&/^sp_message_container_/.test(root.id))force(root);",
            "if(root.querySelectorAll){",
            "var nodes=root.querySelectorAll(\"[id^='sp_message_container_']\");",
            "for(var i=0;i<nodes.length;i++)force(nodes[i]);",
            "}",
            "}",
            "scan(document.documentElement);",
            "var checks=0;",
            "var timer=window.setInterval(function(){",
            "checks++;",
            "scan(document.documentElement);",
            "if(checks>=80)window.clearInterval(timer);",
            "},250);",
            "new MutationObserver(function(muts){",
            "for(var i=0;i<muts.length;i++){",
            "for(var j=0;j<muts[i].addedNodes.length;j++)scan(muts[i].addedNodes[j]);",
            "}",
            "}).observe(document.documentElement,{childList:true,subtree:true});",
            "}catch(e){if(window.console&&console.warn)console.warn(\"Sourcepoint: failed to install debug force banner visibility\",e)}})();",
            "</script>",
        )
        .to_string()
    }
}

fn parse_sourcepoint_url(url: &str) -> Option<Url> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Keep in sync with JS normalization in:
    // crates/js/lib/src/integrations/sourcepoint/script_guard.ts
    // (protocol-relative + bare-domain handling + host-validation behavior).
    let normalized = if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if is_sourcepoint_bare_host_reference(trimmed) {
        format!("https://{trimmed}")
    } else {
        return None;
    };

    Url::parse(&normalized).ok()
}

fn is_sourcepoint_bare_host_reference(value: &str) -> bool {
    let Some(remainder) = value.strip_prefix(SOURCEPOINT_CDN_HOST) else {
        return false;
    };

    remainder
        .as_bytes()
        .first()
        .is_none_or(|byte| matches!(byte, b':' | b'/'))
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
///
/// # Examples
///
/// ```ignore
/// let registration = sourcepoint::register(&settings)?;
/// ```
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
        let endpoint_path = format!("/integrations/{SOURCEPOINT_INTEGRATION_ID}/cdn/*");
        vec![
            self.get("/cdn/*"),
            self.post("/cdn/*"),
            IntegrationEndpoint::new(Method::HEAD, endpoint_path.clone()),
            IntegrationEndpoint::new(Method::OPTIONS, endpoint_path),
        ]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let path = req.uri().path().to_string();
        let method = req.method().clone();
        let target_path = Self::strip_cdn_prefix(&path).ok_or_else(|| {
            Report::new(Self::error(format!("Unknown Sourcepoint route: {path}")))
        })?;

        let target_url = self
            .build_target_url(target_path, req.uri().query())
            .change_context(Self::error("Failed to build Sourcepoint target URL"))?;

        log::info!("Sourcepoint: proxying {method} {path} → {target_url}");

        let (req_parts, req_body) = req.into_parts();

        let request_body = if method == Method::POST {
            let bytes = collect_body_bounded(
                req_body,
                INTEGRATION_MAX_BODY_BYTES,
                SOURCEPOINT_INTEGRATION_ID,
            )
            .await?;
            EdgeBody::from(bytes)
        } else {
            EdgeBody::empty()
        };

        let mut proxy_req = http::Request::builder()
            .method(method.clone())
            .uri(&target_url)
            .body(request_body)
            .change_context(Self::error("Failed to build Sourcepoint proxy request"))?;

        let source_req = http::Request::from_parts(req_parts, EdgeBody::empty());
        let forwarded_cookies =
            self.copy_headers(services.client_info.client_ip, &source_req, &mut proxy_req);

        // Request uncompressed content only for paths that are likely text
        // responses we need to rewrite. All other CDN responses (images, JSON
        // API responses, CSS) keep the client's original Accept-Encoding for
        // efficiency.
        if self.config.rewrite_sdk
            && (Self::is_likely_javascript_path(target_path)
                || Self::is_likely_html_path(target_path))
        {
            proxy_req.headers_mut().insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static("identity"),
            );
        } else if let Some(ae) = source_req.headers().get(header::ACCEPT_ENCODING) {
            proxy_req
                .headers_mut()
                .insert(header::ACCEPT_ENCODING, ae.clone());
        }

        if method == Method::POST {
            if let Some(content_type) = source_req.headers().get(header::CONTENT_TYPE) {
                proxy_req
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, content_type.clone());
            }
        }

        let backend_name = ensure_integration_backend(
            services,
            &self.config.cdn_origin,
            SOURCEPOINT_INTEGRATION_ID,
            None,
        )?;

        let mut response = services
            .http_client()
            .send(PlatformHttpRequest::new(proxy_req, backend_name))
            .await
            .change_context(Self::error("Sourcepoint upstream request failed"))?
            .response;

        log::info!(
            "Sourcepoint: upstream responded with status {}",
            response.status()
        );

        // Rewrite Location headers on redirect responses so the browser
        // follows the redirect through the first-party proxy instead of
        // leaking the CDN origin to the client.
        if response.status().is_redirection() {
            if let Some(location) = response
                .headers()
                .get(header::LOCATION)
                .and_then(|h| h.to_str().ok())
            {
                if let Some(rewritten) = Self::rewrite_redirect_location(location, &target_url) {
                    log::info!("Sourcepoint: rewrote redirect Location to {rewritten}");
                    if let Ok(val) = HeaderValue::from_str(&rewritten) {
                        response.headers_mut().insert(header::LOCATION, val);
                    }
                }
            }
            // Redirects without Set-Cookie intentionally keep upstream cache
            // semantics; default public caching is only applied to successful
            // responses.
            self.apply_cache_headers(&mut response, forwarded_cookies);
            return Ok(response);
        }

        // Rewrite CDN URLs inside JavaScript responses so that dynamically
        // loaded chunks and API calls route through the first-party proxy.
        if method == Method::GET
            && response.status() == StatusCode::OK
            && self.config.rewrite_sdk
            && Self::is_javascript_response(&response)
        {
            log::info!("Sourcepoint: rewriting JavaScript response body for {path}");

            // Guard against unexpectedly large responses to avoid unbounded
            // memory consumption during rewriting.
            let content_length = response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());

            match content_length {
                Some(len) if len > MAX_REWRITE_BODY_SIZE => {
                    log::warn!(
                        "Sourcepoint: response body for {path} exceeds {} bytes \
                         (Content-Length: {len}), skipping rewrite (reason: known_length_too_large)",
                        MAX_REWRITE_BODY_SIZE
                    );
                    self.apply_cache_headers(&mut response, forwarded_cookies);
                    return Ok(response);
                }
                None => {
                    log::warn!(
                        "Sourcepoint: no Content-Length for {path}, \
                         skipping rewrite to avoid unbounded memory read (reason: missing_content_length)"
                    );
                    self.apply_cache_headers(&mut response, forwarded_cookies);
                    return Ok(response);
                }
                Some(_) => {}
            }

            let (resp_parts, resp_body) = response.into_parts();
            let body_bytes = collect_response_bounded(
                resp_body,
                MAX_REWRITE_BODY_SIZE as usize,
                SOURCEPOINT_INTEGRATION_ID,
            )
            .await?;
            let mut response = http::Response::from_parts(resp_parts, EdgeBody::empty());

            let body = match String::from_utf8(body_bytes) {
                Ok(text) => text,
                Err(err) => {
                    log::warn!(
                        "Sourcepoint: upstream body for {path} is not valid UTF-8 \
                         at byte offset {}, passing through unmodified",
                        err.utf8_error().valid_up_to()
                    );
                    *response.body_mut() = EdgeBody::from(err.into_bytes());
                    self.apply_cache_headers(&mut response, forwarded_cookies);
                    return Ok(response);
                }
            };
            let rewritten = Self::rewrite_script_content(&body);

            self.rewrite_javascript_response(&mut response, rewritten);
            return Ok(response);
        }

        // Rewrite root-relative asset URLs inside Sourcepoint HTML apps so
        // privacy-manager iframes can load their CSS and JavaScript through the
        // first-party CDN proxy.
        if method == Method::GET
            && response.status() == StatusCode::OK
            && self.config.rewrite_sdk
            && Self::is_html_response(&response)
        {
            log::info!("Sourcepoint: rewriting HTML response body for {path}");

            let content_length = response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());

            match content_length {
                Some(len) if len > MAX_REWRITE_BODY_SIZE => {
                    log::warn!(
                        "Sourcepoint: response body for {path} exceeds {} bytes \
                         (Content-Length: {len}), skipping rewrite (reason: known_length_too_large)",
                        MAX_REWRITE_BODY_SIZE
                    );
                    self.apply_cache_headers(&mut response, forwarded_cookies);
                    return Ok(response);
                }
                None => {
                    log::warn!(
                        "Sourcepoint: no Content-Length for {path}, \
                         skipping rewrite to avoid unbounded memory read (reason: missing_content_length)"
                    );
                    self.apply_cache_headers(&mut response, forwarded_cookies);
                    return Ok(response);
                }
                Some(_) => {}
            }

            let (resp_parts, resp_body) = response.into_parts();
            let body_bytes = collect_response_bounded(
                resp_body,
                MAX_REWRITE_BODY_SIZE as usize,
                SOURCEPOINT_INTEGRATION_ID,
            )
            .await?;
            let mut response = http::Response::from_parts(resp_parts, EdgeBody::empty());

            let body = match String::from_utf8(body_bytes) {
                Ok(text) => text,
                Err(err) => {
                    log::warn!(
                        "Sourcepoint: upstream body for {path} is not valid UTF-8 \
                         at byte offset {}, passing through unmodified",
                        err.utf8_error().valid_up_to()
                    );
                    *response.body_mut() = EdgeBody::from(err.into_bytes());
                    self.apply_cache_headers(&mut response, forwarded_cookies);
                    return Ok(response);
                }
            };
            let rewritten = Self::rewrite_html_content(&body);

            self.rewrite_html_response(&mut response, rewritten, forwarded_cookies);
            return Ok(response);
        }

        self.apply_cache_headers(&mut response, forwarded_cookies);
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
        // `handles_attribute()` already gates on `rewrite_sdk`, so this
        // method is only called when rewriting is enabled.
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
        let mut inserts = vec![format!(
            "<script>window.__tsjs_sourcepoint={{\"rewriteSdk\":{},\"debugForceBannerVisible\":{},\"debugClearStateOnLoad\":{}}};</script>",
            self.config.rewrite_sdk,
            self.config.debug_force_banner_visible,
            self.config.debug_clear_state_on_load,
        )];

        if self.config.debug_clear_state_on_load {
            inserts.push(Self::debug_clear_state_script());
        }

        if !self.config.rewrite_sdk {
            if self.config.debug_force_banner_visible {
                inserts.push(Self::debug_force_banner_script());
            }
            return inserts;
        }

        // Install a property trap on `window._sp_` so that when the
        // publisher's code (typically a Next.js hydration chunk) sets the
        // Sourcepoint config object, we intercept it and rewrite any
        // `cdn.privacy-mgmt.com` URLs to the first-party proxy prefix.
        //
        // The trap is transparent: the getter returns the (patched) value and
        // the setter accepts any shape the SDK expects.  We also handle the
        // case where `window._sp_` is already set before our script runs.
        //
        // Limitations:
        // - Only intercepts top-level assignment (`window._sp_ = …`).  Nested
        //   mutation like `window._sp_.config.baseEndpoint = "…"` after the
        //   initial assignment is not caught.  The JS body regex rewriter
        //   covers that case for string literals in bundled code.
        // - `s.replace()` replaces only the first occurrence per call, which
        //   is fine for the current set of scalar URL config fields.
        inserts.push(format!(
            concat!(
                "<script>",
                "(function(){{try{{",
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
                "}}catch(e){{if(window.console&&console.warn)console.warn(\"Sourcepoint: failed to install runtime config rewrite trap\",e)}}}})();",
                "</script>",
            ),
            cdn_host = SOURCEPOINT_CDN_HOST,
            cdn_prefix = SOURCEPOINT_CDN_PREFIX,
        ));

        if self.config.debug_force_banner_visible {
            inserts.push(Self::debug_force_banner_script());
        }

        inserts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;

    fn config(enabled: bool) -> SourcepointConfig {
        SourcepointConfig {
            enabled,
            rewrite_sdk: true,
            cdn_origin: default_cdn_origin(),
            auth_cookie_name: None,
            cache_ttl_seconds: default_cache_ttl(),
            debug_force_banner_visible: false,
            debug_clear_state_on_load: false,
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
    fn rewrites_html_root_relative_assets_to_cdn_proxy_prefix() {
        let input = concat!(
            r#"<link rel="manifest" href="/manifest.json">"#,
            r#"<link href='/PrivacyManagerUS.b9d1f.css' rel="preload">"#,
            r#"<script src="/PrivacyManagerUS.1234.js"></script>"#,
            r#"<script src="//cdn.example.com/keep.js"></script>"#,
            "<a href=\"#privacy\">Privacy</a>",
            r#"<script src="/integrations/sourcepoint/cdn/already.js"></script>"#,
        );

        let output = SourcepointIntegration::rewrite_html_content(input);

        assert!(
            output.contains(r#"href="/integrations/sourcepoint/cdn/manifest.json""#),
            "should rewrite root-relative manifest path: {output}"
        );
        assert!(
            output.contains(r#"href='/integrations/sourcepoint/cdn/PrivacyManagerUS.b9d1f.css'"#),
            "should rewrite single-quoted stylesheet path: {output}"
        );
        assert!(
            output.contains(r#"src="/integrations/sourcepoint/cdn/PrivacyManagerUS.1234.js""#),
            "should rewrite root-relative script path: {output}"
        );
        assert!(
            output.contains(r#"src="//cdn.example.com/keep.js""#),
            "should not rewrite protocol-relative paths: {output}"
        );
        assert!(
            output.contains("href=\"#privacy\""),
            "should not rewrite fragment links: {output}"
        );
        assert!(
            output.contains(r#"src="/integrations/sourcepoint/cdn/already.js""#),
            "should not double-prefix already rewritten paths: {output}"
        );
    }

    #[test]
    fn registers_sourcepoint_routes() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(SOURCEPOINT_INTEGRATION_ID, &json!({ "enabled": true }))
            .expect("should insert config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        for method in [Method::GET, Method::POST, Method::HEAD, Method::OPTIONS] {
            assert!(
                registry.has_route(&method, "/integrations/sourcepoint/cdn/wrapper/v2/messages"),
                "should register {method} CDN proxy route"
            );
        }
    }

    #[test]
    fn attribute_rewriter_skips_when_rewrite_disabled() {
        let mut cfg = config(true);
        cfg.rewrite_sdk = false;
        let integration = SourcepointIntegration::new(Arc::new(cfg));

        assert!(
            !integration.handles_attribute("src"),
            "should not handle src when rewrite_sdk is false"
        );
        assert!(
            !integration.handles_attribute("href"),
            "should not handle href when rewrite_sdk is false"
        );
    }

    #[test]
    fn identifies_likely_javascript_paths() {
        assert!(SourcepointIntegration::is_likely_javascript_path(
            "/unified/4.40.1/gdpr-tcf.bundle.js"
        ));
        assert!(!SourcepointIntegration::is_likely_javascript_path(
            "/wrapper/v2/messages"
        ));
        assert!(SourcepointIntegration::is_likely_javascript_path(
            "/wrapperMessagingWithoutDetection.js"
        ));
        assert!(SourcepointIntegration::is_likely_javascript_path(
            "/module/sourcepoint.mjs"
        ));
        assert!(!SourcepointIntegration::is_likely_javascript_path(
            "/mms/v2/get_site_data"
        ));
        assert!(!SourcepointIntegration::is_likely_javascript_path(
            "/consent/tcfv2"
        ));
    }

    #[test]
    fn identifies_likely_html_paths() {
        assert!(SourcepointIntegration::is_likely_html_path(
            "/us_pm/index.html"
        ));
        assert!(SourcepointIntegration::is_likely_html_path(
            "/privacy-manager.htm"
        ));
        assert!(!SourcepointIntegration::is_likely_html_path(
            "/PrivacyManagerUS.b9d1f.css"
        ));
    }

    #[test]
    fn head_injector_emits_config_script_plus_trap_when_enabled() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "ts.prospecta.com",
            request_scheme: "https",
            origin_host: "origin.prospecta.com",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert_eq!(
            inserts.len(),
            2,
            "should emit config plus trap script when enabled"
        );

        let config_script = &inserts[0];
        assert!(
            config_script.contains(
                "window.__tsjs_sourcepoint={\"rewriteSdk\":true,\"debugForceBannerVisible\":false,\"debugClearStateOnLoad\":false}"
            ),
            "should emit rewrite SDK config script: {config_script}"
        );

        let trap_script = &inserts[1];
        assert!(
            trap_script.starts_with("<script>") && trap_script.ends_with("</script>"),
            "should be wrapped in script tags: {trap_script}",
        );
        assert!(
            trap_script.contains("cdn.privacy-mgmt.com"),
            "should reference the CDN host to rewrite: {trap_script}",
        );
        assert!(
            trap_script.contains("/integrations/sourcepoint/cdn"),
            "should contain the first-party CDN prefix: {trap_script}",
        );
        assert!(
            trap_script.contains("try{") && trap_script.contains("catch(e)"),
            "should guard best-effort trap installation: {trap_script}",
        );
        assert!(
            trap_script.contains("console.warn"),
            "should log trap installation failures for observability: {trap_script}",
        );
        assert!(
            trap_script.contains("Object.defineProperty"),
            "should install a property trap on window._sp_: {trap_script}",
        );
        for config_field in ["baseEndpoint", "mmsDomain", "wrapperAPIOrigin", "cmpOrigin"] {
            assert!(
                trap_script.contains(&format!("o.config.{config_field}")),
                "should patch config field {config_field}: {trap_script}",
            );
        }
        assert!(
            trap_script.contains("o.metricUrl"),
            "should patch top-level metricUrl: {trap_script}",
        );
    }

    #[test]
    fn head_injector_returns_config_when_rewrite_disabled() {
        let mut cfg = config(true);
        cfg.rewrite_sdk = false;
        let integration = SourcepointIntegration::new(Arc::new(cfg));
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "ts.prospecta.com",
            request_scheme: "https",
            origin_host: "origin.prospecta.com",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert_eq!(
            inserts.len(),
            1,
            "should emit only config script when rewrite_sdk is false"
        );
        assert!(
            inserts[0].contains(
                "window.__tsjs_sourcepoint={\"rewriteSdk\":false,\"debugForceBannerVisible\":false,\"debugClearStateOnLoad\":false}"
            ),
            "should flag rewriteSdk false"
        );
        assert!(
            !inserts[0].contains("Object.defineProperty"),
            "should not emit runtime trap when rewrite_sdk is disabled"
        );
    }

    #[test]
    fn head_injector_emits_debug_force_banner_script_when_enabled() {
        let mut cfg = config(true);
        cfg.debug_force_banner_visible = true;
        let integration = SourcepointIntegration::new(Arc::new(cfg));
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "ts.prospecta.com",
            request_scheme: "https",
            origin_host: "origin.prospecta.com",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert_eq!(
            inserts.len(),
            3,
            "should emit config, rewrite trap, and debug force script"
        );
        assert!(
            inserts[0].contains(
                "window.__tsjs_sourcepoint={\"rewriteSdk\":true,\"debugForceBannerVisible\":true,\"debugClearStateOnLoad\":false}"
            ),
            "should expose debug force flag in config script"
        );

        let debug_script = &inserts[2];
        assert!(
            debug_script.contains("debug_force_banner_visible enabled"),
            "should warn that debug forcing is enabled: {debug_script}"
        );
        assert!(
            debug_script.contains("sp_message_container_"),
            "should target Sourcepoint message containers: {debug_script}"
        );
        assert!(
            debug_script.contains("setProperty(\"display\",\"block\",\"important\")"),
            "should force display block with important priority: {debug_script}"
        );
    }

    #[test]
    fn head_injector_emits_debug_force_banner_script_when_rewrite_disabled() {
        let mut cfg = config(true);
        cfg.rewrite_sdk = false;
        cfg.debug_force_banner_visible = true;
        let integration = SourcepointIntegration::new(Arc::new(cfg));
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "ts.prospecta.com",
            request_scheme: "https",
            origin_host: "origin.prospecta.com",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert_eq!(
            inserts.len(),
            2,
            "should emit config plus debug force script when rewrite is disabled"
        );
        assert!(
            !inserts
                .iter()
                .any(|insert| insert.contains("Object.defineProperty")),
            "should not install runtime rewrite trap when rewrite_sdk is disabled"
        );
        assert!(
            inserts[1].contains("sp_message_container_"),
            "should still emit debug force script"
        );
    }

    #[test]
    fn head_injector_emits_debug_clear_state_script_when_enabled() {
        let mut cfg = config(true);
        cfg.debug_clear_state_on_load = true;
        let integration = SourcepointIntegration::new(Arc::new(cfg));
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "ts.prospecta.com",
            request_scheme: "https",
            origin_host: "origin.prospecta.com",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert_eq!(
            inserts.len(),
            3,
            "should emit config, debug clear script, and rewrite trap"
        );
        assert!(
            inserts[0].contains(
                "window.__tsjs_sourcepoint={\"rewriteSdk\":true,\"debugForceBannerVisible\":false,\"debugClearStateOnLoad\":true}"
            ),
            "should expose debug clear flag in config script"
        );
        assert!(
            inserts[1].contains("debug_clear_state_on_load enabled"),
            "should warn that debug state clearing is enabled: {}",
            inserts[1]
        );
        assert!(
            inserts[1].contains("localStorage") && inserts[1].contains("document.cookie"),
            "should clear Sourcepoint storage and cookies: {}",
            inserts[1]
        );
        assert!(
            inserts[2].contains("Object.defineProperty"),
            "should keep rewrite trap after debug clear script"
        );
    }

    #[test]
    fn head_injector_orders_debug_clear_before_debug_force() {
        let mut cfg = config(true);
        cfg.debug_clear_state_on_load = true;
        cfg.debug_force_banner_visible = true;
        let integration = SourcepointIntegration::new(Arc::new(cfg));
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "ts.prospecta.com",
            request_scheme: "https",
            origin_host: "origin.prospecta.com",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert_eq!(
            inserts.len(),
            4,
            "should emit config, debug clear, rewrite trap, and debug force"
        );
        assert!(
            inserts[1].contains("debug_clear_state_on_load enabled"),
            "should clear state before later scripts run"
        );
        assert!(
            inserts[3].contains("debug_force_banner_visible enabled"),
            "should force visibility after state clearing"
        );
    }

    #[test]
    fn rejects_cdn_origin_outside_privacy_mgmt_domain() {
        let cfg = SourcepointConfig {
            enabled: true,
            rewrite_sdk: true,
            cdn_origin: "http://169.254.169.254".to_string(),
            auth_cookie_name: None,
            cache_ttl_seconds: default_cache_ttl(),
            debug_force_banner_visible: false,
            debug_clear_state_on_load: false,
        };
        assert!(
            cfg.validate().is_err(),
            "should reject cdn_origin not on cdn.privacy-mgmt.com"
        );
    }

    #[test]
    fn rejects_cdn_origin_with_non_http_scheme() {
        let cfg = SourcepointConfig {
            enabled: true,
            rewrite_sdk: true,
            cdn_origin: "ftp://cdn.privacy-mgmt.com".to_string(),
            auth_cookie_name: None,
            cache_ttl_seconds: default_cache_ttl(),
            debug_force_banner_visible: false,
            debug_clear_state_on_load: false,
        };
        assert!(cfg.validate().is_err(), "should reject non-HTTP(S) scheme");
    }

    #[test]
    fn rejects_cdn_origin_with_different_subdomain() {
        let cfg = SourcepointConfig {
            enabled: true,
            rewrite_sdk: true,
            cdn_origin: "https://cdn-eu.privacy-mgmt.com".to_string(),
            auth_cookie_name: None,
            cache_ttl_seconds: default_cache_ttl(),
            debug_force_banner_visible: false,
            debug_clear_state_on_load: false,
        };
        assert!(
            cfg.validate().is_err(),
            "should reject subdomain other than cdn.privacy-mgmt.com"
        );
    }

    #[test]
    fn rejects_cdn_origin_with_path() {
        let cfg = SourcepointConfig {
            enabled: true,
            rewrite_sdk: true,
            cdn_origin: "https://cdn.privacy-mgmt.com/edge".to_string(),
            auth_cookie_name: None,
            cache_ttl_seconds: default_cache_ttl(),
            debug_force_banner_visible: false,
            debug_clear_state_on_load: false,
        };
        assert!(
            cfg.validate().is_err(),
            "should reject path components in cdn_origin"
        );
    }

    #[test]
    fn rejects_cdn_origin_with_query() {
        let cfg = SourcepointConfig {
            enabled: true,
            rewrite_sdk: true,
            cdn_origin: "https://cdn.privacy-mgmt.com?edge=1".to_string(),
            auth_cookie_name: None,
            cache_ttl_seconds: default_cache_ttl(),
            debug_force_banner_visible: false,
            debug_clear_state_on_load: false,
        };
        assert!(
            cfg.validate().is_err(),
            "should reject query strings in cdn_origin"
        );
    }

    #[test]
    fn rejects_cdn_origin_with_fragment() {
        let cfg = SourcepointConfig {
            enabled: true,
            rewrite_sdk: true,
            cdn_origin: "https://cdn.privacy-mgmt.com#edge".to_string(),
            auth_cookie_name: None,
            cache_ttl_seconds: default_cache_ttl(),
            debug_force_banner_visible: false,
            debug_clear_state_on_load: false,
        };
        assert!(
            cfg.validate().is_err(),
            "should reject fragments in cdn_origin"
        );
    }

    #[test]
    fn accepts_valid_cdn_origin() {
        let cfg = SourcepointConfig {
            enabled: true,
            rewrite_sdk: true,
            cdn_origin: "https://cdn.privacy-mgmt.com".to_string(),
            auth_cookie_name: None,
            cache_ttl_seconds: default_cache_ttl(),
            debug_force_banner_visible: false,
            debug_clear_state_on_load: false,
        };
        assert!(
            cfg.validate().is_ok(),
            "should accept cdn_origin on cdn.privacy-mgmt.com"
        );
    }

    #[test]
    fn accepts_http_cdn_origin() {
        let cfg = SourcepointConfig {
            enabled: true,
            rewrite_sdk: true,
            cdn_origin: "http://cdn.privacy-mgmt.com".to_string(),
            auth_cookie_name: None,
            cache_ttl_seconds: default_cache_ttl(),
            debug_force_banner_visible: false,
            debug_clear_state_on_load: false,
        };
        assert!(
            cfg.validate().is_ok(),
            "should accept http scheme for cdn_origin"
        );
    }

    #[test]
    fn accepts_valid_auth_cookie_names() {
        for auth_cookie_name in ["sp_auth", "sp-auth_01"] {
            let cfg = SourcepointConfig {
                enabled: true,
                rewrite_sdk: true,
                cdn_origin: default_cdn_origin(),
                auth_cookie_name: Some(auth_cookie_name.to_string()),
                cache_ttl_seconds: default_cache_ttl(),
                debug_force_banner_visible: false,
                debug_clear_state_on_load: false,
            };

            assert!(
                cfg.validate().is_ok(),
                "should accept valid auth_cookie_name: {auth_cookie_name}"
            );
        }
    }

    #[test]
    fn rejects_invalid_auth_cookie_names() {
        for auth_cookie_name in [
            "",
            "   ",
            " sp_auth",
            "sp_auth ",
            "sp;auth",
            "sp=auth",
            "sp.auth",
            "Domain",
            "path",
            "SameSite",
            "max-age",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let cfg = SourcepointConfig {
                enabled: true,
                rewrite_sdk: true,
                cdn_origin: default_cdn_origin(),
                auth_cookie_name: Some(auth_cookie_name.to_string()),
                cache_ttl_seconds: default_cache_ttl(),
                debug_force_banner_visible: false,
                debug_clear_state_on_load: false,
            };

            assert!(
                cfg.validate().is_err(),
                "should reject invalid auth_cookie_name: {auth_cookie_name:?}"
            );
        }
    }

    #[test]
    fn parses_bare_sourcepoint_host_references() {
        let parsed = parse_sourcepoint_url("cdn.privacy-mgmt.com/wrapper.js")
            .expect("should parse bare Sourcepoint host reference");

        assert_eq!(parsed.host_str(), Some(SOURCEPOINT_CDN_HOST));
        assert_eq!(parsed.path(), "/wrapper.js");
    }

    #[test]
    fn rejects_bare_host_prefix_spoofing() {
        assert_eq!(
            parse_sourcepoint_url("cdn.privacy-mgmt.com.evil.com/wrapper.js"),
            None,
            "should reject bare host strings that only prefix-match Sourcepoint"
        );
    }

    fn make_req(method: Method, url: &str) -> Request<EdgeBody> {
        http::Request::builder()
            .method(method)
            .uri(url)
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    fn make_resp_with_status(status: StatusCode) -> Response<EdgeBody> {
        http::Response::builder()
            .status(status)
            .body(EdgeBody::empty())
            .expect("should build test response")
    }

    fn get_header_str(
        resp: &Response<EdgeBody>,
        name: impl http::header::AsHeaderName,
    ) -> Option<&str> {
        resp.headers().get(name).and_then(|v| v.to_str().ok())
    }

    fn get_req_header_str(
        req: &Request<EdgeBody>,
        name: impl http::header::AsHeaderName,
    ) -> Option<&str> {
        req.headers().get(name).and_then(|v| v.to_str().ok())
    }

    fn set_header(
        resp: &mut Response<EdgeBody>,
        name: impl http::header::IntoHeaderName,
        value: &str,
    ) {
        resp.headers_mut().insert(
            name,
            HeaderValue::from_str(value).expect("should build header value"),
        );
    }

    fn set_req_header(
        req: &mut Request<EdgeBody>,
        name: impl http::header::IntoHeaderName,
        value: &str,
    ) {
        req.headers_mut().insert(
            name,
            HeaderValue::from_str(value).expect("should build header value"),
        );
    }

    fn take_body_bytes(resp: Response<EdgeBody>) -> Vec<u8> {
        match resp.into_body() {
            EdgeBody::Once(b) => b.to_vec(),
            EdgeBody::Stream(_) => vec![],
        }
    }

    #[test]
    fn copy_headers_sets_x_forwarded_for_from_runtime_client_ip() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let original_req = make_req(Method::GET, "https://publisher.example.com/sourcepoint");
        let mut proxy_req = make_req(Method::GET, "https://cdn.privacy-mgmt.com/wrapper.js");
        let client_ip = "203.0.113.10".parse().expect("should parse test IP");

        let forwarded_cookies =
            integration.copy_headers(Some(client_ip), &original_req, &mut proxy_req);

        assert!(
            !forwarded_cookies,
            "should report no forwarded cookies when request has none"
        );
        assert_eq!(
            get_req_header_str(&proxy_req, "x-forwarded-for"),
            Some("203.0.113.10"),
            "should forward platform-provided client IP"
        );
    }

    #[test]
    fn copy_headers_forwards_preflight_headers() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut original_req =
            make_req(Method::OPTIONS, "https://publisher.example.com/sourcepoint");
        set_req_header(&mut original_req, "access-control-request-method", "POST");
        set_req_header(
            &mut original_req,
            "access-control-request-headers",
            "Content-Type, X-Test",
        );
        let mut proxy_req = make_req(Method::OPTIONS, "https://cdn.privacy-mgmt.com/wrapper.js");

        integration.copy_headers(None, &original_req, &mut proxy_req);

        assert_eq!(
            get_req_header_str(&proxy_req, "access-control-request-method"),
            Some("POST"),
            "should forward requested preflight method"
        );
        assert_eq!(
            get_req_header_str(&proxy_req, "access-control-request-headers"),
            Some("Content-Type, X-Test"),
            "should forward requested preflight headers"
        );
    }

    #[test]
    fn forwards_only_allowlisted_sourcepoint_cookies() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut req = make_req(Method::GET, "https://publisher.example.com/sourcepoint");
        set_req_header(
            &mut req,
            header::COOKIE,
            "consentUUID=uuid123; session_id=secret; euconsent-v2=tcf; _sp_su=1; theme=dark",
        );

        assert_eq!(
            integration
                .filtered_sourcepoint_cookie_header(&req)
                .as_deref(),
            Some("consentUUID=uuid123; euconsent-v2=tcf; _sp_su=1"),
            "should forward only Sourcepoint cookie names"
        );
    }

    #[test]
    fn forwards_configured_auth_cookie_name() {
        let mut cfg = config(true);
        cfg.auth_cookie_name = Some("sp_auth".to_string());
        let integration = SourcepointIntegration::new(Arc::new(cfg));
        let mut req = make_req(Method::GET, "https://publisher.example.com/sourcepoint");
        set_req_header(
            &mut req,
            header::COOKIE,
            "sp_auth=token123; session_id=secret; consentUUID=uuid123",
        );

        assert_eq!(
            integration
                .filtered_sourcepoint_cookie_header(&req)
                .as_deref(),
            Some("sp_auth=token123; consentUUID=uuid123"),
            "should forward configured Sourcepoint auth cookie alongside built-in cookies"
        );
    }

    #[test]
    fn drops_unrelated_publisher_cookies_from_upstream_request() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut req = make_req(Method::GET, "https://publisher.example.com/sourcepoint");
        set_req_header(&mut req, header::COOKIE, "session_id=secret; theme=dark");

        assert_eq!(
            integration.filtered_sourcepoint_cookie_header(&req),
            None,
            "should omit upstream Cookie header when no Sourcepoint cookies are present"
        );
    }

    #[test]
    fn apply_cache_headers_uses_private_no_store_for_cookie_setting_responses() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut response = make_resp_with_status(StatusCode::OK);
        set_header(
            &mut response,
            header::SET_COOKIE,
            "consentUUID=uuid123; Path=/",
        );
        set_header(&mut response, header::CACHE_CONTROL, "public, max-age=3600");

        integration.apply_cache_headers(&mut response, false);

        assert_eq!(
            get_header_str(&response, header::CACHE_CONTROL),
            Some("private, no-store"),
            "should prevent public caching for cookie-setting responses"
        );
    }

    #[test]
    fn apply_cache_headers_uses_private_policy_when_cookies_were_forwarded() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut response = make_resp_with_status(StatusCode::OK);

        integration.apply_cache_headers(&mut response, true);

        assert_eq!(
            get_header_str(&response, header::CACHE_CONTROL),
            Some("private, max-age=0"),
            "should not publicly cache responses that may vary by forwarded Cookie"
        );
    }

    #[test]
    fn apply_cache_headers_uses_public_default_without_forwarded_cookies() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut response = make_resp_with_status(StatusCode::OK);

        integration.apply_cache_headers(&mut response, false);

        let expected_cache_control = format!("public, max-age={}", default_cache_ttl());
        assert_eq!(
            get_header_str(&response, header::CACHE_CONTROL),
            Some(expected_cache_control.as_str()),
            "should keep public default caching for non-personalized responses"
        );
    }

    #[test]
    fn rewrite_javascript_response_preserves_headers() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut response = make_resp_with_status(StatusCode::OK);

        set_header(&mut response, header::VARY, "Accept-Encoding, Origin");
        set_header(
            &mut response,
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            "https://example.com",
        );
        set_header(&mut response, header::CONTENT_ENCODING, "gzip");
        set_header(&mut response, header::CONTENT_LENGTH, "4");
        set_header(&mut response, header::CACHE_CONTROL, "no-store");
        *response.body_mut() = EdgeBody::from(b"payload".to_vec());

        integration.rewrite_javascript_response(&mut response, "rewritten".to_string());

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            get_header_str(&response, header::CONTENT_TYPE),
            Some("application/javascript; charset=utf-8")
        );
        let expected_cache_control = format!("public, max-age={}", default_cache_ttl());
        assert_eq!(
            get_header_str(&response, header::CACHE_CONTROL),
            Some(expected_cache_control.as_str())
        );
        assert_eq!(get_header_str(&response, header::VARY), Some("Origin"));
        assert_eq!(
            get_header_str(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some("https://example.com")
        );
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
        assert!(response.headers().get(header::CONTENT_LENGTH).is_none());

        let body = take_body_bytes(response);
        assert_eq!(
            String::from_utf8(body).expect("should decode rewritten JavaScript response"),
            "rewritten"
        );
    }

    #[test]
    fn rewrite_javascript_response_uses_private_no_store_for_cookie_setting_responses() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut response = make_resp_with_status(StatusCode::OK);
        set_header(
            &mut response,
            header::SET_COOKIE,
            "consentUUID=uuid123; Path=/",
        );
        set_header(&mut response, header::CACHE_CONTROL, "public, max-age=3600");
        *response.body_mut() = EdgeBody::from(b"payload".to_vec());

        integration.rewrite_javascript_response(&mut response, "rewritten".to_string());

        assert_eq!(
            get_header_str(&response, header::CACHE_CONTROL),
            Some("private, no-store"),
            "should avoid public caching when rewritten response still sets cookies"
        );
        assert_eq!(
            get_header_str(&response, header::CONTENT_TYPE),
            Some("application/javascript; charset=utf-8")
        );
    }

    #[test]
    fn rewrite_html_response_sets_html_headers_and_body() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut response = make_resp_with_status(StatusCode::OK);

        set_header(&mut response, header::VARY, "Accept-Encoding, Origin");
        set_header(&mut response, header::CONTENT_ENCODING, "gzip");
        set_header(&mut response, header::CONTENT_LENGTH, "4");
        *response.body_mut() = EdgeBody::from(b"payload".to_vec());

        integration.rewrite_html_response(&mut response, "<html></html>".to_string(), false);

        assert_eq!(
            get_header_str(&response, header::CONTENT_TYPE),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(get_header_str(&response, header::VARY), Some("Origin"));
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
        assert!(response.headers().get(header::CONTENT_LENGTH).is_none());

        let expected_cache_control = format!("public, max-age={}", default_cache_ttl());
        assert_eq!(
            get_header_str(&response, header::CACHE_CONTROL),
            Some(expected_cache_control.as_str())
        );

        let body = take_body_bytes(response);
        assert_eq!(
            String::from_utf8(body).expect("should decode rewritten HTML response"),
            "<html></html>"
        );
    }

    #[test]
    fn rewrite_javascript_response_removes_exact_accept_encoding_vary() {
        let integration = SourcepointIntegration::new(Arc::new(config(true)));
        let mut response = make_resp_with_status(StatusCode::OK);
        set_header(&mut response, header::VARY, "Accept-Encoding");
        *response.body_mut() = EdgeBody::from(b"payload".to_vec());

        integration.rewrite_javascript_response(&mut response, "rewritten".to_string());

        assert!(
            response.headers().get(header::VARY).is_none(),
            "should remove stale Vary: Accept-Encoding after stripping content encoding"
        );
    }

    #[test]
    fn rewrites_single_quoted_origin_plus_unified_pattern() {
        let input = r#"return t.origin+'/unified/4.40.1/'}"#;
        let output = SourcepointIntegration::rewrite_script_content(input);

        assert_eq!(
            output, r#"return t.origin+'/integrations/sourcepoint/cdn/unified/4.40.1/'}"#,
            "should rewrite single-quoted unified path"
        );
    }

    #[test]
    fn rewrites_absolute_redirect_location() {
        let result = SourcepointIntegration::rewrite_redirect_location(
            "https://cdn.privacy-mgmt.com/consent/tcfv2?foo=bar",
            "https://cdn.privacy-mgmt.com/original",
        );
        assert_eq!(
            result.as_deref(),
            Some("/integrations/sourcepoint/cdn/consent/tcfv2?foo=bar"),
            "should rewrite absolute CDN redirect"
        );
    }

    #[test]
    fn rewrites_protocol_relative_redirect_location() {
        let result = SourcepointIntegration::rewrite_redirect_location(
            "//cdn.privacy-mgmt.com/consent/tcfv2",
            "https://cdn.privacy-mgmt.com/original",
        );
        assert_eq!(
            result.as_deref(),
            Some("/integrations/sourcepoint/cdn/consent/tcfv2"),
            "should rewrite protocol-relative CDN redirect"
        );
    }

    #[test]
    fn preserves_redirect_fragment_when_rewriting_location() {
        let result = SourcepointIntegration::rewrite_redirect_location(
            "https://cdn.privacy-mgmt.com/consent/tcfv2#hash",
            "https://cdn.privacy-mgmt.com/original",
        );
        assert_eq!(
            result.as_deref(),
            Some("/integrations/sourcepoint/cdn/consent/tcfv2#hash"),
            "should preserve fragment when rewriting redirect"
        );
    }

    #[test]
    fn ignores_redirect_to_other_host() {
        let result = SourcepointIntegration::rewrite_redirect_location(
            "https://example.com/other",
            "https://cdn.privacy-mgmt.com/original",
        );
        assert_eq!(result, None, "should not rewrite redirect to non-CDN host");
    }

    #[test]
    fn rewrites_relative_redirect_location() {
        let result = SourcepointIntegration::rewrite_redirect_location(
            "/consent/tcfv2/new-path",
            "https://cdn.privacy-mgmt.com/original",
        );
        assert_eq!(
            result.as_deref(),
            Some("/integrations/sourcepoint/cdn/consent/tcfv2/new-path"),
            "should rewrite relative redirect resolved against CDN base"
        );
    }
}
