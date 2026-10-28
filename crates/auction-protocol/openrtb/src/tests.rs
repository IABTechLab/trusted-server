//! Tests of the driver over the plain implementation and over a stand-in
//! that departs from it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::str::FromStr as _;

use edgezero_core::body::Body as EdgeBody;
use http::{Request, header};
use serde_json::{Value, json};
use trusted_server_core::auction::demand::{
    CompiledDemand, DemandResponse, ProviderAuctionInput, RequestFinalization, RoutedAuction,
};
use trusted_server_core::auction::plan::{
    AuctionPlan, AuctionPlanConfig, BidderId, BidderRouteConfig, NotificationPolicy, ProviderId,
    ProviderPlan,
};
use trusted_server_core::auction::provider::apply_notification_policy;
use trusted_server_core::auction::test_support::{
    canonical_parity_auction_request, demand_table, deterministic_signer, parse_as_first_source,
    plan_config_with, route_for_test, route_with_trusted_routes_for_test,
};
use trusted_server_core::auction::types::{
    AdFormat, AdSlot, AuctionResponse, BidStatus, MediaType,
};
use trusted_server_core::consent::jurisdiction::Jurisdiction;
use trusted_server_core::consent::{ConsentContext, SignalRecord, SignalStatus};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::PlatformResponse;
use trusted_server_core::request_signing::{RequestSigner, SigningParams};

use crate::demand::{DemandFieldPolicy, OpenRtbDemand, OpenRtbDriver, RequestExtensions};
use crate::request::build_request;
use crate::response::extract_standard_response;
use crate::wire::BidRequest;
use crate::{MODULE as PLAIN, PlainDemand, builder as plain_builder};

/// A stand-in demand implementation that departs from the plain one. It
/// serves stored requests, forbids all-eligible routing, takes bidder
/// parameters, keeps the unsigned request identity and completes an endpoint
/// that names a host alone.
///
/// It writes each impression's usable bidder parameters under
/// `ext.fixture.bidder`, or the slot id under `ext.fixture.stored` where the
/// slot allows a stored request, and leaves out an impression that has
/// neither.
mod fixture {
    use core::any::Any;
    use std::sync::Arc;

    use async_trait::async_trait;
    use error_stack::Report;
    use serde_json::{Map, Value, json};
    use trusted_server_core::auction::demand::{
        CompiledDemand, DemandImplementation, DemandResponse, DemandTimeoutDefault,
        ProviderAuctionInput,
    };
    use trusted_server_core::auction::types::AuctionResponse;
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::integrations::IntegrationBuilder;
    use trusted_server_core::platform::PlatformResponse;
    use url::Url;

    use crate::demand::{
        DemandFieldPolicy, OpenRtbDemand, OpenRtbDriver, RegsPolicy, RequestExtensions,
    };
    use crate::response::parse_openrtb_response;

    pub(super) const MODULE: &str = "auction-protocol.fixture";
    pub(super) const AUCTION_PATH: &str = "/fixture/auction";

    pub(super) static DEMAND: DemandImplementation = DemandImplementation {
        id: MODULE,
        default_timeout: DemandTimeoutDefault::Fixed(1000),
        allows_all_eligible: false,
        serves_stored_requests: true,
        canonicalize_endpoint,
        compile,
    };

    pub(super) fn builder() -> IntegrationBuilder {
        IntegrationBuilder::implementations(MODULE, "fixture").with_demand(&DEMAND)
    }

    #[derive(Debug, Clone, Default)]
    pub(super) struct FixtureDemand;

    fn compile(
        _settings: &Map<String, Value>,
    ) -> Result<Arc<dyn CompiledDemand>, Report<TrustedServerError>> {
        Ok(Arc::new(OpenRtbDriver::new(FixtureDemand)))
    }

    fn canonicalize_endpoint(endpoint: &mut Url) -> Result<(), String> {
        if matches!(endpoint.path(), "" | "/") {
            endpoint.set_path(AUCTION_PATH);
        }
        Ok(())
    }

    #[async_trait(?Send)]
    impl OpenRtbDemand for FixtureDemand {
        fn field_policy(&self) -> DemandFieldPolicy {
            DemandFieldPolicy {
                imp_tagid: true,
                site_ref: true,
                precise_geo: true,
                unsigned_request_identity: true,
                consumes_bidder_params: true,
                regs: RegsPolicy::Jurisdiction,
                ..DemandFieldPolicy::default()
            }
        }

        fn augment_request(
            &self,
            extensions: &mut RequestExtensions<'_>,
            _input: &ProviderAuctionInput,
        ) -> Result<(), Report<TrustedServerError>> {
            for impression in &mut extensions.impressions {
                let slot = impression.slot;
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
                if fixture.is_empty() {
                    impression.omitted = true;
                    continue;
                }
                *impression.ext = Some(Map::from_iter([(
                    "fixture".to_string(),
                    Value::Object(fixture),
                )]));
            }
            *extensions.request = Some(Map::from_iter([(
                "fixture".to_string(),
                Value::Object(Map::new()),
            )]));
            Ok(())
        }

