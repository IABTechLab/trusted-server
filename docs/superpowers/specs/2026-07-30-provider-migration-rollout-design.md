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

| #   | Decision (today)                                                                                                                                                                    | After epic                                                                                                                                                                                                                                                                                                                                                                                                          | Status                                                                                                                                                                                                                   |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | EU/GDPR request, no TCF consent → no EC                                                                                                                                             | Same (opt-in baseline)                                                                                                                                                                                                                                                                                                                                                                                              | Preserved                                                                                                                                                                                                                |
| 2   | US-state request, GPC/GPP/USP opt-out → no EC, existing EC expired + tombstoned, **even when a consenting TCF string is present**                                                   | Same (precedence §4 of permission spec)                                                                                                                                                                                                                                                                                                                                                                             | Preserved — **must not regress**                                                                                                                                                                                         |
| 3   | US-state request, no signals at all → no EC (fail-closed)                                                                                                                           | Preserved by the recipe: the US rule is `requires_signal`, and the extended grant-signal class (permission spec §4) is what makes that possible — `granted` would allow no-signal traffic; a TCF-only grant class could not grant from GPP/USP values                                                                                                                                                               | Preserved under the recipe                                                                                                                                                                                               |
| 3a  | US-state request, explicit GPP `sale_opt_out = false` or a US Privacy string that is present and not opting out (including "not applicable") → EC allowed                           | Same: these are grant-class signals satisfying `requires_signal` (permission spec §4)                                                                                                                                                                                                                                                                                                                               | Preserved — **must not regress**                                                                                                                                                                                         |
| 3b  | US-state request, TCF record present and refusing Purpose 1 (no US opt-out signal) → no EC                                                                                          | Same: refusal beats coexisting non-TCF grant signals (permission spec §4, precedence 3–4)                                                                                                                                                                                                                                                                                                                           | Preserved                                                                                                                                                                                                                |
| 3c  | Consent-record conflict modes (restrictive/permissive/newest), expiry, KV fallback, proxy mode                                                                                      | Each row of the normalization matrix (permission spec §4.4) is individually marked preserved or changed there; changed rows: malformed-present now blocks acquisition                                                                                                                                                                                                                                               | Per §4.4 matrix                                                                                                                                                                                                          |
| 3d  | Valid + expired consent records: conflict resolution runs first and can select the expired record                                                                                   | Expired sources drop **before** conflict resolution (permission spec §4.4 pipeline)                                                                                                                                                                                                                                                                                                                                 | **Changed (declared)**                                                                                                                                                                                                   |
| 3e  | Only the GPP sale field (and USP) is consulted; `SharingOptOut` / `TargetedAdvertisingOptOut` are ignored                                                                           | All three fields enforced per the §4.5 mapping (targeted-advertising affects P4 only, never destructive)                                                                                                                                                                                                                                                                                                            | **New enforcement, declared** — opt-out effects are more protective; the same fields' not-opted-out values can also **newly grant P4**, which is not protective — both directions are classified in permission spec §4.5 |
| 3f  | Non-privacy-state US traffic (e.g. Wyoming) is non-regulated → EC allowed                                                                                                           | Same: policy enumerates `US/<state>` rules for configured privacy states; country-level `US` resolves non-regulated (permission spec §3.4)                                                                                                                                                                                                                                                                          | Preserved — **must not regress** (a country-wide `US = "us-opt-out"` rule would deny all of it)                                                                                                                          |
| 3g  | Graph rows persist JA4 class, H2 fingerprint hash, and buyer-facing quality metadata                                                                                                | Discontinued for new rows; v1 values retained read-only, never egressed, dropped at rewrite (providers spec §6.3)                                                                                                                                                                                                                                                                                                   | **Changed (declared)** — more protective                                                                                                                                                                                 |
| 4   | UK request, no TCF record → no EC                                                                                                                                                   | Same, unless the policy deliberately adopts a `granted` storage baseline for GB, with citation and sign-off                                                                                                                                                                                                                                                                                                         | Declared change (if made)                                                                                                                                                                                                |
| 5   | No country resolvable (geo unavailable) → no EC (fail-closed)                                                                                                                       | `default_country` baseline, constrained by permission spec §5.3 so the fail-open combination cannot occur silently                                                                                                                                                                                                                                                                                                  | Declared change, guarded                                                                                                                                                                                                 |
| 6   | Non-regulated country, TCF record refusing Purpose 1 → EC still created, existing identity never tombstoned                                                                         | Refusal now blocks _new_ grants everywhere (permission spec §4, precedence 3) — a declared, more-protective change. Existing identity is still **never tombstoned** where the baseline is `granted` (permission spec §4.2)                                                                                                                                                                                          | Split: creation is a declared change; no-tombstone is preserved                                                                                                                                                          |
| 7   | Country resolved but not in any regulation list ("non-regulated") → EC created, EIDs pass through                                                                                   | Governed by the policy's `rules.default` entry (permission spec §5.4). The §5 recipe sets it to a `granted` baseline to preserve today's behavior; the protective example policy instead requires a signal worldwide — a declared operator choice between the two                                                                                                                                                   | Preserved under the recipe; declared change under the protective default                                                                                                                                                 |
| 8   | Opt-out signal (GPC/GPP/USP) **outside** US states → ignored today                                                                                                                  | Revokes and withdraws globally (permission spec §4 and §4.2 trigger 1) — including tombstoning, which is irreversible                                                                                                                                                                                                                                                                                               | Declared change, more protective, **irreversible** — see §6.4                                                                                                                                                            |
| 9   | Fastly bot gate requires JA4 + platform class before KV-backed EC writes                                                                                                            | Only with `[device] provider = "fastly"`; the `builtin` default is UA-only                                                                                                                                                                                                                                                                                                                                          | Declared change with a documented restore step (§5)                                                                                                                                                                      |
| 10  | Fastly always resolves geo per request                                                                                                                                              | Only with `[geo] provider = "platform"`. The neutral geo default flips **only** in the permission model PR, together with the §5.3 guard — never in an intermediate step where absent geo would fail closed and zero EC issuance (providers spec §11)                                                                                                                                                               | Declared change, sequenced, with a documented restore step (§5)                                                                                                                                                          |
| 11a | Raw EC egress on paths gated by the jurisdiction gate today (OpenRTB `user.id`, derived request IDs, page bids, EIDs, identify, pull sync — pull checks the live `EcContext` today) | Gated by the egress inventory (permission spec §7): bidstream and partner egress require both purposes, revocation exempt — at least as strict as today for every path                                                                                                                                                                                                                                              | Preserved (strengthened); **must not regress** — PR #838 gated only EIDs and left `user.id` reachable                                                                                                                    |
| 11b | Proxy / click / Testlight forwarding extract the raw EC cookie/header **without** today's jurisdiction gate                                                                         | Gated by the egress inventory (both purposes)                                                                                                                                                                                                                                                                                                                                                                       | **New privacy hardening, declared change** — not preservation                                                                                                                                                            |
| 11c | Batch sync today only authenticates the S2S caller and checks live/tombstoned row state — it is **not** jurisdiction-gated                                                          | Gated by stored-provenance recompute (permission spec §7); legacy rows fail closed until backfilled                                                                                                                                                                                                                                                                                                                 | **New privacy hardening, declared change** — not preservation                                                                                                                                                            |
| 12  | EC generation succeeds without a configured identity-graph store                                                                                                                    | A minting provider requires an openable graph store at startup (providers spec §5, §6); pre-N+1 readiness step provisions it                                                                                                                                                                                                                                                                                        | **Breaking, declared** — graphless deployments must provision storage before upgrading                                                                                                                                   |
| 13  | Cookies minted by graphless deployments have no graph row                                                                                                                           | Rowless proof via strong-class records under the graphless-migration flag (N+2 convergence + attested stub-backfill first — providers spec §5); verified cookies expire and re-mint without continuity; **rowless withdrawal writes into the capped per-prefix `w` record, then expires** (exact-cookie family records are superseded); unverifiable roaming cookies get disclosed cookie-only expiry (sign-off 29) | **Declared** — pre-existing identities restart rather than carry over                                                                                                                                                    |

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
     enforcer for every N+2 record kind — not a field preserver.**
     Preserving unknown JSON does not chase aliases, consult family
     revocations, honor suppression records, or fail closed on
     provenance; an N+1 that merely preserved would, after rollback,
     treat revoked identities as live (aliases are reserved-future with
     the rewrite deferral, providers spec §6.1). N+1 must also **write
     family revocation records** — a withdrawal arriving on a
     rolled-back N+1 fleet must still revoke. **Authority-state: N+1 may create
     stubs and write negative entries, but never positive commits or
     clears.** The observed-row admission sequences (providers spec §5)
     need an s-class stub before revoking an untouched v1 row and a
     suppression CAS for a non-destructive signal — both are
     **negative/stub writes N+1 is permitted**, so a first post-upgrade
     GPC or SharingOptOut executes fully on N+1 (revocation, or a
     persisted suppression, not a deny-and-forget). What N+1 must **not**
     do is commit or clear **positive** authority (that needs the
     `AuthorityRefresh` revision protocol a v1 writer cannot produce):
     suppression created under N+2 stays in force through a rollback and
     its **clearing waits for roll-forward** — a protective, declared
     limitation. This is one contract, resolving the earlier
     "neither creates nor clears" wording that made first-upgrade
     withdrawal unexecutable.

     **N+1's identity-write behavior is v1, explicitly** — this resolves
     what was an impossible trilemma (write rows without provenance,
     violating active-after-commit; write provenance, violating the
     N+2-only writer boundary; or stop minting, an undeclared outage):
     N+1 **keeps minting v1 rows with today's semantics**, and the new
     active-after-commit/provenance contract activates **with the N+2
     writer**, not before. Likewise the permission model itself:
     **old-shape config on N+1 runs the pre-epic consent gate
     unchanged** — dual-read means dual-behavior — so the compiled
     protective fallback cannot flip behavior mid-convergence before the
     operator pushes the new-shape policy; the new model engages only
     with new-shape config. The interim is declared as sign-off item 20 — with one
     boundary that does **not** wait for N+2: once new-shape config is
     active, **context-free partner egress (batch sync) on N+1 fails
     closed for rows without provenance**, exactly as the permission
     spec's legacy rule requires. Otherwise N+1 would mint a P1-only v1
     row under the new model and then release it through today's
     row-state-only batch check — the fail-closed rule cannot activate
     later than the model it protects. Live-request paths keep v1
     semantics until N+2.

     Rollback tests therefore mirror the one contract exactly:
     family-revocation read **and write**; authority-state **stub
     creation and negative suppression entries, read and write** (the
     earlier "read-and-fail-closed only / N+1 writes none" test text
     contradicted the required observed-row sequences — a rolled-back
     N+1 receiving SharingOptOut must persist the suppression, not deny
     once and forget); **positive-authority commits and clears asserted
     forbidden**; and v1-minting behavior — all on N+1 against
     N+2-written data. **Rollback is binaries-first too, in the other direction** —
     N+2 → N+1 binaries roll back keeping the new config (N+1 reads it
     fully; reverting config first would hand the old shape to N+2
     binaries that reject it) — **with one structural rule that makes it possible at all**:
     providers are compiled into the composition root — there is no
     dynamic provider ABI — so an N+1 binary can only read what it
     shipped with. Therefore **every provider selectable in release R
     must ship compiled-in (dormant: registered, parseable,
     configurable, not selectable as writer) in R−1**; adopting a
     genuinely new provider gets its own reader-first rollout, exactly
     like the epic itself. With that rule, **schema rollback and provider rollback are
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
   together with **filling the PSL snapshot reference**
   (`psl-snapshot-ref.md` is a placeholder; ratification cannot
   reproduce the cookie-domain computation it approves until the
   vendored commit is recorded) and creating the §8 decision records.
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
   the floor exists); after the **fleet-convergence gate**, **N+2
   activates the writer** and begins emitting the new fields. **The rollback floor is crossed at N+2 writer activation itself** — an
   observable deploy event, recorded in the **deployment-metadata
   primitive** (providers spec §7 capability row; the existing
   config-store interface exposes ordinary put/delete and cannot express
   a monotonic floor) with a specified protocol, not an assertion: the
   marker lives in a dedicated namespace outside rollbackable config;
   the **first N+2 instance to activate creates/advances it via
   create-or-CAS** (the creation race resolves to one winner), **reads
   it back, and only then enables new-format writes**; every binary
   reads the floor at startup and a binary below the floor **fails
   startup**; an unreadable floor fails closed (writer stays disabled).
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
   GPC request withdraws correctly with zero migrated state. Mixed-version tests with stated expected results:
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
   `provider = "hmac"` with its block, `[geo] default_country`, and (for
   Fastly) the behavior-preserving `[device] provider = "fastly"` and
   `[geo] provider = "platform"` lines present with a comment stating what
   removing them changes. PR #838's example shipped the passphrase block
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
row): global opt-out honoring (row 8); refusal blocking new grants
everywhere (row 6); newly enforced GPP sharing/targeted fields, which can
also **grant** P4 where nothing granted before (row 3e); the FR
unresolved-geo fallback, where valid TCF consent can grant while today's
unresolved-geo path always denies (row 5); malformed-present blocking
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
textual delta against the example file. Per-adapter because a single
fixture cannot be: `[device] provider = "fastly"` is Fastly-only, and
`[geo] provider = "platform"` varies by host — Cloudflare **does**
support platform geo but resolves **country only, no region** (per the
providers spec adapter matrix), which changes state-level US privacy
outcomes and engages the declared regionless degradation; Axum and Spin
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
- `[device] provider = "fastly"` (Fastly deployments: preserves the JA4
  bot gate);
