//! The shared `OpenRTB` 2.6 request and response driver.
//!
//! Every demand source's request is built here from routed, privacy-approved
//! facts, never from the raw downstream request or unrestricted runtime
//! services. A demand implementation decides only what
//! [`crate::auction::demand`] exposes, and this driver builds the rest the same
//! way for all of them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use error_stack::Report;
use serde_json::{Map, Value, json};

use super::demand::{CompiledDemand, DemandFieldPolicy, RegsPolicy};
use super::plan::{NotificationPolicy, ProviderPlan};
use super::routing::{ProviderAuctionInput, ProviderSlotInput, RoutedAuction, TransportHeaders};
use super::types::{AdFormat, AuctionResponse, Bid};
use crate::error::TrustedServerError;
use crate::openrtb::{
    Banner, ConsentedProvidersSettings, Device, Format, Geo, Imp, OpenRtbRequest, Publisher, Regs,
    RegsExt, Site, ToExt as _, TrustedServerExt, User, UserExt, to_openrtb_i32,
};
use crate::request_signing::{RequestSigner, SIGNING_VERSION, SigningParams};

const DEFAULT_CURRENCY: &str = "USD";

/// Fixed reasons why an upstream bid failed response admission.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum BidRejectionReason {
    InvalidBid,
    UnrequestedImpression,
    DimensionMismatch,
    AmbiguousDimensions,
}

impl BidRejectionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidBid => "invalid_bid",
            Self::UnrequestedImpression => "unrequested_impression",
            Self::DimensionMismatch => "dimension_mismatch",
            Self::AmbiguousDimensions => "ambiguous_dimensions",
        }
    }
}

/// Bounded aggregate diagnostics for rejected upstream bids.
#[derive(Debug, Default)]
pub(crate) struct ResponseAdmissionDiagnostics {
    rejected_bid_count: u32,
    reason_counts: BTreeMap<BidRejectionReason, u32>,
}

impl ResponseAdmissionDiagnostics {
    /// Record one rejected bid without retaining upstream payload data.
    pub(crate) fn record(&mut self, reason: BidRejectionReason) {
        self.rejected_bid_count = self.rejected_bid_count.saturating_add(1);
        let count = self.reason_counts.entry(reason).or_default();
        *count = count.saturating_add(1);
    }

    /// Attach fixed-cardinality rejection counts to a provider response.
    pub(crate) fn attach_to(self, response: &mut AuctionResponse) {
        if self.rejected_bid_count == 0 {
            return;
        }
        let reasons = self
            .reason_counts
            .into_iter()
            .map(|(reason, count)| (reason.as_str().to_string(), json!(count)))
            .collect::<Map<_, _>>();
        response.metadata.insert(
            "response_admission".to_string(),
            json!({
                "rejected_bid_count": self.rejected_bid_count,
                "rejection_reasons": reasons,
            }),
        );
    }
}

#[derive(Clone, Copy)]
enum DimensionMatch {
    Unique((u32, u32)),
    Ambiguous,
}

impl DimensionMatch {
    fn include(&mut self, dimensions: (u32, u32)) {
        if matches!(self, Self::Unique(existing) if *existing != dimensions) {
            *self = Self::Ambiguous;
        }
    }

    fn resolve(self) -> Result<(u32, u32), BidRejectionReason> {
        match self {
            Self::Unique(dimensions) => Ok(dimensions),
            Self::Ambiguous => Err(BidRejectionReason::AmbiguousDimensions),
        }
    }
}

/// Precomputed dimension resolutions for one requested impression.
pub(crate) struct SlotBidDimensions {
    exact: BTreeSet<(u32, u32)>,
    by_width: BTreeMap<u32, DimensionMatch>,
    by_height: BTreeMap<u32, DimensionMatch>,
    unqualified: Option<DimensionMatch>,
}