        async fn parse_response(
            &self,
            context: DemandResponse<'_>,
            response: PlatformResponse,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            parse_openrtb_response(context, response).await
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }
}

const FIXTURE: &str = fixture::MODULE;

fn builders() -> [trusted_server_core::integrations::IntegrationBuilder; 2] {
    [plain_builder(), fixture::builder()]
}

fn config(implementation: &str, settings: Value) -> AuctionPlanConfig {
    config_with_endpoint(
        implementation,
        settings,
        "https://exchange.example.test/openrtb",
    )
}

fn config_with_endpoint(
    implementation: &str,
    settings: Value,
    endpoint: &str,
) -> AuctionPlanConfig {
    let provider_id = ProviderId::from_str("fictional_provider").expect("should parse provider ID");
    // The stand-in forbids all-eligible routing, so a bidder is routed to it.
    let routed_by_bidder = implementation == FIXTURE;
    let mut table = demand_table(implementation, endpoint);
    table.insert("timeout_ms".to_string(), json!(321));
    if !routed_by_bidder {
        table.insert("routing".to_string(), json!("all_eligible"));
    }
    if let Value::Object(settings) = settings {
        table.extend(settings);
    }
    let mut config = plan_config_with(vec![("fictional_provider", table)], &builders());
    config.timeout_ms = 321;
    if routed_by_bidder {
        config.bidders = BTreeMap::from([(
            BidderId::from_str("exampleBidder").expect("should parse bidder ID"),
            BidderRouteConfig {
                module: provider_id,
            },
        )]);
    }
    config
}

fn routed(implementation: &str, settings: Value) -> (AuctionPlan, RoutedAuction) {
    let plan = AuctionPlan::compile(config(implementation, settings)).expect("should compile plan");
    let inbound = Request::builder()
        .uri("https://publisher.example/auction")
        .header(
            header::REFERER,
            "https://referrer.example/story?fictional=1",
        )
        .header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .header("dnt", "1")
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    let routed = route_for_test(&plan, canonical_parity_auction_request(), &inbound, None);
    (plan, routed)
}

fn routed_with_request(
    implementation: &str,
    settings: Value,
    request: trusted_server_core::auction::types::AuctionRequest,
    accept_language: Option<&str>,
) -> (AuctionPlan, RoutedAuction) {
    let plan = AuctionPlan::compile(config(implementation, settings)).expect("should compile plan");
    let mut builder = Request::builder().uri("https://publisher.example/auction");
    if let Some(language) = accept_language {
        builder = builder.header(header::ACCEPT_LANGUAGE, language);
    }
    let inbound = builder
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    let routed = route_for_test(&plan, request, &inbound, None);
    (plan, routed)
}

/// The source a provider plan runs, as the driver sees it.
fn demand_of(provider: &ProviderPlan) -> &dyn OpenRtbDemand {
    let any = provider.demand.as_any();
    if let Some(plain) = any.downcast_ref::<PlainDemand>() {
        return plain;
    }
    any.downcast_ref::<fixture::FixtureDemand>()
        .expect("the plan should run one of the two implementations under test")
}

fn finalization<'a>(signer: Option<&'a RequestSigner>) -> RequestFinalization<'a> {
    RequestFinalization {
        signer,
        signing_params: SigningParams {
            request_id: "fictional-auction".to_string(),
            request_host: "publisher.example".to_string(),
            request_scheme: "https".to_string(),
            timestamp: 1_706_900_000,
        },
    }
}

fn build_routed(
    plan: &AuctionPlan,
    routed: &RoutedAuction,
    signer: Option<&RequestSigner>,
) -> Result<Option<BidRequest>, error_stack::Report<TrustedServerError>> {
    build_request(
        demand_of(&plan.providers()[0]),
        &routed.inputs()[0],
        routed.transport(),
        321,
        &finalization(signer),
    )
}

fn build(implementation: &str, settings: Value, signer: Option<&RequestSigner>) -> BidRequest {
    let (plan, routed) = routed(implementation, settings);
    build_routed(&plan, &routed, signer)
        .expect("should build request")
        .expect("should retain impression")
}

fn build_with_request(
    implementation: &str,
    settings: Value,
    request: trusted_server_core::auction::types::AuctionRequest,
    accept_language: Option<&str>,
) -> BidRequest {
    let (plan, routed) = routed_with_request(implementation, settings, request, accept_language);
    build_routed(&plan, &routed, None)
        .expect("should build request")
        .expect("should retain impression")
}

fn response(status: u16, body: Vec<u8>) -> PlatformResponse {
    PlatformResponse::new(
        edgezero_core::http::response_builder()
            .status(status)
            .body(EdgeBody::from(body))
            .expect("should build the upstream response"),
    )
}

