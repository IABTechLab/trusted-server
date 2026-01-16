use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use error_stack::{Report, ResultExt};
use fastly::http::{header, Method, StatusCode};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json, Value as JsonValue};
use url::Url;
use validator::Validate;

use crate::backend::ensure_backend_from_url;
use crate::constants::{HEADER_SYNTHETIC_FRESH, HEADER_SYNTHETIC_TRUSTED_SERVER};
use crate::creative;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::http_util::RequestInfo;
use crate::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration,
};
use crate::openrtb::{Banner, Format, Imp, ImpExt, OpenRtbRequest, PrebidImpExt, Site};
use crate::request_signing::RequestSigner;
use crate::settings::{IntegrationConfig, Settings};
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};

const PREBID_INTEGRATION_ID: &str = "prebid";
const ROUTE_RENDER: &str = "/ad/render";
const ROUTE_AUCTION: &str = "/ad/auction";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Render,
    Auction,
}

pub fn config_script_tag(mode: Mode) -> String {
    let mode_value = match mode {
        Mode::Render => "render",
        Mode::Auction => "auction",
    };
    format!(
        r#"<script>window.tsjs=window.tsjs||{{que:[]}};tsjs.que.push(function(){{tsjs.setConfig({{mode:"{}"}});}});</script>"#,
        mode_value
    )
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

    async fn handle_auction(
        &self,
        settings: &Settings,
        mut req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let body: AdRequest = serde_json::from_slice(&req.take_body_bytes()).change_context(
            TrustedServerError::Prebid {
                message: "Failed to parse tsjs auction request".to_string(),
            },
        )?;

        log::info!("/auction: received {} adUnits", body.ad_units.len());
        for unit in &body.ad_units {
            if let Some(mt) = &unit.media_types {
                if let Some(banner) = &mt.banner {
                    log::debug!("unit={} sizes={:?}", unit.code, banner.sizes);
                }
            }
        }

        let openrtb = build_openrtb_from_ts(&body, settings, &self.config);
        if let Ok(preview) = serde_json::to_string(&openrtb) {
            log::debug!(
                "OpenRTB payload (truncated): {}",
                &preview.chars().take(512).collect::<String>()
            );
        }

        req.set_body_json(&openrtb)
            .change_context(TrustedServerError::Prebid {
                message: "Failed to set OpenRTB body".to_string(),
            })?;

        handle_prebid_auction(settings, req, &self.config).await
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

        match method {
            Method::GET if path == ROUTE_RENDER => self.handle_render(settings, req).await,
            Method::POST if path == ROUTE_AUCTION => self.handle_auction(settings, req).await,
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

impl IntegrationHeadInjector for PrebidIntegration {
    fn integration_id(&self) -> &'static str {
        PREBID_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        self.config
            .mode
            .map(|mode| vec![config_script_tag(mode)])
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

// Request host/scheme extraction is now centralized in http_util::RequestInfo

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
        assert!(body.contains("// Script overridden by Trusted Server"));
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
    fn config_script_tag_generates_render_mode() {
        let tag = config_script_tag(Mode::Render);
        assert!(tag.starts_with("<script>"));
        assert!(tag.ends_with("</script>"));
        assert!(tag.contains(r#"mode:"render""#));
        assert!(tag.contains("tsjs.setConfig"));
        assert!(tag.contains("tsjs.que.push"));
    }

    #[test]
    fn config_script_tag_generates_auction_mode() {
        let tag = config_script_tag(Mode::Auction);
        assert!(tag.starts_with("<script>"));
        assert!(tag.ends_with("</script>"));
        assert!(tag.contains(r#"mode:"auction""#));
        assert!(tag.contains("tsjs.setConfig"));
    }

    #[test]
    fn head_injector_returns_empty_when_mode_not_set() {
        let integration = PrebidIntegration::new(base_config());
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &Default::default(),
        };
        let inserts = integration.head_inserts(&ctx);
        assert!(
            inserts.is_empty(),
            "should not inject config when mode is None"
        );
    }

    #[test]
    fn head_injector_returns_config_script_when_mode_set() {
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
        assert_eq!(inserts.len(), 1);
        assert!(inserts[0].contains(r#"mode:"auction""#));
    }
}
