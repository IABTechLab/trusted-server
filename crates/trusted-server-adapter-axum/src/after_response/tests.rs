//! Controlled identity activation: production Axum has no record store until A2.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use edgezero_adapter_axum::dev_server::run_production_app;
use edgezero_core::app::Hooks;
use edgezero_core::body::Body;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Method, Response, StatusCode};
use edgezero_core::outbound::{HttpClient, OutboundRequest};
use edgezero_core::router::RouterService;
use error_stack::Report;
use tempfile::TempDir;
use trusted_server_core::ec::EcContext;
use trusted_server_core::ec::after_response::AfterResponseLookups;
use trusted_server_core::ec::app::{IdentityApp, IdentityPlatform};
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::kv_backend::{
    EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
};
use trusted_server_core::ec::kv_types::KvEntry;
use trusted_server_core::ec::page_load::NavigationPermit;
use trusted_server_core::ec::rate_limiter::NoRateLimit;
use trusted_server_core::ec::sharedid::SharedIdRequest;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::test_support::nextjs_auction;
use trusted_server_core::test_support::scripted_client::{
    ScriptedHttpClient, ScriptedReply, TestClock,
};

use super::schedule_lookups;

const EC_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";
const LOOKUP: &str = "https://sync.example.com/pull";

struct FixtureStore {
    entries: Mutex<BTreeMap<String, EcKvLookup>>,
    root: PathBuf,
    delay: Duration,
    armed: AtomicBool,
    task_started: AtomicBool,
    client: HttpClient,
    probe: String,
}

#[async_trait(?Send)]
impl EcKvStore for FixtureStore {
    fn store_name(&self) -> &str {
        "after-response-fixture"
    }

    async fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        // This real SDK request starts only in the after-response callback.
        // The provider response itself is scripted; this independently checks scope ownership.
        if self.armed.swap(false, Ordering::SeqCst) {
            self.task_started.store(true, Ordering::SeqCst);
            let response = self
                .client
                .send(
                    OutboundRequest::get(self.probe.as_str())
                        .expect("should build loopback scope probe"),
                )
                .await
                .expect("should send from the fresh after-response scope");
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "should receive SDK probe response"
            );
            fs::write(self.root.join("probe"), b"completed").expect("should record SDK probe");
        }
        Ok(self
            .entries
            .lock()
            .expect("should read fixture row")
            .get(key)
            .cloned())
    }

    async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        Ok(self
            .entries
            .lock()
            .expect("should inspect fixture row")
            .contains_key(key))
    }

    async fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        let follow_up = self.task_started.load(Ordering::SeqCst)
            && matches!(write.mode, EcKvWriteMode::IfGenerationMatch(_));
        if follow_up {
            fs::write(self.root.join("writing"), b"started")
                .expect("should record follow-up start");
            tokio::time::sleep(self.delay).await;
            while self.root.join("hold-write").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        let mut entries = self.entries.lock().expect("should lock fixture row");
        let generation = entries.get(key).map(|row| row.generation);
        if matches!(write.mode, EcKvWriteMode::Add) && generation.is_some()
            || matches!(write.mode, EcKvWriteMode::IfGenerationMatch(expected) if generation != Some(expected))
        {
            return Ok(EcKvWriteOutcome::PreconditionFailed);
        }
        entries.insert(
            key.to_owned(),
            EcKvLookup {
                body: write.body.as_bytes().to_vec(),
                metadata: Some(write.metadata.as_bytes().to_vec()),
                generation: generation.unwrap_or(0) + 1,
            },
        );
        if follow_up {
            let entry: KvEntry =
                serde_json::from_str(write.body).expect("should decode guarded update");
            assert_eq!(
                entry.ids["sync.example.com"].uid, "fixture-uid",
                "should persist the real pull-sync result"
            );
            fs::write(self.root.join("written"), b"fixture-uid")
                .expect("should record guarded update");
        }
        Ok(EcKvWriteOutcome::Written)
    }

    async fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        Ok(self
            .entries
            .lock()
            .expect("should list fixture rows")
            .keys()
            .filter(|key| key.starts_with(prefix))
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.entries
            .lock()
            .expect("should delete fixture row")
            .remove(key);
        Ok(())
    }
}

struct FixtureApp;

