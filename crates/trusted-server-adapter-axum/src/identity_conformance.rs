//! Portable identity scenarios through the real Axum router and finalizer.

use std::cell::RefCell;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use edgezero_core::after_response::{AfterResponseHandle, AfterResponseScheduler};
use edgezero_core::body::Body;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Method, Request, StatusCode, header, request_builder};
use edgezero_core::ingress::{InboundOrigin, OriginSource};
use edgezero_core::ja4::Ja4Source;
use edgezero_core::router::RouterService;
use error_stack::Report;
use futures::executor::block_on;
use futures::future::LocalBoxFuture;
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::normalize_ec_id_for_kv;
use trusted_server_core::http_util::RuntimeRequestOrigin;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformConfigStore, PlatformError, PlatformGeo, PlatformSecretStore,
    RuntimeServices, StoreId, StoreName, UnavailableKvStore,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::identity_conformance::{
    Adapter, ApiObservation, ApiRequest, Consent, Geo, ID5_LOOKUP, Navigation, Observation,
    RecordingStore, Scenario, configure_api_request, configure_http_fixture, tcf_consent_cookie,
    verify_api_identity_expiries, verify_failed_ec_creation, verify_http_contract,
    verify_identity_gates,
};
use trusted_server_core::test_support::ja4::{CHROME_USER_AGENT, captured_ja4, insert_ja4};
use trusted_server_core::test_support::scripted_client::{
    ScriptedHttpClient, ScriptedReply, TestClock,
};

use crate::app::TrustedServerApp;

const DOCUMENT: &str = "<html><head></head><body>Example article</body></html>";
const ORIGIN: &str = "https://origin.example.com/article";

struct UnusedStores;
#[async_trait(?Send)]
impl PlatformConfigStore for UnusedStores {
    fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore))
    }
    async fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore))
    }
    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore))
    }
}
#[async_trait(?Send)]
impl PlatformSecretStore for UnusedStores {
    fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore))
    }
    async fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore))
    }
    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore))
    }
}
fn settings() -> Settings {
    let mut settings = Settings::from_toml(
        r#"
        [[handlers]]
        path = "^/_ts/admin"
        username = "example-admin"
        password = "example-password"
        [publisher]
        domain = "publisher.example.com"
        cookie_domain = ".publisher.example.com"
        origin_url = "https://origin.example.com"
        proxy_secret = "example-proxy-secret"
        [ec]
        passphrase = "example-secret-at-least-32-bytes-long"
        [request_signing]
        enabled = false
        config_store_id = "example-config"
        secret_store_id = "example-secrets"
        "#,
    )
    .expect("should parse conformance settings");
    configure_http_fixture(&mut settings);
    settings
}

struct FixtureGeo(Geo);

impl PlatformGeo for FixtureGeo {
    fn lookup(&self, _: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        match self.0 {
            Geo::Gdpr => Ok(Some(GeoInfo::country_region("DE", None))),
            Geo::UsCalifornia => Ok(Some(GeoInfo::country_region("US", Some("CA".to_owned())))),
            Geo::Missing => Ok(None),
            Geo::Failed => Err(Report::new(PlatformError::Geo)),
        }
    }
}

struct AxumAdapter {
    store: RecordingStore,
}

impl Adapter for AxumAdapter {
    fn navigate(&mut self, navigation: &Navigation) -> Observation {
        self.execute(navigation, None).response
    }

    fn exchange(&mut self, request: &ApiRequest) -> ApiObservation {
        self.execute(
            &Navigation {
                geo: request.geo,
                ..Navigation::default()
            },
            Some(request),
        )
    }
}

