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
2. For HTML content: `html_post_processors()` is empty — no registered
   post-processors. Non-HTML content types (text/JSON, RSC Flight, binary) can
   always stream regardless of post-processor registration, since
   post-processors only apply to HTML.

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

#### B) `process_gzip_to_gzip` — chunk-based decompression

Currently calls `read_to_end()` to decompress the entire body into memory. The
deflate and brotli paths already use the chunk-based
`process_through_compression()`.

Fix: use the same `process_through_compression` pattern for gzip.

#### C) `process_through_compression` finalization — prerequisite for B

`process_through_compression` currently uses `drop(encoder)` which silently
swallows errors. For gzip specifically, the trailer contains a CRC32 checksum —
if `finish()` fails, corrupted responses are served silently. Today this affects
deflate and brotli (which already use `process_through_compression`); after Step
1B moves gzip to this path, it will affect gzip too.

Fix: call `encoder.finish()` explicitly and propagate errors. This must land
before or with Step 1B.

### Step 2: Stream response to client

Change the publisher proxy path to use Fastly's `StreamingBody` API:

1. Fetch from origin, receive response headers.
2. Validate status — if backend error, return buffered error response via
   `send_to_client()`.
3. Check streaming gate — if `html_post_processors()` is non-empty, fall back
   to buffered path.
4. Finalize all response headers (cookies, synthetic ID, geo, version).
   Today, synthetic ID/cookie headers are set _after_ body processing in
   `handle_publisher_request`. Since they are body-independent (computed from
   request cookies and consent context), they must be reordered to run _before_
   `stream_to_client()` so headers are complete before streaming begins.
5. Remove `Content-Length` header — the final size is unknown after processing.
   Fastly's `StreamingBody` sends the response using chunked transfer encoding
   automatically.
6. Call `response.stream_to_client()` — headers sent to client immediately.
7. Pipe origin body through the streaming pipeline, writing chunks directly to
   `StreamingBody`.
8. Call `finish()` on success; on error, log and drop (client sees truncated
   response).

For binary/non-text content: call `response.take_body()` then
`StreamingBody::append(body)` for zero-copy pass-through, bypassing the pipeline
entirely. Today binary responses skip `take_body()` and return the response
as-is — the streaming path needs to explicitly take the body to hand it to
`append()`.

#### Entry point change

Migrate `main.rs` from `#[fastly::main]` to raw `main()` with `fastly::init()`
\+ `Request::from_client()`. This is required because `stream_to_client()` /
`send_to_client()` are incompatible with `#[fastly::main]`'s return-based model.

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

Memory at steady state: ~8KB input chunk buffer + lol_html internal parser state
\+ gzip encoder window + overlap buffer for replacer. Roughly constant regardless
of document size, versus the current ~4x document size.

### Pass-through path (binary, images, fonts, etc.)

```
Origin body
  → StreamingBody::append(body) → zero-copy transfer
```

No decompression, no processing, no buffering.

### Buffered fallback path (error responses or post-processors present)

```
Origin returns 4xx/5xx OR html_post_processors() is non-empty
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
sent. Log the error server-side, drop the `StreamingBody`. Client sees a
truncated response and the connection closes. Standard reverse proxy behavior.

**Compression finalization fails**: The gzip trailer CRC32 write fails. With the
fix, `encoder.finish()` is called explicitly and errors propagate. Same
mid-stream handling — log and truncate.

No retry logic. No fallback to buffered after streaming has started — once
headers are sent, we are committed.

## Files Changed

| File                                                    | Change                                                                                                                                                                                                | Risk   |
| ------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| `crates/trusted-server-core/src/streaming_processor.rs` | Rewrite `HtmlRewriterAdapter` to stream incrementally (becomes single-use); fix `process_gzip_to_gzip` to use chunk-based processing; fix `process_through_compression` to call `finish()` explicitly | High   |
| `crates/trusted-server-core/src/publisher.rs`           | Refactor `process_response_streaming` to accept `W: Write` instead of hardcoding `Vec<u8>`; split `handle_publisher_request` into streaming vs buffered paths; reorder synthetic ID/cookie logic before streaming | Medium |
| `crates/trusted-server-adapter-fastly/src/main.rs`      | Migrate from `#[fastly::main]` to raw `main()` with `fastly::init()` + `Request::from_client()`; route results to `send_to_client()` or let streaming path handle its own output                      | Medium |

**Not changed**: `html_processor.rs` (builds lol_html `Settings` passed to
`HtmlRewriterAdapter`, works as-is), integration registration, JS build
pipeline, tsjs module serving, auction handler, cookie/synthetic ID logic.

Note: `HtmlWithPostProcessing` wraps `HtmlRewriterAdapter` and applies
post-processors on `is_last`. In the streaming path the post-processor list is
empty (that's the gate condition), so the wrapper is a no-op passthrough. It
remains in place — no need to bypass it.

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

- Streaming gate: when `html_post_processors()` is non-empty, response is
  buffered.
- Streaming gate: when `html_post_processors()` is empty, response streams.
- Backend error (4xx/5xx) returns buffered error response with correct status.
- Binary content passes through without processing.

### End-to-end validation (Viceroy)

- `cargo test --workspace` — all existing tests pass.
- Manual verification via `fastly compute serve` against a real origin.
- Compare response bodies before/after to confirm byte-identical output for
  HTML, text, and binary.

### Measurement (post-deploy)

- Compare TTFB and time-to-last-byte on staging before and after.
- Monitor WASM heap usage via Fastly dashboard.
- Verify no regressions on static endpoints or auction.
