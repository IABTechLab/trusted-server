//! Auction orchestration module for managing multi-provider bidding.
//!
//! This module provides an extensible framework for running auctions across
//! multiple providers (Prebid, Amazon APS, Google GAM, etc.) with support for
//! parallel execution and mediation strategies.
//!
//! Note: Individual auction providers are located in the `integrations` module
//! (e.g., `crate::integrations::aps`, `crate::integrations::gam`, `crate::integrations::prebid`).

use std::sync::Arc;
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

/// Type alias for provider builder functions.
type ProviderBuilder = fn(&Settings) -> Vec<Arc<dyn AuctionProvider>>;

/// Returns the list of all available provider builder functions.
///
/// This list is used to auto-discover and register auction providers from settings.
/// Each builder function checks the settings for its specific provider configuration
/// and returns any enabled providers.
pub(crate) fn provider_builders() -> &'static [ProviderBuilder] {
    &[
        crate::integrations::prebid::register_auction_provider,
        crate::integrations::aps::register_providers,
        crate::integrations::gam::register_providers,
    ]
}
