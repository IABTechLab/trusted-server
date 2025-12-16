//! Auction orchestrator for managing multi-provider auctions.

use error_stack::{Report, ResultExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::error::TrustedServerError;

use super::config::AuctionConfig;
use super::provider::AuctionProvider;
use super::types::{AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus};

/// Manages auction execution across multiple providers.
pub struct AuctionOrchestrator {
    config: AuctionConfig,
    providers: HashMap<String, Arc<dyn AuctionProvider>>,
}

/// Result of an orchestrated auction.
#[derive(Debug, Clone)]
pub struct OrchestrationResult {
    /// All responses from bidders
    pub bidder_responses: Vec<AuctionResponse>,
    /// Final response from mediator (if used)
    pub mediator_response: Option<AuctionResponse>,
    /// Winning bids per slot
    pub winning_bids: HashMap<String, Bid>,
    /// Total orchestration time in milliseconds
    pub total_time_ms: u64,
    /// Metadata about the auction
    pub metadata: HashMap<String, serde_json::Value>,
}

impl AuctionOrchestrator {
    /// Create a new orchestrator with the given configuration.
    pub fn new(config: AuctionConfig) -> Self {
        Self {
            config,
            providers: HashMap::new(),
        }
    }

    /// Register an auction provider.
    pub fn register_provider(&mut self, provider: Arc<dyn AuctionProvider>) {
        let name = provider.provider_name().to_string();
        log::info!("Registering auction provider: {}", name);
        self.providers.insert(name, provider);
    }

    /// Execute an auction using the configured strategy.
    pub async fn run_auction(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        let start_time = Instant::now();

        log::info!("Running auction with strategy: {}", self.config.strategy);

        let result = match self.config.strategy.as_str() {
            "parallel_mediation" => self.run_parallel_mediation(request, context).await,
            "parallel_only" => self.run_parallel_only(request, context).await,
            "waterfall" => self.run_waterfall(request, context).await,
            strategy => Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Unknown auction strategy '{}'. Valid strategies: parallel_mediation, parallel_only, waterfall",
                    strategy
                ),
            })),
        }?;

        Ok(OrchestrationResult {
            total_time_ms: start_time.elapsed().as_millis() as u64,
            ..result
        })
    }

    /// Run auction with parallel bidding + mediation.
    ///
    /// Flow:
    /// 1. Run all bidders in parallel
    /// 2. Collect bids from all bidders
    /// 3. Send combined bids to mediator for final decision
    async fn run_parallel_mediation(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        // Phase 1: Run bidders in parallel
        let bidder_responses = self.run_bidders_parallel(request, context).await?;

        // Phase 2: Send to mediator if configured
        let (mediator_response, winning_bids) = if self.config.has_mediator() {
            let mediator_name = self.config.mediator.as_ref().unwrap();
            let mediator = self.get_provider(mediator_name)?;

            log::info!(
                "Sending {} bidder responses to mediator: {}",
                bidder_responses.len(),
                mediator.provider_name()
            );

            // Create a modified request with all bids attached
            let mut mediation_request = request.clone();
            mediation_request.context.insert(
                "bidder_responses".to_string(),
                serde_json::json!(&bidder_responses),
            );

            let mediator_resp = mediator
                .request_bids(&mediation_request, context)
                .await
                .change_context(TrustedServerError::Auction {
                    message: format!("Mediator {} failed", mediator.provider_name()),
                })?;

            // Extract winning bids from mediator response
            let winning = mediator_resp
                .bids
                .iter()
                .map(|bid| (bid.slot_id.clone(), bid.clone()))
                .collect();

            (Some(mediator_resp), winning)
        } else {
            // No mediator - select best bid per slot from bidder responses
            let winning = self.select_winning_bids(&bidder_responses);
            (None, winning)
        };

        Ok(OrchestrationResult {
            bidder_responses,
            mediator_response,
            winning_bids,
            total_time_ms: 0, // Will be set by caller
            metadata: HashMap::new(),
        })
    }

    /// Run auction with only parallel bidding (no mediation).
    async fn run_parallel_only(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        let bidder_responses = self.run_bidders_parallel(request, context).await?;
        let winning_bids = self.select_winning_bids(&bidder_responses);

        Ok(OrchestrationResult {
            bidder_responses,
            mediator_response: None,
            winning_bids,
            total_time_ms: 0,
            metadata: HashMap::new(),
        })
    }

    /// Run auction with waterfall strategy (sequential).
    async fn run_waterfall(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        let mut bidder_responses = Vec::new();
        let mut winning_bids = HashMap::new();

        // Try each bidder sequentially until we get bids
        for bidder_name in self.config.bidder_names() {
            let provider = match self.providers.get(bidder_name) {
                Some(p) => p,
                None => {
                    log::warn!("Provider '{}' not registered, skipping", bidder_name);
                    continue;
                }
            };

            if !provider.is_enabled() {
                log::debug!(
                    "Provider '{}' is disabled, skipping",
                    provider.provider_name()
                );
                continue;
            }

            log::info!("Waterfall: trying provider {}", provider.provider_name());

            match provider.request_bids(request, context).await {
                Ok(response) => {
                    let has_bids =
                        !response.bids.is_empty() && response.status == BidStatus::Success;
                    bidder_responses.push(response.clone());

                    if has_bids {
                        // Got bids, stop waterfall
                        winning_bids = response
                            .bids
                            .into_iter()
                            .map(|bid| (bid.slot_id.clone(), bid))
                            .collect();
                        break;
                    }
                }
                Err(e) => {
                    log::warn!(
                        "Provider '{}' failed in waterfall: {:?}",
                        provider.provider_name(),
                        e
                    );
                    // Continue to next provider
                }
            }
        }

        Ok(OrchestrationResult {
            bidder_responses,
            mediator_response: None,
            winning_bids,
            total_time_ms: 0,
            metadata: HashMap::new(),
        })
    }

    /// Run all bidders in parallel and collect responses.
    async fn run_bidders_parallel(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<Vec<AuctionResponse>, Report<TrustedServerError>> {
        let bidder_names = self.config.bidder_names();

        if bidder_names.is_empty() {
            return Err(Report::new(TrustedServerError::Auction {
                message: "No bidders configured".to_string(),
            }));
        }

        log::info!("Running {} bidders in parallel", bidder_names.len());

        let mut responses = Vec::new();

        // Note: In a true parallel implementation, we'd use tokio::join_all or similar
        // For Fastly Compute, we run sequentially but designed to be easily parallel
        for bidder_name in bidder_names {
            let provider = match self.providers.get(bidder_name) {
                Some(p) => p,
                None => {
                    log::warn!("Provider '{}' not registered, skipping", bidder_name);
                    continue;
                }
            };

            if !provider.is_enabled() {
                log::debug!(
                    "Provider '{}' is disabled, skipping",
                    provider.provider_name()
                );
                continue;
            }

            log::info!("Requesting bids from: {}", provider.provider_name());

            match provider.request_bids(request, context).await {
                Ok(response) => {
                    log::info!(
                        "Provider '{}' returned {} bids (status: {:?}, time: {}ms)",
                        response.provider,
                        response.bids.len(),
                        response.status,
                        response.response_time_ms
                    );
                    responses.push(response);
                }
                Err(e) => {
                    log::warn!("Provider '{}' failed: {:?}", provider.provider_name(), e);
                    // Don't fail entire auction if one provider fails
                    // Return error response for this provider
                    responses.push(AuctionResponse::error(provider.provider_name(), 0));
                }
            }
        }

        Ok(responses)
    }

    /// Select the best bid for each slot from all responses.
    fn select_winning_bids(&self, responses: &[AuctionResponse]) -> HashMap<String, Bid> {
        let mut winning_bids: HashMap<String, Bid> = HashMap::new();

        for response in responses {
            if response.status != BidStatus::Success {
                continue;
            }

            for bid in &response.bids {
                let should_replace = match winning_bids.get(&bid.slot_id) {
                    Some(current_winner) => bid.price > current_winner.price,
                    None => true,
                };

                if should_replace {
                    winning_bids.insert(bid.slot_id.clone(), bid.clone());
                }
            }
        }

        log::info!("Selected {} winning bids", winning_bids.len());
        winning_bids
    }

    /// Get a provider by name.
    fn get_provider(
        &self,
        name: &str,
    ) -> Result<&Arc<dyn AuctionProvider>, Report<TrustedServerError>> {
        self.providers.get(name).ok_or_else(|| {
            Report::new(TrustedServerError::Auction {
                message: format!("Provider '{}' not registered", name),
            })
        })
    }

    /// Check if orchestrator is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::*;
    use crate::integrations::aps::{MockApsConfig, MockApsProvider};
    use crate::integrations::gam::{MockGamConfig, MockGamProvider};
    use crate::test_support::tests::crate_test_settings_str;
    use fastly::Request;
    use std::collections::HashMap;

    fn create_test_auction_request() -> AuctionRequest {
        AuctionRequest {
            id: "test-auction-123".to_string(),
            slots: vec![
                AdSlot {
                    id: "header-banner".to_string(),
                    formats: vec![AdFormat {
                        media_type: MediaType::Banner,
                        width: 728,
                        height: 90,
                    }],
                    floor_price: Some(1.50),
                    targeting: HashMap::new(),
                },
                AdSlot {
                    id: "sidebar".to_string(),
                    formats: vec![AdFormat {
                        media_type: MediaType::Banner,
                        width: 300,
                        height: 250,
                    }],
                    floor_price: Some(1.00),
                    targeting: HashMap::new(),
                },
            ],
            publisher: PublisherInfo {
                domain: "test.com".to_string(),
                page_url: Some("https://test.com/article".to_string()),
            },
            user: UserInfo {
                id: "user-123".to_string(),
                fresh_id: "fresh-456".to_string(),
                consent: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        }
    }

    fn create_test_settings() -> &'static crate::settings::Settings {
        let settings_str = crate_test_settings_str();
        let settings = crate::settings::Settings::from_toml(&settings_str)
            .expect("should parse test settings");
        Box::leak(Box::new(settings))
    }

    fn create_test_context<'a>(
        settings: &'a crate::settings::Settings,
        req: &'a Request,
    ) -> AuctionContext<'a> {
        AuctionContext {
            settings,
            request: req,
            timeout_ms: 2000,
        }
    }

    #[tokio::test]
    async fn test_parallel_mediation_strategy() {
        let config = AuctionConfig {
            enabled: true,
            strategy: "parallel_mediation".to_string(),
            bidders: vec!["aps_mock".to_string()],
            mediator: Some("gam_mock".to_string()),
            timeout_ms: 2000,
        };

        let mut orchestrator = AuctionOrchestrator::new(config);

        // Register mock providers
        let aps_config = MockApsConfig {
            enabled: true,
            bid_price: 2.50,
            ..Default::default()
        };
        let gam_config = MockGamConfig {
            enabled: true,
            inject_house_bids: true,
            house_bid_price: 1.75,
            win_rate: 50,
            ..Default::default()
        };

        orchestrator.register_provider(Arc::new(MockApsProvider::new(aps_config)));
        orchestrator.register_provider(Arc::new(MockGamProvider::new(gam_config)));

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_context(settings, &req);

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("auction should succeed");

        // Verify bidder ran
        assert_eq!(result.bidder_responses.len(), 1);
        assert_eq!(result.bidder_responses[0].provider, "aps_mock");

        // Verify mediator ran
        assert!(result.mediator_response.is_some());
        let mediator_resp = result.mediator_response.unwrap();
        assert_eq!(mediator_resp.provider, "gam_mock");

        // Verify we got winning bids (GAM mediated)
        assert!(!result.winning_bids.is_empty());

        // Verify timing
        assert!(result.total_time_ms > 0);
    }

    #[tokio::test]
    async fn test_parallel_only_strategy() {
        let config = AuctionConfig {
            enabled: true,
            strategy: "parallel_only".to_string(),
            bidders: vec!["aps_mock".to_string()],
            mediator: None,
            timeout_ms: 2000,
        };

        let mut orchestrator = AuctionOrchestrator::new(config);

        let aps_config = MockApsConfig {
            enabled: true,
            bid_price: 2.50,
            ..Default::default()
        };

        orchestrator.register_provider(Arc::new(MockApsProvider::new(aps_config)));

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_context(settings, &req);

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("auction should succeed");

        // No mediator in parallel_only
        assert!(result.mediator_response.is_none());

        // Should have bids from APS
        assert_eq!(result.bidder_responses.len(), 1);
        assert!(result.bidder_responses[0].bids.len() > 0);

        // Winning bids selected directly from bidders
        assert!(!result.winning_bids.is_empty());
    }

    #[tokio::test]
    async fn test_waterfall_strategy() {
        let config = AuctionConfig {
            enabled: true,
            strategy: "waterfall".to_string(),
            bidders: vec!["aps_mock".to_string()],
            mediator: None,
            timeout_ms: 2000,
        };

        let mut orchestrator = AuctionOrchestrator::new(config);

        let aps_config = MockApsConfig {
            enabled: true,
            bid_price: 2.50,
            ..Default::default()
        };

        orchestrator.register_provider(Arc::new(MockApsProvider::new(aps_config)));

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_context(settings, &req);

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("auction should succeed");

        // Should have tried APS (first in waterfall)
        assert_eq!(result.bidder_responses.len(), 1);
        assert_eq!(result.bidder_responses[0].provider, "aps");

        // No mediator
        assert!(result.mediator_response.is_none());
    }

    #[tokio::test]
    async fn test_multiple_bidders() {
        let config = AuctionConfig {
            enabled: true,
            strategy: "parallel_only".to_string(),
            bidders: vec!["aps_mock".to_string()],
            mediator: None,
            timeout_ms: 2000,
        };

        let mut orchestrator = AuctionOrchestrator::new(config);

        // Register provider with different mock prices
        let aps_config = MockApsConfig {
            enabled: true,
            bid_price: 2.50,
            ..Default::default()
        };

        orchestrator.register_provider(Arc::new(MockApsProvider::new(aps_config)));

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_context(settings, &req);

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("auction should succeed");

        // Should have bids for both slots
        assert_eq!(result.winning_bids.len(), 2);
        assert!(result.winning_bids.contains_key("header-banner"));
        assert!(result.winning_bids.contains_key("sidebar"));
    }

    #[tokio::test]
    async fn test_orchestration_result_helpers() {
        let config = AuctionConfig {
            enabled: true,
            strategy: "parallel_only".to_string(),
            bidders: vec!["aps_mock".to_string()],
            mediator: None,
            timeout_ms: 2000,
        };

        let mut orchestrator = AuctionOrchestrator::new(config);

        let aps_config = MockApsConfig {
            enabled: true,
            ..Default::default()
        };

        orchestrator.register_provider(Arc::new(MockApsProvider::new(aps_config)));

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_context(settings, &req);

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("auction should succeed");

        // Test helper methods
        let header_bid = result.get_winning_bid("header-banner");
        assert!(header_bid.is_some());

        let all_header_bids = result.get_all_bids_for_slot("header-banner");
        assert!(!all_header_bids.is_empty());

        let total_bids = result.total_bids();
        assert!(total_bids > 0);
    }

    #[tokio::test]
    async fn test_unknown_strategy_error() {
        let config = AuctionConfig {
            enabled: true,
            strategy: "invalid_strategy".to_string(),
            bidders: vec!["aps_mock".to_string()],
            mediator: None,
            timeout_ms: 2000,
        };

        let orchestrator = AuctionOrchestrator::new(config);

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_context(settings, &req);

        let result = orchestrator.run_auction(&request, &context).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_msg = format!("{}", err);
        assert!(err_msg.contains("Unknown auction strategy"));
        assert!(err_msg.contains("parallel_mediation, parallel_only, waterfall"));
    }

    #[tokio::test]
    async fn test_no_bidders_configured() {
        let config = AuctionConfig {
            enabled: true,
            strategy: "parallel_only".to_string(),
            bidders: vec![],
            mediator: None,
            timeout_ms: 2000,
        };

        let orchestrator = AuctionOrchestrator::new(config);

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_context(settings, &req);

        let result = orchestrator.run_auction(&request, &context).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(format!("{}", err).contains("No bidders configured"));
    }

    #[test]
    fn test_orchestrator_is_enabled() {
        let config = AuctionConfig {
            enabled: true,
            ..Default::default()
        };
        let orchestrator = AuctionOrchestrator::new(config);
        assert!(orchestrator.is_enabled());

        let config = AuctionConfig {
            enabled: false,
            ..Default::default()
        };
        let orchestrator = AuctionOrchestrator::new(config);
        assert!(!orchestrator.is_enabled());
    }
}

impl OrchestrationResult {
    /// Get the winning bid for a specific slot.
    pub fn get_winning_bid(&self, slot_id: &str) -> Option<&Bid> {
        self.winning_bids.get(slot_id)
    }

    /// Get all bids from all providers for a specific slot.
    pub fn get_all_bids_for_slot(&self, slot_id: &str) -> Vec<&Bid> {
        self.bidder_responses
            .iter()
            .flat_map(|response| &response.bids)
            .filter(|bid| bid.slot_id == slot_id)
            .collect()
    }

    /// Get the total number of bids received.
    pub fn total_bids(&self) -> usize {
        self.bidder_responses.iter().map(|r| r.bids.len()).sum()
    }
}
