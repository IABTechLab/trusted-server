//! Google Ad Manager (GAM) integration.
//!
//! This module provides both real and mock implementations of the GAM auction provider.
//! GAM acts as a mediation server that receives bids from other providers and makes
//! the final ad selection decision.

use error_stack::Report;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::auction::provider::AuctionProvider;
use crate::auction::types::{AuctionContext, AuctionRequest, AuctionResponse, MediaType};
use crate::error::TrustedServerError;
use crate::settings::IntegrationConfig as IntegrationConfigTrait;

// ============================================================================
// Real GAM Provider
// ============================================================================

/// Configuration for GAM integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct GamConfig {
    /// Whether GAM integration is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// GAM network ID
    pub network_id: String,

    /// GAM API endpoint
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
}

impl IntegrationConfigTrait for GamConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    false
}

fn default_endpoint() -> String {
    "https://securepubads.g.doubleclick.net/gampad/ads".to_string()
}

fn default_timeout_ms() -> u32 {
    500
}

impl Default for GamConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            network_id: String::new(),
            endpoint: default_endpoint(),
            timeout_ms: default_timeout_ms(),
        }
    }
}

/// Google Ad Manager provider (acts as mediator).
pub struct GamAuctionProvider {
    config: GamConfig,
}

impl GamAuctionProvider {
    /// Create a new GAM auction provider.
    pub fn new(config: GamConfig) -> Self {
        Self { config }
    }
}

impl AuctionProvider for GamAuctionProvider {
    fn provider_name(&self) -> &'static str {
        "gam"
    }

    fn request_bids(
        &self,
        request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<fastly::http::request::PendingRequest, Report<TrustedServerError>> {
        log::info!(
            "GAM: mediating auction for {} slots (network_id: {})",
            request.slots.len(),
            self.config.network_id
        );

        // TODO: Implement real GAM API integration with send_async
        //
        // Implementation steps:
        // 1. Extract bidder responses from request context (see gam_mock implementation for example)
        // 2. Transform bids to GAM key-value targeting format
        // 3. Make HTTP request to GAM ad server using send_async()
        // 4. Return PendingRequest
        //
        // Reference: https://developers.google.com/ad-manager/api/start

        log::warn!("GAM: Real implementation not yet available");

        Err(Report::new(TrustedServerError::Auction {
            message: "GAM integration not yet implemented. Use 'gam_mock' provider for testing."
                .to_string(),
        }))
    }

    fn parse_response(
        &self,
        _response: fastly::Response,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        // TODO: Parse GAM response
        Ok(AuctionResponse::error("gam", response_time_ms))
    }

    fn supports_media_type(&self, media_type: &MediaType) -> bool {
        // GAM supports all media types
        matches!(
            media_type,
            MediaType::Banner | MediaType::Video | MediaType::Native
        )
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

// ============================================================================
// Provider Auto-Registration
// ============================================================================

use crate::settings::Settings;
use std::sync::Arc;

/// Auto-register GAM providers based on settings configuration.
///
/// This function checks the settings for both real and mock GAM configurations
/// and returns any enabled providers ready for registration with the orchestrator.
pub fn register_providers(settings: &Settings) -> Vec<Arc<dyn AuctionProvider>> {
    let mut providers: Vec<Arc<dyn AuctionProvider>> = Vec::new();

    // Check for real GAM provider configuration
    if let Ok(Some(config)) = settings.integration_config::<GamConfig>("gam") {
        log::info!("Registering real GAM provider");
        providers.push(Arc::new(GamAuctionProvider::new(config)));
    }

    providers
}
