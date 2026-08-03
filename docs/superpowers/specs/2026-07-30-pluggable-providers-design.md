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

| Concern     | Trait                | Built-in default            | Opt-in host implementation                                        |
| ----------- | -------------------- | --------------------------- | ----------------------------------------------------------------- |
| EC identity | `EdgeCookieProvider` | none (stateless)            | `hmac` (in core; HMAC over client IP, preserves today's identity) |
| Device      | `DeviceProvider`     | `builtin` (User-Agent only) | `fastly` (JA4 / HTTP-2 fingerprints)                              |
| Geo         | `GeoProvider`        | none (no location)          | `platform` (host geo lookup)                                      |

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
| **Canonical graph key**                  | KV identity-graph row reads/writes                                                                                     | The provider supplies a canonical key **suffix**; **core constructs the physical key** per the §6.3 key grammar (legacy-HMAC verbatim keys excepted), so cross-provider and cross-record-kind isolation is enforced by construction rather than promised by provider code. Suffixes are stable, KV-safe (length and character-set limits), and collision-free within the provider's space. Two equivalent envelopes of one identity map to one key — verbatim cookie bytes as the key would fork graph rows on canonicalization differences and discard today's batch-sync canonicalization.                                                                |
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
  — dropped entirely, not deferred: §5 explains why these two kinds cannot
  be permission-gated at all.

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

**A generated identity is not active until its graph row commits.** No
cookie write, no egress, no auction use may observe a minted identifier
before its graph row (with provenance, §6.1) has committed — PR #838 let a
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
  (legacy and N+1-minted alike). Only then does the invariant hold:
  every row-backed identity has an authority-state record under its
  derivable family ID, in the globally-strong class — so _rowless_ =
  flag set AND the strong read finds **no record** for the cookie's
  derived family ID. Graphless-era cookies never got a stub because
  they have no row for the scan to find; no eventual read participates. The flag itself is specified: a named
  deployment-metadata key (write-once/CAS class), set by the §4.2
  readiness step only on deployments that actually ran graphless
  (requires the deployment-metadata capability), surviving binary
  rollback, and **cleared by an explicit operator action** once the
  migration window closes (quiet-period criterion in the guide) —
  clearing ends rowless classification permanently. Outside the flag,
  or on any read error, the state is **indeterminate**: no identity
  use, no mint, no cookie expiry — "treated as absent" was the wrong
  contract, since absence feeds the fresh-mint path.
- A verified rowless cookie (`verify → VerifiedIdentity`, carrying the
  matched version) is **expired and replaced by a fresh mint through the
  ordinary graph-backed path** when permissions allow; continuity is
  deliberately not preserved (migration matrix row 13, sign-off 21). An
  unverifiable cookie (including the declared roaming false-negative) is
  simply expired.
- **Rowless withdrawal writes into one capped per-prefix record, then
  expires the cookie** — durable (cookie-only expiry is best-effort: a
  lost response leaves the "withdrawn" cookie usable), and **bounded in
  storage**, which separate exact-cookie records were not: a holder of
  one valid prefix can fabricate billions of suffix variants, and
  per-variant records would be attacker-priced strong storage. The
  record (strong class, keyed on the verified prefix) holds a bounded
  list (cap 8) of withdrawn-suffix hashes; writes are admitted only for
  **prefix-verified** cookies; **saturation escalates to prefix-wide
  rowless revocation** — every rowless cookie under that prefix is
  treated withdrawn, which harms only the abuser's own same-IP graphless
  cohort and is the declared abuse response (legitimate users hold one
  or two variants ever). A re-presented withdrawn variant finds its
  entry (or the saturated record) and stays dead. Row-backed
  withdrawal is untouched: full-graph-key family records, one derivation
  everywhere.
- **Negative-record creation has admission rules everywhere**:
  suppression and family records may be written only for (a) an
  existing family — the authority-state record exists on a strong
  read — or (b) a **verified** identifier (`verify`); a fabricated,
  unverifiable cookie writes nothing. This bounds record creation to
  real identities plus the writer's own evidence, and per-prefix
  rate limits apply to the rowless path above.

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
identity/consent headers, and the EIDs header — with identity reachable
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
- **Device: a decision, not a circularity.** Device classification is not
  an input to permission resolution (the inputs are jurisdiction, policy,
  and signals), so ordering geo → resolution → device → EC and gating
  device is perfectly implementable. This spec deliberately does not:
  the shipped device providers process technical request metadata (UA,
  JA4/HTTP-2 fingerprints) for **security classification** — the bot gate
  protecting KV-backed identity writes — which must run precisely for
  traffic that has granted nothing. The authorization for that processing
  is the operator's explicit `[device] provider` selection — a statement
  about the **opt-in host-fingerprint provider**; the `builtin` UA-only
  default processes nothing beyond the User-Agent every request already
  carries and needs no such authorization — and this spec records that as
  the decision, with its privacy implication stated: a
  device provider whose data use goes beyond security classification (for
  example feeding fingerprints into targeting or identity) is **not
  authorized by selection alone** and requires a vocabulary extension plus
  a gate before it may ship. This bites immediately, not hypothetically:
  today's graph rows persist the JA4 class, an HTTP/2 fingerprint hash,
  and buyer-facing quality metadata — persistence and scoring that exceed
  security classification. The epic therefore **stops writing
  fingerprint-derived buyer-facing fields into new rows** (a declared
  change, migration spec §2); the boolean security classification outcome
  may be persisted. Re-adding them is the vocabulary-extension route.
  The field-level graph contract itself is normative in this spec —
  §6.3 — not deferred to the implementation.

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
  basis, evidence timestamp, resolved jurisdiction, policy revision) that
  the S2S sync authority recomputes from (permission model spec §7).
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

