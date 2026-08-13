//! Target-independent config-first auction plan compiler.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use super::profile::{CompiledOpenRtbProfile, ProfileTimeoutDefault, find_profile};
use crate::error::TrustedServerError;
use crate::platform::{AuctionTargetId, PlatformBackendSpec};
use crate::settings::RequestSigning;

const MAX_ID_BYTES: usize = 128;
const MAX_SUPPRESS_SEATS: usize = 128;
const MAX_SUPPRESS_SEAT_BYTES: usize = 128;
const MOCK_MEDIATOR_ID: &str = "adserver_mock";
const RESERVED_BROWSER_ENVELOPE_BIDDER_ID: &str = "trustedServer";

/// Validated operator-defined provider identifier.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Display)]
pub struct ProviderId(String);

impl ProviderId {
    /// Borrow the validated identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn unchecked_for_legacy_test(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl FromStr for ProviderId {
    type Err = Report<TrustedServerError>;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = !value.is_empty()
            && value.len() <= 63
            && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && value
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
        if !valid {
            return Err(configuration_error(format!(
                "provider ID `{value}` must match ^[a-z][a-z0-9-]{{0,62}}$"
            )));
        }
        Ok(Self(value.to_string()))
    }
}

impl<'de> Deserialize<'de> for ProviderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ProviderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Validated client-visible bidder identifier.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Display)]
pub struct BidderId(String);

impl BidderId {
    /// Borrow the validated identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for BidderId {
    type Err = Report<TrustedServerError>;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || value.len() > MAX_ID_BYTES
            || value.chars().any(char::is_control)
            || value.trim() != value
        {
            return Err(configuration_error(format!(
                "bidder ID must be nonempty, at most {MAX_ID_BYTES} UTF-8 bytes, contain no control characters, and have no surrounding whitespace"
            )));
        }
        Ok(Self(value.to_string()))
    }
}

impl<'de> Deserialize<'de> for BidderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for BidderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Raw config-first provider declaration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Protocol identifier. Version one accepts only `openrtb-2.6`.
    pub protocol: String,
    /// Registered profile identifier.
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Fixed provider endpoint.
    pub endpoint: String,
    /// Optional profile-default timeout override.
    #[serde(default)]
    pub timeout_ms: Option<u32>,
    /// Slot routing mode.
    #[serde(default)]
    pub routing: RoutingMode,
    /// Common `OpenRTB` notification policy.
    #[serde(default)]
    pub notifications: NotificationConfig,
    /// Selected profile's typed configuration object.
    #[serde(default = "empty_object")]
    pub profile_config: Value,
}

/// Raw central bidder route.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BidderRouteConfig {
    /// Referenced provider identifier.
    pub provider: ProviderId,
}

/// Provider slot routing behavior.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    /// Route only centrally assigned or trusted demand.
    #[default]
    Explicit,
    /// Route every banner-compatible slot.
    AllEligible,
}

/// Common normalized-notification suppression configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationConfig {
    /// Suppress notification URLs for every normalized bid.
    #[serde(default)]
    pub suppress_all: bool,
    /// Suppress notification URLs for exact returned-seat matches.
    #[serde(default)]
    pub suppress_seats: Vec<String>,
}

/// Raw internal input for target-independent plan compilation.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuctionPlanConfig {
    /// Auction-wide logical timeout.
    pub timeout_ms: u32,
    /// Operator-defined provider instances.
    #[serde(default)]
    pub providers: BTreeMap<ProviderId, ProviderConfig>,
    /// Client bidder-to-provider routes.
    #[serde(default)]
    pub bidders: BTreeMap<BidderId, BidderRouteConfig>,
    /// Existing separately registered mock mediator.
    #[serde(default)]
    pub mediator: Option<String>,
    /// Existing global Trusted Server signing configuration.
    #[serde(default)]
    pub request_signing: Option<RequestSigning>,
}

/// Canonical absolute provider endpoint.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CanonicalProviderEndpoint(Url);

impl CanonicalProviderEndpoint {
    /// Borrow the canonical endpoint string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn url(&self) -> &Url {
        &self.0
    }
}

/// Closed first-version protocol plan.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProtocolPlan {
    /// `OpenRTB` version 2.6 subset.
    OpenRtb26,
}

/// Immutable common notification policy.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct NotificationPolicy {
    /// Suppress notification URLs for every bid.
    pub suppress_all: bool,
    /// Exact returned seats whose notification URLs are suppressed.
    pub suppress_seats: BTreeSet<String>,
}

