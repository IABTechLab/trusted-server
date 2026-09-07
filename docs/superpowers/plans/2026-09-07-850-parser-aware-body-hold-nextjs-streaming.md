# Parser-Aware Body Hold and Next.js Streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace raw `</body>` auction coordination with a parser-owned inline seam and make Next.js HTML rewriting stream without an unconditional full-document buffer.

**Architecture:** `lol_html` emits a request-private token at the structural body end; the publisher resolves that token after parsing and before compression, yielding the prefix before awaiting the auction. Next.js moves from an EOF-wide post-processor to a per-document streaming output session that resolves bounded RSC groups in order and restores original payloads on unsafe or over-limit input.

**Tech Stack:** Rust 2024, `lol_html`, `error-stack`, `uuid`, `flate2`, `brotli`, Fastly/Viceroy tests, native adapter tests, VitePress/Prettier documentation.

**Spec:** `docs/superpowers/specs/2026-09-07-850-parser-aware-body-hold-nextjs-streaming-design.md`

---

## File structure

### Create

- `crates/trusted-server-core/src/integrations/nextjs/rsc_stream.rs` — boundary-aware RSC group classification, per-document capture state, placeholder restoration, and the Next.js streaming output processor.

### Modify

- `crates/trusted-server-core/src/integrations/registry.rs` — replace full-document post-processor registration with immutable streaming-processor factories and add the per-document factory context.
- `crates/trusted-server-core/src/integrations/mod.rs` — export the new streaming factory/session contracts and remove the production post-processor export.
- `crates/trusted-server-core/src/html_processor.rs` — compose `lol_html` with per-document streaming processors; add the deferred inline body-close variant; delete whole-document accumulation.
- `crates/trusted-server-core/src/streaming_processor.rs` — add a small processor-chain helper only if composition in `html_processor.rs` would otherwise duplicate finalization logic.
- `crates/trusted-server-core/src/integrations/nextjs/mod.rs` — register the Next.js streaming processor and retain legacy public post-processing exports.
- `crates/trusted-server-core/src/integrations/nextjs/rsc.rs` — expose/refine T-chunk scan primitives needed by the boundary-aware classifier without changing the legacy public rewrite API.
- `crates/trusted-server-core/src/integrations/nextjs/rsc_placeholders.rs` — make RSC script capture fragment-safe, bounded, request-namespaced, and per-document.
- `crates/trusted-server-core/src/integrations/nextjs/script_rewriter.rs` — move `__NEXT_DATA__` fragment state out of the shared registry object and enforce bounded unchanged fallback.
- `crates/trusted-server-core/src/integrations/nextjs/html_post_process.rs` — remove the production `NextJsHtmlPostProcessor`; retain the deprecated public compatibility functions and their direct tests.
- `crates/trusted-server-core/src/publisher.rs` — generate and expose the inline seam token, move seam detection after HTML processing, rewire auction collection, and remove `BodyCloseHoldBuffer`.
- `crates/trusted-server-core/src/integrations/google_tag_manager.rs` — supply the new script-context limit in existing unit fixtures if `IntegrationScriptContext` gains that field.
- `docs/guide/integrations/nextjs.md` — document bounded RSC-group streaming and unchanged fallback.

### Test locations

Tests remain beside their implementation under each file's existing `#[cfg(test)]` module. Publisher streaming-order tests stay in `publisher.rs` because they need the existing auction and `EdgeBody::Stream` fixtures. Do not create a second integration-test harness.

## Implementation constraints

- Follow `CLAUDE.md`; use `cargo test-fastly`, never bare `cargo test --workspace`.
- Keep `StreamProcessor::process_chunk(&mut self, &[u8], bool) -> io::Result<Vec<u8>>` unchanged unless Task 3 proves composition impossible without changing it.
- Use `IntegrationDocumentState` for request-progress state. Registry-owned `Arc` values must remain immutable between documents.
- Check every configured limit before extending a retained `String` or `Vec<u8>`.
- Never emit a generated inline token or Next.js placeholder on success, unchanged fallback, or error recovery.
- Preserve the deprecated `post_process_rsc_html` and `post_process_rsc_html_in_place` public functions.
- Use `log` macros and `expect("should ...")`; do not introduce `anyhow`, `thiserror`, `println!`, or `unwrap()`.
- Each task ends with its focused test, `cargo fmt --all -- --check`, and `cargo test-fastly`. Commit only after those pass.

---

### Task 1: Add boundary-aware RSC group classification

**Files:**

- Create: `crates/trusted-server-core/src/integrations/nextjs/rsc_stream.rs`
- Modify: `crates/trusted-server-core/src/integrations/nextjs/mod.rs:10-30`
- Modify: `crates/trusted-server-core/src/integrations/nextjs/rsc.rs:7-21,168-270,334-470`
- Test: `crates/trusted-server-core/src/integrations/nextjs/rsc_stream.rs`

- [ ] **Step 1: Write classifier tests before exposing implementation**

