# Initial ad render ownership implementation plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Prevent publisher auctions begun during TS initial rendering from issuing a competing refresh.

**Architecture:** Extend existing per-element ownership through initial render in runtime and bootstrap. Retain losing tokens while preserving consumed pending-index cleanup and legitimate subsequent refreshes. Avoid restoring initial targeting after settlement.

**Tech Stack:** TypeScript, inline JavaScript, Vitest, Playwright, real Prebid/Universal Creative.

## Tasks

- [x] Add failing request-to-render, lease, successive overlap, callback replay, and
      settlement tests in `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`.
      Move existing legitimate post-request refresh expectations to post-render.
- [x] Extend `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`
      to test requested then rendered then requested using the actual embedded script.
- [x] Run focused Vitest tests and verify failures are forbidden delivery or early closure.
- [x] In `crates/trusted-server-js/lib/src/core/first_impression.ts`, remove the TS
      registration lease gate and release/consume closure. Retain suppressing tokens;
      only render closes registration, and rendered phase never regresses.
- [x] Match lifecycle semantics in `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`.
- [x] In `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`, restore TS
      targeting only before initial settlement. Keep existing pending-index cleanup.
- [x] Run focused tests and resolve regressions against the narrowed contract.
- [x] Add a portable standalone browser regression under
      `crates/trusted-server-integration-tests/browser` using production artifacts and
      the controlled reproduction. Assert one initial native call after callback;
      verify publisher-first and post-render refresh controls.
- [x] Run JS build/tests/lint/format, browser regression, docs format, and the CI
      commands in `AGENTS.md`; report blocked checks precisely.
- [x] Review final diff, address findings, commit spec/plan/code/tests on the same
      branch, and prepare one PR containing the complete change.

## Commands

From `crates/trusted-server-js/lib`:

```sh
npx vitest run test/integrations/prebid/index.test.ts test/integrations/gpt/gpt_bootstrap.test.ts test/integrations/gpt/ad_init.test.ts
npx vitest run
npm run lint
npm run format
node build-all.mjs
```

Run docs formatting from `docs`. Browser invocation and artifact prerequisites
must be documented alongside the committed regression script. See `AGENTS.md`
for target-matched Rust and parity checks. Red phase must fail on the old behavior;
green phase must preserve ordinary post-render refreshes.

## Verification record

- Baseline focused suite: eight expected failures demonstrated premature closure
  and extra native refreshes before runtime changes.
- Fixed focused suite: 362 tests passed. Full JS suite: 1,035 tests passed,
  including type assertions; ESLint, Prettier, and production bundle build passed.
- Portable Chromium regression: all ten scenarios passed across runtime-only
  and bootstrap-first variants. Unfixed runtime produced two overlap requests;
  fixed runtime and bootstrap produced one. Post-render controls still produced two.
- Documentation lint, formatting, and VitePress build passed.
- Read-only review found no blocking issues in the narrowed implementation.
- Rust formatting, all eight clippy configurations, Fastly/Axum/Cloudflare/Spin
  tests, host CLI/codegen tests, and adapter parity passed. Fastly, Axum, and CLI
  tests required execution outside the filesystem/network sandbox for native
  certificates and localhost listeners. No production code changes were needed
  for those environment restrictions.
- Additional template-cache CI harnesses passed: ESI 22 checks and inline 9 checks.
