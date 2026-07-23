# True Origin Streaming Fastly Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix issue #849 for the production Fastly path so publisher HTML origin bodies stream through the rewrite pipeline to the client, with auction collection outside TTFB except for the held `</body>` tail.

**Architecture:** Keep one cohesive Fastly PR because the core pipeline, Fastly origin fetch, and Fastly finalize path are incomplete in isolation. Convert publisher body processing from sync `Read` over buffered bodies to async chunk-pull over `edgezero_core::body::Body`, then enable Fastly `with_stream_response()` and return a lazy streaming body for publisher responses. Leave Cloudflare, Spin, and Axum streaming as follow-up work.

**Tech Stack:** Rust 2024, `edgezero_core::body::Body`, `futures::StreamExt`, `error-stack`, `flate2`, `brotli`, Fastly Compute, Viceroy tests.

---

## Scope

In scope:

- Publisher HTML and processable publisher responses on the Fastly adapter.
- Core publisher pipeline support for `Body::Stream`.
- Fastly platform capability signaling for streaming origin responses.
- Fastly EdgeZero response delivery that streams publisher bodies to clients.
- Tests proving stream-vs-buffer parity, bodiless handling, stream caps, and Fastly routing behavior.

Out of scope:

- HTML post-processor configs (the `nextjs` integration). When any
  `IntegrationHtmlPostProcessor` is registered, `HtmlWithPostProcessing`
  accumulates the full rewritten document and runs post-processors at origin
  EOF, so the lazy stream emits no body bytes until the whole origin transfer
  completes — even for pages where `should_process()` would return `false`.
  Headers still commit early, but first byte/FCP tracks origin EOF for that
  configuration; #849's objective is unmet there. The eventual fix is an
  up-front or streaming `should_process` gate so non-RSC pages skip
  accumulation entirely (follow-up issue).
- Cloudflare origin streaming. Current adapter rejects `PlatformHttpRequest::stream_response`.
- Spin streaming. Current adapter and upstream EdgeZero Spin conversion are buffered/blocking issues.
- Axum client streaming. Axum is dev-only and has `LocalBoxStream`/`Send` constraints.
- Parser-context `</body>` scan fix from issue #850.
- Origin template caching and transformed HTML caching from issue #852.

## Current Failure Points

- `crates/trusted-server-adapter-fastly/src/platform.rs`: `fastly_response_to_platform(..., stream_response: false)` uses `take_body_bytes()` and the 10 MiB platform cap for publisher origin responses.
- `crates/trusted-server-core/src/publisher.rs`: `body_as_reader()` calls `body.into_bytes().unwrap_or_default()`, so `Body::Stream` becomes an empty body.
- `crates/trusted-server-core/src/publisher.rs`: `stream_html_with_auction_hold()` and `body_close_hold_loop()` are sync-`Read` based.
- `crates/trusted-server-adapter-fastly/src/app.rs`: publisher route calls `buffer_publisher_response_async()`, buffering all processed output and awaiting auction before any client bytes are sent.
- `crates/trusted-server-adapter-fastly/src/main.rs`: `send_edgezero_response()` already streams `EdgeBody::Stream`, but currently only asset responses reach that arm.

## File Structure

- Modify `crates/trusted-server-core/src/platform/http.rs`
  - Add `PlatformHttpClient::supports_streaming_responses()` with default `false`.
- Modify adapter platform implementations:
  - `crates/trusted-server-adapter-fastly/src/platform.rs`: return `true` for `supports_streaming_responses()`.
  - `crates/trusted-server-adapter-cloudflare/src/platform.rs`: inherit default `false`.
  - `crates/trusted-server-adapter-spin/src/platform.rs`: inherit default `false`.
  - `crates/trusted-server-adapter-axum/src/platform.rs`: inherit default `false`.
  - `crates/trusted-server-core/src/platform/test_support.rs`: configurable test support if needed.
