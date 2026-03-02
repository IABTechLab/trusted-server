//! Core types for auction requests and responses.

use fastly::Request;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::auction::context::ContextValue;
use crate::geo::GeoInfo;
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
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
    /// Synthetic/hashed user ID
    pub id: String,
    /// Fresh ID for this session
    pub fresh_id: String,
    /// GDPR consent string if applicable
    pub consent: Option<String>,
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
pub struct AuctionContext<'a> {
    pub settings: &'a Settings,
    pub request: &'a Request,
    pub timeout_ms: u32,
    /// Provider responses from the bidding phase, used by mediators.
    /// This is `None` for regular bidders and `Some` when calling a mediator.
    pub provider_responses: Option<&'a [AuctionResponse]>,
}

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
            slot_id: "slot-1".to_string(),
            price: Some(1.0),
            currency: "USD".to_string(),
            creative: None,
            adomain: None,
            bidder: bidder.to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
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
}
