//! Amazon Publisher Services (APS/TAM) `OpenRTB` integration.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
#[cfg(test)]
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

use crate::auction::openrtb::ignored_bidder_params_count;
#[cfg(test)]
use crate::auction::plan::{AuctionPlanConfig, NotificationConfig, ProviderConfig, RoutingMode};
use crate::auction::profile::ApsProfilePlan;
#[cfg(test)]
use crate::auction::provider::{AuctionProvider, ProviderRequestOutcome};
use crate::auction::routing::ProviderAuctionInput;
#[cfg(test)]
use crate::auction::types::{AdSlot, AuctionContext, AuctionRequest};
use crate::auction::types::{
    AdSlot, ApsRendererV1, ApsRendererValidationResult, ApsTagType, AuctionContext,
    AuctionDropReason, AuctionDropReasons, AuctionRequest, AuctionResponse, Bid, BidRenderSourceV1,
    MediaType, RENDER_DIMENSION_MAX, classify_aps_renderer_v1, record_auction_drop,
};
use crate::error::TrustedServerError;
use crate::integrations::ensure_integration_backend_with_transport_timeouts;
use crate::integrations::{
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration, UPSTREAM_RTB_MAX_RESPONSE_BYTES, collect_response_bounded,
};
#[cfg(test)]
use crate::integrations::{
    ensure_integration_backend_with_timeout, predict_integration_backend_name,
};
#[cfg(test)]
use crate::openrtb::ToExt;
#[cfg(test)]
use crate::openrtb::{
    Banner, Device, Format, Geo, Imp, OpenRtbRequest, Publisher, Regs, RegsExt, Site, User,
    UserExt, to_openrtb_i32,
};
use crate::platform::{
    PlatformHttpRequest, PlatformResponse, ProxyHeaderEvidenceV1, RawProxyPolicyV1,
    RawProxyResponseV1, RuntimeServices,
};
use crate::settings::{IntegrationConfig, Settings};

const APS_INTEGRATION_ID: &str = "aps";
const APS_RENDERER_ROUTE: &str = "/integrations/aps/renderer";
pub const APS_RENDERER_V1_ROUTE: &str = "/integrations/aps/renderer/v1";
pub const APS_RUNNER_ROUTE: &str = "/integrations/aps/runner.js";
pub const APS_RUNNER_UPSTREAM_URL: &str =
    "https://client.aps.amazon-adsystem.com/prebid-creative.js";
pub const APS_RUNNER_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const APS_RENDERER_SANDBOX: &str = "allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation";
const DEFAULT_CURRENCY: &str = "USD";
#[cfg(test)]
const APS_SDK_SOURCE: &str = "prebid";
#[cfg(test)]
const APS_SDK_VERSION: &str = "2.2.0";
const MAX_ACCOUNT_ID_BYTES: usize = 1024;
const MAX_BID_ID_BYTES: usize = 64;
const MAX_CREATIVE_ID_BYTES: usize = 1024;
const MAX_DEBUG_RESPONSE_PREVIEW_BYTES: usize = 512;
const MAX_CREATIVE_URL_BYTES: usize = 4096;
#[cfg(test)]
const MAX_LANGUAGE_BYTES: usize = 8;
#[cfg(test)]
const MAX_PAGE_URL_BYTES: usize = 8192;
const MAX_RENDER_ENVELOPE_BYTES: usize = 256 * 1024;
// Exact transport window from dispatch through the final upstream byte.
const APS_RUNNER_TOTAL_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum wait for the APS runner response headers.
pub const APS_RUNNER_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(4);
/// Maximum duration of one blocking APS runner response-body read.
pub const APS_RUNNER_BLOCKING_READ_TIMEOUT: Duration = Duration::from_millis(250);
const APS_RENDERER_CSP: &str = "default-src 'none'; sandbox allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation; script-src 'unsafe-inline' https:; connect-src https:; frame-src https:; img-src https: data:; media-src https: blob:; style-src 'unsafe-inline' https:; font-src https: data:;";

/// Whether `path` belongs to the reserved APS integration family.
#[must_use]
pub fn is_aps_family_path(path: &str) -> bool {
    path == "/integrations/aps" || path.starts_with("/integrations/aps/")
}
const APS_RENDERER_V1_CSP: &str = "default-src 'none'; sandbox allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation; base-uri 'none'; object-src 'none'; script-src 'unsafe-inline' 'self' https:; connect-src https:; frame-src https: data: blob:; img-src https: data: blob:; media-src https: data: blob:; style-src 'unsafe-inline' https:; font-src https: data:; worker-src https: blob:; form-action https:;";

const APS_RENDERER_DOCUMENT: &str = concat!(
    r#"<!doctype html>
<meta charset="utf-8">
<style>html,body{margin:0;padding:0}body>iframe{display:block}</style>
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

const APS_RENDERER_V1_DOCUMENT: &str = concat!(
    r#"<!doctype html>
<meta charset="utf-8">
<style>html,body{margin:0;padding:0;overflow:hidden}</style>
<script>
(function(){
'use strict';
"#,
    include_str!("generated/aps_renderer_validator_v1.js"),
    r#"
var match=/^#tsaps=(n1_[A-Za-z0-9_-]{22})$/.exec(location.hash);
var expected=match&&match[1];
try{history.replaceState(null,'',location.pathname+location.search);}catch(_error){}
if(!expected)return;
var port=null;
var terminal=false;
var runnerLoaded=false;
var callbackOutcome=null;
function send(message){
 if(!port)return;
 try{port.postMessage(message);}catch(_error){}
}
function close(){
 if(!port)return;
 try{port.close();}catch(_error){}
}
function fail(reason){
 if(terminal)return;
 terminal=true;
 send({message:'TS APS Render Failed',version:1,nonce:expected,reason:reason});
 close();
}
function finishCallback(){
 if(terminal||!runnerLoaded||callbackOutcome===null)return;
 if(callbackOutcome==='rejected'){
  fail('runner_failed');
  return;
 }
 terminal=true;
 send({message:'TS APS Render Completed',version:1,nonce:expected});
 close();
}
function publisherOrigin(value){
 if(typeof value!=='string'||value.length===0||value.length>2048)return false;
 for(var index=0;index<value.length;index+=1){
  var code=value.charCodeAt(index);
  if(code<=31||code===127)return false;
 }
 try{
  var parsed=new URL(value);
  return (parsed.protocol==='https:'||parsed.protocol==='http:')&&
   parsed.username===''&&parsed.password===''&&parsed.pathname==='/'&&
   parsed.search===''&&parsed.hash===''&&parsed.origin===value;
 }catch(_error){return false;}
}
function receive(event){
 if(event.source!==parent||!event.ports||event.ports.length!==1)return;
 port=event.ports[0];
 removeEventListener('message',receive);
 var envelope=event.data;
 if(!apsExactRecord(envelope,['nonce','publisherOrigin','renderer','version'])||
    envelope.version!==1||envelope.nonce!==expected||
    !publisherOrigin(envelope.publisherOrigin)||
    classifyApsRendererV1(envelope.renderer,envelope.publisherOrigin)!=='accepted'){
  fail('descriptor_invalid');
  return;
 }
 port.onmessageerror=function(){fail('descriptor_invalid');};
 try{port.start();}catch(_error){}
 var renderer=envelope.renderer;
 send({message:'TS APS Document Accepted',version:1,nonce:expected});
 window._aps=window._aps instanceof Map?window._aps:new Map();
 var account=window._aps.get(renderer.accountId);
 if(!account){
  account={queue:[],store:new Map([['listeners',new Map()]])};
  window._aps.set(renderer.accountId,account);
 }
 var renderPromise=new Promise(function(resolve,reject){
  account.queue.push(new CustomEvent('prebid/creative/render',{detail:{
   aaxResponse:renderer.aaxResponse,
   seatBidId:renderer.bidId,
   source:'internal',
   resolve:resolve,
   reject:reject
 }}));
 });
 renderPromise.then(function(){
  callbackOutcome='resolved';
  finishCallback();
 },function(){
  callbackOutcome='rejected';
  finishCallback();
 });
 var script=document.createElement('script');
 script.src=new URL('/integrations/aps/runner.js',location.href).href;
 script.crossOrigin='anonymous';
 script.referrerPolicy='no-referrer';
 script.onload=function(){
  if(terminal)return;
  runnerLoaded=true;
  send({message:'TS APS Runner Loaded',version:1,nonce:expected});
  finishCallback();
 };
 script.onerror=function(){fail('runner_no_load');};
 document.head.appendChild(script);
}
addEventListener('message',receive);
})();
</script>
"#
);

/// Configuration for the APS `OpenRTB` integration.
#[cfg(test)]
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[validate(schema(function = "validate_inventory_identity_override"))]
pub struct LegacyApsProviderConfig {
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
    #[validate(range(min = 1, max = 60000))]
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
    /// Rendering owner for selected APS bids.
    #[serde(default)]
    pub rendering_mode: ApsRenderingMode,
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

#[cfg(test)]
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

fn validate_inventory_identity_override_values(
    inventory_domain: Option<&str>,
    inventory_page_origin: Option<&str>,
) -> Result<(), ValidationError> {
    let (Some(domain), Some(origin)) = (inventory_domain, inventory_page_origin) else {
        if inventory_domain.is_none() && inventory_page_origin.is_none() {
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

#[cfg(test)]
fn validate_inventory_identity_override(
    config: &LegacyApsProviderConfig,
) -> Result<(), ValidationError> {
    validate_inventory_identity_override_values(
        config.inventory_domain.as_deref(),
        config.inventory_page_origin.as_deref(),
    )
}

#[cfg(test)]
fn default_enabled() -> bool {
    false
}

#[cfg(test)]
fn default_endpoint() -> String {
    "https://web.ads.aps.amazon-adsystem.com/e/pb/bid".to_string()
}

#[cfg(test)]
fn default_timeout_ms() -> u32 {
    800
}

#[cfg(test)]
impl Default for LegacyApsProviderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            account_id: String::new(),
            endpoint: default_endpoint(),
            timeout_ms: default_timeout_ms(),
            debug: false,
            allow_script_creatives: false,
            rendering_mode: ApsRenderingMode::TrustedServer,
            inventory_domain: None,
            inventory_page_origin: None,
        }
    }
}

/// Browser integration toggle retained independently from APS server providers.
#[derive(Debug, Clone, Default, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct ApsConfig {
    /// Whether browser-side APS integration behavior is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Rendering owner for selected APS bids.
    #[serde(default)]
    pub rendering_mode: ApsRenderingMode,
}

#[cfg(test)]
impl IntegrationConfig for LegacyApsProviderConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IntegrationConfig for ApsConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Typed server-side APS profile configuration used by the auction compiler.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApsProfileConfig {
    #[serde(deserialize_with = "deserialize_account_id")]
    pub(crate) account_id: String,
    #[serde(default)]
    pub(crate) debug: bool,
    #[serde(default)]
    pub(crate) allow_script_creatives: bool,
    #[serde(default)]
    pub(crate) inventory_domain: Option<String>,
    #[serde(default)]
    pub(crate) inventory_page_origin: Option<String>,
}

/// Parse and validate server-owned APS profile fields without browser enablement.
pub(crate) fn compile_profile_config(
    value: serde_json::Value,
) -> Result<ApsProfileConfig, Report<TrustedServerError>> {
    let profile: ApsProfileConfig = serde_json::from_value(value).map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: format!("invalid `aps` profile_config: {error}"),
        })
    })?;
    if let Some(domain) = profile.inventory_domain.as_deref() {
        validate_inventory_domain(domain).map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("invalid `aps` profile_config inventory_domain: {error}"),
            })
        })?;
    }
    if let Some(origin) = profile.inventory_page_origin.as_deref() {
        validate_inventory_page_origin(origin).map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("invalid `aps` profile_config inventory_page_origin: {error}"),
            })
        })?;
    }
    validate_inventory_identity_override_values(
        profile.inventory_domain.as_deref(),
        profile.inventory_page_origin.as_deref(),
    )
    .map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: format!("invalid `aps` profile_config inventory identity: {error}"),
        })
    })?;
    Ok(profile)
}