Add table-driven tests for these outcomes:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RscGroupStatus {
    CompleteRewritable,
    CompleteUnrewritable,
    NeedMore,
    Invalid,
}

#[test]
fn classifies_complete_header_with_cross_payload_content_as_rewritable() {
    let payloads = ["1a:T3,ab", "c\n"];
    assert_eq!(
        classify_rsc_group(&payloads, usize::MAX),
        RscGroupStatus::CompleteRewritable,
        "should rewrite a complete header whose content crosses payloads",
    );
}

#[test]
fn classifies_header_split_across_payloads_as_complete_unrewritable() {
    let payloads = ["1a:T", "3,abc\n"];
    assert_eq!(
        classify_rsc_group(&payloads, usize::MAX),
        RscGroupStatus::CompleteUnrewritable,
        "should restore a physically split header unchanged",
    );
}
```

Also cover every split within `1a:T3e,`, incomplete content, a trailing hexadecimal header candidate, malformed hex, a length above `MAX_REASONABLE_TCHUNK_LENGTH`, escaped quotes/backslashes/Unicode, multiple T-chunks, ordinary payloads with no T-chunks, and a candidate disproved by the next payload.

- [ ] **Step 2: Run the new test target and verify red**

Run:

```bash
cargo test-fastly rsc_stream -- --nocapture
```

Expected: compilation fails because `rsc_stream` and `classify_rsc_group` do not exist.

- [ ] **Step 3: Refine the existing scan result without changing public rewrite behavior**

In `rsc.rs`, replace the internal `Option<Vec<TChunkInfo>>` ambiguity with a crate-private detailed result used by both old rewriting and new classification:

```rust
pub(super) enum TChunkScan {
    Complete(Vec<TChunkInfo>),
    NeedMore,
    Invalid,
}
```

Make `TChunkInfo` and only the fields required by `rsc_stream.rs` `pub(super)` so the
detailed result does not expose a private type. Keep `find_tchunks` and
`find_tchunks_with_markers` as compatibility wrappers if that minimizes the diff. An
incomplete declared body or incomplete terminal escape returns `NeedMore`;
invalid/unreasonable lengths return `Invalid`. Existing rewrite functions must continue
restoring originals for either non-complete result.

- [ ] **Step 4: Implement the logical-payload classifier**

In `rsc_stream.rs`:

1. Give `classify_rsc_group(payloads, max_combined_payload_bytes)` an explicit bound and
   concatenate only after the sum of payload lengths has been checked against it.
2. Record cumulative physical payload boundaries.
3. Parse the logical concatenation without inserting `RSC_MARKER`.
4. Detect a strict terminal prefix of `[0-9a-fA-F]+:T[0-9a-fA-F]+,` as `NeedMore`; include a trailing hexadecimal run.
5. Return `NeedMore` for incomplete declared content.
6. Return `Invalid` for malformed or unreasonable declarations.
7. Return `CompleteUnrewritable` if a complete header range crosses a recorded payload boundary.
8. Otherwise return `CompleteRewritable`.

Do not use the marker-based combined rewriter as the completeness oracle. It cannot recognize a header containing a physical marker.

- [ ] **Step 5: Run classifier and existing RSC tests**

Run:

```bash
cargo test-fastly rsc_stream -- --nocapture
cargo test-fastly integrations::nextjs::rsc::tests -- --nocapture
```

Expected: all classifier cases pass; existing length-recalculation and cross-script-content tests remain green.

- [ ] **Step 6: Run the target gate for this code change**

Run:

```bash
cargo fmt --all -- --check
cargo test-fastly
```

Expected: formatting succeeds and all Fastly/core tests pass.

- [ ] **Step 7: Commit the classifier**

```bash
git add crates/trusted-server-core/src/integrations/nextjs/mod.rs \
  crates/trusted-server-core/src/integrations/nextjs/rsc.rs \
  crates/trusted-server-core/src/integrations/nextjs/rsc_stream.rs
git commit -m "Classify bounded Next.js RSC groups"
```

---

### Task 2: Make Next.js script capture bounded and per-document

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/registry.rs:58-105,533-563`
- Modify: `crates/trusted-server-core/src/html_processor.rs:762-791`
- Modify: `crates/trusted-server-core/src/integrations/nextjs/rsc_stream.rs`
- Modify: `crates/trusted-server-core/src/integrations/nextjs/rsc_placeholders.rs:10-105`
- Modify: `crates/trusted-server-core/src/integrations/nextjs/script_rewriter.rs:14-105`
- Modify: `crates/trusted-server-core/src/integrations/nextjs/html_post_process.rs:1-240,329-390`
- Modify fixtures: every `IntegrationScriptContext { ... }` initializer reported by `rg -n "IntegrationScriptContext \\{" crates/trusted-server-core/src`
- Test: `crates/trusted-server-core/src/integrations/nextjs/rsc_placeholders.rs`
- Test: `crates/trusted-server-core/src/integrations/nextjs/script_rewriter.rs`

- [ ] **Step 1: Add red tests for the fragment state machine**