#[test]
fn consent_matrix_holds_under_each_implementation() {
    let applying = SignalRecord::new("tcf", SignalStatus::Decoded)
        .with_value("tc-string")
        .with_applies(true);
    let browser_setting = SignalRecord::new("gpc", SignalStatus::Decoded).with_value("1");
    let cases = [
        ("empty", ConsentContext::default()),
        (
            "gdpr",
            ConsentContext::new(Jurisdiction::Gdpr).with_record(applying),
        ),
        (
            "unknown-gpc",
            ConsentContext::new(Jurisdiction::Unknown).with_record(browser_setting.clone()),
        ),
        (
            "nonregulated-gpc",
            ConsentContext::new(Jurisdiction::NonRegulated).with_record(browser_setting),
        ),
        (
            "usp-gpp",
            ConsentContext::new(Jurisdiction::NonRegulated)
                .with_record(
                    SignalRecord::new("us_privacy", SignalStatus::Decoded).with_value("1YNN"),
                )
                .with_record(
                    SignalRecord::new("gpp", SignalStatus::Decoded)
                        .with_value("gpp-string")
                        .with_sections(vec![7, 8]),
                ),
        ),
    ];
    for (name, consent) in cases {
        for implementation in [PLAIN, FIXTURE] {
            let mut canonical = canonical_parity_auction_request();
            canonical.user.consent = Some(consent.clone());
            let value = serde_json::to_value(build_with_request(
                implementation,
                json!({}),
                canonical,
                None,
            ))
            .expect("should serialize request");
            let regs = value.get("regs");
            if name == "empty" {
                assert!(regs.is_none(), "{implementation} should omit empty regs");
            } else {
                let regs = regs.expect("should emit actionable regs");
                let expected_gdpr = match consent.jurisdiction {
                    Jurisdiction::Gdpr => Some(true),
                    Jurisdiction::Unknown if !consent.applies() => None,
                    _ => Some(consent.applies()),
                };
                assert_eq!(
                    regs.get("gdpr"),
                    expected_gdpr.map(|value| json!(u8::from(value))).as_ref(),
                    "{name} {implementation}"
                );
            }
            let serialized = value.to_string();
            assert!(
                !serialized.contains("1YYY"),
                "must never synthesize USP from GPC"
            );
            if name == "usp-gpp" {
                let regs = regs.expect("should have explicit fields");
                assert_eq!(regs["us_privacy"], "1YNN");
                assert_eq!(regs["gpp"], "gpp-string");
                assert_eq!(regs["gpp_sid"], json!([7, 8]));
                assert_eq!(regs["ext"]["us_privacy"], "1YNN");
                assert_eq!(regs["ext"]["gpp"], "gpp-string");
                assert_eq!(regs["ext"]["gpp_sid"], json!([7, 8]));
            }
        }
    }
}

#[test]
fn language_limits_are_profile_specific() {
    let language = "abcdefghijk";
    for (implementation, expected) in [(FIXTURE, Some(language)), (PLAIN, None)] {
        let request = build_with_request(
            implementation,
            json!({}),
            canonical_parity_auction_request(),
            Some(language),
        );
        assert_eq!(
            request.device.and_then(|device| device.language).as_deref(),
            expected
        );
    }
    for implementation in [FIXTURE, PLAIN] {
        let request = build_with_request(
            implementation,
            json!({}),
            canonical_parity_auction_request(),
            Some("en-US,en;q=0.9"),
        );
        assert_eq!(
            request.device.and_then(|device| device.language).as_deref(),
            Some("en")
        );
    }
}

#[test]
fn the_identifiers_the_request_carries_are_sent_as_eids() {
    let request = build(PLAIN, json!({}), None);
    let value = serde_json::to_value(request).expect("should serialize request");
    assert_eq!(
        value["user"]["ext"]["eids"],
        json!([{"source": "identity.example", "uids": [{"atype": 1, "id": "fictional-uid"}]}]),
        "the identifiers core routed should be formatted as the user's eids"
    );
    assert_eq!(
        value["user"]["id"], "fictional-user",
        "the user id travels beside them"
    );
}

#[test]
fn filtering_keeps_slot_pairs_and_drops_demandless_trusted_routes() {
    let plan = AuctionPlan::compile(config(FIXTURE, json!({}))).expect("should compile plan");
    let mut request = canonical_parity_auction_request();
    let template = request.slots[0].clone();
    request.slots = [
        (
            "drop-first",
            json!({"storedRequest":false,"bidderParams":{"exampleBidder":{}}}),
        ),
        (
            "keep-inline",
            json!({"storedRequest":false,"bidderParams":{"exampleBidder":{"id":2}}}),
        ),
        (
            "drop-trusted",
            json!({"storedRequest":false,"bidderParams":{}}),
        ),
        (
            "keep-stored",
            json!({"storedRequest":true,"bidderParams":{}}),
        ),
    ]
    .into_iter()
    .map(|(id, envelope)| AdSlot {
        id: id.to_string(),
        bidders: HashMap::from([("trustedServer".to_string(), envelope)]),
        ..template.clone()
    })
    .collect();
    let inbound = Request::new(EdgeBody::empty());
    let routed = route_with_trusted_routes_for_test(
        &plan,
        request,
        &inbound,
        &[&[], &[], &["fictional_provider"], &[]],
    );
    assert_eq!(
        routed.inputs()[0].slots().len(),
        4,
        "should preserve server-owned route admission"
    );
    let request = build_routed(&plan, &routed, None)
        .expect("should build request")
        .expect("should retain siblings");
    let wire = serde_json::to_value(request).expect("should serialize request");
    assert_eq!(
        wire["imp"]
            .as_array()
            .expect("should have impressions")
            .len(),
        2
    );
    assert_eq!(wire["imp"][0]["id"], "keep-inline");
    assert_eq!(
        wire["imp"][0]["ext"]["fixture"]["bidder"]["exampleBidder"],
        json!({"id":2})
    );
    assert_eq!(wire["imp"][1]["id"], "keep-stored");
    assert_eq!(wire["imp"][1]["ext"]["fixture"]["stored"], "keep-stored");
}

