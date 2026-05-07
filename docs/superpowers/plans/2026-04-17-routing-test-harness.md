# Routing Test Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add integration tests for multi-backend routing that verify domain routing, www-stripping, path routing, and fallback behaviour using real Viceroy + reqwest.

**Architecture:** A new `[[test]]` binary (`routing`) in `crates/integration-tests/` starts four in-process mock origin servers on fixed ports, spawns Viceroy with a routing-specific WASM (built with `ROUTING_TEST_BACKENDS=1`), then sends `reqwest` requests with per-test `Host` headers via `ClientBuilder::resolve()`. Build.rs in `trusted-server-core` reads `test-backends.toml` instead of `backends.toml` when the env var is set.

**Tech Stack:** Rust, reqwest (blocking), Viceroy, std TcpListener for mock servers.

---

### Task 1: Create test-backends.toml

**Files:**

- Create: `crates/trusted-server-adapter-fastly/test-backends.toml`

- [ ] **Step 1: Create the file**

```toml
# Test-only backend routing config.
# Embedded at compile time when ROUTING_TEST_BACKENDS=1 is set.
# Ports 19090-19093 are used by MockOrigin servers in tests/routing.rs.

[[backends]]
id = "site-a"
origin_url = "http://127.0.0.1:19091"
domains = ["site-a.test", "www.site-a.test"]

[[backends]]
id = "site-b"
origin_url = "http://127.0.0.1:19092"
domains = ["site-b.test"]

[[backends]]
id = "api"
origin_url = "http://127.0.0.1:19093"

  [[backends.path_patterns]]
  host = "*"
  path_prefix = "/.api/"
```

- [ ] **Step 2: Verify it parses as valid TOML**

```bash
cargo check --package trusted-server-core
```

Expected: no errors (build.rs will parse this format).

- [ ] **Step 3: Commit**

```bash
git add crates/trusted-server-adapter-fastly/test-backends.toml
git commit -m "Add test-backends.toml for routing integration tests"
```

---

### Task 2: Update build.rs to support ROUTING_TEST_BACKENDS

**Files:**

- Modify: `crates/trusted-server-core/build.rs`

- [ ] **Step 1: Add the test backends path constant and env var support**

In `build.rs`, add one new constant after `BACKENDS_CONFIG_PATH`:

```rust
const TEST_BACKENDS_CONFIG_PATH: &str =
    "../../crates/trusted-server-adapter-fastly/test-backends.toml";
```

- [ ] **Step 2: Add rerun-if-changed entries in `rerun_if_changed()`**

Add these two lines inside `rerun_if_changed()`, after the existing `rerun-if-changed` println for `BACKENDS_CONFIG_PATH`:

```rust
println!("cargo:rerun-if-changed={}", TEST_BACKENDS_CONFIG_PATH);
println!("cargo:rerun-if-env-changed=ROUTING_TEST_BACKENDS");
```

- [ ] **Step 3: Switch the backends path in `merge_toml()`**

Replace the existing line:

```rust
let backends_path = Path::new(BACKENDS_CONFIG_PATH);
```

With:

```rust
let backends_path = if std::env::var("ROUTING_TEST_BACKENDS").is_ok() {
    Path::new(TEST_BACKENDS_CONFIG_PATH)
} else {
    Path::new(BACKENDS_CONFIG_PATH)
};
```

- [ ] **Step 4: Verify compilation**

```bash
cargo check --package trusted-server-core
```

Expected: exits 0 with no errors.

- [ ] **Step 5: Verify the env var toggles the path**

```bash
ROUTING_TEST_BACKENDS=1 \
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:19090" \
    cargo check --package trusted-server-adapter-fastly
```

