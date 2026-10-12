# Optional asynchronous HTML finalization implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow explicitly registered HTML integrations to await I/O after synchronous document processing, with bounded core-owned buffers and exact fallback on optional failure.

**Architecture:** Add an HTML-only terminal phase after existing synchronous processors and before auction-seam handling and encoding. One coordinator owns retained HTML, bounded replacement storage, and the chain deadline; the no-finalizer route remains unchanged. Enabled finalizers bypass shared-template caching before lookup and use synthesized HTML privacy rules.

**Tech Stack:** Rust 2024, existing `futures` local futures, `error-stack`, `derive_more`, `web_time`, `lol_html`, existing compression codecs, and adapter-local Workers/Spin/Tokio timers.

---

**Status:** Implemented; see the execution record and review hardening for current verification

**Date:** 2026-10-12

**Branch:** `feat/1241-async-html-finalization` (planning originated on `spec/1241-async-html-finalization`)

**Baseline:** `2a0cb0444dc335bd09105abe5952329bbbdfb621`

**Design:** [Optional asynchronous HTML document finalization](../specs/2026-10-12-1241-async-html-finalization-design.md)

**Execution record:** [Implementation and bidirectional coverage review](../reviews/2026-10-12-1241-async-html-finalization-implementation.md)

The task checklists below preserve the original execution instructions, including
proposed intermediate commit boundaries. The execution record is the authoritative
completion record: implementation was reviewed together before committing, and it
records actual commands, deviations, and the initially blocked Workers auction control.

The [review hardening addendum](2026-10-12-1241-review-hardening.md) records the
subsequently authorized response-status/range refinements, telemetry clock
correction, runtime CI coverage, and renewed verification. It supplements the
original task instructions below.

## 1. Execution boundaries

This plan implements the selected terminal-phase design, not the original proposal to add
`async_finish` to `StreamProcessor`. Read the spec before implementing. Use
@superpowers:test-driven-development for runtime changes and
@superpowers:verification-before-completion before recording results or committing.

The tasks below were the implementation instructions. Their expected outcomes alone
are not verification evidence; consult the execution record for observed results. Keep intermediate
commits in a draft branch; do not release an API that registrations can enable before all
publisher entry points support it.

Keep the EdgeZero revision, generic streaming traits, JS runtime, and production platform
manifests unchanged. KV wiring under #1139, broader clock fixes under #1240, and a real signing
integration are separate work. Do not mask a live-runtime failure by replacing actual
SDK services with stubs or by reporting a native test as a runtime smoke.

## 2. File responsibilities

| File                                                                                                            | Responsibility                                                                                                 |
| --------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Create `crates/trusted-server-core/src/integrations/html_finalization.rs`                                       | Public factory/session/input/outcome/error contracts, bounded replacement writer, and finish deadline handle   |
| Modify `crates/trusted-server-core/src/integrations/mod.rs`                                                     | Export the new contracts; later register a feature-gated fictional fixture                                     |
| Modify `crates/trusted-server-core/src/integrations/registry.rs`                                                | Empty-by-default finalizer metadata, builder method, ordered factory access, and test construction seam        |
| Modify `crates/trusted-server-core/src/html_processor.rs`                                                       | Share one document-state instance through parser, synchronous processors, and finalizer construction           |
| Create `crates/trusted-server-core/src/platform/timer.rs`                                                       | Optional platform-neutral timer contract                                                                       |
| Modify `crates/trusted-server-core/src/platform/mod.rs` and `types.rs`                                          | Timer exports, default-absent runtime service, builder/accessor support                                        |
| Modify `crates/trusted-server-core/src/settings.rs`                                                             | Global terminal-phase timeout, matching defaults, and validation                                               |
| Create `crates/trusted-server-core/src/publisher/html_finalization.rs`                                          | Private retained-document collector, ordered async driver, candidate validation, and bounded replay            |
| Modify `crates/trusted-server-core/src/publisher.rs`                                                            | Route opted-in HTML through the coordinator on every body/sink path; early cache/privacy/conditional decisions |
| Modify adapter `src/platform.rs` files and Fastly `src/app.rs` tests                                            | Concrete timers and explicit Fastly timer absence                                                              |
| Create `crates/trusted-server-core/src/test_support/async_html_finalization.rs`; modify `src/test_support.rs`   | Fictional integration compiled only under existing `test-utils`                                                |
| Modify `crates/trusted-server-integration-tests/tests/parity.rs`                                                | Host-adapter finalizer behavior and privacy parity                                                             |
| Create `scripts/fixtures/async-html-finalization-origin.py` and `scripts/async-html-finalization-local-test.sh` | Delayed/hung local HTTP lookup and actual-runtime smoke driver                                                 |
| Modify `trusted-server.example.toml` and canonical guide files                                                  | Explain the timeout, registration API, buffer/cache effects, and adapter limitations                           |

Do not split or reorganize unrelated sections of `publisher.rs`. The focused child module
contains the new state machine; existing decoding, auction collection, telemetry guards,
and framing helpers remain authoritative.

## 3. Dependency order and verification rhythm

