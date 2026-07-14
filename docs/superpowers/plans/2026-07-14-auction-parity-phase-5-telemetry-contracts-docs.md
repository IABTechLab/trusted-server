# Auction Parity Phase 5: Telemetry, Contracts, and Documentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit privacy-safe server telemetry exclusively from canonical auction outcomes,
lock the separate browser lifecycle event contract without adding ingestion, enforce
Rust/TypeScript/Tinybird contract parity in CI, and remove superseded code and obsolete
documentation.

**Architecture:** Refactor the existing telemetry builder to consume Phase 2/3 domain types rather than reconstructing facts from path-specific responses. Add versioned repository fixtures shared by Rust, TypeScript, and Tinybird validation, distinguish server candidate selection from browser render events, and finish with compiler-guided cleanup only after every caller has migrated.

**Tech Stack:** Rust 2024, `serde`, Tinybird datasource/pipe fixtures, TypeScript, Vitest, Node validation scripts, GitHub Actions, VitePress.

**Reference spec:** `docs/superpowers/specs/2026-07-14-server-client-auction-parity-design.md` sections 15-21 and section 17 Phase 5.

**Depends on:** Phases 1-4 completed and their compatibility migrations released or ready for atomic removal.

**Execution workspace:** Continue on `feat/auction-parity-foundation`; do not create a worktree unless the user changes that decision.

---

## File map

### Create

- `contracts/auction/client-request-v1.json` — unversioned legacy request compatibility fixture.
- `contracts/auction/client-request-v2.json` — TypeScript-produced canonical request fixture.
- `contracts/auction/openrtb-response-v2.json` — Rust-produced `/auction` response fixture.
- `contracts/auction/injected-bids-v2.json` — Rust-produced initial-navigation bid-map fixture.
- `contracts/auction/page-bids-v2.json` — Rust-produced SPA page-bids fixture.
- `contracts/auction/browser-events-v1.json` — render/win/billing lifecycle fixture.
- `contracts/auction/telemetry-v2.ndjson` — canonical Tinybird row fixture.
- `contracts/auction/manifest.json` — contract versions, producers, and consumers.
- `scripts/check-auction-contracts.mjs` — fixture shape/version and Tinybird schema comparison.
- `crates/trusted-server-js/lib/test/contracts/auction_contracts.test.ts` — TypeScript consumer tests.

### Modify

- `crates/trusted-server-core/src/auction/telemetry.rs` and telemetry call sites in `endpoints.rs`, `publisher.rs`, and `orchestrator.rs`.
- `crates/trusted-server-core/src/auction/outcome.rs` and `projection.rs`.
- `crates/trusted-server-js/lib/src/core/lifecycle.ts` — browser-local event contract; no ingestion endpoint.
- `crates/trusted-server-adapter-fastly/src/tinybird.rs` and `crates/trusted-server-core/src/platform/mod.rs` — Tinybird sink and runtime-service wiring.
- `tinybird/datasources/auction_events_raw.datasource`, fixtures, pipes, and Tinybird tests.
- `crates/trusted-server-integration-tests/tests/parity.rs`.
- `.github/workflows/format.yml`, `.github/workflows/test.yml`, and `.github/workflows/integration-tests.yml`.
- Public auction, Prebid, GPT, EC, configuration, and telemetry guides.
- Superseded helpers/comments identified after all migrations.

---

### Task 1: Make telemetry consume canonical outcomes

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry.rs:1-850`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs`
- Test: telemetry module

- [ ] **Step 1: Write failing canonical telemetry tests**

Build `AuctionOutcome` fixtures for completed, no-bid, partial failure, all failure,
dispatch failed, and abandoned, plus an admission-terminal fixture for attempts rejected
before a canonical request exists. Assert exactly one summary row, one row per real
provider outcome, one row per valid candidate, and winner flags derived from canonical
candidate keys. Assert every row for an attempt retains the UUID allocated at admission.

- [ ] **Step 2: Run telemetry tests and verify RED**

Run: `cargo test-axum auction::telemetry -- --nocapture`

Expected: existing telemetry APIs require legacy `AuctionRequest`/`OrchestrationResult` or reconstruct winners heuristically.

