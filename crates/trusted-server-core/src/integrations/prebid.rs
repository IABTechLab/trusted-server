use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::HeaderValue;
use http::{header, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use url::Url;
use validator::Validate;

use crate::auction::provider::AuctionProvider;
use crate::auction::types::{
    AuctionContext, AuctionRequest, AuctionResponse, Bid as AuctionBid, MediaType,
};
use crate::consent_config::ConsentForwardingMode;
use crate::cookies::{strip_cookies, CONSENT_COOKIE_NAMES};
use crate::error::TrustedServerError;
use crate::http_util::RequestInfo;
use crate::integrations::{
    collect_body, ensure_integration_backend_with_timeout, predict_integration_backend_name,
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration,
};
use crate::openrtb::{
    to_openrtb_i32, Banner, ConsentedProvidersSettings, Device, Format, Geo, Imp, ImpExt,
    OpenRtbRequest, PrebidExt, PrebidImpExt, Publisher, Regs, RegsExt, RequestExt, Site, ToExt,
    TrustedServerExt, User, UserExt,
};
use crate::platform::{
    PlatformHttpRequest, PlatformPendingRequest, PlatformResponse, RuntimeServices,
};
use crate::request_signing::{RequestSigner, SigningParams, SIGNING_VERSION};
use crate::settings::{IntegrationConfig, Settings};

const PREBID_INTEGRATION_ID: &str = "prebid";
const TRUSTED_SERVER_BIDDER: &str = "trustedServer";
const BIDDER_PARAMS_KEY: &str = "bidderParams";
const ZONE_KEY: &str = "zone";

/// Default currency for `OpenRTB` bid floors and responses.
const DEFAULT_CURRENCY: &str = "USD";

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
    /// Per-bidder, per-zone param overrides. The outer key is a bidder name, the
    /// inner key is a zone name (sent by the JS adapter from `mediaTypes.banner.name`
    /// — a non-standard Prebid.js field used as a temporary workaround),
    /// and the value is a JSON object shallow-merged into that bidder's params.
    ///
    /// Example in TOML:
    /// ```toml
    /// [integrations.prebid.bid_param_zone_overrides.kargo]
    /// header       = {placementId = "_s2sHeaderId"}
    /// in_content   = {placementId = "_s2sContentId"}
    /// fixed_bottom = {placementId = "_s2sBottomId"}
    /// ```
    #[serde(default)]
    pub bid_param_zone_overrides: HashMap<String, HashMap<String, Json>>,
    /// How consent signals are forwarded to Prebid Server.
    ///
    /// - `openrtb_only` — consent in `OpenRTB` body only, consent cookies stripped
    /// - `cookies_only` — consent cookies forwarded, body consent fields omitted
    /// - `both` — consent in both cookies and body (default)
    #[serde(default)]
    pub consent_forwarding: ConsentForwardingMode,
}

impl IntegrationConfig for PrebidIntegrationConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
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

pub struct PrebidIntegration {
    config: PrebidIntegrationConfig,
}

