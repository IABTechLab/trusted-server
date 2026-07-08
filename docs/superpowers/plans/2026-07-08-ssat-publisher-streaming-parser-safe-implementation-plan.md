# SSAT publisher streaming parser-safe implementation plan

**Source spec:** `docs/superpowers/specs/2026-07-08-ssat-publisher-streaming-parser-safe-design.md`  
**Issue:** #857  
**Status:** Draft implementation plan  
**Area:** Trusted Server runtime / publisher fallback / server-side ad stack

## Objective

Implement the spec in small, reviewable slices so Fastly publisher SSAT HTML can
stream origin bytes through decode/rewrite/bid late-binding/re-encode to the
client without full origin-body or assembled-body materialization, while keeping
all non-Fastly adapters and full-document post-processors explicitly buffered for
this slice.

The plan preserves the project constraints in `CLAUDE.md`: minimal unrelated
refactoring, no bare workspace `cargo test`, `error-stack` errors, `log` macros,
no Tokio or OS-only dependencies in core runtime code, and target-aware validation.

## Current code findings

- `crates/trusted-server-core/src/publisher.rs`
  - `handle_publisher_request` always sends publisher origin requests with
    `PlatformHttpRequest::new(req, backend_name)`, so Fastly uses the default
    buffered platform response path.
  - `body_as_reader()` calls `EdgeBody::into_bytes()`, which is incompatible with
    true streaming and would materialize `EdgeBody::Stream(_)`.
  - `stream_publisher_body_async` uses `BodyCloseHoldBuffer`, which raw-scans
    decoded origin bytes for `</body` before the HTML parser confirms context.
  - `PublisherResponse::Stream` currently means "headers/body separated for
    later processing", not necessarily origin/client streaming.
  - Processed routes remove `Content-Length`, but not the rest of the stale
    payload validators/range metadata required by the spec.
- `crates/trusted-server-core/src/html_processor.rs`
  - The body end-tag handler directly reads `ad_bids_state` and inserts bids.
  - `HtmlWithPostProcessing` buffers rewritten output until EOF whenever an
    `IntegrationHtmlPostProcessor` exists, which must remain buffered and must
    see final HTML rather than an internal placeholder.
- `crates/trusted-server-adapter-fastly/src/platform.rs`
  - `PlatformHttpRequest::with_stream_response()` already preserves Fastly
    origin response bodies as `EdgeBody::Stream(_)`.
  - `send_async` rejects `stream_response`, so publisher origin streaming must
    use the synchronous `send` path already used by `handle_publisher_request`.
- `crates/trusted-server-adapter-fastly/src/app.rs`
  - The EdgeZero fallback path always calls `buffer_publisher_response_async`,
    collapsing publisher responses before the Fastly send boundary.
- `crates/trusted-server-adapter-fastly/src/main.rs`
  - `send_edgezero_response` treats every `EdgeBody::Stream(_)` as asset
    pass-through and calls `stream_asset_body`; it cannot carry SSAT auction
    state or run the publisher assembly loop.
- `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/app.rs`
  - These already buffer publisher responses through
    `buffer_publisher_response_async`; the new work should keep that behavior
    explicit and covered by tests.

## Guiding implementation decisions

1. **Parser authority:** only `lol_html` may identify the real body end tag.
   The late-binding layer scans processed, uncompressed output for an opaque
   placeholder inserted by `lol_html`.
2. **Separate publisher streaming from asset streaming:** do not encode SSAT
   streaming state as a plain `EdgeBody::Stream(_)` at the Fastly send boundary.
3. **Buffered compatibility first:** keep existing buffered adapters working
   through the parser-safe path before enabling Fastly true streaming.
4. **Post-processors are a route guard:** any registered full-document HTML
   post-processor means buffered mode for this slice.
5. **No unbounded state:** enforce decoded-input, processed-output, and held-tail
   caps even when no full body is allocated.
6. **Headers commit once:** all privacy/finalization/request-filter/header
   normalization work must happen before Fastly calls `stream_to_client()`.

## Phase 0: Add seam tests before behavior changes

Add focused tests that describe the current desired outcome. Some can initially
fail or be marked around new helpers as they are introduced, but each phase
should land with passing tests for the slice implemented.

### Target files

- `crates/trusted-server-core/src/publisher.rs`
- `crates/trusted-server-core/src/html_processor.rs`
- `crates/trusted-server-core/src/platform/test_support.rs`
- `crates/trusted-server-adapter-fastly/src/app.rs`
- `crates/trusted-server-adapter-fastly/src/main.rs`