- [ ] **Step 3: Replace legacy telemetry input with canonical types**

```rust
pub enum AuctionTelemetryInput<'a> {
    Outcome(&'a AuctionOutcome),
    AdmissionTerminal {
        attempt: &'a AuctionAttemptObservation,
        terminal: AuctionTerminal,
    },
}

pub struct AuctionAttemptObservation {
    pub auction_id: uuid::Uuid,
    pub source: AuctionSource,
    pub telemetry_path: Option<String>,
    pub terminal_reason: AuctionTerminalReason,
}

pub fn build_auction_events(input: AuctionTelemetryInput<'_>) -> AuctionEventBatch;
```

For outcomes, use `outcome.auction_id` and its canonical request summary. For early
admission terminals, use the attempt ID carried by Phase 1's admission result. Remove
the second telemetry-only UUID; never regenerate IDs in request building, outcomes, or
telemetry. Use only the stored normalized telemetry path and never parse a page URL in
the serializer.
Implement `AuctionAttemptObservation::from_admission` and `::from_denial` in
`telemetry.rs`. They copy only UUID, source, already-normalized path, and a bounded typed
terminal reason; tests must prove consent, EC/EIDs, IP/geo, headers, query strings, and
raw URLs cannot enter this view.

- [ ] **Step 4: Run telemetry and path tests and verify GREEN**

Run: `cargo test-axum telemetry -- --nocapture`

Expected: canonical summary/provider/candidate fixtures pass for every terminal state.

- [ ] **Step 5: Commit canonical telemetry input**

```bash
git add crates/trusted-server-core/src/auction/telemetry.rs crates/trusted-server-core/src/auction/endpoints.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/auction/orchestrator.rs
git commit -m "Build telemetry from canonical outcomes"
```

### Task 2: Capture provider and total timing at the correct boundaries

**Files:**

- Modify: `crates/trusted-server-core/src/auction/outcome.rs`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry.rs`
- Test: orchestrator and telemetry modules

- [ ] **Step 1: Write failing controlled-clock tests**

Use a test clock or supplied durations to simulate fast provider + slow origin streaming, out-of-order providers, mediator time, and abandoned dispatch. Assert provider time ends on provider completion and total auction time ends when the canonical outcome is decided, not at origin EOF.

- [ ] **Step 2: Run timing tests and verify RED**

Run: `cargo test-axum provider_timing_ -- --nocapture`

Expected: at least split-phase total time includes later origin/stream work or loses a completion timestamp.

- [ ] **Step 3: Store timing facts on provider/outcome records**

Snapshot start once, record each launch/completion, and finalize outcome duration before projection/streaming. Avoid reading clocks in the telemetry serializer.

- [ ] **Step 4: Run timing tests and verify GREEN**

Run: `cargo test-axum provider_timing_ -- --nocapture`

Expected: controlled durations match exactly.

- [ ] **Step 5: Commit timing boundaries**

```bash
git add crates/trusted-server-core/src/auction/outcome.rs crates/trusted-server-core/src/auction/orchestrator.rs crates/trusted-server-core/src/auction/telemetry.rs
git commit -m "Capture canonical auction timing"
```

### Task 3: Separate server candidate telemetry from browser lifecycle events

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry.rs`
- Modify: `crates/trusted-server-js/lib/src/core/lifecycle.ts`
- Test: Rust telemetry/event tests and TypeScript lifecycle tests

- [ ] **Step 1: Write failing semantic tests**

Assert the server can emit `candidate_winner` but never `impression`/`rendered` by
inference. Browser-local `render_confirmed`, `render_failed`, `win_confirmed`, and
`billing_confirmed` events must carry the canonical auction/bid event key, contain
no EC/EIDs/creative/query strings, and be idempotent.

- [ ] **Step 2: Run Rust and Vitest tests and verify RED**

Run:

```bash
cargo test-axum lifecycle_telemetry -- --nocapture
cd crates/trusted-server-js/lib
npx vitest run test/core/lifecycle.test.ts
```

Expected: telemetry currently has only bid `is_win` semantics and no explicit
browser-event semantic boundary.

- [ ] **Step 3: Add typed event rows without inventing a transport**

