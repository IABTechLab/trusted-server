//! Amazon Publisher Services (APS/TAM) `OpenRTB` integration.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::HeaderName;
use http::{HeaderMap, Method, StatusCode, header};
use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use url::Url;
use validator::{Validate, ValidationError};

use crate::auction::provider::{AuctionProvider, ProviderRequestOutcome};
use crate::auction::types::{
    AdSlot, ApsRendererV1, ApsRendererValidationResult, ApsTagType, AuctionContext, AuctionRequest,
    AuctionResponse, Bid, BidRenderSourceV1, MediaType, RENDER_DIMENSION_MAX,
    classify_aps_renderer_v1,
};
use crate::error::TrustedServerError;
use crate::integrations::{
    IntegrationEndpoint, IntegrationProxy, IntegrationRegistration,
    UPSTREAM_RTB_MAX_RESPONSE_BYTES, collect_response_bounded,
    ensure_integration_backend_with_timeout, predict_integration_backend_name,
};
use crate::openrtb::{
    Banner, Device, Format, Geo, Imp, OpenRtbRequest, Publisher, Regs, RegsExt, Site, ToExt, User,
    UserExt, to_openrtb_i32,
};
use crate::platform::{PlatformHttpRequest, PlatformResponse, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

const APS_INTEGRATION_ID: &str = "aps";
const APS_RENDERER_ROUTE: &str = "/integrations/aps/renderer";
const DEFAULT_CURRENCY: &str = "USD";
const APS_SDK_SOURCE: &str = "prebid";
const APS_SDK_VERSION: &str = "2.2.0";
const MAX_ACCOUNT_ID_BYTES: usize = 1024;
const MAX_CREATIVE_ID_BYTES: usize = 1024;
const MAX_DEBUG_RESPONSE_PREVIEW_BYTES: usize = 512;
const MAX_CREATIVE_URL_BYTES: usize = 4096;
const MAX_LANGUAGE_BYTES: usize = 8;
const MAX_PAGE_URL_BYTES: usize = 8192;
const MAX_RENDER_ENVELOPE_BYTES: usize = 256 * 1024;
const APS_RENDERER_CSP: &str = "default-src 'none'; sandbox allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation; script-src 'unsafe-inline' https:; connect-src https:; frame-src https:; img-src https: data:; media-src https: blob:; style-src 'unsafe-inline' https:; font-src https: data:;";

const APS_RENDERER_DOCUMENT: &str = concat!(
    r#"<!doctype html>
<meta charset="utf-8">
<script>
(function(){
'use strict';
"#,
    include_str!("generated/aps_renderer_validator_v1.js"),
    r#"
var match=/^#tsaps=([A-Za-z0-9_-]{22,128})$/.exec(location.hash);
var expected=match&&match[1];
try{history.replaceState(null,'',location.pathname+location.search);}catch(_error){}
if(!expected)return;
function receive(event){
 if(event.source!==parent)return;
 var message=event.data;
 if(!apsExactRecord(message,['nonce','renderer'])||message.nonce!==expected||
    classifyApsRendererV1(message.renderer,location.origin)!=='accepted')return;
 removeEventListener('message',receive);
 var acceptedNonce=expected;
 expected='';
 var renderer=message.renderer;
 window._aps=window._aps instanceof Map?window._aps:new Map();
 var account=window._aps.get(renderer.accountId);
 if(!account){
  account={queue:[],store:new Map([['listeners',new Map()]])};
  window._aps.set(renderer.accountId,account);
 }
 account.queue.push(new CustomEvent('prebid/creative/render',{detail:{aaxResponse:renderer.aaxResponse,seatBidId:renderer.bidId}}));
 var script=document.createElement('script');
 script.src='https://client.aps.amazon-adsystem.com/prebid-creative.js';
 script.onload=function(){parent.postMessage({message:'trusted-server/aps/renderer-ready',nonce:acceptedNonce},'*');};
 script.onerror=function(){parent.postMessage({message:'trusted-server/aps/renderer-failed',nonce:acceptedNonce},'*');};
 document.head.appendChild(script);
}
addEventListener('message',receive);
})();
</script>
"#
);

/// Configuration for the APS `OpenRTB` integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[validate(schema(function = "validate_inventory_identity_override"))]
pub struct ApsConfig {
    /// Whether APS integration is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// APS account ID. `pub_id` remains a deserialization alias only.
    #[serde(alias = "pub_id", deserialize_with = "deserialize_account_id")]
    pub account_id: String,
    /// APS `OpenRTB` endpoint.
    #[serde(default = "default_endpoint")]
    #[validate(custom(function = "validate_aps_endpoint"))]
    pub endpoint: String,
    /// Timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    /// Whether to include the APS HTTP exchange in auction response metadata.
    ///
    /// This default-off metadata is unredacted and client-visible. It can contain
    /// identity, consent, page, account, bid, and creative data. Enable it only
    /// on controlled test sites and never in production.
    #[serde(default)]
    pub debug: bool,
    /// Whether APS script creatives are eligible before winner selection.
    #[serde(default)]
    pub allow_script_creatives: bool,
    /// APS-authorized inventory domain used instead of the deployment hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(custom(function = "validate_inventory_domain"))]
    pub inventory_domain: Option<String>,
    /// Canonical HTTPS origin used for APS `site.page` while preserving its sanitized path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(custom(function = "validate_inventory_page_origin"))]
    pub inventory_page_origin: Option<String>,
}

fn deserialize_account_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct AccountIdVisitor;

    impl Visitor<'_> for AccountIdVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("a non-empty string or integer for account_id")
        }

        fn visit_str<E>(self, value: &str) -> Result<String, E>
        where
            E: de::Error,
        {
            let value = value.trim();
            if value.is_empty() {
                return Err(E::custom("account_id must not be empty"));
            }
            if value.len() > MAX_ACCOUNT_ID_BYTES {
                return Err(E::custom("account_id is too large"));
            }
            Ok(value.to_string())
        }

        fn visit_string<E>(self, value: String) -> Result<String, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_u64<E>(self, value: u64) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }
    }

    deserializer.deserialize_any(AccountIdVisitor)
}

fn validate_aps_endpoint(value: &str) -> Result<(), ValidationError> {
    let parsed = Url::parse(value).map_err(|_| ValidationError::new("invalid_aps_endpoint"))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(ValidationError::new("invalid_aps_endpoint"));
    }
    if parsed.path().trim_end_matches('/').ends_with("/e/dtb/bid") {
        let mut error = ValidationError::new("legacy_aps_endpoint");
        error.message =
            Some("legacy APS endpoint /e/dtb/bid is unsupported; migrate to /e/pb/bid".into());
        return Err(error);
    }
    Ok(())
}

fn validate_inventory_domain(value: &str) -> Result<(), ValidationError> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > 253
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains(['/', ':'])
    {
        return Err(ValidationError::new("invalid_aps_inventory_domain"));
    }
    for label in value.split('.') {
        let bytes = label.as_bytes();
        if label.is_empty()
            || label.len() > 63
            || bytes.first() == Some(&b'-')
            || bytes.last() == Some(&b'-')
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            return Err(ValidationError::new("invalid_aps_inventory_domain"));
        }
    }
    Ok(())
}

fn validate_inventory_page_origin(value: &str) -> Result<(), ValidationError> {
    let parsed =
        Url::parse(value).map_err(|_| ValidationError::new("invalid_aps_inventory_page_origin"))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ValidationError::new("invalid_aps_inventory_page_origin"));
    }
    Ok(())
}

fn validate_inventory_identity_override(config: &ApsConfig) -> Result<(), ValidationError> {
    let (Some(domain), Some(origin)) = (
        config.inventory_domain.as_deref(),
        config.inventory_page_origin.as_deref(),
    ) else {
        if config.inventory_domain.is_none() && config.inventory_page_origin.is_none() {
            return Ok(());
        }
        return Err(ValidationError::new(
            "incomplete_aps_inventory_identity_override",
        ));
    };
    let parsed = Url::parse(origin)
        .map_err(|_| ValidationError::new("invalid_aps_inventory_page_origin"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| ValidationError::new("invalid_aps_inventory_page_origin"))?
        .to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();
    if host != domain
        && !host
            .strip_suffix(&domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
    {
        return Err(ValidationError::new("aps_inventory_origin_domain_mismatch"));
    }
    Ok(())
}

fn default_enabled() -> bool {
    false
}

fn default_endpoint() -> String {
    "https://web.ads.aps.amazon-adsystem.com/e/pb/bid".to_string()
}

fn default_timeout_ms() -> u32 {
    800
}

impl Default for ApsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            account_id: String::new(),
            endpoint: default_endpoint(),
            timeout_ms: default_timeout_ms(),
            debug: false,
            allow_script_creatives: false,
            inventory_domain: None,
            inventory_page_origin: None,
        }
    }
}

impl IntegrationConfig for ApsConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[derive(Debug, Serialize)]
struct ApsRequestExt<'a> {
    account: &'a str,
    sdk: ApsSdkExt,
}

impl ToExt for ApsRequestExt<'_> {}

#[derive(Debug, Serialize)]
struct ApsSdkExt {
    source: &'static str,
    version: &'static str,
}

struct ApsRendererInput<'a> {
    bid_id: &'a str,
    creative_id: Option<String>,
    tag_type: ApsTagType,
    creative_url: &'a str,
    price: f64,
    width: u32,
    height: u32,
}

#[derive(Clone)]
struct ApsDebugRequest {
    body: String,
    headers: BTreeMap<String, Vec<String>>,
}

/// APS `OpenRTB` auction provider.
pub struct ApsAuctionProvider {
    config: ApsConfig,
}

impl ApsAuctionProvider {
    /// Create an APS provider from validated configuration.
    #[must_use]
    pub fn new(config: ApsConfig) -> Self {
        Self { config }
    }

    fn build_regs(consent: Option<&crate::consent::ConsentContext>) -> Option<Regs> {
        let consent = consent?;
        let ext = RegsExt {
            gdpr: Some(u8::from(consent.gdpr_applies)),
            us_privacy: consent.raw_us_privacy.clone(),
            gpp: consent.raw_gpp_string.clone(),
            gpp_sid: consent.gpp_section_ids.clone(),
        };
        Some(Regs {
            coppa: None,
            gdpr: Some(consent.gdpr_applies),
            us_privacy: ext.us_privacy.clone(),
            gpp: ext.gpp.clone(),
            gpp_sid: ext
                .gpp_sid
                .as_ref()
                .map(|ids| ids.iter().map(|id| i32::from(*id)).collect())
                .unwrap_or_default(),
            ext: ext.to_ext(),
        })
    }