impl AxumAdapter {
    fn execute(&mut self, navigation: &Navigation, api: Option<&ApiRequest>) -> ApiObservation {
        let clock = TestClock::new();
        let origin = ScriptedHttpClient::new(&clock);
        origin.respond_to(
            api.map_or(Method::GET, |request| request.method.clone()),
            &api.map_or_else(
                || ORIGIN.to_owned(),
                |request| format!("https://origin.example.com{}", request.path),
            ),
            {
                let reply = ScriptedReply::status(
                    api.map_or(StatusCode::OK, |request| request.origin_status),
                );
                let mime = api.map_or("text/html", |request| request.origin_content_type);
                let reply = if mime.is_empty() {
                    reply
                } else {
                    reply.header("content-type", mime)
                };
                reply
                    .header("cache-control", "public, max-age=600, s-maxage=600")
                    .header("surrogate-control", "max-age=600")
                    .header("cdn-cache-control", "public, max-age=600")
                    .header("cloudflare-cdn-cache-control", "public, max-age=600")
                    .body(DOCUMENT)
            },
        );
        let services = RuntimeServices::builder()
            .config_store(Arc::new(UnusedStores))
            .secret_store(Arc::new(UnusedStores))
            .kv_store(Arc::new(UnavailableKvStore))
            .outbound_client(origin.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(FixtureGeo(navigation.geo)))
            .client_info(ClientInfo {
                client_ip: Some("192.0.2.7".parse().expect("should parse fixture IP")),
                ..ClientInfo::default()
            })
            .build();
        let mut configured = settings();
        if let Some(api) = api {
            configure_api_request(&mut configured, api);
        }
        let router = if api.is_some_and(|request| !request.store_enabled) {
            TrustedServerApp::routes_with_settings_and_services(configured, services)
        } else {
            TrustedServerApp::routes_with_settings_services_and_ec_store(
                configured,
                services,
                self.store.clone(),
            )
        }
        .expect("should build the Axum conformance router");
        let mut cookies = Vec::new();
        match navigation.consent {
            Consent::Granted => cookies.push(tcf_consent_cookie(true)),
            Consent::Denied => cookies.push(tcf_consent_cookie(false)),
            Consent::Missing => {}
        }
        if let Some(identity) = &navigation.ec_cookie {
            cookies.push(format!("ts-ec={identity}"));
        }
        let document = navigation.destination == "document";
        let request: Request = request_builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/article")
            .header("sec-fetch-dest", navigation.destination)
            .header(
                "sec-fetch-mode",
                if document { "navigate" } else { "no-cors" },
            )
            .header(header::ACCEPT, if document { "text/html" } else { "*/*" })
            .header(header::USER_AGENT, navigation.user_agent)
            .header(header::COOKIE, cookies.join("; "))
            .body(Body::empty())
            .expect("should build conformance navigation");
        let mut request = if let Some(api) = api {
            let mut builder = request_builder()
                .method(api.method.clone())
                .uri(format!("https://publisher.example.com{}", api.path))
                .header(header::USER_AGENT, navigation.user_agent);
            for (name, value) in &api.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            builder
                .body(Body::from(api.body.clone()))
                .expect("should build API conformance request")
        } else {
            request
        };
        // This router seam bypasses native ingress; supply its validated
        // public origin and, where the scenario has one, the JA4 a trusted
        // proxy header gives `ctx.ja4()`.
        request.extensions_mut().insert(RuntimeRequestOrigin::new(
            InboundOrigin::parse("https", "publisher.example.com", OriginSource::RuntimeUri)
                .expect("should validate fixture public origin"),
        ));
        if let Some(ja4) = navigation.ja4 {
            insert_ja4(
                request.extensions_mut(),
                captured_ja4(ja4, Ja4Source::TrustedProxyHeader),
            );
        }
        let response = block_on(router.oneshot(request)).expect("should dispatch the real router");
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let edge_cache_headers_present = [
            "surrogate-control",
            "cdn-cache-control",
            "cloudflare-cdn-cache-control",
        ]
        .iter()
        .any(|name| response.headers().contains_key(*name));
        self.store.response_released();
        let status = response.status();
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .expect("should decode response cookie")
                    .to_owned()
            })
            .collect();
        let body = block_on(response.into_body().into_bytes_bounded(1024 * 1024))
            .expect("should consume the complete publisher response")
            .to_vec();
        ApiObservation {
            response: Observation {
                status,
                cookies,
                body,
            },
            origin_requests: origin.requests().len(),
            outbound: origin.requests(),
            cache_control,
            edge_cache_headers_present,
        }
    }
}

#[test]
fn identity_conformance_axum() {
    for scenario in Scenario::NAVIGATION_SCENARIOS {
        let store = RecordingStore::new(scenario == Scenario::WriteFailure);
        let mut adapter = AxumAdapter {
            store: store.clone(),
        };
        log::info!("Running Axum identity scenario {scenario:?}");
        scenario.verify(&mut adapter, &store);
    }
    let store = RecordingStore::new(false);
    let mut adapter = AxumAdapter {
        store: store.clone(),
    };
    verify_http_contract(&mut adapter, &store);
    let store = RecordingStore::new(true);
    let mut adapter = AxumAdapter {
        store: store.clone(),
    };
    verify_failed_ec_creation(&mut adapter, &store);
}

#[test]
fn identity_gates_axum() {
    let store = RecordingStore::new(false);
    verify_identity_gates(
        &mut AxumAdapter {
            store: store.clone(),
        },
        &store,
    );
}

