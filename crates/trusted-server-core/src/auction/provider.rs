//! Trait definition for auction providers.

use core::any::Any;
use std::collections::HashSet;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
use http::{Method, Request, header};
use serde_json::json;

use crate::error::TrustedServerError;
use crate::platform::{
    PlatformHttpRequest, PlatformPendingRequest, PlatformResponse, RuntimeServices,
};
use crate::request_signing::{RequestSigner, SigningParams};

use super::demand::{CompiledDemand, DemandResponse, DemandTransport};
use super::openrtb::{
    OpenRtbBuildOutcome, RequestFinalization, apply_notification_policy, build_request,
    unused_bidder_params_count,
};
use super::plan::ProviderPlan;
use super::routing::{ProviderAuctionInput, RoutedAuction};
use super::types::{AuctionContext, AuctionRequest, AuctionResponse};

fn attach_provider_routing_metadata(
    response: &mut AuctionResponse,
    demand: &dyn CompiledDemand,
    input: &ProviderAuctionInput,
) {
    response.metadata.insert(
        "routing".to_string(),
        json!({"unused_bidder_params_count": unused_bidder_params_count(demand, input)}),
    );
}

/// Provider-local state carried from request dispatch to response parsing.
pub type ProviderParseState = Box<dyn Any + Send + Sync>;

/// Result of asking a provider to start a bid request.
pub enum ProviderRequestOutcome {
    /// An upstream request is in flight and must be awaited by the orchestrator.
    Pending {
        /// Platform-specific pending request handle.
        request: PlatformPendingRequest,
        /// Optional provider-local state consumed when the response is parsed.
        parse_state: Option<ProviderParseState>,
    },
    /// A complete provider response that required no upstream request.
    Immediate(AuctionResponse),
}

impl ProviderRequestOutcome {
    /// Wrap an ordinary pending provider request without parse state.
    #[must_use]
    pub fn pending(request: PlatformPendingRequest) -> Self {
        Self::Pending {
            request,
            parse_state: None,
        }
    }

