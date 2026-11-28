//! Auction configuration types shared by settings and auction planning.

use serde::de::{
    DeserializeSeed, Error as _, IntoDeserializer as _, MapAccess, SeqAccess, Visitor,
    value::MapAccessDeserializer,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use validator::Validate;

const LEGACY_PROVIDER_LIST_MESSAGE: &str = "Configuration field `auction.providers` uses the removed list schema; migrate to `[auction.providers.<id>]` map entries as described in the CHANGELOG.md breaking migration";

pub use crate::auction::plan::{
    BidderId, BidderRouteConfig, NotificationConfig, ProviderConfig, ProviderId, RoutingMode,
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

    /// Rewrite winning-bid creative asset URLs (images, scripts, styles,
    /// media, iframes, CSS `url()`) to first-party `/first-party/proxy`
    /// endpoints, applied after sanitization when
    /// [`Self::sanitize_creatives`] is enabled.
    ///
    /// Bidder `<base>` `href` removal (a `target` is kept) and, on
    /// `POST /auction`, creative TSJS injection run whenever this or click
    /// rewriting ([`Self::rewrites_auction_clicks`]) is on.
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

    /// Wrap creative click-through links (`<a href>`, `<area href>`) in signed
    /// `/first-party/click` redirects.
    ///
    /// Unset keeps each path's existing behavior: auction creatives follow
    /// [`Self::rewrite_creatives`], and HTML fetched through
    /// `/first-party/proxy` keeps wrapping. An explicit value applies to every
    /// path. Unset is omitted from serialized config blobs, so older binaries
    /// keep loading them; any explicit value is serialized and rejected by
    /// binaries that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_clicks: Option<bool>,

    /// Operator-defined bidder-provider instances, keyed by provider ID.
    #[serde(default, deserialize_with = "deserialize_provider_map")]
    pub providers: BTreeMap<ProviderId, ProviderConfig>,

    /// Client-visible bidder routes, keyed by bidder code.
    #[serde(default)]
    pub bidders: BTreeMap<BidderId, BidderRouteConfig>,

    /// Optional separately registered mediator provider name.
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
    ///
    /// Only config entries from the JS payload whose key appears in this list
    /// are forwarded into the `AuctionRequest.context`. Unrecognised keys are
    /// silently dropped. An empty list blocks all context keys. Keys are
    /// deduplicated and serialized in sorted order so config hashes are stable.
    #[serde(default = "default_allowed_context_keys")]
    pub allowed_context_keys: BTreeSet<String>,
}

impl Default for AuctionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sanitize_creatives: default_sanitize_creatives(),
            rewrite_creatives: default_rewrite_creatives(),
            rewrite_clicks: None,
            providers: BTreeMap::new(),
            bidders: BTreeMap::new(),
            mediator: None,
            timeout_ms: default_timeout(),
            creative_store: default_creative_store(),
            allowed_context_keys: BTreeSet::new(),
        }
    }
}

/// Decode provider execution consistently for settings and direct plans.
///
/// # Errors
///
/// Rejects invalid remote declarations and unavailable internal execution.
pub(crate) fn deserialize_provider_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<ProviderId, ProviderConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ProviderMapVisitor;

    impl<'de> Visitor<'de> for ProviderMapVisitor {
        type Value = BTreeMap<ProviderId, ProviderConfig>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map of auction provider IDs to provider configurations")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut providers = BTreeMap::new();
            while let Some((id, declaration)) = map.next_entry::<ProviderId, DecodedProvider>()? {
                providers.insert(id, declaration.0);
            }
            Ok(providers)
        }

        fn visit_seq<A>(self, _sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            Err(A::Error::custom(LEGACY_PROVIDER_LIST_MESSAGE))
        }
    }

    deserializer.deserialize_any(ProviderMapVisitor)
}

// This wire-only discriminator recognizes unavailable execution without retaining
// a substitute for Auctioneer's shared policy type.
#[derive(Deserialize)]
#[serde(tag = "execution", rename_all = "snake_case")]
enum ProviderDeclaration {
    Remote(ProviderConfig),
    Auctioneer,
}

// Forward original values to Serde so nested typed fields retain duplicate-key
// checks. Append the legacy default only after all authored keys are consumed.
struct DefaultRemoteExecution<A> {
    map: A,
    execution_seen: bool,
    default_value_pending: bool,
}