### Tests to add early

- Parser-safety fixtures:
  - inline `<script>` containing `'</body>'` does not trigger bid collection;
  - script/JSON data containing literal or escaped `</body` does not trigger
    collection;
  - real `</body>` split across origin chunks still injects before the close tag;
  - missing `</body>` appends the fallback tail at EOF;
  - missing `<head>` plus missing `</body>` appends the minimal executable tail;
  - multiple body close tags do not inject multiple bid scripts.
- Routing seams:
  - Fastly request-level SSAT streaming candidates set `stream_response = true`;
  - registries with HTML post-processors do not set `stream_response = true`;
  - non-Fastly adapters keep publisher `stream_response = false`.
- Header seams:
  - processed publisher HTML strips stale payload headers;
  - pass-through/unmodified streams preserve origin validators and range metadata.

## Phase 1: Core parser-safe late binding

### 1.1 Add a late-binding module

Create `crates/trusted-server-core/src/publisher_late_binding.rs` and export it
from `crates/trusted-server-core/src/lib.rs` or keep it `pub(crate)` from
`publisher.rs`.

Suggested types:

```rust
pub(crate) const SSAT_HELD_TAIL_CAP_BYTES: usize = 64 * 1024;

pub(crate) struct BidPlaceholder {
    token: String,
    html: String,
}

pub(crate) struct HtmlInjectionTracker {
    head_injected: AtomicBool,
    placeholder_inserted: AtomicBool,
}

pub(crate) enum LateBindingOutcome {
    BodyClose,
    EofFallback,
    MissingHeadEofFallback,
}
```

Responsibilities:

- Generate a high-entropy per-request placeholder with `uuid::Uuid::new_v4()`.
- Render the placeholder as a valid HTML comment.
- Expose byte accessors for streaming scans.
- Track whether `<head>` bootstrap and body-close placeholder insertion occurred.
- Define small helper errors as `TrustedServerError::Proxy` contexts rather than
  adding broad new error enums unless needed.

Acceptance tests:

- placeholders are unique across requests;
- placeholder bytes are valid UTF-8 and valid comment-shaped HTML;
- `LateBindingScanner` can find a placeholder split across arbitrary chunks;
- later occurrences are stripped after first replacement.

### 1.2 Extend HTML processor configuration

Modify `crates/trusted-server-core/src/html_processor.rs`.

Suggested API:

```rust
pub enum BidInjectionMode {
    DirectState,
    Placeholder { html: String, tracker: Arc<HtmlInjectionTracker> },
}

pub enum HtmlPostProcessingMode {
    Enabled,
    Disabled,
}
```

Add fields to `HtmlProcessorConfig`:

- `bid_injection_mode: BidInjectionMode` defaulting to `DirectState`;
- `post_processing_mode: HtmlPostProcessingMode` defaulting to `Enabled`;
- optional shared `HtmlInjectionTracker`.

Add builder methods:

- `with_ad_state(...)` remains for existing call sites;
- `with_bid_placeholder(placeholder, tracker)` for SSAT late binding;
- `without_post_processing()` for the buffered late-binding-before-postprocess
  flow and Fastly true streaming guard.

Refactor the existing head injection code into a reusable helper, for example:

```rust
pub(crate) fn build_head_bootstrap_snippet(
    integrations: &IntegrationRegistry,
    ctx: &IntegrationHtmlContext<'_>,
    ad_slots_script: Option<&str>,
) -> String
```

The helper must preserve current executable ordering:

1. ad slot state;
2. integration head config inserts;
3. main TSJS bundle;
4. deferred TSJS bundle tags.

Body end-tag behavior:

- If no slots matched, keep skipping bid injection entirely.
- In `DirectState`, preserve current behavior for compatibility.
- In `Placeholder`, insert the placeholder in the first parser-confirmed body
  end tag only, using the existing once-only guard pattern.
- Set `tracker.placeholder_inserted = true` only when the placeholder is inserted.
- Set `tracker.head_injected = true` when the `<head>` injection actually runs.

Acceptance tests:

- `</body>` inside scripts/comments/attributes does not insert the placeholder;
- a real body close inserts one placeholder before the close tag;
- multiple body tags or end tags still produce at most one placeholder;
- direct-state mode remains compatible with existing tests.

### 1.3 Build the placeholder late binder

Implement a streaming scanner over processed uncompressed output.

Suggested shape:

