//! Provider permissions: a technical permission model gating provider execution.
//!
//! A provider (Edge Cookie, device, or geo) advertises the [`Permission`]s its
//! data use *requires*. Trusted Server resolves which permissions are currently
//! *set* from the session's signals and the country it resolves to, and refuses
//! to execute a provider whose required permissions are not set.
//!
//! The vocabulary is the IAB Privacy Taxonomy Data Uses, mapped from the IAB TCF
//! Europe purposes and used **only** as a technical identifier for a permission.
//! Two purposes have no Data Use yet: TCF purpose 1 (device storage) uses a
//! proposed `necessary.operations.storage` key and TCF purpose 11 keeps its TCF
//! identifier. No CMP or TCF *policy* is implemented here, and
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

/// A technical permission a provider may require, labeled with its IAB Privacy
/// Taxonomy Data Use, or its IAB TCF Europe purpose where no Data Use exists yet.
///
/// Only the identifier is used, with no TCF or taxonomy policy implemented. Only
/// [`Permission::StoreOnDevice`] (Purpose 1) and
/// [`Permission::SelectPersonalisedAds`] (Purpose 4) are resolved against a
/// signal today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Permission {
    /// TCF Purpose 1, store and/or access information on a device. Resolved
    /// against a session signal today. No IAB Privacy Taxonomy Data Use exists
    /// for device storage yet, so this uses a proposed `necessary.operations`
    /// key pending an upstream addition.
    StoreOnDevice,
    /// TCF Purpose 2, use limited data to select advertising.
    SelectBasicAds,
    /// TCF Purpose 3, create profiles for personalised advertising.
    CreateAdsProfile,
    /// TCF Purpose 4, use profiles to select personalised advertising.
    SelectPersonalisedAds,
    /// TCF Purpose 5, create profiles to personalise content.
    CreateContentProfile,
    /// TCF Purpose 6, use profiles to select personalised content.
    SelectPersonalisedContent,
    /// TCF Purpose 7, measure advertising performance.
    MeasureAdPerformance,
    /// TCF Purpose 8, measure content performance.
    MeasureContentPerformance,
    /// TCF Purpose 9, understand audiences through statistics.
    MarketResearch,
    /// TCF Purpose 10, develop and improve services.
    DevelopServices,
    /// TCF Purpose 11, use limited data to select content. No IAB Privacy
    /// Taxonomy Data Use exists for limited-data content selection yet, so this
    /// keeps its TCF identifier and is proposed upstream. Not gated today.
    SelectBasicContent,
    /// An IAB Privacy Taxonomy Data Use with no dedicated variant, identified by
    /// its index into [`EXTRA_DATA_USES`]. These carry a policy flag in
    /// `permissions.yaml` for completeness; no provider gates on them today.
    Extra(u8),
}

/// The Data Use identifiers for the named [`Permission`] variants, in variant
/// order (bit index 0..11).
const NAMED_DATA_USES: [&str; 11] = [
    "necessary.operations.storage",
    "advertising_marketing.first_party.contextual",
    "advertising_marketing.profiling",
    "advertising_marketing.first_party.targeted",
    "advertising_marketing.personalize.profiling",
    "advertising_marketing.personalize.content",
    "analytics.ad_reporting.measure_ad_performance",
    "analytics.ad_reporting.content_performance",
    "analytics.ad_reporting.market_research",
    "necessary.operations.improve",
    "select-basic-content",
];

