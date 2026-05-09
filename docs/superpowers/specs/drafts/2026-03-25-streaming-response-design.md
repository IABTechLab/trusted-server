---
status: draft
---

# Streaming Response Optimization (Next.js Disabled)

## Problem

When Next.js is disabled, the publisher proxy buffers the entire response body
in memory before sending any bytes to the client. This creates two costs:

1. **Latency** — The client receives zero bytes until the full response is
   decompressed, rewritten, and recompressed. For a 222KB HTML page, this adds
   hundreds of milliseconds to time-to-last-byte.
2. **Memory** — Peak memory holds ~4x the response size simultaneously
   (compressed input + decompressed + processed output + recompressed output).
   With WASM's ~16MB heap, this limits the size of pages we can proxy.

## Scope

**In scope**: All content types flowing through the publisher proxy path — HTML,
text/JSON, RSC Flight (`text/x-component`), and binary pass-through. Only when
Next.js is disabled (no post-processor requiring the full document).

**Out of scope**: Concurrent origin+auction fetch, Next.js-enabled paths (these
require full-document post-processing by design), non-publisher routes (static
JS, auction, discovery).

## Streaming Gate

Before committing to `stream_to_client()`, check:

1. Backend status is success (2xx).
2. For HTML content: `has_html_post_processors()` returns false — no registered
   post-processors. This method returns a `bool` directly, avoiding the
   allocation of cloning the `Vec<Arc<dyn IntegrationHtmlPostProcessor>>` that
   `html_post_processors()` performs. Non-HTML content types (text/JSON, RSC
   Flight, binary) can always stream regardless of post-processor registration,
   since post-processors only apply to HTML.

If either check fails for the given content type, fall back to the current
buffered path. This keeps the optimization transparent: same behavior for all
existing configurations, streaming only activates when safe.

## Architecture

Two implementation steps, each independently valuable and testable.

### Step 1: Make the pipeline chunk-emitting

Three changes to existing processors:

#### A) `HtmlRewriterAdapter` — incremental streaming

The current implementation accumulates the entire HTML document and processes it
on `is_last`. This is unnecessary — `lol_html::HtmlRewriter` supports
incremental `write()` calls and emits output via its `OutputSink` callback after
each chunk.

Fix: create the rewriter eagerly in the constructor, use
`Rc<RefCell<Vec<u8>>>` to share the output buffer between the sink and
`process_chunk()`, drain the buffer on every call instead of only on `is_last`.
The output buffer is drained _after_ each `rewriter.write()` returns, so the
`RefCell` borrow in the sink closure never overlaps with the drain borrow.

Note: this makes `HtmlRewriterAdapter` single-use — `reset()` becomes a no-op
since the `Settings` are consumed by the rewriter constructor. This matches
actual usage (one adapter per request).

#### B) Chunk-based decompression for all compression paths

`process_gzip_to_gzip` calls `read_to_end()` to decompress the entire body into
memory. The deflate and brotli keep-compression paths already use chunk-based
`process_through_compression()`.

Fix: use the same `process_through_compression` pattern for gzip.

Additionally, `decompress_and_process()` (used by `process_gzip_to_none`,
`process_deflate_to_none`, `process_brotli_to_none`) also calls
`read_to_end()`. These strip-compression paths must be converted to chunk-based
processing too — read decompressed chunks, process each, write uncompressed
output directly.

Reference: `process_uncompressed` already implements the correct chunk-based
pattern (read loop → `process_chunk()` per chunk → `write_all()` → flush). The
compressed paths should follow the same structure.

#### C) `process_through_compression` finalization — prerequisite for B

`process_through_compression` currently calls `flush()` (with error
propagation) then `drop(encoder)` for finalization. The `flush()` only flushes
buffered data but does not write compression trailers/footers — `drop()`
handles finalization but silently swallows errors. Today this affects deflate
and brotli (which already use this path). The current `process_gzip_to_gzip` calls `encoder.finish()` explicitly —
but Step 1B moves gzip to `process_through_compression`, which would **regress**
gzip from working `finish()` to broken `drop()`. This fix prevents that
regression and also fixes the pre-existing issue for deflate/brotli.

Fix: call `encoder.finish()` explicitly and propagate errors. This must land
before or with Step 1B.

### Step 2: Stream response to client

> **Note:** Step 2 may need adjustment to align with the EC (Edge Compute)
> implementation. Coordinate with the EC work before finalizing the approach.

Change the publisher proxy path to use Fastly's `StreamingBody` API:

1. Fetch from origin, receive response headers.
2. Validate status — if backend error, return buffered error response via
   `send_to_client()`.
