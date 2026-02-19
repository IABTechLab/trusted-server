# Trusted Server Optimization Plan

## Status

| Item | Status |
|------|--------|
| Benchmark tooling (`scripts/benchmark.sh`) | **Implemented** |
| WASM guest profiling (`scripts/profile.sh`) | **Implemented** (flame graphs via `--profile-guest`) |
| Viceroy baseline measurements | **Complete** |
| Staging external TTFB baseline | **Complete** (against staging deployment) |
| Streaming architecture (`stream_to_client`) | **Planned** — see Phase 2 |
| Code-level optimizations | **Planned** — see Phase 1 |

---

## Key Finding: Streaming to Client IS Possible

The Fastly Compute SDK provides `Response::stream_to_client()` which returns a `StreamingBody` handle that implements `std::io::Write`. Headers are sent immediately and body chunks stream as they're written.

```rust
// Current: fully buffered (no bytes reach client until everything is done)
let body = response.take_body();
let mut output = Vec::new();
pipeline.process(body, &mut output)?;    // blocks until complete
response.set_body(Body::from(output));   // only NOW does client get anything
return Ok(response);

// Possible: streaming (headers sent immediately, body chunks as processed)
let body = response.take_body();
let mut streaming = response.stream_to_client();  // headers sent NOW
pipeline.process(body, &mut streaming)?;           // each write() → client
streaming.finish()?;
```

This changes the optimization strategy — **time-to-last-byte (TTLB) and peak memory CAN be significantly reduced**. TTFB itself is still gated by the Fastly platform floor (~200ms) plus backend response time, but body bytes start reaching the client as soon as the first chunk is processed instead of waiting for the entire response to be buffered.

### Compatibility with `#[fastly::main]` — use undecorated `main()` (recommended)

For streaming final responses, the Fastly SDK docs already define the intended pattern:

- `Request::from_client()` docs explicitly state it is incompatible with `#[fastly::main]` and recommend an undecorated `main()` with explicit response sending.
- `Response::send_to_client()` / `Response::stream_to_client()` include the same compatibility guidance.
- `fastly::init()` is public (doc-hidden) and can be called from raw `main()` to initialize the ABI.

This means approach #1 is the correct architecture for streaming paths, and approaches like `std::process::exit(0)` or sentinel responses are unnecessary.

Recommended shape:

```rust
fn main() -> Result<(), fastly::Error> {
    fastly::init();
    let req = fastly::Request::from_client();

    match route_request(req)? {
        Some(resp) => resp.send_to_client(), // non-streaming path
        None => {}                           // streaming path already sent + finished
    }

    Ok(())
}
```

**Action item**: Do a focused spike on real Fastly Compute to validate runtime behavior (no double-send panics across mixed routes, proper error behavior for partially streamed responses, and observability expectations). The API viability question is resolved.

Non-streaming endpoints (static JS, discovery, auction) continue returning `Response` normally. Only the publisher proxy path (the hot path) would use streaming.

---

## How to Use This Document

**For any optimization work:**

1. Run `./scripts/benchmark.sh --save baseline` on `main`
2. Make your change on a branch
3. Rebuild: `fastly compute build`
4. Run `./scripts/benchmark.sh --save branch-name`
5. Compare: `diff benchmark-results/baseline.txt benchmark-results/branch-name.txt`
6. For production: `BENCH_URL=https://your-staging.edgecompute.app ./scripts/benchmark.sh --profile`
7. If the numbers don't improve meaningfully, don't ship it

---

## Baseline Measurements

### Viceroy (Local Simulator)

Measured on `main` branch. Value is in **relative comparison between branches**, not absolute values.

| Endpoint | P50 | P95 | Req/sec | Notes |
|---|---|---|---|---|
| `GET /static/tsjs=tsjs-unified.min.js` | 1.9 ms | 3.1 ms | 4,672 | Pure WASM, no backend |
| `GET /.well-known/trusted-server.json` | 1.3 ms | 1.4 ms | ~770 | Server-side only |
| `GET /` (publisher proxy) | 400 ms | 595 ms | 21 | Proxies to golf.com, 222KB HTML |
| `POST /auction` | 984 ms | 1,087 ms | 9.3 | Calls Prebid + APS backends |

