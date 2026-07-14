use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{
    engine::general_purpose::{
        STANDARD as BASE64_STANDARD, STANDARD_NO_PAD as BASE64_STANDARD_NO_PAD,
    },
    Engine as _,
};
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::HeaderValue;
use http::{header, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use url::{Url, Url as ParsedUrl};
use validator::{Validate, ValidationError};

use crate::auction::orchestrator::ERROR_TYPE_HTTP_STATUS;
use crate::auction::provider::AuctionProvider;
use crate::auction::types::{
    AuctionContext, AuctionRequest, AuctionResponse, Bid as AuctionBid, MediaType,
};
use crate::consent_config::ConsentForwardingMode;
use crate::cookies::{strip_cookies, CONSENT_COOKIE_NAMES};
use crate::error::TrustedServerError;
use crate::http_util::RequestInfo;
use crate::integrations::{
    collect_response_bounded, ensure_integration_backend_with_timeout,
    predict_integration_backend_name, AttributeRewriteAction, IntegrationAttributeContext,
    IntegrationAttributeRewriter, IntegrationEndpoint, IntegrationHeadInjector,
    IntegrationHtmlContext, IntegrationProxy, IntegrationRegistration,
    UPSTREAM_RTB_MAX_RESPONSE_BYTES,
};
use crate::openrtb::{
    to_openrtb_i32, Banner, ConsentedProvidersSettings, Device, Format, Geo, Imp, ImpExt,
    ImpStoredRequest, OpenRtbRequest, PrebidExt, PrebidImpExt, Publisher, Regs, RegsExt,
    RequestExt, Site, ToExt, TrustedServerExt, User, UserExt,
};
use crate::platform::{
    PlatformHttpRequest, PlatformPendingRequest, PlatformResponse, RuntimeServices,
};
use crate::proxy::{is_host_allowed, proxy_request, ProxyRequestConfig};
use crate::request_signing::{RequestSigner, SigningParams, SIGNING_VERSION};
use crate::settings::{IntegrationConfig, Settings};

const PREBID_INTEGRATION_ID: &str = "prebid";
const PREBID_BUNDLE_ROUTE: &str = "/integrations/prebid/bundle.js";
const PREBID_BUNDLE_CONTENT_TYPE: &str = "application/javascript; charset=utf-8";
const PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const PREBID_BUNDLE_REVALIDATION_CACHE_CONTROL: &str =
    "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400";
const PREBID_BUNDLE_ERROR_CACHE_CONTROL: &str = "no-store";
const PREBID_BUNDLE_ERROR_CONTENT_TYPE: &str = "text/plain; charset=utf-8";
const PREBID_BUNDLE_NOSNIFF_HEADER: &str = "x-content-type-options";
const PREBID_BUNDLE_NOSNIFF_VALUE: &str = "nosniff";
const TRUSTED_SERVER_BIDDER: &str = "trustedServer";
const BIDDER_PARAMS_KEY: &str = "bidderParams";
const ZONE_KEY: &str = "zone";

/// Default currency for `OpenRTB` bid floors and responses.
const DEFAULT_CURRENCY: &str = "USD";

const PREBID_PUBLIC_ERROR_MESSAGE_CHARS: usize = 500;
const PREBID_ERROR_BODY_PREVIEW_CHARS: usize = 1000;
const PREBID_ERROR_BODY_PREVIEW_BYTES: usize = PREBID_ERROR_BODY_PREVIEW_CHARS * 4;
const PREBID_ERROR_JSON_MAX_DEPTH: usize = 6;
const PREBID_ERROR_JSON_KEYS: [&str; 6] =
    ["message", "error", "errors", "detail", "title", "reason"];

#[derive(Debug, Eq, PartialEq)]
struct BoundedPrebidErrorText {
    text: String,
    truncated: bool,
}

fn bounded_prebid_error_text(value: &str, max_chars: usize) -> Option<BoundedPrebidErrorText> {
    let mut text = String::new();
    let mut char_count = 0;
    let mut pending_space = false;
    let mut truncated = false;

    for character in value.chars() {
        if character.is_whitespace() || character.is_control() {
            pending_space = !text.is_empty();
            continue;
        }

        if pending_space {
            if char_count == max_chars {
                truncated = true;
                break;
            }
            text.push(' ');
            char_count += 1;
            pending_space = false;
        }

        if char_count == max_chars {
            truncated = true;
            break;
        }
        text.push(character);
        char_count += 1;
    }

    (!text.is_empty()).then_some(BoundedPrebidErrorText { text, truncated })
}

fn prebid_body_preview(body: &[u8]) -> Option<BoundedPrebidErrorText> {
    let bounded_body = &body[..body.len().min(PREBID_ERROR_BODY_PREVIEW_BYTES)];
    let mut preview = bounded_prebid_error_text(
        &String::from_utf8_lossy(bounded_body),
        PREBID_ERROR_BODY_PREVIEW_CHARS,
    )?;
    preview.truncated |= body.len() > bounded_body.len();
    Some(preview)
}

fn nested_prebid_json_error_message(
    value: &Json,
    depth: usize,
    allow_direct_string: bool,
) -> Option<&str> {
    if depth > PREBID_ERROR_JSON_MAX_DEPTH {
        return None;
    }

    match value {
        Json::String(message) if allow_direct_string => {
            (!message.trim().is_empty()).then_some(message.as_str())
        }
        Json::Array(values) => values.iter().find_map(|value| {
            nested_prebid_json_error_message(value, depth + 1, allow_direct_string)
        }),
        Json::Object(values) => PREBID_ERROR_JSON_KEYS
            .iter()
            .find_map(|key| {
                values
                    .get(*key)
                    .and_then(|value| nested_prebid_json_error_message(value, depth + 1, true))
            })
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| nested_prebid_json_error_message(value, depth + 1, false))
            }),
        _ => None,
    }
}

fn prebid_json_error_message(value: &Json) -> Option<&str> {
    let Json::Object(values) = value else {
        return None;
    };

    PREBID_ERROR_JSON_KEYS.iter().find_map(|key| {
        values
            .get(*key)
            .and_then(|value| nested_prebid_json_error_message(value, 0, true))
    })
}

fn is_plain_text_content_type(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/plain"))
    })
}

fn extract_prebid_error_message(
    body: &[u8],
    content_type: Option<&str>,
) -> Option<BoundedPrebidErrorText> {
    let candidate = match serde_json::from_slice::<Json>(body) {
        Ok(value) => prebid_json_error_message(&value)?.to_owned(),
        Err(_) if is_plain_text_content_type(content_type) => {
            std::str::from_utf8(body).ok()?.to_owned()
        }
        Err(_) => return None,
    };

    // Do not expose an HTML error page even if an intermediary labels it as text/plain.
    if candidate.trim_start().starts_with('<') {
        return None;
    }

    bounded_prebid_error_text(&candidate, PREBID_PUBLIC_ERROR_MESSAGE_CHARS)
}

/// CCPA/US-privacy string sent when the `Sec-GPC` header signals opt-out.
///
/// Encodes: version `1`, notice given (`Y`), user opted out (`Y`), LSPA not
/// signed (`N`). The opt-out (position 2 = `Y`) matches GPC intent. Position 3
/// (`N` = LSPA not applicable) is a conservative default that may not hold for
/// all publishers — consider making this configurable per-publisher in the future.
#[cfg(test)]
const GPC_US_PRIVACY: &str = "1YYN";

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct PrebidIntegrationConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[validate(url)]
    pub server_url: String,
    /// Prebid Server account ID, injected into the client-side bundle via
    /// `window.__tsjs_prebid.accountId` so publishers don't need to configure
    /// it in JavaScript.
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    #[serde(
        default = "default_bidders",
        deserialize_with = "crate::settings::vec_from_seq_or_map"
    )]
    pub bidders: Vec<String>,
    #[serde(default)]
    pub debug: bool,
    /// Sets the `OpenRTB` `test: 1` flag on outgoing requests. When enabled,
    /// bidders treat the auction as non-billable test traffic, which can
    /// significantly reduce fill rates. Separate from `debug` so you can get
    /// debug diagnostics without suppressing real demand.
    #[serde(default)]
    pub test_mode: bool,
    #[serde(default)]
    pub debug_query_params: Option<String>,
    /// Patterns to match Prebid script URLs for serving empty JS.
    /// Supports suffix matching (e.g., "/prebid.min.js" matches any path ending with that)
    /// and wildcard patterns (e.g., "/static/prebid/*" matches paths under that prefix).
    #[serde(
        default = "default_script_patterns",
        deserialize_with = "crate::settings::vec_from_seq_or_map"
    )]
    pub script_patterns: Vec<String>,
    /// Absolute HTTPS URL of the generated external Prebid bundle.
    #[serde(default)]
    #[validate(custom(function = "validate_external_bundle_url"))]
    pub external_bundle_url: Option<String>,
    /// Optional hex SHA-256 of the exact external bundle bytes.
    #[serde(default)]
    #[validate(custom(function = "validate_external_bundle_sha256"))]
    pub external_bundle_sha256: Option<String>,
    /// Optional browser Subresource Integrity value for the first-party script.
    #[serde(default)]
    #[validate(custom(function = "validate_external_bundle_sri"))]
    pub external_bundle_sri: Option<String>,
    /// Bidders that should run client-side in the browser via native Prebid.js
    /// adapters instead of being routed through the server-side auction.
    ///
    /// These bidders are **not** absorbed into the `trustedServer` adapter and
    /// remain as standalone bids in each ad unit.  The corresponding Prebid.js
    /// adapter modules must be statically imported in the JS bundle so they are
    /// available at runtime.
    ///
    /// This list is independent of [`bidders`](Self::bidders) — the operator
    /// manages both lists explicitly.
    #[serde(default, deserialize_with = "crate::settings::vec_from_seq_or_map")]
    pub client_side_bidders: Vec<String>,
    /// Compatibility sugar for per-bidder, per-zone param overrides.
    ///
    /// This preserves the natural `bidder -> zone -> params` config shape for
    /// the existing zone-based use case, but it is normalized into the
    /// canonical [`bid_param_override_rules`](Self::bid_param_override_rules)
    /// engine before runtime use.
    ///
    /// Example in TOML:
    /// ```toml
    /// [integrations.prebid.bid_param_zone_overrides.kargo]
    /// header       = {placementId = "_s2sHeaderId"}
    /// in_content   = {placementId = "_s2sContentId"}
    /// fixed_bottom = {placementId = "_s2sBottomId"}
    /// ```
    #[serde(default)]
    pub bid_param_zone_overrides: HashMap<String, HashMap<String, serde_json::Map<String, Json>>>,
    /// Compatibility sugar for static per-bidder parameter overrides.
    ///
    /// These rules are normalized into the canonical
    /// [`bid_param_override_rules`](Self::bid_param_override_rules) engine and
    /// therefore share the same validation and precedence behavior as explicit
    /// rules.
    ///
    /// Example in TOML:
    /// ```toml
    /// [integrations.prebid.bid_param_overrides.bidder-name]
    /// param1 = 12345
    /// param2 = "value"
    /// ```
    ///
    /// Example via environment variable:
    /// ```text
    /// TRUSTED_SERVER__INTEGRATIONS__PREBID__BID_PARAM_OVERRIDES='{"bidder-name":{"param1":12345,"param2":"value"}}'
    /// ```
    #[serde(default)]
    pub bid_param_overrides: HashMap<String, serde_json::Map<String, Json>>,
    /// Canonical ordered bidder-param override rules.
    ///
    /// Each rule has structured `when` matchers and a non-empty `set` object
    /// that is shallow-merged into the bidder params when every matcher
    /// matches. Compatibility fields such as [`bid_param_overrides`](Self::bid_param_overrides)
    /// and [`bid_param_zone_overrides`](Self::bid_param_zone_overrides) are
    /// normalized into the same runtime rule engine before request handling.
    ///
    /// Example in TOML:
    /// ```toml
    /// [[integrations.prebid.bid_param_override_rules]]
    /// when.bidder = "kargo"
    /// when.zone = "header"
    /// set = { placementId = "_abc" }
    /// ```
    ///
    /// Example via environment variable:
    /// ```text
    /// TRUSTED_SERVER__INTEGRATIONS__PREBID__BID_PARAM_OVERRIDE_RULES='[{"when":{"bidder":"kargo","zone":"header"},"set":{"placementId":"_abc"}}]'
    /// ```
    #[serde(default)]
    pub bid_param_override_rules: Vec<BidParamOverrideRule>,
    /// How consent signals are forwarded to Prebid Server.
    ///
    /// - `openrtb_only` — consent in `OpenRTB` body only, consent cookies stripped
    /// - `cookies_only` — consent cookies forwarded, body consent fields omitted
    /// - `both` — consent in both cookies and body (default)
    #[serde(default)]
    pub consent_forwarding: ConsentForwardingMode,
    /// Strip `nurl` and `burl` from PBS bids before they reach `window.tsjs.bids`.
    ///
    /// Set to `true` when the PBS deployment is configured to fire win/billing
    /// notifications server-side (e.g. `ext.prebid.events.enabled`), so the
    /// client does not double-fire them via `sendBeacon`. Default: `false`.
    #[serde(default)]
    pub suppress_nurl: bool,
    /// Bidder seats whose `nurl` and `burl` should be stripped before they reach
    /// `window.tsjs.bids`.
    ///
    /// Use this when only specific PBS seats fire win/billing notifications
    /// internally. The global [`suppress_nurl`](Self::suppress_nurl) switch still
    /// suppresses every bidder when set.
    #[serde(default, deserialize_with = "crate::settings::vec_from_seq_or_map")]
    pub suppress_nurl_bidders: Vec<String>,
}

impl IntegrationConfig for PrebidIntegrationConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Validate enabled Prebid config using the same startup-only checks as runtime registration.
///
/// # Errors
///
/// Returns a configuration error if enabled Prebid settings fail typed parsing,
/// schema validation, or bidder-param override compilation.
pub fn validate_config_for_startup(
    settings: &Settings,
) -> Result<Option<PrebidIntegrationConfig>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<PrebidIntegrationConfig>(PREBID_INTEGRATION_ID)?
    else {
        return Ok(None);
    };
    BidParamOverrideEngine::try_from_config(&config)?;
    validate_external_bundle_config(&config, &settings.proxy.allowed_domains)?;
    Ok(Some(config))
}

/// Canonical bidder-param override rule.
///
/// A rule matches against the request-time facts in [`BidParamOverrideWhen`]
/// and shallow-merges [`set`](Self::set) into the bidder params when all
/// populated matchers are equal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BidParamOverrideRule {
    /// Structured exact-match conditions for this rule.
    pub when: BidParamOverrideWhen,
    /// Parameters shallow-merged into bidder params when the rule matches.
    /// Top-level keys in this object are inserted or replaced; nested objects
    /// are replaced wholesale rather than recursed into.
    pub set: serde_json::Map<String, Json>,
}

/// Structured exact-match conditions for a [`BidParamOverrideRule`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BidParamOverrideWhen {
    /// Bidder name matcher.
    #[serde(default)]
    pub bidder: Option<String>,
    /// Zone matcher from `mediaTypes.banner.name` propagated via
    /// `trustedServer.zone`.
    #[serde(default)]
    pub zone: Option<String>,
}

fn default_timeout_ms() -> u32 {
    1000
}

fn default_bidders() -> Vec<String> {
    vec!["mocktioneer".to_string()]
}

fn default_enabled() -> bool {
    true
}

/// Default suffixes that identify Prebid scripts
const PREBID_SCRIPT_SUFFIXES: &[&str] = &[
    "/prebid.js",
    "/prebid.min.js",
    "/prebidjs.js",
    "/prebidjs.min.js",
];

fn default_script_patterns() -> Vec<String> {
    PREBID_SCRIPT_SUFFIXES
        .iter()
        .map(|&s| s.to_owned())
        .collect()
}

fn validate_external_bundle_url(value: &str) -> Result<(), ValidationError> {
    let url = Url::parse(value).map_err(|_| {
        let mut err = ValidationError::new("invalid_external_bundle_url");
        err.message = Some("external_bundle_url must be a valid absolute URL".into());
        err
    })?;

    if url.scheme() != "https" {
        let mut err = ValidationError::new("invalid_external_bundle_scheme");
        err.message = Some("external_bundle_url must use https".into());
        return Err(err);
    }

    if url.host_str().is_none() {
        let mut err = ValidationError::new("missing_external_bundle_host");
        err.message = Some("external_bundle_url must include a host".into());
        return Err(err);
    }

    Ok(())
}

fn validate_external_bundle_sha256(value: &str) -> Result<(), ValidationError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }

    let mut err = ValidationError::new("invalid_external_bundle_sha256");
    err.message = Some("external_bundle_sha256 must be a 64-character hex SHA-256".into());
    Err(err)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ExternalBundleSriAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

impl ExternalBundleSriAlgorithm {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "sha256" => Some(Self::Sha256),
            "sha384" => Some(Self::Sha384),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    fn expected_digest_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }
}

fn external_bundle_sri_validation_error(message: &'static str) -> ValidationError {
    let mut err = ValidationError::new("invalid_external_bundle_sri");
    err.message = Some(message.into());
    err
}

fn parse_external_bundle_sri(value: &str) -> Result<(), ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed != value {
        return Err(external_bundle_sri_validation_error(
            "external_bundle_sri must be non-empty with no surrounding whitespace",
        ));
    }

    for token in trimmed.split_ascii_whitespace() {
        let Some((algorithm_raw, digest_raw)) = token.split_once('-') else {
            return Err(external_bundle_sri_validation_error(
                "external_bundle_sri entries must use algorithm-digest format",
            ));
        };

        let Some(algorithm) = ExternalBundleSriAlgorithm::parse(algorithm_raw) else {
            return Err(external_bundle_sri_validation_error(
                "external_bundle_sri must use sha256, sha384, or sha512",
            ));
        };

        if digest_raw.is_empty() {
            return Err(external_bundle_sri_validation_error(
                "external_bundle_sri digest must be non-empty",
            ));
        }

        let digest = BASE64_STANDARD
            .decode(digest_raw)
            .or_else(|_| BASE64_STANDARD_NO_PAD.decode(digest_raw))
            .map_err(|_| {
                external_bundle_sri_validation_error("external_bundle_sri digest must be base64")
            })?;

        if digest.len() != algorithm.expected_digest_len() {
            return Err(external_bundle_sri_validation_error(
                "external_bundle_sri digest length does not match its algorithm",
            ));
        }
    }

    Ok(())
}

fn validate_external_bundle_sri(value: &str) -> Result<(), ValidationError> {
    parse_external_bundle_sri(value)
}

fn validate_external_bundle_config(
    config: &PrebidIntegrationConfig,
    allowed_domains: &[String],
) -> Result<(), Report<TrustedServerError>> {
    let url = config.external_bundle_url.as_deref().ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: "integrations.prebid.external_bundle_url is required when prebid is enabled"
                .to_string(),
        })
    })?;

    let parsed = Url::parse(url).map_err(|_| {
        Report::new(TrustedServerError::Configuration {
            message: "integrations.prebid.external_bundle_url must be a valid absolute URL"
                .to_string(),
        })
    })?;

    if parsed.scheme() != "https" {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "integrations.prebid.external_bundle_url must use https".to_string(),
        }));
    }

    let host = parsed.host_str().ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: "integrations.prebid.external_bundle_url must include a host".to_string(),
        })
    })?;

    if allowed_domains.is_empty() {
        return Err(Report::new(TrustedServerError::Configuration {
            message:
                "proxy.allowed_domains must include the external Prebid bundle host when integrations.prebid.external_bundle_url is configured"
                    .to_string(),
        }));
    }

    if !allowed_domains
        .iter()
        .any(|pattern| is_host_allowed(host, pattern))
    {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "integrations.prebid.external_bundle_url host `{host}` is not permitted by proxy.allowed_domains"
            ),
        }));
    }

    Ok(())
}

pub struct PrebidIntegration {
    config: PrebidIntegrationConfig,
    engine: Arc<BidParamOverrideEngine>,
}

impl PrebidIntegration {
    fn try_new(config: PrebidIntegrationConfig) -> Result<Arc<Self>, Report<TrustedServerError>> {
        let engine = Arc::new(BidParamOverrideEngine::try_from_config(&config)?);
        Ok(Arc::new(Self { config, engine }))
    }

    #[cfg(test)]
    fn new(config: PrebidIntegrationConfig) -> Arc<Self> {
        Self::try_new(config).expect("should compile prebid bid param overrides")
    }

    fn auction_provider(&self) -> PrebidAuctionProvider {
        PrebidAuctionProvider {
            config: self.config.clone(),
            bid_param_override_engine: Arc::clone(&self.engine),
        }
    }

    fn matches_script_url(&self, attr_value: &str) -> bool {
        let trimmed = attr_value.trim();
        let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);

        if self.matches_script_pattern(without_query) {
            return true;
        }

        if !without_query.starts_with('/')
            && !without_query.starts_with("//")
            && !without_query.contains("://")
        {
            let with_slash = format!("/{without_query}");
            if self.matches_script_pattern(&with_slash) {
                return true;
            }
        }

        let parsed = if without_query.starts_with("//") {
            ParsedUrl::parse(&format!("https:{without_query}"))
        } else {
            ParsedUrl::parse(without_query)
        };

