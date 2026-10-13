//! ID5 server-side ID fetch for the `/auction` endpoint.
//!
//! When enabled, Trusted Server requests an ID5 ID directly from ID5's
//! server-to-server web endpoint (`POST {endpoint}/gs/v2/{partner}.json`) while
//! handling `/auction`, instead of waiting for a browser SDK to obtain one.
//!
//! The fetched ID is:
//!
//! - added to the current auction's `user.eids` in [`Id5FetchMode::Blocking`],
//!   or from the next auction onward in [`Id5FetchMode::Parallel`];
//! - written to the server-owned `HttpOnly` [`COOKIE_TS_EIDS_S`] cookie on the
//!   `/auction` response, so later auctions read it without another fetch;
//! - handed to EC response finalization as a request-local client EID, so the
//!   existing partner-registry ingestion persists it to KV when
//!   `id5-sync.com` is a configured `[[ec.partners]]` source.
//!
//! ID5's `signature` is kept in [`COOKIE_TS_ID5_SIGNATURE`] and echoed back on
//! the next fetch, as ID5 requires. [`COOKIE_TS_ID5_FRESH`] expires after ID5's
//! `cache_control.max_age_sec` and marks when the next refresh is due.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cookie::CookieJar;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{HeaderValue, Method, Request, Response, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use validator::Validate;

use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::{ConsentContext, ConsentSource};
use crate::constants::{
    COOKIE_TS_DEMO_CONSENT, COOKIE_TS_EIDS_S, COOKIE_TS_ID5_FRESH, COOKIE_TS_ID5_SIGNATURE,
};
use crate::cookies::extract_cookie_value;
use crate::ec::prebid_eids::{is_valid_eid_uid, parse_prebid_eids_cookie};
use crate::error::TrustedServerError;
use crate::integrations::{
    IntegrationHeadInjector, IntegrationHtmlContext, IntegrationRegistration,
    collect_response_bounded, ensure_integration_backend_with_timeout,
};
use crate::openrtb::{Eid, Uid};
use crate::platform::{PlatformHttpRequest, PlatformPendingRequest, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

/// Integration ID used for `[integrations.id5]` and backend naming.
pub const ID5_INTEGRATION_ID: &str = "id5";
/// `OpenRTB` EID source for the ID5 universal ID.
pub const ID5_SOURCE: &str = "id5-sync.com";

const ID5_ORIGIN: &str = "trusted-server";
const ID5_VERSION: &str = concat!("trusted-server-", env!("CARGO_PKG_VERSION"));
const MAX_ID5_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_SIGNATURE_BYTES: usize = 1024;
const MIN_REFRESH_SECONDS: u32 = 300;
const MAX_REFRESH_SECONDS: u32 = 86_400;

/// When the ID5 fetch runs relative to bidder dispatch.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Id5FetchMode {
    /// Start the fetch with the auction and collect it before the `/auction`
    /// response is returned. Bidders are not delayed; the ID reaches auctions
    /// from the next one onward through the cookie.
    #[default]
    Parallel,
    /// Fetch before bidders are called, bounded by `timeout_ms`. The current
    /// auction includes the ID when ID5 answers in time.
    Blocking,
}

/// Configuration for `[integrations.id5]`.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Id5Config {
    /// Enables the server-side ID5 fetch on `/auction`.
    #[serde(default)]
    pub enabled: bool,
    /// ID5 partner number. `173` is ID5's public test partner.
    #[validate(range(min = 1))]
    pub partner: u32,
    /// ID5 API origin. The fetch path is `/gs/v2/{partner}.json`.
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,
    /// Whether bidders wait for ID5.
    #[serde(default)]
    pub mode: Id5FetchMode,
    /// First-byte timeout for the ID5 request, in milliseconds.
    #[serde(default = "default_timeout_ms")]
    #[validate(range(min = 50, max = 5000))]
    pub timeout_ms: u32,
    /// Provider name assigned by ID5 when calling on a publisher's behalf.
    #[serde(default)]
    pub provider: Option<String>,
    /// Refresh interval used when ID5's response carries no `cache_control`.
    #[serde(default = "default_refresh_seconds")]
    #[validate(range(min = 300, max = 86400))]
    pub refresh_seconds: u32,
    /// Demo only: injects a Trusted Server consent banner on every page until
    /// the visitor answers, and treats an accepted browser as consented
    /// regardless of location or CMP signals. Never enable on production traffic.
    #[serde(default)]
    pub demo_consent_banner: bool,
    /// Lifetime of the ID and signature cookies, in seconds.
    #[serde(default = "default_cookie_max_age_seconds")]
    #[validate(range(min = 3600, max = 2_592_000))]
    pub cookie_max_age_seconds: u32,
}

