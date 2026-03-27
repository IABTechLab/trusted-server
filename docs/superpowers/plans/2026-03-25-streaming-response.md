# Streaming Response Optimization — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stream HTTP responses through the publisher proxy instead of buffering
them, reducing peak memory from ~4x response size to constant and improving
time-to-last-byte.

**Architecture:** Two independent phases. Phase 1 makes the internal streaming
pipeline truly chunk-emitting (HtmlRewriterAdapter, compression paths, encoder
finalization). Phase 2 wires up Fastly's `StreamingBody` API so processed chunks
flow directly to the client. Each phase is shippable independently.

**Tech Stack:** Rust 1.91.1, Fastly Compute SDK 0.11.12
(`stream_to_client`/`send_to_client`/`StreamingBody`), `lol_html` (HTML
rewriting), `flate2` (gzip/deflate), `brotli` (brotli compression).

**Spec:** `docs/superpowers/specs/2026-03-25-streaming-response-design.md`
**Issue:** #563

---

## File Map

| File                                                    | Role                                                                                   | Phase |
| ------------------------------------------------------- | -------------------------------------------------------------------------------------- | ----- |
| `crates/trusted-server-core/src/streaming_processor.rs` | `HtmlRewriterAdapter` rewrite, compression path fixes, encoder finalization            | 1     |
| `crates/trusted-server-core/src/publisher.rs`           | `process_response_streaming` refactor to `W: Write`, streaming gate, header reordering | 2     |
| `crates/trusted-server-adapter-fastly/src/main.rs`      | Entry point migration from `#[fastly::main]` to raw `main()`, response routing         | 2     |

---

## Phase 1: Make the Pipeline Chunk-Emitting

> **Implementation note (2026-03-26):** Tasks 1-3 were implemented as planned,
> then followed by a refactor that unified all 9 `process_*_to_*` methods into
> a single `process_chunks` method with inline decoder/encoder creation in
> `process()`. This eliminated ~150 lines of duplication. The refactor was
> committed as "Unify compression paths into single process_chunks method".
> Tasks 1-3 descriptions below reflect the original plan; the final code is
> cleaner than described.

### Task 1: Fix encoder finalization in `process_through_compression`

This is the prerequisite for Task 2. The current code calls `flush()` then
`drop(encoder)`, silently swallowing finalization errors. Must fix before
moving gzip to this path.

**Files:**

- Modify: `crates/trusted-server-core/src/streaming_processor.rs:334-393`
- Test: `crates/trusted-server-core/src/streaming_processor.rs` (test module)

- [ ] **Step 1: Write a test verifying deflate round-trip correctness**

Add to the `#[cfg(test)]` module at the bottom of
`streaming_processor.rs`:

```rust
#[test]
fn test_deflate_round_trip_produces_valid_output() {
    // Verify that deflate-to-deflate (which uses process_through_compression)
    // produces valid output that decompresses correctly. This establishes the
    // correctness contract before we change the finalization path.
    use flate2::read::ZlibDecoder;
    use flate2::write::ZlibEncoder;

    let input_data = b"<html><body>hello world</body></html>";

    // Compress input
    let mut compressed_input = Vec::new();
    {
        let mut enc = ZlibEncoder::new(&mut compressed_input, flate2::Compression::default());
        enc.write_all(input_data)
            .expect("should compress test input");
        enc.finish().expect("should finish compression");
    }

    let replacer = StreamingReplacer::new(vec![Replacement {
        find: "hello".to_string(),
        replace_with: "hi".to_string(),
    }]);

    let config = PipelineConfig {
        input_compression: Compression::Deflate,
        output_compression: Compression::Deflate,
        chunk_size: 8192,
    };

    let mut pipeline = StreamingPipeline::new(config, replacer);
    let mut output = Vec::new();

    pipeline
        .process(&compressed_input[..], &mut output)
        .expect("should process deflate-to-deflate");

    // Decompress output and verify correctness
    let mut decompressed = Vec::new();
    ZlibDecoder::new(&output[..])
        .read_to_end(&mut decompressed)
        .expect("should decompress output — implies encoder was finalized correctly");

    assert_eq!(
        String::from_utf8(decompressed).expect("should be valid UTF-8"),
        "<html><body>hi world</body></html>",
        "should have replaced content through deflate round-trip"
    );
}
```

