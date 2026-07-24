//! Provider permissions: a technical permission model gating provider execution.
//!
//! A provider (Edge Cookie, device, or geo) advertises the [`Permission`]s its
//! data use *requires*. Trusted Server resolves which permissions are currently
//! *set* from the session's signals and the country it resolves to, and refuses
//! to execute a provider whose required permissions are not set.
//!
//! The vocabulary is the IAB TCF Europe purpose set, used **only** as a technical
//! identifier for a permission. No CMP or TCF *policy* is implemented here, and
//! only [`Permission::StoreOnDevice`] (TCF Purpose 1) and
//! [`Permission::SelectPersonalisedAds`] (TCF Purpose 4) are resolved against a
//! session signal today. The remaining purposes are modeled for forward
//! compatibility.
//!
//! How a permission is acquired varies by country, so resolution is keyed on the
//! ISO 3166-1 country code a geo provider returns. [`PermissionMaps::standard`]
//! loads the default country and region rules from the embedded
//! `permissions.yaml` (see `DEFAULT_PERMISSION_RULES`).
//! When no country is identified (no geo provider, or a lookup that resolves
//! nothing) or the resolved country/region has no rule, resolution uses the
//! deployer's configured default country (`[geo] default_country`). With none
//! configured, a permission is set only when the incoming signals explicitly
//! grant it.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

/// A technical permission a provider may require, named by its IAB TCF Europe
/// purpose.
///
/// Only the identifier is used, with no TCF policy implemented. Only
/// [`Permission::StoreOnDevice`] (Purpose 1) and
/// [`Permission::SelectPersonalisedAds`] (Purpose 4) are resolved against a
/// signal today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display)]
pub enum Permission {
    /// TCF Purpose 1, store and/or access information on a device. Resolved
    /// against a session signal today.
    #[display("store-on-device")]
    StoreOnDevice,
    /// TCF Purpose 2, use limited data to select advertising.
    #[display("select-basic-ads")]
    SelectBasicAds,
    /// TCF Purpose 3, create profiles for personalised advertising.
    #[display("create-ads-profile")]
    CreateAdsProfile,
    /// TCF Purpose 4, use profiles to select personalised advertising.
    #[display("select-personalised-ads")]
    SelectPersonalisedAds,
    /// TCF Purpose 5, create profiles to personalise content.
    #[display("create-content-profile")]
    CreateContentProfile,
    /// TCF Purpose 6, use profiles to select personalised content.
    #[display("select-personalised-content")]
    SelectPersonalisedContent,
    /// TCF Purpose 7, measure advertising performance.
    #[display("measure-ad-performance")]
    MeasureAdPerformance,
    /// TCF Purpose 8, measure content performance.
    #[display("measure-content-performance")]
    MeasureContentPerformance,
    /// TCF Purpose 9, understand audiences through statistics.
    #[display("market-research")]
    MarketResearch,
    /// TCF Purpose 10, develop and improve services.
    #[display("develop-services")]
    DevelopServices,
    /// TCF Purpose 11, use limited data to select content.
    #[display("select-basic-content")]
    SelectBasicContent,
}

impl Permission {
    /// Every modeled permission, in TCF purpose order.
    pub const ALL: [Permission; 11] = [
        Permission::StoreOnDevice,
        Permission::SelectBasicAds,
        Permission::CreateAdsProfile,
        Permission::SelectPersonalisedAds,
        Permission::CreateContentProfile,
        Permission::SelectPersonalisedContent,
        Permission::MeasureAdPerformance,
        Permission::MeasureContentPerformance,
        Permission::MarketResearch,
        Permission::DevelopServices,
        Permission::SelectBasicContent,
    ];

    /// The IAB TCF Europe purpose number (1 to 11) for this permission.
    #[must_use]
    pub const fn tcf_purpose(self) -> u8 {
        match self {
            Permission::StoreOnDevice => 1,
            Permission::SelectBasicAds => 2,
            Permission::CreateAdsProfile => 3,
            Permission::SelectPersonalisedAds => 4,
            Permission::CreateContentProfile => 5,
            Permission::SelectPersonalisedContent => 6,
            Permission::MeasureAdPerformance => 7,
            Permission::MeasureContentPerformance => 8,
            Permission::MarketResearch => 9,
            Permission::DevelopServices => 10,
            Permission::SelectBasicContent => 11,
        }
    }

    /// The single-bit mask for this permission within a [`PermissionSet`].
    const fn bit(self) -> u16 {
        1 << (self.tcf_purpose() - 1)
    }