```rust
pub(crate) struct PlaceholderLateBinder {
    placeholder: BidPlaceholder,
    overlap: Vec<u8>,
    replaced: bool,
    held_tail_cap: usize,
}

pub(crate) enum BinderEvent {
    Emit(Vec<u8>),
    PlaceholderFound { prefix: Vec<u8>, suffix: Vec<u8> },
}
```

The scanner must:

- keep at most `placeholder.len() - 1` overlap before the placeholder is found;
- emit all safe prefix bytes immediately;
- on first placeholder, return the prefix and suffix around the placeholder;
- hold only the suffix from the current processed chunk while the auction is
  collected;
- enforce `min(SSAT_HELD_TAIL_CAP_BYTES, settings.publisher.max_buffered_body_bytes)`;
- after first replacement, strip any subsequent placeholder bytes defensively;
- never forward the placeholder to the client or to post-processors.

Acceptance tests:

- placeholder split across chunks is detected;
- bytes before the placeholder are emitted as soon as safe;
- replacement happens exactly once;
- duplicate placeholder occurrences are stripped;
- held-tail cap violation maps to a proxy error.

### 1.4 Replace `BodyCloseHoldBuffer`

Modify `crates/trusted-server-core/src/publisher.rs`.

Replace raw `BodyCloseHoldBuffer` / `find_ascii_case_insensitive` usage with the
placeholder late-binding flow:

```text
decoded origin bytes
  -> HTML processor configured with placeholder mode and no post-processors
  -> processed uncompressed output
  -> PlaceholderLateBinder
  -> auction collect / empty bid script / EOF fallback
  -> output cap counter
  -> optional recompression
  -> writer
```

Important behavior:

- If `params.dispatched_auction` exists, collect it when the first placeholder is
  found; otherwise collect at EOF fallback.
- If no auction was dispatched but slots exist, replace the placeholder
  immediately with the current or empty bid script.
- Existing `collect_stream_auction`, `write_bids_to_state`, and
  `build_bids_script` paths should be reused so output shape and debug comments
  remain stable.
- Auction telemetry must complete or abandon on every exit path.
- Delete or deprecate `BodyCloseHoldBuffer` tests once equivalent placeholder
  tests cover the behavior.

### 1.5 Implement EOF fallback tail

On EOF, after finalizing `lol_html`:

1. Feed final processor output through the binder.
2. If a placeholder appears in final output, replace it normally.
3. If no placeholder was ever found:
   - collect any dispatched auction;
   - if `tracker.head_injected` is true, append only the bids script;
   - if `tracker.head_injected` is false, append the minimal bootstrap tail in
     executable order using the extracted head-snippet helper plus the bids
     script.

The fallback tail is best-effort malformed-document handling; it must still:

- count against the processed-output cap;
- never expose placeholders;
- preserve current privacy behavior for SSAT HTML.

## Phase 2: Streaming pipeline, compression, and caps

### 2.1 Add a non-materializing publisher body pump

The current synchronous `StreamingPipeline::process` is safe for buffered
`EdgeBody::Once` but not enough for Fastly true streaming because `body_as_reader`
uses `into_bytes()`.

Add a publisher-specific async body pump in `publisher.rs` or a new core module
such as `publisher_body_stream.rs`.

Requirements:

- Consume `EdgeBody::Once` by chunking the bytes without copying the full payload
  again.
- Consume `EdgeBody::Stream(_)` with `StreamExt::next().await`.
- Never call `EdgeBody::into_bytes()`, `into_bytes_bounded()`, or equivalent on
  the true streaming SSAT path.
- Convert origin stream errors into `TrustedServerError::Proxy` with context.
- Preserve upstream backpressure by pausing reads while auction collection runs.

Compression implementation options:

- Prefer explicit incremental decoder/encoder state machines driven by origin
  chunks:
  - `flate2::Decompress` / `flate2::Compress` for gzip and deflate if practical;
  - brotli streaming reader/writer APIs with small bounded buffers.
- If reusing `std::io::Read`-based decoders with a custom stream reader, document
  why it does not materialize and test that it consumes one upstream chunk at a
  time. Avoid introducing Tokio/OS-only async dependencies into core.

### 2.2 Preserve compression finalization

For processed streaming HTML with supported encodings:

- identity: write processed bytes directly;
- gzip: finalize with `finish()` and propagate errors;
- deflate: finalize with `finish()` and propagate errors;
- brotli: explicitly flush/finalize through the available brotli writer API and
  propagate write errors where available.

