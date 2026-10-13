# Ordered HTML EOF Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Follow repository instructions for centralized implementation and independent review. Steps use checkbox syntax for tracking.

**Goal:** Let registered HTML processors optionally await EOF I/O in stage order without mandatory whole-page buffering.

**Architecture:** Keep synchronous chunk processing and add one optional processor EOF callback. A stepped concrete HTML chain lends driver-owned stage output to a bounded callback and sends the accepted result downstream before downstream EOF. Operator permission is default off; only explicit retention stages buffer output.

**Tech Stack:** Rust 2024, futures LocalBoxFuture, error-stack, lol_html, existing adapter timers and publisher codec/auction drivers.

**Revised:** 2026-10-13. User authorizes specification, plan and implementation updates.
The [specification](../specs/2026-10-12-1241-async-html-finalization-design.md) governs this revision.
The [verification report](../reviews/2026-10-13-1241-ordered-eof-verification.md) records fresh revised-source evidence.
The [old plan](../archive/2026-10-12-1241-buffered-terminal-plan.md) and historical implementation records describe the superseded terminal phase.

## File responsibilities

| File                                                  | Responsibility                                                                    |
| ----------------------------------------------------- | --------------------------------------------------------------------------------- |
| `streaming_processor/finish.rs`                       | Optional finish input/outcome/error, bounded writer and absolute deadline         |
| `streaming_processor.rs`                              | Object-safe default-absent callback and synchronous rejection                     |
| `integrations/registry.rs`                            | One processor registration sequence, early capabilities and permission validation |
| `html_processor.rs` and `html_processor/finish.rs`    | Concrete stepped chain, stage retention and leaf callback completion              |
| `publisher/html_finalization.rs`                      | Existing body/codec/auction driver adapted to stepped EOF                         |
| `publisher.rs`                                        | Early capability routing, direct-helper guards and cache/privacy policy           |
| `settings.rs`                                         | Default-off operator permission; existing finish timeout retained                 |
| `test_support/async_html_finalization.rs`             | Fictional EOF HTTP processor using the production lifecycle                       |
| `scripts/fixtures/async-html-finalization-runtime.py` | Explicit permission in enabled runtime configuration                              |
| Integration/config guides and example TOML            | Capability semantics, operator acceptance and runtime limits                      |

Paths above are relative to `crates/trusted-server-core/src/` unless prefixed with `scripts/` or documentation paths.

## Task 1: Review the revised specification and plan

- [x] Independently review both documents against issue1241 and the accepted latency policy.
- [x] Resolve lifecycle, ownership, cancellation, cache and runtime contradictions before implementation.
- [x] Archive the old documents and link historical reports to the archived design.

## Task 2: Optional callback contract and default behavior

**Files:** `crates/trusted-server-core/src/streaming_processor.rs`, new `streaming_processor/finish.rs`, remove production finalizer API from `integrations/html_finalization.rs` and `integrations/mod.rs`.

- [x] Add failing tests for default `None`, a borrowing non-Send callback, bounded/poisoned replacement and synchronous rejection before reader/writer effects.
- [x] Run `cargo test -p trusted-server-core --all-features async_eof` and record expected missing-capability failures.
- [x] Add `requires_async_finish` and `async_finish` defaults to `StreamProcessor`; adapt existing writer/deadline implementations into generic finish contracts.
- [x] Keep `process_chunk` unchanged. Document that callback input is un-emitted stage output, not necessarily the whole document.
- [x] Make `StreamingPipeline::process` reject async-capable processors before consuming input.
- [x] Re-run targeted tests; format and inspect the diff.

## Task 3: Registration and operator permission

**Files:** `integrations/registry.rs`, `settings.rs`, `trusted-server.example.toml`, fixture configuration tests.

- [x] Write failing tests: permission defaults false; enabled async factory requires explicit acceptance; disabled integration needs no acceptance; permission alone creates no capability; buffered declaration without async capability is invalid.
- [x] Add `publisher.allow_async_html_finish` with serde/default false.
- [x] Add factory capability metadata and preserve the existing single factory sequence.
- [x] Reject missing permission with integration ID and setting in the error. Do not filter or silently omit requested stages.
- [x] Remove separate public finalizer registration and metadata. Test adapters may wrap old fake actions into ordinary production processors.
- [x] Run settings/registry tests and target-matched Axum checks.

## Task 4: Ordered, stepped processor EOF

**Files:** `html_processor.rs`, new `html_processor/finish.rs`, `publisher.rs` concrete async construction helper.

- [x] Write a failing chain test: streaming A, yielding async B, downstream C; assert C transforms B's async output before C receives EOF and every stage receives EOF once.
- [x] Write pending-callback tests proving parser and prior-stage ready bytes are available first.
- [x] Write timeout tests proving downstream synchronous EOF still executes; never race the whole composite future.
- [x] Add boundary-aware marker tests for ordinary-output/EOF splits, buffered/overflow stages, and upstream replacements feeding downstream stages.
- [x] Expose crate-private concrete HTML composition for the enabled driver while preserving public opaque helpers for synchronous callers.
- [x] Implement parser finish, stage-index advancement, driver-owned fallback, bounded candidate and downstream ordinary-chunk propagation.
- [x] Reuse one absolute deadline/timer. Drop each losing callback before selecting output.
- [x] Check factory/session capability consistency before parser effects; reject synchronous composite use with async stages.
- [x] Run `cargo test -p trusted-server-core --all-features async_eof` plus existing HTML/Next.js tests.

