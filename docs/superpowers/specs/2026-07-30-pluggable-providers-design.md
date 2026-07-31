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
| **Canonical graph key**                  | KV identity-graph row reads/writes                                                                                     | The provider maps a canonical identifier to its graph key: stable, KV-safe (within KV length and character-set limits), collision-free across the provider's identifier space, and namespaced so two providers' key spaces cannot collide. Two equivalent envelopes of one identity map to one key — verbatim cookie bytes as the key would fork graph rows on canonicalization differences and discard today's batch-sync canonicalization.                                                                                                                                                                                                                |
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
  characters) and a global maximum length — for the identifier itself, not
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
    /// Mint an identifier from request evidence.
    fn generate(&self, input: &IdentityInput<'_>) -> Result<EcId, Report<EcError>>;
    /// Parse and canonicalize a cookie value into this provider's
    /// identifier; None when unrecognized. Values the provider's declared
    /// equivalence fixtures name as equivalent canonicalize identically.
    fn parse(&self, value: &str) -> Option<EcId>;
    /// Canonical KV graph key for a parsed identifier.
    fn graph_key(&self, id: &EcId) -> GraphKey;
    /// Cluster capability: a literal byte prefix of `graph_key(id)`, shared
    /// across identifiers minted from the same client evidence. None when
    /// the provider does not support IP-cluster semantics (§3).
    fn cluster_prefix(&self, id: &EcId) -> Option<HashPrefix>;
}
```

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
normative order is: gate → `generate` → graph-row commit → cookie write →
eligible for egress. A graph-commit failure means the mint never happened:
no cookie, no egress, error logged, the next request retries.

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
  is the operator's explicit `[device] provider` selection, and this spec
  records that as the decision, with its privacy implication stated: a
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
  Relatedly, the implementation PR must deliver a **field-level graph
  contract table** — for every persisted row field: purpose, source,
  gating permission, TTL, rewrite behavior, egress paths, and tombstone
  scrubbing — reviewed against the egress inventory.

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
| No `provider`, no providers block                                                                                                                    | Valid: the neutral default for that concern.                                                                                                                                                                                                                       |
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
  `mint_version = "v2"` selecting the writer; `parse` consults versions in
  declared order, newest first; removing a version entry is a retirement
  subject to the same evidence rules as retiring a legacy reader
  (migration spec §6).
- A cookie recognized by a legacy reader is a live identity for
  read/withdrawal purposes; whether it is transparently re-minted under the
  active writer is a per-deployment choice
  (`[ec] rewrite_legacy = true|false`), and re-minting is subject to the
  full minting gate of §5.
- **Rewrite is transactional, linking, and confirmed by presentation.**
  Order: new row commits first, carrying a link to the old row (sharing
  its revocation family ID, permission model spec §4.3) and a copy of the
  old row's consent metadata and partner mappings; then the new cookie is
  emitted. The server only emits `Set-Cookie` — it cannot observe delivery
  or acceptance, so **both linked rows stay live** until a later request
  **presents the new cookie** (confirmation by presentation); only then is
  the old row retired. A deployment may additionally cap the window with a
  grace period no shorter than the old cookie's maximum lifetime plus
  rollout skew. An interrupted or unconfirmed rewrite leaves the old
  cookie fully valid — no state in which neither identity works.
  **Withdrawal of either linked row revokes the shared family, i.e.
  both.**
- Retiring a legacy reader is the explicit end of those identities:
  the migration guide documents the cleanup procedure (migration spec §6).
- Tests: switch active provider → request with old cookie → identity still
  resolves and a withdrawal tombstones it (both linked rows when
  rewritten); old cookie with no matching legacy reader → treated as
  absent and **never egresses**; interrupted rewrite → old cookie still
  live, retry completes; `provider = "none"` + legacy reader → no mints,
  withdrawal still works.

Cluster degradation config (referenced from §3): when the active writer
lacks the cluster capability, `[ec] cluster_fallback = "allow" | "deny"`
decides whether KV-backed writes gated on cluster trust proceed; there is
no implicit default — the operator chooses.

### 6.2 Runtime failure matrix — normative

Startup validation (§6) covers configuration; this covers what happens
when a healthy configuration meets an unhealthy runtime. Every row logs at
`error` with a metric; none is silent:

| Failure                                                                   | Behavior                                                                                                         |
| ------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `generate` returns an error                                               | No identity this request; request proceeds stateless; no cookie written                                          |
| Graph-row commit fails at mint                                            | Mint never happened (§5): no cookie, no egress; next request retries                                             |
| Graph read fails on an existing identity                                  | Identity unusable this request (fail closed for egress); cookie untouched                                        |
| Cluster prefix listing fails                                              | Treated as cluster-size-unknown → `cluster_fallback` policy applies                                              |
| Tombstone write fails                                                     | Permission model spec §4.3: family retries, readers fail closed on partial families                              |
| Legacy rewrite fails mid-flight                                           | Old cookie remains live; rewrite retries (§6.1)                                                                  |
| Geo provider returns invalid output (unparseable country) at runtime      | Treated as lookup failure → `default_country` (permission model spec §5.2), counted in the lookup-failure metric |
| Device provider signals unavailable at runtime (e.g. no JA4 on a request) | Classification degrades per the provider's declared fallback, never silently upgrades `looks_like_browser`       |

## 7. Composition root and adapter parity

Provider construction happens in exactly one place per concern
(`build_ec_provider`, `build_device_provider`, `build_geo_provider`), called
by **every** adapter. No adapter may wire a concrete implementation directly:
in PR #838 the Cloudflare adapter installed its host geo unconditionally,
so identical configuration produced different jurisdictions on different
adapters — which the permission model then turned into different privacy
outcomes.

Requirements:

- **Adapters declare capabilities against an explicit matrix.** The
  capability set the composition root checks selections against is
  enumerated, not ad hoc: identity-graph persistence, atomic single-key
  reservation (CAS — required by the client-cycle reservation and any
  future compare-and-set use), KV prefix listing (cluster support),
  platform geo, device host evidence (JA4/HTTP-2), and legacy-rewrite
  support. Each adapter's declaration is part of its wiring, and the §6
  capability-mismatch startup error is driven by this matrix. Every §6.2
  runtime-failure row gets fault-injection coverage on every adapter that
  declares the corresponding capability.
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
