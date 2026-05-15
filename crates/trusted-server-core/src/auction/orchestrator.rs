//! Auction orchestrator for managing multi-provider auctions.

use error_stack::{Report, ResultExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::TrustedServerError;
use crate::platform::{PlatformPendingRequest, RuntimeServices};
use crate::proxy::platform_response_to_fastly;

use super::config::AuctionConfig;
use super::provider::AuctionProvider;
use super::types::{AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus};

/// In-flight auction requests dispatched to SSP backends.
///
/// Created by [`AuctionOrchestrator::dispatch_auction`] and consumed by
/// [`AuctionOrchestrator::collect_dispatched_auction`]. Carrying this handle
/// across `pending_origin.wait()` lets origin response and SSP HTTP requests
/// race in Fastly's native layer, enabling TTFB ≈ origin latency rather than
/// TTFB ≈ auction timeout.
pub struct DispatchedAuction {
    pending_requests: Vec<PlatformPendingRequest>,
    backend_to_provider: HashMap<String, (String, Instant, Arc<dyn AuctionProvider>)>,
    auction_start: Instant,
    timeout_ms: u32,
    floor_prices: HashMap<String, f64>,
    /// Carried so the mediator call in collect can pass it as the auction request.
    request: AuctionRequest,
}

/// Compute the remaining time budget from a deadline.
///
/// Returns the number of milliseconds left before `timeout_ms` is exceeded,
/// measured from `start`. Returns `0` when the deadline has already passed.
#[inline]
fn remaining_budget_ms(start: Instant, timeout_ms: u32) -> u32 {
    let elapsed = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);
    timeout_ms.saturating_sub(elapsed)
}

/// Manages auction execution across multiple providers.
pub struct AuctionOrchestrator {
    config: AuctionConfig,
    providers: HashMap<String, Arc<dyn AuctionProvider>>,
}

