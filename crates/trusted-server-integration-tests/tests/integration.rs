mod common;
mod environments;
mod frameworks;

use base64::Engine as _;
use common::ec::CookieVaryingOrigin;
use common::runtime::{
    RuntimeEnvironment, RuntimeProcess, TestError, origin_port, wasm_binary_path,
};
use common::trace_boundary::{TraceRuntime, with_trace_runtime};
use environments::{RUNTIME_ENVIRONMENTS, ReadyCheckOptions, wait_for_http_ready};
use error_stack::ResultExt as _;
use frameworks::scenarios::EcScenario;
use frameworks::{FRAMEWORKS, FrontendFramework};
use reqwest::header::CONTENT_TYPE;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use testcontainers::runners::SyncRunner as _;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};
use trusted_server_core::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};

/// Initialize the logger once for the test binary.
///
/// Called at the start of each test function. `try_init` is used so that
/// repeated calls (one per test) are harmless — only the first succeeds.
fn init_logger() {
    let _ = env_logger::try_init();
}

#[test]
fn default_fastly_viceroy_template_has_generated_config_store_marker() {
    let template = include_str!("../fixtures/configs/viceroy-template.toml");
    const MARKER: &str = "# GENERATED_TRUSTED_SERVER_CONFIG_STORES";

    assert_eq!(
        template.matches(MARKER).count(),
        1,
        "default Fastly Viceroy template should contain exactly one generated config-store marker"
    );
}

/// Test all combinations: frameworks x runtimes (matrix testing).
///
/// Iterates every registered runtime and framework, running all standard
/// and custom scenarios for each combination. Uses `--test-threads=1`
/// because all containers share the same fixed origin port.
#[test]
#[ignore = "requires Docker, Viceroy, and pre-built WASM binary"]
fn test_all_combinations() {
    init_logger();
    for runtime_factory in RUNTIME_ENVIRONMENTS {
        let runtime = runtime_factory();
        log::info!("Testing runtime: {}", runtime.id());

        for framework_factory in FRAMEWORKS {
            let framework = framework_factory();
            log::info!("  Testing framework: {}", framework.id());

            test_combination(runtime.as_ref(), framework.as_ref())
                .expect("should pass all scenarios for this combination");
        }
    }
}

/// Test a specific framework x runtime combination.
///
/// # Steps
///
/// 1. Start the frontend container mapped to the fixed origin port
/// 2. Spawn the runtime process (e.g. Viceroy) with the pre-built WASM binary
/// 3. Run standard scenarios (HTML injection, script serving, etc.)
/// 4. Run framework-specific custom scenarios
/// 5. Cleanup is automatic via [`Drop`] on both container and runtime
///
/// # Errors
///
/// Propagates any [`TestError`] from container startup, runtime spawn,
/// or scenario assertions, with full context chain.
fn test_combination(
    runtime: &dyn common::runtime::RuntimeEnvironment,
    framework: &dyn FrontendFramework,
) -> common::runtime::TestResult<()> {
    init_logger();
    let runtime_id = runtime.id();
    let framework_id = framework.id();
    let port = origin_port();

    // 1. Start frontend container mapped to the fixed origin port
    let container = framework
        .build_container(port)
        .attach(format!("runtime: {runtime_id}, framework: {framework_id}"))?
        .start()
        .change_context(TestError::ContainerStart {
            reason: format!("framework: {framework_id}"),
        })
        .attach(format!("runtime: {runtime_id}, framework: {framework_id}"))?;

    let origin_url = format!("http://127.0.0.1:{port}");

    // Wait for container to be ready
    wait_for_http_ready(
        &origin_url,
        framework.health_check_path(),
        ReadyCheckOptions {
            max_attempts: 60,
            interval: Duration::from_millis(500),
            fallback_to_root: false,
            timeout_error: TestError::ContainerTimeout,
            timeout_message: format!("Container at {origin_url} not ready after 30s"),
        },
    )?;

    // 2. Spawn runtime process with the pre-built WASM binary
    let wasm_path = wasm_binary_path();
    let process = runtime
        .spawn(&wasm_path)
        .attach(format!("runtime: {runtime_id}, framework: {framework_id}"))?;

    // 3. Run standard scenarios
    for scenario in framework.standard_scenarios() {
        scenario
            .run(&process.base_url, framework_id)
            .attach(format!(
                "runtime: {runtime_id}, framework: {framework_id}, scenario: {scenario:?}"
            ))?;
    }

    // 4. Run custom scenarios
    for scenario in framework.custom_scenarios() {
        scenario
            .run(&process.base_url, framework_id)
            .attach(format!(
                "runtime: {runtime_id}, framework: {framework_id}, custom scenario: {scenario:?}"
            ))?;
    }

    // Explicitly drop container to free the origin port for the next test
    drop(container);

    Ok(())
}
// Individual test functions for faster iteration during development.

#[test]
#[ignore = "requires Docker, Viceroy, and pre-built WASM binary"]
fn test_wordpress_fastly() {
    let runtime = environments::fastly::FastlyViceroy;
    let framework = frameworks::wordpress::WordPress;
    test_combination(&runtime, &framework).expect("should pass WordPress on Fastly");
}

#[test]
#[ignore = "requires Docker, Viceroy, and pre-built WASM binary"]
fn test_nextjs_fastly() {
    let runtime = environments::fastly::FastlyViceroy;
    let framework = frameworks::nextjs::NextJs;
    test_combination(&runtime, &framework).expect("should pass Next.js on Fastly");
}

#[test]
#[ignore = "requires Docker, the `wrangler` CLI in $PATH, and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts `wrangler dev` automatically"]
fn test_wordpress_cloudflare() {
    let runtime = environments::cloudflare::CloudflareWorkers;
    let framework = frameworks::wordpress::WordPress;
    test_combination(&runtime, &framework).expect("should pass WordPress on Cloudflare Workers");
}

#[test]
#[ignore = "requires Docker, the `wrangler` CLI in $PATH, and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts `wrangler dev` automatically"]
fn test_nextjs_cloudflare() {
    let runtime = environments::cloudflare::CloudflareWorkers;
    let framework = frameworks::nextjs::NextJs;
    test_combination(&runtime, &framework).expect("should pass Next.js on Cloudflare Workers");
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn test_cloudflare_dynamic_publisher_response_does_not_cross_cookie_boundaries() {
    init_logger();
    let _origin = CookieVaryingOrigin::start(origin_port());
    let runtime = environments::cloudflare::CloudflareWorkers;
    let process = runtime
        .spawn(&wasm_binary_path())
        .expect("should start Cloudflare Worker");
    let client = reqwest::blocking::Client::new();

    let first_response = client
        .get(format!("{}/cache-regression", process.base_url))
        .header("cookie", "viewer=first")
        .send()
        .expect("should request first dynamic publisher response");
    assert_eq!(
        first_response.status().as_u16(),
        200,
        "first dynamic publisher response should succeed"
    );
    let first_body = first_response
        .text()
        .expect("should read first dynamic publisher response");

    let second_response = client
        .get(format!("{}/cache-regression", process.base_url))
        .header("cookie", "viewer=second")
        .send()
        .expect("should request second dynamic publisher response");
    assert_eq!(
        second_response.status().as_u16(),
        200,
        "second dynamic publisher response should succeed"
    );
    let second_body = second_response
        .text()
        .expect("should read second dynamic publisher response");

    assert!(
        first_body.contains("viewer=first"),
        "first response must preserve its cookie-specific origin body: {first_body}"
    );
    assert!(
        second_body.contains("viewer=second"),
        "second response must not reuse the first visitor's body: {second_body}"
    );
}

/// Exercises an enabled `/auction` request through auction telemetry in workerd.
///
/// `std::time::Instant::now()` traps on Cloudflare's `wasm32-unknown-unknown`
/// target, so this guards the `web_time::Instant` timing in
/// `AuctionObservationContext`. The auction config has no providers, so the
/// enabled auction completes as a no-bid without outbound calls.
#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn test_cloudflare_enabled_auction_reaches_telemetry() {
    init_logger();
    // The readiness probe falls back to the proxied root page.
    let _origin = common::ec::MinimalOrigin::start(origin_port());
    let config_json = common::config::cloudflare_auction_config_json(origin_port())
        .expect("should build auction-enabled Cloudflare config");
    let process = environments::cloudflare::CloudflareWorkers
        .spawn_with_config_json(&config_json)
        .expect("should start Cloudflare Worker");
    let request_body = serde_json::json!({
        "adUnits": [{
            "code": "integration-slot",
            "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
        }]
    });

    // A non-regulated geo passes the server-side auction consent gate, so the
    // request runs the enabled orchestration path instead of a consent skip.
    let response = reqwest::blocking::Client::new()
        .post(format!("{}/auction", process.base_url))
        .header("cf-ipcountry", "US")
        .json(&request_body)
        .send()
        .expect("should send auction request");

    let status = response.status().as_u16();
    let body = response.text().expect("should read auction response");
    assert_eq!(
        status, 200,
        "enabled auction should complete without a worker exception: {body}"
    );
    let auction_response: serde_json::Value =
        serde_json::from_str(&body).expect("should return an OpenRTB JSON response");
    assert!(
        auction_response["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "auction response should carry the auction id: {body}"
    );
    assert_eq!(
        auction_response["ext"]["orchestrator"]["providers"],
        serde_json::json!(0),
        "enabled auction without providers should complete as a no-bid: {body}"
    );
}

#[test]
#[ignore = "requires Docker and pre-built trusted-server-axum binary"]
fn test_wordpress_axum() {
    let runtime = environments::axum::AxumDevServer;
    let framework = frameworks::wordpress::WordPress;
    test_combination(&runtime, &framework).expect("should pass WordPress on Axum");
}

#[test]
#[ignore = "requires Docker and pre-built trusted-server-axum binary"]
fn test_nextjs_axum() {
    let runtime = environments::axum::AxumDevServer;
    let framework = frameworks::nextjs::NextJs;
    test_combination(&runtime, &framework).expect("should pass Next.js on Axum");
}

// ---------------------------------------------------------------------------
// EC identity lifecycle tests (no frontend framework container needed)
// ---------------------------------------------------------------------------

/// Runs all EC lifecycle scenarios against a standalone Viceroy instance.
///
/// Unlike framework tests, these use a minimal TCP origin server instead
/// of a Docker container. The scenarios use a pre-seeded EC row plus
/// explicit consent cookies because Viceroy's local HTTP runtime does not
/// satisfy the production browser bot-gate fingerprint requirements for
/// minting new ECs end-to-end.
#[test]
#[ignore = "requires Viceroy and pre-built WASM binary"]
fn test_ec_lifecycle_fastly() {
    init_logger();
    let port = origin_port();

    // Start a minimal origin server so organic route proxying succeeds.
    let _origin = common::ec::MinimalOrigin::start(port);
    log::info!("EC lifecycle tests: minimal origin running on port {port}");

    let runtime = environments::fastly::FastlyViceroy;
    let wasm_path = wasm_binary_path();

    let process = runtime
        .spawn(&wasm_path)
        .expect("should spawn Viceroy for EC tests");

    log::info!(
        "EC lifecycle tests: Viceroy running at {}",
        process.base_url
    );

    // Verify the post-cutover router is active before route-level scenarios.
    common::ec::assert_edgezero_entry_point(&process.base_url)
        .expect("should answer TRACE with a router-level 405 carrying the version headers");

    for scenario in EcScenario::all() {
        log::info!("  Running EC scenario: {scenario:?}");
        let result = scenario.run(&process.base_url);
        assert!(
            result.is_ok(),
            "EC scenario {scenario:?} should succeed: {result:?}"
        );
    }
}
#[test]
#[ignore = "requires Wrangler in PATH and the current Cloudflare build.sh output"]
fn trace_runtime_boundary_cloudflare() {
    init_logger();
    common::trace_boundary::exercise(common::trace_boundary::TraceRuntime::Cloudflare);
}

#[test]
#[ignore = "requires Viceroy and the current production Fastly WASM artifact"]
fn trace_runtime_boundary_fastly() {
    init_logger();
    common::trace_boundary::exercise(common::trace_boundary::TraceRuntime::Fastly);
}

#[test]
#[ignore = "requires Spin and the current production Spin WASM artifact"]
fn trace_runtime_boundary_spin() {
    init_logger();
    common::trace_boundary::exercise(common::trace_boundary::TraceRuntime::Spin);
}

#[test]
#[ignore = "requires Docker, Chromium and the current native Axum binary"]
fn trace_browser_workflow_axum() {
    init_logger();
    common::trace_boundary::exercise_browser(common::trace_boundary::TraceBrowserRuntime::Axum);
}

#[test]
#[ignore = "requires Docker, Chromium, Viceroy and the current release Fastly WASM"]
fn trace_browser_workflow_fastly() {
    init_logger();
    common::trace_boundary::exercise_browser(common::trace_boundary::TraceBrowserRuntime::Fastly);
}

#[test]
#[ignore = "requires Docker, Chromium, Wrangler and the current Cloudflare build output"]
fn trace_browser_workflow_cloudflare() {
    init_logger();
    common::trace_boundary::exercise_browser(
        common::trace_boundary::TraceBrowserRuntime::Cloudflare,
    );
}

#[test]
#[ignore = "requires Docker, Chromium, Spin and the current release Spin component"]
fn trace_browser_workflow_spin() {
    init_logger();
    common::trace_boundary::exercise_browser(common::trace_boundary::TraceBrowserRuntime::Spin);
}

// ---------------------------------------------------------------------------
// Runtime ingress baseline for the EdgeZero cut
// ---------------------------------------------------------------------------
//
// These cases record how each runtime hands request bodies and methods to
// Trusted Server today, so later changes can show exactly what moved. They
// all bind the fixed origin port, so each one holds `ORIGIN_PORT_LOCK` while
// it runs. The lock serializes only these cases: other tests in this binary
// bind the same port without it, so the binary still needs
// `--test-threads=1`.

/// Largest `/auction` body the handler accepts.
///
/// Mirrors `MAX_AUCTION_BODY_SIZE` in `trusted-server-core`.
const AUCTION_BODY_CAP: usize = 256 * 1024;

/// Status the recording origin returns when a case compares statuses.
///
/// Neither Trusted Server nor `EdgeZero` synthesizes 203, so a 203 at the
/// client can only have come from the origin.
const ORIGIN_PASS_THROUGH_STATUS: u16 = 203;

/// Publisher path the byte-for-byte POST case sends its body to.
const INGRESS_FORM_PATH: &str = "/ingress-form";

/// Serializes the ingress cases that bind the fixed origin port.
///
/// Only those cases take it. The rest of the binary still relies on
/// `--test-threads=1` to keep the port free.
static ORIGIN_PORT_LOCK: Mutex<()> = Mutex::new(());

/// Takes the origin-port lock for the rest of the calling case.
fn lock_origin_port() -> MutexGuard<'static, ()> {
    // A failed case poisons the lock but frees the port, so later cases can
    // still run.
    ORIGIN_PORT_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// A runtime the ingress cases start against the fixed origin port.
#[derive(Clone, Copy, Debug)]
enum IngressRuntime {
    Axum,
    Cloudflare,
    Fastly,
}

impl IngressRuntime {
    /// Starts the runtime with the shared integration app config.
    ///
    /// # Panics
    ///
    /// Panics when the runtime's artifact or CLI is missing or it never
    /// becomes ready.
    fn spawn(self) -> RuntimeProcess {
        match self {
            Self::Axum => environments::axum::AxumDevServer
                .spawn(&wasm_binary_path())
                .expect("should start the Axum dev server"),
            Self::Cloudflare => {
                let config_json = common::config::cloudflare_config_json(origin_port())
                    .expect("should build the Cloudflare config binding");
                environments::cloudflare::CloudflareWorkers
                    .spawn_with_config_json(&config_json)
                    .expect("should start the Cloudflare Worker")
            }
            Self::Fastly => environments::fastly::FastlyViceroy
                .spawn(&wasm_binary_path())
                .expect("should start Viceroy"),
        }
    }
}

/// One request as the recording origin received it.
#[derive(Clone, Debug)]
struct OriginRequest {
    method: String,
    path: String,
    body: Vec<u8>,
    /// The `Content-Length` value the request was framed with, if any.
    content_length: Option<String>,
    /// The `Transfer-Encoding` value the request was framed with, if any.
    transfer_encoding: Option<String>,
    /// Every header field, names lowercased, in arrival order.
    headers: Vec<(String, String)>,
    /// When the request head had arrived.
    received_at: Instant,
    /// When the origin began the last write of its answer, once it has.
    answered_at: Option<Instant>,
}

impl OriginRequest {
    /// Returns the value of the first `name` field, compared case-insensitively.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Builds the complete HTTP response a [`RecordingOrigin`] sends for one request.
type OriginResponder = Arc<dyn Fn(&OriginRequest) -> Vec<u8> + Send + Sync>;

/// Builds a [`RecordingOrigin`]'s answer to one request as writes spaced in time.
type PacedResponder = Arc<dyn Fn(&OriginRequest) -> Vec<OriginWrite> + Send + Sync>;

/// One write of a paced answer: `bytes` go out `delay` after the previous write.
struct OriginWrite {
    delay: Duration,
    bytes: Vec<u8>,
}

/// Loopback origin that records every request and answers it from a responder.
///
/// Unlike [`common::ec::MinimalOrigin`], it reads the whole request body and
/// decodes chunked framing, so a case can compare what reached the origin
/// with what the client sent.
struct RecordingOrigin {
    port: u16,
    requests: Arc<Mutex<Vec<OriginRequest>>>,
    stopping: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RecordingOrigin {
    /// Starts the origin on `127.0.0.1:{port}`, answering every request with
    /// `status` and a small HTML page.
    ///
    /// # Panics
    ///
    /// Panics if the port is already in use.
    fn start(port: u16, status: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("should bind origin port");
        Self::serve(
            listener,
            Arc::new(move |request| html_response(status, request.method == "HEAD")),
        )
    }

    /// Serves `listener`, answering each request with what `respond` builds.
    ///
    /// # Panics
    ///
    /// Panics if the listener cannot be configured.
    fn serve(listener: TcpListener, respond: OriginResponder) -> Self {
        Self::serve_paced(
            listener,
            Arc::new(move |request| {
                vec![OriginWrite {
                    delay: Duration::ZERO,
                    bytes: respond(request),
                }]
            }),
        )
    }

    /// Serves `listener`, answering each request with the writes `respond`
    /// builds, each after its delay.
    ///
    /// # Panics
    ///
    /// Panics if the listener cannot be configured.
    fn serve_paced(listener: TcpListener, respond: PacedResponder) -> Self {
        let port = listener
            .local_addr()
            .expect("should read the origin address")
            .port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);

        // The accept blocks rather than polls, so a recorded arrival time is
        // not late by a polling interval. Drop wakes it with a connection.
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else {
                    break;
                };
                let recorded = Arc::clone(&recorded);
                let respond = Arc::clone(&respond);
                // One thread per connection, so an idle pooled
                // connection cannot hold up the next request.
                thread::spawn(move || {
                    serve_origin_connection(stream, respond.as_ref(), &recorded);
                });
            }
        });

        Self {
            port,
            requests,
            stopping,
            handle: Some(handle),
        }
    }

    /// Returns every request recorded so far, in arrival order.
    fn requests(&self) -> Vec<OriginRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for RecordingOrigin {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // A connection wakes the blocking accept, which then sees the flag.
        // Without one the thread would never end, so it is not joined.
        let woken = TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], self.port)),
            Duration::from_secs(1),
        )
        .is_ok();
        if let Some(handle) = self.handle.take().filter(|_| woken) {
            let _ = handle.join();
        }
    }
}

