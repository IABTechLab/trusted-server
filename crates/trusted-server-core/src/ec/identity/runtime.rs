//! The identity state built once per settings load (spec 10.3).

use std::collections::BTreeMap;

use error_stack::Report;

use super::catalog::ProviderCatalog;
use super::coordinator::IdentityDeps;
use super::ids::{IdentityProviderId, SourceDomain};
use super::input::RandomSource;
use super::revision::IdentityKeys;
use crate::ec::rate_limiter::RateLimiter;
use crate::error::TrustedServerError;
use crate::platform::RuntimeServices;
use crate::settings::{IdentitySettings, Settings};

/// The provider catalog, the keys and the identity policy of one settings load.
///
/// The adapter builds it once and shares it through an `Arc`. It is the one
/// identity type the adapter names, so the crate-private catalog and keys
/// never cross the crate boundary.
pub struct IdentityRuntime {
    catalog: ProviderCatalog,
    // Temporary: first read by a later identity task.
    #[allow(dead_code)]
    keys: IdentityKeys,
    // Temporary: first read by a later identity task.
    #[allow(dead_code)]
    identity: IdentitySettings,
}

impl core::fmt::Debug for IdentityRuntime {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("IdentityRuntime")
            .field("providers", &self.catalog.providers().count())
            .finish_non_exhaustive()
    }
}

impl IdentityRuntime {
    /// Builds the runtime from settings, without resolving any secret.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when a provider cannot be
    /// described or the catalog rules fail.
    pub fn from_settings(settings: &Settings) -> Result<Self, Report<TrustedServerError>> {
        let catalog = ProviderCatalog::from_settings(settings)?
            .with_narrowing(narrowing(&settings.ec.identity));
        Ok(Self {
            catalog,
            keys: IdentityKeys::from_settings(settings),
            identity: settings.ec.identity.clone(),
        })
    }

    /// The enabled providers.
    #[must_use]
    pub(crate) fn catalog(&self) -> &ProviderCatalog {
        &self.catalog
    }

    /// The keys derived from `ec.passphrase`.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn keys(&self) -> &IdentityKeys {
        &self.keys
    }

    /// The `[ec.identity]` policy.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn identity(&self) -> &IdentitySettings {
        &self.identity
    }

    /// What the coordinator needs besides the request.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn deps<'a>(
        &'a self,
        settings: &'a Settings,
        services: &'a RuntimeServices,
        random: &'a dyn RandomSource,
        rate_limiter: Option<&'a dyn RateLimiter>,
    ) -> IdentityDeps<'a> {
        IdentityDeps {
            settings,
            catalog: &self.catalog,
            keys: &self.keys,
            random,
            services,
            rate_limiter,
        }
    }
}

/// Each provider table's `sources`, for the providers that list any.
///
/// Config validation has rejected a malformed source, so one that does not
/// parse is skipped.
fn narrowing(identity: &IdentitySettings) -> BTreeMap<IdentityProviderId, Vec<SourceDomain>> {
    identity
        .providers
        .iter()
        .filter_map(|(id, table)| {
            let listed = table.sources.as_ref()?;
            Some((
                id.parse().ok()?,
                listed
                    .iter()
                    .filter_map(|source| source.parse().ok())
                    .collect(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::test_support::{ec_partner, pid, source};
    use crate::settings::ProviderSettings;
    use crate::test_support::tests::create_test_settings;

    fn settings_with_listed_sources(listed: Option<Vec<String>>) -> Settings {
        let mut settings = create_test_settings();
        settings.ec.partners = vec![ec_partner(
            "ssp.example.com",
            "https://sync.ssp.example.com/pull",
            "example-token-value-0123456789abcdef",
        )];
        settings.ec.identity.providers.insert(
            "pull-ssp-example-com".to_owned(),
            ProviderSettings {
                sources: listed,
                ..ProviderSettings::default()
            },
        );
        settings
    }

    #[test]
    fn runtime_debug_prints_no_key_material() {
        let settings = settings_with_listed_sources(None);
        let runtime = IdentityRuntime::from_settings(&settings).expect("should build");

        let printed = format!("{runtime:?}");

        assert!(
            printed.contains("providers: 1"),
            "should print the provider count: {printed}"
        );
        assert!(
            !printed.contains(settings.ec.passphrase.expose()),
            "should not print the passphrase: {printed}"
        );
        assert!(
            !printed.contains("keys"),
            "should not print the keys: {printed}"
        );
    }

    #[test]
    fn runtime_narrows_providers_by_their_sources_table() {
        let id = pid("pull-ssp-example-com");

        let unlisted = IdentityRuntime::from_settings(&settings_with_listed_sources(None))
            .expect("should build");
        let kept = IdentityRuntime::from_settings(&settings_with_listed_sources(Some(vec![
            "ssp.example.com".to_owned(),
        ])))
        .expect("should build");
        let dropped = IdentityRuntime::from_settings(&settings_with_listed_sources(Some(vec![
            "other.example.com".to_owned(),
        ])))
        .expect("should build");

        assert_eq!(
            unlisted.catalog().sources_of(&id),
            [source("ssp.example.com")],
            "control: should claim the descriptor's source with no table"
        );
        assert_eq!(
            kept.catalog().sources_of(&id),
            [source("ssp.example.com")],
            "should keep a listed source"
        );
        assert!(
            dropped.catalog().sources_of(&id).is_empty(),
            "should drop a source the table does not list"
        );
    }
}
