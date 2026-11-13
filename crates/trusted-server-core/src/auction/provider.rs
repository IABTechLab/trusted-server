//! Trait definition for auction providers.

use core::any::Any;
use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use edgezero_core::outbound::{
    OutboundCachePolicy, OutboundRequest, OutboundResponse, ResponseDecoding,
};
use error_stack::{Report, ResultExt as _};
use http::header::HeaderValue;
use http::{Method, StatusCode, header};
use serde_json::{Value, json};

use crate::integrations::aps::{ApsDebugRequest, parse_planned_aps_response};
use crate::integrations::prebid::{apply_prebid_transport_headers, parse_planned_prebid_response};

use crate::error::TrustedServerError;
use crate::outbound::{OutboundRequestSource, buffered, outbound_error, outbound_request};
use crate::request_signing::{RequestSigner, SigningParams};

use super::openrtb::{
    OpenRtbBuildOutcome, RequestFinalization, apply_notification_policy, build_request,
    extract_standard_response, unused_bidder_params_count,
};
use super::plan::ProviderPlan;
use super::profile::CompiledOpenRtbProfile;
use super::routing::{ProviderAuctionInput, RoutedAuction};
use super::types::{AuctionContext, AuctionRequest, AuctionResponse};

/// Response cap for every bidder request.
const MAX_PLANNED_RESPONSE_BYTES: u64 = 1024 * 1024;

fn attach_provider_routing_metadata(
    response: &mut AuctionResponse,
    profile: &CompiledOpenRtbProfile,
    input: &ProviderAuctionInput,
) {
    response.metadata.insert(
        "routing".to_string(),
        json!({"unused_bidder_params_count": unused_bidder_params_count(profile, input)}),
    );
}

/// Provider-local state carried from request dispatch to response parsing.
pub type ProviderParseState = Box<dyn Any + Send + Sync>;

/// Result of asking a provider to start a bid request.
#[allow(
    clippy::large_enum_variant,
    reason = "the outcome moves once from the provider to the orchestrator, so boxing the request would only add an allocation"
)]
pub enum ProviderRequestOutcome {
    /// An upstream request built and unsent, which the orchestrator sends.
    Ready {
        /// The request, with its transport settings, ready for `EdgeZero`'s client.
        request: OutboundRequest,
        /// Optional provider-local state consumed when the response is parsed.
        parse_state: Option<ProviderParseState>,
    },
    /// A complete provider response that required no upstream request.
    Immediate(AuctionResponse),
}

impl ProviderRequestOutcome {
    /// Wrap an unsent provider request without parse state.
    #[must_use]
    pub fn ready(request: OutboundRequest) -> Self {
        Self::Ready {
            request,
            parse_state: None,
        }
    }

    /// Wrap an unsent provider request with provider-local parse state.
    #[must_use]
    pub fn ready_with_state(request: OutboundRequest, parse_state: ProviderParseState) -> Self {
        Self::Ready {
            request,
            parse_state: Some(parse_state),
        }
    }
}

/// Trait implemented by all auction providers (Prebid, APS, GAM, etc.).
#[async_trait(?Send)]
pub trait AuctionProvider: Send + Sync {
    /// Borrow this provider instance's unique validated identifier.
    ///
    /// Legacy providers may return a string literal; config-first providers
    /// return their owned operator-defined [`super::plan::ProviderId`].
    fn provider_name(&self) -> &str;

    /// Build a bid request for this provider, without sending it.
    ///
    /// Implementations normally return [`ProviderRequestOutcome::Ready`], but may
    /// return an immediate response for a legitimate outcome that requires no
    /// HTTP call. The orchestrator sends the request and parses its response.
    /// Auction and mediator requests set [`OutboundCachePolicy::Bypass`],
    /// because they carry personalized data.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be built.
    async fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>>;

