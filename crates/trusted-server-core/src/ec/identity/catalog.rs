//! The enabled identity providers and source ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use error_stack::{Report, ResultExt as _};

use super::descriptor::ProviderDescriptor;
use super::generic_pull::{GenericPullProvider, generic_pull_id};
use super::ids::{IdentityProviderId, SourceDomain};
use super::provider::IdentityProvider;
use crate::error::TrustedServerError;
use crate::settings::{EcPartner, IdentitySettings, Settings};

/// Most providers one deployment may enable.
const MAX_PROVIDERS: usize = 32;

fn catalog_error(message: String) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration { message })
}

/// Validates the catalog rules of spec 10.2 against every provider
/// descriptor.
///
/// It rejects a duplicate provider ID, more than 32 providers, an
/// `[ec.identity.providers.<id>]` table that names no enabled provider, and
/// `revoke_scope` on a provider whose descriptor declares no `captures`. It
/// also checks source ownership, after each provider's `sources` narrowing: a
/// `sources` list must name declared sources and keep at least one, a
/// capture-only provider must list its `sources`, an `owner` must name an
/// enabled provider that is not capture-only and keeps the source, every
/// claimed source needs an `[[ec.partners]]` entry, and no second provider
/// may claim an owned source. It is the one place these rules live. It reads
/// descriptors only, so it runs on unresolved deploy-time settings.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] naming the first rule
/// that fails.
pub(crate) fn validate_identity_catalog(
    descriptors: &[ProviderDescriptor],
    partners: &[EcPartner],
    identity: &IdentitySettings,
) -> Result<(), Report<TrustedServerError>> {
    let mut ids = BTreeSet::new();
    for descriptor in descriptors {
        if !ids.insert(descriptor.id().as_str()) {
            return Err(catalog_error(format!(
                "identity provider `{}` is declared more than once",
                descriptor.id()
            )));
        }
    }
    if descriptors.len() > MAX_PROVIDERS {
        return Err(catalog_error(format!(
            "at most {MAX_PROVIDERS} identity providers may be enabled, got {}",
            descriptors.len()
        )));
    }

    for (id, settings) in &identity.providers {
        let Some(descriptor) = descriptors
            .iter()
            .find(|descriptor| descriptor.id().as_str() == id)
        else {
            let configured = if ids.is_empty() {
                "none".to_owned()
            } else {
                ids.iter().copied().collect::<Vec<_>>().join(", ")
            };
            let hint = partners
                .iter()
                .find(|partner| {
                    !partner.pull_sync_enabled
                        && generic_pull_id(&partner.source_domain).as_str() == id
                })
                .map_or_else(String::new, |partner| {
                    format!(
                        "; partner `{}` has pull_sync_enabled = false",
                        partner.source_domain
                    )
                });
            return Err(catalog_error(format!(
                "ec.identity.providers.{id} names no enabled identity provider{hint}; \
                 configured providers: {configured}"
            )));
        };
        if settings.revoke_scope.is_some() && descriptor.captures().is_none() {
            return Err(catalog_error(format!(
                "ec.identity.providers.{id}.revoke_scope is set, but provider `{id}` declares no captures"
            )));
        }
    }

    validate_source_ownership(descriptors, partners, identity)
}

/// Whether `descriptor` captures and never resolves.
fn is_capture_only(descriptor: &ProviderDescriptor) -> bool {
    descriptor.captures().is_some() && descriptor.resolves().is_none()
}

