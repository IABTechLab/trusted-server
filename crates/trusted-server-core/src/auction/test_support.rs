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
use crate::consent::{ConsentContext, SignalRecord, SignalStatus};
use crate::ec::eids::{Eid, Uid};
use crate::geo::GeoInfo;
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
            consent: Some(
                ConsentContext::default()
                    .with_record(
                        SignalRecord::new("tcf", SignalStatus::Decoded)
                            .with_value("fictional-tcf")
                            .with_applies(true),
                    )
                    .with_record(
                        SignalRecord::new("us_privacy", SignalStatus::Decoded).with_value("1YNN"),
                    )
                    .with_record(
                        SignalRecord::new("gpp", SignalStatus::Decoded)
                            .with_value("fictional-gpp")
                            .with_sections(vec![2, 6]),
                    ),
            ),
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
            attributes: None,
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

/// Builds the request the first demand source of `plan` would send once
/// `request` is routed to it, or `None` when the source has nothing to ask
/// for.
///
/// A test in an implementation's own crate checks what its source builds
/// through this. The signature, when `signer` is given, is made over
/// [`golden_signing_params`].
///
/// # Errors
///
/// Returns the error the source's request building returns.
pub fn build_for_first_source(
    plan: &crate::auction::AuctionPlan,
    request: AuctionRequest,
    inbound: &Request<EdgeBody>,
    transport_timeout_ms: u32,
    signer: Option<&crate::request_signing::RequestSigner>,
) -> Result<
    Option<crate::auction::demand::OutboundRequest>,
    error_stack::Report<crate::error::TrustedServerError>,
> {
    let routed = crate::auction::routing::route_auction(request, inbound, plan, None);
    plan.providers()[0].demand.build_request(
        &routed.inputs()[0],
        routed.transport(),
        transport_timeout_ms,
        &crate::auction::demand::RequestFinalization {
            signer,
            signing_params: golden_signing_params(),
        },
    )
}

/// Routes `request` through `plan` as core does for an auction, with the
/// client address the platform attested, if any.
///
/// A test in a protocol's own crate builds a request from the routed input
/// and transport this returns.
#[must_use]
pub fn route_for_test(
    plan: &crate::auction::AuctionPlan,
    request: AuctionRequest,
    inbound: &Request<EdgeBody>,
    attested_client_ip: Option<std::net::IpAddr>,
) -> crate::auction::demand::RoutedAuction {
    crate::auction::routing::route_auction(request, inbound, plan, attested_client_ip)
}

/// Routes `request` through `plan` with the server-owned routes a page's
/// slot templates would give, one list of provider names per slot in the
/// request's order.
///
/// # Panics
///
/// Panics when a route names a provider id that does not parse.
#[must_use]
pub fn route_with_trusted_routes_for_test(
    plan: &crate::auction::AuctionPlan,
    request: AuctionRequest,
    inbound: &Request<EdgeBody>,
    routes_by_slot: &[&[&str]],
) -> crate::auction::demand::RoutedAuction {
    let routes = crate::auction::routing::TrustedProviderRoutes::new(
        routes_by_slot
            .iter()
            .map(|slot| {
                slot.iter()
                    .map(|provider| provider.parse().expect("should parse provider ID"))
                    .collect()
            })
            .collect(),
    );
    crate::auction::routing::route_auction_with_trusted_routes(
        request, inbound, plan, None, &routes,
    )
}

/// Routes `request` through `plan` and returns the input its first demand
/// source receives, or `None` when no source takes a slot.
///
/// A test in an implementation's own crate hands this input to its own
/// response reader.
#[must_use]
pub fn route_to_first_source(
    plan: &crate::auction::AuctionPlan,
    request: AuctionRequest,
    inbound: &Request<EdgeBody>,
) -> Option<crate::auction::demand::ProviderAuctionInput> {
    crate::auction::routing::route_auction(request, inbound, plan, None)
        .inputs()
        .first()
        .cloned()
}

