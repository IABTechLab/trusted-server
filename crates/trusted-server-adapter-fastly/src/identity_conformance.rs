//! EC conformance through the Fastly application and its production finalizer.

use crate::app::TrustedServerApp;
use crate::sandbox::RetainedApp;
use async_trait::async_trait;
use edgezero_adapter_fastly::delivery::FastlyResponseCommitState;
use edgezero_adapter_fastly::delivery::testing::response_delivery;
use edgezero_adapter_fastly::request::run_request_with_registries_and_hooks_for_test;
use edgezero_core::app::Hooks as _;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Response, StatusCode};
use edgezero_core::ingress::{InboundOrigin, OriginSource};
use edgezero_core::ja4::CapturedJa4;
use edgezero_core::middleware::{Middleware, Next};
use edgezero_core::response_egress_framing::ResponseEgressFramingMode;
use futures::executor::block_on;
use std::cell::RefCell;
use std::sync::Arc;
use trusted_server_core::ec::kv_backend::EcKvStore;
use trusted_server_core::http_util::RuntimeRequestOrigin;
use trusted_server_core::test_support::identity_conformance::{
    Adapter, ApiObservation, ApiRequest, Consent, Geo, Navigation, Observation, RecordingStore,
    Scenario, configure_http_fixture, tcf_consent_cookie, verify_api_identity_expiries,
    verify_failed_ec_creation, verify_http_contract, verify_known_non_browser_navigation,
};
use trusted_server_core::test_support::ja4::{PYTHON_JA4, insert_ja4};
use trusted_server_core::test_support::scripted_client::{
    ScriptedHttpClient, ScriptedReply, TestClock,
};

use edgezero_core::outbound::HttpClient;

use super::{ClientRequestValues, FinalizeInputs, handle_delivery, with_finalization};
use crate::app::assemble_app_with_middleware;
use crate::platform::RequestGeo;
use crate::sandbox::SandboxCounters;
use crate::tinybird::TelemetryQueue;
use fastly::Request as FastlyRequest;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::platform::{ClientInfo, GeoCoordinates};
use trusted_server_core::settings::Settings;

const DOCUMENT: &str = "<html><head></head><body>Example article</body></html>";
const ORIGIN: &str = "https://origin.example.com/article";

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

