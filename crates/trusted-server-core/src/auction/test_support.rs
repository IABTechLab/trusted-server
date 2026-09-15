use std::collections::HashMap;
use std::sync::LazyLock;

use edgezero_core::body::Body as EdgeBody;
use http::Request;
use serde_json::json;

use super::AuctionContext;
use crate::auction::types::{
    AdFormat, AdSlot, AuctionRequest, DeviceInfo, MediaType, PublisherInfo, UserInfo,
};
use crate::consent::ConsentContext;
use crate::geo::GeoInfo;
use crate::openrtb::{Eid, Uid};
use crate::platform::{RuntimeServices, test_support::noop_services};
use crate::settings::Settings;

static TEST_SERVICES: LazyLock<RuntimeServices> = LazyLock::new(noop_services);

pub(crate) fn create_test_auction_context<'a>(
    settings: &'a Settings,
    request: &'a Request<EdgeBody>,
    timeout_ms: u32,
) -> AuctionContext<'a> {
    let services: &'static RuntimeServices = &TEST_SERVICES;
    AuctionContext {
        settings,
        request,
        timeout_ms,
        transport_timeout_ms: timeout_ms,
        provider_responses: None,
        services,
    }
}

/// Build canonical request facts shared by the PBS and APS Stage 1 wire goldens.
///
/// The supported and unsupported formats deliberately exercise each profile's
/// existing filtering and field-ownership policy. `trustedServer` bidder
/// parameters are included to pin that PBS consumes them while APS ignores
/// them.
pub(crate) fn canonical_parity_auction_request() -> AuctionRequest {
    AuctionRequest {
        id: "fictional-auction".to_string(),
        slots: vec![AdSlot {
            id: "fictional-slot".to_string(),
            formats: vec![
                AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                },
                AdFormat {
                    media_type: MediaType::Video,
                    width: 640,
                    height: 480,
                },
                AdFormat {
                    media_type: MediaType::Banner,
                    width: u32::MAX,
                    height: 90,
                },
                AdFormat {
                    media_type: MediaType::Banner,
                    width: 728,
                    height: 90,
                },
            ],
            floor_price: Some(1.0),
            targeting: HashMap::new(),
            bidders: HashMap::from([(
                "trustedServer".to_string(),
                json!({
                    "bidderParams": {
                        "exampleBidder": { "placement": "fictional-placement" }
                    }
                }),
            )]),
        }],
        publisher: PublisherInfo {
            domain: "publisher.example".to_string(),
            page_url: Some("https://publisher.example/article".to_string()),
        },
        user: UserInfo {
            id: Some("fictional-user".to_string()),
            consent: Some(ConsentContext {
                gdpr_applies: true,
                raw_tc_string: Some("fictional-tcf".to_string()),
                raw_us_privacy: Some("1YNN".to_string()),
                raw_gpp_string: Some("fictional-gpp".to_string()),
                gpp_section_ids: Some(vec![2, 6]),
                raw_ac_string: Some("fictional-ac".to_string()),
                ..Default::default()
            }),
            eids: Some(vec![Eid {
                source: "identity.example".to_string(),
                uids: vec![Uid {
                    id: "fictional-uid".to_string(),
                    atype: Some(1),
                    ext: None,
                }],
            }]),
        },
        device: Some(DeviceInfo {
            user_agent: Some("Fictional Browser".to_string()),
            ip: Some("192.0.2.10".to_string()),
            geo: Some(GeoInfo {
                city: "Example City".to_string(),
                country: "US".to_string(),
                continent: "NA".to_string(),
                latitude: 12.34,
                longitude: 56.78,
                metro_code: 501,
                region: Some("CA".to_string()),
                asn: None,
            }),
        }),
        site: None,
        context: HashMap::new(),
    }
}

/// One `[demand.<name>]` table naming an implementation, for tests that need a
/// compiled plan.
pub(crate) fn demand_table(
    implementation: &str,
    endpoint: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut table = serde_json::Map::from_iter([
        ("implementation".to_string(), json!(implementation)),
        ("endpoint".to_string(), json!(endpoint)),
    ]);
    if implementation == "aps" {
        table.insert("account_id".to_string(), json!("example-account"));
    }
    table
}

/// An `[demand]` table selecting every name given, in the order given.
pub(crate) fn demand_selection(
    tables: Vec<(&str, serde_json::Map<String, serde_json::Value>)>,
) -> crate::provider_table::ProviderList {
    let selected = tables
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect::<Vec<_>>();
    let tables = tables
        .into_iter()
        .map(|(name, table)| (name.to_string(), table))
        .collect::<std::collections::BTreeMap<_, _>>();
    crate::provider_table::ProviderList::new(selected, tables)
}

/// A plan configuration selecting the named demand tables, with every
/// implementation the built-in builders register.
pub(crate) fn plan_config(
    tables: Vec<(&str, serde_json::Map<String, serde_json::Value>)>,
) -> crate::auction::plan::AuctionPlanConfig {
    let builders = crate::integrations::all_builders(&[]).collect::<Vec<_>>();
    crate::auction::plan::AuctionPlanConfig {
        timeout_ms: 1000,
        demand: demand_selection(tables),
        demand_implementations: builders
            .iter()
            .filter_map(crate::integrations::IntegrationBuilder::demand)
            .collect(),
        adserver_implementations: builders
            .iter()
            .filter_map(crate::integrations::IntegrationBuilder::adserver)
            .collect(),
        ..crate::auction::plan::AuctionPlanConfig::default()
    }
}


/// A `[demand]` selection of ordinary `OpenRTB` sources under the names given,
/// each taking every eligible slot.
pub(crate) fn demand_named(names: &[&str]) -> crate::provider_table::ProviderList {
    demand_selection(
        names
            .iter()
            .map(|name| {
                let mut table =
                    demand_table("openrtb", &format!("https://{name}.example/openrtb2/auction"));
                table.insert("routing".to_string(), json!("all_eligible"));
                (*name, table)
            })
            .collect(),
    )
}

/// The legacy test orchestrator's configuration for these settings, carrying
/// the demand source names `[demand] provider` selects.
///
/// Production compiles its sources from the plan instead, so this exists only
/// so the parity tests can drive the pre-plan orchestrator.
pub(crate) fn legacy_auction_config(settings: &Settings) -> crate::auction::AuctionConfig {
    let mut config = settings.auction.clone();
    config.provider_names = settings
        .demand
        .selected()
        .into_iter()
        .map(str::to_string)
        .collect();
    config
}
