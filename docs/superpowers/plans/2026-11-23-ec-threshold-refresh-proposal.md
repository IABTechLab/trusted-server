# DEC-13 optional EC threshold refresh proposal plan

Status: approved for implementation on 2026-11-23 with same-value EC refresh
and signed marker together on unknown-age bootstrap, and browser-only retention
scope in the [spec](../specs/2026-11-23-ec-threshold-refresh-proposal-design.md).
DEC-13 remains open. This plan does not authorize changing the decision record.

**Goal:** provide an off-by-default threshold-refresh alternative to never
refresh, in a separate draft PR stacked on #1646.

**Architecture:** shared core owns typed configuration, the authenticated
issuance marker and document-only renewal. Existing adapter privacy
finalizers remain the enforcement boundary. No record schema, provider, or
EdgeZero change is planned.

**Tools:** Rust 1.98.1; Node 24.20.0 for build scripts; Viceroy 0.21.0 from an
isolated install; real Chrome with owned profiles and publisher proxy.

## 1. Confirm approval, base and baseline

- [ ] Design approval received; use same-response EC-plus-marker bootstrap.
- [ ] Recheck #1646's head, issue state, DEC-13 and repository instructions.
      Create a proposal branch from its current head in an outside worktree.
      If the base moves, merge its head into the proposal with a signed merge
      commit; reassess affected documents and tests. Never change #1646.
      Recapture affected disabled baseline fixtures against each new merged
      base and rerun affected verification after a merge or review correction.
- [ ] Capture hashes of protected operator/launcher files before any runtime
      setup. Keep personal investigation notes outside tracked files.
- [ ] Confirm selected Rust/Node/Viceroy versions; install Viceroy 0.21.0
      under owned scratch if an isolated matching binary is unavailable.
- [ ] Capture disabled baseline response/header/record-I/O fixtures using
      the #1646 revision. Run the existing targeted returning-reader tests.
      Fix source-version/clock inputs in comparison fixtures so expected
      build-version metadata does not conceal changes to cookie behavior.

## 2. Test and add typed configuration

Files: `crates/trusted-server-core/src/settings.rs`,
`crates/trusted-server-core/src/config.rs`, and relevant typed config tests.

- [ ] Add failing tests for omitted, 1, 364, zero, negative, 365, wrong-type
      and overflow inputs. Require the field name and range in errors.
      Test TOML, JSON and the push-valid config wrapper.
- [ ] Add a checked `RefreshAfterDays` value type and an optional EC field.
      Serialize the inner integer only when present. Use checked seconds
      conversion and field-specific deserialization errors. Update necessary
      explicit EC constructors without unrelated refactors.
- [ ] Test absence round-trips and published-envelope equivalence to the
      baseline. Add no serving environment reads or environment tests that
      accidentally enable the feature through test-only legacy loaders.
- [ ] Run new settings/config tests red before production edits, then green.

## 3. Test and implement the authenticated marker

Create: `crates/trusted-server-core/src/ec/refresh.rs` (private core module).
Modify: `ec/mod.rs`, `ec/cookies.rs`, and `constants.rs` as narrowly required.

- [ ] Write pure tests using explicit Unix times for the spec's `v1.i`
      protocol, canonical encoding, EC/domain binding, full MAC,
      unknown version, duplicates, length bounds, malformed base64, future
      timestamps, clock failure, and exact N-day boundary. Verify they fail.
      Include a fixed MAC vector matching the spec's exact framing.
- [ ] Implement bounded parsing, authenticated token creation/verification
      and a small decision enum: unchanged or renew. Reuse existing
      HMAC/SHA256/base64 crates and EC passphrase. Add no production clock
      override and no runtime environment read.
- [ ] Capture optional incoming marker across Cookie fields before routing,
      only with the setting enabled. Keep timestamp data out of records and
      logs. Prove disabled input parsing and record calls remain unchanged.
- [ ] Run the new module tests green; format touched Rust files.

## 4. Integrate document-only renewal test-first

Files: `ec/app/lifecycle.rs`, `ec/finalize.rs`, `ec/mod.rs`, and existing
`ec/app/route_tests.rs` / identity conformance tests where appropriate.

- [ ] Add failing full-finalizer tests for below/exact/above threshold,
      same EC output and Max-Age, EC-plus-marker bootstrap, then immediate
      no-rewrite. Test explicit absent-setting behavior against baseline.