impl Hooks for FixtureApp {
    fn name() -> &'static str {
        "AfterResponseFixture"
    }

    fn routes() -> Result<RouterService, EdgeError> {
        Ok(RouterService::builder()
            .with_state(Arc::new(crate::startup::NativeStoreSnapshot::empty()))
            .route("/health", Method::GET, |_ctx: RequestContext| async {
                Ok::<_, EdgeError>(Response::new(Body::from("ok")))
            })
            .route("/held", Method::GET, |ctx: RequestContext| async move {
                let root = PathBuf::from(
                    std::env::var_os("TS_TEST_TASK_ROOT").expect("should select fixture root"),
                );
                let delay = std::env::var("TS_TEST_TASK_DELAY_MS")
                    .expect("should select delay")
                    .parse::<u64>()
                    .expect("should parse delay");
                let budget = std::env::var("TS_TEST_TASK_BUDGET_MS")
                    .expect("should select task budget")
                    .parse::<u64>()
                    .expect("should parse budget");
                let probe = std::env::var("TS_TEST_TASK_PROBE").expect("should select probe");
                let enabled = std::env::var("TS_TEST_TASK_ENABLED")
                    .expect("should select registration")
                    == "1";
                let handle = ctx
                    .parts()
                    .extensions
                    .get::<edgezero_core::after_response::AfterResponseHandle>()
                    .cloned();
                let services = crate::platform::build_runtime_services(&ctx)?;
                let store = Arc::new(FixtureStore {
                    entries: Mutex::new(BTreeMap::new()),
                    root: root.clone(),
                    delay: Duration::from_millis(delay),
                    armed: AtomicBool::new(false),
                    task_started: AtomicBool::new(false),
                    client: ctx.http_client().expect("should receive SDK client"),
                    probe,
                });
                let mut settings = nextjs_auction::settings();
                settings.ec.ec_store = Some("fixture".to_owned());
                settings.ec.identity_after_response_budget_ms = budget;
                settings.ec.partners = vec![serde_json::from_value(serde_json::json!({
                    "name": "sync", "source_domain": "sync.example.com", "bidstream_enabled": true,
                    "pull_sync_enabled": true, "pull_sync_url": LOOKUP,
                    "pull_sync_allowed_domains": ["sync.example.com"], "pull_sync_rate_limit": 100,
                    "ts_pull_token": "example-fixture-pull-token",
                })).expect("should build pull-sync partner")];
                let partners = Arc::new(
                    trusted_server_identity_providers::partner_registry(&settings.ec)
                        .expect("should build partners"),
                );
                let coordinator = Arc::new(
                    trusted_server_identity_providers::build_after_response_coordinator(
                        &settings,
                        Arc::clone(&partners),
                    )
                    .expect("should build pull-sync coordinator")
                    .expect("should have after-response work"),
                );
                let identity = IdentityApp::build(
                    Arc::new(settings),
                    IdentityPlatform {
                        record_store: Some(store.clone()),
                        rate_limiter: Some(Arc::new(NoRateLimit)),
                        providers: None,
                    },
                )
                .expect("should build fixture identity");
                let graph = identity
                    .identity_graph()
                    .expect("should have fixture graph");
                let mut entry = KvEntry::tombstone(trusted_server_core::ec::current_timestamp());
                entry.consent.ok = true;
                graph
                    .create(EC_ID, &entry)
                    .await
                    .expect("should seed live row");
                let req = ctx.into_request()?;
                let geo = GeoInfo {
                    city: "Example".to_owned(),
                    country: "JP".to_owned(),
                    continent: "Asia".to_owned(),
                    latitude: 0.0,
                    longitude: 0.0,
                    metro_code: 0,
                    region: None,
                    asn: None,
                };
                let mut ec = EcContext::read_from_request_with_geo(
                    identity.settings(),
                    &req,
                    &services,
                    Some(&geo),
                )
                .expect("should read returning consented identity");
                assert_eq!(
                    ec.ec_value(),
                    Some(EC_ID),
                    "should retain returning fixture identity"
                );
                ec.set_navigation_permit(
                    NavigationPermit::classify(&req).expect("should classify navigation"),
                );
                ec.set_kv_snapshot(graph.load_snapshot(EC_ID).await);
                let mut response = Response::new(Body::from("complete-response"));
                ec_finalize_response(
                    identity.settings(),
                    &mut ec,
                    Some(&graph),
                    identity.partners(),
                    None,
                    &SharedIdRequest::default(),
                    &mut response,
                )
                .await;
                let lookups =
                    AfterResponseLookups::capture(coordinator, EC_ID.to_owned(), None, &req);
                let clock = TestClock::new();
                let scripted = ScriptedHttpClient::new(&clock).with_request_observer({
                    let root = root.clone();
                    move |_request| {
                        fs::write(root.join("lookup"), b"dispatched")
                            .expect("should record pull sync dispatch");
                    }
                });
                scripted.respond_to(
                    Method::GET,
                    &format!("{LOOKUP}?ec_id={EC_ID}"),
                    ScriptedReply::status(StatusCode::OK)
                        .body(serde_json::json!({"uid":"fixture-uid"}).to_string()),
                );
                store.armed.store(true, Ordering::SeqCst);
                if enabled {
                    assert!(
                        schedule_lookups(
                            handle.as_ref(),
                            Some(lookups),
                            ec,
                            identity,
                            scripted.http_client()
                        )?,
                        "should register eligible work"
                    );
                }
                fs::write(root.join("registered"), b"request-held")
                    .expect("should expose request stage");
                while !root.join("release").exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                assert!(
                    !root.join("lookup").exists(),
                    "should not dispatch pull sync before response release"
                );
                assert!(
                    !root.join("written").exists(),
                    "should not write before response release"
                );
                Ok::<_, EdgeError>(response)
            })
            .build())
    }
}

