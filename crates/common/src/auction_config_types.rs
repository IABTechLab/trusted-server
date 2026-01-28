//! Auction configuration types (separated to avoid circular deps in build.rs).

use serde::{Deserialize, Serialize};

/// Auction orchestration configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AuctionConfig {
    /// Enable the auction orchestrator
    #[serde(default)]
    pub enabled: bool,

    /// Provider names that participate in bidding
    /// Simply list the provider names (e.g., ["prebid", "aps"])
    #[serde(default, deserialize_with = "crate::settings::vec_from_seq_or_map")]
    pub providers: Vec<String>,

    /// Optional mediator provider name (e.g., "gam")
    /// When set, runs parallel mediation strategy (bidders in parallel, then mediator decides)
    /// When omitted, runs parallel only strategy (bidders in parallel, highest CPM wins)
    pub mediator: Option<String>,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout")]
    pub timeout_ms: u32,

    /// KV store name for creative storage (deprecated: creatives are now delivered inline)
    #[serde(default = "default_creative_store")]
    pub creative_store: String,
}

fn default_timeout() -> u32 {
    2000
}

fn default_creative_store() -> String {
    "creative_store".to_string()
}

#[allow(dead_code)] // Methods used in runtime but not in build script
impl AuctionConfig {
    /// Get all provider names.
    pub fn provider_names(&self) -> &[String] {
        &self.providers
    }

    /// Check if this config has a mediator configured.
    pub fn has_mediator(&self) -> bool {
        self.mediator.is_some()
    }
}
