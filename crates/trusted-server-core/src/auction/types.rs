//! Core types for auction requests and responses.

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD},
};
use edgezero_core::body::Body as EdgeBody;
use http::Request;
use rand::{RngCore as _, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use url::Url;

use crate::auction::context::ContextValue;
use crate::geo::GeoInfo;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Injectable CSPRNG boundary for server-minted response-local identities.
pub(crate) trait AuctionIdentityGenerator: Send + Sync {
    /// Fill the complete destination or report that secure randomness is unavailable.
    fn fill(&self, destination: &mut [u8]) -> Result<(), ()>;
}

/// Production CSPRNG for server-minted auction identities.
pub(crate) struct SystemAuctionIdentityGenerator;

impl AuctionIdentityGenerator for SystemAuctionIdentityGenerator {
    fn fill(&self, destination: &mut [u8]) -> Result<(), ()> {
        OsRng.try_fill_bytes(destination).map_err(|_| ())
    }
}

/// Mint one response-unique unpadded base64url identity.
pub(crate) fn mint_response_unique_base64url_identity(
    generator: &dyn AuctionIdentityGenerator,
    issued: &mut HashSet<String>,
    prefix: &str,
    random_byte_count: usize,
    collision_retries: usize,
) -> Option<String> {
    for _ in 0..=collision_retries {
        let mut bytes = vec![0_u8; random_byte_count];
        if generator.fill(&mut bytes).is_err() {
            return None;
        }
        let identity = format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes));
        if issued.insert(identity.clone()) {
            return Some(identity);
        }
    }
    None
}

/// Represents a unified auction request across all providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuctionRequest {
    /// Unique auction ID
    pub id: String,
    /// Ad slots/impressions being auctioned
    pub slots: Vec<AdSlot>,
    /// Publisher information
    pub publisher: PublisherInfo,
    /// User information (privacy-preserving)
    pub user: UserInfo,
    /// Device information
    pub device: Option<DeviceInfo>,
    /// Site information
    pub site: Option<SiteInfo>,
    /// Additional context forwarded from the JS client payload.
    pub context: HashMap<String, ContextValue>,
}

/// Represents a single ad slot/impression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdSlot {
    /// Slot identifier (e.g., "header-banner")
    pub id: String,
    /// Media types and formats supported
    pub formats: Vec<AdFormat>,
    /// Floor price if any
    pub floor_price: Option<f64>,
    /// Slot-specific targeting
    pub targeting: HashMap<String, serde_json::Value>,
    /// Bidder configurations (bidder name -> params)
    pub bidders: HashMap<String, serde_json::Value>,
}

/// Ad format specification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdFormat {
    pub media_type: MediaType,
    pub width: u32,
    pub height: u32,
}

/// Media type enumeration.
///
/// `Default` is `Banner` for programmatic construction only. Do **not** add
/// `#[serde(default)]` to any field of this type: it would coerce an
/// unknown/missing media type to `Banner` rather than failing, silently
/// mis-typing video/native slots. Deserialization must stay strict.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    #[default]
    Banner,
    Video,
    Native,
}

/// Publisher information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublisherInfo {
    pub domain: String,
    pub page_url: Option<String>,
}

/// Privacy-preserving user information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    /// Stable EC ID (from cookie or freshly generated).
    /// `None` when EC is unavailable or consent denies it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Decoded consent context for this request.
    ///
    /// Carries both raw consent strings (for `OpenRTB` forwarding) and decoded
    /// structured data (for TS-level enforcement and observability).
    /// Skipped during serde since it is populated at runtime from request
    /// cookies/headers, not from stored data.
    #[serde(skip)]
    pub consent: Option<crate::consent::ConsentContext>,
    /// Extended User IDs parsed from the [`crate::constants::COOKIE_TS_EIDS`] cookie.
    ///
    /// Raw (un-gated) values from the browser; consent gating via
    /// [`crate::consent::gate_eids_by_consent`] is applied centrally in the
    /// endpoint handlers (the auction and page-bids paths) before any EID
    /// reaches a bid request — the provider layer just forwards already-gated
    /// EIDs.
    #[serde(skip)]
    pub eids: Option<Vec<crate::openrtb::Eid>>,
}

/// Device information from request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub geo: Option<GeoInfo>,
}

/// Site information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteInfo {
    pub domain: String,
    pub page: String,
}

/// Context passed to auction providers.
///
/// # The `request` field is path-dependent
///
/// `request` carries the **real downstream client request** in the dispatch
/// path ([`AuctionOrchestrator::run_auction`][run] and
/// [`dispatch_auction`][dispatch]). Providers there can read client headers
/// (DNT, User-Agent, cookies, X-* customs) directly off it.
///
/// In the **collect path** ([`collect_dispatched_auction`][collect]) the
/// mediator is invoked with a synthetic placeholder request
/// (`https://placeholder.invalid/`), because the real client request has
/// already been consumed by `send_async` during dispatch and the host pipeline
/// can't lend it across the `.await`. **Mediators must not depend on reading
/// client state from `context.request`** — the placeholder has none of the
/// real headers. If a future mediator needs that data, snapshot it into a new
/// field on this struct at dispatch time and stash it on the
/// [`DispatchedAuction`] token so collect can attach it to the mediator's
/// context. See <https://github.com/IABTechLab/trusted-server/issues/680>
/// (P2-1) for the open follow-up.
///
/// [run]: crate::auction::AuctionOrchestrator::run_auction
/// [dispatch]: crate::auction::AuctionOrchestrator::dispatch_auction
/// [collect]: crate::auction::AuctionOrchestrator::collect_dispatched_auction
pub struct AuctionContext<'a> {
    pub settings: &'a Settings,
    pub request: &'a Request<EdgeBody>,
    pub timeout_ms: u32,
    /// Provider responses from the bidding phase, used by mediators.
    /// This is `None` for regular bidders and `Some` when calling a mediator.
    pub provider_responses: Option<&'a [AuctionResponse]>,
    /// Platform services (config store, secret store, etc.) for use by providers.
    pub services: &'a RuntimeServices,
}

