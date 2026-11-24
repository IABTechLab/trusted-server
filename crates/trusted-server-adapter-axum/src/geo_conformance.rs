//! Shared geo cases through managed native ingress and the real TS router.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
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
) -> LocalStartupFuture<'_> {
    Box::pin(async move {
        let clock = TestClock::new();
        let transport = ScriptedHttpClient::new(&clock).with_request_observer(move |request| {
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
    worker: Option<JoinHandle<()>>,
    bids: Arc<Mutex<Vec<Value>>>,
}

impl Server {
    fn start(source: Source, store: RecordingStore) -> Self {
        let reservation =
            TcpListener::bind("127.0.0.1:0").expect("should reserve native fixture port");
        let address = reservation
            .local_addr()
            .expect("should read fixture address");
        drop(reservation);
        let (stop, stopped) = oneshot::channel();
        let bids = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&bids);
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
                Source::Located | Source::Untrusted => NativeGeoSelection::CloudFront { policy },
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
                move |app, _| initialize_fixture(app, store, captured),
            )
            .expect("should run managed native fixture");
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        while TcpStream::connect_timeout(&address, Duration::from_millis(20)).is_err() {
            assert!(
                !worker.is_finished(),
                "should keep native fixture alive during startup"
            );
            assert!(
                Instant::now() < deadline,
                "should start native fixture within bound"
            );
            thread::sleep(Duration::from_millis(10));
        }
        Self {
            address,
            stop: Some(stop),
            worker: Some(worker),
            bids,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            worker.join().expect("should stop owned native fixture");
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
            .or_insert_with(|| Server::start(request.location.source, self.store.clone()));
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