- [ ] **Step 2: Run test to verify it passes (baseline)**

Run: `cargo test --package trusted-server-core test_deflate_round_trip_produces_valid_output`

Expected: PASS (current code happens to work for this case since
`ZlibEncoder::drop` calls `finish` internally — the test establishes the
contract).

- [ ] **Step 3: Change `process_through_compression` to take `&mut W` and remove `drop(encoder)`**

`finish()` is not on the `Write` trait — each encoder type
(`GzEncoder`, `ZlibEncoder`, `CompressorWriter`) has its own `finish()`.
The fix: change the signature to take `&mut W` so the caller retains
ownership and calls `finish()` explicitly.

Change signature (line 335-338):

```rust
    fn process_through_compression<R: Read, W: Write>(
        &mut self,
        mut decoder: R,
        encoder: &mut W,
    ) -> Result<(), Report<TrustedServerError>> {
```

Replace lines 383-393 (the `flush` + `drop` block):

```rust
        encoder.flush().change_context(TrustedServerError::Proxy {
            message: "Failed to flush encoder".to_string(),
        })?;

        // Caller owns encoder and must call finish() after this returns.
        Ok(())
    }
```

Then update `process_deflate_to_deflate` (lines 276-289):

```rust
    fn process_deflate_to_deflate<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use flate2::read::ZlibDecoder;
        use flate2::write::ZlibEncoder;

        let decoder = ZlibDecoder::new(input);
        let mut encoder = ZlibEncoder::new(output, flate2::Compression::default());
        self.process_through_compression(decoder, &mut encoder)?;
        encoder.finish().change_context(TrustedServerError::Proxy {
            message: "Failed to finalize deflate encoder".to_string(),
        })?;
        Ok(())
    }
```

And update `process_brotli_to_brotli` (lines 303-321):

```rust
    fn process_brotli_to_brotli<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use brotli::enc::writer::CompressorWriter;
        use brotli::enc::BrotliEncoderParams;
        use brotli::Decompressor;

        let decoder = Decompressor::new(input, 4096);
        let mut params = BrotliEncoderParams::default();
        params.quality = 4;
        params.lgwin = 22;
        let mut encoder = CompressorWriter::with_params(output, 4096, &params);
        self.process_through_compression(decoder, &mut encoder)?;
        // CompressorWriter finalizes on flush (already called) and into_inner
        encoder.into_inner();
        Ok(())
    }
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --package trusted-server-core`

Expected: All existing tests pass plus the new one.

- [ ] **Step 5: Commit**

```
git add crates/trusted-server-core/src/streaming_processor.rs
git commit -m "Fix encoder finalization: explicit finish instead of drop"
```

---

### Task 2: Convert `process_gzip_to_gzip` to chunk-based processing

**Files:**

- Modify: `crates/trusted-server-core/src/streaming_processor.rs:183-225`
- Test: `crates/trusted-server-core/src/streaming_processor.rs` (test module)

- [ ] **Step 1: Write a test for gzip chunk-based round-trip**

```rust
#[test]
fn test_gzip_to_gzip_produces_correct_output() {
    use flate2::read::GzDecoder;
    use flate2::write::GzEncoder;

    let input_data = b"<html><body>hello world</body></html>";

    // Compress input as gzip
    let mut compressed_input = Vec::new();
    {
        let mut enc = GzEncoder::new(&mut compressed_input, flate2::Compression::default());
        enc.write_all(input_data)
            .expect("should compress test input");
        enc.finish().expect("should finish compression");
    }

    let replacer = StreamingReplacer::new(vec![Replacement {
        find: "hello".to_string(),
        replace_with: "hi".to_string(),
    }]);

    let config = PipelineConfig {
        input_compression: Compression::Gzip,
        output_compression: Compression::Gzip,
        chunk_size: 8192,
    };

    let mut pipeline = StreamingPipeline::new(config, replacer);
    let mut output = Vec::new();

    pipeline
        .process(&compressed_input[..], &mut output)
        .expect("should process gzip-to-gzip");

    // Decompress and verify
    let mut decompressed = Vec::new();
    GzDecoder::new(&output[..])
        .read_to_end(&mut decompressed)
        .expect("should decompress gzip output");

    assert_eq!(
        String::from_utf8(decompressed).expect("should be valid UTF-8"),
        "<html><body>hi world</body></html>",
        "should have replaced content through gzip round-trip"
    );
}
```