    fn request_language(context: &AuctionContext<'_>) -> Option<String> {
        context
            .request
            .headers()
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .and_then(|value| value.split(';').next())
            .and_then(|value| value.split('-').next())
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= MAX_LANGUAGE_BYTES)
            .map(str::to_string)
    }

    fn request_dnt(context: &AuctionContext<'_>) -> Option<bool> {
        context
            .request
            .headers()
            .get("DNT")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.trim() == "1")
            .then_some(true)
    }

    fn valid_http_url(value: &str) -> Option<String> {
        if value.len() > MAX_PAGE_URL_BYTES {
            return None;
        }
        let parsed = Url::parse(value).ok()?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return None;
        }
        Some(parsed.to_string())
    }

    fn inventory_site_identity(
        &self,
        fallback_domain: &str,
        fallback_page: String,
    ) -> (String, String) {
        let (Some(domain), Some(origin)) = (
            self.config.inventory_domain.as_ref(),
            self.config.inventory_page_origin.as_deref(),
        ) else {
            return (fallback_domain.to_string(), fallback_page);
        };
        let (Ok(mut canonical_page), Ok(current_page)) =
            (Url::parse(origin), Url::parse(&fallback_page))
        else {
            return (fallback_domain.to_string(), fallback_page);
        };
        canonical_page.set_path(current_page.path());
        canonical_page.set_query(current_page.query());
        canonical_page.set_fragment(None);
        (domain.clone(), canonical_page.to_string())
    }

    fn build_openrtb_request(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> OpenRtbRequest {
        let imp = request
            .slots
            .iter()
            .filter_map(|slot| {
                let slot_context = format!("slot '{}'", slot.id);
                let formats: Vec<Format> = slot
                    .formats
                    .iter()
                    .filter(|format| format.media_type == MediaType::Banner)
                    .filter_map(|format| {
                        Some(Format {
                            w: to_openrtb_i32(format.width, "format.w", &slot_context),
                            h: to_openrtb_i32(format.height, "format.h", &slot_context),
                            ..Default::default()
                        })
                        .filter(|format| format.w.is_some() && format.h.is_some())
                    })
                    .collect();
                let first = formats.first()?;
                Some(Imp {
                    id: Some(slot.id.clone()),
                    banner: Some(Banner {
                        format: formats.clone(),
                        w: first.w,
                        h: first.h,
                        topframe: Some(false),
                        ..Default::default()
                    }),
                    bidfloor: slot.floor_price,
                    bidfloorcur: slot.floor_price.map(|_| DEFAULT_CURRENCY.to_string()),
                    secure: Some(true),
                    ..Default::default()
                })
            })
            .collect();

        let consent = request.user.consent.as_ref();
        let raw_tc = consent.and_then(|value| value.raw_tc_string.clone());
        let user = Some(User {
            id: request.user.id.clone(),
            consent: raw_tc.clone(),
            ext: UserExt {
                consent: raw_tc,
                consented_providers_settings: None,
                eids: request.user.eids.clone(),
            }
            .to_ext(),
            ..Default::default()
        });

        let language = Self::request_language(context);
        let dnt = Self::request_dnt(context);
        let device = request
            .device
            .as_ref()
            .map(|device| Device {
                ua: device.user_agent.clone(),
                ip: device.ip.clone(),
                geo: device.geo.as_ref().map(|geo| Geo {
                    country: Some(geo.country.clone()),
                    region: geo.region.clone(),
                    city: Some(geo.city.clone()),
                    metro: (geo.metro_code > 0).then(|| geo.metro_code.to_string()),
                    r#type: Some(2),
                    ..Default::default()
                }),
                dnt,
                language: language.clone(),
                ..Default::default()
            })
            .or_else(|| {
                (dnt.is_some() || language.is_some()).then_some(Device {
                    dnt,
                    language,
                    ..Default::default()
                })
            });

        let page = request
            .publisher
            .page_url
            .as_deref()
            .and_then(Self::valid_http_url)
            .unwrap_or_else(|| format!("https://{}", request.publisher.domain));
        // For the same-origin `/auction` request, the browser Referer is the
        // current publisher page already carried in sanitized form as
        // `site.page`; forwarding the raw header as `site.ref` would reintroduce
        // query-string identifiers and can leak the deployment host.
        let (site_domain, page) = self.inventory_site_identity(&request.publisher.domain, page);

        OpenRtbRequest {
            id: Some(request.id.clone()),
            imp,
            site: Some(Site {
                domain: Some(site_domain.clone()),
                page: Some(page),
                r#ref: None,
                publisher: Some(Publisher {
                    domain: Some(site_domain),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            user,
            device,
            regs: Self::build_regs(consent),
            tmax: to_openrtb_i32(context.timeout_ms, "tmax", "APS request"),
            cur: vec![DEFAULT_CURRENCY.to_string()],
            ext: ApsRequestExt {
                account: &self.config.account_id,
                sdk: ApsSdkExt {
                    source: APS_SDK_SOURCE,
                    version: APS_SDK_VERSION,
                },
            }
            .to_ext(),
            ..Default::default()
        }
    }

    fn serialize_openrtb_request(
        request: &OpenRtbRequest,
    ) -> Result<Vec<u8>, Report<TrustedServerError>> {
        serde_json::to_vec(request).change_context(TrustedServerError::Auction {
            message: "Failed to serialize APS OpenRTB request".to_string(),
        })
    }

    fn debug_headers(headers: &HeaderMap) -> BTreeMap<String, Vec<String>> {
        // This metadata is returned to page JavaScript. Keep the list fail-closed so
        // identity or authentication headers from an upstream response never leak.
        const ALLOWED_HEADERS: &[HeaderName] = &[header::CONTENT_TYPE];

        let mut values = BTreeMap::<String, Vec<String>>::new();
        for (name, value) in headers {
            if !ALLOWED_HEADERS.contains(name) {
                continue;
            }
            let Ok(value) = value.to_str() else {
                continue;
            };
            values
                .entry(name.as_str().to_string())
                .or_default()
                .push(value.to_string());
        }
        values
    }

    fn debug_body_preview(body: &[u8]) -> String {
        let preview_len = body.len().min(MAX_DEBUG_RESPONSE_PREVIEW_BYTES);
        let mut preview = String::from_utf8_lossy(&body[..preview_len]).into_owned();
        if body.len() > preview_len {
            preview.push_str(&format!("…(truncated {} bytes)", body.len() - preview_len));
        }
        preview
    }

    fn attach_debug_metadata(
        &self,
        response: AuctionResponse,
        debug_enabled: bool,
        request: Option<ApsDebugRequest>,
        response_body: Option<&[u8]>,
        response_headers: &BTreeMap<String, Vec<String>>,
        status: StatusCode,
    ) -> AuctionResponse {
        if !debug_enabled {
            return response;
        }

        let mut http_call = json!({
            "responseheaders": response_headers,
            "status": status.as_u16(),
            "uri": self.config.endpoint.clone(),
        });
        if let Some(http_call) = http_call.as_object_mut() {
            if let Some(request) = request {
                http_call.insert("requestbody".to_string(), json!(request.body));
                http_call.insert("requestheaders".to_string(), json!(request.headers));
            }
            if let Some(response_body) = response_body {
                http_call.insert(
                    "responsebody".to_string(),
                    json!(Self::debug_body_preview(response_body)),
                );
            }
        }
        response.with_metadata(
            "debug",
            json!({
                "httpcalls": {
                    (APS_INTEGRATION_ID): [http_call]
                }
            }),
        )
    }

    fn compatible_dimensions(slot: &AdSlot, width: u32, height: u32) -> bool {
        width > 0
            && height > 0
            && slot.formats.iter().any(|format| {
                format.media_type == MediaType::Banner
                    && format.width == width
                    && format.height == height
            })
    }

    fn valid_creative_url(&self, value: &str, publisher_origin: &str) -> bool {
        if value.len() > MAX_CREATIVE_URL_BYTES {
            return false;
        }
        let Ok(parsed) = Url::parse(value) else {
            return false;
        };
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.origin().ascii_serialization() != publisher_origin
    }

    fn build_renderer(
        &self,
        input: ApsRendererInput<'_>,
        publisher_origin: &str,
    ) -> Option<BidRenderSourceV1> {
        let tag_type_value = match input.tag_type {
            ApsTagType::Iframe => "iframe",
            ApsTagType::Script => "script",
        };
        let envelope = json!({
            "seatbid": [{
                "bid": [{
                    "id": input.bid_id,
                    "price": input.price,
                    "w": input.width,
                    "h": input.height,
                    "ext": {
                        "creativeurl": input.creative_url,
                        "tagtype": tag_type_value
                    }
                }]
            }]
        });
        let serialized = serde_json::to_vec(&envelope).ok()?;
        if serialized.len() > MAX_RENDER_ENVELOPE_BYTES {
            return None;
        }
        let renderer = BidRenderSourceV1::Aps(ApsRendererV1 {
            version: 1,
            account_id: self.config.account_id.clone(),
            bid_id: input.bid_id.to_string(),
            creative_id: input.creative_id,
            tag_type: input.tag_type,
            creative_url: input.creative_url.to_string(),
            aax_response: BASE64_STANDARD.encode(serialized),
            width: input.width,
            height: input.height,
        });
        let value = serde_json::to_value(&renderer).ok()?;
        (classify_aps_renderer_v1(&value, publisher_origin)
            == ApsRendererValidationResult::Accepted)
            .then_some(renderer)
    }

    fn increment_reason(reasons: &mut BTreeMap<String, u64>, reason: &'static str) {
        *reasons.entry(reason.to_string()).or_default() += 1;
    }

    fn parse_bid(
        &self,
        value: &Json,
        slots: &HashMap<&str, &AdSlot>,
        publisher_origin: &str,
    ) -> Result<Bid, &'static str> {
        let bid_id = value
            .get("id")
            .and_then(Json::as_str)
            .ok_or("missing_render_source")?;
        if bid_id.is_empty()
            || bid_id.len() > 64
            || bid_id.bytes().any(|byte| byte <= 0x1f || byte == 0x7f)
        {
            return Err("invalid_bid_id");
        }
        let slot_id = value
            .get("impid")
            .and_then(Json::as_str)
            .ok_or("unknown_impid")?;
        let slot = slots.get(slot_id).ok_or("unknown_impid")?;
        let price = value
            .get("price")
            .and_then(Json::as_f64)
            .filter(|price| price.is_finite() && *price >= 0.0)
            .ok_or("invalid_price")?;
        if value
            .get("mtype")
            .is_some_and(|mtype| mtype.as_i64() != Some(1))
        {
            return Err("unsupported_media_type");
        }
        let parse_dimension = |field: &str| {
            let number = value
                .get(field)
                .and_then(Json::as_f64)
                .filter(|number| number.is_finite() && number.fract() == 0.0 && *number > 0.0)
                .ok_or("invalid_dimensions")?;
            if number > RENDER_DIMENSION_MAX as f64 {
                return Err("dimensions_out_of_range");
            }
            u32::try_from(number as u64).map_err(|_| "dimensions_out_of_range")
        };
        let width = parse_dimension("w")?;
        let height = parse_dimension("h")?;
        if !Self::compatible_dimensions(slot, width, height) {
            return Err("invalid_dimensions");
        }
        let ext = value
            .get("ext")
            .and_then(Json::as_object)
            .ok_or("missing_render_source")?;
        let creative_url = ext
            .get("creativeurl")
            .and_then(Json::as_str)
            .ok_or("missing_render_source")?;
        if !self.valid_creative_url(creative_url, publisher_origin) {
            return Err("invalid_creative_url");
        }
        let tag_type = match ext.get("tagtype").and_then(Json::as_str) {
            Some("iframe") => ApsTagType::Iframe,
            Some("script") if self.config.allow_script_creatives => ApsTagType::Script,
            Some("script") => return Err("script_rendering_disabled"),
            _ => return Err("unsupported_tagtype"),
        };
        let creative_id = value
            .get("crid")
            .and_then(Json::as_str)
            .filter(|creative_id| !creative_id.is_empty())
            .map(str::to_string);
        if creative_id
            .as_ref()
            .is_some_and(|creative_id| creative_id.len() > MAX_CREATIVE_ID_BYTES)
        {
            return Err("creative_id_too_large");
        }
        let renderer = self
            .build_renderer(
                ApsRendererInput {
                    bid_id,
                    creative_id: creative_id.clone(),
                    tag_type,
                    creative_url,
                    price,
                    width,
                    height,
                },
                publisher_origin,
            )
            .ok_or("render_payload_too_large")?;
        let adomain = value
            .get("adomain")
            .and_then(Json::as_array)
            .map(|domains| {
                domains
                    .iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect()
            });

        Ok(Bid {
            slot_id: slot_id.to_string(),
            price: Some(price),
            currency: DEFAULT_CURRENCY.to_string(),
            creative: None,
            adomain,
            bidder: APS_INTEGRATION_ID.to_string(),
            width,
            height,
            nurl: None,
            burl: None,
            bid_id: Some(bid_id.to_string()),
            ad_id: value.get("adid").and_then(Json::as_str).map(str::to_string),
            creative_id,
            renderer: Some(renderer),
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        })
    }

    fn parse_aps_response(
        &self,
        value: &Json,
        response_time_ms: u64,
        request: &AuctionRequest,
    ) -> AuctionResponse {
        if !value.is_object()
            || value.get("contextual").is_some()
            || value
                .get("cur")
                .is_some_and(|currency| !currency.is_string())
            || value
                .get("seatbid")
                .is_some_and(|seatbids| !seatbids.is_array())
        {
            return AuctionResponse::error(APS_INTEGRATION_ID, response_time_ms)
                .with_metadata("drop_reasons", json!({"unexpected_response_shape": 1}));
        }
        if value
            .get("cur")
            .and_then(Json::as_str)
            .is_some_and(|currency| !currency.eq_ignore_ascii_case(DEFAULT_CURRENCY))
        {
            return AuctionResponse::no_bid(APS_INTEGRATION_ID, response_time_ms)
                .with_metadata("drop_reasons", json!({"unsupported_currency": 1}));
        }

        let slots: HashMap<&str, &AdSlot> = request
            .slots
            .iter()
            .map(|slot| (slot.id.as_str(), slot))
            .collect();
        let seatbids = value.get("seatbid").and_then(Json::as_array);
        let seatbid_count = seatbids.map_or(0, Vec::len);
        let mut reasons = BTreeMap::new();
        let mut selected: HashMap<String, Bid> = HashMap::new();
        let mut dropped = 0_u64;

        let publisher_origin = request
            .publisher
            .page_url
            .as_deref()
            .and_then(|page_url| Url::parse(page_url).ok())
            .map(|page_url| page_url.origin().ascii_serialization())
            .unwrap_or_else(|| format!("https://{}", request.publisher.domain));

        for seatbid in seatbids.into_iter().flatten() {
            let Some(bids) = seatbid.get("bid").and_then(Json::as_array) else {
                dropped += 1;
                Self::increment_reason(&mut reasons, "empty_seatbid_bids");
                continue;
            };
            for value in bids {
                match self.parse_bid(value, &slots, &publisher_origin) {
                    Ok(candidate) => {
                        let replace = selected.get(&candidate.slot_id).is_none_or(|current| {
                            let candidate_price = candidate.price.unwrap_or_default();
                            let current_price = current.price.unwrap_or_default();
                            candidate_price > current_price
                                || (candidate_price == current_price
                                    && candidate.bid_id.as_deref().unwrap_or_default()
                                        < current.bid_id.as_deref().unwrap_or_default())
                        });
                        if replace {
                            if selected
                                .insert(candidate.slot_id.clone(), candidate)
                                .is_some()
                            {
                                dropped += 1;
                                Self::increment_reason(&mut reasons, "lost_to_higher_bid");
                            }
                        } else {
                            dropped += 1;
                            Self::increment_reason(&mut reasons, "lost_to_higher_bid");
                        }
                    }
                    Err(reason) => {
                        dropped += 1;
                        Self::increment_reason(&mut reasons, reason);
                    }
                }
            }
        }

        if seatbid_count == 0 {
            Self::increment_reason(&mut reasons, "empty_seatbid");
        }
        let accepted = selected.len();
        let metadata = [
            ("seatbid_count".to_string(), json!(seatbid_count)),
            ("accepted_bid_count".to_string(), json!(accepted)),
            ("dropped_bid_count".to_string(), json!(dropped)),
            ("drop_reasons".to_string(), json!(reasons)),
        ];
        let mut response = if selected.is_empty() {
            AuctionResponse::no_bid(APS_INTEGRATION_ID, response_time_ms)
        } else {
            AuctionResponse::success(
                APS_INTEGRATION_ID,
                selected.into_values().collect(),
                response_time_ms,
            )
        };
        response.metadata.extend(metadata);
        response
    }

    async fn parse_response_inner(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        request: Option<&AuctionRequest>,
        debug_request: Option<ApsDebugRequest>,
        debug_enabled: bool,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let response = response.response;
        let status = response.status();
        let response_headers = if debug_enabled {
            Self::debug_headers(response.headers())
        } else {
            BTreeMap::new()
        };

        if status == StatusCode::NO_CONTENT {
            return Ok(self.attach_debug_metadata(
                AuctionResponse::no_bid(APS_INTEGRATION_ID, response_time_ms),
                debug_enabled,
                debug_request,
                Some(&[]),
                &response_headers,
                status,
            ));
        }
        if !status.is_success() {
            log::warn!("APS returns a non-success status");
            let body = if debug_enabled {
                match collect_response_bounded(
                    response.into_body(),
                    UPSTREAM_RTB_MAX_RESPONSE_BYTES,
                    APS_INTEGRATION_ID,
                )
                .await
                {
                    Ok(body) => Some(body),
                    Err(error) => {
                        log::warn!("Failed to read APS debug response body: {error:?}");
                        None
                    }
                }
            } else {
                None
            };
            return Ok(self.attach_debug_metadata(
                AuctionResponse::error(APS_INTEGRATION_ID, response_time_ms),
                debug_enabled,
                debug_request,
                body.as_deref(),
                &response_headers,
                status,
            ));
        }
        let body = collect_response_bounded(
            response.into_body(),
            UPSTREAM_RTB_MAX_RESPONSE_BYTES,
            APS_INTEGRATION_ID,
        )
        .await
        .change_context(TrustedServerError::Auction {
            message: "Failed to read APS response body".to_string(),
        })?;
        log::trace!("APS response body: {}", String::from_utf8_lossy(&body));
        let value: Json = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                log::warn!("Failed to parse APS response JSON: {error}");
                let parsed = AuctionResponse::error(APS_INTEGRATION_ID, response_time_ms)
                    .with_metadata("drop_reasons", json!({"unexpected_response_shape": 1}));
                return Ok(self.attach_debug_metadata(
                    parsed,
                    debug_enabled,
                    debug_request,
                    Some(&body),
                    &response_headers,
                    status,
                ));
            }
        };
        let Some(request) = request else {
            log::error!(
                "APS cannot parse a successful bid response without the original auction request context"
            );
            let response = AuctionResponse::error(APS_INTEGRATION_ID, response_time_ms)
                .with_metadata("drop_reasons", json!({"missing_request_context": 1}));
            return Ok(self.attach_debug_metadata(
                response,
                debug_enabled,
                debug_request,
                Some(&body),
                &response_headers,
                status,
            ));
        };
        let parsed = self.parse_aps_response(&value, response_time_ms, request);
        log::info!(
            "APS returns {} accepted bids in {}ms",
            parsed.bids.len(),
            response_time_ms
        );
        Ok(self.attach_debug_metadata(
            parsed,
            debug_enabled,
            debug_request,
            Some(&body),
            &response_headers,
            status,
        ))
    }
}

#[async_trait(?Send)]
impl AuctionProvider for ApsAuctionProvider {
    fn provider_name(&self) -> &'static str {
        APS_INTEGRATION_ID
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
        let openrtb = self.build_openrtb_request(request, context);
        if openrtb.imp.is_empty() {
            return Err(Report::new(TrustedServerError::Auction {
                message: "No valid APS impressions after filtering".to_string(),
            }));
        }
        log::info!("APS requests bids for {} impressions", openrtb.imp.len());
        log::trace!("APS request body: {openrtb:?}");
        let body = Self::serialize_openrtb_request(&openrtb)?;
        let outbound_request = http::Request::builder()
            .method(Method::POST)
            .uri(&self.config.endpoint)
            .header(header::CONTENT_TYPE, "application/json")
            .body(EdgeBody::from(body.clone()))
            .change_context(TrustedServerError::Auction {
                message: "Failed to build APS request".to_string(),
            })?;
        let debug_request = self.config.debug.then(|| ApsDebugRequest {
            body: String::from_utf8_lossy(&body).into_owned(),
            headers: Self::debug_headers(outbound_request.headers()),
        });
        let backend = ensure_integration_backend_with_timeout(
            context.services,
            &self.config.endpoint,
            APS_INTEGRATION_ID,
            Duration::from_millis(u64::from(context.timeout_ms)),
        )
        .change_context(TrustedServerError::Auction {
            message: "Failed to resolve APS backend".to_string(),
        })?;
        let pending = context
            .services
            .http_client()
            .send_async(PlatformHttpRequest::new(outbound_request, backend))
            .await
            .change_context(TrustedServerError::Auction {
                message: "Failed to send APS request".to_string(),
            })?;
        Ok(match debug_request {
            Some(debug_request) => {
                ProviderRequestOutcome::pending_with_state(pending, Box::new(debug_request))
            }
            None => ProviderRequestOutcome::pending(pending),
        })
    }

    async fn parse_response(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        self.parse_response_inner(response, response_time_ms, None, None, self.config.debug)
            .await
    }

    async fn parse_response_with_context(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let _ = context;
        self.parse_response_inner(
            response,
            response_time_ms,
            Some(request),
            None,
            self.config.debug,
        )
        .await
    }

    async fn parse_response_with_context_and_state(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
        parse_state: Option<&(dyn core::any::Any + Send + Sync)>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let _ = context;
        let debug_request = if self.config.debug {
            let debug_request =
                parse_state.and_then(|state| state.downcast_ref::<ApsDebugRequest>());
            if parse_state.is_some() && debug_request.is_none() {
                log::warn!("APS response received unexpected provider parse state");
            }
            debug_request.cloned()
        } else {
            None
        };
        self.parse_response_inner(
            response,
            response_time_ms,
            Some(request),
            debug_request,
            self.config.debug,
        )
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
            &self.config.endpoint,
            APS_INTEGRATION_ID,
            Duration::from_millis(u64::from(timeout_ms)),
        )
        .inspect_err(|error| log::error!("Failed to predict APS backend name: {error:?}"))
        .ok()
    }
}

