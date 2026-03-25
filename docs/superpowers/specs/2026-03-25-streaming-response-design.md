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
text/JSON, and binary pass-through. Only when Next.js is disabled (no
post-processor requiring the full document).

**Out of scope**: Concurrent origin+auction fetch, Next.js-enabled paths (these
require full-document post-processing by design), non-publisher routes (static
JS, auction, discovery).

## Streaming Gate

Before committing to `stream_to_client()`, check:

1. Backend status is success (2xx).
2. `html_post_processors()` is empty — no registered post-processors.

If either check fails, fall back to the current buffered path. This keeps the
optimization transparent: same behavior for all existing configurations,
streaming only activates when safe.

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

#### B) `process_gzip_to_gzip` — chunk-based decompression

Currently calls `read_to_end()` to decompress the entire body into memory. The
deflate and brotli paths already use the chunk-based
`process_through_compression()`.

Fix: use the same `process_through_compression` pattern for gzip.

#### C) `process_through_compression` finalization

Currently uses `drop(encoder)` which silently swallows errors from the gzip
trailer CRC32 checksum.

Fix: call `encoder.finish()` explicitly and propagate errors.

### Step 2: Stream response to client

Change the publisher proxy path to use Fastly's `StreamingBody` API:

1. Fetch from origin, receive response headers.
2. Validate status — if backend error, return buffered error response via
   `send_to_client()`.
3. Check streaming gate — if `html_post_processors()` is non-empty, fall back
   to buffered path.
4. Finalize all response headers (cookies, synthetic ID, geo, version).
5. Call `response.stream_to_client()` — headers sent to client immediately.
6. Pipe origin body through the streaming pipeline, writing chunks directly to
   `StreamingBody`.
7. Call `finish()` on success; on error, log and drop (client sees truncated
   response).

For binary/non-text content: use `StreamingBody::append(body)` for zero-copy
pass-through, bypassing the pipeline entirely.

#### Entry point change

Migrate `main.rs` from `#[fastly::main]` to raw `main()` with `fastly::init()`
+ `Request::from_client()`. This is required because `stream_to_client()` /
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
+ gzip encoder window + overlap buffer for replacer. Roughly constant regardless
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

**Processing fails mid-stream**: `lol_html` parse error, decompression
corruption, I/O error. Headers (200 OK) are already sent. Log the error
server-side, drop the `StreamingBody`. Client sees a truncated response and the
connection closes. Standard reverse proxy behavior.

**Compression finalization fails**: The gzip trailer CRC32 write fails. With the
fix, `encoder.finish()` is called explicitly and errors propagate. Same
mid-stream handling — log and truncate.

No retry logic. No fallback to buffered after streaming has started — once
headers are sent, we are committed.

## Files Changed

| File | Change | Risk |
|------|--------|------|
| `crates/trusted-server-core/src/streaming_processor.rs` | Rewrite `HtmlRewriterAdapter` to stream incrementally; fix `process_gzip_to_gzip` to use chunk-based processing; fix `process_through_compression` to call `finish()` explicitly | Medium |
| `crates/trusted-server-core/src/publisher.rs` | Split `handle_publisher_request` into streaming vs buffered paths based on `html_post_processors().is_empty()` | Medium |
| `crates/trusted-server-adapter-fastly/src/main.rs` | Migrate from `#[fastly::main]` to raw `main()` with `fastly::init()` + `Request::from_client()`; route results to `send_to_client()` or let streaming path handle its own output | Medium |

**Not changed**: `html_processor.rs` (builds lol_html `Settings` passed to
`HtmlRewriterAdapter`, works as-is), integration registration, JS build
pipeline, tsjs module serving, auction handler, cookie/synthetic ID logic.

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