/// Every other IAB Privacy Taxonomy Data Use, carried so `permissions.yaml` can
/// set a policy flag for the whole taxonomy (bit index 11..). No provider gates
/// on these today; they exist for completeness, testing, and demonstration.
const EXTRA_DATA_USES: [&str; 53] = [
    "advertising_marketing",
    "advertising_marketing.communications",
    "advertising_marketing.communications.email",
    "advertising_marketing.communications.sms",
    "advertising_marketing.first_party",
    "advertising_marketing.frequency_capping",
    "advertising_marketing.negative_targeting",
    "advertising_marketing.personalize",
    "advertising_marketing.personalize.system",
    "advertising_marketing.serving",
    "advertising_marketing.third_party",
    "advertising_marketing.third_party.targeted",
    "analytics",
    "analytics.ad_reporting",
    "analytics.ad_reporting.ad_delivery_and_targeting",
    "analytics.ad_reporting.ad_fraud_detection",
    "analytics.ad_reporting.ad_viewability",
    "analytics.ad_reporting.campaign_insights",
    "analytics.reporting",
    "analytics.reporting.system",
    "disclosure",
    "disclosure.law_enforcement",
    "disclosure.outside_counsel",
    "disclosure.sale",
    "disclosure.share",
    "disclosure.third_party_sale",
    "functional",
    "functional.performance",
    "functional.personalization",
    "functional.security",
    "necessary",
    "necessary.employment",
    "necessary.employment.hr",
    "necessary.employment.hr.hiring",
    "necessary.fraud_detection",
    "necessary.legal_obligation",
    "necessary.legal_obligation.age_verification",
    "necessary.legal_obligation.content_moderation",
    "necessary.legal_obligation.dsr",
    "necessary.legal_obligation.hold",
    "necessary.operations",
    "necessary.operations.authentication",
    "necessary.operations.debugging",
    "necessary.operations.notifications",
    "necessary.operations.notifications.email",
    "necessary.operations.notifications.sms",
    "necessary.operations.payment_processing",
    "necessary.operations.quality_assurance",
    "necessary.operations.security",
    "necessary.operations.support",
    "necessary.operations.survey",
    "necessary.operations.upgrades",
    "necessary.operations.website_use",
];

impl Permission {
    /// The named permission variants, in bit-index order.
    const NAMED: [Permission; 11] = [
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

    /// Every modeled permission: the named variants, then every other Privacy
    /// Taxonomy Data Use.
    pub fn all() -> impl Iterator<Item = Permission> {
        Self::NAMED
            .into_iter()
            .chain((0..EXTRA_DATA_USES.len() as u8).map(Permission::Extra))
    }

    /// The Data Use identifier for this permission.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Extra(index) => EXTRA_DATA_USES[index as usize],
            named => NAMED_DATA_USES[named.index() as usize],
        }
    }

    /// The stable bit position for this permission within a [`PermissionSet`].
    #[must_use]
    const fn index(self) -> u8 {
        match self {
            Permission::StoreOnDevice => 0,
            Permission::SelectBasicAds => 1,
            Permission::CreateAdsProfile => 2,
            Permission::SelectPersonalisedAds => 3,
            Permission::CreateContentProfile => 4,
            Permission::SelectPersonalisedContent => 5,
            Permission::MeasureAdPerformance => 6,
            Permission::MeasureContentPerformance => 7,
            Permission::MarketResearch => 8,
            Permission::DevelopServices => 9,
            Permission::SelectBasicContent => 10,
            Permission::Extra(index) => 11 + index,
        }
    }

    /// The single-bit mask for this permission within a [`PermissionSet`].
    const fn bit(self) -> u128 {
        1 << self.index()
    }

    /// Returns the permission whose Data Use identifier matches `id` (for
    /// example `"necessary.operations.storage"`), or `None` when it is unknown.
    ///
    /// Used to parse permission names from `permissions.yaml`.
    #[must_use]
    pub fn from_identifier(id: &str) -> Option<Permission> {
        Permission::all().find(|p| p.as_str() == id)
    }
}

impl core::fmt::Display for Permission {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A set of [`Permission`]s, stored as a bitset keyed by each permission's bit
/// index.
///
/// Used both for what a provider requires and for what Trusted Server has set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermissionSet(u128);

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
        Permission::all().filter(move |p| self.contains(*p))
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
        self.overrides.insert(permission.index(), acquisition);
        self
    }

    /// The acquisition rule for `permission`.
    #[must_use]
    pub fn rule_for(&self, permission: Permission) -> Acquisition {
        self.overrides
            .get(&permission.index())
            .copied()
            .unwrap_or(self.default)
    }
}