- Modify `crates/trusted-server-core/src/streaming_processor.rs`
  - Add small push decoder/encoder helpers only if keeping them here reduces duplication.
  - Keep the existing `StreamingPipeline::process(Read, Write)` API for existing call sites.
- Modify `crates/trusted-server-core/src/publisher.rs`
  - Replace publisher async processing internals with async chunk-pull.
  - Keep public `buffer_publisher_response_async()` for buffered adapters.
  - Add a streaming response constructor/helper for Fastly to use.
  - Make `body_as_reader()` reject `Body::Stream` loudly or remove its use from any stream-capable path.
- Modify `crates/trusted-server-adapter-fastly/src/app.rs`
  - Replace publisher `buffer_publisher_response_async()` call with streaming finalize for streamable publisher responses.
  - Preserve buffered behavior for `PublisherResponse::Buffered`, pass-through/bodiless responses, and error paths.
- Modify `crates/trusted-server-adapter-fastly/src/main.rs`
  - Reuse existing `EdgeBody::Stream` delivery.
  - If publisher streaming needs a different log message from asset streaming, split the helper name/log text without changing behavior.
- Tests:
  - `crates/trusted-server-core/src/publisher.rs` unit tests.
  - `crates/trusted-server-core/src/streaming_processor.rs` unit tests if push codec helpers are introduced there.
  - `crates/trusted-server-core/src/platform/test_support.rs` tests for capability behavior.
  - `crates/trusted-server-adapter-fastly/src/app.rs` route tests for publisher streaming response shape.

## Design Decisions

- Use one PR for the full Fastly production fix. Intermediate merged PRs would create incomplete behavior and review confusion.
- Use existing `publisher.max_buffered_body_bytes` as the publisher body ceiling after streaming. `settings.rs` already documents that this becomes the sole ceiling after true streaming removes the 10 MiB Fastly materialization cap.
- Keep `Content-Length` removed for rewritten stream responses. Streaming output can change size due to URL rewriting and bid injection.
- Preserve bodiless behavior for `HEAD`, `204`, and `304`: do not attach or drive a body stream, and log abandoned/wasted auctions as current code does.
- Do not build a sync `Read` bridge over `Body::Stream`; nested `block_on` can panic on Fastly because the router already runs under `futures::executor::block_on`.
- Avoid adding `async-stream` initially. Use `futures::stream::unfold` or a custom stream type so the PR does not add a dependency unless the implementation becomes materially clearer.

## Task 1: Baseline Tests for Stream Input Safety

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add a failing test for `Body::Stream` not becoming empty**

Add a test near the existing `stream_publisher_body` tests:

```rust
#[test]
fn stream_publisher_body_rejects_stream_body_in_sync_path() {
    let settings = create_test_settings();
    let registry = IntegrationRegistry::new(&settings).expect("should build registry");
    let body = EdgeBody::from_stream(futures::stream::iter(vec![Ok(Bytes::from_static(
        b"<html><body>live</body></html>",
    ))]));
    let params = test_process_params("text/html", "");
    let mut output = Vec::new();

    let err = stream_publisher_body(body, &mut output, &params, &settings, &registry)
        .expect_err("should reject stream body in sync path");

    assert!(
        format!("{err:?}").contains("streaming body"),
        "should explain that Body::Stream is not supported by the sync path: {err:?}"
    );
}
```

- [ ] **Step 2: Run the targeted test and verify it fails**

Run:

```bash
cargo test-axum stream_publisher_body_rejects_stream_body_in_sync_path
```

Expected: FAIL because current `body_as_reader()` silently returns empty bytes.

- [ ] **Step 3: Replace `body_as_reader()` with a fallible helper**

Change `body_as_reader(body: EdgeBody) -> Cursor<Bytes>` to return `Result<Cursor<Bytes>, Report<TrustedServerError>>` and return a proxy error for `Body::Stream`.

Minimal shape:

