//! Auction orchestration module for managing multi-provider bidding.
//!
//! This module provides an extensible framework for running auctions across
//! multiple providers (Prebid, Amazon APS, Google GAM, etc.) with support for
//! parallel execution and mediation strategies.
//!
//! Note: Individual auction providers are located in the `integrations` module
//! (e.g., `crate::integrations::aps`, `crate::integrations::gam`, `crate::integrations::prebid`).

use std::sync::{Arc, OnceLock};
use crate::settings::Settings;

pub mod config;
pub mod orchestrator;
pub mod provider;
pub mod types;

pub use config::AuctionConfig;
pub use orchestrator::AuctionOrchestrator;
pub use provider::AuctionProvider;
pub use types::{
    AdFormat, AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType,
};

/// Global auction orchestrator singleton.
///
/// Initialized once on first access with the provided settings.
/// All providers are registered during initialization.
static GLOBAL_ORCHESTRATOR: OnceLock<AuctionOrchestrator> = OnceLock::new();

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
        crate::integrations::gam::register_providers,
    ]
}

/// Get or initialize the global auction orchestrator.
///
/// The orchestrator is created once on first access and reused for all subsequent requests.
/// All auction providers are automatically discovered and registered during initialization.
///
/// # Arguments
/// * `settings` - Application settings used to configure the orchestrator and providers
///
/// # Returns
/// Reference to the global orchestrator instance
pub fn get_orchestrator(settings: &Settings) -> &'static AuctionOrchestrator {
    GLOBAL_ORCHESTRATOR.get_or_init(|| {
        log::info!("Initializing global auction orchestrator");
        
        let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());

        // Auto-discover and register all auction providers from settings
        for builder in provider_builders() {
            for provider in builder(settings) {
                orchestrator.register_provider(provider);
            }
        }

        log::info!(
            "Auction orchestrator initialized with {} providers",
            orchestrator.provider_count()
        );

        orchestrator
    })
}
