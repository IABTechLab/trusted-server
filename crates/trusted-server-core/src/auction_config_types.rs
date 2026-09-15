//! Auction configuration types shared by settings and auction planning.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashSet};
use validator::Validate;

const MOVED_PROVIDERS_MESSAGE: &str = "`[auction.providers]` has moved. Select demand sources with `[demand] provider = [...]` and give each its settings in `[demand.<name>]`, as the configuration rules describe";

const MOVED_MEDIATOR_MESSAGE: &str = "`[auction] mediator` has moved. Select the ad server with `[adserver] provider = \"<name>\"` and give it its settings in `[adserver.<name>]`, as the configuration rules describe";

pub use crate::auction::plan::{
    BidderId, BidderRouteConfig, NotificationConfig, ProviderId, RoutingMode,
};

/// Auction orchestration configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
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

    /// Rewrite winning-bid creative HTML to first-party endpoints (applied
    /// after sanitization when [`Self::sanitize_creatives`] is enabled).
    ///
    /// The default stays omitted from serialized config blobs to avoid adding
    /// this field when it has no effect. Any rollback across schema versions
    /// still requires restoring the matching old-schema blob with the old
    /// binary.
    #[serde(
        default = "default_rewrite_creatives",
        skip_serializing_if = "is_default_rewrite_creatives"
    )]
    pub rewrite_creatives: bool,

    /// Refuses the removed `providers` table with a message naming its new
    /// home, so an old configuration fails with the fix rather than as an
    /// unknown field.
    #[serde(default, skip_serializing, deserialize_with = "reject_moved_providers")]
    #[allow(
        dead_code,
        reason = "the field exists so deserialization refuses the removed table"
    )]
    pub(crate) providers: MovedSetting,

    /// Client-visible bidder routes, keyed by bidder code, each naming the
    /// `[demand]` provider the bidder is sent to.
    #[serde(default)]
    pub bidders: BTreeMap<BidderId, BidderRouteConfig>,

    /// The ad server name the legacy test orchestrator runs.
    ///
    /// Production selects its ad server in `[adserver]` instead, so this never
    /// appears in a configuration file.
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) adserver_name: Option<String>,

    /// The demand source names the legacy test orchestrator runs.
    ///
    /// Production compiles its sources from `[demand]` instead, so this never
    /// appears in a configuration file.
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) provider_names: Vec<String>,

    /// Refuses the removed `mediator` setting with a message naming its new
    /// home.
    #[serde(default, skip_serializing, deserialize_with = "reject_moved_mediator")]
    #[allow(
        dead_code,
        reason = "the field exists so deserialization refuses the removed setting"
    )]
    pub(crate) mediator: MovedSetting,

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
            sanitize_creatives: default_sanitize_creatives(),
            rewrite_creatives: default_rewrite_creatives(),
            providers: MovedSetting,
            #[cfg(test)]
            adserver_name: None,
            #[cfg(test)]
            provider_names: Vec::new(),
            bidders: BTreeMap::new(),
            mediator: MovedSetting,
            timeout_ms: default_timeout(),
            creative_store: default_creative_store(),
            allowed_context_keys: HashSet::new(),
        }
    }
}

#[cfg(test)]
impl AuctionConfig {
    /// Whether the legacy test orchestrator runs an ad server.
    pub(crate) fn has_adserver(&self) -> bool {
        self.adserver_name.is_some()
    }
}

/// A setting that has moved elsewhere. It deserializes only by failing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MovedSetting;

fn reject_moved_providers<'de, D>(deserializer: D) -> Result<MovedSetting, D::Error>
where
    D: Deserializer<'de>,
{
    let _ = serde::de::IgnoredAny::deserialize(deserializer)?;
    Err(D::Error::custom(MOVED_PROVIDERS_MESSAGE))
}

fn reject_moved_mediator<'de, D>(deserializer: D) -> Result<MovedSetting, D::Error>
where
    D: Deserializer<'de>,
{
    let _ = serde::de::IgnoredAny::deserialize(deserializer)?;
    Err(D::Error::custom(MOVED_MEDIATOR_MESSAGE))
}

fn default_timeout() -> u32 {
    2000
}

fn default_sanitize_creatives() -> bool {
    false
}

fn default_rewrite_creatives() -> bool {
    true
}

// Omit the default field when it has no effect on the serialized config.
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn creative_processing_defaults() {
        let config: AuctionConfig =
            serde_json::from_value(serde_json::json!({})).expect("should deserialize defaults");

        assert!(
            config.rewrite_creatives,
            "creative rewriting stays enabled by default: existing deployments keep first-party proxying"
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
    fn disabled_rewrite_creatives_is_serialized() {
        let config = AuctionConfig {
            rewrite_creatives: false,
            ..AuctionConfig::default()
        };
        let serialized = serde_json::to_value(config).expect("should serialize disabled rewriting");

        assert_eq!(
            serialized.get("rewrite_creatives"),
            Some(&serde_json::Value::Bool(false)),
            "should preserve an explicit rewrite opt-out"
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

    #[test]
    fn moved_providers_fail_naming_the_demand_table() {
        for providers in [
            serde_json::json!(["prebid"]),
            serde_json::json!({ "pbs_main": { "endpoint": "https://prebid.example" } }),
        ] {
            let error = serde_json::from_value::<AuctionConfig>(serde_json::json!({
                "providers": providers
            }))
            .expect_err("should refuse the moved providers table");

            assert!(
                error.to_string().contains("[demand] provider"),
                "should name the new home: {error}"
            );
        }
    }

    #[test]
    fn moved_mediator_fails_naming_the_adserver_table() {
        let error = serde_json::from_value::<AuctionConfig>(serde_json::json!({
            "mediator": "adserver_mock"
        }))
        .expect_err("should refuse the moved mediator setting");

        assert!(
            error.to_string().contains("[adserver] provider"),
            "should name the new home: {error}"
        );
    }

    #[test]
    fn bidder_routes_round_trip() {
        let config: AuctionConfig = serde_json::from_value(serde_json::json!({
            "bidders": {
                "example-bidder": { "provider": "pbs_main" }
            }
        }))
        .expect("should parse bidder routes");

        assert_eq!(config.bidders.len(), 1);
        let serialized = serde_json::to_value(&config).expect("should serialize");
        assert!(
            serialized.get("providers").is_none() && serialized.get("mediator").is_none(),
            "should never write the moved settings back"
        );
    }
}