impl PrebidIntegration {
    fn new(config: PrebidIntegrationConfig) -> Arc<Self> {
        Arc::new(Self { config })
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
            Url::parse(&format!("https:{without_query}"))
        } else {
            Url::parse(without_query)
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
            .header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .header(header::CACHE_CONTROL, "public, max-age=31536000")
            .body(EdgeBody::from(body))
            .change_context(TrustedServerError::Prebid {
                message: "Failed to build Prebid script handler response".to_string(),
            })
    }
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<PrebidIntegration>>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<PrebidIntegrationConfig>(PREBID_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

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

    Ok(Some(PrebidIntegration::new(config)))
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
            .with_deferred_js()
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
        _settings: &Settings,
        _services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let path = req.uri().path().to_string();
        let method = req.method().clone();

        match method {
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

        vec![format!(
            r#"<script>window.pbjs=window.pbjs||{{}};window.pbjs.que=window.pbjs.que||[];window.pbjs.cmd=window.pbjs.cmd||[];window.__tsjs_prebid={config_json};</script>"#
        )]
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
/// Copies browser headers to the outgoing Prebid Server request.
///
/// In [`ConsentForwardingMode::OpenrtbOnly`] mode, consent cookies are
/// stripped from the `Cookie` header since consent travels exclusively
/// through the `OpenRTB` body.
fn copy_request_headers(
    from: &http::Request<EdgeBody>,
    to: &mut http::Request<EdgeBody>,
    consent_forwarding: ConsentForwardingMode,
) {
    let headers_to_copy = [
        header::USER_AGENT,
        header::HeaderName::from_static("x-forwarded-for"),
        header::REFERER,
        header::ACCEPT_LANGUAGE,
    ];

    for header_name in &headers_to_copy {
        if let Some(value) = from.headers().get(header_name) {
            to.headers_mut().insert(header_name, value.clone());
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
}

impl PrebidAuctionProvider {
    /// Create a new Prebid auction provider.
    #[must_use]
    pub fn new(config: PrebidIntegrationConfig) -> Self {
        Self { config }
    }

    /// Convert auction request to `OpenRTB` format with all enrichments.
    fn to_openrtb(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
        signer: Option<(&RequestSigner, String, &SigningParams)>,
        _request_info: RequestInfo,
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
                // Pass through any other bidders with their params as-is.
                let mut bidder: HashMap<String, Json> = HashMap::new();
                for (name, params) in &slot.bidders {
                    if name == TRUSTED_SERVER_BIDDER {
                        bidder.extend(expand_trusted_server_bidders(&self.config.bidders, params));
                    } else {
                        bidder.insert(name.clone(), params.clone());
                    }
                }

                // Fallback to config bidders if none provided
                if bidder.is_empty() {
                    for b in &self.config.bidders {
                        bidder.insert(b.clone(), Json::Object(serde_json::Map::new()));
                    }
                }

                // Apply zone-specific bid param overrides when configured.
                for (name, params) in &mut bidder {
                    let zone_override = zone.and_then(|z| {
                        self.config
                            .bid_param_zone_overrides
                            .get(name.as_str())
                            .and_then(|zones| zones.get(z))
                    });

                    if let Some(Json::Object(ovr)) = zone_override {
                        if let Json::Object(base) = params {
                            log::debug!(
                                "prebid: zone override for '{}' zone '{}': keys {:?}",
                                name,
                                zone.unwrap_or(""),
                                ovr.keys().collect::<Vec<_>>()
                            );
                            base.extend(ovr.iter().map(|(k, v)| (k.clone(), v.clone())));
                        }
                    }
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
                        prebid: PrebidImpExt { bidder },
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
        // In cookies_only mode, body consent fields are omitted — consent
        // travels exclusively through the forwarded Cookie header.
        let consent_ctx = if self.config.consent_forwarding.includes_body_consent() {
            request.user.consent.as_ref()
        } else {
            None
        };
        let raw_tc = consent_ctx.and_then(|c| c.raw_tc_string.clone());
        let user = Some(User {
            id: Some(request.user.id.clone()),
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
                // EIDs will be populated by identity providers; consent gating
                // is applied via `gate_eids_by_consent` before they are set here.
                eids: None,
                ec_fresh: Some(request.user.fresh_id.clone()),
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
        let request_info = RequestInfo::from_request(context.request, context.client_info);
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

        let tmax = to_openrtb_i32(self.config.timeout_ms, "tmax", "request");

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
            AuctionResponse::no_bid("prebid", response_time_ms)
        } else {
            AuctionResponse::success("prebid", bids, response_time_ms)
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

        let nurl = bid_obj
            .get("nurl")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);

        let burl = bid_obj
            .get("burl")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);

        let adomain = bid_obj
            .get("adomain")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                    .collect()
            });

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
            metadata: std::collections::HashMap::new(),
        })
    }
}

#[async_trait(?Send)]
impl AuctionProvider for PrebidAuctionProvider {
    fn provider_name(&self) -> &'static str {
        "prebid"
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
        log::info!("Prebid: requesting bids for {} slots", request.slots.len());

        let request_info = RequestInfo::from_request(context.request, context.client_info);

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
                    "Prebid OpenRTB request to {}/openrtb2/auction:\n{}",
                    self.config.server_url,
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
            .uri(format!("{}/openrtb2/auction", self.config.server_url))
            .body(EdgeBody::empty())
            .change_context(TrustedServerError::Prebid {
                message: "Failed to build Prebid request".to_string(),
            })?;
        copy_request_headers(
            context.request,
            &mut pbs_req,
            self.config.consent_forwarding,
        );

        let pbs_body = serde_json::to_vec(&openrtb).change_context(TrustedServerError::Prebid {
            message: "Failed to serialize Prebid request body".to_string(),
        })?;
        pbs_req.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        *pbs_req.body_mut() = EdgeBody::from(pbs_body);

        // Send request asynchronously with auction-scoped timeout
        let backend_name = ensure_integration_backend_with_timeout(
            context.services,
            &self.config.server_url,
            "prebid",
            Duration::from_millis(u64::from(context.timeout_ms)),
        )?;
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
        let response = response.response;
        let status = response.status();

        // Parse response — collect_body handles both Once and Stream variants safely.
        let body_bytes = collect_body(response.into_body(), "prebid")
            .await
            .change_context(TrustedServerError::Prebid {
                message: "Failed to read Prebid response body".to_string(),
            })?;

        if !status.is_success() {
            log::warn!("Prebid returned non-success status: {}", status,);
            if log::log_enabled!(log::Level::Trace) {
                let body_preview = String::from_utf8_lossy(&body_bytes);
                log::trace!(
                    "Prebid error response body: {}",
                    &body_preview[..body_preview.floor_char_boundary(1000)]
                );
            }
            return Ok(AuctionResponse::error("prebid", response_time_ms));
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
            true,
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
    let mut providers: Vec<Arc<dyn AuctionProvider>> = Vec::new();

    match settings.integration_config::<PrebidIntegrationConfig>("prebid") {
        Ok(Some(config)) => {
            log::info!(
                "Registering Prebid auction provider (server_url={})",
                config.server_url
            );
            if config.debug {
                log::warn!(
                    "Prebid debug mode is ON — debug data (httpcalls, resolvedrequest, \
                     bidstatus) will be included in /auction responses"
                );
            }
            providers.push(Arc::new(PrebidAuctionProvider::new(config)));
        }
        Ok(None) => {
            log::info!("Prebid auction provider not registered: integration not found or disabled");
        }
        Err(e) => {
            return Err(e);
        }
    }

    Ok(providers)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::auction::test_support::create_test_auction_context as shared_test_auction_context;
    use crate::auction::types::{
        AdFormat, AdSlot, AuctionContext, AuctionRequest, DeviceInfo, PublisherInfo, UserInfo,
    };

    use crate::consent::ConsentContext;
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
    use crate::test_support::tests::crate_test_settings_str;
    use http::Method;
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::Cursor;

    fn make_settings() -> Settings {
        Settings::from_toml(&crate_test_settings_str()).expect("should parse settings")
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
            client_side_bidders: Vec::new(),
            bid_param_zone_overrides: HashMap::new(),
            consent_forwarding: ConsentForwardingMode::Both,
        }
    }

    struct PredictOnlyBackend;

    impl PlatformBackend for PredictOnlyBackend {
        fn predict_name(
            &self,
            spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Ok(format!(
                "predicted_{}_{}_{}",
                spec.scheme,
                spec.host,
                spec.first_byte_timeout.as_millis()
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
            backend_name, "predicted_https_prebid.example_123",
            "should use PlatformBackend::predict_name instead of duplicating the naming scheme"
        );
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
                id: "user-123".to_string(),
                fresh_id: "fresh-456".to_string(),
                consent: None,
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
            client_info: services.client_info(),
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
        client_info: &'a crate::platform::ClientInfo,
    ) -> AuctionContext<'a> {
        shared_test_auction_context(settings, request, client_info, 1000)
    }

    fn make_request_info(context: &AuctionContext<'_>) -> RequestInfo {
        RequestInfo::from_request(context.request, context.client_info)
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
path = "^/admin"
username = "admin"
password = "admin-pass"

[publisher]
domain = "test-publisher.com"
cookie_domain = ".test-publisher.com"
origin_url = "https://origin.test-publisher.com"
proxy_secret = "test-secret"

[edge_cookie]
secret_key = "test-secret-key"
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

    #[test]
    fn attribute_rewriter_removes_prebid_scripts() {
        let integration = PrebidIntegration {
            config: base_config(),
        };
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
        let integration = PrebidIntegration {
            config: base_config(),
        };
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
            processed.contains("tsjs-prebid.min.js"),
            "Deferred prebid bundle should be injected"
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

        let body = String::from_utf8(response.into_body().into_bytes().to_vec())
            .expect("should parse script body as utf-8");
        assert!(body.contains("// Script overridden by Trusted Server"));
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
        assert_eq!(inserts.len(), 1, "should produce exactly one head insert");

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
            }),
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
            }),
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
            }),
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
            }),
        });

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

        let openrtb = provider.to_openrtb(
            &auction_request,
            &context,
            None,
            make_request_info(&context),
        );

        assert_eq!(
            openrtb.tmax,
            Some(1000),
            "should set tmax from config timeout_ms"
        );
        assert_eq!(
            openrtb.cur,
            vec!["USD".to_string()],
            "should set cur to USD"
        );
    }

    #[test]
    fn to_openrtb_omits_tmax_when_timeout_exceeds_i32_max() {
        let mut config = base_config();
        config.timeout_ms = i32::MAX as u32 + 1;
        let provider = PrebidAuctionProvider::new(config);
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let request = build_test_request();
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
    fn to_openrtb_sets_site_ref_from_referer_header() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let mut request = build_test_request();
        request.headers_mut().insert(
            "Referer",
            http::header::HeaderValue::from_static("https://google.com/search?q=test"),
        );
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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
        let context = create_test_auction_context(
            &settings,
            &request,
            &crate::platform::ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            },
        );

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

        // Should have 0 routes when no script patterns configured
        assert_eq!(routes.len(), 0);
    }

    /// Verifies body-preview truncation keeps a UTF-8 char boundary.
    #[test]
    fn body_preview_truncation_is_utf8_safe() {
        // 999 ASCII bytes + U+2603 SNOWMAN (3 bytes: E2 98 83) = 1002 bytes.
        // Byte index 1000 lands on 0x98, the second byte of the snowman.
        let mut body = "x".repeat(999);
        body.push('\u{2603}'); // ☃
        assert_eq!(body.len(), 1002);

        let truncation_index = body.floor_char_boundary(1000);
        assert!(
            body.is_char_boundary(truncation_index),
            "should truncate at a valid UTF-8 boundary"
        );
        assert_eq!(
            body[..truncation_index].len(),
            999,
            "should drop the partial multibyte character"
        );
        assert_eq!(
            truncation_index, 999,
            "should step back to the previous char boundary"
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
                id: "synth-123".to_string(),
                fresh_id: "fresh-456".to_string(),
                consent: None,
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
        let client_info = crate::platform::ClientInfo {
            client_ip: None,
            tls_protocol: None,
            tls_cipher: None,
        };
        let services = noop_services();
        let context = AuctionContext {
            settings: &settings,
            request: &http_req,
            client_info: &client_info,
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
                json!({ "placementId": "s2s_header_id" }),
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
                json!({ "placementId": "zone_header_id" }),
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
                json!({ "placementId": "zone_header_id" }),
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
                json!({ "placementId": "s2s_header_id" }),
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
            HashMap::from([("header".to_string(), json!({ "placementId": "s2s_header" }))]),
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
}
