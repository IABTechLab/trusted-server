# Absolute auction deadline implementation plan

> **For agentic workers:** Use superpowers:test-driven-development and verification-before-completion. Implement independent adapter scopes in parallel; root owns the collection contract and orchestrator. Independently review the draft PR after creation.

**Goal:** Resolve #1015 by ending provider collection at the deadline measured from dispatch, recording outstanding providers as timeouts and avoiding mediation after budget exhaustion.

**Architecture:** Add `PlatformHttpClient::select_until(pending, web_time::Instant)` returning `Result<Option<PlatformSelectResult>, Report<PlatformError>>`. `None` means deadline expiration and relinquishes all pending state. The default fails closed as unsupported; it must never silently delegate to unbounded selection. Add `wait_until` for buffered mediator requests. Preserve existing unbounded APIs for unrelated publisher/proxy traffic.

**Tech stack:** Rust, web_time, platform runtime timers, Fastly nonblocking pending polls and readiness-gated body reads.

## Design decisions

- Relative backend timeouts alone cannot enforce a total budget against trickling bodies. An outer generic async timer cannot interrupt Fastly's synchronous hostcalls. Use adapter-specific bounded collection of complete bodies.
- Axum races its existing whole-body task selection against a tokio timer; timeout drops its abort-on-drop task handles.
- Cloudflare and Spin retain their single-provider capability restrictions, but return owned pending response futures instead of eagerly awaiting complete responses at dispatch. Bounded selection races the entire request/body future against worker Delay or Spin SDK 7's WASI p3 sleep. Cloudflare aborts its fetch through an AbortController on drop; Spin drops cancelable component subtasks and body readers.
- Fastly maintains pending-header and buffered-body states together. Poll headers without blocking; read at most one ready body chunk per provider per sweep. Never use a network-backed read_to_end. Bound readiness waits by the original deadline; preserve remaining collector states between completions. The SDK does not expose high-level pending wait handles, so brief bounded polling may be needed for header readiness. Use the locked low-level ABI only for readiness-gated body I/O, with safety comments and a justified direct dependency.
- Fastly cannot cancel a still-pending backend operation with the locked public API. On expiration, stop waiting and drop guest state; close acquired bodies. Do not claim network cancellation that the SDK cannot provide.
- Apply bounded collection to split dispatch/collect, synchronous planned auctions, legacy compatibility tests, and mediator waits. Recheck the absolute clock before accepting a completion. Retain successful responses collected before the cutoff and attribute every outstanding launch exactly once.
- No expansion of multi-provider support for worker runtimes, no unrelated proxy timeout changes, and no new operator settings.

## Tasks

- [x] Run the existing deadline test baseline in the isolated worktree.
- [x] Add failing regressions for expired collection, delayed/trickling responses, timeout attribution, partial success, and exhausted-budget mediation.
- [x] Implement and test the platform bounded-collection contract and safe unsupported default.
- [x] Implement each adapter's complete-response deadline and cancellation/abandonment behavior; run meaningful adapter tests.
- [x] Integrate all auction collection and mediator paths, replacing the former capability-dependent acceptance of late responses.
- [x] Document semantics and platform limitations, including deferred worker-runtime requests and Fastly abandonment.
- [x] Run every AGENTS.md CI gate, native core tests, and the optimized Fastly build; record exact outcomes and environmental limitations.
- [x] Create a draft PR with .claude/agents/pr-creator.md and Closes #1015.
- [x] Independently review the PR, address validated findings, rerun affected checks, and push follow-up fixes.

## Plan review

Independent review approved the architecture with two requirements: mediator cutoffs use the earlier of the original auction deadline and the mediator's own logical budget; deadline expiry and transport/polling failure retain distinct attribution. Adapter regressions must cover expired-but-ready inputs, full-body timeouts, fairness, cancellation/abandonment, body limits, and bodyless HTTP semantics.

The original deadline baseline passed. A new regression failed against the original collector because current adapters accepted delayed provider responses after the auction deadline.

## Implementation review

A separate requirements review found no required production corrections. It
suggested a tighter mediator-budget regression, which now verifies the mediator
cutoff against a longer auction budget and rounded Fastly transport timeout.

Review also found two Fastly collector edges before PR creation: a partially
collected response could lose its original HEAD method hint, and concurrent
partial bodies could multiply the old single-response memory allowance. Both
have red/green regressions. Collector state now retains method metadata and
shares a 10 MiB payload budget; exceeding providers fail with attribution while
siblings remain collectible.

## Verification before PR

All AGENTS.md CI gates passed: formatting, eight target-specific clippy aliases,
five adapter test aliases (including reusable Fastly sandboxes), 17 parity
tests, native build-digest tests/lint, JS tests/build/format, documentation and
outside-doc Markdown formatting, WASM clock lint, and documentation proposal
script checks. The native core suite passed 2,986 unit tests; Viceroy passed
2,980 core tests and 229 Fastly adapter tests. Four explicitly excluded Fastly
network tests passed separately against the local delayed/trickling origin.
The optimized Fastly WASM build passed.

A stricter mediator test initially supplied an inconsistent transport timeout
in its fixture; after matching the backend's canonical timeout, both native
and Viceroy full core suites passed. Axum's new fixture also now checks its
request read count to satisfy clippy.

Verification used Rust 1.98.1 and Viceroy 0.21.0. Loopback fixtures required
local socket access. Cloudflare and Spin timer/cancellation paths were checked
for their WASM targets; deterministic native tests exercise their actual
arbitration helpers, rather than a deployed worker runtime.

The GitHub token cannot read project-board fields (`read:project` is absent),
so the issue's board status remains unchanged.

## Draft PR and independent review

Draft PR #1433 targets main, is assigned to the author, and closes #1015. A fresh
independent reviewer inspected the complete `f9281917..d4f7018` implementation,
including locked SDK behavior, all four adapters, core collection and mediation,
Fastly buffering/readiness, the network harness, and documentation. It found no
correctness or regression issues requiring changes and independently ran all ten
Cloudflare/Spin deadline tests successfully.

The reviewer confirmed the documented coverage gap: actual deployed worker timer
and cancellation behavior has not received runtime integration coverage. The
review does not establish a production defect. No review findings remain open.
