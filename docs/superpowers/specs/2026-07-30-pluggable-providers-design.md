# Design Spec: Pluggable Edge Cookie, Device, and Geo Providers

**Status:** Draft
**Author:** Engineering
**Issue references:** #777, #778, #780, #781
**Related specs:** `2026-07-30-permission-model-design.md`,
`2026-07-30-provider-migration-rollout-design.md`,
`2026-07-30-client-cycle-ec-resolve-design.md`
**Last updated:** 2026-07-31

> **Context.** PR #838 proposed a first implementation of this epic in a single
> change. Review of that PR surfaced design gaps this spec exists to close
> before a second implementation pass: an identity abstraction that owned
> minting but not recognition, per-adapter divergence in provider selection,
> silent misconfiguration modes, and speculative trait surface with no
> production caller. This spec is the authoritative statement of what the
> provider architecture must do; where it contradicts PR #838, this spec wins.

---

## 1. Overview and goals

Trusted Server makes three per-request data decisions that are currently
hard-wired: whether to create or keep an Edge Cookie (EC) identity, how to
classify the requesting device, and whether to resolve geolocation. Each
becomes a **provider**: a selectable component chosen in operator
configuration, with a deliberately neutral default.

Goals:

- A deployment picks an implementation per concern (including none) without a
  code change to Trusted Server core.
- Defaults are neutral: with no configuration, no EC is created, device
  classification uses only the User-Agent, and no geolocation is performed. A
  default deployment makes no third-party or host-specific call.
- An **EC provider declares** the permissions its data use requires (see the
  permission model spec); **core enforces** that declaration. A provider
  cannot authorize itself. (Geo and device providers are governed
  differently, for two different reasons spelled out in §5.)
- All adapters (Fastly, Axum, Cloudflare, Spin) behave identically for
  identical configuration, or fail loudly at startup where a host cannot
  satisfy the selected provider.

Non-goals:

- No vendor provider ships in this epic beyond the host-platform
  implementations named below.
- The client-cycle (browser round-trip) provider type is **out of scope**
  here; it has its own spec and must clear that spec's requirements first.

## 2. Provider taxonomy

