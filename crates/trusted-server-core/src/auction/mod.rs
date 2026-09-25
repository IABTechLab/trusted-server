//! Auction orchestration across the demand sources and the ad server a
//! deployment selects.
//!
//! `[demand] provider` selects the demand sources and `[adserver] provider`
//! the optional ad server. Their implementations are registered by
//! integrations through [`demand`], so this module names no vendor.

use error_stack::Report;

use crate::error::TrustedServerError;
use crate::integrations::IntegrationBuilder;
use crate::settings::Settings;
use std::sync::Arc;

pub mod config;
pub mod context;
pub mod demand;
pub mod endpoints;
pub mod formats;
pub(crate) mod openrtb;
pub mod orchestrator;
pub mod plan;
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
    AdServerPlan, AuctionPlan, BidderId, BidderRouteConfig, NotificationConfig, ProviderId,
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

/// Compile the canonical target-independent auction plan for [`Settings`] with
/// the implementations the built-in integrations register.
///
/// This is the settings-to-plan boundary used by deploy validation, adapter
/// startup and operator tooling. Global request signing remains owned by
/// [`Settings`] and is copied into compiler input only at this boundary.
///
/// # Errors
///
/// Returns an error when the `[demand]`, `[adserver]`, bidder route or signing
/// configuration is invalid.
pub fn compile_auction_plan(
    settings: &Settings,
) -> Result<AuctionPlan, Report<TrustedServerError>> {
    compile_auction_plan_with(settings, &[])
}

/// Compile the auction plan with the implementations the built-in integrations
/// register followed by those the `extra` builders register, the builders an
/// adapter or a vendor crate supplies.
///
/// # Errors
///
/// Returns an error when the `[demand]`, `[adserver]`, bidder route or signing
/// configuration is invalid, including a selected name whose implementation no
/// builder registers.
pub fn compile_auction_plan_with(
    settings: &Settings,
    extra: &[IntegrationBuilder],
) -> Result<AuctionPlan, Report<TrustedServerError>> {
    let builders = crate::integrations::all_builders(extra).collect::<Vec<_>>();
    AuctionPlan::compile(plan::AuctionPlanConfig {
        timeout_ms: settings.auction.timeout_ms,
        demand: settings.demand.clone(),
        adserver: settings.adserver.clone(),
        bidders: settings.auction.bidders.clone(),
        request_signing: settings.request_signing.clone(),
        demand_implementations: builders
            .iter()
            .filter_map(IntegrationBuilder::demand)
            .collect(),
        adserver_implementations: builders
            .iter()
            .filter_map(IntegrationBuilder::adserver)
            .collect(),
    })
    .map(|plan| plan.with_enabled(settings.auction.enabled))
}

/// Build a new auction orchestrator from one shared compiled plan.
///
/// The demand sources come from the plan, and so does the ad server, which is
/// built here from its implementation and settings. Callers can reuse the
/// returned [`AuctionOrchestrator`] across requests.
///
/// # Errors
///
/// Returns an error when the selected ad server cannot be built from its
/// settings.
pub fn build_orchestrator_with_plan(
    plan: Arc<AuctionPlan>,
) -> Result<AuctionOrchestrator, Report<TrustedServerError>> {
    log::info!("Building plan-backed auction orchestrator");

    let adserver = plan
        .adserver()
        .map(|adserver| (adserver.implementation.build)(adserver.id.as_str(), &adserver.settings))
        .transpose()?;
    let orchestrator = AuctionOrchestrator::from_plan(plan, adserver);

    log::info!(
        "Auction orchestrator built with {} demand sources",
        orchestrator.provider_count()
    );

    Ok(orchestrator)
}

/// Test convenience constructor that compiles a plan before construction.
///
/// # Errors
///
/// Returns an error when plan compilation or ad server construction fails.
#[cfg(test)]
pub fn build_orchestrator(
    settings: &Settings,
) -> Result<AuctionOrchestrator, Report<TrustedServerError>> {
    let plan = Arc::new(compile_auction_plan(settings)?);
    build_orchestrator_with_plan(plan)
}