/// Which Data Uses a signal revokes.
#[derive(Debug, Clone, Default)]
enum RevokeSet {
    /// The signal revokes nothing.
    #[default]
    None,
    /// The signal revokes every Data Use (the map bounds what a revoke drops).
    All,
    /// The signal revokes only the listed Data Uses.
    Set(PermissionSet),
}

/// How each session signal maps onto permissions, parsed from the `signals`
/// section of `permissions.yaml`.
///
/// The permission model holds this as data so the consent mapping applies it
/// rather than encoding any signal policy in the code. It is jurisdiction-free:
/// it says only how a decoded signal grants or revokes each Data Use, and the
/// country/region baseline decides the rest.
#[derive(Debug, Clone, Default)]
pub(crate) struct SignalPolicy {
    /// Whether a present TCF record overrides the opt-out signals.
    tcf_authoritative: bool,
    /// Permission bit index to the TCF purpose number that grants it.
    tcf_purpose: BTreeMap<u8, u8>,
    /// The signals that constitute a US-style opt-out.
    opt_out_sources: Vec<OptOutSource>,
    /// Which Data Uses a US-style opt-out revokes.
    opt_out_revokes: RevokeSet,
}

impl SignalPolicy {
    /// Whether a present TCF record overrides the opt-out signals.
    pub(crate) fn tcf_authoritative(&self) -> bool {
        self.tcf_authoritative
    }

    /// The TCF purpose number that grants `permission`, or `None` when no purpose
    /// maps to it.
    pub(crate) fn tcf_purpose(&self, permission: Permission) -> Option<u8> {
        self.tcf_purpose.get(&permission.index()).copied()
    }

    /// The signals that constitute a US-style opt-out.
    pub(crate) fn opt_out_sources(&self) -> &[OptOutSource] {
        &self.opt_out_sources
    }

    /// Whether a US-style opt-out revokes `permission`.
    pub(crate) fn opt_out_revokes(&self, permission: Permission) -> bool {
        match &self.opt_out_revokes {
            RevokeSet::None => false,
            RevokeSet::All => true,
            RevokeSet::Set(set) => set.contains(permission),
        }
    }
}

