//! Target-independent auction plan compiler.
//!
//! The plan is compiled once at startup from `[demand]`, `[adserver]` and
//! `[auction.bidders]`. Every selected name resolves to an implementation an
//! integration registered, so the compiler names no vendor, and request
//! handling reads only the compiled plan.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::{Host, Url};

use super::demand::{
    AdServerImplementation, CompiledDemand, DemandImplementation, DemandTimeoutDefault,
};
use crate::error::TrustedServerError;
use crate::platform::{AuctionTargetId, PlatformBackendSpec};
use crate::provider_table::{ProviderChoice, ProviderList};
use crate::settings::RequestSigning;

const MAX_ID_BYTES: usize = 128;
const MAX_SUPPRESS_SEATS: usize = 128;
const MAX_SUPPRESS_SEAT_BYTES: usize = 128;
const RESERVED_BROWSER_ENVELOPE_BIDDER_ID: &str = "trustedServer";

/// The keys of a `[demand.<name>]` table that the common driver reads itself,
/// so an implementation receives the rest.
const ENDPOINT_KEY: &str = "endpoint";
const TIMEOUT_KEY: &str = "timeout_ms";
const ROUTING_KEY: &str = "routing";
const NOTIFICATIONS_KEY: &str = "notifications";

/// A validated provider name, as written in `[demand] provider` or
/// `[adserver] provider`.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Display)]
pub struct ProviderId(String);

impl ProviderId {
    /// Borrow the validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "kept for a legacy test path the provider types will retire"
    )]
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
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_');
        if !valid {
            return Err(configuration_error(format!(
                "provider name `{value}` must be snake_case, matching ^[a-z][a-z0-9_]{{0,62}}$"
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

/// A bidder code's route to one selected demand source.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BidderRouteConfig {
    /// The `[demand]` provider the bidder is sent to.
    pub provider: ProviderId,
}

/// Which slots a demand source receives.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    /// Only slots carrying a bidder routed to the source, or demand the server
    /// routed to it.
    #[default]
    Explicit,
    /// Every banner-compatible slot.
    AllEligible,
}

/// Notification URL suppression for one demand source.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationConfig {
    /// Suppress notification URLs for every bid.
    #[serde(default)]
    pub suppress_all: bool,
    /// Suppress notification URLs for bids from these returned seats.
    #[serde(default)]
    pub suppress_seats: Vec<String>,
}

/// Input for target-independent plan compilation.
#[derive(Debug, Clone, Default)]
pub struct AuctionPlanConfig {
    /// Auction-wide logical timeout.
    pub timeout_ms: u32,
    /// The `[demand]` table.
    pub demand: ProviderList,
    /// The `[adserver]` table.
    pub adserver: ProviderChoice,
    /// Client bidder routes from `[auction.bidders]`.
    pub bidders: BTreeMap<BidderId, BidderRouteConfig>,
    /// Trusted Server's request signing configuration.
    pub request_signing: Option<RequestSigning>,
    /// The demand implementations the integration builders registered.
    pub demand_implementations: Vec<&'static DemandImplementation>,
    /// The ad server implementations the integration builders registered.
    pub adserver_implementations: Vec<&'static AdServerImplementation>,
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

/// Immutable common notification policy.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct NotificationPolicy {
    /// Suppress notification URLs for every bid.
    pub suppress_all: bool,
    /// Exact returned seats whose notification URLs are suppressed.
    pub suppress_seats: BTreeSet<String>,
}

/// One compiled demand source.
#[derive(Clone)]
pub struct ProviderPlan {
    /// The configured name.
    pub id: ProviderId,
    /// The implementation the name resolved to.
    pub implementation: &'static DemandImplementation,
    /// Canonical endpoint.
    pub endpoint: CanonicalProviderEndpoint,
    /// The table's timeout, or the implementation's default.
    pub timeout_ms: u32,
    /// Slot routing mode.
    pub routing: RoutingMode,
    /// Common notification policy.
    pub notifications: NotificationPolicy,
    /// The compiled request and response behavior.
    pub demand: Arc<dyn CompiledDemand>,
}

impl core::fmt::Debug for ProviderPlan {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ProviderPlan")
            .field("id", &self.id)
            .field("implementation", &self.implementation.id)
            .field("endpoint", &self.endpoint)
            .field("timeout_ms", &self.timeout_ms)
            .field("routing", &self.routing)
            .field("notifications", &self.notifications)
            .finish_non_exhaustive()
    }
}

