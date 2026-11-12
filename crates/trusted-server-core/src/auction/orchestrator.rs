//! Auction orchestrator for managing multi-provider auctions.

use edgezero_core::ProviderFailurePhase;
use edgezero_core::error::EdgeError;
use edgezero_core::outbound::{
    OutboundBatch, OutboundBatchCheck, OutboundBatchNext, OutboundResponse, OutboundSlotResult,
};
use edgezero_core::time::{Deadline, MonotonicClock, MonotonicInstant};
use error_stack::Report;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::error::TrustedServerError;
use crate::outbound::{OutboundRequestSource, outbound_error};
use crate::platform::RuntimeServices;

use super::openrtb::unused_bidder_params_count;
use super::plan::AuctionPlan;
use super::provider::{
    AuctionProvider, GenericOpenRtbProvider, ProviderParseState, ProviderRequestOutcome,
};
use super::routing::route_auction;
use super::telemetry::AbandonedProviderCall;
use super::types::{AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus};
use crate::request_signing::RequestSigner;
use crate::trace::{
    TraceAuctionCarry, TraceAuctionTerminalReason, TraceAuctionTerminalStatus,
    TraceProviderObservation, TraceProviderRole,
};

/// In-flight auction requests dispatched to SSP backends.
///
/// Created by [`AuctionOrchestrator::dispatch_auction`] and consumed by
/// [`AuctionOrchestrator::collect_dispatched_auction`]. The bidder requests
/// travel as one `EdgeZero` batch whose cutoff is the auction deadline, so they
/// progress while the page loads. Collection keeps the bids that arrived in
/// time and never restarts the auction budget. Dropping the token cancels
/// every bidder request still in flight.
pub struct DispatchedAuction {
    /// The bidder batch, absent when no bidder request was built.
    batch: Option<OutboundBatch>,
    /// The bidder behind each batch slot, at the slot's index, until the slot
    /// is settled.
    slot_providers: Vec<Option<BidderSlot>>,
    completed_responses: Vec<AuctionResponse>,
    clock: MonotonicClock,
    auction_start: MonotonicInstant,
    auction_deadline: Deadline,
    floor_prices: HashMap<String, f64>,
    /// Carried so the mediator call in collect can pass it as the auction request.
    request: AuctionRequest,
    planned_unused_bidder_params: HashMap<String, u32>,
    planned_unroutable_bidder_count: u32,
    planned_provider_order: HashMap<String, usize>,
    trace: Option<TraceAuctionCarry>,
}

/// One bidder request's provider and the state its response needs.
struct BidderSlot {
    provider: Arc<GenericOpenRtbProvider>,
    parse_state: Option<ProviderParseState>,
    trace_observation: Option<TraceProviderObservation>,
}

/// Outcome of attempting to dispatch split-phase auction provider requests.
#[allow(
    clippy::large_enum_variant,
    reason = "Dispatched carries in-flight requests while failure preserves provider responses"
)]
pub enum DispatchAuctionOutcome {
    /// No provider request was started and no provider failure was observed.
    NotStarted,
    /// No provider request could be launched, but launch failures were observed.
    DispatchFailed {
        /// Original auction request.
        request: AuctionRequest,
        /// Provider launch-failure responses.
        provider_responses: Vec<AuctionResponse>,
        /// Fatal admission error that synchronous execution must propagate.
        ///
        /// Split publisher dispatch records the failure and continues without
        /// attempting provider network I/O.
        fatal_admission_error: Option<Report<TrustedServerError>>,
        /// Auction-level metadata materialized before the failure.
        metadata: HashMap<String, serde_json::Value>,
        /// Elapsed dispatch time.
        elapsed_ms: u64,
    },
    /// One or more providers produced an immediate response or started a request.
    Dispatched(DispatchedAuction),
}

impl DispatchedAuction {
    /// Consume the dispatch token without collecting provider responses.
    ///
    /// Cancels every bidder request still in flight, on the guest side.
    #[must_use]
    pub fn abandon(
        self,
    ) -> (
        AuctionRequest,
        Vec<AuctionResponse>,
        Vec<AbandonedProviderCall>,
        u64,
    ) {
        if let Some(trace) = &self.trace {
            trace.finish(
                TraceAuctionTerminalStatus::Abandoned,
                Some(TraceAuctionTerminalReason::Unknown),
            );
        }
        let elapsed_ms = elapsed_ms(&self.clock, self.auction_start);
        let undelivered = self.batch.map(OutboundBatch::cancel).unwrap_or_default();
        let abandoned = undelivered
            .into_iter()
            .filter_map(|index| self.slot_providers.get(index).and_then(Option::as_ref))
            .map(|slot| {
                AbandonedProviderCall::bidder(
                    slot.provider.provider_name(),
                    Some(u32::try_from(elapsed_ms).unwrap_or(u32::MAX)),
                )
            })
            .collect();
        (
            self.request,
            self.completed_responses,
            abandoned,
            elapsed_ms,
        )
    }

    pub(crate) fn trace_carry(&self) -> Option<TraceAuctionCarry> {
        self.trace.clone()
    }
}

#[cfg(test)]
impl DispatchedAuction {
    /// A dispatched auction with no bidder in flight, for publisher tests.
    ///
    /// Its deadline is `timeout_ms` after now on a fresh test clock.
    pub(crate) fn empty_for_test(request: AuctionRequest, timeout_ms: u32) -> Self {
        let clock = crate::test_support::scripted_client::TestClock::new().monotonic_clock();
        let auction_start = clock.now();
        Self {
            batch: None,
            slot_providers: Vec::new(),
            completed_responses: Vec::new(),
            auction_deadline: clock.deadline_after(Duration::from_millis(u64::from(timeout_ms))),
            clock,
            auction_start,
            floor_prices: HashMap::new(),
            request,
            planned_unused_bidder_params: HashMap::new(),
            planned_unroutable_bidder_count: 0,
            planned_provider_order: HashMap::new(),
            trace: None,
        }
    }

    /// Attaches `trace` to this test auction.
    pub(crate) fn with_trace_for_test(mut self, trace: TraceAuctionCarry) -> Self {
        self.trace = Some(trace);
        self
    }
}

const PROVIDER_ERROR_MESSAGE_CHARS: usize = 500;

pub(crate) const ERROR_TYPE_PARSE_RESPONSE: &str = "parse_response";
pub(crate) const ERROR_TYPE_LAUNCH_FAILED: &str = "launch_failed";
pub(crate) const ERROR_TYPE_TRANSPORT: &str = "transport";
pub(crate) const ERROR_TYPE_TIMEOUT: &str = "timeout";
/// A non-2xx HTTP status from an upstream SSP (e.g. a PBS 4xx/5xx). Distinct
/// from [`ERROR_TYPE_TRANSPORT`] (a connection-level failure) so telemetry can
/// bucket it separately. `pub(crate)` so producers such as the prebid provider
/// tag errors with the exact value the telemetry layer recognises.
pub(crate) const ERROR_TYPE_HTTP_STATUS: &str = "http_status";

/// Every server-owned `error_type` classification.
///
/// Consumers that reproduce these values — notably the `ts-debug` redaction
/// layer in [`crate::publisher`] — validate against this list so a new
/// classification cannot silently disappear from their output.
pub(crate) const ERROR_TYPE_ALL: &[&str] = &[
    ERROR_TYPE_PARSE_RESPONSE,
    ERROR_TYPE_LAUNCH_FAILED,
    ERROR_TYPE_TRANSPORT,
    ERROR_TYPE_TIMEOUT,
    ERROR_TYPE_HTTP_STATUS,
];

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

// Transport failures carry a static message: the underlying client error may
// reference upstream-controlled content, so it is logged server-side rather
// than surfaced in the public /auction response.
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

fn provider_skipped_response(provider_name: &str) -> AuctionResponse {
    AuctionResponse::no_bid(provider_name, 0).with_metadata(
        "routing",
        serde_json::json!({"skipped_no_eligible_slots": true}),
    )
}

/// Whether `error` is a failed backend registration, which no request byte
/// follows.
///
/// A registration that runs out of budget stays a timeout.
fn failed_backend_registration(error: &EdgeError) -> bool {
    !matches!(error, EdgeError::GatewayTimeout { .. })
        && error
            .provider_failure_detail()
            .is_some_and(|detail| detail.phase() == ProviderFailurePhase::BackendRegistration)
}

/// Whole milliseconds in `duration`, saturating.
fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Whole milliseconds `clock` has moved since `since`.
fn elapsed_ms(clock: &MonotonicClock, since: MonotonicInstant) -> u64 {
    duration_ms(clock.now().saturating_duration_since(since))
}

/// Whole milliseconds left before `deadline` on `clock`, or zero once it passed.
fn remaining_ms(clock: &MonotonicClock, deadline: Deadline) -> u32 {
    deadline.remaining_at(clock.now()).map_or(0, |remaining| {
        u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX)
    })
}

fn routing_metadata(unroutable_bidder_count: u32) -> HashMap<String, serde_json::Value> {
    HashMap::from([(
        "routing".to_string(),
        serde_json::json!({"unroutable_bidder_count": unroutable_bidder_count}),
    )])
}

