//! Auction orchestrator for managing multi-provider auctions.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::Request;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

use crate::error::TrustedServerError;
use crate::platform::{PlatformPendingRequest, RuntimeServices};

use super::config::AuctionConfig;
use super::provider::AuctionProvider;
use super::telemetry::AbandonedProviderCall;
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
    backend_to_provider: HashMap<String, (String, Instant, Arc<dyn AuctionProvider>, u32)>,
    launch_responses: Vec<AuctionResponse>,
    auction_start: Instant,
    timeout_ms: u32,
    floor_prices: HashMap<String, f64>,
    provider_request_context: Box<Request<EdgeBody>>,
    /// Carried so the mediator call in collect can pass it as the auction request.
    request: AuctionRequest,
}

/// Outcome of attempting to dispatch split-phase auction provider requests.
pub enum DispatchAuctionOutcome {
    /// No provider request was started and no provider failure was observed.
    NotStarted,
    /// No provider request could be launched, but launch failures were observed.
    DispatchFailed {
        /// Original auction request.
        request: AuctionRequest,
        /// Provider launch-failure responses.
        provider_responses: Vec<AuctionResponse>,
        /// Elapsed dispatch time.
        elapsed_ms: u64,
    },
    /// One or more provider requests are in flight.
    Dispatched(DispatchedAuction),
}

impl DispatchedAuction {
    /// Consume the dispatch token without collecting provider responses.
    #[must_use]
    pub fn abandon(
        self,
    ) -> (
        AuctionRequest,
        Vec<AuctionResponse>,
        Vec<AbandonedProviderCall>,
        u64,
    ) {
        let elapsed_ms = self.auction_start.elapsed().as_millis() as u64;
        let abandoned = self
            .backend_to_provider
            .into_values()
            .map(|(provider_name, start_time, _, _)| {
                AbandonedProviderCall::bidder(
                    provider_name,
                    Some(u32::try_from(start_time.elapsed().as_millis()).unwrap_or(u32::MAX)),
                )
            })
            .collect();
        (self.request, self.launch_responses, abandoned, elapsed_ms)
    }
}

#[cfg(test)]
impl DispatchedAuction {
    pub(crate) fn empty_for_test(request: AuctionRequest, timeout_ms: u32) -> Self {
        Self {
            pending_requests: Vec::new(),
            backend_to_provider: HashMap::new(),
            launch_responses: Vec::new(),
            auction_start: Instant::now(),
            timeout_ms,
            floor_prices: HashMap::new(),
            provider_request_context: Box::new(Request::new(EdgeBody::empty())),
            request,
        }
    }
}

const PROVIDER_ERROR_MESSAGE_CHARS: usize = 500;

const ERROR_TYPE_PARSE_RESPONSE: &str = "parse_response";
const ERROR_TYPE_LAUNCH_FAILED: &str = "launch_failed";
const ERROR_TYPE_TRANSPORT: &str = "transport";
const ERROR_TYPE_TIMEOUT: &str = "timeout";

// SECURITY: the returned string is included verbatim (truncated to
// PROVIDER_ERROR_MESSAGE_CHARS) in the public /auction response via
// ProviderSummary.metadata["message"]. Providers MUST NOT interpolate
// upstream-controlled content (response bodies, parse errors, headers) into
// their TrustedServerError::*.message fields. Use static text and log details
// server-side with `log::warn!` instead.
fn provider_error_message(error: &Report<TrustedServerError>) -> String {
    error
        .current_context()
        .to_string()
        .chars()
        .take(PROVIDER_ERROR_MESSAGE_CHARS)
        .collect()
}

fn provider_error_response(
    provider_name: &str,
    response_time_ms: u64,
    error_type: &str,
    error: &Report<TrustedServerError>,
) -> AuctionResponse {
    AuctionResponse::error(provider_name, response_time_ms)
        .with_metadata("error_type", serde_json::json!(error_type))
        .with_metadata("message", serde_json::json!(provider_error_message(error)))
}

fn provider_launch_failed_response(provider_name: &str, response_time_ms: u64) -> AuctionResponse {
    AuctionResponse::error(provider_name, response_time_ms)
        .with_metadata("error_type", serde_json::json!(ERROR_TYPE_LAUNCH_FAILED))
        .with_metadata("message", serde_json::json!("Provider launch failed"))
}

// Transport failures carry a static message: the underlying select() error is a
// `Report<PlatformError>` that may reference upstream-controlled content, so it
// is logged server-side rather than surfaced in the public /auction response.
fn provider_transport_failed_response(
    provider_name: &str,
    response_time_ms: u64,
) -> AuctionResponse {
    AuctionResponse::error(provider_name, response_time_ms)
        .with_metadata("error_type", serde_json::json!(ERROR_TYPE_TRANSPORT))
        .with_metadata("message", serde_json::json!("Provider request failed"))
}

