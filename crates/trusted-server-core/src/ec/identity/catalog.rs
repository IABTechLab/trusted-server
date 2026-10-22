//! The enabled identity providers and source ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use error_stack::Report;
use sha2::{Digest as _, Sha256};

use super::config::{IdentitySettings, ProviderSettings};
use super::generic_pull::{GenericPullProvider, generic_pull_settings};
use super::ids::{IdentityProviderId, SourceDomain};
use super::provider::IdentityProvider;
use crate::ec::kv_types::MAX_PROVIDER_STATES;
use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;

/// Enabled providers, source owners, and the derived source index and `cf`.
/// Built once per settings load.
#[derive(Clone)]
pub(crate) struct ProviderCatalog {
    providers: BTreeMap<IdentityProviderId, Arc<dyn IdentityProvider>>,
    owners: BTreeMap<SourceDomain, IdentityProviderId>,
    sources: Vec<SourceDomain>,
    fingerprint: String,
    /// Provider defaults that an operator table replaces, such as a pull
    /// partner's legacy rate limit.
    bases: BTreeMap<IdentityProviderId, ProviderSettings>,
}

impl core::fmt::Debug for ProviderCatalog {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ProviderCatalog")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("owners", &self.owners)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

impl ProviderCatalog {
    /// A catalog with no providers.
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::new(Vec::new(), BTreeMap::new())
    }

    /// Builds the catalog. Config validation (ownership, duplicates) runs
    /// before this is called.
    #[must_use]
    pub(crate) fn new(
        providers: Vec<Arc<dyn IdentityProvider>>,
        owners: BTreeMap<SourceDomain, IdentityProviderId>,
    ) -> Self {
        let providers: BTreeMap<_, _> = providers
            .into_iter()
            .map(|provider| (provider.descriptor().id().clone(), provider))
            .collect();
        let sources: Vec<SourceDomain> = providers
            .values()
            .flat_map(|provider| provider.descriptor().sources().iter().cloned())
            .chain(owners.keys().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut hasher = Sha256::new();
        for (id, provider) in &providers {
            for source in provider.descriptor().sources() {
                let owner = owners
                    .get(source)
                    .map(IdentityProviderId::as_str)
                    .unwrap_or_default();
                hasher.update(format!("{id}|{source}|{owner}\n").as_bytes());
            }
        }
        let fingerprint = hex::encode(&hasher.finalize()[..4]);
        Self {
            providers,
            owners,
            sources,
            fingerprint,
            bases: BTreeMap::new(),
        }
    }

    /// The catalog for the partner registry: one generic pull provider per
    /// pull-enabled partner. A partner that cannot form a provider is logged
    /// and left out.
    ///
    /// # Errors
    ///
    /// [`TrustedServerError::Configuration`] for two providers with one ID or
    /// more than 32 enabled providers.
    pub(crate) fn from_partners(
        registry: &PartnerRegistry,
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut providers: Vec<Arc<dyn IdentityProvider>> = Vec::new();
        let mut bases = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for partner in registry.pull_enabled_partners() {
            let provider = match GenericPullProvider::from_partner(partner) {
                Ok(provider) => provider,
                Err(error) => {
                    log::warn!(
                        "Pull partner {} cannot run as an identity provider: {}",
                        partner.source_domain,
                        error.current_context()
                    );
                    continue;
                }
            };
            let id = provider.descriptor().id().clone();
            if !seen.insert(id.clone()) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!("two identity providers share the id `{id}`"),
                }));
            }
            bases.insert(id, generic_pull_settings(partner));
            providers.push(Arc::new(provider));
        }
        if providers.len() > MAX_PROVIDER_STATES {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "{} identity providers are enabled; at most {MAX_PROVIDER_STATES} are allowed",
                    providers.len()
                ),
            }));
        }
        let mut catalog = Self::new(providers, BTreeMap::new());
        catalog.bases = bases;
        Ok(catalog)
    }

    /// The operator settings for `id`: its `[ec.identity.providers.<id>]`
    /// table, else the provider's own defaults, else the global defaults.
    #[must_use]
    pub(crate) fn provider_settings(
        &self,
        identity: &IdentitySettings,
        id: &IdentityProviderId,
    ) -> ProviderSettings {
        identity.provider(id, self.bases.get(id).cloned())
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

    /// Every configured source, sorted. Indexes into this list are the `s`
    /// values of `ts-eids-s`.
    #[must_use]
    pub(crate) fn configured_sources(&self) -> &[SourceDomain] {
        &self.sources
    }

    /// `cf`: 8 hex digits over providers, sources and owners.
    #[must_use]
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::ec::identity::test_support::fixed_provider;

    #[test]
    fn indexes_sources_sorted_and_fingerprints_ownership() {
        let catalog = ProviderCatalog::new(
            vec![
                fixed_provider("p", &["b.example.com", "a.example.com"]),
                fixed_provider("q", &["c.example.com"]),
            ],
            BTreeMap::new(),
        );
        assert_eq!(
            catalog
                .configured_sources()
                .iter()
                .map(SourceDomain::as_str)
                .collect::<Vec<_>>(),
            vec!["a.example.com", "b.example.com", "c.example.com"],
            "should sort and de-duplicate sources"
        );
        assert_eq!(catalog.fingerprint().len(), 8, "cf is 8 hex digits");
        assert!(
            catalog.is_configured(&"p".parse().expect("should parse id")),
            "p is configured"
        );
        assert!(
            !catalog.is_configured(&"z".parse().expect("should parse id")),
            "z is not"
        );

        let owned = ProviderCatalog::new(
            vec![
                fixed_provider("p", &["b.example.com", "a.example.com"]),
                fixed_provider("q", &["c.example.com"]),
            ],
            [(
                "a.example.com".parse().expect("should parse source"),
                "p".parse().expect("should parse id"),
            )]
            .into_iter()
            .collect(),
        );
        assert_ne!(
            owned.fingerprint(),
            catalog.fingerprint(),
            "ownership changes cf"
        );
        assert_eq!(
            owned
                .owner_of(&"a.example.com".parse().expect("should parse source"))
                .map(IdentityProviderId::as_str),
            Some("p"),
            "should report the owner"
        );
        assert_eq!(
            ProviderCatalog::empty().configured_sources().len(),
            0,
            "empty catalog"
        );
    }
}
