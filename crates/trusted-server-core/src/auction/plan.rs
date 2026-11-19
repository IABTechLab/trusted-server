//! Target-independent config-first auction plan compiler.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use error_stack::Report;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::{Host, Url};

use super::profile::{CompiledOpenRtbProfile, ProfileTimeoutDefault, find_profile};
use crate::error::TrustedServerError;
use crate::settings::RequestSigning;

const MAX_ID_BYTES: usize = 128;
const MAX_SUPPRESS_SEATS: usize = 128;
const MAX_SUPPRESS_SEAT_BYTES: usize = 128;
const MOCK_MEDIATOR_ID: &str = "adserver_mock";
const RESERVED_BROWSER_ENVELOPE_BIDDER_ID: &str = "trustedServer";
/// Dynamic backends a Fastly service may register.
const FASTLY_DYNAMIC_BACKEND_LIMIT: usize = 200;
/// Fastly dynamic backends kept for the sites outside the auction.
const FASTLY_NON_AUCTION_BACKEND_RESERVE: usize = 40;

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
                "bidder ID {value:?} must be nonempty, at most {MAX_ID_BYTES} UTF-8 bytes, contain no control characters, and have no surrounding whitespace"
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
    /// Permit unencrypted HTTP for this provider only. Defaults to false.
    /// Use only for explicitly approved demo/test endpoints.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_insecure_http: bool,
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
            if registration.id == "prebid-server" && raw.routing == RoutingMode::AllEligible {
                return Err(configuration_error(format!(
                    "provider `{id}` cannot use routing `all_eligible` with profile `prebid-server`; configure explicit bidder routes"
                )));
            }
            if !raw.profile_config.is_object() {
                return Err(configuration_error(format!(
                    "provider `{id}` profile_config must be an object"
                )));
            }
            let endpoint = canonicalize_endpoint(
                &id,
                registration.id,
                &raw.endpoint,
                raw.allow_insecure_http,
            )?;
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