- **WASM heap**: 3.0-4.1 MB per request
- **Init overhead**: <2ms (settings parse + orchestrator + registry)
- **No cold start pattern** detected in Viceroy

### Staging (External)

Measured externally against staging deployment (golf.com proxy), `main` branch.

| Endpoint | TTFB | Total | Size | Notes |
|---|---|---|---|---|
| `GET /static/tsjs=tsjs-unified.min.js` | ~204 ms | ~219 ms | 28 KB | No backend; includes client-network + edge path from benchmark vantage |
| `GET /` (publisher proxy, golf.com) | ~234 ms | ~441 ms | 230 KB | Backend + processing |
| `GET /.well-known/trusted-server.json` | ~191 ms | - | - | Returns 500 (needs investigation) |

**Key insight**: Static JS has ~204ms TTFB with zero backend work **from this specific benchmark vantage point**. That number includes client-to-edge RTT, DNS, TLS/connection state, and edge processing; it is **not** a universal Fastly floor. `WASM` instantiation can contribute on cold paths, but warm requests from clients near a POP can be much lower.

For this dataset, treat static TTFB as an environment baseline and compare deltas: the publisher proxy adds only ~30ms TTFB on top. The larger optimization target is the TTFB→TTLB gap (~207ms here), which streaming can shrink by sending body chunks as they are processed instead of waiting for full buffering.

---

## Implementation Plan

### Phase 0: Tooling and Baselines (DONE)

**Branch**: `feat/optimize-ts`

Completed:
- `scripts/benchmark.sh` — HTTP load testing with TTFB analysis, cold start detection, endpoint latency breakdown
- `scripts/profile.sh` — WASM guest profiling via `fastly compute serve --profile-guest`, outputs Firefox Profiler-compatible flame graphs
- Viceroy baseline measurements (see tables above)
- Staging external TTFB baseline

---

### Phase 1: Low-Risk Code Optimizations

These are small, safe changes that reduce CPU and memory waste. Ship as one PR, measure before/after.

#### 1.1 Fix gzip streaming — remove full-body buffering

**File**: `crates/common/src/streaming_processor.rs` — `process_gzip_to_gzip`

**Problem**: Reads entire decompressed body into memory via `read_to_end`, despite deflate/brotli paths already using chunk-based `process_through_compression`.

**Fix**: 3 lines — use `process_through_compression` like deflate/brotli:

```rust
fn process_gzip_to_gzip<R: Read, W: Write>(&mut self, input: R, output: W) -> Result<...> {
    let decoder = GzDecoder::new(input);
    let encoder = GzEncoder::new(output, Compression::default());
    self.process_through_compression(decoder, encoder)
}
```

| Impact | LOC | Risk |
|--------|-----|------|
| **High** (most responses are gzip; reduces peak memory) | -15/+3 | Low |

#### 1.2 Fix `HtmlRewriterAdapter` — enable true streaming

**File**: `crates/common/src/streaming_processor.rs` — `HtmlRewriterAdapter`

**Problem**: Accumulates entire HTML document before processing, defeating the streaming pipeline. The comment says this is a `lol_html` limitation — **it's not**. `lol_html::HtmlRewriter` supports incremental `write()` calls and emits output via its `OutputSink` callback per-chunk.

**Fix**: Create the `HtmlRewriter` eagerly in `new()`, use `Rc<RefCell<Vec<u8>>>` via the public `lol_html::OutputSink` trait to share the output buffer:

```rust
struct RcVecSink(Rc<RefCell<Vec<u8>>>);

impl lol_html::OutputSink for RcVecSink {
    fn handle_chunk(&mut self, chunk: &[u8]) {
        self.0.borrow_mut().extend_from_slice(chunk);
    }
}

pub struct HtmlRewriterAdapter {
    rewriter: Option<lol_html::HtmlRewriter<'static, RcVecSink>>,
    output: Rc<RefCell<Vec<u8>>>,
}

impl StreamProcessor for HtmlRewriterAdapter {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        if let Some(rewriter) = &mut self.rewriter {
            if !chunk.is_empty() {
                rewriter.write(chunk)?;
            }
        }
        if is_last {
            if let Some(rewriter) = self.rewriter.take() {
                rewriter.end()?;
            }
        }
        // Drain whatever lol_html produced
        Ok(std::mem::take(&mut *self.output.borrow_mut()))
    }
}
```

