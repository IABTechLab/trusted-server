//! Auction orchestration module for managing multi-provider bidding.
//!
//! This module provides an extensible framework for running auctions across
//! multiple providers (Prebid, Amazon APS, Google GAM, etc.) with support for
//! parallel execution and mediation strategies.
//!
//! Note: Individual auction providers are located in the `integrations` module
//! (e.g., `crate::integrations::aps`, `crate::integrations::prebid`).

use error_stack::Report;

use crate::error::TrustedServerError;
use crate::settings::Settings;
use std::sync::Arc;

pub mod config;
pub mod context;
pub mod endpoints;
pub mod formats;
pub mod orchestrator;
pub mod provider;
pub mod telemetry;
#[cfg(test)]
pub(crate) mod test_support;
pub mod types;

pub use config::AuctionConfig;
pub use context::{ContextQueryParams, ContextValue, build_url_with_context_params};
pub use orchestrator::AuctionOrchestrator;
pub use provider::AuctionProvider;
pub use telemetry::{
    AbandonedProviderCall, AuctionEventBatch, AuctionEventRow, AuctionObservationContext,
    AuctionSource, AuctionTelemetrySink, AuctionTerminalOutcome, NoopAuctionTelemetrySink,
    build_auction_events, emit_auction_events_best_effort, emit_auction_events_best_effort_lazy,
};
pub use types::{
    AdFormat, AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType,
};

/// Builds the auction providers an integration contributes from settings.
pub type AuctionProviderBuilderFn =
    fn(&Settings) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>>;

/// Validates the provider's configuration for deployment and reports whether
/// it is enabled.
///
/// Runs for every builder, enabled or not, so a typo in a disabled block is
/// still caught.
pub type AuctionProviderValidateFn = fn(&Settings) -> Result<bool, Report<TrustedServerError>>;

/// A named factory for one auction provider, the unit an adapter or a vendor
/// crate hands to [`build_orchestrator_with_providers`].
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use error_stack::Report;
/// use trusted_server_core::auction::{
///     AuctionProvider, AuctionProviderBuilder, build_orchestrator_with_providers,
/// };
/// use trusted_server_core::error::TrustedServerError;
/// use trusted_server_core::settings::Settings;
///
/// fn build(
///     _settings: &Settings,
/// ) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
///     Ok(Vec::new())
/// }
///
/// fn validate(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
///     Ok(false)
/// }
///
/// # fn demo(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
/// let builder = AuctionProviderBuilder::new("example", "example-crate", build, validate);
/// let orchestrator = build_orchestrator_with_providers(settings, &[builder])?;
/// assert_eq!(orchestrator.provider_count(), 0);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug)]
pub struct AuctionProviderBuilder {
    name: &'static str,
    source: &'static str,
    build: AuctionProviderBuilderFn,
    validate: AuctionProviderValidateFn,
}

impl AuctionProviderBuilder {
    /// Creates a builder for the auction provider `name`, attributed to
    /// `source` (a crate or package name used in duplicate-name errors).
    #[must_use]
    pub const fn new(
        name: &'static str,
        source: &'static str,
        build: AuctionProviderBuilderFn,
        validate: AuctionProviderValidateFn,
    ) -> Self {
        Self {
            name,
            source,
            build,
            validate,
        }
    }

    /// The auction provider name this builder produces.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The source label used in diagnostics.
    #[must_use]
    pub const fn source(&self) -> &'static str {
        self.source
    }

    /// Builds the providers this builder contributes, empty when the provider
    /// is not enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider is enabled with invalid
    /// configuration.
    pub(crate) fn build(
        &self,
        settings: &Settings,
    ) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
        (self.build)(settings)
    }

    /// Validates the provider's configuration for deployment and reports
    /// whether the provider is enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration cannot be parsed or fails
    /// validation.
    // Deploy validation does not yet enumerate auction provider builders, so
    // nothing in the crate calls this until that is wired up.
    #[allow(dead_code)]
    pub(crate) fn validate(&self, settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        (self.validate)(settings)
    }
}

/// The built-in auction providers, in registration order.
const BUILT_IN_PROVIDER_BUILDERS: &[AuctionProviderBuilder] = &[
    AuctionProviderBuilder::new(
        "prebid",
        crate::integrations::CORE_SOURCE,
        crate::integrations::prebid::register_auction_provider,
        crate::integrations::prebid::validate,
    ),
    AuctionProviderBuilder::new(
        "aps",
        crate::integrations::CORE_SOURCE,
        crate::integrations::aps::register_providers,
        crate::integrations::aps::validate,
    ),
    AuctionProviderBuilder::new(
        "adserver_mock",
        crate::integrations::CORE_SOURCE,
        crate::integrations::adserver_mock::register_providers,
        crate::integrations::adserver_mock::validate,
    ),
];

/// The built-in auction provider builders, in registration order.
pub(crate) fn provider_builders() -> &'static [AuctionProviderBuilder] {
    BUILT_IN_PROVIDER_BUILDERS
}

/// Every auction provider builder the orchestrator will consider: the built-in
/// set followed by `extra`, in that order, so registration order for the
/// built-ins never changes.
pub(crate) fn all_provider_builders(
    extra: &[AuctionProviderBuilder],
) -> impl Iterator<Item = AuctionProviderBuilder> + '_ {
    provider_builders()
        .iter()
        .copied()
        .chain(extra.iter().copied())
}

/// Build a new auction orchestrator for the current settings.
///
/// This constructor registers all built-in auction providers discovered from
/// the provided settings. Callers can reuse the returned
/// [`AuctionOrchestrator`] across requests.
///
/// # Arguments
/// * `settings` - Application settings used to configure the orchestrator and providers
///
/// # Errors
///
/// Returns an error when an enabled auction provider has invalid configuration.
pub fn build_orchestrator(
    settings: &Settings,
) -> Result<AuctionOrchestrator, Report<TrustedServerError>> {
    build_orchestrator_with_providers(settings, &[])
}

