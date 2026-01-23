//! Mock Ad Server Integration
//!
//! Provides a mock ad server mediator that calls mocktioneer's mediation endpoint.
//! This integration acts as a mediator in the auction flow, selecting winning bids
//! based on price (highest price wins).

use error_stack::{Report, ResultExt};
use fastly::http::Method;
use fastly::Request;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::sync::Arc;
use validator::Validate;

use crate::auction::provider::AuctionProvider;
use crate::auction::types::{
    AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType,
};
use crate::backend::ensure_backend_from_url;
use crate::error::TrustedServerError;
use crate::settings::{IntegrationConfig, Settings};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for mock ad server integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct AdServerMockConfig {
    /// Whether this integration is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Mediation endpoint URL
    pub endpoint: String,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,

    /// Optional price floor (minimum acceptable CPM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_floor: Option<f64>,
}

fn default_enabled() -> bool {
    false
}

fn default_timeout_ms() -> u32 {
    500
}

impl Default for AdServerMockConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            endpoint: "http://localhost:6767/adserver/mediate".to_string(),
            timeout_ms: default_timeout_ms(),
            price_floor: None,
        }
    }
}

impl IntegrationConfig for AdServerMockConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

// ============================================================================
// Provider
// ============================================================================

/// Mock ad server mediator provider.
pub struct AdServerMockProvider {
    config: AdServerMockConfig,
}

impl AdServerMockProvider {
    /// Create a new mock ad server provider.
    pub fn new(config: AdServerMockConfig) -> Self {
        Self { config }
    }

    /// Build mediation request from auction request and bidder responses.
    ///
    /// Handles both:
    /// - Regular bids with decoded prices (price field set)
    /// - APS-style bids with encoded prices (price=None, encoded price in metadata)
    fn build_mediation_request(
        &self,
        request: &AuctionRequest,
        bidder_responses: &[AuctionResponse],
    ) -> Result<Json, Report<TrustedServerError>> {
        // Convert bidder responses to mediation format
        let bidder_responses_json: Vec<Json> = bidder_responses
            .iter()
            .filter(|r| r.status == BidStatus::Success)
            .map(|response| {
                let bids: Vec<Json> = response
                    .bids
                    .iter()
                    .map(|bid| {
                        // Check if this is an APS bid with encoded price (inferred from amznbid in metadata)
                        let encoded_price = bid
                            .metadata
                            .get("amznbid")
                            .and_then(|v| v.as_str())
                            .map(String::from);

                        if encoded_price.is_some() {
                            // APS bid - send encoded price for mediation to decode
                            json!({
                                "imp_id": bid.slot_id,
                                "encoded_price": encoded_price,
                                "adm": bid.creative,
                                "w": bid.width,
                                "h": bid.height,
                                "crid": format!("{}-creative", bid.bidder),
                                "adomain": bid.adomain,
                            })
                        } else {
                            // Regular bid with decoded price
                            json!({
                                "imp_id": bid.slot_id,
                                "price": bid.price,
                                "adm": bid.creative,
                                "w": bid.width,
                                "h": bid.height,
                                "crid": format!("{}-creative", bid.bidder),
                                "adomain": bid.adomain,
                            })
                        }
                    })
                    .collect();

                json!({
                    "bidder": response.provider,
                    "bids": bids,
                })
            })
            .collect();

        // Build impressions from request slots
        let imps: Vec<Json> = request
            .slots
            .iter()
            .map(|slot| {
                let banner_format = slot
                    .formats
                    .iter()
                    .find(|f| f.media_type == MediaType::Banner);

                json!({
                    "id": slot.id,
                    "banner": banner_format.map(|f| json!({
                        "w": f.width,
                        "h": f.height,
                    })),
                })
            })
            .collect();

        // Build mediation config
        let config_json = if self.config.price_floor.is_some() {
            json!({
                "price_floor": self.config.price_floor,
            })
        } else {
            json!(null)
        };

        // Build full mediation request
        Ok(json!({
            "id": request.id,
            "imp": imps,
            "ext": {
                "bidder_responses": bidder_responses_json,
                "config": config_json,
            },
        }))
    }

