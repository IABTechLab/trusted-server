//! Google Ad Manager (GAM) integration.
//!
//! This module provides both real and mock implementations of the GAM auction provider.
//! GAM acts as a mediation server that receives bids from other providers and makes
//! the final ad selection decision.

use async_trait::async_trait;
use error_stack::Report;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use validator::Validate;

use crate::auction::provider::AuctionProvider;
use crate::auction::types::{
    AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType,
};
use crate::error::TrustedServerError;
use crate::settings::IntegrationConfig as IntegrationConfigTrait;

// ============================================================================
// Real GAM Provider
// ============================================================================

/// Configuration for GAM integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct GamConfig {
    /// Whether GAM integration is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// GAM network ID
    pub network_id: String,

    /// GAM API endpoint
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
}

impl IntegrationConfigTrait for GamConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    false
}

fn default_endpoint() -> String {
    "https://securepubads.g.doubleclick.net/gampad/ads".to_string()
}

fn default_timeout_ms() -> u32 {
    500
}

impl Default for GamConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            network_id: String::new(),
            endpoint: default_endpoint(),
            timeout_ms: default_timeout_ms(),
        }
    }
}

/// Google Ad Manager provider (acts as mediator).
pub struct GamAuctionProvider {
    config: GamConfig,
}

impl GamAuctionProvider {
    /// Create a new GAM auction provider.
    pub fn new(config: GamConfig) -> Self {
        Self { config }
    }
}

#[async_trait(?Send)]
impl AuctionProvider for GamAuctionProvider {
    fn provider_name(&self) -> &'static str {
        "gam"
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let _start = Instant::now();

        log::info!(
            "GAM: mediating auction for {} slots (network_id: {})",
            request.slots.len(),
            self.config.network_id
        );

        // TODO: Implement real GAM API integration
        //
        // Implementation steps:
        // 1. Extract bidder responses from request context (see gam_mock implementation for example)
        // 2. Transform bids to GAM key-value targeting format
        // 3. Make HTTP request to GAM ad server with:
        //    - Network ID
        //    - Ad unit codes
        //    - Key-value targeting (header bidding bids)
        //    - User context (device, geo, etc.)
        // 4. Parse GAM response (winning creative)
        // 5. Transform to our Bid format
        // 6. Handle timeout according to self.config.timeout_ms
        //
        // Reference: https://developers.google.com/ad-manager/api/start

        log::warn!("GAM: Real implementation not yet available");

        Err(Report::new(TrustedServerError::Auction {
            message: "GAM integration not yet implemented. Use 'gam_mock' provider for testing."
                .to_string(),
        }))
    }

    fn supports_media_type(&self, media_type: &MediaType) -> bool {
        // GAM supports all media types
        matches!(
            media_type,
            MediaType::Banner | MediaType::Video | MediaType::Native
        )
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

// ============================================================================
// Mock GAM Provider
// ============================================================================

/// Configuration for mock GAM integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct MockGamConfig {
    /// Whether this mock provider is enabled
    #[serde(default = "mock_default_enabled")]
    pub enabled: bool,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,

    /// Whether GAM should inject its own house ad bids
    #[serde(default = "mock_default_inject_house_bids")]
    pub inject_house_bids: bool,

    /// House ad bid price (CPM)
    #[serde(default = "mock_default_house_bid_price")]
    pub house_bid_price: f64,

    /// Percentage chance GAM house ads win (0-100)
    /// Used when inject_house_bids is true
    #[serde(default = "mock_default_win_rate")]
    #[validate(range(min = 0, max = 100))]
    pub win_rate: u8,

    /// Simulated network latency in milliseconds
    #[serde(default = "mock_default_latency_ms")]
    pub latency_ms: u64,
}

fn mock_default_enabled() -> bool {
    false
}

fn mock_default_inject_house_bids() -> bool {
    true
}

fn mock_default_house_bid_price() -> f64 {
    1.75
}

fn mock_default_win_rate() -> u8 {
    30 // GAM wins 30% of the time by default
}

fn mock_default_latency_ms() -> u64 {
    40
}

impl Default for MockGamConfig {
    fn default() -> Self {
        Self {
            enabled: mock_default_enabled(),
            timeout_ms: default_timeout_ms(),
            inject_house_bids: mock_default_inject_house_bids(),
            house_bid_price: mock_default_house_bid_price(),
            win_rate: mock_default_win_rate(),
            latency_ms: mock_default_latency_ms(),
        }
    }
}

impl IntegrationConfigTrait for MockGamConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Mock Google Ad Manager provider (acts as mediator).
pub struct MockGamProvider {
    config: MockGamConfig,
}

impl MockGamProvider {
    /// Create a new mock GAM auction provider.
    pub fn new(config: MockGamConfig) -> Self {
        Self { config }
    }