- [ ] **Step 2: Run test to verify it passes (baseline)**

Run: `cargo test --package trusted-server-core test_gzip_to_gzip_produces_correct_output`

Expected: PASS (current code works, just buffers everything).

- [ ] **Step 3: Rewrite `process_gzip_to_gzip` to use `process_through_compression`**

Replace `process_gzip_to_gzip` (lines 183-225):

```rust
    fn process_gzip_to_gzip<R: Read, W: Write>(
        &mut self,
        input: R,
        output: W,
    ) -> Result<(), Report<TrustedServerError>> {
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;

        let decoder = GzDecoder::new(input);
        let mut encoder = GzEncoder::new(output, flate2::Compression::default());
        self.process_through_compression(decoder, &mut encoder)?;
        encoder.finish().change_context(TrustedServerError::Proxy {
            message: "Failed to finalize gzip encoder".to_string(),
        })?;
        Ok(())
    }
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --package trusted-server-core`

Expected: All tests pass.

- [ ] **Step 5: Commit**

```
git add crates/trusted-server-core/src/streaming_processor.rs
git commit -m "Convert process_gzip_to_gzip to chunk-based processing"
```

---

### Task 3: Convert `decompress_and_process` to chunk-based processing

**Files:**

- Modify: `crates/trusted-server-core/src/streaming_processor.rs:227-262`
- Test: `crates/trusted-server-core/src/streaming_processor.rs` (test module)

Note: the `*_to_none` callers (`process_gzip_to_none`,
`process_deflate_to_none`, `process_brotli_to_none` at lines 264-332) do
not need changes — they call `decompress_and_process` with the same
signature.

- [ ] **Step 1: Write a test for gzip-to-none chunk-based processing**

```rust
#[test]
fn test_gzip_to_none_produces_correct_output() {
    use flate2::write::GzEncoder;

    let input_data = b"<html><body>hello world</body></html>";

    let mut compressed_input = Vec::new();
    {
        let mut enc = GzEncoder::new(&mut compressed_input, flate2::Compression::default());
        enc.write_all(input_data)
            .expect("should compress test input");
        enc.finish().expect("should finish compression");
    }

    let replacer = StreamingReplacer::new(vec![Replacement {
        find: "hello".to_string(),
        replace_with: "hi".to_string(),
    }]);

    let config = PipelineConfig {
        input_compression: Compression::Gzip,
        output_compression: Compression::None,
        chunk_size: 8192,
    };

    let mut pipeline = StreamingPipeline::new(config, replacer);
    let mut output = Vec::new();

    pipeline
        .process(&compressed_input[..], &mut output)
        .expect("should process gzip-to-none");

    assert_eq!(
        String::from_utf8(output).expect("should be valid UTF-8"),
        "<html><body>hi world</body></html>",
        "should have replaced content and output uncompressed"
    );
}
```

- [ ] **Step 2: Run test to verify baseline**

Run: `cargo test --package trusted-server-core test_gzip_to_none_produces_correct_output`

Expected: PASS.

- [ ] **Step 3: Rewrite `decompress_and_process` to use chunk loop**

Replace `decompress_and_process` (lines 227-262) with a chunk-based
version that mirrors `process_uncompressed`:

