use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
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
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
};
use crate::openrtb::{Banner, Format, Imp, ImpExt, OpenRtbRequest, PrebidImpExt, Site};
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
    #[serde(default = "default_auto_configure")]
    pub auto_configure: bool,
    #[serde(default)]
    pub debug: bool,
    #[serde(default)]
    pub script_handler: Option<String>,
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

fn default_auto_configure() -> bool {
    true
}

fn default_enabled() -> bool {
    true
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

        let mut pbs_resp = pbs_auction_for_get(settings, req, &self.config).await?;

        let body_bytes = pbs_resp.take_body_bytes();
        let html = match serde_json::from_slice::<Json>(&body_bytes) {
            Ok(json) => extract_adm_for_slot(&json, &slot)
                .unwrap_or_else(|| "<!-- no creative -->".to_string()),
            Err(_) => String::from_utf8(body_bytes).unwrap_or_else(|_| "".to_string()),
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

        if let Some(script_path) = &self.config.script_handler {
            routes.push(IntegrationEndpoint::get(script_path.clone()));
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
            Method::GET if self.config.script_handler.as_ref() == Some(&path) => {
                self.handle_script_handler()
            }
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
        self.config.auto_configure && matches!(attribute, "src" | "href")
    }

    fn rewrite(
        &self,
        _attr_name: &str,
        attr_value: &str,
        _ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if self.config.auto_configure && is_prebid_script_url(attr_value) {
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
            page: Some(format!("https://{}", settings.publisher.domain)),
        }),
    }
}

fn is_prebid_script_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let without_query = lower.split('?').next().unwrap_or("");
    let filename = without_query.rsplit('/').next().unwrap_or("");
    matches!(
        filename,
        "prebid.js" | "prebid.min.js" | "prebidjs.js" | "prebidjs.min.js"
    )
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
                let request_host = get_request_host(&req);
                let request_scheme = get_request_scheme(&req);
                transform_prebid_response(&mut response_json, &request_host, &request_scheme)?;

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
        request["site"] = json!({
            "domain": settings.publisher.domain,
            "page": format!("https://{}", settings.publisher.domain),
        });
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