/// Reads one request from `stream`, records it, and answers with the writes
/// `respond` builds.
fn serve_origin_connection(
    mut stream: TcpStream,
    respond: &(dyn Fn(&OriginRequest) -> Vec<OriginWrite> + Send + Sync),
    recorded: &Mutex<Vec<OriginRequest>>,
) {
    // Some platforms hand out accepted sockets in the listener's nonblocking
    // mode; the request read below must block, with a bound.
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .is_err()
    {
        return;
    }
    let Some(mut request) = read_origin_request(&mut stream) else {
        return;
    };
    let mut writes = respond(&request);
    let last = writes.pop().unwrap_or(OriginWrite {
        delay: Duration::ZERO,
        bytes: Vec::new(),
    });
    for write in writes {
        thread::sleep(write.delay);
        if stream
            .write_all(&write.bytes)
            .and_then(|()| stream.flush())
            .is_err()
        {
            return;
        }
    }
    thread::sleep(last.delay);
    // Recorded before the last write, so a client that has the whole answer
    // also finds the request recorded.
    request.answered_at = Some(Instant::now());
    recorded
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(request);

    let _ = stream.write_all(&last.bytes);
    let _ = stream.flush();
}

/// A `status` answer with a small HTML page, or only its head for `HEAD`.
fn html_response(status: u16, head_only: bool) -> Vec<u8> {
    let body = b"<html><body><h1>Example origin</h1></body></html>";
    let reason = http::StatusCode::from_u16(status)
        .ok()
        .and_then(|code| code.canonical_reason())
        .unwrap_or("Example");
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    if !head_only {
        response.extend_from_slice(body);
    }
    response
}

/// Reads one HTTP/1.1 request, decoding a `Content-Length` or chunked body.
///
/// Returns `None` when the connection closes, times out, or sends a malformed
/// head before the request is complete.
fn read_origin_request(stream: &mut TcpStream) -> Option<OriginRequest> {
    let mut received = Vec::new();
    let head_end = loop {
        if let Some(position) = find_subslice(&received, b"\r\n\r\n") {
            break position;
        }
        read_more(stream, &mut received)?;
    };
    let received_at = Instant::now();
    let head = std::str::from_utf8(&received[..head_end]).ok()?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let mut content_length = 0;
    let mut chunked = false;
    let mut declared_length = None;
    let mut transfer_encoding = None;
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        headers.push((name.to_ascii_lowercase(), value.to_owned()));
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().ok()?;
            declared_length = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.eq_ignore_ascii_case("chunked");
            transfer_encoding = Some(value.to_owned());
        }
    }

    let mut body = received.split_off(head_end + 4);
    let body = if chunked {
        loop {
            if let Some(decoded) = decode_chunked(&body) {
                break decoded;
            }
            read_more(stream, &mut body)?;
        }
    } else {
        while body.len() < content_length {
            read_more(stream, &mut body)?;
        }
        body.truncate(content_length);
        body
    };

    Some(OriginRequest {
        method,
        path,
        body,
        content_length: declared_length,
        transfer_encoding,
        headers,
        received_at,
        answered_at: None,
    })
}

/// Appends the next read from `stream` to `buffer`.
///
/// Returns `None` at end of stream or on a read error, including a timeout.
fn read_more(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<()> {
    let mut chunk = [0_u8; 8192];
    let read = stream.read(&mut chunk).ok()?;
    if read == 0 {
        return None;
    }
    buffer.extend_from_slice(&chunk[..read]);
    Some(())
}

/// Decodes a complete chunked body, or returns `None` while it is incomplete.
fn decode_chunked(mut bytes: &[u8]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line_end = find_subslice(bytes, b"\r\n")?;
        let size_line = std::str::from_utf8(&bytes[..line_end]).ok()?;
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        bytes = &bytes[line_end + 2..];
        if size == 0 {
            // Trailer fields, if any, end at an empty line.
            loop {
                let field_end = find_subslice(bytes, b"\r\n")?;
                if field_end == 0 {
                    return Some(body);
                }
                bytes = &bytes[field_end + 2..];
            }
        }
        let data_end = size.checked_add(2)?;
        if bytes.get(size..data_end)? != b"\r\n" {
            return None;
        }
        body.extend_from_slice(&bytes[..size]);
        bytes = &bytes[data_end..];
    }
}

/// Returns the offset of the first `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Builds a valid auction request exactly one byte over [`AUCTION_BODY_CAP`].
///
/// The padding rides in `config`, which the handler accepts and filters. A
/// runtime that skipped the cap would answer 200, and one that delivered an
/// empty body would answer 400, so only an enforced cap answers 413.
fn over_cap_auction_body() -> Vec<u8> {
    let prefix = r#"{"adUnits":[{"code":"ingress-slot","mediaTypes":{"banner":{"sizes":[[300,250]]}}}],"config":{"padding":""#;
    let suffix = r#""}}"#;
    let padding = "a".repeat(AUCTION_BODY_CAP + 1 - prefix.len() - suffix.len());
    let body = format!("{prefix}{padding}{suffix}").into_bytes();
    assert_eq!(
        body.len(),
        AUCTION_BODY_CAP + 1,
        "should build a body one byte over the auction cap"
    );
    body
}

/// A `POST /auction` with a JSON body reaches the handler and gets an
/// auction response, not the parse error an empty body would cause.
fn post_auction_with_json_body_reaches_handler(runtime: IngressRuntime) {
    init_logger();
    let _port = lock_origin_port();
    let _origin = RecordingOrigin::start(origin_port(), 200);
    let process = runtime.spawn();
    let request_body = serde_json::json!({
        "adUnits": [{
            "code": "ingress-slot",
            "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
        }]
    });

    let response = reqwest::blocking::Client::new()
        .post(format!("{}/auction", process.base_url))
        .json(&request_body)
        .send()
        .expect("should send the auction request");

    let status = response.status().as_u16();
    let body = response.text().expect("should read the auction response");
    assert_eq!(
        status, 200,
        "{runtime:?} should hand the JSON body to the auction handler: {body}"
    );
    let auction_response: serde_json::Value =
        serde_json::from_str(&body).expect("should return an OpenRTB JSON response");
    assert!(
        auction_response["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{runtime:?} auction response should carry the auction id: {body}"
    );
}

/// A publisher `POST` body reaches the origin unchanged, every byte value
/// included.
fn publisher_post_body_reaches_origin_byte_for_byte(runtime: IngressRuntime) {
    init_logger();
    let _port = lock_origin_port();
    let origin = RecordingOrigin::start(origin_port(), 200);
    let process = runtime.spawn();
    let sent: Vec<u8> = (0..=u8::MAX).cycle().take(4096).collect();

    let response = reqwest::blocking::Client::new()
        .post(format!("{}{INGRESS_FORM_PATH}", process.base_url))
        .header(CONTENT_TYPE, "application/octet-stream")
        .body(sent.clone())
        .send()
        .expect("should send the publisher POST");

    assert_eq!(
        response.status().as_u16(),
        200,
        "{runtime:?} should return the origin's answer to the publisher POST"
    );
    let received = origin
        .requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.path == INGRESS_FORM_PATH)
        .expect("should record the publisher POST at the origin");
    let first_difference = received
        .body
        .iter()
        .zip(&sent)
        .position(|(received, sent)| received != sent);
    assert!(
        received.body == sent,
        "{runtime:?} should forward all {} body bytes unchanged; the origin received {} bytes, first difference at {first_difference:?}",
        sent.len(),
        received.body.len()
    );
}