3. Check streaming gate — if `has_html_post_processors()` returns true, fall
   back to buffered path.
4. Finalize all response headers. This requires reordering two things:
   - **Synthetic ID/cookie headers**: today set _after_ body processing in
     `handle_publisher_request`. Since they are body-independent (computed from
     request cookies and consent context), move them _before_ streaming.
   - **`finalize_response()`** (main.rs): today called _after_ `route_request`
     returns, adding geo, version, staging, and operator headers. In the
     streaming path, this must run _before_ `stream_to_client()` since the
     publisher handler sends the response directly instead of returning it.
5. Remove `Content-Length` header — the final size is unknown after processing.
   Fastly's `StreamingBody` sends the response using chunked transfer encoding
   automatically.
6. Call `response.stream_to_client()` — headers sent to client immediately.
7. Pipe origin body through the streaming pipeline, writing chunks directly to
   `StreamingBody`.
8. Call `finish()` on success; on error, log and drop (client sees truncated
   response).

For binary/non-text content: call `response.take_body()` then stream via
`io::copy(&mut body, &mut streaming_body)`. The `Body` type implements `Read`
and `StreamingBody` implements `Write`, so this streams the backend body to the
client without buffering the full content. Today binary responses skip
`take_body()` and return the response as-is — the streaming path needs to
explicitly take the body to pipe it through.

#### Entry point change

Migrate `main.rs` from `#[fastly::main]` to an undecorated `main()` with
`Request::from_client()`. No separate initialization call is needed —
`#[fastly::main]` is just syntactic sugar for `Request::from_client()` +
`Response::send_to_client()`. The migration is required because
`stream_to_client()` / `send_to_client()` are incompatible with
`#[fastly::main]`'s return-based model.

```rust
fn main() {
    let req = Request::from_client();
    match handle(req) {
        Ok(()) => {}
        Err(e) => to_error_response(&e).send_to_client(),
    }
}
```

Note: the return type changes from `Result<Response, Error>` to `()` (or
`Result<(), Error>`). Errors that currently propagate to `main`'s `Result` must
now be caught explicitly and sent via `send_to_client()` with
`to_error_response()`.

Non-streaming routes (static, auction, discovery) use `send_to_client()` as
before.

## Data Flow

### Streaming path (HTML, text/JSON with processing)

```
Origin body (gzip)
  → Read 8KB chunk from GzDecoder
  → StreamProcessor::process_chunk(chunk, is_last)
      → HtmlRewriterAdapter: lol_html.write(chunk) → sink emits rewritten bytes
      → OR StreamingReplacer: URL replacement with overlap buffer
  → GzEncoder::write(processed_chunk) → compressed bytes
  → StreamingBody::write(compressed) → chunk sent to client
  → repeat until EOF
  → StreamingBody::finish()
```

Memory at steady state: ~8KB input chunk buffer, lol_html internal parser state,
gzip encoder window, and overlap buffer for replacer. Roughly constant regardless
of document size, versus the current ~4x document size.

### Pass-through path (binary, images, fonts, etc.)

```
Origin body (via take_body())
  → io::copy(&mut body, &mut streaming_body) → streamed transfer
  → StreamingBody::finish()
```

No decompression, no processing. Body streams through as read.

### Buffered fallback path (error responses or post-processors present)

```
Origin returns 4xx/5xx OR has_html_post_processors() is true
  → Current buffered path unchanged
  → send_to_client() with proper status and full body
```

## Error Handling

**Backend returns error status**: Detected before calling `stream_to_client()`.
Return the backend response as-is via `send_to_client()`. Client sees the
correct error status code. No change from current behavior.

**Processor creation fails**: `create_html_stream_processor()` or pipeline
construction errors happen _before_ `stream_to_client()` is called. Since
headers have not been sent yet, return a proper error response via
`send_to_client()`. Same as current behavior.

**Processing fails mid-stream**: `lol_html` parse error, decompression
corruption, I/O error during chunk processing. Headers (200 OK) are already
sent. Log the error server-side, drop the `StreamingBody`. Per the Fastly SDK,
`StreamingBody` automatically aborts the response if dropped without calling
`finish()` — the client sees a connection reset / truncated response. This is
standard reverse proxy behavior.

**Compression finalization fails**: The gzip trailer CRC32 write fails. With the
fix, `encoder.finish()` is called explicitly and errors propagate. Same
mid-stream handling — log and truncate.

No retry logic. No fallback to buffered after streaming has started — once
headers are sent, we are committed.

## Files Changed