```rust
fn body_as_reader(body: EdgeBody) -> Result<std::io::Cursor<bytes::Bytes>, Report<TrustedServerError>> {
    let bytes = body.into_bytes().ok_or_else(|| {
        Report::new(TrustedServerError::Proxy {
            message: "streaming body cannot be processed by sync publisher pipeline".to_owned(),
        })
    })?;
    Ok(std::io::Cursor::new(bytes))
}
```

Update existing sync call sites to use `body_as_reader(body)?`.

- [ ] **Step 4: Run the targeted test and existing publisher sync tests**

Run:

```bash
cargo test-axum stream_publisher_body
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Reject publisher stream bodies on sync path"
```

## Task 2: Async Chunk Source and Cumulative Cap

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add tests for async chunk pulling**

Add focused tests for a private helper that will pull chunks from both body variants:

```rust
#[test]
fn body_chunk_source_yields_once_body_in_chunks() {
    futures::executor::block_on(async {
        let body = EdgeBody::from(Bytes::from_static(b"abcdef"));
        let mut source = BodyChunkSource::new(body, 3);

        assert_eq!(source.next_chunk().await.expect("should read").as_deref(), Some(&b"abc"[..]));
        assert_eq!(source.next_chunk().await.expect("should read").as_deref(), Some(&b"def"[..]));
        assert!(source.next_chunk().await.expect("should read").is_none());
    });
}
```

Add a separate test for `Body::Stream` preserving chunk boundaries and surfacing stream errors.

- [ ] **Step 2: Add a failing test for the cumulative cap**

Use a stream with two chunks whose total exceeds a small cap:

```rust
#[test]
fn body_chunk_source_enforces_cumulative_raw_cap() {
    futures::executor::block_on(async {
        let body = EdgeBody::from_stream(futures::stream::iter(vec![
            Ok(Bytes::from_static(b"1234")),
            Ok(Bytes::from_static(b"5678")),
        ]));
        let mut source = BodyChunkSource::new(body, STREAM_CHUNK_SIZE).with_max_bytes(6);

        assert!(source.next_chunk().await.expect("first chunk should pass").is_some());
        let err = source.next_chunk().await.expect_err("second chunk should exceed cap");
        assert!(
            format!("{err:?}").contains("publisher origin body exceeded"),
            "should report cumulative cap: {err:?}"
        );
    });
}
```

- [ ] **Step 3: Run the new tests and verify they fail**

Run:

```bash
cargo test-axum body_chunk_source
```

Expected: FAIL because helper does not exist.

- [ ] **Step 4: Implement `BodyChunkSource`**

Implement a private helper near `STREAM_CHUNK_SIZE`:

- Owns `EdgeBody`.
- For `Body::Once`, yields `Bytes` slices up to `chunk_size` without copying more than necessary.
- For `Body::Stream`, awaits `stream.next()`.
- Tracks cumulative raw bytes and errors when total exceeds `max_bytes`.
- Maps stream errors to `TrustedServerError::Proxy`.

Do not use `block_on` inside the helper.

- [ ] **Step 5: Run helper tests**

Run:

```bash
cargo test-axum body_chunk_source
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Add async publisher body chunk source"
```

## Task 3: Push Compression Helpers

**Files:**

- Modify: `crates/trusted-server-core/src/streaming_processor.rs`
- Or modify: `crates/trusted-server-core/src/publisher.rs` if helpers are publisher-only

- [ ] **Step 1: Add parity tests for compressed chunk processing**

For each compression mode used by publisher HTML (`gzip`, `deflate`, `br`), add a test that feeds compressed HTML in multiple raw chunks through the future async path and verifies the decompressed/processed/recompressed output decodes to expected HTML.

Start with gzip:

```rust
#[test]
fn async_publisher_pipeline_preserves_gzip_html_across_stream_chunks() {
    futures::executor::block_on(async {
        let compressed = gzip_bytes(b"<html><body>Hello</body></html>");
        let body = EdgeBody::from_stream(bytes_to_two_chunk_stream(compressed));
        let output = process_test_body_async(body, "text/html", "gzip")
            .await
            .expect("should process gzip stream");

        assert_eq!(
            gunzip_bytes(&output),
            b"<html><head></head><body>Hello</body></html>"
        );
    });
}
```

