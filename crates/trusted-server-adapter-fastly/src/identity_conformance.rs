//! EC conformance through the Fastly application and its production finalizer.

use crate::app::TrustedServerApp;
use crate::sandbox::RetainedApp;
use edgezero_adapter_fastly::delivery::FastlyResponseCommitState;
use edgezero_adapter_fastly::delivery::testing::response_delivery;
use edgezero_adapter_fastly::request::run_request_with_registries_and_hooks_for_test;
use edgezero_core::app::Hooks as _;
use edgezero_core::http::StatusCode;
use edgezero_core::ingress::{InboundOrigin, OriginSource};
use edgezero_core::response_egress_framing::ResponseEgressFramingMode;
use futures::executor::block_on;
use std::cell::RefCell;
use std::sync::Arc;
use trusted_server_core::ec::kv_backend::EcKvStore;
use trusted_server_core::http_util::RuntimeRequestOrigin;
use trusted_server_core::test_support::identity_conformance::{
    Adapter, ApiObservation, ApiRequest, Consent, Geo, Navigation, Observation, RecordingStore,
    Scenario, configure_http_fixture, tcf_consent_cookie, verify_failed_ec_creation,
    verify_http_contract,
};
use trusted_server_core::test_support::scripted_client::{
    ScriptedHttpClient, ScriptedReply, TestClock,
};

use edgezero_core::outbound::HttpClient;

use super::{
    ClientRequestValues, FinalizeInputs, assemble_app, handle_delivery, take_navigation_lookups,
    with_finalization,
};
use crate::platform::RequestGeo;
use crate::sandbox::SandboxCounters;
use crate::tinybird::TelemetryQueue;
use fastly::Request as FastlyRequest;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::platform::{ClientInfo, GeoCoordinates};
use trusted_server_core::redacted::Redacted;
use trusted_server_core::settings::{Settings, TrustedClientIpConfig};
use trusted_server_core::test_support::geo_consent::{self, Exchange, Observed, Source};

const DOCUMENT: &str = "<html><head></head><body>Example article</body></html>";
const ORIGIN: &str = "https://origin.example.com/article";
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/146.0.0.0 Safari/537.36";

thread_local! {
    static STORE: RefCell<Option<RecordingStore>> = const { RefCell::new(None) };
}

/// Returns the installed fixture store for the conformance store name.
///
/// The identity app takes its record store from here while a conformance
/// runner assembles the application, so the store it holds for every request
/// is the fixture.
pub(crate) fn fixture_store(store_name: &str) -> Option<Arc<dyn EcKvStore + Send + Sync>> {
    if store_name == "trusted_server_kv" {
        STORE.with_borrow(|store| {
            store
                .clone()
                .map(|store| Arc::new(store) as Arc<dyn EcKvStore + Send + Sync>)
        })
    } else {
        None
    }
}

struct StoreGuard(Option<RecordingStore>);

impl StoreGuard {
    fn install(store: &RecordingStore) -> Self {
        Self(STORE.replace(Some(store.clone())))
    }
}