| File                                                    | Change                                                                                                                                                                                                                                                     | Risk   |
| ------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| `crates/trusted-server-core/src/streaming_processor.rs` | Rewrite `HtmlRewriterAdapter` to stream incrementally (becomes single-use); convert all compression paths to chunk-based processing (`process_gzip_to_gzip` and `decompress_and_process`); fix `process_through_compression` to call `finish()` explicitly | High   |
| `crates/trusted-server-core/src/publisher.rs`           | Refactor `process_response_streaming` to accept `W: Write` instead of hardcoding `Vec<u8>`; split `handle_publisher_request` into streaming vs buffered paths; reorder synthetic ID/cookie logic before streaming                                          | Medium |
| `crates/trusted-server-adapter-fastly/src/main.rs`      | Migrate from `#[fastly::main]` to undecorated `main()` with `Request::from_client()`; explicit error handling via `to_error_response().send_to_client()`; call `finalize_response()` before streaming                                                      | Medium |

**Not changed**: `html_processor.rs` (builds lol_html `Settings` passed to
`HtmlRewriterAdapter`, works as-is), integration registration, JS build
pipeline, tsjs module serving, auction handler, cookie/synthetic ID logic.

Note: `HtmlWithPostProcessing` wraps `HtmlRewriterAdapter` and applies
post-processors on `is_last`. In the streaming path the post-processor list is
empty (that's the gate condition), so the wrapper is a no-op passthrough. It
remains in place — no need to bypass it.

Clarification: `script_rewriters` (used by Next.js and GTM) are distinct from
`html_post_processors`. Script rewriters run inside `lol_html` element handlers
during streaming and are now fragment-safe (resolved in
[Phase 3](#text-node-fragmentation-phase-3)). `html_post_processors` require
the full document for post-processing. The streaming gate checks
`has_html_post_processors()` for the post-processor path. Currently only
Next.js registers a post-processor.

## Text Node Fragmentation (Phase 3)

`lol_html` fragments text nodes across input chunk boundaries when processing
HTML incrementally. Script rewriters (`NextJsNextDataRewriter`,
`GoogleTagManagerIntegration`) expect complete text content — if a domain string
is split across chunks, the rewrite silently fails.

**Resolved in Phase 3**: Each script rewriter is now fragment-safe. They
accumulate text fragments internally via `Mutex<String>` until
`is_last_in_text_node` is true, then process the complete text. Intermediate
fragments return `RemoveNode` (suppressed from output); the final fragment
emits the full rewritten content via `Replace`. If no rewrite is needed,
the full accumulated content is still emitted via `Replace` (since
intermediate fragments were already removed from the output).

The `HtmlRewriterAdapter` buffered mode (`new_buffered()`) has been removed.
`create_html_processor` always uses the streaming adapter.

## Rollback Strategy

The `#[fastly::main]` to raw `main()` migration is a structural change. If
streaming causes issues in production, the fastest rollback is reverting the
`main.rs` change — the buffered path still exists and the pipeline improvements
(Step 1) are safe to keep regardless. No feature flag needed; a git revert of
the Step 2 commit restores buffered behavior while retaining Step 1 memory
improvements.

## Testing Strategy

### Unit tests (streaming_processor.rs)

- `HtmlRewriterAdapter` emits output on every `process_chunk()` call, not just
  `is_last`.
- `process_gzip_to_gzip` produces correct output without `read_to_end`.
- `encoder.finish()` errors propagate (not swallowed by `drop`).
- Multi-chunk HTML produces identical output to single-chunk processing.

### Integration tests (publisher.rs)

- Streaming gate: when `has_html_post_processors()` is true, response is
  buffered.
- Streaming gate: when `has_html_post_processors()` is false, response streams.
- Backend error (4xx/5xx) returns buffered error response with correct status.
- Binary content passes through without processing.

### End-to-end validation (Viceroy)

- `cargo test --workspace` — all existing tests pass.
- Manual verification via `fastly compute serve` against a real origin.
- Compare response bodies before/after to confirm byte-identical output for
  HTML, text, and binary.

### Performance measurement via Chrome DevTools MCP

Capture before/after metrics using Chrome DevTools MCP against Viceroy locally
and staging. Run each measurement set on `main` (baseline) and the feature
branch, then compare.

#### Baseline capture (before — on `main`)

1. Start local server: `fastly compute serve`
2. Navigate to publisher proxy URL via `navigate_page`
3. Capture network timing:
   - `list_network_requests` — record TTFB (`responseStart - requestStart`),
     total time (`responseEnd - requestStart`), and transfer size for the
     document request
   - Filter for the main document (`resourceType: Document`)
4. Run Lighthouse audit:
   - `lighthouse_audit` with categories `["performance"]`
   - Record TTFB, LCP, Speed Index, Total Blocking Time
5. Capture performance trace:
   - `performance_start_trace` → load page → `performance_stop_trace`
   - `performance_analyze_insight` — extract "Time to First Byte" and
     "Network requests" insights
6. Take memory snapshot:
   - `take_memory_snapshot` — record JS heap size as a secondary check
     (WASM heap is measured separately via Fastly dashboard)
7. Repeat 3-5 times for stable medians

#### Post-implementation capture (after — on feature branch)

Repeat the same steps on the feature branch. Compare:

| Metric             | Source                         | Expected change                                       |
| ------------------ | ------------------------------ | ----------------------------------------------------- |
| TTFB (document)    | Network timing                 | Minimal change (gated by backend response time)       |
| Time to last byte  | Network timing (`responseEnd`) | Reduced — body streams incrementally                  |
| LCP                | Lighthouse                     | Improved — browser receives `<head>` resources sooner |
| Speed Index        | Lighthouse                     | Improved — progressive rendering starts earlier       |
| Transfer size      | Network timing                 | Unchanged (same content, same compression)            |
| Response body hash | `evaluate_script` with hash    | Identical — correctness check                         |

#### Automated comparison script

Use `evaluate_script` to compute a response body hash in the browser for
correctness verification:

```js
// Run via evaluate_script after page load
const response = await fetch(location.href)
const buffer = await response.arrayBuffer()
const hash = await crypto.subtle.digest('SHA-256', buffer)
const hex = [...new Uint8Array(hash)]
  .map((b) => b.toString(16).padStart(2, '0'))
  .join('')
hex // compare this between baseline and feature branch
```

#### What to watch for

- **TTFB regression**: If TTFB increases, the header finalization reordering
  may be adding latency. Investigate `finalize_response()` and synthetic ID
  computation timing.
- **Body mismatch**: If response body hashes differ between baseline and
  feature branch, the streaming pipeline is producing different output.
  Bisect between Step 1 and Step 2 to isolate.
- **LCP unchanged**: If LCP doesn't improve, the `<head>` content may not be
  reaching the browser earlier. Check whether `lol_html` emits the `<head>`
  injection in the first chunk or buffers until more input arrives.

### Measurement (post-deploy to staging)

- Repeat Chrome DevTools MCP measurements against staging URL.
- Compare against Viceroy results to account for real network conditions.
- Monitor WASM heap usage via Fastly dashboard.
- Verify no regressions on static endpoints or auction.

### Results (getpurpose.ai, median over 5 runs, Chrome 1440x900)

Measured via Chrome DevTools Protocol against prod (v135, buffered) and
staging (v136, streaming). Chrome `--host-resolver-rules` used to route
`getpurpose.ai` to the staging Fastly edge (167.82.83.52).

| Metric                     | Production (v135, buffered) | Staging (v136, streaming) | Delta              |
| -------------------------- | --------------------------- | ------------------------- | ------------------ |
| **TTFB**                   | 54 ms                       | 35 ms                     | **-19 ms (-35%)**  |
| **First Paint**            | 186 ms                      | 160 ms                    | -26 ms (-14%)      |
| **First Contentful Paint** | 186 ms                      | 160 ms                    | -26 ms (-14%)      |
| **DOM Content Loaded**     | 286 ms                      | 282 ms                    | -4 ms (~same)      |
| **DOM Complete**           | 1060 ms                     | 663 ms                    | **-397 ms (-37%)** |

## Phase 4: Binary Pass-Through Streaming

Non-processable content (images, fonts, video, `application/octet-stream`)
currently passes through `handle_publisher_request` unchanged via the
`Buffered` path, buffering the entire body in memory before sending. For
large binaries (1-10 MB images), this is wasteful.

Phase 4 adds a `PublisherResponse::PassThrough` variant that signals the
adapter to stream the body directly via `io::copy` into `StreamingBody`
with no processing pipeline. This eliminates peak memory for binary
responses and improves DOM Complete for image-heavy pages.

### Streaming gate (updated)

```
is_success (2xx)
├── should_process && (!is_html || !has_post_processors) → Stream (pipeline)
├── should_process && is_html && has_post_processors     → Buffered (post-processors)
└── !should_process                                      → PassThrough (io::copy)

!is_success
└── any content type                                     → Buffered (error page)
```

### `PublisherResponse` enum (updated)

```rust
pub enum PublisherResponse {
    Buffered(Response),
    Stream { response, body, params },
    PassThrough { response, body },
}
```

`Content-Length` is preserved for `PassThrough` since the body is
unmodified — no need for chunked transfer encoding.