| Failure                                                                   | Behavior                                                                                                                                                       |
| ------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `generate` returns an error                                               | No identity this request; request proceeds stateless; no cookie written                                                                                        |
| Graph-row commit fails at mint                                            | Mint never happened (§5): no cookie, no egress; next request retries                                                                                           |
| Authority-state commit fails after the row commit at mint                 | No cookie, no eligibility (the strong record never reported the revision); the orphan row expires by TTL; counted — **no recovery claim**, nothing can find it |
| Graph read fails on an existing identity                                  | Identity unusable this request (fail closed for egress); cookie untouched                                                                                      |
| Cluster prefix listing fails                                              | Treated as cluster-size-unknown → `cluster_fallback` policy applies                                                                                            |
| Tombstone write fails                                                     | Permission model spec §4.3: family retries, readers fail closed on partial families                                                                            |
| Geo provider returns invalid output (unparseable country) at runtime      | Treated as lookup failure → `default_country` (permission model spec §5.2), counted in the lookup-failure metric                                               |
| Device provider signals unavailable at runtime (e.g. no JA4 on a request) | Classification degrades per the provider's declared fallback, never silently upgrades `looks_like_browser`                                                     |

The **degraded-graph health signal** referenced above and by the
withdrawal contract is a defined state machine, not a vibe: it is
**per-instance and in-memory** (no shared propagation, no stored health
record whose own read could fail), entered when graph-write failures cross
a sliding-window threshold (N failures within window W), and exited with
hysteresis after M consecutive successes. While degraded: S2S partner
egress and sync updates fail closed; organic requests continue stateless.
The thresholds ship as constants with the implementation and are printed
in the startup log.

Its protection is therefore **local-only, and the spec says so**: a
backend-wide outage degrades every instance through its own observations
within one window, but an instance-local family-write failure leaves
other instances — which have no record to find, and healthy backends of
their own — serving S2S egress until the browser's durable signal retries
successfully. That residual is **unbounded for a never-returning visitor**
(sign-off item 11 — the permission and migration specs state this and
this spec must not undercut them), is counted (failed family writes are
a first-class metric), and is accepted
in place of a deployment-wide shared fail-closed channel, whose own
availability and freshness would be a harder problem than the one it
solves.

### 6.3 Storage contract — normative

**Physical key grammar.** Core constructs every key; providers supply only
the bounded suffix:

| Record class                                                                           | Key                                                                                                                                                             | Notes                                                                                                                                                                                                                                                                                                                                                                     |
| -------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Identity row (non-hmac)                                                                | `id/<provider>/<suffix>`                                                                                                                                        | Suffix from `graph_key_suffix`, ≤ 128 bytes, KV-safe alphabet. **No version segment** — the mint version lives in the row envelope; a versioned key would be circular (core would need the version to read the row that states the version)                                                                                                                               |
| Identity row (hmac, **every** version)                                                 | The identifier verbatim (`{64hex}.{6alnum}`)                                                                                                                    | Reserved grammar for the whole provider, not just v0 — keeping all HMAC versions on the verbatim scheme is what keeps the 64-hex cluster prefix a literal key prefix for every HMAC row. (Passphrase rotation still changes a given IP's HMAC, so clusters split across rotation until old identities expire — inherent to rotation, declared, not a key-scheme artifact) |
| Alias                                                                                  | **The source identity key itself** — the alias is a value written _at_ the replaced row's address, distinguished by a `kind` discriminator in the JSON envelope | A separate `alias/…` address could never work: lookups by the old cookie hit the source key, and a single-key CAS cannot install a record at a different address                                                                                                                                                                                                          |
| Family revocation                                                                      | `fam/<family-id>`                                                                                                                                               | Family ID from mint or the deterministic legacy derivation (permission spec §4.3)                                                                                                                                                                                                                                                                                         |
| Authority-state (suppression + positive-authority summary)                             | `s` + family ID (fixed-width grammar)                                                                                                                           | Per-permission negative entries **and** the positive summary (permission spec §4.3); permission-exempt writes; consulted by every constructor and S2S recompute                                                                                                                                                                                                           |
| Deployment metadata                                                                    | `m` + fixed metadata name (fixed-width grammar; schema floor, graphless-migration flag)                                                                         | Write-once/CAS class; value carries schema version, state, epoch, set-at; the graphless flag's lifecycle: created by the migration readiness step **after** N+2 convergence + stub-backfill completion (both attested in the value), cleared by explicit operator CAS with the quiet-period criterion recorded                                                            |
| Rewrite transaction _(informative — deferred)_                                         | `rwx/<family-id>`                                                                                                                                               | One in-flight rewrite per family                                                                                                                                                                                                                                                                                                                                          |
| Replay reservation _(informative — deferred with client-cycle; not normative surface)_ | `resv/…`                                                                                                                                                        | Deferred client-cycle draft                                                                                                                                                                                                                                                                                                                                               |

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
1-character class tag — `i` row, `r` family revocation, `s` suppression,
`x` transaction, every tag chosen **outside the hex alphabet** so no
generated key can begin with 64 hex characters, which is what makes
disjointness from the legacy `{64hex}.{6alnum}` grammar _provable_
rather than asserted (an earlier `f` tag was itself a hex digit) — then
a **4-character provider code from the checked-in, append-only,
never-reused registry `docs/superpowers/specs/provider-code-registry.md`** (allocation is a reviewed commit;
codes are immutable and never recycled, including for retired
providers), then the suffix. Segment boundaries are positional, so no
segment can contain or escape a delimiter, prefix queries are plain
string prefixes on every backend, and a grammar-disjointness test covers
every class against the legacy grammar. (hmac verbatim keys remain the
reserved exception, with the 64-hex cluster prefix at position zero.)

**Wire schemas** (JSON, like identity rows; every class carries a schema
version): the **alias record** (reserved-future, with rewrite) holds target key, created-at, retirement
deadline, and fencing epoch; the **family revocation record** holds the
family ID, revoked-at, triggering signal class (§4.5 destructive column),
and a **family epoch** bumped on every revocation-state change (the
client-cycle commit CAS is conditioned on it) — deliberately no identity
data, so it can outlive its members; the **authority-state record** holds, per permission — negative side:
state (`suppressed`/`cleared`), cause, source class, authoritative or
observation evidence timestamp, entry `valid_until` (evidence-class TTL;
expired entries are inert), and the provenance revision a clear
references; positive side (the summary, **every field the permission
spec's absence/replay decisions consume — a reduced schema cannot
reproduce them**): kind (user evidence vs policy baseline), grant
basis/source class, policy revision (digest + activation generation),
`valid_until`, provenance revision, evidence timestamp, and the
per-permission **semantic digest with its pinned first-seen/
first-normalized timestamps** (anti-replay); record level: family ID,
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

| Field                                                                                                                                                                                                                                   | Purpose                                                                                                                                                                                                                                                                              | Source                                                  | Gating permission (egress)                | TTL / refresh                                                                      | Rewrite                                    | On revocation                                                                                                           |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------- | ----------------------------------------- | ---------------------------------------------------------------------------------- | ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------- |
| key (v1: identifier verbatim; v2: core-constructed, §4)                                                                                                                                                                                 | Row identity                                                                                                                                                                                                                                                                         | Provider/core                                           | —                                         | Row TTL (1 y today)                                                                | New canonical row; old key becomes alias   | Family record governs; member tombstone as cleanup                                                                      |
| `v`                                                                                                                                                                                                                                     | Schema discriminator                                                                                                                                                                                                                                                                 | Core                                                    | —                                         | —                                                                                  | Written at current version                 | Retained                                                                                                                |
| `created` / **`expires_at`**                                                                                                                                                                                                            | Row age and the **absolute retention deadline, pinned at mint** — every update writes with the _remaining_ lifetime, never a fresh full TTL (today's full-TTL rewrite lets a frequently visited identity live forever; refreshable derived state must never rejuvenate the identity) | Core                                                    | P1 (first-party ops)                      | Never extended                                                                     | Preserved (no rejuvenation)                | Retained in tombstone                                                                                                   |
| `consent.tcf` / `consent.gpp`                                                                                                                                                                                                           | Raw signal snapshot for audit; superseded as authority by provenance                                                                                                                                                                                                                 | Request                                                 | Never egressed to partners                | Replaced on live resolution (§7 snapshot rule, permission spec)                    | Fresh live values                          | Scrubbed                                                                                                                |
| `consent.ok` / `consent.updated`                                                                                                                                                                                                        | v1 liveness flag — **superseded by the family revocation record**; written during member cleanup for v1-reader benefit through the transition                                                                                                                                        | Core                                                    | —                                         | —                                                                                  | Fresh                                      | `ok = false` written as cleanup; the family record is authoritative (v1's 24 h tombstone TTL does not bound revocation) |
| New: **immutable mint tag** (`mint_provider`, `mint_version`)                                                                                                                                                                           | Credential retirement and audit — write-once at mint (legacy backfill may populate a missing tag once); **never part of the replaceable snapshot**, or a v1 identity revisited after rotation would be restamped v2                                                                  | Mint (or one-time backfill)                             | —                                         | Immutable                                                                          | —                                          | Retained                                                                                                                |
| New: **provenance revision** (application-level monotonic counter, u64, initialized at 1, incremented by every provenance-bearing write, CAS'd with the row generation, serialized as an integer; overflow is a hard error, not a wrap) | Orders clears vs. snapshots (permission spec §4.3)                                                                                                                                                                                                                                   | Core                                                    | Read by S2S/clears                        | Monotonic                                                                          | —                                          | Retained                                                                                                                |
| New: per-permission provenance (grant basis, authoritative timestamp, `valid_until`, jurisdiction, policy revision)                                                                                                                     | S2S authority                                                                                                                                                                                                                                                                        | Live resolution only                                    | Read by S2S recompute                     | `valid_until` per evidence class; replaced atomically, never merged                | **Fresh live resolution** — never copied   | Scrubbed                                                                                                                |
| New: `family_id`                                                                                                                                                                                                                        | Revocation discovery                                                                                                                                                                                                                                                                 | Core at mint (derived for legacy, permission spec §4.3) | —                                         | Immutable                                                                          | Shared across linked rows                  | Is the revocation key                                                                                                   |
| `geo.country` / `geo.region`                                                                                                                                                                                                            | Jurisdiction snapshot                                                                                                                                                                                                                                                                | Geo provider at mint                                    | P1                                        | Written at mint                                                                    | Fresh                                      | Scrubbed                                                                                                                |
| `geo.asn` / `geo.dma`                                                                                                                                                                                                                   | Cluster disambiguation / market signal                                                                                                                                                                                                                                               | Platform at mint                                        | P1; DMA additionally P4 for bidstream use | Written at mint                                                                    | Fresh                                      | Scrubbed                                                                                                                |
| `pub_properties` (origin/seen domains)                                                                                                                                                                                                  | Creation context                                                                                                                                                                                                                                                                     | Core at mint                                            | P1                                        | Write-once                                                                         | Preserved                                  | Scrubbed                                                                                                                |
| `device.*` (JA4 class, H2 hash, quality metadata)                                                                                                                                                                                       | **Discontinued for new rows** (§5): fingerprint-derived, buyer-facing — beyond security-classification authorization. v1 rows retain them read-only; they are never egressed post-epic and are dropped at rewrite                                                                    | Fastly device provider                                  | None grants egress                        | Write-once (v1)                                                                    | **Dropped**                                | Scrubbed                                                                                                                |
| New: security classification outcome (boolean)                                                                                                                                                                                          | Bot-gate result                                                                                                                                                                                                                                                                      | Device provider                                         | — (never egressed)                        | Written at mint                                                                    | Fresh                                      | Scrubbed                                                                                                                |
| `network.*` (immutable evidence: ASN etc.)                                                                                                                                                                                              | Cluster disambiguation                                                                                                                                                                                                                                                               | Platform at mint                                        | P1                                        | Write-once                                                                         | Fresh                                      | Scrubbed                                                                                                                |
| Derived cluster state (`cluster_size`, computed-at)                                                                                                                                                                                     | Trust gating                                                                                                                                                                                                                                                                         | Computed                                                | —                                         | **Refreshable, short validity; generation-CAS update; never touches `expires_at`** | Recomputed                                 | Scrubbed                                                                                                                |
| `ids` (partner → UID map)                                                                                                                                                                                                               | Partner identity graph                                                                                                                                                                                                                                                               | Pixel/pull/batch sync                                   | P1 ∧ P4 (partner egress)                  | Per-mapping timestamps; bounded count/length                                       | Copied **with original timestamps/expiry** | Scrubbed                                                                                                                |
| New: alias record kind                                                                                                                                                                                                                  | Rewrite indirection (§6.1)                                                                                                                                                                                                                                                           | Core                                                    | —                                         | Retirement deadline                                                                | Is the mechanism                           | Family-revoked like any member                                                                                          |

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

  | Record class                                             | Required semantics                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
  | -------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | Replay reservations (client-cycle)                       | **Linearizable CAS with fencing** (ownership epoch). Eventually-consistent KV cannot provide this — on Cloudflare that means a Durable-Object-class primitive, not Workers KV; an adapter without it fails startup for client-cycle selection                                                                                                                                                                                                                                                                                                |
  | Family revocation records                                | **Globally observable strong consistency** — every instance's read observes a committed revocation, not merely the writing session's own writes (writer-scoped read-your-writes is insufficient for a fleet). Cloudflare Workers KV is **not eligible** — "60 seconds or more" is an expectation, not a bound; on Cloudflare this record class needs a Durable-Object-class primitive. An adapter without an eligible primitive fails startup for identity features. A **failed or erroring revocation-record read fails closed** for egress |
  | Authority-state records (suppression + positive summary) | **Globally observable strong reads AND linearizable per-key CAS** — CAS alone orders writes, but a stale successful _read_ on another instance would authorize egress after a committed suppression ("read failures fail closed" does not cover stale successes); both properties are adapter eligibility gates                                                                                                                                                                                                                              |
  | Identity-row mutation                                    | **Generation CAS** (conditional write on row generation) with reread/recompute on conflict — rows are heavily mutable (snapshots replaced, partner IDs merged, derived state refreshed), so unordered last-writer-wins loses newer evidence and mappings; _visibility_ may stay eventual, unordered _mutation_ may not. Fastly KV offers generation-marker conditional writes; Workers KV's documented concurrent last-write-wins is ineligible for mutation-bearing rows                                                                    |
  | Row creation                                             | **Atomic create-if-absent** (fresh mints), same primitive family                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
  | Deployment metadata (schema floor)                       | **Write-once/CAS**, outside ordinary config storage (migration spec §4)                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
  | Rewrite transactions                                     | **Linearizable fenced CAS required** (same primitive class as reservations)                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
  | Alias installs (reserved)                                | Row-store **per-key CAS with read-your-writes** — recorded for the future `rewrite_legacy` spec; nothing in the epic writes an alias                                                                                                                                                                                                                                                                                                                                                                                                         |
  | Identity rows                                            | Eventual **visibility** acceptable _after_ a generation-CAS mutation commits (see Identity-row mutation above) — the earlier "rows are accretive" claim is deleted: rows replace snapshots, merge partner IDs, and refresh derived state, and unordered last-writer-wins loses newer evidence                                                                                                                                                                                                                                                |

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

  | Capability                                                  | Fastly                                                                                                                            | Axum (dev)                                                                                                                    | Cloudflare                                                                                                                                                                 | Spin                                                                                             |
  | ----------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
  | Graph persistence (eventual OK)                             | KV Store: available + wired                                                                                                       | **Not wired** — the current adapter installs `UnavailableKvStore`; an in-process store is dev-feasible but does not exist yet | Workers KV: available + wired (eventually consistent)                                                                                                                      | Key-value: **available, not wired** — Spin config is embedded and EC KV routes are unwired today |
  | Prefix listing (cluster)                                    | Yes (used today)                                                                                                                  | **Unavailable** (no store wired — in-process feasibility is a note, not a cell)                                               | Yes (eventual)                                                                                                                                                             | _verify_                                                                                         |
  | Strongly consistent revocation reads                        | _verify_ against Fastly KV semantics                                                                                              | **Unavailable** (no store wired)                                                                                              | **Workers KV: no** — needs Durable Objects, not currently wired                                                                                                            | _verify_                                                                                         |
  | Linearizable fenced CAS _(informative — deferred features)_ | **Not currently available**                                                                                                       | **Unavailable** (no store wired)                                                                                              | Durable Objects: possible, not wired                                                                                                                                       | **No**                                                                                           |
  | Platform geo                                                | Yes — country + region                                                                                                            | No                                                                                                                            | **Yes — country only, no region** (`cf-ipcountry`): regionless US traffic degrades per the permission spec's declared rule, which directly changes state-level US outcomes | No                                                                                               |
  | Device host evidence (JA4/H2)                               | Yes                                                                                                                               | No                                                                                                                            | No                                                                                                                                                                         | No                                                                                               |
  | Authority-state: global strong reads + CAS                  | Conditional writes available (generation marker); **globally current read semantics to verify** — both required, wiring to verify | **Unavailable** (no store wired)                                                                                              | Workers KV: **ineligible**; Durable Objects: feasible, not wired                                                                                                           | **Unavailable**                                                                                  |
  | Identity-row generation-CAS mutation                        | Same primitive as above                                                                                                           | **Unavailable**                                                                                                               | Workers KV: **ineligible**; DO: feasible, not wired                                                                                                                        | **Unavailable**                                                                                  |
  | Row create-if-absent                                        | Generation-marker create: available, **wiring to verify**                                                                         | **Unavailable**                                                                                                               | Workers KV: **ineligible**; DO: feasible, not wired                                                                                                                        | **Unavailable**                                                                                  |
  | Deployment metadata (write-once/CAS)                        | **Not wired** — needs a primitive distinct from the config store                                                                  | **Unavailable**                                                                                                               | DO: feasible, not wired                                                                                                                                                    | **Unavailable**                                                                                  |
  | Durability / max-retention proof                            | KV durable; TTL ceilings **to verify** against computed horizons                                                                  | **Unavailable**                                                                                                               | Workers KV TTLs: to verify; DO storage: feasible                                                                                                                           | **Unavailable**                                                                                  |

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

- **Bot gate.** The pre-provider EC bot gate required JA4 _and_ platform
  class. With `device.provider = "builtin"` the gate degrades to User-Agent
  heuristics. Restoring the stronger gate requires `[device] provider =
"fastly"`; the migration guide lists this as a behavior-preserving step for
  Fastly deployments.
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
   change** that introduces the `default_country` fallback and the §5.3
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
