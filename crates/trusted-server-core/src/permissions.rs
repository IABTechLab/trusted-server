//! Provider permissions: a technical permission model gating provider execution.
//!
//! A provider advertises the [`Permission`]s its data use *requires*. Trusted
//! Server resolves which permissions are currently *set* from the session's
//! signals and the country it resolves to, and refuses to run the Edge Cookie
//! provider when its required permissions are not set. The device and geo
//! providers declare their requirements through the same method, and the
//! built-in ones require none; gating their execution on that declaration is
//! follow-up work.
//!
//! The vocabulary is the IAB Privacy Taxonomy Data Uses, mapped from the IAB TCF
//! Europe purposes and used **only** as a technical identifier for a permission.
//! Two purposes have no Data Use yet: TCF purpose 1 (device storage) uses a
//! proposed `necessary.operations.storage` key and TCF purpose 11 keeps its TCF
//! identifier. No CMP or TCF *policy* is implemented here. Every TCF purpose
//! is resolved against the session's signals (a present TCF record grants or
//! revokes each purpose, and a US-style opt-out revokes), and the Data Uses
//! with no TCF purpose keep their configured baseline.
//!
//! How a permission is acquired varies by place, so the policy is written as a
//! tree of places: the top of the tree stands for the whole world, countries
//! sit under it keyed by ISO 3166-1 alpha-2 code, and a country's regions sit
//! under that country keyed by ISO 3166-2 subdivision code.
//! [`PermissionMaps::standard`] loads that tree from the embedded
//! `permissions.yaml` (see `DEFAULT_PERMISSION_RULES`).
//!
//! Resolution takes the most specific match and falls back to the node above:
//! the request's region when it is listed, otherwise its country, otherwise
//! the top of the tree. So a request with no country at all (no geo provider,
//! or a lookup that resolved nothing), and a request whose country has no rule,
//! both resolve to the top node's group. The top node also declares the
//! `jurisdiction` the consent gates use for a visitor whose place could not be
//! resolved (see [`PermissionMaps::default_jurisdiction`]). A geo provider that
//! reports an outright lookup failure is the exception, resolving every
//! permission to the requires-signal floor rather than the top node (see
//! [`PermissionMaps::floor_with`]), though no geo provider shipped today
//! reports one.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_yaml_ng::Value;

use crate::consent::jurisdiction::Jurisdiction;

/// A technical permission a provider may require, labeled with its IAB Privacy
/// Taxonomy Data Use, or its IAB TCF Europe purpose where no Data Use exists yet.
///
/// Only the identifier is used, with no TCF or taxonomy policy implemented. Every
/// named variant with a TCF purpose in `permissions.yaml` is resolved against
/// the session's signals. Only [`Permission::StoreOnDevice`] (and
/// [`Permission::SelectPersonalisedAds`] for sharing) gates a shipped provider
/// today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Permission {
    /// TCF Purpose 1, store and/or access information on a device. It gates the
    /// built-in Edge Cookie provider today. No IAB Privacy Taxonomy Data Use exists
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

    /// Iterates the permissions in the set, in bit-index order.
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Acquisition {
    /// Set without any signal, exempt or strictly necessary here.
    Granted,
    /// Set only when the incoming signals grant the matching TCF purpose.
    /// The default, matching the floor an unresolved location falls to.
    #[default]
    RequiresSignal,
    /// Never set in this country.
    Denied,
}

/// What a session signal says about a permission, layered on top of the
/// country/region baseline by the consent mapping.
///
/// This module never reads consent directly. A caller maps its consent model (or
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
    /// Whether a present TCF record's grants and revokes apply. This never
    /// lets a TCF record override an opt-out signal: an opt-out always
    /// suppresses the Data Uses it revokes.
    tcf_authoritative: bool,
    /// Permission bit index to the TCF purpose number that grants it.
    tcf_purpose: BTreeMap<u8, u8>,
    /// The signals that constitute a US-style opt-out.
    opt_out_sources: Vec<OptOutSource>,
    /// Which Data Uses a US-style opt-out revokes.
    opt_out_revokes: RevokeSet,
}