Cover `Idle + intermediate`, `Idle + final`, `Buffering + intermediate`, `Buffering + final`, overflow before final, one-fragment overflow, reset after `last_in_text_node`, and two interleaved `IntegrationDocumentState` instances.

Use a deliberately tiny limit and assert actions, not internal buffers:

```rust
let first = rewriter.rewrite("prefix", &ctx(false, 8, &document_state));
let overflow = rewriter.rewrite("-overflow", &ctx(false, 8, &document_state));
let final_part = rewriter.rewrite("-tail", &ctx(true, 8, &document_state));

assert_eq!(first, ScriptRewriteAction::RemoveNode);
assert_eq!(overflow, ScriptRewriteAction::Replace("prefix-overflow".to_owned()));
assert_eq!(final_part, ScriptRewriteAction::Keep);
```

For RSC, add one test where overflow occurs before the final fragment and another where one final fragment is already over-limit. Assert that unsafe/incomplete RSC sets document-wide bypass and a subsequent RSC script is restored unchanged.

- [ ] **Step 2: Run focused tests and verify red**

Run:

```bash
cargo test-fastly integrations::nextjs::script_rewriter::tests -- --nocapture
cargo test-fastly integrations::nextjs::rsc_placeholders::tests -- --nocapture
```

Expected: new bound/state-isolation assertions fail against the registry-owned `Mutex<String>` and fragment-skipping RSC implementation.

- [ ] **Step 3: Add the document limit to script context**

Add:

```rust
pub struct IntegrationScriptContext<'a> {
    // existing fields
    pub max_buffered_script_bytes: usize,
}
```

Set it from `HtmlProcessorConfig::max_buffered_body_bytes` in the real `html_processor.rs` callback. Update all unit-test initializers, including Google Tag Manager fixtures, with a realistic limit such as `16 * 1024 * 1024`; do not change their behavior.

- [ ] **Step 4: Define per-document Next.js capture state**

In `rsc_stream.rs`, add request-scoped state stored through `IntegrationDocumentState`:

```rust
enum FragmentState {
    Idle,
    Buffering(String),
    BypassUntilLast,
}

struct NextJsDocumentState {
    namespace: String,
    next_data: FragmentState,
    rsc_script: FragmentState,
    captured_payloads: VecDeque<CapturedPayload>,
    captured_payload_bytes: usize,
    bypass_rsc: bool,
}
```

Generate `namespace` once with `Uuid::new_v4().simple()`. Access it through one helper which calls `document_state.get_or_insert_with`; both parser rewriters and the later output session must receive the same `Arc<Mutex<NextJsDocumentState>>`.

Normalize `max_combined_payload_bytes` through one helper before storing it in this state:
preserve the existing public behavior in `rsc.rs` where a configured value of `0` means
`DEFAULT_MAX_COMBINED_PAYLOAD_BYTES`, and use that effective nonzero value for script
capture, classification, queued payloads, and held output.

- [ ] **Step 5: Move `__NEXT_DATA__` capture out of the registry object**

Remove `NextJsNextDataRewriter::accumulated_text`. Implement the spec's three-state transition table against `NextJsDocumentState::next_data`:

- check `current_len + fragment.len()` before `push_str`;
- emit `Replace(accumulated + current)` immediately before overflow;
- stay `BypassUntilLast` until the real final callback;
- reset to `Idle` for the next script;
- on one-fragment overflow, return `Keep` without allocating a second copy.

- [ ] **Step 6: Make RSC script capture follow the same bounded transitions**

Replace the current `if !is_last { Keep }` path. Suppress intermediate fragments with `RemoveNode`; at final, parse the complete script and either restore it or replace only the payload range with a namespaced placeholder such as:

```text
__ts_rsc_<document uuid>_<index>__
```

Store the exact original payload before returning the replacement. For unsafe overflow, set `bypass_rsc`; never classify the final tail independently after the prefix has been emitted.

Before copying a recognized payload, check both `payload.len()` and
`captured_payload_bytes + payload.len()` against `max_combined_payload_bytes`. If the
aggregate would exceed the limit, leave the current script unchanged and set `bypass_rsc`
so the output session restores any earlier unresolved group in the same call. Increment the
shared counter only when pushing a captured original; decrement it when the output session
consumes or restores that entry. This prevents the parser-side FIFO from temporarily
exceeding the promised group bound.

Keep the Task 2 checkpoint compatible with the registered EOF post-processor. Update
`NextJsHtmlPostProcessor` to obtain the same namespaced `NextJsDocumentState`, consume its
captured payload FIFO, and replace only placeholders from that document's namespace. It
must decrement `captured_payload_bytes` on both rewrite and restoration and reject a
missing/mismatched placeholder rather than leaking generated text. Do not remove the
legacy registration or its EOF buffering wrapper until Task 4.

- [ ] **Step 7: Run focused and cross-integration tests**

Run:

```bash
cargo test-fastly integrations::nextjs::script_rewriter::tests -- --nocapture
cargo test-fastly integrations::nextjs::rsc_placeholders::tests -- --nocapture
cargo test-fastly integrations::google_tag_manager -- --nocapture
```

