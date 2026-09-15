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
pub(crate) mod openrtb;
pub mod orchestrator;
pub mod plan;
pub(crate) mod profile;
pub mod provider;
pub(crate) mod routing;
pub mod telemetry;
#[cfg(test)]
pub(crate) mod test_support;
pub mod types;

pub use config::AuctionConfig;
pub use context::{ContextQueryParams, ContextValue, build_url_with_context_params};
pub use orchestrator::AuctionOrchestrator;
pub use plan::{
    AuctionPlan, BidderId, BidderRouteConfig, NotificationConfig, ProviderConfig, ProviderId,
    RoutingMode,
};
pub use provider::AuctionProvider;
pub use telemetry::{
    AbandonedProviderCall, AuctionEventBatch, AuctionEventRow, AuctionObservationContext,
    AuctionSource, AuctionTelemetrySink, AuctionTerminalOutcome, NoopAuctionTelemetrySink,
    build_auction_events, emit_auction_events_best_effort, emit_auction_events_best_effort_lazy,
};
pub use types::{
    AdFormat, AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType,
};

/// Compile the canonical target-independent auction plan for [`Settings`].
///
/// This is the single settings-to-plan boundary used by deploy validation,
/// adapter startup, and operator tooling. Global request signing remains owned
/// by [`Settings`] and is copied into compiler input only at this boundary.
///
/// # Errors
///
/// Returns an error when auction provider, bidder route, signing, or mediator
/// configuration is invalid.
pub fn compile_auction_plan(
    settings: &Settings,
) -> Result<AuctionPlan, Report<TrustedServerError>> {
    AuctionPlan::compile(plan::AuctionPlanConfig {
        timeout_ms: settings.auction.timeout_ms,
        providers: settings.auction.providers.clone(),
        bidders: settings.auction.bidders.clone(),
        mediator: settings.auction.mediator.clone(),
        request_signing: settings.request_signing.clone(),
    })
    .map(|plan| plan.with_enabled(settings.auction.enabled))
}

/// Build a new auction orchestrator from one shared compiled plan.
///
/// This constructor registers all auction providers discovered from the provided settings.
/// Callers can reuse the returned [`AuctionOrchestrator`] across requests.
///
/// # Arguments
/// * `plan` - Shared immutable compiled plan
/// * `settings` - Application settings used only for the separately registered mediator
///
/// # Errors
///
/// Returns an error when an enabled auction provider has invalid configuration.
pub fn build_orchestrator_with_plan(
    plan: Arc<AuctionPlan>,
    settings: &Settings,
) -> Result<AuctionOrchestrator, Report<TrustedServerError>> {
    log::info!("Building plan-backed auction orchestrator");

    let mediator = if let Some(expected_id) = plan.mediator() {
        let provider = crate::integrations::adserver_mock::register_providers(settings)?
            .into_iter()
            .find(|provider| provider.provider_name() == expected_id)
            .ok_or_else(|| {
                Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "auction mediator `{expected_id}` must reference a separately registered enabled integration with the exact same ID"
                    ),
                })
            })?;
        Some(provider)
    } else {
        None
    };
    let orchestrator = AuctionOrchestrator::from_plan(plan, mediator);

    log::info!(
        "Auction orchestrator built with {} bidder providers",
        orchestrator.provider_count()
    );

    Ok(orchestrator)
}

/// Test convenience constructor that compiles a plan before construction.
///
/// # Errors
///
/// Returns an error when plan compilation or mediator construction fails.
#[cfg(test)]
pub fn build_orchestrator(
    settings: &Settings,
) -> Result<AuctionOrchestrator, Report<TrustedServerError>> {
    let plan = Arc::new(compile_auction_plan(settings)?);
    build_orchestrator_with_plan(plan, settings)
}

#[cfg(test)]
mod plan_sharing_tests {
    use super::*;
    use crate::integrations::IntegrationRegistry;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn orchestrator_and_registry_share_the_compiled_plan_allocation() {
        let settings = create_test_settings();
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile auction plan"));
        let orchestrator = build_orchestrator_with_plan(Arc::clone(&plan), &settings)
            .expect("should build orchestrator");
        let registry = IntegrationRegistry::with_plan(&settings, Arc::clone(&plan))
            .expect("should build integration registry");

        assert!(orchestrator.shares_plan(&plan));
        assert!(registry.shares_plan(&plan));
    }

    #[test]
    fn configured_mediator_requires_enabled_exact_registration() {
        for mediator_config in [None, Some(serde_json::json!({"enabled": false}))] {
            let mut settings = create_test_settings();
            settings.auction.mediator = Some("adserver_mock".to_string());
            if let Some(config) = mediator_config {
                settings
                    .integrations
                    .insert_config("adserver_mock", &config)
                    .expect("should insert mediator config");
            } else {
                settings.integrations.remove("adserver_mock");
            }
            let plan = Arc::new(compile_auction_plan(&settings).expect("should compile plan"));

            let error = match build_orchestrator_with_plan(plan, &settings) {
                Ok(_) => panic!("should require enabled mediator registration"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("adserver_mock"));
        }
    }

    #[test]
    fn configured_mediator_builds_when_exact_registration_is_enabled() {
        let mut settings = create_test_settings();
        settings.auction.mediator = Some("adserver_mock".to_string());
        settings
            .integrations
            .insert_config(
                "adserver_mock",
                &serde_json::json!({
                    "enabled": true,
                    "endpoint": "https://mediator.example/mediate"
                }),
            )
            .expect("should insert mediator config");
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile plan"));

        build_orchestrator_with_plan(plan, &settings)
            .expect("should build with enabled exact mediator registration");
    }

    #[test]
    fn cloudflare_and_spin_reject_multi_provider_plans_before_runtime_construction() {
        let mut settings = create_test_settings();
        settings.auction.enabled = true;
        settings.auction.providers =
            AuctionConfig::legacy_provider_map(&["provider-a", "provider-b"]);
        let plan = compile_auction_plan(&settings).expect("should compile target-independent plan");

        for target in [
            crate::platform::AuctionTargetId::Cloudflare,
            crate::platform::AuctionTargetId::Spin,
        ] {
            let error = plan
                .validate_for_target(target)
                .expect_err("should reject unsupported multi-provider fanout");
            assert!(
                error
                    .to_string()
                    .contains("does not support concurrent provider fanout")
            );
        }
    }

    #[test]
    fn aps_profile_registers_renderer_without_browser_aps_config() {
        let mut settings = create_test_settings();
        settings.auction.providers = std::collections::BTreeMap::from([(
            "aps-main".parse().expect("should parse APS provider ID"),
            ProviderConfig {
                protocol: "openrtb-2.6".to_string(),
                profile: "aps".to_string(),
                endpoint: "https://aps.example/e/pb/bid".to_string(),
                timeout_ms: None,
                routing: RoutingMode::AllEligible,
                notifications: NotificationConfig::default(),
                profile_config: serde_json::json!({"account_id":"example-account"}),
            },
        )]);
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile APS plan"));
        let registry = IntegrationRegistry::with_plan(&settings, plan)
            .expect("should build APS renderer registry");

        assert!(registry.has_route(&http::Method::GET, "/integrations/aps/renderer"));
    }
}