Tasks 1–4 establish dormant contracts and services. Task 5 implements asynchronous execution;
Task 6 implements bounded collection/replay. Task 7 establishes request-time policy before
Task 8 enables production routing. Tasks 9–11 verify codecs, adapter paths, and real runtimes.
Tasks 12–13 finish documentation and the complete gate run.

Use native core filters for fast feedback, then the target-matched gates associated with
the touched adapters. Before each commit, run formatting, the task's tests, and its applicable
lint gate. Before PR handoff, run the entire gate list in Task 13. An unavailable runtime
is a reported verification gap, not permission to mark the check passing.

For core-only native checkpoints, run
`cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings`.
An adapter lint alias can compile core as a dependency but does not directly lint it as
a primary package. Retain target-matched Fastly/core and adapter gates where listed.

### Task 1: Define the terminal callback and bounded replacement writer

**Spec coverage:** Sections 6.2, 8, 9.3, 10, and 16.3.

**Files:** Create `crates/trusted-server-core/src/integrations/html_finalization.rs`;
modify `crates/trusted-server-core/src/integrations/mod.rs`.

- [ ] Write failing unit tests in the new module for exact-cap append, cap overflow,
      overflow-safe length checking, bounded capacity growth, poisoned-writer rejection,
      and explicit `Unchanged` versus empty `Replace` outcomes.
- [ ] Define/export the spec's object-safe factory/session/input/outcome/error types.
      The factory exposes `integration_id()` and `create(IntegrationHtmlStreamContext)`;
      it is `Send + Sync`. A session is single-use and returns a local borrowing future.
      Define `HtmlFinalizationDeadline` in this same task so the input contract compiles:
      use `web_time::Instant`, a timeout `Duration`, and cooperative-timer availability;
      expose remaining time and keep expiry-at-explicit-time helpers private for tests.
      Cover before/exactly-at/after expiry and saturating zero remaining time.

The callback signature is:

```rust
pub trait IntegrationHtmlFinalizer {
    fn finalize<'a>(
        &'a mut self,
        input: HtmlFinalizationInput<'a>,
    ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>>;
}
```

The input fields and result variants must match spec section 6.2 exactly. Keep the retained
HTML immutable. The writer exposes fallible `append(&[u8])`; only core can consume it to
commit a candidate. Use a concrete `derive_more::Display` error and `core::error::Error`.

- [ ] Implement capped append: compute the next length with `checked_add`; poison and
      return a controlled limit error before mutation on overflow or excess. For growth,
      use an amortized capacity target bounded by the cap and reserve exactly that target.
      Do not allocate the full cap up front or accept an arbitrary replacement `Vec`.
- [ ] Run `cargo test -p trusted-server-core --lib integrations::html_finalization`.
      Confirm the new tests actually run, reproduce the limit failure before the fix, then pass.
      Also run `cargo fmt --all -- --check`, the focused native core clippy command above,
      and `cargo clippy-axum`.
- [ ] Commit the contract/writer change as `Add bounded HTML finalizer contracts`.

The essential overflow regression has this behavior:

```rust
let mut writer = HtmlReplacementWriter::new(4);
writer.append(b"page").expect("should accept the exact cap");
writer.append(b"!").expect_err("should reject output beyond the cap");
assert!(writer.is_poisoned(), "should reject later replacement commitment");
```

Keep writer construction/poison inspection crate-private where only core needs them. Public
items require summaries, intra-doc links, fallible-function error documentation, and a
fictional usage example.

### Task 2: Add the shared finish budget and optional timer contract

**Spec coverage:** Sections 9 and 16.4.

**Files:** Modify `integrations/html_finalization.rs`, `settings.rs`,
`platform/types.rs`, and `platform/mod.rs`; create
`crates/trusted-server-core/src/platform/timer.rs`.

- [ ] Write failing tests for settings omission/default parity, `1`/`5000`/`60000`,
      rejected `0`/`60001`, default-absent timer construction, and services clone/replacement
      methods preserving an installed timer.
- [ ] Add the publisher setting with this exact default and validation:

```rust
#[serde(default = "default_html_finalization_timeout_ms")]
#[validate(range(min = 1, max = 60000))]
pub html_finalization_timeout_ms: u32,

fn default_html_finalization_timeout_ms() -> u32 {
    5000
}
```

Update the hand-written `Publisher::default()` and only compiler-required publisher
settings literals. Do not modify unrelated OpenRTB types also named `Publisher`.

- [ ] Add a `Send + Sync` timer trait with `delay(Duration) -> LocalBoxFuture<'static, ()>`.
      Add optional `Arc<dyn PlatformTimer>` storage, a builder method, and an accessor to
      `RuntimeServices`; missing timer configuration must never cause builder failure.
- [ ] Connect the timer-presence flag to Task 1's deadline type. Keep any deterministic
      clock injection private to tests; do not introduce a public platform clock service.
      Add settings-to-deadline tests proving the whole chain gets the configured duration,
      rather than a duration independently supplied by each finalizer.
