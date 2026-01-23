//! Auction orchestrator for managing multi-provider auctions.

use error_stack::{Report, ResultExt};
use fastly::http::request::{select, PendingRequest};
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
        let provider_responses = self.run_providers_parallel(request, context).await?;

        let floor_prices = self.floor_prices_by_slot(request);
        let (mediator_response, winning_bids) = if self.config.has_mediator() {
            let mediator_name = self.config.mediator.as_ref().unwrap();
            let mediator = self.get_provider(mediator_name)?;

            log::info!(
                "Sending {} provider responses to mediator: {}",
                provider_responses.len(),
                mediator.provider_name()
            );

            // Create a context with provider responses for the mediator
            let mediator_context = AuctionContext {
                settings: context.settings,
                request: context.request,
                timeout_ms: context.timeout_ms,
                provider_responses: Some(&provider_responses),
            };

            let start_time = Instant::now();
            let pending = mediator
                .request_bids(request, &mediator_context)
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
            // Filter out bids without decoded prices - mediator should have decoded all prices
            let winning = mediator_resp
                .bids
                .iter()
                .filter_map(|bid| {
                    if bid.price.is_none() {
                        log::warn!(
                            "Mediator '{}' returned bid for slot '{}' without decoded price - skipping. \
                             Mediator should decode all prices including APS bids.",
                            mediator.provider_name(),
                            bid.slot_id
                        );
                        None
                    } else {
                        Some((bid.slot_id.clone(), bid.clone()))
                    }
                })
                .collect();

            (
                Some(mediator_resp),
                self.apply_floor_prices(winning, &floor_prices),
            )
        } else {
            // No mediator - select best bid per slot from bidder responses
            let winning = self.select_winning_bids(&provider_responses, &floor_prices);
            (None, winning)
        };

        Ok(OrchestrationResult {
            provider_responses,
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
        let provider_responses = self.run_providers_parallel(request, context).await?;
        let floor_prices = self.floor_prices_by_slot(request);
        let winning_bids = self.select_winning_bids(&provider_responses, &floor_prices);

        Ok(OrchestrationResult {
            provider_responses,
            mediator_response: None,
            winning_bids,
            total_time_ms: 0,
            metadata: HashMap::new(),
        })
    }

    /// Run all providers in parallel and collect responses.
    ///
    /// Uses `fastly::http::request::select()` to process responses as they
    /// become ready, rather than waiting for each response sequentially.
    async fn run_providers_parallel(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<Vec<AuctionResponse>, Report<TrustedServerError>> {
        let provider_names = self.config.provider_names();

        if provider_names.is_empty() {
            return Err(Report::new(TrustedServerError::Auction {
                message: "No providers configured".to_string(),
            }));
        }

        log::info!(
            "Running {} providers in parallel using select",
            provider_names.len()
        );

        // Phase 1: Launch all requests concurrently and build mapping
        // Maps backend_name -> (provider_name, start_time, provider)
        let mut backend_to_provider: HashMap<String, (&str, Instant, &dyn AuctionProvider)> =
            HashMap::new();
        let mut pending_requests: Vec<PendingRequest> = Vec::new();

        for provider_name in provider_names {
            let provider = match self.providers.get(provider_name) {
                Some(p) => p,
                None => {
                    log::warn!("Provider '{}' not registered, skipping", provider_name);
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

            // Get the backend name for this provider to map responses back
            let backend_name = match provider.backend_name() {
                Some(name) => name,
                None => {
                    log::warn!(
                        "Provider '{}' has no backend_name, skipping",
                        provider.provider_name()
                    );
                    continue;
                }
            };

            log::info!(
                "Launching bid request to: {} (backend: {})",
                provider.provider_name(),
                backend_name
            );

            let start_time = Instant::now();
            match provider.request_bids(request, context) {
                Ok(pending) => {
                    backend_to_provider.insert(
                        backend_name,
                        (provider.provider_name(), start_time, provider.as_ref()),
                    );
                    pending_requests.push(pending);
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
            "Launched {} concurrent requests, waiting for responses using select...",
            pending_requests.len()
        );

        // Phase 2: Wait for responses using select() to process as they become ready
        let mut responses = Vec::new();
        let mut remaining = pending_requests;

        while !remaining.is_empty() {
            let (result, rest) = select(remaining);
            remaining = rest;

            match result {
                Ok(response) => {
                    // Identify the provider from the backend name
                    let backend_name = response.get_backend_name().unwrap_or_default().to_string();

                    if let Some((provider_name, start_time, provider)) =
                        backend_to_provider.remove(&backend_name)
                    {
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
                                responses
                                    .push(AuctionResponse::error(provider_name, response_time_ms));
                            }
                        }
                    } else {
                        log::warn!(
                            "Received response from unknown backend '{}', ignoring",
                            backend_name
                        );
                    }
                }
                Err(e) => {
                    // When select() returns an error, we can't easily identify which
                    // provider failed since the PendingRequest is consumed
                    log::warn!("A provider request failed: {:?}", e);
                }
            }
        }

        Ok(responses)
    }

    /// Select the best bid for each slot from all responses.
    /// Note: Bids with None price (e.g., APS bids with encoded prices) are skipped
    /// when no mediator is configured, as we cannot compare them without decoding.
    fn select_winning_bids(
        &self,
        responses: &[AuctionResponse],
        floor_prices: &HashMap<String, f64>,
    ) -> HashMap<String, Bid> {
        let mut winning_bids: HashMap<String, Bid> = HashMap::new();

        for response in responses {
            if response.status != BidStatus::Success {
                continue;
            }

            for bid in &response.bids {
                // Skip bids without decoded prices (e.g., APS bids)
                // These require mediation layer to decode
                let bid_price = match bid.price {
                    Some(p) => p,
                    None => {
                        log::debug!(
                            "Skipping bid for slot '{}' from '{}' - price requires mediation to decode",
                            bid.slot_id,
                            bid.bidder
                        );
                        continue;
                    }
                };

                let should_replace = match winning_bids.get(&bid.slot_id) {
                    Some(current_winner) => current_winner
                        .price
                        .is_none_or(|current_price| bid_price > current_price),
                    None => true,
                };

                if should_replace {
                    winning_bids.insert(bid.slot_id.clone(), bid.clone());
                }
            }
        }

        self.apply_floor_prices(winning_bids, floor_prices)
    }

    fn apply_floor_prices(
        &self,
        mut winning_bids: HashMap<String, Bid>,
        floor_prices: &HashMap<String, f64>,
    ) -> HashMap<String, Bid> {
        if floor_prices.is_empty() {
            log::info!("Selected {} winning bids", winning_bids.len());
            return winning_bids;
        }

        let starting_count = winning_bids.len();
        winning_bids.retain(|slot_id, bid| match floor_prices.get(slot_id) {
            Some(floor) => {
                // Bids without price (e.g., APS) pass through - floor checked in mediation
                match bid.price {
                    Some(price) if price >= *floor => true,
                    Some(_) => {
                        log::info!(
                            "Dropping winning bid below floor price for slot '{}'",
                            slot_id
                        );
                        false
                    }
                    None => {
                        log::debug!(
                            "Passing bid with encoded price for slot '{}' - floor check deferred to mediation",
                            slot_id
                        );
                        true
                    }
                }
            }
            None => true,
        });

        if winning_bids.len() != starting_count {
            log::info!(
                "Filtered winning bids by floor price: {} -> {}",
                starting_count,
                winning_bids.len()
            );
        }

        log::info!("Selected {} winning bids", winning_bids.len());
        winning_bids
    }

    fn floor_prices_by_slot(&self, request: &AuctionRequest) -> HashMap<String, f64> {
        request
            .slots
            .iter()
            .filter_map(|slot| slot.floor_price.map(|price| (slot.id.clone(), price)))
            .collect()
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
    /// All responses from providers
    pub provider_responses: Vec<AuctionResponse>,
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
        self.provider_responses
            .iter()
            .flat_map(|response| &response.bids)
            .filter(|bid| bid.slot_id == slot_id)
            .collect()
    }

    /// Get the total number of bids received.
    pub fn total_bids(&self) -> usize {
        self.provider_responses.iter().map(|r| r.bids.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use crate::auction::config::AuctionConfig;
    use crate::auction::types::{
        AdFormat, AdSlot, AuctionContext, AuctionRequest, Bid, MediaType, PublisherInfo, UserInfo,
    };
    use crate::test_support::tests::crate_test_settings_str;
    use fastly::Request;
    use std::collections::HashMap;

    use super::AuctionOrchestrator;

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
            provider_responses: None,
        }
    }

    #[test]
    fn filters_winning_bids_below_floor() {
        let orchestrator = AuctionOrchestrator::new(AuctionConfig::default());
        let mut floor_prices = HashMap::new();
        floor_prices.insert("slot-1".to_string(), 1.00);
        floor_prices.insert("slot-2".to_string(), 2.00);

        // Arrange winning bids with one below floor.
        let mut winning_bids = HashMap::new();
        winning_bids.insert(
            "slot-1".to_string(),
            Bid {
                slot_id: "slot-1".to_string(),
                price: Some(0.50),
                currency: "USD".to_string(),
                creative: Some("<div>Ad</div>".to_string()),
                adomain: None,
                bidder: "test-bidder".to_string(),
                width: 300,
                height: 250,
                nurl: None,
                burl: None,
                metadata: HashMap::new(),
            },
        );
        winning_bids.insert(
            "slot-2".to_string(),
            Bid {
                slot_id: "slot-2".to_string(),
                price: Some(2.00),
                currency: "USD".to_string(),
                creative: Some("<div>Ad</div>".to_string()),
                adomain: None,
                bidder: "test-bidder".to_string(),
                width: 300,
                height: 250,
                nurl: None,
                burl: None,
                metadata: HashMap::new(),
            },
        );

        // Apply floor pricing and validate the results.
        let filtered = orchestrator.apply_floor_prices(winning_bids, &floor_prices);

        assert_eq!(
            filtered.len(),
            1,
            "Filtered bids should keep only those meeting floor price"
        );
        assert!(
            filtered.contains_key("slot-2"),
            "Filtered bids should include slot-2 winner"
        );
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
    async fn test_no_providers_configured() {
        let config = AuctionConfig {
            enabled: true,
            providers: vec![],
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
        assert!(format!("{}", err).contains("No providers configured"));
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

    #[test]
    fn test_apply_floor_prices_allows_none_prices_for_encoded_bids() {
        // Test that bids with None prices (APS-style) pass through floor pricing
        // This is correct behavior for parallel-only strategy where mediation happens later
        let orchestrator = AuctionOrchestrator::new(AuctionConfig::default());
        let mut floor_prices = HashMap::new();
        floor_prices.insert("slot-1".to_string(), 1.00);

        let mut winning_bids = HashMap::new();
        winning_bids.insert(
            "slot-1".to_string(),
            Bid {
                slot_id: "slot-1".to_string(),
                price: None, // APS bid with encoded price
                currency: "USD".to_string(),
                creative: Some("<div>Ad</div>".to_string()),
                adomain: None,
                bidder: "aps".to_string(),
                width: 300,
                height: 250,
                nurl: None,
                burl: None,
                metadata: {
                    let mut m = HashMap::new();
                    m.insert(
                        "amznbid".to_string(),
                        serde_json::json!("encoded_price_data"),
                    );
                    m
                },
            },
        );

        // Apply floor pricing - should pass through with None price
        let filtered = orchestrator.apply_floor_prices(winning_bids, &floor_prices);

        assert_eq!(
            filtered.len(),
            1,
            "APS bid with None price should pass through floor check"
        );
        assert!(
            filtered.contains_key("slot-1"),
            "Slot-1 should still be present"
        );
        assert!(
            filtered.get("slot-1").unwrap().price.is_none(),
            "Price should still be None (not decoded yet)"
        );
    }
}
