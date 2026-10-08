//! Helpers for tests of the auction, in core and in an implementation's own
//! crate.

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

/// An auction context over services that do nothing.
pub fn create_test_auction_context<'a>(
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

/// Build canonical request facts shared by the implementations' wire goldens.
///
/// The supported and unsupported formats deliberately exercise each
/// implementation's filtering and field-ownership policy. `trustedServer`
/// bidder parameters are included to pin which implementation consumes them
/// and which ignores them.
pub fn canonical_parity_auction_request() -> AuctionRequest {
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

/// The signing input the request goldens were captured with.
pub fn golden_signing_params() -> crate::request_signing::SigningParams {
    crate::request_signing::SigningParams {
        request_id: "fictional-auction".to_string(),
        request_host: "publisher.example".to_string(),
        request_scheme: "https".to_string(),
        timestamp: 1_706_900_000,
    }
}

/// The one-source plan the request goldens were captured with: a source
/// called `fictional_provider`, running `implementation` with `settings`.
///
/// The source takes every eligible slot when `all_eligible` is set. Otherwise
/// the bidder `exampleBidder` is routed to it, which is how a source that
/// takes only routed bidders is reached.
pub fn golden_plan_config(
    implementation: &str,
    settings: serde_json::Value,
    all_eligible: bool,
    extra: &[crate::integrations::IntegrationBuilder],
) -> crate::auction::plan::AuctionPlanConfig {
    let mut table = demand_table(implementation, "https://exchange.example.test/openrtb");
    table.insert("timeout_ms".to_string(), json!(321));
    if all_eligible {
        table.insert("routing".to_string(), json!("all_eligible"));
    }
    if let serde_json::Value::Object(settings) = settings {
        table.extend(settings);
    }
    let mut config = plan_config_with(vec![("fictional_provider", table)], extra);
    config.timeout_ms = 321;
    if !all_eligible {
        config.bidders.insert(
            "exampleBidder".parse().expect("should parse bidder ID"),
            crate::auction::plan::BidderRouteConfig {
                module: "fictional_provider"
                    .parse()
                    .expect("should parse provider ID"),
            },
        );
    }
    config
}

/// The inbound request the request goldens were captured with.
pub fn golden_inbound_request() -> Request<EdgeBody> {
    Request::builder()
        .uri("https://publisher.example/auction")
        .header(
            http::header::REFERER,
            "https://referrer.example/story?fictional=1",
        )
        .header(http::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .header("dnt", "1")
        .body(EdgeBody::empty())
        .expect("should build inbound request")
}

/// A signer over one fixed key, so a signed golden is the same on every run.
pub fn deterministic_signer() -> crate::request_signing::RequestSigner {
    use base64::Engine as _;

    use crate::platform::test_support::{
        HashMapConfigStore, HashMapSecretStore, NoopHttpClient,
        build_services_with_config_secret_and_http_client,
    };

    let config_data = HashMap::from([("current-kid".to_string(), "fictional-kid".to_string())]);
    let secret_data = HashMap::from([(
        "fictional-kid".to_string(),
        base64::engine::general_purpose::STANDARD
            .encode([7_u8; 32])
            .into_bytes(),
    )]);
    let services = build_services_with_config_secret_and_http_client(
        HashMapConfigStore::new(config_data),
        HashMapSecretStore::new(secret_data),
        std::sync::Arc::new(NoopHttpClient),
    );
    crate::request_signing::RequestSigner::from_services(&services)
        .expect("should load deterministic signer")
}

/// Builds the request the driver would send to the first demand source of
/// `plan` once `request` is routed to it, or `None` when the source keeps no
/// impression.
///
/// A test in an implementation's own crate checks what its field policy and
/// its extensions do to a request through this. The signature, when `signer`
/// is given, is made over [`golden_signing_params`].
///
/// # Errors
///
/// Returns the error the request builder returns.
pub fn build_for_first_source(
    plan: &crate::auction::AuctionPlan,
    request: AuctionRequest,
    inbound: &Request<EdgeBody>,
    transport_timeout_ms: u32,
    signer: Option<&crate::request_signing::RequestSigner>,
) -> Result<
    Option<crate::openrtb::OpenRtbRequest>,
    error_stack::Report<crate::error::TrustedServerError>,
> {
    use crate::auction::openrtb::{OpenRtbBuildOutcome, RequestFinalization, build_request};

    let routed = crate::auction::routing::route_auction(request, inbound, plan, None);
    let outcome = build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        transport_timeout_ms,
        &RequestFinalization {
            signer,
            signing_params: golden_signing_params(),
        },
    )?;
    Ok(match outcome {
        OpenRtbBuildOutcome::Ready(request) => Some(request),
        OpenRtbBuildOutcome::NoImpressions => None,
    })
}

/// Reads `response` as the first demand source of `plan` would, once
/// `request` is routed to it.
///
/// A test in an implementation's own crate checks how its implementation
/// answers through this, with no upstream.
///
/// # Errors
///
/// Returns the error the source's own reading returns.
pub async fn parse_as_first_source(
    plan: &crate::auction::AuctionPlan,
    request: AuctionRequest,
    response: crate::platform::PlatformResponse,
    response_time_ms: u64,
) -> Result<
    crate::auction::types::AuctionResponse,
    error_stack::Report<crate::error::TrustedServerError>,
> {
    let inbound = Request::new(EdgeBody::empty());
    let routed = crate::auction::routing::route_auction(request, &inbound, plan, None);
    let provider =
        crate::auction::provider::GenericOpenRtbProvider::new(plan.providers()[0].clone());
    let state = provider.parse_state_for_test(routed.inputs()[0].clone());
    provider
        .parse_response_with_state(response, response_time_ms, Some(state.as_ref()))
        .await
}

/// One `[demand.<name>]` table naming an implementation, for tests that need a
/// compiled plan.
pub fn demand_table(
    implementation: &str,
    endpoint: &str,
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        ("implementation".to_string(), json!(implementation)),
        ("endpoint".to_string(), json!(endpoint)),
    ])
}

