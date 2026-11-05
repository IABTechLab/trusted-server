//! Real managed ingress and TS auth/diagnostic path, with only stores replaced.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use edgezero_adapter_axum::dev_server::{AxumRunOptions, run_app_with_options_and_initializer};
use edgezero_adapter_axum::geo::{MaxMindGeoConfig, NativeGeoSelection, ReaderIpExpectation};
use edgezero_adapter_axum::proxy::{ForwardingHeaderFamily, TrustedProxyPolicy};
use edgezero_core::app::Hooks;
use edgezero_core::key_value_store::{KvError, KvPage};
use edgezero_core::router::RouterService;
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::oneshot;
use trusted_server_core::platform::PlatformKvStore;
use trusted_server_core::test_support::nextjs_auction;

use super::{ConfigSnapshot, NativeStoreSnapshot, SecretSnapshot};
use crate::app::{TrustedServerApp, build_state_with_settings};
use crate::lab_store_check::{PATH, PROOF_KEY};

struct ReadOnlyProof(Arc<AtomicUsize>);

#[async_trait(?Send)]
impl PlatformKvStore for ReadOnlyProof {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        assert_eq!(key, PROOF_KEY);
        Ok(Some(Bytes::from_static(b"retained-proof")))
    }

    async fn put_bytes(&self, _: &str, _: Bytes) -> Result<(), KvError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(KvError::Unavailable)
    }

    async fn put_bytes_with_ttl(&self, _: &str, _: Bytes, _: Duration) -> Result<(), KvError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(KvError::Unavailable)
    }

    async fn delete(&self, _: &str) -> Result<(), KvError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(KvError::Unavailable)
    }

    async fn list_keys_page(&self, _: &str, _: Option<&str>, _: usize) -> Result<KvPage, KvError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(KvError::Unavailable)
    }
}

struct DiagnosticApp;

impl Hooks for DiagnosticApp {
    fn name() -> &'static str {
        "ProtectedDiagnosticFixture"
    }

    fn routes() -> RouterService {
        TrustedServerApp::routes()
    }

    fn owns_logging() -> bool {
        true
    }
}

struct Fixture {
    _directory: TempDir,
    address: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    runner: Option<thread::JoinHandle<()>>,
    client: Client,
    unexpected_store_calls: Arc<AtomicUsize>,
}

impl Fixture {
    fn start(trusted_cidrs: Option<&[&str]>, expectation: ReaderIpExpectation) -> Self {
        let directory = tempfile::tempdir().expect("should create isolated Geo snapshot");
        let database = directory.path().join("city.mmdb");
        std::fs::write(
            &database,
            include_bytes!("../tests/fixtures/geoip/city.mmdb"),
        )
        .expect("should write synthetic database");
        let listener = TcpListener::bind("127.0.0.1:0").expect("should reserve fixture address");
        let address = listener
            .local_addr()
            .expect("should obtain fixture address");
        drop(listener);
        let mut options = AxumRunOptions::new(address)
            .expect("should configure managed runner")
            .with_request_records(false)
            .with_geo(NativeGeoSelection::MaxMind {
                config: MaxMindGeoConfig::new(database, expectation),
            });
        if let Some(cidrs) = trusted_cidrs {
            options = options.with_trusted_proxy_policy(
                TrustedProxyPolicy::new(ForwardingHeaderFamily::XForwarded, cidrs.iter().copied())
                    .expect("should configure explicit proxy trust"),
            );
        }
        let (stop, stopped) = oneshot::channel();
        let unexpected_store_calls = Arc::new(AtomicUsize::new(0));
        let store_calls = Arc::clone(&unexpected_store_calls);
        let runner = thread::spawn(move || {
            run_app_with_options_and_initializer::<DiagnosticApp, _, _>(
                options,
                async {
                    let _ = stopped.await;
                },
                |app, _| {
                    Box::pin(async move {
                        let mut settings = nextjs_auction::settings();
                        settings.auction.enabled = false;
                        settings.creative_opportunities = None;
                        let mut state = build_state_with_settings(settings)
                            .expect("should initialize real TS routes");
                        Arc::get_mut(&mut state)
                            .expect("should uniquely own fixture state")
                            .config_sha256 = Some("verified-fixture-checksum".to_owned());
                        app.insert_state(state);
                        app.insert_state(Arc::new(NativeStoreSnapshot {
                            config_store: Arc::new(ConfigSnapshot {
                                key: String::new(),
                                envelope: String::new(),
                            }),
                            secret_store: Arc::new(SecretSnapshot(Default::default())),
                            kv_store: Arc::new(ReadOnlyProof(store_calls)),
                        }));
                        Ok(())
                    })
                },
            )
            .expect("should finish managed runner cleanly");
        });
        let fixture = Self {
            _directory: directory,
            address,
            stop: Some(stop),
            runner: Some(runner),
            unexpected_store_calls,
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("should create bounded local client"),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if fixture.client.get(fixture.url("/health")).send().is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "should start managed runner");
            thread::sleep(Duration::from_millis(20));
        }
        fixture
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn request(&self, xff: Option<&str>) -> Response {
        let mut request = self
            .client
            .get(self.url(PATH))
            .header("host", nextjs_auction::PUBLISHER_HOST)
            .header("fly-client-ip", "192.0.2.2")
            .header("cloudfront-viewer-country", "DE")
            .basic_auth("admin", Some("admin-pass"));
        if let Some(xff) = xff {
            request = request.header("x-forwarded-for", xff);
        }
        request
            .send()
            .expect("should call real protected diagnostic")
    }