#[cfg(test)]
#[derive(Debug, Serialize)]
struct ApsRequestExt<'a> {
    account: &'a str,
    sdk: ApsSdkExt,
}

#[cfg(test)]
impl ToExt for ApsRequestExt<'_> {}

#[cfg(test)]
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

#[derive(Debug, Clone)]
pub(crate) struct ApsDebugRequest {
    body: String,
    headers: BTreeMap<String, Vec<String>>,
}

impl ApsDebugRequest {
    pub(crate) fn capture(body: &[u8], headers: &HeaderMap) -> Self {
        Self {
            body: String::from_utf8_lossy(body).into_owned(),
            headers: aps_debug_headers(headers),
        }
    }
}

struct PlannedApsResponsePolicy<'a> {
    provider_id: &'a str,
    endpoint: &'a str,
    account_id: &'a str,
    debug: bool,
    allow_script_creatives: bool,
    publisher_domain: &'a str,
}

fn aps_debug_headers(headers: &HeaderMap) -> BTreeMap<String, Vec<String>> {
    // This metadata is client-visible. Keep the list fail-closed so upstream
    // identity or authentication headers can never leak.
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

fn aps_debug_body_preview(body: &[u8]) -> String {
    let preview_len = body.len().min(MAX_DEBUG_RESPONSE_PREVIEW_BYTES);
    let mut preview = String::from_utf8_lossy(&body[..preview_len]).into_owned();
    if body.len() > preview_len {
        preview.push_str(&format!("…(truncated {} bytes)", body.len() - preview_len));
    }
    preview
}

fn attach_planned_aps_metadata(
    mut response: AuctionResponse,
    policy: &PlannedApsResponsePolicy<'_>,
    input: &ProviderAuctionInput,
    debug_request: Option<ApsDebugRequest>,
    response_body: Option<&[u8]>,
    response_headers: &BTreeMap<String, Vec<String>>,
    status: StatusCode,
) -> AuctionResponse {
    response.metadata.insert(
        "routing".to_string(),
        json!({
            "unused_bidder_params_count": ignored_bidder_params_count(input)
        }),
    );
    if !policy.debug {
        return response;
    }

    let mut http_call = json!({
        "responseheaders": response_headers,
        "status": status.as_u16(),
        "uri": policy.endpoint,
    });
    if let Some(http_call) = http_call.as_object_mut() {
        if let Some(request) = debug_request {
            http_call.insert("requestbody".to_string(), json!(request.body));
            http_call.insert("requestheaders".to_string(), json!(request.headers));
        }
        if let Some(response_body) = response_body {
            http_call.insert(
                "responsebody".to_string(),
                json!(aps_debug_body_preview(response_body)),
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

fn planned_aps_renderer(
    policy: &PlannedApsResponsePolicy<'_>,
    input: ApsRendererInput<'_>,
) -> Option<BidRenderer> {
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
    Some(BidRenderer::Aps(ApsRendererV1 {
        version: 1,
        account_id: policy.account_id.to_string(),
        bid_id: input.bid_id.to_string(),
        creative_id: input.creative_id,
        tag_type: input.tag_type,
        creative_url: input.creative_url.to_string(),
        aax_response: BASE64_STANDARD.encode(serialized),
        width: input.width,
        height: input.height,
    }))
}

fn planned_aps_valid_creative_url(value: &str, publisher_domain: &str) -> bool {
    if value.len() > MAX_CREATIVE_URL_BYTES {
        return false;
    }
    let Ok(parsed) = Url::parse(value) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed
            .host_str()
            .is_some_and(|host| !host.eq_ignore_ascii_case(publisher_domain))
        && parsed.username().is_empty()
        && parsed.password().is_none()
}

fn planned_aps_parse_bid(
    policy: &PlannedApsResponsePolicy<'_>,
    value: &Json,
    slots: &HashMap<&str, HashSet<(u32, u32)>>,
    returned_seat: Option<&str>,
) -> Result<Bid, &'static str> {
    let bid_id = value
        .get("id")
        .and_then(Json::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("missing_render_source")?;
    let slot_id = value
        .get("impid")
        .and_then(Json::as_str)
        .ok_or("unknown_impid")?;
    let dimensions = slots.get(slot_id).ok_or("unknown_impid")?;
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
    let width = value
        .get("w")
        .and_then(Json::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("invalid_dimensions")?;
    let height = value
        .get("h")
        .and_then(Json::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("invalid_dimensions")?;
    if width == 0 || height == 0 || !dimensions.contains(&(width, height)) {
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
    if !planned_aps_valid_creative_url(creative_url, policy.publisher_domain) {
        return Err("invalid_creative_url");
    }
    let tag_type = match ext.get("tagtype").and_then(Json::as_str) {
        Some("iframe") => ApsTagType::Iframe,
        Some("script") if policy.allow_script_creatives => ApsTagType::Script,
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
    let renderer = planned_aps_renderer(
        policy,
        ApsRendererInput {
            bid_id,
            creative_id: creative_id.clone(),
            tag_type,
            creative_url,
            price,
            width,
            height,
        },
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
        returned_seat: returned_seat.map(str::to_string),
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

fn increment_planned_aps_reason(reasons: &mut BTreeMap<String, u64>, reason: &'static str) {
    *reasons.entry(reason.to_string()).or_default() += 1;
}

fn parse_planned_aps_value(
    value: &Json,
    response_time_ms: u64,
    input: &ProviderAuctionInput,
    policy: &PlannedApsResponsePolicy<'_>,
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
        return AuctionResponse::error(policy.provider_id, response_time_ms)
            .with_metadata("error_type", json!("parse_response"))
            .with_metadata("drop_reasons", json!({"unexpected_response_shape": 1}));
    }
    if value
        .get("cur")
        .and_then(Json::as_str)
        .is_some_and(|currency| !currency.eq_ignore_ascii_case(DEFAULT_CURRENCY))
    {
        return AuctionResponse::no_bid(policy.provider_id, response_time_ms)
            .with_metadata("drop_reasons", json!({"unsupported_currency": 1}));
    }

    let slots = input
        .slots()
        .iter()
        .map(|slot| {
            let dimensions = slot
                .slot()
                .formats
                .iter()
                .filter(|format| format.media_type == MediaType::Banner)
                .map(|format| (format.width, format.height))
                .collect::<HashSet<_>>();
            (slot.slot().id.as_str(), dimensions)
        })
        .collect::<HashMap<_, _>>();
    let seatbids = value.get("seatbid").and_then(Json::as_array);
    let seatbid_count = seatbids.map_or(0, Vec::len);
    let mut reasons = BTreeMap::new();
    let mut selected: HashMap<String, Bid> = HashMap::new();
    let mut dropped = 0_u64;

    for seatbid in seatbids.into_iter().flatten() {
        let returned_seat = seatbid
            .get("seat")
            .and_then(Json::as_str)
            .filter(|seat| !seat.is_empty());
        let Some(bids) = seatbid.get("bid").and_then(Json::as_array) else {
            dropped += 1;
            increment_planned_aps_reason(&mut reasons, "empty_seatbid_bids");
            continue;
        };
        for value in bids {
            match planned_aps_parse_bid(policy, value, &slots, returned_seat) {
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
                            increment_planned_aps_reason(&mut reasons, "lost_to_higher_bid");
                        }
                    } else {
                        dropped += 1;
                        increment_planned_aps_reason(&mut reasons, "lost_to_higher_bid");
                    }
                }
                Err(reason) => {
                    dropped += 1;
                    increment_planned_aps_reason(&mut reasons, reason);
                }
            }
        }
    }

    if seatbid_count == 0 {
        increment_planned_aps_reason(&mut reasons, "empty_seatbid");
    }
    let accepted = selected.len();
    let metadata = [
        ("seatbid_count".to_string(), json!(seatbid_count)),
        ("accepted_bid_count".to_string(), json!(accepted)),
        ("dropped_bid_count".to_string(), json!(dropped)),
        ("drop_reasons".to_string(), json!(reasons)),
    ];
    let mut response = if selected.is_empty() {
        AuctionResponse::no_bid(policy.provider_id, response_time_ms)
    } else {
        AuctionResponse::success(
            policy.provider_id,
            selected.into_values().collect(),
            response_time_ms,
        )
    };
    response.metadata.extend(metadata);
    response
}

/// Parse one APS-profile response using only provider-local routed state.
pub(crate) async fn parse_planned_aps_response(
    provider_id: &str,
    profile: &ApsProfilePlan,
    endpoint: &str,
    input: &ProviderAuctionInput,
    response: PlatformResponse,
    response_time_ms: u64,
    debug_request: Option<ApsDebugRequest>,
) -> Result<AuctionResponse, Report<TrustedServerError>> {
    let policy = PlannedApsResponsePolicy {
        provider_id,
        endpoint,
        account_id: &profile.account_id,
        debug: profile.debug,
        allow_script_creatives: profile.allow_script_creatives,
        publisher_domain: &input.common_request().publisher.domain,
    };
    let response = response.response;
    let status = response.status();
    let response_headers = if policy.debug {
        aps_debug_headers(response.headers())
    } else {
        BTreeMap::new()
    };

    if status == StatusCode::NO_CONTENT {
        return Ok(attach_planned_aps_metadata(
            AuctionResponse::no_bid(provider_id, response_time_ms),
            &policy,
            input,
            debug_request,
            Some(&[]),
            &response_headers,
            status,
        ));
    }
    if !status.is_success() {
        log::warn!("APS profile {provider_id} returns a non-success status");
        let body = if policy.debug {
            match collect_response_bounded(
                response.into_body(),
                UPSTREAM_RTB_MAX_RESPONSE_BYTES,
                APS_INTEGRATION_ID,
            )
            .await
            {
                Ok(body) => Some(body),
                Err(error) => {
                    log::warn!("Failed to read APS profile debug response body: {error:?}");
                    None
                }
            }
        } else {
            None
        };
        return Ok(attach_planned_aps_metadata(
            AuctionResponse::error(provider_id, response_time_ms)
                .with_metadata("error_type", json!("http_status"))
                .with_metadata("http_status", json!(status.as_u16())),
            &policy,
            input,
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
        message: format!("Failed to read APS profile {provider_id} response body"),
    })?;
    let value: Json = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            log::warn!("Failed to parse APS profile {provider_id} response JSON: {error}");
            let parsed = AuctionResponse::error(provider_id, response_time_ms)
                .with_metadata("error_type", json!("parse_response"))
                .with_metadata("drop_reasons", json!({"unexpected_response_shape": 1}));
            return Ok(attach_planned_aps_metadata(
                parsed,
                &policy,
                input,
                debug_request,
                Some(&body),
                &response_headers,
                status,
            ));
        }
    };
    let parsed = parse_planned_aps_value(&value, response_time_ms, input, &policy);
    Ok(attach_planned_aps_metadata(
        parsed,
        &policy,
        input,
        debug_request,
        Some(&body),
        &response_headers,
        status,
    ))
}

/// Legacy APS `OpenRTB` auction provider retained only for parity tests.
#[cfg(test)]
pub struct ApsAuctionProvider {
    config: LegacyApsProviderConfig,
}

#[cfg(test)]
impl ApsAuctionProvider {
    /// Create an APS provider from validated configuration.
    #[must_use]
    pub fn new(config: LegacyApsProviderConfig) -> Self {
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
    ) -> Result<BidRenderSourceV1, AuctionDropReason> {
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
        let serialized = serde_json::to_vec(&envelope)
            .map_err(|_| AuctionDropReason::InvalidProviderResponse)?;
        if serialized.len() > MAX_RENDER_ENVELOPE_BYTES {
            return Err(AuctionDropReason::RenderPayloadTooLarge);
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
        let value = serde_json::to_value(&renderer)
            .map_err(|_| AuctionDropReason::InvalidProviderResponse)?;
        if classify_aps_renderer_v1(&value, publisher_origin)
            != ApsRendererValidationResult::Accepted
        {
            return Err(AuctionDropReason::InvalidProviderResponse);
        }
        Ok(renderer)
    }

    fn parse_bid(
        &self,
        value: &Json,
        slots: &HashMap<&str, &AdSlot>,
        publisher_origin: &str,
        duplicate_ids: &HashSet<String>,
    ) -> Result<Bid, AuctionDropReason> {
        let object = value.as_object().ok_or(AuctionDropReason::MalformedBid)?;
        let Some(bid_id_value) = object.get("id") else {
            return Err(AuctionDropReason::MissingUpstreamBidId);
        };
        let bid_id = bid_id_value
            .as_str()
            .ok_or(AuctionDropReason::InvalidUpstreamBidId)?;
        if bid_id.is_empty() {
            return Err(AuctionDropReason::MissingUpstreamBidId);
        }
        if bid_id.len() > MAX_BID_ID_BYTES {
            return Err(AuctionDropReason::UpstreamBidIdTooLarge);
        }
        if bid_id.bytes().any(|byte| byte <= 0x1f || byte == 0x7f) {
            return Err(AuctionDropReason::InvalidUpstreamBidId);
        }
        if duplicate_ids.contains(bid_id) {
            return Err(AuctionDropReason::DuplicateUpstreamBidId);
        }
        let slot_id = value
            .get("impid")
            .and_then(Json::as_str)
            .ok_or(AuctionDropReason::UnknownImpression)?;
        let slot = slots
            .get(slot_id)
            .ok_or(AuctionDropReason::UnknownImpression)?;
        let price = value
            .get("price")
            .and_then(Json::as_f64)
            .filter(|price| price.is_finite() && *price >= 0.0)
            .ok_or(AuctionDropReason::InvalidPrice)?;
        if value
            .get("mtype")
            .is_some_and(|mtype| mtype.as_i64() != Some(1))
        {
            return Err(AuctionDropReason::UnsupportedMediaType);
        }
        let parse_dimension = |field: &str| {
            let number = value
                .get(field)
                .and_then(Json::as_f64)
                .filter(|number| number.is_finite() && number.fract() == 0.0 && *number > 0.0)
                .ok_or(AuctionDropReason::InvalidDimensions)?;
            if number > RENDER_DIMENSION_MAX as f64 {
                return Err(AuctionDropReason::DimensionsOutOfRange);
            }
            u32::try_from(number as u64).map_err(|_| AuctionDropReason::DimensionsOutOfRange)
        };
        let width = parse_dimension("w")?;
        let height = parse_dimension("h")?;
        if !Self::compatible_dimensions(slot, width, height) {
            return Err(AuctionDropReason::InvalidDimensions);
        }
        let ext = value
            .get("ext")
            .and_then(Json::as_object)
            .ok_or(AuctionDropReason::MissingCreativeUrl)?;
        let Some(creative_url_value) = ext.get("creativeurl") else {
            return Err(AuctionDropReason::MissingCreativeUrl);
        };
        let creative_url = creative_url_value
            .as_str()
            .ok_or(AuctionDropReason::InvalidCreativeUrl)?;
        if !self.valid_creative_url(creative_url, publisher_origin) {
            return Err(AuctionDropReason::InvalidCreativeUrl);
        }
        let tag_type = match ext.get("tagtype").and_then(Json::as_str) {
            Some("iframe") => ApsTagType::Iframe,
            Some("script") if self.config.allow_script_creatives => ApsTagType::Script,
            Some("script") => return Err(AuctionDropReason::ScriptRenderingDisabled),
            _ => return Err(AuctionDropReason::InvalidTagType),
        };
        let creative_id = match value.get("crid") {
            None => None,
            Some(Json::String(creative_id)) if creative_id.is_empty() => None,
            Some(Json::String(creative_id)) => Some(creative_id.clone()),
            Some(_) => return Err(AuctionDropReason::InvalidCreativeId),
        };
        if creative_id
            .as_ref()
            .is_some_and(|creative_id| creative_id.len() > MAX_CREATIVE_ID_BYTES)
        {
            return Err(AuctionDropReason::CreativeIdTooLarge);
        }
        let renderer = self.build_renderer(
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
        )?;
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
            candidate_id: None,
            candidate_provider: None,
            renderer_reservation_id: None,
            price: Some(price),
            currency: DEFAULT_CURRENCY.to_string(),
            creative: None,
            adomain,
            bidder: APS_INTEGRATION_ID.to_string(),
            returned_seat: None,
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
                .with_drop_reason(AuctionDropReason::InvalidProviderResponse);
        }
        if value
            .get("cur")
            .and_then(Json::as_str)
            .is_some_and(|currency| !currency.eq_ignore_ascii_case(DEFAULT_CURRENCY))
        {
            return AuctionResponse::error(APS_INTEGRATION_ID, response_time_ms)
                .with_drop_reason(AuctionDropReason::InvalidProviderResponse);
        }

        let slots: HashMap<&str, &AdSlot> = request
            .slots
            .iter()
            .map(|slot| (slot.id.as_str(), slot))
            .collect();
        let seatbids = value.get("seatbid").and_then(Json::as_array);
        let seatbid_count = seatbids.map_or(0, Vec::len);
        let mut reasons = AuctionDropReasons::new();
        let mut selected: HashMap<String, Bid> = HashMap::new();
        let mut dropped = 0_u64;

        let mut id_counts = HashMap::<&str, usize>::new();
        for candidate in seatbids
            .into_iter()
            .flatten()
            .filter_map(|seatbid| seatbid.get("bid").and_then(Json::as_array))
            .flatten()
        {
            if let Some(bid_id) = candidate.get("id").and_then(Json::as_str)
                && !bid_id.is_empty()
                && bid_id.len() <= MAX_BID_ID_BYTES
                && !bid_id.bytes().any(|byte| byte <= 0x1f || byte == 0x7f)
            {
                *id_counts.entry(bid_id).or_default() += 1;
            }
        }
        let duplicate_ids: HashSet<String> = id_counts
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(bid_id, _)| bid_id.to_string())
            .collect();

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
                record_auction_drop(&mut reasons, AuctionDropReason::EmptySeatBidBids);
                continue;
            };
            for value in bids {
                match self.parse_bid(value, &slots, &publisher_origin, &duplicate_ids) {
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
                                record_auction_drop(
                                    &mut reasons,
                                    AuctionDropReason::LostToHigherBid,
                                );
                            }
                        } else {
                            dropped += 1;
                            record_auction_drop(&mut reasons, AuctionDropReason::LostToHigherBid);
                        }
                    }
                    Err(reason) => {
                        dropped += 1;
                        record_auction_drop(&mut reasons, reason);
                    }
                }
            }
        }

        if seatbid_count == 0 {
            record_auction_drop(&mut reasons, AuctionDropReason::EmptySeatBid);
        }
        let accepted = selected.len();
        let metadata = [
            ("seatbid_count".to_string(), json!(seatbid_count)),
            ("accepted_bid_count".to_string(), json!(accepted)),
            ("dropped_bid_count".to_string(), json!(dropped)),
        ];
        let mut response = if selected.is_empty() {
            AuctionResponse::no_bid(APS_INTEGRATION_ID, response_time_ms)
        } else {
            let bids = request
                .slots
                .iter()
                .filter_map(|slot| selected.remove(&slot.id))
                .collect();
            AuctionResponse::success(APS_INTEGRATION_ID, bids, response_time_ms)
        };
        response.metadata.extend(metadata);
        response.with_drop_reasons(&reasons)
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
                    .with_drop_reason(AuctionDropReason::InvalidProviderResponse);
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
                .with_drop_reason(AuctionDropReason::MissingRequestContext);
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

#[cfg(test)]
#[async_trait(?Send)]
impl AuctionProvider for ApsAuctionProvider {
    fn provider_name(&self) -> &str {
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
        let debug_body = self
            .config
            .debug
            .then(|| String::from_utf8_lossy(&body).into_owned());
        let outbound_request = http::Request::builder()
            .method(Method::POST)
            .uri(&self.config.endpoint)
            .header(header::CONTENT_TYPE, "application/json")
            .body(EdgeBody::from(body))
            .change_context(TrustedServerError::Auction {
                message: "Failed to build APS request".to_string(),
            })?;
        let debug_request = debug_body.map(|body| ApsDebugRequest {
            body,
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
struct ApsRendererIntegration {
    rendering_mode: ApsRenderingMode,
}

#[async_trait(?Send)]
impl IntegrationProxy for ApsRendererIntegration {
    fn integration_name(&self) -> &'static str {
        APS_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        (self.rendering_mode == ApsRenderingMode::TrustedServer)
            .then(|| IntegrationEndpoint::get(APS_RENDERER_ROUTE))
            .into_iter()
            .collect()
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

#[derive(Debug)]
pub(crate) struct ApsV1Integration {
    enabled: bool,
}

impl ApsV1Integration {
    fn mark_exact_headers(mut response: http::Response<EdgeBody>) -> http::Response<EdgeBody> {
        response
            .extensions_mut()
            .insert(crate::platform::ExactResponseHeadersV1);
        response
    }

    pub(crate) fn from_settings(settings: &Settings) -> Result<Self, Report<TrustedServerError>> {
        Ok(Self {
            enabled: settings
                .integration_config::<ApsConfig>(APS_INTEGRATION_ID)?
                .is_some(),
        })
    }

    fn local_status(
        status: StatusCode,
        allow_get: bool,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let mut builder = http::Response::builder()
            .status(status)
            .header(header::CACHE_CONTROL, "no-store");
        if allow_get {
            builder = builder.header(header::ALLOW, "GET");
        }
        builder
            .body(EdgeBody::empty())
            .change_context(TrustedServerError::Integration {
                integration: APS_INTEGRATION_ID.to_string(),
                message: "Failed to build local APS route response".to_string(),
            })
            .map(Self::mark_exact_headers)
    }

    fn renderer_response() -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
            .header("x-content-type-options", "nosniff")
            .header("referrer-policy", "no-referrer")
            .header(header::CONTENT_SECURITY_POLICY, APS_RENDERER_V1_CSP)
            .body(EdgeBody::from(APS_RENDERER_V1_DOCUMENT))
            .change_context(TrustedServerError::Integration {
                integration: APS_INTEGRATION_ID.to_string(),
                message: "Failed to build APS renderer v1 response".to_string(),
            })
            .map(Self::mark_exact_headers)
    }

    fn singleton_proxy_header(
        evidence: &ProxyHeaderEvidenceV1,
        required: bool,
    ) -> Result<Option<&[u8]>, &'static str> {
        match evidence {
            ProxyHeaderEvidenceV1::Occurrences(values) => match values.as_slice() {
                [] if !required => Ok(None),
                [value] => Ok(Some(value.as_slice())),
                [] => Err("missing_header"),
                _ => Err("duplicate_header"),
            },
            ProxyHeaderEvidenceV1::Combined(value) if !value.contains(&b',') => {
                Ok(Some(value.as_slice()))
            }
            ProxyHeaderEvidenceV1::Combined(_) => Err("listed_header"),
            ProxyHeaderEvidenceV1::Unavailable => Err("unavailable_header"),
        }
    }

    fn trim_http_ows(value: &[u8]) -> &[u8] {
        let start = value
            .iter()
            .position(|byte| !matches!(byte, b' ' | b'\t'))
            .unwrap_or(value.len());
        let end = value
            .iter()
            .rposition(|byte| !matches!(byte, b' ' | b'\t'))
            .map_or(start, |index| index + 1);
        &value[start..end]
    }

    fn validate_runner_content_type(evidence: &ProxyHeaderEvidenceV1) -> Result<(), &'static str> {
        let raw = Self::singleton_proxy_header(evidence, true)?;
        let value = std::str::from_utf8(raw.ok_or("missing_content_type")?)
            .map_err(|_| "invalid_content_type")?;
        if !value.is_ascii() {
            return Err("invalid_content_type");
        }
        if value.as_bytes().contains(&b',') {
            return Err("listed_content_type");
        }

        let mut parts = value.split(';');
        let essence = parts
            .next()
            .ok_or("missing_content_type")?
            .trim_matches([' ', '\t']);
        if !essence.eq_ignore_ascii_case("application/javascript")
            && !essence.eq_ignore_ascii_case("text/javascript")
        {
            return Err("rejected_content_type");
        }
        let Some(parameter) = parts.next() else {
            return Ok(());
        };
        if parts.next().is_some() {
            return Err("duplicate_content_type_parameter");
        }
        let (name, value) = parameter
            .split_once('=')
            .ok_or("invalid_content_type_parameter")?;
        if !name
            .trim_matches([' ', '\t'])
            .eq_ignore_ascii_case("charset")
            || !value
                .trim_matches([' ', '\t'])
                .eq_ignore_ascii_case("utf-8")
        {
            return Err("rejected_content_type_parameter");
        }
        Ok(())
    }

    fn validate_runner_content_encoding(
        evidence: &ProxyHeaderEvidenceV1,
    ) -> Result<(), &'static str> {
        let Some(raw) = Self::singleton_proxy_header(evidence, false)? else {
            return Ok(());
        };
        let value = Self::trim_http_ows(raw);
        if value.contains(&b',') || !value.eq_ignore_ascii_case(b"identity") {
            return Err("rejected_content_encoding");
        }
        Ok(())
    }

    fn validate_runner_content_length(
        evidence: &ProxyHeaderEvidenceV1,
    ) -> Result<Option<usize>, &'static str> {
        let Some(value) = Self::singleton_proxy_header(evidence, false)? else {
            return Ok(None);
        };
        if value.is_empty()
            || value.contains(&b',')
            || !value.iter().all(u8::is_ascii_digit)
            || (value.len() > 1 && value[0] == b'0')
        {
            return Err("invalid_content_length");
        }
        let value = std::str::from_utf8(value)
            .map_err(|_| "invalid_content_length")?
            .parse::<usize>()
            .map_err(|_| "invalid_content_length")?;
        if value > APS_RUNNER_MAX_RESPONSE_BYTES {
            return Err("content_length_overflow");
        }
        Ok(Some(value))
    }

    fn validate_runner_response(response: &RawProxyResponseV1) -> Result<(), &'static str> {
        if response.evidence.status != StatusCode::OK.as_u16() {
            return Err("rejected_status");
        }
        Self::validate_runner_content_type(&response.evidence.content_type)?;
        Self::validate_runner_content_encoding(&response.evidence.content_encoding)?;
        let declared_length =
            Self::validate_runner_content_length(&response.evidence.content_length)?;
        if response.body.len() > APS_RUNNER_MAX_RESPONSE_BYTES {
            return Err("body_overflow");
        }
        if declared_length.is_some_and(|length| length != response.body.len()) {
            return Err("content_length_mismatch");
        }
        std::str::from_utf8(&response.body).map_err(|_| "invalid_utf8")?;
        Ok(())
    }

    async fn runner_response(
        services: &RuntimeServices,
    ) -> Result<http::Response<EdgeBody>, &'static str> {
        let outbound_request = http::Request::builder()
            .method(Method::GET)
            .uri(APS_RUNNER_UPSTREAM_URL)
            .header(header::ACCEPT_ENCODING, "identity")
            .body(EdgeBody::empty())
            .map_err(|_| "request_build_failed")?;
        let backend = ensure_integration_backend_with_transport_timeouts(
            services,
            APS_RUNNER_UPSTREAM_URL,
            APS_INTEGRATION_ID,
            APS_RUNNER_FIRST_BYTE_TIMEOUT,
            APS_RUNNER_BLOCKING_READ_TIMEOUT,
        )
        .map_err(|_| "backend_unavailable")?;
        let response = services
            .http_client()
            .send_raw_proxy_v1(
                PlatformHttpRequest::new(outbound_request, backend),
                RawProxyPolicyV1 {
                    total_timeout: APS_RUNNER_TOTAL_TIMEOUT,
                    first_byte_timeout: APS_RUNNER_FIRST_BYTE_TIMEOUT,
                    blocking_read_timeout: APS_RUNNER_BLOCKING_READ_TIMEOUT,
                    max_response_bytes: APS_RUNNER_MAX_RESPONSE_BYTES,
                },
            )
            .await
            .map_err(|_| "transport_failed")?;
        Self::validate_runner_response(&response)?;

        http::Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header("cross-origin-resource-policy", "cross-origin")
            .header("x-content-type-options", "nosniff")
            .header("referrer-policy", "no-referrer")
            .body(EdgeBody::from(response.body))
            .map(Self::mark_exact_headers)
            .map_err(|_| "response_build_failed")
    }
}

