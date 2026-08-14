//! Shared `OpenRTB` 2.6 request/response support for config-first providers.
//!
//! Profiles receive only routed, privacy-approved facts and never the raw
//! downstream request or unrestricted runtime services.

use std::collections::{BTreeMap, HashMap, HashSet};

use error_stack::Report;
use serde_json::{Map, Value, json};
use url::Url;

use super::plan::{NotificationPolicy, ProviderPlan};
use super::profile::{
    ApsProfilePlan, CompiledOpenRtbProfile, PrebidProfilePlan, StandardProfilePlan,
};
use super::routing::{
    PrebidTransportHeaders, ProviderAuctionInput, ProviderSlotInput, RoutedAuction,
};
use super::types::{AuctionResponse, Bid};
use crate::consent::ConsentSource;
use crate::error::TrustedServerError;
use crate::openrtb::{
    Banner, ConsentedProvidersSettings, Device, Format, Geo, Imp, OpenRtbRequest, Publisher, Regs,
    RegsExt, Site, ToExt as _, TrustedServerExt, User, UserExt, to_openrtb_i32,
};
use crate::request_signing::{RequestSigner, SIGNING_VERSION, SigningParams};

const DEFAULT_CURRENCY: &str = "USD";
const APS_SDK_SOURCE: &str = "prebid";
const APS_SDK_VERSION: &str = "2.2.0";
const MAX_CONSERVATIVE_LANGUAGE_BYTES: usize = 8;

/// Result of request construction before transport.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum OpenRtbBuildOutcome {
    Ready(OpenRtbRequest),
    NoImpressions,
}

/// Explicit, deterministic signing input. No signer is loaded by this driver.
pub(crate) struct RequestFinalization<'a> {
    pub(crate) signer: Option<&'a RequestSigner>,
    pub(crate) signing_params: SigningParams,
}

/// Build one provider request from its immutable routed input.
///
/// # Errors
///
/// Returns an auction error when static/profile extensions cannot be merged or
/// the supplied signing input does not bind the already-fixed request ID.
pub(crate) fn build_request(
    input: &ProviderAuctionInput,
    routed: &RoutedAuction,
    provider: &ProviderPlan,
    effective_timeout_ms: u32,
    finalization: &RequestFinalization<'_>,
) -> Result<OpenRtbBuildOutcome, Report<TrustedServerError>> {
    let policy = ProfilePolicy::from(&provider.profile);
    let mut request = build_common_request(input, routed, policy, effective_timeout_ms);
    if request.imp.is_empty() {
        return Ok(OpenRtbBuildOutcome::NoImpressions);
    }
    policy.augment_request(&mut request, input, routed)?;
    finalize_request(&mut request, policy, finalization)?;
    Ok(OpenRtbBuildOutcome::Ready(request))
}

#[derive(Clone, Copy)]
enum ProfilePolicy<'a> {
    Standard(&'a StandardProfilePlan),
    Prebid(&'a PrebidProfilePlan),
    Aps(&'a ApsProfilePlan),
}

impl<'a> From<&'a CompiledOpenRtbProfile> for ProfilePolicy<'a> {
    fn from(profile: &'a CompiledOpenRtbProfile) -> Self {
        match profile {
            CompiledOpenRtbProfile::Standard(plan) => Self::Standard(plan),
            CompiledOpenRtbProfile::PrebidServer(plan) => Self::Prebid(plan),
            CompiledOpenRtbProfile::Aps(plan) => Self::Aps(plan),
        }
    }
}

impl ProfilePolicy<'_> {
    fn augment_request(
        self,
        request: &mut OpenRtbRequest,
        input: &ProviderAuctionInput,
        routed: &RoutedAuction,
    ) -> Result<(), Report<TrustedServerError>> {
        match self {
            Self::Standard(plan) => apply_standard(request, plan),
            Self::Prebid(plan) => apply_prebid(request, input, routed, plan),
            Self::Aps(plan) => apply_aps(request, plan),
        }
    }

    fn keeps_pbs_identity_when_unsigned(self) -> bool {
        matches!(self, Self::Prebid(_))
    }
}