    /// Parse the response from the provider into an `AuctionResponse`.
    ///
    /// Called by the orchestrator once the provider's request completes.
    ///
    /// # Errors
    ///
    /// Returns an error if the response cannot be parsed into a valid `AuctionResponse`.
    async fn parse_response(
        &self,
        response: OutboundResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>>;

    /// Parse the response with access to the original auction request and context.
    ///
    /// Providers that need request-local metadata while transforming responses
    /// can override this method. `request` is the [`AuctionRequest`] the
    /// orchestrator dispatched, so request-scoped data (e.g. slot ID mappings)
    /// can be derived here instead of stored on the shared provider instance.
    /// The default preserves the existing response-only provider contract.
    ///
    /// # Errors
    ///
    /// Returns an error if the response cannot be parsed into a valid [`AuctionResponse`].
    async fn parse_response_with_context(
        &self,
        response: OutboundResponse,
        response_time_ms: u64,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let _ = (request, context);
        self.parse_response(response, response_time_ms).await
    }

    /// Parse a response with access to provider-local dispatch state.
    ///
    /// The default ignores `parse_state` and preserves the context-aware parser
    /// contract. Providers should downcast only state they created themselves.
    ///
    /// # Errors
    ///
    /// Returns an error if the response cannot be parsed into a valid [`AuctionResponse`].
    async fn parse_response_with_context_and_state(
        &self,
        response: OutboundResponse,
        response_time_ms: u64,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
        parse_state: Option<&(dyn Any + Send + Sync)>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let _ = parse_state;
        self.parse_response_with_context(response, response_time_ms, request, context)
            .await
    }

    /// Check if this provider supports a specific media type.
    fn supports_media_type(&self, media_type: &super::types::MediaType) -> bool {
        // By default, support banner ads
        matches!(media_type, super::types::MediaType::Banner)
    }

    /// Get the configured timeout for this provider in milliseconds.
    fn timeout_ms(&self) -> u32;

    /// Check if this provider is enabled.
    fn is_enabled(&self) -> bool {
        true
    }
}

/// One immutable config-first `OpenRTB` provider instance.
///
/// Every instance owns its validated provider identity and carries only its own
/// typed response state across transport.
pub(crate) struct GenericOpenRtbProvider {
    plan: ProviderPlan,
}

/// Typed state created by and returned to one [`GenericOpenRtbProvider`].
#[allow(
    dead_code,
    clippy::large_enum_variant,
    reason = "typed Stage 6 state avoids provider-state confusion; Stage 7/8 replace profile variants"
)]
pub(crate) enum GenericOpenRtbParseState {
    Standard {
        provider_id: String,
        input: ProviderAuctionInput,
    },
    Prebid {
        provider_id: String,
        auction_id: String,
        input: ProviderAuctionInput,
    },
    Aps {
        provider_id: String,
        input: ProviderAuctionInput,
        debug_request: Option<ApsDebugRequest>,
    },
}

impl GenericOpenRtbProvider {
    pub(crate) fn new(plan: ProviderPlan) -> Self {
        Self { plan }
    }

    pub(crate) fn provider_name(&self) -> &str {
        self.plan.id.as_str()
    }

    pub(crate) fn timeout_ms(&self) -> u32 {
        self.plan.timeout_ms
    }

    #[cfg(test)]
    pub(crate) fn parse_state_for_test(&self, input: ProviderAuctionInput) -> ProviderParseState {
        let state = match &self.plan.profile {
            CompiledOpenRtbProfile::Standard(_) => GenericOpenRtbParseState::Standard {
                provider_id: self.provider_name().to_string(),
                input,
            },
            CompiledOpenRtbProfile::PrebidServer(_) => GenericOpenRtbParseState::Prebid {
                provider_id: self.provider_name().to_string(),
                auction_id: input.common_request().id.clone(),
                input,
            },
            CompiledOpenRtbProfile::Aps(_) => GenericOpenRtbParseState::Aps {
                provider_id: self.provider_name().to_string(),
                input,
                debug_request: None,
            },
        };
        Box::new(state)
    }