/// Build a new auction orchestrator from the built-in provider builders
/// followed by the externally supplied builders an adapter registers.
///
/// # Arguments
/// * `settings` - Application settings used to configure the orchestrator and providers
/// * `extra` - Provider builders supplied by an adapter or a vendor crate
///
/// # Errors
///
/// Returns an error when an enabled auction provider has invalid
/// configuration, when two builders produce the same provider name, or when a
/// configured provider name has no registered provider.
pub fn build_orchestrator_with_providers(
    settings: &Settings,
    extra: &[AuctionProviderBuilder],
) -> Result<AuctionOrchestrator, Report<TrustedServerError>> {
    log::info!("Building auction orchestrator");

    let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());

    // Auto-discover and register all auction providers from settings
    for builder in all_provider_builders(extra) {
        for provider in builder.build(settings)? {
            orchestrator.register_provider(provider, builder.source())?;
        }
    }

    orchestrator.validate_configured_provider_names()?;

    log::info!(
        "Auction orchestrator built with {} providers",
        orchestrator.provider_count()
    );

    Ok(orchestrator)
}

#[cfg(test)]
mod tests {
    use crate::settings::Settings;
    use crate::test_support::tests::crate_test_settings_str;

    use std::sync::Arc;

    use error_stack::Report;

    use super::test_support::NamedTestProvider;
    use super::{
        AuctionProvider, AuctionProviderBuilder, build_orchestrator,
        build_orchestrator_with_providers,
    };
    use crate::error::TrustedServerError;

    fn settings_with_auction_config(auction_config: &str) -> Settings {
        let settings_str = format!("{}\n{auction_config}", crate_test_settings_str());
        let mut settings = Settings::from_toml(&settings_str)
            .expect("should parse auction provider validation test settings");
        settings.proxy.allowed_domains = vec!["*.example".to_string(), "*.example.com".to_string()];
        settings
    }

    fn assert_orchestrator_error_contains(settings: &Settings, expected: &str) {
        let Err(err) = build_orchestrator(settings) else {
            panic!("build_orchestrator should reject invalid auction providers");
        };
        assert!(
            err.to_string().contains(expected),
            "should include expected validation message: {expected}"
        );
    }

    #[test]
    fn configured_unregistered_provider_fails_startup() {
        let settings = settings_with_auction_config(
            r#"
            [auction]
            enabled = true
            providers = ["missing-provider"]
            timeout_ms = 2000
        "#,
        );

        assert_orchestrator_error_contains(
            &settings,
            "Auction provider `missing-provider` is listed in [auction] but no enabled integration provides it",
        );
    }

    #[test]
    fn mixed_registered_and_unregistered_providers_fail_startup() {
        let settings = settings_with_auction_config(
            r#"
            [auction]
            enabled = true
            providers = ["prebid", "missing-provider"]
            timeout_ms = 2000
        "#,
        );

        assert_orchestrator_error_contains(
            &settings,
            "Auction provider `missing-provider` is listed in [auction] but no enabled integration provides it",
        );
    }

    #[test]
    fn configured_unregistered_mediator_fails_startup() {
        let settings = settings_with_auction_config(
            r#"
            [auction]
            enabled = true
            providers = ["prebid"]
            mediator = "missing-mediator"
            timeout_ms = 2000
        "#,
        );

        assert_orchestrator_error_contains(
            &settings,
            "Auction provider `missing-mediator` is listed in [auction] but no enabled integration provides it",
        );
    }

    /// Builds one provider named `seam-probe`, standing in for a provider a
    /// vendor crate contributes.
    fn build_probe_provider(
        _settings: &Settings,
    ) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
        Ok(vec![Arc::new(NamedTestProvider::new("seam-probe"))])
    }

    /// Builds one provider that claims the built-in `prebid` name.
    fn build_conflicting_prebid_provider(
        _settings: &Settings,
    ) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
        Ok(vec![Arc::new(NamedTestProvider::new("prebid"))])
    }

    fn probe_enabled(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        Ok(true)
    }

    #[test]
    fn build_orchestrator_with_providers_registers_an_external_provider() {
        let settings = settings_with_auction_config(
            r#"
            [auction]
            enabled = true
            providers = ["prebid", "seam-probe"]
            timeout_ms = 2000
        "#,
        );
        let extra = [AuctionProviderBuilder::new(
            "seam-probe",
            "seam-probe-crate",
            build_probe_provider,
            probe_enabled,
        )];

        let orchestrator = build_orchestrator_with_providers(&settings, &extra)
            .expect("should register the external auction provider");

        assert_eq!(
            orchestrator.provider_count(),
            2,
            "should register the built-in prebid provider and the external provider"
        );
    }

    #[test]
    fn build_orchestrator_with_providers_rejects_a_duplicate_provider_name() {
        let settings = settings_with_auction_config(
            r#"
            [auction]
            enabled = true
            providers = ["prebid"]
            timeout_ms = 2000
        "#,
        );
        let extra = [AuctionProviderBuilder::new(
            "prebid",
            "seam-probe-crate",
            build_conflicting_prebid_provider,
            probe_enabled,
        )];

        let error = build_orchestrator_with_providers(&settings, &extra)
            .err()
            .expect("should reject a duplicate auction provider name");

        let message = error.to_string();
        assert!(
            message.contains("prebid")
                && message.contains(crate::integrations::CORE_SOURCE)
                && message.contains("seam-probe-crate"),
            "error should name the provider and both sources: {message}"
        );
    }
}
