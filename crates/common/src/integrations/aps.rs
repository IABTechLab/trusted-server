//! Amazon Publisher Services (APS/TAM) integration.
//!
//! This module provides both real and mock implementations of the APS auction provider.

use async_trait::async_trait;
use error_stack::Report;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use validator::Validate;

use crate::auction::provider::AuctionProvider;
use crate::auction::types::{AuctionContext, AuctionRequest, AuctionResponse, Bid, MediaType};
use crate::error::TrustedServerError;
use crate::settings::IntegrationConfig as IntegrationConfigTrait;

// ============================================================================
// Real APS Provider
// ============================================================================

/// Configuration for APS integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct ApsConfig {
    /// Whether APS integration is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// APS publisher ID
    pub pub_id: String,

    /// APS API endpoint
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
}

fn default_enabled() -> bool {
    false
}

fn default_endpoint() -> String {
    "https://aax.amazon-adsystem.com/e/dtb/bid".to_string()
}

fn default_timeout_ms() -> u32 {
    800
}

impl Default for ApsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            pub_id: String::new(),
            endpoint: default_endpoint(),
            timeout_ms: default_timeout_ms(),
        }
    }
}

impl IntegrationConfigTrait for ApsConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Amazon APS auction provider.
pub struct ApsAuctionProvider {
    config: ApsConfig,
}

impl ApsAuctionProvider {
    /// Create a new APS auction provider.
    pub fn new(config: ApsConfig) -> Self {
        Self { config }
    }
}

#[async_trait(?Send)]
impl AuctionProvider for ApsAuctionProvider {
    fn provider_name(&self) -> &'static str {
        "aps"
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let _start = Instant::now();

        log::info!(
            "APS: requesting bids for {} slots (pub_id: {})",
            request.slots.len(),
            self.config.pub_id
        );

        // TODO: Implement real APS TAM API integration
        //
        // Implementation steps:
        // 1. Transform AuctionRequest to APS TAM bid request format
        // 2. Make HTTP POST to self.config.endpoint with:
        //    - Publisher ID (pub_id)
        //    - Slot information (sizes, ad unit codes)
        //    - User agent, page URL from context
        // 3. Parse APS TAM response
        // 4. Transform APS bids to our Bid format
        // 5. Handle timeout according to self.config.timeout_ms
        //
        // Reference: https://aps.amazon.com/aps/transparent-ad-marketplace-api/

        log::warn!("APS: Real implementation not yet available");

        Err(Report::new(TrustedServerError::Auction {
            message: "APS integration not yet implemented. Use 'aps_mock' provider for testing."
                .to_string(),
        }))
    }

    fn supports_media_type(&self, media_type: &MediaType) -> bool {
        // APS supports banner and video formats
        matches!(media_type, MediaType::Banner | MediaType::Video)
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

// ============================================================================
// Mock APS Provider
// ============================================================================

/// Configuration for mock APS integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct MockApsConfig {
    /// Whether this mock provider is enabled
    #[serde(default = "mock_default_enabled")]
    pub enabled: bool,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,

    /// Mock bid price (CPM) - default bid amount
    #[serde(default = "mock_default_bid_price")]
    pub bid_price: f64,

    /// Simulated network latency in milliseconds
    #[serde(default = "mock_default_latency_ms")]
    pub latency_ms: u64,

    /// Fill rate (0.0 to 1.0) - probability of returning a bid
    #[serde(default = "mock_default_fill_rate")]
    #[validate(range(min = 0.0, max = 1.0))]
    pub fill_rate: f64,
}

fn mock_default_enabled() -> bool {
    false
}

fn mock_default_bid_price() -> f64 {
    2.50
}

fn mock_default_latency_ms() -> u64 {
    80
}

fn mock_default_fill_rate() -> f64 {
    1.0 // Always return bids by default
}

impl Default for MockApsConfig {
    fn default() -> Self {
        Self {
            enabled: mock_default_enabled(),
            timeout_ms: default_timeout_ms(),
            bid_price: mock_default_bid_price(),
            latency_ms: mock_default_latency_ms(),
            fill_rate: mock_default_fill_rate(),
        }
    }
}

impl IntegrationConfigTrait for MockApsConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Mock Amazon APS auction provider.
pub struct MockApsProvider {
    config: MockApsConfig,
}

impl MockApsProvider {
    /// Create a new mock APS auction provider.
    pub fn new(config: MockApsConfig) -> Self {
        Self { config }
    }