/// Each descriptor's sources after its `sources` narrowing, rejecting a list
/// that names an undeclared source or keeps none, and a capture-only
/// provider that has no list.
fn narrowed_sources<'a>(
    descriptors: &'a [ProviderDescriptor],
    identity: &IdentitySettings,
) -> Result<BTreeMap<&'a str, Vec<SourceDomain>>, Report<TrustedServerError>> {
    let mut narrowed = BTreeMap::new();
    for descriptor in descriptors {
        let id = descriptor.id().as_str();
        let listed = identity
            .providers
            .get(id)
            .and_then(|table| table.sources.as_ref());
        let Some(listed) = listed else {
            if is_capture_only(descriptor) {
                return Err(catalog_error(format!(
                    "provider `{id}` is capture-only, so ec.identity.providers.{id}.sources must \
                     list the sources it is the delivery path for"
                )));
            }
            narrowed.insert(id, descriptor.sources().to_vec());
            continue;
        };
        let mut kept = Vec::with_capacity(listed.len());
        for name in listed {
            let declared = name
                .parse::<SourceDomain>()
                .ok()
                .filter(|source| descriptor.sources().contains(source));
            let Some(source) = declared else {
                return Err(catalog_error(format!(
                    "ec.identity.providers.{id}.sources names `{name}`, which provider `{id}` does not declare"
                )));
            };
            kept.push(source);
        }
        if kept.is_empty() {
            return Err(catalog_error(format!(
                "ec.identity.providers.{id}.sources narrows provider `{id}` to no source"
            )));
        }
        narrowed.insert(id, kept);
    }
    Ok(narrowed)
}

/// The ownership rules of spec 10.2, on narrowed sources.
fn validate_source_ownership(
    descriptors: &[ProviderDescriptor],
    partners: &[EcPartner],
    identity: &IdentitySettings,
) -> Result<(), Report<TrustedServerError>> {
    let narrowed = narrowed_sources(descriptors, identity)?;

    // A partner whose source does not parse is reported by partner validation.
    let partner_sources: BTreeMap<SourceDomain, Option<&str>> = partners
        .iter()
        .filter_map(|partner| {
            Some((
                partner.source_domain.parse().ok()?,
                partner.owner.as_deref(),
            ))
        })
        .collect();

    for descriptor in descriptors {
        let id = descriptor.id().as_str();
        for source in &narrowed[id] {
            if !partner_sources.contains_key(source) {
                return Err(catalog_error(format!(
                    "identity provider `{id}` claims source `{source}`, which has no [[ec.partners]] entry"
                )));
            }
        }
    }

    for (source, owner) in &partner_sources {
        let Some(owner) = owner else {
            continue;
        };
        let Some(descriptor) = descriptors
            .iter()
            .find(|descriptor| descriptor.id().as_str() == *owner)
        else {
            return Err(catalog_error(format!(
                "ec.partners source `{source}` names owner `{owner}`, which is not an enabled identity provider"
            )));
        };
        if is_capture_only(descriptor) {
            return Err(catalog_error(format!(
                "ec.partners source `{source}` names owner `{owner}`, which is capture-only; \
                 a capture-only provider stores no positive ID and cannot own a source"
            )));
        }
        if !narrowed[*owner].contains(source) {
            return Err(catalog_error(format!(
                "ec.partners source `{source}` names owner `{owner}`, which does not claim that source"
            )));
        }
        let other = descriptors
            .iter()
            .map(|other| other.id().as_str())
            .find(|other| other != owner && narrowed[*other].contains(source));
        if let Some(other) = other {
            return Err(catalog_error(format!(
                "identity provider `{other}` claims source `{source}`, which `{owner}` owns; \
                 use ec.identity.providers.{other}.sources to narrow one of them"
            )));
        }
    }
    Ok(())
}

/// Enabled providers, their narrowed sources and source owners. Built once
/// per settings load.
#[derive(Clone)]
pub(crate) struct ProviderCatalog {
    providers: BTreeMap<IdentityProviderId, Arc<dyn IdentityProvider>>,
    sources: BTreeMap<IdentityProviderId, Vec<SourceDomain>>,
    owners: BTreeMap<SourceDomain, IdentityProviderId>,
}