/// Closed, local reason set for rejecting provider bids or undeliverable winners.
///
/// These values are serialized only into existing auction debug/diagnostic
/// surfaces. They are not a persistence or external-event taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionDropReason {
    /// Configured processing rejected an ordinary creative's only render source.
    CreativeProcessingRejected,
    /// Optional creative ID is present with an invalid type or value.
    InvalidCreativeId,
    /// Optional creative ID exceeds its UTF-8 byte bound.
    CreativeIdTooLarge,
    /// A positive integral dimension exceeds the supported range.
    DimensionsOutOfRange,
    /// An otherwise valid upstream bid ID is repeated in one provider response.
    DuplicateUpstreamBidId,
    /// A response contains no seat bids.
    #[serde(rename = "empty_seatbid")]
    EmptySeatBid,
    /// A seat bid contains no usable bid array.
    #[serde(rename = "empty_seatbid_bids")]
    EmptySeatBidBids,
    /// A creative URL is malformed, unsafe, or self-origin.
    InvalidCreativeUrl,
    /// A dimension is missing, malformed, nonpositive, or not requested.
    InvalidDimensions,
    /// A price is missing, malformed, nonfinite, or negative.
    InvalidPrice,
    /// The provider response violates the response-level contract.
    InvalidProviderResponse,
    /// The APS tag type is missing or unsupported.
    InvalidTagType,
    /// An upstream bid ID contains a forbidden control value or has the wrong type.
    InvalidUpstreamBidId,
    /// A valid sibling was preferred by deterministic per-slot reduction.
    LostToHigherBid,
    /// A provider bid is not an object.
    MalformedBid,
    /// APS creative metadata does not contain `creativeurl`.
    MissingCreativeUrl,
    /// Provider parsing was invoked without its request-local context.
    MissingRequestContext,
    /// A required upstream bid ID is absent or empty.
    MissingUpstreamBidId,
    /// A winner carries more than one render source.
    MultipleRenderSources,
    /// A winner has no render source.
    NoRenderSource,
    /// A typed renderer extension could not be serialized.
    RendererExtensionSerializationFailed,
    /// A validated renderer projection exceeds its bound.
    RenderPayloadTooLarge,
    /// APS script rendering is disabled by configuration.
    ScriptRenderingDisabled,
    /// A provider bid references an impression that was not dispatched.
    UnknownImpression,
    /// A provider bid declares a non-banner media type.
    UnsupportedMediaType,
    /// An upstream bid ID exceeds 64 UTF-8 bytes.
    UpstreamBidIdTooLarge,
}

impl AuctionDropReason {
    /// Return the exact existing debug/projection literal.
    ///
    /// This hand-written mapping also drives [`Ord`] so serialized-map output stays
    /// alphabetically stable even when declaration order changes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreativeProcessingRejected => "creative_processing_rejected",
            Self::InvalidCreativeId => "invalid_creative_id",
            Self::CreativeIdTooLarge => "creative_id_too_large",
            Self::DimensionsOutOfRange => "dimensions_out_of_range",
            Self::DuplicateUpstreamBidId => "duplicate_upstream_bid_id",
            Self::EmptySeatBid => "empty_seatbid",
            Self::EmptySeatBidBids => "empty_seatbid_bids",
            Self::InvalidCreativeUrl => "invalid_creative_url",
            Self::InvalidDimensions => "invalid_dimensions",
            Self::InvalidPrice => "invalid_price",
            Self::InvalidProviderResponse => "invalid_provider_response",
            Self::InvalidTagType => "invalid_tag_type",
            Self::InvalidUpstreamBidId => "invalid_upstream_bid_id",
            Self::LostToHigherBid => "lost_to_higher_bid",
            Self::MalformedBid => "malformed_bid",
            Self::MissingCreativeUrl => "missing_creative_url",
            Self::MissingRequestContext => "missing_request_context",
            Self::MissingUpstreamBidId => "missing_upstream_bid_id",
            Self::MultipleRenderSources => "multiple_render_sources",
            Self::NoRenderSource => "no_render_source",
            Self::RendererExtensionSerializationFailed => "renderer_extension_serialization_failed",
            Self::RenderPayloadTooLarge => "render_payload_too_large",
            Self::ScriptRenderingDisabled => "script_rendering_disabled",
            Self::UnknownImpression => "unknown_impression",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::UpstreamBidIdTooLarge => "upstream_bid_id_too_large",
        }
    }
}

impl Ord for AuctionDropReason {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for AuctionDropReason {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Typed counts projected into the existing `drop_reasons` debug object.
pub type AuctionDropReasons = BTreeMap<AuctionDropReason, u64>;

/// Increment one typed local drop reason.
pub(crate) fn record_auction_drop(reasons: &mut AuctionDropReasons, reason: AuctionDropReason) {
    *reasons.entry(reason).or_default() += 1;
}

/// Closed failure set for one requested slot's server-auction decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionSlotFailureReason {
    /// The auction orchestrator is disabled.
    AuctionDisabled,
    /// Request consent does not permit a server-side auction.
    ConsentDenied,
    /// No enabled configured provider can bid on the slot.
    SlotNotEligible,
    /// A dispatched provider exceeded its deadline.
    ProviderTimeout,
    /// A provider could not launch or complete its transport/HTTP exchange.
    ProviderError,
    /// A provider response failed structural, currency, identity, or bid validation.
    InvalidProviderResponse,
    /// The configured mediator failed or returned invalid provenance.
    MediationFailed,
    /// A selected candidate cannot be represented by the exact browser contract.
    WinnerNotRenderable,
    /// A unique renderer reservation could not be minted.
    IdentityGenerationFailed,
    /// An internal invariant or candidate-identity operation failed.
    InternalError,
}

impl AuctionSlotFailureReason {
    /// Return the exact wire literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuctionDisabled => "auction_disabled",
            Self::ConsentDenied => "consent_denied",
            Self::SlotNotEligible => "slot_not_eligible",
            Self::ProviderTimeout => "provider_timeout",
            Self::ProviderError => "provider_error",
            Self::InvalidProviderResponse => "invalid_provider_response",
            Self::MediationFailed => "mediation_failed",
            Self::WinnerNotRenderable => "winner_not_renderable",
            Self::IdentityGenerationFailed => "identity_generation_failed",
            Self::InternalError => "internal_error",
        }
    }

    /// Closed multi-provider aggregation priority; lower values win.
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::InternalError => 0,
            Self::MediationFailed => 1,
            Self::InvalidProviderResponse => 2,
            Self::ProviderError => 3,
            Self::ProviderTimeout => 4,
            Self::ConsentDenied => 5,
            Self::AuctionDisabled => 6,
            Self::SlotNotEligible => 7,
            Self::WinnerNotRenderable | Self::IdentityGenerationFailed => u8::MAX,
        }
    }
}

