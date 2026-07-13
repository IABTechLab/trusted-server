# Issue #880: No-Op Pull-Sync KV Read Elimination Plan

- **Date:** 2026-07-13
- **Status:** Implemented and verified
- **Issue:** [#880 — Avoid no-op EC KV reads in post-send pull sync](https://github.com/IABTechLab/trusted-server/issues/880)
- **Stack base:** [PR #885 — Thread EC KV read through the request and recover orphaned cookies](https://github.com/IABTechLab/trusted-server/pull/885)

## Goal

Avoid EC identity-graph work whose only purpose is determining that post-send
pull sync has nothing to do. Return before constructing the post-send graph when
there are no pull-enabled partners, and let browsers carry a short-lived signed
proof that their EC row was recently complete for the current pull-partner set.

The change must preserve PR #885's single request-scoped [`EcKvSnapshot`],
auction EID resolution, request EID ingestion, orphan recovery, withdrawal,
missing-UID-only dispatch, rate limits, CAS protection, and best-effort
post-send failure policy.

## Baseline After PR #885

PR #885 changes the original issue context:

- Publisher navigations can load one EC KV snapshot before response delivery,
  after starting a truly asynchronous publisher-origin request.
- Auction EID resolution, EC finalization, and pull sync reuse that snapshot.
- Pull sync no longer performs its own initial `kv.get`; missing, failed,
  unread, and tombstoned snapshots do not dispatch.
- Fastly still constructs a post-send `KvIdentityGraph` before core proves that
  there are no pull partners or no missing partner IDs.
- Publisher snapshot preload remains unconditional for eligible returning-user
  GET navigations, including requests where no auction runs and pull-sync
  completeness is the only reason to inspect partner IDs.

Accordingly, “zero EC KV operations” in this plan means **zero operations caused
solely by pull sync**. A request still reads KV when auction EIDs, EID-cookie
persistence, withdrawal, generation, orphan detection, or another identity
lifecycle consumer needs the row. In particular, a zero-partner registry does
not by itself disable PR #885's navigation preload: doing so would indefinitely
suppress orphan detection on sites without pull partners. The no-partner fast
path instead eliminates the entire post-send pull-sync graph/operation path.

## Clarified Contract

- The first no-partner decision occurs before the post-send graph is
  constructed or opened.
- The completeness marker is a signed, host-only, `HttpOnly`, `Secure`,
  `SameSite=Lax` cookie with a maximum one-hour lifetime.
- The marker is bound to the active EC ID, its expiration, and a deterministic
  fingerprint of the sorted canonical pull-enabled partner source domains.
- The existing `ec.passphrase` supplies keying material through explicit
  marker-specific key separation; no new secret or configuration field is
  introduced.
- Missing, malformed, expired, overlong, wrongly bound, incorrectly signed, or
  partner-set-mismatched markers fall back safely to normal KV behavior.
- A valid marker suppresses snapshot preload only when no other current-request
  consumer needs the identity row.
- A row deleted after marker issuance can remain undetected until the marker
  expires. This bounded delay is accepted; once a `Missing` snapshot is
  actually observed, the marker cannot suppress PR #885's recovery flow.
- An authoritative partial, missing, or tombstoned snapshot disproves the
  marker and clears it.
- Explicit consent withdrawal clears both `ts-ec` and the completeness marker,
  regardless of KV success.
- A post-send pull result cannot mutate the already-delivered response. If pull
  sync fills the final missing UID, a later request must verify completeness
  pre-send before issuing the marker.

## Non-Goals

- Do not introduce another request-scoped EC cache or duplicate `EcKvSnapshot`.
- Do not promise zero request-wide KV reads when another EC consumer needs data.
- Do not store partner UIDs, consent data, or the full EC ID in the marker.
- Do not refresh existing partner UIDs or revive the legacy per-partner sync TTL.
- Do not change batch-sync behavior, withdrawal tombstone semantics, auction
  consent gates, or partner rate limits.
- Do not move partner HTTP dispatch before response delivery.
- Do not add an operator-facing marker setting or signing secret.
- Do not refactor all Fastly partner-registry construction as part of this fix.

## Proposed Design

### 1. Versioned signed marker

Add `crates/trusted-server-core/src/ec/pull_sync_marker.rs` and define the cookie
name in `crates/trusted-server-core/src/constants.rs`, using the private name
`ts-ec-pull-complete`.

Use a cookie-safe versioned wire form:

```text
v1.<expires-unix>.<partner-set-sha256-hex>.<hmac-sha256-hex>
```

The protocol will:

1. Take only pull-enabled partners from [`PartnerRegistry`].
2. Sort their normalized `source_domain` values lexicographically.
3. Hash an unambiguous, versioned encoding of that list with SHA-256.
4. Derive a marker-only HMAC subkey from `ec.passphrase` with a fixed domain
   label such as `trusted-server/ec-pull-complete/key/v1`.
5. Authenticate the protocol version, active EC ID, expiration, and partner-set
   fingerprint with HMAC-SHA256.
6. Verify the tag through the HMAC API's constant-time verification method.
7. Accept only `now < expires <= now + 3600`; malformed numbers, unexpected
   segment counts, invalid digest lengths, and invalid hex fail closed.

The live cookie is host-only and uses:

```text
Path=/; Secure; SameSite=Lax; Max-Age=3600; HttpOnly
```

It deliberately omits `Domain`. Expiration uses the same attributes with an
empty value and `Max-Age=0`. EC passphrase rotation therefore invalidates every
outstanding marker and safely falls back to KV.

The marker module also owns the authoritative completeness predicate: a live
snapshot is complete only when its `ids` map contains every non-empty,
pull-enabled source domain in the current registry. An empty pull-partner set
never produces a marker.

### 2. Carry marker state without carrying KV state twice

Extend `EcContext` with a small non-KV marker state. The state should distinguish
at least:

- no marker on the request;
- a present marker not yet validated;
- invalid/stale marker;
- valid marker, including its bounded expiration.

`parse_ec_from_request` captures the raw marker from the already-parsed
`CookieJar`. The raw value must not appear in logs or an unrestricted `Debug`
representation.

Validate before a publisher snapshot decision, once the active EC ID and
partner registry are available. Finalization performs the same validation if a
route did not pass through publisher handling. If no validated registry is
available, the conservative result is invalid/fallback rather than skip.

The state travels naturally with the existing `EcContext` through
`EcRequestState` and `EcFinalizeState`; it is not a second identity snapshot.
Generating or replacing the active EC ID invalidates marker state bound to the
old ID.

### 3. Gate publisher snapshot preload by actual consumers

Replace the four-boolean `should_preload_ec_snapshot` decision with a named input
or requirement structure that remains readable as the conditions grow.

The existing prerequisites remain:

- document navigation;
- `GET` request;
- consent-allowed active EC ID;
- available EC graph.

Without a valid marker, retain PR #885's normal preload, including its bounded
orphan-detection responsibility. With a valid marker, skip only when no
current-request operation needs the row itself. The marker's signed recent
existence proof is what permits orphan detection to be deferred for at most one
hour; the absence of pull partners alone is not such proof. Force the lookup
when any of these apply:

- a server-side auction will run with a registry that can resolve stored EIDs;
- `ts-eids` or `sharedId` is present and may require persistence;
- an existing authoritative snapshot already requires mutation or recovery;
- a privacy path requires authoritative withdrawal state;
- EC generation/replacement requires persisted-state confirmation.

For a plain returning-user navigation with a valid marker and no such consumer,
treat the marker as recent proof that the row existed and was complete. Leave
the snapshot `NotRead` and defer any newly orphaned-row discovery for at most one
hour.

Whenever a lookup remains necessary, preserve PR #885's ordering exactly:

```text
concurrent client: origin start -> KV snapshot -> auction dispatch -> origin wait
eager client:      KV snapshot -> auction dispatch -> origin execution
```

### 4. Reconcile marker state during pre-send finalization

After finalization finishes any EID ingestion, generation, or recovery mutation,
reconcile the marker against the resulting snapshot and current registry:

| Snapshot / marker state                  | Response action                                               |
| ---------------------------------------- | ------------------------------------------------------------- |
| Live and complete; marker absent/invalid | Set a fresh one-hour marker                                   |
| Live and complete; marker already valid  | Keep it without unnecessary refresh                           |
| Live but partial                         | Expire any present marker                                     |
| Missing or tombstoned                    | Expire any present marker                                     |
| Failed                                   | Do not create a marker; do not turn failure into completeness |
| `NotRead` with a valid marker            | Preserve it until its fixed expiration                        |
| `NotRead` without a valid marker         | Do nothing                                                    |
| No pull-enabled partners                 | Never issue; remove a stale marker if present                 |

After setting or clearing a marker, update the in-request status so post-send
logic sees the same decision. Refreshing is allowed only after a current
snapshot again proves completeness; a skipped request must not slide the marker
expiration indefinitely.

On every explicit withdrawal, append the host-only marker-expiration cookie
before KV work, even when no usable `ts-ec` cookie is present. Keep EC-cookie
expiration and tombstone creation under their existing cookie/valid-ID gates.
KV failure remains best-effort and cannot prevent either applicable browser
cookie deletion. Do not change PR #885's existing-key-only conditional tombstone
operation.

### 5. Plan pull sync before constructing the post-send graph

Change the pull-sync preparation boundary so core receives the finalized
`EcContext` and `PartnerRegistry` before Fastly calls `require_identity_graph`.
The preparation step returns `None` when:

- consent or active-EC validation fails;
- no pull-enabled partner exists;
- the finalized request retains a valid completeness marker and no
  authoritative partial snapshot supersedes it;
- the snapshot is unread, failed, missing, tombstoned, or already complete.

Only a present, live, partial snapshot produces `PullSyncContext`. Fastly then
constructs the graph and rate limiter and calls `dispatch_pull_sync`.

Keep a defensive no-partner check at the start of `dispatch_pull_sync`. Use the
registry's deterministic pull-partner ordering before applying the existing
hourly rotation. Do not alter URL validation, token handling, response limits,
HTTP concurrency, missing-UID filtering, request-wide bulk CAS, tombstone
rejection, or best-effort logging.

## File Map

### New

- `crates/trusted-server-core/src/ec/pull_sync_marker.rs`
  - Marker protocol, key separation, fingerprinting, cookie formatting,
    validation, completeness predicate, and focused unit tests.

### Modify

- `crates/trusted-server-core/src/constants.rs`
  - Add the private completeness-cookie name.
- `crates/trusted-server-core/src/ec/mod.rs`
  - Register the marker module; parse and carry marker state in `EcContext`;
    invalidate it when the active EC changes.
- `crates/trusted-server-core/src/ec/registry.rs`
  - Add deterministic pull-enabled partner/source-domain helpers.
- `crates/trusted-server-core/src/publisher.rs`
  - Validate the marker before preload and retain reads required by auction,
    EID ingestion, recovery, or privacy behavior.
- `crates/trusted-server-core/src/ec/finalize.rs`
  - Reconcile marker issuance/expiration and clear it on withdrawal.
- `crates/trusted-server-core/src/ec/pull_sync.rs`
  - Plan no-op/partial pull work from marker plus finalized snapshot before any
    graph requirement.
- `crates/trusted-server-adapter-fastly/src/main.rs`
  - Move `require_identity_graph` after pull-sync preparation succeeds.
- `docs/guide/edge-cookies.md`
  - Document the internal cookie, bounded stale window, invalidation, and
    limits of the optimization.

No dependency or configuration-file changes are expected: `hmac`, `sha2`, and
`hex` are already direct core dependencies.

## Implementation Tasks

### Task 1 — Marker protocol and registry ordering

**Files:** `constants.rs`, `registry.rs`, new `pull_sync_marker.rs`

- [ ] Add failing tests for deterministic partner fingerprints across config
      order and for changes caused by enabling, disabling, adding, or removing
      a pull partner.
- [ ] Add failing marker tests for valid round-trip, malformed payload, invalid
      hex/length, tampering, wrong EC ID, expiration, expiration beyond one
      hour, passphrase mismatch, and partner-set mismatch.
- [ ] Add failing cookie tests for the exact live/expired security attributes
      and absence of `Domain`.
- [ ] Implement the smallest versioned protocol and deterministic registry
      helpers needed to make those tests pass.
- [ ] Run `cargo test-fastly pull_sync_marker` and registry tests.

### Task 2 — Request marker state

**Files:** `ec/mod.rs`, `pull_sync_marker.rs`

- [ ] Add failing tests for absent, unvalidated, invalid, and valid state.
- [ ] Prove active-EC replacement cannot retain a marker validated for the old
      EC ID.
- [ ] Parse the marker through the existing cookie jar and add crate-private
      validation/status accessors.
- [ ] Ensure marker values and full EC IDs never enter logs.
- [ ] Run focused EC context and marker tests.

### Task 3 — Preload decision

**File:** `publisher.rs`

- [ ] Extend the existing `OrderRecordingKv` tests to prove a valid marker plus
      no auction/EID consumer performs zero lookups.
- [ ] Prove absent, malformed, expired, wrong-EC, and stale-partner-set markers
      retain the normal lookup.
- [ ] Prove a valid marker does not suppress auction EID lookup or EID-cookie
      persistence.
- [ ] Prove marker expiration restores normal missing-row detection and orphan
      recovery.
- [ ] Refactor the preload decision and implement validation before
      `load_snapshot`.
- [ ] Re-run concurrent/eager ordering and origin-error tests unchanged.

### Task 4 — Finalize marker lifecycle

**Files:** `finalize.rs`, `pull_sync_marker.rs`

- [ ] Add failing tests for complete, partial, missing, tombstoned, failed, and
      unread snapshots.
- [ ] Prove a pre-send complete snapshot sets the marker, while post-send
      completion cannot modify the current response.
- [ ] Prove an authoritative partial snapshot clears stale valid state so its
      missing partners remain eligible after send.
- [ ] Prove withdrawal expires both cookies even when KV fails, expires a
      marker even when no usable `ts-ec` cookie is present, and still handles
      differing active/cookie EC IDs under PR #885's rules.
- [ ] Implement one centralized marker reconciliation step at every relevant
      finalize exit.
- [ ] Run focused finalize and withdrawal tests.

### Task 5 — Pre-graph pull planning

**Files:** `pull_sync.rs`, Fastly `main.rs`

- [ ] Add tests showing no partners, a retained valid marker, a complete
      snapshot, a tombstone, and failed/missing/unread snapshots produce no
      dispatch context.
- [ ] Add a minimal test seam around the Fastly post-send graph factory and
      prove it is not invoked for no-partner, valid-marker, complete-snapshot,
      or otherwise no-dispatch preparation outcomes.
- [ ] Separately use a counting/failing core KV collaborator to prove those
      paths perform no EC KV operation if dispatch is called defensively.
- [ ] Preserve a partial snapshot path that dispatches only missing eligible
      partners and persists returned UIDs through PR #885's request-wide CAS.
- [ ] Move Fastly graph construction after successful preparation.
- [ ] Re-run existing multi-batch, response-validation, rate-limit, and CAS
      conflict tests.

### Task 6 — Documentation and regression verification

**File:** `docs/guide/edge-cookies.md`

- [ ] Document that `ts-ec-pull-complete` is signed, contains no UID, is
      host-only, expires after one hour, and is deleted on withdrawal.
- [ ] Explain partner-set fingerprint invalidation and the bounded orphan
      detection delay.
- [ ] State explicitly that auctions, EID ingestion, recovery, and withdrawal
      may still require KV.
- [ ] Explain why sync completed after send can be marked only on a later
      request.
- [ ] Run docs formatting and the full verification contract below.

## Acceptance Test Matrix

| Issue acceptance criterion                                      | Planned evidence                                                                                               |
| --------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| No pull-enabled partners causes zero pull-sync EC KV operations | Core no-partner preparation/counting-KV tests plus a Fastly graph-factory seam proving construction is skipped |
| Complete current marker skips initial pull-only read            | Publisher counting test with valid marker and no other row consumer                                            |
| Missing/expired/invalid state falls back                        | Marker unit tests plus publisher lookup-count tests                                                            |
| Partner changes cannot permanently preserve old completeness    | Deterministic fingerprint mismatch tests plus one-hour maximum validation                                      |
| Partial users dispatch only eligible partners                   | Existing and expanded partial-snapshot pull tests                                                              |
| Returned UIDs retain CAS protection                             | Existing PR #885 bulk snapshot/CAS tests remain green                                                          |
| KV failure stays best-effort                                    | Publisher/pull/finalize failure tests; no marker is minted from failed state                                   |
| Withdrawal clears browser state                                 | Finalize test asserting both cookie-expiration headers despite KV failure                                      |

## Verification Contract

Run focused tests during implementation:

```bash
cargo test-fastly pull_sync_marker
cargo test-fastly ec_snapshot_preload
cargo test-fastly pull_sync
cargo test-fastly withdrawal
```

Before committing and opening the PR, run the repository gates:

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

Record every command and result in the PR test plan. A failed environment-only
check must be reported accurately rather than marked complete.

## Definition of Done

- No-partner and recently complete pull-only paths do not construct the
  post-send graph or perform EC KV operations.
- Marker validation precedes any snapshot read it is allowed to suppress.
- Other identity consumers still obtain the row when required.
- Invalid or stale markers fail safely; configuration changes and the one-hour
  maximum prevent permanent stale completeness.
- Withdrawal deletes the marker independently of KV success.
- Incomplete users preserve current partner eligibility, HTTP, rate-limit, and
  CAS behavior.
- No second request-scoped EC cache, new secret, new setting, or batch-sync
  behavior is introduced.
- Documentation and all applicable CI gates pass.

## Risks and Mitigations

- **Dependency on PR #885:** Reconcile any changes to its snapshot or preload
  contract before implementation continues; do not recreate superseded APIs.
- **Bounded orphan-detection delay:** A row removed after marker issuance may be
  discovered up to one hour later. The signed expiration is enforced server-side
  and cannot slide on a skipped request.
- **Marker cannot help every request:** Auction EIDs and request EID ingestion
  still require actual row contents. Tests and docs must avoid broader claims.
- **Cookie churn and cache privacy:** Set the marker only when absent/invalid or
  newly proven; existing cache-privacy middleware already downgrades responses
  carrying `Set-Cookie`.
- **Host-only scope:** Different serving hostnames establish independent
  markers. This is conservative and avoids widening cookie reach.
- **Partner configuration changes:** Source-set changes invalidate immediately;
  other same-source configuration changes remain bounded by the one-hour
  expiration and do not alter fill-missing completeness semantics.
- **Key rotation:** Passphrase rotation intentionally invalidates all markers and
  falls back to KV.
- **Post-send limitation:** The last successful partner response cannot mark the
  response already sent, so one later verification request is expected.
