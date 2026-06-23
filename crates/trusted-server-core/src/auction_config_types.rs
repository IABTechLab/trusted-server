//! Auction configuration types (separated to avoid circular deps in build.rs).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Default Fastly real-time log endpoint for auction telemetry events.
pub const DEFAULT_AUCTION_TELEMETRY_LOG_ENDPOINT: &str = "ts_auction_events";

/// Auction orchestration configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
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

    /// Keys allowed in the auction request context map.
    /// Only config entries from the JS payload whose key appears in this list
    /// are forwarded into the `AuctionRequest.context`. Unrecognised keys are
    /// silently dropped. An empty list blocks all context keys.
    #[serde(default = "default_allowed_context_keys")]
    pub allowed_context_keys: HashSet<String>,

    /// Fastly real-time log endpoint used for auction telemetry rows.
    #[serde(default = "default_telemetry_log_endpoint")]
    pub telemetry_log_endpoint: String,
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
            telemetry_log_endpoint: default_telemetry_log_endpoint(),
        }
    }
}

fn default_timeout() -> u32 {
    2000
}

fn default_creative_store() -> String {
    "creative_store".to_string()
}

fn default_allowed_context_keys() -> HashSet<String> {
    HashSet::new()
}

fn default_telemetry_log_endpoint() -> String {
    DEFAULT_AUCTION_TELEMETRY_LOG_ENDPOINT.to_string()
}

#[allow(dead_code)] // Methods used in runtime but not in build script
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

    /// Return the configured auction telemetry log endpoint.
    #[must_use]
    pub fn telemetry_log_endpoint(&self) -> &str {
        let endpoint = self.telemetry_log_endpoint.trim();
        if endpoint.is_empty() {
            DEFAULT_AUCTION_TELEMETRY_LOG_ENDPOINT
        } else {
            endpoint
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_telemetry_log_endpoint_matches_existing_endpoint() {
        let config = AuctionConfig::default();
        assert_eq!(
            config.telemetry_log_endpoint(),
            DEFAULT_AUCTION_TELEMETRY_LOG_ENDPOINT,
            "should preserve the existing Fastly log endpoint by default"
        );
    }

    #[test]
    fn blank_telemetry_log_endpoint_falls_back_to_default() {
        let config = AuctionConfig {
            telemetry_log_endpoint: "  ".to_string(),
            ..Default::default()
        };
        assert_eq!(
            config.telemetry_log_endpoint(),
            DEFAULT_AUCTION_TELEMETRY_LOG_ENDPOINT,
            "should not pass an empty endpoint name to Fastly"
        );
    }
}
