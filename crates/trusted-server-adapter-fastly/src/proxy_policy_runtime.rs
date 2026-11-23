//! Actual signed-handler qualification against the owned local HTTP origin.
//!
//! HTTPS identity delivery and Fastly edge-cache hits require separate staging
//! qualification; these HTTP cases exercise default-deny and redirect stripping.

use std::sync::Arc;

use edgezero_adapter_fastly::outbound::FastlyOutboundClient;
use edgezero_core::body::Body;
use edgezero_core::http::{Request, Response, request_builder};
use edgezero_core::outbound::HttpClient;
use edgezero_core::time::MonotonicClock;
use trusted_server_core::ec::EcContext;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::platform::{ClientInfo, RuntimeServices, UnavailableKvStore};
use trusted_server_core::proxy::{handle_first_party_proxy, handle_first_party_proxy_sign};
use trusted_server_core::settings::Settings;

use crate::platform::{FastlyPlatformConfigStore, FastlyPlatformSecretStore, RequestGeo};

const READER_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";
const READER_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.Cd5678";

fn fixture() -> (Settings, RuntimeServices, String) {
    let origin = fastly::ConfigStore::open("proxy_policy_fixture")
        .get("origin")
        .expect("should supply the owned local origin");
    let settings = Settings::from_toml(
        r#"
        [[handlers]]
        path = "^/_ts/admin"
        username = "fixture-admin"
        password = "fictional-fixture-password"
        [publisher]
        domain = "publisher.example.com"
        cookie_domain = ".publisher.example.com"
        origin_url = "https://origin.example.com"
        proxy_secret = "fictional-runtime-signing-secret"
        [proxy]
        allowed_domains = ["127.0.0.1"]
        [rewrite]
        include_domains = ["127.0.0.1"]
        [ec]
        passphrase = "fictional-runtime-key-32-bytes-minimum"
        [request_signing]
        enabled = false
        config_store_id = "fictional-config-store"
        secret_store_id = "fictional-secret-store"
        "#,
    )
    .expect("should parse runtime fixture settings");
    // `EdgeZero`'s Fastly client, with no request lifecycle, as the deadline
    // fixtures use it.
    let client = HttpClient::with_client(FastlyOutboundClient::new());
    let services = RuntimeServices::builder()
        .config_store(Arc::new(FastlyPlatformConfigStore::with_outbound_client(
            client.clone(),
        )))
        .secret_store(Arc::new(FastlyPlatformSecretStore::with_outbound_client(
            client.clone(),
        )))
        .kv_store(Arc::new(UnavailableKvStore))
        .outbound_client(client)
        .clock(MonotonicClock::default())
        .geo(Arc::new(RequestGeo::default()))
        .client_info(ClientInfo::default())
        .build();
    (settings, services, origin)
}

fn signed_url(settings: &Settings, services: &RuntimeServices, target: &str) -> String {
    let request = request_builder()
        .method("POST")
        .uri("https://publisher.example.com/first-party/proxy/sign")
        .body(Body::from(serde_json::json!({ "url": target }).to_string()))
        .expect("should build sign request");
    let response =
        futures::executor::block_on(handle_first_party_proxy_sign(settings, services, request))
            .expect("should sign the real origin URL");
    let bytes = response
        .into_body()
        .into_bytes()
        .expect("should read sign response");
    let result: serde_json::Value =
        serde_json::from_slice(&bytes).expect("should decode sign response");
    result["href"]
        .as_str()
        .expect("should return signed URL")
        .to_owned()
}

fn reader_request(signed: &str, reader: &str) -> Request {
    request_builder()
        .uri(signed)
        .header("cookie", format!("ts-ec={reader}; us_privacy=1YNN"))
        .body(Body::empty())
        .expect("should build canonical reader request")
}

fn context(settings: &Settings, services: &RuntimeServices, request: &Request) -> EcContext {
    let geo = GeoInfo {
        city: Some("Example City".to_owned()),
        country: "US".to_owned(),
        continent: Some("NA".to_owned()),
        coordinates: Some(trusted_server_core::platform::GeoCoordinates {
            latitude: 0.0,
            longitude: 0.0,
        }),
        metro_code: Some(0),
        region: Some("CA".to_owned()),
        asn: None,
    };
    let ec = EcContext::read_from_request_with_geo(settings, request, services, Some(&geo))
        .expect("should read canonical cookie and consent");
    assert!(ec.ec_allowed(), "should exercise consent allowing EC");
    assert!(
        matches!(ec.ec_value(), Some(READER_A | READER_B)),
        "should retain the valid canonical reader EC"
    );
    ec
}

fn assert_object(response: Response) {
    assert_eq!(response.status(), 200, "should return the origin object");
    assert!(
        response.headers().get("location").is_none(),
        "should not return an origin redirect"
    );
    assert_eq!(
        response
            .into_body()
            .into_bytes()
            .expect("should buffer object")
            .as_ref(),
        b"fixture-object",
        "should return the real origin bytes"
    );
}

#[test]
#[ignore = "requires tests/proxy-policy-origin.mjs local origin"]
fn network_two_readers_share_static_origin_uri() {
    let (settings, services, origin) = fixture();
    let signed = signed_url(
        &settings,
        &services,
        &format!("{origin}/static/banner.jpg?size=728"),
    );
    for reader in [READER_A, READER_B] {
        let request = reader_request(&signed, reader);
        let ec = context(&settings, &services, &request);
        let response = futures::executor::block_on(handle_first_party_proxy(
            &settings, &services, &ec, request,
        ))
        .expect("should proxy both canonical readers");
        assert_object(response);
    }
}

#[test]
#[ignore = "requires tests/proxy-policy-origin.mjs local origin"]
fn network_redirect_removes_echoed_identity() {
    let (settings, services, origin) = fixture();
    let signed = signed_url(&settings, &services, &format!("{origin}/redirect"));
    let request = reader_request(&signed, READER_A);
    let ec = context(&settings, &services, &request);
    assert_object(
        futures::executor::block_on(handle_first_party_proxy(&settings, &services, &ec, request))
            .expect("should follow the real network redirect"),
    );
}

#[test]
#[ignore = "requires tests/proxy-policy-origin.mjs local origin"]
fn network_exhausted_redirect_never_returns_raw_location() {
    let (settings, services, origin) = fixture();
    let signed = signed_url(&settings, &services, &format!("{origin}/loop?hop=0"));
    let request = reader_request(&signed, READER_B);
    let ec = context(&settings, &services, &request);
    let result =
        futures::executor::block_on(handle_first_party_proxy(&settings, &services, &ec, request));
    assert!(
        result.is_err(),
        "should fail closed when the network redirect budget is exhausted"
    );
}
