#![allow(dead_code)]

mod common;
mod environments;

use common::runtime::RuntimeEnvironment as _;
use environments::fastly::FastlyViceroy;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;

/// In-process HTTP server that returns a fixed response body.
///
/// Listens on a fixed port. Accepts connections in a background thread,
/// drains each request, and responds with `HTTP/1.1 200 OK` and the
/// configured body. Stopped on [`Drop`] via a shutdown flag + self-connect.
///
/// Does not store the `JoinHandle` so that `MockOrigin` remains `Sync`
/// (required for placement in a `static OnceLock`). The thread exits
/// naturally when the process ends.
struct MockOrigin {
    port: u16,
    shutdown: Arc<AtomicBool>,
}

impl MockOrigin {
    /// Start a mock origin server on `port` that always responds with `body`.
    ///
    /// # Panics
    ///
    /// Panics if the port cannot be bound.
    fn start(port: u16, body: &'static str) -> Self {
        let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
            .unwrap_or_else(|e| panic!("should bind MockOrigin to port {port}: {e}"));

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        thread::spawn(move || {
            for stream in listener.incoming() {
                if shutdown_clone.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(stream) = stream {
                    serve(stream, body);
                }
            }
        });

        MockOrigin { port, shutdown }
    }
}

impl Drop for MockOrigin {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Unblock the accept() call so the thread can observe the shutdown flag.
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
    }
}

/// Write a minimal HTTP/1.1 200 response with `body` to `stream`.
///
/// Drains the incoming request first so the client does not see a broken pipe.
fn serve(mut stream: TcpStream, body: &'static str) {
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    let _ = stream.write_all(response.as_bytes());
}

/// Shared test state: mock origins + Viceroy process + pre-configured reqwest client.
///
/// Initialised once via [`get_harness`]. All five test functions share this
/// single instance to avoid the cost of spinning up Viceroy per test.
struct RoutingHarness {
    _origins: Vec<MockOrigin>,
    _process: common::runtime::RuntimeProcess,
    /// Client with resolve overrides so `http://site-a.test/` connects to Viceroy
    /// while sending the correct `Host` header.
    client: reqwest::blocking::Client,
}

static HARNESS: OnceLock<Option<RoutingHarness>> = OnceLock::new();

/// Return the shared harness, or `None` if `ROUTING_WASM_PATH` is not set.
///
/// Returns `None` rather than panicking so that tests pass trivially when
/// invoked outside the routing-specific CI step (e.g. `cargo test --workspace`).
fn get_harness() -> Option<&'static RoutingHarness> {
    HARNESS
        .get_or_init(|| {
            let wasm_path = std::env::var("ROUTING_WASM_PATH").ok()?;

            let origins = vec![
                MockOrigin::start(19090, "default"),
                MockOrigin::start(19091, "site-a"),
                MockOrigin::start(19092, "site-b"),
                MockOrigin::start(19093, "api"),
            ];

            let process = FastlyViceroy
                .spawn(std::path::Path::new(&wasm_path))
                .expect("should spawn Viceroy with routing WASM");

            let viceroy_port: u16 = process
                .base_url
                .trim_start_matches("http://127.0.0.1:")
                .parse()
                .expect("should parse Viceroy port from base_url");

            let viceroy_addr: std::net::SocketAddr = format!("127.0.0.1:{viceroy_port}")
                .parse()
                .expect("should parse Viceroy socket addr");

            let client = reqwest::blocking::ClientBuilder::new()
                .resolve("site-a.test", viceroy_addr)
                .resolve("www.site-a.test", viceroy_addr)
                .resolve("site-b.test", viceroy_addr)
                .resolve("any.test", viceroy_addr)
                .resolve("unknown.test", viceroy_addr)
                .build()
                .expect("should build reqwest client");

            Some(RoutingHarness {
                _origins: origins,
                _process: process,
                client,
            })
        })
        .as_ref()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn domain_routes_to_site_a() {
    let Some(h) = get_harness() else { return };

    let body = h
        .client
        .get("http://site-a.test/")
        .send()
        .expect("should send request to site-a.test")
        .text()
        .expect("should read response body");

    assert_eq!(body, "site-a", "should route site-a.test to the site-a backend");
}

#[test]
fn domain_routes_to_site_b() {
    let Some(h) = get_harness() else { return };

    let body = h
        .client
        .get("http://site-b.test/")
        .send()
        .expect("should send request to site-b.test")
        .text()
        .expect("should read response body");

    assert_eq!(body, "site-b", "should route site-b.test to the site-b backend");
}

#[test]
fn www_prefix_stripped() {
    let Some(h) = get_harness() else { return };

    let body = h
        .client
        .get("http://www.site-a.test/")
        .send()
        .expect("should send request to www.site-a.test")
        .text()
        .expect("should read response body");

    assert_eq!(
        body, "site-a",
        "should strip www. prefix and route to the site-a backend"
    );
}

#[test]
fn path_routes_to_api() {
    let Some(h) = get_harness() else { return };

    // any.test has no domain entry — path pattern matching fires instead.
    let body = h
        .client
        .get("http://any.test/.api/users")
        .send()
        .expect("should send request to any.test/.api/users")
        .text()
        .expect("should read response body");

    assert_eq!(
        body, "api",
        "should route /.api/ path prefix to the api backend"
    );
}

#[test]
fn unknown_host_falls_back_to_default() {
    let Some(h) = get_harness() else { return };

    let body = h
        .client
        .get("http://unknown.test/")
        .send()
        .expect("should send request to unknown.test")
        .text()
        .expect("should read response body");

    assert_eq!(
        body, "default",
        "should fall back to publisher.origin_url for unmatched hosts"
    );
}