| Impact | LOC | Risk |
|--------|-----|------|
| **High** (HTML is most common content type; eliminates 222KB+ buffer) | ~30 refactored | Medium — needs test coverage |

#### 1.3 Reduce verbose per-request logging

**Files**: `crates/fastly/src/main.rs:37,64-67,152-177`

**Problem**: `log::info!("Settings {settings:?}")` serializes the entire Settings struct (~2KB) on every request. `FASTLY_SERVICE_VERSION` env var logged at info level. The logger is configured with `max_level(LevelFilter::Debug)`, meaning every `debug!` and above is evaluated.

**Fix**: Downgrade the Settings dump to `log::debug!` and tighten the logger's `max_level` to `LevelFilter::Info` for production. The `log_fastly` crate supports `filter_module()` for per-module levels if we still want debug output from specific modules. When the level is filtered, `log` macros short-circuit before evaluating arguments — so the `Settings` `Debug` format is never even computed.

```rust
// Before: everything at Debug and above is serialized
.max_level(log::LevelFilter::Debug)

// After: Info in production, debug only for specific modules if needed
.max_level(log::LevelFilter::Info)
// Optional: .filter_module("trusted_server", log::LevelFilter::Debug)
```

| Impact | LOC | Risk |
|--------|-----|------|
| Medium-High | ~3 | None |

#### 1.4 Eliminate redundant `config` crate parsing in `get_settings()` — **22% CPU**

**Files**: `crates/common/src/settings_data.rs`, `crates/common/src/settings.rs`

**Problem**: Flame graph profiling shows `get_settings()` consuming ~22% of per-request CPU. The `build.rs` already merges `trusted-server.toml` + all `TRUSTED_SERVER__*` env vars at compile time and writes a fully-resolved TOML file to `target/trusted-server-out.toml`. But at runtime, `get_settings()` calls `Settings::from_toml()`, which re-runs the entire `config` crate pipeline — `Config::builder().add_source(File).add_source(Environment).build().try_deserialize()` — redundantly scanning env vars and merging sources that were already resolved at build time.

**Root cause**: `settings_data.rs` embeds the build-time-resolved TOML via `include_bytes!`, then hands it to `from_toml()` which treats it as a raw config source and re-layers env vars on top.

**Fix**: Replace `Settings::from_toml()` with direct `toml::from_str()` in `get_settings()`. The embedded TOML is already fully resolved — no `config` crate needed at runtime.

```rust
// Before (22% CPU — re-runs config crate pipeline + env var scan)
let settings = Settings::from_toml(toml_str)?;

// After (near-instant — just TOML deserialization)
let settings: Settings = toml::from_str(toml_str)
    .change_context(TrustedServerError::Configuration {
        message: "Failed to deserialize embedded config".to_string(),
    })?;
```

**Alternative — binary serialization for near-zero cost**: Since `build.rs` already has a fully constructed `Settings` struct, it could serialize to `postcard` (a `no_std`-compatible, WASM-safe binary format). Runtime deserialization becomes a memcpy-like operation instead of TOML parsing. Requires adding `postcard` + updating `build.rs` to write binary and `settings_data.rs` to deserialize binary.

```rust
// build.rs: serialize to binary instead of TOML
let bytes = postcard::to_allocvec(&settings).expect("Failed to serialize");
fs::write(dest_path, bytes)?;

// settings_data.rs: near-instant deserialization
let settings: Settings = postcard::from_bytes(SETTINGS_DATA)
    .change_context(TrustedServerError::Configuration { ... })?;
```

**Recommendation**: Start with the `toml::from_str()` fix (1-line change, no new deps). If profiling still shows meaningful time in TOML parsing, upgrade to `postcard`.

| Impact | LOC | Risk |
|--------|-----|------|
| **Very High** (~22% CPU eliminated) | 1-3 | Low — `build.rs` already resolves everything |

#### 1.5 Trivial fixes batch