## Task 5: Explicit bounded buffering

**Files:** `html_processor/finish.rs` and its tests.

- [x] Write failing tests for full retained input at EOF, cap crossing and one-time replay; verify callbacks do not start after overflow.
- [x] Add framework-owned stage-output retention only for declaring factories; default stages stream through.
- [x] Move retained bytes into fallback at EOF. Skip optional work for oversized streaming EOF fallback without truncation.
- [x] Preserve exact allocation identity on unchanged/error/timeout; preserve fatal outer buffered-output limits.
- [x] Run holder/finish tests and memory-boundary cases.

## Task 6: Publisher, auction and codec migration

**Files:** `publisher/html_finalization.rs`, `publisher.rs`, publisher test helpers.

- [x] Add failing tests for early output while EOF work is pending across materialized/streamed sources and write/lazy sinks.
- [x] Remove universal collection and separate sessions from the driver. Use the concrete chain's stepped EOF operations.
- [x] Drain decoder/parser ready output before starting stage callbacks. Finish auction/encoder after all stage EOF.
- [x] Cap the held auction suffix with `publisher.max_buffered_body_bytes`, check before growth, split releasable prefixes before copying, and test exact-cap/crossing plus fatal abandonment for large post-body content.
- [x] Keep exact flush-before-await barriers; avoid flushing every replay chunk.
- [x] Guard direct async entry points for operator permission and synchronous helpers before effects.
- [x] Preserve range/bodiless/unsupported bypass, actual status, early template-cache admission and independent privacy.
- [x] Migrate existing full-document matrix tests via a test-only buffered processor adapter, not another terminal list.
- [x] Run `cargo test -p trusted-server-core --all-features` and Fastly/WASI regression tests.

## Task 7: Runtime fixture and guides

**Files:** `test_support/async_html_finalization.rs`, registry fixture tests, runtime Python configuration, guides, example TOML.

- [x] Migrate the fixture to an identity processor requesting bounded output retention and optional HTTP finish.
- [x] Add explicit operator permission to enabled harness configurations; disabled controls retain default permission.
- [x] Require a non-retaining EOF fixture/control proving a prefix reaches the Fastly client before a pending callback completes; keep retained controls for fallback and other runtimes.
- [x] Update public examples and guides to one callback API, stage order, fallback scope, explicit buffering and actual adapter TTFB limits.
- [x] Preserve existing cleanup, deadline, exact fallback, privacy and auction harness assertions.
- [x] Run Bash/Python validation and real-process cleanup tests.

## Task 8: Full verification and review

- [x] Run cargo formatting and all eight target-matched Clippy aliases.
- [x] Run all five adapter test aliases, native core all-feature/digest tests and lint, CLI/codegen tests, parity and exact integration-test CI selections.
- [x] Run existing ESI/inline/purge and winning-bid controls; preserve no-hook cache behavior.
- [x] Run JS build/tests/format, docs lint/build/format and repository Markdown format.
- [x] Run actual Axum, Workers, Spin and Fastly enabled/disabled/success/hung/auction controls on final source; qualify installed-tool/runtime limits honestly.
- [x] Obtain independent architecture and implementation review; fix findings and repeat only affected checks.
- [x] Record source and evidence in a new verification report; do not repurpose historical counts as revised-design evidence.

## Task 9: Publish the reviewed revision

- [x] Verify clean intended diff, archive links, spec/plan/implementation alignment and fresh passing checks.
- [x] Commit using repository sentence-case conventions and push the existing feature branch.
- [x] Rewrite draft PR description around generic ordered EOF completion and operator permission; keep issue1241 as the target and keep draft status.
- [x] Confirm remote head/body and report any remaining maintainer decisions or pending hosted checks.

## Acceptance mapping

| Specification                                     | Plan coverage  |
| ------------------------------------------------- | -------------- |
| Generic callback and single lifecycle             | Tasks 2–4      |
| Operator acceptance and early metadata            | Task 3, Task 6 |
| EOF order and release before waits                | Tasks 4, 6     |
| Optional bounded retention and fallback ownership | Tasks 2, 4, 5  |
| Deadline/cancellation and downstream EOF          | Tasks 2, 4, 6  |
| Auction/codec/partial/cache/privacy safeguards    | Task 6, Task 8 |
| Honest adapter TTFB contract and consumer scope   | Task 7, Task 8 |
| Reproducible verification and draft publication   | Tasks 8–9      |

## Execution completion

Implementation commit `a533df267c149bb8295b23e5d5a1fb700c1b32eb` was pushed and
draft PR #1290 was updated and verified. The newer remote merge was preserved.
All local verification passed; hosted CI results remain separate. This completion
record changes documentation only. See the [verification report](../reviews/2026-10-13-1241-ordered-eof-verification.md).