The placeholder scanner must run after HTML rewriting and before recompression.

Acceptance tests:

- gzip, deflate, and brotli origin HTML decode/rewrite/re-encode successfully;
- decoded client output has bids before the real parser-confirmed `</body>`;
- final compressed response can be decoded by a client;
- finalization errors are propagated or logged/dropped after commit, depending
  on where they occur.

### 2.3 Enforce streaming caps

Use `settings.publisher.max_buffered_body_bytes` as the effective cumulative
limit for this slice.

Counters:

- `decoded_input_bytes`: increment after decompression and before `lol_html`;
- `processed_output_bytes`: increment after late binding and before
  recompression;
- `held_tail_bytes`: enforce via `PlaceholderLateBinder`.

Pre-commit rejection:

- For identity responses, if `Content-Length` exceeds the limit, reject before
  `stream_to_client()`.
- Do not use compressed `Content-Length` as proof of decoded size.

Mid-stream violation:

- Return a proxy error from the streaming loop.
- Fastly send path logs and drops the `StreamingBody` without `finish()` because
  headers are already committed.
- Buffered paths return a normal error response as they do today.

Acceptance tests:

- large identity HTML below cap succeeds;
- decoded input over cap fails/aborts;
- processed output over cap fails/aborts;
- gzip/deflate/br expansion over cap is caught after decode;
- held-tail cap is enforced.

### 2.4 Normalize transformed response headers

Add a helper in `publisher.rs`, for example:

```rust
pub(crate) fn strip_transformed_payload_headers(headers: &mut HeaderMap)
```

Remove at minimum:

- `Content-Length`
- `Content-MD5`
- `Digest`
- `Repr-Digest`
- `Content-Range`
- `Accept-Ranges`
- `ETag`

Apply this helper to every processed publisher HTML response, buffered or
streaming. Do not apply it to unmodified pass-through streams.

Keep `Content-Encoding` when the body is re-encoded with the same encoding.
`Transfer-Encoding` remains adapter-owned.

## Phase 3: Fastly request/response streaming path

### 3.1 Add request-level streaming candidate options

Modify `handle_publisher_request` without breaking existing adapters.

Suggested API:

```rust
pub struct PublisherRequestOptions {
    pub allow_origin_streaming: bool,
}

pub async fn handle_publisher_request_with_options(..., options: PublisherRequestOptions)
```

Keep the current `handle_publisher_request(...)` as a wrapper with
`allow_origin_streaming = false` so Axum/Cloudflare/Spin remain buffered by
default.

Request-level Fastly candidate gate:

- method is `GET` and not `HEAD`;
- request is a navigation;
- server-side ad stack may run for the request;
- no full-document HTML post-processors are registered;
- Fastly caller can preserve publisher streaming state out-of-band;
- origin fetch uses `send`, not `send_async`.

When all gates pass, build the origin request with:

```rust
PlatformHttpRequest::new(req, backend_name).with_stream_response()
```

Tests should assert `StubHttpClient::recorded_stream_response_flags()` for
eligible and ineligible requests. If needed, extend `StubHttpClient` so a queued
response can return `EdgeBody::Stream(_)` when `stream_response` is true.

### 3.2 Refine response-level route decisions

The existing `classify_response_route` treats processable non-HTML content as
`Stream`. For this issue, true Fastly SSAT streaming is only for SSAT HTML.

Add either a new classifier or extend route context:

```rust
pub(crate) enum PublisherSendRoute {
    ProcessedHtmlStream,
    StreamUnmodified,
    BufferedProcessed,
    BufferedUnmodified,
    PassThrough,
    Bodiless,
}
```

Response-level stream eligibility for Fastly:

- request/response can carry a body (`GET`, not `HEAD`; not `204`, `205`, `304`);
- response is HTML and SSAT assembly is needed;
- content encoding is identity/gzip/deflate/br;
- no post-processors;
- identity `Content-Length` preflight passes;
- processor construction succeeds before client commit.

If a streaming candidate resolves to non-HTML or unsupported encoding:

- stream the origin body unmodified when safe;
- abandon any dispatched auction with telemetry;
- preserve bodyless semantics for `HEAD`, `204`, `205`, and `304`.

If the response needs buffered mode and that requirement was known before fetch
(post-processors), it should never have requested `with_stream_response()`.

### 3.3 Preserve publisher-streaming state across the Fastly boundary

Do not put the SSAT streaming state into `http::Extensions`: `DispatchedAuction`
can carry adapter-specific pending request handles and should be treated as
`!Send` / not extension-safe.