/// The runtime TLS JA4 of a scenario, which the prepare hook hands to
/// [`RuntimeTlsJa4`].
#[derive(Clone, Copy)]
struct FixtureTlsJa4(&'static str);

/// Records a scenario's JA4 in the [`RequestIngress`] `EdgeZero` captured.
///
/// `EdgeZero`'s Fastly ingress records `get_tls_ja4()` of the client
/// request, which Viceroy cannot supply for a synthetic request, and drops any
/// ingress a prepare hook supplies. This fills the claim that capture would
/// make, so the app reads it through `ctx.ja4()`'s source as in production.
///
/// [`RequestIngress`]: edgezero_core::ingress::RequestIngress
struct RuntimeTlsJa4;

#[async_trait(?Send)]
impl Middleware for RuntimeTlsJa4 {
    async fn handle(&self, mut ctx: RequestContext, next: Next<'_>) -> Result<Response, EdgeError> {
        if let Some(FixtureTlsJa4(ja4)) = ctx.extensions_mut().remove::<FixtureTlsJa4>() {
            insert_ja4(
                ctx.extensions_mut(),
                CapturedJa4::from_runtime_tls(Some(ja4)),
            );
        }
        next.run(ctx).await
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
}

impl FastlyAdapter {
    fn new(store: RecordingStore) -> Self {
        let settings = settings();
        let clock = TestClock::new();
        let origin = ScriptedHttpClient::new(&clock);
        let mut retained = {
            let _guard = StoreGuard::install(&store);
            assemble_app_with_middleware(settings.clone(), RuntimeTlsJa4)
                .expect("should assemble the conformance application")
        };
        retained.app.set_monotonic_clock(clock.monotonic_clock());
        Self {
            settings,
            retained,
            origin,
            clock,
            store,
        }
    }
}

impl Adapter for FastlyAdapter {
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

impl FastlyAdapter {
    fn execute(&mut self, navigation: &Navigation, api: Option<&ApiRequest>) -> ApiObservation {
        let before_requests = self.origin.requests().len();
        if let Some(api) = api {
            let mut configured = self.settings.clone();
            configured.auction.enabled = api.auction_enabled;
            configured.ec.ec_store = api.store_enabled.then(|| "trusted_server_kv".to_owned());
            let _guard = StoreGuard::install(&self.store);
            self.retained = assemble_app_with_middleware(configured, RuntimeTlsJa4)
                .expect("should assemble HTTP conformance app");
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
                reply
                    .header("cache-control", "public, max-age=600, s-maxage=600")
                    .header("surrogate-control", "max-age=600")
                    .header("cdn-cache-control", "public, max-age=600")
                    .header("cloudflare-cdn-cache-control", "public, max-age=600")
                    .body(DOCUMENT)
            },
        );
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
        let inputs = FinalizeInputs {
            state: self.retained.state.as_ref(),
            geo: geo.as_ref(),
            counters: &counters,
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
            .with_header("user-agent", navigation.user_agent)
            .with_header("cookie", cookies.join("; "));
        if let Some(api) = api {
            request = FastlyRequest::new(
                api.method.as_str(),
                format!("https://publisher.example.com{}", api.path),
            );
            request.set_header("user-agent", navigation.user_agent);
            for (name, value) in &api.headers {
                request.set_header(name.as_str(), value.as_str());
            }
            request.set_body(api.body.clone());
        }
        // Production's native read and `prepare` run unchanged. The fixture
        // replaces only the values Viceroy cannot supply: a routable client IP,
        // its geo result and, where the scenario has one, the runtime TLS JA4
        // that `RuntimeTlsJa4` records in EdgeZero's ingress.
        let mut values = ClientRequestValues::read(&mut request, &self.settings, false);
        let ip = Some("192.0.2.7".parse().expect("should parse fixture IP"));
        values.client_info = ClientInfo {
            client_ip: ip,
            ..ClientInfo::default()
        };
        values.request_geo = if navigation.geo == Geo::Failed {
            RequestGeo::failing(ip)
        } else {
            RequestGeo::new(ip, geo.clone())
        };
        let queue = TelemetryQueue::default();
        let mut observation = None;
        let mut cache_control = None;
        let mut edge_cache_headers_present = false;
        let origin = self.origin.clone();
        let delivery = with_finalization(&inputs, |finalize| {
            run_request_with_registries_and_hooks_for_test(
                &self.retained.app,
                TrustedServerApp::stores(),
                request,
                |_request, extensions| {
                    values.prepare(extensions, &self.settings, &queue);
                    // A known non-browser API client sends a browser
                    // User-Agent over a non-browser TLS stack, whose runtime
                    // JA4 the shared gate refuses.
                    let ja4 = if api.is_some_and(|request| request.known_non_browser) {
                        Some(PYTHON_JA4)
                    } else {
                        navigation.ja4
                    };
                    if let Some(ja4) = ja4 {
                        extensions.insert(FixtureTlsJa4(ja4));
                    }
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
                    self.store.response_released();
                    let status = response.status();
                    cache_control = response
                        .headers()
                        .get("cache-control")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    edge_cache_headers_present = [
                        "surrogate-control",
                        "cdn-cache-control",
                        "cloudflare-cdn-cache-control",
                    ]
                    .iter()
                    .any(|name| response.headers().contains_key(*name));
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
        ApiObservation {
            response: observation.expect("should deliver a finalized response"),
            origin_requests: self.origin.requests().len() - before_requests,
            cache_control,
            edge_cache_headers_present,
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

#[test]
fn known_non_browser_navigation_fastly() {
    let store = RecordingStore::new(false);
    verify_known_non_browser_navigation(&mut FastlyAdapter::new(store.clone()), &store);
}

#[test]
fn api_identity_expiries_fastly() {
    verify_api_identity_expiries(&mut FastlyAdapter::new(RecordingStore::new(false)));
}