#[cfg(test)]
mod plan_sharing_tests {
    use super::*;
    use crate::auction::test_support::{demand_named, demand_selection, demand_table};
    use crate::integrations::IntegrationRegistry;
    use crate::provider_table::ProviderChoice;
    use crate::test_support::tests::create_test_settings;
    use serde_json::{Map, json};
    use std::collections::BTreeMap;

    /// An `[adserver]` table selecting one name with the settings given.
    fn adserver(name: &str, settings: Map<String, serde_json::Value>) -> ProviderChoice {
        ProviderChoice::new(
            Some(name.to_string()),
            BTreeMap::from([(name.to_string(), settings)]),
        )
    }

    #[test]
    fn orchestrator_and_registry_share_the_compiled_plan_allocation() {
        let settings = create_test_settings();
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile auction plan"));
        let orchestrator =
            build_orchestrator_with_plan(Arc::clone(&plan)).expect("should build orchestrator");
        let registry = IntegrationRegistry::with_plan(&settings, Arc::clone(&plan))
            .expect("should build integration registry");

        assert!(orchestrator.shares_plan(&plan));
        assert!(registry.shares_plan(&plan));
    }

    #[test]
    fn an_ad_server_this_build_does_not_have_fails_the_plan() {
        let mut settings = create_test_settings();
        settings.adserver =
            ProviderChoice::new(Some("fictional_adserver".to_string()), BTreeMap::new());
        let error = compile_auction_plan(&settings)
            .expect_err("should refuse an ad server no builder registers");
        assert!(
            error.to_string().contains("fictional_adserver"),
            "should name the ad server: {error:?}"
        );
    }

    #[test]
    fn the_selected_ad_server_is_built_from_its_own_table() {
        let mut settings = create_test_settings();
        settings.adserver = adserver(
            "adserver_mock",
            Map::from_iter([(
                "endpoint".to_string(),
                json!("https://adserver.example/mediate"),
            )]),
        );
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile plan"));

        build_orchestrator_with_plan(plan).expect("should build the selected ad server");
    }

    #[test]
    fn an_ad_server_table_with_no_endpoint_fails_the_plan() {
        let mut settings = create_test_settings();
        settings.adserver = adserver("adserver_mock", Map::new());
        let error = compile_auction_plan(&settings)
            .expect_err("should refuse an ad server with no endpoint");
        assert!(
            format!("{error:?}").contains("endpoint"),
            "should say an endpoint is needed: {error:?}"
        );
    }

    #[test]
    fn cloudflare_and_spin_reject_multi_provider_plans_before_runtime_construction() {
        let mut settings = create_test_settings();
        settings.auction.enabled = true;
        settings.demand = demand_named(&["provider_a", "provider_b"]);
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
    fn an_aps_demand_source_registers_the_renderer_with_no_integration_table() {
        let mut settings = create_test_settings();
        let mut table = demand_table("aps", "https://aps.example/e/pb/bid");
        table.insert("routing".to_string(), json!("all_eligible"));
        settings.demand = demand_selection(vec![("aps_main", table)]);
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile APS plan"));
        let registry = IntegrationRegistry::with_plan(&settings, plan)
            .expect("should build APS renderer registry");

        assert!(registry.has_route(&http::Method::GET, "/integrations/aps/renderer"));
    }

    #[test]
    fn two_aps_sources_that_disagree_on_rendering_are_refused() {
        let mut settings = create_test_settings();
        let mut publisher_native = demand_table("aps", "https://aps.example/e/pb/bid");
        publisher_native.insert("rendering_mode".to_string(), json!("publisher_native"));
        settings.demand = demand_selection(vec![
            (
                "aps_one",
                demand_table("aps", "https://aps.example/e/pb/bid"),
            ),
            ("aps_two", publisher_native),
        ]);
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile APS plan"));
        let error = match IntegrationRegistry::with_plan(&settings, plan) {
            Ok(_) => panic!("should refuse two rendering modes"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("rendering_mode"),
            "should name the setting that disagrees: {error:?}"
        );
    }
}