/// Builds a validated [`SignalPolicy`] from the parsed `signals` section,
/// erroring when it names an unknown Data Use or revoke rule.
fn build_signal_policy(spec: &SignalsSpec) -> Result<SignalPolicy, PermissionsError> {
    let mut policy = SignalPolicy::default();
    if let Some(tcf) = &spec.tcf {
        policy.tcf_authoritative = tcf.authoritative;
        for (purpose, data_use) in &tcf.purposes {
            let permission = Permission::from_identifier(data_use).ok_or_else(|| {
                PermissionsError::UnknownPermission {
                    name: data_use.clone(),
                }
            })?;
            policy.tcf_purpose.insert(permission.index(), *purpose);
        }
    }
    if let Some(opt_out) = &spec.us_opt_out {
        policy.opt_out_sources = opt_out.sources.clone();
        policy.opt_out_revokes = match &opt_out.revokes {
            RevokeSpec::Keyword(keyword) if keyword == "all" => RevokeSet::All,
            RevokeSpec::Keyword(other) => {
                return Err(PermissionsError::UnknownRevoke {
                    value: other.clone(),
                });
            }
            RevokeSpec::List(names) => {
                let mut set = PermissionSet::none();
                for name in names {
                    let permission = Permission::from_identifier(name).ok_or_else(|| {
                        PermissionsError::UnknownPermission { name: name.clone() }
                    })?;
                    set = set.with(permission);
                }
                RevokeSet::Set(set)
            }
        };
    }
    Ok(policy)
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
    signals: SignalPolicy,
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

    /// The signal-to-permission policy parsed from the `signals` section of
    /// `permissions.yaml`. The consent mapping reads this rather than encoding
    /// any signal policy in the code.
    #[must_use]
    pub(crate) fn signals(&self) -> &SignalPolicy {
        &self.signals
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
        maps.signals = build_signal_policy(&file.signals)?;
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
        let set = Permission::all()
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
    /// How each session signal maps onto Data Uses.
    #[serde(default)]
    signals: SignalsSpec,
}

/// The `signals` section: how each signal source maps onto Data Uses. Parsed
/// into a [`SignalPolicy`] by [`build_signal_policy`].
#[derive(Debug, Default, Deserialize)]
struct SignalsSpec {
    /// The TCF record mapping, or `None` when the file declares no TCF policy.
    #[serde(default)]
    tcf: Option<TcfSignalSpec>,
    /// The US-style opt-out mapping, or `None` when none is declared.
    #[serde(default)]
    us_opt_out: Option<OptOutSpec>,
}

/// The `signals.tcf` block.
#[derive(Debug, Deserialize)]
struct TcfSignalSpec {
    /// Whether a present TCF record overrides the opt-out signals.
    #[serde(default = "default_true")]
    authoritative: bool,
    /// TCF purpose number to the Data Use it grants (and revokes when the record
    /// does not consent to that purpose).
    #[serde(default)]
    purposes: BTreeMap<u8, String>,
}

/// The `signals.us_opt_out` block.
#[derive(Debug, Deserialize)]
struct OptOutSpec {
    /// The signals that constitute a US-style opt-out.
    #[serde(default)]
    sources: Vec<OptOutSource>,
    /// Which Data Uses the opt-out revokes.
    #[serde(default)]
    revokes: RevokeSpec,
}

/// A `revokes` value: the keyword `all`, or an explicit list of Data Uses.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RevokeSpec {
    /// A bare keyword, expected to be `all`.
    Keyword(String),
    /// An explicit list of Data Use identifiers.
    List(Vec<String>),
}

impl Default for RevokeSpec {
    fn default() -> Self {
        RevokeSpec::List(Vec::new())
    }
}

/// A single US-style opt-out signal source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OptOutSource {
    /// The `Sec-GPC` request header (Global Privacy Control).
    Gpc,
    /// A GPP US sale opt-out.
    GppSaleOptOut,
    /// A US Privacy string sale opt-out.
    UsPrivacyOptOut,
}