Model server `candidate_winner` separately from the browser-local render, win, and
billing event types. Validate event enum, auction/bid ID lengths, and dedup key in
pure constructors. Keep browser lifecycle events separate from provider notice
delivery and do not add a new public ingestion endpoint in this phase: the frozen
spec says "when collected" but does not approve a transport or retention policy.
Adding collection requires a separate admission/privacy design.

- [ ] **Step 4: Run semantic tests and verify GREEN**

Run the commands from Step 2.

Expected: candidate and render lifecycle semantics remain distinct.

- [ ] **Step 5: Commit lifecycle telemetry**

```bash
git add crates/trusted-server-core/src/auction/telemetry.rs crates/trusted-server-js/lib/src/core/lifecycle.ts crates/trusted-server-js/lib/test/core/lifecycle.test.ts
git commit -m "Distinguish auction render telemetry"
```

### Task 4: Create shared versioned contract fixtures

**Files:**

- Create: `contracts/auction/manifest.json`
- Create: `contracts/auction/client-request-v1.json`
- Create: `contracts/auction/client-request-v2.json`
- Create: `contracts/auction/openrtb-response-v2.json`
- Create: `contracts/auction/injected-bids-v2.json`
- Create: `contracts/auction/page-bids-v2.json`
- Create: `contracts/auction/browser-events-v1.json`
- Create: `contracts/auction/telemetry-v2.ndjson`
- Modify: `crates/trusted-server-core/src/auction/formats.rs`
- Modify: `crates/trusted-server-core/src/auction/projection.rs`
- Modify: `crates/trusted-server-core/src/auction/telemetry.rs`
- Create: `crates/trusted-server-js/lib/test/contracts/auction_contracts.test.ts`

- [ ] **Step 1: Write failing Rust and TypeScript consumers against fixture paths**

Make the fixtures bidirectional producer/consumer contracts rather than examples:

- The TypeScript production builder emits a deterministic version 2 client request
  that is compared byte-for-byte/structurally with its checked-in fixture; Rust's
  production deserializer consumes it. A separate checked-in legacy fixture omits the
  version field and is consumed only by Rust's explicit version 1 compatibility path;
  new TypeScript code must never emit it.
- Rust production projection serializers emit deterministic OpenRTB response,
  injected-bid map, page-bids response, and telemetry fixtures; TypeScript production
  parsers/render-input builders consume every corresponding fixture.
- Rust and TypeScript both consume the browser lifecycle fixture and validate the full
  canonical event identity.

Cover every optional field, absent/empty variants, hostile inline markup, cache
fallback, notices/event trackers, and every supported wire version.

- [ ] **Step 2: Run consumers and verify RED**

Run:

```bash
cargo test-axum auction_contract_fixture -- --nocapture
cd crates/trusted-server-js/lib
npx vitest run test/contracts/auction_contracts.test.ts
```

Expected: fixture files and matching production contracts do not yet exist.

- [ ] **Step 3: Add the minimal fixtures and manifest**

The manifest records version, owner, exact producer test, exact consumer test, and
deprecation policy for each wire shape. Producer tests must fail when checked-in output
drifts, and consumer tests must call production parsing/building code rather than
deserializing into test-only copies. Keep secrets and real identity values out of
fixtures. Use deterministic UUIDs/timestamps only in tests.

- [ ] **Step 4: Run consumers and verify GREEN**

Run the commands from Step 2.

Expected: Rust and TypeScript agree on every fixture.

- [ ] **Step 5: Commit cross-language contracts**

```bash
git add contracts crates/trusted-server-core crates/trusted-server-js/lib/test/contracts
git commit -m "Add versioned auction wire contracts"
```

### Task 5: Enforce Tinybird datasource parity

**Files:**

- Create: `scripts/check-auction-contracts.mjs`
- Modify: `tinybird/datasources/auction_events_raw.datasource`
- Modify: `tinybird/fixtures/auction_events_raw.ndjson`
- Modify: `tinybird/pipes/auction_summary.pipe`
- Modify: `tinybird/pipes/auction_overview_mv.pipe`
- Modify: `tinybird/pipes/auction_provider_stats_mv.pipe`
- Modify: `tinybird/pipes/auction_bid_stats_mv.pipe`
- Modify: `tinybird/pipes/provider_latency.pipe`
- Modify: `tinybird/pipes/seat_yield.pipe`
- Modify: `tinybird/tests/auction_summary.yaml`
- Test: `scripts/check-auction-contracts.mjs` and Tinybird tests