Expected: fragment, limit, reset, and isolation tests pass; unrelated script rewriters compile and retain behavior.

- [ ] **Step 8: Run the target gate and commit**

```bash
cargo fmt --all -- --check
cargo test-fastly
git add crates/trusted-server-core/src/integrations/registry.rs \
  crates/trusted-server-core/src/html_processor.rs \
  crates/trusted-server-core/src/integrations/google_tag_manager.rs \
  crates/trusted-server-core/src/integrations/nextjs/rsc_stream.rs \
  crates/trusted-server-core/src/integrations/nextjs/rsc_placeholders.rs \
  crates/trusted-server-core/src/integrations/nextjs/script_rewriter.rs \
  crates/trusted-server-core/src/integrations/nextjs/html_post_process.rs
git commit -m "Isolate bounded Next.js script capture"
```

Expected: formatting and all Fastly/core tests pass before the commit.

---

### Task 3: Add per-document streaming integration processors

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/registry.rs:554-565,586-660,700-740,850-865,1030-1050`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs:25-45`
- Modify: `crates/trusted-server-core/src/html_processor.rs:20-160,290-310,790-815,1558-1810`
- Optional modify: `crates/trusted-server-core/src/streaming_processor.rs:297-370`
- Test: `crates/trusted-server-core/src/integrations/registry.rs`
- Test: `crates/trusted-server-core/src/html_processor.rs`

- [ ] **Step 1: Write red registry and processor-chain tests**

Add a fake immutable factory which creates a fresh session containing a request-local counter. Assert:

- builder registration and registry lookup preserve factory order;
- two HTML processors from one registry do not share session counters;
- intermediate `lol_html` output passes through a no-op streaming session before EOF;
- two sessions compose in registration order;
- `is_last = true` reaches every session exactly once.

Replace the old test `post_processors_accumulate_while_streaming_path_passes_through` with an assertion that a registered streaming processor still produces non-empty intermediate output.

- [ ] **Step 2: Run focused tests and verify red**

```bash
cargo test-fastly html_stream_processor -- --nocapture
```

Expected: compilation fails because the factory/session registry does not exist.

- [ ] **Step 3: Define the factory context and trait**

Add crate-public contracts alongside the existing integration traits:

```rust
#[derive(Clone)]
pub struct IntegrationHtmlStreamContext {
    pub request_host: String,
    pub request_scheme: String,
    pub origin_host: String,
    pub document_state: IntegrationDocumentState,
}

pub trait IntegrationHtmlStreamProcessorFactory: Send + Sync {
    fn integration_id(&self) -> &'static str;
    fn create(&self, context: IntegrationHtmlStreamContext) -> Box<dyn StreamProcessor>;
}
```

Add repository-standard doc comments to every public item and method. If returning
`Box<dyn StreamProcessor>` requires a visibility or lifetime adjustment, keep the session
request-local and non-`Send`; do not add `Send` to `HtmlRewriterAdapter` or use unsafe code.

- [ ] **Step 4: Add streaming factories to registration and registry storage**

Add `html_stream_processors` and `with_html_stream_processor`. Initially keep the old post-processor fields so Next.js can migrate in Task 4 without an uncompilable intermediate commit. Add `html_stream_processor_factories()` for processor construction.

- [ ] **Step 5: Compose the request-local processor chain**

Create `HtmlWithStreamingProcessors` in `html_processor.rs`. At this checkpoint its inner
processor is the complete existing HTML pipeline, including `HtmlWithPostProcessing` when
legacy post-processors are registered, so adding streaming infrastructure does not bypass
Next.js EOF substitution before Task 4:

```rust
struct HtmlWithStreamingProcessors {
    inner: Box<dyn StreamProcessor>,
    processors: Vec<Box<dyn StreamProcessor>>,
}

impl StreamProcessor for HtmlWithStreamingProcessors {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> io::Result<Vec<u8>> {
        let mut output = self.inner.process_chunk(chunk, is_last)?;
        for processor in &mut self.processors {
            output = processor.process_chunk(&output, is_last)?;
        }
        Ok(output)
    }
}
```

Construct every session once per call to `create_html_processor`, using the same `IntegrationDocumentState` clone supplied to parser callbacks. Do not construct sessions inside `process_chunk`. Build the current rewriter-plus-legacy-postprocessor pipeline first and wrap that pipeline with the new sessions. The fake chain test uses a registration without a legacy post-processor and therefore proves intermediate streaming without changing current Next.js behavior.

- [ ] **Step 6: Run chain tests and target gate**

```bash
cargo test-fastly html_stream_processor -- --nocapture
cargo fmt --all -- --check
cargo test-fastly
```

Expected: sessions stream intermediate chunks, finalize in order, and remain isolated.

- [ ] **Step 7: Commit the streaming contract**