/// Immutable compiled provider instance.
#[derive(Debug, Clone)]
pub struct ProviderPlan {
    /// Provider identity.
    pub id: ProviderId,
    /// Canonical endpoint.
    pub endpoint: CanonicalProviderEndpoint,
    /// Resolved profile-default or explicit timeout.
    pub timeout_ms: u32,
    /// Slot routing mode.
    pub routing: RoutingMode,
    /// Common notification policy.
    pub notifications: NotificationPolicy,
    /// Compiled protocol behavior.
    pub protocol: ProtocolPlan,
    /// Compiled typed profile behavior.
    pub profile: CompiledOpenRtbProfile,
}

/// Immutable target-independent auction plan.
#[derive(Debug, Clone)]
pub struct AuctionPlan {
    enabled: bool,
    providers: Vec<ProviderPlan>,
    bidder_routes: BTreeMap<BidderId, usize>,
    signing_enabled: bool,
    mediator: Option<String>,
}

impl AuctionPlan {
    /// Return whether auction execution is enabled.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Compile a deterministic plan without adapter-specific validation.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for invalid identifiers, protocol/profile
    /// declarations, endpoints, profile configuration, routes, notifications,
    /// signing structure, or mediator selection.
    pub fn compile(config: AuctionPlanConfig) -> Result<Self, Report<TrustedServerError>> {
        if config.timeout_ms == 0 {
            return Err(configuration_error(
                "auction timeout_ms must be greater than zero",
            ));
        }
        validate_mediator(config.mediator.as_deref())?;
        let signing_enabled = compile_signing_enabled(config.request_signing.as_ref())?;
        let mut providers = Vec::with_capacity(config.providers.len());
        let mut provider_indices = BTreeMap::new();
        for (id, raw) in config.providers {
            if raw.protocol != "openrtb-2.6" {
                return Err(configuration_error(format!(
                    "provider `{id}` uses unsupported protocol `{}`",
                    raw.protocol
                )));
            }
            let registration = find_profile(&raw.profile).ok_or_else(|| {
                configuration_error(format!(
                    "provider `{id}` uses unknown OpenRTB profile `{}`",
                    raw.profile
                ))
            })?;
            if !raw.profile_config.is_object() {
                return Err(configuration_error(format!(
                    "provider `{id}` profile_config must be an object"
                )));
            }
            let endpoint = canonicalize_endpoint(&id, registration.id, &raw.endpoint)?;
            let timeout_ms = raw
                .timeout_ms
                .unwrap_or(match registration.default_timeout {
                    ProfileTimeoutDefault::Auction => config.timeout_ms,
                    ProfileTimeoutDefault::Fixed(value) => value,
                });
            if timeout_ms == 0 {
                return Err(configuration_error(format!(
                    "provider `{id}` timeout_ms must be greater than zero"
                )));
            }
            let notifications = compile_notifications(&id, raw.notifications)?;
            let profile = registration.compile(&raw.profile_config)?;
            let index = providers.len();
            provider_indices.insert(id.clone(), index);
            providers.push(ProviderPlan {
                id,
                endpoint,
                timeout_ms,
                routing: raw.routing,
                notifications,
                protocol: ProtocolPlan::OpenRtb26,
                profile,
            });
        }
        let mut bidder_routes = BTreeMap::new();
        for (bidder, route) in config.bidders {
            if bidder.as_str() == RESERVED_BROWSER_ENVELOPE_BIDDER_ID {
                return Err(configuration_error(format!(
                    "bidder ID `{RESERVED_BROWSER_ENVELOPE_BIDDER_ID}` is reserved for browser admission"
                )));
            }
            let provider_index =
                provider_indices
                    .get(&route.provider)
                    .copied()
                    .ok_or_else(|| {
                        configuration_error(format!(
                            "bidder `{bidder}` references unknown provider `{}`",
                            route.provider
                        ))
                    })?;
            bidder_routes.insert(bidder, provider_index);
        }
        Ok(Self {
            enabled: true,
            providers,
            bidder_routes,
            signing_enabled,
            mediator: config.mediator,
        })
    }
}

impl ProviderPlan {
    /// Build the canonical backend specification with the configured timeout.
    #[must_use]
    pub(crate) fn backend_spec(&self) -> PlatformBackendSpec {
        self.backend_spec_with_transport_timeout(self.timeout_ms)
    }