/// Exactly one final server-auction decision for a requested slot.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "outcome",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SlotAuctionDecisionV1 {
    /// A candidate won and joins exactly one projected bid.
    Winner {
        /// Exact request slot identifier.
        slot: String,
        /// Opaque response-local candidate identifier.
        candidate_id: String,
    },
    /// Every dispatched provider completed successfully without a candidate.
    NoBid {
        /// Exact request slot identifier.
        slot: String,
    },
    /// The slot failed with one closed reason.
    Failed {
        /// Exact request slot identifier.
        slot: String,
        /// Exact failure reason.
        reason: AuctionSlotFailureReason,
    },
}

impl SlotAuctionDecisionV1 {
    /// Return the exact slot identifier shared by every variant.
    #[must_use]
    pub fn slot(&self) -> &str {
        match self {
            Self::Winner { slot, .. } | Self::NoBid { slot } | Self::Failed { slot, .. } => slot,
        }
    }
}

impl Serialize for SlotAuctionDecisionV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        match self {
            Self::Winner { slot, candidate_id } => {
                let mut state = serializer.serialize_struct("SlotAuctionDecisionV1", 3)?;
                state.serialize_field("slot", slot)?;
                state.serialize_field("outcome", "winner")?;
                state.serialize_field("candidateId", candidate_id)?;
                state.end()
            }
            Self::NoBid { slot } => {
                let mut state = serializer.serialize_struct("SlotAuctionDecisionV1", 2)?;
                state.serialize_field("slot", slot)?;
                state.serialize_field("outcome", "no_bid")?;
                state.end()
            }
            Self::Failed { slot, reason } => {
                let mut state = serializer.serialize_struct("SlotAuctionDecisionV1", 3)?;
                state.serialize_field("slot", slot)?;
                state.serialize_field("outcome", "failed")?;
                state.serialize_field("reason", reason)?;
                state.end()
            }
        }
    }
}

/// Ordered version-1 decision set for one server auction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuctionDecisionSetV1 {
    /// Contract version.
    pub version: u8,
    /// Exact auction identifier.
    pub auction_id: String,
    /// Exactly one decision per requested slot, in request order.
    pub results: Vec<SlotAuctionDecisionV1>,
}

impl AuctionDecisionSetV1 {
    /// Construct an ordered decision set for a request-wide gate.
    #[must_use]
    pub fn failed(request: &AuctionRequest, reason: AuctionSlotFailureReason) -> Self {
        Self {
            version: 1,
            auction_id: request.id.clone(),
            results: request
                .slots
                .iter()
                .map(|slot| SlotAuctionDecisionV1::Failed {
                    slot: slot.id.clone(),
                    reason,
                })
                .collect(),
        }
    }
}

/// Maximum canonical UTF-8 size of the browser auction projection.
pub const MAX_BROWSER_AUCTION_PROJECTION_BYTES: usize = 8 * 1024 * 1024;
/// Maximum number of requested results or projected winner bids.
pub const MAX_BROWSER_AUCTION_RESULTS: usize = 256;
/// Maximum number of publisher targeting entries on one projected bid.
pub const MAX_BROWSER_AUCTION_TARGETING_ENTRIES: usize = 32;

/// One exact browser-facing winner projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserAuctionBidV1 {
    /// Response-local mediator candidate identity.
    pub candidate_id: String,
    /// Exact requested server slot identity.
    pub slot: String,
    /// Canonical provider integration name.
    pub provider: String,
    /// Exact provider-native upstream bid identity.
    pub upstream_bid_id: String,
    /// Selected finite, nonnegative CPM.
    pub cpm: f64,
    /// Exact auction currency; version 1 admits only `USD`.
    pub currency: String,
    /// Lexically ordered publisher targeting, excluding runtime-owned `hb_adid`.
    pub targeting: BTreeMap<String, String>,
    /// Server-minted renderer capability identity for APS/ADM only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renderer_reservation_id: Option<String>,
    /// Sole tagged render authority for the winner.
    pub render_source: BidRenderSourceV1,
}

/// Exact GAM placement metadata required to publish one server-projected slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserAuctionSlotV1 {
    /// Exact server slot identity joined to one auction decision.
    pub slot: String,
    /// Fully rendered GAM ad-unit path for this navigation.
    pub gam_unit_path: String,
    /// Stable configured DOM id/prefix for responsive resolution.
    pub div_id: String,
    /// Accepted banner dimensions in configured order.
    pub formats: Vec<[u32; 2]>,
    /// Static publisher targeting applied before winner targeting.
    pub targeting: BTreeMap<String, String>,
}