```bash
git add crates/trusted-server-core/src/integrations/registry.rs \
  crates/trusted-server-core/src/integrations/mod.rs \
  crates/trusted-server-core/src/html_processor.rs \
  crates/trusted-server-core/src/streaming_processor.rs
git commit -m "Add per-document HTML stream processors"
```

Only add `streaming_processor.rs` if it changed.

---

### Task 4: Migrate Next.js from EOF post-processing to bounded streaming

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/nextjs/rsc_stream.rs`
- Modify: `crates/trusted-server-core/src/integrations/nextjs/mod.rs:10-110,120-710`
- Modify: `crates/trusted-server-core/src/integrations/nextjs/html_post_process.rs:1-240,329-390`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs:554-565,586-660,700-740,850-865,1030-1050`
- Modify: `crates/trusted-server-core/src/integrations/mod.rs`
- Modify: `crates/trusted-server-core/src/html_processor.rs:20-160,290-310,790-815,1558-1810`
- Test: `crates/trusted-server-core/src/integrations/nextjs/rsc_stream.rs`
- Test: `crates/trusted-server-core/src/integrations/nextjs/mod.rs`
- Test: `crates/trusted-server-core/src/html_processor.rs`

- [ ] **Step 1: Write red streaming-output tests**

Build tests around `process_chunk`, not just final bytes. Cover:

1. Next.js enabled with no RSC emits non-empty output before EOF.
2. A complete single-payload RSC placeholder is rewritten and released in the same call.
3. A content-split T-chunk holds from the first placeholder and releases immediately after the completing payload.
4. A header-split T-chunk restores unchanged.
5. Interstitial HTML stays after the earlier script.
6. Payload and held-output limits restore before exceeding their configured limit.
7. Invalid/EOF-incomplete groups restore unchanged.
8. Parser-side `bypass_rsc` immediately restores a previously held group and current output, clears state, and passes later RSC scripts unchanged.
9. A missing captured original returns `io::Error` and never emits a placeholder.
10. No output contains the current request's placeholder namespace.

Use small chunks and small limits so each transition occurs deterministically.

- [ ] **Step 2: Run focused tests and verify red**

```bash
cargo test-fastly integrations::nextjs::rsc_stream::tests -- --nocapture
cargo test-fastly html_processor::tests::nextjs_stream_processor_emits_before_eof -- --nocapture
```

Expected: the new Next.js streaming tests fail because production registration still uses
the EOF post-processor. The named HTML processor test exists in this step and fails by
observing empty intermediate output; do not target the accumulation test removed in Task 3.

- [ ] **Step 3: Implement the Next.js factory and session**

Implement `IntegrationHtmlStreamProcessorFactory` on an immutable Next.js factory. The session must:

- scan only its request namespace while retaining at most the maximum placeholder length minus one;
- consume captured originals FIFO;
- maintain `group_output`, payload and output counters, classifier state, and `bypass_rsc`;
- substitute `CompleteRewritable` with `rewrite_rsc_scripts_combined_with_limit`;
- restore `CompleteUnrewritable`, `Invalid`, over-limit, and EOF-incomplete groups as specified;
- validate replacement count and absence of generated placeholders before release;
- log only reason/count/byte totals.

Use checked/saturating length arithmetic before allocation. A document-wide bypass transition restores the held group and current chunk in one call, clears all group/candidate state, and remains pass-through through EOF.

- [ ] **Step 4: Register the streaming factory**

Update `nextjs::register` to use:

```rust
IntegrationRegistration::builder(NEXTJS_INTEGRATION_ID)
    .with_script_rewriter(structured)
    .with_script_rewriter(placeholders)
    .with_html_stream_processor(streaming_factory)
```

Remove production construction of `NextJsHtmlPostProcessor`.

- [ ] **Step 5: Delete whole-document production accumulation**

Once no registration uses it:

- remove `IntegrationHtmlPostProcessor`, `html_post_processors`, `has_html_post_processors`, and `with_html_post_processor`;
- remove `HtmlWithPostProcessing` fields `accumulated_output`, `decoded_input_len`, and its EOF branch;
- rename the wrapper to `HtmlWithStreamingProcessors` if not already done;
- delete production-only placeholder substitution from `html_post_process.rs` after moving needed helpers;
- retain deprecated public `post_process_rsc_html` APIs and their tests.

- [ ] **Step 6: Replace old buffering tests with streaming assertions**

Update or remove tests tied to the deleted generic post-processor. Keep coverage for request state, configured bounds, UTF-8 behavior, non-RSC scripts, fragmented RSC, and legacy public helpers. Rename tests and comments so they no longer claim Next.js post-processes at EOF.

- [ ] **Step 7: Run Next.js and HTML processor suites**

```bash
cargo test-fastly integrations::nextjs -- --nocapture
cargo test-fastly html_processor::tests -- --nocapture
```

Expected: all Next.js output is correct, streaming assertions observe intermediate bytes, and no placeholder leaks.

- [ ] **Step 8: Run the target gate and commit**