    /// Returns the permission whose IAB TCF identifier matches `id` (for example
    /// `"store-on-device"`), or `None` when it is unknown.
    ///
    /// Used to parse permission names from `permissions.yaml`.
    #[must_use]
    pub fn from_identifier(id: &str) -> Option<Permission> {
        Permission::ALL.into_iter().find(|p| p.to_string() == id)
    }
}

/// A set of [`Permission`]s, stored as a bitset keyed by TCF purpose number.
///
/// Used both for what a provider requires and for what Trusted Server has set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermissionSet(u16);

impl PermissionSet {
    /// The empty set, requiring or containing nothing.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Returns this set with `permission` added.
    #[must_use]
    pub const fn with(self, permission: Permission) -> Self {
        Self(self.0 | permission.bit())
    }

    /// Whether `permission` is in the set.
    #[must_use]
    pub const fn contains(self, permission: Permission) -> bool {
        self.0 & permission.bit() != 0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every permission in `other` is also in this set.
    #[must_use]
    pub const fn contains_all(self, other: PermissionSet) -> bool {
        self.0 & other.0 == other.0
    }

    /// Iterates the permissions in the set, in TCF purpose order.
    ///
    /// The built-ins read nothing from the full set; this serves a provider or
    /// diagnostic path that enumerates what is present.
    pub fn iter(self) -> impl Iterator<Item = Permission> {
        Permission::ALL
            .into_iter()
            .filter(move |p| self.contains(*p))
    }
}

impl FromIterator<Permission> for PermissionSet {
    fn from_iter<I: IntoIterator<Item = Permission>>(iter: I) -> Self {
        iter.into_iter()
            .fold(PermissionSet::none(), PermissionSet::with)
    }
}

/// How a permission is acquired in a given country.
///
/// This is intentionally country-keyed, not provider-keyed: a provider only
/// advertises *which* permissions it needs, and the country's rules decide *how*
/// each is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acquisition {
    /// Set without any signal, exempt or strictly necessary here.
    Granted,
    /// Set only when the incoming signals grant the matching TCF purpose.
    RequiresSignal,
    /// Never set in this country.
    Denied,
}

/// What a session signal says about a permission, layered on top of the
/// country/region baseline by the consent mapping.
///
/// The core never reads consent directly. A caller maps its consent model (or
/// any other signal source) to a [`ConsentSignal`] per permission, and the
/// permission model applies it: a [`Grant`](Self::Grant) sets a
/// `RequiresSignal` permission, a [`Revoke`](Self::Revoke) drops a `Granted` one
/// (an opt-out), and [`Neutral`](Self::Neutral) leaves the baseline unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentSignal {
    /// The signal grants this permission, so a `RequiresSignal` baseline is set.
    Grant,
    /// The signal withdraws this permission, dropping a `Granted` baseline.
    Revoke,
    /// The signal says nothing, so the baseline stands.
    Neutral,
}

/// The acquisition rule for each permission in one country or region.
///
/// A `default` applies to any permission not explicitly overridden, so a rule
/// table stays compact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountryRules {
    default: Acquisition,
    overrides: BTreeMap<u8, Acquisition>,
}

impl CountryRules {
    /// Rules with `default` for every permission and no per-permission override.
    /// Groups in `permissions.yaml` are built from this plus [`with_rule`].
    ///
    /// [`with_rule`]: Self::with_rule
    #[must_use]
    pub fn with_default(default: Acquisition) -> Self {
        Self {
            default,
            overrides: BTreeMap::new(),
        }
    }

    /// Sets the acquisition rule for a single permission, overriding the default.
    #[must_use]
    pub fn with_rule(mut self, permission: Permission, acquisition: Acquisition) -> Self {
        self.overrides.insert(permission.tcf_purpose(), acquisition);
        self
    }

    /// The acquisition rule for `permission`.
    #[must_use]
    pub fn rule_for(&self, permission: Permission) -> Acquisition {
        self.overrides
            .get(&permission.tcf_purpose())
            .copied()
            .unwrap_or(self.default)
    }
}

/// Looks up the [`CountryRules`] for a request's country and region.
///
/// `by_country` is keyed on the ISO 3166-1 alpha-2 code a geo provider returns
/// (upper-cased). [`PermissionMaps::standard`] populates it with a default set
/// of country rules. `by_region` keeps optional, finer rules keyed by country
/// and region (for example a US state), which take precedence over the country
/// entry. A request whose country and region match no entry resolves to `None`
/// from [`rules_for`](Self::rules_for); the caller substitutes the deployer's
/// configured default country (see [`resolve_with`](Self::resolve_with)).
#[derive(Debug, Clone, Default)]
pub struct PermissionMaps {
    by_country: BTreeMap<String, CountryRules>,
    by_region: BTreeMap<String, CountryRules>,
}