#[test]
#[ignore = "subprocess fixture, invoked only by parent acceptance tests"]
fn runtime_child() {
    if std::env::var_os("TS_TEST_TASK_ROOT").is_none() {
        return;
    }
    let result = run_production_app::<FixtureApp>();
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}

struct Probe {
    stopping: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("should join owned probe");
        }
    }
}

struct Running {
    child: Child,
    root: TempDir,
    address: SocketAddr,
    _probe: Probe,
}

impl Drop for Running {
    fn drop(&mut self) {
        if self
            .child
            .try_wait()
            .expect("should inspect owned child")
            .is_none()
        {
            self.child.kill().expect("should stop owned child");
        }
        self.child.wait().expect("should reap owned child");
    }
}

impl Running {
    fn start(delay: u64, budget: u64, enabled: bool, grace: u64) -> Self {
        let root = tempfile::tempdir().expect("should create runtime root");
        let listener = TcpListener::bind("127.0.0.1:0").expect("should reserve app port");
        let address = listener.local_addr().expect("should read port");
        drop(listener);
        let upstream = TcpListener::bind("127.0.0.1:0").expect("should bind SDK probe");
        let probe_address = upstream.local_addr().expect("should read probe port");
        upstream
            .set_nonblocking(true)
            .expect("should set probe nonblocking");
        let stop_probe = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop_probe);
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                if let Ok((mut socket, _)) = upstream.accept() {
                    socket
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .expect("should bound probe read");
                    let mut request = [0; 4096];
                    let _ = socket.read(&mut request);
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .expect("should answer SDK probe");
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        let probe = Probe {
            stopping: stop_probe,
            worker: Some(worker),
        };
        let output = File::create(root.path().join("output")).expect("should capture child output");
        let child = Command::new(std::env::current_exe().expect("should locate test executable"))
            .args([
                "--exact",
                "after_response::tests::runtime_child",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").expect("should retain tool PATH"),
            )
            .env("TS_TEST_TASK_ROOT", root.path())
            .env("TS_TEST_TASK_DELAY_MS", delay.to_string())
            .env("TS_TEST_TASK_BUDGET_MS", budget.to_string())
            .env("TS_TEST_TASK_ENABLED", if enabled { "1" } else { "0" })
            .env(
                "TS_TEST_TASK_PROBE",
                format!("http://{probe_address}/scope"),
            )
            .env("EDGEZERO__ADAPTER__HOST", "127.0.0.1")
            .env("EDGEZERO__ADAPTER__PORT", address.port().to_string())
            .env(
                "EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS",
                grace.to_string(),
            )
            .stdout(output.try_clone().expect("should copy output handle"))
            .stderr(output)
            .spawn()
            .expect("should launch runtime child");
        let mut app = Self {
            child,
            root,
            address,
            _probe: probe,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(mut stream) =
                TcpStream::connect_timeout(&app.address, Duration::from_millis(50))
            {
                stream.write_all(b"GET /health HTTP/1.1\r\nHost: publisher.example.com\r\nConnection: close\r\n\r\n").expect("should send readiness probe");
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .expect("should bound readiness");
                let mut response = String::new();
                stream
                    .read_to_string(&mut response)
                    .expect("should receive readiness");
                if response.contains("200 OK") {
                    break;
                }
            }
            assert!(
                app.child.try_wait().expect("should poll startup").is_none(),
                "should remain alive: {}",
                app.log()
            );
            assert!(
                Instant::now() < deadline,
                "should become ready: {}",
                app.log()
            );
            thread::sleep(Duration::from_millis(5));
        }
        app
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.path().join("output")).expect("should read child output")
    }
    fn marker(&self, name: &str) -> bool {
        self.root.path().join(name).exists()
    }
    fn wait_marker(&mut self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.marker(name) {
            assert!(
                self.child
                    .try_wait()
                    .expect("should inspect marker progress")
                    .is_none(),
                "should reach {name}: {}",
                self.log()
            );
            assert!(
                Instant::now() < deadline,
                "should reach {name}: {}",
                self.log()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn request(&self) -> TcpStream {
        let mut stream = TcpStream::connect(self.address).expect("should connect held request");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("should bound held request");
        write!(stream, "GET /held HTTP/1.1\r\nHost: publisher.example.com\r\nCookie: ts-ec={EC_ID}\r\nSec-Fetch-Dest: document\r\nSec-Fetch-Mode: navigate\r\nConnection: close\r\n\r\n").expect("should send held navigation");
        stream
    }
    fn terminate(&mut self) -> Instant {
        let start = Instant::now();
        assert!(
            Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status()
                .expect("should signal owned child")
                .success(),
            "should send SIGTERM"
        );
        start
    }
    fn release(&self) {
        fs::write(self.root.path().join("release"), b"release").expect("should release response");
    }
    fn exited(&mut self, start: Instant, grace: Duration) -> bool {
        loop {
            if let Some(status) = self.child.try_wait().expect("should observe exit") {
                let elapsed = start.elapsed();
                let limit = if status.success() {
                    grace
                } else {
                    // Forced expiry is observed after the host's deadline fires.
                    grace + Duration::from_millis(300)
                };
                assert!(
                    elapsed < limit,
                    "should exit within the applicable grace bound: {elapsed:?} < {limit:?}"
                );
                return status.success();
            }
            assert!(
                start.elapsed() < grace + Duration::from_millis(300),
                "should exit within grace: {}",
                self.log()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn complete_response(stream: &mut TcpStream) {
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("should receive complete response");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "should preserve successful response: {response}"
    );
    assert!(
        response.ends_with("complete-response"),
        "should deliver complete body: {response}"
    );
}

#[test]
fn sigterm_drains_the_registered_lookup_and_guarded_write_after_response() {
    let mut app = Running::start(250, 2000, true, 2);
    fs::write(app.root.path().join("hold-write"), b"hold")
        .expect("should gate follow-up completion");
    let mut stream = app.request();
    app.wait_marker("registered");
    let start = app.terminate();
    thread::sleep(Duration::from_millis(40));
    assert!(!app.marker("lookup"), "should wait for held response");
    assert!(
        app.child
            .try_wait()
            .expect("should inspect held process")
            .is_none(),
        "should retain request before release"
    );
    app.release();
    complete_response(&mut stream);
    assert!(
        !app.marker("written"),
        "should deliver response before delayed write completes"
    );
    fs::remove_file(app.root.path().join("hold-write")).expect("should allow follow-up completion");
    assert!(
        app.exited(start, Duration::from_secs(2)),
        "should exit successfully: {}",
        app.log()
    );
    assert!(
        app.marker("probe"),
        "should complete real SDK call in fresh task scope"
    );
    assert!(
        app.marker("lookup"),
        "should reach the real pull-sync coordinator"
    );
    assert!(
        app.marker("written"),
        "should complete guarded fixture write before exit"
    );
}

#[test]
fn disabled_registration_control_delivers_response_without_the_follow_up() {
    let mut app = Running::start(250, 2000, false, 2);
    let mut stream = app.request();
    app.wait_marker("registered");
    let start = app.terminate();
    app.release();
    complete_response(&mut stream);
    assert!(
        app.exited(start, Duration::from_secs(2)),
        "should gracefully drain response"
    );
    assert!(
        !app.marker("lookup") && !app.marker("written"),
        "should distinguish missing registration from successful delivery"
    );
}

#[test]
fn request_and_task_share_the_shutdown_grace() {
    let mut app = Running::start(850, 5000, true, 1);
    let mut stream = app.request();
    app.wait_marker("registered");
    let start = app.terminate();
    thread::sleep(Duration::from_millis(450));
    app.release();
    complete_response(&mut stream);
    assert!(
        !app.exited(start, Duration::from_secs(1)),
        "should exhaust shared grace"
    );
    assert!(
        !app.marker("written"),
        "should cancel task rather than grant a second grace"
    );
    assert!(
        app.marker("writing"),
        "should actually enter the delayed write before cancellation"
    );
}

#[test]
fn task_budget_cancels_work_independently_of_shutdown_grace() {
    let mut app = Running::start(700, 100, true, 2);
    let mut stream = app.request();
    app.wait_marker("registered");
    app.release();
    complete_response(&mut stream);
    app.wait_marker("writing");
    thread::sleep(Duration::from_millis(180));
    let start = app.terminate();
    assert!(
        app.exited(start, Duration::from_secs(2)),
        "should have no task left to drain"
    );
    assert!(
        !app.marker("written"),
        "should cancel at the independent task budget"
    );
}

#[test]
fn client_disconnect_still_releases_registered_work() {
    let mut app = Running::start(50, 2000, true, 2);
    let stream = app.request();
    app.wait_marker("registered");
    drop(stream);
    app.release();
    app.wait_marker("written");
    let start = app.terminate();
    assert!(
        app.exited(start, Duration::from_secs(2)),
        "should drain disconnected request's work"
    );
}

#[test]
fn no_work_needs_no_scheduler_and_registers_nothing() {
    let identity = IdentityApp::build(
        Arc::new(nextjs_auction::settings()),
        IdentityPlatform::default(),
    )
    .expect("should build fixture identity");
    assert!(
        !schedule_lookups(
            None,
            None,
            EcContext::default(),
            identity,
            HttpClient::with_client(ScriptedHttpClient::forbidding())
        )
        .expect("should skip absent work"),
        "should not register a no-op task"
    );
}

struct RegistrationCase {
    root: TempDir,
    identity: Arc<IdentityApp>,
    context: EcContext,
    work: AfterResponseLookups,
    client: ScriptedHttpClient,
}

async fn registration_case(permit: bool) -> RegistrationCase {
    let root = tempfile::tempdir().expect("should create registration fixture");
    let clock = TestClock::new();
    let client = ScriptedHttpClient::forbidding();
    let store = Arc::new(FixtureStore {
        entries: Mutex::new(BTreeMap::new()),
        root: root.path().to_path_buf(),
        delay: Duration::ZERO,
        armed: AtomicBool::new(false),
        task_started: AtomicBool::new(false),
        client: client.http_client(),
        probe: "http://127.0.0.1:1/unreachable".to_owned(),
    });
    let mut settings = nextjs_auction::settings();
    settings.ec.ec_store = Some("fixture".to_owned());
    settings.ec.partners = vec![
        serde_json::from_value(serde_json::json!({
            "name": "sync", "source_domain": "sync.example.com", "bidstream_enabled": true,
            "pull_sync_enabled": true, "pull_sync_url": LOOKUP,
            "pull_sync_allowed_domains": ["sync.example.com"], "pull_sync_rate_limit": 100,
            "ts_pull_token": "example-fixture-pull-token",
        }))
        .expect("should build fixture partner"),
    ];
    let partners = Arc::new(
        trusted_server_identity_providers::partner_registry(&settings.ec)
            .expect("should build partners"),
    );
    let coordinator = Arc::new(
        trusted_server_identity_providers::build_after_response_coordinator(
            &settings,
            Arc::clone(&partners),
        )
        .expect("should build coordinator")
        .expect("should have pull-sync work"),
    );
    let identity = IdentityApp::build(
        Arc::new(settings),
        IdentityPlatform {
            record_store: Some(store.clone()),
            rate_limiter: Some(Arc::new(NoRateLimit)),
            providers: None,
        },
    )
    .expect("should build fixture identity");
    let graph = identity
        .identity_graph()
        .expect("should have fixture graph");
    let mut entry = KvEntry::tombstone(trusted_server_core::ec::current_timestamp());
    entry.consent.ok = true;
    graph
        .create(EC_ID, &entry)
        .await
        .expect("should seed live row");
    let request = edgezero_core::http::request_builder()
        .uri("https://publisher.example.com/article")
        .header("cookie", format!("ts-ec={EC_ID}"))
        .header("sec-fetch-dest", "document")
        .header("sec-fetch-mode", "navigate")
        .body(Body::empty())
        .expect("should build fixture navigation");
    let geo = GeoInfo {
        city: "Example".to_owned(),
        country: "JP".to_owned(),
        continent: "Asia".to_owned(),
        latitude: 0.0,
        longitude: 0.0,
        metro_code: 0,
        region: None,
        asn: None,
    };
    let services = nextjs_auction::services_with_outbound_client(&client, &clock);
    let mut context =
        EcContext::read_from_request_with_geo(identity.settings(), &request, &services, Some(&geo))
            .expect("should read fixture context");
    context.set_kv_snapshot(graph.load_snapshot(EC_ID).await);
    if permit {
        context.set_navigation_permit(
            NavigationPermit::classify(&request).expect("should classify navigation"),
        );
        ec_finalize_response(
            identity.settings(),
            &mut context,
            Some(&graph),
            identity.partners(),
            None,
            &SharedIdRequest::default(),
            &mut Response::new(Body::empty()),
        )
        .await;
    }
    let work = AfterResponseLookups::capture(coordinator, EC_ID.to_owned(), None, &request);
    store.armed.store(true, Ordering::SeqCst);
    RegistrationCase {
        root,
        identity,
        context,
        work,
        client,
    }
}

struct RejectingScheduler(Arc<std::sync::atomic::AtomicUsize>);

impl edgezero_core::after_response::AfterResponseScheduler for RejectingScheduler {
    fn schedule(
        &self,
        _budget: Duration,
        _task: futures::future::LocalBoxFuture<'static, ()>,
    ) -> Result<(), EdgeError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(EdgeError::validation("fixture registration refused"))
    }
}

#[tokio::test]
async fn work_without_a_permit_registers_nothing() {
    let fixture = registration_case(false).await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handle = edgezero_core::after_response::AfterResponseHandle::new(RejectingScheduler(
        Arc::clone(&calls),
    ));
    assert!(
        !schedule_lookups(
            Some(&handle),
            Some(fixture.work),
            fixture.context,
            fixture.identity,
            fixture.client.http_client()
        )
        .expect("should skip work without a permit"),
        "should not register unauthorized work"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "should not call scheduler");
    fixture.client.assert_no_requests();
}

#[tokio::test]
async fn eligible_work_without_a_scheduler_reports_an_error_without_running() {
    let fixture = registration_case(true).await;
    assert!(
        schedule_lookups(
            None,
            Some(fixture.work),
            fixture.context,
            fixture.identity,
            fixture.client.http_client()
        )
        .is_err(),
        "should report unavailable scheduler for eligible work"
    );
    fixture.client.assert_no_requests();
}

#[tokio::test]
async fn cloning_a_context_does_not_duplicate_a_follow_up_permit() {
    let fixture = registration_case(true).await;
    let cloned = fixture.context.clone();
    assert!(
        !schedule_lookups(
            None,
            Some(fixture.work),
            cloned,
            Arc::clone(&fixture.identity),
            fixture.client.http_client()
        )
        .expect("should skip cloned context"),
        "should never authorize the cloned context"
    );
    let mut original = fixture.context;
    assert!(
        original.take_after_response_permit().is_some(),
        "should retain original single-use permit"
    );
    assert!(
        original.take_after_response_permit().is_none(),
        "should consume permit once"
    );
    fixture.client.assert_no_requests();
}

#[tokio::test]
async fn rejected_registration_drops_work_without_executing_it() {
    let fixture = registration_case(true).await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handle = edgezero_core::after_response::AfterResponseHandle::new(RejectingScheduler(
        Arc::clone(&calls),
    ));
    assert!(
        schedule_lookups(
            Some(&handle),
            Some(fixture.work),
            fixture.context,
            fixture.identity,
            fixture.client.http_client()
        )
        .is_err(),
        "should propagate rejected registration"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "should attempt registration only once"
    );
    fixture.client.assert_no_requests();
    assert!(
        !fixture.root.path().join("probe").exists(),
        "should not poll rejected work"
    );
}