```bash
cargo fmt --all -- --check
cargo test-fastly
git add crates/trusted-server-core/src/integrations/registry.rs \
  crates/trusted-server-core/src/integrations/mod.rs \
  crates/trusted-server-core/src/html_processor.rs \
  crates/trusted-server-core/src/integrations/nextjs
git commit -m "Stream bounded Next.js RSC groups"
```

Expected: all Fastly/core tests pass before the commit.

---

### Task 5: Add the parser-owned inline auction seam

**Files:**

- Modify: `crates/trusted-server-core/src/html_processor.rs:159-270,450-510,1880-1945,2070-2230`
- Modify: `crates/trusted-server-core/src/publisher.rs:636-690,1260-1420,3710-3767,8410-8515,15560-15690`
- Test: `crates/trusted-server-core/src/html_processor.rs`
- Test: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Write red parser-marker tests**

Add `BodyCloseInjection::DeferredInlineMarker(String)` expectations:

- a structural mixed-case body end receives exactly one token;
- script/JSON/comment `</body>` text receives none;
- for a structural close, feed the source HTML with every possible origin-chunk split
  across `</BoDy>` and assert exactly one token after parser finalization;
- for each false literal in script, JSON, and comment context, split the source at every
  byte boundary within `</body>` and assert no token;
- multiple body elements still inject once;
- no explicit body end emits no token;
- deferred mode never reads or injects `ad_bids_state`.

Reuse the existing `marker_mode_ignores_a_body_close_written_in_script_data` fixtures where possible, but keep stable ESI `Marker` and request-private deferred behavior as separate assertions.

- [ ] **Step 2: Write red exact-token seam-controller tests**

Replace raw-close buffer tests with a controller initialized from a full token:

```rust
let token = b"<!--ts-inline-body-close-00000000000000000000000000000001-->";
let mut seam = InlineBodyCloseSeam::new(token.to_vec());

let first = seam.push(b"<script>const x = '</body>';</script>");
assert!(!first.found, "publisher body text must not trigger the seam");

let second = seam.push(b"article<!--ts-inline-body-close-00000000000000000000000000000001--></body>");
assert!(second.found, "the exact parser token should trigger the seam");
assert_eq!(second.ready, b"article");
assert_eq!(second.tail, b"</body>");
```

Split the exact token at every byte boundary. Also prove a different UUID-shaped token is ordinary output and released before the seam.

- [ ] **Step 3: Run focused tests and verify red**

```bash
cargo test-fastly deferred_inline_marker -- --nocapture
cargo test-fastly inline_body_close_seam -- --nocapture
```

Expected: compilation fails because the deferred variant and exact-token controller do not exist.

- [ ] **Step 4: Implement the parser variant**

Add the distinct enum variant and handle it in the existing `body` end-tag callback by inserting its markup verbatim. Keep `InlineBids` for paths whose auction has already completed and stable `Marker` for ESI templates. Update exhaustive matches and documentation.

- [ ] **Step 5: Generate one token with the processor construction result**

Change `PublisherBodyProcessor` construction to accept an explicit immediate/deferred inline-seam mode. When deferred inline injection is valid, generate:

```rust
format!(
    "<!--ts-inline-body-close-{}-->",
    uuid::Uuid::new_v4().simple()
)
```

Store the bytes on `PublisherBodyProcessor` and pass the same string into `HtmlProcessorConfig`. Add an accessor which transfers or clones this exact token to the auction driver. ESI mode ignores deferred input and retains `TEMPLATE_SEAM_PLACEHOLDER`.

- [ ] **Step 6: Implement `InlineBodyCloseSeam`**

Replace `BodyCloseHoldBuffer` with an exact-token scanner over processed bytes. Return a value such as:

```rust
struct SeamChunk {
    ready: Vec<u8>,
    tail: Vec<u8>,
    found: bool,
}
```

In searching state, retain only a suffix which could begin the exact token. On a match, remove the token, return preceding bytes as ready and following bytes as tail, then enter released state. `finish` returns the candidate suffix unchanged when no token exists. Delete `BODY_CLOSE_PREFIX` and `find_ascii_case_insensitive` if no non-auction caller uses them.

- [ ] **Step 7: Run focused suites and target gate**

```bash
cargo test-fastly deferred_inline_marker -- --nocapture
cargo test-fastly inline_body_close_seam -- --nocapture
cargo test-fastly marker_mode -- --nocapture
cargo fmt --all -- --check
cargo test-fastly
```

Expected: parser and exact-token scanner tests pass; stable ESI marker tests remain green.

- [ ] **Step 8: Commit seam primitives**

```bash
git add crates/trusted-server-core/src/html_processor.rs \
  crates/trusted-server-core/src/publisher.rs
git commit -m "Mark inline body seams through the HTML parser"
```

---