/// The default permission rules, compiled into the build from the human-editable
/// `permissions.yaml` at the repository root. A deployer edits or replaces that
/// file to change the default policy; it is not read at runtime.
const DEFAULT_PERMISSION_RULES: &str = include_str!("../../../permissions.yaml");

/// Builds the upper-cased `COUNTRY:REGION` key for [`PermissionMaps::by_region`].
fn region_key(country: &str, region: &str) -> String {
    format!(
        "{}:{}",
        country.to_ascii_uppercase(),
        region.to_ascii_uppercase()
    )
}

impl PermissionMaps {
    /// Builds an empty map set with no country or region entries.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Registers explicit rules for an ISO 3166-1 alpha-2 country code.
    #[must_use]
    pub fn with_country(mut self, iso_code: &str, rules: CountryRules) -> Self {
        self.by_country.insert(iso_code.to_ascii_uppercase(), rules);
        self
    }

    /// Registers explicit rules for a region within a country, keyed by the ISO
    /// 3166-1 alpha-2 country and the geo provider's region code (for example
    /// `US` and `CA`).
    ///
    /// A region entry takes precedence over the country entry, so a deployer can
    /// vary a single state or province on top of the country baseline.
    #[must_use]
    pub fn with_region(mut self, iso_country: &str, region: &str, rules: CountryRules) -> Self {
        self.by_region
            .insert(region_key(iso_country, region), rules);
        self
    }

