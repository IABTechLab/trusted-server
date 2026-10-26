//! The enabled identity providers and source ownership.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::ids::{IdentityProviderId, SourceDomain};
use super::provider::IdentityProvider;

/// Enabled providers, their narrowed sources and source owners. Built once
/// per settings load.
#[derive(Clone)]
pub(crate) struct ProviderCatalog {
    providers: BTreeMap<IdentityProviderId, Arc<dyn IdentityProvider>>,
    sources: BTreeMap<IdentityProviderId, Vec<SourceDomain>>,
    owners: BTreeMap<SourceDomain, IdentityProviderId>,
}

impl ProviderCatalog {
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
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
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