    fn diagnostic(&self, xff: Option<&str>) -> Value {
        let response = self.request(xff);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let result: Value = response.json().expect("should decode diagnostic");
        assert_eq!(result["config_sha256"], "verified-fixture-checksum");
        assert_eq!(result["scheme"], "http");
        assert_eq!(result["host"], nextjs_auction::PUBLISHER_HOST);
        assert_eq!(result["key"], PROOF_KEY);
        assert_eq!(result["value"], "retained-proof");
        assert_eq!(result["ingress"]["socket_peer"], "127.0.0.1");
        assert_eq!(result["ingress"]["forwarded_for"], Value::Null);
        assert_eq!(result["ingress"]["forwarding_headers_stripped"], true);
        assert_eq!(result["ingress"]["fly_client_ip"], "192.0.2.2");
        result
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.stop.take().expect("should own stop channel").send(());
        self.runner
            .take()
            .expect("should own runner")
            .join()
            .expect("should join managed runner");
        assert_eq!(
            self.unexpected_store_calls.load(Ordering::Relaxed),
            0,
            "GET and denied requests must not mutate or list the proof store"
        );
    }
}

#[test]
fn diagnostic_requires_basic_auth_and_get_never_writes() {
    let fixture = Fixture::start(None, ReaderIpExpectation::PeerAllowed);
    for password in [None, Some("wrong-password")] {
        let mut request = fixture
            .client
            .get(fixture.url(PATH))
            .header("host", nextjs_auction::PUBLISHER_HOST)
            .header("fly-client-ip", "192.0.2.2");
        if let Some(password) = password {
            request = request.basic_auth("admin", Some(password));
        }
        let response = request.send().expect("should reach auth middleware");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().contains_key("www-authenticate"));
        let body = response.text().expect("should read denied response");
        for absent in [
            "ingress",
            "socket_peer",
            "reader_ip",
            "192.0.2.2",
            "retained-proof",
        ] {
            assert!(
                !body.contains(absent),
                "should not expose protected data: {body}"
            );
        }
    }
    let direct = fixture.diagnostic(Some("192.0.2.2"));
    assert_eq!(direct["ingress"]["reader_ip"], "127.0.0.1");
    assert_eq!(direct["ingress"]["reader_source"], "direct_peer");
    assert_eq!(
        direct["geo"],
        json!({"available": true, "country": "US", "region": "CA"})
    );
    let response = fixture
        .client
        .get(fixture.url(PATH))
        .header("host", nextjs_auction::PUBLISHER_HOST)
        .basic_auth("admin", Some("admin-pass"))
        .send()
        .expect("should call diagnostic without raw Fly header");
    let result: Value = response.json().expect("should decode absent Fly header");
    assert_eq!(result["ingress"]["fly_client_ip"], Value::Null);
}

#[test]
fn diagnostic_reports_checked_chain_and_real_partial_or_unavailable_geo() {
    let fixture = Fixture::start(
        Some(&["127.0.0.1/32", "198.51.100.8/32"]),
        ReaderIpExpectation::ForwardedRequired,
    );
    let result = fixture.diagnostic(Some("192.0.2.2, 192.0.2.1, 198.51.100.8"));
    assert_eq!(result["ingress"]["reader_ip"], "192.0.2.1");
    assert_eq!(result["ingress"]["reader_source"], "trusted_x_forwarded");
    assert_eq!(
        result["geo"],
        json!({"available": true, "country": "US", "region": "CA"})
    );
    let result = fixture.diagnostic(Some("192.0.2.2"));
    assert_eq!(result["ingress"]["reader_ip"], "192.0.2.2");
    assert_eq!(
        result["geo"],
        json!({"available": true, "country": "GB", "region": null})
    );
    for xff in [None, Some("malformed"), Some("203.0.113.1")] {
        let result = fixture.diagnostic(xff);
        assert_eq!(
            result["geo"],
            json!({"available": false, "country": null, "region": null})
        );
    }
}

#[test]
fn diagnostic_does_not_trust_forwarding_from_an_untrusted_socket_peer() {
    let fixture = Fixture::start(
        Some(&["198.51.100.8/32"]),
        ReaderIpExpectation::ForwardedRequired,
    );
    let result = fixture.diagnostic(Some("192.0.2.1, 198.51.100.8"));
    assert_eq!(result["ingress"]["reader_ip"], "127.0.0.1");
    assert_eq!(result["ingress"]["reader_source"], "direct_peer");
    assert_eq!(
        result["geo"],
        json!({"available": false, "country": null, "region": null})
    );
}
