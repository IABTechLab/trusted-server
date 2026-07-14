//! Core types for auction requests and responses.

use edgezero_core::body::Body as EdgeBody;
use http::Request;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::auction::context::ContextValue;
use crate::geo::GeoInfo;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

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
    /// Extended User IDs resolved for the auction bidstream.
    ///
    /// Values can originate from the `/auction` request body, the
    /// [`crate::constants::COOKIE_TS_EIDS`] cookie, or server-side KV partner
    /// resolution keyed by EC. Endpoint handlers merge and consent-gate these
    /// values before they reach providers; provider integrations only forward
    /// already-gated EIDs.
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

/// Individual bid from a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bid {
    /// Slot this bid is for
    pub slot_id: String,
    /// Bid price in CPM
    /// None for APS bids where price is encoded and must be decoded by mediation layer
    pub price: Option<f64>,
    /// Currency code (e.g., "USD")
    pub currency: String,
    /// Creative markup (HTML/VAST)
    /// None when the bidder doesn't provide creative HTML (e.g., APS/TAM)
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
    /// Ad ID from the bidder
    pub ad_id: Option<String>,
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
    /// Provider-specific bid metadata
    /// For APS bids, contains encoded price in "amznbid" field
    pub metadata: HashMap<String, serde_json::Value>,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_bid(bidder: &str) -> Bid {
        Bid {
            slot_id: "slot-1".to_owned(),
            price: Some(1.0),
            currency: "USD".to_owned(),
            creative: None,
            adomain: None,
            bidder: bidder.to_owned(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            ad_id: None,
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
            price: Some(1.50),
            currency: "USD".to_string(),
            creative: None,
            adomain: None,
            bidder: "thetradedesk".to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            ad_id: Some("bid-id".to_string()),
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
    fn media_type_defaults_to_banner() {
        assert_eq!(
            MediaType::default(),
            MediaType::Banner,
            "should default to Banner for serde field defaults"
        );
    }

    #[test]
    fn bid_has_ad_id_field() {
        let bid = Bid {
            slot_id: "s".to_string(),
            price: Some(1.0),
            currency: "USD".to_string(),
            creative: None,
            adomain: None,
            bidder: "kargo".to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            ad_id: Some("prebid-ad-id-abc".to_string()),
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: Default::default(),
        };
        assert_eq!(bid.ad_id.as_deref(), Some("prebid-ad-id-abc"));
    }
}
