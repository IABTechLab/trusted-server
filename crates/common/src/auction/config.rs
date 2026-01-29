//! Configuration structures for auction orchestration.
//!
//! The base types are defined in `auction_config_types.rs` to avoid circular dependencies
//! with `build.rs`. This module re-exports them.

pub use crate::auction_config_types::AuctionConfig;