#[async_trait(?Send)]
impl IntegrationProxy for ApsV1Integration {
    fn integration_name(&self) -> &'static str {
        APS_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        Vec::new()
    }

    async fn handle(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
        request: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let path = request.uri().path();
        if !path.starts_with("/integrations/aps/") {
            return Self::local_status(StatusCode::NOT_FOUND, false);
        }
        if request.method() != Method::GET {
            return Self::local_status(StatusCode::METHOD_NOT_ALLOWED, true);
        }
        if !self.enabled {
            return Self::local_status(StatusCode::NOT_FOUND, false);
        }
        match path {
            APS_RENDERER_V1_ROUTE => Self::renderer_response(),
            APS_RUNNER_ROUTE => match Self::runner_response(services).await {
                Ok(response) => Ok(response),
                Err(reason) => {
                    log::warn!("APS runner proxy failed closed: {reason}");
                    Self::local_status(StatusCode::BAD_GATEWAY, false)
                }
            },
            _ => Self::local_status(StatusCode::NOT_FOUND, false),
        }
    }
}

/// Register the APS static renderer endpoint when APS is enabled.
///
/// # Errors
///
/// Returns an error when enabled APS configuration is invalid.
pub fn register_for_plan(
    settings: &Settings,
    plan: &crate::auction::AuctionPlan,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    if !plan.has_profile(APS_INTEGRATION_ID) {
        return Ok(None);
    }
    let rendering_mode = settings
        .integration_config::<ApsConfig>(APS_INTEGRATION_ID)?
        .map(|config| config.rendering_mode)
        .unwrap_or_default();
    let integration = Arc::new(ApsRendererIntegration { rendering_mode });
    let registration = IntegrationRegistration::builder(APS_INTEGRATION_ID)
        .without_js()
        .with_head_injector(integration.clone());
    let registration = if rendering_mode == ApsRenderingMode::TrustedServer {
        registration.with_proxy(integration)
    } else {
        registration
    };
    Ok(Some(registration.build()))
}

