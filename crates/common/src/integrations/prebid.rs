use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode, Url};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json, Value as JsonValue};
use validator::Validate;

use crate::auction::provider::AuctionProvider;
use crate::auction::types::{
    AuctionContext, AuctionRequest, AuctionResponse, Bid as AuctionBid, MediaType,
};
use crate::backend::ensure_backend_from_url;
use crate::constants::{HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER};
use crate::creative;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::http_util::RequestInfo;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
use crate::openrtb::{
    Banner, Device, Format, Geo, Imp, ImpExt, OpenRtbRequest, PrebidExt, PrebidImpExt, Regs,
    RegsExt, RequestExt, Site, TrustedServerExt, User, UserExt,
};
use crate::request_signing::RequestSigner;
use crate::settings::{IntegrationConfig, Settings};
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};

const PREBID_INTEGRATION_ID: &str = "prebid";

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct PrebidIntegrationConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub server_url: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    #[serde(
        default = "default_bidders",
        deserialize_with = "crate::settings::vec_from_seq_or_map"
    )]
    pub bidders: Vec<String>,
    #[serde(default)]
    pub debug: bool,
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
    // Default patterns to intercept Prebid scripts and serve empty JS
    // - Exact paths like "/prebid.min.js" match only that path
    // - Wildcard paths like "/static/prebid/*" match anything under that prefix
    //   and are filtered by PREBID_SCRIPT_SUFFIXES in matches_script_pattern()
    vec![
        "/prebid.js".to_string(),
        "/prebid.min.js".to_string(),
        "/prebidjs.js".to_string(),
        "/prebidjs.min.js".to_string(),
    ]
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BannerUnit {
    sizes: Vec<Vec<u32>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaTypes {
    banner: Option<BannerUnit>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdUnit {
    code: String,
    media_types: Option<MediaTypes>,
    #[serde(default)]
    bids: Option<Vec<Bid>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdRequest {
    ad_units: Vec<AdUnit>,
    config: Option<JsonValue>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct Bid {
    bidder: String,
    #[serde(default)]
    params: JsonValue,
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

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: PREBID_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }


    fn handle_script_handler(&self) -> Result<Response, Report<TrustedServerError>> {
        let body = "// Script overridden by Trusted Server\n";

        Ok(Response::from_status(StatusCode::OK)
            .with_header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .with_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
            .with_body(body))
    }

    #[allow(dead_code)]
    async fn handle_first_party_ad(
        &self,
        settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let url = req.get_url_str();
        let parsed = Url::parse(url).change_context(TrustedServerError::Prebid {
            message: "Invalid first-party serve-ad URL".to_string(),
        })?;
        let qp = parsed
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        let slot = qp.get("slot").cloned().unwrap_or_default();
        let w = qp
            .get("w")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(300);
        let h = qp
            .get("h")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(250);
        if slot.is_empty() {
            return Err(Report::new(TrustedServerError::BadRequest {
                message: "missing slot".to_string(),
            }));
        }

        let ad_req = AdRequest {
            ad_units: vec![AdUnit {
                code: slot.clone(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit {
                        sizes: vec![vec![w, h]],
                    }),
                }),
                bids: None,
            }],
            config: None,
        };

        let ortb = build_openrtb_from_ts(&ad_req, settings, &self.config);
        req.set_body_json(&ortb)
            .change_context(TrustedServerError::Prebid {
                message: "Failed to set OpenRTB body".to_string(),
            })?;

        let backend_name = ensure_backend_from_url(&self.config.server_url)?;
        let mut pbs_resp = req
            .send(backend_name)
            .change_context(TrustedServerError::Prebid {
                message: "Failed to send first-party ad request to Prebid Server".to_string(),
            })?;

        let body_bytes = pbs_resp.take_body_bytes();
        let html = match serde_json::from_slice::<Json>(&body_bytes) {
            Ok(json) => extract_adm_for_slot(&json, &slot)
                .unwrap_or_else(|| "<!-- no creative -->".to_string()),
            Err(_) => String::from_utf8(body_bytes).unwrap_or_else(|_| String::new()),
        };

        let rewritten = creative::rewrite_creative_html(settings, &html);

        Ok(Response::from_status(StatusCode::OK)
            .with_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .with_body(rewritten))
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

pub fn register(settings: &Settings) -> Option<IntegrationRegistration> {
    let integration = build(settings)?;
    Some(
        IntegrationRegistration::builder(PREBID_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration)
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
            _ => Err(Report::new(Self::error(format!(
                "Unsupported Prebid route: {path}"
            )))),
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

#[allow(dead_code)]
fn build_openrtb_from_ts(
    req: &AdRequest,
    settings: &Settings,
    prebid: &PrebidIntegrationConfig,
) -> OpenRtbRequest {
    use uuid::Uuid;

    let imps: Vec<Imp> = req
        .ad_units
        .iter()
        .map(|unit| {
            let formats: Vec<Format> = unit
                .media_types
                .as_ref()
                .and_then(|mt| mt.banner.as_ref())
                .map(|b| {
                    b.sizes
                        .iter()
                        .filter(|s| s.len() >= 2)
                        .map(|s| Format { w: s[0], h: s[1] })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec![Format { w: 300, h: 250 }]);

            let mut bidder: HashMap<String, JsonValue> = HashMap::new();
            if let Some(bids) = &unit.bids {
                for bid in bids {
                    bidder.insert(bid.bidder.clone(), bid.params.clone());
                }
            }
            if bidder.is_empty() {
                for b in &prebid.bidders {
                    bidder.insert(b.clone(), JsonValue::Object(serde_json::Map::new()));
                }
            }

            Imp {
                id: unit.code.clone(),
                banner: Some(Banner { format: formats }),
                ext: Some(ImpExt {
                    prebid: PrebidImpExt { bidder },
                }),
            }
        })
        .collect();

    OpenRtbRequest {
        id: Uuid::new_v4().to_string(),
        imp: imps,
        site: Some(Site {
            domain: Some(settings.publisher.domain.clone()),
            page: Some(format!("https://{}", &settings.publisher.domain)),
        }),
        user: None,
        device: None,
        regs: None,
        ext: None,
    }
}


#[allow(dead_code)]
async fn handle_prebid_auction(
    settings: &Settings,
    mut req: Request,
    config: &PrebidIntegrationConfig,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Handling Prebid auction request");
    let mut openrtb_request: Json = serde_json::from_slice(&req.take_body_bytes()).change_context(
        TrustedServerError::Prebid {
            message: "Failed to parse OpenRTB request".to_string(),
        },
    )?;

    let synthetic_id = get_or_generate_synthetic_id(settings, &req)?;
    let fresh_id = generate_synthetic_id(settings, &req)?;

    log::info!(
        "Using synthetic ID: {}, fresh ID: {}",
        synthetic_id,
        fresh_id
    );

    enhance_openrtb_request(
        &mut openrtb_request,
        &synthetic_id,
        &fresh_id,
        settings,
        &req,
        config,
    )?;

    let mut pbs_req = Request::new(
        Method::POST,
        format!("{}/openrtb2/auction", config.server_url),
    );
    copy_request_headers(&req, &mut pbs_req);
    pbs_req
        .set_body_json(&openrtb_request)
        .change_context(TrustedServerError::Prebid {
            message: "Failed to set request body".to_string(),
        })?;

    log::info!("Sending request to Prebid Server");

    let backend_name = ensure_backend_from_url(&config.server_url)?;
    let mut pbs_response =
        pbs_req
            .send(backend_name)
            .change_context(TrustedServerError::Prebid {
                message: "Failed to send request to Prebid Server".to_string(),
            })?;

    if pbs_response.get_status().is_success() {
        let response_body = pbs_response.take_body_bytes();
        match serde_json::from_slice::<Json>(&response_body) {
            Ok(mut response_json) => {
                let request_info = RequestInfo::from_request(&req);
                transform_prebid_response(
                    &mut response_json,
                    &request_info.host,
                    &request_info.scheme,
                )?;

                let transformed_body = serde_json::to_vec(&response_json).change_context(
                    TrustedServerError::Prebid {
                        message: "Failed to serialize transformed response".to_string(),
                    },
                )?;

                Ok(Response::from_status(StatusCode::OK)
                    .with_header(header::CONTENT_TYPE, "application/json")
                    .with_header("X-Synthetic-ID", &synthetic_id)
                    .with_header(HEADER_SYNTHETIC_FRESH, &fresh_id)
                    .with_header(HEADER_SYNTHETIC_TRUSTED_SERVER, &synthetic_id)
                    .with_body(transformed_body))
            }
            Err(_) => Ok(Response::from_status(pbs_response.get_status())
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body(response_body)),
        }
    } else {
        Ok(pbs_response)
    }
}

#[allow(dead_code)]
fn enhance_openrtb_request(
    request: &mut Json,
    synthetic_id: &str,
    fresh_id: &str,
    settings: &Settings,
    req: &Request,
    config: &PrebidIntegrationConfig,
) -> Result<(), Report<TrustedServerError>> {
    if !request["user"].is_object() {
        request["user"] = json!({});
    }
    request["user"]["id"] = json!(synthetic_id);

    if !request["user"]["ext"].is_object() {
        request["user"]["ext"] = json!({});
    }
    request["user"]["ext"]["synthetic_fresh"] = json!(fresh_id);

    if req.get_header("Sec-GPC").is_some() {
        if !request["regs"].is_object() {
            request["regs"] = json!({});
        }
        if !request["regs"]["ext"].is_object() {
            request["regs"]["ext"] = json!({});
        }
        request["regs"]["ext"]["us_privacy"] = json!("1YYN");
    }

    if let Some(geo_info) = GeoInfo::from_request(req) {
        let geo_obj = json!({
            "type": 2,
            "country": geo_info.country,
            "city": geo_info.city,
            "region": geo_info.region,
        });

        if !request["device"].is_object() {
            request["device"] = json!({});
        }
        request["device"]["geo"] = geo_obj;
    }

    if !request["site"].is_object() {
        let mut page_url = format!("https://{}", settings.publisher.domain);

        // Append debug query params if configured
        if let Some(ref params) = config.debug_query_params {
            page_url = append_query_params(&page_url, params);
        }

        request["site"] = json!({
            "domain": settings.publisher.domain,
            "page": page_url,
        });
    } else if let Some(ref params) = config.debug_query_params {
        // If site already exists, append debug params to existing page URL
        if let Some(page_url) = request["site"]["page"].as_str() {
            let updated_url = append_query_params(page_url, params);
            if updated_url != page_url {
                request["site"]["page"] = json!(updated_url);
            }
        }
    }

    if let Some(request_signing_config) = &settings.request_signing {
        if request_signing_config.enabled && request["id"].is_string() {
            if !request["ext"].is_object() {
                request["ext"] = json!({});
            }

            let id = request["id"]
                .as_str()
                .expect("should have string id when is_string checked");
            let signer = RequestSigner::from_config()?;
            let signature = signer.sign(id.as_bytes())?;
            request["ext"]["trusted_server"] = json!({
                "signature": signature,
                "kid": signer.kid
            });
        }
    }

    if config.debug {
        if !request["ext"].is_object() {
            request["ext"] = json!({});
        }
        if !request["ext"]["prebid"].is_object() {
            request["ext"]["prebid"] = json!({});
        }
        request["ext"]["prebid"]["debug"] = json!(true);
    }

    Ok(())
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
    let cdn_patterns = vec![
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

/// Extracts the `adm` field from the first bid matching the given slot (by `impid`).
///
/// Searches through the OpenRTB seatbid/bid structure for a bid whose `impid`
/// matches `slot` and returns its `adm` (ad markup) value.
#[allow(dead_code)]
fn extract_adm_for_slot(response: &Json, slot: &str) -> Option<String> {
    let seatbids = response.get("seatbid")?.as_array()?;
    for seatbid in seatbids {
        let bids = seatbid.get("bid")?.as_array()?;
        for bid in bids {
            if bid.get("impid").and_then(|v| v.as_str()) == Some(slot) {
                return bid.get("adm").and_then(|v| v.as_str()).map(String::from);
            }
        }
    }
    None
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
    pub fn new(config: PrebidIntegrationConfig) -> Self {
        Self { config }
    }

    /// Convert auction request to OpenRTB format with all enrichments.
    fn to_openrtb(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
        signer: Option<(&RequestSigner, String)>,
    ) -> OpenRtbRequest {
        let imps: Vec<Imp> = request
            .slots
            .iter()
            .map(|slot| {
                let formats: Vec<Format> = slot
                    .formats
                    .iter()
                    .filter(|f| f.media_type == MediaType::Banner)
                    .map(|f| Format {
                        w: f.width,
                        h: f.height,
                    })
                    .collect();

                // Use bidder params from the slot (passed through from the request)
                let mut bidder: HashMap<String, Json> = slot
                    .bidders
                    .iter()
                    .map(|(name, params)| (name.clone(), params.clone()))
                    .collect();

                // Fallback to config bidders if none provided
                if bidder.is_empty() {
                    for b in &self.config.bidders {
                        bidder.insert(b.clone(), Json::Object(serde_json::Map::new()));
                    }
                }

                Imp {
                    id: slot.id.clone(),
                    banner: Some(Banner { format: formats }),
                    ext: Some(ImpExt {
                        prebid: PrebidImpExt { bidder },
                    }),
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

        // Build user object
        let user = Some(User {
            id: Some(request.user.id.clone()),
            ext: Some(UserExt {
                synthetic_fresh: Some(request.user.fresh_id.clone()),
            }),
        });

        // Build device object with geo if available
        let device = request.device.as_ref().and_then(|d| {
            d.geo.as_ref().map(|geo| Device {
                geo: Some(Geo {
                    geo_type: 2, // IP address per OpenRTB spec
                    country: Some(geo.country.clone()),
                    city: Some(geo.city.clone()),
                    region: geo.region.clone(),
                }),
            })
        });

        // Build regs object if Sec-GPC header is present
        let regs = if context.request.get_header("Sec-GPC").is_some() {
            Some(Regs {
                ext: Some(RegsExt {
                    us_privacy: Some("1YYN".to_string()),
                }),
            })
        } else {
            None
        };

        // Build ext object
        let request_info = RequestInfo::from_request(&context.request);
        let (signature, kid) = signer
            .map(|(s, sig)| (Some(sig), Some(s.kid.clone())))
            .unwrap_or((None, None));

        let ext = Some(RequestExt {
            prebid: if self.config.debug {
                Some(PrebidExt { debug: Some(true) })
            } else {
                None
            },
            trusted_server: Some(TrustedServerExt {
                signature,
                kid,
                request_host: Some(request_info.host),
                request_scheme: Some(request_info.scheme),
            }),
        });

        OpenRtbRequest {
            id: request.id.clone(),
            imp: imps,
            site: Some(Site {
                domain: Some(request.publisher.domain.clone()),
                page: page_url,
            }),
            user,
            device,
            regs,
            ext,
        }
    }

    /// Parse OpenRTB response into auction response.
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

    /// Parse a single bid from OpenRTB response.
    fn parse_bid(&self, bid_obj: &Json, seat: &str) -> Result<AuctionBid, ()> {
        let slot_id = bid_obj
            .get("impid")
            .and_then(|v| v.as_str())
            .ok_or(())?
            .to_string();

        let price = bid_obj.get("price").and_then(|v| v.as_f64()).ok_or(())?;

        let creative = bid_obj
            .get("adm")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let width = bid_obj.get("w").and_then(|v| v.as_u64()).unwrap_or(300) as u32;
        let height = bid_obj.get("h").and_then(|v| v.as_u64()).unwrap_or(250) as u32;

        let nurl = bid_obj
            .get("nurl")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let burl = bid_obj
            .get("burl")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let adomain = bid_obj
            .get("adomain")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
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
        let signer_with_signature =
            if let Some(request_signing_config) = &context.settings.request_signing {
                if request_signing_config.enabled {
                    let signer = RequestSigner::from_config()?;
                    let signature = signer.sign(request.id.as_bytes())?;
                    Some((signer, signature))
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
                .map(|(s, sig)| (s, sig.clone())),
        );

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
        let backend_name = ensure_backend_from_url(&self.config.server_url)?;
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
        if !response.get_status().is_success() {
            log::warn!(
                "Prebid returned non-success status: {}",
                response.get_status()
            );
            return Ok(AuctionResponse::error("prebid", response_time_ms));
        }

        let body_bytes = response.take_body_bytes();

        let mut response_json: Json =
            serde_json::from_slice(&body_bytes).change_context(TrustedServerError::Prebid {
                message: "Failed to parse Prebid response".to_string(),
            })?;

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

        let auction_response = self.parse_openrtb_response(&response_json, response_time_ms);

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
        ensure_backend_from_url(&self.config.server_url).ok()
    }
}

// ============================================================================
// Provider Auto-Registration
// ============================================================================

/// Auto-register Prebid provider based on settings configuration.
///
/// This function checks the settings for Prebid configuration and returns
/// the provider if enabled.
pub fn register_auction_provider(settings: &Settings) -> Vec<Arc<dyn AuctionProvider>> {
    let mut providers: Vec<Arc<dyn AuctionProvider>> = Vec::new();

    // Prebid provider is always registered if integration is enabled
    if let Ok(Some(config)) = settings.integration_config::<PrebidIntegrationConfig>("prebid") {
        log::info!("Registering Prebid auction provider");
        providers.push(Arc::new(PrebidAuctionProvider::new(config)));
    }

    providers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_processor::{create_html_processor, HtmlProcessorConfig};
    use crate::integrations::{AttributeRewriteAction, IntegrationRegistry};
    use crate::settings::Settings;
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::crate_test_settings_str;
    use fastly::http::Method;
    use serde_json::json;
    use std::io::Cursor;

    fn make_settings() -> Settings {
        Settings::from_toml(&crate_test_settings_str()).expect("should parse settings")
    }

    fn base_config() -> PrebidIntegrationConfig {
        PrebidIntegrationConfig {
            enabled: true,
            server_url: "https://prebid.example".to_string(),
            timeout_ms: 1000,
            bidders: vec!["exampleBidder".to_string()],
            debug: false,
            debug_query_params: None,
            script_patterns: default_script_patterns(),
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
        let registry = IntegrationRegistry::new(&settings);
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
        let registry = IntegrationRegistry::new(&settings);
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
            "Prebid script should be removed when script patterns match"
        );
        assert!(
            !processed.contains("cdn.prebid.org/prebid.js"),
            "Prebid preload should be removed when script patterns match"
        );
    }

    #[test]
    fn enhance_openrtb_request_adds_ids_and_regs() {
        let settings = make_settings();
        let mut request_json = json!({
            "id": "openrtb-request-id"
        });

        let synthetic_id = "synthetic-123";
        let fresh_id = "fresh-456";
        let mut req = Request::new(Method::POST, "https://edge.example/auction");
        req.set_header("Sec-GPC", "1");

        let config = base_config();

        enhance_openrtb_request(
            &mut request_json,
            synthetic_id,
            fresh_id,
            &settings,
            &req,
            &config,
        )
        .expect("should enhance request");

        assert_eq!(request_json["user"]["id"], synthetic_id);
        assert_eq!(request_json["user"]["ext"]["synthetic_fresh"], fresh_id);
        assert_eq!(
            request_json["regs"]["ext"]["us_privacy"], "1YYN",
            "GPC header should map to US privacy flag"
        );
        assert_eq!(
            request_json["site"]["domain"], settings.publisher.domain,
            "site domain should match publisher domain"
        );
        assert!(
            request_json["site"]["page"]
                .as_str()
                .unwrap()
                .starts_with("https://"),
            "site page should be populated"
        );
    }

    #[test]
    fn enhance_openrtb_request_adds_debug_flag_when_enabled() {
        let settings = make_settings();
        let mut request_json = json!({
            "id": "openrtb-request-id"
        });

        let synthetic_id = "synthetic-123";
        let fresh_id = "fresh-456";
        let req = Request::new(Method::POST, "https://edge.example/auction");

        let mut config = base_config();
        config.debug = true;

        enhance_openrtb_request(
            &mut request_json,
            synthetic_id,
            fresh_id,
            &settings,
            &req,
            &config,
        )
        .expect("should enhance request");

        assert_eq!(
            request_json["ext"]["prebid"]["debug"], true,
            "debug flag should be set to true when config.debug is enabled"
        );
    }

    #[test]
    fn enhance_openrtb_request_does_not_add_debug_flag_when_disabled() {
        let settings = make_settings();
        let mut request_json = json!({
            "id": "openrtb-request-id"
        });

        let synthetic_id = "synthetic-123";
        let fresh_id = "fresh-456";
        let req = Request::new(Method::POST, "https://edge.example/auction");

        let mut config = base_config();
        config.debug = false;

        enhance_openrtb_request(
            &mut request_json,
            synthetic_id,
            fresh_id,
            &settings,
            &req,
            &config,
        )
        .expect("should enhance request");

        assert!(
            request_json["ext"]["prebid"]["debug"].is_null(),
            "debug flag should not be set when config.debug is disabled"
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
                .unwrap();
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

        let encoded = rewritten.split("/ad-proxy/track/").nth(1).unwrap();
        let decoded = BASE64
            .decode(encoded.as_bytes())
            .expect("should decode base64 proxy payload");
        assert_eq!(String::from_utf8(decoded).unwrap(), url);
    }

    #[test]
    fn matches_script_url_matches_common_variants() {
        let integration = PrebidIntegration::new(base_config());
        assert!(integration.matches_script_url("https://cdn.com/prebid.js"));
        assert!(integration.matches_script_url(
            "https://cdn.com/prebid.min.js?version=1"
        ));
        assert!(!integration.matches_script_url("https://cdn.com/app.js"));
    }

    #[test]
    fn test_script_patterns_config_parsing() {
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

[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
script_patterns = ["/prebid.js", "/custom/prebid.min.js"]
"#;

        let settings = Settings::from_toml(toml_str).expect("should parse TOML");
        let config = settings
            .integration_config::<PrebidIntegrationConfig>("prebid")
            .expect("should get config")
            .expect("should be enabled");

        assert_eq!(config.script_patterns.len(), 2);
        assert!(config.script_patterns.contains(&"/prebid.js".to_string()));
        assert!(config.script_patterns.contains(&"/custom/prebid.min.js".to_string()));
    }

    #[test]
    fn test_script_patterns_defaults() {
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

[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
"#;

        let settings = Settings::from_toml(toml_str).expect("should parse TOML");
        let config = settings
            .integration_config::<PrebidIntegrationConfig>("prebid")
            .expect("should get config")
            .expect("should be enabled");

        // Should have default script patterns
        assert!(!config.script_patterns.is_empty());
        assert!(config.script_patterns.contains(&"/prebid.js".to_string()));
        assert!(config.script_patterns.contains(&"/prebid.min.js".to_string()));
    }

    #[test]
    fn test_script_handler_returns_empty_js() {
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
        assert!(cache_control.contains("immutable"));

        let body = response.into_body_str();
        assert!(body.contains("// Script overridden by Trusted Server"));
    }

    #[test]
    fn test_routes_includes_script_patterns() {
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
        assert!(has_prebid_min_js_route, "should register /prebid.min.js route");
    }

    #[test]
    fn test_routes_with_empty_script_patterns() {
        let mut config = base_config();
        config.script_patterns = vec![];
        let integration = PrebidIntegration::new(config);

        let routes = integration.routes();

        // Should have 0 routes when no script patterns configured
        assert_eq!(routes.len(), 0);
    }

    #[test]
    fn debug_query_params_appended_to_existing_site_page_in_enhance() {
        let settings = make_settings();
        let mut config = base_config();
        config.debug_query_params = Some("kargo_debug=true".to_string());

        let req = Request::new(Method::GET, "https://example.com/test");
        let synthetic_id = "test-synthetic-id";
        let fresh_id = "test-fresh-id";

        // Test with existing site.page
        let mut request = json!({
            "id": "test-id",
            "site": {
                "domain": "example.com",
                "page": "https://example.com/page"
            }
        });

        enhance_openrtb_request(
            &mut request,
            synthetic_id,
            fresh_id,
            &settings,
            &req,
            &config,
        )
        .expect("should enhance request");

        let page = request["site"]["page"].as_str().unwrap();
        assert_eq!(page, "https://example.com/page?kargo_debug=true");
    }

    #[test]
    fn debug_query_params_appended_to_url_with_existing_query() {
        let settings = make_settings();
        let mut config = base_config();
        config.debug_query_params = Some("kargo_debug=true".to_string());

        let req = Request::new(Method::GET, "https://example.com/test");
        let synthetic_id = "test-synthetic-id";
        let fresh_id = "test-fresh-id";

        // Test with existing query params in site.page
        let mut request = json!({
            "id": "test-id",
            "site": {
                "domain": "example.com",
                "page": "https://example.com/page?existing=param"
            }
        });

        enhance_openrtb_request(
            &mut request,
            synthetic_id,
            fresh_id,
            &settings,
            &req,
            &config,
        )
        .expect("should enhance request");

        let page = request["site"]["page"].as_str().unwrap();
        assert_eq!(
            page,
            "https://example.com/page?existing=param&kargo_debug=true"
        );
    }

    #[test]
    fn debug_query_params_not_duplicated() {
        // Verify that if params are already in the URL, they aren't added again
        let settings = make_settings();
        let mut config = base_config();
        config.debug_query_params = Some("kargo_debug=true".to_string());

        let req = Request::new(Method::GET, "https://example.com/test");
        let synthetic_id = "test-synthetic-id";
        let fresh_id = "test-fresh-id";

        // Test with URL that already has the debug params
        let mut request = json!({
            "id": "test-id",
            "site": {
                "domain": "example.com",
                "page": "https://example.com/page?kargo_debug=true"
            }
        });

        enhance_openrtb_request(
            &mut request,
            synthetic_id,
            fresh_id,
            &settings,
            &req,
            &config,
        )
        .expect("should enhance request");

        let page = request["site"]["page"].as_str().unwrap();
        // Should still only have params once
        assert_eq!(page, "https://example.com/page?kargo_debug=true");
        // Verify params appear exactly once
        assert_eq!(page.matches("kargo_debug=true").count(), 1);
    }
}
