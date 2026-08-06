# Design Spec: Provider and Permission Model — Migration and Rollout

**Status:** Draft
**Author:** Engineering
**Issue references:** #777–#781 (epic)
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-permission-model-design.md`
**Last updated:** 2026-07-31

> **Context.** The provider/permission epic is a breaking change to a live
> identity system. PR #838's review showed that the riskiest part of such a
> change is not the new code but the transition: silent misconfiguration
> modes, undeclared behavior changes discovered by deleted tests, and no
> written statement of which pre-change behaviors were guaranteed to
> survive. This spec is that statement. Any implementation PR in the epic
> must reconcile its diff against §2's matrix and list every deliberate
> divergence in its description.

---

## 1. Scope

Covers the transition of existing deployments from the hard-wired EC /
device / geo behavior to the provider architecture and permission model.
Applies to every implementation PR in the epic, and to the operator-facing
migration guide that ships with the last of them.

## 2. Behavior-preservation matrix

For each decision the system makes today, the target behavior after the epic,
and whether that is a preservation or a declared change. **Silent changes are
defects.** PR #838 changed six of these without declaring any; each was
discoverable only because a deleted test had pinned the old behavior.

| #   | Decision (today)                                                                                                                                                                    | After epic                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Status                                                                                                                                                                      |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | EU/GDPR request, no TCF consent → no EC                                                                                                                                             | Same (opt-in baseline)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Preserved                                                                                                                                                                   |
| 2   | US-state request, GPC/GPP/USP sale opt-out → no EC, existing EC expired + tombstoned, even when a consenting TCF string is present                                                  | Opt-out denies P4 globally but does not delete P1 or the first-party identity. A separately valid P1 grant may retain/mint an identity that cannot enter partner egress; auction dispatch may continue only through permission §7's positive `ContextualAuctionView`                                                                                                                                                                                                                                                                                                                         | **Changed deliberately:** sale/sharing opt-out is separated from deletion/storage withdrawal                                                                                |
| 3   | US-state request, no signals at all → no EC (fail-closed)                                                                                                                           | Preserved by the recipe: the US rule is `requires_signal`, and the extended grant-signal class (permission spec §4) is what makes that possible — `granted` would allow no-signal traffic; a TCF-only grant class could not grant from GPP/USP values                                                                                                                                                                                                                                                                                                                                        | Preserved under the recipe                                                                                                                                                  |
| 3a  | US-state request, explicit GPP/USP not-opted-out value (including today's N/A-as-allow behavior) → EC allowed                                                                       | Explicit applicable not-opted-out may grant P4 only; it does not grant P1. N/A, absent, reserved, and unknown values grant nothing. P1 requires its own accepted evidence or baseline                                                                                                                                                                                                                                                                                                                                                                                                        | **Changed deliberately:** N/A is not affirmative permission and sale state is not storage authority                                                                         |
| 3b  | US-state request, TCF record present and refusing Purpose 1 (no US opt-out signal) → no EC                                                                                          | Same: refusal beats coexisting non-TCF grant signals (permission spec §4, precedence 3–4)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Preserved                                                                                                                                                                   |
| 3c  | Consent-record conflict modes (restrictive/permissive/newest), expiry, KV fallback, proxy mode                                                                                      | Each row of the normalization matrix (permission spec §4.4) is individually marked preserved or changed there; changed rows: malformed-present now blocks acquisition                                                                                                                                                                                                                                                                                                                                                                                                                        | Per §4.4 matrix                                                                                                                                                             |
| 3d  | Valid + expired consent records: conflict resolution runs first and can select the expired record                                                                                   | Expired sources drop **before** conflict resolution (permission spec §4.4 pipeline)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | **Changed (declared)**                                                                                                                                                      |
| 3e  | Only the GPP sale field (and USP) is consulted; `SharingOptOut` / `TargetedAdvertisingOptOut` are ignored                                                                           | Sale, sharing, and targeted-advertising opt-outs deny P4; their explicit applicable not-opted-out values may grant P4 under a US opt-out regime. None affects P1 or destroys identity                                                                                                                                                                                                                                                                                                                                                                                                        | **New enforcement, declared**                                                                                                                                               |
| 3f  | Non-privacy-state US traffic (e.g. Wyoming) is non-regulated → EC allowed                                                                                                           | Country-level `US` is the protective `us-opt-out` floor; region-specific rules may be stricter. Regionless traffic never degrades to non-regulated                                                                                                                                                                                                                                                                                                                                                                                                                                           | **Changed deliberately** to make country-only geo safe                                                                                                                      |
| 3g  | Graph rows persist JA4 class, H2 fingerprint hash, and buyer-facing quality metadata                                                                                                | Discontinued for new rows; v1 values retained read-only, never egressed, dropped at rewrite (providers spec §6.3)                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | **Changed (declared)** — more protective                                                                                                                                    |
| 4   | UK request, no TCF record → no EC                                                                                                                                                   | Same, unless the policy deliberately adopts a `granted` storage baseline for GB, with citation and sign-off                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Declared change (if made)                                                                                                                                                   |
| 5   | No country resolvable (geo provider failure) → no EC (fail-closed)                                                                                                                  | Protective failure profile: both permissions require signal and dispatch uses GDPR-class handling; `default_country` is reserved for acknowledged static-jurisdiction mode                                                                                                                                                                                                                                                                                                                                                                                                                   | **Changed deliberately:** deny-all becomes signal-required/GDPR-class; absent or invalid grants remain denied, while valid accepted grants are newly possible (sign-off 18) |
| 6   | Non-regulated country, TCF record refusing Purpose 1 → EC still created, existing identity never tombstoned                                                                         | Refusal now blocks _new_ grants everywhere (permission spec §4, precedence 3) — a declared, more-protective change. Existing identity is still **never tombstoned** where the baseline is `granted` (permission spec §4.2)                                                                                                                                                                                                                                                                                                                                                                   | Split: creation is a declared change; no-tombstone is preserved                                                                                                             |
| 7   | Country resolved but not in any regulation list ("non-regulated") → EC created, EIDs pass through                                                                                   | Governed by the policy's `rules.default` entry (permission spec §5.4). The §5 recipe sets it to a `granted` baseline to preserve today's behavior; the protective example policy instead requires a signal worldwide — a declared operator choice between the two                                                                                                                                                                                                                                                                                                                            | Preserved under the recipe; declared change under the protective default                                                                                                    |
| 8   | Opt-out signal (GPC/GPP/USP) **outside** US states → ignored today                                                                                                                  | Its mapped use restriction is honored globally; sale/sharing/GPC deny P4 but do not tombstone P1 identity                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Declared global privacy hardening without irreversible overreach                                                                                                            |
| 9   | Fastly bot gate requires JA4 + platform class before KV-backed EC writes                                                                                                            | Host fingerprinting is deferred and `[device] provider = "fastly"` startup-fails; builtin UA-only classification is the only shipped behavior                                                                                                                                                                                                                                                                                                                                                                                                                                                | Declared loss of the stronger gate pending a separate security/fingerprinting design                                                                                        |
| 10  | Fastly always resolves geo per request                                                                                                                                              | Only with `[geo] provider = "platform"`. The neutral geo default flips **only** in the permission model PR, together with the §5.3 guard — never in an intermediate step where absent geo would fail closed and zero EC issuance (providers spec §11)                                                                                                                                                                                                                                                                                                                                        | Declared change, sequenced, with a documented restore step (§5)                                                                                                             |
| 11a | Raw EC egress on paths gated by the jurisdiction gate today (OpenRTB `user.id`, derived request IDs, page bids, EIDs, identify, pull sync — pull checks the live `EcContext` today) | Gated by the egress inventory (permission spec §7): bidstream and partner egress require both purposes, revocation exempt — at least as strict as today for every path                                                                                                                                                                                                                                                                                                                                                                                                                       | Preserved (strengthened); **must not regress** — PR #838 gated only EIDs and left `user.id` reachable                                                                       |
| 11b | Proxy / click / Testlight forwarding extract the raw EC cookie/header **without** today's jurisdiction gate                                                                         | Gated by the egress inventory (both purposes)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | **New privacy hardening, declared change** — not preservation                                                                                                               |
| 11c | Batch sync today only authenticates the S2S caller and checks live/tombstoned row state — it is **not** jurisdiction-gated                                                          | Gated by stored-provenance recompute (permission spec §7); legacy rows fail closed until backfilled                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | **New privacy hardening, declared change** — not preservation                                                                                                               |
| 12  | EC generation succeeds without a configured identity-graph store                                                                                                                    | A minting provider requires an openable graph store at startup (providers spec §5, §6); pre-N+1 readiness step provisions it                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | **Breaking, declared** — graphless deployments must provision storage before upgrading                                                                                      |
| 13  | Cookies minted by graphless deployments have no graph row                                                                                                                           | Rowless proof via strong-class records under the graphless-migration flag (N+2 convergence + attested stub-backfill first — providers spec §5); verified cookies expire and re-mint without continuity; **rowless destructive withdrawal writes into the capped per-prefix `w` record, then expires** (exact-cookie family records are superseded); non-destructive signals are request-local for the old rowless identity, while any newly minted row-backed family commits the current suppression before use; unverifiable roaming cookies get disclosed cookie-only expiry (sign-off 29) | **Declared** — pre-existing identities restart rather than carry over                                                                                                       |

Rows 3, 4, and 7 are policy decisions, not code decisions: they belong in
the `[permissions]` policy review, made explicitly by maintainers — not
implied by an implementation.

## 3. Identity stability guarantee

Today's EC identifier is `{64-hex}.{6-char}` where the 64-hex part is
deterministic — `HMAC-SHA256(passphrase, normalized_ip)` — and the 6-char
suffix is **random per mint** (an existing test asserts two mints differ).
Full identifiers are therefore not reproducible by design, and no test may
pretend otherwise. What stability means, precisely, for a deployment that
selects `provider = "hmac"` and carries its passphrase over verbatim:

- **The deterministic prefix is bit-identical.** Pinned known-answer
  vectors: fixed passphrase + IP → exact expected 64-hex prefix, committed
  so any divergence fails CI rather than rotating the production identity
  base.
- **Existing cookies stay parseable.** Fixture `ts-ec` values minted by
  the pre-epic code pass the provider's `parse`, and their graph rows
  (keyed by the identifier verbatim) remain reachable — no row is orphaned.
- **The hash prefix keeps its semantics.** `ec_hash` remains the 64-hex
  prefix, preserving both its stability and its deliberate collision across
  identifiers minted from the same IP — the property IP-cluster trust
  counting depends on (providers spec §3).
- **Cookie name, attributes, and max-age are unchanged** (the domain
  remains config-derived, as today).

## 4. Configuration migration

Old shape:

```toml
[ec]
passphrase = "example-passphrase"
```

New shape:

```toml
[ec]
provider = "hmac"