impl IntegrationConfig for Id5Config {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

const DEMO_CONSENT_GRANTED: &str = "granted";

/// Banner shown until the visitor answers. Built with DOM calls only, so it
/// works under CSPs that forbid `innerHTML` string assignment.
const DEMO_CONSENT_BANNER_SCRIPT: &str = r#"<script>(function(){var n="ts-demo-consent";if(document.cookie.split("; ").some(function(c){return c.indexOf(n+"=")===0})){return}function answer(v){document.cookie=n+"="+v+"; path=/; max-age=86400; secure; samesite=lax";location.reload()}function button(label,value,primary){var b=document.createElement("button");b.type="button";b.textContent=label;b.style.cssText="margin:0 8px 0 0;padding:8px 16px;border:0;border-radius:4px;cursor:pointer;font:inherit;"+(primary?"background:#2563eb;color:#fff":"background:#e5e7eb;color:#111");b.addEventListener("click",function(){answer(value)});return b}function show(){if(document.getElementById(n)){return}var d=document.createElement("div");d.id=n;d.setAttribute("role","dialog");d.setAttribute("aria-label","Trusted Server demo consent");d.style.cssText="position:fixed;left:16px;right:16px;bottom:16px;z-index:2147483647;max-width:560px;margin:0 auto;padding:16px;background:#111827;color:#fff;font:14px/1.45 system-ui,sans-serif;border-radius:8px;box-shadow:0 8px 32px rgba(0,0,0,.45)";var p=document.createElement("p");p.style.cssText="margin:0 0 12px";p.textContent="Trusted Server demo: allow identity (Edge Cookie and ID5) in this browser?";d.appendChild(p);d.appendChild(button("Accept","granted",true));d.appendChild(button("Decline","denied",false));document.body.appendChild(d)}if(document.body){show()}else{document.addEventListener("DOMContentLoaded",show)}})();</script>"#;

/// Registers the demo consent banner when `demo_consent_banner` is enabled.
///
/// The ID5 fetch itself runs from `/auction` and needs no registration.
///
/// # Errors
///
/// Returns an error when `[integrations.id5]` cannot be parsed.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = settings.integration_config::<Id5Config>(ID5_INTEGRATION_ID)? else {
        return Ok(None);
    };
    if !config.demo_consent_banner {
        return Ok(None);
    }
    log::warn!("ID5: demo consent banner enabled; never enable it on production traffic");
    Ok(Some(
        IntegrationRegistration::builder(ID5_INTEGRATION_ID)
            .with_head_injector(Arc::new(DemoConsentBanner))
            .without_js()
            .build(),
    ))
}

struct DemoConsentBanner;

impl IntegrationHeadInjector for DemoConsentBanner {
    fn integration_id(&self) -> &'static str {
        ID5_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        vec![DEMO_CONSENT_BANNER_SCRIPT.to_owned()]
    }
}

/// Treats a browser that accepted the demo banner as consented.
///
/// Applies only when `demo_consent_banner` is enabled and the request carries
/// `ts-demo-consent=granted`. The consent context is replaced with a
/// non-regulated one, so stale or restrictive CMP strings in the browser do not
/// block the Edge Cookie, the ID5 fetch or EID forwarding during the demo.
pub(crate) fn apply_demo_consent(
    settings: &Settings,
    jar: Option<&CookieJar>,
    consent: &mut ConsentContext,
) {
    let granted = jar
        .and_then(|jar| jar.get(COOKIE_TS_DEMO_CONSENT))
        .is_some_and(|cookie| cookie.value() == DEMO_CONSENT_GRANTED);
    if !granted {
        return;
    }
    let enabled = matches!(
        settings.integration_config::<Id5Config>(ID5_INTEGRATION_ID),
        Ok(Some(config)) if config.demo_consent_banner
    );
    if !enabled {
        return;
    }
    log::warn!("ID5 demo: banner accepted, treating this request as consented");
    *consent = ConsentContext {
        jurisdiction: Jurisdiction::NonRegulated,
        source: ConsentSource::PolicyDefault,
        ..ConsentContext::default()
    };
}

fn default_endpoint() -> String {
    "https://api.id5-sync.com".to_owned()
}

fn default_timeout_ms() -> u32 {
    500
}

fn default_refresh_seconds() -> u32 {
    7200
}

fn default_cookie_max_age_seconds() -> u32 {
    2_592_000
}

/// Request-local ID5 state for one `/auction` call.
pub(crate) struct Id5AuctionState {
    config: Id5Config,
    cookie_eid: Option<Eid>,
    signature: Option<String>,
    refresh_due: bool,
}

/// Inputs the ID5 request needs from the auction request.
pub(crate) struct Id5FetchInput<'a> {
    pub consent: &'a ConsentContext,
    pub page_url: &'a str,
    pub client_ip: Option<IpAddr>,
    pub user_agent: Option<&'a str>,
}

/// An ID5 request that has started or already finished.
pub(crate) enum Id5InFlight {
    Pending(PlatformPendingRequest),
    Done(Id5Outcome),
}

/// Result of one ID5 fetch.
#[derive(Debug)]
pub(crate) enum Id5Outcome {
    /// ID5 issued an ID.
    Issued {
        eid: Eid,
        signature: Option<String>,
        refresh_seconds: u32,
    },
    /// ID5 answered without consent to issue an ID (`universal_uid` is `"0"`).
    NoConsent,
    /// The request failed or the response was unusable. Nothing is written.
    Failed,
}