    /// Extract bidder responses from the auction request context.
    fn extract_bidder_responses(&self, request: &AuctionRequest) -> Vec<AuctionResponse> {
        request
            .context
            .get("bidder_responses")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Simulate GAM's mediation logic.
    ///
    /// In mock mode:
    /// 1. Optionally inject GAM's own house bids
    /// 2. Select winning bid per slot based on price + win rate
    /// 3. Return selected bids as GAM's response
    fn mediate_bids(
        &self,
        request: &AuctionRequest,
        bidder_responses: Vec<AuctionResponse>,
    ) -> Vec<Bid> {
        let mut all_bids: HashMap<String, Vec<Bid>> = HashMap::new();

        // Collect all bids by slot
        for response in bidder_responses {
            if response.status != BidStatus::Success {
                continue;
            }

            for bid in response.bids {
                all_bids
                    .entry(bid.slot_id.clone())
                    .or_insert_with(Vec::new)
                    .push(bid);
            }
        }

        // Optionally inject GAM house ads
        if self.config.inject_house_bids {
            for slot in &request.slots {
                let banner_format = slot
                    .formats
                    .iter()
                    .find(|f| f.media_type == MediaType::Banner);

                if let Some(format) = banner_format {
                    let house_bid = Bid {
                        slot_id: slot.id.clone(),
                        price: self.config.house_bid_price,
                        currency: "USD".to_string(),
                        creative: format!(
                            r#"<div style="width:{}px;height:{}px;background:#4285F4;display:flex;align-items:center;justify-content:center;color:white;font-family:sans-serif;">
                                <div style="text-align:center;">
                                    <div style="font-size:24px;font-weight:bold;">Google Ad Manager</div>
                                    <div style="font-size:14px;margin-top:8px;">House Ad: ${:.2} CPM</div>
                                </div>
                            </div>"#,
                            format.width, format.height, self.config.house_bid_price
                        ),
                        adomain: Some(vec!["google.com".to_string()]),
                        bidder: "gam-house-mock".to_string(),
                        width: format.width,
                        height: format.height,
                        nurl: Some(format!(
                            "https://mock-gam.google.com/win?slot={}&price={}",
                            slot.id, self.config.house_bid_price
                        )),
                        burl: None,
                        metadata: {
                            let mut meta = HashMap::new();
                            meta.insert("house_ad".to_string(), serde_json::json!(true));
                            meta
                        },
                    };

                    all_bids
                        .entry(slot.id.clone())
                        .or_insert_with(Vec::new)
                        .push(house_bid);
                }
            }
        }

        // Select winner for each slot
        let mut winning_bids = Vec::new();

        for (slot_id, mut bids) in all_bids {
            if bids.is_empty() {
                continue;
            }

            // Sort by price descending
            bids.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap());

            // In mock mode, sometimes prefer GAM house ads even if not highest bid
            let winner = if self.config.inject_house_bids {
                let has_gam_bid = bids.iter().any(|b| b.bidder == "gam-house-mock");

                // Use hash-based pseudo-randomness for consistent but realistic win rate simulation
                let should_gam_win = {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};

                    let mut hasher = DefaultHasher::new();
                    slot_id.hash(&mut hasher);
                    let hash_val = hasher.finish();
                    (hash_val % 100) < self.config.win_rate as u64
                };

                if has_gam_bid && should_gam_win {
                    bids.iter()
                        .find(|b| b.bidder == "gam-house-mock")
                        .cloned()
                        .unwrap()
                } else {
                    bids[0].clone()
                }
            } else {
                bids[0].clone()
            };

            log::info!(
                "GAM Mock mediation: slot '{}' won by '{}' at ${:.2} CPM (from {} bids)",
                slot_id,
                winner.bidder,
                winner.price,
                bids.len()
            );

            winning_bids.push(winner);
        }

        winning_bids
    }
}

#[async_trait(?Send)]
impl AuctionProvider for MockGamProvider {
    fn provider_name(&self) -> &'static str {
        "gam_mock"
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let start = Instant::now();

        log::info!(
            "GAM Mock: mediating auction for {} slots",
            request.slots.len()
        );

        // Extract bidder responses from context
        let bidder_responses = self.extract_bidder_responses(request);

        log::info!(
            "GAM Mock: received {} bidder responses to mediate",
            bidder_responses.len()
        );

        // Simulate GAM processing latency
        // Note: In real async code we'd use tokio::time::sleep, but in Fastly we just add to elapsed time

        let winning_bids = self.mediate_bids(request, bidder_responses);
        let response_time_ms = start.elapsed().as_millis() as u64 + self.config.latency_ms;

        log::info!(
            "GAM Mock: selected {} winning bids in {}ms (simulated latency: {}ms)",
            winning_bids.len(),
            response_time_ms,
            self.config.latency_ms
        );

        let response = if winning_bids.is_empty() {
            AuctionResponse::no_bid("gam_mock", response_time_ms)
        } else {
            AuctionResponse::success("gam_mock", winning_bids, response_time_ms)
                .with_metadata("mock", serde_json::json!(true))
                .with_metadata("mediator", serde_json::json!(true))
                .with_metadata("provider_type", serde_json::json!("gam"))
        };

        Ok(response)
    }

    fn supports_media_type(&self, media_type: &MediaType) -> bool {
        // GAM supports all media types
        matches!(
            media_type,
            MediaType::Banner | MediaType::Video | MediaType::Native
        )
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}