    /// Parse OpenRTB response from mediation endpoint.
    /// Mediation returns decoded prices for all bids (including APS bids that were encoded).
    fn parse_mediation_response(&self, json: &Json, response_time_ms: u64) -> AuctionResponse {
        // Parse OpenRTB response
        let empty_array = vec![];
        let seatbid = json["seatbid"].as_array().unwrap_or(&empty_array);

        let mut all_bids = Vec::new();

        for seat in seatbid {
            let seat_name = seat["seat"].as_str().unwrap_or("unknown");
            let empty_bids = vec![];
            let bids = seat["bid"].as_array().unwrap_or(&empty_bids);

            for bid in bids {
                // Mediation layer returns decoded prices for all bids
                all_bids.push(Bid {
                    slot_id: bid["impid"].as_str().unwrap_or("").to_string(),
                    price: bid["price"].as_f64(), // Now properly decoded by mediation
                    currency: "USD".to_string(),
                    creative: bid["adm"].as_str().map(String::from),
                    width: bid["w"].as_u64().unwrap_or(0) as u32,
                    height: bid["h"].as_u64().unwrap_or(0) as u32,
                    bidder: seat_name.to_string(),
                    adomain: bid["adomain"].as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    }),
                    nurl: None,
                    burl: None,
                    metadata: HashMap::new(),
                });
            }
        }

        if all_bids.is_empty() {
            AuctionResponse::no_bid("adserver_mock", response_time_ms)
        } else {
            AuctionResponse::success("adserver_mock", all_bids, response_time_ms)
        }
    }
}

impl AuctionProvider for AdServerMockProvider {
    fn provider_name(&self) -> &'static str {
        "adserver_mock"
    }

    fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<fastly::http::request::PendingRequest, Report<TrustedServerError>> {
        // Get bidder responses from context (passed by orchestrator for mediation)
        let bidder_responses = context.provider_responses.unwrap_or(&[]);

        log::info!(
            "AdServer Mock: mediating {} slots with {} bidder responses",
            request.slots.len(),
            bidder_responses.len()
        );

        // Build mediation request
        let mediation_req = self
            .build_mediation_request(request, bidder_responses)
            .change_context(TrustedServerError::Auction {
                message: "Failed to build mediation request".to_string(),
            })?;

        log::debug!("AdServer Mock: mediation request: {:?}", mediation_req);

        // Create HTTP POST request
        let mut req = Request::new(Method::POST, &self.config.endpoint);

        // Set Host header with port to ensure mocktioneer generates correct iframe URLs
        if let Ok(url) = url::Url::parse(&self.config.endpoint) {
            if let Some(host) = url.host_str() {
                let host_with_port = if let Some(port) = url.port() {
                    format!("{}:{}", host, port)
                } else {
                    host.to_string()
                };
                req.set_header("Host", &host_with_port);
            }
        }

        req.set_body_json(&mediation_req)
            .change_context(TrustedServerError::Auction {
                message: "Failed to set mediation request body".to_string(),
            })?;

        // Send async
        let backend_name = ensure_backend_from_url(&self.config.endpoint).change_context(
            TrustedServerError::Auction {
                message: format!(
                    "Failed to resolve backend for mediation endpoint: {}",
                    self.config.endpoint
                ),
            },
        )?;

        let pending = req
            .send_async(backend_name)
            .change_context(TrustedServerError::Auction {
                message: "Failed to send mediation request".to_string(),
            })?;

        Ok(pending)
    }

    fn parse_response(
        &self,
        mut response: fastly::Response,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        if !response.get_status().is_success() {
            log::warn!(
                "AdServer Mock returned non-success: {}",
                response.get_status()
            );
            return Ok(AuctionResponse::error("adserver_mock", response_time_ms));
        }

        let body_bytes = response.take_body_bytes();
        let response_json: Json =
            serde_json::from_slice(&body_bytes).change_context(TrustedServerError::Auction {
                message: "Failed to parse mediation response".to_string(),
            })?;

        log::debug!("AdServer Mock response: {:?}", response_json);

        let auction_response = self.parse_mediation_response(&response_json, response_time_ms);

        log::info!(
            "AdServer Mock returned {} bids in {}ms",
            auction_response.bids.len(),
            response_time_ms
        );

        Ok(auction_response)
    }

    fn supports_media_type(&self, media_type: &MediaType) -> bool {
        matches!(media_type, MediaType::Banner)
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    fn backend_name(&self) -> Option<String> {
        ensure_backend_from_url(&self.config.endpoint).ok()
    }
}

// ============================================================================
// Auto-Registration
// ============================================================================

