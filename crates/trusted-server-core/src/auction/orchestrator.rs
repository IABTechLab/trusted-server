//! Auction orchestrator for managing multi-provider auctions.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::Request;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

use crate::error::TrustedServerError;
use crate::platform::{PlatformPendingRequest, RuntimeServices};

use super::config::AuctionConfig;
use super::provider::{
    AuctionProvider, ProviderParseState, ProviderRequestOutcome, ProviderSlotDisposition,
    ProviderSlotOutcome,
};
use super::telemetry::AbandonedProviderCall;
use super::types::{
    AuctionContext, AuctionDecisionSetV1, AuctionDropReason, AuctionIdentityGenerator,
    AuctionRequest, AuctionResponse, AuctionSlotFailureReason, Bid, BidStatus,
    SlotAuctionDecisionV1, SystemAuctionIdentityGenerator, mint_response_unique_base64url_identity,
};

const CANDIDATE_ID_BYTES: usize = 9;
const CANDIDATE_ID_COLLISION_RETRIES: usize = 8;
const MAX_UPSTREAM_BID_ID_BYTES: usize = 64;

struct NormalizedProviderResponses {
    outcomes: Vec<ProviderSlotOutcome>,
    candidates: HashMap<String, Bid>,
}

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
    completed_responses: Vec<AuctionResponse>,
    auction_start: Instant,
    timeout_ms: u32,
    floor_prices: HashMap<String, f64>,
    provider_request_context: Box<Request<EdgeBody>>,
    /// Carried so the mediator call in collect can pass it as the auction request.
    request: AuctionRequest,
}