    /// Build exactly one routed provider request, unsent.
    ///
    /// The public [`AuctionProvider::request_bids`] seam cannot carry a
    /// [`ProviderAuctionInput`], the complete [`RoutedAuction`], or an
    /// auction-local [`RequestSigner`] without shared mutable provider state.
    /// The plan-backed dispatcher therefore supplies that explicit execution
    /// context here, while this method reuses [`ProviderRequestOutcome`] and
    /// [`ProviderParseState`] for the request and parse token boundary.
    ///
    /// The request carries the configured provider timeout, never the
    /// remaining auction budget, which only `logical_budget_ms` and the batch
    /// cutoff reflect. Its response is buffered under 1 MiB and decoded.
    pub(crate) fn request_bids_routed(
        &self,
        input: &ProviderAuctionInput,
        routed: &RoutedAuction,
        logical_budget_ms: u32,
        signer: Option<&RequestSigner>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
        let signing_params = SigningParams::new(
            input.common_request().id.clone(),
            input.common_request().publisher.domain.clone(),
            "https".to_string(),
        );
        let request = match build_request(
            input,
            routed,
            &self.plan,
            logical_budget_ms,
            &RequestFinalization {
                signer,
                signing_params,
            },
        )? {
            OpenRtbBuildOutcome::Ready(request) => request,
            OpenRtbBuildOutcome::NoImpressions => {
                return Ok(ProviderRequestOutcome::Immediate(
                    AuctionResponse::no_bid(self.provider_name(), 0)
                        .with_metadata("routing", json!({"skipped_no_usable_demand": true})),
                ));
            }
        };

        let body = serde_json::to_vec(&request).change_context(TrustedServerError::Auction {
            message: format!(
                "Provider {} request serialization failed",
                self.provider_name()
            ),
        })?;
        let target =
            outbound_request(Method::POST, self.plan.endpoint.as_str()).map_err(|error| {
                outbound_error(
                    error,
                    TrustedServerError::Auction {
                        message: format!(
                            "Provider {} request construction failed",
                            self.provider_name()
                        ),
                    },
                    OutboundRequestSource::Internal,
                )
            })?;
        let aps_debug_body = matches!(
            &self.plan.profile,
            CompiledOpenRtbProfile::Aps(profile) if profile.debug
        )
        .then(|| body.clone());
        let mut outbound = buffered(
            target,
            Duration::from_millis(u64::from(self.plan.timeout_ms)),
            MAX_PLANNED_RESPONSE_BYTES,
            ResponseDecoding::Decode,
        )
        .cache_policy(OutboundCachePolicy::Bypass)
        .body(body);
        outbound.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if matches!(&self.plan.profile, CompiledOpenRtbProfile::Standard(_)) {
            outbound
                .headers_mut()
                .insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        }
        let aps_debug_request = aps_debug_body
            .as_deref()
            .map(|body| ApsDebugRequest::capture(body, outbound.headers()));
        if let CompiledOpenRtbProfile::PrebidServer(profile) = &self.plan.profile {
            apply_prebid_transport_headers(
                routed.prebid_transport_headers(),
                outbound.headers_mut(),
                profile.consent_forwarding,
                routed.attested_client_ip(),
            );
        }
        let parse_state = match &self.plan.profile {
            CompiledOpenRtbProfile::Standard(_) => GenericOpenRtbParseState::Standard {
                provider_id: self.provider_name().to_string(),
                input: input.clone(),
            },
            CompiledOpenRtbProfile::PrebidServer(_) => {
                debug_assert!(
                    request.imp.iter().all(|impression| impression.id.is_some()),
                    "should populate every PBS impression ID"
                );
                let sent_impression_ids = request
                    .imp
                    .iter()
                    .filter_map(|impression| impression.id.as_deref())
                    .collect::<HashSet<_>>();
                GenericOpenRtbParseState::Prebid {
                    provider_id: self.provider_name().to_string(),
                    auction_id: input.common_request().id.clone(),
                    input: input.filtered_slots(|slot| {
                        sent_impression_ids.contains(slot.slot().id.as_str())
                    }),
                }
            }
            CompiledOpenRtbProfile::Aps(_) => GenericOpenRtbParseState::Aps {
                provider_id: self.provider_name().to_string(),
                input: input.clone(),
                debug_request: aps_debug_request,
            },
        };
        Ok(ProviderRequestOutcome::ready_with_state(
            outbound,
            Box::new(parse_state),
        ))
    }

