# Ordered HTML EOF Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development. Follow repository instructions for centralized implementation and independent review.

**Goal:** Support optional ordered EOF I/O with explicit operator permission, bounded held output and predictable failure behavior.

**Architecture:** Keep synchronous chunk processing and one optional `StreamProcessor::async_finish` callback. Complete each stage before downstream EOF, stream ordinary output, retain output only on explicit declaration, and finish auction/encoding afterward. Limits apply per holder; this is not a total request-memory guarantee.

**Tech Stack:** Rust 2024, futures local futures, error-stack, lol_html, adapter timers, Python 3.11+ and GitHub Actions.

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
- [x] Run `cargo test -p trusted-server-core --all-features async_eof` and observe the reason/growth failures.
- [x] Reserve only when needed, doubling capacity up to the checked cap. Classify suffix overflow before scanning and encoding errors immediately before encoding.
- [x] Repeat the focused tests and relevant adapter checks.

### 2. Harness configuration, build settings and termination

**Files:** `scripts/fixtures/async-html-finalization-runtime.py`, `scripts/fixtures/test_async_html_finalization_runtime.py`.

- [x] Add failing tests for retained compiler/profile settings, missing template matches, invalid generated configuration, unsupported Viceroy and parent SIGTERM cleanup.
- [x] Run `python3 -m unittest discover -s scripts/fixtures -p 'test_async_html_finalization_runtime.py' -v` and record failures before fixes.
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

- [x] Add isolated Viceroy 0.19 Fastly EOF controls to CI, preserving pinned 0.17 ordinary tests. Cache the isolated tool and retain failure artifacts.
- [x] Document Python/runtime prerequisites, exact cleanup/Axum/Fastly commands and compiler setting preservation. Validate workflow syntax and the actual commands locally.
- [x] Explain per-holder memory limits, tail latency, Fastly consumer transport bounds and the configured timeout policy. Add a minimal Rust callback example and environment override documentation.
- [x] Keep the current spec and plan; remove superseded archived designs, hardening logs and execution histories. Preserve executed evidence in the PR description.
- [x] State the deferred processed-template cache requirement explicitly in the draft PR, and document new required public fields.

### 5. Verification and publication

- [x] Obtain independent source/spec/plan review and resolve concrete findings.
- [x] Run `cargo fmt --all -- --check` and the integration crate format check, all eight target-matched Clippy aliases, all adapter test aliases and Fastly reuse, native core/digest gates, CLI/codegen, parity and exact integration CI selections.
- [x] Run existing ESI/inline/purge controls, JS build/tests/format, docs lint/build/format and repository Markdown formatting.
- [x] Record fresh four-runtime success, disabled, hung and auction results, including Fastly early-body-byte proof.

### 6. Runtime evidence and regression assertions

- [x] Correlate EOF and lookup events within each request; prove an earlier request's EOF cannot satisfy the check.
- [x] Require measured lookup duration for Fastly transport fallback without assuming a cooperative timeout log.
- [x] Assert disabled/failed parity has no finalized marker and failed output equals the disabled baseline; preserve the fixture's ordinary cache policy.
- [x] Verify bounded marker history across multiple small chunks and EOF marker protection.
- [x] Check the isolated Viceroy archive against its pinned SHA-256 before extraction; invalidate the earlier unchecked tool cache.
- [x] Preserve the default-disabled permission example, format the Rust example and keep the spec independent of superseded drafts.

**Publication:** After validation, commit and push to the existing draft PR. Confirm its remote head and report hosted CI separately from local results.

## Acceptance mapping

| Requirement                                       | Coverage                                                            |
| ------------------------------------------------- | ------------------------------------------------------------------- |
| Ordered generic lifecycle and explicit permission | Implemented lifecycle plus EOF boundary tests                       |
| Per-holder limits, ownership and cancellation     | Suffix growth, fallback and deadline tests                          |
| No-hook delivery/cache behavior                   | Existing publisher and template-cache controls                      |
| Honest opt-in latency and cache scope             | Canonical spec, configuration/integration guides and PR description |
| Reliable runtime evidence and repeatability       | Harness regressions, actual runtimes and isolated Fastly CI job     |