/// Complete browser-facing version-1 auction projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserAuctionProjectionV1 {
    /// Contract version.
    pub version: u8,
    /// Ordered decision set for every requested slot.
    pub auction: AuctionDecisionSetV1,
    /// Ordered GAM placement definitions; empty only for direct `/auction` serialization.
    pub slots: Vec<BrowserAuctionSlotV1>,
    /// Winner bids in matching decision order.
    pub bids: Vec<BrowserAuctionBidV1>,
}

/// URL used by the orchestrator when invoking a mediator from the collect
/// path. Providers can `debug_assert` against this value to catch a mediator
/// that has accidentally started depending on `context.request` carrying real
/// client headers.
pub const MEDIATOR_PLACEHOLDER_URL: &str = "https://placeholder.invalid/";

/// Response from a single auction provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuctionResponse {
    /// Provider that generated this response
    pub provider: String,
    /// Bids returned
    pub bids: Vec<Bid>,
    /// Status of the auction
    pub status: BidStatus,
    /// Response time in milliseconds
    pub response_time_ms: u64,
    /// Provider-specific metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

/// APS creative tag type accepted by the Trusted Server renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApsTagType {
    /// APS loads the creative URL in a nested iframe.
    Iframe,
    /// APS fetches creative HTML and executes it in its nested renderer frame.
    Script,
}

/// Version 1 APS renderer descriptor shared with browser clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApsRendererV1 {
    /// Renderer contract version.
    pub version: u8,
    /// APS account identifier used to initialize the fixed runner.
    pub account_id: String,
    /// Selected `OpenRTB` bid identifier.
    pub bid_id: String,
    /// Optional `OpenRTB` creative identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creative_id: Option<String>,
    /// APS creative delivery mode.
    pub tag_type: ApsTagType,
    /// HTTPS creative URL consumed by the fixed APS runner.
    pub creative_url: String,
    /// Base64-encoded exact one-bid APS response envelope.
    pub aax_response: String,
    /// Creative width.
    pub width: u32,
    /// Creative height.
    pub height: u32,
}

/// Version 1 inline ADM render source shared with browser clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmRenderSourceV1 {
    /// Render-source contract version.
    pub version: u8,
    /// Exact creative markup.
    pub adm: String,
    /// Creative width.
    pub width: u32,
    /// Creative height.
    pub height: u32,
}

/// Thin version 1 carrier for the current GPT-owned PBS Cache behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaselinePbsCacheSourceV1 {
    /// Render-source contract version.
    pub version: u8,
    /// Exact native PBS Cache identity.
    pub cache_id: String,
    /// Exact current-main `hb_cache_host` value.
    pub cache_host: String,
    /// Exact current-main `hb_cache_path` value.
    pub cache_path: String,
    /// Winning width transported without cache-specific validation.
    pub width: u32,
    /// Winning height transported without cache-specific validation.
    pub height: u32,
}

/// Typed browser render source carried by a bid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BidRenderSourceV1 {
    /// APS renderer version 1.
    Aps(ApsRendererV1),
    /// Inline ADM version 1.
    Adm(AdmRenderSourceV1),
    /// Current-main GPT-owned PBS Cache carrier.
    PbsCache(BaselinePbsCacheSourceV1),
}

impl BidRenderSourceV1 {
    /// Return the APS renderer descriptor when this is an APS renderer.
    #[must_use]
    pub fn as_aps(&self) -> Option<&ApsRendererV1> {
        match self {
            Self::Aps(renderer) => Some(renderer),
            Self::Adm(_) | Self::PbsCache(_) => None,
        }
    }
}

/// Smallest accepted renderer dimension in CSS pixels.
pub const RENDER_DIMENSION_MIN: u64 = 1;
/// Largest accepted renderer dimension in CSS pixels.
pub const RENDER_DIMENSION_MAX: u64 = 4096;

const MAX_APS_ACCOUNT_ID_BYTES: usize = 1024;
const MAX_APS_BID_ID_BYTES: usize = 64;
const MAX_APS_CREATIVE_ID_BYTES: usize = 1024;
const MAX_APS_CREATIVE_URL_BYTES: usize = 4096;
const MAX_APS_RENDER_ENVELOPE_BYTES: usize = 256 * 1024;
const MAX_APS_RENDER_ENVELOPE_BASE64_BYTES: usize = 4 * MAX_APS_RENDER_ENVELOPE_BYTES.div_ceil(3);

/// Cross-language APS descriptor validation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApsRendererValidationResult {
    /// Descriptor and decoded envelope are valid and agree.
    Accepted,
    /// Descriptor or decoded envelope is malformed.
    DescriptorInvalid,
    /// A dimension has the wrong type or is nonfinite, fractional, zero, or negative.
    InvalidDimensions,
    /// An otherwise integral positive dimension is outside the supported range.
    DimensionsOutOfRange,
}

impl ApsRendererValidationResult {
    /// Return the exact browser failure/result literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::DescriptorInvalid => "descriptor_invalid",
            Self::InvalidDimensions => "invalid_dimensions",
            Self::DimensionsOutOfRange => "dimensions_out_of_range",
        }
    }
}

fn has_exact_json_keys(value: &serde_json::Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    })
}

fn classify_render_dimension(value: &serde_json::Value) -> ApsRendererValidationResult {
    let Some(number) = value.as_f64() else {
        return ApsRendererValidationResult::InvalidDimensions;
    };
    if !number.is_finite() || number.fract() != 0.0 || number <= 0.0 {
        return ApsRendererValidationResult::InvalidDimensions;
    }
    if number < RENDER_DIMENSION_MIN as f64 || number > RENDER_DIMENSION_MAX as f64 {
        return ApsRendererValidationResult::DimensionsOutOfRange;
    }
    ApsRendererValidationResult::Accepted
}

fn valid_aps_creative_url(value: &str, publisher_origin: &str) -> bool {
    if value.len() > MAX_APS_CREATIVE_URL_BYTES {
        return false;
    }
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.origin().ascii_serialization() != publisher_origin
}