impl SlotBidDimensions {
    fn from_formats(formats: &[AdFormat]) -> Self {
        let mut exact = BTreeSet::new();
        let mut by_width = BTreeMap::new();
        let mut by_height = BTreeMap::new();
        let mut unqualified: Option<DimensionMatch> = None;

        for format in formats {
            let dimensions = (format.width, format.height);
            if !exact.insert(dimensions) {
                continue;
            }
            by_width
                .entry(format.width)
                .and_modify(|candidate: &mut DimensionMatch| candidate.include(dimensions))
                .or_insert(DimensionMatch::Unique(dimensions));
            by_height
                .entry(format.height)
                .and_modify(|candidate: &mut DimensionMatch| candidate.include(dimensions))
                .or_insert(DimensionMatch::Unique(dimensions));
            match &mut unqualified {
                Some(candidate) => candidate.include(dimensions),
                None => unqualified = Some(DimensionMatch::Unique(dimensions)),
            }
        }

        Self {
            exact,
            by_width,
            by_height,
            unqualified,
        }
    }
}

/// Precomputed requested banner dimensions keyed by impression ID.
pub(crate) type BidDimensionIndex = BTreeMap<String, SlotBidDimensions>;

/// Build the requested-dimension index once for one provider response.
pub(crate) fn build_bid_dimension_index(input: &ProviderAuctionInput) -> BidDimensionIndex {
    let mut index = BidDimensionIndex::new();
    for slot in input.slots() {
        index
            .entry(slot.slot().id.clone())
            .or_insert_with(|| SlotBidDimensions::from_formats(slot.slot().formats.as_slice()));
    }
    index
}

/// Parse an optional positive `OpenRTB` bid dimension.
pub(crate) fn parse_optional_bid_dimension(
    value: &Value,
    key: &str,
) -> Result<Option<u32>, BidRejectionReason> {
    let Some(raw) = value.get(key) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }

    let dimension = raw
        .as_u64()
        .and_then(|dimension| u32::try_from(dimension).ok())
        .or_else(|| {
            let dimension = raw.as_f64()?;
            (dimension.is_finite()
                && dimension > 0.0
                && dimension.fract() == 0.0
                && dimension <= f64::from(u32::MAX))
            .then_some(dimension as u32)
        })
        .filter(|dimension| *dimension > 0)
        .ok_or(BidRejectionReason::InvalidBid)?;
    Ok(Some(dimension))
}

/// Validate explicit dimensions or infer them from one matching banner format.
pub(crate) fn resolve_bid_dimensions(
    dimensions_by_slot: &BidDimensionIndex,
    slot_id: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<(u32, u32), BidRejectionReason> {
    let dimensions = dimensions_by_slot
        .get(slot_id)
        .ok_or(BidRejectionReason::UnrequestedImpression)?;

    match (width, height) {
        (Some(width), Some(height)) => dimensions
            .exact
            .contains(&(width, height))
            .then_some((width, height))
            .ok_or(BidRejectionReason::DimensionMismatch),
        (Some(width), None) => dimensions
            .by_width
            .get(&width)
            .copied()
            .ok_or(BidRejectionReason::DimensionMismatch)?
            .resolve(),
        (None, Some(height)) => dimensions
            .by_height
            .get(&height)
            .copied()
            .ok_or(BidRejectionReason::DimensionMismatch)?
            .resolve(),
        (None, None) => dimensions
            .unqualified
            .ok_or(BidRejectionReason::DimensionMismatch)?
            .resolve(),
    }
}

/// Result of request construction before transport.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "Ready carries the full built request by design"
)]
pub(crate) enum OpenRtbBuildOutcome {
    Ready(OpenRtbRequest),
    NoImpressions,
}

/// Explicit, deterministic signing input. No signer is loaded by this driver.
pub(crate) struct RequestFinalization<'a> {
    pub(crate) signer: Option<&'a RequestSigner>,
    pub(crate) signing_params: SigningParams,
}