impl AuctionPlan {
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

/// Rejects an enabled plan whose providers need more Fastly backends than the auction may use.
///
/// Each distinct tuple of provider scheme, host, port, and configured timeout is at most one
/// `EdgeZero` backend configuration on Fastly, because bidders set no host override and every
/// request carries the same connect timeout. The count can only overstate: `EdgeZero` keys
/// identity on the rounded timer, so two timeouts that round alike share a configuration but
/// count twice here. The auction may use 160 of Fastly's 200 dynamic backends, which keeps 40 for
/// the other outbound sites. A disabled auction registers no provider backend and passes.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when an enabled plan has more than 160 distinct
/// provider tuples.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::plan::{
///     AuctionPlan, AuctionPlanConfig, validate_fastly_backend_bound,
/// };
///
/// let plan = AuctionPlan::compile(AuctionPlanConfig {
///     timeout_ms: 1000,
///     ..AuctionPlanConfig::default()
/// })
/// .expect("should compile an empty plan");
/// validate_fastly_backend_bound(&plan).expect("should fit an empty plan");
/// ```
pub fn validate_fastly_backend_bound(plan: &AuctionPlan) -> Result<(), Report<TrustedServerError>> {
    if !plan.enabled {
        return Ok(());
    }
    let bound = FASTLY_DYNAMIC_BACKEND_LIMIT - FASTLY_NON_AUCTION_BACKEND_RESERVE;
    let tuples = plan
        .providers
        .iter()
        .map(|provider| {
            let endpoint = provider.endpoint.url();
            (
                endpoint.scheme(),
                endpoint.host_str(),
                endpoint.port_or_known_default(),
                provider.timeout_ms,
            )
        })
        .collect::<BTreeSet<_>>();
    if tuples.len() > bound {
        return Err(configuration_error(format!(
            "auction providers need {} Fastly backend configurations, exceeding the bound of {bound} ({FASTLY_DYNAMIC_BACKEND_LIMIT} dynamic backends less {FASTLY_NON_AUCTION_BACKEND_RESERVE} kept for other sites)",
            tuples.len()
        )));
    }
    Ok(())
}

/// Reports whether `url` is plain HTTP to the loopback IP literal `127.0.0.1` or `[::1]`.
///
/// Hosted runtimes cannot reach loopback, so these endpoints serve only local test bidders.
/// Names and other addresses, such as `localhost` or `127.0.0.2`, do not qualify. The port is
/// optional.
fn is_loopback_http_endpoint(url: &Url) -> bool {
    url.scheme() == "http"
        && match url.host() {
            Some(Host::Ipv4(address)) => address == Ipv4Addr::LOCALHOST,
            Some(Host::Ipv6(address)) => address == Ipv6Addr::LOCALHOST,
            Some(Host::Domain(_)) | None => false,
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
    allow_insecure_http: bool,
) -> Result<CanonicalProviderEndpoint, Report<TrustedServerError>> {
    let mut endpoint = Url::parse(value).map_err(|error| {
        configuration_error(format!(
            "provider `{provider_id}` endpoint must be absolute HTTPS, or plain HTTP to 127.0.0.1 or [::1]: {error}"
        ))
    })?;
    if !(endpoint.scheme() == "https"
        || is_loopback_http_endpoint(&endpoint)
        || (allow_insecure_http && endpoint.scheme() == "http"))
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(configuration_error(format!(
            "provider `{provider_id}` endpoint must be absolute HTTPS, or plain HTTP to 127.0.0.1 or [::1], with a host and no credentials or fragment"
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
    if profile_id == "prebid-server" {
        normalize_prebid_server_endpoint(&mut endpoint);
    }
    Ok(CanonicalProviderEndpoint(endpoint))
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn normalize_prebid_server_endpoint(endpoint: &mut Url) {
    match endpoint.path() {
        "" | "/" => endpoint.set_path("/openrtb2/auction"),
        "/openrtb2/auction/" => endpoint.set_path("/openrtb2/auction"),
        _ => {}
    }
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
            allow_insecure_http: false,
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
            let error = BidderId::from_str(invalid).expect_err("should reject invalid bidder ID");
            assert!(
                error.to_string().contains(&format!("{invalid:?}")),
                "should identify invalid bidder ID {invalid:?}: {error:?}"
            );
        }
    }

    #[test]
    fn compiler_rejects_all_eligible_for_prebid_server_only() {
        let mut prebid = provider("prebid-server");
        prebid.routing = RoutingMode::AllEligible;
        let error = AuctionPlan::compile(config(BTreeMap::from([(id("pbs-main"), prebid)])))
            .expect_err("should reject all_eligible Prebid Server routing");
        let message = error.to_string();
        for expected in ["pbs-main", "all_eligible", "prebid-server"] {
            assert!(
                message.contains(expected),
                "should identify provider, routing, and profile: {error:?}"
            );
        }

        let mut standard = provider("standard");
        standard.routing = RoutingMode::AllEligible;
        AuctionPlan::compile(config(BTreeMap::from([(id("standard-main"), standard)])))
            .expect("should retain all_eligible for non-Prebid profiles");
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

    /// Compiles one standard provider per `(endpoint, timeout_ms)` pair.
    fn plan_with_providers(endpoints: impl IntoIterator<Item = (String, u32)>) -> AuctionPlan {
        let providers = endpoints
            .into_iter()
            .enumerate()
            .map(|(index, (endpoint, timeout_ms))| {
                let mut raw_provider = provider("standard");
                raw_provider.endpoint = endpoint;
                raw_provider.timeout_ms = Some(timeout_ms);
                (id(&format!("provider-{index}")), raw_provider)
            })
            .collect();
        AuctionPlan::compile(config(providers)).expect("should compile the provider plan")
    }

    /// One endpoint per port, so every provider has its own backend tuple.
    fn distinct_tuples(count: u16) -> impl Iterator<Item = (String, u32)> {
        (0..count).map(|index| {
            (
                format!("https://bid.example:{}/openrtb", 9000 + index),
                1000,
            )
        })
    }

    #[test]
    fn fastly_backend_bound_accepts_160_and_rejects_161_tuples() {
        validate_fastly_backend_bound(&plan_with_providers(distinct_tuples(160)))
            .expect("should accept 160 distinct provider backend tuples");

        let error = validate_fastly_backend_bound(&plan_with_providers(distinct_tuples(161)))
            .expect_err("should reject 161 distinct provider backend tuples");
        assert!(
            error.to_string().contains("161") && error.to_string().contains("160"),
            "should name the tuple count and the bound: {error}"
        );
    }

    #[test]
    fn providers_sharing_a_tuple_count_once() {
        // The extra provider differs from the first only in its path, which is
        // not part of backend identity.
        let shared = ("https://bid.example:9000/other-path".to_string(), 1000);
        let plan = plan_with_providers(distinct_tuples(160).chain([shared]));

        assert_eq!(plan.providers().len(), 161, "should compile 161 providers");
        validate_fastly_backend_bound(&plan)
            .expect("should count two providers that share a tuple once");

        let other_timeout = ("https://bid.example:9000/openrtb".to_string(), 999);
        let error = validate_fastly_backend_bound(&plan_with_providers(
            distinct_tuples(160).chain([other_timeout]),
        ))
        .expect_err("should count a different configured timeout as its own tuple");
        assert!(
            error.to_string().contains("161"),
            "should count the timeout in the tuple: {error}"
        );
    }

    #[test]
    fn disabled_auction_skips_backend_bound() {
        let disabled = plan_with_providers(distinct_tuples(161)).with_enabled(false);

        validate_fastly_backend_bound(&disabled)
            .expect("should skip the bound while the auction is disabled");
    }

    fn compile_endpoint(endpoint: &str) -> Result<AuctionPlan, Report<TrustedServerError>> {
        let mut raw_provider = provider("standard");
        raw_provider.endpoint = endpoint.to_string();
        AuctionPlan::compile(config(BTreeMap::from([(id("loopback"), raw_provider)])))
    }

    #[test]
    fn loopback_http_endpoints_are_accepted() {
        for endpoint in [
            "http://127.0.0.1:8080/bid",
            "http://127.0.0.1/bid",
            "http://[::1]:8080/bid",
            "http://[::1]/bid",
        ] {
            let plan = compile_endpoint(endpoint)
                .unwrap_or_else(|error| panic!("should accept {endpoint}: {error:?}"));
            assert_eq!(
                plan.providers()[0].endpoint.as_str(),
                endpoint,
                "should keep the loopback endpoint as configured"
            );
        }
    }

    #[test]
    fn loopback_lookalike_endpoints_are_rejected() {
        for endpoint in [
            "http://localhost:8080/bid",
            "http://127.0.0.2:8080/bid", // allow-domain: 127.0.0.2
            "http://0.0.0.0:8080/bid",   // allow-domain: 0.0.0.0
            "http://user@127.0.0.1:8080/bid",
            "http://127.0.0.1:8080/bid#fragment",
        ] {
            assert!(
                compile_endpoint(endpoint).is_err(),
                "should reject plain HTTP to {endpoint}"
            );
        }
    }

    #[test]
    fn unparsable_endpoint_error_states_the_loopback_rule() {
        let Err(error) = compile_endpoint("not a URL") else {
            panic!("should reject an unparsable endpoint");
        };
        assert!(
            error
                .to_string()
                .contains("must be absolute HTTPS, or plain HTTP to 127.0.0.1 or [::1]"),
            "should state the loopback-aware endpoint rule: {error}"
        );
    }

    #[test]
    fn insecure_http_requires_an_explicit_per_provider_opt_in() {
        let mut demo = provider("prebid-server");
        demo.endpoint = "http://pbs.example.com:8000".to_string();
        assert!(
            AuctionPlan::compile(config(BTreeMap::from([(id("demo"), demo.clone())]))).is_err(),
            "should reject HTTP without the provider's explicit opt-in"
        );
        demo.allow_insecure_http = true;
        let plan = AuctionPlan::compile(config(BTreeMap::from([(id("demo"), demo.clone())])))
            .expect("should compile an explicitly opted-in HTTP provider");
        assert_eq!(
            plan.providers()[0].endpoint.as_str(),
            "http://pbs.example.com:8000/openrtb2/auction",
            "should normalize the opted-in Prebid Server endpoint"
        );
        let mut other = provider("standard");
        other.endpoint = "http://other.example.com/bid".to_string();
        assert!(
            AuctionPlan::compile(config(BTreeMap::from([
                (id("demo"), demo),
                (id("other"), other)
            ])))
            .is_err(),
            "should not inherit another provider's HTTP opt-in"
        );
    }

    #[test]
    fn insecure_http_opt_in_does_not_allow_credentials_fragments_or_other_schemes() {
        for endpoint in [
            "http://user:pass@pbs.example.com/bid",
            "http://pbs.example.com/bid#fragment",
            "ftp://pbs.example.com/bid",
            "/relative",
        ] {
            let mut raw = provider("prebid-server");
            raw.allow_insecure_http = true;
            raw.endpoint = endpoint.to_string();
            assert!(
                AuctionPlan::compile(config(BTreeMap::from([(id("demo"), raw)]))).is_err(),
                "should reject {endpoint}"
            );
        }
    }

    #[test]
    fn insecure_http_default_preserves_existing_provider_wire_shape() {
        let legacy = serde_json::to_value(provider("prebid-server"))
            .expect("should serialize the default provider");
        assert!(
            legacy.get("allow_insecure_http").is_none(),
            "should omit the default HTTP opt-in field"
        );
        assert!(
            !serde_json::from_value::<ProviderConfig>(legacy)
                .expect("should deserialize the default provider")
                .allow_insecure_http,
            "should keep HTTP disabled when the field is absent"
        );
        let mut opted_in = provider("prebid-server");
        opted_in.allow_insecure_http = true;
        let encoded =
            serde_json::to_value(opted_in).expect("should serialize the opted-in provider");
        assert_eq!(
            encoded["allow_insecure_http"], true,
            "should publish the explicit HTTP opt-in"
        );
        assert!(
            serde_json::from_value::<ProviderConfig>(encoded)
                .expect("should deserialize the opted-in provider")
                .allow_insecure_http,
            "should retain the explicit HTTP opt-in across serialization"
        );
    }

    #[test]
    fn compiler_normalizes_only_prebid_server_origin_and_canonical_paths() {
        for (configured, expected) in [
            (
                "https://pbs.example",
                "https://pbs.example/openrtb2/auction",
            ),
            (
                "https://pbs.example/",
                "https://pbs.example/openrtb2/auction",
            ),
            (
                "https://pbs.example/openrtb2/auction",
                "https://pbs.example/openrtb2/auction",
            ),
            (
                "https://pbs.example/openrtb2/auction/",
                "https://pbs.example/openrtb2/auction",
            ),
            (
                "https://pbs.example?region=example",
                "https://pbs.example/openrtb2/auction?region=example",
            ),
            ("https://pbs.example/bid", "https://pbs.example/bid"),
            (
                "https://pbs.example/custom/pbs",
                "https://pbs.example/custom/pbs",
            ),
        ] {
            let mut pbs = provider("prebid-server");
            pbs.endpoint = configured.to_string();
            let plan = AuctionPlan::compile(config(BTreeMap::from([(id("pbs"), pbs)])))
                .expect("should compile Prebid Server endpoint");
            assert_eq!(
                plan.providers()[0].endpoint.as_str(),
                expected,
                "{configured}"
            );
        }

        let mut standard = provider("standard");
        standard.endpoint = "https://bid.example/".to_string();
        let plan = AuctionPlan::compile(config(BTreeMap::from([(id("standard"), standard)])))
            .expect("should compile standard root endpoint");
        assert_eq!(
            plan.providers()[0].endpoint.as_str(),
            "https://bid.example/"
        );

        let mut aps = provider("aps");
        aps.endpoint = "https://aps.example/e/pb/bid".to_string();
        aps.profile_config = serde_json::json!({"account_id": "example-account"});
        let plan = AuctionPlan::compile(config(BTreeMap::from([(id("aps"), aps)])))
            .expect("should compile APS endpoint");
        assert_eq!(
            plan.providers()[0].endpoint.as_str(),
            "https://aps.example/e/pb/bid"
        );
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