impl SignalPolicy {
    /// Whether a present TCF record's grants and revokes apply.
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

/// Errors when a Data Use is granted by more than one TCF purpose, because the
/// grant-and-revoke rule needs a single purpose to answer for each Data Use.
fn ensure_none_signal_duplicate(
    previous: Option<u8>,
    data_use: &str,
) -> Result<(), PermissionsError> {
    match previous {
        None => Ok(()),
        Some(_) => Err(PermissionsError::DuplicateTcfDataUse {
            name: data_use.to_owned(),
        }),
    }
}

/// Builds a validated [`SignalPolicy`] from the parsed `signals` section,
/// erroring when it names an unknown Data Use or revoke rule.
fn build_signal_policy(spec: &SignalsSpec) -> Result<SignalPolicy, PermissionsError> {
    let mut policy = SignalPolicy::default();
    if let Some(tcf) = &spec.tcf {
        policy.tcf_authoritative = tcf.authoritative;
        for (purpose, data_uses) in &tcf.purposes {
            for data_use in data_uses.identifiers() {
                let permission = Permission::from_identifier(data_use).ok_or_else(|| {
                    PermissionsError::UnknownPermission {
                        name: data_use.clone(),
                    }
                })?;
                let previous = policy.tcf_purpose.insert(permission.index(), *purpose);
                ensure_none_signal_duplicate(previous, data_use)?;
            }
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

/// The place tree from `permissions.yaml`, flattened for lookup.
///
/// `by_country` is keyed on the ISO 3166-1 alpha-2 code a geo provider returns
/// (upper-cased). `by_region` keeps the finer rules written under a country,
/// keyed by country and region (for example a US state), which take precedence
/// over the country entry. `default_rules` is the top node's group, the answer
/// for a request whose country and region are both unlisted, and for a request
/// with no place at all.
#[derive(Debug, Clone, Default)]
pub struct PermissionMaps {
    by_country: BTreeMap<String, CountryRules>,
    by_region: BTreeMap<String, CountryRules>,
    jurisdiction_by_country: BTreeMap<String, Jurisdiction>,
    jurisdiction_by_region: BTreeMap<String, Jurisdiction>,
    default_rules: Option<CountryRules>,
    default_jurisdiction: Option<Jurisdiction>,
    signals: SignalPolicy,
}

/// The default permission rules, compiled into the build from the repository's
/// vanilla sample in `config/permissions`. A deployer chooses or replaces the
/// compiled-in file to set the default policy; it is not read at runtime.
const DEFAULT_PERMISSION_RULES: &str = include_str!("../../../config/permissions/vanilla.yaml");

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

    /// Registers the top node's rules, which apply to a request whose country
    /// and region are both unlisted, and to a request with no place at all.
    ///
    /// A map built without one resolves such a request at the requires-signal
    /// floor, which is what [`PermissionMaps::empty`] does. Every map parsed
    /// from a `permissions.yaml` has one, because the top node's `group` is
    /// required.
    #[must_use]
    pub fn with_default_rules(mut self, rules: CountryRules) -> Self {
        self.default_rules = Some(rules);
        self
    }

    /// Registers the top node's jurisdiction, the consent handling for a
    /// visitor whose place could not be resolved.
    #[must_use]
    pub fn with_default_jurisdiction(mut self, jurisdiction: Jurisdiction) -> Self {
        self.default_jurisdiction = Some(jurisdiction);
        self
    }

    /// The jurisdiction the policy declares for a visitor whose place the geo
    /// provider could not resolve, taken from the top of the `rules:` tree.
    ///
    /// The consent gates resolve a jurisdiction from the request's place, so
    /// with no place they would resolve [`Jurisdiction::Unknown`] and fail
    /// closed even where the policy has declared what to do. This is that
    /// declaration. A map with no top node (see [`PermissionMaps::empty`])
    /// reports [`Jurisdiction::Unknown`].
    #[must_use]
    pub fn default_jurisdiction(&self) -> Jurisdiction {
        self.default_jurisdiction.clone().unwrap_or_default()
    }

    /// The jurisdiction that applies to `country` and `region`.
    ///
    /// Walks the tree the same way [`rules_or_default`](Self::rules_or_default)
    /// does: the region when it is listed, otherwise the country, otherwise the
    /// top node. Inheritance is settled when the file is parsed, so every
    /// listed place already carries the jurisdiction it inherits.
    #[must_use]
    pub fn jurisdiction_for(&self, country: Option<&str>, region: Option<&str>) -> Jurisdiction {
        if let (Some(country), Some(region)) = (country, region)
            && let Some(jurisdiction) = self
                .jurisdiction_by_region
                .get(&region_key(country, region))
        {
            return jurisdiction.clone();
        }
        if let Some(jurisdiction) = country
            .map(str::to_ascii_uppercase)
            .and_then(|code| self.jurisdiction_by_country.get(&code))
        {
            return jurisdiction.clone();
        }
        self.default_jurisdiction()
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

    /// Builds the maps from a `permissions.yaml` document: named `groups`, the
    /// `rules` tree of places, and the `signals` section that maps each session
    /// signal onto Data Uses.
    ///
    /// # Errors
    ///
    /// Returns [`PermissionsError`] when the YAML is malformed, the top of the
    /// `rules` tree omits `group` or `jurisdiction`, a node below the top is
    /// neither a group name nor a block carrying `group`, a node names an
    /// unknown group, permission, or acquisition flag, two sibling place codes
    /// name the same place, or a signal's revoke list names an unknown Data
    /// Use.
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
        build_rules_tree(&mut maps, &groups, &file.rules)?;
        maps.signals = build_signal_policy(&file.signals)?;
        Ok(maps)
    }

    /// Returns the rules written for `country` and `region` exactly, preferring
    /// a region entry, then the country entry, or `None` when neither is listed.
    ///
    /// This is the literal tree lookup with no fallback to the top node. Use
    /// [`rules_or_default`](Self::rules_or_default) for the resolution a
    /// request actually gets.
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

    /// The rules a request resolves to: its region, else its country, else the
    /// top node of the tree.
    ///
    /// Returns `None` only for a map with no top node, which the caller treats
    /// as the requires-signal floor. Every map parsed from a `permissions.yaml`
    /// has one, because the top node's `group` is required, so this is
    /// unreachable in a deployment and exists for maps built by hand in tests.
    pub(crate) fn rules_or_default(
        &self,
        country: Option<&str>,
        region: Option<&str>,
    ) -> Option<&CountryRules> {
        self.rules_for(country, region)
            .or(self.default_rules.as_ref())
    }

    /// Resolves the permission state for a request: the place baseline
    /// augmented by a session signal.
    ///
    /// `country` and `region` are what a geo provider returns (`region` may be
    /// `None`). Whatever the tree does not answer falls back to the node above,
    /// ending at the top node, so an unlisted country and a request with no
    /// country at all both resolve to the top node's group. `signal` maps each
    /// permission to a [`ConsentSignal`]; the caller derives it from its consent
    /// model so this module stays independent of how a signal is decoded. A
    /// `Granted` baseline is set unless the signal is `Revoke`, a
    /// `RequiresSignal` baseline is set only on `Grant`, and `Denied` is never
    /// set.
    #[must_use]
    pub fn resolve_with(
        &self,
        country: Option<&str>,
        region: Option<&str>,
        signal: impl Fn(Permission) -> ConsentSignal,
    ) -> PermissionState {
        Self::resolve_rules(self.rules_or_default(country, region), signal)
    }

    /// Resolves every permission at the requires-signal floor, whatever the
    /// policy tree says.
    ///
    /// This is the state for a geo provider that reported an outright lookup
    /// failure. The request's place is unknown in a way the policy's top node
    /// must not paper over, so nothing is set unless the session's signals
    /// grant it.
    #[must_use]
    pub fn floor_with(signal: impl Fn(Permission) -> ConsentSignal) -> PermissionState {
        Self::resolve_rules(None, signal)
    }

    /// Applies `signal` against `rules`, or against the requires-signal floor
    /// when no rules resolved.
    fn resolve_rules(
        rules: Option<&CountryRules>,
        signal: impl Fn(Permission) -> ConsentSignal,
    ) -> PermissionState {
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
    /// `Granted` permissions for the place (or the top node it falls back to),
    /// and is what a request resolves to when no signal is present.
    #[must_use]
    pub fn baseline(&self, country: Option<&str>, region: Option<&str>) -> PermissionState {
        self.resolve_with(country, region, |_| ConsentSignal::Neutral)
    }

    /// Convenience over [`resolve_with`](Self::resolve_with) for a boolean
    /// signal with no region and no revocation: a `true` grants the permission
    /// and a `false` is neutral.
    #[must_use]
    pub fn resolve(
        &self,
        country: Option<&str>,
        signal: impl Fn(Permission) -> bool,
    ) -> PermissionState {
        self.resolve_with(country, None, |permission| {
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

    /// The resolved state as the JSON the page receives in
    /// `window.tsjs.permissions`.
    ///
    /// Names are the [`Permission::as_str`] Data Use identifiers, sorted so the
    /// same state always serializes to the same bytes whatever order the set
    /// was built in. An empty state
    /// renders as `{"set":[]}`, which is an answer (nothing is set) rather than
    /// a missing value, so page code never has to tell the two apart.
    ///
    /// This is the only place the page shape is spelled, so no caller writes
    /// the JSON by hand.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::permissions::{
    ///     Permission, PermissionSet, PermissionState,
    /// };
    ///
    /// let state = PermissionState::new(
    ///     PermissionSet::none().with(Permission::StoreOnDevice),
    /// );
    /// assert_eq!(
    ///     state.page_json(),
    ///     r#"{"set":["necessary.operations.storage"]}"#
    /// );
    ///
    /// assert_eq!(PermissionState::default().page_json(), r#"{"set":[]}"#);
    /// ```
    #[must_use]
    pub fn page_json(&self) -> String {
        let mut names: Vec<&'static str> = self.set.iter().map(Permission::as_str).collect();
        names.sort_unstable();
        serde_json::json!({ "set": names }).to_string()
    }
}

// ---------------------------------------------------------------------------
// permissions.yaml parsing
// ---------------------------------------------------------------------------

/// The shape of a `permissions.yaml` document.
#[derive(Debug, Deserialize)]
struct RulesFile {
    /// Named permission baselines, keyed by group name. Each group is a flat map
    /// of per-permission flags, with an optional `default` shorthand for any
    /// permission it omits.
    #[serde(default)]
    groups: BTreeMap<String, BTreeMap<String, String>>,
    /// The `rules` tree of places. Walked by [`build_rules_tree`] rather than
    /// deserialized into a fixed shape, because every key below the reserved
    /// words is a place code chosen by the policy owner.
    #[serde(default)]
    rules: Value,
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
    /// Whether a present TCF record's grants and revokes apply.
    #[serde(default = "default_true")]
    authoritative: bool,
    /// TCF purpose number to the Data Use, or list of Data Uses, it grants
    /// (and revokes when the record does not consent to that purpose).
    #[serde(default)]
    purposes: BTreeMap<u8, DataUseList>,
}

/// One Data Use, or a list of Data Uses, granted by a single TCF purpose.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DataUseList {
    /// A single Data Use identifier.
    One(String),
    /// A list of Data Use identifiers.
    Many(Vec<String>),
}

impl DataUseList {
    /// The Data Use identifiers this value names, in written order.
    fn identifiers(&self) -> &[String] {
        match self {
            DataUseList::One(one) => core::slice::from_ref(one),
            DataUseList::Many(many) => many,
        }
    }
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

/// The reserved key naming a node's permission group.
const KEY_GROUP: &str = "group";

/// The reserved key naming the top node's jurisdiction.
const KEY_JURISDICTION: &str = "jurisdiction";

/// The reserved key holding a node's per-permission overrides.
const KEY_PERMISSIONS: &str = "permissions";

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

/// Applies a rule's per-permission acquisition overrides on top of its group's
/// rules. Each entry maps a Data Use to `granted`, `requires_signal`, or
/// `denied`, overriding the group's baseline for that Data Use, so any
/// acquisition (not just grant or deny) is expressible per rule.
fn apply_modifications(
    mut rules: CountryRules,
    modifications: &BTreeMap<String, String>,
) -> Result<CountryRules, PermissionsError> {
    for (name, value) in modifications {
        let permission = Permission::from_identifier(name).ok_or_else(|| {
            PermissionsError::UnknownPermission {
                name: name.to_owned(),
            }
        })?;
        rules = rules.with_rule(permission, parse_acquisition(value)?);
    }
    Ok(rules)
}

/// One node of the `rules` tree, resolved into its rules, the jurisdiction it
/// names (when it names one), and the child place codes written beside them.
struct RuleNode<'a> {
    /// The node's group, with any `permissions` overrides applied.
    rules: CountryRules,
    /// The `jurisdiction:` value written on this node, or `None` when the node
    /// inherits the one above it.
    jurisdiction: Option<&'a str>,
    /// The child place codes, in document order, each still unparsed.
    children: Vec<(&'a str, &'a Value)>,
}

/// Parses one node of the `rules` tree.
///
/// A node written as a plain string is the shorthand: the string is its group,
/// it names no jurisdiction, and it has no children. A node written as a block
/// must carry a `group:` line, may carry a `jurisdiction:` line and a
/// `permissions:` map of overrides, and may carry child place codes beside
/// them. `path` names the node for error messages (`"US"`, `"US/CA"`, or the
/// top of the tree).
fn parse_rule_node<'a>(
    groups: &BTreeMap<String, CountryRules>,
    path: &str,
    value: &'a Value,
) -> Result<RuleNode<'a>, PermissionsError> {
    match value {
        Value::String(name) => Ok(RuleNode {
            rules: resolve_group(groups, name)?,
            jurisdiction: None,
            children: Vec::new(),
        }),
        Value::Mapping(map) => {
            let group = map.get(KEY_GROUP).and_then(Value::as_str).ok_or_else(|| {
                PermissionsError::MissingGroup {
                    path: path.to_owned(),
                }
            })?;
            let mut rules = resolve_group(groups, group)?;
            if let Some(overrides) = map.get(KEY_PERMISSIONS) {
                let overrides: BTreeMap<String, String> =
                    serde_yaml_ng::from_value(overrides.clone()).map_err(|error| {
                        PermissionsError::Parse {
                            message: format!("`{KEY_PERMISSIONS}` under `{path}`: {error}"),
                        }
                    })?;
                rules = apply_modifications(rules, &overrides)?;
            }
            let jurisdiction =
                match map.get(KEY_JURISDICTION) {
                    Some(value) => Some(value.as_str().ok_or_else(|| {
                        PermissionsError::UnknownJurisdiction {
                            value: format!("{value:?}"),
                        }
                    })?),
                    None => None,
                };
            let mut children = Vec::new();
            for (key, child) in map {
                let key = key.as_str().ok_or_else(|| PermissionsError::InvalidRule {
                    path: path.to_owned(),
                })?;
                if key == KEY_GROUP || key == KEY_PERMISSIONS || key == KEY_JURISDICTION {
                    continue;
                }
                children.push((key, child));
            }
            Ok(RuleNode {
                rules,
                jurisdiction,
                children,
            })
        }
        _ => Err(PermissionsError::InvalidRule {
            path: path.to_owned(),
        }),
    }
}

/// Resolves a `jurisdiction:` value written on the node at `path`.
///
/// `region` is the node's own ISO 3166-2 code, or `None` for the top of the
/// tree and for a country. `us-state` carries no code of its own, because the
/// node naming it is the state, so it is rejected wherever there is no region
/// to name.
fn parse_jurisdiction(
    path: &str,
    region: Option<&str>,
    value: &str,
) -> Result<Jurisdiction, PermissionsError> {
    if value == "us-state" && region.is_none() {
        return Err(PermissionsError::MisplacedUsState {
            path: path.to_owned(),
        });
    }
    Jurisdiction::from_policy_name(value, region).ok_or_else(|| {
        PermissionsError::UnknownJurisdiction {
            value: value.to_owned(),
        }
    })
}

/// Walks the `rules` tree into `maps`: the top node, then the country nodes
/// under it, then the region nodes under each country.
///
/// Each node's group and jurisdiction are settled here, so a node that names
/// neither is stored carrying what it inherits from the node above it. The top
/// node must name both, which is what makes inheritance always terminate.
///
/// The tree is three levels deep, because a geo provider returns a country and
/// a region and nothing finer, so a place written under a region is rejected
/// rather than silently ignored.
fn build_rules_tree(
    maps: &mut PermissionMaps,
    groups: &BTreeMap<String, CountryRules>,
    rules: &Value,
) -> Result<(), PermissionsError> {
    const TOP: &str = "the top of the tree";

    if !matches!(rules, Value::Mapping(_)) {
        return Err(PermissionsError::MissingGroup {
            path: TOP.to_owned(),
        });
    }
    let top = parse_rule_node(groups, TOP, rules)?;
    let name = top
        .jurisdiction
        .ok_or(PermissionsError::MissingJurisdiction)?;
    let top_jurisdiction = parse_jurisdiction(TOP, None, name)?;
    maps.default_rules = Some(top.rules);
    maps.default_jurisdiction = Some(top_jurisdiction.clone());

    // Place codes are matched without regard to case at lookup, so two
    // spellings of one place would silently overwrite each other. Reject the
    // collision instead, among the siblings at each level.
    let mut seen_countries: BTreeMap<String, &str> = BTreeMap::new();
    for (country, node) in top.children {
        check_duplicate(&mut seen_countries, country)?;
        let node = parse_rule_node(groups, country, node)?;
        let jurisdiction = match node.jurisdiction {
            Some(name) => parse_jurisdiction(country, None, name)?,
            None => top_jurisdiction.clone(),
        };
        let code = country.to_ascii_uppercase();
        maps.by_country.insert(code.clone(), node.rules);
        maps.jurisdiction_by_country
            .insert(code, jurisdiction.clone());

        let mut seen_regions: BTreeMap<String, &str> = BTreeMap::new();
        for (region, child) in node.children {
            let path = format!("{country}/{region}");
            check_duplicate(&mut seen_regions, region)?;
            let child = parse_rule_node(groups, &path, child)?;
            if !child.children.is_empty() {
                return Err(PermissionsError::NestedTooDeep { path });
            }
            let child_jurisdiction = match child.jurisdiction {
                Some(name) => parse_jurisdiction(&path, Some(region), name)?,
                None => jurisdiction.clone(),
            };
            let key = region_key(country, region);
            maps.by_region.insert(key.clone(), child.rules);
            maps.jurisdiction_by_region.insert(key, child_jurisdiction);
        }
    }
    Ok(())
}

/// Records a place code among its siblings, erroring when another spelling of
/// the same code is already there.
fn check_duplicate<'a>(
    seen: &mut BTreeMap<String, &'a str>,
    key: &'a str,
) -> Result<(), PermissionsError> {
    if let Some(first) = seen.insert(key.to_ascii_uppercase(), key) {
        return Err(PermissionsError::DuplicateRule {
            first: first.to_owned(),
            second: key.to_owned(),
        });
    }
    Ok(())
}

/// Parses the compiled `permissions.yaml`, so startup validation can reject a
/// malformed policy with a configuration error instead of panicking on the
/// first lookup.
///
/// # Errors
///
/// Returns the same [`PermissionsError`] values [`PermissionMaps::from_yaml`]
/// does, most usefully a top node that omits `group` or `jurisdiction`.
pub fn validate_default_policy() -> Result<(), PermissionsError> {
    PermissionMaps::from_yaml(DEFAULT_PERMISSION_RULES).map(|_| ())
}

/// An error parsing a `permissions.yaml` document.
#[derive(Debug, derive_more::Display)]
pub enum PermissionsError {
    /// Two sibling place codes are the same place spelled differently.
    #[display("place codes `{first}` and `{second}` name the same place, so keep one")]
    DuplicateRule { first: String, second: String },
    /// A node of the `rules` tree carried no `group`.
    #[display(
        "the rule for `{path}` needs a `group:` line (or write the group name on its own, as `GB: gdpr-uk`)"
    )]
    MissingGroup { path: String },
    /// The top of the `rules` tree carried no `jurisdiction`.
    #[display(
        "the top of the `rules` tree needs a `jurisdiction:` line, naming the consent handling for a visitor whose place cannot be resolved"
    )]
    MissingJurisdiction,
    /// A node with no region of its own named `us-state`.
    #[display(
        "the rule for `{path}` names `jurisdiction: us-state`, which only a region may name, because the region is the state"
    )]
    MisplacedUsState { path: String },
    /// A `jurisdiction` value named something the consent model cannot
    /// represent.
    #[display(
        "unknown jurisdiction `{value}` (expected gdpr, us-state, non-regulated, or unknown)"
    )]
    UnknownJurisdiction { value: String },
    /// A node of the `rules` tree was neither a group name nor a block.
    #[display("the rule for `{path}` must be a group name or a block with a `group:` line")]
    InvalidRule { path: String },
    /// A place was written under a region, deeper than a geo provider resolves.
    #[display(
        "the rule for `{path}` has places written under it; the tree stops at a region, because that is the finest place a geo provider returns"
    )]
    NestedTooDeep { path: String },
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
    /// A Data Use appeared under more than one TCF purpose in `signals.tcf`.
    #[display(
        "Data Use `{name}` is granted by more than one TCF purpose; map each Data Use to a single purpose"
    )]
    DuplicateTcfDataUse { name: String },
    /// An acquisition rule was not `granted`, `requires_signal`, or `denied`.
    #[display("unknown acquisition rule `{value}` (expected granted, requires_signal, or denied)")]
    UnknownAcquisition { value: String },
    /// A `signals` opt-out `revokes` value was neither `all` nor a list.
    #[display("unknown revoke rule `{value}` (expected `all` or a list of Data Uses)")]
    UnknownRevoke { value: String },
}

