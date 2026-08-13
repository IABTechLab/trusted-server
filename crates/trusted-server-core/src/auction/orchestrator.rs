//! Auction orchestrator for managing multi-provider auctions.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::Request;
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::sync::Arc;
use web_time::Instant;

use crate::error::TrustedServerError;
use crate::platform::{PlatformPendingRequest, RuntimeServices};

#[cfg(test)]
use super::config::AuctionConfig;
use super::openrtb::unused_bidder_params_count;
use super::plan::AuctionPlan;
use super::provider::{
    AuctionProvider, GenericOpenRtbProvider, ProviderParseState, ProviderRequestOutcome,
};
#[cfg(test)]
use super::routing::RoutedAuction;
use super::routing::route_auction;
use super::telemetry::AbandonedProviderCall;
use super::types::{AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus};
use crate::request_signing::RequestSigner;

/// In-flight auction requests dispatched to SSP backends.
///
/// Created by [`AuctionOrchestrator::dispatch_auction`] and consumed by
/// [`AuctionOrchestrator::collect_dispatched_auction`]. Carrying this handle
/// across `pending_origin.wait()` lets origin response and SSP HTTP requests
/// race in Fastly's native layer, enabling TTFB ≈ origin latency rather than
/// TTFB ≈ auction timeout.
pub struct DispatchedAuction {
    pending_requests: Vec<PlatformPendingRequest>,
    backend_to_provider: HashMap<String, ProviderLaunchState>,
    planned_backend_to_provider: HashMap<String, PlannedLaunchState>,
    completed_responses: Vec<AuctionResponse>,
    auction_start: Instant,
    timeout_ms: u32,
    floor_prices: HashMap<String, f64>,
    provider_request_context: Box<Request<EdgeBody>>,
    /// Carried so the mediator call in collect can pass it as the auction request.
    request: AuctionRequest,
    planned_unused_bidder_params: HashMap<String, u32>,
    planned_unroutable_bidder_count: u32,
    planned_provider_order: HashMap<String, usize>,
}

struct ProviderLaunchState {
    provider_name: String,
    started_at: Instant,
    provider: Arc<dyn AuctionProvider>,
    effective_timeout_ms: u32,
    parse_state: Option<ProviderParseState>,
}