Use existing HTML processor expectations rather than inventing new behavior.

- [ ] **Step 2: Run the gzip test and verify it fails**

Run:

```bash
cargo test-axum async_publisher_pipeline_preserves_gzip_html_across_stream_chunks
```

Expected: FAIL because async compressed processing does not exist.

- [ ] **Step 3: Implement write-based push decoders**

Use write-based APIs:

- `flate2::write::GzDecoder`
- `flate2::write::ZlibDecoder`
- `brotli::DecompressorWriter`

The helper should:

- Accept raw compressed chunks.
- Write decoded bytes into an internal `Vec<u8>` sink.
- Return newly decoded bytes after each input chunk.
- Finalize at EOF and return any decoder tail bytes.
- Surface decoder errors as `TrustedServerError::Proxy`.

Keep this helper private unless tests or other modules need it.

- [ ] **Step 4: Implement output encoding wrapper**

Continue to use existing write-based encoders:

- `flate2::write::GzEncoder`
- `flate2::write::ZlibEncoder`
- `brotli::enc::writer::CompressorWriter`

The async loop should write processed decoded chunks into the encoder and finalize once.

- [ ] **Step 5: Add deflate and brotli tests**

Run:

```bash
cargo test-axum async_publisher_pipeline_preserves_
```

Expected: gzip, deflate, and brotli async parity tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/streaming_processor.rs crates/trusted-server-core/src/publisher.rs
git commit -m "Add push compression support for publisher streams"
```

## Task 4: Async Publisher Pipeline Without Auction

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add stream-vs-once parity tests for no-auction paths**

Cover:

- HTML rewrite.
- RSC flight rewrite.
- Generic URL replacement.
- Unsupported stream body cannot reach sync path.

Example:

```rust
#[test]
fn stream_publisher_body_async_matches_buffered_html_without_auction() {
    futures::executor::block_on(async {
        let settings = create_test_settings();
        let registry = IntegrationRegistry::new(&settings).expect("should build registry");
        let html = Bytes::from_static(b"<html><body><a href=\"https://origin.example/path\">x</a></body></html>");

        let mut once_params = test_process_params("text/html", "");
        let mut once_output = Vec::new();
        stream_publisher_body_async(
            EdgeBody::from(html.clone()),
            &mut once_output,
            &mut once_params,
            &settings,
            &registry,
            &AuctionOrchestrator::new(settings.auction.clone()),
            &noop_services(),
        )
        .await
        .expect("once body should process");

        let mut stream_params = test_process_params("text/html", "");
        let mut stream_output = Vec::new();
        stream_publisher_body_async(
            EdgeBody::from_stream(futures::stream::iter(vec![Ok(html)])),
            &mut stream_output,
            &mut stream_params,
            &settings,
            &registry,
            &AuctionOrchestrator::new(settings.auction.clone()),
            &noop_services(),
        )
        .await
        .expect("stream body should process");

        assert_eq!(stream_output, once_output);
    });
}
```

- [ ] **Step 2: Run parity tests and verify they fail**

Run:

```bash
cargo test-axum stream_publisher_body_async_matches_buffered
```

Expected: FAIL because no-auction async path still delegates to sync processing.

- [ ] **Step 3: Refactor `process_response_streaming` into reusable processor construction**

Extract the shared routing logic into a helper such as:

```rust
enum PublisherProcessor {
    Html(HtmlRewriterAdapter),
    Rsc(RscFlightUrlRewriter),
    Url(StreamingReplacer),
}
```

Or use a generic closure/helper if that fits existing patterns better. The goal is to avoid duplicating content-type routing between sync and async paths.

- [ ] **Step 4: Drive all `stream_publisher_body_async()` calls through async chunk-pull**

Even when `params.dispatched_auction` is `None`, build the same processor and use `BodyChunkSource`. This prevents stream bodies from falling into the sync path.

- [ ] **Step 5: Keep `stream_publisher_body()` for compatibility**

The sync function should remain for old tests and any current non-stream callers, but it must not be used by the async path once this task is complete.

- [ ] **Step 6: Run targeted tests**

Run:

```bash
cargo test-axum stream_publisher_body_async_matches_buffered
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Drive publisher async processing from body chunks"
```

## Task 5: Async Auction Hold Loop

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Replace reader-based hold-loop test with stream-based test**

Update or add a test based on `body_close_hold_loop_processes_close_tail_before_reading_post_body_chunks()`:

- Feed pre-`</body>` chunk.
- Feed held `</body>` chunk.
- Feed post-body chunk.
- Assert the loop collects auction immediately when `</body` is detected before consuming post-body chunks.

Use an instrumented stream, not `ChunkedReader`, so the test matches the new implementation.

- [ ] **Step 2: Add EOF-without-`</body>` test**

Verify that auction collection happens at EOF and finalization still calls `processor.process_chunk(&[], true)`.

- [ ] **Step 3: Add stream error abandonment test**

Feed a stream error after dispatch and assert telemetry abandonment uses `stream_read_error` or the current expected reason.

- [ ] **Step 4: Run tests and verify failures**

Run:

```bash
cargo test-axum body_close_hold_loop
```

Expected: FAIL until the loop consumes `BodyChunkSource`.

- [ ] **Step 5: Change `body_close_hold_loop` to async chunk-pull**

Replace:

```rust
async fn body_close_hold_loop<R: Read, W: Write, P: StreamProcessor>(...)
```

with a shape that accepts decoded chunks from an async driver, or accepts `BodyChunkSource` plus codec state. Keep the control flow:

- Push decoded chunks into `BodyCloseHoldBuffer`.
- Write ready bytes immediately.
- On first `</body`, collect auction, write bids to state, then process held bytes.
- After that, stream subsequent chunks immediately.
- At EOF, collect auction if not collected, then finalize processor.

- [ ] **Step 6: Run hold-loop tests**

Run:

```bash
cargo test-axum body_close_hold_loop
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Make publisher auction hold loop async"
```

## Task 6: Buffered Adapter Compatibility

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add test proving `buffer_publisher_response_async()` can buffer a stream body**

Construct a `PublisherResponse::Stream` with `body: EdgeBody::from_stream(...)`, call `buffer_publisher_response_async()`, and assert:

- Response body contains processed HTML.
- `Content-Length` is set to processed byte length.
- It respects `response_carries_body()` for `HEAD`, `204`, and `304`.

- [ ] **Step 2: Add buffered cap test**

Use a small `settings.publisher.max_buffered_body_bytes`, a stream body exceeding it after processing, and assert `buffer_publisher_response_async()` errors before returning a response.

- [ ] **Step 3: Run tests and verify failures where expected**

Run:

```bash
cargo test-axum buffer_publisher_response_async
```

Expected: new stream-body test fails until previous async changes are wired into buffered finalize.

- [ ] **Step 4: Wire buffered finalize through the async path**

Keep `BoundedWriter` for buffered adapters. `buffer_publisher_response_async()` should call `stream_publisher_body_async()` regardless of whether body is `Once` or `Stream`.

- [ ] **Step 5: Run buffered tests**

Run:

```bash
cargo test-axum buffer_publisher_response_async
cargo test-axum bounded_writer
cargo test-axum response_carries_body
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Preserve buffered publisher finalize with stream inputs"
```

## Task 7: Platform Streaming Capability Gate

**Files:**

- Modify: `crates/trusted-server-core/src/platform/http.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`
- Modify: `crates/trusted-server-core/src/platform/test_support.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add failing tests for publisher origin stream flag**