#[derive(Debug)]
struct ApsRendererIntegration;

#[async_trait(?Send)]
impl IntegrationProxy for ApsRendererIntegration {
    fn integration_name(&self) -> &'static str {
        APS_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![IntegrationEndpoint::get(APS_RENDERER_ROUTE)]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        _services: &RuntimeServices,
        request: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        if request.method() != Method::GET || request.uri().path() != APS_RENDERER_ROUTE {
            return http::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(EdgeBody::from("Not Found"))
                .change_context(TrustedServerError::Integration {
                    integration: APS_INTEGRATION_ID.to_string(),
                    message: "Failed to build APS not-found response".to_string(),
                });
        }
        http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header("x-content-type-options", "nosniff")
            .header("referrer-policy", "no-referrer")
            .header(header::CONTENT_SECURITY_POLICY, APS_RENDERER_CSP)
            .body(EdgeBody::from(APS_RENDERER_DOCUMENT))
            .change_context(TrustedServerError::Integration {
                integration: APS_INTEGRATION_ID.to_string(),
                message: "Failed to build APS renderer response".to_string(),
            })
    }
}

/// Register the APS static renderer endpoint when APS is enabled.
///
/// # Errors
///
/// Returns an error when enabled APS configuration is invalid.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(_config) = settings.integration_config::<ApsConfig>(APS_INTEGRATION_ID)? else {
        return Ok(None);
    };
    let integration = Arc::new(ApsRendererIntegration);
    Ok(Some(
        IntegrationRegistration::builder(APS_INTEGRATION_ID)
            .with_proxy(integration)
            .without_js()
            .build(),
    ))
}