/// Outcome of attempting to dispatch split-phase auction provider requests.
#[allow(clippy::large_enum_variant)]
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
            .map(|state| (state.provider_name, state.started_at))
            .chain(
                self.planned_backend_to_provider
                    .into_values()
                    .map(|state| (state.provider.provider_name().to_string(), state.started_at)),
            )
            .map(|(provider_name, started_at)| {
                AbandonedProviderCall::bidder(
                    provider_name,
                    Some(u32::try_from(started_at.elapsed().as_millis()).unwrap_or(u32::MAX)),
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
}

#[cfg(test)]
impl DispatchedAuction {
    pub(crate) fn empty_for_test(request: AuctionRequest, timeout_ms: u32) -> Self {
        Self {
            pending_requests: Vec::new(),
            backend_to_provider: HashMap::new(),
            planned_backend_to_provider: HashMap::new(),
            completed_responses: Vec::new(),
            auction_start: Instant::now(),
            timeout_ms,
            floor_prices: HashMap::new(),
            provider_request_context: Box::new(Request::new(EdgeBody::empty())),
            request,
            planned_unused_bidder_params: HashMap::new(),
            planned_unroutable_bidder_count: 0,
            planned_provider_order: HashMap::new(),
        }
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

fn provider_skipped_response(provider_name: &str) -> AuctionResponse {
    AuctionResponse::no_bid(provider_name, 0).with_metadata(
        "routing",
        serde_json::json!({"skipped_no_eligible_slots": true}),
    )
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

/// Runtime policy for classifying responses that complete after the logical auction budget.
///
/// Current adapters do not expose an enforceable total-request deadline. They
/// therefore drain already-launched work and accept completed late responses.
#[derive(Debug, Clone, Copy, Default)]
struct AuctionDeadlinePolicy {
    enforceable_total_request_deadline: bool,
}

impl AuctionDeadlinePolicy {
    fn rejects_late_completion(self, start: Instant, timeout_ms: u32) -> bool {
        self.enforceable_total_request_deadline && remaining_budget_ms(start, timeout_ms) == 0
    }

    fn for_runtime(services: &RuntimeServices) -> Self {
        Self {
            enforceable_total_request_deadline: services
                .http_client()
                .has_enforceable_total_request_deadline(),
        }
    }
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
/// including failures produced before or during parsing. Skipped providers are
/// routed separately and retain their exclusive skipped diagnostic.
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
    enabled: bool,
    plan_backed: bool,
    plan: Arc<AuctionPlan>,
    planned_providers: Vec<Arc<GenericOpenRtbProvider>>,
    mediator: Option<Arc<dyn AuctionProvider>>,
    #[cfg(test)]
    config: AuctionConfig,
    #[cfg(test)]
    providers: HashMap<String, Arc<dyn AuctionProvider>>,
}

/// Test harness for the live plan-backed orchestrator semantics.
#[cfg(test)]
pub(crate) struct AuctionOrchestratorHarness {
    plan: Arc<AuctionPlan>,
    providers: Vec<Arc<GenericOpenRtbProvider>>,
    mediator: Option<Arc<dyn AuctionProvider>>,
}

struct PlannedLaunchState {
    provider: Arc<GenericOpenRtbProvider>,
    started_at: Instant,
    parse_state: Option<ProviderParseState>,
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "test harness exercises plan-backed runtime behavior"
)]
impl AuctionOrchestratorHarness {
    pub(crate) fn new(
        plan: impl Into<Arc<AuctionPlan>>,
        mediator: Option<Arc<dyn AuctionProvider>>,
    ) -> Self {
        let plan = plan.into();
        let providers = plan
            .providers()
            .iter()
            .cloned()
            .map(GenericOpenRtbProvider::new)
            .map(Arc::new)
            .collect();
        Self {
            plan,
            providers,
            mediator,
        }
    }

    pub(crate) fn provider_count(&self) -> usize {
        self.providers.len()
    }

    pub(crate) fn mediator(&self) -> Option<&Arc<dyn AuctionProvider>> {
        self.mediator.as_ref()
    }

    /// Route and execute config-first bidder providers in deterministic order.
    pub(crate) async fn run_auction(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        // Admission, including signer-store reads and routing, consumes the same
        // request-local deadline as provider transport and response collection.
        let auction_start = Instant::now();
        let routed = route_auction(
            request.clone(),
            context.request,
            &self.plan,
            context.services.client_info().client_ip,
        );
        if context.timeout_ms == 0 {
            return self
                .run_routed(request, &routed, context, None, auction_start)
                .await;
        }
        if self.providers.len() > 1 && !context.services.http_client().supports_concurrent_fanout()
        {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "{} auction providers configured, but this platform's HTTP client does not support concurrent fanout",
                    self.providers.len()
                ),
            }));
        }

        // Signing admission deliberately precedes every backend call.
        let signer = self
            .plan
            .signing_enabled()
            .then(|| RequestSigner::from_services(context.services))
            .transpose()?;
        self.run_routed(request, &routed, context, signer.as_ref(), auction_start)
            .await
    }

    async fn run_routed(
        &self,
        original_request: &AuctionRequest,
        routed: &RoutedAuction,
        context: &AuctionContext<'_>,
        signer: Option<&RequestSigner>,
        auction_start: Instant,
    ) -> Result<OrchestrationResult, Report<TrustedServerError>> {
        let mut responses = routed
            .skipped_no_eligible_provider_ids()
            .iter()
            .map(|id| provider_skipped_response(id.as_str()))
            .collect::<Vec<_>>();
        let planned_unused_bidder_params = routed
            .inputs()
            .iter()
            .map(|input| {
                (
                    input.provider_id().as_str().to_string(),
                    unused_bidder_params_count(
                        &self
                            .plan
                            .provider(input.provider_id())
                            .expect("should find routed provider in compiled plan")
                            .profile,
                        input,
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut pending = Vec::new();
        let mut launches = HashMap::new();
        let mut reserved_backend_names = HashSet::new();

        for input in routed.inputs() {
            let Some(provider) = self
                .providers
                .iter()
                .find(|provider| provider.provider_name() == input.provider_id().as_str())
                .cloned()
            else {
                responses.push(provider_launch_failed_response(
                    input.provider_id().as_str(),
                    0,
                ));
                continue;
            };
            let remaining_ms = remaining_budget_ms(auction_start, context.timeout_ms);
            let logical_budget_ms = remaining_ms.min(provider.timeout_ms());
            if logical_budget_ms == 0 {
                responses.push(provider_timeout_response(provider.provider_name(), 0));
                continue;
            }
            let transport_timeout_ms = context
                .services
                .backend()
                .canonicalize_transport_timeout_ms(logical_budget_ms, provider.timeout_ms());
            let started_at = Instant::now();
            match provider
                .request_bids_routed(
                    input,
                    routed,
                    logical_budget_ms,
                    transport_timeout_ms,
                    signer,
                    context.services,
                    &mut reserved_backend_names,
                )
                .await
            {
                Ok(ProviderRequestOutcome::Pending {
                    request: launched,
                    parse_state,
                }) => {
                    let Some(backend_name) = launched.backend_name().map(str::to_string) else {
                        log::warn!(
                            "Planned provider '{}' pending request had no backend name",
                            provider.provider_name()
                        );
                        responses.push(provider_launch_failed_response(
                            provider.provider_name(),
                            started_at.elapsed().as_millis() as u64,
                        ));
                        continue;
                    };
                    match launches.entry(backend_name) {
                        Entry::Vacant(entry) => {
                            entry.insert(PlannedLaunchState {
                                provider,
                                started_at,
                                parse_state,
                            });
                            pending.push(launched);
                        }
                        Entry::Occupied(entry) => {
                            log::warn!(
                                "Planned provider '{}' pending backend '{}' already belongs to another provider",
                                provider.provider_name(),
                                entry.key(),
                            );
                            responses.push(provider_launch_failed_response(
                                provider.provider_name(),
                                started_at.elapsed().as_millis() as u64,
                            ));
                        }
                    }
                }
                Ok(ProviderRequestOutcome::Immediate(response)) => responses.push(response),
                Err(error) => {
                    log::warn!(
                        "Planned provider '{}' failed to launch: {:?}",
                        provider.provider_name(),
                        error
                    );
                    responses.push(provider_launch_failed_response(
                        provider.provider_name(),
                        started_at.elapsed().as_millis() as u64,
                    ));
                }
            }
        }

        while !pending.is_empty() {
            let select_result = match context.services.http_client().select(pending).await {
                Ok(result) => result,
                Err(error) => {
                    log::warn!("Planned provider select failed: {:?}", error);
                    break;
                }
            };
            pending = select_result.remaining;
            match select_result.ready {
                Ok(platform_response) => {
                    let backend_name = platform_response
                        .backend_name
                        .as_deref()
                        .unwrap_or_default()
                        .to_string();
                    if let Some(state) = launches.remove(&backend_name) {
                        let elapsed_ms = state.started_at.elapsed().as_millis() as u64;
                        let deadline_policy = AuctionDeadlinePolicy::for_runtime(context.services);
                        if deadline_policy
                            .rejects_late_completion(auction_start, context.timeout_ms)
                        {
                            responses.push(provider_timeout_response(
                                state.provider.provider_name(),
                                elapsed_ms,
                            ));
                            continue;
                        }
                        match state
                            .provider
                            .parse_response_with_state(
                                platform_response,
                                elapsed_ms,
                                state.parse_state.as_deref(),
                            )
                            .await
                        {
                            Ok(response) => responses.push(response),
                            Err(error) => responses.push(provider_error_response(
                                state.provider.provider_name(),
                                elapsed_ms,
                                ERROR_TYPE_PARSE_RESPONSE,
                                &error,
                            )),
                        }
                    }
                }
                Err(error) => {
                    if let Some(backend_name) = select_result.failed_backend_name
                        && let Some(state) = launches.remove(&backend_name)
                    {
                        let elapsed_ms = state.started_at.elapsed().as_millis() as u64;
                        log::warn!(
                            "Planned provider '{}' transport failed: {:?}",
                            state.provider.provider_name(),
                            error
                        );
                        responses.push(provider_transport_failed_response(
                            state.provider.provider_name(),
                            elapsed_ms,
                        ));
                    }
                }
            }
        }
        for state in launches.into_values() {
            responses.push(provider_timeout_response(
                state.provider.provider_name(),
                state.started_at.elapsed().as_millis() as u64,
            ));
        }

        for response in &mut responses {
            if let Some(&unused_bidder_params_count) =
                planned_unused_bidder_params.get(response.provider.as_str())
            {
                *response =
                    materialize_planned_response(response.clone(), unused_bidder_params_count);
            }
        }

        let provider_order = self
            .plan
            .providers()
            .iter()
            .enumerate()
            .map(|(index, provider)| (provider.id.as_str(), index))
            .collect::<HashMap<_, _>>();
        responses.sort_by_key(|response| {
            provider_order
                .get(response.provider.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });

        let floor_prices = original_request
            .slots
            .iter()
            .filter_map(|slot| slot.floor_price.map(|floor| (slot.id.clone(), floor)))
            .collect::<HashMap<_, _>>();
        let helper = AuctionOrchestrator::new(AuctionConfig::default());
        let local_winners = || helper.select_winning_bids(&responses, &floor_prices);
        let (mediator_response, winning_bids) = if let Some(mediator) = &self.mediator {
            let remaining_ms = remaining_budget_ms(auction_start, context.timeout_ms);
            let logical_budget_ms = remaining_ms.min(mediator.timeout_ms());
            if logical_budget_ms == 0 {
                log::warn!(
                    "Auction deadline exhausted before planned mediator; using local ranking"
                );
                (None, local_winners())
            } else {
                let transport_timeout_ms = context
                    .services
                    .backend()
                    .canonicalize_transport_timeout_ms(logical_budget_ms, mediator.timeout_ms());
                let mediator_context = AuctionContext {
                    settings: context.settings,
                    request: context.request,
                    timeout_ms: logical_budget_ms,
                    transport_timeout_ms,
                    provider_responses: Some(&responses),
                    services: context.services,
                };
                let mediator_start = Instant::now();
                let mediated = match mediator
                    .request_bids(original_request, &mediator_context)
                    .await
                {
                    Ok(ProviderRequestOutcome::Immediate(response)) => Some(response),
                    Ok(ProviderRequestOutcome::Pending {
                        request: pending,
                        parse_state,
                    }) => match context.services.http_client().wait(pending).await {
                        Ok(platform_response) => {
                            let response_time_ms = mediator_start.elapsed().as_millis() as u64;
                            if AuctionDeadlinePolicy::for_runtime(context.services)
                                .rejects_late_completion(auction_start, context.timeout_ms)
                            {
                                log::warn!(
                                    "Planned mediator '{}' completed after the hard auction deadline; using local ranking ({}ms)",
                                    mediator.provider_name(),
                                    response_time_ms
                                );
                                None
                            } else {
                                mediator
                                    .parse_response_with_context_and_state(
                                        platform_response,
                                        response_time_ms,
                                        original_request,
                                        &mediator_context,
                                        parse_state.as_deref(),
                                    )
                                    .await
                                    .map_err(|error| {
                                        log::warn!(
                                            "Planned mediator '{}' parse failed: {:?}",
                                            mediator.provider_name(),
                                            error
                                        );
                                    })
                                    .ok()
                            }
                        }
                        Err(error) => {
                            log::warn!(
                                "Planned mediator '{}' request failed: {:?}",
                                mediator.provider_name(),
                                error
                            );
                            None
                        }
                    },
                    Err(error) => {
                        log::warn!(
                            "Planned mediator '{}' failed to launch: {:?}",
                            mediator.provider_name(),
                            error
                        );
                        None
                    }
                };
                if let Some(mediated) = mediated {
                    let winners = mediated
                        .bids
                        .iter()
                        .filter_map(|bid| {
                            if bid.price.is_none() {
                                log::warn!(
                                    "Planned mediator returned a bid without a decoded price"
                                );
                                None
                            } else {
                                Some((bid.slot_id.clone(), bid.clone()))
                            }
                        })
                        .collect();
                    (
                        Some(mediated),
                        helper.apply_floor_prices(winners, &floor_prices),
                    )
                } else {
                    (None, local_winners())
                }
            }
        } else {
            (None, local_winners())
        };
        let unroutable_bidder_count = routed.diagnostics().unroutable_bidder_count();
        log::info!(
            "Auction routing diagnostics: unroutable_bidder_count={}",
            unroutable_bidder_count
        );
        Ok(OrchestrationResult {
            provider_responses: responses,
            mediator_response,
            winning_bids,
            total_time_ms: auction_start.elapsed().as_millis() as u64,
            metadata: routing_metadata(unroutable_bidder_count),
        })
    }
}

impl AuctionOrchestrator {
    /// Create a legacy orchestrator for parity tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn new(config: AuctionConfig) -> Self {
        let plan = Arc::new(
            AuctionPlan::compile(super::plan::AuctionPlanConfig {
                timeout_ms: config.timeout_ms,
                providers: std::collections::BTreeMap::new(),
                bidders: std::collections::BTreeMap::new(),
                mediator: None,
                request_signing: None,
            })
            .expect("should compile empty legacy test plan")
            .with_enabled(config.enabled),
        );
        Self {
            enabled: config.enabled,
            plan_backed: false,
            config,
            plan,
            planned_providers: Vec::new(),
            mediator: None,
            providers: HashMap::new(),
        }
    }

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
            plan_backed: true,
            plan,
            planned_providers,
            mediator,
            #[cfg(test)]
            config: AuctionConfig::default(),
            #[cfg(test)]
            providers: HashMap::new(),
        }
    }

    /// Return whether this orchestrator and another plan consumer share the same plan allocation.
    #[must_use]
    pub fn shares_plan(&self, plan: &Arc<AuctionPlan>) -> bool {
        Arc::ptr_eq(&self.plan, plan)
    }

    /// Register an auction provider in the legacy parity harness.
    #[cfg(test)]
    pub(crate) fn register_provider(&mut self, provider: Arc<dyn AuctionProvider>) {
        let name = provider.provider_name().to_string();
        log::info!("Registering auction provider: {}", name);
        self.providers.insert(name, provider);
    }

    /// Get the number of registered providers.
    #[must_use]
    pub fn provider_count(&self) -> usize {
        self.planned_providers.len()
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
            return Ok(OrchestrationResult::no_bid());
        }
        #[cfg(not(test))]
        {
            return match self.dispatch_auction(request, context).await {
                DispatchAuctionOutcome::Dispatched(dispatched) => Ok(self
                    .collect_dispatched_auction(dispatched, context.services, context)
                    .await),
                DispatchAuctionOutcome::DispatchFailed {
                    provider_responses,
                    fatal_admission_error,
                    metadata,
                    elapsed_ms,
                    ..
                } => {
                    if let Some(error) = fatal_admission_error {
                        return Err(error.change_context(TrustedServerError::Auction {
                            message: "Planned auction admission failed".to_string(),
                        }));
                    }
                    Ok(OrchestrationResult {
                        provider_responses,
                        mediator_response: None,
                        winning_bids: HashMap::new(),
                        total_time_ms: elapsed_ms,
                        metadata,
                    })
                }
                DispatchAuctionOutcome::NotStarted => {
                    if self.planned_providers.is_empty() {
                        Ok(OrchestrationResult::no_bid())
                    } else {
                        Err(Report::new(TrustedServerError::Auction {
                            message: "No planned provider request was started".to_string(),
                        }))
                    }
                }
            };
        }
        #[cfg(test)]
        if self.plan_backed {
            return match self.dispatch_auction(request, context).await {
                DispatchAuctionOutcome::Dispatched(dispatched) => Ok(self
                    .collect_dispatched_auction(dispatched, context.services, context)
                    .await),
                DispatchAuctionOutcome::DispatchFailed {
                    provider_responses,
                    fatal_admission_error,
                    metadata,
                    elapsed_ms,
                    ..
                } => {
                    if let Some(error) = fatal_admission_error {
                        return Err(error.change_context(TrustedServerError::Auction {
                            message: "Planned auction admission failed".to_string(),
                        }));
                    }
                    Ok(OrchestrationResult {
                        provider_responses,
                        mediator_response: None,
                        winning_bids: HashMap::new(),
                        total_time_ms: elapsed_ms,
                        metadata,
                    })
                }
                DispatchAuctionOutcome::NotStarted => {
                    if self.planned_providers.is_empty() {
                        Ok(OrchestrationResult::no_bid())
                    } else {
                        Err(Report::new(TrustedServerError::Auction {
                            message: "No planned provider request was started".to_string(),
                        }))
                    }
                }
            };
        }
        #[cfg(test)]
        let start_time = Instant::now();

        // Auto-detect strategy based on mediator configuration.
        #[cfg(test)]
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

        #[cfg(test)]
        log::info!(
            "Running auction with strategy: {} (auto-detected from mediator config)",
            strategy_name
        );

        #[cfg(test)]
        Ok(OrchestrationResult {
            total_time_ms: start_time.elapsed().as_millis() as u64,
            ..result
        })
    }

    /// Run auction with parallel bidding + mediation.
    #[cfg(test)]
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
            // consumed part of it. Canonicalize the transport timeout so the
            // backend name remains stable across equivalent budget values.
            let remaining_ms = remaining_budget_ms(mediation_start, context.timeout_ms);
            let mediator_timeout = context
                .services
                .backend()
                .canonicalize_transport_timeout_ms(remaining_ms, mediator.timeout_ms());

            if mediator_timeout == 0 {
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
                timeout_ms: mediator_timeout,
                transport_timeout_ms: mediator_timeout,
                provider_responses: Some(&provider_responses),
                services: context.services,
            };

            let start_time = Instant::now();
            let mediator_resp = match mediator
                .request_bids(request, &mediator_context)
                .await
                .change_context(TrustedServerError::Auction {
                    message: format!("Mediator {} failed to launch", mediator.provider_name()),
                })? {
                ProviderRequestOutcome::Immediate(response) => response,
                ProviderRequestOutcome::Pending {
                    request: pending,
                    parse_state,
                } => {
                    let platform_resp = mediator_context
                        .services
                        .http_client()
                        .wait(pending)
                        .await
                        .change_context(TrustedServerError::Auction {
                            message: format!(
                                "Mediator {} request failed",
                                mediator.provider_name()
                            ),
                        })?;
                    let response_time_ms = start_time.elapsed().as_millis() as u64;
                    if AuctionDeadlinePolicy::for_runtime(context.services)
                        .rejects_late_completion(mediation_start, context.timeout_ms)
                    {
                        log::warn!(
                            "Mediator '{}' completed after the hard auction deadline; using local ranking ({}ms)",
                            mediator.provider_name(),
                            response_time_ms
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

                    mediator
                        .parse_response_with_context_and_state(
                            platform_resp,
                            response_time_ms,
                            request,
                            &mediator_context,
                            parse_state.as_deref(),
                        )
                        .await
                        .change_context(TrustedServerError::Auction {
                            message: format!("Mediator {} parse failed", mediator.provider_name()),
                        })?
                }
            };

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
    #[cfg(test)]
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
    #[cfg(test)]
    ///
    /// Uses `PlatformHttpClient::select()` to process responses as they
    /// become ready, rather than waiting for each response sequentially.
    async fn run_providers_parallel(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<Vec<AuctionResponse>, Report<TrustedServerError>> {
        let provider_names = self
            .config
            .providers
            .keys()
            .map(super::plan::ProviderId::as_str)
            .collect::<Vec<_>>();

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
        let mut backend_to_provider: HashMap<String, ProviderLaunchState> = HashMap::new();
        let mut pending_requests: Vec<PlatformPendingRequest> = Vec::new();
        let mut responses = Vec::new();
        let mut immediate_response_count = 0usize;

        for provider_name in &provider_names {
            let provider = match self.providers.get(*provider_name) {
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
            // the overall budget. Canonicalizing keeps backend names stable.
            let remaining_ms = remaining_budget_ms(auction_start, context.timeout_ms);
            let effective_timeout = context
                .services
                .backend()
                .canonicalize_transport_timeout_ms(remaining_ms, provider.timeout_ms());

            // The deadline gate intentionally precedes `request_bids`: zero
            // budget skips every provider, including one that might respond immediately.
            if effective_timeout == 0 {
                log::warn!("Auction timeout exhausted before launching provider request; skipping");
                continue;
            }

            // Immediate providers have no backend name and must remain eligible
            // to return a synchronous result. Pending providers are still
            // guarded before dispatch when their name can be predicted.
            let predicted_backend_name = provider.backend_name(context.services, effective_timeout);
            if let Some(backend_name) = predicted_backend_name.as_ref()
                && backend_to_provider.contains_key(backend_name)
            {
                log::warn!(
                    "Provider '{}' predicted backend name '{}' already belongs to another provider; skipping launch",
                    provider.provider_name(),
                    backend_name,
                );
                responses.push(provider_launch_failed_response(provider.provider_name(), 0));
                continue;
            }

            let provider_context = AuctionContext {
                settings: context.settings,
                request: context.request,
                timeout_ms: effective_timeout,
                transport_timeout_ms: effective_timeout,
                provider_responses: context.provider_responses,
                services: context.services,
            };

            log::info!(
                "Launching bid request to '{}' with a {}ms budget",
                provider.provider_name(),
                effective_timeout
            );

            let start_time = Instant::now();
            match provider.request_bids(request, &provider_context).await {
                Ok(ProviderRequestOutcome::Pending {
                    request: pending,
                    parse_state,
                }) => {
                    let request_backend_name = pending.backend_name().map(str::to_string).or_else(|| {
                        if let Some(backend_name) = predicted_backend_name.as_ref() {
                            log::warn!(
                                "Provider '{}' pending request returned no backend name; using predicted name '{}'",
                                provider.provider_name(),
                                backend_name,
                            );
                        }
                        predicted_backend_name.clone()
                    });
                    let Some(request_backend_name) = request_backend_name else {
                        log::warn!(
                            "Provider '{}' pending request has no backend name; response cannot be correlated",
                            provider.provider_name()
                        );
                        responses.push(provider_launch_failed_response(
                            provider.provider_name(),
                            start_time.elapsed().as_millis() as u64,
                        ));
                        continue;
                    };
                    if backend_to_provider.contains_key(&request_backend_name) {
                        log::warn!(
                            "Provider '{}' resolved backend name '{}' already belongs to another provider; skipping launch",
                            provider.provider_name(),
                            request_backend_name,
                        );
                        responses.push(provider_launch_failed_response(
                            provider.provider_name(),
                            start_time.elapsed().as_millis() as u64,
                        ));
                        continue;
                    }
                    backend_to_provider.insert(
                        request_backend_name,
                        ProviderLaunchState {
                            provider_name: provider.provider_name().to_string(),
                            started_at: start_time,
                            provider: Arc::clone(provider),
                            effective_timeout_ms: effective_timeout,
                            parse_state,
                        },
                    );
                    pending_requests.push(pending);
                    log::debug!(
                        "Request to '{}' launched successfully",
                        provider.provider_name()
                    );
                }
                Ok(ProviderRequestOutcome::Immediate(response)) => {
                    immediate_response_count += 1;
                    log::debug!(
                        "Provider '{}' completed without an upstream request",
                        provider.provider_name()
                    );
                    responses.push(response);
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
            // An immediate response (for example, an APS-only Prebid no-bid) is
            // a completed provider outcome. Launch failures alone remain a
            // terminal auction error rather than being converted to a 200 no-bid.
            if immediate_response_count > 0 {
                return Ok(responses);
            }
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "All {} configured provider(s) skipped or failed to launch",
                    provider_names.len()
                ),
            }));
        }

        let deadline_policy = AuctionDeadlinePolicy::for_runtime(context.services);
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
        // Backend first-byte and between-bytes timeouts are capped to the
        // remaining auction budget in Phase 1. They are transport timers, not
        // absolute wall-clock limits, so connection setup and byte-trickling
        // remain bounded operational risks rather than strict deadline proof.
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

                    if let Some(state) = backend_to_provider.remove(&backend_name) {
                        let response_time_ms = state.started_at.elapsed().as_millis() as u64;
                        if deadline_policy
                            .rejects_late_completion(auction_start, context.timeout_ms)
                        {
                            responses.push(provider_timeout_response(
                                &state.provider_name,
                                response_time_ms,
                            ));
                            continue;
                        }
                        let provider_context = AuctionContext {
                            settings: context.settings,
                            request: context.request,
                            timeout_ms: state.effective_timeout_ms,
                            transport_timeout_ms: state.effective_timeout_ms,
                            provider_responses: context.provider_responses,
                            services: context.services,
                        };

                        match state
                            .provider
                            .parse_response_with_context_and_state(
                                response,
                                response_time_ms,
                                request,
                                &provider_context,
                                state.parse_state.as_deref(),
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
                                    state.provider_name,
                                    e
                                );
                                responses.push(provider_error_response(
                                    &state.provider_name,
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
                        if let Some(state) = backend_to_provider.remove(backend_name) {
                            let response_time_ms = state.started_at.elapsed().as_millis() as u64;
                            log::warn!(
                                "Provider '{}' request failed: {:?}",
                                state.provider_name,
                                e
                            );
                            responses.push(provider_transport_failed_response(
                                &state.provider_name,
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

            // Current adapters cannot enforce a hard total-request deadline, so
            // drain already-launched handles and retain completed late responses.
            // A future adapter that explicitly claims the capability classifies
            // each late completion as a timeout instead.
        }

        for state in backend_to_provider.into_values() {
            let response_time_ms = state.started_at.elapsed().as_millis() as u64;
            log::warn!(
                "Provider '{}' timed out before auction collection completed",
                state.provider_name
            );
            responses.push(provider_timeout_response(
                &state.provider_name,
                response_time_ms,
            ));
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
    #[cfg(test)]
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

    async fn dispatch_planned_auction(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> DispatchAuctionOutcome {
        let plan = &self.plan;
        if self.planned_providers.is_empty() {
            return DispatchAuctionOutcome::NotStarted;
        }
        if self.planned_providers.len() > 1
            && !context.services.http_client().supports_concurrent_fanout()
        {
            log::warn!(
                "{} planned auction providers configured on a runtime without concurrent fanout",
                self.planned_providers.len()
            );
            return DispatchAuctionOutcome::NotStarted;
        }

        let auction_start = Instant::now();
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
        let signer = match plan
            .signing_enabled()
            .then(|| RequestSigner::from_services(context.services))
            .transpose()
        {
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
                    elapsed_ms: auction_start.elapsed().as_millis() as u64,
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
        let mut pending_requests = Vec::new();
        let mut planned_backend_to_provider = HashMap::new();
        let mut reserved_backend_names = HashSet::new();
        let mut immediate_response_count = 0usize;

        for input in routed.inputs() {
            let Some(provider) = self
                .planned_providers
                .iter()
                .find(|provider| provider.provider_name() == input.provider_id().as_str())
                .cloned()
            else {
                completed_responses.push(provider_launch_failed_response(
                    input.provider_id().as_str(),
                    0,
                ));
                continue;
            };
            let logical_budget_ms =
                remaining_budget_ms(auction_start, context.timeout_ms).min(provider.timeout_ms());
            if logical_budget_ms == 0 {
                completed_responses.push(provider_timeout_response(provider.provider_name(), 0));
                continue;
            }
            let transport_timeout_ms = context
                .services
                .backend()
                .canonicalize_transport_timeout_ms(logical_budget_ms, provider.timeout_ms());
            let started_at = Instant::now();
            match provider
                .request_bids_routed(
                    input,
                    &routed,
                    logical_budget_ms,
                    transport_timeout_ms,
                    signer.as_ref(),
                    context.services,
                    &mut reserved_backend_names,
                )
                .await
            {
                Ok(ProviderRequestOutcome::Pending {
                    request: pending,
                    parse_state,
                }) => {
                    let Some(backend_name) = pending.backend_name().map(str::to_string) else {
                        completed_responses.push(provider_launch_failed_response(
                            provider.provider_name(),
                            started_at.elapsed().as_millis() as u64,
                        ));
                        continue;
                    };
                    match planned_backend_to_provider.entry(backend_name.clone()) {
                        Entry::Vacant(entry) => {
                            entry.insert(PlannedLaunchState {
                                provider,
                                started_at,
                                parse_state,
                            });
                            pending_requests.push(pending.with_backend_name(backend_name));
                        }
                        Entry::Occupied(_) => {
                            completed_responses.push(provider_launch_failed_response(
                                provider.provider_name(),
                                started_at.elapsed().as_millis() as u64,
                            ))
                        }
                    }
                }
                Ok(ProviderRequestOutcome::Immediate(response)) => {
                    immediate_response_count += 1;
                    completed_responses.push(response);
                }
                Err(error) => {
                    log::warn!(
                        "Planned provider '{}' failed to dispatch: {error:?}",
                        provider.provider_name()
                    );
                    completed_responses.push(provider_launch_failed_response(
                        provider.provider_name(),
                        started_at.elapsed().as_millis() as u64,
                    ));
                }
            }
        }

        if pending_requests.is_empty()
            && immediate_response_count == 0
            && completed_responses.is_empty()
        {
            return DispatchAuctionOutcome::NotStarted;
        }

        DispatchAuctionOutcome::Dispatched(DispatchedAuction {
            pending_requests,
            backend_to_provider: HashMap::new(),
            planned_backend_to_provider,
            completed_responses,
            auction_start,
            timeout_ms: context.timeout_ms,
            floor_prices: self.floor_prices_by_slot(request),
            provider_request_context: Box::new(snapshot_context_request(context.request)),
            request: request.clone(),
            planned_unused_bidder_params,
            planned_unroutable_bidder_count,
            planned_provider_order,
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
        if !self.enabled {
            return DispatchAuctionOutcome::NotStarted;
        }
        if !cfg!(test) || self.plan_backed {
            return self.dispatch_planned_auction(request, context).await;
        }
        #[cfg(test)]
        let provider_names = self
            .config
            .providers
            .keys()
            .map(super::plan::ProviderId::as_str)
            .collect::<Vec<_>>();
        #[cfg(test)]
        if provider_names.is_empty() {
            return DispatchAuctionOutcome::NotStarted;
        }

        // Mirror run_providers_parallel: reject multi-provider fan-out before
        // any request launches when the platform executes `send_async` eagerly
        // (e.g. Cloudflare Workers, Spin). Sequential execution would accrue
        // the sum of provider latencies before the origin fetch and then fail
        // collection with empty bids.
        #[cfg(test)]
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
        #[cfg(test)]
        let mut backend_to_provider: HashMap<String, ProviderLaunchState> = HashMap::new();
        #[cfg(not(test))]
        let backend_to_provider: HashMap<String, ProviderLaunchState> = HashMap::new();
        #[cfg(test)]
        let mut pending_requests: Vec<PlatformPendingRequest> = Vec::new();
        #[cfg(not(test))]
        let pending_requests: Vec<PlatformPendingRequest> = Vec::new();
        #[cfg(test)]
        let mut completed_responses: Vec<AuctionResponse> = Vec::new();
        #[cfg(not(test))]
        let completed_responses: Vec<AuctionResponse> = Vec::new();
        #[cfg(test)]
        let mut immediate_response_count = 0usize;
        #[cfg(not(test))]
        let immediate_response_count = 0usize;

        #[cfg(test)]
        for provider_name in &provider_names {
            let provider = match self.providers.get(*provider_name) {
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
            let effective_timeout = context
                .services
                .backend()
                .canonicalize_transport_timeout_ms(remaining_ms, provider.timeout_ms());

            // Match the synchronous path's strict deadline semantics: do not
            // invoke even an immediate provider after the budget reaches zero.
            if effective_timeout == 0 {
                log::warn!(
                    "Auction timeout ({}ms) exhausted before launching '{}' — skipping",
                    context.timeout_ms,
                    provider.provider_name()
                );
                continue;
            }

            // Do not require a backend name before dispatch: an immediate
            // provider intentionally has none. Guard predicted names when
            // available; pending requests without either name fail below.
            let predicted_backend_name = provider.backend_name(context.services, effective_timeout);
            if let Some(backend_name) = predicted_backend_name.as_ref()
                && backend_to_provider.contains_key(backend_name)
            {
                log::warn!(
                    "Provider '{}' predicted backend name '{}' already belongs to another provider; skipping dispatch",
                    provider.provider_name(),
                    backend_name,
                );
                completed_responses
                    .push(provider_launch_failed_response(provider.provider_name(), 0));
                continue;
            }

            let provider_context = AuctionContext {
                settings: context.settings,
                request: context.request,
                timeout_ms: effective_timeout,
                transport_timeout_ms: effective_timeout,
                provider_responses: context.provider_responses,
                services: context.services,
            };

            let start_time = Instant::now();
            match provider.request_bids(request, &provider_context).await {
                Ok(ProviderRequestOutcome::Pending {
                    request: pending,
                    parse_state,
                }) => {
                    let backend_name = pending.backend_name().map(str::to_string).or_else(|| {
                        if let Some(backend_name) = predicted_backend_name.as_ref() {
                            log::warn!(
                                "Provider '{}' pending request returned no backend name; using predicted name '{}'",
                                provider.provider_name(),
                                backend_name,
                            );
                        }
                        predicted_backend_name.clone()
                    });
                    let Some(backend_name) = backend_name else {
                        log::warn!(
                            "Provider '{}' pending request has no backend name; response cannot be correlated",
                            provider.provider_name()
                        );
                        completed_responses.push(provider_launch_failed_response(
                            provider.provider_name(),
                            start_time.elapsed().as_millis() as u64,
                        ));
                        continue;
                    };
                    if backend_to_provider.contains_key(&backend_name) {
                        log::warn!(
                            "Provider '{}' resolved backend name '{}' already belongs to another provider; skipping dispatch",
                            provider.provider_name(),
                            backend_name,
                        );
                        completed_responses.push(provider_launch_failed_response(
                            provider.provider_name(),
                            start_time.elapsed().as_millis() as u64,
                        ));
                        continue;
                    }
                    log::info!(
                        "Dispatching bid request to '{}' (backend: {}, budget: {}ms)",
                        provider.provider_name(),
                        backend_name,
                        effective_timeout
                    );
                    backend_to_provider.insert(
                        backend_name.clone(),
                        ProviderLaunchState {
                            provider_name: provider.provider_name().to_string(),
                            started_at: start_time,
                            provider: Arc::clone(provider),
                            effective_timeout_ms: effective_timeout,
                            parse_state,
                        },
                    );
                    pending_requests.push(pending.with_backend_name(backend_name));
                }
                Ok(ProviderRequestOutcome::Immediate(response)) => {
                    immediate_response_count += 1;
                    completed_responses.push(response);
                }
                Err(e) => {
                    let response_time_ms = start_time.elapsed().as_millis() as u64;
                    log::warn!(
                        "Provider '{}' failed to dispatch request: {:?}",
                        provider.provider_name(),
                        e
                    );
                    completed_responses.push(provider_launch_failed_response(
                        provider.provider_name(),
                        response_time_ms,
                    ));
                }
            }
        }

        if pending_requests.is_empty() && immediate_response_count == 0 {
            return if completed_responses.is_empty() {
                DispatchAuctionOutcome::NotStarted
            } else {
                DispatchAuctionOutcome::DispatchFailed {
                    request: request.clone(),
                    provider_responses: completed_responses,
                    fatal_admission_error: None,
                    metadata: HashMap::new(),
                    elapsed_ms: auction_start.elapsed().as_millis() as u64,
                }
            };
        }

        log::info!(
            "Dispatched {} SSP request(s) with {} immediate response(s) (timeout: {}ms)",
            pending_requests.len(),
            immediate_response_count,
            context.timeout_ms
        );

        DispatchAuctionOutcome::Dispatched(DispatchedAuction {
            pending_requests,
            backend_to_provider,
            planned_backend_to_provider: HashMap::new(),
            completed_responses,
            auction_start,
            timeout_ms: context.timeout_ms,
            floor_prices: self.floor_prices_by_slot(request),
            provider_request_context: Box::new(snapshot_context_request(context.request)),
            request: request.clone(),
            planned_unused_bidder_params: HashMap::new(),
            planned_unroutable_bidder_count: 0,
            planned_provider_order: HashMap::new(),
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
            mut planned_backend_to_provider,
            completed_responses,
            auction_start,
            timeout_ms,
            floor_prices,
            provider_request_context,
            request,
            planned_unused_bidder_params,
            planned_unroutable_bidder_count,
            planned_provider_order,
        } = dispatched;

        log::info!(
            "Collecting {} in-flight SSP responses (timeout: {}ms remaining: {}ms)",
            pending_requests.len(),
            timeout_ms,
            remaining_budget_ms(auction_start, timeout_ms),
        );

        let mut responses: Vec<AuctionResponse> = completed_responses;
        let mut remaining = pending_requests;
        let deadline_policy = AuctionDeadlinePolicy::for_runtime(services);

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
                    if let Some(state) = backend_to_provider.remove(&backend_name) {
                        let response_time_ms = state.started_at.elapsed().as_millis() as u64;
                        if deadline_policy.rejects_late_completion(auction_start, timeout_ms) {
                            responses.push(provider_timeout_response(
                                &state.provider_name,
                                response_time_ms,
                            ));
                            continue;
                        }
                        let provider_context = AuctionContext {
                            settings: context.settings,
                            request: &provider_request_context,
                            timeout_ms: state.effective_timeout_ms,
                            transport_timeout_ms: state.effective_timeout_ms,
                            provider_responses: context.provider_responses,
                            services: context.services,
                        };
                        match state
                            .provider
                            .parse_response_with_context_and_state(
                                platform_response,
                                response_time_ms,
                                &request,
                                &provider_context,
                                state.parse_state.as_deref(),
                            )
                            .await
                        {
                            Ok(auction_response) => responses.push(auction_response),
                            Err(error) => responses.push(provider_error_response(
                                &state.provider_name,
                                response_time_ms,
                                ERROR_TYPE_PARSE_RESPONSE,
                                &error,
                            )),
                        }
                    } else if let Some(state) = planned_backend_to_provider.remove(&backend_name) {
                        let response_time_ms = state.started_at.elapsed().as_millis() as u64;
                        if deadline_policy.rejects_late_completion(auction_start, timeout_ms) {
                            responses.push(provider_timeout_response(
                                state.provider.provider_name(),
                                response_time_ms,
                            ));
                            continue;
                        }
                        match state
                            .provider
                            .parse_response_with_state(
                                platform_response,
                                response_time_ms,
                                state.parse_state.as_deref(),
                            )
                            .await
                        {
                            Ok(response) => responses.push(response),
                            Err(error) => responses.push(provider_error_response(
                                state.provider.provider_name(),
                                response_time_ms,
                                ERROR_TYPE_PARSE_RESPONSE,
                                &error,
                            )),
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
                        if let Some(state) = backend_to_provider.remove(backend_name) {
                            let response_time_ms = state.started_at.elapsed().as_millis() as u64;
                            log::warn!(
                                "Provider '{}' request failed: {:?}",
                                state.provider_name,
                                e
                            );
                            responses.push(provider_transport_failed_response(
                                &state.provider_name,
                                response_time_ms,
                            ));
                        } else if let Some(state) = planned_backend_to_provider.remove(backend_name)
                        {
                            let response_time_ms = state.started_at.elapsed().as_millis() as u64;
                            log::warn!(
                                "Planned provider '{}' request failed: {:?}",
                                state.provider.provider_name(),
                                e
                            );
                            responses.push(provider_transport_failed_response(
                                state.provider.provider_name(),
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

        for state in backend_to_provider.values() {
            let response_time_ms = state.started_at.elapsed().as_millis() as u64;
            log::warn!(
                "Provider '{}' timed out before dispatched auction collection completed",
                state.provider_name
            );
            responses.push(provider_timeout_response(
                &state.provider_name,
                response_time_ms,
            ));
        }
        backend_to_provider.clear();
        for state in planned_backend_to_provider.into_values() {
            responses.push(provider_timeout_response(
                state.provider.provider_name(),
                state.started_at.elapsed().as_millis() as u64,
            ));
        }
        for response in &mut responses {
            if let Some(&count) = planned_unused_bidder_params.get(response.provider.as_str()) {
                *response = materialize_planned_response(response.clone(), count);
            }
        }
        if !planned_provider_order.is_empty() {
            responses.sort_by_key(|response| {
                planned_provider_order
                    .get(response.provider.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            });
        }

        #[cfg(not(test))]
        let mediator = self.mediator.as_ref();
        #[cfg(test)]
        let mediator = self.mediator.as_ref().or_else(|| {
            self.config
                .mediator
                .as_ref()
                .and_then(|name| self.providers.get(name))
        });
        let (mediator_response, winning_bids) = if let Some(mediator) = mediator {
            {
                // Cap the mediator at whichever is tighter: its own configured
                // timeout or the remaining auction budget (A_deadline). Backend
                // first-byte and between-bytes timeouts bound normal collection, but
                // they are transport timers rather than absolute wall-clock limits:
                // connection setup and byte-trickling can still consume more of the
                // auction budget. Recomputing the remaining budget here prevents the
                // mediator from extending that bounded response hold.
                let remaining = remaining_budget_ms(auction_start, timeout_ms);
                let logical_budget_ms = remaining.min(mediator.timeout_ms());
                if logical_budget_ms == 0 {
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
                        metadata: routing_metadata(planned_unroutable_bidder_count),
                    };
                }
                let transport_timeout_ms = services
                    .backend()
                    .canonicalize_transport_timeout_ms(logical_budget_ms, mediator.timeout_ms());
                let mediator_start = Instant::now();
                log::info!(
                    "Running mediator '{}' with {}ms logical budget and {}ms transport timeout (A_deadline remaining: {}ms, configured: {}ms)",
                    mediator.provider_name(),
                    logical_budget_ms,
                    transport_timeout_ms,
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
                    timeout_ms: logical_budget_ms,
                    transport_timeout_ms,
                    provider_responses: Some(&responses),
                    services: context.services,
                };
                let mediator_response = match mediator
                    .request_bids(&request, &mediator_context)
                    .await
                {
                    Ok(ProviderRequestOutcome::Immediate(response)) => Some(response),
                    Ok(ProviderRequestOutcome::Pending {
                        request: pending,
                        parse_state,
                    }) => match services.http_client().wait(pending).await.change_context(
                        TrustedServerError::Auction {
                            message: format!(
                                "Mediator {} request failed",
                                mediator.provider_name()
                            ),
                        },
                    ) {
                        Ok(platform_resp) => {
                            let response_time_ms = mediator_start.elapsed().as_millis() as u64;
                            if deadline_policy.rejects_late_completion(auction_start, timeout_ms) {
                                log::warn!(
                                    "Mediator '{}' completed after the hard auction deadline; using local ranking ({}ms)",
                                    mediator.provider_name(),
                                    response_time_ms
                                );
                                None
                            } else {
                                match mediator
                                    .parse_response_with_context_and_state(
                                        platform_resp,
                                        response_time_ms,
                                        &request,
                                        &mediator_context,
                                        parse_state.as_deref(),
                                    )
                                    .await
                                {
                                    Ok(response) => Some(response),
                                    Err(error) => {
                                        log::warn!(
                                            "Mediator '{}' parse failed: {:?}",
                                            mediator.provider_name(),
                                            error
                                        );
                                        None
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            log::warn!("Mediator request failed: {:?}", error);
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

                if let Some(mediator_response) = mediator_response {
                    let winning = mediator_response
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
                    let winning = self.apply_floor_prices(winning, &floor_prices);
                    (Some(mediator_response), winning)
                } else {
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
            metadata: routing_metadata(planned_unroutable_bidder_count),
        }
    }

    /// Check if orchestrator is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
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
    use web_time::Instant;

    use crate::auction::config::AuctionConfig;
    use crate::auction::orchestrator::DispatchAuctionOutcome;
    use crate::auction::plan::{
        AuctionPlan, AuctionPlanConfig, NotificationConfig, ProviderConfig, ProviderId, RoutingMode,
    };
    use crate::auction::provider::{
        AuctionProvider, GenericOpenRtbProvider, ProviderRequestOutcome,
    };
    use crate::auction::routing::{RoutingDiagnostics, route_auction};
    use crate::auction::test_support::create_test_auction_context;
    use crate::auction::types::{
        AdFormat, AdSlot, ApsRendererV1, ApsTagType, AuctionContext, AuctionRequest,
        AuctionResponse, Bid, BidRenderer, BidStatus, MediaType, PublisherInfo, UserInfo,
    };
    use crate::error::TrustedServerError;
    use crate::integrations::adserver_mock::{AdServerMockConfig, AdServerMockProvider};
    use crate::platform::test_support::{
        StubHttpClient, build_services_with_backend_and_http_client,
        build_services_with_http_client, noop_services,
    };
    use crate::platform::{
        BackendNamingPolicy, PlatformBackend, PlatformBackendSpec, PlatformConfigStore,
        PlatformError, PlatformHttpRequest, PlatformResponse, PlatformSecretStore, RuntimeServices,
        StoreId, StoreName,
    };
    use crate::test_support::tests::crate_test_settings_str;
    use error_stack::{Report, ResultExt};
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{
        AuctionOrchestrator, AuctionOrchestratorHarness, DispatchedAuction, ERROR_TYPE_TIMEOUT,
        OrchestrationResult,
    };

    fn planned_config(providers: &[(&str, RoutingMode)], signing: bool) -> AuctionPlanConfig {
        AuctionPlanConfig {
            timeout_ms: 777,
            providers: providers
                .iter()
                .map(|(id, routing)| {
                    (
                        ProviderId::from_str(id).expect("should parse fictional provider ID"),
                        ProviderConfig {
                            protocol: "openrtb-2.6".to_string(),
                            profile: "standard".to_string(),
                            endpoint: "https://example.test/openrtb".to_string(),
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
                            protocol: "openrtb-2.6".to_string(),
                            profile: "prebid-server".to_string(),
                            endpoint: format!("https://{id}.example.test/openrtb"),
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
                            protocol: "openrtb-2.6".to_string(),
                            profile: "aps".to_string(),
                            endpoint: "https://aps.example/e/pb/bid".to_string(),
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
        let http = Arc::new(StubHttpClient::new());
        let services = build_services_with_http_client(Arc::clone(&http) as Arc<_>);
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
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
        assert!(http.recorded_backend_names().is_empty());
    }

    #[tokio::test]
    async fn enabled_empty_plan_is_successful_no_bid_without_dispatch() {
        let plan = Arc::new(
            AuctionPlan::compile(planned_config(&[], false)).expect("should compile empty plan"),
        );
        let orchestrator = AuctionOrchestrator::from_plan(plan, None);
        let http = Arc::new(StubHttpClient::new());
        let services = build_services_with_http_client(Arc::clone(&http) as Arc<_>);
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
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
        assert!(http.recorded_backend_names().is_empty());
    }

    #[tokio::test]
    async fn all_skipped_from_plan_completes_with_routing_metadata_in_sync_and_split_paths() {
        for split in [false, true] {
            let plan = Arc::new(
                AuctionPlan::compile(planned_config(&[("skipped", RoutingMode::Explicit)], false))
                    .expect("should compile all-skipped plan"),
            );
            let orchestrator = AuctionOrchestrator::from_plan(plan, None);
            let http = Arc::new(StubHttpClient::new());
            let services = build_services_with_http_client(Arc::clone(&http) as Arc<_>);
            let settings = create_test_settings();
            let inbound = http::Request::new(edgezero_core::body::Body::empty());
            let context = AuctionContext {
                settings: &settings,
                request: &inbound,
                timeout_ms: 777,
                transport_timeout_ms: 777,
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
            assert!(http.recorded_backend_names().is_empty());
        }
    }

    struct NamingBackend {
        policy: BackendNamingPolicy,
        predicted: AtomicUsize,
        ensured: AtomicUsize,
        specs: Mutex<Vec<PlatformBackendSpec>>,
        fail_ensure_for: Mutex<HashSet<String>>,
    }

    impl NamingBackend {
        fn new(policy: BackendNamingPolicy) -> Self {
            Self {
                policy,
                predicted: AtomicUsize::new(0),
                ensured: AtomicUsize::new(0),
                specs: Mutex::new(Vec::new()),
                fail_ensure_for: Mutex::new(HashSet::new()),
            }
        }

        fn fail_ensure_for(&self, provider_id: &str) {
            self.fail_ensure_for
                .lock()
                .expect("should lock failing provider IDs")
                .insert(provider_id.to_string());
        }

        fn name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            self.policy
                .predict(spec)
                .map(|prediction| prediction.name)
                .change_context(PlatformError::Backend)
        }
    }

    impl PlatformBackend for NamingBackend {
        fn naming_policy(&self) -> BackendNamingPolicy {
            self.policy
        }

        fn predict_name(
            &self,
            spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            self.predicted.fetch_add(1, Ordering::Relaxed);
            self.name(spec)
        }

        fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            self.ensured.fetch_add(1, Ordering::Relaxed);
            if spec.discriminator.as_deref().is_some_and(|provider_id| {
                self.fail_ensure_for
                    .lock()
                    .expect("should lock failing provider IDs")
                    .contains(provider_id)
            }) {
                return Err(Report::new(PlatformError::Backend));
            }
            self.specs
                .lock()
                .expect("should lock planned backend specs")
                .push(spec.clone());
            self.name(spec)
        }
    }

    struct CollidingBackend;

    impl PlatformBackend for CollidingBackend {
        fn naming_policy(&self) -> BackendNamingPolicy {
            BackendNamingPolicy::Axum
        }

        fn predict_name(
            &self,
            _spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Ok("colliding-backend".to_string())
        }

        fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Ok("colliding-backend".to_string())
        }
    }

    struct FailingCountingConfigStore {
        reads: AtomicUsize,
    }

    impl PlatformConfigStore for FailingCountingConfigStore {
        fn get(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<String, Report<PlatformError>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Err(Report::new(PlatformError::ConfigStore))
        }

        fn put(
            &self,
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct CountingConfigStore {
        reads: AtomicUsize,
        current_kid: String,
        delay: Duration,
    }

    impl PlatformConfigStore for CountingConfigStore {
        fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            (key == "current-kid")
                .then(|| self.current_kid.clone())
                .ok_or_else(|| Report::new(PlatformError::ConfigStore))
        }

        fn put(
            &self,
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct CountingSecretStore {
        reads: AtomicUsize,
        key: Vec<u8>,
    }

    impl PlatformSecretStore for CountingSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.key.clone())
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct UnusedSecretStore;

    impl PlatformSecretStore for UnusedSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            panic!("signing key should not be read after current-kid failure")
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    // ---------------------------------------------------------------------------
    // Minimal test double for AuctionProvider
    // ---------------------------------------------------------------------------

    struct StubAuctionProvider {
        name: &'static str,
        backend: &'static str,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for StubAuctionProvider {
        fn provider_name(&self) -> &str {
            self.name
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
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
                .map(ProviderRequestOutcome::pending)
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

    struct DeadlineBidProvider {
        name: &'static str,
        backend: &'static str,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for DeadlineBidProvider {
        fn provider_name(&self) -> &str {
            self.name
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            let request = PlatformHttpRequest::new(
                http::Request::builder()
                    .method("POST")
                    .uri("https://example.com/bid")
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build deadline test request"),
                self.backend,
            );
            context
                .services
                .http_client()
                .send_async(request)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "deadline test provider launch failed".to_string(),
                })
                .map(ProviderRequestOutcome::pending)
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            Ok(AuctionResponse::success(
                self.name,
                vec![auction_bid(self.name, 3.0)],
                response_time_ms,
            ))
        }

        fn timeout_ms(&self) -> u32 {
            1_000
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some(self.backend.to_string())
        }
    }

    type RecordedMediatorBudgets = Arc<Mutex<Vec<(u32, u32)>>>;

    struct DeadlineRecordingMediator {
        launches: Arc<AtomicUsize>,
        budgets: Option<RecordedMediatorBudgets>,
    }

    struct PendingDeadlineMediator;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for PendingDeadlineMediator {
        fn provider_name(&self) -> &str {
            "pending-deadline-mediator"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            let request = PlatformHttpRequest::new(
                http::Request::builder()
                    .method("POST")
                    .uri("https://example.com/mediate")
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build pending mediator request"),
                "pending-mediator-backend",
            );
            context
                .services
                .http_client()
                .send_async(request)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "pending mediator launch failed".to_string(),
                })
                .map(ProviderRequestOutcome::pending)
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            Ok(AuctionResponse::success(
                self.provider_name(),
                vec![auction_bid("mediated", 9.0)],
                response_time_ms,
            ))
        }

        fn timeout_ms(&self) -> u32 {
            1_000
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("pending-mediator-backend".to_string())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for DeadlineRecordingMediator {
        fn provider_name(&self) -> &str {
            "deadline-mediator"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            self.launches.fetch_add(1, Ordering::Relaxed);
            if let Some(budgets) = &self.budgets {
                budgets
                    .lock()
                    .expect("should lock mediator budgets")
                    .push((context.timeout_ms, context.transport_timeout_ms));
            }
            Ok(ProviderRequestOutcome::Immediate(AuctionResponse::no_bid(
                self.provider_name(),
                0,
            )))
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("immediate mediator response should not be parsed");
        }

        fn timeout_ms(&self) -> u32 {
            1_000
        }
    }

    struct RecordingTimeoutProvider {
        name: &'static str,
        backend: &'static str,
        configured_timeout_ms: u32,
        predicted: Arc<Mutex<Vec<u32>>>,
        requested: Arc<Mutex<Vec<u32>>>,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for RecordingTimeoutProvider {
        fn provider_name(&self) -> &str {
            self.name
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            self.requested
                .lock()
                .expect("should lock requested timeouts")
                .push(context.transport_timeout_ms);
            let request = PlatformHttpRequest::new(
                http::Request::builder()
                    .method("POST")
                    .uri("https://example.com/bid")
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build recording request"),
                self.backend,
            );
            context
                .services
                .http_client()
                .send_async(request)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "recording launch failed".to_string(),
                })
                .map(ProviderRequestOutcome::pending)
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

        fn timeout_ms(&self) -> u32 {
            self.configured_timeout_ms
        }

        fn backend_name(&self, _services: &RuntimeServices, timeout_ms: u32) -> Option<String> {
            self.predicted
                .lock()
                .expect("should lock predicted timeouts")
                .push(timeout_ms);
            Some(self.backend.to_string())
        }
    }

    struct DivergentBackendProvider {
        name: &'static str,
        predicted: &'static str,
        resolved: &'static str,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for DivergentBackendProvider {
        fn provider_name(&self) -> &str {
            self.name
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            let request = PlatformHttpRequest::new(
                http::Request::builder()
                    .method("POST")
                    .uri("https://example.com/bid")
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build divergent request"),
                self.resolved,
            );
            context
                .services
                .http_client()
                .send_async(request)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "divergent launch failed".to_string(),
                })
                .map(ProviderRequestOutcome::pending)
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

        fn timeout_ms(&self) -> u32 {
            2000
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some(self.predicted.to_string())
        }
    }

    struct CanonicalTimeoutBackend {
        canonical_ms: u32,
        calls: Arc<Mutex<Vec<(u32, u32)>>>,
    }

    impl PlatformBackend for CanonicalTimeoutBackend {
        fn naming_policy(&self) -> crate::platform::BackendNamingPolicy {
            crate::platform::BackendNamingPolicy::Axum
        }

        fn predict_name(
            &self,
            _spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Ok("stub-backend".to_string())
        }

        fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Ok("stub-backend".to_string())
        }

        fn canonicalize_transport_timeout_ms(&self, remaining_ms: u32, configured_ms: u32) -> u32 {
            self.calls
                .lock()
                .expect("should lock canonicalization calls")
                .push((remaining_ms, configured_ms));
            self.canonical_ms
        }
    }

    fn recording_provider(
        name: &'static str,
        backend: &'static str,
        configured_timeout_ms: u32,
        predicted: &Arc<Mutex<Vec<u32>>>,
        requested: &Arc<Mutex<Vec<u32>>>,
    ) -> RecordingTimeoutProvider {
        RecordingTimeoutProvider {
            name,
            backend,
            configured_timeout_ms,
            predicted: Arc::clone(predicted),
            requested: Arc::clone(requested),
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

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for CacheRestoringMediator {
        fn provider_name(&self) -> &str {
            "mediator"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
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
                .map(ProviderRequestOutcome::pending)
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
            _response: PlatformResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("immediate mediator response should not be parsed");
        }

        fn timeout_ms(&self) -> u32 {
            2000
        }
    }

    #[tokio::test]
    async fn mediated_bid_preserves_restored_fields_through_run_auction() {
        // run_parallel_mediation must parse the mediator response via
        // parse_response_with_context so cache/nurl fields restored from SSP
        // responses survive the synchronous mediation path (POST /auction,
        // /_ts/page-bids), matching the dispatched collect path.
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"{}".to_vec()); // bidder send_async
        stub.push_response(200, b"{}".to_vec()); // mediator send_async
        let services = build_services_with_http_client(stub);
        // SAFETY: `Box::leak` creates a `'static` reference for test use only.
        let services: &'static RuntimeServices = Box::leak(Box::new(services));

        let config = AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["bidder"]),
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
            transport_timeout_ms: 2000,
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

    #[tokio::test]
    async fn immediate_mediator_completes_in_sync_and_split_paths() {
        for split in [false, true] {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            let services = build_services_with_http_client(stub);
            let config = AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["bidder"]),
                mediator: Some("immediate-mediator".to_string()),
                timeout_ms: 2000,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "bidder",
                backend: "bidder-backend",
            }));
            orchestrator.register_provider(Arc::new(ImmediateMediator));
            let request = create_test_auction_request();
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = AuctionContext {
                settings: &settings,
                request: &downstream,
                timeout_ms: 2000,
                transport_timeout_ms: 2000,
                provider_responses: None,
                services: &services,
            };

            let result = if split {
                let DispatchAuctionOutcome::Dispatched(dispatched) =
                    orchestrator.dispatch_auction(&request, &context).await
                else {
                    panic!("bidder request should dispatch");
                };
                orchestrator
                    .collect_dispatched_auction(dispatched, &services, &context)
                    .await
            } else {
                orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect("auction with immediate mediator should complete")
            };

            assert_eq!(
                result
                    .mediator_response
                    .as_ref()
                    .map(|response| response.provider.as_str()),
                Some("immediate-mediator")
            );
            assert_eq!(
                result
                    .winning_bids
                    .get("header-banner")
                    .and_then(|bid| bid.nurl.as_deref()),
                Some("https://nurl.example/immediate")
            );
        }
    }

    async fn collect_deadline_test_result(
        split: bool,
        enforceable_total_request_deadline: bool,
    ) -> OrchestrationResult {
        let stub = Arc::new(StubHttpClient::new());
        stub.set_enforceable_total_request_deadline(enforceable_total_request_deadline);
        stub.push_response(200, b"{}".to_vec());
        stub.push_response(200, b"{}".to_vec());
        stub.push_select_delay(Duration::from_millis(50));
        let services = build_services_with_http_client(Arc::clone(&stub) as Arc<_>);
        let config = AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["late-one", "late-two"]),
            timeout_ms: 10,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(DeadlineBidProvider {
            name: "late-one",
            backend: "late-one-backend",
        }));
        orchestrator.register_provider(Arc::new(DeadlineBidProvider {
            name: "late-two",
            backend: "late-two-backend",
        }));
        let request = create_test_auction_request();
        let settings = create_test_settings();
        let downstream = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &downstream,
            timeout_ms: 10,
            transport_timeout_ms: 10,
            provider_responses: None,
            services: &services,
        };

        if split {
            let DispatchAuctionOutcome::Dispatched(dispatched) =
                orchestrator.dispatch_auction(&request, &context).await
            else {
                panic!("deadline test providers should dispatch");
            };
            orchestrator
                .collect_dispatched_auction(dispatched, &services, &context)
                .await
        } else {
            orchestrator
                .run_auction(&request, &context)
                .await
                .expect("deadline test auction should complete")
        }
    }

    #[tokio::test]
    async fn current_adapter_deadline_drains_late_responses_in_both_paths() {
        for split in [false, true] {
            let result = collect_deadline_test_result(split, false).await;
            assert_eq!(result.provider_responses.len(), 2);
            assert_eq!(result.provider_responses[0].provider, "late-one");
            assert_eq!(result.provider_responses[0].status, BidStatus::Success);
            assert_eq!(result.provider_responses[1].provider, "late-two");
            assert_eq!(result.provider_responses[1].status, BidStatus::Success);
            assert!(
                result
                    .provider_responses
                    .iter()
                    .all(|response| response.response_time_ms >= 50),
                "late response times should retain actual elapsed duration"
            );
            assert_eq!(
                result.winning_bids["slot-1"].bidder, "late-one",
                "a completed response remains eligible after the logical deadline"
            );
        }
    }

    #[tokio::test]
    async fn synthetic_hard_deadline_classifies_late_responses_in_both_paths() {
        for split in [false, true] {
            let result = collect_deadline_test_result(split, true).await;
            assert_eq!(result.provider_responses.len(), 2);
            assert!(result.provider_responses.iter().all(|response| {
                response.status == BidStatus::Error
                    && response.metadata["error_type"] == ERROR_TYPE_TIMEOUT
                    && response.response_time_ms >= 50
            }));
            assert!(result.winning_bids.is_empty());
        }
    }

    async fn pending_mediator_deadline_test_result(
        split: bool,
        enforceable_total_request_deadline: bool,
    ) -> OrchestrationResult {
        let stub = Arc::new(StubHttpClient::new());
        stub.set_enforceable_total_request_deadline(enforceable_total_request_deadline);
        stub.push_response(200, b"{}".to_vec());
        stub.push_response(200, b"{}".to_vec());
        stub.push_select_delay(Duration::ZERO);
        stub.push_select_delay(Duration::from_millis(50));
        let services = build_services_with_http_client(Arc::clone(&stub) as Arc<_>);
        let config = AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["local"]),
            mediator: Some("pending-deadline-mediator".to_string()),
            timeout_ms: 20,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(DeadlineBidProvider {
            name: "local",
            backend: "local-backend",
        }));
        orchestrator.register_provider(Arc::new(PendingDeadlineMediator));
        let request = create_test_auction_request();
        let settings = create_test_settings();
        let downstream = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &downstream,
            timeout_ms: 20,
            transport_timeout_ms: 20,
            provider_responses: None,
            services: &services,
        };

        if split {
            let DispatchAuctionOutcome::Dispatched(dispatched) =
                orchestrator.dispatch_auction(&request, &context).await
            else {
                panic!("deadline test provider should dispatch");
            };
            orchestrator
                .collect_dispatched_auction(dispatched, &services, &context)
                .await
        } else {
            orchestrator
                .run_auction(&request, &context)
                .await
                .expect("deadline test auction should complete")
        }
    }

    #[tokio::test]
    async fn pending_mediator_late_completion_policy_is_equivalent_in_sync_and_split_paths() {
        for split in [false, true] {
            let current = pending_mediator_deadline_test_result(split, false).await;
            let current_mediator = current
                .mediator_response
                .as_ref()
                .expect("current adapters should accept completed late mediator responses");
            assert!(
                current_mediator.response_time_ms >= 50,
                "mediator timing should preserve actual elapsed duration"
            );
            assert_eq!(current.winning_bids["slot-1"].bidder, "mediated");

            let hard = pending_mediator_deadline_test_result(split, true).await;
            assert!(hard.mediator_response.is_none());
            assert_eq!(hard.winning_bids["slot-1"].bidder, "local");
            assert!(
                hard.total_time_ms >= 50,
                "discarding a late mediator must retain actual total elapsed time"
            );
        }
    }

    #[tokio::test]
    async fn split_deadline_skips_mediator_and_falls_back_to_provider_winner() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"{}".to_vec());
        stub.push_select_delay(Duration::from_millis(50));
        let services = build_services_with_http_client(Arc::clone(&stub) as Arc<_>);
        let launches = Arc::new(AtomicUsize::new(0));
        let config = AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["late-one"]),
            mediator: Some("deadline-mediator".to_string()),
            timeout_ms: 10,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(DeadlineBidProvider {
            name: "late-one",
            backend: "late-one-backend",
        }));
        orchestrator.register_provider(Arc::new(DeadlineRecordingMediator {
            launches: Arc::clone(&launches),
            budgets: None,
        }));
        let request = create_test_auction_request();
        let settings = create_test_settings();
        let downstream = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &downstream,
            timeout_ms: 10,
            transport_timeout_ms: 10,
            provider_responses: None,
            services: &services,
        };
        let DispatchAuctionOutcome::Dispatched(dispatched) =
            orchestrator.dispatch_auction(&request, &context).await
        else {
            panic!("deadline test provider should dispatch");
        };
        let result = orchestrator
            .collect_dispatched_auction(dispatched, &services, &context)
            .await;

        assert_eq!(launches.load(Ordering::Relaxed), 0);
        assert!(result.mediator_response.is_none());
        assert_eq!(result.winning_bids["slot-1"].bidder, "late-one");
    }

    #[tokio::test]
    async fn synchronous_deadline_skips_mediator_and_falls_back_to_provider_winner() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"{}".to_vec());
        stub.push_select_delay(Duration::from_millis(50));
        let services = build_services_with_http_client(Arc::clone(&stub) as Arc<_>);
        let launches = Arc::new(AtomicUsize::new(0));
        let config = AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["late-one"]),
            mediator: Some("deadline-mediator".to_string()),
            timeout_ms: 10,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(DeadlineBidProvider {
            name: "late-one",
            backend: "late-one-backend",
        }));
        orchestrator.register_provider(Arc::new(DeadlineRecordingMediator {
            launches: Arc::clone(&launches),
            budgets: None,
        }));
        let request = create_test_auction_request();
        let settings = create_test_settings();
        let downstream = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &downstream,
            timeout_ms: 10,
            transport_timeout_ms: 10,
            provider_responses: None,
            services: &services,
        };
        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("synchronous deadline test should complete");

        assert_eq!(launches.load(Ordering::Relaxed), 0);
        assert!(result.mediator_response.is_none());
        assert_eq!(result.winning_bids["slot-1"].bidder, "late-one");
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
            _response: PlatformResponse,
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

    fn immediate_test_context<'a>(
        settings: &'a crate::settings::Settings,
        request: &'a http::Request<edgezero_core::body::Body>,
        services: &'a RuntimeServices,
    ) -> AuctionContext<'a> {
        AuctionContext {
            settings,
            request,
            timeout_ms: 2000,
            transport_timeout_ms: 2000,
            provider_responses: None,
            services,
        }
    }

    #[tokio::test]
    async fn synchronous_auction_accepts_an_all_immediate_no_bid_result() {
        let config = AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["immediate"]),
            timeout_ms: 2000,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(ImmediateNoBidProvider));
        let settings = create_test_settings();
        let services = noop_services();
        let downstream = http::Request::new(edgezero_core::body::Body::empty());
        let context = immediate_test_context(&settings, &downstream, &services);

        let result = orchestrator
            .run_auction(&create_test_auction_request(), &context)
            .await
            .expect("all-immediate auction should complete");

        assert_eq!(result.provider_responses.len(), 1);
        assert_eq!(result.provider_responses[0].status, BidStatus::NoBid);
        assert!(result.winning_bids.is_empty());
    }

    #[tokio::test]
    async fn split_auction_accepts_an_all_immediate_no_bid_result() {
        let config = AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["immediate"]),
            timeout_ms: 2000,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(ImmediateNoBidProvider));
        let settings = create_test_settings();
        let services = noop_services();
        let downstream = http::Request::new(edgezero_core::body::Body::empty());
        let context = immediate_test_context(&settings, &downstream, &services);

        let DispatchAuctionOutcome::Dispatched(dispatched) = orchestrator
            .dispatch_auction(&create_test_auction_request(), &context)
            .await
        else {
            panic!("enabled immediate provider should dispatch");
        };
        let result = orchestrator
            .collect_dispatched_auction(dispatched, &services, &context)
            .await;

        assert_eq!(result.provider_responses.len(), 1);
        assert_eq!(result.provider_responses[0].status, BidStatus::NoBid);
        assert!(result.winning_bids.is_empty());
    }

    #[tokio::test]
    async fn immediate_and_pending_providers_complete_in_sync_and_split_paths() {
        for split in [false, true] {
            let config = AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["immediate", "pending"]),
                timeout_ms: 2000,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(ImmediateNoBidProvider));
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "pending",
                backend: "pending-backend",
            }));
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            let services = build_services_with_http_client(stub);
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = immediate_test_context(&settings, &downstream, &services);
            let request = create_test_auction_request();

            let result = if split {
                let DispatchAuctionOutcome::Dispatched(dispatched) =
                    orchestrator.dispatch_auction(&request, &context).await
                else {
                    panic!("mixed immediate/pending auction should dispatch");
                };
                orchestrator
                    .collect_dispatched_auction(dispatched, &services, &context)
                    .await
            } else {
                orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect("mixed immediate/pending auction should complete")
            };

            assert_eq!(result.provider_responses.len(), 2);
            assert!(result.provider_responses.iter().any(|response| {
                response.provider == "immediate" && response.status == BidStatus::NoBid
            }));
            assert!(result.provider_responses.iter().any(|response| {
                response.provider == "pending" && response.status == BidStatus::Success
            }));
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

    // Timeout paths still need a controllable delayed pending response:
    // - Deadline check in select() loop (drops remaining requests)
    // - Mediator skip when remaining_ms == 0 (bidding exhausts budget)
    // - Provider skip when effective_timeout == 0 (budget exhausted before launch)
    // - Provider context receives reduced timeout_ms per remaining budget
    //
    // Follow-up: extend StubHttpClient with delayed responses so these paths can
    // be asserted deterministically without real backends or wall-clock races.

    #[test]
    fn test_no_providers_configured() {
        futures::executor::block_on(async {
            let config = AuctionConfig {
                enabled: true,
                sanitize_creatives: true,
                rewrite_creatives: true,
                providers: AuctionConfig::legacy_provider_map(&[]),
                bidders: Default::default(),
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
                providers: AuctionConfig::legacy_provider_map(&["launch-failing"]),
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

            let error = orchestrator
                .run_auction(&request, &context)
                .await
                .expect_err("should fail when every provider launch fails");

            assert!(
                error
                    .to_string()
                    .contains("All 1 configured provider(s) skipped or failed to launch"),
                "should explain that no configured provider request launched"
            );
        });
    }

    #[tokio::test]
    async fn duplicate_backend_name_fails_second_provider_attributably_in_both_paths() {
        for split in [false, true] {
            let config = AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["provider-a", "provider-b"]),
                timeout_ms: 2000,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "provider-a",
                backend: "shared-backend",
            }));
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "provider-b",
                backend: "shared-backend",
            }));
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            let services = build_services_with_http_client(stub);
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = immediate_test_context(&settings, &downstream, &services);
            let request = create_test_auction_request();

            let result = if split {
                let DispatchAuctionOutcome::Dispatched(dispatched) =
                    orchestrator.dispatch_auction(&request, &context).await
                else {
                    panic!("should dispatch the first provider");
                };
                orchestrator
                    .collect_dispatched_auction(dispatched, &services, &context)
                    .await
            } else {
                orchestrator
                    .run_auction(&request, &context)
                    .await
                    .expect("should complete auction despite the collision")
            };

            assert!(result.provider_responses.iter().any(|response| {
                response.provider == "provider-a" && response.status == BidStatus::Success
            }));
            assert!(result.provider_responses.iter().any(|response| {
                response.provider == "provider-b" && response.status == BidStatus::Error
            }));
        }
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
    fn parallel_launch_applies_canonical_timeout_to_name_and_request() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            let calls = Arc::new(Mutex::new(Vec::new()));
            let services = build_services_with_backend_and_http_client(
                Arc::new(CanonicalTimeoutBackend {
                    canonical_ms: 750,
                    calls: Arc::clone(&calls),
                }),
                stub,
            );
            let predicted = Arc::new(Mutex::new(Vec::new()));
            let requested = Arc::new(Mutex::new(Vec::new()));
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["bidder"]),
                timeout_ms: 2000,
                ..Default::default()
            });
            orchestrator.register_provider(Arc::new(recording_provider(
                "bidder",
                "bidder-backend",
                1000,
                &predicted,
                &requested,
            )));
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = immediate_test_context(&settings, &downstream, &services);

            orchestrator
                .run_auction(&create_test_auction_request(), &context)
                .await
                .expect("should complete auction");

            assert_eq!(*predicted.lock().expect("should lock predicted"), vec![750]);
            assert_eq!(*requested.lock().expect("should lock requested"), vec![750]);
            let calls = calls.lock().expect("should lock calls");
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].1, 1000);
            assert!(calls[0].0 > 0 && calls[0].0 <= 2000);
        });
    }

    #[test]
    fn zero_canonical_timeout_skips_parallel_launch() {
        futures::executor::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let services = build_services_with_backend_and_http_client(
                Arc::new(CanonicalTimeoutBackend {
                    canonical_ms: 0,
                    calls,
                }),
                Arc::new(StubHttpClient::new()),
            );
            let predicted = Arc::new(Mutex::new(Vec::new()));
            let requested = Arc::new(Mutex::new(Vec::new()));
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["bidder"]),
                timeout_ms: 2000,
                ..Default::default()
            });
            orchestrator.register_provider(Arc::new(recording_provider(
                "bidder",
                "bidder-backend",
                1000,
                &predicted,
                &requested,
            )));
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = immediate_test_context(&settings, &downstream, &services);

            let result = orchestrator
                .run_auction(&create_test_auction_request(), &context)
                .await;

            assert!(result.is_err(), "zero budget should skip every provider");
            assert!(predicted.lock().expect("should lock predicted").is_empty());
            assert!(requested.lock().expect("should lock requested").is_empty());
        });
    }

    #[test]
    fn synchronous_mediation_applies_canonical_timeout_to_mediator() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            stub.push_response(200, b"{}".to_vec());
            let calls = Arc::new(Mutex::new(Vec::new()));
            let services = build_services_with_backend_and_http_client(
                Arc::new(CanonicalTimeoutBackend {
                    canonical_ms: 500,
                    calls,
                }),
                stub,
            );
            let predicted = Arc::new(Mutex::new(Vec::new()));
            let requested = Arc::new(Mutex::new(Vec::new()));
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["bidder"]),
                mediator: Some("mediator".to_string()),
                timeout_ms: 2000,
                ..Default::default()
            });
            orchestrator.register_provider(Arc::new(StubAuctionProvider {
                name: "bidder",
                backend: "bidder-backend",
            }));
            orchestrator.register_provider(Arc::new(recording_provider(
                "mediator",
                "mediator-backend",
                2000,
                &predicted,
                &requested,
            )));
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = immediate_test_context(&settings, &downstream, &services);

            orchestrator
                .run_auction(&create_test_auction_request(), &context)
                .await
                .expect("should complete mediated auction");

            assert!(predicted.lock().expect("should lock predicted").is_empty());
            assert_eq!(*requested.lock().expect("should lock requested"), vec![500]);
        });
    }

    #[test]
    fn dispatched_collect_applies_canonical_timeout_to_both_paths() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            stub.push_response(200, b"{}".to_vec());
            let calls = Arc::new(Mutex::new(Vec::new()));
            let services = build_services_with_backend_and_http_client(
                Arc::new(CanonicalTimeoutBackend {
                    canonical_ms: 500,
                    calls,
                }),
                stub,
            );
            let bidder_predicted = Arc::new(Mutex::new(Vec::new()));
            let bidder_requested = Arc::new(Mutex::new(Vec::new()));
            let mediator_predicted = Arc::new(Mutex::new(Vec::new()));
            let mediator_requested = Arc::new(Mutex::new(Vec::new()));
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["bidder"]),
                mediator: Some("mediator".to_string()),
                timeout_ms: 2000,
                ..Default::default()
            });
            orchestrator.register_provider(Arc::new(recording_provider(
                "bidder",
                "bidder-backend",
                2000,
                &bidder_predicted,
                &bidder_requested,
            )));
            orchestrator.register_provider(Arc::new(recording_provider(
                "mediator",
                "mediator-backend",
                2000,
                &mediator_predicted,
                &mediator_requested,
            )));
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = immediate_test_context(&settings, &downstream, &services);
            let request = create_test_auction_request();

            let DispatchAuctionOutcome::Dispatched(dispatched) =
                orchestrator.dispatch_auction(&request, &context).await
            else {
                panic!("should dispatch bidder request");
            };
            orchestrator
                .collect_dispatched_auction(dispatched, &services, &context)
                .await;

            assert_eq!(
                *bidder_predicted.lock().expect("should lock predicted"),
                vec![500]
            );
            assert_eq!(
                *bidder_requested.lock().expect("should lock requested"),
                vec![500]
            );
            assert!(
                mediator_predicted
                    .lock()
                    .expect("should lock predicted")
                    .is_empty()
            );
            assert_eq!(
                *mediator_requested.lock().expect("should lock requested"),
                vec![500]
            );
        });
    }

    #[test]
    fn planned_collect_launches_mediator_with_positive_sub_quantum_logical_budget() {
        futures::executor::block_on(async {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let services = build_services_with_backend_and_http_client(
                Arc::new(CanonicalTimeoutBackend {
                    canonical_ms: 0,
                    calls: Arc::clone(&calls),
                }),
                Arc::new(StubHttpClient::new()),
            );
            let launches = Arc::new(AtomicUsize::new(0));
            let budgets = Arc::new(Mutex::new(Vec::new()));
            let plan = AuctionPlan::compile(AuctionPlanConfig {
                timeout_ms: 49,
                providers: BTreeMap::new(),
                bidders: BTreeMap::new(),
                mediator: Some("adserver_mock".to_string()),
                request_signing: None,
            })
            .expect("should compile mediator-only plan")
            .with_enabled(true);
            let orchestrator = AuctionOrchestrator::from_plan(
                Arc::new(plan),
                Some(Arc::new(DeadlineRecordingMediator {
                    launches: Arc::clone(&launches),
                    budgets: Some(Arc::clone(&budgets)),
                })),
            );
            let request = create_test_auction_request();
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = AuctionContext {
                settings: &settings,
                request: &downstream,
                timeout_ms: 49,
                transport_timeout_ms: 49,
                provider_responses: None,
                services: &services,
            };
            let dispatched = DispatchedAuction::empty_for_test(request, 49);

            let result = orchestrator
                .collect_dispatched_auction(dispatched, &services, &context)
                .await;

            assert_eq!(launches.load(Ordering::Relaxed), 1);
            let budgets = budgets.lock().expect("should lock mediator budgets");
            assert_eq!(budgets.len(), 1);
            assert!(
                (1..50).contains(&budgets[0].0),
                "logical budget should remain positive and below the Fastly quantum"
            );
            assert_eq!(budgets[0].1, 0);
            assert_eq!(calls.lock().expect("should lock calls").len(), 1);
            assert!(result.mediator_response.is_some());
        });
    }

    #[test]
    fn dispatched_resolved_backend_name_diverging_from_prediction_still_correlates() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            let services = build_services_with_http_client(stub);
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["provider-a"]),
                timeout_ms: 2000,
                ..Default::default()
            });
            orchestrator.register_provider(Arc::new(DivergentBackendProvider {
                name: "provider-a",
                predicted: "predicted-backend",
                resolved: "resolved-backend",
            }));
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = immediate_test_context(&settings, &downstream, &services);
            let request = create_test_auction_request();

            let DispatchAuctionOutcome::Dispatched(dispatched) =
                orchestrator.dispatch_auction(&request, &context).await
            else {
                panic!("should dispatch provider");
            };
            let result = orchestrator
                .collect_dispatched_auction(dispatched, &services, &context)
                .await;

            assert!(result.provider_responses.iter().any(|response| {
                response.provider == "provider-a" && response.status == BidStatus::Success
            }));
        });
    }

    #[test]
    fn dispatched_post_launch_resolved_name_collision_fails_second_provider_attributably() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            stub.push_response(200, b"{}".to_vec());
            let services = build_services_with_http_client(stub);
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["provider-a", "provider-b"]),
                timeout_ms: 2000,
                ..Default::default()
            });
            orchestrator.register_provider(Arc::new(DivergentBackendProvider {
                name: "provider-a",
                predicted: "predicted-a",
                resolved: "shared-resolved",
            }));
            orchestrator.register_provider(Arc::new(DivergentBackendProvider {
                name: "provider-b",
                predicted: "predicted-b",
                resolved: "shared-resolved",
            }));
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = immediate_test_context(&settings, &downstream, &services);
            let request = create_test_auction_request();

            let DispatchAuctionOutcome::Dispatched(dispatched) =
                orchestrator.dispatch_auction(&request, &context).await
            else {
                panic!("should dispatch first provider");
            };
            let result = orchestrator
                .collect_dispatched_auction(dispatched, &services, &context)
                .await;

            assert!(result.provider_responses.iter().any(|response| {
                response.provider == "provider-a" && response.status == BidStatus::Success
            }));
            assert!(result.provider_responses.iter().any(|response| {
                response.provider == "provider-b" && response.status == BidStatus::Error
            }));
        });
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
                providers: AuctionConfig::legacy_provider_map(&["provider-a", "provider-b"]),
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
                transport_timeout_ms: 2000,
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
                providers: AuctionConfig::legacy_provider_map(&["provider-a"]),
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
                transport_timeout_ms: 750,
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
                transport_timeout_ms: 750,
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
                providers: AuctionConfig::legacy_provider_map(&["provider-a", "provider-b"]),
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
                transport_timeout_ms: 2000,
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
                providers: AuctionConfig::legacy_provider_map(&["provider-a", "provider-b"]),
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
                transport_timeout_ms: 2000,
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

    #[tokio::test]
    async fn from_plan_standard_provider_runs_direct_and_split_with_correlation_and_metadata() {
        for split in [false, true] {
            let http = Arc::new(StubHttpClient::new());
            http.push_response(
                200,
                serde_json::to_vec(&serde_json::json!({
                    "seatbid": [{"seat": "provider-seat", "bid": [{
                        "id": "provider-bid", "impid": "fictional-slot", "price": 2.0,
                        "adm": "<div>provider</div>", "w": 300, "h": 250
                    }]}]
                }))
                .expect("should serialize provider response"),
            );
            let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
            let services = build_services_with_backend_and_http_client(
                Arc::clone(&backend) as Arc<_>,
                Arc::clone(&http) as Arc<_>,
            );
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
                transport_timeout_ms: 777,
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

            assert_eq!(http.recorded_backend_names().len(), 1);
            assert_eq!(backend.ensured.load(Ordering::Relaxed), 1);
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
        let http = Arc::new(StubHttpClient::new());
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "provider-seat", "bid": [{
                    "id": "provider", "impid": "fictional-slot", "price": 2.0,
                    "adm": "<div>provider</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize provider response"),
        );
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[("provider-a", RoutingMode::AllEligible)],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = AuctionOrchestratorHarness::new(plan, Some(Arc::new(ImmediateMediator)));
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
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

    async fn planned_pending_mediator_deadline_result(
        enforceable_total_request_deadline: bool,
    ) -> OrchestrationResult {
        let http = Arc::new(StubHttpClient::new());
        http.set_enforceable_total_request_deadline(enforceable_total_request_deadline);
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "provider-seat", "bid": [{
                    "id": "provider", "impid": "fictional-slot", "price": 2.0,
                    "adm": "<div>provider</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize provider response"),
        );
        http.push_response(200, b"{}".to_vec());
        http.push_select_delay(Duration::ZERO);
        http.push_select_delay(Duration::from_millis(50));
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[("provider-a", RoutingMode::AllEligible)],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator =
            AuctionOrchestratorHarness::new(plan, Some(Arc::new(PendingDeadlineMediator)));
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 20,
            transport_timeout_ms: 20,
            provider_responses: None,
            services: &services,
        };

        orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute planned pending mediator")
    }

    #[tokio::test]
    async fn planned_pending_mediator_applies_explicit_hard_deadline_policy() {
        let current = planned_pending_mediator_deadline_result(false).await;
        let current_mediator = current
            .mediator_response
            .as_ref()
            .expect("current adapters should accept completed late mediator responses");
        assert!(current_mediator.response_time_ms >= 50);
        assert_eq!(current.winning_bids["slot-1"].bidder, "mediated");

        let hard = planned_pending_mediator_deadline_result(true).await;
        assert!(hard.mediator_response.is_none());
        assert_eq!(
            hard.winning_bids["fictional-slot"].bid_id.as_deref(),
            Some("provider")
        );
        assert!(hard.total_time_ms >= 50);
    }

    #[tokio::test]
    async fn planned_executor_mediator_transport_failure_falls_back_locally() {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "provider-seat", "bid": [{
                    "id": "provider", "impid": "fictional-slot", "price": 2.0,
                    "adm": "<div>provider</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize provider response"),
        );
        http.push_response(200, b"{}".to_vec());
        http.push_select_success();
        http.push_select_error();
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[("provider-a", RoutingMode::AllEligible)],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator =
            AuctionOrchestratorHarness::new(plan, Some(Arc::new(CacheRestoringMediator)));
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
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
    async fn planned_prebid_instances_preserve_headers_metadata_suppression_and_identity() {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(
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
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "keep-seat", "bid": [{
                    "id":"good-b","impid":"fictional-slot","price":2.5,"adm":"<div>b</div>","w":300,"h":250,"nurl":"https://notify.example/win","burl":"https://notify.example/bill"
                }]}]
            }))
            .expect("should serialize PBS response b"),
        );
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Fastly));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
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
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
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
            transport_timeout_ms: 777,
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

        let headers = http.recorded_request_headers();
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
        assert_eq!(first_cookie, Some("consent=keep; other=value"));
        let second_cookie = headers[1]
            .iter()
            .find(|(name, _)| name == "cookie")
            .map(|(_, value)| value.as_str());
        assert_eq!(
            second_cookie,
            Some("consent=keep; euconsent-v2=drop; other=value")
        );
    }

    #[tokio::test]
    async fn planned_aps_mock_mediation_preserves_three_identities_and_renderer() {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(
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
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "aps-instance", "bid": [{
                    "id": "mediated-aps", "impid": "fictional-slot", "price": 2.0,
                    "adm": "ignored", "w": 300, "h": 250, "crid": "aps-creative"
                }]}]
            }))
            .expect("should serialize mediator response"),
        );
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan =
            AuctionPlan::compile(planned_aps_config()).expect("should compile planned APS auction");
        let mediator = AdServerMockProvider::new(AdServerMockConfig {
            enabled: true,
            endpoint: "https://mediator.example/mediate".to_string(),
            timeout_ms: 500,
            ..AdServerMockConfig::default()
        });
        let orchestrator = AuctionOrchestratorHarness::new(plan, Some(Arc::new(mediator)));
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
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
        let http = Arc::new(StubHttpClient::new());
        http.push_response(400, Vec::new());
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Fastly));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan =
            AuctionPlan::compile(planned_aps_config()).expect("should compile planned APS auction");
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
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
            transport_timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute planned APS auction");

        let headers = http.recorded_request_headers();
        assert_eq!(headers.len(), 1);
        assert!(
            headers[0].iter().all(|(name, _)| name != "accept"),
            "planned APS transport must not add Accept beyond legacy headers"
        );
    }

    #[tokio::test]
    async fn planned_aps_profile_normalizes_renderer_reduction_and_metadata() {
        let http = Arc::new(StubHttpClient::new());
        http.push_response_with_headers(
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
            vec![
                ("content-type", "application/json"),
                ("authorization", "fictional-secret"),
            ],
        );
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Fastly));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_aps_instances_config(&[(
            "aps-instance",
            serde_json::json!({"account_id": "example-account", "debug": true}),
            NotificationConfig {
                suppress_all: false,
                suppress_seats: vec!["returned-seat".to_string()],
            },
        )]))
        .expect("should compile planned APS profile");
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
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
            serde_json::json!({}),
            "async stub does not preserve queued response headers"
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
        let http = Arc::new(StubHttpClient::new());
        for (seat, id, price) in [("seat-a", "bid-a", 1.0), ("seat-b", "bid-b", 2.0)] {
            http.push_response(
                200,
                serde_json::to_vec(&serde_json::json!({"seatbid":[{"seat":seat,"bid":[{
                    "id":id,"impid":"fictional-slot","price":price,"w":300,"h":250,
                    "ext":{"creativeurl":"https://creative.example/render","tagtype":"iframe"}
                }]}]}))
                .expect("should serialize APS instance response"),
            );
        }
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Fastly));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
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
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
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
        assert_eq!(http.recorded_request_bodies().len(), 2);
        assert_eq!(
            result.winning_bids["fictional-slot"].bid_id.as_deref(),
            Some("bid-b"),
            "global ranking should remain orchestrator-owned"
        );
        let specs = backend.specs.lock().expect("should lock specs");
        assert_eq!(specs.len(), 2);
        assert_ne!(specs[0].discriminator, specs[1].discriminator);
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
            let response = PlatformResponse::new(
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
            (204, Vec::new(), BidStatus::NoBid, None),
            (400, Vec::new(), BidStatus::Error, None),
            (
                200,
                b"not-json".to_vec(),
                BidStatus::Error,
                Some("unexpected_response_shape"),
            ),
            (
                200,
                b"[]".to_vec(),
                BidStatus::Error,
                Some("unexpected_response_shape"),
            ),
            (
                200,
                br#"{"contextual":true}"#.to_vec(),
                BidStatus::Error,
                Some("unexpected_response_shape"),
            ),
            (
                200,
                br#"{"cur":"EUR","seatbid":[]}"#.to_vec(),
                BidStatus::NoBid,
                Some("unsupported_currency"),
            ),
        ];
        for (status, body, expected, reason) in cases {
            let state = provider.parse_state_for_test(routed.inputs()[0].clone());
            let response = PlatformResponse::new(
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
                planned_request(),
                &http::Request::new(edgezero_core::body::Body::empty()),
                plan,
                None,
            );
            let provider = GenericOpenRtbProvider::new(plan.providers()[0].clone());
            let state = provider.parse_state_for_test(routed.inputs()[0].clone());
            let response = PlatformResponse::new(
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
        let response = PlatformResponse::new(
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
        let response = PlatformResponse::new(
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
    async fn planned_standard_instances_have_distinct_backends_and_independent_results() {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "fictional-seat-a", "bid": [{
                    "id": "bid-a", "impid": "fictional-slot", "price": 1.25,
                    "adm": "<div>a</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize response a"),
        );
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "fictional-seat-b", "bid": [{
                    "id": "bid-b", "impid": "fictional-slot", "price": 2.5,
                    "adm": "<div>b</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize response b"),
        );
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Fastly));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("provider-a", RoutingMode::AllEligible),
                ("provider-b", RoutingMode::AllEligible),
            ],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
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
            transport_timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute planned auction");

        assert_eq!(orchestrator.provider_count(), 2);
        assert!(orchestrator.mediator().is_none());
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
        let request_headers = http.recorded_request_headers();
        assert_eq!(request_headers.len(), 2);
        for headers in request_headers {
            assert!(
                headers
                    .iter()
                    .any(|(name, value)| name == "accept" && value == "application/json"),
                "standard planned transport should retain its JSON Accept header"
            );
        }
        let backend_names = http.recorded_backend_names();
        assert_eq!(backend_names.len(), 2);
        assert_ne!(backend_names[0], backend_names[1]);
        let request_bodies = http.recorded_request_bodies();
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
        let specs = backend.specs.lock().expect("should lock specs");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].first_byte_timeout, Duration::from_millis(750));
        assert_eq!(specs[1].first_byte_timeout, Duration::from_millis(750));
        assert_ne!(specs[0].discriminator, specs[1].discriminator);
    }

    #[tokio::test]
    async fn planned_backend_collision_does_not_overwrite_first_launch_state() {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "first", "bid": [{
                    "id": "first-bid", "impid": "fictional-slot", "price": 2.0,
                    "adm": "<div>first</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize first response"),
        );
        let backend = Arc::new(CollidingBackend);
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("provider-a", RoutingMode::AllEligible),
                ("provider-b", RoutingMode::AllEligible),
            ],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should isolate backend collision");

        assert_eq!(http.recorded_backend_names().len(), 1);
        assert_eq!(result.provider_responses[0].provider, "provider-a");
        assert_eq!(
            result.provider_responses[0].bids[0].bid_id.as_deref(),
            Some("first-bid")
        );
        assert_eq!(result.provider_responses[1].provider, "provider-b");
        assert_eq!(
            result.provider_responses[1].metadata["error_type"],
            "launch_failed"
        );
    }

    #[tokio::test]
    async fn planned_pending_backend_divergence_isolated_from_valid_provider() {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(204, Vec::new());
        http.push_response(204, Vec::new());
        http.push_pending_backend_name_override(Some("divergent-backend"));
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("provider-a", RoutingMode::AllEligible),
                ("provider-b", RoutingMode::AllEligible),
            ],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should isolate divergent pending backend");

        assert_eq!(result.provider_responses.len(), 2);
        assert_eq!(result.provider_responses[0].provider, "provider-a");
        assert_eq!(
            result.provider_responses[0].metadata["error_type"],
            "launch_failed"
        );
        assert_eq!(result.provider_responses[1].provider, "provider-b");
        assert_eq!(result.provider_responses[1].status, BidStatus::NoBid);
    }

    #[tokio::test]
    async fn planned_pending_backend_missing_isolated_from_valid_provider() {
        let http = Arc::new(StubHttpClient::new());
        http.push_response(204, Vec::new());
        http.push_response(204, Vec::new());
        http.push_pending_backend_name_override(None);
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("provider-a", RoutingMode::AllEligible),
                ("provider-b", RoutingMode::AllEligible),
            ],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should isolate missing pending backend");

        assert_eq!(result.provider_responses.len(), 2);
        assert_eq!(result.provider_responses[0].provider, "provider-a");
        assert_eq!(
            result.provider_responses[0].metadata["error_type"],
            "launch_failed"
        );
        assert_eq!(result.provider_responses[1].provider, "provider-b");
        assert_eq!(result.provider_responses[1].status, BidStatus::NoBid);
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
        let response = PlatformResponse::new(
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
            planned_request(),
            &http::Request::new(edgezero_core::body::Body::empty()),
            &plan,
            None,
        );
        let provider_a = GenericOpenRtbProvider::new(plan.providers()[0].clone());
        let provider_b = GenericOpenRtbProvider::new(plan.providers()[1].clone());
        let parse_state = provider_a.parse_state_for_test(routed.inputs()[0].clone());
        let response = PlatformResponse::new(
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
        let http = Arc::new(StubHttpClient::new());
        http.push_response(204, Vec::new());
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("eligible", RoutingMode::AllEligible),
                ("skipped", RoutingMode::Explicit),
            ],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute eligible provider only");

        assert_eq!(http.recorded_backend_names().len(), 1);
        assert_eq!(backend.ensured.load(Ordering::Relaxed), 1);
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
        let request_body = &http.recorded_request_bodies()[0];
        let request_value: serde_json::Value =
            serde_json::from_slice(request_body).expect("should parse request body");
        assert_eq!(request_value["imp"].as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn planned_launch_transport_parse_failures_are_isolated_from_valid_winner_and_floor() {
        let http = Arc::new(StubHttpClient::new());
        // BTreeMap plan order is alphabetical: below-floor, parse-fail,
        // transport-fail, valid-winner. Queue responses in that exact order.
        http.push_response(
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
        http.push_response(200, b"not-json".to_vec());
        http.push_response(200, b"{}".to_vec());
        http.push_response(
            200,
            serde_json::to_vec(&serde_json::json!({
                "seatbid": [{"seat": "winner", "bid": [{
                    "id": "winner", "impid": "fictional-slot", "price": 2.0,
                    "adm": "<div>winner</div>", "w": 300, "h": 250
                }]}]
            }))
            .expect("should serialize winner response"),
        );
        http.push_select_success();
        http.push_select_success();
        http.push_select_error();
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        backend.fail_ensure_for("launch-fail");
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("launch-fail", RoutingMode::AllEligible),
                ("transport-fail", RoutingMode::AllEligible),
                ("parse-fail", RoutingMode::AllEligible),
                ("below-floor", RoutingMode::AllEligible),
                ("valid-winner", RoutingMode::AllEligible),
            ],
            false,
        ))
        .expect("should compile failure isolation plan");
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
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
            transport_timeout_ms: 777,
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
        assert_eq!(
            by_provider
                .get("launch-fail")
                .unwrap_or_else(|| panic!(
                    "should include launch-fail response; got {:?}",
                    by_provider.keys().collect::<Vec<_>>()
                ))
                .metadata["error_type"],
            "launch_failed"
        );
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
            let http = Arc::new(StubHttpClient::new());
            http.push_response(200, vec![b'x'; 1024 * 1024 + 1]);
            let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
            let services = build_services_with_backend_and_http_client(
                Arc::clone(&backend) as Arc<_>,
                Arc::clone(&http) as Arc<_>,
            );
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
            let orchestrator = AuctionOrchestratorHarness::new(plan, None);
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
                transport_timeout_ms: 777,
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
        let config_store = Arc::new(CountingConfigStore {
            reads: AtomicUsize::new(0),
            current_kid: "test-kid".to_string(),
            delay: Duration::from_millis(50),
        });
        let secret_store = Arc::new(CountingSecretStore {
            reads: AtomicUsize::new(0),
            key: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7_u8; 32])
                .into_bytes(),
        });
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let http = Arc::new(StubHttpClient::new());
        http.push_response(204, Vec::new());
        let services = RuntimeServices::builder()
            .config_store(Arc::clone(&config_store) as Arc<_>)
            .secret_store(Arc::clone(&secret_store) as Arc<_>)
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::clone(&backend) as Arc<_>)
            .http_client(Arc::clone(&http) as Arc<_>)
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
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 200,
            transport_timeout_ms: 200,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should execute signed planned auction");

        let body: serde_json::Value = serde_json::from_slice(&http.recorded_request_bodies()[0])
            .expect("should parse signed request");
        let tmax = body["tmax"].as_u64().expect("should include tmax");
        assert!(
            (100..=175).contains(&tmax),
            "signer delay should reduce logical budget, got {tmax}"
        );
        assert!(
            result.total_time_ms >= 50,
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
            delay: Duration::ZERO,
        });
        let secret_store = Arc::new(CountingSecretStore {
            reads: AtomicUsize::new(0),
            key: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [11_u8; 32])
                .into_bytes(),
        });
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let http = Arc::new(StubHttpClient::new());
        http.push_response(204, Vec::new());
        http.push_response(204, Vec::new());
        let services = RuntimeServices::builder()
            .config_store(Arc::clone(&config_store) as Arc<_>)
            .secret_store(Arc::clone(&secret_store) as Arc<_>)
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::clone(&backend) as Arc<_>)
            .http_client(Arc::clone(&http) as Arc<_>)
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
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
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
        assert_eq!(http.recorded_backend_names().len(), 2);
        for body in http.recorded_request_bodies() {
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
            let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
            let http = Arc::new(StubHttpClient::new());
            let services = RuntimeServices::builder()
                .config_store(Arc::clone(&config_store) as Arc<_>)
                .secret_store(Arc::new(UnusedSecretStore))
                .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
                .backend(Arc::clone(&backend) as Arc<_>)
                .http_client(Arc::clone(&http) as Arc<_>)
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
                transport_timeout_ms: 777,
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
            assert_eq!(backend.predicted.load(Ordering::Relaxed), 0);
            assert_eq!(backend.ensured.load(Ordering::Relaxed), 0);
            assert!(http.recorded_backend_names().is_empty());
        }
    }

    #[tokio::test]
    async fn planned_signing_failure_reads_store_once_before_backend_or_send() {
        let config_store = Arc::new(FailingCountingConfigStore {
            reads: AtomicUsize::new(0),
        });
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let http = Arc::new(StubHttpClient::new());
        let services = RuntimeServices::builder()
            .config_store(Arc::clone(&config_store) as Arc<_>)
            .secret_store(Arc::new(UnusedSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::clone(&backend) as Arc<_>)
            .http_client(Arc::clone(&http) as Arc<_>)
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
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let _error = orchestrator
            .run_auction(&request, &context)
            .await
            .expect_err("should fail signer admission");

        assert_eq!(config_store.reads.load(Ordering::Relaxed), 1);
        assert_eq!(backend.predicted.load(Ordering::Relaxed), 0);
        assert_eq!(backend.ensured.load(Ordering::Relaxed), 0);
        assert!(http.recorded_backend_names().is_empty());
    }

    #[tokio::test]
    async fn planned_zero_logical_budget_does_no_signer_backend_or_network_work() {
        let config_store = Arc::new(FailingCountingConfigStore {
            reads: AtomicUsize::new(0),
        });
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Axum));
        let http = Arc::new(StubHttpClient::new());
        let services = RuntimeServices::builder()
            .config_store(Arc::clone(&config_store) as Arc<_>)
            .secret_store(Arc::new(UnusedSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::clone(&backend) as Arc<_>)
            .http_client(Arc::clone(&http) as Arc<_>)
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
        let mediator_launches = Arc::new(AtomicUsize::new(0));
        let orchestrator = AuctionOrchestratorHarness::new(
            plan,
            Some(Arc::new(DeadlineRecordingMediator {
                launches: Arc::clone(&mediator_launches),
                budgets: None,
            })),
        );
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 0,
            transport_timeout_ms: 0,
            provider_responses: None,
            services: &services,
        };

        let result = orchestrator
            .run_auction(&request, &context)
            .await
            .expect("should return zero-budget outcomes");

        assert_eq!(result.provider_responses.len(), 1);
        assert_eq!(
            result.provider_responses[0].metadata["error_type"],
            "timeout"
        );
        assert_eq!(config_store.reads.load(Ordering::Relaxed), 0);
        assert_eq!(backend.predicted.load(Ordering::Relaxed), 0);
        assert_eq!(backend.ensured.load(Ordering::Relaxed), 0);
        assert!(http.recorded_backend_names().is_empty());
        assert_eq!(
            mediator_launches.load(Ordering::Relaxed),
            0,
            "zero budget must not invoke even an immediate mediator"
        );
    }

    #[tokio::test]
    async fn planned_fanout_rejection_happens_before_backend_or_send() {
        let http = Arc::new(StubHttpClient::new());
        http.set_concurrent_fanout(false);
        let backend = Arc::new(NamingBackend::new(BackendNamingPolicy::Cloudflare));
        let services = build_services_with_backend_and_http_client(
            Arc::clone(&backend) as Arc<_>,
            Arc::clone(&http) as Arc<_>,
        );
        let plan = AuctionPlan::compile(planned_config(
            &[
                ("provider-a", RoutingMode::AllEligible),
                ("provider-b", RoutingMode::AllEligible),
            ],
            false,
        ))
        .expect("should compile planned auction");
        let orchestrator = AuctionOrchestratorHarness::new(plan, None);
        let request = planned_request();
        let settings = create_test_settings();
        let inbound = http::Request::new(edgezero_core::body::Body::empty());
        let context = AuctionContext {
            settings: &settings,
            request: &inbound,
            timeout_ms: 777,
            transport_timeout_ms: 777,
            provider_responses: None,
            services: &services,
        };

        let _error = orchestrator
            .run_auction(&request, &context)
            .await
            .expect_err("should reject unsupported concurrent fanout");

        assert_eq!(backend.predicted.load(Ordering::Relaxed), 0);
        assert_eq!(backend.ensured.load(Ordering::Relaxed), 0);
        assert!(http.recorded_backend_names().is_empty());
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