fn provider_timeout_response(provider_name: &str, response_time_ms: u64) -> AuctionResponse {
    AuctionResponse::error(provider_name, response_time_ms)
        .with_metadata("error_type", serde_json::json!(ERROR_TYPE_TIMEOUT))
        .with_metadata("message", serde_json::json!("Provider request timed out"))
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

fn snapshot_context_request(request: &Request<EdgeBody>) -> Request<EdgeBody> {
    let mut snapshot = Request::new(EdgeBody::empty());
    *snapshot.method_mut() = request.method().clone();
    *snapshot.uri_mut() = request.uri().clone();
    *snapshot.version_mut() = request.version();
    *snapshot.headers_mut() = request.headers().clone();
    snapshot
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

    /// Validate that every configured provider name has an enabled provider integration.
    pub(crate) fn validate_configured_provider_names(
        &self,
    ) -> Result<(), Report<TrustedServerError>> {
        if !self.config.enabled {
            return Ok(());
        }

        for provider_name in self
            .config
            .providers
            .iter()
            .chain(self.config.mediator.iter())
        {
            if !self.providers.contains_key(provider_name) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "Auction provider `{provider_name}` is listed in [auction] but no enabled integration provides it"
                    ),
                }));
            }
        }

        Ok(())
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
        let mediation_start = Instant::now();
        let provider_responses = self.run_providers_parallel(request, context).await?;

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
                log::warn!("Auction timeout exhausted during bidding phase; skipping mediator");
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
                // Bound by both the remaining auction budget and the mediator's
                // own configured timeout, matching the dispatched collect path.
                timeout_ms: remaining_ms.min(mediator.timeout_ms()),
                provider_responses: Some(&provider_responses),
                services: context.services,
            };

            let start_time = Instant::now();
            let pending = mediator
                .request_bids(request, &mediator_context)
                .await
                .change_context(TrustedServerError::Auction {
                    message: format!("Mediator {} failed to launch", mediator.provider_name()),
                })?;

            let platform_resp = mediator_context
                .services
                .http_client()
                .wait(pending)
                .await
                .change_context(TrustedServerError::Auction {
                    message: format!("Mediator {} request failed", mediator.provider_name()),
                })?;

            let response_time_ms = start_time.elapsed().as_millis() as u64;
            // Use the context-aware parse so mediators (e.g. adserver_mock) can
            // restore nurl/burl/ad_id and PBS cache fields from the collected SSP
            // responses. The dispatched collect path already does this; the
            // synchronous mediation path used by POST /auction and
            // /__ts/page-bids must match or mediated cache bids lose the metadata
            // needed for creative rendering and win/billing beacons.
            let mediator_resp = mediator
                .parse_response_with_context(
                    platform_resp,
                    response_time_ms,
                    request,
                    &mediator_context,
                )
                .await
                .change_context(TrustedServerError::Auction {
                    message: format!("Mediator {} parse failed", mediator.provider_name()),
                })?;

            // Extract only mediator bids with comparable numeric prices.
            let winning = mediator_resp
                .bids
                .iter()
                .filter_map(|bid| {
                    if bid.price.is_none() {
                        log::warn!(
                            "Mediator '{}' returned bid for slot '{}' without a price - skipping",
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
    /// Uses `PlatformHttpClient::select()` to process responses as they
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

        // Reject multi-provider fan-out before any request launches when the
        // platform executes `send_async` eagerly (e.g. Cloudflare Workers):
        // sequential execution would accrue the sum of provider latencies and
        // blow the auction budget before a later `select` could reject it.
        if provider_names.len() > 1 && !context.services.http_client().supports_concurrent_fanout()
        {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "{} auction providers configured, but this platform's HTTP \
                     client executes requests sequentially — configure a single \
                     provider, or use an adapter with concurrent fan-out support",
                    provider_names.len(),
                ),
            }));
        }

        log::info!(
            "Running {} providers in parallel using select",
            provider_names.len()
        );

        // Track auction start time for deadline enforcement
        let auction_start = Instant::now();

        // Phase 1: Launch all requests concurrently and build mapping
        // Maps backend_name to provider state retained for response parsing.
        let mut backend_to_provider: HashMap<String, (&str, Instant, &dyn AuctionProvider, u32)> =
            HashMap::new();
        let mut pending_requests: Vec<crate::platform::PlatformPendingRequest> = Vec::new();
        let mut responses = Vec::new();

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
            // deadline so that backend transport timeouts do not extend past
            // the overall budget. Also respect the provider's own configured
            // timeout when it is tighter than the remaining budget.
            let remaining_ms = remaining_budget_ms(auction_start, context.timeout_ms);
            let effective_timeout = remaining_ms.min(provider.timeout_ms());

            if effective_timeout == 0 {
                log::warn!("Auction timeout exhausted before launching provider request; skipping");
                continue;
            }

            // Get the backend name for this provider to map responses back.
            // Must be computed after effective_timeout since the timeout is
            // part of the backend name.
            let backend_name = match provider.backend_name(context.services, effective_timeout) {
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
            match provider.request_bids(request, &provider_context).await {
                Ok(pending) => {
                    let request_backend_name = pending
                        .backend_name()
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            log::warn!(
                                "Provider '{}' pending request returned no backend name; \
                             using predicted name '{}'",
                                provider.provider_name(),
                                backend_name,
                            );
                            backend_name.clone()
                        });
                    backend_to_provider.insert(
                        request_backend_name.clone(),
                        (
                            provider.provider_name(),
                            start_time,
                            provider.as_ref(),
                            effective_timeout,
                        ),
                    );
                    pending_requests.push(pending);
                    log::debug!(
                        "Request to '{}' launched successfully",
                        provider.provider_name()
                    );
                }
                Err(e) => {
                    let response_time_ms = start_time.elapsed().as_millis() as u64;
                    log::warn!(
                        "Provider '{}' failed to launch request: {:?}",
                        provider.provider_name(),
                        e
                    );
                    responses.push(provider_launch_failed_response(
                        provider.provider_name(),
                        response_time_ms,
                    ));
                }
            }
        }

        if pending_requests.is_empty() {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "All {} configured provider(s) skipped or failed to launch",
                    provider_names.len()
                ),
            }));
        }

        let deadline = Duration::from_millis(u64::from(context.timeout_ms));
        log::info!(
            "Launched {} concurrent provider request(s); waiting for responses",
            pending_requests.len()
        );

        // Phase 2: Wait for responses using select() to process as they become ready.
        // Enforce the auction deadline: after each select() returns, check
        // elapsed time and drop remaining requests if the timeout is exceeded.
        //
        // NOTE: `select()` blocks until at least one backend responds and, on
        // some adapters, buffers the selected response body before returning.
        // Hard deadline enforcement therefore depends on every backend's
        // first-byte and between-bytes timeouts being set to at most the
        // remaining auction budget, which Phase 1 above guarantees.
        let mut remaining = pending_requests;

        while !remaining.is_empty() {
            let platform_result = match context.services.http_client().select(remaining).await {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("select() failed: {:?}", e);
                    break;
                }
            };
            let crate::platform::PlatformSelectResult {
                ready,
                remaining: new_remaining,
                failed_backend_name,
            } = platform_result;
            remaining = new_remaining;

            match ready {
                Ok(response) => {
                    // Identify the provider from the backend name
                    let backend_name = response
                        .backend_name
                        .as_deref()
                        .unwrap_or_default()
                        .to_string();

                    if let Some((provider_name, start_time, provider, effective_timeout)) =
                        backend_to_provider.remove(&backend_name)
                    {
                        let response_time_ms = start_time.elapsed().as_millis() as u64;
                        let provider_context = AuctionContext {
                            settings: context.settings,
                            request: context.request,
                            timeout_ms: effective_timeout,
                            provider_responses: context.provider_responses,
                            services: context.services,
                        };

                        // Use the context-aware parse so a provider overriding
                        // `parse_response_with_context` behaves identically on the
                        // parallel (`/auction`, page-bids) and collect (publisher)
                        // paths. The default impl delegates to `parse_response`.
                        match provider
                            .parse_response_with_context(
                                response,
                                response_time_ms,
                                request,
                                &provider_context,
                            )
                            .await
                        {
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
                                // lgtm[rust/cleartext-logging]
                                // This warning reports provider parse failures only; no secret values are logged.
                                log::warn!(
                                    "Provider '{}' failed to parse response: {:?}",
                                    provider_name,
                                    e
                                );
                                responses.push(provider_error_response(
                                    provider_name,
                                    response_time_ms,
                                    ERROR_TYPE_PARSE_RESPONSE,
                                    &e,
                                ));
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
                    if let Some(ref backend_name) = failed_backend_name {
                        if let Some((provider_name, start_time, _, _)) =
                            backend_to_provider.remove(backend_name)
                        {
                            let response_time_ms = start_time.elapsed().as_millis() as u64;
                            log::warn!("Provider '{}' request failed: {:?}", provider_name, e);
                            responses.push(provider_transport_failed_response(
                                provider_name,
                                response_time_ms,
                            ));
                        } else {
                            log::warn!(
                                "A provider request failed (backend '{}' not tracked): {:?}",
                                backend_name,
                                e
                            );
                        }
                    } else {
                        log::warn!(
                            "A provider request failed (backend not identified): {:?}",
                            e
                        );
                    }
                }
            }

            // Check auction deadline after processing each response.
            // Remaining PendingRequests are dropped, which abandons the
            // in-flight HTTP calls on the Fastly host.
            if auction_start.elapsed() >= deadline && !remaining.is_empty() {
                log::warn!(
                    "Auction timeout reached; dropping {} remaining request(s)",
                    remaining.len()
                );
                break;
            }
        }

        for (provider_name, start_time, _, _) in backend_to_provider.into_values() {
            let response_time_ms = start_time.elapsed().as_millis() as u64;
            log::warn!("Provider '{provider_name}' timed out before auction collection completed");
            responses.push(provider_timeout_response(provider_name, response_time_ms));
        }

        Ok(responses)
    }

    /// Select the best decoded-price bid for each slot from all responses.
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
                let bid_price = match bid.price {
                    Some(p) => p,
                    None => {
                        log::debug!(
                            "Skipping bid for slot '{}' from '{}' without a comparable price",
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
        winning_bids.retain(
            |slot_id, bid| match (floor_prices.get(slot_id), bid.price) {
                (Some(floor), Some(price)) if price >= *floor => true,
                (Some(_), Some(_)) => {
                    log::info!(
                        "Dropping winning bid below floor price for slot '{}'",
                        slot_id
                    );
                    false
                }
                (_, None) => {
                    // Every downstream response requires a comparable numeric price,
                    // so bids without one are always dropped before delivery.
                    log::debug!(
                        "Dropping bid for slot '{}' without a comparable price",
                        slot_id
                    );
                    false
                }
                (None, Some(_)) => true,
            },
        );

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
    /// Returns [`DispatchAuctionOutcome::NotStarted`] when no providers are configured or
    /// all providers are disabled / over budget. Returns
    /// [`DispatchAuctionOutcome::DispatchFailed`] when provider launch attempts
    /// happened but none could be started.
    #[must_use]
    pub async fn dispatch_auction(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> DispatchAuctionOutcome {
        let provider_names = self.config.provider_names();
        if provider_names.is_empty() {
            return DispatchAuctionOutcome::NotStarted;
        }

        // Mirror run_providers_parallel: reject multi-provider fan-out before
        // any request launches when the platform executes `send_async` eagerly
        // (e.g. Cloudflare Workers, Spin). Sequential execution would accrue
        // the sum of provider latencies before the origin fetch and then fail
        // collection with empty bids.
        if provider_names.len() > 1 && !context.services.http_client().supports_concurrent_fanout()
        {
            log::warn!(
                "{} auction providers configured, but this platform's HTTP client \
                 executes requests sequentially — skipping initial-page auction \
                 dispatch; configure a single provider, or use an adapter with \
                 concurrent fan-out support",
                provider_names.len(),
            );
            return DispatchAuctionOutcome::NotStarted;
        }

        let auction_start = Instant::now();
        let mut backend_to_provider: HashMap<
            String,
            (String, Instant, Arc<dyn AuctionProvider>, u32),
        > = HashMap::new();
        let mut pending_requests: Vec<PlatformPendingRequest> = Vec::new();
        let mut launch_responses: Vec<AuctionResponse> = Vec::new();

        for provider_name in provider_names {
            let provider = match self.providers.get(provider_name) {
                Some(p) => p,
                None => {
                    // lgtm[rust/cleartext-logging]
                    // The provider name is a static config identifier (e.g. "prebid"), not a secret.
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

            let backend_name = match provider.backend_name(context.services, effective_timeout) {
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
                timeout_ms: effective_timeout,
                provider_responses: context.provider_responses,
                services: context.services,
            };

            let start_time = Instant::now();
            match provider.request_bids(request, &provider_context).await {
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
                            effective_timeout,
                        ),
                    );
                    pending_requests.push(pending.with_backend_name(backend_name));
                }
                Err(e) => {
                    let response_time_ms = start_time.elapsed().as_millis() as u64;
                    log::warn!(
                        "Provider '{}' failed to dispatch request: {:?}",
                        provider.provider_name(),
                        e
                    );
                    launch_responses.push(provider_launch_failed_response(
                        provider.provider_name(),
                        response_time_ms,
                    ));
                }
            }
        }

        if pending_requests.is_empty() {
            return if launch_responses.is_empty() {
                DispatchAuctionOutcome::NotStarted
            } else {
                DispatchAuctionOutcome::DispatchFailed {
                    request: request.clone(),
                    provider_responses: launch_responses,
                    elapsed_ms: auction_start.elapsed().as_millis() as u64,
                }
            };
        }

        log::info!(
            "Dispatched {} SSP requests (timeout: {}ms); Fastly host will race them against origin",
            pending_requests.len(),
            context.timeout_ms
        );

        DispatchAuctionOutcome::Dispatched(DispatchedAuction {
            pending_requests,
            backend_to_provider,
            launch_responses,
            auction_start,
            timeout_ms: context.timeout_ms,
            floor_prices: self.floor_prices_by_slot(request),
            provider_request_context: Box::new(snapshot_context_request(context.request)),
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
            launch_responses,
            auction_start,
            timeout_ms,
            floor_prices,
            provider_request_context,
            request,
        } = dispatched;

        log::info!(
            "Collecting {} in-flight SSP responses (timeout: {}ms remaining: {}ms)",
            pending_requests.len(),
            timeout_ms,
            remaining_budget_ms(auction_start, timeout_ms),
        );

        let mut responses: Vec<AuctionResponse> = launch_responses;
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
            // Destructure so transport failures can be attributed to a provider
            // via `failed_backend_name`, mirroring run_providers_parallel.
            let crate::platform::PlatformSelectResult {
                ready,
                remaining: new_remaining,
                failed_backend_name,
            } = select_result;
            remaining = new_remaining;

            match ready {
                Ok(platform_response) => {
                    let backend_name = platform_response.backend_name.clone().unwrap_or_default();
                    if let Some((provider_name, start_time, provider, effective_timeout)) =
                        backend_to_provider.remove(&backend_name)
                    {
                        let response_time_ms = start_time.elapsed().as_millis() as u64;
                        let provider_context = AuctionContext {
                            settings: context.settings,
                            request: &provider_request_context,
                            timeout_ms: effective_timeout,
                            provider_responses: context.provider_responses,
                            services: context.services,
                        };
                        // Mirror run_providers_parallel: use the context-aware
                        // parse so providers behave identically on both paths.
                        match provider
                            .parse_response_with_context(
                                platform_response,
                                response_time_ms,
                                &request,
                                &provider_context,
                            )
                            .await
                        {
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
                                log::warn!("Provider '{}' parse failed: {:?}", provider_name, e);
                                // Mirror the parallel path so a parse failure is
                                // attributed (error_type + message) in provider_details.
                                responses.push(provider_error_response(
                                    &provider_name,
                                    response_time_ms,
                                    ERROR_TYPE_PARSE_RESPONSE,
                                    &e,
                                ));
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
                    // Mirror the parallel path: attribute the transport failure to
                    // the provider behind `failed_backend_name` so it appears in
                    // provider_details instead of vanishing.
                    if let Some(ref backend_name) = failed_backend_name {
                        if let Some((provider_name, start_time, _, _)) =
                            backend_to_provider.remove(backend_name)
                        {
                            let response_time_ms = start_time.elapsed().as_millis() as u64;
                            log::warn!("Provider '{}' request failed: {:?}", provider_name, e);
                            responses.push(provider_transport_failed_response(
                                &provider_name,
                                response_time_ms,
                            ));
                        } else {
                            log::warn!(
                                "A provider request failed (backend '{}' not tracked): {:?}",
                                backend_name,
                                e
                            );
                        }
                    } else {
                        log::warn!(
                            "A provider request failed during collection (backend not identified): {:?}",
                            e
                        );
                    }
                }
            }

            // Drain every dispatched request. Each backend was capped with
            // first-byte and between-bytes timeouts at dispatch time, so by the
            // collect phase the remaining handles may already be ready even if
            // wall-clock time elapsed while the origin was slow. Dropping them
            // here would discard SSP responses that already arrived. The
            // mediator launch below still observes A_deadline via
            // `remaining_budget_ms`.
        }

        for (provider_name, start_time, _, _) in backend_to_provider.values() {
            let response_time_ms = start_time.elapsed().as_millis() as u64;
            log::warn!(
                "Provider '{provider_name}' timed out before dispatched auction collection completed"
            );
            responses.push(provider_timeout_response(provider_name, response_time_ms));
        }
        backend_to_provider.clear();

        let (mediator_response, winning_bids) = if let Some(mediator_name) = &self.config.mediator {
            match self.providers.get(mediator_name.as_str()) {
                Some(mediator) => {
                    // Cap the mediator at whichever is tighter: its own configured
                    // timeout or the remaining auction budget (A_deadline).  The old
                    // comment here claimed origin drain could exhaust the budget before
                    // collection, but SSP backends are given first-byte and between-bytes
                    // timeouts equal to effective_timeout (capped at their provider
                    // timeout) at dispatch time, so they cannot run past A_deadline
                    // independently. Giving the mediator an uncapped timeout lets it run
                    // past A_deadline, violating the bounded hold invariant.
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
                    // The mediator runs on the collect path. See the doc-comment on
                    // `AuctionContext::request`: the real client request was already
                    // consumed by `send_async` during dispatch, so we substitute a
                    // canonical placeholder URL. Any future mediator that needs real
                    // client headers must snapshot them at dispatch time onto
                    // `DispatchedAuction` rather than reading `context.request` here.
                    let placeholder = http::Request::builder()
                        .uri(crate::auction::types::MEDIATOR_PLACEHOLDER_URL)
                        .body(edgezero_core::body::Body::empty())
                        .unwrap_or_else(|_| http::Request::new(edgezero_core::body::Body::empty()));
                    let mediator_context = AuctionContext {
                        settings: context.settings,
                        request: &placeholder,
                        timeout_ms: mediator_timeout,
                        provider_responses: Some(&responses),
                        services: context.services,
                    };
                    match mediator.request_bids(&request, &mediator_context).await {
                        Ok(pending) => {
                            let platform_resp = services.http_client().wait(pending).await;
                            match platform_resp.change_context(TrustedServerError::Auction {
                                message: format!(
                                    "Mediator {} request failed",
                                    mediator.provider_name()
                                ),
                            }) {
                                Ok(platform_resp) => {
                                    let response_time_ms =
                                        mediator_start.elapsed().as_millis() as u64;
                                    // Mirror run_parallel_mediation: use the
                                    // context-aware parse so the mediator sees
                                    // the collected provider responses.
                                    match mediator
                                        .parse_response_with_context(
                                            platform_resp,
                                            response_time_ms,
                                            &request,
                                            &mediator_context,
                                        )
                                        .await
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
                                            let winning =
                                                self.apply_floor_prices(winning, &floor_prices);
                                            (Some(mediator_resp), winning)
                                        }
                                        Err(e) => {
                                            log::warn!(
                                                "Mediator '{}' parse failed: {:?}",
                                                mediator.provider_name(),
                                                e
                                            );
                                            let winning =
                                                self.select_winning_bids(&responses, &floor_prices);
                                            (None, winning)
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
                    // lgtm[rust/cleartext-logging]
                    // The mediator name is a static config identifier, not a secret.
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
    use std::time::Duration;
    use web_time::Instant;

    use crate::auction::config::AuctionConfig;
    use crate::auction::orchestrator::DispatchAuctionOutcome;
    use crate::auction::provider::AuctionProvider;
    use crate::auction::test_support::create_test_auction_context;
    use crate::auction::types::{
        AdFormat, AdSlot, ApsRendererV1, ApsTagType, AuctionContext, AuctionRequest,
        AuctionResponse, Bid, BidRenderer, BidStatus, MediaType, PublisherInfo, UserInfo,
    };
    use crate::error::TrustedServerError;
    use crate::platform::test_support::{StubHttpClient, build_services_with_http_client};
    use crate::platform::{
        PlatformHttpRequest, PlatformPendingRequest, PlatformResponse, RuntimeServices,
    };
    use crate::test_support::tests::crate_test_settings_str;
    use error_stack::{Report, ResultExt};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use super::AuctionOrchestrator;

    // ---------------------------------------------------------------------------
    // Minimal test double for AuctionProvider
    // ---------------------------------------------------------------------------

    struct StubAuctionProvider {
        name: &'static str,
        backend: &'static str,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for StubAuctionProvider {
        fn provider_name(&self) -> &'static str {
            self.name
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
            let req = PlatformHttpRequest::new(
                http::Request::builder()
                    .method("POST")
                    .uri("https://example.com/bid")
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build stub bid request"),
                self.backend,
            );
            context
                .services
                .http_client()
                .send_async(req)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "stub launch failed".to_string(),
                })
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            Ok(AuctionResponse::success(
                self.name,
                vec![],
                response_time_ms,
            ))
        }

        async fn parse_response_with_context(
            &self,
            response: PlatformResponse,
            response_time_ms: u64,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            let referer = context
                .request
                .headers()
                .get(http::header::REFERER)
                .and_then(|value| value.to_str().ok());
            Ok(self
                .parse_response(response, response_time_ms)
                .await?
                .with_metadata("context_referer", serde_json::json!(referer))
                .with_metadata("context_timeout_ms", serde_json::json!(context.timeout_ms)))
        }

        fn timeout_ms(&self) -> u32 {
            125
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some(self.backend.to_string())
        }
    }

    /// Mediator whose context-aware parse restores `nurl`/`ad_id` (mirroring
    /// `adserver_mock`), while its context-free parse does not. Lets a test prove
    /// the synchronous mediation path calls `parse_response_with_context`.
    struct CacheRestoringMediator;

    fn auction_bid(bidder: &str, price: f64) -> Bid {
        let renderer = (bidder == "aps").then(|| {
            BidRenderer::Aps(ApsRendererV1 {
                version: 1,
                account_id: "example-account".to_string(),
                bid_id: "aps-selected-bid".to_string(),
                creative_id: None,
                tag_type: ApsTagType::Iframe,
                creative_url: "https://creative.example/render".to_string(),
                aax_response: "fictional-base64".to_string(),
                width: 300,
                height: 250,
            })
        });
        Bid {
            slot_id: "slot-1".to_string(),
            price: Some(price),
            currency: "USD".to_string(),
            creative: renderer
                .is_none()
                .then(|| "<div>ordinary</div>".to_string()),
            adomain: None,
            bidder: bidder.to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            bid_id: (bidder == "aps").then(|| "aps-selected-bid".to_string()),
            ad_id: None,
            creative_id: None,
            renderer,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    fn mediated_bid(nurl: Option<String>) -> Bid {
        Bid {
            slot_id: "header-banner".to_string(),
            price: Some(2.5),
            currency: "USD".to_string(),
            creative: Some("<div>ad</div>".to_string()),
            adomain: None,
            bidder: "mediator".to_string(),
            width: 728,
            height: 90,
            nurl: nurl.clone(),
            burl: nurl,
            bid_id: None,
            ad_id: Some("creative-123".to_string()),
            creative_id: None,
            renderer: None,
            cache_id: Some("cache-abc".to_string()),
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for CacheRestoringMediator {
        fn provider_name(&self) -> &'static str {
            "mediator"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
            let req = PlatformHttpRequest::new(
                http::Request::builder()
                    .method("POST")
                    .uri("https://example.com/mediate")
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build mediator request"),
                "mediator-backend",
            );
            context
                .services
                .http_client()
                .send_async(req)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "mediator launch failed".to_string(),
                })
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            // Context-free path: cannot restore SSP-only render/accounting fields.
            Ok(AuctionResponse::success(
                "mediator",
                vec![mediated_bid(None)],
                response_time_ms,
            ))
        }

        async fn parse_response_with_context(
            &self,
            _response: PlatformResponse,
            response_time_ms: u64,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            // Context-aware path: restores nurl/ad_id from the collected SSP bids.
            Ok(AuctionResponse::success(
                "mediator",
                vec![mediated_bid(Some("https://nurl.example/win".to_string()))],
                response_time_ms,
            ))
        }

        fn timeout_ms(&self) -> u32 {
            2000
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("mediator-backend".to_string())
        }
    }

    #[tokio::test]
    async fn mediated_bid_preserves_restored_fields_through_run_auction() {
        // run_parallel_mediation must parse the mediator response via
        // parse_response_with_context so cache/nurl fields restored from SSP
        // responses survive the synchronous mediation path (POST /auction,
        // /__ts/page-bids), matching the dispatched collect path.
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"{}".to_vec()); // bidder send_async
        stub.push_response(200, b"{}".to_vec()); // mediator send_async
        let services = build_services_with_http_client(stub);
        // SAFETY: `Box::leak` creates a `'static` reference for test use only.
        let services: &'static RuntimeServices = Box::leak(Box::new(services));

        let config = AuctionConfig {
            enabled: true,
            providers: vec!["bidder".to_string()],
            mediator: Some("mediator".to_string()),
            timeout_ms: 2000,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(StubAuctionProvider {
            name: "bidder",
            backend: "bidder-backend",
        }));
        orchestrator.register_provider(Arc::new(CacheRestoringMediator));

        let request = create_test_auction_request();
        let settings = create_test_settings();
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://example.com/test")
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        let context = AuctionContext {
            settings: &settings,
            request: &req,
            timeout_ms: 2000,
            provider_responses: None,
            services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("mediated auction should complete");

        let bid = result
            .winning_bids
            .get("header-banner")
            .expect("mediator should produce a winning bid for the slot");
        assert_eq!(
            bid.nurl.as_deref(),
            Some("https://nurl.example/win"),
            "synchronous mediation must restore nurl via parse_response_with_context"
        );
        assert_eq!(
            bid.ad_id.as_deref(),
            Some("creative-123"),
            "mediated bid must keep its restored ad_id"
        );
    }

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
                id: Some("user-123".to_string()),
                consent: None,
                eids: None,
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

    struct LaunchFailingProvider;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for LaunchFailingProvider {
        fn provider_name(&self) -> &'static str {
            "launch-failing"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
            Err(Report::new(TrustedServerError::Auction {
                message: "launch failed in test provider".to_string(),
            }))
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            Err(Report::new(TrustedServerError::Auction {
                message: "launch-failing provider should not parse responses".to_string(),
            }))
        }

        fn timeout_ms(&self) -> u32 {
            2000
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("launch-failing-backend".to_string())
        }
    }

    #[test]
    fn provider_error_response_includes_diagnostic_metadata() {
        let error = Report::new(TrustedServerError::Auction {
            message: "parse failed".to_string(),
        })
        .attach("internal/source.rs:12:34");

        let response =
            super::provider_error_response("prebid", 37, super::ERROR_TYPE_PARSE_RESPONSE, &error);

        assert_eq!(
            response.status,
            BidStatus::Error,
            "should mark diagnostic provider responses as errors"
        );
        assert_eq!(
            response.metadata["error_type"],
            serde_json::json!("parse_response"),
            "should include the provider error classification"
        );

        let message = response.metadata["message"]
            .as_str()
            .expect("should include provider error message");
        assert!(
            message.contains("parse failed"),
            "should include user-safe diagnostic detail"
        );
        assert!(
            !message.contains("internal/source.rs"),
            "should not include attached internal details"
        );
    }

    #[test]
    fn launch_failed_response_has_safe_static_message() {
        let response = super::provider_launch_failed_response("prebid", 58);

        assert_eq!(
            response.status,
            BidStatus::Error,
            "should mark launch failures as errors"
        );
        assert_eq!(
            response.metadata["error_type"],
            serde_json::json!("launch_failed"),
            "should include launch_failed classification"
        );
        assert_eq!(
            response.metadata["message"],
            serde_json::json!("Provider launch failed"),
            "should use a safe, stable public launch failure message"
        );
    }

    #[test]
    fn transport_failed_response_has_safe_static_message() {
        let response = super::provider_transport_failed_response("prebid", 64);

        assert_eq!(
            response.status,
            BidStatus::Error,
            "should mark transport failures as errors"
        );
        assert_eq!(
            response.metadata["error_type"],
            serde_json::json!("transport"),
            "should classify transport failures consistently with other failure modes"
        );
        assert_eq!(
            response.metadata["message"],
            serde_json::json!("Provider request failed"),
            "should use a safe, stable public transport failure message"
        );
    }

    #[test]
    fn provider_error_message_truncates_user_safe_context() {
        let long_message = "x".repeat(super::PROVIDER_ERROR_MESSAGE_CHARS + 100);
        let error = Report::new(TrustedServerError::Auction {
            message: long_message,
        });

        let message = super::provider_error_message(&error);

        assert_eq!(
            message.chars().count(),
            super::PROVIDER_ERROR_MESSAGE_CHARS,
            "should cap provider error messages"
        );
        assert!(
            message.starts_with("Auction error: "),
            "should preserve the current context display text"
        );
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
                bid_id: None,
                ad_id: None,
                creative_id: None,
                renderer: None,
                cache_id: None,
                cache_host: None,
                cache_path: None,
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
                bid_id: None,
                ad_id: None,
                creative_id: None,
                renderer: None,
                cache_id: None,
                cache_host: None,
                cache_path: None,
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
    // for `PlatformHttpClient::send_async()`. Mock providers currently cannot
    // create realistic pending requests for the select loop without real
    // platform-backed transport handles.
    //
    // Untested timeout enforcement paths (require real backends):
    // - Deadline check in select() loop (drops remaining requests)
    // - Mediator skip when remaining_ms == 0 (bidding exhausts budget)
    // - Provider skip when effective_timeout == 0 (budget exhausted before launch)
    // - Provider context receives reduced timeout_ms per remaining budget
    //
    // Follow-up: introduce a thin abstraction over `PlatformHttpClient::select()`
    // so the deadline/drop logic can be unit-tested with mock futures instead
    // of requiring real platform backends. An `#[ignore]` integration test
    // exercising the full path via Viceroy would also catch regressions.

    #[test]
    fn test_no_providers_configured() {
        futures::executor::block_on(async {
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
            let req = http::Request::builder()
                .method(http::Method::GET)
                .uri("https://test.com/test")
                .body(edgezero_core::body::Body::empty())
                .expect("should build request");
            let context = create_test_auction_context(&settings, &req, 2000);

            let result = orchestrator.run_auction(&request, &context).await;

            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(format!("{}", err).contains("No providers configured"));
        });
    }

    #[test]
    fn provider_launch_failures_error_when_no_requests_launch() {
        futures::executor::block_on(async {
            let config = AuctionConfig {
                enabled: true,
                providers: vec!["launch-failing".to_string()],
                timeout_ms: 2000,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(LaunchFailingProvider));

            let request = create_test_auction_request();
            let settings = create_test_settings();
            let req = http::Request::builder()
                .method(http::Method::GET)
                .uri("https://test.com/test")
                .body(edgezero_core::body::Body::empty())
                .expect("should build request");
            let context = create_test_auction_context(&settings, &req, 2000);

            let result = orchestrator.run_auction(&request, &context).await;

            let err = result.expect_err("should fail when every provider launch fails");
            assert!(
                err.to_string()
                    .contains("All 1 configured provider(s) skipped or failed to launch"),
                "should explain that no configured provider request launched"
            );
        });
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
        let start = Instant::now();
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
        let start = Instant::now();
        // Use a timeout of 0 — elapsed will always exceed it
        let result = super::remaining_budget_ms(start, 0);
        assert_eq!(result, 0, "should return 0 when timeout is 0");
    }

    #[test]
    fn remaining_budget_decreases_over_time() {
        let start = Instant::now();
        std::thread::sleep(Duration::from_millis(50));
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
    fn select_error_is_attributed_to_correct_provider() {
        futures::executor::block_on(async {
            // Arrange: two stub providers backed by distinct backend names.
            // The stub HTTP client injects a select() error for the first request
            // that completes (backend-a). backend-b should still produce a success.
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec()); // consumed by send_async for backend-a
            stub.push_response(200, b"{}".to_vec()); // consumed by send_async for backend-b
            stub.push_select_error(); // first select() reports backend-a as failed

            let services = build_services_with_http_client(stub);
            // SAFETY: `Box::leak` creates a `'static` reference for test use only.
            // The leaked allocation is bounded to the test process lifetime.
            let services: &'static RuntimeServices = Box::leak(Box::new(services));

            let config = AuctionConfig {
                enabled: true,
                providers: vec!["provider-a".to_string(), "provider-b".to_string()],
                timeout_ms: 2000,
                mediator: None,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "provider-a",
                backend: "backend-a",
            }));
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "provider-b",
                backend: "backend-b",
            }));

            let request = create_test_auction_request();
            let settings = create_test_settings();
            let req = http::Request::builder()
                .method(http::Method::GET)
                .uri("https://example.com/test")
                .body(edgezero_core::body::Body::empty())
                .expect("should build request");
            let context = AuctionContext {
                settings: &settings,
                request: &req,
                timeout_ms: 2000,
                provider_responses: None,
                services,
            };

            // Act
            let result = orchestrator
                .run_auction(&request, &context)
                .await
                .expect("should complete auction even when one provider errors");

            // Assert: exactly two responses — one error, one success.
            assert_eq!(
                result.provider_responses.len(),
                2,
                "should collect responses from both providers"
            );

            let provider_a = result
                .provider_responses
                .iter()
                .find(|r| r.provider == "provider-a")
                .expect("should have provider-a response");
            let provider_b = result
                .provider_responses
                .iter()
                .find(|r| r.provider == "provider-b")
                .expect("should have provider-b response");

            assert_eq!(
                provider_a.status,
                BidStatus::Error,
                "provider-a should be marked error — select() Err was attributed via failed_backend_name"
            );
            assert_eq!(
                provider_b.status,
                BidStatus::Success,
                "provider-b should succeed — error was correctly isolated to provider-a"
            );
        });
    }

    #[test]
    fn dispatched_collection_reuses_provider_launch_context() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            let services = build_services_with_http_client(stub);
            let config = AuctionConfig {
                enabled: true,
                providers: vec!["provider-a".to_string()],
                timeout_ms: 750,
                mediator: None,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "provider-a",
                backend: "backend-a",
            }));
            let request = create_test_auction_request();
            let settings = create_test_settings();
            let downstream = http::Request::builder()
                .uri("https://publisher.example/article")
                .header(http::header::REFERER, "https://referrer.example/source")
                .body(edgezero_core::body::Body::empty())
                .expect("should build downstream request");
            let dispatch_context = AuctionContext {
                settings: &settings,
                request: &downstream,
                timeout_ms: 750,
                provider_responses: None,
                services: &services,
            };
            let dispatched = match orchestrator
                .dispatch_auction(&request, &dispatch_context)
                .await
            {
                DispatchAuctionOutcome::Dispatched(dispatched) => dispatched,
                _ => panic!("should dispatch provider request"),
            };
            let placeholder = http::Request::builder()
                .uri("https://placeholder.invalid/")
                .body(edgezero_core::body::Body::empty())
                .expect("should build placeholder request");
            let collect_context = AuctionContext {
                settings: &settings,
                request: &placeholder,
                timeout_ms: 750,
                provider_responses: None,
                services: &services,
            };

            let result = orchestrator
                .collect_dispatched_auction(dispatched, &services, &collect_context)
                .await;

            let response = result
                .provider_responses
                .first()
                .expect("should collect provider response");
            assert_eq!(
                response.metadata["context_referer"], "https://referrer.example/source",
                "should parse with the downstream request used at launch"
            );
            assert_eq!(
                response.metadata["context_timeout_ms"], 125,
                "should parse with the provider-capped launch timeout"
            );
        });
    }

    #[test]
    fn rejects_multi_provider_fanout_before_launch_on_sequential_platform() {
        futures::executor::block_on(async {
            // Arrange: two configured providers on a platform whose HTTP
            // client executes send_async eagerly (no concurrent fan-out).
            let stub = Arc::new(StubHttpClient::new());
            stub.set_concurrent_fanout(false);
            let stub_for_assertion = Arc::clone(&stub);

            let services = build_services_with_http_client(stub);
            // SAFETY: `Box::leak` creates a `'static` reference for test use only.
            // The leaked allocation is bounded to the test process lifetime.
            let services: &'static RuntimeServices = Box::leak(Box::new(services));

            let config = AuctionConfig {
                enabled: true,
                providers: vec!["provider-a".to_string(), "provider-b".to_string()],
                timeout_ms: 2000,
                mediator: None,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "provider-a",
                backend: "backend-a",
            }));
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "provider-b",
                backend: "backend-b",
            }));

            let request = create_test_auction_request();
            let settings = create_test_settings();
            let req = http::Request::builder()
                .method(http::Method::GET)
                .uri("https://example.com/test")
                .body(edgezero_core::body::Body::empty())
                .expect("should build request");
            let context = AuctionContext {
                settings: &settings,
                request: &req,
                timeout_ms: 2000,
                provider_responses: None,
                services,
            };

            // Act
            let result = orchestrator.run_auction(&request, &context).await;

            // Assert: rejected before any provider request launches.
            let err = result.expect_err("should reject multi-provider fan-out");
            assert!(
                format!("{err}").contains("sequentially"),
                "should explain the sequential-execution limitation"
            );
            assert!(
                stub_for_assertion.recorded_backend_names().is_empty(),
                "should not launch any provider request before rejecting"
            );
        });
    }

    #[test]
    fn dispatch_auction_skips_multi_provider_fanout_on_sequential_platform() {
        futures::executor::block_on(async {
            // Arrange: two configured providers on a platform whose HTTP
            // client executes send_async eagerly (no concurrent fan-out).
            // The initial-page dispatch path must apply the same guard as
            // run_providers_parallel or the summed provider latency lands
            // before the origin fetch.
            let stub = Arc::new(StubHttpClient::new());
            stub.set_concurrent_fanout(false);
            let stub_for_assertion = Arc::clone(&stub);

            let services = build_services_with_http_client(stub);
            // SAFETY: `Box::leak` creates a `'static` reference for test use only.
            // The leaked allocation is bounded to the test process lifetime.
            let services: &'static RuntimeServices = Box::leak(Box::new(services));

            let config = AuctionConfig {
                enabled: true,
                providers: vec!["provider-a".to_string(), "provider-b".to_string()],
                timeout_ms: 2000,
                mediator: None,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "provider-a",
                backend: "backend-a",
            }));
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "provider-b",
                backend: "backend-b",
            }));

            let request = create_test_auction_request();
            let settings = create_test_settings();
            let req = http::Request::builder()
                .method(http::Method::GET)
                .uri("https://example.com/test")
                .body(edgezero_core::body::Body::empty())
                .expect("should build request");
            let context = AuctionContext {
                settings: &settings,
                request: &req,
                timeout_ms: 2000,
                provider_responses: None,
                services,
            };

            // Act
            let dispatched = orchestrator.dispatch_auction(&request, &context).await;

            // Assert: no dispatch and no provider request launched.
            assert!(
                matches!(dispatched, DispatchAuctionOutcome::NotStarted),
                "should skip initial-page dispatch on sequential platforms"
            );
            assert!(
                stub_for_assertion.recorded_backend_names().is_empty(),
                "should not launch any provider request on a sequential platform"
            );
        });
    }

    #[test]
    fn decoded_aps_bid_competes_directly_by_cpm() {
        let orchestrator = AuctionOrchestrator::new(AuctionConfig::default());
        let floor_prices = HashMap::new();
        let response = |provider: &str, bid: Bid| AuctionResponse::success(provider, vec![bid], 1);

        let aps_wins = orchestrator.select_winning_bids(
            &[
                response("aps", auction_bid("aps", 2.0)),
                response("ordinary", auction_bid("ordinary", 1.0)),
            ],
            &floor_prices,
        );
        let winner = aps_wins.get("slot-1").expect("should select APS bid");
        assert_eq!(winner.bidder, "aps");
        assert!(winner.renderer.is_some());
        assert!(winner.creative.is_none());

        let ordinary_wins = orchestrator.select_winning_bids(
            &[
                response("aps", auction_bid("aps", 2.0)),
                response("ordinary", auction_bid("ordinary", 3.0)),
            ],
            &floor_prices,
        );
        assert_eq!(
            ordinary_wins
                .get("slot-1")
                .expect("should select ordinary bid")
                .bidder,
            "ordinary"
        );
    }

    #[test]
    fn test_apply_floor_prices_drops_bids_without_price() {
        // Price-less bids cannot be compared or delivered and remain fail-closed.
        let orchestrator = AuctionOrchestrator::new(AuctionConfig::default());
        let mut floor_prices = HashMap::new();
        floor_prices.insert("slot-1".to_string(), 1.00);

        let mut winning_bids = HashMap::new();
        winning_bids.insert(
            "slot-1".to_string(),
            Bid {
                slot_id: "slot-1".to_string(),
                price: None,
                currency: "USD".to_string(),
                creative: Some("<div>Ad</div>".to_string()),
                adomain: None,
                bidder: "aps".to_string(),
                width: 300,
                height: 250,
                nurl: None,
                burl: None,
                bid_id: None,
                ad_id: None,
                creative_id: None,
                renderer: None,
                cache_id: None,
                cache_host: None,
                cache_path: None,
                metadata: HashMap::new(),
            },
        );

        let filtered = orchestrator.apply_floor_prices(winning_bids, &floor_prices);

        assert!(
            filtered.is_empty(),
            "bid with None price should be dropped by apply_floor_prices"
        );
        assert!(
            !filtered.contains_key("slot-1"),
            "slot-1 should not survive when its bid has no price"
        );
    }

    #[test]
    fn test_apply_floor_prices_drops_decoded_aps_bid_below_floor() {
        // APS supplies decoded price at the provider boundary, so normal floors apply.
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
                bid_id: None,
                ad_id: None,
                creative_id: None,
                renderer: None,
                cache_id: None,
                cache_host: None,
                cache_path: None,
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
                bid_id: None,
                ad_id: None,
                creative_id: None,
                renderer: None,
                cache_id: None,
                cache_host: None,
                cache_path: None,
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
