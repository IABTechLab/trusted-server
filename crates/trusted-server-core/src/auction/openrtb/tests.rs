use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::str::FromStr as _;
use std::sync::Arc;

use base64::Engine as _;
use edgezero_core::body::Body as EdgeBody;
use http::{Request, header};
use serde_json::{Value, json};

use super::test_executor::execute_standard_fixture;
use super::*;
use crate::auction::plan::{
    AuctionPlan, AuctionPlanConfig, BidderRouteConfig, NotificationConfig, ProviderConfig,
    ProviderId, RoutingMode,
};
use crate::auction::provider::{GenericOpenRtbProvider, ProviderRequestOutcome};
use crate::auction::routing::route_auction;
use crate::auction::test_support::canonical_parity_auction_request;
use crate::auction::types::{AdFormat, AdSlot, BidStatus, MediaType};
use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::{ConsentContext, ConsentSource};
use crate::platform::PlatformHttpClient;
use crate::platform::test_support::{
    HashMapConfigStore, HashMapSecretStore, NoopHttpClient, StubBackend, StubHttpClient,
    build_services_with_backend_and_http_client, build_services_with_config_secret_and_http_client,
};
use crate::request_signing::RequestSigner;

fn config(profile: &str, profile_config: Value) -> AuctionPlanConfig {
    config_with_endpoint(
        profile,
        profile_config,
        "https://exchange.example.test/openrtb",
    )
}

fn config_with_endpoint(profile: &str, profile_config: Value, endpoint: &str) -> AuctionPlanConfig {
    AuctionPlanConfig {
        timeout_ms: 321,
        providers: BTreeMap::from([(
            ProviderId::from_str("fictional-provider").expect("should parse provider"),
            ProviderConfig {
                protocol: "openrtb-2.6".to_string(),
                profile: profile.to_string(),
                endpoint: endpoint.to_string(),
                timeout_ms: Some(321),
                routing: RoutingMode::AllEligible,
                notifications: NotificationConfig::default(),
                profile_config,
            },
        )]),
        bidders: BTreeMap::new(),
        mediator: None,
        request_signing: None,
    }
}

fn routed(profile: &str, profile_config: Value) -> (AuctionPlan, RoutedAuction) {
    let plan = AuctionPlan::compile(config(profile, profile_config)).expect("should compile plan");
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
    let routed = route_auction(canonical_parity_auction_request(), &inbound, &plan, None);
    (plan, routed)
}

fn routed_with_request(
    profile: &str,
    profile_config: Value,
    request: crate::auction::types::AuctionRequest,
    accept_language: Option<&str>,
) -> (AuctionPlan, RoutedAuction) {
    let plan = AuctionPlan::compile(config(profile, profile_config)).expect("should compile plan");
    let mut builder = Request::builder().uri("https://publisher.example/auction");
    if let Some(language) = accept_language {
        builder = builder.header(header::ACCEPT_LANGUAGE, language);
    }
    let inbound = builder
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    let routed = route_auction(request, &inbound, &plan, None);
    (plan, routed)
}

fn build_with_request(
    profile: &str,
    profile_config: Value,
    request: crate::auction::types::AuctionRequest,
    accept_language: Option<&str>,
) -> OpenRtbRequest {
    let (plan, routed) = routed_with_request(profile, profile_config, request, accept_language);
    match build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        321,
        &finalization(None),
    )
    .expect("should build request")
    {
        OpenRtbBuildOutcome::Ready(request) => request,
        OpenRtbBuildOutcome::NoImpressions => panic!("should retain impression"),
    }
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

fn build(profile: &str, profile_config: Value, signer: Option<&RequestSigner>) -> OpenRtbRequest {
    let (plan, routed) = routed(profile, profile_config);
    match build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        321,
        &finalization(signer),
    )
    .expect("should build request")
    {
        OpenRtbBuildOutcome::Ready(request) => request,
        OpenRtbBuildOutcome::NoImpressions => panic!("should retain impression"),
    }
}

fn deterministic_signer() -> RequestSigner {
    let mut config_data = HashMap::new();
    config_data.insert("current-kid".to_string(), "fictional-kid".to_string());
    let mut secret_data = HashMap::new();
    secret_data.insert(
        "fictional-kid".to_string(),
        base64::engine::general_purpose::STANDARD
            .encode([7_u8; 32])
            .into_bytes(),
    );
    let services = build_services_with_config_secret_and_http_client(
        HashMapConfigStore::new(config_data),
        HashMapSecretStore::new(secret_data),
        Arc::new(NoopHttpClient),
    );
    RequestSigner::from_services(&services).expect("should load deterministic signer")
}