impl Drop for StoreGuard {
    fn drop(&mut self) {
        STORE.replace(self.0.take());
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
        ec_store = "trusted_server_kv"
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

struct FastlyAdapter {
    settings: Settings,
    retained: RetainedApp,
    origin: ScriptedHttpClient,
    clock: TestClock,
    store: RecordingStore,
    forwarding: Forwarding,
    geo_configured: bool,
    stale_origin_geo: bool,
}

impl FastlyAdapter {
    fn new(store: RecordingStore) -> Self {
        let settings = settings();
        let clock = TestClock::new();
        let origin = ScriptedHttpClient::new(&clock);
        let mut retained = {
            let _guard = StoreGuard::install(&store);
            assemble_app(Ok(settings.clone()), &"")
                .expect("should assemble the conformance application")
        };
        retained.app.set_monotonic_clock(clock.monotonic_clock());
        Self {
            settings,
            retained,
            origin,
            clock,
            store,
            forwarding: Forwarding::Trusted,
            geo_configured: false,
            stale_origin_geo: false,
        }
    }
}

impl Adapter for FastlyAdapter {
    fn navigate(&mut self, navigation: &Navigation) -> Observation {
        self.execute(navigation, None, None).response
    }

    fn exchange(&mut self, request: &ApiRequest) -> ApiObservation {
        self.execute(
            &Navigation {
                geo: request.geo,
                ..Navigation::default()
            },
            Some(request),
            None,
        )
    }
}

impl FastlyAdapter {
    fn execute(
        &mut self,
        navigation: &Navigation,
        api: Option<&ApiRequest>,
        geo_request: Option<&Exchange<'_>>,
    ) -> ApiObservation {
        let before_requests = self.origin.requests().len();
        if let Some(api) = api.filter(|_| geo_request.is_none() || !self.geo_configured) {
            let mut configured = self.settings.clone();
            configured.auction.enabled = api.auction_enabled;
            configured.ec.ec_store = api.store_enabled.then(|| "trusted_server_kv".to_owned());
            if geo_request.is_some() {
                geo_consent::configure(&mut configured);
                configured.trusted_client_ip = Some(TrustedClientIpConfig {
                    ip_header: "x-example-reader-ip".to_owned(),
                    auth_header: "x-example-reader-auth".to_owned(),
                    shared_secret: Redacted::new("fictional-shared-secret-0123456789".to_owned()),
                });
            }
            let _guard = StoreGuard::install(&self.store);
            self.retained =
                assemble_app(Ok(configured), &"").expect("should assemble HTTP conformance app");
            self.geo_configured = geo_request.is_some();
            // Rebuilt store/no-store applications keep the outbound double's
            // clock domain, including ingress and response deadlines.
            self.retained
                .app
                .set_monotonic_clock(self.clock.monotonic_clock());
        }
        self.origin.respond_to(
            api.map_or(edgezero_core::http::Method::GET, |request| {
                request.method.clone()
            }),
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
                let reply = if self.stale_origin_geo {
                    reply
                        .header("x-geo-country", "DE")
                        .header("x-geo-region", "BE")
                } else {
                    reply
                };
                if geo_request.is_some_and(|request| request.kind == geo_consent::Kind::Document) {
                    reply.body(
                        "<html><head></head><body><div id=\"example-slot\"></div></body></html>",
                    )
                } else {
                    reply.body(DOCUMENT)
                }
            },
        );
        if geo_request.is_some() {
            self.origin.respond_to(
                edgezero_core::http::Method::POST,
                geo_consent::BIDDER,
                ScriptedReply::status(StatusCode::NO_CONTENT),
            );
        }
        let geo = match navigation.geo {
            Geo::Gdpr => Some(GeoInfo {
                country: "DE".to_owned(),
                city: None,
                continent: Some("EU".to_owned()),
                coordinates: Some(GeoCoordinates {
                    latitude: 0.0,
                    longitude: 0.0,
                }),
                metro_code: Some(0),
                region: None,
                asn: None,
            }),
            Geo::UsCalifornia => Some(GeoInfo::country_region("US", Some("CA".to_owned()))),
            Geo::Missing | Geo::Failed => None,
        };
        let counters = SandboxCounters {
            ordinal: 1,
            builds: 1,
            request_id: "example-conformance-request".to_owned(),
        };
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
        // Model browser Fetch Metadata; the shared classifier uses the
        // destination and does not require Sec-Fetch-Mode.
        let mut request = FastlyRequest::get("https://publisher.example.com/article")
            .with_header("sec-fetch-dest", navigation.destination)
            .with_header(
                "sec-fetch-mode",
                if document { "navigate" } else { "no-cors" },
            )
            .with_header("accept", if document { "text/html" } else { "*/*" })
            .with_header("user-agent", USER_AGENT)
            .with_header("cookie", cookies.join("; "));
        if let Some(api) = api {
            request = FastlyRequest::new(
                api.method.as_str(),
                format!("https://publisher.example.com{}", api.path),
            );
            if !api
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
            {
                request.set_header("user-agent", USER_AGENT);
            }
            for (name, value) in &api.headers {
                request.append_header(name.as_str(), value.as_str());
            }
            request.set_body(api.body.clone());
        }
        // Production's native read and `prepare` run unchanged. The fixture
        // replaces only the values Viceroy cannot supply: a routable client IP,
        // its geo result and, for browser scenarios, JA4-backed device signals.
        let mut producer_settings = self.settings.clone();
        if geo_request.is_some() {
            producer_settings.trusted_client_ip = Some(TrustedClientIpConfig {
                ip_header: "x-example-reader-ip".to_owned(),
                auth_header: "x-example-reader-auth".to_owned(),
                shared_secret: Redacted::new("fictional-shared-secret-0123456789".to_owned()),
            });
        }
        let mut values = ClientRequestValues::read(&mut request, &producer_settings, false);
        let ip = Some("192.0.2.7".parse().expect("should parse fixture IP"));
        if geo_request.is_none() {
            values.client_info = ClientInfo {
                client_ip: ip,
                ..ClientInfo::default()
            };
            values.request_geo = if navigation.geo == Geo::Failed {
                RequestGeo::failing(ip)
            } else {
                RequestGeo::new(ip, geo.clone())
            };
        }
        if geo_request.is_some_and(|request| request.location.source == Source::Error) {
            values.request_geo = RequestGeo::failing(values.client_info.client_ip);
        }
        let held_geo = values.request_geo.geo().cloned();
        let inputs = FinalizeInputs {
            state: self.retained.state.as_ref(),
            geo: held_geo.as_ref(),
            counters: &counters,
        };
        if navigation.browser && geo_request.is_none() {
            values.device_signals = Some(DeviceSignals::derive(
                USER_AGENT,
                Some("t13d1516h2_example_example"),
                None,
            ));
        }
        let queue = TelemetryQueue::default();
        let mut observation = None;
        let mut cache_control = None;
        let mut headers = None;
        let origin = self.origin.clone();
        // As in `serve`, drop identity lookups an earlier request on this
        // thread left in the request-local slot.
        drop(take_navigation_lookups());
        let delivery = with_finalization(&inputs, |finalize| {
            run_request_with_registries_and_hooks_for_test(
                &self.retained.app,
                TrustedServerApp::stores(),
                request,
                |_request, extensions| {
                    values.prepare(extensions, &producer_settings, &queue);
                    extensions.insert(HttpClient::with_client(origin));
                    // This synthetic request bypasses runtime URI capture.
                    extensions.insert(RuntimeRequestOrigin::new(
                        InboundOrigin::parse(
                            "https",
                            "publisher.example.com",
                            OriginSource::RuntimeUri,
                        )
                        .expect("should validate fixture public origin"),
                    ));
                },
                finalize,
                |envelope| {
                    let (prepared, _policy, mut attempt, clock) = envelope
                        .begin(ResponseEgressFramingMode::HostAutomatic)
                        .unwrap_or_else(|_| panic!("should prepare the document response"));
                    let response = prepared.into_response();
                    headers = Some(response.headers().clone());
                    self.store.response_released();
                    let status = response.status();
                    cache_control = response
                        .headers()
                        .get("cache-control")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    let cookies = response
                        .headers()
                        .get_all("set-cookie")
                        .iter()
                        .map(|value| {
                            value
                                .to_str()
                                .expect("should decode response cookie")
                                .to_owned()
                        })
                        .collect();
                    assert!(
                        attempt.begin_writing(),
                        "should start releasing the response"
                    );
                    let body = block_on(response.into_body().into_bytes_bounded(1024 * 1024))
                        .expect("should consume the complete document");
                    assert!(
                        attempt.account_bytes(body.len() as u64, clock.now()),
                        "should account body bytes"
                    );
                    assert!(
                        attempt.complete(clock.now()),
                        "should complete the document response"
                    );
                    observation = Some(Observation {
                        status,
                        cookies,
                        body: body.to_vec(),
                    });
                    response_delivery(
                        FastlyResponseCommitState::Committed,
                        attempt.into_terminal_report(),
                        Ok(()),
                    )
                },
            )
        });
        handle_delivery(delivery, &counters, |_| {});
        drop(take_navigation_lookups());
        ApiObservation {
            headers: headers.expect("should capture finalized headers"),
            bids: self.origin.requests()[before_requests..]
                .iter()
                .filter(|request| request.uri == geo_consent::BIDDER)
                .map(|request| {
                    serde_json::from_slice(&request.body).expect("should record OpenRTB")
                })
                .collect(),
            response: observation.expect("should deliver a finalized response"),
            origin_requests: self.origin.requests().len() - before_requests,
            cache_control,
        }
    }
}

#[test]
fn identity_conformance_fastly() {
    for scenario in Scenario::NAVIGATION_SCENARIOS {
        let store = RecordingStore::new(scenario == Scenario::WriteFailure);
        let mut adapter = FastlyAdapter::new(store.clone());
        log::info!("Running Fastly identity scenario {scenario:?}");
        scenario.verify(&mut adapter, &store);
    }
    let store = RecordingStore::new(false);
    let mut adapter = FastlyAdapter::new(store.clone());
    verify_http_contract(&mut adapter, &store);
    let store = RecordingStore::new(true);
    let mut adapter = FastlyAdapter::new(store.clone());
    verify_failed_ec_creation(&mut adapter, &store);
}

#[derive(Clone, Copy)]
enum Forwarding {
    Trusted,
    DuplicatedIp,
    DuplicatedAuth,
    MalformedIp,
    MissingAuth,
    Ipv6,
}

impl geo_consent::Adapter for FastlyAdapter {
    fn record_is_live(&self, identity: &str) -> bool {
        geo_consent::record_is_live(&self.store, identity)
    }