impl<'de, A> MapAccess<'de> for DefaultRemoteExecution<A>
where
    A: MapAccess<'de>,
{
    type Error = A::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        let key = match self.map.next_key::<String>()? {
            Some(key) => {
                self.execution_seen |= key == "execution";
                key
            }
            None if !self.execution_seen => {
                self.execution_seen = true;
                self.default_value_pending = true;
                "execution".to_owned()
            }
            None => return Ok(None),
        };
        seed.deserialize(key.into_deserializer()).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        if self.default_value_pending {
            self.default_value_pending = false;
            seed.deserialize("remote".into_deserializer())
        } else {
            self.map.next_value_seed(seed)
        }
    }
}

fn deserialize_provider_declaration<'de, D>(deserializer: D) -> Result<ProviderConfig, D::Error>
where
    D: Deserializer<'de>,
{
    struct DeclarationVisitor;

    impl<'de> Visitor<'de> for DeclarationVisitor {
        type Value = ProviderConfig;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an auction provider declaration")
        }

        fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let declaration = ProviderDeclaration::deserialize(MapAccessDeserializer::new(
                DefaultRemoteExecution {
                    map,
                    execution_seen: false,
                    default_value_pending: false,
                },
            ))?;
            match declaration {
                ProviderDeclaration::Remote(provider) => Ok(provider),
                ProviderDeclaration::Auctioneer => Err(A::Error::custom(
                    "AuctioneerUnavailable: this Trusted Server artifact does not include internal Auctioneer execution",
                )),
            }
        }
    }

    deserializer.deserialize_map(DeclarationVisitor)
}

#[derive(Deserialize)]
struct DecodedProvider(
    #[serde(deserialize_with = "deserialize_provider_declaration")] ProviderConfig,
);

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

fn default_allowed_context_keys() -> BTreeSet<String> {
    BTreeSet::new()
}

impl AuctionConfig {
    /// Whether auction creatives (`POST /auction` and inline SSAT/page-bids)
    /// wrap click-through links.
    ///
    /// Unset [`Self::rewrite_clicks`] follows [`Self::rewrite_creatives`].
    #[must_use]
    pub fn rewrites_auction_clicks(&self) -> bool {
        self.rewrite_clicks.unwrap_or(self.rewrite_creatives)
    }

    /// Whether HTML fetched through `/first-party/proxy` wraps click-through
    /// links.
    ///
    /// Unset [`Self::rewrite_clicks`] keeps wrapping, as before the setting
    /// existed.
    #[must_use]
    pub fn rewrites_proxied_clicks(&self) -> bool {
        self.rewrite_clicks.unwrap_or(true)
    }

