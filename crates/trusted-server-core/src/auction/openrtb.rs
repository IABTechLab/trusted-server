//! Shared `OpenRTB` 2.6 request/response support for config-first providers.
//!
//! Profiles receive only routed, privacy-approved facts and never the raw
//! downstream request or unrestricted runtime services.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use error_stack::Report;
use serde_json::{Map, Value, json};

use super::plan::NotificationPolicy;
#[cfg(test)]
use super::plan::ProviderPlan;

use super::routing::{ProviderAuctionInput, ProviderSlotInput, RoutedAuction, TransportHeaders};
use super::types::{AdFormat, AdSlot, AuctionResponse, Bid};
use crate::error::TrustedServerError;
use crate::openrtb::{
    Banner, Device, Format, Geo, Imp, OpenRtbRequest, Publisher, Regs, RegsExt, Site, ToExt as _,
    TrustedServerExt, User, UserExt, to_openrtb_i32,
};
use crate::request_signing::{RequestSigner, SIGNING_VERSION, SigningParams};

const DEFAULT_CURRENCY: &str = "USD";
const MAX_CONSERVATIVE_LANGUAGE_BYTES: usize = 8;

/// Fixed reasons why an upstream bid failed response admission.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub enum BidRejectionReason {
    /// Required bid data is absent, malformed, or outside the admitted range.
    InvalidBid,
    /// The bid names an impression absent from this provider request.
    UnrequestedImpression,
    /// Returned dimensions cannot match a requested banner format.
    DimensionMismatch,
    /// Missing dimensions match more than one requested banner format.
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
pub struct ResponseAdmissionDiagnostics {
    rejected_bid_count: u32,
    reason_counts: BTreeMap<BidRejectionReason, u32>,
}

impl ResponseAdmissionDiagnostics {
    /// Record one rejected bid without retaining upstream payload data.
    ///
    /// Counts saturate rather than overflow.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::auction::openrtb::{BidRejectionReason, ResponseAdmissionDiagnostics};
    /// let mut diagnostics = ResponseAdmissionDiagnostics::default();
    /// diagnostics.record(BidRejectionReason::InvalidBid);
    /// ```
    pub fn record(&mut self, reason: BidRejectionReason) {
        self.rejected_bid_count = self.rejected_bid_count.saturating_add(1);
        let count = self.reason_counts.entry(reason).or_default();
        *count = count.saturating_add(1);
    }