/// The compiled ad server selection.
#[derive(Clone)]
pub struct AdServerPlan {
    /// The configured name.
    pub id: ProviderId,
    /// The implementation the name resolved to.
    pub implementation: &'static AdServerImplementation,
    /// The table's own settings, without `implementation`.
    pub settings: Map<String, Value>,
}

impl core::fmt::Debug for AdServerPlan {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("AdServerPlan")
            .field("id", &self.id)
            .field("implementation", &self.implementation.id)
            .finish_non_exhaustive()
    }
}

/// Immutable target-independent auction plan.
#[derive(Debug, Clone)]
pub struct AuctionPlan {
    enabled: bool,
    timeout_ms: u32,
    providers: Vec<ProviderPlan>,
    bidder_routes: BTreeMap<BidderId, usize>,
    signing_enabled: bool,
    adserver: Option<AdServerPlan>,
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
    /// Returns a configuration error for a table no selector names, a name
    /// that is not `snake_case`, an implementation no builder registered, an
    /// invalid endpoint, timeout, route or notification setting, settings an
    /// implementation rejects, or invalid signing structure.
    pub fn compile(config: AuctionPlanConfig) -> Result<Self, Report<TrustedServerError>> {
        if config.timeout_ms == 0 {
            return Err(configuration_error(
                "auction timeout_ms must be greater than zero",
            ));
        }
        config.demand.validate("demand").map_err(configuration_error)?;
        config.adserver.validate("adserver").map_err(configuration_error)?;
        let signing_enabled = compile_signing_enabled(config.request_signing.as_ref())?;

        let mut providers = Vec::new();
        let mut provider_indices = BTreeMap::new();
        for name in config.demand.selected() {
            let id = ProviderId::from_str(name)?;
            let implementation_id = config.demand.implementation_of(name);
            let implementation = config
                .demand_implementations
                .iter()
                .copied()
                .find(|implementation| implementation.id == implementation_id)
                .ok_or_else(|| {
                    unknown_implementation("demand", name, implementation_id, {
                        config
                            .demand_implementations
                            .iter()
                            .map(|implementation| implementation.id)
                    })
                })?;
            let mut settings = config.demand.settings_of(name);
            let endpoint = take_setting::<String>(&mut settings, "demand", name, ENDPOINT_KEY)?
                .ok_or_else(|| {
                    configuration_error(format!("[demand.{name}] needs an endpoint"))
                })?;
            let timeout_ms = take_setting::<u32>(&mut settings, "demand", name, TIMEOUT_KEY)?;
            let routing = take_setting::<RoutingMode>(&mut settings, "demand", name, ROUTING_KEY)?
                .unwrap_or_default();
            let notifications = take_setting::<NotificationConfig>(
                &mut settings,
                "demand",
                name,
                NOTIFICATIONS_KEY,
            )?
            .unwrap_or_default();

            if routing == RoutingMode::AllEligible && !implementation.allows_all_eligible {
                return Err(configuration_error(format!(
                    "[demand.{name}] cannot use routing `all_eligible` with implementation `{}`; configure explicit bidder routes",
                    implementation.id
                )));
            }
            let mut endpoint_url = check_endpoint("demand", name, &endpoint)?;
            (implementation.canonicalize_endpoint)(&mut endpoint_url).map_err(|reason| {
                configuration_error(format!("[demand.{name}] endpoint {reason}"))
            })?;
            let timeout_ms = timeout_ms.unwrap_or(match implementation.default_timeout {
                DemandTimeoutDefault::Auction => config.timeout_ms,
                DemandTimeoutDefault::Fixed(value) => value,
            });
            if timeout_ms == 0 {
                return Err(configuration_error(format!(
                    "[demand.{name}] timeout_ms must be greater than zero"
                )));
            }
            let notifications = compile_notifications(&id, notifications)?;
            let demand = (implementation.compile)(&settings).change_context(
                TrustedServerError::Configuration {
                    message: format!(
                        "[demand.{name}] settings are not valid for implementation `{}`",
                        implementation.id
                    ),
                },
            )?;
            provider_indices.insert(id.clone(), providers.len());
            providers.push(ProviderPlan {
                id,
                implementation,
                endpoint: CanonicalProviderEndpoint(endpoint_url),
                timeout_ms,
                routing,
                notifications,
                demand,
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
                            "[auction.bidders.{bidder}] sends the bidder to `{}`, which [demand] provider does not select",
                            route.provider
                        ))
                    })?;
            bidder_routes.insert(bidder, provider_index);
        }

        let adserver = match config.adserver.selected().first().copied() {
            None => None,
            Some(name) => {
                let id = ProviderId::from_str(name)?;
                let implementation_id = config.adserver.implementation_of(name);
                let implementation = config
                    .adserver_implementations
                    .iter()
                    .copied()
                    .find(|implementation| implementation.id == implementation_id)
                    .ok_or_else(|| {
                        unknown_implementation("adserver", name, implementation_id, {
                            config
                                .adserver_implementations
                                .iter()
                                .map(|implementation| implementation.id)
                        })
                    })?;
                let settings = config.adserver.settings_of(name);
                if let Some(endpoint) = settings.get(ENDPOINT_KEY) {
                    let endpoint = endpoint.as_str().ok_or_else(|| {
                        configuration_error(format!("[adserver.{name}] endpoint must be a URL"))
                    })?;
                    check_endpoint("adserver", name, endpoint)?;
                }
                (implementation.build)(name, &settings).change_context(
                    TrustedServerError::Configuration {
                        message: format!(
                            "[adserver.{name}] settings are not valid for implementation `{}`",
                            implementation.id
                        ),
                    },
                )?;
                Some(AdServerPlan {
                    id,
                    implementation,
                    settings,
                })
            }
        };

        Ok(Self {
            enabled: true,
            timeout_ms: config.timeout_ms,
            providers,
            bidder_routes,
            signing_enabled,
            adserver,
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

        let naming_policy = target.naming_policy();
        let backend_budget = naming_policy.auction_dynamic_backend_budget();
        let mut required_backend_names = 0_usize;
        let mut predicted_names = BTreeMap::<String, &ProviderId>::new();
        for provider in &self.providers {
            let reachable_timeout_ms = provider.timeout_ms.min(self.timeout_ms);
            required_backend_names = required_backend_names
                .saturating_add(naming_policy.transport_timeout_bucket_count(reachable_timeout_ms));
            if let Some(budget) = backend_budget
                && required_backend_names > budget
            {
                return Err(configuration_error(format!(
                    "auction target `{}` requires up to {required_backend_names} dynamic provider backends, exceeding its auction budget of {budget}",
                    target_id.adapter_id(),
                )));
            }
            let spec = provider.backend_spec();
            let prediction =
                naming_policy
                    .predict(&spec)
                    .change_context(TrustedServerError::Configuration {
                        message: format!(
                            "provider `{}` backend prediction failed for target `{}`",
                            provider.id,
                            target_id.adapter_id()
                        ),
                    })?;
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

    /// Borrow compiled demand sources in the order `[demand] provider` lists
    /// them.
    #[must_use]
    pub fn providers(&self) -> &[ProviderPlan] {
        &self.providers
    }

    /// Borrow a compiled demand source by its name.
    #[must_use]
    pub(crate) fn provider(&self, id: &ProviderId) -> Option<&ProviderPlan> {
        self.providers.iter().find(|provider| provider.id == *id)
    }

    /// Return whether any compiled demand source uses the named implementation.
    ///
    /// This narrow query lets an implementation's integration activate its own
    /// capabilities from the validated plan.
    #[must_use]
    pub fn has_implementation(&self, implementation_id: &str) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.implementation.id == implementation_id)
    }

    /// Borrow validated client-visible bidder route codes in deterministic order.
    ///
    /// This intentionally exposes route keys rather than provider identities or
    /// implementation settings for the browser Prebid injection boundary.
    pub(crate) fn browser_bidder_codes(&self) -> impl Iterator<Item = &str> {
        self.bidder_routes.keys().map(BidderId::as_str)
    }

    /// Resolve a bidder route to a compiled demand source.
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

    /// Borrow the selected ad server, when `[adserver] provider` names one.
    #[must_use]
    pub fn adserver(&self) -> Option<&AdServerPlan> {
        self.adserver.as_ref()
    }
}