Use `StubHttpClient::recorded_stream_response_flags()` in publisher tests:

- When client supports streaming, publisher origin fetch sets `stream_response = true`.
- When client does not support streaming, publisher origin fetch leaves it `false`.

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
cargo test-axum publisher_origin_fetch_sets_stream_response_when_supported
```

Expected: FAIL because no capability method exists and publisher fetch never sets the flag.

- [ ] **Step 3: Add capability method**

In `PlatformHttpClient`:

```rust
fn supports_streaming_responses(&self) -> bool {
    false
}
```

In `FastlyPlatformHttpClient`:

```rust
fn supports_streaming_responses(&self) -> bool {
    true
}
```

In `StubHttpClient`, add a configurable flag if tests need both states.

- [ ] **Step 4: Enable publisher origin streaming behind the gate**

At the publisher origin fetch:

```rust
let mut platform_request = PlatformHttpRequest::new(req, backend_name);
if services.http_client().supports_streaming_responses() {
    platform_request = platform_request.with_stream_response();
}
let mut response = services.http_client().send(platform_request).await?;
```

- [ ] **Step 5: Run capability tests**

Run:

```bash
cargo test-axum publisher_origin_fetch_sets_stream_response_when_supported
cargo test-axum publisher_origin_fetch_leaves_stream_response_disabled_when_unsupported
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/platform/http.rs crates/trusted-server-adapter-fastly/src/platform.rs crates/trusted-server-core/src/platform/test_support.rs crates/trusted-server-core/src/publisher.rs
git commit -m "Gate publisher origin streaming by platform capability"
```

## Task 8: Fastly Publisher Streaming Finalize

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs` if a helper is needed
- Possibly modify: `crates/trusted-server-adapter-fastly/src/main.rs` for logging/helper naming