    /// Attach fixed-cardinality rejection counts to a provider response.
    ///
    /// An empty diagnostic set leaves response metadata unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::auction::openrtb::{BidRejectionReason, ResponseAdmissionDiagnostics};
    /// use trusted_server_core::auction::AuctionResponse;
    /// let mut diagnostics = ResponseAdmissionDiagnostics::default();
    /// diagnostics.record(BidRejectionReason::InvalidBid);
    /// let mut response = AuctionResponse::no_bid("example-provider", 1);
    /// diagnostics.attach_to(&mut response);
    /// assert_eq!(response.metadata["response_admission"]["rejected_bid_count"], 1);
    /// ```
    pub fn attach_to(self, response: &mut AuctionResponse) {
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
pub struct SlotBidDimensions {
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
pub type BidDimensionIndex = BTreeMap<String, SlotBidDimensions>;

/// Build the requested-dimension index once for one provider response.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::openrtb::{BidDimensionIndex, build_bid_dimension_index};
/// use trusted_server_core::auction::routing::ProviderAuctionInput;
/// fn response_index(input: &ProviderAuctionInput) -> BidDimensionIndex {
///     build_bid_dimension_index(input)
/// }
/// ```
pub fn build_bid_dimension_index(input: &ProviderAuctionInput) -> BidDimensionIndex {
    build_bid_dimension_index_from_slots(input.slots().iter().map(ProviderSlotInput::slot))
}

/// Build the requested-dimension index from plain [`AdSlot`]s.
///
/// The first slot wins when several share an ID.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::openrtb::build_bid_dimension_index_from_slots;
/// use trusted_server_core::auction::types::AdSlot;
/// let slots: Vec<AdSlot> = Vec::new();
/// assert!(build_bid_dimension_index_from_slots(&slots).is_empty());
/// ```
pub fn build_bid_dimension_index_from_slots<'a>(
    slots: impl IntoIterator<Item = &'a AdSlot>,
) -> BidDimensionIndex {
    let mut index = BidDimensionIndex::new();
    for slot in slots {
        index
            .entry(slot.id.clone())
            .or_insert_with(|| SlotBidDimensions::from_formats(slot.formats.as_slice()));
    }
    index
}

/// Parse an optional positive `OpenRTB` bid dimension.
///
/// Missing or null dimensions remain absent for later inference.
///
/// # Errors
///
/// Returns [`BidRejectionReason::InvalidBid`] for non-positive, fractional,
/// nonnumeric, or out-of-range dimensions.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::openrtb::parse_optional_bid_dimension;
/// assert_eq!(parse_optional_bid_dimension(&serde_json::json!({"w": 300}), "w"), Ok(Some(300)));
/// ```
pub fn parse_optional_bid_dimension(
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
///
/// # Errors
///
/// Returns [`BidRejectionReason::UnrequestedImpression`] for an unknown slot,
/// [`BidRejectionReason::DimensionMismatch`] for no matching format, or
/// [`BidRejectionReason::AmbiguousDimensions`] when inference is not unique.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::openrtb::{BidDimensionIndex, BidRejectionReason, resolve_bid_dimensions};
/// let index = BidDimensionIndex::new();
/// assert_eq!(resolve_bid_dimensions(&index, "slot", Some(300), Some(250)),
///     Err(BidRejectionReason::UnrequestedImpression));
/// ```
pub fn resolve_bid_dimensions(
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
pub enum OpenRtbBuildOutcome {
    /// Finalized `OpenRTB` envelope ready for HTTP serialization.
    Ready(OpenRtbRequest),
    /// No usable impressions remain after routing and specialization.
    NoImpressions,
}

/// Explicit, deterministic signing input. No signer is loaded by this driver.
pub struct RequestFinalization<'a> {
    /// Optional auction-local signer, loaded by the caller.
    pub signer: Option<&'a RequestSigner>,
    /// Fixed request identity and timestamp to bind during finalization.
    pub signing_params: SigningParams,
}

/// Build one provider request from its immutable routed input.
///
/// # Errors
///
/// Returns an auction error when static/profile extensions cannot be merged or
/// the supplied signing input does not bind the already-fixed request ID.
#[cfg(test)]
pub(crate) fn build_request(
    input: &ProviderAuctionInput,
    routed: &RoutedAuction,
    provider: &ProviderPlan,
    effective_timeout_ms: u32,
    finalization: &RequestFinalization<'_>,
) -> Result<OpenRtbBuildOutcome, Report<TrustedServerError>> {
    super::profile::build_profile_request(
        provider.profile.as_ref(),
        input,
        routed,
        effective_timeout_ms,
        finalization,
    )
}

pub(crate) fn build_common_request(
    input: &ProviderAuctionInput,
    routed: &RoutedAuction,
    effective_timeout_ms: u32,
) -> OpenRtbRequest {
    let common = input.common_request();
    let imps = input.slots().iter().filter_map(build_imp).collect();
    let site_domain = common.publisher.domain.clone();
    let page = common.publisher.page_url.clone();
    let body_consent = common.user.consent.as_ref();
    let raw_tc = body_consent.and_then(|value| value.raw_tc_string.clone());
    let user = Some(User {
        id: common.user.id.clone(),
        consent: raw_tc.clone(),
        ext: UserExt {
            consent: raw_tc,
            consented_providers_settings: None,
            eids: common.user.eids.clone(),
        }
        .to_ext(),
        ..Default::default()
    });
    let language = normalized_language(routed.transport_headers(), true);
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
                lat: None,
                lon: None,
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
            r#ref: None,
            publisher: Some(Publisher {
                domain: Some(site_domain),
                ..Default::default()
            }),
            ..Default::default()
        }),
        user,
        device,
        regs: build_regs(body_consent, false),
        test: None,
        tmax: to_openrtb_i32(
            effective_timeout_ms,
            "tmax",
            "config-first provider request",
        ),
        cur: vec![DEFAULT_CURRENCY.to_string()],
        ..Default::default()
    }
}

