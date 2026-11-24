//! Shared geo cases through managed native ingress and the real TS router.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use edgezero_adapter_axum::dev_server::{
    AxumRunOptions, LocalStartupFuture, run_app_with_options_and_initializer,
};
use edgezero_adapter_axum::geo::{NativeGeoSelection, ProtectedCloudFrontAlb};
use edgezero_core::app::{App, Hooks};
use edgezero_core::error::EdgeError;
use edgezero_core::geo::{GeoError, GeoInput, GeoProvider, GeoProviderId, GeoResolution};
use edgezero_core::http::{HeaderMap, Method, StatusCode};
use edgezero_core::router::RouterService;
use reqwest::blocking::Client;
use serde_json::Value;
use tokio::sync::oneshot;
use trusted_server_core::edge_app::install_edge_policies;
use trusted_server_core::test_support::geo_consent::{self, Adapter, Exchange, Observed, Source};
use trusted_server_core::test_support::identity_conformance::RecordingStore;
use trusted_server_core::test_support::scripted_client::{
    ScriptedHttpClient, ScriptedReply, TestClock,
};

use crate::app::{build_state_with_settings, router_builder};
use crate::identity_conformance::settings;

struct FixtureHooks;

impl Hooks for FixtureHooks {
    fn routes() -> Result<RouterService, EdgeError> {
        Ok(RouterService::builder().build())
    }

    fn configure(app: &mut App) -> Result<(), EdgeError> {
        install_edge_policies(app, HeaderMap::new())
    }

    // The fixture installs no logger and leaves the test process's logging
    // alone. Multiple embedded runners must not install a global backend.
    fn owns_logging() -> bool {
        true
    }
}

struct FailedProvider;

impl GeoProvider for FailedProvider {
    fn resolve(&self, _: &GeoInput) -> GeoResolution {
        GeoResolution::Error(GeoError::ProviderFailure)
    }
}

fn initialize_fixture(
    app: &mut App,
    store: RecordingStore,
    captured: Arc<Mutex<Vec<Value>>>,
    ready: Arc<AtomicBool>,
) -> LocalStartupFuture<'_> {
    Box::pin(async move {
        let clock = TestClock::new();
        let transport = ScriptedHttpClient::new(&clock).with_request_observer(move |request| {
            if request.uri == "https://origin.example.com/api/geo-fixture-ready" {
                ready.store(true, Ordering::Release);
            }
            if request.uri == geo_consent::BIDDER {
                captured
                    .lock()
                    .expect("should lock captured bidder requests")
                    .push(
                        serde_json::from_slice(&request.body)
                            .expect("should record actual OpenRTB"),
                    );
            }
        });
        transport.respond_to(
            Method::GET,
            "https://origin.example.com/api/geo-fixture-ready",
            ScriptedReply::status(StatusCode::OK),
        );
        // All exchanges are sequential. Preload bounded immediate replies
        // so both positive dispatch and absence are externally visible.
        for _ in 0..4096 {
            for path in ["/article", "/image.png", "/api/example"] {
                let mime = if path == "/article" {
                    "text/html"
                } else {
                    "application/json"
                };
                let reply = ScriptedReply::status(StatusCode::OK)
                    .header("content-type", mime)
                    .body("<html><head></head><body><div id=\"example-slot\"></div></body></html>");
                transport.respond_to(
                    Method::GET,
                    &format!("https://origin.example.com{path}"),
                    reply,
                );
            }
            transport.respond_to(
                Method::POST,
                geo_consent::BIDDER,
                ScriptedReply::status(StatusCode::NO_CONTENT),
            );
        }
        let mut configured = settings();
        geo_consent::configure(&mut configured);
        // No RuntimeServices override: production build_runtime_services
        // must read this request's private managed GeoOutcome.
        let mut state =
            build_state_with_settings(configured).expect("should build actual TS state");
        Arc::get_mut(&mut state)
            .expect("should own initializing state")
            .set_ec_store(store);
        app.set_monotonic_clock(clock.monotonic_clock());
        app.set_router(
            router_builder(&state)
                .with_state(transport.http_client())
                .build(),
        );
        Ok(())
    })
}

struct Server {
    address: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<Result<(), String>>>,
    bids: Arc<Mutex<Vec<Value>>>,
}

impl Server {
    fn start(source: Source, store: &RecordingStore) -> Self {
        Self::start_at(source, store, None)
    }