    /// Generate mock bids for testing.
    fn generate_mock_bids(&self, request: &AuctionRequest) -> Vec<Bid> {
        request
            .slots
            .iter()
            .filter_map(|slot| {
                // Check fill rate using hash-based pseudo-randomness
                if self.config.fill_rate < 1.0 {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};

                    let mut hasher = DefaultHasher::new();
                    slot.id.hash(&mut hasher);
                    let hash_val = hasher.finish();
                    let rand_val = (hash_val % 100) as f64 / 100.0;

                    if rand_val >= self.config.fill_rate {
                        log::debug!("APS Mock: No fill for slot '{}' (fill_rate={})", slot.id, self.config.fill_rate);
                        return None;
                    }
                }

                // Only bid on banner ads
                let banner_format = slot
                    .formats
                    .iter()
                    .find(|f| f.media_type == MediaType::Banner)?;

                // Mock APS typically bids slightly higher than floor
                let price = slot
                    .floor_price
                    .map(|floor| floor * 1.15)
                    .unwrap_or(self.config.bid_price);

                Some(Bid {
                    slot_id: slot.id.clone(),
                    price,
                    currency: "USD".to_string(),
                    creative: format!(
                        r#"<div style="width:{}px;height:{}px;background:#FF9900;display:flex;align-items:center;justify-content:center;color:white;font-family:sans-serif;">
                            <div style="text-align:center;">
                                <div style="font-size:24px;font-weight:bold;">Amazon APS</div>
                                <div style="font-size:14px;margin-top:8px;">Mock Bid: ${:.2} CPM</div>
                            </div>
                        </div>"#,
                        banner_format.width, banner_format.height, price
                    ),
                    adomain: Some(vec!["amazon.com".to_string()]),
                    bidder: "amazon-aps-mock".to_string(),
                    width: banner_format.width,
                    height: banner_format.height,
                    nurl: Some(format!(
                        "https://mock-aps.amazon.com/win?slot={}&price={}",
                        slot.id, price
                    )),
                    burl: Some(format!(
                        "https://mock-aps.amazon.com/bill?slot={}&price={}",
                        slot.id, price
                    )),
                    metadata: std::collections::HashMap::new(),
                })
            })
            .collect()
    }
}

#[async_trait(?Send)]
impl AuctionProvider for MockApsProvider {
    fn provider_name(&self) -> &'static str {
        "aps_mock"
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let start = Instant::now();

        log::info!(
            "APS Mock: requesting bids for {} slots",
            request.slots.len()
        );

        // Simulate network latency
        // Note: In real async code we'd use tokio::time::sleep, but in Fastly we just add to elapsed time

        let bids = self.generate_mock_bids(request);
        let response_time_ms = start.elapsed().as_millis() as u64 + self.config.latency_ms;

        log::info!(
            "APS Mock: returning {} bids in {}ms (simulated latency: {}ms)",
            bids.len(),
            response_time_ms,
            self.config.latency_ms
        );

        let response = if bids.is_empty() {
            AuctionResponse::no_bid("aps_mock", response_time_ms)
        } else {
            AuctionResponse::success("aps_mock", bids, response_time_ms)
                .with_metadata("mock", serde_json::json!(true))
                .with_metadata("provider_type", serde_json::json!("aps"))
        };

        Ok(response)
    }

    fn supports_media_type(&self, media_type: &MediaType) -> bool {
        matches!(media_type, MediaType::Banner | MediaType::Video)
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

// ============================================================================
// Provider Auto-Registration
// ============================================================================

use std::sync::Arc;
use crate::settings::Settings;

/// Auto-register APS providers based on settings configuration.
///
/// This function checks the settings for both real and mock APS configurations
/// and returns any enabled providers ready for registration with the orchestrator.
pub fn register_providers(settings: &Settings) -> Vec<Arc<dyn AuctionProvider>> {
    let mut providers: Vec<Arc<dyn AuctionProvider>> = Vec::new();

    // Check for real APS provider configuration
    if let Ok(Some(config)) = settings.integration_config::<ApsConfig>("aps") {
        log::info!("Registering real APS provider");
        providers.push(Arc::new(ApsAuctionProvider::new(config)));
    }

    // Check for mock APS provider configuration
    if let Ok(Some(config)) = settings.integration_config::<MockApsConfig>("aps_mock") {
        log::info!("Registering mock APS provider");
        providers.push(Arc::new(MockApsProvider::new(config)));
    }

    providers
}