impl ProviderCatalog {
    /// Builds the catalog from settings, without resolving any secret.
    ///
    /// It reads `settings.ec.partners` directly and never builds a
    /// `PartnerRegistry`, so a deploy-time `api_token` that holds a secret
    /// store key name is fine. It holds the generic pull providers, then
    /// the `registered` providers integrations contributed, with each
    /// source owner from `[[ec.partners]].owner`. It validates every descriptor with
    /// [`validate_identity_catalog`] before building, so a duplicate ID is
    /// reported by that one validator.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when a pull partner
    /// cannot be described or the catalog rules fail.
    pub(crate) fn from_settings(
        settings: &Settings,
        registered: &[Arc<dyn IdentityProvider>],
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut providers = settings
            .ec
            .partners
            .iter()
            .filter(|partner| partner.pull_sync_enabled)
            .map(|partner| {
                GenericPullProvider::from_partner(partner)
                    .map(|provider| Arc::new(provider) as Arc<dyn IdentityProvider>)
                    .change_context(TrustedServerError::Configuration {
                        message: format!(
                            "ec.partners: pull sync of '{}' cannot be described as an identity provider",
                            partner.source_domain
                        ),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        providers.extend(registered.iter().cloned());
        let descriptors: Vec<_> = providers
            .iter()
            .map(|provider| provider.descriptor().clone())
            .collect();
        validate_identity_catalog(&descriptors, &settings.ec.partners, &settings.ec.identity)?;
        let owners = settings
            .ec
            .partners
            .iter()
            .filter_map(|partner| {
                let owner = partner.owner.as_deref()?;
                Some((partner.source_domain.parse().ok()?, owner.parse().ok()?))
            })
            .collect();
        Ok(Self::new(providers, owners))
    }

    /// A catalog with no providers.
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::new(Vec::new(), BTreeMap::new())
    }

    /// Builds the catalog. Validation (ownership, duplicates) happens in
    /// config validation before this is called.
    #[must_use]
    pub(crate) fn new(
        providers: Vec<Arc<dyn IdentityProvider>>,
        owners: BTreeMap<SourceDomain, IdentityProviderId>,
    ) -> Self {
        let providers: BTreeMap<_, _> = providers
            .into_iter()
            .map(|provider| (provider.descriptor().id().clone(), provider))
            .collect();
        let sources = providers
            .iter()
            .map(|(id, provider)| (id.clone(), provider.descriptor().sources().to_vec()))
            .collect();
        Self {
            providers,
            sources,
            owners,
        }
    }

    /// Narrows providers to the listed sources their descriptors declare
    /// (spec 10.1). Config validation has rejected a source a descriptor
    /// does not declare and a narrowing to nothing.
    #[must_use]
    pub(crate) fn with_narrowing(
        mut self,
        narrowing: BTreeMap<IdentityProviderId, Vec<SourceDomain>>,
    ) -> Self {
        for (id, listed) in narrowing {
            if let Some(sources) = self.sources.get_mut(&id) {
                sources.retain(|source| listed.contains(source));
            }
        }
        self
    }

    /// A provider by ID.
    #[must_use]
    pub(crate) fn get(&self, id: &IdentityProviderId) -> Option<&Arc<dyn IdentityProvider>> {
        self.providers.get(id)
    }

    /// Every provider, ordered by ID.
    pub(crate) fn providers(&self) -> impl Iterator<Item = &Arc<dyn IdentityProvider>> {
        self.providers.values()
    }

    /// Whether `id` is an enabled provider.
    #[must_use]
    pub(crate) fn is_configured(&self, id: &IdentityProviderId) -> bool {
        self.providers.contains_key(id)
    }

    /// The owner of `source`, if any.
    #[must_use]
    pub(crate) fn owner_of(&self, source: &SourceDomain) -> Option<&IdentityProviderId> {
        self.owners.get(source)
    }

    /// A provider's sources after narrowing. Empty for an unknown provider.
    #[must_use]
    pub(crate) fn sources_of(&self, id: &IdentityProviderId) -> &[SourceDomain] {
        self.sources.get(id).map_or(&[], Vec::as_slice)
    }

    /// Every provider that claims `source` after narrowing, ordered by ID.
    pub(crate) fn claimants<'a>(
        &'a self,
        source: &'a SourceDomain,
    ) -> impl Iterator<Item = &'a IdentityProviderId> {
        self.sources
            .iter()
            .filter(move |(_, sources)| sources.contains(source))
            .map(|(id, _)| id)
    }

    /// Whether `id` captures and never resolves.
    #[must_use]
    pub(crate) fn is_capture_only(&self, id: &IdentityProviderId) -> bool {
        self.get(id).is_some_and(|provider| {
            let descriptor = provider.descriptor();
            descriptor.captures().is_some() && descriptor.resolves().is_none()
        })
    }

    /// The narrowed sources of a capture-only provider. Empty for any other.
    #[must_use]
    pub(crate) fn capture_only_sources(&self, id: &IdentityProviderId) -> &[SourceDomain] {
        if self.is_capture_only(id) {
            self.sources_of(id)
        } else {
            &[]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::descriptor::{Captures, Resolves};
    use crate::ec::identity::examples::{ExampleIdProvider, example_config};
    use crate::ec::identity::generic_pull::generic_pull_descriptors;
    use crate::ec::identity::test_support::{ec_partner, fixed_capture_provider, resolver_policy};
    use crate::settings::{ProviderSettings, RevokeScope};
    use crate::test_support::tests::create_test_settings;

    const TOKEN: &str = "example-token-value-0123456789abcdef";

    fn partner() -> EcPartner {
        ec_partner(
            "ssp.example.com",
            "https://sync.ssp.example.com/pull",
            TOKEN,
        )
    }

    fn owned_partner(source: &str, owner: Option<&str>) -> EcPartner {
        let mut partner = ec_partner(source, &format!("https://sync.{source}/pull"), TOKEN);
        partner.pull_sync_enabled = false;
        partner.owner = owner.map(ToOwned::to_owned);
        partner
    }

    fn resolving_descriptor(id: &str, sources: &[&str]) -> ProviderDescriptor {
        let mut builder = ProviderDescriptor::builder(id.parse().expect("should parse the ID"))
            .with_resolves(Resolves::new().with_endpoint("https://api.example.com/fetch"))
            .with_consent_policy(resolver_policy());
        for source in sources {
            builder = builder.with_source(source.parse().expect("should parse the source"));
        }
        builder.build().expect("should build the descriptor")
    }

    fn capture_descriptor(id: &str, sources: &[&str]) -> ProviderDescriptor {
        fixed_capture_provider(id, sources).descriptor().clone()
    }

    fn listing(sources: &[&str]) -> ProviderSettings {
        ProviderSettings {
            sources: Some(sources.iter().map(|source| (*source).to_owned()).collect()),
            ..ProviderSettings::default()
        }
    }

    fn identity_tables(tables: Vec<(&str, ProviderSettings)>) -> IdentitySettings {
        let mut identity = IdentitySettings::default();
        for (id, settings) in tables {
            identity.providers.insert(id.to_owned(), settings);
        }
        identity
    }

    fn validate_with(
        descriptors: &[ProviderDescriptor],
        partners: &[EcPartner],
        identity: &IdentitySettings,
    ) -> Result<(), String> {
        validate_identity_catalog(descriptors, partners, identity).map_err(|e| format!("{e}"))
    }

    fn identity_with(id: &str, settings: ProviderSettings) -> IdentitySettings {
        let mut identity = IdentitySettings::default();
        identity.providers.insert(id.to_owned(), settings);
        identity
    }

    fn validate(partners: &[EcPartner], identity: &IdentitySettings) -> Result<(), String> {
        let descriptors = generic_pull_descriptors(partners).expect("should describe");
        validate_identity_catalog(&descriptors, partners, identity).map_err(|e| format!("{e:?}"))
    }

    #[test]
    fn validate_identity_catalog_rejects_unknown_provider_tables() {
        let partners = [partner()];

        let error = validate(
            &partners,
            &identity_with("pull-other-example-com", ProviderSettings::default()),
        )
        .expect_err("should reject a table naming no provider");
        assert!(
            error.contains("pull-other-example-com") && error.contains("pull-ssp-example-com"),
            "should name the table and list the configured IDs, got: {error}"
        );

        validate(
            &partners,
            &identity_with("pull-ssp-example-com", ProviderSettings::default()),
        )
        .expect("control: a table for the real pull ID should pass");
    }

    #[test]
    fn validate_identity_catalog_rejects_revoke_scope_without_captures() {
        let partners = [partner()];
        let with_scope = ProviderSettings {
            revoke_scope: Some(RevokeScope::Provider),
            ..ProviderSettings::default()
        };

        validate(
            &partners,
            &identity_with("pull-ssp-example-com", with_scope.clone()),
        )
        .expect_err("should reject revoke_scope on a provider without captures");
        validate(
            &partners,
            &identity_with("pull-ssp-example-com", ProviderSettings::default()),
        )
        .expect("control: the same table without revoke_scope should pass");

        let capturing = fixed_capture_provider("capturer", &["capture.example.com"]);
        assert!(
            capturing
                .descriptor()
                .captures()
                .is_some_and(|c| *c != Captures::new()),
            "should hold a capturing descriptor for the control"
        );
        let capturing_scope = ProviderSettings {
            sources: Some(vec!["capture.example.com".to_owned()]),
            ..with_scope
        };
        validate_identity_catalog(
            &[capturing.descriptor().clone()],
            &[owned_partner("capture.example.com", None)],
            &identity_with("capturer", capturing_scope),
        )
        .expect("control: revoke_scope on a capturing provider should pass");
    }

    #[test]
    fn validate_identity_catalog_rejects_duplicate_ids_and_more_than_32_providers() {
        let one = partner();
        let descriptors =
            generic_pull_descriptors(std::slice::from_ref(&one)).expect("should describe");
        let duplicated = [descriptors[0].clone(), descriptors[0].clone()];
        let _ = validate_identity_catalog(&duplicated, &[one], &IdentitySettings::default())
            .expect_err("should reject a duplicate ID");

        let many = |count: usize| -> Vec<EcPartner> {
            (0..count)
                .map(|n| {
                    let source = format!("ssp{n}.example.com");
                    ec_partner(&source, &format!("https://sync.{source}/pull"), TOKEN)
                })
                .collect()
        };
        validate(&many(32), &IdentitySettings::default())
            .expect("control: 32 providers should pass");
        validate(&many(33), &IdentitySettings::default()).expect_err("should reject 33 providers");
    }

    #[test]
    fn from_settings_builds_from_unresolved_settings() {
        let mut settings = create_test_settings();
        let mut unresolved = partner();
        unresolved.api_token = Some(crate::redacted::Redacted::new("key-name".to_owned()));
        settings.ec.partners = vec![unresolved];

        assert!(
            crate::ec::registry::PartnerRegistry::from_config(&settings.ec.partners).is_err(),
            "should hold settings the registry rejects"
        );
        let catalog = ProviderCatalog::from_settings(&settings, &[]).expect("should build");

        let id = "pull-ssp-example-com".parse().expect("should parse the ID");
        assert!(catalog.is_configured(&id), "should hold the pull provider");
        assert_eq!(catalog.sources_of(&id).len(), 1, "should claim its source");
        assert!(
            catalog
                .owner_of(&"ssp.example.com".parse().expect("should parse"))
                .is_none(),
            "should have no owner for a partner that names none"
        );
    }

    #[test]
    fn registered_provider_with_a_generic_pull_id_is_rejected_by_the_shared_validator() {
        let mut settings = create_test_settings();
        settings.ec.partners = vec![partner()];

        let duplicate = fixed_capture_provider("pull-ssp-example-com", &["other.example.com"]);
        let error = ProviderCatalog::from_settings(&settings, &[duplicate])
            .err()
            .expect("should reject a registered ID that a pull partner already uses");
        assert!(
            format!("{error}")
                .contains("identity provider `pull-ssp-example-com` is declared more than once"),
            "should report the duplicate ID: {error}"
        );

        settings
            .ec
            .partners
            .push(owned_partner("exampleid.example.com", None));
        let registered =
            ExampleIdProvider::new(&example_config()).expect("should build the example provider");
        let catalog = ProviderCatalog::from_settings(&settings, &[Arc::new(registered)])
            .expect("control: a distinct registered ID should build");
        assert!(
            catalog.is_configured(&"exampleid".parse().expect("should parse")),
            "should hold the registered provider"
        );
        assert!(
            catalog.is_configured(&"pull-ssp-example-com".parse().expect("should parse")),
            "should keep the pull provider"
        );
    }

    #[test]
    fn owner_must_name_an_enabled_provider_that_declares_the_source() {
        let descriptors = vec![resolving_descriptor(
            "exampleid",
            &["exampleid.example.com"],
        )];
        let ok = vec![owned_partner("exampleid.example.com", Some("exampleid"))];
        validate_with(&descriptors, &ok, &IdentitySettings::default())
            .expect("should accept an owner that declares the source");

        let unknown = vec![owned_partner("exampleid.example.com", Some("missing"))];
        let error = validate_with(&descriptors, &unknown, &IdentitySettings::default())
            .expect_err("should reject an owner that is not enabled");
        assert!(error.contains("missing"), "should name the owner: {error}");

        let wrong_source = vec![
            owned_partner("exampleid.example.com", None),
            owned_partner("other.example.com", Some("exampleid")),
        ];
        let error = validate_with(&descriptors, &wrong_source, &IdentitySettings::default())
            .expect_err("should reject an owner whose descriptor lacks the source");
        assert!(
            error.contains("other.example.com"),
            "should name the source: {error}"
        );
    }

    #[test]
    fn owner_uses_the_narrowed_descriptor() {
        let descriptors = vec![resolving_descriptor(
            "exampleid",
            &["a.example.com", "b.example.com"],
        )];
        let partners = vec![
            owned_partner("a.example.com", Some("exampleid")),
            owned_partner("b.example.com", None),
        ];
        validate_with(
            &descriptors,
            &partners,
            &identity_tables(vec![("exampleid", listing(&["a.example.com"]))]),
        )
        .expect("control: an owner keeping its source should pass");

        let error = validate_with(
            &descriptors,
            &partners,
            &identity_tables(vec![("exampleid", listing(&["b.example.com"]))]),
        )
        .expect_err("should reject an owner narrowed away from its source");
        assert!(
            error.contains("a.example.com"),
            "should name the source: {error}"
        );
    }

    #[test]
    fn provider_claiming_a_source_without_a_partner_entry_is_rejected() {
        let descriptors = vec![resolving_descriptor(
            "exampleid",
            &["a.example.com", "b.example.com"],
        )];
        let partners = vec![owned_partner("a.example.com", None)];
        let error = validate_with(&descriptors, &partners, &IdentitySettings::default())
            .expect_err("should reject a claimed source with no partner entry");
        assert!(
            error.contains("b.example.com") && error.contains("exampleid"),
            "should name the source and the provider: {error}"
        );

        validate_with(
            &descriptors,
            &partners,
            &identity_tables(vec![("exampleid", listing(&["a.example.com"]))]),
        )
        .expect("control: narrowing to the declared partner should pass");
        validate_with(
            &descriptors,
            &[
                owned_partner("a.example.com", None),
                owned_partner("b.example.com", None),
            ],
            &IdentitySettings::default(),
        )
        .expect("control: a partner entry per source should pass");
    }

    #[test]
    fn two_providers_claiming_an_owned_source_are_rejected() {
        let descriptors = vec![
            resolving_descriptor("first", &["a.example.com"]),
            resolving_descriptor("second", &["a.example.com", "b.example.com"]),
        ];
        let owned = vec![
            owned_partner("a.example.com", Some("first")),
            owned_partner("b.example.com", None),
        ];
        let error = validate_with(&descriptors, &owned, &IdentitySettings::default())
            .expect_err("should reject a second claimant of an owned source");
        assert!(
            error.contains("a.example.com") && error.contains("second"),
            "should name the source and the other claimant: {error}"
        );

        validate_with(
            &descriptors,
            &owned,
            &identity_tables(vec![("second", listing(&["b.example.com"]))]),
        )
        .expect("control: narrowing the other claimant away should pass");

        let unowned = vec![
            owned_partner("a.example.com", None),
            owned_partner("b.example.com", None),
        ];
        validate_with(&descriptors, &unowned, &IdentitySettings::default())
            .expect("control: two claimants of an unowned source should pass");
    }

    #[test]
    fn sources_must_name_declared_sources_and_not_narrow_to_nothing() {
        let descriptors = vec![resolving_descriptor("exampleid", &["a.example.com"])];
        let partners = vec![
            owned_partner("a.example.com", None),
            owned_partner("c.example.com", None),
        ];
        validate_with(
            &descriptors,
            &partners,
            &identity_tables(vec![("exampleid", listing(&["a.example.com"]))]),
        )
        .expect("control: a declared source should pass");

        let error = validate_with(
            &descriptors,
            &partners,
            &identity_tables(vec![("exampleid", listing(&["c.example.com"]))]),
        )
        .expect_err("should reject an undeclared source");
        assert!(
            error.contains("c.example.com") && error.contains("exampleid"),
            "should name the source and the provider: {error}"
        );

        let error = validate_with(
            &descriptors,
            &partners,
            &identity_tables(vec![("exampleid", listing(&[]))]),
        )
        .expect_err("should reject narrowing to nothing");
        assert!(
            error.contains("exampleid") && error.contains("no source"),
            "should name the provider: {error}"
        );
    }

    #[test]
    fn capture_only_provider_without_sources_is_rejected() {
        let descriptors = vec![capture_descriptor("capturer", &["capture.example.com"])];
        let partners = vec![owned_partner("capture.example.com", None)];
        let error = validate_with(&descriptors, &partners, &IdentitySettings::default())
            .expect_err("should reject a capture-only provider with no sources list");
        assert!(
            error.contains("capturer") && error.contains("sources"),
            "should name the provider and the key: {error}"
        );

        validate_with(
            &descriptors,
            &partners,
            &identity_tables(vec![("capturer", ProviderSettings::default())]),
        )
        .expect_err("should reject a provider table that lists no sources");

        validate_with(
            &descriptors,
            &partners,
            &identity_tables(vec![("capturer", listing(&["capture.example.com"]))]),
        )
        .expect("control: a sources list should pass");

        let resolving = vec![resolving_descriptor("exampleid", &["capture.example.com"])];
        validate_with(&resolving, &partners, &IdentitySettings::default())
            .expect("control: a resolving provider needs no sources list");
    }

    #[test]
    fn capture_only_provider_cannot_own_a_source() {
        let descriptors = vec![capture_descriptor("capturer", &["capture.example.com"])];
        let identity = identity_tables(vec![("capturer", listing(&["capture.example.com"]))]);
        let owned = vec![owned_partner("capture.example.com", Some("capturer"))];
        let error = validate_with(&descriptors, &owned, &identity)
            .expect_err("should reject a capture-only owner");
        assert!(
            error.contains("capturer") && error.contains("capture-only"),
            "should name the provider and the reason: {error}"
        );

        let unowned = vec![owned_partner("capture.example.com", None)];
        validate_with(&descriptors, &unowned, &identity)
            .expect("control: the same provider without ownership should pass");

        let resolving = vec![resolving_descriptor("capturer", &["capture.example.com"])];
        validate_with(&resolving, &owned, &IdentitySettings::default())
            .expect("control: a resolving provider may own the source");
    }

    #[test]
    fn from_settings_records_owners_from_partners() {
        let mut settings = create_test_settings();
        settings.ec.partners = vec![owned_partner("exampleid.example.com", Some("exampleid"))];
        let registered =
            ExampleIdProvider::new(&example_config()).expect("should build the example provider");
        let catalog = ProviderCatalog::from_settings(&settings, &[Arc::new(registered)])
            .expect("should build");

        assert_eq!(
            catalog.owner_of(&"exampleid.example.com".parse().expect("should parse")),
            Some(&"exampleid".parse().expect("should parse the ID")),
            "should record the partner's owner"
        );
        assert!(
            catalog
                .owner_of(&"other.example.com".parse().expect("should parse"))
                .is_none(),
            "control: an unowned source has no owner"
        );
    }
}