/// Build one demand source's request from its immutable routed input.
///
/// # Errors
///
/// Returns an auction error when an implementation's own extensions cannot be
/// built or the supplied signing input does not bind the already-fixed request
/// ID.
pub(crate) fn build_request(
    input: &ProviderAuctionInput,
    routed: &RoutedAuction,
    provider: &ProviderPlan,
    effective_timeout_ms: u32,
    finalization: &RequestFinalization<'_>,
) -> Result<OpenRtbBuildOutcome, Report<TrustedServerError>> {
    let demand = provider.demand.as_ref();
    let policy = demand.field_policy();
    let mut request = build_common_request(input, routed, demand, policy, effective_timeout_ms);
    if request.imp.is_empty() {
        return Ok(OpenRtbBuildOutcome::NoImpressions);
    }
    demand.augment_request(&mut request, input)?;
    finalize_request(&mut request, policy, finalization)?;
    Ok(OpenRtbBuildOutcome::Ready(request))
}

fn build_common_request(
    input: &ProviderAuctionInput,
    routed: &RoutedAuction,
    demand: &dyn CompiledDemand,
    policy: DemandFieldPolicy,
    effective_timeout_ms: u32,
) -> OpenRtbRequest {
    let common = input.common_request();
    let imps = input
        .slots()
        .iter()
        .filter_map(|slot| build_imp(slot, policy))
        .collect();
    let site_domain = demand.site_domain(&common.publisher.domain);
    let page = demand.site_page(common.publisher.page_url.as_deref(), &site_domain);
    let body_consent = demand.body_consent(common.user.consent.as_ref());
    let raw_tc = body_consent.and_then(|value| value.raw_tc_string.clone());
    let user = Some(User {
        id: common.user.id.clone(),
        consent: raw_tc.clone(),
        ext: UserExt {
            consent: raw_tc,
            consented_providers_settings: policy
                .additional_consent
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
    let language = normalized_language(routed.transport_headers(), policy);
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
                lat: policy.precise_geo.then_some(geo.latitude),
                lon: policy.precise_geo.then_some(geo.longitude),
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
            r#ref: policy
                .site_ref
                .then(|| header_string(routed.transport_headers().referer()))
                .flatten(),
            publisher: Some(Publisher {
                domain: Some(site_domain),
                ..Default::default()
            }),
            ..Default::default()
        }),
        user,
        device,
        regs: build_regs(body_consent, policy.regs),
        test: policy.test.then_some(true),
        tmax: to_openrtb_i32(
            effective_timeout_ms,
            "tmax",
            "config-first provider request",
        ),
        cur: vec![DEFAULT_CURRENCY.to_string()],
        ..Default::default()
    }
}

fn build_imp(slot: &ProviderSlotInput, policy: DemandFieldPolicy) -> Option<Imp> {
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
    let primary_size = policy.primary_banner_size;
    Some(Imp {
        id: Some(slot.slot().id.clone()),
        banner: Some(Banner {
            format: formats,
            w: primary_size.then_some(first_width).flatten(),
            h: primary_size.then_some(first_height).flatten(),
            topframe: primary_size.then_some(false),
            ..Default::default()
        }),
        tagid: policy.imp_tagid.then(|| slot.slot().id.clone()),
        bidfloor: slot.slot().floor_price,
        bidfloorcur: slot
            .slot()
            .floor_price
            .map(|_| DEFAULT_CURRENCY.to_string()),
        secure: Some(true),
        ..Default::default()
    })
}