```rust
    fn decompress_and_process<R: Read, W: Write>(
        &mut self,
        mut decoder: R,
        mut output: W,
        _codec_name: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        let mut buffer = vec![0u8; self.config.chunk_size];

        loop {
            match decoder.read(&mut buffer) {
                Ok(0) => {
                    let final_chunk =
                        self.processor.process_chunk(&[], true).change_context(
                            TrustedServerError::Proxy {
                                message: "Failed to process final chunk".to_string(),
                            },
                        )?;
                    if !final_chunk.is_empty() {
                        output.write_all(&final_chunk).change_context(
                            TrustedServerError::Proxy {
                                message: "Failed to write final chunk".to_string(),
                            },
                        )?;
                    }
                    break;
                }
                Ok(n) => {
                    let processed = self
                        .processor
                        .process_chunk(&buffer[..n], false)
                        .change_context(TrustedServerError::Proxy {
                            message: "Failed to process chunk".to_string(),
                        })?;
                    if !processed.is_empty() {
                        output.write_all(&processed).change_context(
                            TrustedServerError::Proxy {
                                message: "Failed to write processed chunk".to_string(),
                            },
                        )?;
                    }
                }
                Err(e) => {
                    return Err(Report::new(TrustedServerError::Proxy {
                        message: format!("Failed to read from decoder: {e}"),
                    }));
                }
            }
        }

        output.flush().change_context(TrustedServerError::Proxy {
            message: "Failed to flush output".to_string(),
        })?;

        Ok(())
    }
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --package trusted-server-core`

Expected: All tests pass.

- [ ] **Step 5: Commit**

```
git add crates/trusted-server-core/src/streaming_processor.rs
git commit -m "Convert decompress_and_process to chunk-based processing"
```

---

### Task 4: Rewrite `HtmlRewriterAdapter` for incremental streaming

**Files:**

- Modify: `crates/trusted-server-core/src/streaming_processor.rs:396-472`
- Test: `crates/trusted-server-core/src/streaming_processor.rs` (test module)

Important context: `create_html_processor` in `html_processor.rs` returns
`HtmlWithPostProcessing`, which wraps `HtmlRewriterAdapter`. The wrapper's
`process_chunk` (line 31-34 of `html_processor.rs`) returns intermediate
output immediately for `!is_last` chunks — it passes through, not
swallows. When the post-processor list is empty (streaming gate condition),
the wrapper is a no-op passthrough. No changes needed to
`html_processor.rs`.

- [ ] **Step 1: Write a test proving incremental output**

```rust
#[test]
fn test_html_rewriter_adapter_emits_output_per_chunk() {
    use lol_html::Settings;

    let settings = Settings::default();
    let mut adapter = HtmlRewriterAdapter::new(settings);

    // First chunk should produce output (not empty)
    let result1 = adapter
        .process_chunk(b"<html><body>", false)
        .expect("should process chunk 1");
    assert!(
        !result1.is_empty(),
        "should emit output for non-last chunk, got empty"
    );

    // Second chunk should also produce output
    let result2 = adapter
        .process_chunk(b"<p>hello</p>", false)
        .expect("should process chunk 2");
    assert!(
        !result2.is_empty(),
        "should emit output for second non-last chunk, got empty"
    );

    // Final chunk
    let result3 = adapter
        .process_chunk(b"</body></html>", true)
        .expect("should process final chunk");

    // Concatenated output should be the full document
    let mut full_output = result1;
    full_output.extend_from_slice(&result2);
    full_output.extend_from_slice(&result3);
    let output_str = String::from_utf8(full_output).expect("should be valid UTF-8");
    assert!(
        output_str.contains("<html>") && output_str.contains("hello"),
        "should contain complete document, got: {output_str}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails (current code returns empty for non-last chunks)**

Run: `cargo test --package trusted-server-core test_html_rewriter_adapter_emits_output_per_chunk`

Expected: FAIL — assertion `should emit output for non-last chunk` fails.

- [ ] **Step 3: Rewrite `HtmlRewriterAdapter` to stream incrementally**

Replace the struct and impl (lines 396-472):

```rust
/// Adapter to use `lol_html` `HtmlRewriter` as a `StreamProcessor`.
///
/// Creates the rewriter eagerly and emits output on every `process_chunk`
/// call. Single-use: `reset()` is a no-op since `Settings` are consumed
/// by the rewriter constructor.
pub struct HtmlRewriterAdapter {
    rewriter: Option<lol_html::HtmlRewriter<'static, RcVecSink>>,
    output: Rc<RefCell<Vec<u8>>>,
}

/// Output sink that appends to a shared `Vec<u8>`.
struct RcVecSink(Rc<RefCell<Vec<u8>>>);