        parsed
            .ok()
            .is_some_and(|url| self.matches_script_pattern(url.path()))
    }

    fn matches_script_pattern(&self, path: &str) -> bool {
        // Normalize path to lowercase for case-insensitive matching
        let path_lower = path.to_ascii_lowercase();

        // Check if path matches any configured pattern
        for pattern in &self.config.script_patterns {
            let pattern_lower = pattern.to_ascii_lowercase();

            // Check for wildcard patterns: /* or {*name}
            if pattern_lower.ends_with("/*") || pattern_lower.contains("{*") {
                // Extract prefix before the wildcard
                let prefix = if pattern_lower.ends_with("/*") {
                    &pattern_lower[..pattern_lower.len() - 1] // Remove trailing *
                } else {
                    // Find {* and extract prefix before it
                    pattern_lower.split("{*").next().unwrap_or("")
                };

                if path_lower.starts_with(prefix) {
                    // Check if it ends with a known Prebid script suffix
                    if PREBID_SCRIPT_SUFFIXES
                        .iter()
                        .any(|suffix| path_lower.ends_with(suffix))
                    {
                        return true;
                    }
                }
            } else {
                // Exact match or suffix match
                if path_lower.ends_with(&pattern_lower) {
                    return true;
                }
            }
        }
        false
    }

    fn handle_script_handler(
        &self,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let body = "// Script overridden by Trusted Server\n";

        http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, PREBID_BUNDLE_CONTENT_TYPE)
            .header(header::CACHE_CONTROL, "public, max-age=31536000")
            .body(EdgeBody::from(body))
            .change_context(TrustedServerError::Prebid {
                message: "Failed to build Prebid script handler response".to_string(),
            })
    }

    fn external_bundle_script_src(&self) -> String {
        match self.config.external_bundle_sha256.as_deref() {
            Some(sha256) => format!("{PREBID_BUNDLE_ROUTE}?v={sha256}"),
            None => PREBID_BUNDLE_ROUTE.to_string(),
        }
    }

    fn external_bundle_script_tag(&self) -> String {
        let src = self.external_bundle_script_src();
        let integrity = self
            .config
            .external_bundle_sri
            .as_deref()
            .map(|value| format!(" integrity=\"{}\"", escape_html_attr(value)))
            .unwrap_or_default();

        format!("<script src=\"{src}\"{integrity} defer></script>")
    }

    fn is_managed_external(&self) -> bool {
        self.config.external_bundle_url.is_some()
    }

    fn external_bundle_request_cache_mode(
        &self,
        req: &http::Request<EdgeBody>,
    ) -> Result<Option<ExternalBundleCacheMode>, Report<TrustedServerError>> {
        let versions = req
            .uri()
            .query()
            .map(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .filter(|(key, _)| key == "v")
                    .map(|(_, value)| value.into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if versions.len() > 1 {
            return Ok(None);
        }

        let requested_version = versions.first().map(String::as_str);
        match (
            self.config.external_bundle_sha256.as_deref(),
            requested_version,
        ) {
            (None, Some(_)) => Ok(None),
            (Some(expected), Some(actual)) if expected != actual => Ok(None),
            (Some(_), Some(_)) => Ok(Some(ExternalBundleCacheMode::Immutable)),
            _ => Ok(Some(ExternalBundleCacheMode::Revalidate)),
        }
    }

    fn apply_external_bundle_headers(
        &self,
        response: &mut http::Response<EdgeBody>,
        mode: ExternalBundleCacheMode,
    ) {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PREBID_BUNDLE_CONTENT_TYPE),
        );
        response.headers_mut().insert(
            header::HeaderName::from_static(PREBID_BUNDLE_NOSNIFF_HEADER),
            HeaderValue::from_static(PREBID_BUNDLE_NOSNIFF_VALUE),
        );

        match mode {
            ExternalBundleCacheMode::Immutable => {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static(PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL),
                );
                if let Some(sha256) = self.config.external_bundle_sha256.as_deref() {
                    response.headers_mut().insert(
                        header::ETAG,
                        HeaderValue::from_str(&format!("\"sha256:{sha256}\""))
                            .expect("should build etag header"),
                    );
                }
            }
            ExternalBundleCacheMode::Revalidate => {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static(PREBID_BUNDLE_REVALIDATION_CACHE_CONTROL),
                );
                if let Some(sha256) = self.config.external_bundle_sha256.as_deref() {
                    response.headers_mut().insert(
                        header::ETAG,
                        HeaderValue::from_str(&format!("\"sha256:{sha256}\""))
                            .expect("should build etag header"),
                    );
                }
            }
        }
    }

    fn sanitize_external_bundle_response(
        &self,
        response: http::Response<EdgeBody>,
        mode: ExternalBundleCacheMode,
    ) -> http::Response<EdgeBody> {
        let status = response.status();
        let content_encoding = response.headers().get(header::CONTENT_ENCODING).cloned();
        let body = response.into_body();

        let mut sanitized = http::Response::builder()
            .status(status)
            .body(body)
            .expect("should build sanitized response");

        if let Some(content_encoding) = content_encoding {
            sanitized
                .headers_mut()
                .insert(header::CONTENT_ENCODING, content_encoding);
        }

        if status == StatusCode::OK {
            self.apply_external_bundle_headers(&mut sanitized, mode);
        } else {
            sanitized.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(PREBID_BUNDLE_ERROR_CONTENT_TYPE),
            );
            sanitized.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(PREBID_BUNDLE_ERROR_CACHE_CONTROL),
            );
            sanitized.headers_mut().insert(
                header::HeaderName::from_static(PREBID_BUNDLE_NOSNIFF_HEADER),
                HeaderValue::from_static(PREBID_BUNDLE_NOSNIFF_VALUE),
            );
        }

        sanitized
    }

    async fn handle_external_bundle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let Some(cache_mode) = self.external_bundle_request_cache_mode(&req)? else {
            return Ok(http::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(EdgeBody::from("Not Found"))
                .expect("should build not found response"));
        };

        let target_url = self.config.external_bundle_url.as_deref().ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message:
                    "integrations.prebid.external_bundle_url is required when prebid is enabled"
                        .to_string(),
            })
        })?;

        let proxy_config = ProxyRequestConfig::new(target_url)
            .without_ec_id()
            .without_forward_headers()
            .with_streaming()
            .with_allowed_domains(&settings.proxy.allowed_domains)
            .with_https_only();

        let response = proxy_request(settings, req, proxy_config, services).await?;
        Ok(self.sanitize_external_bundle_response(response, cache_mode))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ExternalBundleCacheMode {
    Immutable,
    Revalidate,
}

fn escape_html_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<PrebidIntegration>>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<PrebidIntegrationConfig>(PREBID_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    validate_external_bundle_config(&config, &settings.proxy.allowed_domains)?;

    // Warn about bidders that appear in both lists — this is likely a config
    // mistake. A bidder should be in either `bidders` (server-side) or
    // `client_side_bidders` (browser-side), not both.
    for bidder in &config.client_side_bidders {
        if config.bidders.iter().any(|b| b == bidder) {
            log::warn!(
                "prebid: bidder \"{}\" is in both bidders and client_side_bidders — \
                 it will run server-side AND be left for client-side, which is likely unintended",
                bidder
            );
        }
    }

    Ok(Some(PrebidIntegration::try_new(config)?))
}

/// Register the Prebid integration when enabled.
///
/// # Errors
///
/// Returns an error when the Prebid integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(PREBID_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration.clone())
            .with_head_injector(integration)
            .without_js()
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for PrebidIntegration {
    fn integration_name(&self) -> &'static str {
        PREBID_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        let mut routes = vec![];

        routes.push(self.get("/bundle.js"));

        // Register routes for script removal patterns
        // Patterns can be exact paths (e.g., "/prebid.min.js") or use matchit wildcards
        // (e.g., "/static/prebid/{*rest}")
        for pattern in &self.config.script_patterns {
            // Intentional leak: runs once at startup and patterns are small.
            // `IntegrationEndpoint` requires `&'static str`.
            let static_path: &'static str = Box::leak(pattern.clone().into_boxed_str());
            routes.push(IntegrationEndpoint::get(static_path));
        }

        routes
    }

    async fn handle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let path = req.uri().path().to_string();
        let method = req.method().clone();

        match method {
            Method::GET if self.is_managed_external() && path == PREBID_BUNDLE_ROUTE => {
                self.handle_external_bundle(settings, services, req).await
            }
            // Serve empty JS for matching script patterns
            Method::GET if self.matches_script_pattern(&path) => self.handle_script_handler(),
            _ => http::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(EdgeBody::from("Not Found"))
                .change_context(TrustedServerError::Prebid {
                    message: "Failed to build Prebid not found response".to_string(),
                }),
        }
    }
}

impl IntegrationAttributeRewriter for PrebidIntegration {
    fn integration_id(&self) -> &'static str {
        PREBID_INTEGRATION_ID
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
        if self.matches_script_url(attr_value) {
            AttributeRewriteAction::remove_element()
        } else {
            AttributeRewriteAction::keep()
        }
    }
}

impl IntegrationHeadInjector for PrebidIntegration {
    fn integration_id(&self) -> &'static str {
        PREBID_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct InjectedPrebidClientConfig<'a> {
            account_id: &'a str,
            timeout: u32,
            debug: bool,
            bidders: &'a [String],
            #[serde(skip_serializing_if = "<[String]>::is_empty")]
            client_side_bidders: &'a [String],
        }

        let payload = InjectedPrebidClientConfig {
            account_id: self.config.account_id.as_deref().unwrap_or_default(),
            timeout: self.config.timeout_ms,
            debug: self.config.debug,
            bidders: &self.config.bidders,
            client_side_bidders: &self.config.client_side_bidders,
        };

        // Escape `</` to prevent breaking out of the script tag.
        let config_json = serde_json::to_string(&payload)
            .unwrap_or_else(|e| {
                log::warn!("Prebid: failed to serialize client config: {e}");
                "{}".to_string()
            })
            .replace("</", "<\\/");

        let mut inserts = vec![format!(
            r#"<script>window.pbjs=window.pbjs||{{}};window.pbjs.que=window.pbjs.que||[];window.pbjs.cmd=window.pbjs.cmd||[];window.__tsjs_prebid={config_json};</script>"#
        )];

        inserts.push(self.external_bundle_script_tag());

        inserts
    }
}

fn expand_trusted_server_bidders(
    configured_bidders: &[String],
    params: &Json,
) -> HashMap<String, Json> {
    let per_bidder = params.get(BIDDER_PARAMS_KEY).and_then(Json::as_object);

    if configured_bidders.is_empty() {
        return per_bidder
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
    }

    configured_bidders
        .iter()
        .map(|bidder| {
            let value = per_bidder
                .and_then(|m| m.get(bidder).cloned())
                .unwrap_or_else(|| {
                    // No per-bidder map → use entire params as shared config
                    if per_bidder.is_some() {
                        Json::Object(Default::default())
                    } else {
                        params.clone()
                    }
                });
            (bidder.clone(), value)
        })
        .collect()
}

/// Shallow-merges `override_obj` into `params`.
///
/// When `params` is a JSON object, each top-level key in `override_obj` is
/// inserted or replaced in `params`. Nested objects are replaced wholesale —
/// they are not recursed into. When `params` is not an object, it is left
/// untouched to preserve pre-existing behavior.
///
/// Returns whether `params` was an object and the merge path ran.
fn merge_bidder_param_object(
    params: &mut Json,
    override_obj: &serde_json::Map<String, Json>,
) -> bool {
    if let Json::Object(base) = params {
        for (k, v) in override_obj {
            base.insert(k.clone(), v.clone());
        }

        true
    } else {
        false
    }
}

// ============================================================================
// Generic bid-parameter override engine
// ============================================================================

fn warn_unconfigured_bidder(config: &PrebidIntegrationConfig, bidder: &str, field: &str) {
    if !config.bidders.iter().any(|b| b == bidder) {
        if config.client_side_bidders.iter().any(|b| b == bidder) {
            log::warn!(
                "prebid: {field} entry targets client-side-only bidder \
                 '{bidder}' — server-side override will never apply"
            );
        } else {
            log::warn!(
                "prebid: {field} entry references unconfigured bidder \
                 '{bidder}' — rule will never fire"
            );
        }
    }
}

#[derive(Debug, Default, Clone)]
struct BidParamOverrideEngine {
    rules: Vec<CompiledBidParamOverrideRule>,
    // Maps bidder name to the indices (into `rules`) of rules that constrain on that bidder.
    // Rules with no bidder constraint (zone-only or catch-all) are kept in `wildcard_indices`.
    // Both slices are in declaration order; `apply` merges them without allocation.
    bidder_index: HashMap<String, Vec<usize>>,
    wildcard_indices: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledBidParamOverrideRule {
    bidder: Option<String>,
    zone: Option<String>,
    set: serde_json::Map<String, Json>,
}

#[derive(Debug, Copy, Clone)]
struct BidParamOverrideFacts<'a> {
    bidder: &'a str,
    zone: Option<&'a str>,
}

impl BidParamOverrideEngine {
    fn try_from_config(
        config: &PrebidIntegrationConfig,
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut rules = Vec::new();

        let mut bidder_overrides = config.bid_param_overrides.iter().collect::<Vec<_>>();
        bidder_overrides.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));

        for (bidder, set) in bidder_overrides {
            warn_unconfigured_bidder(config, bidder, "bid_param_overrides");
            rules.push(CompiledBidParamOverrideRule::from_bidder_override(
                bidder.as_str(),
                set,
            )?);
        }

        let mut zone_overrides = config.bid_param_zone_overrides.iter().collect::<Vec<_>>();
        zone_overrides.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));

        for (bidder, zone_override_sets) in zone_overrides {
            warn_unconfigured_bidder(config, bidder, "bid_param_zone_overrides");
            let mut sorted_zone_overrides = zone_override_sets.iter().collect::<Vec<_>>();
            sorted_zone_overrides
                .sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));

            for (zone, set) in sorted_zone_overrides {
                rules.push(CompiledBidParamOverrideRule::from_zone_override(
                    bidder.as_str(),
                    zone.as_str(),
                    set,
                )?);
            }
        }

        for rule in &config.bid_param_override_rules {
            if let Some(bidder) = &rule.when.bidder {
                warn_unconfigured_bidder(config, bidder, "bid_param_override_rules");
            }
            rules.push(CompiledBidParamOverrideRule::try_from(rule)?);
        }

        let mut bidder_index: HashMap<String, Vec<usize>> = HashMap::new();
        let mut wildcard_indices: Vec<usize> = Vec::new();
        for (idx, rule) in rules.iter().enumerate() {
            match &rule.bidder {
                Some(bidder) => bidder_index.entry(bidder.clone()).or_default().push(idx),
                None => wildcard_indices.push(idx),
            }
        }

        Ok(Self {
            rules,
            bidder_index,
            wildcard_indices,
        })
    }

    fn apply(&self, facts: BidParamOverrideFacts<'_>, params: &mut Json) {
        let bidder_indices = self.bidder_index.get(facts.bidder).map(Vec::as_slice);
        for idx in merged_rule_indices(&self.wildcard_indices, bidder_indices) {
            let rule = &self.rules[idx];
            if rule.matches(facts) {
                if merge_bidder_param_object(params, &rule.set) {
                    log::debug!(
                        "prebid: applying bidder param override for bidder '{}' zone {:?}: keys {:?}",
                        facts.bidder,
                        facts.zone,
                        rule.set.keys().collect::<Vec<_>>()
                    );
                } else {
                    log::debug!(
                        "prebid: skipping bidder param override for bidder '{}' zone {:?}: params is not a JSON object",
                        facts.bidder,
                        facts.zone
                    );
                }
            }
        }
    }
}

fn merged_rule_indices<'a>(
    wildcard_indices: &'a [usize],
    bidder_indices: Option<&'a [usize]>,
) -> impl Iterator<Item = usize> + 'a {
    let mut wildcard = wildcard_indices.iter().copied().peekable();
    let mut bidder = bidder_indices.unwrap_or(&[]).iter().copied().peekable();

    std::iter::from_fn(move || match (wildcard.peek(), bidder.peek()) {
        (Some(wildcard_idx), Some(bidder_idx)) if wildcard_idx <= bidder_idx => wildcard.next(),
        (Some(_), Some(_)) => bidder.next(),
        (Some(_), None) => wildcard.next(),
        (None, Some(_)) => bidder.next(),
        (None, None) => None,
    })
}

impl CompiledBidParamOverrideRule {
    fn from_bidder_override(
        bidder: &str,
        set: &serde_json::Map<String, Json>,
    ) -> Result<Self, Report<TrustedServerError>> {
        Self::new(
            Some(bidder),
            None,
            set,
            &format!("integrations.prebid.bid_param_overrides.{bidder}"),
            false,
        )
    }

    fn from_zone_override(
        bidder: &str,
        zone: &str,
        set: &serde_json::Map<String, Json>,
    ) -> Result<Self, Report<TrustedServerError>> {
        Self::new(
            Some(bidder),
            Some(zone),
            set,
            &format!("integrations.prebid.bid_param_zone_overrides.{bidder}.{zone}"),
            false,
        )
    }

    fn new(
        bidder: Option<&str>,
        zone: Option<&str>,
        set: &serde_json::Map<String, Json>,
        source: &str,
        is_canonical: bool,
    ) -> Result<Self, Report<TrustedServerError>> {
        let bidder = bidder
            .map(|value| validate_override_matcher_string(value, "when.bidder", source))
            .transpose()?;
        let zone = zone
            .map(|value| validate_override_matcher_string(value, "when.zone", source))
            .transpose()?;

        if bidder.is_none() && zone.is_none() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("{source} must include at least one matcher"),
            }));
        }

        Ok(Self {
            bidder,
            zone,
            set: non_empty_override_object(set, source, is_canonical)?,
        })
    }

    fn matches(&self, facts: BidParamOverrideFacts<'_>) -> bool {
        if let Some(expected_bidder) = self.bidder.as_deref() {
            if expected_bidder != facts.bidder {
                return false;
            }
        }

        if let Some(expected_zone) = self.zone.as_deref() {
            if facts.zone != Some(expected_zone) {
                return false;
            }
        }

        true
    }
}

impl TryFrom<BidParamOverrideRule> for CompiledBidParamOverrideRule {
    type Error = Report<TrustedServerError>;

    fn try_from(rule: BidParamOverrideRule) -> Result<Self, Self::Error> {
        Self::try_from(&rule)
    }
}

impl TryFrom<&BidParamOverrideRule> for CompiledBidParamOverrideRule {
    type Error = Report<TrustedServerError>;

    fn try_from(rule: &BidParamOverrideRule) -> Result<Self, Self::Error> {
        Self::new(
            rule.when.bidder.as_deref(),
            rule.when.zone.as_deref(),
            &rule.set,
            "integrations.prebid.bid_param_override_rules[*]",
            true,
        )
    }
}

// Trims leading/trailing whitespace from config-side matcher strings so that
// accidental padding in TOML/JSON does not produce a silently non-matching rule.
// Runtime facts (bidder name, zone) are not trimmed — they come from controlled
// internal sources (bidder-map keys, trustedServer.zone) where whitespace is
// never expected. Matching is exact and case-sensitive after this normalisation.
fn validate_override_matcher_string(
    value: &str,
    field: &str,
    source: &str,
) -> Result<String, Report<TrustedServerError>> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("{source}.{field} must not be empty"),
        }));
    }

    Ok(trimmed.to_string())
}

fn non_empty_override_object(
    value: &serde_json::Map<String, Json>,
    source: &str,
    is_canonical: bool,
) -> Result<serde_json::Map<String, Json>, Report<TrustedServerError>> {
    if value.is_empty() {
        let message = if is_canonical {
            format!("{source}.set must not be empty")
        } else {
            format!("{source} entry must not be empty")
        };
        return Err(Report::new(TrustedServerError::Configuration { message }));
    }

    Ok(value.clone())
}

/// Copies browser headers to the outgoing Prebid Server request.
///
/// The inbound `X-Forwarded-For` header is never copied: on the server-side
/// publisher auction path `from` is the browser navigation request, so its
/// XFF value is client-supplied and spoofable. Instead the header is
/// synthesized from `client_ip` — the platform-attested client address —
/// matching the trusted IP already sent in `OpenRTB` `device.ip`.
///
/// In [`ConsentForwardingMode::OpenrtbOnly`] mode, consent cookies are
/// stripped from the `Cookie` header since consent travels exclusively
/// through the `OpenRTB` body.
fn copy_request_headers(
    from: &http::Request<EdgeBody>,
    to: &mut http::Request<EdgeBody>,
    consent_forwarding: ConsentForwardingMode,
    client_ip: Option<std::net::IpAddr>,
) {
    let headers_to_copy = [header::USER_AGENT, header::REFERER, header::ACCEPT_LANGUAGE];

    for header_name in &headers_to_copy {
        if let Some(value) = from.headers().get(header_name) {
            to.headers_mut().insert(header_name, value.clone());
        }
    }

    if let Some(ip) = client_ip {
        if let Ok(value) = HeaderValue::from_str(&ip.to_string()) {
            to.headers_mut()
                .insert(header::HeaderName::from_static("x-forwarded-for"), value);
        }
    }

    let Some(cookie_value) = from.headers().get(header::COOKIE) else {
        return;
    };

    if !consent_forwarding.strips_consent_cookies() {
        to.headers_mut()
            .insert(header::COOKIE, cookie_value.clone());
        return;
    }

    match cookie_value.to_str() {
        Ok(value) => {
            let stripped = strip_cookies(value, CONSENT_COOKIE_NAMES);
            if stripped.is_empty() {
                return;
            }

            if let Ok(cookie_header) = HeaderValue::from_str(&stripped) {
                to.headers_mut().insert(header::COOKIE, cookie_header);
            }
        }
        Err(_) => {
            to.headers_mut()
                .insert(header::COOKIE, cookie_value.clone());
        }
    }
}

/// Appends query parameters to a URL, handling both URLs with and without existing query strings.
/// Returns the original URL unchanged if params are empty or already present.
fn append_query_params(url: &str, params: &str) -> String {
    if params.is_empty() || url.contains(params) {
        return url.to_string();
    }
    if url.contains('?') {
        format!("{}&{}", url, params)
    } else {
        format!("{}?{}", url, params)
    }
}

// ============================================================================
// Prebid Auction Provider
// ============================================================================

/// Prebid Server auction provider.
pub struct PrebidAuctionProvider {
    config: PrebidIntegrationConfig,
    bid_param_override_engine: Arc<BidParamOverrideEngine>,
}

impl PrebidAuctionProvider {
    #[cfg(test)]
    fn new(config: PrebidIntegrationConfig) -> Self {
        Self::try_new(config).expect("should compile prebid bid param overrides")
    }