impl core::error::Error for PermissionsError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

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
    fn page_json_lists_set_permissions_sorted_by_name() {
        // Arrange: a state whose bit-index order is the reverse of its name
        // order, so the sort is what the assertion sees.
        let state = PermissionState::new(
            PermissionSet::none()
                .with(Permission::StoreOnDevice)
                .with(Permission::SelectBasicAds),
        );

        // Act
        let json = state.page_json();

        // Assert
        assert_eq!(
            json,
            json!({
                "set": [
                    "advertising_marketing.first_party.contextual",
                    "necessary.operations.storage",
                ]
            })
            .to_string(),
            "should list every set permission by Data Use name, sorted"
        );
    }

    #[test]
    fn page_json_of_an_empty_state_is_an_empty_set() {
        // Arrange
        let state = PermissionState::default();

        // Act
        let json = state.page_json();

        // Assert
        assert_eq!(
            json,
            json!({ "set": [] }).to_string(),
            "an empty state should render as an empty set, not as nothing"
        );
    }

    #[test]
    fn the_floor_sets_a_permission_only_when_a_signal_grants_it() {
        // Empty maps and no top node: every permission is the requires-signal
        // floor, set only when a signal grants it.
        let maps = PermissionMaps::default();

        let denied = maps.resolve(Some("GB"), |_| false);
        assert!(
            !denied.is_set(Permission::StoreOnDevice),
            "the floor should not set necessary.operations.storage without a signal"
        );

        let granted = maps.resolve(Some("GB"), |p| p == Permission::StoreOnDevice);
        assert!(
            granted.is_set(Permission::StoreOnDevice),
            "the floor should set necessary.operations.storage once a signal grants it"
        );
    }

    #[test]
    fn the_floor_ignores_the_top_node_even_where_one_grants() {
        // A policy whose top node grants everything, so the only way the floor
        // can leave the permission unset is by never consulting the tree. This
        // is the geo lookup failure case.
        let maps = PermissionMaps::empty()
            .with_default_rules(CountryRules::with_default(Acquisition::Granted));
        assert!(
            maps.baseline(None, None).is_set(Permission::StoreOnDevice),
            "the top node grants storage, or this test proves nothing"
        );
        assert!(
            !PermissionMaps::floor_with(|_| ConsentSignal::Neutral)
                .is_set(Permission::StoreOnDevice),
            "the floor must not fall back to the top node"
        );
    }

    #[test]
    fn an_unlisted_country_uses_the_top_node() {
        // A map whose top node grants, so an unlisted country is answered by
        // it rather than by the floor.
        let maps = PermissionMaps::empty()
            .with_default_rules(CountryRules::with_default(Acquisition::Granted));
        assert!(
            maps.resolve(Some("ZZ"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "an unlisted country should resolve at the top node"
        );
        // No country at all resolves the same way.
        assert!(
            maps.resolve(None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "no country should resolve at the top node too"
        );
        // With no top node the requires-signal floor sets nothing.
        assert!(
            !PermissionMaps::empty()
                .resolve(None, |_| false)
                .is_set(Permission::StoreOnDevice),
            "with no top node, an unlisted country sets nothing without a signal"
        );
    }

    #[test]
    fn a_matching_country_is_used_over_the_top_node() {
        // US grants; the top node requires a signal.
        let maps = PermissionMaps::empty()
            .with_country("us", CountryRules::with_default(Acquisition::Granted))
            .with_default_rules(CountryRules::with_default(Acquisition::RequiresSignal));
        assert!(
            maps.resolve(Some("US"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "a country with its own rule uses it, not the top node"
        );
        assert!(
            !maps
                .resolve(Some("ZZ"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "an unlisted country falls back to the top node"
        );
    }

    #[test]
    fn per_permission_override_beats_the_country_default() {
        // Granted by default, but deny necessary.operations.storage specifically.
        let rules = CountryRules::with_default(Acquisition::Granted)
            .with_rule(Permission::StoreOnDevice, Acquisition::Denied);
        let maps = PermissionMaps::empty().with_country("zz", rules);
        let state = maps.resolve(Some("ZZ"), |_| true);

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
                .resolve(Some("ZZ"), |_| false)
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
                .resolve(Some("ZZ"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "a requires-signal default should not be set without a signal"
        );
        assert!(
            opt_in
                .resolve(Some("ZZ"), |p| p == Permission::StoreOnDevice)
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
                .resolve(Some("ZZ"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "a Granted rule is set with no signal"
        );

        // A Denied rule is never set, even when every signal grants.
        let denied = CountryRules::with_default(Acquisition::Granted)
            .with_rule(Permission::StoreOnDevice, Acquisition::Denied);
        assert!(
            !PermissionMaps::empty()
                .with_country("zz", denied)
                .resolve(Some("ZZ"), |_| true)
                .is_set(Permission::StoreOnDevice),
            "a Denied rule is never set even with a signal"
        );
    }

    #[test]
    fn standard_maps_eu_requires_signal_and_uk_grants_storage() {
        let maps = PermissionMaps::standard();
        assert!(
            !maps
                .resolve(Some("DE"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "an EU country should not set necessary.operations.storage without a signal"
        );
        assert!(
            maps.resolve(Some("DE"), |p| p == Permission::StoreOnDevice)
                .is_set(Permission::StoreOnDevice),
            "an EU country should set necessary.operations.storage once a signal grants it"
        );
        assert!(
            maps.resolve(Some("GB"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "the UK should grant necessary.operations.storage without a signal"
        );
    }

    #[test]
    fn standard_maps_us_and_australia_grant_storage_by_default() {
        let maps = PermissionMaps::standard();
        for code in ["US", "AU"] {
            assert!(
                maps.resolve(Some(code), |_| false)
                    .is_set(Permission::StoreOnDevice),
                "{code} should grant necessary.operations.storage by default"
            );
        }
        // No default configured, so an unmapped country hits the requires-signal
        // floor and sets nothing without a signal.
        assert!(
            !maps
                .resolve(Some("ZZ"), |_| false)
                .is_set(Permission::StoreOnDevice),
            "an unmapped country with no default sets nothing without a signal"
        );
    }

    #[test]
    fn resolve_with_revokes_a_granted_permission_on_opt_out() {
        // US grants necessary.operations.storage by default; an opt-out signal revokes it.
        let maps = PermissionMaps::standard();
        assert!(
            maps.baseline(Some("US"), None)
                .is_set(Permission::StoreOnDevice),
            "the US baseline should set necessary.operations.storage"
        );
        let revoked = maps.resolve_with(Some("US"), None, |p| {
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
                .baseline(Some("US"), Some("CA"))
                .is_set(Permission::StoreOnDevice),
            "the CA region rule should require a signal, overriding the US baseline"
        );
        assert!(
            maps.baseline(Some("US"), Some("NY"))
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
  group: eu
  jurisdiction: gdpr
  FR: eu
  US:
    group: us
    jurisdiction: non-regulated
    CA:
      group: eu
      jurisdiction: us-state
      permissions:
        necessary.operations.storage: granted
        advertising_marketing.first_party.contextual: denied
"#;
        let maps = PermissionMaps::from_yaml(yaml).expect("should parse the rules");

        // Bare group references.
        assert!(
            !maps
                .baseline(Some("FR"), None)
                .is_set(Permission::StoreOnDevice),
            "FR (eu) requires a signal for device storage"
        );
        assert!(
            maps.baseline(Some("US"), None)
                .is_set(Permission::StoreOnDevice),
            "US (us) grants device storage"
        );

        // CA references the eu group, but its permissions map grants
        // necessary.operations.storage, overriding the eu baseline.
        assert!(
            maps.baseline(Some("US"), Some("CA"))
                .is_set(Permission::StoreOnDevice),
            "the permissions map grants necessary.operations.storage for CA, overriding the eu baseline"
        );
        // The permissions map denies advertising_marketing.first_party.contextual,
        // so it is not set even when a signal grants it.
        assert!(
            !maps
                .resolve_with(Some("US"), Some("CA"), |_| ConsentSignal::Grant)
                .is_set(Permission::SelectBasicAds),
            "the permissions map denies advertising_marketing.first_party.contextual even when a signal grants it"
        );

        // An unlisted country falls back to the top node (eu, requires signal).
        assert!(
            !maps
                .baseline(Some("ZZ"), None)
                .is_set(Permission::StoreOnDevice),
            "an unlisted country should use the top node"
        );
    }

    /// A two-group policy the tree tests below vary, with `g` granting
    /// everything and `strict` requiring a signal for everything.
    const TEST_GROUPS: &str = "groups:\n  g:\n    default: granted\n  \
                               strict:\n    default: requires_signal\n";

    #[test]
    fn a_shorthand_node_is_a_group_with_nothing_below_it() {
        let yaml = format!(
            "{TEST_GROUPS}\
rules:
  group: strict
  jurisdiction: gdpr
  GB: g
"
        );
        let maps = PermissionMaps::from_yaml(&yaml).expect("a shorthand node should parse");
        assert_eq!(
            maps.rules_for(Some("GB"), None)
                .expect("GB should have a rule")
                .rule_for(Permission::StoreOnDevice),
            Acquisition::Granted,
            "the shorthand string should name the node's group"
        );
        assert_eq!(
            maps.jurisdiction_for(Some("GB"), None),
            Jurisdiction::Gdpr,
            "a shorthand node should inherit the top node's jurisdiction"
        );
    }

    #[test]
    fn a_block_node_carries_children_beside_its_group() {
        let yaml = format!(
            "{TEST_GROUPS}\
rules:
  group: g
  jurisdiction: gdpr
  US:
    group: g
    jurisdiction: non-regulated
    CA:
      group: strict
      jurisdiction: us-state
"
        );
        let maps = PermissionMaps::from_yaml(&yaml).expect("a block node should parse");
        assert_eq!(
            maps.rules_for(Some("US"), Some("CA"))
                .expect("US/CA should have a rule")
                .rule_for(Permission::StoreOnDevice),
            Acquisition::RequiresSignal,
            "the region child should carry its own group"
        );
        assert_eq!(
            maps.rules_or_default(Some("US"), Some("NV"))
                .expect("an unlisted region should fall back to its country")
                .rule_for(Permission::StoreOnDevice),
            Acquisition::Granted,
            "an unlisted region should resolve at its country node"
        );
    }

    #[test]
    fn from_yaml_rejects_a_block_without_a_group() {
        let yaml = format!(
            "{TEST_GROUPS}\
rules:
  group: g
  jurisdiction: gdpr
  US:
    jurisdiction: non-regulated
"
        );
        let err =
            PermissionMaps::from_yaml(&yaml).expect_err("a block with no group should be rejected");
        assert!(
            matches!(&err, PermissionsError::MissingGroup { path } if path == "US"),
            "should report the missing group for US, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_rejects_a_top_node_without_a_group() {
        let yaml = format!("{TEST_GROUPS}rules:\n  jurisdiction: gdpr\n  US: g\n");
        let err = PermissionMaps::from_yaml(&yaml)
            .expect_err("a top node with no group should be rejected");
        assert!(
            matches!(err, PermissionsError::MissingGroup { .. }),
            "should report the missing top group, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_rejects_a_top_node_without_a_jurisdiction() {
        let yaml = format!("{TEST_GROUPS}rules:\n  group: g\n  US: g\n");
        let err = PermissionMaps::from_yaml(&yaml)
            .expect_err("a top node with no jurisdiction should be rejected");
        assert!(
            matches!(err, PermissionsError::MissingJurisdiction),
            "should report the missing top jurisdiction, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_rejects_an_unknown_jurisdiction() {
        let yaml = format!("{TEST_GROUPS}rules:\n  group: g\n  jurisdiction: ccpa\n");
        let err = PermissionMaps::from_yaml(&yaml)
            .expect_err("an unknown jurisdiction should be rejected");
        assert!(
            matches!(err, PermissionsError::UnknownJurisdiction { .. }),
            "should report the unknown jurisdiction, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_rejects_us_state_above_a_region() {
        // `us-state` names no code of its own, so only a region can carry it.
        let top = format!("{TEST_GROUPS}rules:\n  group: g\n  jurisdiction: us-state\n");
        assert!(
            matches!(
                PermissionMaps::from_yaml(&top)
                    .expect_err("us-state at the top should be rejected"),
                PermissionsError::MisplacedUsState { .. }
            ),
            "the top of the tree names no state"
        );
        let country = format!(
            "{TEST_GROUPS}\
rules:
  group: g
  jurisdiction: gdpr
  US:
    group: g
    jurisdiction: us-state
"
        );
        assert!(
            matches!(
                PermissionMaps::from_yaml(&country)
                    .expect_err("us-state on a country should be rejected"),
                PermissionsError::MisplacedUsState { .. }
            ),
            "a country names no state"
        );
    }

    #[test]
    fn from_yaml_rejects_a_place_written_under_a_region() {
        let yaml = format!(
            "{TEST_GROUPS}\
rules:
  group: g
  jurisdiction: gdpr
  US:
    group: g
    CA:
      group: g
      LA:
        group: g
"
        );
        let err = PermissionMaps::from_yaml(&yaml).expect_err("a fourth level should be rejected");
        assert!(
            matches!(err, PermissionsError::NestedTooDeep { .. }),
            "should report the tree being too deep, got {err:?}"
        );
    }

    #[test]
    fn a_jurisdiction_is_inherited_and_can_be_overridden_at_each_level() {
        let yaml = format!(
            "{TEST_GROUPS}\
rules:
  group: g
  jurisdiction: gdpr
  GB: g
  US:
    group: g
    jurisdiction: non-regulated
    CA:
      group: g
      jurisdiction: us-state
    NY: g
"
        );
        let maps = PermissionMaps::from_yaml(&yaml).expect("should parse the tree");
        assert_eq!(
            maps.jurisdiction_for(Some("GB"), None),
            Jurisdiction::Gdpr,
            "a country naming none should inherit the top node"
        );
        assert_eq!(
            maps.jurisdiction_for(Some("US"), None),
            Jurisdiction::NonRegulated,
            "a country naming its own should override the top node"
        );
        assert_eq!(
            maps.jurisdiction_for(Some("US"), Some("NY")),
            Jurisdiction::NonRegulated,
            "a region naming none should inherit its country"
        );
        assert_eq!(
            maps.jurisdiction_for(Some("US"), Some("CA")),
            Jurisdiction::UsState("CA".to_owned()),
            "a region naming us-state should name itself as the state"
        );
        assert_eq!(
            maps.jurisdiction_for(Some("ZZ"), None),
            Jurisdiction::Gdpr,
            "an unlisted country should inherit the top node"
        );
        assert_eq!(
            maps.default_jurisdiction(),
            Jurisdiction::Gdpr,
            "no place at all should resolve the top node"
        );
    }

    #[test]
    fn place_codes_are_matched_without_regard_to_case() {
        let yaml = format!(
            "{TEST_GROUPS}\
rules:
  group: strict
  jurisdiction: gdpr
  us:
    group: strict
    jurisdiction: non-regulated
    ca:
      group: g
      jurisdiction: us-state
"
        );
        let maps = PermissionMaps::from_yaml(&yaml).expect("should parse lower-case place codes");
        assert!(
            maps.baseline(Some("US"), Some("CA"))
                .is_set(Permission::StoreOnDevice),
            "an upper-case request should match a lower-case rule"
        );
        assert_eq!(
            maps.jurisdiction_for(Some("US"), Some("CA")),
            Jurisdiction::UsState("CA".to_owned()),
            "the state code should be upper-cased whatever case the file uses"
        );
    }

    #[test]
    fn the_settled_example_resolves_as_documented() {
        // The example in the permissions.yaml header, kept here so the file's
        // own teaching example is proven rather than asserted.
        let yaml = r#"
groups:
  gdpr-eu:
    default: requires_signal
  gdpr-uk:
    default: requires_signal
  us-notice:
    default: granted
  us-opt-out:
    default: granted
rules:
  group: gdpr-eu
  jurisdiction: gdpr
  GB: gdpr-uk
  US:
    group: us-notice
    jurisdiction: non-regulated
    CA:
      group: us-opt-out
      jurisdiction: us-state
    NY: us-notice
"#;
        let maps = PermissionMaps::from_yaml(yaml).expect("the header example should parse");
        for (country, region, jurisdiction) in [
            (Some("GB"), None, Jurisdiction::Gdpr),
            (Some("US"), None, Jurisdiction::NonRegulated),
            (Some("US"), Some("NY"), Jurisdiction::NonRegulated),
            (
                Some("US"),
                Some("CA"),
                Jurisdiction::UsState("CA".to_owned()),
            ),
            (Some("JP"), None, Jurisdiction::Gdpr),
            (None, None, Jurisdiction::Gdpr),
        ] {
            assert_eq!(
                maps.jurisdiction_for(country, region),
                jurisdiction,
                "{country:?}/{region:?} should resolve as the header documents"
            );
        }
        assert!(
            !maps
                .baseline(Some("GB"), None)
                .is_set(Permission::StoreOnDevice),
            "GB takes the gdpr-uk group, which requires a signal in this example"
        );
        assert!(
            maps.baseline(Some("US"), Some("NY"))
                .is_set(Permission::StoreOnDevice),
            "NY takes us-notice, which grants"
        );
    }

    #[test]
    fn the_shipped_policy_reproduces_the_retired_consent_lists() {
        // The 31 GDPR countries and the 20 US privacy states the consent
        // configuration used to carry as compiled defaults now live in the
        // policy tree, so the shipped file must resolve each the same way.
        let maps = PermissionMaps::standard();
        let gdpr_countries = [
            "AT", "BE", "BG", "HR", "CY", "CZ", "DK", "EE", "FI", "FR", "DE", "GR", "HU", "IE",
            "IT", "LV", "LT", "LU", "MT", "NL", "PL", "PT", "RO", "SK", "SI", "ES", "SE", "IS",
            "LI", "NO", "GB",
        ];
        assert_eq!(
            gdpr_countries.len(),
            31,
            "the retired list held 27 EU, 3 EEA and the UK"
        );
        for country in gdpr_countries {
            assert_eq!(
                maps.jurisdiction_for(Some(country), None),
                Jurisdiction::Gdpr,
                "`{country}` should resolve the GDPR jurisdiction"
            );
        }

        let privacy_states = [
            "CA", "VA", "CO", "CT", "UT", "MT", "OR", "TX", "FL", "DE", "IA", "NE", "NH", "NJ",
            "TN", "MN", "MD", "IN", "KY", "RI",
        ];
        assert_eq!(
            privacy_states.len(),
            20,
            "the retired list held 20 states with a comprehensive privacy law"
        );
        for state in privacy_states {
            assert_eq!(
                maps.jurisdiction_for(Some("US"), Some(state)),
                Jurisdiction::UsState(state.to_owned()),
                "`US/{state}` should resolve its own state jurisdiction"
            );
        }

        // A state with no law of its own inherits the US node, and the US with
        // no region does the same.
        for region in [Some("WY"), Some("NY"), None] {
            assert_eq!(
                maps.jurisdiction_for(Some("US"), region),
                Jurisdiction::NonRegulated,
                "`US/{region:?}` should inherit the country node"
            );
        }
        assert_eq!(
            maps.jurisdiction_for(Some("AU"), None),
            Jurisdiction::NonRegulated,
            "Australia should carry its own non-regulated jurisdiction"
        );
        assert_eq!(
            maps.default_jurisdiction(),
            Jurisdiction::Gdpr,
            "an unresolved place should take the top node's GDPR jurisdiction"
        );
    }

    #[test]
    fn a_us_state_keeps_the_country_permission_baseline() {
        // Each listed state carries the same group as the country node, so
        // adding the states for the jurisdiction changed no permission.
        let maps = PermissionMaps::standard();
        for state in ["CA", "TX", "DE", "RI", "WY"] {
            assert!(
                maps.baseline(Some("US"), Some(state))
                    .is_set(Permission::StoreOnDevice),
                "`US/{state}` should keep the US opt-out storage baseline"
            );
        }
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
        let yaml = format!(
            "{group}rules:\n  group: everything\n  jurisdiction: unknown\n  US: everything\n"
        );
        let maps = PermissionMaps::from_yaml(&yaml).expect("an explicit group should parse");
        assert!(
            maps.baseline(Some("US"), None)
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
    fn from_yaml_rejects_a_non_acquisition_override_value() {
        let yaml = "groups:\n  g:\n    default: granted\nrules:\n  group: g\n  jurisdiction: unknown\n  US:\n    group: g\n    permissions:\n      necessary.operations.storage: enabled\n";
        let err = PermissionMaps::from_yaml(yaml)
            .expect_err("an unknown acquisition value should be rejected");
        assert!(
            matches!(err, PermissionsError::UnknownAcquisition { .. }),
            "should report an unknown acquisition, got {err:?}"
        );
    }

    #[test]
    fn from_yaml_rejects_duplicate_rule_keys_differing_only_by_case() {
        let yaml = "groups:\n  g:\n    default: granted\nrules:\n  group: g\n  jurisdiction: unknown\n  us: g\n  US: g\n";
        let err = PermissionMaps::from_yaml(yaml)
            .expect_err("two spellings of one country should be rejected");
        assert!(
            matches!(err, PermissionsError::DuplicateRule { .. }),
            "should report the duplicate rule, got {err:?}"
        );
    }

    #[test]
    fn a_detailed_rule_can_set_requires_signal_per_permission() {
        let yaml = "groups:\n  g:\n    default: granted\nrules:\n  group: g\n  jurisdiction: unknown\n  US:\n    group: g\n    permissions:\n      necessary.operations.storage: requires_signal\n";
        let maps = PermissionMaps::from_yaml(yaml).expect("should parse the override map");
        let rules = maps
            .rules_for(Some("US"), None)
            .expect("should resolve the US rule");
        assert_eq!(
            rules.rule_for(Permission::StoreOnDevice),
            Acquisition::RequiresSignal,
            "the per-permission map should express requires_signal"
        );
        assert_eq!(
            rules.rule_for(Permission::SelectPersonalisedAds),
            Acquisition::Granted,
            "an unlisted permission should keep the group default"
        );
    }

    #[test]
    fn every_eu_and_eea_member_requires_a_signal_for_storage() {
        // The shipped permissions.yaml must cover all 27 EU member states and
        // the three EEA members, each resolving storage as requires-signal, so
        // no member state silently falls to the top of the tree.
        let maps = PermissionMaps::standard();
        for country in [
            "AT", "BE", "BG", "HR", "CY", "CZ", "DK", "EE", "FI", "FR", "DE", "GR", "HU", "IE",
            "IT", "LV", "LT", "LU", "MT", "NL", "PL", "PT", "RO", "SK", "SI", "ES", "SE", "IS",
            "LI", "NO",
        ] {
            let rules = maps
                .rules_for(Some(country), None)
                .unwrap_or_else(|| panic!("`{country}` should have a rule"));
            assert_eq!(
                rules.rule_for(Permission::StoreOnDevice),
                Acquisition::RequiresSignal,
                "storage in `{country}` should require a signal"
            );
        }
    }

    #[test]
    fn from_yaml_parses_the_signals_section_into_a_policy() {
        let yaml = "\
groups:
  g:
    default: requires_signal
rules:
  group: g
  jurisdiction: gdpr
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
  group: g
  jurisdiction: gdpr
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