/// Default for `#[serde(default = ...)]` on a `bool` field that should be `true`.
fn default_true() -> bool {
    true
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
        for permission in Permission::all() {
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
    /// A `signals` opt-out `revokes` value was neither `all` nor a list.
    #[display("unknown revoke rule `{value}` (expected `all` or a list of Data Uses)")]
    UnknownRevoke { value: String },
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
    fn permission_set_iterates_in_bit_index_order() {
        let set = PermissionSet::none()
            .with(Permission::SelectBasicAds)
            .with(Permission::StoreOnDevice);
        let order: Vec<&str> = set.iter().map(Permission::as_str).collect();
        assert_eq!(
            order,
            vec![
                "necessary.operations.storage",
                "advertising_marketing.first_party.contextual"
            ],
            "iteration should be in stable bit-index order"
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
            "the floor should not set necessary.operations.storage without a signal"
        );

        let granted = maps.resolve(Some("GB"), None, |p| p == Permission::StoreOnDevice);
        assert!(
            granted.is_set(Permission::StoreOnDevice),
            "the floor should set necessary.operations.storage once a signal grants it"
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
        // Granted by default, but deny necessary.operations.storage specifically.
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
            "an EU country should not set necessary.operations.storage without a signal"
        );
        assert!(
            maps.resolve(Some("DE"), None, |p| p == Permission::StoreOnDevice)
                .is_set(Permission::StoreOnDevice),
            "an EU country should set necessary.operations.storage once a signal grants it"
        );
        assert!(
            maps.resolve(Some("GB"), None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "the UK should grant necessary.operations.storage without a signal"
        );
    }

    #[test]
    fn standard_maps_us_and_australia_grant_storage_by_default() {
        let maps = PermissionMaps::standard();
        for code in ["US", "AU"] {
            assert!(
                maps.resolve(Some(code), None, |_| false)
                    .is_set(Permission::StoreOnDevice),
                "{code} should grant necessary.operations.storage by default"
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
        // US grants necessary.operations.storage by default; an opt-out signal revokes it.
        let maps = PermissionMaps::standard();
        assert!(
            maps.baseline(Some("US"), None, None, None)
                .is_set(Permission::StoreOnDevice),
            "the US baseline should set necessary.operations.storage"
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
    permissions: [+necessary.operations.storage, -advertising_marketing.first_party.contextual]
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

        // CA references the eu group, but +necessary.operations.storage grants it, overriding
        // the eu baseline.
        assert!(
            maps.baseline(Some("US"), Some("CA"), None, None)
                .is_set(Permission::StoreOnDevice),
            "+necessary.operations.storage grants it for CA, overriding the eu baseline"
        );
        // -advertising_marketing.first_party.contextual denies it: not set even when a signal grants it.
        assert!(
            !maps
                .resolve_with(Some("US"), Some("CA"), None, None, |_| ConsentSignal::Grant)
                .is_set(Permission::SelectBasicAds),
            "-advertising_marketing.first_party.contextual denies it even when a signal grants it"
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
        // (only necessary.operations.storage) is rejected rather than silently leaving the
        // other ten unset.
        let yaml = "groups:\n  g:\n    necessary.operations.storage: granted\nrules: {}\n";
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
        for permission in Permission::all() {
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
        let yaml = "groups:\n  g:\n    default: granted\nrules:\n  US:\n    group: g\n    permissions: [necessary.operations.storage]\n";
        let err = PermissionMaps::from_yaml(yaml)
            .expect_err("a modification without +/- should be rejected");
        assert!(
            matches!(err, PermissionsError::InvalidModification { .. }),
            "should report an invalid modification, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_parses_the_signals_section_into_a_policy() {
        let yaml = "\
groups:
  g:
    default: requires_signal
rules:
  FR: g
signals:
  tcf:
    authoritative: true
    purposes:
      1: necessary.operations.storage
      4: advertising_marketing.first_party.targeted
  us_opt_out:
    sources: [gpc]
    revokes: [advertising_marketing.first_party.targeted]
";
        let maps = PermissionMaps::from_yaml(yaml).expect("should parse the signals section");
        let signals = maps.signals();
        assert!(signals.tcf_authoritative(), "tcf should be authoritative");
        assert_eq!(
            signals.tcf_purpose(Permission::StoreOnDevice),
            Some(1),
            "Purpose 1 should map to device storage"
        );
        assert_eq!(
            signals.tcf_purpose(Permission::SelectPersonalisedAds),
            Some(4),
            "Purpose 4 should map to targeted advertising"
        );
        assert_eq!(
            signals.tcf_purpose(Permission::CreateAdsProfile),
            None,
            "an unmapped Data Use has no purpose"
        );
        assert!(
            signals.opt_out_revokes(Permission::SelectPersonalisedAds),
            "a listed Data Use is revoked by the opt-out"
        );
        assert!(
            !signals.opt_out_revokes(Permission::StoreOnDevice),
            "an unlisted Data Use is not revoked by the opt-out"
        );
    }

    #[test]
    fn from_yaml_rejects_an_unknown_revoke_keyword() {
        let yaml = "\
groups:
  g:
    default: requires_signal
rules:
  FR: g
signals:
  us_opt_out:
    sources: [gpc]
    revokes: everything
";
        let err = PermissionMaps::from_yaml(yaml)
            .expect_err("an unknown revoke keyword should be rejected");
        assert!(
            matches!(err, PermissionsError::UnknownRevoke { .. }),
            "should report an unknown revoke rule, got {err:?}"
        );
    }
}