/// An `[demand]` table selecting every name given, in the order given.
pub fn demand_selection(
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
pub fn plan_config(
    tables: Vec<(&str, serde_json::Map<String, serde_json::Value>)>,
) -> crate::auction::plan::AuctionPlanConfig {
    plan_config_with(tables, &[])
}

/// A plan configuration selecting the named demand tables, with every
/// implementation the built-in builders register followed by the ones
/// `extra` registers, which is how a crate's own tests reach its
/// implementation.
pub fn plan_config_with(
    tables: Vec<(&str, serde_json::Map<String, serde_json::Value>)>,
    extra: &[crate::integrations::IntegrationBuilder],
) -> crate::auction::plan::AuctionPlanConfig {
    let builders = crate::integrations::all_builders(extra).collect::<Vec<_>>();
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
pub fn demand_named(names: &[&str]) -> crate::provider_table::ProviderList {
    demand_selection(
        names
            .iter()
            .map(|name| {
                let mut table = demand_table(
                    "auction-protocol.openrtb",
                    &format!("https://{name}.example/openrtb2/auction"),
                );
                table.insert("routing".to_string(), json!("all_eligible"));
                (*name, table)
            })
            .collect(),
    )
}

/// The legacy test orchestrator's configuration for these settings, carrying
/// the demand source names `[demand] modules` selects.
///
/// Production compiles its sources from the plan instead, so this exists only
/// so the parity tests can drive the pre-plan orchestrator.
#[cfg(test)]
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

/// A stand-in ad server implementation for core's own tests of the ad server
/// seam, selected with `[ad-server] module = "fixture"`.
///
/// It reads an `endpoint`, which has to be a URL, and an optional
/// `timeout_ms` from its table and refuses any other setting, as an
/// implementation of a vendor's does. Its provider answers every request
/// with no bid and calls nothing.
#[cfg(test)]
pub(crate) mod adserver_fixture {
    use std::sync::Arc;

    use async_trait::async_trait;
    use error_stack::Report;
    use serde::Deserialize;

    use crate::auction::demand::AdServerImplementation;
    use crate::auction::provider::{AuctionProvider, ProviderRequestOutcome};
    use crate::auction::types::{AuctionContext, AuctionRequest, AuctionResponse};
    use crate::error::TrustedServerError;
    use crate::platform::PlatformResponse;

    /// The name the stand-in is selected by, which `[ad-server]` shortens to
    /// `fixture`.
    pub(crate) const MODULE: &str = "ad-server.fixture";

    /// The stand-in ad server implementation.
    pub(crate) static ADSERVER: AdServerImplementation =
        AdServerImplementation { id: MODULE, build };

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FixtureSettings {
        endpoint: String,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u32,
    }

    fn default_timeout_ms() -> u32 {
        500
    }

    fn build(
        name: &str,
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Arc<dyn AuctionProvider>, Report<TrustedServerError>> {
        let settings: FixtureSettings = serde_json::from_value(serde_json::Value::Object(
            settings.clone(),
        ))
        .map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("invalid `{MODULE}` settings: {error}"),
            })
        })?;
        url::Url::parse(&settings.endpoint).map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("invalid `{MODULE}` endpoint: {error}"),
            })
        })?;
        Ok(Arc::new(FixtureAdServer::new(name, settings.timeout_ms)))
    }

    /// The stand-in's provider.
    pub(crate) struct FixtureAdServer {
        name: String,
        timeout_ms: u32,
    }

    impl FixtureAdServer {
        /// A provider called `name`.
        pub(crate) fn new(name: &str, timeout_ms: u32) -> Self {
            Self {
                name: name.to_owned(),
                timeout_ms,
            }
        }
    }

    #[async_trait(?Send)]
    impl AuctionProvider for FixtureAdServer {
        fn provider_name(&self) -> &str {
            &self.name
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            Ok(ProviderRequestOutcome::Immediate(AuctionResponse::no_bid(
                self.name.clone(),
                0,
            )))
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            Ok(AuctionResponse::no_bid(self.name.clone(), response_time_ms))
        }

        fn timeout_ms(&self) -> u32 {
            self.timeout_ms
        }
    }
}