    /// Wrap a pending provider request with provider-local parse state.
    #[must_use]
    pub fn pending_with_state(
        request: PlatformPendingRequest,
        parse_state: ProviderParseState,
    ) -> Self {
        Self::Pending {
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

    /// Submit a bid request to this provider.
    ///
    /// Implementations normally return a pending upstream request, but may return
    /// an immediate response for a legitimate outcome that requires no HTTP call.
    /// The orchestrator handles waiting for and parsing pending responses.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be created or if the provider endpoint
    /// cannot be reached (though usually network errors happen while the returned
    /// [`PlatformPendingRequest`] is polled).
    async fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>>;

    /// Parse the response from the provider into an `AuctionResponse`.
    ///
    /// Called by the orchestrator after the [`PlatformPendingRequest`] completes.
    /// Declared async so implementations can safely drain streaming response bodies
    /// without panicking on the `Body::Stream` variant.
    ///
    /// # Errors
    ///
    /// Returns an error if the response cannot be parsed into a valid `AuctionResponse`.
    async fn parse_response(
        &self,
        response: PlatformResponse,
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
        response: PlatformResponse,
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
        response: PlatformResponse,
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

    /// Return the backend name used by this provider for request routing.
    ///
    /// `timeout_ms` is the effective timeout that will be used when the backend
    /// is registered in [`request_bids`](Self::request_bids).  It must be
    /// forwarded to [`crate::platform::PlatformBackend::predict_name`] through
    /// `services` so the predicted name matches the actual platform backend
    /// registration.
    fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
        None
    }
}

/// One immutable config-first `OpenRTB` provider instance.
///
/// Every instance owns its validated provider identity and carries only its own
/// typed response state across transport.
pub(crate) struct GenericOpenRtbProvider {
    plan: ProviderPlan,
}

/// State created by and returned to one [`GenericOpenRtbProvider`].
///
/// `captured` holds whatever the demand implementation kept from its own
/// request, and only that implementation reads it back.
pub(crate) struct GenericOpenRtbParseState {
    provider_id: String,
    input: ProviderAuctionInput,
    captured: Option<Box<dyn Any + Send + Sync>>,
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
        Box::new(GenericOpenRtbParseState {
            provider_id: self.provider_name().to_string(),
            input,
            captured: None,
        })
    }

    /// Build, register, and start exactly one routed provider request.
    ///
    /// The public [`AuctionProvider::request_bids`] seam cannot carry a
    /// [`ProviderAuctionInput`], the complete [`RoutedAuction`], or an
    /// auction-local [`RequestSigner`] without shared mutable provider state.
    /// The plan-backed split dispatcher therefore supplies that explicit
    /// execution context here, while this method reuses
    /// [`ProviderRequestOutcome`] and [`ProviderParseState`] for the existing
    /// request/parse token boundary.
    #[allow(
        clippy::too_many_arguments,
        reason = "the internal driver keeps routed inputs, both budgets, signer, services, and collision state explicit"
    )]
    pub(crate) async fn request_bids_routed(
        &self,
        input: &ProviderAuctionInput,
        routed: &RoutedAuction,
        logical_budget_ms: u32,
        transport_timeout_ms: u32,
        signer: Option<&RequestSigner>,
        services: &RuntimeServices,
        reserved_backend_names: &mut HashSet<String>,
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
                return Ok(ProviderRequestOutcome::Immediate(AuctionResponse::no_bid(
                    self.provider_name(),
                    0,
                )));
            }
        };

        let spec = self
            .plan
            .backend_spec_with_transport_timeout(transport_timeout_ms);
        let predicted_name =
            services
                .backend()
                .predict_name(&spec)
                .change_context(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} backend prediction failed",
                        self.provider_name()
                    ),
                })?;
        let backend_name =
            services
                .backend()
                .ensure(&spec)
                .change_context(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} backend registration failed",
                        self.provider_name()
                    ),
                })?;
        if backend_name != predicted_name {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} backend registration did not match prediction",
                    self.provider_name()
                ),
            }));
        }
        if !reserved_backend_names.insert(backend_name.clone()) {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} resolved an actual backend name already owned by another provider",
                    self.provider_name()
                ),
            }));
        }

        let body = serde_json::to_vec(&request).change_context(TrustedServerError::Auction {
            message: format!(
                "Provider {} request serialization failed",
                self.provider_name()
            ),
        })?;
        let demand = self.plan.demand.as_ref();
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(self.plan.endpoint.as_str())
            .header(header::CONTENT_TYPE, "application/json");
        if demand.field_policy().accept_json {
            builder = builder.header(header::ACCEPT, "application/json");
        }
        let captured = builder
            .headers_ref()
            .and_then(|headers| demand.capture_request(&body, headers));
        let mut outbound =
            builder
                .body(EdgeBody::from(body))
                .change_context(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} request construction failed",
                        self.provider_name()
                    ),
                })?;
        demand.prepare_outbound(
            &mut outbound,
            DemandTransport {
                headers: routed.transport_headers(),
                attested_client_ip: routed.attested_client_ip(),
            },
        );
        let pending = services
            .http_client()
            .send_async(PlatformHttpRequest::new(outbound, backend_name.clone()))
            .await
            .change_context(TrustedServerError::Auction {
                message: format!("Provider {} request launch failed", self.provider_name()),
            })?;
        if pending.backend_name() != Some(backend_name.as_str()) {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} pending request backend did not match registered backend",
                    self.provider_name()
                ),
            }));
        }
        Ok(ProviderRequestOutcome::pending_with_state(
            pending,
            Box::new(GenericOpenRtbParseState {
                provider_id: self.provider_name().to_string(),
                input: input.clone(),
                captured,
            }),
        ))
    }

    /// Parse a response using state created by this exact provider instance.
    ///
    /// The implementation reads its own response and reports an unusable one as
    /// an error response. The driver then applies the common notification
    /// policy and routing diagnostics, so every demand source reports them the
    /// same way.
    pub(crate) async fn parse_response_with_state(
        &self,
        response: PlatformResponse,
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
        if parse_state.provider_id != self.provider_name() {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} received response state owned by provider {}",
                    self.provider_name(),
                    parse_state.provider_id
                ),
            }));
        }

        let demand = self.plan.demand.as_ref();
        let mut parsed = demand
            .parse_response(
                DemandResponse {
                    provider_id: self.provider_name(),
                    endpoint: self.plan.endpoint.as_str(),
                    input: &parse_state.input,
                    response_time_ms,
                    captured: parse_state.captured.as_deref(),
                },
                response,
            )
            .await?;
        apply_notification_policy(&mut parsed.bids, &self.plan.notifications);
        attach_provider_routing_metadata(&mut parsed, demand, &parse_state.input);
        Ok(parsed)
    }
}