fn finalize_request(
    request: &mut OpenRtbRequest,
    policy: DemandFieldPolicy,
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
    } else if policy.unsigned_request_identity {
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
    policy: RegsPolicy,
) -> Option<Regs> {
    let consent = consent?;
    if policy == RegsPolicy::ApplicabilityBit {
        // Any admitted context produces regs and GDPR comes from the
        // applicability bit alone, without jurisdiction rules.
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

    // The jurisdiction policy sends no regs for an empty context and reads
    // GDPR from the applicability bit or from a GDPR jurisdiction.
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

fn normalized_language(headers: &TransportHeaders, policy: DemandFieldPolicy) -> Option<String> {
    let value = header_string(headers.accept_language())
        .and_then(|value| value.split(',').next().map(str::to_string))
        .and_then(|value| value.split(';').next().map(str::to_string))
        .and_then(|value| value.split('-').next().map(str::to_string))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    match policy.language_max_bytes {
        None => Some(value),
        Some(max_bytes) => (value.len() <= max_bytes).then_some(value),
    }
}

fn header_string(value: Option<&http::HeaderValue>) -> Option<String> {
    value
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
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
    let dimensions_by_slot = build_bid_dimension_index(input);
    let mut diagnostics = ResponseAdmissionDiagnostics::default();
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
            match extract_standard_bid(value, returned_seat, &dimensions_by_slot) {
                Ok(bid) => bids.push(bid),
                Err(reason) => diagnostics.record(reason),
            }
        }
    }
    let mut parsed = if bids.is_empty() {
        AuctionResponse::no_bid(provider_id, response_time_ms)
    } else {
        AuctionResponse::success(provider_id, bids, response_time_ms)
    };
    diagnostics.attach_to(&mut parsed);
    parsed
}

fn extract_standard_bid(
    value: &Value,
    returned_seat: Option<&str>,
    dimensions_by_slot: &BidDimensionIndex,
) -> Result<Bid, BidRejectionReason> {
    let slot_id = value
        .get("impid")
        .and_then(Value::as_str)
        .filter(|slot_id| !slot_id.is_empty())
        .ok_or(BidRejectionReason::InvalidBid)?
        .to_string();
    let width = parse_optional_bid_dimension(value, "w")?;
    let height = parse_optional_bid_dimension(value, "h")?;
    let (width, height) = resolve_bid_dimensions(dimensions_by_slot, &slot_id, width, height)?;
    let price = value
        .get("price")
        .and_then(Value::as_f64)
        .filter(|price| price.is_finite() && *price >= 0.0)
        .ok_or(BidRejectionReason::InvalidBid)?;
    let creative = value
        .get("adm")
        .and_then(Value::as_str)
        .filter(|creative| !creative.is_empty())
        .map(str::to_string)
        .ok_or(BidRejectionReason::InvalidBid)?;
    Ok(Bid {
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

/// Count bidder parameter objects a demand source did not consume.
#[must_use]
pub(crate) fn unused_bidder_params_count(
    demand: &dyn CompiledDemand,
    input: &ProviderAuctionInput,
) -> u32 {
    if demand.field_policy().consumes_bidder_params {
        return 0;
    }
    ignored_bidder_params_count(input)
}

/// Count routed bidder params for a demand source known to ignore them.
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

    use super::{saturating_bidder_param_counts, unused_bidder_params_count};
    use crate::auction::plan::{AuctionPlan, BidderId, BidderRouteConfig, ProviderId};
    use crate::auction::routing::route_auction;
    use crate::auction::test_support::{
        canonical_parity_auction_request, demand_table, plan_config,
    };

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
    fn unused_bidder_param_count_follows_the_implementation() {
        for (implementation, expected) in [("prebid_server", 0), ("openrtb", 1), ("aps", 1)] {
            let endpoint = if implementation == "aps" {
                "https://aps.example/e/pb/bid"
            } else {
                "https://provider.example/openrtb"
            };
            let provider_id =
                ProviderId::from_str("fictional_provider").expect("should parse provider ID");
            let mut config = plan_config(vec![(
                "fictional_provider",
                demand_table(implementation, endpoint),
            )]);
            config.bidders = BTreeMap::from([(
                BidderId::from_str("exampleBidder").expect("should parse bidder ID"),
                BidderRouteConfig {
                    provider: provider_id,
                },
            )]);
            let plan = AuctionPlan::compile(config).expect("should compile the plan");
            let inbound = http::Request::new(edgezero_core::body::Body::empty());
            let routed = route_auction(canonical_parity_auction_request(), &inbound, &plan, None);

            assert_eq!(
                unused_bidder_params_count(
                    plan.providers()[0].demand.as_ref(),
                    &routed.inputs()[0]
                ),
                expected,
                "{implementation} should report only bidder params it ignores"
            );
        }
    }
}

#[cfg(test)]
mod test_executor;
#[cfg(test)]
mod tests;