[ec.providers.hmac]
passphrase = "example-passphrase"
```

Requirements:

1. **The transition has a dual-read release; loud rejection comes one
   release later.** Today's binary _requires_ `[ec] passphrase` and — via
   `deny_unknown_fields` — _rejects_ `[ec] provider` and
   `[ec.providers.*]`; a binary that rejects the old shape outright would
   mean **no config both binaries accept**, and a config-store fleet
   cannot flip config and binaries atomically. Sequence:
   - **Release N+1 (dual-read):** accepts the old shape (mapping
     `[ec] passphrase` to the `hmac` provider internally, logging a
     deprecation warning per startup) _and_ the new shape. Ordering is
     **strictly reader-first, never "either order"**: current binaries
     reject the new shape (and reject the new `[permissions]` / `[device]`
     / `[geo]` additions as unknown fields), so the config may flip only
     after **fleet convergence on N+1 is confirmed** — binaries first,
     convergence gate, then `ts config push`. A config mixing old and new
     fields (`[ec] passphrase` alongside `[ec] provider`) is **rejected**
     by N+1, not reconciled. **N+1 is a full semantic reader and
     fail-restrictive enforcer for every N+2 negative record kind — not a
     field preserver — but it does not originate the new use-suppression
     model.**
     Preserving unknown JSON does not chase aliases, consult family
     revocations, honor suppression records, or fail closed on
     provenance; an N+1 that merely preserved would, after rollback,
     treat revoked identities as live (aliases are reserved-future with
     the rewrite deferral, providers spec §6.1). N+1 must also **write
     family revocation records** — a withdrawal arriving on a
     rolled-back N+1 fleet must still revoke. Existing negative gates are
     **config-shape- and graphless-flag-invariant** (providers spec §5 total
     state table): no config shape, flag state, or v1-semantics row disables
     reading and enforcing an already committed family revocation,
     suppression entry, pending outbox intent, or live `w` record.

     N+1's write boundary is narrower. It may create the minimum s-class stub
     needed to admit an observed untouched v1 row and may write a family
     revocation or its durable outbox intent for an explicit storage
     withdrawal/authenticated deletion that the pre-epic lifecycle already
     recognizes. It **does not create or strengthen a use-suppression entry**
     from GPC, SharingOptOut, SaleOptOut, TargetedAdvertisingOptOut, malformed
     input, or absence. A signal already recognized by the pre-epic gate keeps
     only that request-local behavior; a newly mapped signal is telemetry on
     N+1. None creates durable suppression. Persistence begins only on N+2 after
     active new-shape configuration **and** the fleet-wide `permissions_v2`
     model promotion. N+1 also never commits positive authority and
     never clears N+2 state because it cannot produce the ordered
     `AuthorityRefresh` revision. A suppression created by N+2 therefore stays
     fail-restrictive through binary rollback and clearing waits for
     roll-forward. This asymmetry preserves rollback safety without describing
     a first-observation P4 suppression as "pre-epic behavior."

     **N+1's identity-write behavior is v1, explicitly** — this resolves
     what was an impossible trilemma (write rows without provenance,
     violating active-after-commit; write provenance, violating the
     N+2-only writer boundary; or stop minting, an undeclared outage):
     N+1 **keeps minting v1 rows with today's semantics**. Merely starting an
     N+2 binary does not activate the new contract: both N+1 and N+2 read the
     permission spec §5.5 `model_epoch` from the strong activation register,
     and N+2 must emulate N+1 while it is `pre_epic_v1`. The
     active-after-commit/provenance contract activates only through the
     fleet-wide `permissions_v2` model promotion, not from binary version.
     Likewise the permission model itself:
     **old-shape config on N+1 runs the pre-epic consent gate
     unchanged** — dual-read means dual-behavior — so the compiled
     protective fallback cannot flip behavior mid-convergence before the
     operator pushes the new-shape policy; the new model's **live gating**
     engages only with active new-shape config **and** the promoted
     `permissions_v2` epoch together — before promotion, new-shape config on
     either binary engages validation, telemetry, and the
     batch fail-closed boundary below, never live-request gating. The interim is declared as sign-off item 20 — with one
     boundary that does **not** wait for N+2: once new-shape config is
     active, **context-free partner egress (batch sync) on N+1 fails
     closed for rows without provenance**, exactly as the permission
     spec's legacy rule requires. Otherwise N+1 would mint a P1-only v1
     row under the new model and then release it through today's
     row-state-only batch check — the fail-closed rule cannot activate
     later than the model it protects. Live-request paths keep v1 semantics
     until model promotion.

     The interim is **one matrix, not competing prose** — per release ×
     config shape, each dimension separately (a new-shape P1 denial on
     N+1 has exactly one meaning: telemetry, never gating):

     | Dimension                                                  | N+1, old shape                  | N+1 or N+2, new shape + `pre_epic_v1`                     | N+2, new shape + `permissions_v2` |
     | ---------------------------------------------------------- | ------------------------------- | --------------------------------------------------------- | --------------------------------- |
     | Policy resolution                                          | not parsed                      | parsed, validated, logged — **never gates live requests** | gates                             |
     | Live-request permission gating                             | pre-epic gate                   | pre-epic gate (a new-shape denial is telemetry only)      | new model                         |
     | Existing negative gates (`r`, `s`, `q`, `w`), read/enforce | **active** (rollback invariant) | **active** (rollback invariant)                           | active                            |
     | Explicit withdrawal/deletion family revocation + outbox    | active                          | active                                                    | active                            |
     | Fresh use-suppression creation/strengthening               | **forbidden**                   | **forbidden**                                             | active                            |
     | Identity-row writes                                        | v1 rows                         | v1 rows                                                   | v2 + provenance                   |
     | Positive authority commits / clears                        | forbidden                       | forbidden                                                 | N+2 writer                        |
     | Context-free batch (S2S) egress                            | pre-epic row check              | **fails closed for provenance-less rows**                 | full recompute                    |

     Rollback tests therefore mirror the one contract exactly:
     family-revocation read and explicit-withdrawal write; minimum stub
     creation; existing suppression/outbox read and enforcement; fresh GPC,
     sale, sharing, and targeted-advertising opt-outs asserted to create no
     `s`/`q` record on N+1; **positive-authority commits and every clear
     asserted forbidden**; and v1-minting behavior — all on N+1 against
     N+2-written data. **Before model promotion, rollback is binaries-first too,
     in the other direction** — N+2 → N+1 binaries roll back keeping the new config (N+1 reads it
     fully; reverting config first would hand the old shape to N+2
     binaries that reject it) — **with one structural rule that makes it possible at all**:
     providers are compiled into the composition root — there is no
     dynamic provider ABI — so an N+1 binary can only read what it
     shipped with. Therefore **every provider selectable in release R
     must ship compiled-in (dormant: registered, parseable,
     configurable, not selectable as writer) in R−1**; adopting a
     genuinely new provider gets its own reader-first rollout, exactly
     like the epic itself. After the `permissions_v2` promotion, the active
     minimum binary generation and row schema floor bar N+1 from startup and
     per-request serve admission; rollback below N+2 then requires a separately
     designed forward-compatible recovery release, never an N+1 binary. With
     that rule, **schema rollback and provider rollback are
     distinct sequences**: schema rollback is binaries-first (above);
     **provider rollback is config-first** — a fleet whose config
     _selects_ the new provider as writer cannot roll binaries first,
     because the older binary rejects that active writer even while
     containing its dormant code. The order: switch the current fleet's
     writer back to the older provider (retaining the new one in
     `legacy_providers`, satisfiable because N+1 physically contains the
     code), converge, then roll binaries.
     N+1 additionally **rejects writer selections whose provenance it
     cannot yet encode** — new-writer adoption waits for N+2, so no row
     is minted that N+2 would misclassify. Every new
     config section introduced by the epic follows this same
     compatibility rule, not only `[ec]`.

   - **Release N+2:** rejects `[ec] passphrase` at startup with a message
     naming the new location — not a generic unknown-field error
     (implementation note: producing the actionable message means keeping
     a deprecated `passphrase` field whose presence triggers the custom
     error).

2. **Adapter qualification is a pre-ratification prerequisite, not a
   footnote.** No adapter is presently proven eligible for the complete
   identity protocol — Fastly's global-read/retention cells are
   unverified and its deployment-metadata primitive unwired; every
   other adapter is unavailable or needs a new primitive. Ratifying
   before at least one adapter qualifies risks an epic with no
   selectable identity provider, so Fastly qualification (or an
   explicit decision to proceed without it) gates ratification —
   together with **vendoring the pinned PSL artifact**
   (hook spec §4a.1 now records the upstream commit, but the required
   list bytes and checked hash are not yet present; the vendoring manifest/PR
   must record the upstream commit tree, source blob OID, vendored SHA-256, and
   byte-for-byte verification), **completing the pinned
   GPP corpus and decoder** (permission spec §4.5.1 now records the
   immutable official commit and accepted versions, but the per-section
   conformance corpus and complete decoder are still prerequisites; its
   manifest/PR must likewise record the commit tree and blob OID/SHA-256 for
   each of the four state specifications and `Section Information`), and
   creating the §8 decision records.
3. **Revocation-eligible storage is a per-adapter gate, and ungated
   adapters migrate stateless.** Identity features require the adapter's
   strong-consistency rows in the capability matrix (providers spec §7)
   to be green: today that means Fastly must _verify_ its KV read
   semantics, Cloudflare must wire a Durable-Object-class primitive, and
   Spin must wire storage at all. Until an adapter passes the gate, its
   migration fixture is **explicitly stateless** (`provider = "none"`,
   no `[permissions]`-gated identity features) — calling an HMAC fixture
   "valid" on an adapter that must reject identity features at startup
   would make the required fixtures self-contradictory. Whether ungated
   adapters go stateless or block the release is product sign-off
   item 12.
4. **The graphless migration has an operational runbook, not a
   pointer to the wrong section.** (The providers spec's earlier "§4.2
   readiness step" reference pointed at adapter qualification.) The
   sequence, attested where noted: (a) confirm N+2 fleet convergence
   (deploy records); (b) measure/confirm the backend's listing settle
   window (capability declaration); (c) run the stub-backfill scan to
   **two consecutive zero-discovery passes**, recording watermark and
   pass count; (d) CAS-create the graphless flag with the attestation in
   its value; (e) rowless classification active. **Abort/rollback:** any
   N+1 startup suspends the flag automatically (providers spec §5); a
   failed or interrupted pass restarts from (c) — passes are idempotent;
   re-attestation after any suspension repeats (c)–(d) over the gap
   window. **Clearing:** after the quiet-period criterion (no rowless
   classifications for a full cookie lifetime), the operator CASes the
   flag cleared, permanently ending rowless classification.
   4b. **Graph-store readiness precedes everything.** Today the graph store
   is optional and EC generation succeeds without one; the epic's
   no-active-until-commit invariant (providers spec §5) makes it
   mandatory wherever a minting provider is configured — so a currently
   valid graphless HMAC deployment would **startup-fail on N+1's
   dual-read mapping** without a preparatory step. The migration
   therefore begins with a **pre-N+1 readiness step**: provision and
   verify an openable graph store (and confirm the adapter's capability
   row supports the features in use, providers spec §7) _before_ rolling
   N+1. This is a **declared breaking change** for graphless deployments
   (matrix row 12), not a side effect discovered at boot.
5. **The graph schema change is expand-contract, in lockstep with the
   binary sequence.** New rows carry fields v1 rows never had — provider/
   version, per-permission grant evidence, policy revision, family ID — and two failure modes must be engineered away: a naive
   schema-version bump makes old readers fail closed on new rows, and an
   old worker that reads, modifies, and reserializes a row **silently
   drops** fields it does not model. The sequence shares the config
   release names: **N+1 is the reader/preserver release** — it understands
   the new fields and preserves unknown keys **semantically** through
   read-modify-write (values round-trip; byte-identical JSON is neither
   required nor achievable through a structured serializer — and a
   genuinely pre-N+1 worker cannot preserve at all, which is exactly why
   the floor exists). After authoritative fleet convergence on N+2, the
   controller runs permission §5.5's model prepare/commit: every
   traffic-eligible snapshot member proves N+2 generation and the bound active
   new-shape tuple, then the fleet stops admission and proves no pre-epic
   request remains in flight before one CAS changes `model_epoch`, minimum
   binary generation, and row schema floor together. **The rollback floor is crossed
   by that fleet-wide CAS**, not by the first N+2 process to start. N+2 keeps
   N+1 writer/gating behavior before the CAS; afterward every N+1 request fails
   serve admission and only then may N+2 emit new fields.

   The authoritative floor lives in deployment metadata outside rollbackable
   config and every request reads it through the activation fence. The legacy
   `m00` key is a monotonic startup mirror updated after promotion, never an
   activation source; a lower or unreadable mirror fails startup but a higher
   mirror cannot enable writes without the authoritative active tuple.
   **Mirror completion and repair are an owned runbook step:** after the model
   CAS, the authenticated deployment controller strong-reads active and
   `m00`; a missing or lower mirror is CAS-set to exactly
   `active.row_schema_floor`, equality is an idempotent no-op, and an
   unreadable mirror or failed CAS/read-verification remains closed for retry.
   The controller then strong-reads and verifies exact equality before
   declaring the cutover complete. A crash between model CAS and mirror write
   reruns the same idempotent operation; it never lowers `m00` or changes
   active. A higher mirror is rejected before any write rather than repaired
   by lowering it — startup remains closed while operators investigate the
   register/journal inconsistency.
   Floor-in-rollbackable-config would let "restore the previous config
   version" erase the marker after new-format rows exist — exactly the
   state it guards — and "any new-format row exists" is a fact no
   operator can disprove. Below-floor rollback is
   prohibited from that marker on; a pre-floor binary would silently
   strip the new fields from every row it touches.
   Rows carry the existing `v` schema discriminator; backfill is lazy via
   live requests (the same pass that backfills legacy provenance,
   permission spec §7) — and, critically, **withdrawal never depends on
   backfill**: the family ID for an untouched v1 row is derived
   deterministically (permission spec §4.3), so a first-post-upgrade
   explicit storage-withdrawal request revokes correctly with zero migrated
   state. Once the N+2 writer and new-shape policy are active, a GPC request
   the active N+2 writer instead persists a P4 use suppression without deleting the identity; N+1
   only applies its pre-epic request-local result as specified above.
   Mixed-version tests with stated expected results:
   N+1-reader/old-row → full function; old-reader/new-row → v1 semantics,
   new fields untouched if read-only, preserved semantically if
   read-modify-write on N+1, **test-proven lost on pre-N+1** (documenting
   why the floor is a floor); N+2-reader/N+1-written-row → full function.

6. **Half-migrated fails loud.** A `[ec.providers.hmac]` block with no
   `provider = "hmac"` selector is a startup error (providers spec §6). In
   PR #838 this configuration — the exact state an operator following the
   docs reaches if they miss one line — validated green and silently minted
   zero ECs.
7. **PR #838-era keys fail loud.** `provider = "host-signals"` (shipped by
   PR #838, deliberately not carried into this epic — providers spec §2)
   and `provider = "client-fixed"` are unknown keys and rejected like any
   other, so a config written against the PR #838 example cannot silently
   select a provider that no longer exists.
8. **Provider switches go through legacy readers.** Changing
   `[ec] provider` on a deployment with live identities requires listing
   the outgoing provider in `[ec] legacy_providers` (providers spec §6.1)
   so existing cookies keep resolving and stay withdrawable; the guide
   documents the switch sequence and the retirement/cleanup step that ends
   it.
9. **The example config ships the migrated happy path**, uncommented:
   `provider = "hmac"` with its block and `[geo] provider = "platform"`
   where the adapter qualifies. Host fingerprinting is not a migration
   compatibility option: `[device] provider = "fastly"` is rejected until
   a separate security/fingerprinting design is approved. Static-geo
   examples use `default_country` only together with
   `assume_single_jurisdiction = true`. PR #838's example shipped the passphrase block
   uncommented with the selector commented out — steering operators directly
   into the silent-stateless state.
10. Every misconfiguration in the providers spec §6 table fails at
    **startup**. Request-time failure for a configuration error is a defect.
11. Validation is split into two named layers, because "the same
    validation at push and startup" is not implementable: **structural
    validation** (schema, types, `[permissions]` policy — permission
    spec §3.3) runs at `ts config push` and again at startup;
    **deployment validation** (adapter capabilities, store bindings,
    store openability — a structurally valid selection can still be one
    an adapter must reject) runs at startup, where those facts exist.
    Push may additionally pre-check deployment facts when given a
    **machine-readable adapter capability profile** (the providers §7
    matrix, serialized), but startup remains the authority.

## 5. Minimal-divergence migration recipe (operator-facing)

"Keep exactly today's behavior" is not fully achievable, and the recipe's
name says so. The unavoidable divergences, enumerated (each also a matrix
row): global P4 opt-out honoring (row 8); refusal blocking new grants
everywhere (row 6); newly enforced GPP sharing/targeted fields, where only
an explicit applicable non-opt-out can grant P4 (row 3e); the protective
geo-failure profile (row 5); country-wide protective US handling for
country-only and regionless results (row 3f); malformed-present blocking
acquisition (§4.4); proxy-mode opt-out extraction; and the batch-sync
provenance gate (row 11c). Everything else the recipe preserves.

The migration guide (a new `docs/guide/` page, linked from the release notes)
gives one copy-pasteable recipe per adapter for the minimal-divergence
posture — **branching on capability eligibility**: adapters passing the
revocation-storage gate get the HMAC + graph fixture below; ungated
adapters get the explicitly stateless fixture of §4.2, and no universal
HMAC requirement contradicts that:

The recipe is a **complete, valid TOML fixture per adapter, committed to
the repository** (e.g. `docs/guide/fixtures/migration-preserving-fastly.toml`
and siblings) and included in the guide verbatim — never described as a
textual delta against the example file. Per-adapter because
`[geo] provider = "platform"` varies by host — Cloudflare **does**
support platform geo but resolves **country only, no region** (per the
providers spec adapter matrix), which engages the country-wide protective
US rule rather than degrading to non-regulated; Axum and Spin
have no platform geo and reject the selection (providers spec §6). Each
adapter's fixture carries the selections valid for it, and each is
CI-validated against its adapter. (An earlier draft said "copy the example table,
then set `[permissions.rules] default`" — but the copied table already
declares `[permissions.rules]`, and reopening a TOML table is a parse
error; a prose delta cannot be validated, a committed fixture can.) The
fixture contains, in one document:

- `[ec] provider = "hmac"` with its passphrase block, **and the
  identity-graph store configuration** — selecting a minting provider
  without an openable graph store is a startup error (providers spec §6),
  so a fixture omitting it would not start;
- no host-fingerprinting provider selection; the shipped builtin
  classification is UA-only, and selecting `[device] provider = "fastly"`
  fails startup pending a separate approved design;
- `[geo] provider = "platform"` where supported. A selected provider's
  lookup failure uses the compiled-in protective failure profile and never
  `default_country`; a static deployment instead sets `default_country`
  together with `assume_single_jurisdiction = true` and receives a
  separately validated fixture;
- (Fastly fixture; other adapters substitute their valid selections)
  the full `gdpr-eu` / `gdpr-uk` / `us-opt-out` groups and country rules
  from the example policy (US as `requires_signal` with the grant-signal
  class — §2 rows 3–3b), a country-wide `US = "us-opt-out"` floor, plus
  the `non-regulated` group with `rules.default = "non-regulated"` (row
  7). Operators who prefer the
  protective worldwide default use the example file itself instead.

A partial policy is a trap the first draft of this spec fell into: a
`[permissions]` section containing **only** the permissive
default — with no GDPR/US rules — sends _every_ jurisdiction, France
included, to the permissive fallback, because `default_country` selects a
rule like any other country and finds none. Each committed fixture is
therefore always complete, and CI pins it: **the fixture file itself** is
loaded and run through the complete §4.1/§4.2 decision matrix of the
permission spec, asserting per-jurisdiction outcomes match the pre-epic
gate for every preservation row of §2.

The guide separately documents the neutral configuration and what it does
_not_ do, states explicitly that `default_country` alone does not replace
geo lookup, why the no-geo combination requires the explicit acknowledgment
flag (permission spec §5.3), and that no recipe preserves row 8 of §2 — the
global honoring of opt-out signals is unconditional.

## 6. Rollout sequence and observability

1. Implementation PRs land in the epic's order (providers spec §11:
   providers first with the geo default held at today's behavior, the
   permission model PR flipping it together with its guard); each PR is
   reviewable against §2 in isolation and states which rows it touches.
2. **Adapter qualification is a release gate.** Every adapter that serves any
   request, including identity-free traffic, first qualifies immutable
   publication, config-sequence allocation, the bounded admission-lease
   timebase, shared register/journal clock, store-enforced
   promotion-not-before, fleet readiness/quiescence, and the atomic local
   admission gate. An adapter missing any of those universal activation cells
   cannot serve under this spec; stateless identity does not bypass them.
   Stateful/context-free identity use additionally stays disabled until every
   required providers-spec §7 identity cell is backed by a platform artifact
   and fault/concurrency tests: global strong reads plus CAS, row
   generation-CAS, negative outbox and safety breaker, and retention ceilings.
   An adapter that qualifies universal activation but not those identity-state
   cells takes the declared stateless-identity path. The response hook likewise remains startup-disabled where its §3
   artifact/IR, atomic-commit, Vary-rekey, secret, or header-ceiling cells are
   pending.
3. **Every policy publication uses staged activation.** `ts config push`
   allocates a never-reused `push_sequence`, writes and verifies an immutable
   version-addressed envelope, and CAS-installs the complete blob/data/config/
   policy tuple as candidate. Every authoritative fleet member acknowledges
   that exact tuple and the deployment-qualified
   `serve_admission_lease_bound_ms`. The draining CAS sets a trusted-store
   `promotion_not_before_unix_ms`; an old-generation validation may admit only
   until its hard bound, after which an all-request admission stop and
   authenticated quiescence barrier proves no request or background effect
   remains on the displaced tuple. Time expiry never substitutes for member
   quiescence; only the controller's CAS
   promotion makes the entire settings snapshot active. Instances serve the
   prior active snapshot while readiness is incomplete, stop during the commit
   drain, and reopen only after verifying and leasing new active; staged or stale revisions
   may not perform destructive work. Rollback republishes old content under a new sequence and
   follows the same prepare/commit path. Settings promotion preserves the
   active model fields. The later N+2 writer cutover is a distinct unanimous
   model prepare/commit on the same register: all traffic-eligible members
   prove N+2 readiness against the bound active tuple, wait out the same
   admission-lease bound, drain all pre-epic
   requests and attest quiescence, then one CAS advances model epoch, minimum
   binary generation, and row schema floor together. The controller then runs
   §4 requirement 5's idempotent `m00` raise/read-verify step; cutover
   completion and below-floor startup remain closed until it succeeds.

   **Operational consequence:** every settings promotion — including an
   ordinary configuration-only push — deliberately creates a scheduled
   fleet-wide deployment-unavailable interval while admission is closed,
   displaced-generation work quiesces, the promotion CAS completes, and
   members load the new tuple. This is not a zero-downtime configuration
   protocol. A controller failure can extend the outage until authenticated
   cancellation or successful promotion; scoping the stop to selected settings
   or providing blue/green overlap requires a separate effect-classification
   design.

4. Before/after deploy, operators watch **EC issuance rate** and EID
   attachment rate; the migration guide names these as the canary metrics,
   because the failure mode of a bad migration is a silent drop to zero (or a
   silent grant to everyone), not an error rate. The full metric set, each
   with a stated healthy range: geo lookup-failure/fallback rate (permission
   spec §5.2), raw-egress denials by path, tombstone family retries,
   legacy-reader hit rate, and cluster-fallback engagements. Two of these carry thresholds, not just ranges:
   legacy-reader hits at zero for a **quiet period no shorter than the
   maximum cookie/row lifetime plus rollout skew** is the **only
   retirement-readiness** bar for a
   legacy provider ("trending to ~zero" is not evidence; a yearly visitor
   is not churn). The telemetry set also includes: graph read/commit failures,
   stored-provenance denials, and schema-migration failures. **Each rollout-gate metric ships with a
   threshold, an evaluation window, and a named action** (pause rollout /
   roll back / block retirement) in the migration guide — a metric with a
   "healthy range" but no action is dashboard decoration; the two already
   specified (legacy-reader quiet period) are the pattern the rest follow.
5. Startup logs always print: selected provider per concern, whether geo is
   live, the effective default baseline, and the count of granted-without-
   signal permissions. One line, greppable, stable format.
6. **The batch-sync coverage dip is a gated rollout stage, not a
   notification.** Provenance-coverage thresholds are normative gate
   criteria: the guide defines a target coverage level and evaluation
   window; recovery stalling below threshold for the window triggers the
   **pause action** — investigate backfill (traffic mix, dormant rows),
   never disable the gate; and staging is explicit: provenance
   **writing** begins only when the `permissions_v2` model epoch activates,
   enforcement is already
   in force (there is no fail-open stage), so the only stageable knob is
   partner communication and the cleanup cadence for rows that never
   recover.
   Because legacy rows fail closed for batch updates until backfilled
   (permission spec §7), batch-sync acceptance drops toward zero at
   cutover and recovers along the live-traffic backfill curve. The
   **provenance-coverage metric** (share of active rows carrying
   provenance) is the gate signal; operators notify batch-sync partners
   of the transient rejection rate. There is no fail-open shortcut — the
   alternative (grandfathering pre-epic identities past the permission
   model) is rejected in the permission spec.
7. Rollback is config-only where possible: reverting to the previous
   config version restores the previous behavior on the previous binary.
   The durable artifacts are enumerated — not "one": **family revocation
   records and member tombstones** created by explicit storage withdrawal
   or authenticated deletion (no recovery; that is their purpose), the
   **authoritative `permissions_v2` model epoch/minimum-generation/schema-floor
   tuple** (monotonic by design; no administrative clear) and its `m00`
   compatibility mirror,
   persistent per-family use-opt-out suppressions, and any pending negative
   intent in the durable outbox. A use-opt-out suppression does not expire
   merely because a consent TTL elapses; only strictly newer explicit
   authorization for that same use, or identity deletion, clears it. An
   outbox entry remains until the target negative write is confirmed, and
   the global identity safety breaker remains closed until the outbox is healthy
   and drained. The irreversibility of identity revocation is why the
   destructive triggers (permission spec §4.2) are exhaustive and why
   partial withdrawal failure has a family-record-first, cleanup-retries
   contract (permission spec §4.3). Two operational procedures are documented in the
   guide, not automated: cleanup of identities minted before a policy
   tightening (permission spec §4.2 trigger 3), and retirement of a legacy
   reader after a provider switch (providers spec §6.1), which is the
   deliberate end of the identities only that reader can resolve.

## 7. Documentation deliverables

- Migration guide page (§5), linked from `CHANGELOG.md` and the release
  notes.
- `configuration.md` documents **every** valid `provider` value for all
  three concerns, the full `[permissions]` schema, and environment-variable
  overrides only if they actually work in production builds (in PR #838 the
  documented `TRUSTED_SERVER__EC__PROVIDER` override existed only under
  `#[cfg(test)]`).