    /// Create a new Prebid auction provider with validated override rules.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured bidder-param override rules are invalid.
    pub fn try_new(config: PrebidIntegrationConfig) -> Result<Self, Report<TrustedServerError>> {
        Ok(Self {
            bid_param_override_engine: Arc::new(BidParamOverrideEngine::try_from_config(&config)?),
            config,
        })
    }

    /// Returns the full Prebid Server `OpenRTB2` auction endpoint URL.
    ///
    /// Backward-compatible normalization: `server_url` may be configured as
    /// either the PBS origin (path is appended here) or the full endpoint
    /// already ending in `/openrtb2/auction` (used as-is, ignoring a trailing
    /// slash) — both shapes produce the same request URL.
    fn auction_endpoint_url(&self) -> String {
        let base = self.config.server_url.trim_end_matches('/');
        if base.ends_with("/openrtb2/auction") {
            base.to_string()
        } else {
            format!("{base}/openrtb2/auction")
        }
    }

    /// Convert auction request to `OpenRTB` format with all enrichments.
    fn to_openrtb(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
        signer: Option<(&RequestSigner, String, &SigningParams)>,
        request_info: RequestInfo,
    ) -> OpenRtbRequest {
        let imps = request
            .slots
            .iter()
            .filter_map(|slot| {
                let slot_context = format!("slot '{}'", slot.id);
                let formats: Vec<_> = slot
                    .formats
                    .iter()
                    .filter(|f| f.media_type == MediaType::Banner)
                    .filter_map(|f| {
                        let width = to_openrtb_i32(f.width, "format.w", &slot_context);
                        let height = to_openrtb_i32(f.height, "format.h", &slot_context);

                        match (width, height) {
                            (Some(width), Some(height)) => Some(Format {
                                w: Some(width),
                                h: Some(height),
                                ..Default::default()
                            }),
                            _ => None,
                        }
                    })
                    .collect();

                if formats.is_empty() {
                    log::warn!(
                        "prebid: dropping imp '{}' — no valid banner formats after filtering",
                        slot.id
                    );
                    return None;
                }

                // Extract zone from trustedServer params (sent by the JS
                // adapter from `mediaTypes.banner.name`, e.g. "header", "fixed_bottom").
                let zone: Option<&str> = slot
                    .bidders
                    .get(TRUSTED_SERVER_BIDDER)
                    .and_then(|p| p.get(ZONE_KEY))
                    .and_then(Json::as_str);

                // Build the bidder map for PBS.
                // The JS adapter sends "trustedServer" as the bidder (our orchestrator
                // adapter name). Replace it with the real PBS bidders from config.
                // Only pass through keys that are known PBS bidders — skip provider-specific
                // keys like "aps" which belong to their own separate auction provider.
                let mut bidder: HashMap<String, Json> = HashMap::new();
                for (name, params) in &slot.bidders {
                    if name == TRUSTED_SERVER_BIDDER {
                        bidder.extend(expand_trusted_server_bidders(&self.config.bidders, params));
                    } else if self.config.bidders.iter().any(|b| b == name) {
                        bidder.insert(name.clone(), params.clone());
                    } else if name != "aps" {
                        // `aps` is intentionally handled by its own provider. Any
                        // other unrecognized key is likely a misconfiguration (a
                        // slot bidder absent from `config.bidders`) that silently
                        // yields an empty bidder map and a stored-request no-bid —
                        // log it so the drop is diagnosable.
                        log::debug!(
                            "prebid: dropping slot '{}' bidder '{}' — not in config.bidders and not a known provider key",
                            slot.id,
                            name
                        );
                    }
                }

                // When no inline PBS bidder params exist (e.g. creative-opportunity slots
                // whose PBS params live in stored requests), tell PBS to resolve bidder
                // config from the stored request keyed by this slot ID.
                //
                // This cannot fire for the client /auction path: the JS adapter
                // injects a `trustedServer` entry into every ad unit, so `bidder`
                // is only empty for server-side creative-opportunity slots with
                // no inline provider params (or when `config.bidders` is empty,
                // where PBS previously received an empty bidder map and returned
                // no bids — a stored-request miss is the same no-bid outcome).
                let storedrequest = if bidder.is_empty() {
                    Some(ImpStoredRequest {
                        id: slot.id.clone(),
                    })
                } else {
                    None
                };

                // Apply canonical and compatibility-derived rules in normalized order.
                for (name, params) in &mut bidder {
                    self.bid_param_override_engine
                        .apply(BidParamOverrideFacts { bidder: name, zone }, params);
                }

                Some(Imp {
                    id: Some(slot.id.clone()),
                    banner: Some(Banner {
                        format: formats,
                        ..Default::default()
                    }),
                    bidfloor: slot.floor_price,
                    // NOTE: Currency defaults to DEFAULT_CURRENCY. If
                    // multi-currency support is needed, this should come from
                    // config or the AdSlot itself.
                    bidfloorcur: slot.floor_price.map(|_| DEFAULT_CURRENCY.to_string()),
                    secure: Some(true), // require HTTPS creatives
                    tagid: Some(slot.id.clone()),
                    ext: ImpExt {
                        prebid: PrebidImpExt {
                            bidder,
                            storedrequest,
                        },
                    }
                    .to_ext(),
                    ..Default::default()
                })
            })
            .collect();

        // Build page URL with debug query params if configured
        let page_url = request.publisher.page_url.as_ref().map(|url| {
            if let Some(ref params) = self.config.debug_query_params {
                append_query_params(url, params)
            } else {
                url.clone()
            }
        });

        // Build user object — populate consent at both OpenRTB 2.6 top-level
        // and Prebid ext-based locations (dual placement).
        // In cookies_only mode, cookie-sourced consent travels through the
        // forwarded Cookie header. KV/policy-sourced consent has no inbound
        // cookie to forward, so carry it in the OpenRTB body instead.
        let consent_ctx = request.user.consent.as_ref().filter(|ctx| {
            self.config.consent_forwarding.includes_body_consent()
                || !matches!(ctx.source, crate::consent::ConsentSource::Cookie)
        });
        let raw_tc = consent_ctx.and_then(|c| c.raw_tc_string.clone());
        let user = Some(User {
            id: request.user.id.clone(),
            // OpenRTB 2.6 top-level consent field
            consent: raw_tc.clone(),
            ext: UserExt {
                // Prebid ext-based consent field
                consent: raw_tc,
                consented_providers_settings: consent_ctx
                    .and_then(|c| c.raw_ac_string.as_ref())
                    .map(|ac| ConsentedProvidersSettings {
                        consented_providers: Some(ac.clone()),
                    }),
                // EIDs resolved from the KV identity graph and consent-gated
                // in `handle_auction` via `gate_eids_by_consent`.
                eids: request.user.eids.clone(),
            }
            .to_ext(),
            ..Default::default()
        });

        // Extract DNT header and Accept-Language from the original request
        let dnt = context
            .request
            .headers()
            .get("DNT")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                if value.trim() == "1" {
                    Some(true)
                } else {
                    None
                }
            });

        let language = context
            .request
            .headers()
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|v| {
                // Extract the primary ISO-639 language tag (e.g., "en" from
                // "en-US,en;q=0.9"). Strip the region subtag so bidders get a
                // normalised two-letter code that maximises match quality.
                v.split(',')
                    .next()
                    .and_then(|tag| tag.split(';').next())
                    .map(|tag| {
                        tag.split('-')
                            .next()
                            .expect("should have at least one split segment")
                            .trim()
                            .to_string()
                    })
                    .filter(|s| !s.is_empty())
            });

        // Build device object with user-agent, client IP, geo, DNT, and language.
        // Forwarding the real client IP is critical: without it PBS infers the
        // IP from the incoming connection (a data-center / edge IP), causing
        // bidders like PubMatic to filter the traffic as non-human.
        //
        // When `request.device` is `None` we still construct a minimal `Device`
        // if DNT or language were extracted from HTTP headers, so those signals
        // are never silently discarded.
        let device = request
            .device
            .as_ref()
            .map(|d| Device {
                ua: d.user_agent.clone(),
                ip: d.ip.clone(),
                geo: d.geo.as_ref().map(|geo| Geo {
                    country: Some(geo.country.clone()),
                    city: Some(geo.city.clone()),
                    region: geo.region.clone(),
                    lat: Some(geo.latitude),
                    lon: Some(geo.longitude),
                    // DMA/metro code: convert i64 to string for OpenRTB.
                    // Fastly returns 0 for "no metro code"; negative values
                    // are not realistic for DMA codes so the > 0 guard is
                    // sufficient.
                    metro: if geo.metro_code > 0 {
                        Some(geo.metro_code.to_string())
                    } else {
                        None
                    },
                    r#type: Some(2),
                    ..Default::default()
                }),
                dnt,
                // Clone needed: `language` is also used in the `or_else`
                // fallback below when `request.device` is `None`.
                language: language.clone(),
                ..Default::default()
            })
            .or_else(|| {
                if dnt.is_some() || language.is_some() {
                    Some(Device {
                        dnt,
                        language,
                        ..Default::default()
                    })
                } else {
                    None
                }
            });

        // Build regs object from ConsentContext, populating both OpenRTB 2.6
        // top-level fields and `regs.ext` for Prebid Server compatibility.
        let regs = Self::build_regs(consent_ctx);

        // Build ext object
        let (version, signature, kid, ts) = signer
            .map(|(s, sig, params)| {
                (
                    Some(SIGNING_VERSION.to_string()),
                    Some(sig),
                    Some(s.kid.clone()),
                    Some(params.timestamp),
                )
            })
            .unwrap_or((None, None, None, None));

        let debug_enabled = self.config.debug;

        let ext = RequestExt {
            prebid: Some(PrebidExt {
                debug: debug_enabled.then_some(true),
                returnallbidstatus: debug_enabled.then_some(true),
            }),
            trusted_server: Some(TrustedServerExt {
                version,
                signature,
                kid,
                request_host: Some(request_info.host),
                request_scheme: Some(request_info.scheme),
                ts,
            }),
        }
        .to_ext();

        // Extract Referer header for site.ref
        let referer = context
            .request
            .headers()
            .get(header::REFERER)
            .and_then(|value| value.to_str().ok())
            .map(std::string::ToString::to_string);

        // Advertise the effective auction budget, not the raw provider config:
        // the orchestrator caps `context.timeout_ms` to the remaining auction
        // budget, and the edge backend stops waiting after that long. Telling
        // PBS it has more time than the edge will wait turns partial bids into
        // edge timeouts.
        let tmax = to_openrtb_i32(context.timeout_ms, "tmax", "request");

        OpenRtbRequest {
            id: Some(request.id.clone()),
            imp: imps,
            site: Some(Site {
                domain: Some(request.publisher.domain.clone()),
                page: page_url,
                r#ref: referer,
                publisher: Some(Publisher {
                    domain: Some(request.publisher.domain.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            user,
            device,
            regs,
            test: self.config.test_mode.then_some(true),
            tmax,
            cur: vec![DEFAULT_CURRENCY.to_string()],
            ext,
            ..Default::default()
        }
    }

    /// Builds the `regs` object from a [`ConsentContext`].
    ///
    /// Populates consent fields at **both** `OpenRTB` 2.6 top-level locations
    /// and the `regs.ext.*` locations that Prebid Server reads today.
    ///
    /// Returns [`None`] if no consent-relevant data is present (avoids sending
    /// an empty `regs` object to Prebid Server).
    fn build_regs(consent_ctx: Option<&crate::consent::ConsentContext>) -> Option<Regs> {
        let ctx = consent_ctx?;

        let has_data = ctx.gdpr_applies
            || ctx.raw_us_privacy.is_some()
            || ctx.raw_gpp_string.is_some()
            || ctx.gpp_section_ids.is_some()
            || ctx.gpc;

        if !has_data {
            return None;
        }

        // Use jurisdiction to inform the GDPR flag: if we know the user is in
        // a GDPR jurisdiction, signal that even without a TCF string (e.g.,
        // GPC-only requests from EU users). When jurisdiction is unknown and
        // no TCF signal exists, omit the field rather than falsely signaling
        // "GDPR does not apply."
        let in_gdpr_jurisdiction = matches!(
            ctx.jurisdiction,
            crate::consent::jurisdiction::Jurisdiction::Gdpr
        );
        let gdpr = if ctx.gdpr_applies || in_gdpr_jurisdiction {
            Some(true)
        } else if matches!(
            ctx.jurisdiction,
            crate::consent::jurisdiction::Jurisdiction::Unknown
        ) {
            None
        } else {
            Some(false)
        };

        // Dual-placement: OpenRTB 2.6 top-level fields AND `regs.ext` for
        // backward compatibility with older exchanges. Extract top-level
        // values first, then clone once into the ext to avoid extra copies.
        let us_privacy = ctx.raw_us_privacy.clone();
        let gpp = ctx.raw_gpp_string.clone();
        let gpp_sid = ctx.gpp_section_ids.clone();

        // RegsExt uses u8 for GDPR (Prebid convention) while the top-level
        // Regs uses bool (OpenRTB proto). Map accordingly.
        let gdpr_u8 = gdpr.map(u8::from);
        let gpp_sid_u16 = gpp_sid.clone();
        let ext = RegsExt {
            gdpr: gdpr_u8,
            us_privacy: us_privacy.clone(),
            gpp: gpp.clone(),
            gpp_sid: gpp_sid_u16,
        };

        Some(Regs {
            coppa: None,
            gdpr,
            us_privacy,
            gpp,
            gpp_sid: gpp_sid
                .map(|ids| ids.into_iter().map(i32::from).collect())
                .unwrap_or_default(),
            ext: ext.to_ext(),
        })
    }

    /// Parse `OpenRTB` response into auction response.
    fn parse_openrtb_response(&self, json: &Json, response_time_ms: u64) -> AuctionResponse {
        let mut bids = Vec::new();

        if let Some(seatbids) = json.get("seatbid").and_then(|v| v.as_array()) {
            for seatbid in seatbids {
                let seat = seatbid
                    .get("seat")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                if let Some(bid_array) = seatbid.get("bid").and_then(|v| v.as_array()) {
                    for bid_obj in bid_array {
                        match self.parse_bid(bid_obj, seat) {
                            Ok(bid) => bids.push(bid),
                            Err(()) => {
                                let impid = bid_obj
                                    .get("impid")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("<missing>");
                                log::warn!(
                                    "Prebid: failed to parse bid from seat '{seat}' for imp '{impid}'"
                                );
                            }
                        }
                    }
                }
            }
        }

        if bids.is_empty() {
            AuctionResponse::no_bid(PREBID_INTEGRATION_ID, response_time_ms)
        } else {
            AuctionResponse::success(PREBID_INTEGRATION_ID, bids, response_time_ms)
        }
    }

    /// Enrich an [`AuctionResponse`] with metadata extracted from the Prebid
    /// Server response `ext`.
    ///
    /// Always-on fields: `responsetimemillis`, `errors`, `warnings`.
    /// Debug-only fields (gated on `config.debug`): `debug`, `bidstatus`.
    fn enrich_response_metadata(
        &self,
        response_json: &Json,
        auction_response: &mut AuctionResponse,
    ) {
        let ext = response_json.get("ext");

        // Always attach per-bidder timing and diagnostics.
        if let Some(rtm) = ext.and_then(|e| e.get("responsetimemillis")) {
            auction_response
                .metadata
                .insert("responsetimemillis".to_string(), rtm.clone());
        }
        if let Some(errors) = ext.and_then(|e| e.get("errors")) {
            auction_response
                .metadata
                .insert("errors".to_string(), errors.clone());
        }
        if let Some(warnings) = ext.and_then(|e| e.get("warnings")) {
            auction_response
                .metadata
                .insert("warnings".to_string(), warnings.clone());
        }

        // When debug is enabled, surface httpcalls, resolvedrequest, and
        // per-bid status from the Prebid Server response.
        if self.config.debug {
            if let Some(debug) = ext.and_then(|e| e.get("debug")) {
                auction_response
                    .metadata
                    .insert("debug".to_string(), debug.clone());
            }
            if let Some(bidstatus) = ext
                .and_then(|e| e.get("prebid"))
                .and_then(|p| p.get("bidstatus"))
            {
                auction_response
                    .metadata
                    .insert("bidstatus".to_string(), bidstatus.clone());
            }
        }
    }

    async fn parse_response_inner(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        auction_id: Option<&str>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let response = response.response;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        // Parse response — collect_response_bounded caps memory from misbehaving providers.
        let body_bytes = collect_response_bounded(
            response.into_body(),
            UPSTREAM_RTB_MAX_RESPONSE_BYTES,
            "prebid",
        )
        .await
        .change_context(TrustedServerError::Prebid {
            message: "Failed to read Prebid response body".to_string(),
        })?;

        if !status.is_success() {
            let auction_id = auction_id.unwrap_or("<unavailable>");
            log::warn!("Prebid auction {auction_id:?} returned non-success status: {status}");

            if self.config.debug {
                match prebid_body_preview(&body_bytes) {
                    Some(preview) => {
                        let truncation = if preview.truncated {
                            " (truncated)"
                        } else {
                            ""
                        };
                        log::warn!(
                            "Prebid auction {auction_id:?} error response body preview{truncation}: {}",
                            preview.text
                        );
                    }
                    None => log::warn!(
                        "Prebid auction {auction_id:?} returned an empty error response body"
                    ),
                }
            }

            let status_code = status.as_u16();
            let mut auction_response =
                AuctionResponse::error(PREBID_INTEGRATION_ID, response_time_ms)
                    .with_metadata("error_type", serde_json::json!(ERROR_TYPE_HTTP_STATUS))
                    .with_metadata("http_status", serde_json::json!(status_code))
                    .with_metadata(
                        "message",
                        serde_json::json!(format!("Prebid Server returned HTTP {status_code}")),
                    );

            if self.config.debug {
                if let Some(message) =
                    extract_prebid_error_message(&body_bytes, content_type.as_deref())
                {
                    auction_response.metadata.insert(
                        "upstream_message".to_string(),
                        serde_json::json!(message.text),
                    );
                    auction_response.metadata.insert(
                        "upstream_message_truncated".to_string(),
                        serde_json::json!(message.truncated),
                    );
                }
            }

            return Ok(auction_response);
        }

        let response_json: Json =
            serde_json::from_slice(&body_bytes).change_context(TrustedServerError::Prebid {
                message: "Failed to parse Prebid response".to_string(),
            })?;

        // Log the full response body when debug is enabled to surface
        // ext.debug.httpcalls, resolvedrequest, bidstatus, errors, etc.
        if self.config.debug && log::log_enabled!(log::Level::Trace) {
            match serde_json::to_string_pretty(&response_json) {
                Ok(json) => log::trace!("Prebid OpenRTB response:\n{json}"),
                Err(e) => {
                    log::warn!("Prebid: failed to serialize response for logging: {e}");
                }
            }
        }

        let mut auction_response = self.parse_openrtb_response(&response_json, response_time_ms);
        self.enrich_response_metadata(&response_json, &mut auction_response);

        log::info!(
            "Prebid returned {} bids in {}ms",
            auction_response.bids.len(),
            response_time_ms
        );

        Ok(auction_response)
    }

    fn should_suppress_bid_notifications(&self, bidder: &str) -> bool {
        self.config.suppress_nurl
            || self
                .config
                .suppress_nurl_bidders
                .iter()
                .any(|suppressed_bidder| suppressed_bidder == bidder)
    }

    /// Parse a single bid from `OpenRTB` response.
    fn parse_bid(&self, bid_obj: &Json, seat: &str) -> Result<AuctionBid, ()> {
        let slot_id = bid_obj
            .get("impid")
            .and_then(|v| v.as_str())
            .ok_or(())?
            .to_string();

        let price = bid_obj
            .get("price")
            .and_then(serde_json::Value::as_f64)
            .ok_or(())?;

        let creative = bid_obj
            .get("adm")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);

        let width = bid_obj
            .get("w")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        let height = bid_obj
            .get("h")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);

        let suppress_bid_notifications = self.should_suppress_bid_notifications(seat);
        let nurl = if suppress_bid_notifications {
            None
        } else {
            bid_obj
                .get("nurl")
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string)
        };

        let burl = if suppress_bid_notifications {
            None
        } else {
            bid_obj
                .get("burl")
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string)
        };

        // `adid` is the creative/ad identifier. The OpenRTB `id` is the bid ID,
        // not an ad ID, so it is not used as a fallback: surfacing it as `ad_id`
        // (which is exposed raw in the debug bid) would mislead any consumer that
        // treats `ad_id` as a creative identifier. Absent `adid`, `ad_id` is None.
        let ad_id = bid_obj
            .get("adid")
            .and_then(|v| v.as_str())
            .map(String::from);

        let adomain = bid_obj
            .get("adomain")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                    .collect()
            });

        // Extract PBS Cache coordinates from ext.prebid.cache.bids
        let cache_entry = bid_obj
            .get("ext")
            .and_then(|e| e.get("prebid"))
            .and_then(|p| p.get("cache"))
            .and_then(|c| c.get("bids"));

        let cache_id = cache_entry
            .and_then(|c| c.get("cacheId"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let (cache_host, cache_path) = cache_entry
            .and_then(|c| c.get("url"))
            .and_then(|v| v.as_str())
            .and_then(|url_str| {
                ParsedUrl::parse(url_str)
                    .map_err(|e| log::debug!("PBS cache URL parse failed: {}", e))
                    .ok()
            })
            .map(|u| {
                let host = u.host_str().map(String::from);
                // path() returns "/" for root — only use if non-trivial
                let path = u.path().to_string();
                let path = if path.is_empty() || path == "/" {
                    None
                } else {
                    Some(path)
                };
                (host, path)
            })
            .unwrap_or((None, None));

        // Guard: if we extracted a cache UUID but couldn't extract the host,
        // the bid will have hb_adid set but no endpoint to fetch from — creative will fail.
        if cache_id.is_some() && cache_host.is_none() {
            log::warn!(
                "PBS bid has cache UUID but cache URL could not be parsed — \
                 creative will fail to render for slot '{}'",
                slot_id
            );
        }

        Ok(AuctionBid {
            slot_id,
            price: Some(price), // Prebid provides decoded prices
            currency: DEFAULT_CURRENCY.to_string(),
            creative,
            adomain,
            bidder: seat.to_string(),
            width,
            height,
            nurl,
            burl,
            ad_id,
            cache_id,
            cache_host,
            cache_path,
            metadata: std::collections::HashMap::new(),
        })
    }
}

