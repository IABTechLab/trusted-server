# SSAT Root Document 304 Prevention Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Guarantee that every auction-eligible SSAT navigation receives a complete, non-stored publisher document instead of a browser- or Fastly-generated 304.

**Architecture:** Add a default-off cache-bypass capability to the platform HTTP request and map it to Fastly pass mode. The publisher path enables it only for the existing `should_run_ad_stack` gate, strips browser validators before the origin fetch, removes response validators while setting `private, no-store`, and converts an unexpected eligible-origin 304 into a non-cacheable 502 with abandoned-auction telemetry.

**Tech Stack:** Rust 2024, `edgezero_core` HTTP types, Fastly Rust SDK 0.12.1, async traits, Viceroy tests, `error-stack`.

---

## File Map

| File                                                      | Responsibility                                                                                                                                    |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/platform/http.rs`         | Define the platform-neutral, default-off cache-bypass request option.                                                                             |
| `crates/trusted-server-core/src/platform/test_support.rs` | Record cache-bypass options in the shared stub HTTP client for publisher tests.                                                                   |
| `crates/trusted-server-adapter-fastly/src/platform.rs`    | Translate the platform option to Fastly `Request::set_pass(true)` in both send paths.                                                             |
| `crates/trusted-server-core/src/publisher.rs`             | Apply the eligibility gate, strip validators, set the synthesized response policy, fail closed on unexpected 304, and test the complete behavior. |

No configuration schema, JavaScript, `/page-bids`, auction-ID, asset, or integration files change.

### Task 1: Add Platform Cache-Bypass Metadata and Test Recording

**Files:**

- Modify: `crates/trusted-server-core/src/platform/http.rs`
- Modify: `crates/trusted-server-core/src/platform/test_support.rs`

- [ ] **Step 1: Write failing constructor and builder tests**

Add these tests to `platform::http::tests`:

```rust
#[test]
fn platform_http_request_cache_bypass_defaults_to_false() {
    let request = edgezero_core::http::request_builder()
        .uri("https://example.com/")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let request = PlatformHttpRequest::new(request, "origin");

    assert!(
        !request.bypass_cache,
        "ordinary platform requests should retain normal cache behavior"
    );
}