/// The transport headers a routed auction admits from `inbound`, with the
/// attested client address it carries to every source's outbound request.
///
/// A test in an implementation's own crate checks what it forwards upstream
/// through this.
#[must_use]
pub fn routed_transport(
    plan: &crate::auction::AuctionPlan,
    request: AuctionRequest,
    inbound: &Request<EdgeBody>,
    attested_client_ip: Option<std::net::IpAddr>,
) -> (
    crate::auction::demand::TransportHeaders,
    Option<std::net::IpAddr>,
) {
    let routed = crate::auction::routing::route_auction(request, inbound, plan, attested_client_ip);
    (
        routed.transport_headers().clone(),
        routed.attested_client_ip(),
    )
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
    let provider = crate::auction::provider::RoutedDemandProvider::new(plan.providers()[0].clone());
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

/// A `[demand]` selection of sources under the names given, each running
/// `implementation` at an endpoint of its own and taking every eligible slot.
pub fn demand_named(implementation: &str, names: &[&str]) -> crate::provider_table::ProviderList {
    demand_selection(
        names
            .iter()
            .map(|name| {
                let mut table = demand_table(
                    implementation,
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

/// The plain JSON the two stand-in demand implementations speak, so core's
/// own tests see what a source is given and what core does with its answer
/// without naming any protocol.
///
/// The request carries the auction id, the publisher's domain, the budget,
/// the identifiers, one entry per slot with its id, its banner formats, its
/// floor and the stand-in's slot data, the stand-in's request data, and the
/// signature when the request is signed. The response is read as
/// `{"bids": [...]}`, each bid naming its `slot`, `seat`, `id`, `price`,
/// `creative`, `w`, `h`, `nurl` and `burl`, and admitted by core's rules.
#[cfg(test)]
pub(crate) mod fixture_wire {
    use std::collections::{HashMap, HashSet};

    use error_stack::{Report, ResultExt as _};
    use http::{HeaderMap, HeaderValue, StatusCode, header};
    use serde_json::{Map, Value, json};

    use crate::auction::admission::{
        BidRejectionReason, ResponseAdmissionDiagnostics, build_bid_dimension_index,
        parse_optional_bid_dimension, resolve_bid_dimensions,
    };
    use crate::auction::demand::{
        DemandResponse, DemandTransport, OutboundRequest, ProviderAuctionInput, ProviderSlotInput,
        RequestFinalization,
    };
    use crate::auction::types::{AuctionResponse, Bid};
    use crate::error::TrustedServerError;
    use crate::platform::PlatformResponse;

    const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

    /// Builds the stand-in request, with `slot_data` deciding what each slot
    /// carries and whether it is sent at all, and `request_data` the
    /// stand-in's own request object.
    pub(crate) fn build(
        input: &ProviderAuctionInput,
        transport: DemandTransport<'_>,
        timeout_ms: u32,
        finalization: &RequestFinalization<'_>,
        request_data: Option<Value>,
        slot_data: impl Fn(&ProviderSlotInput) -> Option<Value>,
    ) -> Result<Option<OutboundRequest>, Report<TrustedServerError>> {
        let common = input.common_request();
        let mut slot_ids = HashSet::new();
        let mut slots = Vec::new();
        for slot in input.slots() {
            let Some(data) = slot_data(slot) else {
                continue;
            };
            let mut entry = Map::from_iter([
                ("id".to_string(), json!(slot.slot().id)),
                (
                    "formats".to_string(),
                    json!(
                        slot.slot()
                            .formats
                            .iter()
                            .map(|format| [format.width, format.height])
                            .collect::<Vec<_>>()
                    ),
                ),
                ("ext".to_string(), data),
            ]);
            if let Some(floor) = slot.slot().floor_price {
                entry.insert("floor".to_string(), json!(floor));
            }
            slot_ids.insert(slot.slot().id.clone());
            slots.push(Value::Object(entry));
        }
        if slots.is_empty() {
            return Ok(None);
        }
        let mut request = Map::from_iter([
            ("auction".to_string(), json!(common.id)),
            ("publisher".to_string(), json!(common.publisher.domain)),
            ("timeout_ms".to_string(), json!(timeout_ms)),
            ("slots".to_string(), Value::Array(slots)),
        ]);
        if let Some(dnt) = transport.dnt {
            request.insert("dnt".to_string(), json!(dnt));
        }
        if common.user.id.is_some() || common.user.eids.is_some() {
            request.insert(
                "user".to_string(),
                json!({"id": common.user.id, "eids": common.user.eids}),
            );
        }
        if let Some(data) = request_data {
            request.insert("ext".to_string(), data);
        }
        if let Some(signer) = finalization.signer {
            let signature = signer.sign_request(&finalization.signing_params)?;
            request.insert(
                "signature".to_string(),
                json!({
                    "kid": signer.kid,
                    "signature": signature,
                    "ts": finalization.signing_params.timestamp,
                    "host": finalization.signing_params.request_host,
                    "scheme": finalization.signing_params.request_scheme,
                }),
            );
        }
        let body = serde_json::to_vec(&request).change_context(TrustedServerError::Auction {
            message: "stand-in request serialization failed".to_string(),
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        Ok(Some(OutboundRequest {
            body,
            headers,
            slot_ids,
            captured: None,
        }))
    }

    /// Reads the stand-in response. A `204` is a no-bid, any other
    /// non-success status or unreadable JSON is an error response, and a
    /// body over the size limit is an `Err`.
    pub(crate) async fn read(
        context: DemandResponse<'_>,
        response: PlatformResponse,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let provider_id = context.provider_id;
        let response_time_ms = context.response_time_ms;
        let response = response.response;
        let status = response.status();
        if status == StatusCode::NO_CONTENT {
            return Ok(AuctionResponse::no_bid(provider_id, response_time_ms));
        }
        if !status.is_success() {
            return Ok(AuctionResponse::error(provider_id, response_time_ms)
                .with_metadata("error_type", json!("http_status"))
                .with_metadata("http_status", json!(status.as_u16())));
        }
        let body = response
            .into_body()
            .into_bytes_bounded(MAX_RESPONSE_BYTES)
            .await
            .change_context(TrustedServerError::Auction {
                message: format!("Provider {provider_id} response body failed"),
            })?;
        let value: Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                return Ok(AuctionResponse::error(provider_id, response_time_ms)
                    .with_metadata("error_type", json!("parse_response")));
            }
        };
        let Some(entries) = value.as_object() else {
            return Ok(AuctionResponse::error(provider_id, response_time_ms)
                .with_metadata("error_type", json!("parse_response")));
        };
        let dimensions_by_slot = build_bid_dimension_index(context.input);
        let mut diagnostics = ResponseAdmissionDiagnostics::default();
        let mut bids = Vec::new();
        for entry in entries
            .get("bids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match read_bid(entry, &dimensions_by_slot) {
                Ok(bid) => bids.push(bid),
                Err(reason) => diagnostics.record(reason),
            }
        }
        let mut parsed = if bids.is_empty() {
            AuctionResponse::no_bid(provider_id, response_time_ms)
        } else {
            AuctionResponse::success(provider_id, bids, response_time_ms)
        };
        diagnostics.attach_to(&mut parsed);
        Ok(parsed)
    }

    fn read_bid(
        entry: &Value,
        dimensions_by_slot: &crate::auction::admission::BidDimensionIndex,
    ) -> Result<Bid, BidRejectionReason> {
        let text = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let slot_id = text("slot").ok_or(BidRejectionReason::InvalidBid)?;
        let width = parse_optional_bid_dimension(entry, "w")?;
        let height = parse_optional_bid_dimension(entry, "h")?;
        let (width, height) = resolve_bid_dimensions(dimensions_by_slot, &slot_id, width, height)?;
        let price = entry
            .get("price")
            .and_then(Value::as_f64)
            .filter(|price| price.is_finite() && *price >= 0.0)
            .ok_or(BidRejectionReason::InvalidBid)?;
        let creative = text("creative").ok_or(BidRejectionReason::InvalidBid)?;
        let seat = text("seat");
        Ok(Bid {
            slot_id,
            price: Some(price),
            currency: "USD".to_string(),
            creative: Some(creative),
            adomain: None,
            bidder: seat.clone().unwrap_or_else(|| "unknown".to_string()),
            returned_seat: seat,
            width,
            height,
            nurl: text("nurl"),
            burl: text("burl"),
            bid_id: text("id"),
            ad_id: None,
            creative_id: None,
            renderer: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        })
    }
}

/// A stand-in demand implementation for core's own tests of what core does
/// for a source that departs from the plain one. It serves stored requests,
/// forbids all-eligible routing, takes bidder parameters and completes an
/// endpoint that names a host alone.
///
/// It writes each slot's usable bidder parameters under `ext.fixture.bidder`,
/// or the slot id under `ext.fixture.stored` where the slot allows a stored
/// request, and leaves out a slot that has neither.
#[cfg(test)]
pub(crate) mod demand_fixture {
    use core::any::Any;
    use std::sync::Arc;

    use async_trait::async_trait;
    use error_stack::Report;
    use serde::Deserialize;
    use serde_json::{Map, Value, json};
    use url::Url;

    use super::fixture_wire;
    use crate::auction::demand::{
        CompiledDemand, DemandImplementation, DemandResponse, DemandTimeoutDefault,
        DemandTransport, OutboundRequest, ProviderAuctionInput, RequestFinalization,
    };
    use crate::auction::types::AuctionResponse;
    use crate::error::TrustedServerError;
    use crate::platform::PlatformResponse;

    /// The name an `implementation` line gives the stand-in.
    pub(crate) const MODULE: &str = "auction.fixture";

    /// The path the stand-in completes a bare host with.
    pub(crate) const AUCTION_PATH: &str = "/fixture/auction";

    /// The stand-in demand implementation.
    pub(crate) static DEMAND: DemandImplementation = DemandImplementation {
        id: MODULE,
        default_timeout: DemandTimeoutDefault::Fixed(1000),
        allows_all_eligible: false,
        serves_stored_requests: true,
        canonicalize_endpoint,
        compile,
    };

    /// One compiled stand-in source.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct FixtureDemand;

    #[derive(Debug, Deserialize, Default)]
    #[serde(deny_unknown_fields)]
    struct FixtureSettings {}

    fn compile(
        settings: &Map<String, Value>,
    ) -> Result<Arc<dyn CompiledDemand>, Report<TrustedServerError>> {
        FixtureSettings::deserialize(Value::Object(settings.clone())).map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("invalid `{MODULE}` settings: {error}"),
            })
        })?;
        Ok(Arc::new(FixtureDemand))
    }

    fn canonicalize_endpoint(endpoint: &mut Url) -> Result<(), String> {
        if matches!(endpoint.path(), "" | "/") {
            endpoint.set_path(AUCTION_PATH);
        }
        Ok(())
    }

    #[async_trait(?Send)]
    impl CompiledDemand for FixtureDemand {
        fn build_request(
            &self,
            input: &ProviderAuctionInput,
            transport: DemandTransport<'_>,
            timeout_ms: u32,
            finalization: &RequestFinalization<'_>,
        ) -> Result<Option<OutboundRequest>, Report<TrustedServerError>> {
            fixture_wire::build(
                input,
                transport,
                timeout_ms,
                finalization,
                Some(json!({"fixture": {}})),
                |slot| {
                    let bidder = slot
                        .bidder_params()
                        .iter()
                        .filter(|(_, params)| {
                            params.as_object().is_some_and(|params| !params.is_empty())
                        })
                        .map(|(bidder, params)| (bidder.as_str().to_string(), params.clone()))
                        .collect::<Map<_, _>>();
                    let mut fixture = Map::new();
                    if !bidder.is_empty() {
                        fixture.insert("bidder".to_string(), Value::Object(bidder));
                    } else if slot.allows_stored_fallback() {
                        fixture.insert("stored".to_string(), json!(slot.slot().id));
                    }
                    (!fixture.is_empty()).then(|| json!({"fixture": fixture}))
                },
            )
        }

        async fn parse_response(
            &self,
            context: DemandResponse<'_>,
            response: PlatformResponse,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            fixture_wire::read(context, response).await
        }

        fn consumes_bidder_params(&self) -> bool {
            true
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }
}

/// A stand-in for the plainest demand implementation there can be, which
/// core's own tests of the plan compiler and the provider name where they
/// need a source.
///
/// It takes every eligible slot, inherits the auction's timeout, leaves its
/// endpoint as written and ignores bidder parameters. Its table may set
/// `request_ext` and `slot_ext`, which it writes into the request and into
/// every slot as given, so a test has a source that adds data of its own.
#[cfg(test)]
pub(crate) mod plain_fixture {
    use core::any::Any;
    use std::sync::Arc;

    use async_trait::async_trait;
    use error_stack::Report;
    use serde::Deserialize;
    use serde_json::{Map, Value};

    use super::fixture_wire;
    use crate::auction::demand::{
        CompiledDemand, DemandImplementation, DemandResponse, DemandTimeoutDefault,
        DemandTransport, OutboundRequest, ProviderAuctionInput, RequestFinalization,
        accept_endpoint,
    };
    use crate::auction::types::AuctionResponse;
    use crate::error::TrustedServerError;
    use crate::platform::PlatformResponse;

    /// The name an `implementation` line gives the stand-in.
    pub(crate) const MODULE: &str = "auction.plain-fixture";

    /// The stand-in demand implementation.
    pub(crate) static DEMAND: DemandImplementation = DemandImplementation {
        id: MODULE,
        default_timeout: DemandTimeoutDefault::Auction,
        allows_all_eligible: true,
        serves_stored_requests: false,
        canonicalize_endpoint: accept_endpoint,
        compile,
    };

    /// One compiled stand-in source.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct PlainDemand {
        request_ext: Map<String, Value>,
        slot_ext: Map<String, Value>,
    }

    #[derive(Debug, Deserialize, Default)]
    #[serde(deny_unknown_fields)]
    struct PlainSettings {
        #[serde(default)]
        request_ext: Map<String, Value>,
        #[serde(default)]
        slot_ext: Map<String, Value>,
    }

    fn compile(
        settings: &Map<String, Value>,
    ) -> Result<Arc<dyn CompiledDemand>, Report<TrustedServerError>> {
        let settings =
            PlainSettings::deserialize(Value::Object(settings.clone())).map_err(|error| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("invalid `{MODULE}` settings: {error}"),
                })
            })?;
        Ok(Arc::new(PlainDemand {
            request_ext: settings.request_ext,
            slot_ext: settings.slot_ext,
        }))
    }

    #[async_trait(?Send)]
    impl CompiledDemand for PlainDemand {
        fn build_request(
            &self,
            input: &ProviderAuctionInput,
            transport: DemandTransport<'_>,
            timeout_ms: u32,
            finalization: &RequestFinalization<'_>,
        ) -> Result<Option<OutboundRequest>, Report<TrustedServerError>> {
            fixture_wire::build(
                input,
                transport,
                timeout_ms,
                finalization,
                (!self.request_ext.is_empty()).then(|| Value::Object(self.request_ext.clone())),
                |_| Some(Value::Object(self.slot_ext.clone())),
            )
        }

        async fn parse_response(
            &self,
            context: DemandResponse<'_>,
            response: PlatformResponse,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            fixture_wire::read(context, response).await
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }
}