Expected: exits 0. If `test-backends.toml` is malformed this would fail here.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/build.rs
git commit -m "Support ROUTING_TEST_BACKENDS env var in build.rs to embed test backends"
```

---

### Task 3: Register the routing test binary

**Files:**

- Modify: `crates/integration-tests/Cargo.toml`

- [ ] **Step 1: Add the [[test]] entry**

Append after the existing `[[test]]` block in `crates/integration-tests/Cargo.toml`:

```toml
[[test]]
name = "routing"
path = "tests/routing.rs"
harness = true
```

- [ ] **Step 2: Commit**

```bash
git add crates/integration-tests/Cargo.toml
git commit -m "Register routing test binary in integration-tests Cargo.toml"
```

---

### Task 4: Write routing.rs

**Files:**

- Create: `crates/integration-tests/tests/routing.rs`

This file is a self-contained test binary. It reuses `mod common` and `mod environments` from the same `tests/` directory (same physical files, separate compilation unit).

- [ ] **Step 1: Create the file with the full implementation**

```rust
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

            let viceroy_addr: std::net::SocketAddr =
                format!("127.0.0.1:{viceroy_port}")
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
```

- [ ] **Step 2: Verify the test binary compiles**

```bash
cargo test --manifest-path crates/integration-tests/Cargo.toml --test routing --no-run
```

Expected: `Compiling integration-tests ...` then `Finished`. No errors.

- [ ] **Step 3: Verify tests pass trivially without ROUTING_WASM_PATH set**

```bash
cargo test --manifest-path crates/integration-tests/Cargo.toml --test routing
```

Expected: all 5 tests pass (early-return due to missing env var).

```
test domain_routes_to_site_a ... ok
test domain_routes_to_site_b ... ok
test path_routes_to_api ... ok
test unknown_host_falls_back_to_default ... ok
test www_prefix_stripped ... ok
```

- [ ] **Step 4: Commit**

```bash
git add crates/integration-tests/tests/routing.rs
git commit -m "Add routing integration tests with MockOrigin harness"
```

---

### Task 5: Build routing WASM and run tests for real

- [ ] **Step 1: Build the routing WASM**

```bash
ROUTING_TEST_BACKENDS=1 \
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:19090" \
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="integration-test-proxy-secret" \
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY="integration-test-secret-key" \
TRUSTED_SERVER__PROXY__CERTIFICATE_CHECK=false \
    cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
```

Expected: `Compiling trusted-server-core ...` (build.rs re-runs due to env var change), then `Finished`.

- [ ] **Step 2: Run the routing tests with ROUTING_WASM_PATH set**

```bash
TARGET="$(rustc -vV | sed -n 's/^host: //p')"

ROUTING_WASM_PATH="$(pwd)/target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm" \
RUST_LOG=info \
    cargo test \
        --manifest-path crates/integration-tests/Cargo.toml \
        --target "$TARGET" \
        --test routing \
        -- --test-threads=1
```

Expected: all 5 tests pass with non-trivial execution (Viceroy spawns, requests route correctly):

```
test domain_routes_to_site_a ... ok
test domain_routes_to_site_b ... ok
test path_routes_to_api ... ok
test unknown_host_falls_back_to_default ... ok
test www_prefix_stripped ... ok
```

If a test fails, check:

- Viceroy log output (`RUST_LOG=debug` for more detail)
- That the WASM was built with `ROUTING_TEST_BACKENDS=1` (not the cached regular build)
- That ports 19090-19093 are free: `lsof -i :19090 -i :19091 -i :19092 -i :19093`

---

### Task 6: Add routing step to integration-tests.sh

**Files:**

- Modify: `scripts/integration-tests.sh`

- [ ] **Step 1: Append the routing build and test steps**

At the end of `scripts/integration-tests.sh`, before the final blank line, add:

```bash
echo "==> Building routing WASM binary (test backends: ports 19090-19093)..."
ROUTING_TEST_BACKENDS=1 \
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:19090" \
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="integration-test-proxy-secret" \
TRUSTED_SERVER__SYNTHETIC__SECRET_KEY="integration-test-secret-key" \
TRUSTED_SERVER__PROXY__CERTIFICATE_CHECK=false \
    cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1

echo "==> Running routing integration tests..."
ROUTING_WASM_PATH="$REPO_ROOT/target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm" \
RUST_LOG=info \
    cargo test \
        --manifest-path crates/integration-tests/Cargo.toml \
        --target "$TARGET" \
        --test routing \
        -- --test-threads=1
```

- [ ] **Step 2: Run the full script to verify end-to-end**

```bash
./scripts/integration-tests.sh
```

Expected: existing tests pass, then routing tests pass. Total output ends with:

```
==> Running routing integration tests...
test domain_routes_to_site_a ... ok
test domain_routes_to_site_b ... ok
test path_routes_to_api ... ok
test unknown_host_falls_back_to_default ... ok
test www_prefix_stripped ... ok
```

- [ ] **Step 3: Commit**

```bash
git add scripts/integration-tests.sh
git commit -m "Add routing test build and run step to integration-tests.sh"
```

---

### Task 7: Final verification

- [ ] **Step 1: Run check-ci to verify nothing is broken**

```
/check-ci
```

Expected: all CI checks pass (fmt, clippy, cargo test --workspace, JS tests).

- [ ] **Step 2: Confirm routing tests still pass after check-ci rebuilds**

The regular `cargo test --workspace` step will compile the `routing` test binary without `ROUTING_WASM_PATH` set — all 5 tests should pass trivially (early-return). Confirm this in the output.