impl lol_html::OutputSink for RcVecSink {
    fn handle_chunk(&mut self, chunk: &[u8]) {
        self.0.borrow_mut().extend_from_slice(chunk);
    }
}

impl HtmlRewriterAdapter {
    /// Create a new HTML rewriter adapter.
    ///
    /// The rewriter is created immediately, consuming the settings.
    #[must_use]
    pub fn new(settings: lol_html::Settings<'static, 'static>) -> Self {
        let output = Rc::new(RefCell::new(Vec::new()));
        let sink = RcVecSink(Rc::clone(&output));
        let rewriter = lol_html::HtmlRewriter::new(settings, sink);
        Self {
            rewriter: Some(rewriter),
            output,
        }
    }
}

impl StreamProcessor for HtmlRewriterAdapter {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        if let Some(rewriter) = &mut self.rewriter {
            if !chunk.is_empty() {
                rewriter.write(chunk).map_err(|e| {
                    log::error!("Failed to process HTML chunk: {e}");
                    io::Error::other(format!("HTML processing failed: {e}"))
                })?;
            }
        }

        if is_last {
            if let Some(rewriter) = self.rewriter.take() {
                rewriter.end().map_err(|e| {
                    log::error!("Failed to finalize HTML: {e}");
                    io::Error::other(format!("HTML finalization failed: {e}"))
                })?;
            }
        }

        // Drain whatever lol_html produced since last call.
        // Safe: sink borrow released before we borrow here.
        Ok(std::mem::take(&mut *self.output.borrow_mut()))
    }

    fn reset(&mut self) {
        // No-op: rewriter consumed Settings on construction.
        // Single-use by design (one per request).
    }
}
```

Add these imports at the top of `streaming_processor.rs`:

```rust
use std::cell::RefCell;
use std::rc::Rc;
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --package trusted-server-core`

Expected: The new per-chunk test passes. Some existing tests that assert
"intermediate chunks return empty" will now fail and need updating.

- [ ] **Step 5: Update existing tests for new behavior**

Update `test_html_rewriter_adapter_accumulates_until_last` — it currently
asserts empty output for non-last chunks. Change assertions to expect
non-empty intermediate output and verify the concatenated result.

Update `test_html_rewriter_adapter_handles_large_input` — same: remove
assertions that intermediate chunks are empty.

Update `test_html_rewriter_adapter_reset` — `reset()` is now a no-op.
Remove or update this test since the adapter is single-use.

- [ ] **Step 6: Run all tests again**

Run: `cargo test --package trusted-server-core`

Expected: All tests pass.

- [ ] **Step 7: Run clippy**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: No warnings.

- [ ] **Step 8: Commit**

```
git add crates/trusted-server-core/src/streaming_processor.rs
git commit -m "Rewrite HtmlRewriterAdapter for incremental lol_html streaming"
```

---

### Task 5: Phase 1 full verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test --workspace`

Expected: All tests pass.

- [ ] **Step 2: Run JS tests**

Run: `cd crates/js/lib && npx vitest run`

Expected: All tests pass.

- [ ] **Step 3: Run clippy and fmt**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`

Expected: Clean.

- [ ] **Step 4: Build for WASM target**

Run: `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`

Expected: Builds successfully.

---

## Phase 2: Stream Response to Client

> **Note:** Phase 2 may need adjustment to align with the EC (Edge Compute)
> implementation. Coordinate with the EC work before finalizing the approach.

### Task 6: Migrate entry point from `#[fastly::main]` to raw `main()`

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs:32-68`

- [ ] **Step 1: Rewrite `main` function**

Replace lines 32-68:

```rust
fn main() {
    init_logger();

    let req = Request::from_client();

    // Health probe: independent from settings/routing.
    if req.get_method() == Method::GET && req.get_path() == "/health" {
        Response::from_status(200)
            .with_body_text_plain("ok")
            .send_to_client();
        return;
    }

    let settings = match get_settings() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to load settings: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };
    log::debug!("Settings {settings:?}");

    let orchestrator = build_orchestrator(&settings);

    let integration_registry = match IntegrationRegistry::new(&settings) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create integration registry: {:?}", e);
            to_error_response(&e).send_to_client();
            return;
        }
    };

    let response = futures::executor::block_on(route_request(
        &settings,
        &orchestrator,
        &integration_registry,
        req,
    ));

    match response {
        Ok(resp) => resp.send_to_client(),
        Err(e) => to_error_response(&e).send_to_client(),
    }
}
```

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`

