use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, Value as JsonValue};
use url::{form_urlencoded, Url};
use validator::Validate;

use crate::backend::ensure_backend_from_url;
use crate::constants::{HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER};
use crate::creative;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::http_util::compute_encrypted_sha256_token;
use crate::http_util::RequestInfo;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration,
};
use crate::openrtb::{
    Banner, Device, Format, Geo, Imp, ImpExt, OpenRtbRequest, OpenRtbResponse, PrebidImpExt, Regs,
    RegsExt, RequestExt, Site, TrustedServerExt, User, UserExt,
};
use crate::request_signing::RequestSigner;
use crate::settings::{IntegrationConfig, Settings};
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};

const PREBID_INTEGRATION_ID: &str = "prebid";
const ROUTE_RENDER: &str = "/ad/render";
const ROUTE_AUCTION: &str = "/ad/auction";

/// Mode determines how TSJS handles ad requests.
///
/// - `Render`: Uses iframe-based server-side rendering.
///   The server handles the full auction and returns ready-to-display HTML.
/// - `Auction`: Uses client-side OpenRTB auctions.
///   Clients (for example, Prebid.js) send OpenRTB to /ad/auction.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Render,
    Auction,
}

/// Generate TSJS config script tag based on mode.
/// This script runs after the unified TSJS bundle has loaded.
pub fn tsjs_config_script_tag(mode: Mode) -> String {
    match mode {
        Mode::Render => {
            r#"<script>window.tsjs&&tsjs.setConfig({mode:"render"});</script>"#.to_string()
        }
        Mode::Auction => {
            r#"<script>window.tsjs&&tsjs.setConfig({mode:"auction"});</script>"#.to_string()
        }
    }
}

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
    /// Optional default mode to enqueue when injecting the unified bundle.
    #[serde(default)]
    pub mode: Option<Mode>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BannerUnit {
    sizes: Vec<Vec<u32>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaTypes {
    #[allow(dead_code)]
    banner: Option<BannerUnit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdUnit {
    code: String,
    #[allow(dead_code)]
    media_types: Option<MediaTypes>,
    #[serde(default)]
    bids: Option<Vec<Bid>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdRequest {
    ad_units: Vec<AdUnit>,
    #[allow(dead_code)]
    config: Option<JsonValue>,
}

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

        log::debug!(
            "matches_script_pattern: path='{}', patterns={:?}",
            path,
            self.config.script_patterns
        );

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

                log::debug!(
                    "  wildcard pattern='{}', prefix='{}', path_starts_with_prefix={}",
                    pattern,
                    prefix,
                    path_lower.starts_with(prefix)
                );

                if path_lower.starts_with(prefix) {
                    // Check if it ends with a known Prebid script suffix
                    let has_suffix = PREBID_SCRIPT_SUFFIXES
                        .iter()
                        .any(|suffix| path_lower.ends_with(suffix));
                    log::debug!(
                        "  checking suffixes: path ends with known suffix={}",
                        has_suffix
                    );
                    if has_suffix {
                        return true;
                    }
                }
            } else {
                // Exact match or suffix match
                let matches = path_lower.ends_with(&pattern_lower);
                log::debug!("  exact/suffix pattern='{}', matches={}", pattern, matches);
                if matches {
                    return true;
                }
            }
        }
        log::debug!("  no pattern matched, returning false");
        false
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: PREBID_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    async fn handle_auction(
        &self,
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        handle_prebid_auction(settings, req, &self.config).await
    }

    fn handle_script_handler(&self) -> Result<Response, Report<TrustedServerError>> {
        // Serve empty JS - Prebid.js is already bundled in the unified TSJS bundle
        // that gets injected into the page. We just need to prevent the original
        // Prebid script from loading/executing.
        Ok(Response::from_status(StatusCode::OK)
            .with_header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .with_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
            .with_body("/* prebid.js replaced by tsjs-unified */"))
    }

    async fn handle_render(
        &self,
        settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let url = req.get_url_str();
        let parsed = Url::parse(url).change_context(TrustedServerError::Prebid {
            message: "Invalid render URL".to_string(),
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

        let rewritten = creative::rewrite_creative_html(&html, settings);

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
        let mut routes = vec![
            IntegrationEndpoint::get(ROUTE_RENDER),
            IntegrationEndpoint::post(ROUTE_AUCTION),
        ];

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
        settings: &Settings,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.get_path().to_string();
        let method = req.get_method().clone();

        log::debug!(
            "Prebid handle: method={}, path='{}', script_patterns={:?}",
            method,
            path,
            self.config.script_patterns
        );

        match method {
            Method::GET if path == ROUTE_RENDER => self.handle_render(settings, req).await,
            Method::POST if path == ROUTE_AUCTION => self.handle_auction(settings, req).await,
            // Serve empty JS for any other GET request that was routed here by matchit
            // (i.e., matched one of our script_patterns wildcards).
            // Prebid.js is already bundled in tsjs-unified, so we just need to
            // prevent the original script from loading.
            Method::GET => {
                log::debug!("Prebid: serving empty JS stub for path '{}'", path);
                self.handle_script_handler()
            }
            _ => {
                log::debug!(
                    "Prebid: no handler matched for {} '{}', returning error",
                    method,
                    path
                );
                Err(Report::new(Self::error(format!(
                    "Unsupported Prebid route: {path}"
                ))))
            }
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
        // Only inject TSJS mode config if mode is set
        // The Prebid bundle (served via script interception) already has s2sConfig built-in
        // GAM config is now handled by the separate GAM integration
        self.config
            .mode
            .map(|mode| vec![tsjs_config_script_tag(mode)])
            .unwrap_or_default()
    }
}

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
                banner: Some(Banner {
                    format: formats,
                    extra: HashMap::new(),
                }),
                ext: Some(ImpExt {
                    prebid: Some(PrebidImpExt { bidder }),
                    extra: HashMap::new(),
                }),
                extra: HashMap::new(),
            }
        })
        .collect();

    OpenRtbRequest {
        id: Uuid::new_v4().to_string(),
        imp: imps,
        site: Some(Site {
            domain: Some(settings.publisher.domain.clone()),
            page: Some(format!("https://{}", settings.publisher.domain)),
            extra: HashMap::new(),
        }),
        user: None,
        regs: None,
        device: None,
        ext: None,
        extra: HashMap::new(),
    }
}