impl Id5AuctionState {
    /// Loads ID5 config and cookies for an `/auction` request.
    ///
    /// Returns `None` when the integration is disabled or misconfigured, or
    /// when identity use is not allowed for this request.
    pub(crate) fn load(
        settings: &Settings,
        req: &Request<EdgeBody>,
        identity_allowed: bool,
    ) -> Option<Self> {
        if !identity_allowed {
            return None;
        }
        let config = match settings.integration_config::<Id5Config>(ID5_INTEGRATION_ID) {
            Ok(Some(config)) => config,
            Ok(None) => return None,
            Err(err) => {
                log::warn!("ID5: configuration could not be loaded, skipping: {err:?}");
                return None;
            }
        };
        let cookie_eid = extract_cookie_value(req, COOKIE_TS_EIDS_S)
            .and_then(|value| server_cookie_id5_eid(&value));
        let signature = extract_cookie_value(req, COOKIE_TS_ID5_SIGNATURE)
            .filter(|value| is_valid_signature(value));
        let refresh_due = extract_cookie_value(req, COOKIE_TS_ID5_FRESH).is_none();
        Some(Self {
            config,
            cookie_eid,
            signature,
            refresh_due,
        })
    }

    /// The ID5 EID stored in the server cookie, if any.
    pub(crate) fn cookie_eid(&self) -> Option<&Eid> {
        self.cookie_eid.as_ref()
    }

    /// Whether this request should call ID5.
    ///
    /// Skips the call when the browser already supplied an ID5 EID, matching
    /// the ID5 Prebid Server module, or when the last fetch is still fresh.
    pub(crate) fn should_fetch(&self, client_eids: Option<&[Eid]>) -> bool {
        let browser_has_id5 =
            client_eids.is_some_and(|eids| eids.iter().any(|eid| eid.source == ID5_SOURCE));
        self.refresh_due && !browser_has_id5
    }

    /// Starts the ID5 request.
    ///
    /// In [`Id5FetchMode::Blocking`] this waits for the response. In
    /// [`Id5FetchMode::Parallel`] it returns the pending request for
    /// [`Self::finish`] to collect after the auction.
    pub(crate) async fn begin(
        &self,
        services: &RuntimeServices,
        input: &Id5FetchInput<'_>,
    ) -> Id5InFlight {
        let request = match self.build_request(services, input) {
            Ok(Some(request)) => request,
            Ok(None) => return Id5InFlight::Done(Id5Outcome::Failed),
            Err(err) => {
                log::warn!("ID5: request could not be built: {err:?}");
                return Id5InFlight::Done(Id5Outcome::Failed);
            }
        };
        match self.config.mode {
            Id5FetchMode::Blocking => match services.http_client().send(request).await {
                Ok(response) => Id5InFlight::Done(read_response(response.response).await),
                Err(err) => {
                    log::warn!("ID5: fetch failed: {err:?}");
                    Id5InFlight::Done(Id5Outcome::Failed)
                }
            },
            Id5FetchMode::Parallel => match services.http_client().send_async(request).await {
                Ok(pending) => Id5InFlight::Pending(pending),
                Err(err) => {
                    log::warn!("ID5: fetch could not start: {err:?}");
                    Id5InFlight::Done(Id5Outcome::Failed)
                }
            },
        }
    }

    /// Collects an in-flight ID5 request.
    pub(crate) async fn finish(services: &RuntimeServices, in_flight: Id5InFlight) -> Id5Outcome {
        match in_flight {
            Id5InFlight::Done(outcome) => outcome,
            Id5InFlight::Pending(pending) => match services.http_client().wait(pending).await {
                Ok(response) => read_response(response.response).await,
                Err(err) => {
                    log::warn!("ID5: fetch failed: {err:?}");
                    Id5Outcome::Failed
                }
            },
        }
    }

    /// Writes the cookies for a fetch outcome onto the `/auction` response.
    pub(crate) fn apply_cookies(
        &self,
        settings: &Settings,
        outcome: &Id5Outcome,
        response: &mut Response<EdgeBody>,
    ) {
        let domain = settings.publisher.ec_cookie_domain();
        let max_age = self.config.cookie_max_age_seconds;
        let cookies = match outcome {
            Id5Outcome::Issued {
                eid,
                signature,
                refresh_seconds,
            } => {
                let Some(value) = encode_server_cookie(eid) else {
                    log::warn!("ID5: EID could not be encoded for the server cookie");
                    return;
                };
                let mut cookies = vec![
                    format_cookie(COOKIE_TS_EIDS_S, &value, &domain, max_age),
                    format_cookie(COOKIE_TS_ID5_FRESH, "1", &domain, *refresh_seconds),
                ];
                if let Some(signature) = signature {
                    cookies.push(format_cookie(
                        COOKIE_TS_ID5_SIGNATURE,
                        signature,
                        &domain,
                        max_age,
                    ));
                }
                cookies
            }
            // ID5 withheld the ID, so drop any stored copy and keep the
            // refresh marker so the next auction does not retry immediately.
            Id5Outcome::NoConsent => vec![
                format_cookie(COOKIE_TS_EIDS_S, "", &domain, 0),
                format_cookie(COOKIE_TS_ID5_SIGNATURE, "", &domain, 0),
                format_cookie(
                    COOKIE_TS_ID5_FRESH,
                    "1",
                    &domain,
                    self.config.refresh_seconds,
                ),
            ],
            Id5Outcome::Failed => return,
        };
        for cookie in cookies {
            match HeaderValue::from_str(&cookie) {
                Ok(value) => {
                    response.headers_mut().append(header::SET_COOKIE, value);
                }
                Err(_) => log::warn!("ID5: skipping invalid Set-Cookie value"),
            }
        }
    }