/// Register the APS auction provider when enabled.
///
/// # Errors
///
/// Returns an error when enabled APS configuration is invalid.
#[cfg(test)]
#[allow(clippy::missing_panics_doc)]
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<LegacyApsProviderConfig>(APS_INTEGRATION_ID)?
    else {
        return Ok(None);
    };
    let mut browser_settings = settings.clone();
    browser_settings.integrations.insert_config(
        APS_INTEGRATION_ID,
        &ApsConfig {
            enabled: true,
            rendering_mode: config.rendering_mode,
        },
    )?;
    register_for_plan(
        &browser_settings,
        &crate::auction::AuctionPlan::compile(AuctionPlanConfig {
            timeout_ms: 1000,
            providers: BTreeMap::from([(
                "aps".parse().expect("should parse APS provider ID"),
                ProviderConfig {
                    protocol: "openrtb-2.6".to_string(),
                    profile: "aps".to_string(),
                    endpoint: default_endpoint(),
                    timeout_ms: None,
                    routing: RoutingMode::AllEligible,
                    notifications: NotificationConfig::default(),
                    profile_config: serde_json::json!({"account_id":"example-account"}),
                },
            )]),
            ..AuctionPlanConfig::default()
        })
        .expect("should compile APS renderer test plan"),
    )
}