fn configuration_error(message: impl Into<String>) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: message.into(),
    })
}

fn unknown_implementation<'a>(
    type_name: &str,
    name: &str,
    implementation_id: &str,
    known: impl Iterator<Item = &'a str>,
) -> Report<TrustedServerError> {
    let mut known = known.collect::<Vec<_>>();
    known.sort_unstable();
    configuration_error(format!(
        "[{type_name}] `{name}` uses implementation `{implementation_id}`, which this build does not have. The implementations it has are: {}",
        known.join(", ")
    ))
}

/// Takes one common setting out of a table's settings, parsing it as `T`.
fn take_setting<T>(
    settings: &mut Map<String, Value>,
    type_name: &str,
    name: &str,
    key: &str,
) -> Result<Option<T>, Report<TrustedServerError>>
where
    T: for<'de> Deserialize<'de>,
{
    settings
        .remove(key)
        .map(|value| {
            T::deserialize(value).map_err(|error| {
                configuration_error(format!("[{type_name}.{name}] {key} is not valid: {error}"))
            })
        })
        .transpose()
}

/// Checks an endpoint the common way. It must be HTTPS, or plain HTTP to a
/// loopback address, with a host and no credentials or fragment.
fn check_endpoint(
    type_name: &str,
    name: &str,
    value: &str,
) -> Result<Url, Report<TrustedServerError>> {
    let endpoint = Url::parse(value).map_err(|error| {
        configuration_error(format!(
            "[{type_name}.{name}] endpoint must be an absolute URL: {error}"
        ))
    })?;
    let loopback = match endpoint.host() {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    let scheme_allowed =
        endpoint.scheme() == "https" || (endpoint.scheme() == "http" && loopback);
    if !scheme_allowed
        || endpoint.host().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(configuration_error(format!(
            "[{type_name}.{name}] endpoint must be HTTPS, or HTTP to 127.0.0.1, ::1 or localhost, with a host and no credentials or fragment"
        )));
    }
    Ok(endpoint)
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
    use crate::integrations::aps::ApsDemand;
    use crate::provider_table::IMPLEMENTATION_KEY;
    use crate::integrations::openrtb::OpenRtbDemand;
    use crate::integrations::prebid_server::PrebidServerDemand;
    use serde_json::json;

    /// The demand implementations the built-in builders register.
    fn demand_implementations() -> Vec<&'static DemandImplementation> {
        crate::integrations::all_builders(&[])
            .filter_map(|builder| builder.demand())
            .collect()
    }

    /// The ad server implementations the built-in builders register.
    fn adserver_implementations() -> Vec<&'static AdServerImplementation> {
        crate::integrations::all_builders(&[])
            .filter_map(|builder| builder.adserver())
            .collect()
    }

    /// One `[demand.<name>]` table naming an implementation.
    fn table(implementation: &str) -> Map<String, Value> {
        let mut table = Map::from_iter([
            (IMPLEMENTATION_KEY.to_string(), json!(implementation)),
            (
                ENDPOINT_KEY.to_string(),
                json!("https://bid.example/openrtb2/auction"),
            ),
        ]);
        if implementation == "aps" {
            table.insert(
                ENDPOINT_KEY.to_string(),
                json!("https://aps.example/e/pb/bid"),
            );
            table.insert("account_id".to_string(), json!("example-account"));
        }
        table
    }

    /// A `[demand]` table selecting every name given, in order.
    fn demand(tables: Vec<(&str, Map<String, Value>)>) -> ProviderList {
        let selected = tables
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect::<Vec<_>>();
        let tables = tables
            .into_iter()
            .map(|(name, table)| (name.to_string(), table))
            .collect::<BTreeMap<_, _>>();
        ProviderList::new(selected, tables)
    }

    fn config(tables: Vec<(&str, Map<String, Value>)>) -> AuctionPlanConfig {
        AuctionPlanConfig {
            timeout_ms: 1500,
            demand: demand(tables),
            demand_implementations: demand_implementations(),
            adserver_implementations: adserver_implementations(),
            ..AuctionPlanConfig::default()
        }
    }

    /// A plan with one ordinary `OpenRTB` source called `one`.
    fn one_source() -> AuctionPlanConfig {
        config(vec![("one", table("openrtb"))])
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
            value = Value::Object(Map::from_iter([(format!("level_{level}"), value)]));
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
        let plan = AuctionPlan::compile(config(vec![
            ("provider_one", table("openrtb")),
            ("provider_two", table("openrtb")),
        ]))
        .expect("should compile plan");

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
            crate::platform::AuctionTargetId::Spin,
            crate::platform::AuctionTargetId::Cloudflare,
        ] {
            assert!(
                plan.validate_for_target(target).is_err(),
                "{target:?} runs one demand source at a time and should refuse fanout"
            );
        }
    }

    #[test]
    fn a_table_no_selector_names_is_refused() {
        let mut raw = one_source();
        raw.demand = ProviderList::new(
            vec!["one".to_string()],
            BTreeMap::from([
                ("one".to_string(), table("openrtb")),
                ("left_behind".to_string(), table("openrtb")),
            ]),
        );
        let error =
            AuctionPlan::compile(raw).expect_err("should refuse a table nothing selects");
        let message = error.to_string();
        assert!(
            message.contains("left_behind") && message.contains("provider"),
            "should name the table and the selector: {error:?}"
        );
    }

    #[test]
    fn an_unknown_implementation_names_the_ones_this_build_has() {
        let mut absent = table("openrtb");
        absent.insert(IMPLEMENTATION_KEY.to_string(), json!("fictional_exchange"));
        let error = AuctionPlan::compile(config(vec![("one", absent)]))
            .expect_err("should refuse an implementation this build does not have");
        let message = error.to_string();
        for expected in ["fictional_exchange", "openrtb", "prebid_server", "aps"] {
            assert!(
                message.contains(expected),
                "should name the unknown implementation and the known ones: {error:?}"
            );
        }
    }

    #[test]
    fn a_name_that_is_its_own_implementation_needs_no_implementation_line() {
        let mut named = Map::from_iter([(
            ENDPOINT_KEY.to_string(),
            json!("https://bid.example/openrtb2/auction"),
        )]);
        named.insert("request_ext".to_string(), json!({"fictional": "example"}));
        let plan = AuctionPlan::compile(config(vec![("openrtb", named)]))
            .expect("should take the name as the implementation");
        assert_eq!(plan.providers()[0].implementation.id, "openrtb");
    }

    #[test]
    fn a_demand_table_needs_an_endpoint() {
        let mut without = table("openrtb");
        without.remove(ENDPOINT_KEY);
        let error = AuctionPlan::compile(config(vec![("one", without)]))
            .expect_err("should refuse a source with no endpoint");
        assert!(
            error.to_string().contains("endpoint"),
            "should say an endpoint is needed: {error:?}"
        );
    }

    #[test]
    fn provider_id_enforces_exact_grammar_and_bounds() {
        for value in ["a", "provider_one", "p0", &format!("a{}", "b".repeat(62))] {
            assert!(
                ProviderId::from_str(value).is_ok(),
                "should accept `{value}`"
            );
        }
        for value in [
            "",
            "Provider",
            "provider-one",
            "1provider",
            "_provider",
            "provider.one",
            &format!("a{}", "b".repeat(63)),
        ] {
            assert!(
                ProviderId::from_str(value).is_err(),
                "should reject `{value}`"
            );
        }
    }

    #[test]
    fn bidder_id_enforces_admission_bounds() {
        assert!(BidderId::from_str("example").is_ok());
        assert!(BidderId::from_str("Example").is_ok());
        assert!(BidderId::from_str("").is_err());
        assert!(BidderId::from_str(&"a".repeat(MAX_ID_BYTES + 1)).is_err());
    }

    #[test]
    fn all_eligible_is_refused_for_an_implementation_that_forbids_it() {
        let mut prebid = table("prebid_server");
        prebid.insert(ROUTING_KEY.to_string(), json!("all_eligible"));
        let error = AuctionPlan::compile(config(vec![("pbs_main", prebid)]))
            .expect_err("should refuse all_eligible Prebid Server routing");
        let message = error.to_string();
        for expected in ["pbs_main", "all_eligible", "prebid_server"] {
            assert!(
                message.contains(expected),
                "should name the source, the routing and the implementation: {error:?}"
            );
        }

        let mut openrtb = table("openrtb");
        openrtb.insert(ROUTING_KEY.to_string(), json!("all_eligible"));
        AuctionPlan::compile(config(vec![("openrtb_main", openrtb)]))
            .expect("should keep all_eligible for an implementation that allows it");
    }

    #[test]
    fn compiler_rejects_exact_reserved_browser_envelope_bidder_id() {
        let mut raw = one_source();
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

        let mut case_distinct = one_source();
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
    fn sources_keep_the_order_they_were_selected_in_and_routes_are_deterministic() {
        let mut raw = config(vec![
            ("z_provider", table("openrtb")),
            ("a_provider", table("openrtb")),
        ]);
        raw.bidders.insert(
            bidder("z-bidder"),
            BidderRouteConfig {
                provider: id("z_provider"),
            },
        );
        raw.bidders.insert(
            bidder("a-bidder"),
            BidderRouteConfig {
                provider: id("a_provider"),
            },
        );
        let plan = AuctionPlan::compile(raw).expect("should compile deterministic plan");
        assert_eq!(
            plan.providers()[0].id.as_str(),
            "z_provider",
            "the selector's order is the deployment's order"
        );
        assert_eq!(plan.providers()[1].id.as_str(), "a_provider");
        assert_eq!(
            plan.browser_bidder_codes().collect::<Vec<_>>(),
            vec!["a-bidder", "z-bidder"],
            "browser query should return deduplicated route codes in deterministic order"
        );
        assert_eq!(
            plan.provider_for_bidder(&bidder("a-bidder"))
                .map(|provider| provider.id.as_str()),
            Some("a_provider")
        );
    }

    #[test]
    fn two_sources_can_run_one_implementation_under_their_own_names() {
        let plan = AuctionPlan::compile(config(vec![
            ("pbs_a", table("prebid_server")),
            ("pbs_b", table("prebid_server")),
        ]))
        .expect("should compile two Prebid Server sources");
        assert_eq!(plan.providers().len(), 2);
        assert!(
            plan.providers()
                .iter()
                .all(|provider| provider.implementation.id == "prebid_server"),
            "both names should resolve to the same implementation"
        );
        assert!(
            plan.providers()
                .iter()
                .all(|provider| provider.demand.as_any().is::<PrebidServerDemand>()),
            "each source should compile its own settings"
        );
    }

    #[test]
    fn implementation_defaults_and_an_explicit_timeout_are_resolved() {
        let mut override_table = table("prebid_server");
        override_table.insert(TIMEOUT_KEY.to_string(), json!(321));
        let plan = AuctionPlan::compile(config(vec![
            ("openrtb_one", table("openrtb")),
            ("pbs_one", table("prebid_server")),
            ("aps_one", table("aps")),
            ("pbs_override", override_table),
        ]))
        .expect("should resolve timeouts");
        let timeouts = plan
            .providers()
            .iter()
            .map(|provider| (provider.id.as_str(), provider.timeout_ms))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(timeouts["openrtb_one"], 1500);
        assert_eq!(timeouts["pbs_one"], 1000);
        assert_eq!(timeouts["aps_one"], 800);
        assert_eq!(timeouts["pbs_override"], 321);
    }

    #[test]
    fn the_plan_reports_which_implementations_it_selected() {
        let plan = AuctionPlan::compile(config(vec![("pbs", table("prebid_server"))]))
            .expect("should compile without Settings or browser integration state");
        assert!(!plan.has_implementation("aps"));
        assert!(plan.has_implementation("prebid_server"));

        let plan = AuctionPlan::compile(config(vec![("aps_instance", table("aps"))]))
            .expect("should compile APS plan");
        assert!(
            plan.has_implementation("aps"),
            "a validated plan should expose its APS renderer capability"
        );
        assert!(!plan.has_implementation("prebid_server"));
        assert!(
            plan.providers()[0].demand.as_any().is::<ApsDemand>(),
            "the APS source should compile its own settings"
        );
    }

    #[test]
    fn a_bidder_route_must_name_a_selected_source() {
        let mut raw = one_source();
        raw.bidders.insert(
            bidder("example"),
            BidderRouteConfig {
                provider: id("missing"),
            },
        );
        let error = AuctionPlan::compile(raw).expect_err("should refuse an unrouted bidder");
        assert!(
            error.to_string().contains("missing"),
            "should name the source the route wanted: {error:?}"
        );
    }

    #[test]
    fn compiler_canonicalizes_https_endpoints_and_rejects_unsafe_forms() {
        let mut canonical = table("openrtb");
        canonical.insert(
            ENDPOINT_KEY.to_string(),
            json!("https://BID.EXAMPLE:443/path"),
        );
        let plan = AuctionPlan::compile(config(vec![("one", canonical)]))
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
            let mut raw = table("openrtb");
            raw.insert(ENDPOINT_KEY.to_string(), json!(endpoint));
            assert!(
                AuctionPlan::compile(config(vec![("one", raw)])).is_err(),
                "should reject {endpoint}"
            );
        }
        let mut aps = table("aps");
        aps.insert(
            ENDPOINT_KEY.to_string(),
            json!("https://aps.example/e/dtb/bid"),
        );
        assert!(
            AuctionPlan::compile(config(vec![("aps", aps)])).is_err(),
            "should refuse the legacy APS path"
        );
    }

    #[test]
    fn plain_http_is_allowed_only_to_a_loopback_host() {
        for endpoint in [
            "http://127.0.0.1:8000/openrtb2/auction",
            "http://[::1]:8000/openrtb2/auction",
            "http://localhost:8000/openrtb2/auction",
            "http://LOCALHOST:8000/openrtb2/auction",
        ] {
            let mut raw = table("openrtb");
            raw.insert(ENDPOINT_KEY.to_string(), json!(endpoint));
            AuctionPlan::compile(config(vec![("local", raw)]))
                .unwrap_or_else(|error| panic!("should accept {endpoint}: {error:?}"));
        }
        for endpoint in [
            "http://192.168.0.10:8000/openrtb2/auction",
            "http://bid.example/openrtb2/auction",
            "http://localhost.example/openrtb2/auction",
        ] {
            let mut raw = table("openrtb");
            raw.insert(ENDPOINT_KEY.to_string(), json!(endpoint));
            let error = AuctionPlan::compile(config(vec![("remote", raw)]))
                .expect_err("should reject plain HTTP off the loopback");
            assert!(
                error.to_string().contains("127.0.0.1"),
                "should say which hosts plain HTTP may reach: {error:?}"
            );
        }
    }

    #[test]
    fn only_prebid_server_completes_an_endpoint_that_names_a_host_alone() {
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
            let mut pbs = table("prebid_server");
            pbs.insert(ENDPOINT_KEY.to_string(), json!(configured));
            let plan = AuctionPlan::compile(config(vec![("pbs", pbs)]))
                .expect("should compile Prebid Server endpoint");
            assert_eq!(
                plan.providers()[0].endpoint.as_str(),
                expected,
                "{configured}"
            );
        }

        let mut openrtb = table("openrtb");
        openrtb.insert(ENDPOINT_KEY.to_string(), json!("https://bid.example/"));
        let plan = AuctionPlan::compile(config(vec![("openrtb", openrtb)]))
            .expect("should compile a root endpoint unchanged");
        assert_eq!(
            plan.providers()[0].endpoint.as_str(),
            "https://bid.example/"
        );

        let plan = AuctionPlan::compile(config(vec![("aps", table("aps"))]))
            .expect("should compile APS endpoint");
        assert_eq!(
            plan.providers()[0].endpoint.as_str(),
            "https://aps.example/e/pb/bid"
        );
    }

    #[test]
    fn openrtb_extensions_are_bounded_and_cannot_claim_reserved_fields() {
        let mut valid = table("openrtb");
        valid.insert("request_ext".to_string(), json!({"fictional_account": "example"}));
        valid.insert("imp_ext".to_string(), json!({"placement_group": "display"}));
        let plan = AuctionPlan::compile(config(vec![("one", valid)]))
            .expect("should compile static extensions");
        let openrtb = plan.providers()[0]
            .demand
            .as_any()
            .downcast_ref::<OpenRtbDemand>()
            .expect("should compile the OpenRTB implementation");
        assert_eq!(
            openrtb.request_ext.as_object()["fictional_account"],
            "example"
        );

        let mut reserved = table("openrtb");
        reserved.insert(
            "request_ext".to_string(),
            json!({"trusted_server": {"signature": "forged"}}),
        );
        assert!(
            AuctionPlan::compile(config(vec![("one", reserved)])).is_err(),
            "should refuse an extension claiming a reserved field"
        );

        let mut too_large = table("openrtb");
        too_large.insert(
            "request_ext".to_string(),
            json!({"padding": "x".repeat(17 * 1024)}),
        );
        assert!(
            AuctionPlan::compile(config(vec![("one", too_large)])).is_err(),
            "should refuse an extension over the size bound"
        );

        let mut too_deep = table("openrtb");
        too_deep.insert("request_ext".to_string(), nested_object(9));
        assert!(
            AuctionPlan::compile(config(vec![("one", too_deep)])).is_err(),
            "should refuse an extension over the depth bound"
        );

        let mut deep_array = table("openrtb");
        deep_array.insert(
            "request_ext".to_string(),
            json!({"levels": nested_array(8)}),
        );
        assert!(
            AuctionPlan::compile(config(vec![("one", deep_array)])).is_err(),
            "should count array levels toward the depth bound"
        );

        let mut not_an_object = table("openrtb");
        not_an_object.insert("request_ext".to_string(), json!("string"));
        assert!(
            AuctionPlan::compile(config(vec![("one", not_an_object)])).is_err(),
            "should refuse an extension that is not an object"
        );
    }

    #[test]
    fn notification_policy_rejects_duplicates_and_bounds() {
        let mut valid = table("openrtb");
        valid.insert(
            NOTIFICATIONS_KEY.to_string(),
            json!({"suppress_all": true, "suppress_seats": ["seat-b", "seat-a"]}),
        );
        let plan = AuctionPlan::compile(config(vec![("one", valid)]))
            .expect("should compile notifications");
        assert!(plan.providers()[0].notifications.suppress_all);
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
            json!(["same", "same"]),
            json!([""]),
            json!(["bad\nseat"]),
            json!(["x".repeat(129)]),
            Value::Array(
                (0..129)
                    .map(|index| json!(format!("seat-{index}")))
                    .collect(),
            ),
        ] {
            let mut invalid = table("openrtb");
            invalid.insert(
                NOTIFICATIONS_KEY.to_string(),
                json!({"suppress_seats": seats}),
            );
            assert!(
                AuctionPlan::compile(config(vec![("one", invalid)])).is_err(),
                "should refuse {seats}"
            );
        }
    }

    #[test]
    fn an_implementation_rejects_settings_it_does_not_know() {
        let mut pbs = table("prebid_server");
        pbs.insert("browser_only".to_string(), json!(true));
        let error = AuctionPlan::compile(config(vec![("pbs", pbs)]))
            .expect_err("should refuse a setting the implementation does not know");
        assert!(
            format!("{error:?}").contains("browser_only"),
            "should name the setting: {error:?}"
        );

        let mut aps = table("aps");
        aps.insert("inventory_domain".to_string(), json!("publisher.example"));
        assert!(
            AuctionPlan::compile(config(vec![("aps", aps)])).is_err(),
            "should refuse an APS inventory domain without its page origin"
        );

        let mut without_account = table("aps");
        without_account.remove("account_id");
        assert!(
            AuctionPlan::compile(config(vec![("aps", without_account)])).is_err(),
            "should refuse an APS source with no account"
        );
    }

    #[test]
    fn enabled_signing_requires_nonblank_existing_global_store_ids() {
        let mut raw = one_source();
        raw.request_signing = Some(RequestSigning {
            enabled: true,
            config_store_id: " ".to_string(),
            secret_store_id: "example-secret-store".to_string(),
        });
        assert!(
            AuctionPlan::compile(raw).is_err(),
            "should reject enabled signing without a config store ID"
        );

        let mut raw = one_source();
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
    fn routing_signing_and_the_selected_ad_server_are_preserved_in_the_plan() {
        let mut openrtb = table("openrtb");
        openrtb.insert(ROUTING_KEY.to_string(), json!("all_eligible"));
        let mut raw = config(vec![("one", openrtb)]);
        raw.request_signing = Some(RequestSigning {
            enabled: true,
            config_store_id: "example-config-store".to_string(),
            secret_store_id: "example-secret-store".to_string(),
        });
        raw.adserver = ProviderChoice::new(
            Some("adserver_mock".to_string()),
            BTreeMap::from([(
                "adserver_mock".to_string(),
                Map::from_iter([(
                    ENDPOINT_KEY.to_string(),
                    json!("http://127.0.0.1:6767/adserver/mediate"),
                )]),
            )]),
        );
        let plan = AuctionPlan::compile(raw).expect("should compile common policies");
        assert_eq!(plan.providers()[0].routing, RoutingMode::AllEligible);
        assert!(plan.signing_enabled());
        assert_eq!(
            plan.adserver().map(|adserver| adserver.id.as_str()),
            Some("adserver_mock")
        );

        let mut invalid = config(Vec::new());
        invalid.adserver = ProviderChoice::new(
            Some("fictional_adserver".to_string()),
            BTreeMap::new(),
        );
        let error = AuctionPlan::compile(invalid)
            .expect_err("should refuse an ad server this build does not have");
        assert!(
            error.to_string().contains("fictional_adserver"),
            "should name the ad server: {error:?}"
        );
    }

    #[test]
    fn an_ad_server_is_built_from_its_own_table() {
        let mut raw = one_source();
        raw.adserver = ProviderChoice::new(
            Some("house".to_string()),
            BTreeMap::from([(
                "house".to_string(),
                Map::from_iter([
                    (IMPLEMENTATION_KEY.to_string(), json!("adserver_mock")),
                    (
                        ENDPOINT_KEY.to_string(),
                        json!("https://adserver.example/mediate"),
                    ),
                    ("timeout_ms".to_string(), json!(750)),
                ]),
            )]),
        );
        let plan = AuctionPlan::compile(raw).expect("should compile a named ad server");
        let adserver = plan.adserver().expect("should select an ad server");
        assert_eq!(adserver.id.as_str(), "house");
        assert_eq!(adserver.implementation.id, "adserver_mock");

        let mut unknown_setting = one_source();
        unknown_setting.adserver = ProviderChoice::new(
            Some("adserver_mock".to_string()),
            BTreeMap::from([(
                "adserver_mock".to_string(),
                Map::from_iter([
                    (
                        ENDPOINT_KEY.to_string(),
                        json!("https://adserver.example/mediate"),
                    ),
                    ("enabled".to_string(), json!(true)),
                ]),
            )]),
        );
        let error = AuctionPlan::compile(unknown_setting)
            .expect_err("should refuse a setting the ad server does not know");
        assert!(
            format!("{error:?}").contains("enabled"),
            "should name the setting: {error:?}"
        );
    }
}