/// A request for `/` returns the status the origin answered with.
fn root_returns_origin_status(runtime: IngressRuntime, method: &reqwest::Method) {
    init_logger();
    let _port = lock_origin_port();
    let _origin = RecordingOrigin::start(origin_port(), ORIGIN_PASS_THROUGH_STATUS);
    let process = runtime.spawn();

    let response = reqwest::blocking::Client::new()
        .request(method.clone(), format!("{}/", process.base_url))
        .send()
        .expect("should send the root request");

    assert_eq!(
        response.status().as_u16(),
        ORIGIN_PASS_THROUGH_STATUS,
        "{runtime:?} {method} / should return the origin's status"
    );
}

/// A chunked `POST /auction` one byte over the cap gets a `text/plain` 413.
fn chunked_post_auction_over_cap_returns_413(runtime: IngressRuntime) {
    init_logger();
    let _port = lock_origin_port();
    let _origin = RecordingOrigin::start(origin_port(), 200);
    let process = runtime.spawn();

    // A reader body has no known length, so reqwest frames it as chunked and
    // sends no `Content-Length`. The declared-length check cannot reject it;
    // only reading the body can.
    let response = reqwest::blocking::Client::new()
        .post(format!("{}/auction", process.base_url))
        .header(CONTENT_TYPE, "application/json")
        .body(reqwest::blocking::Body::new(std::io::Cursor::new(
            over_cap_auction_body(),
        )))
        .send()
        .expect("should send the chunked auction request");

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = response.text().expect("should read the rejection body");
    assert_eq!(
        status, 413,
        "{runtime:?} should reject a chunked auction body over the cap: {body}"
    );
    assert!(
        content_type.starts_with("text/plain"),
        "{runtime:?} should answer the over-cap body with text/plain, got `{content_type}`"
    );
    assert!(
        !body.is_empty(),
        "{runtime:?} should explain the over-cap rejection in the body"
    );
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn post_auction_with_json_body_reaches_handler_fastly() {
    post_auction_with_json_body_reaches_handler(IngressRuntime::Fastly);
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn post_auction_with_json_body_reaches_handler_axum() {
    post_auction_with_json_body_reaches_handler(IngressRuntime::Axum);
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn publisher_post_body_reaches_origin_byte_for_byte_fastly() {
    publisher_post_body_reaches_origin_byte_for_byte(IngressRuntime::Fastly);
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn publisher_post_body_reaches_origin_byte_for_byte_cloudflare() {
    publisher_post_body_reaches_origin_byte_for_byte(IngressRuntime::Cloudflare);
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn publisher_post_body_reaches_origin_byte_for_byte_axum() {
    publisher_post_body_reaches_origin_byte_for_byte(IngressRuntime::Axum);
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn head_root_returns_origin_status_fastly() {
    root_returns_origin_status(IngressRuntime::Fastly, &reqwest::Method::HEAD);
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn head_root_returns_origin_status_cloudflare() {
    root_returns_origin_status(IngressRuntime::Cloudflare, &reqwest::Method::HEAD);
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn head_root_returns_origin_status_axum() {
    root_returns_origin_status(IngressRuntime::Axum, &reqwest::Method::HEAD);
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn options_root_returns_origin_status_fastly() {
    root_returns_origin_status(IngressRuntime::Fastly, &reqwest::Method::OPTIONS);
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn options_root_returns_origin_status_cloudflare() {
    root_returns_origin_status(IngressRuntime::Cloudflare, &reqwest::Method::OPTIONS);
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn options_root_returns_origin_status_axum() {
    root_returns_origin_status(IngressRuntime::Axum, &reqwest::Method::OPTIONS);
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn chunked_post_auction_over_cap_returns_413_fastly() {
    chunked_post_auction_over_cap_returns_413(IngressRuntime::Fastly);
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn chunked_post_auction_over_cap_returns_413_cloudflare() {
    chunked_post_auction_over_cap_returns_413(IngressRuntime::Cloudflare);
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn chunked_post_auction_over_cap_returns_413_axum() {
    chunked_post_auction_over_cap_returns_413(IngressRuntime::Axum);
}

// ---------------------------------------------------------------------------
// First-party proxy redirect policy on the Axum binary
// ---------------------------------------------------------------------------
//
// The scripted redirect tests in `trusted-server-core` cover the policy on
// every adapter's code path. This case runs it again on the real Axum binary,
// whose `EdgeZero` client follows the chain against a loopback origin.

/// Answers the paths of the first-party proxy redirect chain.
///
/// `/start` redirects to `/final` on the same origin, `/to-blocked-host` to the
/// same origin under `localhost`, a host outside the proxy allowlist, and
/// `/to-userinfo` to a `Location` that carries userinfo. A policy that let the
/// blocked hop through would reach this origin at `/blocked`.
fn redirect_chain_response(origin_port: u16, request: &OriginRequest) -> Vec<u8> {
    let redirect = |location: &str| {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes()
    };
    match origin_request_path(request) {
        "/start" => redirect("/final"),
        "/to-blocked-host" => redirect(&format!("http://localhost:{origin_port}/blocked")),
        "/to-userinfo" => redirect(&format!(
            "http://fictional:secret@127.0.0.1:{origin_port}/userinfo"
        )),
        "/final" => b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 14\r\nConnection: close\r\n\r\nfollowed-final".to_vec(),
        _ => html_response(404, request.method == "HEAD"),
    }
}

/// The path of `request`'s target, without its query.
fn origin_request_path(request: &OriginRequest) -> &str {
    request.path.split('?').next().unwrap_or_default()
}

/// The path and query of a signed `href` from `/first-party/sign`.
///
/// The route answers with an absolute URL on `publisher.public_origin`, which
/// need not be the runtime's listener, so callers resend it to their own base URL.
fn signed_href_path_and_query(href: &str) -> String {
    let url = url::Url::parse(href).expect("should return an absolute signed href");
    url[url::Position::BeforePath..url::Position::AfterQuery].to_owned()
}

/// Signs `target` through the binary's `/first-party/sign` route and fetches
/// the first-party proxy URL it returns.
fn fetch_signed_proxy_url(
    client: &reqwest::blocking::Client,
    base_url: &str,
    target: &str,
) -> reqwest::blocking::Response {
    let signed: serde_json::Value = client
        .get(format!("{base_url}/first-party/sign"))
        .query(&[("url", target)])
        .send()
        .expect("should send the sign request")
        .error_for_status()
        .expect("should sign the loopback target")
        .json()
        .expect("should return the signed proxy URL");
    let href = signed["href"]
        .as_str()
        .expect("should return the first-party proxy href");
    client
        .get(format!("{base_url}{}", signed_href_path_and_query(href)))
        .send()
        .expect("should send the first-party proxy request")
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn first_party_proxy_redirect_policy_runs_on_axum_binary() {
    init_logger();
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).expect("should bind the redirect chain origin");
    let port = listener
        .local_addr()
        .expect("should read the redirect chain origin address")
        .port();
    let origin = RecordingOrigin::serve(
        listener,
        Arc::new(move |request| redirect_chain_response(port, request)),
    );
    let envelope =
        common::config::proxy_allowlist_app_config_envelope(origin_port(), &["127.0.0.1"])
            .expect("should build the proxy allowlist app config");
    let process = environments::axum::AxumDevServer
        .spawn_with_app_config(&envelope, &[])
        .expect("should start the Axum dev server");
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("should build a client that returns redirects");
    let target = |path: &str| format!("http://127.0.0.1:{}{path}", origin.port);

    let followed = fetch_signed_proxy_url(&client, &process.base_url, &target("/start"));
    assert_eq!(
        followed.status().as_u16(),
        200,
        "should follow a permitted hop to the final origin"
    );
    assert_eq!(
        followed.text().expect("should read the followed body"),
        "followed-final",
        "should serve the final origin's body"
    );
    let blocked = fetch_signed_proxy_url(&client, &process.base_url, &target("/to-blocked-host"));
    assert_eq!(
        blocked.status().as_u16(),
        403,
        "should answer a hop outside proxy.allowed_domains with the allowlist violation"
    );
    let userinfo = fetch_signed_proxy_url(&client, &process.base_url, &target("/to-userinfo"));
    assert_eq!(
        userinfo.status().as_u16(),
        502,
        "should answer a Location with userinfo with the proxy's 502"
    );

    let requests = origin.requests();
    let paths: Vec<&str> = requests.iter().map(origin_request_path).collect();
    assert_eq!(
        paths,
        vec!["/start", "/final", "/to-blocked-host", "/to-userinfo"],
        "should send only the permitted follow-up hop"
    );
}

// ---------------------------------------------------------------------------
// SigV4 asset host check for the EdgeZero cut
// ---------------------------------------------------------------------------
//
// `EdgeZero` drops the `Host` header Trusted Server signs and sends the URI
// authority instead, so these cases check that the authority, path, and query
// an S3 origin receives are exactly what Trusted Server signed. S3 routes need
// an `https` origin, so the origin is a loopback TLS server behind a test CA,
// and `SSL_CERT_FILE` points Viceroy's native root store at that CA.

/// Region the `SigV4` asset routes sign for.
const SIGV4_REGION: &str = "us-east-1";

/// Access key ID behind the routes' `integration_s3_access_key_id` secret.
const SIGV4_ACCESS_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// Secret access key behind the routes' `integration_s3_secret_access_key` secret.
const SIGV4_SECRET_ACCESS_KEY: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

/// One request head as the TLS asset origin received it.
#[derive(Clone, Debug)]
struct SignedAssetRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl SignedAssetRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Loopback HTTPS origin, trusted through a test CA, that records every request
/// head and answers each with a small image.
struct TlsAssetOrigin {
    port: u16,
    ca_file: tempfile::NamedTempFile,
    requests: Arc<Mutex<Vec<SignedAssetRequest>>>,
    shutdown_tx: mpsc::Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl TlsAssetOrigin {
    /// Starts the origin on an ephemeral loopback port.
    ///
    /// # Panics
    ///
    /// Panics if the test certificates or the listener cannot be set up.
    fn start() -> Self {
        let ca_key = rcgen::KeyPair::generate().expect("should generate the test CA key");
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new())
            .expect("should create the test CA parameters");
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Example test CA");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let ca_cert = ca_params
            .self_signed(&ca_key)
            .expect("should self-sign the test CA");
        let leaf_key = rcgen::KeyPair::generate().expect("should generate the origin key");
        let leaf_cert = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()])
            .expect("should create the origin certificate parameters")
            .signed_by(&leaf_key, &ca_cert, &ca_key)
            .expect("should sign the origin certificate");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls_config = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("should select TLS versions")
                .with_no_client_auth()
                .with_single_cert(
                    vec![leaf_cert.der().clone()],
                    rustls::pki_types::PrivateKeyDer::try_from(leaf_key.serialize_der())
                        .expect("should encode the origin key"),
                )
                .expect("should build the origin TLS configuration"),
        );
        let mut ca_file = tempfile::NamedTempFile::new().expect("should create the CA file");
        ca_file
            .write_all(ca_cert.pem().as_bytes())
            .expect("should write the CA certificate");

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("should bind the TLS origin");
        let port = listener
            .local_addr()
            .expect("should read the TLS origin address")
            .port();
        listener
            .set_nonblocking(true)
            .expect("should set listener nonblocking");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            while matches!(shutdown_rx.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        let recorded = Arc::clone(&recorded);
                        let tls_config = Arc::clone(&tls_config);
                        thread::spawn(move || {
                            serve_tls_asset_connection(stream, tls_config, &recorded);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port,
            ca_file,
            requests,
            shutdown_tx,
            handle: Some(handle),
        }
    }

    /// Returns every request recorded so far, in arrival order.
    fn requests(&self) -> Vec<SignedAssetRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for TlsAssetOrigin {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Completes the TLS handshake, records one request head, and answers it.
fn serve_tls_asset_connection(
    stream: TcpStream,
    tls_config: Arc<rustls::ServerConfig>,
    recorded: &Mutex<Vec<SignedAssetRequest>>,
) {
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .is_err()
    {
        return;
    }
    let Ok(connection) = rustls::ServerConnection::new(tls_config) else {
        return;
    };
    let mut tls = rustls::StreamOwned::new(connection, stream);
    let mut received = Vec::new();
    let head_end = loop {
        if let Some(position) = find_subslice(&received, b"\r\n\r\n") {
            break position;
        }
        let mut chunk = [0_u8; 4096];
        match tls.read(&mut chunk) {
            Ok(read) if read > 0 => received.extend_from_slice(&chunk[..read]),
            _ => return,
        }
    };
    let Ok(head) = std::str::from_utf8(&received[..head_end]) else {
        return;
    };
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target)) = (request_line.next(), request_line.next()) else {
        return;
    };
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    let head_only = method == "HEAD";
    recorded
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(SignedAssetRequest {
            method: method.to_owned(),
            target: target.to_owned(),
            headers,
        });

    let body = b"example-image";
    let response_head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = tls.write_all(response_head.as_bytes());
    if !head_only {
        let _ = tls.write_all(body);
    }
    tls.conn.send_close_notify();
    let _ = tls.flush();
}

/// The Viceroy integration config with `envelope` as its app config.
fn viceroy_config_with_app_config(envelope: &str) -> String {
    include_str!("../fixtures/configs/viceroy-template.toml").replace(
        "        # GENERATED_TRUSTED_SERVER_CONFIG_STORES",
        &format!(
            "        [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}]\n            format = \"inline-toml\"\n        [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}.contents]\n            {CONFIG_BLOB_KEY} = '''{envelope}'''"
        ),
    )
}

/// Asserts that `request` carries the `Host`, path, and query Trusted Server signed.
///
/// The recorded `Host` and request target must equal the expected values, and
/// recomputing the signature from what the origin received must reproduce the
/// request's `Authorization`, which holds only if those are the signed values.
fn assert_sent_as_signed(request: &SignedAssetRequest, expected_host: &str, expected_target: &str) {
    let host = request
        .header("host")
        .expect("should record the Host header");
    assert_eq!(
        (host, request.target.as_str()),
        (expected_host, expected_target),
        "should send the {} to the signed authority and target",
        request.method
    );
    let authorization = request
        .header("authorization")
        .expect("should send a SigV4 Authorization header");
    let amz_date = request
        .header("x-amz-date")
        .expect("should send the signing timestamp");
    let method =
        http::Method::from_bytes(request.method.as_bytes()).expect("should record a valid method");
    let recomputed = trusted_server_core::test_support::s3_sigv4_authorization(
        &method,
        host,
        &request.target,
        amz_date,
        SIGV4_REGION,
        SIGV4_ACCESS_KEY_ID,
        SIGV4_SECRET_ACCESS_KEY,
    )
    .expect("should recompute the signature from the received request");
    assert_eq!(
        authorization, recomputed,
        "should sign exactly the Host, path, and query the {} reached the origin with",
        request.method
    );
}

#[test]
#[ignore = "requires Viceroy and the release Fastly WASM"]
fn sigv4_signed_asset_reaches_origin_with_signed_host_path_and_query() {
    init_logger();
    let origin = TlsAssetOrigin::start();
    let expected_host = format!("127.0.0.1:{}", origin.port);
    let envelope = common::config::sigv4_asset_app_config_envelope(
        origin_port(),
        &format!("https://{expected_host}"),
    )
    .expect("should build the SigV4 asset app config");
    let config_dir = tempfile::tempdir().expect("should create the Viceroy config directory");
    let config_path = config_dir.path().join("viceroy.toml");
    std::fs::write(&config_path, viceroy_config_with_app_config(&envelope))
        .expect("should write the Viceroy config");

    let image_status = temp_env::with_vars(
        [
            ("SSL_CERT_FILE", Some(origin.ca_file.path().as_os_str())),
            ("VICEROY_CONFIG_PATH", Some(config_path.as_os_str())),
        ],
        || {
            let process = environments::fastly::FastlyViceroy
                .spawn(&wasm_binary_path())
                .expect("should start Viceroy");
            let client = reqwest::blocking::Client::new();
            let plain = client
                .get(format!(
                    "{}/.sigv4-assets/photos/cat%20one.jpg?version=2&name=a%2Fb",
                    process.base_url
                ))
                .send()
                .expect("should send the plain asset request");
            assert_eq!(
                plain.status().as_u16(),
                200,
                "should serve the signed asset from the origin"
            );
            // Viceroy cannot run Image Optimizer, so only the preflight before it is checked.
            client
                .get(format!(
                    "{}/.sigv4-images/photos/dog.jpg?profile=default",
                    process.base_url
                ))
                .send()
                .expect("should send the optimized asset request")
                .status()
        },
    );
    log::info!("optimized SigV4 asset answered {image_status}");

    let requests = origin.requests();
    let plain = requests
        .iter()
        .find(|request| request.method == "GET" && request.target.starts_with("/.sigv4-assets/"))
        .expect("should record the signed asset GET");
    assert_sent_as_signed(
        plain,
        &expected_host,
        "/.sigv4-assets/photos/cat%20one.jpg?version=2&name=a%2Fb",
    );
    let preflight = requests
        .iter()
        .find(|request| request.method == "HEAD" && request.target.starts_with("/.sigv4-images/"))
        .expect("should record the signed S3 HEAD preflight");
    assert_sent_as_signed(preflight, &expected_host, "/.sigv4-images/photos/dog.jpg");
}

// ---------------------------------------------------------------------------
// Auction fan-out against loopback bidders (spec test 5)
// ---------------------------------------------------------------------------

/// Impression the runtime auction cases request.
const AUCTION_SLOT: &str = "auction-slot";

/// A TCF v2 `euconsent-v2` cookie that grants Purpose 1.
///
/// Runtimes without a geo source treat the jurisdiction as unknown, where the
/// server-side auction needs Purpose 1. The construction matches the HTML
/// end-of-file harness's consent cookie, which lives in another test binary.
fn tcf_purpose_one_cookie() -> String {
    let mut bits = [0_u8; 29];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("should have a current fixture clock")
        .as_millis()
        / 100;
    for (offset, width, value) in [
        (0, 6, 2),
        (6, 36, now),
        (42, 36, now),
        (108, 6, 4),
        (114, 6, 13),
        (152, 1, 1),
    ] {
        for index in 0..width {
            if value & (1 << (width - index - 1)) != 0 {
                bits[(offset + index) / 8] |= 1 << (7 - (offset + index) % 8);
            }
        }
    }
    format!(
        "euconsent-v2={}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bits)
    )
}

/// A loopback bidder on its own port that answers each bid request with one bid.
///
/// It waits `delay` before answering and records every request it receives,
/// framing included.
fn start_loopback_bidder(seat: &str, price: f64, delay: Duration) -> RecordingOrigin {
    let body = serde_json::to_vec(&serde_json::json!({
        "seatbid": [{"seat": seat, "bid": [{
            "id": format!("{seat}-bid"),
            "impid": AUCTION_SLOT,
            "price": price,
            "adm": "<div>loopback bid</div>",
            "w": 300,
            "h": 250
        }]}]
    }))
    .expect("should serialize the loopback bid");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("should bind a loopback bidder port");
    RecordingOrigin::serve(
        listener,
        Arc::new(move |_request| {
            thread::sleep(delay);
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            response.extend_from_slice(&body);
            response
        }),
    )
}

/// Configures one standard provider per loopback bidder, at `/bid` on its port.
fn loopback_bidder_fixtures(
    bidders: &[(&str, &RecordingOrigin, Duration)],
) -> Vec<common::config::AuctionBidderFixture> {
    bidders
        .iter()
        .map(
            |(id, bidder, timeout)| common::config::AuctionBidderFixture {
                id: (*id).to_owned(),
                endpoint: format!("http://127.0.0.1:{}/bid", bidder.port),
                timeout_ms: u32::try_from(timeout.as_millis())
                    .expect("should fit the provider timeout in u32"),
            },
        )
        .collect()
}

/// Sends one `/auction` request for [`AUCTION_SLOT`] and returns its `OpenRTB` response.
fn post_auction(base_url: &str) -> serde_json::Value {
    let response = reqwest::blocking::Client::new()
        .post(format!("{base_url}/auction"))
        .header("cookie", tcf_purpose_one_cookie())
        .header("cf-ipcountry", "US")
        .json(&serde_json::json!({
            "adUnits": [{
                "code": AUCTION_SLOT,
                "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
            }]
        }))
        .timeout(Duration::from_secs(30))
        .send()
        .expect("should send the auction request");
    let status = response.status().as_u16();
    let body = response.text().expect("should read the auction response");
    assert_eq!(status, 200, "should complete the auction: {body}");
    serde_json::from_str(&body).expect("should return an OpenRTB JSON response")
}

/// The provider summary the auction response carries for `provider`.
fn provider_detail<'a>(response: &'a serde_json::Value, provider: &str) -> &'a serde_json::Value {
    let detail = response["ext"]["orchestrator"]["provider_details"]
        .as_array()
        .expect("should list provider details")
        .iter()
        .find(|detail| detail["name"] == provider);
    assert!(
        detail.is_some(),
        "should report provider {provider}: {response}"
    );
    detail.expect("should have found the provider detail")
}

/// Asserts every bidder received `auctions` requests with a JSON bid request body,
/// and logs the framing each request arrived with.
fn assert_bidders_received(runtime: &str, bidders: &[&RecordingOrigin], auctions: usize) {
    for bidder in bidders {
        let requests = bidder.requests();
        assert_eq!(
            requests.len(),
            auctions,
            "{runtime} should send one bid request per auction to port {}",
            bidder.port
        );
        for request in &requests {
            log::info!(
                "{runtime} bid request framing: content-length {:?}, transfer-encoding {:?}",
                request.content_length,
                request.transfer_encoding
            );
            assert_eq!(
                (request.method.as_str(), request.path.as_str()),
                ("POST", "/bid"),
                "{runtime} should POST the bid request to the provider path"
            );
            let bid_request = serde_json::from_slice::<serde_json::Value>(&request.body);
            assert!(
                bid_request.is_ok(),
                "{runtime} should deliver the whole JSON bid request body: {bid_request:?}"
            );
        }
    }
}

/// Runs a two-provider auction on `runtime` and asserts both bids arrive.
fn multi_provider_auction_runs(runtime: common::trace_boundary::TraceRuntime, name: &str) {
    init_logger();
    let first = start_loopback_bidder("first-seat", 1.25, Duration::ZERO);
    let second = start_loopback_bidder("second-seat", 2.5, Duration::ZERO);
    let envelope = common::config::auction_app_config_envelope(
        origin_port(),
        &loopback_bidder_fixtures(&[
            ("first", &first, Duration::from_millis(1500)),
            ("second", &second, Duration::from_millis(1500)),
        ]),
        2000,
    )
    .expect("should build the multi-provider auction config");

    common::trace_boundary::with_trace_runtime(
        runtime,
        &envelope,
        Some("http://127.0.0.1:*"),
        |base_url| {
            let response = post_auction(base_url);
            assert_eq!(
                (
                    &response["ext"]["orchestrator"]["providers"],
                    &response["ext"]["orchestrator"]["total_bids"]
                ),
                (&serde_json::json!(2), &serde_json::json!(2)),
                "{name} should fan out to both providers and collect both bids: {response}"
            );
            assert_eq!(
                response["seatbid"][0]["bid"][0]["price"],
                serde_json::json!(2.5),
                "{name} should deliver the higher bid: {response}"
            );
        },
    );

    assert_bidders_received(name, &[&first, &second], 1);
}

#[test]
#[ignore = "requires Viceroy and the release Fastly WASM"]
fn twelve_bidder_auction_registers_twelve_backends_fastly() {
    init_logger();
    let bidders: Vec<RecordingOrigin> = (1..=12)
        .map(|index| {
            start_loopback_bidder(
                &format!("seat-{index:02}"),
                f64::from(index),
                Duration::from_millis(20),
            )
        })
        .collect();
    let ids: Vec<String> = (1..=12).map(|index| format!("bidder-{index:02}")).collect();
    let fixtures = loopback_bidder_fixtures(
        &ids.iter()
            .zip(&bidders)
            .map(|(id, bidder)| (id.as_str(), bidder, Duration::from_millis(1500)))
            .collect::<Vec<_>>(),
    );
    let envelope = common::config::auction_app_config_envelope(origin_port(), &fixtures, 2000)
        .expect("should build the twelve-bidder auction config");

    common::trace_boundary::with_fastly_runtime(&envelope, |base_url, stdout| {
        // Each bidder listens on its own port, so each needs its own backend.
        // Both auctions run on one configuration with the same budget. Each
        // callback's observation is its own, so this cannot show that the
        // second auction reuses the first one's backend names.
        for auction in 1..=2 {
            let response = post_auction(base_url);
            assert_eq!(
                (
                    &response["ext"]["orchestrator"]["providers"],
                    &response["ext"]["orchestrator"]["total_bids"]
                ),
                (&serde_json::json!(12), &serde_json::json!(12)),
                "auction {auction} should collect one bid from each of the twelve bidders: {response}"
            );
            for id in &ids {
                assert_eq!(
                    provider_detail(&response, id)["bid_count"],
                    serde_json::json!(1),
                    "auction {auction} should parse the bid from {id}"
                );
            }
            assert_eq!(
                response["seatbid"][0]["bid"][0]["price"],
                serde_json::json!(12.0),
                "auction {auction} should deliver the highest of the twelve bids"
            );
        }
        // Each callback has its own observation, so this shows that each
        // auction configures and registers exactly one backend per bidder
        // and no other outbound backend.
        assert_eq!(
            wait_for_outbound_observations(stdout, 2),
            [(12, 12), (12, 12)],
            "each auction should configure and register exactly one backend per bidder"
        );
    });

    assert_bidders_received("Fastly", &bidders.iter().collect::<Vec<_>>(), 2);
}

/// Waits up to five seconds for the guest to log `expected` outbound
/// observations, and returns the configuration and registration counts of each.
///
/// The callback logs its observation after delivery, so the line can trail
/// the response the client already read. Viceroy prints a line once for the
/// `tslog` endpoint and once more as the guest's stdout echo, so only the
/// endpoint copy counts.
fn wait_for_outbound_observations(
    stdout: &environments::fastly::GuestStdout,
    expected: usize,
) -> Vec<(u64, u64)> {
    let count = |line: &str, field: &str| -> u64 {
        line.split_whitespace()
            .find_map(|entry| entry.strip_prefix(field))
            .and_then(|value| value.parse().ok())
            .expect("should log every outbound observation count")
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let observations: Vec<(u64, u64)> = stdout
            .lines()
            .iter()
            .filter_map(|line| line.strip_prefix("tslog :: "))
            .filter(|line| line.contains("[outbound_observation]"))
            .map(|line| {
                (
                    count(line, "configurations="),
                    count(line, "registrations="),
                )
            })
            .collect();
        if observations.len() >= expected || std::time::Instant::now() >= deadline {
            return observations;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn multi_provider_auction_runs_cloudflare() {
    multi_provider_auction_runs(
        common::trace_boundary::TraceRuntime::Cloudflare,
        "Cloudflare",
    );
}

#[test]
#[ignore = "requires Spin and the current production Spin WASM artifact"]
fn multi_provider_auction_runs_spin() {
    multi_provider_auction_runs(common::trace_boundary::TraceRuntime::Spin, "Spin");
}

#[test]
#[ignore = "requires Spin and the current production Spin WASM artifact"]
fn spin_runtime_auction_reports_one_timeout_and_cutoff() {
    init_logger();
    // The first bidder's own 200 ms timeout ends before the 1000 ms auction
    // cutoff. The second bidder has time left but answers after the cutoff.
    let early_timeout = start_loopback_bidder("early-seat", 1.0, Duration::from_millis(600));
    let after_cutoff = start_loopback_bidder("late-seat", 2.0, Duration::from_millis(1600));
    let envelope = common::config::auction_app_config_envelope(
        origin_port(),
        &loopback_bidder_fixtures(&[
            ("early-timeout", &early_timeout, Duration::from_millis(200)),
            ("after-cutoff", &after_cutoff, Duration::from_secs(2)),
        ]),
        1000,
    )
    .expect("should build the timeout and cutoff auction config");

    common::trace_boundary::with_trace_runtime(
        common::trace_boundary::TraceRuntime::Spin,
        &envelope,
        Some("http://127.0.0.1:*"),
        |base_url| {
            let response = post_auction(base_url);
            let early = provider_detail(&response, "early-timeout");
            let late = provider_detail(&response, "after-cutoff");
            assert_eq!(
                (
                    &early["metadata"]["error_type"],
                    &late["metadata"]["error_type"]
                ),
                (&serde_json::json!("timeout"), &serde_json::json!("timeout")),
                "should report the provider timeout and the cutoff as timeouts, not a failed batch: {response}"
            );
            let early_ms = early["time_ms"]
                .as_u64()
                .expect("should time the early timeout");
            let late_ms = late["time_ms"].as_u64().expect("should time the cutoff");
            assert!(
                early_ms < 1000 && late_ms >= 1000,
                "should end the first bidder at its own timeout and the second at the cutoff, got {early_ms} ms and {late_ms} ms: {response}"
            );
            assert!(
                response["seatbid"].as_array().is_none_or(Vec::is_empty),
                "should deliver no bid: {response}"
            );
        },
    );
}

// ---------------------------------------------------------------------------
// Publisher origin through EdgeZero's client (spec tests 1, 2, and 6)
// ---------------------------------------------------------------------------

/// A runtime the publisher-origin cases serve an isolated app config on.
#[derive(Clone, Copy, Debug)]
enum OriginRuntime {
    Axum,
    Cloudflare,
    Fastly,
    Spin,
}

impl OriginRuntime {
    /// Serves `envelope` on this runtime and runs `check` against its base URL.
    ///
    /// Spin may reach any loopback port, so loopback origins and bidders work there.
    ///
    /// # Panics
    ///
    /// Panics when the runtime's artifact or CLI is missing or it never becomes ready.
    fn serve(self, envelope: &str, check: impl FnOnce(&str)) {
        match self {
            Self::Axum => {
                let process = environments::axum::AxumDevServer
                    .spawn_with_app_config(envelope, &[])
                    .expect("should start the Axum dev server");
                check(&process.base_url);
            }
            Self::Cloudflare => with_trace_runtime(TraceRuntime::Cloudflare, envelope, None, check),
            Self::Fastly => with_trace_runtime(TraceRuntime::Fastly, envelope, None, check),
            Self::Spin => with_trace_runtime(
                TraceRuntime::Spin,
                envelope,
                Some("http://127.0.0.1:*"),
                check,
            ),
        }
    }
}

/// A `GET /` that carries body bytes returns 400 and never reaches the origin
/// (EZ-1 criterion 13).
fn get_root_with_body_returns_400(runtime: IngressRuntime) {
    init_logger();
    let _port = lock_origin_port();
    let origin = RecordingOrigin::start(origin_port(), ORIGIN_PASS_THROUGH_STATUS);
    let process = runtime.spawn();
    // Readiness probes may already have fetched the root page from the origin.
    let probes = origin.requests().len();

    let response = reqwest::blocking::Client::new()
        .get(format!("{}/", process.base_url))
        .header(CONTENT_TYPE, "text/plain")
        .body("example GET body")
        .send()
        .expect("should send the GET with a body");

    assert_eq!(
        response.status().as_u16(),
        400,
        "{runtime:?} should refuse to forward a GET that carries a body"
    );
    let forwarded = origin.requests();
    assert_eq!(
        forwarded.len(),
        probes,
        "{runtime:?} should not forward the GET with a body to the origin: {:?}",
        &forwarded[probes..]
    );
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn get_root_with_body_returns_400_fastly() {
    get_root_with_body_returns_400(IngressRuntime::Fastly);
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn get_root_with_body_returns_400_cloudflare() {
    get_root_with_body_returns_400(IngressRuntime::Cloudflare);
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn get_root_with_body_returns_400_axum() {
    get_root_with_body_returns_400(IngressRuntime::Axum);
}

/// A POST whose `Content-Length` is malformed, or repeated with different
/// values, returns 400 at ingress and never reaches the origin.
///
/// The GTM beacon handler no longer checks the field itself, so this pins the
/// answer where it moved. The integration config leaves GTM off, so both the
/// beacon path and the form path reach the publisher fallback, which would
/// forward a well-framed request to the origin.
fn bad_content_length_returns_400(runtime: IngressRuntime) {
    init_logger();
    let _port = lock_origin_port();
    let origin = RecordingOrigin::start(origin_port(), ORIGIN_PASS_THROUGH_STATUS);
    let process = runtime.spawn();
    // Readiness probes may already have fetched the root page from the origin.
    let probes = origin.requests().len();

    for path in [
        "/integrations/google_tag_manager/collect",
        INGRESS_FORM_PATH,
    ] {
        for (framing, length_fields) in [
            ("malformed", "Content-Length: not-a-number\r\n"),
            ("duplicate", "Content-Length: 1\r\nContent-Length: 2\r\n"),
        ] {
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-www-form-urlencoded\r\n{length_fields}Connection: close\r\n\r\nab"
            );

            assert_eq!(
                raw_request_status(&process.base_url, request.as_bytes()),
                Some(400),
                "{runtime:?} should refuse a POST to {path} with a {framing} Content-Length"
            );
        }
    }
    let forwarded = origin.requests();
    assert_eq!(
        forwarded.len(),
        probes,
        "{runtime:?} should forward no request with a bad Content-Length to the origin: {:?}",
        &forwarded[probes..]
    );
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn bad_content_length_returns_400_fastly() {
    bad_content_length_returns_400(IngressRuntime::Fastly);
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn bad_content_length_returns_400_axum() {
    bad_content_length_returns_400(IngressRuntime::Axum);
}

/// Serves the shared integration app config on Spin, with the recording origin
/// on the fixed origin port, and runs `check` against its base URL.
fn with_spin_ingress(status: u16, check: impl FnOnce(&str, &RecordingOrigin)) {
    init_logger();
    let _port = lock_origin_port();
    let origin = RecordingOrigin::start(origin_port(), status);
    let envelope = common::config::integration_app_config_envelope(origin_port())
        .expect("should build the integration app config");
    OriginRuntime::Spin.serve(&envelope, |base_url| check(base_url, &origin));
}

#[test]
#[ignore = "requires Spin and the current production Spin WASM artifact"]
fn spin_runtime_post_auction_with_json_body() {
    with_spin_ingress(200, |base_url, _origin| {
        let response = reqwest::blocking::Client::new()
            .post(format!("{base_url}/auction"))
            .json(&serde_json::json!({
                "adUnits": [{
                    "code": "ingress-slot",
                    "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                }]
            }))
            .send()
            .expect("should send the auction request");
        let status = response.status().as_u16();
        let body = response.text().expect("should read the auction response");
        assert_eq!(
            status, 200,
            "Spin should hand the JSON body to the auction handler: {body}"
        );
        let auction: serde_json::Value =
            serde_json::from_str(&body).expect("should return an OpenRTB JSON response");
        assert!(
            auction["id"].as_str().is_some_and(|id| !id.is_empty()),
            "Spin auction response should carry the auction id: {body}"
        );
    });
}

#[test]
#[ignore = "requires Spin and the current production Spin WASM artifact"]
fn spin_runtime_publisher_post_body_reaches_origin() {
    with_spin_ingress(200, |base_url, origin| {
        let sent: Vec<u8> = (0..=u8::MAX).cycle().take(4096).collect();
        let response = reqwest::blocking::Client::new()
            .post(format!("{base_url}{INGRESS_FORM_PATH}"))
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(sent.clone())
            .send()
            .expect("should send the publisher POST");
        assert_eq!(
            response.status().as_u16(),
            200,
            "Spin should return the origin's answer to the publisher POST"
        );
        let received = origin
            .requests()
            .into_iter()
            .find(|request| request.method == "POST" && request.path == INGRESS_FORM_PATH)
            .expect("should record the publisher POST at the origin");
        assert!(
            received.body == sent,
            "Spin should forward all {} body bytes unchanged; the origin received {} bytes",
            sent.len(),
            received.body.len()
        );
        // EdgeZero's Spin client sends a streamed upload chunked and drops its
        // declared length. Trusted Server asked EdgeZero to keep the declared
        // `Content-Length` on PR 275
        // (https://github.com/IABTechLab/edgezero/pull/275#issuecomment-6863865880).
        // Once a repin carries that fix, this assertion fails: expect
        // `Content-Length: 4096` and no `Transfer-Encoding` instead.
        assert_eq!(
            (
                received.content_length.as_deref(),
                received.transfer_encoding.as_deref()
            ),
            (None, Some("chunked")),
            "Spin should forward a streamed upload chunked until EdgeZero keeps its declared length"
        );
    });
}

#[test]
#[ignore = "requires Spin and the current production Spin WASM artifact"]
fn spin_runtime_head_root() {
    with_spin_ingress(ORIGIN_PASS_THROUGH_STATUS, |base_url, _origin| {
        let response = reqwest::blocking::Client::new()
            .head(format!("{base_url}/"))
            .send()
            .expect("should send the HEAD request");
        assert_eq!(
            response.status().as_u16(),
            ORIGIN_PASS_THROUGH_STATUS,
            "Spin HEAD / should return the origin's status"
        );
    });
}

// ---------------------------------------------------------------------------
// Bodiless publisher responses
// ---------------------------------------------------------------------------

/// Publisher path the bodiless-status cases request.
const BODILESS_PAGE_PATH: &str = "/bodiless-page";

/// Entity tag for which the origin answers a conditional GET with 304.
const BODILESS_PAGE_ETAG: &str = "\"example-etag\"";

/// Answers a HEAD or a matching conditional GET for [`BODILESS_PAGE_PATH`]
/// with a bodiless head that declares `Trailer`, and anything else with a
/// small HTML page.
fn bodiless_page_response(request: &OriginRequest) -> Vec<u8> {
    let head_only = request.method == "HEAD";
    let not_modified = request.header("if-none-match") == Some(BODILESS_PAGE_ETAG);
    if request.path != BODILESS_PAGE_PATH || !(head_only || not_modified) {
        return html_response(200, head_only);
    }
    let status = if head_only {
        ORIGIN_PASS_THROUGH_STATUS
    } else {
        304
    };
    raw_response(
        status,
        &[
            ("Content-Type", "text/html; charset=utf-8".to_owned()),
            ("ETag", BODILESS_PAGE_ETAG.to_owned()),
            ("Trailer", "expires".to_owned()),
        ],
        b"",
    )
}

/// A publisher HEAD and a conditional GET that the origin answers with 304
/// return their real status, with no `Trailer`, although the origin declares one.
fn publisher_head_and_conditional_304_return_real_status(runtime: OriginRuntime) {
    init_logger();
    let _port = lock_origin_port();
    let origin = RecordingOrigin::serve(
        TcpListener::bind(("127.0.0.1", origin_port())).expect("should bind origin port"),
        Arc::new(bodiless_page_response),
    );
    let envelope = common::config::integration_app_config_envelope(origin_port())
        .expect("should build the integration app config");

    runtime.serve(&envelope, |base_url| {
        let url = format!("{base_url}{BODILESS_PAGE_PATH}");
        let head = raw_client()
            .head(&url)
            .send()
            .expect("should send the publisher HEAD");
        let conditional = raw_client()
            .get(&url)
            .header("if-none-match", BODILESS_PAGE_ETAG)
            .send()
            .expect("should send the conditional publisher GET");
        for (request, response, expected) in [
            ("HEAD", &head, ORIGIN_PASS_THROUGH_STATUS),
            ("conditional GET", &conditional, 304),
        ] {
            assert_eq!(
                response.status().as_u16(),
                expected,
                "{runtime:?} {request} should return the origin's status"
            );
            assert!(
                response.headers().get("trailer").is_none(),
                "{runtime:?} {request} should not send Trailer"
            );
        }
    });

    assert!(
        origin.requests().iter().any(|request| {
            request.method == "GET"
                && request.path == BODILESS_PAGE_PATH
                && request.header("if-none-match") == Some(BODILESS_PAGE_ETAG)
        }),
        "{runtime:?} should forward the validator, so the 304 is the origin's"
    );
}

#[test]
#[ignore = "requires Viceroy and the release Fastly WASM"]
fn publisher_head_and_conditional_304_return_real_status_fastly() {
    publisher_head_and_conditional_304_return_real_status(OriginRuntime::Fastly);
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn publisher_head_and_conditional_304_return_real_status_axum() {
    publisher_head_and_conditional_304_return_real_status(OriginRuntime::Axum);
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn publisher_head_and_conditional_304_return_real_status_cloudflare() {
    publisher_head_and_conditional_304_return_real_status(OriginRuntime::Cloudflare);
}

#[test]
#[ignore = "requires Spin and the current production Spin WASM artifact"]
fn publisher_head_and_conditional_304_return_real_status_spin() {
    publisher_head_and_conditional_304_return_real_status(OriginRuntime::Spin);
}

// ---------------------------------------------------------------------------
// Axum ingress variables
// ---------------------------------------------------------------------------

/// Raw-head limit the raised case sets, twice the listener's 64 KiB default.
const RAISED_RAW_HEAD_BYTES: &str = "131072";

/// A `GET /health` whose raw head exceeds the Axum listener's default 64 KiB
/// limit while its header fields stay under `EdgeZero`'s 64 KiB header limit.
///
/// `EdgeZero` counts only header fields toward its limit, so the request line
/// carries the excess, with a target under `EdgeZero`'s 8192 bytes.
fn oversized_head_request() -> Vec<u8> {
    let mut request = format!(
        "GET /health?padding={} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n",
        "a".repeat(7000)
    );
    // 58 fields of 1,040 bytes plus `Host` and `Connection`: 60 field lines
    // of 60,356 bytes. With the 7,016-byte target the raw head is 67,389
    // bytes (about 66 KiB), 1,853 over the default 65,536.
    for index in 0..58 {
        request.push_str(&format!("x-example-{index:02}: {}\r\n", "b".repeat(1024)));
    }
    request.push_str("\r\n");
    request.into_bytes()
}

/// Sends `request` to the runtime at `base_url` and returns the response
/// status, or `None` when the runtime closed the connection without one.
fn raw_request_status(base_url: &str, request: &[u8]) -> Option<u16> {
    let address = base_url
        .strip_prefix("http://")
        .expect("should serve plain HTTP");
    let mut stream = TcpStream::connect(address).expect("should connect to the runtime");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("should set a read timeout");
    // A runtime that refuses the head may close before reading all of it.
    let _ = stream.write_all(request);
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    std::str::from_utf8(&response)
        .ok()?
        .strip_prefix("HTTP/1.1 ")?
        .get(..3)?
        .parse()
        .ok()
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn axum_ingress_variable_raises_limit() {
    init_logger();
    let envelope = common::config::integration_app_config_envelope(origin_port())
        .expect("should build the integration app config");
    let request = oversized_head_request();

    let default = environments::axum::AxumDevServer
        .spawn_with_app_config(&envelope, &[])
        .expect("should start the Axum dev server");
    let default_status = raw_request_status(&default.base_url, &request);
    drop(default);
    let raised = environments::axum::AxumDevServer
        .spawn_with_app_config(
            &envelope,
            &[(
                "EDGEZERO__ADAPTER__INGRESS__MAX_RAW_HEAD_BYTES",
                RAISED_RAW_HEAD_BYTES,
            )],
        )
        .expect("should start the Axum dev server with a raised raw-head limit");
    let raised_status = raw_request_status(&raised.base_url, &request);

    assert_ne!(
        default_status,
        Some(200),
        "should refuse the head under the default raw-head limit, or the case proves nothing"
    );
    assert_eq!(
        raised_status,
        Some(200),
        "should admit the same head once the variable raises the raw-head limit"
    );
}

/// Waits up to `deadline` for `child` to exit, killing it if it does not.
///
/// Returns `None` when the child was still running at the deadline.
fn wait_for_exit(child: &mut Child, deadline: Duration) -> Option<ExitStatus> {
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("should poll the child process") {
            return Some(status);
        }
        if started.elapsed() >= deadline {
            child.kill().expect("should stop the child process");
            child.wait().expect("should reap the child process");
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn axum_invalid_ingress_variable_fails_startup() {
    init_logger();
    let envelope = common::config::integration_app_config_envelope(origin_port())
        .expect("should build the integration app config");
    let port = environments::find_available_port().expect("should reserve a port");
    let mut child = environments::axum::AxumDevServer
        .command(&envelope, port)
        .env("EDGEZERO__ADAPTER__INGRESS__MAX_HEADER_COUNT", "0")
        .env("EDGEZERO__LOGGING__LEVEL", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("should spawn the Axum binary");

    let status = wait_for_exit(&mut child, Duration::from_secs(10));
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout
            .read_to_string(&mut output)
            .expect("should read the Axum binary stdout");
    }
    if let Some(mut stderr) = child.stderr.take() {
        stderr
            .read_to_string(&mut output)
            .expect("should read the Axum binary stderr");
    }

    let status = status.unwrap_or_else(|| {
        panic!("should exit instead of serving with an invalid ingress variable; output:\n{output}")
    });
    assert!(
        !status.success(),
        "should exit non-zero with an invalid ingress variable, got {status}; output:\n{output}"
    );
    assert!(
        output.contains("invalid Axum ingress setting `max_header_count`"),
        "should name the invalid ingress setting; output:\n{output}"
    );
    assert!(
        !output.contains("starting axum server"),
        "should not reach the listener; output:\n{output}"
    );
}

/// Page the navigation bid cases load.
const NAVIGATION_PATH: &str = "/article";

/// When the navigation origin's head arrives.
const NAVIGATION_ORIGIN_DELAY: Duration = Duration::from_millis(700);

/// The navigation auction's deadline.
const NAVIGATION_AUCTION_TIMEOUT_MS: u32 = 500;

/// Pre-seeded EC row of the Viceroy template.
///
/// The Fastly navigation still takes the plain path, not the overlapped one.
/// Viceroy supplies no JA4 fingerprint, so the bot gate gives the request no
/// KV graph and no EC snapshot preload runs.
const NAVIGATION_EC_ID: &str =
    "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.test05";

/// The creative a navigation bidder answering after `delay` delivers.
fn navigation_creative(delay: Duration) -> String {
    format!("example-creative-{}", delay.as_millis())
}

/// A loopback bidder that waits `delay` and bids on its own slot.
fn start_navigation_bidder(delay: Duration) -> RecordingOrigin {
    let slot = format!("slot-{}", delay.as_millis());
    let body = serde_json::to_vec(&serde_json::json!({
        "seatbid": [{"seat": format!("seat-{}", delay.as_millis()), "bid": [{
            "id": format!("bid-{}", delay.as_millis()),
            "impid": slot,
            "price": 1.5,
            "adm": navigation_creative(delay),
            "w": 300,
            "h": 250
        }]}]
    }))
    .expect("should serialize the navigation bid");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("should bind a bidder port");
    RecordingOrigin::serve(
        listener,
        Arc::new(move |_request| {
            thread::sleep(delay);
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            response.extend_from_slice(&body);
            response
        }),
    )
}

/// The publisher origin of the navigation cases: its head arrives after
/// [`NAVIGATION_ORIGIN_DELAY`].
fn start_slow_navigation_origin() -> RecordingOrigin {
    let listener =
        TcpListener::bind(("127.0.0.1", origin_port())).expect("should bind the origin port");
    RecordingOrigin::serve(
        listener,
        Arc::new(|_request| {
            thread::sleep(NAVIGATION_ORIGIN_DELAY);
            let body = b"<html><head><title>Example</title></head><body><main>example article</main></body></html>";
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            response.extend_from_slice(body);
            response
        }),
    )
}

/// The navigation auction config for one bidder per delay in `delays`, each
/// bidding on its own slot.
fn navigation_envelope(delays: &[Duration], bidders: &[RecordingOrigin]) -> String {
    let ids: Vec<String> = delays
        .iter()
        .map(|delay| format!("bidder-{}", delay.as_millis()))
        .collect();
    let fixtures = loopback_bidder_fixtures(
        &ids.iter()
            .zip(bidders)
            .map(|(id, bidder)| (id.as_str(), bidder, Duration::from_millis(1500)))
            .collect::<Vec<_>>(),
    );
    let slots: Vec<String> = delays
        .iter()
        .map(|delay| format!("slot-{}", delay.as_millis()))
        .collect();
    common::config::navigation_auction_app_config_envelope(
        origin_port(),
        &fixtures,
        NAVIGATION_AUCTION_TIMEOUT_MS,
        NAVIGATION_PATH,
        &slots.iter().map(String::as_str).collect::<Vec<_>>(),
    )
    .expect("should build the navigation auction config")
}

/// Loads [`NAVIGATION_PATH`] as a document navigation that carries consent
/// and [`NAVIGATION_EC_ID`], and returns the page `runtime` served.
fn load_navigation(runtime: OriginRuntime, base_url: &str) -> String {
    let response = reqwest::blocking::Client::new()
        .get(format!("{base_url}{NAVIGATION_PATH}"))
        .header(
            "cookie",
            format!("{}; ts-ec={NAVIGATION_EC_ID}", tcf_purpose_one_cookie()),
        )
        .header("cf-ipcountry", "US")
        .header("accept", "text/html")
        .header("sec-fetch-dest", "document")
        .header("sec-fetch-mode", "navigate")
        .timeout(Duration::from_secs(30))
        .send()
        .expect("should load the navigation");
    let status = response.status().as_u16();
    let html = response.text().expect("should read the navigation page");
    assert_eq!(status, 200, "{runtime:?} should serve the page: {html}");
    html
}

/// Loads a navigation whose 500 ms auction races bidders at 180, 320, and 600 ms
/// and an origin whose head arrives at 700 ms (EZ-6).
fn navigation_bids_include_180_and_320_ms_and_exclude_600_ms(runtime: OriginRuntime) {
    init_logger();
    let _port = lock_origin_port();
    let _origin = start_slow_navigation_origin();
    let delays = [180, 320, 600].map(Duration::from_millis);
    let bidders = delays.map(start_navigation_bidder);
    let envelope = navigation_envelope(&delays, &bidders);

    runtime.serve(&envelope, |base_url| {
        let html = load_navigation(runtime, base_url);
        for delay in [180, 320].map(Duration::from_millis) {
            assert!(
                html.contains(&navigation_creative(delay)),
                "{runtime:?} should inject the bid that answered at {} ms: {html}",
                delay.as_millis()
            );
        }
        assert!(
            !html.contains(&navigation_creative(Duration::from_millis(600))),
            "{runtime:?} should drop the bid that answered after the 500 ms cutoff: {html}"
        );
    });

    assert_bidders_received(
        &format!("{runtime:?}"),
        &bidders.iter().collect::<Vec<_>>(),
        1,
    );
}

#[test]
#[ignore = "requires Viceroy and the release Fastly WASM"]
fn navigation_bids_include_180_and_320_ms_and_exclude_600_ms_fastly() {
    navigation_bids_include_180_and_320_ms_and_exclude_600_ms(OriginRuntime::Fastly);
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn navigation_bids_include_180_and_320_ms_and_exclude_600_ms_axum() {
    navigation_bids_include_180_and_320_ms_and_exclude_600_ms(OriginRuntime::Axum);
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn navigation_bids_include_180_and_320_ms_and_exclude_600_ms_cloudflare() {
    navigation_bids_include_180_and_320_ms_and_exclude_600_ms(OriginRuntime::Cloudflare);
}

#[test]
#[ignore = "requires Spin and the current production Spin WASM artifact"]
fn navigation_bids_include_180_and_320_ms_and_exclude_600_ms_spin() {
    navigation_bids_include_180_and_320_ms_and_exclude_600_ms(OriginRuntime::Spin);
}

/// How far apart the streaming navigation origin writes its body chunks.
const STREAMED_CHUNK_INTERVAL: Duration = Duration::from_millis(100);

/// Body chunks the streaming navigation origin writes after its head.
const STREAMED_CHUNKS: usize = 10;

/// Marker each streamed body chunk carries.
const STREAMED_CHUNK: &str = "<p>example streamed chunk</p>";

/// The publisher origin of the streamed-body navigation case.
///
/// It answers [`NAVIGATION_PATH`] with its head and the start of the page at
/// once, then streams [`STREAMED_CHUNKS`] more chunks
/// [`STREAMED_CHUNK_INTERVAL`] apart, so the body ends about 1 s after the
/// request arrives. Any other path gets a small page at once.
fn start_streaming_navigation_origin() -> RecordingOrigin {
    let listener =
        TcpListener::bind(("127.0.0.1", origin_port())).expect("should bind the origin port");
    RecordingOrigin::serve_paced(
        listener,
        Arc::new(|request| {
            if request.path != NAVIGATION_PATH {
                return vec![OriginWrite {
                    delay: Duration::ZERO,
                    bytes: html_response(200, request.method == "HEAD"),
                }];
            }
            let chunk = |text: &str| format!("{:x}\r\n{text}\r\n", text.len()).into_bytes();
            let mut head = b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
            head.extend(chunk(
                "<html><head><title>Example</title></head><body><main>",
            ));
            let mut writes = vec![OriginWrite {
                delay: Duration::ZERO,
                bytes: head,
            }];
            for index in 1..=STREAMED_CHUNKS {
                let mut bytes = chunk(STREAMED_CHUNK);
                if index == STREAMED_CHUNKS {
                    bytes.extend(chunk("</main></body></html>"));
                    bytes.extend_from_slice(b"0\r\n\r\n");
                }
                writes.push(OriginWrite {
                    delay: STREAMED_CHUNK_INTERVAL,
                    bytes,
                });
            }
            writes
        }),
    )
}

/// Milliseconds from `start` to `instant`, negative when `instant` is earlier.
fn millis_from(start: Instant, instant: Instant) -> i128 {
    let millis = |duration: Duration| i128::try_from(duration.as_millis()).unwrap_or(i128::MAX);
    match instant.checked_duration_since(start) {
        Some(after) => millis(after),
        None => -millis(start.duration_since(instant)),
    }
}

/// A bid that answers while the origin body streams is injected on Viceroy
/// (EZ-6, spec 2.5 and test 6).
///
/// The origin sends its head at once and streams its body for about 1 s,
/// across the 500 ms auction. The loopback servers record when each request
/// head arrives and when each answer's last write starts. The origin request
/// arrives before the 200 ms bidder answers, and every bid request arrives
/// before the origin body ends, so the origin fetch and the bids overlap. The
/// 200 ms bid, which comes while the body still streams, is injected. The
/// 600 ms bid, which comes after the cutoff though the body still streams, is
/// dropped.
///
/// Viceroy supplies no JA4 fingerprint, so this navigation takes the plain
/// path ([`NAVIGATION_EC_ID`]). The overlapped path's order, origin request
/// before bid construction, is tested in core with the scripted client.
#[test]
#[ignore = "requires Viceroy and the release Fastly WASM"]
fn navigation_bid_answered_while_origin_body_streams_is_injected_fastly() {
    init_logger();
    let _port = lock_origin_port();
    let origin = start_streaming_navigation_origin();
    let delays = [200, 600].map(Duration::from_millis);
    let bidders = delays.map(start_navigation_bidder);
    let envelope = navigation_envelope(&delays, &bidders);

    OriginRuntime::Fastly.serve(&envelope, |base_url| {
        let html = load_navigation(OriginRuntime::Fastly, base_url);
        assert_eq!(
            html.matches(STREAMED_CHUNK).count(),
            STREAMED_CHUNKS,
            "should serve every chunk the origin streamed: {html}"
        );
        assert!(
            html.contains(&navigation_creative(delays[0])),
            "should inject the bid that answered at 200 ms, while the origin body streamed: {html}"
        );
        assert!(
            !html.contains(&navigation_creative(delays[1])),
            "should drop the bid that answered after the 500 ms cutoff: {html}"
        );
    });

    assert_bidders_received("Fastly", &bidders.iter().collect::<Vec<_>>(), 1);
    let page = origin
        .requests()
        .into_iter()
        .find(|request| request.path == NAVIGATION_PATH)
        .expect("should record the navigation's origin request");
    let body_end = page
        .answered_at
        .expect("should record when the origin body ended");
    let bid_requests = bidders.map(|bidder| bidder.requests().remove(0));
    let answers = bid_requests.each_ref().map(|bid_request| {
        bid_request
            .answered_at
            .expect("should record when the bidder answered")
    });
    for ((delay, bid_request), answered) in delays.iter().zip(&bid_requests).zip(answers) {
        log::info!(
            "timeline in ms from the origin request: {} ms bidder got its request at {}, answered at {}, origin body ended at {}",
            delay.as_millis(),
            millis_from(page.received_at, bid_request.received_at),
            millis_from(page.received_at, answered),
            millis_from(page.received_at, body_end),
        );
        assert!(
            bid_request.received_at < body_end,
            "should send the {} ms bid request while the origin body still streams: it came {} ms after the body ended",
            delay.as_millis(),
            millis_from(body_end, bid_request.received_at),
        );
        assert!(
            answered < body_end,
            "should get the {} ms bidder's answer while the origin body still streams: it came {} ms after the body ended",
            delay.as_millis(),
            millis_from(body_end, answered),
        );
    }
    assert!(
        page.received_at < answers[0],
        "should send the origin request while the 200 ms bid is in flight: it came {} ms after the bid",
        millis_from(answers[0], page.received_at),
    );
}

// ---------------------------------------------------------------------------
// Encoded passthrough on Viceroy (spec test 7, EZ-5 criterion 8)
// ---------------------------------------------------------------------------

/// Passes every chunk through unchanged, so a pipeline only re-encodes.
struct IdentityProcessor;

impl trusted_server_core::streaming_processor::StreamProcessor for IdentityProcessor {
    fn process_chunk(&mut self, chunk: &[u8], _is_last: bool) -> std::io::Result<Vec<u8>> {
        Ok(chunk.to_vec())
    }
}

/// Decodes `bytes` coded with `compression` through Trusted Server's own codec.
fn decode(bytes: &[u8], compression: Compression) -> Vec<u8> {
    let mut decoded = Vec::new();
    StreamingPipeline::new(
        PipelineConfig {
            input_compression: compression,
            output_compression: Compression::None,
            chunk_size: 8192,
        },
        IdentityProcessor,
    )
    .process(bytes, &mut decoded)
    .expect("should decode the body");
    decoded
}

/// A complete HTTP/1.1 response with `status`, `headers`, and `body`.
fn raw_response(status: u16, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let reason = http::StatusCode::from_u16(status)
        .ok()
        .and_then(|code| code.canonical_reason())
        .unwrap_or("Example");
    let mut response = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("Connection: close\r\n\r\n");
    let mut response = response.into_bytes();
    response.extend_from_slice(body);
    response
}

/// HTML the publisher origin serves for the encoded passthrough cases.
const PASSTHROUGH_HTML: &[u8] =
    b"<html><head><title>Example</title></head><body><a href=\"/next\">next</a></body></html>";

/// [`PASSTHROUGH_HTML`] as `gzip -n -9` codes it.
const PASSTHROUGH_HTML_GZIP: &[u8] = &[
    31, 139, 8, 0, 0, 0, 0, 0, 2, 3, 179, 201, 40, 201, 205, 177, 179, 201, 72, 77, 76, 177, 179,
    41, 201, 44, 201, 73, 181, 115, 173, 72, 204, 45, 200, 73, 181, 209, 135, 112, 109, 244, 33,
    146, 73, 249, 41, 149, 118, 54, 137, 10, 25, 69, 169, 105, 182, 74, 250, 121, 169, 21, 37, 74,
    118, 32, 210, 70, 63, 17, 168, 8, 34, 173, 15, 54, 14, 0, 15, 183, 7, 35, 85, 0, 0, 0,
];

/// [`PASSTHROUGH_HTML`] as the `brotli` command line tool codes it.
const PASSTHROUGH_HTML_BROTLI: &[u8] = &[
    161, 160, 2, 0, 224, 56, 176, 99, 52, 211, 83, 219, 181, 121, 205, 227, 195, 214, 54, 195, 208,
    232, 226, 197, 49, 14, 216, 239, 6, 154, 116, 31, 23, 222, 115, 88, 126, 55, 165, 56, 183, 208,
    95, 189, 160, 80, 215, 28, 134, 101, 217, 51, 196, 199, 81, 240, 80, 58,
];

/// The SVG the publisher origin serves br-coded.
const PASSTHROUGH_SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";

/// [`PASSTHROUGH_SVG`] as the `brotli` command line tool codes it.
const PASSTHROUGH_SVG_BROTLI: &[u8] = &[
    161, 64, 1, 192, 47, 122, 178, 212, 191, 206, 71, 69, 171, 214, 109, 14, 27, 112, 34, 148, 4,
    22, 143, 114, 199, 160, 75, 55, 49, 249, 186, 223, 115, 79, 163, 115, 36, 153, 82, 40, 72, 58,
    194, 7,
];

/// The bid response the gzip bidder serves.
const GZIP_BID_JSON: &[u8] = br#"{"seatbid":[{"seat":"gzip-seat","bid":[{"id":"gzip-bid","impid":"auction-slot","price":2.25,"adm":"<div>gzip bid</div>","w":300,"h":250}]}]}"#;

/// [`GZIP_BID_JSON`] as `gzip -n -9` codes it.
const GZIP_BID_JSON_GZIP: &[u8] = &[
    31, 139, 8, 0, 0, 0, 0, 0, 2, 3, 171, 86, 42, 78, 77, 44, 73, 202, 76, 81, 178, 138, 174, 6,
    179, 149, 172, 148, 210, 171, 50, 11, 116, 193, 108, 29, 37, 152, 20, 136, 130, 72, 128, 68,
    116, 148, 50, 115, 11, 192, 66, 137, 165, 201, 37, 153, 249, 121, 186, 197, 57, 249, 32, 229,
    5, 69, 153, 201, 169, 74, 86, 70, 122, 70, 166, 58, 74, 137, 41, 185, 64, 21, 54, 41, 153, 101,
    118, 32, 157, 10, 64, 157, 54, 250, 32, 30, 80, 97, 185, 146, 149, 177, 129, 129, 142, 82, 6,
    80, 177, 169, 65, 109, 44, 16, 2, 0, 181, 125, 165, 97, 140, 0, 0, 0,
];

/// The gzip-stored asset the range case slices: every byte value once, which
/// `gzip -n -9` stores in one uncoded deflate block, so it is 279 bytes long.
fn range_asset() -> Vec<u8> {
    let mut asset = vec![31, 139, 8, 0, 0, 0, 0, 0, 2, 3, 1, 0, 1, 255, 254];
    asset.extend(0..=u8::MAX);
    asset.extend([115, 140, 5, 41, 0, 1, 0, 0]);
    asset
}

/// The `Range` the range case requests.
const RANGE: &str = "bytes=0-99";

/// The length of the part [`RANGE`] selects.
const RANGE_LENGTH: usize = 100;

#[test]
fn encoded_fixtures_decode_to_their_plain_bodies() {
    assert_eq!(
        (
            decode(PASSTHROUGH_HTML_GZIP, Compression::Gzip),
            decode(PASSTHROUGH_HTML_BROTLI, Compression::Brotli),
            decode(PASSTHROUGH_SVG_BROTLI, Compression::Brotli),
            decode(GZIP_BID_JSON_GZIP, Compression::Gzip),
            decode(&range_asset(), Compression::Gzip),
        ),
        (
            PASSTHROUGH_HTML.to_vec(),
            PASSTHROUGH_HTML.to_vec(),
            PASSTHROUGH_SVG.to_vec(),
            GZIP_BID_JSON.to_vec(),
            (0..=u8::MAX).collect::<Vec<u8>>(),
        ),
        "should decode every precomputed fixture to its plain body"
    );
    assert!(
        range_asset().len() > RANGE_LENGTH,
        "should keep the range inside the stored asset"
    );
}

/// Answers the encoded passthrough paths of the publisher origin.
fn encoded_passthrough_response(request: &OriginRequest) -> Vec<u8> {
    match origin_request_path(request) {
        "/gzip-page" => raw_response(
            200,
            &[
                ("Content-Type", "text/html; charset=utf-8".to_owned()),
                ("Content-Encoding", "gzip".to_owned()),
                ("Content-Length", PASSTHROUGH_HTML_GZIP.len().to_string()),
            ],
            PASSTHROUGH_HTML_GZIP,
        ),
        "/image.svg" => raw_response(
            200,
            &[
                ("Content-Type", "image/svg+xml".to_owned()),
                ("Content-Encoding", "br".to_owned()),
                ("Content-Length", PASSTHROUGH_SVG_BROTLI.len().to_string()),
                ("ETag", "\"example-svg\"".to_owned()),
            ],
            PASSTHROUGH_SVG_BROTLI,
        ),
        "/asset.bin" => {
            let asset = range_asset();
            raw_response(
                206,
                &[
                    ("Content-Type", "application/octet-stream".to_owned()),
                    ("Content-Encoding", "gzip".to_owned()),
                    ("Content-Range", format!("bytes 0-99/{}", asset.len())),
                    ("Content-Length", RANGE_LENGTH.to_string()),
                ],
                &asset[..RANGE_LENGTH],
            )
        }
        "/app.js" if request.header("if-none-match") == Some("\"example-js\"") => {
            raw_response(304, &[("ETag", "\"example-js\"".to_owned())], b"")
        }
        "/creative.html" => raw_response(
            200,
            &[
                ("Content-Type", "text/html; charset=utf-8".to_owned()),
                ("Content-Encoding", "br".to_owned()),
                ("Content-Length", PASSTHROUGH_HTML_BROTLI.len().to_string()),
            ],
            PASSTHROUGH_HTML_BROTLI,
        ),
        _ => html_response(200, request.method == "HEAD"),
    }
}

/// Serves the shared integration app config on Viceroy, with the encoded
/// passthrough origin on the fixed origin port.
fn with_encoded_passthrough(check: impl FnOnce(&str, &RecordingOrigin)) {
    init_logger();
    let _port = lock_origin_port();
    let listener =
        TcpListener::bind(("127.0.0.1", origin_port())).expect("should bind the origin port");
    let origin = RecordingOrigin::serve(listener, Arc::new(encoded_passthrough_response));
    let process = IngressRuntime::Fastly.spawn();
    check(&process.base_url, &origin);
}

/// A client that neither decodes responses nor follows redirects.
fn raw_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .no_gzip()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("should build a raw client")
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn gzip_html_page_keeps_gzip_and_carries_tsjs_fastly() {
    with_encoded_passthrough(|base_url, _origin| {
        let response = raw_client()
            .get(format!("{base_url}/gzip-page"))
            .header("accept-encoding", "gzip")
            .send()
            .expect("should load the gzip page");
        assert_eq!(
            response.status().as_u16(),
            200,
            "should serve the gzip page"
        );
        assert_eq!(
            response
                .headers()
                .get("content-encoding")
                .and_then(|value| value.to_str().ok()),
            Some("gzip"),
            "should keep the gzip coding"
        );
        let html = String::from_utf8(decode(
            &response.bytes().expect("should read the gzip page"),
            trusted_server_core::streaming_processor::Compression::Gzip,
        ))
        .expect("should decode to UTF-8 HTML");
        assert!(
            html.contains("/static/tsjs="),
            "should decode to the rewritten document with the TSJS tag: {html}"
        );
    });
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn br_svg_relays_byte_identical_with_length_and_etag_fastly() {
    with_encoded_passthrough(|base_url, _origin| {
        let response = raw_client()
            .get(format!("{base_url}/image.svg"))
            .header("accept-encoding", "br")
            .send()
            .expect("should load the SVG");
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let head = (
            response.status().as_u16(),
            header("content-encoding"),
            header("content-length"),
            header("etag"),
        );
        assert_eq!(
            response.bytes().expect("should read the SVG").as_ref(),
            PASSTHROUGH_SVG_BROTLI,
            "should relay the br SVG byte for byte"
        );
        assert_eq!(
            head,
            (
                200,
                Some("br".to_owned()),
                Some(PASSTHROUGH_SVG_BROTLI.len().to_string()),
                Some("\"example-svg\"".to_owned())
            ),
            "should relay the br SVG with its origin length and ETag"
        );
    });
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn gzip_asset_range_returns_206_with_origin_content_range_fastly() {
    let asset = range_asset();
    with_encoded_passthrough(|base_url, origin| {
        let response = raw_client()
            .get(format!("{base_url}/asset.bin"))
            .header("range", RANGE)
            .header("accept-encoding", "gzip")
            .send()
            .expect("should request the range");
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let head = (
            response.status().as_u16(),
            header("content-range"),
            header("content-length"),
        );
        assert_eq!(
            response.bytes().expect("should read the range").as_ref(),
            &asset[..RANGE_LENGTH],
            "should relay the encoded range unchanged"
        );
        assert!(
            origin
                .requests()
                .iter()
                .any(|request| request.header("range") == Some(RANGE)),
            "should forward the Range header to the origin"
        );
        assert_eq!(
            head,
            (
                206,
                Some(format!("bytes 0-99/{}", asset.len())),
                Some(RANGE_LENGTH.to_string())
            ),
            "should return the origin range with its Content-Range"
        );
    });
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn js_asset_revalidation_returns_304_fastly() {
    with_encoded_passthrough(|base_url, _origin| {
        let response = raw_client()
            .get(format!("{base_url}/app.js"))
            .header("if-none-match", "\"example-js\"")
            .send()
            .expect("should revalidate the script");
        assert_eq!(
            response.status().as_u16(),
            304,
            "should return the origin's 304 for a matching validator"
        );
    });
}

#[test]
#[ignore = "requires Viceroy and the release Fastly WASM"]
fn first_party_proxy_html_creative_keeps_br_fastly() {
    init_logger();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("should bind the creative origin");
    let creative_origin = RecordingOrigin::serve(listener, Arc::new(encoded_passthrough_response));
    let envelope =
        common::config::proxy_allowlist_app_config_envelope(origin_port(), &["127.0.0.1"])
            .expect("should build the proxy allowlist app config");
    common::trace_boundary::with_trace_runtime(
        common::trace_boundary::TraceRuntime::Fastly,
        &envelope,
        None,
        |base_url| {
            let client = raw_client();
            let signed: serde_json::Value = client
                .get(format!("{base_url}/first-party/sign"))
                .query(&[(
                    "url",
                    format!("http://127.0.0.1:{}/creative.html", creative_origin.port),
                )])
                .send()
                .expect("should send the sign request")
                .error_for_status()
                .expect("should sign the creative URL")
                .json()
                .expect("should return the signed proxy URL");
            let href = signed["href"]
                .as_str()
                .expect("should return the first-party proxy href");
            let response = client
                .get(format!("{base_url}{}", signed_href_path_and_query(href)))
                .header("accept-encoding", "br")
                .send()
                .expect("should fetch the proxied creative");
            assert_eq!(
                response.status().as_u16(),
                200,
                "should serve the proxied creative"
            );
            assert_eq!(
                response
                    .headers()
                    .get("content-encoding")
                    .and_then(|value| value.to_str().ok()),
                Some("br"),
                "should keep the creative's br coding"
            );
        },
    );
}

#[test]
#[ignore = "requires Viceroy and the release Fastly WASM"]
fn gzip_json_bid_response_is_decoded_fastly() {
    init_logger();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("should bind the gzip bidder");
    let bidder = RecordingOrigin::serve(
        listener,
        Arc::new(|_request| {
            raw_response(
                200,
                &[
                    ("Content-Type", "application/json".to_owned()),
                    ("Content-Encoding", "gzip".to_owned()),
                    ("Content-Length", GZIP_BID_JSON_GZIP.len().to_string()),
                ],
                GZIP_BID_JSON_GZIP,
            )
        }),
    );
    let envelope = common::config::auction_app_config_envelope(
        origin_port(),
        &loopback_bidder_fixtures(&[("gzip", &bidder, Duration::from_millis(1500))]),
        2000,
    )
    .expect("should build the gzip bidder auction config");

    common::trace_boundary::with_trace_runtime(
        common::trace_boundary::TraceRuntime::Fastly,
        &envelope,
        None,
        |base_url| {
            let response = post_auction(base_url);
            assert_eq!(
                response["seatbid"][0]["bid"][0]["price"],
                serde_json::json!(2.25),
                "should decode the gzip bid response: {response}"
            );
        },
    );
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn origin_records_identity_accept_encoding_when_none_was_set_fastly() {
    with_encoded_passthrough(|base_url, origin| {
        let response = raw_client()
            .get(format!("{base_url}/plain"))
            .send()
            .expect("should load the page without Accept-Encoding");
        assert_eq!(response.status().as_u16(), 200, "should serve the page");
        let forwarded = origin
            .requests()
            .into_iter()
            .find(|request| origin_request_path(request) == "/plain")
            .expect("should forward the page request");
        assert_eq!(
            forwarded.header("accept-encoding"),
            Some("identity"),
            "should send identity when Trusted Server set no Accept-Encoding"
        );
    });
}

// ---------------------------------------------------------------------------
// Head limits fit realistic traffic (spec test 11)
// ---------------------------------------------------------------------------

/// Bytes of the creative URL the realistic first-party proxy case wraps.
const REALISTIC_CREATIVE_URL_BYTES: usize = 4096;

/// Request headers the realistic first-party proxy case sends, each of [`REALISTIC_HEADER_BYTES`].
const REALISTIC_HEADER_COUNT: usize = 40;

/// Bytes of each request header value the realistic first-party proxy case sends.
const REALISTIC_HEADER_BYTES: usize = 1024;

/// Fetches a signed first-party proxy URL that wraps a 4 KiB creative URL, with 40 headers of 1 KiB each.
///
/// `EdgeZero`'s default head limits must admit it, so it reaches `creative_origin`.
fn realistic_first_party_proxy_request_is_admitted(
    base_url: &str,
    creative_origin: &RecordingOrigin,
) {
    let client = raw_client();
    let prefix = format!("http://127.0.0.1:{}/creative?ctx=", creative_origin.port);
    let creative_url = format!(
        "{prefix}{}",
        "a".repeat(REALISTIC_CREATIVE_URL_BYTES - prefix.len())
    );
    let signed: serde_json::Value = client
        .get(format!("{base_url}/first-party/sign"))
        .query(&[("url", creative_url.as_str())])
        .send()
        .expect("should send the sign request")
        .error_for_status()
        .expect("should sign the creative URL")
        .json()
        .expect("should return the signed proxy URL");
    let href = signed["href"]
        .as_str()
        .expect("should return the first-party proxy href");
    let mut request = client.get(format!("{base_url}{}", signed_href_path_and_query(href)));
    for index in 0..REALISTIC_HEADER_COUNT {
        request = request.header(
            format!("x-example-{index:02}"),
            "b".repeat(REALISTIC_HEADER_BYTES),
        );
    }

    let response = request
        .send()
        .expect("should send the realistic first-party proxy request");

    assert_eq!(
        response.status().as_u16(),
        200,
        "should admit and serve the realistic first-party proxy request"
    );
    assert!(
        creative_origin
            .requests()
            .iter()
            .any(|forwarded| origin_request_path(forwarded) == "/creative"),
        "should reach the creative origin"
    );
}

/// Starts a creative origin and the proxy allowlist app config for the realistic case.
fn realistic_creative_origin() -> (RecordingOrigin, String) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("should bind the creative origin");
    let origin = RecordingOrigin::serve(
        listener,
        Arc::new(|request| html_response(200, request.method == "HEAD")),
    );
    let envelope =
        common::config::proxy_allowlist_app_config_envelope(origin_port(), &["127.0.0.1"])
            .expect("should build the proxy allowlist app config");
    (origin, envelope)
}

#[test]
#[ignore = "requires Viceroy and the release Fastly WASM"]
fn realistic_first_party_proxy_request_is_admitted_fastly() {
    init_logger();
    let (origin, envelope) = realistic_creative_origin();
    common::trace_boundary::with_trace_runtime(
        common::trace_boundary::TraceRuntime::Fastly,
        &envelope,
        None,
        |base_url| realistic_first_party_proxy_request_is_admitted(base_url, &origin),
    );
}

#[test]
#[ignore = "requires the `wrangler` CLI in $PATH and a prebuilt Cloudflare Workers bundle (run build.sh first); the test starts wrangler dev automatically"]
fn realistic_first_party_proxy_request_is_admitted_cloudflare() {
    init_logger();
    let (origin, envelope) = realistic_creative_origin();
    common::trace_boundary::with_trace_runtime(
        common::trace_boundary::TraceRuntime::Cloudflare,
        &envelope,
        None,
        |base_url| realistic_first_party_proxy_request_is_admitted(base_url, &origin),
    );
}

#[test]
#[ignore = "requires the pre-built trusted-server-axum binary"]
fn realistic_first_party_proxy_request_is_admitted_axum() {
    init_logger();
    let (origin, envelope) = realistic_creative_origin();
    let process = environments::axum::AxumDevServer
        .spawn_with_app_config(&envelope, &[])
        .expect("should start the Axum dev server");
    realistic_first_party_proxy_request_is_admitted(&process.base_url, &origin);
}

// ---------------------------------------------------------------------------
// Response write deadline (spec test 17)
// ---------------------------------------------------------------------------

/// How long the slow publisher origin streams its page.
const SLOW_PAGE_STREAM: Duration = Duration::from_secs(35);

/// Marker the slow publisher page ends with.
const SLOW_PAGE_END: &str = "<p>example slow page end</p>";

/// Serves one chunked HTML page on `listener` whose body takes [`SLOW_PAGE_STREAM`] to arrive.
fn serve_slow_streaming_page(listener: TcpListener) -> JoinHandle<()> {
    thread::spawn(move || {
        let Ok((mut stream, _addr)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let Some(_request) = read_origin_request(&mut stream) else {
            return;
        };
        let write_chunk = |stream: &mut TcpStream, chunk: &str| {
            stream
                .write_all(format!("{:x}\r\n{chunk}\r\n", chunk.len()).as_bytes())
                .and_then(|()| stream.flush())
        };
        if stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
            .is_err()
            || write_chunk(&mut stream, "<html><head><title>Example</title></head><body>").is_err()
        {
            return;
        }
        let started = std::time::Instant::now();
        while started.elapsed() < SLOW_PAGE_STREAM {
            thread::sleep(Duration::from_secs(1));
            if write_chunk(&mut stream, "<p>example slow chunk</p>").is_err() {
                return;
            }
        }
        let _ = write_chunk(&mut stream, &format!("{SLOW_PAGE_END}</body></html>"))
            .and_then(|()| stream.write_all(b"0\r\n\r\n"))
            .and_then(|()| stream.flush());
    })
}

#[test]
#[ignore = "requires Viceroy, the generated Viceroy config, and the release Fastly WASM"]
fn publisher_page_streaming_35_seconds_completes() {
    init_logger();
    let _port = lock_origin_port();
    let listener =
        TcpListener::bind(("127.0.0.1", origin_port())).expect("should bind the origin port");
    let origin = serve_slow_streaming_page(listener);
    let process = IngressRuntime::Fastly.spawn();
    let started = std::time::Instant::now();

    let response = reqwest::blocking::Client::builder()
        .no_gzip()
        .timeout(Duration::from_secs(90))
        .build()
        .expect("should build a client that waits for the slow page")
        .get(format!("{}/slow-page", process.base_url))
        .header("accept", "text/html")
        .header("accept-encoding", "identity")
        .send()
        .expect("should start the slow page");
    let status = response.status().as_u16();
    let body = response.text().expect("should read the whole slow page");

    assert_eq!(status, 200, "should serve the slow page");
    assert!(
        started.elapsed() >= SLOW_PAGE_STREAM,
        "should stream for the origin's full {SLOW_PAGE_STREAM:?}"
    );
    assert!(
        body.contains(SLOW_PAGE_END),
        "should deliver the page past EdgeZero's 30 s default write deadline"
    );
    drop(process);
    let _ = origin.join();
}
