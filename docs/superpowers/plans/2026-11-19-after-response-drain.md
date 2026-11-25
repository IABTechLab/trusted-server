# Drain after-response tasks implementation plan

**Goal:** Wire bounded Axum after-response work and prove shared SIGTERM grace.

**Architecture:** Share the existing Fastly lookup runner in core; Axum retains
the request scheduler and registers owned work after successful response assembly.
Normal Axum activation remains blocked on A2 #1574; A8 #1579 owns record CI proof.

**Stack:** #1635, above #1531. EdgeZero uses R2 on `rc/identity`
(`9d02d7b3185b49e81d9ac972ca3feef932ed848a`, edgezero#490), matching the base.
No other EdgeZero implementation work is included.

## Task 1: R2 dependency

- [x] Verify R2 already contains #509's bounded Fastly/Axum after-response API.
- [x] Match every workspace EdgeZero dependency and lockfile entry to #1635.
- [x] Keep only this PR's identity-provider test dependency in the Cargo diff.

## Task 2: Shared execution, test first

Files: `crates/trusted-server-core/src/ec/after_response.rs`,
`crates/trusted-server-adapter-fastly/src/app.rs`,
`crates/trusted-server-adapter-fastly/src/post_delivery.rs`.

- [x] Add a failing positive shared-work test plus permit/identity gates.
      Run `cargo test -p trusted-server-core ec::after_response` and retain red.
- [x] Move captured lookup type and runner into core, retaining all guards.
      Fastly wrapper passes its EcContext to that runner; preserve delivery gate.
- [x] Run native core tests and Fastly filtered after-response/post-delivery tests.

## Task 3: Axum registration, test first

Files: `crates/trusted-server-adapter-axum/src/after_response.rs`,
`src/lib.rs`, `src/app.rs`, and scoped tests.

- [x] Test that eligible work registers once, is not polled at registration,
      and needs a permit; add absent-scheduler and no-work cases.
- [x] Retain handle from request extensions and prepare work at publisher route.
      Schedule only after owner attachment; move the permit-bearing context, app
      and client. Never clone the context (Clone drops permits).
- [x] Bound entire task by `identity_after_response_budget_ms`; preserve response
      and log registration failure. No production permits/store are introduced.
- [x] Run `cargo test-axum` and `cargo clippy-axum`.

## Task 4: Runtime and documentation

Files: scoped native subprocess test fixture, crate Cargo manifest if needed,
`crates/trusted-server-core/src/settings.rs`, and
`docs/guide/native-rc-demo.md`.

- [x] Add subprocess tests for response-before-task, SIGTERM completion,
      shared grace and grace exhaustion, using the same scheduling function.
      Exercise the real shared runner with a scripted provider response and fixture
      row, asserting its guarded update; separately probe actual SDK HTTP scope. First run fails without registration;
      compare an explicit disabled control. Also cover disconnect release and
      task-budget cancellation.
- [x] Exercise operator task/proxy with backed-up local inputs, preserve artifact
      identity, and restore exact files. Separate production smoke from controlled
      fixture proof and unavailable A2 record-write consumer.
- [x] Record EdgeZero observations with paths and evidence; do not fix its code.
- [x] Run targeted tests, lint, format and changed-line domain lint as applicable.

## Task 5: Rollout and handoff

- [x] Review the combined implementation and resolve source/test findings.
- [x] Document targeted verification separately from production activation.
- [x] Keep the PR stacked on #1635 and state the narrowed A2/A8 acceptance.
- [x] Use the R2 dependency already shared by the stack.
- [x] Document #1646 overlap: whichever change lands second must merge the other.

## Review log

- Round 1 (independent agent): clarified runtime input reach, permit ownership
  and independent cancellation paths; no blocking findings.

- Round 2 (fresh independent agent): no blocking findings. Added settings
  documentation for the Axum whole-task budget; all optional feedback resolved.

## Reconciliation with #1646 (2026-11-25)

#1646 landed its own shared runner first, so this PR keeps that one and drops
its own: core `ec/app/lookups.rs` `AfterResponseLookups` with
`IdentityApp::run_after_response_lookups`, scheduled by
`IdentityApp::schedule_after_response` from Axum `dispatch_ec_route` and the
shared EC route. The `ec::after_response` runner, `AfterResponseLookups::capture`
and Axum's `after_response::schedule_lookups` described above were removed.
`schedule_after_response` now checks `EcContext::has_after_response_permit()`
before registering and returns whether it registered; a missing scheduler or a
rejected registration is logged, not returned as an error. This PR's drain and
registration tests now exercise that runner from
`crates/trusted-server-adapter-axum/src/after_response_drain_tests.rs`.

The base is now #1646 (A2), which supplies the Axum record store and issues
production follow-up permits, so the "no EC record store" and "activation
remains A2 #1574" statements above describe the earlier #1635 base. This PR's
Cargo dev-dependency change was absorbed: #1646 already depends on
`trusted-server-identity-providers` directly. CI proof of a pending record write
is #1579, implemented in #1654.
