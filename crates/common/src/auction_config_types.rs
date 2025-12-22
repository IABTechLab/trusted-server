//! Auction configuration types (separated to avoid circular deps in build.rs).

use serde::{Deserialize, Serialize};

/// Auction orchestration configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AuctionConfig {
    /// Enable the auction orchestrator
    #[serde(default)]
    pub enabled: bool,

    /// Auction strategy: "parallel_mediation", "waterfall", "parallel_only"
    #[serde(default = "default_strategy")]
    pub strategy: String,

    /// Provider names that participate in bidding
    /// Simply list the provider names (e.g., ["prebid", "aps"])
    #[serde(default)]
    pub bidders: Vec<String>,

    /// Optional mediator provider name (e.g., "gam")
    pub mediator: Option<String>,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout")]
    pub timeout_ms: u32,
}

fn default_strategy() -> String {
    "parallel_mediation".to_string()
}

fn default_timeout() -> u32 {
    2000
}

#[allow(dead_code)] // Methods used in runtime but not in build script
impl AuctionConfig {
    /// Get all bidder names.
    pub fn bidder_names(&self) -> &[String] {
        &self.bidders
    }

    /// Check if this config has a mediator configured.
    pub fn has_mediator(&self) -> bool {
        self.mediator.is_some()
    }
}
