//! Auction configuration types (separated to avoid circular deps in build.rs).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Auction orchestration configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuctionConfig {
    /// Enable the auction orchestrator
    #[serde(default)]
    pub enabled: bool,

    /// Strip executable markup from winning-bid creative HTML before delivery.
    ///
    /// Sanitization removes `script`/`object`/`embed`/`form`/etc. **with their inner
    /// content**, which blanks script-based creatives — the majority of programmatic
    /// display. It is the primary defence when the creative renders in a context that
    /// shares the publisher's origin.
    ///
    /// Disable only when creatives render in a foreign-origin frame (for example the
    /// Prebid Universal Creative inside the ad server's iframe), where the markup
    /// cannot reach the publisher origin. Defaults to disabled.
    #[serde(
        default = "default_sanitize_creatives",
        skip_serializing_if = "is_default_sanitize_creatives"
    )]
    pub sanitize_creatives: bool,

    /// Rewrite sanitized winning-bid creative HTML to first-party endpoints.
    #[serde(
        default = "default_rewrite_creatives",
        skip_serializing_if = "is_default_rewrite_creatives"
    )]
    pub rewrite_creatives: bool,

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
}

impl Default for AuctionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sanitize_creatives: default_sanitize_creatives(),
            rewrite_creatives: default_rewrite_creatives(),
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

fn default_sanitize_creatives() -> bool {
    false
}

fn default_rewrite_creatives() -> bool {
    false
}

fn is_default_rewrite_creatives(value: &bool) -> bool {
    *value == default_rewrite_creatives()
}

fn is_default_sanitize_creatives(value: &bool) -> bool {
    *value == default_sanitize_creatives()
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

    #[test]
    fn creative_processing_defaults_to_disabled() {
        let config: AuctionConfig =
            serde_json::from_value(serde_json::json!({})).expect("should deserialize defaults");

        assert!(
            !config.rewrite_creatives,
            "creative rewriting is opt-in: creatives ship as the bidder returned them"
        );
        assert!(
            !config.sanitize_creatives,
            "creative sanitization is opt-in: it strips executable markup with its content"
        );
    }

    #[test]
    fn default_rewrite_creatives_is_not_serialized() {
        let serialized =
            serde_json::to_value(AuctionConfig::default()).expect("should serialize defaults");

        assert!(
            serialized.get("rewrite_creatives").is_none(),
            "should omit the default rewrite setting"
        );
    }

    #[test]
    fn enabled_rewrite_creatives_is_serialized() {
        let config = AuctionConfig {
            rewrite_creatives: true,
            ..AuctionConfig::default()
        };
        let serialized = serde_json::to_value(config).expect("should serialize enabled rewriting");

        assert_eq!(
            serialized.get("rewrite_creatives"),
            Some(&serde_json::Value::Bool(true)),
            "should preserve an explicit rewrite opt-in"
        );
    }

    #[test]
    fn default_sanitize_creatives_is_not_serialized() {
        let serialized =
            serde_json::to_value(AuctionConfig::default()).expect("should serialize defaults");

        assert!(
            serialized.get("sanitize_creatives").is_none(),
            "should omit the default sanitize setting"
        );
    }

    #[test]
    fn enabled_sanitize_creatives_is_serialized() {
        let config = AuctionConfig {
            sanitize_creatives: true,
            ..AuctionConfig::default()
        };
        let serialized =
            serde_json::to_value(config).expect("should serialize enabled sanitization");

        assert_eq!(
            serialized.get("sanitize_creatives"),
            Some(&serde_json::Value::Bool(true)),
            "should preserve an explicit sanitize opt-in"
        );
    }
}