    /// The built-in default rules, parsed from the embedded `permissions.yaml`
    /// (see `DEFAULT_PERMISSION_RULES`).
    ///
    /// The parse runs once per instance and the result is cached.
    ///
    /// # Panics
    ///
    /// Panics if the embedded `permissions.yaml` fails to parse. The file is a
    /// build-time constant covered by tests, so a panic means the committed file
    /// is malformed, not a runtime condition.
    #[must_use]
    pub fn standard() -> &'static Self {
        static CACHE: OnceLock<PermissionMaps> = OnceLock::new();
        CACHE.get_or_init(|| {
            Self::from_yaml(DEFAULT_PERMISSION_RULES)
                .expect("should parse the embedded default permissions.yaml")
        })
    }

    /// Builds the maps from a `permissions.yaml` document: named `groups` and
    /// the `rules` that map a country or country/region to a group.
    ///
    /// # Errors
    ///
    /// Returns [`PermissionsError`] when the YAML is malformed or names an
    /// unknown group, permission, or acquisition rule.
    pub fn from_yaml(yaml: &str) -> Result<Self, PermissionsError> {
        let file: RulesFile =
            serde_yaml_ng::from_str(yaml).map_err(|error| PermissionsError::Parse {
                message: error.to_string(),
            })?;

        // Build every named group into its CountryRules.
        let mut groups: BTreeMap<String, CountryRules> = BTreeMap::new();
        for (name, flags) in &file.groups {
            groups.insert(name.clone(), group_rules(name, flags)?);
        }

        let mut maps = Self::empty();
        for (key, spec) in &file.rules {
            let rules = match spec {
                RuleSpec::Group(name) => resolve_group(&groups, name)?,
                RuleSpec::Detailed { group, permissions } => {
                    apply_modifications(resolve_group(&groups, group)?, permissions)?
                }
            };
            // A `country/region` key (for example `US/CA`) layers a region rule
            // on top of its country; a bare `country` key sets the country rule.
            match key.split_once('/') {
                Some((country, region)) => maps = maps.with_region(country, region, rules),
                None => maps = maps.with_country(key, rules),
            }
        }
        Ok(maps)
    }

    /// Returns the rules that apply to `country` and `region`, preferring a
    /// region entry, then the country entry, or `None` when neither matches.
    #[must_use]
    pub fn rules_for(&self, country: Option<&str>, region: Option<&str>) -> Option<&CountryRules> {
        if let (Some(country), Some(region)) = (country, region)
            && let Some(rules) = self.by_region.get(&region_key(country, region))
        {
            return Some(rules);
        }
        country
            .map(str::to_ascii_uppercase)
            .and_then(|code| self.by_country.get(&code))
    }

    /// The rules for `country`/`region`, falling back to the configured default
    /// location when the request's own country and region match no rule.
    ///
    /// Returns `None` only when neither resolves (no default configured, or the
    /// default itself has no rule), which the caller treats as the
    /// requires-signal floor. In a validated deployment this is unreachable: a
    /// default is required and checked at startup by
    /// [`GeoConfig::validate_default_country`](crate::settings::GeoConfig::validate_default_country),
    /// so a resolvable default always exists. The floor remains the behavior for
    /// an unconfigured map, exercised by unit tests rather than reached at
    /// runtime.
    fn rules_or_default(
        &self,
        country: Option<&str>,
        region: Option<&str>,
        default_country: Option<&str>,
        default_region: Option<&str>,
    ) -> Option<&CountryRules> {
        self.rules_for(country, region)
            .or_else(|| self.rules_for(default_country, default_region))
    }

    /// Resolves the permission state for a request: the country/region baseline
    /// augmented by a session signal.
    ///
    /// `country` and `region` are what a geo provider returns (`region` may be
    /// `None`). `default_country`/`default_region` are the deployer's configured
    /// default location, used when the request's own country and region match no
    /// rule. When neither matches (no default configured, or the default has no
    /// rule) every permission is `RequiresSignal`, so nothing is set without a
    /// signal. `signal` maps each permission to a [`ConsentSignal`]; the caller
    /// derives it from its consent model so this module stays independent of how
    /// a signal is decoded. A `Granted` baseline is set unless the signal is
    /// `Revoke`, a `RequiresSignal` baseline is set only on `Grant`, and `Denied`
    /// is never set.
    #[must_use]
    pub fn resolve_with(
        &self,
        country: Option<&str>,
        region: Option<&str>,
        default_country: Option<&str>,
        default_region: Option<&str>,
        signal: impl Fn(Permission) -> ConsentSignal,
    ) -> PermissionState {
        let rules = self.rules_or_default(country, region, default_country, default_region);
        let acquisition =
            |permission| rules.map_or(Acquisition::RequiresSignal, |r| r.rule_for(permission));
        let set = Permission::ALL
            .into_iter()
            .filter(
                |&permission| match (acquisition(permission), signal(permission)) {
                    (Acquisition::Denied, _) => false,
                    (Acquisition::Granted, ConsentSignal::Revoke) => false,
                    (Acquisition::Granted, _) => true,
                    (Acquisition::RequiresSignal, ConsentSignal::Grant) => true,
                    (Acquisition::RequiresSignal, _) => false,
                },
            )
            .collect();
        PermissionState { set }
    }

    /// The baseline permission state for a country and region with no session
    /// signal.
    ///
    /// Permissions exist without a consent model, so this is the set of
    /// `Granted` permissions for the location (or the configured default), and is
    /// what a request resolves to when no signal is present.
    #[must_use]
    pub fn baseline(
        &self,
        country: Option<&str>,
        region: Option<&str>,
        default_country: Option<&str>,
        default_region: Option<&str>,
    ) -> PermissionState {
        self.resolve_with(country, region, default_country, default_region, |_| {
            ConsentSignal::Neutral
        })
    }

    /// Convenience over [`resolve_with`](Self::resolve_with) for a boolean
    /// signal with no region and no revocation: a `true` grants the permission
    /// and a `false` is neutral.
    #[must_use]
    pub fn resolve(
        &self,
        country: Option<&str>,
        default_country: Option<&str>,
        signal: impl Fn(Permission) -> bool,
    ) -> PermissionState {
        self.resolve_with(country, None, default_country, None, |permission| {
            if signal(permission) {
                ConsentSignal::Grant
            } else {
                ConsentSignal::Neutral
            }
        })
    }
}

/// The permissions Trusted Server currently has set for a request.
///
/// A provider executes only when [`all_set`](Self::all_set) of its required
/// permissions returns `true`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermissionState {
    set: PermissionSet,
}

impl PermissionState {
    /// Builds a state in which exactly the permissions in `set` are set, for
    /// tests and callers that compute the set directly.
    #[must_use]
    pub const fn new(set: PermissionSet) -> Self {
        Self { set }
    }

    /// Whether a single permission is set.
    #[must_use]
    pub const fn is_set(&self, permission: Permission) -> bool {
        self.set.contains(permission)
    }

    /// Whether every permission in `required` is set. An empty requirement is
    /// always satisfied, so a provider that requires nothing always runs.
    #[must_use]
    pub const fn all_set(&self, required: PermissionSet) -> bool {
        self.set.contains_all(required)
    }

    /// The full set of permissions that are set, for a provider that adapts its
    /// behavior to whatever is present.
    #[must_use]
    pub const fn permissions(&self) -> PermissionSet {
        self.set
    }
}

// ---------------------------------------------------------------------------
// permissions.yaml parsing
// ---------------------------------------------------------------------------