/// Register the APS auction provider when enabled.
///
/// # Errors
///
/// Returns an error when enabled APS configuration is invalid.
pub fn register_providers(
    settings: &Settings,
) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<ApsConfig>(APS_INTEGRATION_ID)? else {
        return Ok(Vec::new());
    };
    log::info!("Registering APS OpenRTB provider");
    if config.debug {
        log::warn!(
            "APS debug mode is ON — raw request and response data, including creative markup, will be included in client-visible /auction responses"
        );
    }
    Ok(vec![Arc::new(ApsAuctionProvider::new(config))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::{
        AdFormat, AdSlot, AuctionContext, AuctionRequest, BidStatus, DeviceInfo, PublisherInfo,
        UserInfo,
    };
    use crate::consent::ConsentContext;
    use crate::openrtb::{Eid, Uid};
    use crate::platform::GeoInfo;
    use crate::platform::test_support::{
        StubHttpClient, build_services_with_http_client, noop_services,
    };
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;

    fn config() -> ApsConfig {
        ApsConfig {
            enabled: true,
            account_id: "example-account-id".to_string(),
            endpoint: default_endpoint(),
            timeout_ms: 800,
            debug: false,
            allow_script_creatives: false,
            inventory_domain: None,
            inventory_page_origin: None,
        }
    }

    fn request() -> AuctionRequest {
        AuctionRequest {
            id: "fictional-auction".to_string(),
            slots: vec![AdSlot {
                id: "fictional-slot".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                }],
                floor_price: Some(1.0),
                targeting: HashMap::new(),
                bidders: HashMap::new(),
            }],
            publisher: PublisherInfo {
                domain: "publisher.example".to_string(),
                page_url: Some("https://publisher.example/article".to_string()),
            },
            user: UserInfo {
                id: Some("fictional-user".to_string()),
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        }
    }

    fn bid(id: &str, price: f64, tagtype: &str) -> Json {
        json!({
            "id": id,
            "impid": "fictional-slot",
            "price": price,
            "w": 300,
            "h": 250,
            "crid": "fictional-creative",
            "adomain": ["advertiser.example"],
            "ext": {
                "creativeurl": "https://creative.example/render",
                "tagtype": tagtype,
                "unknown": "discarded"
            },
            "adm": "<script>discarded</script>",
            "nurl": "https://notice.example/win"
        })
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RendererCorpus {
        publisher_origin: String,
        base_descriptor: Json,
        vectors: Vec<RendererCorpusVector>,
    }

    #[derive(serde::Deserialize)]
    struct RendererCorpusVector {
        id: String,
        expected: String,
        operation: Json,
    }

    fn corpus_value<'a>(operation: &'a Json, field: &str) -> &'a Json {
        operation
            .get(field)
            .unwrap_or_else(|| panic!("should include corpus operation field {field}"))
    }

    fn corpus_string<'a>(operation: &'a Json, field: &str) -> &'a str {
        corpus_value(operation, field)
            .as_str()
            .unwrap_or_else(|| panic!("corpus operation field {field} should be a string"))
    }

    fn corpus_usize(operation: &Json, field: &str) -> usize {
        corpus_value(operation, field)
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or_else(|| panic!("corpus operation field {field} should be a usize"))
    }

    fn set_json_path(root: &mut Json, path: &[Json], value: Json) {
        let (segment, tail) = path
            .split_first()
            .expect("corpus JSON path should not be empty");
        if tail.is_empty() {
            if let Some(field) = segment.as_str() {
                root.as_object_mut()
                    .expect("corpus string path should address an object")
                    .insert(field.to_string(), value);
            } else {
                let index = segment
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .expect("corpus numeric path should be a usize");
                let slot = root
                    .as_array_mut()
                    .and_then(|array| array.get_mut(index))
                    .expect("corpus numeric path should address an array element");
                *slot = value;
            }
            return;
        }

        let child = if let Some(field) = segment.as_str() {
            root.as_object_mut()
                .and_then(|object| object.get_mut(field))
                .expect("corpus string path should address an object field")
        } else {
            let index = segment
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .expect("corpus numeric path should be a usize");
            root.as_array_mut()
                .and_then(|array| array.get_mut(index))
                .expect("corpus numeric path should address an array element")
        };
        set_json_path(child, tail, value);
    }

    fn delete_json_path(root: &mut Json, path: &[Json]) {
        let (segment, tail) = path
            .split_first()
            .expect("corpus JSON path should not be empty");
        if tail.is_empty() {
            let field = segment
                .as_str()
                .expect("corpus delete path should end in an object field");
            root.as_object_mut()
                .expect("corpus delete path should address an object")
                .remove(field);
            return;
        }

        let child = if let Some(field) = segment.as_str() {
            root.as_object_mut()
                .and_then(|object| object.get_mut(field))
                .expect("corpus string path should address an object field")
        } else {
            let index = segment
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .expect("corpus numeric path should be a usize");
            root.as_array_mut()
                .and_then(|array| array.get_mut(index))
                .expect("corpus numeric path should address an array element")
        };
        delete_json_path(child, tail);
    }

    fn descriptor_field(descriptor: &mut Json, field: &str, value: Json) {
        descriptor
            .as_object_mut()
            .expect("corpus descriptor should be an object")
            .insert(field.to_string(), value);
    }

    fn materialize_renderer_corpus_vector(
        corpus: &RendererCorpus,
        vector: &RendererCorpusVector,
    ) -> Json {
        let mut descriptor = corpus.base_descriptor.clone();
        let mut envelope: Json = serde_json::from_str(include_str!(
            "../../../trusted-server-js/lib/test/fixtures/aps-renderer-v1.json"
        ))
        .expect("should parse shared APS renderer fixture");
        let operation = &vector.operation;
        let kind = corpus_string(operation, "kind");
        let mut encoded_envelope = None;

        match kind {
            "none" => {}
            "descriptor-delete" => {
                descriptor
                    .as_object_mut()
                    .expect("corpus descriptor should be an object")
                    .remove(corpus_string(operation, "field"));
            }
            "descriptor-set" => descriptor_field(
                &mut descriptor,
                corpus_string(operation, "field"),
                corpus_value(operation, "value").clone(),
            ),
            "descriptor-repeat" => {
                let mut repeated =
                    corpus_string(operation, "unit").repeat(corpus_usize(operation, "count"));
                if let Some(suffix) = operation.get("suffix").and_then(Json::as_str) {
                    repeated.push_str(suffix);
                }
                descriptor_field(
                    &mut descriptor,
                    corpus_string(operation, "field"),
                    json!(repeated),
                );
            }
            "bid-id-repeat" => {
                let mut repeated =
                    corpus_string(operation, "unit").repeat(corpus_usize(operation, "count"));
                if let Some(suffix) = operation.get("suffix").and_then(Json::as_str) {
                    repeated.push_str(suffix);
                }
                descriptor_field(&mut descriptor, "bidId", json!(repeated));
                set_json_path(
                    &mut envelope,
                    &[
                        json!("seatbid"),
                        json!(0),
                        json!("bid"),
                        json!(0),
                        json!("id"),
                    ],
                    json!(repeated),
                );
            }
            "dimension" => {
                let field = corpus_string(operation, "field");
                let envelope_field = match field {
                    "width" => "w",
                    "height" => "h",
                    _ => panic!("corpus dimension field should be width or height"),
                };
                let value = corpus_value(operation, "value").clone();
                descriptor_field(&mut descriptor, field, value.clone());
                set_json_path(
                    &mut envelope,
                    &[
                        json!("seatbid"),
                        json!(0),
                        json!("bid"),
                        json!(0),
                        json!(envelope_field),
                    ],
                    value,
                );
            }
            "dimensions" => {
                let width = corpus_value(operation, "width").clone();
                let height = corpus_value(operation, "height").clone();
                descriptor_field(&mut descriptor, "width", width.clone());
                descriptor_field(&mut descriptor, "height", height.clone());
                set_json_path(
                    &mut envelope,
                    &[
                        json!("seatbid"),
                        json!(0),
                        json!("bid"),
                        json!(0),
                        json!("w"),
                    ],
                    width,
                );
                set_json_path(
                    &mut envelope,
                    &[
                        json!("seatbid"),
                        json!(0),
                        json!("bid"),
                        json!(0),
                        json!("h"),
                    ],
                    height,
                );
            }
            "creative-url" => {
                let value = corpus_string(operation, "value").to_string();
                descriptor_field(&mut descriptor, "creativeUrl", json!(value));
                set_json_path(
                    &mut envelope,
                    &[
                        json!("seatbid"),
                        json!(0),
                        json!("bid"),
                        json!(0),
                        json!("ext"),
                        json!("creativeurl"),
                    ],
                    json!(value),
                );
            }
            "creative-url-bytes" => {
                let prefix = "https://creative.example/";
                let bytes = corpus_usize(operation, "bytes");
                let value = format!(
                    "{prefix}{}",
                    "a".repeat(
                        bytes
                            .checked_sub(prefix.len())
                            .expect("corpus URL size should include its prefix")
                    )
                );
                descriptor_field(&mut descriptor, "creativeUrl", json!(value));
                set_json_path(
                    &mut envelope,
                    &[
                        json!("seatbid"),
                        json!(0),
                        json!("bid"),
                        json!(0),
                        json!("ext"),
                        json!("creativeurl"),
                    ],
                    json!(value),
                );
            }
            "aax-literal" => {
                encoded_envelope = Some(corpus_string(operation, "value").to_string());
            }
            "aax-bytes" => {
                let bytes: Vec<u8> = corpus_value(operation, "values")
                    .as_array()
                    .expect("corpus byte vector should be an array")
                    .iter()
                    .map(|value| {
                        value
                            .as_u64()
                            .and_then(|value| u8::try_from(value).ok())
                            .expect("corpus byte vector should contain u8 values")
                    })
                    .collect();
                encoded_envelope = Some(BASE64_STANDARD.encode(bytes));
            }
            "aax-raw-json" => {
                encoded_envelope =
                    Some(BASE64_STANDARD.encode(corpus_string(operation, "value").as_bytes()));
            }
            "aax-decoded-bytes" => {
                let mut serialized =
                    serde_json::to_string(&envelope).expect("should serialize corpus envelope");
                let target = corpus_usize(operation, "bytes");
                serialized.push_str(
                    &" ".repeat(
                        target
                            .checked_sub(serialized.len())
                            .expect("corpus decoded size should exceed fixture size"),
                    ),
                );
                encoded_envelope = Some(BASE64_STANDARD.encode(serialized.as_bytes()));
            }
            "aax-raw-price" => {
                let serialized =
                    serde_json::to_string(&envelope).expect("should serialize corpus envelope");
                let replacement = format!("\"price\":{}", corpus_string(operation, "value"));
                let raw = serialized.replacen("\"price\":1.23", &replacement, 1);
                assert_ne!(raw, serialized, "should replace the corpus fixture price");
                encoded_envelope = Some(BASE64_STANDARD.encode(raw.as_bytes()));
            }
            "envelope-set" => {
                let path = corpus_value(operation, "path")
                    .as_array()
                    .expect("corpus path should be an array");
                set_json_path(
                    &mut envelope,
                    path,
                    corpus_value(operation, "value").clone(),
                );
            }
            "envelope-delete" => {
                let path = corpus_value(operation, "path")
                    .as_array()
                    .expect("corpus path should be an array");
                delete_json_path(&mut envelope, path);
            }
            "duplicate-seat" => {
                let seats = envelope
                    .get_mut("seatbid")
                    .and_then(Json::as_array_mut)
                    .expect("corpus fixture should contain a seat array");
                let first = seats
                    .first()
                    .cloned()
                    .expect("corpus fixture should contain one seat");
                seats.push(first);
            }
            "duplicate-bid" => {
                let bids = envelope
                    .get_mut("seatbid")
                    .and_then(Json::as_array_mut)
                    .and_then(|seats| seats.first_mut())
                    .and_then(|seat| seat.get_mut("bid"))
                    .and_then(Json::as_array_mut)
                    .expect("corpus fixture should contain a bid array");
                let first = bids
                    .first()
                    .cloned()
                    .expect("corpus fixture should contain one bid");
                bids.push(first);
            }
            _ => panic!("unknown APS renderer corpus operation: {kind}"),
        }

        let encoded = encoded_envelope.unwrap_or_else(|| {
            BASE64_STANDARD.encode(
                serde_json::to_vec(&envelope).expect("should serialize corpus renderer envelope"),
            )
        });
        descriptor_field(&mut descriptor, "aaxResponse", json!(encoded));
        descriptor
    }

    #[test]
    fn aps_renderer_matches_shared_cross_language_contract_corpus() {
        let corpus: RendererCorpus = serde_json::from_str(include_str!(
            "../../../trusted-server-js/lib/test/fixtures/aps-renderer-v1-corpus.json"
        ))
        .expect("should parse shared APS renderer corpus");
        assert_eq!(
            corpus.publisher_origin, "https://publisher.example",
            "should pin the corpus publisher origin"
        );

        for vector in &corpus.vectors {
            let descriptor = materialize_renderer_corpus_vector(&corpus, vector);
            let actual = classify_aps_renderer_v1(&descriptor, &corpus.publisher_origin).as_str();
            assert_eq!(
                actual, vector.expected,
                "should match APS renderer corpus vector {}",
                vector.id
            );
        }
    }

    fn parse_with_context(
        provider: &ApsAuctionProvider,
        response: PlatformResponse,
    ) -> AuctionResponse {
        let settings = create_test_settings();
        let services = noop_services();
        let downstream = http::Request::builder()
            .uri("https://publisher.example/auction")
            .body(EdgeBody::empty())
            .expect("should build downstream request");
        let context = AuctionContext {
            settings: &settings,
            request: &downstream,
            timeout_ms: 321,
            provider_responses: None,
            services: &services,
        };
        let auction_request = request();
        let debug_request = provider.config.debug.then(|| {
            let openrtb = provider.build_openrtb_request(&auction_request, &context);
            let body = ApsAuctionProvider::serialize_openrtb_request(&openrtb)
                .expect("should serialize APS debug request");
            ApsDebugRequest {
                body: String::from_utf8_lossy(&body).into_owned(),
                headers: BTreeMap::from([(
                    header::CONTENT_TYPE.as_str().to_string(),
                    vec!["application/json".to_string()],
                )]),
            }
        });
        futures::executor::block_on(
            provider.parse_response_with_context_and_state(
                response,
                12,
                &auction_request,
                &context,
                debug_request
                    .as_ref()
                    .map(|state| state as &(dyn core::any::Any + Send + Sync)),
            ),
        )
        .expect("should parse APS response with context")
    }

    #[test]
    fn config_accepts_canonical_alias_and_integer_ids() {
        let canonical: ApsConfig = serde_json::from_value(json!({
            "account_id": "  example-account  "
        }))
        .expect("should parse canonical account ID");
        let alias: ApsConfig =
            serde_json::from_value(json!({"pub_id": 1234})).expect("should parse legacy alias");
        let debug: ApsConfig = serde_json::from_value(json!({
            "account_id": "example-account",
            "debug": true
        }))
        .expect("should parse debug flag");
        assert_eq!(canonical.account_id, "example-account");
        assert_eq!(alias.account_id, "1234");
        assert!(!canonical.enabled);
        assert!(!canonical.debug);
        assert!(debug.debug);
        assert!(!canonical.allow_script_creatives);
        assert!(canonical.endpoint.ends_with("/e/pb/bid"));
    }

    #[test]
    fn config_accepts_default_and_custom_openrtb_endpoints() {
        let default = ApsConfig {
            account_id: "example-account".to_string(),
            ..Default::default()
        };
        let custom: ApsConfig = serde_json::from_value(json!({
            "account_id": "example-account",
            "endpoint": "https://aps.example.com/custom/openrtb"
        }))
        .expect("should deserialize custom endpoint");

        default
            .validate()
            .expect("should accept production default endpoint");
        custom
            .validate()
            .expect("should accept fictional custom HTTPS endpoint");
    }

    #[test]
    fn config_rejects_legacy_aps_endpoint_with_migration_error() {
        for endpoint in [
            "https://aps.example.com/e/dtb/bid",
            "https://aps.example.com/e/dtb/bid/",
            "https://aps.example.com/custom/e/dtb/bid",
        ] {
            let parsed: ApsConfig = serde_json::from_value(json!({
                "account_id": "example-account",
                "endpoint": endpoint
            }))
            .expect("should deserialize legacy endpoint before validation");
            let error = parsed
                .validate()
                .expect_err("should reject legacy endpoint");
            assert!(
                error.to_string().contains("migrate to /e/pb/bid"),
                "should provide endpoint migration guidance: {error}"
            );
        }
    }

    #[test]
    fn config_rejects_blank_duplicate_and_unsafe_endpoint() {
        assert!(serde_json::from_value::<ApsConfig>(json!({"account_id": "   "})).is_err());
        assert!(
            serde_json::from_value::<ApsConfig>(
                json!({"account_id": "x".repeat(MAX_ACCOUNT_ID_BYTES + 1)})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ApsConfig>(json!({
                "account_id": "one",
                "pub_id": "two"
            }))
            .is_err()
        );
        for endpoint in [
            "http://aps.example/e/pb/bid",
            "https://",
            "https://user:password@aps.example/e/pb/bid",
        ] {
            let parsed: ApsConfig = serde_json::from_value(json!({
                "account_id": "example-account",
                "endpoint": endpoint
            }))
            .expect("should deserialize before validation");
            assert!(parsed.validate().is_err(), "should reject {endpoint}");
        }
    }

    #[test]
    fn config_requires_safe_inventory_identity_override_pair() {
        for value in [
            json!({
                "account_id": "example-account",
                "inventory_domain": "publisher.example"
            }),
            json!({
                "account_id": "example-account",
                "inventory_page_origin": "https://www.publisher.example"
            }),
            json!({
                "account_id": "example-account",
                "inventory_domain": "publisher.example",
                "inventory_page_origin": "http://www.publisher.example"
            }),
            json!({
                "account_id": "example-account",
                "inventory_domain": "publisher.example",
                "inventory_page_origin": "https://www.publisher.example/path"
            }),
            json!({
                "account_id": "example-account",
                "inventory_domain": "publisher.example/path",
                "inventory_page_origin": "https://www.publisher.example"
            }),
            json!({
                "account_id": "example-account",
                "inventory_domain": "publisher.example",
                "inventory_page_origin": "https://unrelated.example"
            }),
        ] {
            let parsed: ApsConfig =
                serde_json::from_value(value).expect("should deserialize before validation");
            assert!(
                parsed.validate().is_err(),
                "should reject unsafe or incomplete inventory identity override"
            );
        }
    }

    #[test]
    fn inventory_identity_override_rewrites_site_and_preserves_page_path() {
        let config: ApsConfig = serde_json::from_value(json!({
            "enabled": true,
            "account_id": "example-account",
            "inventory_domain": "publisher.example",
            "inventory_page_origin": "https://www.publisher.example"
        }))
        .expect("should deserialize APS inventory identity override");
        config
            .validate()
            .expect("should validate APS inventory identity override");
        let provider = ApsAuctionProvider::new(config);
        let mut auction_request = request();
        auction_request.publisher.domain = "deployment.example".to_string();
        auction_request.publisher.page_url =
            Some("https://deployment.example/news/story?edition=fictional#section".to_string());
        let settings = create_test_settings();
        let services = noop_services();
        let downstream = http::Request::builder()
            .uri("https://deployment.example/auction")
            .header(
                header::REFERER,
                "https://deployment.example/private?token=fictional#section",
            )
            .body(EdgeBody::empty())
            .expect("should build downstream request");
        let context = AuctionContext {
            settings: &settings,
            request: &downstream,
            timeout_ms: 321,
            provider_responses: None,
            services: &services,
        };

        let serialized =
            serde_json::to_value(provider.build_openrtb_request(&auction_request, &context))
                .expect("should serialize APS request");

        assert_eq!(serialized["site"]["domain"], "publisher.example");
        assert_eq!(
            serialized["site"]["page"],
            "https://www.publisher.example/news/story?edition=fictional"
        );
        assert_eq!(
            serialized["site"]["publisher"]["domain"],
            "publisher.example"
        );
        assert!(serialized["site"].get("ref").is_none());
        assert!(
            !serialized["site"]
                .to_string()
                .contains("deployment.example"),
            "should not leak the deployment host in any Site field"
        );
    }

    #[test]
    fn builds_aps_openrtb_request_with_explicit_privacy_policy() {
        let provider = ApsAuctionProvider::new(config());
        let mut auction_request = request();
        auction_request.user.consent = Some(ConsentContext {
            gdpr_applies: true,
            raw_tc_string: Some("fictional-tcf".to_string()),
            raw_us_privacy: Some("1YNN".to_string()),
            raw_gpp_string: Some("fictional-gpp".to_string()),
            gpp_section_ids: Some(vec![2, 6]),
            ..Default::default()
        });
        auction_request.user.eids = Some(vec![Eid {
            source: "identity.example".to_string(),
            uids: vec![Uid {
                id: "fictional-uid".to_string(),
                atype: Some(1),
                ext: None,
            }],
        }]);
        auction_request.slots[0].formats.extend([
            AdFormat {
                media_type: MediaType::Video,
                width: 640,
                height: 480,
            },
            AdFormat {
                media_type: MediaType::Banner,
                width: u32::MAX,
                height: 90,
            },
            AdFormat {
                media_type: MediaType::Banner,
                width: 728,
                height: 90,
            },
        ]);
        auction_request.device = Some(DeviceInfo {
            user_agent: Some("Fictional Browser".to_string()),
            ip: Some("192.0.2.10".to_string()),
            geo: Some(GeoInfo {
                city: "Example City".to_string(),
                country: "US".to_string(),
                continent: "NA".to_string(),
                latitude: 12.34,
                longitude: 56.78,
                metro_code: 501,
                region: Some("CA".to_string()),
                asn: None,
            }),
        });
        let settings = create_test_settings();
        let services = noop_services();
        let downstream = http::Request::builder()
            .uri("https://publisher.example/auction")
            .header("DNT", "1")
            .header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .header(header::REFERER, "https://referrer.example/article")
            .body(EdgeBody::empty())
            .expect("should build downstream request");
        let context = AuctionContext {
            settings: &settings,
            request: &downstream,
            timeout_ms: 321,
            provider_responses: None,
            services: &services,
        };

        let openrtb = provider.build_openrtb_request(&auction_request, &context);
        let serialized = serde_json::to_value(openrtb).expect("should serialize request");

        assert_eq!(serialized["id"], "fictional-auction");
        assert_eq!(serialized["tmax"], 321);
        assert_eq!(serialized["cur"], json!(["USD"]));
        assert_eq!(serialized["ext"]["account"], "example-account-id");
        assert_eq!(
            serialized["ext"]["sdk"],
            json!({"source": "prebid", "version": "2.2.0"})
        );
        assert_eq!(serialized["imp"][0]["id"], "fictional-slot");
        assert_eq!(serialized["imp"][0]["banner"]["w"], 300);
        assert_eq!(serialized["imp"][0]["banner"]["h"], 250);
        assert_eq!(serialized["imp"][0]["banner"]["topframe"], 0);
        assert_eq!(
            serialized["imp"][0]["banner"]["format"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(serialized["imp"][0]["banner"]["format"][1]["w"], 728);
        assert_eq!(serialized["imp"][0]["bidfloor"], 1.0);
        assert_eq!(serialized["imp"][0]["bidfloorcur"], "USD");
        assert_eq!(serialized["imp"][0]["secure"], 1);
        assert_eq!(serialized["site"]["domain"], "publisher.example");
        assert_eq!(
            serialized["site"]["page"],
            "https://publisher.example/article"
        );
        assert!(
            serialized["site"].get("ref").is_none(),
            "should not forward the raw browser Referer"
        );
        assert_eq!(
            serialized["site"]["publisher"]["domain"],
            "publisher.example"
        );
        assert_eq!(serialized["device"]["ua"], "Fictional Browser");
        assert_eq!(serialized["device"]["ip"], "192.0.2.10");
        assert_eq!(serialized["device"]["dnt"], 1);
        assert_eq!(serialized["device"]["language"], "en");
        assert_eq!(serialized["device"]["geo"]["country"], "US");
        assert!(serialized["device"]["geo"].get("lat").is_none());
        assert!(serialized["device"]["geo"].get("lon").is_none());
        assert_eq!(serialized["user"]["id"], "fictional-user");
        assert_eq!(serialized["user"]["consent"], "fictional-tcf");
        assert_eq!(serialized["user"]["ext"]["consent"], "fictional-tcf");
        assert_eq!(
            serialized["user"]["ext"]["eids"][0]["source"],
            "identity.example"
        );
        assert_eq!(serialized["regs"]["gdpr"], 1);
        assert_eq!(serialized["regs"]["us_privacy"], "1YNN");
        assert_eq!(serialized["regs"]["gpp"], "fictional-gpp");
        assert_eq!(serialized["regs"]["gpp_sid"], json!([2, 6]));
        assert!(serialized["regs"].get("coppa").is_none());
        assert!(serialized["ext"].get("prebid").is_none());
        assert!(serialized["ext"].get("trusted_server").is_none());
        assert!(serialized["imp"][0].get("ext").is_none());
    }

    #[test]
    fn request_language_enforces_byte_limit() {
        let settings = create_test_settings();
        let services = noop_services();

        for (header_value, expected) in [
            ("abcdefgh-US,xy;q=0.9", Some("abcdefgh")),
            ("abcdefghi", None),
        ] {
            let downstream = http::Request::builder()
                .uri("https://publisher.example/auction")
                .header(header::ACCEPT_LANGUAGE, header_value)
                .body(EdgeBody::empty())
                .expect("should build downstream request");
            let context = AuctionContext {
                settings: &settings,
                request: &downstream,
                timeout_ms: 321,
                provider_responses: None,
                services: &services,
            };

            assert_eq!(
                ApsAuctionProvider::request_language(&context).as_deref(),
                expected,
                "should enforce primary language byte limit for {header_value}"
            );
        }
    }

    #[test]
    fn parses_bid_and_builds_exact_minimized_envelope() {
        let provider = ApsAuctionProvider::new(config());
        let response = provider.parse_aps_response(
            &json!({"cur": "USD", "seatbid": [{"seat": 42, "bid": [bid("fictional-selected-bid-id", 1.23, "iframe")]}], "ext": {"userSyncs": []}}),
            12,
            &request(),
        );
        assert_eq!(response.bids.len(), 1);
        let parsed = &response.bids[0];
        assert_eq!(parsed.bidder, "aps");
        assert_eq!(parsed.price, Some(1.23));
        assert!(parsed.creative.is_none());
        assert!(parsed.nurl.is_none());
        let renderer = parsed
            .renderer
            .as_ref()
            .expect("should include renderer")
            .as_aps()
            .expect("should be APS renderer");
        let decoded = BASE64_STANDARD
            .decode(&renderer.aax_response)
            .expect("should decode renderer response");
        let decoded: Json =
            serde_json::from_slice(&decoded).expect("should parse renderer response");
        let fixture: Json = serde_json::from_str(include_str!(
            "../../../trusted-server-js/lib/test/fixtures/aps-renderer-v1.json"
        ))
        .expect("should parse shared APS renderer fixture");
        assert_eq!(decoded, fixture);
    }

    #[test]
    fn creative_id_enforces_utf8_byte_boundary() {
        let provider = ApsAuctionProvider::new(config());
        let mut at_limit = bid("creative-id-limit", 1.23, "iframe");
        at_limit["crid"] = json!("é".repeat(MAX_CREATIVE_ID_BYTES / 2));
        let accepted =
            provider.parse_aps_response(&json!({"seatbid": [{"bid": [at_limit]}]}), 12, &request());
        assert_eq!(
            accepted.bids.len(),
            1,
            "should accept creative ID at byte limit"
        );

        let mut over_limit = bid("creative-id-over-limit", 1.23, "iframe");
        over_limit["crid"] = json!(format!("{}x", "é".repeat(MAX_CREATIVE_ID_BYTES / 2)));
        let rejected = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": [over_limit]}]}),
            12,
            &request(),
        );
        assert!(rejected.bids.is_empty());
        assert_eq!(
            rejected.metadata["drop_reasons"]["creative_id_too_large"],
            1
        );
    }

    #[test]
    fn empty_creative_id_is_omitted_from_renderer() {
        let provider = ApsAuctionProvider::new(config());
        let mut input = bid("bid-with-empty-crid", 1.23, "iframe");
        input["crid"] = json!("");
        let response =
            provider.parse_aps_response(&json!({"seatbid": [{"bid": [input]}]}), 12, &request());
        let bid = response.bids.first().expect("should accept renderer bid");
        assert!(bid.creative_id.is_none());
        assert!(
            bid.renderer
                .as_ref()
                .expect("should retain renderer")
                .as_aps()
                .expect("should be APS renderer")
                .creative_id
                .is_none()
        );
    }

    #[test]
    fn rejects_wrong_typed_response_level_fields() {
        let provider = ApsAuctionProvider::new(config());
        for value in [
            json!({"cur": ["USD"], "seatbid": []}),
            json!({"cur": "USD", "seatbid": "invalid"}),
            json!({"contextual": {"slots": []}}),
        ] {
            let response = provider.parse_aps_response(&value, 12, &request());
            assert!(response.bids.is_empty());
            assert_eq!(
                response.metadata["drop_reasons"]["unexpected_response_shape"],
                1
            );
        }
    }

    #[test]
    fn debug_metadata_matches_pbs_httpcalls_shape() {
        let mut provider_config = config();
        provider_config.debug = true;
        provider_config.endpoint = "https://aps.example/openrtb".to_string();
        let provider = ApsAuctionProvider::new(provider_config);
        let response_body = serde_json::to_vec(&json!({
            "cur": "USD",
            "seatbid": [{"bid": [bid("fictional-debug-bid", 1.23, "iframe")]}]
        }))
        .expect("should serialize APS response fixture");
        let platform_response = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-example-debug", "first")
                .header("x-example-debug", "second")
                .body(EdgeBody::from(response_body.clone()))
                .expect("should build APS response"),
        );

        let response = parse_with_context(&provider, platform_response);

        assert_eq!(response.metadata["accepted_bid_count"], 1);
        let call = &response.metadata["debug"]["httpcalls"]["aps"][0];
        let request_body: Json = serde_json::from_str(
            call["requestbody"]
                .as_str()
                .expect("should include request body as a string"),
        )
        .expect("should parse captured request body");
        assert_eq!(request_body["id"], "fictional-auction");
        assert_eq!(request_body["tmax"], 321);
        assert_eq!(
            call["requestheaders"]["content-type"],
            json!(["application/json"])
        );
        assert_eq!(
            call["responsebody"],
            String::from_utf8_lossy(&response_body).as_ref()
        );
        assert_eq!(
            call["responseheaders"]["content-type"],
            json!(["application/json"])
        );
        assert!(
            call["responseheaders"].get("x-example-debug").is_none(),
            "should omit unapproved debug response headers"
        );
        assert_eq!(call["status"], 200);
        assert_eq!(call["uri"], "https://aps.example/openrtb");
    }

    #[test]
    fn debug_request_metadata_matches_the_dispatched_request() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, br#"{"seatbid":[]}"#.to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let downstream = http::Request::builder()
            .uri("https://publisher.example/auction")
            .body(EdgeBody::empty())
            .expect("should build downstream request");
        let context = AuctionContext {
            settings: &settings,
            request: &downstream,
            timeout_ms: 321,
            provider_responses: None,
            services: &services,
        };
        let mut provider_config = config();
        provider_config.debug = true;
        let provider = ApsAuctionProvider::new(provider_config);
        let auction_request = request();

        let outcome =
            futures::executor::block_on(provider.request_bids(&auction_request, &context))
                .expect("should dispatch APS request");
        let ProviderRequestOutcome::Pending {
            request: pending,
            parse_state,
        } = outcome
        else {
            panic!("should return a pending APS request");
        };
        let platform_response = futures::executor::block_on(services.http_client().wait(pending))
            .expect("should collect APS response");
        let response = futures::executor::block_on(provider.parse_response_with_context_and_state(
            platform_response,
            12,
            &auction_request,
            &context,
            parse_state.as_deref(),
        ))
        .expect("should parse APS response");

        let sent_body = stub
            .recorded_request_bodies()
            .into_iter()
            .next()
            .expect("should capture outbound APS body");
        let sent_headers = stub
            .recorded_request_headers()
            .into_iter()
            .next()
            .expect("should capture outbound APS headers");
        let sent_content_type = sent_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(header::CONTENT_TYPE.as_str()))
            .map(|(_, value)| value.as_str());
        let call = &response.metadata["debug"]["httpcalls"]["aps"][0];
        assert_eq!(
            call["requestbody"],
            String::from_utf8_lossy(&sent_body).as_ref(),
            "debug request body should byte-match the dispatched body"
        );
        assert_eq!(
            call["requestheaders"]["content-type"][0].as_str(),
            sent_content_type,
            "debug request headers should match the dispatched headers"
        );
    }

    #[test]
    fn disabled_debug_omits_httpcall_metadata() {
        let provider = ApsAuctionProvider::new(config());
        let platform_response = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::OK)
                .body(EdgeBody::from(
                    serde_json::to_vec(&json!({"seatbid": []}))
                        .expect("should serialize APS no-bid response"),
                ))
                .expect("should build APS response"),
        );

        let response = parse_with_context(&provider, platform_response);

        assert!(!response.metadata.contains_key("debug"));
        assert_eq!(response.metadata["drop_reasons"]["empty_seatbid"], 1);
    }

    #[test]
    fn debug_metadata_preserves_malformed_and_error_responses() {
        let mut provider_config = config();
        provider_config.debug = true;
        let provider = ApsAuctionProvider::new(provider_config);
        let malformed = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::OK)
                .body(EdgeBody::from(b"{not-json".to_vec()))
                .expect("should build malformed APS response"),
        );
        let unavailable = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header("retry-after", "5")
                .body(EdgeBody::from(b"temporarily unavailable".to_vec()))
                .expect("should build unavailable APS response"),
        );
        let preview_limited = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(EdgeBody::from(vec![
                    0xff;
                    MAX_DEBUG_RESPONSE_PREVIEW_BYTES + 1
                ]))
                .expect("should build preview-limited APS response"),
        );
        let oversized = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(EdgeBody::from(vec![
                    b'x';
                    UPSTREAM_RTB_MAX_RESPONSE_BYTES + 1
                ]))
                .expect("should build oversized APS response"),
        );

        let malformed = parse_with_context(&provider, malformed);
        let unavailable = parse_with_context(&provider, unavailable);
        let preview_limited = parse_with_context(&provider, preview_limited);
        let oversized = parse_with_context(&provider, oversized);

        assert_eq!(
            malformed.metadata["drop_reasons"]["unexpected_response_shape"],
            1
        );
        assert_eq!(
            malformed.metadata["debug"]["httpcalls"]["aps"][0]["responsebody"],
            "{not-json"
        );
        let unavailable_call = &unavailable.metadata["debug"]["httpcalls"]["aps"][0];
        assert_eq!(unavailable.status, BidStatus::Error);
        assert_eq!(unavailable_call["status"], 503);
        assert_eq!(unavailable_call["responsebody"], "temporarily unavailable");
        assert!(
            unavailable_call["responseheaders"]
                .get("retry-after")
                .is_none(),
            "should omit unapproved response headers"
        );
        let preview = preview_limited.metadata["debug"]["httpcalls"]["aps"][0]["responsebody"]
            .as_str()
            .expect("should include bounded response preview");
        assert!(
            preview.ends_with("…(truncated 1 bytes)"),
            "should mark the truncated byte count"
        );
        assert!(
            preview.len() <= MAX_DEBUG_RESPONSE_PREVIEW_BYTES * 3 + 32,
            "lossy UTF-8 expansion should remain bounded"
        );
        let oversized_call = oversized.metadata["debug"]["httpcalls"]["aps"][0]
            .as_object()
            .expect("should include oversized HTTP call metadata");
        assert_eq!(oversized.status, BidStatus::Error);
        assert_eq!(oversized_call["status"], 502);
        assert!(
            !oversized_call.contains_key("responsebody"),
            "should omit response body when bounded capture fails"
        );
    }

    #[test]
    fn debug_metadata_includes_no_content_response() {
        let mut provider_config = config();
        provider_config.debug = true;
        let provider = ApsAuctionProvider::new(provider_config);
        let platform_response = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::NO_CONTENT)
                .body(EdgeBody::empty())
                .expect("should build empty APS response"),
        );

        let response = parse_with_context(&provider, platform_response);

        assert_eq!(response.status, BidStatus::NoBid);
        let call = &response.metadata["debug"]["httpcalls"]["aps"][0];
        assert_eq!(call["status"], 204);
        assert_eq!(call["responsebody"], "");
    }

    #[test]
    fn malformed_json_is_a_safe_shape_error() {
        let provider = ApsAuctionProvider::new(config());
        let platform_response = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::OK)
                .body(EdgeBody::from(b"{not-json".to_vec()))
                .expect("should build malformed APS response"),
        );
        let auction_request = request();
        let response = futures::executor::block_on(provider.parse_response_inner(
            platform_response,
            12,
            Some(&auction_request),
            None,
            false,
        ))
        .expect("should convert malformed JSON into a safe auction response");

        assert!(response.bids.is_empty());
        assert_eq!(
            response.metadata["drop_reasons"]["unexpected_response_shape"],
            1
        );
    }

    #[test]
    fn context_free_parse_response_reports_missing_request_context() {
        let mut provider_config = config();
        provider_config.debug = true;
        let provider = ApsAuctionProvider::new(provider_config);
        let platform_response = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::OK)
                .body(EdgeBody::from(
                    serde_json::to_vec(
                        &json!({"seatbid": [{"bid": [bid("valid", 1.0, "iframe")]}]}),
                    )
                    .expect("should serialize APS response"),
                ))
                .expect("should build APS response"),
        );

        let response = futures::executor::block_on(provider.parse_response(platform_response, 12))
            .expect("should return an explicit context error response");

        assert_eq!(response.status, BidStatus::Error);
        assert!(response.bids.is_empty());
        assert_eq!(
            response.metadata["drop_reasons"]["missing_request_context"],
            1
        );
        let call = &response.metadata["debug"]["httpcalls"]["aps"][0];
        assert_eq!(call["status"], 200);
        assert!(call.get("responsebody").is_some());
        assert!(
            call.get("requestbody").is_none() && call.get("requestheaders").is_none(),
            "context-free parsing should omit unavailable request metadata"
        );
    }

    #[test]
    fn no_content_and_empty_responses_are_no_bids() {
        let provider = ApsAuctionProvider::new(config());
        let platform_response = PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(StatusCode::NO_CONTENT)
                .body(EdgeBody::empty())
                .expect("should build empty APS response"),
        );
        let auction_request = request();
        let no_content = futures::executor::block_on(provider.parse_response_inner(
            platform_response,
            12,
            Some(&auction_request),
            None,
            false,
        ))
        .expect("should parse 204 as no bid");
        assert_eq!(no_content.status, BidStatus::NoBid);

        for value in [json!({}), json!({"seatbid": []})] {
            let empty = provider.parse_aps_response(&value, 12, &auction_request);
            assert_eq!(empty.status, BidStatus::NoBid);
            assert_eq!(empty.metadata["drop_reasons"]["empty_seatbid"], 1);
        }
    }

    #[test]
    fn missing_seatbid_bid_array_is_counted_as_a_drop() {
        let provider = ApsAuctionProvider::new(config());
        let response =
            provider.parse_aps_response(&json!({"seatbid": [{"seat": "aps"}]}), 12, &request());

        assert_eq!(response.status, BidStatus::NoBid);
        assert_eq!(response.metadata["seatbid_count"], 1);
        assert_eq!(response.metadata["dropped_bid_count"], 1);
        assert_eq!(response.metadata["drop_reasons"]["empty_seatbid_bids"], 1);
    }

    #[test]
    fn unsupported_currency_is_a_no_bid() {
        let provider = ApsAuctionProvider::new(config());
        let response = provider.parse_aps_response(
            &json!({"cur": "EUR", "seatbid": [{"bid": [bid("eur-bid", 1.0, "iframe")]}]}),
            12,
            &request(),
        );
        assert_eq!(response.status, BidStatus::NoBid);
        assert_eq!(response.metadata["drop_reasons"]["unsupported_currency"], 1);
    }

    #[test]
    fn malformed_sibling_does_not_suppress_valid_bid() {
        let provider = ApsAuctionProvider::new(config());
        let response = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": ["malformed", bid("valid", 1.0, "iframe")]}]}),
            12,
            &request(),
        );
        assert_eq!(response.bids.len(), 1);
        assert_eq!(response.bids[0].bid_id.as_deref(), Some("valid"));
        assert_eq!(
            response.metadata["drop_reasons"]["missing_render_source"],
            1
        );
    }

    #[test]
    fn enabled_script_bid_keeps_typed_renderer() {
        let mut enabled = config();
        enabled.allow_script_creatives = true;
        let provider = ApsAuctionProvider::new(enabled);
        let response = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": [bid("script", 1.0, "script")]}]}),
            12,
            &request(),
        );
        let renderer = response.bids[0]
            .renderer
            .as_ref()
            .expect("should keep script renderer")
            .as_aps()
            .expect("should be APS renderer");
        assert_eq!(renderer.tag_type, ApsTagType::Script);
    }

    #[test]
    fn disabled_script_cannot_suppress_lower_iframe_bid() {
        let provider = ApsAuctionProvider::new(config());
        let response = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": [bid("script-high", 4.0, "script"), bid("iframe-low", 1.0, "iframe")]}]}),
            12,
            &request(),
        );
        assert_eq!(response.bids.len(), 1);
        assert_eq!(response.bids[0].bid_id.as_deref(), Some("iframe-low"));
        assert_eq!(
            response.metadata["drop_reasons"]["script_rendering_disabled"],
            1
        );
    }

    #[test]
    fn reduces_candidates_by_price_then_bid_id_and_reconciles_drops() {
        let provider = ApsAuctionProvider::new(config());
        let response = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": [
                bid("low-incumbent", 1.0, "iframe"),
                bid("bid-z", 2.0, "iframe"),
                bid("bid-a", 2.0, "iframe"),
                bid("lower-candidate", 1.5, "iframe")
            ]}]}),
            12,
            &request(),
        );
        assert_eq!(response.bids.len(), 1);
        assert_eq!(response.bids[0].bid_id.as_deref(), Some("bid-a"));
        assert_eq!(response.metadata["accepted_bid_count"], 1);
        assert_eq!(response.metadata["dropped_bid_count"], 3);
        assert_eq!(response.metadata["drop_reasons"]["lost_to_higher_bid"], 3);
    }

    #[test]
    fn safe_drops_missing_renderer_and_invalid_dimensions() {
        let provider = ApsAuctionProvider::new(config());
        let mut invalid = bid("invalid", 1.0, "iframe");
        invalid
            .as_object_mut()
            .expect("should be object")
            .remove("w");
        let response = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": [
                {"id": "fixture", "impid": "fictional-slot", "price": 1.0, "w": 300, "h": 250, "ext": {"bidder": "aps"}},
                invalid
            ]}]}),
            12,
            &request(),
        );
        assert!(response.bids.is_empty());
        assert_eq!(
            response.metadata["drop_reasons"]["missing_render_source"],
            1
        );
        assert_eq!(response.metadata["drop_reasons"]["invalid_dimensions"], 1);
    }

    #[test]
    fn rejects_publisher_origin_and_non_https_creative_urls() {
        let provider = ApsAuctionProvider::new(config());
        for creative_url in [
            "https://publisher.example/render",
            "http://creative.example/render",
            "https://user:password@creative.example/render",
        ] {
            let mut invalid = bid("invalid-url", 1.0, "iframe");
            invalid["ext"]["creativeurl"] = json!(creative_url);
            let response = provider.parse_aps_response(
                &json!({"seatbid": [{"bid": [invalid]}]}),
                12,
                &request(),
            );
            assert!(response.bids.is_empty(), "should reject {creative_url}");
            assert_eq!(response.metadata["drop_reasons"]["invalid_creative_url"], 1);
        }

        let mut uppercase_publisher = request();
        uppercase_publisher.publisher.domain = "Creative.Example".to_string();
        uppercase_publisher.publisher.page_url =
            Some("https://Creative.Example/article".to_string());
        let response = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": [bid("same-origin", 1.0, "iframe")]}]}),
            12,
            &uppercase_publisher,
        );
        assert!(response.bids.is_empty());
        assert_eq!(response.metadata["drop_reasons"]["invalid_creative_url"], 1);
    }

    #[test]
    fn registers_and_serves_only_static_renderer_route() {
        let integration = ApsRendererIntegration;
        let routes = integration.routes();
        assert_eq!(routes.len(), 1, "should register one route");
        assert_eq!(routes[0].method, Method::GET);
        assert_eq!(routes[0].path, APS_RENDERER_ROUTE);

        let settings = create_test_settings();
        let services = noop_services();
        let request = http::Request::builder()
            .method(Method::GET)
            .uri(APS_RENDERER_ROUTE)
            .body(EdgeBody::empty())
            .expect("should build renderer request");
        let response =
            futures::executor::block_on(integration.handle(&settings, &services, request))
                .expect("should serve renderer");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            APS_RENDERER_CSP
        );

        let post = http::Request::builder()
            .method(Method::POST)
            .uri(APS_RENDERER_ROUTE)
            .body(EdgeBody::empty())
            .expect("should build method rejection request");
        let response = futures::executor::block_on(integration.handle(&settings, &services, post))
            .expect("should reject unsupported method");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn enabled_config_registers_renderer_proxy() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                APS_INTEGRATION_ID,
                &json!({"enabled": true, "account_id": "example-account"}),
            )
            .expect("should insert APS config");

        let registration = register(&settings)
            .expect("should register APS")
            .expect("should return enabled registration");

        assert_eq!(registration.integration_id, APS_INTEGRATION_ID);
        assert_eq!(registration.proxies.len(), 1);
        assert!(registration.js_disabled);
    }

    #[test]
    fn enabled_invalid_config_fails_provider_registration() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                APS_INTEGRATION_ID,
                &json!({
                    "enabled": true,
                    "account_id": "example-account",
                    "endpoint": "http://insecure.example/openrtb"
                }),
            )
            .expect("should insert invalid APS config for startup validation");

        let _error = register_providers(&settings)
            .err()
            .expect("should reject invalid enabled APS configuration");
    }

    #[test]
    fn renderer_document_is_static_and_nonce_bound() {
        assert!(APS_RENDERER_DOCUMENT.contains("^#tsaps="));
        assert!(APS_RENDERER_DOCUMENT.contains("event.source!==parent"));
        assert!(APS_RENDERER_DOCUMENT.contains("message.nonce!==expected"));
        assert!(APS_RENDERER_DOCUMENT.contains("prebid/creative/render"));
        assert!(APS_RENDERER_DOCUMENT.contains("window._aps instanceof Map"));
        assert!(APS_RENDERER_DOCUMENT.contains("store:new Map([['listeners',new Map()]])"));
        assert!(APS_RENDERER_DOCUMENT.contains("account.queue.push(new CustomEvent"));
        assert!(
            APS_RENDERER_DOCUMENT.contains("trusted-server/aps/renderer-ready")
                && APS_RENDERER_DOCUMENT.contains("trusted-server/aps/renderer-failed")
        );
        assert!(!APS_RENDERER_DOCUMENT.contains("window.apstag"));
        assert!(
            APS_RENDERER_DOCUMENT
                .contains("https://client.aps.amazon-adsystem.com/prebid-creative.js")
        );
        assert!(!APS_RENDERER_DOCUMENT.contains("<script src="));
        let queue_index = APS_RENDERER_DOCUMENT
            .find("account.queue.push(new CustomEvent")
            .expect("should queue render event");
        let runner_index = APS_RENDERER_DOCUMENT
            .find("document.head.appendChild(script)")
            .expect("should dynamically load the APS runner");
        assert!(queue_index < runner_index);
        assert!(!APS_RENDERER_DOCUMENT.contains("allow-same-origin"));
        assert!(APS_RENDERER_CSP.contains("default-src 'none'"));
        assert!(APS_RENDERER_CSP.contains("sandbox allow-forms"));
        assert!(!APS_RENDERER_CSP.contains("allow-same-origin"));
    }
}