#[test]
fn consent_matrix_preserves_pbs_standard_and_aps_policies() {
    let cases = [
        ("empty", ConsentContext::default()),
        (
            "gdpr",
            ConsentContext {
                gdpr_applies: true,
                raw_tc_string: Some("tc-string".to_string()),
                jurisdiction: Jurisdiction::Gdpr,
                ..Default::default()
            },
        ),
        (
            "unknown-gpc",
            ConsentContext {
                gpc: true,
                jurisdiction: Jurisdiction::Unknown,
                ..Default::default()
            },
        ),
        (
            "nonregulated-gpc",
            ConsentContext {
                gpc: true,
                jurisdiction: Jurisdiction::NonRegulated,
                ..Default::default()
            },
        ),
        (
            "usp-gpp",
            ConsentContext {
                raw_us_privacy: Some("1YNN".to_string()),
                raw_gpp_string: Some("gpp-string".to_string()),
                gpp_section_ids: Some(vec![7, 8]),
                jurisdiction: Jurisdiction::NonRegulated,
                ..Default::default()
            },
        ),
    ];
    for (name, consent) in cases {
        for profile in ["standard", "prebid-server", "aps"] {
            let mut canonical = canonical_parity_auction_request();
            canonical.user.consent = Some(consent.clone());
            let config = if profile == "aps" {
                json!({"account_id": "example-account-id"})
            } else {
                json!({})
            };
            let value = serde_json::to_value(build_with_request(profile, config, canonical, None))
                .expect("should serialize request");
            let regs = value.get("regs");
            if profile == "aps" {
                let regs = regs.expect("APS should preserve empty admitted context");
                assert_eq!(
                    regs["gdpr"],
                    json!(u8::from(consent.gdpr_applies)),
                    "{name}"
                );
            } else if name == "empty" {
                assert!(regs.is_none(), "{profile} should omit empty regs");
            } else {
                let regs = regs.expect("should emit actionable regs");
                let expected_gdpr = match consent.jurisdiction {
                    Jurisdiction::Gdpr => Some(true),
                    Jurisdiction::Unknown if !consent.gdpr_applies => None,
                    _ => Some(consent.gdpr_applies),
                };
                assert_eq!(
                    regs.get("gdpr"),
                    expected_gdpr.map(|value| json!(u8::from(value))).as_ref(),
                    "{name} {profile}"
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
fn pbs_body_consent_respects_source_and_forwarding_mode() {
    for (mode, source, expected) in [
        ("cookies_only", ConsentSource::Cookie, false),
        ("cookies_only", ConsentSource::KvStore, true),
        ("cookies_only", ConsentSource::PolicyDefault, true),
        ("openrtb_only", ConsentSource::Cookie, true),
        ("both", ConsentSource::Cookie, true),
    ] {
        let mut canonical = canonical_parity_auction_request();
        canonical.user.consent.as_mut().expect("consent").source = source;
        let value = serde_json::to_value(build_with_request(
            "prebid-server",
            json!({"consent_forwarding": mode}),
            canonical,
            None,
        ))
        .expect("should serialize request");
        assert_eq!(
            value["user"].get("consent").is_some(),
            expected,
            "{mode:?} {source:?}"
        );
        assert_eq!(value.get("regs").is_some(), expected, "{mode:?} {source:?}");
    }
}

#[test]
fn language_limits_are_profile_specific() {
    let language = "abcdefghijk";
    for (profile, expected) in [
        ("prebid-server", Some(language)),
        ("aps", None),
        ("standard", None),
    ] {
        let config = if profile == "aps" {
            json!({"account_id": "example-account-id"})
        } else {
            json!({})
        };
        let request = build_with_request(
            profile,
            config,
            canonical_parity_auction_request(),
            Some(language),
        );
        assert_eq!(
            request.device.and_then(|device| device.language).as_deref(),
            expected
        );
    }
    for profile in ["prebid-server", "aps", "standard"] {
        let config = if profile == "aps" {
            json!({"account_id": "example-account-id"})
        } else {
            json!({})
        };
        let request = build_with_request(
            profile,
            config,
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
fn pbs_debug_query_fragment_preserves_exact_legacy_configured_semantics() {
    for (page, fragment, expected) in [
        (
            "https://publisher.example/article",
            "pbjs_debug=true",
            "https://publisher.example/article?pbjs_debug=true",
        ),
        (
            "https://publisher.example/article?existing=1",
            "pbjs_debug=true",
            "https://publisher.example/article?existing=1&pbjs_debug=true",
        ),
        (
            "https://publisher.example/article",
            "?pbjs_debug=true",
            "https://publisher.example/article??pbjs_debug=true",
        ),
        (
            "https://publisher.example/article?pbjs_debug=true",
            "pbjs_debug=true",
            "https://publisher.example/article?pbjs_debug=true",
        ),
        (
            "https://publisher.example/article",
            "",
            "https://publisher.example/article",
        ),
    ] {
        let mut request = canonical_parity_auction_request();
        request.publisher.page_url = Some(page.to_string());
        let built = build_with_request(
            "prebid-server",
            json!({"debug_query_params": fragment}),
            request,
            None,
        );
        assert_eq!(
            built.site.and_then(|site| site.page).as_deref(),
            Some(expected),
            "should preserve exact legacy query fragment semantics"
        );
    }
}

#[test]
fn pbs_routed_overrides_are_ordered_and_stored_request_is_trusted_fallback() {
    let mut raw = config(
        "prebid-server",
        json!({
            "debug": true,
            "test_mode": true,
            "bid_param_overrides": {"exampleBidder": {"generic": 1, "shared": "generic"}},
            "bid_param_zone_overrides": {"exampleBidder": {"zone-a": {"zone": 2, "shared": "zone"}}},
            "bid_param_override_rules": [
                {"when":{"bidder":"exampleBidder"},"set":{"ordered":1,"shared":"rule-one"}},
                {"when":{"bidder":"exampleBidder","zone":"zone-a"},"set":{"ordered":2,"shared":"rule-two"}}
            ]
        }),
    );
    raw.providers
        .get_mut(&ProviderId::from_str("fictional-provider").expect("should parse provider"))
        .expect("should find provider")
        .routing = RoutingMode::Explicit;
    raw.bidders.insert(
        crate::auction::plan::BidderId::from_str("exampleBidder").expect("should parse bidder"),
        BidderRouteConfig {
            provider: ProviderId::from_str("fictional-provider").expect("should parse provider"),
        },
    );
    let plan = AuctionPlan::compile(raw).expect("should compile PBS override plan");
    let mut request = canonical_parity_auction_request();
    request.slots[0].bidders = HashMap::from([(
        "trustedServer".to_string(),
        json!({"zone":"zone-a","bidderParams":{"exampleBidder":{"original":true,"shared":"original"}}}),
    )]);
    let inbound = Request::builder()
        .uri("https://publisher.example/auction")
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    let routed = route_auction(request, &inbound, &plan, None);
    let built = match build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        321,
        &finalization(None),
    )
    .expect("should build request")
    {
        OpenRtbBuildOutcome::Ready(request) => request,
        OpenRtbBuildOutcome::NoImpressions => panic!("should retain impression"),
    };
    let value = serde_json::to_value(built).expect("should serialize request");
    assert_eq!(
        value["imp"][0]["ext"]["prebid"]["bidder"]["exampleBidder"],
        json!({"generic":1,"ordered":2,"original":true,"shared":"rule-two","zone":2})
    );
    assert_eq!(value["ext"]["prebid"]["debug"], true);
    assert_eq!(value["ext"]["prebid"]["returnallbidstatus"], true);
    assert_eq!(value["test"], 1);

    let mut empty_overridden = canonical_parity_auction_request();
    empty_overridden.slots[0].bidders = HashMap::from([(
        "trustedServer".to_string(),
        json!({"zone":"zone-a","bidderParams":{"exampleBidder":{}}}),
    )]);
    let routed = route_auction(empty_overridden, &inbound, &plan, None);
    let built = match build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        321,
        &finalization(None),
    )
    .expect("should build request after populating empty params")
    {
        OpenRtbBuildOutcome::Ready(request) => request,
        OpenRtbBuildOutcome::NoImpressions => panic!("should retain overridden impression"),
    };
    let value = serde_json::to_value(built).expect("should serialize overridden request");
    assert_eq!(
        value["imp"][0]["ext"]["prebid"]["bidder"]["exampleBidder"],
        json!({"generic":1,"ordered":2,"shared":"rule-two","zone":2}),
        "should allow profile overrides to populate empty browser params"
    );

    let mut stored = canonical_parity_auction_request();
    stored.slots[0].bidders.clear();
    let routed = route_auction(stored, &inbound, &plan, None);
    let built = match build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        321,
        &finalization(None),
    )
    .expect("should build stored request")
    {
        OpenRtbBuildOutcome::Ready(request) => request,
        OpenRtbBuildOutcome::NoImpressions => panic!("should retain impression"),
    };
    let value = serde_json::to_value(built).expect("should serialize stored request");
    assert_eq!(
        value["imp"][0]["ext"]["prebid"]["storedrequest"]["id"],
        "fictional-slot"
    );
}

#[test]
fn pbs_pairs_each_impression_with_its_routed_slot_params() {
    let mut raw = config("prebid-server", json!({}));
    raw.providers
        .get_mut(&ProviderId::from_str("fictional-provider").expect("should parse provider"))
        .expect("should find provider")
        .routing = RoutingMode::Explicit;
    raw.bidders.insert(
        crate::auction::plan::BidderId::from_str("exampleBidder").expect("should parse bidder"),
        BidderRouteConfig {
            provider: ProviderId::from_str("fictional-provider").expect("should parse provider"),
        },
    );
    let plan = AuctionPlan::compile(raw).expect("should compile PBS plan");
    let mut request = canonical_parity_auction_request();
    request.slots[0].id = "first-slot".to_string();
    request.slots[0].bidders = HashMap::from([(
        "trustedServer".to_string(),
        json!({"bidderParams":{"exampleBidder":{"placement":"first"}}}),
    )]);
    let mut second_slot = request.slots[0].clone();
    second_slot.id = "second-slot".to_string();
    second_slot.bidders = HashMap::from([(
        "trustedServer".to_string(),
        json!({"bidderParams":{"exampleBidder":{"placement":"second"}}}),
    )]);
    request.slots.push(second_slot);
    let inbound = Request::builder()
        .uri("https://publisher.example/auction")
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    let routed = route_auction(request, &inbound, &plan, None);

    let built = match build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        321,
        &finalization(None),
    )
    .expect("should build request")
    {
        OpenRtbBuildOutcome::Ready(request) => request,
        OpenRtbBuildOutcome::NoImpressions => panic!("should retain impressions"),
    };
    let value = serde_json::to_value(built).expect("should serialize request");

    assert_eq!(value["imp"][0]["id"], "first-slot");
    assert_eq!(
        value["imp"][0]["ext"]["prebid"]["bidder"]["exampleBidder"],
        json!({"placement":"first"})
    );
    assert_eq!(value["imp"][1]["id"], "second-slot");
    assert_eq!(
        value["imp"][1]["ext"]["prebid"]["bidder"]["exampleBidder"],
        json!({"placement":"second"})
    );
}

#[test]
fn pbs_empty_params_without_matching_override_fall_back_to_stored_request() {
    let mut raw = config("prebid-server", json!({}));
    raw.providers
        .get_mut(&ProviderId::from_str("fictional-provider").expect("should parse provider"))
        .expect("should find provider")
        .routing = RoutingMode::Explicit;
    raw.bidders.insert(
        crate::auction::plan::BidderId::from_str("exampleBidder").expect("should parse bidder"),
        BidderRouteConfig {
            provider: ProviderId::from_str("fictional-provider").expect("should parse provider"),
        },
    );
    let plan = AuctionPlan::compile(raw).expect("should compile PBS plan");
    let mut request = canonical_parity_auction_request();
    request.slots[0].bidders = HashMap::from([(
        "trustedServer".to_string(),
        json!({"bidderParams":{"exampleBidder":{}}}),
    )]);
    let inbound = Request::builder()
        .uri("https://publisher.example/auction")
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    let routed = route_auction(request, &inbound, &plan, None);
    let built = match build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        321,
        &finalization(None),
    )
    .expect("should build stored request")
    {
        OpenRtbBuildOutcome::Ready(request) => request,
        OpenRtbBuildOutcome::NoImpressions => panic!("should retain stored impression"),
    };
    let value = serde_json::to_value(built).expect("should serialize stored request");
    assert_eq!(
        value["imp"][0]["ext"]["prebid"]["storedrequest"]["id"],
        "fictional-slot"
    );
    assert!(value["imp"][0]["ext"]["prebid"].get("bidder").is_none());
}

#[test]
fn pbs_driver_exact_golden_preserves_profile_policy() {
    let mut raw = config("prebid-server", json!({"consent_forwarding": "both"}));
    raw.providers
        .get_mut(&ProviderId::from_str("fictional-provider").expect("should parse provider"))
        .expect("should find provider")
        .routing = RoutingMode::Explicit;
    raw.bidders.insert(
        crate::auction::plan::BidderId::from_str("exampleBidder").expect("should parse bidder"),
        BidderRouteConfig {
            provider: ProviderId::from_str("fictional-provider").expect("should parse provider"),
        },
    );
    let plan = AuctionPlan::compile(raw).expect("should compile PBS plan");
    let mut common = canonical_parity_auction_request();
    common.slots[0].bidders = HashMap::from([(
        "exampleBidder".to_string(),
        json!({"placement": "fictional-placement"}),
    )]);
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
    let routed = route_auction(common, &inbound, &plan, None);
    let request = match build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        321,
        &finalization(None),
    )
    .expect("should build PBS request")
    {
        OpenRtbBuildOutcome::Ready(request) => request,
        OpenRtbBuildOutcome::NoImpressions => panic!("should retain impression"),
    };
    assert_eq!(
        serde_json::to_string(&request).expect("should serialize PBS driver request"),
        r#"{"id":"fictional-auction","imp":[{"id":"fictional-slot","banner":{"format":[{"w":300,"h":250},{"w":728,"h":90}]},"tagid":"fictional-slot","bidfloor":1.0,"bidfloorcur":"USD","secure":1,"ext":{"prebid":{"bidder":{"exampleBidder":{"placement":"fictional-placement"}}}}}],"site":{"domain":"publisher.example","page":"https://publisher.example/article","ref":"https://referrer.example/story?fictional=1","publisher":{"domain":"publisher.example"}},"device":{"geo":{"lat":12.34,"lon":56.78,"type":2,"country":"US","region":"CA","metro":"501","city":"Example City"},"dnt":1,"ua":"Fictional Browser","ip":"192.0.2.10","language":"en"},"user":{"id":"fictional-user","consent":"fictional-tcf","ext":{"ConsentedProvidersSettings":{"consented_providers":"fictional-ac"},"consent":"fictional-tcf","eids":[{"source":"identity.example","uids":[{"atype":1,"id":"fictional-uid"}]}]}},"tmax":321,"cur":["USD"],"regs":{"gdpr":1,"us_privacy":"1YNN","gpp":"fictional-gpp","gpp_sid":[2,6],"ext":{"gdpr":1,"gpp":"fictional-gpp","gpp_sid":[2,6],"us_privacy":"1YNN"}},"ext":{"prebid":{},"trusted_server":{"request_host":"publisher.example","request_scheme":"https"}}}"#,
        "should preserve PBS parity differences"
    );
}

#[test]
fn aps_inventory_identity_and_page_fallback_preserve_legacy_policy() {
    let mut request = canonical_parity_auction_request();
    request.publisher.domain = "deployment.example".to_string();
    request.publisher.page_url =
        Some("https://deployment.example/news/story?edition=fictional#section".to_string());
    let built = build_with_request(
        "aps",
        json!({
            "account_id": "example-account-id",
            "inventory_domain": "publisher.example",
            "inventory_page_origin": "https://www.publisher.example"
        }),
        request,
        None,
    );
    let site = built.site.expect("should include APS site");
    assert_eq!(site.domain.as_deref(), Some("publisher.example"));
    assert_eq!(
        site.page.as_deref(),
        Some("https://www.publisher.example/news/story?edition=fictional")
    );
    assert_eq!(
        site.publisher
            .and_then(|publisher| publisher.domain)
            .as_deref(),
        Some("publisher.example")
    );

    for unsafe_page in [
        "https://user:password@publisher.example/private",
        "data:text/html,fictional",
    ] {
        let mut request = canonical_parity_auction_request();
        request.publisher.page_url = Some(unsafe_page.to_string());
        let built = build_with_request(
            "aps",
            json!({"account_id":"example-account-id"}),
            request,
            None,
        );
        assert_eq!(
            built.site.and_then(|site| site.page).as_deref(),
            Some("https://publisher.example"),
            "unsafe page should fall back to publisher domain"
        );
    }
}

#[test]
fn aps_driver_exact_golden_preserves_profile_policy() {
    let request = build("aps", json!({"account_id": "example-account-id"}), None);
    assert_eq!(
        serde_json::to_string(&request).expect("should serialize APS driver request"),
        r#"{"id":"fictional-auction","imp":[{"id":"fictional-slot","banner":{"format":[{"w":300,"h":250},{"w":728,"h":90}],"w":300,"h":250,"topframe":0},"bidfloor":1.0,"bidfloorcur":"USD","secure":1}],"site":{"domain":"publisher.example","page":"https://publisher.example/article","publisher":{"domain":"publisher.example"}},"device":{"geo":{"type":2,"country":"US","region":"CA","metro":"501","city":"Example City"},"dnt":1,"ua":"Fictional Browser","ip":"192.0.2.10","language":"en"},"user":{"id":"fictional-user","consent":"fictional-tcf","ext":{"consent":"fictional-tcf","eids":[{"source":"identity.example","uids":[{"atype":1,"id":"fictional-uid"}]}]}},"tmax":321,"cur":["USD"],"regs":{"gdpr":1,"us_privacy":"1YNN","gpp":"fictional-gpp","gpp_sid":[2,6],"ext":{"gdpr":1,"gpp":"fictional-gpp","gpp_sid":[2,6],"us_privacy":"1YNN"}},"ext":{"account":"example-account-id","sdk":{"source":"prebid","version":"2.2.0"}}}"#,
        "should preserve APS parity differences"
    );
}

#[test]
fn signing_finalization_is_after_profiles_and_asserts_every_owned_key() {
    let signer = deterministic_signer();
    for (profile, config) in [
        ("standard", json!({"request_ext": {"fictional": true}})),
        ("prebid-server", json!({})),
        ("aps", json!({"account_id": "example-account-id"})),
    ] {
        let unsigned = serde_json::to_value(build(profile, config.clone(), None))
            .expect("should serialize unsigned request");
        let unsigned_ts = unsigned["ext"].get("trusted_server");
        if profile == "prebid-server" {
            assert_eq!(
                unsigned_ts,
                Some(&json!({"request_host": "publisher.example", "request_scheme": "https"})),
                "should retain only PBS host and scheme when unsigned"
            );
        } else {
            assert!(
                unsigned_ts.is_none(),
                "should omit unsigned non-PBS extension"
            );
        }

        let signed = serde_json::to_value(build(profile, config, Some(&signer)))
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

#[test]
fn signed_profiles_and_unsigned_standard_have_exact_full_goldens() {
    let signer = deterministic_signer();
    let cases = [
        (
            "standard",
            json!({"request_ext": {"fictional": true}}),
            r#"{"id":"fictional-auction","imp":[{"id":"fictional-slot","banner":{"format":[{"w":300,"h":250},{"w":728,"h":90}]},"bidfloor":1.0,"bidfloorcur":"USD","secure":1}],"site":{"domain":"publisher.example","page":"https://publisher.example/article","publisher":{"domain":"publisher.example"}},"device":{"geo":{"type":2,"country":"US","region":"CA","metro":"501","city":"Example City"},"dnt":1,"ua":"Fictional Browser","ip":"192.0.2.10","language":"en"},"user":{"id":"fictional-user","consent":"fictional-tcf","ext":{"consent":"fictional-tcf","eids":[{"source":"identity.example","uids":[{"atype":1,"id":"fictional-uid"}]}]}},"tmax":321,"cur":["USD"],"regs":{"gdpr":1,"us_privacy":"1YNN","gpp":"fictional-gpp","gpp_sid":[2,6],"ext":{"gdpr":1,"gpp":"fictional-gpp","gpp_sid":[2,6],"us_privacy":"1YNN"}},"ext":{"fictional":true,"trusted_server":{"kid":"fictional-kid","request_host":"publisher.example","request_scheme":"https","signature":"LU_JUIA1BT80ShZNjSa4PIF5T-uMjEeodwKrV_6bXgh0hi1SYVtCKn9g_DTW62krmjCOFgoFYPHsu6L0nAcuDg","ts":1706900000,"version":"1.1"}}}"#,
        ),
        (
            "prebid-server",
            json!({}),
            r#"{"id":"fictional-auction","imp":[{"id":"fictional-slot","banner":{"format":[{"w":300,"h":250},{"w":728,"h":90}]},"tagid":"fictional-slot","bidfloor":1.0,"bidfloorcur":"USD","secure":1,"ext":{"prebid":{}}}],"site":{"domain":"publisher.example","page":"https://publisher.example/article","ref":"https://referrer.example/story?fictional=1","publisher":{"domain":"publisher.example"}},"device":{"geo":{"lat":12.34,"lon":56.78,"type":2,"country":"US","region":"CA","metro":"501","city":"Example City"},"dnt":1,"ua":"Fictional Browser","ip":"192.0.2.10","language":"en"},"user":{"id":"fictional-user","consent":"fictional-tcf","ext":{"ConsentedProvidersSettings":{"consented_providers":"fictional-ac"},"consent":"fictional-tcf","eids":[{"source":"identity.example","uids":[{"atype":1,"id":"fictional-uid"}]}]}},"tmax":321,"cur":["USD"],"regs":{"gdpr":1,"us_privacy":"1YNN","gpp":"fictional-gpp","gpp_sid":[2,6],"ext":{"gdpr":1,"gpp":"fictional-gpp","gpp_sid":[2,6],"us_privacy":"1YNN"}},"ext":{"prebid":{},"trusted_server":{"kid":"fictional-kid","request_host":"publisher.example","request_scheme":"https","signature":"LU_JUIA1BT80ShZNjSa4PIF5T-uMjEeodwKrV_6bXgh0hi1SYVtCKn9g_DTW62krmjCOFgoFYPHsu6L0nAcuDg","ts":1706900000,"version":"1.1"}}}"#,
        ),
        (
            "aps",
            json!({"account_id": "example-account-id"}),
            r#"{"id":"fictional-auction","imp":[{"id":"fictional-slot","banner":{"format":[{"w":300,"h":250},{"w":728,"h":90}],"w":300,"h":250,"topframe":0},"bidfloor":1.0,"bidfloorcur":"USD","secure":1}],"site":{"domain":"publisher.example","page":"https://publisher.example/article","publisher":{"domain":"publisher.example"}},"device":{"geo":{"type":2,"country":"US","region":"CA","metro":"501","city":"Example City"},"dnt":1,"ua":"Fictional Browser","ip":"192.0.2.10","language":"en"},"user":{"id":"fictional-user","consent":"fictional-tcf","ext":{"consent":"fictional-tcf","eids":[{"source":"identity.example","uids":[{"atype":1,"id":"fictional-uid"}]}]}},"tmax":321,"cur":["USD"],"regs":{"gdpr":1,"us_privacy":"1YNN","gpp":"fictional-gpp","gpp_sid":[2,6],"ext":{"gdpr":1,"gpp":"fictional-gpp","gpp_sid":[2,6],"us_privacy":"1YNN"}},"ext":{"account":"example-account-id","sdk":{"source":"prebid","version":"2.2.0"},"trusted_server":{"kid":"fictional-kid","request_host":"publisher.example","request_scheme":"https","signature":"LU_JUIA1BT80ShZNjSa4PIF5T-uMjEeodwKrV_6bXgh0hi1SYVtCKn9g_DTW62krmjCOFgoFYPHsu6L0nAcuDg","ts":1706900000,"version":"1.1"}}}"#,
        ),
    ];
    for (profile, config, expected) in cases {
        assert_eq!(
            serde_json::to_string(&build(profile, config, Some(&signer)))
                .expect("should serialize signed request"),
            expected,
            "{profile} signed wire fixture should stay exact"
        );
    }

    assert_eq!(
        serde_json::to_string(&build(
            "standard",
            json!({"request_ext": {"fictional": true}}),
            None,
        ))
        .expect("should serialize unsigned standard request"),
        r#"{"id":"fictional-auction","imp":[{"id":"fictional-slot","banner":{"format":[{"w":300,"h":250},{"w":728,"h":90}]},"bidfloor":1.0,"bidfloorcur":"USD","secure":1}],"site":{"domain":"publisher.example","page":"https://publisher.example/article","publisher":{"domain":"publisher.example"}},"device":{"geo":{"type":2,"country":"US","region":"CA","metro":"501","city":"Example City"},"dnt":1,"ua":"Fictional Browser","ip":"192.0.2.10","language":"en"},"user":{"id":"fictional-user","consent":"fictional-tcf","ext":{"consent":"fictional-tcf","eids":[{"source":"identity.example","uids":[{"atype":1,"id":"fictional-uid"}]}]}},"tmax":321,"cur":["USD"],"regs":{"gdpr":1,"us_privacy":"1YNN","gpp":"fictional-gpp","gpp_sid":[2,6],"ext":{"gdpr":1,"gpp":"fictional-gpp","gpp_sid":[2,6],"us_privacy":"1YNN"}},"ext":{"fictional":true}}"#,
        "unsigned standard wire fixture should stay exact"
    );
}

#[test]
fn standard_static_extensions_have_no_invented_bidder_param_location() {
    let request = build(
        "standard",
        json!({
            "request_ext": {"fictional_request": {"enabled": true}},
            "imp_ext": {"fictional_imp": "value"}
        }),
        None,
    );
    let value = serde_json::to_value(request).expect("should serialize request");
    assert_eq!(value["ext"]["fictional_request"]["enabled"], true);
    assert_eq!(value["imp"][0]["ext"]["fictional_imp"], "value");
    assert!(
        !value.to_string().contains("exampleBidder"),
        "standard profile must not invent bidder params placement"
    );
}

#[test]
fn defensive_no_impression_outcome_does_not_build_transportable_request() {
    let (plan, mut routed) = routed("standard", json!({}));
    let mut common = routed.inputs()[0].common_request().clone();
    common.slots = vec![AdSlot {
        id: "video-only".to_string(),
        formats: vec![AdFormat {
            media_type: MediaType::Video,
            width: 640,
            height: 480,
        }],
        floor_price: None,
        targeting: HashMap::new(),
        bidders: HashMap::new(),
    }];
    let inbound = Request::builder()
        .uri("https://publisher.example/auction")
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    routed = route_auction(common, &inbound, &plan, None);
    assert!(
        routed.inputs().is_empty(),
        "should omit provider input before build"
    );
}

fn standard_fixture() -> (AuctionPlan, RoutedAuction, OpenRtbRequest) {
    let mut raw = config(
        "standard",
        json!({"request_ext": {"fixture": true}, "imp_ext": {"slot_fixture": true}}),
    );
    raw.providers
        .get_mut(&ProviderId::from_str("fictional-provider").expect("should parse provider"))
        .expect("should find provider")
        .routing = RoutingMode::Explicit;
    raw.bidders.insert(
        crate::auction::plan::BidderId::from_str("exampleBidder").expect("should parse bidder"),
        BidderRouteConfig {
            provider: ProviderId::from_str("fictional-provider").expect("should parse provider"),
        },
    );
    let plan = AuctionPlan::compile(raw).expect("should compile standard fixture plan");
    let inbound = Request::builder()
        .uri("https://publisher.example/auction")
        .body(EdgeBody::empty())
        .expect("should build inbound request");
    let routed = route_auction(canonical_parity_auction_request(), &inbound, &plan, None);
    let request = match build_request(
        &routed.inputs()[0],
        &routed,
        &plan.providers()[0],
        321,
        &finalization(None),
    )
    .expect("should build standard fixture request")
    {
        OpenRtbBuildOutcome::Ready(request) => request,
        OpenRtbBuildOutcome::NoImpressions => panic!("should retain impression"),
    };
    (plan, routed, request)
}

#[test]
fn standard_response_extraction_isolates_malformed_siblings_and_ignores_response_id() {
    let (_plan, routed, _request) = standard_fixture();
    let response = extract_standard_response(
        "fictional-provider",
        &routed.inputs()[0],
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
    let (_plan, routed, _request) = standard_fixture();
    let bid = json!({"seatbid": [{"seat": "seat", "bid": [
        {"id":"good","impid":"fictional-slot","price":1.0,"adm":"ok","w":300,"h":250}
    ]}]});

    for currency in [None, Some(json!("USD")), Some(json!("usd"))] {
        let mut value = bid.clone();
        if let Some(currency) = currency {
            value["cur"] = currency;
        }
        let response =
            extract_standard_response("fictional-provider", &routed.inputs()[0], &value, 0);
        assert_eq!(
            response.status,
            BidStatus::Success,
            "should accept omitted or USD currency"
        );
        assert_eq!(response.bids[0].currency, "USD");
    }

    let mut eur = bid.clone();
    eur["cur"] = json!("EUR");
    let response = extract_standard_response("fictional-provider", &routed.inputs()[0], &eur, 0);
    assert_eq!(response.status, BidStatus::NoBid);
    assert_eq!(response.metadata["unsupported_currency"], "EUR");

    let mut malformed = bid;
    malformed["cur"] = json!(["USD"]);
    let response =
        extract_standard_response("fictional-provider", &routed.inputs()[0], &malformed, 0);
    assert_eq!(response.status, BidStatus::Error);
    assert_eq!(response.metadata["error_type"], "parse_response");
}

#[test]
fn standard_response_rejects_unknown_impressions_and_dimensions_but_keeps_siblings() {
    let (_plan, routed, _request) = standard_fixture();
    let response = extract_standard_response(
        "fictional-provider",
        &routed.inputs()[0],
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
}

#[test]
fn notification_suppression_matrix_uses_only_exact_valid_returned_seat() {
    let (_plan, routed, _request) = standard_fixture();
    let response = extract_standard_response(
        "fictional-provider",
        &routed.inputs()[0],
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

#[test]
fn fictional_standard_executor_covers_bid_no_bid_malformed_unused_and_redirect() {
    futures::executor::block_on(async {
        let (plan, routed, request) = standard_fixture();
        let provider = &plan.providers()[0];
        let client = Arc::new(StubHttpClient::new());
        client.push_response(
            200,
            serde_json::to_vec(&json!({"id":"mismatch","seatbid":[{"seat":"fictional-seat","bid":[
                {"id":"good","impid":"fictional-slot","price":1.25,"adm":"<div>fictional</div>","w":300,"h":250},
                {"id":"bad","impid":"fictional-slot","price":null,"adm":"bad","w":300,"h":250}
            ]}]})).expect("should serialize response"),
        );
        let response = execute_standard_fixture(
            provider,
            &routed.inputs()[0],
            &request,
            &StubBackend,
            client.as_ref(),
        )
        .await
        .expect("should execute ordinary fixture");
        assert_eq!(response.status, BidStatus::Success);
        assert_eq!(response.bids.len(), 1, "should isolate malformed sibling");
        assert_eq!(
            response.metadata["routing"]["unused_bidder_params_count"],
            1
        );
        assert_eq!(
            client.recorded_request_uris(),
            vec![provider.endpoint.as_str()]
        );
        let headers = &client.recorded_request_headers()[0];
        assert!(
            !headers.iter().any(|(name, _)| name == "authorization"),
            "fixture must add no authentication"
        );
        let body: Value = serde_json::from_slice(&client.recorded_request_bodies()[0])
            .expect("should parse recorded body");
        assert_eq!(body["ext"]["fixture"], true, "should send static extension");

        let no_bid = Arc::new(StubHttpClient::new());
        no_bid.push_response(204, Vec::new());
        let response = execute_standard_fixture(
            provider,
            &routed.inputs()[0],
            &request,
            &StubBackend,
            no_bid.as_ref(),
        )
        .await
        .expect("should execute no-bid fixture");
        assert_eq!(response.status, BidStatus::NoBid);

        let malformed = Arc::new(StubHttpClient::new());
        malformed.push_response(200, b"not-json".to_vec());
        let response = execute_standard_fixture(
            provider,
            &routed.inputs()[0],
            &request,
            &StubBackend,
            malformed.as_ref(),
        )
        .await
        .expect("should classify malformed response");
        assert_eq!(response.status, BidStatus::Error);
        assert_eq!(response.metadata["error_type"], "parse_response");

        let redirect = Arc::new(StubHttpClient::new());
        redirect.push_response_with_headers(
            302,
            Vec::new(),
            vec![("location", "https://redirect.example.test/openrtb")],
        );
        let response = execute_standard_fixture(
            provider,
            &routed.inputs()[0],
            &request,
            &StubBackend,
            redirect.as_ref(),
        )
        .await
        .expect("should classify redirect");
        assert_eq!(response.status, BidStatus::Error);
        assert_eq!(response.metadata["error_type"], "http_status");
        assert_eq!(response.metadata["http_status"], 302);
        assert_eq!(
            redirect.recorded_request_uris(),
            vec![provider.endpoint.as_str()],
            "a 3xx Location must not trigger a second HTTP request"
        );
        assert_eq!(
            redirect.recorded_backend_names(),
            vec!["stub-backend"],
            "the common driver must perform exactly one underlying send for a 3xx"
        );
        let spec = provider.backend_spec();
        assert_eq!(spec.host, "exchange.example.test");
        assert_eq!(spec.discriminator.as_deref(), Some("fictional-provider"));
    });
}

#[test]
fn prebid_endpoint_normalization_reaches_generic_execution_and_preserves_custom_paths() {
    futures::executor::block_on(async {
        for (configured_endpoint, expected_endpoint) in [
            (
                "https://pbs.example",
                "https://pbs.example/openrtb2/auction",
            ),
            ("https://pbs.example/bid", "https://pbs.example/bid"),
        ] {
            let plan = AuctionPlan::compile(config_with_endpoint(
                "prebid-server",
                json!({}),
                configured_endpoint,
            ))
            .expect("should compile Prebid Server endpoint");
            let inbound = Request::builder()
                .uri("https://publisher.example/auction")
                .body(EdgeBody::empty())
                .expect("should build inbound request");
            let routed = route_auction(canonical_parity_auction_request(), &inbound, &plan, None);
            let provider_plan = plan.providers()[0].clone();
            let provider = GenericOpenRtbProvider::new(provider_plan.clone());
            let client = Arc::new(StubHttpClient::new());
            client.push_response(204, Vec::new());
            let services = build_services_with_backend_and_http_client(
                Arc::new(StubBackend),
                Arc::clone(&client) as Arc<dyn PlatformHttpClient>,
            );
            let mut reserved_backend_names = HashSet::new();

            let outcome = provider
                .request_bids_routed(
                    &routed.inputs()[0],
                    &routed,
                    321,
                    321,
                    None,
                    &services,
                    &mut reserved_backend_names,
                )
                .await
                .expect("should launch one Prebid Server request");
            let ProviderRequestOutcome::Pending { request, .. } = outcome else {
                panic!("should launch a pending Prebid Server request");
            };
            let selected = services
                .http_client()
                .select(vec![request])
                .await
                .expect("should select one Prebid Server response");
            let response = selected
                .ready
                .expect("should receive the Prebid Server response");

            assert_eq!(response.response.status(), http::StatusCode::NO_CONTENT);
            assert_eq!(client.recorded_backend_names(), vec!["stub-backend"]);
            assert_eq!(client.recorded_request_methods(), vec!["POST"]);
            assert_eq!(client.recorded_request_uris(), vec![expected_endpoint]);
            assert_eq!(provider_plan.backend_spec().host, "pbs.example");
            let body: Value = serde_json::from_slice(&client.recorded_request_bodies()[0])
                .expect("should parse recorded Prebid Server body");
            assert!(
                !body
                    .as_object()
                    .expect("should serialize an object")
                    .is_empty()
            );
        }
    });
}

#[test]
fn malformed_top_level_standard_response_is_error() {
    let (_plan, routed, _request) = standard_fixture();
    let response =
        extract_standard_response("fictional-provider", &routed.inputs()[0], &json!([]), 0);
    assert_eq!(response.status, BidStatus::Error);
    assert_eq!(response.metadata["error_type"], "parse_response");
}