- [ ] Run `cargo test -p trusted-server-core --lib html_finalization`,
      `cargo test -p trusted-server-core --lib settings::tests`, and
      `cargo test -p trusted-server-core --lib platform::tests`.
      Expected: new default/validation/deadline tests pass and existing service tests remain green.
      Run `cargo fmt --all -- --check`, the focused native core clippy command above,
      and `cargo clippy-axum`.
- [ ] Commit as `Add HTML finalization budgets and optional timers`.

### Task 3: Register finalizers and share the parser's document state

**Spec coverage:** Sections 6.1, 7.1, 7.2, and 14.1.

**Files:** Modify `integrations/registry.rs`, `integrations/mod.rs`, and
`html_processor.rs`.

- [ ] Write failing registry tests for empty defaults, disabled integration exclusion,
      multiple finalizers preserving registration order, and presence checks that never
      instantiate sessions. Extend the existing single-EOF processor test.
- [ ] Add `.with_html_finalizer(...)`, `has_html_finalizers()`, and ordered factory access.
      Update existing hand-written registration/registry literals with empty collections.
      Follow the current registry's integration enablement/build order; do not sort by ID.
- [ ] Add a `#[cfg(any(test, feature = "test-utils"))]` registry constructor from finalizer
      factories for isolated core tests. Keep it out of ordinary production API builds.
- [ ] Introduce a crate-private construction helper taking an explicit document-state
      instance, and keep `create_html_processor(config)` as a synchronous wrapper that
      constructs state and delegates. Publisher assembly creates one state and passes clones
      of its shared handle to parser hooks, streaming factories, and finalizer factories.
- [ ] Test parser-written state reaching the finalizer, two independent requests, and
      retained-registry/reusable-sandbox isolation. Assert synchronous EOF is delivered once
      before the first finalizer. No replacement is ever reprocessed by that chain.
- [ ] Run the registry and HTML filters:
      `cargo test -p trusted-server-core --lib integrations::registry::tests` and
      `cargo test -p trusted-server-core --lib html_processor::tests`.
      Run `cargo test-fastly-reuse`, `cargo fmt --all -- --check`, and `cargo clippy-fastly`.
- [ ] Commit as `Register HTML finalizers with shared document state`.

### Task 4: Wire concrete adapter timers

**Spec coverage:** Sections 9.2, 9.3, and 14.6.

**Files:** Modify `crates/trusted-server-adapter-axum/src/platform.rs`,
`crates/trusted-server-adapter-cloudflare/src/platform.rs`,
`crates/trusted-server-adapter-spin/src/platform.rs`, and
`crates/trusted-server-adapter-fastly/src/app.rs` tests.

- [ ] Add service-construction tests: Axum installs a timer; production Workers and Spin
      install their SDK timer under the existing runtime cfg guards; native Workers/Spin
      construction remains safe without those SDKs; Fastly installs no timer.
- [ ] Implement stateless timer service objects locally. Return a boxed local future from
      `worker::Delay::from(duration)`, `spin_sdk::time::sleep(duration)`, or
      `tokio::time::sleep(duration)` as applicable. Delay construction is inside the returned
      async future where required to avoid needing a runtime merely to build services.
- [ ] Add the timer to each supporting adapter's runtime-services builder. Preserve config,
      secret, KV, HTTP, telemetry, and template service wiring. No manifest or EdgeZero changes.
- [ ] Run `cargo test-axum`, `cargo test-cloudflare`, `cargo test-spin`, and
      `cargo test-fastly-reuse`. Native tests prove construction contracts, not SDK behavior.
- [ ] Run all touched adapter lint targets: `cargo clippy-axum`,
      `cargo clippy-cloudflare`, `cargo clippy-cloudflare-wasm`,
      `cargo clippy-spin-native`, `cargo clippy-spin-wasm`, and `cargo clippy-fastly`.
      Expected: runtime cfg separation compiles on every relevant target.
- [ ] Commit as `Wire adapter timers for HTML finalization`.

### Task 5: Implement ordered, cancellation-safe terminal execution

**Spec coverage:** Sections 7.2–7.4, 9.3, 10, 13, and 14.2.

**Files:** Create `crates/trusted-server-core/src/publisher/html_finalization.rs`;
declare the child module in `publisher.rs`.

- [ ] Write failing coordinator tests using sessions that yield at least once, write partial
      candidates, return unchanged/error/replacement, and record their input and drop events.
      Use a local `Rc` inside a session/future to prove no accidental `Send` requirement.
- [ ] Give the coordinator ownership of current HTML outside every awaited future. Each
      stage borrows current bytes and a separate fresh writer. A valid replacement moves
      writer storage into current HTML; an unchanged/error result discards the writer.
- [ ] Construct one deadline and retain one pinned timer across the entire loop. Poll expiry
      first, check absolute expiry before each stage and after completion, and make expiry win
      ties. With no timer, await work and check elapsed budget afterward; no preemption claim.
- [ ] On timeout, explicitly drop the active future before accessing its borrowed writer or
      moving current HTML; discard the candidate and stop the chain. Do not put current HTML
      exclusively inside an async chain future that is dropped by a race.

The driver-level ownership invariant is:

```text
driver owns current HTML and current replacement writer
    -> callback borrows both
    -> result/timeout
    -> callback future is dropped, releasing both borrows
    -> driver either commits replacement or emits current HTML
```

- [ ] Validate poisoned writers and required marker cardinality before commitment. Preserve
      the marker as an opaque integration contract; do not add a parser or pretend cardinality
      proves structural placement. Callback errors remain soft; upstream/codec/sink errors
      remain fatal and retain existing `Report<TrustedServerError>` mappings.
- [ ] Test A-to-B output propagation, B timeout preserving A's accepted bytes, ignored writer
      errors, invalid markers, one chain budget, late Fastly results, exact tie handling, and
      input-buffer identity on fallback. Capture pointer identity at stage input and compare
      before sink encoding, since writing to a sink necessarily copies into that sink.
- [ ] Emit controlled outcome/elapsed/byte-count logs; never format callback attachments,
      HTML, signatures, tokens, or request data. Test callback drop on cancellation and ensure
      no finalizer starts afterward. Panics retain existing runtime behavior.
- [ ] Run `cargo test -p trusted-server-core --lib publisher::html_finalization`,
      `cargo test-fastly html_finalization`, `cargo fmt --all -- --check`, and
      `cargo clippy-fastly`. Expected: all deterministic coordinator cases pass on native
      and Fastly test targets.
- [ ] Commit as `Coordinate ordered HTML finalization and fallback`.

### Task 6: Collect bounded transformed HTML and replay optional bypasses

**Spec coverage:** Sections 7.2, 8, 12, and 14.3.

**Files:** Modify `publisher/html_finalization.rs` and minimal collector call sites in
`publisher.rs`.

- [ ] Add collector tests for cap minus one/exact/plus one, many chunks, a large crossing
      chunk, synchronous EOF output crossing the cap, and integer-overflow rejection.
- [ ] Model collection states explicitly: collecting, bypassing, and finished. Append
      transformed bytes under the existing `publisher.max_buffered_body_bytes` cap; enforce
      bounded capacity growth without cloning the retained document.
- [ ] On optional collection overflow, move out the retained prefix and forward it, then the
      unretained crossing chunk, then later synchronous output to ordinary seam/encoding
      delivery. Skip every finalizer and create no timer or outside lookup for that document.
- [ ] Reuse existing auction hold state during bypass; a private seam token can straddle
      the retained/crossing boundary. Test exact order, no duplicated EOF, no raw-token leak,
      and prefix-before-auction collection.
- [ ] Retain the outer buffered writer cap and all existing raw/decoded/parser limits. A
      decoder cap error is not an optional-finalizer bypass. Test both failure categories.
      In route tests, reach the optional cap through rewritten-output expansion while raw
      and decoded input remain within their limits; an oversized origin is an old fatal case.
- [ ] Run `cargo test -p trusted-server-core --lib publisher::html_finalization` and
      `cargo test-fastly html_finalization`; run `cargo fmt --all -- --check` and
      `cargo clippy-fastly`. Expected: collector/replay cases preserve bytes and old fatal limits.
- [ ] Commit as `Bound retained HTML and preserve streaming bypass`.

### Task 7: Gate cache lookup and preserve synthesized-document policy

**Spec coverage:** Sections 11, 14.5, and 16.5.

**Files:** Modify `publisher.rs` and its existing cache/privacy test modules. Modify
`cache_policy.rs` only if reusing its current public helpers is insufficient.

- [ ] Write failing request tests showing an enabled finalizer bypasses a prepopulated warm
      template before lookup and excludes reservation/storage, while disabling it restores
      existing eligibility. Include ESI configuration, no matched slots, consent denial,
      bots/prefetch, and ordinary HTML without an auction.
- [ ] Derive the early finalizer-presence decision from enabled registry metadata. Combine
      it with shared-template admission before key creation/lookup. Keep origin readthrough
      eligibility separate; do not accidentally force every raw origin cache hit to miss.
- [ ] Use the existing non-shared inline assembly fallback consistently: no authorized
      shared-template key, no shared-template token, and matching head/body script choices.
      Do not add a first-version `shared_safe` override or a new public cache header.
- [ ] Apply `enforce_synthesized_html_cache_privacy` before response headers are committed for
      HTML entering the opted-in path, including unchanged/failure/collection bypass. Preserve
      duplicate cookies/CSP and existing EC/privacy finalization.
- [ ] For document GETs, extend the existing full-body predicate so
      `strip_conditional_and_range_headers` removes its same four headers. Preserve `If-Match`
      and `If-Unmodified-Since`. Do not broaden this stripping to assets or other methods.
- [ ] Test raw origin readthrough eligibility with finalizer metadata; validator/header
      stripping; unexpected origin 304 behavior with/without the current ad-stack guard;
      bodyless, unsupported-encoding, and non-HTML route selection. This task proves policy
      admission and header decisions only. Assertions requiring callback execution, callback
      outcome, or callback invocation counts belong to Task 8 after routing is wired.
- [ ] Run `cargo test -p trusted-server-core --lib publisher::tests`,
      `cargo test-fastly publisher::tests`, `cargo fmt --all -- --check`, and
      `cargo clippy-fastly`. Expected: new policy tests and all existing publisher tests pass.