    /// Build the canonical backend specification with request-local transport timers.
    #[must_use]
    pub(crate) fn backend_spec_with_transport_timeout(
        &self,
        transport_timeout_ms: u32,
    ) -> PlatformBackendSpec {
        let endpoint = self.endpoint.url();
        let timeout = Duration::from_millis(u64::from(transport_timeout_ms));
        PlatformBackendSpec {
            scheme: endpoint.scheme().to_owned(),
            host: endpoint
                .host_str()
                .expect("should retain validated provider endpoint host")
                .to_owned(),
            port: endpoint.port(),
            host_header_override: None,
            certificate_check: true,
            first_byte_timeout: timeout,
            between_bytes_timeout: timeout,
            discriminator: Some(self.id.as_str().to_owned()),
        }
    }
}

impl AuctionPlan {
    /// Validate adapter capabilities and backend-name correlation before I/O.
    ///
    /// Each provider is predicted from a canonical backend specification using
    /// its exact configured provider timeout as both transport timers and its
    /// provider ID as the stable discriminator. These transport timers do not
    /// replace the auction-wide logical budget.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the target cannot fan out to every
    /// configured provider, backend prediction fails, or two predicted names
    /// collide.
    pub fn validate_for_target(
        &self,
        target_id: AuctionTargetId,
    ) -> Result<(), Report<TrustedServerError>> {
        if !self.enabled {
            return Ok(());
        }
        let target = target_id.descriptor();
        if self.providers.len() > 1 && !target.capabilities().supports_concurrent_provider_fanout()
        {
            return Err(configuration_error(format!(
                "auction target `{}` does not support concurrent provider fanout; configured {} providers",
                target_id.adapter_id(),
                self.providers.len()
            )));
        }

        let mut predicted_names = BTreeMap::<String, &ProviderId>::new();
        for provider in &self.providers {
            let spec = provider.backend_spec();
            let prediction = target.naming_policy().predict(&spec).change_context(
                TrustedServerError::Configuration {
                    message: format!(
                        "provider `{}` backend prediction failed for target `{}`",
                        provider.id,
                        target_id.adapter_id()
                    ),
                },
            )?;
            if let Some(existing) = predicted_names.insert(prediction.name.clone(), &provider.id) {
                return Err(configuration_error(format!(
                    "providers `{existing}` and `{}` predict the same backend name `{}` for target `{}`",
                    provider.id,
                    prediction.name,
                    target_id.adapter_id()
                )));
            }
        }
        Ok(())
    }

    /// Borrow compiled providers in deterministic provider-ID order.
    #[must_use]
    pub fn providers(&self) -> &[ProviderPlan] {
        &self.providers
    }

    /// Borrow a compiled provider by its validated identity.
    #[must_use]
    pub(crate) fn provider(&self, id: &ProviderId) -> Option<&ProviderPlan> {
        self.providers.iter().find(|provider| provider.id == *id)
    }

    /// Return whether any compiled provider uses the named profile.
    ///
    /// This narrow query allows capability activation to follow the validated
    /// plan without exposing profile configuration.
    #[must_use]
    pub fn has_profile(&self, profile_id: &str) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.profile.id() == profile_id)
    }

    /// Borrow validated client-visible bidder route codes in deterministic order.
    ///
    /// This intentionally exposes route keys rather than provider identities or
    /// profile configuration for the browser Prebid injection boundary.
    pub(crate) fn browser_bidder_codes(&self) -> impl Iterator<Item = &str> {
        self.bidder_routes.keys().map(BidderId::as_str)
    }

    /// Resolve a bidder route to a compiled provider.
    #[must_use]
    pub fn provider_for_bidder(&self, bidder: &BidderId) -> Option<&ProviderPlan> {
        self.bidder_routes
            .get(bidder)
            .and_then(|index| self.providers.get(*index))
    }

    /// Return whether auction-wide signing is enabled.
    #[must_use]
    pub fn signing_enabled(&self) -> bool {
        self.signing_enabled
    }

    /// Borrow the separately validated static mediator identifier.
    #[must_use]
    pub fn mediator(&self) -> Option<&str> {
        self.mediator.as_deref()
    }
}

fn default_profile() -> String {
    "standard".to_string()
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

fn configuration_error(message: impl Into<String>) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: message.into(),
    })
}

fn compile_signing_enabled(
    request_signing: Option<&RequestSigning>,
) -> Result<bool, Report<TrustedServerError>> {
    let Some(request_signing) = request_signing else {
        return Ok(false);
    };
    if request_signing.enabled
        && (request_signing.config_store_id.trim().is_empty()
            || request_signing.secret_store_id.trim().is_empty())
    {
        return Err(configuration_error(
            "enabled request_signing requires nonblank config_store_id and secret_store_id",
        ));
    }
    Ok(request_signing.enabled)
}