- [ ] Capture publisher-only authorization after request filters and combine
      it with final HTML/XHTML/304 classification. EC API permits alone
      cannot renew. Preserve method/status semantics from #1646.
- [ ] Require matching incoming cookie, final active EC, and consented
      Present snapshot. Load an unread row only when enabled renewal or
      bootstrap needs it; never infer liveness from the success flag alone.
      Suppress renewal after failed page-load CAS and on missing, failed,
      unreadable or tombstoned storage. Keep write counts unchanged.
- [ ] Add enabled companions to genuine creation/rotation issuance, including
      initial EC POST creation; do not treat those APIs as renewal candidates.
      Validate header preparation and failure behavior described in the spec.
- [ ] Add withdrawal cleanup before early exits, including companion-only
      withdrawal and unavailable stores; reconstruct deletion after identity
      filtering on ineligible responses. Keep disabled paths untouched.
- [ ] Test assets, API/EC POST with forged navigation headers, redirects,
      nonredirect errors, HEAD/POST documents, 304, no consent, unknown geo,
      304 with non-HTML Content-Type, header-only/mismatched EC, orphan
      recovery and failed writes. Green
      tests must assert final headers, not just decision helper results.

## 5. Preserve shared-cache and cookie behavior

Files: `settings.rs`, `cookies/template_cache_policy.rs`,
`response_privacy.rs`, `publisher.rs`, adapter middleware/terminal tests,
and `ec/sharedid.rs` tests as required by actual coverage gaps.

- [ ] Add failing enabled tests rejecting `ts-ec-issued-at` as a template key
      and as a configured SharedID/companion collision. Demonstrate disabled
      configs retain baseline validation behavior. Keep configured SharedID
      denial unchanged.
- [ ] Verify bootstrap and renewal responses pass through existing
      privacy guards, stripping all edge-cache headers despite public origin
      or operator directives. Cover Fastly's late finalization explicitly.
- [ ] Keep raw templates free of companion Set-Cookie headers; preserve
      SharedID/companion and mirror equality tests. Add no unrelated policy.
- [ ] Document the setting in the existing configuration guide and example
      as a commented, off-by-default proposal; state rollback order, browser
      scope, immediate bootstrap, TTL limitation and Safari qualification.

## 6. Targeted validation

Run from the proposal worktree with pinned tools; record each result. Do not
run the full local CI gate list. Commands with test filters cover the actual
new module names selected during implementation.

```bash
cargo fmt --all -- --check
cargo test --locked -p trusted-server-core --lib ec::refresh::tests::
cargo test --locked -p trusted-server-core --lib ec::finalize::tests::
cargo test --locked -p trusted-server-core --lib ec::app::
cargo test --locked -p trusted-server-core --lib ec::sharedid::tests::
cargo test --locked -p trusted-server-core --lib cookies::template_cache_policy::tests::
cargo test --locked -p trusted-server-core --lib response_privacy::tests::
cargo test --locked -p trusted-server-core --lib publisher::tests::template_cache_gate_tests::
cargo test --locked -p trusted-server-core --lib publisher::tests::template_cache_end_to_end_tests::
cargo test-axum
scripts/test-runtime-env.sh
cargo clippy-axum
cargo clippy-fastly
```

- [ ] Run focused settings/config/CLI publication tests discovered in steps
      2 and 5, including byte-equivalent omission. Do not replace them with
      a serving-only test.
- [ ] Under isolated Viceroy 0.21.0, run the core refresh, finalize, SharedID,
      template-cookie and response-privacy filters above with
      `--target wasm32-wasip1`, plus
      `cargo test --locked -p trusted-server-adapter-fastly --target wasm32-wasip1 middleware::tests::`.
      Also run `ec::app::` under that target; run the Fastly adapter's
      `identity_conformance::` filter if its fixtures/assertions change.
- [ ] Run the actual runtime-environment guard for the affected Axum/Fastly
      packages using `scripts/clippy-runtime-env` and the same compiler
      disallowed-method/type flags as `scripts/lint-runtime-env.sh`, including
      shared packages explicitly and Fastly default/reusable branches. Keep
      the full hosted guard, including other runtimes, as hosted evidence.
- [ ] Format/check changed Markdown using the docs-pinned Prettier and run
      the changed-line domain linter. Run no unrelated documentation build
      or full CI matrix locally.