/// Auto-register ad server mock provider based on settings configuration.
pub fn register_providers(settings: &Settings) -> Vec<Arc<dyn AuctionProvider>> {
    let mut providers: Vec<Arc<dyn AuctionProvider>> = Vec::new();

    match settings.integration_config::<AdServerMockConfig>("adserver_mock") {
        Ok(Some(config)) => {
            log::info!(
                "Registering AdServer Mock mediator (endpoint: {})",
                config.endpoint
            );
            providers.push(Arc::new(AdServerMockProvider::new(config)));
        }
        Ok(None) => {
            log::debug!("AdServer Mock config found but is disabled");
        }
        Err(e) => {
            log::error!("Failed to load AdServer Mock config: {:?}", e);
        }
    }

    providers
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::*;

    fn create_test_auction_request() -> AuctionRequest {
        AuctionRequest {
            id: "test-auction-123".to_string(),
            slots: vec![AdSlot {
                id: "header-banner".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 728,
                    height: 90,
                }],
                floor_price: Some(1.50),
                targeting: HashMap::new(),
            }],
            publisher: PublisherInfo {
                domain: "test.com".to_string(),
                page_url: Some("https://test.com/article".to_string()),
            },
            user: UserInfo {
                id: "user-123".to_string(),
                fresh_id: "fresh-456".to_string(),
                consent: None,
            },
            device: Some(DeviceInfo {
                user_agent: Some("Mozilla/5.0".to_string()),
                ip: Some("192.168.1.1".to_string()),
                geo: None,
            }),
            site: None,
            context: HashMap::new(),
        }
    }

    #[test]
    fn test_build_mediation_request() {
        let config = AdServerMockConfig {
            enabled: true,
            endpoint: "http://localhost:6767/adserver/mediate".to_string(),
            timeout_ms: 500,
            price_floor: Some(1.00),
        };

        let provider = AdServerMockProvider::new(config);
        let mut auction_request = create_test_auction_request();

        // Add bidder responses to context
        let bidder_responses = vec![
            AuctionResponse {
                provider: "amazon-aps".to_string(),
                status: BidStatus::Success,
                bids: vec![Bid {
                    slot_id: "header-banner".to_string(),
                    price: Some(3.00),
                    currency: "USD".to_string(),
                    creative: Some("<div>APS Ad</div>".to_string()),
                    width: 728,
                    height: 90,
                    bidder: "amazon-aps".to_string(),
                    adomain: Some(vec!["amazon.com".to_string()]),
                    nurl: None,
                    burl: None,
                    metadata: HashMap::new(),
                }],
                response_time_ms: 150,
                metadata: HashMap::new(),
            },
            AuctionResponse {
                provider: "test-bidder".to_string(),
                status: BidStatus::Success,
                bids: vec![Bid {
                    slot_id: "header-banner".to_string(),
                    price: Some(3.50),
                    currency: "USD".to_string(),
                    creative: Some("<div>Test Ad</div>".to_string()),
                    width: 728,
                    height: 90,
                    bidder: "test-bidder".to_string(),
                    adomain: None,
                    nurl: None,
                    burl: None,
                    metadata: HashMap::new(),
                }],
                response_time_ms: 120,
                metadata: HashMap::new(),
            },
        ];

        auction_request.context.insert(
            "provider_responses".to_string(),
            serde_json::to_value(&bidder_responses).unwrap(),
        );

        let mediation_req = provider
            .build_mediation_request(&auction_request, &bidder_responses)
            .unwrap();

        // Verify structure
        assert_eq!(mediation_req["id"], "test-auction-123");
        assert_eq!(mediation_req["imp"].as_array().unwrap().len(), 1);
        assert_eq!(
            mediation_req["ext"]["bidder_responses"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(mediation_req["ext"]["config"]["price_floor"], 1.00);
    }

    #[test]
    fn test_parse_mediation_response() {
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);

        let mediation_response = json!({
            "id": "test-auction-123",
            "seatbid": [
                {
                    "seat": "test-bidder",
                    "bid": [
                        {
                            "id": "bid-001",
                            "impid": "header-banner",
                            "price": 3.50,
                            "adm": "<div>Test Ad</div>",
                            "w": 728,
                            "h": 90,
                            "crid": "test-creative",
                            "adomain": ["test.com"]
                        }
                    ]
                }
            ],
            "cur": "USD"
        });

        let auction_response = provider.parse_mediation_response(&mediation_response, 200);

        assert_eq!(auction_response.provider, "adserver_mock");
        assert_eq!(auction_response.status, BidStatus::Success);
        assert_eq!(auction_response.bids.len(), 1);
        assert_eq!(auction_response.response_time_ms, 200);

        let bid = &auction_response.bids[0];
        assert_eq!(bid.slot_id, "header-banner");
        assert_eq!(bid.price, Some(3.50)); // Mediation returns decoded price
        assert_eq!(bid.bidder, "test-bidder");
        assert_eq!(bid.width, 728);
        assert_eq!(bid.height, 90);
    }

    #[test]
    fn test_parse_empty_mediation_response() {
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);

        let mediation_response = json!({
            "id": "test-auction-123",
            "seatbid": [],
            "cur": "USD"
        });

        let auction_response = provider.parse_mediation_response(&mediation_response, 100);

        assert_eq!(auction_response.status, BidStatus::NoBid);
        assert_eq!(auction_response.bids.len(), 0);
    }

    #[test]
    fn test_mediation_request_handles_none_creative() {
        // Test that bids without creative HTML (e.g., APS) are properly sent to mediation
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);

        let auction_request = AuctionRequest {
            id: "test-auction".to_string(),
            slots: vec![AdSlot {
                id: "slot-1".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                }],
                floor_price: None,
                targeting: HashMap::new(),
            }],
            publisher: PublisherInfo {
                domain: "test.com".to_string(),
                page_url: None,
            },
            user: UserInfo {
                id: "user-1".to_string(),
                fresh_id: "fresh-1".to_string(),
                consent: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        };

        // APS bid with encoded price (price=None, amznbid in metadata)
        let mut aps_metadata = HashMap::new();
        aps_metadata.insert("amznbid".to_string(), json!("encoded-price-value"));

        let bidder_responses = vec![AuctionResponse {
            provider: "aps".to_string(),
            status: BidStatus::Success,
            bids: vec![Bid {
                slot_id: "slot-1".to_string(),
                price: None, // APS bids have no decoded price
                currency: "USD".to_string(),
                creative: None, // APS doesn't provide creative
                width: 300,
                height: 250,
                bidder: "amazon-aps".to_string(),
                adomain: Some(vec!["amazon.com".to_string()]),
                nurl: None,
                burl: None,
                metadata: aps_metadata,
            }],
            response_time_ms: 100,
            metadata: HashMap::new(),
        }];

        let mediation_req = provider
            .build_mediation_request(&auction_request, &bidder_responses)
            .expect("should build request");

        // Verify the mediation request structure
        assert_eq!(mediation_req["id"], "test-auction");

        // Check that the bid was included with encoded_price
        let bidder_resp = &mediation_req["ext"]["bidder_responses"][0];
        assert_eq!(bidder_resp["bidder"], "aps");

        let bid = &bidder_resp["bids"][0];
        assert_eq!(bid["imp_id"], "slot-1");

        // Key assertions for APS-style encoded price bids:
        // 1. Should NOT have "price" field (or it should be null)
        assert!(
            bid["price"].is_null(),
            "APS bids should not have decoded price, got: {:?}",
            bid["price"]
        );
        // 2. Should have "encoded_price" field
        assert_eq!(
            bid["encoded_price"].as_str(),
            Some("encoded-price-value"),
            "APS bids should have encoded_price from metadata"
        );
        // 3. adm should be null (not a string)
        assert!(
            bid["adm"].is_null(),
            "Creative-less bids should have null adm, got: {:?}",
            bid["adm"]
        );
    }

    #[test]
    fn test_provider_metadata() {
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);

        assert_eq!(provider.provider_name(), "adserver_mock");
        assert!(!provider.is_enabled()); // Default is disabled
        assert_eq!(provider.timeout_ms(), 500);
        assert!(provider.supports_media_type(&MediaType::Banner));
        assert!(!provider.supports_media_type(&MediaType::Video));
        assert!(!provider.supports_media_type(&MediaType::Native));
    }

    #[test]
    fn test_parse_mediation_response_with_missing_prices() {
        // Test that mediator response with missing price fields returns None prices
        // This tests the scenario where mediation failed to decode APS prices
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);

        let mediation_response = json!({
            "id": "test-auction-123",
            "seatbid": [
                {
                    "seat": "test-bidder",
                    "bid": [
                        {
                            "id": "bid-001",
                            "impid": "header-banner",
                            "price": 3.50,
                            "adm": "<div>Valid Ad</div>",
                            "w": 728,
                            "h": 90,
                        },
                        {
                            "id": "bid-002",
                            "impid": "sidebar",
                            // Note: No "price" field - mediation failed to decode
                            "adm": "<div>Failed decode</div>",
                            "w": 300,
                            "h": 250,
                        }
                    ]
                }
            ],
            "cur": "USD"
        });

        let auction_response = provider.parse_mediation_response(&mediation_response, 200);

        assert_eq!(auction_response.status, BidStatus::Success);
        assert_eq!(auction_response.bids.len(), 2);

        // First bid should have decoded price
        let bid1 = &auction_response.bids[0];
        assert_eq!(bid1.slot_id, "header-banner");
        assert_eq!(bid1.price, Some(3.50));

        // Second bid should have None price (failed decode)
        let bid2 = &auction_response.bids[1];
        assert_eq!(bid2.slot_id, "sidebar");
        assert_eq!(
            bid2.price, None,
            "Bid without price field should have None price"
        );
    }
}