#[test]
fn a_disabled_empty_candidate_does_not_become_stored_demand() {
    let mut raw = config(FIXTURE, json!({"routing": "explicit"}));
    raw.bidders.insert(
        BidderId::from_str("exampleBidder").expect("should parse bidder"),
        BidderRouteConfig {
            module: ProviderId::from_str("fictional_provider").expect("should parse provider"),
        },
    );
    let plan = AuctionPlan::compile(raw).expect("should compile the plan");
    let mut request = canonical_parity_auction_request();
    request.slots[0].bidders = HashMap::from([(
        "trustedServer".to_string(),
        json!({"bidderParams":{"exampleBidder":{}}, "storedRequest": false}),
    )]);
    let inbound = Request::builder()
        .uri("https://publisher.example.com/auction")
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    let routed = route_for_test(&plan, request, &inbound, None);
    assert_eq!(
        routed.inputs().len(),
        1,
        "should route candidate for overrides"
    );
    assert!(
        build_routed(&plan, &routed, None)
            .expect("should build request")
            .is_none(),
        "a source left with no impression asks for nothing"
    );
}

#[test]
fn signing_finalization_is_after_profiles_and_asserts_every_owned_key() {
    let signer = deterministic_signer();
    for (implementation, config) in [
        (PLAIN, json!({"request_ext": {"fictional": true}})),
        (FIXTURE, json!({})),
    ] {
        let unsigned = serde_json::to_value(build(implementation, config.clone(), None))
            .expect("should serialize unsigned request");
        let unsigned_ts = unsigned["ext"].get("trusted_server");
        if implementation == FIXTURE {
            assert_eq!(
                unsigned_ts,
                Some(&json!({"request_host": "publisher.example", "request_scheme": "https"})),
                "should retain only the host and scheme when unsigned"
            );
        } else {
            assert!(
                unsigned_ts.is_none(),
                "should omit the extension when unsigned"
            );
        }

        let signed = serde_json::to_value(build(implementation, config, Some(&signer)))
            .expect("should serialize signed request");
        let extension = &signed["ext"]["trusted_server"];
        assert_eq!(extension["version"], "1.1", "should set signing version");
        assert_eq!(extension["kid"], "fictional-kid", "should set key ID");
        assert_eq!(
            extension["request_host"], "publisher.example",
            "should set host"
        );
        assert_eq!(extension["request_scheme"], "https", "should set scheme");
        assert_eq!(
            extension["ts"], 1_706_900_000_u64,
            "should set explicit time"
        );
        assert!(
            extension["signature"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "should set signature"
        );
    }
}

/// The plain source over one bidder-routed slot of the given formats.
fn standard_input_with_formats(formats: Vec<AdFormat>) -> ProviderAuctionInput {
    let mut raw = config(
        PLAIN,
        json!({"request_ext": {"fixture": true}, "imp_ext": {"slot_fixture": true}}),
    );
    raw.bidders.insert(
        BidderId::from_str("exampleBidder").expect("should parse bidder"),
        BidderRouteConfig {
            module: ProviderId::from_str("fictional_provider").expect("should parse provider"),
        },
    );
    let plan = AuctionPlan::compile(raw).expect("should compile standard fixture plan");
    let inbound = Request::builder()
        .uri("https://publisher.example/auction")
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    let mut auction_request = canonical_parity_auction_request();
    auction_request.slots[0].formats = formats;
    route_for_test(&plan, auction_request, &inbound, None).inputs()[0].clone()
}

fn standard_input() -> ProviderAuctionInput {
    standard_input_with_formats(vec![AdFormat {
        media_type: MediaType::Banner,
        width: 300,
        height: 250,
    }])
}

#[test]
fn standard_response_extraction_isolates_malformed_siblings_and_ignores_response_id() {
    let input = standard_input();
    let response = extract_standard_response(
        "fictional_provider",
        &input,
        &json!({
            "id": "informational-mismatch",
            "seatbid": [{"seat": "fictional-seat", "bid": [
                {"id": "good", "impid": "fictional-slot", "price": 1.5, "adm": "<div>ok</div>", "w": 300, "h": 250},
                {"id": "bad", "impid": "fictional-slot", "price": "bad", "adm": "<div>bad</div>", "w": 300, "h": 250}
            ]}]
        }),
        9,
    );
    assert_eq!(response.status, BidStatus::Success);
    assert_eq!(response.bids.len(), 1, "should isolate malformed sibling");
    assert_eq!(
        response.bids[0].returned_seat.as_deref(),
        Some("fictional-seat")
    );
}

#[test]
fn standard_response_currency_accepts_omitted_and_usd_but_rejects_other_or_malformed_values() {
    let input = standard_input();
    let bid = json!({"seatbid": [{"seat": "seat", "bid": [
        {"id":"good","impid":"fictional-slot","price":1.0,"adm":"ok","w":300,"h":250}
    ]}]});

    for currency in [None, Some(json!("USD")), Some(json!("usd"))] {
        let mut value = bid.clone();
        if let Some(currency) = currency {
            value["cur"] = currency;
        }
        let response = extract_standard_response("fictional_provider", &input, &value, 0);
        assert_eq!(
            response.status,
            BidStatus::Success,
            "should accept omitted or USD currency"
        );
        assert_eq!(response.bids[0].currency, "USD");
    }

    let mut eur = bid.clone();
    eur["cur"] = json!("EUR");
    let response = extract_standard_response("fictional_provider", &input, &eur, 0);
    assert_eq!(response.status, BidStatus::NoBid);
    assert_eq!(response.metadata["unsupported_currency"], "EUR");

    let mut malformed = bid;
    malformed["cur"] = json!(["USD"]);
    let response = extract_standard_response("fictional_provider", &input, &malformed, 0);
    assert_eq!(response.status, BidStatus::Error);
    assert_eq!(response.metadata["error_type"], "parse_response");
}

#[test]
fn standard_response_rejects_unknown_impressions_and_dimensions_but_keeps_siblings() {
    let input = standard_input();
    let response = extract_standard_response(
        "fictional_provider",
        &input,
        &json!({"seatbid": [{"seat": "seat", "bid": [
            {"id":"good","impid":"fictional-slot","price":1.0,"adm":"ok","w":300,"h":250},
            {"id":"unknown","impid":"unknown-slot","price":2.0,"adm":"bad","w":300,"h":250},
            {"id":"dimension","impid":"fictional-slot","price":3.0,"adm":"bad","w":320,"h":50}
        ]}]}),
        0,
    );
    assert_eq!(response.status, BidStatus::Success);
    assert_eq!(response.bids.len(), 1);
    assert_eq!(response.bids[0].bid_id.as_deref(), Some("good"));
    assert_eq!(
        response.metadata["response_admission"]["rejected_bid_count"],
        2
    );
    assert_eq!(
        response.metadata["response_admission"]["rejection_reasons"]["unrequested_impression"],
        1
    );
    assert_eq!(
        response.metadata["response_admission"]["rejection_reasons"]["dimension_mismatch"],
        1
    );
}

#[test]
fn standard_response_infers_only_unambiguous_missing_dimensions() {
    let input = standard_input();
    let inferred = extract_standard_response(
        "fictional_provider",
        &input,
        &json!({"seatbid": [{"bid": [
            {"id":"inferred","impid":"fictional-slot","price":1.0,"adm":"ok"}
        ]}]}),
        0,
    );
    assert_eq!(inferred.status, BidStatus::Success);
    assert_eq!(inferred.bids[0].width, 300);
    assert_eq!(inferred.bids[0].height, 250);

    let formats = vec![
        AdFormat {
            media_type: MediaType::Banner,
            width: 300,
            height: 250,
        },
        AdFormat {
            media_type: MediaType::Banner,
            width: 320,
            height: 50,
        },
    ];
    let input = standard_input_with_formats(formats);
    let ambiguous = extract_standard_response(
        "fictional_provider",
        &input,
        &json!({"seatbid": [{"bid": [
            {"id":"ambiguous","impid":"fictional-slot","price":1.0,"adm":"ok"}
        ]}]}),
        0,
    );
    assert_eq!(ambiguous.status, BidStatus::NoBid);
    assert_eq!(
        ambiguous.metadata["response_admission"]["rejected_bid_count"],
        1
    );
    assert_eq!(
        ambiguous.metadata["response_admission"]["rejection_reasons"]["ambiguous_dimensions"],
        1
    );
}

#[test]
fn standard_response_infers_dimensions_from_partial_unique_matches() {
    let formats = vec![
        AdFormat {
            media_type: MediaType::Banner,
            width: 300,
            height: 250,
        },
        AdFormat {
            media_type: MediaType::Banner,
            width: 320,
            height: 50,
        },
    ];
    let input = standard_input_with_formats(formats);

    let response = extract_standard_response(
        "fictional_provider",
        &input,
        &json!({"seatbid": [{"bid": [
            {"id":"width-only","impid":"fictional-slot","price":1.0,"adm":"width","w":300},
            {"id":"height-only","impid":"fictional-slot","price":2.0,"adm":"height","h":50}
        ]}]}),
        0,
    );

    assert_eq!(
        response.status,
        BidStatus::Success,
        "should admit both bids"
    );
    assert_eq!(response.bids.len(), 2, "should retain both unique matches");
    assert_eq!(
        (response.bids[0].width, response.bids[0].height),
        (300, 250),
        "should infer height from a unique width"
    );
    assert_eq!(
        (response.bids[1].width, response.bids[1].height),
        (320, 50),
        "should infer width from a unique height"
    );
}

#[test]
fn standard_response_accepts_null_and_integral_float_dimensions() {
    let input = standard_input();

    let response = extract_standard_response(
        "fictional_provider",
        &input,
        &json!({"seatbid": [{"bid": [
            {"id":"null","impid":"fictional-slot","price":1.0,"adm":"null","w":null,"h":null},
            {"id":"integral-float","impid":"fictional-slot","price":2.0,"adm":"float","w":300.0,"h":250.0}
        ]}]}),
        0,
    );

    assert_eq!(
        response.status,
        BidStatus::Success,
        "should admit both bids"
    );
    assert_eq!(
        response.bids.len(),
        2,
        "should retain both tolerated shapes"
    );
    assert!(
        response
            .bids
            .iter()
            .all(|bid| (bid.width, bid.height) == (300, 250)),
        "should resolve both bids to the requested format"
    );
}

#[test]
fn standard_response_rejects_partial_dimension_mismatch_and_ambiguity() {
    let formats = vec![
        AdFormat {
            media_type: MediaType::Banner,
            width: 300,
            height: 250,
        },
        AdFormat {
            media_type: MediaType::Banner,
            width: 300,
            height: 600,
        },
        AdFormat {
            media_type: MediaType::Banner,
            width: 320,
            height: 50,
        },
    ];
    let input = standard_input_with_formats(formats);

    let response = extract_standard_response(
        "fictional_provider",
        &input,
        &json!({"seatbid": [{"bid": [
            {"id":"ambiguous","impid":"fictional-slot","price":1.0,"adm":"ambiguous","w":300},
            {"id":"mismatch","impid":"fictional-slot","price":2.0,"adm":"mismatch","w":999}
        ]}]}),
        0,
    );

    assert_eq!(
        response.status,
        BidStatus::NoBid,
        "should reject both partial dimensions"
    );
    assert_eq!(
        response.metadata["response_admission"]["rejection_reasons"]["ambiguous_dimensions"], 1,
        "should report the width matching multiple formats"
    );
    assert_eq!(
        response.metadata["response_admission"]["rejection_reasons"]["dimension_mismatch"], 1,
        "should report the width matching no formats"
    );
}

#[test]
fn standard_response_rejects_invalid_numeric_dimension_shapes() {
    let input = standard_input();

    let response = extract_standard_response(
        "fictional_provider",
        &input,
        &json!({"seatbid": [{"bid": [
            {"id":"fractional","impid":"fictional-slot","price":1.0,"adm":"bad","w":300.5},
            {"id":"zero","impid":"fictional-slot","price":1.0,"adm":"bad","w":0},
            {"id":"negative","impid":"fictional-slot","price":1.0,"adm":"bad","w":-1},
            {"id":"large","impid":"fictional-slot","price":1.0,"adm":"bad","w":4294967296.0},
            {"id":"string","impid":"fictional-slot","price":1.0,"adm":"bad","w":"300"}
        ]}]}),
        0,
    );

    assert_eq!(
        response.status,
        BidStatus::NoBid,
        "should reject all invalid numeric shapes"
    );
    assert_eq!(
        response.metadata["response_admission"]["rejected_bid_count"], 5,
        "should report every invalid bid"
    );
    assert_eq!(
        response.metadata["response_admission"]["rejection_reasons"]["invalid_bid"], 5,
        "should classify every invalid dimension as an invalid bid"
    );
}

#[test]
fn notification_suppression_matrix_uses_only_exact_valid_returned_seat() {
    let input = standard_input();
    let response = extract_standard_response(
        "fictional_provider",
        &input,
        &json!({"seatbid": [
            {"seat": "exact", "bid": [{"id":"exact","impid":"fictional-slot","price":1.0,"adm":"ok","w":300,"h":250,"nurl":"https://n.example","burl":"https://b.example"}]},
            {"seat": "Exact", "bid": [{"id":"case","impid":"fictional-slot","price":1.0,"adm":"ok","w":300,"h":250,"nurl":"https://n.example","burl":"https://b.example"}]},
            {"bid": [{"id":"missing","impid":"fictional-slot","price":1.0,"adm":"ok","w":300,"h":250,"nurl":"https://n.example","burl":"https://b.example"}]},
            {"seat": 7, "bid": [{"id":"nonstring","impid":"fictional-slot","price":1.0,"adm":"ok","w":300,"h":250,"nurl":"https://n.example","burl":"https://b.example"}]}
        ]}),
        0,
    );
    let mut exact = response.bids.clone();
    apply_notification_policy(
        &mut exact,
        &NotificationPolicy {
            suppress_all: false,
            suppress_seats: BTreeSet::from(["exact".to_string(), "unknown".to_string()]),
        },
    );
    assert!(exact[0].nurl.is_none(), "should suppress exact seat");
    assert!(exact[1].nurl.is_some(), "matching should be case-sensitive");
    assert!(exact[2].nurl.is_some(), "missing seat must not match");
    assert!(exact[3].nurl.is_some(), "non-string seat must not match");
    assert_eq!(exact[2].bidder, "unknown");
    assert_eq!(exact[3].bidder, "unknown");

    let mut all = response.bids;
    apply_notification_policy(
        &mut all,
        &NotificationPolicy {
            suppress_all: true,
            suppress_seats: BTreeSet::new(),
        },
    );
    assert!(
        all.iter()
            .all(|bid| bid.nurl.is_none() && bid.burl.is_none()),
        "suppress_all should remove every notification"
    );
}

/// The driver hands core the body and headers the source sends, and the
/// responses an exchange may answer with are each read as the ordinary
/// reader classifies them.
#[test]
fn the_driver_builds_what_core_sends_and_classifies_every_answer() {
    futures::executor::block_on(async {
        let mut raw = config(
            PLAIN,
            json!({"request_ext": {"fixture": true}, "imp_ext": {"slot_fixture": true}}),
        );
        raw.bidders.insert(
            BidderId::from_str("exampleBidder").expect("should parse bidder"),
            BidderRouteConfig {
                module: ProviderId::from_str("fictional_provider").expect("should parse provider"),
            },
        );
        let plan = AuctionPlan::compile(raw).expect("should compile the plan");
        let inbound = Request::new(EdgeBody::empty());
        let routed = route_for_test(&plan, canonical_parity_auction_request(), &inbound, None);
        let outbound = plan.providers()[0]
            .demand
            .build_request(
                &routed.inputs()[0],
                routed.transport(),
                321,
                &finalization(None),
            )
            .expect("should build the request")
            .expect("should keep the impression");
        assert_eq!(
            outbound
                .headers
                .get(header::CONTENT_TYPE)
                .map(http::HeaderValue::as_bytes),
            Some(b"application/json".as_slice())
        );
        assert_eq!(
            outbound
                .headers
                .get(header::ACCEPT)
                .map(http::HeaderValue::as_bytes),
            Some(b"application/json".as_slice()),
            "the plain implementation asks for JSON"
        );
        assert!(
            !outbound.headers.contains_key(header::AUTHORIZATION),
            "the plain implementation adds no authentication"
        );
        assert_eq!(
            outbound.slot_ids,
            HashSet::from(["fictional-slot".to_string()]),
            "the slots the request asks for are the impressions it carries"
        );
        let body: Value = serde_json::from_slice(&outbound.body).expect("should parse the body");
        assert_eq!(body["ext"]["fixture"], true, "should send static extension");
        assert_eq!(body["imp"][0]["ext"]["slot_fixture"], true);

        let good = response(
            200,
            serde_json::to_vec(&json!({"id":"mismatch","seatbid":[{"seat":"fictional-seat","bid":[
                {"id":"good","impid":"fictional-slot","price":1.25,"adm":"<div>fictional</div>","w":300,"h":250},
                {"id":"bad","impid":"fictional-slot","price":null,"adm":"bad","w":300,"h":250}
            ]}]}))
            .expect("should serialize response"),
        );
        let parsed = parse_as_first_source(&plan, canonical_parity_auction_request(), good, 5)
            .await
            .expect("should read the response");
        assert_eq!(parsed.status, BidStatus::Success);
        assert_eq!(parsed.bids.len(), 1, "should isolate malformed sibling");
        assert_eq!(
            parsed.metadata["routing"]["unused_bidder_params_count"], 1,
            "the plain implementation ignores the bidder parameters it is routed"
        );

        let no_bid = parse_as_first_source(
            &plan,
            canonical_parity_auction_request(),
            response(204, Vec::new()),
            5,
        )
        .await
        .expect("should read the no-bid");
        assert_eq!(no_bid.status, BidStatus::NoBid);

        let malformed = parse_as_first_source(
            &plan,
            canonical_parity_auction_request(),
            response(200, b"not-json".to_vec()),
            5,
        )
        .await
        .expect("should classify malformed response");
        assert_eq!(malformed.status, BidStatus::Error);
        assert_eq!(malformed.metadata["error_type"], "parse_response");

        let redirect = parse_as_first_source(
            &plan,
            canonical_parity_auction_request(),
            response(302, Vec::new()),
            5,
        )
        .await
        .expect("should classify redirect");
        assert_eq!(redirect.status, BidStatus::Error);
        assert_eq!(redirect.metadata["error_type"], "http_status");
        assert_eq!(redirect.metadata["http_status"], 302);
    });
}

#[test]
fn malformed_top_level_standard_response_is_error() {
    let input = standard_input();
    let response = extract_standard_response("fictional_provider", &input, &json!([]), 0);
    assert_eq!(response.status, BidStatus::Error);
    assert_eq!(response.metadata["error_type"], "parse_response");
}

/// A demand implementation that tries to write the extension the driver owns.
struct ForgingDemand;

#[async_trait::async_trait(?Send)]
impl OpenRtbDemand for ForgingDemand {
    fn field_policy(&self) -> DemandFieldPolicy {
        DemandFieldPolicy::default()
    }

    fn augment_request(
        &self,
        extensions: &mut RequestExtensions<'_>,
        _input: &ProviderAuctionInput,
    ) -> Result<(), error_stack::Report<TrustedServerError>> {
        *extensions.request = Some(serde_json::Map::from_iter([(
            "trusted_server".to_string(),
            json!({"signature": "forged"}),
        )]));
        Ok(())
    }

    async fn parse_response(
        &self,
        context: DemandResponse<'_>,
        _response: PlatformResponse,
    ) -> Result<AuctionResponse, error_stack::Report<TrustedServerError>> {
        Ok(AuctionResponse::error(
            context.provider_id,
            context.response_time_ms,
        ))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn the_driver_refuses_an_implementation_that_claims_the_trusted_server_extension() {
    let (_plan, routed) = routed(PLAIN, json!({}));

    let error = match build_request(
        &ForgingDemand,
        &routed.inputs()[0],
        routed.transport(),
        321,
        &finalization(None),
    ) {
        Ok(_) => panic!("should refuse a forged trusted_server extension"),
        Err(error) => error,
    };

    assert!(
        error.to_string().contains("trusted_server"),
        "should name the extension the driver owns: {error:?}"
    );
}

#[test]
fn the_driver_answers_as_any_with_the_source_it_runs() {
    let driver = OpenRtbDriver::new(ForgingDemand);
    assert!(
        driver.as_any().is::<ForgingDemand>(),
        "an implementation reads its own source back through the driver"
    );
    assert!(
        !driver.consumes_bidder_params(),
        "the policy of the source it runs decides"
    );
}

#[test]
fn an_implementation_sees_each_impression_beside_the_slot_it_came_from() {
    let (plan, routed) = routed(FIXTURE, json!({}));
    let request = build_routed(&plan, &routed, None)
        .expect("should build the request")
        .expect("should keep the routed impression");

    let slots = routed.inputs()[0].slots();
    assert_eq!(request.imp.len(), slots.len());
    for (imp, slot) in request.imp.iter().zip(slots) {
        assert_eq!(imp.id.as_deref(), Some(slot.slot().id.as_str()));
        assert!(
            imp.ext
                .as_ref()
                .and_then(|ext| ext.get("fixture"))
                .is_some(),
            "each impression should carry the extension built from its own slot"
        );
    }
}

fn request_with_device_attributes() -> trusted_server_core::auction::types::AuctionRequest {
    let mut request = canonical_parity_auction_request();
    request
        .device
        .as_mut()
        .expect("the parity request carries a device")
        .attributes = Some(trusted_server_core::ec::device::DeviceAttributes {
        device_type: Some(4),
        make: Some("ExampleCorp".to_owned()),
        model: Some("EX-1".to_owned()),
        os: Some("Android".to_owned()),
        os_version: Some("15.0".to_owned()),
        screen_width: Some(1080),
        screen_height: Some(2400),
    });
    request
}

/// The device object is the driver's, so attributes a device module
/// resolved reach every implementation without any of them mapping a field.
#[test]
fn the_driver_carries_resolved_device_attributes_to_every_implementation() {
    for implementation in [FIXTURE, PLAIN] {
        let request = build_with_request(
            implementation,
            json!({}),
            request_with_device_attributes(),
            None,
        );
        let device = request
            .device
            .as_ref()
            .expect("should carry a device object");
        assert_eq!(
            device.devicetype,
            Some(4),
            "{implementation}: a bidder prices on the device type"
        );
        assert_eq!(
            device.make.as_deref(),
            Some("ExampleCorp"),
            "{implementation}"
        );
        assert_eq!(device.model.as_deref(), Some("EX-1"), "{implementation}");
        assert_eq!(device.os.as_deref(), Some("Android"), "{implementation}");
        assert_eq!(device.osv.as_deref(), Some("15.0"), "{implementation}");
        assert_eq!(
            device.w,
            Some(1080),
            "{implementation}: screen width decides which creative fits"
        );
        assert_eq!(device.h, Some(2400), "{implementation}");
    }
}

#[test]
fn the_driver_omits_device_attributes_nothing_resolved() {
    let request = build_with_request(FIXTURE, json!({}), canonical_parity_auction_request(), None);
    let device = request
        .device
        .as_ref()
        .expect("should carry a device object");
    assert_eq!(
        device.devicetype, None,
        "an absent field is what a bidder reads as unknown, and an invented default would \
         misprice the inventory"
    );
    assert_eq!(device.make, None);
    assert_eq!(device.w, None);
}