fn build_common_request(
    input: &ProviderAuctionInput,
    routed: &RoutedAuction,
    policy: ProfilePolicy<'_>,
    effective_timeout_ms: u32,
) -> OpenRtbRequest {
    let common = input.common_request();
    let imps = input
        .slots()
        .iter()
        .filter_map(|slot| build_imp(slot, policy))
        .collect();
    let site_domain = match policy {
        ProfilePolicy::Aps(plan) => plan
            .inventory_domain
            .clone()
            .unwrap_or_else(|| common.publisher.domain.clone()),
        _ => common.publisher.domain.clone(),
    };
    let page = match policy {
        ProfilePolicy::Aps(plan) => {
            aps_inventory_page(plan, common.publisher.page_url.as_deref(), &site_domain)
        }
        ProfilePolicy::Prebid(plan) => common.publisher.page_url.as_deref().map(|page| {
            plan.debug_query_params.as_deref().map_or_else(
                || page.to_string(),
                |query| append_query_fragment(page, query),
            )
        }),
        ProfilePolicy::Standard(_) => common.publisher.page_url.clone(),
    };
    let consent = common.user.consent.as_ref();
    let body_consent = match policy {
        ProfilePolicy::Prebid(plan) => consent.filter(|value| {
            plan.consent_forwarding.includes_body_consent()
                || !matches!(value.source, ConsentSource::Cookie)
        }),
        _ => consent,
    };
    let raw_tc = body_consent.and_then(|value| value.raw_tc_string.clone());
    let user = Some(User {
        id: common.user.id.clone(),
        consent: raw_tc.clone(),
        ext: UserExt {
            consent: raw_tc,
            consented_providers_settings: matches!(policy, ProfilePolicy::Prebid(_))
                .then(|| {
                    body_consent
                        .and_then(|value| value.raw_ac_string.clone())
                        .map(|consented_providers| ConsentedProvidersSettings {
                            consented_providers: Some(consented_providers),
                        })
                })
                .flatten(),
            eids: common.user.eids.clone(),
        }
        .to_ext(),
        ..Default::default()
    });
    let language = normalized_language(routed.prebid_transport_headers(), policy);
    let device = common
        .device
        .as_ref()
        .map(|device| Device {
            ua: device.user_agent.clone(),
            ip: device.ip.clone(),
            geo: device.geo.as_ref().map(|geo| Geo {
                country: Some(geo.country.clone()),
                region: geo.region.clone(),
                city: Some(geo.city.clone()),
                lat: matches!(policy, ProfilePolicy::Prebid(_)).then_some(geo.latitude),
                lon: matches!(policy, ProfilePolicy::Prebid(_)).then_some(geo.longitude),
                metro: (geo.metro_code > 0).then(|| geo.metro_code.to_string()),
                r#type: Some(2),
                ..Default::default()
            }),
            dnt: routed.dnt(),
            language: language.clone(),
            ..Default::default()
        })
        .or_else(|| {
            (routed.dnt().is_some() || language.is_some()).then_some(Device {
                dnt: routed.dnt(),
                language,
                ..Default::default()
            })
        });

    OpenRtbRequest {
        id: Some(common.id.clone()),
        imp: imps,
        site: Some(Site {
            domain: Some(site_domain.clone()),
            page,
            r#ref: matches!(policy, ProfilePolicy::Prebid(_))
                .then(|| header_string(routed.prebid_transport_headers().referer()))
                .flatten(),
            publisher: Some(Publisher {
                domain: Some(site_domain),
                ..Default::default()
            }),
            ..Default::default()
        }),
        user,
        device,
        regs: build_regs(body_consent, policy),
        test: match policy {
            ProfilePolicy::Prebid(plan) => plan.test_mode.then_some(true),
            _ => None,
        },
        tmax: to_openrtb_i32(
            effective_timeout_ms,
            "tmax",
            "config-first provider request",
        ),
        cur: vec![DEFAULT_CURRENCY.to_string()],
        ..Default::default()
    }
}

