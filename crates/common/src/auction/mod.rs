//! Auction orchestration module for managing multi-provider bidding.
//!
//! This module provides an extensible framework for running auctions across
//! multiple providers (Prebid, Amazon APS, Google GAM, etc.) with support for
//! parallel execution and mediation strategies.
//!
//! Note: Individual auction providers are located in the `integrations` module
//! (e.g., `crate::integrations::aps`, `crate::integrations::prebid`).

use crate::settings::Settings;
use std::sync::Arc;

pub mod config;
pub mod endpoints;
pub mod formats;
pub mod orchestrator;
pub mod provider;
pub mod types;

pub use config::AuctionConfig;
pub use orchestrator::AuctionOrchestrator;
pub use provider::AuctionProvider;
pub use types::{
    AdFormat, AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType,
};

/// Type alias for provider builder functions.
type ProviderBuilder = fn(&Settings) -> Vec<Arc<dyn AuctionProvider>>;

/// Returns the list of all available provider builder functions.
///
/// This list is used to auto-discover and register auction providers from settings.
/// Each builder function checks the settings for its specific provider configuration
/// and returns any enabled providers.
fn provider_builders() -> &'static [ProviderBuilder] {
    &[
        crate::integrations::prebid::register_auction_provider,
        crate::integrations::aps::register_providers,
        crate::integrations::adserver_mock::register_providers,
    ]
}

/// Build a new auction orchestrator for the current settings.
///
/// This constructor registers all auction providers discovered from the provided settings.
/// Callers can reuse the returned [`AuctionOrchestrator`] across requests.
///
/// # Arguments
/// * `settings` - Application settings used to configure the orchestrator and providers
#[must_use]
pub fn build_orchestrator(settings: &Settings) -> AuctionOrchestrator {
    log::info!("Building auction orchestrator");

    let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());

    // Auto-discover and register all auction providers from settings
    for builder in provider_builders() {
        for provider in builder(settings) {
            orchestrator.register_provider(provider);
        }
    }

    log::info!(
        "Auction orchestrator built with {} providers",
        orchestrator.provider_count()
    );

    orchestrator
}