    /// Parse a response using state created by this exact provider instance.
    pub(crate) async fn parse_response_with_state(
        &self,
        response: OutboundResponse,
        response_time_ms: u64,
        parse_state: Option<&(dyn Any + Send + Sync)>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let parse_state = parse_state
            .and_then(|state| state.downcast_ref::<GenericOpenRtbParseState>())
            .ok_or_else(|| {
                Report::new(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} received missing or invalid response state",
                        self.provider_name()
                    ),
                })
            })?;
        let state_provider_id = match parse_state {
            GenericOpenRtbParseState::Standard { provider_id, .. }
            | GenericOpenRtbParseState::Prebid { provider_id, .. }
            | GenericOpenRtbParseState::Aps { provider_id, .. } => provider_id,
        };
        if state_provider_id != self.provider_name() {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} received response state owned by provider {}",
                    self.provider_name(),
                    state_provider_id
                ),
            }));
        }

        if let GenericOpenRtbParseState::Prebid {
            auction_id, input, ..
        } = parse_state
        {
            let CompiledOpenRtbProfile::PrebidServer(profile) = &self.plan.profile else {
                return Err(Report::new(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} received PBS response state for profile {}",
                        self.provider_name(),
                        self.plan.profile.id()
                    ),
                }));
            };
            let mut parsed = match parse_planned_prebid_response(
                self.provider_name(),
                profile,
                input,
                response,
                response_time_ms,
                auction_id,
            )
            .await
            {
                Ok(parsed) => parsed,
                Err(error) => {
                    log::warn!(
                        "Provider '{}' PBS response parse failed: {:?}",
                        self.provider_name(),
                        error
                    );
                    AuctionResponse::error(self.provider_name(), response_time_ms)
                        .with_metadata("error_type", json!("parse_response"))
                }
            };
            apply_notification_policy(&mut parsed.bids, &self.plan.notifications);
            attach_provider_routing_metadata(&mut parsed, &self.plan.profile, input);
            return Ok(parsed);
        }

        if let GenericOpenRtbParseState::Aps {
            input,
            debug_request,
            ..
        } = parse_state
        {
            let CompiledOpenRtbProfile::Aps(profile) = &self.plan.profile else {
                return Err(Report::new(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} received APS response state for profile {}",
                        self.provider_name(),
                        self.plan.profile.id()
                    ),
                }));
            };
            let mut parsed = match parse_planned_aps_response(
                self.provider_name(),
                profile,
                self.plan.endpoint.as_str(),
                input,
                response,
                response_time_ms,
                debug_request.clone(),
            )
            .await
            {
                Ok(parsed) => parsed,
                Err(error) => {
                    log::warn!(
                        "Provider '{}' APS response parse failed: {:?}",
                        self.provider_name(),
                        error
                    );
                    let mut parsed = AuctionResponse::error(self.provider_name(), response_time_ms)
                        .with_metadata("error_type", json!("parse_response"));
                    attach_provider_routing_metadata(&mut parsed, &self.plan.profile, input);
                    parsed
                }
            };
            apply_notification_policy(&mut parsed.bids, &self.plan.notifications);
            return Ok(parsed);
        }

        let status = response.status();
        let GenericOpenRtbParseState::Standard { input, .. } = parse_state else {
            unreachable!("profile-specific states are handled before standard parsing");
        };
        if status == StatusCode::NO_CONTENT {
            let mut parsed = AuctionResponse::no_bid(self.provider_name(), response_time_ms);
            attach_provider_routing_metadata(&mut parsed, &self.plan.profile, input);
            return Ok(parsed);
        }
        if !status.is_success() {
            if status.is_redirection() {
                log::warn!(
                    "Provider '{}' returned a redirect; generic OpenRTB redirects are refused",
                    self.provider_name()
                );
            }
            let mut parsed = AuctionResponse::error(self.provider_name(), response_time_ms)
                .with_metadata("error_type", json!("http_status"))
                .with_metadata("http_status", json!(status.as_u16()));
            attach_provider_routing_metadata(&mut parsed, &self.plan.profile, input);
            return Ok(parsed);
        }

        let body = response
            .into_bytes_bounded(MAX_PLANNED_RESPONSE_BYTES)
            .await
            .map_err(|error| {
                outbound_error(
                    error,
                    TrustedServerError::Auction {
                        message: format!("Provider {} response body failed", self.provider_name()),
                    },
                    OutboundRequestSource::Internal,
                )
            })?;
        let value: Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                log::warn!(
                    "Provider '{}' response JSON was invalid: {}",
                    self.provider_name(),
                    error
                );
                let mut parsed = AuctionResponse::error(self.provider_name(), response_time_ms)
                    .with_metadata("error_type", json!("parse_response"));
                attach_provider_routing_metadata(&mut parsed, &self.plan.profile, input);
                return Ok(parsed);
            }
        };

        let mut parsed =
            extract_standard_response(self.provider_name(), input, &value, response_time_ms);
        apply_notification_policy(&mut parsed.bids, &self.plan.notifications);
        attach_provider_routing_metadata(&mut parsed, &self.plan.profile, input);
        Ok(parsed)
    }
}