impl AuctionOrchestrator {
    /// Create a new orchestrator with the given configuration.
    #[must_use]
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
    #[must_use]
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Execute an auction using the auto-detected strategy.
    ///
    /// Strategy is determined by mediator configuration:
    /// - If mediator is configured: runs parallel mediation (bidders → mediator decides)
    /// - If no mediator: runs parallel only (bidders → highest CPM wins)
    ///
    /// # Errors
    ///
    /// Returns an error if the auction execution fails due to provider errors or
    /// mediation errors.
    pub async fn run_auction(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
        services: &RuntimeServices,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        let start_time = Instant::now();

        // Auto-detect strategy based on mediator configuration
        let (strategy_name, result) = if self.config.has_mediator() {
            (
                "parallel_mediation",
                self.run_parallel_mediation(request, context, services)
                    .await?,
            )
        } else {
            (
                "parallel_only",
                self.run_parallel_only(request, context, services).await?,
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
        services: &RuntimeServices,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        let mediation_start = Instant::now();
        let provider_responses = self
            .run_providers_parallel(request, context, services)
            .await?;

        let floor_prices = self.floor_prices_by_slot(request);
        let (mediator_response, winning_bids) = if let Some(mediator_name) = &self.config.mediator {
            let mediator = self.get_provider(mediator_name)?;

            log::info!(
                "Sending {} provider responses to mediator: {}",
                provider_responses.len(),
                mediator.provider_name()
            );

            // Give the mediator only the remaining time from the auction
            // deadline, not the full timeout — the bidding phase already
            // consumed part of it.
            let remaining_ms = remaining_budget_ms(mediation_start, context.timeout_ms);

            if remaining_ms == 0 {
                log::warn!(
                    "Auction timeout ({}ms) exhausted during bidding phase — skipping mediator",
                    context.timeout_ms
                );
                let winning = self.select_winning_bids(&provider_responses, &floor_prices);
                return Ok(OrchestrationResult {
                    provider_responses,
                    mediator_response: None,
                    winning_bids: winning,
                    total_time_ms: 0,
                    metadata: HashMap::new(),
                });
            }

            let mediator_context = AuctionContext {
                settings: context.settings,
                request: context.request,
                client_info: context.client_info,
                timeout_ms: remaining_ms,
                provider_responses: Some(&provider_responses),
                services: context.services,
            };

            let start_time = Instant::now();
            let pending = mediator
                .request_bids(request, &mediator_context)
                .change_context(TrustedServerError::Auction {
                    message: format!("Mediator {} failed to launch", mediator.provider_name()),
                })?;

            let platform_resp = services
                .http_client()
                .wait(PlatformPendingRequest::new(pending))
                .await
                .change_context(TrustedServerError::Auction {
                    message: format!("Mediator {} request failed", mediator.provider_name()),
                })?;
            let backend_response = platform_response_to_fastly(platform_resp).change_context(
                TrustedServerError::Auction {
                    message: format!(
                        "Mediator {} returned an unsupported response body",
                        mediator.provider_name()
                    ),
                },
            )?;

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
        services: &RuntimeServices,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        let provider_responses = self
            .run_providers_parallel(request, context, services)
            .await?;
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
    /// Uses [`RuntimeServices::http_client`] and
    /// [`crate::platform::PlatformHttpClient::select`] to process responses as
    /// they become ready, rather than waiting for each response sequentially.
    async fn run_providers_parallel(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
        services: &RuntimeServices,
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

        // Track auction start time for deadline enforcement
        let auction_start = Instant::now();

        // Phase 1: Launch all requests concurrently and build mapping
        // Maps backend_name -> (provider_name, start_time, provider)
        let mut backend_to_provider: HashMap<String, (&str, Instant, &dyn AuctionProvider)> =
            HashMap::new();
        let mut pending_requests: Vec<PlatformPendingRequest> = Vec::new();

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

            // Give each provider only the remaining time from the auction
            // deadline so that its backend first_byte_timeout doesn't extend
            // past the overall budget. Also respect the provider's own
            // configured timeout when it is tighter than the remaining budget.
            let remaining_ms = remaining_budget_ms(auction_start, context.timeout_ms);
            let effective_timeout = remaining_ms.min(provider.timeout_ms());

            if effective_timeout == 0 {
                log::warn!(
                    "Auction timeout ({}ms) exhausted before launching '{}' — skipping",
                    context.timeout_ms,
                    provider.provider_name()
                );
                continue;
            }

            // Get the backend name for this provider to map responses back.
            // Must be computed after effective_timeout since the timeout is
            // part of the backend name.
            let backend_name = match provider.backend_name(effective_timeout) {
                Some(name) => name,
                None => {
                    log::warn!(
                        "Provider '{}' has no backend_name, skipping",
                        provider.provider_name()
                    );
                    continue;
                }
            };

            let provider_context = AuctionContext {
                settings: context.settings,
                request: context.request,
                client_info: context.client_info,
                timeout_ms: effective_timeout,
                provider_responses: context.provider_responses,
                services: context.services,
            };

            log::info!(
                "Launching bid request to: {} (backend: {}, budget: {}ms)",
                provider.provider_name(),
                backend_name,
                effective_timeout
            );

            let start_time = Instant::now();
            match provider.request_bids(request, &provider_context) {
                Ok(pending) => {
                    backend_to_provider.insert(
                        backend_name.clone(),
                        (provider.provider_name(), start_time, provider.as_ref()),
                    );
                    pending_requests
                        .push(PlatformPendingRequest::new(pending).with_backend_name(backend_name));
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

        let deadline = Duration::from_millis(u64::from(context.timeout_ms));
        log::info!(
            "Launched {} concurrent requests, waiting for responses (timeout: {}ms)...",
            pending_requests.len(),
            context.timeout_ms
        );

        // Phase 2: Wait for responses using select() to process as they become ready.
        // Enforce the auction deadline: after each select() returns, check
        // elapsed time and drop remaining requests if the timeout is exceeded.
        //
        // NOTE: `select()` blocks until at least one backend responds (or its
        // transport timeout fires). Hard deadline enforcement therefore depends
        // on every backend's `first_byte_timeout` being set to at most the
        // remaining auction budget — which Phase 1 above guarantees.
        let mut responses = Vec::new();
        let mut remaining = pending_requests;

        while !remaining.is_empty() {
            let select_result = services
                .http_client()
                .select(remaining)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "HTTP select failed".to_string(),
                })?;
            remaining = select_result.remaining;

            match select_result.ready {
                Ok(platform_response) => {
                    // Identify the provider from the backend name
                    let backend_name = platform_response.backend_name.clone().unwrap_or_default();

                    if let Some((provider_name, start_time, provider)) =
                        backend_to_provider.remove(&backend_name)
                    {
                        let response_time_ms = start_time.elapsed().as_millis() as u64;

                        match platform_response_to_fastly(platform_response) {
                            Ok(response) => {
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
                                        responses.push(AuctionResponse::error(
                                            provider_name,
                                            response_time_ms,
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "Provider '{}' returned an unsupported response body: {:?}",
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

            // Check auction deadline after processing each response.
            // Remaining PendingRequests are dropped, which abandons the
            // in-flight HTTP calls on the Fastly host.
            if auction_start.elapsed() >= deadline && !remaining.is_empty() {
                log::warn!(
                    "Auction timeout ({}ms) reached, dropping {} remaining request(s)",
                    context.timeout_ms,
                    remaining.len()
                );
                break;
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
                // price=None means the SSP returned an encoded price (e.g. APS amznbid).
                // In the parallel-only path this bid cannot yet be floor-checked; it passes
                // through and will be decoded (and re-checked) by the mediation layer.
                // In the mediation path, mediation decodes prices before calling this
                // function, so any bid still carrying price=None is dropped upstream.
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
                            "Passing encoded-price bid for slot '{}' - price not yet decoded",
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

    /// Dispatch SSP bid requests without blocking WASM.
    ///
    /// Calls each enabled provider's [`AuctionProvider::request_bids`] (which
    /// internally calls Fastly's `send_async`), then returns immediately with a
    /// [`DispatchedAuction`] token. The Fastly host begins the SSP round-trips
    /// while WASM continues to `pending_origin.wait()`.
    ///
    /// Returns `None` when no providers are configured or all providers are
    /// disabled / over budget. The caller should fall back to the synchronous
    /// `run_auction` path.
    #[must_use]
    pub fn dispatch_auction(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Option<DispatchedAuction> {
        let provider_names = self.config.provider_names();
        if provider_names.is_empty() {
            return None;
        }

        let auction_start = Instant::now();
        let mut backend_to_provider: HashMap<String, (String, Instant, Arc<dyn AuctionProvider>)> =
            HashMap::new();
        let mut pending_requests: Vec<PlatformPendingRequest> = Vec::new();

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

            let remaining_ms = remaining_budget_ms(auction_start, context.timeout_ms);
            let effective_timeout = remaining_ms.min(provider.timeout_ms());

            if effective_timeout == 0 {
                log::warn!(
                    "Auction timeout ({}ms) exhausted before launching '{}' — skipping",
                    context.timeout_ms,
                    provider.provider_name()
                );
                continue;
            }

            let backend_name = match provider.backend_name(effective_timeout) {
                Some(name) => name,
                None => {
                    log::warn!(
                        "Provider '{}' has no backend_name, skipping",
                        provider.provider_name()
                    );
                    continue;
                }
            };

            let provider_context = AuctionContext {
                settings: context.settings,
                request: context.request,
                client_info: context.client_info,
                timeout_ms: effective_timeout,
                provider_responses: context.provider_responses,
                services: context.services,
            };

            let start_time = Instant::now();
            match provider.request_bids(request, &provider_context) {
                Ok(pending) => {
                    log::info!(
                        "Dispatching bid request to '{}' (backend: {}, budget: {}ms)",
                        provider.provider_name(),
                        backend_name,
                        effective_timeout
                    );
                    backend_to_provider.insert(
                        backend_name.clone(),
                        (
                            provider.provider_name().to_string(),
                            start_time,
                            Arc::clone(provider),
                        ),
                    );
                    pending_requests
                        .push(PlatformPendingRequest::new(pending).with_backend_name(backend_name));
                }
                Err(e) => {
                    log::warn!(
                        "Provider '{}' failed to dispatch request: {:?}",
                        provider.provider_name(),
                        e
                    );
                }
            }
        }

        if pending_requests.is_empty() {
            return None;
        }

        log::info!(
            "Dispatched {} SSP requests (timeout: {}ms); Fastly host will race them against origin",
            pending_requests.len(),
            context.timeout_ms
        );

        Some(DispatchedAuction {
            pending_requests,
            backend_to_provider,
            auction_start,
            timeout_ms: context.timeout_ms,
            floor_prices: self.floor_prices_by_slot(request),
            request: request.clone(),
        })
    }

    /// Collect bid responses from a previously-dispatched auction.
    ///
    /// Runs the select-loop phase (equivalent to Phase 2 of
    /// `run_providers_parallel`) and, if the orchestrator has a mediator
    /// configured, forwards collected bids to it. The overall auction deadline
    /// is enforced from `dispatched.auction_start`.
    ///
    /// On any error or partial failure the method returns the best available
    /// result rather than propagating — the caller should still inject the
    /// winning bids even if some providers timed out.
    pub async fn collect_dispatched_auction(
        &self,
        dispatched: DispatchedAuction,
        services: &RuntimeServices,
        context: &AuctionContext<'_>,
    ) -> OrchestrationResult {
        let DispatchedAuction {
            pending_requests,
            mut backend_to_provider,
            auction_start,
            timeout_ms,
            floor_prices,
            request,
        } = dispatched;

        log::info!(
            "Collecting {} in-flight SSP responses (timeout: {}ms remaining: {}ms)",
            pending_requests.len(),
            timeout_ms,
            remaining_budget_ms(auction_start, timeout_ms),
        );

        let mut responses: Vec<AuctionResponse> = Vec::new();
        let mut remaining = pending_requests;

        while !remaining.is_empty() {
            let select_result = match services
                .http_client()
                .select(remaining)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "HTTP select failed".to_string(),
                }) {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("select() failed during auction collection: {:?}", e);
                    break;
                }
            };
            remaining = select_result.remaining;

            match select_result.ready {
                Ok(platform_response) => {
                    let backend_name = platform_response.backend_name.clone().unwrap_or_default();
                    if let Some((provider_name, start_time, provider)) =
                        backend_to_provider.remove(&backend_name)
                    {
                        let response_time_ms = start_time.elapsed().as_millis() as u64;
                        match platform_response_to_fastly(platform_response) {
                            Ok(response) => {
                                match provider.parse_response(response, response_time_ms) {
                                    Ok(auction_response) => {
                                        log::info!(
                                            "Provider '{}' returned {} bids ({}ms)",
                                            auction_response.provider,
                                            auction_response.bids.len(),
                                            auction_response.response_time_ms
                                        );
                                        responses.push(auction_response);
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "Provider '{}' parse failed: {:?}",
                                            provider_name,
                                            e
                                        );
                                        responses.push(AuctionResponse::error(
                                            &provider_name,
                                            response_time_ms,
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "Provider '{}' unsupported body: {:?}",
                                    provider_name,
                                    e
                                );
                                responses
                                    .push(AuctionResponse::error(&provider_name, response_time_ms));
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
                    log::warn!("A provider request failed during collection: {:?}", e);
                }
            }
        }

        let (mediator_response, winning_bids) = if let Some(mediator_name) = &self.config.mediator {
            match self.providers.get(mediator_name.as_str()) {
                Some(mediator) => {
                    // Cap the mediator at whichever is tighter: its own configured
                    // timeout or the remaining auction budget (A_deadline).  The old
                    // comment here claimed origin drain could exhaust the budget before
                    // collection, but SSP backends are given first_byte_timeout =
                    // effective_timeout (capped at their provider timeout) at dispatch
                    // time, so they cannot run past A_deadline independently.  Giving
                    // the mediator an uncapped timeout lets it run past A_deadline,
                    // violating the bounded hold invariant.
                    let remaining = remaining_budget_ms(auction_start, timeout_ms);
                    if remaining == 0 {
                        log::warn!(
                            "A_deadline exhausted before mediator '{}' — returning {} SSP bids without mediation",
                            mediator.provider_name(),
                            responses.len(),
                        );
                        let winning = self.select_winning_bids(&responses, &floor_prices);
                        return OrchestrationResult {
                            provider_responses: responses,
                            mediator_response: None,
                            winning_bids: winning,
                            total_time_ms: auction_start.elapsed().as_millis() as u64,
                            metadata: HashMap::new(),
                        };
                    }
                    let mediator_timeout = remaining.min(mediator.timeout_ms());
                    let mediator_start = Instant::now();
                    log::info!(
                        "Running mediator '{}' with {}ms budget (A_deadline remaining: {}ms, configured: {}ms)",
                        mediator.provider_name(),
                        mediator_timeout,
                        remaining,
                        mediator.timeout_ms(),
                    );
                    let placeholder = fastly::Request::get("https://placeholder.invalid/");
                    let mediator_context = AuctionContext {
                        settings: context.settings,
                        request: &placeholder,
                        client_info: context.client_info,
                        timeout_ms: mediator_timeout,
                        provider_responses: Some(&responses),
                        services: context.services,
                    };
                    match mediator.request_bids(&request, &mediator_context) {
                        Ok(pending) => {
                            let platform_resp = services
                                .http_client()
                                .wait(PlatformPendingRequest::new(pending))
                                .await;
                            match platform_resp.change_context(TrustedServerError::Auction {
                                message: format!(
                                    "Mediator {} request failed",
                                    mediator.provider_name()
                                ),
                            }) {
                                Ok(platform_resp) => {
                                    match platform_response_to_fastly(platform_resp).change_context(
                                        TrustedServerError::Auction {
                                            message: format!(
                                                "Mediator {} unsupported body",
                                                mediator.provider_name()
                                            ),
                                        },
                                    ) {
                                        Ok(response) => {
                                            let response_time_ms =
                                                mediator_start.elapsed().as_millis() as u64;
                                            match mediator
                                                .parse_response(response, response_time_ms)
                                            {
                                                Ok(mediator_resp) => {
                                                    let winning = mediator_resp
                                                        .bids
                                                        .iter()
                                                        .filter_map(|bid| {
                                                            if bid.price.is_none() {
                                                                log::warn!(
                                                                    "Mediator '{}' returned bid for slot '{}' without decoded price - skipping",
                                                                    mediator.provider_name(),
                                                                    bid.slot_id
                                                                );
                                                                None
                                                            } else {
                                                                Some((bid.slot_id.clone(), bid.clone()))
                                                            }
                                                        })
                                                        .collect();
                                                    let winning = self
                                                        .apply_floor_prices(winning, &floor_prices);
                                                    (Some(mediator_resp), winning)
                                                }
                                                Err(e) => {
                                                    log::warn!(
                                                        "Mediator '{}' parse failed: {:?}",
                                                        mediator.provider_name(),
                                                        e
                                                    );
                                                    let winning = self.select_winning_bids(
                                                        &responses,
                                                        &floor_prices,
                                                    );
                                                    (None, winning)
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            log::warn!("Mediator body error: {:?}", e);
                                            (
                                                None,
                                                self.select_winning_bids(&responses, &floor_prices),
                                            )
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::warn!("Mediator request failed: {:?}", e);
                                    (None, self.select_winning_bids(&responses, &floor_prices))
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "Mediator '{}' failed to dispatch: {:?}",
                                mediator.provider_name(),
                                e
                            );
                            (None, self.select_winning_bids(&responses, &floor_prices))
                        }
                    }
                }
                None => {
                    log::warn!("Mediator '{}' not registered", mediator_name);
                    (None, self.select_winning_bids(&responses, &floor_prices))
                }
            }
        } else {
            (None, self.select_winning_bids(&responses, &floor_prices))
        };

        OrchestrationResult {
            provider_responses: responses,
            mediator_response,
            winning_bids,
            total_time_ms: auction_start.elapsed().as_millis() as u64,
            metadata: HashMap::new(),
        }
    }

    /// Check if orchestrator is enabled.
    #[must_use]
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
    #[must_use]
    pub fn get_winning_bid(&self, slot_id: &str) -> Option<&Bid> {
        self.winning_bids.get(slot_id)
    }

    /// Get all bids from all providers for a specific slot.
    #[must_use]
    pub fn get_all_bids_for_slot(&self, slot_id: &str) -> Vec<&Bid> {
        self.provider_responses
            .iter()
            .flat_map(|response| &response.bids)
            .filter(|bid| bid.slot_id == slot_id)
            .collect()
    }

    /// Get the total number of bids received.
    #[must_use]
    pub fn total_bids(&self) -> usize {
        self.provider_responses.iter().map(|r| r.bids.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use crate::auction::config::AuctionConfig;
    use crate::auction::test_support::create_test_auction_context;
    use crate::auction::types::{
        AdFormat, AdSlot, AuctionRequest, Bid, MediaType, PublisherInfo, UserInfo,
    };

    // All-None ClientInfo used across tests that don't need real IP/TLS data.
    // Defined as a const so &EMPTY_CLIENT_INFO has 'static lifetime, avoiding
    // the temporary-lifetime issue that arises with &ClientInfo::default().
    const EMPTY_CLIENT_INFO: crate::platform::ClientInfo = crate::platform::ClientInfo {
        client_ip: None,
        tls_protocol: None,
        tls_cipher: None,
    };
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::crate_test_settings_str;
    use fastly::Request;
    use std::collections::{HashMap, HashSet};

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
                    bidders: HashMap::new(),
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
                    bidders: HashMap::new(),
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
                ad_id: None,
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
                ad_id: None,
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

    // TODO: Re-enable provider integration tests after implementing mock support
    // for send_async(). Mock providers can't create PendingRequest without real
    // Fastly backends.
    //
    // Untested timeout enforcement paths (require real backends):
    // - Deadline check in select() loop (drops remaining requests)
    // - Mediator skip when remaining_ms == 0 (bidding exhausts budget)
    // - Provider skip when effective_timeout == 0 (budget exhausted before launch)
    // - Provider context receives reduced timeout_ms per remaining budget
    //
    // Follow-up: introduce a thin abstraction over `select()` (e.g. a trait)
    // so the deadline/drop logic can be unit-tested with mock futures instead
    // of requiring real Fastly backends.  An `#[ignore]` integration test
    // exercising the full path via Viceroy would also catch regressions.

    #[tokio::test]
    async fn test_no_providers_configured() {
        let config = AuctionConfig {
            enabled: true,
            providers: vec![],
            mediator: None,
            timeout_ms: 2000,
            creative_store: "creative_store".to_string(),
            allowed_context_keys: HashSet::from(["permutive_segments".to_string()]),
        };

        let orchestrator = AuctionOrchestrator::new(config);

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_auction_context(&settings, &req, &EMPTY_CLIENT_INFO, 2000);

        let result = orchestrator
            .run_auction(&request, &context, &noop_services())
            .await;

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
    fn remaining_budget_returns_full_timeout_immediately() {
        let start = std::time::Instant::now();
        let result = super::remaining_budget_ms(start, 2000);
        // Should be very close to 2000 (allow a few ms for test execution)
        assert!(
            result >= 1990,
            "should return ~full timeout immediately, got {result}"
        );
    }

    #[test]
    fn remaining_budget_saturates_at_zero() {
        // Create an instant in the past by sleeping briefly with a tiny timeout
        let start = std::time::Instant::now();
        // Use a timeout of 0 — elapsed will always exceed it
        let result = super::remaining_budget_ms(start, 0);
        assert_eq!(result, 0, "should return 0 when timeout is 0");
    }

    #[test]
    fn remaining_budget_decreases_over_time() {
        let start = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let result = super::remaining_budget_ms(start, 2000);
        assert!(
            result < 2000,
            "should be less than full timeout after sleeping"
        );
        assert!(
            result > 1900,
            "should still have most of the budget, got {result}"
        );
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
                ad_id: None,
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
            filtered
                .get("slot-1")
                .expect("slot-1 should be present")
                .price
                .is_none(),
            "Price should still be None (not decoded yet)"
        );
    }

    #[test]
    fn test_apply_floor_prices_drops_decoded_aps_bid_below_floor() {
        // After mediation decodes an APS bid, apply_floor_prices must enforce the
        // slot floor on the resulting price=Some(x) value. This test simulates the
        // state of a bid after mediator decoding: price is Some, amznbid is gone.
        let orchestrator = AuctionOrchestrator::new(AuctionConfig::default());
        let mut floor_prices = HashMap::new();
        floor_prices.insert("atf".to_string(), 0.50);

        let mut winning_bids = HashMap::new();
        winning_bids.insert(
            "atf".to_string(),
            Bid {
                slot_id: "atf".to_string(),
                price: Some(0.30), // decoded APS price — below $0.50 floor
                currency: "USD".to_string(),
                creative: Some("<div>APS Ad</div>".to_string()),
                adomain: None,
                bidder: "aps".to_string(),
                width: 300,
                height: 250,
                nurl: None,
                burl: None,
                ad_id: None,
                metadata: HashMap::new(),
            },
        );

        let filtered = orchestrator.apply_floor_prices(winning_bids, &floor_prices);

        assert!(
            filtered.is_empty(),
            "Decoded APS bid below slot floor should be dropped"
        );
    }

    #[test]
    fn test_apply_floor_prices_keeps_decoded_aps_bid_at_or_above_floor() {
        let orchestrator = AuctionOrchestrator::new(AuctionConfig::default());
        let mut floor_prices = HashMap::new();
        floor_prices.insert("atf".to_string(), 0.50);

        let mut winning_bids = HashMap::new();
        winning_bids.insert(
            "atf".to_string(),
            Bid {
                slot_id: "atf".to_string(),
                price: Some(0.75), // decoded APS price — above floor
                currency: "USD".to_string(),
                creative: Some("<div>APS Ad</div>".to_string()),
                adomain: None,
                bidder: "aps".to_string(),
                width: 300,
                height: 250,
                nurl: None,
                burl: None,
                ad_id: None,
                metadata: HashMap::new(),
            },
        );

        let filtered = orchestrator.apply_floor_prices(winning_bids, &floor_prices);

        assert_eq!(
            filtered.len(),
            1,
            "Decoded APS bid at or above floor should be kept"
        );
        assert_eq!(
            filtered.get("atf").expect("atf should be present").price,
            Some(0.75),
            "Price should be preserved"
        );
    }
}
