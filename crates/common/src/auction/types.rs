//! Core types for auction requests and responses.

use fastly::Request;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Additional context
    pub context: HashMap<String, serde_json::Value>,
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

/// OpenRTB response metadata for the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorExt {
    pub strategy: String,
    pub providers: usize,
    pub total_bids: usize,
    pub time_ms: u64,
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