- [ ] **Step 1: Write a failing schema comparison**

The script must parse the datasource schema and telemetry NDJSON, verify every emitted key has a compatible column/type/nullability, detect datasource columns absent from all fixtures, and require manifest version changes for breaking field changes.

- [ ] **Step 2: Run the script and verify RED**

Run: `node scripts/check-auction-contracts.mjs`

Expected: current schema/fixture semantics do not include the new canonical server
candidate/failure fields or the script does not exist.

- [ ] **Step 3: Update datasource, fixtures, pipes, and tests atomically**

Keep server candidate price separate from mediated clearing/original provider price and
use server event kinds such as `candidate_winner` rather than impression/rendered.
Validate `browser-events-v1.json` as a separate repository contract, but do not add
browser-confirmation rows or columns to Tinybird until a future ingestion design is
approved. Update materialized views only where their meaning remains correct; add
migrations/release notes for incompatible production schema changes.

- [ ] **Step 4: Run local contract and Tinybird validation**

Run:

```bash
node scripts/check-auction-contracts.mjs
tb test
```

Expected: the Node check exits zero. If Tinybird CLI is not installed/configured locally, record that as an environment gap and require the CI Tinybird job to pass before merge.

- [ ] **Step 5: Commit Tinybird parity**

```bash
git add scripts/check-auction-contracts.mjs tinybird contracts/auction/telemetry-v2.ndjson
git commit -m "Align auction telemetry schema"
```

### Task 6: Add contract and adapter parity gates to CI

**Files:**

- Modify: `.github/workflows/format.yml`
- Modify: `.github/workflows/test.yml`
- Modify: `.github/workflows/integration-tests.yml`
- Test: workflow syntax and local commands

- [ ] **Step 1: Add failing/local-equivalent gate documentation**

List the exact jobs required for Rust contract fixtures, Vitest consumers, Node schema comparison, adapter parity, and Tinybird tests. Avoid duplicating expensive builds across workflows when an existing job can own the check.

- [ ] **Step 2: Add the commands to CI**

Pin Node through the repository tool version. Cache dependencies using existing workflow patterns. Ensure a contract change cannot merge with only one language/schema updated.

- [ ] **Step 3: Validate workflow YAML and run local equivalents**

Use the docs workspace's pinned Prettier rather than downloading a root-level version:

```bash
cd docs
npx prettier --check ../.github/workflows/format.yml ../.github/workflows/test.yml ../.github/workflows/integration-tests.yml
```

Then execute the commands introduced in Tasks 4-5 locally.

Expected: YAML parses and all available local gates exit zero.

- [ ] **Step 4: Inspect CI diff for least privilege and secret safety**

Tinybird validation must use read/test credentials only where required; fixture/schema comparison needs no network or secrets.

- [ ] **Step 5: Commit CI enforcement**

```bash
git add .github/workflows scripts/check-auction-contracts.mjs
git commit -m "Enforce auction contract parity in CI"
```

### Task 7: Replace obsolete public documentation with the deployed two-path guide

**Files:**

- Modify: `docs/guide/auction-orchestration.md`
- Modify: `docs/guide/ad-serving.md`
- Modify: `docs/guide/integrations/prebid.md`
- Modify: `docs/guide/integrations/gpt.md`
- Modify: `docs/guide/edge-cookies.md`
- Modify: `docs/guide/ec-setup-guide.md`
- Modify: `docs/guide/integration-guide.md`
- Modify: `docs/guide/configuration.md`
- Modify: `docs/guide/architecture.md`
- Modify: `docs/guide/creative-processing.md`
- Modify: `docs/guide/api-reference.md`
- Modify: `trusted-server.example.toml`
- Modify: `README.md`
- Modify: `crates/trusted-server-core/src/auction/README.md`
- Modify: `crates/trusted-server-integration-tests/README.md`

