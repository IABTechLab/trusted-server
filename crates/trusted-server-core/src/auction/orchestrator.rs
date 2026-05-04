//! Auction orchestrator for managing multi-provider auctions.

use error_stack::{Report, ResultExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::TrustedServerError;
use crate::platform::{PlatformPendingRequest, PlatformPollResult, RuntimeServices};
use crate::proxy::platform_response_to_fastly;

use super::config::AuctionConfig;
use super::provider::AuctionProvider;
use super::types::{AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus};

/// Compute the remaining time budget from a deadline.
///
/// Returns the number of milliseconds left before `timeout_ms` is exceeded,
/// measured from `start`. Returns `0` when the deadline has already passed.
#[inline]
fn remaining_budget_ms(start: Instant, timeout_ms: u32) -> u32 {
    let elapsed = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);
    timeout_ms.saturating_sub(elapsed)
}

fn select_winning_bids_from_responses(
    responses: &[AuctionResponse],
    floor_prices: &HashMap<String, f64>,
) -> HashMap<String, Bid> {
    let mut winning_bids: HashMap<String, Bid> = HashMap::new();

    for response in responses {
        if response.status != BidStatus::Success {
            continue;
        }

        for bid in &response.bids {
            let bid_price = match bid.price {
                Some(price) => price,
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

    if floor_prices.is_empty() {
        log::info!("Selected {} winning bids", winning_bids.len());
        return winning_bids;
    }

    let starting_count = winning_bids.len();
    winning_bids.retain(|slot_id, bid| match floor_prices.get(slot_id) {
        Some(floor) => match bid.price {
            Some(price) if price >= *floor => true,
            Some(_) => {
                log::info!("Dropping winning bid below floor price for slot '{slot_id}'");
                false
            }
            None => true,
        },
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

/// Manages auction execution across multiple providers.
pub struct AuctionOrchestrator {
    config: AuctionConfig,
    providers: HashMap<String, Arc<dyn AuctionProvider>>,
}

/// Server-side template auction that can advance without blocking page streaming.
pub struct PendingAuction {
    request: AuctionRequest,
    pending: Vec<PendingProviderRequest>,
    provider_responses: Vec<AuctionResponse>,
    floor_prices: HashMap<String, f64>,
    auction_started_at: Instant,
    auction_deadline: Instant,
}

struct PendingProviderRequest {
    provider: Arc<dyn AuctionProvider>,
    started_at: Instant,
    pending: PlatformPendingRequest,
}

/// Result of a non-blocking pending auction poll.
pub enum PendingAuctionPoll {
    /// At least one provider is still in flight.
    Pending,
    /// The auction completed or reached its original deadline.
    Complete(OrchestrationResult),
}

impl PendingAuction {
    /// Builds a completed pending auction for stream-polling tests.
    #[cfg(test)]
    pub(crate) fn from_completed_result_for_test(
        request: AuctionRequest,
        result: OrchestrationResult,
    ) -> Self {
        Self {
            request,
            pending: Vec::new(),
            provider_responses: result.provider_responses,
            floor_prices: HashMap::new(),
            auction_started_at: Instant::now(),
            auction_deadline: Instant::now(),
        }
    }

    /// Advance provider requests once without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError`] when the platform poll operation fails.
    pub async fn poll_once(
        &mut self,
        services: &RuntimeServices,
    ) -> Result<PendingAuctionPoll, Report<TrustedServerError>> {
        let mut still_pending = Vec::with_capacity(self.pending.len());

        for pending_provider in std::mem::take(&mut self.pending) {
            let PendingProviderRequest {
                provider,
                started_at,
                pending,
            } = pending_provider;
            let provider_name = provider.provider_name();
            let poll_result = services.http_client().poll(pending).await.change_context(
                TrustedServerError::Auction {
                    message: format!("HTTP poll failed for provider '{provider_name}'"),
                },
            )?;
            self.record_poll_result(provider, started_at, poll_result, &mut still_pending);
        }

        self.finish_poll_round(still_pending)
    }

    /// Advance provider requests once without blocking from a synchronous call
    /// site.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError`] when the platform poll operation fails.
    pub fn poll_once_now(
        &mut self,
        services: &RuntimeServices,
    ) -> Result<PendingAuctionPoll, Report<TrustedServerError>> {
        let mut still_pending = Vec::with_capacity(self.pending.len());

        for pending_provider in std::mem::take(&mut self.pending) {
            let PendingProviderRequest {
                provider,
                started_at,
                pending,
            } = pending_provider;
            let provider_name = provider.provider_name();
            let poll_result = services.http_client().poll_now(pending).change_context(
                TrustedServerError::Auction {
                    message: format!("HTTP poll failed for provider '{provider_name}'"),
                },
            )?;
            self.record_poll_result(provider, started_at, poll_result, &mut still_pending);
        }

        self.finish_poll_round(still_pending)
    }

    fn finish_poll_round(
        &mut self,
        still_pending: Vec<PendingProviderRequest>,
    ) -> Result<PendingAuctionPoll, Report<TrustedServerError>> {
        self.pending = still_pending;

        if self.pending.is_empty() || Instant::now() >= self.auction_deadline {
            Ok(PendingAuctionPoll::Complete(self.finish_due_to_deadline()))
        } else {
            Ok(PendingAuctionPoll::Pending)
        }
    }

    fn record_poll_result(
        &mut self,
        provider: Arc<dyn AuctionProvider>,
        started_at: Instant,
        poll_result: PlatformPollResult,
        still_pending: &mut Vec<PendingProviderRequest>,
    ) {
        match poll_result {
            PlatformPollResult::Pending(pending) => {
                still_pending.push(PendingProviderRequest {
                    provider,
                    started_at,
                    pending,
                });
            }
            PlatformPollResult::Ready(Ok(platform_response)) => {
                let response_time_ms = started_at.elapsed().as_millis() as u64;
                match platform_response_to_fastly(platform_response).and_then(|response| {
                    provider.parse_response_for_request(response, response_time_ms, &self.request)
                }) {
                    Ok(response) => self.provider_responses.push(response),
                    Err(error) => {
                        log::warn!(
                            "Provider '{}' failed during non-blocking auction poll: {error:?}",
                            provider.provider_name()
                        );
                        self.provider_responses.push(AuctionResponse::error(
                            provider.provider_name(),
                            response_time_ms,
                        ));
                    }
                }
            }
            PlatformPollResult::Ready(Err(error)) => {
                log::warn!(
                    "Provider '{}' poll completed with error: {error:?}",
                    provider.provider_name()
                );
                self.provider_responses.push(AuctionResponse::error(
                    provider.provider_name(),
                    started_at.elapsed().as_millis() as u64,
                ));
            }
        }
    }

    /// Finish the auction using responses collected so far.
    #[must_use]
    pub fn finish_due_to_deadline(&self) -> OrchestrationResult {
        OrchestrationResult {
            provider_responses: self.provider_responses.clone(),
            mediator_response: None,
            winning_bids: select_winning_bids_from_responses(
                &self.provider_responses,
                &self.floor_prices,
            ),
            total_time_ms: self.auction_started_at.elapsed().as_millis() as u64,
            metadata: HashMap::new(),
        }
    }

    /// Return whether no provider requests remain in flight.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.pending.is_empty()
    }
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

    /// Start a server-side template auction without waiting for provider responses.
    ///
    /// The returned [`PendingAuction`] must be advanced with
    /// [`PendingAuction::poll_once`] while the page response streams.
    ///
    /// # Errors
    ///
    /// Returns an error only when launching a provider request fails in a way
    /// that should abort the auction setup. Individual provider launch failures
    /// are logged and skipped.
    pub fn start_server_side_auction(
        &self,
        request: AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<PendingAuction, Report<TrustedServerError>> {
        if self.config.has_mediator() {
            return Err(Report::new(TrustedServerError::Auction {
                message: "server-side template auctions do not support mediation yet; disable the mediator for this Fastly Phase 1 path".to_string(),
            }));
        }

        let auction_started_at = Instant::now();
        let auction_deadline = auction_started_at
            .checked_add(Duration::from_millis(u64::from(context.timeout_ms)))
            .unwrap_or(auction_started_at);
        let mut pending = Vec::new();

        for provider_name in self.config.provider_names() {
            let Some(provider) = self.providers.get(provider_name).cloned() else {
                log::warn!("Provider '{}' not registered, skipping", provider_name);
                continue;
            };

            if !provider.is_enabled() {
                log::debug!(
                    "Provider '{}' is disabled, skipping",
                    provider.provider_name()
                );
                continue;
            }

            let remaining_ms = remaining_budget_ms(auction_started_at, context.timeout_ms);
            let effective_timeout = remaining_ms.min(provider.timeout_ms());
            if effective_timeout == 0 {
                log::warn!(
                    "Auction timeout ({}ms) exhausted before launching '{}' — skipping",
                    context.timeout_ms,
                    provider.provider_name()
                );
                continue;
            }

            let provider_context = AuctionContext {
                settings: context.settings,
                request: context.request,
                client_info: context.client_info,
                timeout_ms: effective_timeout,
                provider_responses: context.provider_responses,
                services: context.services,
            };

            match provider.request_bids(&request, &provider_context) {
                Ok(provider_pending) => {
                    let mut platform_pending = PlatformPendingRequest::new(provider_pending);
                    if let Some(backend_name) = provider.backend_name(effective_timeout) {
                        platform_pending = platform_pending.with_backend_name(backend_name);
                    }
                    pending.push(PendingProviderRequest {
                        provider,
                        started_at: Instant::now(),
                        pending: platform_pending,
                    });
                }
                Err(error) => {
                    log::warn!(
                        "Provider '{}' failed to launch request: {error:?}",
                        provider.provider_name()
                    );
                }
            }
        }

        Ok(PendingAuction {
            floor_prices: self.floor_prices_by_slot(&request),
            request,
            pending,
            provider_responses: Vec::new(),
            auction_started_at,
            auction_deadline,
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
                .parse_response_for_request(backend_response, response_time_ms, request)
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
                                match provider.parse_response_for_request(
                                    response,
                                    response_time_ms,
                                    request,
                                ) {
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
        select_winning_bids_from_responses(responses, floor_prices)
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
    use crate::auction::provider::AuctionProvider;
    use crate::auction::test_support::create_test_auction_context;
    use crate::auction::types::{
        AdFormat, AdSlot, AuctionRequest, AuctionResponse, Bid, MediaType, PublisherInfo, UserInfo,
    };
    use crate::error::TrustedServerError;

    // All-None ClientInfo used across tests that don't need real IP/TLS data.
    // Defined as a const so &EMPTY_CLIENT_INFO has 'static lifetime, avoiding
    // the temporary-lifetime issue that arises with &ClientInfo::default().
    const EMPTY_CLIENT_INFO: crate::platform::ClientInfo = crate::platform::ClientInfo {
        client_ip: None,
        tls_protocol: None,
        tls_cipher: None,
    };
    use crate::platform::test_support::{
        build_services_with_http_client, noop_services, StubHttpClient,
    };
    use crate::platform::{PlatformHttpClient, PlatformHttpRequest};
    use crate::test_support::tests::crate_test_settings_str;
    use edgezero_core::body::Body;
    use edgezero_core::http::request_builder;
    use error_stack::Report;
    use fastly::Request;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use super::{AuctionOrchestrator, PendingAuction, PendingAuctionPoll, PendingProviderRequest};

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

    struct TestAuctionProvider;

    impl AuctionProvider for TestAuctionProvider {
        fn provider_name(&self) -> &'static str {
            "test-provider"
        }

        fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &crate::auction::types::AuctionContext<'_>,
        ) -> Result<fastly::http::request::PendingRequest, Report<TrustedServerError>> {
            Err(Report::new(TrustedServerError::Auction {
                message: "test provider does not launch real Fastly requests".to_string(),
            }))
        }

        fn parse_response(
            &self,
            mut response: fastly::Response,
            response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            let body = response.take_body_str();
            let price = body.parse::<f64>().unwrap_or(1.5);
            Ok(AuctionResponse::success(
                self.provider_name(),
                vec![Bid {
                    slot_id: "header-banner".to_string(),
                    price: Some(price),
                    currency: "USD".to_string(),
                    creative: None,
                    adomain: None,
                    bidder: "test-bidder".to_string(),
                    width: 728,
                    height: 90,
                    nurl: None,
                    burl: None,
                    ad_id: Some("ad-123".to_string()),
                    metadata: HashMap::new(),
                }],
                response_time_ms,
            ))
        }

        fn timeout_ms(&self) -> u32 {
            100
        }

        fn backend_name(&self, _timeout_ms: u32) -> Option<String> {
            Some("backend-poll".to_string())
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

    #[test]
    fn pending_auction_poll_once_completes_ready_provider_response() {
        let http_client = Arc::new(StubHttpClient::new());
        http_client.push_response(200, b"2.25".to_vec());
        let services = build_services_with_http_client(http_client.clone());
        let pending = futures::executor::block_on(
            http_client.send_async(PlatformHttpRequest::new(
                request_builder()
                    .method("POST")
                    .uri("https://bidder.example/openrtb2/auction")
                    .body(Body::empty())
                    .expect("should build platform request"),
                "backend-poll",
            )),
        )
        .expect("should create pending request");
        let provider = Arc::new(TestAuctionProvider);
        let request = create_test_auction_request();
        let floor_prices = HashMap::from([("header-banner".to_string(), 1.50)]);
        let mut pending_auction = PendingAuction {
            request,
            pending: vec![PendingProviderRequest {
                provider,
                started_at: std::time::Instant::now(),
                pending,
            }],
            provider_responses: Vec::new(),
            floor_prices,
            auction_started_at: std::time::Instant::now(),
            auction_deadline: std::time::Instant::now() + std::time::Duration::from_millis(50),
        };

        let result =
            futures::executor::block_on(pending_auction.poll_once(&services)).expect("should poll");

        let PendingAuctionPoll::Complete(result) = result else {
            panic!("should complete after ready provider response");
        };
        let winner = result
            .winning_bids
            .get("header-banner")
            .expect("should select winning bid");
        assert_eq!(winner.price, Some(2.25), "should parse provider response");
        assert!(
            pending_auction.is_complete(),
            "should have no pending providers after completion"
        );
    }

    #[test]
    fn server_side_auction_rejects_mediator_config_instead_of_bypassing_it() {
        let config = AuctionConfig {
            enabled: true,
            providers: Vec::new(),
            mediator: Some("gam".to_string()),
            timeout_ms: 2000,
            creative_store: "creative_store".to_string(),
            allowed_context_keys: HashSet::new(),
        };
        let orchestrator = AuctionOrchestrator::new(config);
        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = Request::get("https://test.com/test");
        let context = create_test_auction_context(&settings, &req, &EMPTY_CLIENT_INFO, 2000);

        let error = match orchestrator.start_server_side_auction(request, &context) {
            Ok(_) => {
                panic!("mediated server-side template auctions should be explicit unsupported")
            }
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("mediation"),
            "should not silently run the non-mediated pending-auction path"
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
}