### Task 6: Move auction holding after HTML processing

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:774-1150,2280-2550,3507-4055,15500-15695,17130-17690,18035-18140`
- Test: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Write the decisive red streaming-order regression**

Use the existing pending-auction/lazy-body fixtures. Feed at least three logical regions:

1. head/body prefix;
2. a Next.js-style script containing a false `</body>` plus later article markup;
3. the structural body close.

Poll while the auction remains pending and assert region 2 and its later article bytes are available. Then assert the stream becomes pending only at the parser token. Complete the auction and assert the bid script is immediately before the structural close.

The test must not use elapsed-time thresholds or sleeps. For the write-sink path, use an
observing writer whose `write` and `flush` calls are recorded separately. Before resolving
the auction, assert that region 2 was written, `flush()` was called after that write, and
collection has not started.

- [ ] **Step 2: Add EOF and failure-path red tests**

Cover no explicit body end, parser failure, source read failure, decoder failure, and encoder failure. Assert one terminal collect/abandon outcome and no generated token. Add an authorized ESI cold-miss test proving transform reaches EOF before auction collection and collects once before reader assembly.

- [ ] **Step 3: Run focused tests and verify red**

```bash
cargo test-fastly parser_confirmed_auction_seam -- --nocapture
cargo test-fastly esi_cold_miss_collects_after_transform -- --nocapture
```

Expected: old raw scanner stalls at the script literal or new test helpers are not yet wired.

- [ ] **Step 4: Refactor chunk steps to process before seam detection**

Replace the old sequence:

```text
decode -> raw </body> hold -> process -> encode
```

with:

```text
decode -> process -> exact generated seam -> encode
```

Refactor `hold_step_decoded_chunk`, `hold_collect_close_tail`, `hold_finish_ready_segments`, and `hold_finish_tail_segments` around `InlineBodyCloseSeam`. Keep shared step/finish helpers for the lazy stream and write-sink driver.

The step result must separate ready encoded segments from the seam event. The lazy caller
yields every ready segment before `collect_stream_auction(...).await`. A write-sink caller
writes every ready segment and calls synchronous `Write::flush()` successfully before starting
collection; propagate a flush failure through the same one-terminal-outcome guard as write,
decode, process, and encode failures.

At source EOF, first finalize the decoder and HTML processor, feed every final processed
byte through `InlineBodyCloseSeam`, and expose its ready output. If no token was found,
release the seam's retained candidate suffix before awaiting collection, then collect once
for telemetry without injecting bids, and only then finalize the encoder trailer. This
preserves streaming for malformed/bodyless documents and prevents a token emitted during
`lol_html::end()` from being missed.

- [ ] **Step 5: Wire deferred construction only when a controller exists**

In `publisher_response_into_streaming_response` and `stream_publisher_body_async`, request a deferred inline token only when all are true:

- an auction is still dispatched;
- content is HTML;
- `effective_assembly_mode` is inline;
- inline body injection is enabled for the response.

Already-collected paths retain `InlineBids`; no-auction paths emit no token. Assert at construction that a deferred processor and controller either both have the same token or both have none.

- [ ] **Step 6: Route authorized ESI cold misses through no-hold processing**

When `template_cache_key.is_some()`, process the entire reader-neutral transform without an inline seam controller. Preserve the existing order of template validation/store and per-reader assembly, but collect the dispatched auction after transform completion and before substituting current-reader bid content. A cache-gate rejection already becomes inline through `effective_assembly_mode` and must use the deferred inline path.

- [ ] **Step 7: Remove raw close-body orchestration**

Delete `BodyCloseHoldBuffer`, `BODY_CLOSE_PREFIX`, raw close tests, and comments describing pre-parser `</body>` scanning. Rename `AuctionHoldState` and helper names to refer to inline seams rather than raw body-close holds. Keep abandonment guard behavior and telemetry reason strings stable unless a test demonstrates they are misleading.

- [ ] **Step 8: Run publisher regressions**

```bash
cargo test-fastly parser_confirmed_auction_seam -- --nocapture
cargo test-fastly streaming_finalize_auction -- --nocapture
cargo test-fastly publisher_response_streaming_finalize -- --nocapture
cargo test-fastly template_cache_end_to_end_tests -- --nocapture
```

Expected: false literals stream before collection, real seams stall and inject once, EOF/failures terminate once, and ESI behavior remains correct.

- [ ] **Step 9: Run the target gate and commit**

```bash
cargo fmt --all -- --check
cargo test-fastly
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Resolve auctions at parser-confirmed body seams"
```

Expected: all Fastly/core tests pass before the commit.

---

### Task 7: Complete compression, adapter, and documentation regressions

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:16650-16895,17130-17690,18035-18430`
- Modify: `crates/trusted-server-core/src/integrations/nextjs/mod.rs:285-710`
- Modify: `crates/trusted-server-adapter-axum/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-spin/tests/routes.rs`
- Modify: `docs/guide/integrations/nextjs.md`
- Test: existing adapter/core test modules only

- [ ] **Step 1: Add compressed parser-seam cases**

Extend existing identity/gzip/deflate/Brotli publisher tests so the decoded HTML contains a
false script literal before the structural close. For every encoding, keep the auction
pending, poll the output, and assert that decoded prefix bytes through the false literal are
observable before collection begins. Then resolve the auction and assert decoded final-byte
parity, bid placement, valid encoder trailers, and no token leakage. For gzip, retain
multi-member coverage with the script and close in different members.