fn build_imp(slot: &ProviderSlotInput) -> Option<Imp> {
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
    formats.first()?;
    Some(Imp {
        id: Some(slot.slot().id.clone()),
        banner: Some(Banner {
            format: formats,
            w: None,
            h: None,
            topframe: None,
            ..Default::default()
        }),
        tagid: None,
        bidfloor: slot.slot().floor_price,
        bidfloorcur: slot
            .slot()
            .floor_price
            .map(|_| DEFAULT_CURRENCY.to_string()),
        secure: Some(true),
        ..Default::default()
    })
}

pub(crate) fn finalize_request(
    request: &mut OpenRtbRequest,
    retains_unsigned_identity: bool,
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
    } else if retains_unsigned_identity {
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

/// Build consent regulation fields under an explicit empty-context policy.
///
/// `retain_empty_context` preserves every admitted consent context and uses only
/// its GDPR applicability bit. Otherwise empty contexts are omitted and GDPR
/// follows the existing jurisdiction-aware inference.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::openrtb::build_regs;
/// assert!(build_regs(None, false).is_none());
/// ```
pub fn build_regs(
    consent: Option<&crate::consent::ConsentContext>,
    retain_empty_context: bool,
) -> Option<Regs> {
    let consent = consent?;
    if retain_empty_context {
        // This wire policy retains every admitted consent context and derives
        // GDPR only from applicability, without jurisdiction inference.
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

    // Conservative consent omits empty contexts and retains jurisdiction-aware
    // GDPR inference.
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

/// Normalize the first language range to its primary language subtag.
///
/// The conservative policy omits subtags longer than eight bytes.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::{openrtb::normalized_language, routing::TransportHeaders};
/// assert!(normalized_language(&TransportHeaders::default(), true).is_none());
/// ```
pub fn normalized_language(headers: &TransportHeaders, conservative: bool) -> Option<String> {
    let value = header_string(headers.accept_language())
        .and_then(|value| value.split(',').next().map(str::to_string))
        .and_then(|value| value.split(';').next().map(str::to_string))
        .and_then(|value| value.split('-').next().map(str::to_string))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    (!conservative || value.len() <= MAX_CONSERVATIVE_LANGUAGE_BYTES).then_some(value)
}

/// Copy a UTF-8 transport header, omitting non-UTF-8 values.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::openrtb::header_string;
/// let value = http::HeaderValue::from_static("en");
/// assert_eq!(header_string(Some(&value)).as_deref(), Some("en"));
/// ```
pub fn header_string(value: Option<&http::HeaderValue>) -> Option<String> {
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

/// Count bidder parameter objects a profile did not consume.
#[must_use]
pub(crate) fn unused_bidder_params_count(
    profile: &std::sync::Arc<dyn super::profile::CompiledOpenRtbProfile>,
    input: &ProviderAuctionInput,
) -> u32 {
    if profile.routing_policy().consumes_bidder_params {
        return 0;
    }
    ignored_bidder_params_count(input)
}

/// Count routed bidder parameter objects for a profile that ignores them.
///
/// Counts saturate at the diagnostic field's maximum value.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::{openrtb::ignored_bidder_params_count, routing::ProviderAuctionInput};
/// fn ignored(input: &ProviderAuctionInput) -> u32 { ignored_bidder_params_count(input) }
/// ```
#[must_use]
pub fn ignored_bidder_params_count(input: &ProviderAuctionInput) -> u32 {
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
