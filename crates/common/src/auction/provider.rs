//! Trait definition for auction providers.

use error_stack::Report;
use fastly::http::request::PendingRequest;

use crate::error::TrustedServerError;

use super::types::{AuctionContext, AuctionRequest, AuctionResponse};

/// Trait implemented by all auction providers (Prebid, APS, GAM, etc.).
pub trait AuctionProvider: Send + Sync {
    /// Unique identifier for this provider (e.g., "prebid", "aps", "gam").
    fn provider_name(&self) -> &'static str;

    /// Submit a bid request to this provider and return a pending request.
    ///
    /// Implementations should:
    /// - Transform AuctionRequest to provider-specific format
    /// - Make HTTP call to provider endpoint using send_async()
    /// - Return PendingRequest for orchestrator to await
    ///
    /// The orchestrator will handle waiting for responses and parsing them.
    fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<PendingRequest, Report<TrustedServerError>>;

    /// Parse the response from the provider into an AuctionResponse.
    ///
    /// Called by the orchestrator after the PendingRequest completes.
    fn parse_response(
        &self,
        response: fastly::Response,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>>;

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
    /// This is used by the orchestrator to correlate responses with providers
    /// when using `select()` to wait for multiple concurrent requests.
    /// The backend name should match what `ensure_backend_from_url()` returns
    /// for this provider's endpoint.
    fn backend_name(&self) -> Option<String> {
        None
    }
}