| Concern     | Trait                | Built-in default            | Opt-in host implementation                                                     |
| ----------- | -------------------- | --------------------------- | ------------------------------------------------------------------------------ |
| EC identity | `EdgeCookieProvider` | none (stateless)            | `hmac` (in core; HMAC over client IP, preserves today's identity)              |
| Device      | `DeviceProvider`     | `builtin` (User-Agent only) | `fastly` (JA4 / HTTP-2 fingerprints) — deferred; startup-rejected in this epic |
| Geo         | `GeoProvider`        | none (no location)          | `platform` (host geo lookup)                                                   |

Selection keys are strings in operator configuration:

```toml
[ec]
provider = "hmac"

[ec.providers.hmac]
passphrase = "example-passphrase"

[device]
provider = "builtin"

[geo]
provider = "platform"
```

**Deliberately not carried over from PR #838:** the `host-signals` EC
provider (identity from HMAC over JA4/HTTP-2 TLS fingerprints plus client
IP). Minting _identity_ from TLS fingerprints is a different privacy
proposition from device _classification_ (#780) and was specified by no
issue; if wanted, it returns with its own spec and its own vocabulary
discussion. A config selecting `provider = "host-signals"` is rejected at
startup like any unknown key (migration spec §4).

## 3. The identity lifecycle contract

This is the section PR #838 lacked, and the source of its most structural
defect: the trait abstracted **minting** an identifier but left
**recognition** (`is_valid_ec_id`), **hashing** (`ec_hash`), and **KV key
normalization** hard-coded to the built-in HMAC shape. Any provider whose
identifiers do not match `{64hex}.{6alnum}` minted cookies that the very next
request discarded, and whose identities could never be tombstoned on
withdrawal.

An `EdgeCookieProvider` owns the **complete lifecycle** of the identifiers it
mints. Every lifecycle operation core performs on an EC value MUST be routed
through the selected provider:

| Lifecycle operation                      | Where core uses it today                                                                                               | Contract                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Mint**                                 | EC generation on first eligible request                                                                                | Provider returns the identifier (and only core writes the cookie).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| **Parse / canonicalize**                 | Reading `ts-ec` back from the request; deciding `ec_was_present`; batch-sync ingestion                                 | Provider parses a cookie value into its **canonical** identifier, or rejects it. Canonicalization and **equivalence are provider-declared, never imposed globally**: each provider ships equivalence fixtures naming exactly which variants are the same identity — case sensitivity is provider-specific (signed/base64-style envelopes are case-sensitive; even the built-in HMAC id is case-insensitive only in its hex prefix, with a case-preserved suffix). Declared-equivalent values parse to the same canonical identifier (satisfying #778). A value the selected provider does not recognize is treated as absent (but see §6.1 legacy readers). |
| **Canonical graph key**                  | KV identity-graph row reads/writes                                                                                     | The provider supplies a canonical key **suffix**; **core constructs the physical key** per the §6.3 key grammar (legacy-HMAC verbatim keys excepted), so cross-provider and cross-record-kind isolation is enforced by construction rather than promised by provider code. A provider either emits an injective suffix within the 123-byte portable grammar or declares the collision-detecting SHA-256 form defined in §6.3. Two equivalent envelopes of one identity map to one key — verbatim cookie bytes as the key would fork graph rows on canonicalization differences and discard today's batch-sync canonicalization.                             |
| **Cluster prefix** (optional capability) | IP-cluster sizing (`cluster_trust_threshold`, implemented as a **KV prefix listing**), pull-sync dedupe, log redaction | A provider declaring cluster support returns a prefix that is a **literal byte prefix of the canonical graph key** — the cluster count lists keys by prefix, so an independently derived hash that is not an actual key prefix silently reports the wrong cluster size. The prefix deliberately collides across identifiers minted from the same client evidence. A provider without the capability declares so, and cluster-dependent gating follows a configured degradation policy (treat cluster size as unknown, with the KV-write decision that implies made explicit in config) instead of counting garbage.                                         |
| **Tombstone**                            | Withdrawal: expiring the cookie and writing revocation markers                                                         | The identifiers eligible for tombstoning are exactly those the provider parses — never a shape-gated subset.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |

**Invariant:** for every provider `P` and every identifier `id` minted by `P`,
`P.parse` round-trips `id` — including every variant `P`'s declared
equivalence fixtures name, all of which canonicalize to the same identifier
and graph key; where `P` declares cluster support, `cluster_prefix(id)` is a
literal prefix of `graph_key(id)` and is shared by identifiers minted from
the same client evidence; and a withdrawal request carrying `id` tombstones
it. A conformance test suite MUST assert this round-trip for every shipped
provider — driven by each provider's equivalence fixtures, plus
cross-provider key-namespace and KV length/charset cases — and the suite
MUST be written so a future provider crate can run it against its own
implementation. Conformance tests inject deterministic entropy;
probabilistic assertions ("two random suffixes differ") are not accepted.

Three global rules sit above every provider:

- **Identifier bounds.** A minted identifier obeys a global cookie-safe
  alphabet (valid cookie-octets: no separators, whitespace, or control
  characters; normatively `[A-Za-z0-9._~-]`) and a global maximum of
  **256 bytes** — stated here, in the normative contract, so dependent
  documents reference one number instead of assuming their own — for the identifier itself, not
  only the graph key — enforced by core at mint and at parse, so no
  provider can emit a value the cookie layer or logs cannot carry.
- **Graph-key representability.** The 256-byte identifier bound does not imply
  that every identifier can be injectively encoded in the 123-byte physical-key
  suffix. Each provider's namespace descriptor therefore declares exactly one
  graph-key mode: `injective`, whose conformance fixtures prove a one-to-one
  canonical suffix within the portable grammar and length cap; or
  `sha256-detect`, whose suffix is `h` plus unpadded base64url of
  `SHA-256("tsgk1|" || provider_code || canonical_identifier_bytes)` (44 ASCII
  bytes total). Known-answer vector: provider code `vend` and canonical
  identifier bytes `AbC123` produce
  `hY1DtOYYBwBBUs-ZPabyh5JOC_aqc5mdCQngW7yfcHTY`. A `sha256-detect` row stores the canonical identifier bytes in
  its encrypted/identity-bearing row envelope and every read compares them in
  constant time before returning the row. A mismatch is a cryptographic key
  collision: the read and attempted write fail closed, the existing row is
  never overwritten or joined, and a fleet-visible security alert is emitted.
  Such a provider cannot declare cluster-prefix support unless its graph-key
  suffix independently preserves the required literal prefix. The security
  model is explicit 256-bit collision resistance plus collision detection,
  not the mathematically impossible claim that arbitrary 256-byte identifiers
  map collision-free into 123 bytes.
- **Namespaces are declarative and core-proven.** Disjointness of two
  opaque `parse` functions is not provable, so every provider declares a
  **static namespace descriptor** in a core-owned declarative form — a set
  of literal prefixes and/or fixed-shape grammars (alphabet + length
  segments), never arbitrary parser logic. Core proves pairwise
  disjointness of all configured descriptors at startup (§6.1), and the
  conformance suite asserts each provider's `parse` accepts **only**
  values matching its declared descriptor — so the declaration, not the
  parser, is the authority the overlap check rests on.
- **Namespace reservation.** The legacy HMAC grammar `{64hex}.{6alnum}` is
  formally **reserved as the `hmac` provider's namespace descriptor**. `hmac`'s graph
  key is the identifier verbatim and its cluster prefix is the 64-hex
  prefix, so every pre-epic row stays reachable and every prefix listing
  intact (migration spec §3) — and **no other provider may mint
  identifiers or produce graph keys matching that grammar**, which is what
  makes verbatim-compatibility and provider-namespacing coexist.
  Conformance fixtures include an existing pre-epic row (reachability) and
  a prefix-listing case. For `hmac`, the equivalence fixtures pin:
  uppercase/lowercase hex-prefix variants are equivalent; suffix case is
  preserved and significant.
- **Cluster size means live identity rows.** Prefix counting lists
  identity-row keys; family, suppression, reservation, and transaction
  records live in other namespaces and never inflate a count. Member
  tombstones share the identity key but carry the short cleanup TTL, so
  their inflation is transient and biases conservative (an over-count
  trips the trust threshold toward denial, never toward extra writes);
  the listing filters by the value's `kind`/liveness within the existing
  list limit where the backend returns values, and the residual
  over-count where it cannot is declared. `cluster_trust_threshold` is validated against the backend's listing
  cap at startup — a threshold of 200 against a 100-key listing cap
  would make every capped count look trusted; the count must page or
  saturate at threshold + 1. A computed cluster size is
  **not persisted beyond its inputs' lifetime**: today's code stores the
  calculated `cluster_size` in the row and reuses it for the row's full
  TTL, which would freeze a tombstone-inflated count for up to a year —
  stored values carry a short validity (within the tombstone-TTL
  horizon) or are recomputed on use. Aliases are reserved-future
  (§6.1) and excluded by `kind` when they exist.
- **No-cluster behavior is still defined.** A provider without cluster
  support deduplicates pull-sync by canonical graph key and redacts logs
  with a fixed-length hash of the graph key; `cluster_fallback` (§6.1)
  governs only the trust/write decision, not these.

## 4. Trait surface: minimalism rule

Every trait method MUST have at least one production (non-test) caller in the
same PR that introduces it. Speculative surface observed in PR #838 that MUST
NOT ship without a caller:

- `keys_equal` (no production caller; existed to serve a unit test — its
  legitimate purpose, #778's equivalent-envelope comparison, is satisfied
  structurally by §3's canonicalizing `parse` instead: equivalents
  canonicalize to the same identifier, so no comparison method is needed),
- `GeneratedEdgeCookie::response_headers` (empty in all built-ins, plumbed
  through three layers),
- `IdentityInput.permissions` / `IdentityInput.consent` (ignored by all
  built-ins),
- `DeviceProvider::required_permissions` / `GeoProvider::required_permissions`
  — dropped entirely, not deferred: §5 explains why — geo _cannot_ be
  gated (circular), and device _could_ be but deliberately is not (a
  recorded decision, not an impossibility).

If a future feature needs one of these, it arrives with that feature.

The minimal `EdgeCookieProvider` surface implied by §3 is:

```rust
pub trait EdgeCookieProvider {
    /// Stable configuration key ("hmac").
    fn id(&self) -> &'static str;
    /// Permissions this provider's data use requires. Enforced by core for
    /// minting and identity use — never for parse/tombstone (§5).
    fn required_permissions(&self) -> PermissionSet;
    /// Parse and canonicalize a cookie value into this provider's
    /// identifier; None when unrecognized. Identifies the provider
    /// NAMESPACE only — never a configuration version (§6.1: versions
    /// resolve from row provenance). Values the provider's declared
    /// equivalence fixtures name as equivalent canonicalize identically.
    fn parse(&self, value: &str) -> Option<EcId>;
    /// Canonical graph-key SUFFIX (bounded length, KV-safe). Core — not
    /// the provider — constructs the physical key (§6.3 key grammar), so
    /// cross-provider and cross-record-kind isolation is structural.
    /// Sole exception: hmac keys (every version) are the identifier
    /// verbatim — the reserved legacy grammar.
    fn graph_key_suffix(&self, id: &EcId) -> GraphKeySuffix;
    /// Cluster capability: a literal byte prefix of the physical graph
    /// key, shared across identifiers minted from the same client
    /// evidence. None when the provider lacks IP-cluster semantics (§3).
    fn cluster_prefix(&self, id: &EcId) -> Option<HashPrefix>;
    /// Mint an identifier from request evidence. The one acquisition
    /// operation of the epic (server mint); failure means no identity
    /// this request (§6.2). Returns the identifier WITH the active
    /// configuration version — the immutable mint tag needs it and core
    /// cannot reach into provider-specific configuration to learn it.
    fn generate(&self, input: &IdentityInput<'_>)
        -> Result<GeneratedIdentity, Report<EcError>>;
    // GeneratedIdentity { id: EcId, mint_version: ProviderVersion }
    /// Cryptographic verification of a parsed identifier against request
    /// evidence. Recognition (`parse`) is not authentication; rowless
    /// handling (§5) requires this. Returns the matched configuration
    /// version — provenance needs it and a bool cannot carry it — or
    /// None when nothing verifies.
    fn verify(&self, id: &EcId, input: &IdentityInput<'_>) -> Option<VerifiedIdentity>;
}
// VerifiedIdentity { version: ProviderVersion /* … */ }
```

The acquisition-mode enum (`ServerMint` / `ClientResolve`), the
`ClientResolveContext` contract, replay-reservation schemas, and the
reservation capability rows are **not part of this normative surface** —
they live in the deferred client-cycle document and return with that
feature, per this spec's own minimalism rule (§4): the epic's only
acquisition mode is server mint, expressed directly as `generate`.

(Names indicative; the shape is normative. `required_permissions` joins the
trait at step 5 of §11, together with its enforcement point.)

## 5. Permission enforcement is core's job — for EC providers

The gate is on **minting and identity use, never on the lifecycle
operations that withdrawal depends on**. Before minting through an EC
provider or using an identity (raw-EC egress, permission model spec §7),
core resolves the request's permission set and refuses when the provider's
`required_permissions()` are not all set. **Parse, canonicalization, graph
lookup for revocation, and tombstoning always run**, permissions or not — a
blanket execution gate would refuse to run the provider in exactly the
state an opt-out produces, making the withdrawal it demands impossible. A
spy-provider test pins the split: with `store-on-device` unset, `generate`
is never called while a withdrawal request still parses the cookie and
writes tombstones.

**A generated identity is not active until its authority-state record
commits** (the two-record commit point — the graph row alone is not
activation). No cookie write, no egress, no auction use may observe a
minted identifier before both writes have committed — PR #838 let a
generated EC reach an auction before finalization refused the cookie,
producing an identity that existed for one request and nowhere else. The
normative order is: gate → `generate` → graph-row commit →
**authority-state commit** (the strong record reporting the row's
revision — the commit point of the two-record protocol, permission model
spec §4.3) → cookie scheduled → eligible for egress. Eligibility begins
at the authority-state commit and nowhere earlier. **Mint-path failure
between the two writes is declared, not "recovered"**: no cookie was
emitted, so no later request can identify the orphan — the earlier
claim that recovery "re-runs step 2" is false for the mint path (it
holds only for _presented_ identities via `AuthorityRefresh`). An
orphaned row (or orphaned pending record, if the order's first write
succeeded alone) authorizes nothing — the strong record never reported
it — and is bounded by its `expires_at`/retention TTL; the failure is
counted (a first-class metric) and appears in the §6.2 runtime matrix. "Cookie scheduled" means queued onto the
final response — `Set-Cookie` is physically emitted after first-request
processing, so egress eligibility begins at the **authority-state commit**
(§5 mint order — not graph commit, and not header emission). A
graph-commit failure means the mint never happened: no cookie, no egress,
error logged, the next request retries.

**Pre-existing rowless cookies are expired and re-minted — never
adopted.** An earlier adoption design failed on an authentication limit:
HMAC verification can authenticate only the 64-hex prefix; the 6-char
suffix is independent randomness, so a client holding `H.aaaaaa` can
present `H.aaaaab`, `H.aaaaac`, … — every variant prefix-verifies, and
an adopt path would mint a **separate durable row and family per
variant**. Therefore:

- **"Rowless" is proven from the strong class, never from eventual
  storage.** Identity-row visibility may be eventual, so a plain
  not-found proves nothing — a just-minted row invisible on a stale
  replica would classify its own cookie as rowless and fork the
  identity, and "the backend's strongest read" over eventual storage is
  not an authoritative primitive. The proof needs more than the flag —
  a per-deployment flag cannot prove a per-cookie fact, and "no
  authority-state record" alone would misclassify every graph-backed
  legacy row and every N+1-minted v1 row (neither has one). The
  protocol closes both gaps with **prerequisites for setting the
  flag**: (1) the fleet has fully converged on **N+2** (no v1 minting
  anywhere — an N+1 fleet must never run rowless classification), and
  (2) an idempotent **stub-backfill scan** has stamped a minimal
  authority-state existence stub onto **every existing identity row**
  (legacy and N+1-minted alike) — and "every" is established by an
  actual algorithm over an eventual store, not asserted: after N+2
  convergence at time T, wait the backend's **documented listing settle
  window** (a listing-completeness bound is a declared adapter
  capability; a backend that cannot bound listing visibility cannot host
  this migration), then scan repeatedly until **two consecutive full
  passes discover zero unstubbed rows**; the flag value attests the
  watermark, pass count, and settle window. Rows minted after T carry
  records by protocol — **and rollback cannot silently break that
  invariant — with fleet-linearizable mechanics, not a hopeful CAS**:
  the flag is a **globally-strong state machine**
  (absent → active → suspended → re-attested-active → cleared, each
  transition bumping an epoch; this metadata key requires globally
  observable strong reads _in addition to_ CAS — a capability cell,
  since CAS alone says nothing about what other instances currently
  see). The suspension transition stamps a **fleet-stable `not_before`
  deadline** into the suspended epoch — **and the deadline comparison
  has a clock contract, not just the write**: where the backend issues
  store time, `not_before` = store time + L and every instance's
  `now ≥ not_before` check **re-reads store current time on the same
  strong primitive** — one clock domain end to end, because a fast
  local clock compared against a store-issued deadline could cross it
  while another instance's N+2 lease was still valid. Where the
  backend has no store clock, `not_before` = committer time + L +
  S\*fleet and every local comparison subtracts S\*fleet again
  (mint only when `now − S_fleet ≥ not_before`) — **S_fleet is the
  maximum pairwise fleet clock skew, a declared and monitored
  infrastructure bound, a separate qualification from the
  evidence-timestamp tolerance S even though both are assigned 300 s**
  (an adapter capability cell states which branch the backend
  qualifies for; a deployment that can guarantee neither store time
  nor a fleet-skew bound cannot host the graphless migration). Without
  the bound, a slow-clocked suspender could write a deadline already
  passed on another instance while an N+2 lease survived, and a
  fast-clocked N+1 could mint before the fleet had quiesced — where
  **L is an assigned constant (120 s)**; **`not_before`, its clock
  domain (store or committer), and L are serialized fields of the
  metadata value** (the schema names them), and clock-skew
  (fastest-observer and slowest-committer schedules) and
  suspender-restart schedules are named tests: **every** N+1 instance — the one that suspended, a second that
  starts and reads an already-suspended state, or one recovering after
  the suspender crashed — refuses to mint until the branch-appropriate deadline comparison passes (store-clock branch: strong-read `store_now ≥ not_before`; fallback branch: local `now − S_fleet ≥ not_before`)
  (globally strong read of the epoch). N+2 instances prove `active` at
  classification time through a **bounded lease ≤ L** (strong read at
  lease expiry). So suspension is fleet-effective within L, no minting
  occurs before `not_before` = suspension + L, and **no unstubbed
  cookie can exist while any instance still holds an `active` lease** —
  closed by construction, crash-safe, and independent of which instance
  suspended. The state chain is cyclic:
  absent → active → suspended → re-attested-active → **suspended**
  (a second rollback) → … ; CAS losers on any transition re-read and
  retry against the winner's epoch. Under `suspended`, rowless
  _classification_ stops but **`w` consultation and enforcement
  continue** (withdrawn stays withdrawn). Re-activation after
  roll-forward requires complete re-attestation over the gap window.
  The N+2 → rollback-to-N+1 → mint → roll-forward-to-N+2 schedule is a
  named test proving those rows are never classified rowless. And because no scan over an eventual store is
  provably perfect, misses are **reconciled, not fatal**: a per-prefix
  withdrawal entry (below) doubles as pending intent — if a real row for
  a withdrawn suffix ever surfaces, core **promotes** the entry to a
  full family revocation on that row's family at first sight, so a
  missed row cannot quietly keep S2S egress after its cookie was
  withdrawn. Only then does the invariant hold:
  every row-backed identity has an authority-state record under its
  derivable family ID, in the globally-strong class — so _rowless_ =
  flag set AND the strong read finds **no record** for the cookie's
  derived family ID — and **every HMAC row discovery consults the prefix's `w` state
  before any live or S2S use whenever a live `w` record exists**
  (keyed on the record's `valid_until`, never on the flag — an earlier
  "while the flag is active" scope contradicted the valid_until rule
  one sentence later) (an exact pending suffix means promotion-then-denial;
  saturation alone governs only rowless admission), so a withdrawn suffix cannot slip into use
  through the row path. **`w` consultation is keyed on the record's `valid_until`, not the rowless-classification flag** — the flag may clear after one cookie lifetime while `w` is retained through the longer max(cookie, row, S2S) horizon, and a late row must still find a live `w`; enforcement ends only when the `w` record itself expires.

  **The classification decision is an ordered procedure, not a
  Cartesian table** — the earlier five-column table was neither total
  nor disjoint (overlapping negative rows with different outcomes, a
  rowless row keyed on an "authoritative not-found" eventual read the
  store cannot produce, and no rows for absent authority records or
  nonmatching revisions). The procedure is evaluated top-down,
  **first match wins**: disjointness holds by construction (each step
  assumes every earlier step did not match) and totality by the final
  default. **Negative gates are release-, config-, and
  flag-invariant** — steps 2–6 run before the v1 exception and before
  every positive path, so the v1 exception relaxes only the _positive_
  side and an N+2-written record still binds a rolled-back N+1
  (migration spec §4.4):
  1. **Any read error** (safety breaker, `q`, authority/family records,
     `w`, or a consulted row read)
     → indeterminate: no use, no mint, no expiry, and no writes whose
     admission depended on the failed read. Admitted negative writes
     still proceed: a successfully read strong authority record proves
     family admission (§5), so a live destructive signal commits its
     family revocation (and a non-destructive one its suppression CAS)
     even when the eventual row read failed — only the browser-cookie
     expiry waits for the commit. A row read failure must never leave
     S2S authority live against an already-provable withdrawal.
  2. **Global identity safety breaker active** → no positive identity mint,
     use, graph access, or egress. Negative repair, withdrawal, and deletion
     operations continue so the deployment can recover.
  3. **Pending family intent in `q`** → apply its negative meaning
     immediately: pending revocation denies the family; pending suppression
     denies only its mapped permission. Workers continue the idempotent target
     write; positive evaluation never skips ahead of the intent.
  4. **Live exact suffix in a `w` entry** → withdrawn: denied, and
     promoted to family revocation at first sight of a real row (§6.2).
     If already revoked, promotion is idempotent. **Saturated prefix with
     no exact suffix match** → deny only a rowless candidate; an
     authenticated row continues to the family/suppression checks below
     and is never revoked from cohort membership alone.
  5. **Family revocation present** → denied — all semantics, all flag
     states.
  6. **Live suppression entry for a permission** → that permission is
     denied (identity retained — non-destructive); evaluation
     continues below for permissions without a live suppression.
  7. **v1 semantics** (release N+1, or N+2 under old-shape config) →
     v1 positive behavior: recognized cookies are used per pre-epic
     rules, rows not required — the declared v1 exception, not an
     outage: the pre-epic privacy posture persists until the new model
     activates (matrix row 14). Steps 2–6 have already run.
  8. **New model, row found:**
     - authority record **absent or stub-only** (no positive summary)
       → no egress, but not a dead end: the live-backfill path applies
       — a live request resolving a regime-accepted grant runs the
       permission-exempt `AuthorityRefresh` (admission is the observed
       row + live resolution, never a prior summary or matching
       revision), commits the positive summary, and the fence then
       opens use (permission spec §7 legacy rule);
     - positive summary present and
       `row.provenance_revision == summary_revision` → **normal use**,
       in every flag state — active and suspended included: the flag
       governs rowless _classification_ only, never row-backed
       operation;
     - positive summary present, revisions **do not match** → the
       fence fails: the row's identity/partner data is withheld and
       the strong summary alone governs (exactly the S2S posture); a
       live request re-runs `AuthorityRefresh` to re-commit and
       realign the fence, then proceeds.
  9. **New model, row not found** (successful eventual read — which by
     itself proves nothing):
     - strong records for the derived family **absent on the
       authoritative strong-class read** and flag = `active` →
       **rowless**: expire-and-re-mint, request-local non-destructive denial,
       or destructive `w` withdrawal per §5 — the
       strong-class absence is the proof; the eventual read is never
       the evidence;
     - any strong record present → **visibility lag, not absence**:
       the record proves a row committed, so the identity is
       indeterminate this request — no use, no mint, no expiry (a
       stale replica must not fork a just-minted identity);
     - flag absent, cleared, or suspended → indeterminate (no rowless
       classification without an `active` flag; suspension pauses
       classification pending re-attestation).
  10. **Default** — any state not matched above → indeterminate: no
      use, no mint, no expiry.

  Graphless-era cookies never got a stub because they have no row for
  the scan to find; no eventual read participates. The flag itself is
  specified: a named deployment-metadata key (write-once/CAS class),
  set by the §4.2 migration runbook step (migration spec §4) only on
  deployments that actually ran graphless (requires the
  deployment-metadata capability), surviving binary rollback, and
  **cleared by an explicit operator action** once the migration window
  closes (quiet-period criterion in the guide) — clearing ends rowless
  classification permanently. Outside the flag, or on any read error,
  the state is **indeterminate**: no identity use, no mint, no cookie
  expiry — "treated as absent" was the wrong contract, since absence
  feeds the fresh-mint path (admitted negative writes still proceed,
  per step 1).

- A verified rowless cookie (`verify → VerifiedIdentity`, carrying the
  matched version) is **expired and replaced by a fresh mint through the
  ordinary graph-backed path** when permissions allow; continuity is
  deliberately not preserved (migration matrix row 13, sign-off 21). An
  unverifiable cookie (including the declared roaming false-negative)
  gets **cookie-only expiry — a disclosed best-effort residual, not a
  buried one**: admission rules forbid durable records for unverified
  values, so if the expiry response is lost the cookie survives and may
  resurface on the old network; every re-presentation re-attempts
  expiry. The residual is bounded to graphless-era cookies from changed
  networks and by the cookie's original absolute expiry — migration never
  refreshes it. Telemetry counts rowless presentations, unverifiable
  issued-version cookies, expiry attempts, and the oldest still-observed
  issuance version. The guide discloses the residual and gives it a sunset:
  the compatibility path may be removed only after no issued graphless-era
  version is observed for a quiet period at least the maximum original
  cookie lifetime plus rollout skew. This acceptance is **sign-off item
  29**.
- **Rowless withdrawal writes into one capped per-prefix record, then
  expires the cookie** — durable (cookie-only expiry is best-effort: a
  lost response leaves the "withdrawn" cookie usable), and **bounded in
  storage**, which separate exact-cookie records were not: a holder of
  one valid prefix can fabricate billions of suffix variants, and
  per-variant records would be attacker-priced strong storage. The
  record (strong class, keyed on the verified prefix) holds a bounded
  list (cap 8) of withdrawn-suffix hashes; writes are admitted only for
  **prefix-verified** cookies. Saturation blocks every further _rowless_
  admission under that prefix and forces cookie expiry, but it is not
  evidence that a row-backed family withdrew. When a real row later
  surfaces, only an exact listed suffix promotes to family revocation;
  the saturated flag alone never revokes or denies an authenticated row
  belonging to another member of the NAT cohort. A re-presented exact
  withdrawn variant stays dead. Row-backed withdrawal remains keyed by
  full-graph-key family ID.
- **Negative-record creation has admission rules everywhere — and
  rowless identifiers get no per-family records at all.** Durable
  suppression and family-revocation records may be written for an
  **existing, row-backed family** (authority-state record present on a
  strong read) — **or for a positively observed real row**: a successful
  row read is safe admission evidence (an eventual not-found is not),
  and without this arm the first post-upgrade GPC request could not
  suppress P4 for an untouched v1 row, and the promotion path could not promote a
  late-surfacing row (neither has a stub by definition). There are **two**
  permission-exempt observed-row sequences, because one shape cannot
  serve both signal classes (the single revocation-shaped sequence
  either destroyed identities for use opt-outs or dropped their
  suppression entirely). **Destructive** (explicit storage-consent
  withdrawal or authenticated deletion): derive the family ID →
  create-if-absent a minimal strong stub carrying no positive authority
  → commit the family revocation → the identity is denied all use between
  discovery and revocation commit → only then expire the browser cookie.
  **Non-destructive** (GPC, SaleOptOut, USP sale opt-out, SharingOptOut,
  TargetedAdvertisingOptOut, malformed, or a TCF refusal that does not
  meet permission spec §4.2's destructive P1 conditions): row read → same stub
  create-if-absent → **CAS the per-permission suppression entry** →
  the suppressed permission is denied use while the sequence is
  incomplete → **the family and the cookie are retained** (nothing is
  revoked or expired). The permission spec's "unconditional" creation
  means **independent of prior positive authority — never independent
  of family admission**: every durable negative write passes one of
  these admission arms, which is what keeps fabricated HMAC suffixes
  from minting unbounded strong records. A rowless identifier — even a _verified_ one — creates
  none: verification authenticates only the prefix, so per-identifier
  records would let one prefix-holder fabricate unlimited suffixes into
  unlimited strong-storage records (rate limits slow creation; they do
  not bound cardinality), and a rowless identity has no authority to
  suppress anyway. **For the old rowless identity, a refusal,
  malformed-present record, GPC, SaleOptOut, USP sale opt-out,
  SharingOptOut, or TargetedAdvertisingOptOut is request-local:** it denies
  the mapped permissions for this request but creates no `s`, `q`, `fam`, or
  `w` state for that rowless family. The `w` class is reserved for explicit
  storage withdrawal or authenticated deletion; non-destructive signals never
  enter it. If P1 permits this request to expire and re-mint through the
  ordinary graph-backed path, the newly created family's authority-state
  commit includes the current suppression before its cookie or identity may
  become usable; a failed suppression write follows the ordinary negative-
  intent/breaker protocol. Durable suppression then belongs to the new
  row-backed family, never the unauthenticated old suffix. If no new family is
  minted, nothing durable exists and a later presentation is reevaluated from
  its current signals. **The capped per-prefix withdrawal record is the
  entirety of old-rowless negative state.** Fabricated, unverifiable cookies
  write nothing anywhere.

**Egress is typed, not policed.** The inventory-and-denylist test
(permission model spec §7) is a backstop, but conventions do not survive
new code — the ungated proxy/click/Testlight paths happened precisely
because raw EC values circulate as ordinary strings. Core therefore
introduces a **scope-parameterized `AuthorizedIdentity<Scope>`**,
constructible only by core, only after the checks _for that exact scope_:
`AuthorizedIdentity<GraphOps>` after parse + `store-on-device` +
family-revocation **and suppression** checks;
`AuthorizedIdentity<PartnerEgress>` additionally after
`select-personalised-ads` — suppression is part of both constructors, not
a separate prose obligation on S2S callers. Outbound serializers (ORTB builder, page
bids, sync, identify, forwarding) accept `AuthorizedIdentity<PartnerEgress>`
and nothing weaker — an unparameterized wrapper would let a P1-only
identity flow into an ORTB request. A future bypass then
requires deliberately reconstructing the raw string — visible in review —
rather than passing along what was already in hand. The same boundary
applies **request-side, as a concrete API transition, not an
assertion**: the current filter/proxy inputs expose the raw request
(cookies included), so a filter can read `ts-ec`, copy it into
`X-Vendor-Identity`, and return it through response effects — response
snapshot redaction cannot undo that. The contract: integration-facing
request access moves to a typed **`RedactedRequestView`** whose stripped
set is enumerated — the `ts-ec` cookie and every `ts-*` cookie, `x-ts-*`
identity/consent headers, the incoming `X-DataDome-ClientID` header (owner-only, hook spec §4a), and the EIDs header — with identity reachable
only through a scoped `AuthorizedIdentity` parameter; the raw-request
filter/proxy interfaces are migrated in the **same PR** as the typed
egress boundary (they are the same boundary), and the tests are
enumerated: a denied/withdrawn request through a filter, a proxy, and a
forwarding path, each asserting no identity value is readable or
emittable.

The gate applies to EC providers **only**. Geo and device are ungated for
two _different_ reasons, stated separately because only one of them is
structural:

- **Geo: circularity.** The permission set is resolved _from_ jurisdiction,
  which is resolved _by_ the geo provider. Gating geo on the resolved set
  is unsatisfiable.
- **Device: host fingerprinting is deferred, not authorized by selection.**
  Device classification is not an input to permission resolution, so a
  separate security/fraud design can order geo → resolution → security
  classification → EC and define its own authority. This epic ships only
  the `builtin` UA-only classifier. Selecting a provider that reads JA4,
  HTTP/2 fingerprints, or comparable host evidence is a startup error until
  a reviewed design defines the exact security purpose, lawful/permission
  basis, retention, deletion, downstream visibility, and an explicit ban on
  advertising or identity reuse. Existing fingerprint-derived graph fields
  remain read-only for migration, never egress, and are scrubbed on rewrite;
  new rows persist neither raw fingerprints nor a fingerprint-derived
  classification outcome.

PR #838 declared `required_permissions` on all three traits but consulted
it only for the EC provider; the geo and device declarations were
decorative — worse than absent, because they read as a gate and are not
one. This spec resolves that by **not having** the method on those traits
(§4), with the two rationales above in place of the pretense.

## 6. Selection, validation, and failure modes

All validation happens at **settings construction** — a misconfiguration is a
startup error, never a request-time error and never a silent behavior change.

| Configuration state                                                                                                                                  | Behavior                                                                                                                                                                                                                                                           |
| ---------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `provider` names an unknown key                                                                                                                      | Startup error listing valid keys.                                                                                                                                                                                                                                  |
| `provider` set, its `[ec.providers.<key>]` block missing                                                                                             | Startup error.                                                                                                                                                                                                                                                     |
| `[ec.providers.<key>]` block present, `provider` unset                                                                                               | **Startup error.** (In PR #838 this silently ran stateless — the half-migrated config becomes a production identity outage detected by revenue drop. Rejecting it is the fix.) An operator who genuinely wants stateless deletes the block.                        |
| `provider` set to an implementation the running adapter cannot satisfy (e.g. a provider requiring host TLS fingerprints on an adapter that has none) | Startup error at adapter wiring time. Adapters declare their host capabilities to the composition root; the root checks the selected provider's needs against them **once**, at startup — not per request.                                                         |
| A `[ec.providers.<key>]` block referenced by neither `provider`, `legacy_providers`, nor a `versions`/`mint_version` chain                           | **Startup error.** An unreferenced block is almost always a dropped `legacy_providers` entry — accepted silently, it strands every identity that provider minted: unresolvable and, worse, non-withdrawable.                                                       |
| No `provider`, no providers block                                                                                                                    | Valid: the neutral default for that concern.                                                                                                                                                                                                                       |
| `rewrite_legacy` present at all (deferred out of the epic, §6.1)                                                                                     | **Startup error** — unknown key; transparent re-mint returns only with its own spec.                                                                                                                                                                               |
| `provider = "none"` (explicit stateless)                                                                                                             | Valid, and the only way to combine statelessness with `legacy_providers`: minting stops, legacy readers keep existing identities resolvable and **withdrawable** (§6.1). Without this state, `hmac` → stateless would strand every live row in revoke-proof limbo. |
| A minting provider (or any `legacy_providers`) configured, but no identity-graph store configured or openable                                        | **Startup error.** The lifecycle contract assumes graph persistence (§5); discovering its absence at first mint would be a request-time config failure, which this table exists to forbid.                                                                         |

Unknown fields inside every provider config block are rejected
(`deny_unknown_fields` on all new settings structs — the pre-existing `Ec`
struct already has it, but PR #838 shipped `EcProviders`, `DeviceConfig`,
and `GeoConfig` without it, so a typo like `providr` was silently ignored).

### 6.1 Provider switching: active writer, legacy readers

Switching `[ec] provider` must not strand the identities the previous
provider minted: with only the selected provider recognizing cookies, an
`hmac` → vendor switch turns every existing cookie into "absent", orphans
its graph row, and — worst — makes a later opt-out unable to tombstone it.
The contract:

- `[ec] provider` names the **active writer**: the only provider that
  mints.
- `[ec] legacy_providers = ["hmac"]` (optional list) names **legacy
  readers**: providers consulted, in order, for parse, graph lookup, and
  tombstoning when the active writer does not recognize a value. Legacy
  readers never mint. Each listed key must have its `[ec.providers.<key>]`
  block, validated like the active one (§6 table).
- **Parse order and ambiguity.** The active writer parses first; the first
  match wins. Overlapping recognition is not resolved at request time but
  **forbidden at startup**: the declared namespace descriptors (§3) of the
  active writer and every legacy reader must be pairwise disjoint — a
  check core can actually perform, because descriptors are declarative;
  configuring a pair whose descriptors intersect is a validation error.
- **The recognizing provider governs.** A legacy-owned identity is gated
  by the **legacy provider's** `required_permissions()` for identity use —
  the provider that minted under a declared data-use contract is the one
  whose contract applies.
- **Provenance is provider- and version-tagged, with a defined rotation
  schema.** Every graph row carries the minting provider id, its
  configuration version, and the per-permission grant evidence (grant
  basis, evidence timestamp, resolved jurisdiction, policy revision)
  **mirrored for audit only** — the S2S sync authority recomputes from
  the strong authority summary alone, never from row provenance
  (permission model spec §7; the §6.3 authority-state wire record is
  the one normative summary schema).
  Same-provider key/passphrase rotation is configuration, not a provider
  switch: a provider block may hold multiple `versions` entries
  (`[ec.providers.hmac.versions.v2] passphrase = …`) with
  `mint_version = "v2"` selecting the writer. **`parse` cannot identify a
  version** — every HMAC version shares one grammar and `parse` returns no
  version — so the mint version lives in **immutable row provenance**
  (rows without a tag are `hmac-v0`), and cryptographic verification
  consults configured versions newest-first only where provenance is
  unavailable (a cookie with no reachable row). Removing a version entry
  is a retirement subject to the same evidence rules as retiring a legacy
  reader (migration spec §6).
- **`rewrite_legacy` is deferred out of the epic.** Transparent re-mint
  under the active writer required primitives no production adapter has
  (row-store CAS with read-your-writes plus a linearizable transaction
  class — §7 matrix), and successive reviews kept surfacing open protocol
  problems: retention lineage (a rewritten 364-day-old row either
  rejuvenates the identity or leaves a year-long cookie pointing at an
  expiring row — a lineage expiry must be pinned across canonical row,
  alias, family record, and emitted cookie), alias visibility under
  eventual stores, chain stranding after repeated migrations, and
  cluster-count inflation by alias keys. Those are recorded here as the
  entry bar for a future `rewrite_legacy` spec. Within the epic, provider
  switching is served by **legacy readers alone**: old identities keep
  resolving and stay withdrawable; they are never transparently
  re-minted. The `rewrite_legacy` key is rejected at startup as unknown,
  and the alias record class exists in the key grammar (§6.3) only as
  reserved-for-future — nothing in the epic writes one.
- Retiring a legacy reader is the explicit end of those identities:
  the migration guide documents the cleanup procedure (migration spec
  §6). **Provenance backfill is not retirement evidence** — a backfilled
  row still lives under the legacy cookie namespace and still needs that
  provider's parser; only a quiet period spanning the full cookie/row
  lifetime justifies removal.
- Tests: switch active provider → request with old cookie → identity
  still resolves and a withdrawal tombstones it; old cookie with no
  matching legacy reader → treated as absent and **never egresses**;
  `provider = "none"` + legacy reader → no mints, withdrawal still
  works. (Rewrite-specific tests left with the rewrite deferral.)

Cluster degradation config (referenced from §3): when the active writer
lacks the cluster capability, `[ec] cluster_fallback = "allow" | "deny"`
decides whether KV-backed writes gated on cluster trust proceed; there is
no implicit default — the operator chooses.

### 6.2 Runtime failure matrix — normative

Startup validation (§6) covers configuration; this covers what happens
when a healthy configuration meets an unhealthy runtime. Every row logs at
`error` with a metric; none is silent:

| Failure                                                                                                                             | Behavior                                                                                                                                                                                                                                                                                              |
| ----------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `generate` returns an error                                                                                                         | No identity this request; request proceeds stateless; no cookie written                                                                                                                                                                                                                               |
| Graph-row commit fails at mint                                                                                                      | Mint never happened (§5): no cookie, no egress; next request retries                                                                                                                                                                                                                                  |
| Authority-state commit fails after the row commit at mint                                                                           | No cookie, no eligibility (the strong record never reported the revision); the orphan row expires by TTL; counted — **no recovery claim**, nothing can find it                                                                                                                                        |
| `w` read fails (rowless path, or any HMAC row discovery while a live `w` record exists — enforcement outlives the migration window) | Fail closed: the cookie/row is **indeterminate** — no use, no mint, no expiry this request                                                                                                                                                                                                            |
| `w` write fails at rowless withdrawal                                                                                               | Cookie retained (withdrawal did not durably occur); durable client signal retries next presentation                                                                                                                                                                                                   |
| `w` saturation encountered, a real row surfaces under the prefix                                                                    | Exact listed suffix: denied and promoted. No exact match: saturation alone is not withdrawal evidence; the authenticated row proceeds through its own family revocation and suppression checks. No NAT-cohort collateral revocation.                                                                  |
| Promotion (listed suffix-hash matches a discovered row) fails                                                                       | The row is denied all use until the promoted family revocation commits; retried on next observation                                                                                                                                                                                                   |
| Graph read fails on an existing identity                                                                                            | Identity unusable this request (fail closed for egress); cookie untouched                                                                                                                                                                                                                             |
| Cluster prefix listing fails                                                                                                        | Treated as cluster-size-unknown → `cluster_fallback` policy applies                                                                                                                                                                                                                                   |
| Revocation or suppression write fails                                                                                               | Enqueue the idempotent negative intent in the durable outbox. Until its strong target record commits, every identity decision observes the intent and applies its negative scope. If outbox enqueue also fails, trip the globally visible identity safety breaker immediately (permission spec §4.3). |
| Geo provider returns invalid output (unparseable country) at runtime                                                                | Treated as lookup failure → protective failure profile (permission model spec §5.2), counted in the lookup-failure metric                                                                                                                                                                             |
| Host-fingerprint device provider selected                                                                                           | Startup error in this epic; only the builtin UA-only provider is eligible (§5)                                                                                                                                                                                                                        |

The **degraded-graph health signal** for ordinary row availability is a
defined per-instance state machine, entered when graph-write failures cross
a sliding-window threshold and exited with hysteresis. It is not the
negative-intent safety mechanism: revocation and suppression failures use
the durable outbox and globally visible identity breaker above. While ordinary
graph health is degraded, S2S partner egress and sync updates fail closed;
organic requests continue stateless.
The thresholds ship as constants with the implementation and are printed
in the startup log.

The ordinary health signal is local-only by design. Negative intent is not:
the outbox is durable, workers retry to the strong record, and a failed
enqueue trips a fleet-visible breaker. A deployment that cannot provide or
read this state is ineligible for persisted identity use; no
never-returning-visitor residual is accepted.

### 6.3 Storage contract — normative

**Physical key grammar.** Core constructs every key; providers supply only
the bounded suffix:

| Record class                                                                           | Key                                                                                                                                                                                                             | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| -------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Identity row (non-hmac)                                                                | `id/<provider>/<suffix>`                                                                                                                                                                                        | Suffix from the provider's declared `injective` or `sha256-detect` graph-key mode, ≤ **123** bytes (128-byte total key cap minus tag and provider code; 123/124 boundary fixtures required), portable alphabet **`[A-Za-z0-9._~-]`**. Native identifiers outside that representable space use the assigned collision-detecting form above rather than an unqualified claim of collision-free base64url encoding. **No version segment** — the mint version lives in the row envelope; a versioned key would be circular (core would need the version to read the row that states the version)                                                                                                                                                                                                                                                       |
| Identity row (hmac, **every** version)                                                 | The identifier verbatim (`{64hex}.{6alnum}`)                                                                                                                                                                    | Reserved grammar for the whole provider, not just v0 — keeping all HMAC versions on the verbatim scheme is what keeps the 64-hex cluster prefix a literal key prefix for every HMAC row. (Passphrase rotation still changes a given IP's HMAC, so clusters split across rotation until old identities expire — inherent to rotation, declared, not a key-scheme artifact)                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Alias                                                                                  | **The source identity key itself** — the alias is a value written _at_ the replaced row's address, distinguished by a `kind` discriminator in the JSON envelope                                                 | A separate `alias/…` address could never work: lookups by the old cookie hit the source key, and a single-key CAS cannot install a record at a different address                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| Family revocation                                                                      | `fam/<family-id>`                                                                                                                                                                                               | Family ID from mint or the deterministic legacy derivation (permission spec §4.3)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| Authority-state (suppression + positive-authority summary)                             | `s` + family ID (fixed-width grammar)                                                                                                                                                                           | Per-permission negative entries **and** the positive summary (permission spec §4.3); permission-exempt writes; consulted by every constructor and S2S recompute                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| Negative-intent outbox                                                                 | `q` + family ID (fixed-width grammar)                                                                                                                                                                           | One strong CAS record per family in a failure domain independent of the authority/revocation target. It holds the permission spec §4.3 queue schema and idempotent pending negative transitions. Every live, cached, and S2S identity decision checks it before positive use; a pending intent denies its mapped scope until applied. Workers commit the target before CAS-acknowledging the exact intent.                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| Rowless prefix-withdrawal record                                                       | `w` + provider code + 64-hex prefix                                                                                                                                                                             | Strong class, **linearizable CAS**; bounded exact suffix-hash list (cap 8), each with `valid_until` = withdrawal time + max(cookie, row, S2S horizon), plus a saturation flag that blocks only further rowless admission. The record expires after its last exact entry and saturation migration window. Only an exact suffix match can promote to family revocation; the prefix flag never authorizes cohort-wide row revocation. Read fail-closed for rowless classification and exact-match enforcement.                                                                                                                                                                                                                                                                                                                                         |
| Deployment metadata                                                                    | `m` + fixed metadata name (fixed-width grammar): schema-floor compatibility mirror, graphless-migration flag, policy/config/model-activation register, config-sequence register, global identity safety breaker | Strong-read/CAS class outside rollbackable config. The authoritative row-schema floor and binary/model epoch live in the activation register and advance in its fleet-fenced model CAS; `m00` is only a monotonic startup mirror and never enables writes. The register also carries immutable active/settings-candidate/model-candidate bindings, authoritative fleet readiness and quiescence, the activation-journal head, and a 16-entry operational history; the journal supplies the independent time-based audit/GC horizon. The config-sequence register allocates envelope `push_sequence` independently of ordinary config-store `put`; the global breaker disables every positive identity operation when neither a target negative record nor its fault-isolated outbox intent can commit, while leaving repair/deletion paths enabled. |
| Rewrite transaction _(informative — deferred)_                                         | **No physical key allocated in this epic**                                                                                                                                                                      | A revived rewrite design must allocate a delimiter-free class in this registry; the old `rwx/<family-id>` sketch is not valid grammar                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Replay reservation _(informative — deferred with client-cycle; not normative surface)_ | **No physical key allocated in this epic**                                                                                                                                                                      | A revived client-cycle design must allocate a delimiter-free class in this registry; the old `resv/…` sketch is not valid grammar                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |

`m00` completion is part of the metadata wire contract, not an operator
overwrite: after the authoritative model CAS, the authenticated deployment
controller strong-reads active and `m00`; a missing or lower mirror is CAS-set
to exactly `active.row_schema_floor`, equality is an idempotent no-op, and an
unreadable mirror or failed CAS/read-verification remains closed for retry.
The controller then strong-reads and verifies exact equality. The operation
never lowers the mirror and never authorizes or changes active;
`m00 > active.row_schema_floor` is rejected before any write as a fail-closed
register/journal inconsistency, not an automatic repair case.

Grammars are pairwise non-intersecting by their literal prefixes (plus
the reserved hmac grammar), and every record value carries a `kind`
discriminator alongside its schema version — so a reader always knows
what it fetched, including where two classes deliberately share an
address (row vs. alias). The `/` shown in key sketches is **notation,
not the wire byte** — and the wire form is **one portable grammar, not
per-adapter delimiters** (per-adapter delimiters would give the same
logical identity different physical keys on different adapters, breaking
migration, shared storage, and parity; and Fastly's prefix queries
reject both `/` and `:`, so no delimiter character is safely portable).
Physical keys are **delimiter-free with fixed-width segments**: a
1-character class tag — `i` row, `r` family revocation, `s`
authority-state, `q` negative-intent outbox, `w` rowless prefix-withdrawal,
`m` deployment metadata (the complete normative enumeration for this epic), every
tag chosen **outside the hex alphabet** so no
generated key can begin with 64 hex characters, which is what makes
disjointness from the legacy `{64hex}.{6alnum}` grammar _provable_
rather than asserted (an earlier `f` tag was itself a hex digit) — then
a **4-character provider code from the checked-in, append-only,
never-reused registry `docs/superpowers/specs/provider-code-registry.md`** (allocation is a reviewed commit;
codes are immutable and never recycled, including for retired
providers), then the suffix. The **complete physical constructor set** (logical
`fam/`-style sketches elsewhere are notation for these): `i` +
provider-code(4) + suffix(≤123 — the 128-byte total minus tag and code;
an earlier "suffix ≤128" was inconsistent with its own cap) for rows; `r`/`s` + family-id(64
lowercase hex — family IDs are canonically
SHA-256 over a **fully assigned derivation input**: the domain tag
`tsfam1|`, then the record-kind byte — **assigned: the class tag's
ASCII byte** (`i` = 0x69 for identity-derived families) — then the
provider code (4 bytes), then the canonical graph-key bytes,
concatenated in that order, no separators beyond the tag's `|`.
**Known-answer vector**: input `tsfam1|` + `i` + `hmac` +
`{64×"a"}.AbC123` →
`e90616c381f64965b8326f17108c3c481cee932b2d7f8af783c7bdc2e21591ef`.
A **non-HMAC vector** disambiguates "canonical graph-key bytes" =
the provider's `graph_key_suffix` bytes (not the full physical key —
the tag and provider code are already separate derivation fields):
input `tsfam1|` + `i` + `vend` + `abcdef` →
`278e67d721babaee94690cd246ee567d6ce709c43f8737c2e9dce1e1119c6be1`.
`w` suffix hashes are likewise assigned: SHA-256 with domain tag
`tswsx1|` over the raw suffix bytes, truncated to 16 bytes, lowercase
hex (32 chars) — vector: `AbC123` → `08cb55acf42929772862e82b0960c134`.
Cross-language vector suites extend these) for revocation/authority records
(no provider code: the family id already encodes derivation); `q` +
family-id(64) for the negative-intent outbox; `w` + provider-code(4) +
prefix(64 hex) for rowless withdrawal; `m` + a **2-digit
registry-assigned index** for deployment metadata (a closed name
registry in this spec: `00` schema-floor compatibility mirror, `01` graphless-migration
flag, `02` policy/config/model-activation register, `03` config-sequence allocator,
`04` global identity safety breaker — padding-based names aliased `foo` and `foo-`, so names are not
encoded in keys at all). Maximum
physical key length **128 bytes**; every class has a total parser, and
segment boundaries are positional, so no segment can contain or escape a
delimiter, prefix queries are plain string prefixes on every backend,
and a grammar-disjointness test covers every class pair plus the legacy
grammar. (hmac verbatim keys remain the
reserved exception, with the 64-hex cluster prefix at position zero.)

**Wire schemas** (JSON, like identity rows; every class carries a schema
version): the **alias record** (reserved-future, with rewrite) holds target key, created-at, retirement
deadline, and fencing epoch; the **family revocation record** holds the
family ID, revoked-at, triggering withdrawal class (explicit storage
withdrawal, authenticated deletion, or qualifying TCF Purpose-1 refusal),
and a **family epoch** bumped on every revocation-state change (the
client-cycle commit CAS is conditioned on it) — deliberately no identity
data, so it can outlive its members; the **authority-state record** holds, per permission — negative side:
state (`suppressed`/`cleared`), cause, source class, authoritative or
observation evidence timestamp, optional entry `valid_until` (required for
refusal/malformed/absence; absent for a persistent use opt-out), and the
provenance revision plus explicit newer-authorization evidence a clear
references. Each permission also carries a monotonic
`authorization_revision: u64`: a persistent opt-out records the current value
as its clearing floor; only an authenticated same-subject authorization CAS
may increment it, and a clear based on that channel requires a value strictly
above the floor. Overflow is a hard error. Ordinary GPP/USP/GPC presentation
never increments this field. Positive side (the summary, **every field the permission
spec's absence/replay decisions consume — a reduced schema cannot
reproduce them**): kind (user evidence vs policy baseline), grant
basis/source class, policy revision (the §5.5 pair: digest + activation
ordinal), and **tagged jurisdiction provenance** — exactly one of
`Live { jurisdiction, provider_id, observed_at }`,
`StaticDefault { jurisdiction, config_revision, observed_at }`, or
`ProtectiveLookupFailure { provider_id, profile_revision, observed_at }`.
The first comes from successful live geo, the second only from acknowledged
static mode, and the third carries no jurisdiction and is never eligible for
context-free S2S egress. `observed_at` is written by the browser-request
resolution that produced the tag, never derived from evidence timestamps (TCF
`LastUpdated` can predate it, and a policy-baseline grant has no wall-clock
evidence time at all). S2S therefore never reads jurisdiction from an
eventually stale row; static authority is fenced to its exact active config,
lookup failure is structurally non-authorizing for S2S, and decision 25's
stored-jurisdiction age gate has a field that actually measures jurisdiction
age (the summary is self-sufficient for the recompute) —
`valid_until`, provenance revision, evidence timestamp, and a **bounded replay history** whose slots are keyed by a
**timestamp-independent `state_key`** — (source class, semantic result
digest _excluding_ `LastUpdated`) — distinct from the _evidence
digest_ (which for TCF includes `LastUpdated` for recency), both with
**canonical wire construction — the hash input is the lowercase ASCII
source token, not an enum byte** (an earlier draft said "enum bytes:
tcf=1…" while its own vector hashed the ASCII token; the vector was
right, the prose wrong — enum bytes exist only in storage, never in hash
input): `state_key` = SHA-256 `tsstk1|` + `<source-token>` (`tcf` /
`gpp` / `usp` / `gpc`) + `|` + canonical semantic result covering **all enforced
permissions for that source** (slots are **per source**, not per
permission·source — the result string carries every permission, as the
vector shows; timestamps integer epoch-ms where present); evidence
digest = SHA-256 `tsevd1|` over the same, **plus `|lu=<LastUpdated>`
only for sources with an intrinsic authoritative timestamp (TCF);
timestamp-less sources (GPP, USP, and GPC) omit the `|lu=` field entirely —
omission is the canonical form, no sentinel value exists**.
Vectors: `tsstk1|tcf|p1=grant,p4=refuse` →
`a49148a0e3b486fd93a141404857868871319a7e1f2ef85b5499aed80c7e59df`;
`tsevd1|tcf|p1=grant,p4=refuse|lu=1690000000000` →
`67259c0247ae2b33c52d9f18193bcd622f48ad4754ffbab6998d7c293b0143b4`;
timestamp-less form `tsevd1|gpp|p1=grant,p4=refuse` →
`89b08580c214070b6d1d58ad57c12bd585134f324c718cbc0802b4d82a0d72e6`;
GPC vectors `tsstk1|gpc|p4=opt-out` →
`3a0ff99076bfa849edd18da99018e1993251d128c322a1d73857f6057eb3fe96`
and `tsevd1|gpc|p4=opt-out` →
`a5df50001cd9ef127365a18754e96d43527e2c3f34e94bcd03e04e0a1f65b28c`:
keying slots on the
evidence digest would give every renewal a fresh key and make
"updates its slot in place" impossible, the incompatibility an earlier
draft shipped. Replay transition is **source-specific**, because TCF has
an authoritative clock and GPP/USP/GPC do not:

- **TCF:** the record stores a per-source `max_last_updated`. A value with
  `LastUpdated <= max_last_updated` is replay and changes nothing, even on
  A→B→A. A value with a strictly newer valid `LastUpdated` is genuine newer
  evidence: it updates or returns to the semantic `state_key` slot, replaces
  that slot's evidence digest, and refreshes only the permissions represented
  by the new evidence. Same semantics plus newer `LastUpdated` is a valid CMP
  renewal. No first-seen timestamp participates in TCF authority age.
- **GPP/USP/GPC:** a slot stores `state_key`, evidence digest, and a bounded
  per-permission `observed_at` map. The record stores `current_state_key`.
  For a new vector V, unchanged permission tokens copy `observed_at` from
  the current slot; a changed token reuses the earliest live-slot
  `observed_at` for the same `(permission, token)`, else uses the current
  observation. The target slot's map is replaced, never merged. Thus
  A→B→A, grant→refusal→same-grant, and equivalent encodings do not refresh
  timestamp-less authority; a P4-only change leaves P1 age untouched.
  `observed_at` orders acquisition and non-user suppression recovery only; it
  never proves that a bare timestamp-less grant is new enough to clear a
  persistent use opt-out. That clear needs TCF's authoritative
  `LastUpdated` or the authenticated `authorization_revision` path above.
  After such a clear, presentation of the same restrictive GPP/USP/GPC value
  starts a new suppression episode: it keeps the evidence's original
  `observed_at`, records the current authorization revision as the new clearing
  floor, and therefore remains effective for later no-signal/S2S decisions.
  Restrictive presentation can never clear any state.

Both branches update `current_state_key` inside the authority record's one
CAS. Named vectors cover TCF same-state renewal, TCF stale A→B→A replay,
GPP/USP/GPC A→B→A, equivalent encoding, grant→refusal→same-grant, and P4-only
change, plus persistent opt-out → later bare not-opted-out (does not clear) →
authenticated authorization-revision increment (clears) → identical
timestamp-less opt-out (new restrictive episode) → no-signal/S2S remains
suppressed.

There are 16 slots per source. Saturation is restrictive without creating
collateral withdrawal:

1. Expired slots are reclaimed first; otherwise grant-class slots are
   eviction candidates before refusal or opt-out slots.
2. A novel grant with no eligible slot cannot authorize and is rejected
   fail-restrictive. Repetition cannot create more slots.
3. A restrictive overflow updates one bounded per-source,
   per-permission marker. A valid use opt-out marker has no time-based
   expiry and clears only on strictly newer explicit authorization or
   identity deletion. A refusal/malformed marker receives its complete
   evidence horizon from the new observation; it never inherits an older
   marker's nearly expired clock.
4. Saturation is a first-class metric and rate-abuse signal. It never turns
   one source's history pressure into revocation of another family, and it
   never shortens a newly observed opt-out.

The **negative-intent outbox record** is the other strong wire schema. Its only
physical constructor is `q` + family-id(64 lowercase hex), exactly 65 bytes;
the parser rejects every other length, alphabet, or family-id case. It holds
`schema_version`, family ID, monotonic `queue_revision`, and at most 32 entries
sorted by lowercase 64-hex `intent_id`. Each entry stores the canonical target
key, the canonical transition payload below, and `enqueued_at_unix_ms`. The
payload materializes these keys, including nullable values, before hashing:
`cause` (closed string token), `clearing_floor_authorization_revision` (integer
or null), `evidence_digest` (64 lowercase hex), `evidence_time_unix_ms`
(integer), `permission` (permission ID or null), `source_class` (closed string
token), `state` (`revoked` or `suppressed`), `transition_kind`
(`family_revocation` or `suppression_strengthen`), and
`valid_until_unix_ms` (integer or null). Integers are in `0..=2^53-1`, never
strings or floating-point values.
The closed `source_class` tokens are `tcf`, `gpp`, `usp`, `gpc`, `absence`,
`storage-withdrawal`, and `authenticated-deletion`. The closed cause tokens are
`tcf-refusal`, `gpc`, `sale-opt-out`, `sharing-opt-out`,
`targeted-advertising-opt-out`, `malformed-present`, `absence`,
`explicit-storage-withdrawal`, `authenticated-deletion`, and
`tcf-purpose-1-withdrawal`. Signal evidence uses the authority record's
canonical `tsevd1|` digest. Absence uses
`SHA-256("tsevd1|absence|" || permission-id || "=absence")`; explicit
withdrawal/deletion uses SHA-256 over `tsneg1|`, the source-class token, `|`,
and the canonical authenticated audit-event bytes defined by that endpoint.
Raw action tokens, subjects, and credentials never enter the outbox.
`permission` and `clearing_floor_authorization_revision` are non-null for
`suppression_strengthen` and null for `family_revocation`;
`valid_until_unix_ms` is null only for a non-expiring transition. Enqueue time,
target key, family ID, and queue metadata are deliberately outside the payload
and therefore outside transition identity. `intent_id` is SHA-256 over the UTF-8
bytes `tsq1|`, the fixed-width canonical target-key bytes, and RFC 8785 JSON of
that complete materialized payload, in that order with no added separator.
Cross-field validation is total: `family_revocation` requires target
`r` + this outbox's family ID, state `revoked`, null permission/floor, and one
of the three withdrawal causes with its matching source class;
`suppression_strengthen` requires target `s` + this family ID, state
`suppressed`, a vocabulary permission, a present floor, and a compatible
signal/malformed/absence cause. Any other target family, tag, nullability,
state/transition pair, or cause/source pair is malformed and denies the family.

Known-answer vector representing a suppression after authorization revision 7:
target key `s` + 64 zeroes and payload
`{"cause":"gpc","clearing_floor_authorization_revision":7,"evidence_digest":"0000000000000000000000000000000000000000000000000000000000000000","evidence_time_unix_ms":1785369600000,"permission":"select-personalised-ads","source_class":"gpc","state":"suppressed","transition_kind":"suppression_strengthen","valid_until_unix_ms":null}`
produce
`cafa2fadf3625ab0e6a0108224e97955b8230dc42dd589948c6ac2caf7eabbb0`.
Enqueue merge, absorbing revocation, exact-revision acknowledgment,
retention, capacity overflow, and failure-domain behavior are normative in the
permission spec §4.3. Unknown fields/schema, unsorted/duplicate ids, a payload
whose recomputed id differs, or revision regression fail closed; none is
skipped as “best effort.”

The replay cap and fail-restrictive behavior are product sign-off item 31; authority record level: family ID,
CAS version counter, schema version, stub marker (backfill, §5); unknown-field and range validation apply
like every class (strong class, permission-exempt writes per the
permission spec's inventory; revisions are app-level counters because
backend generation markers detect change without ordering);
the **rewrite transaction** _(informative — deferred with rewrite)_
holds source key, target key, copy point, state, and epoch; the **reservation** _(informative — deferred with client-cycle)_ holds
state, owner hash, lease epoch, outcome, and created-at. Field validation and
TTLs: aliases live to their retirement deadline; family records to the
§7 retention rule (beyond every member, cookie, rewrite, and retry
lifetime); transactions to completion plus an audit window; reservations
at least through token expiry.

#### Graph row contract

The per-field contract for identity rows, covering today's v1 fields and
the fields this epic adds. Serialization is JSON with the existing `v`
schema-version discriminator; from release N+1 onward (migration spec §4),
readers round-trip unknown keys **semantically** (values preserved through
read-modify-write; byte-identical output is not required and not
achievable through a structured serializer).

| Field                                                                                                                                                                                                                                   | Purpose                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | Source                                                  | Gating permission (egress)                                                        | TTL / refresh                                                                      | Rewrite                                                   | On revocation                                                                                                           |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------- | --------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- | --------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| key (v1: identifier verbatim; v2: core-constructed, §4)                                                                                                                                                                                 | Row identity                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Provider/core                                           | —                                                                                 | Row TTL (1 y today)                                                                | New canonical row; old key becomes alias                  | Family record governs; member tombstone as cleanup                                                                      |
| `v`                                                                                                                                                                                                                                     | Schema discriminator                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Core                                                    | —                                                                                 | —                                                                                  | Written at current version                                | Retained                                                                                                                |
| New: `graph_key_canonical_identifier`                                                                                                                                                                                                   | Collision witness required **only** for `sha256-detect` graph-key mode. Exact JSON string containing the provider-canonical identifier's ASCII bytes (global ≤256-byte alphabet); forbidden in `injective` and legacy-HMAC rows. Before any row read, merge, CAS update, alias action, graph use, or partner use, core compares length and bytes against the request's canonical identifier using the shared constant-time comparator. Mismatch is the §2 collision outcome: fail closed, never overwrite/join, trip the fleet-visible security alert. | Provider canonicalization at mint                       | Never egressed; identity-bearing row field                                        | Immutable for the complete row/tombstone lifetime                                  | Preserved byte-for-byte; N+1 knows and enforces the field | Retained in tombstone; family revocation remains authoritative after row cleanup                                        |
| `created` / **`expires_at`**                                                                                                                                                                                                            | Row age and the **absolute retention deadline, pinned at mint** — every update writes with the _remaining_ lifetime, never a fresh full TTL (today's full-TTL rewrite lets a frequently visited identity live forever; refreshable derived state must never rejuvenate the identity)                                                                                                                                                                                                                                                                   | Core                                                    | P1 (first-party ops)                                                              | Never extended                                                                     | Preserved (no rejuvenation)                               | Retained in tombstone                                                                                                   |
| `consent.tcf` / `consent.gpp`                                                                                                                                                                                                           | **Legacy-only raw snapshots.** New rows do not store them; a live resolution stores normalized per-permission provenance and a domain-separated evidence digest. A separately approved audit store, if deployed, is encrypted, access-controlled, and retention-bounded outside the identity row.                                                                                                                                                                                                                                                      | Legacy request data                                     | Never egressed from the row                                                       | Read-only until scrubbed; no new writes                                            | **Dropped**                                               | Scrubbed                                                                                                                |
| `consent.ok` / `consent.updated`                                                                                                                                                                                                        | v1 liveness flag — **superseded by the family revocation record**; written during member cleanup for v1-reader benefit through the transition                                                                                                                                                                                                                                                                                                                                                                                                          | Core                                                    | —                                                                                 | —                                                                                  | Fresh                                                     | `ok = false` written as cleanup; the family record is authoritative (v1's 24 h tombstone TTL does not bound revocation) |
| New: **immutable mint tag** (`mint_provider`, `mint_version`)                                                                                                                                                                           | Credential retirement and audit — write-once at mint (legacy backfill may populate a missing tag once); **never part of the replaceable snapshot**, or a v1 identity revisited after rotation would be restamped v2                                                                                                                                                                                                                                                                                                                                    | Mint (or one-time backfill)                             | —                                                                                 | Immutable                                                                          | —                                                         | Retained                                                                                                                |
| New: **provenance revision** (application-level monotonic counter, u64, initialized at 1, incremented by every provenance-bearing write, CAS'd with the row generation, serialized as an integer; overflow is a hard error, not a wrap) | Orders clears vs. snapshots (permission spec §4.3)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | Core                                                    | Read by S2S/clears                                                                | Monotonic                                                                          | —                                                         | Retained                                                                                                                |
| New: per-permission provenance (grant basis, authoritative timestamp, `valid_until`, jurisdiction, policy revision)                                                                                                                     | **Audit mirror only — never the S2S decision input**: the strong summary carries every decision field (jurisdiction included); the row supplies identity/partner data only after the exact revision fence                                                                                                                                                                                                                                                                                                                                              | Live resolution only                                    | Not read for gating — audit and cleanup only (S2S reads the strong summary alone) | `valid_until` per evidence class; replaced atomically, never merged                | **Fresh live resolution** — never copied                  | Scrubbed                                                                                                                |
| New: `family_id`                                                                                                                                                                                                                        | Revocation discovery                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Core at mint (derived for legacy, permission spec §4.3) | —                                                                                 | Immutable                                                                          | Shared across linked rows                                 | Is the revocation key                                                                                                   |
| `geo.country` / `geo.region`                                                                                                                                                                                                            | Jurisdiction snapshot                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Geo provider at mint                                    | P1                                                                                | Written at mint                                                                    | Fresh                                                     | Scrubbed                                                                                                                |
| `geo.asn` / `geo.dma`                                                                                                                                                                                                                   | Cluster disambiguation / market signal                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | Platform at mint                                        | P1; DMA additionally P4 for bidstream use                                         | Written at mint                                                                    | Fresh                                                     | Scrubbed                                                                                                                |
| `pub_properties` (origin/seen domains)                                                                                                                                                                                                  | Creation context                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Core at mint                                            | P1                                                                                | Write-once                                                                         | Preserved                                                 | Scrubbed                                                                                                                |
| `device.*` (JA4 class, H2 hash, quality metadata)                                                                                                                                                                                       | **Discontinued for new rows** (§5): fingerprint-derived, buyer-facing — beyond security-classification authorization. v1 rows retain them read-only; they are never egressed post-epic and are dropped at rewrite                                                                                                                                                                                                                                                                                                                                      | Fastly device provider                                  | None grants egress                                                                | Write-once (v1)                                                                    | **Dropped**                                               | Scrubbed                                                                                                                |
| New: security classification outcome (boolean)                                                                                                                                                                                          | Deferred with host fingerprinting; absent in this epic                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | —                                                       | —                                                                                 | Not written                                                                        | —                                                         | —                                                                                                                       |
| `network.*` (immutable evidence: ASN etc.)                                                                                                                                                                                              | Cluster disambiguation                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | Platform at mint                                        | P1                                                                                | Write-once                                                                         | Fresh                                                     | Scrubbed                                                                                                                |
| Derived cluster state (`cluster_size`, computed-at)                                                                                                                                                                                     | Trust gating                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Computed                                                | —                                                                                 | **Refreshable, short validity; generation-CAS update; never touches `expires_at`** | Recomputed                                                | Scrubbed                                                                                                                |
| `ids` (partner → UID map)                                                                                                                                                                                                               | Partner identity graph                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | Pixel/pull/batch sync                                   | P1 ∧ P4 (partner egress)                                                          | Per-mapping timestamps; bounded count/length                                       | Copied **with original timestamps/expiry**                | Scrubbed                                                                                                                |
| New: alias record kind                                                                                                                                                                                                                  | Rewrite indirection (§6.1)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | Core                                                    | —                                                                                 | Retirement deadline                                                                | Is the mechanism                                          | Family-revoked like any member                                                                                          |

For `sha256-detect`, create-if-absent stores
`graph_key_canonical_identifier` in the same atomic row value as `v`; a
pre-existing value must match before a generation CAS is even attempted. A
missing/invalid witness in that mode, or a witness present in `injective`/HMAC
mode, is malformed row state and fails closed. N+1's reader/preserver release
parses and enforces this field rather than relying on generic unknown-field
round-tripping, which is why an N+2-created collision witness remains effective
during pre-promotion rollback.

## 7. Composition root and adapter parity

Provider construction happens in exactly one place per concern
(`build_ec_provider`, `build_device_provider`, `build_geo_provider`), called
by **every** adapter. No adapter may wire a concrete implementation directly:
in PR #838 the Cloudflare adapter installed its host geo unconditionally,
so identical configuration produced different jurisdictions on different
adapters — which the permission model then turned into different privacy
outcomes.

Requirements:

- **Adapters declare capabilities against an explicit matrix — with
  consistency semantics, not just feature bits.** The capability set:
  identity-graph persistence, atomic single-key reservation, KV prefix
  listing (cluster support), platform geo, device host evidence
  (JA4/HTTP-2), and legacy-rewrite support. Persistence capabilities carry
  **per-record-class consistency requirements**, because "has KV" says
  nothing about whether revocation is observable:

  | Record class                                                                                                                                                                | Required semantics                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
  | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | Replay reservations (client-cycle)                                                                                                                                          | **Linearizable CAS with fencing** (ownership epoch). Eventually-consistent KV cannot provide this — on Cloudflare that means a Durable-Object-class primitive, not Workers KV; an adapter without it fails startup for client-cycle selection                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
  | Family revocation records                                                                                                                                                   | **Globally observable strong consistency** — every instance's read observes a committed revocation, not merely the writing session's own writes (writer-scoped read-your-writes is insufficient for a fleet). Cloudflare Workers KV is **not eligible** — "60 seconds or more" is an expectation, not a bound; on Cloudflare this record class needs a Durable-Object-class primitive. An adapter without an eligible primitive fails startup for identity features. A failed/erroring read fails closed; a successful absence has no positive-use lease, while a present revocation may be cached only to deny.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
  | Authority-state records (suppression + positive summary)                                                                                                                    | **Globally observable strong reads AND linearizable per-key CAS** — CAS alone orders writes, but a stale successful _read_ on another instance would authorize egress after a committed suppression ("read failures fail closed" does not cover stale successes); both properties are adapter eligibility gates. Absence and positive authority receive no success lease. A typed restrictive result may be cached only to deny and cannot construct authority, clear state, acknowledge an intent, or drive a CAS.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
  | Negative-intent outbox + global identity safety breaker                                                                                                                     | Per-family outbox records require globally strong reads + linearizable CAS in a failure domain independent of the authority/revocation target. The deployment breaker follows permission §4.3's qualified failure contract. Every identity decision performs fresh checks of both before positive operation; no absence/healthy success lease is allowed. A typed pending/tripped result may be cached only to deny. A deployment lacking the independent primitive or its failure-semantics proof cannot enable persisted identity use.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
  | Identity-row mutation                                                                                                                                                       | **Generation CAS** (conditional write on row generation) with reread/recompute on conflict — rows are heavily mutable (snapshots replaced, partner IDs merged, derived state refreshed), so unordered last-writer-wins loses newer evidence and mappings; _visibility_ may stay eventual, unordered _mutation_ may not. Fastly KV offers generation-marker conditional writes; Workers KV's documented concurrent last-write-wins is ineligible for mutation-bearing rows                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
  | Row creation                                                                                                                                                                | **Atomic create-if-absent** (fresh mints), same primitive family                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
  | Deployment metadata — schema-floor mirror; graphless flag; policy/config/model candidate and activation register; config-sequence allocator; global identity safety breaker | **Globally strong reads + linearizable CAS outside ordinary config storage.** Activation additionally needs immutable version-addressed publication, authenticated authoritative fleet membership/readiness/quiescence, one deployment-qualified `serve_admission_lease_bound_ms`, one authenticated nondecreasing Unix-ms time domain shared by the activation register and immutable journal-object service, atomic local admission/refcount closure, fail-closed lease renewal/restart/suspend behavior, and an immutable time-retained activation journal. The common clock enforces `promotion_not_before_unix_ms`, journal creation at/after that gate, and the 60-second publication-age ceiling; incomparable clocks are ineligible. The bounded lease amortizes only whole-settings/model admission; it never leases privacy-state success. Graphless deadlines need store-issued time or a declared bounded fleet clock skew. The config-sequence allocator assigns ordered envelope versions, and the activation register binds complete blob/data/config/policy/model identities plus the journal head; ordinary config-store `put` and the `m00` compatibility mirror activate nothing. After model promotion the authenticated controller idempotently CAS-raises and read-verifies `m00`; a higher mirror fails closed for investigation. |
  | Rewrite transactions                                                                                                                                                        | **Linearizable fenced CAS required** (same primitive class as reservations)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
  | Rowless prefix-withdrawal (`w`) records                                                                                                                                     | **Globally strong reads + linearizable CAS** (same bar as authority-state), plus the **bounded listing-visibility window** the backfill scan depends on — all three are eligibility gates for the graphless migration. Successful absence has no positive-use lease; a matching restrictive result may be cached only to deny. The strong-read obligation is **permanent for HMAC row discovery** — enforcement runs to the last entry's `valid_until`, long after the flag clears — and retention runs through the **max(cookie, row, S2S) horizon of each entry** (an earlier "cookie lifetime" cell contradicted §6.3's per-entry horizons)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
  | Alias installs (reserved)                                                                                                                                                   | Row-store **per-key CAS with read-your-writes** — recorded for the future `rewrite_legacy` spec; nothing in the epic writes an alias                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
  | Identity rows                                                                                                                                                               | Eventual **visibility** acceptable _after_ a generation-CAS mutation commits (see Identity-row mutation above) — the earlier "rows are accretive" claim is deleted: rows replace snapshots, merge partner IDs, and refresh derived state, and unordered last-writer-wins loses newer evidence                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |

  Every record class additionally declares **durability and maximum
  retention**: a store passing the consistency check but capping TTLs
  below the computed revocation/suppression horizon (e.g. a 30-day
  maximum against one-year rows) would let identities become usable
  again when their revocation expires — startup proves the configured
  store meets each class's computed horizon, and persistence across
  restart is part of the declaration. Each adapter's declaration is part
  of its wiring, drives the §6 capability-mismatch startup error, and every §6.2 runtime-failure row
  gets fault-injection coverage on every adapter declaring the
  corresponding capability. The **concrete per-adapter values** — the
  actual matrix, not the abstract capability list — as known today; a
  cell marked _verify_ must be established before the depending feature
  is selectable on that adapter, and the filled matrix is normative:

  Cells distinguish **platform availability** (the host offers a
  primitive) from **wired** (Trusted Server integrates it) — conflating
  them is how a "yes" cell hides an unusable feature. Feature eligibility
  requires wired, not merely available:

  | Capability                                                                                                                                                                                                                                   | Fastly                                                                                                                                                                                                             | Axum (dev)                                                                                                                    | Cloudflare                                                                                                                                                                               | Spin                                                                                             |
  | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
  | Graph persistence (eventual OK)                                                                                                                                                                                                              | KV Store: available + wired                                                                                                                                                                                        | **Not wired** — the current adapter installs `UnavailableKvStore`; an in-process store is dev-feasible but does not exist yet | Workers KV: available + wired (eventually consistent)                                                                                                                                    | Key-value: **available, not wired** — Spin config is embedded and EC KV routes are unwired today |
  | Prefix listing (cluster)                                                                                                                                                                                                                     | Used today, but **ratification requires a cited platform completeness bound, pagination behavior, and failure semantics** — the graphless scan's settle window cannot be derived from "works in practice"          | **Unavailable** (no store wired — in-process feasibility is a note, not a cell)                                               | Yes (eventual)                                                                                                                                                                           | _verify_                                                                                         |
  | Strongly consistent revocation reads                                                                                                                                                                                                         | _verify_ against Fastly KV semantics                                                                                                                                                                               | **Unavailable** (no store wired)                                                                                              | **Workers KV: no** — needs Durable Objects, not currently wired                                                                                                                          | _verify_                                                                                         |
  | Linearizable fenced CAS _(informative — deferred features)_                                                                                                                                                                                  | **Not currently available**                                                                                                                                                                                        | **Unavailable** (no store wired)                                                                                              | Durable Objects: possible, not wired                                                                                                                                                     | **No**                                                                                           |
  | Platform geo                                                                                                                                                                                                                                 | Yes — country + region                                                                                                                                                                                             | No                                                                                                                            | **Yes — country only, no region** (`cf-ipcountry`): regionless US traffic uses the country-wide protective US floor                                                                      | No                                                                                               |
  | Device host evidence (JA4/H2)                                                                                                                                                                                                                | Platform-available but **feature deferred and startup-ineligible in this epic**                                                                                                                                    | No                                                                                                                            | No                                                                                                                                                                                       | No                                                                                               |
  | Authority-state: global strong reads + CAS                                                                                                                                                                                                   | Conditional writes available (generation marker); **globally current read semantics to verify** — both required, wiring to verify                                                                                  | **Unavailable** (no store wired)                                                                                              | Workers KV: **ineligible**; Durable Objects: feasible, not wired                                                                                                                         | **Unavailable**                                                                                  |
  | Negative-intent outbox + global identity safety breaker                                                                                                                                                                                      | **Not wired**; requires a globally strong CAS failure domain independent of authority/revocation targets plus the §4.3 failure proof                                                                               | **Unavailable**                                                                                                               | A separate DO-class domain is feasible, not wired                                                                                                                                        | **Unavailable**                                                                                  |
  | Identity-row generation-CAS mutation                                                                                                                                                                                                         | Same primitive as above                                                                                                                                                                                            | **Unavailable**                                                                                                               | Workers KV: **ineligible**; DO: feasible, not wired                                                                                                                                      | **Unavailable**                                                                                  |
  | Row create-if-absent                                                                                                                                                                                                                         | Generation-marker create: available, **wiring to verify**                                                                                                                                                          | **Unavailable**                                                                                                               | Workers KV: **ineligible**; DO: feasible, not wired                                                                                                                                      | **Unavailable**                                                                                  |
  | Deployment metadata — `m00` schema-floor compatibility mirror (monotonic CAS; never activates writes; idempotent controller repair/read-verification)                                                                                        | **Not wired** — needs a primitive distinct from the config store and the controller completion/repair path                                                                                                         | **Unavailable**                                                                                                               | DO: feasible, not wired; repair path absent                                                                                                                                              | **Unavailable**                                                                                  |
  | Graphless flag (globally strong reads + CAS, plus the store-clock **or** S_fleet deadline branch)                                                                                                                                            | **Not wired**; neither clock branch qualified (no store-time primitive verified, no declared fleet-skew bound)                                                                                                     | **Unavailable**                                                                                                               | DO: feasible, not wired; clock branch unqualified                                                                                                                                        | **Unavailable**                                                                                  |
  | Config-sequence allocator (linearizable CAS)                                                                                                                                                                                                 | **Not wired** — ordinary config-store `put` is insufficient                                                                                                                                                        | **Unavailable**                                                                                                               | DO: feasible, not wired                                                                                                                                                                  | **Unavailable**                                                                                  |
  | Serve-admission lease bound/timebase (positive `serve_admission_lease_bound_ms`, timer error/suspend proof, shared register/journal clock, non-early deadline)                                                                               | **Unqualified** — no exact bound or timer/suspend proof declared and no shared register/journal time domain or non-early gate wired                                                                                | **Unavailable**                                                                                                               | DO-class coordination is feasible, but no exact bound, timer/suspend proof, shared time domain, or non-early gate is wired                                                               | **Unavailable**                                                                                  |
  | Immutable config publication + settings/model prepare/activation register (strong reads + CAS + bounded admission lease + store-clock not-before + membership/readiness/quiescence + atomic whole-request gate/refcount + journal retention) | **Not wired**; version-addressed publication, authoritative settings/model readiness/quiescence, lease timebase/bound, store-enforced not-before, atomic admission/refcount, and journal lifecycle are unqualified | **Unavailable**                                                                                                               | DO metadata is feasible; immutable publication, settings/model membership/readiness/quiescence, bounded lease/not-before, atomic admission/refcount, and journal lifecycle are not wired | **Unavailable**                                                                                  |
  | Durability / max-retention proof                                                                                                                                                                                                             | KV durable; TTL ceilings **to verify** against computed horizons                                                                                                                                                   | **Unavailable**                                                                                                               | Workers KV TTLs: to verify; DO storage: feasible                                                                                                                                         | **Unavailable**                                                                                  |

  Activation qualification fixtures cover: a validation read linearizing before
  and after the drain CAS; a delayed successful response whose usable lease is
  shortened or expired, including a delayed old-generation response after
  reopen; a last-instant admission with a long-running request (time bound
  passed but early promotion still rejected until quiescence); zero-bound
  candidate rejection, qualified/candidate/readiness bound mismatch, and a
  bound-change attempt while traffic remains eligible; slow/fast timer and
  store-clock rate, forward clock steps, incomparable register/journal clocks,
  an attempted time-domain change while traffic/candidate is live, restart,
  suspend/resume, expiry-boundary, activation-watcher delay, and renewal
  failure; member
  partition/crash/replacement, membership restage, cancel-drain/retry, and stale
  acknowledgment; and background origin/vendor egress, cache publication, and
  identity mutation remaining inside the atomic quiescence refcount. The model
  fixture proves no `pre_epic_v1` request overlaps a v2 write. Privacy-state
  fixtures prove stale restrictive caches only deny and every positive path
  still performs fresh revocation, authority, outbox, `w`, and breaker reads.
  Mirror fixtures cover missing, lower, equal, higher, unreadable, CAS failure,
  read-verification failure, and controller crash after the authoritative model
  CAS but before `m00`; only missing/lower/equal are idempotently raised or
  confirmed, while higher remains fail-closed.

  Whole-settings/model activation is a universal serve capability, including
  for identity-free requests. A stateless-identity selection waives only the
  identity persistence/strong-state cells; it never bypasses activation,
  admission, journal, or quiescence qualification. An adapter that cannot
  qualify the activation rows cannot serve under this spec.

- Each adapter's runtime-services setup routes through the shared builders.
- Providers are constructed **once** per application instance and stored in
  app state; PR #838 rebuilt the provider (cloning the secret into a fresh
  `Box<dyn …>`) up to three times per request.
- The cross-adapter parity suite gains cases asserting: (a) the selected
  provider is honored on every adapter, (b) the neutral default performs no
  host call on every adapter, and (c) a capability-unsatisfiable selection
  fails startup on the adapters that cannot satisfy it.

## 8. Crate layout and CI

Provider crates live flat under `crates/` following the existing naming
convention: `crates/trusted-server-geo-fastly`,
`crates/trusted-server-device-fastly`. (PR #838 introduced a nested
`crates/geo/fastly` layout that broke the directory–package correspondence
every other member follows.) No placeholder directories: a `crates/…/README.md`
with no crate ships when the first crate does.

Every new crate is added to the `.cargo/config.toml` aliases
(`check-fastly`, `clippy-fastly`, `test-fastly`, `build-fastly`) in the same
PR that adds the crate, and to the CI gate list in `CLAUDE.md`. PR #838's new
crates compiled only transitively and were never linted with `-D warnings`
nor had a single test.

## 9. Behavior preservation notes

Two defaults chosen for neutrality change effective behavior on existing
Fastly deployments; both are called out in the migration spec and must be
prominent in release notes:

- **Bot gate.** The pre-provider EC bot gate required JA4 and platform
  class. This epic intentionally falls back to the builtin User-Agent-only
  classifier; `[device] provider = "fastly"` is startup-rejected until the
  separate host-fingerprinting/security design is ratified. Release notes
  call out the loss of the stronger gate rather than presenting selection
  alone as authorization.
- **Geo.** With no geo provider, jurisdiction resolution falls to the
  configured default country. The permission model spec (§5.3) constrains
  this combination so it cannot silently grant permissions to mis-attributed
  traffic, and §11 below sequences the default flip so the constraint exists
  before the flip does.

## 10. Testing strategy

- Provider conformance suite (§3 invariant, deterministic entropy) run
  against every shipped provider.
- EC minting-gate tests (§5), including the spy-provider case: permission
  unset → `generate` never called, withdrawal still tombstones.
- Legacy-reader tests (§6.1): provider switch → old cookie resolves and
  withdraws; unmatched old cookie never egresses.
- Settings validation tests for every row of the §6 table, including the
  block-without-selector rejection and the `legacy_providers` rules.
- Parity suite additions of §7.
- Unit tests inside each provider crate; crates with no native-target tests
  still get clippy coverage via the alias wiring of §8.

## 11. Implementation order

1. Traits + lifecycle contract + conformance suite, `hmac` provider
   passing it (behavior-identical to today; see migration spec §3 for the
   ID-stability vectors).
2. Settings selection + validation table.
3. Composition root + all four adapters wired through it, parity cases.
4. Device and geo provider selection. **The geo neutral default does not
   flip in this step**: under the current jurisdiction gate, absent geo
   resolves to `Unknown`, which fails closed — flipping the default here
   would zero EC issuance for every deployment that had not yet opted into
   `[geo] provider = "platform"`. Until step 5, the Fastly adapter's geo
   selection defaults to `platform` (today's always-on behavior); the
   selector exists, only its default is held back.
5. The permission model PR: flips the geo default to none **in the same
   change** that introduces acknowledged static-jurisdiction
   `default_country` mode, the protective provider-failure profile, and the §5.3
   acknowledgment guard, and adds the EC permission-enforcement point of
   §5; `required_permissions()` appears on the EC trait in this step, not
   before (per the §4 minimalism rule).

Steps 1–4 are independently reviewable, behavior-preserving, and do not
depend on the permission model: the EC gate keeps its current jurisdiction
logic until the permission model PR replaces it.

## 12. Divergences from issue #778

This spec supersedes #778 on the following points; the issue is updated to
reference this spec when the PR merges, so implementation has one
acceptance contract:

| #778 says                                                    | This spec says                                                                                                         | Why                                                                                                                             |
| ------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Identifier comparison is a provider operation (`keys_equal`) | Comparison is structural: `parse` canonicalizes, so equivalent envelopes become the same identifier and graph key (§3) | Satisfies the same requirement with no comparison method to leave uncalled                                                      |
| A provider can return response headers                       | Dropped (§4)                                                                                                           | Empty in every built-in in PR #838, plumbed through three layers with no consumer; returns with the first feature that needs it |
| One built-in provider (HMAC) preserving today's behavior     | Same, plus explicit legacy-reader semantics for later switches (§6.1)                                                  | Switching was unspecified in #778 and stranded identities in the #838 shape                                                     |