#[async_trait(?Send)]
impl AuctionProvider for PrebidAuctionProvider {
    fn provider_name(&self) -> &'static str {
        PREBID_INTEGRATION_ID
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
        log::info!("Prebid: requesting bids for {} slots", request.slots.len());

        let request_info =
            RequestInfo::from_request(context.request, context.services.client_info());

        // Create signer and compute signature if request signing is enabled
        let signer_with_signature =
            if let Some(request_signing_config) = &context.settings.request_signing {
                if request_signing_config.enabled {
                    let signer = RequestSigner::from_services(context.services)?;
                    let params = SigningParams::new(
                        request.id.clone(),
                        request_info.host.clone(),
                        request_info.scheme.clone(),
                    );
                    let signature = signer.sign_request(&params)?;
                    Some((signer, signature, params))
                } else {
                    None
                }
            } else {
                None
            };

        // Convert to OpenRTB with all enrichments
        let openrtb = self.to_openrtb(
            request,
            context,
            signer_with_signature
                .as_ref()
                .map(|(s, sig, params)| (s, sig.clone(), params)),
            request_info,
        );

        // An empty `imp` array violates the OpenRTB spec and wastes a network
        // round-trip. This can happen when all slots are non-Banner or all
        // banner dimensions overflow `i32::MAX`.
        if openrtb.imp.is_empty() {
            log::info!("Prebid: skipping request — no valid impressions after filtering");
            return Err(Report::new(TrustedServerError::Prebid {
                message: "No valid impressions after filtering".to_string(),
            }));
        }

        // Log the outgoing OpenRTB request for debugging.
        if log::log_enabled!(log::Level::Debug) {
            match serde_json::to_string_pretty(&openrtb) {
                Ok(json) => log::debug!(
                    "Prebid OpenRTB request to {}:\n{}",
                    self.auction_endpoint_url(),
                    json
                ),
                Err(e) => {
                    log::warn!("Prebid: failed to serialize OpenRTB request for logging: {e}")
                }
            }
        }

        // Create HTTP request
        let mut pbs_req = http::Request::builder()
            .method(http::Method::POST)
            .uri(self.auction_endpoint_url())
            .body(EdgeBody::empty())
            .change_context(TrustedServerError::Prebid {
                message: "Failed to build Prebid request".to_string(),
            })?;
        copy_request_headers(
            context.request,
            &mut pbs_req,
            self.config.consent_forwarding,
            context.services.client_info().client_ip,
        );

        let pbs_body = serde_json::to_vec(&openrtb).change_context(TrustedServerError::Prebid {
            message: "Failed to serialize Prebid request body".to_string(),
        })?;
        pbs_req.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        *pbs_req.body_mut() = EdgeBody::from(pbs_body);

        // Uses context.timeout_ms (auction-scoped) rather than the 15 s fixed
        // timeout in ensure_integration_backend, which is for proxy endpoints.
        // Send request asynchronously with auction-scoped timeout
        let backend_name = ensure_integration_backend_with_timeout(
            context.services,
            &self.config.server_url,
            "prebid",
            Duration::from_millis(u64::from(context.timeout_ms)),
        )
        .change_context(TrustedServerError::Auction {
            message: format!(
                "Failed to resolve backend for Prebid Server endpoint: {}",
                self.config.server_url
            ),
        })?;
        let pending = context
            .services
            .http_client()
            .send_async(PlatformHttpRequest::new(pbs_req, backend_name))
            .await
            .change_context(TrustedServerError::Prebid {
                message: "Failed to send async request to Prebid Server".to_string(),
            })?;

        Ok(pending)
    }

    async fn parse_response(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        self.parse_response_inner(response, response_time_ms, None)
            .await
    }

    async fn parse_response_with_context(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        self.parse_response_inner(response, response_time_ms, Some(request.id.as_str()))
            .await
    }

    fn supports_media_type(&self, media_type: &MediaType) -> bool {
        matches!(media_type, MediaType::Banner)
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    fn backend_name(&self, services: &RuntimeServices, timeout_ms: u32) -> Option<String> {
        predict_integration_backend_name(
            services,
            &self.config.server_url,
            PREBID_INTEGRATION_ID,
            Duration::from_millis(u64::from(timeout_ms)),
        )
        .inspect_err(|e| {
            log::error!(
                "Failed to predict backend name for Prebid server URL '{}': {e:?}",
                self.config.server_url
            );
        })
        .ok()
    }
}

// ============================================================================
// Provider Auto-Registration
// ============================================================================