/// The shape of a `permissions.yaml` document.
#[derive(Debug, Deserialize)]
struct RulesFile {
    /// Named permission baselines, keyed by group name. Each group is a flat map
    /// of `default` plus optional per-permission flags.
    #[serde(default)]
    groups: BTreeMap<String, BTreeMap<String, String>>,
    /// Rules keyed by country (`FR`) or country and region (`US/CA`).
    #[serde(default)]
    rules: BTreeMap<String, RuleSpec>,
}

/// A rule entry: either a bare group name, or a group with explicit
/// per-permission modifications applied on top.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RuleSpec {
    /// A bare group name, for example `gdpr-eu`.
    Group(String),
    /// A group with `+permission` / `-permission` modifications.
    Detailed {
        group: String,
        #[serde(default)]
        permissions: Vec<String>,
    },
}

/// Resolves an acquisition rule name to its [`Acquisition`].
fn parse_acquisition(value: &str) -> Result<Acquisition, PermissionsError> {
    match value {
        "granted" => Ok(Acquisition::Granted),
        "requires_signal" => Ok(Acquisition::RequiresSignal),
        "denied" => Ok(Acquisition::Denied),
        other => Err(PermissionsError::UnknownAcquisition {
            value: other.to_owned(),
        }),
    }
}

/// Builds a group's [`CountryRules`] from its flag map. Each key names a
/// permission and its flag; an optional `default` key sets any permission the
/// group omits. A group without a `default` must list every permission, so its
/// meaning is fully explicit (this is how the shipped groups are written).
fn group_rules(
    name: &str,
    flags: &BTreeMap<String, String>,
) -> Result<CountryRules, PermissionsError> {
    let default = flags
        .get("default")
        .map(|value| parse_acquisition(value))
        .transpose()?;
    // With no `default`, every permission must be listed, so this placeholder is
    // never consulted once completeness is checked below.
    let mut rules = CountryRules::with_default(default.unwrap_or(Acquisition::Denied));
    let mut listed = PermissionSet::none();
    for (key, value) in flags {
        if key == "default" {
            continue;
        }
        let permission = Permission::from_identifier(key)
            .ok_or_else(|| PermissionsError::UnknownPermission { name: key.clone() })?;
        rules = rules.with_rule(permission, parse_acquisition(value)?);
        listed = listed.with(permission);
    }
    if default.is_none() {
        for permission in Permission::ALL {
            if !listed.contains(permission) {
                return Err(PermissionsError::IncompleteGroup {
                    group: name.to_owned(),
                    permission: permission.to_string(),
                });
            }
        }
    }
    Ok(rules)
}

/// Looks up a group by name, erroring when a rule references a group that is not
/// defined.
fn resolve_group(
    groups: &BTreeMap<String, CountryRules>,
    name: &str,
) -> Result<CountryRules, PermissionsError> {
    groups
        .get(name)
        .cloned()
        .ok_or_else(|| PermissionsError::UnknownGroup {
            name: name.to_owned(),
        })
}

/// Applies `+permission` (granted) and `-permission` (denied) modifications on
/// top of a group's rules, overriding the group's baseline for each.
fn apply_modifications(
    mut rules: CountryRules,
    modifications: &[String],
) -> Result<CountryRules, PermissionsError> {
    for modification in modifications {
        let (acquisition, name) = if let Some(name) = modification.strip_prefix('+') {
            (Acquisition::Granted, name)
        } else if let Some(name) = modification.strip_prefix('-') {
            (Acquisition::Denied, name)
        } else {
            return Err(PermissionsError::InvalidModification {
                value: modification.clone(),
            });
        };
        let permission = Permission::from_identifier(name).ok_or_else(|| {
            PermissionsError::UnknownPermission {
                name: name.to_owned(),
            }
        })?;
        rules = rules.with_rule(permission, acquisition);
    }
    Ok(rules)
}

/// An error parsing a `permissions.yaml` document.
#[derive(Debug, derive_more::Display)]
pub enum PermissionsError {
    /// The YAML was malformed or did not match the expected shape.
    #[display("failed to parse permission rules: {message}")]
    Parse { message: String },
    /// A group without a `default` did not list every permission.
    #[display(
        "permission group `{group}` has no `default` and is missing a flag for `{permission}` (list every permission, or add a `default`)"
    )]
    IncompleteGroup { group: String, permission: String },
    /// A rule referenced a group that is not defined.
    #[display("unknown permission group `{name}`")]
    UnknownGroup { name: String },
    /// A permission flag or modification named an unknown permission.
    #[display("unknown permission `{name}`")]
    UnknownPermission { name: String },
    /// An acquisition rule was not `granted`, `requires_signal`, or `denied`.
    #[display("unknown acquisition rule `{value}` (expected granted, requires_signal, or denied)")]
    UnknownAcquisition { value: String },
    /// A rule modification did not start with `+` or `-`.
    #[display("permission modification `{value}` must start with `+` or `-`")]
    InvalidModification { value: String },
}