- [ ] Commit as `Gate shared caching for asynchronous HTML finalizers`.

### Task 8: Route every enabled publisher body through the terminal coordinator

**Spec coverage:** Sections 7, 12, 14.4, and 16.1–16.2.

**Files:** Modify `publisher.rs`, `publisher/html_finalization.rs`, and
`html_processor.rs` only where construction data must be threaded.

- [ ] Build failing route tests across `Once`/`Stream`, lazy/buffered output, and auction
      present/absent. Assert every enabled body calls its finalizer once after decoder and
      synchronous EOF, and no disabled/bodiless path calls it.
      Explicitly include processable 404/500 HTML and preserved method/status behavior.
- [ ] Extend relevant publisher parameters with request-local finalizer factories/session
      construction data and shared document state. Keep ownership compatible with the existing
      lazy response stream and borrowed buffered driver.
- [ ] Wire enabled HTML in both `publisher_response_into_streaming_response` and
      `stream_publisher_body_async`/`buffer_publisher_response_async` through one coordinator.
      In particular, prevent opted-in `Once` bodies without an auction from falling through
      the synchronous `stream_publisher_body` path, and route enabled reader-based auction
      bodies through the same terminal sequence.
- [ ] Reuse the existing decoder semantics on each body representation: drain/validate tail,
      finish the synchronous chain once, run finalizers, replay accepted bytes in bounded chunks
      through the existing seam controller, emit ready prefix, collect auction, finish encoder.
      Do not feed candidates through `lol_html` or Next.js a second time.
- [ ] Keep current no-finalizer branches and public synchronous helper behavior intact. Add
      API documentation that finalizers require supported publisher entry points; synchronous
      helpers provide synchronous transformations only.
- [ ] Preserve dispatched-auction guards through collection and every new await. Test
      cancellation during finalizer I/O and each fatal error category for exactly-once terminal
      telemetry. Auction dispatch/deadlines must not restart or expand.
- [ ] Complete Task 7's execution-dependent assertions: raw origin readthrough hits run
      finalizers on every request; success, unchanged, callback error, timeout, and collection
      bypass retain private/no-store headers; HEAD/204/205/304 and unsupported routes invoke
      no callback. Include enabled pages with no auction or matched slot.
- [ ] Run `cargo test-fastly`, `cargo test-fastly-reuse`, `cargo test-axum`,
      `cargo test-cloudflare`, and `cargo test-spin`; then touched adapter lint aliases.
      Expected: all enabled route combinations work and existing early-streaming tests pass.
- [ ] Commit as `Route opted-in HTML through terminal finalization`.

Tasks 1–8 are one coherent feature. No release/PR readiness claim is allowed until this task
passes on all applicable paths, even if earlier dormant API commits compile.

### Task 9: Complete the codec and legacy-behavior regression matrix

**Spec coverage:** Sections 14.3–14.5 and 16.2–16.3.

**Files:** Modify core tests in `publisher.rs`, `publisher/html_finalization.rs`, and
`html_processor.rs`. Keep `streaming_processor.rs` implementation unchanged.

- [ ] Parameterize the new route tests over identity, gzip, deflate, and Brotli; test
      concatenated gzip members and output released only in decoder/processor tails.
- [ ] Add malformed/truncated origin, empty HTML, split UTF-8, absent body end, marker
      loss/duplication, oversized candidate, and optional collection bypass cases.
- [ ] Assert decoder validation precedes I/O, synchronous EOF precedes callback, finalizer
      output precedes auction token removal, and encoder trailers follow accepted output.
- [ ] Retain and run these existing regressions unchanged:

```text
html_stream_processors_compose_in_order_and_receive_final_once
streaming_html_with_stream_processors_rewrites_body
document_state_placeholders_substitute_through_streaming_path
streaming_finalize_emits_compressed_html_before_origin_eof
streaming_finalize_auction_hold_emits_prefix_before_origin_eof
streaming_finalize_auction_hold_emits_small_compressed_page_before_collection
parser_confirmed_auction_seam_streams_nextjs_for_every_encoding
stream_publisher_body_async_auction_hold_decodes_multi_member_gzip_buffered
body_chunk_source_resumes_after_cancelled_poll
finalizers_emit_abandoned_auction_for_bodiless_dispatched_response
```

- [ ] Compare no-finalizer results against the baseline using equivalent deterministic
      fixtures: status, critical/repeated headers, decoded body, emission before source EOF,
      and collection/telemetry counts. Do not compare request-random cookies or IDs literally.
      Preserve exact encoded output where fixtures and chunking are deterministic.
- [ ] Run the three focused native filters from spec section 17 and `cargo test-fastly`.
      Run `cargo fmt --all -- --check` and `cargo clippy-fastly`.
- [ ] Commit as `Verify HTML finalization across body and codec paths`.

### Task 10: Add a feature-gated fictional consumer and host parity tests

**Spec coverage:** Sections 4, 14.1, 14.5, and 14.6.