- [ ] **Step 2: Add full-pipeline Next.js plus auction coverage**

Enable Next.js and a pending auction together. Assert:

- HTML before the first RSC group streams;
- a false `</body>` inside `__next_f` does not collect;
- a bounded content-split group resolves in order;
- the real parser seam triggers collection;
- final output has rewritten URLs, corrected T length, bids before `</body>`, and no placeholders.

- [ ] **Step 3: Run focused compressed and combined tests**

```bash
cargo test-fastly streaming_finalize_auction_hold -- --nocapture
cargo test-fastly stream_publisher_body_async_processes_ -- --nocapture
cargo test-fastly nextjs -- --nocapture
```

Expected: identity and all supported encodings preserve content/trailers; combined Next.js/auction streaming passes.

- [ ] **Step 4: Update the Next.js guide**

Document that ordinary HTML streams immediately, unresolved cross-script T-chunk groups are bounded, `max_combined_payload_bytes` limits payload and held output independently, and invalid/incomplete/over-limit groups are restored unchanged. Remove wording that implies guaranteed rewriting under fallback or full-document EOF post-processing.

- [ ] **Step 5: Run adapter parity suites**

First add one buffered route regression in each adapter test module. Feed the same Next.js
fixture and configuration through Axum, Cloudflare, and Spin, then assert the complete body
matches the core expected bytes: rewritten RSC URL and length, preserved script order, bid
markup immediately before structural `</body>`, and no generated seam or RSC placeholder.
These are final-byte assertions because these adapters collect the core stream.

```bash
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: native buffered adapters preserve final-byte behavior.

- [ ] **Step 6: Run formatting and commit regressions/docs**

```bash
cargo fmt --all -- --check
(cd docs && npm run format)
git add crates/trusted-server-core/src/publisher.rs \
  crates/trusted-server-core/src/integrations/nextjs/mod.rs \
  crates/trusted-server-adapter-axum/tests/routes.rs \
  crates/trusted-server-adapter-cloudflare/tests/routes.rs \
  crates/trusted-server-adapter-spin/tests/routes.rs \
  docs/guide/integrations/nextjs.md
git commit -m "Cover streaming seams across encodings and adapters"
```

Expected: Rust formatting and documentation formatting pass before the commit. If tests land in another touched source file, include that file explicitly in `git add`.

---

### Task 8: Run the complete repository verification and review the diff

**Files:**

- Verify all files changed in Tasks 1-7
- Update only files required to fix failures found by these gates

- [ ] **Step 1: Confirm the raw scanner and EOF post-processor are gone**

```bash
rg -n "BodyCloseHoldBuffer|BODY_CLOSE_PREFIX|find_ascii_case_insensitive|IntegrationHtmlPostProcessor|with_html_post_processor|html_post_processors" \
  crates/trusted-server-core/src
```

Expected: no production matches. Test names/comments should also use the new streaming terminology.

- [ ] **Step 2: Confirm generated bytes cannot leak**

Run the focused token/placeholder suites once more:

```bash
cargo test-fastly inline_body_close_seam -- --nocapture
cargo test-fastly rsc_stream -- --nocapture
cargo test-fastly parser_confirmed_auction_seam -- --nocapture
```

Expected: all pass, including success, fallback, EOF, and error paths.

- [ ] **Step 3: Run all Rust format and lint gates**

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: every command exits zero with warnings denied.

- [ ] **Step 4: Run all Rust test gates**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: all suites pass with zero failures.

- [ ] **Step 5: Run JS and documentation gates required by repository CI**

```bash
(cd crates/trusted-server-js/lib && npx vitest run)
(cd crates/trusted-server-js/lib && node build-all.mjs)
(cd crates/trusted-server-js/lib && npm run format)
(cd docs && npm run format)
```

Expected: tests/build succeed and both format checks report no changes required.

- [ ] **Step 6: Review scope and requirements against the spec**

```bash
git diff main...HEAD --stat
git diff main...HEAD -- crates/trusted-server-core/src docs/guide/integrations/nextjs.md
git status --short
```

Verify line by line:

- parser context is the only source of inline body seams;
- the ready prefix is yielded before auction await;
- Next.js no longer buffers every document to EOF;
- every accumulator and held group is checked before growth;
- unsafe RSC continuation enters immediate byte-preserving bypass;
- request state is isolated;
- ESI cold/warm and compression paths retain semantics;
- no unrelated refactor or generated build artifact is present;
- the worktree is clean after the final commit.

- [ ] **Step 7: Request implementation code review**

Use `superpowers:requesting-code-review` with `main` as the base and the current branch head. Address all Critical and Important findings, rerun the affected focused suite, and repeat the relevant full gate before claiming completion.

- [ ] **Step 8: Commit only if verification required corrections**

If Steps 1-7 required changes:

```bash
git add <only the files changed to address verification>
git commit -m "Resolve parser-seam verification findings"
```

If no files changed, do not create an empty commit.