| Fix | File | LOC |
|-----|------|-----|
| Const cookie prefix instead of `format!()` | `publisher.rs:207-210` | 2 |
| `mem::take` instead of `clone` for overlap buffer | `streaming_replacer.rs:63` | 1 |
| `eq_ignore_ascii_case` for compression detection | `streaming_processor.rs:47` | 5 |
| `Cow<str>` for string replacements | `streaming_replacer.rs:120-125` | 5-10 |
| Remove base64 roundtrip in token computation | `http_util.rs:286-294` | 10-15 |
| Replace Handlebars with manual interpolation | `synthetic.rs:82-99` | ~20 |
| Cache `origin_host()` result per-request | `settings.rs` | 5-10 |

---

### Phase 2: Streaming Response Architecture

This is the high-impact architectural change. Uses Fastly's `stream_to_client()` API to send response headers and body chunks to the client as they're processed, instead of buffering everything.

#### 2.1 Publisher proxy: `stream_to_client()` integration

**Files**: `crates/common/src/publisher.rs`, `crates/fastly/src/main.rs`

**Current flow** (fully buffered):
```
req.send() → wait for full response → take_body()
  → process_response_streaming() → collects into Vec<u8>
  → Body::from(output) → return complete Response
```

**New flow** (streaming):
```
req.send() → take_body() → set response headers
  → stream_to_client() → returns StreamingBody (headers sent immediately)
  → pipeline.process(body, &mut streaming_body) → chunks written to client as processed
  → streaming_body.finish()
```

**Key enablers**:
- `StreamingPipeline.process()` already accepts `W: Write` — `StreamingBody` implements `Write`
- With Phase 1 fixes (gzip streaming + HTML rewriter streaming), the pipeline is already chunk-based
- Non-text responses can use `streaming_body.append(body)` for O(1) pass-through

**Architecture change in `main.rs`**: The publisher proxy path calls `stream_to_client()` directly instead of returning a `Response`. Other endpoints (static, auction, discovery) continue returning `Response` as before.

**Error handling for streaming**: Once `stream_to_client()` is called, response headers (including status 200) are already sent. If processing fails mid-stream:
- We cannot change the status code — the client already received 200
- The `StreamingBody` will be aborted on drop (client sees incomplete response)
- We should log the error server-side for debugging
- This is the same trade-off every streaming proxy makes (nginx, Cloudflare Workers, etc.)

To mitigate: validate backend response status and content-type **before** calling `stream_to_client()`. If the backend returns an error, fall back to the buffered path to return a proper error response.

```rust
// Fetch from backend (blocks for full response including headers)
let mut backend_resp = req.send(&backend)?;

// Check backend status BEFORE committing to streaming
if !backend_resp.get_status().is_success() || !should_process_content_type(&backend_resp) {
    // Buffered path — can return proper error/pass-through response
    return Ok(backend_resp);
}

// Commit to streaming — headers sent to client NOW
let backend_body = backend_resp.take_body();
let mut client_body = backend_resp.stream_to_client();

// Process chunks — errors logged but response is already in flight
match pipeline.process(backend_body, &mut client_body) {
    Ok(()) => client_body.finish()?,
    Err(e) => {
        log::error!("Streaming processing failed: {:?}", e);
        // StreamingBody dropped → client sees truncated response
        // This is the best we can do after headers are sent
    }
}
```

| Impact | LOC | Risk |
|--------|-----|------|
| **High** — reduces time-to-last-byte and peak memory for all proxied pages | ~80-120 | Medium — error handling requires careful design |

#### 2.2 Concurrent origin fetch + auction (future)

**Not applicable for golf.com** (no on-page auction), but for publishers with auction.

The idea: use `req.send_async()` to launch the origin fetch concurrently with auction backend calls (which already use `fastly::http::request::select()` internally). When the origin response arrives, start streaming it to the client via `stream_to_client()`. When the lol_html rewriter reaches the ad injection point in the HTML, check if auction results are available.

This would overlap origin fetch time with auction execution, so the browser starts receiving `<head>` content (CSS, fonts) while the auction is still running.

**Note**: This requires significant refactoring of the auction orchestrator and HTML processor to support async injection. The pseudo-code in the teammate's proposal (`origin_pending.poll()`, `run_auction_async`) represents the desired architecture but these APIs don't exist yet and would need to be built.

| Impact | LOC | Risk |
|--------|-----|------|
| **Very High** for auction pages — browser starts loading ~400ms earlier | ~150-200 | High — complex coordination |

