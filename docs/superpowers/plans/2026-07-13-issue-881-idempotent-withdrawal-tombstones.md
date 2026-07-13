# Issue #881: Idempotent EC Withdrawal Tombstones Plan

- **Date:** 2026-07-13
- **Status:** Implemented and verified
- **Issue:** [#881 — Make EC withdrawal tombstoning idempotent across request bursts](https://github.com/IABTechLab/trusted-server/issues/881)
- **Stack base:** [Draft PR #900 — Avoid no-op EC KV reads in post-send pull sync](https://github.com/IABTechLab/trusted-server/pull/900)
- **Underlying dependency:** [PR #885 — Request-scoped EC KV snapshot and orphan recovery](https://github.com/IABTechLab/trusted-server/pull/885)

## Goal

Make explicit EC withdrawal idempotent across repeated and concurrent requests
without weakening the existing-key-only privacy invariant introduced by PR
#885. The first successful withdrawal of a live row writes a CAS-protected
24-hour tombstone. A request that already has authoritative tombstone state
returns without reading or writing KV, so it cannot refresh the tombstone's
entry timestamp or TTL.

Browser-cookie deletion remains synchronous and best-effort KV failure must
never block the response.

## Clarified Semantics

- An authoritative missing row is a no-op. A valid-looking but unverified
  browser cookie must never create a KV root.
- A matching authoritative tombstone snapshot is returned unchanged before any
  lookup, serialization, or write, regardless of whether its generation is
  available.
- A matching live snapshot with a generation uses one conditional write.
- A live snapshot without a generation, a failed/not-read snapshot, or a
  snapshot for another EC ID rereads the requested row before deciding.
- CAS conflicts reread and retry. If another withdrawal has already written a
  tombstone, retry ends without another write. If a concurrent live update or
  re-consent changed the generation first, withdrawal retries against that live
  row and tombstones it.
- A later re-consent may legitimately win when it linearizes after a completed
  or no-op withdrawal. Idempotency does not impose global withdrawal priority.
- Repeated withdrawal preserves the original tombstone expiration because it
  performs no second write.
- When cookie and active EC IDs differ, every valid existing row is withdrawn
  independently; missing or malformed IDs are never created.
- `ts-ec` and the pull-completeness marker are expired before best-effort KV
  work. Store failure is logged/swallowed by finalization.

## Non-Goals

- Do not recreate missing roots from browser cookies.
- Do not add a cross-request lock, deduplication store, or withdrawal cookie.
- Do not restrict withdrawal handling to document navigations.
- Do not change the 24-hour tombstone duration or KV schema.
- Do not change EC generation, marker behavior, batch sync, pull sync, or
  partner-upsert semantics beyond preserving tombstone rejection.
- Do not make withdrawal dominate a re-consent that occurs after withdrawal's
  linearization point.
- Do not rewrite archival specs that describe the superseded unconditional
  helper.

## Current Behavior

`KvIdentityGraph::tombstone_existing_from_snapshot` already preserves PR #885's
existing-key-only and CAS behavior, but it writes a fresh tombstone whenever the
snapshot is `Present`, including when that entry is already a tombstone. Parallel
withdrawals therefore converge safely but still perform a redundant CAS write,
and repeated requests reset the tombstone's 24-hour TTL.

The older public `write_withdrawal_tombstone` unconditional-overwrite helper is
now dead in production but remains available and could bypass the conditional
path in future code.

Finalization already:

- expires browser state before KV work;
- collects both valid cookie and active EC IDs;
- uses the carried snapshot only for its matching ID;
- independently resolves the other ID;
- logs/swallows KV failures.

The implementation should therefore remain concentrated in the core KV method,
with finalization changes limited to acceptance-level integration tests unless a
test exposes a defect.

## Proposed Design

### 1. Add an authoritative tombstone fast path

At the beginning of each `tombstone_existing_from_snapshot` retry iteration,
match a `Present` snapshot only when its `ec_id` equals the requested ID.

- If `entry.consent.ok == false`, return the exact snapshot immediately.
- Perform this check before requiring a generation or constructing a new
  `KvEntry::tombstone`.
- Preserve the snapshot's entry timestamp and generation exactly.

Then retain the existing state machine:

| Initial/refreshed state                   | Action                              |
| ----------------------------------------- | ----------------------------------- |
| Matching tombstone                        | Return unchanged; zero backend work |
| Matching live entry + generation          | CAS-write a 24-hour tombstone       |
| Matching live entry without generation    | Reread                              |
| Matching `Missing`                        | Return unchanged; never create      |
| `Failed`, `NotRead`, or wrong-ID snapshot | Reread requested ID                 |
| CAS precondition failure                  | Reread and retry                    |
| Row disappears during retry               | Return `Missing`                    |
| Store failure or retry exhaustion         | Return ID-bound `Failed`            |

A successful tombstone remains `consent.ok = false`, has empty partner IDs, and
uses `TOMBSTONE_TTL`.

### 2. Remove the unconditional bypass

Delete the now-unused public `write_withdrawal_tombstone` method and its obsolete
overwrite test. Update `KvIdentityGraph::delete` documentation so it describes
the snapshot-aware conditional withdrawal path without linking to the removed
API.

Repository search must confirm there is no live Rust caller before removal.
Historical design documents may remain unchanged.

### 3. Prove operation-level idempotency

Use a focused recording backend around the existing in-memory store. It must
count every lookup and insert attempt, including inserts that return a CAS
precondition failure, and record each write's mode and TTL. Tests must show:

- a supplied matching tombstone returns with zero lookups and zero insert
  attempts;
- generation-unavailable tombstone state also performs no backend operation;
- the first live withdrawal performs exactly one `IfGenerationMatch` insert
  with `TOMBSTONE_TTL`, while the second causes zero additional insert attempts
  and leaves stored generation and `consent.updated` unchanged;
- two stale live snapshots model parallel requests: the first writes the
  tombstone; the second conflicts, rereads the tombstone, and performs no
  replacement write;
- a supplied tombstone succeeds even against an always-failing backend, proving
  no hidden operation;
- live state with generation avoids an initial read and uses one CAS write;
- missing state remains a no-op;
- `NotRead`, failed, generation-unavailable, and wrong-ID states reread the
  requested row before applying the documented write/no-create behavior.

The recording backend's first-write TTL assertion plus zero additional insert
attempts on repetition is the authoritative proof that the original tombstone
TTL was not refreshed.

### 4. Preserve race ordering and tombstone authority

Extend conflict tests to model a concurrent live update/re-consent that changes
the generation before withdrawal's first CAS. Withdrawal must reread the live
row and eventually write the tombstone, clearing any partner IDs.

Retain the existing batch-sync conditional-upsert and snapshot bulk-upsert
tombstone tests, and add focused coverage for the public single-partner upsert
path so all live enrichment APIs are proven unable to repopulate tombstones:

- `upsert_partner_id_if_exists` rejects tombstones;
- `upsert_partner_id` returns an error and leaves tombstone IDs empty;
- snapshot bulk upsert cannot repopulate a tombstone;
- a disappeared row is not recreated;
- store errors return failed state rather than claiming persistence.

### 5. Verify finalization behavior

Retain the existing finalization coverage for malformed/absent IDs and a
present active ID plus missing secondary ID. Add only the missing integration
cases:

- differing valid cookie and active EC IDs are both tombstoned when both rows
  exist;
- repeated finalization preserves existing tombstone generations;
- a failing KV graph still returns the response and emits both applicable
  browser-cookie expiration headers.

Production finalization should not change unless these tests expose a defect.

## File Map

### Modify

- `crates/trusted-server-core/src/ec/kv.rs`
  - Add the matching-tombstone no-op branch.
  - Remove the unconditional overwrite helper.
  - Update withdrawal documentation.
  - Add operation-count, repetition, and concurrency tests.
- `crates/trusted-server-core/src/ec/finalize.rs`
  - Add two-ID, repeated-withdrawal, and KV-failure integration coverage.

### Add

- `docs/superpowers/plans/2026-07-13-issue-881-idempotent-withdrawal-tombstones.md`
  - Record the reviewed design and verification contract.

No dependency, configuration, adapter, JavaScript, or public wire-format change
is expected.

## Implementation Tasks

### Task 1 — Establish failing idempotency tests

- [x] Add a recording withdrawal backend that counts lookups and every insert
      attempt and captures write mode/TTL.
- [x] Add a supplied-tombstone test proving zero lookups and zero inserts.
- [x] Add repeated and stale-parallel snapshot tests proving the first insert is
      `IfGenerationMatch` with `TOMBSTONE_TTL`, then no further insert occurs and
      generation/`consent.updated` remain unchanged.
- [x] Add live-generation, unavailable-generation, `NotRead`, failed, wrong-ID,
      and missing state tests.
- [x] Run `cargo test-fastly tombstone_existing_from_snapshot` and confirm the
      new repeated/no-backend tests fail before implementation.

### Task 2 — Implement the no-op branch and remove the bypass

- [x] Return a matching tombstone snapshot before generation lookup,
      serialization, or write.
- [x] Keep live/missing/failed/mismatched/CAS behavior unchanged.
- [x] Remove `write_withdrawal_tombstone` and update the `delete` documentation.
- [x] Search for remaining Rust references to the removed helper.
- [x] Run focused KV tests until green.

### Task 3 — Cover concurrent state changes

- [x] Model another withdrawal winning between read and CAS; prove the loser
      rereads and stops without replacing the tombstone.
- [x] Model a concurrent live update/re-consent winning before CAS; prove
      withdrawal retries and tombstones the refreshed row.
- [x] Assert final tombstones contain no partner IDs.
- [x] Add direct `upsert_partner_id` tombstone rejection coverage and re-run the
      existing conditional and snapshot-bulk rejection tests.

### Task 4 — Verify finalization

- [x] Add a test with both differing valid IDs present and assert both become
      tombstones.
- [x] Retain the existing missing and invalid ID tests unchanged as regression
      coverage.
- [x] Add repeated-finalization generation-stability coverage.
- [x] Add a failing-store test proving EC and marker cookie deletion survives KV
      failure.
- [x] Run focused withdrawal/finalization tests.

### Task 5 — Review and full verification

- [x] Run independent correctness/concurrency and test-quality reviews.
- [x] Apply only fixes required by issue scope.
- [x] Mark this plan implemented only after all checks below pass.

## Acceptance Mapping

| Issue requirement                                | Planned evidence                                                              |
| ------------------------------------------------ | ----------------------------------------------------------------------------- |
| First withdrawal establishes a 24-hour tombstone | Live-entry CAS test and existing `TOMBSTONE_TTL` assertion                    |
| Repeated requests avoid overwrite writes         | Operation counts plus unchanged generation and `consent.updated`              |
| Concurrent withdrawal cannot restore IDs         | Stale-snapshot and concurrent-live-update conflict tests                      |
| Both differing valid IDs are withdrawn           | Finalization test with both rows seeded                                       |
| Missing/unverified IDs create no root            | Existing-key-only and invalid-ID tests                                        |
| KV failure remains best-effort                   | Finalization response/cookie test with failing graph                          |
| Late partner updates cannot repopulate           | Conditional, public single, and snapshot-bulk upsert rejection tests          |
| Original TTL is not refreshed                    | First insert records `TOMBSTONE_TTL`; repetition records zero further inserts |

## Verification Contract

Run focused checks during implementation:

```bash
cargo test-fastly tombstone_existing_from_snapshot
cargo test-fastly withdrawal
cargo test-fastly upsert_partner_id_if_exists_rejects_tombstone
cargo test-fastly upsert_partner_id_rejects_tombstone
cargo test-fastly snapshot_upsert_rejects_tombstone
```

Before committing and opening the draft PR, run:

```bash
cargo fmt --all -- --check
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cd crates/trusted-server-js/lib && npx vitest run
cd crates/trusted-server-js/lib && npm run format
cd docs && npm run format
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
git diff --check
```

## Definition of Done

- The first live-row withdrawal writes one CAS-protected 24-hour tombstone.
- Repeated and concurrent withdrawals observing that tombstone perform no
  replacement write and do not refresh its expiration.
- Missing IDs remain absent; the unconditional overwrite API no longer exists.
- Concurrent live updates before successful withdrawal are tombstoned on retry.
- Later partner writes cannot repopulate tombstones.
- Both valid differing IDs are handled independently.
- Browser-cookie deletion remains independent of KV success.
- Focused tests, independent review, and every applicable repository gate pass.

## Risks and Mitigations

- **Snapshot binding:** The no-op branch must require the snapshot ID to match
  the requested EC ID; a tombstone for another ID cannot suppress withdrawal.
- **Linearization:** Returning an observed tombstone linearizes withdrawal at
  that observation. A later re-consent may legitimately win.
- **TTL visibility:** Record the first insert's TTL and every subsequent insert
  attempt at the wrapper boundary; stable generation/timestamp alone is not
  sufficient evidence.
- **Dead API removal:** Compile and repository-search after deletion to catch any
  hidden caller.
- **Conflict-test realism:** Inject actual generation changes and persisted
  state, not endless synthetic precondition failures.
- **Stack dependency:** Reconcile changes if PR #885 or draft PR #900 modifies
  snapshot/finalization contracts before this stack lands.
