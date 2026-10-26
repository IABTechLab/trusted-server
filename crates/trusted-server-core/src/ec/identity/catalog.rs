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
/// is the one place these rules live. It reads descriptors only, so it runs
/// on unresolved deploy-time settings.
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
    /// store key name is fine. In Part 1 it holds generic pull providers
    /// only, with no source owners. It validates their descriptors with
    /// [`validate_identity_catalog`] before building.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when a pull partner
    /// cannot be described or the catalog rules fail.
    pub(crate) fn from_settings(settings: &Settings) -> Result<Self, Report<TrustedServerError>> {
        let providers = settings
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
        let descriptors: Vec<_> = providers
            .iter()
            .map(|provider| provider.descriptor().clone())
            .collect();
        validate_identity_catalog(&descriptors, &settings.ec.partners, &settings.ec.identity)?;
        Ok(Self::new(providers, BTreeMap::new()))
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
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
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
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
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
    use crate::ec::identity::descriptor::Captures;
    use crate::ec::identity::generic_pull::generic_pull_descriptors;
    use crate::ec::identity::test_support::{ec_partner, fixed_capture_provider};
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
        validate_identity_catalog(
            &[capturing.descriptor().clone()],
            &[],
            &identity_with("capturer", with_scope),
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
        let catalog = ProviderCatalog::from_settings(&settings).expect("should build");

        let id = "pull-ssp-example-com".parse().expect("should parse the ID");
        assert!(catalog.is_configured(&id), "should hold the pull provider");
        assert_eq!(catalog.sources_of(&id).len(), 1, "should claim its source");
        assert!(
            catalog
                .owner_of(&"ssp.example.com".parse().expect("should parse"))
                .is_none(),
            "should have no owners in Part 1"
        );
    }
}
