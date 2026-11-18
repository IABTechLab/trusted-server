# Identity-only tsjs implementation plan

**Goal:** Capture native Prebid IDs and deliver mirrored IDs without installing
Trusted Server auction behavior.

**Architecture:** Add a typed browser mode to the existing Prebid integration.
Retain shared identity setup, then select a narrow request-local identity wrapper
or the existing auction installation. Use the existing bundle/build pipeline.

**Tech stack:** Rust, serde, TypeScript, Prebid 10.26, Vitest and browser runtime.

## Tasks

- [x] Add failing Rust parsing/default/injection tests in
      `crates/trusted-server-core/src/integrations/prebid.rs` and JS integration tests
      in `crates/trusted-server-js/lib/test/integrations/prebid/identity.test.ts`.
      Run the targeted tests and retain the missing-mode/missing-mirror failures.
- [x] Add documented `PrebidMode` enum and `mode` config field, defaults and
      injection in `crates/trusted-server-core/src/integrations/prebid.rs`.
- [x] Add mirror validation/merge in
      `crates/trusted-server-js/lib/src/integrations/prebid/identity.ts`; integrate
      after managed identity setup in `index.ts`. Move auction-only state setup
      after mode selection. Gate GPT refresh initialization on auction mode.
- [x] Retain native ID capture after identity auction callbacks, including
      no-options auctions, and prove receiver/return/error preservation. Cover
      malformed payload, Unicode, expiry, metadata, global/request EID merging,
      same-source deduplication, removal and repeated installation. Real bidder tests
      must cover global/request metadata conflicts and `transmitEids` denial.
- [x] Document mode/configuration, rollout, mirror opt-in and limits in
      `docs/guide/integrations/prebid.md`; add mode to the example config if useful.
- [x] Run targeted Vitest, native core tests, format and adapter-matched clippy.
      Then run all applicable AGENTS.md CI gates with pinned tools and retain logs.
- [x] Build production browser bundles and run actual bidder-consumer browser
      probes via normal Fastly/Axum startup and controlled origin. Use auction mode
      and missing/expired/replaced cookie controls; stop owned processes afterward.
- [x] Obtain independent implementation and code-quality reviews, resolve all
      findings, and recheck affected tests. Cover real-consumer metadata conflicts
      and identity-mode late/replaced consent APIs.

## Commands

Use Node 24.20.0 via its installed directory on PATH and Rust 1.98.1.
Run `npx vitest run test/integrations/prebid/identity.test.ts` in the JS lib;
`cargo test -p trusted-server-core integrations::prebid` and `cargo test-fastly`
in the worktree. Full verification follows the checked-in AGENTS.md aliases,
including JS/docs formatting, domain lint and native build-digest tests.

## Review log

- Round 1: corrected the Prebid array-merge assumption and added publisher
  metadata and activity-redaction controls.
- Round 2: no blocking findings; clarified expiry validation and default-options
  behavior.
- Implementation review: no blocking findings. Added real-consumer metadata
  conflict and identity-mode late/replaced consent tests.
- Code-quality review: no blocking findings. Updated installation comments to
  describe both modes.

## Automatic-capture follow-up

Two independent amendment reviews found no blocking findings. Optional
strengthening was accepted: hold the real SDK async helper pending without an
auction, clear an existing cookie on denial, prove navigation cleanup and one
BFCache timer, and retain auction-mode cookie refresh without a sampler.

- [x] Add a failing no-auction capture test with native IDs already available;
      add delayed IDs, stale clearing and lifecycle regression cases.
- [x] Add an identity-only page-lifecycle sampler in
      `crates/trusted-server-js/lib/src/integrations/prebid/native_capture.ts`.
      Capture immediately and every second; preserve native consent checks,
      clear stale/failed values and deduplicate cookie writes in `index.ts`.
- [x] Prove real Prebid no-auction capture and consent withdrawal/restoration in
      `test/prebid-artifact-integration.test.mjs`. Retain auction-mode behavior.
- [x] Document automatic capture, including background timer throttling and
      absence of forced identity synchronization.
- [x] Independently review the amendment and implementation, resolve findings,
      and run relevant/full gates. Cookie getter/setter failures are contained
      and covered by regression tests.
- [x] Repeat browser verification on Fastly and Axum without a manual
      `requestBids` trigger. Both execute the changed served bundle; controlled
      bidder tests cover replacement/removal, expiry, malformed data, consent
      denial/restoration and capture without mirror echo.

Normal provider issuance remains a separate runtime qualification: local
Viceroy omits JA4 metadata required by the existing browser gate, and Axum
does not support enabled identity providers. Backend provider/finalizer tests
and fixture-issued mirror browser tests establish their respective boundaries.

Local operator task/configuration fixes remain outside this shared change.
