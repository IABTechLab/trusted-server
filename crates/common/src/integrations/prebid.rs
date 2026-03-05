use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode, Url};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use validator::Validate;

use crate::auction::provider::AuctionProvider;
use crate::auction::types::{
    AuctionContext, AuctionRequest, AuctionResponse, Bid as AuctionBid, MediaType,
};
use crate::backend::BackendConfig;
use crate::error::TrustedServerError;
use crate::http_util::RequestInfo;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration,
};
use crate::openrtb::{
    Banner, Device, Format, Geo, Imp, ImpExt, OpenRtbRequest, PrebidExt, PrebidImpExt, Publisher,
    Regs, RequestExt, Site, ToExt, TrustedServerExt, User, UserExt,
};
use crate::request_signing::{RequestSigner, SigningParams, SIGNING_VERSION};
use crate::settings::{IntegrationConfig, Settings};

const PREBID_INTEGRATION_ID: &str = "prebid";
const TRUSTED_SERVER_BIDDER: &str = "trustedServer";
const BIDDER_PARAMS_KEY: &str = "bidderParams";
const ZONE_KEY: &str = "zone";

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct PrebidIntegrationConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
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

    fn handle_script_handler(&self) -> Result<Response, Report<TrustedServerError>> {
        let body = "// Script overridden by Trusted Server\n";

        Ok(Response::from_status(StatusCode::OK)
            .with_header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .with_header(header::CACHE_CONTROL, "public, max-age=31536000")
            .with_body(body))
    }
}

fn build(settings: &Settings) -> Option<Arc<PrebidIntegration>> {
    let config = settings
        .integration_config::<PrebidIntegrationConfig>(PREBID_INTEGRATION_ID)
        .ok()
        .flatten()?;
    if !config.enabled {
        return None;
    }
    if config.server_url.trim().is_empty() {
        log::warn!("Prebid integration disabled: prebid.server_url missing");
        return None;
    }
    Some(PrebidIntegration::new(config))
}