    fn build_request(
        &self,
        services: &RuntimeServices,
        input: &Id5FetchInput<'_>,
    ) -> Result<Option<PlatformHttpRequest>, Report<TrustedServerError>> {
        let Some(body) = build_request_body(
            &self.config,
            input,
            self.signature.as_deref(),
            crate::ec::checked_current_timestamp(),
        ) else {
            return Ok(None);
        };
        let endpoint = self.config.endpoint.trim_end_matches('/');
        let url = format!("{endpoint}/gs/v2/{}.json", self.config.partner);
        let backend_name = ensure_integration_backend_with_timeout(
            services,
            endpoint,
            ID5_INTEGRATION_ID,
            Duration::from_millis(u64::from(self.config.timeout_ms)),
        )?;
        let body_bytes = serde_json::to_vec(&body)
            .change_context(id5_error("Failed to serialize ID5 request body"))?;
        let request = http::Request::builder()
            .method(Method::POST)
            .uri(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .body(EdgeBody::from(body_bytes))
            .change_context(id5_error("Failed to build ID5 request"))?;
        Ok(Some(PlatformHttpRequest::new(request, backend_name)))
    }
}

fn id5_error(message: &str) -> TrustedServerError {
    TrustedServerError::Integration {
        integration: ID5_INTEGRATION_ID.to_owned(),
        message: message.to_owned(),
    }
}

/// Builds the JSON body for ID5's server-side web endpoint.
///
/// Returns `None` when a field ID5 requires is unavailable: an IPv4 client
/// address, a user agent, or a usable clock. ID5's `ip` field rejects IPv6,
/// so IPv6 clients are skipped rather than sent a placeholder address.
fn build_request_body(
    config: &Id5Config,
    input: &Id5FetchInput<'_>,
    signature: Option<&str>,
    now: Option<u64>,
) -> Option<JsonValue> {
    let ip = match input.client_ip {
        Some(IpAddr::V4(ip)) => ip,
        Some(IpAddr::V6(_)) => {
            log::debug!("ID5: skipping fetch for an IPv6 client");
            return None;
        }
        None => {
            log::debug!("ID5: skipping fetch without a client IP");
            return None;
        }
    };
    let user_agent = input.user_agent.filter(|ua| !ua.is_empty())?;
    let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(i64::try_from(now?).ok()?, 0)?
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let consent = input.consent;
    let mut body = json!({
        "partner": config.partner,
        "v": ID5_VERSION,
        "o": ID5_ORIGIN,
        "ts": ts,
        "u": input.page_url,
        "top": 1,
        "gdpr": u8::from(consent.gdpr_applies),
        "ip": ip.to_string(),
        "ua": user_agent,
    });
    let object = body.as_object_mut()?;
    if consent.gdpr_applies
        && let Some(tc) = consent.raw_tc_string.as_deref().filter(|tc| !tc.is_empty())
    {
        object.insert("gdpr_consent".to_owned(), json!(tc));
    }
    if let Some(usp) = consent
        .raw_us_privacy
        .as_deref()
        .filter(|usp| !usp.is_empty())
    {
        object.insert("us_privacy".to_owned(), json!(usp));
    }
    if let Some(gpp) = consent
        .raw_gpp_string
        .as_deref()
        .filter(|gpp| !gpp.is_empty())
    {
        object.insert("gpp_string".to_owned(), json!(gpp));
        if let Some(sections) = consent
            .gpp_section_ids
            .as_ref()
            .filter(|ids| !ids.is_empty())
        {
            object.insert("gpp_sid".to_owned(), json!(sections));
        }
    }
    if let Some(signature) = signature {
        object.insert("s".to_owned(), json!(signature));
    }
    if let Some(provider) = config.provider.as_deref().filter(|p| !p.is_empty()) {
        object.insert("provider".to_owned(), json!(provider));
    }
    Some(body)
}

async fn read_response(response: Response<EdgeBody>) -> Id5Outcome {
    let status = response.status();
    if !status.is_success() {
        log::warn!("ID5: fetch returned status {status}");
        return Id5Outcome::Failed;
    }
    match collect_response_bounded(
        response.into_body(),
        MAX_ID5_RESPONSE_BYTES,
        ID5_INTEGRATION_ID,
    )
    .await
    {
        Ok(bytes) => parse_response(&bytes),
        Err(err) => {
            log::warn!("ID5: response body could not be read: {err:?}");
            Id5Outcome::Failed
        }
    }
}

/// Parses an ID5 fetch response into an outcome.
fn parse_response(body: &[u8]) -> Id5Outcome {
    let Ok(response) = serde_json::from_slice::<JsonValue>(body) else {
        log::warn!("ID5: response was not valid JSON");
        return Id5Outcome::Failed;
    };
    if response.get("universal_uid").and_then(JsonValue::as_str) == Some("0") {
        log::info!("ID5: no consent to issue an ID for this request");
        return Id5Outcome::NoConsent;
    }
    let Some(eid) = response
        .pointer("/ids/id5id/eid")
        .and_then(id5_eid_from_json)
    else {
        log::warn!("ID5: response carried no usable ids.id5id.eid");
        return Id5Outcome::Failed;
    };
    let signature = response
        .get("signature")
        .and_then(JsonValue::as_str)
        .filter(|signature| is_valid_signature(signature))
        .map(str::to_owned);
    let refresh_seconds = response
        .pointer("/cache_control/max_age_sec")
        .and_then(JsonValue::as_u64)
        .and_then(|seconds| u32::try_from(seconds).ok())
        .map_or(default_refresh_seconds(), |seconds| {
            seconds.clamp(MIN_REFRESH_SECONDS, MAX_REFRESH_SECONDS)
        });
    log::info!("ID5: issued an ID");
    Id5Outcome::Issued {
        eid,
        signature,
        refresh_seconds,
    }
}

/// Extracts a validated single-UID ID5 EID from an `OpenRTB` EID object.
fn id5_eid_from_json(value: &JsonValue) -> Option<Eid> {
    if value.get("source")?.as_str()? != ID5_SOURCE {
        return None;
    }
    let uid = value.get("uids")?.as_array()?.first()?;
    let id = uid.get("id")?.as_str()?;
    if id == "0" || !is_valid_eid_uid(id) {
        return None;
    }
    let atype = uid
        .get("atype")
        .and_then(JsonValue::as_i64)
        .and_then(|atype| i32::try_from(atype).ok());
    let ext = uid.get("ext").filter(|ext| ext.is_object()).cloned();
    Some(Eid {
        source: ID5_SOURCE.to_owned(),
        uids: vec![Uid {
            id: id.to_owned(),
            atype,
            ext,
        }],
    })
}

#[derive(Serialize)]
struct ServerCookieEid<'a> {
    source: &'a str,
    uids: &'a [Uid],
}

