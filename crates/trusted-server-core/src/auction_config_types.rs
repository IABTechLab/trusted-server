//! Auction configuration types (separated to avoid circular deps in build.rs).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use validator::Validate;

/// Auction orchestration configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
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
    #[validate(range(min = 1, max = 60000))]
    pub timeout_ms: u32,

    /// KV store name for creative storage (deprecated: creatives are now delivered inline)
    #[serde(default = "default_creative_store")]
    pub creative_store: String,

    /// Keys allowed in the auction request context map.
    /// Only config entries from the JS payload whose key appears in this list
    /// are forwarded into the `AuctionRequest.context`. Unrecognised keys are
    /// silently dropped. An empty list blocks all context keys.
    #[serde(default = "default_allowed_context_keys")]
    pub allowed_context_keys: HashSet<String>,
}

impl Default for AuctionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            providers: Vec::new(),
            mediator: None,
            timeout_ms: default_timeout(),
            creative_store: default_creative_store(),
            allowed_context_keys: HashSet::new(),
        }
    }
}

fn default_timeout() -> u32 {
    2000
}

fn default_creative_store() -> String {
    "creative_store".to_owned()
}

fn default_allowed_context_keys() -> HashSet<String> {
    HashSet::new()
}

#[allow(
    dead_code,
    reason = "methods are used by the runtime crate but not by build.rs path inclusion"
)]
impl AuctionConfig {
    /// Get all provider names.
    #[must_use]
    pub fn provider_names(&self) -> &[String] {
        &self.providers
    }

    /// Check if this config has a mediator configured.
    #[must_use]
    pub fn has_mediator(&self) -> bool {
        self.mediator.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use validator::Validate as _;

    fn config_with_timeout(timeout_ms: u32) -> AuctionConfig {
        AuctionConfig {
            timeout_ms,
            ..AuctionConfig::default()
        }
    }

    #[test]
    fn timeout_ms_range_is_enforced() {
        for good in [1, 2000, 60000] {
            config_with_timeout(good)
                .validate()
                .unwrap_or_else(|err| panic!("timeout {good} should be accepted: {err:?}"));
        }
        for bad in [0, 60001] {
            config_with_timeout(bad)
                .validate()
                .expect_err(&format!("timeout {bad} should be rejected"));
        }
    }
}