#[cfg(test)]
#[allow(clippy::missing_errors_doc)]
pub fn register_providers(
    settings: &Settings,
) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<LegacyApsProviderConfig>(APS_INTEGRATION_ID)?
    else {
        return Ok(Vec::new());
    };
    log::info!("Registering APS OpenRTB provider");
    if config.debug {
        log::warn!(
            "APS debug mode is ON — raw request and response data, including creative markup, will be included in client-visible /auction responses"
        );
    }
    if config.rendering_mode == ApsRenderingMode::PublisherNative && config.allow_script_creatives {
        log::warn!(
            "APS publisher-native rendering with script creatives is ON; selected bidder scripts execute with publisher-origin privileges"
        );
    }
    Ok(vec![Arc::new(ApsAuctionProvider::new(config))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::test_support::canonical_parity_auction_request;
    use crate::auction::types::{
        AdFormat, AdSlot, AuctionContext, AuctionDropReason, AuctionRequest, BidStatus, DeviceInfo,
        PublisherInfo, UserInfo,
    };
    use crate::consent::ConsentContext;
    use crate::openrtb::{Eid, Uid};
    use crate::platform::test_support::{
        StubHttpClient, build_services_with_http_client, noop_services,
    };
    use crate::platform::{
        GeoInfo, ProxyHeaderEvidenceV1, ProxyResponseEvidenceV1, RawProxyPolicyV1,
        RawProxyResponseV1,
    };
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;

    fn config() -> LegacyApsProviderConfig {
        LegacyApsProviderConfig {
            enabled: true,
            account_id: "example-account-id".to_string(),
            endpoint: default_endpoint(),
            timeout_ms: 800,
            debug: false,
            allow_script_creatives: false,
            rendering_mode: ApsRenderingMode::TrustedServer,
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

    fn drop_count(response: &AuctionResponse, reason: AuctionDropReason) -> u64 {
        response.metadata["drop_reasons"][reason.as_str()]
            .as_u64()
            .unwrap_or_default()
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
            transport_timeout_ms: 321,
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
    fn config_defaults_to_the_800ms_aps_budget() {
        let parsed: LegacyApsProviderConfig = serde_json::from_value(json!({
            "account_id": "example-account"
        }))
        .expect("should parse APS defaults");

        assert_eq!(
            parsed.timeout_ms, 800,
            "should preserve APS's 800ms default"
        );
    }

    #[test]
    fn config_accepts_canonical_alias_and_integer_ids() {
        let canonical: LegacyApsProviderConfig = serde_json::from_value(json!({
            "account_id": "  example-account  "
        }))
        .expect("should parse canonical account ID");
        let alias: LegacyApsProviderConfig =
            serde_json::from_value(json!({"pub_id": 1234})).expect("should parse legacy alias");
        let debug: LegacyApsProviderConfig = serde_json::from_value(json!({
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
        assert_eq!(canonical.rendering_mode, ApsRenderingMode::TrustedServer);
        assert!(canonical.endpoint.ends_with("/e/pb/bid"));
    }

    #[test]
    fn config_accepts_default_and_custom_openrtb_endpoints() {
        let default = LegacyApsProviderConfig {
            account_id: "example-account".to_string(),
            ..Default::default()
        };
        let custom: LegacyApsProviderConfig = serde_json::from_value(json!({
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
            let parsed: LegacyApsProviderConfig = serde_json::from_value(json!({
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
        assert!(
            serde_json::from_value::<LegacyApsProviderConfig>(json!({"account_id": "   "}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<LegacyApsProviderConfig>(
                json!({"account_id": "x".repeat(MAX_ACCOUNT_ID_BYTES + 1)})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<LegacyApsProviderConfig>(json!({
                "account_id": "one",
                "pub_id": "two"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ApsConfig>(json!({
                "enabled": true,
                "rendering_mode": "unsupported"
            }))
            .is_err(),
            "should reject an unknown APS rendering mode"
        );
        for endpoint in [
            "http://aps.example/e/pb/bid",
            "https://",
            "https://user:password@aps.example/e/pb/bid",
        ] {
            let parsed: LegacyApsProviderConfig = serde_json::from_value(json!({
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
            let parsed: LegacyApsProviderConfig =
                serde_json::from_value(value).expect("should deserialize before validation");
            assert!(
                parsed.validate().is_err(),
                "should reject unsafe or incomplete inventory identity override"
            );
        }
    }

    #[test]
    fn inventory_identity_override_rewrites_site_and_preserves_page_path() {
        let config: LegacyApsProviderConfig = serde_json::from_value(json!({
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
            transport_timeout_ms: 321,
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
        let auction_request = canonical_parity_auction_request();
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
            transport_timeout_ms: 321,
            provider_responses: None,
            services: &services,
        };

        let openrtb = provider.build_openrtb_request(&auction_request, &context);
        let serialized = serde_json::to_value(&openrtb).expect("should serialize request");

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
        assert!(
            serialized["user"]["ext"].get("ConsentSettings").is_none(),
            "should omit PBS-only Google Additional Consent placement"
        );
        assert!(
            serialized["imp"][0].get("tagid").is_none(),
            "should ignore shared trustedServer bidder parameters"
        );
        assert_eq!(
            serde_json::to_string(&openrtb).expect("should serialize APS request"),
            r#"{"id":"fictional-auction","imp":[{"id":"fictional-slot","banner":{"format":[{"w":300,"h":250},{"w":728,"h":90}],"w":300,"h":250,"topframe":0},"bidfloor":1.0,"bidfloorcur":"USD","secure":1}],"site":{"domain":"publisher.example","page":"https://publisher.example/article","publisher":{"domain":"publisher.example"}},"device":{"geo":{"type":2,"country":"US","region":"CA","metro":"501","city":"Example City"},"dnt":1,"ua":"Fictional Browser","ip":"192.0.2.10","language":"en"},"user":{"id":"fictional-user","consent":"fictional-tcf","ext":{"consent":"fictional-tcf","eids":[{"source":"identity.example","uids":[{"atype":1,"id":"fictional-uid"}]}]}},"tmax":321,"cur":["USD"],"regs":{"gdpr":1,"us_privacy":"1YNN","gpp":"fictional-gpp","gpp_sid":[2,6],"ext":{"gdpr":1,"gpp":"fictional-gpp","gpp_sid":[2,6],"us_privacy":"1YNN"}},"ext":{"account":"example-account-id","sdk":{"source":"prebid","version":"2.2.0"}}}"#,
            "should preserve the complete APS wire shape without a signing extension"
        );
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
                transport_timeout_ms: 321,
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
            &json!({"cur": "USD", "seatbid": [{"seat": "fictional-upstream-seat", "bid": [bid("fictional-selected-bid-id", 1.23, "iframe")]}], "ext": {"userSyncs": []}}),
            12,
            &request(),
        );
        assert_eq!(response.bids.len(), 1);
        let parsed = &response.bids[0];
        assert_eq!(parsed.bidder, "aps");
        assert!(
            parsed.returned_seat.is_none(),
            "legacy APS parsing must not attach planned telemetry identity"
        );
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
                drop_count(&response, AuctionDropReason::InvalidProviderResponse),
                1
            );
        }
    }

    #[test]
    fn upstream_bid_ids_are_required_bounded_control_free_and_response_unique() {
        let provider = ApsAuctionProvider::new(config());
        let mut missing = bid("missing", 1.0, "iframe");
        missing
            .as_object_mut()
            .expect("should build an object bid")
            .remove("id");
        let empty = bid("", 1.1, "iframe");
        let oversized = bid(&format!("{}x", "é".repeat(32)), 1.2, "iframe");
        let control = bid("control\u{0000}id", 1.3, "iframe");
        let duplicate_low = bid("duplicate", 1.4, "iframe");
        let duplicate_high = bid("duplicate", 9.0, "iframe");
        let valid_boundary = bid(&"é".repeat(32), 2.0, "iframe");

        let response = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": [
                missing,
                empty,
                oversized,
                control,
                duplicate_low,
                duplicate_high,
                valid_boundary
            ]}]}),
            12,
            &request(),
        );

        assert_eq!(response.status, BidStatus::Success);
        assert_eq!(response.bids.len(), 1);
        assert_eq!(
            response.bids[0].bid_id.as_deref(),
            Some("é".repeat(32).as_str())
        );
        assert_eq!(
            drop_count(&response, AuctionDropReason::MissingUpstreamBidId),
            2
        );
        assert_eq!(
            drop_count(&response, AuctionDropReason::UpstreamBidIdTooLarge),
            1
        );
        assert_eq!(
            drop_count(&response, AuctionDropReason::InvalidUpstreamBidId),
            1
        );
        assert_eq!(
            drop_count(&response, AuctionDropReason::DuplicateUpstreamBidId),
            2
        );
    }

    #[test]
    fn bid_validation_is_typed_and_isolated_from_a_valid_sibling() {
        let provider = ApsAuctionProvider::new(config());
        let mut unknown_imp = bid("unknown-imp", 1.0, "iframe");
        unknown_imp["impid"] = json!("not-requested");
        let negative_price = bid("negative-price", -0.1, "iframe");
        let mut wrong_price = bid("wrong-price", 1.0, "iframe");
        wrong_price["price"] = json!("1.0");
        let mut zero_width = bid("zero-width", 1.0, "iframe");
        zero_width["w"] = json!(0);
        let mut over_height = bid("over-height", 1.0, "iframe");
        over_height["h"] = json!(4097);
        let mut unmatched_size = bid("unmatched-size", 1.0, "iframe");
        unmatched_size["w"] = json!(728);
        unmatched_size["h"] = json!(90);
        let mut missing_url = bid("missing-url", 1.0, "iframe");
        missing_url["ext"]
            .as_object_mut()
            .expect("should build a bid extension")
            .remove("creativeurl");
        let mut invalid_url = bid("invalid-url", 1.0, "iframe");
        invalid_url["ext"]["creativeurl"] = json!("http://creative.example/render");
        let invalid_tag = bid("invalid-tag", 1.0, "video");
        let mut invalid_creative_id = bid("invalid-creative-id", 1.0, "iframe");
        invalid_creative_id["crid"] = json!(42);
        let mut unsupported_media = bid("unsupported-media", 1.0, "iframe");
        unsupported_media["mtype"] = json!(2);
        let valid = bid("valid-sibling", 2.0, "iframe");

        let response = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": [
                "malformed",
                unknown_imp,
                negative_price,
                wrong_price,
                zero_width,
                over_height,
                unmatched_size,
                missing_url,
                invalid_url,
                invalid_tag,
                invalid_creative_id,
                unsupported_media,
                valid
            ]}]}),
            12,
            &request(),
        );

        assert_eq!(response.bids.len(), 1);
        assert_eq!(response.bids[0].bid_id.as_deref(), Some("valid-sibling"));
        for reason in [
            AuctionDropReason::MalformedBid,
            AuctionDropReason::UnknownImpression,
            AuctionDropReason::DimensionsOutOfRange,
            AuctionDropReason::MissingCreativeUrl,
            AuctionDropReason::InvalidCreativeUrl,
            AuctionDropReason::InvalidTagType,
            AuctionDropReason::InvalidCreativeId,
            AuctionDropReason::UnsupportedMediaType,
        ] {
            assert_eq!(drop_count(&response, reason), 1, "should report {reason:?}");
        }
        assert_eq!(drop_count(&response, AuctionDropReason::InvalidPrice), 2);
        assert_eq!(
            drop_count(&response, AuctionDropReason::InvalidDimensions),
            2
        );
    }

    #[test]
    fn dimensions_accept_exact_requested_membership_at_contract_boundaries() {
        let provider = ApsAuctionProvider::new(config());
        let mut auction_request = request();
        auction_request.slots = vec![
            AdSlot {
                id: "minimum-slot".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 1,
                    height: 1,
                }],
                floor_price: None,
                targeting: HashMap::new(),
                bidders: HashMap::new(),
            },
            AdSlot {
                id: "maximum-slot".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 4096,
                    height: 4096,
                }],
                floor_price: None,
                targeting: HashMap::new(),
                bidders: HashMap::new(),
            },
        ];
        let mut minimum = bid("minimum", 1.0, "iframe");
        minimum["impid"] = json!("minimum-slot");
        minimum["w"] = json!(1);
        minimum["h"] = json!(1);
        let mut maximum = bid("maximum", 1.0, "iframe");
        maximum["impid"] = json!("maximum-slot");
        maximum["w"] = json!(4096);
        maximum["h"] = json!(4096);

        let response = provider.parse_aps_response(
            &json!({"seatbid": [{"bid": [minimum, maximum]}]}),
            12,
            &auction_request,
        );

        assert_eq!(response.status, BidStatus::Success);
        assert_eq!(response.bids.len(), 2);
        assert_eq!(response.bids[0].slot_id, "minimum-slot");
        assert_eq!(response.bids[1].slot_id, "maximum-slot");
        assert_eq!(response.metadata["dropped_bid_count"], 0);
    }

    #[test]
    fn contextual_currency_and_nonfinite_json_are_invalid_provider_responses() {
        let provider = ApsAuctionProvider::new(config());
        for value in [
            json!({"contextual": {"slots": []}, "seatbid": [{"bid": [bid("valid", 1.0, "iframe")]}]}),
            json!({"cur": "EUR", "seatbid": [{"bid": [bid("eur", 1.0, "iframe")]}]}),
        ] {
            let response = provider.parse_aps_response(&value, 12, &request());
            assert_eq!(response.status, BidStatus::Error);
            assert_eq!(
                drop_count(&response, AuctionDropReason::InvalidProviderResponse),
                1
            );
        }

        let body = br#"{"seatbid":[{"bid":[{"id":"overflow","impid":"fictional-slot","price":1e400,"w":300,"h":250,"ext":{"creativeurl":"https://creative.example/render","tagtype":"iframe"}}]}]}"#;
        let response = futures::executor::block_on(
            provider.parse_response_inner(
                PlatformResponse::new(
                    edgezero_core::http::response_builder()
                        .status(StatusCode::OK)
                        .body(EdgeBody::from(body.to_vec()))
                        .expect("should build nonfinite APS response"),
                ),
                12,
                Some(&request()),
                None,
                false,
            ),
        )
        .expect("should reject nonfinite APS JSON safely");
        assert_eq!(response.status, BidStatus::Error);
        assert_eq!(
            drop_count(&response, AuctionDropReason::InvalidProviderResponse),
            1
        );
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
            transport_timeout_ms: 321,
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
            drop_count(&malformed, AuctionDropReason::InvalidProviderResponse),
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
            drop_count(&response, AuctionDropReason::InvalidProviderResponse),
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
    fn unsupported_currency_is_an_invalid_provider_response() {
        let provider = ApsAuctionProvider::new(config());
        let response = provider.parse_aps_response(
            &json!({"cur": "EUR", "seatbid": [{"bid": [bid("eur-bid", 1.0, "iframe")]}]}),
            12,
            &request(),
        );
        assert_eq!(response.status, BidStatus::Error);
        assert_eq!(
            drop_count(&response, AuctionDropReason::InvalidProviderResponse),
            1
        );
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
        assert_eq!(drop_count(&response, AuctionDropReason::MalformedBid), 1);
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
            drop_count(&response, AuctionDropReason::MissingCreativeUrl),
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
        let integration = ApsRendererIntegration {
            rendering_mode: ApsRenderingMode::TrustedServer,
        };
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
        assert_eq!(registration.head_injectors.len(), 1);
        let document_state = IntegrationDocumentState::default();
        let context = IntegrationHtmlContext {
            request_host: "publisher.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };
        assert!(
            registration.head_injectors[0]
                .head_inserts(&context)
                .is_empty(),
            "should not inject a native-mode head marker by default"
        );
        assert!(
            registration.head_injectors[0]
                .tsjs_script_tag_attributes()
                .is_empty(),
            "should not authorize native rendering by default"
        );
        assert!(registration.js_disabled);
    }

    #[test]
    fn publisher_native_config_registers_runner_mode_without_renderer_route() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                APS_INTEGRATION_ID,
                &json!({
                    "enabled": true,
                    "account_id": "example-account",
                    "rendering_mode": "publisher_native"
                }),
            )
            .expect("should insert native APS config");

        let registration = register(&settings)
            .expect("should register APS")
            .expect("should return enabled registration");
        assert!(
            registration.proxies.is_empty(),
            "should not register the static renderer"
        );
        assert_eq!(registration.head_injectors.len(), 1);

        let integration = ApsRendererIntegration {
            rendering_mode: ApsRenderingMode::PublisherNative,
        };
        assert!(
            integration.routes().is_empty(),
            "should expose no renderer route"
        );
        let document_state = IntegrationDocumentState::default();
        let context = IntegrationHtmlContext {
            request_host: "publisher.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };
        assert!(
            integration.head_inserts(&context).is_empty(),
            "should not inject a forgeable native-mode marker"
        );
        assert_eq!(
            integration.tsjs_script_tag_attributes(),
            vec![("data-ts-aps-rendering-mode", "publisher_native")],
            "should authorize native mode on the publisher bundle tag"
        );
    }

    #[test]
    fn publisher_native_script_creatives_remain_available_for_controlled_validation() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                APS_INTEGRATION_ID,
                &json!({
                    "enabled": true,
                    "account_id": "example-account",
                    "allow_script_creatives": true,
                    "rendering_mode": "publisher_native"
                }),
            )
            .expect("should insert native APS script config");

        let providers = register_providers(&settings).expect("should register APS provider");

        assert_eq!(
            providers.len(),
            1,
            "should retain the controlled experiment"
        );
    }

    #[test]
    fn config_without_enabled_does_not_register_provider_or_renderer() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                APS_INTEGRATION_ID,
                &json!({"account_id": "example-account"}),
            )
            .expect("should insert default-disabled APS config");

        assert!(
            register(&settings)
                .expect("should evaluate renderer registration")
                .is_none(),
            "omitted enabled should not register the renderer route"
        );
        assert!(
            register_providers(&settings)
                .expect("should evaluate provider registration")
                .is_empty(),
            "omitted enabled should not register the auction provider"
        );
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
        assert!(
            APS_RENDERER_DOCUMENT
                .contains("html,body{margin:0;padding:0}body>iframe{display:block}")
        );
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

    #[test]
    fn coordinated_cutover_routes_are_reserved_with_exact_local_method_policy() {
        let enabled = ApsV1Integration { enabled: true };
        let disabled = ApsV1Integration { enabled: false };
        let settings = create_test_settings();
        let services = noop_services();

        for path in [APS_RENDERER_V1_ROUTE, APS_RUNNER_ROUTE] {
            let disabled_get = http::Request::builder()
                .method(Method::GET)
                .uri(path)
                .body(EdgeBody::empty())
                .expect("should build disabled APS request");
            let response =
                futures::executor::block_on(disabled.handle(&settings, &services, disabled_get))
                    .expect("disabled APS family should answer locally");
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

            for method in [
                Method::POST,
                Method::HEAD,
                Method::OPTIONS,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::TRACE,
                Method::CONNECT,
                Method::from_bytes(b"PROPFIND").expect("PROPFIND should be a valid method"),
            ] {
                let request = http::Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .body(EdgeBody::empty())
                    .expect("should build APS method rejection");
                let response =
                    futures::executor::block_on(enabled.handle(&settings, &services, request))
                        .expect("reserved APS family should reject unsupported methods locally");
                assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(response.headers()[header::ALLOW], "GET");
                assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
                assert_eq!(response.headers().len(), 2, "method={method} path={path}");
                assert!(
                    response
                        .into_body()
                        .into_bytes()
                        .unwrap_or_default()
                        .is_empty()
                );
            }
        }

        for path in [
            "/integrations/aps/renderer/v2",
            "/integrations/aps/runner/v1.js",
            "/integrations/aps/renderer/v1/extra",
            "/integrations/aps/not-a-route",
        ] {
            let request = http::Request::builder()
                .method(Method::GET)
                .uri(path)
                .body(EdgeBody::empty())
                .expect("should build unknown APS family request");
            let response =
                futures::executor::block_on(enabled.handle(&settings, &services, request))
                    .expect("unknown APS family path should answer locally");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "path={path}");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
    }

    #[test]
    fn aps_family_classifier_has_an_exact_segment_boundary() {
        assert!(is_aps_family_path("/integrations/aps"));
        assert!(is_aps_family_path("/integrations/aps/renderer/v1"));
        assert!(!is_aps_family_path("/integrations/apsx"));
        assert!(!is_aps_family_path("/integrations/ap"));
    }

    #[test]
    fn coordinated_cutover_renderer_has_exact_immutable_embedding_policy() {
        let integration = ApsV1Integration { enabled: true };
        let request = http::Request::builder()
            .method(Method::GET)
            .uri(APS_RENDERER_V1_ROUTE)
            .body(EdgeBody::empty())
            .expect("should build versioned renderer request");
        let response = futures::executor::block_on(integration.handle(
            &create_test_settings(),
            &noop_services(),
            request,
        ))
        .expect("versioned renderer should be served");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            APS_RENDERER_V1_CSP
        );
        assert!(!response.headers().contains_key("x-frame-options"));
        assert!(!APS_RENDERER_V1_CSP.contains("frame-ancestors"));
        assert_eq!(
            APS_RENDERER_SANDBOX,
            "allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation"
        );

        let body = futures::executor::block_on(
            response
                .into_body()
                .into_bytes_bounded(APS_RUNNER_MAX_RESPONSE_BYTES),
        )
        .expect("renderer body should stay within the runner cap");
        assert_eq!(body.as_ref(), APS_RENDERER_V1_DOCUMENT.as_bytes());
        assert!(APS_RENDERER_V1_DOCUMENT.contains("/integrations/aps/runner.js"));
        assert!(!APS_RENDERER_V1_DOCUMENT.contains("client.aps.amazon-adsystem.com"));
    }

    fn raw_runner_response(
        body: impl Into<Vec<u8>>,
        content_type: ProxyHeaderEvidenceV1,
        content_encoding: ProxyHeaderEvidenceV1,
        content_length: ProxyHeaderEvidenceV1,
    ) -> RawProxyResponseV1 {
        RawProxyResponseV1 {
            evidence: ProxyResponseEvidenceV1 {
                status: 200,
                content_type,
                content_encoding,
                content_length,
            },
            body: body.into(),
        }
    }

    fn request_runner(
        stub: &Arc<StubHttpClient>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let services = build_services_with_http_client(
            Arc::clone(stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let request = http::Request::builder()
            .method(Method::GET)
            .uri(APS_RUNNER_ROUTE)
            .body(EdgeBody::empty())
            .expect("should build APS runner request");
        futures::executor::block_on(ApsV1Integration { enabled: true }.handle(
            &create_test_settings(),
            &services,
            request,
        ))
    }

    #[test]
    fn coordinated_cutover_runner_proxies_exact_valid_identity_bytes() {
        let body = b"window.fictionalApsRunner = true;".to_vec();
        let stub = Arc::new(StubHttpClient::new());
        stub.push_raw_proxy_response(raw_runner_response(
            body.clone(),
            ProxyHeaderEvidenceV1::one("application/javascript; charset=utf-8"),
            ProxyHeaderEvidenceV1::absent(),
            ProxyHeaderEvidenceV1::one(body.len().to_string()),
        ));

        let response = request_runner(&stub).expect("valid runner should be proxied");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/javascript; charset=utf-8"
        );
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(
            response.headers()["cross-origin-resource-policy"],
            "cross-origin"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert_eq!(response.headers().len(), 5);
        let returned = futures::executor::block_on(
            response
                .into_body()
                .into_bytes_bounded(APS_RUNNER_MAX_RESPONSE_BYTES),
        )
        .expect("valid runner body should remain bounded");
        assert_eq!(returned.as_ref(), body.as_slice());

        assert_eq!(stub.recorded_backend_names(), vec!["stub-backend"]);
        assert_eq!(stub.recorded_request_uris(), vec![APS_RUNNER_UPSTREAM_URL]);
        assert_eq!(stub.recorded_request_methods(), vec!["GET"]);
        assert_eq!(
            stub.recorded_request_headers(),
            vec![vec![(
                "accept-encoding".to_string(),
                "identity".to_string()
            )]]
        );
        assert_eq!(stub.recorded_request_bodies(), vec![Vec::<u8>::new()]);
        assert_eq!(APS_RUNNER_TOTAL_TIMEOUT, Duration::from_secs(5));
        assert_eq!(
            stub.recorded_raw_proxy_policies(),
            vec![RawProxyPolicyV1 {
                total_timeout: APS_RUNNER_TOTAL_TIMEOUT,
                first_byte_timeout: Duration::from_secs(4),
                blocking_read_timeout: Duration::from_millis(250),
                max_response_bytes: APS_RUNNER_MAX_RESPONSE_BYTES,
            }]
        );
    }

    #[test]
    fn coordinated_cutover_runner_rejects_ambiguous_or_invalid_upstream_evidence() {
        let valid_body = b"window.fictionalApsRunner = true;".to_vec();
        let valid_length = valid_body.len().to_string();
        let cases = [
            (
                "redirect",
                RawProxyResponseV1 {
                    evidence: ProxyResponseEvidenceV1 {
                        status: 302,
                        content_type: ProxyHeaderEvidenceV1::one("application/javascript"),
                        content_encoding: ProxyHeaderEvidenceV1::absent(),
                        content_length: ProxyHeaderEvidenceV1::one(valid_length.clone()),
                    },
                    body: valid_body.clone(),
                },
            ),
            (
                "missing content type",
                raw_runner_response(
                    valid_body.clone(),
                    ProxyHeaderEvidenceV1::absent(),
                    ProxyHeaderEvidenceV1::absent(),
                    ProxyHeaderEvidenceV1::one(valid_length.clone()),
                ),
            ),
            (
                "duplicate content type",
                raw_runner_response(
                    valid_body.clone(),
                    ProxyHeaderEvidenceV1::Occurrences(vec![
                        b"application/javascript".to_vec(),
                        b"text/javascript".to_vec(),
                    ]),
                    ProxyHeaderEvidenceV1::absent(),
                    ProxyHeaderEvidenceV1::one(valid_length.clone()),
                ),
            ),
            (
                "combined content type list",
                raw_runner_response(
                    valid_body.clone(),
                    ProxyHeaderEvidenceV1::Combined(
                        b"application/javascript, text/javascript".to_vec(),
                    ),
                    ProxyHeaderEvidenceV1::absent(),
                    ProxyHeaderEvidenceV1::one(valid_length.clone()),
                ),
            ),
            (
                "wrong charset",
                raw_runner_response(
                    valid_body.clone(),
                    ProxyHeaderEvidenceV1::one("application/javascript; charset=iso-8859-1"),
                    ProxyHeaderEvidenceV1::absent(),
                    ProxyHeaderEvidenceV1::one(valid_length.clone()),
                ),
            ),
            (
                "encoded body",
                raw_runner_response(
                    valid_body.clone(),
                    ProxyHeaderEvidenceV1::one("application/javascript"),
                    ProxyHeaderEvidenceV1::one("gzip"),
                    ProxyHeaderEvidenceV1::one(valid_length.clone()),
                ),
            ),
            (
                "unavailable encoding evidence",
                raw_runner_response(
                    valid_body.clone(),
                    ProxyHeaderEvidenceV1::one("application/javascript"),
                    ProxyHeaderEvidenceV1::Unavailable,
                    ProxyHeaderEvidenceV1::one(valid_length.clone()),
                ),
            ),
            (
                "leading-zero length",
                raw_runner_response(
                    valid_body.clone(),
                    ProxyHeaderEvidenceV1::one("application/javascript"),
                    ProxyHeaderEvidenceV1::absent(),
                    ProxyHeaderEvidenceV1::one(format!("0{valid_length}")),
                ),
            ),
            (
                "mismatched length",
                raw_runner_response(
                    valid_body.clone(),
                    ProxyHeaderEvidenceV1::one("application/javascript"),
                    ProxyHeaderEvidenceV1::absent(),
                    ProxyHeaderEvidenceV1::one("1"),
                ),
            ),
            (
                "invalid utf-8",
                raw_runner_response(
                    vec![0xff],
                    ProxyHeaderEvidenceV1::one("application/javascript"),
                    ProxyHeaderEvidenceV1::absent(),
                    ProxyHeaderEvidenceV1::one("1"),
                ),
            ),
        ];

        for (name, response) in cases {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_raw_proxy_response(response);
            let response = request_runner(&stub).expect("invalid runner should fail locally");
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY, "case={name}");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers().len(), 1, "case={name}");
            let body = futures::executor::block_on(
                response
                    .into_body()
                    .into_bytes_bounded(APS_RUNNER_MAX_RESPONSE_BYTES),
            )
            .expect("local failure body should be bounded");
            assert!(body.is_empty(), "case={name}");
        }
    }

    #[test]
    fn coordinated_cutover_runner_header_grammars_are_closed_and_complete() {
        for content_type in [
            ProxyHeaderEvidenceV1::one("application/javascript"),
            ProxyHeaderEvidenceV1::one("text/javascript"),
            ProxyHeaderEvidenceV1::one(" Application/JavaScript ; Charset = UTF-8 "),
            ProxyHeaderEvidenceV1::Combined(b"text/javascript; charset=utf-8".to_vec()),
        ] {
            assert!(
                ApsV1Integration::validate_runner_content_type(&content_type).is_ok(),
                "accepted content type: {content_type:?}"
            );
        }
        for content_type in [
            ProxyHeaderEvidenceV1::Unavailable,
            ProxyHeaderEvidenceV1::absent(),
            ProxyHeaderEvidenceV1::one("application/ecmascript"),
            ProxyHeaderEvidenceV1::one("application/javascript; charset=\"utf-8\""),
            ProxyHeaderEvidenceV1::one("application/javascript; charset=utf-8; level=1"),
            ProxyHeaderEvidenceV1::one("application/javascript; boundary=x"),
            ProxyHeaderEvidenceV1::one("application/javascript,"),
            ProxyHeaderEvidenceV1::one("application/javascript\u{a0}"),
        ] {
            assert!(
                ApsV1Integration::validate_runner_content_type(&content_type).is_err(),
                "rejected content type: {content_type:?}"
            );
        }

        for encoding in [
            ProxyHeaderEvidenceV1::absent(),
            ProxyHeaderEvidenceV1::one("identity"),
            ProxyHeaderEvidenceV1::Combined(b" IDENTITY\t".to_vec()),
        ] {
            assert!(
                ApsV1Integration::validate_runner_content_encoding(&encoding).is_ok(),
                "accepted encoding: {encoding:?}"
            );
        }
        for encoding in [
            ProxyHeaderEvidenceV1::Unavailable,
            ProxyHeaderEvidenceV1::one(""),
            ProxyHeaderEvidenceV1::one("gzip"),
            ProxyHeaderEvidenceV1::one("identity, identity"),
            ProxyHeaderEvidenceV1::Occurrences(vec![b"identity".to_vec(), b"identity".to_vec()]),
        ] {
            assert!(
                ApsV1Integration::validate_runner_content_encoding(&encoding).is_err(),
                "rejected encoding: {encoding:?}"
            );
        }

        assert_eq!(
            ApsV1Integration::validate_runner_content_length(&ProxyHeaderEvidenceV1::absent()),
            Ok(None)
        );
        assert_eq!(
            ApsV1Integration::validate_runner_content_length(&ProxyHeaderEvidenceV1::one("0")),
            Ok(Some(0))
        );
        assert_eq!(
            ApsV1Integration::validate_runner_content_length(&ProxyHeaderEvidenceV1::Combined(
                APS_RUNNER_MAX_RESPONSE_BYTES.to_string().into_bytes()
            )),
            Ok(Some(APS_RUNNER_MAX_RESPONSE_BYTES))
        );
        for length in [
            ProxyHeaderEvidenceV1::Unavailable,
            ProxyHeaderEvidenceV1::one(""),
            ProxyHeaderEvidenceV1::one("00"),
            ProxyHeaderEvidenceV1::one("01"),
            ProxyHeaderEvidenceV1::one("+1"),
            ProxyHeaderEvidenceV1::one(" 1"),
            ProxyHeaderEvidenceV1::one("1 "),
            ProxyHeaderEvidenceV1::one("1, 1"),
            ProxyHeaderEvidenceV1::one((APS_RUNNER_MAX_RESPONSE_BYTES + 1).to_string()),
            ProxyHeaderEvidenceV1::Occurrences(vec![b"1".to_vec(), b"1".to_vec()]),
        ] {
            assert!(
                ApsV1Integration::validate_runner_content_length(&length).is_err(),
                "rejected length: {length:?}"
            );
        }
    }

    #[test]
    fn coordinated_cutover_runner_accepts_exact_cap_and_rejects_one_byte_over() {
        let exact = raw_runner_response(
            vec![b' '; APS_RUNNER_MAX_RESPONSE_BYTES],
            ProxyHeaderEvidenceV1::one("application/javascript"),
            ProxyHeaderEvidenceV1::absent(),
            ProxyHeaderEvidenceV1::absent(),
        );
        assert!(ApsV1Integration::validate_runner_response(&exact).is_ok());

        let over = raw_runner_response(
            vec![b' '; APS_RUNNER_MAX_RESPONSE_BYTES + 1],
            ProxyHeaderEvidenceV1::one("application/javascript"),
            ProxyHeaderEvidenceV1::absent(),
            ProxyHeaderEvidenceV1::absent(),
        );
        assert_eq!(
            ApsV1Integration::validate_runner_response(&over),
            Err("body_overflow")
        );
    }

    #[test]
    fn coordinated_cutover_runner_transport_failure_is_empty_and_non_leaking() {
        let stub = Arc::new(StubHttpClient::new());
        let response = request_runner(&stub).expect("transport failure should answer locally");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers().len(), 1);
        let body = futures::executor::block_on(
            response
                .into_body()
                .into_bytes_bounded(APS_RUNNER_MAX_RESPONSE_BYTES),
        )
        .expect("local failure body should be bounded");
        assert!(body.is_empty());
    }
}
