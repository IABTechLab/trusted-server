# Issue #882 — Group Batch Sync by EC ID Implementation Plan

## Metadata

- **Issue:** [#882 — Group S2S batch-sync mappings by EC ID before KV updates](https://github.com/IABTechLab/trusted-server/issues/882)
- **Stack base:** Draft PR #901, branch `fix/idempotent-ec-withdrawal-tombstones`
- **Implementation branch:** `perf/group-batch-sync-by-ec-id`
- **Status:** Implemented and verified
- **Date:** 2026-07-13

## Goal

Reduce S2S batch-sync KV work from one read-modify-write per valid mapping to
one call of the existing CAS-protected writer per distinct valid normalized EC
ID, while preserving deterministic last-valid-write-wins behavior and complete
per-input response accounting.

## Approved Behavioral Contract

1. Validate every input before making any writer call.
2. Group only valid mappings by normalized EC ID. Normalization continues to
   lowercase only the 64-character hash prefix; the six-character suffix
   remains case-sensitive.
3. Process groups in the order of their first valid input occurrence. A lookup
   map may locate groups, but only an insertion-ordered vector may determine
   writer-call order.
4. The last valid mapping for a group in request order supplies the one partner
   UID sent to the writer. Invalid mappings never join a group or replace its
   final UID. The required `timestamp` field remains non-ordering.
5. Call the existing `upsert_partner_id_if_exists` writer once per group in the
   uncontended normal path. Its existing bounded CAS loop remains responsible
   for same-key conflicts.
6. `Written` and `Unchanged` accept every valid input in the group.
   `NotFound` and `ConsentWithdrawn` reject every valid input in the group as
   `ineligible`.
7. On infrastructure failure, reject every input in the failing group and all
   unprocessed valid groups as `kv_unavailable`, then stop making writer calls.
   Already processed groups retain their outcomes, and prevalidated invalid
   inputs retain their specific errors.
8. Sort all errors by original input index before responding. Exactly one
   outcome must exist per input, so `accepted + rejected == mappings.len()`.
9. Preserve response fields, reason strings, and status behavior: `200 OK` when
   all mappings are accepted and `207 Multi-Status` when any are rejected.

The group boundary intentionally supersedes the old positional abort boundary.
For inputs `A(valid), B(valid), A(valid)`, if group A succeeds and group B then
fails, both A inputs are accepted even though the second A appears after B's
first input.

## Existing Behavior and Constraints

- `process_mappings` currently validates and writes sequentially, causing a
  writer call for every valid mapping and making duplicate EC IDs contend with
  earlier writes from the same request.
- `KvIdentityGraph::upsert_partner_id_if_exists` already implements existing-key
  only behavior, tombstone rejection, unchanged-value detection, and bounded
  CAS retries. Grouping must reuse it rather than introduce a bulk KV API.
- The request limit is 1,000 mappings. An owned group vector plus an EC-ID to
  group-index lookup map is bounded and appropriate for this limit.
- Authentication, rate limiting, request body limits, request/response schemas,
  and adapter routing are outside this issue.

## File Map

### Modify

- `crates/trusted-server-core/src/ec/batch_sync.rs`
  - Add deterministic validation/grouping and fan-out processing.
  - Upgrade the writer mock to record calls.
  - Add grouped ordering, accounting, outcome, abort, and HTTP tests.
  - Document the internal grouped contract.
- `crates/trusted-server-core/src/ec/kv.rs`
  - Add test-only coverage proving the unchanged conditional writer retries a
    generation conflict and persists the requested UID.
- `docs/guide/api-reference.md`
  - Document validation-before-write, grouping, last-valid-wins, outcome fan-out,
    error ordering, and infrastructure-abort semantics.

### Add

- `docs/superpowers/plans/2026-07-13-issue-882-group-batch-sync-by-ec-id.md`
  - Record the reviewed plan and verification contract.

No dependency, configuration, adapter, JavaScript, public schema, KV API, or
normalization change is expected.

## Implementation Tasks

### Task 1 — Establish failing grouped-processing tests

- [x] Extend `MockWriter` to record ordered `(ec_id, partner_id, uid)` calls
      while retaining queued outcomes.
- [x] Add duplicate and hash-prefix case-variant tests proving one writer call
      per normalized EC ID.
- [x] Add an interleaved-group test proving first-valid-occurrence processing
      order independent of lookup-map iteration order.
- [x] Add conflicting-UID coverage proving the last valid input wins and an
      invalid duplicate cannot override it.
- [x] Add group fan-out tests for `Written`, `Unchanged`, `NotFound`, and
      `ConsentWithdrawn`.
- [x] Run `cargo test-fastly batch_sync` and confirm the new call-count and
      last-valid-wins tests fail before implementation.

### Task 2 — Implement validation and deterministic grouping

- [x] Add a private group representation containing the normalized EC ID, final
      valid partner UID, and all valid original indexes.
- [x] Validate the complete request before any writer call, retaining validation
      errors at their original indexes.
- [x] Maintain a lookup map only for group location and a separate vector as the
      authoritative first-occurrence order.
- [x] Process each group once through the existing `BatchSyncWriter`.
- [x] Fan out success and eligibility outcomes to every valid group member.
- [x] Sort all errors by original input index before returning.
- [x] Assert complete accounting in grouped tests.

### Task 3 — Preserve and test infrastructure-abort semantics

- [x] Add mixed validation/success/failure tests proving validation errors are
      retained and the failing plus unprocessed groups become `kv_unavailable`.
- [x] Prove no writer calls occur after the failing group.
- [x] Add the explicit `A, B, A` test proving a successful early group accepts
      its later-positioned duplicate when B fails.
- [x] Assert final errors are sorted by input index and every input receives
      exactly one outcome.
- [x] Keep infrastructure failures encoded in the response rather than returned
      as a handler error.

### Task 4 — Verify the unchanged CAS boundary and HTTP compatibility

- [x] Add a test using `ConflictInjectingEcKv` that seeds a live entry, injects
      one generation conflict, and proves `upsert_partner_id_if_exists` retries,
      returns `Written`, and persists the requested UID.
- [x] Do not modify production KV behavior, retry limits, or backend APIs.
- [x] Add handler-level grouped tests for `200 OK`, `207 Multi-Status`, serialized
      counts/errors, and `accepted + rejected == mappings.len()`.
- [x] Retain existing auth, rate-limit, body-size, and timestamp tests.

### Task 5 — Document the public contract

- [x] Update module/function commentary in `batch_sync.rs`.
- [x] Expand the batch-sync API reference with normalized grouping,
      first-occurrence processing, last-valid-wins, group outcome fan-out,
      sorted errors, and groupwise infrastructure abort behavior.
- [x] Explicitly document the intentional later-positioned duplicate case.
- [x] Keep the required-but-non-ordering timestamp guidance.

### Task 6 — Review and full verification

- [x] Run independent correctness/performance and test-quality reviews.
- [x] Apply only fixes required by issue scope.
- [x] Mark this plan implemented only after every check below passes.

## Acceptance Mapping

| Issue requirement                                | Planned evidence                                                   |
| ------------------------------------------------ | ------------------------------------------------------------------ |
| Work bounded by distinct valid normalized EC IDs | Recording-writer call count with duplicate and case-variant inputs |
| Validation errors retain indexes                 | Mixed validation/group processing test                             |
| Last valid UID wins                              | Conflicting UID test including an invalid duplicate                |
| Missing and withdrawn fan out consistently       | Duplicate `NotFound` and `ConsentWithdrawn` tests                  |
| Accepted/rejected counts remain compatible       | Unit and handler complete-accounting assertions                    |
| Deterministic processing                         | Interleaved groups and recorded call order                         |
| Infrastructure abort is explicit                 | Failure test covering prior, failing, and unprocessed groups       |
| Errors remain in input order                     | Mixed validation/eligibility/infrastructure sorted-error assertion |
| CAS conflicts remain safe                        | Real conditional-writer conflict-injection test                    |
| Contract is documented                           | Updated module docs and API reference                              |

## Test Design Notes

- Use at least two distinct normalized EC IDs when queuing distinct mock outcomes;
  duplicate inputs now deliberately consume only one outcome.
- Test uppercase/lowercase variants only in the hash prefix. Do not imply the
  suffix is case-insensitive.
- Include invalid EC ID and invalid UID mappings before and after valid members
  to expose accidental positional processing and final-UID replacement.
- Record writer arguments, not only call count, to prove normalized keys,
  partner identity, final UID, and deterministic group order.
- For every processing test, either directly assert or share a helper asserting
  `accepted + errors.len() == mappings.len()`.
- The CAS test covers retries inside the existing writer. The batch writer trait
  must still be called once for that EC group; backend read/write attempts are
  intentionally not equated with trait-call count.

## Verification Contract

Run focused checks while implementing:

```bash
cargo test-fastly batch_sync
cargo test-fastly upsert_partner_id_if_exists_retries_cas_conflict
cargo fmt --all -- --check
cd docs && npx prettier --check guide/api-reference.md superpowers/plans/2026-07-13-issue-882-group-batch-sync-by-ec-id.md
```

Run the complete PR gates before marking the plan verified:

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

cd crates/trusted-server-js/lib
npx vitest run
npm run format
cd ../../..

cd docs
npm run format
cd ..

cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
git diff --check
```

## Risks and Mitigations

- **Nondeterministic map iteration:** never process groups by iterating the
  lookup map; use the insertion-ordered vector.
- **Incorrect normalization:** continue using `normalize_ec_id_for_kv`; do not
  lowercase the suffix.
- **Invalid mapping overrides final UID:** update a group's UID only after both
  EC ID and partner UID validation pass.
- **Double or missing accounting:** fan out exactly once per valid group member,
  retain exactly one validation error per invalid input, and assert totals.
- **Abort fan-out overwrites validation:** unprocessed fan-out traverses only
  already validated group member indexes.
- **Mock masks excess calls:** make missing queued results fail and explicitly
  assert recorded calls.
- **Scope creep into KV design:** production `kv.rs` is unchanged; only add a
  regression test for its existing retry contract.

## Non-Goals

- No new KV bulk API, cross-key transaction, parallel group processing, or
  retry policy.
- No coalescing across separate HTTP requests.
- No change to authentication, rate limits, body limits, maximum batch size,
  timestamps, wire schemas, reason strings, or EC normalization rules.
- No change to root creation, tombstone, or consent semantics.

## Review and Verification Record

- Independent plan review: approved (no blockers)
- Focused tests: passed (22 batch-sync tests plus CAS conflict regression)
- Independent code review: approved after resolving all test/documentation findings
- Full cross-adapter verification: passed
- Release WASM build: passed