#[test]
fn api_identity_expiries_axum() {
    verify_api_identity_expiries(&mut AxumAdapter {
        store: RecordingStore::new(false),
    });
}

/// The page-lookup budget of the provider fixtures.
const IDENTITY_BUDGET: Duration = Duration::from_millis(200);
/// The UID the fixture's ID5 endpoint answers with.
const ID5_UID: &str = "example-id5-uid";

thread_local! {
    /// After-response tasks the capturing scheduler holds, in schedule order.
    static HELD_TASKS: RefCell<Vec<LocalBoxFuture<'static, ()>>> = const { RefCell::new(Vec::new()) };
}

/// Holds every after-response task on the test thread until the test runs it,
/// so a later request can be handled while the earlier task is pending.
struct HoldingScheduler;

impl AfterResponseScheduler for HoldingScheduler {
    fn schedule(
        &self,
        _budget: Duration,
        task: LocalBoxFuture<'static, ()>,
    ) -> Result<(), EdgeError> {
        HELD_TASKS.with_borrow_mut(|tasks| tasks.push(task));
        Ok(())
    }
}

/// The real Axum router with an ID5 provider for a mirrored partner, whose
/// lookups the scripted client answers after `id5_delay`.
struct ProviderFixture {
    clock: TestClock,
    client: ScriptedHttpClient,
    store: RecordingStore,
    router: RouterService,
}

impl ProviderFixture {
    fn new(origin_status: StatusCode, id5_delay: Duration, collect: &str, geo: Geo) -> Self {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            Method::GET,
            ORIGIN,
            ScriptedReply::status(origin_status)
                .header("content-type", "text/html")
                .body(DOCUMENT),
        );
        client.respond_to(
            Method::POST,
            ID5_LOOKUP,
            ScriptedReply::status(StatusCode::OK)
                .body(
                    serde_json::json!({"universal_uid": ID5_UID, "signature":"",
                        "cache_control":{"max_age_sec":3600}})
                    .to_string(),
                )
                .after(id5_delay),
        );
        let services = RuntimeServices::builder()
            .config_store(Arc::new(UnusedStores))
            .secret_store(Arc::new(UnusedStores))
            .kv_store(Arc::new(UnavailableKvStore))
            .outbound_client(client.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(FixtureGeo(geo)))
            .client_info(ClientInfo {
                client_ip: Some("192.0.2.7".parse().expect("should parse IP")),
                ..ClientInfo::default()
            })
            .build();
        let mut configured = settings();
        configured.ec.identity_budget_ms =
            u64::try_from(IDENTITY_BUDGET.as_millis()).expect("should fit the budget");
        configured.ec.identity_providers = vec![
            serde_json::from_value(serde_json::json!({
                "id":"id5", "kind":"id5", "endpoint":"https://id5.example.com/gs/v2",
                "allowed_domains":["id5.example.com"], "sources":["id5.example.com"],
                "collect": collect, "settings":{"partner_id":1234}
            }))
            .expect("should configure ID5"),
        ];
        configured.ec.partners.push(
            serde_json::from_value(serde_json::json!({
                "name":"Example identity", "source_domain":"id5.example.com",
                "bidstream_enabled":true, "mirror_enabled":true
            }))
            .expect("should configure partner"),
        );
        let store = RecordingStore::new(false);
        let router = TrustedServerApp::routes_with_settings_services_and_ec_store(
            configured,
            services,
            store.clone(),
        )
        .expect("should build router");
        Self {
            clock,
            client,
            store,
            router,
        }
    }

    /// Sends `request` through the router and returns the response with how
    /// long the test clock advanced while it was handled.
    fn send(&self, mut request: Request) -> (edgezero_core::http::Response, Duration) {
        request.extensions_mut().insert(RuntimeRequestOrigin::new(
            InboundOrigin::parse("https", "publisher.example.com", OriginSource::RuntimeUri)
                .expect("should validate origin"),
        ));
        request
            .extensions_mut()
            .insert(AfterResponseHandle::new(HoldingScheduler));
        let timer = self.clock.monotonic_clock();
        let started = timer.now();
        let response = self
            .clock
            .drive(self.router.clone().oneshot(request))
            .expect("should dispatch the request");
        (response, timer.now().duration_since(started))
    }

    fn id5_lookups(&self) -> usize {
        self.client
            .requests()
            .iter()
            .filter(|request| request.uri.host() == Some("id5.example.com"))
            .count()
    }

    fn record(&self, identity: &str) -> trusted_server_core::ec::kv_types::KvEntry {
        block_on(KvIdentityGraph::new(self.store.clone()).get(&normalize_ec_id_for_kv(identity)))
            .expect("should read the identity record")
            .expect("should retain the identity record")
            .0
    }
}