fn build_imp(slot: &ProviderSlotInput, policy: ProfilePolicy<'_>) -> Option<Imp> {
    let formats = slot
        .slot()
        .formats
        .iter()
        .filter_map(|format| {
            Some(Format {
                w: to_openrtb_i32(format.width, "format.w", "routed slot"),
                h: to_openrtb_i32(format.height, "format.h", "routed slot"),
                ..Default::default()
            })
            .filter(|value| value.w.is_some() && value.h.is_some())
        })
        .collect::<Vec<_>>();
    let first_width = formats.first()?.w;
    let first_height = formats.first()?.h;
    let aps_banner = matches!(policy, ProfilePolicy::Aps(_));
    Some(Imp {
        id: Some(slot.slot().id.clone()),
        banner: Some(Banner {
            format: formats,
            w: aps_banner.then_some(first_width).flatten(),
            h: aps_banner.then_some(first_height).flatten(),
            topframe: aps_banner.then_some(false),
            ..Default::default()
        }),
        tagid: matches!(policy, ProfilePolicy::Prebid(_)).then(|| slot.slot().id.clone()),
        bidfloor: slot.slot().floor_price,
        bidfloorcur: slot
            .slot()
            .floor_price
            .map(|_| DEFAULT_CURRENCY.to_string()),
        secure: Some(true),
        ..Default::default()
    })
}

fn apply_standard(
    request: &mut OpenRtbRequest,
    plan: &StandardProfilePlan,
) -> Result<(), Report<TrustedServerError>> {
    request.ext = nonempty_map(plan.request_ext.as_object().clone());
    for imp in &mut request.imp {
        imp.ext = nonempty_map(plan.imp_ext.as_object().clone());
    }
    Ok(())
}

fn apply_prebid(
    request: &mut OpenRtbRequest,
    input: &ProviderAuctionInput,
    _routed: &RoutedAuction,
    plan: &PrebidProfilePlan,
) -> Result<(), Report<TrustedServerError>> {
    for (imp, slot) in request.imp.iter_mut().zip(input.slots()) {
        let bidder = slot
            .bidder_params()
            .iter()
            .filter_map(|(bidder, params)| {
                let mut params = params.clone();
                plan.override_engine
                    .apply_routed(bidder.as_str(), slot.prebid_zone(), &mut params);
                params
                    .as_object()
                    .is_some_and(|params| !params.is_empty())
                    .then(|| (bidder.as_str().to_string(), params))
            })
            .collect::<Map<_, _>>();
        let mut prebid = Map::new();
        if !bidder.is_empty() {
            prebid.insert("bidder".to_string(), Value::Object(bidder));
        } else if slot.has_trusted_stored_request() || !slot.bidder_params().is_empty() {
            prebid.insert("storedrequest".to_string(), json!({"id": slot.slot().id}));
        }
        imp.ext = Some(Map::from_iter([(
            "prebid".to_string(),
            Value::Object(prebid),
        )]));
    }
    let mut prebid_request = Map::new();
    if plan.debug {
        prebid_request.insert("debug".to_string(), Value::Bool(true));
        prebid_request.insert("returnallbidstatus".to_string(), Value::Bool(true));
    }
    request.ext = Some(Map::from_iter([(
        "prebid".to_string(),
        Value::Object(prebid_request),
    )]));
    Ok(())
}

fn apply_aps(
    request: &mut OpenRtbRequest,
    plan: &ApsProfilePlan,
) -> Result<(), Report<TrustedServerError>> {
    request.ext = Some(Map::from_iter([
        (
            "account".to_string(),
            Value::String(plan.account_id.clone()),
        ),
        (
            "sdk".to_string(),
            json!({"source": APS_SDK_SOURCE, "version": APS_SDK_VERSION}),
        ),
    ]));
    Ok(())
}

fn finalize_request(
    request: &mut OpenRtbRequest,
    policy: ProfilePolicy<'_>,
    finalization: &RequestFinalization<'_>,
) -> Result<(), Report<TrustedServerError>> {
    let request_id = request.id.as_deref().ok_or_else(|| {
        Report::new(TrustedServerError::Auction {
            message: "OpenRTB request ID must be fixed before signing".to_string(),
        })
    })?;
    if request_id != finalization.signing_params.request_id {
        return Err(Report::new(TrustedServerError::Auction {
            message: "OpenRTB signing params do not bind the fixed request ID".to_string(),
        }));
    }
    let trusted_server = if let Some(signer) = finalization.signer {
        let signature = signer.sign_request(&finalization.signing_params)?;
        Some(TrustedServerExt {
            version: Some(SIGNING_VERSION.to_string()),
            signature: Some(signature),
            kid: Some(signer.kid.clone()),
            request_host: Some(finalization.signing_params.request_host.clone()),
            request_scheme: Some(finalization.signing_params.request_scheme.clone()),
            ts: Some(finalization.signing_params.timestamp),
        })
    } else if policy.keeps_pbs_identity_when_unsigned() {
        Some(TrustedServerExt {
            version: None,
            signature: None,
            kid: None,
            request_host: Some(finalization.signing_params.request_host.clone()),
            request_scheme: Some(finalization.signing_params.request_scheme.clone()),
            ts: None,
        })
    } else {
        None
    };
    if let Some(trusted_server) = trusted_server {
        let ext = request.ext.get_or_insert_with(Map::new);
        let serialized = serde_json::to_value(trusted_server).map_err(|error| {
            Report::new(TrustedServerError::Auction {
                message: format!("Failed to serialize Trusted Server extension: {error}"),
            })
        })?;
        ext.insert("trusted_server".to_string(), serialized);
    }
    Ok(())
}