- [ ] **Step 1: Add Fastly route test for publisher response body shape**

In Fastly app tests, configure a publisher HTML origin response and assert the router returns `Body::Stream` for processable publisher responses on `GET`.

Expected assertion:

```rust
assert!(
    matches!(response.body(), Body::Stream(_)),
    "processable publisher response should remain streaming on Fastly"
);
```

- [ ] **Step 2: Add bodiless route tests**

Assert `HEAD`, `204`, and `304` publisher responses do not carry a stream body, preserving existing metadata.

- [ ] **Step 3: Run tests and verify failure**

Run:

```bash
cargo test-fastly publisher_response_streams
```

Expected: FAIL because app still calls `buffer_publisher_response_async()`.

- [ ] **Step 4: Add a core helper to convert `PublisherResponse` to streaming response**

Preferred shape in `publisher.rs`:

```rust
pub fn publisher_response_into_streaming_body(
    publisher_response: PublisherResponse,
    method: &Method,
    settings: Arc<Settings>,
    integration_registry: Arc<IntegrationRegistry>,
    orchestrator: Arc<AuctionOrchestrator>,
    services: RuntimeServices,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>>
```

The helper should:

- Return `PublisherResponse::Buffered` unchanged.
- Return `PublisherResponse::PassThrough` with body attached, except bodiless responses.
- For `PublisherResponse::Stream`, build `EdgeBody::from_stream(futures::stream::unfold(...))` or equivalent.
- Move `OwnedProcessResponseParams`, origin body, settings, registry, orchestrator, and services into the stream state.
- Yield processed chunks as they become available.
- On mid-stream processing error, log and end the stream. The client sees a truncated body, matching existing mid-stream error behavior.

If borrowing/lifetime pressure is high, keep the helper in Fastly `app.rs` and call core `stream_publisher_body_async()` from inside the stream. Prefer core if it avoids Fastly-specific body processing logic.

- [ ] **Step 5: Replace Fastly buffered finalize for publisher route**

In `handle_publisher_route`, replace the `buffer_publisher_response_async()` call for Fastly with the streaming helper. Keep non-Fastly adapters on `buffer_publisher_response_async()`.

- [ ] **Step 6: Preserve entry-point finalization**