**Files:** Create `crates/trusted-server-core/src/test_support/async_html_finalization.rs`;
modify `src/test_support.rs`, `integrations/mod.rs`, and
`crates/trusted-server-integration-tests/tests/parity.rs`.

- [ ] Add a fictional `html_finalization_fixture` builder only under the existing
      `test-utils` feature. Keep its Rust module and builder entry absent from ordinary builds.
      Use `.without_js()` so no nonexistent JS module enters the bundle.
- [ ] Configure only fixture `enabled`, `lookup_url`, and `transport_timeout_ms` values in
      the existing integration settings map. The finish budget comes exclusively from
      `[publisher].html_finalization_timeout_ms`; do not introduce a second fixture deadline.
- [ ] Implement a terminal finalizer, not a buffering `StreamProcessor`. It receives
      `input.html`, confirms a fictional EOF sentinel, sends a digest/EOF observation using
      `json!` through `RuntimeServices::http_client`, then appends prefix/replacement/suffix
      to the bounded writer. It performs no signing and needs no KV handle.
- [ ] Ensure its backend through `PlatformBackendSpec`, deriving TLS policy from the URL
      scheme. Existing integration convenience helpers force TLS; do not use them for the
      harness's generated loopback HTTP endpoint. Derive deterministic backend names from the
      same spec where Viceroy needs a local backend mapping.
- [ ] Cap response collection for the fixture lookup. Honor remaining finish budget when
      bounding Fastly first-byte/between-byte transport calls. When a transport cannot offer
      the required bound without a timer, return `Unchanged` instead of an unbounded call.
- [ ] Use the normal enabled registry builder in host parity tests and inject deterministic
      services through existing adapter testing seams. Verify yielding completion, unchanged
      error/timeout bytes, no-finalizer output, privacy, and no mandatory KV. Do not add public
      adapter constructors only to inject a custom registry.
- [ ] Run `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`,
      plus `cargo test-axum`, `cargo test-cloudflare`, and `cargo test-spin`.
      Run `cargo clippy-axum`, `cargo clippy-cloudflare`, and `cargo clippy-spin-native`.
- [ ] Build/check normal artifacts without `test-utils` and prove the fixture builder ID is
      absent using feature-off registry tests. Commit as `Add fictional HTML finalization parity fixtures`.

### Task 11: Verify finalization on the four actual local runtimes

**Spec coverage:** Sections 9.2–9.3 and 14.6.

**Files:** Create `scripts/fixtures/async-html-finalization-origin.py` and
`scripts/async-html-finalization-local-test.sh`; use existing `scripts/smoke-common.sh`
helpers and provisioning examples. Do not alter production manifests.

- [ ] Write the threaded local fixture origin with `/article/success`, `/article/hang`, and
      `/lookup`. Generate fictional HTML containing `ASYNC_FINALIZATION_PENDING` and
      `ARTICLE_EOF_SENTINEL`; serve the EOF sentinel in a later chunk. A lookup records receipt
      of a complete-document digest/EOF observation before replying after 150 ms or remaining
      pending for 30 seconds. Record start/completion counts without request-sensitive data.
- [ ] Add a driver accepting `fastly|cloudflare|spin|axum`. Generate all configs, fake secrets,
      endpoints, runtime manifests, response captures, and logs in a temporary workspace.
      Reuse each adapter's smoke-script provisioning pattern and `smoke-common.sh` cleanup.
      Do not read operator-owned `trusted-server.toml`, `.env`, or cloud credentials.
- [ ] Build a fresh fixture artifact with the feature explicitly enabled and isolated output
      paths. Do not reuse an ordinary cached Worker bundle, and do not call the existing
      Cloudflare `build.sh`, which sources operator env files and rebuilds without fixture flags.

Build commands used by the driver, with its own `CARGO_TARGET_DIR`, are:

```bash
cargo build -p trusted-server-adapter-fastly --target wasm32-wasip1 --release --features trusted-server-core/test-utils
cargo build -p trusted-server-adapter-axum --features trusted-server-core/test-utils
cargo build -p trusted-server-adapter-spin --target wasm32-wasip1 --release --features spin,trusted-server-core/test-utils
```

For Workers, resolve the `worker-build` version from `Cargo.lock`, use that installed
version, and invoke from `crates/trusted-server-adapter-cloudflare`. The driver sets
`FINALIZATION_WORKER_OUTPUT` to an absolute directory inside its temporary workspace:

```bash
worker-build --release --out-dir "$FINALIZATION_WORKER_OUTPUT" . --features trusted-server-core/test-utils
```

Point the temporary Wrangler manifest directly at that fixture bundle and disable implicit
rebuild. Use temporary manifest copies for Viceroy and Spin; point them to the fresh
fixture artifacts. Provision only existing application config/secrets infrastructure.
The new consumer must not require an integration KV namespace or label.

- [ ] Run disabled/enabled cases on the same artifact, host, and port with auctions and
      request-random body content disabled. Capture baseline transformed bodies. Success changes
      only the expected sentinel; a hung lookup returns exact baseline body bytes.