---

### Phase 3: Measure and Validate

After implementing Phases 1-2:

1. Deploy to staging
2. Run `./scripts/benchmark.sh` against staging for external TTFB/TTLB
3. Run `./scripts/profile.sh` locally for flame graph comparison
4. Compare external TTFB and time-to-last-byte before vs after
5. Check Fastly dashboard for memory/compute metrics
6. If improvement is marginal, don't ship the streaming architecture (Phase 2)

**Success criteria**:
- Peak memory per request reduced by 30%+ (measurable via Fastly logs)
- Time-to-last-byte reduced for large HTML pages
- No regression on static endpoints or auction
- Code complexity is justified by measured improvement

---

## Optimization Summary Table

| # | Optimization | Impact | LOC | Risk | Phase |
|---|---|---|---|---|---|
| **P0** | Tooling and baselines | Prerequisite | Done | None | 0 |
| **1.1** | Gzip streaming fix | **High** (memory) | -15/+3 | Low | 1 |
| **1.2** | HTML rewriter streaming | **High** (memory) | ~30 | Medium | 1 |
| **1.3** | Remove verbose logging | Medium-High | ~3 | None | 1 |
| **1.4** | Eliminate redundant `config` crate in `get_settings()` | **Very High** (~22% CPU) | 1-3 | Low | 1 |
| **1.5** | Trivial fixes batch | Low-Medium | ~50 | None | 1 |
| **2.1** | `stream_to_client()` integration | **High** (TTLB) | ~80-120 | Medium | 2 |
| **2.2** | Concurrent origin + auction | **Very High** | ~150-200 | High | 2 (future) |

---

## Architecture: Current vs Target

### Current (fully buffered)

```
Client → Fastly Edge → [WASM starts]
  → init (settings, orchestrator, registry)     ~1ms
  → req.send(backend)                           blocks for full response
  → response.take_body()                        full body in memory
  → GzDecoder.read_to_end()                     full decompressed in memory
  → HtmlRewriterAdapter accumulates all input   full HTML in memory
  → lol_html processes entire document           full output in memory
  → GzEncoder.write_all()                       full recompressed in memory
  → Body::from(output)                          Response constructed
  → return Response                             NOW client gets first byte
```

**Memory**: compressed + decompressed + processed + recompressed = ~4x response size
**TTLB**: cannot send any bytes until all processing is complete

### Target (streaming)

```
Client → Fastly Edge → [WASM starts]
  → init (settings, orchestrator, registry)     ~1ms
  → req.send(backend)                           blocks for full response (same as current)
  → response.take_body()                        body available as Read stream
  → validate status, set response headers
  → stream_to_client()                          headers sent to client NOW
  → GzDecoder.read(8KB chunk)                   8KB decompressed
  → HtmlRewriter.write(chunk)                   output emitted via callback
  → GzEncoder.write(processed)                  compressed chunk
  → StreamingBody.write(chunk)                  chunk sent to client
  → ... repeat for each chunk ...
  → StreamingBody.finish()                      done
```

**Memory**: ~8KB chunk buffer + lol_html internal state (significantly less than 4x response size — exact savings need measurement)
**TTLB**: client receives first body bytes after first processed chunk, instead of waiting for all processing to complete. For a 222KB page, the savings is the entire processing time (decompression + rewriting + recompression).

---

## Benchmarking Setup

### Prerequisites

```bash
brew install hey    # HTTP load testing tool (auto-installed by benchmark.sh)
```

### Available Modes

```bash
./scripts/benchmark.sh                    # Full benchmark suite
./scripts/benchmark.sh --quick            # Quick smoke test
./scripts/benchmark.sh --ttfb             # TTFB analysis only
./scripts/benchmark.sh --load-test        # Load test only
./scripts/benchmark.sh --cold-start       # Cold start analysis
./scripts/benchmark.sh --save baseline    # Save results to file
./scripts/benchmark.sh --compare baseline # Compare against saved results
```

### WASM Guest Profiling (Flame Graphs)

`fastly compute serve --profile-guest` samples the WASM call stack every 50us and writes a Firefox Profiler-compatible JSON on exit. This shows exactly which Rust functions consume CPU time — compression, HTML rewriting, string operations, init, etc.