/// Classify a raw APS renderer descriptor using the cross-language version-1 contract.
#[must_use]
pub fn classify_aps_renderer_v1(
    value: &serde_json::Value,
    publisher_origin: &str,
) -> ApsRendererValidationResult {
    const REQUIRED_KEYS: &[&str] = &[
        "aaxResponse",
        "accountId",
        "bidId",
        "creativeUrl",
        "height",
        "tagType",
        "type",
        "version",
        "width",
    ];
    const KEYS_WITH_CREATIVE_ID: &[&str] = &[
        "aaxResponse",
        "accountId",
        "bidId",
        "creativeId",
        "creativeUrl",
        "height",
        "tagType",
        "type",
        "version",
        "width",
    ];

    if !has_exact_json_keys(value, REQUIRED_KEYS)
        && !has_exact_json_keys(value, KEYS_WITH_CREATIVE_ID)
    {
        return ApsRendererValidationResult::DescriptorInvalid;
    }

    let Some(descriptor) = value.as_object() else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    if descriptor.get("type").and_then(serde_json::Value::as_str) != Some("aps")
        || descriptor
            .get("version")
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|version| version != 1.0)
    {
        return ApsRendererValidationResult::DescriptorInvalid;
    }

    let Some(account_id) = descriptor
        .get("accountId")
        .and_then(serde_json::Value::as_str)
    else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    let Some(bid_id) = descriptor.get("bidId").and_then(serde_json::Value::as_str) else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    if account_id.is_empty()
        || account_id.len() > MAX_APS_ACCOUNT_ID_BYTES
        || bid_id.is_empty()
        || bid_id.len() > MAX_APS_BID_ID_BYTES
        || bid_id.bytes().any(|byte| byte <= 0x1f || byte == 0x7f)
    {
        return ApsRendererValidationResult::DescriptorInvalid;
    }
    if let Some(creative_id) = descriptor.get("creativeId") {
        let Some(creative_id) = creative_id.as_str() else {
            return ApsRendererValidationResult::DescriptorInvalid;
        };
        if creative_id.is_empty() || creative_id.len() > MAX_APS_CREATIVE_ID_BYTES {
            return ApsRendererValidationResult::DescriptorInvalid;
        }
    }
    let Some(tag_type) = descriptor
        .get("tagType")
        .and_then(serde_json::Value::as_str)
    else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    if tag_type != "iframe" && tag_type != "script" {
        return ApsRendererValidationResult::DescriptorInvalid;
    }

    let width_result =
        classify_render_dimension(descriptor.get("width").unwrap_or(&serde_json::Value::Null));
    if width_result != ApsRendererValidationResult::Accepted {
        return width_result;
    }
    let height_result =
        classify_render_dimension(descriptor.get("height").unwrap_or(&serde_json::Value::Null));
    if height_result != ApsRendererValidationResult::Accepted {
        return height_result;
    }

    let Some(creative_url) = descriptor
        .get("creativeUrl")
        .and_then(serde_json::Value::as_str)
    else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    let Some(aax_response) = descriptor
        .get("aaxResponse")
        .and_then(serde_json::Value::as_str)
    else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    if !valid_aps_creative_url(creative_url, publisher_origin)
        || aax_response.is_empty()
        || aax_response.len() > MAX_APS_RENDER_ENVELOPE_BASE64_BYTES
    {
        return ApsRendererValidationResult::DescriptorInvalid;
    }
    let Ok(decoded_bytes) = BASE64_STANDARD.decode(aax_response) else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    if decoded_bytes.len() > MAX_APS_RENDER_ENVELOPE_BYTES
        || BASE64_STANDARD.encode(&decoded_bytes) != aax_response
    {
        return ApsRendererValidationResult::DescriptorInvalid;
    }
    let Ok(decoded_utf8) = core::str::from_utf8(&decoded_bytes) else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    let Ok(decoded) = serde_json::from_str::<serde_json::Value>(decoded_utf8) else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    if !has_exact_json_keys(&decoded, &["seatbid"]) {
        return ApsRendererValidationResult::DescriptorInvalid;
    }
    let Some(seats) = decoded.get("seatbid").and_then(serde_json::Value::as_array) else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    if seats.len() != 1 || !has_exact_json_keys(&seats[0], &["bid"]) {
        return ApsRendererValidationResult::DescriptorInvalid;
    }
    let Some(bids) = seats[0].get("bid").and_then(serde_json::Value::as_array) else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    if bids.len() != 1 || !has_exact_json_keys(&bids[0], &["ext", "h", "id", "price", "w"]) {
        return ApsRendererValidationResult::DescriptorInvalid;
    }
    let bid = &bids[0];
    let Some(ext) = bid.get("ext") else {
        return ApsRendererValidationResult::DescriptorInvalid;
    };
    if !has_exact_json_keys(ext, &["creativeurl", "tagtype"]) {
        return ApsRendererValidationResult::DescriptorInvalid;
    }

    let bid_width_result =
        classify_render_dimension(bid.get("w").unwrap_or(&serde_json::Value::Null));
    if bid_width_result != ApsRendererValidationResult::Accepted {
        return bid_width_result;
    }
    let bid_height_result =
        classify_render_dimension(bid.get("h").unwrap_or(&serde_json::Value::Null));
    if bid_height_result != ApsRendererValidationResult::Accepted {
        return bid_height_result;
    }
    let price_is_valid = bid
        .get("price")
        .and_then(serde_json::Value::as_f64)
        .is_some_and(|price| price.is_finite() && price >= 0.0);
    if bid.get("id").and_then(serde_json::Value::as_str) != Some(bid_id)
        || bid.get("w").and_then(serde_json::Value::as_f64)
            != descriptor.get("width").and_then(serde_json::Value::as_f64)
        || bid.get("h").and_then(serde_json::Value::as_f64)
            != descriptor.get("height").and_then(serde_json::Value::as_f64)
        || ext.get("creativeurl").and_then(serde_json::Value::as_str) != Some(creative_url)
        || ext.get("tagtype").and_then(serde_json::Value::as_str) != Some(tag_type)
        || !price_is_valid
    {
        return ApsRendererValidationResult::DescriptorInvalid;
    }

    ApsRendererValidationResult::Accepted
}