- [ ] Set the fixture publisher finish timeout to 750 ms. On Axum/Workers/Spin, keep the
      transport hang much longer and verify release within a documented cooperative scheduling
      tolerance, well before 30 seconds. Record elapsed values and lookup invocation counts;
      do not make native tests stand in for SDK timers.
      Require a controlled timeout outcome so an early transport error cannot pass the test.
      Measure terminal-phase elapsed time separately from origin transfer and server readiness.
- [ ] For Fastly, use an independently bounded transport timeout within the remaining finish
      budget and verify unchanged fallback after the host timeout. Report the host bound and
      elapsed checks; never describe this as timer-driven interruption.
- [ ] Run the actual runtime entry points through the driver:

```bash
./scripts/async-html-finalization-local-test.sh fastly
./scripts/async-html-finalization-local-test.sh cloudflare
./scripts/async-html-finalization-local-test.sh spin
./scripts/async-html-finalization-local-test.sh axum
```

The driver launches `viceroy serve`, `wrangler dev --local`, `spin up`, or the freshly built
Axum binary with its generated manifests/environment, waits for readiness, captures
headers/body/timing, and always terminates child process groups. Missing tools fail with
an explicit prerequisite message; a script cannot silently skip a selected adapter.

- [ ] Exercise auction-enabled runtime cases separately where supported. If the existing
      Cloudflare clock panic prevents that case, record the exact failure and #1240 dependency;
      no change to this feature's scope and no passing claim for the blocked case.
- [ ] Check script syntax and fixture server tests; run
      `bash -n scripts/async-html-finalization-local-test.sh` and
      `python3 -m py_compile scripts/fixtures/async-html-finalization-origin.py`.
      Commit as `Add local runtime checks for HTML finalization`.

### Task 12: Document the operator and integration contracts

**Spec coverage:** Sections 6–13 and 15.

**Files:** Modify `trusted-server.example.toml`, `docs/guide/configuration.md`,
`docs/guide/integration-guide.md`, `docs/guide/cloudflare.md`,
`docs/guide/spin.md`, `docs/guide/axum-dev.md`, and
`crates/trusted-server-integration-tests/README.md` only in relevant sections.

- [ ] Add the optional publisher setting to the example configuration and canonical settings
      table: default 5000, range 1–60000, chain scope after EOF, no effect without registration.
      Put comments above configuration values, following repository conventions.
- [ ] Document `.with_html_finalizer`, object-safe local borrowing futures, terminal ordering,
      shared document capture state, bounded replacement writes, unchanged fallback, and the
      distinction between stage-input bytes and origin bytes. Include a fictional example.
- [ ] Explain opted-in whole-page latency, unchanged no-finalizer streaming, auction budget
      interactions, shared-template bypass, `private, no-store`, raw readthrough separation,
      and preservation of internal markers/CSP/RSC semantics.
- [ ] Describe timer availability, cooperative scheduling, Fastly host bounds, remote-side-effect
      limitations, and independent KV/clock work without copying volatile adapter matrices into
      several guides. Link to the canonical integration contract from each affected guide.
- [ ] Document the feature-gated fixture and exact runtime commands, prerequisite versions,
      generated temporary workspace behavior, and how to record blocked smoke cases.
- [ ] Run `cargo doc -p trusted-server-core --no-deps --all-features`, `npm run lint` and
      `npm run format` from `docs`, plus the repository Markdown check from Task 13.
      Validate new local links. Commit as `Document asynchronous HTML finalizer behavior`.

### Task 13: Run complete gates and review the implemented change against both documents

**Spec coverage:** Sections 14, 16, and every coverage row below.

**Files:** No additional production changes unless a check exposes a defect in this feature.

- [ ] Inspect the diff for scope: no generic streaming trait change, EdgeZero repin, mandatory
      store, real signing consumer, JS behavior change, or unrelated refactor. Check feature-off
      fixture exclusion and request-state isolation.