impl core::error::Error for PermissionsError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_set_membership_and_subset() {
        let set = PermissionSet::none()
            .with(Permission::StoreOnDevice)
            .with(Permission::SelectBasicAds);

        assert!(set.contains(Permission::StoreOnDevice));
        assert!(set.contains(Permission::SelectBasicAds));
        assert!(
            !set.contains(Permission::SelectPersonalisedAds),
            "an absent permission should not be reported as present"
        );

        let required = PermissionSet::none().with(Permission::StoreOnDevice);
        assert!(set.contains_all(required), "a subset should be contained");
        assert!(
            !required.contains_all(set),
            "a superset is not contained in a subset"
        );
        assert!(
            set.contains_all(PermissionSet::none()),
            "the empty requirement is always satisfied"
        );
    }

    #[test]
    fn permission_set_iterates_in_purpose_order() {
        let set = PermissionSet::none()
            .with(Permission::SelectBasicAds)
            .with(Permission::StoreOnDevice);
        let order: Vec<u8> = set.iter().map(Permission::tcf_purpose).collect();
        assert_eq!(
            order,
            vec![1, 2],
            "iteration should be in TCF purpose order"
        );
    }

    #[test]
    fn tcf_purpose_numbers_are_one_to_eleven() {
        let numbers: Vec<u8> = Permission::ALL.iter().map(|p| p.tcf_purpose()).collect();
        assert_eq!(
            numbers,
            (1..=11).collect::<Vec<_>>(),
            "purposes should be 1..=11"
        );
    }

    #[test]
    fn the_floor_sets_a_permission_only_when_a_signal_grants_it() {
        // Empty maps and no default: every permission is the requires-signal
        // floor, set only when a signal grants it.
        let maps = PermissionMaps::default();

        let denied = maps.resolve(Some("GB"), None, |_| false);
        assert!(
            !denied.is_set(Permission::StoreOnDevice),
            "the floor should not set store-on-device without a signal"
        );

        let granted = maps.resolve(Some("GB"), None, |p| p == Permission::StoreOnDevice);
        assert!(
            granted.is_set(Permission::StoreOnDevice),
            "the floor should set store-on-device once a signal grants it"
        );
    }

    #[test]
    fn unknown_country_uses_the_configured_default() {
        // A map with a granted "us" rule, used as the default for unknown geo.
        let maps = PermissionMaps::empty()
            .with_country("us", CountryRules::with_default(Acquisition::Granted));
        // No country, default US: the US (granted) rule applies.
        assert!(
            maps.resolve(None, Some("US"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "the configured default should set permissions when geo gives no country"
        );
        // No country and no default: the requires-signal floor sets nothing.
        assert!(
            !maps
                .resolve(None, None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "with no default, an unknown country sets nothing without a signal"
        );
    }

    #[test]
    fn a_matching_country_is_used_over_the_default() {
        // US grants; the default points at an opt-in "de" rule.
        let maps = PermissionMaps::empty()
            .with_country("us", CountryRules::with_default(Acquisition::Granted))
            .with_country(
                "de",
                CountryRules::with_default(Acquisition::RequiresSignal),
            );
        // US has its own rule, used directly even when a default is configured.
        assert!(
            maps.resolve(Some("US"), Some("DE"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "a country with its own rule uses it, not the default"
        );
        // An unmapped country falls through to the default (de, requires signal).
        assert!(
            !maps
                .resolve(Some("ZZ"), Some("DE"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "an unmapped country uses the default rule"
        );
    }

    #[test]
    fn per_permission_override_beats_the_country_default() {
        // Granted by default, but deny store-on-device specifically.
        let rules = CountryRules::with_default(Acquisition::Granted)
            .with_rule(Permission::StoreOnDevice, Acquisition::Denied);
        let maps = PermissionMaps::empty().with_country("zz", rules);
        let state = maps.resolve(Some("ZZ"), None, |_| true);

        assert!(
            !state.is_set(Permission::StoreOnDevice),
            "an explicit Denied override should beat the granted default"
        );
        assert!(
            state.is_set(Permission::SelectBasicAds),
            "other permissions should still follow the granted default"
        );
    }

    #[test]
    fn all_set_gates_on_the_required_set() {
        let state = PermissionState::new(PermissionSet::none().with(Permission::StoreOnDevice));

        assert!(
            state.all_set(PermissionSet::none()),
            "a provider requiring nothing always runs"
        );
        assert!(
            state.all_set(PermissionSet::none().with(Permission::StoreOnDevice)),
            "a set requirement is satisfied"
        );
        assert!(
            !state.all_set(PermissionSet::none().with(Permission::SelectPersonalisedAds)),
            "an unset requirement is not satisfied"
        );
    }

    #[test]
    fn with_default_sets_the_baseline_acquisition() {
        // A granted default is set with no signal required.
        let granted = PermissionMaps::empty()
            .with_country("zz", CountryRules::with_default(Acquisition::Granted));
        assert!(
            granted
                .resolve(Some("ZZ"), None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "a granted default should set with no signal"
        );
        // A requires-signal default is set only once a signal grants it.
        let opt_in = PermissionMaps::empty().with_country(
            "zz",
            CountryRules::with_default(Acquisition::RequiresSignal),
        );
        assert!(
            !opt_in
                .resolve(Some("ZZ"), None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "a requires-signal default should not be set without a signal"
        );
        assert!(
            opt_in
                .resolve(Some("ZZ"), None, |p| p == Permission::StoreOnDevice)
                .is_set(Permission::StoreOnDevice),
            "a requires-signal default should set once a signal grants it"
        );
    }

    #[test]
    fn granted_and_denied_rules_ignore_signals() {
        // A Granted rule is set even when no signal is present.
        let granted = CountryRules::with_default(Acquisition::RequiresSignal)
            .with_rule(Permission::StoreOnDevice, Acquisition::Granted);
        assert!(
            PermissionMaps::empty()
                .with_country("zz", granted)
                .resolve(Some("ZZ"), None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "a Granted rule is set with no signal"
        );

        // A Denied rule is never set, even when every signal grants.
        let denied = CountryRules::with_default(Acquisition::Granted)
            .with_rule(Permission::StoreOnDevice, Acquisition::Denied);
        assert!(
            !PermissionMaps::empty()
                .with_country("zz", denied)
                .resolve(Some("ZZ"), None, |_| true)
                .is_set(Permission::StoreOnDevice),
            "a Denied rule is never set even with a signal"
        );
    }

    #[test]
    fn standard_maps_eu_requires_signal_and_uk_grants_storage() {
        let maps = PermissionMaps::standard();
        assert!(
            !maps
                .resolve(Some("DE"), None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "an EU country should not set store-on-device without a signal"
        );
        assert!(
            maps.resolve(Some("DE"), None, |p| p == Permission::StoreOnDevice)
                .is_set(Permission::StoreOnDevice),
            "an EU country should set store-on-device once a signal grants it"
        );
        assert!(
            maps.resolve(Some("GB"), None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "the UK should grant store-on-device without a signal"
        );
    }

    #[test]
    fn standard_maps_us_and_australia_grant_storage_by_default() {
        let maps = PermissionMaps::standard();
        for code in ["US", "AU"] {
            assert!(
                maps.resolve(Some(code), None, |_| false)
                    .is_set(Permission::StoreOnDevice),
                "{code} should grant store-on-device by default"
            );
        }
        // No default configured, so an unmapped country hits the requires-signal
        // floor and sets nothing without a signal.
        assert!(
            !maps
                .resolve(Some("ZZ"), None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "an unmapped country with no default sets nothing without a signal"
        );
    }

    #[test]
    fn resolve_with_revokes_a_granted_permission_on_opt_out() {
        // US grants store-on-device by default; an opt-out signal revokes it.
        let maps = PermissionMaps::standard();
        assert!(
            maps.baseline(Some("US"), None, None, None)
                .is_set(Permission::StoreOnDevice),
            "the US baseline should set store-on-device"
        );
        let revoked = maps.resolve_with(Some("US"), None, None, None, |p| {
            if p == Permission::StoreOnDevice {
                ConsentSignal::Revoke
            } else {
                ConsentSignal::Neutral
            }
        });
        assert!(
            !revoked.is_set(Permission::StoreOnDevice),
            "an opt-out signal should revoke a granted permission"
        );
    }

    #[test]
    fn a_region_entry_overrides_the_country_baseline() {
        // US grants by default; a state can require a signal instead.
        let maps = PermissionMaps::standard().clone().with_region(
            "US",
            "CA",
            CountryRules::with_default(Acquisition::RequiresSignal),
        );
        assert!(
            !maps
                .baseline(Some("US"), Some("CA"), None, None)
                .is_set(Permission::StoreOnDevice),
            "the CA region rule should require a signal, overriding the US baseline"
        );
        assert!(
            maps.baseline(Some("US"), Some("NY"), None, None)
                .is_set(Permission::StoreOnDevice),
            "a state with no region entry should follow the US baseline"
        );
    }

    #[test]
    fn from_yaml_parses_groups_rules_and_modifications() {
        let yaml = r#"
groups:
  eu:
    default: requires_signal
  us:
    default: granted
rules:
  FR: eu
  US: us
  US/CA:
    group: eu
    permissions: [+store-on-device, -select-basic-ads]
"#;
        let maps = PermissionMaps::from_yaml(yaml).expect("should parse the rules");

        // Bare group references.
        assert!(
            !maps
                .baseline(Some("FR"), None, None, None)
                .is_set(Permission::StoreOnDevice),
            "FR (eu) requires a signal for device storage"
        );
        assert!(
            maps.baseline(Some("US"), None, None, None)
                .is_set(Permission::StoreOnDevice),
            "US (us) grants device storage"
        );

        // CA references the eu group, but +store-on-device grants it, overriding
        // the eu baseline.
        assert!(
            maps.baseline(Some("US"), Some("CA"), None, None)
                .is_set(Permission::StoreOnDevice),
            "+store-on-device grants it for CA, overriding the eu baseline"
        );
        // -select-basic-ads denies it: not set even when a signal grants it.
        assert!(
            !maps
                .resolve_with(Some("US"), Some("CA"), None, None, |_| ConsentSignal::Grant)
                .is_set(Permission::SelectBasicAds),
            "-select-basic-ads denies it even when a signal grants it"
        );

        // An unmapped country with a default of `US` uses the us (granted) rule.
        assert!(
            maps.baseline(Some("ZZ"), None, Some("US"), None)
                .is_set(Permission::StoreOnDevice),
            "an unmapped country uses the configured default (us, granted)"
        );
        // With no default, an unmapped country hits the requires-signal floor.
        assert!(
            !maps
                .baseline(Some("ZZ"), None, None, None)
                .is_set(Permission::StoreOnDevice),
            "with no default, an unmapped country sets nothing"
        );
    }

    #[test]
    fn from_yaml_rejects_unknown_group() {
        let err = PermissionMaps::from_yaml("groups: {}\nrules:\n  US: nope\n")
            .expect_err("a rule naming an undefined group should be rejected");
        assert!(
            matches!(err, PermissionsError::UnknownGroup { .. }),
            "should report an unknown group, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_rejects_an_incomplete_group_without_default() {
        // A group with no `default` must list every permission, so this one
        // (only store-on-device) is rejected rather than silently leaving the
        // other ten unset.
        let yaml = "groups:\n  g:\n    store-on-device: granted\nrules: {}\n";
        let err = PermissionMaps::from_yaml(yaml)
            .expect_err("an incomplete group without a default should be rejected");
        assert!(
            matches!(err, PermissionsError::IncompleteGroup { .. }),
            "should report an incomplete group, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_accepts_an_explicit_group_listing_every_permission() {
        // The shipped style: no `default`, every permission spelled out.
        let mut group = String::from("groups:\n  everything:\n");
        for permission in Permission::ALL {
            group.push_str(&format!("    {permission}: granted\n"));
        }
        let yaml = format!("{group}rules:\n  US: everything\n");
        let maps = PermissionMaps::from_yaml(&yaml).expect("an explicit group should parse");
        assert!(
            maps.baseline(Some("US"), None, None, None)
                .is_set(Permission::MarketResearch),
            "every listed permission should take its flag"
        );
    }

    #[test]
    fn from_yaml_rejects_unknown_permission() {
        let yaml = "groups:\n  g:\n    default: granted\n    not-a-permission: denied\nrules: {}\n";
        let err =
            PermissionMaps::from_yaml(yaml).expect_err("an unknown permission should be rejected");
        assert!(
            matches!(err, PermissionsError::UnknownPermission { .. }),
            "should report an unknown permission, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_rejects_unknown_acquisition() {
        let yaml = "groups:\n  g:\n    default: maybe\nrules: {}\n";
        let err =
            PermissionMaps::from_yaml(yaml).expect_err("an unknown acquisition should be rejected");
        assert!(
            matches!(err, PermissionsError::UnknownAcquisition { .. }),
            "should report an unknown acquisition, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_rejects_modification_without_sign() {
        let yaml = "groups:\n  g:\n    default: granted\nrules:\n  US:\n    group: g\n    permissions: [store-on-device]\n";
        let err = PermissionMaps::from_yaml(yaml)
            .expect_err("a modification without +/- should be rejected");
        assert!(
            matches!(err, PermissionsError::InvalidModification { .. }),
            "should report an invalid modification, got {err:?}"
        );
    }
}