Add a Fastly-specific out-of-band outcome, likely in a new file
`crates/trusted-server-adapter-fastly/src/publisher_streaming.rs`:

```rust
pub(crate) enum FastlyPublisherFallbackOutcome {
    Response(HttpResponse),
    Stream(FastlyPublisherStream),
}

pub(crate) struct FastlyPublisherStream {
    response: HttpResponse,
    body: EdgeBody,
    params: Box<OwnedProcessResponseParams>,
    method: Method,
    ec_state: Option<EcFinalizeState>,
    request_filter_effects: Option<RequestFilterEffects>,
    services: RuntimeServices,
    settings: Arc<Settings>,
    registry: IntegrationRegistry,
    orchestrator: Arc<AuctionOrchestrator>,
}
```

The exact ownership can vary, but the stream outcome must carry everything the
send path needs after headers are finalized:

- response skeleton;
- origin stream;
- `OwnedProcessResponseParams`;
- method/status for no-body checks;
- settings and registry;
- orchestrator and runtime services;
- EC/request-filter/finalization state.

### 3.4 Split Fastly publisher fallback dispatch from generic EdgeZero response dispatch

The existing router returns only `HttpResponse`, which forces buffering. Add a
Fastly-specific publisher fallback dispatcher that can return
`FastlyPublisherFallbackOutcome` before generic `send_edgezero_response` loses
state.

Recommended approach:

1. Extract shared publisher fallback setup from `dispatch_fallback` into helper
   functions where practical, without refactoring unrelated named routes.
2. In `edgezero_main`, after request conversion and app state construction,
   detect the fallback publisher route that is eligible for the Fastly SSAT path.
3. For that path, call the new Fastly publisher dispatcher directly.
4. For all other paths, continue using `app.router().oneshot(core_req)` and
   `send_edgezero_response` unchanged.

The Fastly publisher dispatcher must preserve middleware ordering:

1. forwarded-header sanitization already happened on the original Fastly request;
2. EC request-state setup;
3. pre-route request filters;
4. asset/named/integration routes must still bypass this path;
5. `handle_publisher_request_with_options(... allow_origin_streaming = true ...)`;
6. buffered fallback for non-stream outcomes;
7. attach or carry EC finalize state and request-filter effects;
8. entry-point finalize headers;
9. EC finalization;
10. request-filter response effects;
11. final set-cookie cache privacy guard.

If this direct path would duplicate too much router logic, an acceptable
alternative is to make `execute_fallback` return an adapter-private enum and keep
the router for all ordinary responses. The key invariant is that the publisher
stream state reaches Fastly `main.rs` without becoming a plain `EdgeBody::Stream`.

### 3.5 Add Fastly publisher stream send path

In `crates/trusted-server-adapter-fastly/src/main.rs`:

- Add `send_fastly_publisher_stream(FastlyPublisherStream)`.
- Apply request-filter effects to headers before commit.
- Reapply final set-cookie privacy guard before commit.
- Strip transformed payload headers before commit.
- Convert response headers to a Fastly skeleton.
- Call `stream_to_client()`.
- Drive `stream_publisher_body_async(...)` into the Fastly `StreamingBody`.
- Call `finish()` only on success.
- On mid-stream error, log and drop the `StreamingBody` without buffered fallback.
- Run pull-sync-after-send behavior equivalent to the existing EC path once the
  response has been sent or attempted.

Acceptance tests:

- Fastly SSAT HTML takes the publisher stream path, not asset `stream_asset_body`;
- headers are finalized before streaming starts;
- `Content-Length` and stale validators are absent for processed streaming HTML;
- non-HTML/unsupported-encoding candidate responses stream unmodified and
  abandon auction;
- `HEAD`, `204`, `205`, and `304` do not attach processed streaming bodies.

## Phase 4: Buffered-mode guards, documentation, and adapter parity

### 4.1 Route post-processors to buffered mode

Use `IntegrationRegistry::has_html_post_processors()` in the request-level gate
before origin fetch. If true:

- do not call `with_stream_response()`;
- keep using `buffer_publisher_response_async`;
- run parser-safe late binding before post-processors;
- ensure no placeholder is visible to post-processors or clients.

Implementation detail:

- Configure `lol_html` with placeholder mode and post-processing disabled.
- Run late binding into the bounded buffer.
- Then run registered post-processors over the full final HTML.

This may require extracting post-processing from `HtmlWithPostProcessing` into a
reusable helper, while keeping existing public behavior unchanged for other
callers.