Verify the returned `Response<EdgeBody>` still carries extensions needed by `main.rs`:

- `EcFinalizeState`
- `RequestFilterEffects`
- Final cache privacy guard

Headers must be finalized before `send_edgezero_response()` splits the response and commits headers.

- [ ] **Step 7: Run Fastly route tests**

Run:

```bash
cargo test-fastly publisher_response
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Stream Fastly publisher responses to clients"
```

## Task 9: Pass-Through Publisher Bodies

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`

- [ ] **Step 1: Add pass-through large body test**

Verify a non-processable successful publisher response (`image/png`, font, video) is returned as `Body::Stream` when origin streaming is supported and body is not bodiless.

- [ ] **Step 2: Add pass-through bodiless test**

Verify `HEAD`, `204`, and `304` pass-through arms preserve headers but do not attach/drain the stream body.

- [ ] **Step 3: Run targeted tests**

Run:

```bash
cargo test-fastly publisher_pass_through
```

Expected: FAIL if pass-through still buffers or attaches a body for bodiless responses.

- [ ] **Step 4: Make pass-through use the same body-carrying guard**

Mirror `asset_response_carries_body()` semantics in publisher finalize. If `response_carries_body(method, status)` is false, drop the body and return headers only.

- [ ] **Step 5: Run targeted tests**

Run:

```bash
cargo test-fastly publisher_pass_through
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-adapter-fastly/src/app.rs
git commit -m "Preserve publisher pass-through streaming semantics"
```

## Task 10: Headers, Length, and Error Semantics

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` if logs are misleading

- [ ] **Step 1: Add header tests**

Assert for streamed processed publisher responses:

- `Content-Length` is absent.
- `Transfer-Encoding` is not manually set.
- `Content-Encoding` is preserved when recompression is used.
- Cache/privacy headers still downgrade when `Set-Cookie` is present.

- [ ] **Step 2: Add mid-stream cap/error test**

Use a body stream that exceeds `publisher.max_buffered_body_bytes` after headers would be committed. Assert the stream returns an error/truncates consistently with existing mid-stream asset behavior and logs enough context.

- [ ] **Step 3: Run tests and verify failures**

Run:

```bash
cargo test-fastly publisher_stream
```

Expected: FAIL until header/error cleanup is complete.

- [ ] **Step 4: Clean header handling**

Ensure the existing `response.headers_mut().remove(header::CONTENT_LENGTH)` remains on `PublisherResponse::Stream`. Do not re-add content length for streaming finalize.

- [ ] **Step 5: Improve log wording**

If `main.rs` still logs "asset streaming" for all `EdgeBody::Stream` responses, rename log messages to "EdgeZero streaming body" or split publisher/asset helpers.

- [ ] **Step 6: Run tests**

Run:

```bash
cargo test-fastly publisher_stream
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Tighten publisher streaming headers and errors"
```

## Task 11: End-to-End Regression Coverage

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: existing integration/parity tests if appropriate

- [ ] **Step 1: Add a slow-origin behavior test if feasible in existing harness**

Preferred test shape:

- Origin response body is a stream with first chunk available immediately and second chunk delayed or instrumented.
- Router returns `Body::Stream` without collecting the whole body.
- Pulling the first output chunk does not require pulling the entire origin stream.

If the harness cannot model time cleanly, use an instrumented stream that panics if polled past the first chunk before the returned response body is consumed.

- [ ] **Step 2: Add auction timing test**

Verify response construction does not await `collect_dispatched_auction`; collection happens when the body stream is pulled and reaches `</body>` or EOF.

- [ ] **Step 3: Run targeted tests**

Run:

```bash
cargo test-fastly publisher_streaming_does_not_buffer_origin_before_response
```