async fn pbs_auction_for_get(
    settings: &Settings,
    req: Request,
    config: &PrebidIntegrationConfig,
) -> Result<Response, Report<TrustedServerError>> {
    handle_prebid_auction(settings, req, config).await
}

fn extract_adm_for_slot(json: &Json, slot: &str) -> Option<String> {
    let seatbids = json.get("seatbid")?.as_array()?;
    for sb in seatbids {
        if let Some(bids) = sb.get("bid").and_then(|b| b.as_array()) {
            for bid in bids {
                let impid = bid.get("impid").and_then(|v| v.as_str()).unwrap_or("");
                if impid == slot {
                    if let Some(adm) = bid.get("adm").and_then(|v| v.as_str()) {
                        return Some(adm.to_string());
                    }
                }
            }
        }
    }
    for sb in seatbids {
        if let Some(bids) = sb.get("bid").and_then(|b| b.as_array()) {
            for bid in bids {
                if let Some(adm) = bid.get("adm").and_then(|v| v.as_str()) {
                    return Some(adm.to_string());
                }
            }
        }
    }
    None
}

async fn handle_prebid_auction(
    settings: &Settings,
    mut req: Request,
    config: &PrebidIntegrationConfig,
) -> Result<Response, Report<TrustedServerError>> {
    log::info!("Handling Prebid auction request");
    let mut openrtb_request: OpenRtbRequest = serde_json::from_slice(&req.take_body_bytes())
        .change_context(TrustedServerError::Prebid {
            message: "Failed to parse OpenRTB request".to_string(),
        })?;

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
    override_prebid_bidders(&mut openrtb_request, &config.bidders);

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
    let backend_name =
        ensure_backend_from_url(&config.server_url, settings.proxy.certificate_check)?;
    let mut pbs_response =
        pbs_req
            .send(backend_name)
            .change_context(TrustedServerError::Prebid {
                message: "Failed to send request to Prebid Server".to_string(),
            })?;

    if pbs_response.get_status().is_success() {
        let response_body = pbs_response.take_body_bytes();
        match serde_json::from_slice::<OpenRtbResponse>(&response_body) {
            Ok(mut response) => {
                let request_info = RequestInfo::from_request(&req);
                transform_prebid_response(
                    &mut response,
                    settings,
                    &request_info.host,
                    &request_info.scheme,
                )?;

                let transformed_body =
                    serde_json::to_vec(&response).change_context(TrustedServerError::Prebid {
                        message: "Failed to serialize transformed response".to_string(),
                    })?;

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

fn enhance_openrtb_request(
    request: &mut OpenRtbRequest,
    synthetic_id: &str,
    fresh_id: &str,
    settings: &Settings,
    req: &Request,
) -> Result<(), Report<TrustedServerError>> {
    let user = request.user.get_or_insert_with(User::default);
    user.id = Some(synthetic_id.to_string());
    let user_ext = user.ext.get_or_insert_with(UserExt::default);
    user_ext.synthetic_fresh = Some(fresh_id.to_string());

    if req.get_header("Sec-GPC").is_some() {
        let regs = request.regs.get_or_insert_with(Regs::default);
        let regs_ext = regs.ext.get_or_insert_with(RegsExt::default);
        regs_ext.us_privacy = Some("1YYN".to_string());
    }

    if let Some(geo_info) = GeoInfo::from_request(req) {
        let device = request.device.get_or_insert_with(Device::default);
        let geo = device.geo.get_or_insert_with(Geo::default);
        geo.geo_type = Some(2);
        geo.country = Some(geo_info.country);
        geo.city = Some(geo_info.city);
        geo.region = geo_info.region;
    }

    if request.site.is_none() {
        request.site = Some(Site {
            domain: Some(settings.publisher.domain.clone()),
            page: Some(format!("https://{}", settings.publisher.domain)),
            extra: HashMap::new(),
        });
    }

    if let Some(request_signing_config) = &settings.request_signing {
        if request_signing_config.enabled {
            let signer = RequestSigner::from_config()?;
            let signature = signer.sign(request.id.as_bytes())?;
            let ext = request.ext.get_or_insert_with(RequestExt::default);
            let trusted = ext
                .trusted_server
                .get_or_insert_with(TrustedServerExt::default);
            trusted.signature = Some(signature);
            trusted.kid = Some(signer.kid);
        }
    }

    Ok(())
}

/// Override bidders in the OpenRTB request with the server-configured bidder list.
///
/// This replaces any client-provided bidder configuration with the bidders specified
/// in `[integrations.prebid].bidders` from trusted-server.toml. This ensures that:
///
/// 1. Only approved bidders are used (security/compliance control)
/// 2. Bidder params are managed server-side, not exposed to clients
/// 3. The publisher's Prebid.js config doesn't need bidder-specific setup
///
/// Note: Client-provided bidder params (e.g., `appnexus: { placementId: '123' }`)
/// are intentionally stripped. Bidder params should be configured in Prebid Server
/// or through trusted-server's bidder configuration.
fn override_prebid_bidders(request: &mut OpenRtbRequest, bidders: &[String]) {
    let bidder_map = bidders
        .iter()
        .map(|bidder| (bidder.clone(), JsonValue::Object(serde_json::Map::new())))
        .collect::<HashMap<_, _>>();

    log::debug!(
        "Overriding OpenRTB bidders from settings: count={}",
        bidder_map.len()
    );

    for imp in &mut request.imp {
        let ext = imp.ext.get_or_insert_with(|| ImpExt {
            prebid: None,
            extra: HashMap::new(),
        });
        let prebid = ext.prebid.get_or_insert_with(PrebidImpExt::default);
        prebid.bidder = bidder_map.clone();
    }
}

fn transform_prebid_response(
    response: &mut OpenRtbResponse,
    settings: &Settings,
    request_host: &str,
    request_scheme: &str,
) -> Result<(), Report<TrustedServerError>> {
    let Some(seatbids) = response.seatbid.as_mut() else {
        return Ok(());
    };

    for seatbid in seatbids {
        let Some(bids) = seatbid.bid.as_mut() else {
            continue;
        };

        for bid in bids {
            if let Some(adm) = bid.adm.as_deref() {
                if looks_like_html(adm) {
                    bid.adm = Some(creative::rewrite_creative_html(adm, settings));
                }
            }

            if let Some(nurl) = bid.nurl.as_deref() {
                bid.nurl = Some(first_party_proxy_url(
                    settings,
                    request_host,
                    request_scheme,
                    nurl,
                ));
            }

            if let Some(burl) = bid.burl.as_deref() {
                bid.burl = Some(first_party_proxy_url(
                    settings,
                    request_host,
                    request_scheme,
                    burl,
                ));
            }
        }
    }

    Ok(())
}

fn first_party_proxy_url(
    settings: &Settings,
    request_host: &str,
    request_scheme: &str,
    clear_url: &str,
) -> String {
    let trimmed = clear_url.trim();
    let Some(abs) = normalize_absolute_url(trimmed, request_scheme) else {
        return clear_url.to_string();
    };
    if is_excluded_by_domain(settings, &abs) {
        return clear_url.to_string();
    }
    let signed = creative::build_proxy_url(settings, &abs);
    let proxy_path = if signed == abs {
        build_proxy_path_for_raw_url(settings, &abs)
    } else {
        signed
    };
    absolutize_proxy_path(settings, request_host, request_scheme, proxy_path)
}

fn absolutize_proxy_path(
    settings: &Settings,
    request_host: &str,
    request_scheme: &str,
    proxy_path: String,
) -> String {
    if proxy_path.starts_with('/') {
        let host = if request_host.is_empty() {
            settings.publisher.domain.as_str()
        } else {
            request_host
        };
        if host.is_empty() {
            return proxy_path;
        }
        return format!("{request_scheme}://{host}{proxy_path}");
    }
    proxy_path
}

fn looks_like_html(markup: &str) -> bool {
    let trimmed = markup.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with('<') {
        return false;
    }
    if lower.starts_with("<?xml") || lower.starts_with("<vast") {
        return false;
    }
    [
        "<!doctype html",
        "<html",
        "<body",
        "<div",
        "<span",
        "<img",
        "<script",
        "<iframe",
        "<a",
        "<link",
        "<style",
        "<video",
        "<audio",
        "<source",
        "<object",
        "<embed",
        "<input",
        "<svg",
        "<table",
        "<p",
        "<canvas",
        "<meta",
        "<form",
    ]
    .iter()
    .any(|token| lower.contains(token))
}

fn normalize_absolute_url(url: &str, request_scheme: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(url.to_string());
    }
    if url.starts_with("//") {
        return Some(format!("{}:{}", request_scheme, url));
    }
    None
}

fn is_excluded_by_domain(settings: &Settings, abs_url: &str) -> bool {
    if settings.rewrite.exclude_domains.is_empty() {
        return false;
    }
    if let Ok(parsed) = Url::parse(abs_url) {
        return settings.rewrite.is_excluded(parsed.as_str());
    }
    let Some(host) = extract_host(abs_url) else {
        return false;
    };
    let check = format!("https://{}", host);
    settings.rewrite.is_excluded(&check)
}

fn extract_host(abs_url: &str) -> Option<&str> {
    let lower = abs_url.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &abs_url["https://".len()..]
    } else if lower.starts_with("http://") {
        &abs_url["http://".len()..]
    } else {
        return None;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    Some(host)
}

fn build_proxy_path_for_raw_url(settings: &Settings, clear_url: &str) -> String {
    let token = compute_encrypted_sha256_token(settings, clear_url);
    let mut qs = form_urlencoded::Serializer::new(String::new());
    qs.append_pair("tsurl", clear_url);
    qs.append_pair("tstoken", &token);
    format!("/first-party/proxy?{}", qs.finish())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{AttributeRewriteAction, IntegrationAttributeContext};
    use crate::settings::Settings;
    use crate::test_support::tests::crate_test_settings_str;
    use fastly::http::Method;
    use serde_json::json;

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
            mode: None,
            script_patterns: default_script_patterns(),
        }
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
        assert!(
            matches!(rewritten, AttributeRewriteAction::RemoveElement),
            "Prebid script tags should be removed"
        );

        let untouched = integration.rewrite("src", "https://cdn.example.com/app.js", &ctx);
        assert!(
            matches!(untouched, AttributeRewriteAction::Keep),
            "Non-Prebid scripts should remain"
        );
    }

    #[test]
    fn attribute_rewriter_handles_query_strings() {
        let integration = PrebidIntegration::new(base_config());
        let ctx = IntegrationAttributeContext {
            attribute_name: "href",
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
        };

        let rewritten =
            integration.rewrite("href", "https://cdn.prebid.org/prebid.js?v=1.2.3", &ctx);
        assert!(
            matches!(rewritten, AttributeRewriteAction::RemoveElement),
            "Prebid links with query strings should be removed"
        );
    }

    #[test]
    fn attribute_rewriter_matches_wildcard_patterns() {
        let mut config = base_config();
        config.script_patterns = vec!["/static/prebid/*".to_string()];
        let integration = PrebidIntegration::new(config);
        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
        };

        let rewritten = integration.rewrite(
            "src",
            "https://cdn.example.com/static/prebid/v1/prebid.min.js",
            &ctx,
        );
        assert!(
            matches!(rewritten, AttributeRewriteAction::RemoveElement),
            "Wildcard patterns should match prebid assets on full URLs"
        );

        let rewritten_relative = integration.rewrite("src", "static/prebid/prebid.min.js", &ctx);
        assert!(
            matches!(rewritten_relative, AttributeRewriteAction::RemoveElement),
            "Wildcard patterns should match relative paths without a leading slash"
        );
    }

    #[test]
    fn script_pattern_matching_exact_paths() {
        let integration = PrebidIntegration::new(base_config());

        // Should match default exact patterns (suffix matching)
        assert!(integration.matches_script_pattern("/prebid.js"));
        assert!(integration.matches_script_pattern("/prebid.min.js"));
        assert!(integration.matches_script_pattern("/prebidjs.js"));
        assert!(integration.matches_script_pattern("/prebidjs.min.js"));

        // Suffix matching means nested paths also match
        assert!(integration.matches_script_pattern("/static/prebid.min.js"));
        assert!(integration.matches_script_pattern("/static/prebid/v8.53.0/prebid.min.js"));

        // Should not match other scripts
        assert!(!integration.matches_script_pattern("/app.js"));
        assert!(!integration.matches_script_pattern("/static/bundle.min.js"));
    }

    #[test]
    fn script_pattern_matching_wildcard_slash_star() {
        // Test /* wildcard pattern matching
        let mut config = base_config();
        config.script_patterns = vec!["/static/prebid/*".to_string()];
        let integration = PrebidIntegration::new(config);

        // Should match paths under the prefix with known suffixes
        assert!(integration.matches_script_pattern("/static/prebid/prebid.min.js"));
        assert!(integration.matches_script_pattern("/static/prebid/v8.53.0/prebid.min.js"));
        assert!(integration.matches_script_pattern("/static/prebid/prebidjs.js"));

        // Should not match paths outside prefix
        assert!(!integration.matches_script_pattern("/prebid.min.js"));
        assert!(!integration.matches_script_pattern("/other/prebid.min.js"));

        // Should not match non-prebid scripts even under prefix
        assert!(!integration.matches_script_pattern("/static/prebid/app.js"));
    }

    #[test]
    fn script_pattern_matching_wildcard_matchit_syntax() {
        // Test {*rest} matchit-style wildcard pattern matching
        let mut config = base_config();
        config.script_patterns = vec!["/wp-content/plugins/prebidjs/{*rest}".to_string()];
        let integration = PrebidIntegration::new(config);

        // Should match paths under the prefix with known suffixes
        assert!(
            integration.matches_script_pattern("/wp-content/plugins/prebidjs/js/prebidjs.min.js")
        );
        assert!(integration.matches_script_pattern("/wp-content/plugins/prebidjs/prebid.min.js"));
        assert!(integration.matches_script_pattern("/wp-content/plugins/prebidjs/v1/v2/prebid.js"));

        // Should not match paths outside prefix
        assert!(!integration.matches_script_pattern("/prebid.min.js"));
        assert!(!integration.matches_script_pattern("/wp-content/other/prebid.min.js"));

        // Should not match non-prebid scripts even under prefix
        assert!(!integration.matches_script_pattern("/wp-content/plugins/prebidjs/app.js"));
    }

    #[test]
    fn script_pattern_matching_case_insensitive() {
        let integration = PrebidIntegration::new(base_config());

        assert!(integration.matches_script_pattern("/Prebid.JS"));
        assert!(integration.matches_script_pattern("/PREBID.MIN.JS"));
        assert!(integration.matches_script_pattern("/Static/Prebid.min.js"));
    }

    #[test]
    fn routes_include_script_patterns() {
        let integration = PrebidIntegration::new(base_config());
        let routes = integration.routes();

        // Should include the default ad routes
        assert!(routes.iter().any(|r| r.path == "/ad/render"));
        assert!(routes.iter().any(|r| r.path == "/ad/auction"));

        // Should include default script removal patterns
        assert!(routes.iter().any(|r| r.path == "/prebid.js"));
        assert!(routes.iter().any(|r| r.path == "/prebid.min.js"));
        assert!(routes.iter().any(|r| r.path == "/prebidjs.js"));
        assert!(routes.iter().any(|r| r.path == "/prebidjs.min.js"));
    }

    #[test]
    fn enhance_openrtb_request_adds_ids_and_regs() {
        let settings = make_settings();
        let mut request = OpenRtbRequest {
            id: "openrtb-request-id".to_string(),
            imp: Vec::new(),
            site: None,
            user: None,
            regs: None,
            device: None,
            ext: None,
            extra: HashMap::new(),
        };

        let synthetic_id = "synthetic-123";
        let fresh_id = "fresh-456";
        let mut req = Request::new(Method::POST, "https://edge.example/auction");
        req.set_header("Sec-GPC", "1");

        enhance_openrtb_request(&mut request, synthetic_id, fresh_id, &settings, &req)
            .expect("should enhance request");

        let user = request.user.as_ref().expect("should have user");
        assert_eq!(user.id.as_deref(), Some(synthetic_id), "should set user id");
        let user_ext = user.ext.as_ref().expect("should have user ext");
        assert_eq!(
            user_ext.synthetic_fresh.as_deref(),
            Some(fresh_id),
            "should set synthetic fresh id"
        );
        let regs = request.regs.as_ref().expect("should have regs");
        let regs_ext = regs.ext.as_ref().expect("should have regs ext");
        assert_eq!(
            regs_ext.us_privacy.as_deref(),
            Some("1YYN"),
            "should map GPC header to US privacy flag"
        );
        let site = request.site.as_ref().expect("should have site");
        assert_eq!(
            site.domain.as_deref(),
            Some(settings.publisher.domain.as_str()),
            "should set site domain"
        );
        let page = site.page.as_ref().expect("should have site page");
        assert!(page.starts_with("https://"), "should set site page");
    }

    #[test]
    fn override_prebid_bidders_replaces_request_values() {
        let mut request: OpenRtbRequest = serde_json::from_value(json!({
            "id": "openrtb-request-id",
            "imp": [
                {
                    "id": "slot-a",
                    "ext": {
                        "prebid": {
                            "bidder": { "legacy": {} }
                        }
                    }
                },
                { "id": "slot-b" }
            ]
        }))
        .expect("should parse openrtb request");
        let bidders = vec!["appnexus".to_string(), "rubicon".to_string()];

        override_prebid_bidders(&mut request, &bidders);

        let expected = bidders
            .iter()
            .map(|bidder| (bidder.clone(), JsonValue::Object(serde_json::Map::new())))
            .collect::<std::collections::HashMap<_, _>>();
        let first = request.imp.first().expect("should have first imp");
        let first_prebid = first
            .ext
            .as_ref()
            .and_then(|ext| ext.prebid.as_ref())
            .expect("should have prebid ext for first imp");
        assert_eq!(
            first_prebid.bidder, expected,
            "should replace bidders in first imp"
        );
        let second = request.imp.get(1).expect("should have second imp");
        let second_prebid = second
            .ext
            .as_ref()
            .and_then(|ext| ext.prebid.as_ref())
            .expect("should have prebid ext for second imp");
        assert_eq!(
            second_prebid.bidder, expected,
            "should replace bidders in second imp"
        );
    }

    #[test]
    fn transform_prebid_response_rewrites_creatives_and_tracking() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut response: OpenRtbResponse = serde_json::from_value(json!({
            "seatbid": [{
                "bid": [{
                    "adm": r#"<img src="https://cdn.adsrvr.org/pixel.png">"#,
                    "nurl": "https://notify.example/win",
                    "burl": "https://notify.example/bill"
                }]
            }]
        }))
        .expect("should parse openrtb response");

        transform_prebid_response(&mut response, &settings, "pub.example", "https")
            .expect("should rewrite response");

        let rewritten_adm = response
            .seatbid
            .as_ref()
            .and_then(|seatbids| seatbids.first())
            .and_then(|seatbid| seatbid.bid.as_ref())
            .and_then(|bids| bids.first())
            .and_then(|bid| bid.adm.as_deref())
            .expect("adm should be string");
        assert!(
            rewritten_adm.contains("/first-party/proxy?tsurl="),
            "creative markup should proxy asset urls"
        );

        let bid = response
            .seatbid
            .as_ref()
            .and_then(|seatbids| seatbids.first())
            .and_then(|seatbid| seatbid.bid.as_ref())
            .and_then(|bids| bids.first())
            .expect("should have bid");
        for value in [bid.nurl.as_deref(), bid.burl.as_deref()] {
            let value = value.expect("should have tracking url");
            assert!(
                value.starts_with("https://pub.example/first-party/proxy?tsurl="),
                "tracking URLs should be proxied"
            );
        }
    }

    #[test]
    fn transform_prebid_response_preserves_non_html_adm() {
        let settings = crate::test_support::tests::create_test_settings();
        let adm = r#"<VAST version="4.0"></VAST>"#;
        let mut response: OpenRtbResponse = serde_json::from_value(json!({
            "seatbid": [{
                "bid": [{
                    "adm": adm,
                    "nurl": "https://notify.example/win"
                }]
            }]
        }))
        .expect("should parse openrtb response");

        transform_prebid_response(&mut response, &settings, "pub.example", "https")
            .expect("should rewrite response");

        let rewritten_adm = response
            .seatbid
            .as_ref()
            .and_then(|seatbids| seatbids.first())
            .and_then(|seatbid| seatbid.bid.as_ref())
            .and_then(|bids| bids.first())
            .and_then(|bid| bid.adm.as_deref())
            .expect("adm should be string");
        assert_eq!(rewritten_adm, adm, "non-html adm should remain unchanged");
    }

    #[test]
    fn extract_adm_for_slot_prefers_exact_match() {
        let response = json!({
            "seatbid": [{
                "bid": [
                    { "impid": "slot-b", "adm": "<!-- slot B -->" },
                    { "impid": "slot-a", "adm": "<!-- slot A -->" }
                ]
            }]
        });

        let adm = extract_adm_for_slot(&response, "slot-a").expect("adm should exist");
        assert_eq!(adm, "<!-- slot A -->");

        let fallback = extract_adm_for_slot(&response, "missing")
            .expect("should fall back to first available adm");
        assert!(
            fallback == "<!-- slot B -->" || fallback == "<!-- slot A -->",
            "fallback should return some creative"
        );
    }

    #[test]
    fn first_party_proxy_url_signs_target() {
        let settings = crate::test_support::tests::create_test_settings();
        let url = "https://cdn.example/path?x=1";
        let rewritten = first_party_proxy_url(&settings, "pub.example", "https", url);
        assert!(
            rewritten.starts_with("https://pub.example/first-party/proxy?tsurl="),
            "proxy prefix should be applied"
        );
        assert!(
            rewritten.contains("tstoken="),
            "proxy url should include a signature"
        );
    }

    #[test]
    fn first_party_proxy_url_handles_macros() {
        let settings = crate::test_support::tests::create_test_settings();
        let url = "https://notify.example/win?price=${AUCTION_PRICE}&id=123";
        let rewritten = first_party_proxy_url(&settings, "pub.example", "https", url);
        assert!(
            rewritten.starts_with("https://pub.example/first-party/proxy?tsurl="),
            "proxy prefix should be applied"
        );
        assert!(
            rewritten.contains("tstoken="),
            "proxy url should include a signature"
        );
        assert!(
            rewritten.contains("notify.example"),
            "proxy url should include target host"
        );
        assert!(
            rewritten.contains("AUCTION_PRICE"),
            "proxy url should preserve macro tokens"
        );
    }

    #[test]
    fn first_party_proxy_url_respects_exclude_domains() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.rewrite.exclude_domains = vec!["notify.example".to_string()];
        let url = "https://notify.example/win";
        let rewritten = first_party_proxy_url(&settings, "pub.example", "https", url);
        assert_eq!(rewritten, url, "excluded domains should not be proxied");
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
script_patterns = ["/static/prebid/*"]
"#;

        let settings = Settings::from_toml(toml_str).expect("should parse TOML");
        let config = settings
            .integration_config::<PrebidIntegrationConfig>("prebid")
            .expect("should get config")
            .expect("should be enabled");

        assert_eq!(config.script_patterns, vec!["/static/prebid/*"]);
    }

    #[test]
    fn test_script_patterns_default() {
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

        // Should have default patterns
        assert_eq!(config.script_patterns, default_script_patterns());
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
        assert!(
            body.contains("tsjs-unified"),
            "should contain comment about replacement"
        );
    }

    #[test]
    fn test_routes_with_default_patterns() {
        let config = base_config(); // Has default script_patterns
        let integration = PrebidIntegration::new(config);

        let routes = integration.routes();

        // Should have 2 ad routes + 4 default script patterns
        assert_eq!(routes.len(), 6);

        // Verify ad routes
        assert!(routes.iter().any(|r| r.path == "/ad/render"));
        assert!(routes.iter().any(|r| r.path == "/ad/auction"));

        // Verify script pattern routes
        assert!(routes.iter().any(|r| r.path == "/prebid.js"));
        assert!(routes.iter().any(|r| r.path == "/prebid.min.js"));
        assert!(routes.iter().any(|r| r.path == "/prebidjs.js"));
        assert!(routes.iter().any(|r| r.path == "/prebidjs.min.js"));
    }

    #[test]
    fn tsjs_config_script_tag_generates_render_mode() {
        let tag = tsjs_config_script_tag(Mode::Render);
        assert!(tag.starts_with("<script>"));
        assert!(tag.ends_with("</script>"));
        assert!(tag.contains(r#"mode:"render""#));
        assert!(tag.contains("tsjs.setConfig"));
        // Should have guard for tsjs existence
        assert!(tag.contains("window.tsjs&&"));
    }

    #[test]
    fn tsjs_config_script_tag_generates_auction_mode() {
        let tag = tsjs_config_script_tag(Mode::Auction);
        assert!(tag.starts_with("<script>"));
        assert!(tag.ends_with("</script>"));
        assert!(tag.contains(r#"mode:"auction""#));
        assert!(tag.contains("tsjs.setConfig"));
        // Should have guard for tsjs existence
        assert!(tag.contains("window.tsjs&&"));
    }

    #[test]
    fn head_injector_returns_empty_when_no_mode() {
        // When no mode is set, no head inserts needed
        // (s2sConfig is built into the Prebid bundle served via script interception)
        let integration = PrebidIntegration::new(base_config());
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &Default::default(),
        };
        let inserts = integration.head_inserts(&ctx);
        assert!(inserts.is_empty(), "no head inserts when mode not set");
    }

    #[test]
    fn head_injector_injects_mode_config_when_mode_set() {
        let mut config = base_config();
        config.mode = Some(Mode::Auction);
        let integration = PrebidIntegration::new(config);
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &Default::default(),
        };
        let inserts = integration.head_inserts(&ctx);
        assert_eq!(inserts.len(), 1, "should inject mode config");
        assert!(
            inserts[0].contains(r#"mode:"auction""#),
            "should contain mode config"
        );
    }

    #[test]
    fn script_handler_serves_empty_js_stub() {
        let integration = PrebidIntegration::new(base_config());

        let response = integration
            .handle_script_handler()
            .expect("should return response");

        assert_eq!(response.get_status(), StatusCode::OK);
        let body = response.into_body_str();
        // Should be a small stub comment, not the full bundle
        assert!(
            body.len() < 100,
            "should serve empty JS stub, not full bundle"
        );
        assert!(body.starts_with("/*"), "should be a JS comment");
    }
}