## 7. Runtime verification before publication

- [ ] Build real Axum and Fastly artifacts from the proposal head. Prepare
      disposable app-config copies through the documented config-push and
      prepared-store flow. Never edit operator config or `.env`.
- [ ] Follow `.vscode/tasks.json` startup semantics with reversible, owned
      launcher/config copies. Use local record storage and controlled origin/
      consent fixtures; record effective config, binary digest and revision.
      These ignored task files are absent from an outside worktree: read
      them from the operator checkout, copy only needed launcher files into
      an owned launcher directory, and point source/config paths at the
      proposal worktree/disposable config. Do not source original `.env`
      files or modify original launchers. If the copy uses the obsolete
      config environment bridge, use equivalent prepared-store startup
      below, recording exact commands and paths in the private launch record.

```bash
ts config push --local --adapter axum --manifest "<scratch>/edgezero.toml" --app-config "<scratch>/trusted-server.toml" --no-diff -y
# Use existing prepared-store and synthetic-secret inputs for this owned process.
cargo run --locked -p trusted-server-adapter-axum --features geo-cloudfront
cargo build --locked -p trusted-server-adapter-fastly --target wasm32-wasip1
"<isolated-viceroy>/bin/viceroy" --addr 127.0.0.1:<port> -C "<scratch>/fastly.toml" target/wasm32-wasip1/debug/trusted-server-adapter-fastly.wasm
ts dev proxy --map www.publisher.example=127.0.0.1:<port> --upstream-plaintext --rewrite-host --listen 127.0.0.1:<proxy-port>
"<chrome-binary>" --user-data-dir="<scratch>/chrome-profile" --remote-debugging-port=<debug-port> --proxy-server=http://127.0.0.1:<proxy-port> --no-first-run
```

Use separate manifests, ports and profiles for each adapter. Reconcile the
task copy and these command templates with the pinned CLI's help before
execution. Record existing prepared-store inputs and effective selections
without secrets; do not mistake these templates for a complete runnable
configuration. Trust must come from an existing authorized proxy CA.

- [ ] Use fresh owned Chrome profiles through the publisher proxy for each
      runtime. With setting unset, compare returning headers/cookies with
      baseline, including a stale/invalid carried companion ignored entirely.
- [ ] With N=1, prove missing-marker bootstrap refreshes the same EC plus its marker;
      seed an old authenticated `i` marker for the actual EC using synthetic
      fixture keys and the published token protocol. No production time
      override. Observe same-value EC renewal, unchanged attributes/full
      Max-Age, private cache policy and removed edge-cache directives.
- [ ] Immediately revisit: no renewal cookies. Exercise unknown/no consent
      and explicit withdrawal with an overdue valid marker; no renewal, and
      required deletion. Verify ineligible asset/API requests cannot renew.
- [ ] Record this as Chrome plus local stores/controlled consent and seeded
      time, not real CMP, Safari, deployed Fastly, or AWS qualification.

## 8. Draft PR, review and cleanup

- [ ] Explain the concrete change and results before signed commits/push.
      Confirm local signatures and GitHub Verified for every new commit.
      Refresh the base by signed merges if it moves; never force-push.
      Record the base/head SHAs used for comparison/signature inspection;
      recheck remote proposal SHA, draft state and base metadata after push.
- [ ] Create a DRAFT using the repository's `.claude/agents/pr-creator.md`.
      Title: `Propose off-by-default EC threshold refresh for DEC-13`.
      Start the body with the exact warning at the spec's top. Include the
      A/B/C comparison, bootstrap/TTL/replay trade-offs, verification limits
      and `Part of #1595`. Do not claim DEC-13 is decided or close the issue.
- [ ] Watch every hosted check finish. Resolve proposal failures within
      scope, preserving the approval boundary if the design must change.
- [ ] Independently self-review the full proposal diff and resolve every
      blocking and optional finding. Update only this proposal's description.
      Never comment on other PRs/issues, touch the board, or merge.
- [ ] Stop owned servers/proxies/Chrome, remove profiles/disposable config/
      scratch, remove clean worktrees, and confirm with `git worktree list`.
      Compare protected-file hashes and verify owned ports/profile processes
      are gone before declaring cleanup complete.
      Preserve unfinished work through verification and handoff.