/// Encodes an EID in the structured `ts-eids` cookie format.
fn encode_server_cookie(eid: &Eid) -> Option<String> {
    let entries = [ServerCookieEid {
        source: &eid.source,
        uids: &eid.uids,
    }];
    serde_json::to_vec(&entries)
        .ok()
        .map(|bytes| BASE64.encode(bytes))
}

/// Reads the ID5 entry from a [`COOKIE_TS_EIDS_S`] value.
fn server_cookie_id5_eid(value: &str) -> Option<Eid> {
    match parse_prebid_eids_cookie(value) {
        Ok(eids) => eids.into_iter().find(|eid| {
            eid.source == ID5_SOURCE && eid.uids.iter().any(|uid| is_valid_eid_uid(&uid.id))
        }),
        Err(err) => {
            log::debug!("ID5: server EID cookie could not be parsed: {err}");
            None
        }
    }
}

/// Drops the KV-stored ID5 EID when the request already carries a current one.
///
/// ID5 re-encrypts `universal_uid` on every fetch, and KV keeps the first value
/// it stored. Forwarding both would send bidders two ID5 IDs for one user, the
/// older one without ID5's `ext`. Other stored sources are kept.
pub(crate) fn drop_stored_id5_when_current(
    stored: Option<Vec<Eid>>,
    current: Option<&[Eid]>,
) -> Option<Vec<Eid>> {
    if !current.is_some_and(|eids| eids.iter().any(|eid| eid.source == ID5_SOURCE)) {
        return stored;
    }
    stored.map(|mut eids| {
        eids.retain(|eid| eid.source != ID5_SOURCE);
        eids
    })
}

/// Parses the server-owned EID cookie for any auction path.
///
/// Returns `None` when the cookie is absent, malformed, or carries no ID5 ID.
pub(crate) fn server_cookie_eids(value: Option<&str>) -> Option<Vec<Eid>> {
    value.and_then(server_cookie_id5_eid).map(|eid| vec![eid])
}