    #[cfg(test)]
    pub(crate) fn legacy_provider_map(names: &[&str]) -> BTreeMap<ProviderId, ProviderConfig> {
        names
            .iter()
            .map(|name| {
                let id = ProviderId::unchecked_for_legacy_test(name);
                (
                    id,
                    ProviderConfig {
                        protocol: "openrtb-2.6".to_string(),
                        profile: "standard".to_string(),
                        endpoint: format!("https://{name}.example/openrtb2/auction"),
                        timeout_ms: None,
                        routing: RoutingMode::AllEligible,
                        notifications: NotificationConfig::default(),
                        profile_config: serde_json::json!({}),
                    },
                )
            })
            .collect()
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
    fn default_rewrite_clicks_is_unset_and_not_serialized() {
        let config = AuctionConfig::default();

        assert_eq!(
            config.rewrite_clicks, None,
            "should leave click rewriting unset by default"
        );
        let serialized = serde_json::to_value(&config).expect("should serialize defaults");
        assert!(
            serialized.get("rewrite_clicks").is_none(),
            "should omit the unset click setting from serialized config"
        );
    }

    #[test]
    fn explicit_rewrite_clicks_is_serialized() {
        for value in [true, false] {
            let config = AuctionConfig {
                rewrite_clicks: Some(value),
                ..AuctionConfig::default()
            };

            let serialized =
                serde_json::to_value(config).expect("should serialize explicit click setting");

            assert_eq!(
                serialized.get("rewrite_clicks"),
                Some(&serde_json::Value::Bool(value)),
                "should serialize explicit rewrite_clicks = {value}"
            );
        }
    }

    #[test]
    fn click_rewriting_resolves_per_entry_point() {
        // (rewrite_creatives, rewrite_clicks, auction clicks, proxied clicks)
        let cases = [
            (true, None, true, true),
            (false, None, false, true),
            (true, Some(false), false, false),
            (false, Some(true), true, true),
            (true, Some(true), true, true),
            (false, Some(false), false, false),
        ];

        for (rewrite_creatives, rewrite_clicks, auction, proxied) in cases {
            let config = AuctionConfig {
                rewrite_creatives,
                rewrite_clicks,
                ..AuctionConfig::default()
            };

            assert_eq!(
                config.rewrites_auction_clicks(),
                auction,
                "auction clicks for rewrite_creatives={rewrite_creatives} rewrite_clicks={rewrite_clicks:?}"
            );
            assert_eq!(
                config.rewrites_proxied_clicks(),
                proxied,
                "proxied clicks for rewrite_creatives={rewrite_creatives} rewrite_clicks={rewrite_clicks:?}"
            );
        }
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
    fn provider_list_shape_is_rejected() {
        let error = serde_json::from_value::<AuctionConfig>(serde_json::json!({
            "providers": ["prebid"]
        }))
        .expect_err("should reject the removed provider-list schema");

        assert!(
            error.to_string().contains("map") || error.to_string().contains("object"),
            "should require map-shaped providers: {error}"
        );
    }

    fn remote_provider() -> serde_json::Value {
        serde_json::json!({
            "protocol": "openrtb-2.6",
            "profile": "prebid-server",
            "endpoint": "https://prebid.example.com/openrtb2/auction",
            "timeout_ms": 850,
            "routing": "explicit",
            "notifications": {"suppress_all": true, "suppress_seats": ["example-seat"]},
            "profile_config": {"debug": false, "test_mode": true}
        })
    }

    #[test]
    fn execution_selection_remote_preserves_legacy_serialization_and_plan() {
        let mut provider = remote_provider();
        let legacy: AuctionConfig = serde_json::from_value(serde_json::json!({
            "providers": {"pbs-main": provider.clone()}
        }))
        .expect("should parse the legacy remote declaration");
        provider["execution"] = serde_json::json!("remote");
        let explicit: AuctionConfig = serde_json::from_value(serde_json::json!({
            "providers": {"pbs-main": provider.clone()}
        }))
        .expect("should accept explicit remote execution");
        let legacy_wire = serde_json::to_value(legacy).expect("should serialize legacy config");
        let explicit_wire = serde_json::to_value(explicit).expect("should serialize remote config");
        assert_eq!(
            explicit_wire, legacy_wire,
            "should preserve the existing remote wire shape"
        );
        assert!(
            explicit_wire["providers"]["pbs-main"]
                .get("execution")
                .is_none(),
            "should omit the normalized remote discriminator"
        );

        let plan: crate::auction::plan::AuctionPlanConfig =
            serde_json::from_value(serde_json::json!({
                "timeout_ms": 1000, "providers": {"pbs-main": provider}
            }))
            .expect("should accept the same selection at the direct plan boundary");
        let compiled = crate::auction::plan::AuctionPlan::compile(plan)
            .expect("should compile the existing remote provider");
        assert_eq!(
            compiled.providers().len(),
            1,
            "should retain the remote provider"
        );
    }

    #[test]
    fn execution_selection_toml_accepts_remote_and_refuses_internal() {
        let remote = r#"
[providers.pbs-main]
execution = "remote"
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"
"#;
        let config: AuctionConfig =
            toml::from_str(remote).expect("should accept explicit remote execution in TOML");
        let serialized = toml::to_string(&config).expect("should serialize remote TOML");
        assert!(
            !serialized.contains("execution"),
            "should omit normalized remote execution"
        );
        let internal = r#"
[providers.local]
execution = "auctioneer"
auctioneer = false
account = "invalid"
artifact_identity = 42
"#;
        let error = toml::from_str::<AuctionConfig>(internal)
            .expect_err("should refuse unavailable internal execution before policy validation");
        assert!(
            error.to_string().contains("AuctioneerUnavailable"),
            "should identify unavailable engine support: {error}"
        );
    }

    #[test]
    fn execution_selection_internal_refusal_precedes_branch_field_validation() {
        let fields = serde_json::to_string(&serde_json::json!({
            "protocol": 42, "routing": "all_eligible", "auctioneer": false,
            "account": "invalid", "artifact_identity": 42
        }))
        .expect("should encode intentionally invalid branch fields");
        let fields = fields.trim_start_matches('{').trim_end_matches('}');
        let discriminator = r#""execution":"auctioneer""#;
        for declaration in [
            format!("{{{discriminator},{fields}}}"),
            format!("{{{fields},{discriminator}}}"),
        ] {
            let input = format!(r#"{{"providers":{{"local":{declaration}}}}}"#);
            let error = serde_json::from_str::<AuctionConfig>(&input)
                .expect_err("should refuse the unavailable engine in either field order");
            assert!(
                error.to_string().contains("AuctioneerUnavailable"),
                "should select execution before typed branch decoding: {error}"
            );
            let input = format!(r#"{{"timeout_ms":1000,"providers":{{"local":{declaration}}}}}"#);
            let error = serde_json::from_str::<crate::auction::plan::AuctionPlanConfig>(&input)
                .expect_err("should refuse the unavailable engine at the direct plan boundary");
            assert!(
                error.to_string().contains("AuctioneerUnavailable"),
                "should share execution refusal with direct plans: {error}"
            );
        }
    }

    #[test]
    fn execution_selection_rejects_unknown_or_non_string_modes() {
        for execution in [
            serde_json::json!("unknown"),
            serde_json::json!("Remote"),
            serde_json::json!(null),
            serde_json::json!(42),
            serde_json::json!(false),
            serde_json::json!([]),
        ] {
            let mut provider = remote_provider();
            provider["execution"] = execution.clone();
            let error = serde_json::from_value::<AuctionConfig>(serde_json::json!({
                "providers": {"pbs-main": provider}
            }))
            .expect_err("should reject invalid execution selection");
            assert!(
                !error.to_string().contains("AuctioneerUnavailable"),
                "should distinguish invalid selection from unavailable support: {execution}: {error}"
            );
        }
    }

    #[test]
    fn execution_selection_remote_retains_strict_fields() {
        for execution in [None, Some("remote")] {
            for field in ["auctioneer", "account", "artifact_identity", "unexpected"] {
                let mut provider = remote_provider();
                if let Some(execution) = execution {
                    provider["execution"] = serde_json::json!(execution);
                }
                provider[field] = serde_json::json!({});
                let error = serde_json::from_value::<AuctionConfig>(serde_json::json!({
                    "providers": {"pbs-main": provider}
                }))
                .expect_err("should reject local or unknown fields on remote execution");
                assert!(
                    error.to_string().contains(field),
                    "should name the invalid remote field: {error}"
                );
            }
        }
    }

    #[test]
    fn execution_selection_remote_rejects_duplicate_fields() {
        // String input is necessary because json! discards duplicate object keys.
        for duplicate in [
            r#""notifications":{"suppress_all":false,"suppress_all":true}"#,
            r#""execution":"remote","execution":"remote""#,
            r#""execution":"remote","profile":"standard","profile":"prebid-server""#,
        ] {
            let input = format!(
                r#"{{"providers":{{"pbs-main":{{{duplicate},"protocol":"openrtb-2.6","endpoint":"https://prebid.example.com/auction"}}}}}}"#
            );
            let error = serde_json::from_str::<AuctionConfig>(&input)
                .expect_err("should preserve duplicate-field rejection");
            assert!(
                error.to_string().contains("duplicate field"),
                "should reject duplicate fields: {error}"
            );
        }
    }

    #[test]
    fn map_schema_round_trips_provider_and_bidder_routes() {
        let config: AuctionConfig = serde_json::from_value(serde_json::json!({
            "providers": {
                "pbs-main": {
                    "protocol": "openrtb-2.6",
                    "profile": "prebid-server",
                    "endpoint": "https://prebid.example/openrtb2/auction"
                }
            },
            "bidders": {
                "example-bidder": { "provider": "pbs-main" }
            }
        }))
        .expect("should parse map-shaped auction config");

        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.bidders.len(), 1);
    }
}