- [ ] Run these Rust gates from the workspace root, using the target-matched aliases:

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy-cli
cargo clippy-codegen
cargo test-fastly
cargo test-fastly-reuse
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cargo test -p trusted-server-core --test template_build_digest
cargo clippy -p trusted-server-core --test template_build_digest -- -D warnings
```

- [ ] From `crates/trusted-server-js/lib`, run `node build-all.mjs`, `npx vitest run`, and
      `npm run format`. From `docs`, run `npm run lint` and `npm run format`.
      Install locked docs dependencies with `npm ci` if absent; then from root run:

```bash
docs/node_modules/.bin/prettier --config docs/.prettierrc --check "*.md" ".claude/**/*.md" ".github/**/*.md" "crates/**/*.md" "scripts/**/*.md" "tinybird/**/*.md"
```

- [ ] Run the existing shared-template controls:

```bash
./scripts/template-cache-local-test.sh
./scripts/template-cache-local-test.sh inline
```

Also run the feature's four runtime commands from Task 11 and the existing integration
jobs described in `scripts/integration-tests.sh` and `scripts/integration-tests-browser.sh`
where the feature changes their exercised routes. Record setup and exact outcomes; do not
substitute an empty-auction fixture for the existing winning-bid checks.

- [ ] Map implementation and tests to every coverage row below. Have a reviewer inspect
      the spec and plan together with the final diff; resolve behavioral contradictions centrally.
      Do not weaken a regression assertion merely to match a new implementation.
- [ ] Record passed gates and all unavailable/blocked runtime cases in the PR handoff. If any
      required gate is unresolved, keep the PR draft. No merge/deploy authorization is implied.

## 4. Bidirectional spec/plan coverage

Every requirement must have a task and evidence, and every task must have a requirement.
Use this table during review, not just as a task checklist.

| Spec contract                                                                          | Implementation tasks | Required evidence                                                               |
| -------------------------------------------------------------------------------------- | -------------------- | ------------------------------------------------------------------------------- |
| Sections 1, 4, 5: HTML terminal phase; generic streaming unchanged; no unrelated work  | 1, 3, 8, 13          | API/diff inspection; unchanged legacy streaming tests                           |
| Section 6.1: ordered registration and one request-local document state                 | 3, 10                | factory-order and capture-state tests; request/reuse isolation                  |
| Section 6.2: local borrowing future and bounded replacement sink                       | 1, 5                 | non-`Send` execution; poisoned writer tests                                     |
| Section 7.1: no-finalizer behavior unchanged                                           | 3, 8, 9              | prefix before EOF; one synchronous EOF; no timer/finalizer I/O                  |
| Section 7.2: drain decoder, synchronous EOF, terminal chain, seam, encoder             | 5, 8, 9              | all body/sink/auction/codec combinations; exact event ordering                  |
| Section 7.3: auction deadline unchanged; opted-in delay explicit                       | 8, 9, 11, 12         | unchanged dispatch deadline; prefix-before-collection; expired-auction behavior |
| Section 7.4: cancellation releases borrows and does not lose owned input               | 5, 8                 | drop observer, buffer identity, exactly-once auction abandonment                |
| Section 8: capped collection/candidate capacity; safe optional overflow bypass         | 1, 6, 9              | boundary/overflow/capacity tests; exact replay; old fatal caps                  |
| Section 9.1: default 5000 and validated 1–60000 chain budget                           | 2, 12                | defaults/TOML/range tests; canonical configuration docs                         |
| Sections 9.2–9.3: optional SDK timers, one absolute budget, Fastly limitations         | 1, 2, 4, 5, 11       | deterministic deadline tests; actual-runtime timings; bounded host calls        |
| Section 10: stage-local fallback, error categories, markers, panic behavior            | 1, 5, 9, 12          | each outcome/candidate failure; late/tie behavior; reviewed callback contract   |
| Section 11.1: template bypass before lookup/storage and consistent inline fallback     | 7, 9                 | warm-hit bypass; no store/reservation; disabled cache behavior restored         |
| Section 11.2: downstream privacy, raw-origin cache separation, conditional/range rules | 7, 8, 10             | private headers with no auction; raw-hit execution; four-header stripping       |
| Section 11.3: existing body eligibility, status/method/framing/encoding                | 7, 8, 9              | bodiless/no-HTML/unsupported cases; non-2xx HTML; header preservation           |
| Section 12: every materialized/streamed and lazy/buffered branch                       | 8, 9, 10             | route matrix and host parity; no synchronous-only skip                          |
| Section 13: controlled logging and preserved auction telemetry                         | 5, 8, 11             | no sensitive log fields; outcome counts and abandonment tests                   |
| Section 14: deterministic, real-runtime, and full repository verification              | 9, 10, 11, 13        | named regressions, runtime reports, complete gates                              |
| Sections 15–16: scoped delivery and readiness conditions                               | 12, 13               | guides, diff review, requirements/evidence mapping                              |

## 5. Review and handoff record

Before implementation planning is considered ready, review both documents for:

- Spec requirements omitted from the plan or tasks that contradict the spec.
- Plan additions without a spec requirement, especially fixtures becoming production
  consumers, private buffering reappearing, or deadlines multiplying per callback.
- Unrealistic runtime commands, unsupported cfg combinations, and existing behavior that
  the plan mistakenly assumes instead of checking.
- Ownership of current HTML during timeout, shared state during factory construction, and
  early cache admission that an EOF-only decision could not influence.

Document review results describe planning readiness only. They do not prove the feature
is implemented, regression-free, or ready for deployment. Execution begins only after
the user requests implementation.

### Review completed on 2026-10-12

The standalone spec review and both directions of the independent spec/plan alignment
review approved planning readiness with no unresolved substantive findings.

The review corrected these points before handoff:

- Policy-admission tests in Task 7 no longer require callback execution first wired in
  Task 8; execution-dependent assertions now follow the routing implementation.
- The deadline type referenced by the callback API is defined in Task 1, avoiding an
  unbuildable intermediate contract commit.
- Core edits have a direct primary-package lint checkpoint; non-2xx HTML is an explicit
  acceptance test rather than implicit coverage.
- Overflow route tests distinguish rewrite expansion from preexisting fatal decoder caps.
- Workers fixture output stays in the temporary workspace; runtime timeout tests verify
  timer expiry rather than accidentally passing on an early transport error.

This historical record covers planning review only. The linked execution record now
maps the implemented contracts and observed verification to both documents.