/// Attach only the count derived from the routed provider input at dispatch.
///
/// This is intentionally applied after every provider outcome is materialized,
/// including failures produced before or during parsing. Only providers skipped
/// with `skipped_no_eligible_slots` retain an exclusive skipped diagnostic.
fn materialize_planned_response(
    mut response: AuctionResponse,
    unused_bidder_params_count: u32,
) -> AuctionResponse {
    let routing = response
        .metadata
        .entry("routing".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if routing
        .get("skipped_no_eligible_slots")
        .is_some_and(|value| value == &serde_json::json!(true))
    {
        return response;
    }
    if !routing.is_object() {
        *routing = serde_json::json!({});
    }
    routing
        .as_object_mut()
        .expect("should normalize planned routing metadata to an object")
        .insert(
            "unused_bidder_params_count".to_string(),
            serde_json::json!(unused_bidder_params_count),
        );
    response
}

/// Logs one failed bidder or mediator request without its provider text.
///
/// A timeout is an expected auction outcome, so it logs at debug. Every other
/// failure goes through [`outbound_error`], which logs its typed detail.
fn log_request_failure(role: &str, provider_name: &str, error: EdgeError) {
    if matches!(error, EdgeError::GatewayTimeout { .. }) {
        log::debug!("{role} '{provider_name}' exhausted its response budget");
        return;
    }
    let report = outbound_error(
        error,
        TrustedServerError::Auction {
            message: format!("{role} {provider_name} request failed"),
        },
        OutboundRequestSource::Internal,
    );
    log::warn!("{role} '{provider_name}' request failed: {report:?}");
}

/// Manages auction execution across multiple providers.
pub struct AuctionOrchestrator {
    enabled: bool,
    plan: Arc<AuctionPlan>,
    planned_providers: Vec<Arc<GenericOpenRtbProvider>>,
    mediator: Option<Arc<dyn AuctionProvider>>,
}

impl AuctionOrchestrator {
    /// Create the live orchestrator from one shared compiled auction plan.
    #[must_use]
    pub fn from_plan(plan: Arc<AuctionPlan>, mediator: Option<Arc<dyn AuctionProvider>>) -> Self {
        let planned_providers = plan
            .providers()
            .iter()
            .cloned()
            .map(GenericOpenRtbProvider::new)
            .map(Arc::new)
            .collect();
        Self {
            enabled: plan.enabled(),
            plan,
            planned_providers,
            mediator,
        }
    }

    /// Return whether this orchestrator and another plan consumer share the same plan allocation.
    #[must_use]
    pub fn shares_plan(&self, plan: &Arc<AuctionPlan>) -> bool {
        Arc::ptr_eq(&self.plan, plan)
    }

    /// Get the number of registered providers.
    #[must_use]
    pub fn provider_count(&self) -> usize {
        self.planned_providers.len()
    }

    async fn run_planned_auction(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        let trace = context.request.extensions().get::<TraceAuctionCarry>();
        let _trace_guard = trace.map(TraceAuctionCarry::cancellation_guard);
        match self.dispatch_planned_auction(request, context).await {
            DispatchAuctionOutcome::Dispatched(dispatched) => Ok(self
                .collect_dispatched_auction(dispatched, context.services, context)
                .await),
            DispatchAuctionOutcome::DispatchFailed {
                fatal_admission_error,
                ..
            } => {
                if let Some(trace) = trace {
                    trace.finish_dispatch_failed(false);
                }
                if let Some(error) = fatal_admission_error {
                    return Err(error.change_context(TrustedServerError::Auction {
                        message: "Planned auction admission failed".to_string(),
                    }));
                }
                Err(Report::new(TrustedServerError::Auction {
                    message: "All eligible planned provider requests failed to launch".to_string(),
                }))
            }
            DispatchAuctionOutcome::NotStarted => {
                if self.planned_providers.is_empty() {
                    if let Some(trace) = trace {
                        trace.finish(TraceAuctionTerminalStatus::Completed, None);
                    }
                    Ok(OrchestrationResult::no_bid())
                } else {
                    if let Some(trace) = trace {
                        trace.finish_dispatch_failed(true);
                    }
                    Err(Report::new(TrustedServerError::Auction {
                        message: "No planned provider request was started".to_string(),
                    }))
                }
            }
        }
    }

    /// Execute an auction through the compiled plan.
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
        if !self.enabled {
            if let Some(trace) = context.request.extensions().get::<TraceAuctionCarry>() {
                trace.finish(
                    TraceAuctionTerminalStatus::Skipped,
                    Some(TraceAuctionTerminalReason::PolicySkipped),
                );
            }
            return Ok(OrchestrationResult::no_bid());
        }
        self.run_planned_auction(request, context).await
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

    async fn dispatch_planned_auction(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> DispatchAuctionOutcome {
        let plan = &self.plan;
        let trace = context
            .request
            .extensions()
            .get::<TraceAuctionCarry>()
            .cloned();
        if self.planned_providers.is_empty() {
            return DispatchAuctionOutcome::NotStarted;
        }

        let clock = context.services.clock().clone();
        let auction_start = clock.now();
        let auction_deadline =
            clock.deadline_after(Duration::from_millis(u64::from(context.timeout_ms)));
        let routed = route_auction(
            request.clone(),
            context.request,
            plan,
            context.services.client_info().client_ip,
        );
        let planned_unused_bidder_params = routed
            .inputs()
            .iter()
            .map(|input| {
                (
                    input.provider_id().as_str().to_string(),
                    unused_bidder_params_count(
                        &plan
                            .provider(input.provider_id())
                            .expect("should find routed provider in compiled plan")
                            .profile,
                        input,
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let planned_unroutable_bidder_count = routed.diagnostics().unroutable_bidder_count();
        // A zero request budget is terminal before signing admission. In the
        // split path, signer initialization would otherwise read config and
        // secret stores even though every provider is materialized as timeout.
        let signer_result = if context.timeout_ms == 0 {
            Ok(None)
        } else {
            plan.signing_enabled()
                .then(|| RequestSigner::from_services(context.services))
                .transpose()
        };
        let signer = match signer_result {
            Ok(signer) => signer,
            Err(error) => {
                log::warn!("Planned auction signer initialization failed: {error:?}");
                let mut provider_responses = routed
                    .skipped_no_eligible_provider_ids()
                    .iter()
                    .map(|id| provider_skipped_response(id.as_str()))
                    .chain(routed.inputs().iter().map(|input| {
                        materialize_planned_response(
                            provider_launch_failed_response(input.provider_id().as_str(), 0),
                            unused_bidder_params_count(
                                &plan
                                    .provider(input.provider_id())
                                    .expect("should find routed provider in compiled plan")
                                    .profile,
                                input,
                            ),
                        )
                    }))
                    .collect::<Vec<_>>();
                let provider_order = plan
                    .providers()
                    .iter()
                    .enumerate()
                    .map(|(index, provider)| (provider.id.as_str(), index))
                    .collect::<HashMap<_, _>>();
                provider_responses.sort_by_key(|response| {
                    provider_order
                        .get(response.provider.as_str())
                        .copied()
                        .unwrap_or(usize::MAX)
                });
                return DispatchAuctionOutcome::DispatchFailed {
                    request: request.clone(),
                    provider_responses,
                    fatal_admission_error: Some(error),
                    metadata: routing_metadata(planned_unroutable_bidder_count),
                    elapsed_ms: elapsed_ms(&clock, auction_start),
                };
            }
        };
        let mut completed_responses = routed
            .skipped_no_eligible_provider_ids()
            .iter()
            .map(|id| provider_skipped_response(id.as_str()))
            .collect::<Vec<_>>();
        let planned_provider_order = plan
            .providers()
            .iter()
            .enumerate()
            .map(|(index, provider)| (provider.id.as_str().to_string(), index))
            .collect::<HashMap<_, _>>();
        let mut bidder_requests = Vec::new();
        let mut slot_providers = Vec::new();
        let mut launch_failure_count = 0usize;

        for input in routed.inputs() {
            let Some(provider) = self
                .planned_providers
                .iter()
                .find(|provider| provider.provider_name() == input.provider_id().as_str())
                .cloned()
            else {
                launch_failure_count += 1;
                completed_responses.push(provider_launch_failed_response(
                    input.provider_id().as_str(),
                    0,
                ));
                continue;
            };
            let logical_budget_ms =
                remaining_ms(&clock, auction_deadline).min(provider.timeout_ms());
            if logical_budget_ms == 0 {
                log::debug!(
                    "Planned provider '{}' has no remaining auction budget before launch",
                    provider.provider_name()
                );
                completed_responses.push(provider_timeout_response(provider.provider_name(), 0));
                continue;
            }
            let built_at = clock.now();
            let trace_observation = trace
                .as_ref()
                .map(|trace| trace.launch_provider(TraceProviderRole::Bidder));
            match provider.request_bids_routed(input, &routed, logical_budget_ms, signer.as_ref()) {
                Ok(ProviderRequestOutcome::Ready {
                    request: bidder_request,
                    parse_state,
                }) => {
                    bidder_requests.push(bidder_request);
                    slot_providers.push(Some(BidderSlot {
                        provider,
                        parse_state,
                        trace_observation,
                    }));
                }
                Ok(ProviderRequestOutcome::Immediate(response)) => {
                    if let (Some(trace), Some(observation)) = (&trace, trace_observation) {
                        trace.observe_response(observation, &response);
                    }
                    completed_responses.push(response);
                }
                Err(error) => {
                    if let (Some(trace), Some(observation)) = (&trace, trace_observation) {
                        trace.observe_failure(observation);
                    }
                    log::warn!(
                        "Planned provider '{}' failed to dispatch: {error:?}",
                        provider.provider_name()
                    );
                    launch_failure_count += 1;
                    completed_responses.push(provider_launch_failed_response(
                        provider.provider_name(),
                        elapsed_ms(&clock, built_at),
                    ));
                }
            }
        }

        if routed.inputs().is_empty() && completed_responses.is_empty() {
            return DispatchAuctionOutcome::NotStarted;
        }

        // Every bidder request leaves together, after all are built and
        // signed. An auction whose providers were all filtered out starts no
        // batch, so it completes without waiting for the deadline.
        let batch = if bidder_requests.is_empty() {
            None
        } else {
            let mut batch = context
                .services
                .outbound_client()
                .start_batch_until(bidder_requests, auction_deadline);
            // Fastly registers every bidder backend while it starts the batch,
            // so a registration failure is already recorded here.
            launch_failure_count += settle_recorded_slots(
                &mut batch,
                &mut slot_providers,
                &mut completed_responses,
                trace.as_ref(),
            )
            .await;
            Some(batch)
        };

        // A pre-launch timeout is a completed provider outcome. Only a set
        // made entirely of launch failures crosses the fatal boundary.
        if launch_failure_count > 0 && launch_failure_count == routed.inputs().len() {
            for response in &mut completed_responses {
                if let Some(&count) = planned_unused_bidder_params.get(response.provider.as_str()) {
                    *response = materialize_planned_response(response.clone(), count);
                }
            }
            completed_responses.sort_by_key(|response| {
                planned_provider_order
                    .get(response.provider.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            return DispatchAuctionOutcome::DispatchFailed {
                request: request.clone(),
                provider_responses: completed_responses,
                fatal_admission_error: None,
                metadata: routing_metadata(planned_unroutable_bidder_count),
                elapsed_ms: elapsed_ms(&clock, auction_start),
            };
        }
        DispatchAuctionOutcome::Dispatched(DispatchedAuction {
            batch,
            slot_providers,
            completed_responses,
            clock,
            auction_start,
            auction_deadline,
            floor_prices: self.floor_prices_by_slot(request),
            request: request.clone(),
            planned_unused_bidder_params,
            planned_unroutable_bidder_count,
            planned_provider_order,
            trace,
        })
    }

    /// Dispatch SSP bid requests without waiting for their responses.
    ///
    /// Builds every routed provider request and starts them together as one
    /// `EdgeZero` batch whose cutoff is the auction deadline, then returns a
    /// [`DispatchedAuction`] token. The bids progress while the caller waits
    /// on the publisher origin.
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
        let trace = context.request.extensions().get::<TraceAuctionCarry>();
        if !self.enabled {
            if let Some(trace) = trace {
                trace.finish(
                    TraceAuctionTerminalStatus::Skipped,
                    Some(TraceAuctionTerminalReason::PolicySkipped),
                );
            }
            return DispatchAuctionOutcome::NotStarted;
        }
        let mut guard = trace.map(TraceAuctionCarry::cancellation_guard);
        let outcome = self.dispatch_planned_auction(request, context).await;
        match &outcome {
            DispatchAuctionOutcome::Dispatched(_) => {
                if let Some(guard) = &mut guard {
                    guard.disarm();
                }
            }
            DispatchAuctionOutcome::DispatchFailed { .. } => {
                if let Some(trace) = trace {
                    trace.finish_dispatch_failed(false);
                }
            }
            DispatchAuctionOutcome::NotStarted => {
                if let Some(trace) = trace {
                    trace.finish_dispatch_failed(true);
                }
            }
        }
        outcome
    }

    /// Collect bid responses from a previously-dispatched auction.
    ///
    /// Observes the bidder batch until it finishes, then forwards the
    /// collected bids to the mediator, if one is configured. A bid that has
    /// not arrived by the auction deadline becomes a provider timeout.
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
            batch,
            slot_providers,
            completed_responses,
            clock,
            auction_start,
            auction_deadline,
            floor_prices,
            request,
            planned_unused_bidder_params,
            planned_unroutable_bidder_count,
            planned_provider_order,
            trace,
        } = dispatched;
        let _trace_guard = trace.as_ref().map(TraceAuctionCarry::cancellation_guard);

        log::info!(
            "Collecting {} in-flight SSP responses ({}ms of the auction budget remaining)",
            slot_providers.iter().flatten().count(),
            remaining_ms(&clock, auction_deadline),
        );

        let mut responses = completed_responses;
        let mut unresolved = slot_providers;
        if let Some(mut batch) = batch {
            let failed = loop {
                match batch.next().await {
                    Ok(OutboundBatchNext::Item(item)) => {
                        let (index, result) = item.into_parts();
                        let Some(slot) = unresolved.get_mut(index).and_then(Option::take) else {
                            log::warn!("Auction batch reported unknown slot {index}, ignoring");
                            continue;
                        };
                        responses.push(settle_bidder_slot(slot, result, trace.as_ref()).await);
                    }
                    Ok(OutboundBatchNext::Finished(_termination)) => break false,
                    Err(error) => {
                        if let Some(trace) = &trace {
                            trace.observe_collection_failure();
                        }
                        let report = outbound_error(
                            error,
                            TrustedServerError::Auction {
                                message: "Auction bidder batch failed".to_string(),
                            },
                            OutboundRequestSource::Internal,
                        );
                        log::warn!("Auction bidder batch failed: {report:?}");
                        break true;
                    }
                }
            };
            // The batch never resolves these slots: a cutoff leaves a late bid
            // unresolved, and a failed batch leaves every slot it had not
            // reported.
            let unresolved_ms = duration_ms(
                clock
                    .now()
                    .min(auction_deadline.instant())
                    .saturating_duration_since(auction_start),
            );
            for slot in unresolved.into_iter().flatten() {
                if let (Some(trace), Some(observation)) = (&trace, slot.trace_observation) {
                    trace.observe_failure(observation);
                }
                let provider_name = slot.provider.provider_name();
                responses.push(if failed {
                    provider_transport_failed_response(provider_name, unresolved_ms)
                } else {
                    log::debug!(
                        "Planned provider '{provider_name}' exhausted its dispatched auction collection budget"
                    );
                    provider_timeout_response(provider_name, unresolved_ms)
                });
            }
        }
        for response in &mut responses {
            if let Some(&count) = planned_unused_bidder_params.get(response.provider.as_str()) {
                *response = materialize_planned_response(response.clone(), count);
            }
        }
        responses.sort_by_key(|response| {
            planned_provider_order
                .get(response.provider.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });

        let mediated = match &self.mediator {
            Some(mediator) => {
                self.run_mediator(
                    mediator.as_ref(),
                    &responses,
                    &MediatorRun {
                        request: &request,
                        clock: &clock,
                        auction_deadline,
                        trace: trace.as_ref(),
                        services,
                        context,
                    },
                )
                .await
            }
            None => None,
        };
        let winning_bids = match &mediated {
            Some(mediator_response) => {
                let winning = mediator_response
                    .bids
                    .iter()
                    .filter_map(|bid| {
                        if bid.price.is_none() {
                            log::warn!(
                                "Mediator '{}' returned bid for slot '{}' without decoded price - skipping",
                                mediator_response.provider,
                                bid.slot_id
                            );
                            None
                        } else {
                            Some((bid.slot_id.clone(), bid.clone()))
                        }
                    })
                    .collect();
                self.apply_floor_prices(winning, &floor_prices)
            }
            None => self.select_winning_bids(&responses, &floor_prices),
        };

        if let Some(trace) = &trace {
            trace.finish(TraceAuctionTerminalStatus::Completed, None);
        }
        OrchestrationResult {
            provider_responses: responses,
            mediator_response: mediated,
            winning_bids,
            total_time_ms: elapsed_ms(&clock, auction_start),
            metadata: routing_metadata(planned_unroutable_bidder_count),
        }
    }

    /// Sends the mediator after the bidder batch, bounded by the auction deadline.
    ///
    /// Returns `None`, so the caller ranks bids locally, when the deadline has
    /// passed, or the mediator fails, times out, or returns an unparsable
    /// response.
    async fn run_mediator(
        &self,
        mediator: &dyn AuctionProvider,
        responses: &[AuctionResponse],
        run: &MediatorRun<'_>,
    ) -> Option<AuctionResponse> {
        let remaining = remaining_ms(run.clock, run.auction_deadline);
        let logical_budget_ms = remaining.min(mediator.timeout_ms());
        if logical_budget_ms == 0 {
            log::warn!(
                "A_deadline exhausted before mediator '{}' — returning {} SSP bids without mediation",
                mediator.provider_name(),
                responses.len(),
            );
            return None;
        }
        // This logs a configured mediator identifier and timeout values, not request data or secrets.
        log::info!(
            "Running mediator '{}' with {}ms logical budget (A_deadline remaining: {}ms, configured: {}ms)",
            mediator.provider_name(),
            logical_budget_ms,
            remaining,
            mediator.timeout_ms(),
        );
        // The mediator runs on the collect path. See the doc-comment on
        // `AuctionContext::request`: the real client request was already
        // consumed during dispatch, so we substitute a canonical placeholder
        // URL. Any future mediator that needs real client headers must
        // snapshot them at dispatch time onto `DispatchedAuction` rather than
        // reading `context.request` here.
        let placeholder = http::Request::builder()
            .uri(crate::auction::types::MEDIATOR_PLACEHOLDER_URL)
            .body(edgezero_core::body::Body::empty())
            .unwrap_or_else(|_| http::Request::new(edgezero_core::body::Body::empty()));
        let mediator_context = AuctionContext {
            settings: run.context.settings,
            request: &placeholder,
            timeout_ms: logical_budget_ms,
            provider_responses: Some(responses),
            services: run.context.services,
        };
        let mut trace_observation = run
            .trace
            .map(|trace| trace.launch_provider(TraceProviderRole::Mediator));
        let mediator_start = run.clock.now();
        let mediated = match mediator.request_bids(run.request, &mediator_context).await {
            Ok(ProviderRequestOutcome::Immediate(response)) => Some(response),
            Ok(ProviderRequestOutcome::Ready {
                request: mediator_request,
                parse_state,
            }) => match run
                .services
                .outbound_client()
                .send(mediator_request.deadline(run.auction_deadline))
                .await
            {
                Ok(response) => {
                    parse_mediator_response(
                        mediator,
                        response,
                        elapsed_ms(run.clock, mediator_start),
                        run.request,
                        &mediator_context,
                        parse_state.as_deref(),
                    )
                    .await
                }
                Err(error) => {
                    log_request_failure("Mediator", mediator.provider_name(), error);
                    None
                }
            },
            Err(error) => {
                log::warn!(
                    "Mediator '{}' failed to dispatch: {:?}",
                    mediator.provider_name(),
                    error
                );
                None
            }
        };
        if let (Some(trace), Some(observation)) = (run.trace, trace_observation.take()) {
            match &mediated {
                Some(response) => trace.observe_response(observation, response),
                None => trace.observe_failure(observation),
            }
        }
        mediated
    }

    /// Check if orchestrator is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// What the mediator call needs from one collection.
struct MediatorRun<'a> {
    request: &'a AuctionRequest,
    clock: &'a MonotonicClock,
    auction_deadline: Deadline,
    trace: Option<&'a TraceAuctionCarry>,
    services: &'a RuntimeServices,
    context: &'a AuctionContext<'a>,
}

/// Turns one resolved bidder slot into the provider's auction response.
///
/// The slot's `elapsed` runs from the batch start to the bid's arrival, so a
/// collection delayed after a timely arrival reports the same response time.
async fn settle_bidder_slot(
    slot: BidderSlot,
    result: OutboundSlotResult,
    trace: Option<&TraceAuctionCarry>,
) -> AuctionResponse {
    let OutboundSlotResult {
        elapsed, outcome, ..
    } = result;
    let response_time_ms = duration_ms(elapsed);
    let provider_name = slot.provider.provider_name();
    let observation = slot.trace_observation;
    let (response, succeeded) = match outcome {
        Ok(response) => match slot
            .provider
            .parse_response_with_state(response, response_time_ms, slot.parse_state.as_deref())
            .await
        {
            Ok(parsed) => (parsed, true),
            Err(error) => (
                provider_error_response(
                    provider_name,
                    response_time_ms,
                    ERROR_TYPE_PARSE_RESPONSE,
                    &error,
                ),
                false,
            ),
        },
        Err(error) => {
            let timed_out = matches!(error, EdgeError::GatewayTimeout { .. });
            let launch_failed = failed_backend_registration(&error);
            log_request_failure("Planned provider", provider_name, error);
            let response = if timed_out {
                provider_timeout_response(provider_name, response_time_ms)
            } else if launch_failed {
                provider_launch_failed_response(provider_name, response_time_ms)
            } else {
                provider_transport_failed_response(provider_name, response_time_ms)
            };
            (response, false)
        }
    };
    if let (Some(trace), Some(observation)) = (trace, observation) {
        if succeeded {
            trace.observe_response(observation, &response);
        } else {
            trace.observe_failure(observation);
        }
    }
    response
}

/// Settles the bidder slots `batch` has already recorded, without waiting.
///
/// Returns how many of them failed backend registration. A failed batch fails
/// every later observation too, so collection marks the slots it leaves.
async fn settle_recorded_slots(
    batch: &mut OutboundBatch,
    slots: &mut [Option<BidderSlot>],
    responses: &mut Vec<AuctionResponse>,
    trace: Option<&TraceAuctionCarry>,
) -> usize {
    let mut registration_failures = 0;
    loop {
        match batch.check() {
            Ok(OutboundBatchCheck::Item(item)) => {
                let (index, result) = item.into_parts();
                let Some(slot) = slots.get_mut(index).and_then(Option::take) else {
                    log::warn!("Auction batch reported unknown slot {index}, ignoring");
                    continue;
                };
                if result
                    .outcome
                    .as_ref()
                    .is_err_and(failed_backend_registration)
                {
                    registration_failures += 1;
                }
                responses.push(settle_bidder_slot(slot, result, trace).await);
            }
            Ok(OutboundBatchCheck::Pending | OutboundBatchCheck::Finished(_)) => {
                return registration_failures;
            }
            Err(error) => {
                let report = outbound_error(
                    error,
                    TrustedServerError::Auction {
                        message: "Auction bidder batch failed".to_string(),
                    },
                    OutboundRequestSource::Internal,
                );
                log::warn!("Auction bidder batch failed as it started: {report:?}");
                return registration_failures;
            }
        }
    }
}

/// Parses a mediator response, logging and dropping a parse failure.
async fn parse_mediator_response(
    mediator: &dyn AuctionProvider,
    response: OutboundResponse,
    response_time_ms: u64,
    request: &AuctionRequest,
    context: &AuctionContext<'_>,
    parse_state: Option<&(dyn core::any::Any + Send + Sync)>,
) -> Option<AuctionResponse> {
    mediator
        .parse_response_with_context_and_state(
            response,
            response_time_ms,
            request,
            context,
            parse_state,
        )
        .await
        .map_err(|error| {
            log::warn!(
                "Mediator '{}' parse failed: {:?}",
                mediator.provider_name(),
                error
            );
        })
        .ok()
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
    fn no_bid() -> Self {
        Self {
            provider_responses: Vec::new(),
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 0,
            metadata: HashMap::new(),
        }
    }

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
    use std::str::FromStr as _;
    use std::time::Duration;

    use base64::Engine as _;
    use edgezero_core::error::{BadGatewayReason, BudgetSource, EdgeError};
    use edgezero_core::outbound::OutboundResponse;
    use edgezero_core::{FastlyFailureCause, ProviderFailureDetail, ProviderFailurePhase};
    use http::{Method, StatusCode};

    use crate::auction::orchestrator::DispatchAuctionOutcome;
    use crate::auction::plan::{
        AuctionPlan, AuctionPlanConfig, NotificationConfig, ProviderConfig, ProviderId, RoutingMode,
    };
    use crate::auction::provider::{
        AuctionProvider, GenericOpenRtbProvider, ProviderRequestOutcome,
    };
    use crate::auction::routing::{RoutingDiagnostics, route_auction};

    use crate::auction::types::{
        AdFormat, AdSlot, ApsRendererV1, ApsTagType, AuctionContext, AuctionRequest,
        AuctionResponse, Bid, BidRenderer, BidStatus, MediaType, PublisherInfo, UserInfo,
    };
    use crate::error::TrustedServerError;
    use crate::integrations::adserver_mock::{AdServerMockConfig, AdServerMockProvider};
    use crate::platform::test_support::build_services_with_outbound_client;
    use crate::platform::{
        PlatformConfigStore, PlatformError, PlatformSecretStore, RuntimeServices, StoreId,
        StoreName,
    };
    use crate::test_support::scripted_client::{
        ScriptedHttpClient, ScriptedReply, TestClock, outbound_response,
    };
    use crate::test_support::tests::crate_test_settings_str;
    use error_stack::Report;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        AuctionOrchestrator, DispatchedAuction, ERROR_TYPE_LAUNCH_FAILED, ERROR_TYPE_TIMEOUT,
        ERROR_TYPE_TRANSPORT, OrchestrationResult,
    };

    /// Endpoint every [`planned_config`] provider shares.
    const PLANNED_ENDPOINT: &str = "https://example.test/openrtb";

    /// Endpoint every [`planned_aps_instances_config`] provider shares.
    const APS_ENDPOINT: &str = "https://aps.example/e/pb/bid";

    /// Builds the live orchestrator over `plan` with an optional mediator.
    fn planned_orchestrator(
        plan: impl Into<Arc<AuctionPlan>>,
        mediator: Option<Arc<dyn AuctionProvider>>,
    ) -> AuctionOrchestrator {
        AuctionOrchestrator::from_plan(plan.into(), mediator)
    }

    /// Builds an orchestrator over an empty plan, for the ranking helpers.
    fn empty_orchestrator(enabled: bool) -> AuctionOrchestrator {
        planned_orchestrator(
            AuctionPlan::compile(AuctionPlanConfig {
                timeout_ms: 777,
                ..AuctionPlanConfig::default()
            })
            .expect("should compile an empty plan")
            .with_enabled(enabled),
            None,
        )
    }

    /// A scripted reply with `status` and `body`.
    fn reply(status: u16, body: Vec<u8>) -> ScriptedReply {
        ScriptedReply::status(StatusCode::from_u16(status).expect("should use a valid status"))
            .body(body)
    }

    /// Queues one reply for the next request to `endpoint`.
    fn respond(client: &ScriptedHttpClient, endpoint: &str, status: u16, body: Vec<u8>) {
        client.respond_to(Method::POST, endpoint, reply(status, body));
    }

    /// Queues one reply for the next request to [`PLANNED_ENDPOINT`].
    fn respond_planned(client: &ScriptedHttpClient, status: u16, body: Vec<u8>) {
        respond(client, PLANNED_ENDPOINT, status, body);
    }

    /// The URI of every request sent so far, in order.
    fn recorded_uris(client: &ScriptedHttpClient) -> Vec<String> {
        client
            .requests()
            .iter()
            .map(|request| request.uri.to_string())
            .collect()
    }

    /// The headers of every request sent so far, as lowercase name and value pairs.
    fn recorded_headers(client: &ScriptedHttpClient) -> Vec<Vec<(String, String)>> {
        client
            .requests()
            .iter()
            .map(|request| {
                request
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_owned(),
                            value
                                .to_str()
                                .expect("should send UTF-8 header values")
                                .to_owned(),
                        )
                    })
                    .collect()
            })
            .collect()
    }

    /// Queues one reply with response `headers` for the next request to `endpoint`.
    fn respond_with_headers(
        client: &ScriptedHttpClient,
        endpoint: &str,
        status: u16,
        body: Vec<u8>,
        headers: &[(&str, &str)],
    ) {
        let reply = headers
            .iter()
            .fold(reply(status, body), |reply, (name, value)| {
                reply.header(name, value)
            });
        client.respond_to(Method::POST, endpoint, reply);
    }

    /// The headers of every request sent so far, with raw value bytes.
    fn recorded_header_bytes(client: &ScriptedHttpClient) -> Vec<Vec<(String, Vec<u8>)>> {
        client
            .requests()
            .iter()
            .map(|request| {
                request
                    .headers
                    .iter()
                    .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
                    .collect()
            })
            .collect()
    }

    /// The body of every request sent so far.
    fn recorded_bodies(client: &ScriptedHttpClient) -> Vec<Vec<u8>> {
        client
            .requests()
            .iter()
            .map(|request| request.body.to_vec())
            .collect()
    }

    fn planned_config(providers: &[(&str, RoutingMode)], signing: bool) -> AuctionPlanConfig {
        AuctionPlanConfig {
            timeout_ms: 777,
            providers: providers
                .iter()
                .map(|(id, routing)| {
                    (
                        ProviderId::from_str(id).expect("should parse fictional provider ID"),
                        ProviderConfig {
                            allow_insecure_http: false,
                            protocol: "openrtb-2.6".to_string(),
                            profile: "standard".to_string(),
                            endpoint: PLANNED_ENDPOINT.to_string(),
                            timeout_ms: Some(1_000),
                            routing: *routing,
                            notifications: Default::default(),
                            profile_config: serde_json::json!({}),
                        },
                    )
                })
                .collect(),
            bidders: BTreeMap::new(),
            mediator: None,
            request_signing: signing.then(|| crate::settings::RequestSigning {
                enabled: true,
                config_store_id: "fictional-config-store".to_string(),
                secret_store_id: "fictional-secret-store".to_string(),
            }),
        }
    }

    /// Endpoint of the bidder that the launch-failure tests make fail.
    const FAILING_BIDDER: &str = "https://failing.example.test/openrtb";

    /// [`FAILING_BIDDER`] with a backslash in its query. The plan accepts it,
    /// but `EdgeZero` builds no outbound request for a target with a backslash.
    const UNBUILDABLE_BIDDER: &str = r"https://failing.example.test/openrtb?x=\";

    /// Endpoint of the bidder whose backend registration runs out of budget.
    const TIMED_OUT_BIDDER: &str = "https://timed-out.example.test/openrtb";

    /// How a launch-failure test makes its bidder fail to launch.
    #[derive(Clone, Copy, Debug)]
    enum LaunchFailure {
        /// Trusted Server cannot build the request, so it never joins the batch.
        Construction,
        /// Fastly cannot register the bidder backend as the batch starts.
        Registration,
    }

    impl LaunchFailure {
        const ALL: [Self; 2] = [Self::Construction, Self::Registration];

        /// The endpoint a provider failing this way is configured with.
        fn endpoint(self) -> &'static str {
            match self {
                Self::Construction => UNBUILDABLE_BIDDER,
                Self::Registration => FAILING_BIDDER,
            }
        }

        /// Scripts the failure for `auctions` auctions on `bidders`.
        fn script(self, bidders: &ScriptedHttpClient, auctions: usize) {
            if matches!(self, Self::Registration) {
                for _ in 0..auctions {
                    bidders.respond_to(
                        Method::POST,
                        FAILING_BIDDER,
                        ScriptedReply::error(backend_registration_error()),
                    );
                }
            }
        }

        /// How many bid requests reach `EdgeZero` in one auction.
        fn requests_per_auction(self) -> usize {
            match self {
                Self::Construction => 0,
                Self::Registration => 1,
            }
        }
    }

    /// The error Fastly reports when it cannot register a bidder's backend.
    fn backend_registration_error() -> EdgeError {
        EdgeError::bad_gateway_with_detail(
            "Fastly failed to register a dynamic backend",
            BadGatewayReason::Unspecified,
            ProviderFailureDetail::Fastly {
                phase: ProviderFailurePhase::BackendRegistration,
                cause: FastlyFailureCause::BackendHostError { status: 1 },
            },
        )
    }

    /// The error Fastly reports when a backend registration overruns the
    /// request budget. It carries the registration phase, but it is a timeout.
    fn backend_registration_timeout() -> EdgeError {
        EdgeError::gateway_timeout_with_detail(
            "outbound request deadline expired",
            BudgetSource::PerCallTimeout,
            ProviderFailureDetail::Fastly {
                phase: ProviderFailurePhase::BackendRegistration,
                cause: FastlyFailureCause::BackendHostError { status: 1 },
            },
        )
    }

    /// Compiles one standard provider per `(id, endpoint)`.
    fn launch_plan(providers: &[(&str, &str)]) -> Arc<AuctionPlan> {
        let mut config = planned_config(
            &providers
                .iter()
                .map(|(id, _)| (*id, RoutingMode::AllEligible))
                .collect::<Vec<_>>(),
            false,
        );
        for (id, endpoint) in providers {
            config
                .providers
                .get_mut(&ProviderId::from_str(id).expect("should parse fictional provider ID"))
                .expect("should configure the provider")
                .endpoint = (*endpoint).to_string();
        }
        Arc::new(AuctionPlan::compile(config).expect("should compile the launch-failure plan"))
    }

    fn launch_context<'a>(
        settings: &'a crate::settings::Settings,
        inbound: &'a http::Request<edgezero_core::body::Body>,
        services: &'a RuntimeServices,
    ) -> AuctionContext<'a> {
        AuctionContext {
            settings,
            request: inbound,
            timeout_ms: 777,
            provider_responses: None,
            services,
        }
    }

    /// Each provider with the `error_type` its response carries, if any.
    fn error_types(result: &OrchestrationResult) -> Vec<(&str, Option<serde_json::Value>)> {
        result
            .provider_responses
            .iter()
            .map(|response| {
                (
                    response.provider.as_str(),
                    response.metadata.get("error_type").cloned(),
                )
            })
            .collect()
    }

    /// Bidders that fail to launch, through construction or backend registration.
    mod launch_failure_tests {
        use super::*;

        #[tokio::test]
        async fn all_planned_launch_failures_error_direct_and_surface_split_failure() {
            for failure in LaunchFailure::ALL {
                let clock = TestClock::new();
                let bidders = ScriptedHttpClient::new(&clock);
                failure.script(&bidders, 2);
                let services = build_services_with_outbound_client(&bidders, &clock);
                let orchestrator = AuctionOrchestrator::from_plan(
                    launch_plan(&[("launch-fail", failure.endpoint())]),
                    None,
                );
                let settings = create_test_settings();
                let inbound = http::Request::new(edgezero_core::body::Body::empty());
                let context = launch_context(&settings, &inbound, &services);
                let request = planned_request();

                let error = orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect_err("should fail direct execution when every planned launch fails");
                assert!(
                    error
                        .to_string()
                        .contains("All eligible planned provider requests failed to launch"),
                    "{failure:?} should distinguish launch failure from an ordinary no-bid: {error:?}"
                );

                let DispatchAuctionOutcome::DispatchFailed {
                    provider_responses,
                    fatal_admission_error,
                    ..
                } = orchestrator.dispatch_auction(&request, &context).await
                else {
                    panic!("{failure:?} should surface a split dispatch failure");
                };
                assert!(
                    fatal_admission_error.is_none(),
                    "{failure:?} should carry no fatal admission error"
                );
                assert_eq!(
                    provider_responses
                        .iter()
                        .map(|response| (
                            response.provider.as_str(),
                            response.metadata["error_type"].clone()
                        ))
                        .collect::<Vec<_>>(),
                    vec![("launch-fail", serde_json::json!(ERROR_TYPE_LAUNCH_FAILED))],
                    "{failure:?} should keep one launch failure for the failed provider"
                );
                assert_eq!(
                    bidders.requests().len(),
                    2 * failure.requests_per_auction(),
                    "{failure:?} should send no bid request beyond the failed registrations"
                );
            }
        }

        #[tokio::test]
        async fn launch_failure_plus_prelaunch_timeout_remains_a_completed_auction() {
            let settings = create_test_settings();
            let inbound = http::Request::new(edgezero_core::body::Body::empty());
            for failure in LaunchFailure::ALL {
                let clock = TestClock::new();
                let bidders = ScriptedHttpClient::new(&clock);
                failure.script(&bidders, 1);
                bidders.respond_to(
                    Method::POST,
                    TIMED_OUT_BIDDER,
                    ScriptedReply::error(backend_registration_timeout()),
                );
                let services = build_services_with_outbound_client(&bidders, &clock);
                let orchestrator = AuctionOrchestrator::from_plan(
                    launch_plan(&[
                        ("launch-fail", failure.endpoint()),
                        ("timeout", TIMED_OUT_BIDDER),
                    ]),
                    None,
                );
                let context = launch_context(&settings, &inbound, &services);

                let result = orchestrator
                    .run_auction(&planned_request(), &context)
                    .await
                    .expect("should treat a registration timeout as a completed provider outcome");

                assert_eq!(
                    error_types(&result),
                    vec![
                        (
                            "launch-fail",
                            Some(serde_json::json!(ERROR_TYPE_LAUNCH_FAILED))
                        ),
                        ("timeout", Some(serde_json::json!(ERROR_TYPE_TIMEOUT))),
                    ],
                    "{failure:?} should keep the launch failure beside the timeout"
                );
            }

            let clock = TestClock::new();
            let bidders = ScriptedHttpClient::new(&clock);
            bidders.respond_to(
                Method::POST,
                TIMED_OUT_BIDDER,
                ScriptedReply::error(backend_registration_timeout()),
            );
            let services = build_services_with_outbound_client(&bidders, &clock);
            let orchestrator =
                AuctionOrchestrator::from_plan(launch_plan(&[("timeout", TIMED_OUT_BIDDER)]), None);
            let context = launch_context(&settings, &inbound, &services);

            let result = orchestrator
                .run_auction(&planned_request(), &context)
                .await
                .expect("should complete an auction whose only bidder timed out registering");

            assert_eq!(
                error_types(&result),
                vec![("timeout", Some(serde_json::json!(ERROR_TYPE_TIMEOUT)))],
                "should report the lone registration timeout as a timeout, not a launch failure"
            );
        }

        #[tokio::test]
        async fn provider_launch_failures_error_when_no_requests_launch() {
            let clock = TestClock::new();
            let bidders = ScriptedHttpClient::new(&clock);
            LaunchFailure::Registration.script(&bidders, 1);
            let services = build_services_with_outbound_client(&bidders, &clock);
            let orchestrator = AuctionOrchestrator::from_plan(
                launch_plan(&[
                    ("unbuildable", UNBUILDABLE_BIDDER),
                    ("unregistered", FAILING_BIDDER),
                ]),
                None,
            );
            let settings = create_test_settings();
            let inbound = http::Request::new(edgezero_core::body::Body::empty());
            let context = launch_context(&settings, &inbound, &services);

            let error = orchestrator
                .run_auction(&planned_request(), &context)
                .await
                .expect_err("should fail when every provider launch fails");

            assert!(
                error
                    .to_string()
                    .contains("All eligible planned provider requests failed to launch"),
                "should explain that no provider request launched: {error:?}"
            );
            assert_eq!(
                recorded_uris(&bidders),
                vec![FAILING_BIDDER.to_string()],
                "should send only the request whose registration failed"
            );
        }
    }

    fn planned_prebid_config(
        providers: &[(&str, serde_json::Value, NotificationConfig)],
    ) -> AuctionPlanConfig {
        AuctionPlanConfig {
            timeout_ms: 777,
            providers: providers
                .iter()
                .map(|(id, profile_config, notifications)| {
                    (
                        ProviderId::from_str(id).expect("should parse fictional provider ID"),
                        ProviderConfig {
                            allow_insecure_http: false,
                            protocol: "openrtb-2.6".to_string(),
                            profile: "prebid-server".to_string(),
                            endpoint: format!("https://{id}.example.test/openrtb"),
                            timeout_ms: Some(1_000),
                            routing: RoutingMode::Explicit,
                            notifications: notifications.clone(),
                            profile_config: profile_config.clone(),
                        },
                    )
                })
                .collect(),
            bidders: BTreeMap::new(),
            mediator: None,
            request_signing: None,
        }
    }

    fn planned_aps_config() -> AuctionPlanConfig {
        planned_aps_instances_config(&[(
            "aps-instance",
            serde_json::json!({"account_id": "example-account"}),
            NotificationConfig::default(),
        )])
    }

    fn planned_aps_instances_config(
        providers: &[(&str, serde_json::Value, NotificationConfig)],
    ) -> AuctionPlanConfig {
        AuctionPlanConfig {
            timeout_ms: 777,
            providers: providers
                .iter()
                .map(|(id, profile_config, notifications)| {
                    (
                        ProviderId::from_str(id).expect("should parse fictional provider ID"),
                        ProviderConfig {
                            allow_insecure_http: false,
                            protocol: "openrtb-2.6".to_string(),
                            profile: "aps".to_string(),
                            endpoint: APS_ENDPOINT.to_string(),
                            timeout_ms: Some(1_000),
                            routing: RoutingMode::AllEligible,
                            notifications: notifications.clone(),
                            profile_config: profile_config.clone(),
                        },
                    )
                })
                .collect(),
            bidders: BTreeMap::new(),
            mediator: None,
            request_signing: None,
        }
    }

    fn planned_request() -> AuctionRequest {
        AuctionRequest {
            id: "fictional-auction".to_string(),
            slots: vec![AdSlot {
                id: "fictional-slot".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                }],
                floor_price: Some(1.0),
                targeting: HashMap::new(),
                bidders: HashMap::new(),
            }],
            publisher: PublisherInfo {
                domain: "publisher.example".to_string(),
                page_url: Some("https://publisher.example/article".to_string()),
            },
            user: UserInfo {
                id: None,
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        }
    }

    fn planned_prebid_request() -> AuctionRequest {
        let mut request = planned_request();
        request.slots[0]
            .bidders
            .insert("trustedServer".to_string(), serde_json::json!({}));
        request
    }

    /// Bidders and the mediator through `EdgeZero` batches on the scripted client.
    mod batch_tests {
        use super::*;
        use crate::auction::telemetry::{
            AuctionObservationContext, AuctionSource, AuctionTerminalOutcome, build_auction_events,
        };
        use crate::platform::test_support::{
            build_services_with_outbound_client, noop_services_with_client_ip,
        };
        use crate::test_support::scripted_client::{
            ExpectedTransport, RecordedRequest, ReplyRelease, ScriptedReply,
        };
        use edgezero_core::error::{BudgetSource, EdgeError};
        use edgezero_core::outbound::{
            OutboundCachePolicy, OutboundSendMode, ResponseDecoding, ResponseMode,
        };
        use http::{Method, StatusCode};
        use std::net::{IpAddr, Ipv4Addr};

        const AUCTION_TIMEOUT: Duration = Duration::from_millis(500);
        const FAST_BIDDER: &str = "https://fast.example.test/openrtb";
        const SLOW_BIDDER: &str = "https://slow.example.test/openrtb";
        const MEDIATOR_ENDPOINT: &str = "https://mediator.example.test/mediate";
        const BIDDER_RESPONSE_BYTES: u64 = 1024 * 1024;

        /// Compiles standard providers from `(id, endpoint, timeout)` triples.
        fn batch_plan(providers: &[(&str, &str, Duration)]) -> Arc<AuctionPlan> {
            Arc::new(
                AuctionPlan::compile(AuctionPlanConfig {
                    timeout_ms: duration_ms(AUCTION_TIMEOUT),
                    providers: providers
                        .iter()
                        .map(|(id, endpoint, timeout)| {
                            (
                                ProviderId::from_str(id)
                                    .expect("should parse fictional provider ID"),
                                ProviderConfig {
                                    allow_insecure_http: false,
                                    protocol: "openrtb-2.6".to_string(),
                                    profile: "standard".to_string(),
                                    endpoint: (*endpoint).to_string(),
                                    timeout_ms: Some(duration_ms(*timeout)),
                                    routing: RoutingMode::AllEligible,
                                    notifications: NotificationConfig::default(),
                                    profile_config: serde_json::json!({}),
                                },
                            )
                        })
                        .collect(),
                    bidders: BTreeMap::new(),
                    mediator: None,
                    request_signing: None,
                })
                .expect("should compile the batch plan"),
            )
        }

        fn duration_ms(duration: Duration) -> u32 {
            u32::try_from(duration.as_millis()).expect("should fit the fixture duration in u32")
        }

        /// A standard `OpenRTB` reply with one bid for the fixture slot.
        fn bid_reply(price: f64) -> ScriptedReply {
            ScriptedReply::status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(
                    serde_json::to_vec(&serde_json::json!({
                        "seatbid": [{"seat": "fictional-seat", "bid": [{
                            "id": "fictional-bid",
                            "impid": "fictional-slot",
                            "price": price,
                            "adm": "<div>fictional creative</div>",
                            "w": 300,
                            "h": 250
                        }]}]
                    }))
                    .expect("should serialize the bid reply"),
                )
        }

        fn batch_context<'a>(
            settings: &'a crate::settings::Settings,
            inbound: &'a http::Request<edgezero_core::body::Body>,
            services: &'a RuntimeServices,
            timeout: Duration,
        ) -> AuctionContext<'a> {
            AuctionContext {
                settings,
                request: inbound,
                timeout_ms: duration_ms(timeout),
                provider_responses: None,
                services,
            }
        }

        fn inbound_request() -> http::Request<edgezero_core::body::Body> {
            http::Request::builder()
                .uri("https://publisher.example/article")
                .body(edgezero_core::body::Body::empty())
                .expect("should build the inbound request")
        }

        fn dispatched(outcome: DispatchAuctionOutcome) -> DispatchedAuction {
            match outcome {
                DispatchAuctionOutcome::Dispatched(dispatched) => dispatched,
                DispatchAuctionOutcome::NotStarted => panic!("should dispatch, not skip"),
                DispatchAuctionOutcome::DispatchFailed { .. } => {
                    panic!("should dispatch, not fail")
                }
            }
        }

        fn response_for<'a>(
            result: &'a OrchestrationResult,
            provider: &str,
        ) -> &'a AuctionResponse {
            result
                .provider_responses
                .iter()
                .find(|response| response.provider == provider)
                .unwrap_or_else(|| panic!("should report provider {provider}"))
        }

        fn request_to<'a>(requests: &'a [RecordedRequest], uri: &str) -> &'a RecordedRequest {
            requests
                .iter()
                .find(|request| request.uri == uri)
                .unwrap_or_else(|| panic!("should send a request to {uri}"))
        }

        #[test]
        fn bidders_leave_together_in_one_batch() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            client
                .respond_to(
                    Method::POST,
                    FAST_BIDDER,
                    bid_reply(1.5).after(Duration::from_millis(100)),
                )
                .respond_to(
                    Method::POST,
                    SLOW_BIDDER,
                    bid_reply(2.5).after(Duration::from_millis(200)),
                );
            let services = build_services_with_outbound_client(&client, &clock);
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[
                    ("fast", FAST_BIDDER, Duration::from_secs(1)),
                    ("slow", SLOW_BIDDER, Duration::from_secs(1)),
                ]),
                None,
            );
            let settings = create_test_settings();
            let inbound = inbound_request();
            let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);
            let started = clock.monotonic_clock().now();

            let dispatched = dispatched(
                clock.drive(orchestrator.dispatch_auction(&planned_request(), &context)),
            );
            let sent_at_dispatch = client.requests();
            let result = clock
                .drive(orchestrator.collect_dispatched_auction(dispatched, &services, &context));

            assert_eq!(
                sent_at_dispatch.len(),
                2,
                "should send both bidder requests before any reply arrives"
            );
            let cutoff = started
                .checked_add(AUCTION_TIMEOUT)
                .expect("should compute the auction cutoff");
            for request in &sent_at_dispatch {
                assert_eq!(
                    (
                        request.mode,
                        request.budget.cause,
                        request.budget.deadline.instant()
                    ),
                    (OutboundSendMode::Batch, BudgetSource::BatchCutoff, cutoff),
                    "should send {} in the auction batch, capped by its cutoff",
                    request.uri
                );
            }
            assert_eq!(
                result.provider_responses.len(),
                2,
                "should report both bidders"
            );
            assert_eq!(
                result
                    .get_winning_bid("fictional-slot")
                    .and_then(|bid| bid.price),
                Some(2.5),
                "should rank both bidders' bids"
            );
        }

        #[test]
        fn bid_after_cutoff_becomes_provider_timeout() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            client
                .respond_to(
                    Method::POST,
                    FAST_BIDDER,
                    bid_reply(1.5).after(Duration::from_millis(100)),
                )
                .respond_to(
                    Method::POST,
                    SLOW_BIDDER,
                    bid_reply(9.0).after(Duration::from_millis(600)),
                );
            let services = build_services_with_outbound_client(&client, &clock);
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[
                    ("fast", FAST_BIDDER, Duration::from_secs(1)),
                    ("slow", SLOW_BIDDER, Duration::from_secs(1)),
                ]),
                None,
            );
            let settings = create_test_settings();
            let inbound = inbound_request();
            let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);

            let result = clock
                .drive(orchestrator.run_auction(&planned_request(), &context))
                .expect("should complete the auction at the cutoff");

            let slow = response_for(&result, "slow");
            assert_eq!(
                (slow.status.clone(), slow.metadata.get("error_type")),
                (
                    BidStatus::Error,
                    Some(&serde_json::json!(ERROR_TYPE_TIMEOUT))
                ),
                "should report the bid after the cutoff as a provider timeout"
            );
            assert_eq!(
                slow.response_time_ms, 500,
                "should time the late bidder to the cutoff"
            );
            assert_eq!(
                result
                    .get_winning_bid("fictional-slot")
                    .and_then(|bid| bid.price),
                Some(1.5),
                "should keep only the bid that answered before the cutoff"
            );
        }

        #[test]
        fn mediator_runs_after_batch_with_auction_deadline() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            client
                .respond_to(
                    Method::POST,
                    FAST_BIDDER,
                    bid_reply(1.5).after(Duration::from_millis(100)),
                )
                .respond_to(
                    Method::POST,
                    MEDIATOR_ENDPOINT,
                    ScriptedReply::status(StatusCode::OK)
                        .body(
                            serde_json::to_vec(&serde_json::json!({
                                "seatbid": [{"seat": "fast", "bid": [{
                                    "id": "mediated-bid",
                                    "impid": "fictional-slot",
                                    "price": 1.75,
                                    "adm": "<div>mediated</div>",
                                    "w": 300,
                                    "h": 250,
                                    "crid": "fictional-seat-creative"
                                }]}]
                            }))
                            .expect("should serialize the mediator reply"),
                        )
                        .after(Duration::from_millis(50)),
                );
            let services = build_services_with_outbound_client(&client, &clock);
            let mediator = AdServerMockProvider::new(AdServerMockConfig {
                enabled: true,
                endpoint: MEDIATOR_ENDPOINT.to_string(),
                timeout_ms: 300,
                price_floor: None,
                context_query_params: BTreeMap::new(),
            });
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[("fast", FAST_BIDDER, Duration::from_secs(1))]),
                Some(Arc::new(mediator)),
            );
            let settings = create_test_settings();
            let inbound = inbound_request();
            let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);
            let auction_deadline = clock.monotonic_clock().deadline_after(AUCTION_TIMEOUT);

            let dispatched = dispatched(
                clock.drive(orchestrator.dispatch_auction(&planned_request(), &context)),
            );
            let sent_before_collection = client.requests().len();
            let result = clock
                .drive(orchestrator.collect_dispatched_auction(dispatched, &services, &context));

            assert_eq!(
                sent_before_collection, 1,
                "should not send the mediator request while bidders are in flight"
            );
            let requests = client.requests();
            assert_eq!(
                requests
                    .iter()
                    .map(|request| request.uri.to_string())
                    .collect::<Vec<_>>(),
                vec![FAST_BIDDER.to_string(), MEDIATOR_ENDPOINT.to_string()],
                "should send the mediator after the bidder batch"
            );
            let mediator_request = &requests[1];
            assert_eq!(
                (
                    mediator_request.mode,
                    mediator_request.timeout,
                    mediator_request.deadline.map(|deadline| deadline.instant())
                ),
                (
                    OutboundSendMode::Send,
                    Some(Duration::from_millis(300)),
                    Some(auction_deadline.instant())
                ),
                "should send the mediator with its own timeout and the auction deadline"
            );
            assert_eq!(
                (
                    mediator_request.connect_timeout,
                    mediator_request.cache_policy.clone(),
                    mediator_request.response_decoding,
                    mediator_request.response_mode
                ),
                (
                    Some(Duration::from_secs(1)),
                    OutboundCachePolicy::Bypass,
                    ResponseDecoding::Decode,
                    ResponseMode::Buffered {
                        max_bytes: 2 * 1024 * 1024
                    }
                ),
                "should buffer the mediator response under 2 MiB, decoded, uncached"
            );
            let mediated = result
                .mediator_response
                .as_ref()
                .expect("should keep the mediator response");
            assert_eq!(
                mediated.bids.first().and_then(|bid| bid.price),
                Some(1.75),
                "should parse the mediated bid"
            );
        }

        #[test]
        fn bidder_timeout_is_configured_value_not_remaining_budget() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            client.respond_to(Method::POST, FAST_BIDDER, bid_reply(1.5));
            let services = build_services_with_outbound_client(&client, &clock);
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[("fast", FAST_BIDDER, Duration::from_millis(300))]),
                None,
            );
            let settings = create_test_settings();
            let inbound = inbound_request();
            let auction_budget = Duration::from_millis(200);
            let context = batch_context(&settings, &inbound, &services, auction_budget);
            let cutoff = clock.monotonic_clock().deadline_after(auction_budget);

            clock
                .drive(orchestrator.run_auction(&planned_request(), &context))
                .expect("should complete the auction");

            let requests = client.requests();
            let bidder = request_to(&requests, FAST_BIDDER);
            bidder.assert_transport(ExpectedTransport {
                timeout: Duration::from_millis(300),
                response_mode: ResponseMode::Buffered {
                    max_bytes: BIDDER_RESPONSE_BYTES,
                },
                response_decoding: ResponseDecoding::Decode,
                cache_policy: OutboundCachePolicy::Bypass,
            });
            assert_eq!(
                (bidder.budget.cause, bidder.budget.deadline.instant()),
                (BudgetSource::BatchCutoff, cutoff.instant()),
                "should let the batch cutoff, not the timeout, bound the shorter auction budget"
            );
            let body: serde_json::Value =
                serde_json::from_slice(&bidder.body).expect("should send a JSON bid request");
            assert_eq!(
                body["tmax"],
                serde_json::json!(200),
                "should keep the remaining auction budget in the payload's tmax"
            );
        }

        #[test]
        fn two_auctions_with_different_budgets_record_identical_request_options() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            client
                .respond_to(Method::POST, FAST_BIDDER, bid_reply(1.5))
                .respond_to(Method::POST, FAST_BIDDER, bid_reply(1.5));
            let services = build_services_with_outbound_client(&client, &clock);
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[("fast", FAST_BIDDER, Duration::from_secs(1))]),
                None,
            );
            let settings = create_test_settings();
            let inbound = inbound_request();

            for budget in [Duration::from_millis(450), Duration::from_millis(250)] {
                let context = batch_context(&settings, &inbound, &services, budget);
                clock
                    .drive(orchestrator.run_auction(&planned_request(), &context))
                    .expect("should complete the auction");
            }

            let requests = client.requests();
            assert_eq!(requests.len(), 2, "should send one request per auction");
            let options = |request: &RecordedRequest| {
                (
                    request.method.clone(),
                    request.uri.clone(),
                    request.timeout,
                    request.connect_timeout,
                    request.deadline.map(|deadline| deadline.instant()),
                    request.cache_policy.clone(),
                    request.response_decoding,
                    request.response_mode,
                    request.host_authority.clone(),
                    request.has_host_authority_override,
                )
            };
            assert_eq!(
                options(&requests[0]),
                options(&requests[1]),
                "should send the same backend-identity options whatever the remaining budget"
            );
            assert_ne!(
                requests[0].budget.deadline.instant(),
                requests[1].budget.deadline.instant(),
                "should still cap each auction at its own cutoff"
            );
        }

        #[test]
        fn abandoned_auction_cancels_pending_slots() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            let (held, release): (ScriptedReply, ReplyRelease) = bid_reply(1.5).held();
            client.respond_to(Method::POST, FAST_BIDDER, held);
            let services = build_services_with_outbound_client(&client, &clock);
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[("fast", FAST_BIDDER, Duration::from_secs(1))]),
                None,
            );
            let settings = create_test_settings();
            let inbound = inbound_request();
            let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);

            let dispatched = dispatched(
                clock.drive(orchestrator.dispatch_auction(&planned_request(), &context)),
            );
            let awaited_in_flight = release.is_awaited();
            let (_request, completed, abandoned, _elapsed_ms) = dispatched.abandon();

            assert!(
                awaited_in_flight,
                "should have the bidder request in flight before abandoning"
            );
            assert!(
                completed.is_empty(),
                "should complete no provider before abandoning"
            );
            assert_eq!(
                abandoned
                    .iter()
                    .map(|call| call.provider.as_str())
                    .collect::<Vec<_>>(),
                vec!["fast"],
                "should report the in-flight bidder as abandoned"
            );
            assert!(
                !release.is_awaited(),
                "should cancel the bidder request, so nothing awaits its held reply"
            );
        }

        #[test]
        fn delayed_collection_keeps_slot_elapsed_and_response_time() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            client.respond_to(
                Method::POST,
                FAST_BIDDER,
                bid_reply(1.5).after(Duration::from_millis(100)),
            );
            let services = build_services_with_outbound_client(&client, &clock);
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[("fast", FAST_BIDDER, Duration::from_secs(1))]),
                None,
            );
            let settings = create_test_settings();
            let inbound = inbound_request();
            let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);
            let request = planned_request();

            let dispatched =
                dispatched(clock.drive(orchestrator.dispatch_auction(&request, &context)));
            // The bid arrives at 100 ms, and a slow origin delays collection to 400 ms.
            clock.advance(Duration::from_millis(400));
            let result = clock
                .drive(orchestrator.collect_dispatched_auction(dispatched, &services, &context));

            assert_eq!(
                response_for(&result, "fast").response_time_ms,
                100,
                "should time the bid to its arrival, not its delayed collection"
            );
            let rows = build_auction_events(
                AuctionObservationContext::for_test(
                    AuctionSource::InitialNavigation,
                    "/article",
                    1,
                ),
                AuctionTerminalOutcome::Completed {
                    request: &request,
                    result: &result,
                    delivered_winner_slots: None,
                },
            );
            let provider_times = rows
                .rows()
                .iter()
                .filter(|row| {
                    row.event_kind == "provider_call" && row.provider.as_deref() == Some("fast")
                })
                .map(|row| row.provider_response_time_ms)
                .collect::<Vec<_>>();
            assert!(
                !provider_times.is_empty() && provider_times.iter().all(|time| *time == Some(100)),
                "should report the arrival time in telemetry, got {provider_times:?}"
            );
        }

        #[test]
        fn empty_auction_returns_without_waiting_for_deadline() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            let services = build_services_with_outbound_client(&client, &clock);
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[("fast", FAST_BIDDER, Duration::from_secs(1))]),
                None,
            );
            let settings = create_test_settings();
            let inbound = inbound_request();
            let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);
            // A video-only slot leaves no provider an eligible banner slot.
            let mut request = planned_request();
            request.slots[0].formats[0].media_type = MediaType::Video;
            let started = clock.monotonic_clock().now();

            let direct = clock
                .drive(orchestrator.run_auction(&request, &context))
                .expect("should complete the empty auction");
            let split = clock.drive(async {
                let dispatched =
                    dispatched(orchestrator.dispatch_auction(&request, &context).await);
                orchestrator
                    .collect_dispatched_auction(dispatched, &services, &context)
                    .await
            });

            for result in [&direct, &split] {
                assert!(
                    result.winning_bids.is_empty(),
                    "should return no winning bids"
                );
                assert_eq!(
                    response_for(result, "fast").metadata["routing"]["skipped_no_eligible_slots"],
                    serde_json::json!(true),
                    "should report the filtered provider as skipped"
                );
            }
            assert_eq!(
                clock.monotonic_clock().now(),
                started,
                "should not wait for the auction deadline"
            );
            client.assert_no_requests();
        }

        #[test]
        fn bidder_requests_carry_consent_and_attested_client_ip_headers() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            let endpoint = "https://pbs.example.test/openrtb";
            client.respond_to(
                Method::POST,
                endpoint,
                ScriptedReply::status(StatusCode::NO_CONTENT),
            );
            let attested = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 44));
            let services = RuntimeServices {
                outbound_client: client.http_client(),
                clock: clock.monotonic_clock(),
                ..noop_services_with_client_ip(attested)
            };
            let mut config = planned_prebid_config(&[(
                "pbs",
                serde_json::json!({"consent_forwarding": "both"}),
                NotificationConfig::default(),
            )]);
            config
                .providers
                .values_mut()
                .for_each(|provider| provider.endpoint = endpoint.to_string());
            let orchestrator = AuctionOrchestrator::from_plan(
                Arc::new(AuctionPlan::compile(config).expect("should compile the PBS plan")),
                None,
            );
            let settings = create_test_settings();
            let inbound = http::Request::builder()
                .uri("https://publisher.example/article")
                .header(
                    http::header::COOKIE,
                    "session=private; euconsent-v2=fictional-tcf",
                )
                .header(http::header::USER_AGENT, "Fictional Browser/7")
                .header("x-forwarded-for", "203.0.113.250")
                .body(edgezero_core::body::Body::empty())
                .expect("should build the inbound request");
            let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);

            clock
                .drive(orchestrator.run_auction(&planned_prebid_request(), &context))
                .expect("should complete the auction");

            let requests = client.requests();
            let headers = &request_to(&requests, endpoint).headers;
            assert_eq!(
                (
                    headers.get(http::header::COOKIE),
                    headers.get("x-forwarded-for"),
                    headers.get(http::header::USER_AGENT)
                ),
                (
                    Some(&http::HeaderValue::from_static(
                        "euconsent-v2=fictional-tcf"
                    )),
                    Some(&http::HeaderValue::from_static("192.0.2.44")),
                    Some(&http::HeaderValue::from_static("Fictional Browser/7"))
                ),
                "should send only the consent cookie and the attested client IP, never the inbound XFF"
            );
        }

        fn mediator_with_timeout(timeout: Duration) -> AdServerMockProvider {
            AdServerMockProvider::new(AdServerMockConfig {
                enabled: true,
                endpoint: MEDIATOR_ENDPOINT.to_string(),
                timeout_ms: duration_ms(timeout),
                price_floor: None,
                context_query_params: BTreeMap::new(),
            })
        }

        #[test]
        fn mediator_ends_at_its_own_timeout_or_the_auction_deadline_and_ranks_locally() {
            // The mediator's own timeout ends first in the first case and the
            // auction deadline in the second.
            for (mediator_timeout, bid_delay, cause, ends_at) in [
                (
                    Duration::from_millis(100),
                    Duration::from_millis(50),
                    BudgetSource::PerCallTimeout,
                    Duration::from_millis(150),
                ),
                (
                    Duration::from_secs(1),
                    Duration::from_millis(400),
                    BudgetSource::RequestDeadline,
                    AUCTION_TIMEOUT,
                ),
            ] {
                let clock = TestClock::new();
                let client = ScriptedHttpClient::new(&clock);
                let (mediator_reply, _never_released) =
                    ScriptedReply::status(StatusCode::OK).held();
                client
                    .respond_to(Method::POST, FAST_BIDDER, bid_reply(1.5).after(bid_delay))
                    .respond_to(Method::POST, MEDIATOR_ENDPOINT, mediator_reply);
                let services = build_services_with_outbound_client(&client, &clock);
                let mediator = mediator_with_timeout(mediator_timeout);
                let orchestrator = AuctionOrchestrator::from_plan(
                    batch_plan(&[("fast", FAST_BIDDER, Duration::from_secs(1))]),
                    Some(Arc::new(mediator)),
                );
                let settings = create_test_settings();
                let inbound = inbound_request();
                let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);

                let result = clock
                    .drive(orchestrator.run_auction(&planned_request(), &context))
                    .expect("should complete without the mediator");

                assert!(
                    result.mediator_response.is_none(),
                    "should drop the mediator that never answered"
                );
                assert_eq!(
                    result
                        .get_winning_bid("fictional-slot")
                        .and_then(|bid| bid.price),
                    Some(1.5),
                    "should rank the bidder's bid locally"
                );
                assert_eq!(
                    (
                        request_to(&client.requests(), MEDIATOR_ENDPOINT)
                            .budget
                            .cause,
                        result.total_time_ms
                    ),
                    (cause, duration_ms(ends_at).into()),
                    "should end the mediator at the earlier of its timeout and the auction deadline"
                );
            }
        }

        #[test]
        fn deadline_spent_by_bidders_skips_the_mediator() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            client.respond_to(
                Method::POST,
                FAST_BIDDER,
                bid_reply(1.5).after(Duration::from_millis(600)),
            );
            let services = build_services_with_outbound_client(&client, &clock);
            let mediator = mediator_with_timeout(Duration::from_millis(300));
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[("fast", FAST_BIDDER, Duration::from_secs(1))]),
                Some(Arc::new(mediator)),
            );
            let settings = create_test_settings();
            let inbound = inbound_request();
            let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);

            let result = clock
                .drive(orchestrator.run_auction(&planned_request(), &context))
                .expect("should complete at the auction deadline");

            assert!(
                result.mediator_response.is_none() && result.winning_bids.is_empty(),
                "should return the late bidder's timeout without mediation"
            );
            assert_eq!(
                client
                    .requests()
                    .iter()
                    .map(|request| request.uri.to_string())
                    .collect::<Vec<_>>(),
                vec![FAST_BIDDER.to_string()],
                "should not send the mediator once the auction deadline passed"
            );
        }

        #[test]
        fn batch_failure_marks_unresolved_slots_as_provider_errors() {
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            client
                .respond_to(
                    Method::POST,
                    FAST_BIDDER,
                    bid_reply(1.5).after(Duration::from_millis(100)),
                )
                .respond_to(
                    Method::POST,
                    SLOW_BIDDER,
                    ScriptedReply::failing_batch(EdgeError::internal(std::io::Error::other(
                        "fictional batch driver failure",
                    )))
                    .after(Duration::from_millis(200)),
                );
            let services = build_services_with_outbound_client(&client, &clock);
            let orchestrator = AuctionOrchestrator::from_plan(
                batch_plan(&[
                    ("fast", FAST_BIDDER, Duration::from_secs(1)),
                    ("slow", SLOW_BIDDER, Duration::from_secs(1)),
                ]),
                None,
            );
            let settings = create_test_settings();
            let inbound = inbound_request();
            let context = batch_context(&settings, &inbound, &services, AUCTION_TIMEOUT);

            let result = clock
                .drive(orchestrator.run_auction(&planned_request(), &context))
                .expect("should complete the auction after a batch failure");

            assert_eq!(
                response_for(&result, "fast").bids.len(),
                1,
                "should keep the bid observed before the batch failed"
            );
            let slow = response_for(&result, "slow");
            assert_eq!(
                (slow.status.clone(), slow.metadata.get("error_type")),
                (
                    BidStatus::Error,
                    Some(&serde_json::json!(ERROR_TYPE_TRANSPORT))
                ),
                "should report the slot the failed batch left unresolved as a provider error"
            );
        }
    }

    mod trace_auction_live_tests {
        use super::*;
        use crate::trace::{TraceAuctionCarry, TraceAuctionSource};
        use edgezero_core::body::Body as EdgeBody;
        use http::Request;

        fn captured_request(request: &AuctionRequest) -> (Request<EdgeBody>, TraceAuctionCarry) {
            let carry = TraceAuctionCarry::capture_if_enabled(
                true,
                TraceAuctionSource::AuctionApi,
                &request.slots,
            )
            .expect("should capture the enabled request");
            let mut inbound = Request::new(EdgeBody::empty());
            inbound.extensions_mut().insert(carry.clone());
            (inbound, carry)
        }

        fn evidence(carry: &TraceAuctionCarry) -> serde_json::Value {
            serde_json::to_value(
                carry
                    .transport()
                    .expect("should terminalize the live auction"),
            )
            .expect("should serialize redacted facts")["evidence"]
                .clone()
        }

        #[tokio::test]
        async fn trace_auction_live_launch_failure_survives_the_ordinary_error() {
            for failure in LaunchFailure::ALL {
                let clock = TestClock::new();
                let bidders = ScriptedHttpClient::new(&clock);
                failure.script(&bidders, 1);
                let services = build_services_with_outbound_client(&bidders, &clock);
                let orchestrator = AuctionOrchestrator::from_plan(
                    launch_plan(&[("launch-fail", failure.endpoint())]),
                    None,
                );
                let settings = create_test_settings();
                let request = planned_request();
                let (inbound, carry) = captured_request(&request);
                let context = launch_context(&settings, &inbound, &services);

                let _error = orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect_err("should preserve the ordinary launch failure");

                let value = evidence(&carry);
                assert_eq!(
                    value["terminal_status"], "dispatch_failed",
                    "{failure:?} should preserve the actual dispatch failure"
                );
                assert_eq!(
                    value["terminal_reason"], "provider_execution_failed",
                    "{failure:?} should use the directly observed launch failure"
                );
                assert_eq!(
                    value["provider_calls"][0]["status"], "error",
                    "{failure:?} should retain the failed actual invocation"
                );
                assert!(
                    !value.to_string().contains("launch-fail"),
                    "{failure:?} should exclude provider identities from public facts"
                );
                assert_eq!(
                    bidders.requests().len(),
                    failure.requests_per_auction(),
                    "{failure:?} should send no bid request beyond the failed registration"
                );
            }
        }

        #[tokio::test]
        async fn trace_auction_live_empty_plan_distinguishes_sync_success_from_split_failure() {
            let plan = Arc::new(
                AuctionPlan::compile(planned_config(&[], false))
                    .expect("should compile an empty plan"),
            );
            let orchestrator = AuctionOrchestrator::from_plan(plan, None);
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            let services = build_services_with_outbound_client(&http, &clock);
            let settings = create_test_settings();
            let request = planned_request();
            for split in [false, true] {
                let (inbound, carry) = captured_request(&request);
                let token = carry.token();
                let context = AuctionContext {
                    settings: &settings,
                    request: &inbound,
                    timeout_ms: 777,
                    provider_responses: None,
                    services: &services,
                };
                if split {
                    assert!(
                        matches!(
                            orchestrator.dispatch_auction(&request, &context).await,
                            DispatchAuctionOutcome::NotStarted
                        ),
                        "should preserve the ordinary split outcome"
                    );
                } else {
                    let result = orchestrator
                        .run_auction(&request, &context)
                        .await
                        .expect("should preserve successful empty-plan execution");
                    assert!(
                        result.winning_bids.is_empty(),
                        "should preserve an ordinary zero-bid result"
                    );
                }
                let value = evidence(&carry);
                assert_eq!(
                    value["diagnostic_auction_id"],
                    token.as_str(),
                    "should preserve the pre-dispatch token"
                );
                assert_eq!(
                    value["terminal_status"],
                    if split {
                        "dispatch_failed"
                    } else {
                        "completed"
                    },
                    "should map the observed path semantics"
                );
                assert_eq!(
                    value["provider_calls"],
                    serde_json::json!([]),
                    "should not fabricate a launched provider"
                );
            }
        }

        #[tokio::test]
        async fn trace_auction_live_collection_failure_keeps_launch_order_through_local_fallback() {
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            // The first slot's arrival fails the whole batch driver.
            http.respond_to(
                Method::POST,
                PLANNED_ENDPOINT,
                ScriptedReply::failing_batch(edgezero_core::error::EdgeError::internal(
                    std::io::Error::other("fictional batch driver failure"),
                )),
            );
            respond_planned(&http, 204, Vec::new());
            let services = build_services_with_outbound_client(&http, &clock);
            let plan = Arc::new(
                AuctionPlan::compile(planned_config(
                    &[
                        ("provider-b", RoutingMode::AllEligible),
                        ("provider-a", RoutingMode::AllEligible),
                    ],
                    false,
                ))
                .expect("should compile the ordered plan"),
            );
            let orchestrator = AuctionOrchestrator::from_plan(plan, None);
            let request = planned_request();
            let settings = create_test_settings();
            let (inbound, carry) = captured_request(&request);
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };
            let result = orchestrator
                .run_auction(&request, &context)
                .await
                .expect("should preserve ordinary local fallback");
            assert!(
                result.winning_bids.is_empty(),
                "should preserve ordinary ranking"
            );
            let value = evidence(&carry);
            assert_eq!(
                value["terminal_status"], "execution_failed",
                "should not infer successful collection from an ordinary Ok result"
            );
            assert_eq!(
                value["terminal_reason"], "collection_failed",
                "should record the failure at its source"
            );
            assert_eq!(
                value["provider_calls"][0]["provider_number"], 1,
                "should preserve first launch order"
            );
            assert_eq!(
                value["provider_calls"][1]["provider_number"], 2,
                "should preserve second launch order"
            );
            assert_eq!(
                value["provider_calls"][0]["status"], "error",
                "should retain transport failure facts"
            );
        }

        #[tokio::test]
        async fn trace_auction_live_mediator_no_bid_is_an_actual_ordered_call() {
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            respond_planned(&http, 204, Vec::new());
            let services = build_services_with_outbound_client(&http, &clock);
            let plan = Arc::new(
                AuctionPlan::compile(planned_config(
                    &[("bidder", RoutingMode::AllEligible)],
                    false,
                ))
                .expect("should compile bidder plan"),
            );
            let orchestrator =
                AuctionOrchestrator::from_plan(plan, Some(Arc::new(ImmediateNoBidProvider)));
            let request = planned_request();
            let settings = create_test_settings();
            let (inbound, carry) = captured_request(&request);
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };
            orchestrator
                .run_auction(&request, &context)
                .await
                .expect("should retain ordinary successful zero-bid execution");
            let value = evidence(&carry);
            assert_eq!(
                value["terminal_status"], "completed",
                "should not label ordinary zero bids a failure"
            );
            assert_eq!(
                value["provider_calls"][0]["role"], "bidder",
                "should retain the bidder launch"
            );
            assert_eq!(
                value["provider_calls"][1]["role"], "mediator",
                "should retain the real mediator invocation"
            );
            assert_eq!(
                value["provider_calls"][1]["status"], "no_bid",
                "should retain mediator zero-bid status"
            );
        }
        struct NeverFinishesMediator;

        #[async_trait::async_trait(?Send)]
        impl AuctionProvider for NeverFinishesMediator {
            fn provider_name(&self) -> &str {
                "pending-mediator"
            }
            async fn request_bids(
                &self,
                _request: &AuctionRequest,
                _context: &AuctionContext<'_>,
            ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
                std::future::pending().await
            }
            async fn parse_response(
                &self,
                _response: OutboundResponse,
                _response_time_ms: u64,
            ) -> Result<AuctionResponse, Report<TrustedServerError>> {
                panic!("should never parse an uncompleted mediator invocation");
            }
            fn timeout_ms(&self) -> u32 {
                777
            }
        }

        #[tokio::test]
        async fn trace_auction_live_cancellation_preserves_pending_mediator_after_bidder_collection()
         {
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            respond_planned(&http, 204, Vec::new());
            let services = build_services_with_outbound_client(&http, &clock);
            let plan = Arc::new(
                AuctionPlan::compile(planned_config(
                    &[("bidder", RoutingMode::AllEligible)],
                    false,
                ))
                .expect("should compile the bidder plan"),
            );
            let orchestrator =
                AuctionOrchestrator::from_plan(plan, Some(Arc::new(NeverFinishesMediator)));
            let request = planned_request();
            let settings = create_test_settings();
            let (inbound, carry) = captured_request(&request);
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };
            let mut pending = Box::pin(orchestrator.run_auction(&request, &context));
            let mut task = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(
                std::future::Future::poll(pending.as_mut(), &mut task).is_pending(),
                "should stop at the actual mediator await"
            );
            assert!(
                carry.transport().is_none(),
                "should not invent a terminal result while work is pending"
            );

            drop(pending);

            let facts = evidence(&carry);
            assert_eq!(
                facts["terminal_status"], "abandoned",
                "should finish synchronously when collection is cancelled"
            );
            assert_eq!(
                facts["provider_calls"][0]["status"], "no_bid",
                "should retain the collected bidder result"
            );
            assert_eq!(
                facts["provider_calls"][1]["role"], "mediator",
                "should retain the actual pending mediator launch"
            );
            assert_eq!(
                facts["provider_calls"][1]["status"], "abandoned",
                "should close only still-pending observations"
            );
        }

        #[tokio::test]
        async fn trace_auction_live_mediator_failure_preserves_successful_local_fallback() {
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            respond_planned(&http, 204, Vec::new());
            let services = build_services_with_outbound_client(&http, &clock);
            let plan = Arc::new(
                AuctionPlan::compile(planned_config(
                    &[("bidder", RoutingMode::AllEligible)],
                    false,
                ))
                .expect("should compile the bidder plan"),
            );
            let orchestrator =
                AuctionOrchestrator::from_plan(plan, Some(Arc::new(LaunchFailingProvider)));
            let request = planned_request();
            let settings = create_test_settings();
            let (inbound, carry) = captured_request(&request);
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };

            let result = orchestrator
                .run_auction(&request, &context)
                .await
                .expect("should preserve successful ordinary local fallback");

            assert!(
                result.mediator_response.is_none(),
                "should preserve the ordinary discarded mediator response"
            );
            let facts = evidence(&carry);
            assert_eq!(
                facts["terminal_status"], "completed",
                "should not infer whole-auction failure from provider error"
            );
            assert!(
                facts.get("terminal_reason").is_none(),
                "should omit a reason for completed execution"
            );
            assert_eq!(
                facts["provider_calls"][1]["status"], "error",
                "should preserve the failed mediator invocation before fallback"
            );
        }

        #[tokio::test]
        async fn trace_auction_live_zero_budget_has_no_synthetic_provider_launches() {
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            let services = build_services_with_outbound_client(&http, &clock);
            let plan = Arc::new(
                AuctionPlan::compile(planned_config(
                    &[("bidder", RoutingMode::AllEligible)],
                    false,
                ))
                .expect("should compile the bidder plan"),
            );
            let orchestrator =
                AuctionOrchestrator::from_plan(plan, Some(Arc::new(NeverFinishesMediator)));
            let request = planned_request();
            let settings = create_test_settings();
            let (inbound, carry) = captured_request(&request);
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 0,
                provider_responses: None,
                services: &services,
            };

            orchestrator
                .run_auction(&request, &context)
                .await
                .expect("should preserve the ordinary zero-budget result");

            assert_eq!(
                evidence(&carry)["provider_calls"],
                serde_json::json!([]),
                "should exclude synthetic timeout responses from launched work"
            );
            assert!(
                recorded_uris(&http).is_empty(),
                "should not add network work for trace"
            );
        }
    }

    #[tokio::test]
    async fn disabled_from_plan_is_a_no_work_kill_switch_for_sync_and_split_paths() {
        let plan = Arc::new(
            AuctionPlan::compile(planned_config(
                &[("provider-a", RoutingMode::AllEligible)],
                false,
            ))
            .expect("should compile plan")
            .with_enabled(false),
        );
        let orchestrator = AuctionOrchestrator::from_plan(plan, None);
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        let services = build_services_with_outbound_client(&http, &clock);
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };
        let request = planned_request();

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("disabled auction should complete as no-bid");
        assert!(result.provider_responses.is_empty());
        assert!(result.winning_bids.is_empty());
        assert!(matches!(
            orchestrator.dispatch_auction(&request, &context).await,
            DispatchAuctionOutcome::NotStarted
        ));
        assert!(recorded_uris(&http).is_empty());
    }

    #[tokio::test]
    async fn enabled_empty_plan_is_successful_no_bid_without_dispatch() {
        let plan = Arc::new(
            AuctionPlan::compile(planned_config(&[], false)).expect("should compile empty plan"),
        );
        let orchestrator = AuctionOrchestrator::from_plan(plan, None);
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        let services = build_services_with_outbound_client(&http, &clock);
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };
        let request = planned_request();

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("empty auction should complete as no-bid");
        assert!(result.provider_responses.is_empty());
        assert!(result.winning_bids.is_empty());
        assert!(matches!(
            orchestrator.dispatch_auction(&request, &context).await,
            DispatchAuctionOutcome::NotStarted
        ));
        assert!(recorded_uris(&http).is_empty());
    }

    #[tokio::test]
    async fn all_skipped_from_plan_completes_with_routing_metadata_in_sync_and_split_paths() {
        for split in [false, true] {
            let plan = Arc::new(
                AuctionPlan::compile(planned_config(&[("skipped", RoutingMode::Explicit)], false))
                    .expect("should compile all-skipped plan"),
            );
            let orchestrator = AuctionOrchestrator::from_plan(plan, None);
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            let services = build_services_with_outbound_client(&http, &clock);
            let settings = create_test_settings();
            let inbound = http::Request::new(edgezero_core::body::Body::empty());
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };
            let request = planned_request();

            let result = if split {
                let DispatchAuctionOutcome::Dispatched(dispatched) =
                    orchestrator.dispatch_auction(&request, &context).await
                else {
                    panic!("all-skipped auction should produce a completed dispatch token");
                };
                orchestrator
                    .collect_dispatched_auction(dispatched, &services, &context)
                    .await
            } else {
                orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect("all-skipped auction should complete")
            };

            assert_eq!(result.provider_responses.len(), 1);
            assert_eq!(
                result.provider_responses[0].metadata["routing"]["skipped_no_eligible_slots"],
                true
            );
            assert!(result.metadata.contains_key("routing"));
            assert!(recorded_uris(&http).is_empty());
        }
    }

    struct FailingCountingConfigStore {
        reads: AtomicUsize,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformConfigStore for FailingCountingConfigStore {
        fn get(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<String, Report<PlatformError>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Err(Report::new(PlatformError::ConfigStore))
        }

        async fn put(
            &self,
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn delete(
            &self,
            _store_id: &StoreId,
            _key: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct CountingConfigStore {
        reads: AtomicUsize,
        current_kid: String,
        /// Time each read spends, on the given test clock.
        admission_delay: Option<(TestClock, Duration)>,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformConfigStore for CountingConfigStore {
        fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if let Some((clock, delay)) = &self.admission_delay {
                clock.advance(*delay);
            }
            (key == "current-kid")
                .then(|| self.current_kid.clone())
                .ok_or_else(|| Report::new(PlatformError::ConfigStore))
        }

        async fn put(
            &self,
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn delete(
            &self,
            _store_id: &StoreId,
            _key: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct CountingSecretStore {
        reads: AtomicUsize,
        key: Vec<u8>,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformSecretStore for CountingSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.key.clone())
        }

        async fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn delete(
            &self,
            _store_id: &StoreId,
            _name: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct UnusedSecretStore;

    #[async_trait::async_trait(?Send)]
    impl PlatformSecretStore for UnusedSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            panic!("signing key should not be read after current-kid failure")
        }

        async fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn delete(
            &self,
            _store_id: &StoreId,
            _name: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    // ---------------------------------------------------------------------------
    // Minimal test double for AuctionProvider
    // ---------------------------------------------------------------------------

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
            returned_seat: None,
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
            returned_seat: None,
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

    /// Endpoint the [`CacheRestoringMediator`] sends its mediation request to.
    const CACHE_RESTORING_MEDIATOR: &str = "https://mediator.example.com/mediate";

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for CacheRestoringMediator {
        fn provider_name(&self) -> &str {
            "mediator"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            Ok(ProviderRequestOutcome::ready(
                edgezero_core::outbound::OutboundRequest::post(CACHE_RESTORING_MEDIATOR)
                    .expect("should build the mediator request"),
            ))
        }

        async fn parse_response(
            &self,
            _response: OutboundResponse,
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
            _response: OutboundResponse,
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
    }

    struct ImmediateMediator;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for ImmediateMediator {
        fn provider_name(&self) -> &str {
            "immediate-mediator"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            Ok(ProviderRequestOutcome::Immediate(AuctionResponse::success(
                self.provider_name(),
                vec![mediated_bid(Some(
                    "https://nurl.example/immediate".to_string(),
                ))],
                0,
            )))
        }

        async fn parse_response(
            &self,
            _response: OutboundResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("immediate mediator response should not be parsed");
        }

        fn timeout_ms(&self) -> u32 {
            2000
        }
    }

    fn create_test_settings() -> crate::settings::Settings {
        let settings_str = crate_test_settings_str();
        crate::settings::Settings::from_toml(&settings_str).expect("should parse test settings")
    }

    struct ImmediateNoBidProvider;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for ImmediateNoBidProvider {
        fn provider_name(&self) -> &str {
            "immediate"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            Ok(ProviderRequestOutcome::Immediate(AuctionResponse::no_bid(
                "immediate",
                0,
            )))
        }

        async fn parse_response(
            &self,
            _response: OutboundResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("immediate response should not be parsed");
        }

        fn timeout_ms(&self) -> u32 {
            2000
        }
    }

    struct LaunchFailingProvider;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for LaunchFailingProvider {
        fn provider_name(&self) -> &str {
            "launch-failing"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            Err(Report::new(TrustedServerError::Auction {
                message: "launch failed in test provider".to_string(),
            }))
        }

        async fn parse_response(
            &self,
            _response: OutboundResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            Err(Report::new(TrustedServerError::Auction {
                message: "launch-failing provider should not parse responses".to_string(),
            }))
        }

        fn timeout_ms(&self) -> u32 {
            2000
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
        let orchestrator = empty_orchestrator(false);
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
                returned_seat: None,
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
                returned_seat: None,
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

    #[test]
    fn test_orchestrator_is_enabled() {
        assert!(empty_orchestrator(true).is_enabled());
        assert!(!empty_orchestrator(false).is_enabled());
    }

    #[tokio::test]
    async fn from_plan_standard_provider_runs_direct_and_split_with_correlation_and_metadata() {
        for split in [false, true] {
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            respond_planned(
                &http,
                200,
                serde_json::to_vec(&serde_json::json!({
                    "seatbid": [{"seat": "provider-seat", "bid": [{
                        "id": "provider-bid", "impid": "fictional-slot", "price": 2.0,
                        "adm": "<div>provider</div>", "w": 300, "h": 250
                    }]}]
                }))
                .expect("should serialize provider response"),
            );
            let services = build_services_with_outbound_client(&http, &clock);
            let mut config = planned_config(&[("provider-a", RoutingMode::AllEligible)], false);
            config.bidders.insert(
                "routed-bidder"
                    .parse()
                    .expect("should parse fictional bidder ID"),
                crate::auction::plan::BidderRouteConfig {
                    provider: "provider-a"
                        .parse()
                        .expect("should parse fictional provider ID"),
                },
            );
            let plan = Arc::new(AuctionPlan::compile(config).expect("should compile plan"));
            let orchestrator = AuctionOrchestrator::from_plan(plan, None);
            let mut request = planned_request();
            request.slots[0].bidders.insert(
                "routed-bidder".to_string(),
                serde_json::json!({"placement": 7}),
            );
            request.slots[0].bidders.insert(
                "unknown-private-id".to_string(),
                serde_json::json!({"secret": 9}),
            );
            let settings = create_test_settings();
            let inbound = http::Request::new(edgezero_core::body::Body::empty());
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };

            let result = if split {
                let DispatchAuctionOutcome::Dispatched(dispatched) =
                    orchestrator.dispatch_auction(&request, &context).await
                else {
                    panic!("standard provider should dispatch");
                };
                orchestrator
                    .collect_dispatched_auction(dispatched, &services, &context)
                    .await
            } else {
                orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect("standard provider should run")
            };

            assert_eq!(recorded_uris(&http).len(), 1);
            assert_eq!(result.provider_responses.len(), 1);
            assert_eq!(result.provider_responses[0].provider, "provider-a");
            assert_eq!(result.provider_responses[0].status, BidStatus::Success);
            assert_eq!(
                result.provider_responses[0].metadata["routing"]["unused_bidder_params_count"],
                1
            );
            assert_eq!(result.metadata["routing"]["unroutable_bidder_count"], 1);
            assert_eq!(
                result.winning_bids["fictional-slot"].bid_id.as_deref(),
                Some("provider-bid")
            );
            let metadata =
                serde_json::to_string(&result.metadata).expect("should serialize auction metadata");
            assert!(!metadata.contains("unknown-private-id") && !metadata.contains("secret"));
        }
    }

    #[tokio::test]
    async fn planned_executor_invokes_immediate_mediator_and_applies_floor() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        respond_planned(
            &http,
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "provider-seat", "bid": [{
                    "id": "provider", "impid": "fictional-slot", "price": 2.0,
                    "adm": "<div>provider</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize provider response"),
        );
        let services = build_services_with_outbound_client(&http, &clock);
        let plan = AuctionPlan::compile(planned_config(
            &[("provider-a", RoutingMode::AllEligible)],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = planned_orchestrator(plan, Some(Arc::new(ImmediateMediator)));
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute planned mediation");

        assert_eq!(
            result
                .mediator_response
                .as_ref()
                .map(|response| response.provider.as_str()),
            Some("immediate-mediator")
        );
        assert_eq!(
            result.winning_bids["header-banner"].nurl.as_deref(),
            Some("https://nurl.example/immediate")
        );
        assert!(
            !result.winning_bids.contains_key("fictional-slot"),
            "mediator output owns final selection"
        );
    }

    #[tokio::test]
    async fn planned_executor_mediator_transport_failure_falls_back_locally() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        respond_planned(
            &http,
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "provider-seat", "bid": [{
                    "id": "provider", "impid": "fictional-slot", "price": 2.0,
                    "adm": "<div>provider</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize provider response"),
        );
        http.respond_to(
            Method::POST,
            CACHE_RESTORING_MEDIATOR,
            ScriptedReply::error(edgezero_core::error::EdgeError::bad_gateway(
                "fictional mediator transport failure",
            )),
        );
        let services = build_services_with_outbound_client(&http, &clock);
        let plan = AuctionPlan::compile(planned_config(
            &[("provider-a", RoutingMode::AllEligible)],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = planned_orchestrator(plan, Some(Arc::new(CacheRestoringMediator)));
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should fall back from mediator transport failure");

        assert!(result.mediator_response.is_none());
        assert_eq!(
            result.winning_bids["fictional-slot"].bid_id.as_deref(),
            Some("provider")
        );
    }

    #[tokio::test]
    async fn planned_prebid_stored_intent_filters_wire_demand_and_skips_empty_transports() {
        for inline_providers in 0..=2 {
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            // Providers launch in ID order: APS, then only PBS instances with usable demand.
            respond(&http, APS_ENDPOINT, 204, Vec::new());
            for index in 0..inline_providers {
                respond(
                    &http,
                    if index == 0 {
                        "https://pbs-a.example.test/openrtb"
                    } else {
                        "https://pbs-b.example.test/openrtb"
                    },
                    200,
                    serde_json::to_vec(&serde_json::json!({
                        "seatbid":[{"seat":"example-seat","bid":[
                            {
                                "id":format!("bid-{index}"), "impid":format!("inline-{index}"),
                                "price":2.0, "adm":"<div>example</div>", "w":300, "h":250
                            },
                            {
                                "id":format!("omitted-{index}"), "impid":"fictional-slot",
                                "price":3.0, "adm":"<div>omitted</div>", "w":300, "h":250
                            }
                        ]}]
                    }))
                    .expect("should serialize PBS response"),
                );
            }
            let services = build_services_with_outbound_client(&http, &clock);
            let mut config = planned_prebid_config(&[
                (
                    "pbs-a",
                    serde_json::json!({}),
                    NotificationConfig::default(),
                ),
                (
                    "pbs-b",
                    serde_json::json!({}),
                    NotificationConfig::default(),
                ),
            ]);
            config.providers.extend(planned_aps_config().providers);
            for (bidder, provider) in [("alpha", "pbs-a"), ("beta", "pbs-b")] {
                config.bidders.insert(
                    bidder.parse().expect("should parse bidder"),
                    crate::auction::plan::BidderRouteConfig {
                        provider: provider.parse().expect("should parse provider"),
                    },
                );
            }
            let plan = AuctionPlan::compile(config).expect("should compile plan");
            let orchestrator = planned_orchestrator(plan, None);
            let mut request = planned_request();
            let template = request.slots[0].clone();
            // Both PBS instances must evaluate candidates but omit them after overrides.
            request.slots[0].bidders.insert(
                "trustedServer".to_string(),
                serde_json::json!({
                    "storedRequest":false, "bidderParams":{"alpha":{},"beta":{}}
                }),
            );
            request.slots.push(AdSlot {
                id: "synthetic-no-pbs".to_string(),
                bidders: HashMap::from([(
                    "trustedServer".to_string(),
                    serde_json::json!({"storedRequest":false,"bidderParams":{}}),
                )]),
                ..template.clone()
            });
            for index in 0..inline_providers {
                let bidder = if index == 0 { "alpha" } else { "beta" };
                request.slots.push(AdSlot {
                    id:format!("inline-{index}"),
                    bidders:HashMap::from([("trustedServer".to_string(),serde_json::json!({"storedRequest":false,"bidderParams":{bidder:{"placement":index}}}))]),
                    ..template.clone()
                });
            }
            let settings = create_test_settings();
            let inbound = http::Request::builder()
                .uri("https://example.com/auction")
                .body(edgezero_core::body::Body::empty())
                .expect("should build inbound");
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };
            let result = orchestrator
                .run_auction(&request, &context)
                .await
                .expect("should run auction");
            let bodies = recorded_bodies(&http);
            assert_eq!(
                bodies.len(),
                1 + inline_providers,
                "should never transport an empty PBS request"
            );
            let aps: serde_json::Value =
                serde_json::from_slice(&bodies[0]).expect("should parse APS wire request");
            assert_eq!(
                aps["imp"]
                    .as_array()
                    .expect("should have impressions")
                    .len(),
                2 + inline_providers
            );
            assert_eq!(result.winning_bids.len(), inline_providers);
            assert!(
                result.get_all_bids_for_slot("fictional-slot").is_empty(),
                "should reject a bid for an omitted impression"
            );
            for index in inline_providers..2 {
                let provider_id = if index == 0 { "pbs-a" } else { "pbs-b" };
                let response = result
                    .provider_responses
                    .iter()
                    .find(|response| response.provider == provider_id)
                    .expect("should retain skipped PBS provider response");
                assert_eq!(
                    response.metadata["routing"]["skipped_no_usable_demand"],
                    true
                );
            }
            for index in 0..inline_providers {
                let wire: serde_json::Value = serde_json::from_slice(&bodies[index + 1])
                    .expect("should parse PBS wire request");
                assert_eq!(
                    wire["imp"]
                        .as_array()
                        .expect("should have impressions")
                        .len(),
                    1
                );
                assert_eq!(wire["imp"][0]["id"], format!("inline-{index}"));
                let prebid = &wire["imp"][0]["ext"]["prebid"];
                assert!(prebid.get("storedrequest").is_none());
                let bidder = if index == 0 { "alpha" } else { "beta" };
                assert_eq!(
                    prebid["bidder"],
                    serde_json::json!({bidder:{"placement":index}})
                );
                assert_eq!(
                    result.winning_bids[&format!("inline-{index}")]
                        .bid_id
                        .as_deref(),
                    Some(format!("bid-{index}").as_str())
                );
                let provider_id = if index == 0 { "pbs-a" } else { "pbs-b" };
                let response = result
                    .provider_responses
                    .iter()
                    .find(|response| response.provider == provider_id)
                    .expect("should retain PBS provider response");
                assert_eq!(
                    response.metadata["response_admission"]["rejected_bid_count"],
                    1
                );
                assert_eq!(
                    response.metadata["response_admission"]["rejection_reasons"]["unrequested_impression"],
                    1
                );
            }
        }
    }

    #[tokio::test]
    async fn planned_prebid_instances_preserve_headers_metadata_suppression_and_identity() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        respond(&http, "https://pbs-a.example.test/openrtb", 
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "suppress-exact", "bid": [
                    {"id":"good-a","impid":"fictional-slot","price":1.25,"adm":"<div>a</div>","w":300,"h":250,"nurl":"https://notify.example/win","burl":"https://notify.example/bill","ext":{"prebid":{"cache":{"bids":{"cacheId":"cache-a","url":"https://cache-a.example/cache/path"}}}}},
                    {"id":"bad-a","price":2.0}
                ]}],
                "ext": {"responsetimemillis":{"suppress-exact":4},"errors":{"other":["fictional"]},"warnings":{"other":["warning"]},"debug":{"httpcalls":[]},"prebid":{"bidstatus":{"suppress-exact":[{"bidid":"good-a"}]}}}
            }))
            .expect("should serialize PBS response a"),
        );
        respond(&http, "https://pbs-b.example.test/openrtb", 
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "keep-seat", "bid": [{
                    "id":"good-b","impid":"fictional-slot","price":2.5,"adm":"<div>b</div>","w":300,"h":250,"nurl":"https://notify.example/win","burl":"https://notify.example/bill"
                }]}]
            }))
            .expect("should serialize PBS response b"),
        );
        let services = build_services_with_outbound_client(&http, &clock);
        let notifications = NotificationConfig {
            suppress_all: false,
            suppress_seats: vec!["suppress-exact".to_string()],
        };
        let plan = AuctionPlan::compile(planned_prebid_config(&[
            (
                "pbs-a",
                serde_json::json!({"debug":true,"test_mode":true,"consent_forwarding":"openrtb_only"}),
                notifications,
            ),
            ("pbs-b", serde_json::json!({}), NotificationConfig::default()),
        ]))
        .expect("should compile planned PBS auction");
        let orchestrator = planned_orchestrator(plan, None);
        let request = planned_prebid_request();
        let settings = create_test_settings();
        let inbound = http::Request::builder()
            .uri("https://publisher.example/auction")
            .header(
                http::header::COOKIE,
                "consent=keep; euconsent-v2=drop; other=value",
            )
            .header(http::header::USER_AGENT, "Fictional Browser/7")
            .header(http::header::REFERER, "https://referrer.example/story")
            .header(http::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .header("x-forwarded-for", "203.0.113.250")
            .body(edgezero_core::body::Body::empty())
            .expect("should build inbound request");
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute planned PBS auction");

        assert_eq!(result.provider_responses.len(), 2);
        let first = &result.provider_responses[0];
        assert_eq!(first.provider, "pbs-a");
        assert_eq!(first.bids.len(), 1, "should isolate malformed sibling");
        assert_eq!(
            first.bids[0].returned_seat.as_deref(),
            Some("suppress-exact")
        );
        assert_eq!(first.bids[0].bidder, "suppress-exact");
        assert!(
            first.bids[0].nurl.is_none(),
            "should suppress after normalization"
        );
        assert!(
            first.bids[0].burl.is_none(),
            "should suppress billing notification"
        );
        assert_eq!(first.bids[0].cache_id.as_deref(), Some("cache-a"));
        assert_eq!(first.bids[0].cache_host.as_deref(), Some("cache-a.example"));
        assert_eq!(first.bids[0].cache_path.as_deref(), Some("/cache/path"));
        assert_eq!(first.metadata["responsetimemillis"]["suppress-exact"], 4);
        assert!(first.metadata.contains_key("errors"));
        assert!(first.metadata.contains_key("warnings"));
        assert!(first.metadata.contains_key("debug"));
        assert!(first.metadata.contains_key("bidstatus"));
        let second = &result.provider_responses[1];
        assert_eq!(second.provider, "pbs-b");
        assert_eq!(second.bids[0].returned_seat.as_deref(), Some("keep-seat"));
        assert!(second.bids[0].nurl.is_some());
        assert!(!second.metadata.contains_key("debug"));
        assert!(!second.metadata.contains_key("bidstatus"));

        let headers = recorded_headers(&http);
        assert_eq!(headers.len(), 2);
        for request_headers in &headers {
            assert!(
                request_headers
                    .iter()
                    .any(|(name, value)| name == "user-agent" && value == "Fictional Browser/7")
            );
            assert!(request_headers.iter().any(
                |(name, value)| name == "referer" && value == "https://referrer.example/story"
            ));
            assert!(
                request_headers
                    .iter()
                    .any(|(name, value)| name == "accept-language" && value == "en-US,en;q=0.9")
            );
            assert!(
                request_headers
                    .iter()
                    .all(|(name, _)| name != "x-forwarded-for"),
                "must ignore inbound XFF without attestation"
            );
            assert!(
                request_headers.iter().all(|(name, _)| name != "accept"),
                "planned PBS transport must not add Accept beyond legacy headers"
            );
        }
        let first_cookie = headers[0]
            .iter()
            .find(|(name, _)| name == "cookie")
            .map(|(_, value)| value.as_str());
        assert_eq!(first_cookie, None, "should omit cookies in openrtb_only");
        let second_cookie = headers[1]
            .iter()
            .find(|(name, _)| name == "cookie")
            .map(|(_, value)| value.as_str());
        assert_eq!(second_cookie, Some("euconsent-v2=drop"));
    }

    #[tokio::test]
    async fn planned_prebid_cookie_modes_preserve_body_consent_policy() {
        for (mode, forwards_body, forwards_cookies) in [
            ("both", true, true),
            ("cookies_only", false, true),
            ("openrtb_only", true, false),
        ] {
            for source in [
                crate::consent::ConsentSource::Cookie,
                crate::consent::ConsentSource::PolicyDefault,
            ] {
                for (cookie, allowed_cookie) in [
                    (
                        Some(http::HeaderValue::from_static(
                            "euconsent-v2=fictional-tcf; __gpp=fictional-gpp; __gpp_sid=2,6; us_privacy=1YNN; ts-ec=secret; ts-eids=secret; sharedId=secret; session=secret",
                        )),
                        Some(
                            "euconsent-v2=fictional-tcf; __gpp=fictional-gpp; __gpp_sid=2,6; us_privacy=1YNN",
                        ),
                    ),
                    (
                        Some(
                            http::HeaderValue::from_bytes(
                                b"euconsent-v2=fictional-tcf; session=secret;\xffbroken",
                            )
                            .expect("should build malformed header"),
                        ),
                        None,
                    ),
                    (
                        Some(http::HeaderValue::from_static(
                            "session=secret; ts-ec=secret",
                        )),
                        None,
                    ),
                    (None, None),
                ] {
                    let unreadable_cookie =
                        cookie.as_ref().is_some_and(|value| value.to_str().is_err());
                    let clock = TestClock::new();
                    let http = ScriptedHttpClient::new(&clock);
                    respond(&http, "https://pbs.example.test/openrtb", 204, Vec::new());
                    let services = build_services_with_outbound_client(&http, &clock);
                    let plan = AuctionPlan::compile(planned_prebid_config(&[(
                        "pbs",
                        serde_json::json!({"consent_forwarding":mode}),
                        NotificationConfig::default(),
                    )]))
                    .expect("should compile PBS plan");
                    let orchestrator = planned_orchestrator(plan, None);
                    let mut request = planned_prebid_request();
                    request.user.consent = Some(crate::consent::ConsentContext {
                        source,
                        gdpr_applies: true,
                        raw_tc_string: Some("fictional-tcf".to_string()),
                        raw_gpp_string: Some("fictional-gpp".to_string()),
                        gpp_section_ids: Some(vec![2, 6]),
                        raw_us_privacy: Some("1YNN".to_string()),
                        raw_ac_string: Some("fictional-ac".to_string()),
                        ..Default::default()
                    });
                    let settings = create_test_settings();
                    let mut inbound = http::Request::new(edgezero_core::body::Body::empty());
                    if let Some(cookie) = cookie {
                        inbound.headers_mut().append(http::header::COOKIE, cookie);
                        // A second Cookie field cannot bypass the selected-header policy.
                        inbound.headers_mut().append(
                            http::header::COOKIE,
                            http::HeaderValue::from_static("session=second-secret"),
                        );
                    }
                    let context = AuctionContext {
                        settings: &settings,
                        request: &inbound,
                        timeout_ms: 777,
                        provider_responses: None,
                        services: &services,
                    };

                    orchestrator
                        .run_auction(&request, &context)
                        .await
                        .expect("should run PBS auction");

                    let headers = recorded_header_bytes(&http);
                    assert_eq!(headers.len(), 1, "should dispatch one PBS request");
                    let cookies = headers[0]
                        .iter()
                        .filter(|(name, _)| name.eq_ignore_ascii_case("cookie"))
                        .map(|(_, value)| value.as_slice())
                        .collect::<Vec<_>>();
                    let expected = if forwards_cookies {
                        allowed_cookie
                    } else {
                        None
                    };
                    assert_eq!(
                        cookies,
                        expected
                            .map(|value| vec![value.as_bytes()])
                            .unwrap_or_default(),
                        "should apply {mode} cookie policy"
                    );
                    let bodies = recorded_bodies(&http);
                    let body: serde_json::Value =
                        serde_json::from_slice(&bodies[0]).expect("should parse PBS body");
                    let includes_body =
                        forwards_body || source == crate::consent::ConsentSource::PolicyDefault;
                    let includes_fallback = !includes_body && unreadable_cookie;
                    for (pointer, value) in [
                        ("/user/consent", serde_json::json!("fictional-tcf")),
                        ("/user/ext/consent", serde_json::json!("fictional-tcf")),
                        (
                            "/user/ext/ConsentedProvidersSettings/consented_providers",
                            serde_json::json!("fictional-ac"),
                        ),
                        ("/regs/gdpr", serde_json::json!(1)),
                        ("/regs/gpp", serde_json::json!("fictional-gpp")),
                        ("/regs/gpp_sid", serde_json::json!([2, 6])),
                        ("/regs/us_privacy", serde_json::json!("1YNN")),
                        ("/regs/ext/gdpr", serde_json::json!(1)),
                        ("/regs/ext/gpp", serde_json::json!("fictional-gpp")),
                        ("/regs/ext/gpp_sid", serde_json::json!([2, 6])),
                        ("/regs/ext/us_privacy", serde_json::json!("1YNN")),
                    ] {
                        assert_eq!(
                            body.pointer(pointer),
                            (includes_body
                                || (includes_fallback
                                    && pointer != "/user/ext/ConsentedProvidersSettings/consented_providers"))
                                .then_some(&value),
                            "should preserve {mode} body policy at {pointer} for {source:?}"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn planned_prebid_cookie_consent_survives_lenient_extraction() {
        for mode in ["both", "cookies_only", "openrtb_only"] {
            for (cookie, forwarded, body_fallback) in [
                (
                    "__gpp_sid=2, 6; us_privacy = 1YYN; session=secret",
                    "__gpp_sid=2,6; us_privacy=1YYN",
                    false,
                ),
                (
                    "us_privacy=1YNN; us_privacy=1YYN; session=secret",
                    "us_privacy=1YYN",
                    false,
                ),
                (
                    "us_privacy=1YYN; __gpp=bad\\value; session=secret",
                    "us_privacy=1YYN",
                    true,
                ),
            ] {
                let clock = TestClock::new();
                let http = ScriptedHttpClient::new(&clock);
                respond(&http, "https://pbs.example.test/openrtb", 204, Vec::new());
                let services = build_services_with_outbound_client(&http, &clock);
                let plan = AuctionPlan::compile(planned_prebid_config(&[(
                    "pbs",
                    serde_json::json!({"consent_forwarding":mode}),
                    NotificationConfig::default(),
                )]))
                .expect("should compile PBS plan");
                let orchestrator = planned_orchestrator(plan, None);
                let inbound = http::Request::builder()
                    .header(http::header::COOKIE, cookie)
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build browser request");
                let jar = crate::cookies::handle_request_cookies(&inbound)
                    .expect("should parse browser cookies");
                let signals = crate::consent::extract_consent_signals(jar.as_ref(), &inbound);
                let consent = crate::consent::build_context_from_signals(&signals);
                assert!(
                    consent.us_privacy.as_ref().is_some_and(|value| {
                        value.opt_out_sale == crate::consent::PrivacyFlag::Yes
                    }),
                    "should decode the browser opt-out before dispatch"
                );
                let mut request = planned_prebid_request();
                request.user.consent = Some(consent);
                let settings = create_test_settings();
                let context = AuctionContext {
                    settings: &settings,
                    request: &inbound,
                    timeout_ms: 777,
                    provider_responses: None,
                    services: &services,
                };
                orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect("should dispatch PBS auction");
                let headers = recorded_headers(&http);
                assert_eq!(
                    headers[0]
                        .iter()
                        .find(|(name, _)| name == "cookie")
                        .map(|(_, value)| value.as_str()),
                    (mode != "openrtb_only").then_some(forwarded),
                    "should preserve only the selected consent values for {cookie:?} in {mode}"
                );
                let body: serde_json::Value = serde_json::from_slice(&recorded_bodies(&http)[0])
                    .expect("should parse PBS body");
                let includes_body = mode != "cookies_only";
                assert_eq!(
                    body.pointer("/regs/us_privacy"),
                    includes_body.then_some(&serde_json::json!("1YYN")),
                    "should preserve opt-out through at least one channel"
                );
                if cookie.starts_with("__gpp_sid") {
                    assert_eq!(
                        body.pointer("/regs/gpp_sid"),
                        includes_body.then_some(&serde_json::json!([2, 6])),
                        "should preserve decoded GPP sections"
                    );
                }
                if body_fallback {
                    assert_eq!(
                        body.pointer("/regs/gpp"),
                        Some(&serde_json::json!("bad\\value")),
                        "should carry unsafe cookie framing only as a JSON string"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn planned_prebid_cookies_only_falls_back_only_for_unsafe_signal() {
        for (cookie, forwarded, expected_tc, expected_gpp, expected_sid, expected_us) in [
            (
                r"euconsent-v2=bad\value; __gpp=GPP; __gpp_sid=2,6; us_privacy=1YYN",
                "__gpp=GPP; __gpp_sid=2,6; us_privacy=1YYN",
                Some(r"bad\value"),
                None,
                None,
                None,
            ),
            (
                "euconsent-v2=TC; __gpp=GPP; __gpp_sid=2, 6,; us_privacy=1YYN",
                "euconsent-v2=TC; __gpp=GPP; us_privacy=1YYN",
                None,
                Some("GPP"),
                Some(serde_json::json!([2, 6])),
                None,
            ),
            (
                r"euconsent-v2=TC; __gpp=GPP; __gpp_sid=2,6; us_privacy=bad\value",
                "euconsent-v2=TC; __gpp=GPP; __gpp_sid=2,6",
                None,
                None,
                None,
                Some(r"bad\value"),
            ),
        ] {
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            respond(&http, "https://pbs.example.test/openrtb", 204, Vec::new());
            let services = build_services_with_outbound_client(&http, &clock);
            let plan = AuctionPlan::compile(planned_prebid_config(&[(
                "pbs",
                serde_json::json!({"consent_forwarding":"cookies_only"}),
                NotificationConfig::default(),
            )]))
            .expect("should compile PBS plan");
            let orchestrator = planned_orchestrator(plan, None);
            let inbound = http::Request::builder()
                .header(http::header::COOKIE, cookie)
                .body(edgezero_core::body::Body::empty())
                .expect("should build browser request");
            let jar = crate::cookies::handle_request_cookies(&inbound)
                .expect("should parse browser cookies");
            let signals = crate::consent::extract_consent_signals(jar.as_ref(), &inbound);
            let mut request = planned_prebid_request();
            request.user.consent = Some(crate::consent::build_context_from_signals(&signals));
            let settings = create_test_settings();
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };

            orchestrator
                .run_auction(&request, &context)
                .await
                .expect("should dispatch PBS auction");

            let headers = recorded_headers(&http);
            assert_eq!(headers.len(), 1, "should dispatch exactly one PBS request");
            assert_eq!(
                headers[0]
                    .iter()
                    .find(|(name, _)| name == "cookie")
                    .map(|(_, value)| value.as_str()),
                Some(forwarded),
                "should keep unaffected safe consent cookies"
            );
            let bodies = recorded_bodies(&http);
            let body: serde_json::Value =
                serde_json::from_slice(&bodies[0]).expect("should parse PBS body");
            for path in ["/user/consent", "/user/ext/consent"] {
                assert_eq!(
                    body.pointer(path),
                    expected_tc.map(serde_json::Value::from).as_ref(),
                    "should fall back only for affected TC"
                );
            }
            for prefix in ["/regs", "/regs/ext"] {
                assert_eq!(
                    body.pointer(&format!("{prefix}/gpp")),
                    expected_gpp.map(serde_json::Value::from).as_ref(),
                    "should fall back for the coupled GPP string"
                );
                assert_eq!(
                    body.pointer(&format!("{prefix}/gpp_sid")),
                    expected_sid.as_ref(),
                    "should fall back for coupled GPP sections"
                );
                assert_eq!(
                    body.pointer(&format!("{prefix}/us_privacy")),
                    expected_us.map(serde_json::Value::from).as_ref(),
                    "should fall back only for affected US Privacy"
                );
            }
        }
    }

    #[tokio::test]
    async fn planned_aps_mock_mediation_preserves_three_identities_and_renderer() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        respond(
            &http,
            APS_ENDPOINT,
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "upstream-seat", "bid": [{
                    "id": "aps-bid", "impid": "fictional-slot", "price": 2.0,
                    "w": 300, "h": 250,
                    "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}
                }]}]
            }))
            .expect("should serialize APS response"),
        );
        respond(
            &http,
            "https://mediator.example/mediate",
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "aps-instance", "bid": [{
                    "id": "mediated-aps", "impid": "fictional-slot", "price": 2.0,
                    "adm": "ignored", "w": 300, "h": 250, "crid": "aps-creative"
                }]}]
            }))
            .expect("should serialize mediator response"),
        );
        let services = build_services_with_outbound_client(&http, &clock);
        let plan =
            AuctionPlan::compile(planned_aps_config()).expect("should compile planned APS auction");
        let mediator = AdServerMockProvider::new(AdServerMockConfig {
            enabled: true,
            endpoint: "https://mediator.example/mediate".to_string(),
            timeout_ms: 500,
            ..AdServerMockConfig::default()
        });
        let orchestrator = planned_orchestrator(plan, Some(Arc::new(mediator)));
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should mediate planned APS bid");

        let provider_bid = &result.provider_responses[0].bids[0];
        assert_eq!(result.provider_responses[0].provider, "aps-instance");
        assert_eq!(provider_bid.returned_seat.as_deref(), Some("upstream-seat"));
        assert_eq!(provider_bid.bidder, "aps");
        let winner = &result.winning_bids["fictional-slot"];
        assert_eq!(winner.returned_seat.as_deref(), Some("upstream-seat"));
        assert_eq!(winner.bidder, "aps");
        assert!(winner.renderer.is_some());
        assert!(winner.creative.is_none());
        assert_eq!(
            result
                .mediator_response
                .as_ref()
                .map(|response| response.provider.as_str()),
            Some("adserver_mock")
        );
    }

    #[tokio::test]
    async fn planned_aps_transport_omits_accept_header() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        respond(&http, APS_ENDPOINT, 400, Vec::new());
        let services = build_services_with_outbound_client(&http, &clock);
        let plan =
            AuctionPlan::compile(planned_aps_config()).expect("should compile planned APS auction");
        let orchestrator = planned_orchestrator(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::builder()
            .uri("https://publisher.example/auction")
            .body(edgezero_core::body::Body::empty())
            .expect("should build inbound request");
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute planned APS auction");

        let headers = recorded_headers(&http);
        assert_eq!(headers.len(), 1);
        assert!(
            headers[0].iter().all(|(name, _)| name != "accept"),
            "planned APS transport must not add Accept beyond legacy headers"
        );
    }

    #[tokio::test]
    async fn planned_aps_profile_normalizes_renderer_reduction_and_metadata() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        respond_with_headers(
            &http,
            APS_ENDPOINT,
            200,
            serde_json::to_vec(&serde_json::json!({
                "cur": "USD",
                "seatbid": [
                    {"seat": "returned-seat", "bid": [
                        {"id": "z-high", "impid": "fictional-slot", "price": 2.0, "w": 300, "h": 250,
                         "nurl": "https://notice.example/win", "burl": "https://notice.example/bill",
                         "crid": "fictional-creative", "adomain": ["advertiser.example"],
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}},
                        {"id": "a-high", "impid": "fictional-slot", "price": 2.0, "w": 300, "h": 250,
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}},
                        {"id": "bad-script", "impid": "fictional-slot", "price": 9.0, "w": 300, "h": 250,
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "script"}},
                        {"id": "bad-domain", "impid": "fictional-slot", "price": 8.0, "w": 300, "h": 250,
                         "ext": {"creativeurl": "https://publisher.example/render", "tagtype": "iframe"}},
                        {"id": "bad-credentials", "impid": "fictional-slot", "price": 8.0, "w": 300, "h": 250,
                         "ext": {"creativeurl": "https://user:password@creative.example/render", "tagtype": "iframe"}},
                        {"id": "bad-imp", "impid": "unknown-slot", "price": 8.0, "w": 300, "h": 250,
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}},
                        {"id": "bad-dimensions", "impid": "fictional-slot", "price": 8.0, "w": 320, "h": 50,
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}},
                        {"id": "bad-price", "impid": "fictional-slot", "price": "high", "w": 300, "h": 250,
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}},
                        {"id": "bad-mtype", "impid": "fictional-slot", "price": 8.0, "mtype": 2, "w": 300, "h": 250,
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}},
                        {"id": "bad-tag", "impid": "fictional-slot", "price": 8.0, "w": 300, "h": 250,
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "native"}},
                        {"id": "bad-crid", "impid": "fictional-slot", "price": 8.0, "w": 300, "h": 250,
                         "crid": "x".repeat(1025),
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}},
                        {"impid": "fictional-slot", "price": 8.0, "w": 300, "h": 250,
                         "ext": {"creativeurl": "https://creative.example/render", "tagtype": "iframe"}}
                    ]},
                    {"seat": 7, "bid": "bad-shape"}
                ]
            }))
            .expect("should serialize APS profile response"),
            &[
                ("content-type", "application/json"),
                ("authorization", "fictional-secret"),
            ],
        );
        let services = build_services_with_outbound_client(&http, &clock);
        let plan = AuctionPlan::compile(planned_aps_instances_config(&[(
            "aps-instance",
            serde_json::json!({"account_id": "example-account", "debug": true}),
            NotificationConfig {
                suppress_all: false,
                suppress_seats: vec!["returned-seat".to_string()],
            },
        )]))
        .expect("should compile planned APS profile");
        let orchestrator = planned_orchestrator(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute planned APS profile");

        let response = &result.provider_responses[0];
        assert_eq!(response.provider, "aps-instance");
        assert_eq!(response.status, BidStatus::Success);
        assert_eq!(
            response.bids.len(),
            1,
            "should retain one bid per impression"
        );
        let bid = &response.bids[0];
        assert_eq!(bid.bidder, "aps");
        assert_eq!(bid.returned_seat.as_deref(), Some("returned-seat"));
        assert_eq!(
            bid.bid_id.as_deref(),
            Some("a-high"),
            "lexical ID should break equal-price tie"
        );
        assert!(bid.creative.is_none());
        assert!(
            bid.nurl.is_none() && bid.burl.is_none(),
            "APS must discard notification URLs"
        );
        let renderer = bid
            .renderer
            .as_ref()
            .and_then(BidRenderer::as_aps)
            .expect("should construct typed APS renderer");
        assert_eq!(renderer.account_id, "example-account");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&renderer.aax_response)
            .expect("should decode minimized APS response");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&decoded)
                .expect("should parse minimized APS response"),
            serde_json::json!({"seatbid":[{"bid":[{
                "id":"a-high","price":2.0,"w":300,"h":250,
                "ext":{"creativeurl":"https://creative.example/render","tagtype":"iframe"}
            }]}]})
        );
        assert_eq!(response.metadata["seatbid_count"], 2);
        assert_eq!(response.metadata["accepted_bid_count"], 1);
        assert_eq!(response.metadata["dropped_bid_count"], 12);
        for reason in [
            "lost_to_higher_bid",
            "script_rendering_disabled",
            "unknown_impid",
            "invalid_dimensions",
            "invalid_price",
            "unsupported_media_type",
            "unsupported_tagtype",
            "creative_id_too_large",
            "missing_render_source",
            "empty_seatbid_bids",
        ] {
            assert_eq!(response.metadata["drop_reasons"][reason], 1, "{reason}");
        }
        assert_eq!(
            response.metadata["drop_reasons"]["invalid_creative_url"], 2,
            "same-publisher and credentialed URLs should both be rejected"
        );
        assert_eq!(
            response.metadata["routing"]["unused_bidder_params_count"],
            0
        );
        let debug = &response.metadata["debug"]["httpcalls"]["aps"][0];
        assert_eq!(debug["uri"], "https://aps.example/e/pb/bid");
        assert_eq!(
            debug["responseheaders"],
            serde_json::json!({"content-type": ["application/json"]}),
            "async stub should preserve allowlisted response headers"
        );
        assert!(
            debug["requestbody"]
                .as_str()
                .is_some_and(|body| body.contains("example-account"))
        );
        assert!(debug["requestheaders"].get("authorization").is_none());
        assert!(debug["responseheaders"].get("authorization").is_none());
    }

    #[tokio::test]
    async fn two_planned_aps_instances_correlate_independently() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        for (seat, id, price) in [("seat-a", "bid-a", 1.0), ("seat-b", "bid-b", 2.0)] {
            respond(
                &http,
                APS_ENDPOINT,
                200,
                serde_json::to_vec(&serde_json::json!({"seatbid":[{"seat":seat,"bid":[{
                    "id":id,"impid":"fictional-slot","price":price,"w":300,"h":250,
                    "ext":{"creativeurl":"https://creative.example/render","tagtype":"iframe"}
                }]}]}))
                .expect("should serialize APS instance response"),
            );
        }
        let services = build_services_with_outbound_client(&http, &clock);
        let plan = AuctionPlan::compile(planned_aps_instances_config(&[
            (
                "aps-a",
                serde_json::json!({"account_id":"account-a"}),
                NotificationConfig::default(),
            ),
            (
                "aps-b",
                serde_json::json!({"account_id":"account-b"}),
                NotificationConfig::default(),
            ),
        ]))
        .expect("should compile two APS instances");
        let orchestrator = planned_orchestrator(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute two APS instances");

        assert_eq!(result.provider_responses.len(), 2);
        assert_eq!(result.provider_responses[0].provider, "aps-a");
        assert_eq!(
            result.provider_responses[0].bids[0].bid_id.as_deref(),
            Some("bid-a")
        );
        assert_eq!(result.provider_responses[1].provider, "aps-b");
        assert_eq!(
            result.provider_responses[1].bids[0].bid_id.as_deref(),
            Some("bid-b")
        );
        assert_eq!(recorded_bodies(&http).len(), 2);
        assert_eq!(
            result.winning_bids["fictional-slot"].bid_id.as_deref(),
            Some("bid-b"),
            "global ranking should remain orchestrator-owned"
        );
    }

    #[tokio::test]
    async fn planned_aps_returned_seat_accepts_only_valid_nonempty_strings() {
        let plan = AuctionPlan::compile(planned_aps_config()).expect("should compile APS plan");
        let routed = route_auction(
            planned_request(),
            &http::Request::new(edgezero_core::body::Body::empty()),
            &plan,
            None,
        );
        let provider = GenericOpenRtbProvider::new(plan.providers()[0].clone());
        for (seat, expected) in [
            (serde_json::Value::Null, None),
            (serde_json::json!(7), None),
            (serde_json::json!(""), None),
            (serde_json::json!("exact-seat"), Some("exact-seat")),
        ] {
            let state = provider.parse_state_for_test(routed.inputs()[0].clone());
            let response = outbound_response(
                edgezero_core::http::response_builder()
                    .status(200)
                    .body(edgezero_core::body::Body::from(
                        serde_json::to_vec(&serde_json::json!({"seatbid":[{"seat":seat,"bid":[{
                            "id":"bid","impid":"fictional-slot","price":1.0,"w":300,"h":250,
                            "nurl":"https://notice.example/win","burl":"https://notice.example/bill",
                            "ext":{"creativeurl":"https://creative.example/render","tagtype":"iframe"}
                        }]}]}))
                        .expect("should serialize seat identity response"),
                    ))
                    .expect("should build seat identity response"),
            );
            let parsed = provider
                .parse_response_with_state(response, 4, Some(state.as_ref()))
                .await
                .expect("should parse seat identity response");
            assert_eq!(parsed.bids[0].returned_seat.as_deref(), expected);
            assert!(parsed.bids[0].nurl.is_none() && parsed.bids[0].burl.is_none());
        }
    }

    #[tokio::test]
    async fn planned_aps_response_status_shape_and_currency_matrix() {
        let plan = AuctionPlan::compile(planned_aps_config()).expect("should compile APS plan");
        let routed = route_auction(
            planned_request(),
            &http::Request::new(edgezero_core::body::Body::empty()),
            &plan,
            None,
        );
        let provider = GenericOpenRtbProvider::new(plan.providers()[0].clone());
        let cases = [
            (204, Vec::new(), BidStatus::NoBid, None, None),
            (400, Vec::new(), BidStatus::Error, None, Some("http_status")),
            (
                200,
                b"not-json".to_vec(),
                BidStatus::Error,
                Some("unexpected_response_shape"),
                Some("parse_response"),
            ),
            (
                200,
                b"[]".to_vec(),
                BidStatus::Error,
                Some("unexpected_response_shape"),
                Some("parse_response"),
            ),
            (
                200,
                br#"{"contextual":true}"#.to_vec(),
                BidStatus::Error,
                Some("unexpected_response_shape"),
                Some("parse_response"),
            ),
            (
                200,
                br#"{"cur":"EUR","seatbid":[]}"#.to_vec(),
                BidStatus::NoBid,
                Some("unsupported_currency"),
                None,
            ),
        ];
        for (status, body, expected, reason, error_type) in cases {
            let state = provider.parse_state_for_test(routed.inputs()[0].clone());
            let response = outbound_response(
                edgezero_core::http::response_builder()
                    .status(status)
                    .body(edgezero_core::body::Body::from(body))
                    .expect("should build APS matrix response"),
            );
            let parsed = provider
                .parse_response_with_state(response, 4, Some(state.as_ref()))
                .await
                .expect("should classify APS matrix response");
            assert_eq!(parsed.status, expected, "status {status}");
            if let Some(reason) = reason {
                assert_eq!(
                    parsed.metadata["drop_reasons"][reason], 1,
                    "status {status}"
                );
            }
            if let Some(error_type) = error_type {
                assert_eq!(parsed.metadata["error_type"], error_type, "status {status}");
            }
        }
    }

    #[tokio::test]
    async fn planned_provider_outcome_matrix_has_fixed_count_only_routing_metadata() {
        let standard_plan = AuctionPlan::compile(planned_config(
            &[("standard", RoutingMode::AllEligible)],
            false,
        ))
        .expect("should compile standard plan");
        let prebid_plan = AuctionPlan::compile(planned_prebid_config(&[(
            "pbs",
            serde_json::json!({}),
            NotificationConfig::default(),
        )]))
        .expect("should compile PBS plan");
        let cases = [
            (&standard_plan, 204, Vec::new(), BidStatus::NoBid),
            (&standard_plan, 502, Vec::new(), BidStatus::Error),
            (&standard_plan, 200, b"not-json".to_vec(), BidStatus::Error),
            (
                &standard_plan,
                200,
                br#"{"seatbid":[]}"#.to_vec(),
                BidStatus::NoBid,
            ),
            (&prebid_plan, 204, b"{}".to_vec(), BidStatus::NoBid),
            (&prebid_plan, 502, Vec::new(), BidStatus::Error),
            (&prebid_plan, 200, b"not-json".to_vec(), BidStatus::Error),
            (
                &prebid_plan,
                200,
                br#"{"seatbid":[]}"#.to_vec(),
                BidStatus::NoBid,
            ),
        ];

        for (plan, status, body, expected) in cases {
            let routed = route_auction(
                planned_prebid_request(),
                &http::Request::new(edgezero_core::body::Body::empty()),
                plan,
                None,
            );
            let provider = GenericOpenRtbProvider::new(plan.providers()[0].clone());
            let state = provider.parse_state_for_test(routed.inputs()[0].clone());
            let response = outbound_response(
                edgezero_core::http::response_builder()
                    .status(status)
                    .body(edgezero_core::body::Body::from(body))
                    .expect("should build provider matrix response"),
            );
            let parsed = provider
                .parse_response_with_state(response, 4, Some(state.as_ref()))
                .await
                .expect("should classify provider matrix response");
            assert_eq!(parsed.status, expected, "status {status}");
            if expected == BidStatus::Error {
                let expected_error_type = if (200..300).contains(&status) {
                    "parse_response"
                } else {
                    "http_status"
                };
                assert_eq!(parsed.metadata["error_type"], expected_error_type);
            }
            assert_eq!(
                parsed.metadata["routing"],
                serde_json::json!({"unused_bidder_params_count": 0})
            );
            let serialized = serde_json::to_string(&parsed.metadata["routing"])
                .expect("should serialize routing metadata");
            assert!(!serialized.contains("fictional-provider"));
            assert!(!serialized.contains("fictional-slot"));
        }
    }

    #[tokio::test]
    async fn planned_aps_script_opt_in_matches_shared_renderer_fixture() {
        let plan = AuctionPlan::compile(planned_aps_instances_config(&[(
            "aps-instance",
            serde_json::json!({
                "account_id":"example-account-id",
                "allow_script_creatives":true
            }),
            NotificationConfig::default(),
        )]))
        .expect("should compile script-enabled APS plan");
        let routed = route_auction(
            planned_request(),
            &http::Request::new(edgezero_core::body::Body::empty()),
            &plan,
            None,
        );
        let provider = GenericOpenRtbProvider::new(plan.providers()[0].clone());
        let state = provider.parse_state_for_test(routed.inputs()[0].clone());
        let response = outbound_response(
            edgezero_core::http::response_builder()
                .status(200)
                .body(edgezero_core::body::Body::from(
                    serde_json::to_vec(&serde_json::json!({"seatbid":[{"bid":[{
                        "id":"fictional-selected-bid-id","impid":"fictional-slot","price":1.23,
                        "w":300,"h":250,"crid":"fictional-creative",
                        "ext":{"creativeurl":"https://creative.example/render","tagtype":"iframe"}
                    },{
                        "id":"script-bid","impid":"fictional-slot","price":1.0,
                        "w":300,"h":250,
                        "ext":{"creativeurl":"https://creative.example/script","tagtype":"script"}
                    }]}]}))
                    .expect("should serialize APS renderer fixture response"),
                ))
                .expect("should build APS renderer fixture response"),
        );

        let parsed = provider
            .parse_response_with_state(response, 3, Some(state.as_ref()))
            .await
            .expect("should parse APS renderer fixture response");

        assert_eq!(parsed.status, BidStatus::Success);
        assert_eq!(
            parsed.metadata["drop_reasons"]["lost_to_higher_bid"], 1,
            "enabled script creative should be eligible before reduction"
        );
        let renderer = parsed.bids[0]
            .renderer
            .as_ref()
            .and_then(BidRenderer::as_aps)
            .expect("should construct APS renderer");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&renderer.aax_response)
            .expect("should decode APS fixture envelope");
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../trusted-server-js/lib/test/fixtures/aps-renderer-v1.json"
        ))
        .expect("should parse shared APS renderer fixture");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&decoded)
                .expect("should parse decoded APS renderer"),
            fixture
        );
    }

    #[tokio::test]
    async fn planned_aps_debug_response_headers_are_allowlisted() {
        let plan = AuctionPlan::compile(planned_aps_instances_config(&[(
            "aps-instance",
            serde_json::json!({"account_id":"example-account","debug":true}),
            NotificationConfig::default(),
        )]))
        .expect("should compile debug APS plan");
        let routed = route_auction(
            planned_request(),
            &http::Request::new(edgezero_core::body::Body::empty()),
            &plan,
            None,
        );
        let provider = GenericOpenRtbProvider::new(plan.providers()[0].clone());
        let state = provider.parse_state_for_test(routed.inputs()[0].clone());
        let response = outbound_response(
            edgezero_core::http::response_builder()
                .status(200)
                .header("content-type", "application/json")
                .header("authorization", "fictional-secret")
                .body(edgezero_core::body::Body::from("{}"))
                .expect("should build debug APS response"),
        );

        let parsed = provider
            .parse_response_with_state(response, 3, Some(state.as_ref()))
            .await
            .expect("should parse debug APS response");

        let headers = &parsed.metadata["debug"]["httpcalls"]["aps"][0]["responseheaders"];
        assert_eq!(
            headers,
            &serde_json::json!({"content-type":["application/json"]})
        );
        assert!(headers.get("authorization").is_none());
    }

    #[tokio::test]
    async fn planned_standard_instances_have_independent_results_and_configured_timeouts() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        respond_planned(
            &http,
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "fictional-seat-a", "bid": [{
                    "id": "bid-a", "impid": "fictional-slot", "price": 1.25,
                    "adm": "<div>a</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize response a"),
        );
        respond_planned(
            &http,
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "fictional-seat-b", "bid": [{
                    "id": "bid-b", "impid": "fictional-slot", "price": 2.5,
                    "adm": "<div>b</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize response b"),
        );
        let services = build_services_with_outbound_client(&http, &clock);
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("provider-a", RoutingMode::AllEligible),
                ("provider-b", RoutingMode::AllEligible),
            ],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = planned_orchestrator(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::builder()
            .uri("https://publisher.example/auction")
            .body(edgezero_core::body::Body::empty())
            .expect("should build inbound request");
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute planned auction");

        assert_eq!(orchestrator.provider_count(), 2);
        assert_eq!(result.provider_responses.len(), 2);
        assert_eq!(result.provider_responses[0].provider, "provider-a");
        assert_eq!(
            result.provider_responses[0].bids[0].bid_id.as_deref(),
            Some("bid-a")
        );
        assert_eq!(
            result.provider_responses[0].bids[0]
                .returned_seat
                .as_deref(),
            Some("fictional-seat-a")
        );
        assert_eq!(
            result.provider_responses[0].metadata["routing"]["unused_bidder_params_count"],
            0
        );
        assert_eq!(result.provider_responses[1].provider, "provider-b");
        assert_eq!(
            result.provider_responses[1].bids[0].bid_id.as_deref(),
            Some("bid-b")
        );
        assert_eq!(
            result.provider_responses[1].bids[0]
                .returned_seat
                .as_deref(),
            Some("fictional-seat-b")
        );
        assert_eq!(
            result.provider_responses[1].metadata["routing"]["unused_bidder_params_count"],
            0
        );
        assert_eq!(
            result.winning_bids["fictional-slot"].bid_id.as_deref(),
            Some("bid-b")
        );
        let request_headers = recorded_headers(&http);
        assert_eq!(request_headers.len(), 2);
        for headers in request_headers {
            assert!(
                headers
                    .iter()
                    .any(|(name, value)| name == "accept" && value == "application/json"),
                "standard planned transport should retain its JSON Accept header"
            );
        }
        let request_bodies = recorded_bodies(&http);
        assert_eq!(request_bodies.len(), 2);
        for body in request_bodies {
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("should parse planned request");
            let tmax = value["tmax"].as_u64().expect("should include logical tmax");
            assert!(
                (750..=777).contains(&tmax),
                "logical budget should remain near the auction budget, got {tmax}"
            );
            assert_eq!(value["imp"].as_array().map(Vec::len), Some(1));
        }
        for request in http.requests() {
            assert_eq!(
                request.timeout,
                Some(Duration::from_secs(1)),
                "should send each instance with its own configured timeout"
            );
        }
    }

    #[tokio::test]
    async fn planned_same_profile_rejects_cross_provider_parse_state() {
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("provider-a", RoutingMode::AllEligible),
                ("provider-b", RoutingMode::AllEligible),
            ],
            false,
        ))
        .expect("should compile planned auction");
        let routed = route_auction(
            planned_request(),
            &http::Request::new(edgezero_core::body::Body::empty()),
            &plan,
            None,
        );
        let provider_a = GenericOpenRtbProvider::new(plan.providers()[0].clone());
        let provider_b = GenericOpenRtbProvider::new(plan.providers()[1].clone());
        let parse_state = provider_a.parse_state_for_test(routed.inputs()[0].clone());
        let response = outbound_response(
            edgezero_core::http::response_builder()
                .status(204)
                .body(edgezero_core::body::Body::empty())
                .expect("should build no-content response"),
        );

        let error = provider_b
            .parse_response_with_state(response, 1, Some(parse_state.as_ref()))
            .await
            .expect_err("should reject another provider's parse state");

        assert!(
            error.to_string().contains("owned by provider provider-a"),
            "should identify cross-provider state ownership"
        );
    }

    #[tokio::test]
    async fn planned_prebid_rejects_cross_provider_parse_state() {
        let plan = AuctionPlan::compile(planned_prebid_config(&[
            (
                "pbs-a",
                serde_json::json!({}),
                NotificationConfig::default(),
            ),
            (
                "pbs-b",
                serde_json::json!({}),
                NotificationConfig::default(),
            ),
        ]))
        .expect("should compile planned PBS auction");
        let routed = route_auction(
            planned_prebid_request(),
            &http::Request::new(edgezero_core::body::Body::empty()),
            &plan,
            None,
        );
        let provider_a = GenericOpenRtbProvider::new(plan.providers()[0].clone());
        let provider_b = GenericOpenRtbProvider::new(plan.providers()[1].clone());
        let parse_state = provider_a.parse_state_for_test(routed.inputs()[0].clone());
        let response = outbound_response(
            edgezero_core::http::response_builder()
                .status(200)
                .body(edgezero_core::body::Body::from_bytes(b"{}".as_slice()))
                .expect("should build PBS response"),
        );

        let error = provider_b
            .parse_response_with_state(response, 1, Some(parse_state.as_ref()))
            .await
            .expect_err("should reject another PBS provider's parse state");

        assert!(
            error.to_string().contains("owned by provider pbs-a"),
            "should identify cross-provider PBS state ownership"
        );
    }

    #[tokio::test]
    async fn planned_skipped_provider_has_no_io_and_no_zero_impression_launch() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        respond_planned(&http, 204, Vec::new());
        let services = build_services_with_outbound_client(&http, &clock);
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("eligible", RoutingMode::AllEligible),
                ("skipped", RoutingMode::Explicit),
            ],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = planned_orchestrator(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute eligible provider only");

        assert_eq!(recorded_uris(&http).len(), 1);
        let skipped = result
            .provider_responses
            .iter()
            .find(|response| response.provider == "skipped")
            .expect("should materialize skipped provider");
        assert_eq!(skipped.status, BidStatus::NoBid);
        assert_eq!(
            skipped.metadata["routing"]["skipped_no_eligible_slots"],
            true
        );
        let request_body = &recorded_bodies(&http)[0];
        let request_value: serde_json::Value =
            serde_json::from_slice(request_body).expect("should parse request body");
        assert_eq!(request_value["imp"].as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn planned_transport_and_parse_failures_are_isolated_from_valid_winner_and_floor() {
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        // BTreeMap plan order is alphabetical: below-floor, parse-fail,
        // transport-fail, valid-winner. Queue responses in that exact order.
        respond_planned(
            &http,
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "below-floor", "bid": [{
                    "id": "below", "impid": "fictional-slot", "price": 0.5,
                    "adm": "<div>below</div>", "w": 300, "h": 250
                }, {
                    "id": "below-only", "impid": "below-only-slot", "price": 0.5,
                    "adm": "<div>below only</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize below-floor response"),
        );
        respond_planned(&http, 200, b"not-json".to_vec());
        http.respond_to(
            Method::POST,
            PLANNED_ENDPOINT,
            ScriptedReply::error(edgezero_core::error::EdgeError::bad_gateway(
                "fictional transport failure",
            )),
        );
        respond_planned(
            &http,
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "winner", "bid": [{
                    "id": "winner", "impid": "fictional-slot", "price": 2.0,
                    "adm": "<div>winner</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize winner response"),
        );
        let services = build_services_with_outbound_client(&http, &clock);
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("transport-fail", RoutingMode::AllEligible),
                ("parse-fail", RoutingMode::AllEligible),
                ("below-floor", RoutingMode::AllEligible),
                ("valid-winner", RoutingMode::AllEligible),
            ],
            false,
        ))
        .expect("should compile failure isolation plan");
        let orchestrator = planned_orchestrator(plan, None);
        let mut request = planned_request();
        let mut below_only_slot = request.slots[0].clone();
        below_only_slot.id = "below-only-slot".to_string();
        request.slots.push(below_only_slot);
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should isolate planned provider failures");

        let by_provider = result
            .provider_responses
            .iter()
            .map(|response| (response.provider.as_str(), response))
            .collect::<HashMap<_, _>>();
        let below_floor = by_provider.get("below-floor").unwrap_or_else(|| {
            panic!(
                "should include below-floor response; got {:?}",
                by_provider.keys().collect::<Vec<_>>()
            )
        });
        assert_eq!(below_floor.status, BidStatus::Success);
        assert_eq!(below_floor.bids[0].bid_id.as_deref(), Some("below"));
        assert_eq!(below_floor.bids[0].price, Some(0.5));
        assert_eq!(below_floor.bids[1].bid_id.as_deref(), Some("below-only"));
        assert_eq!(
            by_provider["transport-fail"].metadata["error_type"],
            "transport"
        );
        let parse_failure = by_provider.get("parse-fail").unwrap_or_else(|| {
            panic!(
                "should include parse-fail response; got {:?}",
                by_provider.keys().collect::<Vec<_>>()
            )
        });
        assert_eq!(parse_failure.status, BidStatus::Error);
        assert_eq!(parse_failure.metadata["error_type"], "parse_response");
        assert_eq!(
            parse_failure.metadata["routing"]["unused_bidder_params_count"],
            0
        );
        for response in by_provider.values() {
            assert_eq!(
                response.metadata["routing"]["unused_bidder_params_count"], 0,
                "every materialized planned provider response should have routing count"
            );
        }
        assert_eq!(by_provider["valid-winner"].status, BidStatus::Success);
        assert_eq!(
            result.winning_bids["fictional-slot"].bid_id.as_deref(),
            Some("winner")
        );
        assert!(
            !result.winning_bids.contains_key("below-only-slot"),
            "valid below-floor bid should be discarded when it is the only candidate"
        );
    }

    #[tokio::test]
    async fn planned_routing_count_survives_standard_and_aps_bounded_body_failures() {
        for (profile, provider_id) in [("standard", "standard"), ("aps", "aps-instance")] {
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            let endpoint = if profile == "standard" {
                PLANNED_ENDPOINT
            } else {
                APS_ENDPOINT
            };
            respond(&http, endpoint, 200, vec![b'x'; 1024 * 1024 + 1]);
            let services = build_services_with_outbound_client(&http, &clock);
            let mut config = if profile == "standard" {
                planned_config(&[(provider_id, RoutingMode::AllEligible)], false)
            } else {
                planned_aps_config()
            };
            config.bidders.insert(
                "example-bidder"
                    .parse()
                    .expect("should parse fictional bidder ID"),
                crate::auction::plan::BidderRouteConfig {
                    provider: provider_id
                        .parse()
                        .expect("should parse fictional provider ID"),
                },
            );
            let plan = AuctionPlan::compile(config).expect("should compile bounded-body plan");
            let orchestrator = planned_orchestrator(plan, None);
            let mut request = planned_request();
            request.slots[0].bidders.insert(
                "example-bidder".to_string(),
                serde_json::json!({"private": "value"}),
            );
            let settings = create_test_settings();
            let inbound = http::Request::new(edgezero_core::body::Body::empty());
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };

            let result = orchestrator
                .run_auction(&request, &context)
                .await
                .expect("should materialize bounded-body failure");
            let response = &result.provider_responses[0];
            assert_eq!(response.status, BidStatus::Error, "{profile}");
            assert_eq!(
                response.metadata["routing"]["unused_bidder_params_count"], 1,
                "{profile} bounded-body failure should retain the input-derived count"
            );
            let routing = serde_json::to_string(&response.metadata["routing"])
                .expect("should serialize routing metadata");
            assert!(!routing.contains("example-bidder") && !routing.contains("private"));
        }
    }

    #[tokio::test]
    async fn planned_signer_admission_time_reduces_budget_and_total_time_includes_it() {
        let clock = TestClock::new();
        let config_store = Arc::new(CountingConfigStore {
            reads: AtomicUsize::new(0),
            current_kid: "test-kid".to_string(),
            admission_delay: Some((clock.clone(), Duration::from_millis(50))),
        });
        let secret_store = Arc::new(CountingSecretStore {
            reads: AtomicUsize::new(0),
            key: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7_u8; 32])
                .into_bytes(),
        });
        let http = ScriptedHttpClient::new(&clock);
        respond_planned(&http, 204, Vec::new());
        let services = RuntimeServices::builder()
            .config_store(Arc::clone(&config_store) as Arc<_>)
            .secret_store(Arc::clone(&secret_store) as Arc<_>)
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .outbound_client(http.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(crate::platform::test_support::NoopGeo))
            .auction_telemetry_sink(Arc::new(
                crate::auction::telemetry::NoopAuctionTelemetrySink,
            ))
            .client_info(crate::platform::ClientInfo::default())
            .build();
        let plan = AuctionPlan::compile(planned_config(
            &[("signed", RoutingMode::AllEligible)],
            true,
        ))
        .expect("should compile signed plan");
        let orchestrator = planned_orchestrator(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 200,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute signed planned auction");

        let body: serde_json::Value = serde_json::from_slice(&recorded_bodies(&http)[0])
            .expect("should parse signed request");
        let tmax = body["tmax"].as_u64().expect("should include tmax");
        assert_eq!(
            tmax, 150,
            "signer admission should reduce the logical budget"
        );
        assert_eq!(
            result.total_time_ms, 50,
            "total time should include signer admission"
        );
        assert_eq!(config_store.reads.load(Ordering::Relaxed), 1);
        assert_eq!(secret_store.reads.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn planned_signed_multi_provider_loads_signer_once_and_sends_twice() {
        let config_store = Arc::new(CountingConfigStore {
            reads: AtomicUsize::new(0),
            current_kid: "test-kid".to_string(),
            admission_delay: None,
        });
        let secret_store = Arc::new(CountingSecretStore {
            reads: AtomicUsize::new(0),
            key: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [11_u8; 32])
                .into_bytes(),
        });
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        respond_planned(&http, 204, Vec::new());
        respond_planned(&http, 204, Vec::new());
        let services = RuntimeServices::builder()
            .config_store(Arc::clone(&config_store) as Arc<_>)
            .secret_store(Arc::clone(&secret_store) as Arc<_>)
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .outbound_client(http.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(crate::platform::test_support::NoopGeo))
            .auction_telemetry_sink(Arc::new(
                crate::auction::telemetry::NoopAuctionTelemetrySink,
            ))
            .client_info(crate::platform::ClientInfo::default())
            .build();
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("provider-a", RoutingMode::AllEligible),
                ("provider-b", RoutingMode::AllEligible),
            ],
            true,
        ))
        .expect("should compile signed plan");
        let orchestrator = planned_orchestrator(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute signed multi-provider auction");

        assert_eq!(result.provider_responses.len(), 2);
        assert_eq!(config_store.reads.load(Ordering::Relaxed), 1);
        assert_eq!(secret_store.reads.load(Ordering::Relaxed), 1);
        assert_eq!(recorded_uris(&http).len(), 2);
        for body in recorded_bodies(&http) {
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("should parse signed provider request");
            assert!(
                value["ext"]["trusted_server"]["signature"].is_string(),
                "should sign every request"
            );
        }
    }

    #[tokio::test]
    async fn from_plan_signing_failure_is_fatal_for_direct_and_safe_for_split() {
        for split in [false, true] {
            let config_store = Arc::new(FailingCountingConfigStore {
                reads: AtomicUsize::new(0),
            });
            let clock = TestClock::new();
            let http = ScriptedHttpClient::new(&clock);
            let services = RuntimeServices::builder()
                .config_store(Arc::clone(&config_store) as Arc<_>)
                .secret_store(Arc::new(UnusedSecretStore))
                .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
                .outbound_client(http.http_client())
                .clock(clock.monotonic_clock())
                .geo(Arc::new(crate::platform::test_support::NoopGeo))
                .auction_telemetry_sink(Arc::new(
                    crate::auction::telemetry::NoopAuctionTelemetrySink,
                ))
                .client_info(crate::platform::ClientInfo::default())
                .build();
            let mut config = planned_config(&[("signed", RoutingMode::AllEligible)], true);
            config.bidders.insert(
                "unknown-private-id"
                    .parse()
                    .expect("should parse fictional bidder ID"),
                crate::auction::plan::BidderRouteConfig {
                    provider: "signed"
                        .parse()
                        .expect("should parse fictional provider ID"),
                },
            );
            let plan = Arc::new(AuctionPlan::compile(config).expect("should compile signed plan"));
            let orchestrator = AuctionOrchestrator::from_plan(plan, None);
            let mut request = planned_request();
            request.slots[0].bidders.insert(
                "unroutable-private-id".to_string(),
                serde_json::json!({"secret": 9}),
            );
            let settings = create_test_settings();
            let inbound = http::Request::new(edgezero_core::body::Body::empty());
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                provider_responses: None,
                services: &services,
            };

            if split {
                let DispatchAuctionOutcome::DispatchFailed {
                    provider_responses,
                    fatal_admission_error,
                    metadata,
                    ..
                } = orchestrator.dispatch_auction(&request, &context).await
                else {
                    panic!("split signer failure should be explicit");
                };
                let error = fatal_admission_error.expect("should carry fatal admission error");
                assert!(format!("{error:?}").contains("current-kid"));
                assert_eq!(provider_responses.len(), 1);
                assert_eq!(
                    provider_responses[0].metadata["routing"]["unused_bidder_params_count"],
                    0
                );
                assert_eq!(metadata["routing"]["unroutable_bidder_count"], 1);
                let serialized =
                    serde_json::to_string(&metadata).expect("should serialize routing metadata");
                assert!(
                    !serialized.contains("unroutable-private-id") && !serialized.contains("secret")
                );
            } else {
                let error = orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect_err("direct signer failure should propagate");
                let report = format!("{error:?}");
                assert!(report.contains("Planned auction admission failed"));
                assert!(report.contains("current-kid"));
            }

            assert_eq!(config_store.reads.load(Ordering::Relaxed), 1);
            assert!(recorded_uris(&http).is_empty());
        }
    }

    #[tokio::test]
    async fn planned_signing_failure_reads_store_once_before_backend_or_send() {
        let config_store = Arc::new(FailingCountingConfigStore {
            reads: AtomicUsize::new(0),
        });
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        let services = RuntimeServices::builder()
            .config_store(Arc::clone(&config_store) as Arc<_>)
            .secret_store(Arc::new(UnusedSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .outbound_client(http.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(crate::platform::test_support::NoopGeo))
            .auction_telemetry_sink(Arc::new(
                crate::auction::telemetry::NoopAuctionTelemetrySink,
            ))
            .client_info(crate::platform::ClientInfo::default())
            .build();
        let plan = AuctionPlan::compile(planned_config(
            &[("signed", RoutingMode::AllEligible)],
            true,
        ))
        .expect("should compile signed plan");
        let orchestrator = planned_orchestrator(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let _error = orchestrator
            .run_auction(&request, &context)
            .await
            .expect_err("should fail signer admission");

        assert_eq!(config_store.reads.load(Ordering::Relaxed), 1);
        assert!(recorded_uris(&http).is_empty());
    }

    /// An immediate mediator that counts its launches.
    struct CountingMediator {
        launches: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for CountingMediator {
        fn provider_name(&self) -> &str {
            "counting-mediator"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            self.launches.fetch_add(1, Ordering::Relaxed);
            Ok(ProviderRequestOutcome::Immediate(AuctionResponse::no_bid(
                "counting-mediator",
                0,
            )))
        }

        async fn parse_response(
            &self,
            _response: OutboundResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("should never parse an immediate mediator response");
        }

        fn timeout_ms(&self) -> u32 {
            777
        }
    }

    #[tokio::test]
    async fn from_plan_split_zero_budget_does_no_signer_backend_or_network_work() {
        let config_store = Arc::new(CountingConfigStore {
            reads: AtomicUsize::new(0),
            current_kid: "unused-kid".to_string(),
            admission_delay: None,
        });
        let secret_store = Arc::new(CountingSecretStore {
            reads: AtomicUsize::new(0),
            key: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [13_u8; 32])
                .into_bytes(),
        });
        let clock = TestClock::new();
        let http = ScriptedHttpClient::new(&clock);
        let services = RuntimeServices::builder()
            .config_store(Arc::clone(&config_store) as Arc<_>)
            .secret_store(Arc::clone(&secret_store) as Arc<_>)
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .outbound_client(http.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(crate::platform::test_support::NoopGeo))
            .auction_telemetry_sink(Arc::new(
                crate::auction::telemetry::NoopAuctionTelemetrySink,
            ))
            .client_info(crate::platform::ClientInfo::default())
            .build();
        let plan = Arc::new(
            AuctionPlan::compile(planned_config(
                &[("signed", RoutingMode::AllEligible)],
                true,
            ))
            .expect("should compile signed plan"),
        );
        let mediator_launches = Arc::new(AtomicUsize::new(0));
        let orchestrator = AuctionOrchestrator::from_plan(
            plan,
            Some(Arc::new(CountingMediator {
                launches: Arc::clone(&mediator_launches),
            })),
        );
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 0,
            provider_responses: None,
            services: &services,
        };

        let DispatchAuctionOutcome::Dispatched(dispatched) =
            orchestrator.dispatch_auction(&request, &context).await
        else {
            panic!("zero budget should materialize a completed split dispatch");
        };
        let result = orchestrator
            .collect_dispatched_auction(dispatched, &services, &context)
            .await;

        assert_eq!(result.provider_responses.len(), 1);
        assert_eq!(
            result.provider_responses[0].metadata["error_type"],
            "timeout"
        );
        assert_eq!(config_store.reads.load(Ordering::Relaxed), 0);
        assert_eq!(secret_store.reads.load(Ordering::Relaxed), 0);
        assert!(recorded_uris(&http).is_empty());
        assert_eq!(
            mediator_launches.load(Ordering::Relaxed),
            0,
            "zero budget must not invoke even an immediate mediator"
        );
    }

    #[test]
    fn routing_metadata_is_fixed_count_only_and_saturating() {
        let metadata = super::routing_metadata(
            RoutingDiagnostics::saturated_for_test().unroutable_bidder_count(),
        );
        assert_eq!(
            metadata,
            HashMap::from([(
                "routing".to_string(),
                serde_json::json!({"unroutable_bidder_count": u32::MAX}),
            )])
        );
        assert!(
            !serde_json::to_string(&metadata)
                .expect("should serialize routing metadata")
                .contains("bidder_id"),
            "routing metadata must not expose bidder identifiers"
        );
    }

    #[test]
    fn decoded_aps_bid_competes_directly_by_cpm() {
        let orchestrator = empty_orchestrator(false);
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
        let orchestrator = empty_orchestrator(false);
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
                returned_seat: None,
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
        let orchestrator = empty_orchestrator(false);
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
                returned_seat: None,
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
        let orchestrator = empty_orchestrator(false);
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
                returned_seat: None,
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