```bash
./scripts/profile.sh                           # Profile GET / (publisher proxy)
./scripts/profile.sh --endpoint /auction \
    --method POST --body '{"adUnits":[]}'      # Profile specific endpoint
./scripts/profile.sh --requests 50             # More samples for stable flame graph
./scripts/profile.sh --no-build                # Skip rebuild
./scripts/profile.sh --open                    # Auto-open Firefox Profiler (macOS)

# View: drag output file onto https://profiler.firefox.com/
```

The script builds, starts the profiling server, fires requests, stops the server, and saves the profile to `benchmark-results/profiles/`.

### What the Tools Measure

| Tool | What it tells you |
|---|---|
| `benchmark.sh` — TTFB analysis | 20 sequential requests — detects cold start patterns |
| `benchmark.sh` — Cold start | First vs subsequent request latency |
| `benchmark.sh` — Endpoint latency | Per-endpoint timing breakdown (DNS, connect, TTFB, total) |
| `benchmark.sh` — Load test (hey) | Throughput (req/sec), latency distribution (P50/P95/P99) |
| `profile.sh` | Per-function CPU time inside WASM — flame graph via `--profile-guest` |

**Use `profile.sh` first** to identify which functions are bottlenecks, then use `benchmark.sh` to measure the impact of fixes on external timing.

### What These Tools Do NOT Measure

- Real Fastly edge performance (Viceroy is a simulator)
- WASM cold start on actual Fastly infrastructure
- Production TLS handshake overhead
- Memory usage (use Fastly dashboard or Viceroy logs)

---

## Notes for Team

### What's already on `feat/optimize-ts` branch (uncommitted)

| File | Change |
|------|--------|
| `scripts/benchmark.sh` | HTTP load testing, TTFB analysis, cold start detection, auto-install `hey` |
| `scripts/profile.sh` | WASM guest profiling via `--profile-guest`, flame graph workflow |
| `OPTIMIZATION.md` | This document |

### Teammate's `streaming_processor.rs` Changes

A teammate has prepared changes to `streaming_processor.rs` that address items 1.1 and 1.2:

- **Gzip fix**: `process_gzip_to_gzip` now uses `process_through_compression` (3-line change)
- **HTML rewriter fix**: `HtmlRewriterAdapter` rewritten to use `lol_html::OutputSink` trait with `Rc<RefCell<Vec<u8>>>` for incremental streaming

**Review notes on the HTML rewriter change**:
- `lol_html::OutputSink` is a public trait (verified in lol_html 2.7.1)
- The `Rc<RefCell>` pattern is necessary because `HtmlRewriter::new()` takes ownership of the sink, but we need to read output in `process_chunk()`
- `Option<HtmlRewriter>` with `.take()` is correct — `end()` consumes self
- The adapter is no longer reusable after `end()` — one per document, which matches actual usage
- Tests correctly updated to collect output across all chunks

**Correctness issue — must fix in same PR**: `process_through_compression` uses `drop(encoder)` for finalization. For `GzEncoder`, `Drop` calls `finish()` internally but **silently ignores errors**. The gzip trailer contains a CRC32 checksum — if `finish()` fails, corrupted gzip responses are served to clients without any error being reported. This is a pre-existing issue (deflate/brotli have the same `drop()` pattern) but it **must be fixed** when gzip moves to this code path, since gzip is the most common encoding.

Fix: change `process_through_compression` to accept an optional finalization closure, or add a separate `process_gzip_to_gzip` that calls `encoder.finish()` explicitly after `process_through_compression`-style chunk loop.

### Decisions Needed

1. **Raw `main()` migration spike** — Validate end-to-end behavior on Fastly Compute when using undecorated `main()` + `Request::from_client()` and mixing buffered + streaming routes in one service.
2. **Phase 1 vs Phase 2 priority** — Phase 1 (code fixes) is low risk and can ship independently. Phase 2 (streaming architecture) is higher impact and should proceed after decision #1 confirms runtime behavior.
3. **Concurrent auction + origin (2.2)** — Not applicable for golf.com. Defer to a separate ticket?
4. **GzEncoder `finish()` correctness** — Fix the `drop(encoder)` error swallowing in `process_through_compression`, or accept the risk?