fn validate_mediator(mediator: Option<&str>) -> Result<(), Report<TrustedServerError>> {
    if mediator.is_some_and(|value| value != MOCK_MEDIATOR_ID) {
        return Err(configuration_error(format!(
            "auction mediator must be `{MOCK_MEDIATOR_ID}` when configured"
        )));
    }
    Ok(())
}

fn canonicalize_endpoint(
    provider_id: &ProviderId,
    profile_id: &str,
    value: &str,
) -> Result<CanonicalProviderEndpoint, Report<TrustedServerError>> {
    let mut endpoint = Url::parse(value).map_err(|error| {
        configuration_error(format!(
            "provider `{provider_id}` endpoint must be an absolute HTTPS URL: {error}"
        ))
    })?;
    if endpoint.scheme() != "https"
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(configuration_error(format!(
            "provider `{provider_id}` endpoint must be absolute HTTPS with a host and no credentials or fragment"
        )));
    }
    if profile_id == "aps"
        && endpoint
            .path()
            .trim_end_matches('/')
            .ends_with("/e/dtb/bid")
    {
        return Err(configuration_error(format!(
            "provider `{provider_id}` uses unsupported legacy APS endpoint `/e/dtb/bid`"
        )));
    }
    endpoint.set_fragment(None);
    Ok(CanonicalProviderEndpoint(endpoint))
}

