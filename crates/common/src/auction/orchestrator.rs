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

    /// Get the number of registered providers.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Execute an auction using the auto-detected strategy.
    ///
    /// Strategy is determined by mediator configuration:
    /// - If mediator is configured: runs parallel mediation (bidders → mediator decides)
    /// - If no mediator: runs parallel only (bidders → highest CPM wins)
    pub async fn run_auction(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        let start_time = Instant::now();

        // Auto-detect strategy based on mediator configuration
        let (strategy_name, result) = if self.config.has_mediator() {
            (
                "parallel_mediation",
                self.run_parallel_mediation(request, context).await?,
            )
        } else {
            (
                "parallel_only",
                self.run_parallel_only(request, context).await?,
            )
        };

        log::info!(
            "Running auction with strategy: {} (auto-detected from mediator config)",
            strategy_name
        );

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

            let start_time = Instant::now();
            let pending = mediator
                .request_bids(&mediation_request, context)
                .change_context(TrustedServerError::Auction {
                    message: format!("Mediator {} failed to launch", mediator.provider_name()),
                })?;

            let backend_response = pending.wait().change_context(TrustedServerError::Auction {
                message: format!("Mediator {} request failed", mediator.provider_name()),
            })?;

            let response_time_ms = start_time.elapsed().as_millis() as u64;
            let mediator_resp = mediator
                .parse_response(backend_response, response_time_ms)
                .change_context(TrustedServerError::Auction {
                    message: format!("Mediator {} parse failed", mediator.provider_name()),
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

    /// Run all bidders in parallel and collect responses.
    async fn run_bidders_parallel(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<Vec<AuctionResponse>, Report<TrustedServerError>> {
        use std::time::Instant;

        let bidder_names = self.config.bidder_names();

        if bidder_names.is_empty() {
            return Err(Report::new(TrustedServerError::Auction {
                message: "No bidders configured".to_string(),
            }));
        }

        log::info!(
            "Running {} bidders in parallel using send_async",
            bidder_names.len()
        );

        // Phase 1: Launch all requests concurrently
        let mut pending_requests = Vec::new();

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

            log::info!("Launching bid request to: {}", provider.provider_name());

            let start_time = Instant::now();
            match provider.request_bids(request, context) {
                Ok(pending) => {
                    pending_requests.push((
                        provider.provider_name(),
                        pending,
                        start_time,
                        provider.as_ref(),
                    ));
                    log::debug!(
                        "Request to '{}' launched successfully",
                        provider.provider_name()
                    );
                }
                Err(e) => {
                    log::warn!(
                        "Provider '{}' failed to launch request: {:?}",
                        provider.provider_name(),
                        e
                    );
                }
            }
        }

        log::info!(
            "Launched {} concurrent requests, waiting for responses...",
            pending_requests.len()
        );

        // Phase 2: Wait for all responses
        let mut responses = Vec::new();

        for (provider_name, pending, start_time, provider) in pending_requests {
            match pending.wait() {
                Ok(response) => {
                    let response_time_ms = start_time.elapsed().as_millis() as u64;

                    match provider.parse_response(response, response_time_ms) {
                        Ok(auction_response) => {
                            log::info!(
                                "Provider '{}' returned {} bids (status: {:?}, time: {}ms)",
                                auction_response.provider,
                                auction_response.bids.len(),
                                auction_response.status,
                                auction_response.response_time_ms
                            );
                            responses.push(auction_response);
                        }
                        Err(e) => {
                            log::warn!(
                                "Provider '{}' failed to parse response: {:?}",
                                provider_name,
                                e
                            );
                            responses.push(AuctionResponse::error(provider_name, response_time_ms));
                        }
                    }
                }
                Err(e) => {
                    let response_time_ms = start_time.elapsed().as_millis() as u64;
                    log::warn!("Provider '{}' request failed: {:?}", provider_name, e);
                    responses.push(AuctionResponse::error(provider_name, response_time_ms));
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
            log::warn!(
                "Provider '{}' configured but not registered. Available providers: {:?}",
                name,
                self.providers.keys().collect::<Vec<_>>()
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::*;
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

    fn create_test_settings() -> crate::settings::Settings {
        let settings_str = crate_test_settings_str();
        crate::settings::Settings::from_toml(&settings_str).expect("should parse test settings")
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

    // TODO: Re-enable these tests after implementing mock provider support for send_async()
    // Mock providers currently don't work with concurrent requests because they can't
    // create PendingRequest without real backends configured in Fastly.
    //
    // Options to fix:
    // 1. Configure dummy backends in fastly.toml for testing
    // 2. Refactor mock providers to use a different pattern
    // 3. Create a test-only mock backend server

    #[tokio::test]
    async fn test_no_bidders_configured() {
        let config = AuctionConfig {
            enabled: true,
            bidders: vec![],
            mediator: None,
            timeout_ms: 2000,
            creative_store: "creative_store".to_string(),
        };

        let orchestrator = AuctionOrchestrator::new(config);

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_context(&settings, &req);

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