/// Individual bid from a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bid {
    /// Slot this bid is for
    pub slot_id: String,
    /// Server-minted opaque identifier used only for this auction response.
    #[serde(skip)]
    pub candidate_id: Option<String>,
    /// Provider integration name paired with the upstream bid ID for provenance.
    #[serde(skip)]
    pub candidate_provider: Option<String>,
    /// Server-minted renderer capability identifier (populated during projection).
    #[serde(skip)]
    pub renderer_reservation_id: Option<String>,
    /// Bid price in CPM.
    pub price: Option<f64>,
    /// Currency code (e.g., "USD")
    pub currency: String,
    /// Creative markup (HTML/VAST).
    ///
    /// `None` when the bid uses a typed [`BidRenderSourceV1`] instead.
    pub creative: Option<String>,
    /// Advertiser domain
    pub adomain: Option<Vec<String>>,
    /// Bidder/seat identifier
    pub bidder: String,
    /// Width of creative
    pub width: u32,
    /// Height of creative
    pub height: u32,
    /// Win notification URL
    pub nurl: Option<String>,
    /// Billing notification URL
    pub burl: Option<String>,
    /// `OpenRTB` bid identifier — the `id` of the bid object itself.
    ///
    /// Distinct from [`ad_id`](Self::ad_id): unique per bid instance rather
    /// than a creative identifier. Always present per the `OpenRTB` spec, so it
    /// is the last-resort `hb_adid` source for bidders that return neither a
    /// Prebid Cache UUID nor `adid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_id: Option<String>,
    /// Ad ID from the bidder.
    pub ad_id: Option<String>,
    /// Optional `OpenRTB` creative identifier.
    pub creative_id: Option<String>,
    /// Typed browser renderer capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renderer: Option<BidRenderSourceV1>,
    /// Prebid Cache UUID for this bid.
    ///
    /// Populated from `ext.prebid.cache.bids.cacheId` in the PBS response.
    /// Used as `hb_adid` targeting value in `window.tsjs.bids`. `None` for
    /// non-PBS providers (e.g., APS) and PBS bids without Prebid Cache enabled.
    pub cache_id: Option<String>,
    /// Prebid Cache host (e.g., `"openads.adsrvr.org"`).
    ///
    /// Populated from the host of `ext.prebid.cache.bids.url`. Used as
    /// `hb_cache_host` targeting value. `None` when cache is absent.
    pub cache_host: Option<String>,
    /// Prebid Cache path (e.g., `"/cache"`).
    ///
    /// Populated from the path of `ext.prebid.cache.bids.url`. Used as
    /// `hb_cache_path` targeting value. `None` when cache is absent.
    pub cache_path: Option<String>,
    /// Provider-specific bid metadata.
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Length of the hex-encoded creative trace hash.
const ADM_TRACE_HASH_LEN: usize = 16;

/// Compute the trace hash for delivered creative markup.
#[must_use]
pub fn adm_trace_hash(adm: &str) -> String {
    use sha2::{Digest as _, Sha256};

    let digest = Sha256::digest(adm.as_bytes());
    let mut hex = hex::encode(digest);
    hex.truncate(ADM_TRACE_HASH_LEN);
    hex
}

impl Bid {
    /// Trace hash of this bid's creative markup, when present.
    #[must_use]
    pub fn creative_trace_hash(&self) -> Option<String> {
        self.creative.as_deref().map(adm_trace_hash)
    }
}

/// Per-provider summary included in the auction response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSummary {
    /// Provider name (e.g., "prebid", "aps").
    pub name: String,
    /// Bid status from this provider.
    pub status: BidStatus,
    /// Number of bids returned.
    pub bid_count: usize,
    /// Unique bidder/seat names (e.g., "kargo", "pubmatic", "ix").
    pub bidders: Vec<String>,
    /// Response time in milliseconds.
    pub time_ms: u64,
    /// Provider-specific metadata (from [`AuctionResponse::metadata`]).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl From<&AuctionResponse> for ProviderSummary {
    fn from(response: &AuctionResponse) -> Self {
        let mut bidders: Vec<String> = response.bids.iter().map(|b| b.bidder.clone()).collect();
        bidders.sort_unstable();
        bidders.dedup();

        Self {
            name: response.provider.clone(),
            status: response.status.clone(),
            bid_count: response.bids.len(),
            bidders,
            time_ms: response.response_time_ms,
            metadata: response.metadata.clone(),
        }
    }
}

/// `OpenRTB` response metadata for the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorExt {
    pub strategy: String,
    pub providers: usize,
    pub total_bids: usize,
    pub time_ms: u64,
    /// Per-provider breakdown of the auction.
    #[serde(default)]
    pub provider_details: Vec<ProviderSummary>,
    /// Winners omitted during final response conversion.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped_winner_count: usize,
    /// Machine-readable reasons for omitted winners.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dropped_winner_reasons: AuctionDropReasons,
}

/// Status of bid response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BidStatus {
    /// Auction completed successfully
    Success,
    /// No bids returned
    NoBid,
    /// Auction failed/timed out
    Error,
    /// Auction still in progress
    Pending,
}

impl AuctionResponse {
    /// Create a new successful auction response.
    pub fn success(provider: impl Into<String>, bids: Vec<Bid>, response_time_ms: u64) -> Self {
        Self {
            provider: provider.into(),
            bids,
            status: BidStatus::Success,
            response_time_ms,
            metadata: HashMap::new(),
        }
    }