    fn exchange(&mut self, request: &Exchange<'_>) -> Observed {
        let (method, path) = request.target();
        let mut api = ApiRequest::new(method, path);
        api.headers = request
            .headers()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect();
        let index = geo_consent::LOCATIONS
            .iter()
            .position(|location| location.name == request.location.name)
            .expect("should find shared geo row");
        api.headers.push((
            "x-example-reader-ip".to_owned(),
            format!("192.0.2.{}", index + 1),
        ));
        api.headers.push((
            "x-example-reader-auth".to_owned(),
            if request.location.source == Source::Untrusted {
                "forged"
            } else {
                "fictional-shared-secret-0123456789"
            }
            .to_owned(),
        ));
        match self.forwarding {
            Forwarding::Trusted => {}
            Forwarding::DuplicatedIp => api
                .headers
                .push(("x-example-reader-ip".to_owned(), "198.51.100.7".to_owned())),
            Forwarding::DuplicatedAuth => api.headers.push((
                "x-example-reader-auth".to_owned(),
                "fictional-shared-secret-0123456789".to_owned(),
            )),
            Forwarding::MalformedIp => {
                api.headers
                    .retain(|(name, _)| name != "x-example-reader-ip");
                api.headers.push((
                    "x-example-reader-ip".to_owned(),
                    "192.0.2.3, 198.51.100.7".to_owned(),
                ));
            }
            Forwarding::MissingAuth => api
                .headers
                .retain(|(name, _)| name != "x-example-reader-auth"),
            Forwarding::Ipv6 => {
                api.headers
                    .retain(|(name, _)| name != "x-example-reader-ip");
                api.headers
                    .push(("x-example-reader-ip".to_owned(), "2001:db8::7".to_owned()));
                api.headers
                    .push(("x-forwarded-for".to_owned(), "192.0.2.1".to_owned()));
            }
        }
        api.body = request.body();
        api.origin_content_type = if request.kind == geo_consent::Kind::Document {
            "text/html"
        } else {
            "application/json"
        };
        let result = self.execute(&Navigation::default(), Some(&api), Some(request));
        Observed {
            status: result.response.status,
            headers: result.headers,
            bids: result.bids,
        }
    }
}

#[test]
#[ignore = "requires test-only Viceroy geo mapping; run scripts/test-geo-consent.mjs"]
fn geo_consent_fastly() {
    let mut adapter = FastlyAdapter::new(RecordingStore::new(false));
    geo_consent::verify(&mut adapter, false, false);
    verify_deferred_stale_origin_headers(&mut adapter);
}

fn verify_deferred_stale_origin_headers(adapter: &mut FastlyAdapter) {
    // Pin today's deferred response-header behavior, while proving that neither
    // consent decision consumes these stale origin facts. A future clearing
    // fix must update this assertion deliberately. Deferral:
    // https://github.com/IABTechLab/trusted-server/pull/1532#discussion_r4569600903
    adapter.stale_origin_geo = true;
    for signal in [geo_consent::Signal::None, geo_consent::Signal::Purpose1] {
        let result = geo_consent::Adapter::exchange(
            adapter,
            &Exchange {
                location: *geo_consent::LOCATIONS
                    .iter()
                    .find(|location| location.name == "missing")
                    .expect("should find no-location row"),
                signal,
                kind: geo_consent::Kind::Document,
                ec: None,
            },
        );
        assert_eq!(
            result.status,
            StatusCode::OK,
            "should preserve publisher response"
        );
        assert_eq!(
            result.headers["x-geo-info-available"], "false",
            "should report no trusted location"
        );
        assert_eq!(
            result
                .headers
                .get("x-geo-country")
                .and_then(|value| value.to_str().ok()),
            Some("DE"),
            "should explicitly preserve deferred origin country behavior"
        );
        assert_eq!(
            result
                .headers
                .get("x-geo-region")
                .and_then(|value| value.to_str().ok()),
            Some("BE"),
            "should explicitly preserve deferred origin region behavior"
        );
        assert!(
            !result.headers.contains_key("set-cookie"),
            "should refuse EC creation despite stale origin country"
        );
        assert_eq!(
            result.bids.len(),
            usize::from(signal == geo_consent::Signal::Purpose1),
            "should apply Unknown auction policy despite stale origin country"
        );
        for bid in result.bids {
            assert!(
                bid["device"]["geo"].is_null(),
                "should not export stale origin geography"
            );
        }
    }
}

#[test]
#[ignore = "requires test-only Viceroy geo mapping; run scripts/test-geo-consent.mjs"]
fn geo_consent_fastly_forwarding() {
    let mut adapter = FastlyAdapter::new(RecordingStore::new(false));
    for forwarding in [
        Forwarding::DuplicatedIp,
        Forwarding::DuplicatedAuth,
        Forwarding::MalformedIp,
        Forwarding::MissingAuth,
    ] {
        adapter.forwarding = forwarding;
        for signal in [geo_consent::Signal::None, geo_consent::Signal::Purpose1] {
            for kind in [geo_consent::Kind::Document, geo_consent::Kind::Auction] {
                let result = geo_consent::Adapter::exchange(
                    &mut adapter,
                    &Exchange {
                        location: *geo_consent::LOCATIONS
                            .iter()
                            .find(|location| location.name == "us-ca")
                            .expect("should find US-CA forwarding case"),
                        signal,
                        kind,
                        ec: None,
                    },
                );
                assert!(
                    result.status.is_success(),
                    "should accept rejected forwarding input without crashing"
                );
                assert_eq!(
                    result.headers["x-geo-info-available"], "false",
                    "should have no location when rejected forwarding leaves no peer IP"
                );
                assert!(
                    !result.headers.contains_key("set-cookie"),
                    "should not create EC without trusted location"
                );
                assert_eq!(
                    result.bids.len(),
                    usize::from(signal == geo_consent::Signal::Purpose1),
                    "should apply Unknown auction policy"
                );
                for bid in result.bids {
                    assert!(
                        bid["device"]["geo"].is_null(),
                        "should not export forged location"
                    );
                }
            }
        }
    }
    adapter.forwarding = Forwarding::Ipv6;
    let result = geo_consent::Adapter::exchange(
        &mut adapter,
        &Exchange {
            location: *geo_consent::LOCATIONS
                .iter()
                .find(|location| location.name == "us-ca")
                .expect("should find US-CA forwarding case"),
            signal: geo_consent::Signal::None,
            kind: geo_consent::Kind::Auction,
            ec: None,
        },
    );
    assert_eq!(
        result.headers["x-geo-country"], "US",
        "should use authenticated IPv6 reader instead of forged forwarding"
    );
    assert_eq!(
        result.headers["x-geo-region"], "CA",
        "should retain IPv6 reader subdivision"
    );
    assert_eq!(
        result.bids[0]["device"]["geo"]["country"],
        serde_json::json!("US"),
        "should export trusted IPv6 location"
    );
}
