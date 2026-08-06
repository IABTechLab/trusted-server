//! Trait definition for auction providers.

use core::any::Any;

use async_trait::async_trait;
use error_stack::Report;

use crate::error::TrustedServerError;
use crate::platform::{PlatformPendingRequest, PlatformResponse, RuntimeServices};

use super::types::{
    AuctionContext, AuctionRequest, AuctionResponse, AuctionSlotFailureReason, Bid,
};

/// Exactly one normalized outcome for a slot dispatched to one provider.
#[derive(Debug, Clone)]
pub struct ProviderSlotOutcome {
    /// Provider integration that received the slot.
    pub provider: String,
    /// Exact dispatched slot identifier.
    pub slot: String,
    /// Candidate, successful no-bid, or typed failure.
    pub disposition: ProviderSlotDisposition,
}

/// Closed normalized provider result for one dispatched slot.
#[derive(Debug, Clone)]
pub enum ProviderSlotDisposition {
    /// One or more independently validated candidates returned for the slot.
    Candidates(Vec<Bid>),
    /// Provider completed successfully without a candidate for this slot.
    NoBid,
    /// Provider failed for this slot.
    Failed(AuctionSlotFailureReason),
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
    /// Unique identifier for this provider (e.g., "prebid", "aps", "gam").
    fn provider_name(&self) -> &'static str;

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