- `[geo] provider = "platform"` and `default_country = "FR"` (per-request
  jurisdiction detection preserved; the FR default is a **protective
  opt-in fallback**, not fail-closed — valid TCF consent still grants,
  where today's unresolved-geo path always denies);
- (Fastly fixture; other adapters substitute their valid selections)
  the full `gdpr-eu` / `gdpr-uk` / `us-opt-out` groups and country rules
  from the example policy (US as `requires_signal` with the grant-signal
  class — §2 rows 3–3b), plus the `non-regulated` group with
  `rules.default = "non-regulated"` (row 7). Operators who prefer the
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
2. Before/after deploy, operators watch **EC issuance rate** and EID
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
   specified (legacy-reader quiet period) are the
   pattern the rest follow.
3. Startup logs always print: selected provider per concern, whether geo is
   live, the effective default baseline, and the count of granted-without-
   signal permissions. One line, greppable, stable format.
4. **The batch-sync coverage dip is a gated rollout stage, not a
   notification.** Provenance-coverage thresholds are normative gate
   criteria: the guide defines a target coverage level and evaluation
   window; recovery stalling below threshold for the window triggers the
   **pause action** — investigate backfill (traffic mix, dormant rows),
   never disable the gate; and staging is explicit: provenance
   **writing** begins the moment N+2 activates, enforcement is already
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
5. Rollback is config-only where possible: reverting to the previous
   config version restores the previous behavior on the previous binary. The
   irreversible artifacts are enumerated — not "one": **family
   revocation records and member tombstones** (no recovery; that is
   their purpose), the **schema-floor marker** (write-once by design;
   no administrative clear), and — corrected from the former "sticky
   timestamp-less suppression" entry, which the permission spec's
   TTL-sticky rule supersedes — nothing suppression-shaped:
   timestamp-less opt-out suppression is **TTL-bounded and goes inert
   automatically** (administrative clear is an optional early exit, not
   a requirement, and the guide's cleanup and expiry tests follow the
   TTL rule). The irreversibility of revocation is also why the
   withdrawal triggers (permission spec §4.2) are exhaustive, why partial
   withdrawal failure has an explicit tombstones-first, browser-retries
   contract (permission spec §4.3), and why §2 rows 6 and 8 call out
   tombstoning explicitly. Two operational procedures are documented in the
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

These are decisions this spec set makes that #838 had not already made (or
made differently). **Implementation is blocked while any row is `open`**;
each row is a **decision, not an assignment**: the table tracks the
decision and its record; _who_ decided is captured inside the record
itself (`docs/superpowers/specs/decisions/NN-title.md` — the decision,
the deciders, the date). The Decision-record column holds the link (`—`
while open); an unratified row reverts to open, not to silently
implemented.

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Where                                       | Decision record | Status                                                                                  |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- | --------------- | --------------------------------------------------------------------------------------- |
| 1   | Opt-outs honored globally; destructive ones irreversibly withdraw outside the defining jurisdiction                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | permission §4, §4.2                         | —               | open                                                                                    |
| 2   | Sale opt-outs (GPP, USP) control both P1 and P4 and destroy the identity                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | permission §4.5                             | —               | open                                                                                    |
| 3   | Sharing / targeted-advertising fields: opt-outs remove P4 and retain the stored identity; the same fields' not-opted-out values **newly grant P4**                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | permission §4.5                             | —               | open                                                                                    |
| 4   | US contextual auctions continue during opt-out, identity removed                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | permission §7                               | —               | open                                                                                    |
| 5   | Regionless US traffic treated as non-regulated unless the operator opts into country-wide gating                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | permission §3.4                             | —               | open                                                                                    |
| 6   | Full consent strings continue downstream; raw consent snapshots retained in rows for audit                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | providers §6.3                              | —               | open                                                                                    |
| 7   | Legacy batch-sync traffic rejected until live-browser provenance backfill                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | this spec §6.4; permission §7               | —               | open                                                                                    |
| 8   | Proxy / click / Testlight forwarding newly gated by P1 ∧ P4                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | §2 row 11b                                  | —               | open                                                                                    |
| 9   | Integration cookie operations — deferred out of the v1 hook with the full read/use/withdraw model as entry bar                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | hook §3                                     | —               | open (descope ratification still required; record-less ⇒ open per the decisions README) |
| 10  | Session-cookie exemption question                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | hook §3                                     | —               | open (deferred, but record-less ⇒ open per the decisions README)                        |
| 11  | A single failed destructive-revocation **or suppression** write may leave S2S identity use live **indefinitely** for a never-returning visitor (no durable external retry queue)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | permission §4.3                             | —               | open                                                                                    |
| 12  | Adapters without revocation-eligible storage migrate **stateless** rather than blocking the release                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | §4.2 of this spec                           | —               | open                                                                                    |
| 13  | Batch-sync acceptance dropping toward zero at cutover, with dormant identities having no automatic recovery path                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | §6.4 of this spec                           | —               | open                                                                                    |
| 14  | Policy tightening never reuses stored refusals destructively — a fresh, live post-change refusal is required (the spec decides this; ratify it)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | permission §4.2 trigger 2                   | —               | open                                                                                    |
| 15  | **Epic descope**: client-cycle spec demoted to deferred-informative; `rewrite_legacy` cut to a recorded deferral; hook ships headers-only                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | client spec status; providers §6.1; hook §3 | —               | open                                                                                    |
| 16  | Timestamp-less opt-out suppression is **TTL-sticky**: within its consent-TTL lifetime only a newer timestamped grant clears it; at `valid_until` it goes inert automatically (not user-sticky-forever, not administratively sticky — administrative clear is an optional early exit)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | permission §4.3                             | —               | open                                                                                    |
| 17  | Explicit N/A values are grant-class and can newly authorize personalized advertising                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | permission §4.5                             | —               | open                                                                                    |
| 18  | Permissive `default_country` remains in effect during prolonged geo-provider failure (metered residual)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | permission §5.2                             | —               | open                                                                                    |
| 19  | Mixed policy revisions during rollout can produce irreversible destructive outcomes on part of the fleet                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | permission §5.5                             | —               | open                                                                                    |
| 20  | N+1 interim: v1 minting semantics and pre-epic gating persist under old-shape config until N+2/new-shape                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | migration §4.4                              | —               | open                                                                                    |
| 21  | Rowless legacy cookies are expired and re-minted **without continuity** (prefix-only verification cannot authenticate the suffix; adoption would let suffix variants mint unbounded rows)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | providers §5                                | —               | open                                                                                    |
| 22  | Device fingerprint (JA4/H2) processing authorized by operator selection, with the boolean classification persisted — collection purpose, retention, downstream visibility, and the vocabulary-extension boundary                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | providers §5                                | —               | open                                                                                    |
| 23  | **Open question, not ratified**: may DataDome's security identifier (tag injection, `datadome` cookie, `X-DataDome-ClientID` read and vendor egress) operate outside the permission model? The decision must enumerate exactly which consumers may observe the cookie/ClientID — the enumerated observers are the security channel **and the publisher origin, which receives `X-DataDome-ClientID` via the upstream overlay** — "owner-scoped overlay" names the mechanism, this row names the recipient — plus retention, whether TS withdrawal expires it, **the browser-side observers the vendor's design implies — every same-origin page script (the cookie must not be HttpOnly per vendor guidance), vendor challenge pages executing with publisher-origin access, and (if header mode is ever opted into) the JavaScript/local-storage observer** — and challenge redirect targets | hook §4a; permission §7                     | —               | open                                                                                    |
| 24  | Malformed/absence-caused suppression clears on any newer valid grant (non-sticky; opt-out stickiness applies only to opt-out causes) — and an active suppression **overrides a `granted` baseline**: one malformed request denies later no-signal requests until valid evidence clears it, and a policy-baseline grant alone never clears                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | permission §4.3, §4.1                       | —               | open                                                                                    |
| 25  | Batch-sync stored-jurisdiction maximum age (consent-TTL horizon): a mover into GDPR stops old-rule egress at the horizon; a mover out is denied until a live visit                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | permission §7                               | —               | open                                                                                    |
| 26  | Embedded GPP GPC maps to the destructive global opt-out (header-OR-embedded aggregation)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | permission §4.5                             | —               | open                                                                                    |
| 27  | Proxy-mode minimal opt-out extraction (decode only §4.5-mapped opt-out fields; no grants)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | permission §4.4                             | —               | open                                                                                    |
| 28  | DataDome integration is deliberately reduced relative to vendor defaults (spec-pinned pointer allowlist starting at ClientID-only; hardened cookie attributes) — requires product **and vendor** acceptance — including CSP interaction with vendor challenge pages, same-origin vendor code on the publisher origin (or an origin-isolation/sandboxing requirement), and the fail-open consequence of batch invalidation                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | hook §4a; `datadome-header-allowlist.md`    | —               | open                                                                                    |
| 29  | Unverifiable roaming rowless cookies receive best-effort cookie-only expiry (admission rules forbid durable records for unverified values); a lost response can leave the cookie usable on the old network until re-presented                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | providers §5                                | —               | open                                                                                    |
| 30  | Prefix-wide rowless saturation: the per-prefix cap (8) and its escalation treat every rowless cookie behind one IP-derived prefix as withdrawn — NAT cohorts can be affected by one actor; cap value, threat assumptions, expected cohort size, reset/retention, observability, **and the saturation collateral: under a saturated prefix, any real row (listed or overflow) is denied and revoked immediately on surfacing — a non-abuser NAT-cohort row can be revoked; `w` is retained through the max(cookie, row, S2S) horizon and consulted by `valid_until`, not the flag, so this is deterministic, not a retention accident** — all in scope                                                                                                                                                                                                                                         | providers §5                                | —               | open                                                                                    |
| 31  | Replay-history capacity (16 per-source semantic-state slots + a saturation epoch whose restrictive marker is pinned at the **first** restrictive overflow with its own full TTL): while saturated, fresh consent cannot grant until the epoch expires, and **later restrictive overflows inherit the first marker — a bounded shortening of their lifetime**                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | permission §4.3; providers wire schema      | —               | open                                                                                    |
| 32  | GPP sections 24–27 (MD/IN/KY/RI) are reserved with **national-section-only** handling until an official binary layout can be vendored — a state-specific opt-out expressed only in an undecodable state section is not honored                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | permission §4.5; `gpp-registry-snapshot.md` | —               | open                                                                                    |