fn compile_notifications(
    provider_id: &ProviderId,
    config: NotificationConfig,
) -> Result<NotificationPolicy, Report<TrustedServerError>> {
    if config.suppress_seats.len() > MAX_SUPPRESS_SEATS {
        return Err(configuration_error(format!(
            "provider `{provider_id}` notifications.suppress_seats exceeds {MAX_SUPPRESS_SEATS} entries"
        )));
    }
    let mut seats = BTreeSet::new();
    for seat in config.suppress_seats {
        if seat.is_empty()
            || seat.len() > MAX_SUPPRESS_SEAT_BYTES
            || seat.chars().any(|character| character.is_ascii_control())
        {
            return Err(configuration_error(format!(
                "provider `{provider_id}` notification seat must be nonempty, at most {MAX_SUPPRESS_SEAT_BYTES} UTF-8 bytes, and contain no ASCII control characters"
            )));
        }
        if !seats.insert(seat.clone()) {
            return Err(configuration_error(format!(
                "provider `{provider_id}` notification seat `{seat}` is duplicated"
            )));
        }
    }
    Ok(NotificationPolicy {
        suppress_all: config.suppress_all,
        suppress_seats: seats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::profile::CompiledOpenRtbProfile;

    fn provider(profile: &str) -> ProviderConfig {
        ProviderConfig {
            protocol: "openrtb-2.6".to_string(),
            profile: profile.to_string(),
            endpoint: "https://bid.example/openrtb2/auction".to_string(),
            timeout_ms: None,
            routing: RoutingMode::Explicit,
            notifications: NotificationConfig::default(),
            profile_config: empty_object(),
        }
    }

    fn config(providers: BTreeMap<ProviderId, ProviderConfig>) -> AuctionPlanConfig {
        AuctionPlanConfig {
            timeout_ms: 1500,
            providers,
            ..AuctionPlanConfig::default()
        }
    }

    fn id(value: &str) -> ProviderId {
        ProviderId::from_str(value).expect("should parse provider ID")
    }

    fn bidder(value: &str) -> BidderId {
        BidderId::from_str(value).expect("should parse bidder ID")
    }

    fn nested_object(levels: usize) -> Value {
        let mut value = Value::String("leaf".to_string());
        for level in 0..levels {
            value = Value::Object(serde_json::Map::from_iter([(
                format!("level-{level}"),
                value,
            )]));
        }
        value
    }

    fn nested_array(levels: usize) -> Value {
        let mut value = Value::String("leaf".to_string());
        for _ in 0..levels {
            value = Value::Array(vec![value]);
        }
        value
    }

    #[test]
    fn target_validation_accepts_fanout_and_rejects_unsupported_targets() {
        let providers = BTreeMap::from([
            (id("provider-one"), provider("standard")),
            (id("provider-two"), provider("standard")),
        ]);
        let plan = AuctionPlan::compile(config(providers)).expect("should compile plan");

        assert!(
            plan.validate_for_target(crate::platform::AuctionTargetId::Fastly)
                .is_ok(),
            "Fastly should accept provider fanout"
        );
        assert!(
            plan.validate_for_target(crate::platform::AuctionTargetId::Axum)
                .is_ok(),
            "Axum should accept provider fanout"
        );
        for target in [
            crate::platform::AuctionTargetId::Cloudflare,
            crate::platform::AuctionTargetId::Spin,
        ] {
            let error = plan
                .validate_for_target(target)
                .expect_err("should reject unsupported provider fanout");
            assert!(
                error.to_string().contains("fanout"),
                "should explain fanout rejection: {error:?}"
            );
        }
    }

    #[test]
    fn disabled_target_validation_skips_fanout_and_collision_checks() {
        let providers = BTreeMap::from([
            (id("provider-one"), provider("standard")),
            (id("provider-two"), provider("standard")),
        ]);
        let disabled = AuctionPlan::compile(config(providers))
            .expect("should compile plan")
            .with_enabled(false);

        for target in [
            crate::platform::AuctionTargetId::Cloudflare,
            crate::platform::AuctionTargetId::Spin,
        ] {
            disabled
                .validate_for_target(target)
                .expect("disabled dormant providers should skip target validation");
        }

        let provider = disabled.providers[0].clone();
        let disabled_collision = AuctionPlan {
            enabled: false,
            providers: vec![provider.clone(), provider],
            bidder_routes: BTreeMap::new(),
            signing_enabled: false,
            mediator: None,
        };
        disabled_collision
            .validate_for_target(crate::platform::AuctionTargetId::Axum)
            .expect("disabled dormant providers should skip collision validation");
    }

    #[test]
    fn target_validation_keeps_same_origin_timeout_profile_instances_distinct() {
        let shared = ProviderConfig {
            timeout_ms: Some(777),
            ..provider("standard")
        };
        let plan = AuctionPlan::compile(config(BTreeMap::from([
            (id("provider-one"), shared.clone()),
            (id("provider-two"), shared),
        ])))
        .expect("should compile same-origin provider instances");

        for target in [
            crate::platform::AuctionTargetId::Fastly,
            crate::platform::AuctionTargetId::Axum,
        ] {
            plan.validate_for_target(target)
                .expect("provider ID discriminators should prevent predicted collisions");
        }
    }

    #[test]
    fn target_validation_rejects_predicted_name_collisions() {
        let compiled = AuctionPlan::compile(config(BTreeMap::from([(
            id("provider-a"),
            provider("standard"),
        )])))
        .expect("should compile plan");
        let provider = compiled.providers[0].clone();
        // The compiler prevents duplicate provider IDs. Construct the otherwise
        // impossible duplicate internally to pin validation's defense-in-depth
        // collision rejection independently of compiler invariants.
        let collision_plan = AuctionPlan {
            enabled: true,
            providers: vec![provider.clone(), provider],
            bidder_routes: BTreeMap::new(),
            signing_enabled: false,
            mediator: None,
        };

        let error = collision_plan
            .validate_for_target(crate::platform::AuctionTargetId::Axum)
            .expect_err("should reject predicted backend collision");
        assert!(error.to_string().contains("same backend name"));
    }

    #[test]
    fn provider_id_enforces_exact_grammar_and_bounds() {
        for valid in ["a", "pbs-primary", &format!("a{}", "0".repeat(62))] {
            assert!(ProviderId::from_str(valid).is_ok(), "should accept {valid}");
        }
        for invalid in [
            "",
            "A",
            "1provider",
            "provider_name",
            "provider.name",
            "provider/one",
            &format!("a{}", "0".repeat(63)),
        ] {
            assert!(
                ProviderId::from_str(invalid).is_err(),
                "should reject {invalid}"
            );
        }
    }

    #[test]
    fn bidder_id_enforces_admission_bounds() {
        assert!(BidderId::from_str("exampleBidder").is_ok());
        for invalid in ["", " bidder", "bidder\n", &"a".repeat(129)] {
            assert!(BidderId::from_str(invalid).is_err());
        }
    }

    #[test]
    fn compiler_rejects_exact_reserved_browser_envelope_bidder_id() {
        let mut raw = config(BTreeMap::from([(id("one"), provider("standard"))]));
        raw.bidders.insert(
            bidder("trustedServer"),
            BidderRouteConfig {
                provider: id("one"),
            },
        );
        assert!(
            AuctionPlan::compile(raw).is_err(),
            "exact reserved bidder ID should be rejected"
        );

        let mut case_distinct = config(BTreeMap::from([(id("one"), provider("standard"))]));
        case_distinct.bidders.insert(
            bidder("TrustedServer"),
            BidderRouteConfig {
                provider: id("one"),
            },
        );
        assert!(
            AuctionPlan::compile(case_distinct).is_ok(),
            "reserved bidder comparison should remain case-sensitive"
        );
    }

    #[test]
    fn compiler_orders_providers_and_routes_deterministically() {
        let mut providers = BTreeMap::new();
        providers.insert(id("z-provider"), provider("standard"));
        providers.insert(id("a-provider"), provider("standard"));
        let mut raw = config(providers);
        raw.bidders.insert(
            bidder("z-bidder"),
            BidderRouteConfig {
                provider: id("z-provider"),
            },
        );
        raw.bidders.insert(
            bidder("a-bidder"),
            BidderRouteConfig {
                provider: id("a-provider"),
            },
        );
        let plan = AuctionPlan::compile(raw).expect("should compile deterministic plan");
        assert_eq!(plan.providers()[0].id.as_str(), "a-provider");
        assert_eq!(plan.providers()[1].id.as_str(), "z-provider");
        assert_eq!(
            plan.browser_bidder_codes().collect::<Vec<_>>(),
            vec!["a-bidder", "z-bidder"],
            "browser query should return deduplicated route codes in deterministic order"
        );
        assert_eq!(
            plan.provider_for_bidder(&bidder("a-bidder"))
                .map(|provider| provider.id.as_str()),
            Some("a-provider")
        );
    }

    #[test]
    fn compiler_supports_two_instances_of_the_same_profile() {
        let mut providers = BTreeMap::new();
        providers.insert(id("pbs-a"), provider("prebid-server"));
        providers.insert(id("pbs-b"), provider("prebid-server"));
        let plan = AuctionPlan::compile(config(providers)).expect("should compile two PBS plans");
        assert_eq!(plan.providers().len(), 2);
        assert!(
            plan.providers().iter().all(|provider| matches!(
                provider.profile,
                CompiledOpenRtbProfile::PrebidServer(_)
            ))
        );
    }

    #[test]
    fn profile_defaults_and_explicit_timeout_override_are_resolved() {
        let mut providers = BTreeMap::new();
        providers.insert(id("standard-one"), provider("standard"));
        providers.insert(id("pbs-one"), provider("prebid-server"));
        providers.insert(
            id("aps-one"),
            ProviderConfig {
                endpoint: "https://aps.example/e/pb/bid".to_string(),
                profile_config: serde_json::json!({"account_id": "example-account"}),
                ..provider("aps")
            },
        );
        providers.insert(
            id("pbs-override"),
            ProviderConfig {
                timeout_ms: Some(321),
                ..provider("prebid-server")
            },
        );
        let plan = AuctionPlan::compile(config(providers)).expect("should resolve timeouts");
        let timeouts = plan
            .providers()
            .iter()
            .map(|provider| (provider.id.as_str(), provider.timeout_ms))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(timeouts["standard-one"], 1500);
        assert_eq!(timeouts["pbs-one"], 1000);
        assert_eq!(timeouts["aps-one"], 800);
        assert_eq!(timeouts["pbs-override"], 321);
    }

    #[test]
    fn profile_registry_is_independent_of_browser_configuration() {
        let ids = crate::auction::profile::profile_registrations()
            .iter()
            .map(|registration| registration.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["standard", "prebid-server", "aps"]);
        let mut providers = BTreeMap::new();
        providers.insert(id("pbs"), provider("prebid-server"));
        let plan = AuctionPlan::compile(config(providers))
            .expect("should compile without Settings or browser integration state");
        assert!(!plan.has_profile("aps"));

        let mut providers = BTreeMap::new();
        providers.insert(
            id("aps-instance"),
            ProviderConfig {
                endpoint: "https://aps.example/e/pb/bid".to_string(),
                profile_config: serde_json::json!({"account_id": "example-account"}),
                ..provider("aps")
            },
        );
        let plan = AuctionPlan::compile(config(providers)).expect("should compile APS plan");
        assert!(
            plan.has_profile("aps"),
            "validated plan should expose APS renderer capability"
        );
        assert!(!plan.has_profile("prebid-server"));
    }

    #[test]
    fn compiler_rejects_unknown_protocol_profile_and_route() {
        let mut unknown_protocol = provider("standard");
        unknown_protocol.protocol = "openrtb-2.5".to_string();
        assert!(
            AuctionPlan::compile(config(BTreeMap::from([(id("one"), unknown_protocol)]))).is_err()
        );
        assert!(
            AuctionPlan::compile(config(BTreeMap::from([(id("one"), provider("unknown"))])))
                .is_err()
        );
        let mut raw = config(BTreeMap::from([(id("one"), provider("standard"))]));
        raw.bidders.insert(
            bidder("example"),
            BidderRouteConfig {
                provider: id("missing"),
            },
        );
        assert!(AuctionPlan::compile(raw).is_err());
    }

    #[test]
    fn compiler_canonicalizes_https_endpoints_and_rejects_unsafe_forms() {
        let mut canonical = provider("standard");
        canonical.endpoint = "https://BID.EXAMPLE:443/path".to_string();
        let plan = AuctionPlan::compile(config(BTreeMap::from([(id("one"), canonical)])))
            .expect("should canonicalize endpoint");
        assert_eq!(
            plan.providers()[0].endpoint.as_str(),
            "https://bid.example/path"
        );
        for endpoint in [
            "http://bid.example/path",
            "https://",
            "https://user@bid.example/path",
            "https://bid.example/path#fragment",
            "/relative",
        ] {
            let mut raw_provider = provider("standard");
            raw_provider.endpoint = endpoint.to_string();
            assert!(
                AuctionPlan::compile(config(BTreeMap::from([(id("one"), raw_provider)]))).is_err(),
                "should reject {endpoint}"
            );
        }
        let mut aps = provider("aps");
        aps.endpoint = "https://aps.example/e/dtb/bid".to_string();
        aps.profile_config = serde_json::json!({"account_id": "example-account"});
        assert!(AuctionPlan::compile(config(BTreeMap::from([(id("aps"), aps)]))).is_err());
    }

    #[test]
    fn standard_extensions_are_typed_bounded_and_cannot_claim_reserved_fields() {
        let mut valid = provider("standard");
        valid.profile_config = serde_json::json!({
            "request_ext": {"fictional_account": "example"},
            "imp_ext": {"placement_group": "display"}
        });
        let plan = AuctionPlan::compile(config(BTreeMap::from([(id("one"), valid)])))
            .expect("should compile static extensions");
        let CompiledOpenRtbProfile::Standard(standard) = &plan.providers()[0].profile else {
            panic!("should compile standard profile")
        };
        assert_eq!(
            standard.request_ext.as_object()["fictional_account"],
            "example"
        );

        let mut standard_owned_fields = provider("standard");
        standard_owned_fields.profile_config = serde_json::json!({
            "request_ext": {
                "account": "example-account",
                "sdk": {"source": "example"},
                "prebid": {"example": true}
            },
            "imp_ext": {"prebid": {"example": true}}
        });
        AuctionPlan::compile(config(BTreeMap::from([(
            id("standard-owned-fields"),
            standard_owned_fields,
        )])))
        .expect("should allow standard static extensions outside common-owned fields");

        for profile_config in [
            serde_json::json!({"request_ext": "bad"}),
            serde_json::json!({"request_ext": {"trusted_server": {}}}),
        ] {
            let mut invalid = provider("standard");
            invalid.profile_config = profile_config;
            assert!(AuctionPlan::compile(config(BTreeMap::from([(id("one"), invalid)]))).is_err());
        }
        let oversized = "x".repeat(16 * 1024);
        let mut invalid = provider("standard");
        invalid.profile_config = serde_json::json!({"request_ext": {"value": oversized}});
        assert!(AuctionPlan::compile(config(BTreeMap::from([(id("one"), invalid)]))).is_err());

        let too_many_keys = (0..257)
            .map(|index| (format!("key-{index}"), Value::Bool(true)))
            .collect::<serde_json::Map<_, _>>();
        let mut invalid = provider("standard");
        invalid.profile_config = serde_json::json!({"imp_ext": too_many_keys});
        assert!(AuctionPlan::compile(config(BTreeMap::from([(id("one"), invalid)]))).is_err());
    }

    #[test]
    fn standard_extension_depth_counts_container_levels() {
        let mut object_valid = provider("standard");
        object_valid.profile_config = serde_json::json!({"request_ext": nested_object(8)});
        AuctionPlan::compile(config(BTreeMap::from([(id("object-valid"), object_valid)])))
            .expect("should accept eight nested object levels");

        let mut object_invalid = provider("standard");
        object_invalid.profile_config = serde_json::json!({"request_ext": nested_object(9)});
        assert!(
            AuctionPlan::compile(config(BTreeMap::from([(
                id("object-invalid"),
                object_invalid,
            )])))
            .is_err(),
            "should reject nine nested object levels"
        );

        let mut array_valid = provider("standard");
        array_valid.profile_config = serde_json::json!({"request_ext": {"value": nested_array(7)}});
        AuctionPlan::compile(config(BTreeMap::from([(id("array-valid"), array_valid)])))
            .expect("should accept one object plus seven nested array levels");

        let mut array_invalid = provider("standard");
        array_invalid.profile_config =
            serde_json::json!({"request_ext": {"value": nested_array(8)}});
        assert!(
            AuctionPlan::compile(config(BTreeMap::from([(
                id("array-invalid"),
                array_invalid,
            )])))
            .is_err(),
            "should reject one object plus eight nested array levels"
        );
    }

    #[test]
    fn notification_policy_rejects_duplicates_and_bounds() {
        let mut valid = provider("standard");
        valid.notifications = NotificationConfig {
            suppress_all: true,
            suppress_seats: vec!["seat-b".to_string(), "seat-a".to_string()],
        };
        let plan = AuctionPlan::compile(config(BTreeMap::from([(id("one"), valid)])))
            .expect("should compile notifications");
        assert_eq!(
            plan.providers()[0]
                .notifications
                .suppress_seats
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["seat-a", "seat-b"]
        );
        for seats in [
            vec!["same".to_string(), "same".to_string()],
            vec![String::new()],
            vec!["bad\nseat".to_string()],
            vec!["x".repeat(129)],
            (0..129).map(|index| format!("seat-{index}")).collect(),
        ] {
            let mut invalid = provider("standard");
            invalid.notifications.suppress_seats = seats;
            assert!(AuctionPlan::compile(config(BTreeMap::from([(id("one"), invalid)]))).is_err());
        }
    }

    #[test]
    fn typed_profile_config_rejects_unknown_fields_and_validates_aps_pairing() {
        let mut pbs = provider("prebid-server");
        pbs.profile_config = serde_json::json!({"browser_only": true});
        assert!(AuctionPlan::compile(config(BTreeMap::from([(id("pbs"), pbs)]))).is_err());

        let mut non_object = provider("standard");
        non_object.profile_config = Value::Null;
        assert!(
            AuctionPlan::compile(config(BTreeMap::from([(id("standard"), non_object,)]))).is_err()
        );

        let mut aps = provider("aps");
        aps.endpoint = "https://aps.example/e/pb/bid".to_string();
        aps.profile_config = serde_json::json!({
            "account_id": "example-account",
            "inventory_domain": "publisher.example"
        });
        assert!(AuctionPlan::compile(config(BTreeMap::from([(id("aps"), aps)]))).is_err());
    }

    #[test]
    fn enabled_signing_requires_nonblank_existing_global_store_ids() {
        let mut raw = config(BTreeMap::from([(id("one"), provider("standard"))]));
        raw.request_signing = Some(RequestSigning {
            enabled: true,
            config_store_id: " ".to_string(),
            secret_store_id: "example-secret-store".to_string(),
        });
        assert!(
            AuctionPlan::compile(raw).is_err(),
            "should reject enabled signing without a config store ID"
        );

        let mut raw = config(BTreeMap::from([(id("one"), provider("standard"))]));
        raw.request_signing = Some(RequestSigning {
            enabled: true,
            config_store_id: "example-config-store".to_string(),
            secret_store_id: "\t".to_string(),
        });
        assert!(
            AuctionPlan::compile(raw).is_err(),
            "should reject enabled signing without a secret store ID"
        );
    }

    #[test]
    fn routing_signing_and_static_mediator_are_preserved_in_plan() {
        let mut provider = provider("standard");
        provider.routing = RoutingMode::AllEligible;
        let mut raw = config(BTreeMap::from([(id("one"), provider)]));
        raw.request_signing = Some(RequestSigning {
            enabled: true,
            config_store_id: "example-config-store".to_string(),
            secret_store_id: "example-secret-store".to_string(),
        });
        raw.mediator = Some(MOCK_MEDIATOR_ID.to_string());
        let plan = AuctionPlan::compile(raw).expect("should compile common policies");
        assert_eq!(plan.providers()[0].routing, RoutingMode::AllEligible);
        assert!(plan.signing_enabled());
        assert_eq!(plan.mediator(), Some(MOCK_MEDIATOR_ID));

        let mut invalid = config(BTreeMap::new());
        invalid.mediator = Some("generic-mediator".to_string());
        assert!(AuctionPlan::compile(invalid).is_err());
    }
}
