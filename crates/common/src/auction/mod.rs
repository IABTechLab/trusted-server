//! Auction orchestration module for managing multi-provider bidding.
//!
//! This module provides an extensible framework for running auctions across
//! multiple providers (Prebid, Amazon APS, Google GAM, etc.) with support for
//! parallel execution and mediation strategies.
//!
//! Note: Individual auction providers are located in the `integrations` module
//! (e.g., `crate::integrations::aps`, `crate::integrations::gam`, `crate::integrations::prebid`).

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