### 4.2 Keep non-Fastly adapters explicitly buffered

Update comments and tests in:

- `crates/trusted-server-adapter-axum/src/app.rs`
- `crates/trusted-server-adapter-cloudflare/src/app.rs`
- `crates/trusted-server-adapter-spin/src/app.rs`

Expected behavior:

- they call the default non-streaming publisher handler;
- they call `buffer_publisher_response_async`;
- they benefit from parser-safe late binding and EOF fallback;
- they enforce the existing buffered cap;
- they are not described as true streaming.

### 4.3 Preserve privacy/cache behavior

Regression tests should prove SSAT HTML still gets:

```http
Cache-Control: private, max-age=0
```

and strips shared/runtime edge-cache headers:

- `Surrogate-Control`
- `Fastly-Surrogate-Control`
- `CDN-Cache-Control`
- `Cloudflare-CDN-Cache-Control`

The final set-cookie cache privacy guard in Fastly must still run after EC
finalization and request-filter effects.

### 4.4 Update docs

Candidate docs to update after implementation behavior exists:

- Fastly runtime / architecture docs: Fastly SSAT publisher HTML is true
  streaming only on the guarded path.
- Adapter docs: Axum, Cloudflare, and Spin publisher SSAT are buffered for this
  slice.
- Next.js integration docs: full-document post-processing remains buffered.

Run docs formatting if docs are edited:

```bash
cd docs && npm run format
```

## Phase 5: Verification plan

Run focused validation after each phase, then the full target-matched gate before
handoff.

Minimum Rust verification:

```bash
cargo fmt --all -- --check
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

If JS/TS is touched unexpectedly:

```bash
cd crates/trusted-server-js/lib && npx vitest run
cd crates/trusted-server-js/lib && npm run format
```

If docs are touched:

```bash
cd docs && npm run format
```

Do **not** use bare `cargo test --workspace`; it compiles Fastly for the wrong
target in this workspace.

## Acceptance criteria checklist

- [ ] Fastly request-level SSAT candidates request origin streaming with
      `with_stream_response()`.
- [ ] Fastly processed SSAT HTML writes to `fastly::StreamingBody` without first
      materializing the origin body or final assembled body.
- [ ] Publisher SSAT streaming state is not mistaken for asset pass-through
      `EdgeBody::Stream(_)`.
- [ ] `</body` literals inside scripts, JSON, comments, and attributes do not
      trigger auction collection.
- [ ] Bids are injected before the first parser-confirmed `</body>`.
- [ ] Missing `</body>` appends bids or minimal SSAT fallback tail at EOF.
- [ ] gzip, deflate, and brotli stream through decode/rewrite/re-encode.
- [ ] Decoded-input, processed-output, and held-tail caps are enforced without a
      full-document allocation.
- [ ] Full-document post-processors route to buffered mode and never see a raw
      placeholder.
- [ ] Axum, Cloudflare, and Spin remain documented/tested buffered mode.
- [ ] SSAT HTML retains `Cache-Control: private, max-age=0` and no shared edge
      cache headers.
- [ ] Processed HTML strips stale payload validators/range metadata; unmodified
      pass-through preserves them.
- [ ] Every dispatched auction is collected or abandoned on all exit paths.

## Risks and open questions

- **Fastly fallback parity risk:** a direct Fastly publisher streaming dispatcher
  can accidentally bypass auth, request filters, EC finalization, pull sync, or
  cache privacy guards. Keep the direct path narrow and share helpers with the
  existing fallback path where possible.
- **Streaming decoder complexity:** current compression helpers are `Read`/`Write`
  based. The true stream path needs chunk-driven processing without
  `into_bytes()`. Prefer a small publisher-specific pump and extensive tests over
  broad refactors to `StreamingPipeline`.
- **Post-processor ordering:** Next.js/full-document post-processing must see
  final HTML with bids, not placeholders. This likely requires extracting
  post-processing into an explicit buffered step.
- **Mid-stream failures:** once Fastly commits headers, cap/decode/processor/write
  errors can only truncate the response. Tests and logs should reflect that; do
  not attempt buffered fallback after commit.
- **Type ownership:** avoid storing `DispatchedAuction` or other adapter-specific
  pending handles in `http::Extensions`; use an adapter-private outcome that can
  carry non-`Send` state directly.
- **Scope control:** do not make Next.js post-processing streaming-safe and do not
  add true streaming for Axum/Cloudflare/Spin in this issue.
