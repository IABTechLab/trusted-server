# Ordered HTML EOF Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development. Follow repository instructions for centralized implementation and independent review.

**Goal:** Support optional ordered EOF I/O with explicit operator permission, bounded held output and predictable failure behavior.

**Architecture:** Keep synchronous chunk processing and one optional `StreamProcessor::async_finish` callback. Complete each stage before downstream EOF, stream ordinary output, retain output only on explicit declaration, and finish auction/encoding afterward. Limits apply per holder; this is not a total request-memory guarantee.

**Tech Stack:** Rust 2024, futures local futures, error-stack, lol_html, adapter timers, native Rust integration tests and GitHub Actions.

**Design:** [Ordered asynchronous HTML processor completion](../specs/2026-10-12-1241-async-html-finalization-design.md).

## Implemented lifecycle

- [x] Add default-absent borrowing callback contracts, bounded candidate writer and shared absolute deadline in `streaming_processor.rs` and `streaming_processor/finish.rs`.
- [x] Declare async/retention capabilities in the existing registry sequence; reject missing operator permission and inconsistent metadata before input effects.
- [x] Implement stepped parser/stage EOF, optional retention, overflow replay, downstream propagation and cancellation in `html_processor.rs` and `html_processor/finish.rs`.
- [x] Migrate the publisher driver without universal buffering; preserve auction/codec ordering, cache/privacy and partial-response safeguards.
- [x] Add production-lifecycle fictional fixtures, native/WASI regression coverage and actual four-runtime controls.

## Review hardening

### 1. Auction suffix storage and diagnostics

**Files:** `crates/trusted-server-core/src/publisher.rs`, `crates/trusted-server-core/src/publisher/html_finalization.rs`.

- [x] Extend the fatal-overflow regression to require `auction_suffix_overflow` telemetry. Add repeated-chunk growth coverage with a non-power-of-two cap.
- [x] Cover auction suffix growth and failure classification with focused EOF tests.
- [x] Reserve only when needed, doubling capacity up to the checked cap. Classify suffix overflow before scanning and encoding errors immediately before encoding.
- [x] Repeat the focused tests and relevant adapter checks.

### 2. Harness configuration, build settings and termination

**Files:** `crates/trusted-server-integration-tests/tests/html_eof_runtime.rs` and `tests/html_eof_runtime/{config,origin,process}.rs`.

- [x] Add failing tests for retained compiler/profile settings, missing template matches, invalid generated configuration, unsupported Viceroy and parent SIGTERM cleanup.
- [x] Verify native harness helpers with `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime --test html_eof_runtime`.
- [x] Preserve explicit Rust build settings without inheriting operator configuration; check template replacements and parsed timeout/permission/loopback targets before configuration push.
- [x] Safely quote TOML paths, validate Fastly tooling before builds, unwind on SIGTERM, and require cooperative timeout diagnostics.
- [x] Repeat unit/real-process tests and all four actual-runtime commands in `scripts/README.md`.

### 3. Remaining EOF acceptance boundaries

**File:** `crates/trusted-server-core/src/html_processor/finish.rs`.

- [x] Cover elapsed timerless completion after polling, oversized non-retained fallback, one-byte markers, duplicate parser EOF and successful EOF at the retention cap.
- [x] Verify fallback ownership, no late commitment, no optional polling after overflow and exactly-once EOF.
- [x] Keep existing cancellation, poisoning, ties, split markers and accepted-upstream-output tests; verify disabled, failed and successful EOF parity across all host adapters.

### 4. CI and maintainer documentation

**Files:** `.github/workflows/test.yml`, `AGENTS.md`, `scripts/README.md`, `docs/guide/testing.md`, configuration/integration guides, canonical spec/plan.

- [x] Run Fastly EOF controls in CI using the repository-pinned Viceroy shared with ordinary tests; retain failure artifacts.
- [x] Document Rust/runtime prerequisites, exact cleanup/Axum/Fastly commands and compiler setting preservation. Validate workflow syntax and the actual commands locally.
- [x] Explain per-holder memory limits, tail latency, Fastly consumer transport bounds and the configured timeout policy. Add a minimal Rust callback example and environment override documentation.
- [x] Keep the current spec and plan focused on the implemented contract. Preserve executed evidence in the PR description.
- [x] Track deferred processed-template caching in #1397, and document new public fields.

### 5. Verification and publication

- [x] Obtain independent source/spec/plan review and resolve concrete findings.
- [x] Run `cargo fmt --all -- --check` and the integration crate format check, all eight target-matched Clippy aliases, all adapter test aliases and Fastly reuse, native core/digest gates, CLI/codegen, parity and exact integration CI selections.
- [x] Run existing ESI/inline/purge controls, JS build/tests/format, docs lint/build/format and repository Markdown formatting.
- [x] Record four-runtime success, disabled, hung and empty-bid auction assembly controls, including Fastly early-body-byte proof. The auction fixture deliberately uses an unreachable bidder.

### 6. Runtime evidence and regression assertions

- [x] Correlate EOF and lookup events within each request; prove an earlier request's EOF cannot satisfy the check.
- [x] Require measured lookup duration for Fastly transport fallback without assuming a cooperative timeout log.
- [x] Assert disabled/failed parity has no finalized marker and failed output equals the disabled baseline; preserve the fixture's ordinary cache policy.
- [x] Verify bounded marker history across multiple small chunks and EOF marker protection.
- [x] Reuse the repository-pinned Viceroy installation for Fastly EOF controls.
- [x] Preserve the default-disabled permission example, format the Rust example and keep the spec independent of superseded drafts.

**Publication:** Record validation in PR #1290. Confirm its remote head and report hosted CI separately from local results.

### 7. Native Rust runtime harness

**Files:** `crates/trusted-server-integration-tests/tests/html_eof_runtime.rs`,
`tests/html_eof_runtime/{config,origin,process}.rs`, integration-test manifest,
shell entry point, workflow, verification guides and this plan.

- [x] Add a feature-gated Rust test target so the existing integration CI selection cannot accidentally launch external runtime controls.
- [x] Cover configuration, compiler environment, event ordering, timing and real process cleanup with native Rust regressions.
- [x] Use typed TOML edits and validated loopback configuration; an Axum origin records monotonic events in process and streams delayed EOF.
- [x] Own spawned process groups with bounded TERM/KILL/wait cleanup and cancellation handling, including active build commands and runtime descendants.
- [x] Preserve all four adapter controls and Fastly early-body proof through the existing shell command, selecting the host target explicitly.
- [x] Keep verification in Rust helper tests and lint gates without additional Python prerequisites.
- [x] Independently review the harness and record the gate matrix and actual runtime controls in PR #1290.

## Acceptance mapping

| Requirement                                       | Coverage                                                            |
| ------------------------------------------------- | ------------------------------------------------------------------- |
| Ordered generic lifecycle and explicit permission | Implemented lifecycle plus EOF boundary tests                       |
| Per-holder limits, ownership and cancellation     | Suffix growth, fallback and deadline tests                          |
| No-hook delivery/cache behavior                   | Existing publisher and template-cache controls                      |
| Honest opt-in latency and cache scope             | Canonical spec, configuration/integration guides and PR description |
| Reliable runtime evidence and repeatability       | Harness regressions, actual runtimes and Fastly EOF CI job          |