- The permission model page states the §4 precedence rules of the permission
  spec verbatim — operator docs and normative spec must not diverge on
  precedence, and prose like "signals are mapped as a grant or a revoke"
  without stating which wins is insufficient.

## 8. Product decisions requiring explicit sign-off

These are product decisions this spec set needs that #838 had not already
made (or made differently). The table records the recommended resolution
approved for this spec revision, not a final product decision.
**Implementation is blocked while any row is `open`**; each row is a
**decision, not an assignment**: _who_ decided is captured inside the record
itself (`docs/superpowers/specs/decisions/NN-title.md` — the decision,
the deciders, the date). The Decision-record column holds the link (`—`
while open); an unratified row reverts to open, not to silently
implemented.

| #   | Recommended resolution                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | Where                                       | Decision record | Status |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- | --------------- | ------ |
| 1   | Honor mapped use opt-outs globally. Destructive identity effects are limited to explicit storage withdrawal, authenticated deletion, or a qualifying live TCF Purpose 1 refusal.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | permission §4, §4.2                         | —               | open   |
| 2   | GPP/USP sale opt-outs suppress P4 only; they neither revoke P1 nor delete the identity.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | permission §4.5                             | —               | open   |
| 3   | Sharing/targeted-advertising opt-outs suppress P4; an explicit applicable not-opted-out value may grant P4. Neither affects P1.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | permission §4.5                             | —               | open   |
| 4   | US auction dispatch may continue while P4 is unset only through permission §7's positive `ContextualAuctionView` and the sole normative inline manifest in permission §7.1. Unknown, unlisted, ill-typed, or untraceable leaves cause no dispatch; there is no client IP/UA, precise geo, page/referrer URL, user identifiers/data/segments, arbitrary extensions, or client forwarding headers. A separately authorized P1 identity may remain stored but cannot enter auction or partner egress. A destination that cannot consume the exact projection receives no request.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | permission §7.1                             | —               | open   |
| 5   | Country-only and regionless US traffic use a protective country-wide `us-opt-out` floor; state rules may be stricter.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | permission §3.4                             | —               | open   |
| 6   | Raw regulatory strings reach only the positively registered OpenRTB field that requires each source; all other destinations default deny. Identity rows retain normalized provenance/digests, not raw consent snapshots.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | permission §7; providers §6.3               | —               | open   |
| 7   | Reject legacy batch-sync traffic until live-browser provenance backfill makes the row re-evaluable.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | rollout §6 item 6; permission §7            | —               | open   |
| 8   | Gate proxy, click, and Testlight identity forwarding on P1 ∧ P4.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | §2 row 11b                                  | —               | open   |
| 9   | Defer integration-owned cookie operations from the v1 response hook; require a complete read/use/withdraw lifecycle before admission.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | hook §3                                     | —               | open   |
| 10  | Do not create a blanket session-cookie exemption; every cookie must be covered by an approved permission or narrowly defined security-use authority.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | hook §3                                     | —               | open   |
| 11  | Require a durable per-family negative-intent outbox in a failure domain independent of its strong target and checked freshly by every identity consumer. If neither target nor outbox can commit, close a globally visible breaker over all positive identity operations until audited recovery; adapters must prove the §4.3 failure contract rather than merely expose three CAS keys. This rejects the prior alternative of accepting an indefinite S2S/use residual when a negative target write fails and the visitor never returns.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | permission §4.3                             | —               | open   |
| 12  | Adapters that cannot meet the revocation-storage contract migrate stateless rather than weakening the contract.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | rollout §6 item 2; recipe §5                | —               | open   |
| 13  | Keep batch sync fail-closed at cutover; stage partner communication and cleanup using explicit coverage thresholds, windows, and pause actions.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | rollout §6 item 6                           | —               | open   |
| 14  | Policy tightening does not reinterpret historical refusal as a destructive event; destructive withdrawal requires fresh, live qualifying evidence.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | permission §4.2 trigger 2                   | —               | open   |
| 15  | Descope the client cycle and `rewrite_legacy`; ship the v1 integration hook as headers-only.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | client spec status; providers §6.1; hook §3 | —               | open   |
| 16  | Persist use-opt-out suppression until ordered explicit authorization for that use or identity deletion. TCF `LastUpdated` or an authenticated monotonic authorization revision proves order; bare timestamp-less GPP/USP does not. A currently presented identical timestamp-less opt-out starts a new restrictive episode after a clear without refreshing its original age. TTL and saturation never shorten it. This rejects the prior TTL-sticky alternative under which suppression became inert at consent-TTL expiry, as well as administrative clear without newer authorization and saturation-based shortening.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | permission §4.3                             | —               | open   |
| 17  | N/A, absent, reserved, unknown, and unsupported values never grant processing.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | permission §4.5                             | —               | open   |
| 18  | A selected geo provider's lookup failure uses the compiled-in protective profile; `default_country` is only for acknowledged static-jurisdiction mode.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | permission §5.2                             | —               | open   |
| 19  | Use immutable version-addressed whole-config publication plus prepare/commit activation of the complete blob/data/config/policy tuple with authenticated authoritative fleet membership/readiness, a deployment-qualified bounded admission lease, a shared authenticated register/journal time domain, store-enforced promotion-not-before, an all-request quiescence barrier, and a time-retained immutable activation journal. Use a second unanimous, lease-drained and quiescent model transition on the same register to advance model epoch, minimum binary generation, and row schema floor atomically. Every ordinary settings promotion intentionally causes a scheduled fleet-wide deployment-unavailable interval; this is not a zero-downtime protocol, and controller failure may extend the outage until authenticated cancellation or promotion. A mutable “latest” blob never activates settings or the writer; membership changes restage the candidate; no request admitted under a displaced logical `activation_generation` remains able to produce effects after either promotion. The activation fence is universal, including stateless-identity and identity-free traffic; an adapter that cannot qualify it cannot serve under this spec. The lease amortizes only whole-settings admission: positive authority, revocation, outbox, `w`, and breaker decisions retain fresh strong reads. | permission §5.5; CLI §5                     | —               | open   |
| 20  | N+1 keeps v1 minting and pre-epic live gating. It reads/enforces N+2 negative state for rollback safety and can persist an explicit pre-epic withdrawal/deletion, but it does not originate durable P4 use suppression. New-shape settings alone do not activate the new writer/model: N+2 emulates N+1 until the fleet-wide `permissions_v2` model CAS, after which the register's minimum binary generation bars N+1 from serving. The N+1 batch boundary already fails closed as soon as new-shape settings are active.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | migration §4.4                              | —               | open   |
| 21  | Expire and re-mint rowless legacy cookies without continuity; a prefix match cannot authenticate the cookie suffix. Non-destructive signals are request-local and create no negative record for the old rowless identity; if ordinary P1-gated re-mint succeeds, the new row-backed family commits the current suppression before use.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | providers §5                                | —               | open   |
| 22  | Defer host JA4/H2 fingerprint processing to a separate approved design; reject `[device] provider = "fastly"` at startup and do not persist fingerprint-derived classifications.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | providers §5                                | —               | open   |
| 23  | Permit a narrow `SecurityUse` authority for DataDome only: exact request-scoped security fields may reach the fixed HTTPS Protection API host/path, with redirects disabled, but no TS-controlled ad identity, graph, partner egress, persistence, or ordinary logs. `Request` is path-only and `Referer` origin-only; the remaining publisher path is an explicit vendor retention/DSR disclosure, not described as identity-free. Publisher-origin ClientID exposure is disabled by default; the cookie has one immutable configured domain/path scope so deletion is total for TS-created cookies, and its lost-response residual is explicit.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | hook §4a; permission §7                     | —               | open   |
| 24  | Malformed/absence suppression overrides a permissive baseline but clears on newer valid evidence; it is not sticky like an explicit use opt-out.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | permission §4.3, §4.1                       | —               | open   |
| 25  | Enforce a stored-jurisdiction/provenance horizon for batch sync: moves into stricter regimes fail closed by the horizon, and moves out require a live visit.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | permission §7                               | —               | open   |
| 26  | Aggregate embedded GPP GPC with `Sec-GPC` by OR as a global, non-destructive P4 use opt-out.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | permission §4.5                             | —               | open   |
| 27  | In proxy mode, decode only mapped opt-out fields and derive no grants.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | permission §4.4                             | —               | open   |
| 28  | Require product and written vendor conformance approval for the reduced DataDome surface: fixed `api-fastly.datadome.co/validate-request` egress, path-only Request/origin-only Referer, trusted connection IP/port with raw forwarding headers omitted, exact repeated-header normalization and request-field byte limits (including omitted `CookiesList`, `TlsCipher`, and `H2Fingerprint`, and opt-in JA4), a 24,576-byte encoded request ceiling, bounded fixed-scope cookie lifecycle, CSP/challenge behavior, hardened headers, reserved security budget, and security-owned replace-all forwarding of documented `X-DD-B` exactly once.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | hook §4a.2                                  | —               | open   |
| 29  | Accept rowless roaming-cookie expiry as a bounded residual only with telemetry, an explicit maximum lifetime, operator documentation, and a removal/sunset criterion.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | providers §5                                | —               | open   |
| 30  | Saturation blocks rowless admission for that prefix but never revokes an authenticated real row without its exact suffix; monitor NAT-cohort pressure.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | providers §5                                | —               | open   |
| 31  | Keep replay history bounded by evicting expired/grant entries first and retaining restrictive state for its full horizon; saturation never shortens a later opt-out.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | permission §4.3; providers wire schema      | —               | open   |
| 32  | Accept official GPP sections 24–27 version 1, pin their layouts to the vendored IAB commit, and treat complete decoder/fixture support as a release prerequisite. The vendoring evidence records the commit tree and per-source blob OID/SHA-256 for all four state layouts plus `Section Information`; a commit string alone does not close the gate.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | permission §4.5.1                           | —               | open   |
| 33  | Treat any malformed or unsupported-version **mapped** GPP section as a global blocker for grants to the permissions its schema maps (P4 in v1), while still honoring decodable opt-outs elsewhere and never deriving withdrawal from malformed bytes; unknown unmapped section IDs remain non-contributing.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | permission §4.5                             | —               | open   |
| 34  | Permit providers whose canonical identifiers cannot fit an injective 123-byte graph suffix to use the providers §2/§6.3 `sha256-detect` mode: 256-bit domain-separated collision resistance plus stored canonical-identifier comparison, fail-closed collision handling, no overwrite/join, and no cluster capability unless a literal prefix is independently preserved.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | providers §2, §6.3                          | —               | open   |