struct ProviderLaunchState {
    provider_name: String,
    started_at: Instant,
    provider: Arc<dyn AuctionProvider>,
    effective_timeout_ms: u32,
    parse_state: Option<ProviderParseState>,
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
            .map(|state| {
                AbandonedProviderCall::bidder(
                    state.provider_name,
                    Some(u32::try_from(state.started_at.elapsed().as_millis()).unwrap_or(u32::MAX)),
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
            completed_responses: Vec::new(),
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
/// A non-2xx HTTP status from an upstream SSP (e.g. a PBS 4xx/5xx). Distinct
/// from [`ERROR_TYPE_TRANSPORT`] (a connection-level failure) so telemetry can
/// bucket it separately. `pub(crate)` so producers such as the prebid provider
/// tag errors with the exact value the telemetry layer recognises.
pub(crate) const ERROR_TYPE_HTTP_STATUS: &str = "http_status";

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

fn canonical_provider_response(
    expected_provider: &str,
    response: AuctionResponse,
) -> AuctionResponse {
    if response.provider == expected_provider {
        response
    } else {
        log::warn!(
            "Provider '{}' returned response identity '{}'; rejecting mismatched response",
            expected_provider,
            response.provider
        );
        AuctionResponse::error(expected_provider, response.response_time_ms)
            .with_drop_reason(AuctionDropReason::InvalidProviderResponse)
    }
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
    identity_generator: Arc<dyn AuctionIdentityGenerator>,
}

impl AuctionOrchestrator {
    /// Create a new orchestrator with the given configuration.
    #[must_use]
    pub fn new(config: AuctionConfig) -> Self {
        Self {
            config,
            providers: HashMap::new(),
            identity_generator: Arc::new(SystemAuctionIdentityGenerator),
        }
    }

    #[cfg(test)]
    fn with_identity_generator(
        config: AuctionConfig,
        identity_generator: Arc<dyn AuctionIdentityGenerator>,
    ) -> Self {
        Self {
            config,
            providers: HashMap::new(),
            identity_generator,
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

        let mut configured_providers = HashSet::new();
        for provider_name in &self.config.providers {
            if !configured_providers.insert(provider_name.as_str()) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "Auction provider `{provider_name}` is listed more than once in [auction].providers; each provider may appear at most once"
                    ),
                }));
            }
        }

        if let Some(mediator_name) = &self.config.mediator
            && configured_providers.contains(mediator_name.as_str())
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "Auction mediator `{mediator_name}` is also listed in [auction].providers; a provider may not mediate its own auction"
                ),
            }));
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

    fn provider_is_eligible_for_slot(
        &self,
        provider_name: &str,
        slot: &super::types::AdSlot,
    ) -> bool {
        self.providers.get(provider_name).is_some_and(|provider| {
            provider.is_enabled()
                && slot
                    .formats
                    .iter()
                    .any(|format| provider.supports_media_type(&format.media_type))
        })
    }

    fn eligible_slot_ids(&self, provider_name: &str, request: &AuctionRequest) -> HashSet<String> {
        request
            .slots
            .iter()
            .filter(|slot| self.provider_is_eligible_for_slot(provider_name, slot))
            .map(|slot| slot.id.clone())
            .collect()
    }

    fn valid_upstream_bid_id(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= MAX_UPSTREAM_BID_ID_BYTES
            && !value.bytes().any(|byte| byte <= 0x1f || byte == 0x7f)
    }

    fn mint_candidate_id(&self, issued: &mut HashSet<String>) -> Option<String> {
        let candidate_id = mint_response_unique_base64url_identity(
            self.identity_generator.as_ref(),
            issued,
            "",
            CANDIDATE_ID_BYTES,
            CANDIDATE_ID_COLLISION_RETRIES,
        )?;
        debug_assert_eq!(candidate_id.len(), 12);
        Some(candidate_id)
    }

    fn response_failure_reason(response: &AuctionResponse) -> Option<AuctionSlotFailureReason> {
        if response.status == BidStatus::Error || response.status == BidStatus::Pending {
            return match response
                .metadata
                .get("error_type")
                .and_then(serde_json::Value::as_str)
            {
                Some(ERROR_TYPE_TIMEOUT) => Some(AuctionSlotFailureReason::ProviderTimeout),
                Some(ERROR_TYPE_PARSE_RESPONSE) => {
                    Some(AuctionSlotFailureReason::InvalidProviderResponse)
                }
                _ => {
                    let invalid = response
                        .metadata
                        .get("drop_reasons")
                        .and_then(serde_json::Value::as_object)
                        .is_some_and(|reasons| reasons.contains_key("invalid_provider_response"));
                    Some(if invalid {
                        AuctionSlotFailureReason::InvalidProviderResponse
                    } else {
                        AuctionSlotFailureReason::ProviderError
                    })
                }
            };
        }

        None
    }

    fn normalize_provider_responses(
        &self,
        request: &AuctionRequest,
        responses: &mut [AuctionResponse],
    ) -> NormalizedProviderResponses {
        let requested_slots: HashMap<&str, &super::types::AdSlot> = request
            .slots
            .iter()
            .map(|slot| (slot.id.as_str(), slot))
            .collect();
        let mut issued_candidate_ids = HashSet::new();
        let mut candidates = HashMap::new();
        let mut outcomes = Vec::new();

        for response in responses {
            let eligible_slots = self.eligible_slot_ids(&response.provider, request);
            let response_failure = Self::response_failure_reason(response);
            let mut upstream_counts = HashMap::<String, usize>::new();
            for bid in &response.bids {
                if let Some(upstream_id) = bid.bid_id.as_deref()
                    && Self::valid_upstream_bid_id(upstream_id)
                {
                    *upstream_counts.entry(upstream_id.to_string()).or_default() += 1;
                }
            }

            let mut invalid_slots = HashMap::<String, AuctionSlotFailureReason>::new();
            let mut global_invalid = false;
            let mut accepted = Vec::new();
            for mut bid in core::mem::take(&mut response.bids) {
                let requested_slot = requested_slots.get(bid.slot_id.as_str()).copied();
                let slot_is_eligible = eligible_slots.contains(&bid.slot_id);
                let dimensions_match = requested_slot.is_some_and(|slot| {
                    slot.formats.iter().any(|format| {
                        format.width == bid.width
                            && format.height == bid.height
                            && self
                                .providers
                                .get(&response.provider)
                                .is_some_and(|provider| {
                                    provider.supports_media_type(&format.media_type)
                                })
                    })
                });
                let upstream_id = bid.bid_id.as_deref();
                let upstream_is_valid = upstream_id.is_some_and(Self::valid_upstream_bid_id);
                let upstream_is_unique = upstream_id.is_some_and(|upstream_id| {
                    upstream_counts.get(upstream_id).copied() == Some(1)
                });
                let bid_is_valid = response.status == BidStatus::Success
                    && slot_is_eligible
                    && dimensions_match
                    && upstream_is_valid
                    && upstream_is_unique
                    && bid.currency == "USD"
                    && bid
                        .price
                        .is_some_and(|price| price.is_finite() && price >= 0.0);

                if !bid_is_valid {
                    if requested_slot.is_some() {
                        invalid_slots
                            .entry(bid.slot_id.clone())
                            .or_insert(AuctionSlotFailureReason::InvalidProviderResponse);
                    } else {
                        global_invalid = true;
                    }
                    continue;
                }

                let Some(candidate_id) = self.mint_candidate_id(&mut issued_candidate_ids) else {
                    invalid_slots
                        .insert(bid.slot_id.clone(), AuctionSlotFailureReason::InternalError);
                    continue;
                };
                bid.candidate_id = Some(candidate_id.clone());
                bid.candidate_provider = Some(response.provider.clone());
                bid.renderer_reservation_id = None;
                candidates.insert(candidate_id, bid.clone());
                accepted.push(bid);
            }
            let internally_failed_slots: HashSet<&str> = invalid_slots
                .iter()
                .filter_map(|(slot, reason)| {
                    (*reason == AuctionSlotFailureReason::InternalError).then_some(slot.as_str())
                })
                .collect();
            if !internally_failed_slots.is_empty() {
                accepted.retain(|bid| !internally_failed_slots.contains(bid.slot_id.as_str()));
                candidates.retain(|_, bid| {
                    bid.candidate_provider.as_deref() != Some(response.provider.as_str())
                        || !internally_failed_slots.contains(bid.slot_id.as_str())
                });
            }
            response.bids = accepted;

            for slot in &request.slots {
                if !eligible_slots.contains(&slot.id) {
                    continue;
                }
                let slot_candidates: Vec<Bid> = response
                    .bids
                    .iter()
                    .filter(|bid| bid.slot_id == slot.id)
                    .cloned()
                    .collect();
                let disposition = if !slot_candidates.is_empty() {
                    ProviderSlotDisposition::Candidates(slot_candidates)
                } else if let Some(reason) = invalid_slots.get(&slot.id).copied() {
                    ProviderSlotDisposition::Failed(reason)
                } else if global_invalid {
                    ProviderSlotDisposition::Failed(
                        AuctionSlotFailureReason::InvalidProviderResponse,
                    )
                } else if let Some(reason) = response_failure {
                    ProviderSlotDisposition::Failed(reason)
                } else {
                    ProviderSlotDisposition::NoBid
                };
                outcomes.push(ProviderSlotOutcome {
                    provider: response.provider.clone(),
                    slot: slot.id.clone(),
                    disposition,
                });
            }
        }

        NormalizedProviderResponses {
            outcomes,
            candidates,
        }
    }

    fn build_decision_set(
        &self,
        request: &AuctionRequest,
        outcomes: &[ProviderSlotOutcome],
        winning_bids: &HashMap<String, Bid>,
        mediation_failed: bool,
    ) -> AuctionDecisionSetV1 {
        let results = request
            .slots
            .iter()
            .map(|slot| {
                if let Some(winner) = winning_bids.get(&slot.id) {
                    return winner.candidate_id.as_ref().map_or_else(
                        || SlotAuctionDecisionV1::Failed {
                            slot: slot.id.clone(),
                            reason: AuctionSlotFailureReason::WinnerNotRenderable,
                        },
                        |candidate_id| SlotAuctionDecisionV1::Winner {
                            slot: slot.id.clone(),
                            candidate_id: candidate_id.clone(),
                        },
                    );
                }

                let eligible_provider_count = self
                    .config
                    .provider_names()
                    .iter()
                    .filter(|provider| self.provider_is_eligible_for_slot(provider, slot))
                    .count();
                if eligible_provider_count == 0 {
                    return SlotAuctionDecisionV1::Failed {
                        slot: slot.id.clone(),
                        reason: AuctionSlotFailureReason::SlotNotEligible,
                    };
                }

                let mut failures: Vec<AuctionSlotFailureReason> = outcomes
                    .iter()
                    .filter(|outcome| outcome.slot == slot.id)
                    .filter_map(|outcome| match outcome.disposition {
                        ProviderSlotDisposition::Failed(reason) => Some(reason),
                        ProviderSlotDisposition::Candidates(_) | ProviderSlotDisposition::NoBid => {
                            None
                        }
                    })
                    .collect();
                if mediation_failed {
                    failures.push(AuctionSlotFailureReason::MediationFailed);
                }
                failures.sort_by_key(|reason| reason.priority());
                failures.first().copied().map_or_else(
                    || SlotAuctionDecisionV1::NoBid {
                        slot: slot.id.clone(),
                    },
                    |reason| SlotAuctionDecisionV1::Failed {
                        slot: slot.id.clone(),
                        reason,
                    },
                )
            })
            .collect();

        AuctionDecisionSetV1 {
            version: 1,
            auction_id: request.id.clone(),
            results,
        }
    }

    fn resolve_mediator_candidates(
        mediator_response: AuctionResponse,
        candidates: &HashMap<String, Bid>,
    ) -> Result<AuctionResponse, ()> {
        if mediator_response.status == BidStatus::Error
            || mediator_response.status == BidStatus::Pending
        {
            return Err(());
        }

        let mut seen = HashSet::new();
        let mut seen_slots = HashSet::new();
        let mut resolved = Vec::with_capacity(mediator_response.bids.len());
        for selection in &mediator_response.bids {
            let Some(candidate_id) = selection.candidate_id.as_deref() else {
                return Err(());
            };
            if !seen.insert(candidate_id.to_string()) {
                return Err(());
            }
            let Some(source) = candidates.get(candidate_id) else {
                return Err(());
            };
            let Some(selected_price) = selection
                .price
                .filter(|price| price.is_finite() && *price >= 0.0)
            else {
                return Err(());
            };
            let source_authority_matches = selection.slot_id == source.slot_id
                && selection.candidate_provider == source.candidate_provider
                && selection.currency == source.currency
                && selection.creative == source.creative
                && selection.adomain == source.adomain
                && selection.bidder == source.bidder
                && selection.width == source.width
                && selection.height == source.height
                && selection.nurl == source.nurl
                && selection.burl == source.burl
                && selection.bid_id == source.bid_id
                && selection.ad_id == source.ad_id
                && selection.creative_id == source.creative_id
                && selection.renderer == source.renderer
                && selection.cache_id == source.cache_id
                && selection.cache_host == source.cache_host
                && selection.cache_path == source.cache_path;
            if !seen_slots.insert(source.slot_id.as_str()) || !source_authority_matches {
                return Err(());
            }

            let mut restored = source.clone();
            restored.price = Some(selected_price);
            resolved.push(restored);
        }

        Ok(AuctionResponse {
            provider: mediator_response.provider,
            status: if resolved.is_empty() {
                BidStatus::NoBid
            } else {
                BidStatus::Success
            },
            bids: resolved,
            response_time_ms: mediator_response.response_time_ms,
            metadata: mediator_response.metadata,
        })
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

        if !self.config.enabled {
            return Ok(OrchestrationResult {
                provider_responses: Vec::new(),
                mediator_response: None,
                winning_bids: HashMap::new(),
                decision_set: AuctionDecisionSetV1::failed(
                    request,
                    AuctionSlotFailureReason::AuctionDisabled,
                ),
                total_time_ms: 0,
                metadata: HashMap::new(),
            });
        }

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
        let mut provider_responses = self.run_providers_parallel(request, context).await?;
        let normalized = self.normalize_provider_responses(request, &mut provider_responses);

        let floor_prices = self.floor_prices_by_slot(request);
        let mut mediation_failed = false;
        let mut mediator_response = None;
        let mut winning_bids = None;

        if let Some(mediator_name) = &self.config.mediator {
            if let Some(mediator) = self.providers.get(mediator_name) {
                log::info!(
                    "Sending {} provider responses to mediator: {}",
                    provider_responses.len(),
                    mediator.provider_name()
                );
                let remaining_ms = remaining_budget_ms(mediation_start, context.timeout_ms);
                if remaining_ms == 0 {
                    log::warn!("Auction timeout exhausted during bidding phase; skipping mediator");
                    mediation_failed = true;
                } else {
                    let mediator_context = AuctionContext {
                        settings: context.settings,
                        request: context.request,
                        timeout_ms: context
                            .services
                            .backend()
                            .canonicalize_transport_timeout_ms(remaining_ms, mediator.timeout_ms()),
                        provider_responses: Some(&provider_responses),
                        services: context.services,
                    };
                    let start_time = Instant::now();
                    let raw_response = match mediator.request_bids(request, &mediator_context).await
                    {
                        Ok(ProviderRequestOutcome::Immediate(response)) => Some(response),
                        Ok(ProviderRequestOutcome::Pending {
                            request: pending,
                            parse_state,
                        }) => match mediator_context.services.http_client().wait(pending).await {
                            Ok(platform_response) => mediator
                                .parse_response_with_context_and_state(
                                    platform_response,
                                    start_time.elapsed().as_millis() as u64,
                                    request,
                                    &mediator_context,
                                    parse_state.as_deref(),
                                )
                                .await
                                .inspect_err(|error| {
                                    log::warn!(
                                        "Mediator '{}' parse failed: {error:?}",
                                        mediator.provider_name()
                                    );
                                })
                                .ok(),
                            Err(error) => {
                                log::warn!(
                                    "Mediator '{}' request failed: {error:?}",
                                    mediator.provider_name()
                                );
                                None
                            }
                        },
                        Err(error) => {
                            log::warn!(
                                "Mediator '{}' failed to launch: {error:?}",
                                mediator.provider_name()
                            );
                            None
                        }
                    };

                    if let Some(raw_response) = raw_response {
                        mediator_response = Some(raw_response.clone());
                        match Self::resolve_mediator_candidates(
                            raw_response,
                            &normalized.candidates,
                        ) {
                            Ok(resolved) => {
                                let selected = resolved
                                    .bids
                                    .iter()
                                    .map(|bid| (bid.slot_id.clone(), bid.clone()))
                                    .collect();
                                winning_bids =
                                    Some(self.apply_floor_prices(selected, &floor_prices));
                                mediator_response = Some(resolved);
                            }
                            Err(()) => {
                                log::warn!(
                                    "Mediator '{}' returned invalid candidate provenance",
                                    mediator.provider_name()
                                );
                                mediation_failed = true;
                            }
                        }
                    } else {
                        mediation_failed = true;
                    }
                }
            } else {
                log::warn!("Mediator '{}' not registered", mediator_name);
                mediation_failed = true;
            }
        }

        let winning_bids = winning_bids
            .unwrap_or_else(|| self.select_winning_bids(&provider_responses, &floor_prices));
        let decision_set = self.build_decision_set(
            request,
            &normalized.outcomes,
            &winning_bids,
            mediation_failed,
        );

        Ok(OrchestrationResult {
            provider_responses,
            mediator_response,
            winning_bids,
            decision_set,
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
        let mut provider_responses = self.run_providers_parallel(request, context).await?;
        let normalized = self.normalize_provider_responses(request, &mut provider_responses);
        let floor_prices = self.floor_prices_by_slot(request);
        let winning_bids = self.select_winning_bids(&provider_responses, &floor_prices);
        let decision_set =
            self.build_decision_set(request, &normalized.outcomes, &winning_bids, false);

        Ok(OrchestrationResult {
            provider_responses,
            mediator_response: None,
            winning_bids,
            decision_set,
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
            return Ok(Vec::new());
        }

        // Reject multi-provider fan-out before any request launches when the
        // platform executes `send_async` eagerly (e.g. Cloudflare Workers):
        // sequential execution would accrue the sum of provider latencies and
        // blow the auction budget before a later `select` could reject it.
        if provider_names.len() > 1 && !context.services.http_client().supports_concurrent_fanout()
        {
            log::warn!(
                "{} auction providers configured, but this platform's HTTP client executes requests sequentially",
                provider_names.len(),
            );
            return Ok(provider_names
                .iter()
                .map(|provider_name| provider_launch_failed_response(provider_name, 0))
                .collect());
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
                responses.push(provider_timeout_response(provider.provider_name(), 0));
                continue;
            }

            // Pre-launch guard: `request_bids` fires the outbound send, and
            // discarding the returned pending handle afterwards does not retract
            // it. If another provider this auction already claimed the predicted
            // backend name, skip *before* dispatching so a duplicate never hits
            // the wire. The post-launch check below stays as a defense for a
            // provider that resolves to an unexpected name.
            if let Some(predicted) = provider.backend_name(context.services, effective_timeout)
                && backend_to_provider.contains_key(&predicted)
            {
                log::warn!(
                    "Provider '{}' predicted backend name '{}' already belongs to another provider; skipping launch",
                    provider.provider_name(),
                    predicted,
                );
                responses.push(provider_launch_failed_response(provider.provider_name(), 0));
                continue;
            }

            let provider_context = AuctionContext {
                settings: context.settings,
                request: context.request,
                timeout_ms: effective_timeout,
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
                        provider.backend_name(context.services, effective_timeout).inspect(|name| {
                            log::warn!(
                                "Provider '{}' pending request returned no backend name; using predicted name '{}'",
                                provider.provider_name(),
                                name,
                            );
                        })
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
                    // Post-launch defense: a resolved backend name already
                    // claimed by another provider would misattribute that
                    // provider's response, so fail this launch attributably
                    // instead of overwriting the correlation entry.
                    if backend_to_provider.contains_key(&request_backend_name) {
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
                    log::debug!(
                        "Provider '{}' completed without an upstream request",
                        provider.provider_name()
                    );
                    responses.push(canonical_provider_response(
                        provider.provider_name(),
                        response,
                    ));
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
            return Ok(responses);
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
        // Backend first-byte and between-bytes timeouts are capped to the
        // remaining auction budget in Phase 1. They are transport timers, not
        // absolute wall-clock limits, so connection setup and byte trickling
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
                        let provider_context = AuctionContext {
                            settings: context.settings,
                            request: context.request,
                            timeout_ms: state.effective_timeout_ms,
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
                                responses.push(canonical_provider_response(
                                    &state.provider_name,
                                    auction_response,
                                ));
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
                    Some(current_winner) => current_winner.price.is_none_or(|current_price| {
                        bid_price > current_price
                            || (bid_price == current_price
                                && (
                                    bid.candidate_provider.as_deref().unwrap_or(&bid.bidder),
                                    bid.bid_id.as_deref().unwrap_or_default(),
                                ) < (
                                    current_winner
                                        .candidate_provider
                                        .as_deref()
                                        .unwrap_or(&current_winner.bidder),
                                    current_winner.bid_id.as_deref().unwrap_or_default(),
                                ))
                    }),
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
        let mut backend_to_provider: HashMap<String, ProviderLaunchState> = HashMap::new();
        let mut pending_requests: Vec<PlatformPendingRequest> = Vec::new();
        let mut completed_responses: Vec<AuctionResponse> = Vec::new();
        let mut immediate_response_count = 0usize;

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

            // Pre-launch guard: skip before `request_bids` fires the outbound
            // send when another provider this auction already claimed the
            // predicted backend name (see the parallel path). Dropping the
            // pending handle afterwards would not retract the request.
            if let Some(predicted) = provider.backend_name(context.services, effective_timeout)
                && backend_to_provider.contains_key(&predicted)
            {
                log::warn!(
                    "Provider '{}' predicted backend name '{}' already belongs to another provider; skipping dispatch",
                    provider.provider_name(),
                    predicted,
                );
                completed_responses
                    .push(provider_launch_failed_response(provider.provider_name(), 0));
                continue;
            }

            let provider_context = AuctionContext {
                settings: context.settings,
                request: context.request,
                timeout_ms: effective_timeout,
                provider_responses: context.provider_responses,
                services: context.services,
            };

            let start_time = Instant::now();
            match provider.request_bids(request, &provider_context).await {
                Ok(ProviderRequestOutcome::Pending {
                    request: pending,
                    parse_state,
                }) => {
                    let backend_name = pending
                        .backend_name()
                        .map(str::to_string)
                        .or_else(|| provider.backend_name(context.services, effective_timeout));
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
                    // Post-launch defense: a resolved backend name already
                    // claimed by another provider would misattribute that
                    // provider's response, so fail this dispatch attributably
                    // instead of overwriting the correlation entry.
                    if backend_to_provider.contains_key(&backend_name) {
                        log::warn!(
                            "Provider '{}' resolved to backend name '{}' already claimed by another \
                             provider this auction; skipping launch to avoid response misattribution",
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
                    completed_responses.push(canonical_provider_response(
                        provider.provider_name(),
                        response,
                    ));
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
            completed_responses,
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
            completed_responses,
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

        let mut responses: Vec<AuctionResponse> = completed_responses;
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
                    if let Some(state) = backend_to_provider.remove(&backend_name) {
                        let response_time_ms = state.started_at.elapsed().as_millis() as u64;
                        let provider_context = AuctionContext {
                            settings: context.settings,
                            request: &provider_request_context,
                            timeout_ms: state.effective_timeout_ms,
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
                            Ok(auction_response) => {
                                log::info!(
                                    "Provider '{}' returned {} bids ({}ms)",
                                    auction_response.provider,
                                    auction_response.bids.len(),
                                    auction_response.response_time_ms
                                );
                                responses.push(canonical_provider_response(
                                    &state.provider_name,
                                    auction_response,
                                ));
                            }
                            Err(e) => {
                                log::warn!(
                                    "Provider '{}' parse failed: {:?}",
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
        let normalized = self.normalize_provider_responses(&request, &mut responses);
        let mut mediation_failed = false;
        let mut mediator_response = None;
        let mut mediated_winners = None;

        if let Some(mediator_name) = &self.config.mediator {
            if let Some(mediator) = self.providers.get(mediator_name.as_str()) {
                let remaining = remaining_budget_ms(auction_start, timeout_ms);
                if remaining == 0 {
                    log::warn!(
                        "A_deadline exhausted before mediator '{}' — using direct fallback",
                        mediator.provider_name(),
                    );
                    mediation_failed = true;
                } else {
                    let mediator_timeout = services
                        .backend()
                        .canonicalize_transport_timeout_ms(remaining, mediator.timeout_ms());
                    let mediator_start = Instant::now();
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
                    let raw_response =
                        match mediator.request_bids(&request, &mediator_context).await {
                            Ok(ProviderRequestOutcome::Immediate(response)) => Some(response),
                            Ok(ProviderRequestOutcome::Pending {
                                request: pending,
                                parse_state,
                            }) => match services.http_client().wait(pending).await {
                                Ok(platform_response) => mediator
                                    .parse_response_with_context_and_state(
                                        platform_response,
                                        mediator_start.elapsed().as_millis() as u64,
                                        &request,
                                        &mediator_context,
                                        parse_state.as_deref(),
                                    )
                                    .await
                                    .inspect_err(|error| {
                                        log::warn!(
                                            "Mediator '{}' parse failed: {error:?}",
                                            mediator.provider_name()
                                        );
                                    })
                                    .ok(),
                                Err(error) => {
                                    log::warn!("Mediator request failed: {error:?}");
                                    None
                                }
                            },
                            Err(error) => {
                                log::warn!(
                                    "Mediator '{}' failed to dispatch: {error:?}",
                                    mediator.provider_name()
                                );
                                None
                            }
                        };
                    if let Some(raw_response) = raw_response {
                        mediator_response = Some(raw_response.clone());
                        match Self::resolve_mediator_candidates(
                            raw_response,
                            &normalized.candidates,
                        ) {
                            Ok(resolved) => {
                                let selected = resolved
                                    .bids
                                    .iter()
                                    .map(|bid| (bid.slot_id.clone(), bid.clone()))
                                    .collect();
                                mediated_winners =
                                    Some(self.apply_floor_prices(selected, &floor_prices));
                                mediator_response = Some(resolved);
                            }
                            Err(()) => mediation_failed = true,
                        }
                    } else {
                        mediation_failed = true;
                    }
                }
            } else {
                log::warn!("Mediator '{}' not registered", mediator_name);
                mediation_failed = true;
            }
        }

        let winning_bids =
            mediated_winners.unwrap_or_else(|| self.select_winning_bids(&responses, &floor_prices));
        let decision_set = self.build_decision_set(
            &request,
            &normalized.outcomes,
            &winning_bids,
            mediation_failed,
        );

        OrchestrationResult {
            provider_responses: responses,
            mediator_response,
            winning_bids,
            decision_set,
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
    /// Exact ordered decision for every requested slot.
    pub decision_set: AuctionDecisionSetV1,
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
    use crate::auction::provider::{
        AuctionProvider, ProviderRequestOutcome, ProviderSlotDisposition,
    };
    use crate::auction::test_support::create_test_auction_context;
    use crate::auction::types::{
        AdFormat, AdSlot, ApsRendererV1, ApsTagType, AuctionContext, AuctionDropReason,
        AuctionRequest, AuctionResponse, AuctionSlotFailureReason, Bid, BidRenderSourceV1,
        BidStatus, MediaType, PublisherInfo, SlotAuctionDecisionV1, UserInfo,
    };
    use crate::error::TrustedServerError;
    use crate::platform::test_support::{
        StubHttpClient, build_services_with_backend_and_http_client,
        build_services_with_http_client, noop_services,
    };
    use crate::platform::{
        PlatformBackend, PlatformBackendSpec, PlatformError, PlatformHttpRequest, PlatformResponse,
        RuntimeServices,
    };
    use crate::test_support::tests::crate_test_settings_str;
    use error_stack::{Report, ResultExt};
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{AuctionIdentityGenerator, AuctionOrchestrator};

    // ---------------------------------------------------------------------------
    // Minimal test double for AuctionProvider
    // ---------------------------------------------------------------------------

    struct StubAuctionProvider {
        name: &'static str,
        backend: &'static str,
        configured_timeout_ms: u32,
        predicted_timeouts: Option<Arc<Mutex<Vec<u32>>>>,
        request_timeouts: Option<Arc<Mutex<Vec<u32>>>>,
    }

    impl StubAuctionProvider {
        fn new(name: &'static str, backend: &'static str) -> Self {
            Self {
                name,
                backend,
                configured_timeout_ms: 125,
                predicted_timeouts: None,
                request_timeouts: None,
            }
        }

        fn recording(
            name: &'static str,
            backend: &'static str,
            configured_timeout_ms: u32,
            predicted_timeouts: Arc<Mutex<Vec<u32>>>,
            request_timeouts: Arc<Mutex<Vec<u32>>>,
        ) -> Self {
            Self {
                name,
                backend,
                configured_timeout_ms,
                predicted_timeouts: Some(predicted_timeouts),
                request_timeouts: Some(request_timeouts),
            }
        }

        fn record(slot: &Option<Arc<Mutex<Vec<u32>>>>, timeout_ms: u32) {
            if let Some(observed) = slot {
                observed
                    .lock()
                    .expect("should lock observed timeouts")
                    .push(timeout_ms);
            }
        }
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
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            Self::record(&self.request_timeouts, context.timeout_ms);
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
            self.configured_timeout_ms
        }

        fn backend_name(&self, _services: &RuntimeServices, timeout_ms: u32) -> Option<String> {
            Self::record(&self.predicted_timeouts, timeout_ms);
            Some(self.backend.to_string())
        }
    }

    /// Provider whose `backend_name` prediction deliberately differs from the
    /// backend name its `request_bids` puts on the wire.
    struct DivergentBackendProvider {
        name: &'static str,
        predicted: &'static str,
        resolved: &'static str,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for DivergentBackendProvider {
        fn provider_name(&self) -> &'static str {
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
                    .expect("should build divergent request"),
                self.resolved,
            );
            context
                .services
                .http_client()
                .send_async(req)
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
    ) -> StubAuctionProvider {
        StubAuctionProvider::recording(
            name,
            backend,
            configured_timeout_ms,
            Arc::clone(predicted),
            Arc::clone(requested),
        )
    }

    /// Mediator whose context-aware parse restores `nurl`/`ad_id` (mirroring
    /// `adserver_mock`), while its context-free parse does not. Lets a test prove
    /// the synchronous mediation path calls `parse_response_with_context`.
    struct CacheRestoringMediator;

    fn auction_bid(bidder: &str, price: f64) -> Bid {
        let renderer = (bidder == "aps").then(|| {
            BidRenderSourceV1::Aps(ApsRendererV1 {
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
            candidate_id: None,
            candidate_provider: None,
            renderer_reservation_id: None,
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

    struct CounterIdentityGenerator {
        draws: AtomicUsize,
    }

    impl CounterIdentityGenerator {
        fn new() -> Self {
            Self {
                draws: AtomicUsize::new(0),
            }
        }
    }

    impl AuctionIdentityGenerator for CounterIdentityGenerator {
        fn fill(&self, destination: &mut [u8]) -> Result<(), ()> {
            destination.fill(0);
            let draw = self.draws.fetch_add(1, Ordering::SeqCst) + 1;
            let last = destination.last_mut().ok_or(())?;
            *last = u8::try_from(draw).map_err(|_| ())?;
            Ok(())
        }
    }

    struct FixedIdentityGenerator {
        draws: AtomicUsize,
    }

    impl FixedIdentityGenerator {
        fn new() -> Self {
            Self {
                draws: AtomicUsize::new(0),
            }
        }
    }

    impl AuctionIdentityGenerator for FixedIdentityGenerator {
        fn fill(&self, destination: &mut [u8]) -> Result<(), ()> {
            self.draws.fetch_add(1, Ordering::SeqCst);
            destination.fill(0);
            Ok(())
        }
    }

    fn mediated_bid(nurl: Option<String>) -> Bid {
        Bid {
            slot_id: "header-banner".to_string(),
            candidate_id: None,
            candidate_provider: None,
            renderer_reservation_id: None,
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

    struct SourceBidProvider {
        nurl: &'static str,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for SourceBidProvider {
        fn provider_name(&self) -> &'static str {
            "bidder"
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
                    .expect("should build source bid request"),
                "bidder-backend",
            );
            context
                .services
                .http_client()
                .send_async(request)
                .await
                .change_context(TrustedServerError::Auction {
                    message: "source bidder launch failed".to_string(),
                })
                .map(ProviderRequestOutcome::pending)
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            let mut bid = mediated_bid(Some(self.nurl.to_string()));
            bid.price = Some(1.0);
            bid.bid_id = Some("source-bid-id".to_string());
            Ok(AuctionResponse::success(
                self.provider_name(),
                vec![bid],
                response_time_ms,
            ))
        }

        fn timeout_ms(&self) -> u32 {
            2000
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("bidder-backend".to_string())
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
            context: &AuctionContext<'_>,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            let mut selection = context
                .provider_responses
                .and_then(|responses| responses.first())
                .and_then(|response| response.bids.first())
                .cloned()
                .expect("should provide one source candidate to mediator");
            selection.price = Some(2.5);
            Ok(AuctionResponse::success(
                "mediator",
                vec![selection],
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
        fn provider_name(&self) -> &'static str {
            "immediate-mediator"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            let mut selection = context
                .provider_responses
                .and_then(|responses| responses.first())
                .and_then(|response| response.bids.first())
                .cloned()
                .expect("should provide one source candidate to immediate mediator");
            selection.price = Some(2.5);
            Ok(ProviderRequestOutcome::Immediate(AuctionResponse::success(
                self.provider_name(),
                vec![selection],
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
            providers: vec!["bidder".to_string()],
            mediator: Some("mediator".to_string()),
            timeout_ms: 2000,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(SourceBidProvider {
            nurl: "https://nurl.example/win",
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

    #[tokio::test]
    async fn immediate_mediator_completes_in_sync_and_split_paths() {
        for split in [false, true] {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            let services = build_services_with_http_client(stub);
            let config = AuctionConfig {
                enabled: true,
                providers: vec!["bidder".to_string()],
                mediator: Some("immediate-mediator".to_string()),
                timeout_ms: 2000,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(SourceBidProvider {
                nurl: "https://nurl.example/immediate",
            }));
            orchestrator.register_provider(Arc::new(ImmediateMediator));
            let request = create_test_auction_request();
            let settings = create_test_settings();
            let downstream = http::Request::new(edgezero_core::body::Body::empty());
            let context = AuctionContext {
                settings: &settings,
                request: &downstream,
                timeout_ms: 2000,
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

    fn one_slot_request() -> AuctionRequest {
        let mut request = create_test_auction_request();
        request.slots = vec![AdSlot {
            id: "slot-1".to_string(),
            formats: vec![AdFormat {
                media_type: MediaType::Banner,
                width: 300,
                height: 250,
            }],
            floor_price: None,
            targeting: HashMap::new(),
            bidders: HashMap::new(),
        }];
        request
    }

    fn enabled_config(providers: &[&str]) -> AuctionConfig {
        AuctionConfig {
            enabled: true,
            providers: providers
                .iter()
                .map(|provider| (*provider).to_string())
                .collect(),
            ..AuctionConfig::default()
        }
    }

    #[test]
    fn normalized_provider_outcomes_cover_every_dispatched_slot() {
        let generator = Arc::new(CounterIdentityGenerator::new());
        let mut orchestrator =
            AuctionOrchestrator::with_identity_generator(enabled_config(&["alpha"]), generator);
        orchestrator.register_provider(Arc::new(StubAuctionProvider::new("alpha", "alpha")));
        let request = one_slot_request();
        let mut candidate = auction_bid("aps", 2.0);
        candidate.slot_id = "slot-1".to_string();
        let mut responses = vec![AuctionResponse::success("alpha", vec![candidate], 10)];

        let normalized = orchestrator.normalize_provider_responses(&request, &mut responses);

        assert_eq!(normalized.outcomes.len(), 1);
        assert_eq!(normalized.outcomes[0].provider, "alpha");
        assert_eq!(normalized.outcomes[0].slot, "slot-1");
        assert!(matches!(
            &normalized.outcomes[0].disposition,
            ProviderSlotDisposition::Candidates(candidates)
                if candidates.len() == 1
                    && candidates[0].candidate_id.as_deref().is_some_and(|id| id.len() == 12)
        ));

        let mut no_bid = vec![AuctionResponse::no_bid("alpha", 10)];
        let normalized = orchestrator.normalize_provider_responses(&request, &mut no_bid);
        assert!(matches!(
            normalized.outcomes[0].disposition,
            ProviderSlotDisposition::NoBid
        ));

        let mut timeout = vec![super::provider_timeout_response("alpha", 10)];
        let normalized = orchestrator.normalize_provider_responses(&request, &mut timeout);
        assert!(matches!(
            normalized.outcomes[0].disposition,
            ProviderSlotDisposition::Failed(AuctionSlotFailureReason::ProviderTimeout)
        ));
    }

    #[test]
    fn provider_failure_classes_map_to_closed_slot_reasons() {
        let mut orchestrator = AuctionOrchestrator::new(enabled_config(&["alpha"]));
        orchestrator.register_provider(Arc::new(StubAuctionProvider::new("alpha", "alpha")));
        let request = one_slot_request();

        for (error_type, expected) in [
            (
                super::ERROR_TYPE_LAUNCH_FAILED,
                AuctionSlotFailureReason::ProviderError,
            ),
            (
                super::ERROR_TYPE_TRANSPORT,
                AuctionSlotFailureReason::ProviderError,
            ),
            (
                super::ERROR_TYPE_HTTP_STATUS,
                AuctionSlotFailureReason::ProviderError,
            ),
            (
                super::ERROR_TYPE_PARSE_RESPONSE,
                AuctionSlotFailureReason::InvalidProviderResponse,
            ),
        ] {
            let error = Report::new(TrustedServerError::Auction {
                message: "provider failed".to_string(),
            });
            let mut responses = vec![super::provider_error_response(
                "alpha", 1, error_type, &error,
            )];
            let normalized = orchestrator.normalize_provider_responses(&request, &mut responses);
            assert!(matches!(
                normalized.outcomes[0].disposition,
                ProviderSlotDisposition::Failed(reason) if reason == expected
            ));
        }
    }

    #[test]
    fn candidate_collision_exhaustion_fails_only_the_affected_slot() {
        let generator = Arc::new(FixedIdentityGenerator::new());
        let mut orchestrator = AuctionOrchestrator::with_identity_generator(
            enabled_config(&["alpha"]),
            generator.clone(),
        );
        orchestrator.register_provider(Arc::new(StubAuctionProvider::new("alpha", "alpha")));
        let mut request = one_slot_request();
        request.slots.push(AdSlot {
            id: "slot-2".to_string(),
            formats: request.slots[0].formats.clone(),
            floor_price: None,
            targeting: HashMap::new(),
            bidders: HashMap::new(),
        });
        let mut first = auction_bid("aps", 2.0);
        first.slot_id = "slot-1".to_string();
        first.bid_id = Some("upstream-1".to_string());
        let mut second = auction_bid("aps", 1.0);
        second.slot_id = "slot-2".to_string();
        second.bid_id = Some("upstream-2".to_string());
        let mut responses = vec![AuctionResponse::success("alpha", vec![first, second], 10)];

        let normalized = orchestrator.normalize_provider_responses(&request, &mut responses);

        assert_eq!(generator.draws.load(Ordering::SeqCst), 10);
        assert!(matches!(
            normalized.outcomes[0].disposition,
            ProviderSlotDisposition::Candidates(_)
        ));
        assert!(matches!(
            normalized.outcomes[1].disposition,
            ProviderSlotDisposition::Failed(AuctionSlotFailureReason::InternalError)
        ));
    }

    #[test]
    fn candidate_collision_exhaustion_discards_earlier_sibling_for_same_slot() {
        let generator = Arc::new(FixedIdentityGenerator::new());
        let mut orchestrator = AuctionOrchestrator::with_identity_generator(
            enabled_config(&["alpha"]),
            generator.clone(),
        );
        orchestrator.register_provider(Arc::new(StubAuctionProvider::new("alpha", "alpha")));
        let request = one_slot_request();
        let mut first = auction_bid("aps", 2.0);
        first.bid_id = Some("upstream-1".to_string());
        let mut second = auction_bid("aps", 1.0);
        second.bid_id = Some("upstream-2".to_string());
        let mut responses = vec![AuctionResponse::success("alpha", vec![first, second], 10)];

        let normalized = orchestrator.normalize_provider_responses(&request, &mut responses);

        assert_eq!(generator.draws.load(Ordering::SeqCst), 10);
        assert!(responses[0].bids.is_empty());
        assert!(normalized.candidates.is_empty());
        assert!(matches!(
            normalized.outcomes[0].disposition,
            ProviderSlotDisposition::Failed(AuctionSlotFailureReason::InternalError)
        ));
    }

    #[test]
    fn per_bid_drop_does_not_poison_an_unrelated_missing_slot() {
        let mut orchestrator = AuctionOrchestrator::new(enabled_config(&["alpha"]));
        orchestrator.register_provider(Arc::new(StubAuctionProvider::new("alpha", "alpha")));
        let mut request = one_slot_request();
        request.slots.push(AdSlot {
            id: "slot-2".to_string(),
            formats: request.slots[0].formats.clone(),
            floor_price: None,
            targeting: HashMap::new(),
            bidders: HashMap::new(),
        });
        let mut valid = auction_bid("aps", 2.0);
        valid.bid_id = Some("upstream-1".to_string());
        let mut response = AuctionResponse::success("alpha", vec![valid], 10);
        response = response.with_drop_reason(AuctionDropReason::InvalidDimensions);
        let mut responses = vec![response];

        let normalized = orchestrator.normalize_provider_responses(&request, &mut responses);

        assert!(matches!(
            normalized.outcomes[0].disposition,
            ProviderSlotDisposition::Candidates(_)
        ));
        assert!(matches!(
            normalized.outcomes[1].disposition,
            ProviderSlotDisposition::NoBid
        ));
    }

    #[test]
    fn final_decisions_are_request_ordered_and_use_closed_failure_priority() {
        let mut orchestrator = AuctionOrchestrator::new(enabled_config(&["alpha", "zeta"]));
        orchestrator.register_provider(Arc::new(StubAuctionProvider::new("alpha", "alpha")));
        orchestrator.register_provider(Arc::new(StubAuctionProvider::new("zeta", "zeta")));
        let request = one_slot_request();
        let outcomes = vec![
            crate::auction::provider::ProviderSlotOutcome {
                provider: "alpha".to_string(),
                slot: "slot-1".to_string(),
                disposition: ProviderSlotDisposition::Failed(
                    AuctionSlotFailureReason::ProviderTimeout,
                ),
            },
            crate::auction::provider::ProviderSlotOutcome {
                provider: "zeta".to_string(),
                slot: "slot-1".to_string(),
                disposition: ProviderSlotDisposition::Failed(
                    AuctionSlotFailureReason::InvalidProviderResponse,
                ),
            },
        ];

        let decisions = orchestrator.build_decision_set(&request, &outcomes, &HashMap::new(), true);

        assert_eq!(decisions.results.len(), 1);
        assert!(matches!(
            &decisions.results[0],
            SlotAuctionDecisionV1::Failed { slot, reason }
                if slot == "slot-1" && *reason == AuctionSlotFailureReason::MediationFailed
        ));
        assert_eq!(
            serde_json::to_string(&decisions).expect("decision set should serialize"),
            r#"{"version":1,"auctionId":"test-auction-123","results":[{"slot":"slot-1","outcome":"failed","reason":"mediation_failed"}]}"#
        );
        assert_eq!(
            serde_json::to_string(&SlotAuctionDecisionV1::Failed {
                slot: "slot-1".to_string(),
                reason: AuctionSlotFailureReason::IdentityGenerationFailed,
            })
            .expect("direct identity-generation failure should serialize"),
            r#"{"slot":"slot-1","outcome":"failed","reason":"identity_generation_failed"}"#
        );
    }

    #[test]
    fn deliverable_winner_beats_a_sibling_provider_failure() {
        let mut orchestrator = AuctionOrchestrator::new(enabled_config(&["alpha", "zeta"]));
        orchestrator.register_provider(Arc::new(StubAuctionProvider::new("alpha", "alpha")));
        orchestrator.register_provider(Arc::new(StubAuctionProvider::new("zeta", "zeta")));
        let request = one_slot_request();
        let mut winner = auction_bid("alpha-seat", 2.0);
        winner.candidate_id = Some("AAAAAAAAAAAA".to_string());
        winner.candidate_provider = Some("alpha".to_string());
        winner.bid_id = Some("upstream-alpha".to_string());
        let outcomes = vec![crate::auction::provider::ProviderSlotOutcome {
            provider: "zeta".to_string(),
            slot: "slot-1".to_string(),
            disposition: ProviderSlotDisposition::Failed(AuctionSlotFailureReason::ProviderTimeout),
        }];

        let decisions = orchestrator.build_decision_set(
            &request,
            &outcomes,
            &HashMap::from([("slot-1".to_string(), winner)]),
            true,
        );

        assert_eq!(
            decisions.results,
            vec![SlotAuctionDecisionV1::Winner {
                slot: "slot-1".to_string(),
                candidate_id: "AAAAAAAAAAAA".to_string(),
            }]
        );
    }

    #[test]
    fn direct_ties_ignore_arrival_and_candidate_ids() {
        let orchestrator = AuctionOrchestrator::new(enabled_config(&["alpha", "zeta"]));
        let mut alpha = auction_bid("seat-a", 2.0);
        alpha.candidate_provider = Some("alpha".to_string());
        alpha.candidate_id = Some("zzzzzzzzzzzz".to_string());
        alpha.bid_id = Some("upstream-z".to_string());
        let mut zeta = auction_bid("seat-z", 2.0);
        zeta.candidate_provider = Some("zeta".to_string());
        zeta.candidate_id = Some("AAAAAAAAAAAA".to_string());
        zeta.bid_id = Some("upstream-a".to_string());
        let left = AuctionResponse::success("alpha", vec![alpha], 1);
        let right = AuctionResponse::success("zeta", vec![zeta], 1);

        for responses in [vec![left.clone(), right.clone()], vec![right, left]] {
            let winners = orchestrator.select_winning_bids(&responses, &HashMap::new());
            assert_eq!(
                winners["slot-1"].candidate_provider.as_deref(),
                Some("alpha")
            );
        }
    }

    #[test]
    fn mediator_can_select_only_known_candidate_provenance() {
        let mut source = auction_bid("aps", 1.0);
        source.candidate_id = Some("AAAAAAAAAAAA".to_string());
        source.candidate_provider = Some("aps".to_string());
        source.nurl = Some("https://source.example/win".to_string());
        let candidates = HashMap::from([("AAAAAAAAAAAA".to_string(), source.clone())]);
        let mut selection = source.clone();
        selection.price = Some(9.0);

        let resolved = AuctionOrchestrator::resolve_mediator_candidates(
            AuctionResponse::success("mediator", vec![selection], 2),
            &candidates,
        )
        .expect("known candidate should resolve");
        assert_eq!(resolved.bids[0].price, Some(9.0));
        assert_eq!(resolved.bids[0].width, source.width);
        assert_eq!(resolved.bids[0].height, source.height);
        assert_eq!(resolved.bids[0].renderer, source.renderer);
        assert_eq!(resolved.bids[0].nurl, source.nurl);

        let mut substituted = source.clone();
        substituted.price = Some(9.0);
        substituted.width = 1;
        assert!(
            AuctionOrchestrator::resolve_mediator_candidates(
                AuctionResponse::success("mediator", vec![substituted], 2),
                &candidates,
            )
            .is_err(),
            "mediator source-field substitutions should fail provenance validation"
        );

        let mut second_source = source.clone();
        second_source.candidate_id = Some("BBBBBBBBBBBB".to_string());
        second_source.bid_id = Some("upstream-2".to_string());
        let same_slot_candidates = HashMap::from([
            ("AAAAAAAAAAAA".to_string(), source.clone()),
            ("BBBBBBBBBBBB".to_string(), second_source.clone()),
        ]);
        assert!(
            AuctionOrchestrator::resolve_mediator_candidates(
                AuctionResponse::success("mediator", vec![source.clone(), second_source], 2),
                &same_slot_candidates,
            )
            .is_err(),
            "a mediator may select at most one candidate for a slot"
        );

        let mut unknown = source;
        unknown.candidate_id = Some("BBBBBBBBBBBB".to_string());
        assert!(
            AuctionOrchestrator::resolve_mediator_candidates(
                AuctionResponse::success("mediator", vec![unknown], 2),
                &candidates,
            )
            .is_err()
        );
    }

    fn create_test_settings() -> crate::settings::Settings {
        let settings_str = crate_test_settings_str();
        crate::settings::Settings::from_toml(&settings_str).expect("should parse test settings")
    }

    struct ImmediateNoBidProvider;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for ImmediateNoBidProvider {
        fn provider_name(&self) -> &'static str {
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
        fn provider_name(&self) -> &'static str {
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
            provider_responses: None,
            services,
        }
    }

    #[tokio::test]
    async fn synchronous_auction_accepts_an_all_immediate_no_bid_result() {
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["immediate".to_string()],
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
        assert_eq!(
            result.decision_set.results,
            vec![
                SlotAuctionDecisionV1::NoBid {
                    slot: "header-banner".to_string(),
                },
                SlotAuctionDecisionV1::NoBid {
                    slot: "sidebar".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn split_auction_accepts_an_all_immediate_no_bid_result() {
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["immediate".to_string()],
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
        assert_eq!(
            result.decision_set.results,
            vec![
                SlotAuctionDecisionV1::NoBid {
                    slot: "header-banner".to_string(),
                },
                SlotAuctionDecisionV1::NoBid {
                    slot: "sidebar".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn immediate_and_pending_providers_complete_in_sync_and_split_paths() {
        for split in [false, true] {
            let config = AuctionConfig {
                enabled: true,
                providers: vec!["immediate".to_string(), "pending".to_string()],
                timeout_ms: 2000,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(ImmediateNoBidProvider));
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "pending",
                "pending-backend",
            )));
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
                candidate_id: None,
                candidate_provider: None,
                renderer_reservation_id: None,
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
                candidate_id: None,
                candidate_provider: None,
                renderer_reservation_id: None,
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

            let result = result.expect("should return one decision per requested slot");
            assert_eq!(
                result.decision_set.results,
                vec![
                    SlotAuctionDecisionV1::Failed {
                        slot: "header-banner".to_string(),
                        reason: AuctionSlotFailureReason::SlotNotEligible,
                    },
                    SlotAuctionDecisionV1::Failed {
                        slot: "sidebar".to_string(),
                        reason: AuctionSlotFailureReason::SlotNotEligible,
                    },
                ]
            );
        });
    }

    #[test]
    fn provider_launch_failures_are_explicit_when_no_requests_launch() {
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
            let result = result.expect("should preserve launch failures as slot decisions");
            assert_eq!(
                result.decision_set.results,
                vec![
                    SlotAuctionDecisionV1::Failed {
                        slot: "header-banner".to_string(),
                        reason: AuctionSlotFailureReason::ProviderError,
                    },
                    SlotAuctionDecisionV1::Failed {
                        slot: "sidebar".to_string(),
                        reason: AuctionSlotFailureReason::ProviderError,
                    },
                ]
            );
        });
    }

    #[test]
    fn rejects_duplicate_configured_providers() {
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["prebid".to_string(), "prebid".to_string()],
            timeout_ms: 2000,
            ..Default::default()
        };
        let err = AuctionOrchestrator::new(config)
            .validate_configured_provider_names()
            .expect_err("should reject a provider listed more than once");
        assert!(err.to_string().contains("listed more than once"));
    }

    #[test]
    fn rejects_mediator_also_listed_as_provider() {
        let config = AuctionConfig {
            enabled: true,
            providers: vec!["prebid".to_string()],
            mediator: Some("prebid".to_string()),
            timeout_ms: 2000,
            ..Default::default()
        };
        let err = AuctionOrchestrator::new(config)
            .validate_configured_provider_names()
            .expect_err("should reject a mediator also configured as a provider");
        assert!(err.to_string().contains("may not mediate its own auction"));
    }

    #[tokio::test]
    async fn duplicate_backend_name_fails_second_provider_attributably_in_both_paths() {
        for split in [false, true] {
            let config = AuctionConfig {
                enabled: true,
                providers: vec!["provider-a".to_string(), "provider-b".to_string()],
                timeout_ms: 2000,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "provider-a",
                "shared-backend",
            )));
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "provider-b",
                "shared-backend",
            )));
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
                providers: vec!["bidder".to_string()],
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
            // A platform that canonicalizes to zero signals "budget exhausted";
            // the orchestrator must skip the launch and retain an attributable
            // timeout decision for every eligible requested slot.
            let stub = Arc::new(StubHttpClient::new());
            let calls = Arc::new(Mutex::new(Vec::new()));
            let services = build_services_with_backend_and_http_client(
                Arc::new(CanonicalTimeoutBackend {
                    canonical_ms: 0,
                    calls,
                }),
                stub,
            );
            let predicted = Arc::new(Mutex::new(Vec::new()));
            let requested = Arc::new(Mutex::new(Vec::new()));
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: vec!["bidder".to_string()],
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
            let request = create_test_auction_request();

            let result = orchestrator
                .run_auction(&request, &context)
                .await
                .expect("should preserve an exhausted budget as slot decisions");
            assert_eq!(
                result.decision_set.results,
                vec![
                    SlotAuctionDecisionV1::Failed {
                        slot: "header-banner".to_string(),
                        reason: AuctionSlotFailureReason::ProviderTimeout,
                    },
                    SlotAuctionDecisionV1::Failed {
                        slot: "sidebar".to_string(),
                        reason: AuctionSlotFailureReason::ProviderTimeout,
                    },
                ]
            );
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
                providers: vec!["bidder".to_string()],
                mediator: Some("mediator".to_string()),
                timeout_ms: 2000,
                ..Default::default()
            });
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "bidder",
                "bidder-backend",
            )));
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
                providers: vec!["bidder".to_string()],
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
    fn dispatched_resolved_backend_name_diverging_from_prediction_still_correlates() {
        futures::executor::block_on(async {
            let stub = Arc::new(StubHttpClient::new());
            stub.push_response(200, b"{}".to_vec());
            let services = build_services_with_http_client(stub);
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: vec!["provider-a".to_string()],
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

            let provider_a = result
                .provider_responses
                .iter()
                .find(|response| response.provider == "provider-a")
                .expect("should have provider-a response");
            assert_eq!(
                provider_a.status,
                BidStatus::Success,
                "response should correlate by the resolved backend name, not the prediction"
            );
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
                providers: vec!["provider-a".to_string(), "provider-b".to_string()],
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

            let provider_a = result
                .provider_responses
                .iter()
                .find(|response| response.provider == "provider-a")
                .expect("should have provider-a response");
            let provider_b = result
                .provider_responses
                .iter()
                .find(|response| response.provider == "provider-b")
                .expect("should have provider-b response");
            assert_eq!(provider_a.status, BidStatus::Success);
            assert_eq!(provider_b.status, BidStatus::Error);
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
                providers: vec!["provider-a".to_string(), "provider-b".to_string()],
                timeout_ms: 2000,
                mediator: None,
                ..Default::default()
            };
            let mut orchestrator = AuctionOrchestrator::new(config);
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "provider-a",
                "backend-a",
            )));
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "provider-b",
                "backend-b",
            )));

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
            let mut provider = StubAuctionProvider::new("provider-a", "backend-a");
            provider.configured_timeout_ms = 125;
            orchestrator.register_provider(Arc::new(provider));
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
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "provider-a",
                "backend-a",
            )));
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "provider-b",
                "backend-b",
            )));

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

            // Assert: every affected slot gets an explicit provider failure
            // without launching either provider request.
            let result = result.expect("should preserve sequential-platform failures");
            assert_eq!(
                result.decision_set.results,
                vec![
                    SlotAuctionDecisionV1::Failed {
                        slot: "header-banner".to_string(),
                        reason: AuctionSlotFailureReason::ProviderError,
                    },
                    SlotAuctionDecisionV1::Failed {
                        slot: "sidebar".to_string(),
                        reason: AuctionSlotFailureReason::ProviderError,
                    },
                ]
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
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "provider-a",
                "backend-a",
            )));
            orchestrator.register_provider(Arc::new(StubAuctionProvider::new(
                "provider-b",
                "backend-b",
            )));

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
                candidate_id: None,
                candidate_provider: None,
                renderer_reservation_id: None,
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
                candidate_id: None,
                candidate_provider: None,
                renderer_reservation_id: None,
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
                candidate_id: None,
                candidate_provider: None,
                renderer_reservation_id: None,
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