fn build_regs(
    consent: Option<&crate::consent::ConsentContext>,
    policy: ProfilePolicy<'_>,
) -> Option<Regs> {
    let consent = consent?;
    if matches!(policy, ProfilePolicy::Aps(_)) {
        // Preserve APS exactly: any admitted context produces regs and GDPR is
        // derived only from the applicability bit, without jurisdiction rules.
        let ext = RegsExt {
            gdpr: Some(u8::from(consent.gdpr_applies)),
            us_privacy: consent.raw_us_privacy.clone(),
            gpp: consent.raw_gpp_string.clone(),
            gpp_sid: consent.gpp_section_ids.clone(),
        };
        return Some(Regs {
            coppa: None,
            gdpr: Some(consent.gdpr_applies),
            us_privacy: ext.us_privacy.clone(),
            gpp: ext.gpp.clone(),
            gpp_sid: ext
                .gpp_sid
                .as_ref()
                .map(|ids| ids.iter().copied().map(i32::from).collect())
                .unwrap_or_default(),
            ext: ext.to_ext(),
        });
    }

    // Standard deliberately shares PBS's conservative consent baseline. Keep
    // the legacy PBS empty-context and jurisdiction behavior byte-for-byte.
    let has_data = consent.gdpr_applies
        || consent.raw_us_privacy.is_some()
        || consent.raw_gpp_string.is_some()
        || consent.gpp_section_ids.is_some()
        || consent.gpc;
    if !has_data {
        return None;
    }
    let gdpr = if consent.gdpr_applies
        || matches!(
            consent.jurisdiction,
            crate::consent::jurisdiction::Jurisdiction::Gdpr
        ) {
        Some(true)
    } else if matches!(
        consent.jurisdiction,
        crate::consent::jurisdiction::Jurisdiction::Unknown
    ) {
        None
    } else {
        Some(false)
    };
    let us_privacy = consent.raw_us_privacy.clone();
    let gpp = consent.raw_gpp_string.clone();
    let gpp_sid = consent.gpp_section_ids.clone();
    let ext = RegsExt {
        gdpr: gdpr.map(u8::from),
        us_privacy: us_privacy.clone(),
        gpp: gpp.clone(),
        gpp_sid: gpp_sid.clone(),
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

fn normalized_language(
    headers: &PrebidTransportHeaders,
    policy: ProfilePolicy<'_>,
) -> Option<String> {
    let value = header_string(headers.accept_language())
        .and_then(|value| value.split(',').next().map(str::to_string))
        .and_then(|value| value.split(';').next().map(str::to_string))
        .and_then(|value| value.split('-').next().map(str::to_string))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    match policy {
        ProfilePolicy::Prebid(_) => Some(value),
        ProfilePolicy::Aps(_) | ProfilePolicy::Standard(_) => {
            (value.len() <= MAX_CONSERVATIVE_LANGUAGE_BYTES).then_some(value)
        }
    }
}

fn header_string(value: Option<&http::HeaderValue>) -> Option<String> {
    value
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn aps_inventory_page(
    plan: &ApsProfilePlan,
    publisher_page: Option<&str>,
    domain: &str,
) -> Option<String> {
    let fallback = publisher_page
        .and_then(valid_aps_page_url)
        .unwrap_or_else(|| format!("https://{domain}"));
    let Some(origin) = plan.inventory_page_origin.as_deref() else {
        return Some(fallback);
    };
    let (Ok(mut canonical), Ok(current)) = (Url::parse(origin), Url::parse(&fallback)) else {
        return Some(fallback);
    };
    canonical.set_path(current.path());
    canonical.set_query(current.query());
    canonical.set_fragment(None);
    Some(canonical.to_string())
}

fn append_query_fragment(url: &str, query: &str) -> String {
    if query.is_empty() || url.contains(query) {
        return url.to_string();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{query}")
}

fn valid_aps_page_url(value: &str) -> Option<String> {
    const MAX_APS_PAGE_URL_BYTES: usize = 8192;

    if value.len() > MAX_APS_PAGE_URL_BYTES {
        return None;
    }
    let parsed = Url::parse(value).ok()?;
    (matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none())
    .then(|| parsed.to_string())
}

fn nonempty_map(value: Map<String, Value>) -> Option<Map<String, Value>> {
    (!value.is_empty()).then_some(value)
}

/// Suppress notification URLs using exact returned-seat identity.
pub(crate) fn apply_notification_policy(bids: &mut [Bid], policy: &NotificationPolicy) {
    for bid in bids {
        let suppress = policy.suppress_all
            || bid
                .returned_seat
                .as_ref()
                .is_some_and(|seat| policy.suppress_seats.contains(seat));
        if suppress {
            bid.nurl = None;
            bid.burl = None;
        }
    }
}

/// Parse ordinary `OpenRTB` bids independently. Response ID is informational.
pub(crate) fn extract_standard_response(
    provider_id: &str,
    input: &ProviderAuctionInput,
    value: &Value,
    response_time_ms: u64,
) -> AuctionResponse {
    let Some(response) = value.as_object() else {
        return AuctionResponse::error(provider_id, response_time_ms)
            .with_metadata("error_type", json!("parse_response"));
    };
    match response.get("cur") {
        None => {}
        Some(Value::String(currency)) if currency.eq_ignore_ascii_case(DEFAULT_CURRENCY) => {}
        Some(Value::String(currency)) => {
            return AuctionResponse::no_bid(provider_id, response_time_ms)
                .with_metadata("unsupported_currency", json!(currency));
        }
        Some(_) => {
            return AuctionResponse::error(provider_id, response_time_ms)
                .with_metadata("error_type", json!("parse_response"));
        }
    }
    let allowed_impressions = input
        .slots()
        .iter()
        .map(|slot| {
            let dimensions = slot
                .slot()
                .formats
                .iter()
                .map(|format| (format.width, format.height))
                .collect::<HashSet<_>>();
            (slot.slot().id.as_str(), dimensions)
        })
        .collect::<BTreeMap<_, _>>();
    let mut bids = Vec::new();
    for seatbid in response
        .get("seatbid")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let returned_seat = seatbid
            .get("seat")
            .and_then(Value::as_str)
            .filter(|seat| !seat.is_empty());
        let Some(entries) = seatbid.get("bid").and_then(Value::as_array) else {
            continue;
        };
        for value in entries {
            if let Some(bid) = extract_standard_bid(value, returned_seat)
                && allowed_impressions
                    .get(bid.slot_id.as_str())
                    .is_some_and(|dimensions| dimensions.contains(&(bid.width, bid.height)))
            {
                bids.push(bid);
            }
        }
    }
    if bids.is_empty() {
        AuctionResponse::no_bid(provider_id, response_time_ms)
    } else {
        AuctionResponse::success(provider_id, bids, response_time_ms)
    }
}

fn extract_standard_bid(value: &Value, returned_seat: Option<&str>) -> Option<Bid> {
    let slot_id = value.get("impid")?.as_str()?.to_string();
    let price = value
        .get("price")?
        .as_f64()
        .filter(|price| price.is_finite() && *price >= 0.0)?;
    let width = u32::try_from(value.get("w")?.as_u64()?)
        .ok()
        .filter(|value| *value > 0)?;
    let height = u32::try_from(value.get("h")?.as_u64()?)
        .ok()
        .filter(|value| *value > 0)?;
    let creative = value
        .get("adm")
        .and_then(Value::as_str)
        .filter(|creative| !creative.is_empty())
        .map(str::to_string)?;
    Some(Bid {
        slot_id,
        price: Some(price),
        currency: DEFAULT_CURRENCY.to_string(),
        creative: Some(creative),
        adomain: value
            .get("adomain")
            .and_then(Value::as_array)
            .map(|domains| {
                domains
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            }),
        bidder: returned_seat.unwrap_or("unknown").to_string(),
        returned_seat: returned_seat.map(str::to_string),
        width,
        height,
        nurl: value
            .get("nurl")
            .and_then(Value::as_str)
            .map(str::to_string),
        burl: value
            .get("burl")
            .and_then(Value::as_str)
            .map(str::to_string),
        bid_id: value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        ad_id: value
            .get("adid")
            .and_then(Value::as_str)
            .map(str::to_string),
        creative_id: value
            .get("crid")
            .and_then(Value::as_str)
            .map(str::to_string),
        renderer: None,
        cache_id: None,
        cache_host: None,
        cache_path: None,
        metadata: HashMap::new(),
    })
}

/// Count bidder parameter objects a profile did not consume.
#[must_use]
pub(crate) fn unused_bidder_params_count(
    profile: &CompiledOpenRtbProfile,
    input: &ProviderAuctionInput,
) -> u32 {
    if profile.is_prebid_server() {
        return 0;
    }
    ignored_bidder_params_count(input)
}

/// Count routed bidder params for a profile known to ignore them.
#[must_use]
pub(crate) fn ignored_bidder_params_count(input: &ProviderAuctionInput) -> u32 {
    saturating_bidder_param_counts(input.slots().iter().map(|slot| slot.bidder_params().len()))
}

fn saturating_bidder_param_counts(counts: impl IntoIterator<Item = usize>) -> u32 {
    counts.into_iter().fold(0_u32, |count, slot_count| {
        count.saturating_add(u32::try_from(slot_count).unwrap_or(u32::MAX))
    })
}

#[cfg(test)]
mod routing_metadata_tests {
    use std::collections::BTreeMap;
    use std::str::FromStr as _;

    use serde_json::json;

    use super::{saturating_bidder_param_counts, unused_bidder_params_count};
    use crate::auction::plan::{
        AuctionPlan, AuctionPlanConfig, BidderId, BidderRouteConfig, NotificationConfig,
        ProviderConfig, ProviderId, RoutingMode,
    };
    use crate::auction::routing::route_auction;
    use crate::auction::test_support::canonical_parity_auction_request;

    #[test]
    fn unused_bidder_param_count_saturates_across_slots_and_large_values() {
        assert_eq!(saturating_bidder_param_counts([1, 2, 3]), 6);
        assert_eq!(
            saturating_bidder_param_counts([usize::try_from(u32::MAX).unwrap_or(usize::MAX), 1]),
            u32::MAX
        );
        assert_eq!(saturating_bidder_param_counts([usize::MAX]), u32::MAX);
    }

    #[test]
    fn unused_bidder_param_count_is_profile_aware() {
        for (profile, profile_config, expected) in [
            ("prebid-server", json!({}), 0),
            ("standard", json!({}), 1),
            ("aps", json!({"account_id":"example-account"}), 1),
        ] {
            let provider_id =
                ProviderId::from_str("fictional-provider").expect("should parse provider ID");
            let plan = AuctionPlan::compile(AuctionPlanConfig {
                timeout_ms: 1_000,
                providers: BTreeMap::from([(
                    provider_id.clone(),
                    ProviderConfig {
                        protocol: "openrtb-2.6".to_string(),
                        profile: profile.to_string(),
                        endpoint: if profile == "aps" {
                            "https://aps.example/e/pb/bid".to_string()
                        } else {
                            "https://provider.example/openrtb".to_string()
                        },
                        timeout_ms: None,
                        routing: RoutingMode::Explicit,
                        notifications: NotificationConfig::default(),
                        profile_config,
                    },
                )]),
                bidders: BTreeMap::from([(
                    BidderId::from_str("exampleBidder").expect("should parse bidder ID"),
                    BidderRouteConfig {
                        provider: provider_id,
                    },
                )]),
                mediator: None,
                request_signing: None,
            })
            .expect("should compile profile plan");
            let inbound = http::Request::new(edgezero_core::body::Body::empty());
            let routed = route_auction(canonical_parity_auction_request(), &inbound, &plan, None);

            assert_eq!(
                unused_bidder_params_count(&plan.providers()[0].profile, &routed.inputs()[0]),
                expected,
                "{profile} should report only bidder params it ignores"
            );
        }
    }
}

#[cfg(test)]
mod test_executor;
#[cfg(test)]
mod tests;