Expected: PASS after Fastly finalize is lazy.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-core/src/publisher.rs
git commit -m "Cover lazy publisher streaming behavior"
```

## Task 12: Documentation Cleanup

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/settings.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Optionally modify: issue/PR description only, not repo docs

- [ ] **Step 1: Update stale interim comments**

Remove or rewrite comments saying publisher stream bodies are already materialized into WASM heap.

Targets:

- `PublisherResponse::Stream` doc.
- `PublisherResponse::PassThrough` doc if Fastly now preserves stream bodies.
- `settings.rs` comments that reference future true streaming.
- Fastly app module comment that says publisher responses are buffered by `publisher.max_buffered_body_bytes`.

- [ ] **Step 2: Run doc-related checks locally**

Run:

```bash
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/settings.rs crates/trusted-server-adapter-fastly/src/app.rs
git commit -m "Update publisher streaming documentation"
```

## Task 13: Full Verification

**Files:**

- No source edits unless failures reveal issues.

- [ ] **Step 1: Run formatting**

Run:

```bash
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 2: Run target checks**

Run:

```bash
cargo check-fastly
cargo check-axum
cargo check-cloudflare
```

Expected: PASS.

- [ ] **Step 3: Run target tests**

Run:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
```

Expected: PASS.

- [ ] **Step 4: Run Spin if touched by shared trait changes**

Run:

```bash
cargo test-spin
```

Expected: PASS.

- [ ] **Step 5: Run clippy gates**

Run:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: PASS.

- [ ] **Step 6: Run parity suite if available locally**

Run:

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: PASS.

- [ ] **Step 7: Optional local TTFB smoke**

Run Fastly local serve against an artificially slow publisher origin:

- Origin sends headers and first HTML chunk immediately.
- Origin delays later body chunks.
- Verify browser/curl receives response headers and first chunk before full origin drain.
- Verify bids still inject before `</body>` when auction completes.

Expected: TTFB tracks origin first byte, not full origin transfer or auction collection.

## Review Checklist

- [ ] No `block_on` inside stream body processing or `Read::read` equivalents.
- [ ] `Body::Stream` never falls through `into_bytes().unwrap_or_default()`.
- [ ] Fastly publisher origin fetch sets `with_stream_response()` only through capability gate.
- [ ] Cloudflare, Spin, and Axum do not start receiving stream-response requests.
- [ ] `HEAD`, `204`, and `304` do not drive or attach response bodies.
- [ ] `Content-Length` is absent on processed streaming responses.
- [ ] Existing buffered adapters still work through `buffer_publisher_response_async()`.
- [ ] Auction telemetry handles completed and abandoned stream cases.
- [ ] Mid-stream errors do not panic; they log and truncate consistently with current streaming behavior.
- [ ] Comments no longer describe publisher streaming as interim/in-memory cursor based.

## PR Description Skeleton

```markdown
## Summary

- convert publisher response processing to async chunk-pull over `Body::Stream`
- enable Fastly publisher origin streaming behind a platform capability gate
- stream Fastly publisher responses to clients instead of buffering and awaiting auction before send

## Scope

Fastly production path for issue #849. Cloudflare, Spin, and Axum streaming remain follow-up work.

## Tests

- [ ] cargo fmt --all -- --check
- [ ] cargo check-fastly
- [ ] cargo check-axum
- [ ] cargo check-cloudflare
- [ ] cargo test-fastly
- [ ] cargo test-axum
- [ ] cargo test-cloudflare
- [ ] cargo test-spin
- [ ] cargo clippy-fastly
- [ ] cargo clippy-axum
- [ ] cargo clippy-cloudflare
- [ ] cargo clippy-cloudflare-wasm
- [ ] cargo clippy-spin-native
- [ ] cargo clippy-spin-wasm
```

## Known Follow-Ups

- Cloudflare origin streaming once Worker `ReadableStream` is wrapped into `Body::Stream` and response header/set-cookie behavior is verified.
- Spin streaming after upstream EdgeZero Spin response conversion supports incremental body writes.
- Axum streaming only if the dev server needs it enough to justify a `Send` bridge.
- Issue #850 parser-context `</body>` detection.