fn is_valid_signature(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SIGNATURE_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn format_cookie(name: &str, value: &str, domain: &str, max_age: u32) -> String {
    format!(
        "{name}={value}; Domain={domain}; Path=/; Secure; SameSite=Lax; Max-Age={max_age}; HttpOnly"
    )
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::sync::Arc;

    use super::*;
    use crate::consent::ConsentContext;
    use crate::platform::test_support::{StubHttpClient, build_services_with_http_client};
    use crate::test_support::tests::create_test_settings;

    const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";

    fn config(mode: Id5FetchMode) -> Id5Config {
        Id5Config {
            enabled: true,
            partner: 173,
            endpoint: default_endpoint(),
            mode,
            timeout_ms: 300,
            provider: None,
            refresh_seconds: default_refresh_seconds(),
            demo_consent_banner: false,
            cookie_max_age_seconds: default_cookie_max_age_seconds(),
        }
    }

    fn input(consent: &ConsentContext, ip: Option<IpAddr>) -> Id5FetchInput<'_> {
        Id5FetchInput {
            consent,
            page_url: "https://publisher.example.com/article",
            client_ip: ip,
            user_agent: Some(UA),
        }
    }

    fn issued_response() -> JsonValue {
        json!({
            "signature": "ID5_synthetic-signature_01",
            "universal_uid": "ID5*synthetic",
            "cache_control": {"max_age_sec": 7200},
            "ids": {"id5id": {"eid": {
                "source": "id5-sync.com",
                "uids": [{"id": "ID5*synthetic", "atype": 1, "ext": {"linkType": 1, "pba": "synthetic-pba"}}]
            }}}
        })
    }

    #[test]
    fn request_body_carries_required_fields_and_consent() {
        let consent = ConsentContext {
            raw_us_privacy: Some("1YNN".to_owned()),
            raw_gpp_string: Some("DBABTA~1YNN".to_owned()),
            gpp_section_ids: Some(vec![6]),
            ..ConsentContext::default()
        };
        let ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 42));

        let body = build_request_body(
            &config(Id5FetchMode::Parallel),
            &input(&consent, Some(ip)),
            Some("ID5_sig"),
            Some(1_791_000_000),
        )
        .expect("should build a body for an IPv4 client");

        assert_eq!(body["partner"], 173, "should send the partner number");
        assert_eq!(body["o"], "trusted-server", "should send the origin");
        assert_eq!(body["ip"], "198.51.100.42", "should send the client IPv4");
        assert_eq!(body["ua"], UA, "should send the user agent");
        assert_eq!(
            body["ts"], "2026-10-03T04:00:00Z",
            "should send an ISO-8601 timestamp"
        );
        assert_eq!(body["gdpr"], 0, "should mark GDPR as not applying");
        assert_eq!(body["us_privacy"], "1YNN", "should forward US Privacy");
        assert_eq!(body["gpp_string"], "DBABTA~1YNN", "should forward GPP");
        assert_eq!(
            body["gpp_sid"],
            json!([6]),
            "should forward GPP section IDs"
        );
        assert_eq!(body["s"], "ID5_sig", "should echo the stored signature");
        assert!(
            body.get("gdpr_consent").is_none(),
            "should omit TCF when GDPR does not apply"
        );
        assert!(
            body["v"]
                .as_str()
                .is_some_and(|v| v.starts_with("trusted-server-")),
            "should send a version"
        );
    }

    #[test]
    fn request_body_sends_tcf_only_when_gdpr_applies() {
        let consent = ConsentContext {
            gdpr_applies: true,
            raw_tc_string: Some("CPsynthetic".to_owned()),
            ..ConsentContext::default()
        };
        let ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 42));

        let body = build_request_body(
            &config(Id5FetchMode::Parallel),
            &input(&consent, Some(ip)),
            None,
            Some(1_791_000_000),
        )
        .expect("should build a body");

        assert_eq!(body["gdpr"], 1, "should mark GDPR as applying");
        assert_eq!(
            body["gdpr_consent"], "CPsynthetic",
            "should forward the TC string"
        );
        assert!(body.get("s").is_none(), "should omit an absent signature");
    }

    #[test]
    fn request_body_skips_ipv6_missing_ip_missing_ua_and_missing_clock() {
        let consent = ConsentContext::default();
        let config = config(Id5FetchMode::Parallel);
        let v4 = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 42));
        let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);

        assert!(
            build_request_body(&config, &input(&consent, Some(v6)), None, Some(1)).is_none(),
            "should skip IPv6 clients"
        );
        assert!(
            build_request_body(&config, &input(&consent, None), None, Some(1)).is_none(),
            "should skip requests without a client IP"
        );
        let mut no_ua = input(&consent, Some(v4));
        no_ua.user_agent = None;
        assert!(
            build_request_body(&config, &no_ua, None, Some(1)).is_none(),
            "should skip requests without a user agent"
        );
        assert!(
            build_request_body(&config, &input(&consent, Some(v4)), None, None).is_none(),
            "should skip when the clock is unavailable"
        );
    }

    #[test]
    fn parse_response_issues_id5_eid_with_signature_and_refresh() {
        let body = serde_json::to_vec(&issued_response()).expect("should serialize fixture");

        let outcome = parse_response(&body);

        let Id5Outcome::Issued {
            eid,
            signature,
            refresh_seconds,
        } = outcome
        else {
            panic!("should issue an ID, got {outcome:?}");
        };
        assert_eq!(eid.source, ID5_SOURCE, "should use the ID5 source");
        assert_eq!(eid.uids[0].id, "ID5*synthetic", "should use the issued UID");
        assert_eq!(eid.uids[0].atype, Some(1), "should keep atype");
        assert_eq!(
            eid.uids[0].ext.as_ref().and_then(|ext| ext.get("pba")),
            Some(&json!("synthetic-pba")),
            "should keep ID5 ext for bidders"
        );
        assert_eq!(signature.as_deref(), Some("ID5_synthetic-signature_01"));
        assert_eq!(refresh_seconds, 7200, "should use ID5's refresh interval");
    }

    #[test]
    fn parse_response_handles_opt_out_and_malformed_bodies() {
        let opt_out = serde_json::to_vec(&json!({"universal_uid": "0", "ids": {}}))
            .expect("should serialize fixture");
        assert!(
            matches!(parse_response(&opt_out), Id5Outcome::NoConsent),
            "should treat a zero UID as no consent"
        );
        assert!(
            matches!(parse_response(b"not json"), Id5Outcome::Failed),
            "should fail on invalid JSON"
        );
        let wrong_source = serde_json::to_vec(&json!({
            "universal_uid": "ID5*x",
            "ids": {"id5id": {"eid": {"source": "other.example", "uids": [{"id": "ID5*x"}]}}}
        }))
        .expect("should serialize fixture");
        assert!(
            matches!(parse_response(&wrong_source), Id5Outcome::Failed),
            "should reject a non-ID5 source"
        );
        let mut bad_signature = issued_response();
        bad_signature["signature"] = json!("has spaces; and=semicolons");
        let Id5Outcome::Issued { signature, .. } =
            parse_response(&serde_json::to_vec(&bad_signature).expect("should serialize fixture"))
        else {
            panic!("should still issue the ID");
        };
        assert!(
            signature.is_none(),
            "should drop a signature that is not cookie-safe"
        );
    }

    #[test]
    fn server_cookie_round_trips_through_the_eid_cookie_parser() {
        let body = serde_json::to_vec(&issued_response()).expect("should serialize fixture");
        let Id5Outcome::Issued { eid, .. } = parse_response(&body) else {
            panic!("should issue an ID");
        };

        let encoded = encode_server_cookie(&eid).expect("should encode the cookie");
        let decoded = server_cookie_eids(Some(&encoded)).expect("should decode the cookie");

        assert_eq!(decoded.len(), 1, "should hold one EID");
        assert_eq!(decoded[0].source, ID5_SOURCE);
        assert_eq!(decoded[0].uids[0].id, "ID5*synthetic");
        assert_eq!(
            decoded[0].uids[0].atype,
            Some(1),
            "should keep atype through the cookie"
        );
        assert!(
            server_cookie_eids(Some("not-base64!")).is_none(),
            "should ignore a bad cookie"
        );
        assert!(
            server_cookie_eids(None).is_none(),
            "should ignore a missing cookie"
        );
    }

    fn auction_request(cookie: Option<&str>) -> Request<EdgeBody> {
        let mut builder = http::Request::builder().uri("https://publisher.example.com/auction");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    fn settings_with_id5(mode: &str) -> Settings {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                ID5_INTEGRATION_ID,
                &json!({"enabled": true, "partner": 173, "mode": mode}),
            )
            .expect("should insert ID5 config");
        settings
    }

    #[test]
    fn load_respects_identity_consent_and_disabled_config() {
        let settings = settings_with_id5("parallel");
        let req = auction_request(None);

        assert!(
            Id5AuctionState::load(&settings, &req, false).is_none(),
            "should not load without identity consent"
        );
        assert!(
            Id5AuctionState::load(&create_test_settings(), &req, true).is_none(),
            "should not load when the integration is not configured"
        );
        let state = Id5AuctionState::load(&settings, &req, true).expect("should load");
        assert!(
            state.should_fetch(None),
            "should fetch without a fresh marker"
        );
    }

    #[test]
    fn should_fetch_skips_fresh_ids_and_browser_supplied_id5() {
        let settings = settings_with_id5("parallel");
        let fresh =
            Id5AuctionState::load(&settings, &auction_request(Some("ts-id5-fresh=1")), true)
                .expect("should load");
        assert!(!fresh.should_fetch(None), "should not refetch a fresh ID");

        let state =
            Id5AuctionState::load(&settings, &auction_request(None), true).expect("should load");
        let browser = [Eid {
            source: ID5_SOURCE.to_owned(),
            uids: vec![Uid {
                id: "ID5*browser".to_owned(),
                atype: Some(1),
                ext: None,
            }],
        }];
        assert!(
            !state.should_fetch(Some(&browser)),
            "should not fetch when the browser supplied an ID5 EID"
        );
    }

    #[tokio::test]
    async fn parallel_fetch_posts_to_partner_route_and_sets_cookies() {
        let settings = settings_with_id5("parallel");
        let state =
            Id5AuctionState::load(&settings, &auction_request(None), true).expect("should load");
        let client = Arc::new(StubHttpClient::new());
        client.push_response(
            200,
            serde_json::to_vec(&issued_response()).expect("should serialize fixture"),
        );
        let services = build_services_with_http_client(client.clone());
        let consent = ConsentContext::default();
        let fetch_input = input(&consent, Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 42))));

        let in_flight = state.begin(&services, &fetch_input).await;
        assert!(
            matches!(in_flight, Id5InFlight::Pending(_)),
            "parallel mode should not wait for ID5"
        );
        let outcome = Id5AuctionState::finish(&services, in_flight).await;
        let mut response = Response::new(EdgeBody::empty());
        state.apply_cookies(&settings, &outcome, &mut response);

        assert_eq!(
            client.recorded_request_uris(),
            vec!["https://api.id5-sync.com/gs/v2/173.json".to_owned()],
            "should call the partner's web fetch route"
        );
        let cookies: Vec<&str> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect();
        assert!(
            cookies.iter().any(|c| c.starts_with("ts-eids-s=")
                && c.contains("HttpOnly")
                && c.contains("Max-Age=2592000")),
            "should set the server EID cookie: {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with("ts-id5-sig=ID5_synthetic-signature_01;")),
            "should store the signature: {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with("ts-id5-fresh=1;") && c.contains("Max-Age=7200")),
            "should mark the ID fresh for ID5's refresh interval: {cookies:?}"
        );
    }

    #[tokio::test]
    async fn blocking_fetch_returns_the_outcome_and_failures_write_nothing() {
        let settings = settings_with_id5("blocking");
        let state =
            Id5AuctionState::load(&settings, &auction_request(None), true).expect("should load");
        let client = Arc::new(StubHttpClient::new());
        client.push_response(503, b"unavailable".to_vec());
        let services = build_services_with_http_client(client);
        let consent = ConsentContext::default();
        let fetch_input = input(&consent, Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 42))));

        let in_flight = state.begin(&services, &fetch_input).await;
        let Id5InFlight::Done(outcome) = in_flight else {
            panic!("blocking mode should finish before returning");
        };
        assert!(
            matches!(outcome, Id5Outcome::Failed),
            "should fail on a 5xx"
        );
        let mut response = Response::new(EdgeBody::empty());
        state.apply_cookies(&settings, &outcome, &mut response);
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "should not write cookies after a failed fetch"
        );
    }

    #[test]
    fn no_consent_clears_the_stored_id() {
        let settings = settings_with_id5("parallel");
        let state =
            Id5AuctionState::load(&settings, &auction_request(None), true).expect("should load");
        let mut response = Response::new(EdgeBody::empty());

        state.apply_cookies(&settings, &Id5Outcome::NoConsent, &mut response);

        let cookies: Vec<&str> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect();
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with("ts-eids-s=;") && c.contains("Max-Age=0")),
            "should expire the server EID cookie: {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with("ts-id5-sig=;") && c.contains("Max-Age=0")),
            "should expire the signature: {cookies:?}"
        );
    }

    fn settings_with_demo_banner(enabled: bool) -> Settings {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                ID5_INTEGRATION_ID,
                &json!({"enabled": true, "partner": 173, "demo_consent_banner": enabled}),
            )
            .expect("should insert ID5 config");
        settings
    }

    fn restrictive_us_consent() -> ConsentContext {
        ConsentContext {
            jurisdiction: Jurisdiction::UsState("TN".to_owned()),
            gpc: true,
            raw_tc_string: Some("CPsynthetic".to_owned()),
            ..ConsentContext::default()
        }
    }

    fn jar(cookie: &str) -> CookieJar {
        crate::cookies::parse_cookies_to_jar(cookie)
    }

    #[test]
    fn demo_consent_overrides_restrictive_signals_when_accepted() {
        let settings = settings_with_demo_banner(true);
        let mut consent = restrictive_us_consent();
        assert!(
            !crate::ec::consent::ec_consent_granted(&consent),
            "fixture should start without EC consent"
        );

        apply_demo_consent(
            &settings,
            Some(&jar("ts-demo-consent=granted")),
            &mut consent,
        );

        assert!(
            crate::ec::consent::ec_consent_granted(&consent),
            "an accepted demo banner should allow EC"
        );
        assert!(
            crate::consent::consent_allows_server_side_auction(&consent),
            "an accepted demo banner should allow the server-side auction"
        );
        assert!(
            consent.raw_tc_string.is_none(),
            "should drop the stale TCF string so EIDs are not stripped"
        );
    }

    #[test]
    fn demo_consent_leaves_consent_alone_when_declined_absent_or_disabled() {
        let cases = [
            (
                settings_with_demo_banner(true),
                Some("ts-demo-consent=denied"),
            ),
            (settings_with_demo_banner(true), None),
            (
                settings_with_demo_banner(false),
                Some("ts-demo-consent=granted"),
            ),
            (create_test_settings(), Some("ts-demo-consent=granted")),
        ];
        for (settings, cookie) in cases {
            let mut consent = restrictive_us_consent();
            let cookie_jar = cookie.map(jar);

            apply_demo_consent(&settings, cookie_jar.as_ref(), &mut consent);

            assert!(
                !crate::ec::consent::ec_consent_granted(&consent),
                "should keep normal consent rules for {cookie:?}"
            );
        }
    }

    #[test]
    fn demo_banner_registers_only_when_enabled() {
        assert!(
            register(&settings_with_demo_banner(false))
                .expect("should parse config")
                .is_none(),
            "should not register the banner when disabled"
        );
        let registration = register(&settings_with_demo_banner(true))
            .expect("should parse config")
            .expect("should register the banner when enabled");

        assert_eq!(
            registration.head_injectors.len(),
            1,
            "should inject one banner"
        );
        assert!(registration.js_disabled, "should not ship a JS bundle");
        assert!(
            DEMO_CONSENT_BANNER_SCRIPT.contains("ts-demo-consent")
                && !DEMO_CONSENT_BANNER_SCRIPT.contains("innerHTML"),
            "banner should write the demo cookie without innerHTML"
        );
    }

    fn eid(source: &str, id: &str) -> Eid {
        Eid {
            source: source.to_owned(),
            uids: vec![Uid {
                id: id.to_owned(),
                atype: Some(1),
                ext: None,
            }],
        }
    }

    #[test]
    fn stored_id5_is_dropped_only_when_a_current_id5_exists() {
        let stored = Some(vec![
            eid(ID5_SOURCE, "ID5*stored"),
            eid("partner.example.com", "kept"),
        ]);
        let current = [eid(ID5_SOURCE, "ID5*current")];

        let kept = drop_stored_id5_when_current(stored.clone(), Some(&current))
            .expect("should keep the stored list");

        assert_eq!(kept.len(), 1, "should drop only the stored ID5 entry");
        assert_eq!(
            kept[0].source, "partner.example.com",
            "should keep other sources"
        );
        assert_eq!(
            drop_stored_id5_when_current(stored.clone(), None).map(|eids| eids.len()),
            Some(2),
            "should keep the stored ID5 when the request has none"
        );
        assert_eq!(
            drop_stored_id5_when_current(stored, Some(&[eid("partner.example.com", "x")]))
                .map(|eids| eids.len()),
            Some(2),
            "should keep the stored ID5 when the request has other sources only"
        );
        assert!(
            drop_stored_id5_when_current(None, Some(&current)).is_none(),
            "should leave an absent stored list absent"
        );
    }
}