    /// Create a no-bid response.
    pub fn no_bid(provider: impl Into<String>, response_time_ms: u64) -> Self {
        Self {
            provider: provider.into(),
            bids: Vec::new(),
            status: BidStatus::NoBid,
            response_time_ms,
            metadata: HashMap::new(),
        }
    }

    /// Create an error response.
    pub fn error(provider: impl Into<String>, response_time_ms: u64) -> Self {
        Self {
            provider: provider.into(),
            bids: Vec::new(),
            status: BidStatus::Error,
            response_time_ms,
            metadata: HashMap::new(),
        }
    }

    /// Add metadata to the response.
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Project typed local drop reasons into the existing provider metadata surface.
    #[must_use]
    pub fn with_drop_reasons(mut self, reasons: &AuctionDropReasons) -> Self {
        if !reasons.is_empty() {
            let values = reasons
                .iter()
                .map(|(reason, count)| {
                    (reason.as_str().to_string(), serde_json::Value::from(*count))
                })
                .collect();
            self.metadata.insert(
                "drop_reasons".to_string(),
                serde_json::Value::Object(values),
            );
        }
        self
    }

    /// Project one typed local drop reason into provider metadata.
    #[must_use]
    pub fn with_drop_reason(self, reason: AuctionDropReason) -> Self {
        self.with_drop_reasons(&BTreeMap::from([(reason, 1)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_drop_reasons_use_exact_literals_in_provider_summary_metadata() {
        let reasons = [
            AuctionDropReason::CreativeProcessingRejected,
            AuctionDropReason::InvalidCreativeId,
            AuctionDropReason::CreativeIdTooLarge,
            AuctionDropReason::DimensionsOutOfRange,
            AuctionDropReason::DuplicateUpstreamBidId,
            AuctionDropReason::EmptySeatBid,
            AuctionDropReason::EmptySeatBidBids,
            AuctionDropReason::InvalidCreativeUrl,
            AuctionDropReason::InvalidDimensions,
            AuctionDropReason::InvalidPrice,
            AuctionDropReason::InvalidProviderResponse,
            AuctionDropReason::InvalidTagType,
            AuctionDropReason::InvalidUpstreamBidId,
            AuctionDropReason::LostToHigherBid,
            AuctionDropReason::MalformedBid,
            AuctionDropReason::MissingCreativeUrl,
            AuctionDropReason::MissingRequestContext,
            AuctionDropReason::MissingUpstreamBidId,
            AuctionDropReason::MultipleRenderSources,
            AuctionDropReason::NoRenderSource,
            AuctionDropReason::RendererExtensionSerializationFailed,
            AuctionDropReason::RenderPayloadTooLarge,
            AuctionDropReason::ScriptRenderingDisabled,
            AuctionDropReason::UnknownImpression,
            AuctionDropReason::UnsupportedMediaType,
            AuctionDropReason::UpstreamBidIdTooLarge,
        ];
        for reason in reasons {
            assert_eq!(
                serde_json::to_value(reason).expect("drop reason should serialize"),
                json!(reason.as_str()),
                "serde and diagnostic literal should agree for {reason:?}"
            );
        }

        let response = AuctionResponse::no_bid("aps", 12)
            .with_drop_reason(AuctionDropReason::InvalidProviderResponse);
        let summary = ProviderSummary::from(&response);
        assert_eq!(
            summary.metadata["drop_reasons"]["invalid_provider_response"], 1,
            "publisher provider-summary projection should retain the typed reason"
        );
    }

    fn make_bid(bidder: &str) -> Bid {
        Bid {
            slot_id: "slot-1".to_owned(),
            candidate_id: None,
            candidate_provider: None,
            renderer_reservation_id: None,
            price: Some(1.0),
            currency: "USD".to_owned(),
            creative: None,
            adomain: None,
            bidder: bidder.to_owned(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            bid_id: None,
            ad_id: None,
            creative_id: None,
            renderer: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn provider_summary_from_successful_response() {
        let response = AuctionResponse::success(
            "prebid",
            vec![make_bid("kargo"), make_bid("pubmatic"), make_bid("ix")],
            95,
        );

        let summary = ProviderSummary::from(&response);

        assert_eq!(summary.name, "prebid", "should use provider name");
        assert_eq!(summary.status, BidStatus::Success, "should preserve status");
        assert_eq!(summary.bid_count, 3, "should count all bids");
        assert_eq!(
            summary.bidders,
            vec!["ix", "kargo", "pubmatic"],
            "should list unique bidders sorted"
        );
        assert_eq!(summary.time_ms, 95, "should preserve response time");
        assert!(summary.metadata.is_empty(), "should have no metadata");
    }

    #[test]
    fn provider_summary_deduplicates_bidder_names() {
        let response = AuctionResponse::success(
            "prebid",
            vec![make_bid("kargo"), make_bid("kargo"), make_bid("pubmatic")],
            50,
        );

        let summary = ProviderSummary::from(&response);

        assert_eq!(
            summary.bid_count, 3,
            "should count all bids including dupes"
        );
        assert_eq!(
            summary.bidders,
            vec!["kargo", "pubmatic"],
            "should deduplicate bidder names"
        );
    }

    #[test]
    fn provider_summary_from_no_bid_response() {
        let response = AuctionResponse::no_bid("aps", 110);

        let summary = ProviderSummary::from(&response);

        assert_eq!(summary.name, "aps", "should use provider name");
        assert_eq!(
            summary.status,
            BidStatus::NoBid,
            "should preserve no-bid status"
        );
        assert_eq!(summary.bid_count, 0, "should have zero bids");
        assert!(summary.bidders.is_empty(), "should have no bidders");
    }

    #[test]
    fn provider_summary_from_error_response() {
        let response = AuctionResponse::error("prebid", 200);

        let summary = ProviderSummary::from(&response);

        assert_eq!(
            summary.status,
            BidStatus::Error,
            "should preserve error status"
        );
        assert_eq!(summary.bid_count, 0, "should have zero bids");
        assert!(summary.bidders.is_empty(), "should have no bidders");
    }

    #[test]
    fn provider_summary_passes_through_metadata() {
        let response = AuctionResponse::success("prebid", vec![make_bid("kargo")], 80)
            .with_metadata("responsetimemillis", json!({"kargo": 70, "pubmatic": 90}))
            .with_metadata("errors", json!({"pubmatic": [{"code": 1}]}));

        let summary = ProviderSummary::from(&response);

        assert_eq!(summary.metadata.len(), 2, "should forward all metadata");
        assert_eq!(
            summary.metadata["responsetimemillis"],
            json!({"kargo": 70, "pubmatic": 90}),
            "should preserve responsetimemillis"
        );
        assert_eq!(
            summary.metadata["errors"],
            json!({"pubmatic": [{"code": 1}]}),
            "should preserve errors"
        );
    }

    #[test]
    fn provider_summary_skips_metadata_in_serialization_when_empty() {
        let response = AuctionResponse::no_bid("aps", 100);
        let summary = ProviderSummary::from(&response);

        let json = serde_json::to_value(&summary).expect("should serialize");

        assert!(
            json.get("metadata").is_none(),
            "should omit metadata field when empty"
        );
    }

    #[test]
    fn bid_with_cache_fields_round_trips_through_json() {
        let bid = Bid {
            slot_id: "atf".to_string(),
            candidate_id: None,
            candidate_provider: None,
            renderer_reservation_id: None,
            price: Some(1.50),
            currency: "USD".to_string(),
            creative: None,
            adomain: None,
            bidder: "thetradedesk".to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            bid_id: None,
            ad_id: Some("bid-id".to_string()),
            creative_id: None,
            renderer: None,
            cache_id: Some("cache-uuid".to_string()),
            cache_host: Some("cache.example.com".to_string()),
            cache_path: Some("/pbc/v1/cache".to_string()),
            metadata: HashMap::new(),
        };
        let json = serde_json::to_string(&bid).expect("should serialize Bid");
        let restored: Bid = serde_json::from_str(&json).expect("should deserialize Bid");
        assert_eq!(
            restored.cache_id.as_deref(),
            Some("cache-uuid"),
            "should round-trip cache_id"
        );
        assert_eq!(
            restored.cache_host.as_deref(),
            Some("cache.example.com"),
            "should round-trip cache_host"
        );
        assert_eq!(
            restored.cache_path.as_deref(),
            Some("/pbc/v1/cache"),
            "should round-trip cache_path"
        );
    }

    #[test]
    fn aps_renderer_serializes_to_versioned_camel_case_contract() {
        let renderer = BidRenderSourceV1::Aps(ApsRendererV1 {
            version: 1,
            account_id: "example-account-id".to_string(),
            bid_id: "fictional-bid-id".to_string(),
            creative_id: Some("fictional-creative-id".to_string()),
            tag_type: ApsTagType::Iframe,
            creative_url: "https://creative.example/render".to_string(),
            aax_response: "base64-data".to_string(),
            width: 300,
            height: 250,
        });

        let serialized = serde_json::to_value(&renderer).expect("should serialize renderer");

        assert_eq!(
            serialized,
            json!({
                "type": "aps",
                "version": 1,
                "accountId": "example-account-id",
                "bidId": "fictional-bid-id",
                "creativeId": "fictional-creative-id",
                "tagType": "iframe",
                "creativeUrl": "https://creative.example/render",
                "aaxResponse": "base64-data",
                "width": 300,
                "height": 250
            }),
            "should match renderer wire contract"
        );
    }

    #[test]
    fn aps_renderer_omits_absent_creative_id() {
        let renderer = BidRenderSourceV1::Aps(ApsRendererV1 {
            version: 1,
            account_id: "example-account-id".to_string(),
            bid_id: "fictional-bid-id".to_string(),
            creative_id: None,
            tag_type: ApsTagType::Iframe,
            creative_url: "https://creative.example/render".to_string(),
            aax_response: "base64-data".to_string(),
            width: 300,
            height: 250,
        });

        let serialized = serde_json::to_value(&renderer).expect("should serialize renderer");

        assert!(
            serialized.get("creativeId").is_none(),
            "should omit absent creative ID"
        );
    }

    #[test]
    fn media_type_defaults_to_banner() {
        assert_eq!(
            MediaType::default(),
            MediaType::Banner,
            "should default to Banner for serde field defaults"
        );
    }

    #[test]
    fn slot_failure_priority_matches_the_closed_contract() {
        let ordered = [
            AuctionSlotFailureReason::InternalError,
            AuctionSlotFailureReason::MediationFailed,
            AuctionSlotFailureReason::InvalidProviderResponse,
            AuctionSlotFailureReason::ProviderError,
            AuctionSlotFailureReason::ProviderTimeout,
            AuctionSlotFailureReason::ConsentDenied,
            AuctionSlotFailureReason::AuctionDisabled,
            AuctionSlotFailureReason::SlotNotEligible,
        ];

        assert_eq!(
            ordered.map(AuctionSlotFailureReason::priority),
            [0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(
            AuctionSlotFailureReason::WinnerNotRenderable.priority(),
            u8::MAX
        );
        assert_eq!(
            AuctionSlotFailureReason::IdentityGenerationFailed.priority(),
            u8::MAX
        );
    }

    #[test]
    fn bid_has_ad_id_field() {
        let bid = Bid {
            slot_id: "s".to_string(),
            candidate_id: None,
            candidate_provider: None,
            renderer_reservation_id: None,
            price: Some(1.0),
            currency: "USD".to_string(),
            creative: None,
            adomain: None,
            bidder: "kargo".to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            bid_id: None,
            ad_id: Some("prebid-ad-id-abc".to_string()),
            creative_id: None,
            renderer: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: Default::default(),
        };
        assert_eq!(bid.ad_id.as_deref(), Some("prebid-ad-id-abc"));
    }
}