fn get_request_host(req: &Request) -> String {
    req.get_header(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string()
}

fn get_request_scheme(req: &Request) -> String {
    if req.get_tls_protocol().is_some() || req.get_tls_cipher_openssl_name().is_some() {
        return "https".to_string();
    }

    if let Some(proto) = req.get_header("X-Forwarded-Proto") {
        if let Ok(proto_str) = proto.to_str() {
            return proto_str.to_lowercase();
        }
    }

    "https".to_string()
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

    /// Convert auction request to OpenRTB format.
    fn to_openrtb(&self, request: &AuctionRequest) -> OpenRtbRequest {
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

                let mut bidder = std::collections::HashMap::new();
                for bidder_name in &self.config.bidders {
                    bidder.insert(bidder_name.clone(), Json::Object(serde_json::Map::new()));
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

        OpenRtbRequest {
            id: request.id.clone(),
            imp: imps,
            site: Some(Site {
                domain: Some(request.publisher.domain.clone()),
                page: request.publisher.page_url.clone(),
            }),
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

        // Convert to OpenRTB
        let openrtb = self.to_openrtb(request);
        let mut openrtb_json =
            serde_json::to_value(&openrtb).change_context(TrustedServerError::Prebid {
                message: "Failed to serialize OpenRTB request".to_string(),
            })?;

        // Enhance with user info
        if !openrtb_json["user"].is_object() {
            openrtb_json["user"] = json!({});
        }
        openrtb_json["user"]["id"] = json!(&request.user.id);
        if !openrtb_json["user"]["ext"].is_object() {
            openrtb_json["user"]["ext"] = json!({});
        }
        openrtb_json["user"]["ext"]["synthetic_fresh"] = json!(&request.user.fresh_id);

        // Add device info if available
        if let Some(device) = &request.device {
            if let Some(geo) = &device.geo {
                let geo_obj = json!({
                    "type": 2,
                    "country": geo.country,
                    "city": geo.city,
                    "region": geo.region,
                });
                if !openrtb_json["device"].is_object() {
                    openrtb_json["device"] = json!({});
                }
                openrtb_json["device"]["geo"] = geo_obj;
            }
        }

        // Add privacy regulations based on Sec-GPC header
        if context.request.get_header("Sec-GPC").is_some() {
            if !openrtb_json["regs"].is_object() {
                openrtb_json["regs"] = json!({});
            }
            if !openrtb_json["regs"]["ext"].is_object() {
                openrtb_json["regs"]["ext"] = json!({});
            }
            openrtb_json["regs"]["ext"]["us_privacy"] = json!("1YYN");
        }

        if !openrtb_json["ext"].is_object() {
            openrtb_json["ext"] = json!({});
        }
        if !openrtb_json["ext"]["trusted_server"].is_object() {
            openrtb_json["ext"]["trusted_server"] = json!({});
        }

        let request_host = get_request_host(context.request);
        let request_scheme = get_request_scheme(context.request);
        openrtb_json["ext"]["trusted_server"]["request_host"] = json!(request_host);
        openrtb_json["ext"]["trusted_server"]["request_scheme"] = json!(request_scheme);

        // Add request signing if enabled
        if let Some(request_signing_config) = &context.settings.request_signing {
            if request_signing_config.enabled && openrtb_json["id"].is_string() {
                let id = openrtb_json["id"]
                    .as_str()
                    .expect("should have string id when is_string checked");
                let signer = RequestSigner::from_config()?;
                let signature = signer.sign(id.as_bytes())?;

                openrtb_json["ext"]["trusted_server"]["signature"] = json!(signature);
                openrtb_json["ext"]["trusted_server"]["kid"] = json!(signer.kid);
            }
        }

        // Create HTTP request
        let mut pbs_req = Request::new(
            Method::POST,
            format!("{}/openrtb2/auction", self.config.server_url),
        );
        copy_request_headers(context.request, &mut pbs_req);

        pbs_req
            .set_body_json(&openrtb_json)
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
            auto_configure: true,
            debug: false,
            script_handler: None,
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
    fn html_processor_keeps_prebid_scripts_when_auto_config_disabled() {
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
                    "auto_configure": false,
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
            "Prebid script should remain when auto-config is disabled"
        );
        assert!(
            processed.contains("cdn.prebid.org/prebid.js"),
            "Prebid preload should remain when auto-config is disabled"
        );
    }

    #[test]
    fn html_processor_removes_prebid_scripts_when_auto_config_enabled() {
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
                    "auto_configure": true,
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
            "Prebid script should be removed when auto-config is enabled"
        );
        assert!(
            !processed.contains("cdn.prebid.org/prebid.js"),
            "Prebid preload should be removed when auto-config is enabled"
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

        enhance_openrtb_request(&mut request_json, synthetic_id, fresh_id, &settings, &req)
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
    fn is_prebid_script_url_matches_common_variants() {
        assert!(is_prebid_script_url("https://cdn.com/prebid.js"));
        assert!(is_prebid_script_url(
            "https://cdn.com/prebid.min.js?version=1"
        ));
        assert!(!is_prebid_script_url("https://cdn.com/app.js"));
    }

    #[test]
    fn test_script_handler_config_parsing() {
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
script_handler = "/prebid.js"
"#;

        let settings = Settings::from_toml(toml_str).expect("should parse TOML");
        let config = settings
            .integration_config::<PrebidIntegrationConfig>("prebid")
            .expect("should get config")
            .expect("should be enabled");

        assert_eq!(config.script_handler, Some("/prebid.js".to_string()));
    }

    #[test]
    fn test_script_handler_none_by_default() {
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

        assert_eq!(config.script_handler, None);
    }

    #[test]
    fn test_script_handler_returns_empty_js() {
        let config = PrebidIntegrationConfig {
            enabled: true,
            server_url: "https://prebid.example".to_string(),
            timeout_ms: 1000,
            bidders: vec![],
            auto_configure: false,
            debug: false,
            script_handler: Some("/prebid.js".to_string()),
        };
        let integration = PrebidIntegration::new(config);

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
    fn test_routes_includes_script_handler() {
        let config = PrebidIntegrationConfig {
            enabled: true,
            server_url: "https://prebid.example".to_string(),
            timeout_ms: 1000,
            bidders: vec![],
            auto_configure: false,
            debug: false,
            script_handler: Some("/prebid.js".to_string()),
        };
        let integration = PrebidIntegration::new(config);

        let routes = integration.routes();

        // Should have 1 route: script handler
        assert_eq!(routes.len(), 1);

        let has_script_route = routes
            .iter()
            .any(|r| r.path == "/prebid.js" && r.method == Method::GET);
        assert!(has_script_route, "should register script handler route");
    }

    #[test]
    fn test_routes_without_script_handler() {
        let config = base_config(); // Has script_handler: None
        let integration = PrebidIntegration::new(config);

        let routes = integration.routes();

        // Should have 0 routes when no script handler configured
        assert_eq!(routes.len(), 0);
    }
}
