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

pub mod admission;
pub mod config;
pub mod context;
pub mod endpoints;
pub mod formats;
pub mod identity;
pub mod orchestrator;
pub mod provider;
pub mod telemetry;
#[cfg(test)]
pub(crate) mod test_support;
pub mod types;

pub use admission::AuctionSource;
pub use config::AuctionConfig;
pub use context::{build_url_with_context_params, ContextQueryParams, ContextValue};
pub use orchestrator::AuctionOrchestrator;
pub use provider::AuctionProvider;
pub use telemetry::{
    build_auction_events, emit_auction_events_best_effort, emit_auction_events_best_effort_lazy,
    AbandonedProviderCall, AuctionEventBatch, AuctionEventRow, AuctionObservationContext,
    AuctionTelemetrySink, AuctionTerminalOutcome, NoopAuctionTelemetrySink,
};
pub use types::{
    AdFormat, AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType,
};

/// Type alias for provider builder functions.
type ProviderBuilder =
    fn(&Settings) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>>;

/// Returns the list of all available provider builder functions.
///
/// This list is used to auto-discover and register auction providers from settings.
/// Each builder function checks the settings for its specific provider configuration
/// and returns any enabled providers.
fn provider_builders() -> &'static [ProviderBuilder] {
    &[
        crate::integrations::prebid::register_auction_provider,
        crate::integrations::aps::register_providers,
        crate::integrations::adserver_mock::register_providers,
    ]
}

/// Build a new auction orchestrator for the current settings.
///
/// This constructor registers all auction providers discovered from the provided settings.
/// Callers can reuse the returned [`AuctionOrchestrator`] across requests.
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
    log::info!("Building auction orchestrator");

    let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());

    // Auto-discover and register all auction providers from settings
    for builder in provider_builders() {
        for provider in builder(settings)? {
            orchestrator.register_provider(provider);
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

    use super::build_orchestrator;

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
}