fn navigation() -> Request {
    request_builder()
        .method(Method::GET)
        .uri("https://publisher.example.com/article")
        .header("sec-fetch-dest", "document")
        .header("sec-fetch-mode", "navigate")
        .header(header::ACCEPT, "text/html")
        .header(header::USER_AGENT, CHROME_USER_AGENT)
        .header(header::COOKIE, "us_privacy=1YNN")
        .body(Body::empty())
        .expect("should build navigation")
}

fn ec_post(tcf_cookie: &str, cookie: &str) -> Request {
    let tc_string = tcf_cookie
        .strip_prefix("euconsent-v2=")
        .expect("should encode TCF");
    request_builder()
        .method(Method::POST)
        .uri("https://publisher.example.com/_ts/api/v1/ec")
        .header(header::ORIGIN, "https://publisher.example.com")
        .header(header::USER_AGENT, CHROME_USER_AGENT)
        .header(header::COOKIE, cookie)
        .body(Body::from(
            serde_json::json!({"tcString": tc_string, "applicableSections": []}).to_string(),
        ))
        .expect("should build EC POST")
}

fn set_cookies(response: &edgezero_core::http::Response) -> Vec<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| {
            value
                .to_str()
                .expect("should decode response cookie")
                .to_owned()
        })
        .collect()
}