    fn start_at(source: Source, store: &RecordingStore, initial: Option<SocketAddr>) -> Self {
        let probe = Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .expect("should build readiness client");
        let deadline = Instant::now() + Duration::from_secs(15);
        for attempt in 0..5 {
            let reservation =
                TcpListener::bind("127.0.0.1:0").expect("should reserve native fixture port");
            let address = if attempt == 0 { initial } else { None }.unwrap_or_else(|| {
                reservation
                    .local_addr()
                    .expect("should read fixture address")
            });
            drop(reservation);
            let (stop, stopped) = oneshot::channel();
            let bids = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&bids);
            let store = store.clone();
            let ready = Arc::new(AtomicBool::new(false));
            let observed_ready = Arc::clone(&ready);
            let worker = thread::spawn(move || {
                let policy = ProtectedCloudFrontAlb::new(
                    [if source == Source::Untrusted {
                        "192.0.2.1/32"
                    } else {
                        "127.0.0.1/32"
                    }],
                    "x-example-origin-credential",
                )
                .expect("should validate protected fixture peer");
                let selection = match source {
                    Source::Located | Source::Untrusted => {
                        NativeGeoSelection::CloudFront { policy }
                    }
                    Source::Unselected => NativeGeoSelection::Off,
                    Source::Error => NativeGeoSelection::Supplied {
                        policy,
                        id: GeoProviderId::new("example-failure")
                            .expect("should name failure provider"),
                        provider: Arc::new(FailedProvider),
                    },
                };
                let options = AxumRunOptions::new(address)
                    .expect("should build explicit native options")
                    .with_geo(selection);
                run_app_with_options_and_initializer::<FixtureHooks, _, _>(
                    options,
                    async move {
                        let _ = stopped.await;
                    },
                    move |app, _| initialize_fixture(app, store, captured, observed_ready),
                )
                .map_err(|error| error.to_string())
            });
            let mut server = Self {
                address,
                stop: Some(stop),
                worker: Some(worker),
                bids,
            };
            loop {
                if server
                    .worker
                    .as_ref()
                    .expect("should own worker")
                    .is_finished()
                {
                    let result = server
                        .worker
                        .take()
                        .expect("should own finished worker")
                        .join()
                        .expect("should join native startup worker");
                    if result
                        .as_ref()
                        .is_err_and(|error| error == "startup bind: listener: address unavailable")
                        && attempt < 4
                    {
                        break;
                    }
                    result.expect("should run managed native fixture");
                    unreachable!("should keep native fixture alive after startup");
                }
                // Only this attempt's actual application transport can mark
                // readiness; a competing listener cannot satisfy the probe.
                let _ = probe
                    .get(format!("http://{address}/api/geo-fixture-ready"))
                    .header("host", "publisher.example.com")
                    .send();
                if ready.load(Ordering::Acquire) {
                    return server;
                }
                assert!(
                    Instant::now() < deadline,
                    "should start native fixture within bound"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        unreachable!("should return or report bounded startup failure")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                assert!(
                    matches!(result, Ok(Ok(()))),
                    "should stop owned native fixture: {result:?}"
                );
            }
        }
    }
}

struct AxumGeoAdapter {
    servers: HashMap<Source, Server>,
    store: RecordingStore,
    client: Client,
}

impl Adapter for AxumGeoAdapter {
    fn record_is_live(&self, identity: &str) -> bool {
        geo_consent::record_is_live(&self.store, identity)
    }

    fn exchange(&mut self, request: &Exchange<'_>) -> Observed {
        let server = self
            .servers
            .entry(request.location.source)
            .or_insert_with(|| Server::start(request.location.source, &self.store));
        let before = server.bids.lock().expect("should lock observed bids").len();
        let (method, path) = request.target();
        let mut outbound = self
            .client
            .request(method, format!("http://{}{path}", server.address));
        for (name, value) in request.headers() {
            outbound = outbound.header(
                name,
                if name == "origin" {
                    "http://publisher.example.com".to_owned()
                } else {
                    value
                },
            );
        }
        if let Some(country) = request.location.country {
            outbound = outbound.header("cloudfront-viewer-country", country);
        }
        if let Some(region) = request.location.region {
            outbound = outbound.header("cloudfront-viewer-country-region", region);
        }
        let response = outbound
            .body(request.body())
            .send()
            .expect("should execute actual managed HTTP request");
        let status = response.status();
        let headers = response.headers().clone();
        let _body = response
            .bytes()
            .expect("should finish actual response body");
        let bids = server
            .bids
            .lock()
            .expect("should read completed bidder observations")[before..]
            .to_vec();
        Observed {
            status,
            headers,
            bids,
        }
    }
}

#[test]
fn geo_consent_axum() {
    let mut adapter = AxumGeoAdapter {
        servers: HashMap::new(),
        store: RecordingStore::new(false),
        client: Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("should build bounded fixture client"),
    };
    geo_consent::verify(&mut adapter, true, true);
}

#[test]
fn geo_consent_fixture_retries_an_occupied_port() {
    let occupied = TcpListener::bind("127.0.0.1:0").expect("should occupy initial port");
    let address = occupied.local_addr().expect("should read occupied port");
    let server = Server::start_at(Source::Located, &RecordingStore::new(false), Some(address));
    assert_ne!(
        server.address, address,
        "should retry instead of accepting another listener as ready"
    );
}
