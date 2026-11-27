//! EC conformance through the Fastly application and its production finalizer.

use crate::app::TrustedServerApp;
use crate::sandbox::RetainedApp;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use edgezero_adapter_fastly::delivery::FastlyResponseCommitState;
use edgezero_adapter_fastly::delivery::testing::response_delivery;
use edgezero_adapter_fastly::request::run_request_with_registries_and_hooks_for_test;
use edgezero_core::app::Hooks as _;
use edgezero_core::http::StatusCode;
use edgezero_core::response_egress_framing::ResponseEgressFramingMode;
use futures::executor::block_on;
use std::cell::RefCell;
use std::sync::Arc;
use trusted_server_core::ec::kv_backend::EcKvStore;
use trusted_server_core::test_support::identity_conformance::{
    Adapter, Consent, Geo, Navigation, Observation, RecordingStore, Scenario,
};
use trusted_server_core::test_support::scripted_client::{
    ScriptedHttpClient, ScriptedReply, TestClock,
};

use edgezero_core::outbound::HttpClient;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    ClientRequestValues, FinalizeInputs, assemble_app, handle_delivery, with_finalization,
};
use crate::platform::RequestGeo;
use crate::sandbox::SandboxCounters;
use crate::tinybird::TelemetryQueue;
use fastly::Request as FastlyRequest;
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::platform::{ClientInfo, GeoCoordinates};
use trusted_server_core::settings::Settings;

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
    Settings::from_toml(
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
    .expect("should parse conformance settings")
}

fn consent_cookie(granted: bool) -> String {
    let mut core = [0_u8; 32];
    // TCF v2, followed by current Created and LastUpdated decisecond fields.
    core[0] = 2 << 2;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("should read fixture time")
        .as_secs()
        * 10;
    for offset in [6, 42] {
        for bit in 0..36 {
            if now & (1 << (35 - bit)) != 0 {
                core[(offset + bit) / 8] |= 1 << (7 - (offset + bit) % 8);
            }
        }
    }
    // Purpose 1 is the first purpose-consent bit, at offset 152.
    core[19] = if granted { 0x80 } else { 0 };
    format!("euconsent-v2={}", URL_SAFE_NO_PAD.encode(core))
}

struct FastlyAdapter {
    settings: Settings,
    retained: RetainedApp,
    origin: ScriptedHttpClient,
    store: RecordingStore,
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
            store,
        }
    }
}

impl Adapter for FastlyAdapter {
    fn navigate(&mut self, navigation: &Navigation) -> Observation {
        self.origin.respond_to(
            edgezero_core::http::Method::GET,
            ORIGIN,
            ScriptedReply::status(StatusCode::OK)
                .header("content-type", "text/html")
                .body(DOCUMENT),
        );
        let geo = (navigation.geo == Geo::Gdpr).then(|| GeoInfo {
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
        });
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
            Consent::Granted => cookies.push(consent_cookie(true)),
            Consent::Denied => cookies.push(consent_cookie(false)),
            Consent::Missing => {}
        }
        if let Some(identity) = &navigation.ec_cookie {
            cookies.push(format!("ts-ec={identity}"));
        }
        let document = navigation.destination == "document";
        // A browser sends `navigate` only for a document navigation, and the
        // page-load write permit requires both headers.
        let mut request = FastlyRequest::get("https://publisher.example.com/article")
            .with_header("sec-fetch-dest", navigation.destination)
            .with_header(
                "sec-fetch-mode",
                if document { "navigate" } else { "no-cors" },
            )
            .with_header("accept", if document { "text/html" } else { "*/*" })
            .with_header("user-agent", USER_AGENT)
            .with_header("cookie", cookies.join("; "));
        // Production's native read and `prepare` run unchanged. The fixture
        // replaces only the values Viceroy cannot supply: a routable client IP,
        // its geo result and, for browser scenarios, JA4-backed device signals.
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
        if navigation.browser {
            values.device_signals = Some(DeviceSignals::derive(
                USER_AGENT,
                Some("t13d1516h2_example_example"),
                None,
            ));
        }
        let queue = TelemetryQueue::default();
        let mut observation = None;
        let origin = self.origin.clone();
        let delivery = with_finalization(&inputs, |finalize| {
            run_request_with_registries_and_hooks_for_test(
                &self.retained.app,
                TrustedServerApp::stores(),
                request,
                |_request, extensions| {
                    values.prepare(extensions, &self.settings, &queue);
                    extensions.insert(HttpClient::with_client(origin));
                },
                finalize,
                |envelope| {
                    let (prepared, _policy, mut attempt, clock) = envelope
                        .begin(ResponseEgressFramingMode::HostAutomatic)
                        .unwrap_or_else(|_| panic!("should prepare the document response"));
                    let response = prepared.into_response();
                    self.store.response_released();
                    let status = response.status();
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
        observation.expect("should deliver a finalized response")
    }
}

#[test]
fn identity_conformance_fastly() {
    for scenario in Scenario::FASTLY_BASELINE {
        let store = RecordingStore::new(scenario == Scenario::WriteFailure);
        let mut adapter = FastlyAdapter::new(store.clone());
        log::info!("Running Fastly identity scenario {scenario:?}");
        scenario.verify(&mut adapter, &store);
    }
}
