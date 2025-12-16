//! Trait definition for auction providers.

use async_trait::async_trait;
use error_stack::Report;

use crate::error::TrustedServerError;

use super::types::{AuctionContext, AuctionRequest, AuctionResponse};

/// Trait implemented by all auction providers (Prebid, APS, GAM, etc.).
#[async_trait(?Send)]
pub trait AuctionProvider: Send + Sync {
    /// Unique identifier for this provider (e.g., "prebid", "aps", "gam").
    fn provider_name(&self) -> &'static str;

    /// Submit a bid request to this provider and await response.
    ///
    /// Implementations should:
    /// - Transform AuctionRequest to provider-specific format
    /// - Make HTTP call to provider endpoint
    /// - Parse response into AuctionResponse
    /// - Handle timeouts gracefully
    async fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
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
}