- [ ] **Step 1: Inventory stale claims**

Search for obsolete Equativ configuration, nonexistent endpoints, `auto_configure`, debug-only `adm`, raw EID response headers, EC-derived auction IDs, unqualified `user.ext.eids` downstream claims, and missing adapter limitations.

Run:

```bash
rg -n -i "equativ|auto_configure|x-ts-eids|debug.*adm|ec.*auction id|user\.ext\.eids|page-bids|requestAds" docs README.md crates/*/README.md
```

- [ ] **Step 2: Write one current operational narrative**

Clearly distinguish automatic initial-navigation SSAT, SPA page-bids, direct core `requestAds`, custom Prebid adapter, and native client bidders. Include rendering/PUC, notice lifecycle, identity provenance, timeout tiers, refresh, media capabilities, adapter matrix, telemetry semantics, and migration breaks.

- [ ] **Step 3: Update configuration reference**

Document `creative_opportunities`, page patterns, GAM templates, allowed context keys, Prebid account ID, provider/adapter capabilities, identity/KV differences, debug policy, and USD-only behavior.

- [ ] **Step 4: Run docs formatting/build and link checks**

Run:

```bash
cd docs
npm run format
npm run build
```

Expected: both commands exit zero with no broken internal-link build failures.

- [ ] **Step 5: Commit public documentation**

```bash
git add docs README.md trusted-server.example.toml crates/trusted-server-core/src/auction/README.md crates/trusted-server-integration-tests/README.md
git commit -m "Publish unified auction operations guide"
```

### Task 8: Remove superseded helpers and compatibility code

**Files:**

- Modify/delete only compiler-confirmed obsolete auction helpers across Rust/TypeScript
- Test: full repository

- [ ] **Step 1: Produce a removal inventory from actual references**

Use `rg` and compiler/TypeScript unused diagnostics to identify duplicate request builders, legacy response serializers, old EID header encoders, URL-based notice dedupe, mutable refresh fallbacks, debug render branches, and stale telemetry reconstruction.

- [ ] **Step 2: Delete one coherent obsolete surface at a time**

After each deletion, run the narrow owning tests. Do not mix behavior changes into cleanup and do not remove a compatibility parser until the documented migration window is complete.

- [ ] **Step 3: Run format/lint after mechanical cleanup**

Run Rust format and every target-matched clippy plus JS format. Treat new warnings as failures.

- [ ] **Step 4: Inspect the final diff for unrelated refactors**

Every deletion must map to a migrated caller or explicit compatibility removal in the spec.

- [ ] **Step 5: Commit cleanup**

```bash
git add crates docs
git commit -m "Remove superseded auction paths"
```

### Task 9: Execute the final program verification and handoff

**Files:**

- Modify only failures discovered by verification, with focused regression tests first

- [ ] **Step 1: Run the complete repository gate from the frozen spec**

Run every command in master spec section 18.3: all Rust format/clippy/test aliases, CLI/integration tests where applicable, JS build/Vitest/format, docs format/build, contract script, Tinybird tests, and browser integration scenarios.

- [ ] **Step 2: Re-run any failed command after a focused fix**

Do not infer that adjacent targets pass. Record command, exit status, and failure count.

- [ ] **Step 3: Audit all completion criteria line by line**

Map evidence to all 12 master-spec completion criteria and all S/R/E/B/J/T/C findings. Any missing evidence remains open.

- [ ] **Step 4: Request final code review**

Use `superpowers:requesting-code-review` with the frozen spec, five phase plans, commit range, verification evidence, and known environment limitations.

- [ ] **Step 5: Finish the development branch**

Use `superpowers:finishing-a-development-branch` only after review findings are resolved and fresh verification is green. Present merge/PR/cleanup options; do not silently merge or push.

---

## Program completion checkpoint

The auction parity program is complete only when canonical server outcomes drive server
telemetry, browser lifecycle events have a distinct versioned local contract without
being misreported as server impressions, Rust/TypeScript/Tinybird contracts cannot
drift in CI, public docs describe deployed behavior, obsolete paths are removed only
after migration, and fresh evidence satisfies every command and completion criterion
in the frozen master spec.