#[test]
fn platform_http_request_cache_bypass_builder_enables_bypass() {
    let request = edgezero_core::http::request_builder()
        .uri("https://example.com/")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let request = PlatformHttpRequest::new(request, "origin").with_cache_bypass();

    assert!(
        request.bypass_cache,
        "cache-bypass builder should enable platform cache bypass"
    );
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run:

```bash
cargo test-fastly platform_http_request_cache_bypass -- --nocapture
```

Expected: compilation fails because `bypass_cache` and `with_cache_bypass` do not exist.

- [ ] **Step 3: Add the minimal platform request option**

Add a documented public field and builder to `PlatformHttpRequest`:

```rust
/// Whether the platform's intermediary response cache must be bypassed.
///
/// Adapters without an intermediary outbound cache may treat this as already
/// satisfied. The option defaults to `false` so existing call sites preserve
/// their current cache behavior.
pub bypass_cache: bool,
```

Initialize it to `false` in `new`, then add:

```rust
/// Bypass the platform's intermediary response cache for this request.
#[must_use]
pub fn with_cache_bypass(mut self) -> Self {
    self.bypass_cache = true;
    self
}
```

- [ ] **Step 4: Extend the shared HTTP stub**

Add `cache_bypass_flags: Mutex<Vec<bool>>` to `StubHttpClient`, initialize it,
record `request.bypass_cache` in both `send` and `send_async`, and expose:

```rust
pub fn recorded_cache_bypass_flags(&self) -> Vec<bool> {
    self.cache_bypass_flags
        .lock()
        .expect("should lock cache bypass flags")
        .clone()
}
```

Record the flag before consuming `request.request`.

- [ ] **Step 5: Run targeted platform tests**

Run:

```bash
cargo test-fastly platform_http_request_cache_bypass -- --nocapture
cargo test-fastly platform::test_support -- --nocapture
```

Expected: both commands pass.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/platform/http.rs crates/trusted-server-core/src/platform/test_support.rs
git commit -m "Add platform HTTP cache bypass option"
```

### Task 2: Map Cache Bypass to Fastly Pass Mode

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`

- [ ] **Step 1: Write failing Fastly cache-override tests**

Introduce a private helper named `apply_fastly_cache_bypass` and add tests that
construct a `fastly::Request`, invoke the helper, and inspect the SDK's derived
debug representation:

```rust
#[test]
fn apply_fastly_cache_bypass_sets_pass_when_enabled() {
    let mut request = fastly::Request::get("https://example.com/");

    apply_fastly_cache_bypass(&mut request, true);

    assert!(
        format!("{request:?}").contains("cache_override: Pass"),
        "enabled bypass should select Fastly pass mode"
    );
}

#[test]
fn apply_fastly_cache_bypass_preserves_default_when_disabled() {
    let mut request = fastly::Request::get("https://example.com/");

    apply_fastly_cache_bypass(&mut request, false);

    assert!(
        format!("{request:?}").contains("cache_override: None"),
        "disabled bypass should preserve Fastly read-through caching"
    );
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run:

```bash
cargo test-fastly apply_fastly_cache_bypass -- --nocapture
```

Expected: compilation fails because the helper does not exist.

- [ ] **Step 3: Implement and use the Fastly mapping**

Add:

```rust
fn apply_fastly_cache_bypass(request: &mut fastly::Request, bypass_cache: bool) {
    if bypass_cache {
        request.set_pass(true);
    }
}
```

In `FastlyPlatformHttpClient::send`, copy `request.bypass_cache` before moving
the inner request, make the converted Fastly request mutable, and invoke the
helper before `.send()`.

Do the same in `send_async` before `.send_async()`. Preserve the existing Image
Optimizer and streaming-response rejection behavior.

- [ ] **Step 4: Run targeted Fastly adapter tests**

Run:

```bash
cargo test-fastly apply_fastly_cache_bypass -- --nocapture
cargo test-fastly fastly_platform_http_client -- --nocapture
```

Expected: all matching tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Bypass Fastly cache for marked HTTP requests"
```

### Task 3: Protect Eligible Publisher Requests and Successful HTML Responses

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add focused eligible- and ineligible-request test helpers**

Add a `ssat_cache_policy_tests` module beside the existing handler-level test
modules. Reuse `StubHttpClient`, `StubBackend`, no-op services, a
non-regulated `EcContext`, a slot matching `/article`, and an enabled
orchestrator. A launch-failing provider is sufficient for these policy tests:
eligibility depends on the configured gate, not the dispatch outcome.

The eligible request must be:

```rust
HttpRequest::builder()
    .method(Method::GET)
    .uri("https://ts.example.com/article")
    .header(header::HOST, "ts.example.com")
    .header("sec-fetch-dest", "document")
    .header(header::IF_NONE_MATCH, "\"origin-tag\"")
    .header(header::IF_MODIFIED_SINCE, "Wed, 21 Oct 2015 07:28:00 GMT")
    .body(EdgeBody::empty())
    .expect("should build eligible navigation")
```

Queue an origin 200 with `Content-Type: text/html`, `Cache-Control: public,
max-age=300`, `ETag`, `Last-Modified`, `Surrogate-Control`, and
`Fastly-Surrogate-Control`.

- [ ] **Step 2: Write the failing eligible-request test**

Drive `handle_publisher_request` and assert:

```rust
assert_eq!(stub.recorded_cache_bypass_flags(), vec![true]);
let origin_headers = stub
    .recorded_request_headers()
    .into_iter()
    .last()
    .expect("should record publisher request headers");
assert!(!origin_headers.iter().any(|(name, _)| name == "if-none-match"));
assert!(!origin_headers.iter().any(|(name, _)| name == "if-modified-since"));
```

Extract the response headers from the returned `PublisherResponse` and assert:

```rust
assert_eq!(
    response.headers().get(header::CACHE_CONTROL),
    Some(&HeaderValue::from_static("private, no-store"))
);
for name in [
    header::ETAG.as_str(),
    header::LAST_MODIFIED.as_str(),
    "surrogate-control",
    "fastly-surrogate-control",
] {
    assert!(response.headers().get(name).is_none(), "{name} should be removed");
}
```

- [ ] **Step 3: Write the failing noneligible-request test**

Use the existing `run_publisher_proxy` helper with no slots and the same
conditional headers. Queue a normal response and assert:

```rust
assert_eq!(stub.recorded_cache_bypass_flags(), vec![false]);
assert!(origin_headers.iter().any(|(name, _)| name == "if-none-match"));
assert!(origin_headers.iter().any(|(name, _)| name == "if-modified-since"));
```

Also assert the origin's cache policy and validators remain unchanged. This is
the regression guard for HEAD, bots, prefetches, no-slot pages, and every other
request that fails the existing gate.

- [ ] **Step 4: Run the publisher policy tests and verify they fail**

Run:

```bash
cargo test-fastly ssat_cache_policy_tests -- --nocapture
```

Expected: the eligible assertions fail because validators are forwarded,
bypass is false, the response uses `private, max-age=0`, and validators remain.

- [ ] **Step 5: Implement request protection**

Immediately after auction dispatch and before URI/Host rewriting, add:

```rust
if should_run_ad_stack {
    req.headers_mut().remove(header::IF_NONE_MATCH);
    req.headers_mut().remove(header::IF_MODIFIED_SINCE);
}
```

Build the publisher request once, conditionally apply the builder, and send it:

```rust
let platform_request = PlatformHttpRequest::new(req, backend_name);
let platform_request = if should_run_ad_stack {
    platform_request.with_cache_bypass()
} else {
    platform_request
};
```

- [ ] **Step 6: Implement successful HTML response protection**

Within the existing `should_run_ad_stack && is_html_content_type(...)` branch:

```rust
response.headers_mut().insert(
    header::CACHE_CONTROL,
    HeaderValue::from_static("private, no-store"),
);
response.headers_mut().remove(header::ETAG);
response.headers_mut().remove(header::LAST_MODIFIED);
response.headers_mut().remove("surrogate-control");
response.headers_mut().remove("fastly-surrogate-control");
```

Update the adjacent rationale: synthesized, per-navigation auction state must
not be stored or validated as though it were the origin representation.

- [ ] **Step 7: Run targeted publisher tests**

Run:

```bash
cargo test-fastly ssat_cache_policy_tests -- --nocapture
cargo test-fastly publisher_request_uses_platform_http_client_with_http_types -- --nocapture
cargo test-fastly response_carries_body_preserves_bodiless_metadata -- --nocapture
```

Expected: all commands pass.

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Prevent SSAT publisher document revalidation"
```

### Task 4: Fail Closed on an Unexpected Eligible-Origin 304

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add a dispatching test provider**

Within `ssat_cache_policy_tests`, add a provider whose `request_bids` sends one
request through `context.services.http_client().send_async(...)`. Give it a
stable backend name and make `parse_response` panic because an unexpected 304
must abandon, not collect, the pending request.

Queue responses in this order because `StubHttpClient` consumes the provider
response during `send_async` before the publisher response during `send`:

```rust
stub.push_response(200, b"unused provider response".to_vec());
stub.push_response_with_headers(
    304,
    Vec::new(),
    vec![("etag", "\"origin-tag\"")],
);
```

- [ ] **Step 2: Write the failing unexpected-304 test**

Drive an eligible navigation with the dispatching provider and recording
telemetry sink. Assert the returned variant is `PublisherResponse::Buffered`
with:

```rust
assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
assert_eq!(
    response.headers().get(header::CACHE_CONTROL),
    Some(&HeaderValue::from_static("private, no-store"))
);
assert!(response.headers().get(header::ETAG).is_none());
assert!(response.headers().get(header::LAST_MODIFIED).is_none());
assert!(response.headers().get("surrogate-control").is_none());
assert!(response.headers().get("fastly-surrogate-control").is_none());
```

Flatten telemetry rows and assert exactly one summary row has
`terminal_status == Some("abandoned")` and
`terminal_reason == Some("unexpected_origin_304")`. Assert no provider parse or
auction collection occurred.

Cover both a typical 304 without `Content-Type` and a 304 carrying
`Content-Type: text/html` using a small table/helper so response classification
cannot affect the guard.

- [ ] **Step 3: Verify the test fails**

Run:

```bash
cargo test-fastly unexpected_origin_304 -- --nocapture
```

Expected: the handler returns 304 and no `unexpected_origin_304` telemetry.

- [ ] **Step 4: Add a noneligible-304 regression test**

Use `run_publisher_proxy` with no slots, queue a 304 carrying `ETag`,
`Last-Modified`, and origin cache headers, and assert:

```rust
let response = match run_publisher_proxy(&settings, &services, request).await {
    PublisherResponse::Buffered(response) => response,
    _ => panic!("noneligible 304 should remain a buffered response"),
};
assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
assert_eq!(response.headers().get(header::ETAG), Some(&origin_etag));
assert_eq!(
    response.headers().get(header::LAST_MODIFIED),
    Some(&origin_last_modified)
);
```

Also assert the request used `bypass_cache == false` and preserved its incoming
conditional headers. This proves the 304-to-502 guard is eligibility-scoped
rather than global.

- [ ] **Step 5: Implement the fail-closed guard before content classification**

Immediately after receiving/logging the publisher response and before reading
its content type:

```rust
if should_run_ad_stack && response.status() == StatusCode::NOT_MODIFIED {
    if let Some(dispatched) = dispatched_auction.take() {
        emit_abandoned_auction(
            services,
            auction_observation.take(),
            dispatched,
            "unexpected_origin_304",
        )
        .await;
    }

    let response = Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(EdgeBody::from("Publisher origin returned an invalid conditional response"))
        .change_context(TrustedServerError::Proxy {
            message: "failed to build unexpected origin 304 response".to_string(),
        })?;
    return Ok(PublisherResponse::Buffered(response));
}
```

Because the response is built from a fresh builder, it contains no origin
validators or surrogate cache headers and still goes through the adapter's
normal finalization after the publisher handler returns.

- [ ] **Step 6: Run the unexpected-304 and generic bodiless tests**

Run:

```bash
cargo test-fastly ssat_cache_policy_tests -- --nocapture
cargo test-fastly response_carries_body_preserves_bodiless_metadata -- --nocapture
cargo test-fastly serve_static -- --nocapture
```

Expected: eligible publisher 304 tests return 502; generic publisher/static
conditional semantics remain passing.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Reject unexpected SSAT origin 304 responses"
```

### Task 5: Full Verification and Scope Audit

**Files:**

- Verify only; modify production files only if a verification failure exposes a defect in the approved scope.

- [ ] **Step 1: Format and inspect the diff**

Run:

```bash
cargo fmt --all
git diff --check origin/main...HEAD
git diff --stat origin/main...HEAD
git status --short
```

Expected: formatting succeeds; no whitespace errors; only the spec, plan, two
core platform files, publisher, and Fastly platform adapter are changed. Local
`fastly.toml` remains untouched.

- [ ] **Step 2: Run all target-specific test suites**

Run:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
./scripts/test-cli.sh
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: all tests pass.

- [ ] **Step 3: Run JavaScript and documentation gates**

Run from `crates/trusted-server-js/lib`:

```bash
npx vitest run
npm run format
```

Then run from `docs`:

```bash
npm run format
```

Expected: JavaScript tests pass and both format commands complete without
errors. Inspect `git status --short` afterward; formatting must not introduce
unrelated content changes.

- [ ] **Step 4: Run formatting and target-specific lints/checks**

Run:

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo check-fastly
cargo check-axum
cargo check-cloudflare
cargo check-spin
```

Expected: all checks pass with no warnings promoted to errors.

- [ ] **Step 5: Audit constructors and behavior boundaries**

Run:

```bash
rg -n "with_cache_bypass|bypass_cache|set_pass" crates
rg -n "If-None-Match|If-Modified-Since|private, no-store|unexpected_origin_304" crates/trusted-server-core/src/publisher.rs
git diff origin/main...HEAD -- fastly.toml crates/trusted-server-js
```

Expected:

- `with_cache_bypass` is used only by the eligible publisher fetch.
- Fastly honors the option in both send paths.
- all other request constructors default to false;
- `fastly.toml` and JavaScript have no branch diff.

- [ ] **Step 6: Commit any formatting-only changes if needed**

```bash
git add crates/trusted-server-core/src/platform/http.rs \
  crates/trusted-server-core/src/platform/test_support.rs \
  crates/trusted-server-core/src/publisher.rs \
  crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Format SSAT 304 prevention changes"
```

Skip this commit when `cargo fmt --all` produces no new diff.

- [ ] **Step 7: Request final code review**

Run the repository's code-review workflow against `origin/main...HEAD`. Resolve
only correctness, security, test, or approved-scope findings, then repeat the
affected verification commands before reporting completion.