Expected: All tests pass.

- [ ] **Step 3: Build for WASM target**

Run: `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`

Expected: Builds successfully.

- [ ] **Step 4: Commit**

```
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Migrate entry point from #[fastly::main] to raw main()"
```

---

### Task 7: Refactor `process_response_streaming` to accept `W: Write`

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:97-180`

- [ ] **Step 1: Change signature to accept generic writer**

Change `process_response_streaming` from returning `Body` to writing into
a generic `W: Write`:

```rust
fn process_response_streaming<W: Write>(
    body: Body,
    output: &mut W,
    params: &ProcessResponseParams,
) -> Result<(), Report<TrustedServerError>> {
```

Remove `let mut output = Vec::new();` (line 117) and
`Ok(Body::from(output))` (line 179). The caller passes the output writer.

- [ ] **Step 2: Update the call site in `handle_publisher_request`**

In `handle_publisher_request`, replace the current call (lines 338-341):

```rust
// Before:
match process_response_streaming(body, &params) {
    Ok(processed_body) => {
        response.set_body(processed_body);

// After:
let mut output = Vec::new();
match process_response_streaming(body, &mut output, &params) {
    Ok(()) => {
        response.set_body(Body::from(output));
```

This preserves existing behavior — the buffered path still works.

- [ ] **Step 3: Run all tests**

Run: `cargo test --workspace`

Expected: All tests pass (behavior unchanged).

- [ ] **Step 4: Commit**

```
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Refactor process_response_streaming to accept generic writer"
```

---

### Task 8: Add streaming path to publisher proxy

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

This is the core change. `handle_publisher_request` needs to support two
modes: buffered (returns `Response`) and streaming (sends response directly
via `StreamingBody`). The streaming path requires access to Fastly-specific
types (`StreamingBody`, `send_to_client`), but `publisher.rs` lives in
`trusted-server-core` which is platform-agnostic.

**Approach:** Add a `ResponseMode` enum or callback that
`handle_publisher_request` uses to decide how to send the response. The
simplest approach: split into a preparation phase (returns headers + body
stream + processing params) and a send phase (in the fastly adapter).

Alternatively, since `StreamingPipeline::process` already takes `W: Write`,
the adapter can call `process_response_streaming` with a `StreamingBody`
directly. The key is that the adapter needs to:

1. Call `handle_publisher_request` logic up to the point of body processing
2. Decide buffered vs streaming
3. Either buffer or stream

This task is complex — the implementer should read the spec's Step 2
carefully and adapt the approach to minimize changes. The plan provides the
structure; exact code depends on how the publisher function is decomposed.

- [ ] **Step 1: Export `finalize_response` or its logic for use before streaming**

In `main.rs`, make `finalize_response` callable from the publisher path.
Either make it `pub` and move to `trusted-server-core`, or pass a
pre-finalized response to the streaming path.

- [ ] **Step 2: Add `has_html_post_processors()` to `IntegrationRegistry`**

Add a method that returns `bool` to avoid the allocation that
`html_post_processors()` incurs (cloning `Vec<Arc<dyn ...>>`):

```rust
pub fn has_html_post_processors(&self) -> bool {
    !self.inner.html_post_processors.is_empty()
}
```

**File:** `crates/trusted-server-core/src/integrations/registry.rs`

- [ ] **Step 3: Add streaming gate check**

Add a helper in `publisher.rs`:

```rust
fn should_stream(
    status: u16,
    content_type: &str,
    integration_registry: &IntegrationRegistry,
) -> bool {
    if !(200..300).contains(&status) {
        return false;
    }
    // Use has_html_post_processors() to avoid allocating a Vec<Arc<...>>
    // just to check emptiness.
    // Only html_post_processors gate streaming — NOT script_rewriters.
    // Script rewriters (Next.js, GTM) run inside lol_html element handlers
    // during streaming and do not require full-document buffering.
    // Currently only Next.js registers a post-processor.
    let is_html = content_type.contains("text/html");
    if is_html && integration_registry.has_html_post_processors() {
        return false;
    }
    true
}
```

- [ ] **Step 4: Restructure `handle_publisher_request` to support streaming**

Split the function into:

1. Pre-processing: request info, cookies, synthetic ID, consent, backend
   request — everything before `response.take_body()`
2. Header finalization: synthetic ID/cookie headers, `finalize_response()`
   headers, Content-Length removal
3. Body processing: either buffered (`Vec<u8>`) or streaming
   (`StreamingBody`)

The streaming path in the fastly adapter:

```rust
// After header finalization, before body processing:
if should_stream {
    let body = response.take_body();
    response.remove_header(header::CONTENT_LENGTH);
    let mut streaming_body = response.stream_to_client();

    match process_response_streaming(body, &mut streaming_body, &params) {
        Ok(()) => {
            streaming_body.finish()
                .expect("should finish streaming body");
        }
        Err(e) => {
            log::error!("Streaming processing failed: {:?}", e);
            // StreamingBody dropped → client sees abort
        }
    }
} else {
    // Existing buffered path
}
```

- [ ] **Step 5: Handle binary pass-through in streaming path**

For non-text content when streaming is enabled:

```rust
if !should_process {
    let body = response.take_body();
    response.remove_header(header::CONTENT_LENGTH);
    let mut streaming_body = response.stream_to_client();
    io::copy(&mut body, &mut streaming_body)
        .expect("should copy body to streaming output");
    streaming_body.finish()
        .expect("should finish streaming body");
}
```

- [ ] **Step 6: Run all tests**

Run: `cargo test --workspace`

Expected: All tests pass.

- [ ] **Step 7: Build for WASM target**

Run: `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`

Expected: Builds successfully.

- [ ] **Step 8: Commit**

```
git add crates/trusted-server-core/src/publisher.rs \
       crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Add streaming response path for publisher proxy"
```

---

### Task 9: Phase 2 full verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test --workspace`

Expected: All tests pass.

- [ ] **Step 2: Run clippy, fmt, JS tests**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cd crates/js/lib && npx vitest run
```

Expected: All clean.

- [ ] **Step 3: Build for WASM target**

Run: `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`

Expected: Builds.

- [ ] **Step 4: Manual verification with Viceroy**

Run: `fastly compute serve`

Test:

- `curl -s http://localhost:7676/ | sha256sum` — compare with baseline
- `curl -sI http://localhost:7676/` — verify headers present (geo, version,
  synthetic ID cookie if consent configured)
- `curl -s http://localhost:7676/static/tsjs=tsjs-unified.min.js` — verify
  static routes still work via `send_to_client`

- [ ] **Step 5: Chrome DevTools MCP performance capture**

Follow the measurement protocol in the spec's "Performance measurement via
Chrome DevTools MCP" section. Compare against baseline captured on `main`.

---

### Task 10: Chrome DevTools MCP baseline + comparison

- [ ] **Step 1: Capture baseline on `main`**

Follow spec section "Baseline capture" — use `navigate_page`,
`list_network_requests`, `lighthouse_audit`, `performance_start_trace` /
`performance_stop_trace`, `performance_analyze_insight`,
`take_memory_snapshot`. Record median TTFB, TTLB, LCP, Speed Index across
5 runs.

- [ ] **Step 2: Capture metrics on feature branch**

Repeat the same measurements after building the feature branch.

- [ ] **Step 3: Compare and document results**

Create a comparison table and save to PR description or a results file.
Check for:

- TTLB improvement (primary goal)
- No TTFB regression
- Identical response body hash (correctness)
- LCP/Speed Index improvement (secondary)

---

## Phase 3: Make Script Rewriters Fragment-Safe (PR #591)

> **Implementation note (2026-03-27):** All tasks completed. Script rewriters
> accumulate text fragments via `Mutex<String>` until `last_in_text_node` is
> true. Buffered mode removed from `HtmlRewriterAdapter`. 2xx streaming gate
> added. Small-chunk (32 byte) pipeline regression tests added for both
> NextJS `__NEXT_DATA__` and GTM inline scripts.

### Task 11: Make `NextJsNextDataRewriter` fragment-safe

**Files:** `crates/trusted-server-core/src/integrations/nextjs/script_rewriter.rs`

- [x] Add `accumulated_text: Mutex<String>` field
- [x] Accumulate intermediate fragments, return `RemoveNode`
- [x] On last fragment, process full accumulated text
- [x] Handle Keep-after-accumulation (emit `Replace(full_content)`)
- [x] Add regression tests

### Task 12: Make `GoogleTagManagerIntegration` rewrite fragment-safe

**Files:** `crates/trusted-server-core/src/integrations/google_tag_manager.rs`

- [x] Add `accumulated_text: Mutex<String>` field
- [x] Accumulate intermediate fragments, return `RemoveNode`
- [x] On last fragment, match and rewrite on complete text
- [x] Non-GTM accumulated scripts emitted unchanged via `Replace`
- [x] Add regression tests

### Task 13: Remove buffered mode from `HtmlRewriterAdapter`

**Files:** `crates/trusted-server-core/src/streaming_processor.rs`

- [x] Delete `new_buffered()`, `buffered` flag, `accumulated_input`
- [x] Simplify `process_chunk` to streaming-only path
- [x] Remove `buffered_adapter_prevents_text_fragmentation` test
- [x] Update doc comments

### Task 14: Always use streaming adapter in `create_html_processor`

**Files:** `crates/trusted-server-core/src/html_processor.rs`

- [x] Remove `has_script_rewriters` check
- [x] Always call `HtmlRewriterAdapter::new(settings)`

### Task 15: Full verification, regression tests, and performance measurement

- [x] Add 2xx streaming gate (`response.get_status().is_success()`)
- [x] Add streaming gate unit tests (5 tests)
- [x] Add `stream_publisher_body` gzip round-trip test
- [x] Add small-chunk (32 byte) pipeline tests for NextJS and GTM
- [x] `cargo test --workspace` — 766 passed
- [x] `cargo clippy` — clean
- [x] `cargo fmt --check` — clean
- [x] WASM release build — success
- [x] Staging performance comparison (see results below)

### Performance Results (getpurpose.ai, median over 5 runs, Chrome 1440x900)

| Metric                     | Production (v135, buffered) | Staging (v136, streaming) | Delta              |
| -------------------------- | --------------------------- | ------------------------- | ------------------ |
| **TTFB**                   | 54 ms                       | 35 ms                     | **-19 ms (-35%)**  |
| **First Paint**            | 186 ms                      | 160 ms                    | -26 ms (-14%)      |
| **First Contentful Paint** | 186 ms                      | 160 ms                    | -26 ms (-14%)      |
| **DOM Content Loaded**     | 286 ms                      | 282 ms                    | -4 ms (~same)      |
| **DOM Complete**           | 1060 ms                     | 663 ms                    | **-397 ms (-37%)** |

---

## Phase 4: Stream Binary Pass-Through Responses

Non-processable content (images, fonts, video, `application/octet-stream`)
currently passes through `handle_publisher_request` unchanged via the
`Buffered` path. This buffers the entire response body in memory — wasteful
for large binaries that need no processing. Phase 4 adds a `PassThrough`
variant that streams the body directly via `io::copy` into `StreamingBody`.

### Task 16: Stream binary pass-through responses via `io::copy`

**Files:**

- `crates/trusted-server-core/src/publisher.rs`
- `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] Add `PublisherResponse::PassThrough { response, body }` variant
- [ ] Return `PassThrough` when `!should_process` and backend returned 2xx
- [ ] Handle in `main.rs`: `stream_to_client()` + `io::copy(body, &mut streaming_body)`
- [ ] Keep `Buffered` for non-2xx responses and `request_host.is_empty()`
- [ ] Preserve `Content-Length` for pass-through (body is unmodified)

### Task 17: Binary pass-through tests and verification

- [ ] Publisher-level test: image content type returns `PassThrough`
- [ ] Publisher-level test: 4xx image stays `Buffered`
- [ ] `cargo test --workspace`
- [ ] `cargo clippy` + `cargo fmt --check`
- [ ] WASM release build
- [ ] Staging performance comparison (DOM Complete for image-heavy pages)
