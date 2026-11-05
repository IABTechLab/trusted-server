//! Legacy provider compatibility and the neutral plan-backed transport driver.

use super::openrtb::{RequestFinalization, apply_notification_policy, unused_bidder_params_count};
use super::plan::{NotificationPolicy, ProviderPlan};
use super::profile::{BoundOpenRtbResponseParser, OpenRtbPreparation, PreparedOpenRtbOutcome};
use super::routing::{ProviderAuctionInput, RoutedAuction};
use super::types::{AuctionContext, AuctionRequest, AuctionResponse};
use crate::error::TrustedServerError;
use crate::platform::{
    PlatformHttpRequest, PlatformPendingRequest, PlatformResponse, RuntimeServices,
};
use crate::request_signing::{RequestSigner, SigningParams};
use async_trait::async_trait;
use core::any::Any;
use error_stack::{Report, ResultExt as _};
use serde_json::json;
use std::collections::HashSet;

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
    /// Outbound auction and mediator requests must use
    /// [`PlatformHttpRequest::with_cache_bypass`]: they carry personalized data,
    /// and Fastly requires this marker for deadline-bounded collection.
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

/// A planned launch carries a consuming parser, never erased vendor state.
pub(crate) enum PlannedProviderRequestOutcome {
    Pending {
        request: PlatformPendingRequest,
        parse_state: Option<Box<BoundProviderResponse>>,
    },
    Immediate(AuctionResponse),
}

/// Generic envelope around one request-bound profile parser.
pub(crate) struct BoundProviderResponse {
    provider_id: String,
    parser: Box<dyn BoundOpenRtbResponseParser>,
    notifications: NotificationPolicy,
    unused_bidder_params: u32,
}

/// Immutable provider transport driver. All specialization stays in its profile.
pub(crate) struct GenericOpenRtbProvider {
    plan: ProviderPlan,
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

    fn prepare(
        &self,
        input: &ProviderAuctionInput,
        routed: &RoutedAuction,
        logical_budget_ms: u32,
        transport_budget_ms: u32,
        signer: Option<&RequestSigner>,
    ) -> Result<PreparedOpenRtbOutcome, Report<TrustedServerError>> {
        self.plan.profile.prepare(OpenRtbPreparation {
            provider: &self.plan,
            input,
            routed,
            logical_budget_ms,
            transport_budget_ms,
            finalization: RequestFinalization {
                signer,
                signing_params: SigningParams::new(
                    input.common_request().id.clone(),
                    input.common_request().publisher.domain.clone(),
                    "https".to_string(),
                ),
            },
        })
    }

    fn bind(
        &self,
        input: &ProviderAuctionInput,
        parser: Box<dyn BoundOpenRtbResponseParser>,
    ) -> Box<BoundProviderResponse> {
        Box::new(BoundProviderResponse {
            provider_id: self.provider_name().to_string(),
            parser,
            notifications: self.plan.notifications.clone(),
            unused_bidder_params: unused_bidder_params_count(&self.plan.profile, input),
        })
    }

    #[cfg(test)]
    pub(crate) fn parse_state_for_test(
        &self,
        input: &ProviderAuctionInput,
    ) -> Box<BoundProviderResponse> {
        let routed = RoutedAuction::for_response_test();
        let PreparedOpenRtbOutcome::Exchange(exchange) = self
            .prepare(
                input,
                &routed,
                self.plan.timeout_ms,
                self.plan.timeout_ms,
                None,
            )
            .expect("should prepare parser for response test")
        else {
            panic!("should retain response test impressions")
        };
        self.bind(input, exchange.parser)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "routed facts, both budgets, signer, services and collision state stay explicit"
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
    ) -> Result<PlannedProviderRequestOutcome, Report<TrustedServerError>> {
        let exchange = match self.prepare(
            input,
            routed,
            logical_budget_ms,
            transport_timeout_ms,
            signer,
        )? {
            PreparedOpenRtbOutcome::Skip { metadata } => {
                let mut response = AuctionResponse::no_bid(self.provider_name(), 0);
                response.metadata = metadata;
                return Ok(PlannedProviderRequestOutcome::Immediate(response));
            }
            PreparedOpenRtbOutcome::Exchange(exchange) => exchange,
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
        let pending = services
            .http_client()
            .send_async(
                PlatformHttpRequest::new(exchange.request, backend_name.clone())
                    .with_cache_bypass(),
            )
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
        Ok(PlannedProviderRequestOutcome::Pending {
            request: pending,
            parse_state: Some(self.bind(input, exchange.parser)),
        })
    }

    pub(crate) async fn parse_response_with_state(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        state: Option<Box<BoundProviderResponse>>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let state = state.ok_or_else(|| {
            Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} received missing response parser",
                    self.provider_name()
                ),
            })
        })?;
        if state.provider_id != self.provider_name() {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} received response state owned by provider {}",
                    self.provider_name(),
                    state.provider_id
                ),
            }));
        }
        let mut parsed = state.parser.parse(response, response_time_ms).await?;
        parsed.provider = state.provider_id;
        apply_notification_policy(&mut parsed.bids, &state.notifications);
        parsed
            .metadata
            .entry("routing".to_string())
            .or_insert_with(|| json!({"unused_bidder_params_count": state.unused_bidder_params}));
        Ok(parsed)
    }
}
