# Axum shared identity implementation plan

**Goal:** Implement #1574 on #1641 with A1, T5 and A9 merged, proving identity
through Axum production startup and preserving Fastly.

**Architecture:** Core owns one identity application and request lifecycle.
Adapters provide prepared EdgeZero records, trusted platform signals and delivery
scheduling. Provider selection remains in EdgeZero bindings.

**Tech stack:** Rust, EdgeZero record API, Axum, Fastly/Viceroy, Node, Docker
DynamoDB Local, Chrome.

## Task 1: Declare the record service and preserve startup

Files: Axum `src/record_ec.rs`, `src/app.rs`, `tests/ec_kv_configuration.rs`,
`tests/startup.rs`, `tests/native_acceptance.rs`; core `src/runtime_env.rs`;
`docs/guide/native-ec-persistence.md`, `native-container.md`; image smoke script.

- [ ] Add assertions that `Hooks::stores` always declares `trusted_server_kv`
      and preparation succeeds without obsolete switches; run to show failure.
- [ ] Remove switch parsing; always call
      `app.require_record_store(STORE_ID, EcRecordLayout)` and return static
      metadata. Keep `RecordEcKvStore::new` capability validation.
- [ ] Give startup fixtures isolated data directories; retain no-identity
      configuration, invalid binding and Unsupported startup regressions.
- [ ] Replace old fixture switches and current docs with binding configuration.
- [ ] Add the exact geo profile name to the checked macro's allowlist with
      #1529/#1532 justification; route its existing read through that macro.
- [ ] Run `cargo test-axum --test ec_kv_configuration --test startup` and
      `scripts/lint-runtime-env.sh`, retaining any initial failures.

## Task 2: Shared request and partner operations

Files: core `src/ec/app.rs`, new `src/ec/app/request.rs` and route helpers as
needed; Axum `src/app.rs`, `src/startup.rs`; Fastly `src/app.rs`, `src/main.rs`,
`src/post_delivery.rs`; Cloudflare/Spin identity call sites.

- [ ] Add failing tests for shared identify/batch responses with/without graph,
      one authoritative graph for admin/partner/lifecycle, and adapter boundary.
- [ ] Introduce core request state/API for reading context, preparing generation,
      finalization, identify/batch/consent dispatch. Preserve request-local state
      and error propagation; use parameter structs to stay under 8 arguments.
- [ ] Move native lifecycle operations to core and inject prepared record store
      into `IdentityApp`; remove parallel graph/registry ownership where possible.
- [ ] Replace Fastly direct generation/finalize/handler calls with core app calls;
      retain lazy storage, limiter, filtering and delivery behavior.
- [ ] Move provider request signals, lookup eligibility/dispatch/collect and
      after-response live-row/permit/commit decisions into core. Keep actual
      successful-delivery scheduling and fresh-client creation at the bridge.
- [ ] Remove remaining non-test adapter direct identity-handler calls, including
      unsupported-store consent paths. Check the complete adapter source set.
- [ ] Run targeted Axum native EC tests and Fastly lifecycle tests after each
      coherent change; add actual behavior regressions for any discovered gap.

## Task 3: Body ingestion and response scope

Files: core `src/auction/endpoints.rs`, `src/ec/{finalize,kv,page_load}.rs` and
core app request/route code.

- [ ] Replace the inherited auction-never-persists regression with a positive
      body-EID persistence assertion. Run it and observe the expected failure.
- [ ] Add denied consent, missing/withdrawn row and concurrent-withdrawal cases.
- [ ] Add an existing-record conditional body-ingest operation; never issue a
      navigation permit, mint an EC or create/recover a missing auction row.
- [ ] Add failing tests for identity cookies on asset/API/non-HTML responses
      including withdrawal/mirror/pull marker inputs.
- [ ] Scope identity cookie output to final HTML document or EC responses while
      preserving authoritative withdrawal writes and other response cookies.
- [ ] Run affected core and adapter regressions; retain input/output evidence.

## Task 4: EC route and portable conformance

Files: core `src/ec/app.rs`, EC route helper, route policy, portable
`src/test_support/identity_conformance.rs`; Axum and Fastly conformance runners;
`scripts/test-identity-conformance{,.test}.mjs` and relevant workflow.

- [ ] Add failing shared tests for same-origin POST creation, failed create,
      cross-origin/missing-Origin rejection, GET204/no mint/SharedID restoration,
      POST withdrawal/tombstone, mirror JSON filtering, no-store and cookie scope.
- [ ] Implement bounded TCF/GPP snapshot preparation and mirror-view-only JSON;
      use the shared creation/finalization operation and private responses.
      Keep inherited `/_ts/consent` compatibility; do not add CMP client work.
- [ ] Add Axum runner using existing trusted service/geo fixture seams, real TCF
      cookies and complete body consumption/response-release observations.
- [ ] Extend portable acceptance with partner locality, body EIDs and EC/API
      cookie scope; run the same assertions against Fastly and Axum.
- [ ] Update runner script tests so both adapters must list and execute the
      exact required test; reject zero-test successes.
- [ ] Run `node scripts/test-identity-conformance.mjs axum` and `fastly`, script
      unit tests, targeted adapter clippy and `cargo fmt --all -- --check`.

## Task 5: Production runtime proof

Use disposable files under OS temporary storage. Read runtime prerequisites
and the existing image smoke harness before building a new probe.

- [ ] Build actual production Axum binary with `aws-dynamodb-ec,geo-cloudfront`.
- [ ] Start owned DynamoDB Local Docker container, create fixture table using
      dummy credentials, and supply record binding and A1 prepared config/secrets.
- [ ] Supply known country using the current protected geo profile and trusted
      loopback peer admission; no geo implementation changes.
- [ ] Exercise consented navigation in isolated Chrome through the publisher
      proxy; inspect cookie plus corresponding DynamoDB record.
- [ ] Exercise store outage, no-store application control, local identify and
      batch sync with a capturing publisher origin.
- [ ] Hold a record write, send SIGTERM while request is active, release it and
      assert completed response, durable write, clean bounded shutdown.
- [ ] Record what existing #1579 CI harness still needs; separate local results
      from unexecuted CI qualification and #1581 broader acceptance.
- [ ] Restore any backed-up local launcher/overlay changes and confirm hashes;
      never edit operator trusted-server.toml or .env. Stop owned resources.

## Task 6: Publication and independent review

- [ ] Re-fetch all four dependency heads; merge any updates and repeat affected
      targeted checks. Keep #1641 as base unless evidence requires a change.
- [ ] Explain final behavior, resolutions, evidence and limitations; create
      explicitly signed commits and verify signatures locally.
- [ ] Push only the new branch, confirm every new commit GitHub Verified, and
      create draft PR using repository template and pr-creator conventions.
      Do not change boards, create issues or comment on dependency PRs.
- [ ] Fresh independent self-review of complete diff against merge base, affected
      unchanged consumers and stack compatibility. Resolve all findings within
      existing implementation authorization, verify corrections and update PR.
- [ ] Remove only owned clean worktrees/scratch/resources after preserving useful
      evidence. Report #1574 coverage, #1579/#1581 residuals, geo AX-2 exception
      and actual merge blockers accurately.

## Review log

- Independent source review confirmed feasibility and acceptance coverage.
  Incorporated concrete EC controls, payload limits, successful-HTML status
  eligibility and explicit provider/scheduling ownership from spec round 2.
  No unresolved blocking or optional document findings remain.