/// The live value of cookie `name` the response sets, if any.
fn set_value(response: &edgezero_core::http::Response, name: &str) -> Option<String> {
    set_cookies(response).iter().find_map(|cookie| {
        cookie
            .strip_prefix(&format!("{name}="))
            .and_then(|rest| rest.split(';').next())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn decoded_mirror(response: &edgezero_core::http::Response) -> Option<String> {
    set_value(response, "__Host-ts-eids-mirror").map(|value| {
        String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(value)
                .expect("should encode the mirror as unpadded base64url"),
        )
        .expect("should encode the mirror as UTF-8 JSON")
    })
}

#[test]
fn page_lookup_within_the_deadline_reaches_the_record_and_the_mirror_cookie() {
    // Arrange
    let fixture = ProviderFixture::new(
        StatusCode::OK,
        IDENTITY_BUDGET / 4,
        "page",
        Geo::UsCalifornia,
    );

    // Act
    let (response, held) = fixture.send(navigation());

    // Assert
    assert_eq!(response.status(), StatusCode::OK, "should serve the page");
    assert_eq!(fixture.id5_lookups(), 1, "should look up ID5 on navigation");
    let identity = set_value(&response, "ts-ec").expect("should issue an EC");
    assert_eq!(
        fixture
            .record(&identity)
            .ids
            .get("id5.example.com")
            .map(|id| id.uid.as_str()),
        Some(ID5_UID),
        "should write the ID5 UID in the navigation's page-load write"
    );
    let mirror = decoded_mirror(&response)
        .expect("should set the mirror cookie on the navigation's response");
    assert!(
        mirror.contains(ID5_UID) && mirror.contains("id5.example.com"),
        "should mirror the ID5 UID found on this navigation, got {mirror}"
    );
    assert!(
        held <= IDENTITY_BUDGET,
        "should not hold the response past the lookup deadline, held {held:?}"
    );
}

#[test]
fn page_lookup_past_the_deadline_is_skipped_without_delaying_the_response() {
    // Arrange
    let fixture = ProviderFixture::new(
        StatusCode::OK,
        IDENTITY_BUDGET * 5,
        "page",
        Geo::UsCalifornia,
    );

    // Act
    let (response, held) = fixture.send(navigation());

    // Assert
    assert_eq!(response.status(), StatusCode::OK, "should serve the page");
    assert_eq!(fixture.id5_lookups(), 1, "should look up ID5 on navigation");
    let identity = set_value(&response, "ts-ec").expect("should issue an EC");
    assert!(
        !fixture
            .record(&identity)
            .ids
            .contains_key("id5.example.com"),
        "should skip a provider that misses the deadline"
    );
    assert_eq!(
        decoded_mirror(&response),
        None,
        "should set no mirror cookie from a skipped lookup"
    );
    assert!(
        held <= IDENTITY_BUDGET,
        "should release the response at the deadline, held {held:?}"
    );
}

/// ID-9 with a page-only provider: grant A arrives with no EC while its ID5
/// lookup is slower than the deadline, and the script sends withdrawal B once
/// A answers. A's lookup is skipped at the deadline and its UID is never
/// stored, and B leaves no identity cookie. The after-response case is
/// [`held_follow_up_from_a_grant_calls_no_provider_after_a_withdrawal`].
#[test]
fn withdrawal_after_a_grant_with_a_slow_lookup_leaves_no_live_identity() {
    // Arrange
    let fixture = ProviderFixture::new(StatusCode::OK, IDENTITY_BUDGET * 5, "page", Geo::Gdpr);
    let (grant, held) = fixture.send(ec_post(&tcf_consent_cookie(true), ""));
    let identity = set_value(&grant, "ts-ec").expect("should mint an EC for grant A");
    let sharedid = set_value(&grant, "_pubcid").expect("should set SharedID for grant A");
    assert_eq!(fixture.id5_lookups(), 1, "should look up ID5 for grant A");
    assert!(
        held <= IDENTITY_BUDGET,
        "should answer grant A by the lookup deadline, held {held:?}"
    );

    // Act
    let (withdrawal, _) = fixture.send(ec_post(
        &tcf_consent_cookie(false),
        &format!("ts-ec={identity}; _pubcid={sharedid}"),
    ));

    // Assert: A set no mirror (its lookup was skipped), so B has none to expire.
    for name in ["ts-ec", "_pubcid", "_pubcid_cst"] {
        assert!(
            set_cookies(&withdrawal)
                .iter()
                .any(|cookie| cookie.starts_with(&format!("{name}=;"))
                    && cookie.contains("Max-Age=0")),
            "withdrawal B should expire {name}: {:?}",
            set_cookies(&withdrawal)
        );
    }
    assert!(
        set_cookies(&withdrawal)
            .iter()
            .all(|cookie| cookie.contains("Max-Age=0")),
        "withdrawal B should set no identity cookie: {:?}",
        set_cookies(&withdrawal)
    );
    let record = fixture.record(&identity);
    assert!(!record.consent.ok, "should keep A's record withdrawn");
    assert!(
        !record.ids.contains_key("id5.example.com"),
        "should never store the slow lookup's UID"
    );
}

/// ID-9 with an after-response provider: grant A's follow-up lookup is still
/// held when withdrawal B is handled, and running it afterwards calls no
/// provider and leaves A's record withdrawn. The follow-up is scheduled for
/// the new EC; it checks the EC and consent only when it runs.
#[test]
fn held_follow_up_from_a_grant_calls_no_provider_after_a_withdrawal() {
    // Arrange
    let fixture = ProviderFixture::new(
        StatusCode::OK,
        IDENTITY_BUDGET * 5,
        "after_response",
        Geo::Gdpr,
    );
    HELD_TASKS.with_borrow_mut(Vec::clear);
    let (grant, _) = fixture.send(ec_post(&tcf_consent_cookie(true), ""));
    let identity = set_value(&grant, "ts-ec").expect("should mint an EC for grant A");
    let sharedid = set_value(&grant, "_pubcid").expect("should set SharedID for grant A");
    assert_eq!(
        HELD_TASKS.with_borrow(Vec::len),
        1,
        "should hold grant A's follow-up lookup, proving the capture"
    );

    // Act
    fixture.send(ec_post(
        &tcf_consent_cookie(false),
        &format!("ts-ec={identity}; _pubcid={sharedid}"),
    ));
    for task in HELD_TASKS.with_borrow_mut(std::mem::take) {
        fixture.clock.drive(task);
    }

    // Assert
    assert_eq!(
        fixture.id5_lookups(),
        0,
        "should call no provider for an EC minted by grant A"
    );
    let record = fixture.record(&identity);
    assert!(!record.consent.ok, "should keep A's record withdrawn");
    assert!(
        !record.ids.contains_key("id5.example.com"),
        "should store no late UID"
    );
}

#[test]
fn redirect_cancels_page_lookups_without_waiting() {
    let fixture = ProviderFixture::new(
        StatusCode::FOUND,
        Duration::from_secs(1),
        "page",
        Geo::UsCalifornia,
    );
    let (response, held) = fixture.send(navigation());
    assert_eq!(
        response.status(),
        StatusCode::FOUND,
        "should preserve redirect"
    );
    assert_eq!(
        fixture.client.requests().len(),
        2,
        "should start origin and page provider before cancellation"
    );
    assert!(
        held < IDENTITY_BUDGET,
        "should cancel redirect lookups without waiting for their deadline"
    );
    assert!(
        !response.headers().contains_key(header::SET_COOKIE),
        "should issue no identity cookies on redirect"
    );
    assert!(
        fixture.store.events().is_empty(),
        "should write no identity record on redirect"
    );
}