#[must_use]
pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;
    Some(
        IntegrationRegistration::builder(PREBID_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration.clone())
            .with_head_injector(integration)
            .build(),
    )
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
            let static_path: &'static str = Box::leak(pattern.clone().into_boxed_str());
            routes.push(IntegrationEndpoint::get(static_path));
        }

        routes
    }

    async fn handle(
        &self,
        _settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path().to_string();
        let method = req.get_method().clone();

        match method {
            // Serve empty JS for matching script patterns
            Method::GET if self.matches_script_pattern(&path) => self.handle_script_handler(),
            _ => Ok(Response::from_status(StatusCode::NOT_FOUND).with_body("Not Found")),
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
        }

        let payload = InjectedPrebidClientConfig {
            account_id: self.config.account_id.as_deref().unwrap_or_default(),
            timeout: self.config.timeout_ms,
            debug: self.config.debug,
            bidders: &self.config.bidders,
        };

        // Escape `</` to prevent breaking out of the script tag.
        let config_json = serde_json::to_string(&payload)
            .unwrap_or_else(|_| "{}".to_string())
            .replace("</", "<\\/");

        vec![format!(
            r#"<script>window.__tsjs_prebid={config_json};</script>"#
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
fn transform_prebid_response(
    response: &mut Json,
    request_host: &str,
    request_scheme: &str,
) -> Result<(), Report<TrustedServerError>> {
    if let Some(seatbids) = response["seatbid"].as_array_mut() {
        for seatbid in seatbids {
            if let Some(bids) = seatbid["bid"].as_array_mut() {
                for bid in bids {
                    if let Some(adm) = bid["adm"].as_str() {
                        bid["adm"] = json!(rewrite_ad_markup(adm, request_host, request_scheme));
                    }

                    if let Some(nurl) = bid["nurl"].as_str() {
                        bid["nurl"] = json!(make_first_party_proxy_url(
                            nurl,
                            request_host,
                            request_scheme,
                            "track"
                        ));
                    }

                    if let Some(burl) = bid["burl"].as_str() {
                        bid["burl"] = json!(make_first_party_proxy_url(
                            burl,
                            request_host,
                            request_scheme,
                            "track"
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

fn rewrite_ad_markup(markup: &str, request_host: &str, request_scheme: &str) -> String {
    let mut content = markup.to_string();
    let cdn_patterns = [
        ("https://cdn.adsrvr.org", "adsrvr"),
        ("https://ib.adnxs.com", "adnxs"),
        ("https://rtb.openx.net", "openx"),
        ("https://as.casalemedia.com", "casale"),
        ("https://eus.rubiconproject.com", "rubicon"),
    ];

    for (cdn_url, cdn_name) in cdn_patterns {
        if content.contains(cdn_url) {
            let proxy_base = format!(
                "{}://{}/ad-proxy/{}",
                request_scheme, request_host, cdn_name
            );
            content = content.replace(cdn_url, &proxy_base);
        }
    }

    content = content.replace(
        "//cdn.adsrvr.org",
        &format!("//{}/ad-proxy/adsrvr", request_host),
    );
    content = content.replace(
        "//ib.adnxs.com",
        &format!("//{}/ad-proxy/adnxs", request_host),
    );
    content
}

fn make_first_party_proxy_url(
    third_party_url: &str,
    request_host: &str,
    request_scheme: &str,
    proxy_type: &str,
) -> String {
    let encoded = BASE64.encode(third_party_url.as_bytes());
    format!(
        "{}://{}/ad-proxy/{}/{}",
        request_scheme, request_host, proxy_type, encoded
    )
}

fn copy_request_headers(from: &Request, to: &mut Request) {
    let headers_to_copy = [
        header::COOKIE,
        header::USER_AGENT,
        header::HeaderName::from_static("x-forwarded-for"),
        header::REFERER,
        header::ACCEPT_LANGUAGE,
    ];

    for header_name in &headers_to_copy {
        if let Some(value) = from.get_header(header_name) {
            to.set_header(header_name, value);
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

const REDACTED_OPENRTB_LOG_VALUE: &str = "[REDACTED]";

fn redact_json_field_for_logging(payload: &mut Json, path: &[&str]) {
    if path.is_empty() {
        return;
    }

    let mut current = payload;
    for segment in &path[..path.len() - 1] {
        let Some(next) = current.get_mut(*segment) else {
            return;
        };
        current = next;
    }

    if let Some(last) = path.last() {
        if let Some(value) = current.get_mut(*last) {
            *value = Json::String(REDACTED_OPENRTB_LOG_VALUE.to_string());
        }
    }
}

fn redact_openrtb_request_for_logging(openrtb: &OpenRtbRequest) -> Result<Json, serde_json::Error> {
    let mut payload = serde_json::to_value(openrtb)?;

    for path in [
        &["user", "consent"][..],
        &["device", "ip"][..],
        &["device", "ipv6"][..],
        &["device", "geo", "lat"][..],
        &["device", "geo", "lon"][..],
        &["site", "ref"][..],
    ] {
        redact_json_field_for_logging(&mut payload, path);
    }

    Ok(payload)
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
    ) -> OpenRtbRequest {
        let imps = request
            .slots
            .iter()
            .map(|slot| {
                let formats = slot
                    .formats
                    .iter()
                    .filter(|f| f.media_type == MediaType::Banner)
                    .map(|f| Format {
                        w: Some(f.width),
                        h: Some(f.height),
                        ..Default::default()
                    })
                    .collect();

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

                Imp {
                    id: slot.id.clone(),
                    banner: Some(Banner {
                        format: Some(formats),
                        ..Default::default()
                    }),
                    bidfloor: slot.floor_price,
                    // NOTE: Currency is hardcoded to USD. If multi-currency
                    // support is needed, this should come from config or the
                    // AdSlot itself.
                    bidfloorcur: slot.floor_price.map(|_| "USD".to_string()),
                    secure: Some(1), // require HTTPS creatives
                    tagid: Some(slot.id.clone()),
                    ext: ImpExt {
                        prebid: PrebidImpExt { bidder },
                    }
                    .to_ext(),
                    ..Default::default()
                }
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

        // Build user object with consent string when available
        let user = Some(User {
            id: Some(request.user.id.clone()),
            consent: request.user.consent.clone(),
            ext: UserExt {
                synthetic_fresh: Some(request.user.fresh_id.clone()),
            }
            .to_ext(),
            ..Default::default()
        });

        // Extract DNT header and Accept-Language from the original request
        let dnt = context.request.get_header_str("DNT").and_then(|v| {
            if v.trim() == "1" {
                Some(1)
            } else {
                None
            }
        });

        let language = context
            .request
            .get_header_str(header::ACCEPT_LANGUAGE)
            .and_then(|v| {
                // Extract the primary language tag (e.g., "en" from "en-US,en;q=0.9")
                v.split(',')
                    .next()
                    .and_then(|tag| tag.split(';').next())
                    .map(|tag| tag.trim().to_string())
            });

        // Build device object with user-agent, client IP, geo, DNT, and language.
        // Forwarding the real client IP is critical: without it PBS infers the
        // IP from the incoming connection (a data-center / edge IP), causing
        // bidders like PubMatic to filter the traffic as non-human.
        let device = request.device.as_ref().map(|d| Device {
            ua: d.user_agent.clone(),
            ip: d.ip.clone(),
            geo: d.geo.as_ref().map(|geo| Geo {
                country: Some(geo.country.clone()),
                city: Some(geo.city.clone()),
                region: geo.region.clone(),
                lat: Some(geo.latitude),
                lon: Some(geo.longitude),
                // DMA/metro code: convert i64 to string for OpenRTB
                metro: if geo.metro_code > 0 {
                    Some(geo.metro_code.to_string())
                } else {
                    None
                },
                r#type: Some(2),
                ..Default::default()
            }),
            dnt,
            language,
            ..Default::default()
        });

        // Build regs object. Set GDPR flag when consent string is present,
        // and us_privacy when Sec-GPC header signals opt-out.
        //
        // NOTE: Setting gdpr=1 whenever a TCF consent string exists is the
        // conservative approach — it may reduce fill for non-EU traffic where
        // a CMP sets a consent string regardless of jurisdiction. A more
        // precise approach would derive GDPR applicability from the consent
        // string itself (segment 0 encodes `isSubjectToGDPR`) or from user
        // geo (EU/EEA country check).
        let has_consent = request.user.consent.is_some();
        let has_gpc = context
            .request
            .get_header_str("Sec-GPC")
            .is_some_and(|v| v.trim() == "1");

        let regs = if has_consent || has_gpc {
            let gdpr = if has_consent { Some(1) } else { None };
            let us_privacy = if has_gpc {
                Some("1YYN".to_string())
            } else {
                None
            };
            Some(Regs {
                gdpr,
                us_privacy,
                ..Default::default()
            })
        } else {
            None
        };

        // Build ext object
        let request_info = RequestInfo::from_request(context.request);
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
            .get_header_str(header::REFERER)
            .map(std::string::ToString::to_string);

        OpenRtbRequest {
            id: request.id.clone(),
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
            test: self.config.test_mode.then_some(1),
            tmax: Some(self.config.timeout_ms),
            cur: Some(vec!["USD".to_string()]),
            ext,
            ..Default::default()
        }
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
                        if let Ok(bid) = self.parse_bid(bid_obj, seat) {
                            bids.push(bid);
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
            .unwrap_or(300) as u32;
        let height = bid_obj
            .get("h")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(250) as u32;

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
            currency: "USD".to_string(),
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

impl AuctionProvider for PrebidAuctionProvider {
    fn provider_name(&self) -> &'static str {
        "prebid"
    }

    fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<fastly::http::request::PendingRequest, Report<TrustedServerError>> {
        log::info!("Prebid: requesting bids for {} slots", request.slots.len());

        // Create signer and compute signature if request signing is enabled
        let signer_with_signature = if let Some(request_signing_config) =
            &context.settings.request_signing
        {
            if request_signing_config.enabled {
                let request_info = RequestInfo::from_request(context.request);
                let signer = RequestSigner::from_config()?;
                let params =
                    SigningParams::new(request.id.clone(), request_info.host, request_info.scheme);
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
        );

        // Log the outgoing OpenRTB request for debugging with sensitive fields redacted.
        if log::log_enabled!(log::Level::Debug) {
            match redact_openrtb_request_for_logging(&openrtb)
                .and_then(|payload| serde_json::to_string_pretty(&payload))
            {
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
        let mut pbs_req = Request::new(
            Method::POST,
            format!("{}/openrtb2/auction", self.config.server_url),
        );
        copy_request_headers(context.request, &mut pbs_req);

        pbs_req
            .set_body_json(&openrtb)
            .change_context(TrustedServerError::Prebid {
                message: "Failed to set request body".to_string(),
            })?;

        // Send request asynchronously
        let backend_name = BackendConfig::from_url(&self.config.server_url, true)?;
        let pending =
            pbs_req
                .send_async(backend_name)
                .change_context(TrustedServerError::Prebid {
                    message: "Failed to send async request to Prebid Server".to_string(),
                })?;

        Ok(pending)
    }

    fn parse_response(
        &self,
        mut response: fastly::Response,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        // Parse response
        let body_bytes = response.take_body_bytes();

        if !response.get_status().is_success() {
            let body_preview = String::from_utf8_lossy(&body_bytes);
            log::warn!(
                "Prebid returned non-success status: {} — body: {}",
                response.get_status(),
                &body_preview[..body_preview.floor_char_boundary(1000)]
            );
            return Ok(AuctionResponse::error("prebid", response_time_ms));
        }

        let mut response_json: Json =
            serde_json::from_slice(&body_bytes).change_context(TrustedServerError::Prebid {
                message: "Failed to parse Prebid response".to_string(),
            })?;

        // Log the full response body when debug is enabled to surface
        // ext.debug.httpcalls, resolvedrequest, bidstatus, errors, etc.
        if self.config.debug && log::log_enabled!(log::Level::Debug) {
            match serde_json::to_string_pretty(&response_json) {
                Ok(json) => log::debug!("Prebid OpenRTB response:\n{json}"),
                Err(e) => {
                    log::warn!("Prebid: failed to serialize response for logging: {e}");
                }
            }
        }

        let request_host = response_json
            .get("ext")
            .and_then(|ext| ext.get("trusted_server"))
            .and_then(|trusted_server| trusted_server.get("request_host"))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let request_scheme = response_json
            .get("ext")
            .and_then(|ext| ext.get("trusted_server"))
            .and_then(|trusted_server| trusted_server.get("request_scheme"))
            .and_then(|value| value.as_str())
            .unwrap_or("https")
            .to_string();

        if request_host.is_empty() {
            log::warn!("Prebid response missing request host; skipping URL rewrites");
        } else {
            transform_prebid_response(&mut response_json, &request_host, &request_scheme)?;
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

    fn backend_name(&self) -> Option<String> {
        BackendConfig::from_url(&self.config.server_url, true).ok()
    }
}

// ============================================================================
// Provider Auto-Registration
// ============================================================================

/// Auto-register Prebid provider based on settings configuration.
///
/// This function checks the settings for Prebid configuration and returns
/// the provider if enabled.
#[must_use]
pub fn register_auction_provider(settings: &Settings) -> Vec<Arc<dyn AuctionProvider>> {
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
            log::error!("Prebid auction provider not registered: config error: {e:?}");
        }
    }

    providers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::{
        AdFormat, AdSlot, AuctionContext, AuctionRequest, DeviceInfo, PublisherInfo, UserInfo,
    };
    use crate::geo::GeoInfo;
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};
    use crate::integrations::{
        AttributeRewriteAction, IntegrationDocumentState, IntegrationRegistry,
    };
    use crate::settings::Settings;
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::crate_test_settings_str;
    use fastly::http::Method;
    use fastly::Request;
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
            bid_param_zone_overrides: HashMap::new(),
        }
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

    fn create_test_auction_context<'a>(
        settings: &'a Settings,
        request: &'a Request,
    ) -> AuctionContext<'a> {
        AuctionContext {
            settings,
            request,
            timeout_ms: 1000,
            provider_responses: None,
        }
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

    /// Shared TOML prefix for config-parsing tests (publisher + synthetic sections).
    const TOML_BASE: &str = r#"
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
            !processed.contains("prebid.min.js"),
            "Prebid script should be removed when auto-config is enabled"
        );
        assert!(
            !processed.contains("cdn.prebid.org/prebid.js"),
            "Prebid preload should be removed when auto-config is enabled"
        );
    }

    #[test]
    fn transform_prebid_response_rewrites_creatives_and_tracking() {
        let mut response = json!({
            "seatbid": [{
                "bid": [{
                    "adm": r#"<img src="https://cdn.adsrvr.org/pixel.png">"#,
                    "nurl": "https://notify.example/win",
                    "burl": "https://notify.example/bill"
                }]
            }]
        });

        transform_prebid_response(&mut response, "pub.example", "https")
            .expect("should rewrite response");

        let rewritten_adm = response["seatbid"][0]["bid"][0]["adm"]
            .as_str()
            .expect("adm should be string");
        assert!(
            rewritten_adm.contains("/ad-proxy/adsrvr"),
            "creative markup should proxy CDN urls"
        );

        for url_field in ["nurl", "burl"] {
            let value = response["seatbid"][0]["bid"][0][url_field]
                .as_str()
                .expect("should get tracking URL");
            assert!(
                value.contains("/ad-proxy/track/"),
                "tracking URLs should be proxied"
            );
        }
    }

    #[test]
    fn make_first_party_proxy_url_base64_encodes_target() {
        let url = "https://cdn.example/path?x=1";
        let rewritten = make_first_party_proxy_url(url, "pub.example", "https", "track");
        assert!(
            rewritten.starts_with("https://pub.example/ad-proxy/track/"),
            "proxy prefix should be applied"
        );

        let encoded = rewritten
            .split("/ad-proxy/track/")
            .nth(1)
            .expect("should have encoded payload after proxy prefix");
        let decoded = BASE64
            .decode(encoded.as_bytes())
            .expect("should decode base64 proxy payload");
        assert_eq!(
            String::from_utf8(decoded).expect("should be valid UTF-8"),
            url
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

        assert_eq!(response.get_status(), StatusCode::OK);

        let content_type = response
            .get_header_str(header::CONTENT_TYPE)
            .expect("should have content-type");
        assert_eq!(content_type, "application/javascript; charset=utf-8");

        let cache_control = response
            .get_header_str(header::CACHE_CONTROL)
            .expect("should have cache-control");
        assert!(cache_control.contains("max-age=31536000"));

        let body = response.into_body_str();
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
    fn to_openrtb_includes_debug_flags_when_enabled() {
        let mut config = base_config();
        config.debug = true;

        let provider = PrebidAuctionProvider::new(config);
        let auction_request = create_test_auction_request();
        let settings = make_settings();
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);

        assert_eq!(
            openrtb.test, None,
            "debug alone should not set top-level OpenRTB test field"
        );

        let prebid_ext = openrtb
            .ext
            .as_ref()
            .and_then(|ext| ext.get("prebid"))
            .expect("should include ext.prebid");
        assert_eq!(
            prebid_ext.get("debug").and_then(serde_json::Value::as_bool),
            Some(true),
            "should include ext.prebid.debug when debug is enabled"
        );
        assert_eq!(
            prebid_ext
                .get("returnallbidstatus")
                .and_then(serde_json::Value::as_bool),
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
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);

        assert_eq!(
            openrtb.test,
            Some(1),
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
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);

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
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);

        assert_eq!(
            openrtb.test, None,
            "should omit top-level OpenRTB test field when test_mode is disabled"
        );

        let prebid_ext = openrtb
            .ext
            .as_ref()
            .and_then(|ext| ext.get("prebid"))
            .expect("should include ext.prebid");
        assert_eq!(
            prebid_ext.get("debug"),
            None,
            "should omit ext.prebid.debug when debug is disabled"
        );
        assert_eq!(
            prebid_ext.get("returnallbidstatus"),
            None,
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
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
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
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
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
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
        let imp = &openrtb.imp[0];

        assert_eq!(imp.secure, Some(1), "should require HTTPS creatives");
        assert_eq!(
            imp.tagid.as_deref(),
            Some("slot-1"),
            "should set tagid from slot id"
        );
    }

    #[test]
    fn to_openrtb_includes_consent_and_gdpr_flag() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.user.consent = Some("BOtest-consent-string".to_string());

        let settings = make_settings();
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);

        assert_eq!(
            openrtb.user.as_ref().and_then(|u| u.consent.as_deref()),
            Some("BOtest-consent-string"),
            "should forward consent string to user.consent"
        );
        assert_eq!(
            openrtb.regs.as_ref().and_then(|r| r.gdpr),
            Some(1),
            "should set regs.gdpr when consent is present"
        );
    }

    #[test]
    fn to_openrtb_omits_regs_when_no_consent_or_gpc() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request(); // consent=None

        let settings = make_settings();
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);

        assert!(openrtb.regs.is_none(), "should omit regs entirely");
    }

    #[test]
    fn to_openrtb_sets_gpc_us_privacy() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let mut request = Request::get("https://pub.example/auction");
        request.set_header("Sec-GPC", "1");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
        let regs = openrtb.regs.as_ref().expect("should have regs");

        assert_eq!(
            regs.us_privacy.as_deref(),
            Some("1YYN"),
            "should set us_privacy from Sec-GPC: 1"
        );
    }

    #[test]
    fn to_openrtb_ignores_gpc_header_with_non_one_value() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let mut request = Request::get("https://pub.example/auction");
        request.set_header("Sec-GPC", "0");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);

        assert!(
            openrtb.regs.is_none(),
            "should not set regs when Sec-GPC is not '1'"
        );
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
        let mut request = Request::get("https://pub.example/auction");
        request.set_header("DNT", "1");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
        let device = openrtb.device.as_ref().expect("should have device");

        assert_eq!(device.dnt, Some(1), "should set dnt from DNT header");
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
        let mut request = Request::get("https://pub.example/auction");
        request.set_header("Accept-Language", "en-US,en;q=0.9,fr;q=0.8");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
        let device = openrtb.device.as_ref().expect("should have device");

        assert_eq!(
            device.language.as_deref(),
            Some("en-US"),
            "should extract primary language tag"
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
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
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
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);

        assert_eq!(
            openrtb.tmax,
            Some(1000),
            "should set tmax from config timeout_ms"
        );
        assert_eq!(
            openrtb.cur.as_deref(),
            Some(&["USD".to_string()][..]),
            "should set cur to USD"
        );
    }

    #[test]
    fn to_openrtb_sets_site_ref_from_referer_header() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let mut request = Request::get("https://pub.example/auction");
        request.set_header("Referer", "https://google.com/search?q=test");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
        let site = openrtb.site.as_ref().expect("should have site");

        assert_eq!(
            site.r#ref.as_deref(),
            Some("https://google.com/search?q=test"),
            "should set site.ref from Referer header"
        );
    }

    #[test]
    fn redact_openrtb_request_for_logging_redacts_sensitive_fields() {
        let provider = PrebidAuctionProvider::new(base_config());
        let mut auction_request = create_test_auction_request();
        auction_request.user.consent = Some("BOtest-consent-string".to_string());
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("TestAgent".to_string()),
            ip: Some("203.0.113.42".to_string()),
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
        let mut request = Request::get("https://pub.example/auction");
        request.set_header("Referer", "https://google.com/search?q=test");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
        let redacted = redact_openrtb_request_for_logging(&openrtb)
            .expect("should serialize OpenRTB request for redaction");

        assert_eq!(
            redacted["user"]["consent"],
            json!(REDACTED_OPENRTB_LOG_VALUE),
            "should redact user.consent"
        );
        assert_eq!(
            redacted["device"]["ip"],
            json!(REDACTED_OPENRTB_LOG_VALUE),
            "should redact device.ip"
        );
        assert_eq!(
            redacted["device"]["geo"]["lat"],
            json!(REDACTED_OPENRTB_LOG_VALUE),
            "should redact device.geo.lat"
        );
        assert_eq!(
            redacted["device"]["geo"]["lon"],
            json!(REDACTED_OPENRTB_LOG_VALUE),
            "should redact device.geo.lon"
        );
        assert_eq!(
            redacted["site"]["ref"],
            json!(REDACTED_OPENRTB_LOG_VALUE),
            "should redact site.ref"
        );
        assert_eq!(
            redacted["site"]["page"],
            json!("https://pub.example/article"),
            "should preserve non-sensitive fields"
        );
    }

    #[test]
    fn to_openrtb_sets_site_publisher() {
        let provider = PrebidAuctionProvider::new(base_config());
        let auction_request = create_test_auction_request();

        let settings = make_settings();
        let request = Request::get("https://pub.example/auction");
        let context = create_test_auction_context(&settings, &request);

        let openrtb = provider.to_openrtb(&auction_request, &context, None);
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
        let provider = PrebidAuctionProvider::new(config);
        let settings = make_settings();
        let fastly_req = Request::new(Method::POST, "https://example.com/auction");
        let context = AuctionContext {
            settings: &settings,
            request: &fastly_req,
            timeout_ms: 1000,
            provider_responses: None,
        };
        provider.to_openrtb(request, &context, None)
    }

    fn bidder_params(ortb: &OpenRtbRequest) -> &serde_json::Map<String, Json> {
        let ext = ortb.imp[0].ext.as_ref().expect("should have imp ext");
        ext.get("prebid")
            .and_then(|p| p.get("bidder"))
            .and_then(|b| b.as_object())
            .expect("should have prebid.bidder in imp ext")
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