/// Auto-register Prebid provider based on settings configuration.
///
/// This function checks the settings for Prebid configuration and returns
/// the provider if enabled.
///
/// # Errors
///
/// Returns an error when the Prebid provider is enabled with invalid
/// configuration.
pub fn register_auction_provider(
    settings: &Settings,
) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        log::info!("Prebid auction provider not registered: integration not found or disabled");
        return Ok(Vec::new());
    };

    log::info!(
        "Registering Prebid auction provider (server_url={})",
        integration.config.server_url
    );
    if integration.config.debug {
        log::warn!(
            "Prebid debug mode is ON — debug data (httpcalls, resolvedrequest, \
             bidstatus) will be included in /auction responses"
        );
    }

    Ok(vec![Arc::new(integration.auction_provider())])
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::auction::test_support::create_test_auction_context as shared_test_auction_context;
    use crate::auction::types::{
        AdFormat, AdSlot, AuctionContext, AuctionRequest, DeviceInfo, PublisherInfo, UserInfo,
    };

    use crate::consent::{ConsentContext, ConsentSource};
    use crate::geo::GeoInfo;
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};
    use crate::integrations::{
        AttributeRewriteAction, IntegrationDocumentState, IntegrationRegistry,
    };
    use crate::platform::test_support::{
        build_services_with_http_client, NoopConfigStore, NoopGeo, NoopHttpClient, NoopSecretStore,
        StubHttpClient,
    };
    use crate::platform::{
        ClientInfo, PlatformBackend, PlatformBackendSpec, PlatformError, RuntimeServices,
    };
    use crate::settings::Settings;
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::create_test_settings;
    use base64::engine::general_purpose::STANDARD as TEST_BASE64_STANDARD;
    use http::Method;
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::Cursor;

    fn make_settings() -> Settings {
        create_test_settings()
    }

    fn base_config() -> PrebidIntegrationConfig {
        PrebidIntegrationConfig {
            enabled: true,
            server_url: "https://prebid.example".to_string(),
            account_id: Some("test-account".to_string()),
            timeout_ms: 1000,
            bidders: vec!["exampleBidder".to_string()],
            debug: false,
            test_mode: false,
            debug_query_params: None,
            script_patterns: default_script_patterns(),
            external_bundle_url: Some(
                "https://assets.example/prebid/trusted-prebid.js".to_string(),
            ),
            external_bundle_sha256: None,
            external_bundle_sri: None,
            client_side_bidders: Vec::new(),
            bid_param_zone_overrides: HashMap::default(),
            bid_param_overrides: HashMap::default(),
            bid_param_override_rules: Vec::new(),
            consent_forwarding: ConsentForwardingMode::Both,
            suppress_nurl: false,
            suppress_nurl_bidders: Vec::new(),
        }
    }

    struct PredictOnlyBackend;

    impl PlatformBackend for PredictOnlyBackend {
        fn predict_name(
            &self,
            spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Ok(format!(
                "predicted_{}_{}_{}_{}",
                spec.scheme,
                spec.host,
                spec.first_byte_timeout.as_millis(),
                spec.between_bytes_timeout.as_millis()
            ))
        }

        fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Ok("unused".to_string())
        }
    }

    fn services_with_backend(backend: impl PlatformBackend + 'static) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::new(backend))
            .http_client(Arc::new(NoopHttpClient))
            .geo(Arc::new(NoopGeo))
            .client_info(ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
                ..ClientInfo::default()
            })
            .build()
    }

    #[test]
    fn prebid_backend_name_delegates_to_platform_backend_prediction() {
        let provider = PrebidAuctionProvider::new(base_config());
        let services = services_with_backend(PredictOnlyBackend);

        let backend_name = provider
            .backend_name(&services, 123)
            .expect("should predict backend name through platform backend");

        assert_eq!(
            backend_name, "predicted_https_prebid.example_123_123",
            "should cap both first-byte and between-bytes timeouts to the auction budget"
        );
    }

    fn test_sri(algorithm: &str, digest: &[u8]) -> String {
        format!("{algorithm}-{}", TEST_BASE64_STANDARD.encode(digest))
    }

    fn test_request(url: impl AsRef<str>) -> http::Request<EdgeBody> {
        http::Request::builder()
            .method(http::Method::GET)
            .uri(url.as_ref())
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    fn header_value_str(response: &http::Response<EdgeBody>, name: &str) -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok().map(std::string::ToString::to_string))
    }

    fn response_header_is_present(response: &http::Response<EdgeBody>, name: &str) -> bool {
        response.headers().contains_key(name)
    }

    fn response_body_string(response: http::Response<EdgeBody>) -> String {
        String::from_utf8(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .to_vec(),
        )
        .expect("should parse response body as utf-8")
    }

    fn prebid_platform_response(
        status: StatusCode,
        content_type: Option<&str>,
        body: impl Into<Vec<u8>>,
    ) -> PlatformResponse {
        let mut builder = http::Response::builder().status(status);
        if let Some(content_type) = content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }

        PlatformResponse::new(
            builder
                .body(EdgeBody::from(body.into()))
                .expect("should build Prebid platform response"),
        )
    }

    fn create_test_auction_request() -> AuctionRequest {
        AuctionRequest {
            id: "auction-123".to_string(),
            slots: vec![AdSlot {
                id: "slot-1".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                }],
                floor_price: None,
                targeting: HashMap::new(),
                bidders: HashMap::new(),
            }],
            publisher: PublisherInfo {
                domain: "pub.example".to_string(),
                page_url: Some("https://pub.example/article".to_string()),
            },
            user: UserInfo {
                id: Some("user-123".to_string()),
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        }
    }

    fn build_test_request() -> http::Request<EdgeBody> {
        http::Request::builder()
            .method(http::Method::GET)
            .uri("https://pub.example/auction")
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    #[test]
    fn prebid_provider_uses_platform_http_client_for_bid_request() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, br#"{"seatbid":[]}"#.to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = make_settings();
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();
        let http_req = http::Request::builder()
            .method(http::Method::POST)
            .uri("https://publisher.example/auction")
            .body(EdgeBody::empty())
            .expect("should build request");
        let context = AuctionContext {
            settings: &settings,
            request: &http_req,
            timeout_ms: 500,
            provider_responses: None,
            services: &services,
        };

        let pending =
            futures::executor::block_on(provider.request_bids(&auction_request, &context))
                .expect("should start request");

        assert!(
            pending.backend_name().is_some(),
            "should preserve backend correlation"
        );
        assert_eq!(
            stub.recorded_backend_names().len(),
            1,
            "should launch one upstream request through PlatformHttpClient"
        );
    }

    fn create_test_auction_context<'a>(
        settings: &'a Settings,
        request: &'a http::Request<EdgeBody>,
    ) -> AuctionContext<'a> {
        shared_test_auction_context(settings, request, 1000)
    }

    fn make_request_info(context: &AuctionContext<'_>) -> RequestInfo {
        RequestInfo::from_request(context.request, context.services.client_info())
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

    /// Shared TOML prefix for config-parsing tests (publisher + ec sections).
    const TOML_BASE: &str = r#"
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
"#;

    /// Parse a TOML string containing only the `[integrations.prebid]` section
    /// (plus any sub-tables) into a [`PrebidIntegrationConfig`].
    fn parse_prebid_toml(prebid_section: &str) -> PrebidIntegrationConfig {
        let toml_str = format!("{}{}", TOML_BASE, prebid_section);
        let settings = Settings::from_toml(&toml_str).expect("should parse TOML");
        settings
            .integration_config::<PrebidIntegrationConfig>("prebid")
            .expect("should get config")
            .expect("should be enabled")
    }

    fn parse_prebid_toml_result(
        prebid_section: &str,
    ) -> Result<PrebidIntegrationConfig, Report<TrustedServerError>> {
        let toml_str = format!("{}{}", TOML_BASE, prebid_section);
        let settings = Settings::from_toml(&toml_str)?;
        settings
            .integration_config::<PrebidIntegrationConfig>("prebid")?
            .ok_or_else(|| {
                Report::new(TrustedServerError::Configuration {
                    message: "prebid integration config should be present and enabled".to_string(),
                })
            })
    }

    fn json_object(value: Json) -> serde_json::Map<String, Json> {
        serde_json::from_value(value).expect("should build JSON object")
    }

    #[test]
    fn attribute_rewriter_removes_prebid_scripts() {
        let integration = PrebidIntegration::new(base_config());
        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
        };

        let rewritten = integration.rewrite("src", "https://cdn.prebid.org/prebid.min.js", &ctx);
        assert!(matches!(rewritten, AttributeRewriteAction::RemoveElement));

        let untouched = integration.rewrite("src", "https://cdn.example.com/app.js", &ctx);
        assert!(matches!(untouched, AttributeRewriteAction::Keep));
    }

    #[test]
    fn attribute_rewriter_handles_query_strings_and_links() {
        let integration = PrebidIntegration::new(base_config());
        let ctx = IntegrationAttributeContext {
            attribute_name: "href",
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
        };

        let rewritten =
            integration.rewrite("href", "https://cdn.prebid.org/prebid.js?v=1.2.3", &ctx);
        assert!(matches!(rewritten, AttributeRewriteAction::RemoveElement));
    }

    #[test]
    fn html_processor_keeps_prebid_scripts_when_no_patterns() {
        let html = r#"<html><head>
            <script src="https://cdn.prebid.org/prebid.min.js"></script>
            <link rel="preload" as="script" href="https://cdn.prebid.org/prebid.js" />
        </head><body></body></html>"#;

        let mut settings = make_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &json!({
                    "enabled": true,
                    "server_url": "https://test-prebid.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "timeout_ms": 1000,
                    "bidders": ["mocktioneer"],
                    "script_patterns": [],
                    "debug": false
                }),
            )
            .expect("should update prebid config");
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
        assert!(
            processed.contains("tsjs-unified"),
            "Unified bundle should be injected"
        );
        assert!(
            processed.contains("prebid.min.js"),
            "Prebid script should remain when no script patterns configured"
        );
        assert!(
            processed.contains("cdn.prebid.org/prebid.js"),
            "Prebid preload should remain when no script patterns configured"
        );
    }

    #[test]
    fn html_processor_removes_prebid_scripts_when_patterns_match() {
        let html = r#"<html><head>
            <script src="https://cdn.prebid.org/prebid.min.js"></script>
            <link rel="preload" as="script" href="https://cdn.prebid.org/prebid.js" />
        </head><body></body></html>"#;

        let mut settings = make_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &json!({
                    "enabled": true,
                    "server_url": "https://test-prebid.com/openrtb2/auction",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "timeout_ms": 1000,
                    "bidders": ["mocktioneer"],
                    "script_patterns": ["/prebid.js", "/prebid.min.js"],
                    "debug": false
                }),
            )
            .expect("should update prebid config");
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
        assert!(
            processed.contains("tsjs-unified"),
            "Unified bundle should be injected"
        );
        assert!(
            !processed.contains("cdn.prebid.org/prebid.min.js"),
            "Publisher prebid script should be removed when auto-config is enabled"
        );
        assert!(
            !processed.contains("cdn.prebid.org/prebid.js"),
            "Prebid preload should be removed when auto-config is enabled"
        );
        assert!(
            processed.contains(PREBID_BUNDLE_ROUTE),
            "External prebid bundle route should be injected"
        );
        assert!(
            !processed.contains("tsjs-prebid.min.js"),
            "Embedded deferred prebid bundle should not be injected"
        );
    }

    #[test]
    fn matches_script_url_matches_common_variants() {
        let integration = PrebidIntegration::new(base_config());
        assert!(integration.matches_script_url("https://cdn.com/prebid.js"));
        assert!(integration.matches_script_url("https://cdn.com/prebid.min.js?version=1"));
        assert!(!integration.matches_script_url("https://cdn.com/app.js"));
    }

    #[test]
    fn script_patterns_config_parsing() {
        let config = parse_prebid_toml(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
script_patterns = ["/prebid.js", "/custom/prebid.min.js"]
"#,
        );

        assert_eq!(config.script_patterns.len(), 2);
        assert!(config.script_patterns.contains(&"/prebid.js".to_string()));
        assert!(config
            .script_patterns
            .contains(&"/custom/prebid.min.js".to_string()));
    }

    #[test]
    fn script_patterns_defaults() {
        let config = parse_prebid_toml(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
"#,
        );

        assert!(!config.script_patterns.is_empty());
        assert!(config.script_patterns.contains(&"/prebid.js".to_string()));
        assert!(config
            .script_patterns
            .contains(&"/prebid.min.js".to_string()));
    }

    #[test]
    fn external_bundle_config_parses_with_optional_hash_metadata() {
        let config = parse_prebid_toml(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"
"#,
        );

        assert_eq!(
            config.external_bundle_url.as_deref(),
            Some("https://assets.example/prebid/trusted-prebid.js"),
            "should preserve configured external bundle URL"
        );
        assert!(
            config.external_bundle_sha256.is_none(),
            "SHA-256 should be optional"
        );
    }

    #[test]
    fn external_bundle_config_rejects_malformed_hash_metadata() {
        let err = parse_prebid_toml_result(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"
external_bundle_sha256 = "not-a-sha"
"#,
        )
        .expect_err("should reject malformed SHA-256");

        assert!(
            err.to_string().contains("external_bundle_sha256"),
            "error should mention malformed SHA-256: {err:?}"
        );
    }

    #[test]
    fn external_bundle_config_rejects_non_https_bundle_url() {
        let err = parse_prebid_toml_result(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
external_bundle_url = "http://assets.example/prebid/trusted-prebid.js"
"#,
        )
        .expect_err("should reject non-HTTPS external bundle URL");

        assert!(
            err.to_string().contains("external_bundle_url"),
            "error should mention external bundle URL: {err:?}"
        );
    }

    #[test]
    fn external_bundle_config_rejects_invalid_sri_base64() {
        let err = parse_prebid_toml_result(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"
external_bundle_sri = "sha384-not-valid!!!"
"#,
        )
        .expect_err("should reject invalid SRI base64");

        assert!(
            err.to_string().contains("external_bundle_sri"),
            "error should mention external bundle SRI: {err:?}"
        );
    }

    #[test]
    fn external_bundle_config_rejects_sri_with_wrong_digest_length() {
        let err = parse_prebid_toml_result(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"
external_bundle_sri = "sha384-AAAA"
"#,
        )
        .expect_err("should reject SRI with wrong digest length");

        assert!(
            err.to_string().contains("external_bundle_sri"),
            "error should mention external bundle SRI: {err:?}"
        );
    }

    #[test]
    fn external_bundle_registration_allows_sha256_without_sri() {
        let mut settings = make_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &json!({
                    "enabled": true,
                    "server_url": "https://prebid.example/openrtb2/auction",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "external_bundle_sha256": "0".repeat(64)
                }),
            )
            .expect("should update prebid config");

        let registry = IntegrationRegistry::new(&settings)
            .expect("should create registry with valid SHA-256 and no SRI");

        assert!(
            registry.has_route(&Method::GET, PREBID_BUNDLE_ROUTE),
            "should register external bundle route"
        );
    }

    #[test]
    fn external_bundle_registration_allows_sha256_with_valid_sha384_sri() {
        let mut settings = make_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &json!({
                    "enabled": true,
                    "server_url": "https://prebid.example/openrtb2/auction",
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "external_bundle_sha256": "0".repeat(64),
                    "external_bundle_sri": test_sri("sha384", &[0; 48])
                }),
            )
            .expect("should update prebid config");

        let registry = IntegrationRegistry::new(&settings)
            .expect("should create registry with valid SHA-256 and SHA-384 SRI");

        assert!(
            registry.has_route(&Method::GET, PREBID_BUNDLE_ROUTE),
            "should register external bundle route"
        );
    }

    #[test]
    fn external_bundle_registration_requires_bundle_url() {
        let mut settings = make_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &json!({
                    "enabled": true,
                    "server_url": "https://prebid.example/openrtb2/auction"
                }),
            )
            .expect("should update prebid config");

        let err = match IntegrationRegistry::new(&settings) {
            Ok(_) => panic!("should reject missing URL"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("external_bundle_url"),
            "error should mention missing external bundle URL: {err:?}"
        );
    }

    #[test]
    fn external_bundle_registration_uses_proxy_allowed_domains() {
        let mut settings = make_settings();
        settings.proxy.allowed_domains = vec!["allowed.example".to_string()];
        settings
            .integrations
            .insert_config(
                "prebid",
                &json!({
                    "enabled": true,
                    "server_url": "https://prebid.example/openrtb2/auction",
                    "external_bundle_url": "https://blocked.example/prebid/trusted-prebid.js"
                }),
            )
            .expect("should update prebid config");

        let err = match IntegrationRegistry::new(&settings) {
            Ok(_) => panic!("should reject bundle host outside proxy.allowed_domains"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("proxy.allowed_domains"),
            "error should mention proxy.allowed_domains: {err:?}"
        );
    }

    #[test]
    fn script_handler_returns_empty_js() {
        let integration = PrebidIntegration::new(base_config());

        let response = integration
            .handle_script_handler()
            .expect("should return response");

        assert_eq!(response.status(), StatusCode::OK);

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .expect("should have content-type");
        assert_eq!(content_type, "application/javascript; charset=utf-8");

        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .expect("should have cache-control");
        assert!(cache_control.contains("max-age=31536000"));

        let body = String::from_utf8(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .to_vec(),
        )
        .expect("should parse script body as utf-8");
        assert!(body.contains("// Script overridden by Trusted Server"));
    }

    #[test]
    fn external_bundle_request_cache_mode_validates_version_query() {
        let sha256 = "a".repeat(64);
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        config.external_bundle_sha256 = Some(sha256.clone());
        let integration = PrebidIntegration::new(config);

        let versioned_req = test_request(format!(
            "https://pub.example{PREBID_BUNDLE_ROUTE}?v={sha256}"
        ));
        let missing_version_req = test_request(format!("https://pub.example{PREBID_BUNDLE_ROUTE}"));
        let mismatched_req = test_request(format!(
            "https://pub.example{PREBID_BUNDLE_ROUTE}?v={}",
            "b".repeat(64)
        ));

        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&versioned_req)
                .expect("should parse versioned request"),
            Some(ExternalBundleCacheMode::Immutable),
            "matching v query should use immutable cache mode"
        );
        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&missing_version_req)
                .expect("should parse unversioned request"),
            Some(ExternalBundleCacheMode::Revalidate),
            "missing v query should use revalidation cache mode"
        );
        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&mismatched_req)
                .expect("should parse mismatched request"),
            None,
            "mismatched v query should 404"
        );
    }

    #[test]
    fn external_bundle_request_cache_mode_rejects_version_when_hash_is_absent() {
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        let integration = PrebidIntegration::new(config);

        let versioned_req = test_request(format!(
            "https://pub.example{PREBID_BUNDLE_ROUTE}?v={}",
            "a".repeat(64)
        ));
        let unversioned_req = test_request(format!("https://pub.example{PREBID_BUNDLE_ROUTE}"));

        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&versioned_req)
                .expect("should parse versioned request"),
            None,
            "v query should 404 when SHA-256 is omitted"
        );
        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&unversioned_req)
                .expect("should parse unversioned request"),
            Some(ExternalBundleCacheMode::Revalidate),
            "unversioned request should be served with revalidation cache mode"
        );
    }

    #[test]
    fn external_bundle_headers_use_cache_policy_for_mode() {
        let sha256 = "a".repeat(64);
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        config.external_bundle_sha256 = Some(sha256.clone());
        config.external_bundle_sri = Some(test_sri("sha384", &[0; 48]));
        let integration = PrebidIntegration::new(config);

        let mut immutable = http::Response::builder()
            .status(StatusCode::OK)
            .body(EdgeBody::empty())
            .expect("should build response");
        integration
            .apply_external_bundle_headers(&mut immutable, ExternalBundleCacheMode::Immutable);
        assert_eq!(
            header_value_str(&immutable, "content-type"),
            Some(PREBID_BUNDLE_CONTENT_TYPE.to_string()),
            "should normalize JS content type"
        );
        assert_eq!(
            header_value_str(&immutable, PREBID_BUNDLE_NOSNIFF_HEADER),
            Some(PREBID_BUNDLE_NOSNIFF_VALUE.to_string()),
            "should disable content sniffing"
        );
        assert_eq!(
            header_value_str(&immutable, "cache-control"),
            Some(PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL.to_string()),
            "versioned responses should be immutable"
        );
        assert_eq!(
            header_value_str(&immutable, "etag"),
            Some(format!("\"sha256:{sha256}\"")),
            "should emit configured hash ETag"
        );

        let mut revalidate = http::Response::builder()
            .status(StatusCode::OK)
            .body(EdgeBody::empty())
            .expect("should build response");
        integration
            .apply_external_bundle_headers(&mut revalidate, ExternalBundleCacheMode::Revalidate);
        assert_eq!(
            header_value_str(&revalidate, "cache-control"),
            Some(PREBID_BUNDLE_REVALIDATION_CACHE_CONTROL.to_string()),
            "unversioned responses should use short-lived revalidation"
        );
    }

    #[test]
    fn external_bundle_response_sanitization_uses_header_whitelist_for_ok_response() {
        let sha256 = "a".repeat(64);
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        config.external_bundle_sha256 = Some(sha256.clone());
        config.external_bundle_sri = Some(test_sri("sha384", &[0; 48]));
        let integration = PrebidIntegration::new(config);

        let mut upstream = http::Response::builder()
            .status(StatusCode::OK)
            .body(EdgeBody::from("console.log('ok');"))
            .expect("should build upstream response");
        upstream
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        upstream.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=0"),
        );
        upstream.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("bad=1; Path=/"),
        );
        upstream
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        upstream
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("16"));
        upstream.headers_mut().insert(
            header::HeaderName::from_static("x-upstream"),
            HeaderValue::from_static("leak"),
        );

        let sanitized = integration
            .sanitize_external_bundle_response(upstream, ExternalBundleCacheMode::Immutable);

        assert_eq!(
            header_value_str(&sanitized, "content-type"),
            Some(PREBID_BUNDLE_CONTENT_TYPE.to_string()),
            "should normalize JS content type"
        );
        assert_eq!(
            header_value_str(&sanitized, PREBID_BUNDLE_NOSNIFF_HEADER),
            Some(PREBID_BUNDLE_NOSNIFF_VALUE.to_string()),
            "should disable content sniffing"
        );
        assert_eq!(
            header_value_str(&sanitized, "cache-control"),
            Some(PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL.to_string()),
            "should apply trusted cache policy"
        );
        assert_eq!(
            header_value_str(&sanitized, "etag"),
            Some(format!("\"sha256:{sha256}\"")),
            "should emit trusted ETag"
        );
        assert_eq!(
            header_value_str(&sanitized, "content-encoding"),
            Some("gzip".to_string()),
            "should preserve body encoding metadata"
        );
        assert!(
            !response_header_is_present(&sanitized, "content-length"),
            "should strip upstream content length so the platform can derive it from the body"
        );
        assert!(
            !response_header_is_present(&sanitized, "set-cookie"),
            "should strip upstream Set-Cookie"
        );
        assert!(
            !response_header_is_present(&sanitized, "x-upstream"),
            "should strip arbitrary upstream headers"
        );
        assert_eq!(
            response_body_string(sanitized),
            "console.log('ok');",
            "should preserve body bytes"
        );
    }

    #[test]
    fn external_bundle_response_sanitization_strips_headers_for_error_response() {
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        let integration = PrebidIntegration::new(config);

        let mut upstream = http::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(EdgeBody::from("missing"))
            .expect("should build upstream response");
        upstream
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        upstream.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000"),
        );
        upstream.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("bad=1; Path=/"),
        );
        upstream.headers_mut().insert(
            header::HeaderName::from_static("x-upstream"),
            HeaderValue::from_static("leak"),
        );

        let sanitized = integration
            .sanitize_external_bundle_response(upstream, ExternalBundleCacheMode::Revalidate);

        assert_eq!(
            sanitized.status(),
            StatusCode::NOT_FOUND,
            "should preserve upstream status"
        );
        assert_eq!(
            header_value_str(&sanitized, "cache-control"),
            Some(PREBID_BUNDLE_ERROR_CACHE_CONTROL.to_string()),
            "should prevent caching upstream error responses"
        );
        assert_eq!(
            header_value_str(&sanitized, "content-type"),
            Some(PREBID_BUNDLE_ERROR_CONTENT_TYPE.to_string()),
            "should replace upstream content type on error responses"
        );
        assert_eq!(
            header_value_str(&sanitized, PREBID_BUNDLE_NOSNIFF_HEADER),
            Some(PREBID_BUNDLE_NOSNIFF_VALUE.to_string()),
            "should disable content sniffing on error responses"
        );
        assert!(
            !response_header_is_present(&sanitized, "set-cookie"),
            "should strip upstream Set-Cookie on error responses"
        );
        assert!(
            !response_header_is_present(&sanitized, "x-upstream"),
            "should strip arbitrary upstream headers on error responses"
        );
    }

    #[test]
    fn external_bundle_startup_validation_requires_proxy_allowed_domains() {
        let mut settings = make_settings();
        settings.proxy.allowed_domains.clear();

        let err = validate_config_for_startup(&settings)
            .expect_err("should reject external bundle without proxy allowlist");

        assert!(
            err.to_string().contains("proxy.allowed_domains"),
            "error should mention proxy.allowed_domains: {err:?}"
        );
    }

    #[test]
    fn external_bundle_handler_fetches_and_sanitizes_with_platform_client() {
        futures::executor::block_on(async {
            let sha256 = "a".repeat(64);
            let mut config = base_config();
            config.external_bundle_sha256 = Some(sha256.clone());
            let integration = PrebidIntegration::new(config);
            let mut settings = make_settings();
            settings.proxy.allowed_domains = vec!["assets.example".to_string()];

            let stub = Arc::new(StubHttpClient::new());
            stub.push_response_with_headers(
                200,
                b"console.log('bundle');".to_vec(),
                vec![
                    (header::CONTENT_TYPE.as_str(), "text/html"),
                    (header::CACHE_CONTROL.as_str(), "private, max-age=0"),
                    (header::SET_COOKIE.as_str(), "bad=1; Path=/"),
                    ("x-upstream", "leak"),
                ],
            );
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
            );
            let req = http::Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "https://pub.example{PREBID_BUNDLE_ROUTE}?v={sha256}"
                ))
                .header(header::COOKIE, "ts-ec=should-not-forward")
                .header(header::ACCEPT, "*/*")
                .body(EdgeBody::empty())
                .expect("should build external bundle request");

            let response = integration
                .handle_external_bundle(&settings, &services, req)
                .await
                .expect("should proxy external bundle");

            assert_eq!(response.status(), StatusCode::OK, "should preserve status");
            assert_eq!(
                header_value_str(&response, header::CONTENT_TYPE.as_str()),
                Some(PREBID_BUNDLE_CONTENT_TYPE.to_string()),
                "should normalize JS content type"
            );
            assert_eq!(
                header_value_str(&response, header::CACHE_CONTROL.as_str()),
                Some(PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL.to_string()),
                "versioned bundle response should be immutable"
            );
            assert_eq!(
                header_value_str(&response, header::ETAG.as_str()),
                Some(format!("\"sha256:{sha256}\"")),
                "should emit configured hash ETag"
            );
            assert!(
                !response_header_is_present(&response, header::SET_COOKIE.as_str()),
                "should strip upstream Set-Cookie"
            );
            assert!(
                !response_header_is_present(&response, "x-upstream"),
                "should strip arbitrary upstream headers"
            );
            assert_eq!(
                response_body_string(response),
                "console.log('bundle');",
                "should preserve bundle bytes"
            );

            assert_eq!(
                stub.recorded_request_uris(),
                vec!["https://assets.example/prebid/trusted-prebid.js".to_string()],
                "should fetch the configured external bundle URL without adding EC query params"
            );
            let recorded_headers = stub.recorded_request_headers();
            assert_eq!(
                recorded_headers.len(),
                1,
                "should make one upstream request"
            );
            assert!(
                !recorded_headers[0]
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(header::COOKIE.as_str())),
                "external bundle fetch should not forward client cookies"
            );
            assert!(
                !recorded_headers[0]
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(header::ACCEPT.as_str())),
                "external bundle fetch should not forward client headers"
            );
        });
    }

    #[test]
    fn routes_include_script_patterns() {
        let integration = PrebidIntegration::new(base_config());

        let routes = integration.routes();

        // Should have routes for default script patterns
        assert!(!routes.is_empty());

        let has_prebid_js_route = routes
            .iter()
            .any(|r| r.path == "/prebid.js" && r.method == Method::GET);
        assert!(has_prebid_js_route, "should register /prebid.js route");

        let has_prebid_min_js_route = routes
            .iter()
            .any(|r| r.path == "/prebid.min.js" && r.method == Method::GET);
        assert!(
            has_prebid_min_js_route,
            "should register /prebid.min.js route"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.path == PREBID_BUNDLE_ROUTE && r.method == Method::GET),
            "should register the bundle route"
        );
    }

    #[test]
    fn head_injector_emits_config_script() {
        let integration = PrebidIntegration::new(base_config());
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert_eq!(inserts.len(), 2, "should produce config and bundle inserts");

        let script = &inserts[0];
        assert!(
            script.starts_with("<script>") && script.ends_with("</script>"),
            "should be wrapped in script tags"
        );
        assert!(
            script.contains(r#""accountId":"test-account""#),
            "should include accountId from config: {}",
            script
        );
        assert!(
            script.contains(r#""timeout":1000"#),
            "should include timeout: {}",
            script
        );
        assert!(
            script.contains(r#""debug":false"#),
            "should include debug flag: {}",
            script
        );
        assert!(
            script.contains(r#""bidders":["exampleBidder"]"#),
            "should include bidders array: {}",
            script
        );
    }

    #[test]
    fn head_injector_handles_missing_account_id() {
        let mut config = base_config();
        config.account_id = None;
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];
        assert!(
            script.contains(r#""accountId":"""#),
            "should emit empty accountId when not configured: {}",
            script
        );
    }

    #[test]
    fn head_injector_emits_external_bundle_script_with_hash_and_integrity() {
        let sha256 = "a".repeat(64);
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        config.external_bundle_sha256 = Some(sha256.clone());
        config.external_bundle_sri = Some(test_sri("sha384", &[0; 48]));
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);

        assert_eq!(inserts.len(), 2, "should emit config and bundle scripts");
        assert!(
            inserts[1].contains(&format!("src=\"{PREBID_BUNDLE_ROUTE}?v={sha256}\"")),
            "bundle script should use content-addressed first-party URL: {}",
            inserts[1]
        );
        assert!(
            inserts[1].contains("integrity=\"sha384-"),
            "bundle script should include configured SRI: {}",
            inserts[1]
        );
        assert!(
            !inserts[1].contains("crossorigin"),
            "same-origin bundle script should not include crossorigin: {}",
            inserts[1]
        );
    }

    #[test]
    fn head_injector_emits_external_bundle_script_without_hash_query_when_unhashed() {
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);

        assert_eq!(inserts.len(), 2, "should emit config and bundle scripts");
        assert!(
            inserts[1].contains(&format!("src=\"{PREBID_BUNDLE_ROUTE}\"")),
            "bundle script should use first-party route without hash query: {}",
            inserts[1]
        );
        assert!(
            !inserts[1].contains("?v="),
            "unhashed bundle script should not include version query: {}",
            inserts[1]
        );
    }

    #[test]
    fn head_injector_escapes_closing_script_tags_in_values() {
        let mut config = base_config();
        config.account_id = Some("</script><script>alert(1)</script>".to_string());
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];
        assert!(
            script.contains(r#""accountId":"<\/script><script>alert(1)<\/script>""#),
            "should escape closing script tags inside JSON values: {}",
            script
        );
    }

    #[test]
    fn head_injector_omits_client_side_bidders_when_empty() {
        let integration = PrebidIntegration::new(base_config());
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];
        assert!(
            !script.contains("clientSideBidders"),
            "should omit clientSideBidders when empty: {}",
            script
        );
    }

    #[test]
    fn head_injector_includes_client_side_bidders_when_configured() {
        let mut config = base_config();
        config.client_side_bidders = vec!["rubicon".to_string(), "magnite".to_string()];
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];
        assert!(
            script.contains(r#""clientSideBidders":["rubicon","magnite"]"#),
            "should include clientSideBidders array: {}",
            script
        );
    }

    #[test]
    fn to_openrtb_includes_debug_flags_when_enabled() {
        let mut config = base_config();
        config.debug = true;

        let provider = PrebidAuctionProvider::new(config);
        let auction_request = create_test_auction_request();
        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.test, None,
            "debug alone should not set top-level OpenRTB test field"
        );

        let prebid_ext = get_prebid_ext(&openrtb);
        assert_eq!(
            prebid_ext.debug,
            Some(true),
            "should include ext.prebid.debug when debug is enabled"
        );
        assert_eq!(
            prebid_ext.returnallbidstatus,
            Some(true),
            "should include ext.prebid.returnallbidstatus when debug is enabled"
        );

        let serialized = serde_json::to_value(&openrtb).expect("should serialize OpenRTB request");
        assert!(
            serialized.get("test").is_none(),
            "debug alone should not serialize top-level test"
        );
        assert_eq!(
            serialized["ext"]["prebid"]["debug"],
            json!(true),
            "should serialize ext.prebid.debug when debug is enabled"
        );
        assert_eq!(
            serialized["ext"]["prebid"]["returnallbidstatus"],
            json!(true),
            "should serialize ext.prebid.returnallbidstatus when debug is enabled"
        );
    }

    #[test]
    fn to_openrtb_sets_test_flag_when_test_mode_enabled() {
        let mut config = base_config();
        config.test_mode = true;

        let provider = PrebidAuctionProvider::new(config);
        let auction_request = create_test_auction_request();
        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.test,
            Some(true),
            "should set top-level OpenRTB test field when test_mode is enabled"
        );

        let serialized = serde_json::to_value(&openrtb).expect("should serialize OpenRTB request");
        assert_eq!(
            serialized["test"],
            json!(1),
            "should serialize top-level test as 1 when test_mode is enabled"
        );
    }

    #[test]
    fn to_openrtb_serializes_device_ip_when_present() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("test-agent".to_string()),
            ip: Some("203.0.113.42".to_string()),
            geo: None,
        });
        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb
                .device
                .as_ref()
                .and_then(|device| device.ip.as_deref()),
            Some("203.0.113.42"),
            "should propagate client IP into OpenRTB device.ip"
        );

        let serialized = serde_json::to_value(&openrtb).expect("should serialize OpenRTB request");
        assert_eq!(
            serialized["device"]["ip"],
            json!("203.0.113.42"),
            "should serialize device.ip when client IP is available"
        );
    }

    #[test]
    fn to_openrtb_omits_debug_flags_when_disabled() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();
        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.test, None,
            "should omit top-level OpenRTB test field when test_mode is disabled"
        );

        let prebid_ext = get_prebid_ext(&openrtb);
        assert_eq!(
            prebid_ext.debug, None,
            "should omit ext.prebid.debug when debug is disabled"
        );
        assert_eq!(
            prebid_ext.returnallbidstatus, None,
            "should omit ext.prebid.returnallbidstatus when debug is disabled"
        );

        let serialized = serde_json::to_value(&openrtb).expect("should serialize OpenRTB request");
        assert!(
            serialized.get("test").is_none(),
            "should not serialize top-level test when test_mode is disabled"
        );

        let prebid = serialized["ext"]["prebid"]
            .as_object()
            .expect("should serialize ext.prebid object");
        assert!(
            !prebid.contains_key("debug"),
            "should not serialize ext.prebid.debug when debug is disabled"
        );
        assert!(
            !prebid.contains_key("returnallbidstatus"),
            "should not serialize ext.prebid.returnallbidstatus when debug is disabled"
        );
    }

    // ========================================================================
    // OpenRTB field enrichment tests
    // ========================================================================

    #[test]
    fn to_openrtb_sets_bidfloor_from_slot_floor_price() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.slots[0].floor_price = Some(1.5);

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let imp = &openrtb.imp[0];

        assert_eq!(imp.bidfloor, Some(1.5), "should set bidfloor from slot");
        assert_eq!(
            imp.bidfloorcur.as_deref(),
            Some("USD"),
            "should set bidfloorcur when floor is present"
        );
    }

    #[test]
    fn to_openrtb_omits_bidfloor_when_no_floor_price() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request(); // floor_price is None

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let imp = &openrtb.imp[0];

        assert_eq!(imp.bidfloor, None, "should omit bidfloor when not set");
        assert_eq!(
            imp.bidfloorcur, None,
            "should omit bidfloorcur when floor not set"
        );
    }

    #[test]
    fn to_openrtb_sets_secure_and_tagid() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let imp = &openrtb.imp[0];

        assert_eq!(imp.secure, Some(true), "should require HTTPS creatives");
        assert_eq!(
            imp.tagid.as_deref(),
            Some("slot-1"),
            "should set tagid from slot id"
        );
    }

    #[test]
    fn to_openrtb_includes_consent_and_gdpr_flag_from_geo() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.user.consent = Some(ConsentContext {
            raw_tc_string: Some("BOtest-consent-string".to_string()),
            gdpr_applies: true,
            ..Default::default()
        });
        // Set device with EU geo so GDPR applies from geo check
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("TestAgent".to_string()),
            ip: None,
            geo: Some(GeoInfo {
                city: "Berlin".to_string(),
                country: "DE".to_string(),
                continent: "EU".to_string(),
                latitude: 52.52,
                longitude: 13.405,
                metro_code: 0,
                region: None,
                asn: None,
            }),
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.user.as_ref().and_then(|u| u.consent.as_deref()),
            Some("BOtest-consent-string"),
            "should forward consent string to user.consent"
        );
        assert_eq!(
            openrtb.regs.as_ref().and_then(|r| r.gdpr),
            Some(true),
            "should set regs.gdpr=true for EU country"
        );
    }

    #[test]
    fn to_openrtb_includes_kv_consent_when_cookies_only_has_no_cookie_to_forward() {
        let mut config = base_config();
        config.consent_forwarding = ConsentForwardingMode::CookiesOnly;
        let provider = PrebidAuctionProvider::new(config);
        let mut auction_request = create_test_auction_request();
        auction_request.user.consent = Some(ConsentContext {
            raw_tc_string: Some("BOkv-backed-consent-string".to_string()),
            raw_us_privacy: Some("1YNN".to_string()),
            gdpr_applies: true,
            source: ConsentSource::KvStore,
            ..Default::default()
        });

        let settings = make_settings();
        let request = build_test_request();
        assert!(
            !request.headers().contains_key(header::COOKIE),
            "test request should not carry a consent cookie to forward"
        );
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.user.as_ref().and_then(|u| u.consent.as_deref()),
            Some("BOkv-backed-consent-string"),
            "cookies_only should fall back to body consent when consent came from KV"
        );
        let regs = openrtb.regs.as_ref().expect("should include consent regs");
        assert_eq!(regs.gdpr, Some(true), "should carry GDPR applicability");
        assert_eq!(
            regs.us_privacy.as_deref(),
            Some("1YNN"),
            "should carry non-cookie consent strings from KV"
        );
    }

    #[test]
    fn to_openrtb_sets_gdpr_true_for_non_eu_country_with_consent() {
        // When geo says non-GDPR but a consent string is present, the consent
        // string is the stronger signal (VPN / carrier IP / geo-DB drift can
        // cause false negatives).
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.user.consent = Some(ConsentContext {
            raw_tc_string: Some("BOtest-consent-string".to_string()),
            gdpr_applies: true,
            ..Default::default()
        });
        // US geo — but consent string overrides
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("TestAgent".to_string()),
            ip: None,
            geo: Some(GeoInfo {
                city: "New York".to_string(),
                country: "US".to_string(),
                continent: "NA".to_string(),
                latitude: 40.7128,
                longitude: -74.006,
                metro_code: 501,
                region: Some("NY".to_string()),
                asn: None,
            }),
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.regs.as_ref().and_then(|r| r.gdpr),
            Some(true),
            "should set regs.gdpr=true when consent string present, even for non-EU geo"
        );
    }

    #[test]
    fn to_openrtb_omits_regs_for_non_eu_country_without_consent() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.user.consent = None;
        // US geo, no consent string — no consent data to forward
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("TestAgent".to_string()),
            ip: None,
            geo: Some(GeoInfo {
                city: "New York".to_string(),
                country: "US".to_string(),
                continent: "NA".to_string(),
                latitude: 40.7128,
                longitude: -74.006,
                metro_code: 501,
                region: Some("NY".to_string()),
                asn: None,
            }),
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert!(
            openrtb.regs.is_none(),
            "should omit regs when no consent context is present"
        );
    }

    #[test]
    fn to_openrtb_falls_back_to_consent_when_no_geo() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.user.consent = Some(ConsentContext {
            raw_tc_string: Some("BOtest-consent-string".to_string()),
            gdpr_applies: true,
            ..Default::default()
        });
        // No device/geo

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.regs.as_ref().and_then(|r| r.gdpr),
            Some(true),
            "should conservatively assume GDPR when geo is absent but consent exists"
        );
    }

    #[test]
    fn to_openrtb_omits_regs_when_no_consent_or_gpc() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request(); // consent=None, no geo

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert!(openrtb.regs.is_none(), "should omit regs entirely");
    }

    #[test]
    fn to_openrtb_sets_gpc_us_privacy() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        // GPC signal is carried via ConsentContext, populated by the
        // consent extraction pipeline from the Sec-GPC header.
        auction_request.user.consent = Some(ConsentContext {
            gpc: true,
            raw_us_privacy: Some(GPC_US_PRIVACY.to_string()),
            ..Default::default()
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let regs = openrtb.regs.as_ref().expect("should have regs");

        assert_eq!(
            regs.us_privacy.as_deref(),
            Some(GPC_US_PRIVACY),
            "should set us_privacy from GPC consent signal"
        );
    }

    // ========================================================================
    // build_regs unit tests
    // ========================================================================

    #[test]
    fn build_regs_returns_none_when_no_consent_context() {
        let result = PrebidAuctionProvider::build_regs(None);
        assert!(
            result.is_none(),
            "should return None when consent context is absent"
        );
    }

    #[test]
    fn build_regs_returns_none_when_consent_context_is_empty() {
        let ctx = ConsentContext::default();
        let result = PrebidAuctionProvider::build_regs(Some(&ctx));
        assert!(
            result.is_none(),
            "should return None when consent context has no actionable data"
        );
    }

    #[test]
    fn build_regs_populates_gpp_and_section_ids() {
        let ctx = ConsentContext {
            raw_gpp_string: Some("DBACNYA~CPXxRfA".to_string()),
            gpp_section_ids: Some(vec![2, 6]),
            ..Default::default()
        };

        let regs = PrebidAuctionProvider::build_regs(Some(&ctx))
            .expect("should produce regs for GPP data");

        assert_eq!(
            regs.gpp.as_deref(),
            Some("DBACNYA~CPXxRfA"),
            "should set top-level regs.gpp"
        );
        assert_eq!(
            regs.gpp_sid,
            vec![2i32, 6i32],
            "should convert gpp_sid u16 to i32"
        );

        // Verify dual-placement in ext
        let ext = regs
            .ext
            .as_ref()
            .expect("should have ext for dual-placement");
        assert_eq!(
            ext.get("gpp").and_then(|v| v.as_str()),
            Some("DBACNYA~CPXxRfA"),
            "should mirror gpp in regs.ext"
        );
        assert_eq!(
            ext.get("gpp_sid"),
            Some(&serde_json::json!([2, 6])),
            "should mirror gpp_sid in regs.ext"
        );
    }

    #[test]
    fn build_regs_sets_gdpr_true_from_jurisdiction() {
        // EU user with GPC but no TCF string — GDPR should still be flagged
        // based on jurisdiction alone.
        let ctx = ConsentContext {
            gpc: true,
            raw_us_privacy: Some("1YYN".to_string()),
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::Gdpr,
            ..Default::default()
        };

        let regs = PrebidAuctionProvider::build_regs(Some(&ctx))
            .expect("should produce regs for GDPR jurisdiction");

        assert_eq!(
            regs.gdpr,
            Some(true),
            "should set gdpr=true from GDPR jurisdiction even without TCF string"
        );
    }

    #[test]
    fn build_regs_omits_gdpr_for_unknown_jurisdiction_without_tcf() {
        // GPC-only request with no geo — jurisdiction is Unknown, no TCF
        // signal. GDPR field should be None (omitted) rather than false.
        let ctx = ConsentContext {
            gpc: true,
            raw_us_privacy: Some("1YYN".to_string()),
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::Unknown,
            ..Default::default()
        };

        let regs = PrebidAuctionProvider::build_regs(Some(&ctx))
            .expect("should produce regs for GPC signal");

        assert!(
            regs.gdpr.is_none(),
            "should omit gdpr when jurisdiction is unknown and no TCF signal exists"
        );
    }

    #[test]
    fn build_regs_sets_gdpr_false_for_non_regulated_jurisdiction() {
        // Non-EU, non-US-state user with a US privacy string.
        let ctx = ConsentContext {
            raw_us_privacy: Some("1NNN".to_string()),
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..Default::default()
        };

        let regs = PrebidAuctionProvider::build_regs(Some(&ctx))
            .expect("should produce regs for us_privacy");

        assert_eq!(
            regs.gdpr,
            Some(false),
            "should set gdpr=false for non-regulated jurisdiction"
        );
    }

    #[test]
    fn build_regs_dual_placement_mirrors_all_fields() {
        let ctx = ConsentContext {
            gdpr_applies: true,
            raw_us_privacy: Some("1YNN".to_string()),
            raw_gpp_string: Some("DBACNYA~CPXxRfA".to_string()),
            gpp_section_ids: Some(vec![7]),
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::Gdpr,
            ..Default::default()
        };

        let regs = PrebidAuctionProvider::build_regs(Some(&ctx))
            .expect("should produce regs with full consent data");

        let ext = regs
            .ext
            .as_ref()
            .expect("should have ext for dual-placement");

        // Top-level uses bool (OpenRTB proto); ext uses integer (Prebid convention).
        // Both serialize to the same JSON value (1).
        assert_eq!(regs.gdpr, Some(true), "top-level gdpr should be true");
        assert_eq!(
            ext.get("gdpr").and_then(serde_json::Value::as_u64),
            Some(1),
            "ext.gdpr should be 1 (mirroring top-level)"
        );

        assert_eq!(regs.us_privacy.as_deref(), Some("1YNN"));
        assert_eq!(
            ext.get("us_privacy").and_then(|v| v.as_str()),
            Some("1YNN"),
            "ext.us_privacy should mirror top-level"
        );

        assert_eq!(regs.gpp.as_deref(), Some("DBACNYA~CPXxRfA"));
        assert_eq!(
            ext.get("gpp").and_then(|v| v.as_str()),
            Some("DBACNYA~CPXxRfA"),
            "ext.gpp should mirror top-level"
        );

        assert_eq!(regs.gpp_sid, vec![7i32]);
        assert_eq!(
            ext.get("gpp_sid"),
            Some(&serde_json::json!([7])),
            "ext.gpp_sid should mirror top-level"
        );
    }

    #[test]
    fn build_regs_ext_omitted_when_all_fields_none() {
        // gdpr_applies=true but no strings — only gdpr flag is set.
        // RegsExt should serialize to an object with only gdpr, which is
        // non-empty, so ext should still be present.
        let ctx = ConsentContext {
            gdpr_applies: true,
            ..Default::default()
        };

        let regs = PrebidAuctionProvider::build_regs(Some(&ctx))
            .expect("should produce regs for gdpr_applies");

        assert_eq!(regs.gdpr, Some(true));
        // ext should exist because RegsExt has gdpr=Some(1)
        let ext = regs.ext.as_ref().expect("should have ext when gdpr is set");
        assert_eq!(ext.get("gdpr").and_then(serde_json::Value::as_u64), Some(1));
    }

    #[test]
    fn to_openrtb_sets_dnt_from_header() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("TestAgent".to_string()),
            ip: None,
            geo: None,
        });

        let settings = make_settings();
        let mut request = build_test_request();
        request
            .headers_mut()
            .insert("DNT", http::header::HeaderValue::from_static("1"));
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let device = openrtb.device.as_ref().expect("should have device");

        assert_eq!(device.dnt, Some(true), "should set dnt from DNT header");
    }

    #[test]
    fn to_openrtb_sets_language_from_accept_language() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("TestAgent".to_string()),
            ip: None,
            geo: None,
        });

        let settings = make_settings();
        let mut request = build_test_request();
        request.headers_mut().insert(
            "Accept-Language",
            http::header::HeaderValue::from_static("en-US,en;q=0.9,fr;q=0.8"),
        );
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let device = openrtb.device.as_ref().expect("should have device");

        assert_eq!(
            device.language.as_deref(),
            Some("en"),
            "should extract primary ISO-639 language tag (stripped of locale subtag)"
        );
    }

    #[test]
    fn to_openrtb_omits_language_for_empty_accept_language() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("TestAgent".to_string()),
            ip: None,
            geo: None,
        });

        let settings = make_settings();
        let mut request = build_test_request();
        request.headers_mut().insert(
            "Accept-Language",
            http::header::HeaderValue::from_static(""),
        );
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let device = openrtb.device.as_ref().expect("should have device");

        assert_eq!(
            device.language, None,
            "empty Accept-Language header should not produce Some(\"\")"
        );
    }

    #[test]
    fn to_openrtb_drops_imp_with_no_valid_banner_formats() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        // Set dimensions that overflow i32 — they will be filtered out,
        // leaving an empty format list.
        auction_request.slots = vec![AdSlot {
            id: "oversized-slot".to_string(),
            formats: vec![AdFormat {
                media_type: MediaType::Banner,
                width: i32::MAX as u32 + 1,
                height: 250,
            }],
            floor_price: None,
            targeting: HashMap::new(),
            bidders: HashMap::new(),
        }];

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert!(
            openrtb.imp.is_empty(),
            "imp with no valid banner formats should be dropped entirely"
        );
    }

    #[test]
    fn to_openrtb_sets_geo_lat_lon_metro() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("TestAgent".to_string()),
            ip: Some("1.2.3.4".to_string()),
            geo: Some(GeoInfo {
                city: "New York".to_string(),
                country: "US".to_string(),
                continent: "NA".to_string(),
                latitude: 40.7128,
                longitude: -74.006,
                metro_code: 501,
                region: Some("NY".to_string()),
                asn: None,
            }),
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let geo = openrtb
            .device
            .as_ref()
            .and_then(|d| d.geo.as_ref())
            .expect("should have geo");

        assert_eq!(geo.lat, Some(40.7128), "should set latitude");
        assert_eq!(geo.lon, Some(-74.006), "should set longitude");
        assert_eq!(
            geo.metro.as_deref(),
            Some("501"),
            "should set metro (DMA code)"
        );
        assert_eq!(geo.country.as_deref(), Some("US"));
        assert_eq!(geo.city.as_deref(), Some("New York"));
        assert_eq!(geo.region.as_deref(), Some("NY"));
    }

    #[test]
    fn to_openrtb_sets_tmax_and_cur() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.tmax,
            Some(1000),
            "should set tmax from the effective auction context timeout"
        );
        assert_eq!(
            openrtb.cur,
            vec!["USD".to_string()],
            "should set cur to USD"
        );
    }

    #[test]
    fn auction_endpoint_url_appends_path_to_base_origin() {
        let provider = PrebidAuctionProvider::new(base_config());
        assert_eq!(
            provider.auction_endpoint_url(),
            "https://prebid.example/openrtb2/auction",
            "should append /openrtb2/auction to a base origin"
        );
    }

    #[test]
    fn auction_endpoint_url_does_not_double_append_full_endpoint() {
        let mut config = base_config();
        config.server_url = "https://prebid.example/openrtb2/auction".to_string();
        let provider = PrebidAuctionProvider::new(config);
        assert_eq!(
            provider.auction_endpoint_url(),
            "https://prebid.example/openrtb2/auction",
            "should use a full endpoint URL as-is"
        );

        let mut config = base_config();
        config.server_url = "https://prebid.example/openrtb2/auction/".to_string();
        let provider = PrebidAuctionProvider::new(config);
        assert_eq!(
            provider.auction_endpoint_url(),
            "https://prebid.example/openrtb2/auction",
            "should normalize a trailing slash on a full endpoint URL"
        );
    }

    #[test]
    fn to_openrtb_tmax_uses_effective_context_timeout_not_provider_config() {
        // Provider config says 1000ms but the auction budget is only 500ms —
        // PBS must be told the tighter effective deadline, otherwise the edge
        // gives up before PBS responds.
        let config = base_config();
        assert_eq!(config.timeout_ms, 1000, "should start from 1000ms config");
        let provider = PrebidAuctionProvider::new(config);
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let request = build_test_request();
        let context = shared_test_auction_context(&settings, &request, 500);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.tmax,
            Some(500),
            "should set tmax from the effective auction context timeout, not provider config"
        );
    }

    #[test]
    fn to_openrtb_omits_tmax_when_timeout_exceeds_i32_max() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let request = build_test_request();
        let context = shared_test_auction_context(&settings, &request, i32::MAX as u32 + 1);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.tmax, None,
            "should omit tmax when timeout_ms exceeds i32::MAX"
        );
    }

    #[test]
    fn to_openrtb_drops_banner_format_with_out_of_range_dimensions() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.slots[0].formats.push(AdFormat {
            media_type: MediaType::Banner,
            width: i32::MAX as u32 + 1,
            height: 250,
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let formats = &openrtb.imp[0]
            .banner
            .as_ref()
            .expect("should have banner")
            .format;

        assert_eq!(
            formats.len(),
            1,
            "should keep only valid banner formats when one is out of range"
        );
        assert_eq!(formats[0].w, Some(300), "should preserve valid width");
        assert_eq!(formats[0].h, Some(250), "should preserve valid height");
    }

    #[test]
    fn to_openrtb_drops_imp_when_all_banner_formats_exceed_i32_max() {
        // The build-time bound: every banner format's u32 dimensions pass through
        // `to_openrtb_i32`, which omits any value above i32::MAX. When a slot's
        // only format is out of range (here u32::MAX), no valid formats remain, so
        // the whole imp must be dropped rather than emitted with an empty format
        // list — a sizeless imp is unbiddable and would only waste an SSP call.
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.slots[0].formats = vec![AdFormat {
            media_type: MediaType::Banner,
            width: u32::MAX,
            height: u32::MAX,
        }];

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert!(
            openrtb.imp.is_empty(),
            "should drop the imp entirely when every banner format exceeds i32::MAX"
        );
    }

    #[test]
    fn to_openrtb_sets_site_ref_from_referer_header() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let mut request = build_test_request();
        request.headers_mut().insert(
            "Referer",
            http::header::HeaderValue::from_static("https://google.com/search?q=test"),
        );
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let site = openrtb.site.as_ref().expect("should have site");

        assert_eq!(
            site.r#ref.as_deref(),
            Some("https://google.com/search?q=test"),
            "should set site.ref from Referer header"
        );
    }

    #[test]
    fn to_openrtb_sets_site_publisher() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );
        let publisher = openrtb
            .site
            .as_ref()
            .and_then(|s| s.publisher.as_ref())
            .expect("should have site.publisher");

        assert_eq!(
            publisher.domain.as_deref(),
            Some("pub.example"),
            "should set publisher domain"
        );
    }

    #[test]
    fn to_openrtb_includes_eids_from_auction_request() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.user.eids = Some(vec![
            crate::openrtb::Eid {
                source: "liveramp.com".to_owned(),
                uids: vec![crate::openrtb::Uid {
                    id: "LR_xyz".to_owned(),
                    atype: Some(3),
                    ext: None,
                }],
            },
            crate::openrtb::Eid {
                source: "id5-sync.com".to_owned(),
                uids: vec![crate::openrtb::Uid {
                    id: "ID5_abc".to_owned(),
                    atype: Some(1),
                    ext: None,
                }],
            },
        ]);

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        let serialized = serde_json::to_value(&openrtb).expect("should serialize OpenRTB request");
        let ext_eids = &serialized["user"]["ext"]["eids"];
        assert!(ext_eids.is_array(), "should populate user.ext.eids");
        assert_eq!(ext_eids.as_array().unwrap().len(), 2, "should have 2 EIDs");
        assert_eq!(
            ext_eids[0]["source"], "liveramp.com",
            "should include liveramp EID"
        );
        assert_eq!(
            ext_eids[1]["source"], "id5-sync.com",
            "should include id5 EID"
        );
    }

    #[test]
    fn to_openrtb_omits_eids_when_none() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        let serialized = serde_json::to_value(&openrtb).expect("should serialize OpenRTB request");
        assert!(
            serialized["user"]["ext"]["eids"].is_null(),
            "should omit user.ext.eids when no EIDs available"
        );
    }

    #[test]
    fn expand_trusted_server_bidders_uses_per_bidder_map_when_present() {
        let params = json!({
            "bidderParams": {
                "appnexus": { "placementId": 123 },
                "rubicon": { "accountId": "abc" }
            }
        });

        let expanded = expand_trusted_server_bidders(
            &[
                "appnexus".to_string(),
                "rubicon".to_string(),
                "openx".to_string(),
            ],
            &params,
        );

        assert_eq!(
            expanded.get("appnexus"),
            Some(&json!({ "placementId": 123 })),
            "should map appnexus-specific params"
        );
        assert_eq!(
            expanded.get("rubicon"),
            Some(&json!({ "accountId": "abc" })),
            "should map rubicon-specific params"
        );
        assert_eq!(
            expanded.get("openx"),
            Some(&json!({})),
            "should default missing bidder params to empty object"
        );
    }

    #[test]
    fn expand_trusted_server_bidders_falls_back_to_shared_params() {
        let params = json!({ "placementId": 999 });
        let expanded = expand_trusted_server_bidders(
            &["appnexus".to_string(), "rubicon".to_string()],
            &params,
        );

        assert_eq!(
            expanded.get("appnexus"),
            Some(&params),
            "should reuse shared params when bidderParams map is absent"
        );
        assert_eq!(
            expanded.get("rubicon"),
            Some(&params),
            "should reuse shared params when bidderParams map is absent"
        );
    }

    #[test]
    fn routes_with_empty_script_patterns() {
        let mut config = base_config();
        config.script_patterns = vec![];
        let integration = PrebidIntegration::new(config);

        let routes = integration.routes();

        assert_eq!(
            routes.len(),
            1,
            "should keep bundle route when no script patterns configured"
        );
        assert!(
            routes
                .iter()
                .any(|route| route.path == PREBID_BUNDLE_ROUTE && route.method == Method::GET),
            "should register the bundle route"
        );
    }

    #[test]
    fn bounded_prebid_error_text_normalizes_control_characters_and_whitespace() {
        let message = bounded_prebid_error_text("\n invalid\trequest\0  payload \r\n", 100)
            .expect("should extract bounded text");

        assert_eq!(
            message.text, "invalid request payload",
            "should make upstream text safe for one-line responses and logs"
        );
        assert!(!message.truncated, "should retain the complete message");
    }

    #[test]
    fn prebid_body_preview_truncates_to_character_limit() {
        let body = "x".repeat(PREBID_ERROR_BODY_PREVIEW_CHARS + 100);

        let preview = prebid_body_preview(body.as_bytes()).expect("should build body preview");

        assert_eq!(
            preview.text.chars().count(),
            PREBID_ERROR_BODY_PREVIEW_CHARS,
            "should cap the upstream body preview"
        );
        assert!(preview.truncated, "should report body preview truncation");
    }

    #[test]
    fn prebid_body_preview_handles_non_utf8_lossily() {
        let preview =
            prebid_body_preview(&[b'o', b'k', 0xff, b'!']).expect("should build body preview");

        assert_eq!(
            preview.text, "ok\u{fffd}!",
            "should replace invalid UTF-8 bytes without panicking"
        );
        assert!(!preview.truncated, "should retain the complete preview");
    }

    #[test]
    fn prebid_body_preview_ignores_bytes_after_bounded_slice() {
        let mut body = vec![b'x'; PREBID_ERROR_BODY_PREVIEW_BYTES];
        body.extend_from_slice(&[0xff, b't', b'a', b'i', b'l']);

        let preview = prebid_body_preview(&body).expect("should build body preview");

        assert_eq!(
            preview.text.chars().count(),
            PREBID_ERROR_BODY_PREVIEW_CHARS,
            "should keep the log preview capped"
        );
        assert!(
            !preview.text.contains('\u{fffd}') && !preview.text.contains("tail"),
            "should not process bytes beyond the bounded preview slice"
        );
        assert!(preview.truncated, "should report bounded-slice truncation");
    }

    #[test]
    fn prebid_body_preview_bounds_partial_utf8_at_byte_boundary() {
        let mut body = vec![b'a'; PREBID_ERROR_BODY_PREVIEW_BYTES - 1];
        body.extend_from_slice("\u{2603}".as_bytes());
        body.extend_from_slice(b"tail");

        let preview = prebid_body_preview(&body).expect("should build body preview");

        assert_eq!(
            preview.text.chars().count(),
            PREBID_ERROR_BODY_PREVIEW_CHARS,
            "should keep the log preview capped"
        );
        assert!(
            !preview.text.contains("tail"),
            "should not include bytes beyond the bounded preview slice"
        );
        assert!(preview.truncated, "should report partial-body truncation");
    }

    #[test]
    fn extract_prebid_error_message_reads_nested_json_message() {
        let body = br#"{
            "errors": {
                "exampleBidder": [{"code": 1, "message": " invalid\nrequest "}]
            }
        }"#;

        let message = extract_prebid_error_message(body, Some("application/json"))
            .expect("should extract nested JSON error message");

        assert_eq!(message.text, "invalid request");
        assert!(!message.truncated, "should retain the complete message");
    }

    #[test]
    fn extract_prebid_error_message_reads_plain_text() {
        let message = extract_prebid_error_message(
            b" request rejected\r\nby Prebid Server ",
            Some("Text/Plain; charset=utf-8"),
        )
        .expect("should extract plain-text error message");

        assert_eq!(message.text, "request rejected by Prebid Server");
        assert!(!message.truncated, "should retain the complete message");
    }

    #[test]
    fn extract_prebid_error_message_rejects_html_and_unknown_json_fields() {
        assert!(
            extract_prebid_error_message(
                b"<html><body>internal proxy error</body></html>",
                Some("text/plain"),
            )
            .is_none(),
            "should not expose HTML error pages"
        );

        for body in [
            br#"{"resolvedrequest":{"account":"internal"}}"#.as_slice(),
            br#"{"errors":{"resolvedrequest":{"account":"internal"}}}"#.as_slice(),
            br#""internal""#.as_slice(),
            br#"["internal"]"#.as_slice(),
        ] {
            assert!(
                extract_prebid_error_message(body, Some("application/json")).is_none(),
                "should only expose strings associated with allowlisted JSON error fields"
            );
        }
    }

    #[test]
    fn extract_prebid_error_message_truncates_public_message() {
        let body = serde_json::to_vec(&json!({
            "message": "x".repeat(PREBID_PUBLIC_ERROR_MESSAGE_CHARS + 100),
        }))
        .expect("should serialize test error response");

        let message = extract_prebid_error_message(&body, Some("application/json"))
            .expect("should extract JSON error message");

        assert_eq!(
            message.text.chars().count(),
            PREBID_PUBLIC_ERROR_MESSAGE_CHARS,
            "should cap the browser-visible upstream message"
        );
        assert!(message.truncated, "should report public message truncation");
    }

    #[test]
    fn non_success_prebid_response_always_includes_safe_http_metadata() {
        let provider = PrebidAuctionProvider::new(base_config());
        let response = prebid_platform_response(
            StatusCode::BAD_REQUEST,
            Some("application/json"),
            br#"{"message":"request details should remain hidden"}"#.to_vec(),
        );

        let auction_response = futures::executor::block_on(provider.parse_response(response, 42))
            .expect("should convert upstream HTTP failure to auction response");

        assert_eq!(
            auction_response.status,
            crate::auction::types::BidStatus::Error
        );
        assert_eq!(
            auction_response.metadata["error_type"],
            json!(ERROR_TYPE_HTTP_STATUS)
        );
        assert_eq!(auction_response.metadata["http_status"], json!(400));
        assert_eq!(
            auction_response.metadata["message"],
            json!("Prebid Server returned HTTP 400")
        );
        assert!(
            !auction_response.metadata.contains_key("upstream_message"),
            "should hide upstream text when Prebid debug is disabled"
        );
    }

    #[test]
    fn debug_non_success_prebid_response_includes_bounded_upstream_message() {
        let mut config = base_config();
        config.debug = true;
        let provider = PrebidAuctionProvider::new(config);
        let response = prebid_platform_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("application/json; charset=utf-8"),
            br#"{"error":{"message":"imp[0] has no valid bidders"}}"#.to_vec(),
        );
        let settings = make_settings();
        let http_request = build_test_request();
        let context = create_test_auction_context(&settings, &http_request);
        let auction_request = create_test_auction_request();

        let auction_response = futures::executor::block_on(provider.parse_response_with_context(
            response,
            66,
            &auction_request,
            &context,
        ))
        .expect("should convert upstream HTTP failure to debug auction response");

        assert_eq!(auction_response.metadata["http_status"], json!(422));
        assert_eq!(
            auction_response.metadata["upstream_message"],
            json!("imp[0] has no valid bidders")
        );
        assert_eq!(
            auction_response.metadata["upstream_message_truncated"],
            json!(false)
        );
    }

    fn make_auction_request(slots: Vec<AdSlot>) -> AuctionRequest {
        AuctionRequest {
            id: "test-auction-1".to_string(),
            slots,
            publisher: PublisherInfo {
                domain: "example.com".to_string(),
                page_url: Some("https://example.com/page".to_string()),
            },
            user: UserInfo {
                id: Some("synth-123".to_string()),
                consent: None,
                eids: None,
            },
            device: Some(DeviceInfo {
                user_agent: Some("test-agent".to_string()),
                ip: None,
                geo: None,
            }),
            site: None,
            context: HashMap::new(),
        }
    }

    fn make_slot(id: &str, bidders: HashMap<String, Json>) -> AdSlot {
        AdSlot {
            id: id.to_string(),
            formats: vec![AdFormat {
                media_type: MediaType::Banner,
                width: 300,
                height: 250,
            }],
            floor_price: None,
            targeting: HashMap::new(),
            bidders,
        }
    }

    fn call_to_openrtb(
        config: PrebidIntegrationConfig,
        request: &AuctionRequest,
    ) -> OpenRtbRequest {
        use crate::platform::test_support::noop_services;
        let provider = PrebidAuctionProvider::new(config);
        let settings = make_settings();
        let http_req = http::Request::builder()
            .method(http::Method::POST)
            .uri("https://example.com/auction")
            .body(EdgeBody::empty())
            .expect("should build request");
        let services = noop_services();
        let context = AuctionContext {
            settings: &settings,
            request: &http_req,
            timeout_ms: 1000,
            provider_responses: None,
            services: &services,
        };
        let request_info = make_request_info(&context);
        provider.to_openrtb(request, &context, None, request_info)
    }

    fn bidder_params(ortb: &OpenRtbRequest) -> &serde_json::Map<String, Json> {
        let ext = ortb.imp[0].ext.as_ref().expect("should have imp ext");
        ext.get("prebid")
            .and_then(|p| p.get("bidder"))
            .and_then(|b| b.as_object())
            .expect("should have prebid.bidder in imp ext")
    }

    /// Typed helper to extract `ext.prebid` from an `OpenRTB` request,
    /// deserialising into [`PrebidExt`] so test assertions catch field name
    /// typos at compile time.
    fn get_prebid_ext(req: &OpenRtbRequest) -> PrebidExt {
        let ext = req.ext.as_ref().expect("should have request ext");
        serde_json::from_value(ext["prebid"].clone()).expect("should deserialise ext.prebid")
    }

    // ========================================================================
    // bid_param_overrides tests
    // ========================================================================

    #[test]
    fn bidder_param_override_replaces_and_merges_client_params() {
        let config = parse_prebid_toml(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
bidders = ["criteo"]

[integrations.prebid.bid_param_overrides.criteo]
networkId = 99999
pubid = "server-pub"
"#,
        );

        let slot = make_ts_slot(
            "ad-header-0",
            &json!({
                "criteo": {
                    "networkId": 11111,
                    "pubid": "client-pub",
                    "keep": "present"
                }
            }),
            None,
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        let params = bidder_params(&ortb);

        assert_eq!(
            params["criteo"]["networkId"], 99999,
            "override should replace the client-side networkId"
        );
        assert_eq!(
            params["criteo"]["pubid"], "server-pub",
            "override should replace the client-side pubid"
        );
        assert_eq!(
            params["criteo"]["keep"], "present",
            "override should preserve unrelated bidder params"
        );
    }

    #[test]
    fn bidder_param_override_replaces_nested_objects() {
        let config = parse_prebid_toml(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
bidders = ["appnexus"]

[[integrations.prebid.bid_param_override_rules]]
when = { bidder = "appnexus" }
set = { keywords = { genre = "news" } }
"#,
        );

        // Client sends a nested `keywords` object; shallow merge replaces the
        // entire `keywords` value with the override object.
        let slot = make_ts_slot(
            "ad-header-0",
            &json!({
                "appnexus": {
                    "placementId": 12345,
                    "keywords": { "sport": "football", "genre": "sports" }
                }
            }),
            None,
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        let params = bidder_params(&ortb);
        let keywords = &params["appnexus"]["keywords"];

        assert_eq!(
            keywords["genre"], "news",
            "override should replace the client-side keywords object"
        );
        assert_eq!(
            keywords["sport"],
            Json::Null,
            "shallow merge should replace the entire keywords object, not preserve stale sub-keys"
        );
        assert_eq!(
            params["appnexus"]["placementId"], 12345,
            "shallow merge should preserve unrelated top-level bidder params"
        );
    }

    // ========================================================================
    // bid_param_zone_overrides tests
    // ========================================================================

    /// Helper: build a slot whose bidders entry is a trustedServer payload
    /// with per-bidder params and an optional zone.
    fn make_ts_slot(id: &str, bidder_params: &Json, zone: Option<&str>) -> AdSlot {
        let mut ts_params = json!({ BIDDER_PARAMS_KEY: bidder_params });
        if let Some(z) = zone {
            ts_params[ZONE_KEY] = json!(z);
        }
        make_slot(
            id,
            HashMap::from([(TRUSTED_SERVER_BIDDER.to_string(), ts_params)]),
        )
    }

    #[test]
    fn zone_override_replaces_placement_id() {
        let mut config = base_config();
        config.bidders = vec!["kargo".to_string()];
        config.bid_param_zone_overrides.insert(
            "kargo".to_string(),
            HashMap::from([(
                "header".to_string(),
                json_object(json!({ "placementId": "s2s_header_id" })),
            )]),
        );

        let slot = make_ts_slot(
            "ad-header-0",
            &json!({ "kargo": { "placementId": "client_side_123" } }),
            Some("header"),
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        assert_eq!(
            bidder_params(&ortb)["kargo"]["placementId"],
            "s2s_header_id",
            "zone override should replace the client-side placementId"
        );
    }

    #[test]
    fn zone_override_noop_for_unknown_zone() {
        let mut config = base_config();
        config.bidders = vec!["kargo".to_string()];
        config.bid_param_zone_overrides.insert(
            "kargo".to_string(),
            HashMap::from([(
                "header".to_string(),
                json_object(json!({ "placementId": "zone_header_id" })),
            )]),
        );

        // Zone "sidebar" is NOT in the zone overrides map
        let slot = make_ts_slot(
            "ad-sidebar-0",
            &json!({ "kargo": { "placementId": "client_123" } }),
            Some("sidebar"),
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        assert_eq!(
            bidder_params(&ortb)["kargo"]["placementId"],
            "client_123",
            "unrecognised zone should pass through original params"
        );
    }

    #[test]
    fn zone_override_noop_when_no_zone() {
        let mut config = base_config();
        config.bidders = vec!["kargo".to_string()];
        config.bid_param_zone_overrides.insert(
            "kargo".to_string(),
            HashMap::from([(
                "header".to_string(),
                json_object(json!({ "placementId": "zone_header_id" })),
            )]),
        );

        // No zone in the trustedServer params
        let slot = make_ts_slot(
            "slot1",
            &json!({ "kargo": { "placementId": "client_123" } }),
            None,
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        assert_eq!(
            bidder_params(&ortb)["kargo"]["placementId"],
            "client_123",
            "missing zone should pass through original params"
        );
    }

    #[test]
    fn zone_override_only_affects_configured_bidders() {
        let mut config = base_config();
        config.bidders = vec!["kargo".to_string(), "rubicon".to_string()];
        config.bid_param_zone_overrides.insert(
            "kargo".to_string(),
            HashMap::from([(
                "header".to_string(),
                json_object(json!({ "placementId": "s2s_header_id" })),
            )]),
        );

        let slot = make_ts_slot(
            "ad-header-0",
            &json!({
                "kargo": { "placementId": "client_kargo" },
                "rubicon": { "accountId": 100 }
            }),
            Some("header"),
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        let params = bidder_params(&ortb);
        assert_eq!(
            params["kargo"]["placementId"], "s2s_header_id",
            "kargo should get zone override"
        );
        assert_eq!(
            params["rubicon"]["accountId"], 100,
            "rubicon should be untouched"
        );
    }

    #[test]
    fn zone_override_merges_with_existing_params() {
        let mut config = base_config();
        config.bidders = vec!["kargo".to_string()];
        config.bid_param_zone_overrides.insert(
            "kargo".to_string(),
            HashMap::from([(
                "header".to_string(),
                json_object(json!({ "placementId": "s2s_header" })),
            )]),
        );

        // Client sends extra field alongside placementId
        let slot = make_ts_slot(
            "ad-header-0",
            &json!({ "kargo": { "placementId": "client_123", "extra": "keep_me" } }),
            Some("header"),
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        let params = bidder_params(&ortb);
        let kargo = &params["kargo"];
        assert_eq!(
            kargo["placementId"], "s2s_header",
            "overridden field should have the zone value"
        );
        assert_eq!(
            kargo["extra"], "keep_me",
            "non-overridden fields should be preserved"
        );
    }

    #[test]
    fn zone_overrides_config_parsing_from_toml() {
        let config = parse_prebid_toml(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"

[integrations.prebid.bid_param_zone_overrides.kargo]
header = {placementId = "_s2sHeader"}
in_content = {placementId = "_s2sContent"}
fixed_bottom = {placementId = "_s2sBottom"}
"#,
        );

        let kargo_zones = &config.bid_param_zone_overrides["kargo"];
        assert_eq!(kargo_zones.len(), 3, "should have three zone entries");
        assert_eq!(
            kargo_zones["header"]["placementId"], "_s2sHeader",
            "should parse header zone"
        );
        assert_eq!(
            kargo_zones["in_content"]["placementId"], "_s2sContent",
            "should parse in_content zone"
        );
        assert_eq!(
            kargo_zones["fixed_bottom"]["placementId"], "_s2sBottom",
            "should parse fixed_bottom zone"
        );
    }

    #[test]
    fn bid_param_override_rules_config_parsing_from_toml() {
        let config = parse_prebid_toml(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "_s2sHeader", extra = "x" }
"#,
        );

        assert_eq!(
            config.bid_param_override_rules.len(),
            1,
            "should parse one canonical override rule"
        );
        assert_eq!(
            config.bid_param_override_rules[0].when.bidder.as_deref(),
            Some("kargo"),
            "should parse bidder matcher"
        );
        assert_eq!(
            config.bid_param_override_rules[0].when.zone.as_deref(),
            Some("header"),
            "should parse zone matcher"
        );
        assert_eq!(
            config.bid_param_override_rules[0].set["placementId"], "_s2sHeader",
            "should parse canonical set object"
        );
    }

    #[test]
    fn bid_param_overrides_config_rejects_non_object_bidder_value() {
        let result = parse_prebid_toml_result(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"

[integrations.prebid.bid_param_overrides]
criteo = "not-an-object"
"#,
        );

        assert!(result.is_err(), "should reject non-object bidder overrides");
    }

    #[test]
    fn zone_overrides_config_rejects_non_object_zone_value() {
        let result = parse_prebid_toml_result(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"

[integrations.prebid.bid_param_zone_overrides.kargo]
header = "not-an-object"
"#,
        );

        assert!(result.is_err(), "should reject non-object zone overrides");
    }

    #[test]
    fn bid_param_override_rules_config_rejects_non_object_set() {
        let result = parse_prebid_toml_result(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
set = "not-an-object"
"#,
        );

        assert!(
            result.is_err(),
            "should reject canonical override rules with non-object sets"
        );
    }

    #[test]
    fn explicit_bid_param_override_rule_applies_for_bidder_and_zone() {
        let config = parse_prebid_toml(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
bidders = ["kargo"]

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "rule_header", keep = "server" }
"#,
        );

        let slot = make_ts_slot(
            "ad-header-0",
            &json!({
                "kargo": {
                    "placementId": "client",
                    "keep": "client",
                    "other": "present"
                }
            }),
            Some("header"),
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        let params = bidder_params(&ortb);

        assert_eq!(
            params["kargo"]["placementId"], "rule_header",
            "canonical rule should override placementId"
        );
        assert_eq!(
            params["kargo"]["keep"], "server",
            "canonical rule should replace overlapping keys"
        );
        assert_eq!(
            params["kargo"]["other"], "present",
            "canonical rule should preserve unrelated keys"
        );
    }

    #[test]
    fn explicit_bid_param_override_rule_wins_over_zone_compatibility_rule() {
        let config = parse_prebid_toml(
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
bidders = ["kargo"]

[integrations.prebid.bid_param_zone_overrides.kargo]
header = { placementId = "compat_header" }

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "explicit_header" }
"#,
        );

        let slot = make_ts_slot(
            "ad-header-0",
            &json!({ "kargo": { "placementId": "client" } }),
            Some("header"),
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        assert_eq!(
            bidder_params(&ortb)["kargo"]["placementId"],
            "explicit_header",
            "canonical rules should run after compatibility-derived rules"
        );
    }

    // ========================================================================
    // BidParamOverrideEngine unit tests
    // ========================================================================

    mod bid_param_override_engine {
        use super::*;

        fn empty_params() -> Json {
            Json::Object(serde_json::Map::new())
        }

        fn params_with(key: &str, value: Json) -> Json {
            let mut map = serde_json::Map::new();
            map.insert(key.to_string(), value);
            Json::Object(map)
        }

        fn rule_signature(rule: &CompiledBidParamOverrideRule) -> String {
            format!(
                "{}:{}",
                rule.bidder.as_deref().unwrap_or("*"),
                rule.zone.as_deref().unwrap_or("*")
            )
        }

        #[test]
        fn engine_normalizes_static_compatibility_rule() {
            let mut config = base_config();
            config.bid_param_overrides.insert(
                "criteo".to_string(),
                json_object(json!({ "networkId": 99 })),
            );

            let engine = BidParamOverrideEngine::try_from_config(&config)
                .expect("should compile static compatibility overrides");

            assert_eq!(engine.rules.len(), 1, "should compile one static rule");
            assert_eq!(
                engine.rules[0].bidder.as_deref(),
                Some("criteo"),
                "should set bidder matcher"
            );
            assert_eq!(
                engine.rules[0].zone, None,
                "should not set zone matcher for static overrides"
            );
            assert_eq!(
                engine.rules[0].set.get("networkId"),
                Some(&json!(99)),
                "should preserve set object"
            );
        }

        #[test]
        fn engine_normalizes_zone_compatibility_rule() {
            let mut config = base_config();
            config.bid_param_zone_overrides.insert(
                "kargo".to_string(),
                HashMap::from([(
                    "header".to_string(),
                    json_object(json!({ "placementId": "zone-header" })),
                )]),
            );

            let engine = BidParamOverrideEngine::try_from_config(&config)
                .expect("should compile zone compatibility overrides");

            assert_eq!(engine.rules.len(), 1, "should compile one zone rule");
            assert_eq!(
                engine.rules[0].bidder.as_deref(),
                Some("kargo"),
                "should set bidder matcher"
            );
            assert_eq!(
                engine.rules[0].zone.as_deref(),
                Some("header"),
                "should set zone matcher"
            );
            assert_eq!(
                engine.rules[0].set.get("placementId"),
                Some(&json!("zone-header")),
                "should preserve zone set object"
            );
        }

        #[test]
        fn engine_applies_bidder_only_rule() {
            let mut config = base_config();
            config.bid_param_overrides.insert(
                "criteo".to_string(),
                json_object(json!({ "networkId": 42 })),
            );

            let engine = BidParamOverrideEngine::try_from_config(&config)
                .expect("should compile bidder-only override");
            let mut params = empty_params();

            engine.apply(
                BidParamOverrideFacts {
                    bidder: "criteo",
                    zone: Some("header"),
                },
                &mut params,
            );

            assert_eq!(
                params["networkId"], 42,
                "bidder-only override should apply regardless of zone"
            );
        }

        #[test]
        fn engine_applies_bidder_and_zone_rule() {
            let mut config = base_config();
            config.bid_param_zone_overrides.insert(
                "kargo".to_string(),
                HashMap::from([(
                    "header".to_string(),
                    json_object(json!({ "placementId": "s2s_h" })),
                )]),
            );

            let engine = BidParamOverrideEngine::try_from_config(&config)
                .expect("should compile bidder-and-zone override");
            let mut params = params_with("placementId", json!("client"));
            engine.apply(
                BidParamOverrideFacts {
                    bidder: "kargo",
                    zone: Some("header"),
                },
                &mut params,
            );
            assert_eq!(
                params["placementId"], "s2s_h",
                "bidder-and-zone override should apply when both facts match"
            );
        }

        #[test]
        fn engine_noop_when_facts_do_not_match() {
            let mut config = base_config();
            config.bid_param_zone_overrides.insert(
                "kargo".to_string(),
                HashMap::from([(
                    "header".to_string(),
                    json_object(json!({ "placementId": "s2s_h" })),
                )]),
            );

            let engine = BidParamOverrideEngine::try_from_config(&config)
                .expect("should compile unmatched override");
            let mut params = params_with("placementId", json!("client"));

            engine.apply(
                BidParamOverrideFacts {
                    bidder: "kargo",
                    zone: Some("sidebar"),
                },
                &mut params,
            );

            assert_eq!(
                params["placementId"], "client",
                "should leave params unchanged when facts do not match"
            );
        }

        #[test]
        fn merge_reports_skip_for_non_object_params() {
            let mut params = json!("client-string");
            let override_obj = json_object(json!({ "placementId": "server" }));

            let merged = merge_bidder_param_object(&mut params, &override_obj);

            assert!(
                !merged,
                "should report skip when bidder params are not an object"
            );
            assert_eq!(
                params,
                json!("client-string"),
                "should leave non-object bidder params unchanged"
            );
        }

        #[test]
        fn engine_apply_is_noop_for_non_object_params() {
            let mut config = base_config();
            config.bid_param_overrides.insert(
                "criteo".to_string(),
                json_object(json!({ "networkId": 42 })),
            );

            let engine = BidParamOverrideEngine::try_from_config(&config).expect("should compile");
            let mut params = json!("client-string");

            engine.apply(
                BidParamOverrideFacts {
                    bidder: "criteo",
                    zone: None,
                },
                &mut params,
            );

            assert_eq!(
                params,
                json!("client-string"),
                "should leave non-object params unchanged when a matching rule exists"
            );
        }

        #[test]
        fn engine_applies_later_rule_last_write_wins() {
            let mut config = base_config();
            config.bid_param_overrides.insert(
                "kargo".to_string(),
                json_object(json!({ "placementId": "compat" })),
            );
            config.bid_param_override_rules.push(BidParamOverrideRule {
                when: BidParamOverrideWhen {
                    bidder: Some("kargo".to_string()),
                    zone: Some("header".to_string()),
                },
                set: json_object(json!({ "placementId": "explicit", "extra": "x" })),
            });

            let engine = BidParamOverrideEngine::try_from_config(&config)
                .expect("should compile ordered overrides");
            let mut params = params_with("keep", json!("yes"));

            engine.apply(
                BidParamOverrideFacts {
                    bidder: "kargo",
                    zone: Some("header"),
                },
                &mut params,
            );

            assert_eq!(
                params["placementId"], "explicit",
                "later canonical rule should override earlier compatibility rule"
            );
            assert_eq!(
                params["extra"], "x",
                "should merge additional explicit keys"
            );
            assert_eq!(params["keep"], "yes", "should preserve unrelated params");
        }

        #[test]
        fn merged_rule_indices_preserve_declaration_order() {
            let wildcard_indices = [0, 3, 6];
            let bidder_indices = [1, 2, 5, 7];

            let actual =
                merged_rule_indices(&wildcard_indices, Some(&bidder_indices)).collect::<Vec<_>>();

            assert_eq!(
                actual,
                vec![0, 1, 2, 3, 5, 6, 7],
                "merged indices should preserve global declaration order"
            );
        }

        #[test]
        fn compile_rule_trims_matcher_whitespace() {
            let rule = BidParamOverrideRule {
                when: BidParamOverrideWhen {
                    bidder: Some(" kargo ".to_string()),
                    zone: Some(" header ".to_string()),
                },
                set: json_object(json!({ "placementId": "trimmed" })),
            };

            let compiled = CompiledBidParamOverrideRule::try_from(rule)
                .expect("should compile rule with surrounding whitespace");

            assert_eq!(
                compiled.bidder.as_deref(),
                Some("kargo"),
                "should store trimmed bidder matcher"
            );
            assert_eq!(
                compiled.zone.as_deref(),
                Some("header"),
                "should store trimmed zone matcher"
            );
            assert!(
                compiled.matches(BidParamOverrideFacts {
                    bidder: "kargo",
                    zone: Some("header"),
                }),
                "trimmed matchers should match normalized request facts"
            );
        }

        #[test]
        fn engine_compiles_compatibility_rules_in_sorted_matcher_order() {
            let expected = [
                "alpha:*",
                "beta:*",
                "gamma:*",
                "kargo:footer",
                "kargo:header",
                "openx:sidebar",
            ];

            for iteration in 0..16 {
                let mut config = base_config();
                config.bid_param_overrides = HashMap::from([
                    ("gamma".to_string(), json_object(json!({ "networkId": 3 }))),
                    ("alpha".to_string(), json_object(json!({ "networkId": 1 }))),
                    ("beta".to_string(), json_object(json!({ "networkId": 2 }))),
                ]);
                config.bid_param_zone_overrides = HashMap::from([
                    (
                        "openx".to_string(),
                        HashMap::from([(
                            "sidebar".to_string(),
                            json_object(json!({ "placementId": "openx-sidebar" })),
                        )]),
                    ),
                    (
                        "kargo".to_string(),
                        HashMap::from([
                            (
                                "header".to_string(),
                                json_object(json!({ "placementId": "kargo-header" })),
                            ),
                            (
                                "footer".to_string(),
                                json_object(json!({ "placementId": "kargo-footer" })),
                            ),
                        ]),
                    ),
                ]);

                let engine = BidParamOverrideEngine::try_from_config(&config)
                    .expect("should compile compatibility overrides");
                let actual = engine.rules.iter().map(rule_signature).collect::<Vec<_>>();

                assert_eq!(
                    actual,
                    expected,
                    "compatibility rules should compile in sorted matcher order on iteration {iteration}"
                );
            }
        }

        #[test]
        fn compile_rule_rejects_empty_when() {
            let rule = BidParamOverrideRule {
                when: BidParamOverrideWhen::default(),
                set: json_object(json!({ "placementId": "x" })),
            };

            let result = CompiledBidParamOverrideRule::try_from(rule);
            assert!(result.is_err(), "should reject empty when");
        }

        #[test]
        fn compile_rule_rejects_empty_object_set() {
            let rule = BidParamOverrideRule {
                when: BidParamOverrideWhen {
                    bidder: Some("kargo".to_string()),
                    zone: None,
                },
                set: serde_json::Map::new(),
            };

            let result = CompiledBidParamOverrideRule::try_from(rule);
            assert!(result.is_err(), "should reject empty set object");
        }
    }

    #[test]
    fn enrich_response_metadata_attaches_always_on_fields() {
        let provider = PrebidAuctionProvider::new(base_config());

        let response_json = json!({
            "seatbid": [{
                "seat": "kargo",
                "bid": [{
                    "impid": "slot-1",
                    "price": 2.50,
                    "adm": "<div>ad</div>"
                }]
            }],
            "ext": {
                "responsetimemillis": {
                    "kargo": 98,
                    "appnexus": 0,
                    "ix": 120
                },
                "errors": {
                    "openx": [{"code": 1, "message": "timeout"}]
                },
                "warnings": {
                    "kargo": [{"code": 10, "message": "bid floor"}]
                },
                "debug": {
                    "httpcalls": {"kargo": []}
                },
                "prebid": {
                    "bidstatus": [{"bidder": "kargo", "status": "bid"}]
                }
            }
        });

        let mut auction_response = provider.parse_openrtb_response(&response_json, 150);
        provider.enrich_response_metadata(&response_json, &mut auction_response);

        assert_eq!(auction_response.bids.len(), 1, "should parse one bid");

        // Always-on fields should be present.
        let rtm = auction_response
            .metadata
            .get("responsetimemillis")
            .expect("should have responsetimemillis");
        assert_eq!(rtm["kargo"], 98);
        assert_eq!(rtm["appnexus"], 0);
        assert_eq!(rtm["ix"], 120);

        let errors = auction_response
            .metadata
            .get("errors")
            .expect("should have errors");
        assert_eq!(errors["openx"][0]["code"], 1);

        let warnings = auction_response
            .metadata
            .get("warnings")
            .expect("should have warnings");
        assert_eq!(warnings["kargo"][0]["code"], 10);

        // Debug-gated fields should NOT be present (base_config has debug: false).
        assert!(
            !auction_response.metadata.contains_key("debug"),
            "should not include debug when config.debug is false"
        );
        assert!(
            !auction_response.metadata.contains_key("bidstatus"),
            "should not include bidstatus when config.debug is false"
        );
    }

    #[test]
    fn enrich_response_metadata_includes_debug_fields_when_enabled() {
        let mut config = base_config();
        config.debug = true;
        let provider = PrebidAuctionProvider::new(config);

        let response_json = json!({
            "seatbid": [],
            "ext": {
                "responsetimemillis": {"kargo": 50},
                "debug": {
                    "httpcalls": {"kargo": [{"uri": "https://pbs.example/bid", "status": 200}]},
                    "resolvedrequest": {"id": "resolved-123"}
                },
                "prebid": {
                    "bidstatus": [
                        {"bidder": "kargo", "status": "bid"},
                        {"bidder": "openx", "status": "timeout"}
                    ]
                }
            }
        });

        let mut auction_response = provider.parse_openrtb_response(&response_json, 100);
        provider.enrich_response_metadata(&response_json, &mut auction_response);

        // Always-on field should still be present.
        assert!(
            auction_response.metadata.contains_key("responsetimemillis"),
            "should have responsetimemillis"
        );

        // Debug-gated fields should now be present.
        let debug = auction_response
            .metadata
            .get("debug")
            .expect("should have debug when config.debug is true");
        assert_eq!(
            debug["httpcalls"]["kargo"][0]["status"], 200,
            "should include httpcalls from PBS debug response"
        );
        assert_eq!(
            debug["resolvedrequest"]["id"], "resolved-123",
            "should include resolvedrequest from PBS debug response"
        );

        let bidstatus = auction_response
            .metadata
            .get("bidstatus")
            .expect("should have bidstatus when config.debug is true");
        let statuses = bidstatus.as_array().expect("bidstatus should be array");
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0]["bidder"], "kargo");
        assert_eq!(statuses[1]["status"], "timeout");
    }

    // ========================================================================
    // PBS stored request tests
    // ========================================================================

    #[test]
    fn to_openrtb_uses_stored_request_when_slot_has_no_pbs_bidder_params() {
        // Slot only has "aps" provider — not a PBS bidder
        let slot = make_slot(
            "atf_sidebar_ad",
            HashMap::from([("aps".to_string(), json!({"slotID": "aps-slot-atf-sidebar"}))]),
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(base_config(), &request);
        let ext = ortb.imp[0].ext.as_ref().expect("should have imp ext");
        let prebid = ext.get("prebid").expect("should have prebid in ext");

        assert!(
            prebid.get("bidder").is_none(),
            "should not send inline bidder params when using stored request"
        );
        assert_eq!(
            prebid["storedrequest"]["id"], "atf_sidebar_ad",
            "should use slot id as stored request id"
        );
    }

    #[test]
    fn to_openrtb_uses_stored_request_when_slot_has_empty_bidders() {
        let slot = make_slot("homepage_header_ad", HashMap::new());
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(base_config(), &request);
        let ext = ortb.imp[0].ext.as_ref().expect("should have imp ext");
        let prebid = ext.get("prebid").expect("should have prebid in ext");

        assert_eq!(
            prebid["storedrequest"]["id"], "homepage_header_ad",
            "should use slot id as stored request id for slot with no bidder map"
        );
    }

    #[test]
    fn to_openrtb_uses_inline_bidder_params_not_stored_request_for_trusted_server_slots() {
        let mut config = base_config();
        config.bidders = vec!["kargo".to_string()];

        let slot = make_ts_slot(
            "in_content_ad",
            &json!({ "kargo": { "placementId": "client_123" } }),
            None,
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(config, &request);
        let ext = ortb.imp[0].ext.as_ref().expect("should have imp ext");
        let prebid = ext.get("prebid").expect("should have prebid in ext");

        assert!(
            prebid.get("storedrequest").is_none(),
            "should not use stored request when inline bidder params are present"
        );
        assert_eq!(
            prebid["bidder"]["kargo"]["placementId"], "client_123",
            "should use inline bidder params from trustedServer expansion"
        );
    }

    #[test]
    fn to_openrtb_skips_aps_key_from_slot_bidders_in_pbs_request() {
        let slot = make_slot(
            "atf_sidebar_ad",
            HashMap::from([("aps".to_string(), json!({"slotID": "aps-slot-atf-sidebar"}))]),
        );
        let request = make_auction_request(vec![slot]);

        let ortb = call_to_openrtb(base_config(), &request);
        let ext = ortb.imp[0].ext.as_ref().expect("should have imp ext");
        let prebid = ext.get("prebid").expect("should have prebid in ext");

        assert!(
            prebid.get("bidder").is_none(),
            "should not forward aps key into PBS imp.ext.prebid.bidder"
        );
    }

    #[test]
    fn register_rejects_invalid_bid_param_override_rule() {
        let toml = format!(
            "{}\n{}",
            TOML_BASE,
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
bidders = ["criteo"]

[[integrations.prebid.bid_param_override_rules]]
when = {}
set = { networkId = 42 }
"#
        );
        let settings = Settings::from_toml(&toml).expect("should parse TOML");
        let result = register(&settings);
        assert!(
            result.is_err(),
            "should fail fast when a canonical rule has no matcher fields"
        );
    }

    #[test]
    fn register_auction_provider_rejects_invalid_bid_param_override_rule() {
        let toml = format!(
            "{}\n{}",
            TOML_BASE,
            r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
bidders = ["criteo"]

[[integrations.prebid.bid_param_override_rules]]
when = {}
set = { networkId = 42 }
"#
        );
        let settings = Settings::from_toml(&toml).expect("should parse TOML");
        let result = register_auction_provider(&settings);
        assert!(
            result.is_err(),
            "should fail fast when a canonical rule has no matcher fields"
        );
    }

    #[test]
    fn parse_bid_extracts_cache_id_from_ext_prebid_cache_bids() {
        let bid_json = serde_json::json!({
            "id": "bid-id-123",
            "impid": "atf_sidebar_ad",
            "price": 1.50,
            "adm": "<div>ad</div>",
            "w": 300,
            "h": 250,
            "ext": {
                "prebid": {
                    "cache": {
                        "bids": {
                            "url": "https://openads.adsrvr.org/cache?uuid=f47447a0-b759-4f2f-9887-af458b79b570",
                            "cacheId": "f47447a0-b759-4f2f-9887-af458b79b570"
                        }
                    }
                }
            }
        });
        let provider = PrebidAuctionProvider::new(base_config());
        let bid = provider
            .parse_bid(&bid_json, "thetradedesk")
            .expect("should parse bid");
        assert_eq!(
            bid.cache_id.as_deref(),
            Some("f47447a0-b759-4f2f-9887-af458b79b570"),
            "should extract cacheId as cache_id"
        );
        assert_eq!(
            bid.cache_host.as_deref(),
            Some("openads.adsrvr.org"),
            "should extract host from cache URL"
        );
        assert_eq!(
            bid.cache_path.as_deref(),
            Some("/cache"),
            "should extract path from cache URL"
        );
    }

    #[test]
    fn parse_bid_sets_cache_fields_to_none_when_no_cache_entry() {
        let bid_json = serde_json::json!({
            "id": "bid-id-456",
            "impid": "atf_sidebar_ad",
            "price": 0.50,
            "w": 300,
            "h": 250
        });
        let provider = PrebidAuctionProvider::new(base_config());
        let bid = provider
            .parse_bid(&bid_json, "appnexus")
            .expect("should parse bid");
        assert!(bid.cache_id.is_none(), "should be None when cache absent");
        assert!(bid.cache_host.is_none(), "should be None when cache absent");
        assert!(bid.cache_path.is_none(), "should be None when cache absent");
    }

    #[test]
    fn parse_bid_handles_malformed_cache_url_gracefully() {
        let bid_json = serde_json::json!({
            "id": "bid-id-789",
            "impid": "atf_sidebar_ad",
            "price": 0.50,
            "w": 300,
            "h": 250,
            "ext": {
                "prebid": {
                    "cache": {
                        "bids": {
                            "url": "not-a-valid-url",
                            "cacheId": "some-uuid"
                        }
                    }
                }
            }
        });
        let provider = PrebidAuctionProvider::new(base_config());
        let bid = provider
            .parse_bid(&bid_json, "appnexus")
            .expect("should parse bid without panicking");
        assert_eq!(
            bid.cache_id.as_deref(),
            Some("some-uuid"),
            "should still extract cacheId even if URL is malformed"
        );
        assert!(
            bid.cache_host.is_none(),
            "should be None when URL parse fails"
        );
        assert!(
            bid.cache_path.is_none(),
            "should be None when URL parse fails"
        );
    }

    #[test]
    fn parse_bid_includes_nurl_and_burl_by_default() {
        let bid_json = serde_json::json!({
            "impid": "atf_sidebar_ad",
            "price": 1.50,
            "w": 300,
            "h": 250,
            "nurl": "https://ssp.example/win?id=abc123",
            "burl": "https://ssp.example/bill?id=abc123"
        });
        let provider = PrebidAuctionProvider::new(base_config());
        let bid = provider
            .parse_bid(&bid_json, "appnexus")
            .expect("should parse bid");
        assert_eq!(
            bid.nurl.as_deref(),
            Some("https://ssp.example/win?id=abc123"),
            "should include nurl when suppress_nurl is false"
        );
        assert_eq!(
            bid.burl.as_deref(),
            Some("https://ssp.example/bill?id=abc123"),
            "should include burl when suppress_nurl is false"
        );
    }

    #[test]
    fn parse_bid_strips_nurl_and_burl_when_suppress_nurl_enabled() {
        let bid_json = serde_json::json!({
            "impid": "atf_sidebar_ad",
            "price": 1.50,
            "w": 300,
            "h": 250,
            "nurl": "https://ssp.example/win?id=abc123",
            "burl": "https://ssp.example/bill?id=abc123"
        });
        let config = PrebidIntegrationConfig {
            suppress_nurl: true,
            ..base_config()
        };
        let provider = PrebidAuctionProvider::new(config);
        let bid = provider
            .parse_bid(&bid_json, "appnexus")
            .expect("should parse bid");
        assert_eq!(
            bid.nurl, None,
            "should strip nurl when suppress_nurl is true"
        );
        assert_eq!(
            bid.burl, None,
            "should strip burl when suppress_nurl is true"
        );
    }

    #[test]
    fn parse_bid_strips_nurl_and_burl_for_configured_suppressed_bidder_only() {
        let bid_json = serde_json::json!({
            "impid": "atf_sidebar_ad",
            "price": 1.50,
            "w": 300,
            "h": 250,
            "nurl": "https://ssp.example/win?id=abc123",
            "burl": "https://ssp.example/bill?id=abc123"
        });
        let config = PrebidIntegrationConfig {
            suppress_nurl_bidders: vec!["appnexus".to_string()],
            ..base_config()
        };
        let provider = PrebidAuctionProvider::new(config);

        let suppressed_bid = provider
            .parse_bid(&bid_json, "appnexus")
            .expect("should parse suppressed bidder bid");
        let preserved_bid = provider
            .parse_bid(&bid_json, "openx")
            .expect("should parse unsuppressed bidder bid");

        assert_eq!(
            suppressed_bid.nurl, None,
            "should strip nurl only for the configured bidder"
        );
        assert_eq!(
            suppressed_bid.burl, None,
            "should strip burl only for the configured bidder"
        );
        assert_eq!(
            preserved_bid.nurl.as_deref(),
            Some("https://ssp.example/win?id=abc123"),
            "should preserve nurl for bidders not configured for suppression"
        );
        assert_eq!(
            preserved_bid.burl.as_deref(),
            Some("https://ssp.example/bill?id=abc123"),
            "should preserve burl for bidders not configured for suppression"
        );
    }

    #[test]
    fn parse_bid_preserves_ad_id_alongside_cache_id() {
        let bid_json = serde_json::json!({
            "id": "bid-impression-id",
            "impid": "atf_sidebar_ad",
            "adid": "bidder-ad-id-abc",
            "price": 1.0,
            "w": 300,
            "h": 250,
            "ext": {
                "prebid": {
                    "cache": {
                        "bids": {
                            "url": "https://cache.example.com/cache",
                            "cacheId": "cache-uuid-xyz"
                        }
                    }
                }
            }
        });
        let provider = PrebidAuctionProvider::new(base_config());
        let bid = provider
            .parse_bid(&bid_json, "appnexus")
            .expect("should parse bid");
        assert_eq!(
            bid.ad_id.as_deref(),
            Some("bidder-ad-id-abc"),
            "should keep ad_id from adid field"
        );
        assert_eq!(
            bid.cache_id.as_deref(),
            Some("cache-uuid-xyz"),
            "should extract cache UUID separately"
        );
    }

    #[test]
    fn copy_request_headers_replaces_client_supplied_xff_with_attested_ip() {
        let from = http::Request::builder()
            .uri("https://publisher.example.com/")
            .header("x-forwarded-for", "6.6.6.6")
            .header(header::USER_AGENT, "test-agent")
            .body(EdgeBody::empty())
            .expect("should build inbound request");
        let mut to = http::Request::builder()
            .uri("https://pbs.example.com/openrtb2/auction")
            .body(EdgeBody::empty())
            .expect("should build outbound request");

        copy_request_headers(
            &from,
            &mut to,
            ConsentForwardingMode::Both,
            Some(std::net::IpAddr::from([203, 0, 113, 7])),
        );

        assert_eq!(
            to.headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok()),
            Some("203.0.113.7"),
            "should synthesize XFF from the platform-attested client IP, not the spoofable inbound header"
        );
        assert_eq!(
            to.headers()
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok()),
            Some("test-agent"),
            "should still copy the browser User-Agent"
        );
    }

    #[test]
    fn copy_request_headers_omits_xff_without_attested_client_ip() {
        let from = http::Request::builder()
            .uri("https://publisher.example.com/")
            .header("x-forwarded-for", "6.6.6.6")
            .body(EdgeBody::empty())
            .expect("should build inbound request");
        let mut to = http::Request::builder()
            .uri("https://pbs.example.com/openrtb2/auction")
            .body(EdgeBody::empty())
            .expect("should build outbound request");

        copy_request_headers(&from, &mut to, ConsentForwardingMode::Both, None);

        assert!(
            !to.headers().contains_key("x-forwarded-for"),
            "should not forward the client-supplied XFF when no attested IP exists"
        );
    }
}
