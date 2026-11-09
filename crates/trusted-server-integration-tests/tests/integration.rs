mod common;
mod environments;
mod frameworks;

use common::ec::CookieVaryingOrigin;
use common::runtime::{
    RuntimeEnvironment, RuntimeProcess, TestError, origin_port, wasm_binary_path,
};
use environments::{RUNTIME_ENVIRONMENTS, ReadyCheckOptions, wait_for_http_ready};
use error_stack::ResultExt as _;
use frameworks::scenarios::EcScenario;
use frameworks::{FRAMEWORKS, FrontendFramework};
use reqwest::header::CONTENT_TYPE;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use testcontainers::runners::SyncRunner as _;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};

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
        .expect("EdgeZero entry-point canary failed: TRACE did not return a router-level 405");

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
// it runs and stays correct without `--test-threads=1`.

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
}

/// Builds the complete HTTP response a [`RecordingOrigin`] sends for one request.
type OriginResponder = Arc<dyn Fn(&OriginRequest) -> Vec<u8> + Send + Sync>;

/// Loopback origin that records every request and answers it from a responder.
///
/// Unlike [`common::ec::MinimalOrigin`], it reads the whole request body and
/// decodes chunked framing, so a case can compare what reached the origin
/// with what the client sent.
struct RecordingOrigin {
    port: u16,
    requests: Arc<Mutex<Vec<OriginRequest>>>,
    shutdown_tx: mpsc::Sender<()>,
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
        let port = listener
            .local_addr()
            .expect("should read the origin address")
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
                        let respond = Arc::clone(&respond);
                        // One thread per connection, so an idle pooled
                        // connection cannot hold up the next request.
                        thread::spawn(move || {
                            serve_origin_connection(stream, respond.as_ref(), &recorded);
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
            requests,
            shutdown_tx,
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
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Reads one request from `stream`, records it, and answers with what `respond` builds.
fn serve_origin_connection(
    mut stream: TcpStream,
    respond: &(dyn Fn(&OriginRequest) -> Vec<u8> + Send + Sync),
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
    let Some(request) = read_origin_request(&mut stream) else {
        return;
    };
    let response = respond(&request);
    recorded
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(request);

    let _ = stream.write_all(&response);
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
    let head = std::str::from_utf8(&received[..head_end]).ok()?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let mut content_length = 0;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().ok()?;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.eq_ignore_ascii_case("chunked");
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

    Some(OriginRequest { method, path, body })
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
        .get(format!("{base_url}{href}"))
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
        .spawn_with_app_config(&envelope)
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
