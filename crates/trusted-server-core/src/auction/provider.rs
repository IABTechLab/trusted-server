//! Trait definition for auction providers.

use async_trait::async_trait;
use error_stack::Report;

use crate::error::TrustedServerError;
use crate::platform::{PlatformPendingRequest, PlatformResponse};

use super::types::{AuctionContext, AuctionRequest, AuctionResponse};

/// Trait implemented by all auction providers (Prebid, APS, GAM, etc.).
#[async_trait(?Send)]
pub trait AuctionProvider: Send + Sync {
    /// Unique identifier for this provider (e.g., "prebid", "aps", "gam").
    fn provider_name(&self) -> &'static str;

    /// Submit a bid request to this provider and return a pending request.
    ///
    /// Implementations should:
    /// - Transform `AuctionRequest` to provider-specific format
    /// - Make an HTTP call through `context.services.http_client().send_async(...)`
    /// - Return [`PlatformPendingRequest`] for the orchestrator to await
    ///
    /// The orchestrator will handle waiting for responses and parsing them.
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
    ) -> Result<PlatformPendingRequest, Report<TrustedServerError>>;

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

    /// Parse the response with access to the original auction context.
    ///
    /// Providers that need request-local metadata while transforming responses
    /// can override this method. The default preserves the existing
    /// response-only provider contract.
    ///
    /// # Errors
    ///
    /// Returns an error if the response cannot be parsed into a valid [`AuctionResponse`].
    async fn parse_response_with_context(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let _ = context;
        self.parse_response(response, response_time_ms).await
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
    /// forwarded to [`crate::backend::BackendConfig::backend_name_for_url()`] so the predicted
    /// name matches the actual registration (the timeout is part of the name).
    fn backend_name(&self, _timeout_ms: u32) -> Option<String> {
        None
    }
}
